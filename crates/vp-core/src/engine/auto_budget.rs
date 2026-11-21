use super::*;

impl Engine {
    pub async fn get_project_auto_budget_policy(
        &self,
        project_id: &str,
    ) -> Result<AutoBudgetPolicyResult> {
        let store = self.store.clone();
        let project_id_for_load = project_id.to_string();
        let (policy, updated_at) = tokio::task::spawn_blocking(move || {
            if store.get_project(&project_id_for_load)?.is_none() {
                return Err(anyhow::anyhow!("project not found: {project_id_for_load}"));
            }
            store.get_project_auto_budget_policy_record(&project_id_for_load)
        })
        .await??;
        Ok(AutoBudgetPolicyResult {
            project_id: project_id.to_string(),
            policy,
            updated_at,
        })
    }

    pub async fn set_project_auto_budget_policy(
        &self,
        params: SetAutoBudgetPolicyParams,
    ) -> Result<AutoBudgetPolicyResult> {
        params.policy.validate()?;
        let store = self.store.clone();
        let project_id = params.project_id.clone();
        let policy = params.policy.clone();
        let (stored, event) = tokio::task::spawn_blocking(move || {
            store.set_project_auto_budget_policy_with_event(&project_id, &policy)
        })
        .await??;
        self.record_project_manifest_best_effort().await;
        Ok(AutoBudgetPolicyResult {
            project_id: params.project_id,
            policy: stored,
            updated_at: Some(event.created_at),
        })
    }

    pub async fn project_auto_budget_events(
        &self,
        project_id: &str,
        limit: Option<u32>,
    ) -> Result<Vec<ProjectAutoBudgetEvent>> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking(move || {
            store.list_project_auto_budget_events(&project_id, limit)
        })
        .await?
    }

    pub async fn session_auto_budget(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionAutoBudgetRecord>> {
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.get_session_auto_budget(&id)).await?
    }

    pub async fn session_auto_budget_state(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionAutoBudgetState>> {
        Ok(self
            .session_auto_budget(session_id)
            .await?
            .map(|record| record.state))
    }

    pub async fn update_session_auto_budget_state(
        &self,
        session_id: &str,
        state: SessionAutoBudgetState,
    ) -> Result<SessionAutoBudgetRecord> {
        self.ensure_session_mutable(session_id).await?;
        let store = self.store.clone();
        let id = session_id.to_string();
        let (record, event) = tokio::task::spawn_blocking(move || {
            store.update_session_auto_budget_state_with_progress_event(&id, &state)
        })
        .await??;
        if let Some(event) = event {
            let _ = self.events.send(event);
        }
        Ok(record)
    }

    /// Event-backed pause/resume state write. This is infrastructure for the
    /// runtime no-progress/auto-budget policy, not a policy decision itself.
    pub async fn update_session_auto_budget_state_with_event(
        &self,
        session_id: &str,
        state: SessionAutoBudgetState,
        event_kind: &str,
        payload: serde_json::Value,
    ) -> Result<SessionAutoBudgetRecord> {
        self.ensure_session_mutable(session_id).await?;
        let store = self.store.clone();
        let id = session_id.to_string();
        let event_kind = event_kind.to_string();
        let (record, event) = tokio::task::spawn_blocking(move || {
            store.update_session_auto_budget_state_with_event(&id, &state, &event_kind, payload)
        })
        .await??;
        let _ = self.events.send(event);
        Ok(record)
    }

    /// Atomically extend an auto session. Parallel callers serialize on one
    /// SQLite immediate transaction; the effective budget is clamped to the
    /// hard cap and a no-op at the cap returns `AUTO_BUDGET_CAP_REACHED`
    /// without appending an event.
    pub async fn extend_session_auto_budget(
        &self,
        session_id: &str,
        extension: AutoBudgetExtension,
    ) -> Result<AutoBudgetExtensionResult> {
        self.ensure_session_mutable(session_id).await?;
        let store = self.store.clone();
        let id = session_id.to_string();
        let outcome = tokio::task::spawn_blocking(move || {
            store.extend_session_auto_budget_with_event(&id, &extension)
        })
        .await??;
        let event_seq = outcome.event.as_ref().map(|event| event.seq);
        if let Some(event) = outcome.event.clone() {
            let _ = self.events.send(event);
        }
        let budget = self.load_session_budget_status(session_id).await?;
        Ok(AutoBudgetExtensionResult {
            session_id: session_id.to_string(),
            extended: outcome.outcome == AutoBudgetExtensionOutcome::Extended,
            code: outcome.outcome.as_str().to_string(),
            outcome: outcome.outcome,
            reason: outcome.reason,
            old_token_budget: outcome.old_token_budget,
            old_cost_budget_usd: outcome.old_cost_budget_usd,
            old_time_budget_secs: outcome.old_time_budget_secs,
            new_token_budget: outcome.new_token_budget,
            new_cost_budget_usd: outcome.new_cost_budget_usd,
            new_time_budget_secs: outcome.new_time_budget_secs,
            event_seq,
            budget,
        })
    }

    pub async fn session_budget(&self, session_id: &str) -> Result<SessionBudgetStatus> {
        self.load_session_budget_status(session_id).await
    }

    pub async fn session_budget_forecast(
        &self,
        session_id: &str,
    ) -> Result<SessionBudgetForecastResult> {
        let status = self.load_session_budget_status(session_id).await?;
        Ok(SessionBudgetForecastResult {
            session_id: status.session_id,
            forecast: status.forecast,
        })
    }

    pub async fn set_session_budget(
        &self,
        params: SetSessionBudgetParams,
    ) -> Result<SetSessionBudgetResult> {
        let session_id = params.session_id.clone();
        self.ensure_session_mutable(&session_id).await?;
        let update = vp_store::store::SessionBudgetUpdate {
            mode: params.mode,
            unlimited: params.unlimited,
            token_budget: params.token_budget,
            cost_budget_usd: params.cost_budget_usd,
            time_budget_secs: params.time_budget_secs,
            clear_token_budget: params.clear_token_budget,
            clear_cost_budget: params.clear_cost_budget,
            clear_time_budget: params.clear_time_budget,
            hard_token_cap: params.hard_token_cap,
            hard_cost_cap_usd: params.hard_cost_cap_usd,
            hard_wall_clock_secs: params.hard_wall_clock_secs,
            clear_hard_token_cap: params.clear_hard_token_cap,
            clear_hard_cost_cap: params.clear_hard_cost_cap,
            clear_hard_wall_clock_cap: params.clear_hard_wall_clock_cap,
            policy: params.policy,
        };
        let store = self.store.clone();
        let id = session_id.clone();
        let (_, _, event) =
            tokio::task::spawn_blocking(move || store.set_session_budget_with_event(&id, &update))
                .await??;
        let _ = self.events.send(event);
        let budget = self.load_session_budget_status(&session_id).await?;
        Ok(SetSessionBudgetResult { session_id, budget })
    }

    async fn load_session_budget_status(&self, session_id: &str) -> Result<SessionBudgetStatus> {
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || {
            let session = store
                .get_session(&id)?
                .ok_or_else(|| anyhow::anyhow!("session not found: {id}"))?;
            let record = store.get_session_auto_budget(&id)?;
            let project_policy = store.get_project_auto_budget_policy(&session.project_id)?;
            let forecast_calls = record
                .as_ref()
                .map(|row| row.policy.forecast_calls)
                .unwrap_or(project_policy.forecast_calls)
                .clamp(1, 1_000);
            let usage = store.list_recent_usage_entries(&id, forecast_calls)?;
            Ok::<_, anyhow::Error>(crate::auto_budget::budget_status(
                &session,
                record.as_ref(),
                &usage,
                &project_policy,
                &now_rfc3339(),
            ))
        })
        .await?
    }

    async fn ensure_session_mutable(&self, session_id: &str) -> Result<()> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let status = tokio::task::spawn_blocking(move || {
            store
                .get_session(&id)?
                .map(|session| session.status)
                .ok_or_else(|| anyhow::anyhow!("session not found: {id}"))
        })
        .await??;
        if status == "imported" {
            return Err(anyhow!("imported sessions are read-only archives"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;
    use tempfile::TempDir;

    fn init_repo(path: &Path) {
        std::fs::write(path.join("README.md"), "hello\n").unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "test@verlinpark.local"],
            vec!["config", "user.name", "VerlinPark Test"],
            vec!["add", "."],
            vec!["commit", "-q", "-m", "initial"],
        ] {
            let status = Command::new("git")
                .arg("-C")
                .arg(path)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success());
        }
    }

    fn auto_policy() -> AutoBudgetPolicy {
        AutoBudgetPolicy {
            enabled: true,
            initial_token_budget: Some(1_000),
            hard_token_cap: Some(5_000),
            hard_cost_cap_usd: Some(2.5),
            ..AutoBudgetPolicy::default()
        }
    }

    async fn fixture() -> (TempDir, TempDir, Engine, String) {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("auto-budget".into()),
            )
            .await
            .unwrap();
        (data, repo, engine, project.id)
    }

    #[tokio::test]
    async fn auto_session_creation_persists_hard_caps_and_status() {
        let (_data, _repo, engine, project_id) = fixture().await;
        let session = engine
            .create_session_with_budget_options(
                project_id,
                "auto work".into(),
                None,
                None,
                None,
                Some(BudgetMode::Auto),
                Some(auto_policy()),
                0,
                false,
            )
            .await
            .unwrap();
        assert!(!session.unlimited);
        assert_eq!(session.token_budget, Some(1_000));
        assert_eq!(session.cost_budget_usd, Some(2.5));
        let record = engine
            .session_auto_budget(&session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.mode, BudgetMode::Auto);
        assert_eq!(record.caps.token, Some(5_000));
        let status = engine.session_budget(&session.id).await.unwrap();
        assert_eq!(status.mode, BudgetMode::Auto);
        assert_eq!(status.hard_token_cap, Some(5_000));
        assert_eq!(status.remaining_tokens, Some(1_000));
    }

    #[tokio::test]
    async fn auto_without_a_hard_cap_fails_closed_before_creating_a_session() {
        let (_data, _repo, engine, project_id) = fixture().await;
        let policy = AutoBudgetPolicy {
            enabled: true,
            initial_token_budget: Some(1_000),
            ..AutoBudgetPolicy::default()
        };
        let error = engine
            .create_session_with_budget_options(
                project_id.clone(),
                "no cap".into(),
                None,
                None,
                None,
                Some(BudgetMode::Auto),
                Some(policy),
                0,
                false,
            )
            .await
            .unwrap_err();
        let typed = error.downcast_ref::<BudgetValidationError>().unwrap();
        assert_eq!(typed.code, "AUTO_BUDGET_CAP_REQUIRED");
        assert!(
            engine
                .list_sessions(Some(project_id))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn session_budget_set_is_event_backed_and_rejects_below_spend() {
        let (_data, _repo, engine, project_id) = fixture().await;
        let session = engine
            .create_session_with_budget_options(
                project_id,
                "set".into(),
                None,
                None,
                None,
                Some(BudgetMode::Auto),
                Some(auto_policy()),
                0,
                false,
            )
            .await
            .unwrap();
        engine
            .record_agent_usage(
                &session.id,
                "agent",
                "Agent",
                "leader",
                None,
                None,
                "test",
                300,
                100,
                false,
                0.5,
            )
            .await
            .unwrap();
        let error = engine
            .set_session_budget(SetSessionBudgetParams {
                session_id: session.id.clone(),
                token_budget: Some(100),
                ..SetSessionBudgetParams::default()
            })
            .await
            .unwrap_err();
        let typed = error.downcast_ref::<BudgetValidationError>().unwrap();
        assert_eq!(typed.code, "BUDGET_BELOW_SPENT");

        let result = engine
            .set_session_budget(SetSessionBudgetParams {
                session_id: session.id.clone(),
                mode: Some(BudgetMode::Unlimited),
                ..SetSessionBudgetParams::default()
            })
            .await
            .unwrap();
        assert!(result.budget.unlimited);
        assert_eq!(result.budget.mode, BudgetMode::Unlimited);
        let events = engine
            .list_events(session.id.clone(), 0, 100)
            .await
            .unwrap();
        assert!(
            events
                .iter()
                .any(|event| event.kind == "session.budget.updated")
        );
    }

    #[tokio::test]
    async fn auto_extension_is_atomic_and_returns_a_typed_cap_no_op() {
        let (_data, _repo, engine, project_id) = fixture().await;
        let policy = AutoBudgetPolicy {
            enabled: true,
            initial_token_budget: Some(1_000),
            hard_token_cap: Some(1_500),
            ..AutoBudgetPolicy::default()
        };
        let session = engine
            .create_session_with_budget_options(
                project_id,
                "extend".into(),
                None,
                None,
                None,
                Some(BudgetMode::Auto),
                Some(policy),
                0,
                false,
            )
            .await
            .unwrap();
        let first = engine
            .extend_session_auto_budget(
                &session.id,
                AutoBudgetExtension {
                    requested_token_extension: Some(100),
                    reason: Some("worker runway".into()),
                    ..AutoBudgetExtension::default()
                },
            )
            .await
            .unwrap();
        assert!(first.extended);
        assert_eq!(first.old_token_budget, Some(1_000));
        assert_eq!(first.new_token_budget, Some(1_250));
        assert_eq!(first.code, "extended");
        assert!(first.event_seq.is_some());
        assert_eq!(first.budget.effective_token_budget, Some(1_250));

        let second = engine
            .extend_session_auto_budget(&session.id, AutoBudgetExtension::default())
            .await
            .unwrap();
        assert!(second.extended);
        assert_eq!(second.new_token_budget, Some(1_500));

        let third = engine
            .extend_session_auto_budget(&session.id, AutoBudgetExtension::default())
            .await
            .unwrap();
        assert!(!third.extended);
        assert_eq!(third.code, "AUTO_BUDGET_CAP_REACHED");
        assert!(third.event_seq.is_none());
        assert_eq!(third.new_token_budget, Some(1_500));
        assert_eq!(third.budget.hard_token_cap, Some(1_500));
    }

    #[tokio::test]
    async fn project_policy_manifest_and_database_rebuild_restore_auto_budget() {
        let (data, _repo, engine, project_id) = fixture().await;
        let policy = auto_policy();
        engine
            .set_project_auto_budget_policy(SetAutoBudgetPolicyParams {
                project_id: project_id.clone(),
                policy: policy.clone(),
            })
            .await
            .unwrap();
        let session = engine
            .create_session_with_budget_options(
                project_id.clone(),
                "rebuild auto".into(),
                None,
                None,
                None,
                Some(BudgetMode::Auto),
                Some(policy.clone()),
                0,
                false,
            )
            .await
            .unwrap();
        engine
            .update_session_auto_budget_state(
                &session.id,
                SessionAutoBudgetState {
                    total_turns: 4,
                    progress: Some(serde_json::json!({"turn": 4, "progress": false})),
                    ..SessionAutoBudgetState::default()
                },
            )
            .await
            .unwrap();
        engine.record_project_manifest().await.unwrap();
        drop(engine);

        let report = crate::rebuild_database(data.path(), false).await.unwrap();
        assert_eq!(report.sessions, 1, "{:?}", report.skipped);
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let manifest = vp_store::read_project_manifest(data.path()).unwrap();
        assert_eq!(manifest[0].auto_budget_policy, policy);
        assert_eq!(
            engine
                .get_project_auto_budget_policy(&project_id)
                .await
                .unwrap()
                .policy,
            policy
        );
        let record = engine
            .session_auto_budget(&session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.mode, BudgetMode::Auto);
        assert_eq!(record.caps.token, Some(5_000));
        assert_eq!(
            record.state.progress,
            Some(serde_json::json!({"turn": 4, "progress": false}))
        );
        let restored = engine.store().get_session(&session.id).unwrap().unwrap();
        assert_eq!(restored.token_budget, Some(1_000));
        assert_eq!(restored.cost_budget_usd, Some(2.5));
    }

    #[tokio::test]
    async fn session_bundle_round_trip_carries_auto_budget_and_stays_read_only() {
        let (_data, repo, engine, project_id) = fixture().await;
        let session = engine
            .create_session_with_budget_options(
                project_id,
                "bundle auto".into(),
                None,
                None,
                None,
                Some(BudgetMode::Auto),
                Some(auto_policy()),
                0,
                false,
            )
            .await
            .unwrap();
        engine
            .update_session_auto_budget_state(
                &session.id,
                SessionAutoBudgetState {
                    total_turns: 2,
                    progress: Some(serde_json::json!({"turn": 2, "progress": true})),
                    ..SessionAutoBudgetState::default()
                },
            )
            .await
            .unwrap();
        let bundle = engine.export_session(&session.id).await.unwrap();
        let bundled = bundle.session_auto_budget.as_ref().unwrap();
        assert_eq!(
            bundled.state.progress,
            Some(serde_json::json!({"turn": 2, "progress": true}))
        );

        let imported_data = TempDir::new().unwrap();
        let imported_engine = Engine::new(Store::open(imported_data.path()).unwrap());
        let result = imported_engine.import_session(bundle).await.unwrap();
        assert_eq!(result.status, "imported");
        let record = imported_engine
            .session_auto_budget(&session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.mode, BudgetMode::Auto);
        assert_eq!(record.caps.cost_usd, Some(2.5));
        assert_eq!(
            record.state.progress,
            Some(serde_json::json!({"turn": 2, "progress": true}))
        );
        let error = imported_engine
            .set_session_budget(SetSessionBudgetParams {
                session_id: session.id.clone(),
                mode: Some(BudgetMode::Fixed),
                ..SetSessionBudgetParams::default()
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("read-only"));
        let _ = repo;
    }
}

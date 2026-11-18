use super::*;

impl Engine {
    pub async fn plan_revisions(&self, session_id: &str) -> Result<Vec<PlanRevisionInfo>> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let events =
            tokio::task::spawn_blocking(move || store.plan_lifecycle_events(&session_id)).await??;
        Ok(crate::orchestration::revisions_from_events(&events))
    }

    pub async fn revise_team_plan(
        &self,
        session_id: &str,
        plan: TeamPlan,
        reason: &str,
        author: &str,
        expected_revision: Option<u32>,
        max_managers: usize,
    ) -> Result<ReviseTeamPlanResult> {
        let reason = reason.trim();
        if reason.is_empty() || reason.chars().count() > 1_000 {
            anyhow::bail!("revision reason must be 1-1000 chars");
        }
        let author = author.trim();
        if author.is_empty() || author.chars().count() > 80 {
            anyhow::bail!("revision author must be 1-80 chars");
        }
        let session_id = session_id.to_string();
        let store = self.store.clone();
        let session = tokio::task::spawn_blocking({
            let store = store.clone();
            let session_id = session_id.clone();
            move || store.get_session(&session_id)
        })
        .await??
        .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        crate::ensure_session_quiescent(&session, "revise the team plan")?;
        let current_raw = tokio::task::spawn_blocking({
            let store = store.clone();
            let session_id = session_id.clone();
            move || store.get_team_plan(&session_id)
        })
        .await??;
        let current: TeamPlan = current_raw
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok())
            .ok_or_else(|| anyhow::anyhow!("session has no TeamPlan to revise"))?;
        let tasks = tokio::task::spawn_blocking({
            let store = store.clone();
            let session_id = session_id.clone();
            move || store.list_plan_tasks(None, Some(&session_id))
        })
        .await??;
        let events = self.recent_events(&session_id, 5_000).await?;
        // The store count is authoritative; the recent-event window is only a
        // bounded status fallback and must never number revisions.
        let current_revision = self.current_plan_revision(&session_id).await?;
        if let Some(expected) = expected_revision
            && expected != current_revision
        {
            anyhow::bail!(
                "PLAN_REVISION_CONFLICT: plan revision conflict: expected {expected}, current {current_revision}"
            );
        }
        crate::agent::validate_team_plan(&plan, max_managers)?;
        let statuses = crate::orchestration::status_map(&current, &tasks, &events);
        crate::orchestration::ensure_revision_safe(&current, &plan, &statuses)?;
        let diff = crate::orchestration::diff_plan(&current, &plan);
        let revision = current_revision.saturating_add(1);
        let snapshot = self.pin_plan_snapshot(&session, &plan, revision).await?;
        let snapshot_json = serde_json::to_string(&snapshot)?;
        let payload = serde_json::json!({
            "plan": plan,
            "revision": revision,
            "reason": reason,
            "author": author,
            "task_count": plan.tasks.len(),
            "added": diff.added,
            "removed": diff.removed,
            "changed": diff.changed,
            "runtime_snapshot": snapshot,
        });
        let plan_json = serde_json::to_string(&plan)?;
        let expected_at_write = current_revision;
        let event = tokio::task::spawn_blocking({
            let store = store.clone();
            let session_id = session_id.clone();
            let plan_json = plan_json.clone();
            let payload = payload.clone();
            let snapshot_json = snapshot_json.clone();
            move || {
                store.compare_and_set_team_plan_revision_with_runtime_snapshot_and_event(
                    &session_id,
                    &plan_json,
                    "team_plan.revised",
                    payload,
                    Some(&snapshot_json),
                    expected_at_write,
                )
            }
        })
        .await??
        .ok_or_else(|| {
            anyhow::anyhow!(
                "PLAN_REVISION_CONFLICT: plan revision conflict: revision {current_revision} was written concurrently"
            )
        })?;
        let _ = self.events.send(event.clone());
        self.scheduler.sync_plan(&session, &plan).await?;
        Ok(ReviseTeamPlanResult {
            revision: PlanRevisionInfo {
                revision,
                seq: event.seq,
                kind: event.kind,
                reason: Some(reason.to_string()),
                author: author.to_string(),
                task_count: plan.tasks.len(),
                added: diff.added,
                removed: diff.removed,
                changed: diff.changed,
                created_at: event.created_at,
            },
            plan,
        })
    }

    pub async fn plan_revision_detail(
        &self,
        session_id: &str,
        revision: u32,
    ) -> Result<PlanRevisionDetailResult> {
        let store = self.store.clone();
        let session_id_owned = session_id.to_string();
        let events =
            tokio::task::spawn_blocking(move || store.plan_lifecycle_events(&session_id_owned))
                .await??;
        let revisions = crate::orchestration::revisions_from_events(&events);
        let info = revisions
            .iter()
            .find(|candidate| candidate.revision == revision)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("plan revision {revision} not found"))?;
        let plan = events
            .iter()
            .filter(|event| {
                matches!(
                    event.kind.as_str(),
                    "team_plan.submitted" | "team_plan.revised"
                )
            })
            .filter(|event| {
                event
                    .payload
                    .get("revision")
                    .and_then(serde_json::Value::as_u64)
                    .map(|value| value as u32)
                    .unwrap_or_else(|| {
                        revisions
                            .iter()
                            .find(|candidate| candidate.seq == event.seq)
                            .map(|candidate| candidate.revision)
                            .unwrap_or(0)
                    })
                    == revision
            })
            .find_map(crate::orchestration::plan::plan_from_event)
            .ok_or_else(|| anyhow::anyhow!("plan revision {revision} has no stored plan"))?;
        Ok(PlanRevisionDetailResult {
            revision: info,
            plan,
        })
    }

    pub async fn restore_team_plan_revision(
        &self,
        session_id: &str,
        revision: u32,
        reason: &str,
        expected_revision: Option<u32>,
        max_managers: usize,
    ) -> Result<ReviseTeamPlanResult> {
        let detail = self.plan_revision_detail(session_id, revision).await?;
        self.revise_team_plan(
            session_id,
            detail.plan,
            reason,
            "user",
            expected_revision,
            max_managers,
        )
        .await
    }

    pub async fn edit_plan_task(
        &self,
        session_id: &str,
        task: vp_protocol::types::PlanTask,
        reason: &str,
        expected_revision: Option<u32>,
        max_managers: usize,
    ) -> Result<ReviseTeamPlanResult> {
        let current = self
            .get_team_plan(session_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("session has no TeamPlan to edit"))?;
        let mut plan = current.clone();
        let index = plan
            .tasks
            .iter()
            .position(|candidate| candidate.id == task.id)
            .ok_or_else(|| anyhow::anyhow!("task not found in plan: {}", task.id))?;
        plan.tasks[index] = task;
        self.revise_team_plan(
            session_id,
            plan,
            reason,
            "user",
            expected_revision,
            max_managers,
        )
        .await
    }

    /// Operator/internal plan write. It intentionally replaces the stored
    /// plan; the Leader tool surface uses [`Engine::submit_team_plan`] and the
    /// explicit revision path instead.
    pub async fn set_team_plan(&self, session_id: &str, plan: &TeamPlan) -> Result<()> {
        self.persist_team_plan(session_id, plan, false).await
    }

    /// First Leader submission for a session. A session has exactly one
    /// baseline `team_plan.submitted`; a second submission is rejected with a
    /// typed error so the task projection (attempts/completed rows) cannot be
    /// silently reset. Changing a plan requires the explicit revision path.
    pub async fn submit_team_plan(&self, session_id: &str, plan: &TeamPlan) -> Result<()> {
        self.persist_team_plan(session_id, plan, true).await
    }

    async fn persist_team_plan(
        &self,
        session_id: &str,
        plan: &TeamPlan,
        require_absent: bool,
    ) -> Result<()> {
        if require_absent && self.get_team_plan(session_id).await?.is_some() {
            anyhow::bail!(
                "PLAN_REVISION_REQUIRED: session {session_id} already has a TeamPlan; submit_team_plan cannot replace it without an explicit revision"
            );
        }
        let store = self.store.clone();
        let id = session_id.to_string();
        let json = serde_json::to_string(plan)?;
        let loaded = {
            let store = store.clone();
            let id = id.clone();
            tokio::task::spawn_blocking(move || store.get_session(&id)).await??
        }
        .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        if loaded.status == "imported" {
            anyhow::bail!("imported sessions are read-only archives");
        }
        let revision = self
            .current_plan_revision(session_id)
            .await?
            .saturating_add(1);
        let snapshot = self.pin_plan_snapshot(&loaded, plan, revision).await?;
        let snapshot_json = serde_json::to_string(&snapshot)?;
        let event = if require_absent {
            tokio::task::spawn_blocking({
                let store = store.clone();
                let id = id.clone();
                let json = json.clone();
                let snapshot_json = snapshot_json.clone();
                move || {
                    store.set_team_plan_if_absent_with_runtime_snapshot_and_event(
                        &id,
                        &json,
                        Some(&snapshot_json),
                    )
                }
            })
            .await??
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "PLAN_REVISION_REQUIRED: session {session_id} already has a TeamPlan; submit_team_plan cannot replace it without an explicit revision"
                )
            })?
        } else {
            tokio::task::spawn_blocking(move || {
                store.set_team_plan_with_runtime_snapshot_and_event(
                    &id,
                    &json,
                    Some(&snapshot_json),
                )
            })
            .await??
        };
        let _ = self.events.send(event);
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        self.scheduler.sync_plan(&session, plan).await?;
        Ok(())
    }

    pub async fn get_team_plan(&self, session_id: &str) -> Result<Option<TeamPlan>> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let raw = tokio::task::spawn_blocking(move || store.get_team_plan(&id)).await??;
        Ok(raw.and_then(|json| serde_json::from_str(&json).ok()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use vp_protocol::types::{PlanDepartment, PlanTask};

    fn task(id: &str, deps: Vec<String>) -> PlanTask {
        PlanTask {
            id: id.into(),
            department: "impl".into(),
            title: id.into(),
            kind: "coding".into(),
            priority: 0,
            profile: None,
            deps,
            write_scope: vec![format!("src/{id}")],
            acceptance: vec!["done".into()],
            token_budget: None,
            cost_budget_usd: None,
            parent_task_id: None,
            fanout_key: None,
            fanout_join_for: None,
        }
    }

    fn plan() -> TeamPlan {
        TeamPlan {
            goal: "plan revision".into(),
            departments: vec![PlanDepartment {
                name: "impl".into(),
                manager_title: "Implementation".into(),
                profile: "coding".into(),
                objective: "build".into(),
                token_budget: None,
                cost_budget_usd: None,
            }],
            tasks: vec![task("T1", vec![])],
        }
    }

    #[tokio::test]
    async fn resubmission_requires_revision_and_revision_preserves_completed_tasks() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("plans"))
            .unwrap();
        let session = store.create_session(&project.id, "resubmit").unwrap();
        let engine = Engine::new(store.clone());

        let original = plan();
        engine
            .submit_team_plan(&session.id, &original)
            .await
            .unwrap();
        let plan_seq = store
            .latest_event_seq(&session.id, "team_plan.submitted")
            .unwrap();
        store
            .claim_plan_tasks_with_events(&session.id, plan_seq, &["T1".into()])
            .unwrap();
        store
            .finish_plan_task(&session.id, plan_seq, "T1", "completed", None)
            .unwrap()
            .unwrap();
        store
            .append_event(
                &session.id,
                "plan_task.finished",
                serde_json::json!({
                    "task_id": "T1",
                    "task_status": "completed",
                    "summary": "done",
                }),
            )
            .unwrap();
        let lifecycle_before = store.plan_lifecycle_events(&session.id).unwrap().len();
        assert_eq!(lifecycle_before, 1);

        let error = engine
            .submit_team_plan(&session.id, &original)
            .await
            .unwrap_err();
        assert!(
            error.to_string().starts_with("PLAN_REVISION_REQUIRED"),
            "{error}"
        );
        assert_eq!(
            store.plan_lifecycle_events(&session.id).unwrap().len(),
            lifecycle_before,
            "a rejected submission must not append team_plan.submitted"
        );
        let t1 = store
            .list_plan_tasks(None, Some(&session.id))
            .unwrap()
            .into_iter()
            .find(|task| task.task_id == "T1")
            .unwrap();
        assert_eq!(t1.status, "completed");
        assert_eq!(t1.attempts, 1);

        let mut revised = original.clone();
        revised.goal = "plan revision with follow-up".into();
        revised.tasks.push(task("T2", vec!["T1".into()]));
        let result = engine
            .revise_team_plan(&session.id, revised, "add a follow-up", "user", Some(1), 4)
            .await
            .unwrap();
        assert_eq!(result.revision.revision, 2);
        assert_eq!(engine.plan_revisions(&session.id).await.unwrap().len(), 2);

        let tasks = store.list_plan_tasks(None, Some(&session.id)).unwrap();
        let t1 = tasks.iter().find(|task| task.task_id == "T1").unwrap();
        assert_eq!(t1.status, "completed");
        assert_eq!(t1.attempts, 1);
        let t2 = tasks.iter().find(|task| task.task_id == "T2").unwrap();
        assert_eq!(t2.status, "pending");

        let error = engine
            .submit_team_plan(&session.id, &original)
            .await
            .unwrap_err();
        assert!(
            error.to_string().starts_with("PLAN_REVISION_REQUIRED"),
            "{error}"
        );
        assert_eq!(store.plan_lifecycle_events(&session.id).unwrap().len(), 2);
    }
}

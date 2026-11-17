use super::*;

impl AgentService {
    pub async fn start(&self, session_id: &str, prompt: &str) -> Result<()> {
        self.start_with_operation(session_id, prompt, None).await
    }

    /// Start a Leader run with an optional idempotency key. A completed
    /// receipt with the same method and payload is replayed as accepted;
    /// pending, conflicting, or crash-unknown receipts never start a second
    /// run.
    pub async fn start_with_operation(
        &self,
        session_id: &str,
        prompt: &str,
        operation_id: Option<&str>,
    ) -> Result<()> {
        let payload = json!({
            "session_id": session_id,
            "content": prompt,
        });
        let receipt = match crate::operation::begin(
            self.engine.store(),
            session_id,
            operation_id,
            "session.prompt",
            &payload,
        )
        .await?
        {
            crate::operation::OperationStart::Replay(_) => return Ok(()),
            crate::operation::OperationStart::Proceed(receipt) => receipt,
        };
        let result = self.start_inner(session_id, prompt).await;
        match &result {
            Ok(()) => {
                if let Some(receipt) = &receipt
                    && let Err(error) = receipt.complete(&json!({"accepted": true})).await
                {
                    return Err(error.context(
                        "run started but the session operation receipt could not be completed",
                    ));
                }
            }
            Err(error) => {
                if let Some(receipt) = &receipt
                    && let Err(receipt_error) = receipt.fail(&format!("{error:#}")).await
                {
                    tracing::warn!(
                        %receipt_error,
                        session_id,
                        operation_id = ?operation_id,
                        "failed to record a failed session operation"
                    );
                }
            }
        }
        result
    }

    pub(in crate::agent) async fn start_inner(&self, session_id: &str, prompt: &str) -> Result<()> {
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned());
        let Some(config) = config else {
            self.engine
                .append_event(
                    session_id,
                    "agent.unavailable",
                    serde_json::json!({
                        "message": "No model provider configured. Set VERLINPARK_BASE_URL and VERLINPARK_MODEL (and an API key if needed)."
                    }),
                )
                .await?;
            return Err(anyhow!(
                "agent provider is not configured; set VERLINPARK_BASE_URL and VERLINPARK_MODEL"
            ));
        };
        let (session, _) = load_session_project(self.engine.store().clone(), session_id).await?;
        if session.status == "imported" {
            anyhow::bail!(
                "imported sessions are read-only archives; create a new session to continue"
            );
        }
        if session.status == "landed" {
            anyhow::bail!("landed sessions are final; create a new session or fork to continue");
        }

        let control = Arc::new(RunControl::new());
        let preemption_policy = self
            .engine
            .get_preemption_policy(&session.project_id)
            .await?;
        control.arm_preemption(session.time_budget_secs, preemption_policy);
        {
            let mut runs = self
                .runs
                .lock()
                .map_err(|_| anyhow!("agent run mutex poisoned"))?;
            if runs.contains_key(session_id) {
                return Err(anyhow!("session is already running"));
            }
            runs.insert(session_id.to_string(), control.clone());
        }
        let lease = match self
            .engine
            .acquire_session_lease(session_id, std::time::Duration::from_secs(180))
            .await
        {
            Ok(lease) => lease,
            Err(error) => {
                if let Ok(mut runs) = self.runs.lock() {
                    runs.remove(session_id);
                }
                return Err(error);
            }
        };

        let prepared: Result<()> = async {
            self.engine
                .update_session_status(session_id, "queued")
                .await?;
            self.engine
                .append_message(session_id, "user", prompt, serde_json::json!([]), None)
                .await?;
            self.engine
                .append_event(
                    session_id,
                    "agent.queued",
                    serde_json::json!({"prompt": prompt}),
                )
                .await?;
            Ok(())
        }
        .await;
        if let Err(error) = prepared {
            if let Ok(mut runs) = self.runs.lock() {
                runs.remove(session_id);
            }
            let _ = self
                .engine
                .update_session_status(session_id, "failed")
                .await;
            lease.release().await;
            return Err(error);
        }

        self.spawn_run(
            config,
            session.id.clone(),
            session.project_id.clone(),
            session.priority,
            prompt.to_string(),
            control,
            lease,
            None,
        );
        Ok(())
    }

    /// Explicit manual resume. The optional operation id is a receipt for this
    /// RPC only; it is deliberately separate from `session.prompt` admission.
    pub async fn resume_session_run(
        &self,
        session_id: &str,
        run_id: Option<&str>,
        operation_id: Option<&str>,
    ) -> Result<SessionRunResumeResult> {
        let payload = serde_json::json!({
            "session_id": session_id,
            "run_id": run_id,
        });
        let receipt = match crate::operation::begin(
            self.engine.store(),
            session_id,
            operation_id,
            "session.run.resume",
            &payload,
        )
        .await?
        {
            crate::operation::OperationStart::Replay(result) => {
                return crate::operation::replay(&result);
            }
            crate::operation::OperationStart::Proceed(receipt) => receipt,
        };
        let result = self
            .resume_run_inner(session_id, run_id, ResumeMode::Manual)
            .await;
        match &result {
            Ok(resume) => crate::operation::complete(&receipt, resume).await?,
            Err(error) => crate::operation::fail(&receipt, &format!("{error:#}")).await,
        }
        result
    }

    /// Daemon startup auto-resume. Opt-in per project and bounded by the same
    /// preflight as manual resume. One attempt per session per tick.
    pub async fn auto_resume_interrupted_runs(&self) -> Result<usize> {
        let configured = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().map(|_| ()))
            .is_some();
        if !configured {
            return Ok(0);
        }
        let runs = crate::run_journal::all_resumable_runs(&self.engine).await?;
        let mut seen_sessions = std::collections::HashSet::new();
        let mut started = 0usize;
        for run in runs {
            if !run.top_level || !seen_sessions.insert(run.session_id.clone()) {
                continue;
            }
            let policy = crate::run_journal::policy(&self.engine, &run.project_id).await?;
            if !policy.auto_resume {
                continue;
            }
            match self
                .resume_run_inner(&run.session_id, Some(&run.run_id), ResumeMode::Auto)
                .await
            {
                Ok(_) => started = started.saturating_add(1),
                Err(error) => {
                    tracing::warn!(
                        %error,
                        run_id = %run.run_id,
                        session_id = %run.session_id,
                        "auto-resume refused a durable run"
                    );
                }
            }
        }
        Ok(started)
    }

    /// Cooperatively cancel one active run. The cancellation is requested on
    /// the session's in-memory run control; the run loop is the only writer
    /// that records the terminal interrupted/cancelled journal row.
    pub async fn cancel_session_run(
        &self,
        session_id: &str,
        run_id: Option<&str>,
    ) -> Result<SessionRunCancelResult> {
        let store = self.engine.store().clone();
        let session_id_owned = session_id.to_string();
        let requested_run = run_id.map(str::to_string);
        let run = tokio::task::spawn_blocking(move || {
            if let Some(run_id) = requested_run {
                return store.get_session_run(&run_id);
            }
            Ok::<_, anyhow::Error>(
                store
                    .list_session_runs(&session_id_owned, 100)?
                    .into_iter()
                    .find(|run| run.status == "running"),
            )
        })
        .await??;
        let Some(run) = run else {
            return Ok(SessionRunCancelResult {
                session_id: session_id.to_string(),
                run_id: run_id.map(str::to_string),
                cancelled: false,
                detail: "no running durable run was found".to_string(),
            });
        };
        if run.session_id != session_id {
            anyhow::bail!("run {} belongs to another session", run.run_id);
        }
        if run.status != "running" {
            return Ok(SessionRunCancelResult {
                session_id: session_id.to_string(),
                run_id: Some(run.run_id),
                cancelled: false,
                detail: format!("run is already {}", run.status),
            });
        }
        let cancelled = self.cancel(session_id);
        if cancelled {
            // A cancelled run must not retain the physical GUI. Grace is for
            // normal multi-step interaction, not for an operator cancellation.
            self.engine.gui_end_session(session_id, "cancelled").await;
            if let Err(error) = self
                .engine
                .append_event(
                    session_id,
                    "session.run.cancel_requested",
                    serde_json::json!({"run_id": run.run_id}),
                )
                .await
            {
                tracing::warn!(%error, run_id = %run.run_id, "failed to record durable cancel intent");
            }
        }
        Ok(SessionRunCancelResult {
            session_id: session_id.to_string(),
            run_id: Some(run.run_id),
            cancelled,
            detail: if cancelled {
                "cooperative cancellation requested; the run loop records the terminal journal row at its next boundary"
                    .to_string()
            } else {
                "no live run control is available in this daemon".to_string()
            },
        })
    }

    pub(in crate::agent) async fn resume_run_inner(
        &self,
        session_id: &str,
        run_id: Option<&str>,
        mode: ResumeMode,
    ) -> Result<SessionRunResumeResult> {
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned())
            .ok_or_else(|| {
                anyhow!("agent provider is not configured; a resumed run needs the same provider and model")
            })?;
        let store = self.engine.store().clone();
        let (session, _project) = load_session_project(store.clone(), session_id).await?;
        let run = if let Some(run_id) = run_id {
            let store = store.clone();
            let requested = run_id.to_string();
            let lookup = requested.clone();
            tokio::task::spawn_blocking(move || store.get_session_run(&lookup))
                .await??
                .ok_or_else(|| anyhow!("run not found: {requested}"))?
        } else {
            let store = store.clone();
            let id = session_id.to_string();
            tokio::task::spawn_blocking(move || store.list_session_runs(&id, 100))
                .await??
                .into_iter()
                .find(|run| {
                    run.top_level
                        && crate::run_journal::is_resumable_status(&run.status)
                        && !run.failure.as_ref().is_some_and(|failure| {
                            matches!(
                                failure.kind.as_str(),
                                "cancelled" | "superseded" | "preempted"
                            )
                        })
                })
                .ok_or_else(|| {
                    anyhow!("session has no interrupted or failed top-level run to resume")
                })?
        };
        if run.session_id != session_id {
            anyhow::bail!("run {} belongs to another session", run.run_id);
        }
        let policy = crate::run_journal::policy(&self.engine, &session.project_id).await?;
        if mode == ResumeMode::Auto && !policy.auto_resume {
            anyhow::bail!(
                "project {} has not opted into auto_resume",
                session.project_id
            );
        }
        let compatibility = self
            .engine
            .run_compatibility(session_id, &run.run_id)
            .await?;
        if compatibility.status == vp_protocol::runtime::CompatibilityStatus::Incompatible {
            return Err(crate::runtime_pin::RuntimePinError::new(
                crate::runtime_pin::codes::RUNTIME_INCOMPATIBLE,
                format!(
                    "run runtime identity is incompatible with the stored snapshot: {}",
                    compatibility.warnings.join("; ")
                ),
            )
            .with_report(compatibility)
            .into());
        }
        if compatibility.status == vp_protocol::runtime::CompatibilityStatus::Drift {
            self.engine
                .record_compatibility_drift(&compatibility)
                .await?;
        }
        let model = model_for(&config, AgentRole::Leader);
        let plan = crate::run_journal::preflight(
            &self.engine,
            &session,
            &run,
            &policy,
            &config.provider_name,
            &model,
            time::OffsetDateTime::now_utc(),
        )
        .await?;
        let worktree = PathBuf::from(
            session
                .worktree
                .clone()
                .ok_or_else(|| anyhow!("session has no worktree"))?,
        );
        let control = self.begin_resumable_run(
            session_id,
            session.time_budget_secs,
            self.engine
                .get_preemption_policy(&session.project_id)
                .await?,
        )?;
        let lease = match self
            .engine
            .acquire_session_lease(session_id, std::time::Duration::from_secs(180))
            .await
        {
            Ok(lease) => lease,
            Err(error) => {
                if let Ok(mut runs) = self.runs.lock() {
                    runs.remove(session_id);
                }
                return Err(error);
            }
        };
        let budget = RunBudgetSnapshot {
            token_limit: session.token_budget,
            tokens_spent: session.tokens_in.saturating_add(session.tokens_out),
            cost_limit_usd: session.cost_budget_usd,
            cost_spent_usd: session.cost_spent_usd,
            time_limit_secs: session.time_budget_secs,
            elapsed_secs: 0,
            unlimited: session.unlimited,
        };
        let journal = match crate::run_journal::begin_resumed_run(
            &self.engine,
            &session,
            &plan.previous,
            &config.provider_name,
            &model,
            &worktree,
            budget,
        )
        .await
        {
            Ok(journal) => journal,
            Err(error) => {
                if let Ok(mut runs) = self.runs.lock() {
                    runs.remove(session_id);
                }
                let _ = self
                    .engine
                    .update_session_status(session_id, "failed")
                    .await;
                lease.release().await;
                return Err(error);
            }
        };
        let new_run_id = journal.id().to_string();
        let attempt = journal.run.attempt;
        let resumed_from = journal.run.resumed_from_run_id.clone();
        let prepared: Result<()> = async {
            self.engine
                .update_session_status(session_id, "queued")
                .await?;
            self.engine
                .append_message(session_id, "user", &plan.note, serde_json::json!([]), None)
                .await?;
            self.engine
                .append_event(
                    session_id,
                    "agent.queued",
                    serde_json::json!({
                        "prompt": plan.note,
                        "resumed_run_id": new_run_id,
                        "resumed_from_run_id": plan.previous.run_id,
                        "attempt": attempt,
                    }),
                )
                .await?;
            Ok(())
        }
        .await;
        if let Err(error) = prepared {
            let _ = journal
                .finish_failed(&self.engine, "resume_start_failed", &format!("{error:#}"))
                .await;
            if let Ok(mut runs) = self.runs.lock() {
                runs.remove(session_id);
            }
            let _ = self
                .engine
                .update_session_status(session_id, "failed")
                .await;
            lease.release().await;
            return Err(error);
        }
        self.spawn_run(
            config,
            session.id.clone(),
            session.project_id.clone(),
            session.priority,
            plan.note,
            control,
            lease,
            Some(journal),
        );
        Ok(SessionRunResumeResult {
            session_id: session_id.to_string(),
            resumed: true,
            run_id: new_run_id,
            resumed_from_run_id: resumed_from,
            attempt,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::agent) fn spawn_run(
        &self,
        config: AgentConfig,
        session_id: String,
        project_id: String,
        priority: i32,
        prompt: String,
        control: Arc<RunControl>,
        lease: crate::SessionLease,
        resume_journal: Option<crate::run_journal::RunJournal>,
    ) {
        let engine = self.engine.clone();
        let runs = self.runs.clone();
        let global_slots = self.global_slots.clone();
        tokio::spawn(async move {
            control
                .preempt
                .set_phase(crate::preemption::Phase::Queue, None);
            let permit = tokio::select! {
                permit = global_slots.acquire(&session_id, &project_id, priority, &control) => permit,
                _ = wait_for_cancel(&control) => None,
            };
            let Some(_permit) = permit else {
                if let Some(journal) = &resume_journal {
                    let _ = journal
                        .finish_interrupted(
                            &engine,
                            "cancelled",
                            "admission was cancelled before the resumed run started",
                        )
                        .await;
                }
                if let Some(snapshot) = control.preempt_report() {
                    let _ = engine
                        .append_event(
                            &session_id,
                            "agent.cancelled",
                            serde_json::json!({
                                "preempted": true,
                                "reason": snapshot.reason.as_str(),
                                "phase": snapshot.phase.as_str(),
                            }),
                        )
                        .await;
                    let _ = record_queued_preemption(
                        &engine,
                        &session_id,
                        &project_id,
                        &control,
                        &snapshot,
                    )
                    .await;
                } else {
                    let _ = engine
                        .append_event(&session_id, "agent.cancelled", serde_json::json!({}))
                        .await;
                    let _ = control
                        .finalize_session(&engine, &session_id, "cancelled")
                        .await;
                }
                if let Ok(mut runs) = runs.lock() {
                    runs.remove(&session_id);
                }
                lease.release().await;
                return;
            };
            if control.is_cancelled() {
                if let Some(journal) = &resume_journal {
                    let _ = journal
                        .finish_interrupted(
                            &engine,
                            "cancelled",
                            "the resumed run was cancelled before it started",
                        )
                        .await;
                }
                let _ = engine
                    .append_event(&session_id, "agent.cancelled", serde_json::json!({}))
                    .await;
                let _ = control
                    .finalize_session(&engine, &session_id, "cancelled")
                    .await;
                if let Ok(mut runs) = runs.lock() {
                    runs.remove(&session_id);
                }
                lease.release().await;
                return;
            }
            let status = if control.is_paused() {
                "paused"
            } else {
                "running"
            };
            let _ = engine.update_session_status(&session_id, status).await;
            let result = run_agent(
                engine.clone(),
                config,
                &session_id,
                &prompt,
                control.clone(),
                resume_journal,
            )
            .await;
            if let Err(error) = &result {
                let _ = engine
                    .append_event(
                        &session_id,
                        "agent.failed",
                        serde_json::json!({"error": format!("{error:#}")}),
                    )
                    .await;
                let _ = control
                    .finalize_session(&engine, &session_id, "failed")
                    .await;
            }
            if let Ok(mut runs) = runs.lock() {
                runs.remove(&session_id);
            }
            lease.release().await;
        });
    }

    /// Re-enqueue sessions that were admitted but had not started when the
    /// daemon restarted. Their original prompt is read from `agent.queued`.
    pub async fn resume_queued_sessions(&self) -> Result<usize> {
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned());
        let Some(config) = config else {
            return Ok(0);
        };
        let store = self.engine.store().clone();
        let queued = tokio::task::spawn_blocking(move || store.queued_session_prompts()).await??;
        let mut started = 0;
        for (session_id, project_id, prompt, priority) in queued {
            let control = Arc::new(RunControl::new());
            match load_session_project(self.engine.store().clone(), &session_id).await {
                Ok((session, _)) => {
                    match self.engine.get_preemption_policy(&session.project_id).await {
                        Ok(policy) => control.arm_preemption(session.time_budget_secs, policy),
                        Err(error) => tracing::warn!(
                            %error,
                            %session_id,
                            "could not read preemption policy for a queued session"
                        ),
                    }
                }
                Err(error) => tracing::warn!(
                    %error,
                    %session_id,
                    "could not read a queued session for preemption policy"
                ),
            }
            {
                let mut runs = self
                    .runs
                    .lock()
                    .map_err(|_| anyhow!("agent run mutex poisoned"))?;
                if runs.contains_key(&session_id) {
                    continue;
                }
                runs.insert(session_id.clone(), control.clone());
            }
            let lease = match self
                .engine
                .acquire_session_lease(&session_id, std::time::Duration::from_secs(180))
                .await
            {
                Ok(lease) => lease,
                Err(error) => {
                    if let Ok(mut runs) = self.runs.lock() {
                        runs.remove(&session_id);
                    }
                    tracing::warn!(%error, %session_id, "could not acquire queued session lease");
                    continue;
                }
            };
            self.spawn_run(
                config.clone(),
                session_id,
                project_id,
                priority,
                prompt,
                control,
                lease,
                None,
            );
            started += 1;
        }
        Ok(started)
    }
}

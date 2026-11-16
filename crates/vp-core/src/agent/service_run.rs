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

    pub(super) async fn start_inner(&self, session_id: &str, prompt: &str) -> Result<()> {
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

    pub(super) async fn resume_run_inner(
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
    pub(super) fn spawn_run(
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

    /// Serialize a checkpoint with prompt/plan/compaction operations. The
    /// checkpoint itself is provider-free but reads Git state and messages, so
    /// it must not race a run.
    pub async fn checkpoint_now(
        &self,
        session_id: &str,
        label: Option<String>,
    ) -> Result<SessionCheckpoint> {
        self.begin_operation(session_id)?;
        let result = self.engine.checkpoint_session(session_id, label).await;
        self.end_operation(session_id);
        result
    }

    /// Serialize a worktree checkpoint restore with prompts/plans/compaction.
    pub async fn restore_checkpoint_now(
        &self,
        session_id: &str,
        checkpoint_id: &str,
    ) -> Result<SessionCheckpointRestoreResult> {
        self.begin_operation(session_id)?;
        let result = self
            .engine
            .restore_checkpoint(session_id, checkpoint_id)
            .await;
        self.end_operation(session_id);
        result
    }

    /// Serialize a checkpoint diff preview with prompts/plans/compaction.
    pub async fn diff_checkpoint_now(
        &self,
        session_id: &str,
        checkpoint_id: &str,
    ) -> Result<SessionCheckpointDiffResult> {
        self.begin_operation(session_id)?;
        let result = self.engine.diff_checkpoint(session_id, checkpoint_id).await;
        self.end_operation(session_id);
        result
    }

    /// Serialize a fork with prompt/plan/compaction operations on the source
    /// session. The new session is created only after the source is quiescent.
    pub async fn fork_now(
        &self,
        source_session_id: &str,
        checkpoint_id: Option<&str>,
        goal: Option<String>,
    ) -> Result<SessionForkResult> {
        self.begin_operation(source_session_id)?;
        let result = self
            .engine
            .fork_session(source_session_id, checkpoint_id, goal)
            .await;
        self.end_operation(source_session_id);
        result
    }

    /// Merge a direct fork's committed branch into its source session. The
    /// source worktree must be clean; conflicts are aborted and reported.
    pub async fn merge_fork_now(
        &self,
        source_session_id: &str,
        fork_session_id: &str,
    ) -> Result<SessionMergeForkResult> {
        self.begin_operations(&[source_session_id, fork_session_id])?;
        let result = async {
            let integration_command = self.config.read().ok().and_then(|config| {
                config
                    .as_ref()
                    .and_then(|config| config.integration_command.clone())
            });
            let (source, project) =
                load_session_project(self.engine.store().clone(), source_session_id).await?;
            let (fork, fork_project) =
                load_session_project(self.engine.store().clone(), fork_session_id).await?;
            if source.project_id != fork.project_id || project.id != fork_project.id {
                anyhow::bail!("sessions must belong to the same project");
            }
            crate::ensure_session_quiescent(&source, "merge a fork")?;
            crate::ensure_session_quiescent(&fork, "merge a fork")?;
            let lineage = self.engine.session_lineage(fork_session_id).await?;
            if lineage
                .parent
                .as_ref()
                .map(|parent| parent.session.id.as_str())
                != Some(source_session_id)
            {
                anyhow::bail!("fork is not a direct child of the source session");
            }
            self.merge_fork_inner(&source, &project, &fork, integration_command)
                .await
        }
        .await;
        self.end_operations(&[source_session_id, fork_session_id]);
        result
    }

    pub(super) async fn merge_fork_inner(
        &self,
        source: &Session,
        project: &Project,
        fork: &Session,
        integration_command: Option<String>,
    ) -> Result<SessionMergeForkResult> {
        let source_worktree = PathBuf::from(
            source
                .worktree
                .clone()
                .ok_or_else(|| anyhow!("source session has no worktree"))?,
        );
        let source_branch = source
            .branch
            .clone()
            .ok_or_else(|| anyhow!("source session has no branch"))?;
        let fork_branch = fork
            .branch
            .clone()
            .ok_or_else(|| anyhow!("fork session has no branch"))?;
        if !source_worktree.is_dir() {
            anyhow::bail!("source session worktree is missing");
        }
        let git_lock = self.engine.git_lock.clone();
        let status_path = source_worktree.clone();
        let branch_path = source_worktree.clone();
        let head_path = source_worktree.clone();
        let expected_branch = source_branch.clone();
        let (status, branch, pre_merge_commit) = tokio::task::spawn_blocking(move || {
            let _guard = git_lock.lock().map_err(|_| anyhow!("git mutex poisoned"))?;
            Ok::<_, anyhow::Error>((
                worktree::status_porcelain(&status_path)?,
                worktree::current_branch(&branch_path)?,
                worktree::head_commit(&head_path)?,
            ))
        })
        .await??;
        if branch != expected_branch {
            anyhow::bail!("source worktree is on branch {branch}; expected {expected_branch}");
        }
        if !status.is_empty() {
            anyhow::bail!(
                "source worktree has uncommitted changes; commit or discard them before merging a fork"
            );
        }
        if let Some(fork_worktree) = &fork.worktree {
            let fork_path = PathBuf::from(fork_worktree);
            if fork_path.is_dir() {
                let git_lock = self.engine.git_lock.clone();
                let status = tokio::task::spawn_blocking(move || {
                    let _guard = git_lock.lock().map_err(|_| anyhow!("git mutex poisoned"))?;
                    worktree::status_porcelain(&fork_path)
                })
                .await??;
                if !status.is_empty() {
                    anyhow::bail!(
                        "fork worktree has uncommitted changes; commit them before merging"
                    );
                }
            }
        }
        let root = PathBuf::from(&project.root);
        let fork_branch_for_git = fork_branch.clone();
        let root_for_git = root.clone();
        let fork_commit = tokio::task::spawn_blocking(move || {
            worktree::git_output(&root_for_git, &["rev-parse", &fork_branch_for_git])
        })
        .await??;
        let message = format!("merge fork {}", fork.id);
        let merge_worktree = source_worktree.clone();
        let merge_lock = self.engine.git_lock.clone();
        let merge_outcome = tokio::task::spawn_blocking(move || {
            let _guard = merge_lock
                .lock()
                .map_err(|_| anyhow!("git mutex poisoned"))?;
            worktree::merge_fork_branch(&merge_worktree, &fork_branch, &message)
        })
        .await??;
        if !merge_outcome.merged {
            let detail = truncate(&format!("merge conflict: {}", merge_outcome.detail));
            self.record_fork_merge(
                source,
                fork,
                "conflict",
                &pre_merge_commit,
                &fork_commit,
                None,
                &merge_outcome.conflicts,
                &detail,
            )
            .await?;
            return Ok(SessionMergeForkResult {
                source_session_id: source.id.clone(),
                fork_session_id: fork.id.clone(),
                status: "conflict".to_string(),
                pre_merge_commit,
                fork_commit,
                merge_commit: None,
                conflicts: merge_outcome.conflicts,
                detail,
            });
        }

        let merge_worktree = source_worktree.clone();
        let check_base = pre_merge_commit.clone();
        let check_lock = self.engine.git_lock.clone();
        let check = tokio::task::spawn_blocking(move || {
            let _guard = check_lock
                .lock()
                .map_err(|_| anyhow!("git mutex poisoned"))?;
            worktree::check_merged_diff(&merge_worktree, &check_base)
        })
        .await?;
        let integration = match check {
            Ok(()) => {
                if let Some(command) = integration_command {
                    run_integration_command(
                        &self.engine,
                        project,
                        source,
                        source_worktree.clone(),
                        command.clone(),
                    )
                    .await
                } else {
                    Ok("diff check passed".to_string())
                }
            }
            Err(error) => Err(error),
        };
        let integration = match integration {
            Ok(detail) => detail,
            Err(error) => {
                let reset_worktree = source_worktree.clone();
                let reset_base = pre_merge_commit.clone();
                let reset_lock = self.engine.git_lock.clone();
                let reset = tokio::task::spawn_blocking(move || {
                    let _guard = reset_lock
                        .lock()
                        .map_err(|_| anyhow!("git mutex poisoned"))?;
                    worktree::reset_hard(&reset_worktree, &reset_base)
                })
                .await?;
                let detail = truncate(&match reset {
                    Ok(()) => format!("integration check failed: {error}"),
                    Err(reset_error) => {
                        format!("integration check failed: {error}; rollback failed: {reset_error}")
                    }
                });
                self.record_fork_merge(
                    source,
                    fork,
                    "failed",
                    &pre_merge_commit,
                    &fork_commit,
                    None,
                    &[],
                    &detail,
                )
                .await?;
                return Ok(SessionMergeForkResult {
                    source_session_id: source.id.clone(),
                    fork_session_id: fork.id.clone(),
                    status: "failed".to_string(),
                    pre_merge_commit,
                    fork_commit,
                    merge_commit: None,
                    conflicts: Vec::new(),
                    detail,
                });
            }
        };
        let head_path = source_worktree.clone();
        let merge_commit =
            tokio::task::spawn_blocking(move || worktree::head_commit(&head_path)).await??;
        let detail = truncate(&integration);
        self.record_fork_merge(
            source,
            fork,
            "merged",
            &pre_merge_commit,
            &fork_commit,
            Some(&merge_commit),
            &[],
            &detail,
        )
        .await?;
        Ok(SessionMergeForkResult {
            source_session_id: source.id.clone(),
            fork_session_id: fork.id.clone(),
            status: "merged".to_string(),
            pre_merge_commit,
            fork_commit,
            merge_commit: Some(merge_commit),
            conflicts: Vec::new(),
            detail,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn record_fork_merge(
        &self,
        source: &Session,
        fork: &Session,
        status: &str,
        pre_merge_commit: &str,
        fork_commit: &str,
        merge_commit: Option<&str>,
        conflicts: &[String],
        detail: &str,
    ) -> Result<()> {
        self.engine
            .append_event(
                &source.id,
                "session.fork.merge",
                serde_json::json!({
                    "fork_session_id": fork.id,
                    "status": status,
                    "pre_merge_commit": pre_merge_commit,
                    "fork_commit": fork_commit,
                    "merge_commit": merge_commit,
                    "conflicts": conflicts,
                    "detail": detail,
                }),
            )
            .await?;
        let _ = self
            .engine
            .append_event(
                &fork.id,
                "session.fork.merge",
                serde_json::json!({
                    "source_session_id": source.id,
                    "status": status,
                    "merge_commit": merge_commit,
                }),
            )
            .await;
        Ok(())
    }

    pub(super) fn begin_manual_operation(&self, session_id: &str) -> Result<Arc<RunControl>> {
        let mut runs = self
            .runs
            .lock()
            .map_err(|_| anyhow!("agent run mutex poisoned"))?;
        if runs.contains_key(session_id) {
            anyhow::bail!("session is already running");
        }
        let control = Arc::new(RunControl::new_manual());
        runs.insert(session_id.to_string(), control.clone());
        Ok(control)
    }

    /// A resumed or normal long run is cancellable and pausable. Foreground
    /// operations use the non-cancellable variants above.
    pub(super) fn begin_resumable_run(
        &self,
        session_id: &str,
        time_budget_secs: Option<u64>,
        policy: vp_protocol::types::PreemptionPolicy,
    ) -> Result<Arc<RunControl>> {
        let mut runs = self
            .runs
            .lock()
            .map_err(|_| anyhow!("agent run mutex poisoned"))?;
        if runs.contains_key(session_id) {
            anyhow::bail!("session is already running");
        }
        let control = Arc::new(RunControl::new());
        control.arm_preemption(time_budget_secs, policy);
        runs.insert(session_id.to_string(), control.clone());
        Ok(control)
    }

    pub(super) fn begin_operation(&self, session_id: &str) -> Result<()> {
        let mut runs = self
            .runs
            .lock()
            .map_err(|_| anyhow!("agent run mutex poisoned"))?;
        if runs.contains_key(session_id) {
            anyhow::bail!("session is already running");
        }
        runs.insert(
            session_id.to_string(),
            Arc::new(RunControl::new_operation()),
        );
        Ok(())
    }

    pub(super) fn end_operation(&self, session_id: &str) {
        if let Ok(mut runs) = self.runs.lock() {
            runs.remove(session_id);
        }
    }

    pub(super) fn begin_operations(&self, session_ids: &[&str]) -> Result<()> {
        let mut ids = session_ids.to_vec();
        ids.sort_unstable();
        ids.dedup();
        let mut runs = self
            .runs
            .lock()
            .map_err(|_| anyhow!("agent run mutex poisoned"))?;
        for id in &ids {
            if runs.contains_key(*id) {
                anyhow::bail!("session is already running: {id}");
            }
        }
        for id in ids {
            runs.insert(id.to_string(), Arc::new(RunControl::new_operation()));
        }
        Ok(())
    }

    pub(super) fn end_operations(&self, session_ids: &[&str]) {
        if let Ok(mut runs) = self.runs.lock() {
            for id in session_ids {
                runs.remove(*id);
            }
        }
    }

    /// Apply a user-requested plan revision with the configured Manager cap.
    pub async fn revise_team_plan(
        &self,
        session_id: &str,
        plan: TeamPlan,
        reason: &str,
        expected_revision: Option<u32>,
    ) -> Result<ReviseTeamPlanResult> {
        let max_managers = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().map(|config| config.max_managers))
            .unwrap_or(5);
        self.engine
            .revise_team_plan(
                session_id,
                plan,
                reason,
                "user",
                expected_revision,
                max_managers,
            )
            .await
    }

    pub async fn plan_revision_detail(
        &self,
        session_id: &str,
        revision: u32,
    ) -> Result<PlanRevisionDetailResult> {
        self.engine.plan_revision_detail(session_id, revision).await
    }

    pub async fn restore_team_plan_revision(
        &self,
        session_id: &str,
        revision: u32,
        reason: &str,
        expected_revision: Option<u32>,
    ) -> Result<ReviseTeamPlanResult> {
        let max_managers = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().map(|config| config.max_managers))
            .unwrap_or(5);
        self.engine
            .restore_team_plan_revision(
                session_id,
                revision,
                reason,
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
    ) -> Result<ReviseTeamPlanResult> {
        let max_managers = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().map(|config| config.max_managers))
            .unwrap_or(5);
        self.engine
            .edit_plan_task(session_id, task, reason, expected_revision, max_managers)
            .await
    }

    /// Resume a stored TeamPlan without another Leader model call. Completed
    /// tasks are skipped; failed/blocked/cancelled/pending tasks are retried in
    /// dependency batches.
    pub async fn resume_plan(&self, session_id: &str) -> Result<bool> {
        self.start_plan_run(session_id, None).await
    }

    /// Advance pending stall-recovery requests. This is deliberately separate
    /// from the task scheduler: it first waits for the cancelled task to become
    /// terminal and for the session to become quiescent, then reuses the normal
    /// `retry_plan_task` + `resume_plan` path once. It never widens a budget,
    /// capacity, write-scope, approval, Verifier, or merge gate.
    pub async fn reconcile_stall_recoveries(&self) -> Result<usize> {
        let store = self.engine.store().clone();
        let rows =
            tokio::task::spawn_blocking(move || store.list_active_orchestration_recoveries(200))
                .await??;
        let mut advanced = 0usize;
        for recovery in rows {
            if self.reconcile_stall_recovery(recovery).await? {
                advanced = advanced.saturating_add(1);
            }
        }
        Ok(advanced)
    }

    pub(super) async fn reconcile_stall_recovery(
        &self,
        recovery: OrchestrationRecovery,
    ) -> Result<bool> {
        let store = self.engine.store().clone();
        let session_id = recovery.session_id.clone();
        let Some(session) =
            tokio::task::spawn_blocking(move || store.get_session(&session_id)).await??
        else {
            return Ok(false);
        };
        let policy = self
            .engine
            .orchestration_policy(&session.project_id)
            .await?;
        if policy.stall_recovery != StallRecoveryMode::Retry {
            // The project policy changed after the request. Leave the row
            // untouched rather than mutating scheduling state under observe.
            return Ok(false);
        }
        let store = self.engine.store().clone();
        let session_id = recovery.session_id.clone();
        let tasks =
            tokio::task::spawn_blocking(move || store.list_plan_tasks(None, Some(&session_id)))
                .await??;
        let Some(task) = tasks
            .iter()
            .find(|task| task.plan_seq == recovery.plan_seq && task.task_id == recovery.task_id)
        else {
            return Ok(false);
        };
        let session_terminal = matches!(
            session.status.as_str(),
            "idle" | "failed" | "cancelled" | "draft"
        );
        let now = time::OffsetDateTime::now_utc();
        let grace_expired = recovery
            .next_action_at
            .as_deref()
            .and_then(|value| {
                time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
                    .ok()
            })
            .is_some_and(|deadline| now >= deadline);

        // A crash can land after the durable intent but before the cancel
        // request. Re-issue the same cooperative cancellation once the task
        // row is observed again.
        if recovery.status == "requested" && matches!(task.status.as_str(), "running" | "claimed") {
            match self
                .engine
                .request_plan_task_cancel(&recovery.session_id, &recovery.task_id)
                .await
            {
                Ok(_) => {
                    self.mark_recovery_waiting_cancel(
                        &recovery,
                        "cancellation requested; waiting for the task and session to settle",
                    )
                    .await?;
                    return Ok(true);
                }
                Err(error) => {
                    if grace_expired {
                        self.fail_recovery(
                            &recovery,
                            &format!(
                                "stall recovery grace expired before cancellation could be re-requested: {error:#}"
                            ),
                        )
                        .await?;
                        return Ok(true);
                    }
                    return Ok(false);
                }
            }
        }

        match task.status.as_str() {
            "completed" | "skipped" => {
                self.finish_recovery_unnecessary(&recovery, "task completed before recovery retry")
                    .await?;
                Ok(true)
            }
            "cancelled" | "failed" | "interrupted" => {
                if !session_terminal {
                    if grace_expired {
                        self.fail_recovery(
                            &recovery,
                            "stall recovery grace expired before the session became idle",
                        )
                        .await?;
                        return Ok(true);
                    }
                    return Ok(false);
                }
                if crate::session_budget_exhausted(&session) {
                    self.fail_recovery(
                        &recovery,
                        "session budget is exhausted; the stalled task was not resumed",
                    )
                    .await?;
                    return Ok(true);
                }
                if recovery.attempts > policy.stall_max_attempts {
                    self.fail_recovery(&recovery, "stall recovery retry budget is exhausted")
                        .await?;
                    return Ok(true);
                }
                match self
                    .engine
                    .retry_plan_task(&recovery.session_id, &recovery.task_id)
                    .await
                {
                    Ok(_) => {
                        self.complete_recovery_retry(&recovery).await?;
                        if let Err(error) = self.resume_plan(&recovery.session_id).await {
                            tracing::warn!(
                                %error,
                                session_id = %recovery.session_id,
                                task_id = %recovery.task_id,
                                "stall recovery retried a task but the plan resume failed"
                            );
                        }
                    }
                    Err(error) => {
                        self.fail_recovery(
                            &recovery,
                            &format!("stall recovery retry failed: {error:#}"),
                        )
                        .await?;
                    }
                }
                Ok(true)
            }
            "pending" if session_terminal => {
                // A crash can land after `plan_task.retried` and before the
                // recovery row is finalized. Treat the pending task as the
                // already-applied retry, finalize the row, and resume once.
                if crate::session_budget_exhausted(&session) {
                    self.fail_recovery(
                        &recovery,
                        "session budget is exhausted; the stalled task was not resumed",
                    )
                    .await?;
                    return Ok(true);
                }
                self.complete_recovery_retry(&recovery).await?;
                if let Err(error) = self.resume_plan(&recovery.session_id).await {
                    tracing::warn!(
                        %error,
                        session_id = %recovery.session_id,
                        task_id = %recovery.task_id,
                        "stall recovery resume after a partial retry failed"
                    );
                }
                Ok(true)
            }
            _ => {
                if grace_expired {
                    self.fail_recovery(
                        &recovery,
                        "stall recovery grace expired before the task became terminal",
                    )
                    .await?;
                    return Ok(true);
                }
                Ok(false)
            }
        }
    }

    pub(super) async fn complete_recovery_retry(
        &self,
        recovery: &OrchestrationRecovery,
    ) -> Result<()> {
        let store = self.engine.store().clone();
        let session_id = recovery.session_id.clone();
        let task_id = recovery.task_id.clone();
        let plan_seq = recovery.plan_seq;
        let result = tokio::task::spawn_blocking(move || {
            store.complete_stall_recovery_retry(&session_id, plan_seq, &task_id)
        })
        .await??;
        if let Some((_row, event)) = result {
            self.engine.broadcast_event(event);
        }
        Ok(())
    }

    pub(super) async fn mark_recovery_waiting_cancel(
        &self,
        recovery: &OrchestrationRecovery,
        reason: &str,
    ) -> Result<()> {
        let store = self.engine.store().clone();
        let session_id = recovery.session_id.clone();
        let task_id = recovery.task_id.clone();
        let plan_seq = recovery.plan_seq;
        let reason = reason.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.mark_stall_recovery_waiting_cancel(&session_id, plan_seq, &task_id, &reason)
        })
        .await??;
        if let Some((_row, event)) = result {
            self.engine.broadcast_event(event);
        }
        Ok(())
    }

    pub(super) async fn finish_recovery_unnecessary(
        &self,
        recovery: &OrchestrationRecovery,
        reason: &str,
    ) -> Result<()> {
        let store = self.engine.store().clone();
        let session_id = recovery.session_id.clone();
        let task_id = recovery.task_id.clone();
        let plan_seq = recovery.plan_seq;
        let reason = reason.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.finish_stall_recovery_unnecessary(&session_id, plan_seq, &task_id, &reason)
        })
        .await??;
        if let Some((_row, event)) = result {
            self.engine.broadcast_event(event);
        }
        Ok(())
    }

    pub(super) async fn fail_recovery(
        &self,
        recovery: &OrchestrationRecovery,
        reason: &str,
    ) -> Result<()> {
        let store = self.engine.store().clone();
        let session_id = recovery.session_id.clone();
        let task_id = recovery.task_id.clone();
        let plan_seq = recovery.plan_seq;
        let reason_for_store = reason.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.fail_stall_recovery(&session_id, plan_seq, &task_id, &reason_for_store)
        })
        .await??;
        if let Some((_row, event)) = result {
            self.engine.broadcast_event(event);
            self.engine
                .record_stall_escalation(&recovery.session_id, &recovery.task_id, reason)
                .await?;
        }
        Ok(())
    }

    /// Read-only preview of the per-role prompt cache plan and component
    /// hashes. It never calls a provider, creates a worktree, or starts a run.
    /// Operators use it to verify that a source/skill edit moves only the
    /// project component and that a policy TTL/breakpoint plan is what they
    /// expect before spending tokens on a warm.
    pub async fn cache_plan_now(
        &self,
        session_id: &str,
        roles: &[String],
    ) -> Result<SessionCachePlanResult> {
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned())
            .ok_or_else(|| anyhow!("agent provider is not configured"))?;
        let (session, project) =
            load_session_project(self.engine.store().clone(), session_id).await?;
        if session.status == "imported" {
            anyhow::bail!("imported sessions are read-only archives");
        }
        let requested = if roles.is_empty() {
            vec!["leader".to_string()]
        } else {
            roles.to_vec()
        };
        let agents = self.engine.list_agents(session_id).await?;
        let policy = self.engine.get_cache_policy(&project.id).await?;
        let mut plans = Vec::with_capacity(requested.len());
        for name in requested {
            let role = AgentRole::parse(name.trim().to_ascii_lowercase().as_str())
                .ok_or_else(|| anyhow!("unknown cache-plan role: {name}"))?;
            let agent = agents
                .iter()
                .find(|agent| agent.role == role.as_str())
                .cloned();
            let task_kind = plan_task_kind_for_agent(&self.engine, &session.id, &agent).await;
            let spec = stable_agent_spec(&session, agent, role, task_kind);
            let sections = build_prompt_sections(
                &self.engine,
                &project,
                &session,
                &spec,
                None,
                &session.goal,
                false,
                InstructionBuildMode::Preview,
                None,
            )
            .await?;
            let dynamic_mcp =
                collect_dynamic_mcp_tools(&self.engine, spec.role, &session.project_id, None).await;
            let frozen_bundle = frozen_tool_bundle(&config, &spec, &dynamic_mcp);
            let tools = frozen_bundle.tools.clone();
            let system = sections.render();
            let hashes = sections.hashes(&model_for(&config, spec.role), &tools);
            let plan = crate::prompt_cache::request_cache_plan(
                &config.provider_name,
                &sections,
                tools.len(),
                1,
                &policy,
                spec.role.as_str(),
            );
            plans.push(SessionCachePlanRole {
                role: spec.role.as_str().to_string(),
                provider: config.provider_name.clone(),
                model: model_for(&config, spec.role),
                cache_breakpoints: plan.breakpoints,
                cache_ttl: plan.ttl.as_str().to_string(),
                ttl_explicit: plan.ttl_explicit,
                automatic: plan.automatic,
                tail_message: plan.tail_message,
                tool_breakpoint: plan.tool_breakpoint,
                core_breakpoint: plan.core_breakpoint,
                project_breakpoint: plan.project_breakpoint,
                core_hash: hashes.core_hash,
                project_hash: hashes.project_hash,
                tools_hash: hashes.tools_hash,
                volatile_hash: hashes.volatile_hash,
                prefix_hash: hashes.prefix_hash,
                identity_hash: hashes.identity_hash,
                identity_id: hashes.identity_id,
                identity_generation: hashes.identity_generation,
                prompt_layout: hashes.prompt_layout,
                stable_prefix_hash: hashes.stable_prefix_hash,
                head_sections: hashes.head_sections,
                tail_sections: hashes.tail_sections,
                section_chars: hashes.section_chars,
                core_chars: sections.core.chars().count() as u64,
                project_chars: sections.project.chars().count() as u64,
                stable_prefix_chars: sections.stable_prefix().chars().count() as u64,
                tool_count: tools.len(),
                tool_bundle: frozen_bundle.selection.kind.as_str().to_string(),
                tool_bundle_hash: frozen_bundle.bundle_hash.clone(),
                tool_bundle_tool_count: frozen_bundle.builtin_count(),
                tool_bundle_mcp_count: frozen_bundle.mcp_count(),
                tool_bundle_target_limit: frozen_bundle.target_limit,
                tool_bundle_within_target: frozen_bundle.within_target(),
                tool_bundle_fallback: frozen_bundle.selection.fallback,
                system_tokens_estimate: estimate_context_tokens(&system),
                tool_tokens_estimate: serde_json::to_string(&tools)
                    .map(|schema| estimate_context_tokens(&schema))
                    .unwrap_or(0),
            });
        }
        Ok(SessionCachePlanResult {
            session_id: session.id,
            provider: config.provider_name,
            policy: Some(policy),
            roles: plans,
        })
    }

    /// Read-only tool-bundle inspection for one session. It reports the frozen
    /// bundle the next run would use for each role, the exact built-in and MCP
    /// counts, the canonical hashes, and the latest observed prefix event. It
    /// never calls a provider, creates a worktree, or starts a run.
    pub async fn tool_bundle_now(
        &self,
        session_id: &str,
        roles: &[String],
        task_kind: Option<&str>,
        include_tools: bool,
    ) -> Result<ToolBundleResult> {
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned())
            .ok_or_else(|| anyhow!("agent provider is not configured"))?;
        let (session, _project) =
            load_session_project(self.engine.store().clone(), session_id).await?;
        let agents = self.engine.list_agents(session_id).await?;
        let mut requested = if roles.is_empty() {
            let mut names = agents
                .iter()
                .map(|agent| agent.role.clone())
                .collect::<Vec<_>>();
            names.push(AgentRole::Leader.as_str().to_string());
            names
        } else {
            roles.to_vec()
        };
        let mut seen = HashSet::new();
        requested.retain(|name| {
            let normalized = name.trim().to_ascii_lowercase();
            !normalized.is_empty() && seen.insert(normalized)
        });
        if requested.is_empty() {
            requested.push(AgentRole::Leader.as_str().to_string());
        }
        let plan = self.engine.get_team_plan(&session.id).await.ok().flatten();
        let recent_prefixes = self
            .engine
            .recent_events_of_kind(session_id, "context.prompt_prefix", 400)
            .await?;
        let explicit_task_kind = task_kind
            .map(str::trim)
            .filter(|kind| !kind.is_empty())
            .map(str::to_string);
        let mut results = Vec::with_capacity(requested.len());
        for name in requested {
            let role = AgentRole::parse(name.trim().to_ascii_lowercase().as_str())
                .ok_or_else(|| anyhow!("unknown tool-bundle role: {name}"))?;
            let agent = agents
                .iter()
                .find(|agent| agent.role == role.as_str())
                .cloned();
            let profile = agent
                .as_ref()
                .and_then(|agent| agent.profile.as_deref())
                .and_then(AgentProfile::parse)
                .unwrap_or_else(|| default_profile_for_preview(role));
            let inferred_kind = agent.as_ref().and_then(|agent| {
                let task_id = agent.task_id.as_ref()?;
                plan.as_ref()?
                    .tasks
                    .iter()
                    .find(|task| task.id == *task_id)
                    .map(|task| task.kind.clone())
            });
            let task_kind = explicit_task_kind.clone().or(inferred_kind);
            let spec = stable_agent_spec(&session, agent, role, task_kind);
            let dynamic_mcp =
                collect_dynamic_mcp_tools(&self.engine, spec.role, &session.project_id, None).await;
            let frozen = frozen_tool_bundle(&config, &spec, &dynamic_mcp);
            let tools = if include_tools {
                frozen
                    .tools
                    .iter()
                    .map(|tool| {
                        let spec = crate::tool_bundle::catalog_entry(&tool.name);
                        ToolBundleToolInfo {
                            name: tool.name.clone(),
                            category: frozen.category_for(&tool.name).to_string(),
                            workspace_write: spec.is_some_and(|spec| spec.workspace_write),
                            delegation: spec.is_some_and(|spec| spec.delegation),
                            schema_hash: crate::tool_bundle::tool_schema_hash(tool),
                            description_chars: tool.description.chars().count() as u64,
                            parameter_fields: tool
                                .parameters
                                .get("properties")
                                .and_then(serde_json::Value::as_object)
                                .map_or(0, serde_json::Map::len)
                                as u64,
                        }
                    })
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            let last_run = recent_prefixes
                .iter()
                .rev()
                .find(|event| {
                    event
                        .payload
                        .get("agent_role")
                        .and_then(serde_json::Value::as_str)
                        == Some(role.as_str())
                        && event.payload.get("tool_bundle").is_some()
                })
                .map(|event| ToolBundleLastRun {
                    agent_id: event
                        .payload
                        .get("agent_id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    agent_name: event
                        .payload
                        .get("agent_name")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    role: event
                        .payload
                        .get("agent_role")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or(role.as_str())
                        .to_string(),
                    bundle: event
                        .payload
                        .get("tool_bundle")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown")
                        .to_string(),
                    bundle_hash: event
                        .payload
                        .get("tool_bundle_hash")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    tools_hash: event
                        .payload
                        .get("tools_hash")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    tool_count: event
                        .payload
                        .get("tool_count")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0),
                    mcp_tool_count: event
                        .payload
                        .get("tool_bundle_mcp_count")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0),
                    created_at: event.created_at.clone(),
                });
            results.push(ToolBundleRoleResult {
                role: role.as_str().to_string(),
                profile: profile.as_str().to_string(),
                task_kind: frozen.selection.task_kind.clone(),
                bundle: frozen.selection.kind.as_str().to_string(),
                title: crate::tool_bundle::definition(frozen.selection.kind)
                    .title
                    .to_string(),
                reason: frozen.selection.reason.to_string(),
                fallback: frozen.selection.fallback,
                bundle_hash: frozen.bundle_hash.clone(),
                tools_hash: frozen.tools_hash.clone(),
                requested: frozen.requested.clone(),
                omitted: frozen.omitted.clone(),
                warnings: frozen
                    .warnings
                    .iter()
                    .map(|warning| ToolBundleWarningInfo {
                        code: warning.code.to_string(),
                        message: warning.message.clone(),
                    })
                    .collect(),
                builtin_count: frozen.builtin_count(),
                mcp_count: frozen.mcp_count(),
                total_count: frozen.tool_count(),
                schema_bytes: frozen.schema_bytes(),
                estimated_tool_tokens: frozen.estimated_tokens(),
                target_limit: frozen.target_limit,
                within_target: frozen.within_target(),
                categories: frozen
                    .category_counts()
                    .into_iter()
                    .map(|count| ToolBundleCategoryCount {
                        category: count.category.to_string(),
                        tools: count.tools,
                        schema_bytes: count.schema_bytes,
                    })
                    .collect(),
                tools,
                last_run,
            });
        }
        let catalog = crate::tool_bundle::catalog_entries()
            .into_iter()
            .map(|entry| ToolBundleCatalogRow {
                bundle: entry.kind.as_str().to_string(),
                title: entry.title.to_string(),
                summary: entry.summary.to_string(),
                target_limit: entry.target_limit,
                requested_count: entry.requested_count,
                representative_role: entry.representative_role.to_string(),
                representative_profile: entry.representative_profile.to_string(),
                allows_dynamic_mcp: entry.allows_dynamic_mcp,
            })
            .collect();
        Ok(ToolBundleResult {
            session_id: session.id,
            roles: results,
            catalog,
        })
    }

    /// Bounded history of persisted tool-bundle observations for a session.
    /// It reads `context.prompt_prefix` events only; it never reconstructs a
    /// historical schema or claims that the current registry matches the run.
    pub async fn tool_bundle_history(
        &self,
        session_id: &str,
        role: Option<&str>,
        limit: Option<u64>,
    ) -> Result<ToolBundleHistoryResult> {
        let (session, _project) =
            load_session_project(self.engine.store().clone(), session_id).await?;
        let role = match role {
            Some(role) => Some(
                AgentRole::parse(role.trim().to_ascii_lowercase().as_str())
                    .ok_or_else(|| anyhow!("unknown tool-bundle role: {role}"))?,
            ),
            None => None,
        };
        let limit = limit.unwrap_or(50).clamp(1, 200) as usize;
        let events = self
            .engine
            .recent_events_of_kind(session_id, "context.prompt_prefix", 2_000)
            .await?;
        let entries = events
            .iter()
            .rev()
            .filter(|event| event.payload.get("tool_bundle").is_some())
            .filter(|event| {
                role.is_none_or(|role| {
                    event
                        .payload
                        .get("agent_role")
                        .and_then(serde_json::Value::as_str)
                        == Some(role.as_str())
                })
            })
            .take(limit)
            .map(|event| ToolBundleHistoryEntry {
                seq: event.seq,
                role: event
                    .payload
                    .get("agent_role")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                agent_id: event
                    .payload
                    .get("agent_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                agent_name: event
                    .payload
                    .get("agent_name")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                bundle: event
                    .payload
                    .get("tool_bundle")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                bundle_hash: event
                    .payload
                    .get("tool_bundle_hash")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                tools_hash: event
                    .payload
                    .get("tools_hash")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                tool_count: event
                    .payload
                    .get("tool_count")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                mcp_tool_count: event
                    .payload
                    .get("tool_bundle_mcp_count")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                task_kind: event
                    .payload
                    .get("tool_bundle_task_kind")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                fallback: event
                    .payload
                    .get("tool_bundle_fallback")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                reason: event
                    .payload
                    .get("tool_bundle_reason")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                created_at: event.created_at.clone(),
            })
            .collect();
        Ok(ToolBundleHistoryResult {
            session_id: session.id,
            entries,
        })
    }

    /// Deterministic preview diff between two named bundles for one role and
    /// profile. Empty bundles yield an empty list; unknown names fail closed.
    pub fn tool_bundle_diff(
        &self,
        from_bundle: &str,
        to_bundle: &str,
        role: Option<&str>,
        profile: Option<&str>,
        task_kind: Option<&str>,
    ) -> Result<ToolBundleDiffResult> {
        let config = self.configured_agent()?;
        let from_kind = crate::tool_bundle::ToolBundleKind::parse(from_bundle)
            .ok_or_else(|| anyhow!("unknown tool bundle: {from_bundle}"))?;
        let to_kind = crate::tool_bundle::ToolBundleKind::parse(to_bundle)
            .ok_or_else(|| anyhow!("unknown tool bundle: {to_bundle}"))?;
        let role = AgentRole::parse(role.unwrap_or(AgentRole::Leader.as_str()))
            .ok_or_else(|| anyhow!("unknown tool-bundle role"))?;
        let profile = match profile {
            Some(profile) => AgentProfile::parse(profile)
                .ok_or_else(|| anyhow!("unknown tool-bundle profile: {profile}"))?,
            None => default_profile_for_preview(role),
        };
        let task_kind = task_kind
            .map(str::trim)
            .filter(|kind| !kind.is_empty())
            .map(str::to_string);
        let spec = preview_spec(role, profile, task_kind.clone());
        let from = frozen_tool_bundle_for_kind(&config, &spec, from_kind);
        let to = frozen_tool_bundle_for_kind(&config, &spec, to_kind);
        let from_names = from
            .tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let to_names = to
            .tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let added = to_names
            .difference(&from_names)
            .cloned()
            .collect::<Vec<_>>();
        let removed = from_names
            .difference(&to_names)
            .cloned()
            .collect::<Vec<_>>();
        let shared = from_names
            .intersection(&to_names)
            .cloned()
            .collect::<Vec<_>>();
        let mut warnings = from
            .warnings
            .iter()
            .chain(to.warnings.iter())
            .map(|warning| ToolBundleWarningInfo {
                code: warning.code.to_string(),
                message: warning.message.clone(),
            })
            .collect::<Vec<_>>();
        warnings.sort_by(|left, right| {
            left.code
                .cmp(&right.code)
                .then_with(|| left.message.cmp(&right.message))
        });
        warnings.dedup();
        Ok(ToolBundleDiffResult {
            from_bundle: from_kind.as_str().to_string(),
            to_bundle: to_kind.as_str().to_string(),
            role: role.as_str().to_string(),
            profile: profile.as_str().to_string(),
            task_kind: spec.task_kind.clone(),
            from_hash: from.tools_hash.clone(),
            to_hash: to.tools_hash.clone(),
            from_count: from.tool_count(),
            to_count: to.tool_count(),
            added,
            removed,
            shared,
            changed: from.tools_hash != to.tools_hash,
            warnings,
        })
    }

    /// Explicitly warm the stable core/project prompt prefix and tool bundle.
    /// This is the only operator-triggered cache warm path. It never runs
    /// automatically unless the project policy sets `warm_on_resume`; it
    /// records its own usage and cost in the normal ledger.
    pub async fn warm_cache_now(
        &self,
        session_id: &str,
        role: Option<&str>,
    ) -> Result<SessionCacheWarmResult> {
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned())
            .ok_or_else(|| anyhow!("agent provider is not configured"))?;
        let role = match role {
            Some(role) => AgentRole::parse(role.trim().to_ascii_lowercase().as_str())
                .ok_or_else(|| anyhow!("unknown cache-warm role: {role}"))?,
            None => AgentRole::Leader,
        };
        let (session, project) =
            load_session_project(self.engine.store().clone(), session_id).await?;
        if session.status == "imported" {
            anyhow::bail!("imported sessions are read-only archives");
        }
        if matches!(
            session.status.as_str(),
            "queued" | "running" | "paused" | "waiting_input"
        ) {
            anyhow::bail!(
                "session must be idle, failed, or cancelled before an explicit cache warm"
            );
        }
        let _control = self.begin_manual_operation(session_id)?;
        let result = async {
            let agents = self.engine.list_agents(session_id).await?;
            let agent = agents.into_iter().find(|agent| agent.role == role.as_str());
            let task_kind = plan_task_kind_for_agent(&self.engine, &session.id, &agent).await;
            let spec = stable_agent_spec(&session, agent, role, task_kind);
            // Warm only the stable sections. Memory, objective, and
            // conversation history are deliberately excluded.
            let sections = build_prompt_sections(
                &self.engine,
                &project,
                &session,
                &spec,
                None,
                &session.goal,
                false,
                InstructionBuildMode::Materialize,
                None,
            )
            .await?;
            let dynamic_mcp =
                collect_dynamic_mcp_tools(&self.engine, spec.role, &session.project_id, None).await;
            let frozen_bundle = frozen_tool_bundle(&config, &spec, &dynamic_mcp);
            let tools = frozen_bundle.tools.clone();
            let budget = SessionBudget::new(
                session.token_budget,
                session.tokens_in + session.tokens_out,
                AgentCaps {
                    managers: config.max_managers,
                    workers: config.max_workers,
                    labour: config.max_labour,
                },
            )
            .with_cost(session.cost_budget_usd, session.cost_spent_usd)
            .with_time(session.time_budget_secs)
            .with_unlimited_flag(self.engine.unlimited_flag(&session.id, session.unlimited))
            .with_scaling_slots(self.engine.scaling_slots(
                &session.id,
                &session.project_id,
                config.max_workers,
                config.max_labour,
            ));
            if budget.exhausted() {
                anyhow::bail!("session budget is exhausted; cache warm needs provider tokens");
            }
            warm_prompt_cache(
                &self.engine,
                &config,
                &session,
                &spec,
                &budget,
                &sections,
                &tools,
                "manual",
            )
            .await
        }
        .await;
        self.end_operation(session_id);
        result
    }

    /// Force a summary/trim of an idle session's active conversation. This is
    /// the explicit counterpart to automatic compaction: it never starts a
    /// prompt, and it refuses to run while the session is active so the
    /// in-memory conversation of a live run cannot diverge from the event log.
    pub async fn compact_now(&self, session_id: &str) -> Result<SessionCompactResult> {
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned())
            .ok_or_else(|| anyhow!("agent provider is not configured"))?;
        let (session, project) =
            load_session_project(self.engine.store().clone(), session_id).await?;
        if session.status == "imported" {
            anyhow::bail!("imported sessions are read-only archives");
        }
        if matches!(
            session.status.as_str(),
            "queued" | "running" | "paused" | "waiting_input"
        ) {
            anyhow::bail!(
                "session must not be queued, running, paused, or waiting for input while compacting context"
            );
        }
        // Reuse the run registry so a prompt cannot start while the
        // compaction call is in flight. The manual control is cancellable but
        // not pausable.
        let control = self.begin_manual_operation(session_id)?;
        let result = async {
            let agents = self.engine.list_agents(session_id).await?;
            let leader = agents.into_iter().find(|agent| agent.role == "leader");
            let profile = leader
                .as_ref()
                .and_then(|agent| agent.profile.as_deref())
                .and_then(AgentProfile::parse)
                .unwrap_or(AgentProfile::Generic);
            let worktree = PathBuf::from(session.worktree.clone().unwrap_or_default());
            let branch = session.branch.clone().unwrap_or_default();
            let spec = AgentSpec {
                id: leader
                    .as_ref()
                    .map(|agent| agent.id.clone())
                    .unwrap_or_else(|| "leader".to_string()),
                role: AgentRole::Leader,
                name: leader
                    .as_ref()
                    .map(|agent| agent.name.clone())
                    .unwrap_or_else(|| "Leader".to_string()),
                depth: 0,
                parent_name: None,
                title: leader.as_ref().and_then(|agent| agent.title.clone()),
                task_id: None,
                department: None,
                identity_id: None,
                identity_generation: None,
                profile,
                allow_delegation: true,
                task_kind: None,
                auto_approve: false,
                worktree: worktree.clone(),
                branch: branch.clone(),
                session_worktree: worktree,
                session_branch: branch,
                write_scope: Vec::new(),
                base_commit: session.base_commit.clone().unwrap_or_default(),

                workspace_id: None,
                session_workspace_id: None,
                parent_workspace_id: None,
            };
            let system =
                role_system_prompt(spec.role, &project, &session, &spec, &session.goal).render();
            let dynamic_mcp =
                collect_dynamic_mcp_tools(&self.engine, spec.role, &session.project_id, None).await;
            let frozen_bundle = frozen_tool_bundle(&config, &spec, &dynamic_mcp);
            let tools = frozen_bundle.tools.clone();
            let model_name = model_for(&config, spec.role);
            let calibration = load_context_calibration(&self.engine, session_id, &model_name).await;
            let system_tokens = estimate_context_tokens(&system);
            let tool_tokens = serde_json::to_string(&tools)
                .map(|schema| estimate_context_tokens(&schema))
                .unwrap_or(0);
            let compaction_policy = context_compaction_policy(&config, spec.role);
            let (threshold, hard_limit) =
                (compaction_policy.threshold, compaction_policy.hard_limit);
            let loaded = load_conversation(&self.engine, session_id).await?;
            let mut conversation = loaded.messages;
            let mut conversation_ids = loaded.ids;
            let before = self
                .engine
                .latest_event_of_kind(session_id, "context.compacted")
                .await?;
            let failed_before = self
                .engine
                .latest_event_of_kind(session_id, "context.compact_failed")
                .await?;
            let budget = SessionBudget::new(
                session.token_budget,
                session.tokens_in + session.tokens_out,
                AgentCaps {
                    managers: config.max_managers,
                    workers: config.max_workers,
                    labour: config.max_labour,
                },
            )
            .with_cost(session.cost_budget_usd, session.cost_spent_usd)
            .with_time(session.time_budget_secs)
            .with_unlimited_flag(self.engine.unlimited_flag(&session.id, session.unlimited))
            .with_scaling_slots(self.engine.scaling_slots(
                &session.id,
                &session.project_id,
                config.max_workers,
                config.max_labour,
            ));
            if budget.exhausted() {
                anyhow::bail!("session budget is exhausted; compaction needs provider tokens");
            }
            let prefix_hash = crate::sha256_hex(
                format!(
                    "{system}\n{}",
                    serde_json::to_string(&tools).unwrap_or_default()
                )
                .as_bytes(),
            );
            let mut compaction_runtime = CompactionRuntime::default();
            let changed = compact_context_if_needed(
                &self.engine,
                &config,
                &session,
                &spec,
                &budget,
                &control,
                &mut conversation,
                &mut conversation_ids,
                threshold,
                hard_limit,
                system_tokens.saturating_add(tool_tokens),
                calibration.factor,
                true,
                &prefix_hash,
                config.compact_prepare_ratio,
                &mut compaction_runtime,
                context_strategy(spec.role),
            )
            .await?;
            if changed {
                let context_window = context_window_for(&config, spec.role);
                if context_window > 0 {
                    emit(
                        &self.engine,
                        session_id,
                        &spec,
                        "context.usage",
                        serde_json::json!({
                            "estimated_tokens": calibrate_context_tokens(
                                estimate_messages_tokens(&conversation)
                                    .saturating_add(system_tokens)
                                    .saturating_add(tool_tokens),
                                calibration.factor,
                            ),
                            "context_window": context_window,
                            "threshold_tokens": threshold,
                            "hard_limit_tokens": hard_limit,
                            "threshold_source": compaction_policy.source(),
                            "threshold_ratio_basis_points": compaction_policy.ratio_basis_points,
                            "output_reserve_tokens": compaction_policy.output_reserve,
                            "messages": conversation.len(),
                            "calibration": calibration.factor,
                        }),
                    )
                    .await?;
                }
            }
            if !changed
                && let Some(event) = self
                    .engine
                    .latest_event_of_kind(session_id, "context.compact_failed")
                    .await?
                && failed_before
                    .as_ref()
                    .is_none_or(|before| event.seq > before.seq)
            {
                let message = event
                    .payload
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("context compaction failed");
                anyhow::bail!("context compaction failed: {message}");
            }
            let compaction = if changed {
                self.engine
                    .latest_event_of_kind(session_id, "context.compacted")
                    .await?
                    .filter(|event| before.as_ref().is_none_or(|before| event.seq > before.seq))
                    .and_then(|event| {
                        serde_json::from_value::<ContextCompactionSummary>(event.payload.clone())
                            .ok()
                            .map(|mut summary| {
                                summary.created_at = event.created_at;
                                summary
                            })
                    })
            } else {
                None
            };
            Ok(SessionCompactResult {
                session_id: session.id.clone(),
                compacted: changed,
                compaction,
            })
        }
        .await;
        self.end_operation(session_id);
        result
    }

    pub(super) fn configured_agent(&self) -> Result<AgentConfig> {
        self.config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned())
            .ok_or_else(|| anyhow!("agent provider is not configured"))
    }

    pub(super) async fn context_preamble(
        &self,
        config: &AgentConfig,
        session: &Session,
        project: &Project,
    ) -> Result<(AgentSpec, u64, String)> {
        let agents = self.engine.list_agents(&session.id).await?;
        let leader = agents.into_iter().find(|agent| agent.role == "leader");
        let profile = leader
            .as_ref()
            .and_then(|agent| agent.profile.as_deref())
            .and_then(AgentProfile::parse)
            .unwrap_or(AgentProfile::Generic);
        let worktree = PathBuf::from(session.worktree.clone().unwrap_or_default());
        let branch = session.branch.clone().unwrap_or_default();
        let spec = AgentSpec {
            id: leader
                .as_ref()
                .map(|agent| agent.id.clone())
                .unwrap_or_else(|| "leader".to_string()),
            role: AgentRole::Leader,
            name: leader
                .as_ref()
                .map(|agent| agent.name.clone())
                .unwrap_or_else(|| "Leader".to_string()),
            depth: 0,
            parent_name: None,
            title: leader.as_ref().and_then(|agent| agent.title.clone()),
            task_id: None,
            department: None,
            identity_id: None,
            identity_generation: None,
            profile,
            allow_delegation: true,
            task_kind: None,
            auto_approve: false,
            worktree: worktree.clone(),
            branch: branch.clone(),
            session_worktree: worktree,
            session_branch: branch,
            write_scope: Vec::new(),
            base_commit: session.base_commit.clone().unwrap_or_default(),

            workspace_id: None,
            session_workspace_id: None,
            parent_workspace_id: None,
        };
        let system = role_system_prompt(spec.role, project, session, &spec, &session.goal).render();
        let dynamic_mcp =
            collect_dynamic_mcp_tools(&self.engine, spec.role, &session.project_id, None).await;
        let frozen_bundle = frozen_tool_bundle(config, &spec, &dynamic_mcp);
        let tools = frozen_bundle.tools.clone();
        let prefix_hash = crate::sha256_hex(
            format!(
                "{system}\n{}",
                crate::tool_bundle::canonical_tools_json(&tools)
            )
            .as_bytes(),
        );
        let tokens = estimate_context_tokens(&system).saturating_add(
            serde_json::to_string(&tools)
                .map(|schema| estimate_context_tokens(&schema))
                .unwrap_or(0),
        );
        Ok((spec, tokens, prefix_hash))
    }

    /// Capture the current active-context boundary without running a prompt or
    /// creating a new compaction. When the session already has a compaction
    /// boundary, the manual checkpoint records exactly that boundary's summary,
    /// cursor, structured state, and token estimates.
    pub async fn context_checkpoint_now(
        &self,
        session_id: &str,
        label: Option<String>,
    ) -> Result<ContextCheckpoint> {
        let config = self.configured_agent()?;
        let (session, project) =
            load_session_project(self.engine.store().clone(), session_id).await?;
        if session.status == "imported" {
            anyhow::bail!("imported sessions are read-only archives");
        }
        if crate::is_active_session_status(&session.status) {
            anyhow::bail!(
                "session must be idle, failed, or cancelled before creating a context checkpoint"
            );
        }
        self.begin_operation(session_id)?;
        let result = async {
            let (spec, base_tokens, prefix_hash) =
                self.context_preamble(&config, &session, &project).await?;
            let store = self.engine.store().clone();
            let revision_session = session.id.clone();
            let current_revision =
                tokio::task::spawn_blocking(move || store.plan_revision_count(&revision_session))
                    .await??;
            let latest = self
                .engine
                .latest_event_of_kind(session_id, "context.compacted")
                .await?;
            let (
                mode,
                summary,
                through_message_id,
                structured_state,
                plan_revision,
                tokens_before,
                tokens_after,
            ) = if let Some(event) = &latest {
                let mode = event
                    .payload
                    .get("mode")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("summary")
                    .to_string();
                let summary = event
                    .payload
                    .get("summary")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                let through = event
                    .payload
                    .get("through_message_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                let structured_state = event
                    .payload
                    .get("structured_state")
                    .cloned()
                    .and_then(|value| serde_json::from_value::<ContextStructuredState>(value).ok());
                let plan_revision = structured_state
                    .as_ref()
                    .map(|state| state.plan_revision)
                    .unwrap_or(current_revision);
                let tokens_before = event
                    .payload
                    .get("estimated_tokens_before")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                let tokens_after = event
                    .payload
                    .get("estimated_tokens_after")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                (
                    mode,
                    summary,
                    through,
                    structured_state,
                    plan_revision,
                    tokens_before,
                    tokens_after,
                )
            } else {
                let state = load_compaction_structured_state(&self.engine, &session, &spec).await;
                let loaded = load_conversation(&self.engine, session_id).await?;
                let model_name = model_for(&config, spec.role);
                let calibration =
                    load_context_calibration(&self.engine, session_id, &model_name).await;
                let estimated = calibrate_context_tokens(
                    estimate_messages_tokens(&loaded.messages).saturating_add(base_tokens),
                    calibration.factor,
                );
                (
                    "manual".to_string(),
                    None,
                    None,
                    Some(state),
                    current_revision,
                    estimated,
                    estimated,
                )
            };
            let checkpoint = ContextCheckpoint {
                id: uuid::Uuid::now_v7().to_string(),
                session_id: session.id.clone(),
                project_id: project.id.clone(),
                label: label.unwrap_or_else(|| "manual context checkpoint".to_string()),
                mode,
                summary,
                through_message_id,
                message_cursor: 0,
                plan_revision,
                provider: config.provider_name.clone(),
                model: model_for(&config, spec.role),
                prompt_prefix_hash: prefix_hash,
                worktree_head: context_worktree_head(session.worktree.as_deref()),
                tokens_before,
                tokens_after,
                structured_state,
                source_compaction_seq: latest.as_ref().map(|event| event.seq),
                automatic: false,
                created_at: vp_protocol::now_rfc3339(),
            };
            let event = self
                .engine
                .record_context_checkpoint(checkpoint.clone())
                .await?;
            Ok::<_, anyhow::Error>(
                crate::context::context_checkpoint_from_event(&event).unwrap_or(checkpoint),
            )
        }
        .await;
        self.end_operation(session_id);
        result
    }

    /// Deterministic restore preflight shared by the detail RPC and the
    /// restore command. Errors are hard safety refusals; warnings are
    /// token-threshold advisories that `force` may bypass.
    pub(super) async fn context_restore_preflight(
        &self,
        config: Option<&AgentConfig>,
        session: &Session,
        project: &Project,
        checkpoint: &ContextCheckpoint,
    ) -> Result<ContextRestorePreflight> {
        let mut preflight = ContextRestorePreflight {
            checkpoint_plan_revision: checkpoint.plan_revision,
            worktree_head: checkpoint.worktree_head.clone(),
            ..ContextRestorePreflight::default()
        };
        if session.status == "imported" {
            preflight
                .errors
                .push("imported sessions are read-only archives".to_string());
        } else if !matches!(session.status.as_str(), "idle" | "failed" | "cancelled") {
            preflight.errors.push(format!(
                "session must be idle, failed, or cancelled before restoring context; current status is {}",
                session.status
            ));
        }
        let store = self.engine.store().clone();
        let imported_session = session.id.clone();
        let imported = tokio::task::spawn_blocking(move || {
            store.session_has_imported_event(&imported_session)
        })
        .await??;
        if imported {
            preflight.errors.push(
                "imported or rehydrated sessions cannot restore a context checkpoint".to_string(),
            );
        }
        let store = self.engine.store().clone();
        let revision_session = session.id.clone();
        let current_revision =
            tokio::task::spawn_blocking(move || store.plan_revision_count(&revision_session))
                .await??;
        preflight.plan_revision = current_revision;
        if checkpoint.plan_revision != current_revision {
            preflight.errors.push(format!(
                "plan revision changed: checkpoint={}, current={current_revision}",
                checkpoint.plan_revision
            ));
        }
        if let Some(config) = config {
            preflight.provider = config.provider_name.clone();
            preflight.model = model_for(config, AgentRole::Leader);
            if checkpoint.provider != config.provider_name {
                preflight.errors.push(format!(
                    "provider changed: checkpoint={}, current={}",
                    checkpoint.provider, config.provider_name
                ));
            }
            if checkpoint.model != preflight.model {
                preflight.errors.push(format!(
                    "model changed: checkpoint={}, current={}",
                    checkpoint.model, preflight.model
                ));
            }
        } else {
            preflight
                .errors
                .push("agent provider is not configured".to_string());
        }

        let worktree_head = checkpoint.worktree_head.clone();
        if worktree_head.is_empty() {
            preflight
                .errors
                .push("checkpoint does not record a worktree head".to_string());
        }
        let worktree = session.worktree.clone().map(PathBuf::from);
        let project_root = PathBuf::from(&project.root);
        match worktree {
            Some(worktree) if worktree.is_dir() => {
                let status = worktree::status_porcelain(&worktree);
                let current = worktree::head_commit(&worktree);
                match (status, current) {
                    (Ok(status), Ok(current)) => {
                        preflight.current_worktree_head = Some(current.clone());
                        if !status.is_empty() {
                            preflight.errors.push(
                                "session worktree has uncommitted changes; commit or discard them before restoring context"
                                    .to_string(),
                            );
                        }
                        if !worktree_head.is_empty() {
                            if !worktree::commit_exists(&project_root, &worktree_head) {
                                preflight.errors.push(format!(
                                    "checkpoint worktree head is missing: {worktree_head}"
                                ));
                            } else if worktree_head == current {
                                preflight.worktree_descendant = true;
                            } else {
                                let ancestor = worktree::git_output(
                                    &project_root,
                                    &["merge-base", "--is-ancestor", &worktree_head, &current],
                                )
                                .is_ok();
                                preflight.worktree_descendant = ancestor;
                                if !ancestor {
                                    preflight.errors.push(format!(
                                        "current worktree head {current} is not a descendant of checkpoint head {worktree_head}"
                                    ));
                                }
                            }
                        }
                    }
                    (Err(error), _) => preflight
                        .errors
                        .push(format!("could not inspect session worktree: {error:#}")),
                    (_, Err(error)) => preflight
                        .errors
                        .push(format!("could not read session worktree head: {error:#}")),
                }
            }
            Some(worktree) => preflight.errors.push(format!(
                "session worktree is missing: {}",
                worktree.display()
            )),
            None => preflight.errors.push("session has no worktree".to_string()),
        }

        let budget = SessionBudget::new(
            session.token_budget,
            session.tokens_in + session.tokens_out,
            AgentCaps {
                managers: 1,
                workers: 1,
                labour: 1,
            },
        )
        .with_cost(session.cost_budget_usd, session.cost_spent_usd)
        .with_time(session.time_budget_secs)
        .with_unlimited_flag(self.engine.unlimited_flag(&session.id, session.unlimited));
        if budget.exhausted() {
            preflight.budget_exhausted = true;
            preflight
                .errors
                .push("session budget is exhausted".to_string());
        }

        let messages = self.engine.list_messages(&session.id).await?;
        let (restored, through_found) = restored_active_messages(&messages, checkpoint);
        if checkpoint.through_message_id.is_some() && !through_found {
            preflight.errors.push(format!(
                "checkpoint through message is missing: {}",
                checkpoint.through_message_id.as_deref().unwrap_or_default()
            ));
        }
        preflight.active_messages = restored.len();
        if let Some(config) = config {
            match self.context_preamble(config, session, project).await {
                Ok((spec, base_tokens, _)) => {
                    let model_name = model_for(config, spec.role);
                    let calibration =
                        load_context_calibration(&self.engine, &session.id, &model_name).await;
                    let (threshold, hard_limit) = context_limits(config, spec.role);
                    let estimated = calibrate_context_tokens(
                        estimate_messages_tokens(&restored).saturating_add(base_tokens),
                        calibration.factor,
                    );
                    preflight.estimated_tokens = estimated;
                    preflight.threshold_tokens = threshold;
                    preflight.hard_limit_tokens = hard_limit;
                    if estimated > hard_limit {
                        preflight.errors.push(format!(
                            "restored context estimate {estimated} exceeds the hard limit {hard_limit}"
                        ));
                    } else if threshold > 0 && estimated >= threshold {
                        preflight.warnings.push(format!(
                            "restored context estimate {estimated} is at or above the compaction threshold {threshold}; pass force=true to accept it"
                        ));
                    }
                }
                Err(error) => preflight
                    .errors
                    .push(format!("could not estimate restored context: {error:#}")),
            }
        }
        preflight.restorable = preflight.errors.is_empty();
        Ok(preflight)
    }

    pub async fn context_checkpoint_detail(
        &self,
        session_id: &str,
        checkpoint_id: &str,
    ) -> Result<ContextCheckpointDetailResult> {
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned());
        let (session, project) =
            load_session_project(self.engine.store().clone(), session_id).await?;
        let checkpoint = self
            .engine
            .list_context_checkpoints(session_id, Some(200))
            .await?
            .checkpoints
            .into_iter()
            .find(|checkpoint| checkpoint.id == checkpoint_id)
            .ok_or_else(|| crate::context::empty_context_checkpoint_error(checkpoint_id))?;
        let preflight = self
            .context_restore_preflight(config.as_ref(), &session, &project, &checkpoint)
            .await?;
        let events = self.engine.all_events(session_id).await?;
        let is_active = crate::context::active_context_checkpoint_id(&events)
            .is_some_and(|id| id == checkpoint.id);
        let summary_preview =
            crate::context::context_checkpoint_preview(checkpoint.summary.as_deref(), 160);
        Ok(ContextCheckpointDetailResult {
            checkpoint,
            preflight,
            is_active,
            summary_preview,
        })
    }

    /// Context-only restore. It appends a reconstructed compaction boundary
    /// plus a restore audit event; it never resets the worktree, creates a
    /// task/run/merge, or rewrites stored messages.
    pub async fn restore_context_checkpoint_now(
        &self,
        session_id: &str,
        checkpoint_id: &str,
        force: bool,
    ) -> Result<ContextRestoreResult> {
        let config = self.configured_agent()?;
        let (session, project) =
            load_session_project(self.engine.store().clone(), session_id).await?;
        self.begin_operation(session_id)?;
        let result = async {
            let checkpoint = self
                .engine
                .list_context_checkpoints(session_id, Some(200))
                .await?
                .checkpoints
                .into_iter()
                .find(|checkpoint| checkpoint.id == checkpoint_id)
                .ok_or_else(|| crate::context::empty_context_checkpoint_error(checkpoint_id))?;
            let preflight = self
                .context_restore_preflight(Some(&config), &session, &project, &checkpoint)
                .await?;
            if !preflight.errors.is_empty() {
                anyhow::bail!("context restore refused: {}", preflight.errors.join("; "));
            }
            let warning = if preflight.warnings.is_empty() {
                None
            } else {
                let warning = preflight.warnings.join("; ");
                if !force {
                    anyhow::bail!("context restore warning: {warning}");
                }
                Some(warning)
            };
            let previous = self
                .engine
                .latest_event_of_kind(session_id, "context.compacted")
                .await?;
            let previous_seq = previous.as_ref().map(|event| event.seq);
            let compaction_count = self
                .engine
                .count_events_of_kind(session_id, "context.compacted")
                .await?
                .saturating_add(1);
            let model_name = model_for(&config, AgentRole::Leader);
            let calibration = load_context_calibration(&self.engine, session_id, &model_name).await;
            let restore_mode = crate::context::context_checkpoint_restore_mode(&checkpoint);
            let checkpoint_summary = checkpoint.summary.clone();
            let checkpoint_through = checkpoint.through_message_id.clone();
            let checkpoint_structured_state = checkpoint.structured_state.clone();
            let checkpoint_id_value = checkpoint.id.clone();
            let (usage_spec, _, _) = self.context_preamble(&config, &session, &project).await?;
            let compaction_payload = serde_json::json!({
                "mode": restore_mode,
                "through_message_id": checkpoint_through.clone(),
                "summary": checkpoint_summary.clone(),
                "summarized_messages": 0,
                "dropped_messages": 0,
                "kept_messages": preflight.active_messages,
                "estimated_tokens_before": checkpoint.tokens_before,
                "estimated_tokens_after": preflight.estimated_tokens,
                "threshold_tokens": preflight.threshold_tokens,
                "hard_limit_tokens": preflight.hard_limit_tokens,
                "calibration": calibration.factor,
                "input_tokens": 0,
                "output_tokens": 0,
                "estimated": true,
                "latency_ms": 0,
                "background_reuse": false,
                "fallback_to_sync": false,
                "compaction_count": compaction_count,
                "structured_state": checkpoint_structured_state,
                "background_prepared": false,
                "restored": true,
                "restored_from_checkpoint": checkpoint_id_value.clone(),
                "context_checkpoint_id": checkpoint_id_value.clone(),
            });
            let restored_payload = serde_json::json!({
                "checkpoint_id": checkpoint_id_value,
                "mode": restore_mode,
                "through_message_id": checkpoint_through,
                "checkpoint": checkpoint.clone(),
                "estimated_tokens": preflight.estimated_tokens,
                "hard_limit_tokens": preflight.hard_limit_tokens,
                "active_messages": preflight.active_messages,
                "previous_compaction_seq": previous_seq,
                "forced": force,
                "warning": warning,
            });
            let (compaction_event, restored_event) = self
                .engine
                .append_context_restore_events(session_id, compaction_payload, restored_payload)
                .await?;
            let context_window = context_window_for(&config, AgentRole::Leader);
            let threshold = preflight.threshold_tokens;
            let hard_limit = preflight.hard_limit_tokens;
            if context_window > 0 {
                emit(
                    &self.engine,
                    session_id,
                    &usage_spec,
                    "context.usage",
                    serde_json::json!({
                        "estimated_tokens": preflight.estimated_tokens,
                        "context_window": context_window,
                        "threshold_tokens": threshold,
                        "hard_limit_tokens": hard_limit,
                        "messages": preflight.active_messages,
                        "calibration": calibration.factor,
                    }),
                )
                .await?;
            }
            Ok(ContextRestoreResult {
                session_id: session.id.clone(),
                checkpoint_id: checkpoint.id,
                mode: restore_mode.to_string(),
                through_message_id: checkpoint.through_message_id,
                active_messages: preflight.active_messages,
                estimated_tokens: preflight.estimated_tokens,
                hard_limit_tokens: preflight.hard_limit_tokens,
                warning,
                compaction_event_seq: compaction_event.seq,
                restored_event_seq: restored_event.seq,
            })
        }
        .await;
        self.end_operation(session_id);
        result
    }

    /// Dispatch exactly one ready task from a stored TeamPlan. The session is
    /// released after the task settles so the global scheduler can make fair
    /// progress across sessions instead of running an entire plan at once.
    pub async fn dispatch_next_task(&self, session_id: &str) -> Result<bool> {
        self.start_plan_run(session_id, Some(1)).await
    }

    pub(super) async fn start_plan_run(
        &self,
        session_id: &str,
        max_tasks: Option<usize>,
    ) -> Result<bool> {
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned());
        let Some(config) = config else {
            anyhow::bail!("agent provider is not configured");
        };
        let (session, _) = load_session_project(self.engine.store().clone(), session_id).await?;
        if session.status == "imported" {
            anyhow::bail!(
                "imported sessions are read-only archives; rehydrate before resuming a plan"
            );
        }
        if matches!(
            session.status.as_str(),
            "queued" | "running" | "paused" | "landed"
        ) {
            anyhow::bail!("session must be idle, failed, cancelled, or draft to resume a plan");
        }
        let Some(plan) = self.engine.get_team_plan(session_id).await? else {
            anyhow::bail!("session has no TeamPlan");
        };
        let compatibility = self.engine.plan_compatibility(session_id).await?;
        if compatibility.status == vp_protocol::runtime::CompatibilityStatus::Incompatible {
            return Err(crate::runtime_pin::RuntimePinError::new(
                crate::runtime_pin::codes::RUNTIME_INCOMPATIBLE,
                format!(
                    "plan runtime identity is incompatible with the stored snapshot: {}",
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
        let (_, scheduled) = self.engine.scheduler().sync_plan(&session, &plan).await?;
        let mut terminal = scheduled
            .iter()
            .filter(|task| matches!(task.status.as_str(), "completed" | "skipped"))
            .map(|task| task.task_id.clone())
            .collect::<std::collections::HashSet<_>>();
        // Tasks absent from a just-synced queue are treated as pending.
        for task in &plan.tasks {
            if !scheduled.iter().any(|row| row.task_id == task.id) {
                terminal.remove(&task.id);
            }
        }
        if plan.tasks.iter().all(|task| terminal.contains(&task.id)) {
            anyhow::bail!("plan is already complete or skipped");
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
            let (event_kind, mut payload) = if max_tasks.is_some() {
                (
                    "agent.task_dispatch_queued",
                    serde_json::json!({
                        "goal": plan.goal,
                        "remaining_tasks": plan.tasks.len().saturating_sub(terminal.len()),
                        "task_limit": 1,
                    }),
                )
            } else {
                (
                    "agent.plan_resume_queued",
                    serde_json::json!({
                        "goal": plan.goal,
                        "remaining_tasks": plan.tasks.len().saturating_sub(terminal.len()),
                    }),
                )
            };
            if let Some(object) = payload.as_object_mut() {
                object.insert("task_limit".into(), serde_json::json!(max_tasks));
            }
            self.engine
                .append_event(session_id, event_kind, payload)
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

        let engine = self.engine.clone();
        let session_id = session_id.to_string();
        let project_id = session.project_id.clone();
        let priority = session.priority;
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
            let result = run_plan_resume(
                engine.clone(),
                config,
                &session_id,
                control.clone(),
                max_tasks,
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
        Ok(true)
    }

    pub fn cancel(&self, session_id: &str) -> bool {
        if let Ok(runs) = self.runs.lock()
            && let Some(control) = runs.get(session_id)
        {
            if !control.cancellable {
                return false;
            }
            control.cancel();
            return true;
        }
        false
    }

    /// Hard-preempt one active run. This aborts in-flight provider and tool
    /// work instead of waiting for the next cooperative boundary. It is
    /// operator-only: no scheduler or agent can call it, and unknown runs fail
    /// closed without creating a preemption record.
    pub async fn preempt(
        &self,
        session_id: &str,
        reason: Option<&str>,
    ) -> Result<SessionPreemptResult> {
        let reason = reason
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
            .map(|reason| reason.chars().take(500).collect::<String>())
            .unwrap_or_else(|| "operator requested preemption".to_string());
        let control = self
            .runs
            .lock()
            .map_err(|_| anyhow!("agent run mutex poisoned"))?
            .get(session_id)
            .cloned();
        let Some(control) = control else {
            return Ok(SessionPreemptResult {
                session_id: session_id.to_string(),
                preempted: false,
                reason: String::new(),
                detail: "no active run holds a preempt token".to_string(),
            });
        };
        if !control.cancellable {
            return Ok(SessionPreemptResult {
                session_id: session_id.to_string(),
                preempted: false,
                reason: String::new(),
                detail: "this operation is not preemptible".to_string(),
            });
        }
        if control.is_cancelled() {
            return Ok(SessionPreemptResult {
                session_id: session_id.to_string(),
                preempted: false,
                reason: String::new(),
                detail: "the run is already cancelling".to_string(),
            });
        }
        if control.is_preempted() {
            return Ok(SessionPreemptResult {
                session_id: session_id.to_string(),
                preempted: false,
                reason: control
                    .preempt_report()
                    .map(|snapshot| snapshot.reason.as_str().to_string())
                    .unwrap_or_default(),
                detail: "the run is already preempted".to_string(),
            });
        }
        self.engine
            .append_event(
                session_id,
                "session.preempt_requested",
                serde_json::json!({
                    "reason": reason,
                    "phase": control.preempt.phase().as_str(),
                }),
            )
            .await?;
        let snapshot = control.request_preempt();
        Ok(SessionPreemptResult {
            session_id: session_id.to_string(),
            preempted: true,
            reason: snapshot.reason.as_str().to_string(),
            detail: format!(
                "preemption signalled during {}; the run will stop after bounded cleanup",
                snapshot.phase.as_str()
            ),
        })
    }

    /// Session-scoped preemption history plus the live in-memory state. The
    /// history is derived from events; `active` is the only volatile part.
    pub async fn preemption(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<SessionPreemptionResult> {
        let mut result = self.engine.session_preemption(session_id, limit).await?;
        result.active = self
            .runs
            .lock()
            .ok()
            .and_then(|runs| runs.get(session_id).cloned())
            .is_some_and(|control| control.is_preempted());
        Ok(result)
    }

    pub async fn pause(&self, session_id: &str) -> Result<bool> {
        let control = self
            .runs
            .lock()
            .map_err(|_| anyhow!("agent run mutex poisoned"))?
            .get(session_id)
            .cloned();
        let Some(control) = control else {
            return Ok(false);
        };
        if control.manual {
            // Manual context compaction is a short foreground operation and
            // has no resumable run loop; pausing it would leave the session
            // stuck in `paused` after the compaction finishes.
            return Ok(false);
        }
        if control.is_paused() {
            return Ok(true);
        }
        let finalized = control.finalized.lock().await;
        if finalized.is_some() {
            return Ok(false);
        }
        control.pause();
        self.engine
            .update_session_status(session_id, "paused")
            .await?;
        self.engine
            .append_event(session_id, "agent.paused", serde_json::json!({}))
            .await?;
        drop(finalized);
        Ok(true)
    }

    pub async fn resume(&self, session_id: &str) -> Result<bool> {
        let control = self
            .runs
            .lock()
            .map_err(|_| anyhow!("agent run mutex poisoned"))?
            .get(session_id)
            .cloned();
        let Some(control) = control else {
            return Ok(false);
        };
        if !control.is_paused() {
            return Ok(true);
        }
        let finalized = control.finalized.lock().await;
        if finalized.is_some() {
            return Ok(false);
        }
        control.resume();
        self.engine
            .update_session_status(session_id, "running")
            .await?;
        self.engine
            .append_event(session_id, "agent.resumed", serde_json::json!({}))
            .await?;
        drop(finalized);
        Ok(true)
    }
}

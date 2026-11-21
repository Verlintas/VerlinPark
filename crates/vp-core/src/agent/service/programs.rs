use super::*;

impl AgentService {
    /// Deterministic project-program controller. This reconciles completed,
    /// failed, retrying, and dependency-blocked milestones even when no
    /// provider is configured, then launches at most one ready milestone per
    /// program slot through the ordinary session/plan path.
    pub async fn reconcile_programs(&self, project_id: Option<&str>) -> Result<ProgramTickResult> {
        let controller = self.engine.programs();
        let result = controller.reconcile(project_id).await?;
        let mut started = 0usize;
        if self.info().configured {
            let candidates = controller.ready_candidates_in_project(project_id, None, 10)?;
            for candidate in candidates {
                if !candidate.program.policy.auto_dispatch {
                    continue;
                }
                match self.start_program_milestone(&candidate.milestone.id).await {
                    Ok(_) => started += 1,
                    Err(error) => {
                        tracing::warn!(
                            %error,
                            program_id = %candidate.program.id,
                            milestone_id = %candidate.milestone.id,
                            "program controller could not start a ready milestone"
                        );
                    }
                }
            }
        }
        Ok(ProgramTickResult {
            programs: result.programs,
            milestones_advanced: result.milestones_advanced,
            sessions_started: started,
            completed: result.completed,
            failed: result.failed,
            ready: Vec::new(),
        })
    }

    /// Cooperatively cancel a whole program. Running sessions receive the
    /// normal cancellation signal; the ledger rows become cancelled and the
    /// program becomes failed with an explicit operator reason. This never
    /// marks partial work as completed.
    pub async fn cancel_program(&self, program_id: &str, reason: &str) -> Result<Program> {
        let reason = reason.trim();
        if reason.is_empty() || reason.chars().count() > 1_000 {
            anyhow::bail!("program cancellation reason must be 1-1000 characters");
        }
        let store = self.engine.store().clone();
        let program_id_owned = program_id.to_string();
        let Some(program) = tokio::task::spawn_blocking({
            let store = store.clone();
            let program_id = program_id_owned.clone();
            move || store.get_program(&program_id)
        })
        .await??
        else {
            anyhow::bail!("program {program_id} was not found");
        };
        if program.status.is_terminal() {
            anyhow::bail!("program {program_id} is already terminal");
        }
        if program.archived {
            anyhow::bail!("program {program_id} is an imported archive");
        }
        let milestones = tokio::task::spawn_blocking({
            let store = store.clone();
            let program_id = program_id_owned.clone();
            move || store.list_program_milestones(&program_id)
        })
        .await??;
        let ledger_reason = format!("program cancelled: {reason}");
        for milestone in milestones {
            if milestone.status.is_terminal() {
                continue;
            }
            if let Some(session_id) = milestone.session_id.as_deref() {
                let _ = self.cancel(session_id);
            }
            let store = store.clone();
            let milestone_id = milestone.id.clone();
            let ledger_reason = ledger_reason.clone();
            let _ = tokio::task::spawn_blocking(move || {
                store.cancel_program_milestone(&milestone_id, &ledger_reason, false)
            })
            .await?;
        }
        let store = self.engine.store().clone();
        let program_id_owned = program_id.to_string();
        let program_reason = format!("cancelled by operator: {reason}");
        let program = tokio::task::spawn_blocking(move || {
            store.fail_program(&program_id_owned, &program_reason)
        })
        .await??
        .map(|(program, _event)| program)
        .ok_or_else(|| anyhow!("program {program_id} could not be cancelled"))?;
        // Every non-terminal milestone was cancelled above. Release any
        // program allowance still held by a session/lifecycle race.
        let controller = self.engine.programs();
        controller.release_program_reservations(program_id);
        let _ = controller.release_stale_reservations();
        self.engine.record_project_manifest().await?;
        Ok(program)
    }

    /// Start one milestone through the normal session path. The milestone is
    /// claimed before the session is created, so a concurrent tick cannot
    /// create a duplicate worktree or plan. All gates remain in force.
    pub async fn start_program_milestone(&self, milestone_id: &str) -> Result<ProgramMilestone> {
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned())
            .ok_or_else(|| anyhow!("agent provider is not configured"))?;
        let controller = self.engine.programs();
        let Some(milestone) = controller.claim_start(milestone_id)? else {
            anyhow::bail!("milestone {milestone_id} is not runnable");
        };
        let store = self.engine.store().clone();
        let program_id = milestone.program_id.clone();
        let Some(program) = tokio::task::spawn_blocking({
            let store = store.clone();
            let program_id = program_id.clone();
            move || store.get_program(&program_id)
        })
        .await??
        else {
            anyhow::bail!("program {program_id} disappeared after the milestone was claimed");
        };
        let (token_budget, cost_budget_usd) =
            match controller.budget_for_start(&program, &milestone) {
                Ok(budget) => (budget.token_budget, budget.cost_budget_usd),
                Err(error) => {
                    controller.fail_start(&program, &milestone, &format!("{error:#}"))?;
                    return Err(error);
                }
            };
        let plan = crate::program::milestone_plan_with_budget(
            &program,
            &milestone,
            token_budget,
            cost_budget_usd,
        );
        if let Err(error) = validate_team_plan(&plan, config.max_managers) {
            controller.fail_start(
                &program,
                &milestone,
                &format!("program milestone plan validation failed: {error:#}"),
            )?;
            return Err(error);
        }
        let goal = crate::program::milestone_session_goal(&program, &milestone);
        let priority = milestone.priority;
        let session = match self
            .engine
            .create_session_with_options(
                program.project_id.clone(),
                goal,
                token_budget,
                cost_budget_usd,
                None,
                priority,
            )
            .await
        {
            Ok(session) => session,
            Err(error) => {
                controller.fail_start(
                    &program,
                    &milestone,
                    &format!("could not create a program session: {error:#}"),
                )?;
                return Err(error);
            }
        };
        let setup = async {
            validate_team_plan(&plan, config.max_managers)?;
            self.engine.set_team_plan(&session.id, &plan).await?;
            controller.attach_session(&milestone.id, &session.id, 1)?;
            self.engine
                .append_event(
                    &session.id,
                    "program.milestone.dispatched",
                    serde_json::json!({
                        "program_id": program.id,
                        "milestone_id": milestone.id,
                        "milestone_key": milestone.key,
                        "attempt": milestone.attempts,
                    }),
                )
                .await?;
            Ok::<(), anyhow::Error>(())
        }
        .await;
        if let Err(error) = setup {
            let _ = self
                .engine
                .update_session_status(&session.id, "failed")
                .await;
            let _ = self
                .engine
                .append_event(
                    &session.id,
                    "program.milestone.dispatch_failed",
                    serde_json::json!({
                        "program_id": program.id,
                        "milestone_id": milestone.id,
                        "error": format!("{error:#}"),
                    }),
                )
                .await;
            controller.fail_start(
                &program,
                &milestone,
                &format!("could not prepare the milestone plan: {error:#}"),
            )?;
            return Err(error);
        }
        if let Err(error) = self.resume_plan(&session.id).await {
            let _ = self
                .engine
                .update_session_status(&session.id, "failed")
                .await;
            let _ = self
                .engine
                .append_event(
                    &session.id,
                    "program.milestone.dispatch_failed",
                    serde_json::json!({
                        "program_id": program.id,
                        "milestone_id": milestone.id,
                        "error": format!("{error:#}"),
                    }),
                )
                .await;
            controller.fail_start(
                &program,
                &milestone,
                &format!("could not start the milestone run: {error:#}"),
            )?;
            return Err(error);
        }
        let store = self.engine.store().clone();
        let milestone_id = milestone.id.clone();
        Ok(
            tokio::task::spawn_blocking(move || store.get_program_milestone(&milestone_id))
                .await??
                .unwrap_or(milestone),
        )
    }
}

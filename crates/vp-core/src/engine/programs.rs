use super::*;

impl Engine {
    pub fn programs(&self) -> crate::program::ProgramController {
        crate::program::ProgramController::new(self.store.clone())
    }

    /// Validate and create a project program. A draft program does not
    /// dispatch; explicit activation is the operator's opt-in to autonomous
    /// milestone scheduling. When activation omits a policy, auto-dispatch is
    /// enabled explicitly rather than by an invisible default.
    pub async fn create_program(&self, params: ProgramCreateParams) -> Result<Program> {
        crate::program::validate_program_goal(&params.goal)?;
        crate::program::validate_program_budgets(params.token_budget, params.cost_budget_usd)?;
        let policy = match params.policy {
            Some(policy) => policy,
            None => ProgramPolicy {
                auto_dispatch: params.activate,
                ..Default::default()
            },
        };
        crate::program::validate_program_policy(&policy)?;
        let store = self.store.clone();
        let project_id = params.project_id.clone();
        tokio::task::spawn_blocking({
            let store = store.clone();
            let project_id = project_id.clone();
            move || store.get_project(&project_id)
        })
        .await??
        .ok_or_else(|| anyhow!("project not found: {project_id}"))?;
        let input = crate::program::new_program_input(
            &project_id,
            &params.goal,
            policy,
            params.token_budget,
            params.cost_budget_usd,
            params.activate,
        );
        let (program, _event) =
            tokio::task::spawn_blocking(move || store.create_program(&input)).await??;
        self.record_project_manifest_best_effort().await;
        Ok(program)
    }

    pub async fn update_program(&self, params: ProgramUpdateParams) -> Result<Program> {
        crate::program::validate_program_goal(&params.goal)?;
        crate::program::validate_program_policy(&params.policy)?;
        crate::program::validate_program_budgets(params.token_budget, params.cost_budget_usd)?;
        let store = self.store.clone();
        let program_id = params.program_id.clone();
        let update = crate::program::program_update(
            &params.goal,
            params.policy,
            params.token_budget,
            params.cost_budget_usd,
        );
        let program =
            tokio::task::spawn_blocking(move || store.update_program(&program_id, &update))
                .await??
                .map(|(program, _event)| program)
                .ok_or_else(|| anyhow!("program {} was not found", params.program_id))?;
        self.record_project_manifest_best_effort().await;
        Ok(program)
    }

    pub async fn pause_program(&self, params: ProgramActionParams) -> Result<Program> {
        let store = self.store.clone();
        let program_id = params.program_id.clone();
        let reason = params.reason.clone();
        let result = tokio::task::spawn_blocking(move || {
            store.pause_program(&program_id, reason.as_deref())
        })
        .await??;
        if let Some((program, _event)) = result {
            self.record_project_manifest_best_effort().await;
            return Ok(program);
        }
        let store = self.store.clone();
        let program_id = params.program_id.clone();
        let program = tokio::task::spawn_blocking(move || store.get_program(&program_id))
            .await??
            .ok_or_else(|| anyhow!("program {} was not found", params.program_id))?;
        if program.status == ProgramStatus::Paused {
            return Ok(program);
        }
        anyhow::bail!(
            "program {} is {} and cannot be paused",
            program.id,
            program.status
        )
    }

    pub async fn resume_program(&self, params: ProgramActionParams) -> Result<Program> {
        let store = self.store.clone();
        let program_id = params.program_id.clone();
        let result =
            tokio::task::spawn_blocking(move || store.resume_program(&program_id)).await??;
        if let Some((program, _event)) = result {
            self.record_project_manifest_best_effort().await;
            return Ok(program);
        }
        let store = self.store.clone();
        let program_id = params.program_id.clone();
        let program = tokio::task::spawn_blocking(move || store.get_program(&program_id))
            .await??
            .ok_or_else(|| anyhow!("program {} was not found", params.program_id))?;
        if program.status == ProgramStatus::Active {
            return Ok(program);
        }
        anyhow::bail!(
            "program {} is {} and cannot be resumed",
            program.id,
            program.status
        )
    }

    pub async fn delete_program(&self, params: ProgramDeleteParams) -> Result<bool> {
        let store = self.store.clone();
        let program_id = params.program_id.clone();
        let deleted =
            tokio::task::spawn_blocking(move || store.delete_program(&program_id)).await??;
        self.record_project_manifest_best_effort().await;
        if !deleted {
            anyhow::bail!(
                "program {} was not found or could not be deleted",
                params.program_id
            );
        }
        Ok(true)
    }

    pub async fn list_programs(
        &self,
        project_id: Option<String>,
        status: Option<ProgramStatus>,
        limit: Option<u32>,
    ) -> Result<ProgramListResult> {
        let controller = self.programs();
        let programs = tokio::task::spawn_blocking(move || {
            controller.list_programs(project_id.as_deref(), status, limit)
        })
        .await??;
        Ok(ProgramListResult { programs })
    }

    pub async fn program_status(&self, params: ProgramStatusParams) -> Result<ProgramStatusResult> {
        let controller = self.programs();
        let program_id = params.program_id.clone();
        let result =
            tokio::task::spawn_blocking(move || controller.status(&program_id, params.event_limit))
                .await??;
        result.ok_or_else(|| anyhow!("program {} was not found", params.program_id))
    }

    pub async fn list_program_milestones(
        &self,
        program_id: String,
    ) -> Result<ProgramMilestoneListResult> {
        let store = self.store.clone();
        let milestones =
            tokio::task::spawn_blocking(move || store.list_program_milestones(&program_id))
                .await??;
        Ok(ProgramMilestoneListResult { milestones })
    }

    pub async fn create_program_milestone(
        &self,
        params: ProgramMilestoneCreateParams,
    ) -> Result<ProgramMilestone> {
        let store = self.store.clone();
        let program_id = params.program_id.clone();
        let program = tokio::task::spawn_blocking({
            let store = store.clone();
            let program_id = program_id.clone();
            move || store.get_program(&program_id)
        })
        .await??
        .ok_or_else(|| anyhow!("program {program_id} was not found"))?;
        if program.archived || program.status.is_terminal() {
            anyhow::bail!(
                "program {} is archived or terminal and cannot accept milestones",
                program.id
            );
        }
        let milestones = tokio::task::spawn_blocking({
            let store = store.clone();
            let program_id = program_id.clone();
            move || store.list_program_milestones(&program_id)
        })
        .await??;
        let key = match params.key {
            Some(key) => key,
            None => crate::program::next_milestone_key(&milestones),
        };
        crate::program::validate_milestone_key(&key)?;
        crate::program::validate_milestone_dependencies(&milestones, None, &params.deps)?;
        let priority = params.priority.unwrap_or(0);
        let max_attempts = params.max_attempts.unwrap_or(program.policy.max_attempts);
        let profile = params.profile.unwrap_or_else(|| "coding".to_string());
        crate::program::validate_milestone_fields(
            &params.title,
            &params.objective,
            &params.acceptance,
            &params.write_scope,
            &profile,
            priority,
            max_attempts,
            params.token_budget,
            params.cost_budget_usd,
        )?;
        let input = crate::program::new_program_milestone_input(
            &program_id,
            key,
            &params.title,
            &params.objective,
            params.acceptance,
            params.deps,
            priority,
            max_attempts,
            profile,
            params.write_scope,
            params.token_budget,
            params.cost_budget_usd,
        );
        let (milestone, _event) =
            tokio::task::spawn_blocking(move || store.create_program_milestone(&input)).await??;
        self.record_project_manifest_best_effort().await;
        Ok(milestone)
    }

    pub async fn update_program_milestone(
        &self,
        params: ProgramMilestoneUpdateParams,
    ) -> Result<ProgramMilestone> {
        let store = self.store.clone();
        let milestone_id = params.milestone_id.clone();
        let current = tokio::task::spawn_blocking({
            let store = store.clone();
            let milestone_id = milestone_id.clone();
            move || store.get_program_milestone(&milestone_id)
        })
        .await??
        .ok_or_else(|| anyhow!("milestone {milestone_id} was not found"))?;
        let milestones = tokio::task::spawn_blocking({
            let store = store.clone();
            let program_id = current.program_id.clone();
            move || store.list_program_milestones(&program_id)
        })
        .await??;
        let key = params.key.unwrap_or_else(|| current.key.clone());
        let priority = params.priority.unwrap_or(current.priority);
        let max_attempts = params.max_attempts.unwrap_or(current.max_attempts);
        let profile = params
            .profile
            .clone()
            .unwrap_or_else(|| current.profile.clone());
        let update = crate::program::program_milestone_update(
            key,
            &params.title,
            &params.objective,
            params.acceptance,
            params.deps,
            priority,
            max_attempts,
            profile,
            params.write_scope,
            params.token_budget,
            params.cost_budget_usd,
        );
        crate::program::validate_program_milestone_update(&milestones, &current, &update)?;
        let moved_id = milestone_id.clone();
        let milestone =
            tokio::task::spawn_blocking(move || store.update_program_milestone(&moved_id, &update))
                .await??
                .map(|(milestone, _event)| milestone)
                .ok_or_else(|| anyhow!("milestone {milestone_id} cannot be edited"))?;
        self.record_project_manifest_best_effort().await;
        Ok(milestone)
    }

    pub async fn retry_program_milestone(
        &self,
        params: ProgramMilestoneActionParams,
    ) -> Result<ProgramMilestone> {
        let store = self.store.clone();
        let milestone_id = params.milestone_id.clone();
        let milestone =
            tokio::task::spawn_blocking(move || store.retry_program_milestone(&milestone_id))
                .await??
                .map(|(milestone, _event)| milestone)
                .ok_or_else(|| {
                    anyhow!(
                        "milestone {} is not failed, blocked, or cancelled and cannot be retried",
                        params.milestone_id
                    )
                })?;
        self.record_project_manifest_best_effort().await;
        Ok(milestone)
    }

    pub async fn skip_program_milestone(
        &self,
        params: ProgramMilestoneActionParams,
    ) -> Result<ProgramMilestone> {
        let store = self.store.clone();
        let milestone_id = params.milestone_id.clone();
        let reason = params
            .reason
            .unwrap_or_else(|| "skipped by operator".to_string());
        let milestone = tokio::task::spawn_blocking(move || {
            store.cancel_program_milestone(&milestone_id, &reason, true)
        })
        .await??
        .map(|(milestone, _event)| milestone)
        .ok_or_else(|| {
            anyhow!(
                "milestone {} is already terminal or cannot be skipped",
                params.milestone_id
            )
        })?;
        self.record_project_manifest_best_effort().await;
        Ok(milestone)
    }

    pub async fn replay_program(
        &self,
        program_id: &str,
        repair: bool,
    ) -> Result<ProgramReplayReport> {
        let controller = self.programs();
        let program_id = program_id.to_string();
        tokio::task::spawn_blocking(move || controller.replay(&program_id, repair)).await?
    }

    /// Manual controller tick for diagnostics and CLI. Session dispatch still
    /// happens through the agent service, so this method never launches work.
    pub async fn program_tick(&self, params: ProgramTickParams) -> Result<ProgramTickResult> {
        let controller = self.programs();
        let result = controller.reconcile(params.project_id.as_deref()).await?;
        let ready = controller
            .ready_candidates_in_project(params.project_id.as_deref(), None, 50)
            .unwrap_or_default()
            .into_iter()
            .map(|candidate| candidate.milestone)
            .collect();
        Ok(ProgramTickResult {
            programs: result.programs,
            milestones_advanced: result.milestones_advanced,
            sessions_started: 0,
            completed: result.completed,
            failed: result.failed,
            ready,
        })
    }
}

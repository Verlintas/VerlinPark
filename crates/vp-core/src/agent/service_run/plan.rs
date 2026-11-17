use super::*;

impl AgentService {
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
}

use super::*;

mod continuations;
mod governor;
mod orchestration;
mod revisions;
mod stalls;
mod steering;
mod tasks;

impl Engine {
    pub async fn orchestration_policy(&self, project_id: &str) -> Result<OrchestrationPolicy> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking(move || store.get_orchestration_policy(&project_id)).await?
    }

    pub async fn set_orchestration_policy(
        &self,
        project_id: &str,
        policy: &OrchestrationPolicy,
    ) -> Result<OrchestrationPolicy> {
        crate::orchestration::validate_policy(policy)?;
        let store = self.store.clone();
        let project_id_owned = project_id.to_string();
        let policy_owned = policy.clone();
        tokio::task::spawn_blocking(move || {
            if store.get_project(&project_id_owned)?.is_none() {
                return Err(anyhow::anyhow!("project not found: {project_id_owned}"));
            }
            store.set_orchestration_policy(&project_id_owned, &policy_owned)
        })
        .await??;
        self.record_project_manifest().await?;
        self.orchestration_policy(project_id).await
    }

    pub async fn all_events(&self, session_id: &str) -> Result<Vec<Event>> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.list_all_events(&session_id)).await?
    }

    pub async fn current_plan_revision(&self, session_id: &str) -> Result<u32> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.plan_revision_count(&session_id)).await?
    }
}

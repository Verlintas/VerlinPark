use super::*;

mod agent_memory;
mod conflicts;
mod embedding;
mod index;
mod policy;
mod records;
mod support;

impl Engine {
    pub(super) async fn ensure_project_exists(&self, project_id: &str) -> Result<()> {
        let store = self.store.clone();
        let project = project_id.to_string();
        let exists = tokio::task::spawn_blocking(move || store.get_project(&project))
            .await??
            .is_some();
        if !exists {
            anyhow::bail!("project not found: {project_id}");
        }
        Ok(())
    }
}

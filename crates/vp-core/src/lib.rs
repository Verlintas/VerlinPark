//! Application services shared by the daemon and future harness components.
//!
//! M0 is intentionally thin: project/session CRUD, event append, and a
//! broadcast bus. The agent runtime and scheduler land in M1/M2.

use anyhow::Result;
use tokio::sync::broadcast;
use vp_protocol::types::{Event, Project, Session};
use vp_store::Store;

#[derive(Clone)]
pub struct Engine {
    store: Store,
    events: broadcast::Sender<Event>,
}

impl Engine {
    pub fn new(store: Store) -> Self {
        let (events, _) = broadcast::channel(2048);
        Self { store, events }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    pub async fn add_project(&self, root: String, name: Option<String>) -> Result<Project> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.add_project(&root, name.as_deref())).await?
    }

    pub async fn list_projects(&self) -> Result<Vec<Project>> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.list_projects()).await?
    }

    pub async fn create_session(&self, project_id: String, goal: String) -> Result<Session> {
        let store = self.store.clone();
        let session =
            tokio::task::spawn_blocking(move || store.create_session(&project_id, &goal)).await??;
        let payload = serde_json::json!({
            "goal": session.goal,
            "project_id": session.project_id,
        });
        let _ = self
            .append_event(&session.id, "session.created", payload)
            .await?;
        Ok(session)
    }

    pub async fn list_sessions(&self, project_id: Option<String>) -> Result<Vec<Session>> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.list_sessions(project_id.as_deref())).await?
    }

    pub async fn list_events(
        &self,
        session_id: String,
        since_seq: u64,
        limit: u32,
    ) -> Result<Vec<Event>> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.list_events(&session_id, since_seq, limit))
            .await?
    }

    pub async fn append_event(
        &self,
        session_id: &str,
        kind: &str,
        payload: serde_json::Value,
    ) -> Result<Event> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let kind = kind.to_string();
        let event =
            tokio::task::spawn_blocking(move || store.append_event(&session_id, &kind, payload))
                .await??;
        let _ = self.events.send(event.clone());
        Ok(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn creates_session_and_broadcasts_event() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let engine = Engine::new(store);
        let mut rx = engine.subscribe();
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("demo".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session(project.id, "ship it".into())
            .await
            .unwrap();
        let event = rx.recv().await.unwrap();
        assert_eq!(event.session_id, session.id);
        assert_eq!(event.kind, "session.created");
        let events = engine.list_events(session.id, 0, 10).await.unwrap();
        assert_eq!(events.len(), 1);
    }
}

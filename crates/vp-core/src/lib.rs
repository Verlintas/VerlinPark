//! Application services shared by the daemon and future harness components.
//!
//! M1 adds the Leader agent loop, tool registry, approvals, and event stream.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use tokio::sync::{broadcast, oneshot};
use uuid::Uuid;
use vp_protocol::types::{
    AgentRecord, Approval, Event, Project, Session, SessionCommit, SessionDiff, StoredMessage,
    TodoItem,
};
use vp_store::Store;

pub mod agent;
pub mod tools;
pub mod worktree;

#[derive(Clone)]
pub struct Engine {
    store: Store,
    events: broadcast::Sender<Event>,
    approval_waiters: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>,
    git_lock: Arc<Mutex<()>>,
}

impl Engine {
    pub fn new(store: Store) -> Self {
        let (events, _) = broadcast::channel(2048);
        Self {
            store,
            events,
            approval_waiters: Arc::new(Mutex::new(HashMap::new())),
            git_lock: Arc::new(Mutex::new(())),
        }
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

    pub async fn create_session(
        &self,
        project_id: String,
        goal: String,
        token_budget: Option<u64>,
    ) -> Result<Session> {
        let store = self.store.clone();
        let project_id_for_load = project_id.clone();
        let project = tokio::task::spawn_blocking(move || store.get_project(&project_id_for_load))
            .await??
            .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;

        let session_id = Uuid::now_v7().to_string();
        let session_id_for_worktree = session_id.clone();
        let project_id_for_worktree = project.id.clone();
        let root = PathBuf::from(&project.root);
        let data_dir = self.store.data_dir().to_path_buf();
        let git_lock = self.git_lock.clone();
        let worktree = tokio::task::spawn_blocking(move || {
            let _guard = git_lock
                .lock()
                .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
            worktree::create(
                &root,
                &data_dir,
                &project_id_for_worktree,
                &session_id_for_worktree,
            )
        })
        .await??;

        let store = self.store.clone();
        let worktree_path = worktree.path.to_string_lossy().to_string();
        let branch = worktree.branch.clone();
        let session = tokio::task::spawn_blocking({
            let session_id = session_id.clone();
            let project_id = project.id.clone();
            let goal = goal.clone();
            let worktree_path = worktree_path.clone();
            let branch = branch.clone();
            move || {
                store.create_session_with_worktree(
                    &session_id,
                    &project_id,
                    &goal,
                    &worktree_path,
                    &branch,
                    token_budget,
                )
            }
        })
        .await;

        let session = match session {
            Ok(Ok(session)) => session,
            Ok(Err(error)) => {
                worktree::remove(
                    &PathBuf::from(&project.root),
                    &worktree.path,
                    &worktree.branch,
                );
                return Err(error);
            }
            Err(error) => {
                worktree::remove(
                    &PathBuf::from(&project.root),
                    &worktree.path,
                    &worktree.branch,
                );
                return Err(error.into());
            }
        };

        let payload = serde_json::json!({
            "goal": session.goal,
            "project_id": session.project_id,
            "worktree": session.worktree,
            "branch": session.branch,
        });
        let _ = self
            .append_event(&session.id, "session.created", payload)
            .await?;
        Ok(session)
    }

    pub async fn ensure_session_worktree(&self, session_id: &str) -> Result<Session> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let (session, project) = tokio::task::spawn_blocking(move || {
            let session = store
                .get_session(&id)?
                .ok_or_else(|| anyhow::anyhow!("session not found"))?;
            let project = store
                .get_project(&session.project_id)?
                .ok_or_else(|| anyhow::anyhow!("project not found"))?;
            Ok::<_, anyhow::Error>((session, project))
        })
        .await??;

        if let Some(path) = &session.worktree
            && Path::new(path).join(".git").is_file()
        {
            return Ok(session);
        }

        let data_dir = self.store.data_dir().to_path_buf();
        let root = PathBuf::from(&project.root);
        let project_id = project.id.clone();
        let session_id = session.id.clone();
        let git_lock = self.git_lock.clone();
        let info = tokio::task::spawn_blocking(move || {
            let _guard = git_lock
                .lock()
                .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
            worktree::create(&root, &data_dir, &project_id, &session_id)
        })
        .await??;

        let worktree_path = info.path.to_string_lossy().to_string();
        let store = self.store.clone();
        let id = session.id.clone();
        let branch = info.branch.clone();
        let worktree_for_store = worktree_path.clone();
        tokio::task::spawn_blocking(move || {
            store.set_session_worktree(&id, &worktree_for_store, &branch)
        })
        .await??;

        let session = Session {
            worktree: Some(worktree_path),
            branch: Some(info.branch),
            ..session
        };
        self.append_event(
            &session.id,
            "session.worktree",
            serde_json::json!({
                "worktree": session.worktree,
                "branch": session.branch,
            }),
        )
        .await?;
        Ok(session)
    }

    pub async fn list_sessions(&self, project_id: Option<String>) -> Result<Vec<Session>> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.list_sessions(project_id.as_deref())).await?
    }

    pub async fn set_todos(&self, session_id: &str, items: Vec<TodoItem>) -> Result<()> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let items_for_store = items.clone();
        tokio::task::spawn_blocking(move || store.set_todos(&id, &items_for_store)).await??;
        self.append_event(
            session_id,
            "todo.updated",
            serde_json::json!({"items": items}),
        )
        .await?;
        Ok(())
    }

    pub async fn list_todos(&self, session_id: &str) -> Result<Vec<TodoItem>> {
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.list_todos(&id)).await?
    }

    pub async fn recover_interrupted_sessions(&self) -> Result<usize> {
        let store = self.store.clone();
        let (sessions, approvals, _agents) = tokio::task::spawn_blocking(move || {
            let sessions = store.interrupt_running_sessions()?;
            let agents = store.interrupt_running_agents()?;
            let pending = store
                .list_approvals(None)?
                .into_iter()
                .filter(|approval| approval.status == "pending")
                .collect::<Vec<_>>();
            let mut denied = Vec::new();
            for approval in pending {
                if let Some(approval) = store.resolve_approval(&approval.id, false)? {
                    denied.push(approval);
                }
            }
            Ok::<_, anyhow::Error>((sessions, denied, agents))
        })
        .await??;

        for session_id in &sessions {
            let _ = self
                .append_event(
                    session_id,
                    "session.updated",
                    serde_json::json!({"status": "failed"}),
                )
                .await;
            let _ = self
                .append_event(
                    session_id,
                    "agent.failed",
                    serde_json::json!({
                        "error": "The daemon restarted while this run was active."
                    }),
                )
                .await;
        }
        for approval in &approvals {
            let _ = self
                .append_event(
                    &approval.session_id,
                    "approval.resolved",
                    serde_json::json!({
                        "approval_id": approval.id,
                        "approved": false,
                    }),
                )
                .await;
        }
        Ok(sessions.len())
    }

    pub async fn discard_session(&self, session_id: &str) -> Result<()> {
        let session = self.ensure_session_worktree(session_id).await?;
        let worktree = session
            .worktree
            .clone()
            .ok_or_else(|| anyhow::anyhow!("session has no worktree"))?;
        let worktree = PathBuf::from(worktree);
        tokio::task::spawn_blocking(move || {
            git_output(&worktree, &["reset", "--hard", "HEAD"])?;
            git_output(&worktree, &["clean", "-fd"])?;
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        self.append_event(&session.id, "session.discarded", serde_json::json!({}))
            .await?;
        Ok(())
    }

    pub async fn commit_session(&self, session_id: &str, message: &str) -> Result<SessionCommit> {
        let message = message.trim().to_string();
        if message.is_empty() {
            return Err(anyhow::anyhow!("commit message is required"));
        }
        let session = self.ensure_session_worktree(session_id).await?;
        let worktree = session
            .worktree
            .clone()
            .ok_or_else(|| anyhow::anyhow!("session has no worktree"))?;
        let session_id = session.id.clone();
        let worktree = PathBuf::from(worktree);
        let message_for_git = message.clone();
        let (commit, summary) = tokio::task::spawn_blocking(move || {
            git_output(&worktree, &["add", "-A"])?;
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(&worktree)
                .args(["commit", "-m", &message_for_git])
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .context("running git commit")?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if stderr.contains("nothing to commit") || stdout.contains("nothing to commit") {
                    anyhow::bail!("no changes to commit");
                }
                anyhow::bail!("git commit failed: {stderr}");
            }
            let commit = git_output(&worktree, &["rev-parse", "--short", "HEAD"])?;
            let summary = git_output(
                &worktree,
                &["show", "--stat", "--oneline", "--no-color", "-1"],
            )?;
            Ok::<_, anyhow::Error>((commit, summary))
        })
        .await??;

        self.append_event(
            &session_id,
            "session.committed",
            serde_json::json!({
                "commit": commit,
                "message": message,
            }),
        )
        .await?;

        Ok(SessionCommit {
            session_id,
            commit,
            message,
            summary,
        })
    }

    pub async fn record_usage(&self, session_id: &str, input: u64, output: u64) -> Result<()> {
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.add_session_usage(&id, input, output)).await??;
        self.append_event(
            session_id,
            "agent.usage",
            serde_json::json!({
                "input_tokens": input,
                "output_tokens": output,
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn session_diff(&self, session_id: &str) -> Result<SessionDiff> {
        let session = self.ensure_session_worktree(session_id).await?;
        let worktree = session
            .worktree
            .clone()
            .ok_or_else(|| anyhow::anyhow!("session has no worktree"))?;
        let branch = session.branch.clone().unwrap_or_default();
        let session_id = session.id.clone();
        let worktree_for_git = PathBuf::from(&worktree);
        let (status, patch) = tokio::task::spawn_blocking(move || {
            let status = git_output(&worktree_for_git, &["status", "--short"])?;
            let stat = git_output(&worktree_for_git, &["diff", "--stat"])?;
            let diff = git_output(&worktree_for_git, &["diff", "--no-color"])?;
            let mut patch = String::new();
            if !stat.is_empty() {
                patch.push_str(&stat);
                patch.push('\n');
            }
            if !diff.is_empty() {
                patch.push_str(&diff);
            }
            const MAX_DIFF: usize = 200_000;
            if patch.len() > MAX_DIFF {
                patch.truncate(MAX_DIFF);
                patch.push_str("\n… diff truncated");
            }
            Ok::<_, anyhow::Error>((status, patch))
        })
        .await??;
        Ok(SessionDiff {
            session_id,
            branch,
            worktree,
            status,
            patch,
        })
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

    pub async fn update_session_status(&self, session_id: &str, status: &str) -> Result<()> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let id_for_store = session_id.clone();
        let status = status.to_string();
        let status_for_store = status.clone();
        tokio::task::spawn_blocking(move || {
            store.update_session_status(&id_for_store, &status_for_store)
        })
        .await??;
        self.append_event(
            &session_id,
            "session.updated",
            serde_json::json!({"status": status}),
        )
        .await?;
        Ok(())
    }

    pub async fn create_agent(
        &self,
        session_id: &str,
        role: &str,
        name: &str,
        parent_id: Option<String>,
        model: &str,
    ) -> Result<AgentRecord> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let role = role.to_string();
        let name = name.to_string();
        let model = model.to_string();
        tokio::task::spawn_blocking(move || {
            store.create_agent(&session_id, &role, &name, parent_id.as_deref(), &model)
        })
        .await?
    }

    pub async fn update_agent_status(&self, agent_id: &str, status: &str) -> Result<()> {
        let store = self.store.clone();
        let id = agent_id.to_string();
        let status = status.to_string();
        tokio::task::spawn_blocking(move || store.update_agent_status(&id, &status)).await?
    }

    pub async fn list_agents(&self, session_id: &str) -> Result<Vec<AgentRecord>> {
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.list_agents(&id)).await?
    }

    pub async fn create_leader_agent(&self, session_id: &str, model: &str) -> Result<String> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let model = model.to_string();
        tokio::task::spawn_blocking(move || store.create_leader_agent(&session_id, &model)).await?
    }

    pub async fn append_message(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        tool_calls: serde_json::Value,
        tool_call_id: Option<String>,
    ) -> Result<StoredMessage> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let role = role.to_string();
        let content = content.to_string();
        tokio::task::spawn_blocking(move || {
            store.append_message(
                &session_id,
                &role,
                &content,
                &tool_calls,
                tool_call_id.as_deref(),
            )
        })
        .await?
    }

    pub async fn list_messages(&self, session_id: &str) -> Result<Vec<StoredMessage>> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.list_messages(&session_id)).await?
    }

    pub async fn list_approvals(&self, session_id: Option<String>) -> Result<Vec<Approval>> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.list_approvals(session_id.as_deref())).await?
    }

    pub async fn request_approval(
        &self,
        session_id: &str,
        tool_call_id: &str,
        tool: &str,
        input: serde_json::Value,
        agent: Option<serde_json::Value>,
    ) -> Result<(Approval, oneshot::Receiver<bool>)> {
        let store = self.store.clone();
        let session_id_owned = session_id.to_string();
        let tool_call_id = tool_call_id.to_string();
        let tool = tool.to_string();
        let approval = tokio::task::spawn_blocking(move || {
            store.create_approval(&session_id_owned, &tool_call_id, &tool, &input)
        })
        .await??;
        let (sender, receiver) = oneshot::channel();
        self.approval_waiters
            .lock()
            .map_err(|_| anyhow::anyhow!("approval mutex poisoned"))?
            .insert(approval.id.clone(), sender);
        let mut payload = serde_json::to_value(&approval)?;
        if let (Some(payload), Some(agent)) = (payload.as_object_mut(), agent)
            && let Some(agent) = agent.as_object()
        {
            for (key, value) in agent {
                payload.insert(key.clone(), value.clone());
            }
        }
        self.append_event(session_id, "approval.requested", payload)
            .await?;
        Ok((approval, receiver))
    }

    pub async fn resolve_approval(&self, approval_id: &str, approved: bool) -> Result<()> {
        let store = self.store.clone();
        let id = approval_id.to_string();
        let approval =
            tokio::task::spawn_blocking(move || store.resolve_approval(&id, approved)).await??;
        if let Some(approval) = approval {
            if let Some(sender) = self
                .approval_waiters
                .lock()
                .ok()
                .and_then(|mut waiters| waiters.remove(approval_id))
            {
                let _ = sender.send(approved);
            }
            self.append_event(
                &approval.session_id,
                "approval.resolved",
                serde_json::json!({
                    "approval_id": approval.id,
                    "approved": approved,
                }),
            )
            .await?;
        }
        Ok(())
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

fn git_output(root: &Path, args: &[&str]) -> Result<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .context("running git")?;
    if !output.status.success() {
        anyhow::bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[tokio::test]
    async fn discards_uncommitted_worktree_changes() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("demo".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session(project.id, "discard".into(), None)
            .await
            .unwrap();
        let worktree = PathBuf::from(session.worktree.clone().unwrap());
        std::fs::write(worktree.join("README.md"), "dirty\n").unwrap();
        std::fs::write(worktree.join("scratch.txt"), "untracked\n").unwrap();

        engine.discard_session(&session.id).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(worktree.join("README.md")).unwrap(),
            "hello\n"
        );
        assert!(!worktree.join("scratch.txt").exists());
        assert!(
            engine
                .session_diff(&session.id)
                .await
                .unwrap()
                .patch
                .is_empty()
        );
        let events = engine.list_events(session.id, 0, 100).await.unwrap();
        assert!(events.iter().any(|event| event.kind == "session.discarded"));
    }

    #[tokio::test]
    async fn attributes_approvals_to_agents() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("demo".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session(project.id, "approve".into(), None)
            .await
            .unwrap();
        let (_approval, _receiver) = engine
            .request_approval(
                &session.id,
                "call-1",
                "write_file",
                serde_json::json!({"path": "README.md"}),
                Some(serde_json::json!({
                    "agent_id": "worker-1",
                    "agent_name": "Worker 1",
                    "agent_role": "worker"
                })),
            )
            .await
            .unwrap();
        let events = engine.list_events(session.id, 0, 10).await.unwrap();
        let event = events
            .iter()
            .find(|event| event.kind == "approval.requested")
            .unwrap();
        assert_eq!(event.payload["agent_name"], "Worker 1");
        assert_eq!(event.payload["agent_role"], "worker");
    }

    #[tokio::test]
    async fn commits_worktree_changes_once() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("demo".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session(project.id, "commit".into(), None)
            .await
            .unwrap();
        let worktree = session.worktree.clone().unwrap();
        std::fs::write(Path::new(&worktree).join("README.md"), "committed\n").unwrap();

        let commit = engine
            .commit_session(&session.id, "update readme")
            .await
            .unwrap();
        assert_eq!(commit.commit.len(), 7);
        assert_eq!(commit.message, "update readme");
        assert!(
            engine
                .session_diff(&session.id)
                .await
                .unwrap()
                .patch
                .is_empty()
        );

        let error = engine
            .commit_session(&session.id, "again")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("no changes"));
        let events = engine.list_events(session.id, 0, 100).await.unwrap();
        assert!(events.iter().any(|event| event.kind == "session.committed"));
    }

    #[tokio::test]
    async fn recovers_interrupted_sessions_and_pending_approvals() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("demo".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session(project.id, "run".into(), None)
            .await
            .unwrap();
        engine
            .create_leader_agent(&session.id, "test")
            .await
            .unwrap();
        engine
            .update_session_status(&session.id, "running")
            .await
            .unwrap();
        let (approval, _receiver) = engine
            .request_approval(
                &session.id,
                "call-1",
                "write_file",
                serde_json::json!({}),
                None,
            )
            .await
            .unwrap();
        let recovered = engine.recover_interrupted_sessions().await.unwrap();
        assert_eq!(recovered, 1);
        let sessions = engine
            .list_sessions(Some(session.project_id.clone()))
            .await
            .unwrap();
        assert_eq!(sessions[0].status, "failed");
        let approvals = engine
            .list_approvals(Some(session.id.clone()))
            .await
            .unwrap();
        assert_eq!(approvals[0].status, "denied");
        let events = engine.list_events(session.id, 0, 100).await.unwrap();
        assert!(events.iter().any(|event| event.kind == "agent.failed"));
        assert!(events.iter().any(|event| event.kind == "approval.resolved"
            && event.payload["approval_id"] == approval.id));
    }

    #[tokio::test]
    async fn creates_session_and_broadcasts_event() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
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
            .create_session(project.id, "ship it".into(), Some(100_000))
            .await
            .unwrap();
        assert!(session.worktree.is_some());
        assert!(session.branch.is_some());
        assert_eq!(session.token_budget, Some(100_000));
        let event = rx.recv().await.unwrap();
        assert_eq!(event.session_id, session.id);
        assert_eq!(event.kind, "session.created");
        let worktree = session.worktree.clone().unwrap();
        std::fs::write(Path::new(&worktree).join("README.md"), "changed\n").unwrap();
        let diff = engine.session_diff(&session.id).await.unwrap();
        assert!(diff.patch.contains("-hello"));
        assert!(diff.patch.contains("+changed"));

        let events = engine.list_events(session.id, 0, 10).await.unwrap();
        assert_eq!(events.len(), 1);
    }
}

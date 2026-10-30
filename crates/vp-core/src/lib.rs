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
    AgentRecord, Approval, Event, MergeQueueEntry, Project, PullRequestResult, Session,
    SessionCommit, SessionDiff, SessionLandResult, SessionReplayReport, StoredMessage, TeamPlan,
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
        let base_commit = worktree.base_commit.clone();
        let session = tokio::task::spawn_blocking({
            let session_id = session_id.clone();
            let project_id = project.id.clone();
            let goal = goal.clone();
            let worktree_path = worktree_path.clone();
            let branch = branch.clone();
            let base_commit = base_commit.clone();
            move || {
                store.create_session_with_worktree(
                    &session_id,
                    &project_id,
                    &goal,
                    &worktree_path,
                    &branch,
                    token_budget,
                    &base_commit,
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

    pub async fn set_team_plan(&self, session_id: &str, plan: &TeamPlan) -> Result<()> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let json = serde_json::to_string(plan)?;
        let event = tokio::task::spawn_blocking(move || store.set_team_plan_with_event(&id, &json))
            .await??;
        let _ = self.events.send(event);
        Ok(())
    }

    pub async fn get_team_plan(&self, session_id: &str) -> Result<Option<TeamPlan>> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let raw = tokio::task::spawn_blocking(move || store.get_team_plan(&id)).await??;
        Ok(raw.and_then(|json| serde_json::from_str(&json).ok()))
    }

    pub async fn set_todos(&self, session_id: &str, items: Vec<TodoItem>) -> Result<()> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let items_for_store = items.clone();
        let event =
            tokio::task::spawn_blocking(move || store.set_todos_with_event(&id, &items_for_store))
                .await??;
        let _ = self.events.send(event);
        Ok(())
    }

    pub async fn list_todos(&self, session_id: &str) -> Result<Vec<TodoItem>> {
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.list_todos(&id)).await?
    }

    pub async fn recover_interrupted_sessions(&self) -> Result<usize> {
        let store = self.store.clone();
        let (sessions, events) =
            tokio::task::spawn_blocking(move || store.recover_state()).await??;
        for event in events {
            let _ = self.events.send(event);
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

    pub async fn land_session(&self, session_id: &str) -> Result<SessionLandResult> {
        let session = self.ensure_session_worktree(session_id).await?;
        let session_branch = session
            .branch
            .clone()
            .ok_or_else(|| anyhow::anyhow!("session has no branch"))?;
        let session_worktree = PathBuf::from(
            session
                .worktree
                .clone()
                .ok_or_else(|| anyhow::anyhow!("session has no worktree"))?,
        );
        let status = tokio::task::spawn_blocking(move || {
            git_output(&session_worktree, &["status", "--porcelain"])
        })
        .await??;
        if !status.is_empty() {
            anyhow::bail!("session worktree has uncommitted changes; commit or discard them first");
        }
        let store = self.store.clone();
        let project_id = session.project_id.clone();
        let project = tokio::task::spawn_blocking(move || store.get_project(&project_id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("project not found"))?;
        let target_branch = project.default_branch.clone();
        let project_root = PathBuf::from(&project.root);
        let branch_for_merge = session_branch.clone();
        let target_for_merge = target_branch.clone();
        let git_lock = self.git_lock.clone();
        let commit = tokio::task::spawn_blocking(move || -> Result<String> {
            let _guard = git_lock
                .lock()
                .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
            let status = git_output(&project_root, &["status", "--porcelain"])?;
            if !status.is_empty() {
                anyhow::bail!(
                    "project worktree has uncommitted changes; commit or stash them first"
                );
            }
            let current = git_output(&project_root, &["branch", "--show-current"])?;
            if current != target_for_merge {
                anyhow::bail!("project is on branch '{current}', expected '{target_for_merge}'");
            }
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(&project_root)
                .args([
                    "merge",
                    "--no-ff",
                    &branch_for_merge,
                    "-m",
                    &format!("land {branch_for_merge}"),
                ])
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .context("running git merge")?;
            if !output.status.success() {
                let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
                let _ = git_output(&project_root, &["merge", "--abort"]);
                anyhow::bail!("landing merge failed: {detail}");
            }
            git_output(&project_root, &["rev-parse", "--short", "HEAD"])
        })
        .await??;

        self.append_event(
            &session.id,
            "session.landed",
            serde_json::json!({
                "branch": session_branch,
                "target_branch": target_branch,
                "commit": commit,
            }),
        )
        .await?;
        self.update_session_status(&session.id, "landed").await?;
        Ok(SessionLandResult {
            session_id: session.id,
            branch: session_branch,
            target_branch,
            commit,
            detail: "landed into the project default branch".to_string(),
        })
    }

    pub async fn open_pull_request(&self, session_id: &str) -> Result<PullRequestResult> {
        let gh = std::env::var("VERLINPARK_GH_BIN").unwrap_or_else(|_| "gh".to_string());
        self.open_pull_request_with_gh(session_id, &gh).await
    }

    async fn open_pull_request_with_gh(
        &self,
        session_id: &str,
        gh_bin: &str,
    ) -> Result<PullRequestResult> {
        let session = self.ensure_session_worktree(session_id).await?;
        if session.status == "running" {
            anyhow::bail!("cannot open a PR while the session is running");
        }
        let branch = session
            .branch
            .clone()
            .ok_or_else(|| anyhow::anyhow!("session has no branch"))?;
        let session_worktree = PathBuf::from(
            session
                .worktree
                .clone()
                .ok_or_else(|| anyhow::anyhow!("session has no worktree"))?,
        );
        let status = tokio::task::spawn_blocking(move || {
            git_output(&session_worktree, &["status", "--porcelain"])
        })
        .await??;
        if !status.is_empty() {
            anyhow::bail!("session worktree has uncommitted changes; commit or discard them first");
        }

        let store = self.store.clone();
        let project_id = session.project_id.clone();
        let project = tokio::task::spawn_blocking(move || store.get_project(&project_id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("project not found"))?;
        let project_root = PathBuf::from(&project.root);
        let base_branch = project.default_branch.clone();
        let branch_for_push = branch.clone();
        let base_for_check = base_branch.clone();
        let gh_bin = gh_bin.to_string();
        let title = session
            .goal
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("VerlinPark change")
            .chars()
            .take(120)
            .collect::<String>();
        let body = format!(
            "Opened from VerlinPark session {}.\n\nGoal: {}",
            session.id, session.goal
        );
        let title_for_gh = title.clone();
        let body_for_gh = body.clone();
        let gh_bin_for_pr = gh_bin.clone();
        let git_lock = self.git_lock.clone();

        tokio::task::spawn_blocking(move || -> Result<()> {
            let _guard = git_lock
                .lock()
                .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
            git_output(&project_root, &["remote", "get-url", "origin"])
                .context("a git origin remote is required to open a PR")?;
            let ahead = git_output(
                &project_root,
                &[
                    "rev-list",
                    "--count",
                    &format!("{base_for_check}..{branch_for_push}"),
                ],
            )?;
            if ahead.trim() == "0" {
                anyhow::bail!("session branch has no commits ahead of {base_for_check}");
            }
            let auth = std::process::Command::new(&gh_bin)
                .args(["auth", "status"])
                .current_dir(&project_root)
                .output()
                .context("running gh auth status")?;
            if !auth.status.success() {
                anyhow::bail!(
                    "gh is not authenticated: {}",
                    String::from_utf8_lossy(&auth.stderr).trim()
                );
            }
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(&project_root)
                .args(["push", "-u", "origin", &branch_for_push])
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .context("running git push")?;
            if !output.status.success() {
                anyhow::bail!(
                    "git push failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
            Ok(())
        })
        .await??;

        self.append_event(
            &session.id,
            "session.pushed",
            serde_json::json!({
                "branch": branch,
                "base_branch": base_branch,
            }),
        )
        .await?;

        let project_root_for_gh = PathBuf::from(&project.root);
        let branch_for_pr = branch.clone();
        let base_for_pr = base_branch.clone();
        let url = tokio::task::spawn_blocking(move || -> Result<String> {
            let output = std::process::Command::new(&gh_bin_for_pr)
                .args([
                    "pr",
                    "create",
                    "--base",
                    &base_for_pr,
                    "--head",
                    &branch_for_pr,
                    "--title",
                    &title_for_gh,
                    "--body",
                    &body_for_gh,
                ])
                .current_dir(&project_root_for_gh)
                .output()
                .context("running gh pr create")?;
            if !output.status.success() {
                anyhow::bail!(
                    "branch pushed, but gh pr create failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
            let stdout = String::from_utf8_lossy(&output.stdout);
            let url = stdout
                .lines()
                .map(str::trim)
                .find(|line| line.starts_with("http"))
                .ok_or_else(|| {
                    anyhow::anyhow!("gh pr create succeeded but did not return a URL: {stdout}")
                })?
                .to_string();
            Ok(url)
        })
        .await??;

        self.append_event(
            &session.id,
            "session.pr_opened",
            serde_json::json!({
                "branch": branch,
                "base_branch": base_branch,
                "url": url,
            }),
        )
        .await?;
        Ok(PullRequestResult {
            session_id: session.id,
            branch,
            base_branch,
            pushed: true,
            url: Some(url.clone()),
            detail: format!("branch pushed and PR opened at {url}"),
        })
    }

    pub async fn record_usage(&self, session_id: &str, input: u64, output: u64) -> Result<()> {
        self.record_usage_with_source(session_id, input, output, false)
            .await
    }

    pub async fn record_estimated_usage(
        &self,
        session_id: &str,
        input: u64,
        output: u64,
    ) -> Result<()> {
        self.record_usage_with_source(session_id, input, output, true)
            .await
    }

    async fn record_usage_with_source(
        &self,
        session_id: &str,
        input: u64,
        output: u64,
        estimated: bool,
    ) -> Result<()> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let event = tokio::task::spawn_blocking(move || {
            store.add_session_usage_with_event(&id, input, output, estimated)
        })
        .await??;
        let _ = self.events.send(event);
        Ok(())
    }

    pub async fn session_diff(&self, session_id: &str) -> Result<SessionDiff> {
        let session = self.ensure_session_worktree(session_id).await?;
        let worktree = session
            .worktree
            .clone()
            .ok_or_else(|| anyhow::anyhow!("session has no worktree"))?;
        let branch = session.branch.clone().unwrap_or_default();
        let base_commit = session.base_commit.clone();
        let session_id = session.id.clone();
        let diff_base = base_commit.clone().unwrap_or_else(|| "HEAD".to_string());
        let worktree_for_git = PathBuf::from(&worktree);
        let (status, patch) = tokio::task::spawn_blocking(move || {
            let status = git_output(&worktree_for_git, &["status", "--short"])?;
            let stat = git_output(&worktree_for_git, &["diff", "--stat", &diff_base])?;
            let diff = git_output(&worktree_for_git, &["diff", "--no-color", &diff_base])?;
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
            base_commit,
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

    pub async fn replay_session(&self, session_id: &str) -> Result<SessionReplayReport> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let (session, messages, plan, todos, queue, events) =
            tokio::task::spawn_blocking(move || -> Result<_> {
                let session = store
                    .get_session(&id)?
                    .ok_or_else(|| anyhow::anyhow!("session not found: {id}"))?;
                let messages = store.list_messages(&id)?;
                let plan = store.get_team_plan(&id)?;
                let todos = store.list_todos(&id)?;
                let queue = store.list_merge_queue(&id)?;
                let events = store.list_all_events(&id)?;
                Ok((session, messages, plan, todos, queue, events))
            })
            .await??;

        let mut mismatches = Vec::new();
        let replay_status = events
            .iter()
            .rev()
            .find(|event| event.kind == "session.updated")
            .and_then(|event| event.payload.get("status"))
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                events
                    .iter()
                    .any(|event| event.kind == "session.created")
                    .then_some("draft")
            });
        let status_matches = replay_status == Some(session.status.as_str());
        if !status_matches {
            mismatches.push(format!(
                "session status: projection={}, replay={:?}",
                session.status, replay_status
            ));
        }

        let (replay_in, replay_out) = events
            .iter()
            .filter(|event| event.kind == "agent.usage")
            .fold((0u64, 0u64), |(input, output), event| {
                (
                    input.saturating_add(event.payload["input_tokens"].as_u64().unwrap_or(0)),
                    output.saturating_add(event.payload["output_tokens"].as_u64().unwrap_or(0)),
                )
            });
        let usage_matches = replay_in == session.tokens_in && replay_out == session.tokens_out;
        if !usage_matches {
            mismatches.push(format!(
                "usage: projection={}/{}, replay={replay_in}/{replay_out}",
                session.tokens_in, session.tokens_out
            ));
        }

        let replay_plan = events
            .iter()
            .rev()
            .find(|event| event.kind == "team_plan.submitted")
            .map(|event| event.payload.clone());
        let projected_plan = plan
            .as_deref()
            .and_then(|plan| serde_json::from_str::<serde_json::Value>(plan).ok());
        let plan_matches = replay_plan == projected_plan;
        if !plan_matches {
            mismatches.push("team plan projection differs from event stream".to_string());
        }

        let replay_todos = events
            .iter()
            .rev()
            .find(|event| event.kind == "todo.updated")
            .map(|event| event.payload["items"].clone())
            .unwrap_or_else(|| serde_json::json!([]));
        let projected_todos = serde_json::to_value(&todos)?;
        let todos_matches = replay_todos == projected_todos;
        if !todos_matches {
            mismatches.push("todo projection differs from event stream".to_string());
        }

        let replay_messages = events
            .iter()
            .filter(|event| event.kind == "message.created")
            .count();
        let messages_matches = replay_messages == messages.len();
        if !messages_matches {
            mismatches.push(format!(
                "messages: projection={}, replay={replay_messages}",
                messages.len()
            ));
        }

        let replay_queue = events
            .iter()
            .filter_map(|event| {
                let entry = event.payload.get("entry")?;
                Some((
                    entry.get("id")?.as_str()?.to_string(),
                    entry.get("status")?.as_str()?.to_string(),
                ))
            })
            .fold(HashMap::new(), |mut map, (id, status)| {
                map.insert(id, status);
                map
            });
        let projected_queue = queue
            .iter()
            .map(|entry| (entry.id.clone(), entry.status.clone()))
            .collect::<HashMap<_, _>>();
        let queue_matches = replay_queue == projected_queue;
        if !queue_matches {
            mismatches.push("merge queue projection differs from event stream".to_string());
        }

        let consistent = status_matches
            && usage_matches
            && plan_matches
            && todos_matches
            && messages_matches
            && queue_matches;
        Ok(SessionReplayReport {
            session_id: session.id,
            event_count: events.len(),
            status_matches,
            usage_matches,
            plan_matches,
            todos_matches,
            messages_matches,
            queue_matches,
            consistent,
            mismatches,
        })
    }

    pub async fn update_session_status(&self, session_id: &str, status: &str) -> Result<()> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let status = status.to_string();
        let event = tokio::task::spawn_blocking(move || {
            store.update_session_status_with_event(&id, &status)
        })
        .await??;
        let _ = self.events.send(event);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_agent(
        &self,
        session_id: &str,
        role: &str,
        name: &str,
        title: Option<String>,
        parent_id: Option<String>,
        model: &str,
    ) -> Result<AgentRecord> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let role = role.to_string();
        let name = name.to_string();
        let model = model.to_string();
        tokio::task::spawn_blocking(move || {
            store.create_agent(
                &session_id,
                &role,
                &name,
                title.as_deref(),
                parent_id.as_deref(),
                &model,
            )
        })
        .await?
    }

    pub async fn create_task_worktree(
        &self,
        project_root: &str,
        project_id: &str,
        session_id: &str,
        task_id: &str,
        base_branch: &str,
    ) -> Result<worktree::WorktreeInfo> {
        let data_dir = self.store.data_dir().to_path_buf();
        let root = PathBuf::from(project_root);
        let project_id = project_id.to_string();
        let session_id = session_id.to_string();
        let task_id = task_id.to_string();
        let base_branch = base_branch.to_string();
        let git_lock = self.git_lock.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = git_lock
                .lock()
                .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
            worktree::create_task(
                &root,
                &data_dir,
                &project_id,
                &session_id,
                &task_id,
                &base_branch,
            )
        })
        .await?
    }

    pub async fn set_agent_worktree(
        &self,
        agent_id: &str,
        worktree: &str,
        branch: &str,
    ) -> Result<()> {
        let store = self.store.clone();
        let id = agent_id.to_string();
        let worktree = worktree.to_string();
        let branch = branch.to_string();
        tokio::task::spawn_blocking(move || store.set_agent_worktree(&id, &worktree, &branch))
            .await?
    }

    pub async fn set_agent_profile(&self, agent_id: &str, profile: &str) -> Result<()> {
        let store = self.store.clone();
        let id = agent_id.to_string();
        let profile = profile.to_string();
        tokio::task::spawn_blocking(move || store.set_agent_profile(&id, &profile)).await?
    }

    pub async fn update_agent_status(&self, agent_id: &str, status: &str) -> Result<()> {
        let store = self.store.clone();
        let id = agent_id.to_string();
        let status = status.to_string();
        let event =
            tokio::task::spawn_blocking(move || store.update_agent_status_with_event(&id, &status))
                .await??;
        let _ = self.events.send(event);
        Ok(())
    }

    pub async fn list_agents(&self, session_id: &str) -> Result<Vec<AgentRecord>> {
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.list_agents(&id)).await?
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn enqueue_merge(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        task_id: Option<String>,
        branch: &str,
        commit: &str,
        base_commit: &str,
        detail: Option<String>,
    ) -> Result<MergeQueueEntry> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let agent_id = agent_id.to_string();
        let agent_name = agent_name.to_string();
        let branch = branch.to_string();
        let commit = commit.to_string();
        let base_commit = base_commit.to_string();
        let (entry, event) = tokio::task::spawn_blocking(move || {
            store.enqueue_merge_with_event(
                &session_id,
                &agent_id,
                &agent_name,
                task_id.as_deref(),
                &branch,
                &commit,
                &base_commit,
                detail.as_deref(),
            )
        })
        .await??;
        let _ = self.events.send(event);
        Ok(entry)
    }

    pub async fn update_merge_queue_entry(
        &self,
        id: &str,
        status: &str,
        detail: Option<String>,
    ) -> Result<Option<MergeQueueEntry>> {
        let store = self.store.clone();
        let id = id.to_string();
        let status = status.to_string();
        let updated = tokio::task::spawn_blocking(move || {
            store.update_merge_queue_entry_with_event(&id, &status, detail.as_deref())
        })
        .await??;
        let entry = updated.map(|(entry, event)| {
            let _ = self.events.send(event);
            entry
        });
        Ok(entry)
    }

    pub async fn list_merge_queue(&self, session_id: &str) -> Result<Vec<MergeQueueEntry>> {
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.list_merge_queue(&id)).await?
    }

    pub async fn get_merge_queue_entry(&self, id: &str) -> Result<Option<MergeQueueEntry>> {
        let store = self.store.clone();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || store.get_merge_queue_entry(&id)).await?
    }

    pub async fn retry_merge_queue_entry(
        &self,
        id: &str,
        commit: &str,
        base_commit: &str,
        detail: Option<String>,
    ) -> Result<Option<MergeQueueEntry>> {
        let store = self.store.clone();
        let id = id.to_string();
        let commit = commit.to_string();
        let base_commit = base_commit.to_string();
        let updated = tokio::task::spawn_blocking(move || {
            store.retry_merge_queue_entry_with_event(&id, &commit, &base_commit, detail.as_deref())
        })
        .await??;
        let entry = updated.map(|(entry, event)| {
            let _ = self.events.send(event);
            entry
        });
        Ok(entry)
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
        let (message, event) = tokio::task::spawn_blocking(move || {
            store.append_message_with_event(
                &session_id,
                &role,
                &content,
                &tool_calls,
                tool_call_id.as_deref(),
            )
        })
        .await??;
        let _ = self.events.send(event);
        Ok(message)
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
        let diff = engine.session_diff(&session.id).await.unwrap();
        assert!(diff.status.is_empty());
        assert!(diff.patch.is_empty());
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
        let diff = engine.session_diff(&session.id).await.unwrap();
        assert!(diff.status.is_empty());
        assert!(diff.patch.contains("+committed"));

        let error = engine
            .commit_session(&session.id, "again")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("no changes"));
        let events = engine.list_events(session.id, 0, 100).await.unwrap();
        assert!(events.iter().any(|event| event.kind == "session.committed"));
    }

    #[tokio::test]
    async fn lands_a_committed_session_into_the_default_branch() {
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
            .create_session(project.id.clone(), "land".into(), None)
            .await
            .unwrap();
        let worktree = session.worktree.clone().unwrap();
        std::fs::write(Path::new(&worktree).join("README.md"), "landed\n").unwrap();
        engine
            .commit_session(&session.id, "session change")
            .await
            .unwrap();

        let landed = engine.land_session(&session.id).await.unwrap();
        assert_eq!(landed.target_branch, project.default_branch);
        assert_eq!(
            std::fs::read_to_string(repo.path().join("README.md")).unwrap(),
            "landed\n"
        );
        let sessions = engine.list_sessions(Some(project.id)).await.unwrap();
        assert_eq!(sessions[0].status, "landed");
        let events = engine.list_events(session.id, 0, 100).await.unwrap();
        assert!(events.iter().any(|event| event.kind == "session.landed"));
    }

    #[tokio::test]
    async fn pushes_a_branch_and_opens_a_pull_request() {
        use std::os::unix::fs::PermissionsExt;

        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let remote = TempDir::new().unwrap();
        init_repo(repo.path());
        let status = Command::new("git")
            .args(["init", "--bare", "-q"])
            .arg(remote.path())
            .status()
            .unwrap();
        assert!(status.success());
        let status = Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["remote", "add", "origin", remote.path().to_str().unwrap()])
            .status()
            .unwrap();
        assert!(status.success());

        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("demo".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session(project.id.clone(), "PR me".into(), None)
            .await
            .unwrap();
        let worktree = session.worktree.clone().unwrap();
        std::fs::write(Path::new(&worktree).join("README.md"), "pr\n").unwrap();
        engine
            .commit_session(&session.id, "session change")
            .await
            .unwrap();

        let gh = data.path().join("fake-gh");
        std::fs::write(
            &gh,
            "#!/bin/sh\nif [ \"$1\" = \"auth\" ]; then exit 0; fi\nif [ \"$1\" = \"pr\" ]; then echo https://example.test/pr/1; exit 0; fi\nexit 0\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&gh).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&gh, permissions).unwrap();

        let result = engine
            .open_pull_request_with_gh(&session.id, gh.to_str().unwrap())
            .await
            .unwrap();
        assert!(result.pushed);
        assert_eq!(result.url.as_deref(), Some("https://example.test/pr/1"));
        let branch = session.branch.clone().unwrap();
        let remote_ref = Command::new("git")
            .arg("--git-dir")
            .arg(remote.path())
            .args(["rev-parse", &format!("refs/heads/{branch}")])
            .output()
            .unwrap();
        assert!(remote_ref.status.success());
        let events = engine.list_events(session.id, 0, 100).await.unwrap();
        assert!(events.iter().any(|event| event.kind == "session.pushed"));
        assert!(events.iter().any(|event| event.kind == "session.pr_opened"));
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
        let queue_entry = engine
            .enqueue_merge(
                &session.id,
                "agent-1",
                "Worker 1",
                Some("T1".into()),
                "verlinpark/task",
                "abc123",
                "def456",
                None,
            )
            .await
            .unwrap();
        engine
            .update_merge_queue_entry(&queue_entry.id, "merging", None)
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
        let events = engine
            .list_events(session.id.clone(), 0, 100)
            .await
            .unwrap();
        assert!(events.iter().any(|event| event.kind == "agent.failed"));
        assert!(events.iter().any(|event| event.kind == "approval.resolved"
            && event.payload["approval_id"] == approval.id));
        let queue = engine.list_merge_queue(&session.id).await.unwrap();
        assert_eq!(queue[0].status, "interrupted");
        assert!(events.iter().any(|event| {
            event.kind == "merge_queue.updated"
                && event.payload["entry"]["id"] == queue_entry.id
                && event.payload["entry"]["status"] == "interrupted"
        }));
    }

    #[tokio::test]
    async fn events_cover_critical_state_transitions() {
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
            .create_session(project.id, "events".into(), None)
            .await
            .unwrap();
        engine
            .update_session_status(&session.id, "running")
            .await
            .unwrap();
        engine.record_usage(&session.id, 10, 5).await.unwrap();
        let agent = engine
            .create_agent(&session.id, "worker", "Worker 1", None, None, "test")
            .await
            .unwrap();
        engine
            .update_agent_status(&agent.id, "running")
            .await
            .unwrap();
        let entry = engine
            .enqueue_merge(
                &session.id,
                &agent.id,
                &agent.name,
                Some("T1".into()),
                "verlinpark/task",
                "abc123",
                "def456",
                None,
            )
            .await
            .unwrap();
        engine
            .update_merge_queue_entry(&entry.id, "merged", Some("ok".into()))
            .await
            .unwrap();

        let events = engine
            .list_events(session.id.clone(), 0, 100)
            .await
            .unwrap();
        let last_status = events
            .iter()
            .rev()
            .find(|event| event.kind == "session.updated")
            .and_then(|event| event.payload["status"].as_str());
        assert_eq!(last_status, Some("running"));
        let usage = events
            .iter()
            .filter(|event| event.kind == "agent.usage")
            .map(|event| {
                event.payload["input_tokens"].as_u64().unwrap_or(0)
                    + event.payload["output_tokens"].as_u64().unwrap_or(0)
            })
            .sum::<u64>();
        assert_eq!(usage, 15);
        assert!(events.iter().any(|event| {
            event.kind == "agent.updated"
                && event.payload["agent_id"] == agent.id
                && event.payload["status"] == "running"
        }));
        let queue_status = events
            .iter()
            .filter(|event| {
                event.kind == "merge_queue.enqueued" || event.kind == "merge_queue.updated"
            })
            .filter_map(|event| {
                Some((
                    event.payload["entry"]["id"].as_str()?.to_string(),
                    event.payload["entry"]["status"].as_str()?.to_string(),
                ))
            })
            .fold(std::collections::HashMap::new(), |mut map, (id, status)| {
                map.insert(id, status);
                map
            });
        assert_eq!(
            queue_status.get(&entry.id).map(String::as_str),
            Some("merged")
        );
    }

    #[tokio::test]
    async fn replay_checks_critical_projections() {
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
            .create_session(project.id, "replay".into(), None)
            .await
            .unwrap();
        engine
            .update_session_status(&session.id, "running")
            .await
            .unwrap();
        engine.record_usage(&session.id, 7, 3).await.unwrap();
        let plan: TeamPlan = serde_json::from_value(serde_json::json!({
            "goal": "replay",
            "departments": [{
                "name": "impl",
                "manager_title": "Implementation",
                "profile": "coding",
                "objective": "build"
            }],
            "tasks": [{
                "id": "T1",
                "department": "impl",
                "title": "Build",
                "kind": "coding",
                "deps": [],
                "acceptance": ["done"]
            }]
        }))
        .unwrap();
        engine.set_team_plan(&session.id, &plan).await.unwrap();
        engine
            .set_todos(
                &session.id,
                vec![TodoItem {
                    title: "Build".into(),
                    status: "in_progress".into(),
                }],
            )
            .await
            .unwrap();
        engine
            .append_message(&session.id, "user", "hello", serde_json::json!([]), None)
            .await
            .unwrap();
        let agent = engine
            .create_agent(&session.id, "worker", "Worker", None, None, "test")
            .await
            .unwrap();
        let entry = engine
            .enqueue_merge(
                &session.id,
                &agent.id,
                &agent.name,
                Some("T1".into()),
                "verlinpark/task",
                "abc123",
                "def456",
                None,
            )
            .await
            .unwrap();
        engine
            .update_merge_queue_entry(&entry.id, "merged", None)
            .await
            .unwrap();

        let report = engine.replay_session(&session.id).await.unwrap();
        assert!(report.consistent, "{:?}", report.mismatches);
        assert!(report.status_matches);
        assert!(report.usage_matches);
        assert!(report.plan_matches);
        assert!(report.todos_matches);
        assert!(report.messages_matches);
        assert!(report.queue_matches);

        // Bypass the event path deliberately: replay must expose the drift.
        engine
            .store()
            .update_session_status(&session.id, "idle")
            .unwrap();
        let report = engine.replay_session(&session.id).await.unwrap();
        assert!(!report.consistent);
        assert!(!report.status_matches);
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
        let replay = engine.replay_session(&session.id).await.unwrap();
        assert!(replay.consistent, "{:?}", replay.mismatches);

        let events = engine.list_events(session.id, 0, 10).await.unwrap();
        assert_eq!(events.len(), 1);
    }
}

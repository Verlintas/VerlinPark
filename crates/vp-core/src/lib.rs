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
    AgentMessage, AgentRecord, Approval, Artifact, ArtifactGcResult, ArtifactPolicy, Event,
    IntegrationPolicy, MailboxGcResult, MailboxPolicy, MergeQueueEntry, Project, PullRequestResult,
    Session, SessionBundle, SessionCommit, SessionDiff, SessionImportResult, SessionLandResult,
    SessionRehydrateResult, SessionReplayReport, ShellPolicy, StoredMessage, TeamPlan, TodoItem,
    UsageBucket, UsageEntry, UsageLedgerResult, UsageTotals,
};
use vp_store::Store;
use vp_store::store::SessionRepair;

use crate::budget::BudgetRegistry;

pub mod agent;
pub mod budget;
pub mod tools;
pub mod worktree;

struct ReplayData {
    session: Session,
    messages: Vec<StoredMessage>,
    plan: Option<String>,
    todos: Vec<TodoItem>,
    agents: Vec<AgentRecord>,
    approvals: Vec<Approval>,
    queue: Vec<MergeQueueEntry>,
    agent_messages: Vec<AgentMessage>,
    artifacts: Vec<Artifact>,
    usage: Vec<UsageEntry>,
    events: Vec<Event>,
}

struct ExpectedArtifact {
    artifact: Artifact,
    full: bool,
    has_storage: bool,
    has_size: bool,
}

#[derive(Clone)]
pub struct Engine {
    store: Store,
    events: broadcast::Sender<Event>,
    approval_waiters: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>,
    git_lock: Arc<Mutex<()>>,
    budget_registry: Arc<BudgetRegistry>,
}

fn add_usage_bucket(buckets: &mut HashMap<String, UsageBucket>, key: &str, entry: &UsageEntry) {
    let bucket = buckets
        .entry(key.to_string())
        .or_insert_with(|| UsageBucket {
            key: key.to_string(),
            ..UsageBucket::default()
        });
    bucket.calls = bucket.calls.saturating_add(1);
    bucket.input_tokens = bucket.input_tokens.saturating_add(entry.input_tokens);
    bucket.output_tokens = bucket.output_tokens.saturating_add(entry.output_tokens);
    bucket.cost_usd += entry.cost_usd;
}

fn sorted_usage_buckets(buckets: HashMap<String, UsageBucket>) -> Vec<UsageBucket> {
    let mut values: Vec<_> = buckets.into_values().collect();
    values.sort_by(|left, right| {
        right
            .cost_usd
            .total_cmp(&left.cost_usd)
            .then_with(|| left.key.cmp(&right.key))
    });
    values
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(data))
}

fn normalize_shell_policy(mut policy: ShellPolicy) -> Result<ShellPolicy> {
    let network = policy.network.trim().to_ascii_lowercase();
    if network != "allow" && network != "deny" {
        anyhow::bail!("shell policy network must be 'allow' or 'deny'");
    }
    if policy.read_deny.len() > 64 {
        anyhow::bail!("shell policy supports at most 64 read-deny paths");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut normalized = Vec::new();
    for raw in policy.read_deny {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        if raw.len() > 4096 {
            anyhow::bail!("shell read-deny path is too long");
        }
        let path = if raw == "~" {
            home.clone()
                .ok_or_else(|| anyhow::anyhow!("HOME is not set; cannot expand ~"))?
        } else if let Some(rest) = raw.strip_prefix("~/") {
            home.clone()
                .ok_or_else(|| anyhow::anyhow!("HOME is not set; cannot expand ~"))?
                .join(rest)
        } else if Path::new(raw).is_absolute() {
            PathBuf::from(raw)
        } else {
            anyhow::bail!("shell read-deny paths must be absolute or start with ~/: {raw}");
        };
        let path = path.canonicalize().unwrap_or(path);
        let path = path.to_string_lossy().to_string();
        if !normalized.contains(&path) {
            normalized.push(path);
        }
    }
    policy.network = network;
    policy.read_deny = normalized;
    Ok(policy)
}

impl Engine {
    pub fn new(store: Store) -> Self {
        let (events, _) = broadcast::channel(2048);
        Self {
            store,
            events,
            approval_waiters: Arc::new(Mutex::new(HashMap::new())),
            git_lock: Arc::new(Mutex::new(())),
            budget_registry: Arc::new(BudgetRegistry::new()),
        }
    }

    pub fn budget_registry(&self) -> &Arc<BudgetRegistry> {
        &self.budget_registry
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

    pub async fn get_shell_policy(&self, project_id: &str) -> Result<ShellPolicy> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking({
            let store = store.clone();
            let project_id = project_id.clone();
            move || store.get_project(&project_id)
        })
        .await??
        .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        tokio::task::spawn_blocking(move || store.get_shell_policy(&project_id)).await?
    }

    pub async fn set_shell_policy(
        &self,
        project_id: &str,
        policy: ShellPolicy,
    ) -> Result<ShellPolicy> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking({
            let store = store.clone();
            let project_id = project_id.clone();
            move || store.get_project(&project_id)
        })
        .await??
        .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        let policy = normalize_shell_policy(policy)?;
        let policy_for_store = policy.clone();
        tokio::task::spawn_blocking(move || store.set_shell_policy(&project_id, &policy_for_store))
            .await??;
        Ok(policy)
    }

    pub async fn get_integration_policy(&self, project_id: &str) -> Result<IntegrationPolicy> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking({
            let store = store.clone();
            let project_id = project_id.clone();
            move || store.get_project(&project_id)
        })
        .await??
        .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        tokio::task::spawn_blocking(move || store.get_integration_policy(&project_id)).await?
    }

    pub async fn set_integration_policy(
        &self,
        project_id: &str,
        policy: IntegrationPolicy,
    ) -> Result<IntegrationPolicy> {
        if policy.max_auto_attempts > 5 {
            anyhow::bail!("max_auto_attempts must be between 0 and 5");
        }
        if let Some(tokens) = policy.max_resolve_tokens
            && !(1_000..=10_000_000).contains(&tokens)
        {
            anyhow::bail!("max_resolve_tokens must be between 1000 and 10000000");
        }
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking({
            let store = store.clone();
            let project_id = project_id.clone();
            move || store.get_project(&project_id)
        })
        .await??
        .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        let for_store = policy.clone();
        tokio::task::spawn_blocking(move || store.set_integration_policy(&project_id, &for_store))
            .await??;
        Ok(policy)
    }

    pub async fn auto_resolve_attempts(&self, entry_id: &str) -> Result<u32> {
        let store = self.store.clone();
        let entry_id = entry_id.to_string();
        tokio::task::spawn_blocking(move || store.auto_resolve_attempts(&entry_id)).await?
    }

    pub async fn bump_auto_resolve_attempts(&self, entry_id: &str) -> Result<u32> {
        let store = self.store.clone();
        let entry_id = entry_id.to_string();
        tokio::task::spawn_blocking(move || store.bump_auto_resolve_attempts(&entry_id)).await?
    }

    pub async fn create_session(
        &self,
        project_id: String,
        goal: String,
        token_budget: Option<u64>,
    ) -> Result<Session> {
        self.create_session_with_limits(project_id, goal, token_budget, None, None)
            .await
    }

    pub async fn create_session_with_limits(
        &self,
        project_id: String,
        goal: String,
        token_budget: Option<u64>,
        cost_budget_usd: Option<f64>,
        time_budget_secs: Option<u64>,
    ) -> Result<Session> {
        if cost_budget_usd.is_some_and(|value| !value.is_finite() || value <= 0.0) {
            anyhow::bail!("cost budget must be a positive finite number");
        }
        if time_budget_secs == Some(0) {
            anyhow::bail!("time budget must be greater than zero");
        }
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
                    cost_budget_usd,
                    time_budget_secs,
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

    async fn reject_imported_session(&self, session_id: &str) -> Result<()> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        if session.status == "imported" {
            anyhow::bail!(
                "imported sessions are read-only archives; create a new session to continue"
            );
        }
        Ok(())
    }

    pub async fn discard_session(&self, session_id: &str) -> Result<()> {
        self.reject_imported_session(session_id).await?;
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
        self.reject_imported_session(session_id).await?;
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
        self.reject_imported_session(session_id).await?;
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
        self.reject_imported_session(session_id).await?;
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

    #[allow(clippy::too_many_arguments)]
    pub async fn record_agent_usage(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        role: &str,
        task_id: Option<String>,
        department: Option<String>,
        model: &str,
        input: u64,
        output: u64,
        estimated: bool,
        cost_usd: f64,
    ) -> Result<UsageEntry> {
        let cost_usd = if cost_usd.is_finite() && cost_usd > 0.0 {
            cost_usd
        } else {
            0.0
        };
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let agent_id = agent_id.to_string();
        let agent_name = agent_name.to_string();
        let role = role.to_string();
        let model = model.to_string();
        let (entry, events) = tokio::task::spawn_blocking(move || {
            store.add_usage_entry_with_events(
                &session_id,
                &agent_id,
                &agent_name,
                &role,
                task_id.as_deref(),
                department.as_deref(),
                &model,
                input,
                output,
                estimated,
                cost_usd,
            )
        })
        .await??;
        for event in events {
            let _ = self.events.send(event);
        }
        Ok(entry)
    }

    pub async fn usage_ledger(&self, session_id: &str) -> Result<UsageLedgerResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let entries = tokio::task::spawn_blocking(move || store.list_usage_entries(&id)).await??;
        let mut totals = UsageTotals {
            calls: entries.len() as u64,
            ..UsageTotals::default()
        };
        let mut by_role = HashMap::new();
        let mut by_model = HashMap::new();
        let mut by_agent = HashMap::new();
        let mut by_task = HashMap::new();
        let mut by_department = HashMap::new();
        for entry in &entries {
            totals.input_tokens = totals.input_tokens.saturating_add(entry.input_tokens);
            totals.output_tokens = totals.output_tokens.saturating_add(entry.output_tokens);
            totals.cost_usd += entry.cost_usd;
            add_usage_bucket(&mut by_role, &entry.role, entry);
            add_usage_bucket(&mut by_model, &entry.model, entry);
            add_usage_bucket(&mut by_agent, &entry.agent_name, entry);
            add_usage_bucket(
                &mut by_task,
                entry.task_id.as_deref().unwrap_or("(unassigned)"),
                entry,
            );
            add_usage_bucket(
                &mut by_department,
                entry.department.as_deref().unwrap_or("(unassigned)"),
                entry,
            );
        }
        totals.by_role = sorted_usage_buckets(by_role);
        totals.by_model = sorted_usage_buckets(by_model);
        totals.by_agent = sorted_usage_buckets(by_agent);
        totals.by_task = sorted_usage_buckets(by_task);
        totals.by_department = sorted_usage_buckets(by_department);
        Ok(UsageLedgerResult {
            session_id: session_id.to_string(),
            entries,
            totals,
        })
    }

    pub async fn usage_spent(
        &self,
        session_id: &str,
        task_id: Option<&str>,
        department: Option<&str>,
    ) -> Result<(u64, f64, u64, f64)> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let task_id = task_id.map(str::to_string);
        let department = department.map(str::to_string);
        tokio::task::spawn_blocking(move || {
            store.usage_spent(&session_id, task_id.as_deref(), department.as_deref())
        })
        .await?
    }

    pub async fn record_cost(&self, session_id: &str, cost_usd: f64) -> Result<()> {
        if !cost_usd.is_finite() || cost_usd <= 0.0 {
            return Ok(());
        }
        let store = self.store.clone();
        let id = session_id.to_string();
        let event =
            tokio::task::spawn_blocking(move || store.add_session_cost_with_event(&id, cost_usd))
                .await??;
        let _ = self.events.send(event);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn send_agent_message(
        &self,
        session_id: &str,
        from_agent_id: &str,
        from_name: &str,
        to_agent_id: &str,
        to_name: &str,
        kind: &str,
        body: &str,
        payload: serde_json::Value,
    ) -> Result<AgentMessage> {
        if kind.trim().is_empty() || kind.len() > 64 {
            anyhow::bail!("message kind must be 1-64 characters");
        }
        if body.len() > 20_000 {
            anyhow::bail!("message body exceeds 20000 bytes");
        }
        if serde_json::to_string(&payload)
            .map(|encoded| encoded.len() > 20_000)
            .unwrap_or(true)
        {
            anyhow::bail!("message payload exceeds 20000 bytes");
        }
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let gc_session_id = session_id.clone();
        let from_agent_id = from_agent_id.to_string();
        let from_name = from_name.to_string();
        let to_agent_id = to_agent_id.to_string();
        let to_name = to_name.to_string();
        let kind = kind.to_string();
        let body = body.to_string();
        let (message, event) = tokio::task::spawn_blocking(move || {
            store.create_agent_message_with_event(
                &session_id,
                &from_agent_id,
                &from_name,
                &to_agent_id,
                &to_name,
                &kind,
                &body,
                &payload,
            )
        })
        .await??;
        let _ = self.events.send(event);
        self.auto_gc_mailbox_for_session(&gc_session_id).await;
        Ok(message)
    }

    pub async fn list_agent_messages(
        &self,
        session_id: &str,
        to_agent_id: Option<String>,
        unread_only: bool,
    ) -> Result<Vec<AgentMessage>> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || {
            store.list_agent_messages(&session_id, to_agent_id.as_deref(), unread_only)
        })
        .await?
    }

    pub async fn mark_agent_message_read(&self, message_id: &str) -> Result<Option<AgentMessage>> {
        let store = self.store.clone();
        let message_id = message_id.to_string();
        let marked =
            tokio::task::spawn_blocking(move || store.mark_agent_message_read(&message_id))
                .await??;
        match marked {
            Some((message, event)) => {
                if let Some(event) = event {
                    let _ = self.events.send(event);
                }
                self.auto_gc_mailbox_for_session(&message.session_id).await;
                Ok(Some(message))
            }
            None => Ok(None),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn publish_artifact(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        task_id: Option<String>,
        name: &str,
        kind: &str,
        mime_type: &str,
        content: &str,
    ) -> Result<Artifact> {
        let name = name.trim();
        if name.is_empty() || name.len() > 200 {
            anyhow::bail!("artifact name must be 1-200 characters");
        }
        if content.len() > 100_000 {
            anyhow::bail!("artifact content exceeds 100000 bytes");
        }
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let agent_id = agent_id.to_string();
        let agent_name = agent_name.to_string();
        let name = name.to_string();
        let kind = kind.to_string();
        let mime_type = mime_type.to_string();
        let content = content.to_string();
        let size_bytes = content.len() as u64;
        let sha256 = sha256_hex(content.as_bytes());
        let (artifact, event) = tokio::task::spawn_blocking(move || {
            store.create_artifact_with_event(
                &session_id,
                &agent_id,
                &agent_name,
                task_id.as_deref(),
                &name,
                &kind,
                &mime_type,
                &content,
                "inline",
                size_bytes,
                &sha256,
                None,
            )
        })
        .await??;
        let _ = self.events.send(event);
        self.auto_gc_restarted_artifact(&artifact.session_id).await;
        Ok(artifact)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn publish_artifact_file(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        task_id: Option<String>,
        name: &str,
        kind: &str,
        mime_type: &str,
        source_path: &Path,
        allowed_root: &Path,
    ) -> Result<Artifact> {
        let root = allowed_root.canonicalize()?;
        let source = source_path.canonicalize()?;
        if !source.starts_with(&root) {
            anyhow::bail!("artifact source is outside the agent worktree");
        }
        let data = tokio::fs::read(&source).await?;
        if data.len() > 5 * 1024 * 1024 {
            anyhow::bail!("binary artifact exceeds 5 MiB");
        }
        let name = name.trim();
        if name.is_empty() || name.len() > 200 {
            anyhow::bail!("artifact name must be 1-200 characters");
        }
        let sha256 = sha256_hex(&data);
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let agent_id = agent_id.to_string();
        let agent_name = agent_name.to_string();
        let name = name.to_string();
        let kind = kind.to_string();
        let mime_type = mime_type.to_string();
        let (artifact, event) = tokio::task::spawn_blocking(move || {
            store.create_artifact_file_with_event(
                &session_id,
                &agent_id,
                &agent_name,
                task_id.as_deref(),
                &name,
                &kind,
                &mime_type,
                &data,
                &sha256,
            )
        })
        .await??;
        let _ = self.events.send(event);
        self.auto_gc_restarted_artifact(&artifact.session_id).await;
        Ok(artifact)
    }

    async fn auto_gc_restarted_artifact(&self, session_id: &str) {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let project_id =
            match tokio::task::spawn_blocking(move || store.get_session(&session_id)).await {
                Ok(Ok(Some(session))) => session.project_id,
                _ => return,
            };
        let policy = match self.get_artifact_policy(&project_id).await {
            Ok(policy) if policy.auto_gc => policy,
            _ => return,
        };
        let _ = self.gc_artifacts(&project_id, &policy).await;
    }

    pub async fn get_artifact_policy(&self, project_id: &str) -> Result<ArtifactPolicy> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking(move || store.get_artifact_policy(&project_id)).await?
    }

    pub async fn set_artifact_policy(
        &self,
        project_id: &str,
        policy: ArtifactPolicy,
    ) -> Result<ArtifactPolicy> {
        if policy.max_bytes.is_some_and(|value| value == 0) {
            anyhow::bail!("max_bytes must be greater than zero");
        }
        if policy.max_age_secs.is_some_and(|value| value == 0) {
            anyhow::bail!("max_age_secs must be greater than zero");
        }
        if policy.max_count.is_some_and(|value| value == 0) {
            anyhow::bail!("max_count must be greater than zero");
        }
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking({
            let store = store.clone();
            let project_id = project_id.clone();
            move || store.get_project(&project_id)
        })
        .await??
        .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        let for_store = policy.clone();
        tokio::task::spawn_blocking(move || store.set_artifact_policy(&project_id, &for_store))
            .await??;
        Ok(policy)
    }

    pub async fn gc_artifacts(
        &self,
        project_id: &str,
        policy: &ArtifactPolicy,
    ) -> Result<ArtifactGcResult> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        let artifacts = tokio::task::spawn_blocking({
            let store = store.clone();
            let project_id = project_id.clone();
            move || store.list_artifacts_for_project(&project_id)
        })
        .await??;
        let now = time::OffsetDateTime::now_utc();
        let mut remove = Vec::new();
        let mut total_bytes: u64 = artifacts.iter().map(|artifact| artifact.size_bytes).sum();
        if let Some(max_age) = policy.max_age_secs {
            for artifact in &artifacts {
                if let Ok(created) = time::OffsetDateTime::parse(
                    &artifact.created_at,
                    &time::format_description::well_known::Rfc3339,
                ) && (now - created).whole_seconds().max(0) as u64 > max_age
                {
                    remove.push(artifact.id.clone());
                }
            }
        }
        if let Some(max_count) = policy.max_count
            && artifacts.len() as u64 > max_count
        {
            let keep = artifacts.len() - max_count as usize;
            remove.extend(
                artifacts
                    .iter()
                    .take(keep)
                    .map(|artifact| artifact.id.clone()),
            );
        }
        if let Some(max_bytes) = policy.max_bytes {
            for artifact in &artifacts {
                if total_bytes <= max_bytes {
                    break;
                }
                if !remove.contains(&artifact.id) {
                    remove.push(artifact.id.clone());
                    total_bytes = total_bytes.saturating_sub(artifact.size_bytes);
                } else {
                    total_bytes = total_bytes.saturating_sub(artifact.size_bytes);
                }
            }
        }
        remove.sort();
        remove.dedup();
        let mut bytes_freed = 0u64;
        for id in &remove {
            let logical_size = artifacts
                .iter()
                .find(|artifact| &artifact.id == id)
                .map(|artifact| artifact.size_bytes)
                .unwrap_or(0);
            let store = self.store.clone();
            let id = id.clone();
            let pruned = tokio::task::spawn_blocking(move || store.delete_artifact_with_event(&id))
                .await??;
            if let Some((event, remove_path)) = pruned {
                let _ = self.events.send(event);
                if let Some(path) = remove_path {
                    match tokio::fs::remove_file(&path).await {
                        Ok(()) => bytes_freed = bytes_freed.saturating_add(logical_size),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => {
                            tracing::warn!(%error, path, "failed to remove artifact blob");
                        }
                    }
                }
            }
        }
        Ok(ArtifactGcResult {
            removed: remove.len(),
            bytes_freed,
        })
    }

    pub async fn list_artifacts(&self, session_id: &str) -> Result<Vec<Artifact>> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.list_artifacts(&session_id)).await?
    }

    pub async fn get_artifact(&self, artifact_id: &str) -> Result<Option<Artifact>> {
        let store = self.store.clone();
        let artifact_id = artifact_id.to_string();
        tokio::task::spawn_blocking(move || store.get_artifact(&artifact_id)).await?
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
        let store = self.store.clone();
        let id = session_id.to_string();
        if let Some(session) = tokio::task::spawn_blocking(move || store.get_session(&id)).await??
            && session.status == "imported"
        {
            return Ok(SessionDiff {
                session_id: session.id,
                branch: session.branch.unwrap_or_default(),
                worktree: session.worktree.unwrap_or_default(),
                base_commit: session.base_commit,
                status: "imported archive · worktree not restored".to_string(),
                patch: String::new(),
            });
        }
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

    async fn load_replay_data(&self, session_id: &str) -> Result<ReplayData> {
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<ReplayData> {
            let session = store
                .get_session(&id)?
                .ok_or_else(|| anyhow::anyhow!("session not found: {id}"))?;
            let messages = store.list_messages(&id)?;
            let plan = store.get_team_plan(&id)?;
            let todos = store.list_todos(&id)?;
            let agents = store.list_agents(&id)?;
            let approvals = store.list_approvals(Some(&id))?;
            let queue = store.list_merge_queue(&id)?;
            let agent_messages = store.list_agent_messages(&id, None, false)?;
            let artifacts = store.list_artifacts(&id)?;
            let usage = store.list_usage_entries(&id)?;
            let events = store.list_all_events(&id)?;
            Ok(ReplayData {
                session,
                messages,
                plan,
                todos,
                agents,
                approvals,
                queue,
                agent_messages,
                artifacts,
                usage,
                events,
            })
        })
        .await?
    }

    fn expected_artifacts(data: &ReplayData) -> Vec<ExpectedArtifact> {
        let mut artifacts = HashMap::<String, ExpectedArtifact>::new();
        for event in &data.events {
            match event.kind.as_str() {
                "artifact.published" => {
                    let Some(id) = event
                        .payload
                        .get("artifact_id")
                        .and_then(serde_json::Value::as_str)
                    else {
                        continue;
                    };
                    let full = event.payload.get("mime_type").is_some()
                        && event.payload.get("sha256").is_some()
                        && event.payload.get("content").is_some();
                    let has_storage = event.payload.get("storage").is_some();
                    let has_size = event.payload.get("size_bytes").is_some();
                    let artifact = Artifact {
                        id: id.to_string(),
                        session_id: data.session.id.clone(),
                        agent_id: event
                            .payload
                            .get("agent_id")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("unknown")
                            .to_string(),
                        agent_name: event
                            .payload
                            .get("agent_name")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("unknown")
                            .to_string(),
                        task_id: event
                            .payload
                            .get("task_id")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string),
                        name: event
                            .payload
                            .get("name")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("unknown")
                            .to_string(),
                        kind: event
                            .payload
                            .get("kind")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("unknown")
                            .to_string(),
                        mime_type: event
                            .payload
                            .get("mime_type")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        content: event
                            .payload
                            .get("content")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        storage: event
                            .payload
                            .get("storage")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("inline")
                            .to_string(),
                        size_bytes: event
                            .payload
                            .get("size_bytes")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0),
                        sha256: event
                            .payload
                            .get("sha256")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        path: event
                            .payload
                            .get("path")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string),
                        data_base64: None,
                        created_at: event.created_at.clone(),
                    };
                    artifacts.insert(
                        id.to_string(),
                        ExpectedArtifact {
                            artifact,
                            full,
                            has_storage,
                            has_size,
                        },
                    );
                }
                "artifact.pruned" => {
                    if let Some(id) = event
                        .payload
                        .get("artifact_id")
                        .and_then(serde_json::Value::as_str)
                    {
                        artifacts.remove(id);
                    }
                }
                _ => {}
            }
        }
        let mut values = artifacts.into_values().collect::<Vec<_>>();
        values.sort_by(|left, right| {
            left.artifact
                .created_at
                .cmp(&right.artifact.created_at)
                .then_with(|| left.artifact.id.cmp(&right.artifact.id))
        });
        values
    }

    fn same_expected_artifact(expected: &ExpectedArtifact, projected: &Artifact) -> bool {
        let full = expected.full;
        let has_storage = expected.has_storage;
        let has_size = expected.has_size;
        let expected = &expected.artifact;
        expected.id == projected.id
            && expected.session_id == projected.session_id
            && expected.agent_id == projected.agent_id
            && expected.agent_name == projected.agent_name
            && expected.name == projected.name
            && expected.kind == projected.kind
            && (!has_storage || expected.storage == projected.storage)
            && (!has_size || expected.size_bytes == projected.size_bytes)
            && (!full
                || (expected.task_id == projected.task_id
                    && expected.mime_type == projected.mime_type
                    && expected.content == projected.content
                    && expected.sha256 == projected.sha256))
    }

    fn expected_agent_messages(data: &ReplayData) -> Vec<AgentMessage> {
        let mut mailbox = HashMap::<String, AgentMessage>::new();
        for event in &data.events {
            match event.kind.as_str() {
                "mailbox.message_sent" => {
                    if let Ok(message) =
                        serde_json::from_value::<AgentMessage>(event.payload.clone())
                    {
                        mailbox.insert(message.id.clone(), message);
                    }
                }
                "mailbox.message_read" => {
                    if let Some(message_id) = event
                        .payload
                        .get("message_id")
                        .and_then(serde_json::Value::as_str)
                        && let Some(message) = mailbox.get_mut(message_id)
                    {
                        message.status = "read".to_string();
                        message.read_at = event
                            .payload
                            .get("read_at")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                            .or_else(|| Some(event.created_at.clone()));
                    }
                }
                "mailbox.pruned" => {
                    if let Some(message_id) = event
                        .payload
                        .get("message_id")
                        .and_then(serde_json::Value::as_str)
                    {
                        mailbox.remove(message_id);
                    }
                }
                _ => {}
            }
        }
        let mut messages = mailbox.into_values().collect::<Vec<_>>();
        messages.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        messages
    }

    fn expected_usage_entries(data: &ReplayData) -> Vec<UsageEntry> {
        data.events
            .iter()
            .filter(|event| event.kind == "agent.usage")
            .map(|event| UsageEntry {
                id: event
                    .payload
                    .get("usage_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("event-{}", event.seq)),
                session_id: data.session.id.clone(),
                agent_id: event
                    .payload
                    .get("agent_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                agent_name: event
                    .payload
                    .get("agent_name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                role: event
                    .payload
                    .get("agent_role")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                task_id: event
                    .payload
                    .get("task_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                department: event
                    .payload
                    .get("department")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                model: event
                    .payload
                    .get("model")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                input_tokens: event
                    .payload
                    .get("input_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                output_tokens: event
                    .payload
                    .get("output_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                estimated: event
                    .payload
                    .get("estimated")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                cost_usd: event
                    .payload
                    .get("cost_usd")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(0.0),
                created_at: event.created_at.clone(),
            })
            .collect()
    }

    fn same_usage(left: &UsageEntry, right: &UsageEntry) -> bool {
        left.id == right.id
            && left.agent_id == right.agent_id
            && left.agent_name == right.agent_name
            && left.role == right.role
            && left.task_id == right.task_id
            && left.department == right.department
            && left.model == right.model
            && left.input_tokens == right.input_tokens
            && left.output_tokens == right.output_tokens
            && left.estimated == right.estimated
            && left.cost_usd.to_bits() == right.cost_usd.to_bits()
    }

    fn replay_report(data: &ReplayData, repaired: bool) -> Result<SessionReplayReport> {
        let mut mismatches = Vec::new();
        let replay_status = data
            .events
            .iter()
            .rev()
            .find(|event| event.kind == "session.updated")
            .and_then(|event| event.payload.get("status"))
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                data.events
                    .iter()
                    .any(|event| event.kind == "session.created")
                    .then_some("draft")
            });
        let status_matches = replay_status == Some(data.session.status.as_str());
        if !status_matches {
            mismatches.push(format!(
                "session status: projection={}, replay={:?}",
                data.session.status, replay_status
            ));
        }

        let (replay_in, replay_out) = data
            .events
            .iter()
            .filter(|event| event.kind == "agent.usage")
            .fold((0u64, 0u64), |(input, output), event| {
                (
                    input.saturating_add(event.payload["input_tokens"].as_u64().unwrap_or(0)),
                    output.saturating_add(event.payload["output_tokens"].as_u64().unwrap_or(0)),
                )
            });
        let usage_matches =
            replay_in == data.session.tokens_in && replay_out == data.session.tokens_out;
        if !usage_matches {
            mismatches.push(format!(
                "usage: projection={}/{}, replay={replay_in}/{replay_out}",
                data.session.tokens_in, data.session.tokens_out
            ));
        }

        let expected_usage = Self::expected_usage_entries(data);
        let has_usage_id = data
            .events
            .iter()
            .any(|event| event.kind == "agent.usage" && event.payload.get("usage_id").is_some());
        let projected_usage = data
            .usage
            .iter()
            .map(|entry| (entry.id.clone(), entry))
            .collect::<HashMap<_, _>>();
        let ledger_matches = if data.usage.is_empty() && !has_usage_id {
            // Legacy sessions predate the ledger; their events have no usage_id
            // and no projection to compare against.
            true
        } else {
            expected_usage.len() == data.usage.len()
                && expected_usage.iter().all(|expected| {
                    projected_usage
                        .get(&expected.id)
                        .is_some_and(|projected| Self::same_usage(projected, expected))
                })
        };
        if !ledger_matches {
            mismatches.push(format!(
                "usage ledger: projection={}, replay={}",
                data.usage.len(),
                expected_usage.len()
            ));
        }

        let expected_mailbox = Self::expected_agent_messages(data);
        let expected_mailbox_values = expected_mailbox
            .iter()
            .filter_map(|message| Some((message.id.clone(), serde_json::to_value(message).ok()?)))
            .collect::<HashMap<_, _>>();
        let projected_mailbox_values = data
            .agent_messages
            .iter()
            .filter_map(|message| Some((message.id.clone(), serde_json::to_value(message).ok()?)))
            .collect::<HashMap<_, _>>();
        let mailbox_matches = expected_mailbox_values == projected_mailbox_values;
        if !mailbox_matches {
            mismatches.push(format!(
                "mailbox: projection={}, replay={}",
                projected_mailbox_values.len(),
                expected_mailbox_values.len()
            ));
        }

        let expected_artifacts = Self::expected_artifacts(data);
        let projected_artifacts = data
            .artifacts
            .iter()
            .map(|artifact| (artifact.id.clone(), artifact))
            .collect::<HashMap<_, _>>();
        let artifacts_matches = expected_artifacts.len() == data.artifacts.len()
            && expected_artifacts.iter().all(|expected| {
                projected_artifacts
                    .get(&expected.artifact.id)
                    .is_some_and(|projected| Self::same_expected_artifact(expected, projected))
            });
        if !artifacts_matches {
            mismatches.push(format!(
                "artifacts: projection={}, replay={}",
                data.artifacts.len(),
                expected_artifacts.len()
            ));
        }

        let replay_plan = data
            .events
            .iter()
            .rev()
            .find(|event| event.kind == "team_plan.submitted")
            .map(|event| event.payload.clone());
        let projected_plan = data
            .plan
            .as_deref()
            .and_then(|plan| serde_json::from_str::<serde_json::Value>(plan).ok());
        let plan_matches = replay_plan == projected_plan;
        if !plan_matches {
            mismatches.push("team plan projection differs from event stream".to_string());
        }

        let replay_todos = data
            .events
            .iter()
            .rev()
            .find(|event| event.kind == "todo.updated")
            .map(|event| event.payload["items"].clone())
            .unwrap_or_else(|| serde_json::json!([]));
        let projected_todos = serde_json::to_value(&data.todos)?;
        let todos_matches = replay_todos == projected_todos;
        if !todos_matches {
            mismatches.push("todo projection differs from event stream".to_string());
        }

        let replay_messages = data
            .events
            .iter()
            .filter(|event| event.kind == "message.created")
            .filter_map(|event| {
                let message =
                    serde_json::from_value::<StoredMessage>(event.payload.clone()).ok()?;
                Some((message.id.clone(), serde_json::to_value(message).ok()?))
            })
            .collect::<HashMap<_, _>>();
        let projected_messages = data
            .messages
            .iter()
            .filter_map(|message| Some((message.id.clone(), serde_json::to_value(message).ok()?)))
            .collect::<HashMap<_, _>>();
        let messages_matches = replay_messages == projected_messages;
        if !messages_matches {
            mismatches.push(format!(
                "messages: projection={}, replay={}",
                projected_messages.len(),
                replay_messages.len()
            ));
        }

        let replay_agents = data
            .events
            .iter()
            .filter_map(|event| {
                let agent = if event.kind == "agent.snapshot" {
                    serde_json::from_value::<AgentRecord>(event.payload.clone()).ok()?
                } else if event.kind == "agent.updated" {
                    serde_json::from_value::<AgentRecord>(event.payload.get("agent")?.clone())
                        .ok()?
                } else {
                    return None;
                };
                Some((agent.id.clone(), serde_json::to_value(agent).ok()?))
            })
            .collect::<HashMap<_, _>>();
        let projected_agents = data
            .agents
            .iter()
            .filter_map(|agent| Some((agent.id.clone(), serde_json::to_value(agent).ok()?)))
            .collect::<HashMap<_, _>>();
        let agents_matches = replay_agents == projected_agents;
        if !agents_matches {
            mismatches.push(format!(
                "agents: projection={}, replay={}",
                projected_agents.len(),
                replay_agents.len()
            ));
        }

        let replay_queue = data
            .events
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
        let projected_queue = data
            .queue
            .iter()
            .map(|entry| (entry.id.clone(), entry.status.clone()))
            .collect::<HashMap<_, _>>();
        let queue_matches = replay_queue == projected_queue;
        if !queue_matches {
            mismatches.push("merge queue projection differs from event stream".to_string());
        }

        let consistent = status_matches
            && usage_matches
            && ledger_matches
            && mailbox_matches
            && artifacts_matches
            && plan_matches
            && todos_matches
            && messages_matches
            && agents_matches
            && queue_matches;
        Ok(SessionReplayReport {
            session_id: data.session.id.clone(),
            event_count: data.events.len(),
            status_matches,
            usage_matches,
            plan_matches,
            todos_matches,
            messages_matches,
            agents_matches,
            queue_matches,
            mailbox_matches,
            artifacts_matches,
            ledger_matches,
            consistent,
            repaired,
            rebuilt: false,
            mismatches,
        })
    }

    fn build_session_repair(data: &ReplayData) -> Result<SessionRepair> {
        let status = data
            .events
            .iter()
            .rev()
            .find(|event| event.kind == "session.updated")
            .and_then(|event| event.payload.get("status"))
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                data.events
                    .iter()
                    .any(|event| event.kind == "session.created")
                    .then_some("draft")
            })
            .unwrap_or(data.session.status.as_str())
            .to_string();
        let (tokens_in, tokens_out) = data
            .events
            .iter()
            .filter(|event| event.kind == "agent.usage")
            .fold((0u64, 0u64), |(input, output), event| {
                (
                    input.saturating_add(event.payload["input_tokens"].as_u64().unwrap_or(0)),
                    output.saturating_add(event.payload["output_tokens"].as_u64().unwrap_or(0)),
                )
            });

        let plan_event = data
            .events
            .iter()
            .rev()
            .find(|event| event.kind == "team_plan.submitted");
        let replace_plan = plan_event.is_some() || data.plan.is_none();
        let plan = plan_event
            .map(|event| serde_json::to_string(&event.payload))
            .transpose()?
            .or_else(|| data.plan.clone());

        let todos = data
            .events
            .iter()
            .rev()
            .find(|event| event.kind == "todo.updated")
            .map(|event| serde_json::from_value::<Vec<TodoItem>>(event.payload["items"].clone()))
            .transpose()?
            .or_else(|| data.todos.is_empty().then(Vec::new));

        let messages = {
            let mut event_messages = Vec::new();
            let mut valid = true;
            for event in data
                .events
                .iter()
                .filter(|event| event.kind == "message.created")
            {
                match serde_json::from_value::<StoredMessage>(event.payload.clone()) {
                    Ok(message) => event_messages.push(message),
                    Err(_) => {
                        valid = false;
                        break;
                    }
                }
            }
            let projected_ids = data
                .messages
                .iter()
                .map(|message| message.id.as_str())
                .collect::<std::collections::HashSet<_>>();
            let event_ids = event_messages
                .iter()
                .map(|message| message.id.as_str())
                .collect::<std::collections::HashSet<_>>();
            (valid && projected_ids.is_subset(&event_ids)).then_some(event_messages)
        };

        let agents = {
            let mut event_agents = HashMap::new();
            let mut valid = true;
            for event in data
                .events
                .iter()
                .filter(|event| event.kind == "agent.snapshot" || event.kind == "agent.updated")
            {
                let agent = if event.kind == "agent.snapshot" {
                    serde_json::from_value::<AgentRecord>(event.payload.clone())
                } else {
                    serde_json::from_value::<AgentRecord>(event.payload["agent"].clone())
                };
                match agent {
                    Ok(agent) => {
                        event_agents.insert(agent.id.clone(), agent);
                    }
                    Err(_) => {
                        valid = false;
                        break;
                    }
                }
            }
            let projected_ids = data
                .agents
                .iter()
                .map(|agent| agent.id.as_str())
                .collect::<std::collections::HashSet<_>>();
            let event_ids = event_agents
                .keys()
                .map(String::as_str)
                .collect::<std::collections::HashSet<_>>();
            if valid && projected_ids.is_subset(&event_ids) {
                let mut agents = event_agents.into_values().collect::<Vec<_>>();
                agents.sort_by(|left, right| {
                    left.created_at
                        .cmp(&right.created_at)
                        .then_with(|| left.id.cmp(&right.id))
                });
                Some(agents)
            } else {
                None
            }
        };

        let queue = {
            let mut entries = HashMap::new();
            let mut valid = true;
            for event in data.events.iter().filter(|event| {
                event.kind == "merge_queue.enqueued" || event.kind == "merge_queue.updated"
            }) {
                match serde_json::from_value::<MergeQueueEntry>(event.payload["entry"].clone()) {
                    Ok(entry) => {
                        entries.insert(entry.id.clone(), entry);
                    }
                    Err(_) => {
                        valid = false;
                        break;
                    }
                }
            }
            if valid && (!entries.is_empty() || data.queue.is_empty()) {
                let mut entries = entries.into_values().collect::<Vec<_>>();
                entries.sort_by(|left, right| {
                    left.created_at
                        .cmp(&right.created_at)
                        .then_with(|| left.id.cmp(&right.id))
                });
                Some(entries)
            } else {
                None
            }
        };

        let expected_artifacts = Self::expected_artifacts(data);
        let all_full = expected_artifacts.iter().all(|expected| expected.full);
        let mut artifact_warnings = Vec::new();
        let artifacts = if all_full {
            Some(
                expected_artifacts
                    .iter()
                    .map(|expected| {
                        let artifact = expected.artifact.clone();
                        if artifact.storage == "file"
                            && artifact
                                .path
                                .as_deref()
                                .is_some_and(|path| !Path::new(path).is_file())
                        {
                            artifact_warnings.push(format!(
                                "artifact {} blob is missing at {}",
                                artifact.id,
                                artifact.path.as_deref().unwrap_or("(no path)")
                            ));
                        }
                        artifact
                    })
                    .collect(),
            )
        } else {
            artifact_warnings.extend(
                expected_artifacts
                    .iter()
                    .filter(|expected| !expected.full)
                    .map(|expected| {
                        format!(
                            "artifact {} predates full publish metadata and was not rebuilt",
                            expected.artifact.id
                        )
                    }),
            );
            None
        };
        Ok(SessionRepair {
            status,
            tokens_in,
            tokens_out,
            replace_plan,
            plan,
            todos,
            messages,
            agent_messages: Some(Self::expected_agent_messages(data)),
            agents,
            queue,
            artifacts,
            artifact_warnings,
        })
    }

    pub async fn rehydrate_session(
        &self,
        session_id: &str,
        worktree: Option<String>,
    ) -> Result<SessionRehydrateResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        if session.status != "imported" {
            anyhow::bail!("only imported archive sessions can be rehydrated");
        }
        let store = self.store.clone();
        let project_id = session.project_id.clone();
        let project = tokio::task::spawn_blocking(move || store.get_project(&project_id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("project not found"))?;
        let project_root = PathBuf::from(&project.root);
        if !project_root.is_dir() {
            anyhow::bail!("project root does not exist: {}", project.root);
        }
        let branch = session
            .branch
            .clone()
            .unwrap_or_else(|| format!("verlinpark/{session_id}"));
        let base_commit = session.base_commit.clone().unwrap_or_default();
        let target = match worktree {
            Some(path) => PathBuf::from(path),
            None => self
                .store
                .data_dir()
                .join("worktrees")
                .join(&project.id)
                .join(&session.id)
                .join("leader"),
        };
        let canonical_root = project_root.canonicalize()?;
        let target_abs = if target.is_absolute() {
            target
        } else {
            std::env::current_dir()?.join(target)
        };
        let target_for_check = target_abs.clone();
        if target_abs.starts_with(&canonical_root) {
            anyhow::bail!("worktree path must be outside the project root");
        }
        let git_lock = self.git_lock.clone();
        let branch_for_worktree = branch.clone();
        let base_for_worktree = base_commit.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = git_lock
                .lock()
                .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
            worktree::rehydrate(
                &project_root,
                &target_for_check,
                &branch_for_worktree,
                &base_for_worktree,
            )
        })
        .await??;
        let worktree_path = target_abs.clone();
        let commit =
            tokio::task::spawn_blocking(move || worktree::head_commit(&worktree_path)).await??;
        let store = self.store.clone();
        let id = session_id.to_string();
        let worktree_string = target_abs.to_string_lossy().to_string();
        let branch_for_store = branch.clone();
        let events = tokio::task::spawn_blocking(move || {
            store.rehydrate_session_with_events(&id, &worktree_string, &branch_for_store)
        })
        .await??;
        for event in events {
            let _ = self.events.send(event);
        }

        let agents = self.list_agents(&session.id).await?;
        let data_dir = self.store.data_dir().to_path_buf();
        let project_root_for_agents = PathBuf::from(&project.root);
        let canonical_root_for_agents = project_root_for_agents.canonicalize()?;
        let session_worktree = target_abs.clone();
        let session_branch = branch.clone();
        let session_id_for_agents = session.id.clone();
        let project_id_for_agents = project.id.clone();
        let mut branch_paths: HashMap<String, PathBuf> = HashMap::new();
        let mut agents_remapped = 0usize;
        let mut warnings = Vec::new();

        for agent in agents {
            let desired = if agent.role == "leader" || agent.role == "manager" {
                Some((session_worktree.clone(), session_branch.clone()))
            } else {
                let Some(agent_branch) = agent.branch.clone() else {
                    warnings.push(format!(
                        "{} has no branch; task worktree not remapped",
                        agent.name
                    ));
                    continue;
                };
                if agent_branch == session_branch {
                    Some((session_worktree.clone(), session_branch.clone()))
                } else if let Some(path) = branch_paths.get(&agent_branch) {
                    Some((path.clone(), agent_branch))
                } else {
                    let target = data_dir
                        .join("worktrees")
                        .join(&project_id_for_agents)
                        .join(&session_id_for_agents)
                        .join("agents")
                        .join(&agent.id);
                    if target.starts_with(&canonical_root_for_agents) {
                        warnings.push(format!(
                            "{} task worktree target is inside the project root; skipped",
                            agent.name
                        ));
                        continue;
                    }
                    let root = project_root_for_agents.clone();
                    let branch_for_worktree = agent_branch.clone();
                    let target_for_worktree = target.clone();
                    let git_lock = self.git_lock.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        let _guard = git_lock
                            .lock()
                            .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
                        if !worktree::branch_exists(&root, &branch_for_worktree)? {
                            anyhow::bail!("branch does not exist: {branch_for_worktree}");
                        }
                        worktree::rehydrate(&root, &target_for_worktree, &branch_for_worktree, "")
                    })
                    .await?;
                    match result {
                        Ok(()) => {
                            branch_paths.insert(agent_branch.clone(), target.clone());
                            Some((target, agent_branch))
                        }
                        Err(error) => {
                            warnings.push(format!(
                                "{} task worktree not remapped: {error:#}",
                                agent.name
                            ));
                            None
                        }
                    }
                }
            };
            if let Some((worktree_path, branch_name)) = desired {
                self.set_agent_worktree(&agent.id, &worktree_path.to_string_lossy(), &branch_name)
                    .await?;
                agents_remapped += 1;
            }
        }

        Ok(SessionRehydrateResult {
            session_id: session.id,
            worktree: target_abs.to_string_lossy().to_string(),
            branch,
            commit,
            status: "idle".to_string(),
            agents_remapped,
            warnings,
        })
    }

    pub async fn import_session(&self, bundle: SessionBundle) -> Result<SessionImportResult> {
        let root = PathBuf::from(&bundle.project.root);
        if !root.is_dir() {
            anyhow::bail!(
                "bundle project root does not exist locally: {}",
                bundle.project.root
            );
        }
        if bundle.session.project_id != bundle.project.id {
            anyhow::bail!("bundle session/project ids do not match");
        }
        let project_id = bundle.project.id.clone();
        let session_id = bundle.session.id.clone();
        let event_count = bundle.events.len() + 2;
        let store = self.store.clone();
        let event =
            tokio::task::spawn_blocking(move || store.import_session_bundle(&bundle)).await??;
        let _ = self.events.send(event);
        Ok(SessionImportResult {
            project_id,
            session_id,
            events: event_count,
            status: "imported".to_string(),
        })
    }

    pub async fn export_session(&self, session_id: &str) -> Result<SessionBundle> {
        let data = self.load_replay_data(session_id).await?;
        let store = self.store.clone();
        let project_id = data.session.project_id.clone();
        let project_store = store.clone();
        let project = tokio::task::spawn_blocking(move || project_store.get_project(&project_id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("project not found"))?;
        let session_id = data.session.id.clone();
        let message_store = store.clone();
        let messages_session = session_id.clone();
        let agent_messages = tokio::task::spawn_blocking(move || {
            message_store.list_agent_messages(&messages_session, None, false)
        })
        .await??;
        let artifact_store = store.clone();
        let artifacts_session = session_id.clone();
        let mut artifacts =
            tokio::task::spawn_blocking(move || artifact_store.list_artifacts(&artifacts_session))
                .await??;
        let usage_store = store;
        let usage_session = session_id;
        let usage =
            tokio::task::spawn_blocking(move || usage_store.list_usage_entries(&usage_session))
                .await??;
        for artifact in &mut artifacts {
            if artifact.storage == "file"
                && let Some(path) = &artifact.path
                && let Ok(data) = tokio::fs::read(path).await
            {
                use base64::Engine as _;
                artifact.data_base64 = Some(base64::engine::general_purpose::STANDARD.encode(data));
            }
        }
        let team_plan = data
            .plan
            .as_deref()
            .and_then(|plan| serde_json::from_str::<TeamPlan>(plan).ok());
        Ok(SessionBundle {
            version: 1,
            exported_at: vp_protocol::now_rfc3339(),
            project,
            session: data.session,
            events: data.events,
            agents: data.agents,
            messages: data.messages,
            team_plan,
            todos: data.todos,
            approvals: data.approvals,
            merge_queue: data.queue,
            agent_messages,
            artifacts,
            usage,
        })
    }

    pub async fn rebuild_session(&self, session_id: &str) -> Result<SessionReplayReport> {
        let data = self.load_replay_data(session_id).await?;
        let repair = Self::build_session_repair(&data)?;
        for warning in &repair.artifact_warnings {
            tracing::warn!(session_id, %warning, "artifact replay warning");
        }
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.repair_session_projections(&id, &repair))
            .await??;
        let expected_usage = Self::expected_usage_entries(&data);
        let usage_store = self.store.clone();
        let usage_id = session_id.to_string();
        tokio::task::spawn_blocking(move || {
            usage_store.replace_usage_ledger(&usage_id, &expected_usage)
        })
        .await??;
        self.append_event(
            session_id,
            "session.rebuilt",
            serde_json::json!({"event_count": data.events.len()}),
        )
        .await?;
        let data = self.load_replay_data(session_id).await?;
        let mut report = Self::replay_report(&data, true)?;
        report.rebuilt = true;
        Ok(report)
    }

    pub async fn replay_session(&self, session_id: &str) -> Result<SessionReplayReport> {
        let data = self.load_replay_data(session_id).await?;
        Self::replay_report(&data, false)
    }

    pub async fn repair_session(&self, session_id: &str) -> Result<SessionReplayReport> {
        let data = self.load_replay_data(session_id).await?;
        let report = Self::replay_report(&data, false)?;
        if report.consistent {
            return Ok(report);
        }
        let repair = Self::build_session_repair(&data)?;
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.repair_session_projections(&id, &repair))
            .await??;
        let expected_usage = Self::expected_usage_entries(&data);
        let usage_store = self.store.clone();
        let usage_id = session_id.to_string();
        tokio::task::spawn_blocking(move || {
            usage_store.replace_usage_ledger(&usage_id, &expected_usage)
        })
        .await??;
        self.append_event(
            session_id,
            "session.repaired",
            serde_json::json!({
                "mismatches": report.mismatches,
            }),
        )
        .await?;
        let data = self.load_replay_data(session_id).await?;
        Self::replay_report(&data, true)
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
        let (agent, event) = tokio::task::spawn_blocking(move || {
            store.create_agent_with_snapshot(
                &session_id,
                &role,
                &name,
                title.as_deref(),
                parent_id.as_deref(),
                &model,
            )
        })
        .await??;
        let _ = self.events.send(event);
        Ok(agent)
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
        let updated = tokio::task::spawn_blocking(move || {
            store.set_agent_worktree_with_snapshot(&id, &worktree, &branch)
        })
        .await??;
        if let Some((_, event)) = updated {
            let _ = self.events.send(event);
        }
        Ok(())
    }

    pub async fn set_agent_profile(&self, agent_id: &str, profile: &str) -> Result<()> {
        let store = self.store.clone();
        let id = agent_id.to_string();
        let profile = profile.to_string();
        let updated = tokio::task::spawn_blocking(move || {
            store.set_agent_profile_with_snapshot(&id, &profile)
        })
        .await??;
        if let Some((_, event)) = updated {
            let _ = self.events.send(event);
        }
        Ok(())
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
        let (agent, event) = tokio::task::spawn_blocking(move || {
            store.create_leader_agent_with_snapshot(&session_id, &model)
        })
        .await??;
        let _ = self.events.send(event);
        Ok(agent.id)
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

    pub async fn get_mailbox_policy(&self, project_id: &str) -> Result<MailboxPolicy> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking(move || store.get_mailbox_policy(&project_id)).await?
    }

    pub async fn set_mailbox_policy(
        &self,
        project_id: &str,
        policy: MailboxPolicy,
    ) -> Result<MailboxPolicy> {
        if policy.max_age_secs.is_some_and(|value| value == 0) {
            anyhow::bail!("max_age_secs must be greater than zero");
        }
        if policy.max_count.is_some_and(|value| value == 0) {
            anyhow::bail!("max_count must be greater than zero");
        }
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking({
            let store = store.clone();
            let project_id = project_id.clone();
            move || store.get_project(&project_id)
        })
        .await??
        .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        let for_store = policy.clone();
        tokio::task::spawn_blocking(move || store.set_mailbox_policy(&project_id, &for_store))
            .await??;
        Ok(policy)
    }

    async fn auto_gc_mailbox_for_session(&self, session_id: &str) {
        let store = self.store.clone();
        let id = session_id.to_string();
        let project_id = match tokio::task::spawn_blocking(move || store.get_session(&id)).await {
            Ok(Ok(Some(session))) => session.project_id,
            Ok(Ok(None)) | Err(_) | Ok(Err(_)) => return,
        };
        let policy = match self.get_mailbox_policy(&project_id).await {
            Ok(policy) if policy.auto_gc => policy,
            _ => return,
        };
        if let Err(error) = self.gc_mailbox(&project_id, &policy).await {
            tracing::warn!(%error, project_id, "automatic mailbox GC failed");
        }
    }

    pub async fn gc_mailbox(
        &self,
        project_id: &str,
        policy: &MailboxPolicy,
    ) -> Result<MailboxGcResult> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        let messages = tokio::task::spawn_blocking({
            let store = store.clone();
            let project_id = project_id.clone();
            move || store.list_agent_messages_for_project(&project_id)
        })
        .await??;
        let now = time::OffsetDateTime::now_utc();
        let mut remove = Vec::new();
        if let Some(max_age) = policy.max_age_secs {
            for message in &messages {
                if let Ok(created) = time::OffsetDateTime::parse(
                    &message.created_at,
                    &time::format_description::well_known::Rfc3339,
                ) && (now - created).whole_seconds().max(0) as u64 > max_age
                {
                    remove.push(message.id.clone());
                }
            }
        }
        if let Some(max_count) = policy.max_count
            && messages.len() as u64 > max_count
        {
            let keep = messages.len() - max_count as usize;
            remove.extend(messages.iter().take(keep).map(|message| message.id.clone()));
        }
        remove.sort();
        remove.dedup();
        let mut removed = 0usize;
        for id in remove {
            let Some(message) = messages.iter().find(|message| message.id == id) else {
                continue;
            };
            if !policy.delete_unread && message.status == "unread" {
                continue;
            }
            let store = self.store.clone();
            let event =
                tokio::task::spawn_blocking(move || store.delete_agent_message_with_event(&id))
                    .await??;
            if let Some(event) = event {
                let _ = self.events.send(event);
                removed += 1;
            }
        }
        Ok(MailboxGcResult { removed })
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
    async fn shell_policy_round_trips_and_validates() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("policy".into()),
            )
            .await
            .unwrap();
        assert_eq!(
            engine.get_shell_policy(&project.id).await.unwrap(),
            ShellPolicy::default()
        );
        assert!(engine.get_shell_policy("missing-project").await.is_err());
        let deny_path = data.path().join("secret.txt");
        let saved = engine
            .set_shell_policy(
                &project.id,
                ShellPolicy {
                    network: "deny".into(),
                    read_deny: vec![deny_path.to_string_lossy().to_string()],
                },
            )
            .await
            .unwrap();
        assert_eq!(saved.network, "deny");
        assert_eq!(engine.get_shell_policy(&project.id).await.unwrap(), saved);
        assert!(
            engine
                .set_shell_policy(
                    &project.id,
                    ShellPolicy {
                        network: "sometimes".into(),
                        read_deny: Vec::new(),
                    },
                )
                .await
                .is_err()
        );
        assert!(
            engine
                .set_shell_policy(
                    &project.id,
                    ShellPolicy {
                        network: "allow".into(),
                        read_deny: vec!["relative/path".into()],
                    },
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn session_budgets_round_trip() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("budgets".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session_with_limits(
                project.id.clone(),
                "budgeted".into(),
                Some(1_000),
                Some(2.5),
                Some(120),
            )
            .await
            .unwrap();
        assert_eq!(session.token_budget, Some(1_000));
        assert_eq!(session.cost_budget_usd, Some(2.5));
        assert_eq!(session.time_budget_secs, Some(120));
        assert_eq!(session.cost_spent_usd, 0.0);
        let stored = engine
            .list_sessions(Some(project.id.clone()))
            .await
            .unwrap()
            .into_iter()
            .find(|candidate| candidate.id == session.id)
            .unwrap();
        assert_eq!(stored.cost_budget_usd, Some(2.5));
        assert_eq!(stored.time_budget_secs, Some(120));
        engine.record_cost(&session.id, 1.25).await.unwrap();
        let stored = engine
            .list_sessions(Some(project.id.clone()))
            .await
            .unwrap()
            .into_iter()
            .find(|candidate| candidate.id == session.id)
            .unwrap();
        assert_eq!(stored.cost_spent_usd, 1.25);
        assert!(
            engine
                .create_session_with_limits(
                    project.id.clone(),
                    "bad cost".into(),
                    None,
                    Some(0.0),
                    None,
                )
                .await
                .is_err()
        );
        assert!(
            engine
                .create_session_with_limits(project.id, "bad time".into(), None, None, Some(0),)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn binary_artifacts_and_gc_retention() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("artifacts".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session(project.id.clone(), "artifacts".into(), None)
            .await
            .unwrap();
        let worktree = PathBuf::from(session.worktree.clone().unwrap());
        std::fs::write(worktree.join("binary.bin"), b"\x00\x01\x02").unwrap();
        let first = engine
            .publish_artifact_file(
                &session.id,
                "agent-a",
                "Worker",
                Some("T1".into()),
                "binary.bin",
                "binary",
                "application/octet-stream",
                &worktree.join("binary.bin"),
                &worktree,
            )
            .await
            .unwrap();
        assert_eq!(first.storage, "file");
        assert_eq!(first.size_bytes, 3);
        assert!(!first.sha256.is_empty());
        assert!(std::path::Path::new(first.path.as_ref().unwrap()).is_file());

        std::fs::write(worktree.join("second.bin"), b"second").unwrap();
        let second = engine
            .publish_artifact_file(
                &session.id,
                "agent-a",
                "Worker",
                None,
                "second.bin",
                "binary",
                "application/octet-stream",
                &worktree.join("second.bin"),
                &worktree,
            )
            .await
            .unwrap();
        let policy = engine
            .set_artifact_policy(
                &project.id,
                ArtifactPolicy {
                    auto_gc: false,
                    max_bytes: None,
                    max_age_secs: None,
                    max_count: Some(1),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            engine.get_artifact_policy(&project.id).await.unwrap(),
            policy
        );
        assert!(
            engine
                .set_artifact_policy(
                    &project.id,
                    ArtifactPolicy {
                        auto_gc: false,
                        max_bytes: None,
                        max_age_secs: None,
                        max_count: Some(0),
                    },
                )
                .await
                .is_err()
        );
        let gc = engine.gc_artifacts(&project.id, &policy).await.unwrap();
        assert_eq!(gc.removed, 1);
        let remaining = engine.list_artifacts(&session.id).await.unwrap();
        assert_eq!(remaining.len(), 1);
        let kept = &remaining[0];
        assert!(kept.id == first.id || kept.id == second.id);
        assert!(
            std::path::Path::new(first.path.as_ref().unwrap()).exists()
                || std::path::Path::new(second.path.as_ref().unwrap()).exists()
        );
        let pruned_id = if kept.id == first.id {
            &second.id
        } else {
            &first.id
        };
        let events = engine
            .list_events(session.id.clone(), 0, 200)
            .await
            .unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "artifact.pruned" && event.payload["artifact_id"] == *pruned_id
        }));
    }

    #[tokio::test]
    async fn artifact_blobs_dedupe_and_keep_a_reference_count() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("blobs".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session(project.id.clone(), "blobs".into(), None)
            .await
            .unwrap();
        let worktree = PathBuf::from(session.worktree.clone().unwrap());
        std::fs::write(worktree.join("same.bin"), b"same bytes").unwrap();
        let first = engine
            .publish_artifact_file(
                &session.id,
                "agent-a",
                "Worker",
                Some("T1".into()),
                "first.bin",
                "binary",
                "application/octet-stream",
                &worktree.join("same.bin"),
                &worktree,
            )
            .await
            .unwrap();
        let second = engine
            .publish_artifact_file(
                &session.id,
                "agent-a",
                "Worker",
                Some("T1".into()),
                "second.bin",
                "binary",
                "application/octet-stream",
                &worktree.join("same.bin"),
                &worktree,
            )
            .await
            .unwrap();
        assert_eq!(first.path, second.path);
        assert_eq!(first.sha256, second.sha256);
        let blob = std::path::PathBuf::from(first.path.as_ref().unwrap());
        assert!(blob.is_file());
        assert_eq!(blob.file_name().unwrap().to_string_lossy(), first.sha256);

        let policy = ArtifactPolicy {
            auto_gc: false,
            max_bytes: None,
            max_age_secs: None,
            max_count: Some(1),
        };
        let gc = engine.gc_artifacts(&project.id, &policy).await.unwrap();
        assert_eq!(gc.removed, 1);
        assert_eq!(gc.bytes_freed, 0, "a shared blob must not be freed early");
        assert!(blob.is_file());
        let remaining = engine.list_artifacts(&session.id).await.unwrap();
        assert_eq!(remaining.len(), 1);
        let (event, remove_path) = engine
            .store()
            .delete_artifact_with_event(&remaining[0].id)
            .unwrap()
            .unwrap();
        assert_eq!(event.kind, "artifact.pruned");
        assert_eq!(
            remove_path.as_deref(),
            Some(blob.to_string_lossy().as_ref())
        );
        std::fs::remove_file(&blob).unwrap();
        assert!(!blob.exists());
    }

    #[tokio::test]
    async fn bundle_import_rejects_artifact_sha_mismatch() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("bundle-sha".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session(project.id, "bundle-sha".into(), None)
            .await
            .unwrap();
        let worktree = PathBuf::from(session.worktree.clone().unwrap());
        std::fs::write(worktree.join("payload.bin"), b"payload").unwrap();
        engine
            .publish_artifact_file(
                &session.id,
                "agent-a",
                "Worker",
                Some("T1".into()),
                "payload.bin",
                "binary",
                "application/octet-stream",
                &worktree.join("payload.bin"),
                &worktree,
            )
            .await
            .unwrap();
        let mut bundle = engine.export_session(&session.id).await.unwrap();
        assert_eq!(bundle.artifacts.len(), 1);
        bundle.artifacts[0].sha256 = "deadbeef".to_string();
        let import_data = TempDir::new().unwrap();
        let imported_engine = Engine::new(Store::open(import_data.path()).unwrap());
        let error = imported_engine.import_session(bundle).await.unwrap_err();
        assert!(error.to_string().contains("sha256"), "{error}");
    }

    #[tokio::test]
    async fn mailbox_retention_protects_unread_messages() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("mailbox".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session(project.id.clone(), "mailbox".into(), None)
            .await
            .unwrap();
        for index in 0..3 {
            engine
                .send_agent_message(
                    &session.id,
                    "sender",
                    "Sender",
                    "receiver",
                    "Receiver",
                    "note",
                    &format!("message {index}"),
                    serde_json::json!({}),
                )
                .await
                .unwrap();
        }
        let messages = engine
            .list_agent_messages(&session.id, None, false)
            .await
            .unwrap();
        assert_eq!(messages.len(), 3);

        let policy = engine
            .set_mailbox_policy(
                &project.id,
                MailboxPolicy {
                    max_age_secs: None,
                    max_count: Some(2),
                    delete_unread: false,
                    auto_gc: false,
                },
            )
            .await
            .unwrap();
        let gc = engine.gc_mailbox(&project.id, &policy).await.unwrap();
        assert_eq!(gc.removed, 0);
        assert_eq!(
            engine
                .list_agent_messages(&session.id, None, false)
                .await
                .unwrap()
                .len(),
            3
        );

        engine
            .mark_agent_message_read(&messages[0].id)
            .await
            .unwrap();
        let gc = engine.gc_mailbox(&project.id, &policy).await.unwrap();
        assert_eq!(gc.removed, 1);
        let remaining = engine
            .list_agent_messages(&session.id, None, false)
            .await
            .unwrap();
        assert_eq!(remaining.len(), 2);
        assert!(!remaining.iter().any(|message| message.id == messages[0].id));

        let policy = engine
            .set_mailbox_policy(
                &project.id,
                MailboxPolicy {
                    max_age_secs: None,
                    max_count: Some(1),
                    delete_unread: true,
                    auto_gc: false,
                },
            )
            .await
            .unwrap();
        let gc = engine.gc_mailbox(&project.id, &policy).await.unwrap();
        assert_eq!(gc.removed, 1);
        assert_eq!(
            engine
                .list_agent_messages(&session.id, None, false)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            engine
                .set_mailbox_policy(
                    &project.id,
                    MailboxPolicy {
                        max_age_secs: None,
                        max_count: Some(0),
                        delete_unread: false,
                        auto_gc: false,
                    },
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn mailbox_auto_gc_respects_unread_protection() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("auto-mailbox".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session(project.id.clone(), "auto-mailbox".into(), None)
            .await
            .unwrap();
        engine
            .set_mailbox_policy(
                &project.id,
                MailboxPolicy {
                    max_age_secs: None,
                    max_count: Some(1),
                    delete_unread: true,
                    auto_gc: true,
                },
            )
            .await
            .unwrap();
        for index in 0..2 {
            engine
                .send_agent_message(
                    &session.id,
                    "sender",
                    "Sender",
                    "receiver",
                    "Receiver",
                    "note",
                    &format!("auto {index}"),
                    serde_json::json!({}),
                )
                .await
                .unwrap();
        }
        let messages = engine
            .list_agent_messages(&session.id, None, false)
            .await
            .unwrap();
        assert_eq!(messages.len(), 1, "auto GC should trim to max_count");

        engine
            .set_mailbox_policy(
                &project.id,
                MailboxPolicy {
                    max_age_secs: None,
                    max_count: Some(2),
                    delete_unread: false,
                    auto_gc: true,
                },
            )
            .await
            .unwrap();
        for index in 0..2 {
            engine
                .send_agent_message(
                    &session.id,
                    "sender",
                    "Sender",
                    "receiver",
                    "Receiver",
                    "note",
                    &format!("protected {index}"),
                    serde_json::json!({}),
                )
                .await
                .unwrap();
        }
        let messages = engine
            .list_agent_messages(&session.id, None, false)
            .await
            .unwrap();
        assert_eq!(messages.len(), 3, "unread messages must not be deleted");
        engine
            .mark_agent_message_read(&messages[0].id)
            .await
            .unwrap();
        let messages = engine
            .list_agent_messages(&session.id, None, false)
            .await
            .unwrap();
        assert_eq!(messages.len(), 2, "auto GC should trim after a read");
        let events = engine
            .list_events(session.id.clone(), 0, 200)
            .await
            .unwrap();
        assert!(
            events
                .iter()
                .filter(|event| event.kind == "mailbox.pruned")
                .count()
                >= 2
        );
    }

    #[tokio::test]
    async fn mailbox_replay_tracks_reads_prunes_and_repairs() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("mailbox replay".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session(project.id.clone(), "mailbox replay".into(), None)
            .await
            .unwrap();
        let first = engine
            .send_agent_message(
                &session.id,
                "sender",
                "Sender",
                "receiver",
                "Receiver",
                "note",
                "first",
                serde_json::json!({}),
            )
            .await
            .unwrap();
        engine
            .send_agent_message(
                &session.id,
                "sender",
                "Sender",
                "receiver",
                "Receiver",
                "note",
                "second",
                serde_json::json!({}),
            )
            .await
            .unwrap();
        engine.mark_agent_message_read(&first.id).await.unwrap();

        let report = engine.replay_session(&session.id).await.unwrap();
        assert!(report.mailbox_matches, "{:?}", report.mismatches);

        // Bypass the event path: replay must expose the missing row.
        engine.store().delete_agent_message(&first.id).unwrap();
        let report = engine.replay_session(&session.id).await.unwrap();
        assert!(!report.mailbox_matches);
        let repaired = engine.repair_session(&session.id).await.unwrap();
        assert!(repaired.consistent, "{:?}", repaired.mismatches);
        assert!(repaired.mailbox_matches);
        let messages = engine
            .list_agent_messages(&session.id, None, false)
            .await
            .unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages
                .iter()
                .find(|message| message.id == first.id)
                .unwrap()
                .status,
            "read"
        );

        let policy = engine
            .set_mailbox_policy(
                &project.id,
                MailboxPolicy {
                    max_age_secs: None,
                    max_count: Some(1),
                    delete_unread: false,
                    auto_gc: false,
                },
            )
            .await
            .unwrap();
        let gc = engine.gc_mailbox(&project.id, &policy).await.unwrap();
        assert_eq!(gc.removed, 1);
        let events = engine
            .list_events(session.id.clone(), 0, 200)
            .await
            .unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "mailbox.pruned" && event.payload["message_id"] == first.id
        }));
        let report = engine.replay_session(&session.id).await.unwrap();
        assert!(report.consistent, "{:?}", report.mismatches);
        assert!(report.mailbox_matches);
    }

    #[tokio::test]
    async fn integration_policy_round_trips_and_validates() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("integration-policy".into()),
            )
            .await
            .unwrap();
        assert_eq!(
            engine.get_integration_policy(&project.id).await.unwrap(),
            IntegrationPolicy::default()
        );
        let saved = engine
            .set_integration_policy(
                &project.id,
                IntegrationPolicy {
                    auto_resolve: true,
                    max_auto_attempts: 3,
                    max_resolve_tokens: Some(25_000),
                },
            )
            .await
            .unwrap();
        assert!(saved.auto_resolve);
        assert_eq!(
            engine.get_integration_policy(&project.id).await.unwrap(),
            saved
        );
        assert!(
            engine
                .set_integration_policy(
                    &project.id,
                    IntegrationPolicy {
                        auto_resolve: true,
                        max_auto_attempts: 6,
                        max_resolve_tokens: None,
                    },
                )
                .await
                .is_err()
        );
        assert!(
            engine
                .set_integration_policy(
                    &project.id,
                    IntegrationPolicy {
                        auto_resolve: true,
                        max_auto_attempts: 1,
                        max_resolve_tokens: Some(10),
                    },
                )
                .await
                .is_err()
        );
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
            .create_session(project.id.clone(), "replay".into(), None)
            .await
            .unwrap();
        engine
            .update_session_status(&session.id, "running")
            .await
            .unwrap();
        engine
            .record_agent_usage(
                &session.id,
                "agent-1",
                "Worker",
                "worker",
                Some("T1".into()),
                Some("impl".into()),
                "test",
                7,
                3,
                false,
                0.25,
            )
            .await
            .unwrap();
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
        let (approval, _approval_rx) = engine
            .request_approval(
                &session.id,
                "call-approval",
                "shell",
                serde_json::json!({"command": "true"}),
                None,
            )
            .await
            .unwrap();
        engine.resolve_approval(&approval.id, true).await.unwrap();
        let agent = engine
            .create_agent(&session.id, "worker", "Worker", None, None, "test")
            .await
            .unwrap();
        let agent_task = engine
            .create_task_worktree(
                &project.root,
                &project.id,
                &session.id,
                "agent-task",
                session.branch.as_deref().unwrap(),
            )
            .await
            .unwrap();
        engine
            .set_agent_worktree(
                &agent.id,
                &agent_task.path.to_string_lossy(),
                &agent_task.branch,
            )
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
        assert!(report.ledger_matches);
        assert!(report.plan_matches);
        assert!(report.todos_matches);
        assert!(report.messages_matches);
        assert!(report.queue_matches);

        // Bypass the event path deliberately: replay must expose the drift.
        engine
            .store()
            .update_session_status(&session.id, "idle")
            .unwrap();
        engine
            .store()
            .replace_usage_ledger(&session.id, &[])
            .unwrap();
        let report = engine.replay_session(&session.id).await.unwrap();
        assert!(!report.consistent);
        assert!(!report.status_matches);
        assert!(!report.ledger_matches);

        let repaired = engine.repair_session(&session.id).await.unwrap();
        assert!(repaired.repaired);
        assert!(repaired.consistent, "{:?}", repaired.mismatches);
        assert!(repaired.ledger_matches);
        assert_eq!(
            engine
                .usage_ledger(&session.id)
                .await
                .unwrap()
                .entries
                .len(),
            1
        );
        let sessions = engine
            .list_sessions(Some(session.project_id.clone()))
            .await
            .unwrap();
        assert_eq!(sessions[0].status, "running");

        engine
            .store()
            .update_agent_status(&agent.id, "cancelled")
            .unwrap();
        let report = engine.replay_session(&session.id).await.unwrap();
        assert!(!report.agents_matches);
        let repaired = engine.repair_session(&session.id).await.unwrap();
        assert!(repaired.consistent, "{:?}", repaired.mismatches);
        let agents = engine.list_agents(&session.id).await.unwrap();
        assert_eq!(agents[0].status, "running");

        engine
            .send_agent_message(
                &session.id,
                &agent.id,
                &agent.name,
                &agent.id,
                &agent.name,
                "note",
                "bundle mailbox note",
                serde_json::json!({"bundle": true}),
            )
            .await
            .unwrap();
        engine
            .publish_artifact(
                &session.id,
                &agent.id,
                &agent.name,
                Some("T1".into()),
                "bundle-note.md",
                "note",
                "text/markdown",
                "artifact content",
            )
            .await
            .unwrap();
        let session_worktree = PathBuf::from(session.worktree.clone().unwrap());
        std::fs::write(session_worktree.join("bundle.bin"), b"binary bundle").unwrap();
        engine
            .publish_artifact_file(
                &session.id,
                &agent.id,
                &agent.name,
                Some("T1".into()),
                "bundle.bin",
                "binary",
                "application/octet-stream",
                &session_worktree.join("bundle.bin"),
                &session_worktree,
            )
            .await
            .unwrap();
        engine
            .record_agent_usage(
                &session.id,
                &agent.id,
                &agent.name,
                "worker",
                Some("T1".into()),
                Some("impl".into()),
                "test",
                11,
                5,
                false,
                0.125,
            )
            .await
            .unwrap();

        let artifacts = engine.list_artifacts(&session.id).await.unwrap();
        let note = artifacts
            .iter()
            .find(|artifact| artifact.name == "bundle-note.md")
            .unwrap();
        let note_id = note.id.clone();
        let report = engine.replay_session(&session.id).await.unwrap();
        assert!(report.consistent, "{:?}", report.mismatches);
        assert!(report.artifacts_matches);
        engine.store().delete_artifact(&note_id).unwrap();
        let report = engine.replay_session(&session.id).await.unwrap();
        assert!(!report.artifacts_matches);
        let repaired = engine.repair_session(&session.id).await.unwrap();
        assert!(repaired.consistent, "{:?}", repaired.mismatches);
        assert!(repaired.artifacts_matches);
        let restored = engine
            .list_artifacts(&session.id)
            .await
            .unwrap()
            .into_iter()
            .find(|artifact| artifact.id == note_id)
            .unwrap();
        assert_eq!(restored.content, "artifact content");
        assert_eq!(restored.mime_type, "text/markdown");

        let bundle = engine.export_session(&session.id).await.unwrap();
        assert_eq!(bundle.version, 1);
        assert_eq!(bundle.session.id, session.id);
        assert_eq!(bundle.agents.len(), 1);
        assert_eq!(bundle.messages.len(), 1);
        assert_eq!(bundle.todos.len(), 1);
        assert_eq!(bundle.approvals.len(), 1);
        assert_eq!(bundle.approvals[0].status, "approved");
        assert_eq!(bundle.merge_queue.len(), 1);
        assert_eq!(bundle.agent_messages.len(), 1);
        assert_eq!(bundle.artifacts.len(), 2);
        assert_eq!(bundle.usage.len(), 2);
        assert!(bundle.usage.iter().any(|usage| {
            usage.agent_name == agent.name && usage.department.as_deref() == Some("impl")
        }));
        assert!(bundle.team_plan.is_some());
        assert!(!bundle.events.is_empty());
        let rebuilt = engine.rebuild_session(&session.id).await.unwrap();
        assert!(rebuilt.repaired);
        assert!(rebuilt.consistent, "{:?}", rebuilt.mismatches);
        let events = engine
            .list_events(session.id.clone(), 0, 1000)
            .await
            .unwrap();
        assert!(events.iter().any(|event| event.kind == "session.rebuilt"));

        let original_worktree = session.worktree.clone().unwrap();
        let original_branch = session.branch.clone().unwrap();
        let import_data = TempDir::new().unwrap();
        let imported_engine = Engine::new(Store::open(import_data.path()).unwrap());
        let imported = imported_engine.import_session(bundle).await.unwrap();
        assert_eq!(imported.session_id, session.id);
        assert_eq!(imported.status, "imported");
        assert_eq!(
            imported_engine
                .list_agent_messages(&session.id, None, false)
                .await
                .unwrap()
                .len(),
            1
        );
        let imported_artifacts = imported_engine.list_artifacts(&session.id).await.unwrap();
        assert_eq!(imported_artifacts.len(), 2);
        let imported_binary = imported_artifacts
            .iter()
            .find(|artifact| artifact.name == "bundle.bin")
            .unwrap();
        assert_eq!(imported_binary.storage, "file");
        let imported_bytes = std::fs::read(imported_binary.path.as_ref().unwrap()).unwrap();
        assert_eq!(imported_bytes, b"binary bundle");
        let imported_sessions = imported_engine
            .list_sessions(Some(imported.project_id.clone()))
            .await
            .unwrap();
        assert_eq!(imported_sessions[0].status, "imported");
        assert!(imported_sessions[0].worktree.is_none());
        let imported_agents = imported_engine.list_agents(&session.id).await.unwrap();
        assert_eq!(imported_agents.len(), 1);
        let imported_messages = imported_engine.list_messages(&session.id).await.unwrap();
        assert_eq!(imported_messages.len(), 1);
        let imported_usage = imported_engine.usage_ledger(&session.id).await.unwrap();
        assert_eq!(imported_usage.entries.len(), 2);
        assert!(
            imported_usage
                .entries
                .iter()
                .any(|entry| entry.cost_usd == 0.125)
        );
        assert_eq!(imported_usage.totals.input_tokens, 18);
        let imported_approvals = imported_engine
            .list_approvals(Some(session.id.clone()))
            .await
            .unwrap();
        assert_eq!(imported_approvals.len(), 1);
        assert_eq!(imported_approvals[0].status, "approved");
        let imported_report = imported_engine.replay_session(&session.id).await.unwrap();
        assert!(
            imported_report.consistent,
            "{:?}",
            imported_report.mismatches
        );
        let imported_diff = imported_engine.session_diff(&session.id).await.unwrap();
        assert!(imported_diff.status.contains("imported archive"));
        assert!(imported_diff.worktree.is_empty());
        let error = imported_engine
            .discard_session(&session.id)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("read-only"));

        let task_remove = Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args([
                "worktree",
                "remove",
                "--force",
                agent_task.path.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(task_remove.status.success());
        worktree::remove(
            repo.path(),
            std::path::Path::new(&original_worktree),
            &original_branch,
        );
        let rehydrated_worktree = import_data.path().join("rehydrated");
        let rehydrated = imported_engine
            .rehydrate_session(
                &session.id,
                Some(rehydrated_worktree.to_string_lossy().to_string()),
            )
            .await
            .unwrap();
        assert_eq!(rehydrated.status, "idle");
        assert_eq!(rehydrated.worktree, rehydrated_worktree.to_string_lossy());
        assert!(rehydrated_worktree.join(".git").is_file());
        assert!(rehydrated.agents_remapped >= 1, "{:?}", rehydrated.warnings);
        let remapped_agents = imported_engine.list_agents(&session.id).await.unwrap();
        let remapped_agent = remapped_agents
            .iter()
            .find(|candidate| candidate.id == agent.id)
            .unwrap();
        let remapped_path = remapped_agent.worktree.clone().unwrap();
        assert_ne!(remapped_path, agent_task.path.to_string_lossy().to_string());
        assert!(std::path::Path::new(&remapped_path).join(".git").is_file());
        let rehydrated_report = imported_engine.replay_session(&session.id).await.unwrap();
        assert!(
            rehydrated_report.consistent,
            "{:?}",
            rehydrated_report.mismatches
        );
        let rehydrated_diff = imported_engine.session_diff(&session.id).await.unwrap();
        assert!(!rehydrated_diff.status.contains("imported archive"));
        let error = imported_engine
            .rehydrate_session(&session.id, None)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("only imported"));
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

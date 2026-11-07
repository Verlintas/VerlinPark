//! Application services shared by the daemon and future harness components.
//!
//! M1 adds the Leader agent loop, tool registry, approvals, and event stream.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::tools::ToolContext;
use anyhow::{Context, Result};
use async_recursion::async_recursion;
use tokio::sync::{broadcast, oneshot};
use uuid::Uuid;
use vp_protocol::types::{
    AcpAgent, AcpAgentListResult, AcpProbeResult, AgentMessage, AgentRecord, Approval, Artifact,
    ArtifactBackfillResult, ArtifactGcResult, ArtifactMaterialization, ArtifactPolicy,
    CheckpointPolicyResult, CheckpointRetentionPolicy, ConsoleCloseResult, ConsoleOpenResult,
    ConsoleReadResult, ConsoleSignalResult, ConsoleWriteResult, ContextCompactionSummary,
    ContextMeasurement, ContextUsage, CreateModelPresetParams, DoctorCheck, Event, GitLogResult,
    GitShowResult, IntegrationPolicy, MailboxGcResult, MailboxPolicy, McpCallResult,
    McpGetPromptResult, McpPromptListResult, McpReadResourceResult, McpResourceListResult,
    McpServer, McpServerListResult, McpToolListResult, MemoryDeleteResult, MemoryItem,
    MemoryListResult, MemoryPolicy, MemoryPolicyResult, MemoryPruneResult, MergeQueueEntry,
    ModelPreset, ModelPresetListResult, Project, ProjectBudget, ProjectBudgetResult,
    ProjectUsageResult, PullRequestResult, Question, QuestionListResult, ScheduledTask,
    SearchResult, Session, SessionBundle, SessionCheckpoint, SessionCheckpointDeleteResult,
    SessionCheckpointDiffResult, SessionCheckpointListResult, SessionCheckpointPruneResult,
    SessionCheckpointRestoreResult, SessionCheckpointStatsResult, SessionCommit,
    SessionCompareFile, SessionCompareResult, SessionContextStatus, SessionDiff, SessionForkResult,
    SessionForkTreeNode, SessionForkTreeResult, SessionImportResult, SessionLandResult,
    SessionLineageEntry, SessionLineageResult, SessionLogStatus, SessionPriorityResult,
    SessionRehydrateResult, SessionReplayReport, SessionTranscriptResult, ShellPolicy,
    StoredMessage, TaskQueueResult, TaskSchedulerPolicy, TaskSchedulerPolicyResult, TeamPlan,
    TerminalCloseResult, TerminalOpenResult, TerminalReadResult, TerminalResizeResult,
    TerminalWriteResult, TodoItem, UpdateModelPresetParams, UsageBucket, UsageCsvResult,
    UsageEntry, UsageLedgerResult, UsageTotals, WorkspaceFileResult, WorkspaceTreeResult,
    WorkspaceWriteResult, WorktreeCleanupEntry, WorktreeCleanupResult, WorktreePolicyResult,
    WorktreeRetentionPolicy, WriteLease,
};
use vp_store::Store;
use vp_store::store::SessionRepair;

use crate::budget::BudgetRegistry;

pub mod acp;
pub mod agent;
pub mod budget;
pub mod console;
mod dashboard;
pub mod history;
pub mod mcp;
pub mod rebuild;
pub mod scheduler;
mod session_graph;
pub mod terminal;
pub mod update;
pub mod workspace;

pub use rebuild::rebuild_database;
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
    memories: Vec<MemoryItem>,
    questions: Vec<Question>,
    tasks: Vec<ScheduledTask>,
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
    question_waiters: Arc<Mutex<HashMap<String, oneshot::Sender<String>>>>,
    git_lock: Arc<Mutex<()>>,
    budget_registry: Arc<BudgetRegistry>,
    mcp: Arc<mcp::McpManager>,
    consoles: Arc<console::ConsoleManager>,
    terminals: Arc<terminal::TerminalManager>,
    scheduler: Arc<scheduler::TaskScheduler>,
}

fn artifact_event_payload(artifact: &Artifact) -> serde_json::Value {
    serde_json::json!({
        "artifact_id": artifact.id,
        "name": artifact.name,
        "kind": artifact.kind,
        "agent_id": artifact.agent_id,
        "agent_name": artifact.agent_name,
        "task_id": artifact.task_id,
        "mime_type": artifact.mime_type,
        "storage": artifact.storage,
        "compression": artifact.compression,
        "size_bytes": artifact.size_bytes,
        "sha256": artifact.sha256,
        "path": artifact.path,
        "content": artifact.content,
    })
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

fn validate_memory_policy(policy: &MemoryPolicy) -> Result<()> {
    if let Some(max_items) = policy.max_items
        && !(1..=100_000).contains(&max_items)
    {
        anyhow::bail!("memory max_items must be between 1 and 100000");
    }
    if let Some(max_age_days) = policy.max_age_days
        && !(1..=36_500).contains(&max_age_days)
    {
        anyhow::bail!("memory max_age_days must be between 1 and 36500");
    }
    if policy.enabled && policy.max_items.is_none() && policy.max_age_days.is_none() {
        anyhow::bail!("enabled memory policy requires max_items or max_age_days");
    }
    Ok(())
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

#[derive(Clone, Copy)]
enum PrCli {
    GitHub,
    GitLab,
}

impl PrCli {
    fn binary_name(self) -> &'static str {
        match self {
            Self::GitHub => "gh",
            Self::GitLab => "glab",
        }
    }

    fn change_name(self) -> &'static str {
        match self {
            Self::GitHub => "PR",
            Self::GitLab => "MR",
        }
    }

    fn auth_args(self) -> &'static [&'static str] {
        &["auth", "status"]
    }

    fn create_args(self, base_branch: &str, branch: &str, title: &str, body: &str) -> Vec<String> {
        match self {
            Self::GitHub => vec![
                "pr".into(),
                "create".into(),
                "--base".into(),
                base_branch.into(),
                "--head".into(),
                branch.into(),
                "--title".into(),
                title.into(),
                "--body".into(),
                body.into(),
            ],
            Self::GitLab => vec![
                "mr".into(),
                "create".into(),
                "--source-branch".into(),
                branch.into(),
                "--target-branch".into(),
                base_branch.into(),
                "--title".into(),
                title.into(),
                "--description".into(),
                body.into(),
                "--yes".into(),
            ],
        }
    }

    fn create_label(self) -> &'static str {
        match self {
            Self::GitHub => "gh pr create",
            Self::GitLab => "glab mr create",
        }
    }
}

fn detect_pr_cli(origin: &str) -> PrCli {
    if origin.to_ascii_lowercase().contains("gitlab") {
        PrCli::GitLab
    } else {
        PrCli::GitHub
    }
}

impl Engine {
    pub fn new(store: Store) -> Self {
        let (events, _) = broadcast::channel(2048);
        Self {
            scheduler: Arc::new(scheduler::TaskScheduler::new(store.clone())),
            store,
            events,
            approval_waiters: Arc::new(Mutex::new(HashMap::new())),
            question_waiters: Arc::new(Mutex::new(HashMap::new())),
            git_lock: Arc::new(Mutex::new(())),
            budget_registry: Arc::new(BudgetRegistry::new()),
            mcp: Arc::new(mcp::McpManager::from_env()),
            consoles: Arc::new(console::ConsoleManager::new()),
            terminals: Arc::new(terminal::TerminalManager::new()),
        }
    }

    pub fn budget_registry(&self) -> &Arc<BudgetRegistry> {
        &self.budget_registry
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn scheduler(&self) -> &Arc<scheduler::TaskScheduler> {
        &self.scheduler
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

    pub async fn get_task_scheduler_policy(
        &self,
        project_id: &str,
    ) -> Result<TaskSchedulerPolicyResult> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking({
            let store = store.clone();
            let project_id = project_id.clone();
            move || store.get_project(&project_id)
        })
        .await??
        .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        let id = project_id.clone();
        let policy =
            tokio::task::spawn_blocking(move || store.get_task_scheduler_policy(&id)).await??;
        Ok(TaskSchedulerPolicyResult { project_id, policy })
    }

    pub async fn set_task_scheduler_policy(
        &self,
        project_id: &str,
        policy: TaskSchedulerPolicy,
    ) -> Result<TaskSchedulerPolicyResult> {
        if !(1..=86_400).contains(&policy.cooldown_secs) {
            anyhow::bail!("task scheduler cooldown must be between 1 and 86400 seconds");
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
        let stored = policy.clone();
        let id = project_id.clone();
        tokio::task::spawn_blocking(move || store.set_task_scheduler_policy(&id, &stored))
            .await??;
        Ok(TaskSchedulerPolicyResult { project_id, policy })
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
        self.create_session_full(
            project_id,
            goal,
            token_budget,
            cost_budget_usd,
            time_budget_secs,
            0,
        )
        .await
    }

    pub async fn create_session_with_priority(
        &self,
        project_id: String,
        goal: String,
        token_budget: Option<u64>,
        priority: i32,
    ) -> Result<Session> {
        self.create_session_full(project_id, goal, token_budget, None, None, priority)
            .await
    }

    async fn create_session_full(
        &self,
        project_id: String,
        goal: String,
        token_budget: Option<u64>,
        cost_budget_usd: Option<f64>,
        time_budget_secs: Option<u64>,
        priority: i32,
    ) -> Result<Session> {
        if !(-100..=100).contains(&priority) {
            anyhow::bail!("session priority must be between -100 and 100");
        }
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
                    priority,
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
            "priority": session.priority,
            "token_budget": session.token_budget,
            "cost_budget_usd": session.cost_budget_usd,
            "time_budget_secs": session.time_budget_secs,
            "base_commit": session.base_commit,
            "created_at": session.created_at,
        });
        let _ = self
            .append_event(&session.id, "session.created", payload)
            .await?;
        Ok(session)
    }

    /// Snapshot the active conversation and committed session branch. The
    /// worktree must be clean so the checkpoint commit is reproducible.
    pub async fn checkpoint_session(
        &self,
        session_id: &str,
        label: Option<String>,
    ) -> Result<SessionCheckpoint> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        ensure_session_quiescent(&session, "create a checkpoint")?;
        let worktree = PathBuf::from(
            session
                .worktree
                .clone()
                .ok_or_else(|| anyhow::anyhow!("session has no worktree"))?,
        );
        if !worktree.is_dir() {
            anyhow::bail!("session worktree is missing: {}", worktree.display());
        }
        let checkpoint_id = Uuid::now_v7().to_string();
        let label = label
            .map(|label| {
                label
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(120)
                    .collect::<String>()
            })
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| format!("checkpoint {}", &checkpoint_id[..8]));
        let git_lock = self.git_lock.clone();
        let status_path = worktree.clone();
        let branch_path = worktree.clone();
        let snapshot_path = worktree.clone();
        let data_dir = self.store.data_dir().to_path_buf();
        let session_id_for_snapshot = session.id.clone();
        let checkpoint_id_for_snapshot = checkpoint_id.clone();
        let label_for_snapshot = label.clone();
        let expected_branch = session.branch.clone();
        let (commit, status) = tokio::task::spawn_blocking(move || {
            let _guard = git_lock
                .lock()
                .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
            let status = worktree::status_porcelain(&status_path)?;
            let branch = worktree::current_branch(&branch_path)?;
            if let Some(expected) = expected_branch.as_deref()
                && branch != expected
            {
                anyhow::bail!(
                    "session worktree is on branch {branch}; expected {expected}. Check it out before creating a checkpoint"
                );
            }
            let commit = worktree::snapshot_worktree(
                &snapshot_path,
                &data_dir,
                &session_id_for_snapshot,
                &checkpoint_id_for_snapshot,
                &label_for_snapshot,
            )?;
            Ok::<_, anyhow::Error>((commit, status))
        })
        .await??;
        let dirty = !status.is_empty();
        let (messages, start, summary) = self.load_active_snapshot(session_id).await?;
        let start_message_id = messages.get(start).map(|message| message.id.clone());
        let through_message_id = if start < messages.len() {
            messages.last().map(|message| message.id.clone())
        } else {
            None
        };
        let message_count = messages.len().saturating_sub(start);
        let payload = serde_json::json!({
            "checkpoint_id": checkpoint_id,
            "label": label,
            "commit": commit,
            "branch": session.branch,
            "worktree": session.worktree,
            "start_message_id": start_message_id,
            "through_message_id": through_message_id,
            "message_count": message_count,
            "summary": summary,
            "dirty": dirty,
        });
        let event = self
            .append_event(session_id, "session.checkpoint", payload)
            .await?;
        checkpoint_from_event(&event)
            .ok_or_else(|| anyhow::anyhow!("could not decode checkpoint event"))
    }

    pub async fn list_checkpoints(&self, session_id: &str) -> Result<SessionCheckpointListResult> {
        Ok(SessionCheckpointListResult {
            checkpoints: self.active_checkpoints(session_id).await?,
        })
    }

    pub(crate) async fn active_checkpoints(
        &self,
        session_id: &str,
    ) -> Result<Vec<SessionCheckpoint>> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let (created, deleted) = tokio::task::spawn_blocking(move || {
            Ok::<_, anyhow::Error>((
                store.list_events_by_kind(&id, "session.checkpoint", 1000)?,
                store.list_events_by_kind(&id, "session.checkpoint.deleted", 1000)?,
            ))
        })
        .await??;
        let deleted_ids = deleted
            .iter()
            .filter_map(|event| {
                event
                    .payload
                    .get("checkpoint_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .collect::<std::collections::HashSet<_>>();
        Ok(created
            .iter()
            .filter_map(checkpoint_from_event)
            .filter(|checkpoint| !deleted_ids.contains(&checkpoint.id))
            .collect())
    }

    async fn ensure_checkpoint_mutable(&self, session_id: &str) -> Result<()> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        if session.status == "imported" {
            anyhow::bail!("imported sessions are read-only archives");
        }
        Ok(())
    }

    pub async fn checkpoint_stats(&self, session_id: &str) -> Result<SessionCheckpointStatsResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let (session, project) = tokio::task::spawn_blocking(move || {
            let session = store
                .get_session(&id)?
                .ok_or_else(|| anyhow::anyhow!("session not found: {id}"))?;
            let project = store
                .get_project(&session.project_id)?
                .ok_or_else(|| anyhow::anyhow!("project not found: {}", session.project_id))?;
            Ok::<_, anyhow::Error>((session, project))
        })
        .await??;
        let checkpoints = self.active_checkpoints(session_id).await?;
        let dirty = checkpoints
            .iter()
            .filter(|checkpoint| checkpoint.dirty)
            .count();
        let project_root = PathBuf::from(&project.root);
        let git_lock = self.git_lock.clone();
        let id = session.id.clone();
        let checkpoints_for_git = checkpoints.clone();
        let (commit_bytes, snapshot_bytes, unique_blobs, refs) =
            tokio::task::spawn_blocking(move || -> Result<_> {
                let _guard = git_lock
                    .lock()
                    .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
                let mut commit_bytes = 0u64;
                let mut snapshot_bytes = 0u64;
                let mut seen = std::collections::HashSet::new();
                for checkpoint in &checkpoints_for_git {
                    if let Ok(size) =
                        worktree::git_output(&project_root, &["cat-file", "-s", &checkpoint.commit])
                    {
                        commit_bytes =
                            commit_bytes.saturating_add(size.trim().parse().unwrap_or(0));
                    }
                    let parent = format!("{}^", checkpoint.commit);
                    if let Ok(parent) = worktree::git_output(&project_root, &["rev-parse", &parent])
                    {
                        let range = format!("{parent}..{}", checkpoint.commit);
                        if let Ok(paths) =
                            worktree::git_output(&project_root, &["diff", "--name-only", &range])
                        {
                            for path in paths.lines().filter(|path| !path.trim().is_empty()) {
                                let spec = format!("{}:{}", checkpoint.commit, path);
                                if let Ok(blob) =
                                    worktree::git_output(&project_root, &["rev-parse", &spec])
                                    && seen.insert(blob.clone())
                                    && let Ok(size) = worktree::git_output(
                                        &project_root,
                                        &["cat-file", "-s", &blob],
                                    )
                                {
                                    snapshot_bytes = snapshot_bytes
                                        .saturating_add(size.trim().parse().unwrap_or(0));
                                }
                            }
                        }
                    }
                }
                let prefix = format!("refs/verlinpark/checkpoints/{id}/");
                let refs = worktree::git_output(
                    &project_root,
                    &["for-each-ref", "--format=%(refname)", &prefix],
                )
                .map(|output| {
                    output
                        .lines()
                        .filter(|line| !line.trim().is_empty())
                        .count()
                })
                .unwrap_or(0);
                Ok((commit_bytes, snapshot_bytes, seen.len(), refs))
            })
            .await??;
        Ok(SessionCheckpointStatsResult {
            session_id: session.id,
            checkpoints: checkpoints.len(),
            dirty,
            refs,
            commit_bytes,
            snapshot_bytes,
            unique_blobs,
        })
    }

    pub async fn restore_checkpoint(
        &self,
        session_id: &str,
        checkpoint_id: &str,
    ) -> Result<SessionCheckpointRestoreResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let (session, project) = tokio::task::spawn_blocking(move || {
            let session = store
                .get_session(&id)?
                .ok_or_else(|| anyhow::anyhow!("session not found: {id}"))?;
            let project = store
                .get_project(&session.project_id)?
                .ok_or_else(|| anyhow::anyhow!("project not found: {}", session.project_id))?;
            Ok::<_, anyhow::Error>((session, project))
        })
        .await??;
        ensure_session_quiescent(&session, "restore a checkpoint")?;
        let checkpoint = self
            .active_checkpoints(session_id)
            .await?
            .into_iter()
            .find(|checkpoint| checkpoint.id == checkpoint_id)
            .ok_or_else(|| anyhow::anyhow!("checkpoint not found: {checkpoint_id}"))?;
        let worktree = PathBuf::from(
            session
                .worktree
                .clone()
                .ok_or_else(|| anyhow::anyhow!("session has no worktree"))?,
        );
        if !worktree.is_dir() {
            anyhow::bail!("session worktree is missing: {}", worktree.display());
        }
        let expected_branch = session.branch.clone();
        let project_root = PathBuf::from(&project.root);
        let git_lock = self.git_lock.clone();
        let restore_path = worktree.clone();
        let status_path = worktree.clone();
        let branch_path = worktree.clone();
        let restore_commit = checkpoint.commit.clone();
        let (previous_commit, restored_commit) = tokio::task::spawn_blocking(move || {
            let _guard = git_lock
                .lock()
                .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
            let status = worktree::status_porcelain(&status_path)?;
            let branch = worktree::current_branch(&branch_path)?;
            if let Some(expected) = expected_branch.as_deref()
                && branch != expected
            {
                anyhow::bail!(
                    "session worktree is on branch {branch}; expected {expected}"
                );
            }
            if !status.is_empty() {
                anyhow::bail!(
                    "session worktree has uncommitted changes; commit or discard them before restoring a checkpoint"
                );
            }
            if !worktree::commit_exists(&project_root, &restore_commit) {
                anyhow::bail!("checkpoint commit is missing: {restore_commit}");
            }
            let previous = worktree::head_commit(&restore_path)?;
            worktree::reset_hard(&restore_path, &restore_commit)?;
            Ok::<_, anyhow::Error>((previous, restore_commit))
        })
        .await??;
        self.append_event(
            session_id,
            "session.checkpoint.restored",
            serde_json::json!({
                "checkpoint_id": checkpoint.id,
                "previous_commit": previous_commit,
                "restored_commit": restored_commit,
            }),
        )
        .await?;
        Ok(SessionCheckpointRestoreResult {
            session_id: session.id,
            checkpoint_id: checkpoint.id,
            previous_commit,
            restored_commit,
        })
    }

    pub async fn diff_checkpoint(
        &self,
        session_id: &str,
        checkpoint_id: &str,
    ) -> Result<SessionCheckpointDiffResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let (session, project) = tokio::task::spawn_blocking(move || {
            let session = store
                .get_session(&id)?
                .ok_or_else(|| anyhow::anyhow!("session not found: {id}"))?;
            let project = store
                .get_project(&session.project_id)?
                .ok_or_else(|| anyhow::anyhow!("project not found: {}", session.project_id))?;
            Ok::<_, anyhow::Error>((session, project))
        })
        .await??;
        ensure_session_quiescent(&session, "inspect a checkpoint")?;
        let checkpoint = self
            .active_checkpoints(session_id)
            .await?
            .into_iter()
            .find(|checkpoint| checkpoint.id == checkpoint_id)
            .ok_or_else(|| anyhow::anyhow!("checkpoint not found: {checkpoint_id}"))?;
        let worktree = PathBuf::from(
            session
                .worktree
                .clone()
                .ok_or_else(|| anyhow::anyhow!("session has no worktree"))?,
        );
        if !worktree.is_dir() {
            anyhow::bail!("session worktree is missing: {}", worktree.display());
        }
        let expected_branch = session.branch.clone();
        let project_root = PathBuf::from(&project.root);
        let git_lock = self.git_lock.clone();
        let diff_path = worktree.clone();
        let checkpoint_commit = checkpoint.commit.clone();
        let (current_commit, current_branch, clean, numstat, patch) =
            tokio::task::spawn_blocking(move || {
                let _guard = git_lock
                    .lock()
                    .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
                let branch = worktree::current_branch(&diff_path)?;
                if let Some(expected) = expected_branch.as_deref()
                    && branch != expected
                {
                    anyhow::bail!("session worktree is on branch {branch}; expected {expected}");
                }
                let clean = worktree::status_porcelain(&diff_path)?.is_empty();
                if !worktree::commit_exists(&project_root, &checkpoint_commit) {
                    anyhow::bail!("checkpoint commit is missing: {checkpoint_commit}");
                }
                let current = worktree::head_commit(&diff_path)?;
                let range = format!("{current}..{checkpoint_commit}");
                let numstat = worktree::git_output(
                    &project_root,
                    &["diff", "--numstat", "--no-color", "--no-ext-diff", &range],
                )?;
                let patch = worktree::git_output(
                    &project_root,
                    &["diff", "--no-color", "--no-ext-diff", &range],
                )?;
                Ok::<_, anyhow::Error>((current, branch, clean, numstat, patch))
            })
            .await??;
        let truncated = patch.chars().count() > 200_000;
        Ok(SessionCheckpointDiffResult {
            session_id: session.id,
            checkpoint_id: checkpoint.id,
            current_commit,
            checkpoint_commit: checkpoint.commit,
            current_branch,
            clean,
            files: parse_numstat(&numstat),
            patch: clip_transcript(&patch, 200_000),
            truncated,
        })
    }

    pub async fn delete_checkpoint(
        &self,
        session_id: &str,
        checkpoint_id: &str,
    ) -> Result<SessionCheckpointDeleteResult> {
        self.ensure_checkpoint_mutable(session_id).await?;
        let checkpoint = self
            .active_checkpoints(session_id)
            .await?
            .into_iter()
            .find(|checkpoint| checkpoint.id == checkpoint_id);
        let Some(checkpoint) = checkpoint else {
            return Ok(SessionCheckpointDeleteResult {
                session_id: session_id.to_string(),
                checkpoint_id: checkpoint_id.to_string(),
                removed: false,
            });
        };
        self.delete_checkpoint_record(session_id, &checkpoint, "manual")
            .await?;
        Ok(SessionCheckpointDeleteResult {
            session_id: session_id.to_string(),
            checkpoint_id: checkpoint_id.to_string(),
            removed: true,
        })
    }

    async fn delete_checkpoint_record(
        &self,
        session_id: &str,
        checkpoint: &SessionCheckpoint,
        reason: &str,
    ) -> Result<()> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let project_root = tokio::task::spawn_blocking(move || -> Result<PathBuf> {
            let session = store
                .get_session(&id)?
                .ok_or_else(|| anyhow::anyhow!("session not found: {id}"))?;
            let project = store
                .get_project(&session.project_id)?
                .ok_or_else(|| anyhow::anyhow!("project not found: {}", session.project_id))?;
            Ok(PathBuf::from(project.root))
        })
        .await??;
        self.append_event(
            session_id,
            "session.checkpoint.deleted",
            serde_json::json!({
                "checkpoint_id": checkpoint.id,
                "label": checkpoint.label,
                "commit": checkpoint.commit,
                "reason": reason,
            }),
        )
        .await?;
        if let Err(error) =
            worktree::delete_checkpoint_ref(&project_root, session_id, &checkpoint.id)
        {
            tracing::warn!(
                %error,
                session_id,
                checkpoint_id = %checkpoint.id,
                "could not remove checkpoint snapshot ref"
            );
        }
        Ok(())
    }

    pub async fn prune_checkpoints(
        &self,
        session_id: &str,
        older_than_days: Option<u32>,
        keep_latest: Option<u32>,
        dry_run: bool,
    ) -> Result<SessionCheckpointPruneResult> {
        if older_than_days.is_none() && keep_latest.is_none() {
            anyhow::bail!("checkpoint prune requires older_than_days or keep_latest");
        }
        self.ensure_checkpoint_mutable(session_id).await?;
        let checkpoints = self.active_checkpoints(session_id).await?;
        let keep_latest = keep_latest.unwrap_or(0) as usize;
        let cutoff = older_than_days
            .map(|days| time::OffsetDateTime::now_utc() - time::Duration::days(i64::from(days)));
        let mut candidates = Vec::new();
        for (index, checkpoint) in checkpoints.iter().enumerate() {
            let beyond_keep = index < checkpoints.len().saturating_sub(keep_latest);
            let old = cutoff.is_none_or(|cutoff| {
                time::OffsetDateTime::parse(
                    &checkpoint.created_at,
                    &time::format_description::well_known::Rfc3339,
                )
                .map(|created_at| created_at < cutoff)
                .unwrap_or(false)
            });
            if beyond_keep && old {
                candidates.push(checkpoint.clone());
            }
        }
        if dry_run {
            return Ok(SessionCheckpointPruneResult {
                session_id: session_id.to_string(),
                candidates: candidates.len(),
                removed: 0,
                dry_run: true,
                checkpoints: candidates,
            });
        }
        let mut removed = 0usize;
        for checkpoint in &candidates {
            self.delete_checkpoint_record(session_id, checkpoint, "prune")
                .await?;
            removed += 1;
        }
        Ok(SessionCheckpointPruneResult {
            session_id: session_id.to_string(),
            candidates: candidates.len(),
            removed,
            dry_run: false,
            checkpoints: candidates,
        })
    }

    pub async fn list_model_presets(&self) -> Result<ModelPresetListResult> {
        let store = self.store.clone();
        let presets = tokio::task::spawn_blocking(move || store.list_model_presets()).await??;
        Ok(ModelPresetListResult { presets })
    }

    pub async fn create_model_preset(
        &self,
        params: CreateModelPresetParams,
    ) -> Result<ModelPreset> {
        validate_model_preset(
            &params.name,
            &params.provider,
            &params.model,
            params.context_window,
            params.input_usd_per_million,
            params.output_usd_per_million,
            &params.notes,
        )?;
        let preset = ModelPreset {
            id: Uuid::now_v7().to_string(),
            name: params.name.trim().to_string(),
            provider: params.provider.trim().to_string(),
            model: params.model.trim().to_string(),
            context_window: params.context_window,
            input_usd_per_million: params.input_usd_per_million,
            output_usd_per_million: params.output_usd_per_million,
            notes: params.notes.trim().to_string(),
            created_at: vp_protocol::now_rfc3339(),
        };
        let store = self.store.clone();
        let saved = preset.clone();
        tokio::task::spawn_blocking(move || store.create_model_preset(&saved)).await??;
        Ok(preset)
    }

    pub async fn update_model_preset(
        &self,
        params: UpdateModelPresetParams,
    ) -> Result<ModelPreset> {
        validate_model_preset(
            &params.name,
            &params.provider,
            &params.model,
            params.context_window,
            params.input_usd_per_million,
            params.output_usd_per_million,
            &params.notes,
        )?;
        let store = self.store.clone();
        let id = params.id.clone();
        let existing = tokio::task::spawn_blocking(move || store.get_model_preset(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("model preset not found: {}", params.id))?;
        let preset = ModelPreset {
            id: existing.id,
            name: params.name.trim().to_string(),
            provider: params.provider.trim().to_string(),
            model: params.model.trim().to_string(),
            context_window: params.context_window,
            input_usd_per_million: params.input_usd_per_million,
            output_usd_per_million: params.output_usd_per_million,
            notes: params.notes.trim().to_string(),
            created_at: existing.created_at,
        };
        let store = self.store.clone();
        let saved = preset.clone();
        tokio::task::spawn_blocking(move || store.update_model_preset(&saved)).await??;
        Ok(preset)
    }

    pub async fn delete_model_preset(&self, id: &str) -> Result<bool> {
        let store = self.store.clone();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || store.delete_model_preset(&id)).await?
    }

    pub async fn get_checkpoint_policy(
        &self,
        project_id: &str,
    ) -> Result<CheckpointRetentionPolicy> {
        let store = self.store.clone();
        let id = project_id.to_string();
        tokio::task::spawn_blocking(move || {
            store
                .get_project(&id)?
                .ok_or_else(|| anyhow::anyhow!("project not found: {id}"))?;
            store.get_checkpoint_policy(&id)
        })
        .await?
    }

    pub async fn set_checkpoint_policy(
        &self,
        project_id: &str,
        policy: &CheckpointRetentionPolicy,
    ) -> Result<CheckpointPolicyResult> {
        if policy.older_than_secs > 365 * 24 * 60 * 60 {
            anyhow::bail!("checkpoint retention must be at most 365 days");
        }
        if policy.keep_latest > 100 {
            anyhow::bail!("checkpoint keep_latest must be at most 100");
        }
        let store = self.store.clone();
        let id = project_id.to_string();
        let policy = policy.clone();
        let saved = tokio::task::spawn_blocking(move || {
            store
                .get_project(&id)?
                .ok_or_else(|| anyhow::anyhow!("project not found: {id}"))?;
            store.set_checkpoint_policy(&id, &policy)?;
            store.get_checkpoint_policy(&id)
        })
        .await??;
        Ok(CheckpointPolicyResult {
            project_id: project_id.to_string(),
            policy: saved,
        })
    }

    /// Run the opt-in checkpoint retention policy for every project. Deletes
    /// only checkpoints older than the policy threshold and beyond its
    /// keep-latest count. Existing fork branches are unaffected.
    pub async fn sweep_checkpoint_retention(&self) -> Result<usize> {
        let store = self.store.clone();
        let policies =
            tokio::task::spawn_blocking(move || store.list_enabled_checkpoint_policies()).await??;
        let mut removed = 0usize;
        for (project_id, policy) in policies {
            let store = self.store.clone();
            let sessions =
                tokio::task::spawn_blocking(move || store.list_sessions(Some(&project_id)))
                    .await??;
            let days = policy.older_than_secs.div_ceil(86_400) as u32;
            for session in sessions {
                if session.status == "imported" {
                    continue;
                }
                removed += self
                    .prune_checkpoints(&session.id, Some(days), Some(policy.keep_latest), false)
                    .await?
                    .removed;
            }
        }
        Ok(removed)
    }

    /// Create a new session/worktree from a checkpoint. Without an explicit
    /// checkpoint, one is created from the source's current worktree state.
    pub async fn fork_session(
        &self,
        source_session_id: &str,
        checkpoint_id: Option<&str>,
        goal: Option<String>,
    ) -> Result<SessionForkResult> {
        let store = self.store.clone();
        let source_id = source_session_id.to_string();
        let source = tokio::task::spawn_blocking(move || store.get_session(&source_id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {source_session_id}"))?;
        ensure_session_quiescent(&source, "fork a session")?;
        let checkpoint = if let Some(checkpoint_id) = checkpoint_id {
            self.list_checkpoints(source_session_id)
                .await?
                .checkpoints
                .into_iter()
                .find(|checkpoint| checkpoint.id == checkpoint_id)
                .ok_or_else(|| anyhow::anyhow!("checkpoint not found: {checkpoint_id}"))?
        } else {
            self.checkpoint_session(source_session_id, None).await?
        };
        let store = self.store.clone();
        let project_id = source.project_id.clone();
        let project = tokio::task::spawn_blocking(move || store.get_project(&project_id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("project not found: {}", source.project_id))?;
        let goal = goal
            .map(|goal| goal.trim().to_string())
            .filter(|goal| !goal.is_empty())
            .unwrap_or_else(|| format!("{} (fork)", source.goal));
        let session_id = Uuid::now_v7().to_string();
        let root = PathBuf::from(&project.root);
        let data_dir = self.store.data_dir().to_path_buf();
        let project_id_for_worktree = project.id.clone();
        let session_id_for_worktree = session_id.clone();
        let commit = checkpoint.commit.clone();
        let git_lock = self.git_lock.clone();
        let worktree = tokio::task::spawn_blocking(move || {
            let _guard = git_lock
                .lock()
                .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
            worktree::create_from_commit(
                &root,
                &data_dir,
                &project_id_for_worktree,
                &session_id_for_worktree,
                &commit,
            )
        })
        .await??;
        let worktree_path = worktree.path.to_string_lossy().to_string();
        let branch = worktree.branch.clone();
        let base_commit = worktree.base_commit.clone();
        let store = self.store.clone();
        let new_session = {
            let session_id = session_id.clone();
            let project_id = project.id.clone();
            let goal = goal.clone();
            let worktree_path = worktree_path.clone();
            let branch = branch.clone();
            let base_commit = base_commit.clone();
            let token_budget = source.token_budget;
            let cost_budget = source.cost_budget_usd;
            let time_budget = source.time_budget_secs;
            let priority = source.priority;
            tokio::task::spawn_blocking(move || {
                store.create_session_with_worktree(
                    &session_id,
                    &project_id,
                    &goal,
                    &worktree_path,
                    &branch,
                    token_budget,
                    cost_budget,
                    time_budget,
                    &base_commit,
                    priority,
                )
            })
            .await?
        };
        let new_session = match new_session {
            Ok(session) => session,
            Err(error) => {
                worktree::remove(&PathBuf::from(&project.root), &worktree.path, &branch);
                return Err(error);
            }
        };

        let created = serde_json::json!({
            "goal": new_session.goal,
            "project_id": new_session.project_id,
            "worktree": new_session.worktree,
            "branch": new_session.branch,
            "priority": new_session.priority,
            "token_budget": new_session.token_budget,
            "cost_budget_usd": new_session.cost_budget_usd,
            "time_budget_secs": new_session.time_budget_secs,
            "base_commit": new_session.base_commit,
            "created_at": new_session.created_at,
            "forked_from_session_id": source.id,
            "forked_from_checkpoint_id": checkpoint.id,
            "forked_from_commit": checkpoint.commit,
        });
        self.append_event(&new_session.id, "session.created", created)
            .await?;
        let source_leader = self
            .list_agents(source_session_id)
            .await?
            .into_iter()
            .find(|agent| agent.role == "leader");
        let model = source_leader
            .map(|agent| agent.model)
            .unwrap_or_else(|| "unconfigured".to_string());
        if let Err(error) = self.create_leader_agent(&new_session.id, &model).await {
            tracing::warn!(%error, session_id = %new_session.id, "could not seed forked leader agent");
        }
        self.append_event(
            &new_session.id,
            "session.forked",
            serde_json::json!({
                "source_session_id": source.id,
                "source_goal": source.goal,
                "checkpoint_id": checkpoint.id,
                "commit": checkpoint.commit,
                "message_count": checkpoint.message_count,
                "has_summary": checkpoint.summary.is_some(),
            }),
        )
        .await?;
        self.copy_checkpoint_messages(source_session_id, &new_session.id, &checkpoint)
            .await?;
        if let Err(error) = self
            .append_event(
                source_session_id,
                "session.forked",
                serde_json::json!({
                    "fork_session_id": new_session.id,
                    "fork_goal": new_session.goal,
                    "checkpoint_id": checkpoint.id,
                    "commit": checkpoint.commit,
                }),
            )
            .await
        {
            tracing::warn!(%error, session_id = %source.id, "could not record source fork event");
        }
        Ok(SessionForkResult {
            source_session_id: source.id,
            checkpoint_id: checkpoint.id,
            session: new_session,
        })
    }

    pub async fn session_fork_tree(&self, session_id: &str) -> Result<SessionForkTreeResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        let store = self.store.clone();
        let id = session_id.to_string();
        let created = tokio::task::spawn_blocking(move || {
            store.list_events_by_kind(&id, "session.created", 1)
        })
        .await??;
        let created_payload = created.first().map(|event| event.payload.clone());
        let checkpoint_id = created_payload
            .as_ref()
            .and_then(|payload| payload.get("forked_from_checkpoint_id"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let commit = created_payload
            .as_ref()
            .and_then(|payload| payload.get("forked_from_commit"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let total = Arc::new(AtomicUsize::new(1));
        let truncated = Arc::new(AtomicBool::new(false));
        let children = self
            .build_fork_children(session_id, 0, total.clone(), truncated.clone())
            .await?;
        let total = total.load(Ordering::Relaxed);
        let truncated = truncated.load(Ordering::Relaxed);
        Ok(SessionForkTreeResult {
            session_id: session.id.clone(),
            root: SessionForkTreeNode {
                session,
                checkpoint_id,
                commit,
                children,
            },
            total,
            truncated,
        })
    }

    #[async_recursion]
    async fn build_fork_children(
        &self,
        session_id: &str,
        depth: u32,
        total: Arc<AtomicUsize>,
        truncated: Arc<AtomicBool>,
    ) -> Result<Vec<SessionForkTreeNode>> {
        if depth >= 32 {
            truncated.store(true, Ordering::Relaxed);
            return Ok(Vec::new());
        }
        let store = self.store.clone();
        let id = session_id.to_string();
        let events = tokio::task::spawn_blocking(move || {
            store.list_events_by_kind(&id, "session.forked", 1000)
        })
        .await??;
        let mut nodes = Vec::new();
        for event in events {
            if total.load(Ordering::Relaxed) >= 200 {
                truncated.store(true, Ordering::Relaxed);
                break;
            }
            let Some(entry) = self.lineage_entry_from_fork_event(&event).await else {
                continue;
            };
            total.fetch_add(1, Ordering::Relaxed);
            let children = self
                .build_fork_children(
                    &entry.session.id,
                    depth + 1,
                    total.clone(),
                    truncated.clone(),
                )
                .await?;
            nodes.push(SessionForkTreeNode {
                session: entry.session,
                checkpoint_id: entry.checkpoint_id,
                commit: entry.commit,
                children,
            });
        }
        nodes.sort_by(|left, right| left.session.created_at.cmp(&right.session.created_at));
        Ok(nodes)
    }

    pub async fn session_lineage(&self, session_id: &str) -> Result<SessionLineageResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        let store = self.store.clone();
        let id = session_id.to_string();
        let created = tokio::task::spawn_blocking(move || {
            store.list_events_by_kind(&id, "session.created", 1)
        })
        .await??;
        let created_payload = created.first().map(|event| event.payload.clone());
        let parent_id = created_payload
            .as_ref()
            .and_then(|payload| payload.get("forked_from_session_id"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let parent_checkpoint = created_payload
            .as_ref()
            .and_then(|payload| payload.get("forked_from_checkpoint_id"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let parent_commit = created_payload
            .as_ref()
            .and_then(|payload| payload.get("forked_from_commit"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let mut parent = None;
        let mut siblings = Vec::new();
        if let Some(parent_id) = &parent_id {
            let store = self.store.clone();
            let parent_for_load = parent_id.clone();
            if let Some(parent_session) =
                tokio::task::spawn_blocking(move || store.get_session(&parent_for_load)).await??
            {
                parent = Some(SessionLineageEntry {
                    session: parent_session,
                    checkpoint_id: parent_checkpoint,
                    commit: parent_commit,
                });
            }
            let store = self.store.clone();
            let parent_for_events = parent_id.clone();
            let events = tokio::task::spawn_blocking(move || {
                store.list_events_by_kind(&parent_for_events, "session.forked", 1000)
            })
            .await??;
            for event in events {
                let Some(entry) = self.lineage_entry_from_fork_event(&event).await else {
                    continue;
                };
                if entry.session.id != session_id {
                    siblings.push(entry);
                }
            }
        }
        let store = self.store.clone();
        let id = session_id.to_string();
        let events = tokio::task::spawn_blocking(move || {
            store.list_events_by_kind(&id, "session.forked", 1000)
        })
        .await??;
        let mut children = Vec::new();
        for event in events {
            if let Some(entry) = self.lineage_entry_from_fork_event(&event).await {
                children.push(entry);
            }
        }
        siblings.sort_by(|left, right| left.session.created_at.cmp(&right.session.created_at));
        children.sort_by(|left, right| left.session.created_at.cmp(&right.session.created_at));
        Ok(SessionLineageResult {
            session_id: session.id,
            parent,
            siblings,
            children,
        })
    }

    async fn lineage_entry_from_fork_event(&self, event: &Event) -> Option<SessionLineageEntry> {
        let fork_id = event
            .payload
            .get("fork_session_id")
            .and_then(serde_json::Value::as_str)?
            .to_string();
        let checkpoint_id = event
            .payload
            .get("checkpoint_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let mut commit = event
            .payload
            .get("commit")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let store = self.store.clone();
        let session_id = fork_id.clone();
        let session =
            match tokio::task::spawn_blocking(move || store.get_session(&session_id)).await {
                Ok(Ok(Some(session))) => session,
                _ => return None,
            };
        if commit.is_none() {
            let store = self.store.clone();
            let session_id = fork_id.clone();
            if let Ok(Ok(events)) = tokio::task::spawn_blocking(move || {
                store.list_events_by_kind(&session_id, "session.created", 1)
            })
            .await
                && let Some(created) = events.first()
            {
                commit = created
                    .payload
                    .get("forked_from_commit")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
            }
        }
        Some(SessionLineageEntry {
            session,
            checkpoint_id,
            commit,
        })
    }

    pub async fn compare_sessions(
        &self,
        session_id: &str,
        other_session_id: &str,
    ) -> Result<SessionCompareResult> {
        if session_id == other_session_id {
            anyhow::bail!("choose two different sessions to compare");
        }
        let store = self.store.clone();
        let left_id = session_id.to_string();
        let right_id = other_session_id.to_string();
        let (left, right) = tokio::task::spawn_blocking(move || {
            let left = store
                .get_session(&left_id)?
                .ok_or_else(|| anyhow::anyhow!("session not found: {left_id}"))?;
            let right = store
                .get_session(&right_id)?
                .ok_or_else(|| anyhow::anyhow!("session not found: {right_id}"))?;
            Ok::<_, anyhow::Error>((left, right))
        })
        .await??;
        if left.project_id != right.project_id {
            anyhow::bail!("sessions must belong to the same project");
        }
        let left_branch = left
            .branch
            .clone()
            .ok_or_else(|| anyhow::anyhow!("source session has no branch"))?;
        let right_branch = right
            .branch
            .clone()
            .ok_or_else(|| anyhow::anyhow!("other session has no branch"))?;
        let store = self.store.clone();
        let project_id = left.project_id.clone();
        let project = tokio::task::spawn_blocking(move || store.get_project(&project_id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("project not found: {}", left.project_id))?;
        let root = PathBuf::from(&project.root);
        let git_lock = self.git_lock.clone();
        let left_branch_for_git = left_branch.clone();
        let right_branch_for_git = right_branch.clone();
        let root_for_git = root.clone();
        let (left_commit, right_commit, numstat, patch) = tokio::task::spawn_blocking(move || {
            let _guard = git_lock
                .lock()
                .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
            let left_commit =
                worktree::git_output(&root_for_git, &["rev-parse", &left_branch_for_git])?;
            let right_commit =
                worktree::git_output(&root_for_git, &["rev-parse", &right_branch_for_git])?;
            let range = format!("{left_commit}..{right_commit}");
            let numstat = worktree::git_output(
                &root_for_git,
                &["diff", "--numstat", "--no-color", "--no-ext-diff", &range],
            )?;
            let patch = worktree::git_output(
                &root_for_git,
                &["diff", "--no-color", "--no-ext-diff", &range],
            )?;
            Ok::<_, anyhow::Error>((left_commit, right_commit, numstat, patch))
        })
        .await??;
        let truncated = patch.chars().count() > 200_000;
        Ok(SessionCompareResult {
            session_id: session_id.to_string(),
            other_session_id: other_session_id.to_string(),
            left_commit,
            right_commit,
            files: parse_numstat(&numstat),
            patch: clip_transcript(&patch, 200_000),
            truncated,
        })
    }

    async fn load_active_snapshot(
        &self,
        session_id: &str,
    ) -> Result<(Vec<StoredMessage>, usize, Option<String>)> {
        let messages = self.list_messages(session_id).await?;
        let mut start = 0usize;
        let mut summary = None;
        if let Some(event) = self
            .latest_event_of_kind(session_id, "context.compacted")
            .await?
        {
            let mode = event
                .payload
                .get("mode")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("summary");
            let through = event
                .payload
                .get("through_message_id")
                .and_then(serde_json::Value::as_str);
            let text = event
                .payload
                .get("summary")
                .and_then(serde_json::Value::as_str)
                .filter(|text| !text.trim().is_empty());
            if let Some(through) = through
                && let Some(index) = messages.iter().position(|message| message.id == through)
            {
                start = index.saturating_add(1);
                if mode == "summary" {
                    summary = text.map(str::to_string);
                }
            }
        }
        Ok((messages, start, summary))
    }

    async fn copy_checkpoint_messages(
        &self,
        source_session_id: &str,
        target_session_id: &str,
        checkpoint: &SessionCheckpoint,
    ) -> Result<usize> {
        let messages = self.list_messages(source_session_id).await?;
        let mut tool_names = std::collections::HashMap::new();
        if let Some(summary) = &checkpoint.summary {
            self.append_message(
                target_session_id,
                "user",
                summary,
                serde_json::json!([]),
                None,
            )
            .await?;
        }
        let (start, end) = match (
            checkpoint.start_message_id.as_deref(),
            checkpoint.through_message_id.as_deref(),
        ) {
            (Some(start_id), Some(end_id)) => {
                let start = messages
                    .iter()
                    .position(|message| message.id == start_id)
                    .ok_or_else(|| anyhow::anyhow!("checkpoint start message is missing"))?;
                let end = messages
                    .iter()
                    .position(|message| message.id == end_id)
                    .ok_or_else(|| anyhow::anyhow!("checkpoint through message is missing"))?;
                if end < start {
                    anyhow::bail!("checkpoint message range is invalid");
                }
                (start, end)
            }
            (None, None) => return Ok(0),
            _ => anyhow::bail!("checkpoint message range is incomplete"),
        };

        let mut copied = 0usize;
        for message in &messages[start..=end] {
            let tool_calls = message.tool_calls.clone();
            self.append_message(
                target_session_id,
                &message.role,
                &message.content,
                tool_calls.clone(),
                message.tool_call_id.clone(),
            )
            .await?;
            if message.role == "assistant" {
                if !message.content.trim().is_empty() {
                    self.append_event(
                        target_session_id,
                        "agent.message",
                        serde_json::json!({
                            "text": message.content,
                            "agent_role": "leader",
                            "agent_name": "Leader",
                            "forked": true,
                        }),
                    )
                    .await?;
                }
                if let Some(calls) = tool_calls.as_array() {
                    for call in calls {
                        let Some(id) = call.get("id").and_then(serde_json::Value::as_str) else {
                            continue;
                        };
                        let name = call
                            .get("name")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("tool");
                        let arguments = call
                            .get("arguments")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("");
                        tool_names.insert(id.to_string(), name.to_string());
                        let input = serde_json::from_str::<serde_json::Value>(arguments)
                            .unwrap_or(serde_json::Value::Null);
                        self.append_event(
                            target_session_id,
                            "tool.started",
                            serde_json::json!({
                                "tool_call_id": id,
                                "name": name,
                                "input": input,
                                "agent_role": "leader",
                                "agent_name": "Leader",
                                "forked": true,
                            }),
                        )
                        .await?;
                    }
                }
            } else if message.role == "tool"
                && let Some(tool_call_id) = &message.tool_call_id
            {
                let name = tool_names
                    .get(tool_call_id)
                    .cloned()
                    .unwrap_or_else(|| "tool".to_string());
                self.append_event(
                    target_session_id,
                    "tool.finished",
                    serde_json::json!({
                        "tool_call_id": tool_call_id,
                        "name": name,
                        "output": message.content,
                        "is_error": false,
                        "agent_role": "leader",
                        "agent_name": "Leader",
                        "forked": true,
                    }),
                )
                .await?;
            }
            copied += 1;
        }
        Ok(copied)
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
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        self.scheduler.sync_plan(&session, plan).await?;
        Ok(())
    }

    pub async fn task_queue(
        &self,
        project_id: Option<String>,
        session_id: Option<String>,
    ) -> Result<TaskQueueResult> {
        self.scheduler.queue(project_id, session_id).await
    }

    pub async fn write_leases(
        &self,
        project_id: Option<String>,
        session_id: Option<String>,
    ) -> Result<Vec<WriteLease>> {
        self.scheduler.leases(project_id, session_id).await
    }

    pub async fn retry_plan_task(&self, session_id: &str, task_id: &str) -> Result<ScheduledTask> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let task_id = task_id.to_string();
        let (task, event) = tokio::task::spawn_blocking(move || {
            store.retry_plan_task_with_event(&session_id, &task_id)
        })
        .await??;
        let _ = self.events.send(event);
        self.scheduler.notify_state();
        Ok(task)
    }

    pub async fn skip_plan_task(&self, session_id: &str, task_id: &str) -> Result<ScheduledTask> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let task_id = task_id.to_string();
        let (task, event) = tokio::task::spawn_blocking(move || {
            store.skip_plan_task_with_event(&session_id, &task_id)
        })
        .await??;
        let _ = self.events.send(event);
        self.scheduler.notify_state();
        Ok(task)
    }

    pub async fn request_plan_task_cancel(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<ScheduledTask> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let task_id = task_id.to_string();
        let (task, event) = tokio::task::spawn_blocking(move || {
            store.request_plan_task_cancel_with_event(&session_id, &task_id)
        })
        .await??;
        let _ = self.events.send(event);
        self.scheduler.notify_state();
        Ok(task)
    }

    pub async fn pause_plan_task(&self, session_id: &str, task_id: &str) -> Result<ScheduledTask> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let task_id = task_id.to_string();
        let (task, event) = tokio::task::spawn_blocking(move || {
            store.pause_plan_task_with_event(&session_id, &task_id)
        })
        .await??;
        let _ = self.events.send(event);
        self.scheduler.notify_state();
        Ok(task)
    }

    pub async fn mark_plan_task_paused(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<Option<ScheduledTask>> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let task_id = task_id.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.mark_plan_task_paused_with_event(&session_id, &task_id)
        })
        .await??;
        let Some((task, event)) = result else {
            return Ok(None);
        };
        let _ = self.events.send(event);
        self.scheduler.notify_state();
        Ok(Some(task))
    }

    pub async fn resume_plan_task(&self, session_id: &str, task_id: &str) -> Result<ScheduledTask> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let task_id = task_id.to_string();
        let (task, event) = tokio::task::spawn_blocking(move || {
            store.resume_plan_task_with_event(&session_id, &task_id)
        })
        .await??;
        let _ = self.events.send(event);
        self.scheduler.notify_state();
        Ok(task)
    }

    pub async fn plan_task_status(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<Option<String>> {
        self.scheduler.plan_task_status(session_id, task_id).await
    }

    pub async fn plan_task_cancel_requested(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<bool> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let task_id = task_id.to_string();
        let status =
            tokio::task::spawn_blocking(move || store.plan_task_status(&session_id, &task_id))
                .await??;
        Ok(matches!(
            status.as_deref(),
            Some("cancelling" | "cancelled")
        ))
    }

    pub async fn interrupt_running_plan_tasks(&self, session_id: &str) -> Result<usize> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let tasks = tokio::task::spawn_blocking(move || {
            store.interrupt_session_plan_tasks(&id, "the session ended before the task finished")
        })
        .await??;
        let count = tasks.len();
        for task in tasks {
            self.append_event(
                session_id,
                "plan_task.interrupted",
                serde_json::json!({
                    "task_id": task.task_id,
                    "task_status": "interrupted",
                    "reason": "the session ended before the task finished",
                }),
            )
            .await?;
        }
        Ok(count)
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
        let kind = match std::env::var("VERLINPARK_PR_PROVIDER")
            .ok()
            .map(|value| value.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("gitlab") => PrCli::GitLab,
            Some("github") => PrCli::GitHub,
            Some(other) => anyhow::bail!("unsupported PR provider: {other}"),
            None => detect_pr_cli(&self.pull_request_origin(session_id).await?),
        };
        let bin = match kind {
            PrCli::GitHub => std::env::var("VERLINPARK_GH_BIN").unwrap_or_else(|_| "gh".into()),
            PrCli::GitLab => std::env::var("VERLINPARK_GLAB_BIN").unwrap_or_else(|_| "glab".into()),
        };
        self.open_pull_request_with_cli(session_id, &bin, kind)
            .await
    }

    async fn pull_request_origin(&self, session_id: &str) -> Result<String> {
        let session = self.ensure_session_worktree(session_id).await?;
        let store = self.store.clone();
        let project_id = session.project_id;
        let project = tokio::task::spawn_blocking(move || store.get_project(&project_id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("project not found"))?;
        let root = PathBuf::from(&project.root);
        tokio::task::spawn_blocking(move || {
            git_output(&root, &["remote", "get-url", "origin"])
                .map(|origin| origin.trim().to_string())
                .context("a git origin remote is required to open a PR")
        })
        .await?
    }

    async fn open_pull_request_with_cli(
        &self,
        session_id: &str,
        cli_bin: &str,
        kind: PrCli,
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
        let cli_bin = cli_bin.to_string();
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
        let title_for_cli = title.clone();
        let body_for_cli = body.clone();
        let cli_bin_for_create = cli_bin.clone();
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
            let auth = std::process::Command::new(&cli_bin)
                .args(kind.auth_args())
                .current_dir(&project_root)
                .output()
                .with_context(|| format!("running {} auth status", kind.binary_name()))?;
            if !auth.status.success() {
                anyhow::bail!(
                    "{} is not authenticated: {}",
                    kind.binary_name(),
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

        let project_root_for_cli = PathBuf::from(&project.root);
        let branch_for_pr = branch.clone();
        let base_for_pr = base_branch.clone();
        let url = tokio::task::spawn_blocking(move || -> Result<String> {
            let output = std::process::Command::new(&cli_bin_for_create)
                .args(kind.create_args(&base_for_pr, &branch_for_pr, &title_for_cli, &body_for_cli))
                .current_dir(&project_root_for_cli)
                .output()
                .with_context(|| format!("running {}", kind.create_label()))?;
            if !output.status.success() {
                anyhow::bail!(
                    "branch pushed, but {} failed: {}",
                    kind.create_label(),
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
            let stdout = String::from_utf8_lossy(&output.stdout);
            let url = stdout
                .lines()
                .map(str::trim)
                .find(|line| line.starts_with("http"))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "{} succeeded but did not return a URL: {stdout}",
                        kind.create_label()
                    )
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
            detail: format!("branch pushed and {} opened at {url}", kind.change_name()),
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

    pub async fn usage_csv(&self, session_id: &str) -> Result<UsageCsvResult> {
        let ledger = self.usage_ledger(session_id).await?;
        let mut csv = String::from(
            "created_at,agent_name,role,task_id,department,model,input_tokens,output_tokens,total_tokens,estimated,cost_usd\n",
        );
        for entry in ledger.entries {
            let fields = [
                entry.created_at,
                entry.agent_name,
                entry.role,
                entry.task_id.unwrap_or_default(),
                entry.department.unwrap_or_default(),
                entry.model,
                entry.input_tokens.to_string(),
                entry.output_tokens.to_string(),
                (entry.input_tokens + entry.output_tokens).to_string(),
                entry.estimated.to_string(),
                format!("{:.6}", entry.cost_usd),
            ];
            csv.push_str(
                &fields
                    .iter()
                    .map(|field| csv_field(field))
                    .collect::<Vec<_>>()
                    .join(","),
            );
            csv.push('\n');
        }
        Ok(UsageCsvResult {
            session_id: session_id.to_string(),
            csv,
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

    pub async fn get_project_budget(&self, project_id: &str) -> Result<ProjectBudgetResult> {
        let store = self.store.clone();
        let id = project_id.to_string();
        let (budget, usage) = tokio::task::spawn_blocking(move || {
            let budget = store.get_project_budget(&id)?;
            let usage = store.project_usage(&id)?;
            Ok::<_, anyhow::Error>((budget, usage))
        })
        .await??;
        Ok(ProjectBudgetResult {
            project_id: project_id.to_string(),
            budget,
            spent_tokens: usage.0,
            spent_cost_usd: usage.1,
        })
    }

    pub async fn set_project_budget(
        &self,
        project_id: &str,
        budget: ProjectBudget,
    ) -> Result<ProjectBudgetResult> {
        if budget.token_budget.is_some_and(|value| value == 0) {
            anyhow::bail!("project token budget must be greater than zero");
        }
        if budget
            .cost_budget_usd
            .is_some_and(|value| !value.is_finite() || value <= 0.0)
        {
            anyhow::bail!("project cost budget must be a positive finite number");
        }
        let store = self.store.clone();
        let project_id_owned = project_id.to_string();
        tokio::task::spawn_blocking({
            let store = store.clone();
            let project_id = project_id_owned.clone();
            move || store.get_project(&project_id)
        })
        .await??
        .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        let for_store = budget.clone();
        tokio::task::spawn_blocking(move || {
            store.set_project_budget(&project_id_owned, &for_store)
        })
        .await??;
        self.get_project_budget(project_id).await
    }

    pub async fn record_project_manifest(&self) -> Result<()> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.record_project_manifest()).await?
    }

    pub async fn project_usage_report(&self, project_id: &str) -> Result<ProjectUsageResult> {
        let store = self.store.clone();
        let id = project_id.to_string();
        let sessions = tokio::task::spawn_blocking(move || {
            store
                .get_project(&id)?
                .ok_or_else(|| anyhow::anyhow!("project not found: {id}"))?;
            store.project_usage_by_session(&id)
        })
        .await??;
        let mut totals = UsageTotals::default();
        for session in &sessions {
            totals.calls = totals.calls.saturating_add(session.calls);
            totals.input_tokens = totals.input_tokens.saturating_add(session.input_tokens);
            totals.output_tokens = totals.output_tokens.saturating_add(session.output_tokens);
            totals.cost_usd += session.cost_usd;
        }
        Ok(ProjectUsageResult {
            project_id: project_id.to_string(),
            sessions,
            totals,
        })
    }

    pub async fn project_usage(&self, project_id: &str) -> Result<(u64, f64)> {
        let store = self.store.clone();
        let id = project_id.to_string();
        tokio::task::spawn_blocking(move || store.project_usage(&id)).await?
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
            let sha256 = artifacts
                .iter()
                .find(|artifact| &artifact.id == id)
                .map(|artifact| artifact.sha256.clone())
                .unwrap_or_default();
            let store = self.store.clone();
            let id = id.clone();
            let pruned = tokio::task::spawn_blocking(move || store.delete_artifact_with_event(&id))
                .await??;
            if let Some((event, remove_path)) = pruned {
                let _ = self.events.send(event);
                if let Some(path) = remove_path {
                    let physical_size = tokio::fs::metadata(&path)
                        .await
                        .map(|metadata| metadata.len())
                        .unwrap_or(0);
                    match tokio::fs::remove_file(&path).await {
                        Ok(()) => bytes_freed = bytes_freed.saturating_add(physical_size),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => {
                            tracing::warn!(%error, path, "failed to remove artifact blob");
                        }
                    }
                    if !sha256.is_empty() {
                        let materialized = self
                            .store()
                            .data_dir()
                            .join("artifacts")
                            .join("materialized")
                            .join(&sha256);
                        let _ = tokio::fs::remove_file(materialized).await;
                    }
                }
            }
        }
        Ok(ArtifactGcResult {
            removed: remove.len(),
            bytes_freed,
        })
    }

    pub async fn backfill_artifact_events(
        &self,
        session_id: &str,
    ) -> Result<ArtifactBackfillResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let (artifacts, events) = tokio::task::spawn_blocking(move || {
            let artifacts = store.list_artifacts(&id)?;
            let events = store.list_all_events(&id)?;
            Ok::<_, anyhow::Error>((artifacts, events))
        })
        .await??;
        let mut full = std::collections::HashSet::new();
        for event in &events {
            if event.kind == "artifact.published"
                && event.payload.get("mime_type").is_some()
                && event.payload.get("sha256").is_some()
                && event.payload.get("content").is_some()
                && let Some(artifact_id) = event
                    .payload
                    .get("artifact_id")
                    .and_then(serde_json::Value::as_str)
            {
                full.insert(artifact_id.to_string());
            }
        }
        let mut backfilled = 0;
        for artifact in &artifacts {
            if full.contains(&artifact.id) {
                continue;
            }
            self.append_event(
                session_id,
                "artifact.published",
                artifact_event_payload(artifact),
            )
            .await?;
            backfilled += 1;
        }
        Ok(ArtifactBackfillResult {
            session_id: session_id.to_string(),
            scanned: artifacts.len(),
            backfilled,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_memory_as(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        kind: &str,
        title: &str,
        body: &str,
        tags: Vec<String>,
    ) -> Result<MemoryItem> {
        let kind = kind.trim();
        let title = title.trim();
        if kind.is_empty() || kind.chars().count() > 40 {
            anyhow::bail!("memory kind must be 1-40 characters");
        }
        if title.is_empty() || title.chars().count() > 200 {
            anyhow::bail!("memory title must be 1-200 characters");
        }
        if body.trim().is_empty() || body.chars().count() > 20_000 {
            anyhow::bail!("memory body must be 1-20000 characters");
        }
        if tags.len() > 10 || tags.iter().any(|tag| tag.chars().count() > 40) {
            anyhow::bail!("at most 10 tags of 40 characters are allowed");
        }
        let store = self.store.clone();
        let session_id_owned = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&session_id_owned))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        let store = self.store.clone();
        let session_id = session.id;
        let project_id = session.project_id;
        let agent_id = agent_id.to_string();
        let agent_name = agent_name.to_string();
        let kind = kind.to_string();
        let title = title.to_string();
        let body = body.to_string();
        let (memory, event) = tokio::task::spawn_blocking(move || {
            store.create_memory_with_event(
                &session_id,
                &project_id,
                &agent_id,
                &agent_name,
                &kind,
                &title,
                &body,
                &tags,
            )
        })
        .await??;
        let _ = self.events.send(event);
        Ok(memory)
    }

    pub async fn create_memory(
        &self,
        session_id: &str,
        kind: &str,
        title: &str,
        body: &str,
        tags: Vec<String>,
    ) -> Result<MemoryItem> {
        self.create_memory_as(session_id, "user", "User", kind, title, body, tags)
            .await
    }

    pub async fn list_memories(
        &self,
        project_id: &str,
        query: Option<String>,
        kind: Option<String>,
        limit: u32,
    ) -> Result<MemoryListResult> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        let memories = tokio::task::spawn_blocking(move || {
            store.list_memories(&project_id, query.as_deref(), kind.as_deref(), limit)
        })
        .await??;
        Ok(MemoryListResult { memories })
    }

    pub async fn delete_memory(&self, id: &str) -> Result<MemoryDeleteResult> {
        let store = self.store.clone();
        let memory_id = id.to_string();
        let lookup = memory_id.clone();
        let removed =
            tokio::task::spawn_blocking(move || store.delete_memories_with_events(&[lookup]))
                .await??;
        for (_, event) in &removed {
            let _ = self.events.send(event.clone());
        }
        Ok(MemoryDeleteResult {
            memory_id,
            removed: !removed.is_empty(),
        })
    }

    pub async fn prune_memories(
        &self,
        project_id: &str,
        older_than_days: Option<u32>,
        keep_latest: Option<u32>,
        dry_run: bool,
    ) -> Result<MemoryPruneResult> {
        if older_than_days.is_none() && keep_latest.is_none() {
            anyhow::bail!("memory prune requires older_than_days or keep_latest");
        }
        let store = self.store.clone();
        let project = project_id.to_string();
        let ids = tokio::task::spawn_blocking(move || {
            store.memory_ids_for_prune(&project, older_than_days, keep_latest)
        })
        .await??;
        let candidates = ids.len();
        if dry_run || ids.is_empty() {
            return Ok(MemoryPruneResult {
                candidates,
                removed: 0,
                dry_run,
            });
        }
        let store = self.store.clone();
        let removed =
            tokio::task::spawn_blocking(move || store.delete_memories_with_events(&ids)).await??;
        for (_, event) in &removed {
            let _ = self.events.send(event.clone());
        }
        Ok(MemoryPruneResult {
            candidates,
            removed: removed.len(),
            dry_run: false,
        })
    }

    pub async fn get_memory_policy(&self, project_id: &str) -> Result<MemoryPolicyResult> {
        let store = self.store.clone();
        let project = project_id.to_string();
        let policy =
            tokio::task::spawn_blocking(move || store.get_memory_policy(&project)).await??;
        Ok(MemoryPolicyResult {
            project_id: project_id.to_string(),
            policy,
        })
    }

    pub async fn set_memory_policy(
        &self,
        project_id: &str,
        policy: MemoryPolicy,
    ) -> Result<MemoryPolicyResult> {
        validate_memory_policy(&policy)?;
        let store = self.store.clone();
        let project = project_id.to_string();
        let stored = policy.clone();
        tokio::task::spawn_blocking(move || store.set_memory_policy(&project, &stored)).await??;
        Ok(MemoryPolicyResult {
            project_id: project_id.to_string(),
            policy,
        })
    }

    pub async fn sweep_memory_retention(&self) -> Result<usize> {
        let store = self.store.clone();
        let policies =
            tokio::task::spawn_blocking(move || store.list_enabled_memory_policies()).await??;
        let mut removed = 0;
        for (project_id, policy) in policies {
            if policy.max_items.is_none() && policy.max_age_days.is_none() {
                continue;
            }
            match self
                .prune_memories(&project_id, policy.max_age_days, policy.max_items, false)
                .await
            {
                Ok(result) => removed += result.removed,
                Err(error) => {
                    tracing::warn!(%error, %project_id, "memory retention sweep failed");
                }
            }
        }
        Ok(removed)
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

    pub async fn materialize_artifact(
        &self,
        artifact_id: &str,
    ) -> Result<Option<ArtifactMaterialization>> {
        let store = self.store.clone();
        let id = artifact_id.to_string();
        let path = tokio::task::spawn_blocking(move || store.materialize_artifact(&id)).await??;
        Ok(path.map(|path| ArtifactMaterialization {
            artifact_id: artifact_id.to_string(),
            path: path.to_string_lossy().to_string(),
        }))
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

    pub async fn session_transcript(&self, session_id: &str) -> Result<SessionTranscriptResult> {
        use std::fmt::Write as _;

        let store = self.store.clone();
        let id = session_id.to_string();
        let (session, project, plan, messages) =
            tokio::task::spawn_blocking(move || -> Result<_> {
                let session = store
                    .get_session(&id)?
                    .ok_or_else(|| anyhow::anyhow!("session not found: {id}"))?;
                let project = store
                    .get_project(&session.project_id)?
                    .ok_or_else(|| anyhow::anyhow!("project not found: {}", session.project_id))?;
                let plan = store
                    .get_team_plan(&id)?
                    .and_then(|raw| serde_json::from_str::<TeamPlan>(&raw).ok());
                let messages = store.list_messages(&id)?;
                Ok((session, project, plan, messages))
            })
            .await??;

        let mut out = String::new();
        let _ = writeln!(out, "# {}\n", session.goal);
        let _ = writeln!(out, "- **Session:** `{}`", session.id);
        let _ = writeln!(out, "- **Project:** {} (`{}`)", project.name, project.root);
        let _ = writeln!(out, "- **Status:** {}", session.status);
        let _ = writeln!(out, "- **Created:** {}", session.created_at);
        if let Some(branch) = &session.branch {
            let _ = writeln!(out, "- **Branch:** `{branch}`");
        }
        if let Some(worktree) = &session.worktree {
            let _ = writeln!(out, "- **Worktree:** `{worktree}`");
        }
        let _ = writeln!(
            out,
            "- **Tokens:** {} in / {} out",
            session.tokens_in, session.tokens_out
        );
        let _ = writeln!(out, "- **Cost:** ${:.4}", session.cost_spent_usd);

        if let Some(plan) = &plan {
            let _ = writeln!(out, "\n## Team Plan\n");
            for department in &plan.departments {
                let _ = writeln!(out, "### {} — {}", department.name, department.objective);
                for task in plan
                    .tasks
                    .iter()
                    .filter(|task| task.department == department.name)
                {
                    let mut details = vec![task.kind.clone()];
                    if let Some(profile) = &task.profile {
                        details.push(profile.clone());
                    }
                    if task.priority != 0 {
                        details.push(format!("priority {}", task.priority));
                    }
                    if !task.deps.is_empty() {
                        details.push(format!("deps: {}", task.deps.join(", ")));
                    }
                    if !task.write_scope.is_empty() {
                        details.push(format!("scope: {}", task.write_scope.join(", ")));
                    }
                    let _ = writeln!(
                        out,
                        "- **{}** {} · {}",
                        task.id,
                        task.title,
                        details.join(" · ")
                    );
                }
            }
        }

        let _ = writeln!(out, "\n## Conversation\n");
        if messages.is_empty() {
            let _ = writeln!(out, "_No messages yet._");
        }
        for message in messages {
            let _ = writeln!(out, "### {} · {}\n", message.role, message.created_at);
            let _ = writeln!(out, "{}", clip_transcript(&message.content, 8_000));
            if let Some(calls) = message.tool_calls.as_array() {
                for call in calls {
                    let name = call["name"].as_str().unwrap_or("tool");
                    let arguments = call["arguments"].as_str().unwrap_or("");
                    let _ = writeln!(
                        out,
                        "\n- `{}` `{}`",
                        name,
                        clip_transcript(arguments, 1_000)
                    );
                }
            }
            let _ = writeln!(out);
        }
        Ok(SessionTranscriptResult {
            session_id: session.id,
            markdown: out,
        })
    }

    pub async fn search(
        &self,
        query: &str,
        project_id: Option<&str>,
        limit: u32,
    ) -> Result<SearchResult> {
        let query = query.trim();
        if query.is_empty() {
            anyhow::bail!("search query is required");
        }
        if query.chars().count() > 200 {
            anyhow::bail!("search query must be at most 200 characters");
        }
        let limit = limit.clamp(1, 100);
        let store = self.store.clone();
        let query_owned = query.to_string();
        let project_owned = project_id.map(str::to_string);
        let hits = tokio::task::spawn_blocking(move || {
            store.search(
                &query_owned,
                project_owned.as_deref(),
                limit.saturating_add(1),
            )
        })
        .await??;
        let truncated = hits.len() > limit as usize;
        let hits = hits.into_iter().take(limit as usize).collect();
        Ok(SearchResult {
            query: query.to_string(),
            hits,
            truncated,
        })
    }

    pub async fn latest_event_of_kind(
        &self,
        session_id: &str,
        kind: &str,
    ) -> Result<Option<Event>> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let kind = kind.to_string();
        tokio::task::spawn_blocking(move || store.latest_event_by_kind(&session_id, &kind)).await?
    }

    pub async fn session_context_status(&self, session_id: &str) -> Result<SessionContextStatus> {
        let usage_event = self
            .latest_event_of_kind(session_id, "context.usage")
            .await?;
        let compaction_event = self
            .latest_event_of_kind(session_id, "context.compacted")
            .await?;
        let usage = usage_event.and_then(|event| {
            serde_json::from_value::<ContextUsage>(event.payload.clone())
                .ok()
                .map(|mut usage| {
                    usage.created_at = event.created_at;
                    usage
                })
        });
        let last_compaction = compaction_event.and_then(|event| {
            serde_json::from_value::<ContextCompactionSummary>(event.payload.clone())
                .ok()
                .map(|mut summary| {
                    summary.created_at = event.created_at;
                    summary
                })
        });
        let measurement_event = self
            .latest_event_of_kind(session_id, "context.measured")
            .await?;
        let last_measurement = measurement_event.and_then(|event| {
            serde_json::from_value::<ContextMeasurement>(event.payload.clone())
                .ok()
                .map(|mut measurement| {
                    measurement.created_at = event.created_at;
                    measurement
                })
        });
        Ok(SessionContextStatus {
            session_id: session_id.to_string(),
            usage,
            last_compaction,
            last_measurement,
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
            let memories = store.list_session_memories(&id)?;
            let questions = store.list_session_questions(&id)?;
            let tasks = store.list_plan_tasks(None, Some(&id))?;
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
                memories,
                questions,
                tasks,
                events,
            })
        })
        .await?
    }

    /// Reconstruct task-DAG rows from the active plan event and the plan
    /// lifecycle events after it. Timestamps are derived from event times;
    /// `same_expected_task` deliberately compares only durable scheduling state
    /// so a live claim/finish timestamp cannot create false drift.
    fn expected_tasks(data: &ReplayData) -> Option<Vec<ScheduledTask>> {
        let plan_event = data
            .events
            .iter()
            .rev()
            .find(|event| event.kind == "team_plan.submitted");
        let (plan_seq, plan, enqueued_at) = match plan_event {
            Some(event) => {
                let plan = serde_json::from_value::<TeamPlan>(event.payload.clone()).ok()?;
                (event.seq, plan, event.created_at.clone())
            }
            None => {
                let Some(plan) = data
                    .plan
                    .as_deref()
                    .and_then(|plan| serde_json::from_str::<TeamPlan>(plan).ok())
                else {
                    return data.tasks.is_empty().then(Vec::new);
                };
                (0, plan, data.session.created_at.clone())
            }
        };
        let mut tasks = plan
            .tasks
            .iter()
            .map(|task| {
                (
                    task.id.clone(),
                    ScheduledTask {
                        session_id: data.session.id.clone(),
                        project_id: data.session.project_id.clone(),
                        plan_seq,
                        task_id: task.id.clone(),
                        department: task.department.clone(),
                        title: task.title.clone(),
                        priority: task.priority,
                        status: "pending".to_string(),
                        deps: task.deps.clone(),
                        write_scope: task.write_scope.clone(),
                        attempts: 0,
                        enqueued_at: enqueued_at.clone(),
                        started_at: None,
                        finished_at: None,
                        last_error: None,
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        for event in data.events.iter().filter(|event| event.seq > plan_seq) {
            if !matches!(
                event.kind.as_str(),
                "plan_task.started"
                    | "plan_task.finished"
                    | "plan_task.interrupted"
                    | "plan_task.retried"
                    | "plan_task.skipped"
                    | "plan_task.cancelling"
                    | "plan_task.pausing"
                    | "plan_task.paused"
                    | "plan_task.resumed"
            ) {
                continue;
            }
            let Some(task_id) = event.payload["task_id"].as_str() else {
                continue;
            };
            let Some(task) = tasks.get_mut(task_id) else {
                continue;
            };
            match event.kind.as_str() {
                "plan_task.started" => {
                    if task.status != "completed" {
                        task.status = "interrupted".to_string();
                        task.finished_at = None;
                        task.last_error = None;
                    }
                    task.attempts = task.attempts.saturating_add(1);
                    task.started_at = Some(event.created_at.clone());
                }
                "plan_task.finished" => {
                    let status = event.payload["task_status"]
                        .as_str()
                        .unwrap_or("failed")
                        .to_string();
                    task.last_error = if status == "completed" {
                        None
                    } else {
                        event.payload["reason"]
                            .as_str()
                            .or_else(|| event.payload["summary"].as_str())
                            .map(str::to_string)
                    };
                    task.status = status;
                    task.finished_at = Some(event.created_at.clone());
                }
                "plan_task.interrupted" => {
                    task.status = "interrupted".to_string();
                    task.finished_at = Some(event.created_at.clone());
                    task.last_error = event.payload["reason"].as_str().map(str::to_string);
                }
                "plan_task.retried" => {
                    task.status = "pending".to_string();
                    task.finished_at = None;
                    task.last_error = None;
                }
                "plan_task.skipped" => {
                    task.status = "skipped".to_string();
                    task.finished_at = Some(event.created_at.clone());
                    task.last_error = event.payload["reason"].as_str().map(str::to_string);
                }
                "plan_task.cancelling" => {
                    task.status = "cancelling".to_string();
                    task.finished_at = None;
                    task.last_error = event.payload["reason"].as_str().map(str::to_string);
                }
                "plan_task.pausing" => {
                    task.status = "pausing".to_string();
                    task.finished_at = None;
                    task.last_error = event.payload["reason"].as_str().map(str::to_string);
                }
                "plan_task.paused" => {
                    task.status = "paused".to_string();
                    task.finished_at = None;
                    task.last_error = event.payload["reason"].as_str().map(str::to_string);
                }
                "plan_task.resumed" => {
                    task.status = "running".to_string();
                    task.finished_at = None;
                    task.last_error = None;
                }
                _ => {}
            }
        }
        let mut tasks = tasks.into_values().collect::<Vec<_>>();
        tasks.sort_by(|left, right| left.task_id.cmp(&right.task_id));
        Some(tasks)
    }

    fn same_expected_task(left: &ScheduledTask, right: &ScheduledTask) -> bool {
        left.session_id == right.session_id
            && left.project_id == right.project_id
            && left.plan_seq == right.plan_seq
            && left.task_id == right.task_id
            && left.department == right.department
            && left.title == right.title
            && left.priority == right.priority
            && left.status == right.status
            && left.deps == right.deps
            && left.write_scope == right.write_scope
            && left.attempts == right.attempts
            && left.enqueued_at == right.enqueued_at
    }

    fn expected_approvals(data: &ReplayData) -> Vec<Approval> {
        let mut approvals = HashMap::<String, Approval>::new();
        for event in &data.events {
            match event.kind.as_str() {
                "approval.requested" => {
                    if let Ok(approval) = serde_json::from_value::<Approval>(event.payload.clone())
                    {
                        approvals.insert(approval.id.clone(), approval);
                    }
                }
                "approval.resolved" => {
                    if let Some(approval_id) = event
                        .payload
                        .get("approval_id")
                        .and_then(serde_json::Value::as_str)
                        && let Some(approval) = approvals.get_mut(approval_id)
                    {
                        approval.status = if event
                            .payload
                            .get("approved")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false)
                        {
                            "approved".to_string()
                        } else {
                            "denied".to_string()
                        };
                        approval.resolved_at = event
                            .payload
                            .get("resolved_at")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                            .or_else(|| Some(event.created_at.clone()));
                    }
                }
                _ => {}
            }
        }
        let mut approvals = approvals.into_values().collect::<Vec<_>>();
        approvals.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        approvals
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
                        compression: event
                            .payload
                            .get("compression")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("none")
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

        let replay_questions = data
            .events
            .iter()
            .filter(|event| {
                matches!(
                    event.kind.as_str(),
                    "question.requested"
                        | "question.answered"
                        | "question.cancelled"
                        | "question.expired"
                )
            })
            .filter_map(|event| {
                let question = serde_json::from_value::<Question>(event.payload.clone()).ok()?;
                Some((question.id.clone(), serde_json::to_value(question).ok()?))
            })
            .collect::<HashMap<_, _>>();
        let projected_questions = data
            .questions
            .iter()
            .filter_map(|question| {
                Some((question.id.clone(), serde_json::to_value(question).ok()?))
            })
            .collect::<HashMap<_, _>>();
        let questions_matches = replay_questions == projected_questions;
        if !questions_matches {
            mismatches.push(format!(
                "questions: projection={}, replay={}",
                projected_questions.len(),
                replay_questions.len()
            ));
        }

        let expected_tasks = Self::expected_tasks(data);
        let task_queue_matches = match &expected_tasks {
            Some(expected) => {
                expected.len() == data.tasks.len()
                    && expected.iter().all(|expected| {
                        data.tasks
                            .iter()
                            .find(|task| task.task_id == expected.task_id)
                            .is_some_and(|projected| Self::same_expected_task(expected, projected))
                    })
            }
            None => false,
        };
        if !task_queue_matches {
            mismatches.push(format!(
                "task queue: projection={}, replay={}",
                data.tasks.len(),
                expected_tasks.as_ref().map(Vec::len).unwrap_or(0)
            ));
        }

        let mut replay_memories = HashMap::new();
        for event in &data.events {
            match event.kind.as_str() {
                "memory.created" => {
                    if let Ok(memory) = serde_json::from_value::<MemoryItem>(event.payload.clone())
                    {
                        replay_memories.insert(memory.id.clone(), serde_json::to_value(memory)?);
                    }
                }
                "memory.deleted" => {
                    if let Ok(memory) = serde_json::from_value::<MemoryItem>(event.payload.clone())
                    {
                        replay_memories.remove(&memory.id);
                    }
                }
                _ => {}
            }
        }
        let projected_memories = data
            .memories
            .iter()
            .filter_map(|memory| Some((memory.id.clone(), serde_json::to_value(memory).ok()?)))
            .collect::<HashMap<_, _>>();
        let memories_matches = replay_memories == projected_memories;
        if !memories_matches {
            mismatches.push(format!(
                "memories: projection={}, replay={}",
                projected_memories.len(),
                replay_memories.len()
            ));
        }

        let expected_approvals = Self::expected_approvals(data);
        let expected_approval_values = expected_approvals
            .iter()
            .filter_map(|approval| {
                Some((approval.id.clone(), serde_json::to_value(approval).ok()?))
            })
            .collect::<HashMap<_, _>>();
        let projected_approval_values = data
            .approvals
            .iter()
            .filter_map(|approval| {
                Some((approval.id.clone(), serde_json::to_value(approval).ok()?))
            })
            .collect::<HashMap<_, _>>();
        let approvals_matches = expected_approval_values == projected_approval_values;
        if !approvals_matches {
            mismatches.push(format!(
                "approvals: projection={}, replay={}",
                projected_approval_values.len(),
                expected_approval_values.len()
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
            && approvals_matches
            && memories_matches
            && questions_matches
            && task_queue_matches
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
            approvals_matches,
            ledger_matches,
            memories_matches,
            questions_matches,
            task_queue_matches,
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

        let questions = {
            let mut latest = HashMap::new();
            let mut valid = true;
            for event in data.events.iter().filter(|event| {
                matches!(
                    event.kind.as_str(),
                    "question.requested"
                        | "question.answered"
                        | "question.cancelled"
                        | "question.expired"
                )
            }) {
                match serde_json::from_value::<Question>(event.payload.clone()) {
                    Ok(question) => {
                        latest.insert(question.id.clone(), question);
                    }
                    Err(_) => {
                        valid = false;
                        break;
                    }
                }
            }
            if valid {
                let mut items = latest.into_values().collect::<Vec<_>>();
                items.sort_by(|left, right| left.created_at.cmp(&right.created_at));
                Some(items)
            } else {
                None
            }
        };

        let memories = {
            let mut items = Vec::new();
            let mut valid = true;
            for event in &data.events {
                match event.kind.as_str() {
                    "memory.created" => {
                        match serde_json::from_value::<MemoryItem>(event.payload.clone()) {
                            Ok(memory) => items.push(memory),
                            Err(_) => {
                                valid = false;
                                break;
                            }
                        }
                    }
                    "memory.deleted" => {
                        match serde_json::from_value::<MemoryItem>(event.payload.clone()) {
                            Ok(memory) => items.retain(|item| item.id != memory.id),
                            Err(_) => {
                                valid = false;
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            }
            valid.then_some(items)
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
        let priority = data
            .events
            .iter()
            .rev()
            .find(|event| event.kind == "session.priority")
            .and_then(|event| event.payload.get("priority"))
            .and_then(serde_json::Value::as_i64)
            .and_then(|value| i32::try_from(value).ok());
        Ok(SessionRepair {
            status,
            priority,
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
            approvals: Some(Self::expected_approvals(data)),
            memories,
            questions,
            tasks: Self::expected_tasks(data),
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
            {
                let mut data = tokio::fs::read(path)
                    .await
                    .with_context(|| format!("reading artifact blob {path}"))?;
                if artifact.compression == "zstd" {
                    data = zstd::stream::decode_all(&data[..])
                        .with_context(|| format!("decompressing artifact {}", artifact.id))?;
                }
                if !artifact.sha256.is_empty() && sha256_hex(&data) != artifact.sha256 {
                    anyhow::bail!(
                        "artifact {} content does not match its SHA-256",
                        artifact.id
                    );
                }
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
            memories: data.memories,
            questions: data.questions,
            tasks: data.tasks,
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

    pub async fn doctor_checks(&self) -> Result<Vec<DoctorCheck>> {
        let store = self.store.clone();
        let data_dir = store.data_dir().to_path_buf();
        let (integrity, projects, sessions) = tokio::task::spawn_blocking(move || {
            Ok::<_, anyhow::Error>((
                store.integrity_check()?,
                store.list_projects()?,
                store.list_sessions(None)?,
            ))
        })
        .await??;
        let mut checks = Vec::new();
        checks.push(DoctorCheck {
            name: "database".into(),
            status: if integrity == "ok" { "ok" } else { "error" }.into(),
            detail: integrity,
        });
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&data_dir)
                .map(|metadata| metadata.permissions().mode() & 0o777)
                .unwrap_or(0);
            checks.push(DoctorCheck {
                name: "data directory".into(),
                status: if mode & 0o077 == 0 { "ok" } else { "warn" }.into(),
                detail: format!("{} (mode {mode:o})", data_dir.display()),
            });
        }
        let missing_projects = projects
            .iter()
            .filter(|project| !Path::new(&project.root).is_dir())
            .count();
        checks.push(DoctorCheck {
            name: "project roots".into(),
            status: if missing_projects == 0 { "ok" } else { "warn" }.into(),
            detail: format!(
                "{} project(s), {missing_projects} missing root(s)",
                projects.len()
            ),
        });
        let missing_worktrees = sessions
            .iter()
            .filter(|session| session.status != "imported")
            .filter(|session| {
                session
                    .worktree
                    .as_deref()
                    .is_some_and(|path| !Path::new(path).exists())
            })
            .count();
        let active = sessions
            .iter()
            .filter(|session| matches!(session.status.as_str(), "running" | "queued" | "paused"))
            .count();
        checks.push(DoctorCheck {
            name: "sessions".into(),
            status: if active > 0 { "warn" } else { "ok" }.into(),
            detail: format!(
                "{} session(s), {active} active/queued/paused, {missing_worktrees} missing worktree(s)",
                sessions.len()
            ),
        });
        let mut mirror_mismatches = Vec::new();
        for session in &sessions {
            match self.session_log_status(&session.id).await {
                Ok(status) if !status.matches => mirror_mismatches.push(status.session_id),
                Ok(_) => {}
                Err(error) => mirror_mismatches.push(format!("{}: {error}", session.id)),
            }
        }
        checks.push(DoctorCheck {
            name: "event mirrors".into(),
            status: if mirror_mismatches.is_empty() {
                "ok"
            } else {
                "warn"
            }
            .into(),
            detail: if mirror_mismatches.is_empty() {
                "all mirrors match".to_string()
            } else {
                format!(
                    "{} mirror mismatch(es): {}",
                    mirror_mismatches.len(),
                    mirror_mismatches
                        .iter()
                        .take(3)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            },
        });
        let sandbox = Path::new("/usr/bin/sandbox-exec").is_file();
        checks.push(DoctorCheck {
            name: "shell sandbox".into(),
            status: if sandbox { "ok" } else { "warn" }.into(),
            detail: if sandbox {
                "/usr/bin/sandbox-exec is available (deprecated by macOS)".to_string()
            } else {
                "sandbox-exec is missing; shell execution fails closed".to_string()
            },
        });
        Ok(checks)
    }

    pub async fn session_log_status(&self, session_id: &str) -> Result<SessionLogStatus> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let (mirror, database) = tokio::task::spawn_blocking(move || {
            let mirror = store.read_event_mirror(&id)?;
            let database = store.list_all_events(&id)?;
            Ok::<_, anyhow::Error>((mirror, database))
        })
        .await??;
        let mirror_count = mirror.as_ref().map_or(0, Vec::len);
        let matches = match &mirror {
            None => database.is_empty(),
            Some(mirror) => {
                mirror.len() == database.len()
                    && mirror.iter().zip(database.iter()).all(|(left, right)| {
                        left.seq == right.seq
                            && left.kind == right.kind
                            && left.payload == right.payload
                            && left.created_at == right.created_at
                    })
            }
        };
        let detail = if matches {
            "event mirror matches the database".to_string()
        } else if mirror.is_none() {
            format!(
                "event mirror is missing; database has {} event(s)",
                database.len()
            )
        } else if mirror_count != database.len() {
            format!(
                "event mirror has {} event(s); database has {}",
                mirror_count,
                database.len()
            )
        } else {
            "event mirror differs from the database".to_string()
        };
        Ok(SessionLogStatus {
            session_id: session_id.to_string(),
            mirror_events: mirror_count,
            database_events: database.len(),
            matches,
            detail,
        })
    }

    pub async fn restore_session_log(&self, session_id: &str) -> Result<SessionLogStatus> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let mirror = tokio::task::spawn_blocking(move || store.read_event_mirror(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("event mirror is missing for {session_id}"))?;
        if mirror.is_empty() {
            anyhow::bail!("event mirror is empty for {session_id}");
        }
        let mut expected_seq = 1u64;
        for event in &mirror {
            if event.session_id != session_id {
                anyhow::bail!("event mirror contains an event for a different session");
            }
            if event.seq != expected_seq {
                anyhow::bail!(
                    "event mirror sequence is not contiguous at {} (expected {}, found {})",
                    event.seq,
                    expected_seq,
                    event.seq
                );
            }
            expected_seq = expected_seq.saturating_add(1);
        }
        let store = self.store.clone();
        let events = mirror.clone();
        let log_session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.replace_events(&log_session_id, &events))
            .await??;
        self.rebuild_session(session_id).await?;
        self.session_log_status(session_id).await
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

    pub async fn set_session_priority(
        &self,
        session_id: &str,
        priority: i32,
    ) -> Result<SessionPriorityResult> {
        if !(-100..=100).contains(&priority) {
            anyhow::bail!("session priority must be between -100 and 100");
        }
        let store = self.store.clone();
        let id = session_id.to_string();
        let event = tokio::task::spawn_blocking(move || {
            store.set_session_priority_with_event(&id, priority)
        })
        .await??;
        let _ = self.events.send(event);
        Ok(SessionPriorityResult {
            session_id: session_id.to_string(),
            priority,
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
        let (approval, event) = tokio::task::spawn_blocking(move || {
            store.create_approval_with_event(
                &session_id_owned,
                &tool_call_id,
                &tool,
                &input,
                agent.as_ref(),
            )
        })
        .await??;
        let (sender, receiver) = oneshot::channel();
        self.approval_waiters
            .lock()
            .map_err(|_| anyhow::anyhow!("approval mutex poisoned"))?
            .insert(approval.id.clone(), sender);
        let _ = self.events.send(event);
        Ok((approval, receiver))
    }

    pub async fn request_question(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        question: &str,
        options: Vec<String>,
    ) -> Result<(Question, oneshot::Receiver<String>)> {
        let question = question.trim();
        if question.is_empty() || question.chars().count() > 2_000 {
            anyhow::bail!("question must be 1-2000 characters");
        }
        if options.len() > 8 || options.iter().any(|option| option.chars().count() > 200) {
            anyhow::bail!("at most 8 options of 200 characters are allowed");
        }
        let store = self.store.clone();
        let session_id_owned = session_id.to_string();
        let agent_id = agent_id.to_string();
        let agent_name = agent_name.to_string();
        let question = question.to_string();
        let (item, event) = tokio::task::spawn_blocking(move || {
            store.create_question_with_event(
                &session_id_owned,
                &agent_id,
                &agent_name,
                &question,
                &options,
            )
        })
        .await??;
        let (sender, receiver) = oneshot::channel();
        self.question_waiters
            .lock()
            .map_err(|_| anyhow::anyhow!("question mutex poisoned"))?
            .insert(item.id.clone(), sender);
        let _ = self.events.send(event);
        Ok((item, receiver))
    }

    pub async fn answer_question(&self, question_id: &str, answer: &str) -> Result<bool> {
        let answer = answer.trim();
        if answer.is_empty() || answer.chars().count() > 4_000 {
            anyhow::bail!("answer must be 1-4000 characters");
        }
        let store = self.store.clone();
        let question_id_owned = question_id.to_string();
        let answer_owned = answer.to_string();
        let answered = tokio::task::spawn_blocking(move || {
            store.answer_question_with_event(&question_id_owned, &answer_owned)
        })
        .await??;
        let Some((question, event)) = answered else {
            return Ok(false);
        };
        if let Ok(mut waiters) = self.question_waiters.lock()
            && let Some(sender) = waiters.remove(&question.id)
        {
            let _ = sender.send(question.answer.unwrap_or_default());
        }
        let _ = self.events.send(event);
        Ok(true)
    }

    pub async fn close_question(
        &self,
        question_id: &str,
        status: &str,
    ) -> Result<Option<Question>> {
        let store = self.store.clone();
        let question_id_owned = question_id.to_string();
        let status_owned = status.to_string();
        let closed = tokio::task::spawn_blocking(move || {
            store.close_question_with_event(&question_id_owned, &status_owned)
        })
        .await??;
        let Some((question, event)) = closed else {
            return Ok(None);
        };
        if let Ok(mut waiters) = self.question_waiters.lock() {
            waiters.remove(&question.id);
        }
        let _ = self.events.send(event);
        Ok(Some(question))
    }

    pub async fn list_mcp_servers(&self) -> Result<McpServerListResult> {
        let store = self.store.clone();
        let servers = tokio::task::spawn_blocking(move || store.list_mcp_servers()).await??;
        Ok(McpServerListResult { servers })
    }

    pub async fn add_mcp_server(
        &self,
        name: &str,
        command: &str,
        args: Vec<String>,
        env: std::collections::BTreeMap<String, String>,
        enabled: bool,
    ) -> Result<McpServer> {
        if name.trim().is_empty() || command.trim().is_empty() {
            anyhow::bail!("MCP server name and command are required");
        }
        let store = self.store.clone();
        let name = name.trim().to_string();
        let command = command.trim().to_string();
        tokio::task::spawn_blocking(move || {
            store.add_mcp_server(&name, &command, &args, &env, enabled)
        })
        .await?
    }

    pub async fn update_mcp_server(
        &self,
        id: &str,
        name: &str,
        command: &str,
        args: Vec<String>,
        env: std::collections::BTreeMap<String, String>,
        enabled: bool,
    ) -> Result<Option<McpServer>> {
        if name.trim().is_empty() || command.trim().is_empty() {
            anyhow::bail!("MCP server name and command are required");
        }
        let store = self.store.clone();
        let id = id.to_string();
        let name = name.trim().to_string();
        let command = command.trim().to_string();
        let updated = tokio::task::spawn_blocking(move || {
            store.update_mcp_server(&id, &name, &command, &args, &env, enabled)
        })
        .await??;
        if updated.is_some() {
            self.mcp
                .invalidate(
                    &updated
                        .as_ref()
                        .map(|server| server.id.clone())
                        .unwrap_or_default(),
                )
                .await;
        }
        Ok(updated)
    }

    pub async fn remove_mcp_server(&self, id: &str) -> Result<bool> {
        let store = self.store.clone();
        let id = id.to_string();
        let lookup = id.clone();
        let removed =
            tokio::task::spawn_blocking(move || store.remove_mcp_server(&lookup)).await??;
        self.mcp.invalidate(&id).await;
        Ok(removed)
    }

    pub async fn list_acp_agents(&self) -> Result<AcpAgentListResult> {
        let store = self.store.clone();
        let agents = tokio::task::spawn_blocking(move || store.list_acp_agents()).await??;
        Ok(AcpAgentListResult { agents })
    }

    /// Resolve an ACP agent by id or by its unique display name.
    pub async fn resolve_acp_agent(&self, identifier: &str) -> Result<Option<AcpAgent>> {
        let store = self.store.clone();
        let identifier = identifier.to_string();
        tokio::task::spawn_blocking(move || store.find_acp_agent(&identifier)).await?
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn add_acp_agent(
        &self,
        name: &str,
        command: &str,
        args: Vec<String>,
        env: std::collections::BTreeMap<String, String>,
        enabled: bool,
        auto_approve: bool,
        timeout_secs: u64,
    ) -> Result<AcpAgent> {
        let name = validate_acp_agent_fields(name, command, &args, &env, timeout_secs)?;
        let store = self.store.clone();
        let command = command.trim().to_string();
        tokio::task::spawn_blocking(move || {
            store.add_acp_agent(
                &name,
                &command,
                &args,
                &env,
                enabled,
                auto_approve,
                timeout_secs,
            )
        })
        .await?
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn update_acp_agent(
        &self,
        id: &str,
        name: &str,
        command: &str,
        args: Vec<String>,
        env: std::collections::BTreeMap<String, String>,
        enabled: bool,
        auto_approve: bool,
        timeout_secs: u64,
    ) -> Result<Option<AcpAgent>> {
        let name = validate_acp_agent_fields(name, command, &args, &env, timeout_secs)?;
        let store = self.store.clone();
        let id = id.to_string();
        let command = command.trim().to_string();
        tokio::task::spawn_blocking(move || {
            store.update_acp_agent(
                &id,
                &name,
                &command,
                &args,
                &env,
                enabled,
                auto_approve,
                timeout_secs,
            )
        })
        .await?
    }

    pub async fn remove_acp_agent(&self, id: &str) -> Result<bool> {
        let store = self.store.clone();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || store.remove_acp_agent(&id)).await?
    }

    /// Spawn an external ACP agent, initialize it, and create a session without
    /// sending a prompt. This validates the executable and protocol handshake.
    pub async fn probe_acp_agent(
        &self,
        agent_id: &str,
        project_id: Option<String>,
    ) -> Result<AcpProbeResult> {
        let agent = self
            .resolve_acp_agent(agent_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("ACP agent not found: {agent_id}"))?;
        let store = self.store.clone();
        let pid = project_id.clone();
        let project = tokio::task::spawn_blocking(move || match pid {
            Some(project_id) => store.get_project(&project_id),
            None => Ok(None),
        })
        .await??;
        let policy = match &project {
            Some(project) => self.get_shell_policy(&project.id).await?,
            None => ShellPolicy::default(),
        };
        let mut secret_roots = crate::agent::shell_secret_roots(self.store.data_dir());
        secret_roots.extend(policy.read_deny.iter().map(PathBuf::from));
        let store = self.store.clone();
        let mcp_servers = tokio::task::spawn_blocking(move || store.list_mcp_servers())
            .await??
            .into_iter()
            .filter(|server| server.enabled)
            .map(|server| {
                acp::mcp_stdio_server(&server.name, &server.command, &server.args, &server.env)
            })
            .collect();
        // Probe in an isolated scratch directory. The agent is initialized and
        // gets a session, but must not be able to write into the project, the
        // project's Git metadata, or the daemon data directory.
        // A unique probe directory avoids sharing HOME/cwd between
        // concurrent probes and lets us remove all probe state afterwards.
        let probe_root = self
            .store
            .data_dir()
            .join("sandbox")
            .join("acp-probe")
            .join(&agent.id)
            .join(Uuid::now_v7().to_string());
        let cwd = probe_root.join("cwd");
        tokio::fs::create_dir_all(&cwd).await?;
        let launch = acp::AcpLaunch {
            name: agent.name.clone(),
            command: agent.command.clone(),
            args: agent.args.clone(),
            env: agent.env.clone(),
            cwd: cwd.clone(),
            project_root: cwd.clone(),
            sandbox_root: probe_root.join("scratch"),
            secret_roots,
            network_allowed: policy.network != "deny",
            home: std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default(),
            restrict_home: policy.restrict_home,
            read_jail: policy.read_jail,
            mcp_servers,
            timeout: std::time::Duration::from_secs(agent.timeout_secs.min(120)),
        };
        let result = acp::probe(launch).await;
        if let Err(error) = tokio::fs::remove_dir_all(&probe_root).await {
            tracing::warn!(%error, path = %probe_root.display(), "failed to remove ACP probe scratch");
        }
        let result = result?;
        Ok(AcpProbeResult {
            agent_id: agent.id,
            agent_name: result.agent_name,
            version: result.agent_version,
            protocol_version: result.protocol_version,
            session_created: result.session_created,
        })
    }

    pub async fn mcp_tools(&self, server_id: &str) -> Result<McpToolListResult> {
        let store = self.store.clone();
        let id = server_id.to_string();
        let server = tokio::task::spawn_blocking(move || store.get_mcp_server(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("MCP server not found: {server_id}"))?;
        let tools = self.mcp.list_tools(&server).await?;
        Ok(McpToolListResult {
            server_id: server_id.to_string(),
            tools,
        })
    }

    pub async fn mcp_call(
        &self,
        server_id: &str,
        tool: &str,
        arguments: serde_json::Value,
    ) -> Result<McpCallResult> {
        let store = self.store.clone();
        let id = server_id.to_string();
        let server = tokio::task::spawn_blocking(move || store.get_mcp_server(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("MCP server not found: {server_id}"))?;
        let result = self.mcp.call_tool(&server, tool, arguments).await?;
        Ok(McpCallResult {
            server_id: server_id.to_string(),
            tool: tool.to_string(),
            result,
        })
    }

    pub async fn mcp_resources(&self, server_id: &str) -> Result<McpResourceListResult> {
        let store = self.store.clone();
        let id = server_id.to_string();
        let server = tokio::task::spawn_blocking(move || store.get_mcp_server(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("MCP server not found: {server_id}"))?;
        self.mcp.list_resources(&server).await
    }

    pub async fn mcp_read_resource(
        &self,
        server_id: &str,
        uri: &str,
    ) -> Result<McpReadResourceResult> {
        let store = self.store.clone();
        let id = server_id.to_string();
        let server = tokio::task::spawn_blocking(move || store.get_mcp_server(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("MCP server not found: {server_id}"))?;
        self.mcp.read_resource(&server, uri).await
    }

    pub async fn mcp_prompts(&self, server_id: &str) -> Result<McpPromptListResult> {
        let store = self.store.clone();
        let id = server_id.to_string();
        let server = tokio::task::spawn_blocking(move || store.get_mcp_server(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("MCP server not found: {server_id}"))?;
        self.mcp.list_prompts(&server).await
    }

    pub async fn mcp_prompt(
        &self,
        server_id: &str,
        name: &str,
        arguments: std::collections::BTreeMap<String, String>,
    ) -> Result<McpGetPromptResult> {
        let store = self.store.clone();
        let id = server_id.to_string();
        let server = tokio::task::spawn_blocking(move || store.get_mcp_server(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("MCP server not found: {server_id}"))?;
        self.mcp.get_prompt(&server, name, arguments).await
    }

    pub async fn console_open(&self, session_id: &str) -> Result<ConsoleOpenResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let (session, project) = tokio::task::spawn_blocking(move || {
            let session = store
                .get_session(&id)?
                .ok_or_else(|| anyhow::anyhow!("session not found: {id}"))?;
            let project = store
                .get_project(&session.project_id)?
                .ok_or_else(|| anyhow::anyhow!("project not found"))?;
            Ok::<_, anyhow::Error>((session, project))
        })
        .await??;
        if session.status == "imported" {
            anyhow::bail!("imported sessions are read-only archives");
        }
        let session = self.ensure_session_worktree(&session.id).await?;
        let worktree = session
            .worktree
            .clone()
            .ok_or_else(|| anyhow::anyhow!("session has no worktree"))?;
        let policy = self.get_shell_policy(&project.id).await?;
        let mut secret_roots = crate::agent::shell_secret_roots(self.store.data_dir());
        secret_roots.extend(policy.read_deny.iter().map(PathBuf::from));
        let context = ToolContext {
            root: PathBuf::from(&worktree),
            project_root: PathBuf::from(&project.root),
            sandbox_root: self
                .store
                .data_dir()
                .join("sandbox")
                .join(&session.id)
                .join("console"),
            secret_roots,
            network_allowed: policy.network != "deny",
            home: std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default(),
            restrict_home: policy.restrict_home,
            read_jail: policy.read_jail,
        };
        self.consoles.open(&session.id, context).await
    }

    pub async fn console_write(&self, console_id: &str, data: &str) -> Result<ConsoleWriteResult> {
        let written = self.consoles.write(console_id, data).await?;
        Ok(ConsoleWriteResult { written })
    }

    pub async fn console_read(
        &self,
        console_id: &str,
        after_seq: u64,
    ) -> Result<ConsoleReadResult> {
        self.consoles.read(console_id, after_seq).await
    }

    pub async fn console_signal(
        &self,
        console_id: &str,
        signal: &str,
    ) -> Result<ConsoleSignalResult> {
        self.consoles.signal(console_id, signal).await
    }

    pub async fn console_close(&self, console_id: &str) -> Result<ConsoleCloseResult> {
        self.consoles.close(console_id).await
    }

    pub async fn workspace_tree(&self, session_id: &str) -> Result<WorkspaceTreeResult> {
        let root = self.workspace_root(session_id).await?;
        tokio::task::spawn_blocking(move || workspace::tree(Path::new(&root))).await?
    }

    pub async fn workspace_file(
        &self,
        session_id: &str,
        path: &str,
    ) -> Result<WorkspaceFileResult> {
        let root = self.workspace_root(session_id).await?;
        let path = path.to_string();
        tokio::task::spawn_blocking(move || workspace::read(Path::new(&root), &path)).await?
    }

    pub async fn workspace_write(
        &self,
        session_id: &str,
        path: &str,
        content: String,
        expected_sha256: Option<String>,
    ) -> Result<WorkspaceWriteResult> {
        let root = self.workspace_root(session_id).await?;
        let path_for_write = path.to_string();
        let expected = expected_sha256.clone();
        let result = tokio::task::spawn_blocking(move || {
            workspace::write(
                Path::new(&root),
                &path_for_write,
                &content,
                expected.as_deref(),
            )
        })
        .await??;
        if let Err(error) = self
            .append_event(
                session_id,
                "workspace.file_saved",
                serde_json::json!({
                    "path": result.path,
                    "size": result.size,
                    "sha256": result.sha256,
                }),
            )
            .await
        {
            tracing::warn!(%error, "failed to append workspace save event");
        }
        Ok(result)
    }

    async fn workspace_root(&self, session_id: &str) -> Result<String> {
        Ok(self.workspace_session(session_id).await?.1)
    }

    async fn workspace_session(&self, session_id: &str) -> Result<(Session, String)> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        if session.status == "imported" {
            anyhow::bail!("imported sessions are read-only archives");
        }
        let session = self.ensure_session_worktree(&session.id).await?;
        let worktree = session
            .worktree
            .clone()
            .ok_or_else(|| anyhow::anyhow!("session has no worktree"))?;
        Ok((session, worktree))
    }

    pub async fn git_log(&self, session_id: &str, limit: u32) -> Result<GitLogResult> {
        let (session, root) = self.workspace_session(session_id).await?;
        let commits =
            tokio::task::spawn_blocking(move || history::log(Path::new(&root), limit)).await??;
        Ok(GitLogResult {
            session_id: session_id.to_string(),
            branch: session.branch,
            base_commit: session.base_commit,
            commits,
        })
    }

    pub async fn git_show(&self, session_id: &str, commit: &str) -> Result<GitShowResult> {
        let (_, root) = self.workspace_session(session_id).await?;
        let commit = commit.to_string();
        let mut result =
            tokio::task::spawn_blocking(move || history::show(Path::new(&root), &commit)).await??;
        result.session_id = session_id.to_string();
        Ok(result)
    }

    pub async fn terminal_open(
        &self,
        session_id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<TerminalOpenResult> {
        let (session, worktree) = self.workspace_session(session_id).await?;
        let store = self.store.clone();
        let project_id = session.project_id.clone();
        let project = tokio::task::spawn_blocking(move || store.get_project(&project_id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("project not found"))?;
        let policy = self.get_shell_policy(&project.id).await?;
        let mut secret_roots = crate::agent::shell_secret_roots(self.store.data_dir());
        secret_roots.extend(policy.read_deny.iter().map(PathBuf::from));
        let context = ToolContext {
            root: PathBuf::from(&worktree),
            project_root: PathBuf::from(&project.root),
            sandbox_root: self
                .store
                .data_dir()
                .join("sandbox")
                .join(&session.id)
                .join("terminal"),
            secret_roots,
            network_allowed: policy.network != "deny",
            home: std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default(),
            restrict_home: policy.restrict_home,
            read_jail: policy.read_jail,
        };
        self.terminals.open(&session.id, context, cols, rows).await
    }

    pub async fn terminal_write(
        &self,
        terminal_id: &str,
        data: &str,
    ) -> Result<TerminalWriteResult> {
        self.terminals.write(terminal_id, data).await
    }

    pub async fn terminal_read(
        &self,
        terminal_id: &str,
        after_seq: u64,
    ) -> Result<TerminalReadResult> {
        self.terminals.read(terminal_id, after_seq).await
    }

    pub async fn terminal_resize(
        &self,
        terminal_id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<TerminalResizeResult> {
        self.terminals.resize(terminal_id, cols, rows).await
    }

    pub async fn terminal_close(&self, terminal_id: &str) -> Result<TerminalCloseResult> {
        self.terminals.close(terminal_id).await
    }

    pub async fn list_questions(&self, session_id: Option<String>) -> Result<QuestionListResult> {
        let store = self.store.clone();
        let questions =
            tokio::task::spawn_blocking(move || store.list_questions(session_id.as_deref()))
                .await??;
        Ok(QuestionListResult { questions })
    }

    pub async fn resolve_approval(&self, approval_id: &str, approved: bool) -> Result<()> {
        let store = self.store.clone();
        let id = approval_id.to_string();
        let resolved =
            tokio::task::spawn_blocking(move || store.resolve_approval_with_event(&id, approved))
                .await??;
        if let Some((approval, event)) = resolved {
            if let Some(sender) = self
                .approval_waiters
                .lock()
                .ok()
                .and_then(|mut waiters| waiters.remove(&approval.id))
            {
                let _ = sender.send(approved);
            }
            let _ = self.events.send(event);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn get_worktree_policy(&self, project_id: &str) -> Result<WorktreeRetentionPolicy> {
        let store = self.store.clone();
        let id = project_id.to_string();
        tokio::task::spawn_blocking(move || {
            store
                .get_project(&id)?
                .ok_or_else(|| anyhow::anyhow!("project not found: {id}"))?;
            store.get_worktree_policy(&id)
        })
        .await?
    }

    pub async fn set_worktree_policy(
        &self,
        project_id: &str,
        policy: &WorktreeRetentionPolicy,
    ) -> Result<WorktreePolicyResult> {
        if policy.older_than_secs > 365 * 24 * 60 * 60 {
            anyhow::bail!("worktree retention must be at most 365 days");
        }
        let store = self.store.clone();
        let id = project_id.to_string();
        let policy = policy.clone();
        let saved = tokio::task::spawn_blocking(move || {
            store
                .get_project(&id)?
                .ok_or_else(|| anyhow::anyhow!("project not found: {id}"))?;
            store.set_worktree_policy(&id, &policy)?;
            store.get_worktree_policy(&id)
        })
        .await??;
        Ok(WorktreePolicyResult {
            project_id: project_id.to_string(),
            policy: saved,
        })
    }

    /// Run the opt-in retention policy for every project with it enabled.
    /// Returns the number of worktrees removed.
    pub async fn sweep_worktree_retention(&self) -> Result<usize> {
        let store = self.store.clone();
        let policies =
            tokio::task::spawn_blocking(move || store.list_enabled_worktree_policies()).await??;
        let mut removed = 0;
        for (project_id, policy) in policies {
            match self
                .cleanup_worktrees(
                    &project_id,
                    policy.older_than_secs,
                    false,
                    false,
                    policy.delete_branches,
                    policy.keep_failed,
                )
                .await
            {
                Ok(result) => removed += result.removed,
                Err(error) => {
                    tracing::warn!(
                        project_id = %project_id,
                        error = %error,
                        "automatic worktree retention failed"
                    );
                }
            }
        }
        Ok(removed)
    }

    pub async fn cleanup_worktrees(
        &self,
        project_id: &str,
        older_than_secs: u64,
        dry_run: bool,
        force: bool,
        delete_branches: bool,
        keep_failed: bool,
    ) -> Result<WorktreeCleanupResult> {
        let store = self.store.clone();
        let project_store = store.clone();
        let project_id_owned = project_id.to_string();
        let project =
            tokio::task::spawn_blocking(move || project_store.get_project(&project_id_owned))
                .await??
                .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        let project_root = PathBuf::from(&project.root);
        let list_store = store.clone();
        let list_project_id = project_id.to_string();
        let sessions =
            tokio::task::spawn_blocking(move || list_store.list_sessions(Some(&list_project_id)))
                .await??;
        let now = time::OffsetDateTime::now_utc();
        let sandbox_root = store.data_dir().join("sandbox");
        let mut entries = Vec::new();
        let mut removed = 0usize;
        let mut skipped = 0usize;
        let mut scanned = 0usize;
        for session in sessions {
            let terminal = matches!(
                session.status.as_str(),
                "idle" | "failed" | "cancelled" | "landed"
            );
            if !terminal {
                continue;
            }
            let Some(worktree) = session.worktree.clone() else {
                continue;
            };
            let Some(branch) = session.branch.clone() else {
                continue;
            };
            scanned += 1;
            if keep_failed && matches!(session.status.as_str(), "failed" | "cancelled") {
                skipped += 1;
                entries.push(WorktreeCleanupEntry {
                    session_id: session.id.clone(),
                    status: session.status.clone(),
                    worktree,
                    branch,
                    removed: false,
                    reason: "kept failed/cancelled worktree".to_string(),
                });
                continue;
            }
            let age = time::OffsetDateTime::parse(
                &session.created_at,
                &time::format_description::well_known::Rfc3339,
            )
            .map(|created| (now - created).whole_seconds().max(0) as u64)
            .unwrap_or(0);
            if age < older_than_secs {
                skipped += 1;
                entries.push(WorktreeCleanupEntry {
                    session_id: session.id.clone(),
                    status: session.status.clone(),
                    worktree,
                    branch,
                    removed: false,
                    reason: "younger than retention threshold".to_string(),
                });
                continue;
            }
            let path = PathBuf::from(&worktree);
            if !path.exists() {
                let sandbox = sandbox_root.join(&session.id);
                let _ =
                    tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&sandbox)).await?;
                skipped += 1;
                entries.push(WorktreeCleanupEntry {
                    session_id: session.id.clone(),
                    status: session.status.clone(),
                    worktree,
                    branch,
                    removed: false,
                    reason: "worktree path is already missing".to_string(),
                });
                continue;
            }
            let dirty = tokio::task::spawn_blocking({
                let path = path.clone();
                move || worktree::has_uncommitted_changes(&path)
            })
            .await??;
            if dirty && !force {
                skipped += 1;
                entries.push(WorktreeCleanupEntry {
                    session_id: session.id.clone(),
                    status: session.status.clone(),
                    worktree,
                    branch,
                    removed: false,
                    reason: "uncommitted changes (use force to remove)".to_string(),
                });
                continue;
            }
            if dry_run {
                skipped += 1;
                entries.push(WorktreeCleanupEntry {
                    session_id: session.id.clone(),
                    status: session.status.clone(),
                    worktree,
                    branch,
                    removed: false,
                    reason: "would remove".to_string(),
                });
                continue;
            }
            match tokio::task::spawn_blocking({
                let root = project_root.clone();
                let path = path.clone();
                move || worktree::remove_worktree(&root, &path)
            })
            .await?
            {
                Ok(()) => {
                    let mut reason = "removed worktree".to_string();
                    let sandbox = sandbox_root.join(&session.id);
                    if let Err(error) =
                        tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&sandbox))
                            .await?
                        && error.kind() != std::io::ErrorKind::NotFound
                    {
                        reason = format!("{reason}; scratch cleanup failed: {error}");
                    }
                    if delete_branches
                        && let Err(error) = tokio::task::spawn_blocking({
                            let root = project_root.clone();
                            let branch = branch.clone();
                            move || worktree::delete_branch(&root, &branch)
                        })
                        .await?
                    {
                        reason = format!("removed worktree; branch delete failed: {error}");
                    }
                    removed += 1;
                    entries.push(WorktreeCleanupEntry {
                        session_id: session.id.clone(),
                        status: session.status.clone(),
                        worktree,
                        branch,
                        removed: true,
                        reason,
                    });
                }
                Err(error) => {
                    skipped += 1;
                    entries.push(WorktreeCleanupEntry {
                        session_id: session.id.clone(),
                        status: session.status.clone(),
                        worktree,
                        branch,
                        removed: false,
                        reason: format!("remove failed: {error}"),
                    });
                }
            }
        }
        Ok(WorktreeCleanupResult {
            project_id: project.id,
            scanned,
            removed,
            skipped,
            entries,
        })
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

fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn parse_numstat(numstat: &str) -> Vec<SessionCompareFile> {
    let mut files = Vec::new();
    for line in numstat.lines() {
        let mut parts = line.splitn(3, '\t');
        let additions = parts.next().unwrap_or("-");
        let deletions = parts.next().unwrap_or("-");
        let path = parts.next().unwrap_or("");
        if path.is_empty() {
            continue;
        }
        let binary = additions == "-" || deletions == "-";
        files.push(SessionCompareFile {
            path: path.to_string(),
            additions: additions.parse().unwrap_or(0),
            deletions: deletions.parse().unwrap_or(0),
            binary,
        });
    }
    files
}

fn clip_transcript(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        text.to_string()
    } else {
        text.chars().take(limit).collect::<String>() + "\n… truncated"
    }
}

fn validate_acp_agent_fields(
    name: &str,
    command: &str,
    args: &[String],
    env: &std::collections::BTreeMap<String, String>,
    timeout_secs: u64,
) -> Result<String> {
    let name = name.trim();
    let command = command.trim();
    if name.is_empty() || name.chars().count() > 100 {
        anyhow::bail!("ACP agent name must be 1-100 characters");
    }
    if command.is_empty() || command.chars().count() > 4_096 {
        anyhow::bail!("ACP agent command must be 1-4096 characters");
    }
    if args.len() > 128 || args.iter().any(|arg| arg.chars().count() > 4_096) {
        anyhow::bail!("ACP agent arguments are limited to 128 values of 4096 characters");
    }
    if env.len() > 64
        || env.iter().any(|(key, value)| {
            key.is_empty()
                || key.chars().count() > 256
                || key.contains('=')
                || key.contains('\0')
                || value.chars().count() > 16_384
                || value.contains('\0')
        })
    {
        anyhow::bail!("ACP agent environment is limited to 64 valid variables of 16 KiB each");
    }
    if !(30..=14_400).contains(&timeout_secs) {
        anyhow::bail!("ACP agent timeout must be between 30 and 14400 seconds");
    }
    Ok(name.to_string())
}

fn validate_model_preset(
    name: &str,
    provider: &str,
    model: &str,
    context_window: Option<u64>,
    input_price: Option<f64>,
    output_price: Option<f64>,
    notes: &str,
) -> Result<()> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        anyhow::bail!("model preset name must be 1-100 characters");
    }
    let provider = provider.trim();
    if !matches!(provider, "openai-compatible" | "anthropic") {
        anyhow::bail!("model preset provider must be openai-compatible or anthropic");
    }
    let model = model.trim();
    if model.is_empty() || model.chars().count() > 200 {
        anyhow::bail!("model preset model must be 1-200 characters");
    }
    if context_window.is_some_and(|window| window > 10_000_000) {
        anyhow::bail!("model preset context window must be at most 10,000,000");
    }
    for price in [input_price, output_price].into_iter().flatten() {
        if !price.is_finite() || price < 0.0 || price > 100_000.0 {
            anyhow::bail!("model preset prices must be finite and between 0 and 100,000");
        }
    }
    if notes.chars().count() > 2_000 {
        anyhow::bail!("model preset notes must be at most 2,000 characters");
    }
    Ok(())
}

pub(crate) fn ensure_session_quiescent(session: &Session, action: &str) -> Result<()> {
    if session.status == "imported" {
        anyhow::bail!("imported sessions are read-only archives");
    }
    if matches!(
        session.status.as_str(),
        "queued" | "running" | "paused" | "waiting_input"
    ) {
        anyhow::bail!(
            "session must not be queued, running, paused, or waiting for input to {action}"
        );
    }
    Ok(())
}

pub(crate) fn checkpoint_from_event(event: &Event) -> Option<SessionCheckpoint> {
    let payload = event.payload.as_object()?;
    Some(SessionCheckpoint {
        id: payload.get("checkpoint_id")?.as_str()?.to_string(),
        session_id: event.session_id.clone(),
        label: payload
            .get("label")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("checkpoint")
            .to_string(),
        commit: payload.get("commit")?.as_str()?.to_string(),
        branch: payload
            .get("branch")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        start_message_id: payload
            .get("start_message_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        through_message_id: payload
            .get("through_message_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        message_count: payload
            .get("message_count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as usize,
        summary: payload
            .get("summary")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        dirty: payload
            .get("dirty")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        created_at: payload
            .get("created_at")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(&event.created_at)
            .to_string(),
    })
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

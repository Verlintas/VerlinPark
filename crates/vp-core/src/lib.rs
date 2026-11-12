//! Application services shared by the daemon and future harness components.
//!
//! M1 adds the Leader agent loop, tool registry, approvals, and event stream.

#![recursion_limit = "512"]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::tools::{ToolContext, ToolOutput};
use anyhow::{Context, Result, anyhow};
use async_recursion::async_recursion;
use ignore::WalkBuilder;
use tokio::sync::{broadcast, oneshot};
use uuid::Uuid;
use vp_protocol::lsp::{LspLanguageStatus, LspStatusResult};
use vp_protocol::now_rfc3339;
use vp_protocol::remote::RemoteTestEvidence;
use vp_protocol::types::{
    AcpAgent, AcpAgentConfig, AcpAgentListResult, AcpProbeResult, AcpTransport, AgentMessage,
    AgentRecord, Approval, ApprovalAuditEvent, ApprovalAuditResult, ApprovalBatchItemResult,
    ApprovalBatchResolveResult, ApprovalGrant, ApprovalQueueResult, ApprovalTemplate,
    ApprovalTemplateCreateParams, ApprovalTemplateDeleteParams, ApprovalTemplateDeleteResult,
    ApprovalTemplateFromApprovalParams, ApprovalTemplateResult, ApprovalTemplateUpdateParams,
    Artifact, ArtifactBackfillResult, ArtifactDiffParams, ArtifactDiffResult, ArtifactGcResult,
    ArtifactMaterialization, ArtifactPolicy, ArtifactVersionsResult, AttentionPolicy,
    AttentionPolicyResult, AttentionPolicySetParams, BudgetGovernorDecision, CachePolicy,
    CheckpointPolicyResult, CheckpointRetentionPolicy, CitationAudit, CitationAuditMessage,
    ConsoleCloseResult, ConsoleOpenResult, ConsoleReadResult, ConsoleSignalResult,
    ConsoleWriteResult, ContextCompactionSummary, ContextMeasurement, ContextUsage,
    ContinuationOutcome, ContractPolicy, CreateModelPresetParams, CriticalPathPolicy, DoctorCheck,
    EffectPolicy, EffectReceipt, EffectReconcileParams, EffectReconcileResult, EffectState, Event,
    ForgeOriginStatus, ForgePreflightResult, ForgeProviderStatus, ForgeStatusResult, GateRecord,
    GitLogResult, GitShowResult, GovernorSessionStatus, IntakeCreateParams, IntakeListResult,
    IntakeTask, IntakeUpdateParams, IntegrationPolicy, IntentDecisionRecord, MailboxGcResult,
    MailboxPolicy, McpCallResult, McpGetPromptResult, McpPinActionResult, McpPinApproveParams,
    McpPinRejectParams, McpProbeResult, McpPromptListResult, McpReadResourceResult,
    McpResourceListResult, McpServer, McpServerConfig, McpServerListResult, McpToolListResult,
    McpToolPin, MemoryBriefResult, MemoryConflict, MemoryConflictResolveResult,
    MemoryConflictScanResult, MemoryConflictsResult, MemoryDeleteResult, MemoryEmbeddingPolicy,
    MemoryEmbeddingsIndexRebuildResult, MemoryEmbeddingsIndexStatusResult,
    MemoryEmbeddingsRebuildResult, MemoryEmbeddingsStatusResult, MemoryEmbeddingsTestResult,
    MemoryEvidence, MemoryHealthResult, MemoryItem, MemoryListResult, MemoryMatch, MemoryPolicy,
    MemoryPolicyResult, MemoryPruneResult, MemorySearchParams, MemorySearchResult,
    MemorySemanticSummary, MemoryStatsResult, MemorySweepResult, MemoryUpdateResult,
    MemoryVerificationCheck, MemoryVerifyResult, MergeQueueEntry, MessageCitation, MetricCount,
    ModelPreset, ModelPresetListResult, OrchestrationDecision, OrchestrationNote,
    OrchestrationOverviewResult, OrchestrationOverviewTotals, OrchestrationPolicy,
    OrchestrationPreviewAction, OrchestrationPreviewResult, OrchestrationRecoveriesResult,
    OrchestrationRecovery, OrchestrationSessionSummary, OrchestrationSnapshot,
    PlanRevisionDetailResult, PlanRevisionInfo, PreemptionOverviewResult, PreemptionPolicy,
    PreemptionRecord, ProcessListResult, ProcessLogsResult, ProcessPolicy, ProcessRestartResult,
    ProcessStopResult, ProcessSupervisorStatus, Program, ProgramActionParams, ProgramCreateParams,
    ProgramDeleteParams, ProgramListResult, ProgramMilestone, ProgramMilestoneActionParams,
    ProgramMilestoneCreateParams, ProgramMilestoneListResult, ProgramMilestoneUpdateParams,
    ProgramPolicy, ProgramReplayReport, ProgramStatus, ProgramStatusParams, ProgramStatusResult,
    ProgramTickParams, ProgramTickResult, ProgramUpdateParams, Project, ProjectBudget,
    ProjectBudgetResult, ProjectCacheEpochPruneResult, ProjectCacheEpochResult,
    ProjectMcpPinsResult, ProjectUsageResult, PromptPrefixAggregate, PromptPrefixObservation,
    PullRequestResult, Question, QuestionListResult, ReviseTeamPlanResult, ScheduledTask,
    SearchResult, Session, SessionBundle, SessionCacheStatus, SessionCheckpoint,
    SessionCheckpointDeleteResult, SessionCheckpointDiffResult, SessionCheckpointListResult,
    SessionCheckpointPruneResult, SessionCheckpointRestoreResult, SessionCheckpointStatsResult,
    SessionCitationAuditResult, SessionCitationsResult, SessionCommit, SessionCompareFile,
    SessionCompareResult, SessionContextStatus, SessionContinuation,
    SessionContinuationCancelResult, SessionContinuationClaimInfo,
    SessionContinuationReclaimResult, SessionContinuationStatusResult, SessionContinuationsResult,
    SessionContractResult, SessionDeferResult, SessionDiff, SessionEffectsResult,
    SessionForkResult, SessionForkTreeNode, SessionForkTreeResult, SessionImportResult,
    SessionInstructionSection, SessionInstructionStateReport, SessionInstructionsReport,
    SessionLandResult, SessionLineageEntry, SessionLineageResult, SessionLogStatus,
    SessionPreemptionResult, SessionPriorityResult, SessionRehydrateResult, SessionReplayReport,
    SessionSkillActivationsResult, SessionStallsResult, SessionSteerResult,
    SessionSteeringCancelResult, SessionSteeringListResult, SessionTranscriptResult,
    SessionUnlimitedResult, ShellPolicy, Skill, SkillActivation, SkillCreateParams,
    SkillDeleteResult, SkillEnabledParams, SkillImportParams, SkillListResult, SkillUpdateParams,
    Source, SourceChunk, SourceCitation, SourceCollectionsResult, SourceCreateFileParams,
    SourceCreateNoteParams, SourceCreateUrlParams, SourceDeleteResult, SourceEmbeddingPolicy,
    SourceEmbeddingPolicyResult, SourceEmbeddingReindexResult, SourceEmbeddingStatusResult,
    SourceGetResult, SourceImportError, SourceImportParams, SourceImportResult,
    SourceIndexRebuildResult, SourceIndexStatusResult, SourceListResult, SourceReindexResult,
    SourceRetrievalPolicy, SourceRetrievalPolicyResult, SourceSearchMode, SourceSearchResult,
    SourceStats, SteeringMessage, StoredMessage, TaskQueueResult, TaskSchedulerPolicy,
    TaskSchedulerPolicyResult, TaskStall, TeamPlan, TerminalCloseResult, TerminalOpenResult,
    TerminalReadResult, TerminalResizeResult, TerminalWriteResult, TodoItem, ToolActivityCodeStat,
    ToolActivityEntry, ToolActivityResult, ToolActivitySummary, ToolActivityToolStat,
    ToolOutputAuditParams, ToolOutputAuditResult, ToolOutputListParams, ToolOutputListResult,
    ToolOutputPolicy, ToolOutputPolicyResult, ToolOutputPruneParams, ToolOutputPruneResult,
    ToolOutputStatus, ToolOutputStatusParams, UpdateModelPresetParams, UpdateSourceMetadataParams,
    UsageBucket, UsageCsvResult, UsageEntry, UsageLedgerResult, UsageTotals, WorkspaceFileResult,
    WorkspaceGrepResult, WorkspaceListResult, WorkspaceSearchResult, WorkspaceTreeResult,
    WorkspaceWriteResult, WorktreeCleanupEntry, WorktreeCleanupResult, WorktreePolicyResult,
    WorktreeRetentionPolicy, WriteLease,
};
use vp_store::Store;
use vp_store::store::{
    ContractUsageRow, DeliveredSteering, MemoryCandidate, NewIntakeTask, NewMemory,
    NewSessionContinuation, PromptPrefixCall, PromptPrefixTotals, RuntimeLeaseAcquisition,
    RuntimeLeaseToken, SessionContinuationAckError, SessionContinuationClaim, SessionRepair,
    SourceEmbeddingChunkRow, SourceSearchQuery, SourceSnapshot, StoredContractRow,
};

use crate::budget::BudgetRegistry;
use crate::budget_governor::GovernorBudgetView;

pub mod acp;
pub mod agent;
pub mod approvals;
pub mod archive_media;
pub mod backup;
pub mod browser;
pub mod budget;
pub mod budget_governor;
pub mod cache_audit;
pub mod cache_epoch;
pub mod computer;
pub mod console;
pub mod context;
pub mod continuation;
pub mod contract;
pub mod conversation;
pub mod critical_path;
pub mod daemon_agent;
mod dashboard;
pub mod effect;
pub mod forge;
pub mod gate;
pub mod gui;
pub mod history;
pub mod instruction;
pub mod mcp;
pub mod mcp_pins;
pub mod memory;
pub mod memory_embedding;
pub mod memory_index;
pub mod multi_path;
mod operation;
pub mod orchestration;
pub mod permissions;
pub mod preemption;
pub mod process_supervisor;
pub mod program;
pub mod prompt_cache;
pub mod rebuild;
pub mod remote;
pub mod remote_bootstrap;
pub mod remote_integration;
pub mod remote_test;
pub mod run_journal;
pub mod runtime_pin;
pub mod scaling;
pub mod scheduler;
mod session_graph;
pub mod skills;
pub mod source_ann;
pub mod source_embedding;
pub mod source_retrieval;
pub mod sources;
pub mod terminal;
pub mod test_cache;
pub mod textdiff;
pub mod tool_bundle;
pub mod tool_output;
pub mod tooling;
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
    memory_conflicts: Vec<MemoryConflict>,
    questions: Vec<Question>,
    tasks: Vec<ScheduledTask>,
    steering: Vec<SteeringMessage>,
    continuations: Vec<SessionContinuation>,
    gates: Vec<GateRecord>,
    recoveries: Vec<OrchestrationRecovery>,
    notes: Vec<OrchestrationNote>,
    decisions: Vec<OrchestrationDecision>,
    scaling_decisions: Vec<vp_protocol::scaling::ScalingDecision>,
    runs: Vec<vp_protocol::types::SessionRun>,
    run_steps: Vec<vp_protocol::types::RunStep>,
    run_checkpoints: Vec<vp_protocol::types::RunCheckpoint>,
    context_checkpoints: Vec<vp_protocol::types::ContextCheckpoint>,
    processes: Vec<vp_protocol::types::SupervisedProcess>,
    test_results: Vec<vp_protocol::test_cache::TestCacheEntry>,
    remote_test_evidence: Vec<RemoteTestEvidence>,
    instructions: Vec<vp_protocol::types::SessionInstructionState>,
    contracts: Vec<StoredContractRow>,
    contract_usage: Vec<ContractUsageRow>,
    effects: Vec<EffectReceipt>,
    policy_snapshots: Vec<vp_protocol::runtime::PolicySnapshot>,
    plan_runtime_snapshot: Option<String>,
    prompt_prefix_observations: Vec<PromptPrefixObservation>,
    prompt_prefix_aggregates: Vec<PromptPrefixAggregate>,
    prompt_prefix_inconsistent_epochs: Vec<String>,
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
    approval_waiters: Arc<Mutex<HashMap<String, Vec<oneshot::Sender<bool>>>>>,
    question_waiters: Arc<Mutex<HashMap<String, oneshot::Sender<String>>>>,
    git_lock: Arc<Mutex<()>>,
    budget_registry: Arc<BudgetRegistry>,
    unlimited_flags: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    mcp: Arc<mcp::McpManager>,
    remote: Arc<remote::RemoteManager>,
    consoles: Arc<console::ConsoleManager>,
    terminals: Arc<terminal::TerminalManager>,
    browsers: Arc<browser::BrowserManager>,
    computers: Arc<computer::ComputerManager>,
    gui: Arc<gui::GuiCoordinator>,
    scheduler: Arc<scheduler::TaskScheduler>,
    process_supervisor: Arc<process_supervisor::ProcessSupervisor>,
    scaling: Arc<scaling::ScalingCoordinator>,
    runtime_id: String,
    continuation_claims: Arc<Mutex<HashMap<String, SessionContinuationClaim>>>,
    /// Serializes MCP pin discovery so two concurrent agents cannot both
    /// perform the first TOFU sync and emit duplicate lifecycle events.
    pin_sync: Arc<tokio::sync::Mutex<()>>,
}

/// A held cross-process session lease. Dropping without calling `release`
/// aborts the heartbeat and performs a best-effort fenced release; a process
/// abort still leaves the row to expire after the TTL.
pub struct SessionLease {
    store: Store,
    events: broadcast::Sender<Event>,
    pub lease_key: String,
    pub owner_id: String,
    pub owner_instance: String,
    pub session_id: String,
    pub fencing_token: u64,
    token: RuntimeLeaseToken,
    lost: Arc<AtomicBool>,
    released: bool,
    heartbeat: Option<tokio::task::JoinHandle<()>>,
}

impl SessionLease {
    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::SeqCst)
    }

    fn abort_heartbeat(&mut self) {
        if let Some(heartbeat) = self.heartbeat.take() {
            heartbeat.abort();
        }
    }

    fn release_sync(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        self.abort_heartbeat();
        let _ = self
            .store
            .release_runtime_lease(&self.lease_key, &self.owner_id, &self.token);
    }

    pub async fn release(mut self) {
        self.abort_heartbeat();
        if self.released {
            return;
        }
        let failed = {
            let store = self.store.clone();
            let key = self.lease_key.clone();
            let owner = self.owner_id.clone();
            let token = self.token.clone();
            tokio::task::spawn_blocking(move || {
                store.release_runtime_lease(&key, &owner, &token).is_err()
            })
            .await
            .unwrap_or(true)
        };
        self.released = true;
        if failed {
            tracing::warn!(
                lease_key = %self.lease_key,
                owner_id = %self.owner_id,
                "session lease was already lost before explicit release"
            );
        }
        if let Ok(event) = self.store.append_event(
            &self.session_id,
            "session.lease.released",
            serde_json::json!({
                "lease_key": self.lease_key,
                "owner_id": self.owner_id,
                "owner_instance": self.owner_instance,
                "fencing_token": self.fencing_token,
                "lease_lost": failed,
            }),
        ) {
            let _ = self.events.send(event);
        }
    }
}

impl Drop for SessionLease {
    fn drop(&mut self) {
        self.release_sync();
    }
}

/// A generic daemon-wide lease used for integration and other serialized
/// operations. It carries the unpredictable acquisition token and monotonic
/// generation. Renewal is fenced; a failed heartbeat marks the guard lost so
/// the critical section can stop before its next external effect.
pub struct RuntimeLeaseGuard {
    store: Store,
    pub lease_key: String,
    pub kind: String,
    pub owner_id: String,
    pub owner_instance: String,
    pub fencing_token: u64,
    token: RuntimeLeaseToken,
    lost: Arc<AtomicBool>,
    lost_notify: Arc<tokio::sync::Notify>,
    released: bool,
    heartbeat: Option<tokio::task::JoinHandle<()>>,
}

impl RuntimeLeaseGuard {
    fn from_acquisition(store: Store, acquisition: RuntimeLeaseAcquisition) -> Self {
        let (lease, token) = acquisition.into_parts();
        Self {
            store,
            lease_key: lease.lease_key,
            kind: lease.kind,
            owner_id: lease.owner_id,
            owner_instance: lease.owner_instance,
            fencing_token: lease.fencing_token,
            token,
            lost: Arc::new(AtomicBool::new(false)),
            lost_notify: Arc::new(tokio::sync::Notify::new()),
            released: false,
            heartbeat: None,
        }
    }

    pub fn generation(&self) -> u64 {
        self.fencing_token
    }

    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::SeqCst)
    }

    pub(crate) fn token(&self) -> &RuntimeLeaseToken {
        &self.token
    }

    /// Cheap local check for code that cannot await. The authoritative check
    /// is [`Self::assert_held`] at the effect boundary.
    pub fn assert_not_lost(&self) -> Result<()> {
        if self.is_lost() {
            anyhow::bail!(
                "runtime lease {} generation {} was lost",
                self.lease_key,
                self.fencing_token
            );
        }
        Ok(())
    }

    /// Query the database and fail if the row is missing, expired, released,
    /// or owned by a newer token. This is the fence the Integrator uses before
    /// mutating the session branch.
    pub async fn assert_held(&self) -> Result<vp_store::store::RuntimeLease> {
        self.assert_not_lost()?;
        let store = self.store.clone();
        let key = self.lease_key.clone();
        let owner = self.owner_id.clone();
        let token = self.token.clone();
        let result =
            tokio::task::spawn_blocking(move || store.assert_runtime_lease(&key, &owner, &token))
                .await?;
        match result {
            Ok(lease) => Ok(lease),
            Err(error) => {
                self.lost.store(true, Ordering::SeqCst);
                self.lost_notify.notify_waiters();
                Err(anyhow!(error))
            }
        }
    }

    /// Await the moment this guard loses ownership. The future returns
    /// immediately when the flag is already set. Timers and cancellation are
    /// the caller's responsibility; this is a notification, not a queue.
    pub async fn lost_signal(&self) {
        if self.is_lost() {
            return;
        }
        self.lost_notify.notified().await;
    }

    fn abort_heartbeat(&mut self) {
        if let Some(heartbeat) = self.heartbeat.take() {
            heartbeat.abort();
        }
    }

    fn release_sync(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        self.abort_heartbeat();
        let _ = self
            .store
            .release_runtime_lease(&self.lease_key, &self.owner_id, &self.token);
    }

    pub async fn release(mut self) {
        self.abort_heartbeat();
        if self.released {
            return;
        }
        let failed = {
            let store = self.store.clone();
            let key = self.lease_key.clone();
            let owner = self.owner_id.clone();
            let token = self.token.clone();
            tokio::task::spawn_blocking(move || {
                store.release_runtime_lease(&key, &owner, &token).is_err()
            })
            .await
            .unwrap_or(true)
        };
        self.released = true;
        if failed {
            tracing::warn!(
                lease_key = %self.lease_key,
                owner_id = %self.owner_id,
                generation = self.fencing_token,
                "runtime lease was already lost before explicit release"
            );
        }
    }
}

impl Drop for RuntimeLeaseGuard {
    fn drop(&mut self) {
        // Best effort: normal callers await `release`, but a dropped guard
        // (including a panic unwind) should not pin the key until TTL.
        self.release_sync();
    }
}

/// A held daemon-wide lease over an interactive surface such as the browser.
/// It has its own SQLite row and audit events; it is not a worktree write
/// scope. Dropping the guard stops the heartbeat and leaves the row to expire
/// after the TTL, which is the crash path. Normal callers release explicitly.
pub struct ComputerLeaseGuard {
    store: Store,
    events: broadcast::Sender<Event>,
    pub lease: vp_store::store::ComputerLease,
    heartbeat: Option<tokio::task::JoinHandle<()>>,
}

impl ComputerLeaseGuard {
    pub fn lease_id(&self) -> &str {
        &self.lease.lease_id
    }

    pub fn surface(&self) -> &str {
        &self.lease.surface
    }

    pub async fn release(mut self) {
        if let Some(heartbeat) = self.heartbeat.take() {
            heartbeat.abort();
        }
        let store = self.store.clone();
        let lease_id = self.lease.lease_id.clone();
        let owner = self.lease.owner_id.clone();
        let event = tokio::task::spawn_blocking(move || {
            store.release_computer_lease(&lease_id, Some(&owner))
        })
        .await
        .ok()
        .and_then(Result::ok)
        .flatten();
        if let Some(event) = event {
            let _ = self.events.send(event);
        }
    }
}

impl Drop for ComputerLeaseGuard {
    fn drop(&mut self) {
        if let Some(heartbeat) = self.heartbeat.take() {
            heartbeat.abort();
        }
    }
}

fn validate_approval_template_name(
    value: &str,
) -> std::result::Result<String, crate::approvals::ApprovalRuleError> {
    let name = value.trim();
    if name.is_empty() || name.chars().count() > 120 {
        return Err(crate::approvals::ApprovalRuleError::invalid(
            "approval template name must be 1-120 characters",
        ));
    }
    if name.chars().any(char::is_control) {
        return Err(crate::approvals::ApprovalRuleError::invalid(
            "approval template name cannot contain control characters",
        ));
    }
    Ok(name.to_string())
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
        "provenance": artifact.provenance,
    })
}

pub(crate) fn session_budget_exhausted(session: &Session) -> bool {
    if session.unlimited {
        return false;
    }
    let token_spent = session.tokens_in.saturating_add(session.tokens_out);
    if let Some(limit) = session.token_budget
        && token_spent >= limit
    {
        return true;
    }
    if let Some(limit) = session.cost_budget_usd
        && session.cost_spent_usd >= limit
    {
        return true;
    }
    false
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
    bucket.cache_read_tokens = bucket
        .cache_read_tokens
        .saturating_add(entry.cache_read_tokens);
    bucket.cache_write_tokens = bucket
        .cache_write_tokens
        .saturating_add(entry.cache_write_tokens);
    bucket.cache_hit_ratio = vp_protocol::types::cache_hit_ratio(
        bucket.input_tokens,
        bucket.cache_read_tokens,
        bucket.cache_write_tokens,
    );
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

pub(crate) fn sha256_hex(data: &[u8]) -> String {
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
    for (name, days) in [
        ("task_result_verify_days", policy.task_result_verify_days),
        ("default_verify_days", policy.default_verify_days),
    ] {
        if let Some(days) = days
            && !(1..=36_500).contains(&days)
        {
            anyhow::bail!("memory {name} must be between 1 and 36500 (or null to disable)");
        }
    }
    if let Some(max_chars) = policy.brief_max_chars
        && !(200..=100_000).contains(&max_chars)
    {
        anyhow::bail!("memory brief_max_chars must be between 200 and 100000");
    }
    if let Some(embedding) = &policy.embedding {
        crate::memory_embedding::validate_embedding_policy(embedding)?;
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

/// Backward-compatible alias for the CLI provider kinds used by existing tests
/// and callers. `ForgeProviderKind` is the single source of provider metadata.
pub(crate) use crate::forge::ForgeProviderKind as PrCli;

fn detect_pr_cli(origin: &str) -> PrCli {
    crate::forge::detect_provider(origin)
}

fn selection_source_id(source: crate::forge::ForgeSelectionSource) -> &'static str {
    match source {
        crate::forge::ForgeSelectionSource::Explicit => "explicit",
        crate::forge::ForgeSelectionSource::Environment => "environment",
        crate::forge::ForgeSelectionSource::Detected => "detected",
    }
}

/// RFC3339 UTC cutoff used by GUI timeline retention. `days` is validated by
/// the caller; saturating arithmetic avoids a panic on an absurd policy.
fn gui_retention_cutoff(days: u32) -> Result<String> {
    let cutoff = time::OffsetDateTime::now_utc()
        .checked_sub(time::Duration::days(i64::from(days)))
        .ok_or_else(|| anyhow!("GUI retention cutoff is out of range"))?;
    cutoff
        .format(&time::format_description::well_known::Rfc3339)
        .context("formatting GUI retention cutoff")
}

fn gui_prune_result(
    outcome: vp_store::store::GuiTimelinePruneOutcome,
) -> vp_protocol::types::GuiTimelinePruneResult {
    vp_protocol::types::GuiTimelinePruneResult {
        project_id: outcome.project_id,
        retention_days: outcome.retention_days,
        cutoff: outcome.cutoff,
        dry_run: outcome.dry_run,
        pruned: outcome.pruned,
        remaining: outcome.remaining,
        sessions: outcome.sessions,
        skipped_sessions: outcome.skipped_sessions,
        skipped_events: outcome.skipped_events,
        other_kinds_touched: outcome.other_kinds_touched,
        limit: outcome.limit,
        truncated: outcome.truncated,
        reason: outcome.reason,
        affected_sessions: outcome.affected_sessions,
    }
}

impl Engine {
    pub fn new(store: Store) -> Self {
        let (events, _) = broadcast::channel(2048);
        let data_dir = store.data_dir().to_path_buf();
        let runtime_id = store.owner_id_for_nonce(&Uuid::now_v7().to_string());
        Self {
            scheduler: Arc::new(scheduler::TaskScheduler::new(store.clone())),
            process_supervisor: Arc::new(process_supervisor::ProcessSupervisor::new(store.clone())),
            scaling: Arc::new(scaling::ScalingCoordinator::default()),
            store,
            events,
            approval_waiters: Arc::new(Mutex::new(HashMap::new())),
            question_waiters: Arc::new(Mutex::new(HashMap::new())),
            git_lock: Arc::new(Mutex::new(())),
            budget_registry: Arc::new(BudgetRegistry::new()),
            unlimited_flags: Arc::new(Mutex::new(HashMap::new())),
            mcp: Arc::new(mcp::McpManager::from_env()),
            remote: Arc::new(remote::RemoteManager::new(data_dir.clone())),
            consoles: Arc::new(console::ConsoleManager::new()),
            terminals: Arc::new(terminal::TerminalManager::new()),
            browsers: Arc::new(browser::BrowserManager::new(data_dir.clone())),
            computers: Arc::new(computer::ComputerManager::new(data_dir)),
            gui: Arc::new(gui::GuiCoordinator::new()),
            runtime_id,
            continuation_claims: Arc::new(Mutex::new(HashMap::new())),
            pin_sync: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Replace the remote manager after construction. The daemon always uses
    /// [`Engine::new`] (SSH connector); contract tests use this to attach an
    /// in-process helper stream without bypassing the real handshake.
    pub fn with_remote_manager(store: Store, remote: remote::RemoteManager) -> Self {
        let mut engine = Self::new(store);
        engine.remote = Arc::new(remote);
        engine
    }

    pub fn daemon_identity(&self) -> &vp_store::store::DaemonIdentity {
        self.store.daemon_identity()
    }

    pub fn owner_instance(&self) -> String {
        self.store.process_owner_instance()
    }

    pub fn budget_registry(&self) -> &Arc<BudgetRegistry> {
        &self.budget_registry
    }

    /// Shared runtime flag for a session's budget mode. A running agent loop
    /// holds the same `Arc<AtomicBool>`, so an explicit toggle takes effect at
    /// the next turn/tool boundary without restarting the session.
    pub fn unlimited_flag(&self, session_id: &str, initial: bool) -> Arc<AtomicBool> {
        let mut flags = match self.unlimited_flags.lock() {
            Ok(flags) => flags,
            Err(_) => return Arc::new(AtomicBool::new(initial)),
        };
        flags
            .entry(session_id.to_string())
            .or_insert_with(|| Arc::new(AtomicBool::new(initial)))
            .clone()
    }

    pub fn update_unlimited_flag(&self, session_id: &str, unlimited: bool) -> Arc<AtomicBool> {
        let mut flags = match self.unlimited_flags.lock() {
            Ok(flags) => flags,
            Err(_) => return Arc::new(AtomicBool::new(unlimited)),
        };
        let flag = flags
            .entry(session_id.to_string())
            .or_insert_with(|| Arc::new(AtomicBool::new(unlimited)))
            .clone();
        flag.store(unlimited, Ordering::SeqCst);
        flag
    }

    pub fn clear_unlimited_flag(&self, session_id: &str) {
        if let Ok(mut flags) = self.unlimited_flags.lock() {
            flags.remove(session_id);
        }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn remote_manager(&self) -> &Arc<remote::RemoteManager> {
        &self.remote
    }

    pub fn scheduler(&self) -> &Arc<scheduler::TaskScheduler> {
        &self.scheduler
    }

    pub fn runtime_id(&self) -> &str {
        &self.runtime_id
    }

    /// Acquire a named runtime lease (for example `integration:{session}`).
    /// The returned guard carries the random acquisition token and monotonic
    /// generation; all later renew/release/assert operations are fenced.
    pub async fn acquire_runtime_lease(
        &self,
        kind: &str,
        resource: &str,
        ttl: std::time::Duration,
    ) -> Result<RuntimeLeaseGuard> {
        let ttl_secs = ttl.as_secs().clamp(10, 86_400);
        let lease_key = format!("{kind}:{resource}");
        let owner_id = self.runtime_id.clone();
        let owner_instance = self.owner_instance();
        let store = self.store.clone();
        let key = lease_key.clone();
        let owner = owner_id.clone();
        let instance = owner_instance.clone();
        let kind_owned = kind.to_string();
        let resource_owned = resource.to_string();
        let acquisition = tokio::task::spawn_blocking(move || {
            store.acquire_runtime_lease(
                &key,
                &owner,
                &instance,
                &kind_owned,
                Some(&resource_owned),
                ttl_secs,
            )
        })
        .await??;
        let mut guard = RuntimeLeaseGuard::from_acquisition(self.store.clone(), acquisition);
        let heartbeat_store = self.store.clone();
        let heartbeat_key = guard.lease_key.clone();
        let heartbeat_owner = guard.owner_id.clone();
        let heartbeat_token = guard.token.clone();
        let heartbeat_lost = guard.lost.clone();
        let heartbeat_notify = guard.lost_notify.clone();
        let heartbeat_secs = (ttl_secs / 3).max(5);
        let heartbeat = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(heartbeat_secs));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            ticker.tick().await;
            loop {
                ticker.tick().await;
                let store = heartbeat_store.clone();
                let key = heartbeat_key.clone();
                let owner = heartbeat_owner.clone();
                let token = heartbeat_token.clone();
                let renewed = tokio::task::spawn_blocking(move || {
                    store
                        .renew_runtime_lease(&key, &owner, &token, ttl_secs)
                        .is_ok()
                })
                .await
                .unwrap_or(false);
                if !renewed {
                    heartbeat_lost.store(true, Ordering::SeqCst);
                    heartbeat_notify.notify_waiters();
                    break;
                }
            }
        });
        guard.heartbeat = Some(heartbeat);
        Ok(guard)
    }

    /// Acquire the cross-process lease for a session run. The database row is
    /// the authority; the in-process run map is only a fast path. A lease
    /// heartbeat keeps a long run from expiring, and an expired lease from a
    /// crashed daemon can be taken over after the TTL.
    pub async fn acquire_session_lease(
        &self,
        session_id: &str,
        ttl: std::time::Duration,
    ) -> Result<SessionLease> {
        let ttl_secs = ttl.as_secs().clamp(10, 86_400);
        let lease_key = format!("session:{session_id}");
        let owner_id = self.runtime_id.clone();
        let owner_instance = self.owner_instance();
        let store = self.store.clone();
        let key = lease_key.clone();
        let owner = owner_id.clone();
        let instance = owner_instance.clone();
        let session = session_id.to_string();
        let acquisition = match tokio::task::spawn_blocking(move || {
            store.acquire_runtime_lease(
                &key,
                &owner,
                &instance,
                "session",
                Some(&session),
                ttl_secs,
            )
        })
        .await?
        {
            Ok(acquisition) => acquisition,
            Err(vp_store::store::RuntimeLeaseError::Busy {
                holder: Some(holder),
            }) => {
                anyhow::bail!(
                    "session {session_id} is already running (held by {} on {} until {})",
                    holder.owner_id,
                    holder.owner_instance,
                    holder.expires_at
                );
            }
            Err(vp_store::store::RuntimeLeaseError::Busy { holder: None }) => {
                anyhow::bail!("session {session_id} is already running");
            }
            Err(error) => return Err(anyhow!(error)),
        };
        let fencing_token = acquisition.fencing_token();
        let token = acquisition.token().clone();
        let event = self
            .append_event(
                session_id,
                "session.lease.acquired",
                serde_json::json!({
                    "lease_key": lease_key,
                    "owner_id": owner_id,
                    "owner_instance": owner_instance,
                    "fencing_token": fencing_token,
                    "ttl_secs": ttl_secs,
                }),
            )
            .await;
        if let Err(error) = event {
            let release_store = self.store.clone();
            let release_key = lease_key.clone();
            let release_owner = owner_id.clone();
            let release_token = token.clone();
            let _ = tokio::task::spawn_blocking(move || {
                release_store.release_runtime_lease(&release_key, &release_owner, &release_token)
            })
            .await;
            return Err(error);
        }
        let lost = Arc::new(AtomicBool::new(false));
        let heartbeat_store = self.store.clone();
        let heartbeat_key = lease_key.clone();
        let heartbeat_owner = owner_id.clone();
        let heartbeat_token = token.clone();
        let heartbeat_lost = lost.clone();
        let heartbeat_secs = (ttl_secs / 3).max(5);
        let heartbeat = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(heartbeat_secs));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            ticker.tick().await;
            loop {
                ticker.tick().await;
                let store = heartbeat_store.clone();
                let key = heartbeat_key.clone();
                let owner = heartbeat_owner.clone();
                let token = heartbeat_token.clone();
                let renewed = tokio::task::spawn_blocking(move || {
                    store
                        .renew_runtime_lease(&key, &owner, &token, ttl_secs)
                        .is_ok()
                })
                .await
                .unwrap_or(false);
                if !renewed {
                    heartbeat_lost.store(true, Ordering::SeqCst);
                    break;
                }
            }
        });
        Ok(SessionLease {
            store: self.store.clone(),
            events: self.events.clone(),
            lease_key,
            owner_id,
            owner_instance,
            session_id: session_id.to_string(),
            fencing_token,
            token,
            lost,
            released: false,
            heartbeat: Some(heartbeat),
        })
    }

    /// Acquire a runtime lease at an explicit clock for deterministic tests and
    /// recovery drills. Production callers use `acquire_runtime_lease`.
    #[cfg(test)]
    pub(crate) async fn acquire_runtime_lease_at_for_test(
        &self,
        kind: &str,
        resource: &str,
        ttl_secs: u64,
        now: time::OffsetDateTime,
    ) -> Result<RuntimeLeaseGuard> {
        let lease_key = format!("{kind}:{resource}");
        let store = self.store.clone();
        let key = lease_key.clone();
        let owner = self.runtime_id.clone();
        let instance = self.owner_instance();
        let kind_owned = kind.to_string();
        let resource_owned = resource.to_string();
        let acquisition = tokio::task::spawn_blocking(move || {
            store.acquire_runtime_lease_at(
                &key,
                &owner,
                &instance,
                &kind_owned,
                Some(&resource_owned),
                ttl_secs,
                now,
            )
        })
        .await??;
        Ok(RuntimeLeaseGuard::from_acquisition(
            self.store.clone(),
            acquisition,
        ))
    }

    pub async fn orchestration_candidates(&self, limit: u32) -> Result<Vec<String>> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.orchestration_candidate_sessions(limit)).await?
    }

    pub async fn list_runtime_leases(&self) -> Result<Vec<vp_store::store::RuntimeLease>> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.list_runtime_leases()).await?
    }

    pub async fn sweep_runtime_leases(&self) -> Result<usize> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.clear_expired_runtime_leases()).await?
    }

    /// Token-free lease history for operators. The audit is operational state,
    /// not a replay projection.
    pub async fn runtime_lease_audit(
        &self,
        lease_key: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<vp_store::store::RuntimeLeaseAuditEntry>> {
        let store = self.store.clone();
        let lease_key = lease_key.map(str::to_string);
        tokio::task::spawn_blocking(move || {
            store.list_runtime_lease_audit(lease_key.as_deref(), limit)
        })
        .await?
    }

    /// Bound the operational lease audit table. Defaults to 30 days and runs
    /// from the daemon's existing lease sweep loop.
    pub async fn prune_runtime_lease_audit(&self, retention_days: u64) -> Result<usize> {
        let days = retention_days.clamp(1, 365);
        let cutoff = time::OffsetDateTime::now_utc() - time::Duration::days(days as i64);
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.prune_runtime_lease_audit(cutoff)).await?
    }

    /// Acquire the daemon-wide browser/GUI surface lease. The SQLite row is
    /// the authority; an unexpired lease on the same surface fails fast with
    /// the holder instead of queueing. The guard heartbeats at TTL/3.
    pub async fn acquire_computer_lease(
        &self,
        surface: &str,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        ttl: std::time::Duration,
    ) -> Result<ComputerLeaseGuard> {
        let ttl_secs = ttl.as_secs().clamp(10, 3_600);
        let store = self.store.clone();
        let owner_id = self.runtime_id.clone();
        let surface_owned = surface.to_string();
        let session_owned = session_id.to_string();
        let agent_owned = agent_id.to_string();
        let agent_name_owned = agent_name.to_string();
        let attempt = tokio::task::spawn_blocking(move || {
            store.acquire_computer_lease(
                &surface_owned,
                &owner_id,
                &session_owned,
                &agent_owned,
                &agent_name_owned,
                ttl_secs,
            )
        })
        .await??;
        match attempt {
            vp_store::store::ComputerLeaseAttempt::Acquired { lease, events } => {
                for event in events {
                    let _ = self.events.send(event);
                }
                let heartbeat_store = self.store.clone();
                let heartbeat_owner = lease.owner_id.clone();
                let heartbeat_lease = lease.lease_id.clone();
                let heartbeat_secs = (ttl_secs / 3).max(5);
                let heartbeat = tokio::spawn(async move {
                    let mut ticker =
                        tokio::time::interval(std::time::Duration::from_secs(heartbeat_secs));
                    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    ticker.tick().await;
                    loop {
                        ticker.tick().await;
                        let store = heartbeat_store.clone();
                        let owner = heartbeat_owner.clone();
                        let lease = heartbeat_lease.clone();
                        let renewed = tokio::task::spawn_blocking(move || {
                            store.renew_computer_lease(&lease, &owner, ttl_secs)
                        })
                        .await
                        .ok()
                        .and_then(Result::ok)
                        .unwrap_or(false);
                        if !renewed {
                            break;
                        }
                    }
                });
                Ok(ComputerLeaseGuard {
                    store: self.store.clone(),
                    events: self.events.clone(),
                    lease,
                    heartbeat: Some(heartbeat),
                })
            }
            vp_store::store::ComputerLeaseAttempt::Busy { holder, events } => {
                for event in events {
                    let _ = self.events.send(event);
                }
                let detail = holder
                    .map(|holder| {
                        format!(
                            "held by session {} agent {} until {}",
                            holder.session_id, holder.agent_name, holder.expires_at
                        )
                    })
                    .unwrap_or_else(|| "held by another runtime".to_string());
                anyhow::bail!("{surface} computer surface is busy ({detail})")
            }
        }
    }

    /// Operator release of a stuck surface lease. Normal agents release their
    /// guard, which also enforces the owner id.
    pub async fn release_computer_lease(&self, lease_id: &str) -> Result<bool> {
        let store = self.store.clone();
        let lease_id = lease_id.to_string();
        let event =
            tokio::task::spawn_blocking(move || store.release_computer_lease(&lease_id, None))
                .await??;
        if let Some(event) = event {
            let _ = self.events.send(event);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub async fn list_computer_leases(&self) -> Result<Vec<vp_store::store::ComputerLease>> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.list_computer_leases()).await?
    }

    pub async fn computer_lease_active(&self, lease_id: &str) -> Result<bool> {
        let store = self.store.clone();
        let lease_id = lease_id.to_string();
        let lease = tokio::task::spawn_blocking(move || store.computer_lease(&lease_id)).await??;
        Ok(lease.is_some())
    }

    pub async fn sweep_computer_leases(&self) -> Result<usize> {
        let store = self.store.clone();
        let (removed, events) =
            tokio::task::spawn_blocking(move || store.clear_expired_computer_leases()).await??;
        for event in events {
            let _ = self.events.send(event);
        }
        Ok(removed)
    }

    /// Retire legacy `browser`/`computer` rows at daemon startup. Helper
    /// processes are daemon children, so no legacy helper survives a restart.
    pub async fn retire_legacy_gui_leases(&self) -> Result<usize> {
        let store = self.store.clone();
        let (removed, events) = tokio::task::spawn_blocking(move || {
            store.clear_legacy_gui_leases("legacy GUI surface retired at daemon startup")
        })
        .await??;
        for event in events {
            let _ = self.events.send(event);
        }
        if removed > 0 {
            tracing::info!(removed, "retired legacy browser/computer GUI leases");
        }
        Ok(removed)
    }

    pub async fn gui_lease_holder(&self) -> Result<Option<vp_store::store::ComputerLease>> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.gui_lease_holder()).await?
    }

    pub async fn get_gui_policy(&self, project_id: &str) -> Result<vp_protocol::types::GuiPolicy> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking(move || store.get_gui_policy(&project_id)).await?
    }

    pub async fn set_gui_policy(
        &self,
        project_id: &str,
        policy: vp_protocol::types::GuiPolicy,
    ) -> Result<vp_protocol::types::GuiPolicy> {
        policy.validate().map_err(|error| anyhow::anyhow!(error))?;
        let policy = policy.normalized();
        let store = self.store.clone();
        let project = project_id.to_string();
        tokio::task::spawn_blocking({
            let store = store.clone();
            move || store.get_project(&project)
        })
        .await??
        .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        let for_store = policy.clone();
        let project = project_id.to_string();
        tokio::task::spawn_blocking(move || store.set_gui_policy(&project, &for_store)).await??;
        if let Err(error) = self.record_project_manifest().await {
            tracing::warn!(%error, "failed to record GUI policy in the project manifest");
        }
        Ok(policy)
    }

    pub async fn gui_policy_for_session(
        &self,
        session_id: &str,
    ) -> Result<vp_protocol::types::GuiPolicy> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.gui_policy_for_session(&session_id)).await?
    }

    /// Acquire or wait for the single physical GUI surface. The wait is bounded
    /// by the project policy; a timeout returns a typed `GUI_BUSY` with the
    /// holder and retry hint. There is no hidden infinite wait.
    pub async fn gui_begin_action(
        &self,
        surface: gui::GuiSurface,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
    ) -> Result<gui::GuiAcquire> {
        let policy = self.gui_policy_for_session(session_id).await?;
        self.gui_begin_action_with_policy(&policy, surface, session_id, agent_id, agent_name)
            .await
    }

    /// Same acquisition with a policy the caller already loaded. Managers use
    /// this so one action performs one project-policy lookup.
    pub async fn gui_begin_action_with_policy(
        &self,
        policy: &vp_protocol::types::GuiPolicy,
        surface: gui::GuiSurface,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
    ) -> Result<gui::GuiAcquire> {
        self.gui
            .begin(self, policy, surface, session_id, agent_id, agent_name)
            .await
    }

    /// Record one bounded GUI action event. Screen content, typed text, and
    /// element values are never included. A failed event append does not fail
    /// the completed GUI action.
    pub async fn gui_finish_action(
        &self,
        ticket: &gui::GuiActionTicket,
        outcome: gui::GuiActionOutcome,
    ) -> Option<vp_protocol::types::GuiActionInfo> {
        let action = self.gui.finish(ticket, outcome).await;
        let payload = match serde_json::to_value(&action) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::warn!(%error, "failed to serialize GUI action event");
                return None;
            }
        };
        if let Err(error) = self
            .append_event(&ticket.session_id, "gui.action", payload)
            .await
        {
            tracing::warn!(%error, "failed to append GUI action event");
        }
        Some(action)
    }

    /// Release the GUI lease immediately on close or session cleanup.
    pub async fn gui_end_session(&self, session_id: &str, reason: &str) -> bool {
        self.gui.release_session(session_id, reason).await
    }

    /// Release leases whose grace elapsed. Called by the short GUI sweeper.
    pub async fn sweep_gui_grace(&self) -> usize {
        self.gui.sweep().await
    }

    /// Lightweight holder/FIFO snapshot used by dashboard alerts. Unlike
    /// `gui_status` this does not touch helper/TCC status.
    pub async fn gui_lease_snapshot(
        &self,
    ) -> (
        Option<vp_protocol::types::GuiLeaseInfo>,
        Vec<vp_protocol::types::GuiQueueInfo>,
    ) {
        let (mut holder, queue) = self.gui.status().await;
        if let Some(holder) = holder.as_mut() {
            holder.owner_id = self.runtime_id.clone();
        } else if let Ok(Some(lease)) = self.gui_lease_holder().await {
            holder = Some(vp_protocol::types::GuiLeaseInfo {
                lease_id: lease.lease_id,
                owner_id: lease.owner_id,
                session_id: lease.session_id,
                agent_id: lease.agent_id,
                agent_name: lease.agent_name,
                surface: lease.surface,
                lease_generation: lease.generation,
                acquired_at: lease.acquired_at,
                last_action_at: lease.heartbeat_at,
                expires_at: lease.expires_at,
                queue_position: None,
            });
        }
        (holder, queue)
    }

    pub async fn gui_warm_helper_counts(&self) -> (usize, usize) {
        (self.computers.warm_helpers(), self.browsers.warm_helpers())
    }

    pub async fn gui_timeline(
        &self,
        session_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<vp_protocol::types::GuiActionInfo>> {
        let store = self.store.clone();
        let session_id = session_id.map(str::to_string);
        tokio::task::spawn_blocking(move || store.recent_gui_actions(session_id.as_deref(), limit))
            .await?
    }

    /// Bounded audit history for GUI timeline prunes. Entries contain counts
    /// and cutoffs only; pruned action payloads are gone and are never
    /// reconstructed.
    pub async fn gui_timeline_audit(
        &self,
        project_id: &str,
        limit: Option<u32>,
    ) -> Result<vp_protocol::types::GuiTimelineAuditResult> {
        let store = self.store.clone();
        let project = project_id.to_string();
        let limit = limit.unwrap_or(50).clamp(1, 1_000);
        let (entries, total) =
            tokio::task::spawn_blocking(move || store.gui_timeline_prune_audit(&project, limit))
                .await??;
        Ok(vp_protocol::types::GuiTimelineAuditResult {
            project_id: project_id.to_string(),
            entries,
            total,
            limit,
        })
    }

    /// Read-only retention stats for the GUI action timeline. It performs one
    /// COUNT/MIN/MAX query and never reads payloads or mirrors.
    pub async fn gui_timeline_retention(
        &self,
        project_id: &str,
        older_than_days: Option<u32>,
    ) -> Result<vp_protocol::types::GuiTimelineRetentionResult> {
        let policy = self.get_gui_policy(project_id).await?;
        let days = older_than_days.unwrap_or(policy.timeline_retention_days);
        if days > vp_protocol::types::GuiPolicy::MAX_TIMELINE_RETENTION_DAYS {
            anyhow::bail!(
                "older_than_days must be 0..={}",
                vp_protocol::types::GuiPolicy::MAX_TIMELINE_RETENTION_DAYS
            );
        }
        let cutoff = if days == 0 {
            None
        } else {
            Some(gui_retention_cutoff(days)?)
        };
        let store = self.store.clone();
        let project = project_id.to_string();
        let cutoff_for_query = cutoff.clone();
        let stats = tokio::task::spawn_blocking(move || {
            store.gui_action_retention_stats(&project, cutoff_for_query.as_deref())
        })
        .await??;
        Ok(vp_protocol::types::GuiTimelineRetentionResult {
            project_id: project_id.to_string(),
            retention_days: days,
            keep_forever: days == 0,
            cutoff,
            total: stats.total,
            eligible: stats.eligible,
            oldest_at: stats.oldest_at,
            newest_at: stats.newest_at,
        })
    }

    /// Explicit, bounded prune of the GUI action timeline for one project.
    /// `older_than_days = 0` means "use the project policy"; an explicit
    /// `Some(0)` from an RPC is rejected before reaching this method because
    /// pruning everything is not a retention operation.
    pub async fn prune_gui_timeline(
        &self,
        project_id: &str,
        older_than_days: Option<u32>,
        dry_run: bool,
        limit: Option<u32>,
    ) -> Result<vp_protocol::types::GuiTimelinePruneResult> {
        let policy = self.get_gui_policy(project_id).await?;
        let days = older_than_days.unwrap_or(policy.timeline_retention_days);
        if days == 0 {
            anyhow::bail!(
                "GUI timeline retention is 0 (keep forever); set a positive retention or pass older_than_days"
            );
        }
        if days > vp_protocol::types::GuiPolicy::MAX_TIMELINE_RETENTION_DAYS {
            anyhow::bail!(
                "older_than_days must be 1..={}",
                vp_protocol::types::GuiPolicy::MAX_TIMELINE_RETENTION_DAYS
            );
        }
        let cutoff = gui_retention_cutoff(days)?;
        let store = self.store.clone();
        let project = project_id.to_string();
        let outcome = tokio::task::spawn_blocking(move || {
            store.prune_gui_action_events(&project, &cutoff, days, limit.unwrap_or(1_000), dry_run)
        })
        .await??;
        Ok(gui_prune_result(outcome))
    }

    /// Daemon sweep: apply each project's `timeline_retention_days` with a
    /// bounded batch. `0` keeps events forever and is skipped. Errors are
    /// returned per project so the daemon can log without stopping other
    /// projects; a partial prune is never reported as complete.
    pub async fn sweep_gui_timeline_retention(
        &self,
        limit_per_project: u32,
    ) -> Result<Vec<vp_protocol::types::GuiTimelinePruneResult>> {
        let store = self.store.clone();
        let projects = tokio::task::spawn_blocking(move || store.list_projects()).await??;
        let mut results = Vec::new();
        for project in projects {
            let policy = self.get_gui_policy(&project.id).await?;
            if policy.timeline_retention_days == 0 {
                continue;
            }
            match self
                .prune_gui_timeline(
                    &project.id,
                    Some(policy.timeline_retention_days),
                    false,
                    Some(limit_per_project),
                )
                .await
            {
                Ok(result) if result.pruned > 0 || result.skipped_events > 0 => {
                    results.push(result)
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(
                        %error,
                        project_id = %project.id,
                        "GUI timeline retention sweep failed for project"
                    );
                }
            }
        }
        Ok(results)
    }

    /// Unified GUI status for Manage Dash, the CLI, and `gui.status`. The
    /// computer status path is cached for the project `status_ttl_secs`.
    pub async fn gui_status(
        &self,
        project_id: Option<&str>,
    ) -> Result<vp_protocol::types::GuiStatusResult> {
        let policy = match project_id {
            Some(project_id) => self.get_gui_policy(project_id).await?,
            None => vp_protocol::types::GuiPolicy::default(),
        };
        let computer = self.computers.status(&policy, false).await;
        let browser = self.browsers.status();
        let (mut holder, queue) = self.gui.status().await;
        if let Some(holder) = holder.as_mut() {
            holder.owner_id = self.runtime_id.clone();
        } else if let Ok(Some(lease)) = self.gui_lease_holder().await {
            holder = Some(vp_protocol::types::GuiLeaseInfo {
                lease_id: lease.lease_id,
                owner_id: lease.owner_id,
                session_id: lease.session_id,
                agent_id: lease.agent_id,
                agent_name: lease.agent_name,
                surface: lease.surface,
                lease_generation: lease.generation,
                acquired_at: lease.acquired_at,
                last_action_at: lease.heartbeat_at,
                expires_at: lease.expires_at,
                queue_position: None,
            });
        }
        let (warm_computer_helpers, warm_browser_helpers) = self.gui_warm_helper_counts().await;
        Ok(vp_protocol::types::GuiStatusResult {
            runtime_id: self.runtime_id.clone(),
            lease_surface: gui::GUI_SURFACE.to_string(),
            holder,
            queue,
            policy,
            computer,
            browser_helper_available: browser.helper_available,
            browser_helper_path: browser.helper_path,
            browser_sandbox_available: browser.sandbox_available,
            warm_computer_helpers,
            warm_browser_helpers,
            stale_ref_supported: true,
        })
    }

    pub(crate) fn git_mutex(&self) -> Arc<Mutex<()>> {
        self.git_lock.clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    /// Broadcast an event that was inserted atomically by the store together
    /// with another projection update. Do not call `append_event` here: that
    /// would create a second event row for the same transition.
    pub(crate) fn broadcast_event(&self, event: Event) {
        let _ = self.events.send(event);
    }

    pub async fn add_project(&self, root: String, name: Option<String>) -> Result<Project> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.add_project(&root, name.as_deref())).await?
    }

    pub async fn list_projects(&self) -> Result<Vec<Project>> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.list_projects()).await?
    }

    pub async fn get_cache_policy(&self, project_id: &str) -> Result<CachePolicy> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        // One bounded lookup on the hot prompt-construction path. A missing row
        // decodes as the default policy; `set_cache_policy` still validates the
        // project and the values before writing.
        let policy =
            tokio::task::spawn_blocking(move || store.get_cache_policy(&project_id)).await??;
        Ok(policy.normalized().unwrap_or_default())
    }

    pub async fn set_cache_policy(
        &self,
        project_id: &str,
        policy: CachePolicy,
    ) -> Result<CachePolicy> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking({
            let store = store.clone();
            let project_id = project_id.clone();
            move || store.get_project(&project_id)
        })
        .await??
        .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        let policy = policy
            .normalized()
            .map_err(|error| anyhow::anyhow!("invalid cache policy: {error}"))?;
        let policy_for_store = policy.clone();
        tokio::task::spawn_blocking(move || store.set_cache_policy(&project_id, &policy_for_store))
            .await??;
        Ok(policy)
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

    pub async fn get_process_policy(&self, project_id: &str) -> Result<ProcessPolicy> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking(move || {
            if store.get_project(&project_id)?.is_none() {
                return Err(anyhow::anyhow!("project not found: {project_id}"));
            }
            store.get_process_policy(&project_id)
        })
        .await?
    }

    pub async fn set_process_policy(
        &self,
        project_id: &str,
        policy: ProcessPolicy,
    ) -> Result<ProcessPolicy> {
        policy
            .validate()
            .map_err(|error| anyhow::anyhow!("invalid process policy: {error}"))?;
        let store = self.store.clone();
        let project_id_for_store = project_id.to_string();
        let policy_for_store = policy.clone();
        tokio::task::spawn_blocking(move || {
            if store.get_project(&project_id_for_store)?.is_none() {
                return Err(anyhow::anyhow!("project not found: {project_id_for_store}"));
            }
            store.set_process_policy(&project_id_for_store, &policy_for_store)
        })
        .await??;
        Ok(policy)
    }

    /// Project-scoped hard-preemption policy. This never changes cooperative
    /// pause/cancel and never widens a grace past its hard bound.
    pub async fn get_preemption_policy(&self, project_id: &str) -> Result<PreemptionPolicy> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking(move || {
            if store.get_project(&project_id)?.is_none() {
                return Err(anyhow::anyhow!("project not found: {project_id}"));
            }
            store.get_preemption_policy(&project_id)
        })
        .await?
    }

    pub async fn set_preemption_policy(
        &self,
        project_id: &str,
        policy: PreemptionPolicy,
    ) -> Result<PreemptionPolicy> {
        let policy = policy
            .normalized()
            .map_err(|error| anyhow::anyhow!("invalid preemption policy: {error}"))?;
        let store = self.store.clone();
        let project_id_for_store = project_id.to_string();
        let policy_for_store = policy.clone();
        tokio::task::spawn_blocking(move || {
            if store.get_project(&project_id_for_store)?.is_none() {
                return Err(anyhow::anyhow!("project not found: {project_id_for_store}"));
            }
            store.set_preemption_policy(&project_id_for_store, &policy_for_store)
        })
        .await??;
        Ok(policy)
    }

    /// Event-derived preemption history. This intentionally reads the session
    /// event stream rather than a mutable projection, so replay, repair, bundle
    /// import, and `db-rebuild` all reproduce the same records.
    pub async fn session_preemption(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<SessionPreemptionResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        let policy = self.get_preemption_policy(&session.project_id).await?;
        let events = self
            .recent_events_of_kind(session_id, "session.preempted", 1_000)
            .await?;
        let provider_events = self
            .recent_events_of_kind(session_id, "provider.preempted", 1_000)
            .await?;
        let tool_events = self
            .recent_events_of_kind(session_id, "tool.preempted", 1_000)
            .await?;
        let mut records = Vec::new();
        for event in provider_events.into_iter().chain(tool_events).chain(events) {
            if let Ok(record) = serde_json::from_value::<PreemptionRecord>(event.payload) {
                records.push(record);
            }
        }
        records.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.phase.cmp(&right.phase))
        });
        let cap = limit.clamp(1, 1_000) as usize;
        if records.len() > cap {
            records = records.split_off(records.len() - cap);
        }
        let mut phases: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
        let mut reasons: std::collections::BTreeMap<String, u64> =
            std::collections::BTreeMap::new();
        let mut terminal = false;
        for record in &records {
            if !record.terminal {
                continue;
            }
            terminal = true;
            *phases.entry(record.phase.clone()).or_default() += 1;
            *reasons.entry(record.reason.clone()).or_default() += 1;
        }
        Ok(SessionPreemptionResult {
            session_id: session_id.to_string(),
            policy,
            active: false,
            preempted: terminal,
            records,
            by_phase: phases
                .into_iter()
                .map(|(key, count)| MetricCount { key, count })
                .collect(),
            by_reason: reasons
                .into_iter()
                .map(|(key, count)| MetricCount { key, count })
                .collect(),
        })
    }

    /// Event-derived project overview for Manage Dash and operational audit.
    pub async fn preemption_overview(
        &self,
        project_id: &str,
        limit: u32,
    ) -> Result<PreemptionOverviewResult> {
        let policy = self.get_preemption_policy(project_id).await?;
        let store = self.store.clone();
        let project_id_string = project_id.to_string();
        let (sessions, by_phase, by_reason) = tokio::task::spawn_blocking(move || {
            store.preemption_overview(&project_id_string, limit)
        })
        .await??;
        let total = sessions
            .iter()
            .map(|summary| summary.terminal_count)
            .sum::<u64>();
        Ok(PreemptionOverviewResult {
            project_id: project_id.to_string(),
            policy,
            total,
            sessions,
            by_phase,
            by_reason,
        })
    }

    /// Read-only language-server availability for a project checkout. This
    /// never spawns a server; it only checks PATH and the in-process registry.
    pub async fn lsp_status(&self, project_id: &str) -> Result<LspStatusResult> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        let project = tokio::task::spawn_blocking({
            let project_id = project_id.clone();
            move || store.get_project(&project_id)
        })
        .await??
        .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        let report = tokio::task::spawn_blocking(move || {
            crate::tooling::lsp::status_report(Path::new(&project.root))
        })
        .await?;
        Ok(LspStatusResult {
            project_id,
            root: report.root,
            languages: report
                .languages
                .into_iter()
                .map(|language| LspLanguageStatus {
                    language: language.language,
                    display: language.display,
                    server: language.server,
                    installed: language.installed,
                    running: language.running,
                    capabilities: language.capabilities,
                    install_hint: language.install_hint,
                    idle_secs: language.idle_secs,
                })
                .collect(),
            running: report.running,
            installed: report.installed,
            supported_languages: crate::tooling::lsp::all_languages()
                .iter()
                .map(|language| language.id.to_string())
                .collect(),
        })
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
        if !(1..=10).contains(&policy.max_intake_parallel) {
            anyhow::bail!("max intake parallelism must be between 1 and 10");
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

    pub async fn get_tool_output_policy(&self, project_id: &str) -> Result<ToolOutputPolicy> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking({
            let store = store.clone();
            let id = project_id.clone();
            move || store.get_project(&id)
        })
        .await??
        .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        tokio::task::spawn_blocking(move || store.get_tool_output_policy(&project_id)).await?
    }

    pub async fn set_tool_output_policy(
        &self,
        project_id: &str,
        policy: ToolOutputPolicy,
    ) -> Result<ToolOutputPolicyResult> {
        crate::tool_output::validate_tool_output_policy(&policy)?;
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking({
            let store = store.clone();
            let id = project_id.clone();
            move || store.get_project(&id)
        })
        .await??
        .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        let stored = policy.clone();
        let id = project_id.clone();
        tokio::task::spawn_blocking(move || store.set_tool_output_policy(&id, &stored)).await??;
        Ok(ToolOutputPolicyResult { project_id, policy })
    }

    pub async fn tool_output_status(
        &self,
        params: ToolOutputStatusParams,
    ) -> Result<ToolOutputStatus> {
        let project_id = params.project_id.clone();
        let session_id = params.session_id.clone();
        let store = self.store.clone();
        let project_for_policy = project_id.clone();
        let session_for_policy = session_id.clone();
        let policy = tokio::task::spawn_blocking(move || match project_for_policy {
            Some(project_id) => store.get_tool_output_policy(&project_id),
            None => match session_for_policy {
                Some(session_id) => match store.get_session(&session_id)? {
                    Some(session) => store.get_tool_output_policy(&session.project_id),
                    None => Ok(ToolOutputPolicy::default()),
                },
                None => Ok(ToolOutputPolicy::default()),
            },
        })
        .await??;
        let output_store = crate::tool_output::ToolOutputStore::new(self.store.clone());
        tokio::task::spawn_blocking(move || {
            output_store.status(project_id.as_deref(), session_id.as_deref(), policy)
        })
        .await?
    }

    pub async fn list_tool_outputs(
        &self,
        params: ToolOutputListParams,
    ) -> Result<ToolOutputListResult> {
        let output_store = crate::tool_output::ToolOutputStore::new(self.store.clone());
        tokio::task::spawn_blocking(move || {
            let limit = params.limit.unwrap_or(100) as usize;
            output_store.list(
                params.project_id.as_deref(),
                params.session_id.as_deref(),
                params.tool_name.as_deref(),
                params.before_created_at.as_deref(),
                params.include_expired,
                limit,
            )
        })
        .await?
    }

    pub async fn prune_tool_outputs(
        &self,
        params: ToolOutputPruneParams,
    ) -> Result<ToolOutputPruneResult> {
        let output_store = crate::tool_output::ToolOutputStore::new(self.store.clone());
        tokio::task::spawn_blocking(move || {
            let policy_project = match params.project_id.as_deref() {
                Some(project_id) => Some(project_id.to_string()),
                None => match params.session_id.as_deref() {
                    Some(session_id) => output_store.session_project_id(session_id)?,
                    None => None,
                },
            };
            let policy = match policy_project.as_deref() {
                Some(project_id) => output_store.policy(project_id)?,
                None => ToolOutputPolicy::default(),
            };
            let older_than_secs = Some(params.older_than_secs.unwrap_or(policy.retention_secs));
            output_store.prune(&vp_store::store::ToolOutputPruneRequest {
                project_id: params.project_id,
                session_id: params.session_id,
                older_than_secs,
                max_entries: params.max_entries.or(Some(policy.max_entries)),
                dry_run: params.dry_run,
            })
        })
        .await?
    }

    pub async fn audit_tool_outputs(
        &self,
        params: ToolOutputAuditParams,
    ) -> Result<ToolOutputAuditResult> {
        let output_store = crate::tool_output::ToolOutputStore::new(self.store.clone());
        tokio::task::spawn_blocking(move || {
            output_store.audit(
                params.project_id.as_deref(),
                params.verify_hashes,
                params.clean_orphans,
            )
        })
        .await?
    }

    /// Daemon retention sweep. It runs per project so each project policy can
    /// protect active sessions and enforce its own entry cap.
    pub async fn sweep_tool_output_retention(&self) -> Result<u64> {
        let projects = self.list_projects().await?;
        let mut removed = 0u64;
        for project in projects {
            let output_store = crate::tool_output::ToolOutputStore::new(self.store.clone());
            let project_id = project.id.clone();
            let report = tokio::task::spawn_blocking(move || {
                let policy = output_store.policy(&project_id)?;
                output_store.prune(&vp_store::store::ToolOutputPruneRequest {
                    project_id: Some(project_id),
                    session_id: None,
                    older_than_secs: Some(policy.retention_secs),
                    max_entries: Some(policy.max_entries),
                    dry_run: false,
                })
            })
            .await??;
            removed = removed.saturating_add(report.removed);
        }
        // Clean crash-left orphan blobs after metadata pruning. This never
        // removes a referenced body and never touches metadata.
        let output_store = crate::tool_output::ToolOutputStore::new(self.store.clone());
        tokio::task::spawn_blocking(move || output_store.audit(None, false, true)).await??;
        Ok(removed)
    }

    pub async fn list_intake_tasks(
        &self,
        project_id: Option<String>,
        status: Option<String>,
        limit: Option<u32>,
    ) -> Result<IntakeListResult> {
        let store = self.store.clone();
        let tasks = tokio::task::spawn_blocking(move || {
            store.list_intake_tasks(project_id.as_deref(), status.as_deref(), limit)
        })
        .await??;
        Ok(IntakeListResult { tasks })
    }

    pub async fn get_intake_task(&self, id: String) -> Result<Option<IntakeTask>> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.get_intake_task(&id)).await?
    }

    /// Validate and persist a standalone task. The daemon scheduler turns it
    /// into a deterministic one-task TeamPlan when a Worker slot is available.
    pub async fn create_intake_task(&self, params: IntakeCreateParams) -> Result<IntakeTask> {
        let draft = build_intake_draft(&params)?;
        let store = self.store.clone();
        let project_id = draft.project_id.clone();
        let deps = draft.deps.clone();
        let existing = tokio::task::spawn_blocking({
            let store = store.clone();
            move || store.list_intake_tasks(Some(&project_id), None, None)
        })
        .await??;
        validate_intake_dependencies(&existing, None, &deps)?;
        let task = tokio::task::spawn_blocking(move || store.create_intake_task(&draft)).await??;
        self.record_project_manifest_best_effort().await;
        Ok(task)
    }

    /// Edit a queued or failed intake task. Saving an edit re-queues it with a
    /// fresh manual attempt budget, matching the task-inbox workflow.
    pub async fn update_intake_task(&self, params: IntakeUpdateParams) -> Result<IntakeTask> {
        let store = self.store.clone();
        let id = params.id.clone();
        let current = tokio::task::spawn_blocking({
            let store = store.clone();
            let id = id.clone();
            move || store.get_intake_task(&id)
        })
        .await??
        .ok_or_else(|| anyhow!("intake task not found: {id}"))?;
        if matches!(
            current.status.as_str(),
            "dispatching" | "running" | "completed"
        ) {
            anyhow::bail!(
                "intake task {id} cannot be edited while it is {}",
                current.status
            );
        }
        let draft = build_intake_draft(&IntakeCreateParams {
            project_id: current.project_id.clone(),
            title: params.title,
            instructions: params.instructions,
            profile: params.profile,
            kind: params.kind,
            priority: params.priority,
            write_scope: params.write_scope,
            acceptance: params.acceptance,
            deps: params.deps,
            token_budget: params.token_budget,
            cost_budget_usd: params.cost_budget_usd,
            time_budget_secs: params.time_budget_secs,
            max_attempts: params.max_attempts,
            labels: params.labels,
            origin: Some(current.origin.clone()),
        })?;
        let existing = tokio::task::spawn_blocking({
            let store = store.clone();
            let project_id = current.project_id.clone();
            move || store.list_intake_tasks(Some(&project_id), None, None)
        })
        .await??;
        validate_intake_dependencies(&existing, Some(&id), &draft.deps)?;
        let updated = tokio::task::spawn_blocking({
            let store = store.clone();
            let id = id.clone();
            move || store.update_intake_task(&id, &draft)
        })
        .await??
        .ok_or_else(|| anyhow!("intake task {id} cannot be edited"))?;
        self.record_project_manifest_best_effort().await;
        Ok(updated)
    }

    pub async fn claim_intake_task(&self, id: String) -> Result<Option<IntakeTask>> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.claim_intake_task(&id)).await?
    }

    pub async fn start_intake_task(
        &self,
        id: String,
        session_id: String,
        task_id: String,
    ) -> Result<IntakeTask> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.start_intake_task(&id, &session_id, &task_id))
            .await?
    }

    pub async fn settle_intake_task(
        &self,
        id: String,
        status: String,
        result_summary: Option<String>,
        last_error: Option<String>,
        next_attempt_at: Option<String>,
    ) -> Result<Option<IntakeTask>> {
        let store = self.store.clone();
        let task = tokio::task::spawn_blocking(move || {
            store.settle_intake_task(
                &id,
                &status,
                result_summary.as_deref(),
                last_error.as_deref(),
                next_attempt_at.as_deref(),
            )
        })
        .await??;
        self.record_project_manifest_best_effort().await;
        Ok(task)
    }

    pub async fn retry_intake_task(&self, id: String) -> Result<Option<IntakeTask>> {
        let store = self.store.clone();
        let task = tokio::task::spawn_blocking(move || store.retry_intake_task(&id)).await??;
        self.record_project_manifest_best_effort().await;
        Ok(task)
    }

    pub async fn cancel_intake_task(&self, id: String) -> Result<Option<IntakeTask>> {
        let store = self.store.clone();
        let task = tokio::task::spawn_blocking(move || store.cancel_intake_task(&id)).await??;
        self.record_project_manifest_best_effort().await;
        Ok(task)
    }

    pub async fn delete_intake_task(&self, id: String) -> Result<bool> {
        let store = self.store.clone();
        let deleted = tokio::task::spawn_blocking(move || store.delete_intake_task(&id)).await??;
        self.record_project_manifest_best_effort().await;
        Ok(deleted)
    }

    pub async fn count_active_intake_tasks(&self, project_id: String) -> Result<u32> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.count_active_intake_tasks(&project_id)).await?
    }

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

    /// Derive citation evidence from persisted `agent.message` events. The
    /// event log remains the source of truth; no second citation projection can
    /// drift from it.
    pub async fn session_citations(
        &self,
        session_id: &str,
        message_id: Option<String>,
    ) -> Result<SessionCitationsResult> {
        let events = self.recent_events(session_id, 10_000).await?;
        let mut citations = Vec::new();
        for event in events {
            let Some(parsed) = parse_message_citation_event(&event) else {
                continue;
            };
            if message_id
                .as_deref()
                .is_some_and(|wanted| wanted != parsed.message_id)
            {
                continue;
            }
            for citation in parsed.citations {
                citations.push(MessageCitation {
                    message_id: parsed.message_id.clone(),
                    citation,
                });
            }
        }
        Ok(SessionCitationsResult { citations })
    }

    /// Deterministic citation coverage report. It exposes dangling `[cN]`
    /// markers and citation records the model never referenced so the UI can
    /// surface evidence problems instead of silently trusting prose.
    pub async fn session_citation_audit(
        &self,
        session_id: &str,
    ) -> Result<SessionCitationAuditResult> {
        let events = self.recent_events(session_id, 10_000).await?;
        let mut messages = Vec::new();
        let mut messages_with_citations = 0usize;
        let mut messages_without_citations = 0usize;
        let mut total_citations = 0usize;
        let mut unique_sources = std::collections::HashSet::new();
        let mut dangling_total = 0usize;
        let mut unused_total = 0usize;
        for event in events {
            let Some(parsed) = parse_message_citation_event(&event) else {
                continue;
            };
            if parsed.text.trim().is_empty() {
                continue;
            }
            let referenced = citation_keys_in_text(&parsed.text);
            let available = parsed
                .citations
                .iter()
                .map(|citation| citation.key.clone())
                .collect::<Vec<_>>();
            let dangling = referenced
                .iter()
                .filter(|key| !available.iter().any(|available| available == *key))
                .cloned()
                .collect::<Vec<_>>();
            let unused = available
                .iter()
                .filter(|key| !referenced.iter().any(|referenced| referenced == *key))
                .cloned()
                .collect::<Vec<_>>();
            if parsed.citations.is_empty() {
                messages_without_citations += 1;
            } else {
                messages_with_citations += 1;
            }
            total_citations += parsed.citations.len();
            for citation in &parsed.citations {
                unique_sources.insert(citation.source_id.clone());
            }
            dangling_total += dangling.len();
            unused_total += unused.len();
            messages.push(CitationAuditMessage {
                message_id: parsed.message_id,
                agent_name: parsed.agent_name,
                excerpt: truncate_chars(&parsed.text, 240),
                citation_count: parsed.citations.len(),
                referenced_keys: referenced,
                dangling_keys: dangling,
                unused_keys: unused,
                created_at: parsed.created_at,
            });
        }
        Ok(SessionCitationAuditResult {
            audit: CitationAudit {
                messages_with_citations,
                messages_without_citations,
                total_citations,
                unique_sources: unique_sources.len(),
                dangling_key_count: dangling_total,
                unused_citation_count: unused_total,
                messages,
            },
        })
    }

    /// Skill activations derived from the persistent `skill.activated` events.
    /// Events remain the source of truth; no second usage table can drift.
    pub async fn session_skill_activations(
        &self,
        session_id: &str,
    ) -> Result<SessionSkillActivationsResult> {
        let events = self.recent_events(session_id, 10_000).await?;
        let mut activations = Vec::new();
        for event in events {
            if event.kind != "skill.activated" {
                continue;
            }
            let payload = &event.payload;
            let Some(name) = payload.get("name").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let string = |key: &str| {
                payload
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            let list = |key: &str| {
                payload
                    .get(key)
                    .and_then(serde_json::Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(serde_json::Value::as_str)
                            .map(ToOwned::to_owned)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            };
            activations.push(SkillActivation {
                name: name.to_string(),
                slug: string("slug"),
                version: string("version"),
                role: string("role"),
                profile: string("profile"),
                tool_allow: list("tool_allow"),
                tool_deny: list("tool_deny"),
                agent_id: payload
                    .get("agent_id")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
                agent_name: payload
                    .get("agent_name")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
                task_id: payload
                    .get("task_id")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
                activated_at: event.created_at.clone(),
            });
        }
        Ok(SessionSkillActivationsResult { activations })
    }

    // ---- Project knowledge sources -------------------------------------

    pub async fn list_sources(
        &self,
        project_id: String,
        query: Option<String>,
        status: Option<String>,
        limit: Option<u32>,
    ) -> Result<SourceListResult> {
        let store = self.store.clone();
        let project_id_for_query = project_id.clone();
        let sources = tokio::task::spawn_blocking(move || {
            store.list_sources(
                &project_id_for_query,
                query.as_deref(),
                status.as_deref(),
                limit,
            )
        })
        .await??;
        let store = self.store.clone();
        let stats = tokio::task::spawn_blocking(move || store.source_stats(&project_id)).await??;
        Ok(SourceListResult { sources, stats })
    }

    pub async fn get_source(
        &self,
        source_id: String,
        offset: Option<u32>,
        limit: Option<u32>,
    ) -> Result<SourceGetResult> {
        let store = self.store.clone();
        let id = source_id.clone();
        let source = tokio::task::spawn_blocking(move || store.get_source(&id))
            .await??
            .ok_or_else(|| anyhow!("source not found: {source_id}"))?;
        let store = self.store.clone();
        let id = source_id.clone();
        let chunks = tokio::task::spawn_blocking(move || {
            store.source_chunks(&id, offset.unwrap_or(0), limit.unwrap_or(50))
        })
        .await??;
        let store = self.store.clone();
        let total =
            tokio::task::spawn_blocking(move || store.source_chunk_count(&source_id)).await??;
        Ok(SourceGetResult {
            source,
            chunks,
            total_chunks: total,
        })
    }

    pub async fn add_source_note(&self, params: SourceCreateNoteParams) -> Result<Source> {
        let project = self.project_for_sources(&params.project_id).await?;
        validate_source_title(&params.title)?;
        let labels = validate_source_labels(params.labels)?;
        self.ingest_source(
            &project,
            "note",
            normalize_source_collection(params.collection.as_deref())?,
            params.title,
            None,
            Some("text/markdown".to_string()),
            params.content,
            labels,
        )
        .await
    }

    pub async fn add_source_file(&self, params: SourceCreateFileParams) -> Result<Source> {
        let project = self.project_for_sources(&params.project_id).await?;
        validate_source_title(params.title.as_deref().unwrap_or("source"))?;
        let labels = validate_source_labels(params.labels)?;
        let root = PathBuf::from(&project.root);
        let relative = params.path.clone();
        let file =
            tokio::task::spawn_blocking(move || read_source_file(&root, &relative)).await??;
        let title = params.title.unwrap_or_else(|| file.relative.clone());
        validate_source_title(&title)?;
        self.ingest_source(
            &project,
            "file",
            normalize_source_collection(params.collection.as_deref())?,
            title,
            Some(file.relative),
            Some(file.mime_type),
            file.content,
            labels,
        )
        .await
    }

    pub async fn add_source_url(&self, params: SourceCreateUrlParams) -> Result<Source> {
        let project = self.project_for_sources(&params.project_id).await?;
        let url = params.url.trim();
        if url.is_empty() || url.len() > 2_000 {
            anyhow::bail!("source URL must be 1-2000 characters");
        }
        let fetched = crate::tools::fetch_public_url(
            url,
            1_000_000,
            crate::sources::MAX_SOURCE_BYTES as usize,
        )
        .await?;
        if fetched.truncated {
            tracing::warn!(url, "source URL response was truncated by the fetch limit");
        }
        let is_html = fetched.content_type.to_ascii_lowercase().contains("html")
            || fetched.text.trim_start().starts_with("<!doctype")
            || fetched.text.trim_start().starts_with("<html");
        let content = if is_html {
            crate::sources::html_to_text(&fetched.text)
        } else {
            fetched.text
        };
        if content.trim().is_empty() {
            anyhow::bail!("the URL returned no readable text");
        }
        let title = params
            .title
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| source_title_from_url(url));
        validate_source_title(&title)?;
        let labels = validate_source_labels(params.labels)?;
        self.ingest_source(
            &project,
            "url",
            normalize_source_collection(params.collection.as_deref())?,
            title,
            Some(url.to_string()),
            Some(fetched.content_type),
            content,
            labels,
        )
        .await
    }

    /// Index text-like files under the project root. Existing source URIs are
    /// skipped; binary, oversized, malformed, and ignored files are counted as
    /// skipped rather than silently becoming partial sources.
    pub async fn import_sources(&self, params: SourceImportParams) -> Result<SourceImportResult> {
        let project = self.project_for_sources(&params.project_id).await?;
        let labels = validate_source_labels(params.labels)?;
        let root = PathBuf::from(&project.root);
        let existing = {
            let store = self.store.clone();
            let project_id = project.id.clone();
            tokio::task::spawn_blocking(move || {
                store.list_sources(&project_id, None, None, Some(1_000))
            })
            .await??
        };
        let existing_uris = existing
            .iter()
            .filter_map(|source| source.uri.clone())
            .collect::<std::collections::HashSet<_>>();
        let relative = params.path.clone();
        let max_files = params.max_files.unwrap_or(100).clamp(1, 500) as usize;
        let scan = tokio::task::spawn_blocking(move || {
            collect_import_files(&root, relative.as_deref(), max_files)
        })
        .await??;
        let mut imported = Vec::new();
        let mut skipped = scan.skipped;
        let mut errors = Vec::new();
        for file in scan.files {
            if existing_uris.contains(&file.relative) {
                skipped += 1;
                continue;
            }
            match self
                .ingest_source(
                    &project,
                    "file",
                    normalize_source_collection(params.collection.as_deref())?,
                    file.title.clone(),
                    Some(file.relative.clone()),
                    Some(file.mime_type.clone()),
                    file.content,
                    labels.clone(),
                )
                .await
            {
                Ok(source) => imported.push(source),
                Err(error) => errors.push(SourceImportError {
                    path: file.relative,
                    error: format!("{error:#}"),
                }),
            }
        }
        Ok(SourceImportResult {
            imported,
            skipped,
            errors,
        })
    }

    pub async fn delete_source(&self, source_id: String) -> Result<SourceDeleteResult> {
        let store = self.store.clone();
        let id = source_id.clone();
        let source = tokio::task::spawn_blocking(move || store.get_source(&id)).await??;
        let Some(source) = source else {
            return Ok(SourceDeleteResult { deleted: false });
        };
        // Remove the durable snapshot first. If the row delete then fails the
        // source remains visible but cannot be rebuilt from a stale snapshot;
        // the reverse order could resurrect a deleted source in `db-rebuild`.
        let store = self.store.clone();
        let project_id = source.project_id.clone();
        let id = source_id.clone();
        let deleted = tokio::task::spawn_blocking(move || {
            store.delete_source_snapshot(&project_id, &id)?;
            store.delete_source(&id)
        })
        .await??;
        Ok(SourceDeleteResult { deleted })
    }

    pub async fn reindex_source(&self, source_id: String) -> Result<SourceReindexResult> {
        let store = self.store.clone();
        let id = source_id.clone();
        let source = tokio::task::spawn_blocking(move || store.get_source(&id))
            .await??
            .ok_or_else(|| anyhow!("source not found: {source_id}"))?;
        let store = self.store.clone();
        let project_id = source.project_id.clone();
        let id = source_id.clone();
        let snapshot =
            tokio::task::spawn_blocking(move || store.read_source_snapshot(&project_id, &id))
                .await??
                .ok_or_else(|| {
                    anyhow!("source snapshot is missing; delete and re-add the source")
                })?;
        self.replace_source_content(source, snapshot.content, None)
            .await
    }

    /// Re-read a file or public URL and replace the indexed content. Notes are
    /// reindexed from their durable snapshot.
    pub async fn refresh_source(&self, source_id: String) -> Result<SourceReindexResult> {
        let store = self.store.clone();
        let id = source_id.clone();
        let source = tokio::task::spawn_blocking(move || store.get_source(&id))
            .await??
            .ok_or_else(|| anyhow!("source not found: {source_id}"))?;
        let project = self.project_for_sources(&source.project_id).await?;
        let (content, mime_type) = match source.kind.as_str() {
            "file" => {
                let relative = source
                    .uri
                    .clone()
                    .ok_or_else(|| anyhow!("file source has no path"))?;
                let root = PathBuf::from(&project.root);
                let file = tokio::task::spawn_blocking(move || read_source_file(&root, &relative))
                    .await??;
                (file.content, Some(file.mime_type))
            }
            "url" => {
                let url = source
                    .uri
                    .clone()
                    .ok_or_else(|| anyhow!("URL source has no URL"))?;
                let fetched = crate::tools::fetch_public_url(
                    &url,
                    1_000_000,
                    crate::sources::MAX_SOURCE_BYTES as usize,
                )
                .await?;
                let is_html = fetched.content_type.to_ascii_lowercase().contains("html")
                    || fetched.text.trim_start().starts_with("<!doctype")
                    || fetched.text.trim_start().starts_with("<html");
                let content = if is_html {
                    crate::sources::html_to_text(&fetched.text)
                } else {
                    fetched.text
                };
                (content, Some(fetched.content_type))
            }
            _ => {
                let store = self.store.clone();
                let project_id = source.project_id.clone();
                let id = source.id.clone();
                let snapshot = tokio::task::spawn_blocking(move || {
                    store.read_source_snapshot(&project_id, &id)
                })
                .await??
                .ok_or_else(|| anyhow!("source snapshot is missing"))?;
                (snapshot.content, None)
            }
        };
        if content.trim().is_empty() {
            anyhow::bail!("the refreshed source returned no readable text");
        }
        self.replace_source_content(source, content, mime_type)
            .await
    }

    async fn replace_source_content(
        &self,
        source: Source,
        content: String,
        mime_type: Option<String>,
    ) -> Result<SourceReindexResult> {
        let content = crate::sources::normalize_source_text(&content);
        if content.chars().count() > crate::sources::MAX_SOURCE_CHARS
            || content.len() > crate::sources::MAX_SOURCE_BYTES as usize
        {
            anyhow::bail!(
                "source is too large (limit {} bytes)",
                crate::sources::MAX_SOURCE_BYTES
            );
        }
        let chunks = build_source_chunks(&source.id, &content);
        if chunks.is_empty() {
            anyhow::bail!("source produced no readable chunks");
        }
        let now = now_rfc3339();
        let mut updated = source;
        updated.revision = updated.revision.saturating_add(1);
        updated.chunk_count = chunks.len();
        updated.char_count = content.chars().count();
        updated.token_count = chunks
            .iter()
            .map(|chunk| u64::from(chunk.token_count))
            .sum();
        updated.status = "indexed".to_string();
        updated.error = None;
        updated.size_bytes = content.len() as u64;
        updated.sha256 = sha256_hex(content.as_bytes());
        updated.updated_at = now.clone();
        updated.indexed_at = Some(now);
        if let Some(mime_type) = mime_type {
            updated.mime_type = Some(mime_type);
        }
        let snapshot = SourceSnapshot {
            version: 1,
            source: updated.clone(),
            content,
        };
        let store = self.store.clone();
        let task = updated.clone();
        let chunks_for_db = chunks.clone();
        tokio::task::spawn_blocking(move || {
            store.write_source_snapshot(&snapshot)?;
            store.insert_source(&task, &chunks_for_db)
        })
        .await??;
        Ok(SourceReindexResult {
            source: updated,
            chunks: chunks.len(),
        })
    }

    pub async fn search_sources(
        &self,
        project_id: String,
        query: String,
        source_ids: Vec<String>,
        limit: Option<u32>,
    ) -> Result<SourceSearchResult> {
        self.search_sources_with_mode(
            project_id,
            query,
            source_ids,
            Vec::new(),
            Vec::new(),
            limit,
            None,
        )
        .await
    }

    /// Search with explicit collection/label filters. An empty collection list
    /// means all collections; the empty string selects uncollected sources.
    pub async fn search_sources_scoped(
        &self,
        project_id: String,
        query: String,
        source_ids: Vec<String>,
        collections: Vec<String>,
        labels: Vec<String>,
        limit: Option<u32>,
    ) -> Result<SourceSearchResult> {
        self.search_sources_with_mode(
            project_id,
            query,
            source_ids,
            collections,
            labels,
            limit,
            None,
        )
        .await
    }

    /// Source retrieval with an explicit lane. `None` selects the
    /// backward-compatible dynamic default: hybrid when the project opted into
    /// source embeddings, otherwise lexical.
    ///
    /// Retrieval v2 layers on top of the unchanged lexical and exact-cosine
    /// lanes: optional deterministic IVF candidate selection, deterministic
    /// rerank, and optional lexical query expansion. The exact scan remains the
    /// fallback for every ANN condition, and every fallback reason is reported.
    #[allow(clippy::too_many_arguments)]
    pub async fn search_sources_with_mode(
        &self,
        project_id: String,
        query: String,
        source_ids: Vec<String>,
        collections: Vec<String>,
        labels: Vec<String>,
        limit: Option<u32>,
        mode: Option<SourceSearchMode>,
    ) -> Result<SourceSearchResult> {
        let store = self.store.clone();
        let policy_project = project_id.clone();
        let (embedding_policy, retrieval_policy) = tokio::task::spawn_blocking(move || {
            let embedding = store.get_source_embedding_policy(&policy_project)?;
            let retrieval = store.get_source_retrieval_policy(&policy_project)?;
            Ok::<_, anyhow::Error>((embedding, retrieval))
        })
        .await??;
        let (requested, wants_semantic) =
            crate::source_retrieval::source_effective_mode(mode, Some(&embedding_policy));
        let requested_was_default = mode.is_none();
        let requested_label = requested.as_str().to_string();
        let Some(parsed) = crate::sources::source_query(&query) else {
            return Ok(SourceSearchResult {
                hits: Vec::new(),
                degraded: false,
                mode: SourceSearchMode::Lexical.as_str().to_string(),
                requested_mode: requested_label,
                semantic_unavailable: wants_semantic.then(|| "empty_query".to_string()),
                candidates_scanned: 0,
                index_path: "exact".to_string(),
                rerank: retrieval_policy.rerank.as_str().to_string(),
                expansion: retrieval_policy.query_expansion.as_str().to_string(),
                ..SourceSearchResult::default()
            });
        };
        let search = SourceSearchQuery {
            all: parsed.all,
            any: parsed.any,
            terms: parsed.terms,
            raw: parsed.raw.clone(),
            prefer_like: parsed.prefer_like,
            collections: normalize_source_filters(collections, 24, 80, "collection")?,
            labels: normalize_source_filters(labels, 24, 40, "label")?,
        };
        let final_limit = limit.unwrap_or(8).clamp(1, 50);
        let request = crate::source_retrieval::SourceRetrievalRequest {
            project_id: project_id.clone(),
            query: search,
            source_ids,
            limit: final_limit,
            policy: retrieval_policy.clone(),
        };

        if !wants_semantic {
            let semantic_unavailable = if requested_was_default && !embedding_policy.is_active() {
                crate::source_embedding::source_semantic_unavailable(Some(&embedding_policy), 0, 0)
            } else {
                None
            };
            let mut result = self
                .lexical_source_result(request, semantic_unavailable)
                .await?;
            result.requested_mode = requested_label;
            return Ok(result);
        }

        // The dynamic default asks for hybrid only when the lane is active; an
        // explicit semantic/hybrid request still reports why it fell back.
        if !embedding_policy.is_active() {
            let reason =
                crate::source_embedding::source_semantic_unavailable(Some(&embedding_policy), 0, 0)
                    .unwrap_or_else(|| "disabled".to_string());
            return self
                .lexical_source_fallback(request, reason, requested_label)
                .await;
        }
        let client =
            match crate::source_embedding::SourceEmbeddingClient::from_policy(&embedding_policy) {
                Ok(client) => client,
                Err(error) => {
                    return self
                        .lexical_source_fallback(request, error.reason, requested_label)
                        .await;
                }
            };
        let store = self.store.clone();
        let project = project_id.clone();
        let model = client.model().to_string();
        let dimensions = client.dimensions();
        let max_chunks = embedding_policy.max_chunks_per_source;
        let ready = tokio::task::spawn_blocking(move || {
            store.source_embedding_ready_count(&project, &model, dimensions, max_chunks)
        })
        .await??;
        if ready == 0 {
            let store = self.store.clone();
            let project = project_id.clone();
            let stale =
                tokio::task::spawn_blocking(move || store.source_embedding_stale_count(&project))
                    .await??;
            let reason = crate::source_embedding::source_semantic_unavailable(
                Some(&embedding_policy),
                ready,
                stale,
            )
            .unwrap_or_else(|| "no_indexed_vectors".to_string());
            return self
                .lexical_source_fallback(request, reason, requested_label)
                .await;
        }

        // Query expansion is planned from a bounded first lexical pass before
        // the query is embedded. The expanded query is embedded in the same
        // bounded batch as the original; no second network round trip is made.
        let expansion_terms = if retrieval_policy.query_expansion
            == vp_protocol::types::SourceQueryExpansionMode::Lexical
        {
            let store = self.store.clone();
            let plan_request = request.clone();
            match tokio::task::spawn_blocking(move || {
                crate::source_retrieval::plan_expansion(&store, &plan_request, requested)
            })
            .await?
            {
                Ok(terms) => terms,
                Err(error) => {
                    tracing::warn!(%error, "source query expansion planning failed");
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };
        let mut texts = vec![crate::source_embedding::source_embedding_query_text(
            &parsed.raw,
        )];
        if !expansion_terms.is_empty() {
            let expanded_text = crate::source_embedding::source_embedding_query_text(&format!(
                "{} {}",
                parsed.raw,
                expansion_terms.join(" ")
            ));
            if expanded_text != texts[0] {
                texts.push(expanded_text);
            }
        }
        let batch = match client.embed(&texts).await {
            Ok(batch) => batch,
            Err(error) => {
                let code = crate::source_embedding::source_embedding_error_code(&error);
                return self
                    .lexical_source_fallback(
                        request,
                        format!("request_failed:{code}"),
                        requested_label,
                    )
                    .await;
            }
        };
        let mut items = batch.items.into_iter();
        let original = items.next();
        let Some(original) = original else {
            return self
                .lexical_source_fallback(
                    request,
                    "request_failed:invalid_response".to_string(),
                    requested_label,
                )
                .await;
        };
        if let Some(error) = original.error {
            let code = crate::source_embedding::source_embedding_item_error_code(&error);
            return self
                .lexical_source_fallback(request, format!("request_failed:{code}"), requested_label)
                .await;
        }
        let Some(query_vector) = original
            .vector
            .filter(|vector| vector.len() == dimensions as usize)
        else {
            return self
                .lexical_source_fallback(
                    request,
                    "request_failed:invalid_dimensions".to_string(),
                    requested_label,
                )
                .await;
        };
        let expanded_vector = if texts.len() > 1 {
            items.next().and_then(|item| {
                if item.error.is_some() {
                    return None;
                }
                item.vector
                    .filter(|vector| vector.len() == dimensions as usize)
            })
        } else {
            None
        };
        let context = crate::source_retrieval::SourceSemanticContext {
            model: client.model().to_string(),
            dimensions,
            max_chunks,
            query_vector,
            expanded_vector,
            expansion_terms,
        };
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            crate::source_retrieval::search_semantic(&store, &request, &context, requested)
        })
        .await?
    }

    async fn lexical_source_result(
        &self,
        request: crate::source_retrieval::SourceRetrievalRequest,
        semantic_unavailable: Option<String>,
    ) -> Result<SourceSearchResult> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            crate::source_retrieval::search_lexical(&store, &request, semantic_unavailable)
        })
        .await?
    }

    /// Every semantic preflight failure funnels through one deterministic
    /// lexical path so the fallback shape and reason cannot drift from the
    /// successful lexical path.
    async fn lexical_source_fallback(
        &self,
        request: crate::source_retrieval::SourceRetrievalRequest,
        reason: String,
        requested_label: String,
    ) -> Result<SourceSearchResult> {
        let mut result = self
            .lexical_source_result(request, Some(reason.clone()))
            .await?;
        result.requested_mode = requested_label;
        result.index_fallback_reason = Some(reason);
        Ok(result)
    }

    pub async fn source_stats(&self, project_id: String) -> Result<SourceStats> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.source_stats(&project_id)).await?
    }

    // ---- Semantic source index -----------------------------------------

    /// Read the opt-in source embedding policy. No request is made.
    pub async fn get_source_embedding_policy(
        &self,
        project_id: &str,
    ) -> Result<SourceEmbeddingPolicyResult> {
        self.ensure_project_exists(project_id).await?;
        let store = self.store.clone();
        let project = project_id.to_string();
        let policy =
            tokio::task::spawn_blocking(move || store.get_source_embedding_policy(&project))
                .await??;
        Ok(SourceEmbeddingPolicyResult {
            project_id: project_id.to_string(),
            policy,
        })
    }

    /// Persist a validated policy. Enabling is explicit: this never makes a
    /// network call and never queues a backlog. Operators call
    /// `source_embedding_reindex` after enabling to index existing sources.
    pub async fn set_source_embedding_policy(
        &self,
        project_id: &str,
        mut policy: SourceEmbeddingPolicy,
    ) -> Result<SourceEmbeddingPolicyResult> {
        self.ensure_project_exists(project_id).await?;
        crate::source_embedding::normalize_source_embedding_policy(&mut policy);
        crate::source_embedding::validate_source_embedding_policy(&policy)?;
        let store = self.store.clone();
        let project = project_id.to_string();
        let stored = policy.clone();
        tokio::task::spawn_blocking(move || store.set_source_embedding_policy(&project, &stored))
            .await??;
        Ok(SourceEmbeddingPolicyResult {
            project_id: project_id.to_string(),
            policy,
        })
    }

    /// Bounded operational status for the source semantic lane. It contains
    /// counts, stable fallback reasons, and environment *names*, never vectors,
    /// embedded text, endpoint values, or credentials.
    pub async fn source_embedding_status(
        &self,
        project_id: &str,
    ) -> Result<SourceEmbeddingStatusResult> {
        self.ensure_project_exists(project_id).await?;
        let store = self.store.clone();
        let project = project_id.to_string();
        let policy =
            tokio::task::spawn_blocking(move || store.get_source_embedding_policy(&project))
                .await??;
        let store = self.store.clone();
        let project = project_id.to_string();
        let policy_for_store = policy.clone();
        let mut status = tokio::task::spawn_blocking(move || {
            store.source_embedding_status(&project, Some(&policy_for_store))
        })
        .await??;
        status.endpoint_env_present = policy.endpoint_env_name().is_some_and(|name| {
            std::env::var(name)
                .ok()
                .is_some_and(|value| !value.trim().is_empty())
        });
        status.api_key_env_present = policy
            .api_key_env_name()
            .map(|name| {
                std::env::var(name)
                    .ok()
                    .is_some_and(|value| !value.trim().is_empty())
            })
            .unwrap_or(true);
        status.semantic_unavailable = crate::source_embedding::source_semantic_unavailable(
            Some(&policy),
            status.indexed_chunks,
            status.stale_chunks,
        );
        Ok(status)
    }

    /// Read the additive retrieval v2 policy. No request or index work occurs.
    pub async fn get_source_retrieval_policy(
        &self,
        project_id: &str,
    ) -> Result<SourceRetrievalPolicyResult> {
        self.ensure_project_exists(project_id).await?;
        let store = self.store.clone();
        let project = project_id.to_string();
        let policy =
            tokio::task::spawn_blocking(move || store.get_source_retrieval_policy(&project))
                .await??;
        Ok(SourceRetrievalPolicyResult {
            project_id: project_id.to_string(),
            policy,
        })
    }

    /// Persist a normalized, validated retrieval policy. Disabling ANN keeps
    /// the derived index on disk but never uses it; a later enable can reuse it
    /// once maintenance validates the generation. Rerank and expansion changes
    /// take effect on the next search and never rebuild vectors.
    pub async fn set_source_retrieval_policy(
        &self,
        project_id: &str,
        mut policy: SourceRetrievalPolicy,
    ) -> Result<SourceRetrievalPolicyResult> {
        self.ensure_project_exists(project_id).await?;
        crate::source_retrieval::normalize_source_retrieval_policy(&mut policy);
        crate::source_retrieval::validate_source_retrieval_policy(&policy)?;
        let store = self.store.clone();
        let project = project_id.to_string();
        let stored = policy.clone();
        tokio::task::spawn_blocking(move || store.set_source_retrieval_policy(&project, &stored))
            .await??;
        Ok(SourceRetrievalPolicyResult {
            project_id: project_id.to_string(),
            policy,
        })
    }

    /// Bounded operational status for the derived source IVF index. It contains
    /// counts, generations, configuration, and stable reasons only; centroid
    /// bytes and vectors are never serialized.
    pub async fn source_index_status(&self, project_id: &str) -> Result<SourceIndexStatusResult> {
        self.ensure_project_exists(project_id).await?;
        let store = self.store.clone();
        let project = project_id.to_string();
        let (embedding, retrieval) = tokio::task::spawn_blocking(move || {
            let embedding = store.get_source_embedding_policy(&project)?;
            let retrieval = store.get_source_retrieval_policy(&project)?;
            Ok::<_, anyhow::Error>((embedding, retrieval))
        })
        .await??;
        let model = embedding
            .model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let dimensions = embedding.dimensions;
        let embedding_enabled = embedding.enabled;
        let embedding_configured = embedding.is_configured();
        let max_chunks = embedding
            .max_chunks_per_source
            .clamp(1, crate::source_embedding::MAX_SOURCE_EMBEDDING_CHUNKS);
        let store = self.store.clone();
        let project = project_id.to_string();
        let retrieval_for_status = retrieval.clone();
        let model_for_status = model.clone();
        let status = tokio::task::spawn_blocking(move || {
            crate::source_ann::source_ann_status(
                &store,
                &project,
                &retrieval_for_status,
                embedding_enabled,
                embedding_configured,
                model_for_status.as_deref(),
                dimensions,
                max_chunks,
            )
        })
        .await??;
        let ann_enabled = retrieval.ann_enabled;
        Ok(SourceIndexStatusResult {
            project_id: project_id.to_string(),
            policy: retrieval,
            embedding_enabled,
            embedding_configured,
            model,
            dimensions,
            ann_enabled,
            ready: status.ready,
            path: status.path,
            generation: status.generation,
            vector_count: status.vector_count,
            assigned_vectors: status.assigned_vectors,
            invalid_assignments: status.invalid_assignments,
            centroid_count: status.centroid_count,
            nlist: status.nlist,
            nprobe: status.nprobe,
            min_vectors: status.min_vectors,
            coverage: status.coverage,
            bytes: status.bytes,
            fallback_reason: status.fallback_reason,
            last_rebuilt_at: status.last_rebuilt_at,
            last_error: status.last_error,
        })
    }

    /// Explicit operator rebuild. Generation-fenced and idempotent for
    /// unchanged vector data. It never makes an embedding request and never
    /// writes source content.
    pub async fn rebuild_source_index(&self, project_id: &str) -> Result<SourceIndexRebuildResult> {
        self.ensure_project_exists(project_id).await?;
        let store = self.store.clone();
        let project = project_id.to_string();
        let (embedding, retrieval) = tokio::task::spawn_blocking(move || {
            let embedding = store.get_source_embedding_policy(&project)?;
            let retrieval = store.get_source_retrieval_policy(&project)?;
            Ok::<_, anyhow::Error>((embedding, retrieval))
        })
        .await??;
        let model = embedding
            .model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or_default()
            .to_string();
        let dimensions = embedding.dimensions;
        let max_chunks = embedding
            .max_chunks_per_source
            .clamp(1, crate::source_embedding::MAX_SOURCE_EMBEDDING_CHUNKS);
        let store = self.store.clone();
        let project = project_id.to_string();
        let retrieval_for_rebuild = retrieval.clone();
        let result = tokio::task::spawn_blocking(move || {
            crate::source_ann::rebuild_source_ann(
                &store,
                &project,
                &retrieval_for_rebuild,
                &model,
                dimensions,
                max_chunks,
            )
        })
        .await?;
        match result {
            Ok(report) => Ok(SourceIndexRebuildResult {
                project_id: project_id.to_string(),
                rebuilt: report.rebuilt,
                reason: report.reason,
                generation: report.generation,
                vector_count: report.vector_count,
                assigned_vectors: report.assigned_vectors,
                centroid_count: report.centroid_count,
                nlist: report.nlist,
                nprobe: report.nprobe,
                path: report.path,
            }),
            Err(error) => {
                let store = self.store.clone();
                let project = project_id.to_string();
                let sanitized =
                    crate::source_embedding::sanitized_source_embedding_error(&error.to_string());
                let _ = tokio::task::spawn_blocking(move || {
                    store.record_source_ann_error(&project, &sanitized)
                })
                .await;
                Err(error)
            }
        }
    }

    /// Explicit operator reindex. Derived vectors/jobs are cleared first. When
    /// the lane is active, every indexed source is queued for a fresh
    /// generation; when it is disabled or incomplete this is a cleanup action.
    /// No embedding request is made here.
    pub async fn reindex_source_embeddings(
        &self,
        project_id: &str,
    ) -> Result<SourceEmbeddingReindexResult> {
        self.ensure_project_exists(project_id).await?;
        let store = self.store.clone();
        let project = project_id.to_string();
        let mut policy =
            tokio::task::spawn_blocking(move || store.get_source_embedding_policy(&project))
                .await??;
        crate::source_embedding::normalize_source_embedding_policy(&mut policy);
        if policy.is_configured() {
            crate::source_embedding::validate_source_embedding_policy(&policy)?;
        }
        let store = self.store.clone();
        let project = project_id.to_string();
        let (cleared_vectors, cleared_jobs) =
            tokio::task::spawn_blocking(move || store.clear_source_embeddings(&project)).await??;
        let enabled = policy.is_active();
        let model = policy.model.clone();
        let max_chunks = policy
            .max_chunks_per_source
            .clamp(1, crate::source_embedding::MAX_SOURCE_EMBEDDING_CHUNKS);
        let enqueued = if let (true, Some(model)) = (enabled, model.as_deref()) {
            let store = self.store.clone();
            let project = project_id.to_string();
            let model = model.to_string();
            tokio::task::spawn_blocking(move || {
                store.enqueue_source_embedding_backlog(&project, &model, max_chunks)
            })
            .await??
        } else {
            0
        };
        Ok(SourceEmbeddingReindexResult {
            project_id: project_id.to_string(),
            enabled,
            model,
            cleared_vectors,
            cleared_jobs,
            enqueued_sources: enqueued,
            max_chunks_per_source: max_chunks,
        })
    }

    /// Daemon reconciler over every explicitly enabled project. Returns
    /// `(projects, indexed_chunks, failed_chunks, completed_jobs)`.
    pub async fn reconcile_source_embeddings(&self) -> Result<(usize, usize, usize, usize)> {
        let store = self.store.clone();
        let policies =
            tokio::task::spawn_blocking(move || store.list_enabled_source_embedding_policies())
                .await??;
        let mut projects = 0usize;
        let mut indexed = 0usize;
        let mut failed = 0usize;
        let mut done = 0usize;
        for (project_id, policy) in policies {
            match self
                .reconcile_project_source_embeddings(&project_id, policy)
                .await
            {
                Ok((did_index, did_fail, did_done)) => {
                    projects += 1;
                    indexed += did_index;
                    failed += did_fail;
                    done += did_done;
                }
                Err(error) => {
                    tracing::warn!(%error, %project_id, "source embedding reconciliation failed");
                }
            }
        }
        Ok((projects, indexed, failed, done))
    }

    /// One bounded project tick. Configuration failures (missing env vars) are
    /// returned without burning a job's retry budget. Content/network failures
    /// advance the job with exponential backoff up to a dead letter.
    pub async fn reconcile_project_source_embeddings(
        &self,
        project_id: &str,
        policy: SourceEmbeddingPolicy,
    ) -> Result<(usize, usize, usize)> {
        if !policy.is_active() {
            return Ok((0, 0, 0));
        }
        let client = crate::source_embedding::SourceEmbeddingClient::from_policy(&policy)
            .map_err(|error| anyhow!("source embedding unavailable: {}", error.reason))?;
        let model = client.model().to_string();
        let dimensions = client.dimensions();
        let max_chunks = policy
            .max_chunks_per_source
            .clamp(1, crate::source_embedding::MAX_SOURCE_EMBEDDING_CHUNKS);
        let batch_size = policy
            .batch_size
            .clamp(1, crate::source_embedding::MAX_SOURCE_EMBEDDING_BATCH);
        let now = now_rfc3339();
        let stale_before = crate::source_embedding::SOURCE_EMBEDDING_JOB_STALE_SECS;
        let stale_before = source_embedding_time_before(stale_before);
        let store = self.store.clone();
        let project = project_id.to_string();
        let model_for_jobs = model.clone();
        let jobs = tokio::task::spawn_blocking(move || {
            store.due_source_embedding_jobs(
                &project,
                &model_for_jobs,
                crate::source_embedding::MAX_SOURCE_EMBEDDING_ATTEMPTS,
                &now,
                &stale_before,
                crate::source_embedding::MAX_SOURCE_EMBEDDING_JOBS_PER_TICK,
            )
        })
        .await??;
        let mut indexed = 0usize;
        let mut failed = 0usize;
        let mut done = 0usize;
        for job in jobs {
            let store = self.store.clone();
            let source_id = job.source_id.clone();
            let chunks = tokio::task::spawn_blocking(move || {
                store.source_embedding_chunks(&source_id, max_chunks)
            })
            .await??;
            if chunks.is_empty() {
                let store = self.store.clone();
                let source_id = job.source_id.clone();
                let generation = job.generation;
                let _ = tokio::task::spawn_blocking(move || {
                    store.record_source_embedding_job_done(&source_id, generation, 0)
                })
                .await?;
                done += 1;
                continue;
            }
            let mut current = 0u32;
            let mut missing: Vec<(SourceEmbeddingChunkRow, String, String)> = Vec::new();
            for row in &chunks {
                let text = crate::source_embedding::source_chunk_embedding_text(
                    &row.source_title,
                    &SourceChunk {
                        id: row.chunk_id.clone(),
                        source_id: row.source_id.clone(),
                        chunk_no: row.chunk_no,
                        heading: row.heading.clone(),
                        content: row.content.clone(),
                        start_offset: 0,
                        end_offset: 0,
                        token_count: row.token_count,
                    },
                );
                let hash = crate::source_embedding::source_chunk_embedding_hash(&text);
                let valid = row.generation == job.generation
                    && row.vector_model.as_deref() == Some(model.as_str())
                    && row.vector_dimensions == Some(dimensions)
                    && row.vector_generation == Some(job.generation)
                    && row.vector_status.as_deref() == Some("current")
                    && row.vector_content_hash.as_deref() == Some(hash.as_str());
                if valid {
                    current += 1;
                } else {
                    missing.push((row.clone(), text, hash));
                }
            }
            let chunks_total = u32::try_from(chunks.len()).unwrap_or(u32::MAX);
            if missing.is_empty() {
                let store = self.store.clone();
                let source_id = job.source_id.clone();
                let generation = job.generation;
                let _ = tokio::task::spawn_blocking(move || {
                    store.record_source_embedding_job_done(&source_id, generation, chunks_total)
                })
                .await?;
                done += 1;
                continue;
            }
            let batch_len = missing.len().min(batch_size as usize);
            let has_more = missing.len() > batch_len;
            let batch = missing.into_iter().take(batch_len).collect::<Vec<_>>();
            let texts = batch
                .iter()
                .map(|(_, text, _)| text.clone())
                .collect::<Vec<_>>();
            let store = self.store.clone();
            let source_id = job.source_id.clone();
            let generation = job.generation;
            let marked_running = tokio::task::spawn_blocking(move || {
                store.mark_source_embedding_job_running(&source_id, generation)
            })
            .await??;
            if !marked_running {
                continue;
            }
            let result = client.embed(&texts).await;
            let mut chunks_done = current;
            let mut item_failures: Vec<(bool, String)> = Vec::new();
            let mut obsolete = false;
            match result {
                Ok(batch_result) => {
                    let store = self.store.clone();
                    let project = project_id.to_string();
                    let usage = batch_result.usage.clone();
                    let model_for_usage = model.clone();
                    if usage.calls > 0 {
                        let _ = tokio::task::spawn_blocking(move || {
                            store.record_source_embedding_usage(
                                &project,
                                &model_for_usage,
                                usage.calls,
                                usage.input_tokens,
                                usage.estimated_tokens,
                                usage.bytes,
                                usage.provider_reported,
                            )
                        })
                        .await;
                    }
                    for (index, (row, _text, hash)) in batch.iter().enumerate() {
                        let item = batch_result.items.get(index);
                        let vector = item.and_then(|item| item.vector.as_ref());
                        let item_error = item.and_then(|item| item.error.clone());
                        match vector {
                            Some(vector) => {
                                match crate::source_embedding::encode_source_vector(vector) {
                                    Ok(encoded) => {
                                        let store = self.store.clone();
                                        let source_id = row.source_id.clone();
                                        let chunk_id = row.chunk_id.clone();
                                        let chunk_no = row.chunk_no;
                                        let generation = job.generation;
                                        let model = model.clone();
                                        let hash = hash.clone();
                                        match tokio::task::spawn_blocking(move || {
                                            store.record_source_embedding_success(
                                                &source_id, &chunk_id, chunk_no, generation,
                                                &model, dimensions, &hash, &encoded,
                                            )
                                        })
                                        .await
                                        {
                                            Ok(Ok(true)) => {
                                                chunks_done = chunks_done.saturating_add(1);
                                                indexed += 1;
                                            }
                                            Ok(Ok(false)) => {
                                                obsolete = true;
                                                break;
                                            }
                                            Ok(Err(error)) => {
                                                item_failures.push((
                                                    false,
                                                    format!("storage_failed: {error}"),
                                                ));
                                            }
                                            Err(error) => {
                                                item_failures.push((
                                                    false,
                                                    format!("storage_task_failed: {error}"),
                                                ));
                                            }
                                        }
                                    }
                                    Err(error) => item_failures.push((
                                        false,
                                        crate::source_embedding::source_embedding_item_error_code(
                                            &error.to_string(),
                                        ),
                                    )),
                                }
                            }
                            None => {
                                let error = item_error
                                    .unwrap_or_else(|| "endpoint returned no vector".to_string());
                                item_failures.push((
                                    crate::source_embedding::source_embedding_item_error_retryable(
                                        &error,
                                    ),
                                    crate::source_embedding::source_embedding_item_error_code(
                                        &error,
                                    ),
                                ));
                            }
                        }
                    }
                    if obsolete {
                        continue;
                    }
                    let store = self.store.clone();
                    let source_id = job.source_id.clone();
                    let generation = job.generation;
                    let _ = tokio::task::spawn_blocking(move || {
                        store.record_source_embedding_progress(&source_id, generation, chunks_done)
                    })
                    .await;
                    if item_failures.is_empty() {
                        if has_more {
                            let store = self.store.clone();
                            let source_id = job.source_id.clone();
                            let generation = job.generation;
                            let _ = tokio::task::spawn_blocking(move || {
                                store.mark_source_embedding_job_pending(
                                    &source_id,
                                    generation,
                                    chunks_done,
                                )
                            })
                            .await;
                        } else {
                            let store = self.store.clone();
                            let source_id = job.source_id.clone();
                            let generation = job.generation;
                            let _ = tokio::task::spawn_blocking(move || {
                                store.record_source_embedding_job_done(
                                    &source_id,
                                    generation,
                                    chunks_total,
                                )
                            })
                            .await;
                            done += 1;
                        }
                    } else {
                        failed += item_failures.len();
                        let attempts = job.attempts.saturating_add(1);
                        let terminal = item_failures.iter().any(|(retryable, _)| !retryable)
                            || attempts >= crate::source_embedding::MAX_SOURCE_EMBEDDING_ATTEMPTS;
                        let next_attempt_at = if terminal {
                            None
                        } else {
                            Some(source_embedding_retry_at(attempts))
                        };
                        let error = item_failures
                            .iter()
                            .take(3)
                            .map(|(_, code)| code.as_str())
                            .collect::<Vec<_>>()
                            .join("; ");
                        let store = self.store.clone();
                        let source_id = job.source_id.clone();
                        let generation = job.generation;
                        let _ = tokio::task::spawn_blocking(move || {
                            store.record_source_embedding_failure(
                                &source_id,
                                generation,
                                attempts,
                                &error,
                                next_attempt_at.as_deref(),
                                terminal,
                            )
                        })
                        .await;
                    }
                }
                Err(error) => {
                    let attempts = job.attempts.saturating_add(1);
                    let terminal = !error.retryable_http()
                        || attempts >= crate::source_embedding::MAX_SOURCE_EMBEDDING_ATTEMPTS;
                    let next_attempt_at = if terminal {
                        None
                    } else {
                        Some(source_embedding_retry_at(attempts))
                    };
                    let code = crate::source_embedding::source_embedding_error_code(&error);
                    let store = self.store.clone();
                    let source_id = job.source_id.clone();
                    let generation = job.generation;
                    let _ = tokio::task::spawn_blocking(move || {
                        store.record_source_embedding_failure(
                            &source_id,
                            generation,
                            attempts,
                            &code,
                            next_attempt_at.as_deref(),
                            terminal,
                        )
                    })
                    .await;
                    failed += batch_len;
                }
            }
        }
        // Retention is cheap metadata cleanup and intentionally best-effort.
        let store = self.store.clone();
        let project = project_id.to_string();
        let retention = policy.retention_days;
        if let Ok(Ok((vectors, jobs))) = tokio::task::spawn_blocking(move || {
            store.sweep_source_embedding_retention(&project, retention)
        })
        .await
            && (vectors > 0 || jobs > 0)
        {
            tracing::debug!(
                vectors,
                jobs,
                %project_id,
                "pruned stale source embedding metadata"
            );
        }
        // Derived ANN maintenance is best-effort and never changes the
        // embedding result. It runs only when the project explicitly opted in,
        // so a default project does no extra SQL or CPU work.
        let store = self.store.clone();
        let project = project_id.to_string();
        if let Ok(Ok(retrieval)) =
            tokio::task::spawn_blocking(move || store.get_source_retrieval_policy(&project)).await
            && retrieval.ann_enabled
        {
            let store = self.store.clone();
            let project = project_id.to_string();
            let model = model.clone();
            match tokio::task::spawn_blocking(move || {
                crate::source_ann::maintain_source_ann(
                    &store, &project, &retrieval, &model, dimensions, max_chunks,
                )
            })
            .await
            {
                Ok(Ok(maintenance)) => {
                    if maintenance.rebuilt_generation.is_some()
                        || maintenance.assigned_vectors > 0
                        || maintenance.repaired_assignments > 0
                    {
                        tracing::debug!(
                            rebuilt = ?maintenance.rebuilt_generation,
                            assigned = maintenance.assigned_vectors,
                            repaired = maintenance.repaired_assignments,
                            %project_id,
                            "maintained derived source ANN index"
                        );
                    }
                }
                Ok(Err(error)) => {
                    tracing::warn!(%error, %project_id, "source ANN maintenance failed");
                }
                Err(error) => {
                    tracing::warn!(%error, %project_id, "source ANN maintenance task failed");
                }
            }
        }
        Ok((indexed, failed, done))
    }

    pub async fn source_collections(&self, project_id: String) -> Result<SourceCollectionsResult> {
        let store = self.store.clone();
        let project_for_count = project_id.clone();
        let collections =
            tokio::task::spawn_blocking(move || store.list_source_collections(&project_id))
                .await??;
        let store = self.store.clone();
        let stats =
            tokio::task::spawn_blocking(move || store.source_stats(&project_for_count)).await??;
        Ok(SourceCollectionsResult {
            total_sources: stats.sources,
            collections,
        })
    }

    /// Replace user-editable source metadata and keep the durable snapshot in
    /// sync in the same blocking section. Content/chunks are untouched.
    pub async fn update_source_metadata(
        &self,
        params: UpdateSourceMetadataParams,
    ) -> Result<Source> {
        let title = params.title.trim();
        validate_source_title(title)?;
        let collection = normalize_source_collection(params.collection.as_deref())?;
        let labels = validate_source_labels(params.labels)?;
        let store = self.store.clone();
        let id = params.source_id.clone();
        let current = tokio::task::spawn_blocking(move || store.get_source(&id))
            .await??
            .ok_or_else(|| anyhow!("source not found: {}", params.source_id))?;
        let mut updated = current;
        updated.title = title.to_string();
        updated.collection = collection;
        updated.labels = labels;
        updated.updated_at = now_rfc3339();
        let store = self.store.clone();
        let task = updated.clone();
        let stored = tokio::task::spawn_blocking(move || {
            let updated = store.update_source_metadata(&task)?;
            if let Some(mut snapshot) =
                store.read_source_snapshot(&updated.project_id, &updated.id)?
            {
                snapshot.source = updated.clone();
                store.write_source_snapshot(&snapshot)?;
            }
            Ok::<_, anyhow::Error>(updated)
        })
        .await??;
        Ok(stored)
    }

    pub async fn set_source_collection(
        &self,
        source_id: String,
        collection: Option<String>,
    ) -> Result<Source> {
        let collection = normalize_source_collection(collection.as_deref())?;
        let store = self.store.clone();
        let id = source_id.clone();
        let current = tokio::task::spawn_blocking(move || store.get_source(&id))
            .await??
            .ok_or_else(|| anyhow!("source not found: {source_id}"))?;
        let mut updated = current;
        updated.collection = collection;
        updated.updated_at = now_rfc3339();
        let store = self.store.clone();
        let task = updated.clone();
        let stored = tokio::task::spawn_blocking(move || {
            let updated = store.update_source_metadata(&task)?;
            if let Some(mut snapshot) =
                store.read_source_snapshot(&updated.project_id, &updated.id)?
            {
                snapshot.source = updated.clone();
                store.write_source_snapshot(&snapshot)?;
            }
            Ok::<_, anyhow::Error>(updated)
        })
        .await??;
        Ok(stored)
    }

    pub async fn set_source_labels(
        &self,
        source_id: String,
        labels: Vec<String>,
    ) -> Result<Source> {
        let labels = validate_source_labels(labels)?;
        let store = self.store.clone();
        let id = source_id.clone();
        let current = tokio::task::spawn_blocking(move || store.get_source(&id))
            .await??
            .ok_or_else(|| anyhow!("source not found: {source_id}"))?;
        let mut updated = current;
        updated.labels = labels;
        updated.updated_at = now_rfc3339();
        let store = self.store.clone();
        let task = updated.clone();
        let stored = tokio::task::spawn_blocking(move || {
            let updated = store.update_source_metadata(&task)?;
            if let Some(mut snapshot) =
                store.read_source_snapshot(&updated.project_id, &updated.id)?
            {
                snapshot.source = updated.clone();
                store.write_source_snapshot(&snapshot)?;
            }
            Ok::<_, anyhow::Error>(updated)
        })
        .await??;
        Ok(stored)
    }

    /// Short source catalog for agent system prompts. Only indexed titles are
    /// exposed; the agent still has to call `search_sources` for content.
    pub async fn source_brief(&self, project_id: &str) -> Result<String> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        let sources = tokio::task::spawn_blocking(move || {
            store.list_sources(&project_id, None, Some("indexed"), Some(8))
        })
        .await??;
        if sources.is_empty() {
            return Ok(String::new());
        }
        Ok(sources
            .iter()
            .map(|source| {
                let collection = source
                    .collection
                    .as_deref()
                    .map(|value| format!("[{value}] "))
                    .unwrap_or_default();
                format!("- {collection}{} ({})", source.title, source.kind)
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }

    /// Rebuild the SQLite source projection from durable JSON snapshots. Used
    /// by `db-rebuild`; malformed snapshots are skipped and counted by the
    /// caller's rebuild report.
    pub async fn restore_sources_from_disk(&self) -> Result<usize> {
        let store = self.store.clone();
        let snapshots =
            tokio::task::spawn_blocking(move || store.list_source_snapshots()).await??;
        let mut restored = 0usize;
        for snapshot in snapshots {
            let project = {
                let store = self.store.clone();
                let id = snapshot.source.project_id.clone();
                tokio::task::spawn_blocking(move || store.get_project(&id)).await??
            };
            if project.is_none() {
                tracing::warn!(
                    source_id = %snapshot.source.id,
                    "skipping source snapshot for a missing project"
                );
                continue;
            }
            let content = crate::sources::normalize_source_text(&snapshot.content);
            let chunks = build_source_chunks(&snapshot.source.id, &content);
            if chunks.is_empty() {
                continue;
            }
            let now = now_rfc3339();
            let mut source = snapshot.source.clone();
            source.chunk_count = chunks.len();
            source.char_count = content.chars().count();
            source.token_count = chunks
                .iter()
                .map(|chunk| u64::from(chunk.token_count))
                .sum();
            source.status = "indexed".to_string();
            source.error = None;
            source.updated_at = now.clone();
            source.indexed_at = Some(now);
            let store = self.store.clone();
            let source_for_db = source.clone();
            let chunks_for_db = chunks.clone();
            tokio::task::spawn_blocking(move || {
                store.insert_source(&source_for_db, &chunks_for_db)
            })
            .await??;
            restored += 1;
        }
        Ok(restored)
    }

    async fn project_for_sources(&self, project_id: &str) -> Result<Project> {
        let store = self.store.clone();
        let id = project_id.to_string();
        tokio::task::spawn_blocking(move || store.get_project(&id))
            .await??
            .ok_or_else(|| anyhow!("project not found: {project_id}"))
    }

    #[allow(clippy::too_many_arguments)]
    async fn ingest_source(
        &self,
        project: &Project,
        kind: &str,
        collection: Option<String>,
        title: String,
        uri: Option<String>,
        mime_type: Option<String>,
        content: String,
        labels: Vec<String>,
    ) -> Result<Source> {
        validate_source_title(&title)?;
        let content = crate::sources::normalize_source_text(&content);
        if content.trim().is_empty() {
            anyhow::bail!("source content must not be empty");
        }
        if content.chars().count() > crate::sources::MAX_SOURCE_CHARS
            || content.len() > crate::sources::MAX_SOURCE_BYTES as usize
        {
            anyhow::bail!(
                "source is too large (limit {} bytes)",
                crate::sources::MAX_SOURCE_BYTES
            );
        }
        let id = Uuid::now_v7().to_string();
        let chunks = build_source_chunks(&id, &content);
        if chunks.is_empty() {
            anyhow::bail!("source produced no readable chunks");
        }
        let now = now_rfc3339();
        let source = Source {
            id: id.clone(),
            project_id: project.id.clone(),
            kind: kind.to_string(),
            collection,
            title,
            uri,
            mime_type,
            status: "indexed".to_string(),
            size_bytes: content.len() as u64,
            sha256: sha256_hex(content.as_bytes()),
            revision: 1,
            chunk_count: chunks.len(),
            char_count: content.chars().count(),
            token_count: chunks
                .iter()
                .map(|chunk| u64::from(chunk.token_count))
                .sum(),
            error: None,
            labels,
            created_at: now.clone(),
            updated_at: now.clone(),
            indexed_at: Some(now),
        };
        let snapshot = SourceSnapshot {
            version: 1,
            source: source.clone(),
            content,
        };
        let store = self.store.clone();
        let source_for_db = source.clone();
        let chunks_for_db = chunks.clone();
        tokio::task::spawn_blocking(move || {
            store.write_source_snapshot(&snapshot)?;
            store.insert_source(&source_for_db, &chunks_for_db)
        })
        .await??;
        Ok(source)
    }

    // ---- Project skill packs -------------------------------------------

    pub async fn list_skills(
        &self,
        project_id: String,
        include_disabled: bool,
    ) -> Result<SkillListResult> {
        self.project_for_sources(&project_id).await?;
        let store = self.store.clone();
        let skills = tokio::task::spawn_blocking(move || {
            store.list_skills(Some(&project_id), include_disabled)
        })
        .await??;
        Ok(SkillListResult { skills })
    }

    pub async fn create_skill(&self, params: SkillCreateParams) -> Result<Skill> {
        let project = self.project_for_sources(&params.project_id).await?;
        let skill = build_skill(
            &project.id,
            &params.name,
            &params.description,
            &params.when_to_use,
            params.version.as_deref(),
            &params.instructions,
            params.tool_allow,
            params.tool_deny,
            params.profiles,
            params.roles,
            "user",
            true,
        )?;
        self.ensure_skill_slug_free(&project.id, &skill.slug, None)
            .await?;
        let store = self.store.clone();
        let stored = tokio::task::spawn_blocking(move || store.insert_skill(&skill)).await??;
        self.record_project_manifest_best_effort().await;
        Ok(stored)
    }

    pub async fn import_skill(&self, params: SkillImportParams) -> Result<Skill> {
        let project = self.project_for_sources(&params.project_id).await?;
        let root = PathBuf::from(&project.root);
        let path = params.path.clone();
        let (content, fallback_name) = tokio::task::spawn_blocking(move || {
            read_project_text_file(&root, &path, crate::skills::MAX_SKILL_FILE_BYTES).map(
                |(content, relative)| {
                    (
                        content,
                        crate::skills::skill_file_name(Path::new(&relative)),
                    )
                },
            )
        })
        .await??;
        let parsed = crate::skills::parse_skill_markdown(&content, &fallback_name)?;
        let skill = build_skill(
            &project.id,
            &parsed.name,
            &parsed.description,
            &parsed.when_to_use,
            Some(&parsed.version),
            &parsed.instructions,
            parsed.tool_allow,
            parsed.tool_deny,
            parsed.profiles,
            parsed.roles,
            "import",
            true,
        )?;
        self.ensure_skill_slug_free(&project.id, &skill.slug, None)
            .await?;
        let store = self.store.clone();
        let stored = tokio::task::spawn_blocking(move || store.insert_skill(&skill)).await??;
        self.record_project_manifest_best_effort().await;
        Ok(stored)
    }

    pub async fn update_skill(&self, params: SkillUpdateParams) -> Result<Skill> {
        let store = self.store.clone();
        let id = params.id.clone();
        let current = tokio::task::spawn_blocking(move || store.get_skill(&id))
            .await??
            .ok_or_else(|| anyhow!("skill not found: {}", params.id))?;
        let skill = build_skill(
            &current.project_id,
            &params.name,
            &params.description,
            &params.when_to_use,
            params.version.as_deref(),
            &params.instructions,
            params.tool_allow,
            params.tool_deny,
            params.profiles,
            params.roles,
            &current.source,
            current.enabled,
        )?;
        let mut updated = skill;
        updated.id = current.id;
        updated.created_at = current.created_at;
        self.ensure_skill_slug_free(&updated.project_id, &updated.slug, Some(&updated.id))
            .await?;
        let store = self.store.clone();
        let stored = tokio::task::spawn_blocking(move || store.insert_skill(&updated)).await??;
        self.record_project_manifest_best_effort().await;
        Ok(stored)
    }

    pub async fn set_skill_enabled(&self, params: SkillEnabledParams) -> Result<Skill> {
        let store = self.store.clone();
        let id = params.id.clone();
        let skill =
            tokio::task::spawn_blocking(move || store.set_skill_enabled(&id, params.enabled))
                .await??
                .ok_or_else(|| anyhow!("skill not found: {}", params.id))?;
        self.record_project_manifest_best_effort().await;
        Ok(skill)
    }

    pub async fn delete_skill(&self, id: String) -> Result<SkillDeleteResult> {
        let store = self.store.clone();
        let deleted = tokio::task::spawn_blocking(move || store.delete_skill(&id)).await??;
        self.record_project_manifest_best_effort().await;
        Ok(SkillDeleteResult { deleted })
    }

    /// Enabled skills applicable to this role/profile.
    pub async fn skills_for(
        &self,
        project_id: &str,
        role: &str,
        profile: &str,
    ) -> Result<Vec<Skill>> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        let skills =
            tokio::task::spawn_blocking(move || store.list_skills(Some(&project_id), false))
                .await??;
        Ok(skills
            .into_iter()
            .filter(|skill| skill_applies(skill, role, profile))
            .collect())
    }

    /// Enabled-skill catalog for agent system prompts. Only a compact index is
    /// injected; the full instructions are loaded by the `use_skill` tool.
    pub async fn skill_catalog(
        &self,
        project_id: &str,
        role: &str,
        profile: &str,
    ) -> Result<String> {
        let skills = self.skills_for(project_id, role, profile).await?;
        let mut output = String::new();
        for skill in skills.iter().take(24) {
            let when = if skill.when_to_use.trim().is_empty() {
                String::new()
            } else {
                format!(" — {}", truncate_chars(&skill.when_to_use, 220))
            };
            let line = format!(
                "- {} ({}){}: {}\n",
                skill.name,
                skill.version,
                when,
                truncate_chars(&skill.description, 300)
            );
            if output.len() + line.len() > 4_000 {
                break;
            }
            output.push_str(&line);
        }
        Ok(output)
    }

    async fn ensure_skill_slug_free(
        &self,
        project_id: &str,
        slug: &str,
        excluding_id: Option<&str>,
    ) -> Result<()> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        let slug = slug.to_string();
        let existing =
            tokio::task::spawn_blocking(move || store.get_skill_by_slug(&project_id, &slug))
                .await??;
        if let Some(existing) = existing
            && excluding_id != Some(existing.id.as_str())
        {
            anyhow::bail!("a skill with slug '{}' already exists", existing.slug);
        }
        Ok(())
    }

    /// Build the one-task TeamPlan used for an intake item. It is public for
    /// tests and for future import/migration tooling; dispatch uses it too.
    pub fn intake_plan(&self, task: &IntakeTask) -> TeamPlan {
        let department = if task.profile == "research" {
            "research"
        } else if task.profile == "verification" {
            "verification"
        } else {
            "delivery"
        };
        let manager_title = if department == "research" {
            "Research Manager"
        } else if department == "verification" {
            "Verification Manager"
        } else {
            "Delivery Manager"
        };
        let objective = format!("{}: {}", task.title, first_line(&task.instructions, 240));
        TeamPlan {
            goal: task.title.clone(),
            departments: vec![vp_protocol::types::PlanDepartment {
                name: department.to_string(),
                manager_title: manager_title.to_string(),
                profile: task.profile.clone(),
                objective,
                token_budget: task.token_budget,
                cost_budget_usd: task.cost_budget_usd,
            }],
            tasks: vec![vp_protocol::types::PlanTask {
                id: "T1".to_string(),
                department: department.to_string(),
                title: task.title.clone(),
                kind: task.kind.clone(),
                priority: task.priority,
                profile: Some(task.profile.clone()),
                deps: Vec::new(),
                write_scope: task.write_scope.clone(),
                acceptance: task.acceptance.clone(),
                token_budget: task.token_budget,
                cost_budget_usd: task.cost_budget_usd,
            }],
        }
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
            false,
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
        self.create_session_full(project_id, goal, token_budget, None, None, priority, false)
            .await
    }

    /// Create a session with every admission and budget option. This is the
    /// full option path used by the RPC layer; the narrower helpers above are
    /// convenience wrappers for tests and older call sites.
    pub async fn create_session_with_options(
        &self,
        project_id: String,
        goal: String,
        token_budget: Option<u64>,
        cost_budget_usd: Option<f64>,
        time_budget_secs: Option<u64>,
        priority: i32,
    ) -> Result<Session> {
        self.create_session_full(
            project_id,
            goal,
            token_budget,
            cost_budget_usd,
            time_budget_secs,
            priority,
            false,
        )
        .await
    }

    /// Full session creation path that can also select unlimited budget mode.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_session_with_options_and_unlimited(
        &self,
        project_id: String,
        goal: String,
        token_budget: Option<u64>,
        cost_budget_usd: Option<f64>,
        time_budget_secs: Option<u64>,
        priority: i32,
        unlimited: bool,
    ) -> Result<Session> {
        self.create_session_full(
            project_id,
            goal,
            token_budget,
            cost_budget_usd,
            time_budget_secs,
            priority,
            unlimited,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn create_session_full(
        &self,
        project_id: String,
        goal: String,
        token_budget: Option<u64>,
        cost_budget_usd: Option<f64>,
        time_budget_secs: Option<u64>,
        priority: i32,
        unlimited: bool,
    ) -> Result<Session> {
        if !(-100..=100).contains(&priority) {
            anyhow::bail!("session priority must be between -100 and 100");
        }
        if unlimited
            && (token_budget.is_some() || cost_budget_usd.is_some() || time_budget_secs.is_some())
        {
            anyhow::bail!("an unlimited session cannot also declare token, cost, or time budgets");
        }
        if token_budget == Some(0) {
            anyhow::bail!("token budget must be greater than zero");
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
                    unlimited,
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
            "unlimited": session.unlimited,
            "budget_mode": if session.unlimited { "unlimited" } else { "limited" },
            "base_commit": session.base_commit,
            "prompt_layout": session.prompt_layout,
            "created_at": session.created_at,
        });
        let _ = self
            .append_event(&session.id, "session.created", payload)
            .await?;
        Ok(session)
    }

    /// Snapshot the active conversation and committed session branch. The
    /// worktree must be clean so the checkpoint commit is reproducible.
    /// Snapshot a preempted Worker/Labour task worktree under a daemon-owned
    /// ref. This preserves incomplete work for inspection and an explicit
    /// retry; it never merges, commits to the task branch, or enters the merge
    /// queue.
    pub(crate) async fn snapshot_preempted_task_worktree(
        &self,
        session_id: &str,
        task_id: &str,
        worktree_path: &Path,
        label: &str,
    ) -> Result<String> {
        let data_dir = self.store.data_dir().to_path_buf();
        let session_id = session_id.to_string();
        let task_id = task_id.to_string();
        let worktree_path = worktree_path.to_path_buf();
        let checkpoint_id = format!("preempt-{}", Uuid::now_v7());
        let label = format!(
            "{} [task {}]",
            label.chars().take(80).collect::<String>(),
            task_id.chars().take(80).collect::<String>()
        );
        let git_lock = self.git_lock.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = git_lock
                .lock()
                .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
            if !worktree_path.is_dir() {
                anyhow::bail!("task worktree is missing: {}", worktree_path.display());
            }
            worktree::snapshot_worktree(
                &worktree_path,
                &data_dir,
                &session_id,
                &checkpoint_id,
                &label,
            )
        })
        .await?
    }

    pub async fn checkpoint_session(
        &self,
        session_id: &str,
        label: Option<String>,
    ) -> Result<SessionCheckpoint> {
        self.checkpoint_session_inner(session_id, label, false)
            .await
    }

    /// Checkpoint at a hard-preemption boundary. The run loop has already
    /// stopped provider/tool work and is the only writer, so the normal
    /// running/paused quiescence gate is intentionally bypassed. This is not a
    /// general-purpose escape hatch and is not exposed over RPC.
    pub(crate) async fn checkpoint_session_at_preemption(
        &self,
        session_id: &str,
        label: Option<String>,
    ) -> Result<SessionCheckpoint> {
        self.checkpoint_session_inner(session_id, label, true).await
    }

    async fn checkpoint_session_inner(
        &self,
        session_id: &str,
        label: Option<String>,
        preemption_safe: bool,
    ) -> Result<SessionCheckpoint> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        if !preemption_safe {
            ensure_session_quiescent(&session, "create a checkpoint")?;
        }
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

    pub async fn get_context_checkpoint_policy(
        &self,
        project_id: &str,
    ) -> Result<vp_protocol::types::ContextCheckpointPolicy> {
        let store = self.store.clone();
        let id = project_id.to_string();
        tokio::task::spawn_blocking(move || {
            store
                .get_project(&id)?
                .ok_or_else(|| anyhow::anyhow!("project not found: {id}"))?;
            store.get_context_checkpoint_policy(&id)
        })
        .await?
    }

    pub async fn set_context_checkpoint_policy(
        &self,
        project_id: &str,
        policy: &vp_protocol::types::ContextCheckpointPolicy,
    ) -> Result<vp_protocol::types::ContextCheckpointPolicyResult> {
        crate::context::validate_context_checkpoint_policy(policy)?;
        let store = self.store.clone();
        let id = project_id.to_string();
        let policy = policy.clone();
        let saved = tokio::task::spawn_blocking(move || {
            store
                .get_project(&id)?
                .ok_or_else(|| anyhow::anyhow!("project not found: {id}"))?;
            store.set_context_checkpoint_policy(&id, &policy)?;
            store.get_context_checkpoint_policy(&id)
        })
        .await??;
        Ok(vp_protocol::types::ContextCheckpointPolicyResult {
            project_id: project_id.to_string(),
            policy: saved,
        })
    }

    /// Insert a context checkpoint row and its full-payload event atomically.
    /// Used by automatic compaction, the manual checkpoint command, and tests.
    pub(crate) async fn record_context_checkpoint(
        &self,
        checkpoint: vp_protocol::types::ContextCheckpoint,
    ) -> Result<Event> {
        let checkpoint = crate::context::bound_context_checkpoint(checkpoint)?;
        let store = self.store.clone();
        let session_id = checkpoint.session_id.clone();
        let event = tokio::task::spawn_blocking(move || {
            store.record_context_checkpoint_with_event(&session_id, &checkpoint)
        })
        .await??;
        let _ = self.events.send(event.clone());
        Ok(event)
    }

    /// Append a successful compaction and its automatic checkpoint atomically.
    pub(crate) async fn append_context_compaction_with_checkpoint(
        &self,
        checkpoint: vp_protocol::types::ContextCheckpoint,
        compaction_payload: serde_json::Value,
    ) -> Result<(Event, Event)> {
        let checkpoint = crate::context::bound_context_checkpoint(checkpoint)?;
        let store = self.store.clone();
        let session_id = checkpoint.session_id.clone();
        let (compaction, checkpoint_event) = tokio::task::spawn_blocking(move || {
            store.append_context_compaction_with_checkpoint(
                &session_id,
                compaction_payload,
                &checkpoint,
            )
        })
        .await??;
        let _ = self.events.send(compaction.clone());
        let _ = self.events.send(checkpoint_event.clone());
        Ok((compaction, checkpoint_event))
    }

    /// Append the reconstructed compaction boundary and the restore audit
    /// event. This does not touch messages, checkpoints, or the worktree.
    pub(crate) async fn append_context_restore_events(
        &self,
        session_id: &str,
        compaction_payload: serde_json::Value,
        restored_payload: serde_json::Value,
    ) -> Result<(Event, Event)> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let (compaction, restored) = tokio::task::spawn_blocking(move || {
            store.append_context_restore_events(&session_id, compaction_payload, restored_payload)
        })
        .await??;
        let _ = self.events.send(compaction.clone());
        let _ = self.events.send(restored.clone());
        Ok((compaction, restored))
    }

    pub async fn list_context_checkpoints(
        &self,
        session_id: &str,
        limit: Option<u32>,
    ) -> Result<vp_protocol::types::ContextCheckpointListResult> {
        let limit = limit
            .unwrap_or(50)
            .clamp(1, crate::context::MAX_CONTEXT_CHECKPOINT_LIST_LIMIT);
        let store = self.store.clone();
        let id = session_id.to_string();
        let (session, checkpoints) = tokio::task::spawn_blocking(move || {
            let session = store
                .get_session(&id)?
                .ok_or_else(|| anyhow::anyhow!("session not found: {id}"))?;
            let checkpoints = store.list_context_checkpoints(&id, limit)?;
            Ok::<_, anyhow::Error>((session, checkpoints))
        })
        .await??;
        let policy = self
            .get_context_checkpoint_policy(&session.project_id)
            .await?;
        let latest_id = checkpoints.last().map(|checkpoint| checkpoint.id.clone());
        Ok(vp_protocol::types::ContextCheckpointListResult {
            session_id: session.id,
            checkpoints,
            latest_id,
            policy,
            limit,
        })
    }

    pub async fn delete_context_checkpoint(
        &self,
        session_id: &str,
        checkpoint_id: &str,
    ) -> Result<vp_protocol::types::ContextCheckpointDeleteResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        if session.status == "imported" {
            anyhow::bail!("imported sessions are read-only archives");
        }
        let active = is_active_session_status(&session.status);
        let latest = {
            let store = self.store.clone();
            let id = session_id.to_string();
            tokio::task::spawn_blocking(move || store.latest_context_checkpoint(&id)).await??
        };
        if active && latest.as_ref().is_some_and(|row| row.id == checkpoint_id) {
            anyhow::bail!(
                "cannot delete the latest context checkpoint while the session is active"
            );
        }
        let store = self.store.clone();
        let id = session_id.to_string();
        let checkpoint_id_owned = checkpoint_id.to_string();
        let event = tokio::task::spawn_blocking(move || {
            store.delete_context_checkpoint_with_event(&id, &checkpoint_id_owned, "manual")
        })
        .await??;
        if let Some(event) = event {
            let _ = self.events.send(event);
            return Ok(vp_protocol::types::ContextCheckpointDeleteResult {
                session_id: session_id.to_string(),
                checkpoint_id: checkpoint_id.to_string(),
                removed: true,
            });
        }
        Ok(vp_protocol::types::ContextCheckpointDeleteResult {
            session_id: session_id.to_string(),
            checkpoint_id: checkpoint_id.to_string(),
            removed: false,
        })
    }

    /// Oldest-first retention. The latest checkpoint of an active run is
    /// always protected, even when `keep_latest` is zero.
    pub async fn prune_context_checkpoints(
        &self,
        session_id: &str,
        older_than_days: Option<u32>,
        keep_latest: Option<u32>,
        dry_run: bool,
    ) -> Result<vp_protocol::types::ContextCheckpointPruneResult> {
        if older_than_days.is_some_and(|days| days > 365) {
            anyhow::bail!("context checkpoint older_than_days must be at most 365");
        }
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        if session.status == "imported" {
            anyhow::bail!("imported sessions are read-only archives");
        }
        let policy = self
            .get_context_checkpoint_policy(&session.project_id)
            .await?;
        let keep_latest = keep_latest
            .unwrap_or(policy.max_checkpoints)
            .min(crate::context::MAX_CONTEXT_CHECKPOINT_POLICY_ITEMS)
            as usize;
        let age_days = match older_than_days {
            Some(0) => None,
            Some(days) => Some(days),
            None if policy.max_age_days == 0 => None,
            None => Some(policy.max_age_days),
        };
        let cutoff = age_days
            .map(|days| time::OffsetDateTime::now_utc() - time::Duration::days(i64::from(days)));
        let store = self.store.clone();
        let id = session_id.to_string();
        let checkpoints =
            tokio::task::spawn_blocking(move || store.list_context_checkpoints(&id, 10_000))
                .await??;
        let active = is_active_session_status(&session.status);
        let mut candidates = Vec::new();
        for (index, checkpoint) in checkpoints.iter().enumerate() {
            let protected_by_count = index >= checkpoints.len().saturating_sub(keep_latest);
            let protected_active = active && index + 1 == checkpoints.len();
            let old = cutoff.is_none_or(|cutoff| {
                time::OffsetDateTime::parse(
                    &checkpoint.created_at,
                    &time::format_description::well_known::Rfc3339,
                )
                .map(|created_at| created_at < cutoff)
                .unwrap_or(false)
            });
            if !protected_by_count && !protected_active && old {
                candidates.push(checkpoint.clone());
            }
        }
        if dry_run {
            return Ok(vp_protocol::types::ContextCheckpointPruneResult {
                session_id: session_id.to_string(),
                candidates: candidates.len(),
                removed: 0,
                dry_run: true,
                checkpoints: candidates,
            });
        }
        let mut removed = 0usize;
        for checkpoint in &candidates {
            let store = self.store.clone();
            let id = session_id.to_string();
            let checkpoint_id = checkpoint.id.clone();
            let event = tokio::task::spawn_blocking(move || {
                store.delete_context_checkpoint_with_event(&id, &checkpoint_id, "prune")
            })
            .await??;
            if let Some(event) = event {
                let _ = self.events.send(event);
                removed += 1;
            }
        }
        Ok(vp_protocol::types::ContextCheckpointPruneResult {
            session_id: session_id.to_string(),
            candidates: candidates.len(),
            removed,
            dry_run: false,
            checkpoints: candidates,
        })
    }

    /// Run the opt-in context checkpoint retention policy for every project.
    pub async fn sweep_context_checkpoint_retention(&self) -> Result<usize> {
        let store = self.store.clone();
        let policies =
            tokio::task::spawn_blocking(move || store.list_enabled_context_checkpoint_policies())
                .await??;
        let mut removed = 0usize;
        for (project_id, policy) in policies {
            let store = self.store.clone();
            let sessions =
                tokio::task::spawn_blocking(move || store.list_sessions(Some(&project_id)))
                    .await??;
            for session in sessions {
                if session.status == "imported" {
                    continue;
                }
                let result = self
                    .prune_context_checkpoints(
                        &session.id,
                        (policy.max_age_days > 0).then_some(policy.max_age_days),
                        Some(policy.max_checkpoints),
                        false,
                    )
                    .await?;
                removed = removed.saturating_add(result.removed);
            }
        }
        Ok(removed)
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
                    source.unlimited,
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
            "unlimited": new_session.unlimited,
            "budget_mode": if new_session.unlimited { "unlimited" } else { "limited" },
            "base_commit": new_session.base_commit,
            "prompt_layout": new_session.prompt_layout,
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

    pub async fn plan_revisions(&self, session_id: &str) -> Result<Vec<PlanRevisionInfo>> {
        let events = self.recent_events(session_id, 5_000).await?;
        Ok(crate::orchestration::revisions_from_events(&events))
    }

    pub async fn critical_path_policy(&self, project_id: &str) -> Result<CriticalPathPolicy> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking(move || store.get_critical_path_policy(&project_id)).await?
    }

    pub async fn set_critical_path_policy(
        &self,
        project_id: &str,
        policy: &CriticalPathPolicy,
    ) -> Result<CriticalPathPolicy> {
        policy
            .validate()
            .map_err(|error| anyhow::anyhow!("invalid critical-path policy: {error}"))?;
        let store = self.store.clone();
        let project_id_owned = project_id.to_string();
        let policy_owned = policy.clone();
        tokio::task::spawn_blocking(move || {
            if store.get_project(&project_id_owned)?.is_none() {
                return Err(anyhow::anyhow!("project not found: {project_id_owned}"));
            }
            store.set_critical_path_policy(&project_id_owned, &policy_owned)
        })
        .await??;
        self.record_project_manifest().await?;
        self.critical_path_policy(project_id).await
    }

    /// Observed input+output tokens per task. The read only happens for
    /// multi-path policies that actually blend observed spend; single mode
    /// never touches the ledger here.
    async fn governor_observed_tokens(
        &self,
        session_id: &str,
        policy: &CriticalPathPolicy,
    ) -> Result<HashMap<String, u64>> {
        if policy.mode != vp_protocol::types::CriticalPathMode::Multi
            || policy.observed_weight_ratio <= 0.0
        {
            return Ok(HashMap::new());
        }
        let store = self.store.clone();
        let session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.usage_tokens_by_task(&session_id)).await?
    }

    /// Bounded newest `budget.governor.decision` history. The events remain the
    /// source of truth; this is a read-only projection for the CLI/Swift UI.
    pub async fn governor_decisions(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<Vec<BudgetGovernorDecision>> {
        let events = self.recent_events(session_id, 5_000).await?;
        Ok(crate::budget_governor::decisions_from_events(
            &events,
            limit.clamp(1, 100) as usize,
        ))
    }

    /// Live critical-path governor status for one session. It evaluates the
    /// same pure function as scheduler admission; `governor.preview` uses the
    /// same builder with an optional remaining-token override.
    pub async fn budget_governor_status(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<GovernorSessionStatus> {
        self.build_governor_status(session_id, None, limit, None)
            .await
    }

    pub async fn preview_budget_governor(
        &self,
        session_id: &str,
        remaining_tokens: Option<u64>,
        limit: u32,
    ) -> Result<GovernorSessionStatus> {
        self.preview_budget_governor_with_mode(session_id, remaining_tokens, limit, None)
            .await
    }

    /// Preview with an explicit `single`/`multi` override. This changes only
    /// the preview evaluation; the project policy is not persisted or
    /// rewritten. The response's `policy` reflects the effective preview mode.
    pub async fn preview_budget_governor_with_mode(
        &self,
        session_id: &str,
        remaining_tokens: Option<u64>,
        limit: u32,
        mode_override: Option<vp_protocol::types::CriticalPathMode>,
    ) -> Result<GovernorSessionStatus> {
        self.build_governor_status(session_id, remaining_tokens, limit, mode_override)
            .await
    }

    async fn build_governor_status(
        &self,
        session_id: &str,
        remaining_override: Option<u64>,
        limit: u32,
        mode_override: Option<vp_protocol::types::CriticalPathMode>,
    ) -> Result<GovernorSessionStatus> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        let mut policy = self.critical_path_policy(&session.project_id).await?;
        if let Some(mode) = mode_override {
            // Preview override only. `normalized` keeps the rest of the policy
            // exactly as persisted; no write path is touched.
            policy.mode = mode;
            policy = policy.normalized();
        }
        let plan = self.get_team_plan(session_id).await?;
        let queue = self.task_queue(None, Some(session_id.to_string())).await?;
        let tasks = queue.tasks;
        let mut statuses = tasks
            .iter()
            .map(|task| (task.task_id.clone(), task.status.clone()))
            .collect::<HashMap<_, _>>();
        if let Some(plan) = plan.as_ref() {
            for task in &plan.tasks {
                statuses
                    .entry(task.id.clone())
                    .or_insert_with(|| "pending".to_string());
            }
        }
        let plan_revision = self.current_plan_revision(session_id).await?;
        let reserved = self.budget_registry.reserved_for_session(&session.id);
        let spent = session.tokens_in.saturating_add(session.tokens_out);
        let budget = GovernorBudgetView::new(session.token_budget, spent, reserved);
        let remaining = remaining_override.or_else(|| budget.remaining_tokens());
        let observed_tokens = self.governor_observed_tokens(&session.id, &policy).await?;
        let evaluated_at = now_rfc3339();
        let last_decisions = self.governor_decisions(session_id, limit).await?;
        let mut status = GovernorSessionStatus {
            session_id: session.id.clone(),
            project_id: session.project_id.clone(),
            session_status: session.status.clone(),
            plan_revision,
            enabled: policy.enabled && !session.unlimited,
            unlimited: session.unlimited,
            plan_present: plan.is_some(),
            policy: policy.clone(),
            critical_path: None,
            reserve: crate::budget_governor::reserve_math(&policy, budget, remaining, 0, true)
                .info(),
            inputs_hash: String::new(),
            decisions: Vec::new(),
            admitted: Vec::new(),
            deferred: Vec::new(),
            last_decisions,
            paths: Vec::new(),
            departments: Vec::new(),
            deferred_order: Vec::new(),
            error: None,
            generated_at: evaluated_at.clone(),
        };
        let Some(plan) = plan else {
            return Ok(status);
        };
        let orchestration = self.orchestration_policy(&session.project_id).await?;
        let infos = governor_candidate_infos(&session, &plan, &tasks, &orchestration);
        let candidates = infos
            .iter()
            .map(|info| crate::budget_governor::GovernorCandidate {
                task: &info.task,
                effective_priority: info.effective_priority,
                enqueued_at: info.enqueued_at.as_str(),
            })
            .collect::<Vec<_>>();
        if session.unlimited {
            status.critical_path = crate::critical_path::compute_critical_path(
                crate::critical_path::CriticalPathInput {
                    session_id: &session.id,
                    project_id: &session.project_id,
                    plan_revision,
                    plan: &plan,
                    statuses: &statuses,
                    remaining_budget_tokens: None,
                },
            )
            .ok()
            .map(|path| path.info());
            if policy.mode == vp_protocol::types::CriticalPathMode::Multi {
                status.paths =
                    crate::multi_path::compute_multi_path(crate::multi_path::MultiPathInput {
                        session_id: &session.id,
                        project_id: &session.project_id,
                        plan_revision,
                        plan: &plan,
                        statuses: &statuses,
                        remaining_budget_tokens: None,
                        policy: &policy,
                        observed_tokens: &observed_tokens,
                    })
                    .ok()
                    .map(|allocation| allocation.path_infos())
                    .unwrap_or_default();
            }
            return Ok(status);
        }
        match crate::budget_governor::evaluate(crate::budget_governor::GovernorEvaluation {
            session_id: &session.id,
            project_id: &session.project_id,
            plan_revision,
            plan: &plan,
            statuses: &statuses,
            policy: &policy,
            candidates: &candidates,
            budget,
            remaining_override,
            observed_tokens: &observed_tokens,
            evaluated_at: &evaluated_at,
        }) {
            Ok(admission) => {
                status.enabled = admission.enabled;
                status.critical_path = Some(admission.path.info());
                status.reserve = admission.reserve.info();
                status.inputs_hash = admission.inputs_hash.clone();
                status.decisions = admission.decision_infos();
                status.admitted = admission.admitted.clone();
                status.deferred = admission.deferral_infos();
                status.paths = admission.path_infos();
                status.departments = admission.department_infos();
                status.deferred_order = admission.deferred_order();
            }
            Err(failure) => {
                status.enabled = policy.enabled;
                status.error = Some(failure.info());
            }
        }
        Ok(status)
    }

    /// Static governor context for one provider run. It is computed once at
    /// run start; a plan revision or completion changes the next run or
    /// scheduler tick, never an in-flight provider request.
    pub async fn governor_run_context(
        &self,
        session_id: &str,
        task_id: Option<&str>,
    ) -> Result<Option<crate::budget_governor::GovernorRunContext>> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        if session.unlimited {
            return Ok(None);
        }
        let policy = self.critical_path_policy(&session.project_id).await?;
        if !policy.enabled {
            return Ok(None);
        }
        let Some(plan) = self.get_team_plan(session_id).await? else {
            return Ok(None);
        };
        let queue = self.task_queue(None, Some(session_id.to_string())).await?;
        let statuses = queue
            .tasks
            .iter()
            .map(|task| (task.task_id.clone(), task.status.clone()))
            .collect::<HashMap<_, _>>();
        let plan_revision = self.current_plan_revision(session_id).await?;
        let reserved = self.budget_registry.reserved_for_session(&session.id);
        let remaining = session.token_budget.map(|limit| {
            limit
                .saturating_sub(session.tokens_in)
                .saturating_sub(session.tokens_out)
                .saturating_sub(reserved)
        });
        let path =
            crate::critical_path::compute_critical_path(crate::critical_path::CriticalPathInput {
                session_id: &session.id,
                project_id: &session.project_id,
                plan_revision,
                plan: &plan,
                statuses: &statuses,
                remaining_budget_tokens: remaining,
            })?;
        if policy.mode == vp_protocol::types::CriticalPathMode::Multi {
            let observed_tokens = self.governor_observed_tokens(&session.id, &policy).await?;
            let allocation =
                crate::multi_path::compute_multi_path(crate::multi_path::MultiPathInput {
                    session_id: &session.id,
                    project_id: &session.project_id,
                    plan_revision,
                    plan: &plan,
                    statuses: &statuses,
                    remaining_budget_tokens: remaining,
                    policy: &policy,
                    observed_tokens: &observed_tokens,
                })?;
            let critical = task_id.is_some_and(|task| {
                allocation
                    .paths
                    .iter()
                    .any(|path| path.task_ids.iter().any(|id| id == task))
            });
            let runner_path = task_id
                .map(|task| allocation.task_path_id(task))
                .unwrap_or(None);
            let runner_allocation = runner_path
                .and_then(|path_id| allocation.paths.get(path_id as usize))
                .map(|path| path.allocation_tokens);
            let mut path_task_ids = allocation
                .paths
                .iter()
                .flat_map(|path| path.task_ids.iter().cloned())
                .collect::<Vec<_>>();
            path_task_ids.sort();
            path_task_ids.dedup();
            return Ok(Some(crate::budget_governor::GovernorRunContext {
                enabled: true,
                policy,
                task_id: task_id.map(str::to_string),
                critical,
                path_task_ids,
                path_weight_tokens: allocation.path_weight_tokens,
                path_count: allocation.paths.len(),
                path_allocation_tokens: runner_allocation,
                path_deficit_tokens: task_id
                    .map(|task| allocation.task_deficit(task))
                    .unwrap_or(0),
                plan_revision,
                inputs_hash: allocation.inputs_hash,
            }));
        }
        let critical = task_id.is_some_and(|task| path.task_ids.iter().any(|id| id == task));
        Ok(Some(crate::budget_governor::GovernorRunContext {
            enabled: true,
            policy,
            task_id: task_id.map(str::to_string),
            critical,
            path_task_ids: path.task_ids.clone(),
            path_weight_tokens: path.weight_tokens,
            path_count: usize::from(!path.task_ids.is_empty()),
            path_allocation_tokens: None,
            path_deficit_tokens: 0,
            plan_revision,
            inputs_hash: path.inputs_hash,
        }))
    }

    /// Persist one scheduler admission decision. Preview never calls this.
    pub async fn record_budget_governor_admission(
        &self,
        session_id: &str,
        admission: &crate::budget_governor::GovernorAdmission,
    ) -> Result<()> {
        if !admission.enabled {
            return Ok(());
        }
        let decision_id = format!("gov-{}", Uuid::now_v7());
        let now = now_rfc3339();
        let payload =
            crate::budget_governor::admission_payload(session_id, &decision_id, admission, &now);
        self.append_event(
            session_id,
            crate::budget_governor::DECISION_EVENT_KIND,
            payload,
        )
        .await?;
        Ok(())
    }

    pub async fn revise_team_plan(
        &self,
        session_id: &str,
        plan: TeamPlan,
        reason: &str,
        author: &str,
        expected_revision: Option<u32>,
        max_managers: usize,
    ) -> Result<ReviseTeamPlanResult> {
        let reason = reason.trim();
        if reason.is_empty() || reason.chars().count() > 1_000 {
            anyhow::bail!("revision reason must be 1-1000 chars");
        }
        let author = author.trim();
        if author.is_empty() || author.chars().count() > 80 {
            anyhow::bail!("revision author must be 1-80 chars");
        }
        let session_id = session_id.to_string();
        let store = self.store.clone();
        let session = tokio::task::spawn_blocking({
            let store = store.clone();
            let session_id = session_id.clone();
            move || store.get_session(&session_id)
        })
        .await??
        .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        crate::ensure_session_quiescent(&session, "revise the team plan")?;
        let current_raw = tokio::task::spawn_blocking({
            let store = store.clone();
            let session_id = session_id.clone();
            move || store.get_team_plan(&session_id)
        })
        .await??;
        let current: TeamPlan = current_raw
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok())
            .ok_or_else(|| anyhow::anyhow!("session has no TeamPlan to revise"))?;
        let tasks = tokio::task::spawn_blocking({
            let store = store.clone();
            let session_id = session_id.clone();
            move || store.list_plan_tasks(None, Some(&session_id))
        })
        .await??;
        let events = self.recent_events(&session_id, 5_000).await?;
        let current_revision = crate::orchestration::plan_revision(&events);
        if let Some(expected) = expected_revision
            && expected != current_revision
        {
            anyhow::bail!(
                "plan revision conflict: expected {expected}, current {current_revision}"
            );
        }
        crate::agent::validate_team_plan(&plan, max_managers)?;
        let statuses = crate::orchestration::status_map(&current, &tasks, &events);
        crate::orchestration::ensure_revision_safe(&current, &plan, &statuses)?;
        let diff = crate::orchestration::diff_plan(&current, &plan);
        let revision = current_revision.saturating_add(1);
        let snapshot = self.pin_plan_snapshot(&session, &plan, revision).await?;
        let snapshot_json = serde_json::to_string(&snapshot)?;
        let payload = serde_json::json!({
            "plan": plan,
            "revision": revision,
            "reason": reason,
            "author": author,
            "task_count": plan.tasks.len(),
            "added": diff.added,
            "removed": diff.removed,
            "changed": diff.changed,
            "runtime_snapshot": snapshot,
        });
        let plan_json = serde_json::to_string(&plan)?;
        let event = tokio::task::spawn_blocking({
            let store = store.clone();
            let session_id = session_id.clone();
            let plan_json = plan_json.clone();
            let payload = payload.clone();
            let snapshot_json = snapshot_json.clone();
            move || {
                store.set_team_plan_revision_with_runtime_snapshot_and_event(
                    &session_id,
                    &plan_json,
                    "team_plan.revised",
                    payload,
                    Some(&snapshot_json),
                )
            }
        })
        .await??;
        let _ = self.events.send(event.clone());
        self.scheduler.sync_plan(&session, &plan).await?;
        Ok(ReviseTeamPlanResult {
            revision: PlanRevisionInfo {
                revision,
                seq: event.seq,
                kind: event.kind,
                reason: Some(reason.to_string()),
                author: author.to_string(),
                task_count: plan.tasks.len(),
                added: diff.added,
                removed: diff.removed,
                changed: diff.changed,
                created_at: event.created_at,
            },
            plan,
        })
    }

    pub async fn coordination_notes(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<Vec<OrchestrationNote>> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.list_coordination_notes(&session_id, limit))
            .await?
    }

    pub async fn orchestration_decisions(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<Vec<OrchestrationDecision>> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.list_orchestration_decisions(&session_id, limit))
            .await?
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn record_coordination_note(
        &self,
        session_id: &str,
        action: &str,
        severity: &str,
        task_id: Option<&str>,
        target_agent_id: Option<&str>,
        target_note_id: Option<&str>,
        agent_id: &str,
        agent_name: &str,
        agent_role: Option<&str>,
        message: &str,
    ) -> Result<Event> {
        let store = self.store.clone();
        let session_id_owned = session_id.to_string();
        let action = action.to_string();
        let severity = severity.to_string();
        let task_id = task_id.map(str::to_string);
        let target_agent_id = target_agent_id.map(str::to_string);
        let target_note_id = target_note_id.map(str::to_string);
        let agent_id = agent_id.to_string();
        let agent_name = agent_name.to_string();
        let agent_role = agent_role.map(str::to_string);
        let message = message.to_string();
        let event = tokio::task::spawn_blocking(move || {
            store.record_coordination_note_with_event(
                &session_id_owned,
                &action,
                &severity,
                task_id.as_deref(),
                target_agent_id.as_deref(),
                target_note_id.as_deref(),
                &agent_id,
                &agent_name,
                agent_role.as_deref(),
                &message,
            )
        })
        .await??;
        let _ = self.events.send(event.clone());
        // A targeted note is also delivered to the durable mailbox so the
        // recipient still sees it after the recent-coordination window moves on.
        let target = event
            .payload
            .get("target_agent_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let note_action = event
            .payload
            .get("action")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("note");
        if let Some(target) = target
            && !matches!(note_action, "unblock" | "list")
        {
            let agents = self.list_agents(session_id).await?;
            if let Some(agent) = agents.iter().find(|agent| agent.id == target) {
                let _ = self
                    .send_agent_message(
                        session_id,
                        event
                            .payload
                            .get("agent_id")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("orchestrator"),
                        event
                            .payload
                            .get("agent_name")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("Orchestrator"),
                        &agent.id,
                        &agent.name,
                        "coordination",
                        event
                            .payload
                            .get("message")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or(""),
                        serde_json::json!({
                            "note_id": event.payload.get("note_id"),
                            "action": note_action,
                            "task_id": event.payload.get("task_id"),
                            "severity": event.payload.get("severity"),
                        }),
                    )
                    .await;
            }
        }
        Ok(event)
    }

    pub async fn coordination_notes_for_agent(
        &self,
        session_id: &str,
        agent_id: &str,
        task_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<OrchestrationNote>> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let agent_id = agent_id.to_string();
        let task_id = task_id.map(str::to_string);
        tokio::task::spawn_blocking(move || {
            store.list_coordination_notes_for_agent(
                &session_id,
                &agent_id,
                task_id.as_deref(),
                limit,
            )
        })
        .await?
    }

    pub async fn plan_revision_detail(
        &self,
        session_id: &str,
        revision: u32,
    ) -> Result<PlanRevisionDetailResult> {
        let events = self.all_events(session_id).await?;
        let revisions = crate::orchestration::revisions_from_events(&events);
        let info = revisions
            .iter()
            .find(|candidate| candidate.revision == revision)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("plan revision {revision} not found"))?;
        let plan = events
            .iter()
            .filter(|event| {
                matches!(
                    event.kind.as_str(),
                    "team_plan.submitted" | "team_plan.revised"
                )
            })
            .filter(|event| {
                event
                    .payload
                    .get("revision")
                    .and_then(serde_json::Value::as_u64)
                    .map(|value| value as u32)
                    .unwrap_or_else(|| {
                        revisions
                            .iter()
                            .find(|candidate| candidate.seq == event.seq)
                            .map(|candidate| candidate.revision)
                            .unwrap_or(0)
                    })
                    == revision
            })
            .find_map(crate::orchestration::plan::plan_from_event)
            .ok_or_else(|| anyhow::anyhow!("plan revision {revision} has no stored plan"))?;
        Ok(PlanRevisionDetailResult {
            revision: info,
            plan,
        })
    }

    pub async fn restore_team_plan_revision(
        &self,
        session_id: &str,
        revision: u32,
        reason: &str,
        expected_revision: Option<u32>,
        max_managers: usize,
    ) -> Result<ReviseTeamPlanResult> {
        let detail = self.plan_revision_detail(session_id, revision).await?;
        self.revise_team_plan(
            session_id,
            detail.plan,
            reason,
            "user",
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
        max_managers: usize,
    ) -> Result<ReviseTeamPlanResult> {
        let current = self
            .get_team_plan(session_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("session has no TeamPlan to edit"))?;
        let mut plan = current.clone();
        let index = plan
            .tasks
            .iter()
            .position(|candidate| candidate.id == task.id)
            .ok_or_else(|| anyhow::anyhow!("task not found in plan: {}", task.id))?;
        plan.tasks[index] = task;
        self.revise_team_plan(
            session_id,
            plan,
            reason,
            "user",
            expected_revision,
            max_managers,
        )
        .await
    }

    pub async fn all_events(&self, session_id: &str) -> Result<Vec<Event>> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.list_all_events(&session_id)).await?
    }

    /// Read-only controller replay. This never calls a provider, executes a
    /// tool, appends an event, or mutates a projection.
    pub async fn replay_orchestration(
        &self,
        session_id: &str,
        limit: Option<u32>,
    ) -> Result<vp_protocol::replay::ControllerReplayReport> {
        crate::orchestration::replay::replay_orchestration(self, session_id, limit).await
    }

    pub async fn orchestration_snapshot(&self, session_id: &str) -> Result<OrchestrationSnapshot> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        let plan = self.get_team_plan(session_id).await?;
        let queue = self.task_queue(None, Some(session_id.to_string())).await?;
        let tasks = queue.tasks;
        let progress = queue.progress;
        let store = self.store.clone();
        let id = session_id.to_string();
        let recoveries = tokio::task::spawn_blocking(move || {
            store.list_current_orchestration_recoveries(None, Some(&id))
        })
        .await??;
        let stall_threshold_secs = match task_stall_threshold_secs() {
            0 => None,
            secs => Some(secs),
        };
        let agents = self.list_agents(session_id).await?;
        let merge_queue = self.list_merge_queue(session_id).await?;
        let questions = self
            .list_questions(Some(session_id.to_string()))
            .await?
            .questions;
        let events = self.recent_events(session_id, 5_000).await?;
        let notes = self.coordination_notes(session_id, 2_000).await?;
        let decisions = self.orchestration_decisions(session_id, 2_000).await?;
        let policy = self.orchestration_policy(&session.project_id).await?;
        Ok(crate::orchestration::build_snapshot(
            crate::orchestration::OrchestrationInput {
                session: &session,
                plan: plan.as_ref(),
                scheduled: &tasks,
                agents: &agents,
                events: &events,
                merge_queue: &merge_queue,
                questions: &questions,
                notes: &notes,
                decisions: &decisions,
                progress: &progress,
                recoveries: &recoveries,
                stall_threshold_secs,
                policy: &policy,
            },
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn record_user_coordination_note(
        &self,
        session_id: &str,
        action: &str,
        task_id: Option<&str>,
        target_agent_id: Option<&str>,
        target_note_id: Option<&str>,
        message: &str,
        severity: Option<&str>,
    ) -> Result<String> {
        if !matches!(
            action,
            "note" | "blocker" | "unblock" | "decision" | "escalate"
        ) {
            anyhow::bail!("unsupported coordination action: {action}");
        }
        let message = message.trim();
        if message.is_empty() || message.chars().count() > 2_000 {
            anyhow::bail!("coordination message must be 1-2000 chars");
        }
        let task_id = task_id.map(str::trim).filter(|task| !task.is_empty());
        if let Some(task_id) = task_id {
            crate::worktree::validate_task_id(task_id)?;
        }
        let severity = severity.unwrap_or(if matches!(action, "blocker" | "escalate") {
            "warning"
        } else {
            "info"
        });
        if !matches!(severity, "info" | "warning" | "critical") {
            anyhow::bail!("severity must be info, warning, or critical");
        }
        if let Some(target) = target_agent_id {
            let agents = self.list_agents(session_id).await?;
            if !agents.iter().any(|agent| agent.id == target) {
                anyhow::bail!("target agent is not in this session: {target}");
            }
        }
        if let Some(note) = target_note_id {
            let notes = self.coordination_notes(session_id, 2_000).await?;
            if !notes.iter().any(|candidate| candidate.note_id == note) {
                anyhow::bail!("coordination note not found: {note}");
            }
        }
        let event = self
            .record_coordination_note(
                session_id,
                action,
                severity,
                task_id,
                target_agent_id,
                target_note_id,
                "user",
                "User",
                Some("user"),
                message,
            )
            .await?;
        Ok(event
            .payload
            .get("note_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    /// Recent TeamPlan sessions across the selected project (or all projects)
    /// with their live metrics and top blockers. The daemon exposes this for
    /// Manage Dash; the per-session snapshot remains the source of truth.
    pub async fn orchestration_overview(
        &self,
        project_id: Option<&str>,
        limit: u32,
    ) -> Result<OrchestrationOverviewResult> {
        let sessions = self.list_sessions(project_id.map(str::to_string)).await?;
        let projects = self.list_projects().await?;
        let project_names = projects
            .into_iter()
            .map(|project| (project.id, project.name))
            .collect::<HashMap<_, _>>();
        let limit = limit.clamp(1, 50) as usize;
        let mut summaries = Vec::new();
        for session in sessions.into_iter().take(limit.saturating_mul(4)) {
            let Ok(snapshot) = self.orchestration_snapshot(&session.id).await else {
                continue;
            };
            if snapshot.plan.is_none() {
                continue;
            }
            let blockers = snapshot
                .blockers
                .iter()
                .take(3)
                .map(|blocker| format!("{}: {}", blocker.kind, blocker.message))
                .collect::<Vec<_>>();
            let last = snapshot.decisions.last().cloned();
            let active_recoveries = snapshot
                .recoveries
                .iter()
                .filter(|recovery| {
                    matches!(recovery.status.as_str(), "requested" | "waiting_cancel")
                })
                .count();
            let failed_recoveries = snapshot
                .recoveries
                .iter()
                .filter(|recovery| recovery.status == "failed")
                .count();
            let last_recovery = snapshot
                .recoveries
                .iter()
                .max_by(|left, right| left.updated_at.cmp(&right.updated_at))
                .map(|recovery| {
                    format!(
                        "{}: {} ({})",
                        recovery.task_id,
                        recovery.status,
                        recovery
                            .last_error
                            .as_deref()
                            .unwrap_or(recovery.action.as_str())
                    )
                });
            summaries.push(OrchestrationSessionSummary {
                session_id: session.id.clone(),
                project_id: session.project_id.clone(),
                project_name: project_names
                    .get(&session.project_id)
                    .cloned()
                    .unwrap_or_else(|| session.project_id.clone()),
                goal: session.goal.clone(),
                status: session.status.clone(),
                priority: session.priority,
                plan_revision: snapshot.plan_revision,
                metrics: snapshot.metrics.clone(),
                blockers,
                last_decision: last.as_ref().map(|decision| decision.detail.clone()),
                last_decision_at: last.map(|decision| decision.created_at),
                recoveries: snapshot.recoveries.len(),
                active_recoveries,
                failed_recoveries,
                last_recovery,
            });
        }
        summaries.sort_by(|lhs, rhs| {
            let left_attention = lhs.metrics.attention_count;
            let right_attention = rhs.metrics.attention_count;
            if left_attention != right_attention {
                return right_attention.cmp(&left_attention);
            }
            let left_running = lhs.status == "running" || lhs.status == "queued";
            let right_running = rhs.status == "running" || rhs.status == "queued";
            if left_running != right_running {
                return right_running.cmp(&left_running);
            }
            rhs.priority.cmp(&lhs.priority)
        });
        summaries.truncate(limit);
        let totals = OrchestrationOverviewTotals {
            sessions: summaries.len(),
            running: summaries
                .iter()
                .filter(|summary| matches!(summary.status.as_str(), "running" | "queued"))
                .count(),
            attention: summaries
                .iter()
                .filter(|summary| summary.metrics.attention_count > 0)
                .count(),
            blocked_tasks: summaries
                .iter()
                .map(|summary| summary.metrics.blocked_tasks)
                .sum(),
            failed_tasks: summaries
                .iter()
                .map(|summary| summary.metrics.failed_tasks)
                .sum(),
            retrying_tasks: summaries
                .iter()
                .map(|summary| summary.metrics.retrying_tasks)
                .sum(),
            open_blockers: summaries
                .iter()
                .map(|summary| summary.metrics.open_blockers)
                .sum(),
            merge_conflicts: summaries
                .iter()
                .map(|summary| summary.metrics.merge_conflicts)
                .sum(),
            stalled_tasks: summaries
                .iter()
                .map(|summary| summary.metrics.stalled_tasks)
                .sum(),
            recoveries: summaries.iter().map(|summary| summary.recoveries).sum(),
            active_recoveries: summaries
                .iter()
                .map(|summary| summary.active_recoveries)
                .sum(),
            failed_recoveries: summaries
                .iter()
                .map(|summary| summary.failed_recoveries)
                .sum(),
        };
        Ok(OrchestrationOverviewResult {
            sessions: summaries,
            totals,
            generated_at: vp_protocol::now_rfc3339(),
        })
    }

    /// Preview the controller decisions without mutating task rows or notes.
    pub async fn preview_orchestration(
        &self,
        session_id: &str,
    ) -> Result<OrchestrationPreviewResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        let Some(plan) = self.get_team_plan(session_id).await? else {
            return Ok(OrchestrationPreviewResult {
                session_id: session_id.to_string(),
                actions: Vec::new(),
            });
        };
        let tasks = self
            .task_queue(None, Some(session_id.to_string()))
            .await?
            .tasks;
        let events = self.recent_events(session_id, 5_000).await?;
        let notes = self.coordination_notes(session_id, 2_000).await?;
        let policy = self.orchestration_policy(&session.project_id).await?;
        let actions = crate::orchestration::plan_actions(crate::orchestration::ControllerInput {
            session: &session,
            plan: &plan,
            scheduled: &tasks,
            events: &events,
            notes: &notes,
            policy: &policy,
            budget_exhausted: session_budget_exhausted(&session),
        });
        Ok(OrchestrationPreviewResult {
            session_id: session_id.to_string(),
            actions: actions
                .into_iter()
                .map(|action| OrchestrationPreviewAction {
                    action: action.action().to_string(),
                    task_id: action.task_id().to_string(),
                    detail: action.detail(),
                    attempt: action.attempt(),
                })
                .collect(),
        })
    }

    /// Run one deterministic controller pass for a session. Actions are
    /// idempotent: retries are gated by `next_attempt_at`, blocked rows are
    /// skipped once moved, and escalations are deduplicated by attempt.
    pub async fn advance_orchestration(
        &self,
        session_id: &str,
    ) -> Result<Vec<OrchestrationDecision>> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        let Some(plan) = self.get_team_plan(session_id).await? else {
            return Ok(Vec::new());
        };
        let tasks = self
            .task_queue(None, Some(session_id.to_string()))
            .await?
            .tasks;
        let events = self.recent_events(session_id, 5_000).await?;
        let notes = self.coordination_notes(session_id, 2_000).await?;
        let policy = self.orchestration_policy(&session.project_id).await?;
        let actions = crate::orchestration::plan_actions(crate::orchestration::ControllerInput {
            session: &session,
            plan: &plan,
            scheduled: &tasks,
            events: &events,
            notes: &notes,
            policy: &policy,
            budget_exhausted: session_budget_exhausted(&session),
        });
        let mut decisions = Vec::new();
        for action in actions {
            let task_id = action.task_id().to_string();
            let Some(row) = tasks.iter().find(|row| row.task_id == task_id) else {
                continue;
            };
            let plan_seq = row.plan_seq;
            match &action {
                crate::orchestration::ControllerAction::Retry {
                    task_id,
                    delay_secs,
                    reason,
                } => {
                    if let Err(error) = self
                        .schedule_plan_task_retry(
                            session_id,
                            plan_seq,
                            task_id,
                            std::time::Duration::from_secs(*delay_secs),
                            reason,
                        )
                        .await
                    {
                        tracing::warn!(%error, task_id, "orchestration retry failed");
                        continue;
                    }
                }
                crate::orchestration::ControllerAction::Block {
                    task_id,
                    dependency,
                    reason,
                } => {
                    let store = self.store.clone();
                    let session_id_owned = session_id.to_string();
                    let task_id_owned = task_id.clone();
                    let dependency = dependency.clone();
                    let reason = reason.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        store.block_plan_task_with_event(
                            &session_id_owned,
                            plan_seq,
                            &task_id_owned,
                            &reason,
                            &dependency,
                        )
                    })
                    .await??;
                    let Some((_task, event)) = result else {
                        continue;
                    };
                    let _ = self.events.send(event);
                }
                crate::orchestration::ControllerAction::Unblock { task_id, reason } => {
                    let store = self.store.clone();
                    let session_id_owned = session_id.to_string();
                    let task_id_owned = task_id.clone();
                    let reason = reason.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        store.unblock_plan_task_with_event(
                            &session_id_owned,
                            plan_seq,
                            &task_id_owned,
                            &reason,
                        )
                    })
                    .await??;
                    let Some((_task, event)) = result else {
                        continue;
                    };
                    let _ = self.events.send(event);
                }
                crate::orchestration::ControllerAction::Cancel {
                    task_id,
                    dependency,
                    reason,
                } => {
                    let store = self.store.clone();
                    let session_id_owned = session_id.to_string();
                    let task_id_owned = task_id.clone();
                    let dependency = dependency.clone();
                    let reason = reason.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        store.cancel_plan_task_with_event(
                            &session_id_owned,
                            plan_seq,
                            &task_id_owned,
                            &reason,
                            &dependency,
                        )
                    })
                    .await??;
                    let Some((_task, event)) = result else {
                        continue;
                    };
                    let _ = self.events.send(event);
                }
                crate::orchestration::ControllerAction::Escalate {
                    task_id,
                    attempt: _,
                    message,
                } => {
                    if let Err(error) = self
                        .record_coordination_note(
                            session_id,
                            "escalate",
                            "critical",
                            Some(task_id),
                            None,
                            None,
                            "orchestrator",
                            "Orchestrator",
                            Some("orchestrator"),
                            message,
                        )
                        .await
                    {
                        tracing::warn!(%error, task_id, "orchestration escalation failed");
                        continue;
                    }
                }
                crate::orchestration::ControllerAction::ResolveBlockers { task_id, count } => {
                    if let Err(error) = self
                        .record_coordination_note(
                            session_id,
                            "unblock",
                            "info",
                            Some(task_id),
                            None,
                            None,
                            "orchestrator",
                            "Orchestrator",
                            Some("orchestrator"),
                            &format!(
                                "{task_id} completed; {count} coordination blocker(s) resolved automatically"
                            ),
                        )
                        .await
                    {
                        tracing::warn!(%error, task_id, "orchestration blocker resolution failed");
                        continue;
                    }
                }
            }
            decisions.push(
                self.record_orchestration_decision(
                    session_id,
                    action.action(),
                    Some(action.task_id()),
                    &action.detail(),
                    action.attempt(),
                )
                .await?,
            );
        }
        Ok(decisions)
    }

    pub async fn record_orchestration_decision(
        &self,
        session_id: &str,
        action: &str,
        task_id: Option<&str>,
        detail: &str,
        attempt: u32,
    ) -> Result<OrchestrationDecision> {
        let store = self.store.clone();
        let session_id_owned = session_id.to_string();
        let action_owned = action.to_string();
        let task_id_owned = task_id.map(str::to_string);
        let detail_owned = detail.to_string();
        let event = tokio::task::spawn_blocking(move || {
            store.record_orchestration_decision_with_event(
                &session_id_owned,
                &action_owned,
                task_id_owned.as_deref(),
                &detail_owned,
                attempt,
                "orchestrator",
            )
        })
        .await??;
        let _ = self.events.send(event.clone());
        Ok(OrchestrationDecision {
            decision_id: event.payload["decision_id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            action: action.to_string(),
            task_id: task_id.map(str::to_string),
            detail: detail.to_string(),
            attempt,
            agent_id: "orchestrator".to_string(),
            created_at: event.created_at,
        })
    }

    pub async fn schedule_plan_task_retry(
        &self,
        session_id: &str,
        plan_seq: u64,
        task_id: &str,
        delay: std::time::Duration,
        reason: &str,
    ) -> Result<ScheduledTask> {
        let next = time::OffsetDateTime::now_utc()
            + time::Duration::seconds(delay.as_secs().min(i64::MAX as u64) as i64);
        let next = next
            .format(&time::format_description::well_known::Rfc3339)
            .context("formatting retry time")?;
        let store = self.store.clone();
        let session_id_owned = session_id.to_string();
        let task_id_owned = task_id.to_string();
        let reason_owned = reason.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.schedule_plan_task_retry_with_event(
                &session_id_owned,
                plan_seq,
                &task_id_owned,
                &next,
                &reason_owned,
            )
        })
        .await??;
        let (task, event) =
            result.ok_or_else(|| anyhow::anyhow!("task {task_id} cannot be retried"))?;
        let _ = self.events.send(event);
        self.scheduler.notify_state();
        Ok(task)
    }

    pub async fn set_team_plan(&self, session_id: &str, plan: &TeamPlan) -> Result<()> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let json = serde_json::to_string(plan)?;
        let loaded = {
            let store = store.clone();
            let id = id.clone();
            tokio::task::spawn_blocking(move || store.get_session(&id)).await??
        }
        .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        let revision = self
            .current_plan_revision(session_id)
            .await?
            .saturating_add(1);
        let snapshot = self.pin_plan_snapshot(&loaded, plan, revision).await?;
        let snapshot_json = serde_json::to_string(&snapshot)?;
        let event = tokio::task::spawn_blocking(move || {
            store.set_team_plan_with_runtime_snapshot_and_event(&id, &json, Some(&snapshot_json))
        })
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

    pub async fn recent_events(&self, session_id: &str, limit: u32) -> Result<Vec<Event>> {
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.recent_events(&id, limit)).await?
    }

    pub async fn recent_events_of_kind(
        &self,
        session_id: &str,
        kind: &str,
        limit: u32,
    ) -> Result<Vec<Event>> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let kind = kind.to_string();
        tokio::task::spawn_blocking(move || store.recent_events_by_kind(&id, &kind, limit)).await?
    }

    /// Bounded cross-session cache-epoch observation for `session.cache`.
    /// This is a read-only scan of recent `context.cache` events in other
    /// sessions; it never calls a provider and never claims a guaranteed hit.
    pub async fn cache_epoch_cross_session(
        &self,
        epoch: &str,
        exclude_session: &str,
    ) -> Result<crate::prompt_cache::CacheEpochReuse> {
        const CROSS_SESSION_SCAN_LIMIT: u32 = 5_000;
        let store = self.store.clone();
        let epoch = epoch.to_string();
        let exclude = exclude_session.to_string();
        let (sessions, hit_tokens, calls) = tokio::task::spawn_blocking(move || {
            store.cache_epoch_cross_session(&epoch, &exclude, CROSS_SESSION_SCAN_LIMIT)
        })
        .await??;
        Ok(crate::prompt_cache::CacheEpochReuse {
            sessions,
            hit_tokens,
            calls,
        })
    }

    pub async fn task_queue(
        &self,
        project_id: Option<String>,
        session_id: Option<String>,
    ) -> Result<TaskQueueResult> {
        self.scheduler.queue(project_id, session_id).await
    }

    /// Create an operator steering request for one running agent/run. When
    /// `agent_id` is omitted the session Leader is targeted; there is no
    /// implicit broadcast to Workers. An optional operation id replays the
    /// first steering row instead of creating a duplicate.
    pub async fn steer_session(
        &self,
        session_id: &str,
        body: &str,
        agent_id: Option<&str>,
        task_id: Option<&str>,
        operation_id: Option<&str>,
    ) -> Result<SessionSteerResult> {
        let payload = serde_json::json!({
            "session_id": session_id,
            "body": body,
            "agent_id": agent_id,
            "task_id": task_id,
        });
        let receipt = match crate::operation::begin(
            self.store(),
            session_id,
            operation_id,
            "session.steer",
            &payload,
        )
        .await?
        {
            crate::operation::OperationStart::Replay(result) => {
                return crate::operation::replay(&result);
            }
            crate::operation::OperationStart::Proceed(receipt) => receipt,
        };
        let result = self
            .steer_session_inner(session_id, body, agent_id, task_id)
            .await;
        match &result {
            Ok(value) => {
                if let Err(error) = crate::operation::complete(&receipt, value).await {
                    return Err(error.context(
                        "steering was created but the session operation receipt could not be completed",
                    ));
                }
            }
            Err(error) => {
                crate::operation::fail(&receipt, &format!("{error:#}")).await;
            }
        }
        result
    }

    async fn steer_session_inner(
        &self,
        session_id: &str,
        body: &str,
        agent_id: Option<&str>,
        task_id: Option<&str>,
    ) -> Result<SessionSteerResult> {
        let body = body.trim();
        if body.is_empty() || body.chars().count() > 4_000 {
            anyhow::bail!("steering body must be 1-4000 chars");
        }
        let task_id = task_id.map(str::trim).filter(|task| !task.is_empty());
        if let Some(task_id) = task_id {
            crate::worktree::validate_task_id(task_id)?;
        }
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        if session.status == "imported" {
            anyhow::bail!("imported sessions are read-only archives");
        }
        let agents = self.list_agents(session_id).await?;
        let target = match agent_id {
            Some(agent_id) => agents
                .iter()
                .find(|agent| agent.id == agent_id)
                .ok_or_else(|| anyhow!("target agent is not in this session: {agent_id}"))?,
            None => agents
                .iter()
                .find(|agent| {
                    agent.role == "leader"
                        && matches!(agent.status.as_str(), "running" | "waiting_input")
                })
                .ok_or_else(|| {
                    anyhow!(
                        "no running Leader in this session; pass --agent <id> or start the session"
                    )
                })?,
        };
        if !matches!(target.status.as_str(), "running" | "waiting_input") {
            anyhow::bail!(
                "target agent {} is {}; steering requires a running or waiting agent",
                target.id,
                target.status
            );
        }
        if let Some(task_id) = task_id {
            if target.task_id.as_deref() != Some(task_id) {
                anyhow::bail!(
                    "target agent {} is not working on task {task_id}",
                    target.id
                );
            }
            if let Some(plan) = self.get_team_plan(session_id).await?
                && !plan.tasks.iter().any(|task| task.id == task_id)
            {
                anyhow::bail!("task {task_id} is not in the stored TeamPlan");
            }
        }
        let target_id = target.id.clone();
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let body = body.to_string();
        let task_id = task_id.map(str::to_string);
        let (steering, event) = tokio::task::spawn_blocking(move || {
            store.create_steering_with_event(&session_id, &target_id, task_id.as_deref(), &body)
        })
        .await??;
        let _ = self.events.send(event);
        Ok(SessionSteerResult { steering })
    }

    pub async fn session_steering(
        &self,
        session_id: &str,
        status: Option<&str>,
        limit: Option<u32>,
    ) -> Result<SessionSteeringListResult> {
        if let Some(status) = status
            && !matches!(status, "pending" | "delivered" | "cancelled")
        {
            anyhow::bail!("steering status must be pending, delivered, or cancelled");
        }
        let store = self.store.clone();
        let id = session_id.to_string();
        let status = status.map(str::to_string);
        let steering =
            tokio::task::spawn_blocking(move || store.list_steering(&id, status.as_deref(), limit))
                .await??;
        Ok(SessionSteeringListResult {
            session_id: session_id.to_string(),
            steering,
        })
    }

    pub async fn cancel_steering(&self, id: &str) -> Result<SessionSteeringCancelResult> {
        let store = self.store.clone();
        let id = id.to_string();
        let (steering, event) =
            tokio::task::spawn_blocking(move || store.cancel_steering_with_event(&id)).await??;
        let _ = self.events.send(event);
        Ok(SessionSteeringCancelResult {
            cancelled: true,
            steering,
        })
    }

    /// Defer an operator prompt until watched plan tasks settle or a deadline
    /// arrives. This is a persisted session-level continuation, not a plan
    /// dependency: it never changes task ordering, approvals, budgets, or
    /// write scopes.
    #[allow(clippy::too_many_arguments)]
    pub async fn defer_session(
        &self,
        session_id: &str,
        body: &str,
        watch_task_ids: Vec<String>,
        watch_mode: Option<&str>,
        deadline_at: Option<&str>,
        after_secs: Option<u64>,
        max_attempts: Option<u32>,
        every_secs: Option<u64>,
        max_runs: Option<u32>,
        operation_id: Option<&str>,
    ) -> Result<SessionDeferResult> {
        self.defer_session_watching_gate(
            session_id,
            body,
            watch_task_ids,
            watch_mode,
            deadline_at,
            after_secs,
            max_attempts,
            every_secs,
            max_runs,
            None,
            operation_id,
        )
        .await
    }

    /// Defer an operator prompt until watched plan tasks settle, a deadline
    /// arrives, or an existing external gate resolves, expires, or is
    /// cancelled. A gate watch is additive; it does not change approvals,
    /// budgets, write scopes, the Verifier, or the merge queue.
    #[allow(clippy::too_many_arguments)]
    pub async fn defer_session_watching_gate(
        &self,
        session_id: &str,
        body: &str,
        watch_task_ids: Vec<String>,
        watch_mode: Option<&str>,
        deadline_at: Option<&str>,
        after_secs: Option<u64>,
        max_attempts: Option<u32>,
        every_secs: Option<u64>,
        max_runs: Option<u32>,
        watch_gate_id: Option<&str>,
        operation_id: Option<&str>,
    ) -> Result<SessionDeferResult> {
        let mut payload = serde_json::json!({
            "session_id": session_id,
            "body": body,
            "watch_task_ids": &watch_task_ids,
            "watch_mode": watch_mode,
            "deadline_at": deadline_at,
            "after_secs": after_secs,
            "max_attempts": max_attempts,
            "every_secs": every_secs,
            "max_runs": max_runs,
        });
        if let Some(watch_gate_id) = watch_gate_id {
            payload["watch_gate_id"] = serde_json::json!(watch_gate_id);
        }
        let receipt = match crate::operation::begin(
            self.store(),
            session_id,
            operation_id,
            "session.defer",
            &payload,
        )
        .await?
        {
            crate::operation::OperationStart::Replay(result) => {
                return crate::operation::replay(&result);
            }
            crate::operation::OperationStart::Proceed(receipt) => receipt,
        };
        let result = self
            .defer_session_inner(
                session_id,
                body,
                watch_task_ids,
                watch_mode,
                deadline_at,
                after_secs,
                max_attempts,
                every_secs,
                max_runs,
                watch_gate_id,
            )
            .await;
        match &result {
            Ok(value) => {
                if let Err(error) = crate::operation::complete(&receipt, value).await {
                    return Err(error.context(
                        "continuation was created but the session operation receipt could not be completed",
                    ));
                }
            }
            Err(error) => {
                crate::operation::fail(&receipt, &format!("{error:#}")).await;
            }
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn defer_session_inner(
        &self,
        session_id: &str,
        body: &str,
        watch_task_ids: Vec<String>,
        watch_mode: Option<&str>,
        deadline_at: Option<&str>,
        after_secs: Option<u64>,
        max_attempts: Option<u32>,
        every_secs: Option<u64>,
        max_runs: Option<u32>,
        watch_gate_id: Option<&str>,
    ) -> Result<SessionDeferResult> {
        let body = crate::continuation::validate_body(body)?;
        let watch_mode = crate::continuation::normalize_watch_mode(watch_mode)?;
        let recurrence = crate::continuation::normalize_recurrence(every_secs, max_runs)?;
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        if session.status == "imported" {
            anyhow::bail!("imported sessions are read-only archives");
        }
        if session.status == "landed" {
            anyhow::bail!("landed sessions are final; create a new session or fork to continue");
        }

        let mut seen = std::collections::HashSet::new();
        let mut watch_task_ids = watch_task_ids
            .into_iter()
            .map(|task_id| task_id.trim().to_string())
            .filter(|task_id| !task_id.is_empty())
            .collect::<Vec<_>>();
        watch_task_ids.retain(|task_id| seen.insert(task_id.clone()));
        if watch_task_ids.len() > crate::continuation::MAX_WATCH_TASKS {
            anyhow::bail!(
                "a continuation may watch at most {} tasks",
                crate::continuation::MAX_WATCH_TASKS
            );
        }
        for task_id in &watch_task_ids {
            crate::worktree::validate_task_id(task_id)?;
        }
        let watch_gate_id = watch_gate_id
            .map(str::trim)
            .filter(|gate_id| !gate_id.is_empty())
            .map(str::to_string);
        if let Some(gate_id) = &watch_gate_id {
            if gate_id.chars().count() > 200 || gate_id.chars().any(char::is_control) {
                return Err(crate::gate::gate_error(
                    crate::gate::codes::GATE_INVALID,
                    "gate_id is invalid",
                ));
            }
            let store = self.store.clone();
            let lookup = gate_id.clone();
            let record = tokio::task::spawn_blocking(move || store.get_gate(&lookup))
                .await??
                .ok_or_else(|| {
                    crate::gate::gate_error(
                        crate::gate::codes::GATE_NOT_FOUND,
                        format!("gate {gate_id} does not exist"),
                    )
                })?;
            if record.gate.session_id != session_id {
                return Err(crate::gate::gate_error(
                    crate::gate::codes::GATE_NOT_FOUND,
                    "the gate does not belong to this session",
                ));
            }
            if !record.gate.is_open() {
                return Err(crate::gate::gate_error(
                    crate::gate::codes::GATE_INVALID,
                    format!(
                        "gate {gate_id} is already {}; read its outcome instead of waiting",
                        record.gate.state.as_str()
                    ),
                ));
            }
            let store = self.store.clone();
            let lookup = gate_id.clone();
            if tokio::task::spawn_blocking(move || store.has_open_gate_watch(&lookup)).await?? {
                return Err(crate::gate::gate_error(
                    crate::gate::codes::GATE_ALREADY_WATCHED,
                    format!("gate {gate_id} already has an open continuation watch"),
                ));
            }
        }
        let deadline_at = crate::continuation::resolve_deadline(
            after_secs,
            deadline_at,
            time::OffsetDateTime::now_utc(),
        )?;
        if watch_task_ids.is_empty() && deadline_at.is_none() && watch_gate_id.is_none() {
            anyhow::bail!("a continuation needs a watch task, a deadline, or a gate");
        }

        if !watch_task_ids.is_empty() {
            let plan = self.get_team_plan(session_id).await?;
            let Some(plan) = plan else {
                anyhow::bail!("session has no stored TeamPlan to watch");
            };
            let plan_ids = plan
                .tasks
                .iter()
                .map(|task| task.id.as_str())
                .collect::<std::collections::HashSet<_>>();
            if let Some(missing) = watch_task_ids
                .iter()
                .find(|task_id| !plan_ids.contains(task_id.as_str()))
            {
                anyhow::bail!("task {missing} is not in the stored TeamPlan");
            }
            let store = self.store.clone();
            let id = session_id.to_string();
            let tasks = tokio::task::spawn_blocking(move || store.list_plan_tasks(None, Some(&id)))
                .await??;
            for task_id in &watch_task_ids {
                let Some(task) = tasks
                    .iter()
                    .filter(|task| &task.task_id == task_id)
                    .max_by_key(|task| task.plan_seq)
                else {
                    anyhow::bail!("task {task_id} has no scheduling state yet");
                };
                if crate::continuation::is_terminal_task_status(&task.status) {
                    anyhow::bail!(
                        "task {task_id} is already {}; read its outcome instead of watching it",
                        task.status
                    );
                }
            }
        }

        let store = self.store.clone();
        let session_id = session_id.to_string();
        let project_id = session.project_id.clone();
        let max_attempts = crate::continuation::normalize_max_attempts(max_attempts);
        let gate_for_error = watch_gate_id.clone();
        let created = tokio::task::spawn_blocking(move || {
            store.create_session_continuation_with_event(&NewSessionContinuation {
                session_id,
                project_id,
                body,
                watch_task_ids,
                watch_mode,
                watch_gate_id,
                deadline_at,
                max_attempts,
                series_id: None,
                parent_continuation_id: None,
                recurrence_every_secs: recurrence.every_secs,
                recurrence_max_runs: recurrence.max_runs,
                run_count: 0,
            })
        })
        .await?;
        let (continuation, event) = match created {
            Ok(created) => created,
            Err(error) => {
                if let Some(gate_id) = gate_for_error
                    && format!("{error:#}").contains("UNIQUE constraint failed")
                {
                    return Err(crate::gate::gate_error(
                        crate::gate::codes::GATE_ALREADY_WATCHED,
                        format!("gate {gate_id} already has an open continuation watch"),
                    ));
                }
                return Err(error);
            }
        };
        let _ = self.events.send(event);
        Ok(SessionDeferResult { continuation })
    }

    pub async fn session_continuations(
        &self,
        session_id: &str,
        status: Option<&str>,
        limit: Option<u32>,
    ) -> Result<SessionContinuationsResult> {
        if let Some(status) = status
            && !matches!(
                status,
                "pending" | "ready" | "delivering" | "delivered" | "cancelled" | "failed"
            )
        {
            anyhow::bail!(
                "continuation status must be pending, ready, delivering, delivered, cancelled, or failed"
            );
        }
        let store = self.store.clone();
        let id = session_id.to_string();
        let status = status.map(str::to_string);
        let continuations = tokio::task::spawn_blocking(move || {
            store.list_session_continuations(&id, status.as_deref(), limit)
        })
        .await??;
        Ok(SessionContinuationsResult {
            session_id: session_id.to_string(),
            continuations,
        })
    }

    /// Token-free status for one continuation delivery claim. The scheduler
    /// and operators use it to distinguish a live foreign claim from an
    /// expired row that is safe to reclaim.
    pub async fn session_continuation_status(
        &self,
        id: &str,
    ) -> Result<SessionContinuationStatusResult> {
        let store = self.store.clone();
        let id_owned = id.to_string();
        let continuation =
            tokio::task::spawn_blocking(move || store.get_session_continuation(&id_owned))
                .await??
                .ok_or_else(|| anyhow!("continuation not found: {id}"))?;
        let store = self.store.clone();
        let id_owned = id.to_string();
        let claim =
            tokio::task::spawn_blocking(move || store.session_continuation_claim_status(&id_owned))
                .await??;
        Ok(SessionContinuationStatusResult {
            continuation,
            claim: claim.map(|claim| SessionContinuationClaimInfo {
                status: claim.status,
                owner_id: claim.owner_id,
                expires_at: claim.expires_at,
                delivery_attempt: claim.delivery_attempt,
                live: claim.live,
            }),
        })
    }

    /// Explicitly reclaim an expired delivery claim. This is the same
    /// operation startup recovery and the scheduler use; it cannot reclaim a
    /// live claim or a terminal row.
    pub async fn reclaim_session_continuation(
        &self,
        id: &str,
        reason: Option<&str>,
    ) -> Result<SessionContinuationReclaimResult> {
        let reason = reason
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
            .unwrap_or("claim expired before acknowledgement");
        let reclaimed = self
            .reclaim_expired_session_continuation(id, reason)
            .await?;
        Ok(SessionContinuationReclaimResult {
            reclaimed: reclaimed.is_some(),
            continuation: reclaimed,
        })
    }

    pub async fn cancel_session_continuation(
        &self,
        id: &str,
    ) -> Result<SessionContinuationCancelResult> {
        let store = self.store.clone();
        let continuation_id = id.to_string();
        let reason = "cancelled by operator".to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.cancel_session_continuation_with_event(&continuation_id, &reason)
        })
        .await??;
        let Some((continuation, event)) = result else {
            anyhow::bail!("continuation {id} is not pending or ready");
        };
        let _ = self.events.send(event);
        Ok(SessionContinuationCancelResult {
            cancelled: true,
            continuation,
        })
    }

    /// Transition a pending row to ready and broadcast the event. Scheduler
    /// internals use this; replay reconstructs the same row from the payload.
    pub async fn mark_session_continuation_ready(
        &self,
        id: &str,
        reason: &str,
        outcomes: &[ContinuationOutcome],
    ) -> Result<Option<SessionContinuation>> {
        let store = self.store.clone();
        let id = id.to_string();
        let reason = reason.to_string();
        let outcomes = outcomes.to_vec();
        let result = tokio::task::spawn_blocking(move || {
            store.mark_session_continuation_ready_with_event(&id, &reason, &outcomes)
        })
        .await??;
        let Some((continuation, event)) = result else {
            return Ok(None);
        };
        let _ = self.events.send(event);
        Ok(Some(continuation))
    }

    /// Atomically claim a ready row, or reclaim an expired delivery claim,
    /// with a new process owner and token. The scheduler uses this path and
    /// acknowledges with the returned claim. A live foreign claim returns
    /// `None` and is never touched.
    pub async fn claim_session_continuation_for_delivery(
        &self,
        id: &str,
        ttl_secs: u64,
    ) -> Result<Option<SessionContinuationClaim>> {
        let store = self.store.clone();
        let id = id.to_string();
        let owner_id = self.runtime_id.clone();
        let claim = tokio::task::spawn_blocking(move || {
            store.claim_session_continuation_for_delivery_with_event(&id, &owner_id, ttl_secs)
        })
        .await??;
        let Some(claim) = claim else {
            return Ok(None);
        };
        let _ = self.events.send(claim.event.clone());
        Ok(Some(claim))
    }

    /// Backwards-compatible claim used by tests and older internal callers. It
    /// records the token in the Engine's in-memory claim map so the un-fenced
    /// acknowledgement helpers stay safe within this process. New code should
    /// call `claim_session_continuation_for_delivery` and pass the claim.
    pub async fn claim_session_continuation(
        &self,
        id: &str,
    ) -> Result<Option<SessionContinuation>> {
        let Some(claim) = self
            .claim_session_continuation_for_delivery(id, 120)
            .await?
        else {
            return Ok(None);
        };
        let continuation = claim.continuation.clone();
        if let Ok(mut claims) = self.continuation_claims.lock() {
            claims.insert(id.to_string(), claim);
        }
        Ok(Some(continuation))
    }

    fn take_continuation_claim(&self, id: &str) -> Option<SessionContinuationClaim> {
        self.continuation_claims
            .lock()
            .ok()
            .and_then(|mut claims| claims.remove(id))
    }

    /// Fenced delivery acknowledgement. A stale owner receives a typed
    /// `SessionContinuationAckError::Lost` and no terminal row is written.
    pub async fn ack_session_continuation_delivered(
        &self,
        claim: &SessionContinuationClaim,
    ) -> std::result::Result<Option<SessionContinuation>, SessionContinuationAckError> {
        let store = self.store.clone();
        let id = claim.continuation.id.clone();
        let owner = claim.owner_id.clone();
        let token = claim.token().clone();
        let result = tokio::task::spawn_blocking(move || {
            store.mark_session_continuation_delivered_with_event(&id, &owner, &token)
        })
        .await
        .map_err(|error| SessionContinuationAckError::Internal(error.to_string()))??;
        let Some(delivery) = result else {
            return Ok(None);
        };
        for event in delivery.events {
            let _ = self.events.send(event);
        }
        Ok(Some(delivery.delivered))
    }

    pub async fn ack_session_continuation_retry(
        &self,
        claim: &SessionContinuationClaim,
        error: &str,
        next_attempt_at: &str,
    ) -> std::result::Result<Option<SessionContinuation>, SessionContinuationAckError> {
        let store = self.store.clone();
        let id = claim.continuation.id.clone();
        let owner = claim.owner_id.clone();
        let token = claim.token().clone();
        let error = error.to_string();
        let next_attempt_at = next_attempt_at.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.retry_session_continuation_with_event(
                &id,
                &owner,
                &token,
                &error,
                &next_attempt_at,
            )
        })
        .await
        .map_err(|error| SessionContinuationAckError::Internal(error.to_string()))??;
        let Some((continuation, event)) = result else {
            return Ok(None);
        };
        let _ = self.events.send(event);
        Ok(Some(continuation))
    }

    pub async fn ack_session_continuation_failed(
        &self,
        claim: &SessionContinuationClaim,
        error: &str,
    ) -> std::result::Result<Option<SessionContinuation>, SessionContinuationAckError> {
        let store = self.store.clone();
        let id = claim.continuation.id.clone();
        let owner = claim.owner_id.clone();
        let token = claim.token().clone();
        let error = error.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.fail_session_continuation_with_event(&id, &owner, &token, &error)
        })
        .await
        .map_err(|error| SessionContinuationAckError::Internal(error.to_string()))??;
        let Some((continuation, event)) = result else {
            return Ok(None);
        };
        let _ = self.events.send(event);
        Ok(Some(continuation))
    }

    /// Idempotent legacy acknowledgement. A repeated terminal write or a stale
    /// in-memory claim returns `Ok(None)` instead of rewriting the row.
    pub async fn mark_session_continuation_delivered(
        &self,
        id: &str,
    ) -> Result<Option<SessionContinuation>> {
        let Some(claim) = self.take_continuation_claim(id) else {
            return Ok(None);
        };
        match self.ack_session_continuation_delivered(&claim).await {
            Ok(delivered) => Ok(delivered),
            Err(SessionContinuationAckError::Lost { .. }) => Ok(None),
            Err(error) => Err(anyhow!(error)),
        }
    }

    pub async fn retry_session_continuation(
        &self,
        id: &str,
        error: &str,
        next_attempt_at: &str,
    ) -> Result<Option<SessionContinuation>> {
        let Some(claim) = self.take_continuation_claim(id) else {
            return Ok(None);
        };
        match self
            .ack_session_continuation_retry(&claim, error, next_attempt_at)
            .await
        {
            Ok(retried) => Ok(retried),
            Err(SessionContinuationAckError::Lost { .. }) => Ok(None),
            Err(error) => Err(anyhow!(error)),
        }
    }

    pub async fn fail_session_continuation(
        &self,
        id: &str,
        error: &str,
    ) -> Result<Option<SessionContinuation>> {
        if let Some(claim) = self.take_continuation_claim(id) {
            return match self.ack_session_continuation_failed(&claim, error).await {
                Ok(failed) => Ok(failed),
                Err(SessionContinuationAckError::Lost { .. }) => Ok(None),
                Err(error) => Err(anyhow!(error)),
            };
        }
        self.fail_unclaimed_session_continuation(id, error).await
    }

    /// Fail a pending/ready row, or an expired delivery claim, without a token.
    /// A live foreign claim is left untouched.
    pub async fn fail_unclaimed_session_continuation(
        &self,
        id: &str,
        error: &str,
    ) -> Result<Option<SessionContinuation>> {
        let store = self.store.clone();
        let id = id.to_string();
        let error = error.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.fail_unclaimed_session_continuation_with_event(&id, &error)
        })
        .await??;
        let Some((continuation, event)) = result else {
            return Ok(None);
        };
        let _ = self.events.send(event);
        Ok(Some(continuation))
    }

    /// Reclaim one expired claim at startup or from an operator drill. The
    /// delivery operation receipt makes a retry safe when the old process may
    /// already have started a run.
    pub async fn reclaim_expired_session_continuation(
        &self,
        id: &str,
        reason: &str,
    ) -> Result<Option<SessionContinuation>> {
        let store = self.store.clone();
        let id = id.to_string();
        let reason = reason.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.reclaim_expired_session_continuation_with_event(&id, &reason)
        })
        .await??;
        let Some((continuation, event)) = result else {
            return Ok(None);
        };
        let _ = self.events.send(event);
        Ok(Some(continuation))
    }

    pub async fn cancel_session_continuation_with_reason(
        &self,
        id: &str,
        reason: &str,
    ) -> Result<Option<SessionContinuation>> {
        let store = self.store.clone();
        let id = id.to_string();
        let reason = reason.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.cancel_session_continuation_with_event(&id, &reason)
        })
        .await??;
        let Some((continuation, event)) = result else {
            return Ok(None);
        };
        let _ = self.events.send(event);
        Ok(Some(continuation))
    }

    /// Drain pending steering for the current turn. Called by the agent loop
    /// before the next provider request; top-level runs also persist a visible
    /// user message while child runs keep the injected turn in memory and the
    /// event log.
    pub async fn deliver_pending_steering(
        &self,
        session_id: &str,
        agent_id: &str,
        task_id: Option<&str>,
        persist_messages: bool,
    ) -> Result<Vec<DeliveredSteering>> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let agent_id = agent_id.to_string();
        let task_id = task_id.map(str::to_string);
        let (delivered, events) = tokio::task::spawn_blocking(move || {
            store.deliver_pending_steering_with_events(
                &session_id,
                &agent_id,
                task_id.as_deref(),
                persist_messages,
            )
        })
        .await??;
        for event in events {
            let _ = self.events.send(event);
        }
        Ok(delivered)
    }

    /// Bounded turn/tool progress observation for a task-scoped run.
    pub async fn touch_task_progress(
        &self,
        session_id: &str,
        task_id: Option<&str>,
        turn: u32,
        tool: Option<&str>,
    ) -> Result<bool> {
        let Some(task_id) = task_id else {
            return Ok(false);
        };
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let task_id = task_id.to_string();
        let tool = tool.map(str::to_string);
        tokio::task::spawn_blocking(move || {
            store.touch_plan_task_progress(&session_id, &task_id, turn, tool.as_deref())
        })
        .await?
    }

    /// Terminal-run cleanup: cancel anything still pending for this agent.
    pub async fn cancel_pending_steering_for_agent(
        &self,
        session_id: &str,
        agent_id: &str,
        reason: &str,
    ) -> Result<usize> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let agent_id = agent_id.to_string();
        let reason = reason.to_string();
        let events = tokio::task::spawn_blocking(move || {
            store.cancel_pending_steering_for_agent_with_events(&session_id, &agent_id, &reason)
        })
        .await??;
        let count = events.len();
        for event in events {
            let _ = self.events.send(event);
        }
        Ok(count)
    }

    /// Run one stall sweep. Observation is always recorded; when the project
    /// policy opts into `escalate` or `retry`, the same fresh observations may
    /// advance a bounded recovery. Zero disables the sweep and recovery.
    pub async fn sweep_task_stalls(&self, threshold: std::time::Duration) -> Result<usize> {
        self.sweep_task_stalls_at(threshold, time::OffsetDateTime::now_utc())
            .await
    }

    /// Deterministic-time variant used by tests and replay repairs.
    pub async fn sweep_task_stalls_at(
        &self,
        threshold: std::time::Duration,
        now: time::OffsetDateTime,
    ) -> Result<usize> {
        if threshold.is_zero() {
            return Ok(0);
        }
        let store = self.store.clone();
        let (stalls, events) = tokio::task::spawn_blocking(move || {
            store.sweep_task_stalls_with_events_at(threshold, now)
        })
        .await??;
        for event in events {
            self.broadcast_event(event);
        }
        self.evaluate_stall_recoveries(&stalls).await?;
        Ok(stalls.len())
    }

    async fn evaluate_stall_recoveries(&self, stalls: &[TaskStall]) -> Result<()> {
        for stall in stalls {
            let policy = self.orchestration_policy(&stall.project_id).await?;
            let store = self.store.clone();
            let session_id = stall.session_id.clone();
            let plan_seq = stall.plan_seq;
            let task_id = stall.task_id.clone();
            let recovery = tokio::task::spawn_blocking(move || {
                store.get_orchestration_recovery(&session_id, plan_seq, &task_id)
            })
            .await??;
            let decision =
                crate::orchestration::decide_stall_recovery(&policy, recovery.as_ref(), stall);
            match decision {
                crate::orchestration::StallRecoveryDecision::None => {}
                crate::orchestration::StallRecoveryDecision::Escalate => {
                    let reason = format!(
                        "{} produced {} stall observation(s); latest observation was {}s old",
                        stall.task_id, stall.stall_count, stall.age_secs
                    );
                    let session_id = stall.session_id.clone();
                    let task_id = stall.task_id.clone();
                    let store = self.store.clone();
                    let owned_stall = stall.clone();
                    let reason_for_store = reason.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        store.escalate_stall_recovery(&owned_stall, &reason_for_store)
                    })
                    .await??;
                    let Some((_recovery, event)) = result else {
                        continue;
                    };
                    self.broadcast_event(event);
                    self.record_stall_escalation(&session_id, &task_id, &reason)
                        .await?;
                }
                crate::orchestration::StallRecoveryDecision::Retry => {
                    self.request_stall_recovery_retry(stall, &policy).await?;
                }
                crate::orchestration::StallRecoveryDecision::Exhausted { reason } => {
                    self.record_stall_recovery_failure(stall, &reason).await?;
                }
            }
        }
        Ok(())
    }

    async fn request_stall_recovery_retry(
        &self,
        stall: &TaskStall,
        policy: &OrchestrationPolicy,
    ) -> Result<()> {
        let store = self.store.clone();
        let session_id = stall.session_id.clone();
        let session = tokio::task::spawn_blocking(move || store.get_session(&session_id))
            .await??
            .ok_or_else(|| anyhow!("session not found: {}", stall.session_id))?;
        if session_budget_exhausted(&session) {
            self.record_stall_recovery_failure(
                stall,
                "session budget is exhausted; automatic stall recovery did not start",
            )
            .await?;
            return Ok(());
        }
        let reason = format!(
            "{} produced {} stall observation(s); latest observation was {}s old",
            stall.task_id, stall.stall_count, stall.age_secs
        );
        let store = self.store.clone();
        let owned = stall.clone();
        let reason_for_store = reason.clone();
        let max_attempts = policy.stall_max_attempts;
        let grace_secs = policy.stall_grace_secs;
        let requested = tokio::task::spawn_blocking(move || {
            store.begin_stall_retry_recovery(&owned, max_attempts, grace_secs, &reason_for_store)
        })
        .await??;
        let Some((_recovery, event)) = requested else {
            return Ok(());
        };
        self.broadcast_event(event);
        match self
            .request_plan_task_cancel(&stall.session_id, &stall.task_id)
            .await
        {
            Ok(_) => {
                self.finish_stall_recovery_cancel(
                    stall,
                    "cancellation requested; waiting for the task and session to settle",
                )
                .await?;
            }
            Err(error) => {
                let store = self.store.clone();
                let session_id = stall.session_id.clone();
                let task_id = stall.task_id.clone();
                let status = tokio::task::spawn_blocking(move || {
                    store.plan_task_status(&session_id, &task_id)
                })
                .await??;
                match status.as_deref() {
                    Some("completed" | "skipped") | None => {
                        self.finish_stall_recovery_unnecessary(
                            stall,
                            "task completed before the cancellation request took effect",
                        )
                        .await?;
                    }
                    Some("cancelled" | "interrupted" | "failed" | "cancelling") => {
                        self.finish_stall_recovery_cancel(
                            stall,
                            "the task ended before cancellation was delivered; waiting for the session to settle",
                        )
                        .await?;
                    }
                    Some(_) => {
                        self.record_stall_recovery_failure(
                            stall,
                            &format!("cooperative cancellation failed: {error:#}"),
                        )
                        .await?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn finish_stall_recovery_cancel(&self, stall: &TaskStall, reason: &str) -> Result<()> {
        let store = self.store.clone();
        let session_id = stall.session_id.clone();
        let task_id = stall.task_id.clone();
        let plan_seq = stall.plan_seq;
        let reason = reason.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.mark_stall_recovery_waiting_cancel(&session_id, plan_seq, &task_id, &reason)
        })
        .await??;
        if let Some((_recovery, event)) = result {
            self.broadcast_event(event);
        }
        Ok(())
    }

    async fn finish_stall_recovery_unnecessary(
        &self,
        stall: &TaskStall,
        reason: &str,
    ) -> Result<()> {
        let store = self.store.clone();
        let session_id = stall.session_id.clone();
        let task_id = stall.task_id.clone();
        let plan_seq = stall.plan_seq;
        let reason = reason.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.finish_stall_recovery_unnecessary(&session_id, plan_seq, &task_id, &reason)
        })
        .await??;
        if let Some((_recovery, event)) = result {
            self.broadcast_event(event);
        }
        Ok(())
    }

    pub(crate) async fn record_stall_recovery_failure(
        &self,
        stall: &TaskStall,
        reason: &str,
    ) -> Result<()> {
        let store = self.store.clone();
        let owned = stall.clone();
        let reason_for_store = reason.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.fail_stall_recovery_for_stall(&owned, &reason_for_store)
        })
        .await??;
        if let Some((_recovery, event)) = result {
            self.broadcast_event(event);
            self.record_stall_escalation(&stall.session_id, &stall.task_id, reason)
                .await?;
        }
        Ok(())
    }

    pub(crate) async fn record_stall_escalation(
        &self,
        session_id: &str,
        task_id: &str,
        reason: &str,
    ) -> Result<()> {
        self.record_coordination_note(
            session_id,
            "escalate",
            "critical",
            Some(task_id),
            None,
            None,
            "orchestrator",
            "Orchestrator",
            Some("orchestrator"),
            &format!("Automatic stall recovery escalation for {task_id}: {reason}"),
        )
        .await?;
        Ok(())
    }

    pub async fn orchestration_recoveries(
        &self,
        session_id: &str,
    ) -> Result<OrchestrationRecoveriesResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let recoveries = tokio::task::spawn_blocking(move || {
            store.list_current_orchestration_recoveries(None, Some(&id))
        })
        .await??;
        Ok(OrchestrationRecoveriesResult {
            session_id: session_id.to_string(),
            recoveries,
        })
    }

    pub async fn session_stalls(&self, session_id: &str) -> Result<SessionStallsResult> {
        let threshold_secs = task_stall_threshold_secs();
        let store = self.store.clone();
        let id = session_id.to_string();
        let progress =
            tokio::task::spawn_blocking(move || store.list_plan_task_progress(None, Some(&id)))
                .await??;
        let events = self.recent_events(session_id, 5_000).await?;
        let mut stalls = events
            .into_iter()
            .filter(|event| event.kind == "orchestration.stall")
            .filter_map(|event| serde_json::from_value::<TaskStall>(event.payload).ok())
            .collect::<Vec<_>>();
        stalls.truncate(200);
        Ok(SessionStallsResult {
            session_id: session_id.to_string(),
            threshold_secs,
            progress,
            stalls,
        })
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

    pub async fn current_plan_revision(&self, session_id: &str) -> Result<u32> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.plan_revision_count(&session_id)).await?
    }

    pub async fn session_runs(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<vp_protocol::types::SessionRunsResult> {
        let runs = crate::run_journal::session_runs(self, session_id, limit).await?;
        Ok(vp_protocol::types::SessionRunsResult {
            session_id: session_id.to_string(),
            runs,
        })
    }

    pub async fn session_run_detail(
        &self,
        run_id: &str,
    ) -> Result<vp_protocol::types::SessionRunDetailResult> {
        let (run, steps, tool_calls, checkpoints) =
            crate::run_journal::run_detail(self, run_id).await?;
        Ok(vp_protocol::types::SessionRunDetailResult {
            run,
            steps,
            tool_calls,
            checkpoints,
        })
    }

    pub async fn session_run_policy(
        &self,
        project_id: &str,
    ) -> Result<vp_protocol::types::SessionRunPolicyResult> {
        let policy = crate::run_journal::policy(self, project_id).await?;
        Ok(vp_protocol::types::SessionRunPolicyResult {
            project_id: project_id.to_string(),
            policy,
        })
    }

    pub async fn set_session_run_policy(
        &self,
        project_id: &str,
        policy: vp_protocol::types::SessionRunPolicy,
    ) -> Result<vp_protocol::types::SessionRunPolicyResult> {
        let policy = crate::run_journal::set_policy(self, project_id, &policy).await?;
        Ok(vp_protocol::types::SessionRunPolicyResult {
            project_id: project_id.to_string(),
            policy,
        })
    }

    /// Reconstruct the top-level conversation from the durable event stream,
    /// not from a live in-memory vector. Malformed message events fail closed.
    pub async fn conversation_from_events(
        &self,
        session_id: &str,
    ) -> Result<(Vec<vp_providers::ChatMessage>, Vec<Option<String>>)> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let events = tokio::task::spawn_blocking(move || store.list_all_events(&id)).await??;
        crate::agent::conversation_from_events(&events, session_id)
    }

    pub async fn recover_interrupted_sessions(&self) -> Result<usize> {
        let store = self.store.clone();
        let (sessions, events) =
            tokio::task::spawn_blocking(move || store.recover_state()).await??;
        for event in events {
            let _ = self.events.send(event);
        }
        // Durable run journal recovery runs after session/agent status changes:
        // every run left `running` by the previous process becomes
        // `interrupted`, and its tool intents become unknown.
        let interrupted_runs = crate::run_journal::interrupt_running_runs(self).await?;
        if interrupted_runs > 0 {
            tracing::warn!(
                runs = interrupted_runs,
                "marked durable runs interrupted after daemon recovery"
            );
        }
        // An operation receipt that is still `pending` belongs to the previous
        // daemon process. Its side effect may or may not have landed, so it is
        // failed closed as `unknown` instead of being replayed.
        let operation_store = self.store.clone();
        let unknown_operations = tokio::task::spawn_blocking(move || {
            operation_store.mark_pending_session_operations_unknown()
        })
        .await??;
        if unknown_operations > 0 {
            tracing::warn!(
                operations = unknown_operations,
                "marked pending session operations unknown after daemon recovery"
            );
        }
        // A crash may land after `git merge` but before the queue status is
        // finalized. If the session worktree is clean, reset it to the
        // recorded pre-merge HEAD so retry starts from a known state.
        let recovery_store = self.store.clone();
        let recoveries =
            tokio::task::spawn_blocking(move || recovery_store.interrupted_merge_recoveries())
                .await??;
        let git_lock = self.git_lock.clone();
        for entry in recoveries {
            let Some(pre_merge) = entry.pre_merge_commit.clone() else {
                continue;
            };
            let session_store = self.store.clone();
            let session_id = entry.session_id.clone();
            let session =
                tokio::task::spawn_blocking(move || session_store.get_session(&session_id))
                    .await??;
            let Some(session) = session else {
                continue;
            };
            let Some(worktree_path) = session.worktree.clone() else {
                continue;
            };
            let path = PathBuf::from(worktree_path);
            let reset = {
                let git_lock = git_lock.clone();
                let path = path.clone();
                let pre_merge = pre_merge.clone();
                tokio::task::spawn_blocking(move || -> Result<()> {
                    let _guard = git_lock
                        .lock()
                        .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
                    if !worktree::is_clean(&path)? {
                        anyhow::bail!("session worktree is not clean");
                    }
                    worktree::reset_hard(&path, &pre_merge)
                })
                .await?
            };
            let (kind, payload) = match reset {
                Ok(()) => (
                    "merge_queue.recovered",
                    serde_json::json!({
                        "entry_id": entry.id,
                        "pre_merge_commit": pre_merge,
                        "detail": "reset clean session worktree to the pre-merge HEAD",
                    }),
                ),
                Err(error) => (
                    "merge_queue.recovery_skipped",
                    serde_json::json!({
                        "entry_id": entry.id,
                        "pre_merge_commit": pre_merge,
                        "detail": format!("{error:#}"),
                    }),
                ),
            };
            if let Err(error) = self.append_event(&session.id, kind, payload).await {
                tracing::warn!(%error, entry_id = %entry.id, "failed to record merge recovery");
            }
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
        ensure_session_quiescent(&session, "discard the session worktree")?;
        let worktree = session
            .worktree
            .clone()
            .ok_or_else(|| anyhow::anyhow!("session has no worktree"))?;
        self.stop_session_processes(&session.id, "session discard")
            .await;
        let worktree = PathBuf::from(worktree);
        let git_lock = self.git_lock.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = git_lock
                .lock()
                .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
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
        ensure_session_quiescent(&session, "commit the session worktree")?;
        let worktree = session
            .worktree
            .clone()
            .ok_or_else(|| anyhow::anyhow!("session has no worktree"))?;
        let session_id = session.id.clone();
        let worktree = PathBuf::from(worktree);
        let message_for_git = message.clone();
        let git_lock = self.git_lock.clone();
        let (commit, summary) = tokio::task::spawn_blocking(move || {
            let _guard = git_lock
                .lock()
                .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
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

        let commit_target = crate::effect::EffectTarget::new(
            crate::effect::kinds::GIT_COMMIT,
            session
                .branch
                .clone()
                .unwrap_or_else(|| "session".to_string()),
        );
        let commit_request = crate::effect::canonical_request_hash(
            "session.commit",
            serde_json::json!({
                "branch": session.branch.clone(),
                "commit": commit.clone(),
                "message_hash": crate::sha256_hex(message.as_bytes()),
            }),
        )?;
        let mut commit_effect = crate::effect::runtime_receipt(
            &session_id,
            None,
            "session.commit",
            commit_target,
            commit_request,
            Some("workspace".to_string()),
        );
        commit_effect.state = EffectState::Done;
        commit_effect.external_id = Some(commit.clone());
        commit_effect.evidence = vp_protocol::types::EffectEvidence {
            probe: crate::effect::probes::GIT_REV_PARSE.to_string(),
            outcome: "observed".to_string(),
            commit: Some(commit.clone()),
            branch: session.branch.clone(),
            ..Default::default()
        };
        self.record_settled_effect(commit_effect).await?;
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
        ensure_session_quiescent(&session, "land the session")?;
        if session.status == "landed" {
            anyhow::bail!("session is already landed");
        }
        self.assert_effects_settled(&session.id, None).await?;
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
        let head_worktree = session_worktree.clone();
        let session_head =
            tokio::task::spawn_blocking(move || worktree::head_commit(&head_worktree)).await??;
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
        let effect_target = crate::effect::EffectTarget::new(
            crate::effect::kinds::GIT_MERGE,
            format!("land:{target_branch}:{session_branch}"),
        );
        let effect_request = crate::effect::canonical_request_hash(
            "session.land",
            serde_json::json!({
                "branch": session_branch.clone(),
                "target_branch": target_branch.clone(),
                "head": session_head.clone(),
            }),
        )?;
        let existing = self
            .lookup_effect(
                &session.id,
                None,
                "session.land",
                &effect_target.canonical(),
                &effect_request,
            )
            .await?;
        if let Some(receipt) = existing.as_ref()
            && receipt.state == EffectState::Done
        {
            let commit = receipt
                .external_id
                .clone()
                .or_else(|| receipt.evidence.commit.clone())
                .unwrap_or_else(|| session_head.clone());
            return Ok(SessionLandResult {
                session_id: session.id,
                branch: session_branch,
                target_branch,
                commit,
                detail:
                    "already landed; the stored effect receipt was replayed without merging again"
                        .to_string(),
            });
        }
        let intent = crate::effect::runtime_receipt(
            &session.id,
            None,
            "session.land",
            effect_target,
            effect_request,
            Some("workspace".to_string()),
        );
        let (intent, inserted) = self.begin_effect(intent).await?;
        if !inserted && intent.state != EffectState::Intent {
            return Err(anyhow::Error::new(crate::effect::EffectBlocked {
                session_id: session.id.clone(),
                task_id: None,
                receipt_ids: vec![intent.id.clone()],
                message: format!(
                    "EFFECT_UNKNOWN: landing is blocked by effect receipt {} ({})",
                    intent.id,
                    intent.state.as_str()
                ),
            }));
        }
        let git_lock = self.git_lock.clone();
        let merge = tokio::task::spawn_blocking(move || -> Result<(String, String)> {
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
            Ok((
                git_output(&project_root, &["rev-parse", "--short", "HEAD"])?,
                worktree::head_commit(&project_root)?,
            ))
        })
        .await?;
        let (commit, full_commit) = match merge {
            Ok(value) => value,
            Err(error) => {
                let mut failed = intent.clone();
                failed.state = EffectState::Failed;
                failed.evidence = vp_protocol::types::EffectEvidence {
                    probe: "git_merge_head".to_string(),
                    outcome: "mismatch".to_string(),
                    reason: Some("landing_merge_failed".to_string()),
                    branch: Some(session_branch.clone()),
                    ..Default::default()
                };
                let _ = self.settle_effect(failed).await;
                return Err(error);
            }
        };
        let mut settled = intent;
        settled.state = EffectState::Done;
        settled.external_id = Some(commit.clone());
        settled.response_hash = Some(crate::sha256_hex(full_commit.as_bytes()));
        settled.evidence = vp_protocol::types::EffectEvidence {
            probe: "git_merge_head".to_string(),
            outcome: "observed".to_string(),
            commit: Some(full_commit),
            branch: Some(session_branch.clone()),
            ..Default::default()
        };
        self.settle_effect(settled).await?;

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
        self.open_pull_request_with_provider(session_id, None).await
    }

    /// Open a pull request with an optional explicit provider. Explicit input
    /// wins over `VERLINPARK_PR_PROVIDER`, which wins over origin detection.
    pub async fn open_pull_request_with_provider(
        &self,
        session_id: &str,
        provider: Option<&str>,
    ) -> Result<PullRequestResult> {
        self.reject_imported_session(session_id).await?;
        let origin = self.pull_request_origin(session_id).await?;
        let environment = std::env::var("VERLINPARK_PR_PROVIDER").ok();
        let target = crate::forge::resolve_target(provider, environment.as_deref(), &origin)?;
        let bitbucket = if target.provider == PrCli::BitbucketCloud {
            Some(crate::forge::BitbucketCloudProvider::from_env(
                crate::forge::BitbucketCloudConfig::default(),
            )?)
        } else {
            None
        };
        self.open_pull_request_with_target(session_id, target, bitbucket, None)
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

    /// Testable CLI entry point. The fake CLI path is explicit while the
    /// provider preflight/push/create sequence remains the production one.
    #[cfg(test)]
    async fn open_pull_request_with_cli(
        &self,
        session_id: &str,
        cli_bin: &str,
        kind: PrCli,
    ) -> Result<PullRequestResult> {
        let target = crate::forge::ForgeTarget {
            provider: kind,
            origin: None,
            selected_by: crate::forge::ForgeSelectionSource::Explicit,
        };
        self.open_pull_request_with_target(session_id, target, None, Some(cli_bin.to_string()))
            .await
    }

    pub(crate) async fn open_pull_request_with_target(
        &self,
        session_id: &str,
        target: crate::forge::ForgeTarget,
        bitbucket: Option<crate::forge::BitbucketCloudProvider>,
        cli_binary: Option<String>,
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
        let session_worktree_for_status = session_worktree.clone();
        let status = tokio::task::spawn_blocking(move || {
            git_output(&session_worktree_for_status, &["status", "--porcelain"])
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
        let title = session
            .goal
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("VerlinPark change")
            .chars()
            .take(120)
            .collect::<String>();
        let description = format!(
            "Opened from VerlinPark session {}.\n\nGoal: {}",
            session.id, session.goal
        );
        let request = crate::forge::ForgeChangeRequest::new(
            title,
            description,
            branch.clone(),
            base_branch.clone(),
        );

        // Local safety checks happen before any forge network preflight so a
        // branch with no commits, or a dirty worktree, never causes a push or a
        // provider call.
        let project_root_for_check = project_root.clone();
        let branch_for_check = branch.clone();
        let base_for_check = base_branch.clone();
        let git_lock = self.git_lock.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let _guard = git_lock
                .lock()
                .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
            git_output(&project_root_for_check, &["remote", "get-url", "origin"])
                .context("a git origin remote is required to open a PR")?;
            let ahead = git_output(
                &project_root_for_check,
                &[
                    "rev-list",
                    "--count",
                    &format!("{base_for_check}..{branch_for_check}"),
                ],
            )?;
            if ahead.trim() == "0" {
                anyhow::bail!("session branch has no commits ahead of {base_for_check}");
            }
            Ok(())
        })
        .await??;

        let provider: Box<dyn crate::forge::ForgeProvider> = match target.provider {
            PrCli::BitbucketCloud => Box::new(bitbucket.ok_or_else(|| {
                anyhow::anyhow!("the Bitbucket Cloud client was not initialized")
            })?),
            kind => {
                let binary = match cli_binary {
                    Some(binary) => binary,
                    None => kind.resolve_cli().ok_or_else(|| {
                        anyhow::anyhow!("no CLI configured for the {kind} provider")
                    })?,
                };
                // Gitea/Forgejo run from the session worktree where the branch
                // is checked out; GitHub/GitLab keep the project-root cwd.
                let create_cwd = if kind == PrCli::Gitea {
                    session_worktree.clone()
                } else {
                    project_root.clone()
                };
                Box::new(crate::forge::CliForgeProvider::new(
                    kind,
                    binary,
                    project_root.clone(),
                    create_cwd,
                ))
            }
        };

        // Preflight (credential/repository check) happens before the push where
        // practical, so missing Bitbucket credentials or an inaccessible repo
        // never leaves a pushed branch behind.
        let preflight = provider.preflight(&target).await?;
        let repository = preflight.repository.clone().or_else(|| target.repository());
        let provider_id = target.provider.id().to_string();
        let repository_key = repository
            .clone()
            .unwrap_or_else(|| format!("origin:{branch}"));
        let head_worktree = project_root.clone();
        let head_branch = branch.clone();
        let head_commit = tokio::task::spawn_blocking(move || {
            git_output(&head_worktree, &["rev-parse", &head_branch])
        })
        .await??;

        // --- git push effect -------------------------------------------------
        let push_target = crate::effect::EffectTarget::new(
            crate::effect::kinds::GIT_PUSH,
            format!("{provider_id}:{repository_key}:{branch}"),
        );
        let push_request = crate::effect::canonical_request_hash(
            "session.pr.push",
            serde_json::json!({
                "provider": provider_id.clone(),
                "repository": repository_key.clone(),
                "branch": branch.clone(),
                "commit": head_commit.clone(),
            }),
        )?;
        let existing_push = self
            .lookup_effect(
                &session.id,
                None,
                "session.pr.push",
                &push_target.canonical(),
                &push_request,
            )
            .await?;
        let push_done = matches!(
            existing_push.as_ref().map(|receipt| receipt.state),
            Some(EffectState::Done)
        );
        if let Some(receipt) = existing_push.as_ref()
            && matches!(receipt.state, EffectState::Intent | EffectState::Unknown)
        {
            return Err(anyhow::Error::new(crate::effect::EffectBlocked {
                session_id: session.id.clone(),
                task_id: None,
                receipt_ids: vec![receipt.id.clone()],
                message: format!(
                    "EFFECT_UNKNOWN: a previous push of {branch} is {}; reconcile it before pushing again",
                    receipt.state.as_str()
                ),
            }));
        }
        let mut push_intent = None;
        if !push_done {
            let intent = crate::effect::runtime_receipt(
                &session.id,
                None,
                "session.pr.push",
                push_target,
                push_request,
                Some(provider_id.clone()),
            );
            let (intent, inserted) = self.begin_effect(intent).await?;
            if !inserted && intent.state != EffectState::Intent {
                return Err(anyhow::Error::new(crate::effect::EffectBlocked {
                    session_id: session.id.clone(),
                    task_id: None,
                    receipt_ids: vec![intent.id.clone()],
                    message: format!(
                        "EFFECT_UNKNOWN: a previous push of {branch} is {}; reconcile it before pushing again",
                        intent.state.as_str()
                    ),
                }));
            }
            push_intent = Some(intent);
        }

        let project_root_for_push = project_root.clone();
        let branch_for_push = branch.clone();
        let base_for_push_check = base_branch.clone();
        let git_lock = self.git_lock.clone();
        let push_result = tokio::task::spawn_blocking(move || -> Result<()> {
            let _guard = git_lock
                .lock()
                .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
            git_output(&project_root_for_push, &["remote", "get-url", "origin"])
                .context("a git origin remote is required to open a PR")?;
            // Re-check after preflight (which may wait on the network) so a
            // concurrently reset branch never gets pushed as an empty update.
            let ahead = git_output(
                &project_root_for_push,
                &[
                    "rev-list",
                    "--count",
                    &format!("{base_for_push_check}..{branch_for_push}"),
                ],
            )?;
            if ahead.trim() == "0" {
                anyhow::bail!("session branch has no commits ahead of {base_for_push_check}");
            }
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(&project_root_for_push)
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
        .await?;
        if let Some(intent) = push_intent {
            let mut settled = intent;
            match &push_result {
                Ok(()) => {
                    settled.state = EffectState::Done;
                    settled.external_id = Some(head_commit.clone());
                    settled.evidence = vp_protocol::types::EffectEvidence {
                        probe: crate::effect::probes::GIT_LS_REMOTE.to_string(),
                        outcome: "declared".to_string(),
                        branch: Some(branch.clone()),
                        commit: Some(head_commit.clone()),
                        note: Some(
                            "the push command completed; reconcile if the remote head is disputed"
                                .to_string(),
                        ),
                        ..Default::default()
                    };
                }
                Err(error) => {
                    let unknown = push_error_is_ambiguous(error);
                    settled.state = if unknown {
                        EffectState::Unknown
                    } else {
                        EffectState::Failed
                    };
                    settled.evidence = vp_protocol::types::EffectEvidence {
                        probe: crate::effect::probes::GIT_LS_REMOTE.to_string(),
                        outcome: if unknown { "unverifiable" } else { "mismatch" }.to_string(),
                        reason: Some(if unknown {
                            crate::effect::reasons::TRANSPORT_UNKNOWN.to_string()
                        } else {
                            "git_push_failed".to_string()
                        }),
                        branch: Some(branch.clone()),
                        ..Default::default()
                    };
                }
            }
            self.settle_effect(settled).await?;
        }
        push_result?;

        self.append_event(
            &session.id,
            if push_done {
                "session.push_replayed"
            } else {
                "session.pushed"
            },
            serde_json::json!({
                "branch": branch.clone(),
                "base_branch": base_branch.clone(),
                "provider": provider_id.clone(),
                "provider_selected_by": selection_source_id(target.selected_by),
                "repository": repository.clone(),
                "effect_receipt_id": existing_push.as_ref().map(|receipt| receipt.id.clone()),
                "effect_replayed": push_done,
            }),
        )
        .await?;

        // --- forge PR effect -------------------------------------------------
        let pr_target = crate::effect::EffectTarget::new(
            crate::effect::kinds::FORGE_PR,
            format!("{provider_id}/{repository_key}:{branch}->{base_branch}"),
        );
        let pr_request = crate::effect::canonical_request_hash(
            "session.pr.create",
            serde_json::json!({
                "provider": provider_id.clone(),
                "repository": repository_key.clone(),
                "branch": branch.clone(),
                "base_branch": base_branch.clone(),
                "commit": head_commit.clone(),
            }),
        )?;
        let existing_pr = self
            .lookup_effect(
                &session.id,
                None,
                "session.pr.create",
                &pr_target.canonical(),
                &pr_request,
            )
            .await?;
        if let Some(receipt) = existing_pr.as_ref() {
            match receipt.state {
                EffectState::Done => {
                    let url = receipt
                        .external_id
                        .clone()
                        .filter(|value| !value.is_empty());
                    return Ok(PullRequestResult {
                        session_id: session.id,
                        branch,
                        base_branch,
                        pushed: true,
                        url: url.clone(),
                        detail: format!(
                            "the stored effect receipt for this branch was replayed instead of opening a duplicate {}",
                            receipt.evidence.change.as_deref().unwrap_or("change")
                        ),
                        provider: Some(provider_id),
                        repository: receipt.evidence.repository.clone().or(repository),
                        change: receipt.evidence.change.clone(),
                    });
                }
                EffectState::Intent | EffectState::Unknown => {
                    return Err(anyhow::Error::new(crate::effect::EffectBlocked {
                        session_id: session.id.clone(),
                        task_id: None,
                        receipt_ids: vec![receipt.id.clone()],
                        message: format!(
                            "EFFECT_UNKNOWN: a previous PR/MR creation for {branch} is {}; reconcile it before trying again",
                            receipt.state.as_str()
                        ),
                    }));
                }
                EffectState::Failed => {}
            }
        }
        let pr_intent = crate::effect::runtime_receipt(
            &session.id,
            None,
            "session.pr.create",
            pr_target,
            pr_request,
            Some(provider_id.clone()),
        );
        let (pr_intent, inserted) = self.begin_effect(pr_intent).await?;
        if !inserted && pr_intent.state != EffectState::Intent {
            return Err(anyhow::Error::new(crate::effect::EffectBlocked {
                session_id: session.id.clone(),
                task_id: None,
                receipt_ids: vec![pr_intent.id.clone()],
                message: format!(
                    "EFFECT_UNKNOWN: a previous PR/MR creation for {branch} is {}; reconcile it before trying again",
                    pr_intent.state.as_str()
                ),
            }));
        }
        let change = match provider.create(&target, &request).await {
            Ok(change) => {
                let mut settled = pr_intent;
                settled.state = EffectState::Done;
                settled.external_id = Some(change.url.clone());
                settled.evidence = vp_protocol::types::EffectEvidence {
                    probe: crate::effect::probes::FORGE_RESPONSE.to_string(),
                    outcome: "observed".to_string(),
                    branch: Some(branch.clone()),
                    commit: Some(head_commit.clone()),
                    external_id: Some(change.url.clone()),
                    change: Some(change.change_name.to_string()),
                    repository: change.repository.clone().or_else(|| repository.clone()),
                    caveat: Some(
                        "the forge response proves the local client observed a change; it is not a provider exactly-once guarantee"
                            .to_string(),
                    ),
                    ..Default::default()
                };
                self.settle_effect(settled).await?;
                change
            }
            Err(error) => {
                let unknown = forge_error_is_ambiguous(&error);
                let mut settled = pr_intent;
                settled.state = if unknown {
                    EffectState::Unknown
                } else {
                    EffectState::Failed
                };
                settled.evidence = vp_protocol::types::EffectEvidence {
                    probe: crate::effect::probes::FORGE_RESPONSE.to_string(),
                    outcome: if unknown { "unverifiable" } else { "mismatch" }.to_string(),
                    reason: Some(if unknown {
                        crate::effect::reasons::TRANSPORT_UNKNOWN.to_string()
                    } else {
                        "forge_create_failed".to_string()
                    }),
                    branch: Some(branch.clone()),
                    commit: Some(head_commit.clone()),
                    repository: repository.clone(),
                    ..Default::default()
                };
                self.settle_effect(settled).await?;
                // The branch is already on the remote at this point. Make the
                // failure detail honest without claiming the PR exists.
                return Err(anyhow::Error::new(
                    error.prefix_message("branch pushed, but "),
                ));
            }
        };

        let mut event = serde_json::json!({
            "branch": branch,
            "base_branch": base_branch,
            "url": change.url,
            "provider": change.provider.id(),
            "repository": change.repository,
            "change": change.change_name,
        });
        if let Some(id) = change.id {
            event["change_id"] = serde_json::json!(id);
        }
        self.append_event(&session.id, "session.pr_opened", event)
            .await?;
        Ok(PullRequestResult {
            session_id: session.id,
            branch,
            base_branch,
            pushed: true,
            url: Some(change.url.clone()),
            detail: format!(
                "branch pushed and {} opened at {} via {}",
                change.change_name,
                change.url,
                change.provider.display_name()
            ),
            provider: Some(change.provider.id().to_string()),
            repository: change.repository,
            change: Some(change.change_name.to_string()),
        })
    }

    /// Read-only forge inventory for CLI/Settings. It does not spawn a CLI or
    /// make a provider request, and it never returns credential values.
    pub async fn forge_status(&self, project_id: Option<String>) -> Result<ForgeStatusResult> {
        let provider_override = std::env::var("VERLINPARK_PR_PROVIDER")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .map(|value| crate::forge::sanitize_status_text(&value, 64));
        let parsed_override = provider_override.as_deref().map(PrCli::parse).transpose();
        let override_error = parsed_override
            .as_ref()
            .err()
            .map(|error| error.message.clone());
        let override_kind = parsed_override.ok().flatten();

        let mut detected_provider = PrCli::GitHub;
        let mut origin_status = None;
        if let Some(project_id) = project_id {
            let store = self.store.clone();
            let project = tokio::task::spawn_blocking(move || store.get_project(&project_id))
                .await??
                .ok_or_else(|| anyhow::anyhow!("project not found"))?;
            let root = PathBuf::from(&project.root);
            let origin = tokio::task::spawn_blocking(move || {
                git_output(&root, &["remote", "get-url", "origin"]).ok()
            })
            .await?
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
            if let Some(origin) = origin {
                let inspection = crate::forge::inspect_origin(&origin);
                detected_provider = inspection
                    .provider
                    .unwrap_or_else(|| detect_pr_cli(&origin));
                origin_status = Some(ForgeOriginStatus {
                    host: inspection.host,
                    provider: inspection
                        .provider
                        .map(|provider| provider.id().to_string()),
                    repository: inspection.repository,
                    supported: inspection.supported,
                    server_detected: inspection.server_detected,
                    detail: inspection.detail,
                });
            }
        }

        let effective = override_kind.unwrap_or(detected_provider);
        let providers = crate::forge::provider_snapshots()
            .into_iter()
            .map(|snapshot| ForgeProviderStatus {
                provider: snapshot.provider.id().to_string(),
                change_name: snapshot.provider.change_name().to_string(),
                transport: snapshot.provider.transport().to_string(),
                command: snapshot.command,
                command_available: snapshot.command_available,
                auth_configured: snapshot.auth_configured,
                credential_source: snapshot.credential_source,
                supported: snapshot.supported,
                detail: snapshot.detail,
            })
            .collect::<Vec<_>>();
        let detail = match (&provider_override, override_error) {
            (Some(raw), Some(error)) => format!(
                "VERLINPARK_PR_PROVIDER='{raw}' is invalid ({error}); pull request creation fails with FORGE_UNSUPPORTED_PROVIDER until the override is corrected"
            ),
            (Some(_), None) => format!(
                "VERLINPARK_PR_PROVIDER selects {effective}; credentials are read from the daemon environment only"
            ),
            (None, _) => format!(
                "no provider override; {effective} is selected from the origin; credentials are read from the daemon environment only"
            ),
        };
        Ok(ForgeStatusResult {
            provider_override,
            default_provider: effective.id().to_string(),
            origin: origin_status,
            providers,
            detail,
        })
    }

    /// Run the provider's read-only preflight for a project origin without
    /// pushing a branch or creating a pull request. This is the explicit
    /// `forge.preflight` operator surface; it is never called by a scheduler.
    pub async fn forge_preflight(
        &self,
        project_id: &str,
        provider: Option<&str>,
    ) -> Result<ForgePreflightResult> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        let project = tokio::task::spawn_blocking(move || store.get_project(&project_id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("project not found"))?;
        let root = PathBuf::from(&project.root);
        let root_for_origin = root.clone();
        let origin = tokio::task::spawn_blocking(move || {
            git_output(&root_for_origin, &["remote", "get-url", "origin"])
                .map(|origin| origin.trim().to_string())
                .context("a git origin remote is required to preflight a forge provider")
        })
        .await??;
        let environment = std::env::var("VERLINPARK_PR_PROVIDER").ok();
        let target = crate::forge::resolve_target(provider, environment.as_deref(), &origin)?;
        let provider_impl: Box<dyn crate::forge::ForgeProvider> = match target.provider {
            PrCli::BitbucketCloud => Box::new(crate::forge::BitbucketCloudProvider::from_env(
                crate::forge::BitbucketCloudConfig::default(),
            )?),
            kind => {
                let binary = kind
                    .resolve_cli()
                    .ok_or_else(|| anyhow::anyhow!("no CLI configured for the {kind} provider"))?;
                Box::new(crate::forge::CliForgeProvider::new(
                    kind,
                    binary,
                    root.clone(),
                    root,
                ))
            }
        };
        self.run_forge_preflight(&target, provider_impl.as_ref())
            .await
    }

    async fn run_forge_preflight(
        &self,
        target: &crate::forge::ForgeTarget,
        provider: &dyn crate::forge::ForgeProvider,
    ) -> Result<ForgePreflightResult> {
        let preflight = provider.preflight(target).await?;
        Ok(ForgePreflightResult {
            provider: preflight.provider.id().to_string(),
            change_name: preflight.provider.change_name().to_string(),
            repository: preflight.repository.or_else(|| target.repository()),
            auth: preflight.auth,
            detail: preflight.detail,
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
        self.record_agent_usage_with_cache(
            session_id, agent_id, agent_name, role, task_id, department, model, input, output, 0,
            0, estimated, cost_usd,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn record_agent_usage_with_cache(
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
        cache_read: u64,
        cache_write: u64,
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
                cache_read,
                cache_write,
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
            totals.cache_read_tokens = totals
                .cache_read_tokens
                .saturating_add(entry.cache_read_tokens);
            totals.cache_write_tokens = totals
                .cache_write_tokens
                .saturating_add(entry.cache_write_tokens);
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
        totals.cache_hit_ratio = vp_protocol::types::cache_hit_ratio(
            totals.input_tokens,
            totals.cache_read_tokens,
            totals.cache_write_tokens,
        );
        Ok(UsageLedgerResult {
            session_id: session_id.to_string(),
            entries,
            totals,
        })
    }

    pub async fn usage_csv(&self, session_id: &str) -> Result<UsageCsvResult> {
        let ledger = self.usage_ledger(session_id).await?;
        let mut csv = String::from(
            "created_at,agent_name,role,task_id,department,model,input_tokens,cache_read_tokens,cache_write_tokens,output_tokens,total_tokens,estimated,cost_usd,cache_hit_ratio\n",
        );
        for entry in ledger.entries {
            let ratio = vp_protocol::types::cache_hit_ratio(
                entry.input_tokens,
                entry.cache_read_tokens,
                entry.cache_write_tokens,
            );
            let fields = [
                entry.created_at,
                entry.agent_name,
                entry.role,
                entry.task_id.unwrap_or_default(),
                entry.department.unwrap_or_default(),
                entry.model,
                entry.input_tokens.to_string(),
                entry.cache_read_tokens.to_string(),
                entry.cache_write_tokens.to_string(),
                entry.output_tokens.to_string(),
                (entry.input_tokens + entry.output_tokens).to_string(),
                entry.estimated.to_string(),
                format!("{:.6}", entry.cost_usd),
                format!("{ratio:.6}"),
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

    /// Bounded cache telemetry derived from the usage ledger and the latest
    /// context prompt-prefix/measurement/cache events. Never returns prompt
    /// text or provider secrets.
    /// Append a cache call event and its epoch observation atomically. The
    /// store computes cross-session fields inside the same transaction so the
    /// event payload and projection cannot disagree after a commit.
    pub async fn append_cache_call_event(
        &self,
        session_id: &str,
        kind: &str,
        payload: serde_json::Value,
        call: &PromptPrefixCall,
    ) -> Result<Event> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let kind = kind.to_string();
        let call = call.clone();
        let (event, _record) = tokio::task::spawn_blocking(move || {
            store.append_cache_call_event(&id, &kind, payload, &call)
        })
        .await??;
        let _ = self.events.send(event.clone());
        Ok(event)
    }

    /// Projection view of one epoch for one session. A read, not a claim.
    pub async fn cache_epoch_reuse(
        &self,
        project_id: &str,
        cache_epoch: &str,
        session_id: &str,
    ) -> Result<crate::cache_epoch::CacheEpochReuse> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        let cache_epoch = cache_epoch.to_string();
        let session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || {
            let aggregate = store.prompt_prefix_aggregate(&project_id, &cache_epoch)?;
            let observation =
                store.prompt_prefix_observation(&project_id, &cache_epoch, &session_id)?;
            Ok(crate::cache_epoch::reuse_from_projection(
                aggregate.as_ref(),
                observation.as_ref(),
            ))
        })
        .await?
    }

    pub async fn session_cache_status(&self, session_id: &str) -> Result<SessionCacheStatus> {
        const CACHE_CALL_SCAN_LIMIT: u32 = 2_000;
        const RECENT_CALLS: usize = 20;
        const RECENT_BREAKS: usize = 20;
        let ledger = self.usage_ledger(session_id).await?;
        let prefix_event = self
            .latest_event_of_kind(session_id, "context.prompt_prefix")
            .await?;
        let measured_event = self
            .latest_event_of_kind(session_id, "context.measured")
            .await?;
        let cache_events = self
            .recent_events_of_kind(session_id, "context.cache", CACHE_CALL_SCAN_LIMIT)
            .await?;
        let break_events = self
            .recent_events_of_kind(session_id, "context.cache_break", 500)
            .await?;
        let compaction_events = self
            .recent_events_of_kind(session_id, "context.compaction", CACHE_CALL_SCAN_LIMIT)
            .await?;
        let cache_reset_events = self
            .recent_events_of_kind(
                session_id,
                "context.compaction.cache_reset",
                CACHE_CALL_SCAN_LIMIT,
            )
            .await?;
        // L4 reset events are authoritative when present. Older sessions only
        // have `context.compaction`, whose reset fields are policy-gated; using
        // both would double-count the same boundary.
        let lifecycle_events = if cache_reset_events.is_empty() {
            compaction_events
        } else {
            cache_reset_events
        };
        let aggregate =
            crate::prompt_cache::aggregate_cache_events(&cache_events, &lifecycle_events);
        let continuity = crate::conversation::aggregate_continuity(&cache_events, &break_events);

        let prompt_prefix = prefix_event
            .as_ref()
            .and_then(|event| event.payload.get("prefix_hash"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                cache_events
                    .last()
                    .and_then(|event| event.payload.get("prompt_prefix_hash"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .or_else(|| {
                measured_event
                    .as_ref()
                    .and_then(|event| event.payload.get("prompt_prefix_hash"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            });
        let hash = |key: &str| {
            prefix_event
                .as_ref()
                .and_then(|event| event.payload.get(key))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        };
        let preamble_tokens_estimate = prefix_event.as_ref().and_then(|event| {
            event
                .payload
                .get("preamble_tokens_estimate")
                .and_then(serde_json::Value::as_u64)
                .or_else(|| {
                    event
                        .payload
                        .get("stable_prefix_chars")
                        .and_then(serde_json::Value::as_u64)
                        .map(|chars| chars / 4)
                })
        });
        let estimated_savings_usd = crate::prompt_cache::round_usd(
            cache_events
                .iter()
                .filter_map(|event| {
                    event
                        .payload
                        .get("estimated_savings_usd")
                        .and_then(serde_json::Value::as_f64)
                })
                .sum::<f64>(),
        );
        let last_recorded = cache_events.last();
        // Warm calls are real provider calls and count in totals, but "last
        // miss" should describe the latest ordinary agent call so a warm does
        // not hide the most recent production miss.
        let last_cache = cache_events
            .iter()
            .rev()
            .find(|event| {
                event
                    .payload
                    .get("warm")
                    .and_then(serde_json::Value::as_bool)
                    != Some(true)
            })
            .or(last_recorded);
        let last_call_at = ledger
            .entries
            .last()
            .map(|entry| entry.created_at.clone())
            .or_else(|| last_recorded.map(|event| event.created_at.clone()))
            .or_else(|| measured_event.map(|event| event.created_at));
        let cache_calls = cache_events
            .iter()
            .filter(|event| {
                event
                    .payload
                    .get("warm")
                    .and_then(serde_json::Value::as_bool)
                    != Some(true)
            })
            .count() as u64;
        let recent_calls = cache_events
            .iter()
            .rev()
            .take(RECENT_CALLS)
            .map(|event| crate::prompt_cache::cache_call_record(&event.payload, &event.created_at))
            .collect::<Vec<_>>();
        let recent_breaks = break_events
            .iter()
            .rev()
            .take(RECENT_BREAKS)
            .map(|event| crate::prompt_cache::cache_break_record(&event.payload, &event.created_at))
            .collect::<Vec<_>>();
        let session_store = self.store.clone();
        let lookup_id = session_id.to_string();
        let project_id = tokio::task::spawn_blocking(move || session_store.get_session(&lookup_id))
            .await??
            .map(|session| session.project_id);
        let cache_epoch = prefix_event
            .as_ref()
            .and_then(|event| crate::cache_epoch::finalized_epoch_hash(&event.payload))
            .map(str::to_string)
            .or_else(|| {
                last_cache
                    .and_then(|event| crate::cache_epoch::finalized_epoch_hash(&event.payload))
                    .map(str::to_string)
            });
        let (epoch_aggregate, epoch_observation) =
            match (project_id.as_deref(), cache_epoch.as_deref()) {
                (Some(project_id), Some(epoch)) => {
                    let store = self.store.clone();
                    let project = project_id.to_string();
                    let epoch = epoch.to_string();
                    let session = session_id.to_string();
                    tokio::task::spawn_blocking(move || {
                        let aggregate = store.prompt_prefix_aggregate(&project, &epoch)?;
                        let observation =
                            store.prompt_prefix_observation(&project, &epoch, &session)?;
                        Ok::<_, anyhow::Error>((aggregate, observation))
                    })
                    .await??
                }
                _ => (None, None),
            };
        let policy = match project_id {
            Some(project_id) => self.get_cache_policy(&project_id).await.ok(),
            None => None,
        };
        let latest_cache_epoch = aggregate.latest_cache_epoch.clone().or_else(|| {
            prefix_event
                .as_ref()
                .and_then(|event| event.payload.get("cache_epoch"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        });
        let cross_session = match latest_cache_epoch.as_deref() {
            Some(epoch) => self
                .cache_epoch_cross_session(epoch, session_id)
                .await
                .unwrap_or_default(),
            None => crate::prompt_cache::CacheEpochReuse::default(),
        };
        let role_costs = ledger
            .totals
            .by_role
            .iter()
            .map(|bucket| (bucket.key.clone(), bucket.cost_usd))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut per_role = aggregate.per_role.clone();
        for bucket in &mut per_role {
            if let Some(cost) = role_costs.get(&bucket.role) {
                bucket.cost_usd = crate::prompt_cache::round_usd(*cost);
            }
        }
        let cache_miss_tokens = crate::prompt_cache::cache_miss_tokens(
            ledger.totals.input_tokens,
            ledger.totals.cache_read_tokens,
        );

        let epoch_reuse = crate::cache_epoch::reuse_from_projection(
            epoch_aggregate.as_ref(),
            epoch_observation.as_ref(),
        );
        let epoch_session_count = epoch_aggregate
            .as_ref()
            .map(|row| row.session_count)
            .unwrap_or(0);

        Ok(SessionCacheStatus {
            session_id: session_id.to_string(),
            cache_epoch: cache_epoch.clone(),
            cache_epoch_source: cache_epoch.as_ref().and_then(|_| {
                prefix_event
                    .as_ref()
                    .and_then(|event| event.payload.get("cache_epoch_source"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            }),
            cache_epoch_first_seen: epoch_observation.as_ref().map(|row| row.first_seen.clone()),
            cache_epoch_last_seen: epoch_observation.as_ref().map(|row| row.last_seen.clone()),
            cache_epoch_project_first_seen: epoch_aggregate
                .as_ref()
                .map(|row| row.first_seen.clone()),
            cache_epoch_project_last_seen: epoch_aggregate
                .as_ref()
                .map(|row| row.last_seen.clone()),
            cache_epoch_core_sections: epoch_aggregate
                .as_ref()
                .map(|row| row.core_sections.clone())
                .unwrap_or_default(),
            cache_epoch_project_sections: epoch_aggregate
                .as_ref()
                .map(|row| row.project_sections.clone())
                .unwrap_or_default(),
            cache_epoch_session_count: epoch_session_count,
            cache_epoch_call_count: epoch_aggregate
                .as_ref()
                .map(|row| row.call_count)
                .unwrap_or(0),
            cache_epoch_observed_input_tokens: epoch_aggregate
                .as_ref()
                .map(|row| row.observed_input_tokens)
                .unwrap_or(0),
            cache_epoch_observed_cache_read_tokens: epoch_aggregate
                .as_ref()
                .map(|row| row.observed_cache_read_tokens)
                .unwrap_or(0),
            cache_epochs_seen: aggregate.cache_epoch_counts.len() as u64,
            prefix_reused_from_sessions: last_cache
                .and_then(|event| event.payload.get("cache_epoch_reused_from_sessions"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(epoch_reuse.prior_sessions),
            cross_session_hit_tokens: aggregate.cross_session_hit_tokens,
            cross_session_first_calls: aggregate.cross_session_first_calls,
            provider_cache_misses: aggregate.provider_cache_misses,
            prefix_changes: aggregate.prefix_changes,
            compaction_cache_resets: aggregate.compaction_resets,
            compaction_lost_cache_usd: aggregate.compaction_lost_cache_usd,
            input_tokens: ledger.totals.input_tokens,
            cache_read_tokens: ledger.totals.cache_read_tokens,
            cache_write_tokens: ledger.totals.cache_write_tokens,
            output_tokens: ledger.totals.output_tokens,
            cache_hit_ratio: ledger.totals.cache_hit_ratio,
            cache_read_ratio: if ledger.totals.input_tokens == 0 {
                0.0
            } else {
                (ledger.totals.cache_read_tokens as f64 / ledger.totals.input_tokens as f64)
                    .clamp(0.0, 1.0)
            },
            cache_write_ratio: if ledger.totals.input_tokens == 0 {
                0.0
            } else {
                (ledger.totals.cache_write_tokens as f64 / ledger.totals.input_tokens as f64)
                    .clamp(0.0, 1.0)
            },
            estimated_savings_usd,
            cache_calls,
            prompt_prefix,
            cache_generation: last_cache
                .and_then(|event| event.payload.get("cache_generation"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .or_else(|| hash("cache_generation")),
            core_hash: hash("core_hash"),
            project_hash: hash("project_hash"),
            tools_hash: hash("tools_hash"),
            tool_bundle: hash("tool_bundle"),
            tool_bundle_hash: hash("tool_bundle_hash"),
            tool_bundle_tool_count: prefix_event.as_ref().and_then(|event| {
                event
                    .payload
                    .get("tool_bundle_tool_count")
                    .or_else(|| event.payload.get("tool_count"))
                    .and_then(serde_json::Value::as_u64)
            }),
            tool_bundle_mcp_count: prefix_event.as_ref().and_then(|event| {
                event
                    .payload
                    .get("tool_bundle_mcp_count")
                    .and_then(serde_json::Value::as_u64)
            }),
            volatile_hash: hash("volatile_hash"),
            preamble_tokens_estimate,
            last_call_at,
            last_breakpoints: last_cache
                .and_then(|event| event.payload.get("cache_breakpoints"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as u32,
            last_cache_ttl: last_cache
                .and_then(|event| event.payload.get("cache_ttl"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .or_else(|| policy.as_ref().map(|policy| policy.ttl.clone())),
            last_miss_reason: last_cache
                .and_then(|event| event.payload.get("miss_reason"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            last_first_token_ms: last_cache
                .and_then(|event| event.payload.get("first_token_ms"))
                .and_then(serde_json::Value::as_u64),
            last_message_prefix_hash: continuity.last_message_prefix_hash.or_else(|| {
                last_cache
                    .and_then(|event| event.payload.get("message_prefix_hash"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            }),
            last_previous_message_prefix_hash: continuity.last_previous_message_prefix_hash,
            non_append_only_calls: continuity.non_append_only_calls,
            last_message_count: continuity.last_message_count,
            last_tail_memory_mode: continuity.last_tail_memory_mode,
            provider_tail_system: continuity.provider_tail_system,
            last_tail_system_capability: continuity.last_tail_system_capability,
            cache_break_counts: continuity.break_reason_counts,
            recent_breaks,
            last_cache_break_at: continuity.last_break_at,
            last_message_sequence_issue_count: continuity.last_message_sequence_issue_count,
            last_message_sequence_issues: continuity.last_message_sequence_issues,
            message_sequence_issue_counts: continuity.message_sequence_issue_counts,
            recent_calls,
            miss_reason_counts: aggregate.miss_reason_counts,
            ttl_counts: aggregate.ttl_counts,
            breakpoint_counts: aggregate.breakpoint_counts,
            savings_by_role: aggregate.savings_by_role,
            first_token_p50_ms: aggregate.first_token_p50_ms,
            first_token_p95_ms: aggregate.first_token_p95_ms,
            compaction_resets: aggregate.compaction_resets,
            compaction_invalidation_tokens: aggregate.compaction_invalidation_tokens,
            compaction_invalidation_estimated_usd: aggregate.compaction_invalidation_estimated_usd,
            policy,
            cache_miss_tokens,
            theoretical_max_hit_ratio: aggregate.latest_theoretical_max_hit_ratio,
            append_only_calls: aggregate.append_only_calls,
            append_only_breaks: aggregate.append_only_breaks,
            last_append_only: aggregate.last_append_only,
            hit_ratio_p50: aggregate.hit_ratio_p50,
            hit_ratio_p95: aggregate.hit_ratio_p95,
            theoretical_max_p50: aggregate.theoretical_max_p50,
            theoretical_max_p95: aggregate.theoretical_max_p95,
            cost_usd: ledger.totals.cost_usd,
            cache_epoch_timeline: aggregate.cache_epoch_timeline.clone(),
            per_role,
            top_miss_reasons: aggregate.top_miss_reasons.clone(),
            cross_session_reuse_sessions: cross_session.sessions,
            cross_session_calls_observed: cross_session.calls,
        })
    }

    /// Bounded newest-first per-call cache table for `session.cache.calls`.
    ///
    /// Filters are exact and case-insensitive for role/reason. Warm calls are
    /// excluded by default. The scan is bounded to the newest 2,000 cache
    /// events; `truncated` reports only whether the requested limit cut the
    /// matching rows, not whether the underlying scan hit its bound.
    pub async fn session_cache_calls(
        &self,
        params: &vp_protocol::types::SessionCacheCallsParams,
    ) -> Result<vp_protocol::types::SessionCacheCallsResult> {
        const SCAN_LIMIT: u32 = 2_000;
        let limit = params.limit.unwrap_or(20).clamp(1, 100);
        let role_filter = params
            .role
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_ascii_lowercase);
        let reason_filter = params
            .miss_reason
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_ascii_lowercase);
        let events = self
            .recent_events_of_kind(&params.session_id, "context.cache", SCAN_LIMIT)
            .await?;
        let mut matching = Vec::new();
        for event in events.iter().rev() {
            let warm = event
                .payload
                .get("warm")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            if warm && !params.include_warm {
                continue;
            }
            let role = event
                .payload
                .get("agent_role")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            if let Some(filter) = role_filter.as_deref()
                && role != filter
            {
                continue;
            }
            let reason = event
                .payload
                .get("miss_reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            if let Some(filter) = reason_filter.as_deref()
                && reason != filter
            {
                continue;
            }
            matching.push(crate::prompt_cache::cache_call_record(
                &event.payload,
                &event.created_at,
            ));
        }
        let observed = matching.len() as u64;
        matching.truncate(limit as usize);
        let returned = matching.len() as u64;
        Ok(vp_protocol::types::SessionCacheCallsResult {
            session_id: params.session_id.clone(),
            calls: matching,
            observed,
            returned,
            truncated: observed > returned,
            limit,
            role: params.role.clone(),
            miss_reason: params.miss_reason.clone(),
            include_warm: params.include_warm,
        })
    }

    /// Read-only project-wide cache overview over a bounded scan of recent
    /// `context.cache` events. No provider call, no prompt text, and no
    /// cross-host claim.
    pub async fn project_cache_overview(
        &self,
        params: &vp_protocol::types::ProjectCacheParams,
    ) -> Result<vp_protocol::types::ProjectCacheOverview> {
        let limit = params.limit.unwrap_or(5_000).clamp(1, 50_000);
        let store = self.store.clone();
        let project_id = params.project_id.clone();
        let (events, truncated) =
            tokio::task::spawn_blocking(move || store.project_cache_events(&project_id, limit))
                .await??;
        let sessions = events
            .iter()
            .map(|event| event.session_id.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len() as u64;
        let aggregate = crate::prompt_cache::aggregate_cache_events(&events, &[]);
        Ok(vp_protocol::types::ProjectCacheOverview {
            project_id: params.project_id.clone(),
            sessions_observed: sessions,
            calls: aggregate.calls,
            input_tokens: aggregate.input_tokens,
            cache_read_tokens: aggregate.cache_read_tokens,
            cache_miss_tokens: aggregate.cache_miss_tokens,
            cache_write_tokens: aggregate.cache_write_tokens,
            hit_ratio: aggregate.hit_ratio,
            hit_ratio_p50: aggregate.hit_ratio_p50,
            hit_ratio_p95: aggregate.hit_ratio_p95,
            append_only_calls: aggregate.append_only_calls,
            append_only_breaks: aggregate.append_only_breaks,
            cache_epochs: aggregate.cache_epoch_timeline.len() as u64,
            top_miss_reasons: aggregate.top_miss_reasons,
            cache_epoch_timeline: aggregate.cache_epoch_timeline,
            scan_limit: limit,
            truncated,
        })
    }

    /// Bounded read-only structural break history for one session. It scans
    /// `context.cache_break` and the ordinary `context.cache` window to report
    /// append-only counts, never message text, and never calls a provider.
    pub async fn session_cache_breaks(
        &self,
        session_id: &str,
        limit: Option<u32>,
    ) -> Result<vp_protocol::types::SessionCacheBreaksResult> {
        const SCAN_LIMIT: u32 = 2_000;
        let limit = limit.unwrap_or(50).clamp(1, 200) as usize;
        let break_events = self
            .recent_events_of_kind(session_id, "context.cache_break", SCAN_LIMIT)
            .await?;
        let cache_events = self
            .recent_events_of_kind(session_id, "context.cache", SCAN_LIMIT)
            .await?;
        let continuity = crate::conversation::aggregate_continuity(&cache_events, &break_events);
        let total_breaks = break_events.len() as u64;
        let breaks = break_events
            .iter()
            .rev()
            .take(limit)
            .map(|event| crate::prompt_cache::cache_break_record(&event.payload, &event.created_at))
            .collect::<Vec<_>>();
        Ok(vp_protocol::types::SessionCacheBreaksResult {
            session_id: session_id.to_string(),
            append_only_calls: continuity.append_only_calls,
            non_append_only_calls: continuity.non_append_only_calls,
            total_breaks,
            truncated: total_breaks > breaks.len() as u64,
            breaks,
        })
    }

    /// Read-only cache-epoch inspection. No provider call is made and nothing
    /// is mutated. The epoch timeline is derived from bounded event windows.
    pub async fn session_cache_epoch(
        &self,
        session_id: &str,
        limit: Option<u32>,
    ) -> Result<vp_protocol::types::SessionCacheEpochResult> {
        let limit = limit
            .unwrap_or(50)
            .clamp(1, crate::cache_epoch::MAX_CACHE_EPOCH_TIMELINE as u32);
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        let prefix_events = self
            .recent_events_of_kind(
                session_id,
                "context.prompt_prefix",
                crate::cache_epoch::MAX_CACHE_EPOCH_TIMELINE as u32,
            )
            .await?;
        let reset_events = self
            .recent_events_of_kind(
                session_id,
                "context.compaction.cache_reset",
                crate::cache_epoch::MAX_CACHE_EPOCH_TIMELINE as u32,
            )
            .await?;
        let cache_events = self
            .recent_events_of_kind(session_id, "context.cache", 2_000)
            .await?;
        let (timeline, timeline_truncated) =
            crate::cache_epoch::epoch_timeline(&prefix_events, &reset_events, limit as usize);
        let current_epoch = prefix_events
            .iter()
            .rev()
            .find_map(|event| crate::cache_epoch::CacheEpoch::from_event_payload(&event.payload))
            .map(|epoch| epoch.hash)
            .or_else(|| {
                cache_events.iter().rev().find_map(|event| {
                    crate::cache_epoch::finalized_epoch_hash(&event.payload).map(str::to_string)
                })
            });
        let store = self.store.clone();
        let project_id = session.project_id.clone();
        let reuse_project = session.project_id.clone();
        let sid = session_id.to_string();
        let epoch_limit = limit;
        let (aggregates, observations, total_epochs) = tokio::task::spawn_blocking(move || {
            let aggregates = store.list_prompt_prefix_aggregates(&project_id, 1_000)?;
            let observations = store.list_prompt_prefix_observations(&project_id, &sid)?;
            let total = store.count_prompt_prefix_aggregates(&project_id)?;
            Ok::<_, anyhow::Error>((aggregates, observations, total))
        })
        .await??;
        let epochs =
            crate::cache_epoch::summarize_epochs(&aggregates, &observations, epoch_limit as usize);
        let mut current = epochs
            .iter()
            .find(|summary| Some(&summary.cache_epoch) == current_epoch.as_ref())
            .cloned();
        if current.is_none()
            && let Some(epoch) = current_epoch.as_deref()
            && let Some(aggregate) = aggregates
                .iter()
                .find(|aggregate| aggregate.cache_epoch == epoch)
        {
            current = crate::cache_epoch::summarize_epochs(
                std::slice::from_ref(aggregate),
                &observations,
                1,
            )
            .into_iter()
            .next();
        }
        let reused_sessions = if let Some(epoch) = current_epoch.clone() {
            let store = self.store.clone();
            let sid = session_id.to_string();
            tokio::task::spawn_blocking(move || {
                store.list_prompt_prefix_reuse_sessions(
                    &reuse_project,
                    &epoch,
                    Some(&sid),
                    limit.min(50),
                )
            })
            .await??
        } else {
            Vec::new()
        };
        let stats = crate::prompt_cache::aggregate_cache_events(&cache_events, &reset_events);
        let reuse = crate::cache_epoch::reuse_from_projection(
            current_epoch.as_deref().and_then(|epoch| {
                aggregates
                    .iter()
                    .find(|aggregate| aggregate.cache_epoch == epoch)
            }),
            current_epoch.as_deref().and_then(|epoch| {
                observations
                    .iter()
                    .find(|observation| observation.cache_epoch == epoch)
            }),
        );
        Ok(vp_protocol::types::SessionCacheEpochResult {
            session_id: session_id.to_string(),
            project_id: session.project_id,
            prompt_layout: session.prompt_layout,
            current_epoch: current,
            epochs,
            timeline,
            reused_sessions,
            total_epochs,
            timeline_truncated,
            prefix_reused_from_sessions: reuse.prior_sessions,
            cross_session_hit_tokens: stats.cross_session_hit_tokens,
            cross_session_first_calls: stats.cross_session_first_calls,
            provider_cache_misses: stats.provider_cache_misses,
            compaction_resets: stats.compaction_resets,
            compaction_lost_cache_usd: stats.compaction_lost_cache_usd,
            generated_at: vp_protocol::now_rfc3339(),
        })
    }

    /// Read-only project-wide cache-epoch report. It summarizes the same
    /// event-derived projection as `session.cache.epoch` without requiring a
    /// session and without a provider call.
    pub async fn project_cache_epochs(
        &self,
        project_id: &str,
        limit: Option<u32>,
    ) -> Result<ProjectCacheEpochResult> {
        let limit = limit
            .unwrap_or(50)
            .clamp(1, crate::cache_epoch::MAX_CACHE_EPOCH_AGGREGATES as u32);
        let store = self.store.clone();
        let project = project_id.to_string();
        let (aggregates, total_epochs, bundles, totals) = tokio::task::spawn_blocking(move || {
            let aggregates = store.list_prompt_prefix_aggregates(&project, 1_000)?;
            let total = store.count_prompt_prefix_aggregates(&project)?;
            let bundles = store.prompt_prefix_bundle_buckets(&project)?;
            let totals = store.prompt_prefix_totals(&project)?;
            Ok::<_, anyhow::Error>((aggregates, total, bundles, totals))
        })
        .await??;
        let epochs = crate::cache_epoch::summarize_epochs(&aggregates, &[], limit as usize);
        let PromptPrefixTotals {
            total_sessions,
            total_calls,
            observed_input_tokens,
            observed_cache_read_tokens,
        } = totals;
        Ok(ProjectCacheEpochResult {
            project_id: project_id.to_string(),
            epochs,
            bundles,
            total_epochs,
            total_sessions,
            total_calls,
            observed_input_tokens,
            observed_cache_read_tokens,
            cache_hit_ratio: if observed_input_tokens == 0 {
                0.0
            } else {
                (observed_cache_read_tokens as f64 / observed_input_tokens as f64).clamp(0.0, 1.0)
            },
            generated_at: vp_protocol::now_rfc3339(),
        })
    }

    /// Prune old cache-epoch telemetry for one project. This is an explicit
    /// maintenance operation; nothing in the scheduler calls it. The newest
    /// epochs and epochs with active sessions are retained.
    pub async fn prune_project_cache_epochs(
        &self,
        project_id: &str,
        older_than_days: Option<u32>,
        keep_epochs: Option<u32>,
        dry_run: bool,
    ) -> Result<ProjectCacheEpochPruneResult> {
        let days = older_than_days.unwrap_or(90).clamp(1, 3_650);
        let keep = keep_epochs.unwrap_or(50).clamp(1, 1_000);
        let cutoff = (time::OffsetDateTime::now_utc() - time::Duration::days(i64::from(days)))
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|error| anyhow::anyhow!("formatting cache-epoch cutoff: {error}"))?;
        let store = self.store.clone();
        let project = project_id.to_string();
        let cutoff_for_store = cutoff.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            store.prune_prompt_prefixes(&project, &cutoff_for_store, keep, dry_run)
        })
        .await??;
        Ok(ProjectCacheEpochPruneResult {
            project_id: project_id.to_string(),
            dry_run,
            cutoff,
            removed_epochs: outcome.removed_epochs,
            removed_observations: outcome.removed_observations,
            kept_epochs: outcome.kept_epochs,
            remaining_epochs: outcome.remaining_epochs,
            protected_epochs: outcome.protected_epochs,
        })
    }

    /// Bounded inspection of the ordered instruction state for one session.
    ///
    /// Returns exact keys, hashes, ordinals, tail flags, and character counts.
    /// It never returns section text; the only exact-byte path is test-only in
    /// `vp_core::instruction`. No provider call, worktree, or generation is
    /// triggered by this read.
    pub async fn session_instructions(
        &self,
        session_id: &str,
        agent_id: Option<&str>,
    ) -> Result<SessionInstructionsReport> {
        let session = self
            .store
            .get_session(session_id)?
            .ok_or_else(|| anyhow!("session not found: {session_id}"))?;
        let states = self.store.list_instruction_states_for_session(session_id)?;
        let agents = self.list_agents(session_id).await?;
        let (latest_prefix_hash, latest_cache_epoch, latest_stable_prefix_hash) = {
            let prefixes = self
                .recent_events_of_kind(session_id, "context.prompt_prefix", 1)
                .await?;
            crate::instruction::latest_prompt_prefix(&prefixes)
        };
        let rebuilt_events = self
            .recent_events_of_kind(session_id, "context.instruction.rebuilt", 200)
            .await?;
        let mut by_state: std::collections::BTreeMap<
            String,
            Vec<vp_protocol::types::SessionInstructionState>,
        > = std::collections::BTreeMap::new();
        for state in states {
            by_state
                .entry(state.session_id.clone())
                .or_default()
                .push(state);
        }
        let mut reports = Vec::with_capacity(by_state.len());
        for (state_id, rows) in by_state {
            let scoped_agent = crate::instruction::instruction_state_agent(&state_id, session_id);
            if let Some(filter) = agent_id {
                let matches_leader = filter == "leader" && state_id == session_id;
                if scoped_agent.as_deref() != Some(filter) && !matches_leader {
                    continue;
                }
            }
            let role = scoped_agent
                .as_deref()
                .and_then(|agent_id| agents.iter().find(|agent| agent.id == agent_id))
                .map(|agent| agent.role.clone())
                .or_else(|| (state_id == session_id).then(|| "leader".to_string()));
            let mut sections = Vec::with_capacity(rows.len());
            let mut head_texts = Vec::new();
            let mut tail_texts = Vec::new();
            for row in &rows {
                let blob = self.store.get_instruction_blob(&row.hash)?;
                let available = blob
                    .as_ref()
                    .is_some_and(|blob| sha256_hex(blob.text.as_bytes()) == row.hash);
                let chars = blob
                    .as_ref()
                    .filter(|_| available)
                    .map(|blob| blob.text.chars().count() as u64)
                    .unwrap_or(0);
                if let Some(blob) = blob.filter(|_| available) {
                    if row.tail {
                        tail_texts.push(blob.text);
                    } else {
                        head_texts.push(blob.text);
                    }
                }
                sections.push(SessionInstructionSection {
                    key: row.key.clone(),
                    hash: row.hash.clone(),
                    ordinal: row.ordinal,
                    tail: row.tail,
                    chars,
                    available,
                });
            }
            let head_hash = if head_texts.is_empty() {
                String::new()
            } else {
                sha256_hex(head_texts.join("\n\n").as_bytes())
            };
            let tail_hash = if tail_texts.is_empty() {
                String::new()
            } else {
                sha256_hex(tail_texts.join("\n\n").as_bytes())
            };
            reports.push(SessionInstructionStateReport {
                state_id,
                role,
                agent_id: scoped_agent,
                prompt_layout: crate::instruction::PROMPT_LAYOUT_V3,
                head_hash,
                tail_hash,
                sections,
            });
        }
        Ok(SessionInstructionsReport {
            session_id: session_id.to_string(),
            prompt_layout: crate::instruction::normalize_prompt_layout(session.prompt_layout),
            latest_prefix_hash,
            latest_cache_epoch,
            latest_stable_prefix_hash,
            states: reports,
            last_rebuilt: rebuilt_events.len() as u64,
        })
    }

    /// Persist a session-scoped prompt layout override. A rollback to v2 keeps
    /// the existing instruction state untouched so switching back to v3 reuses
    /// the same bytes; no migration is destructive.
    pub async fn set_session_prompt_layout(
        &self,
        session_id: &str,
        prompt_layout: u32,
    ) -> Result<vp_protocol::types::SetSessionPromptLayoutResult> {
        if !matches!(prompt_layout, 2 | 3) {
            anyhow::bail!("prompt_layout must be 2 or 3");
        }
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking({
            let store = store.clone();
            let id = id.clone();
            move || store.get_session(&id)
        })
        .await??
        .ok_or_else(|| anyhow!("session not found: {session_id}"))?;
        if session.status == "imported" {
            anyhow::bail!("imported sessions are read-only archives");
        }
        let event = tokio::task::spawn_blocking(move || {
            store.set_session_prompt_layout_with_event(&id, prompt_layout)
        })
        .await??;
        let _ = self.events.send(event);
        Ok(vp_protocol::types::SetSessionPromptLayoutResult {
            session_id: session_id.to_string(),
            prompt_layout,
        })
    }

    /// Set the project default for newly created sessions. Existing sessions
    /// keep their persisted layout.
    pub async fn set_project_prompt_layout(
        &self,
        project_id: &str,
        prompt_layout: u32,
    ) -> Result<vp_protocol::types::SetProjectPromptLayoutResult> {
        if !matches!(prompt_layout, 2 | 3) {
            anyhow::bail!("prompt_layout must be 2 or 3");
        }
        let mut policy = self.get_cache_policy(project_id).await?;
        policy.prompt_layout = prompt_layout;
        let policy = policy
            .normalized()
            .map_err(|message| anyhow!("invalid cache policy: {message}"))?;
        self.set_cache_policy(project_id, policy).await?;
        Ok(vp_protocol::types::SetProjectPromptLayoutResult {
            project_id: project_id.to_string(),
            prompt_layout,
        })
    }

    /// Deterministic cache audit over persisted events. No provider call and
    /// no TTL inference; findings are derived only from recorded telemetry.
    pub async fn session_cache_audit(
        &self,
        session_id: &str,
    ) -> Result<vp_protocol::types::SessionCacheAuditResult> {
        const LIMIT: u32 = 2_000;
        let cache_events = self
            .recent_events_of_kind(session_id, "context.cache", LIMIT)
            .await?;
        let prefix_events = self
            .recent_events_of_kind(session_id, "context.prompt_prefix", 50)
            .await?;
        let compaction_events = self
            .recent_events_of_kind(session_id, "context.compaction", LIMIT)
            .await?;
        let break_events = self
            .recent_events_of_kind(session_id, "context.cache_break", LIMIT)
            .await?;
        let cache_reset_events = self
            .recent_events_of_kind(session_id, "context.compaction.cache_reset", LIMIT)
            .await?;
        // Prefer the L4 reset events when present; older sessions only have
        // `context.compaction`, whose reset fields are policy-gated.
        let lifecycle_events = if cache_reset_events.is_empty() {
            compaction_events
        } else {
            cache_reset_events
        };
        let session_store = self.store.clone();
        let lookup_id = session_id.to_string();
        let project_id = tokio::task::spawn_blocking(move || session_store.get_session(&lookup_id))
            .await??
            .map(|session| session.project_id);
        let policy = match project_id {
            Some(project_id) => self.get_cache_policy(&project_id).await.ok(),
            None => None,
        };
        let provider = prefix_events
            .iter()
            .rev()
            .find_map(|event| {
                event
                    .payload
                    .get("cache_plan")
                    .and_then(|plan| plan.get("provider"))
                    .and_then(serde_json::Value::as_str)
            })
            .map(str::to_string);
        Ok(crate::cache_audit::audit_cache_events(
            session_id,
            provider.as_deref(),
            policy.as_ref(),
            &cache_events,
            &prefix_events,
            &lifecycle_events,
            &break_events,
        ))
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

    /// Publish an artifact with an idempotency key scoped to the session. A
    /// repeated key returns the first committed artifact instead of creating a
    /// duplicate. The pre-check is atomic enough for the local single-daemon
    /// runtime; a cross-process race can still create an orphan row but never
    /// causes the caller to receive a different artifact than the registered
    /// winner.
    #[allow(clippy::too_many_arguments)]
    pub async fn publish_artifact_keyed(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        task_id: Option<String>,
        name: &str,
        kind: &str,
        mime_type: &str,
        content: &str,
        request_key: &str,
    ) -> Result<Artifact> {
        let request_key = request_key.trim();
        if request_key.is_empty() || request_key.len() > 160 {
            anyhow::bail!("artifact request key must be 1-160 characters");
        }
        if !request_key
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_:./".contains(character))
        {
            anyhow::bail!("artifact request key contains unsupported characters");
        }
        let store = self.store.clone();
        let session_id_owned = session_id.to_string();
        let key = request_key.to_string();
        if let Some(existing) = tokio::task::spawn_blocking(move || {
            store.get_artifact_by_request_key(&session_id_owned, &key)
        })
        .await??
        {
            return Ok(existing);
        }
        let artifact = self
            .publish_artifact(
                session_id, agent_id, agent_name, task_id, name, kind, mime_type, content,
            )
            .await?;
        let store = self.store.clone();
        let session_id_owned = session_id.to_string();
        let key = request_key.to_string();
        let artifact_id = artifact.id.clone();
        tokio::task::spawn_blocking(move || {
            store.register_artifact_request(&session_id_owned, &key, &artifact_id)
        })
        .await??;
        Ok(artifact)
    }

    /// Publish a new version of an inline artifact. `expected_version` is an
    /// optimistic concurrency guard so two writers cannot silently overwrite
    /// each other.
    #[allow(clippy::too_many_arguments)]
    pub async fn publish_artifact_version(
        &self,
        artifact_id: &str,
        expected_version: Option<u32>,
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
        let artifact_id = artifact_id.to_string();
        let artifact_id_for_error = artifact_id.clone();
        let name = name.to_string();
        let kind = kind.to_string();
        let mime_type = mime_type.to_string();
        let content = content.to_string();
        let Some((artifact, event)) = tokio::task::spawn_blocking(move || {
            store.update_artifact_with_event(
                &artifact_id,
                expected_version,
                &name,
                &kind,
                &mime_type,
                &content,
            )
        })
        .await??
        else {
            anyhow::bail!("artifact not found: {artifact_id_for_error}");
        };
        let _ = self.events.send(event);
        self.auto_gc_restarted_artifact(&artifact.session_id).await;
        Ok(artifact)
    }

    /// Compare two inline versions (or a version against the current artifact)
    /// with a bounded deterministic line diff.
    pub async fn artifact_diff(&self, params: ArtifactDiffParams) -> Result<ArtifactDiffResult> {
        let store = self.store.clone();
        let id = params.artifact_id.clone();
        let current = tokio::task::spawn_blocking(move || store.get_artifact(&id))
            .await??
            .ok_or_else(|| anyhow!("artifact not found: {}", params.artifact_id))?;
        if current.storage != "inline" {
            anyhow::bail!("artifact diff is only available for inline text artifacts");
        }
        let to_version = params.to_version.unwrap_or(current.version);
        let store = self.store.clone();
        let id = params.artifact_id.clone();
        let from = tokio::task::spawn_blocking(move || {
            store.get_artifact_version(&id, params.from_version)
        })
        .await??
        .ok_or_else(|| {
            anyhow!(
                "artifact version not found: {}@{}",
                params.artifact_id,
                params.from_version
            )
        })?;
        if from.storage != "inline" {
            anyhow::bail!("artifact diff is only available for inline text artifacts");
        }
        let to = if to_version == current.version {
            let mut version = from.clone();
            version.version = current.version;
            version.name = current.name.clone();
            version.kind = current.kind.clone();
            version.mime_type = current.mime_type.clone();
            version.content = current.content.clone();
            version.storage = current.storage.clone();
            version.compression = current.compression.clone();
            version.size_bytes = current.size_bytes;
            version.sha256 = current.sha256.clone();
            version.path = current.path.clone();
            version.created_at = current.created_at.clone();
            version
        } else {
            let store = self.store.clone();
            let id = params.artifact_id.clone();
            tokio::task::spawn_blocking(move || store.get_artifact_version(&id, to_version))
                .await??
                .ok_or_else(|| {
                    anyhow!(
                        "artifact version not found: {}@{}",
                        params.artifact_id,
                        to_version
                    )
                })?
        };
        if to.storage != "inline" {
            anyhow::bail!("artifact diff is only available for inline text artifacts");
        }
        Ok(ArtifactDiffResult {
            artifact_id: current.id,
            name: current.name,
            from_version: from.version,
            to_version: to.version,
            diff: crate::textdiff::diff_text(
                &from.content,
                &to.content,
                params.context.unwrap_or(3) as usize,
                crate::textdiff::DEFAULT_MAX_DIFF_LINES,
            ),
        })
    }

    pub async fn artifact_versions(&self, artifact_id: &str) -> Result<ArtifactVersionsResult> {
        let store = self.store.clone();
        let artifact_id = artifact_id.to_string();
        let versions =
            tokio::task::spawn_blocking(move || store.list_artifact_versions(&artifact_id))
                .await??;
        Ok(ArtifactVersionsResult { versions })
    }

    pub async fn restore_artifact_version(
        &self,
        artifact_id: &str,
        version: u32,
    ) -> Result<Artifact> {
        let store = self.store.clone();
        let artifact_id = artifact_id.to_string();
        let artifact_id_for_error = artifact_id.clone();
        let Some((artifact, event)) = tokio::task::spawn_blocking(move || {
            store.restore_artifact_version(&artifact_id, version)
        })
        .await??
        else {
            anyhow::bail!("artifact version not found: {artifact_id_for_error}@{version}");
        };
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
                None,
            )
        })
        .await??;
        let _ = self.events.send(event);
        self.auto_gc_restarted_artifact(&artifact.session_id).await;
        Ok(artifact)
    }

    /// Publish in-memory bytes as a binary artifact. This is the sanctioned path
    /// for daemon-produced screenshots: the source lives in daemon scratch, not
    /// in the agent worktree, and the 5 MiB cap matches `publish_artifact_file`.
    #[allow(clippy::too_many_arguments)]
    pub async fn publish_artifact_binary(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        task_id: Option<String>,
        name: &str,
        kind: &str,
        mime_type: &str,
        data: &[u8],
    ) -> Result<Artifact> {
        self.publish_artifact_binary_with_provenance(
            session_id, agent_id, agent_name, task_id, name, kind, mime_type, data, None,
        )
        .await
    }

    /// Publish binary bytes with explicit derivation metadata. Archive
    /// extraction and media transcoding use this path so the resulting
    /// artifact records its source artifact, entry path, and operation without
    /// a separate mutable metadata table.
    #[allow(clippy::too_many_arguments)]
    pub async fn publish_artifact_binary_with_provenance(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        task_id: Option<String>,
        name: &str,
        kind: &str,
        mime_type: &str,
        data: &[u8],
        provenance: Option<vp_protocol::types::ArtifactProvenance>,
    ) -> Result<Artifact> {
        if data.is_empty() {
            anyhow::bail!("binary artifact is empty");
        }
        if data.len() > 5 * 1024 * 1024 {
            anyhow::bail!("binary artifact exceeds 5 MiB");
        }
        let name = name.trim();
        if name.is_empty() || name.len() > 200 {
            anyhow::bail!("artifact name must be 1-200 characters");
        }
        let sha256 = crate::tooling::fs::sha256_hex(data);
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let agent_id = agent_id.to_string();
        let agent_name = agent_name.to_string();
        let name = name.to_string();
        let kind = kind.to_string();
        let mime_type = mime_type.to_string();
        let data = data.to_vec();
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
                provenance.as_ref(),
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
        self.budget_registry.clear_project(project_id);
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
            totals.cache_read_tokens = totals
                .cache_read_tokens
                .saturating_add(session.cache_read_tokens);
            totals.cache_write_tokens = totals
                .cache_write_tokens
                .saturating_add(session.cache_write_tokens);
            totals.cost_usd += session.cost_usd;
        }
        totals.cache_hit_ratio = vp_protocol::types::cache_hit_ratio(
            totals.input_tokens,
            totals.cache_read_tokens,
            totals.cache_write_tokens,
        );
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
        self.create_memory_with_metadata(
            session_id,
            agent_id,
            agent_name,
            kind,
            title,
            body,
            tags,
            "agent",
            Vec::new(),
            false,
            None,
        )
        .await
    }

    /// Create a memory with explicit provenance. The policy's
    /// `auto_supersede` and verify TTLs apply; duplicate content is idempotent.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_memory_with_metadata(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        kind: &str,
        title: &str,
        body: &str,
        tags: Vec<String>,
        source: &str,
        evidence: Vec<MemoryEvidence>,
        pinned: bool,
        verify_after: Option<String>,
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
        if source.trim().is_empty() || source.chars().count() > 40 {
            anyhow::bail!("memory source must be 1-40 characters");
        }
        if evidence.len() > 64 {
            anyhow::bail!("memory supports at most 64 evidence entries");
        }
        for item in &evidence {
            if item.kind.trim().is_empty()
                || item.kind.chars().count() > 40
                || item.reference.trim().is_empty()
                || item.reference.chars().count() > 4_096
            {
                anyhow::bail!(
                    "memory evidence requires a 1-40 char kind and 1-4096 char reference"
                );
            }
        }
        let store = self.store.clone();
        let session_id_owned = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&session_id_owned))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        if session.status == "imported" {
            anyhow::bail!("imported sessions are read-only archives");
        }
        let store = self.store.clone();
        let policy_project = session.project_id.clone();
        let policy =
            tokio::task::spawn_blocking(move || store.get_memory_policy(&policy_project)).await??;
        let verify_days = if source == "task_result" {
            policy.task_result_verify_days
        } else {
            policy.default_verify_days
        };
        let verify_after = verify_after.or_else(|| memory_verify_after(verify_days));
        let store = self.store.clone();
        let session_id = session.id;
        let project_id = session.project_id;
        let agent_id = agent_id.to_string();
        let agent_name = agent_name.to_string();
        let kind = kind.to_string();
        let title = title.to_string();
        let body = body.to_string();
        let source = source.to_string();
        let outcome = tokio::task::spawn_blocking(move || {
            store.create_memory_with_metadata_event(NewMemory {
                session_id: &session_id,
                project_id: &project_id,
                agent_id: &agent_id,
                agent_name: &agent_name,
                kind: &kind,
                title: &title,
                body: &body,
                tags: &tags,
                source: &source,
                evidence: &evidence,
                pinned,
                verify_after: verify_after.as_deref(),
                dedupe: true,
                auto_supersede: policy.auto_supersede,
            })
        })
        .await??;
        for event in outcome.events {
            let _ = self.events.send(event);
        }
        Ok(outcome.memory)
    }

    #[allow(clippy::too_many_arguments)]
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
        self.list_memories_filtered(project_id, query, kind, None, false, limit)
            .await
    }

    /// Active memories by default; exact `status` wins over `include_stale`.
    #[allow(clippy::too_many_arguments)]
    pub async fn list_memories_filtered(
        &self,
        project_id: &str,
        query: Option<String>,
        kind: Option<String>,
        status: Option<String>,
        include_stale: bool,
        limit: u32,
    ) -> Result<MemoryListResult> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        let memories = tokio::task::spawn_blocking(move || {
            store.list_memories_filtered(
                &project_id,
                query.as_deref(),
                kind.as_deref(),
                status.as_deref(),
                include_stale,
                limit,
            )
        })
        .await??;
        Ok(MemoryListResult { memories })
    }

    pub async fn memory_search(&self, params: MemorySearchParams) -> Result<MemorySearchResult> {
        let query_text = params.query.trim().to_string();
        let store = self.store.clone();
        let project_id = params.project_id.clone();
        let kind = params.kind.clone();
        let kinds = params.kinds.clone();
        let include_stale = params.include_stale;
        let limit = params.limit.unwrap_or(20).clamp(1, 100);
        let lexical_query = query_text.clone();
        let mut candidates = tokio::task::spawn_blocking(move || {
            store.memory_candidates(
                &project_id,
                &lexical_query,
                kind.as_deref(),
                &kinds,
                include_stale,
                limit,
            )
        })
        .await??;
        if let Some(task_id) = params
            .task_id
            .as_deref()
            .map(str::trim)
            .filter(|task_id| !task_id.is_empty())
        {
            let store = self.store.clone();
            let project_id = params.project_id.clone();
            let task_id = task_id.to_string();
            let extra = tokio::task::spawn_blocking(move || {
                store.memory_task_candidates(&project_id, &task_id, include_stale, limit)
            })
            .await??;
            let mut seen = candidates
                .iter()
                .map(|candidate| candidate.memory.id.clone())
                .collect::<HashSet<_>>();
            for memory in extra {
                if seen.insert(memory.id.clone()) {
                    candidates.push(MemoryCandidate {
                        memory,
                        lexical: 0.0,
                        fts: false,
                        semantic: None,
                    });
                }
            }
        }

        // The semantic lane is opt-in twice: the project policy must have an
        // embedding object, and that object must be enabled and configured. A
        // disabled policy never reads the key, queries the store, or opens a
        // socket.
        let memory_policy = self.get_memory_policy(&params.project_id).await?.policy;
        let embedding_policy = if params.lexical_only {
            None
        } else {
            memory_policy.embedding.clone()
        };
        let mut semantic_summary = MemorySemanticSummary {
            enabled: embedding_policy
                .as_ref()
                .is_some_and(|policy| policy.enabled && policy.is_configured()),
            model: embedding_policy
                .as_ref()
                .filter(|policy| policy.enabled)
                .and_then(|policy| policy.model.clone()),
            lexical_candidates: u32::try_from(candidates.len()).unwrap_or(u32::MAX),
            semantic_candidates: 0,
            fallback: None,
            index_source: None,
            index_generation: None,
            index_fallback: None,
            probed_clusters: 0,
            candidate_vectors: 0,
        };
        let mut semantic_ranking = crate::memory::SemanticRanking::Disabled;
        if let Some(policy) = embedding_policy.as_ref().filter(|policy| policy.enabled) {
            let unavailable = if !policy.is_configured() {
                Some("embedding policy is incomplete".to_string())
            } else if query_text.is_empty() {
                Some("query text is empty".to_string())
            } else {
                None
            };
            if let Some(reason) = unavailable {
                semantic_summary.fallback = Some(reason.clone());
                semantic_ranking = crate::memory::SemanticRanking::Unavailable(reason);
            } else {
                match crate::memory_embedding::EmbeddingClient::from_policy(policy) {
                    Err(error) => {
                        let reason =
                            crate::memory_embedding::sanitize_embedding_error(&error.to_string());
                        semantic_summary.fallback = Some(reason.clone());
                        semantic_ranking = crate::memory::SemanticRanking::Unavailable(reason);
                    }
                    Ok(client) => {
                        let query_vector = crate::memory::embedding_query_text(&query_text);
                        match client.embed(std::slice::from_ref(&query_vector)).await {
                            Err(error) => {
                                let reason = crate::memory_embedding::sanitize_embedding_error(
                                    &error.to_string(),
                                );
                                semantic_summary.fallback = Some(reason.clone());
                                semantic_ranking =
                                    crate::memory::SemanticRanking::Unavailable(reason);
                            }
                            Ok(batch) => {
                                let store = self.store.clone();
                                let project = params.project_id.clone();
                                let model = policy.model.clone().unwrap_or_default();
                                let usage = batch.usage.clone();
                                if usage.calls > 0 {
                                    let _ = tokio::task::spawn_blocking(move || {
                                        store.record_memory_embedding_usage(
                                            &project,
                                            &model,
                                            usage.calls,
                                            usage.input_tokens,
                                            usage.estimated_tokens,
                                            usage.bytes,
                                            usage.provider_reported,
                                        )
                                    })
                                    .await;
                                }
                                let query_vector =
                                    batch.items.first().and_then(|item| item.vector.clone());
                                let item_error =
                                    batch.items.first().and_then(|item| item.error.clone());
                                match query_vector {
                                    None => {
                                        let reason = item_error.unwrap_or_else(|| {
                                            "embedding endpoint returned no query vector"
                                                .to_string()
                                        });
                                        let reason =
                                            crate::memory_embedding::sanitize_embedding_error(
                                                &reason,
                                            );
                                        semantic_summary.fallback = Some(reason.clone());
                                        semantic_ranking =
                                            crate::memory::SemanticRanking::Unavailable(reason);
                                    }
                                    Some(query_vector) => {
                                        let model = policy.model.clone().unwrap_or_default();
                                        let dimensions = policy.dimensions;
                                        let max_candidates = policy.max_candidates.clamp(1, 256);
                                        let scored = if policy.uses_ivf_index() {
                                            let store = self.store.clone();
                                            let project = params.project_id.clone();
                                            let probe_policy = policy.clone();
                                            let probe_query = query_vector.clone();
                                            let probe = tokio::task::spawn_blocking(move || {
                                                crate::memory_index::probe_ivf(
                                                    &store,
                                                    &project,
                                                    &probe_policy,
                                                    include_stale,
                                                    &probe_query,
                                                )
                                            })
                                            .await?;
                                            match probe.fallback {
                                                Some(reason) => {
                                                    let store = self.store.clone();
                                                    let project = params.project_id.clone();
                                                    let fallback_model = model.clone();
                                                    let nlist = policy.ivf_nlist;
                                                    let reason_for_log = reason.clone();
                                                    let _ = tokio::task::spawn_blocking(move || {
                                                        store.note_memory_embedding_index_fallback(
                                                            &project,
                                                            &fallback_model,
                                                            dimensions,
                                                            nlist,
                                                            &reason_for_log,
                                                        )
                                                    })
                                                    .await;
                                                    semantic_summary.index_source =
                                                        Some("scan".to_string());
                                                    semantic_summary.index_generation =
                                                        probe.generation;
                                                    semantic_summary.index_fallback = Some(reason);
                                                    semantic_summary.probed_clusters = 0;
                                                    semantic_summary.candidate_vectors = 0;
                                                    self.scan_semantic_candidates(
                                                        &params.project_id,
                                                        &model,
                                                        dimensions,
                                                        include_stale,
                                                        max_candidates,
                                                        &query_vector,
                                                        f64::from(policy.min_similarity),
                                                    )
                                                    .await?
                                                }
                                                None => {
                                                    semantic_summary.index_source =
                                                        Some("ivf".to_string());
                                                    semantic_summary.index_generation =
                                                        probe.generation;
                                                    semantic_summary.index_fallback = None;
                                                    semantic_summary.probed_clusters =
                                                        probe.probed_clusters;
                                                    semantic_summary.candidate_vectors =
                                                        probe.candidate_vectors;
                                                    probe.scored
                                                }
                                            }
                                        } else {
                                            semantic_summary.index_source =
                                                Some("scan".to_string());
                                            semantic_summary.index_generation = None;
                                            semantic_summary.index_fallback = None;
                                            semantic_summary.probed_clusters = 0;
                                            semantic_summary.candidate_vectors = 0;
                                            self.scan_semantic_candidates(
                                                &params.project_id,
                                                &model,
                                                dimensions,
                                                include_stale,
                                                max_candidates,
                                                &query_vector,
                                                f64::from(policy.min_similarity),
                                            )
                                            .await?
                                        };
                                        semantic_summary.semantic_candidates =
                                            u32::try_from(scored.len()).unwrap_or(u32::MAX);
                                        let mut semantic_by_id = scored
                                            .into_iter()
                                            .map(|(memory, score)| {
                                                (memory.id.clone(), (memory, score))
                                            })
                                            .collect::<std::collections::HashMap<_, _>>();
                                        for candidate in &mut candidates {
                                            if let Some((_, score)) =
                                                semantic_by_id.remove(&candidate.memory.id)
                                            {
                                                candidate.semantic = Some(score);
                                            }
                                        }
                                        candidates.extend(semantic_by_id.into_values().map(
                                            |(memory, score)| MemoryCandidate {
                                                memory,
                                                lexical: 0.0,
                                                fts: false,
                                                semantic: Some(score),
                                            },
                                        ));
                                        semantic_ranking = crate::memory::SemanticRanking::Active;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let query = crate::memory::MemoryQuery {
            text: params.query,
            task_id: params.task_id,
            department: params.department,
            paths: params.paths,
        };
        let mut matches = crate::memory::rank_candidates_with_semantics(
            candidates,
            &query,
            time::OffsetDateTime::now_utc(),
            include_stale,
            semantic_ranking,
        );
        if let Some(max_chars) = params.max_chars {
            matches = crate::memory::select_brief(&matches, max_chars).memories;
        }
        matches.truncate(limit as usize);
        Ok(MemorySearchResult {
            memories: matches,
            semantic: Some(semantic_summary),
        })
    }

    /// Exact semantic scan used by the default lane and by every IVF fallback.
    /// The candidate window is bounded by the policy and decoded/scored by the
    /// same function as the ANN probe, so a fallback cannot change cosine
    /// semantics.
    #[allow(clippy::too_many_arguments)]
    async fn scan_semantic_candidates(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        include_stale: bool,
        max_candidates: u32,
        query_vector: &[f32],
        min_similarity: f64,
    ) -> Result<Vec<(MemoryItem, f64)>> {
        let store = self.store.clone();
        let project = project_id.to_string();
        let model = model.to_string();
        let semantic_candidates = tokio::task::spawn_blocking(move || {
            store.memory_semantic_candidates(
                &project,
                &model,
                dimensions,
                include_stale,
                max_candidates,
            )
        })
        .await??;
        Ok(crate::memory::semantic_scores(
            semantic_candidates,
            query_vector,
            dimensions,
            min_similarity,
        ))
    }

    /// Build the injected project-memory brief. Usage is observed per selected
    /// memory and session, and `memory.used` is idempotent for that pair.
    pub async fn memory_brief(
        &self,
        session_id: &str,
        task_id: Option<&str>,
    ) -> Result<MemoryBriefResult> {
        self.memory_brief_with_department(session_id, task_id, None)
            .await
    }

    /// Same as `memory_brief`, with an explicit department override for agents
    /// that carry a department but no plan-task row.
    pub async fn memory_brief_with_department(
        &self,
        session_id: &str,
        task_id: Option<&str>,
        department_override: Option<&str>,
    ) -> Result<MemoryBriefResult> {
        let store = self.store.clone();
        let lookup = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&lookup))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        let project_id = session.project_id.clone();
        let policy = self.get_memory_policy(&project_id).await?.policy;
        let brief_max_chars = policy.brief_max_chars.unwrap_or(4_000).clamp(200, 100_000);

        let mut text_parts = vec![session.goal.clone()];
        if let Some(task_id) = task_id {
            text_parts.push(task_id.to_string());
        }
        let mut department = department_override
            .map(str::trim)
            .filter(|department| !department.is_empty())
            .map(str::to_string);
        let mut paths = Vec::new();
        if let Some(task_id) = task_id {
            if let Some(plan) = self.get_team_plan(session_id).await?
                && let Some(task) = plan.tasks.iter().find(|task| task.id == task_id)
            {
                if !task.title.trim().is_empty() {
                    text_parts.push(task.title.clone());
                }
                text_parts.push(task.department.clone());
                text_parts.extend(task.write_scope.iter().cloned());
                text_parts.extend(task.acceptance.iter().cloned());
                department = Some(task.department.clone());
                paths = task.write_scope.clone();
            }
            if department.is_none() {
                let store = self.store.clone();
                let lookup = session_id.to_string();
                if let Ok(tasks) =
                    tokio::task::spawn_blocking(move || store.list_plan_tasks(None, Some(&lookup)))
                        .await?
                    && let Some(task) = tasks.into_iter().find(|task| task.task_id == task_id)
                {
                    if !task.title.trim().is_empty() {
                        text_parts.push(task.title);
                    }
                    text_parts.push(task.department.clone());
                    text_parts.extend(task.write_scope.iter().cloned());
                    department = Some(task.department);
                    paths = task.write_scope;
                }
            }
        }

        let search = MemorySearchParams {
            project_id: project_id.clone(),
            query: text_parts.join(" "),
            kind: None,
            kinds: Vec::new(),
            task_id: task_id.map(str::to_string),
            department,
            paths,
            limit: Some(20),
            include_stale: false,
            max_chars: None,
            lexical_only: false,
        };
        let mut matches = self.memory_search(search).await?.memories;

        if matches.is_empty() {
            let store = self.store.clone();
            let project = project_id.clone();
            let recent = tokio::task::spawn_blocking(move || {
                store.list_memories_filtered(&project, None, None, None, false, 8)
            })
            .await??;
            matches = recent
                .into_iter()
                .map(|memory| MemoryMatch {
                    memory,
                    score: 0.0,
                    reasons: vec!["recent-fallback".to_string()],
                    lexical_score: Some(0.0),
                    semantic_score: None,
                    source: Some("structural".to_string()),
                    semantic_unavailable: None,
                })
                .collect();
        }

        let selection = crate::memory::select_brief(&matches, brief_max_chars);
        for matched in &selection.memories {
            let store = self.store.clone();
            let memory_id = matched.memory.id.clone();
            let using_session = session_id.to_string();
            if let Ok(Some((_, event))) = tokio::task::spawn_blocking(move || {
                store.record_memory_use_with_event(&memory_id, &using_session)
            })
            .await
            .unwrap_or(Ok(None))
            {
                let _ = self.events.send(event);
            }
        }
        Ok(MemoryBriefResult {
            text: selection.text,
            memories: selection.memories,
        })
    }

    /// Update lifecycle metadata only. Title and body are immutable so
    /// provenance cannot be rewritten after the fact.
    pub async fn memory_update(
        &self,
        id: &str,
        status: Option<&str>,
        pinned: Option<bool>,
        tags: Option<Vec<String>>,
    ) -> Result<MemoryUpdateResult> {
        if let Some(status) = status
            && !matches!(status, "active" | "stale" | "superseded" | "archived")
        {
            anyhow::bail!("memory status must be active, stale, superseded, or archived");
        }
        if status.is_none() && pinned.is_none() && tags.is_none() {
            anyhow::bail!("memory update requires status, pinned, or tags");
        }
        if let Some(tags) = &tags
            && (tags.len() > 10 || tags.iter().any(|tag| tag.chars().count() > 40))
        {
            anyhow::bail!("at most 10 tags of 40 characters are allowed");
        }
        let store = self.store.clone();
        let memory_id = id.to_string();
        let status = status.map(str::to_string);
        let tags_for_store = tags.clone();
        let updated = tokio::task::spawn_blocking(move || {
            store.update_memory_with_event(
                &memory_id,
                status.as_deref(),
                pinned,
                tags_for_store.as_deref(),
            )
        })
        .await??;
        let Some((memory, event)) = updated else {
            anyhow::bail!("memory not found: {id}");
        };
        let _ = self.events.send(event);
        Ok(MemoryUpdateResult { memory })
    }

    /// Check evidence locally, then explicitly verify or mark stale. This never
    /// asks a model to judge whether a claim is true.
    pub async fn memory_verify(&self, id: &str) -> Result<MemoryVerifyResult> {
        let store = self.store.clone();
        let memory_id = id.to_string();
        let memory = tokio::task::spawn_blocking(move || store.get_memory(&memory_id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("memory not found: {id}"))?;
        if matches!(memory.status.as_str(), "superseded" | "archived") {
            anyhow::bail!(
                "memory {id} is {}; restore it with memory.update before verifying",
                memory.status
            );
        }
        let store = self.store.clone();
        let project_id = memory.project_id.clone();
        let project = tokio::task::spawn_blocking(move || store.get_project(&project_id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("project not found for memory: {id}"))?;
        let root = PathBuf::from(&project.root);
        let store = self.store.clone();
        let session_id = memory.session_id.clone();
        let project_id = memory.project_id.clone();
        let evidence = memory.evidence.clone();
        let checks = tokio::task::spawn_blocking(move || {
            verify_memory_evidence(&store, &root, &project_id, &session_id, &evidence)
        })
        .await?;

        let failed = checks
            .iter()
            .find(|check| check.status == "missing")
            .map(|check| {
                format!(
                    "evidence verification failed: {} {}: {}",
                    check.evidence.kind, check.evidence.reference, check.detail
                )
            });
        let checked = checks
            .iter()
            .filter(|check| check.status != "unchecked")
            .count();
        let store = self.store.clone();
        let memory_id = id.to_string();
        let mut memory = memory;
        let mut event = None;
        if let Some(reason) = failed {
            let updated = tokio::task::spawn_blocking(move || {
                store.mark_memory_stale_with_event(&memory_id, &reason)
            })
            .await??;
            let Some((stale, stale_event)) = updated else {
                anyhow::bail!("memory not found: {id}");
            };
            memory = stale;
            event = Some(stale_event);
        } else if checked > 0 || checks.is_empty() {
            // A memory with only unchecked evidence (for example a URL) is not
            // silently treated as verified.
            let policy = self.get_memory_policy(&memory.project_id).await?.policy;
            let verified_at = now_rfc3339();
            let verify_after = memory_verify_after(policy.default_verify_days);
            let updated = tokio::task::spawn_blocking(move || {
                store.mark_memory_verified_with_event(
                    &memory_id,
                    &verified_at,
                    verify_after.as_deref(),
                )
            })
            .await??;
            let Some((verified, verified_event)) = updated else {
                anyhow::bail!("memory not found: {id}");
            };
            memory = verified;
            event = Some(verified_event);
        }
        if let Some(event) = event {
            let _ = self.events.send(event);
        }
        Ok(MemoryVerifyResult { memory, checks })
    }

    /// Mark expired active, non-pinned memories stale. Never deletes and never
    /// restores. `dry_run` returns candidates without mutating.
    pub async fn memory_sweep(
        &self,
        project_id: &str,
        older_than_days: Option<u32>,
        dry_run: bool,
    ) -> Result<MemorySweepResult> {
        if let Some(days) = older_than_days
            && !(1..=36_500).contains(&days)
        {
            anyhow::bail!("memory sweep older_than_days must be between 1 and 36500");
        }
        let store = self.store.clone();
        let project = project_id.to_string();
        let candidates = tokio::task::spawn_blocking(move || {
            store.memory_sweep_candidates(&project, older_than_days)
        })
        .await??;
        let memory_ids = candidates
            .iter()
            .map(|memory| memory.id.clone())
            .collect::<Vec<_>>();
        let mut marked_stale = 0;
        if !dry_run {
            for id in &memory_ids {
                let store = self.store.clone();
                let memory_id = id.clone();
                let updated = tokio::task::spawn_blocking(move || {
                    store.mark_memory_stale_with_event(&memory_id, "verification expired")
                })
                .await??;
                if let Some((_, event)) = updated {
                    let _ = self.events.send(event);
                    marked_stale += 1;
                }
            }
        }
        Ok(MemorySweepResult {
            project_id: project_id.to_string(),
            candidates: memory_ids.len(),
            marked_stale,
            memory_ids,
            dry_run,
        })
    }

    pub async fn memory_stats(&self, project_id: &str) -> Result<MemoryStatsResult> {
        let store = self.store.clone();
        let project = project_id.to_string();
        let stats = tokio::task::spawn_blocking(move || store.memory_stats(&project)).await??;
        Ok(MemoryStatsResult {
            project_id: project_id.to_string(),
            total: stats.total,
            by_status: stats.by_status,
            by_source: stats.by_source,
            by_kind: stats.by_kind,
        })
    }

    pub async fn memory_conflicts(
        &self,
        project_id: &str,
        status: Option<&str>,
        limit: Option<u32>,
    ) -> Result<MemoryConflictsResult> {
        if let Some(status) = status
            && !matches!(status, "open" | "resolved")
        {
            anyhow::bail!("memory conflict status must be open or resolved");
        }
        self.ensure_project_exists(project_id).await?;
        let store = self.store.clone();
        let project = project_id.to_string();
        let status = status.map(str::to_string);
        let conflicts = tokio::task::spawn_blocking(move || {
            store.list_memory_conflicts(&project, status.as_deref(), limit)
        })
        .await??;
        Ok(MemoryConflictsResult {
            project_id: project_id.to_string(),
            conflicts,
        })
    }

    /// Deterministic detection pass. Never auto-resolves and never changes a
    /// memory lifecycle status.
    pub async fn memory_conflict_scan(&self, project_id: &str) -> Result<MemoryConflictScanResult> {
        self.ensure_project_exists(project_id).await?;
        let store = self.store.clone();
        let project = project_id.to_string();
        let scan =
            tokio::task::spawn_blocking(move || store.scan_memory_conflicts(&project)).await??;
        for event in &scan.events {
            let _ = self.events.send(event.clone());
        }
        Ok(MemoryConflictScanResult {
            project_id: project_id.to_string(),
            detected: u32::try_from(scan.new.len()).unwrap_or(u32::MAX),
            open: u32::try_from(scan.open.len()).unwrap_or(u32::MAX),
            conflicts: scan.open,
        })
    }

    pub async fn memory_conflict_resolve(
        &self,
        id: &str,
        action: &str,
        note: Option<&str>,
    ) -> Result<MemoryConflictResolveResult> {
        if !matches!(action, "keep_a" | "keep_b" | "keep_both" | "archive_both") {
            anyhow::bail!(
                "memory conflict action must be keep_a, keep_b, keep_both, or archive_both"
            );
        }
        let store = self.store.clone();
        let conflict_id = id.to_string();
        let conflict = tokio::task::spawn_blocking(move || store.get_memory_conflict(&conflict_id))
            .await??
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "memory conflict {id} not found: memory no longer exists or the conflict was removed with a deleted memory"
                )
            })?;
        let project_id = conflict.project_id.clone();
        let store = self.store.clone();
        let conflict_id = id.to_string();
        let action = action.to_string();
        let note = note
            .map(str::trim)
            .filter(|note| !note.is_empty())
            .map(str::to_string);
        let resolution = tokio::task::spawn_blocking(move || {
            store.resolve_memory_conflict(&project_id, &conflict_id, &action, note.as_deref())
        })
        .await??;
        for event in &resolution.events {
            let _ = self.events.send(event.clone());
        }
        Ok(MemoryConflictResolveResult {
            conflict: resolution.conflict,
            memories: resolution.memories,
        })
    }

    pub async fn memory_health(&self, project_id: &str) -> Result<MemoryHealthResult> {
        self.ensure_project_exists(project_id).await?;
        let store = self.store.clone();
        let project = project_id.to_string();
        tokio::task::spawn_blocking(move || store.memory_health(&project)).await?
    }

    /// Coverage/health for the optional semantic lane. Reads only derived
    /// tables and the project policy; never returns vectors or credentials.
    pub async fn memory_embedding_status(
        &self,
        project_id: &str,
    ) -> Result<MemoryEmbeddingsStatusResult> {
        self.ensure_project_exists(project_id).await?;
        let policy = self.get_memory_policy(project_id).await?.policy;
        let embedding = policy.embedding;
        let store = self.store.clone();
        let project = project_id.to_string();
        let embedding_for_store = embedding.clone();
        let mut status = tokio::task::spawn_blocking(move || {
            store.memory_embedding_status(&project, embedding_for_store.as_ref())
        })
        .await??;
        if let Some(policy) = embedding.as_ref()
            && policy.enabled
            && policy.is_configured()
            && let Some(name) = policy
                .api_key_env
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
        {
            let missing = std::env::var(name)
                .ok()
                .is_none_or(|value| value.trim().is_empty());
            if missing {
                status.last_error = Some(format!(
                    "embedding API key environment variable {name} is not set"
                ));
            }
        }
        let store = self.store.clone();
        let project = project_id.to_string();
        let index_policy = embedding.clone();
        let index = tokio::task::spawn_blocking(move || {
            crate::memory_index::index_status(&store, &project, index_policy.as_ref())
        })
        .await??;
        status.index = Some(index);
        Ok(status)
    }

    /// Derived IVF index health for one project. Contains aggregate counts and
    /// generation metadata only; centroids and vectors never leave the store.
    pub async fn memory_embedding_index_status(
        &self,
        project_id: &str,
    ) -> Result<MemoryEmbeddingsIndexStatusResult> {
        self.ensure_project_exists(project_id).await?;
        let policy = self.get_memory_policy(project_id).await?.policy;
        let store = self.store.clone();
        let project = project_id.to_string();
        let index = tokio::task::spawn_blocking(move || {
            crate::memory_index::index_status(&store, &project, policy.embedding.as_ref())
        })
        .await??;
        Ok(MemoryEmbeddingsIndexStatusResult {
            project_id: project_id.to_string(),
            index,
        })
    }

    /// Explicit derived-index rebuild. Never calls the embedding endpoint,
    /// never changes memory truth, and never deletes vectors. Scan mode or a
    /// below-minimum vector count removes stale derived index state and
    /// returns `rebuilt = false` with a reason.
    pub async fn memory_embedding_index_rebuild(
        &self,
        project_id: &str,
    ) -> Result<MemoryEmbeddingsIndexRebuildResult> {
        self.ensure_project_exists(project_id).await?;
        let policy = self.get_memory_policy(project_id).await?.policy;
        let Some(embedding) = policy.embedding else {
            return Ok(MemoryEmbeddingsIndexRebuildResult {
                project_id: project_id.to_string(),
                rebuilt: false,
                reason: Some("not_configured".to_string()),
                generation: None,
                vector_count: 0,
                assigned_vectors: 0,
                cluster_count: 0,
                nlist: 0,
            });
        };
        crate::memory_embedding::validate_embedding_policy(&embedding)?;
        let store = self.store.clone();
        let project = project_id.to_string();
        let embedding_for_build = embedding.clone();
        let report = tokio::task::spawn_blocking(move || {
            crate::memory_index::rebuild_index(&store, &project, &embedding_for_build)
        })
        .await??;
        Ok(MemoryEmbeddingsIndexRebuildResult {
            project_id: project_id.to_string(),
            rebuilt: report.rebuilt,
            reason: report.reason,
            generation: report.generation,
            vector_count: report.vector_count,
            assigned_vectors: report.assigned_vectors,
            cluster_count: report.cluster_count,
            nlist: report.nlist,
        })
    }

    /// Clear derived vectors/jobs and queue every active/stale memory. This is
    /// an explicit operator action; it never deletes memories.
    pub async fn memory_embedding_rebuild(
        &self,
        project_id: &str,
    ) -> Result<MemoryEmbeddingsRebuildResult> {
        self.ensure_project_exists(project_id).await?;
        let policy = self.get_memory_policy(project_id).await?.policy;
        let embedding = policy.embedding;
        let enabled = embedding
            .as_ref()
            .is_some_and(|embedding| embedding.enabled && embedding.is_configured());
        if let Some(embedding) = embedding.as_ref() {
            crate::memory_embedding::validate_embedding_policy(embedding)?;
        }
        // Clearing is always allowed, including while disabled or removed. This
        // is the operator's privacy/cleanup path: it removes derived vectors
        // and jobs without touching memory truth or usage accounting.
        let store = self.store.clone();
        let project = project_id.to_string();
        let cleared =
            tokio::task::spawn_blocking(move || store.clear_memory_embeddings(&project)).await??;
        let model = embedding
            .as_ref()
            .and_then(|embedding| embedding.model.clone())
            .filter(|model| !model.trim().is_empty());
        let dimensions = embedding
            .as_ref()
            .map_or(1536, |embedding| embedding.dimensions);
        let enqueued = if enabled {
            let model_for_queue = model
                .clone()
                .ok_or_else(|| anyhow::anyhow!("embedding model is missing"))?;
            let store = self.store.clone();
            let project = project_id.to_string();
            tokio::task::spawn_blocking(move || {
                store.enqueue_memory_embedding_backlog(&project, &model_for_queue)
            })
            .await??
        } else {
            0
        };
        Ok(MemoryEmbeddingsRebuildResult {
            project_id: project_id.to_string(),
            enabled,
            model,
            dimensions,
            cleared,
            enqueued,
        })
    }

    /// Explicit connectivity probe. Unlike indexing/recall this is allowed for
    /// a configured-but-disabled policy: the operator asked for one request.
    /// Toggle `enabled` when the probe succeeds. No vector is returned.
    pub async fn memory_embedding_test(
        &self,
        project_id: &str,
        text: Option<&str>,
    ) -> Result<MemoryEmbeddingsTestResult> {
        self.ensure_project_exists(project_id).await?;
        let policy = self.get_memory_policy(project_id).await?.policy;
        let Some(embedding) = policy.embedding else {
            anyhow::bail!("embedding policy is not configured");
        };
        if !embedding.is_configured() {
            anyhow::bail!("embedding policy needs endpoint and model");
        }
        crate::memory_embedding::validate_embedding_policy(&embedding)?;
        let client = crate::memory_embedding::EmbeddingClient::from_policy(&embedding)?;
        let probe = text
            .map(crate::memory::embedding_query_text)
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| "verlinpark embedding readiness probe".to_string());
        let started = std::time::Instant::now();
        let result = client.embed(std::slice::from_ref(&probe)).await;
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let model = embedding.model.clone();
        let dimensions = embedding.dimensions;
        match result {
            Ok(batch) => {
                let store = self.store.clone();
                let project = project_id.to_string();
                let usage = batch.usage.clone();
                let model_for_usage = model.clone().unwrap_or_default();
                if usage.calls > 0 {
                    let _ = tokio::task::spawn_blocking(move || {
                        store.record_memory_embedding_usage(
                            &project,
                            &model_for_usage,
                            usage.calls,
                            usage.input_tokens,
                            usage.estimated_tokens,
                            usage.bytes,
                            usage.provider_reported,
                        )
                    })
                    .await;
                }
                let item = batch.items.into_iter().next();
                let vector_dimensions = item
                    .as_ref()
                    .and_then(|item| item.vector.as_ref())
                    .map(|vector| u32::try_from(vector.len()).unwrap_or(u32::MAX));
                let error = item.and_then(|item| item.error);
                Ok(MemoryEmbeddingsTestResult {
                    project_id: project_id.to_string(),
                    ok: vector_dimensions.is_some(),
                    model,
                    dimensions: vector_dimensions.unwrap_or(dimensions),
                    latency_ms,
                    input_tokens: batch.usage.input_tokens,
                    estimated_tokens: batch.usage.estimated_tokens,
                    provider_reported: batch.usage.provider_reported,
                    error: error
                        .map(|error| crate::memory_embedding::sanitize_embedding_error(&error)),
                })
            }
            Err(error) => Ok(MemoryEmbeddingsTestResult {
                project_id: project_id.to_string(),
                ok: false,
                model,
                dimensions,
                latency_ms,
                input_tokens: 0,
                estimated_tokens: 0,
                provider_reported: false,
                error: Some(crate::memory_embedding::sanitize_embedding_error(
                    &error.to_string(),
                )),
            }),
        }
    }

    /// Daemon reconciler: process one bounded batch for each enabled project.
    /// Returns `(projects, indexed, failed, skipped)`.
    pub async fn reconcile_memory_embeddings(&self) -> Result<(usize, usize, usize, usize)> {
        let store = self.store.clone();
        let policies =
            tokio::task::spawn_blocking(move || store.list_memory_embedding_policies()).await??;
        let mut projects = 0usize;
        let mut indexed = 0usize;
        let mut failed = 0usize;
        let mut skipped = 0usize;
        for (project_id, policy) in policies {
            if !policy.enabled || !policy.is_configured() {
                continue;
            }
            match self
                .reconcile_project_memory_embeddings(&project_id, policy)
                .await
            {
                Ok((did_index, did_fail, did_skip)) => {
                    projects += 1;
                    indexed += did_index;
                    failed += did_fail;
                    skipped += did_skip;
                }
                Err(error) => {
                    tracing::warn!(%error, %project_id, "memory embedding reconciliation failed");
                }
            }
        }
        Ok((projects, indexed, failed, skipped))
    }

    /// One bounded indexing batch. Content hashes are recomputed before any
    /// network call; unchanged rows transition to `skipped` without a request.
    pub async fn reconcile_project_memory_embeddings(
        &self,
        project_id: &str,
        policy: MemoryEmbeddingPolicy,
    ) -> Result<(usize, usize, usize)> {
        if !policy.enabled {
            return Ok((0, 0, 0));
        }
        // Index maintenance does not depend on the embedding endpoint. Run one
        // bounded pass before the client is built so a corrupt/stale index can
        // still repair itself while the endpoint or API key is unavailable.
        self.maintain_project_memory_index(project_id, &policy)
            .await;
        let runtime = crate::memory_embedding::EmbeddingRuntimePolicy::from_policy(&policy)?;
        let client = match crate::memory_embedding::EmbeddingClient::from_policy(&policy) {
            Ok(client) => client,
            Err(error) => {
                // Configuration failures are retried after the operator fixes
                // them. They are not recorded as per-memory embedding failures,
                // so a missing key does not burn every job's retry budget.
                return Err(anyhow::anyhow!("{error}"));
            }
        };
        let model = runtime.model.clone();
        let dimensions = runtime.dimensions;
        let scan_limit = runtime
            .batch_size
            .saturating_mul(2)
            .clamp(runtime.batch_size, 128);
        let store = self.store.clone();
        let project = project_id.to_string();
        let model_for_scan = model.clone();
        let candidates = tokio::task::spawn_blocking(move || {
            store.memory_embedding_index_candidates(
                &project,
                &model_for_scan,
                dimensions,
                crate::memory_embedding::MAX_EMBEDDING_INDEX_ATTEMPTS,
                scan_limit,
            )
        })
        .await??;

        let mut needs = Vec::new();
        let mut skipped = 0usize;
        for candidate in candidates {
            let text = crate::memory::embedding_text(&candidate.memory);
            let content_hash = crate::memory::embedding_content_hash(&text);
            let current = candidate.embedded_model.as_deref() == Some(model.as_str())
                && candidate.embedded_dimensions == Some(dimensions)
                && candidate.embedded_content_hash.as_deref() == Some(content_hash.as_str());
            if current {
                let store = self.store.clone();
                let memory_id = candidate.memory.id.clone();
                let model = model.clone();
                if let Ok(Ok(Some(event))) = tokio::task::spawn_blocking(move || {
                    store.record_memory_embedding_skip(&memory_id, &model, "content hash unchanged")
                })
                .await
                {
                    let _ = self.events.send(event);
                }
                skipped += 1;
                continue;
            }
            let reset_attempts = candidate
                .job_model
                .as_deref()
                .is_some_and(|job_model| job_model != model)
                || candidate
                    .embedded_model
                    .as_deref()
                    .is_some_and(|embedded_model| embedded_model != model)
                || candidate
                    .embedded_dimensions
                    .is_some_and(|embedded_dimensions| embedded_dimensions != dimensions);
            needs.push((candidate.memory.id, text, content_hash, reset_attempts));
            if needs.len() >= runtime.batch_size as usize {
                break;
            }
        }
        if needs.is_empty() {
            self.maintain_project_memory_index(project_id, &policy)
                .await;
            return Ok((0, 0, skipped));
        }

        let mut attempted = Vec::with_capacity(needs.len());
        for (memory_id, _text, _content_hash, reset_attempts) in &needs {
            let store = self.store.clone();
            let memory_id = memory_id.clone();
            let model = model.clone();
            let reset_attempts = *reset_attempts;
            let attempts = tokio::task::spawn_blocking(move || {
                store.mark_memory_embedding_attempted(&memory_id, &model, reset_attempts)
            })
            .await??;
            attempted.push(attempts);
        }

        let texts = needs
            .iter()
            .map(|(_, text, _, _)| text.clone())
            .collect::<Vec<_>>();
        let result = client.embed(&texts).await;
        let mut indexed = 0usize;
        let mut failed = 0usize;
        let mut failure_events = Vec::new();
        match result {
            Ok(batch) => {
                let store = self.store.clone();
                let project = project_id.to_string();
                let usage = batch.usage.clone();
                let model_for_usage = model.clone();
                if usage.calls > 0 {
                    let _ = tokio::task::spawn_blocking(move || {
                        store.record_memory_embedding_usage(
                            &project,
                            &model_for_usage,
                            usage.calls,
                            usage.input_tokens,
                            usage.estimated_tokens,
                            usage.bytes,
                            usage.provider_reported,
                        )
                    })
                    .await;
                }
                for (index, (memory_id, _, content_hash, _)) in needs.iter().enumerate() {
                    let item = batch.items.get(index);
                    let vector = item.and_then(|item| item.vector.as_ref());
                    let item_error = item.and_then(|item| item.error.clone());
                    match vector {
                        Some(vector) => match crate::memory::encode_embedding_vector(vector) {
                            Ok(encoded) => {
                                let store = self.store.clone();
                                let memory_id = memory_id.clone();
                                let model = model.clone();
                                let content_hash = content_hash.clone();
                                if let Ok(Ok(Some(event))) =
                                    tokio::task::spawn_blocking(move || {
                                        store.record_memory_embedding_success(
                                            &memory_id,
                                            &model,
                                            dimensions,
                                            &content_hash,
                                            &encoded,
                                        )
                                    })
                                    .await
                                {
                                    let _ = self.events.send(event);
                                    indexed += 1;
                                } else {
                                    failed += 1;
                                }
                            }
                            Err(error) => {
                                let error = crate::memory_embedding::sanitize_embedding_error(
                                    &error.to_string(),
                                );
                                if let Some(event) = self
                                    .record_embedding_failure(
                                        memory_id,
                                        &model,
                                        attempted[index],
                                        &error,
                                        false,
                                    )
                                    .await?
                                {
                                    let _ = self.events.send(event);
                                }
                                failed += 1;
                            }
                        },
                        None => {
                            let error = item_error.unwrap_or_else(|| {
                                "embedding endpoint returned no vector".to_string()
                            });
                            let error = crate::memory_embedding::sanitize_embedding_error(&error);
                            if let Some(event) = self
                                .record_embedding_failure(
                                    memory_id,
                                    &model,
                                    attempted[index],
                                    &error,
                                    false,
                                )
                                .await?
                            {
                                failure_events.push(event);
                            }
                            failed += 1;
                        }
                    }
                }
                for event in failure_events {
                    let _ = self.events.send(event);
                }
            }
            Err(error) => {
                let terminal = !error.retryable_http();
                let error = crate::memory_embedding::sanitize_embedding_error(&error.to_string());
                for (index, (memory_id, _, _, _)) in needs.iter().enumerate() {
                    if let Some(event) = self
                        .record_embedding_failure(
                            memory_id,
                            &model,
                            attempted[index],
                            &error,
                            terminal,
                        )
                        .await?
                    {
                        failure_events.push(event);
                    }
                    failed += 1;
                }
                for event in failure_events {
                    let _ = self.events.send(event);
                }
            }
        }
        self.maintain_project_memory_index(project_id, &policy)
            .await;
        Ok((indexed, failed, skipped))
    }

    /// Look up the project embedding policy and run bounded index
    /// maintenance. Used after delete/prune so assignments and centroid
    /// metadata cannot drift from the live vector set.
    async fn maintain_memory_index_if_configured(&self, project_id: &str) {
        match self.get_memory_policy(project_id).await {
            Ok(policy) => {
                if let Some(embedding) = policy.policy.embedding {
                    self.maintain_project_memory_index(project_id, &embedding)
                        .await;
                }
            }
            Err(error) => {
                tracing::warn!(%error, %project_id, "could not load memory policy for index maintenance");
            }
        }
    }

    /// Bounded derived-index maintenance after an embedding batch. This is
    /// best-effort: a failed index build never fails memory writes or the
    /// embedding reconciler, and recall falls back to the exact scan.
    async fn maintain_project_memory_index(
        &self,
        project_id: &str,
        policy: &MemoryEmbeddingPolicy,
    ) {
        if !policy.uses_ivf_index() {
            return;
        }
        let store = self.store.clone();
        let project = project_id.to_string();
        let policy = policy.clone();
        match tokio::task::spawn_blocking(move || {
            crate::memory_index::maintain_index(&store, &project, &policy)
        })
        .await
        {
            Ok(Ok(report)) => {
                if let Some(generation) = report.rebuilt_generation {
                    tracing::info!(
                        %project_id,
                        generation,
                        assigned = report.assigned_vectors,
                        vectors = report.vector_count,
                        "memory embedding IVF index rebuilt"
                    );
                }
            }
            Ok(Err(error)) => {
                tracing::warn!(%error, %project_id, "memory embedding IVF index maintenance failed");
            }
            Err(error) => {
                tracing::warn!(%error, %project_id, "memory embedding IVF index task failed");
            }
        }
    }

    async fn record_embedding_failure(
        &self,
        memory_id: &str,
        model: &str,
        attempts: u32,
        error: &str,
        terminal: bool,
    ) -> Result<Option<Event>> {
        let next_attempt_at =
            if terminal || attempts >= crate::memory_embedding::MAX_EMBEDDING_INDEX_ATTEMPTS {
                None
            } else {
                let delay = crate::memory_embedding::index_retry_delay(attempts);
                Some(
                    (time::OffsetDateTime::now_utc()
                        + time::Duration::seconds(i64::try_from(delay.as_secs()).unwrap_or(3_600)))
                    .format(&time::format_description::well_known::Rfc3339)
                    .unwrap_or_else(|_| vp_protocol::now_rfc3339()),
                )
            };
        let store = self.store.clone();
        let memory_id = memory_id.to_string();
        let model = model.to_string();
        let error = error.to_string();
        tokio::task::spawn_blocking(move || {
            store.record_memory_embedding_failure(
                &memory_id,
                &model,
                attempts,
                &error,
                next_attempt_at.as_deref(),
            )
        })
        .await?
    }

    async fn ensure_project_exists(&self, project_id: &str) -> Result<()> {
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

    /// Daemon tick: detect conflicts for every project. Detection only; it
    /// never resolves a conflict or changes a memory status.
    pub async fn scan_all_memory_conflicts(&self) -> Result<(usize, usize, usize)> {
        let projects = self.list_projects().await?;
        let mut projects_scanned = 0usize;
        let mut detected = 0usize;
        let mut open = 0usize;
        for project in projects {
            match self.memory_conflict_scan(&project.id).await {
                Ok(result) => {
                    projects_scanned += 1;
                    detected += result.detected as usize;
                    open += result.open as usize;
                    if result.detected > 0 {
                        tracing::info!(
                            project_id = %project.id,
                            detected = result.detected,
                            open = result.open,
                            "memory conflict scan detected new same-scope conflicts"
                        );
                    } else {
                        tracing::debug!(
                            project_id = %project.id,
                            detected = result.detected,
                            open = result.open,
                            "memory conflict scan complete"
                        );
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, project_id = %project.id, "memory conflict scan failed");
                }
            }
        }
        Ok((projects_scanned, detected, open))
    }

    /// Daemon tick: sweep only projects whose policy opted into `sweep_stale`.
    pub async fn sweep_memory_staleness(&self) -> Result<(usize, usize)> {
        let store = self.store.clone();
        let policies =
            tokio::task::spawn_blocking(move || store.list_memory_sweep_policies()).await??;
        let mut projects = 0;
        let mut marked = 0;
        for (project_id, policy) in policies {
            if !policy.sweep_stale {
                continue;
            }
            match self.memory_sweep(&project_id, None, false).await {
                Ok(result) => {
                    projects += 1;
                    marked += result.marked_stale;
                }
                Err(error) => {
                    tracing::warn!(%error, %project_id, "memory staleness sweep failed");
                }
            }
        }
        Ok((projects, marked))
    }

    pub async fn delete_memory(&self, id: &str) -> Result<MemoryDeleteResult> {
        let store = self.store.clone();
        let memory_id = id.to_string();
        let lookup = memory_id.clone();
        let removed =
            tokio::task::spawn_blocking(move || store.delete_memories_with_events(&[lookup]))
                .await??;
        let project_id = removed.first().map(|(memory, _)| memory.project_id.clone());
        for (_, event) in &removed {
            let _ = self.events.send(event.clone());
        }
        if let Some(project_id) = project_id {
            self.maintain_memory_index_if_configured(&project_id).await;
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
        if !removed.is_empty() {
            self.maintain_memory_index_if_configured(project_id).await;
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
        if session.unlimited {
            let _ = writeln!(out, "- **Budget:** unlimited (economic gates bypassed)");
        }

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

        let store = self.store.clone();
        let checkpoint_session = session.id.clone();
        let context_checkpoints = tokio::task::spawn_blocking(move || {
            store.list_context_checkpoints(&checkpoint_session, 200)
        })
        .await??;
        if !context_checkpoints.is_empty() {
            let _ = writeln!(out, "\n## Context Checkpoints\n");
            let _ = writeln!(
                out,
                "Context restores rewind the active summary boundary only. They do not reset the worktree or create a run.\n"
            );
            let _ = writeln!(
                out,
                "| id | mode | plan | tokens | active? | label | created |"
            );
            let _ = writeln!(out, "|---|---|---|---|---|---|---|");
            let active_id = {
                let events = self.all_events(&session.id).await?;
                crate::context::active_context_checkpoint_id(&events)
            };
            for checkpoint in &context_checkpoints {
                let label = checkpoint
                    .label
                    .replace('|', "\\|")
                    .replace(['\n', '\r'], " ");
                let _ = writeln!(
                    out,
                    "| `{}` | {} | {} | {} → {} | {} | {} | {} |",
                    checkpoint.id,
                    checkpoint.mode,
                    checkpoint.plan_revision,
                    checkpoint.tokens_before,
                    checkpoint.tokens_after,
                    if active_id.as_deref() == Some(checkpoint.id.as_str()) {
                        "yes"
                    } else {
                        ""
                    },
                    label,
                    checkpoint.created_at
                );
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

    pub async fn count_events_of_kind(&self, session_id: &str, kind: &str) -> Result<u64> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let kind = kind.to_string();
        tokio::task::spawn_blocking(move || store.event_count_by_kind(&session_id, &kind)).await?
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
        let compaction_count = last_compaction
            .as_ref()
            .map(|summary| summary.compaction_count)
            .unwrap_or(0);
        let last_compaction_latency_ms = last_compaction
            .as_ref()
            .map(|summary| summary.latency_ms)
            .unwrap_or(0);
        let last_compaction_tokens_before = last_compaction
            .as_ref()
            .map(|summary| summary.estimated_tokens_before)
            .unwrap_or(0);
        let last_compaction_tokens_after = last_compaction
            .as_ref()
            .map(|summary| summary.estimated_tokens_after)
            .unwrap_or(0);
        let compaction_background_reuse = last_compaction
            .as_ref()
            .is_some_and(|summary| summary.background_reuse);
        let compaction_fallback_to_sync = last_compaction
            .as_ref()
            .is_some_and(|summary| summary.fallback_to_sync);
        let store = self.store.clone();
        let checkpoint_session = session_id.to_string();
        let (checkpoint_count, latest_checkpoint) = tokio::task::spawn_blocking(move || {
            Ok::<_, anyhow::Error>((
                store.context_checkpoint_count(&checkpoint_session)?,
                store.latest_context_checkpoint(&checkpoint_session)?,
            ))
        })
        .await??;
        Ok(SessionContextStatus {
            session_id: session_id.to_string(),
            usage,
            last_compaction,
            last_measurement,
            compaction_count,
            last_compaction_latency_ms,
            last_compaction_tokens_before,
            last_compaction_tokens_after,
            compaction_background_reuse,
            compaction_fallback_to_sync,
            checkpoint_count,
            latest_checkpoint_id: latest_checkpoint
                .as_ref()
                .map(|checkpoint| checkpoint.id.clone()),
            latest_checkpoint_at: latest_checkpoint.map(|checkpoint| checkpoint.created_at),
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

    /// Read-only, bounded tool-activity projection. It reads `tool.finished`
    /// events only; provider content and raw structured payloads are not
    /// copied into the response. Legacy events without the additive metrics
    /// fields decode with zeroes and a derived `TOOL_FAILED` code.
    pub async fn session_tool_activity(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<ToolActivityResult> {
        let limit = limit.clamp(1, 200);
        let store = self.store.clone();
        let id = session_id.to_string();
        let (events, total) = tokio::task::spawn_blocking(move || -> Result<(Vec<Event>, u64)> {
            if store.get_session(&id)?.is_none() {
                anyhow::bail!("session not found: {id}");
            }
            let events = store.list_recent_events_by_kind(&id, "tool.finished", limit + 1)?;
            let total = store.event_count_by_kind(&id, "tool.finished")?;
            Ok((events, total))
        })
        .await??;
        let truncated = events.len() > limit as usize;
        let mut entries = events
            .into_iter()
            .take(limit as usize)
            .map(tool_activity_entry)
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.seq));
        let summary = tool_activity_summary(&entries);
        Ok(ToolActivityResult {
            session_id: session_id.to_string(),
            entries,
            total,
            limit,
            truncated,
            summary,
        })
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
            let memory_conflicts = store.list_session_memory_conflicts(&id)?;
            let questions = store.list_session_questions(&id)?;
            let tasks = store.list_plan_tasks(None, Some(&id))?;
            let steering = store.list_steering(&id, None, Some(2_000))?;
            let continuations = store.list_session_continuations(&id, None, Some(2_000))?;
            let gates = store.list_gates(&id, None, 500)?;
            let recoveries = store.list_orchestration_recoveries(None, Some(&id))?;
            let notes = store.list_coordination_notes(&id, 2_000)?;
            let decisions = store.list_orchestration_decisions(&id, 2_000)?;
            let scaling_decisions = store.list_all_scaling_decisions(&id)?;
            let runs = store.list_all_session_runs(&id)?;
            let run_steps = store.list_session_run_steps(&id)?;
            let run_checkpoints = store.list_session_run_checkpoints(&id)?;
            let context_checkpoints = store.list_context_checkpoints(&id, 10_000)?;
            let processes = store.list_supervised_processes(&id, true, 500)?;
            let test_results = store.list_test_results(
                &session.project_id,
                Some(&id),
                None,
                None,
                true,
                10_000,
                &vp_protocol::now_rfc3339(),
            )?;
            let remote_test_evidence = store.list_remote_test_evidence_for_session(&id)?;
            let instructions = store.list_instruction_states_for_session(&id)?;
            let contracts = store.list_contracts_for_session(&id)?;
            let contract_usage = store.list_contract_usage_for_session(&id)?;
            let effects = store.list_effect_receipts(&id, None, true, 500)?;
            let policy_snapshots = store.list_policy_snapshots(&session.project_id, 10_000)?;
            let plan_runtime_snapshot = store.get_plan_runtime_snapshot(&id)?;
            let prompt_prefix_observations =
                store.list_prompt_prefix_observations(&session.project_id, &id)?;
            let prompt_prefix_aggregates =
                store.list_prompt_prefix_aggregates(&session.project_id, 1_000)?;
            let epochs = prompt_prefix_observations
                .iter()
                .map(|row| row.cache_epoch.clone())
                .collect::<Vec<_>>();
            let prompt_prefix_inconsistent_epochs =
                store.prompt_prefix_inconsistent_epochs(&session.project_id, &epochs)?;
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
                memory_conflicts,
                questions,
                tasks,
                steering,
                continuations,
                gates,
                recoveries,
                notes,
                decisions,
                scaling_decisions,
                runs,
                run_steps,
                run_checkpoints,
                context_checkpoints,
                processes,
                test_results,
                remote_test_evidence,
                instructions,
                contracts,
                contract_usage,
                effects,
                policy_snapshots,
                plan_runtime_snapshot,
                prompt_prefix_observations,
                prompt_prefix_aggregates,
                prompt_prefix_inconsistent_epochs,
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
                        next_attempt_at: None,
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
                    | "plan_task.retry_scheduled"
                    | "plan_task.blocked"
                    | "plan_task.unblocked"
                    | "plan_task.cancelled"
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
                        task.status = if matches!(
                            data.session.status.as_str(),
                            "queued" | "running" | "paused" | "waiting_input"
                        ) {
                            "running".to_string()
                        } else {
                            "interrupted".to_string()
                        };
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
                    task.next_attempt_at = None;
                }
                "plan_task.retry_scheduled" => {
                    task.status = "pending".to_string();
                    task.finished_at = None;
                    task.next_attempt_at = event.payload["next_attempt_at"]
                        .as_str()
                        .map(str::to_string);
                    task.last_error = event.payload["reason"].as_str().map(str::to_string);
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
                    task.next_attempt_at = None;
                }
                "plan_task.skipped" => {
                    task.status = "skipped".to_string();
                    task.finished_at = Some(event.created_at.clone());
                    task.last_error = event.payload["reason"].as_str().map(str::to_string);
                }
                "plan_task.blocked" => {
                    task.status = "blocked".to_string();
                    task.finished_at = None;
                    task.last_error = event.payload["reason"].as_str().map(str::to_string);
                }
                "plan_task.unblocked" => {
                    task.status = "pending".to_string();
                    task.finished_at = None;
                    task.last_error = None;
                    task.next_attempt_at = None;
                }
                "plan_task.cancelled" => {
                    task.status = "cancelled".to_string();
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
                "approval.requested" | "approval.created" => {
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
                "approval.expired" => {
                    if let Some(approval_id) = event
                        .payload
                        .get("approval_id")
                        .and_then(serde_json::Value::as_str)
                        && let Some(approval) = approvals.get_mut(approval_id)
                    {
                        approval.status = "expired".to_string();
                        approval.resolved_at = event
                            .payload
                            .get("resolved_at")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                            .or_else(|| Some(event.created_at.clone()));
                    }
                }
                "approval.coalesced" => {
                    if let Some(approval_id) = event
                        .payload
                        .get("approval_id")
                        .and_then(serde_json::Value::as_str)
                        && let Some(approval) = approvals.get_mut(approval_id)
                        && let Some(count) = event
                            .payload
                            .get("waiter_count")
                            .and_then(serde_json::Value::as_u64)
                    {
                        approval.waiter_count = count.min(u64::from(u32::MAX)) as u32;
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
                        version: event
                            .payload
                            .get("version")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(1)
                            .clamp(1, u32::MAX as u64) as u32,
                        provenance: event
                            .payload
                            .get("provenance")
                            .filter(|value| !value.is_null())
                            .and_then(|value| {
                                serde_json::from_value::<vp_protocol::types::ArtifactProvenance>(
                                    value.clone(),
                                )
                                .ok()
                            }),
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
                    && expected.sha256 == projected.sha256
                    && (expected.provenance.is_none()
                        || expected.provenance == projected.provenance)))
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
                cache_read_tokens: event
                    .payload
                    .get("cache_read_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                cache_write_tokens: event
                    .payload
                    .get("cache_write_tokens")
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
            && left.cache_read_tokens == right.cache_read_tokens
            && left.cache_write_tokens == right.cache_write_tokens
            && left.estimated == right.estimated
            && left.cost_usd.to_bits() == right.cost_usd.to_bits()
    }

    /// Rebuild supplemental evidence rows from `remote_test.completed`
    /// events. A malformed event returns `None` rather than silently
    /// dropping evidence.
    fn expected_remote_test_evidence(data: &ReplayData) -> Option<Vec<RemoteTestEvidence>> {
        let mut rows = Vec::new();
        for event in data
            .events
            .iter()
            .filter(|event| event.kind == "remote_test.completed")
        {
            match serde_json::from_value::<RemoteTestEvidence>(
                event.payload.get("evidence")?.clone(),
            ) {
                Ok(row) => rows.push(row),
                Err(_) => return None,
            }
        }
        rows.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        Some(rows)
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
        let replay_unlimited = replay_session_unlimited(&data.events);
        if replay_unlimited != data.session.unlimited {
            mismatches.push(format!(
                "session budget mode: projection={}, replay={}",
                data.session.unlimited, replay_unlimited
            ));
        }
        let prompt_layout_matches = replay_session_prompt_layout(&data.events)
            .map(|replay_layout| replay_layout == data.session.prompt_layout)
            .unwrap_or(true);
        if !prompt_layout_matches {
            mismatches.push(format!(
                "session prompt layout: projection={}, replay={:?}",
                data.session.prompt_layout,
                replay_session_prompt_layout(&data.events)
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
        let replay_cost = replay_session_cost(&data.events);
        let usage_matches = replay_in == data.session.tokens_in
            && replay_out == data.session.tokens_out
            && (replay_cost - data.session.cost_spent_usd).abs() < 1e-9;
        if !usage_matches {
            mismatches.push(format!(
                "usage: projection={}/{}/{:.6}, replay={replay_in}/{replay_out}/{replay_cost:.6}",
                data.session.tokens_in, data.session.tokens_out, data.session.cost_spent_usd
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

        let expected_notes = crate::orchestration::coordination_notes(&data.events);
        let expected_note_values = expected_notes
            .iter()
            .filter_map(|note| Some((note.note_id.clone(), serde_json::to_value(note).ok()?)))
            .collect::<HashMap<_, _>>();
        let projected_note_values = data
            .notes
            .iter()
            .filter_map(|note| Some((note.note_id.clone(), serde_json::to_value(note).ok()?)))
            .collect::<HashMap<_, _>>();
        let notes_match = expected_note_values == projected_note_values;
        if !notes_match {
            mismatches.push(format!(
                "coordination notes: projection={}, replay={}",
                projected_note_values.len(),
                expected_note_values.len()
            ));
        }
        let expected_decisions = crate::orchestration::decisions_from_events(&data.events);
        let expected_decision_values = expected_decisions
            .iter()
            .filter_map(|decision| {
                Some((
                    decision.decision_id.clone(),
                    serde_json::to_value(decision).ok()?,
                ))
            })
            .collect::<HashMap<_, _>>();
        let projected_decision_values = data
            .decisions
            .iter()
            .filter_map(|decision| {
                Some((
                    decision.decision_id.clone(),
                    serde_json::to_value(decision).ok()?,
                ))
            })
            .collect::<HashMap<_, _>>();
        let decisions_match = expected_decision_values == projected_decision_values;
        if !decisions_match {
            mismatches.push(format!(
                "orchestration decisions: projection={}, replay={}",
                projected_decision_values.len(),
                expected_decision_values.len()
            ));
        }
        let expected_scaling = crate::scaling::decisions_from_events(&data.events);
        let scaling_decisions_match =
            crate::scaling::decisions_match(&data.scaling_decisions, &expected_scaling);
        if !scaling_decisions_match {
            mismatches.push(format!(
                "scaling decisions: projection={}, replay={}",
                data.scaling_decisions.len(),
                expected_scaling.len()
            ));
        }
        let expected_recoveries = crate::orchestration::recoveries_from_events(&data.events);
        let expected_recovery_values = expected_recoveries
            .iter()
            .filter_map(|recovery| {
                Some((recovery_key(recovery), serde_json::to_value(recovery).ok()?))
            })
            .collect::<HashMap<_, _>>();
        let projected_recovery_values = data
            .recoveries
            .iter()
            .filter_map(|recovery| {
                Some((recovery_key(recovery), serde_json::to_value(recovery).ok()?))
            })
            .collect::<HashMap<_, _>>();
        let recoveries_match = expected_recovery_values == projected_recovery_values;
        if !recoveries_match {
            mismatches.push(format!(
                "orchestration recoveries: projection={}, replay={}",
                projected_recovery_values.len(),
                expected_recovery_values.len()
            ));
        }
        let expected_steering = replay_steering(&data.events);
        let expected_steering_values = expected_steering
            .iter()
            .filter_map(|message| Some((message.id.clone(), serde_json::to_value(message).ok()?)))
            .collect::<HashMap<_, _>>();
        let projected_steering_values = data
            .steering
            .iter()
            .filter_map(|message| Some((message.id.clone(), serde_json::to_value(message).ok()?)))
            .collect::<HashMap<_, _>>();
        let steering_matches = expected_steering_values == projected_steering_values;
        if !steering_matches {
            mismatches.push(format!(
                "steering: projection={}, replay={}",
                projected_steering_values.len(),
                expected_steering_values.len()
            ));
        }
        let expected_continuations = replay_continuations(&data.events);
        let expected_continuation_values = expected_continuations
            .iter()
            .filter_map(|continuation| {
                Some((
                    continuation.id.clone(),
                    serde_json::to_value(continuation).ok()?,
                ))
            })
            .collect::<HashMap<_, _>>();
        let projected_continuation_values = data
            .continuations
            .iter()
            .filter_map(|continuation| {
                Some((
                    continuation.id.clone(),
                    serde_json::to_value(continuation).ok()?,
                ))
            })
            .collect::<HashMap<_, _>>();
        let continuations_match = expected_continuation_values == projected_continuation_values;
        if !continuations_match {
            mismatches.push(format!(
                "continuations: projection={}, replay={}",
                projected_continuation_values.len(),
                expected_continuation_values.len()
            ));
        }
        let expected_gates = crate::gate::gates_from_events(&data.events);
        let has_gate_events = data.events.iter().any(|event| {
            matches!(
                event.kind.as_str(),
                crate::gate::OPENED_EVENT
                    | crate::gate::RESOLVED_EVENT
                    | crate::gate::EXPIRED_EVENT
                    | crate::gate::CANCELLED_EVENT
            )
        });
        let gates_match = if !has_gate_events {
            // Legacy/imported archives may carry a gate projection without a
            // gate event stream. Preserve it rather than reporting drift.
            true
        } else {
            match expected_gates {
                Some(expected) => {
                    let matches = data.gates.len() == expected.len()
                        && data.gates.iter().all(|projected| {
                            expected.iter().any(|candidate| {
                                candidate.gate.id == projected.gate.id
                                    && candidate.gate.state == projected.gate.state
                                    && candidate.gate.outcome == projected.gate.outcome
                                    && candidate.gate.resolution_note
                                        == projected.gate.resolution_note
                                    && candidate.gate.resolved_at == projected.gate.resolved_at
                                    && candidate.gate.resolved_by == projected.gate.resolved_by
                                    && candidate.token_hash == projected.token_hash
                            })
                        });
                    if !matches {
                        mismatches.push(format!(
                            "external gates: projection={}, replay={}",
                            data.gates.len(),
                            expected.len()
                        ));
                    }
                    matches
                }
                None => {
                    mismatches.push("external gates: malformed replay event".to_string());
                    false
                }
            }
        };

        let expected_journal = crate::run_journal::projections_from_events(&data.events);
        let (runs_match, run_steps_match, run_checkpoints_match) = match &expected_journal {
            Some((runs, steps, checkpoints)) => (
                crate::run_journal::runs_match(&data.runs, runs),
                crate::run_journal::steps_match(&data.run_steps, steps),
                crate::run_journal::checkpoints_match(&data.run_checkpoints, checkpoints),
            ),
            None => (false, false, false),
        };
        if !runs_match {
            mismatches.push(format!(
                "session runs: projection={}, replay={}",
                data.runs.len(),
                expected_journal
                    .as_ref()
                    .map(|value| value.0.len())
                    .unwrap_or(0)
            ));
        }
        if !run_steps_match {
            mismatches.push(format!(
                "run steps: projection={}, replay={}",
                data.run_steps.len(),
                expected_journal
                    .as_ref()
                    .map(|value| value.1.len())
                    .unwrap_or(0)
            ));
        }
        if !run_checkpoints_match {
            mismatches.push(format!(
                "run checkpoints: projection={}, replay={}",
                data.run_checkpoints.len(),
                expected_journal
                    .as_ref()
                    .map(|value| value.2.len())
                    .unwrap_or(0)
            ));
        }

        let expected_checkpoints = crate::context::checkpoints_from_events(&data.events);
        let context_checkpoints_match = expected_checkpoints.as_ref().is_some_and(|expected| {
            crate::context::context_checkpoints_match(&data.context_checkpoints, expected)
        });
        if !context_checkpoints_match {
            mismatches.push(format!(
                "context checkpoints: projection={}, replay={}",
                data.context_checkpoints.len(),
                expected_checkpoints.as_ref().map(Vec::len).unwrap_or(0)
            ));
        }

        let expected_processes = crate::process_supervisor::processes_from_events(&data.events);
        let processes_match = expected_processes.as_ref().is_some_and(|expected| {
            crate::process_supervisor::processes_match(&data.processes, expected)
        });
        if !processes_match {
            mismatches.push(format!(
                "supervised processes: projection={}, replay={}",
                data.processes.len(),
                expected_processes.as_ref().map(Vec::len).unwrap_or(0)
            ));
        }

        let expected_test_results = crate::test_cache::results_from_events(&data.events);
        if !crate::test_cache::results_match(&data.test_results, &expected_test_results) {
            mismatches.push(format!(
                "test cache: projection={}, replay={}",
                data.test_results.len(),
                expected_test_results.len()
            ));
        }

        let has_instruction_events = data
            .events
            .iter()
            .any(|event| event.kind == "context.instruction");
        let instructions_match = match crate::instruction::states_from_events(&data.events) {
            Some(expected_instructions) => {
                let matches = crate::instruction::instruction_rows_match(
                    &data.instructions,
                    &expected_instructions,
                );
                if !matches {
                    mismatches.push(format!(
                        "instruction state: projection={}, replay={}",
                        data.instructions.len(),
                        expected_instructions.len()
                    ));
                }
                matches
            }
            None if has_instruction_events => {
                mismatches.push("instruction state: malformed replay event".to_string());
                false
            }
            None => {
                // Legacy/imported archives may carry instruction rows without
                // an instruction event stream. There is no replay authority in
                // that case, so the projection is preserved rather than
                // reported as drift.
                true
            }
        };

        let has_contract_events = data
            .events
            .iter()
            .any(|event| event.kind == "contract.derived");
        let expected_contracts = crate::contract::contracts_from_events(&data.events);
        let contracts_projection = data
            .contracts
            .iter()
            .map(|row| {
                serde_json::from_str::<crate::contract::TaskContract>(&row.contract).map_err(|_| ())
            })
            .collect::<std::result::Result<Vec<_>, ()>>();
        let contracts_match = if !has_contract_events {
            // Legacy/imported archives may carry rows without a contract event
            // stream. The projection is preserved rather than treated as drift.
            true
        } else {
            match (&expected_contracts, contracts_projection) {
                (Some(expected), Ok(projected)) => {
                    let rows_match = crate::contract::contracts_match(&projected, expected);
                    if !rows_match {
                        mismatches.push(format!(
                            "task contracts: projection={}, replay={}",
                            data.contracts.len(),
                            expected.len()
                        ));
                    }
                    let expected_usage =
                        crate::contract::usage_from_events(&data.events).unwrap_or_default();
                    let mut usage_delta = false;
                    if expected_usage.len() != data.contract_usage.len() {
                        usage_delta = true;
                    }
                    for row in &data.contract_usage {
                        let expected = expected_usage
                            .get(&row.contract_id)
                            .copied()
                            .unwrap_or_default();
                        if expected.writes != row.writes
                            || expected.bytes != row.bytes
                            || expected.commits != row.commits
                        {
                            usage_delta = true;
                        }
                    }
                    if usage_delta {
                        mismatches.push(format!(
                            "contract usage: projection={}, replay={}",
                            data.contract_usage.len(),
                            expected_usage.len()
                        ));
                    }
                    rows_match && !usage_delta
                }
                (None, _) if has_contract_events => {
                    mismatches.push("task contracts: malformed replay event".to_string());
                    false
                }
                (None, _) => {
                    // Legacy/imported archives may carry no contract event stream.
                    // Preserve the projection rather than reporting drift.
                    true
                }
                (Some(_), Err(())) => {
                    mismatches.push("task contracts: malformed projection row".to_string());
                    false
                }
            }
        };

        let expected_prefix_observations = crate::cache_epoch::observations_from_events(
            &data.session.project_id,
            &data.session.id,
            &data.events,
        );
        let (prefix_observations_match, prefix_aggregates_match) = if let Some(expected) =
            expected_prefix_observations
        {
            let observations_match =
                crate::cache_epoch::observations_match(&data.prompt_prefix_observations, &expected);
            if !observations_match {
                mismatches.push(format!(
                    "prompt prefix observations: projection={}, replay={}",
                    data.prompt_prefix_observations.len(),
                    expected.len()
                ));
            }
            let mut aggregates_match = data.prompt_prefix_inconsistent_epochs.is_empty();
            if !aggregates_match {
                mismatches.push(format!(
                    "prompt prefix aggregates: {} epoch(s) diverge from observations",
                    data.prompt_prefix_inconsistent_epochs.len()
                ));
            }
            let missing_aggregate = expected.iter().any(|row| {
                !data
                    .prompt_prefix_aggregates
                    .iter()
                    .any(|aggregate| aggregate.cache_epoch == row.cache_epoch)
            });
            if missing_aggregate {
                aggregates_match = false;
                mismatches.push(format!(
                    "prompt prefix aggregates: missing an epoch row for {} session epoch(s)",
                    expected.len()
                ));
            }
            (observations_match, aggregates_match)
        } else {
            // No epoch event stream means a legacy session; its projection
            // is preserved rather than reported as drift.
            (true, true)
        };

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
                "memory.created" | "memory.updated" | "memory.verified" | "memory.stale"
                | "memory.superseded" | "memory.used" => {
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

        let replay_conflicts = replay_memory_conflicts(&data.events)
            .into_iter()
            .filter_map(|conflict| {
                Some((conflict.id.clone(), serde_json::to_value(conflict).ok()?))
            })
            .collect::<HashMap<_, _>>();
        let projected_conflicts = data
            .memory_conflicts
            .iter()
            .filter_map(|conflict| {
                Some((conflict.id.clone(), serde_json::to_value(conflict).ok()?))
            })
            .collect::<HashMap<_, _>>();
        let memory_conflicts_match = replay_conflicts == projected_conflicts;
        if !memory_conflicts_match {
            mismatches.push(format!(
                "memory conflicts: projection={}, replay={}",
                projected_conflicts.len(),
                replay_conflicts.len()
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
            .find(|event| {
                matches!(
                    event.kind.as_str(),
                    "team_plan.submitted" | "team_plan.revised"
                )
            })
            .map(|event| {
                let mut payload = event.payload.clone();
                if let Some(object) = payload.as_object_mut() {
                    object.remove("runtime_snapshot");
                }
                payload
            });
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

        let replay_evidence = Self::expected_remote_test_evidence(data).unwrap_or_default();
        let mut projected_evidence = data.remote_test_evidence.clone();
        projected_evidence.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        let remote_test_evidence_matches = replay_evidence == projected_evidence;
        if !remote_test_evidence_matches {
            mismatches
                .push("remote test evidence projection differs from event stream".to_string());
        }

        let has_effect_events = data.events.iter().any(|event| {
            matches!(
                event.kind.as_str(),
                crate::effect::EFFECT_INTENT_EVENT
                    | crate::effect::EFFECT_SETTLED_EVENT
                    | crate::effect::EFFECT_FAILED_EVENT
                    | crate::effect::EFFECT_UNKNOWN_EVENT
                    | crate::effect::EFFECT_RECONCILED_EVENT
            )
        });
        let effects_match = if !has_effect_events {
            // Legacy/imported archives may carry rows without effect events.
            true
        } else {
            match crate::effect::receipts_from_events(&data.events) {
                Some(expected) => {
                    let rows_match = crate::effect::receipts_match(&data.effects, &expected);
                    if !rows_match {
                        mismatches.push(format!(
                            "effect receipts: projection={}, replay={}",
                            data.effects.len(),
                            expected.len()
                        ));
                    }
                    rows_match
                }
                None => {
                    mismatches.push("effect receipts: malformed replay event".to_string());
                    false
                }
            }
        };

        let expected_policy = crate::runtime_pin::policy_snapshots_from_events(&data.events);
        let mut referenced_policy_hashes = expected_policy
            .iter()
            .map(|row| row.hash.clone())
            .collect::<std::collections::BTreeSet<_>>();
        if let Some(raw) = &data.plan_runtime_snapshot
            && let Some(snapshot) = crate::runtime_pin::parse_snapshot(raw)
            && !snapshot.policy_hash.is_empty()
        {
            referenced_policy_hashes.insert(snapshot.policy_hash);
        }
        for run in &data.runs {
            if let Some(snapshot) = &run.runtime_snapshot
                && !snapshot.policy_hash.is_empty()
            {
                referenced_policy_hashes.insert(snapshot.policy_hash.clone());
            }
        }
        let projected_policy = data
            .policy_snapshots
            .iter()
            .map(|row| (row.hash.clone(), row.json.clone()))
            .collect::<std::collections::HashMap<_, _>>();
        let policy_snapshots_match = expected_policy.iter().all(|row| {
            projected_policy
                .get(&row.hash)
                .is_some_and(|json| json == &row.json)
        }) && referenced_policy_hashes
            .iter()
            .all(|hash| projected_policy.contains_key(hash));
        if !policy_snapshots_match {
            mismatches.push(format!(
                "policy snapshots: projection={}, replay={}, referenced={}",
                data.policy_snapshots.len(),
                expected_policy.len(),
                referenced_policy_hashes.len()
            ));
        }

        let consistent = status_matches
            && prompt_layout_matches
            && instructions_match
            && prefix_observations_match
            && prefix_aggregates_match
            && usage_matches
            && ledger_matches
            && mailbox_matches
            && artifacts_matches
            && approvals_matches
            && memories_matches
            && memory_conflicts_match
            && questions_matches
            && task_queue_matches
            && plan_matches
            && todos_matches
            && messages_matches
            && agents_matches
            && queue_matches
            && notes_match
            && decisions_match
            && scaling_decisions_match
            && recoveries_match
            && steering_matches
            && continuations_match
            && gates_match
            && runs_match
            && run_steps_match
            && run_checkpoints_match
            && context_checkpoints_match
            && processes_match
            && remote_test_evidence_matches
            && contracts_match
            && effects_match
            && policy_snapshots_match;
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
            memory_conflicts_match,
            questions_matches,
            task_queue_matches,
            notes_match,
            decisions_match,
            scaling_decisions_match,
            recoveries_match,
            steering_matches,
            continuations_match,
            gates_match,
            runs_match,
            run_steps_match,
            run_checkpoints_match,
            context_checkpoints_match,
            processes_match,
            remote_test_evidence_matches,
            contracts_match,
            effects_match,
            policy_snapshots_match,
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

        let plan_event = data.events.iter().rev().find(|event| {
            matches!(
                event.kind.as_str(),
                "team_plan.submitted" | "team_plan.revised"
            )
        });
        let replace_plan = plan_event.is_some() || data.plan.is_none();
        let plan = plan_event
            .map(|event| {
                let mut payload = event.payload.clone();
                if let Some(object) = payload.as_object_mut() {
                    object.remove("runtime_snapshot");
                }
                serde_json::to_string(&payload).map_err(anyhow::Error::from)
            })
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
            let mut items: Vec<MemoryItem> = Vec::new();
            let mut valid = true;
            for event in &data.events {
                match event.kind.as_str() {
                    "memory.created" | "memory.updated" | "memory.verified" | "memory.stale"
                    | "memory.superseded" | "memory.used" => {
                        match serde_json::from_value::<MemoryItem>(event.payload.clone()) {
                            Ok(memory) => {
                                if let Some(existing) =
                                    items.iter_mut().find(|item| item.id == memory.id)
                                {
                                    *existing = memory;
                                } else {
                                    items.push(memory);
                                }
                            }
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
        let cost_spent_usd = replay_session_cost(&data.events);
        let priority = data
            .events
            .iter()
            .rev()
            .find(|event| event.kind == "session.priority")
            .and_then(|event| event.payload.get("priority"))
            .and_then(serde_json::Value::as_i64)
            .and_then(|value| i32::try_from(value).ok());
        let journal = crate::run_journal::projections_from_events(&data.events);
        let context_checkpoints = crate::context::checkpoints_from_events(&data.events);
        let processes = crate::process_supervisor::processes_from_events(&data.events);
        Ok(SessionRepair {
            status,
            priority,
            unlimited: Some(replay_session_unlimited(&data.events)),
            prompt_layout: replay_session_prompt_layout(&data.events),
            tokens_in,
            tokens_out,
            cost_spent_usd,
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
            memory_conflicts: Some(replay_memory_conflicts(&data.events)),
            questions,
            tasks: Self::expected_tasks(data),
            steering: Some(replay_steering(&data.events)),
            continuations: Some(replay_continuations(&data.events)),
            gates: if !data.events.iter().any(|event| {
                matches!(
                    event.kind.as_str(),
                    crate::gate::OPENED_EVENT
                        | crate::gate::RESOLVED_EVENT
                        | crate::gate::EXPIRED_EVENT
                        | crate::gate::CANCELLED_EVENT
                )
            }) {
                // Legacy/imported archives may carry gate rows without an
                // event stream. Their projection is preserved rather than
                // treated as drift.
                if data.gates.is_empty() {
                    Some(Vec::new())
                } else {
                    None
                }
            } else {
                match crate::gate::gates_from_events(&data.events) {
                    Some(rows) => Some(rows),
                    None if data.gates.is_empty() => Some(Vec::new()),
                    None => None,
                }
            },
            recoveries: Some(crate::orchestration::recoveries_from_events(&data.events)),
            runs: journal.as_ref().map(|value| value.0.clone()),
            run_steps: journal.as_ref().map(|value| value.1.clone()),
            run_checkpoints: journal.as_ref().map(|value| value.2.clone()),
            context_checkpoints,
            supervised_processes: processes,
            test_results: Some(crate::test_cache::results_from_events(&data.events)),
            remote_test_evidence: {
                let expected = Self::expected_remote_test_evidence(data);
                expected.filter(|rows| !rows.is_empty() || data.remote_test_evidence.is_empty())
            },
            instructions: match crate::instruction::states_from_events(&data.events) {
                Some(rows) => Some(rows),
                None if data.instructions.is_empty() => Some(Vec::new()),
                None => None,
            },
            contracts: if !data
                .events
                .iter()
                .any(|event| event.kind == "contract.derived")
            {
                None
            } else {
                match crate::contract::contracts_from_events(&data.events) {
                    Some(contracts) => Some(
                        contracts
                            .iter()
                            .map(|contract| StoredContractRow {
                                contract_id: contract.contract_id.clone(),
                                project_id: contract.project_id.clone(),
                                session_id: contract.session_id.clone(),
                                task_id: contract.task_id.clone(),
                                plan_revision: contract.plan_revision,
                                role: contract.role.clone(),
                                hash: contract.hash.clone(),
                                source_hash: contract.source_hash.clone(),
                                contract: serde_json::to_string(contract).unwrap_or_default(),
                                created_at: contract.created_at.clone(),
                            })
                            .collect::<Vec<_>>(),
                    ),
                    None if data.contracts.is_empty() => Some(Vec::new()),
                    None => None,
                }
            },
            contract_usage: if !data
                .events
                .iter()
                .any(|event| event.kind == "intent.mutation")
            {
                None
            } else {
                match crate::contract::usage_from_events(&data.events) {
                    Some(usage) => Some(
                        usage
                            .into_iter()
                            .map(|(contract_id, usage)| ContractUsageRow {
                                contract_id,
                                project_id: data.session.project_id.clone(),
                                session_id: data.session.id.clone(),
                                task_id: None,
                                writes: usage.writes,
                                bytes: usage.bytes,
                                commits: usage.commits,
                                updated_at: vp_protocol::now_rfc3339(),
                            })
                            .collect::<Vec<_>>(),
                    ),
                    None if data.contract_usage.is_empty() => Some(Vec::new()),
                    None => None,
                }
            },
            effects: if !data.events.iter().any(|event| {
                matches!(
                    event.kind.as_str(),
                    crate::effect::EFFECT_INTENT_EVENT
                        | crate::effect::EFFECT_SETTLED_EVENT
                        | crate::effect::EFFECT_FAILED_EVENT
                        | crate::effect::EFFECT_UNKNOWN_EVENT
                        | crate::effect::EFFECT_RECONCILED_EVENT
                )
            }) {
                None
            } else {
                match crate::effect::receipts_from_events(&data.events) {
                    Some(rows) => Some(rows),
                    None if data.effects.is_empty() => Some(Vec::new()),
                    None => None,
                }
            },
            prompt_prefix_observations: match crate::cache_epoch::observations_from_events(
                &data.session.project_id,
                &data.session.id,
                &data.events,
            ) {
                Some(rows) => Some(rows),
                None if data.prompt_prefix_observations.is_empty() => Some(Vec::new()),
                None => None,
            },
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
        let bundle_test_results = bundle.test_results.clone();
        let store = self.store.clone();
        let event =
            tokio::task::spawn_blocking(move || store.import_session_bundle(&bundle)).await??;
        let _ = self.events.send(event);
        // Imported archives keep their raw events; rebuild the derived
        // coordination and decision projections so the archive is inspectable.
        let projection_store = self.store.clone();
        let projection_id = session_id.clone();
        let events = {
            let store = self.store.clone();
            let id = session_id.clone();
            tokio::task::spawn_blocking(move || store.list_all_events(&id)).await??
        };
        let notes = crate::orchestration::coordination_notes(&events);
        let decisions = crate::orchestration::decisions_from_events(&events);
        let scaling_decisions = crate::scaling::decisions_from_events(&events);
        let recoveries = crate::orchestration::recoveries_from_events(&events);
        tokio::task::spawn_blocking(move || -> Result<()> {
            projection_store.replace_coordination_notes(&projection_id, &notes, &events)?;
            projection_store.replace_orchestration_decisions(
                &projection_id,
                &decisions,
                &events,
            )?;
            projection_store.replace_scaling_decisions(
                &projection_id,
                &scaling_decisions,
                &events,
            )?;
            projection_store.replace_orchestration_recoveries(&projection_id, &recoveries)?;
            if let Some((runs, steps, checkpoints)) =
                crate::run_journal::projections_from_events(&events)
            {
                projection_store.replace_run_journal_projections(
                    &projection_id,
                    &runs,
                    &steps,
                    &checkpoints,
                )?;
            }
            let has_context_checkpoint_events = events.iter().any(|event| {
                matches!(
                    event.kind.as_str(),
                    crate::context::CONTEXT_CHECKPOINT_CREATED_EVENT
                        | crate::context::CONTEXT_CHECKPOINT_DELETED_EVENT
                )
            });
            // A legacy projection-only bundle may carry rows without the
            // matching audit events. Preserve those rows; when events exist
            // they are authoritative and rebuild (or tombstone) the projection.
            if has_context_checkpoint_events
                && let Some(context_checkpoints) = crate::context::checkpoints_from_events(&events)
            {
                projection_store
                    .replace_context_checkpoints(&projection_id, &context_checkpoints)?;
            }
            let replayed_test_results = crate::test_cache::results_from_events(&events);
            let test_results = if replayed_test_results.is_empty() {
                bundle_test_results
            } else {
                replayed_test_results
            };
            projection_store.replace_test_results(&projection_id, &test_results)?;
            // Effect events are authoritative when the archive carries them;
            // a legacy projection-only bundle keeps the rows inserted above.
            if events.iter().any(|event| {
                matches!(
                    event.kind.as_str(),
                    crate::effect::EFFECT_INTENT_EVENT
                        | crate::effect::EFFECT_SETTLED_EVENT
                        | crate::effect::EFFECT_FAILED_EVENT
                        | crate::effect::EFFECT_UNKNOWN_EVENT
                        | crate::effect::EFFECT_RECONCILED_EVENT
                )
            }) && let Some(effects) = crate::effect::receipts_from_events(&events)
            {
                projection_store.replace_effect_receipts(&projection_id, &effects)?;
            }
            Ok(())
        })
        .await??;
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
        let usage_store = store.clone();
        let usage_session = session_id.clone();
        let usage =
            tokio::task::spawn_blocking(move || usage_store.list_usage_entries(&usage_session))
                .await??;
        let program_store = store.clone();
        let program_session = session_id.clone();
        let programs = tokio::task::spawn_blocking(move || {
            program_store.list_programs_for_session(&program_session)
        })
        .await??;
        let mut program_milestones = Vec::new();
        let mut program_events = Vec::new();
        for program in &programs {
            let milestone_store = store.clone();
            let program_id = program.id.clone();
            program_milestones.extend(
                tokio::task::spawn_blocking(move || {
                    milestone_store.list_program_milestones(&program_id)
                })
                .await??,
            );
            let event_store = store.clone();
            let program_id = program.id.clone();
            program_events.extend(
                tokio::task::spawn_blocking(move || {
                    event_store.list_program_events(&program_id, Some(10_000))
                })
                .await??,
            );
        }
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
        let blob_store = store.clone();
        let instruction_hashes = data
            .instructions
            .iter()
            .map(|instruction| instruction.hash.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let instruction_blobs = tokio::task::spawn_blocking(move || {
            let mut blobs = Vec::with_capacity(instruction_hashes.len());
            for hash in instruction_hashes {
                let blob = blob_store.get_instruction_blob(&hash)?.ok_or_else(|| {
                    anyhow!("instruction state references missing blob {hash}; export refused")
                })?;
                if crate::instruction::hash_instruction_text(&blob.text) != hash {
                    anyhow::bail!(
                        "instruction blob {hash} does not match its text; export refused"
                    );
                }
                blobs.push(blob);
            }
            Ok::<_, anyhow::Error>(blobs)
        })
        .await??;
        let policy_store = store.clone();
        let policy_project = data.session.project_id.clone();
        let policy_session = data.session.id.clone();
        let mut policy_hashes = data
            .events
            .iter()
            .filter(|event| event.kind == crate::runtime_pin::POLICY_SNAPSHOT_EVENT)
            .filter_map(|event| {
                event
                    .payload
                    .get("hash")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .collect::<std::collections::BTreeSet<_>>();
        for run in &data.runs {
            if let Some(snapshot) = &run.runtime_snapshot
                && !snapshot.policy_hash.is_empty()
            {
                policy_hashes.insert(snapshot.policy_hash.clone());
            }
        }
        let policy_snapshots = tokio::task::spawn_blocking(move || {
            let mut policy_hashes = policy_hashes;
            if let Some(raw) = policy_store.get_plan_runtime_snapshot(&policy_session)?
                && let Some(snapshot) = crate::runtime_pin::parse_snapshot(&raw)
                && !snapshot.policy_hash.is_empty()
            {
                policy_hashes.insert(snapshot.policy_hash);
            }
            let mut rows = Vec::new();
            for hash in policy_hashes {
                if let Some(row) = policy_store.get_policy_snapshot(&policy_project, &hash)? {
                    rows.push(row);
                }
            }
            Ok::<_, anyhow::Error>(rows)
        })
        .await??;
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
            memory_conflicts: data.memory_conflicts,
            questions: data.questions,
            tasks: data.tasks,
            steering: data.steering,
            continuations: data.continuations,
            gates: data.gates,
            policy_snapshots,
            recoveries: data.recoveries,
            programs,
            program_milestones,
            program_events,
            session_runs: data.runs,
            run_steps: data.run_steps,
            run_checkpoints: data.run_checkpoints,
            context_checkpoints: data.context_checkpoints,
            supervised_processes: data.processes,
            test_results: data.test_results,
            instruction_blobs,
            instructions: data.instructions,
            prompt_prefix_observations: data.prompt_prefix_observations,
            effects: data.effects,
        })
    }

    pub async fn rebuild_session(&self, session_id: &str) -> Result<SessionReplayReport> {
        ensure_session_not_active(&self.store, session_id, "rebuild a session").await?;
        let data = self.load_replay_data(session_id).await?;
        self.restore_policy_snapshots_from_events(&data.events)
            .await?;
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
        let expected_notes = crate::orchestration::coordination_notes(&data.events);
        let expected_decisions = crate::orchestration::decisions_from_events(&data.events);
        let expected_scaling = crate::scaling::decisions_from_events(&data.events);
        let expected_recoveries = crate::orchestration::recoveries_from_events(&data.events);
        let projection_store = self.store.clone();
        let projection_id = session_id.to_string();
        let projection_events = data.events.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            projection_store.replace_coordination_notes(
                &projection_id,
                &expected_notes,
                &projection_events,
            )?;
            projection_store.replace_orchestration_decisions(
                &projection_id,
                &expected_decisions,
                &projection_events,
            )?;
            projection_store.replace_scaling_decisions(
                &projection_id,
                &expected_scaling,
                &projection_events,
            )?;
            projection_store
                .replace_orchestration_recoveries(&projection_id, &expected_recoveries)?;
            Ok(())
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
        ensure_session_not_active(&self.store, session_id, "restore an event log").await?;
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

    /// Rebuild the immutable policy projection from `policy.snapshot` events.
    /// Rows are content-addressed and idempotent; a malformed row fails closed.
    async fn restore_policy_snapshots_from_events(&self, events: &[Event]) -> Result<()> {
        let rows = crate::runtime_pin::policy_snapshots_from_events(events);
        if rows.is_empty() {
            return Ok(());
        }
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            for row in &rows {
                store.insert_policy_snapshot_snapshot(row)?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        Ok(())
    }

    pub async fn repair_session(&self, session_id: &str) -> Result<SessionReplayReport> {
        ensure_session_not_active(&self.store, session_id, "repair a session").await?;
        let data = self.load_replay_data(session_id).await?;
        self.restore_policy_snapshots_from_events(&data.events)
            .await?;
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
        let expected_notes = crate::orchestration::coordination_notes(&data.events);
        let expected_decisions = crate::orchestration::decisions_from_events(&data.events);
        let expected_scaling = crate::scaling::decisions_from_events(&data.events);
        let expected_recoveries = crate::orchestration::recoveries_from_events(&data.events);
        let projection_store = self.store.clone();
        let projection_id = session_id.to_string();
        let projection_events = data.events.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            projection_store.replace_coordination_notes(
                &projection_id,
                &expected_notes,
                &projection_events,
            )?;
            projection_store.replace_orchestration_decisions(
                &projection_id,
                &expected_decisions,
                &projection_events,
            )?;
            projection_store.replace_scaling_decisions(
                &projection_id,
                &expected_scaling,
                &projection_events,
            )?;
            projection_store
                .replace_orchestration_recoveries(&projection_id, &expected_recoveries)?;
            Ok(())
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

    /// Toggle unlimited budget mode. The persisted budget values are retained,
    /// so disabling unlimited restores the previous limits. The shared runtime
    /// flag changes immediately for any active agent loop.
    pub async fn set_session_unlimited(
        &self,
        session_id: &str,
        unlimited: bool,
    ) -> Result<SessionUnlimitedResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking({
            let store = store.clone();
            let id = id.clone();
            move || store.get_session(&id)
        })
        .await??
        .ok_or_else(|| anyhow!("session not found: {session_id}"))?;
        if session.status == "imported" {
            anyhow::bail!("imported sessions are read-only archives");
        }
        let store = self.store.clone();
        let event = tokio::task::spawn_blocking(move || {
            store.set_session_unlimited_with_event(&id, unlimited)
        })
        .await??;
        let _ = self.events.send(event);
        self.update_unlimited_flag(session_id, unlimited);
        Ok(SessionUnlimitedResult {
            session_id: session_id.to_string(),
            unlimited,
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

    pub async fn set_agent_task(
        &self,
        agent_id: &str,
        task_id: Option<String>,
        department: Option<String>,
    ) -> Result<()> {
        let store = self.store.clone();
        let id = agent_id.to_string();
        let event = tokio::task::spawn_blocking(move || {
            store.set_agent_task_with_event(&id, task_id.as_deref(), department.as_deref())
        })
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
        self.enqueue_merge_with_source(
            session_id,
            agent_id,
            agent_name,
            task_id,
            branch,
            commit,
            base_commit,
            detail,
            vp_protocol::types::MergeQueueSource::local(),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn enqueue_merge_with_source(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        task_id: Option<String>,
        branch: &str,
        commit: &str,
        base_commit: &str,
        detail: Option<String>,
        source: vp_protocol::types::MergeQueueSource,
    ) -> Result<MergeQueueEntry> {
        // Fail closed before a queue row exists. Integration re-checks the
        // same gate under the integration lease in case an effect becomes
        // unresolved while the entry waits.
        self.assert_effects_settled(session_id, task_id.as_deref())
            .await?;
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let agent_id = agent_id.to_string();
        let agent_name = agent_name.to_string();
        let branch = branch.to_string();
        let commit = commit.to_string();
        let base_commit = base_commit.to_string();
        let (entry, event) = tokio::task::spawn_blocking(move || {
            store.enqueue_merge_with_event_source(
                &session_id,
                &agent_id,
                &agent_name,
                task_id.as_deref(),
                &branch,
                &commit,
                &base_commit,
                detail.as_deref(),
                &source,
            )
        })
        .await??;
        let _ = self.events.send(event);
        Ok(entry)
    }

    pub async fn set_merge_queue_verification(
        &self,
        id: &str,
        approved: Option<bool>,
        summary: Option<String>,
        environment_caveat: Option<String>,
    ) -> Result<Option<MergeQueueEntry>> {
        let store = self.store.clone();
        let id = id.to_string();
        let updated = tokio::task::spawn_blocking(move || {
            store.set_merge_queue_verification_with_event(
                &id,
                approved,
                summary.as_deref(),
                environment_caveat.as_deref(),
            )
        })
        .await??;
        let entry = updated.map(|(entry, event)| {
            let _ = self.events.send(event);
            entry
        });
        Ok(entry)
    }

    pub async fn begin_merge_queue_entry(
        &self,
        id: &str,
        pre_merge_commit: &str,
    ) -> Result<Option<MergeQueueEntry>> {
        let store = self.store.clone();
        let id = id.to_string();
        let pre_merge_commit = pre_merge_commit.to_string();
        tokio::task::spawn_blocking(move || store.begin_merge_queue_entry(&id, &pre_merge_commit))
            .await?
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

    pub async fn list_approval_grants(&self, project_id: &str) -> Result<Vec<ApprovalGrant>> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking(move || store.list_approval_grants(&project_id)).await?
    }

    pub async fn create_approval_grant(
        &self,
        project_id: &str,
        tool: &str,
        path_prefix: &str,
        include_children: bool,
        created_by: &str,
    ) -> Result<ApprovalGrant> {
        let tool = tool.trim();
        let path_prefix = path_prefix.trim().trim_start_matches("./");
        if tool.is_empty() || tool.chars().count() > 120 {
            anyhow::bail!("approval tool must be 1-120 characters");
        }
        if path_prefix.is_empty() || path_prefix.chars().count() > 1_000 {
            anyhow::bail!("approval path must be 1-1000 characters");
        }
        if path_prefix.contains('$') || path_prefix.contains('~') {
            anyhow::bail!("approval path cannot contain shell or home expansion");
        }
        let store = self.store.clone();
        let project_id = project_id.to_string();
        let tool = tool.to_string();
        let path_prefix = path_prefix.to_string();
        let created_by = created_by.to_string();
        let grant = tokio::task::spawn_blocking(move || {
            store.create_approval_grant(
                &project_id,
                &tool,
                &path_prefix,
                include_children,
                &created_by,
            )
        })
        .await??;
        self.record_project_manifest().await?;
        Ok(grant)
    }

    pub async fn delete_approval_grant(&self, id: &str) -> Result<bool> {
        let store = self.store.clone();
        let id = id.to_string();
        let deleted =
            tokio::task::spawn_blocking(move || store.delete_approval_grant(&id)).await??;
        if deleted {
            self.record_project_manifest().await?;
        }
        Ok(deleted)
    }

    /// Deterministic precedence evaluation for one tool call. The store's
    /// single writer lock makes template use consumption atomic.
    #[allow(clippy::too_many_arguments)]
    pub fn evaluate_tool_permission(
        &self,
        session_id: &str,
        project_id: &str,
        assessment: &crate::permissions::PermissionAssessment,
        tool: &str,
        input: &serde_json::Value,
        mode: vp_protocol::types::ApprovalMode,
        auto_approve: bool,
    ) -> Result<crate::approvals::ApprovalEvaluation> {
        let templates = self.store.list_approval_templates(project_id)?;
        let grants = self.store.list_approval_grants(project_id)?;
        let now = time::OffsetDateTime::now_utc();
        let now_string = now_rfc3339();
        let mut consume = |template: &ApprovalTemplate| {
            self.store
                .consume_approval_template_use(&template.id, &now_string)
                .ok()
                .flatten()
        };
        let evaluation = crate::approvals::evaluate_approval_rules(
            &templates,
            &grants,
            assessment,
            tool,
            input,
            mode,
            auto_approve,
            now,
            &mut consume,
        );
        if let (Some(template_id), Some(template_name)) = (
            evaluation.template_id.as_deref(),
            evaluation.template_name.as_deref(),
        ) {
            let event = ApprovalAuditEvent {
                id: Uuid::now_v7().to_string(),
                project_id: project_id.to_string(),
                kind: "approval.template.matched".to_string(),
                payload: serde_json::json!({
                    "session_id": session_id,
                    "template_id": template_id,
                    "template_name": template_name,
                    "source": evaluation.source.as_str(),
                    "action": match evaluation.source {
                        crate::approvals::ApprovalDecisionSource::TemplateDeny => "deny",
                        crate::approvals::ApprovalDecisionSource::TemplateEscalate => "escalate",
                        _ => "allow",
                    },
                    "tool": tool,
                    "risk": evaluation.risk().as_str(),
                    "scope": evaluation.scope.clone(),
                    "remaining_uses": evaluation.remaining_uses,
                    "auto_approve": auto_approve,
                }),
                created_at: now_rfc3339(),
            };
            if let Err(error) = self.store.insert_approval_audit(&event) {
                tracing::warn!(%error, "failed to record approval template match audit");
            }
        }
        Ok(evaluation)
    }

    pub async fn approval_grant_matches(
        &self,
        project_id: &str,
        tool: &str,
        input: &serde_json::Value,
    ) -> Result<bool> {
        if tool.starts_with("remote_") {
            return Ok(false);
        }
        let paths = crate::approvals::extract_input_paths(input);
        if paths.is_empty() {
            return Ok(false);
        }
        let grants = self.list_approval_grants(project_id).await?;
        Ok(crate::approvals::match_grant(&grants, tool, &paths).is_some())
    }

    pub async fn request_approval(
        &self,
        session_id: &str,
        tool_call_id: &str,
        tool: &str,
        input: serde_json::Value,
        agent: Option<serde_json::Value>,
    ) -> Result<(Approval, oneshot::Receiver<bool>)> {
        self.request_approval_with_risk(session_id, tool_call_id, tool, input, agent, None, None)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn request_approval_with_risk(
        &self,
        session_id: &str,
        tool_call_id: &str,
        tool: &str,
        input: serde_json::Value,
        agent: Option<serde_json::Value>,
        approval_risk: Option<&str>,
        approval_reason: Option<&str>,
    ) -> Result<(Approval, oneshot::Receiver<bool>)> {
        let store = self.store.clone();
        let session_id_owned = session_id.to_string();
        let tool_call_id = tool_call_id.to_string();
        let tool = tool.to_string();
        let approval_risk = approval_risk.map(str::to_string);
        let approval_reason = approval_reason.map(str::to_string);
        let policy = self.store.attention_policy_for_session(session_id)?;
        let priority = agent
            .as_ref()
            .and_then(|agent| agent.get("task_id"))
            .and_then(serde_json::Value::as_str)
            .and_then(|task_id| self.store.approval_task_priority(session_id, task_id).ok())
            .unwrap_or(0);
        let scope_hash = crate::approvals::approval_scope_hash(&input);
        let dedup_key = crate::approvals::approval_dedup_key(
            session_id,
            agent.as_ref(),
            &tool,
            &scope_hash,
            approval_risk.as_deref().unwrap_or("unknown"),
        );
        let now = time::OffsetDateTime::now_utc();
        let expires_at = policy
            .enabled
            .then(|| crate::approvals::approval_expires_at(now, approval_risk.as_deref(), &policy));
        let coalesce_window = if policy.enabled {
            policy.coalesce_window_secs
        } else {
            0
        };
        let notify_window = if policy.enabled {
            policy.notify_coalesce_secs
        } else {
            0
        };
        let max_pending = if policy.enabled {
            policy.max_pending_per_session
        } else {
            0
        };
        let agent_for_store = agent.clone();
        let risk_for_store = approval_risk.clone();
        let reason_for_store = approval_reason.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            store.create_approval_deduplicated(
                &session_id_owned,
                &tool_call_id,
                &tool,
                &input,
                agent_for_store.as_ref(),
                risk_for_store.as_deref(),
                reason_for_store.as_deref(),
                Some(&dedup_key),
                Some(&scope_hash),
                priority,
                expires_at.as_deref(),
                coalesce_window,
                notify_window,
                max_pending,
                &policy.priority_boost_risk,
            )
        })
        .await??;

        // Expired rows from the per-session cap deny their current waiters.
        for expired in &outcome.expired {
            self.deny_approval_waiters(&expired.id);
        }

        let (sender, receiver) = oneshot::channel();
        self.approval_waiters
            .lock()
            .map_err(|_| anyhow::anyhow!("approval mutex poisoned"))?
            .entry(outcome.approval.id.clone())
            .or_default()
            .push(sender);
        // Close the insert/resolve race: an operator may resolve before the
        // waiter is registered. Check the row once and deliver immediately if
        // it already reached a terminal status.
        if let Some(current) = self.store.get_approval(&outcome.approval.id)?
            && current.status != "pending"
        {
            let approved = current.status == "approved";
            if let Some(senders) = self
                .approval_waiters
                .lock()
                .ok()
                .and_then(|mut waiters| waiters.remove(&current.id))
            {
                for sender in senders {
                    let _ = sender.send(approved);
                }
            }
        }
        for event in &outcome.events {
            let _ = self.events.send(event.clone());
        }
        Ok((outcome.approval, receiver))
    }

    fn deny_approval_waiters(&self, approval_id: &str) {
        if let Some(senders) = self
            .approval_waiters
            .lock()
            .ok()
            .and_then(|mut waiters| waiters.remove(approval_id))
        {
            for sender in senders {
                let _ = sender.send(false);
            }
        }
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

    pub async fn probe_mcp_server(&self, server_id: &str) -> Result<McpProbeResult> {
        let store = self.store.clone();
        let id = server_id.to_string();
        let server = tokio::task::spawn_blocking(move || store.get_mcp_server(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("MCP server not found: {server_id}"))?;
        self.mcp.probe(&server).await
    }

    pub async fn add_mcp_server(&self, config: McpServerConfig) -> Result<McpServer> {
        let config = crate::mcp::normalize_config(config)?;
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.add_mcp_server(&config)).await?
    }

    pub async fn update_mcp_server(
        &self,
        id: &str,
        config: McpServerConfig,
    ) -> Result<Option<McpServer>> {
        let config = crate::mcp::normalize_config(config)?;
        let store = self.store.clone();
        let id = id.to_string();
        let updated =
            tokio::task::spawn_blocking(move || store.update_mcp_server(&id, &config)).await??;
        if let Some(server) = &updated {
            self.mcp.invalidate(&server.id).await;
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

    /// Add an ACP agent using the full transport-aware configuration.
    pub async fn add_acp_agent_config(&self, config: AcpAgentConfig) -> Result<AcpAgent> {
        let config = validate_acp_agent_config(config)?;
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.add_acp_agent_config(&config)).await?
    }

    /// Backward-compatible stdio-only add used by older internal callers.
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
        self.add_acp_agent_config(AcpAgentConfig {
            name: name.to_string(),
            command: command.to_string(),
            args,
            env,
            enabled,
            auto_approve,
            timeout_secs,
            transport: AcpTransport::Stdio,
            endpoint: None,
            auth_header_env: None,
            headers: Default::default(),
            connect_timeout_secs: 30,
            allow_remote: false,
        })
        .await
    }

    /// Update an ACP agent using the full transport-aware configuration.
    pub async fn update_acp_agent_config(
        &self,
        id: &str,
        config: AcpAgentConfig,
    ) -> Result<Option<AcpAgent>> {
        let config = validate_acp_agent_config(config)?;
        let store = self.store.clone();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || store.update_acp_agent_config(&id, &config)).await?
    }

    /// Backward-compatible stdio-only update used by older internal callers.
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
        self.update_acp_agent_config(
            id,
            AcpAgentConfig {
                name: name.to_string(),
                command: command.to_string(),
                args,
                env,
                enabled,
                auto_approve,
                timeout_secs,
                transport: AcpTransport::Stdio,
                endpoint: None,
                auth_header_env: None,
                headers: Default::default(),
                connect_timeout_secs: 30,
                allow_remote: false,
            },
        )
        .await
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
        self.probe_acp_agent_options(agent_id, project_id, true)
            .await
    }

    /// Probe an agent, optionally skipping session creation.
    pub async fn probe_acp_agent_options(
        &self,
        agent_id: &str,
        project_id: Option<String>,
        create_session: bool,
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
            .map(|server| acp::mcp_server(&server))
            .collect::<Result<Vec<_>>>()?;
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
            git_branch: None,
            mcp_servers,
            timeout: std::time::Duration::from_secs(agent.timeout_secs.min(120)),
            transport: agent.transport,
            endpoint: agent.endpoint.clone(),
            auth_header_env: agent.auth_header_env.clone(),
            headers: agent.headers.clone(),
            connect_timeout: std::time::Duration::from_secs(agent.connect_timeout_secs),
            allow_remote: agent.allow_remote,
            resume_session_id: None,
            diagnostics: None,
        };
        let result = acp::probe_with_options(launch, create_session).await;
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
            transport: agent.transport,
            endpoint: agent.endpoint,
            remote: agent.transport.is_remote(),
            load_session: result.load_session,
            capabilities: result.capabilities,
            http_version: result.http_version,
            messages_sent: result.messages_sent,
            messages_received: result.messages_received,
            streams_opened: result.streams_opened,
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

    /// Run one bounded browser action for an agent. The helper is session- and
    /// worktree-scoped and uses a non-persistent WebKit store. The physical GUI
    /// is owned by the unified `gui` lease for the action plus its project
    /// grace window; the warm helper itself holds no lease.
    pub async fn browser_action(
        &self,
        session_id: &str,
        worktree: &std::path::Path,
        agent_id: &str,
        agent_name: &str,
        input: serde_json::Value,
    ) -> Result<ToolOutput> {
        let target = browser::BrowserTarget {
            session_id,
            worktree,
            agent_id,
            agent_name,
        };
        self.browsers.action(self, &target, input).await
    }

    /// Stop browser helpers owned by a session before scratch cleanup and
    /// release the GUI lease immediately.
    pub async fn stop_session_browsers(&self, session_id: &str) -> usize {
        self.gui_end_session(session_id, "session_cleanup").await;
        self.browsers.stop_session(session_id).await
    }

    /// Stop browsers idle beyond the configured TTL.
    pub async fn sweep_idle_browsers(&self, idle: std::time::Duration) -> usize {
        self.browsers.sweep_idle(idle).await
    }

    /// Run one bounded native computer action for an agent. The helper is
    /// session- and worktree-scoped and runs with the logged-in user's
    /// authority. The unified `gui` lease serializes it with the browser.
    pub async fn computer_action(
        &self,
        session_id: &str,
        worktree: &std::path::Path,
        agent_id: &str,
        agent_name: &str,
        task_id: Option<&str>,
        input: serde_json::Value,
    ) -> Result<ToolOutput> {
        let target = computer::ComputerTarget {
            session_id,
            worktree,
            agent_id,
            agent_name,
            task_id,
        };
        self.computers.action(self, &target, input).await
    }

    /// Inspect helper availability and TCC permissions without taking the GUI
    /// lease or posting any input.
    pub async fn computer_status(&self) -> vp_protocol::types::ComputerStatusResult {
        self.computer_status_for(None, false).await
    }

    /// Cached helper/TCC status. When `session_id` is present its project
    /// policy controls the cache TTL and grace hint.
    pub async fn computer_status_for(
        &self,
        session_id: Option<&str>,
        refresh: bool,
    ) -> vp_protocol::types::ComputerStatusResult {
        let policy = match session_id {
            Some(session_id) => self
                .gui_policy_for_session(session_id)
                .await
                .unwrap_or_default(),
            None => vp_protocol::types::GuiPolicy::default(),
        };
        self.computers.status(&policy, refresh).await
    }

    /// Stop computer helpers owned by a session before scratch cleanup and
    /// release the GUI lease immediately.
    pub async fn stop_session_computers(&self, session_id: &str) -> usize {
        self.gui_end_session(session_id, "session_cleanup").await;
        self.computers.stop_session(session_id).await
    }

    /// Stop computer helpers idle beyond the configured TTL.
    pub async fn sweep_idle_computers(&self, idle: std::time::Duration) -> usize {
        self.computers.sweep_idle(idle).await
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
            git_branch: session.branch.clone(),
            remote: None,
            session_id: None,
            process_supervisor: None,
            test_cache: None,
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

    pub fn process_supervisor(&self) -> &Arc<process_supervisor::ProcessSupervisor> {
        &self.process_supervisor
    }

    /// Reconcile durable supervised processes after a daemon restart. Adoption
    /// requires full PID/start-time/nonce/command-hash evidence.
    pub async fn reconcile_supervised_processes(
        &self,
    ) -> Result<process_supervisor::ReconcileReport> {
        self.process_supervisor.reconcile().await
    }

    /// Idle-stop and retention-sweep durable processes. The daemon controls the
    /// cadence and can disable idle stops with
    /// `VERLINPARK_PROCESS_IDLE_SWEEP_SECS=0`.
    pub async fn sweep_supervised_processes(
        &self,
        idle_enabled: bool,
    ) -> Result<process_supervisor::SweepReport> {
        self.process_supervisor.sweep(idle_enabled).await
    }

    pub async fn stop_session_processes(&self, session_id: &str, reason: &str) -> usize {
        self.process_supervisor
            .stop_session(session_id, reason)
            .await
    }

    pub fn process_supervisor_status(&self) -> Result<ProcessSupervisorStatus> {
        self.process_supervisor.status()
    }

    pub async fn session_processes(
        &self,
        session_id: &str,
        include_finished: Option<bool>,
        limit: Option<u32>,
    ) -> Result<ProcessListResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || {
            if store.get_session(&id)?.is_none() {
                return Err(anyhow::anyhow!("session not found: {id}"));
            }
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        let rows = self.process_supervisor.list(
            session_id,
            include_finished.unwrap_or(true),
            limit.unwrap_or(50).clamp(1, 200),
        )?;
        Ok(ProcessListResult {
            session_id: session_id.to_string(),
            active: self.process_supervisor.active_count(session_id)?,
            processes: rows,
            limit: self
                .process_supervisor
                .policy_for_session(session_id)
                .max_processes,
        })
    }

    pub async fn session_process_logs(
        &self,
        session_id: &str,
        process_id: &str,
        offset: Option<u64>,
        max_bytes: Option<u32>,
        max_lines: Option<u32>,
        from_end: bool,
    ) -> Result<ProcessLogsResult> {
        let supervisor = Arc::clone(&self.process_supervisor);
        let session_id = session_id.to_string();
        let process_id = process_id.to_string();
        tokio::task::spawn_blocking(move || {
            supervisor.read_logs(
                &session_id,
                &process_id,
                offset,
                max_bytes,
                max_lines,
                from_end,
            )
        })
        .await?
    }

    pub async fn session_process_stop(
        &self,
        session_id: &str,
        process_id: &str,
        signal: Option<String>,
        grace_secs: Option<u32>,
    ) -> Result<ProcessStopResult> {
        self.process_supervisor
            .stop(
                session_id,
                process_id,
                signal.as_deref(),
                grace_secs.map(u64::from),
                "stopped by operator",
            )
            .await
    }

    /// Explicit operator retry for a non-live supervised process. The
    /// supervisor still revalidates identity and reuses the boot-restart env,
    /// cwd, sandbox, and bounded attempt machinery.
    pub async fn session_process_restart(
        &self,
        session_id: &str,
        process_id: &str,
        confirm: bool,
    ) -> Result<ProcessRestartResult> {
        let supervisor = Arc::clone(&self.process_supervisor);
        let session = session_id.to_string();
        let process = process_id.to_string();
        tokio::task::spawn_blocking(move || supervisor.request_restart(&session, &process, confirm))
            .await?
    }

    pub async fn workspace_tree(&self, session_id: &str) -> Result<WorkspaceTreeResult> {
        let root = self.workspace_root(session_id).await?;
        tokio::task::spawn_blocking(move || workspace::tree(Path::new(&root))).await?
    }

    pub async fn workspace_list(
        &self,
        session_id: &str,
        path: &str,
        offset: Option<u32>,
        limit: Option<u32>,
    ) -> Result<WorkspaceListResult> {
        let root = self.workspace_root(session_id).await?;
        let path = path.to_string();
        tokio::task::spawn_blocking(move || workspace::list(Path::new(&root), &path, offset, limit))
            .await?
    }

    pub async fn workspace_search(
        &self,
        session_id: &str,
        query: &str,
        limit: Option<u32>,
    ) -> Result<WorkspaceSearchResult> {
        let root = self.workspace_root(session_id).await?;
        let query = query.to_string();
        tokio::task::spawn_blocking(move || workspace::search(Path::new(&root), &query, limit))
            .await?
    }

    pub async fn workspace_grep(
        &self,
        session_id: &str,
        query: &str,
        path: Option<String>,
        case_sensitive: Option<bool>,
        max_results: Option<u32>,
    ) -> Result<WorkspaceGrepResult> {
        let root = self.workspace_root(session_id).await?;
        let query = query.to_string();
        tokio::task::spawn_blocking(move || {
            workspace::grep(
                Path::new(&root),
                &query,
                path.as_deref(),
                case_sensitive.unwrap_or(false),
                max_results,
            )
        })
        .await?
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
            git_branch: session.branch.clone(),
            remote: None,
            session_id: None,
            process_supervisor: None,
            test_cache: None,
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

    /// Sweep consoles and terminals that have been idle for `idle`. Called by
    /// the daemon so abandoned GUI sessions cannot accumulate child processes.
    pub async fn sweep_idle_terminals(&self, idle: std::time::Duration) -> (usize, usize) {
        let consoles = self.consoles.sweep_idle(idle).await;
        let terminals = self.terminals.sweep_idle(idle).await;
        (consoles, terminals)
    }

    pub async fn list_questions(&self, session_id: Option<String>) -> Result<QuestionListResult> {
        let store = self.store.clone();
        let questions =
            tokio::task::spawn_blocking(move || store.list_questions(session_id.as_deref()))
                .await??;
        Ok(QuestionListResult { questions })
    }

    pub async fn resolve_approval(&self, approval_id: &str, approved: bool) -> Result<bool> {
        let store = self.store.clone();
        let id = approval_id.to_string();
        let resolved =
            tokio::task::spawn_blocking(move || store.resolve_approval_with_events(&id, approved))
                .await??;
        if let Some((approval, events)) = resolved {
            self.release_approval_waiters(&approval.id, approved);
            for event in events {
                let _ = self.events.send(event);
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn release_approval_waiters(&self, approval_id: &str, approved: bool) {
        if let Some(senders) = self
            .approval_waiters
            .lock()
            .ok()
            .and_then(|mut waiters| waiters.remove(approval_id))
        {
            for sender in senders {
                let _ = sender.send(approved);
            }
        }
    }

    /// Resolve an explicit per-id decision list. There is intentionally no
    /// "approve every pending" shorthand; the caller must name each id.
    pub async fn resolve_approvals(
        &self,
        decisions: Vec<(String, bool)>,
    ) -> Result<ApprovalBatchResolveResult> {
        if decisions.is_empty() {
            anyhow::bail!("approval batch must contain at least one decision");
        }
        if decisions.len() > crate::approvals::MAX_BATCH_APPROVALS {
            anyhow::bail!(
                "approval batch accepts at most {} decisions",
                crate::approvals::MAX_BATCH_APPROVALS
            );
        }
        let mut seen = HashSet::new();
        for (id, _) in &decisions {
            if id.trim().is_empty() {
                anyhow::bail!("approval id cannot be empty");
            }
            if !seen.insert(id.clone()) {
                anyhow::bail!("approval id '{id}' appears more than once");
            }
        }
        let store = self.store.clone();
        let decisions_for_store = decisions.clone();
        let outcomes = tokio::task::spawn_blocking(move || {
            store.resolve_approvals_with_events(&decisions_for_store)
        })
        .await??;
        let mut results = Vec::with_capacity(outcomes.len());
        let mut resolved_count = 0u32;
        let mut failed_count = 0u32;
        for outcome in outcomes {
            let approved = decisions
                .iter()
                .find(|(id, _)| id == &outcome.approval_id)
                .map(|(_, approved)| *approved)
                .unwrap_or(false);
            if let Some(approval) = &outcome.approval {
                if outcome.code.is_none() {
                    self.release_approval_waiters(&approval.id, approved);
                    resolved_count = resolved_count.saturating_add(1);
                } else {
                    failed_count = failed_count.saturating_add(1);
                }
                for event in &outcome.events {
                    let _ = self.events.send(event.clone());
                }
                let code = outcome.code.clone();
                let message = code.as_deref().map(|code| {
                    let (hint, _) = crate::approvals::error_hint(code);
                    format!("approval is {}; {hint}", approval.status)
                });
                results.push(ApprovalBatchItemResult {
                    approval_id: outcome.approval_id,
                    resolved: outcome.code.is_none(),
                    status: Some(approval.status.clone()),
                    code,
                    message,
                    waiter_count: approval.waiter_count,
                });
            } else {
                failed_count = failed_count.saturating_add(1);
                results.push(ApprovalBatchItemResult {
                    approval_id: outcome.approval_id,
                    resolved: false,
                    status: outcome.status,
                    code: outcome.code.clone(),
                    message: Some(
                        crate::approvals::error_hint(crate::approvals::APPROVAL_NOT_FOUND)
                            .0
                            .to_string(),
                    ),
                    waiter_count: 0,
                });
            }
        }
        Ok(ApprovalBatchResolveResult {
            results,
            resolved_count,
            failed_count,
        })
    }

    /// Expire stale approvals for every project with attention enabled. The
    /// sweep denies waiters and never approves a tool call.
    pub async fn sweep_expired_approvals(&self) -> Result<usize> {
        let store = self.store.clone();
        let now = time::OffsetDateTime::now_utc();
        let expired =
            tokio::task::spawn_blocking(move || store.expire_stale_approvals(now)).await??;
        let mut count = 0;
        for (approval, events) in expired {
            self.deny_approval_waiters(&approval.id);
            for event in events {
                let _ = self.events.send(event);
            }
            count += 1;
        }
        Ok(count)
    }

    pub async fn approval_queue(
        &self,
        session_id: Option<String>,
        project_id: Option<String>,
    ) -> Result<ApprovalQueueResult> {
        let store = self.store.clone();
        let session_filter = session_id.clone();
        let project_filter = project_id.clone();
        let approvals = tokio::task::spawn_blocking(move || {
            store.list_pending_approvals(session_filter.as_deref(), project_filter.as_deref())
        })
        .await??;
        let policy = match (&session_id, &project_id) {
            (Some(session_id), _) => self.store.attention_policy_for_session(session_id)?,
            (None, Some(project_id)) => self.store.get_attention_policy(project_id)?,
            (None, None) => AttentionPolicy::default(),
        };
        Ok(ApprovalQueueResult {
            pending_count: approvals.len() as u32,
            approvals,
            policy,
            generated_at: now_rfc3339(),
        })
    }

    pub async fn list_approval_templates(&self, project_id: &str) -> Result<Vec<ApprovalTemplate>> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking(move || store.list_approval_templates(&project_id)).await?
    }

    pub async fn create_approval_template(
        &self,
        params: ApprovalTemplateCreateParams,
    ) -> Result<ApprovalTemplateResult> {
        let now = time::OffsetDateTime::now_utc();
        let action = crate::approvals::ApprovalTemplateAction::parse(&params.action)?;
        let tool_pattern = crate::approvals::validate_tool_pattern(&params.tool_pattern)?;
        let risk_levels = crate::approvals::validate_risk_levels(&params.risk_levels, action)?;
        let path_globs = crate::approvals::validate_path_globs(&params.path_globs)?;
        let expires_at =
            crate::approvals::validate_template_expiry(params.expires_at.as_deref(), now)?;
        let max_uses = crate::approvals::validate_max_uses(params.max_uses)?;
        let name = validate_approval_template_name(&params.name)?;
        let store = self.store.clone();
        let project_id = params.project_id.clone();
        let existing = store.list_approval_templates(&project_id)?;
        if existing.len() >= crate::approvals::MAX_APPROVAL_TEMPLATES {
            return Err(crate::approvals::ApprovalRuleError::invalid(format!(
                "a project supports at most {} approval templates",
                crate::approvals::MAX_APPROVAL_TEMPLATES
            ))
            .into());
        }
        if existing
            .iter()
            .any(|template| template.name.eq_ignore_ascii_case(&name))
        {
            return Err(crate::approvals::ApprovalRuleError::invalid(
                "approval template name already exists in this project",
            )
            .into());
        }
        let template = ApprovalTemplate {
            id: Uuid::now_v7().to_string(),
            project_id: project_id.clone(),
            name,
            enabled: true,
            tool_pattern,
            risk_levels,
            path_globs,
            action: action.as_str().to_string(),
            expires_at,
            max_uses,
            used_count: 0,
            created_by: params
                .created_by
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| "operator".to_string()),
            created_at: now
                .format(&time::format_description::well_known::Rfc3339)
                .map_err(|error| anyhow::anyhow!("formatting template time: {error}"))?,
            updated_at: None,
        };
        let store = self.store.clone();
        let template_for_store = template.clone();
        tokio::task::spawn_blocking(move || store.create_approval_template(&template_for_store))
            .await??;
        self.record_approval_audit(
            &project_id,
            "approval.template.created",
            serde_json::json!({
                "template_id": template.id,
                "name": template.name,
                "action": template.action,
                "tool_pattern": template.tool_pattern,
                "risk_levels": template.risk_levels,
                "path_globs": template.path_globs,
                "expires_at": template.expires_at,
                "max_uses": template.max_uses,
                "created_by": template.created_by,
            }),
        )
        .await?;
        self.record_project_manifest().await?;
        Ok(ApprovalTemplateResult { template })
    }

    pub async fn update_approval_template(
        &self,
        params: ApprovalTemplateUpdateParams,
    ) -> Result<ApprovalTemplateResult> {
        let now = time::OffsetDateTime::now_utc();
        let action = crate::approvals::ApprovalTemplateAction::parse(&params.action)?;
        let tool_pattern = crate::approvals::validate_tool_pattern(&params.tool_pattern)?;
        let risk_levels = crate::approvals::validate_risk_levels(&params.risk_levels, action)?;
        let path_globs = crate::approvals::validate_path_globs(&params.path_globs)?;
        let expires_at =
            crate::approvals::validate_template_expiry(params.expires_at.as_deref(), now)?;
        let max_uses = crate::approvals::validate_max_uses(params.max_uses)?;
        let name = validate_approval_template_name(&params.name)?;
        let store = self.store.clone();
        let existing = store.get_approval_template(&params.id)?.ok_or_else(|| {
            crate::approvals::ApprovalRuleError::new(
                crate::approvals::APPROVAL_TEMPLATE_INVALID,
                format!("approval template not found: {}", params.id),
            )
        })?;
        if !existing.name.eq_ignore_ascii_case(&name) {
            let siblings = store.list_approval_templates(&existing.project_id)?;
            if siblings
                .iter()
                .any(|template| template.name.eq_ignore_ascii_case(&name))
            {
                return Err(crate::approvals::ApprovalRuleError::invalid(
                    "approval template name already exists in this project",
                )
                .into());
            }
        }
        let template = ApprovalTemplate {
            id: existing.id,
            project_id: existing.project_id.clone(),
            name,
            enabled: params.enabled,
            tool_pattern,
            risk_levels,
            path_globs,
            action: action.as_str().to_string(),
            expires_at,
            max_uses,
            used_count: existing.used_count,
            created_by: existing.created_by,
            created_at: existing.created_at,
            updated_at: Some(
                now.format(&time::format_description::well_known::Rfc3339)
                    .map_err(|error| anyhow::anyhow!("formatting template time: {error}"))?,
            ),
        };
        let store = self.store.clone();
        let template_for_store = template.clone();
        let updated = tokio::task::spawn_blocking(move || {
            store.update_approval_template(&template_for_store)
        })
        .await??;
        if !updated {
            anyhow::bail!("approval template disappeared during update");
        }
        self.record_approval_audit(
            &template.project_id,
            "approval.template.updated",
            serde_json::json!({
                "template_id": template.id,
                "name": template.name,
                "enabled": template.enabled,
                "action": template.action,
                "tool_pattern": template.tool_pattern,
                "risk_levels": template.risk_levels,
                "path_globs": template.path_globs,
                "expires_at": template.expires_at,
                "max_uses": template.max_uses,
                "used_count": template.used_count,
            }),
        )
        .await?;
        self.record_project_manifest().await?;
        Ok(ApprovalTemplateResult { template })
    }

    pub async fn delete_approval_template(
        &self,
        params: ApprovalTemplateDeleteParams,
    ) -> Result<ApprovalTemplateDeleteResult> {
        let store = self.store.clone();
        let existing = store.get_approval_template(&params.id)?;
        let Some(existing) = existing else {
            return Ok(ApprovalTemplateDeleteResult { deleted: false });
        };
        let store = self.store.clone();
        let id = params.id.clone();
        let deleted =
            tokio::task::spawn_blocking(move || store.delete_approval_template(&id)).await??;
        if deleted {
            self.record_approval_audit(
                &existing.project_id,
                "approval.template.deleted",
                serde_json::json!({
                    "template_id": existing.id,
                    "name": existing.name,
                    "action": existing.action,
                    "used_count": existing.used_count,
                }),
            )
            .await?;
            self.record_project_manifest().await?;
        }
        Ok(ApprovalTemplateDeleteResult { deleted })
    }

    pub async fn approval_template_from_approval(
        &self,
        params: ApprovalTemplateFromApprovalParams,
    ) -> Result<ApprovalTemplateResult> {
        let store = self.store.clone();
        let approval = store.get_approval(&params.approval_id)?.ok_or_else(|| {
            crate::approvals::ApprovalRuleError::new(
                crate::approvals::APPROVAL_NOT_FOUND,
                format!("approval not found: {}", params.approval_id),
            )
        })?;
        let session = store
            .get_session(&approval.session_id)?
            .ok_or_else(|| anyhow::anyhow!("session not found: {}", approval.session_id))?;
        let now = time::OffsetDateTime::now_utc();
        let mut template = crate::approvals::template_from_approval(
            &approval,
            params.name.as_deref(),
            params.max_uses,
            params.expires_at.as_deref(),
            "operator",
            now,
        )?;
        template.project_id = session.project_id.clone();
        let existing = store.list_approval_templates(&session.project_id)?;
        if existing.len() >= crate::approvals::MAX_APPROVAL_TEMPLATES {
            return Err(crate::approvals::ApprovalRuleError::invalid(format!(
                "a project supports at most {} approval templates",
                crate::approvals::MAX_APPROVAL_TEMPLATES
            ))
            .into());
        }
        if existing
            .iter()
            .any(|candidate| candidate.name.eq_ignore_ascii_case(&template.name))
        {
            template.name = format!(
                "{} {}",
                template.name,
                &template.id[..8.min(template.id.len())]
            );
        }
        let store = self.store.clone();
        let template_for_store = template.clone();
        tokio::task::spawn_blocking(move || store.create_approval_template(&template_for_store))
            .await??;
        self.record_approval_audit(
            &template.project_id,
            "approval.template.created",
            serde_json::json!({
                "template_id": template.id,
                "name": template.name,
                "action": template.action,
                "tool_pattern": template.tool_pattern,
                "risk_levels": template.risk_levels,
                "path_globs": template.path_globs,
                "expires_at": template.expires_at,
                "max_uses": template.max_uses,
                "created_by": template.created_by,
                "from_approval": approval.id,
            }),
        )
        .await?;
        self.record_project_manifest().await?;
        Ok(ApprovalTemplateResult { template })
    }

    pub async fn get_attention_policy(&self, project_id: &str) -> Result<AttentionPolicyResult> {
        let project_id = project_id.to_string();
        let store = self.store.clone();
        let id = project_id.clone();
        let policy = tokio::task::spawn_blocking(move || store.get_attention_policy(&id)).await??;
        Ok(AttentionPolicyResult { project_id, policy })
    }

    pub async fn set_attention_policy(
        &self,
        params: AttentionPolicySetParams,
    ) -> Result<AttentionPolicyResult> {
        let policy = crate::approvals::validate_attention_policy(params.policy);
        let store = self.store.clone();
        let project_id = params.project_id.clone();
        let policy_for_store = policy.clone();
        tokio::task::spawn_blocking(move || {
            store.set_attention_policy(&project_id, &policy_for_store)
        })
        .await??;
        self.record_approval_audit(
            &params.project_id,
            "approval.policy.updated",
            serde_json::json!({
                "enabled": policy.enabled,
                "max_pending_per_session": policy.max_pending_per_session,
                "coalesce_window_secs": policy.coalesce_window_secs,
                "stale_after_secs": policy.stale_after_secs,
                "priority_boost_risk": policy.priority_boost_risk,
                "notify_coalesce_secs": policy.notify_coalesce_secs,
            }),
        )
        .await?;
        self.record_project_manifest().await?;
        Ok(AttentionPolicyResult {
            project_id: params.project_id,
            policy,
        })
    }

    pub async fn approval_audit(
        &self,
        project_id: &str,
        limit: Option<u32>,
    ) -> Result<ApprovalAuditResult> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        let events =
            tokio::task::spawn_blocking(move || store.list_approval_audit(&project_id, limit))
                .await??;
        Ok(ApprovalAuditResult { events })
    }

    async fn record_approval_audit(
        &self,
        project_id: &str,
        kind: &str,
        payload: serde_json::Value,
    ) -> Result<()> {
        let event = ApprovalAuditEvent {
            id: Uuid::now_v7().to_string(),
            project_id: project_id.to_string(),
            kind: kind.to_string(),
            payload,
            created_at: now_rfc3339(),
        };
        let store = self.store.clone();
        match tokio::task::spawn_blocking(move || store.insert_approval_audit(&event)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => {
                tracing::warn!(%error, kind, "failed to record approval audit event");
                Err(error)
            }
            Err(error) => {
                tracing::warn!(%error, kind, "approval audit task failed");
                Err(anyhow::anyhow!("approval audit task failed: {error}"))
            }
        }
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
                self.stop_session_browsers(&session.id).await;
                self.stop_session_computers(&session.id).await;
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
            self.stop_session_processes(&session.id, "worktree cleanup")
                .await;
            crate::tooling::lsp::stop_lsp_servers(&path);
            self.stop_session_browsers(&session.id).await;
            self.stop_session_computers(&session.id).await;
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

    // ---------------------------------------------------------------------
    // Task contracts and intent authorization
    // ---------------------------------------------------------------------

    pub async fn get_contract_policy(&self, project_id: &str) -> Result<ContractPolicy> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking(move || store.get_contract_policy(&project_id)).await?
    }

    pub async fn set_contract_policy(
        &self,
        project_id: &str,
        policy: ContractPolicy,
    ) -> Result<ContractPolicy> {
        let store = self.store.clone();
        let project_id_for_check = project_id.to_string();
        tokio::task::spawn_blocking(move || store.get_project(&project_id_for_check))
            .await??
            .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        let policy = policy
            .normalized()
            .map_err(|error| anyhow::anyhow!("invalid contract policy: {error}"))?;
        let for_store = policy.clone();
        let project_id = project_id.to_string();
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.set_contract_policy(&project_id, &for_store))
            .await??;
        Ok(policy)
    }

    // ---------------------------------------------------------------------
    // Effect receipts and postconditions
    // ---------------------------------------------------------------------

    pub async fn get_effect_policy(&self, project_id: &str) -> Result<EffectPolicy> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking(move || store.get_effect_policy(&project_id)).await?
    }

    pub async fn set_effect_policy(
        &self,
        project_id: &str,
        policy: EffectPolicy,
    ) -> Result<EffectPolicy> {
        let store = self.store.clone();
        let project_id_for_check = project_id.to_string();
        tokio::task::spawn_blocking(move || store.get_project(&project_id_for_check))
            .await??
            .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        let policy = policy
            .normalized()
            .map_err(|error| anyhow::anyhow!("invalid effect policy: {error}"))?;
        let for_store = policy.clone();
        let project_id = project_id.to_string();
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.set_effect_policy(&project_id, &for_store))
            .await??;
        Ok(policy)
    }

    /// Look up the logical effect before a call executes. A completed row is
    /// replayed, an unresolved row blocks execution, and a failed row starts a
    /// new attempt. The caller still passes the returned receipt to the journal
    /// intent so the idempotency check happens inside the same transaction.
    pub async fn prepare_tool_effect(
        &self,
        session_id: &str,
        task_id: Option<&str>,
        plan: &crate::effect::ToolEffectPlan,
    ) -> Result<crate::effect::EffectPrepare> {
        let store = self.store.clone();
        let session = session_id.to_string();
        let task = task_id.map(str::to_string);
        let tool = plan.tool.clone();
        let target = plan.target.canonical();
        let request_hash = plan.request_hash.clone();
        let existing = tokio::task::spawn_blocking(move || {
            store.latest_effect(&session, task.as_deref(), &tool, &target, &request_hash)
        })
        .await??;
        let prepared = match existing {
            Some(receipt) if receipt.state == EffectState::Done => {
                crate::effect::EffectPrepare::Replay(receipt)
            }
            Some(receipt)
                if matches!(receipt.state, EffectState::Intent | EffectState::Unknown) =>
            {
                crate::effect::EffectPrepare::Blocked(receipt)
            }
            Some(receipt) => crate::effect::EffectPrepare::Proceed(crate::effect::intent_receipt(
                session_id,
                task_id,
                plan,
                receipt.attempt.saturating_add(1),
            )),
            None => crate::effect::EffectPrepare::Proceed(crate::effect::intent_receipt(
                session_id, task_id, plan, 1,
            )),
        };
        Ok(prepared)
    }

    /// Persist a standalone effect intent (runtime actions such as a Git push,
    /// PR creation, landing, or remote integration). Returns the stored row and
    /// whether a new intent was actually inserted.
    pub async fn begin_effect(&self, receipt: EffectReceipt) -> Result<(EffectReceipt, bool)> {
        let store = self.store.clone();
        let (stored, events, inserted) =
            tokio::task::spawn_blocking(move || store.begin_effect(&receipt)).await??;
        for event in events {
            let _ = self.events.send(event);
        }
        Ok((stored, inserted))
    }

    /// Settle a standalone effect with bounded evidence. The stored state is
    /// terminal for that attempt.
    pub async fn settle_effect(&self, receipt: EffectReceipt) -> Result<EffectReceipt> {
        let store = self.store.clone();
        let (stored, events, _) =
            tokio::task::spawn_blocking(move || store.settle_effect(&receipt)).await??;
        for event in events {
            let _ = self.events.send(event);
        }
        Ok(stored)
    }

    /// Record an already-settled runtime effect (Worker commit, integration
    /// merge). The row and event commit together; an existing logical receipt
    /// is returned unchanged.
    pub async fn record_settled_effect(&self, receipt: EffectReceipt) -> Result<EffectReceipt> {
        let store = self.store.clone();
        let (stored, event, _) =
            tokio::task::spawn_blocking(move || store.record_settled_effect(&receipt)).await??;
        if let Some(event) = event {
            let _ = self.events.send(event);
        }
        Ok(stored)
    }

    pub async fn lookup_effect(
        &self,
        session_id: &str,
        task_id: Option<&str>,
        tool: &str,
        target: &str,
        request_hash: &str,
    ) -> Result<Option<EffectReceipt>> {
        let store = self.store.clone();
        let session = session_id.to_string();
        let task = task_id.map(str::to_string);
        let tool = tool.to_string();
        let target = target.to_string();
        let hash = request_hash.to_string();
        tokio::task::spawn_blocking(move || {
            store.latest_effect(&session, task.as_deref(), &tool, &target, &hash)
        })
        .await?
    }

    pub async fn get_effect(&self, id: &str) -> Result<Option<EffectReceipt>> {
        let store = self.store.clone();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || store.get_effect(&id)).await?
    }

    /// Unresolved receipts that block a task merge. The check always includes
    /// session-scoped rows (for example a partially failed PR or remote
    /// integration) and, when `task_id` is present, that task's rows too.
    pub async fn effect_blockers(
        &self,
        session_id: &str,
        task_id: Option<&str>,
    ) -> Result<Vec<EffectReceipt>> {
        let store = self.store.clone();
        let session = session_id.to_string();
        let rows = tokio::task::spawn_blocking(move || {
            store.list_effect_receipts(&session, None, false, 500)
        })
        .await??;
        let project_id = self.session_project_id(session_id).await?;
        let policy = self.get_effect_policy(&project_id).await?;
        let task = task_id.map(str::to_string);
        Ok(rows
            .into_iter()
            .filter(|receipt| {
                let in_scope = receipt.task_id.is_none()
                    || task
                        .as_deref()
                        .is_some_and(|task| receipt.task_id.as_deref() == Some(task));
                in_scope
                    && receipt.state.blocks_merge()
                    && policy.enabled
                    && policy.unknown_blocks_merge
            })
            .collect())
    }

    /// Fail-closed integration gate. This is checked before a queue entry is
    /// created and again immediately before the session branch is mutated.
    pub async fn assert_effects_settled(
        &self,
        session_id: &str,
        task_id: Option<&str>,
    ) -> Result<()> {
        let blockers = self.effect_blockers(session_id, task_id).await?;
        if blockers.is_empty() {
            return Ok(());
        }
        let refs = blockers.iter().collect::<Vec<_>>();
        Err(anyhow::Error::new(crate::effect::EffectBlocked {
            session_id: session_id.to_string(),
            task_id: task_id.map(str::to_string),
            receipt_ids: blockers.iter().map(|receipt| receipt.id.clone()).collect(),
            message: crate::effect::block_message(&refs),
        }))
    }

    async fn session_project_id(&self, session_id: &str) -> Result<String> {
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .map(|session| session.project_id)
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))
    }

    pub async fn session_effects(
        &self,
        session_id: &str,
        task_id: Option<&str>,
        include_settled: bool,
        limit: u32,
    ) -> Result<SessionEffectsResult> {
        let project_id = self.session_project_id(session_id).await?;
        let policy = self.get_effect_policy(&project_id).await?;
        let store = self.store.clone();
        let session = session_id.to_string();
        let task = task_id.map(str::to_string);
        let limit = limit.clamp(1, 500);
        let receipts = tokio::task::spawn_blocking(move || {
            store.list_effect_receipts(&session, task.as_deref(), include_settled, limit)
        })
        .await??;
        let other_store = self.store.clone();
        let other_session = session_id.to_string();
        let other_task = task_id.map(str::to_string);
        let rows = tokio::task::spawn_blocking(move || {
            other_store.list_effect_receipts(&other_session, other_task.as_deref(), true, 500)
        })
        .await??;
        let counts = crate::effect::counts(&rows, &policy);
        let blockers = crate::effect::blocking_receipts(&rows, &policy);
        let blocked = !blockers.is_empty();
        Ok(SessionEffectsResult {
            session_id: session_id.to_string(),
            policy,
            receipts,
            counts,
            blocked,
            block_reason: blocked.then(|| crate::effect::block_message(&blockers)),
            truncated: rows.len() >= 500,
        })
    }

    pub async fn reconcile_effect(
        &self,
        params: EffectReconcileParams,
    ) -> Result<EffectReconcileResult> {
        let receipt = self
            .get_effect(&params.effect_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("effect receipt not found: {}", params.effect_id))?;
        if let Some(session_id) = params.session_id.as_deref()
            && session_id != receipt.session_id
        {
            anyhow::bail!("effect receipt belongs to a different session");
        }
        let evidence = crate::effect::reconcile_evidence(params.decision, &params.evidence)?;
        let store = self.store.clone();
        let effect_id = params.effect_id.clone();
        let decision = params.decision;
        let operator = params.operator.clone();
        let (stored, events, changed) = tokio::task::spawn_blocking(move || {
            store.reconcile_effect(&effect_id, decision, &evidence, operator.as_deref())
        })
        .await??;
        for event in events {
            let _ = self.events.send(event);
        }
        Ok(EffectReconcileResult {
            receipt: stored,
            changed,
        })
    }

    /// Sweep stale intents across every project with an enabled effect policy.
    /// This is the daemon-restart path: an `intent` that outlived the grace
    /// period becomes `unknown`, never a silent retry.
    pub async fn sweep_stale_effect_intents(&self, limit: u32) -> Result<usize> {
        let store = self.store.clone();
        let projects = tokio::task::spawn_blocking(move || store.list_projects()).await??;
        let mut total = 0usize;
        let now = time::OffsetDateTime::now_utc();
        for project in projects {
            let policy = self.get_effect_policy(&project.id).await?;
            if !policy.enabled {
                continue;
            }
            let store = self.store.clone();
            let project_id = project.id.clone();
            let grace = policy.stale_intent_grace_secs;
            let events = tokio::task::spawn_blocking(move || {
                store.sweep_stale_effect_intents(&project_id, grace, limit, &now)
            })
            .await??;
            total = total.saturating_add(events.len());
            for event in events {
                let _ = self.events.send(event);
            }
        }
        Ok(total)
    }

    /// Derive and persist a contract from trusted inputs. The same inputs
    /// return the existing row unchanged; a changed plan/policy produces a new
    /// contract and a new `contract.derived` event.
    pub async fn derive_task_contract(
        &self,
        input: crate::contract::ContractDerivation,
    ) -> Result<crate::contract::TaskContract> {
        let mut contract =
            crate::contract::derive_contract(input).map_err(|error| anyhow::anyhow!("{error}"))?;
        let store = self.store.clone();
        let session_id = contract.session_id.clone();
        let task_id = contract.task_id.clone();
        let existing = tokio::task::spawn_blocking({
            let store = store.clone();
            let session_id = session_id.clone();
            let task_id = task_id.clone();
            move || store.latest_contract(&session_id, task_id.as_deref())
        })
        .await??;
        if let Some(row) = existing
            && let Ok(existing) =
                serde_json::from_str::<crate::contract::TaskContract>(&row.contract)
            && existing.hash == contract.hash
            && existing.verify_hash()
        {
            return Ok(existing);
        }
        contract.created_at = vp_protocol::now_rfc3339();
        let encoded = serde_json::to_string(&contract)?;
        let row = StoredContractRow {
            contract_id: contract.contract_id.clone(),
            project_id: contract.project_id.clone(),
            session_id: contract.session_id.clone(),
            task_id: contract.task_id.clone(),
            plan_revision: contract.plan_revision,
            role: contract.role.clone(),
            hash: contract.hash.clone(),
            source_hash: contract.source_hash.clone(),
            contract: encoded,
            created_at: contract.created_at.clone(),
        };
        // The audit event is written first. Replay/repair recreates the
        // projection if the process dies between the two writes.
        self.append_event(
            &contract.session_id,
            "contract.derived",
            serde_json::json!({
                "contract": contract,
                "created_at": contract.created_at,
            }),
        )
        .await?;
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.insert_contract(&row)).await??;
        Ok(contract)
    }

    pub async fn latest_task_contract(
        &self,
        session_id: &str,
        task_id: Option<&str>,
    ) -> Result<Option<crate::contract::TaskContract>> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let task_id = task_id.map(str::to_string);
        let row = tokio::task::spawn_blocking(move || {
            store.latest_contract(&session_id, task_id.as_deref())
        })
        .await??;
        let Some(row) = row else {
            return Ok(None);
        };
        let contract = serde_json::from_str::<crate::contract::TaskContract>(&row.contract)
            .context("stored task contract is malformed")?;
        if !contract.verify_hash() {
            anyhow::bail!("stored task contract hash does not match its body");
        }
        Ok(Some(contract))
    }

    pub async fn contract_usage(
        &self,
        contract_id: &str,
    ) -> Result<crate::contract::MutationUsage> {
        let store = self.store.clone();
        let contract_id = contract_id.to_string();
        let row =
            tokio::task::spawn_blocking(move || store.get_contract_usage(&contract_id)).await??;
        Ok(crate::contract::MutationUsage {
            writes: row.writes,
            bytes: row.bytes,
            commits: row.commits,
        })
    }

    /// Append the bounded `intent.decision` audit row. No argument values,
    /// prompt text, URLs, paths, or secrets are copied into the payload.
    pub async fn record_intent_decision(
        &self,
        contract: &crate::contract::TaskContract,
        decision: &crate::contract::ContractDecision,
        spec: &crate::contract::ContractCallSpec<'_>,
    ) -> Result<()> {
        let payload = decision.payload(contract, spec);
        self.append_event(&contract.session_id, "intent.decision", payload)
            .await?;
        Ok(())
    }

    /// Record the post-approval mutation delta and update the usage
    /// projection. The event is appended before the projection update.
    pub async fn record_intent_mutation(
        &self,
        contract: &crate::contract::TaskContract,
        delta: crate::contract::MutationDelta,
    ) -> Result<crate::contract::MutationUsage> {
        if delta.is_zero() {
            return self.contract_usage(&contract.contract_id).await;
        }
        self.append_event(
            &contract.session_id,
            "intent.mutation",
            serde_json::json!({
                "contract_id": contract.contract_id,
                "contract_hash": contract.hash,
                "task_id": contract.task_id,
                "writes": delta.writes,
                "bytes": delta.bytes,
                "commits": delta.commits,
            }),
        )
        .await?;
        let store = self.store.clone();
        let contract_id = contract.contract_id.clone();
        let project_id = contract.project_id.clone();
        let session_id = contract.session_id.clone();
        let task_id = contract.task_id.clone();
        let row = tokio::task::spawn_blocking(move || {
            store.add_contract_usage(
                &contract_id,
                &project_id,
                &session_id,
                task_id.as_deref(),
                delta.writes,
                delta.bytes,
                delta.commits,
            )
        })
        .await??;
        Ok(crate::contract::MutationUsage {
            writes: row.writes,
            bytes: row.bytes,
            commits: row.commits,
        })
    }

    pub async fn session_contract_status(
        &self,
        session_id: &str,
        task_id: Option<&str>,
    ) -> Result<SessionContractResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        let policy = self.get_contract_policy(&session.project_id).await?;
        let contract = self.latest_task_contract(session_id, task_id).await?;
        let usage = match &contract {
            Some(contract) => self.contract_usage(&contract.contract_id).await?,
            None => crate::contract::MutationUsage::default(),
        };
        let contract = contract.map(|contract| contract.metadata(policy.mode, usage));
        let decisions = self
            .recent_events_of_kind(session_id, "intent.decision", 20)
            .await?
            .into_iter()
            .filter(|event| {
                task_id.is_none_or(|task| {
                    event
                        .payload
                        .get("task_id")
                        .and_then(serde_json::Value::as_str)
                        == Some(task)
                })
            })
            .map(|event| IntentDecisionRecord {
                seq: event
                    .payload
                    .get("seq")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(event.seq),
                at: event
                    .payload
                    .get("at")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(&event.created_at)
                    .to_string(),
                contract_hash: event
                    .payload
                    .get("contract_hash")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                tool: event
                    .payload
                    .get("tool")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                task_id: event
                    .payload
                    .get("task_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                role: event
                    .payload
                    .get("role")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                rule: event
                    .payload
                    .get("rule")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                decision: event
                    .payload
                    .get("decision")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                risk: event
                    .payload
                    .get("risk")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                reason: event
                    .payload
                    .get("reason")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            })
            .collect();
        Ok(SessionContractResult {
            session_id: session.id,
            project_id: session.project_id,
            contract,
            policy,
            recent_decisions: decisions,
        })
    }

    // ---------------------------------------------------------------------
    // MCP tool schema pins
    // ---------------------------------------------------------------------

    pub async fn sync_mcp_pins(
        &self,
        session_id: &str,
        project_id: &str,
        server: &McpServer,
        tools: &[vp_protocol::types::McpTool],
    ) -> Result<Vec<McpToolPin>> {
        let _pin_guard = self.pin_sync.lock().await;
        let store = self.store.clone();
        let project = project_id.to_string();
        let server_id = server.id.clone();
        let existing = tokio::task::spawn_blocking({
            let store = store.clone();
            let project = project.clone();
            let server_id = server_id.clone();
            move || store.list_mcp_tool_pins(&project, Some(&server_id))
        })
        .await??;
        let now = vp_protocol::now_rfc3339();
        let result = crate::mcp_pins::sync_pins(project_id, server, tools, &existing, &now);
        // Pin lifecycle events are appended before the project rows are
        // updated. The rows are project configuration, not a session replay
        // projection, so a crash after the event leaves a stale row that the
        // next sync repairs and re-emits.
        for change in &result.changes {
            let payload = crate::mcp_pins::change_payload(change, result.first_sync);
            self.append_event(session_id, change.kind.event_kind(), payload)
                .await?;
        }
        for pin in &result.pins {
            let store = store.clone();
            let pin = pin.clone();
            tokio::task::spawn_blocking(move || store.upsert_mcp_tool_pin(&pin)).await??;
        }
        Ok(result.pins)
    }

    pub async fn project_mcp_pins(
        &self,
        project_id: &str,
        server_id: Option<&str>,
    ) -> Result<ProjectMcpPinsResult> {
        let store = self.store.clone();
        let project = project_id.to_string();
        let server = server_id.map(str::to_string);
        let pins = tokio::task::spawn_blocking(move || {
            store.list_mcp_tool_pins(&project, server.as_deref())
        })
        .await??;
        let mut result = ProjectMcpPinsResult {
            project_id: project_id.to_string(),
            pins,
            ..ProjectMcpPinsResult::default()
        };
        for pin in &result.pins {
            match pin.status.as_str() {
                "pinned" => result.pinned += 1,
                "drift" => result.drift += 1,
                "unverified" => result.unverified += 1,
                "removed" => result.removed += 1,
                _ => {}
            }
        }
        Ok(result)
    }

    pub async fn approve_mcp_pin(
        &self,
        params: &McpPinApproveParams,
        session_id: Option<&str>,
    ) -> Result<McpPinActionResult> {
        let store = self.store.clone();
        let project = params.project_id.clone();
        let server = params.server_id.clone();
        let pins =
            tokio::task::spawn_blocking(move || store.list_mcp_tool_pins(&project, Some(&server)))
                .await??;
        let mut pin = pins
            .into_iter()
            .find(|pin| pin.tool_name == params.tool_name)
            .ok_or_else(|| anyhow::anyhow!("MCP tool pin not found"))?;
        if pin.observed_schema_hash != params.expected_schema_hash
            || pin.observed_description_hash != params.expected_description_hash
        {
            anyhow::bail!(
                "expected pin hashes do not match the last observation; rediscover the tool and retry"
            );
        }
        let now = vp_protocol::now_rfc3339();
        pin.status = "pinned".to_string();
        pin.schema_hash = params.expected_schema_hash.clone();
        pin.description_hash = params.expected_description_hash.clone();
        pin.server_fingerprint = pin.observed_server_fingerprint.clone();
        pin.approved_at = Some(now.clone());
        pin.approved_by = Some(
            params
                .approved_by
                .clone()
                .unwrap_or_else(|| "operator".to_string()),
        );
        pin.last_seen_at = now;
        if let Some(session_id) = self
            .resolve_pin_audit_session(session_id, &params.project_id)
            .await?
        {
            self.append_event(
                &session_id,
                "mcp.tool.approved",
                serde_json::json!({
                    "project_id": pin.project_id,
                    "server_id": pin.server_id,
                    "server_name": pin.server_name,
                    "tool_name": pin.tool_name,
                    "schema_hash": pin.schema_hash,
                    "description_hash": pin.description_hash,
                    "approved_by": pin.approved_by,
                    "approved_at": pin.approved_at,
                }),
            )
            .await?;
        }
        let store = self.store.clone();
        let for_store = pin.clone();
        tokio::task::spawn_blocking(move || store.upsert_mcp_tool_pin(&for_store)).await??;
        Ok(McpPinActionResult {
            pin: Some(pin),
            status: "pinned".to_string(),
            message: "MCP tool pin approved".to_string(),
        })
    }

    pub async fn reject_mcp_pin(
        &self,
        params: &McpPinRejectParams,
        session_id: Option<&str>,
    ) -> Result<McpPinActionResult> {
        let store = self.store.clone();
        let project = params.project_id.clone();
        let server = params.server_id.clone();
        let pins =
            tokio::task::spawn_blocking(move || store.list_mcp_tool_pins(&project, Some(&server)))
                .await??;
        let pin = pins
            .into_iter()
            .find(|pin| pin.tool_name == params.tool_name)
            .ok_or_else(|| anyhow::anyhow!("MCP tool pin not found"))?;
        if let Some(session_id) = self
            .resolve_pin_audit_session(session_id, &params.project_id)
            .await?
        {
            self.append_event(
                &session_id,
                "mcp.tool.rejected",
                serde_json::json!({
                    "project_id": pin.project_id,
                    "server_id": pin.server_id,
                    "server_name": pin.server_name,
                    "tool_name": pin.tool_name,
                    "status": pin.status,
                    "schema_hash": pin.schema_hash,
                    "description_hash": pin.description_hash,
                    "reason": params.reason.as_deref().unwrap_or("operator rejected the observed pin"),
                }),
            )
            .await?;
        }
        Ok(McpPinActionResult {
            status: pin.status.clone(),
            message: "MCP tool pin was not approved; the observed status remains visible"
                .to_string(),
            pin: Some(pin),
        })
    }

    async fn resolve_pin_audit_session(
        &self,
        session_id: Option<&str>,
        project_id: &str,
    ) -> Result<Option<String>> {
        if let Some(session_id) = session_id {
            return Ok(Some(session_id.to_string()));
        }
        let store = self.store.clone();
        let project = project_id.to_string();
        tokio::task::spawn_blocking(move || store.latest_session_for_project(&project)).await?
    }

    pub async fn mcp_pin_trust_for_tool(
        &self,
        project_id: &str,
        server_id: &str,
        tool_name: &str,
    ) -> Result<crate::contract::McpPinTrust> {
        let store = self.store.clone();
        let project = project_id.to_string();
        let server = server_id.to_string();
        let pins =
            tokio::task::spawn_blocking(move || store.list_mcp_tool_pins(&project, Some(&server)))
                .await??;
        let Some(pin) = pins.into_iter().find(|pin| pin.tool_name == tool_name) else {
            return Ok(crate::contract::McpPinTrust::Unverified);
        };
        Ok(match pin.status.as_str() {
            "pinned" if crate::mcp_pins::pin_is_callable(&pin) => {
                crate::contract::McpPinTrust::Trusted
            }
            "pinned" => crate::contract::McpPinTrust::Drift,
            "drift" => crate::contract::McpPinTrust::Drift,
            "removed" => crate::contract::McpPinTrust::Removed,
            _ => crate::contract::McpPinTrust::Unverified,
        })
    }

    pub async fn find_mcp_server(&self, selector: &str) -> Result<Option<McpServer>> {
        let servers = self.list_mcp_servers().await?.servers;
        Ok(servers
            .into_iter()
            .find(|server| server.id == selector || server.name == selector))
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

fn memory_verify_after(days: Option<u32>) -> Option<String> {
    let days = days?;
    let when = time::OffsetDateTime::now_utc() + time::Duration::days(i64::from(days));
    Some(
        when.format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| now_rfc3339()),
    )
}

fn verify_memory_evidence(
    store: &Store,
    project_root: &Path,
    project_id: &str,
    memory_session_id: &str,
    evidence: &[MemoryEvidence],
) -> Vec<MemoryVerificationCheck> {
    evidence
        .iter()
        .map(|item| {
            let (status, detail) = match item.kind.as_str() {
                "commit" => verify_commit_evidence(project_root, &item.reference),
                "path" => verify_path_evidence(project_root, &item.reference),
                "task" => match store.plan_task_exists(project_id, item.reference.trim()) {
                    Ok(true) => (
                        "verified".to_string(),
                        "task exists in this project".to_string(),
                    ),
                    Ok(false) => (
                        "missing".to_string(),
                        "task not found in this project".to_string(),
                    ),
                    Err(error) => (
                        "unchecked".to_string(),
                        format!("task lookup failed: {error}"),
                    ),
                },
                "session" => match store.get_session(item.reference.trim()) {
                    Ok(Some(_)) => ("verified".to_string(), "session exists".to_string()),
                    Ok(None) => ("missing".to_string(), "session not found".to_string()),
                    Err(error) => (
                        "unchecked".to_string(),
                        format!("session lookup failed: {error}"),
                    ),
                },
                "event" => match parse_event_reference(&item.reference, memory_session_id) {
                    Some((session_id, seq)) => match store.event_exists(&session_id, seq) {
                        Ok(true) => (
                            "verified".to_string(),
                            format!("event {session_id}:{seq} exists"),
                        ),
                        Ok(false) => (
                            "missing".to_string(),
                            format!("event {session_id}:{seq} not found"),
                        ),
                        Err(error) => (
                            "unchecked".to_string(),
                            format!("event lookup failed: {error}"),
                        ),
                    },
                    None => (
                        "unchecked".to_string(),
                        "event reference is not '<session>:<seq>' or '<seq>'".to_string(),
                    ),
                },
                "url" => (
                    "unchecked".to_string(),
                    "URL evidence is not fetched by local verification".to_string(),
                ),
                other => (
                    "unchecked".to_string(),
                    format!("unsupported evidence kind: {other}"),
                ),
            };
            MemoryVerificationCheck {
                evidence: item.clone(),
                status,
                detail,
            }
        })
        .collect()
}

fn verify_commit_evidence(project_root: &Path, reference: &str) -> (String, String) {
    let reference = reference.trim();
    if reference.is_empty() {
        return ("missing".to_string(), "empty commit reference".to_string());
    }
    let repo = std::process::Command::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["rev-parse", "--git-dir"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output();
    match repo {
        Ok(output) if output.status.success() => {}
        Ok(_) | Err(_) => {
            return (
                "unchecked".to_string(),
                "project root is not a git repository".to_string(),
            );
        }
    }
    let object = std::process::Command::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["cat-file", "-e", &format!("{reference}^{{commit}}")])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output();
    match object {
        Ok(output) if output.status.success() => {}
        Ok(_) => {
            return (
                "missing".to_string(),
                format!("commit object {reference} not found"),
            );
        }
        Err(error) => {
            return (
                "missing".to_string(),
                format!("git cat-file failed: {error}"),
            );
        }
    }
    let ancestor = std::process::Command::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["merge-base", "--is-ancestor", reference, "HEAD"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output();
    match ancestor {
        Ok(output) if output.status.success() => (
            "verified".to_string(),
            format!("{reference} is an ancestor of HEAD"),
        ),
        Ok(output) if output.status.code() == Some(1) => (
            "missing".to_string(),
            format!("{reference} is not an ancestor of HEAD"),
        ),
        Ok(output) => (
            "missing".to_string(),
            format!(
                "git merge-base failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ),
        Err(error) => (
            "missing".to_string(),
            format!("git merge-base failed: {error}"),
        ),
    }
}

fn verify_path_evidence(project_root: &Path, reference: &str) -> (String, String) {
    let reference = reference.trim();
    if reference.is_empty() {
        return ("missing".to_string(), "empty path reference".to_string());
    }
    let path = Path::new(reference);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return (
            "missing".to_string(),
            "path escapes the project root".to_string(),
        );
    }
    let candidate = project_root.join(path);
    if !candidate.exists() {
        return (
            "missing".to_string(),
            "path not found under the project root".to_string(),
        );
    }
    if let (Ok(root), Ok(resolved)) = (project_root.canonicalize(), candidate.canonicalize())
        && !resolved.starts_with(&root)
    {
        return (
            "missing".to_string(),
            "path resolves outside the project root".to_string(),
        );
    }
    (
        "verified".to_string(),
        format!("exists at {}", candidate.display()),
    )
}

fn parse_event_reference(reference: &str, fallback_session_id: &str) -> Option<(String, u64)> {
    let reference = reference.trim();
    if let Some((session_id, seq)) = reference.split_once(':') {
        let seq = seq.trim().parse::<u64>().ok()?;
        let session_id = session_id.trim();
        if session_id.is_empty() {
            return None;
        }
        return Some((session_id.to_string(), seq));
    }
    reference
        .parse::<u64>()
        .ok()
        .map(|seq| (fallback_session_id.to_string(), seq))
}

/// A failed push is `unknown` only when the transport may have reached the
/// remote. A local rejection (non-fast-forward, missing remote) is `failed`.
fn push_error_is_ambiguous(error: &anyhow::Error) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("timed out")
        || message.contains("timeout")
        || message.contains("connection reset")
        || message.contains("connection refused")
        || message.contains("could not resolve")
        || message.contains("unable to access")
        || message.contains("ssl")
        || message.contains("tls")
        || message.contains("remote end hung up")
}

/// A forge create error is `unknown` when the provider may have created the
/// change despite the local failure. This is exactly the double-PR case the
/// receipt prevents; the operator reconciles rather than retries blindly.
fn forge_error_is_ambiguous(error: &crate::forge::ForgeError) -> bool {
    matches!(
        error.code.as_str(),
        "FORGE_TIMEOUT"
            | "FORGE_SERVICE_UNAVAILABLE"
            | "FORGE_NETWORK_FAILED"
            | "FORGE_CONFLICT"
            | "FORGE_RESPONSE_INVALID"
    )
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

fn validate_acp_agent_config(config: AcpAgentConfig) -> Result<AcpAgentConfig> {
    acp::validate_agent_config(config)
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

async fn ensure_session_not_active(store: &Store, session_id: &str, action: &str) -> Result<()> {
    let store = store.clone();
    let id = session_id.to_string();
    let session = tokio::task::spawn_blocking(move || store.get_session(&id))
        .await??
        .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
    if matches!(
        session.status.as_str(),
        "queued" | "running" | "paused" | "waiting_input"
    ) {
        anyhow::bail!(
            "session must be idle to {action}; current status is {}",
            session.status
        );
    }
    Ok(())
}

/// Cost is recorded twice when a usage entry has a price: `agent.usage`
/// carries the per-call cost and `agent.cost` carries the running session total.
/// Prefer the latest total so both `record_agent_usage` and the legacy
/// `record_cost` path replay identically; fall back to summing older usage
/// events that predate `agent.cost`.
/// Fold conflict lifecycle events last-write-wins by conflict id. A deleted
/// memory also tombstones conflicts that reference it, matching the projection
/// delete in `Store::delete_memories_with_events`.
fn replay_memory_conflicts(events: &[Event]) -> Vec<MemoryConflict> {
    let mut conflicts: HashMap<String, MemoryConflict> = HashMap::new();
    for event in events {
        match event.kind.as_str() {
            "memory.conflict.detected" | "memory.conflict.resolved" => {
                if let Ok(conflict) =
                    serde_json::from_value::<MemoryConflict>(event.payload.clone())
                {
                    conflicts.insert(conflict.id.clone(), conflict);
                }
            }
            "memory.deleted" => {
                if let Ok(memory) = serde_json::from_value::<MemoryItem>(event.payload.clone()) {
                    conflicts.retain(|_, conflict| {
                        conflict.memory_a_id != memory.id && conflict.memory_b_id != memory.id
                    });
                }
            }
            _ => {}
        }
    }
    let mut conflicts = conflicts.into_values().collect::<Vec<_>>();
    conflicts.sort_by(|left, right| {
        left.scope_kind
            .cmp(&right.scope_kind)
            .then_with(|| left.scope_value.cmp(&right.scope_value))
            .then_with(|| left.memory_a_id.cmp(&right.memory_a_id))
            .then_with(|| left.memory_b_id.cmp(&right.memory_b_id))
            .then_with(|| right.detected_at.cmp(&left.detected_at))
            .then_with(|| left.id.cmp(&right.id))
    });
    conflicts
}

fn replay_session_prompt_layout(events: &[Event]) -> Option<u32> {
    events
        .iter()
        .rev()
        .find_map(|event| match event.kind.as_str() {
            "session.prompt_layout" => event
                .payload
                .get("prompt_layout")
                .and_then(serde_json::Value::as_u64)
                .map(|value| value as u32)
                .filter(|value| matches!(value, 2 | 3)),
            "session.created" => event
                .payload
                .get("prompt_layout")
                .and_then(serde_json::Value::as_u64)
                .map(|value| value as u32)
                .filter(|value| matches!(value, 2 | 3)),
            _ => None,
        })
}

fn replay_session_unlimited(events: &[Event]) -> bool {
    events
        .iter()
        .rev()
        .find_map(|event| match event.kind.as_str() {
            "session.budget_mode_changed" => event
                .payload
                .get("unlimited")
                .and_then(serde_json::Value::as_bool),
            "session.created" => event
                .payload
                .get("unlimited")
                .and_then(serde_json::Value::as_bool),
            _ => None,
        })
        .unwrap_or(false)
}

/// Stall observability threshold. `VERLINPARK_TASK_STALL_SECS=0` disables the
/// sweep; malformed or absent values fall back to 30 minutes.
pub fn task_stall_threshold_secs() -> u64 {
    parse_task_stall_threshold(std::env::var("VERLINPARK_TASK_STALL_SECS").ok().as_deref())
}

pub fn parse_task_stall_threshold(value: Option<&str>) -> u64 {
    value
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(1_800)
}

fn replay_steering(events: &[Event]) -> Vec<SteeringMessage> {
    let mut latest = HashMap::new();
    for event in events {
        if !matches!(
            event.kind.as_str(),
            "steering.created" | "steering.delivered" | "steering.cancelled"
        ) {
            continue;
        }
        if let Ok(message) = serde_json::from_value::<SteeringMessage>(event.payload.clone()) {
            latest.insert(message.id.clone(), message);
        }
    }
    let mut messages = latest.into_values().collect::<Vec<_>>();
    messages.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    messages
}

fn replay_continuations(events: &[Event]) -> Vec<SessionContinuation> {
    let mut latest = HashMap::new();
    for event in events {
        if !matches!(
            event.kind.as_str(),
            "session.continuation.created"
                | "session.continuation.ready"
                | "session.continuation.delivering"
                | "session.continuation.reclaimed"
                | "session.continuation.retrying"
                | "session.continuation.delivered"
                | "session.continuation.scheduled"
                | "session.continuation.cancelled"
                | "session.continuation.failed"
        ) {
            continue;
        }
        if let Ok(continuation) =
            serde_json::from_value::<SessionContinuation>(event.payload.clone())
        {
            latest.insert(continuation.id.clone(), continuation);
        }
    }
    let mut continuations = latest.into_values().collect::<Vec<_>>();
    continuations.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    continuations
}

fn recovery_key(recovery: &OrchestrationRecovery) -> (String, u64, String) {
    (
        recovery.session_id.clone(),
        recovery.plan_seq,
        recovery.task_id.clone(),
    )
}

fn replay_session_cost(events: &[Event]) -> f64 {
    if let Some(total) = events.iter().rev().find_map(|event| {
        if event.kind != "agent.cost" {
            return None;
        }
        event
            .payload
            .get("total_usd")
            .and_then(serde_json::Value::as_f64)
    }) {
        return total;
    }
    events
        .iter()
        .filter(|event| event.kind == "agent.usage")
        .map(|event| {
            event
                .payload
                .get("cost_usd")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0)
        })
        .sum()
}

fn validate_intake_dependencies(
    tasks: &[IntakeTask],
    task_id: Option<&str>,
    deps: &[String],
) -> Result<()> {
    if deps.len() > 20 {
        anyhow::bail!("intake supports at most 20 dependencies");
    }
    let by_id = tasks
        .iter()
        .map(|task| (task.id.as_str(), task))
        .collect::<HashMap<_, _>>();
    for dep in deps {
        if Some(dep.as_str()) == task_id {
            anyhow::bail!("an intake task cannot depend on itself");
        }
        if !by_id.contains_key(dep.as_str()) {
            anyhow::bail!("unknown intake dependency: {dep}");
        }
    }
    let Some(task_id) = task_id else {
        return Ok(());
    };
    // Editing can introduce a cycle. Walk existing dependency edges from each
    // new dependency and reject a path back to the edited task.
    let mut stack = deps.to_vec();
    let mut visited = std::collections::HashSet::new();
    while let Some(current) = stack.pop() {
        if current == task_id {
            anyhow::bail!("intake dependencies would create a cycle");
        }
        if !visited.insert(current.clone()) {
            continue;
        }
        if let Some(task) = by_id.get(current.as_str()) {
            stack.extend(task.deps.iter().cloned());
        }
    }
    Ok(())
}

impl Engine {
    /// Manifest snapshots are a rebuild aid; a failed append must not turn a
    /// successfully committed SQLite mutation into a user-visible failure and
    /// invite a duplicate retry.
    pub(crate) async fn record_project_manifest_best_effort(&self) {
        let store = self.store.clone();
        match tokio::task::spawn_blocking(move || store.record_project_manifest()).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::warn!(%error, "failed to update the project manifest after an intake change");
            }
            Err(error) => {
                tracing::warn!(%error, "project manifest task failed after an intake change");
            }
        }
    }
}

struct SourceFileInput {
    relative: String,
    title: String,
    mime_type: String,
    content: String,
}

struct ImportScan {
    files: Vec<SourceFileInput>,
    skipped: usize,
}

fn validate_source_title(title: &str) -> Result<()> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > 200 || title.chars().any(char::is_control) {
        anyhow::bail!("source title must be 1-200 characters without control characters");
    }
    Ok(())
}

fn validate_source_labels(labels: Vec<String>) -> Result<Vec<String>> {
    if labels.len() > 12 {
        anyhow::bail!("a source supports at most 12 labels");
    }
    let mut output = Vec::new();
    for label in labels {
        let label = label.trim();
        if label.is_empty() || label.chars().count() > 40 || label.chars().any(char::is_control) {
            anyhow::bail!("source labels must be 1-40 characters without control characters");
        }
        if !output.iter().any(|existing| existing == label) {
            output.push(label.to_string());
        }
    }
    Ok(output)
}

fn build_source_chunks(source_id: &str, content: &str) -> Vec<SourceChunk> {
    crate::sources::chunk_text(content)
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            let digest = sha256_hex(
                format!(
                    "{}:{}:{}:{}",
                    source_id, index, chunk.start_offset, chunk.content
                )
                .as_bytes(),
            );
            SourceChunk {
                id: format!("{source_id}:{:04}:{}", index + 1, &digest[..12]),
                source_id: source_id.to_string(),
                chunk_no: (index + 1) as u32,
                heading: chunk.heading,
                content: chunk.content,
                start_offset: chunk.start_offset,
                end_offset: chunk.end_offset,
                token_count: chunk.token_count,
            }
        })
        .collect()
}

fn read_source_file(root: &Path, relative: &str) -> Result<SourceFileInput> {
    let root = root.canonicalize().context("resolving project root")?;
    let relative_path = Path::new(relative.trim());
    if relative.trim().is_empty()
        || relative_path.is_absolute()
        || relative_path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
        || relative_path.components().any(|component| {
            component
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(".git")
        })
    {
        anyhow::bail!("source path must be relative, inside the project, and outside .git");
    }
    let path = root
        .join(relative_path)
        .canonicalize()
        .with_context(|| format!("resolving source file {relative}"))?;
    if !path.starts_with(&root) {
        anyhow::bail!("source path escapes the project root");
    }
    let metadata = std::fs::metadata(&path)?;
    if !metadata.is_file() {
        anyhow::bail!("source path is not a file: {relative}");
    }
    if metadata.len() > crate::sources::MAX_SOURCE_BYTES {
        anyhow::bail!(
            "source file is {} bytes; the limit is {} bytes",
            metadata.len(),
            crate::sources::MAX_SOURCE_BYTES
        );
    }
    let bytes = std::fs::read(&path)?;
    if bytes.contains(&0) {
        anyhow::bail!("source file looks binary");
    }
    let content = String::from_utf8(bytes).context("source file is not UTF-8")?;
    let relative = path
        .strip_prefix(&root)
        .unwrap_or(&path)
        .to_string_lossy()
        .to_string();
    Ok(SourceFileInput {
        title: relative.clone(),
        mime_type: source_mime_type(&relative),
        relative,
        content,
    })
}

const MAX_SOURCE_IMPORT_SCAN: usize = 20_000;

struct GovernorCandidateInfo {
    task: vp_protocol::types::PlanTask,
    effective_priority: i64,
    enqueued_at: String,
}

/// Build the same deterministic ready-candidate set used by governor status and
/// preview. The live scheduler additionally applies `next_attempt_at` gating;
/// this read-only view is intentionally simpler and never mutates a task.
fn governor_candidate_infos(
    session: &Session,
    plan: &TeamPlan,
    scheduled: &[ScheduledTask],
    orchestration: &OrchestrationPolicy,
) -> Vec<GovernorCandidateInfo> {
    let by_task = scheduled
        .iter()
        .map(|task| (task.task_id.as_str(), task))
        .collect::<HashMap<_, _>>();
    let statuses = scheduled
        .iter()
        .map(|task| (task.task_id.clone(), task.status.clone()))
        .collect::<HashMap<_, _>>();
    crate::orchestration::ready_tasks(plan, &statuses, plan.tasks.len())
        .into_iter()
        .map(|task| {
            let row = by_task.get(task.id.as_str());
            let retry_boost = row
                .filter(|row| {
                    row.attempts > 0 && matches!(row.status.as_str(), "failed" | "pending")
                })
                .map(|_row| i64::from(orchestration.retry_priority_boost))
                .unwrap_or(0);
            let base = session
                .priority
                .saturating_add(task.priority)
                .clamp(-100, 100);
            GovernorCandidateInfo {
                task: task.clone(),
                effective_priority: i64::from(base).saturating_add(retry_boost),
                enqueued_at: row
                    .map(|row| row.enqueued_at.clone())
                    .unwrap_or_else(|| session.created_at.clone()),
            }
        })
        .collect()
}

fn collect_import_files(
    root: &Path,
    relative: Option<&str>,
    max_files: usize,
) -> Result<ImportScan> {
    let root = root.canonicalize().context("resolving project root")?;
    let search_root = match relative.map(str::trim).filter(|value| !value.is_empty()) {
        Some(relative) => {
            let path = Path::new(relative);
            if path.is_absolute()
                || path.components().any(|component| {
                    matches!(
                        component,
                        std::path::Component::ParentDir
                            | std::path::Component::RootDir
                            | std::path::Component::Prefix(_)
                    )
                })
            {
                anyhow::bail!("source import path must be relative and inside the project");
            }
            let resolved = root
                .join(path)
                .canonicalize()
                .with_context(|| format!("resolving source import path {relative}"))?;
            if !resolved.starts_with(&root) {
                anyhow::bail!("source import path escapes the project root");
            }
            if !resolved.is_dir() {
                anyhow::bail!("source import path must be a directory");
            }
            resolved
        }
        None => root.clone(),
    };
    let walker = WalkBuilder::new(&search_root)
        .hidden(false)
        .max_depth(Some(20))
        .filter_entry(|entry| {
            !entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(".git")
        })
        .build();
    let mut files = Vec::new();
    let mut skipped = 0usize;
    for (scanned, entry) in walker.enumerate() {
        if files.len() >= max_files || scanned >= MAX_SOURCE_IMPORT_SCAN {
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() || file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        let relative_path = match path.strip_prefix(&root) {
            Ok(relative) => relative.to_string_lossy().to_string(),
            Err(_) => continue,
        };
        if !is_source_extension(path) {
            skipped += 1;
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        if metadata.len() > crate::sources::MAX_SOURCE_BYTES {
            skipped += 1;
            continue;
        }
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        if bytes.contains(&0) {
            skipped += 1;
            continue;
        }
        let content = match String::from_utf8(bytes) {
            Ok(content) => content,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        if content.trim().is_empty() {
            skipped += 1;
            continue;
        }
        files.push(SourceFileInput {
            title: relative_path.clone(),
            mime_type: source_mime_type(&relative_path),
            relative: relative_path,
            content,
        });
    }
    Ok(ImportScan { files, skipped })
}

fn is_source_extension(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "md" | "markdown"
            | "mdx"
            | "txt"
            | "rst"
            | "org"
            | "adoc"
            | "json"
            | "jsonl"
            | "yaml"
            | "yml"
            | "toml"
            | "csv"
            | "tsv"
            | "rs"
            | "swift"
            | "py"
            | "js"
            | "jsx"
            | "ts"
            | "tsx"
            | "go"
            | "java"
            | "kt"
            | "kts"
            | "c"
            | "h"
            | "cc"
            | "cpp"
            | "hpp"
            | "m"
            | "mm"
            | "cs"
            | "rb"
            | "php"
            | "sh"
            | "bash"
            | "zsh"
            | "fish"
            | "html"
            | "htm"
            | "css"
            | "scss"
            | "less"
            | "sql"
            | "graphql"
            | "proto"
            | "xml"
            | "svg"
    )
}

fn source_mime_type(path: &str) -> String {
    match Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "md" | "markdown" | "mdx" => "text/markdown",
        "json" | "jsonl" => "application/json",
        "yaml" | "yml" => "application/yaml",
        "html" | "htm" => "text/html",
        "css" | "scss" | "less" => "text/css",
        "xml" | "svg" => "application/xml",
        "csv" | "tsv" => "text/csv",
        _ => "text/plain",
    }
    .to_string()
}

fn normalize_source_filters(
    values: Vec<String>,
    max: usize,
    max_chars: usize,
    label: &str,
) -> Result<Vec<String>> {
    if values.len() > max {
        anyhow::bail!("at most {max} source {label} filters are supported");
    }
    let mut output = Vec::new();
    for value in values {
        let value = value.trim();
        if value.chars().count() > max_chars || value.chars().any(char::is_control) {
            anyhow::bail!(
                "source {label} filters must be at most {max_chars} characters without control characters"
            );
        }
        if !output.iter().any(|existing| existing == value) {
            output.push(value.to_string());
        }
    }
    Ok(output)
}

fn normalize_source_collection(value: Option<&str>) -> Result<Option<String>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.chars().count() > 80 || value.chars().any(char::is_control) {
        anyhow::bail!("source collection must be at most 80 characters without control characters");
    }
    Ok(Some(value.to_string()))
}

fn source_embedding_retry_at(attempts: u32) -> String {
    let delay = crate::memory_embedding::index_retry_delay(attempts);
    (time::OffsetDateTime::now_utc()
        + time::Duration::seconds(i64::try_from(delay.as_secs()).unwrap_or(3_600)))
    .format(&time::format_description::well_known::Rfc3339)
    .unwrap_or_else(|_| now_rfc3339())
}

fn source_embedding_time_before(seconds: u64) -> String {
    (time::OffsetDateTime::now_utc()
        - time::Duration::seconds(i64::try_from(seconds).unwrap_or(900)))
    .format(&time::format_description::well_known::Rfc3339)
    .unwrap_or_else(|_| now_rfc3339())
}

fn source_title_from_url(url: &str) -> String {
    let parsed = reqwest::Url::parse(url).ok();
    let host = parsed
        .as_ref()
        .and_then(|url| url.host_str())
        .unwrap_or("web");
    let tail = parsed
        .as_ref()
        .and_then(|url| url.path_segments())
        .and_then(|mut segments| segments.next_back())
        .filter(|segment| !segment.is_empty())
        .unwrap_or("");
    let title = if tail.is_empty() {
        host.to_string()
    } else {
        format!("{host} / {tail}")
    };
    title.chars().take(200).collect()
}

fn skill_applies(skill: &Skill, role: &str, profile: &str) -> bool {
    (skill.roles.is_empty()
        || skill
            .roles
            .iter()
            .any(|value| value.eq_ignore_ascii_case(role)))
        && (skill.profiles.is_empty()
            || skill
                .profiles
                .iter()
                .any(|value| value.eq_ignore_ascii_case(profile)))
}

#[allow(clippy::too_many_arguments)]
fn build_skill(
    project_id: &str,
    name: &str,
    description: &str,
    when_to_use: &str,
    version: Option<&str>,
    instructions: &str,
    tool_allow: Vec<String>,
    tool_deny: Vec<String>,
    profiles: Vec<String>,
    roles: Vec<String>,
    source: &str,
    enabled: bool,
) -> Result<Skill> {
    let name = name.trim();
    let description = description.trim();
    let when_to_use = when_to_use.trim();
    let instructions = instructions.trim();
    let version = version.unwrap_or("1.0.0").trim();
    crate::skills::validate_skill_fields(name, description, when_to_use, instructions, version)?;
    let slug = crate::skills::slugify(name);
    if slug.is_empty() {
        anyhow::bail!("skill name must contain a letter or number");
    }
    let tool_allow = normalize_skill_list(tool_allow, 64, 80, "tool")?;
    let tool_deny = normalize_skill_list(tool_deny, 64, 80, "tool")?;
    if tool_allow
        .iter()
        .any(|tool| tool_deny.iter().any(|denied| denied == tool))
    {
        anyhow::bail!("a skill tool cannot be both allowed and denied");
    }
    let profiles = normalize_skill_list(profiles, 4, 32, "profile")?;
    for profile in &profiles {
        if crate::agent::AgentProfile::parse(profile).is_none() {
            anyhow::bail!("unknown skill profile: {profile}");
        }
    }
    let roles = normalize_skill_list(roles, 5, 32, "role")?;
    for role in &roles {
        if !matches!(
            role.to_ascii_lowercase().as_str(),
            "leader" | "manager" | "worker" | "verifier" | "labour"
        ) {
            anyhow::bail!("unknown skill role: {role}");
        }
    }
    let now = now_rfc3339();
    Ok(Skill {
        id: Uuid::now_v7().to_string(),
        project_id: project_id.to_string(),
        name: name.to_string(),
        slug,
        description: description.to_string(),
        when_to_use: when_to_use.to_string(),
        version: version.to_string(),
        instructions: instructions.to_string(),
        tool_allow,
        tool_deny,
        profiles,
        roles,
        enabled,
        source: source.to_string(),
        sha256: sha256_hex(
            format!("{name}\n{description}\n{when_to_use}\n{instructions}\n{version}").as_bytes(),
        ),
        created_at: now.clone(),
        updated_at: now,
    })
}

fn normalize_skill_list(
    values: Vec<String>,
    max: usize,
    max_chars: usize,
    label: &str,
) -> Result<Vec<String>> {
    if values.len() > max {
        anyhow::bail!("skill supports at most {max} {label} entries");
    }
    let mut output = Vec::new();
    for value in values {
        let value = value.trim();
        if value.is_empty() || value.chars().count() > max_chars {
            anyhow::bail!("skill {label} entries must be 1-{max_chars} characters");
        }
        if !output.iter().any(|existing| existing == value) {
            output.push(value.to_string());
        }
    }
    Ok(output)
}

fn read_project_text_file(root: &Path, relative: &str, max_bytes: u64) -> Result<(String, String)> {
    let root = root.canonicalize().context("resolving project root")?;
    let relative_path = Path::new(relative.trim());
    if relative.trim().is_empty()
        || relative_path.is_absolute()
        || relative_path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
        || relative_path.components().any(|component| {
            component
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(".git")
        })
    {
        anyhow::bail!("skill path must be relative, inside the project, and outside .git");
    }
    let path = root
        .join(relative_path)
        .canonicalize()
        .with_context(|| format!("resolving skill file {relative}"))?;
    if !path.starts_with(&root) {
        anyhow::bail!("skill path escapes the project root");
    }
    let metadata = std::fs::metadata(&path)?;
    if !metadata.is_file() {
        anyhow::bail!("skill path is not a file");
    }
    if metadata.len() > max_bytes {
        anyhow::bail!("skill file is larger than {max_bytes} bytes");
    }
    let content = std::fs::read_to_string(&path).context("skill file is not UTF-8")?;
    let relative = path
        .strip_prefix(&root)
        .unwrap_or(&path)
        .to_string_lossy()
        .to_string();
    Ok((content, relative))
}

fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let mut output = value
        .chars()
        .take(limit.saturating_sub(1))
        .collect::<String>();
    output.push('…');
    output
}

struct ParsedCitationMessage {
    message_id: String,
    agent_name: Option<String>,
    text: String,
    citations: Vec<SourceCitation>,
    created_at: String,
}

fn parse_message_citation_event(event: &Event) -> Option<ParsedCitationMessage> {
    if event.kind != "agent.message" {
        return None;
    }
    let payload = &event.payload;
    let message_id = payload
        .get("message_id")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("{}:{}", event.session_id, event.seq));
    let text = payload
        .get("text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let citations = payload
        .get("citations")
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| serde_json::from_value::<SourceCitation>(value.clone()).ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let agent_name = payload
        .get("agent_name")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            payload
                .get("agent_title")
                .and_then(serde_json::Value::as_str)
        })
        .map(ToOwned::to_owned);
    Some(ParsedCitationMessage {
        message_id,
        agent_name,
        text,
        citations,
        created_at: event.created_at.clone(),
    })
}

/// Extract `[cN]` markers without a regex. Markers are ASCII even when the
/// surrounding prose is not, so byte scanning is safe and deterministic.
fn citation_keys_in_text(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut keys = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] != b'[' || index + 3 >= bytes.len() {
            index += 1;
            continue;
        }
        let prefix = bytes[index + 1];
        if prefix != b'c' && prefix != b'C' {
            index += 1;
            continue;
        }
        let mut end = index + 2;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end > index + 2 && end < bytes.len() && bytes[end] == b']' {
            let key = format!("c{}", &text[index + 2..end]);
            if !keys.iter().any(|existing| existing == &key) {
                keys.push(key);
            }
            index = end + 1;
        } else {
            index += 1;
        }
    }
    keys
}

fn first_line(value: &str, limit: usize) -> String {
    let line = value
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let line = line.trim();
    if line.chars().count() <= limit {
        line.to_string()
    } else {
        let mut truncated = line
            .chars()
            .take(limit.saturating_sub(1))
            .collect::<String>();
        truncated.push('…');
        truncated
    }
}

fn build_intake_draft(params: &IntakeCreateParams) -> Result<NewIntakeTask> {
    const MAX_TITLE_CHARS: usize = 200;
    const MAX_INSTRUCTIONS_CHARS: usize = 20_000;
    const MAX_SCOPES: usize = 32;
    const MAX_SCOPE_CHARS: usize = 500;
    const MAX_ACCEPTANCE: usize = 12;
    const MAX_ACCEPTANCE_CHARS: usize = 500;
    const MAX_LABELS: usize = 12;
    const MAX_LABEL_CHARS: usize = 40;
    let title = params.title.trim();
    if title.is_empty() || title.chars().count() > MAX_TITLE_CHARS {
        anyhow::bail!("intake title must be 1-{MAX_TITLE_CHARS} characters");
    }
    let instructions = params.instructions.trim();
    if instructions.is_empty() || instructions.chars().count() > MAX_INSTRUCTIONS_CHARS {
        anyhow::bail!("intake instructions must be 1-{MAX_INSTRUCTIONS_CHARS} characters");
    }
    let profile = params
        .profile
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("coding");
    if crate::agent::AgentProfile::parse(profile).is_none() {
        anyhow::bail!("unknown intake profile: {profile}");
    }
    let kind = params
        .kind
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            match profile {
                "research" => "research",
                "verification" => "verification",
                "generic" => "analysis",
                _ => "coding",
            }
            .to_string()
        });
    if kind.chars().count() > 60 {
        anyhow::bail!("intake kind must be at most 60 characters");
    }
    let priority = params.priority.unwrap_or(0);
    if !(-100..=100).contains(&priority) {
        anyhow::bail!("intake priority must be between -100 and 100");
    }
    if params.write_scope.len() > MAX_SCOPES {
        anyhow::bail!("intake write scope supports at most {MAX_SCOPES} paths");
    }
    let mut write_scope = Vec::new();
    for scope in &params.write_scope {
        let raw = scope.trim();
        if raw.is_empty()
            || raw.starts_with('/')
            || raw.starts_with('~')
            || raw.contains("..")
            || Path::new(raw).is_absolute()
        {
            anyhow::bail!("invalid intake write scope: {scope}");
        }
        let scope = raw.trim_end_matches('/');
        if scope.is_empty() || scope.chars().count() > MAX_SCOPE_CHARS {
            anyhow::bail!("invalid intake write scope: {scope}");
        }
        if !write_scope.iter().any(|existing| existing == scope) {
            write_scope.push(scope.to_string());
        }
    }
    let needs_write = !write_scope.is_empty()
        || matches!(
            kind.to_ascii_lowercase().as_str(),
            "coding" | "code" | "implement" | "implementation" | "edit" | "fix"
        );
    if needs_write && matches!(profile, "generic" | "verification") {
        anyhow::bail!(
            "writable intake tasks need a writable profile; use coding or research, or remove the write scope"
        );
    }
    let mut acceptance = Vec::new();
    for item in &params.acceptance {
        let item = item.trim();
        if item.is_empty() || item.chars().count() > MAX_ACCEPTANCE_CHARS {
            anyhow::bail!("intake acceptance items must be 1-{MAX_ACCEPTANCE_CHARS} characters");
        }
        acceptance.push(item.to_string());
    }
    if acceptance.len() > MAX_ACCEPTANCE {
        anyhow::bail!("intake supports at most {MAX_ACCEPTANCE} acceptance items");
    }
    if acceptance.is_empty() {
        acceptance
            .push("The requested outcome is implemented, verified, and summarized.".to_string());
    }
    if params.token_budget == Some(0) {
        anyhow::bail!("intake token budget must be greater than zero");
    }
    if params
        .cost_budget_usd
        .is_some_and(|value| !value.is_finite() || value <= 0.0)
    {
        anyhow::bail!("intake cost budget must be a positive finite number");
    }
    if params.time_budget_secs == Some(0) {
        anyhow::bail!("intake time budget must be greater than zero");
    }
    let max_attempts = params.max_attempts.unwrap_or(2);
    if !(1..=10).contains(&max_attempts) {
        anyhow::bail!("intake max attempts must be between 1 and 10");
    }
    let mut labels = Vec::new();
    for label in &params.labels {
        let label = label.trim();
        if label.is_empty() || label.chars().count() > MAX_LABEL_CHARS {
            anyhow::bail!("intake labels must be 1-{MAX_LABEL_CHARS} characters");
        }
        if !labels.iter().any(|existing| existing == label) {
            labels.push(label.to_string());
        }
    }
    if labels.len() > MAX_LABELS {
        anyhow::bail!("intake supports at most {MAX_LABELS} labels");
    }
    let origin = params
        .origin
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("user");
    if origin.chars().count() > 32 {
        anyhow::bail!("intake origin must be at most 32 characters");
    }
    let mut deps = Vec::new();
    for dep in &params.deps {
        let dep = dep.trim();
        if dep.is_empty() || dep.chars().count() > 80 {
            anyhow::bail!("intake dependency ids must be 1-80 characters");
        }
        if !deps.iter().any(|existing| existing == dep) {
            deps.push(dep.to_string());
        }
    }
    if deps.len() > 20 {
        anyhow::bail!("intake supports at most 20 dependencies");
    }
    Ok(NewIntakeTask {
        project_id: params.project_id.clone(),
        title: title.to_string(),
        instructions: instructions.to_string(),
        profile: profile.to_string(),
        kind,
        priority,
        write_scope,
        acceptance,
        deps,
        token_budget: params.token_budget,
        cost_budget_usd: params.cost_budget_usd,
        time_budget_secs: params.time_budget_secs,
        max_attempts,
        origin: origin.to_string(),
        labels,
    })
}

pub(crate) fn is_active_session_status(status: &str) -> bool {
    matches!(status, "queued" | "running" | "paused" | "waiting_input")
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

fn tool_activity_entry(event: Event) -> ToolActivityEntry {
    let payload = &event.payload;
    let is_error = payload
        .get("is_error")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let structured = payload.get("structured");
    let error_code = payload
        .get("error_code")
        .and_then(serde_json::Value::as_str)
        .map(|code| {
            crate::tooling::errors::ToolErrorCode::from_code_or_failed(code)
                .as_str()
                .to_string()
        })
        .or_else(|| {
            if !is_error {
                return None;
            }
            structured
                .and_then(|value| value.get("code"))
                .and_then(serde_json::Value::as_str)
                .map(|code| {
                    crate::tooling::errors::ToolErrorCode::from_code_or_failed(code)
                        .as_str()
                        .to_string()
                })
                .or_else(|| Some("TOOL_FAILED".to_string()))
        });
    let hint = payload
        .get("error_hint")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            structured
                .and_then(|value| value.get("hint"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| {
            error_code.as_deref().map(|code| {
                crate::tooling::errors::ToolErrorCode::from_code_or_failed(code)
                    .default_hint()
                    .to_string()
            })
        })
        .map(|hint| hint.chars().take(600).collect());
    let retryable = payload
        .get("retryable")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let duration_ms = payload
        .get("duration_ms")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
        .min(86_400_000);
    let output_bytes = payload
        .get("output_bytes")
        .or_else(|| payload.get("total_bytes"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    ToolActivityEntry {
        tool_call_id: payload
            .get("tool_call_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .chars()
            .take(200)
            .collect(),
        name: payload
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
            .chars()
            .take(120)
            .collect(),
        seq: event.seq,
        is_error,
        error_code,
        retryable,
        hint,
        duration_ms,
        output_bytes,
        truncated: payload
            .get("truncated")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        approval_risk: payload
            .get("approval_risk")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        approval_mode: payload
            .get("approval_mode")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        approval_wait_ms: payload
            .get("approval_wait_ms")
            .and_then(serde_json::Value::as_u64)
            .map(|wait| wait.min(86_400_000)),
        created_at: event.created_at,
    }
}

fn tool_activity_summary(entries: &[ToolActivityEntry]) -> ToolActivitySummary {
    let calls = entries.len() as u64;
    let errors = entries.iter().filter(|entry| entry.is_error).count() as u64;
    let retryable_errors = entries
        .iter()
        .filter(|entry| entry.is_error && entry.retryable)
        .count() as u64;
    let total_duration_ms = entries
        .iter()
        .fold(0u64, |total, entry| total.saturating_add(entry.duration_ms));
    let max_duration_ms = entries
        .iter()
        .map(|entry| entry.duration_ms)
        .max()
        .unwrap_or(0);
    let output_bytes = entries.iter().fold(0u64, |total, entry| {
        total.saturating_add(entry.output_bytes)
    });
    let truncated_results = entries.iter().filter(|entry| entry.truncated).count() as u64;
    let average_duration_ms = total_duration_ms.checked_div(calls).unwrap_or(0);
    let mut by_tool: std::collections::BTreeMap<String, ToolActivityToolStat> =
        std::collections::BTreeMap::new();
    let mut by_code: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    let mut approval_waits = 0u64;
    let mut total_approval_wait_ms = 0u64;
    let mut max_approval_wait_ms = 0u64;
    for entry in entries {
        let stat = by_tool
            .entry(entry.name.clone())
            .or_insert_with(|| ToolActivityToolStat {
                name: entry.name.clone(),
                ..ToolActivityToolStat::default()
            });
        stat.calls = stat.calls.saturating_add(1);
        stat.errors = stat.errors.saturating_add(u64::from(entry.is_error));
        stat.total_duration_ms = stat.total_duration_ms.saturating_add(entry.duration_ms);
        stat.output_bytes = stat.output_bytes.saturating_add(entry.output_bytes);
        if let Some(code) = &entry.error_code {
            *by_code.entry(code.clone()).or_default() = by_code
                .get(code)
                .copied()
                .unwrap_or_default()
                .saturating_add(1);
        }
        if let Some(wait) = entry.approval_wait_ms {
            approval_waits = approval_waits.saturating_add(1);
            total_approval_wait_ms = total_approval_wait_ms.saturating_add(wait);
            max_approval_wait_ms = max_approval_wait_ms.max(wait);
        }
    }
    let mut by_tool = by_tool.into_values().collect::<Vec<_>>();
    by_tool.sort_by(|left, right| {
        right
            .calls
            .cmp(&left.calls)
            .then_with(|| left.name.cmp(&right.name))
    });
    by_tool.truncate(12);
    let mut error_codes = by_code
        .into_iter()
        .map(|(code, calls)| ToolActivityCodeStat { code, calls })
        .collect::<Vec<_>>();
    error_codes.sort_by(|left, right| {
        right
            .calls
            .cmp(&left.calls)
            .then_with(|| left.code.cmp(&right.code))
    });
    error_codes.truncate(8);
    ToolActivitySummary {
        calls,
        errors,
        retryable_errors,
        truncated_results,
        total_duration_ms,
        max_duration_ms,
        average_duration_ms,
        output_bytes,
        approval_waits,
        total_approval_wait_ms,
        max_approval_wait_ms,
        by_tool,
        error_codes,
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

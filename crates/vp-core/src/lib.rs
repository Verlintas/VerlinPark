//! Application services shared by the daemon and future harness components.
//!
//! M1 adds the Leader agent loop, tool registry, approvals, and event stream.

#![recursion_limit = "512"]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::tools::{ToolContext, ToolOutput};
use anyhow::{Context, Result, anyhow, bail};
use async_recursion::async_recursion;
use ignore::WalkBuilder;
use tokio::sync::{broadcast, oneshot};
use uuid::Uuid;
use vp_protocol::lsp::{LspLanguageStatus, LspStatusResult};
use vp_protocol::now_rfc3339;
use vp_protocol::progress::{MAX_PROGRESS_ROUNDS, ProgressLedgerResult, ProgressRound};
use vp_protocol::remote::RemoteTestEvidence;
use vp_protocol::types::{
    AcpAgent, AcpAgentConfig, AcpAgentListResult, AcpProbeResult, AcpTransport,
    AgentCoordinationReplayParams, AgentCoordinationReplayReport, AgentCoordinationWake,
    AgentIdentitiesResult, AgentIdentity, AgentIdentityPolicy, AgentIdentityReplayParams,
    AgentIdentityReplayReport, AgentIdentityResult, AgentInboxListParams, AgentInboxListResult,
    AgentInboxMessageParams, AgentInboxReadResult, AgentInboxSendParams, AgentInboxSendResult,
    AgentMemoryForgetResult, AgentMemoryListParams, AgentMemoryListResult, AgentMemoryParams,
    AgentMemoryReplayParams, AgentMemoryReplayReport, AgentMemoryResult,
    AgentMemorySupersedeResult, AgentMemorySweepResult, AgentMessage, AgentRecord, Approval,
    ApprovalAuditEvent, ApprovalAuditResult, ApprovalBatchItemResult, ApprovalBatchResolveResult,
    ApprovalGrant, ApprovalQueueResult, ApprovalTemplate, ApprovalTemplateCreateParams,
    ApprovalTemplateDeleteParams, ApprovalTemplateDeleteResult, ApprovalTemplateFromApprovalParams,
    ApprovalTemplateResult, ApprovalTemplateUpdateParams, Artifact, ArtifactBackfillResult,
    ArtifactDiffParams, ArtifactDiffResult, ArtifactGcResult, ArtifactMaterialization,
    ArtifactPolicy, ArtifactVersionsResult, AttentionPolicy, AttentionPolicyResult,
    AttentionPolicySetParams, AutoBudgetExtension, AutoBudgetExtensionOutcome,
    AutoBudgetExtensionResult, AutoBudgetPolicy, AutoBudgetPolicyResult, BudgetGovernorDecision,
    BudgetMode, BudgetValidationError, CachePolicy, CheckpointPolicyResult,
    CheckpointRetentionPolicy, CitationAudit, CitationAuditMessage, ConsoleCloseResult,
    ConsoleOpenResult, ConsoleReadResult, ConsoleSignalResult, ConsoleWriteResult,
    ContextCompactionSummary, ContextMeasurement, ContextUsage, ContinuationOutcome,
    ContractPolicy, CreateAgentIdentityParams, CreateAgentMemoryParams, CreateModelPresetParams,
    CriticalPathPolicy, DoctorCheck, EffectPolicy, EffectReceipt, EffectReconcileParams,
    EffectReconcileResult, EffectState, Event, ForgeOriginStatus, ForgePreflightResult,
    ForgeProviderStatus, ForgeStatusResult, ForgetAgentMemoryParams, GateRecord, GitLogResult,
    GitShowResult, GovernorSessionStatus, IntakeCreateParams, IntakeListResult, IntakeTask,
    IntakeUpdateParams, IntegrationPolicy, IntentDecisionRecord, MailboxGcResult, MailboxPolicy,
    McpCallResult, McpGetPromptResult, McpPinActionResult, McpPinApproveParams, McpPinRejectParams,
    McpProbeResult, McpPromptListResult, McpReadResourceResult, McpResourceListResult, McpServer,
    McpServerConfig, McpServerListResult, McpToolListResult, McpToolPin, MemoryBriefResult,
    MemoryConflict, MemoryConflictResolveResult, MemoryConflictScanResult, MemoryConflictsResult,
    MemoryDeleteResult, MemoryEmbeddingPolicy, MemoryEmbeddingsIndexRebuildResult,
    MemoryEmbeddingsIndexStatusResult, MemoryEmbeddingsRebuildResult, MemoryEmbeddingsStatusResult,
    MemoryEmbeddingsTestResult, MemoryEvidence, MemoryHealthResult, MemoryItem, MemoryListResult,
    MemoryMatch, MemoryPolicy, MemoryPolicyResult, MemoryPruneResult, MemorySearchParams,
    MemorySearchResult, MemorySemanticSummary, MemoryStatsResult, MemorySweepResult,
    MemoryUpdateResult, MemoryVerificationCheck, MemoryVerifyResult, MergeQueueEntry,
    MessageCitation, MetricCount, ModelPreset, ModelPresetListResult, OrchestrationDecision,
    OrchestrationNote, OrchestrationOverviewResult, OrchestrationOverviewTotals,
    OrchestrationPolicy, OrchestrationPreviewAction, OrchestrationPreviewResult,
    OrchestrationRecoveriesResult, OrchestrationRecovery, OrchestrationSessionSummary,
    OrchestrationSnapshot, PauseAgentIdentityParams, PlanRevisionDetailResult, PlanRevisionInfo,
    PreemptionOverviewResult, PreemptionPolicy, PreemptionRecord, ProcessListResult,
    ProcessLogsResult, ProcessPolicy, ProcessRestartResult, ProcessStopResult,
    ProcessSupervisorStatus, Program, ProgramActionParams, ProgramCreateParams,
    ProgramDeleteParams, ProgramListResult, ProgramMilestone, ProgramMilestoneActionParams,
    ProgramMilestoneCreateParams, ProgramMilestoneListResult, ProgramMilestoneUpdateParams,
    ProgramPolicy, ProgramReplayReport, ProgramStatus, ProgramStatusParams, ProgramStatusResult,
    ProgramTickParams, ProgramTickResult, ProgramUpdateParams, Project, ProjectAutoBudgetEvent,
    ProjectBudget, ProjectBudgetResult, ProjectCacheEpochPruneResult, ProjectCacheEpochResult,
    ProjectMcpPinsResult, ProjectRunPolicyResult, ProjectUsageResult, PromptPrefixAggregate,
    PromptPrefixObservation, PullRequestResult, Question, QuestionListResult,
    ResumeAgentIdentityParams, RetireAgentIdentityParams, ReviseTeamPlanResult, ScheduledTask,
    SearchResult, Session, SessionAssignParams, SessionAssignResult, SessionAssignmentRenewParams,
    SessionAssignmentRenewResult, SessionAssignmentsParams, SessionAssignmentsResult,
    SessionAutoBudgetRecord, SessionAutoBudgetState, SessionBudgetForecastResult,
    SessionBudgetStatus, SessionBundle, SessionCacheStatus, SessionCheckpoint,
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
    SessionPreemptionResult, SessionPriorityResult, SessionRehydrateResult, SessionReleaseParams,
    SessionReleaseResult, SessionReplayReport, SessionSkillActivationsResult, SessionStallsResult,
    SessionSteerResult, SessionSteeringCancelResult, SessionSteeringListResult,
    SessionTranscriptResult, SessionUnlimitedResult, SetAutoBudgetPolicyParams,
    SetProjectRunPolicyParams, SetSessionBudgetParams, SetSessionBudgetResult, ShellPolicy, Skill,
    SkillActivation, SkillCreateParams, SkillDeleteResult, SkillEnabledParams, SkillImportParams,
    SkillListResult, SkillUpdateParams, Source, SourceChunk, SourceCitation,
    SourceCollectionsResult, SourceCreateFileParams, SourceCreateNoteParams, SourceCreateUrlParams,
    SourceDeleteResult, SourceEmbeddingPolicy, SourceEmbeddingPolicyResult,
    SourceEmbeddingReindexResult, SourceEmbeddingStatusResult, SourceGetResult, SourceImportError,
    SourceImportParams, SourceImportResult, SourceIndexRebuildResult, SourceIndexStatusResult,
    SourceListResult, SourceReindexResult, SourceRetrievalPolicy, SourceRetrievalPolicyResult,
    SourceSearchMode, SourceSearchResult, SourceStats, SteeringMessage, StoredMessage,
    SupersedeAgentMemoryParams, SweepAgentMemoriesParams, TaskQueueResult, TaskSchedulerPolicy,
    TaskSchedulerPolicyResult, TaskStall, TeamPlan, TerminalCloseResult, TerminalOpenResult,
    TerminalReadResult, TerminalResizeResult, TerminalWriteResult, TodoItem, ToolActivityCodeStat,
    ToolActivityEntry, ToolActivityResult, ToolActivitySummary, ToolActivityToolStat,
    ToolOutputAuditParams, ToolOutputAuditResult, ToolOutputListParams, ToolOutputListResult,
    ToolOutputPolicy, ToolOutputPolicyResult, ToolOutputPruneParams, ToolOutputPruneResult,
    ToolOutputStatus, ToolOutputStatusParams, UpdateAgentIdentityParams, UpdateAgentMemoryParams,
    UpdateModelPresetParams, UpdateSourceMetadataParams, UsageBucket, UsageCsvResult, UsageEntry,
    UsageLedgerResult, UsageTotals, VerifyAgentMemoryParams, WorkspaceFileResult,
    WorkspaceGrepResult, WorkspaceListResult, WorkspaceSearchResult, WorkspaceTreeResult,
    WorkspaceWriteResult, WorktreeCleanupEntry, WorktreeCleanupResult, WorktreePolicyResult,
    WorktreeRetentionPolicy, WriteLease, auto_budget_codes,
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

pub mod a2a;
pub mod acp;
pub mod agent;
pub mod agent_workspace;
pub mod approvals;
pub mod archive_media;
pub mod auto_budget;
pub mod auto_progress;
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
pub mod coordination;
pub mod critical_path;
pub mod custos_notice;
pub mod daemon_agent;
mod dashboard;
pub mod diagnosis;
pub mod effect;
pub mod employment;
pub mod fanout;
pub mod forge;
pub mod gate;
pub mod gui;
pub mod history;
pub mod identity;
pub mod identity_memory;
pub mod identity_prompt;
pub mod instruction;
pub mod mcp;
pub mod mcp_pins;
pub mod memory;
pub mod memory_embedding;
pub mod memory_index;
pub mod multi_path;
mod operation;
pub mod orchestration;
pub mod org;
pub mod otel;
pub mod permissions;
pub mod preemption;
pub mod process_supervisor;
pub mod program;
pub mod progress_ledger;
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
pub mod script;
mod session_graph;
pub mod skills;
pub mod source_ann;
pub mod source_embedding;
pub mod source_retrieval;
pub mod sources;
pub mod team_template;
pub mod terminal;
pub mod test_cache;
pub mod textdiff;
pub mod tool_bundle;
pub mod tool_output;
pub mod tooling;
pub mod update;
pub mod workspace;
pub mod workspace_engine;

pub use rebuild::rebuild_database;
pub mod tools;
pub mod worktree;

mod engine;

// Lease guards moved into `engine::leases`; re-exported here so
// `crate::SessionLease` / `crate::RuntimeLeaseGuard` / `crate::ComputerLeaseGuard`
// and the public `vp_core::...` paths keep resolving.
pub use engine::leases::{ComputerLeaseGuard, RuntimeLeaseGuard, SessionLease};

// Explicit partial-delivery failures carry a stable code for the daemon's RPC
// mapper. The record itself is event-only; this re-export exposes only the
// typed error and its codes.
pub use engine::delivery::{PartialDeliveryError, partial_delivery_codes};

// Typed rescue failures carry a stable code for the daemon's RPC mapper. The
// grant/receipt rows are event-only; the type itself lives in the protocol.
pub use vp_protocol::types::{RescueError, rescue_codes};

// Shared free helpers moved into `engine::support`; re-exported so existing
// `crate::helper` paths used by sibling modules and tests keep resolving.
pub(crate) use engine::support::*;
pub use engine::support::{parse_task_stall_threshold, task_stall_threshold_secs};

/// Result of one run-loop lease heartbeat. Warnings are observations, not
/// terminal errors: the run may continue on the legacy path, but the caller
/// records the loss so a silent mode switch is impossible.
#[derive(Debug, Clone, Default)]
pub struct RunLeaseHeartbeat {
    pub assignment_renewed: bool,
    pub workspaces_renewed: usize,
    pub warnings: Vec<String>,
}

/// Short-lived readiness snapshot keyed by the newest session event sequence.
/// Readiness runs `git status` plus queue/effect queries; explicit operator
/// views can reuse a snapshot for a couple of seconds, and any newly appended
/// event invalidates it by sequence.
#[derive(Clone)]
pub(crate) struct CachedReadiness {
    pub(crate) seq: u64,
    pub(crate) cached_at: std::time::Instant,
    pub(crate) snapshot: vp_protocol::types::ReadinessSnapshot,
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
    a2a: Arc<a2a::A2aRuntime>,
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
    /// One async mutex per project workspace. Same-component Worker runs in one
    /// session serialize; different components remain parallel.
    workspace_runs: Arc<WorkspaceRunLocks>,
    /// Bounded, in-process readiness cache for explicit resource/readiness
    /// views. It is never used by the lean turn governor.
    readiness_cache: Arc<Mutex<HashMap<String, CachedReadiness>>>,
    /// Serializes rescue grant/consume check-and-append sequences across
    /// manual and automatic paths. Eligibility reads run before the automatic
    /// lock because readiness can be slow; without this lock concurrent turn
    /// boundaries could observe zero prior grants and append two automatic
    /// grants, or consume the same grant twice.
    rescue_lock: Arc<tokio::sync::Mutex<()>>,
}

/// In-process serialization for workspace-bound Worker runs. Cross-session
/// exclusion is the SQLite workspace lease; this only prevents two parallel
/// children of one session from mutating one worktree at the same time.
#[derive(Debug, Default)]
pub struct WorkspaceRunLocks {
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl WorkspaceRunLocks {
    pub fn lock_for(&self, workspace_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = match self.locks.lock() {
            Ok(locks) => locks,
            Err(poisoned) => poisoned.into_inner(),
        };
        locks
            .entry(workspace_id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }
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
pub(crate) fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(data))
}
/// Backward-compatible alias for the CLI provider kinds used by existing tests
/// and callers. `ForgeProviderKind` is the single source of provider metadata.
pub(crate) use crate::forge::ForgeProviderKind as PrCli;
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
            a2a: Arc::new(a2a::A2aRuntime::default()),
            remote: Arc::new(remote::RemoteManager::new(data_dir.clone())),
            consoles: Arc::new(console::ConsoleManager::new()),
            terminals: Arc::new(terminal::TerminalManager::new()),
            browsers: Arc::new(browser::BrowserManager::new(data_dir.clone())),
            computers: Arc::new(computer::ComputerManager::new(data_dir)),
            gui: Arc::new(gui::GuiCoordinator::new()),
            runtime_id,
            continuation_claims: Arc::new(Mutex::new(HashMap::new())),
            pin_sync: Arc::new(tokio::sync::Mutex::new(())),
            workspace_runs: Arc::new(WorkspaceRunLocks::default()),
            readiness_cache: Arc::new(Mutex::new(HashMap::new())),
            rescue_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Lock one workspace for an in-process Worker run.
    pub fn workspace_run_lock(&self, workspace_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.workspace_runs.lock_for(workspace_id)
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
}

impl Engine {
    pub async fn append_event(
        &self,
        session_id: &str,
        kind: &str,
        mut payload: serde_json::Value,
    ) -> Result<Event> {
        apply_event_persistence_redaction(kind, &mut payload);
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

/// Sanitize only event kinds that persist tool input/output or approval input.
/// Agent messages, file content stored by dedicated tools, and journal/effect
/// inputs are deliberately outside this filter.
fn apply_event_persistence_redaction(kind: &str, payload: &mut serde_json::Value) {
    if matches!(
        kind,
        "tool.started" | "tool.finished" | "approval.created" | "approval.requested"
    ) {
        crate::tooling::redaction::redact_persisted_payload(payload);
    }
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

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

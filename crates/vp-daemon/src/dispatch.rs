use serde_json::json;

use vp_protocol::diagnosis::{
    SessionAttributionsParams, SessionDiagnosisParams, SessionDiagnosisRecordParams,
};
use vp_protocol::lsp::LspStatusParams;
use vp_protocol::otel::{
    OtelExportParams, OtelPolicyParams, OtelStatusParams, OtelTestParams, SetOtelPolicyParams,
};
use vp_protocol::remote::{
    HostListParams, HostListResult, HostProbeParams, HostProjectParams, HostSetParams,
    RemoteBootstrapParams, RemoteBootstrapStatusParams, RemoteIntegrationCleanupParams,
    RemoteIntegrationParams, RemoteIntegrationPolicyParams, RemoteIntegrationStatusParams,
    RemoteLedgerParams, RemoteStatusParams, RemoteTestOutputParams, RemoteTestParams,
    RemoteTestPolicyParams, RemoteTestStatusParams, RemoteWorktreeListParams,
    SetRemoteIntegrationPolicyParams, SetRemoteTestPolicyParams,
};
use vp_protocol::replay::ControllerReplayParams;
use vp_protocol::rpc::{RpcError, RpcRequest, RpcResponse, codes};
use vp_protocol::runtime::{PlanMigrateParams, SessionCompatibilityParams};
use vp_protocol::scaling::{
    ScalingPolicyParams, ScalingPreviewParams, ScalingStatusParams, SetScalingPolicyParams,
};
use vp_protocol::test_cache::{
    SetTestCachePolicyParams, TestCacheClearParams, TestCacheEntriesParams, TestCacheImpactParams,
    TestCachePolicyParams, TestCachePruneParams, TestCacheScopePreviewParams, TestCacheStatsParams,
};
use vp_protocol::types::{
    AcpAgentConfig, AcpAgentIdParams, AcpProbeParams, AddAcpAgentParams, AddMcpServerParams,
    AddProjectParams, AgentCoordinationReplayParams, AgentEmploymentReplayParams,
    AgentHireApproveParams, AgentHireListParams, AgentHireParams, AgentHireRejectParams,
    AgentIdentityDetailParams, AgentIdentityListParams, AgentIdentityParams,
    AgentIdentityPolicyParams, AgentIdentityPolicyResult, AgentIdentityReplayParams,
    AgentInboxListParams, AgentInboxMessageParams, AgentInboxSendParams, AgentInspectParams,
    AgentListParams, AgentListResult, AgentMemoryListParams, AgentMemoryParams,
    AgentMemoryReplayParams, AgentMessageResult, AgentMessageSendParams, AgentOrgSnapshotParams,
    AgentPerformanceParams, AgentPerformanceReplayParams, AgentPerformanceSummaryParams,
    AgentPromoteParams, AgentRetireParams, AgentRetirementCandidatesParams,
    AgentRetirementPolicyParams, AgentRetirementProposalListParams, AgentRetirementProposalParams,
    AgentRetirementProposeParams, AgentRetirementRejectParams, AgentTransferParams,
    AgentWorkspaceArchiveParams, AgentWorkspaceEnsureParams, AgentWorkspaceLandParams,
    AgentWorkspaceListParams, AgentWorkspaceParams, AgentWorkspaceReplayParams,
    AgentWorkspaceRetireParams, AnswerQuestionParams, AnswerQuestionResult, ApprovalAuditParams,
    ApprovalBatchResolveParams, ApprovalBatchResolveResult, ApprovalGrantCreateParams,
    ApprovalGrantCreateResult, ApprovalGrantDeleteParams, ApprovalGrantDeleteResult,
    ApprovalGrantListParams, ApprovalGrantListResult, ApprovalListResult, ApprovalQueueParams,
    ApprovalTemplateCreateParams, ApprovalTemplateDeleteParams, ApprovalTemplateFromApprovalParams,
    ApprovalTemplateListParams, ApprovalTemplateListResult, ApprovalTemplateUpdateParams,
    ArtifactArchiveExtractParams, ArtifactArchiveListParams, ArtifactBackfillParams,
    ArtifactDiffParams, ArtifactGcParams, ArtifactGetParams, ArtifactListParams,
    ArtifactListResult, ArtifactMaterializeParams, ArtifactPolicyParams, ArtifactPolicyResult,
    ArtifactRestoreParams, ArtifactResult, ArtifactVersionsParams, AttentionPolicyGetParams,
    AttentionPolicySetParams, AutoBudgetPolicyParams, CachePolicyParams, CachePolicyResult,
    CancelSessionParams, CancelSessionResult, CheckpointPolicyParams, CleanupWorktreesParams,
    CommitSessionParams, ConfigureAgentParams, ConsoleCloseParams, ConsoleOpenParams,
    ConsoleReadParams, ConsoleSignalParams, ConsoleWriteParams, ContextCheckpointCreateParams,
    ContextCheckpointDeleteParams, ContextCheckpointDetailParams, ContextCheckpointListParams,
    ContextCheckpointPolicyParams, ContextCheckpointPruneParams, ContextRestoreParams,
    ContractPolicyParams, ContractPolicyResult, CreateAgentIdentityParams, CreateAgentMemoryParams,
    CreateModelPresetParams, CreateSessionParams, CriticalPathMode, CriticalPathPolicyParams,
    CriticalPathPolicyResult, DaemonMetricsResult, DashboardParams, DeleteModelPresetParams,
    DiscardSessionParams, DoctorCheck, DoctorReport, EffectPolicyParams, EffectPolicyResult,
    EffectReconcileParams, EffectReconcileResult, EventListResult, ForgePreflightParams,
    ForgeStatusParams, ForgetAgentMemoryParams, ForkAuditParams, ForkRepairParams,
    GateCancelParams, GateListParams, GateOpenParams, GatePolicyParams, GatePolicyResult,
    GateResolveParams, GateStatusParams, GateSweepParams, GitLogParams, GitShowParams,
    GovernorPreviewParams, GovernorStatusParams, IntakeCreateParams, IntakeDeleteResult,
    IntakeListParams, IntakeTaskParams, IntakeUpdateParams, IntegrationPolicyParams,
    IntegrationPolicyResult, LandSessionParams, ListApprovalsParams, ListEventsParams,
    ListSessionsParams, MailboxGcParams, MailboxListParams, MailboxListResult, MailboxPolicyParams,
    MailboxPolicyResult, MarkMessageReadParams, McpCallParams, McpGetPromptParams,
    McpPinApproveParams, McpPinRejectParams, McpProbeParams, McpReadResourceParams,
    McpServerIdParams, MediaInfoParams, MediaTranscodeParams, MemoryBriefParams,
    MemoryConflictResolveParams, MemoryConflictScanParams, MemoryConflictsParams,
    MemoryCreateParams, MemoryCreateResult, MemoryDeleteParams, MemoryEmbeddingsIndexRebuildParams,
    MemoryEmbeddingsIndexStatusParams, MemoryEmbeddingsRebuildParams, MemoryEmbeddingsStatusParams,
    MemoryEmbeddingsTestParams, MemoryHealthParams, MemoryListParams, MemoryPolicyParams,
    MemoryPruneParams, MemorySearchParams, MemoryStatsParams, MemorySweepParams,
    MemoryUpdateParams, MemoryVerifyParams, MergeQueueListParams, MergeQueueListResult,
    OpenPullRequestParams, PauseAgentIdentityParams, PingResult, PreemptionOverviewParams,
    PreemptionPolicyParams, PreemptionPolicyResult, ProcessListParams, ProcessLogsParams,
    ProcessPolicyParams, ProcessPolicyResult, ProcessRestartParams, ProcessStopParams,
    ProjectBudgetParams, ProjectCacheEpochParams, ProjectCacheEpochPruneParams, ProjectCacheParams,
    ProjectForkForestParams, ProjectListResult, ProjectMcpPinsParams, ProjectRunPolicyParams,
    ProjectUsageParams, PromptParams, PromptResult, QuestionListParams, RehydrateSessionParams,
    ReplaySessionParams, ResolveApprovalParams, ResolveApprovalResult, ResolveMergeParams,
    ResolveMergeResult, ResumeAgentIdentityParams, RetireAgentIdentityParams, RetryMergeParams,
    RetryMergeResult, SchedulerStatusParams, SearchParams, SessionAssignParams,
    SessionAssignmentRenewParams, SessionAssignmentsParams, SessionBudgetForecastParams,
    SessionBudgetParams, SessionCacheAuditParams, SessionCacheBreaksParams,
    SessionCacheCallsParams, SessionCacheEpochParams, SessionCacheParams, SessionCachePlanParams,
    SessionCacheWarmParams, SessionCheckpointDeleteParams, SessionCheckpointDiffParams,
    SessionCheckpointListParams, SessionCheckpointParams, SessionCheckpointPruneParams,
    SessionCheckpointRestoreParams, SessionCheckpointStatsParams, SessionCitationAuditParams,
    SessionCitationsParams, SessionCompactParams, SessionCompareParams, SessionContextParams,
    SessionContinuationCancelParams, SessionContinuationReclaimParams,
    SessionContinuationStatusParams, SessionContinuationsParams, SessionContractParams,
    SessionDeferParams, SessionDiffParams, SessionEffectsParams, SessionExportParams,
    SessionForkParams, SessionForkTreeParams, SessionImportParams, SessionInstructionsParams,
    SessionLineageParams, SessionListResult, SessionLogParams, SessionMergeForkParams,
    SessionPauseParams, SessionPauseResult, SessionPlanResumeParams, SessionPlanResumeResult,
    SessionPreemptParams, SessionPreemptionParams, SessionReleaseParams, SessionResumeParams,
    SessionResumeResult, SessionRunCancelParams, SessionRunDetailParams, SessionRunPolicyParams,
    SessionRunResumeParams, SessionRunsParams, SessionSkillActivationsParams, SessionStallsParams,
    SessionSteerParams, SessionSteeringCancelParams, SessionSteeringParams, SessionTodosParams,
    SessionTodosResult, SessionTranscriptParams, SessionWorkspaceParams,
    SetAgentIdentityPolicyParams, SetArtifactPolicyParams, SetAutoBudgetPolicyParams,
    SetCachePolicyParams, SetCheckpointPolicyParams, SetContextCheckpointPolicyParams,
    SetContractPolicyParams, SetCriticalPathPolicyParams, SetGatePolicyParams,
    SetIntegrationPolicyParams, SetMailboxPolicyParams, SetMemoryPolicyParams,
    SetPreemptionPolicyParams, SetProcessPolicyParams, SetProjectBudgetParams,
    SetProjectPromptLayoutParams, SetProjectRunPolicyParams, SetSessionBudgetParams,
    SetSessionPriorityParams, SetSessionPromptLayoutParams, SetSessionRunPolicyParams,
    SetShellPolicyParams, SetSourceCollectionParams, SetSourceEmbeddingPolicyParams,
    SetSourceLabelsParams, SetSourceRetrievalPolicyParams, SetTaskSchedulerPolicyParams,
    SetToolOutputPolicyParams, SetWorktreePolicyParams, ShellPolicyParams, ShellPolicyResult,
    SkillCreateParams, SkillEnabledParams, SkillIdParams, SkillImportParams, SkillListParams,
    SkillUpdateParams, SourceActionParams, SourceCollectionsParams, SourceCreateFileParams,
    SourceCreateNoteParams, SourceCreateUrlParams, SourceEmbeddingPolicyParams,
    SourceEmbeddingReindexParams, SourceEmbeddingStatusParams, SourceGetParams, SourceImportParams,
    SourceIndexRebuildParams, SourceIndexStatusParams, SourceListParams,
    SourceRetrievalPolicyParams, SourceSearchParams, SupersedeAgentMemoryParams,
    SweepAgentMemoriesParams, TaskActionParams, TaskActionResult, TaskDispatchParams,
    TaskDispatchResult, TaskQueueParams, TaskSchedulerPolicyParams, TeamPlanParams, TeamPlanResult,
    TeamTemplateDiffParams, TeamTemplateExportParams, TeamTemplateGetParams,
    TeamTemplateImportParams, TeamTemplateListParams, TeamTemplateReplayParams,
    TerminalCloseParams, TerminalOpenParams, TerminalReadParams, TerminalResizeParams,
    TerminalWriteParams, ToolActivityParams, ToolBundleDiffParams, ToolBundleHistoryParams,
    ToolBundleMatrixParams, ToolBundleParams, ToolOutputAuditParams, ToolOutputListParams,
    ToolOutputPolicyParams, ToolOutputPolicyResult, ToolOutputPruneParams, ToolOutputStatusParams,
    UpdateAcpAgentParams, UpdateAgentIdentityParams, UpdateAgentMemoryParams, UpdateCheckParams,
    UpdateDownloadParams, UpdateMcpServerParams, UpdateModelPresetParams,
    UpdateSourceMetadataParams, UsageLedgerParams, VerifyAgentMemoryParams, WorkspaceFileParams,
    WorkspaceGrepParams, WorkspaceListParams, WorkspaceSearchParams, WorkspaceTreeParams,
    WorkspaceWriteParams, WorktreePolicyParams,
};
use vp_protocol::types::{
    ComputerLeaseInfo, ComputerLeaseListResult, ComputerLeaseReleaseParams,
    ComputerLeaseReleaseResult, ComputerLeaseSweepResult, ComputerStatusParams,
    DaemonAgentStatusParams, DaemonAgentStatusResult, DaemonInstanceInfo, GuiPolicyParams,
    GuiPolicyResult, GuiTimelineAuditParams, GuiTimelineParams, GuiTimelinePruneParams,
    GuiTimelineResult, GuiTimelineRetentionParams, RuntimeLeaseAuditInfo,
    RuntimeLeaseHistoryParams, RuntimeLeaseHistoryResult, RuntimeLeaseInfo, RuntimeLeaseListResult,
    RuntimeLeaseSweepResult, SetGuiPolicyParams,
};
use vp_protocol::types::{
    CoordinationNoteParams, CoordinationNoteResult, OrchestrationAdvanceParams,
    OrchestrationAdvanceResult, OrchestrationOverviewParams, OrchestrationPolicyParams,
    OrchestrationPolicyResult, OrchestrationPreviewParams, OrchestrationRecoveriesParams,
    OrchestrationSnapshotParams, OrchestrationSnapshotResult, PlanRevisionGetParams,
    PlanRevisionListParams, PlanRevisionListResult, PlanRevisionRestoreParams,
    ReviseTeamPlanParams, SetOrchestrationPolicyParams, TaskEditParams,
};
use vp_protocol::types::{
    ProgramActionParams, ProgramCreateParams, ProgramDeleteParams, ProgramDeleteResult,
    ProgramListParams, ProgramMilestoneActionParams, ProgramMilestoneCreateParams,
    ProgramMilestoneParams, ProgramMilestoneResult, ProgramMilestoneUpdateParams, ProgramParams,
    ProgramReplayParams, ProgramResult, ProgramStatusParams, ProgramTickParams,
    ProgramUpdateParams,
};
use vp_protocol::{APP_VERSION, PROTOCOL_VERSION};

use crate::AppState;
use crate::backup_status::backup_status;
use crate::errors::{
    agent_operator_error, approval_error, archive_media_error, budget_error, coordination_error,
    diagnosis_error, effect_error, fanout_error, forge_error, gate_error, identity_error,
    internal_error, memory_error, operation_error, otel_error, remote_bootstrap_error,
    remote_integration_error, remote_test_error, retrieval_policy_error, runtime_pin_error,
    session_start_error, source_index_error, validation_or_internal, workspace_error,
};
use crate::http::collect_daemon_metrics;
use crate::metrics;
use crate::params::{parse_params, response_with_id};
use crate::server::server_info;

mod agent;
mod artifact_mailbox;
mod backup_forge;
mod daemon;
mod gui_process;
mod insight;
mod mcp_acp;
mod memory_source;
mod policy;
mod project;
mod session;
mod team;

#[derive(Clone, Copy)]
enum Family {
    Agent,
    ArtifactMailbox,
    BackupForge,
    Daemon,
    GuiProcess,
    Insight,
    McpAcp,
    MemorySource,
    Policy,
    Project,
    Session,
    Team,
}

fn family(method: &str) -> Option<Family> {
    match method {
        "auth" | "ping" => return Some(Family::Daemon),
        "task.stalls" => return Some(Family::Session),
        "session.budget.governor"
        | "session.contract"
        | "session.resources"
        | "session.resources.top_up"
        | "session.readiness"
        | "session.delivery"
        | "session.delivery.partial"
        | "session.allocation"
        | "session.tradeoffs"
        | "session.rescue.status"
        | "session.rescue"
        | "session.run_contract.get"
        | "session.run_contract.set" => return Some(Family::Insight),
        _ => {}
    }
    let prefix = method.split_once('.').map(|(prefix, _)| prefix)?;
    match prefix {
        "session" => Some(Family::Session),
        "project" | "host" | "remote" | "lsp" => Some(Family::Project),
        "agent" | "workspace" => Some(Family::Agent),
        "team" | "team_plan" | "plan" | "task" | "intake" | "orchestration" | "scaling"
        | "critical_path" | "governor" | "scheduler" | "coordination" | "program" | "a2a" => {
            Some(Family::Team)
        }
        "mcp" | "acp" => Some(Family::McpAcp),
        "memory" | "source" | "skill" => Some(Family::MemorySource),
        "artifact" | "mailbox" | "media" | "tool_output" => Some(Family::ArtifactMailbox),
        "gui" | "computer" | "console" | "terminal" | "process" => Some(Family::GuiProcess),
        "backup" | "forge" | "git" | "merge_queue" | "update" => Some(Family::BackupForge),
        "approval" | "gate" | "effect" | "runtime" | "preemption" | "tool_bundle"
        | "test_cache" | "telemetry" | "model" | "question" => Some(Family::Policy),
        "server" | "daemon" | "dashboard" | "event" => Some(Family::Daemon),
        _ => None,
    }
}

pub(crate) async fn dispatch(state: &AppState, request: RpcRequest) -> RpcResponse {
    let id = request.id.clone();
    let method = request.method.clone();
    let response = match family(request.method.as_str()) {
        Some(Family::Agent) => agent::handle(state, id, request).await,
        Some(Family::ArtifactMailbox) => artifact_mailbox::handle(state, id, request).await,
        Some(Family::BackupForge) => backup_forge::handle(state, id, request).await,
        Some(Family::Daemon) => daemon::handle(state, id, request).await,
        Some(Family::GuiProcess) => gui_process::handle(state, id, request).await,
        Some(Family::Insight) => insight::handle(state, id, request).await,
        Some(Family::McpAcp) => mcp_acp::handle(state, id, request).await,
        Some(Family::MemorySource) => memory_source::handle(state, id, request).await,
        Some(Family::Policy) => policy::handle(state, id, request).await,
        Some(Family::Project) => project::handle(state, id, request).await,
        Some(Family::Session) => session::handle(state, id, request).await,
        Some(Family::Team) => team::handle(state, id, request).await,
        None => RpcResponse::error(
            id,
            RpcError::new(codes::METHOD_NOT_FOUND, "method not found"),
        ),
    };
    if matches!(
        method.as_str(),
        "project.add"
            | "project.shell_policy.set"
            | "project.contract_policy.set"
            | "project.agent_identity_policy.set"
            | "agent.identity.create"
            | "team.template.export"
            | "team.template.import"
            | "team.template.replay"
            | "agent.identity.update"
            | "agent.identity.pause"
            | "agent.identity.resume"
            | "agent.identity.retire"
            | "agent.hire"
            | "agent.hire.approve"
            | "agent.hire.reject"
            | "agent.promote"
            | "agent.transfer"
            | "agent.retire"
            | "agent.retirement.propose"
            | "agent.retirement.approve"
            | "agent.retirement.reject"
            | "agent.performance.replay"
            | "agent.employment.replay"
            | "project.retirement_policy.set"
            | "agent.identities.replay"
            | "agent.memory.remember"
            | "agent.memory.update"
            | "agent.memory.supersede"
            | "agent.memory.forget"
            | "agent.memory.verify"
            | "agent.memory.sweep"
            | "agent.memory.replay"
            | "session.assign"
            | "session.assignment.renew"
            | "session.release"
            | "agent.inbox.send"
            | "agent.inbox.read"
            | "agent.inbox.ack"
            | "agent.coordination.replay"
            | "agent.workspace.ensure"
            | "agent.workspace.land"
            | "agent.workspace.archive"
            | "agent.workspace.retire"
            | "agent.workspace.replay"
            | "project.effect_policy.set"
            | "project.cache_policy.set"
            | "project.integration_policy.set"
            | "project.budget.set"
            | "project.auto_budget.set"
            | "project.run_policy.set"
            | "project.worktree_policy.set"
            | "project.checkpoint_policy.set"
            | "project.task_scheduler.set"
            | "scaling.policy.set"
            | "host.add"
            | "host.update"
            | "host.remove"
            | "orchestration.policy.set"
            | "memory.policy.set"
            | "source.embedding.policy.set"
            | "source.embedding.reindex"
            | "source.retrieval.policy.set"
            | "process.policy.set"
            | "mailbox.policy.set"
            | "artifact.policy.set"
            | "tool_output.policy.set"
            | "session.import"
            | "mcp.pin.approve"
            | "mcp.pin.reject"
    ) && let Err(error) = state.engine.record_project_manifest().await
    {
        tracing::warn!(%error, "failed to update project manifest");
    }
    response
}

/// Known model references are daemon configuration, never client input. A
/// template binding to anything outside this set fails closed on import.
async fn template_known_models(state: &AppState) -> Vec<String> {
    let mut models = vp_core::team_template::configured_model_references(&state.agent.info());
    if let Ok(presets) = state.engine.list_model_presets().await {
        for preset in presets.presets {
            let model = preset.model.trim();
            if !model.is_empty() {
                models.push(model.to_string());
            }
        }
    }
    models.sort();
    models.dedup();
    models
}

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use axum::body::to_bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as Base64Engine;
use clap::Parser;
use fs2::FileExt;
use rand::RngCore;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tracing::{info, warn};
use vp_core::Engine;
use vp_core::agent::AgentService;
use vp_protocol::diagnosis::{
    SessionAttributionsParams, SessionDiagnosisParams, SessionDiagnosisRecordParams,
};
use vp_protocol::discovery::Discovery;
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
use vp_protocol::rpc::{RpcError, RpcNotification, RpcRequest, RpcResponse, codes};
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
    AgentInboxListParams, AgentInboxMessageParams, AgentInboxSendParams, AgentListParams,
    AgentListResult, AgentMemoryListParams, AgentMemoryParams, AgentMemoryReplayParams,
    AgentMessageResult, AgentOrgSnapshotParams, AgentPerformanceParams,
    AgentPerformanceReplayParams, AgentPerformanceSummaryParams, AgentPromoteParams,
    AgentRetireParams, AgentRetirementCandidatesParams, AgentRetirementPolicyParams,
    AgentRetirementProposalListParams, AgentRetirementProposalParams, AgentRetirementProposeParams,
    AgentRetirementRejectParams, AgentTransferParams, AgentWorkspaceArchiveParams,
    AgentWorkspaceEnsureParams, AgentWorkspaceLandParams, AgentWorkspaceListParams,
    AgentWorkspaceParams, AgentWorkspaceReplayParams, AgentWorkspaceRetireParams,
    AnswerQuestionParams, AnswerQuestionResult, ApprovalAuditParams, ApprovalBatchResolveParams,
    ApprovalBatchResolveResult, ApprovalGrantCreateParams, ApprovalGrantCreateResult,
    ApprovalGrantDeleteParams, ApprovalGrantDeleteResult, ApprovalGrantListParams,
    ApprovalGrantListResult, ApprovalListResult, ApprovalQueueParams, ApprovalTemplateCreateParams,
    ApprovalTemplateDeleteParams, ApprovalTemplateFromApprovalParams, ApprovalTemplateListParams,
    ApprovalTemplateListResult, ApprovalTemplateUpdateParams, ArtifactArchiveExtractParams,
    ArtifactArchiveListParams, ArtifactBackfillParams, ArtifactDiffParams, ArtifactGcParams,
    ArtifactGetParams, ArtifactListParams, ArtifactListResult, ArtifactMaterializeParams,
    ArtifactPolicyParams, ArtifactPolicyResult, ArtifactRestoreParams, ArtifactResult,
    ArtifactVersionsParams, AttentionPolicyGetParams, AttentionPolicySetParams, AuthParams,
    AuthResult, CachePolicyParams, CachePolicyResult, CancelSessionParams, CancelSessionResult,
    CheckpointPolicyParams, CleanupWorktreesParams, CommitSessionParams, ConfigureAgentParams,
    ConsoleCloseParams, ConsoleOpenParams, ConsoleReadParams, ConsoleSignalParams,
    ConsoleWriteParams, ContextCheckpointCreateParams, ContextCheckpointDeleteParams,
    ContextCheckpointDetailParams, ContextCheckpointListParams, ContextCheckpointPolicyParams,
    ContextCheckpointPruneParams, ContextRestoreParams, ContractPolicyParams, ContractPolicyResult,
    CreateAgentIdentityParams, CreateAgentMemoryParams, CreateModelPresetParams,
    CreateSessionParams, CriticalPathMode, CriticalPathPolicyParams, CriticalPathPolicyResult,
    DaemonMetrics, DaemonMetricsResult, DashboardParams, DeleteModelPresetParams,
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
    ProjectForkForestParams, ProjectListResult, ProjectMcpPinsParams, ProjectUsageParams,
    PromptParams, PromptResult, QuestionListParams, RehydrateSessionParams, ReplaySessionParams,
    ResolveApprovalParams, ResolveApprovalResult, ResolveMergeParams, ResolveMergeResult,
    ResumeAgentIdentityParams, RetireAgentIdentityParams, RetryMergeParams, RetryMergeResult,
    SchedulerStatusParams, SearchParams, ServerInfo, SessionAssignParams,
    SessionAssignmentRenewParams, SessionAssignmentsParams, SessionCacheAuditParams,
    SessionCacheBreaksParams, SessionCacheCallsParams, SessionCacheEpochParams, SessionCacheParams,
    SessionCachePlanParams, SessionCacheWarmParams, SessionCheckpointDeleteParams,
    SessionCheckpointDiffParams, SessionCheckpointListParams, SessionCheckpointParams,
    SessionCheckpointPruneParams, SessionCheckpointRestoreParams, SessionCheckpointStatsParams,
    SessionCitationAuditParams, SessionCitationsParams, SessionCompactParams, SessionCompareParams,
    SessionContextParams, SessionContinuationCancelParams, SessionContinuationReclaimParams,
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
    SetAgentIdentityPolicyParams, SetArtifactPolicyParams, SetCachePolicyParams,
    SetCheckpointPolicyParams, SetContextCheckpointPolicyParams, SetContractPolicyParams,
    SetCriticalPathPolicyParams, SetGatePolicyParams, SetIntegrationPolicyParams,
    SetMailboxPolicyParams, SetMemoryPolicyParams, SetPreemptionPolicyParams,
    SetProcessPolicyParams, SetProjectBudgetParams, SetProjectPromptLayoutParams,
    SetSessionPriorityParams, SetSessionPromptLayoutParams, SetSessionRunPolicyParams,
    SetSessionUnlimitedParams, SetShellPolicyParams, SetSourceCollectionParams,
    SetSourceEmbeddingPolicyParams, SetSourceLabelsParams, SetSourceRetrievalPolicyParams,
    SetTaskSchedulerPolicyParams, SetToolOutputPolicyParams, SetWorktreePolicyParams,
    ShellPolicyParams, ShellPolicyResult, SkillCreateParams, SkillEnabledParams, SkillIdParams,
    SkillImportParams, SkillListParams, SkillUpdateParams, SourceActionParams,
    SourceCollectionsParams, SourceCreateFileParams, SourceCreateNoteParams, SourceCreateUrlParams,
    SourceEmbeddingPolicyParams, SourceEmbeddingReindexParams, SourceEmbeddingStatusParams,
    SourceGetParams, SourceImportParams, SourceIndexRebuildParams, SourceIndexStatusParams,
    SourceListParams, SourceRetrievalPolicyParams, SourceSearchParams, SubscribeParams,
    SupersedeAgentMemoryParams, SweepAgentMemoriesParams, TaskActionParams, TaskActionResult,
    TaskDispatchParams, TaskDispatchResult, TaskQueueParams, TaskSchedulerPolicyParams,
    TeamPlanParams, TeamPlanResult, TeamTemplateDiffParams, TeamTemplateExportParams,
    TeamTemplateGetParams, TeamTemplateImportParams, TeamTemplateListParams,
    TeamTemplateReplayParams, TerminalCloseParams, TerminalOpenParams, TerminalReadParams,
    TerminalResizeParams, TerminalWriteParams, ToolActivityParams, ToolBundleDiffParams,
    ToolBundleHistoryParams, ToolBundleMatrixParams, ToolBundleParams, ToolOutputAuditParams,
    ToolOutputListParams, ToolOutputPolicyParams, ToolOutputPolicyResult, ToolOutputPruneParams,
    ToolOutputStatusParams, UpdateAcpAgentParams, UpdateAgentIdentityParams,
    UpdateAgentMemoryParams, UpdateCheckParams, UpdateDownloadParams, UpdateMcpServerParams,
    UpdateModelPresetParams, UpdateSourceMetadataParams, UsageLedgerParams,
    VerifyAgentMemoryParams, WorkspaceFileParams, WorkspaceGrepParams, WorkspaceListParams,
    WorkspaceSearchParams, WorkspaceTreeParams, WorkspaceWriteParams, WorktreePolicyParams,
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
use vp_protocol::{APP_VERSION, PROTOCOL_VERSION, now_rfc3339};
use vp_store::Store;
use vp_store::paths;

mod metrics;

#[derive(Debug, Parser)]
#[command(name = "verlinpadd", about = "VerlinPark local agent daemon")]
struct Args {
    /// Override the local data directory (useful for development and tests).
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// Bind a specific loopback port. Defaults to an ephemeral port.
    #[arg(long)]
    port: Option<u16>,
}

#[derive(Clone)]
struct AppState {
    engine: Engine,
    agent: AgentService,
    token: Arc<String>,
    data_dir: PathBuf,
    started_at: String,
    started: Instant,
    pid: u32,
}

/// Recursive Leader -> Manager -> Worker -> Labour delegation keeps a boxed
/// async future per level. Give daemon workers a deterministic stack budget so
/// a deep team graph cannot overflow the default 2 MiB worker stack.
const DAEMON_WORKER_STACK_BYTES: usize = 16 * 1024 * 1024;

fn main() -> Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(DAEMON_WORKER_STACK_BYTES)
        .build()?
        .block_on(run())
}

async fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "verlinpadd=info,vp_core=info".into()),
        )
        .with_target(false)
        .compact()
        .init();

    let args = Args::parse();
    let data_dir = args.data_dir.clone().unwrap_or_else(paths::data_dir);
    paths::ensure_private_dir(&data_dir)?;
    let _lock_file = acquire_instance_lock(&data_dir)?;

    let store = Store::open(&data_dir)?;
    let engine = Engine::new(store);
    let recovered = engine.recover_interrupted_sessions().await?;
    if recovered > 0 {
        tracing::warn!(
            sessions = recovered,
            "marked sessions interrupted by the previous daemon as failed"
        );
    }
    match engine.reconcile_session_agent_assignments().await {
        Ok(0) => {}
        Ok(expired) => tracing::info!(
            expired,
            "released expired identity leases from the previous daemon"
        ),
        Err(error) => tracing::warn!(%error, "failed to reconcile identity leases"),
    }
    match engine.sweep_agent_inbox_expiry().await {
        Ok(0) => {}
        Ok(expired) => tracing::info!(expired, "expired due project inbox rows"),
        Err(error) => tracing::warn!(%error, "failed to sweep project inbox expiry"),
    }
    match engine.sweep_agent_workspaces(Some(1_000)).await {
        Ok(0) => {}
        Ok(released) => tracing::info!(released, "reconciled Worker workspace leases"),
        Err(error) => tracing::warn!(%error, "failed to reconcile Worker workspace leases"),
    }
    if let Err(error) = engine.retire_legacy_gui_leases().await {
        tracing::warn!(%error, "failed to retire legacy GUI leases");
    }
    match engine.reconcile_supervised_processes().await {
        Ok(report)
            if report.adopted > 0
                || report.lost > 0
                || report.unknown > 0
                || report.skipped_imported > 0 =>
        {
            tracing::info!(
                adopted = report.adopted,
                lost = report.lost,
                unknown = report.unknown,
                restart_scheduled = report.restart_scheduled,
                restart_blocked = report.restart_blocked,
                skipped_imported = report.skipped_imported,
                "reconciled durable supervised processes"
            );
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "failed to reconcile supervised processes"),
    }
    if let Err(error) = engine.record_project_manifest().await {
        tracing::warn!(%error, "failed to write project manifest");
    }
    let agent = AgentService::from_env(engine.clone());
    match agent.resume_queued_sessions().await {
        Ok(0) => {}
        Ok(started) => tracing::info!(started, "re-enqueued persisted queued sessions"),
        Err(error) => tracing::warn!(%error, "failed to re-enqueue queued sessions"),
    }
    match agent.auto_resume_interrupted_runs().await {
        Ok(0) => {}
        Ok(started) => tracing::info!(started, "auto-resumed interrupted durable runs"),
        Err(error) => tracing::warn!(%error, "failed to auto-resume interrupted durable runs"),
    }
    let token = generate_token();
    let started_at = now_rfc3339();
    let state = AppState {
        engine,
        agent,
        token: Arc::new(token.clone()),
        data_dir: data_dir.clone(),
        started_at: started_at.clone(),
        started: Instant::now(),
        pid: std::process::id(),
    };
    state.agent.start_auto_resolver();
    state.agent.start_task_scheduler();
    state.agent.start_scaling_controller();
    spawn_worktree_sweeper(state.engine.clone());
    spawn_checkpoint_sweeper(state.engine.clone());
    spawn_tool_output_sweeper(state.engine.clone());
    spawn_memory_sweeper(state.engine.clone());
    spawn_memory_stale_sweeper(state.engine.clone());
    spawn_memory_conflict_scanner(state.engine.clone());
    spawn_memory_embedding_reconciler(state.engine.clone());
    spawn_source_embedding_reconciler(state.engine.clone());
    spawn_terminal_sweeper(state.engine.clone());
    spawn_browser_sweeper(state.engine.clone());
    spawn_computer_sweeper(state.engine.clone());
    spawn_gui_grace_sweeper(state.engine.clone());
    spawn_gui_timeline_retention_sweeper(state.engine.clone());
    spawn_task_stall_sweeper(state.engine.clone());
    spawn_effect_sweeper(state.engine.clone());
    spawn_gate_sweeper(state.engine.clone());
    spawn_coordination_sweeper(state.engine.clone());
    spawn_stall_recovery_reconciler(state.agent.clone());
    spawn_process_sweeper(state.engine.clone());
    spawn_approval_sweeper(state.engine.clone());
    spawn_orchestration_controller(state.engine.clone());

    let listener = TcpListener::bind(("127.0.0.1", args.port.unwrap_or(0))).await?;
    let addr = listener.local_addr()?;
    let discovery = Discovery {
        pid: state.pid,
        port: addr.port(),
        token,
        protocol: PROTOCOL_VERSION,
        version: APP_VERSION.to_string(),
        started_at: started_at.clone(),
    };
    write_discovery(&data_dir, &discovery)?;

    let app = Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics_http))
        .route(
            "/rpc",
            post(rpc_http).layer(axum::extract::DefaultBodyLimit::disable()),
        )
        .route("/ws", get(ws_handler))
        .route(
            "/gate/resolve",
            post(gate_resolve_http).layer(axum::extract::DefaultBodyLimit::max(16 * 1024)),
        )
        .with_state(state.clone());

    info!(
        addr = %addr,
        data_dir = %data_dir.display(),
        "verlinpadd listening"
    );

    let result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await;
    remove_discovery(&data_dir);
    result?;
    Ok(())
}

async fn health() -> Json<Value> {
    Json(json!({"ok": true, "version": APP_VERSION, "protocol": PROTOCOL_VERSION}))
}

/// Optional loopback-only gate resolve endpoint. It is disabled unless the
/// gate's project sets `loopback_resolve=true`; the scoped gate token is the
/// only credential. The daemon never binds a public interface, and this route
/// never resolves approvals, merges work, or exposes the token/verifier.
#[derive(serde::Deserialize)]
struct LoopbackGateResolve {
    gate_id: String,
    token: String,
    #[serde(default)]
    outcome: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

async fn gate_resolve_http(State(state): State<AppState>, request: Request) -> Response {
    let body = match to_bytes(request.into_body(), 16 * 1024).await {
        Ok(body) => body,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(
                    json!({"error": {"code": "GATE_INVALID", "message": "request body too large"}}),
                ),
            )
                .into_response();
        }
    };
    let params = match serde_json::from_slice::<LoopbackGateResolve>(&body) {
        Ok(params) => params,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": {"code": "GATE_INVALID", "message": "invalid request body"}})),
            )
                .into_response();
        }
    };
    match state
        .engine
        .resolve_gate_loopback(
            &params.gate_id,
            &params.token,
            params.outcome.as_deref(),
            params.note.as_deref(),
        )
        .await
    {
        Ok(result) => Json(result).into_response(),
        Err(error) => {
            let code = vp_core::gate::gate_error_code(&error).unwrap_or("GATE_INVALID");
            let status = match code {
                vp_core::gate::codes::GATE_NOT_FOUND => StatusCode::NOT_FOUND,
                vp_core::gate::codes::GATE_TOKEN_INVALID => StatusCode::UNAUTHORIZED,
                vp_core::gate::codes::GATE_LOOPBACK_DISABLED => StatusCode::FORBIDDEN,
                vp_core::gate::codes::GATE_EXPIRED | vp_core::gate::codes::GATE_CANCELLED => {
                    StatusCode::GONE
                }
                vp_core::gate::codes::GATE_CONFLICT
                | vp_core::gate::codes::GATE_ALREADY_WATCHED => StatusCode::CONFLICT,
                _ => StatusCode::BAD_REQUEST,
            };
            (
                status,
                Json(json!({"error": {"code": code, "message": format!("{error:#}")}})),
            )
                .into_response()
        }
    }
}

/// Prometheus scrape endpoint. It uses the same loopback Bearer token as
/// `/rpc`; Prometheus can send it through `authorization` in its scrape config.
async fn metrics_http(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !authorized(&headers, &state.token) {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "unauthorized\n",
        )
            .into_response();
    }
    match collect_daemon_metrics(&state).await {
        Ok(metrics) => {
            let body = metrics::render_prometheus(&metrics);
            ([(header::CONTENT_TYPE, "text/plain; version=0.0.4")], body).into_response()
        }
        Err(error) => {
            warn!(%error, "failed to collect daemon metrics");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                "metrics unavailable\n",
            )
                .into_response()
        }
    }
}

async fn collect_daemon_metrics(state: &AppState) -> Result<DaemonMetrics> {
    let store = state.engine.store().clone();
    let snapshot = tokio::task::spawn_blocking(move || store.daemon_metrics_snapshot()).await??;
    Ok(DaemonMetrics {
        version: APP_VERSION.to_string(),
        protocol: PROTOCOL_VERSION,
        uptime_secs: state.started.elapsed().as_secs(),
        projects: snapshot.projects,
        sessions: snapshot.sessions,
        plan_tasks: snapshot.plan_tasks,
        merge_queue: snapshot.merge_queue,
        intake: snapshot.intake,
        continuations: snapshot.continuations,
        recoveries: snapshot.recoveries,
        memories: snapshot.memories,
        memory_conflicts: snapshot.memory_conflicts,
        runtime_leases: snapshot.runtime_leases,
        supervised_processes: snapshot.supervised_processes,
        supervised_process_output_bytes: snapshot.supervised_process_output_bytes,
        preemptions_by_phase: snapshot.preemptions_by_phase,
        preemptions_by_reason: snapshot.preemptions_by_reason,
        tokens_in: snapshot.tokens_in,
        tokens_out: snapshot.tokens_out,
        cache_read_tokens: snapshot.cache_read_tokens,
        cache_write_tokens: snapshot.cache_write_tokens,
        cache_hit_ratio: vp_protocol::types::cache_hit_ratio(
            snapshot.tokens_in,
            snapshot.cache_read_tokens,
            snapshot.cache_write_tokens,
        ),
        preamble_tokens_estimate: snapshot.preamble_tokens_estimate,
        cost_spent_usd: snapshot.cost_spent_usd,
    })
}

const MAX_RPC_BODY_BYTES: usize = 64 * 1024 * 1024;

async fn rpc_http(State(state): State<AppState>, headers: HeaderMap, request: Request) -> Response {
    if !authorized(&headers, &state.token) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    }

    let body = match to_bytes(request.into_body(), MAX_RPC_BODY_BYTES).await {
        Ok(body) => body,
        Err(_) => {
            return Json(RpcResponse::error(
                None,
                RpcError::new(codes::INVALID_REQUEST, "request body too large"),
            ))
            .into_response();
        }
    };
    let body = match String::from_utf8(body.to_vec()) {
        Ok(body) => body,
        Err(_) => {
            return Json(RpcResponse::error(
                None,
                RpcError::new(codes::PARSE_ERROR, "request body is not UTF-8"),
            ))
            .into_response();
        }
    };
    let request: RpcRequest = match serde_json::from_str(&body) {
        Ok(request) => request,
        Err(error) => {
            return Json(RpcResponse::error(
                None,
                RpcError::new(codes::PARSE_ERROR, error.to_string()),
            ))
            .into_response();
        }
    };
    let response = dispatch(&state, request).await;
    Json(response).into_response()
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: AppState) {
    let auth = tokio::time::timeout(Duration::from_secs(10), socket.recv()).await;
    let request = match auth {
        Ok(Some(Ok(Message::Text(text)))) => serde_json::from_str::<RpcRequest>(&text).ok(),
        _ => None,
    };
    let Some(request) = request else {
        let _ = send_json(
            &mut socket,
            &RpcResponse::error(
                None,
                RpcError::new(codes::UNAUTHORIZED, "authentication required"),
            ),
        )
        .await;
        return;
    };

    if request.method != "auth" {
        let _ = send_json(
            &mut socket,
            &RpcResponse::error(
                request.id,
                RpcError::new(codes::UNAUTHORIZED, "first request must be auth"),
            ),
        )
        .await;
        return;
    }

    let params: AuthParams = match serde_json::from_value(request.params) {
        Ok(params) => params,
        Err(error) => {
            let _ = send_json(
                &mut socket,
                &RpcResponse::error(
                    request.id,
                    RpcError::new(codes::INVALID_PARAMS, error.to_string()),
                ),
            )
            .await;
            return;
        }
    };

    if params
        .protocol
        .is_some_and(|protocol| protocol != PROTOCOL_VERSION)
    {
        let _ = send_json(
            &mut socket,
            &RpcResponse::error(
                request.id,
                RpcError::new(
                    codes::INVALID_REQUEST,
                    format!(
                        "protocol version mismatch: client {} server {}",
                        params.protocol.unwrap_or_default(),
                        PROTOCOL_VERSION
                    ),
                ),
            ),
        )
        .await;
        return;
    }

    if !constant_time_eq(&params.token, &state.token) {
        let _ = send_json(
            &mut socket,
            &RpcResponse::error(
                request.id,
                RpcError::new(codes::UNAUTHORIZED, "invalid token"),
            ),
        )
        .await;
        return;
    }

    let server = match server_info(&state).await {
        Ok(server) => server,
        Err(error) => {
            let _ = send_json(
                &mut socket,
                &RpcResponse::error(
                    request.id,
                    RpcError::new(codes::INTERNAL_ERROR, error.to_string()),
                ),
            )
            .await;
            return;
        }
    };
    if send_json(
        &mut socket,
        &RpcResponse::success(request.id, AuthResult { ok: true, server }),
    )
    .await
    .is_err()
    {
        return;
    }

    info!(pid = state.pid, "websocket client authenticated");
    let mut events = state.engine.subscribe();
    let mut filter: Option<String> = None;

    loop {
        tokio::select! {
            outgoing = socket.recv() => {
                match outgoing {
                    Some(Ok(Message::Text(text))) => {
                        let request: RpcRequest = match serde_json::from_str(&text) {
                            Ok(request) => request,
                            Err(error) => {
                                let _ = send_json(
                                    &mut socket,
                                    &RpcResponse::error(None, RpcError::new(codes::PARSE_ERROR, error.to_string())),
                                ).await;
                                continue;
                            }
                        };
                        if request.method == "event.subscribe" {
                            let params: SubscribeParams = if request.params.is_null() {
                                SubscribeParams { session_id: None }
                            } else {
                                match serde_json::from_value(request.params) {
                                    Ok(params) => params,
                                    Err(error) => {
                                        let _ = send_json(
                                            &mut socket,
                                            &RpcResponse::error(
                                                request.id,
                                                RpcError::new(codes::INVALID_PARAMS, error.to_string()),
                                            ),
                                        ).await;
                                        continue;
                                    }
                                }
                            };
                            filter = params.session_id;
                            let _ = send_json(
                                &mut socket,
                                &RpcResponse::success(request.id, json!({"subscribed": true})),
                            ).await;
                            continue;
                        }
                        let response = dispatch(&state, request).await;
                        if send_json(&mut socket, &response).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        if socket.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        warn!(%error, "websocket receive error");
                        break;
                    }
                }
            }
            event = events.recv() => {
                match event {
                    Ok(event) => {
                        if filter.as_deref().is_some_and(|id| id != event.session_id) {
                            continue;
                        }
                        let notification = RpcNotification::new("event.appended", &event);
                        if send_json(&mut socket, &notification).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(skipped, "websocket event subscriber lagged");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

async fn dispatch(state: &AppState, request: RpcRequest) -> RpcResponse {
    let id = request.id.clone();
    let method = request.method.clone();
    let response = match request.method.as_str() {
        "ping" => RpcResponse::success(
            id,
            PingResult {
                pong: true,
                version: APP_VERSION.to_string(),
                protocol: PROTOCOL_VERSION,
                uptime_secs: state.started.elapsed().as_secs(),
            },
        ),
        "server.info" => match server_info(state).await {
            Ok(info) => RpcResponse::success(id, info),
            Err(error) => internal_error(id, error),
        },
        "daemon.metrics" => match collect_daemon_metrics(state).await {
            Ok(metrics) => {
                let prometheus = metrics::render_prometheus(&metrics);
                RpcResponse::success(
                    id,
                    DaemonMetricsResult {
                        metrics,
                        prometheus,
                    },
                )
            }
            Err(error) => internal_error(id, error),
        },
        "project.add" => match parse_params::<AddProjectParams>(request.params) {
            Ok(params) => match state.engine.add_project(params.root, params.name).await {
                Ok(project) => RpcResponse::success(id, project),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "server.doctor" => match state.engine.doctor_checks().await {
            Ok(mut checks) => {
                let info = state.agent.info();
                checks.push(DoctorCheck {
                    name: "provider".into(),
                    status: if info.configured { "ok" } else { "warn" }.into(),
                    detail: if info.configured {
                        format!(
                            "{} / {}",
                            info.provider.as_deref().unwrap_or("?"),
                            info.model.as_deref().unwrap_or("?")
                        )
                    } else {
                        "no provider configured".to_string()
                    },
                });
                let ok = !checks.iter().any(|check| check.status == "error");
                RpcResponse::success(id, DoctorReport { checks, ok })
            }
            Err(error) => internal_error(id, error),
        },
        "project.list" => match state.engine.list_projects().await {
            Ok(projects) => RpcResponse::success(id, ProjectListResult { projects }),
            Err(error) => internal_error(id, error),
        },
        "host.list" => match parse_params::<HostListParams>(request.params) {
            Ok(_) => match state.engine.list_execution_hosts().await {
                Ok(hosts) => RpcResponse::success(id, HostListResult { hosts }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "host.add" | "host.update" => match parse_params::<HostSetParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_execution_host(&params.project_id, params.config)
                .await
            {
                Ok(config) => RpcResponse::success(id, config),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "host.remove" => match parse_params::<HostProjectParams>(request.params) {
            Ok(params) => match state.engine.remove_execution_host(&params.project_id).await {
                Ok(removed) => RpcResponse::success(id, json!({"removed": removed})),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "host.probe" => match parse_params::<HostProbeParams>(request.params) {
            Ok(params) => match state
                .engine
                .probe_execution_host(&params.project_id, params.timeout_secs)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "remote.status" => match parse_params::<RemoteStatusParams>(request.params) {
            Ok(params) => match state.engine.remote_status(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "lsp.status" => match parse_params::<LspStatusParams>(request.params) {
            Ok(params) => match state.engine.lsp_status(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "remote.ledger" => match parse_params::<RemoteLedgerParams>(request.params) {
            Ok(params) => match state
                .engine
                .remote_ledger(&params.project_id, params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "remote.worktree_list" => match parse_params::<RemoteWorktreeListParams>(request.params) {
            Ok(params) => match state.engine.remote_worktree_list(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "remote.integrate" => match parse_params::<RemoteIntegrationParams>(request.params) {
            Ok(params) => match state.agent.integrate_remote(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => remote_integration_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "remote.bootstrap" => match parse_params::<RemoteBootstrapParams>(request.params) {
            Ok(params) => match state.engine.remote_bootstrap(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => remote_bootstrap_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "remote.bootstrap.status" => {
            match parse_params::<RemoteBootstrapStatusParams>(request.params) {
                Ok(params) => match state.engine.remote_bootstrap_status(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "remote.test" => match parse_params::<RemoteTestParams>(request.params) {
            Ok(params) => match state.engine.run_remote_test(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => remote_test_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "remote.test.status" => match parse_params::<RemoteTestStatusParams>(request.params) {
            Ok(params) => match state.engine.remote_test_status(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => remote_test_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "remote.test.output" => match parse_params::<RemoteTestOutputParams>(request.params) {
            Ok(params) => match state.engine.remote_test_output(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => remote_test_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "remote.test.policy.get" => match parse_params::<RemoteTestPolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .remote_test_policy_result(&params.project_id)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "remote.test.policy.set" => {
            match parse_params::<SetRemoteTestPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_remote_test_policy(&params.project_id, params.policy)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "remote.integration.status" => {
            match parse_params::<RemoteIntegrationStatusParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .remote_integration_status(&params.project_id, params.session_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "remote.integration.policy.get" => {
            match parse_params::<RemoteIntegrationPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .remote_integration_policy_result(&params.project_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "remote.integration.cleanup" => {
            match parse_params::<RemoteIntegrationCleanupParams>(request.params) {
                Ok(params) => match state.engine.remote_integration_cleanup(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "remote.integration.policy.set" => {
            match parse_params::<SetRemoteIntegrationPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_remote_integration_policy(
                        &params.project_id,
                        params.policy,
                        params.confirm,
                    )
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => remote_integration_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "project.task_scheduler.get" => {
            match parse_params::<TaskSchedulerPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .get_task_scheduler_policy(&params.project_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "project.task_scheduler.set" => {
            match parse_params::<SetTaskSchedulerPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_task_scheduler_policy(&params.project_id, params.policy)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "scaling.policy.get" => match parse_params::<ScalingPolicyParams>(request.params) {
            Ok(params) => match state.engine.get_scaling_policy(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "scaling.policy.set" => match parse_params::<SetScalingPolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_scaling_policy(&params.project_id, params.policy)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "scaling.preview" => match parse_params::<ScalingPreviewParams>(request.params) {
            Ok(params) => match state.agent.scaling_preview(&params.session_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "scaling.status" => match parse_params::<ScalingStatusParams>(request.params) {
            Ok(params) => match state
                .agent
                .scaling_status(
                    params.project_id.as_deref(),
                    params.session_id.as_deref(),
                    params.limit,
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "orchestration.policy.get" => {
            match parse_params::<OrchestrationPolicyParams>(request.params) {
                Ok(params) => match state.engine.orchestration_policy(&params.project_id).await {
                    Ok(policy) => RpcResponse::success(
                        id,
                        OrchestrationPolicyResult {
                            project_id: params.project_id,
                            policy,
                        },
                    ),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "orchestration.policy.set" => {
            match parse_params::<SetOrchestrationPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_orchestration_policy(&params.project_id, &params.policy)
                    .await
                {
                    Ok(policy) => RpcResponse::success(
                        id,
                        OrchestrationPolicyResult {
                            project_id: params.project_id,
                            policy,
                        },
                    ),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "critical_path.policy.get" => {
            match parse_params::<CriticalPathPolicyParams>(request.params) {
                Ok(params) => match state.engine.critical_path_policy(&params.project_id).await {
                    Ok(policy) => RpcResponse::success(
                        id,
                        CriticalPathPolicyResult {
                            project_id: params.project_id,
                            policy,
                        },
                    ),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "critical_path.policy.set" => {
            match parse_params::<SetCriticalPathPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_critical_path_policy(&params.project_id, &params.policy)
                    .await
                {
                    Ok(policy) => RpcResponse::success(
                        id,
                        CriticalPathPolicyResult {
                            project_id: params.project_id,
                            policy,
                        },
                    ),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.budget.governor" => match parse_params::<GovernorStatusParams>(request.params) {
            Ok(params) => match state
                .engine
                .budget_governor_status(&params.session_id, params.limit.unwrap_or(20))
                .await
            {
                Ok(status) => RpcResponse::success(id, status),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "governor.preview" => match parse_params::<GovernorPreviewParams>(request.params) {
            Ok(params) => {
                let mode = match params.mode.as_deref() {
                    None => None,
                    Some(value) => match CriticalPathMode::parse(value) {
                        Some(mode) => Some(mode),
                        None => {
                            return RpcResponse::error(
                                id,
                                RpcError::new(
                                    codes::INVALID_PARAMS,
                                    format!(
                                        "invalid governor preview mode {value:?}; use `single` or `multi`"
                                    ),
                                ),
                            );
                        }
                    },
                };
                match state
                    .engine
                    .preview_budget_governor_with_mode(
                        &params.session_id,
                        params.remaining_tokens,
                        params.limit.unwrap_or(20),
                        mode,
                    )
                    .await
                {
                    Ok(status) => RpcResponse::success(id, status),
                    Err(error) => internal_error(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "runtime.leases" => match state.engine.list_runtime_leases().await {
            Ok(leases) => {
                let identity = state.engine.daemon_identity();
                let result = RuntimeLeaseListResult {
                    runtime_id: state.engine.runtime_id().to_string(),
                    daemon_instance: Some(DaemonInstanceInfo {
                        instance_id: identity.instance_id.clone(),
                        pid: identity.pid,
                        started_at: identity.started_at.clone(),
                    }),
                    leases: leases
                        .into_iter()
                        .map(|lease| RuntimeLeaseInfo {
                            state: lease.state().as_str().to_string(),
                            lease_key: lease.lease_key,
                            owner_id: lease.owner_id,
                            owner_instance: lease.owner_instance,
                            kind: lease.kind,
                            session_id: lease.session_id,
                            fencing_token: lease.fencing_token,
                            acquired_at: lease.acquired_at,
                            heartbeat_at: lease.heartbeat_at,
                            renewed_at: lease.renewed_at,
                            expires_at: lease.expires_at,
                            released_at: lease.released_at,
                        })
                        .collect(),
                };
                RpcResponse::success(id, result)
            }
            Err(error) => internal_error(id, error),
        },
        "runtime.leases.sweep" => match state.engine.sweep_runtime_leases().await {
            Ok(removed) => RpcResponse::success(id, RuntimeLeaseSweepResult { removed }),
            Err(error) => internal_error(id, error),
        },
        "runtime.leases.history" => {
            match parse_params::<RuntimeLeaseHistoryParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .runtime_lease_audit(params.lease_key.as_deref(), params.limit)
                    .await
                {
                    Ok(entries) => RpcResponse::success(
                        id,
                        RuntimeLeaseHistoryResult {
                            lease_key: params.lease_key,
                            entries: entries
                                .into_iter()
                                .map(|entry| RuntimeLeaseAuditInfo {
                                    id: entry.id,
                                    lease_key: entry.lease_key,
                                    kind: entry.kind,
                                    action: entry.action,
                                    owner_id: entry.owner_id,
                                    owner_instance: entry.owner_instance,
                                    fencing_token: entry.fencing_token,
                                    detail: entry.detail,
                                    occurred_at: entry.occurred_at,
                                })
                                .collect(),
                        },
                    ),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "computer.leases" => match state.engine.list_computer_leases().await {
            Ok(leases) => RpcResponse::success(
                id,
                ComputerLeaseListResult {
                    runtime_id: state.engine.runtime_id().to_string(),
                    leases: leases
                        .into_iter()
                        .map(|lease| ComputerLeaseInfo {
                            lease_id: lease.lease_id,
                            surface: lease.surface,
                            owner_id: lease.owner_id,
                            session_id: lease.session_id,
                            agent_id: lease.agent_id,
                            agent_name: lease.agent_name,
                            acquired_at: lease.acquired_at,
                            heartbeat_at: lease.heartbeat_at,
                            expires_at: lease.expires_at,
                            generation: lease.generation,
                        })
                        .collect(),
                },
            ),
            Err(error) => internal_error(id, error),
        },
        "computer.leases.sweep" => match state.engine.sweep_computer_leases().await {
            Ok(removed) => RpcResponse::success(id, ComputerLeaseSweepResult { removed }),
            Err(error) => internal_error(id, error),
        },
        "computer.leases.release" => {
            match parse_params::<ComputerLeaseReleaseParams>(request.params) {
                Ok(params) => match state.engine.release_computer_lease(&params.lease_id).await {
                    Ok(released) => {
                        RpcResponse::success(id, ComputerLeaseReleaseResult { released })
                    }
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "coordination.note" => match parse_params::<CoordinationNoteParams>(request.params) {
            Ok(params) => match state
                .engine
                .record_user_coordination_note(
                    &params.session_id,
                    &params.action,
                    params.task_id.as_deref(),
                    params.target_agent_id.as_deref(),
                    params.target_note_id.as_deref(),
                    &params.message,
                    params.severity.as_deref(),
                )
                .await
            {
                Ok(note_id) => RpcResponse::success(id, CoordinationNoteResult { note_id }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "orchestration.overview" => {
            match parse_params::<OrchestrationOverviewParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .orchestration_overview(
                        params.project_id.as_deref(),
                        params.limit.unwrap_or(20),
                    )
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "orchestration.preview" => {
            match parse_params::<OrchestrationPreviewParams>(request.params) {
                Ok(params) => match state.engine.preview_orchestration(&params.session_id).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "orchestration.advance" => {
            match parse_params::<OrchestrationAdvanceParams>(request.params) {
                Ok(params) => match state.engine.advance_orchestration(&params.session_id).await {
                    Ok(actions) => RpcResponse::success(
                        id,
                        OrchestrationAdvanceResult {
                            session_id: params.session_id,
                            actions,
                        },
                    ),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "orchestration.replay" => match parse_params::<ControllerReplayParams>(request.params) {
            Ok(params) => match state
                .engine
                .replay_orchestration(&params.session_id, params.limit)
                .await
            {
                Ok(report) => RpcResponse::success(id, report),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "orchestration.snapshot" => {
            match parse_params::<OrchestrationSnapshotParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .orchestration_snapshot(&params.session_id)
                    .await
                {
                    Ok(snapshot) => {
                        RpcResponse::success(id, OrchestrationSnapshotResult { snapshot })
                    }
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "orchestration.recoveries" => {
            match parse_params::<OrchestrationRecoveriesParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .orchestration_recoveries(&params.session_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "team_plan.revisions" => match parse_params::<PlanRevisionListParams>(request.params) {
            Ok(params) => match state.engine.plan_revisions(&params.session_id).await {
                Ok(revisions) => RpcResponse::success(id, PlanRevisionListResult { revisions }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "team_plan.revision" => match parse_params::<PlanRevisionGetParams>(request.params) {
            Ok(params) => match state
                .agent
                .plan_revision_detail(&params.session_id, params.revision)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "team_plan.restore" => match parse_params::<PlanRevisionRestoreParams>(request.params) {
            Ok(params) => match state
                .agent
                .restore_team_plan_revision(
                    &params.session_id,
                    params.revision,
                    &params.reason,
                    params.expected_revision,
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "team_plan.task_edit" => match parse_params::<TaskEditParams>(request.params) {
            Ok(params) => match state
                .agent
                .edit_plan_task(
                    &params.session_id,
                    params.task,
                    &params.reason,
                    params.expected_revision,
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "team_plan.revise" => match parse_params::<ReviseTeamPlanParams>(request.params) {
            Ok(params) => match state
                .agent
                .revise_team_plan(
                    &params.session_id,
                    params.plan,
                    &params.reason,
                    params.expected_revision,
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.shell_policy.get" => match parse_params::<ShellPolicyParams>(request.params) {
            Ok(params) => match state.engine.get_shell_policy(&params.project_id).await {
                Ok(policy) => RpcResponse::success(
                    id,
                    ShellPolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.shell_policy.set" => match parse_params::<SetShellPolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_shell_policy(&params.project_id, params.policy)
                .await
            {
                Ok(policy) => RpcResponse::success(
                    id,
                    ShellPolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.contract_policy.get" => {
            match parse_params::<ContractPolicyParams>(request.params) {
                Ok(params) => match state.engine.get_contract_policy(&params.project_id).await {
                    Ok(policy) => RpcResponse::success(
                        id,
                        ContractPolicyResult {
                            project_id: params.project_id,
                            policy,
                        },
                    ),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "project.contract_policy.set" => {
            match parse_params::<SetContractPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_contract_policy(&params.project_id, params.policy)
                    .await
                {
                    Ok(policy) => RpcResponse::success(
                        id,
                        ContractPolicyResult {
                            project_id: params.project_id,
                            policy,
                        },
                    ),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.contract" => match parse_params::<SessionContractParams>(request.params) {
            Ok(params) => {
                let task = params.task_id.as_deref();
                match state
                    .engine
                    .session_contract_status(&params.session_id, task)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "project.effect_policy.get" => match parse_params::<EffectPolicyParams>(request.params) {
            Ok(params) => match state.engine.get_effect_policy(&params.project_id).await {
                Ok(policy) => RpcResponse::success(
                    id,
                    EffectPolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.effect_policy.set" => {
            match parse_params::<vp_protocol::types::SetEffectPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_effect_policy(&params.project_id, params.policy)
                    .await
                {
                    Ok(policy) => RpcResponse::success(
                        id,
                        EffectPolicyResult {
                            project_id: params.project_id,
                            policy,
                        },
                    ),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.effects" => match parse_params::<SessionEffectsParams>(request.params) {
            Ok(params) => {
                let include_settled = params.include_settled.unwrap_or(true);
                match state
                    .engine
                    .session_effects(
                        &params.session_id,
                        params.task_id.as_deref(),
                        include_settled,
                        params.limit.unwrap_or(100),
                    )
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "effect.reconcile" => match parse_params::<EffectReconcileParams>(request.params) {
            Ok(params) => match state.engine.reconcile_effect(params).await {
                Ok(result) => {
                    let _ = state.engine.record_project_manifest().await;
                    RpcResponse::success(
                        id,
                        EffectReconcileResult {
                            receipt: result.receipt,
                            changed: result.changed,
                        },
                    )
                }
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "effect.sweep" => {
            #[derive(serde::Deserialize)]
            struct EffectSweepParams {
                #[serde(default)]
                limit: Option<u32>,
            }
            match parse_params::<EffectSweepParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .sweep_stale_effect_intents(params.limit.unwrap_or(100))
                    .await
                {
                    Ok(swept) => RpcResponse::success(id, json!({"swept": swept})),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "gate.open" => match parse_params::<GateOpenParams>(request.params) {
            Ok(params) => match state.engine.open_gate(params).await {
                Ok(outcome) => RpcResponse::success(id, vp_core::gate::open_result(outcome)),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "gate.resolve" => match parse_params::<GateResolveParams>(request.params) {
            Ok(params) => match state
                .engine
                .resolve_gate(
                    &params.gate_id,
                    &params.token,
                    params.outcome.as_deref(),
                    params.note.as_deref(),
                    params.resolved_by.as_deref(),
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "gate.cancel" => match parse_params::<GateCancelParams>(request.params) {
            Ok(params) => match state
                .engine
                .cancel_gate(
                    &params.gate_id,
                    params.reason.as_deref(),
                    params.cancelled_by.as_deref(),
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "gate.list" => match parse_params::<GateListParams>(request.params) {
            Ok(params) => match state
                .engine
                .list_gates(&params.session_id, params.state.as_deref(), params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "gate.status" => match parse_params::<GateStatusParams>(request.params) {
            Ok(params) => match state.engine.gate_status(&params.gate_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "gate.sweep" => match parse_params::<GateSweepParams>(request.params) {
            Ok(params) => match state
                .engine
                .sweep_expired_gates(params.limit.unwrap_or(100))
                .await
            {
                Ok(expired) => RpcResponse::success(
                    id,
                    vp_protocol::types::GateSweepResult {
                        expired_gates: expired as u64,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.gate_policy.get" => match parse_params::<GatePolicyParams>(request.params) {
            Ok(params) => match state.engine.get_gate_policy(&params.project_id).await {
                Ok(policy) => RpcResponse::success(
                    id,
                    GatePolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.gate_policy.set" => match parse_params::<SetGatePolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_gate_policy(&params.project_id, params.policy)
                .await
            {
                Ok(policy) => RpcResponse::success(
                    id,
                    GatePolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.otel_policy.get" => match parse_params::<OtelPolicyParams>(request.params) {
            Ok(params) => match state.engine.get_otel_policy(&params.project_id).await {
                Ok(policy) => {
                    let endpoint_valid = vp_core::otel::validate_endpoint(&policy.endpoint).is_ok();
                    RpcResponse::success(
                        id,
                        vp_protocol::otel::OtelPolicyResult {
                            project_id: params.project_id,
                            policy,
                            endpoint_valid,
                            endpoint_loopback: endpoint_valid,
                            warnings: Vec::new(),
                        },
                    )
                }
                Err(error) => otel_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.otel_policy.set" => match parse_params::<SetOtelPolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_otel_policy(&params.project_id, params.policy)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => otel_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "telemetry.otel.status" => match parse_params::<OtelStatusParams>(request.params) {
            Ok(params) => match state
                .engine
                .otel_status(&params.project_id, params.session_id.as_deref())
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => otel_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "telemetry.otel.export" => match parse_params::<OtelExportParams>(request.params) {
            Ok(params) => match state
                .engine
                .otel_export(&params.session_id, params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => otel_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "telemetry.otel.test" => match parse_params::<OtelTestParams>(request.params) {
            Ok(params) => match state.engine.otel_test(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => otel_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.mcp_pins" => match parse_params::<ProjectMcpPinsParams>(request.params) {
            Ok(params) => match state
                .engine
                .project_mcp_pins(&params.project_id, params.server_id.as_deref())
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.pin.approve" => match parse_params::<McpPinApproveParams>(request.params) {
            Ok(params) => match state.engine.approve_mcp_pin(&params, None).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.pin.reject" => match parse_params::<McpPinRejectParams>(request.params) {
            Ok(params) => match state.engine.reject_mcp_pin(&params, None).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "preemption.overview" => match parse_params::<PreemptionOverviewParams>(request.params) {
            Ok(params) => match state
                .engine
                .preemption_overview(&params.project_id, params.limit.unwrap_or(100))
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.preemption_policy.get" => {
            match parse_params::<PreemptionPolicyParams>(request.params) {
                Ok(params) => match state.engine.get_preemption_policy(&params.project_id).await {
                    Ok(policy) => RpcResponse::success(
                        id,
                        PreemptionPolicyResult {
                            project_id: params.project_id,
                            policy,
                        },
                    ),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "project.preemption_policy.set" => {
            match parse_params::<SetPreemptionPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_preemption_policy(&params.project_id, params.policy)
                    .await
                {
                    Ok(policy) => RpcResponse::success(
                        id,
                        PreemptionPolicyResult {
                            project_id: params.project_id,
                            policy,
                        },
                    ),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "project.cache_policy.get" => match parse_params::<CachePolicyParams>(request.params) {
            Ok(params) => match state.engine.get_cache_policy(&params.project_id).await {
                Ok(policy) => RpcResponse::success(
                    id,
                    CachePolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.cache_policy.set" => match parse_params::<SetCachePolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_cache_policy(&params.project_id, params.policy)
                .await
            {
                Ok(policy) => RpcResponse::success(
                    id,
                    CachePolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.integration_policy.get" => {
            match parse_params::<IntegrationPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .get_integration_policy(&params.project_id)
                    .await
                {
                    Ok(policy) => RpcResponse::success(
                        id,
                        IntegrationPolicyResult {
                            project_id: params.project_id,
                            policy,
                        },
                    ),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "project.integration_policy.set" => {
            match parse_params::<SetIntegrationPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_integration_policy(&params.project_id, params.policy)
                    .await
                {
                    Ok(policy) => {
                        let project_id = params.project_id.clone();
                        if policy.auto_resolve {
                            let agent = state.agent.clone();
                            tokio::spawn(async move {
                                agent.auto_resolve_project(&project_id).await;
                            });
                        }
                        RpcResponse::success(
                            id,
                            IntegrationPolicyResult {
                                project_id: params.project_id,
                                policy,
                            },
                        )
                    }
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.create" => match parse_params::<CreateSessionParams>(request.params) {
            Ok(params) => {
                let created = state
                    .engine
                    .create_session_with_options_and_unlimited(
                        params.project_id,
                        params.goal.clone(),
                        params.token_budget,
                        params.cost_budget_usd,
                        params.time_budget_secs,
                        params.priority.unwrap_or(0),
                        params.unlimited,
                    )
                    .await;
                match created {
                    Ok(session) => {
                        let info = state.agent.info();
                        let model = info
                            .role_models
                            .leader
                            .model
                            .clone()
                            .or(info.model)
                            .unwrap_or_else(|| "unconfigured".to_string());
                        let _ = state.engine.create_leader_agent(&session.id, &model).await;
                        if let Err(error) = state.agent.start(&session.id, &params.goal).await {
                            tracing::warn!(%error, session_id = %session.id, "agent did not start");
                        }
                        RpcResponse::success(id, session)
                    }
                    Err(error) => internal_error(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "artifact.diff" => match parse_params::<ArtifactDiffParams>(request.params) {
            Ok(params) => match state.engine.artifact_diff(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "artifact.versions" => match parse_params::<ArtifactVersionsParams>(request.params) {
            Ok(params) => match state.engine.artifact_versions(&params.artifact_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "artifact.restore" => match parse_params::<ArtifactRestoreParams>(request.params) {
            Ok(params) => match state
                .engine
                .restore_artifact_version(&params.artifact_id, params.version)
                .await
            {
                Ok(artifact) => {
                    let provenance = artifact.provenance.clone();
                    RpcResponse::success(
                        id,
                        ArtifactResult {
                            artifact,
                            provenance,
                        },
                    )
                }
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "skill.list" => match parse_params::<SkillListParams>(request.params) {
            Ok(params) => match state
                .engine
                .list_skills(params.project_id, params.include_disabled.unwrap_or(true))
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "skill.create" => match parse_params::<SkillCreateParams>(request.params) {
            Ok(params) => match state.engine.create_skill(params).await {
                Ok(skill) => RpcResponse::success(id, skill),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "skill.import" => match parse_params::<SkillImportParams>(request.params) {
            Ok(params) => match state.engine.import_skill(params).await {
                Ok(skill) => RpcResponse::success(id, skill),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "skill.update" => match parse_params::<SkillUpdateParams>(request.params) {
            Ok(params) => match state.engine.update_skill(params).await {
                Ok(skill) => RpcResponse::success(id, skill),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "skill.set_enabled" => match parse_params::<SkillEnabledParams>(request.params) {
            Ok(params) => match state.engine.set_skill_enabled(params).await {
                Ok(skill) => RpcResponse::success(id, skill),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "skill.delete" => match parse_params::<SkillIdParams>(request.params) {
            Ok(params) => match state.engine.delete_skill(params.id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.list" => match parse_params::<SourceListParams>(request.params) {
            Ok(params) => match state
                .engine
                .list_sources(params.project_id, params.query, params.status, params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.update" => match parse_params::<UpdateSourceMetadataParams>(request.params) {
            Ok(params) => match state.engine.update_source_metadata(params).await {
                Ok(source) => RpcResponse::success(id, source),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.collections" => match parse_params::<SourceCollectionsParams>(request.params) {
            Ok(params) => match state.engine.source_collections(params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.set_collection" => {
            match parse_params::<SetSourceCollectionParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_source_collection(params.source_id, params.collection)
                    .await
                {
                    Ok(source) => RpcResponse::success(id, source),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "source.set_labels" => match parse_params::<SetSourceLabelsParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_source_labels(params.source_id, params.labels)
                .await
            {
                Ok(source) => RpcResponse::success(id, source),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.get" => match parse_params::<SourceGetParams>(request.params) {
            Ok(params) => match state
                .engine
                .get_source(params.source_id, params.offset, params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.add_note" => match parse_params::<SourceCreateNoteParams>(request.params) {
            Ok(params) => match state.engine.add_source_note(params).await {
                Ok(source) => RpcResponse::success(id, source),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.add_file" => match parse_params::<SourceCreateFileParams>(request.params) {
            Ok(params) => match state.engine.add_source_file(params).await {
                Ok(source) => RpcResponse::success(id, source),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.add_url" => match parse_params::<SourceCreateUrlParams>(request.params) {
            Ok(params) => match state.engine.add_source_url(params).await {
                Ok(source) => RpcResponse::success(id, source),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.import" => match parse_params::<SourceImportParams>(request.params) {
            Ok(params) => match state.engine.import_sources(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.delete" => match parse_params::<SourceActionParams>(request.params) {
            Ok(params) => match state.engine.delete_source(params.source_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.refresh" => match parse_params::<SourceActionParams>(request.params) {
            Ok(params) => match state.engine.refresh_source(params.source_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.reindex" => match parse_params::<SourceActionParams>(request.params) {
            Ok(params) => match state.engine.reindex_source(params.source_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.search" => match parse_params::<SourceSearchParams>(request.params) {
            Ok(params) => match state
                .engine
                .search_sources_with_mode(
                    params.project_id,
                    params.query,
                    params.source_ids,
                    params.collections,
                    params.labels,
                    params.limit,
                    params.mode,
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.embedding.status" => {
            match parse_params::<SourceEmbeddingStatusParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .source_embedding_status(&params.project_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "source.embedding.reindex" => {
            match parse_params::<SourceEmbeddingReindexParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .reindex_source_embeddings(&params.project_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "source.embedding.policy.get" => {
            match parse_params::<SourceEmbeddingPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .get_source_embedding_policy(&params.project_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "source.embedding.policy.set" => {
            match parse_params::<SetSourceEmbeddingPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_source_embedding_policy(&params.project_id, params.policy)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "source.retrieval.policy.get" => {
            match parse_params::<SourceRetrievalPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .get_source_retrieval_policy(&params.project_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "source.retrieval.policy.set" => {
            match parse_params::<SetSourceRetrievalPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_source_retrieval_policy(&params.project_id, params.policy)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => retrieval_policy_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "source.embedding.index_status" => {
            match parse_params::<SourceIndexStatusParams>(request.params) {
                Ok(params) => match state.engine.source_index_status(&params.project_id).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "source.embedding.index_rebuild" => {
            match parse_params::<SourceIndexRebuildParams>(request.params) {
                Ok(params) => match state.engine.rebuild_source_index(&params.project_id).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => source_index_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "scheduler.status" => match parse_params::<SchedulerStatusParams>(request.params) {
            Ok(params) => match state.agent.scheduler_status(params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "intake.list" => match parse_params::<IntakeListParams>(request.params) {
            Ok(params) => match state
                .engine
                .list_intake_tasks(params.project_id, params.status, params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "intake.create" => match parse_params::<IntakeCreateParams>(request.params) {
            Ok(params) => match state.engine.create_intake_task(params).await {
                Ok(task) => RpcResponse::success(id, task),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "intake.update" => match parse_params::<IntakeUpdateParams>(request.params) {
            Ok(params) => match state.engine.update_intake_task(params).await {
                Ok(task) => RpcResponse::success(id, task),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "intake.dispatch" => match parse_params::<IntakeTaskParams>(request.params) {
            Ok(params) => match state.agent.dispatch_intake_task(&params.id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "intake.cancel" => match parse_params::<IntakeTaskParams>(request.params) {
            Ok(params) => match state.agent.cancel_intake_task(&params.id).await {
                Ok(task) => RpcResponse::success(id, task),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "intake.retry" => match parse_params::<IntakeTaskParams>(request.params) {
            Ok(params) => match state.agent.retry_intake_task(&params.id).await {
                Ok(task) => RpcResponse::success(id, task),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "intake.delete" => match parse_params::<IntakeTaskParams>(request.params) {
            Ok(params) => match state.agent.delete_intake_task(&params.id).await {
                Ok(()) => RpcResponse::success(id, IntakeDeleteResult { deleted: true }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "program.list" => match parse_params::<ProgramListParams>(request.params) {
            Ok(params) => match state
                .engine
                .list_programs(params.project_id, params.status, params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "program.create" => match parse_params::<ProgramCreateParams>(request.params) {
            Ok(params) => match state.engine.create_program(params).await {
                Ok(program) => RpcResponse::success(id, ProgramResult { program }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "program.update" => match parse_params::<ProgramUpdateParams>(request.params) {
            Ok(params) => match state.engine.update_program(params).await {
                Ok(program) => RpcResponse::success(id, ProgramResult { program }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "program.pause" => match parse_params::<ProgramActionParams>(request.params) {
            Ok(params) => match state.engine.pause_program(params).await {
                Ok(program) => RpcResponse::success(id, ProgramResult { program }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "program.resume" => match parse_params::<ProgramActionParams>(request.params) {
            Ok(params) => match state.engine.resume_program(params).await {
                Ok(program) => RpcResponse::success(id, ProgramResult { program }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "program.cancel" => match parse_params::<ProgramActionParams>(request.params) {
            Ok(params) => {
                let reason = params
                    .reason
                    .unwrap_or_else(|| "cancelled from program.cancel".to_string());
                match state
                    .agent
                    .cancel_program(&params.program_id, &reason)
                    .await
                {
                    Ok(program) => RpcResponse::success(id, ProgramResult { program }),
                    Err(error) => internal_error(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "program.delete" => match parse_params::<ProgramDeleteParams>(request.params) {
            Ok(params) => match state.engine.delete_program(params).await {
                Ok(deleted) => RpcResponse::success(id, ProgramDeleteResult { deleted }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "program.status" => match parse_params::<ProgramStatusParams>(request.params) {
            Ok(params) => match state.engine.program_status(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "program.milestone.list" => match parse_params::<ProgramParams>(request.params) {
            Ok(params) => match state
                .engine
                .list_program_milestones(params.program_id)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "program.milestone.create" => {
            match parse_params::<ProgramMilestoneCreateParams>(request.params) {
                Ok(params) => match state.engine.create_program_milestone(params).await {
                    Ok(milestone) => RpcResponse::success(id, ProgramMilestoneResult { milestone }),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "program.milestone.update" => {
            match parse_params::<ProgramMilestoneUpdateParams>(request.params) {
                Ok(params) => match state.engine.update_program_milestone(params).await {
                    Ok(milestone) => RpcResponse::success(id, ProgramMilestoneResult { milestone }),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "program.milestone.retry" => {
            match parse_params::<ProgramMilestoneActionParams>(request.params) {
                Ok(params) => match state.engine.retry_program_milestone(params).await {
                    Ok(milestone) => RpcResponse::success(id, ProgramMilestoneResult { milestone }),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "program.milestone.skip" => {
            match parse_params::<ProgramMilestoneActionParams>(request.params) {
                Ok(params) => match state.engine.skip_program_milestone(params).await {
                    Ok(milestone) => RpcResponse::success(id, ProgramMilestoneResult { milestone }),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "program.milestone.start" => match parse_params::<ProgramMilestoneParams>(request.params) {
            Ok(params) => match state
                .agent
                .start_program_milestone(&params.milestone_id)
                .await
            {
                Ok(milestone) => RpcResponse::success(id, ProgramMilestoneResult { milestone }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "program.replay" => match parse_params::<ProgramReplayParams>(request.params) {
            Ok(params) => match state
                .engine
                .replay_program(&params.program_id, params.repair)
                .await
            {
                Ok(report) => RpcResponse::success(id, report),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "program.tick" => match parse_params::<ProgramTickParams>(request.params) {
            Ok(params) => match state
                .agent
                .reconcile_programs(params.project_id.as_deref())
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "task.queue" => match parse_params::<TaskQueueParams>(request.params) {
            Ok(params) => match state
                .engine
                .task_queue(params.project_id, params.session_id)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "task.dispatch" => match parse_params::<TaskDispatchParams>(request.params) {
            Ok(params) => match state.agent.dispatch_next_task(&params.session_id).await {
                Ok(started) => RpcResponse::success(id, TaskDispatchResult { started }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "task.retry" => match parse_params::<TaskActionParams>(request.params) {
            Ok(params) => match state
                .engine
                .retry_plan_task(&params.session_id, &params.task_id)
                .await
            {
                Ok(task) => RpcResponse::success(id, TaskActionResult { task }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "task.pause" => match parse_params::<TaskActionParams>(request.params) {
            Ok(params) => match state
                .engine
                .pause_plan_task(&params.session_id, &params.task_id)
                .await
            {
                Ok(task) => RpcResponse::success(id, TaskActionResult { task }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "task.resume" => match parse_params::<TaskActionParams>(request.params) {
            Ok(params) => match state
                .engine
                .resume_plan_task(&params.session_id, &params.task_id)
                .await
            {
                Ok(task) => RpcResponse::success(id, TaskActionResult { task }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "task.cancel" => match parse_params::<TaskActionParams>(request.params) {
            Ok(params) => match state
                .engine
                .request_plan_task_cancel(&params.session_id, &params.task_id)
                .await
            {
                Ok(task) => RpcResponse::success(id, TaskActionResult { task }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "task.skip" => match parse_params::<TaskActionParams>(request.params) {
            Ok(params) => match state
                .engine
                .skip_plan_task(&params.session_id, &params.task_id)
                .await
            {
                Ok(task) => RpcResponse::success(id, TaskActionResult { task }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "terminal.open" => match parse_params::<TerminalOpenParams>(request.params) {
            Ok(params) => match state
                .engine
                .terminal_open(
                    &params.session_id,
                    params.cols.unwrap_or(80),
                    params.rows.unwrap_or(24),
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "terminal.write" => match parse_params::<TerminalWriteParams>(request.params) {
            Ok(params) => match state
                .engine
                .terminal_write(&params.terminal_id, &params.data)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "terminal.read" => match parse_params::<TerminalReadParams>(request.params) {
            Ok(params) => match state
                .engine
                .terminal_read(&params.terminal_id, params.after_seq)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "terminal.resize" => match parse_params::<TerminalResizeParams>(request.params) {
            Ok(params) => match state
                .engine
                .terminal_resize(&params.terminal_id, params.cols, params.rows)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "terminal.close" => match parse_params::<TerminalCloseParams>(request.params) {
            Ok(params) => match state.engine.terminal_close(&params.terminal_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "console.open" => match parse_params::<ConsoleOpenParams>(request.params) {
            Ok(params) => match state.engine.console_open(&params.session_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "console.write" => match parse_params::<ConsoleWriteParams>(request.params) {
            Ok(params) => match state
                .engine
                .console_write(&params.console_id, &params.data)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "console.read" => match parse_params::<ConsoleReadParams>(request.params) {
            Ok(params) => match state
                .engine
                .console_read(&params.console_id, params.after_seq)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "console.signal" => match parse_params::<ConsoleSignalParams>(request.params) {
            Ok(params) => match state
                .engine
                .console_signal(&params.console_id, &params.signal)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "console.close" => match parse_params::<ConsoleCloseParams>(request.params) {
            Ok(params) => match state.engine.console_close(&params.console_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.processes" => match parse_params::<ProcessListParams>(request.params) {
            Ok(params) => match state
                .engine
                .session_processes(&params.session_id, params.include_finished, params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.process.logs" => match parse_params::<ProcessLogsParams>(request.params) {
            Ok(params) => match state
                .engine
                .session_process_logs(
                    &params.session_id,
                    &params.process_id,
                    params.offset,
                    params.max_bytes,
                    params.max_lines,
                    params.from_end,
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.process.stop" => match parse_params::<ProcessStopParams>(request.params) {
            Ok(params) => match state
                .engine
                .session_process_stop(
                    &params.session_id,
                    &params.process_id,
                    params.signal,
                    params.grace_secs,
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.process.restart" => match parse_params::<ProcessRestartParams>(request.params) {
            Ok(params) => match state
                .engine
                .session_process_restart(&params.session_id, &params.process_id, params.confirm)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "process.policy.get" => match parse_params::<ProcessPolicyParams>(request.params) {
            Ok(params) => match state.engine.get_process_policy(&params.project_id).await {
                Ok(policy) => RpcResponse::success(
                    id,
                    ProcessPolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "process.policy.set" => match parse_params::<SetProcessPolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_process_policy(&params.project_id, params.policy)
                .await
            {
                Ok(policy) => RpcResponse::success(
                    id,
                    ProcessPolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "git.log" => match parse_params::<GitLogParams>(request.params) {
            Ok(params) => match state
                .engine
                .git_log(&params.session_id, params.limit.unwrap_or(50))
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "git.show" => match parse_params::<GitShowParams>(request.params) {
            Ok(params) => match state
                .engine
                .git_show(&params.session_id, &params.commit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "workspace.tree" => match parse_params::<WorkspaceTreeParams>(request.params) {
            Ok(params) => match state.engine.workspace_tree(&params.session_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        // The historical `workspace.list` method is the file-browser listing.
        // The S4 agent workspace list reuses the same method name only when the
        // params carry an agent-shape selector; otherwise the file browser keeps
        // the exact legacy behavior. `agent.workspace.list` is the canonical
        // S4 name.
        "workspace.list"
            if request.params.get("project_id").is_some()
                || request.params.get("identity_id").is_some()
                || request.params.get("status").is_some() =>
        {
            match parse_params::<AgentWorkspaceListParams>(request.params) {
                Ok(params) => match state.engine.list_agent_workspaces(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => workspace_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "workspace.list" => match parse_params::<WorkspaceListParams>(request.params) {
            Ok(params) => match state
                .engine
                .workspace_list(
                    &params.session_id,
                    &params.path,
                    params.offset,
                    params.limit,
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "workspace.search" => match parse_params::<WorkspaceSearchParams>(request.params) {
            Ok(params) => match state
                .engine
                .workspace_search(&params.session_id, &params.query, params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "workspace.grep" => match parse_params::<WorkspaceGrepParams>(request.params) {
            Ok(params) => match state
                .engine
                .workspace_grep(
                    &params.session_id,
                    &params.query,
                    params.path,
                    params.case_sensitive,
                    params.max_results,
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "workspace.file" => match parse_params::<WorkspaceFileParams>(request.params) {
            Ok(params) => match state
                .engine
                .workspace_file(&params.session_id, &params.path)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "workspace.write" => match parse_params::<WorkspaceWriteParams>(request.params) {
            Ok(params) => match state
                .engine
                .workspace_write(
                    &params.session_id,
                    &params.path,
                    params.content,
                    params.expected_sha256,
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.budget.set" => match parse_params::<SetSessionUnlimitedParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_session_unlimited(&params.session_id, params.unlimited)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.priority.set" => match parse_params::<SetSessionPriorityParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_session_priority(&params.session_id, params.priority)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.list" => {
            let params: ListSessionsParams = if request.params.is_null() {
                ListSessionsParams { project_id: None }
            } else {
                match serde_json::from_value(request.params) {
                    Ok(params) => params,
                    Err(error) => {
                        return RpcResponse::error(
                            id,
                            RpcError::new(codes::INVALID_PARAMS, error.to_string()),
                        );
                    }
                }
            };
            match state.engine.list_sessions(params.project_id).await {
                Ok(sessions) => RpcResponse::success(id, SessionListResult { sessions }),
                Err(error) => internal_error(id, error),
            }
        }
        "event.list" => match parse_params::<ListEventsParams>(request.params) {
            Ok(params) => match state
                .engine
                .list_events(params.session_id, params.since_seq, params.limit)
                .await
            {
                Ok(events) => RpcResponse::success(id, EventListResult { events }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "skill.activations" => {
            match parse_params::<SessionSkillActivationsParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .session_skill_activations(&params.session_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.citation_audit" => {
            match parse_params::<SessionCitationAuditParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .session_citation_audit(&params.session_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.citations" => match parse_params::<SessionCitationsParams>(request.params) {
            Ok(params) => match state
                .engine
                .session_citations(&params.session_id, params.message_id)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.transcript" => match parse_params::<SessionTranscriptParams>(request.params) {
            Ok(params) => match state.engine.session_transcript(&params.session_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.search" => match parse_params::<SearchParams>(request.params) {
            Ok(params) => match state
                .engine
                .search(
                    &params.query,
                    params.project_id.as_deref(),
                    params.limit.unwrap_or(50),
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.info" => RpcResponse::success(id, state.agent.info()),
        "computer.status" => match parse_params::<ComputerStatusParams>(request.params) {
            Ok(params) => RpcResponse::success(
                id,
                state
                    .engine
                    .computer_status_for(params.session_id.as_deref(), params.refresh)
                    .await,
            ),
            Err(response) => response_with_id(id, response),
        },
        "gui.status" => match parse_params::<vp_protocol::types::GuiStatusParams>(request.params) {
            Ok(params) => match state.engine.gui_status(params.project_id.as_deref()).await {
                Ok(status) => RpcResponse::success(id, status),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "gui.timeline.audit" => match parse_params::<GuiTimelineAuditParams>(request.params) {
            Ok(params) => match state
                .engine
                .gui_timeline_audit(&params.project_id, params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "gui.timeline.retention" => {
            match parse_params::<GuiTimelineRetentionParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .gui_timeline_retention(&params.project_id, params.older_than_days)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "gui.timeline.prune" => match parse_params::<GuiTimelinePruneParams>(request.params) {
            Ok(params) if !params.dry_run.unwrap_or(false) && !params.confirm => {
                RpcResponse::error(
                    id,
                    vp_protocol::rpc::RpcError::new(
                        vp_protocol::rpc::codes::INVALID_PARAMS,
                        "gui.timeline.prune requires confirm=true for a non-dry-run prune",
                    ),
                )
            }
            Ok(params) => match state
                .engine
                .prune_gui_timeline(
                    &params.project_id,
                    params.older_than_days,
                    params.dry_run.unwrap_or(false),
                    params.limit,
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "daemon.agent.status" => match parse_params::<DaemonAgentStatusParams>(request.params) {
            Ok(_) => {
                let data_dir = state.data_dir.clone();
                let status = tokio::task::spawn_blocking(move || {
                    let paths =
                        vp_core::daemon_agent::DaemonAgentPaths::from_env(Some(data_dir), None)?;
                    let runner = vp_core::daemon_agent::SystemLaunchctl;
                    Ok::<_, anyhow::Error>(
                        vp_core::daemon_agent::DaemonAgent::new(paths, &runner).status(),
                    )
                })
                .await;
                match status {
                    Ok(Ok(status)) => {
                        RpcResponse::success(id, DaemonAgentStatusResult { ..status })
                    }
                    Ok(Err(error)) => internal_error(id, error),
                    Err(error) => internal_error(id, anyhow::anyhow!(error)),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "gui.timeline" => match parse_params::<GuiTimelineParams>(request.params) {
            Ok(params) => {
                let limit = params.limit.unwrap_or(100).clamp(1, 500);
                match state
                    .engine
                    .gui_timeline(params.session_id.as_deref(), limit)
                    .await
                {
                    Ok(actions) => RpcResponse::success(id, GuiTimelineResult { actions, limit }),
                    Err(error) => internal_error(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "gui.policy.get" => match parse_params::<GuiPolicyParams>(request.params) {
            Ok(params) => match state.engine.get_gui_policy(&params.project_id).await {
                Ok(policy) => RpcResponse::success(
                    id,
                    GuiPolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "gui.policy.set" => match parse_params::<SetGuiPolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_gui_policy(&params.project_id, params.policy)
                .await
            {
                Ok(policy) => RpcResponse::success(
                    id,
                    GuiPolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.probe" => match state.agent.probe().await {
            Ok(result) => RpcResponse::success(id, result),
            Err(error) => internal_error(id, error),
        },
        "session.discard" => match parse_params::<DiscardSessionParams>(request.params) {
            Ok(params) => match state.engine.discard_session(&params.session_id).await {
                Ok(()) => RpcResponse::success(id, json!({"discarded": true})),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.preempt" => match parse_params::<SessionPreemptParams>(request.params) {
            Ok(params) => match state
                .agent
                .preempt(&params.session_id, params.reason.as_deref())
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.preemption" => match parse_params::<SessionPreemptionParams>(request.params) {
            Ok(params) => match state
                .agent
                .preemption(&params.session_id, params.limit.unwrap_or(100))
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.cancel" => match parse_params::<CancelSessionParams>(request.params) {
            Ok(params) => RpcResponse::success(
                id,
                CancelSessionResult {
                    cancelled: state.agent.cancel(&params.session_id),
                },
            ),
            Err(response) => response_with_id(id, response),
        },
        "session.pause" => match parse_params::<SessionPauseParams>(request.params) {
            Ok(params) => match state.agent.pause(&params.session_id).await {
                Ok(paused) => RpcResponse::success(
                    id,
                    SessionPauseResult {
                        session_id: params.session_id,
                        paused,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.steer" => match parse_params::<SessionSteerParams>(request.params) {
            Ok(params) => match state
                .engine
                .steer_session(
                    &params.session_id,
                    &params.body,
                    params.agent_id.as_deref(),
                    params.task_id.as_deref(),
                    params.operation_id.as_deref(),
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.steering" => match parse_params::<SessionSteeringParams>(request.params) {
            Ok(params) => match state
                .engine
                .session_steering(&params.session_id, params.status.as_deref(), params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.steering.cancel" => {
            match parse_params::<SessionSteeringCancelParams>(request.params) {
                Ok(params) => match state.engine.cancel_steering(&params.id).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.defer" => match parse_params::<SessionDeferParams>(request.params) {
            Ok(params) => match state
                .engine
                .defer_session_watching_gate(
                    &params.session_id,
                    &params.body,
                    params.watch_task_ids,
                    params.watch_mode.as_deref(),
                    params.deadline_at.as_deref(),
                    params.after_secs,
                    params.max_attempts,
                    params.every_secs,
                    params.max_runs,
                    params.watch_gate_id.as_deref(),
                    params.operation_id.as_deref(),
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.continuations" => match parse_params::<SessionContinuationsParams>(request.params)
        {
            Ok(params) => match state
                .engine
                .session_continuations(&params.session_id, params.status.as_deref(), params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.continuation.cancel" => {
            match parse_params::<SessionContinuationCancelParams>(request.params) {
                Ok(params) => match state.engine.cancel_session_continuation(&params.id).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.continuation.status" => {
            match parse_params::<SessionContinuationStatusParams>(request.params) {
                Ok(params) => match state.engine.session_continuation_status(&params.id).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.continuation.reclaim" => {
            match parse_params::<SessionContinuationReclaimParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .reclaim_session_continuation(&params.id, params.reason.as_deref())
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.stalls" | "task.stalls" => {
            match parse_params::<SessionStallsParams>(request.params) {
                Ok(params) => match state.engine.session_stalls(&params.session_id).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.context" => match parse_params::<SessionContextParams>(request.params) {
            Ok(params) => match state
                .engine
                .session_context_status(&params.session_id)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.context.checkpoints" => {
            match parse_params::<ContextCheckpointListParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .list_context_checkpoints(&params.session_id, params.limit)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.context.checkpoint.detail" => {
            match parse_params::<ContextCheckpointDetailParams>(request.params) {
                Ok(params) => match state
                    .agent
                    .context_checkpoint_detail(&params.session_id, &params.checkpoint_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.context.checkpoint" => {
            match parse_params::<ContextCheckpointCreateParams>(request.params) {
                Ok(params) => match state
                    .agent
                    .context_checkpoint_now(&params.session_id, params.label)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.context.restore" => match parse_params::<ContextRestoreParams>(request.params) {
            Ok(params) => match state
                .agent
                .restore_context_checkpoint_now(
                    &params.session_id,
                    &params.checkpoint_id,
                    params.force,
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.context.checkpoint.delete" => {
            match parse_params::<ContextCheckpointDeleteParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .delete_context_checkpoint(&params.session_id, &params.checkpoint_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.context.checkpoint.prune" => {
            match parse_params::<ContextCheckpointPruneParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .prune_context_checkpoints(
                        &params.session_id,
                        params.older_than_days,
                        params.keep_latest,
                        params.dry_run,
                    )
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.context.policy" => {
            match parse_params::<ContextCheckpointPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .get_context_checkpoint_policy(&params.project_id)
                    .await
                {
                    Ok(policy) => RpcResponse::success(
                        id,
                        vp_protocol::types::ContextCheckpointPolicyResult {
                            project_id: params.project_id,
                            policy,
                        },
                    ),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.context.policy.set" => {
            match parse_params::<SetContextCheckpointPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_context_checkpoint_policy(&params.project_id, &params.policy)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.instructions" => match parse_params::<SessionInstructionsParams>(request.params) {
            Ok(params) => match state
                .engine
                .session_instructions(&params.session_id, params.agent_id.as_deref())
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.prompt_layout.set" => {
            match parse_params::<SetSessionPromptLayoutParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_session_prompt_layout(&params.session_id, params.prompt_layout)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "project.prompt_layout.set" => {
            match parse_params::<SetProjectPromptLayoutParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_project_prompt_layout(&params.project_id, params.prompt_layout)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.cache" => match parse_params::<SessionCacheParams>(request.params) {
            Ok(params) => match state.engine.session_cache_status(&params.session_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.cache.epoch" => match parse_params::<SessionCacheEpochParams>(request.params) {
            Ok(params) => match state
                .engine
                .session_cache_epoch(&params.session_id, params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.cache.calls" => match parse_params::<SessionCacheCallsParams>(request.params) {
            Ok(params) => match state.engine.session_cache_calls(&params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.cache_epochs" => match parse_params::<ProjectCacheEpochParams>(request.params) {
            Ok(params) => match state
                .engine
                .project_cache_epochs(&params.project_id, params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.cache_epochs.prune" => {
            match parse_params::<ProjectCacheEpochPruneParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .prune_project_cache_epochs(
                        &params.project_id,
                        params.older_than_days,
                        params.keep_epochs,
                        params.dry_run,
                    )
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.cache.audit" => match parse_params::<SessionCacheAuditParams>(request.params) {
            Ok(params) => match state.engine.session_cache_audit(&params.session_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.cache.breaks" => match parse_params::<SessionCacheBreaksParams>(request.params) {
            Ok(params) => match state
                .engine
                .session_cache_breaks(&params.session_id, params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.cache.plan" => match parse_params::<SessionCachePlanParams>(request.params) {
            Ok(params) => match state
                .agent
                .cache_plan_now(&params.session_id, &params.roles)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "tool_bundle.catalog" => RpcResponse::success(id, state.agent.tool_bundle_catalog()),
        "tool_bundle.matrix" => match parse_params::<ToolBundleMatrixParams>(request.params) {
            Ok(params) => match state.agent.tool_bundle_matrix(&params.task_kinds) {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "tool_bundle.diff" => match parse_params::<ToolBundleDiffParams>(request.params) {
            Ok(params) => match state.agent.tool_bundle_diff(
                &params.from_bundle,
                &params.to_bundle,
                params.role.as_deref(),
                params.profile.as_deref(),
                params.task_kind.as_deref(),
            ) {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.tool_bundle.history" => {
            match parse_params::<ToolBundleHistoryParams>(request.params) {
                Ok(params) => match state
                    .agent
                    .tool_bundle_history(&params.session_id, params.role.as_deref(), params.limit)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.tool_bundle" => match parse_params::<ToolBundleParams>(request.params) {
            Ok(params) => match state
                .agent
                .tool_bundle_now(
                    &params.session_id,
                    &params.roles,
                    params.task_kind.as_deref(),
                    params.include_tools.unwrap_or(true),
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.cache.warm" => match parse_params::<SessionCacheWarmParams>(request.params) {
            Ok(params) => match state
                .agent
                .warm_cache_now(&params.session_id, params.role.as_deref())
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.compact" => match parse_params::<SessionCompactParams>(request.params) {
            Ok(params) => match state.agent.compact_now(&params.session_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.checkpoint" => match parse_params::<SessionCheckpointParams>(request.params) {
            Ok(params) => match state
                .agent
                .checkpoint_now(&params.session_id, params.label)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.checkpoints" => {
            match parse_params::<SessionCheckpointListParams>(request.params) {
                Ok(params) => match state.engine.list_checkpoints(&params.session_id).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.checkpoint.stats" => {
            match parse_params::<SessionCheckpointStatsParams>(request.params) {
                Ok(params) => match state.engine.checkpoint_stats(&params.session_id).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.checkpoint.diff" => {
            match parse_params::<SessionCheckpointDiffParams>(request.params) {
                Ok(params) => match state
                    .agent
                    .diff_checkpoint_now(&params.session_id, &params.checkpoint_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.checkpoint.restore" => {
            match parse_params::<SessionCheckpointRestoreParams>(request.params) {
                Ok(params) => match state
                    .agent
                    .restore_checkpoint_now(&params.session_id, &params.checkpoint_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.checkpoint.delete" => {
            match parse_params::<SessionCheckpointDeleteParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .delete_checkpoint(&params.session_id, &params.checkpoint_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.checkpoint.prune" => {
            match parse_params::<SessionCheckpointPruneParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .prune_checkpoints(
                        &params.session_id,
                        params.older_than_days,
                        params.keep_latest,
                        params.dry_run,
                    )
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.fork.tree" => match parse_params::<SessionForkTreeParams>(request.params) {
            Ok(params) => match state.engine.session_fork_tree(&params.session_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.lineage" => match parse_params::<SessionLineageParams>(request.params) {
            Ok(params) => match state.engine.session_lineage(&params.session_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.compare" => match parse_params::<SessionCompareParams>(request.params) {
            Ok(params) => match state
                .engine
                .compare_sessions(&params.session_id, &params.other_session_id)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.fork.merge" => match parse_params::<SessionMergeForkParams>(request.params) {
            Ok(params) => match state
                .agent
                .merge_fork_now(&params.source_session_id, &params.fork_session_id)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.fork" => match parse_params::<SessionForkParams>(request.params) {
            Ok(params) => match state
                .agent
                .fork_now(
                    &params.session_id,
                    params.checkpoint_id.as_deref(),
                    params.goal,
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.plan_resume" => match parse_params::<SessionPlanResumeParams>(request.params) {
            Ok(params) => match state.agent.resume_plan(&params.session_id).await {
                Ok(started) => RpcResponse::success(
                    id,
                    SessionPlanResumeResult {
                        session_id: params.session_id,
                        started,
                    },
                ),
                Err(error) => runtime_pin_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.resume" => match parse_params::<SessionResumeParams>(request.params) {
            Ok(params) => match state.agent.resume(&params.session_id).await {
                Ok(resumed) => RpcResponse::success(
                    id,
                    SessionResumeResult {
                        session_id: params.session_id,
                        resumed,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.runs" => match parse_params::<SessionRunsParams>(request.params) {
            Ok(params) => match state
                .engine
                .session_runs(&params.session_id, params.limit.unwrap_or(50))
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.run.detail" => match parse_params::<SessionRunDetailParams>(request.params) {
            Ok(params) => match state.engine.session_run_detail(&params.run_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.compatibility" => {
            match parse_params::<SessionCompatibilityParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .session_compatibility(&params.session_id, params.run_id.as_deref())
                    .await
                {
                    Ok(report) => RpcResponse::success(id, report),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.diagnosis" => match parse_params::<SessionDiagnosisParams>(request.params) {
            Ok(params) => match state.engine.session_diagnosis(params).await {
                Ok(report) => RpcResponse::success(id, report),
                Err(error) => diagnosis_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.diagnosis.record" => {
            match parse_params::<SessionDiagnosisRecordParams>(request.params) {
                Ok(params) => match state.engine.record_session_diagnosis(params).await {
                    Ok(report) => RpcResponse::success(id, report),
                    Err(error) => diagnosis_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.attributions" => match parse_params::<SessionAttributionsParams>(request.params) {
            Ok(params) => match state.engine.session_attributions(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => diagnosis_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "plan.migrate" => match parse_params::<PlanMigrateParams>(request.params) {
            Ok(params) => match state.engine.migrate_plan(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => runtime_pin_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.run.resume" => match parse_params::<SessionRunResumeParams>(request.params) {
            Ok(params) => match state
                .agent
                .resume_session_run(
                    &params.session_id,
                    params.run_id.as_deref(),
                    params.operation_id.as_deref(),
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => runtime_pin_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.run.cancel" => match parse_params::<SessionRunCancelParams>(request.params) {
            Ok(params) => match state
                .agent
                .cancel_session_run(&params.session_id, params.run_id.as_deref())
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.run.policy.get" => match parse_params::<SessionRunPolicyParams>(request.params) {
            Ok(params) => match state.engine.session_run_policy(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.run.policy.set" => {
            match parse_params::<SetSessionRunPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_session_run_policy(&params.project_id, params.policy)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.commit" => match parse_params::<CommitSessionParams>(request.params) {
            Ok(params) => match state
                .engine
                .commit_session(&params.session_id, &params.message)
                .await
            {
                Ok(commit) => RpcResponse::success(id, commit),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "team_plan.get" => match parse_params::<TeamPlanParams>(request.params) {
            Ok(params) => match state.engine.get_team_plan(&params.session_id).await {
                Ok(plan) => RpcResponse::success(id, TeamPlanResult { plan }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.todos" => match parse_params::<SessionTodosParams>(request.params) {
            Ok(params) => match state.engine.list_todos(&params.session_id).await {
                Ok(items) => RpcResponse::success(
                    id,
                    SessionTodosResult {
                        session_id: params.session_id,
                        items,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.usage" => match parse_params::<ProjectUsageParams>(request.params) {
            Ok(params) => match state.engine.project_usage_report(&params.project_id).await {
                Ok(report) => RpcResponse::success(id, report),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.cache" => match parse_params::<ProjectCacheParams>(request.params) {
            Ok(params) => match state.engine.project_cache_overview(&params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.budget.get" => match parse_params::<ProjectBudgetParams>(request.params) {
            Ok(params) => match state.engine.get_project_budget(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.budget.set" => match parse_params::<SetProjectBudgetParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_project_budget(&params.project_id, params.budget)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.cleanup_worktrees" => {
            match parse_params::<CleanupWorktreesParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .cleanup_worktrees(
                        &params.project_id,
                        params.older_than_secs,
                        params.dry_run,
                        params.force,
                        params.delete_branches,
                        params.keep_failed,
                    )
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "project.worktree_policy.get" => match parse_params::<WorktreePolicyParams>(request.params)
        {
            Ok(params) => match state.engine.get_worktree_policy(&params.project_id).await {
                Ok(policy) => RpcResponse::success(
                    id,
                    vp_protocol::types::WorktreePolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.worktree_policy.set" => {
            match parse_params::<SetWorktreePolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_worktree_policy(&params.project_id, &params.policy)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "project.checkpoint_policy.get" => {
            match parse_params::<CheckpointPolicyParams>(request.params) {
                Ok(params) => match state.engine.get_checkpoint_policy(&params.project_id).await {
                    Ok(policy) => RpcResponse::success(
                        id,
                        vp_protocol::types::CheckpointPolicyResult {
                            project_id: params.project_id,
                            policy,
                        },
                    ),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "project.checkpoint_policy.set" => {
            match parse_params::<SetCheckpointPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_checkpoint_policy(&params.project_id, &params.policy)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "model.preset.list" => match state.engine.list_model_presets().await {
            Ok(result) => RpcResponse::success(id, result),
            Err(error) => internal_error(id, error),
        },
        "model.preset.create" => match parse_params::<CreateModelPresetParams>(request.params) {
            Ok(params) => match state.engine.create_model_preset(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "model.preset.update" => match parse_params::<UpdateModelPresetParams>(request.params) {
            Ok(params) => match state.engine.update_model_preset(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "model.preset.delete" => match parse_params::<DeleteModelPresetParams>(request.params) {
            Ok(params) => match state.engine.delete_model_preset(&params.id).await {
                Ok(removed) => RpcResponse::success(
                    id,
                    vp_protocol::types::DeleteModelPresetResult { removed },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "update.download" => match parse_params::<UpdateDownloadParams>(request.params) {
            Ok(params) => match vp_core::update::download_update(params.repo.as_deref()).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "update.check" => match parse_params::<UpdateCheckParams>(request.params) {
            Ok(params) => match vp_core::update::check_for_update(params.repo.as_deref()).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "backup.status" => {
            match parse_params::<vp_protocol::types::BackupStatusParams>(request.params) {
                Ok(_) => match backup_status(&state.data_dir) {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "dashboard.snapshot" => match parse_params::<DashboardParams>(request.params) {
            Ok(params) => match state
                .engine
                .dashboard_snapshot(params.project_id.as_deref())
                .await
            {
                Ok(mut result) => {
                    result.summary.agent = Some(state.agent.info());
                    RpcResponse::success(id, result)
                }
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.fork.forest" => match parse_params::<ProjectForkForestParams>(request.params) {
            Ok(params) => match state.engine.project_fork_forest(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.fork.audit" => match parse_params::<ForkAuditParams>(request.params) {
            Ok(params) => match state.engine.audit_project_forks(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.fork.repair" => match parse_params::<ForkRepairParams>(request.params) {
            Ok(params) => match state.engine.repair_project_forks(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.log_status" => match parse_params::<SessionLogParams>(request.params) {
            Ok(params) => match state.engine.session_log_status(&params.session_id).await {
                Ok(status) => RpcResponse::success(id, status),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.restore_log" => match parse_params::<SessionLogParams>(request.params) {
            Ok(params) => match state.engine.restore_session_log(&params.session_id).await {
                Ok(status) => RpcResponse::success(id, status),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.tool_activity" => match parse_params::<ToolActivityParams>(request.params) {
            Ok(params) => match state
                .engine
                .session_tool_activity(&params.session_id, params.limit.unwrap_or(50))
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.usage" => match parse_params::<UsageLedgerParams>(request.params) {
            Ok(params) => match state.engine.usage_ledger(&params.session_id).await {
                Ok(ledger) => RpcResponse::success(id, ledger),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.usage_csv" => match parse_params::<UsageLedgerParams>(request.params) {
            Ok(params) => match state.engine.usage_csv(&params.session_id).await {
                Ok(csv) => RpcResponse::success(id, csv),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.diff" => match parse_params::<SessionDiffParams>(request.params) {
            Ok(params) => match state.engine.session_diff(&params.session_id).await {
                Ok(diff) => RpcResponse::success(id, diff),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.list" => match parse_params::<AgentListParams>(request.params) {
            Ok(params) => match state.engine.list_agents(&params.session_id).await {
                Ok(agents) => RpcResponse::success(id, AgentListResult { agents }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.identities.list" => match parse_params::<AgentIdentityListParams>(request.params) {
            Ok(params) => match state
                .engine
                .list_agent_identities_for_project(
                    &params.project_id,
                    params.role.as_deref(),
                    params.status.as_deref(),
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => identity_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.identities.get" => match parse_params::<AgentIdentityParams>(request.params) {
            Ok(params) => match state.engine.get_agent_identity(&params.identity_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => identity_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.org.snapshot" => match parse_params::<AgentOrgSnapshotParams>(request.params) {
            Ok(params) => match state.engine.agent_org_snapshot(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => identity_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.identity.detail" => {
            match parse_params::<AgentIdentityDetailParams>(request.params) {
                Ok(params) => match state.engine.agent_identity_detail(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => identity_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "team.template.export" => match parse_params::<TeamTemplateExportParams>(request.params) {
            Ok(params) => match state.engine.export_team_template(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => identity_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "team.template.preview" => match parse_params::<TeamTemplateImportParams>(request.params) {
            Ok(params) => {
                let known_models = template_known_models(state).await;
                match state
                    .engine
                    .preview_team_template(params, known_models)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => identity_error(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "team.template.import" => match parse_params::<TeamTemplateImportParams>(request.params) {
            Ok(params) => {
                let known_models = template_known_models(state).await;
                match state
                    .engine
                    .import_team_template(params, known_models)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => identity_error(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "team.template.list" => match parse_params::<TeamTemplateListParams>(request.params) {
            Ok(params) => match state
                .engine
                .list_team_templates(params.project_id, params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => identity_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "team.template.get" => match parse_params::<TeamTemplateGetParams>(request.params) {
            Ok(params) => match state
                .engine
                .get_team_template(&params.template_id, params.project_id.as_deref())
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => identity_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "team.template.diff" => match parse_params::<TeamTemplateDiffParams>(request.params) {
            Ok(params) => {
                let known_models = template_known_models(state).await;
                match state
                    .engine
                    .diff_team_template(&params.project_id, &params.template_id, known_models)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => identity_error(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "team.template.replay" => match parse_params::<TeamTemplateReplayParams>(request.params) {
            Ok(params) => match state
                .engine
                .replay_team_templates(&params.project_id, params.repair)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => identity_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.identity.create" => {
            match parse_params::<CreateAgentIdentityParams>(request.params) {
                Ok(params) => {
                    // S6 hires Managers/Workers through the approval-aware
                    // surface. The Leader bootstrap remains the S1 explicit
                    // creation path; there is exactly one live Leader.
                    if !params.role.trim().eq_ignore_ascii_case("leader") {
                        RpcResponse::error(
                            id,
                            RpcError::with_data(
                                codes::INVALID_PARAMS,
                                "use agent.hire for Manager/Worker identities; agent.identity.create is the Leader bootstrap path",
                                json!({"code": vp_protocol::types::agent_identity_codes::HIRE_INVALID}),
                            ),
                        )
                    } else {
                        match state.engine.create_agent_identity(params).await {
                            Ok(result) => RpcResponse::success(id, result),
                            Err(error) => identity_error(id, error),
                        }
                    }
                }
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.identity.update" => {
            match parse_params::<UpdateAgentIdentityParams>(request.params) {
                Ok(params) => {
                    // Role/department/component/parent changes are promotion or
                    // transfer and must go through their approval-classified
                    // S6 operations. The S1 update surface remains for bounded
                    // metadata, profile/model, and successor bookkeeping.
                    if params.role.is_some()
                        || params.department.is_some()
                        || params.component.is_some()
                        || params.parent_identity_id.is_some()
                    {
                        RpcResponse::error(
                            id,
                            RpcError::with_data(
                                codes::INVALID_PARAMS,
                                "role/department/component/parent changes require agent.promote or agent.transfer approval",
                                json!({"code": vp_protocol::types::agent_identity_codes::IDENTITY_APPROVAL_REQUIRED}),
                            ),
                        )
                    } else {
                        match state.engine.update_agent_identity(params).await {
                            Ok(result) => RpcResponse::success(id, result),
                            Err(error) => identity_error(id, error),
                        }
                    }
                }
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.identity.pause" => match parse_params::<PauseAgentIdentityParams>(request.params) {
            Ok(params) => match state.engine.pause_agent_identity(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => identity_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.identity.resume" => {
            match parse_params::<ResumeAgentIdentityParams>(request.params) {
                Ok(params) => match state.engine.resume_agent_identity(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => identity_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.identity.retire" => {
            // Legacy alias. It now follows S6 approval policy and fails closed
            // unless the project has explicitly disabled retirement approval.
            match parse_params::<RetireAgentIdentityParams>(request.params) {
                Ok(params) => {
                    match state
                        .engine
                        .retire_agent(AgentRetireParams {
                            identity_id: params.identity_id,
                            successor_id: params.successor_id,
                            approved: false,
                            ..AgentRetireParams::default()
                        })
                        .await
                    {
                        Ok(result) => RpcResponse::success(
                            id,
                            vp_protocol::types::AgentIdentityResult {
                                identity: result.identity,
                                changed: result.changed,
                            },
                        ),
                        Err(error) => identity_error(id, error),
                    }
                }
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.identities.replay" => {
            match parse_params::<AgentIdentityReplayParams>(request.params) {
                Ok(params) => match state.engine.replay_agent_identities(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => identity_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.hire" => match parse_params::<AgentHireParams>(request.params) {
            Ok(params) => match state.engine.hire_agent_identity(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => identity_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.hire.approve" => match parse_params::<AgentHireApproveParams>(request.params) {
            Ok(params) => match state.engine.approve_agent_hire(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => identity_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.hire.reject" => match parse_params::<AgentHireRejectParams>(request.params) {
            Ok(params) => match state.engine.reject_agent_hire(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => identity_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.hire.list" => match parse_params::<AgentHireListParams>(request.params) {
            Ok(params) => match state.engine.list_agent_hire_requests(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => identity_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.promote" => match parse_params::<AgentPromoteParams>(request.params) {
            Ok(params) => match state.engine.promote_agent_identity(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => identity_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.transfer" => match parse_params::<AgentTransferParams>(request.params) {
            Ok(params) => match state.engine.transfer_agent_identity(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => identity_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.retire" => match parse_params::<AgentRetireParams>(request.params) {
            Ok(params) => match state.engine.retire_agent(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => identity_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.retirement.propose" => {
            match parse_params::<AgentRetirementProposeParams>(request.params) {
                Ok(params) => match state.engine.propose_agent_retirement(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => identity_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.retirement.approve" => {
            match parse_params::<AgentRetirementProposalParams>(request.params) {
                Ok(params) => match state.engine.approve_agent_retirement_proposal(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => identity_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.retirement.reject" => {
            match parse_params::<AgentRetirementRejectParams>(request.params) {
                Ok(params) => match state.engine.reject_agent_retirement_proposal(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => identity_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.retirement.proposals" => {
            match parse_params::<AgentRetirementProposalListParams>(request.params) {
                Ok(params) => match state.engine.agent_retirement_proposals(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => identity_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.retirement.candidates" => {
            match parse_params::<AgentRetirementCandidatesParams>(request.params) {
                Ok(params) => match state.engine.agent_retirement_candidates(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => identity_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.performance" => match parse_params::<AgentPerformanceParams>(request.params) {
            Ok(params) => match state.engine.agent_performance(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => identity_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.performance.summary" => {
            match parse_params::<AgentPerformanceSummaryParams>(request.params) {
                Ok(params) => match state.engine.agent_performance_summary(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => identity_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.performance.replay" => {
            match parse_params::<AgentPerformanceReplayParams>(request.params) {
                Ok(params) => match state.engine.agent_performance_replay(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => identity_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.employment.replay" => {
            match parse_params::<AgentEmploymentReplayParams>(request.params) {
                Ok(params) => match state.engine.replay_agent_employment(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => identity_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "project.retirement_policy.get" => {
            match parse_params::<AgentRetirementPolicyParams>(request.params) {
                Ok(params) => {
                    match state
                        .engine
                        .get_agent_retirement_policy(&params.project_id)
                        .await
                    {
                        Ok(result) => RpcResponse::success(id, result),
                        Err(error) => identity_error(id, error),
                    }
                }
                Err(response) => response_with_id(id, response),
            }
        }
        "project.retirement_policy.set" => {
            match parse_params::<vp_protocol::types::SetAgentRetirementPolicyParams>(request.params)
            {
                Ok(params) => match state.engine.set_agent_retirement_policy(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => identity_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.memory.list" => match parse_params::<AgentMemoryListParams>(request.params) {
            Ok(params) => match state.engine.list_agent_memories(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => memory_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.memory.get" => match parse_params::<AgentMemoryParams>(request.params) {
            Ok(params) => match state.engine.get_agent_memory(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => memory_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.memory.remember" => match parse_params::<CreateAgentMemoryParams>(request.params) {
            Ok(params) => match state.engine.remember_agent_memory(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => memory_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.memory.update" => match parse_params::<UpdateAgentMemoryParams>(request.params) {
            Ok(params) => match state.engine.update_agent_memory(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => memory_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.memory.supersede" => {
            match parse_params::<SupersedeAgentMemoryParams>(request.params) {
                Ok(params) => match state.engine.supersede_agent_memory(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => memory_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.memory.forget" => match parse_params::<ForgetAgentMemoryParams>(request.params) {
            Ok(params) => match state.engine.forget_agent_memory(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => memory_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.memory.verify" => match parse_params::<VerifyAgentMemoryParams>(request.params) {
            Ok(params) => match state.engine.verify_agent_memory(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => memory_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.memory.sweep" => match parse_params::<SweepAgentMemoriesParams>(request.params) {
            Ok(params) => match state.engine.sweep_agent_memories(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => memory_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.memory.replay" => match parse_params::<AgentMemoryReplayParams>(request.params) {
            Ok(params) => match state.engine.replay_agent_memories(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => memory_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.assignments" => match parse_params::<SessionAssignmentsParams>(request.params) {
            Ok(params) => match state.engine.session_assignments(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => coordination_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.assign" => match parse_params::<SessionAssignParams>(request.params) {
            Ok(params) => match state.engine.assign_session_identity(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => coordination_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.assignment.renew" => {
            match parse_params::<SessionAssignmentRenewParams>(request.params) {
                Ok(params) => match state.engine.renew_session_assignment(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => coordination_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.release" => match parse_params::<SessionReleaseParams>(request.params) {
            Ok(params) => match state.engine.release_session_identity(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => coordination_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.inbox.list" => match parse_params::<AgentInboxListParams>(request.params) {
            Ok(params) => match state.engine.list_agent_inbox(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => coordination_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.inbox.send" => match parse_params::<AgentInboxSendParams>(request.params) {
            Ok(params) => match state.engine.send_agent_inbox(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => coordination_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.inbox.read" => match parse_params::<AgentInboxMessageParams>(request.params) {
            Ok(params) => match state.engine.read_agent_inbox(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => coordination_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.inbox.ack" => match parse_params::<AgentInboxMessageParams>(request.params) {
            Ok(params) => match state.engine.ack_agent_inbox(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => coordination_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.coordination.replay" => {
            match parse_params::<AgentCoordinationReplayParams>(request.params) {
                Ok(params) => match state.engine.replay_agent_coordination(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => coordination_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.workspace.list" => match parse_params::<AgentWorkspaceListParams>(request.params) {
            Ok(params) => match state.engine.list_agent_workspaces(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => workspace_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.workspace.get" | "workspace.get" => {
            match parse_params::<AgentWorkspaceParams>(request.params) {
                Ok(params) => match state.engine.get_agent_workspace(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => workspace_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.workspace.ensure" | "workspace.ensure" => {
            match parse_params::<AgentWorkspaceEnsureParams>(request.params) {
                Ok(params) => match state.engine.ensure_agent_workspace(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => workspace_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.workspace.land" | "workspace.land" => {
            match parse_params::<AgentWorkspaceLandParams>(request.params) {
                Ok(params) => match state.engine.land_agent_workspace(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => workspace_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.workspace.archive" | "workspace.archive" => {
            match parse_params::<AgentWorkspaceArchiveParams>(request.params) {
                Ok(params) => match state.engine.archive_agent_workspace(params, false).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => workspace_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.workspace.retire" | "workspace.retire" => {
            match parse_params::<AgentWorkspaceRetireParams>(request.params) {
                Ok(params) => match state.engine.retire_agent_workspace(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => workspace_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "agent.workspace.replay" | "workspace.replay" => {
            match parse_params::<AgentWorkspaceReplayParams>(request.params) {
                Ok(params) => match state.engine.replay_agent_workspaces(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => workspace_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.workspace" => match parse_params::<SessionWorkspaceParams>(request.params) {
            Ok(params) => match state.engine.session_workspace(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => workspace_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "project.agent_identity_policy.get" => {
            match parse_params::<AgentIdentityPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .get_agent_identity_policy(&params.project_id)
                    .await
                {
                    Ok(policy) => RpcResponse::success(
                        id,
                        AgentIdentityPolicyResult {
                            project_id: params.project_id,
                            policy,
                        },
                    ),
                    Err(error) => identity_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "project.agent_identity_policy.set" => {
            match parse_params::<SetAgentIdentityPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_agent_identity_policy(&params.project_id, params.policy)
                    .await
                {
                    Ok(policy) => RpcResponse::success(
                        id,
                        AgentIdentityPolicyResult {
                            project_id: params.project_id,
                            policy,
                        },
                    ),
                    Err(error) => identity_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "mailbox.list" => match parse_params::<MailboxListParams>(request.params) {
            Ok(params) => match state
                .engine
                .list_agent_messages(&params.session_id, params.agent_id, params.unread_only)
                .await
            {
                Ok(messages) => RpcResponse::success(id, MailboxListResult { messages }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mailbox.read" => match parse_params::<MarkMessageReadParams>(request.params) {
            Ok(params) => match state
                .engine
                .mark_agent_message_read(&params.message_id)
                .await
            {
                Ok(Some(message)) => RpcResponse::success(id, AgentMessageResult { message }),
                Ok(None) => RpcResponse::error(
                    id,
                    RpcError::new(codes::INVALID_PARAMS, "message not found".to_string()),
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.server.list" => match state.engine.list_mcp_servers().await {
            Ok(result) => RpcResponse::success(id, result),
            Err(error) => internal_error(id, error),
        },
        "mcp.server.add" => match parse_params::<AddMcpServerParams>(request.params) {
            Ok(params) => match state.engine.add_mcp_server(params.config).await {
                Ok(server) => RpcResponse::success(id, json!({"server": server})),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.server.update" => match parse_params::<UpdateMcpServerParams>(request.params) {
            Ok(params) => match state
                .engine
                .update_mcp_server(&params.id, params.config)
                .await
            {
                Ok(Some(server)) => RpcResponse::success(id, json!({"server": server})),
                Ok(None) => RpcResponse::error(
                    id,
                    RpcError::new(codes::INVALID_PARAMS, "MCP server not found"),
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.server.probe" => match parse_params::<McpProbeParams>(request.params) {
            Ok(params) => match state.engine.probe_mcp_server(&params.server_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.server.remove" => match parse_params::<McpServerIdParams>(request.params) {
            Ok(params) => match state.engine.remove_mcp_server(&params.server_id).await {
                Ok(removed) => RpcResponse::success(id, json!({"removed": removed})),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "acp.agent.list" => match state.engine.list_acp_agents().await {
            Ok(result) => RpcResponse::success(id, result),
            Err(error) => internal_error(id, error),
        },
        "acp.agent.add" => match parse_params::<AddAcpAgentParams>(request.params) {
            Ok(params) => match state
                .engine
                .add_acp_agent_config(AcpAgentConfig {
                    name: params.name,
                    command: params.command,
                    args: params.args,
                    env: params.env,
                    enabled: params.enabled,
                    auto_approve: params.auto_approve,
                    timeout_secs: params.timeout_secs,
                    transport: params.transport,
                    endpoint: params.endpoint,
                    auth_header_env: params.auth_header_env,
                    headers: params.headers,
                    connect_timeout_secs: params.connect_timeout_secs,
                    allow_remote: params.allow_remote,
                })
                .await
            {
                Ok(agent) => RpcResponse::success(id, json!({"agent": agent})),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "acp.agent.update" => match parse_params::<UpdateAcpAgentParams>(request.params) {
            Ok(params) => match state
                .engine
                .update_acp_agent_config(
                    &params.id,
                    AcpAgentConfig {
                        name: params.name,
                        command: params.command,
                        args: params.args,
                        env: params.env,
                        enabled: params.enabled,
                        auto_approve: params.auto_approve,
                        timeout_secs: params.timeout_secs,
                        transport: params.transport,
                        endpoint: params.endpoint,
                        auth_header_env: params.auth_header_env,
                        headers: params.headers,
                        connect_timeout_secs: params.connect_timeout_secs,
                        allow_remote: params.allow_remote,
                    },
                )
                .await
            {
                Ok(Some(agent)) => RpcResponse::success(id, json!({"agent": agent})),
                Ok(None) => RpcResponse::error(
                    id,
                    RpcError::new(codes::INVALID_PARAMS, "ACP agent not found".to_string()),
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "acp.agent.remove" => match parse_params::<AcpAgentIdParams>(request.params) {
            Ok(params) => match state.engine.remove_acp_agent(&params.agent_id).await {
                Ok(removed) => RpcResponse::success(id, json!({"removed": removed})),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "acp.agent.probe" => match parse_params::<AcpProbeParams>(request.params) {
            Ok(params) => match state
                .engine
                .probe_acp_agent_options(&params.agent_id, params.project_id, params.create_session)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.tools" => match parse_params::<McpServerIdParams>(request.params) {
            Ok(params) => match state.engine.mcp_tools(&params.server_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.call" => match parse_params::<McpCallParams>(request.params) {
            Ok(params) => match state
                .engine
                .mcp_call(&params.server_id, &params.tool, params.arguments)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.resources" => match parse_params::<McpServerIdParams>(request.params) {
            Ok(params) => match state.engine.mcp_resources(&params.server_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.resource.read" => match parse_params::<McpReadResourceParams>(request.params) {
            Ok(params) => match state
                .engine
                .mcp_read_resource(&params.server_id, &params.uri)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.prompts" => match parse_params::<McpServerIdParams>(request.params) {
            Ok(params) => match state.engine.mcp_prompts(&params.server_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.prompt.get" => match parse_params::<McpGetPromptParams>(request.params) {
            Ok(params) => match state
                .engine
                .mcp_prompt(&params.server_id, &params.name, params.arguments)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "question.list" => match parse_params::<QuestionListParams>(request.params) {
            Ok(params) => match state.engine.list_questions(params.session_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "question.answer" => match parse_params::<AnswerQuestionParams>(request.params) {
            Ok(params) => match state
                .engine
                .answer_question(&params.question_id, &params.answer)
                .await
            {
                Ok(answered) => RpcResponse::success(id, AnswerQuestionResult { answered }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.list" => match parse_params::<MemoryListParams>(request.params) {
            Ok(params) => match state
                .engine
                .list_memories_filtered(
                    &params.project_id,
                    params.query,
                    params.kind,
                    params.status,
                    params.include_stale,
                    params.limit.unwrap_or(100),
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.search" => match parse_params::<MemorySearchParams>(request.params) {
            Ok(params) => match state.engine.memory_search(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.brief" => match parse_params::<MemoryBriefParams>(request.params) {
            Ok(params) => match state
                .engine
                .memory_brief(&params.session_id, params.task_id.as_deref())
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.update" => match parse_params::<MemoryUpdateParams>(request.params) {
            Ok(params) => match state
                .engine
                .memory_update(
                    &params.id,
                    params.status.as_deref(),
                    params.pinned,
                    params.tags,
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.verify" => match parse_params::<MemoryVerifyParams>(request.params) {
            Ok(params) => match state.engine.memory_verify(&params.id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.sweep" => match parse_params::<MemorySweepParams>(request.params) {
            Ok(params) => match state
                .engine
                .memory_sweep(&params.project_id, params.older_than_days, params.dry_run)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.stats" => match parse_params::<MemoryStatsParams>(request.params) {
            Ok(params) => match state.engine.memory_stats(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.conflicts" => match parse_params::<MemoryConflictsParams>(request.params) {
            Ok(params) => match state
                .engine
                .memory_conflicts(&params.project_id, params.status.as_deref(), params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.conflict.scan" => match parse_params::<MemoryConflictScanParams>(request.params) {
            Ok(params) => match state.engine.memory_conflict_scan(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.conflict.resolve" => {
            match parse_params::<MemoryConflictResolveParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .memory_conflict_resolve(&params.id, &params.action, params.note.as_deref())
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "memory.health" => match parse_params::<MemoryHealthParams>(request.params) {
            Ok(params) => match state.engine.memory_health(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.embeddings.status" => {
            match parse_params::<MemoryEmbeddingsStatusParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .memory_embedding_status(&params.project_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "memory.embeddings.rebuild" => {
            match parse_params::<MemoryEmbeddingsRebuildParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .memory_embedding_rebuild(&params.project_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "memory.embeddings.test" => {
            match parse_params::<MemoryEmbeddingsTestParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .memory_embedding_test(&params.project_id, params.text.as_deref())
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "memory.embeddings.index.status" => {
            match parse_params::<MemoryEmbeddingsIndexStatusParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .memory_embedding_index_status(&params.project_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "memory.embeddings.index.rebuild" => {
            match parse_params::<MemoryEmbeddingsIndexRebuildParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .memory_embedding_index_rebuild(&params.project_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "memory.create" => match parse_params::<MemoryCreateParams>(request.params) {
            Ok(params) => match state
                .engine
                .create_memory(
                    &params.session_id,
                    &params.kind,
                    &params.title,
                    &params.body,
                    params.tags,
                )
                .await
            {
                Ok(memory) => RpcResponse::success(id, MemoryCreateResult { memory }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.delete" => match parse_params::<MemoryDeleteParams>(request.params) {
            Ok(params) => match state.engine.delete_memory(&params.id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.prune" => match parse_params::<MemoryPruneParams>(request.params) {
            Ok(params) => match state
                .engine
                .prune_memories(
                    &params.project_id,
                    params.older_than_days,
                    params.keep_latest,
                    params.dry_run,
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.policy.get" => match parse_params::<MemoryPolicyParams>(request.params) {
            Ok(params) => match state.engine.get_memory_policy(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.policy.set" => match parse_params::<SetMemoryPolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_memory_policy(&params.project_id, params.policy)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "artifact.list" => match parse_params::<ArtifactListParams>(request.params) {
            Ok(params) => match state.engine.list_artifacts(&params.session_id).await {
                Ok(artifacts) => RpcResponse::success(id, ArtifactListResult { artifacts }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mailbox.policy.get" => match parse_params::<MailboxPolicyParams>(request.params) {
            Ok(params) => match state.engine.get_mailbox_policy(&params.project_id).await {
                Ok(policy) => RpcResponse::success(
                    id,
                    MailboxPolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mailbox.policy.set" => match parse_params::<SetMailboxPolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_mailbox_policy(&params.project_id, params.policy)
                .await
            {
                Ok(policy) => RpcResponse::success(
                    id,
                    MailboxPolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mailbox.gc" => match parse_params::<MailboxGcParams>(request.params) {
            Ok(params) => {
                let policy = match state.engine.get_mailbox_policy(&params.project_id).await {
                    Ok(policy) => policy,
                    Err(error) => return internal_error(id, error),
                };
                match state.engine.gc_mailbox(&params.project_id, &policy).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "artifact.backfill" => match parse_params::<ArtifactBackfillParams>(request.params) {
            Ok(params) => {
                match state
                    .engine
                    .backfill_artifact_events(&params.session_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "artifact.materialize" => match parse_params::<ArtifactMaterializeParams>(request.params) {
            Ok(params) => match state.engine.materialize_artifact(&params.artifact_id).await {
                Ok(Some(materialized)) => RpcResponse::success(id, materialized),
                Ok(None) => RpcResponse::error(
                    id,
                    RpcError::new(codes::INVALID_PARAMS, "artifact is not a file artifact"),
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "tool_output.policy.get" => match parse_params::<ToolOutputPolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .get_tool_output_policy(&params.project_id)
                .await
            {
                Ok(policy) => RpcResponse::success(
                    id,
                    ToolOutputPolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "tool_output.policy.set" => {
            match parse_params::<SetToolOutputPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_tool_output_policy(&params.project_id, params.policy)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "tool_output.status" => match parse_params::<ToolOutputStatusParams>(request.params) {
            Ok(params) => match state.engine.tool_output_status(params).await {
                Ok(status) => RpcResponse::success(id, status),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "tool_output.list" => match parse_params::<ToolOutputListParams>(request.params) {
            Ok(params) => match state.engine.list_tool_outputs(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "tool_output.prune" => match parse_params::<ToolOutputPruneParams>(request.params) {
            Ok(params) => match state.engine.prune_tool_outputs(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "tool_output.audit" => match parse_params::<ToolOutputAuditParams>(request.params) {
            Ok(params) => match state.engine.audit_tool_outputs(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "test_cache.scope.preview" => {
            match parse_params::<TestCacheScopePreviewParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .preview_test_cache_scopes(&params.project_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "test_cache.policy.get" => match parse_params::<TestCachePolicyParams>(request.params) {
            Ok(params) => match state.engine.get_test_cache_policy(&params.project_id).await {
                Ok(policy) => RpcResponse::success(
                    id,
                    vp_protocol::test_cache::TestCachePolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "test_cache.policy.set" => match parse_params::<SetTestCachePolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_test_cache_policy(&params.project_id, params.policy)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "test_cache.stats" => match parse_params::<TestCacheStatsParams>(request.params) {
            Ok(params) => match state.engine.test_cache_stats(&params.project_id).await {
                Ok(stats) => RpcResponse::success(id, stats),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "test_cache.entries" => match parse_params::<TestCacheEntriesParams>(request.params) {
            Ok(params) => match state.engine.list_test_cache_entries(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "test_cache.prune" => match parse_params::<TestCachePruneParams>(request.params) {
            Ok(params) => match state.engine.prune_test_cache(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "test_cache.clear" => match parse_params::<TestCacheClearParams>(request.params) {
            Ok(params) => match state.engine.clear_test_cache(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "test_cache.impact" => match parse_params::<TestCacheImpactParams>(request.params) {
            Ok(params) => match state.engine.test_cache_impact(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "artifact.policy.get" => match parse_params::<ArtifactPolicyParams>(request.params) {
            Ok(params) => match state.engine.get_artifact_policy(&params.project_id).await {
                Ok(policy) => RpcResponse::success(
                    id,
                    ArtifactPolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "artifact.policy.set" => match parse_params::<SetArtifactPolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_artifact_policy(&params.project_id, params.policy)
                .await
            {
                Ok(policy) => RpcResponse::success(
                    id,
                    ArtifactPolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "artifact.gc" => match parse_params::<ArtifactGcParams>(request.params) {
            Ok(params) => {
                let policy = match state.engine.get_artifact_policy(&params.project_id).await {
                    Ok(policy) => policy,
                    Err(error) => return internal_error(id, error),
                };
                match state.engine.gc_artifacts(&params.project_id, &policy).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "artifact.get" => match parse_params::<ArtifactGetParams>(request.params) {
            Ok(params) => match state.engine.get_artifact(&params.artifact_id).await {
                Ok(Some(artifact)) => {
                    let provenance = artifact.provenance.clone();
                    RpcResponse::success(
                        id,
                        ArtifactResult {
                            artifact,
                            provenance,
                        },
                    )
                }
                Ok(None) => RpcResponse::error(
                    id,
                    RpcError::new(codes::INVALID_PARAMS, "artifact not found".to_string()),
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "artifact.archive.list" => {
            match parse_params::<ArtifactArchiveListParams>(request.params) {
                Ok(params) => match state.engine.artifact_archive_list(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => archive_media_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "artifact.archive.extract" => {
            match parse_params::<ArtifactArchiveExtractParams>(request.params) {
                Ok(params) => match state.engine.artifact_archive_extract(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => archive_media_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "media.info" => match parse_params::<MediaInfoParams>(request.params) {
            Ok(params) => match state.engine.media_info(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => archive_media_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "media.transcode" => match parse_params::<MediaTranscodeParams>(request.params) {
            Ok(params) => match state.engine.media_transcode(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => archive_media_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "merge_queue.list" => match parse_params::<MergeQueueListParams>(request.params) {
            Ok(params) => match state.engine.list_merge_queue(&params.session_id).await {
                Ok(entries) => RpcResponse::success(id, MergeQueueListResult { entries }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "merge_queue.retry" => match parse_params::<RetryMergeParams>(request.params) {
            Ok(params) => match state.agent.retry_merge(&params.entry_id).await {
                Ok(entry) => RpcResponse::success(id, RetryMergeResult { entry }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "merge_queue.resolve" => match parse_params::<ResolveMergeParams>(request.params) {
            Ok(params) => match state.agent.resolve_merge(&params.entry_id).await {
                Ok(entry) => RpcResponse::success(id, ResolveMergeResult { entry }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.land" => match parse_params::<LandSessionParams>(request.params) {
            Ok(params) => match state.engine.land_session(&params.session_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.pr" => match parse_params::<OpenPullRequestParams>(request.params) {
            Ok(params) => match state
                .engine
                .open_pull_request_with_provider(&params.session_id, params.provider.as_deref())
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => forge_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "forge.status" => match parse_params::<ForgeStatusParams>(request.params) {
            Ok(params) => match state.engine.forge_status(params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "forge.preflight" => match parse_params::<ForgePreflightParams>(request.params) {
            Ok(params) => match state
                .engine
                .forge_preflight(&params.project_id, params.provider.as_deref())
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => forge_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.export" => match parse_params::<SessionExportParams>(request.params) {
            Ok(params) => match state.engine.export_session(&params.session_id).await {
                Ok(bundle) => RpcResponse::success(id, bundle),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.import" => match parse_params::<SessionImportParams>(request.params) {
            Ok(params) => match state.engine.import_session(params.bundle).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.rehydrate" => match parse_params::<RehydrateSessionParams>(request.params) {
            Ok(params) => match state
                .engine
                .rehydrate_session(&params.session_id, params.worktree)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.replay" => match parse_params::<ReplaySessionParams>(request.params) {
            Ok(params) => {
                let result = if params.rebuild {
                    state.engine.rebuild_session(&params.session_id).await
                } else if params.repair {
                    state.engine.repair_session(&params.session_id).await
                } else {
                    state.engine.replay_session(&params.session_id).await
                };
                match result {
                    Ok(report) => RpcResponse::success(id, report),
                    Err(error) => internal_error(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "agent.configure" => match parse_params::<ConfigureAgentParams>(request.params) {
            Ok(params) => match state.agent.configure(params) {
                Ok(info) => {
                    let agent = state.agent.clone();
                    tokio::spawn(async move {
                        let _ = agent.resume_queued_sessions().await;
                        agent.auto_resolve_all().await;
                    });
                    RpcResponse::success(id, info)
                }
                Err(error) => {
                    RpcResponse::error(id, RpcError::new(codes::INVALID_PARAMS, error.to_string()))
                }
            },
            Err(response) => response_with_id(id, response),
        },
        "session.prompt" => match parse_params::<PromptParams>(request.params) {
            Ok(params) => match state
                .agent
                .start_with_operation(
                    &params.session_id,
                    &params.content,
                    params.operation_id.as_deref(),
                )
                .await
            {
                Ok(()) => RpcResponse::success(id, PromptResult { accepted: true }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "approval.grants" => match parse_params::<ApprovalGrantListParams>(request.params) {
            Ok(params) => match state.engine.list_approval_grants(&params.project_id).await {
                Ok(grants) => RpcResponse::success(id, ApprovalGrantListResult { grants }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "approval.grant.create" => {
            match parse_params::<ApprovalGrantCreateParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .create_approval_grant(
                        &params.project_id,
                        &params.tool,
                        &params.path_prefix,
                        params.include_children,
                        "user",
                    )
                    .await
                {
                    Ok(grant) => RpcResponse::success(id, ApprovalGrantCreateResult { grant }),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "approval.grant.delete" => {
            match parse_params::<ApprovalGrantDeleteParams>(request.params) {
                Ok(params) => match state.engine.delete_approval_grant(&params.id).await {
                    Ok(deleted) => RpcResponse::success(id, ApprovalGrantDeleteResult { deleted }),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "approval.list" => {
            let params: ListApprovalsParams = if request.params.is_null() {
                ListApprovalsParams { session_id: None }
            } else {
                match serde_json::from_value(request.params) {
                    Ok(params) => params,
                    Err(error) => {
                        return RpcResponse::error(
                            id,
                            RpcError::new(codes::INVALID_PARAMS, error.to_string()),
                        );
                    }
                }
            };
            match state.engine.list_approvals(params.session_id).await {
                Ok(approvals) => RpcResponse::success(id, ApprovalListResult { approvals }),
                Err(error) => internal_error(id, error),
            }
        }
        "approval.resolve" => match parse_params::<ResolveApprovalParams>(request.params) {
            Ok(params) => match state
                .engine
                .resolve_approval(&params.approval_id, params.approved)
                .await
            {
                Ok(resolved) => RpcResponse::success(id, ResolveApprovalResult { resolved }),
                Err(error) => approval_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "approval.queue" => match parse_params::<ApprovalQueueParams>(request.params) {
            Ok(params) => match state
                .engine
                .approval_queue(params.session_id, params.project_id)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => approval_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "approval.batch.resolve" => {
            match parse_params::<ApprovalBatchResolveParams>(request.params) {
                Ok(params) => {
                    let decisions = params
                        .decisions
                        .into_iter()
                        .map(|decision| (decision.approval_id, decision.approved))
                        .collect::<Vec<_>>();
                    match state.engine.resolve_approvals(decisions).await {
                        Ok(result) => RpcResponse::success(
                            id,
                            ApprovalBatchResolveResult {
                                results: result.results,
                                resolved_count: result.resolved_count,
                                failed_count: result.failed_count,
                            },
                        ),
                        Err(error) => approval_error(id, error),
                    }
                }
                Err(response) => response_with_id(id, response),
            }
        }
        "approval.templates" | "approval.template.list" => {
            match parse_params::<ApprovalTemplateListParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .list_approval_templates(&params.project_id)
                    .await
                {
                    Ok(templates) => {
                        RpcResponse::success(id, ApprovalTemplateListResult { templates })
                    }
                    Err(error) => approval_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "approval.template.create" => {
            match parse_params::<ApprovalTemplateCreateParams>(request.params) {
                Ok(params) => match state.engine.create_approval_template(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => approval_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "approval.template.update" => {
            match parse_params::<ApprovalTemplateUpdateParams>(request.params) {
                Ok(params) => match state.engine.update_approval_template(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => approval_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "approval.template.delete" => {
            match parse_params::<ApprovalTemplateDeleteParams>(request.params) {
                Ok(params) => match state.engine.delete_approval_template(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => approval_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "approval.template.from_approval" => {
            match parse_params::<ApprovalTemplateFromApprovalParams>(request.params) {
                Ok(params) => match state.engine.approval_template_from_approval(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => approval_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "approval.policy" => match parse_params::<AttentionPolicyGetParams>(request.params) {
            Ok(params) => match state.engine.get_attention_policy(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => approval_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "approval.policy.set" => match parse_params::<AttentionPolicySetParams>(request.params) {
            Ok(params) => match state.engine.set_attention_policy(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => approval_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "approval.audit" => match parse_params::<ApprovalAuditParams>(request.params) {
            Ok(params) => {
                match state
                    .engine
                    .approval_audit(&params.project_id, params.limit)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => approval_error(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "event.subscribe" => RpcResponse::success(id, json!({"subscribed": true})),
        "auth" => RpcResponse::success(id, json!({"ok": true})),
        _ => RpcResponse::error(
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

/// Read-only backup signing status. Never returns private key material.
fn backup_status(data_dir: &Path) -> Result<vp_protocol::types::BackupStatusResult> {
    backup_status_in(data_dir, &vp_core::backup::signing::default_key_dir())
}

/// Testable status reader with an explicit key directory.
fn backup_status_in(
    data_dir: &Path,
    key_dir: &Path,
) -> Result<vp_protocol::types::BackupStatusResult> {
    use vp_core::backup::anchor::AnchorStore;
    use vp_core::backup::signing::Keyring;
    use vp_protocol::types::{BackupAnchorStatus, BackupSigningKeyStatus, BackupStatusResult};

    let anchor = AnchorStore::new(data_dir).summary()?;
    let keyring_path = key_dir.join("keyring.json");
    let keyring = Keyring::load_optional(key_dir)?;
    let (keyring_present, active_key_id, keys) = match &keyring {
        Some(keyring) => {
            let summary = keyring.summary();
            (
                true,
                summary.active_key_id.clone(),
                summary
                    .keys
                    .into_iter()
                    .map(|key| BackupSigningKeyStatus {
                        key_id: key.key_id,
                        algorithm: key.algorithm,
                        public_key_b64: key.public_key_b64,
                        created_at: key.created_at,
                        retired_at: key.retired_at,
                        revoked_at: key.revoked_at,
                        active: key.active,
                    })
                    .collect(),
            )
        }
        None => (false, None, Vec::new()),
    };
    let imported_repositories = anchor.imported_repositories;
    let repositories = anchor
        .repositories
        .into_iter()
        .map(|entry| BackupAnchorStatus {
            repository_id: entry.repository_id,
            repository_path: entry.repository_path,
            highest_generation: entry.highest_generation,
            last_verified_generation: entry.last_verified_generation,
            last_snapshot_id: entry.last_snapshot_id,
            last_operation: entry.last_operation,
            first_seen_at: entry.first_seen_at,
            last_verified_at: entry.last_verified_at,
            origin: entry.origin,
        })
        .collect();
    Ok(BackupStatusResult {
        anchor_path: anchor.path,
        anchor_present: anchor.present,
        anchor_updated_at: anchor.updated_at,
        repositories,
        keyring_path: keyring_path.display().to_string(),
        keyring_present,
        active_key_id,
        key_count: keys.len(),
        revoked_key_count: keys.iter().filter(|key| key.revoked_at.is_some()).count(),
        keys,
        signed_repository_formats: vec![3],
        portable_anchor_format_version: vp_core::backup::anchor::PORTABLE_ANCHOR_FORMAT_VERSION,
        imported_repositories,
        note: "Backup signing, verification, rekey, and restore are CLI operations. Revoked keys verify only with --allow-revoked. The local anchor is not tamper-proof against this OS user or root and is not remote attestation, KMS, escrow, or a signed timestamp.".to_string(),
    })
}

fn parse_params<T: serde::de::DeserializeOwned>(params: Value) -> std::result::Result<T, RpcError> {
    serde_json::from_value(params)
        .map_err(|error| RpcError::new(codes::INVALID_PARAMS, error.to_string()))
}

fn response_with_id(id: Option<Value>, error: RpcError) -> RpcResponse {
    RpcResponse::error(id, error)
}

fn internal_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    RpcResponse::error(id, RpcError::new(codes::INTERNAL_ERROR, error.to_string()))
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

/// Map the stable identity error taxonomy to a bounded RPC failure. Message
/// text never changes the classification; only the typed code does.
fn identity_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    let typed = error.downcast_ref::<vp_protocol::types::AgentIdentityError>();
    let Some(typed) = typed else {
        return RpcResponse::error(
            id,
            RpcError::new(codes::INTERNAL_ERROR, format!("{error:#}")),
        );
    };
    let rpc_code = match typed.code {
        vp_protocol::types::agent_identity_codes::IDENTITY_NOT_FOUND
        | vp_protocol::types::agent_identity_codes::HIRE_NOT_FOUND
        | vp_protocol::types::agent_identity_codes::RETIREMENT_NOT_FOUND
        | vp_protocol::types::agent_identity_codes::ORG_PROJECT_NOT_FOUND
        | vp_protocol::types::team_template_codes::TEMPLATE_NOT_FOUND
        | vp_protocol::types::team_template_codes::TEMPLATE_PROJECT_NOT_FOUND => codes::NOT_FOUND,
        vp_protocol::types::agent_identity_codes::IDENTITY_BUSY
        | vp_protocol::types::agent_identity_codes::IDENTITY_RETIRED
        | vp_protocol::types::agent_identity_codes::IDENTITY_CONFLICT
        | vp_protocol::types::agent_identity_codes::HIRE_CONFLICT
        | vp_protocol::types::agent_identity_codes::HIRE_ALREADY_DECIDED
        | vp_protocol::types::agent_identity_codes::PROMOTION_CONFLICT
        | vp_protocol::types::agent_identity_codes::TRANSFER_CONFLICT
        | vp_protocol::types::agent_identity_codes::RETIREMENT_CONFLICT
        | vp_protocol::types::agent_identity_codes::RETIREMENT_COOLDOWN
        | vp_protocol::types::agent_identity_codes::RETIREMENT_NOT_QUIESCENT
        | vp_protocol::types::agent_identity_codes::RETIREMENT_NOT_ELIGIBLE
        | vp_protocol::types::team_template_codes::TEMPLATE_CONFLICT
        | vp_protocol::types::team_template_codes::TEMPLATE_APPLY_CONFLICT => codes::CONFLICT,
        _ => codes::INVALID_PARAMS,
    };
    RpcResponse::error(
        id,
        RpcError::with_data(rpc_code, typed.message.clone(), json!({"code": typed.code})),
    )
}

/// Map the stable agent-memory taxonomy to a bounded RPC failure. Message text
/// never changes the classification; only the typed code does.
fn memory_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    if let Some(typed) = error.downcast_ref::<vp_protocol::types::AgentMemoryError>() {
        let rpc_code = match typed.code {
            vp_protocol::types::agent_memory_codes::MEMORY_NOT_FOUND => codes::NOT_FOUND,
            vp_protocol::types::agent_memory_codes::MEMORY_CONFLICT
            | vp_protocol::types::agent_memory_codes::MEMORY_IMPORTED_READONLY
            | vp_protocol::types::agent_memory_codes::MEMORY_IDENTITY_RETIRED => codes::CONFLICT,
            _ => codes::INVALID_PARAMS,
        };
        return RpcResponse::error(
            id,
            RpcError::with_data(rpc_code, typed.message.clone(), json!({"code": typed.code})),
        );
    }
    RpcResponse::error(
        id,
        RpcError::new(codes::INTERNAL_ERROR, format!("{error:#}")),
    )
}

/// Map the stable workspace taxonomy to a bounded RPC failure. The typed code is
/// carried in `data.code`; message text is never used for classification.
fn workspace_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    if let Some(typed) = error.downcast_ref::<vp_protocol::types::AgentWorkspaceError>() {
        let rpc_code = match typed.code {
            vp_protocol::types::agent_workspace_codes::WORKSPACE_NOT_FOUND => codes::NOT_FOUND,
            vp_protocol::types::agent_workspace_codes::WORKSPACE_BUSY
            | vp_protocol::types::agent_workspace_codes::WORKSPACE_CONFLICT
            | vp_protocol::types::agent_workspace_codes::WORKSPACE_NOT_ACTIVE
            | vp_protocol::types::agent_workspace_codes::WORKSPACE_NOT_QUIESCENT
            | vp_protocol::types::agent_workspace_codes::WORKSPACE_NOT_ACCEPTED => codes::CONFLICT,
            _ => codes::INVALID_PARAMS,
        };
        return RpcResponse::error(
            id,
            RpcError::with_data(rpc_code, typed.message.clone(), json!({"code": typed.code})),
        );
    }
    if let Some(typed) = error.downcast_ref::<vp_protocol::types::AgentIdentityError>() {
        let rpc_code = match typed.code {
            vp_protocol::types::agent_identity_codes::IDENTITY_NOT_FOUND => codes::NOT_FOUND,
            _ => codes::INVALID_PARAMS,
        };
        return RpcResponse::error(
            id,
            RpcError::with_data(rpc_code, typed.message.clone(), json!({"code": typed.code})),
        );
    }
    RpcResponse::error(
        id,
        RpcError::new(codes::INTERNAL_ERROR, format!("{error:#}")),
    )
}

/// Map the stable assignment/inbox taxonomy to a bounded RPC failure. It also
/// accepts the S1 identity taxonomy so a missing identity or read-only import
/// keeps its existing wire code.
fn coordination_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    if let Some(typed) = error.downcast_ref::<vp_protocol::types::AgentCoordinationError>() {
        let rpc_code = match typed.code {
            vp_protocol::types::agent_coordination_codes::ASSIGNMENT_NOT_FOUND
            | vp_protocol::types::agent_coordination_codes::INBOX_NOT_FOUND => codes::NOT_FOUND,
            _ => codes::INVALID_PARAMS,
        };
        return RpcResponse::error(
            id,
            RpcError::with_data(rpc_code, typed.message.clone(), json!({"code": typed.code})),
        );
    }
    if let Some(typed) = error.downcast_ref::<vp_protocol::types::AgentIdentityError>() {
        let rpc_code = match typed.code {
            vp_protocol::types::agent_identity_codes::IDENTITY_NOT_FOUND => codes::NOT_FOUND,
            _ => codes::INVALID_PARAMS,
        };
        return RpcResponse::error(
            id,
            RpcError::with_data(rpc_code, typed.message.clone(), json!({"code": typed.code})),
        );
    }
    RpcResponse::error(
        id,
        RpcError::new(codes::INTERNAL_ERROR, format!("{error:#}")),
    )
}

/// Runtime pin/migration failures carry a stable Q3 code, retryability, hint,
/// and the bounded compatibility report. The message already includes
/// `error[CODE]`.
fn runtime_pin_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    if let Some(pin) = error.downcast_ref::<vp_core::runtime_pin::RuntimePinError>() {
        return RpcResponse::error(
            id,
            RpcError::with_data(codes::INTERNAL_ERROR, pin.to_string(), pin.data()),
        );
    }
    internal_error(id, error)
}

/// Diagnosis failures use the Q3 `{code,message,retryable,hint}` shape. They
/// never include prompt text, tool output, command text, or secrets.
fn diagnosis_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    if let Some(diagnosis) = error.downcast_ref::<vp_core::diagnosis::DiagnosisError>() {
        let rpc_code = match diagnosis.code {
            vp_core::diagnosis::codes::DIAGNOSIS_SESSION_NOT_FOUND => codes::NOT_FOUND,
            _ => codes::INVALID_PARAMS,
        };
        return RpcResponse::error(
            id,
            RpcError::with_data(rpc_code, diagnosis.to_string(), diagnosis.data()),
        );
    }
    internal_error(id, error)
}

/// OTel export failures carry a stable `OTEL_*` code and bounded data. They
/// never include a collector credential or session content.
fn otel_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    if let Some(otel) = error.downcast_ref::<vp_core::otel::OtelError>() {
        return RpcResponse::error(
            id,
            RpcError::with_data(codes::INTERNAL_ERROR, otel.to_string(), otel.data()),
        );
    }
    internal_error(id, error)
}

/// Forge failures carry the Q3 `FORGE_*` code, retryability, hint, and safe
/// evidence in RPC error data. The message already starts with `error[CODE]`.
fn forge_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    if let Some(forge) = error.downcast_ref::<vp_core::forge::ForgeError>() {
        return RpcResponse::error(
            id,
            RpcError::with_data(codes::INTERNAL_ERROR, forge.to_string(), forge.data()),
        );
    }
    internal_error(id, error)
}

/// Retrieval policy validation uses the Q3 error catalog. The numeric fields
/// are clamped by the engine, so a failure here means a manifest or hand-edited
/// value is outside the supported range.
fn retrieval_policy_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    let code = vp_core::tooling::errors::ToolErrorCode::SOURCE_RETRIEVAL_POLICY_INVALID;
    RpcResponse::error(
        id,
        RpcError::with_data(
            codes::INVALID_PARAMS,
            format!("error[{}] {}", code.as_str(), error),
            serde_json::json!({
                "code": code.as_str(),
                "retryable": code.default_retryable(),
                "hint": code.default_hint(),
            }),
        ),
    )
}

/// A derived-index rebuild failure is retryable but never silently swallowed.
fn source_index_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    let code = vp_core::tooling::errors::ToolErrorCode::SOURCE_INDEX_FAILED;
    RpcResponse::error(
        id,
        RpcError::with_data(
            codes::INTERNAL_ERROR,
            format!("error[{}] {}", code.as_str(), error),
            serde_json::json!({
                "code": code.as_str(),
                "retryable": code.default_retryable(),
                "hint": code.default_hint(),
            }),
        ),
    )
}

/// Approval rule failures are typed with the Q3 catalog. Invalid template or
/// policy input is a client error; everything else stays an internal error.
fn approval_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    if let Some(rule) = error.downcast_ref::<vp_core::approvals::ApprovalRuleError>() {
        return RpcResponse::error(
            id,
            RpcError::with_data(codes::INVALID_PARAMS, rule.to_string(), rule.data()),
        );
    }
    RpcResponse::error(id, RpcError::new(codes::INVALID_PARAMS, error.to_string()))
}

/// Expire stale pending approvals for projects with attention enabled. The
/// sweep never approves; it denies the waiting agents and emits
/// `approval.expired` for each row.
fn spawn_approval_sweeper(engine: Engine) {
    let seconds = std::env::var("VERLINPARK_APPROVAL_SWEEP_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(30);
    if seconds == 0 {
        return;
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(seconds));
        loop {
            interval.tick().await;
            match engine.sweep_expired_approvals().await {
                Ok(0) => {}
                Ok(expired) => tracing::info!(expired, "approval attention sweep"),
                Err(error) => tracing::warn!(%error, "approval attention sweep failed"),
            }
        }
    });
}

fn spawn_gui_timeline_retention_sweeper(engine: Engine) {
    let interval_secs = std::env::var("VERLINPARK_GUI_RETENTION_SWEEP_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value >= 60)
        .unwrap_or(6 * 3_600);
    let batch = std::env::var("VERLINPARK_GUI_RETENTION_BATCH")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(1_000)
        .clamp(1, 10_000);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
        ticker.tick().await;
        loop {
            ticker.tick().await;
            match engine.sweep_gui_timeline_retention(batch).await {
                Ok(results) if !results.is_empty() => {
                    let pruned = results.iter().map(|result| result.pruned).sum::<u64>();
                    let skipped = results
                        .iter()
                        .map(|result| result.skipped_events)
                        .sum::<u64>();
                    tracing::info!(
                        projects = results.len(),
                        pruned,
                        skipped,
                        "GUI timeline retention sweep"
                    );
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(%error, "GUI timeline retention sweep failed"),
            }
        }
    });
}

/// Move stale effect `intent` rows to `unknown` after the project grace
/// period. The first tick runs immediately so a daemon restart cannot leave a
/// crash-between-intent-and-finish row silently retryable.
fn spawn_effect_sweeper(engine: Engine) {
    let seconds = std::env::var("VERLINPARK_EFFECT_SWEEP_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(30)
        .clamp(5, 3_600);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(seconds));
        loop {
            interval.tick().await;
            match engine.sweep_stale_effect_intents(100).await {
                Ok(0) => {}
                Ok(swept) => tracing::warn!(
                    swept,
                    "moved stale effect intents to unknown; reconcile before retrying or merging"
                ),
                Err(error) => tracing::warn!(%error, "effect intent sweep failed"),
            }
        }
    });
}

/// Expire open external gates whose deadline passed. Expiry is terminal and
/// wakes the watching continuation; it never resolves an approval or retries
/// an external action. The first tick runs immediately so a restart catches up.
fn spawn_gate_sweeper(engine: Engine) {
    let seconds = std::env::var("VERLINPARK_GATE_SWEEP_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(30);
    if seconds == 0 {
        return;
    }
    let seconds = seconds.clamp(5, 3_600);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(seconds));
        loop {
            interval.tick().await;
            match engine.sweep_expired_gates(100).await {
                Ok(0) => {}
                Ok(expired) => {
                    tracing::info!(
                        expired,
                        "expired external gates and woke their continuations"
                    );
                }
                Err(error) => tracing::warn!(%error, "external gate expiry sweep failed"),
            }
        }
    });
}

/// Assignment leases and inbox TTLs are project-scoped, so one bounded sweep
/// covers every project. It never reassigns an active identity and never
/// removes an inbox row; expiry is a status transition with one event.
fn spawn_coordination_sweeper(engine: Engine) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(30));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            match engine.reconcile_session_agent_assignments().await {
                Ok(expired) if expired > 0 => {
                    tracing::info!(expired, "identity lease expiry sweep");
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(%error, "identity lease expiry sweep failed"),
            }
            match engine.sweep_agent_inbox_expiry().await {
                Ok(expired) if expired > 0 => {
                    tracing::info!(expired, "project inbox expiry sweep");
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(%error, "project inbox expiry sweep failed"),
            }
            match engine.sweep_agent_workspaces(Some(1_000)).await {
                Ok(released) if released > 0 => {
                    tracing::info!(released, "Worker workspace lease sweep");
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(%error, "Worker workspace lease sweep failed"),
            }
        }
    });
}

fn spawn_process_sweeper(engine: Engine) {
    let seconds = std::env::var("VERLINPARK_PROCESS_SWEEP_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(60);
    if seconds == 0 {
        return;
    }
    let idle_enabled = std::env::var("VERLINPARK_PROCESS_IDLE_SWEEP_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|value| value > 0)
        .unwrap_or(true);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(seconds));
        interval.tick().await;
        loop {
            interval.tick().await;
            match engine.sweep_supervised_processes(idle_enabled).await {
                Ok(report)
                    if report.idle_stopped > 0
                        || report.pruned > 0
                        || report.lost > 0
                        || report.unknown > 0 =>
                {
                    tracing::info!(
                        idle_stopped = report.idle_stopped,
                        lost = report.lost,
                        unknown = report.unknown,
                        pruned = report.pruned,
                        "supervised-process sweep"
                    );
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(%error, "supervised-process sweep failed"),
            }
        }
    });
}

/// Remote integration failures carry the Q3 tool-error shape in `data` so the
/// CLI and Swift can show a stable code, retryability, and hint. Other errors
/// remain ordinary internal errors and never invent a code.
fn remote_bootstrap_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    match vp_core::remote_bootstrap::failure_data(&error) {
        Some(data) => RpcResponse::error(
            id,
            RpcError::with_data(codes::INTERNAL_ERROR, error.to_string(), data),
        ),
        None => internal_error(id, error),
    }
}

fn remote_test_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    match vp_core::remote_test::failure_data(&error) {
        Some(data) => RpcResponse::error(
            id,
            RpcError::with_data(codes::INTERNAL_ERROR, error.to_string(), data),
        ),
        None => internal_error(id, error),
    }
}

fn remote_integration_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    match vp_core::remote_integration::failure_data(&error) {
        Some(data) => RpcResponse::error(
            id,
            RpcError::with_data(codes::INTERNAL_ERROR, error.to_string(), data),
        ),
        None => internal_error(id, error),
    }
}

/// Archive/media failures are typed with the Q3 catalog. Request-shaped
/// failures use INVALID_PARAMS; environment failures stay INTERNAL_ERROR. The
/// `data` object carries the same bounded code/retryable/hint contract used by
/// agent tools, so the Swift client can show the typed reason without parsing
/// the human message.
fn archive_media_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    match vp_core::archive_media::failure_data(&error) {
        Some(data) => {
            let code = data
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("TOOL_FAILED");
            let rpc_code = match code {
                "MEDIA_TOOL_UNAVAILABLE" | "INTERNAL_ERROR" | "ARCHIVE_CORRUPT" => {
                    codes::INTERNAL_ERROR
                }
                _ => codes::INVALID_PARAMS,
            };
            RpcResponse::error(id, RpcError::with_data(rpc_code, error.to_string(), data))
        }
        None => internal_error(id, error),
    }
}

fn spawn_worktree_sweeper(engine: Engine) {
    let seconds = std::env::var("VERLINPARK_WORKTREE_SWEEP_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(3600);
    if seconds == 0 {
        return;
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(seconds));
        interval.tick().await;
        loop {
            interval.tick().await;
            match engine.sweep_worktree_retention().await {
                Ok(removed) if removed > 0 => {
                    tracing::info!(removed, "automatic worktree retention removed worktrees");
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(error = %error, "automatic worktree retention failed");
                }
            }
        }
    });
}

fn spawn_checkpoint_sweeper(engine: Engine) {
    let seconds = std::env::var("VERLINPARK_CHECKPOINT_SWEEP_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(3600);
    if seconds == 0 {
        return;
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(seconds));
        interval.tick().await;
        loop {
            interval.tick().await;
            match engine.sweep_checkpoint_retention().await {
                Ok(removed) if removed > 0 => {
                    tracing::info!(removed, "automatic checkpoint retention pruned checkpoints");
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "automatic checkpoint retention failed");
                }
            }
            match engine.sweep_context_checkpoint_retention().await {
                Ok(removed) if removed > 0 => {
                    tracing::info!(
                        removed,
                        "automatic context checkpoint retention pruned checkpoints"
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "automatic context checkpoint retention failed");
                }
            }
        }
    });
}

fn spawn_tool_output_sweeper(engine: Engine) {
    let seconds = std::env::var("VERLINPARK_TOOL_OUTPUT_SWEEP_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(3600);
    if seconds == 0 {
        return;
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(seconds));
        interval.tick().await;
        loop {
            interval.tick().await;
            match engine.sweep_tool_output_retention().await {
                Ok(removed) if removed > 0 => {
                    tracing::info!(
                        removed,
                        "tool-output retention removed expired or excess handles"
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "tool-output retention sweep failed");
                }
            }
        }
    });
}

fn spawn_terminal_sweeper(engine: Engine) {
    let idle_secs = std::env::var("VERLINPARK_TERMINAL_IDLE_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(3600);
    if idle_secs == 0 {
        return;
    }
    // Sweep several times per idle window, but keep the check cheap.
    let interval_secs = (idle_secs / 4).clamp(15, 300);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
        interval.tick().await;
        loop {
            interval.tick().await;
            let (consoles, terminals) = engine
                .sweep_idle_terminals(Duration::from_secs(idle_secs))
                .await;
            if consoles + terminals > 0 {
                tracing::info!(
                    consoles,
                    terminals,
                    "closed idle console/terminal processes"
                );
            }
        }
    });
}

fn spawn_browser_sweeper(engine: Engine) {
    let idle = vp_core::browser::BrowserManager::idle_default();
    if idle.is_zero() {
        return;
    }
    let interval_secs = (idle.as_secs() / 4).clamp(15, 300);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
        interval.tick().await;
        loop {
            interval.tick().await;
            let browsers = engine.sweep_idle_browsers(idle).await;
            if browsers > 0 {
                tracing::info!(browsers, "closed idle browser helpers");
            }
        }
    });
}

/// Release the logical GUI lease shortly after its grace expires. Helpers stay
/// warm for their own idle window; this sweeper only releases ownership so the
/// next session can take the GUI without waiting for the 120s lease TTL.
fn spawn_gui_grace_sweeper(engine: Engine) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        interval.tick().await;
        loop {
            interval.tick().await;
            let released = engine.sweep_gui_grace().await;
            if released > 0 {
                tracing::debug!(released, "released GUI lease after grace");
            }
        }
    });
}

fn spawn_computer_sweeper(engine: Engine) {
    let idle = vp_core::computer::ComputerManager::idle_default();
    if idle.is_zero() {
        return;
    }
    let interval_secs = (idle.as_secs() / 4).clamp(15, 300);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
        interval.tick().await;
        loop {
            interval.tick().await;
            let computers = engine.sweep_idle_computers(idle).await;
            if computers > 0 {
                tracing::info!(computers, "closed idle computer helpers");
            }
        }
    });
}

/// Stall observation sweep. It always emits `orchestration.stall` events and
/// refreshes the observation window. When a project opts into `escalate` or
/// `retry`, the same call may record one bounded recovery transition; the
/// default `observe` mode remains telemetry-only.
fn spawn_task_stall_sweeper(engine: Engine) {
    let threshold_secs = std::env::var("VERLINPARK_TASK_STALL_SECS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(1_800);
    if threshold_secs == 0 {
        return;
    }
    let interval_secs = (threshold_secs / 4).clamp(1, 300);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
        interval.tick().await;
        loop {
            interval.tick().await;
            match engine
                .sweep_task_stalls(Duration::from_secs(threshold_secs))
                .await
            {
                Ok(stalled) if stalled > 0 => {
                    tracing::warn!(stalled, threshold_secs, "plan task stall observed");
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "task stall sweep failed");
                }
            }
        }
    });
}

/// Advance pending stall-recovery requests separately from the task scheduler.
/// A request waits for cooperative cancellation and session quiescence, then
/// reuses the normal retry + plan-resume path at most once per attempt.
fn spawn_stall_recovery_reconciler(agent: AgentService) {
    let seconds = std::env::var("VERLINPARK_STALL_RECOVERY_TICK_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(30);
    if seconds == 0 {
        return;
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(seconds.max(5)));
        interval.tick().await;
        loop {
            interval.tick().await;
            match agent.reconcile_stall_recoveries().await {
                Ok(advanced) if advanced > 0 => {
                    tracing::info!(advanced, "stall recovery reconciler advanced");
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "stall recovery reconciler failed");
                }
            }
        }
    });
}

/// Run the deterministic controller for sessions with failed or blocked plan
/// tasks. Task finishes already trigger a pass; this catches retry gates that
/// become due later and failures that survived a daemon restart.
fn spawn_orchestration_controller(engine: Engine) {
    let seconds = std::env::var("VERLINPARK_ORCHESTRATION_TICK_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(20);
    if seconds == 0 {
        return;
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(seconds.max(5)));
        interval.tick().await;
        loop {
            interval.tick().await;
            let candidates = match engine.orchestration_candidates(100).await {
                Ok(candidates) => candidates,
                Err(error) => {
                    tracing::warn!(%error, "orchestration candidate scan failed");
                    continue;
                }
            };
            for session_id in candidates {
                match engine.advance_orchestration(&session_id).await {
                    Ok(actions) if !actions.is_empty() => {
                        tracing::info!(
                            %session_id,
                            actions = actions.len(),
                            "orchestration controller advanced a plan"
                        );
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(%error, %session_id, "orchestration controller pass failed");
                    }
                }
            }
            if let Err(error) = engine.sweep_runtime_leases().await {
                tracing::warn!(%error, "runtime lease sweep failed");
            }
            let audit_retention_days = std::env::var("VERLINPARK_LEASE_AUDIT_DAYS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(30);
            if let Err(error) = engine.prune_runtime_lease_audit(audit_retention_days).await {
                tracing::warn!(%error, "runtime lease audit prune failed");
            }
            if let Err(error) = engine.sweep_computer_leases().await {
                tracing::warn!(%error, "computer lease sweep failed");
            }
        }
    });
}

fn spawn_memory_sweeper(engine: Engine) {
    let seconds = std::env::var("VERLINPARK_MEMORY_SWEEP_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(3600);
    if seconds == 0 {
        return;
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(seconds));
        interval.tick().await;
        loop {
            interval.tick().await;
            match engine.sweep_memory_retention().await {
                Ok(removed) if removed > 0 => {
                    tracing::info!(removed, "automatic memory retention pruned memories");
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "automatic memory retention failed");
                }
            }
        }
    });
}

fn spawn_memory_stale_sweeper(engine: Engine) {
    let seconds = std::env::var("VERLINPARK_MEMORY_STALE_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(3600);
    if seconds == 0 {
        return;
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(seconds));
        interval.tick().await;
        loop {
            interval.tick().await;
            match engine.sweep_memory_staleness().await {
                Ok((projects, marked)) if marked > 0 => {
                    tracing::info!(
                        projects,
                        marked,
                        "automatic memory staleness sweep marked memories stale"
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "automatic memory staleness sweep failed");
                }
            }
        }
    });
}

/// Bounded semantic indexing. One batch per enabled project per tick; the
/// reconciler performs no request when no project has an enabled embedding
/// policy and never touches session budgets.
fn spawn_memory_embedding_reconciler(engine: Engine) {
    let seconds = std::env::var("VERLINPARK_MEMORY_EMBEDDING_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(30);
    if seconds == 0 {
        return;
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(seconds));
        interval.tick().await;
        loop {
            interval.tick().await;
            match engine.reconcile_memory_embeddings().await {
                Ok((_projects, indexed, failed, skipped))
                    if indexed > 0 || failed > 0 || skipped > 0 =>
                {
                    tracing::info!(
                        indexed,
                        failed,
                        skipped,
                        "memory embedding reconciler processed a batch"
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "memory embedding reconciler failed");
                }
            }
        }
    });
}

/// Bounded semantic source indexing. One batch per enabled project per tick;
/// no request is made while no project opted in, and embedding failures never
/// touch source writes, lexical search, or session budgets.
fn spawn_source_embedding_reconciler(engine: Engine) {
    let seconds = std::env::var("VERLINPARK_SOURCE_EMBEDDING_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(45);
    if seconds == 0 {
        return;
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(seconds));
        interval.tick().await;
        loop {
            interval.tick().await;
            match engine.reconcile_source_embeddings().await {
                Ok((_projects, indexed, failed, done)) if indexed > 0 || failed > 0 || done > 0 => {
                    tracing::info!(
                        indexed,
                        failed,
                        done,
                        "source embedding reconciler processed a batch"
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "source embedding reconciler failed");
                }
            }
        }
    });
}

/// Deterministic same-scope conflict detection. This tick never resolves a
/// conflict and never mutates memory status; operator resolution stays explicit.
fn spawn_memory_conflict_scanner(engine: Engine) {
    let seconds = std::env::var("VERLINPARK_MEMORY_CONFLICT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(3600);
    if seconds == 0 {
        return;
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(seconds));
        interval.tick().await;
        loop {
            interval.tick().await;
            match engine.scan_all_memory_conflicts().await {
                Ok((projects, detected, open)) if detected > 0 => {
                    tracing::info!(
                        projects,
                        detected,
                        open,
                        "automatic memory conflict scan detected new conflicts"
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "automatic memory conflict scan failed");
                }
            }
        }
    });
}

async fn server_info(state: &AppState) -> Result<ServerInfo> {
    let projects = state.engine.list_projects().await?;
    let sessions = state.engine.list_sessions(None).await?;
    Ok(ServerInfo {
        pid: state.pid,
        version: APP_VERSION.to_string(),
        protocol: PROTOCOL_VERSION,
        data_dir: state.data_dir.to_string_lossy().to_string(),
        started_at: state.started_at.clone(),
        sessions: sessions.len(),
        projects: projects.len(),
    })
}

async fn send_json(socket: &mut WebSocket, value: &impl serde::Serialize) -> Result<()> {
    let text = serde_json::to_string(value)?;
    socket
        .send(Message::Text(text.into()))
        .await
        .context("sending websocket message")
}

fn authorized(headers: &HeaderMap, token: &str) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|candidate| constant_time_eq(candidate, token))
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (left, right)| acc | (left ^ right))
        == 0
}

fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn discovery_path(data_dir: &Path) -> PathBuf {
    data_dir.join("run").join("daemon.json")
}

fn acquire_instance_lock(data_dir: &Path) -> Result<std::fs::File> {
    let run_dir = data_dir.join("run");
    paths::ensure_private_dir(&run_dir)?;
    let path = run_dir.join("daemon.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    paths::ensure_private_file(&path)?;
    file.try_lock_exclusive().map_err(|_| {
        anyhow::anyhow!(
            "another verlinpadd is already running for {}",
            data_dir.display()
        )
    })?;
    Ok(file)
}

fn write_discovery(data_dir: &Path, discovery: &Discovery) -> Result<()> {
    let path = discovery_path(data_dir);
    let parent = path.parent().context("discovery path has no parent")?;
    paths::ensure_private_dir(parent)?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, serde_json::to_vec_pretty(discovery)?)?;
    paths::ensure_private_file(&temp)?;
    std::fs::rename(&temp, &path)?;
    Ok(())
}

fn remove_discovery(data_dir: &Path) {
    let _ = std::fs::remove_file(discovery_path(data_dir));
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut signal) = signal(SignalKind::terminate()) {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

#[cfg(test)]
mod process_rpc_tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;
    use std::sync::Arc;
    use tempfile::TempDir;
    use vp_core::process_supervisor::{ProcessSupervisor, StartSpec};
    use vp_core::tools::ToolContext;
    use vp_protocol::rpc::RpcRequest;

    fn context(
        worktree: &Path,
        data: &Path,
        _store: &Store,
        session: &str,
        supervisor: Arc<ProcessSupervisor>,
    ) -> ToolContext {
        ToolContext {
            root: worktree.to_path_buf(),
            project_root: worktree.to_path_buf(),
            sandbox_root: data.join("sandbox").join(session),
            secret_roots: Vec::new(),
            network_allowed: true,
            home: worktree.join("home"),
            restrict_home: false,
            read_jail: false,
            git_branch: None,
            remote: None,
            session_id: Some(session.to_string()),
            process_supervisor: Some(supervisor),
            test_cache: None,
        }
    }

    async fn call(state: &AppState, method: &str, params: Value) -> Value {
        let response = dispatch(state, RpcRequest::new(json!(1), method, params)).await;
        assert!(
            response.error.is_none(),
            "{} failed: {:?}",
            method,
            response.error
        );
        response.result.unwrap_or(Value::Null)
    }

    #[tokio::test]
    async fn process_rpcs_and_metrics_cover_the_durable_surface() {
        let worktree = TempDir::new().unwrap();
        std::fs::create_dir_all(worktree.path().join("home")).unwrap();
        let data = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(worktree.path().to_str().unwrap(), None)
            .unwrap();
        let session = store
            .create_session(&project.id, "process rpc test")
            .unwrap();
        let engine = Engine::new(store.clone());
        let supervisor = Arc::clone(engine.process_supervisor());
        let context = context(
            worktree.path(),
            data.path(),
            &store,
            &session.id,
            supervisor.clone(),
        );
        let row = supervisor
            .start(
                &session.id,
                &context,
                &StartSpec {
                    command: "printf 'rpc-ready\\n'; sleep 20".to_string(),
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap();
        let state = AppState {
            engine: engine.clone(),
            agent: AgentService::from_env(engine.clone()),
            token: Arc::new("test-token".to_string()),
            data_dir: data.path().to_path_buf(),
            started_at: now_rfc3339(),
            started: Instant::now(),
            pid: std::process::id(),
        };

        let list = call(
            &state,
            "session.processes",
            json!({"session_id": session.id, "include_finished": false, "limit": 10}),
        )
        .await;
        assert_eq!(list["session_id"], session.id);
        assert_eq!(list["active"], 1);
        assert_eq!(list["processes"][0]["id"], row.id);

        let logs = call(
            &state,
            "session.process.logs",
            json!({
                "session_id": session.id,
                "process_id": row.id,
                "from_end": true,
                "max_bytes": 1024,
            }),
        )
        .await;
        assert_eq!(logs["process_id"], row.id);
        assert_eq!(logs["state"], "running");
        let content = logs["content"].as_str().unwrap_or_default();
        // The shell may not have flushed the first line yet on a loaded CI box;
        // poll briefly rather than asserting on a scheduling race.
        if !content.contains("rpc-ready") {
            for _ in 0..50 {
                tokio::time::sleep(Duration::from_millis(50)).await;
                let logs = call(
                    &state,
                    "session.process.logs",
                    json!({
                        "session_id": session.id,
                        "process_id": row.id,
                        "from_end": true,
                        "max_bytes": 1024,
                    }),
                )
                .await;
                if logs["content"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("rpc-ready")
                {
                    break;
                }
            }
        }

        let metrics = call(&state, "daemon.metrics", json!({})).await;
        let process_counts = metrics["metrics"]["supervised_processes"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            process_counts
                .iter()
                .any(|entry| entry["key"] == "running" && entry["count"].as_u64() == Some(1)),
            "metrics={metrics}"
        );
        assert!(
            metrics["metrics"]["supervised_process_output_bytes"]
                .as_u64()
                .is_some()
        );
        assert!(
            metrics["prometheus"]
                .as_str()
                .unwrap_or_default()
                .contains("verlinpark_supervised_processes{state=\"running\"} 1")
        );

        let policy = call(
            &state,
            "process.policy.get",
            json!({"project_id": project.id}),
        )
        .await;
        assert_eq!(policy["policy"]["max_processes"], 8);
        let updated = call(
            &state,
            "process.policy.set",
            json!({
                "project_id": project.id,
                "policy": {
                    "idle_sweep_enabled": false,
                    "max_processes": 2,
                    "idle_ttl_secs": 600,
                    "max_output_bytes": 65536,
                    "max_output_lines": 2000
                }
            }),
        )
        .await;
        assert_eq!(updated["policy"]["max_processes"], 2);
        assert_eq!(updated["policy"]["idle_sweep_enabled"], false);
        let reread = call(
            &state,
            "process.policy.get",
            json!({"project_id": project.id}),
        )
        .await;
        assert_eq!(reread["policy"]["idle_ttl_secs"], 600);

        let stopped = call(
            &state,
            "session.process.stop",
            json!({
                "session_id": session.id,
                "process_id": row.id,
                "signal": "TERM",
                "grace_secs": 2,
            }),
        )
        .await;
        assert_eq!(stopped["process"]["state"], "stopped");
        assert_eq!(stopped["forced"], false);

        let missing = dispatch(
            &state,
            RpcRequest::new(
                json!(2),
                "session.process.logs",
                json!({"session_id": session.id, "process_id": "psup_missing"}),
            ),
        )
        .await;
        assert!(missing.error.is_some());
    }

    #[tokio::test]
    async fn backup_status_reports_anchor_without_secret_material() {
        let data = TempDir::new().unwrap();
        let keys = TempDir::new().unwrap();
        let repository_id = "0190f000-0000-7000-8000-000000000000";
        let anchor = vp_core::backup::anchor::AnchorStore::new(data.path());
        anchor
            .record_verified(repository_id, "/backups/repo", None, 4, "verify", false)
            .unwrap();
        let status = backup_status_in(data.path(), keys.path()).unwrap();
        assert!(status.anchor_present);
        assert_eq!(status.repositories.len(), 1);
        assert_eq!(status.repositories[0].highest_generation, 4);
        assert_eq!(status.repositories[0].last_verified_generation, 4);
        assert_eq!(status.signed_repository_formats, vec![3]);
        assert_eq!(status.revoked_key_count, 0);
        assert_eq!(
            status.portable_anchor_format_version,
            vp_core::backup::anchor::PORTABLE_ANCHOR_FORMAT_VERSION
        );
        assert_eq!(status.imported_repositories, 0);
        assert!(status.note.contains("not tamper-proof"));
        assert!(status.note.contains("allow-revoked"));
        let rendered = serde_json::to_string(&status).unwrap();
        assert!(!rendered.contains("seed_b64"));
        assert!(!rendered.contains("private_key"));
        assert!(!rendered.contains("passphrase"));
    }

    #[tokio::test]
    async fn backup_status_reports_revoked_keys_and_portable_imports_without_secrets() {
        use vp_core::backup::anchor::{AnchorStore, PortableAnchorImportOptions};
        use vp_core::backup::signing::{Keyring, SecretSource};

        let data = TempDir::new().unwrap();
        let keys = TempDir::new().unwrap();
        let source_data = TempDir::new().unwrap();
        Keyring::generate_key(
            keys.path(),
            "old",
            true,
            SecretSource::File {
                name: "old.key".to_string(),
            },
        )
        .unwrap();
        Keyring::generate_key(
            keys.path(),
            "new",
            true,
            SecretSource::File {
                name: "new.key".to_string(),
            },
        )
        .unwrap();
        let mut keyring = Keyring::load(keys.path()).unwrap();
        keyring.revoke_key("old", false).unwrap();

        let repository_id = "0190f000-0000-7000-8000-000000000011";
        let source = AnchorStore::new(source_data.path());
        source
            .record_verified(repository_id, "/source/repo", None, 7, "verify", false)
            .unwrap();
        let export = source
            .export_portable(&Keyring::load(keys.path()).unwrap(), None, None)
            .unwrap();
        let target = AnchorStore::new(data.path());
        target
            .import_portable(
                &export.document,
                PortableAnchorImportOptions {
                    keyring: &Keyring::load(keys.path()).unwrap(),
                    allow_revoked: false,
                    reject_stale: false,
                },
            )
            .unwrap();

        let status = backup_status_in(data.path(), keys.path()).unwrap();
        assert_eq!(status.active_key_id.as_deref(), Some("new"));
        assert_eq!(status.revoked_key_count, 1);
        assert_eq!(status.imported_repositories, 1);
        assert_eq!(status.portable_anchor_format_version, 1);
        let old = status.keys.iter().find(|key| key.key_id == "old").unwrap();
        assert!(old.revoked_at.is_some());
        assert!(!old.active);
        assert_eq!(
            status.repositories[0].origin.as_deref(),
            Some("portable_import")
        );
        assert_eq!(status.repositories[0].last_operation, "anchor_import");

        let rendered = serde_json::to_string(&status).unwrap();
        assert!(!rendered.contains("seed_b64"));
        assert!(!rendered.contains("private_key"));
        assert!(!rendered.contains("passphrase"));
    }

    #[tokio::test]
    async fn preemption_rpcs_round_trip_policy_and_report_no_active_run() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        std::fs::write(repo.path().join("README.md"), "preempt\n").unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(repo.path())
            .status()
            .unwrap();
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("preempt"))
            .unwrap();
        let session = store.create_session(&project.id, "preemption rpc").unwrap();
        let engine = Engine::new(store.clone());
        let state = AppState {
            engine: engine.clone(),
            agent: AgentService::from_env(engine.clone()),
            token: Arc::new("test-token".to_string()),
            data_dir: data.path().to_path_buf(),
            started_at: now_rfc3339(),
            started: Instant::now(),
            pid: std::process::id(),
        };
        let current = call(
            &state,
            "project.preemption_policy.get",
            json!({"project_id": project.id}),
        )
        .await;
        assert_eq!(current["policy"]["enabled"], true);
        assert_eq!(current["policy"]["provider_grace_ms"], 2_000);
        let updated = call(
            &state,
            "project.preemption_policy.set",
            json!({
                "project_id": project.id,
                "policy": {
                    "enabled": true,
                    "provider_grace_ms": 999_999,
                    "tool_grace_ms": 1_500,
                    "kill_escalation_ms": 500,
                    "tool_deadline_secs": 9_999,
                    "checkpoint_on_preempt": false
                }
            }),
        )
        .await;
        assert_eq!(updated["policy"]["provider_grace_ms"], 30_000);
        assert_eq!(updated["policy"]["tool_grace_ms"], 1_500);
        assert_eq!(updated["policy"]["tool_deadline_secs"], 3_600);
        assert_eq!(updated["policy"]["checkpoint_on_preempt"], false);

        let report = call(
            &state,
            "session.preemption",
            json!({"session_id": session.id, "limit": 10}),
        )
        .await;
        assert_eq!(report["session_id"], session.id);
        assert_eq!(report["preempted"], false);
        assert_eq!(report["active"], false);
        assert_eq!(report["policy"]["tool_grace_ms"], 1_500);
        assert!(report["records"].as_array().unwrap().is_empty());

        let preempt = call(
            &state,
            "session.preempt",
            json!({"session_id": session.id, "reason": "rpc test"}),
        )
        .await;
        assert_eq!(preempt["preempted"], false);
        assert!(
            preempt["detail"]
                .as_str()
                .unwrap_or_default()
                .contains("no active run")
        );

        engine
            .append_event(
                &session.id,
                "session.preempted",
                json!({
                    "session_id": session.id,
                    "reason": "wall_clock",
                    "phase": "provider",
                    "elapsed_ms": 1005,
                    "terminal": true,
                    "created_at": now_rfc3339()
                }),
            )
            .await
            .unwrap();
        let metrics = call(&state, "daemon.metrics", json!({})).await;
        let phases = metrics["metrics"]["preemptions_by_phase"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            phases
                .iter()
                .any(|entry| entry["key"] == "provider" && entry["count"].as_u64() == Some(1)),
            "metrics={metrics}"
        );
        assert!(
            metrics["prometheus"]
                .as_str()
                .unwrap_or_default()
                .contains("verlinpark_preemptions{phase=\"provider\"} 1")
        );
        let overview = call(
            &state,
            "preemption.overview",
            json!({"project_id": project.id, "limit": 10}),
        )
        .await;
        assert_eq!(overview["total"], 1);
        assert_eq!(overview["sessions"].as_array().unwrap().len(), 1);
        assert_eq!(overview["sessions"][0]["last_reason"], "wall_clock");
        assert_eq!(overview["sessions"][0]["last_phase"], "provider");
        assert_eq!(overview["by_phase"][0]["key"], "provider");
    }

    #[tokio::test]
    async fn forge_status_and_preflight_rpcs_are_secret_free() {
        let data = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let engine = Engine::new(store);
        let state = AppState {
            engine: engine.clone(),
            agent: AgentService::from_env(engine),
            token: Arc::new("test-token".to_string()),
            data_dir: data.path().to_path_buf(),
            started_at: now_rfc3339(),
            started: Instant::now(),
            pid: std::process::id(),
        };

        let status = call(&state, "forge.status", json!({})).await;
        assert_eq!(status["default_provider"], "github");
        let providers = status["providers"].as_array().unwrap();
        assert_eq!(providers.len(), 4);
        let bitbucket = providers
            .iter()
            .find(|provider| provider["provider"] == "bitbucket")
            .unwrap();
        assert_eq!(bitbucket["transport"], "rest");
        assert_eq!(bitbucket["change_name"], "PR");
        assert!(bitbucket.get("command").is_none());
        let serialized = status.to_string();
        assert!(!serialized.contains("BITBUCKET_APP_PASSWORD="));

        let response = dispatch(
            &state,
            RpcRequest::new(
                json!(2),
                "forge.preflight",
                json!({"project_id": "prj-missing"}),
            ),
        )
        .await;
        let error = response.error.unwrap();
        assert!(
            error.message.contains("project not found"),
            "{}",
            error.message
        );
    }
}

#[cfg(test)]
mod archive_media_rpc_tests {
    use super::*;
    use serde_json::json;
    use std::io::Write as _;
    use tempfile::TempDir;
    use vp_protocol::rpc::RpcRequest;
    use vp_protocol::types::MediaTranscodeParams;

    fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buffer = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buffer);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for (name, data) in entries {
                writer.start_file(*name, options).unwrap();
                writer.write_all(data).unwrap();
            }
            writer.finish().unwrap();
        }
        buffer.into_inner()
    }

    async fn call(state: &AppState, method: &str, params: Value) -> Value {
        let response = dispatch(state, RpcRequest::new(json!(1), method, params)).await;
        assert!(
            response.error.is_none(),
            "{} failed: {:?}",
            method,
            response.error
        );
        response.result.unwrap_or(Value::Null)
    }

    async fn call_error(state: &AppState, method: &str, params: Value) -> RpcError {
        let response = dispatch(state, RpcRequest::new(json!(1), method, params)).await;
        response
            .error
            .unwrap_or_else(|| panic!("{method} unexpectedly succeeded"))
    }

    #[tokio::test]
    async fn archive_and_media_rpcs_return_typed_results_and_failures() {
        let worktree = TempDir::new().unwrap();
        let data = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(worktree.path().to_str().unwrap(), Some("archive-rpc"))
            .unwrap();
        let session = store.create_session(&project.id, "archive rpc").unwrap();
        let engine = Engine::new(store);
        let artifact = engine
            .publish_artifact_binary(
                &session.id,
                "tester",
                "Tester",
                None,
                "bundle.zip",
                "archive",
                "application/zip",
                &zip_bytes(&[("a.txt", b"alpha"), ("dir/b.txt", b"beta")]),
            )
            .await
            .unwrap();
        let state = AppState {
            engine: engine.clone(),
            agent: AgentService::from_env(engine.clone()),
            token: Arc::new("test-token".to_string()),
            data_dir: data.path().to_path_buf(),
            started_at: now_rfc3339(),
            started: Instant::now(),
            pid: std::process::id(),
        };

        let listing = call(
            &state,
            "artifact.archive.list",
            json!({"artifact_id": artifact.id}),
        )
        .await;
        assert_eq!(listing["format"], "zip");
        assert_eq!(listing["entry_count"], 2);
        assert_eq!(listing["entries"][1]["path"], "dir/b.txt");
        assert_eq!(listing["entries"][1]["safe"], true);

        let extraction = call(
            &state,
            "artifact.archive.extract",
            json!({
                "artifact_id": artifact.id,
                "entries": ["dir/b.txt"],
                "include_previews": true
            }),
        )
        .await;
        assert_eq!(extraction["entries"][0]["preview"], "beta");
        assert_eq!(extraction["scratch_retained"], false);
        let derived_id = extraction["published_artifacts"][0]["id"]
            .as_str()
            .unwrap()
            .to_string();
        let derived = call(&state, "artifact.get", json!({"artifact_id": derived_id})).await;
        assert_eq!(derived["provenance"]["operation"], "archive_extract");
        assert_eq!(derived["provenance"]["source_artifact_id"], artifact.id);
        assert_eq!(derived["provenance"]["entry_path"], "dir/b.txt");

        let malicious = engine
            .publish_artifact_binary(
                &session.id,
                "tester",
                "Tester",
                None,
                "slip.zip",
                "archive",
                "application/zip",
                &zip_bytes(&[("../escape.txt", b"bad")]),
            )
            .await
            .unwrap();
        let error = call_error(
            &state,
            "artifact.archive.extract",
            json!({
                "artifact_id": malicious.id,
                "entries": ["../escape.txt"]
            }),
        )
        .await;
        let failure = error.data.expect("typed archive failure data");
        assert_eq!(failure["code"], "ARCHIVE_UNSAFE_ENTRY");
        assert_eq!(failure["retryable"], false);

        let transcode_error = call_error(
            &state,
            "media.transcode",
            json!({
                "artifact_id": artifact.id,
                "format": "mp4",
                "approved": false
            }),
        )
        .await;
        let failure = transcode_error.data.expect("typed media failure data");
        assert_eq!(failure["code"], "APPROVAL_REQUIRED");

        let media_error =
            call_error(&state, "media.info", json!({"artifact_id": artifact.id})).await;
        let failure = media_error.data.expect("typed media failure data");
        assert!(
            matches!(
                failure["code"].as_str(),
                Some("MEDIA_TOOL_UNAVAILABLE")
                    | Some("MEDIA_PROBE_FAILED")
                    | Some("MEDIA_LIMIT_EXCEEDED")
            ),
            "{failure}"
        );
    }

    #[tokio::test]
    async fn archive_rpc_rejects_unapproved_and_unsupported_requests() {
        let worktree = TempDir::new().unwrap();
        let data = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(worktree.path().to_str().unwrap(), None)
            .unwrap();
        let session = store.create_session(&project.id, "archive rpc 2").unwrap();
        let engine = Engine::new(store);
        let artifact = engine
            .publish_artifact_binary(
                &session.id,
                "tester",
                "Tester",
                None,
                "bundle.zip",
                "archive",
                "application/zip",
                &zip_bytes(&[("a.txt", b"a")]),
            )
            .await
            .unwrap();
        let state = AppState {
            engine: engine.clone(),
            agent: AgentService::from_env(engine.clone()),
            token: Arc::new("test-token".to_string()),
            data_dir: data.path().to_path_buf(),
            started_at: now_rfc3339(),
            started: Instant::now(),
            pid: std::process::id(),
        };

        let error = call_error(
            &state,
            "artifact.archive.extract",
            json!({"artifact_id": artifact.id, "entries": []}),
        )
        .await;
        assert_eq!(error.data.unwrap()["code"], "ARCHIVE_EMPTY_SELECTION");

        let error = call_error(
            &state,
            "artifact.archive.extract",
            json!({"artifact_id": artifact.id, "entries": ["missing.txt"]}),
        )
        .await;
        assert_eq!(error.data.unwrap()["code"], "ARCHIVE_ENTRY_NOT_FOUND");

        let error = call_error(
            &state,
            "media.transcode",
            json!({
                "artifact_id": artifact.id,
                "format": "avi",
                "approved": true
            }),
        )
        .await;
        assert_eq!(error.data.unwrap()["code"], "MEDIA_FORMAT_UNSUPPORTED");
        let _ = MediaTranscodeParams {
            artifact_id: artifact.id,
            format: "mp4".to_string(),
            max_duration_secs: None,
            max_bytes: None,
            approved: false,
        };
    }
}

#[cfg(test)]
mod runtime_pin_rpc_tests {
    use super::*;

    #[test]
    fn compatibility_and_replay_params_decode_with_defaults() {
        let compatibility = parse_params::<SessionCompatibilityParams>(json!({
            "session_id": "ses-1"
        }))
        .unwrap();
        assert_eq!(compatibility.session_id, "ses-1");
        assert!(compatibility.run_id.is_none());

        let migrate = parse_params::<PlanMigrateParams>(json!({
            "session_id": "ses-1"
        }))
        .unwrap();
        assert!(!migrate.approved);
        assert!(migrate.expected_revision.is_none());

        let replay = parse_params::<ControllerReplayParams>(json!({
            "session_id": "ses-1"
        }))
        .unwrap();
        assert!(replay.limit.is_none());
    }

    #[test]
    fn otel_params_decode_with_defaults_and_errors_are_bounded() {
        let status = parse_params::<OtelStatusParams>(json!({"project_id": "proj-1"})).unwrap();
        assert!(status.session_id.is_none());
        let export = parse_params::<OtelExportParams>(json!({"session_id": "ses-1"})).unwrap();
        assert!(export.limit.is_none());
        let test = parse_params::<OtelTestParams>(json!({"project_id": "proj-1"})).unwrap();
        assert_eq!(test.project_id, "proj-1");
        let set = parse_params::<SetOtelPolicyParams>(json!({
            "project_id": "proj-1",
            "policy": {"enabled": true}
        }))
        .unwrap();
        assert!(set.policy.enabled);
        assert_eq!(set.policy.batch_limit, 200);

        let error = vp_core::otel::otel_error(
            vp_protocol::otel::otel_codes::OTEL_ENDPOINT_NOT_LOOPBACK,
            "the endpoint is public",
        );
        let response = otel_error(Some(json!("id")), error);
        let error = response.error.expect("error");
        assert!(error.message.contains("OTEL_ENDPOINT_NOT_LOOPBACK"));
        let data = error.data.expect("data");
        assert_eq!(data["code"], "OTEL_ENDPOINT_NOT_LOOPBACK");
        assert_eq!(data["retryable"], false);
        assert!(!data.to_string().contains("api_key"));
        assert!(!data.to_string().contains("prompt"));
    }

    #[test]
    fn diagnosis_params_decode_and_typed_errors_expose_no_content() {
        let params = parse_params::<SessionDiagnosisParams>(json!({
            "session_id": "ses-1",
            "target_task_id": "t2",
            "limit": 500,
            "include_graph": true
        }))
        .unwrap();
        assert_eq!(params.session_id, "ses-1");
        assert_eq!(params.target_task_id.as_deref(), Some("t2"));
        assert_eq!(params.limit, Some(500));
        assert!(params.include_graph);
        let record = parse_params::<SessionDiagnosisRecordParams>(json!({
            "session_id": "ses-1",
            "target_task_id": "t2"
        }))
        .unwrap();
        assert_eq!(record.target_task_id.as_deref(), Some("t2"));
        let history = parse_params::<SessionAttributionsParams>(json!({
            "session_id": "ses-1"
        }))
        .unwrap();
        assert!(history.limit.is_none());

        let error = vp_core::diagnosis::DiagnosisError::new(
            vp_core::diagnosis::codes::DIAGNOSIS_SESSION_NOT_FOUND,
            "session not found: ses-missing",
        );
        let response = diagnosis_error(Some(json!("id")), error.into());
        let error = response.error.expect("error");
        let data = error.data.expect("data");
        assert_eq!(data["code"], "DIAGNOSIS_SESSION_NOT_FOUND");
        assert_eq!(data["advisory"], true);
        assert_eq!(data["retryable"], false);
        assert!(!data.to_string().contains("prompt"));
        assert!(!data.to_string().contains("output"));
    }

    #[test]
    fn runtime_pin_errors_expose_q3_data_without_secrets() {
        let report = vp_protocol::runtime::CompatibilityReport {
            status: vp_protocol::runtime::CompatibilityStatus::Incompatible,
            target: "plan".to_string(),
            session_id: "ses-1".to_string(),
            plan_revision: 2,
            warnings: vec!["contract_changed".to_string()],
            migration_required: true,
            ..Default::default()
        };
        let error = anyhow::Error::new(
            vp_core::runtime_pin::RuntimePinError::new(
                vp_core::runtime_pin::codes::RUNTIME_INCOMPATIBLE,
                "plan changed",
            )
            .with_report(report),
        );
        let response = runtime_pin_error(Some(json!("id")), error);
        let error = response.error.expect("error");
        assert!(error.message.contains("RUNTIME_INCOMPATIBLE"));
        let data = error.data.expect("data");
        assert_eq!(data["code"], "RUNTIME_INCOMPATIBLE");
        assert_eq!(data["retryable"], false);
        assert_eq!(data["compatibility"]["status"], "incompatible");
        assert!(!data.to_string().contains("api_key"));
    }
}

#[cfg(test)]
mod agent_identity_rpc_error_tests {
    use super::*;
    use vp_protocol::types::{AgentIdentityError, agent_identity_codes};

    #[test]
    fn typed_identity_errors_map_to_stable_rpc_codes_without_text_classification() {
        let response = identity_error(
            Some(json!("1")),
            anyhow::Error::new(AgentIdentityError::new(
                agent_identity_codes::IDENTITY_NOT_FOUND,
                "agent identity not found: identity-1",
            )),
        );
        let error = response.error.expect("error");
        assert_eq!(error.code, codes::NOT_FOUND);
        assert_eq!(
            error.data.expect("data")["code"],
            agent_identity_codes::IDENTITY_NOT_FOUND
        );

        let response = identity_error(
            Some(json!("2")),
            anyhow::Error::new(AgentIdentityError::new(
                agent_identity_codes::IDENTITY_CAP_EXCEEDED,
                "max_workers is 1",
            )),
        );
        let error = response.error.expect("error");
        assert_eq!(error.code, codes::INVALID_PARAMS);
        let data = error.data.expect("data");
        assert_eq!(data["code"], agent_identity_codes::IDENTITY_CAP_EXCEEDED);
        assert!(!data.to_string().contains("prompt"));
        assert!(!data.to_string().contains("api_key"));
    }

    #[test]
    fn untyped_identity_failures_remain_internal_errors() {
        let response = identity_error(Some(json!("3")), anyhow::anyhow!("database is closed"));
        let error = response.error.expect("error");
        assert_eq!(error.code, codes::INTERNAL_ERROR);
        assert!(error.data.is_none());
    }

    #[test]
    fn s6_employment_errors_are_conflicts_and_do_not_leak_evidence_text() {
        for code in [
            agent_identity_codes::IDENTITY_BUSY,
            agent_identity_codes::HIRE_ALREADY_DECIDED,
            agent_identity_codes::RETIREMENT_NOT_QUIESCENT,
            agent_identity_codes::RETIREMENT_COOLDOWN,
        ] {
            let response = identity_error(
                Some(json!("4")),
                anyhow::Error::new(AgentIdentityError::new(code, "identity needs a decision")),
            );
            let error = response.error.expect("error");
            assert_eq!(error.code, codes::CONFLICT);
            assert_eq!(error.data.expect("data")["code"], code);
        }
        for code in [
            agent_identity_codes::RETIREMENT_APPROVAL_REQUIRED,
            agent_identity_codes::IDENTITY_APPROVAL_REQUIRED,
            agent_identity_codes::HIRE_INVALID,
        ] {
            let response = identity_error(
                Some(json!("5")),
                anyhow::Error::new(AgentIdentityError::new(code, "approval required")),
            );
            let error = response.error.expect("error");
            assert_eq!(error.code, codes::INVALID_PARAMS);
            assert_eq!(error.data.expect("data")["code"], code);
        }
    }
}

#[cfg(test)]
mod agent_coordination_rpc_error_tests {
    use super::*;
    use vp_protocol::types::{AgentCoordinationError, agent_coordination_codes};

    #[test]
    fn typed_coordination_errors_map_to_bounded_rpc_codes() {
        let response = coordination_error(
            Some(json!("1")),
            anyhow::Error::new(AgentCoordinationError::new(
                agent_coordination_codes::ASSIGNMENT_NOT_FOUND,
                "session not found: ses-1",
            )),
        );
        let error = response.error.expect("error");
        assert_eq!(error.code, codes::NOT_FOUND);
        assert_eq!(
            error.data.expect("data")["code"],
            agent_coordination_codes::ASSIGNMENT_NOT_FOUND
        );

        let response = coordination_error(
            Some(json!("2")),
            anyhow::Error::new(AgentCoordinationError::new(
                agent_coordination_codes::ASSIGNMENT_CONFLICT,
                "identity already assigned",
            )),
        );
        let error = response.error.expect("error");
        assert_eq!(error.code, codes::INVALID_PARAMS);
        let data = error.data.expect("data");
        assert_eq!(data["code"], agent_coordination_codes::ASSIGNMENT_CONFLICT);
        assert!(!data.to_string().contains("prompt"));
        assert!(!data.to_string().contains("api_key"));

        let response = coordination_error(
            Some(json!("3")),
            anyhow::Error::new(vp_protocol::types::AgentIdentityError::new(
                vp_protocol::types::agent_identity_codes::IDENTITY_NOT_FOUND,
                "agent identity not found: id-1",
            )),
        );
        let error = response.error.expect("error");
        assert_eq!(error.code, codes::NOT_FOUND);
        assert_eq!(
            error.data.expect("data")["code"],
            vp_protocol::types::agent_identity_codes::IDENTITY_NOT_FOUND
        );
    }

    #[test]
    fn untyped_coordination_failures_remain_internal_errors() {
        let response = coordination_error(Some(json!("4")), anyhow::anyhow!("database closed"));
        let error = response.error.expect("error");
        assert_eq!(error.code, codes::INTERNAL_ERROR);
        assert!(error.data.is_none());
    }

    #[test]
    fn workspace_busy_is_a_typed_conflict_and_imported_is_read_only() {
        let busy = workspace_error(
            Some(json!("9")),
            anyhow::Error::new(vp_protocol::types::AgentWorkspaceError::new(
                vp_protocol::types::agent_workspace_codes::WORKSPACE_BUSY,
                "workspace is leased by session ses-1",
            )),
        );
        let error = busy.error.expect("error");
        assert_eq!(error.code, codes::CONFLICT);
        assert_eq!(
            error.data.expect("data")["code"],
            vp_protocol::types::agent_workspace_codes::WORKSPACE_BUSY
        );

        let imported = workspace_error(
            Some(json!("10")),
            anyhow::Error::new(vp_protocol::types::AgentWorkspaceError::new(
                vp_protocol::types::agent_workspace_codes::WORKSPACE_IMPORTED_READONLY,
                "an imported workspace is read-only evidence",
            )),
        );
        let error = imported.error.expect("error");
        assert_eq!(error.code, codes::INVALID_PARAMS);
        assert_eq!(
            error.data.expect("data")["code"],
            vp_protocol::types::agent_workspace_codes::WORKSPACE_IMPORTED_READONLY
        );
    }

    #[test]
    fn workspace_untyped_failures_remain_internal_errors() {
        let response = workspace_error(Some(json!("11")), anyhow::anyhow!("git unavailable"));
        let error = response.error.expect("error");
        assert_eq!(error.code, codes::INTERNAL_ERROR);
        assert!(error.data.is_none());
    }
}

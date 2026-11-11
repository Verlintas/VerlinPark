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
use vp_protocol::discovery::Discovery;
use vp_protocol::lsp::LspStatusParams;
use vp_protocol::remote::{
    HostListParams, HostListResult, HostProbeParams, HostProjectParams, HostSetParams,
    RemoteLedgerParams, RemoteStatusParams, RemoteWorktreeListParams,
};
use vp_protocol::rpc::{RpcError, RpcNotification, RpcRequest, RpcResponse, codes};
use vp_protocol::scaling::{
    ScalingPolicyParams, ScalingPreviewParams, ScalingStatusParams, SetScalingPolicyParams,
};
use vp_protocol::types::{
    AcpAgentConfig, AcpAgentIdParams, AcpProbeParams, AddAcpAgentParams, AddMcpServerParams,
    AddProjectParams, AgentListParams, AgentListResult, AgentMessageResult, AnswerQuestionParams,
    AnswerQuestionResult, ApprovalGrantCreateParams, ApprovalGrantCreateResult,
    ApprovalGrantDeleteParams, ApprovalGrantDeleteResult, ApprovalGrantListParams,
    ApprovalGrantListResult, ApprovalListResult, ArtifactBackfillParams, ArtifactDiffParams,
    ArtifactGcParams, ArtifactGetParams, ArtifactListParams, ArtifactListResult,
    ArtifactMaterializeParams, ArtifactPolicyParams, ArtifactPolicyResult, ArtifactRestoreParams,
    ArtifactResult, ArtifactVersionsParams, AuthParams, AuthResult, CachePolicyParams,
    CachePolicyResult, CancelSessionParams, CancelSessionResult, CheckpointPolicyParams,
    CleanupWorktreesParams, CommitSessionParams, ConfigureAgentParams, ConsoleCloseParams,
    ConsoleOpenParams, ConsoleReadParams, ConsoleSignalParams, ConsoleWriteParams,
    ContextCheckpointCreateParams, ContextCheckpointDeleteParams, ContextCheckpointDetailParams,
    ContextCheckpointListParams, ContextCheckpointPolicyParams, ContextCheckpointPruneParams,
    ContextRestoreParams, CreateModelPresetParams, CreateSessionParams, DaemonMetrics,
    DaemonMetricsResult, DashboardParams, DeleteModelPresetParams, DiscardSessionParams,
    DoctorCheck, DoctorReport, EventListResult, ForkAuditParams, ForkRepairParams, GitLogParams,
    GitShowParams, IntakeCreateParams, IntakeDeleteResult, IntakeListParams, IntakeTaskParams,
    IntakeUpdateParams, IntegrationPolicyParams, IntegrationPolicyResult, LandSessionParams,
    ListApprovalsParams, ListEventsParams, ListSessionsParams, MailboxGcParams, MailboxListParams,
    MailboxListResult, MailboxPolicyParams, MailboxPolicyResult, MarkMessageReadParams,
    McpCallParams, McpGetPromptParams, McpProbeParams, McpReadResourceParams, McpServerIdParams,
    MemoryBriefParams, MemoryConflictResolveParams, MemoryConflictScanParams,
    MemoryConflictsParams, MemoryCreateParams, MemoryCreateResult, MemoryDeleteParams,
    MemoryEmbeddingsIndexRebuildParams, MemoryEmbeddingsIndexStatusParams,
    MemoryEmbeddingsRebuildParams, MemoryEmbeddingsStatusParams, MemoryEmbeddingsTestParams,
    MemoryHealthParams, MemoryListParams, MemoryPolicyParams, MemoryPruneParams,
    MemorySearchParams, MemoryStatsParams, MemorySweepParams, MemoryUpdateParams,
    MemoryVerifyParams, MergeQueueListParams, MergeQueueListResult, OpenPullRequestParams,
    PingResult, ProcessListParams, ProcessLogsParams, ProcessPolicyParams, ProcessPolicyResult,
    ProcessStopParams, ProjectBudgetParams, ProjectForkForestParams, ProjectListResult,
    ProjectUsageParams, PromptParams, PromptResult, QuestionListParams, RehydrateSessionParams,
    ReplaySessionParams, ResolveApprovalParams, ResolveApprovalResult, ResolveMergeParams,
    ResolveMergeResult, RetryMergeParams, RetryMergeResult, SchedulerStatusParams, SearchParams,
    ServerInfo, SessionCacheAuditParams, SessionCacheParams, SessionCachePlanParams,
    SessionCacheWarmParams, SessionCheckpointDeleteParams, SessionCheckpointDiffParams,
    SessionCheckpointListParams, SessionCheckpointParams, SessionCheckpointPruneParams,
    SessionCheckpointRestoreParams, SessionCheckpointStatsParams, SessionCitationAuditParams,
    SessionCitationsParams, SessionCompactParams, SessionCompareParams, SessionContextParams,
    SessionContinuationCancelParams, SessionContinuationReclaimParams,
    SessionContinuationStatusParams, SessionContinuationsParams, SessionDeferParams,
    SessionDiffParams, SessionExportParams, SessionForkParams, SessionForkTreeParams,
    SessionImportParams, SessionLineageParams, SessionListResult, SessionLogParams,
    SessionMergeForkParams, SessionPauseParams, SessionPauseResult, SessionPlanResumeParams,
    SessionPlanResumeResult, SessionResumeParams, SessionResumeResult, SessionRunCancelParams,
    SessionRunDetailParams, SessionRunPolicyParams, SessionRunResumeParams, SessionRunsParams,
    SessionSkillActivationsParams, SessionStallsParams, SessionSteerParams,
    SessionSteeringCancelParams, SessionSteeringParams, SessionTodosParams, SessionTodosResult,
    SessionTranscriptParams, SetArtifactPolicyParams, SetCachePolicyParams,
    SetCheckpointPolicyParams, SetContextCheckpointPolicyParams, SetIntegrationPolicyParams,
    SetMailboxPolicyParams, SetMemoryPolicyParams, SetProcessPolicyParams, SetProjectBudgetParams,
    SetSessionPriorityParams, SetSessionRunPolicyParams, SetSessionUnlimitedParams,
    SetShellPolicyParams, SetSourceCollectionParams, SetSourceEmbeddingPolicyParams,
    SetSourceLabelsParams, SetTaskSchedulerPolicyParams, SetToolOutputPolicyParams,
    SetWorktreePolicyParams, ShellPolicyParams, ShellPolicyResult, SkillCreateParams,
    SkillEnabledParams, SkillIdParams, SkillImportParams, SkillListParams, SkillUpdateParams,
    SourceActionParams, SourceCollectionsParams, SourceCreateFileParams, SourceCreateNoteParams,
    SourceCreateUrlParams, SourceEmbeddingPolicyParams, SourceEmbeddingReindexParams,
    SourceEmbeddingStatusParams, SourceGetParams, SourceImportParams, SourceListParams,
    SourceSearchParams, SubscribeParams, TaskActionParams, TaskActionResult, TaskDispatchParams,
    TaskDispatchResult, TaskQueueParams, TaskSchedulerPolicyParams, TeamPlanParams, TeamPlanResult,
    TerminalCloseParams, TerminalOpenParams, TerminalReadParams, TerminalResizeParams,
    TerminalWriteParams, ToolActivityParams, ToolOutputAuditParams, ToolOutputListParams,
    ToolOutputPolicyParams, ToolOutputPolicyResult, ToolOutputPruneParams, ToolOutputStatusParams,
    UpdateAcpAgentParams, UpdateCheckParams, UpdateDownloadParams, UpdateMcpServerParams,
    UpdateModelPresetParams, UpdateSourceMetadataParams, UsageLedgerParams, WorkspaceFileParams,
    WorkspaceGrepParams, WorkspaceListParams, WorkspaceSearchParams, WorkspaceTreeParams,
    WorkspaceWriteParams, WorktreePolicyParams,
};
use vp_protocol::types::{
    ComputerLeaseInfo, ComputerLeaseListResult, ComputerLeaseReleaseParams,
    ComputerLeaseReleaseResult, ComputerLeaseSweepResult, ComputerStatusParams, DaemonInstanceInfo,
    GuiPolicyParams, GuiPolicyResult, GuiTimelineParams, GuiTimelineResult, RuntimeLeaseAuditInfo,
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

#[tokio::main]
async fn main() -> Result<()> {
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
    spawn_task_stall_sweeper(state.engine.clone());
    spawn_stall_recovery_reconciler(state.agent.clone());
    spawn_process_sweeper(state.engine.clone());
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
                Ok(artifact) => RpcResponse::success(id, ArtifactResult { artifact }),
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
                .defer_session(
                    &params.session_id,
                    &params.body,
                    params.watch_task_ids,
                    params.watch_mode.as_deref(),
                    params.deadline_at.as_deref(),
                    params.after_secs,
                    params.max_attempts,
                    params.every_secs,
                    params.max_runs,
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
        "session.cache" => match parse_params::<SessionCacheParams>(request.params) {
            Ok(params) => match state.engine.session_cache_status(&params.session_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.cache.audit" => match parse_params::<SessionCacheAuditParams>(request.params) {
            Ok(params) => match state.engine.session_cache_audit(&params.session_id).await {
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
                Err(error) => internal_error(id, error),
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
                Err(error) => internal_error(id, error),
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
                Ok(Some(artifact)) => RpcResponse::success(id, ArtifactResult { artifact }),
                Ok(None) => RpcResponse::error(
                    id,
                    RpcError::new(codes::INVALID_PARAMS, "artifact not found".to_string()),
                ),
                Err(error) => internal_error(id, error),
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
            Ok(params) => match state.engine.open_pull_request(&params.session_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
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
                Err(error) => internal_error(id, error),
            },
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
            | "process.policy.set"
            | "mailbox.policy.set"
            | "artifact.policy.set"
            | "tool_output.policy.set"
            | "session.import"
    ) && let Err(error) = state.engine.record_project_manifest().await
    {
        tracing::warn!(%error, "failed to update project manifest");
    }
    response
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
            agent: AgentService::from_env(engine),
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
}

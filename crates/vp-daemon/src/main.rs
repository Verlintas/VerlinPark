use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
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
use vp_protocol::rpc::{RpcError, RpcNotification, RpcRequest, RpcResponse, codes};
use vp_protocol::types::{
    AddMcpServerParams, AddProjectParams, AgentListParams, AgentListResult, AgentMessageResult,
    AnswerQuestionParams, AnswerQuestionResult, ApprovalListResult, ArtifactBackfillParams,
    ArtifactGcParams, ArtifactGetParams, ArtifactListParams, ArtifactListResult,
    ArtifactMaterializeParams, ArtifactPolicyParams, ArtifactPolicyResult, ArtifactResult,
    AuthParams, AuthResult, CancelSessionParams, CancelSessionResult, CleanupWorktreesParams,
    CommitSessionParams, ConfigureAgentParams, CreateSessionParams, DiscardSessionParams,
    DoctorCheck, DoctorReport, EventListResult, IntegrationPolicyParams, IntegrationPolicyResult,
    LandSessionParams, ListApprovalsParams, ListEventsParams, ListSessionsParams, MailboxGcParams,
    MailboxListParams, MailboxListResult, MailboxPolicyParams, MailboxPolicyResult,
    MarkMessageReadParams, McpCallParams, McpGetPromptParams, McpReadResourceParams,
    McpServerIdParams, MemoryCreateParams, MemoryCreateResult, MemoryDeleteParams,
    MemoryListParams, MemoryPruneParams, MergeQueueListParams, MergeQueueListResult,
    OpenPullRequestParams, PingResult, ProjectBudgetParams, ProjectListResult, ProjectUsageParams,
    PromptParams, PromptResult, QuestionListParams, RehydrateSessionParams, ReplaySessionParams,
    ResolveApprovalParams, ResolveApprovalResult, ResolveMergeParams, ResolveMergeResult,
    RetryMergeParams, RetryMergeResult, SearchParams, ServerInfo, SessionDiffParams,
    SessionExportParams, SessionImportParams, SessionListResult, SessionLogParams,
    SessionPauseParams, SessionPauseResult, SessionPlanResumeParams, SessionPlanResumeResult,
    SessionResumeParams, SessionResumeResult, SessionTodosParams, SessionTodosResult,
    SessionTranscriptParams, SetArtifactPolicyParams, SetIntegrationPolicyParams,
    SetMailboxPolicyParams, SetProjectBudgetParams, SetShellPolicyParams, SetWorktreePolicyParams,
    ShellPolicyParams, ShellPolicyResult, SubscribeParams, TeamPlanParams, TeamPlanResult,
    UpdateMcpServerParams, UsageLedgerParams, WorktreePolicyParams,
};
use vp_protocol::{APP_VERSION, PROTOCOL_VERSION, now_rfc3339};
use vp_store::Store;
use vp_store::paths;

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
    if let Err(error) = engine.record_project_manifest().await {
        tracing::warn!(%error, "failed to write project manifest");
    }
    let agent = AgentService::from_env(engine.clone());
    match agent.resume_queued_sessions().await {
        Ok(0) => {}
        Ok(started) => tracing::info!(started, "re-enqueued persisted queued sessions"),
        Err(error) => tracing::warn!(%error, "failed to re-enqueue queued sessions"),
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
    spawn_worktree_sweeper(state.engine.clone());

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
        .route("/rpc", post(rpc_http))
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

async fn rpc_http(State(state): State<AppState>, headers: HeaderMap, body: String) -> Response {
    if !authorized(&headers, &state.token) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    }

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
            Ok(params) => match state
                .engine
                .create_session_with_limits(
                    params.project_id,
                    params.goal.clone(),
                    params.token_budget,
                    params.cost_budget_usd,
                    params.time_budget_secs,
                )
                .await
            {
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
            Ok(params) => match state
                .engine
                .add_mcp_server(
                    &params.name,
                    &params.command,
                    params.args,
                    params.env,
                    params.enabled,
                )
                .await
            {
                Ok(server) => RpcResponse::success(id, json!({"server": server})),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.server.update" => match parse_params::<UpdateMcpServerParams>(request.params) {
            Ok(params) => match state
                .engine
                .update_mcp_server(
                    &params.id,
                    &params.name,
                    &params.command,
                    params.args,
                    params.env,
                    params.enabled,
                )
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
        "mcp.server.remove" => match parse_params::<McpServerIdParams>(request.params) {
            Ok(params) => match state.engine.remove_mcp_server(&params.server_id).await {
                Ok(removed) => RpcResponse::success(id, json!({"removed": removed})),
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
                .list_memories(
                    &params.project_id,
                    params.query,
                    params.kind,
                    params.limit.unwrap_or(100),
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
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
            Ok(params) => match state.agent.start(&params.session_id, &params.content).await {
                Ok(()) => RpcResponse::success(id, PromptResult { accepted: true }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
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
                Ok(()) => RpcResponse::success(id, ResolveApprovalResult { resolved: true }),
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
            | "project.integration_policy.set"
            | "project.budget.set"
            | "project.worktree_policy.set"
            | "mailbox.policy.set"
            | "artifact.policy.set"
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

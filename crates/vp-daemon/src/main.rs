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
    AddProjectParams, ApprovalListResult, AuthParams, AuthResult, CommitSessionParams,
    ConfigureAgentParams, CreateSessionParams, EventListResult, ListApprovalsParams,
    ListEventsParams, ListSessionsParams, PingResult, ProjectListResult, PromptParams,
    PromptResult, ResolveApprovalParams, ResolveApprovalResult, ServerInfo, SessionDiffParams,
    SessionListResult, SubscribeParams,
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
    let agent = AgentService::from_env(engine.clone());
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
    match request.method.as_str() {
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
        "project.list" => match state.engine.list_projects().await {
            Ok(projects) => RpcResponse::success(id, ProjectListResult { projects }),
            Err(error) => internal_error(id, error),
        },
        "session.create" => match parse_params::<CreateSessionParams>(request.params) {
            Ok(params) => match state
                .engine
                .create_session(params.project_id, params.goal.clone())
                .await
            {
                Ok(session) => {
                    let model = state
                        .agent
                        .info()
                        .model
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
        "agent.info" => RpcResponse::success(id, state.agent.info()),
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
        "session.diff" => match parse_params::<SessionDiffParams>(request.params) {
            Ok(params) => match state.engine.session_diff(&params.session_id).await {
                Ok(diff) => RpcResponse::success(id, diff),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "agent.configure" => match parse_params::<ConfigureAgentParams>(request.params) {
            Ok(params) => match state.agent.configure(params) {
                Ok(info) => RpcResponse::success(id, info),
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
    }
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

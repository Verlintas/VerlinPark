use std::time::Duration;

use anyhow::{Context, Result};
use axum::Json;
use axum::body::to_bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tracing::{info, warn};
use vp_protocol::rpc::{RpcError, RpcNotification, RpcRequest, RpcResponse, codes};
use vp_protocol::types::{AuthParams, AuthResult, DaemonMetrics, SubscribeParams};
use vp_protocol::{APP_VERSION, PROTOCOL_VERSION};

use crate::AppState;
use crate::dispatch::dispatch;
use crate::metrics;
use crate::params::parse_params;
use crate::server::{authorized, constant_time_eq, server_info};
use crate::ws::{self, EventCursor};

pub(crate) async fn health() -> Json<Value> {
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

pub(crate) async fn gate_resolve_http(State(state): State<AppState>, request: Request) -> Response {
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
pub(crate) async fn metrics_http(State(state): State<AppState>, headers: HeaderMap) -> Response {
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

pub(crate) async fn collect_daemon_metrics(state: &AppState) -> Result<DaemonMetrics> {
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

pub(crate) async fn rpc_http(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Request,
) -> Response {
    if !authorized(&headers, &state.token) {
        // A JSON-RPC client may ignore the HTTP status; the body is a real
        // `RpcResponse` error so both transports agree on the error shape.
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            Json(RpcResponse::error(
                None,
                RpcError::new(codes::UNAUTHORIZED, "unauthorized"),
            )),
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

pub(crate) async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    // Explicit, documented bounds instead of axum's 64 MiB defaults. The
    // pre-auth frame additionally uses the much smaller `MAX_PREAUTH_FRAME_BYTES`
    // after the upgrade.
    ws.max_message_size(ws::MAX_WS_MESSAGE_BYTES)
        .max_frame_size(ws::MAX_WS_FRAME_BYTES)
        .on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: AppState) {
    let auth = tokio::time::timeout(Duration::from_secs(10), socket.recv()).await;
    let request = match auth {
        // Size-check before parsing: an unauthenticated peer must not get a
        // 64 MiB JSON DOM allocated on its behalf.
        Ok(Some(Ok(Message::Text(text)))) => match ws::parse_auth_frame(&text) {
            Ok(request) => Some(request),
            Err(error) => {
                let _ = send_json(&mut socket, &RpcResponse::error(None, error)).await;
                return;
            }
        },
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
    let mut cursor = EventCursor::new(None, None);

    'ws: loop {
        tokio::select! {
            outgoing = socket.recv() => {
                match outgoing {
                    Some(Ok(Message::Text(text))) => {
                        let request: RpcRequest =
                            match ws::parse_rpc_frame(&text, ws::MAX_WS_MESSAGE_BYTES) {
                                Ok(request) => request,
                                Err(error) => {
                                    let oversized = error.code == codes::INVALID_REQUEST;
                                    let _ = send_json(
                                        &mut socket,
                                        &RpcResponse::error(None, error),
                                    ).await;
                                    if oversized {
                                        // The documented bound is a hard limit; do not
                                        // keep reading frames from this peer.
                                        break 'ws;
                                    }
                                    continue;
                                }
                            };
                        if request.method == "event.subscribe" {
                            let params: SubscribeParams =
                                match parse_params::<SubscribeParams>(request.params) {
                                    Ok(params) => params,
                                    Err(error) => {
                                        let _ = send_json(
                                            &mut socket,
                                            &RpcResponse::error(request.id, error),
                                        ).await;
                                        continue;
                                    }
                                };
                            // A seq cursor is per session; a global subscriber
                            // has no cross-session cursor in the protocol.
                            if params.since_seq.is_some() && params.session_id.is_none() {
                                let _ = send_json(
                                    &mut socket,
                                    &RpcResponse::error(
                                        request.id,
                                        RpcError::with_data(
                                            codes::INVALID_PARAMS,
                                            "since_seq requires session_id; cross-session backfill is not supported",
                                            json!({
                                                "code": "EVENT_SUBSCRIBE_SESSION_REQUIRED",
                                                "retryable": false,
                                                "hint": "subscribe per session with since_seq, or call event.list to resync",
                                            }),
                                        ),
                                    ),
                                ).await;
                                continue;
                            }
                            if params.since_seq.is_some_and(|seq| seq > i64::MAX as u64) {
                                let _ = send_json(
                                    &mut socket,
                                    &RpcResponse::error(
                                        request.id,
                                        RpcError::with_data(
                                            codes::INVALID_PARAMS,
                                            "since_seq is outside the persisted event sequence range",
                                            json!({
                                                "code": "EVENT_SUBSCRIBE_SEQ_INVALID",
                                                "retryable": false,
                                                "hint": "use the last seq reported by event.appended or event.list",
                                            }),
                                        ),
                                    ),
                                ).await;
                                continue;
                            }
                            let mut replayed: Vec<vp_protocol::types::Event> = Vec::new();
                            let mut truncated = false;
                            if let (Some(session_id), Some(since_seq)) =
                                (params.session_id.clone(), params.since_seq)
                            {
                                match state
                                    .engine
                                    .list_events(
                                        session_id.clone(),
                                        since_seq.saturating_add(1),
                                        ws::MAX_REPLAY_EVENTS,
                                    )
                                    .await
                                {
                                    Ok(events) => {
                                        if events.len() as u32 >= ws::MAX_REPLAY_EVENTS
                                            && let Some(last) = events.last()
                                        {
                                            match state
                                                .engine
                                                .list_events(
                                                    session_id.clone(),
                                                    last.seq.saturating_add(1),
                                                    1,
                                                )
                                                .await
                                            {
                                                Ok(more) => truncated = !more.is_empty(),
                                                Err(error) => {
                                                    let _ = send_json(
                                                        &mut socket,
                                                        &RpcResponse::error(
                                                            request.id,
                                                            RpcError::new(
                                                                codes::INTERNAL_ERROR,
                                                                error.to_string(),
                                                            ),
                                                        ),
                                                    ).await;
                                                    continue;
                                                }
                                            }
                                        }
                                        replayed = events;
                                    }
                                    Err(error) => {
                                        let _ = send_json(
                                            &mut socket,
                                            &RpcResponse::error(
                                                request.id,
                                                RpcError::new(
                                                    codes::INTERNAL_ERROR,
                                                    error.to_string(),
                                                ),
                                            ),
                                        ).await;
                                        continue;
                                    }
                                }
                            }
                            let replayed_count = replayed.len();
                            cursor = EventCursor::new(params.session_id.clone(), params.since_seq);
                            if send_json(
                                &mut socket,
                                &RpcResponse::success(
                                    request.id,
                                    json!({
                                        "subscribed": true,
                                        "since_seq": params.since_seq,
                                        "replayed": replayed_count,
                                        "truncated": truncated,
                                    }),
                                ),
                            ).await.is_err()
                            {
                                break 'ws;
                            }
                            for event in &replayed {
                                if !cursor.observe(event) {
                                    continue;
                                }
                                let notification = RpcNotification::new("event.appended", event);
                                if send_json(&mut socket, &notification).await.is_err() {
                                    break 'ws;
                                }
                            }
                            if truncated {
                                // The client asked for a seq older than the
                                // bounded replay window; tell it to resync
                                // instead of pretending the stream is complete.
                                let gap = cursor.gap_notification(None, "backfill_truncated");
                                if send_json(&mut socket, &gap).await.is_err() {
                                    break 'ws;
                                }
                            }
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
                        if !cursor.observe(&event) {
                            continue;
                        }
                        let notification = RpcNotification::new("event.appended", &event);
                        if send_json(&mut socket, &notification).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        // Never drop state silently. The client receives the
                        // last seq it actually saw and a resync instruction;
                        // the audit event log remains the source of truth.
                        warn!(skipped, "websocket event subscriber lagged; sending event.gap");
                        let gap = cursor.gap_notification(Some(skipped), "lagged");
                        if send_json(&mut socket, &gap).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

async fn send_json(socket: &mut WebSocket, value: &impl serde::Serialize) -> Result<()> {
    let text = serde_json::to_string(value)?;
    socket
        .send(Message::Text(text.into()))
        .await
        .context("sending websocket message")
}

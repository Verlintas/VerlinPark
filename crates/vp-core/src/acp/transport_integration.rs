//! In-process ACP Streamable HTTP / WebSocket conformance harness.
//!
//! The server below implements the subset of the ACP RFD transport that
//! VerlinPark's client uses. It is intentionally test-only: it can inject
//! malformed frames, require cookies, and record forwarding so the client
//! adapter is exercised without a child process or a remote service.

use std::collections::HashMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_client_protocol::Client;
use agent_client_protocol::schema::{
    ContentBlock, InitializeRequest, NewSessionRequest, PromptRequest, ProtocolVersion, TextContent,
};
use axum::body::Bytes;
use axum::extract::ws::{Message as AxumWsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc};
use vp_protocol::types::AcpTransport;

use super::{AcpHost, AcpLaunch, AcpPermissionDecision, AcpPermissionRequest, AcpUpdate};
use crate::acp::{probe, probe_with_options, run_prompt};
use async_trait::async_trait;

const TEST_AUTH_ENV: &str = "VERLINPARK_TEST_ACP_HTTP_AUTH";
const TEST_AUTH_ENV_BAD: &str = "VERLINPARK_TEST_ACP_HTTP_AUTH_BAD";
const TEST_AUTH_ENV_WS: &str = "VERLINPARK_TEST_ACP_WS_AUTH";
const TEST_AUTH_ENV_WS_BAD: &str = "VERLINPARK_TEST_ACP_WS_AUTH_BAD";

/// Optional WebSocket extractor.
///
/// `Option<WebSocketUpgrade>` is not an axum extractor (it does not implement
/// `OptionalFromRequestParts`), so the harness inspects the upgrade header and
/// then delegates to the real extractor.
struct MaybeWebSocket(Option<WebSocketUpgrade>);

impl<S> FromRequestParts<S> for MaybeWebSocket
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let is_upgrade = parts
            .headers
            .get(header::UPGRADE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value
                    .split(',')
                    .any(|part| part.trim().eq_ignore_ascii_case("websocket"))
            });
        if !is_upgrade {
            return Ok(Self(None));
        }
        Ok(Self(
            WebSocketUpgrade::from_request_parts(parts, state)
                .await
                .ok(),
        ))
    }
}

#[derive(Clone, Default)]
pub(crate) struct TestServerConfig {
    expected_auth: Option<String>,
    expected_header: Option<(String, String)>,
    require_cookie: bool,
    load_session: bool,
    fail_session_stream: bool,
    malformed_connection_stream: bool,
    prompt_requires_permission: bool,
    prompt_streams_messages: bool,
    hold_prompt_until_cancel: bool,
    direct_session_new_response: bool,
    close_session_stream: bool,
    omit_connection_id_header: bool,
    omit_connection_id_body: bool,
    session_new_status: Option<u16>,
    prompt_status: Option<u16>,
    get_status: Option<u16>,
    delete_status: Option<u16>,
    ws_malformed: bool,
    ws_batch: bool,
    ws_close_after_session_new: bool,
}

#[derive(Clone)]
pub(crate) struct TestServer {
    config: TestServerConfig,
    inner: Arc<Mutex<TestInner>>,
    records: Arc<Mutex<Vec<RecordedRequest>>>,
    pub(crate) counters: Arc<TestCounters>,
}

#[derive(Default)]
pub(crate) struct TestCounters {
    deletes: AtomicUsize,
    pub(crate) prompts: AtomicUsize,
    permission_responses: AtomicUsize,
    initialize: AtomicUsize,
    session_new: AtomicUsize,
    session_load: AtomicUsize,
    cookie_requests: AtomicUsize,
    auth_requests: AtomicUsize,
    custom_header_requests: AtomicUsize,
}

#[derive(Debug, Clone)]
struct RecordedRequest {
    method: String,
    route: String,
    connection_id: Option<String>,
    session_id: Option<String>,
}

struct TestInner {
    connections: HashMap<String, ServerConnection>,
    next_connection: u64,
    next_session: u64,
}

struct ServerConnection {
    events: broadcast::Sender<String>,
    sessions: HashMap<String, ServerSession>,
}

struct ServerSession {
    events: broadcast::Sender<String>,
    commands: mpsc::UnboundedSender<SessionCommand>,
}

enum SessionCommand {
    Prompt { id: Value },
    PermissionResponse { value: Value },
    Cancel,
}

impl TestServer {
    fn new(config: TestServerConfig) -> Self {
        Self {
            config,
            inner: Arc::new(Mutex::new(TestInner {
                connections: HashMap::new(),
                next_connection: 1,
                next_session: 1,
            })),
            records: Arc::new(Mutex::new(Vec::new())),
            counters: Arc::new(TestCounters::default()),
        }
    }

    fn record(&self, record: RecordedRequest) {
        if let Ok(mut records) = self.records.lock() {
            records.push(record);
        }
    }

    fn records(&self) -> Vec<RecordedRequest> {
        self.records
            .lock()
            .map(|records| records.clone())
            .unwrap_or_default()
    }

    fn check_headers(&self, headers: &HeaderMap) -> Option<Response> {
        if let Some(expected) = &self.config.expected_auth {
            let actual = headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok());
            self.counters.auth_requests.fetch_add(1, Ordering::SeqCst);
            if actual != Some(expected.as_str()) {
                return Some((StatusCode::UNAUTHORIZED, "bad auth").into_response());
            }
        }
        if let Some((name, value)) = &self.config.expected_header {
            let actual = headers.get(name).and_then(|value| value.to_str().ok());
            self.counters
                .custom_header_requests
                .fetch_add(1, Ordering::SeqCst);
            if actual != Some(value.as_str()) {
                return Some((StatusCode::FORBIDDEN, "missing header").into_response());
            }
        }
        if self.config.require_cookie
            && headers
                .get(header::COOKIE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|cookie| cookie.contains("acp_test=1"))
        {
            self.counters.cookie_requests.fetch_add(1, Ordering::SeqCst);
        }
        None
    }

    fn require_cookie(&self, headers: &HeaderMap) -> Option<Response> {
        if !self.config.require_cookie {
            return None;
        }
        let has_cookie = headers
            .get(header::COOKIE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|cookie| cookie.contains("acp_test=1"));
        if has_cookie {
            self.counters.cookie_requests.fetch_add(1, Ordering::SeqCst);
            None
        } else {
            Some((StatusCode::UNAUTHORIZED, "missing cookie").into_response())
        }
    }
}

pub(crate) struct TestServerHandle {
    pub(crate) endpoint: String,
    pub(crate) server: TestServer,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl TestServerHandle {
    pub(crate) async fn start(config: TestServerConfig) -> Self {
        let server = TestServer::new(config);
        let app = Router::new()
            .route("/acp", post(post_acp).get(get_acp).delete(delete_acp))
            .with_state(server.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test ACP server");
        let address = listener.local_addr().expect("test server address");
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await;
        });
        Self {
            endpoint: format!("http://{address}/acp"),
            server,
            shutdown: Some(shutdown_tx),
            task,
        }
    }

    pub(crate) fn websocket_endpoint(&self) -> String {
        self.endpoint.replacen("http://", "ws://", 1)
    }
}

impl Drop for TestServerHandle {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.task.abort();
    }
}

async fn post_acp(State(server): State<TestServer>, headers: HeaderMap, body: Bytes) -> Response {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !content_type.starts_with("application/json") {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "expected application/json",
        )
            .into_response();
    }
    if let Some(response) = server.check_headers(&headers) {
        return response;
    }
    let body_value: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => return jsonrpc_bad_request("invalid JSON"),
    };
    if body_value.is_array() {
        return jsonrpc_bad_request("batches are not supported");
    }
    let method = body_value.get("method").and_then(Value::as_str);
    let id = body_value.get("id").cloned();
    let connection_id = headers
        .get("acp-connection-id")
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    let session_id = headers
        .get("acp-session-id")
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    if let Some(connection_id) = &connection_id {
        let inner = server.inner.lock().expect("server state");
        if !inner.connections.contains_key(connection_id) {
            return (StatusCode::NOT_FOUND, "unknown connection").into_response();
        }
    }

    server.record(RecordedRequest {
        method: method.unwrap_or("<response>").to_string(),
        route: "POST".into(),
        connection_id: connection_id.clone(),
        session_id: session_id.clone(),
    });

    if connection_id.is_none() {
        if method != Some("initialize") {
            return jsonrpc_bad_request("initialize must be the first request");
        }
        server.counters.initialize.fetch_add(1, Ordering::SeqCst);
        let (connection_id, events) = {
            let mut inner = server.inner.lock().expect("server state");
            let connection_id = format!("conn-{}", inner.next_connection);
            inner.next_connection += 1;
            let (events, _) = broadcast::channel(128);
            inner.connections.insert(
                connection_id.clone(),
                ServerConnection {
                    events: events.clone(),
                    sessions: HashMap::new(),
                },
            );
            (connection_id, events)
        };
        let _ = events;
        let mut result = json!({
            "protocolVersion": 1,
            "agentCapabilities": {"loadSession": server.config.load_session},
            "agentInfo": {"name": "test-acp-http", "version": "0.1.0"},
        });
        if !server.config.omit_connection_id_body {
            result["connectionId"] = json!(connection_id);
        }
        let mut response = Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        }))
        .into_response();
        if !server.config.omit_connection_id_header {
            response.headers_mut().insert(
                "acp-connection-id",
                HeaderValue::from_str(&connection_id).expect("connection id header"),
            );
        }
        response.headers_mut().insert(
            header::SET_COOKIE,
            HeaderValue::from_static("acp_test=1; Path=/; HttpOnly"),
        );
        return response;
    }

    let connection_id = connection_id.expect("connection id checked");
    if session_id.is_some()
        && let Some(response) = server.require_cookie(&headers)
    {
        let _ = response;
        return (StatusCode::UNAUTHORIZED, "missing cookie").into_response();
    }

    match method {
        Some("session/new") => {
            server.counters.session_new.fetch_add(1, Ordering::SeqCst);
            if let Some(status) = server.config.session_new_status {
                return (
                    StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                    "session/new rejected",
                )
                    .into_response();
            }
            let (session_id, session_events, connection_events) = {
                let mut inner = server.inner.lock().expect("server state");
                let session_id = format!("sess-{}", inner.next_session);
                inner.next_session += 1;
                let Some(connection) = inner.connections.get_mut(&connection_id) else {
                    return (StatusCode::NOT_FOUND, "unknown connection").into_response();
                };
                let (events, _) = broadcast::channel(128);
                let (commands, command_rx) = mpsc::unbounded_channel();
                connection.sessions.insert(
                    session_id.clone(),
                    ServerSession {
                        events: events.clone(),
                        commands,
                    },
                );
                let connection_events = connection.events.clone();
                tokio::spawn(run_session_actor(
                    session_id.clone(),
                    server.config.clone(),
                    server.counters.clone(),
                    command_rx,
                    events.clone(),
                ));
                (session_id, events, connection_events)
            };
            let response = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {"sessionId": session_id}
            });
            if server.config.direct_session_new_response {
                return Json(response).into_response();
            }
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(5)).await;
                let _ = connection_events.send(response.to_string());
            });
            let _ = session_events;
            StatusCode::ACCEPTED.into_response()
        }
        Some("session/load") => {
            let Some(session_id) = session_id.clone() else {
                return jsonrpc_bad_request("session/load requires Acp-Session-Id");
            };
            if !server.config.load_session {
                return jsonrpc_bad_request("session/load is not supported");
            }
            server.counters.session_load.fetch_add(1, Ordering::SeqCst);
            let connection_events = {
                let mut inner = server.inner.lock().expect("server state");
                let Some(connection) = inner.connections.get_mut(&connection_id) else {
                    return (StatusCode::NOT_FOUND, "unknown connection").into_response();
                };
                if !connection.sessions.contains_key(&session_id) {
                    let (events, _) = broadcast::channel(128);
                    let (commands, command_rx) = mpsc::unbounded_channel();
                    connection.sessions.insert(
                        session_id.clone(),
                        ServerSession {
                            events: events.clone(),
                            commands,
                        },
                    );
                    tokio::spawn(run_session_actor(
                        session_id.clone(),
                        server.config.clone(),
                        server.counters.clone(),
                        command_rx,
                        events,
                    ));
                }
                connection.events.clone()
            };
            let response = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {"sessionId": session_id}
            });
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(5)).await;
                let _ = connection_events.send(response.to_string());
            });
            StatusCode::ACCEPTED.into_response()
        }
        Some("session/prompt") => {
            let Some(session_id) = session_id.clone() else {
                return jsonrpc_bad_request("session/prompt requires Acp-Session-Id");
            };
            if let Some(status) = server.config.prompt_status {
                return (
                    StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                    "prompt rejected",
                )
                    .into_response();
            }
            server.counters.prompts.fetch_add(1, Ordering::SeqCst);
            let commands = {
                let inner = server.inner.lock().expect("server state");
                let Some(connection) = inner.connections.get(&connection_id) else {
                    return (StatusCode::NOT_FOUND, "unknown connection").into_response();
                };
                let Some(session) = connection.sessions.get(&session_id) else {
                    return (StatusCode::NOT_FOUND, "unknown session").into_response();
                };
                session.commands.clone()
            };
            if commands
                .send(SessionCommand::Prompt {
                    id: id.clone().unwrap_or(Value::Null),
                })
                .is_err()
            {
                return (StatusCode::GONE, "session actor stopped").into_response();
            }
            StatusCode::ACCEPTED.into_response()
        }
        Some("session/cancel") => {
            let Some(session_id) = session_id.clone() else {
                return jsonrpc_bad_request("session/cancel requires Acp-Session-Id");
            };
            let commands = {
                let inner = server.inner.lock().expect("server state");
                let Some(connection) = inner.connections.get(&connection_id) else {
                    return (StatusCode::NOT_FOUND, "unknown connection").into_response();
                };
                let Some(session) = connection.sessions.get(&session_id) else {
                    return (StatusCode::NOT_FOUND, "unknown session").into_response();
                };
                session.commands.clone()
            };
            let _ = commands.send(SessionCommand::Cancel);
            StatusCode::ACCEPTED.into_response()
        }
        None => {
            // Client response to a server-initiated request.
            let Some(session_id) = session_id.clone() else {
                return jsonrpc_bad_request("response requires Acp-Session-Id");
            };
            let commands = {
                let inner = server.inner.lock().expect("server state");
                let Some(connection) = inner.connections.get(&connection_id) else {
                    return (StatusCode::NOT_FOUND, "unknown connection").into_response();
                };
                let Some(session) = connection.sessions.get(&session_id) else {
                    return (StatusCode::NOT_FOUND, "unknown session").into_response();
                };
                session.commands.clone()
            };
            server
                .counters
                .permission_responses
                .fetch_add(1, Ordering::SeqCst);
            let _ = commands.send(SessionCommand::PermissionResponse {
                value: body_value.get("result").cloned().unwrap_or(Value::Null),
            });
            StatusCode::ACCEPTED.into_response()
        }
        Some(method) => {
            server.record(RecordedRequest {
                method: method.to_string(),
                route: "POST".into(),
                connection_id: Some(connection_id),
                session_id,
            });
            jsonrpc_bad_request("unsupported method")
        }
    }
}

fn jsonrpc_bad_request(message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": {"code": -32600, "message": message}
        })),
    )
        .into_response()
}

async fn get_acp(
    State(server): State<TestServer>,
    headers: HeaderMap,
    upgrade: MaybeWebSocket,
) -> Response {
    if let Some(response) = server.check_headers(&headers) {
        return response;
    }
    if let Some(upgrade) = upgrade.0 {
        let connection_id = {
            let mut inner = server.inner.lock().expect("server state");
            let connection_id = format!("ws-conn-{}", inner.next_connection);
            inner.next_connection += 1;
            connection_id
        };
        server.record(RecordedRequest {
            method: "UPGRADE".into(),
            route: "GET".into(),
            connection_id: Some(connection_id.clone()),
            session_id: None,
        });
        let config = server.config.clone();
        let counters = server.counters.clone();
        let mut response = upgrade.on_upgrade(move |socket| async move {
            run_websocket_actor(socket, config, counters).await;
        });
        response.headers_mut().insert(
            "acp-connection-id",
            HeaderValue::from_str(&connection_id).expect("ws connection id header"),
        );
        return response;
    }

    if let Some(status) = server.config.get_status {
        return (
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            "stream rejected",
        )
            .into_response();
    }
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !accept.contains("text/event-stream") {
        return (StatusCode::NOT_ACCEPTABLE, "expected text/event-stream").into_response();
    }
    if let Some(response) = server.require_cookie(&headers) {
        return response;
    }
    let Some(connection_id) = headers
        .get("acp-connection-id")
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
    else {
        return (StatusCode::BAD_REQUEST, "missing Acp-Connection-Id").into_response();
    };
    let session_id = headers
        .get("acp-session-id")
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    if session_id.is_some() && server.config.fail_session_stream {
        return (StatusCode::NOT_FOUND, "unknown session").into_response();
    }
    let receiver = {
        let mut inner = server.inner.lock().expect("server state");
        let Some(connection) = inner.connections.get_mut(&connection_id) else {
            return (StatusCode::NOT_FOUND, "unknown connection").into_response();
        };
        if let Some(session_id) = &session_id {
            if !connection.sessions.contains_key(session_id) {
                if server.config.load_session {
                    let (events, _) = broadcast::channel(128);
                    let (commands, command_rx) = mpsc::unbounded_channel();
                    connection.sessions.insert(
                        session_id.clone(),
                        ServerSession {
                            events: events.clone(),
                            commands,
                        },
                    );
                    tokio::spawn(run_session_actor(
                        session_id.clone(),
                        server.config.clone(),
                        server.counters.clone(),
                        command_rx,
                        events.clone(),
                    ));
                } else {
                    return (StatusCode::NOT_FOUND, "unknown session").into_response();
                }
            }
            connection
                .sessions
                .get(session_id)
                .expect("session exists")
                .events
                .subscribe()
        } else {
            connection.events.subscribe()
        }
    };
    server.record(RecordedRequest {
        method: "GET".into(),
        route: "SSE".into(),
        connection_id: Some(connection_id),
        session_id: session_id.clone(),
    });
    if server.config.close_session_stream && session_id.is_some() {
        let empty = tokio_stream::iter(Vec::<Result<Event, Infallible>>::new());
        return Sse::new(empty).into_response();
    }
    if server.config.malformed_connection_stream && session_id_is_none(&headers) {
        drop(receiver);
        // Emit a malformed event immediately; the client must fail closed.
        let malformed =
            tokio_stream::iter(vec![Ok::<_, Infallible>(Event::default().data("not-json"))]);
        return Sse::new(malformed).into_response();
    }
    let stream =
        tokio_stream::wrappers::BroadcastStream::new(receiver).filter_map(|message| async move {
            message
                .ok()
                .map(|text| Ok::<_, Infallible>(Event::default().event("message").data(text)))
        });
    Sse::new(stream).into_response()
}

fn session_id_is_none(headers: &HeaderMap) -> bool {
    headers.get("acp-session-id").is_none()
}

async fn delete_acp(State(server): State<TestServer>, headers: HeaderMap) -> Response {
    if let Some(response) = server.check_headers(&headers) {
        return response;
    }
    let Some(connection_id) = headers
        .get("acp-connection-id")
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
    else {
        return (StatusCode::BAD_REQUEST, "missing Acp-Connection-Id").into_response();
    };
    let removed = {
        let mut inner = server.inner.lock().expect("server state");
        inner.connections.remove(&connection_id).is_some()
    };
    if !removed {
        return (StatusCode::NOT_FOUND, "unknown connection").into_response();
    }
    server.counters.deletes.fetch_add(1, Ordering::SeqCst);
    if let Some(status) = server.config.delete_status {
        return (
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            "delete rejected",
        )
            .into_response();
    }
    server.record(RecordedRequest {
        method: "DELETE".into(),
        route: "DELETE".into(),
        connection_id: Some(connection_id),
        session_id: None,
    });
    StatusCode::ACCEPTED.into_response()
}

async fn run_session_actor(
    session_id: String,
    config: TestServerConfig,
    counters: Arc<TestCounters>,
    mut commands: mpsc::UnboundedReceiver<SessionCommand>,
    events: broadcast::Sender<String>,
) {
    let mut permission_seq = 0_u64;
    while let Some(command) = commands.recv().await {
        match command {
            SessionCommand::Prompt { id } => {
                let _ = events.send(notification_text(
                    &session_id,
                    if config.hold_prompt_until_cancel {
                        "started"
                    } else {
                        "working "
                    },
                ));
                if config.prompt_requires_permission {
                    permission_seq += 1;
                    let permission_id = format!("perm-{permission_seq}");
                    let _ = events.send(permission_request(&session_id, &permission_id));
                    let approved = loop {
                        match commands.recv().await {
                            Some(SessionCommand::PermissionResponse { value, .. }) => {
                                let option = value
                                    .get("outcome")
                                    .and_then(|outcome| outcome.get("optionId"))
                                    .and_then(Value::as_str)
                                    .unwrap_or_default();
                                break option == "allow";
                            }
                            Some(SessionCommand::Cancel) => {
                                let _ = events
                                    .send(response_text(&id, json!({"stopReason": "cancelled"})));
                                break false;
                            }
                            Some(SessionCommand::Prompt { .. }) => {}
                            None => return,
                        }
                    };
                    let _ = events.send(notification_text(
                        &session_id,
                        if approved { "approved " } else { "denied " },
                    ));
                }
                if config.hold_prompt_until_cancel {
                    loop {
                        match commands.recv().await {
                            Some(SessionCommand::Cancel) => {
                                let _ = events
                                    .send(response_text(&id, json!({"stopReason": "cancelled"})));
                                break;
                            }
                            Some(SessionCommand::PermissionResponse { .. })
                            | Some(SessionCommand::Prompt { .. }) => {}
                            None => return,
                        }
                    }
                    continue;
                }
                if config.prompt_streams_messages {
                    let _ = events.send(notification_text(&session_id, "still working "));
                }
                let _ = events.send(tool_call_text(&session_id));
                let _ = events.send(notification_text(&session_id, "done"));
                let _ = events.send(response_text(&id, json!({"stopReason": "end_turn"})));
                counters.prompts.fetch_add(0, Ordering::SeqCst);
            }
            SessionCommand::Cancel => {}
            SessionCommand::PermissionResponse { .. } => {}
        }
    }
}

fn notification_text(session_id: &str, text: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": session_id,
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": text}
            }
        }
    })
    .to_string()
}

fn tool_call_text(session_id: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": session_id,
            "update": {
                "sessionUpdate": "tool_call",
                "toolCallId": "tool-1",
                "title": "test tool",
                "kind": "edit",
                "status": "completed"
            }
        }
    })
    .to_string()
}

fn permission_request(session_id: &str, permission_id: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": permission_id,
        "method": "session/request_permission",
        "params": {
            "sessionId": session_id,
            "toolCall": {
                "toolCallId": "tool-1",
                "title": "Write a file",
                "kind": "edit",
                "rawInput": {"path": "acp.txt"}
            },
            "options": [
                {"optionId": "allow", "name": "Allow", "kind": "allow_once"},
                {"optionId": "deny", "name": "Reject", "kind": "reject_once"}
            ]
        }
    })
    .to_string()
}

fn response_text(id: &Value, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

async fn run_websocket_actor(
    mut socket: WebSocket,
    config: TestServerConfig,
    counters: Arc<TestCounters>,
) {
    let mut session_id: Option<String> = None;
    let mut pending_prompt: Option<Value> = None;
    let mut pending_permission: Option<String> = None;
    while let Some(message) = socket.recv().await {
        let Ok(message) = message else {
            break;
        };
        match message {
            AxumWsMessage::Text(text) => {
                let Ok(value) = serde_json::from_str::<Value>(&text) else {
                    let _ = socket
                        .send(AxumWsMessage::Text(
                            json!({
                                "jsonrpc": "2.0",
                                "id": null,
                                "error": {"code": -32700, "message": "parse error"}
                            })
                            .to_string()
                            .into(),
                        ))
                        .await;
                    continue;
                };
                if value.is_array() {
                    let _ = socket
                        .send(AxumWsMessage::Text(
                            json!({
                                "jsonrpc": "2.0",
                                "id": null,
                                "error": {"code": -32600, "message": "batches are not supported"}
                            })
                            .to_string()
                            .into(),
                        ))
                        .await;
                    continue;
                }
                match value.get("method").and_then(Value::as_str) {
                    Some("initialize") => {
                        counters.initialize.fetch_add(1, Ordering::SeqCst);
                        counters.prompts.fetch_add(0, Ordering::SeqCst);
                        let _ = socket
                            .send(AxumWsMessage::Text(
                                json!({
                                    "jsonrpc": "2.0",
                                    "id": value.get("id").cloned().unwrap_or(Value::Null),
                                    "result": {
                                        "protocolVersion": 1,
                                        "agentCapabilities": {"loadSession": config.load_session},
                                        "agentInfo": {"name": "test-acp-ws", "version": "0.1.0"}
                                    }
                                })
                                .to_string()
                                .into(),
                            ))
                            .await;
                        if config.ws_malformed {
                            let _ = socket.send(AxumWsMessage::Text("not-json".into())).await;
                            continue;
                        }
                        if config.ws_batch {
                            let _ = socket.send(AxumWsMessage::Text("[]".into())).await;
                            continue;
                        }
                        // A binary frame must be ignored by the client.
                        let _ = socket
                            .send(AxumWsMessage::Binary(vec![1, 2, 3].into()))
                            .await;
                    }
                    Some("session/load") => {
                        let Some(sid) = value
                            .get("params")
                            .and_then(|params| params.get("sessionId"))
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned)
                        else {
                            continue;
                        };
                        if !config.load_session {
                            let _ = socket
                                .send(AxumWsMessage::Text(
                                    json!({
                                        "jsonrpc": "2.0",
                                        "id": value.get("id").cloned().unwrap_or(Value::Null),
                                        "error": {"code": -32601, "message": "session/load is not supported"}
                                    })
                                    .to_string()
                                    .into(),
                                ))
                                .await;
                            continue;
                        }
                        session_id = Some(sid.clone());
                        let _ = socket
                            .send(AxumWsMessage::Text(
                                json!({
                                    "jsonrpc": "2.0",
                                    "id": value.get("id").cloned().unwrap_or(Value::Null),
                                    "result": {"sessionId": sid}
                                })
                                .to_string()
                                .into(),
                            ))
                            .await;
                    }
                    Some("session/new") => {
                        counters.session_new.fetch_add(1, Ordering::SeqCst);
                        let id = format!("ws-sess-{}", counters.session_new.load(Ordering::SeqCst));
                        session_id = Some(id.clone());
                        let _ = socket
                            .send(AxumWsMessage::Text(
                                json!({
                                    "jsonrpc": "2.0",
                                    "id": value.get("id").cloned().unwrap_or(Value::Null),
                                    "result": {"sessionId": id}
                                })
                                .to_string()
                                .into(),
                            ))
                            .await;
                        if config.ws_close_after_session_new {
                            let _ = socket.send(AxumWsMessage::Close(None)).await;
                            break;
                        }
                    }
                    Some("session/prompt") => {
                        counters.prompts.fetch_add(1, Ordering::SeqCst);
                        let Some(sid) = session_id.clone() else {
                            continue;
                        };
                        pending_prompt = value.get("id").cloned();
                        let _ = socket
                            .send(AxumWsMessage::Text(
                                notification_text(&sid, "started").into(),
                            ))
                            .await;
                        if config.hold_prompt_until_cancel {
                            continue;
                        }
                        if config.prompt_requires_permission {
                            pending_permission = Some("perm-ws-1".into());
                            let _ = socket
                                .send(AxumWsMessage::Text(
                                    permission_request(&sid, "perm-ws-1").into(),
                                ))
                                .await;
                        } else if let Some(id) = pending_prompt.take() {
                            let _ = socket
                                .send(AxumWsMessage::Text(tool_call_text(&sid).into()))
                                .await;
                            let _ = socket
                                .send(AxumWsMessage::Text(notification_text(&sid, "done").into()))
                                .await;
                            let _ = socket
                                .send(AxumWsMessage::Text(
                                    response_text(&id, json!({"stopReason": "end_turn"})).into(),
                                ))
                                .await;
                        }
                    }
                    Some("session/cancel") => {
                        if let Some(id) = pending_prompt.take() {
                            let _ = socket
                                .send(AxumWsMessage::Text(
                                    response_text(&id, json!({"stopReason": "cancelled"})).into(),
                                ))
                                .await;
                        }
                    }
                    None => {
                        counters.permission_responses.fetch_add(1, Ordering::SeqCst);
                        if pending_permission.is_some() {
                            pending_permission = None;
                            if let Some(id) = pending_prompt.take() {
                                let _ = socket
                                    .send(AxumWsMessage::Text(
                                        notification_text(
                                            session_id.as_deref().unwrap_or_default(),
                                            "approved",
                                        )
                                        .into(),
                                    ))
                                    .await;
                                let _ = socket
                                    .send(AxumWsMessage::Text(
                                        response_text(&id, json!({"stopReason": "end_turn"}))
                                            .into(),
                                    ))
                                    .await;
                            }
                        }
                    }
                    Some(_) => {
                        let _ = socket
                            .send(AxumWsMessage::Text(
                                json!({
                                    "jsonrpc": "2.0",
                                    "id": value.get("id").cloned().unwrap_or(Value::Null),
                                    "error": {"code": -32601, "message": "method not found"}
                                })
                                .to_string()
                                .into(),
                            ))
                            .await;
                    }
                }
            }
            AxumWsMessage::Binary(_) => {
                // Binary frames are ignored by the client; the server accepting
                // one here just proves the test harness can send them.
            }
            AxumWsMessage::Close(_) => break,
            AxumWsMessage::Ping(_) | AxumWsMessage::Pong(_) => {}
        }
    }
}

#[derive(Default)]
struct RecordingHost {
    messages: Mutex<String>,
    permissions: AtomicUsize,
    tool_calls: AtomicUsize,
    started: Mutex<Option<tokio::sync::mpsc::UnboundedSender<()>>>,
}

#[async_trait]
impl AcpHost for RecordingHost {
    async fn update(&self, update: AcpUpdate) {
        match update {
            AcpUpdate::Message(text) => {
                if let Ok(mut messages) = self.messages.lock() {
                    messages.push_str(&text);
                }
                if text.contains("started")
                    && let Some(sender) = self.started.lock().ok().and_then(|value| value.clone())
                {
                    let _ = sender.send(());
                }
            }
            AcpUpdate::ToolCall { .. } => {
                self.tool_calls.fetch_add(1, Ordering::SeqCst);
            }
            _ => {}
        }
    }

    async fn permission(&self, _request: AcpPermissionRequest) -> AcpPermissionDecision {
        self.permissions.fetch_add(1, Ordering::SeqCst);
        AcpPermissionDecision::Approve
    }
}

fn remote_launch(endpoint: &str, transport: AcpTransport) -> AcpLaunch {
    AcpLaunch {
        name: "http-test".into(),
        command: "/definitely/not/spawned".into(),
        args: vec!["--should-not-run".into()],
        env: Default::default(),
        cwd: PathBuf::from("/tmp/verlinpark-acp-http"),
        project_root: PathBuf::from("/tmp/verlinpark-acp-http"),
        sandbox_root: PathBuf::from("/tmp/verlinpark-acp-http-sandbox"),
        secret_roots: Vec::new(),
        network_allowed: true,
        home: PathBuf::from("/tmp"),
        restrict_home: false,
        read_jail: false,
        git_branch: None,
        mcp_servers: Vec::new(),
        timeout: Duration::from_secs(30),
        transport,
        endpoint: Some(endpoint.into()),
        auth_header_env: None,
        headers: Default::default(),
        connect_timeout: Duration::from_secs(5),
        allow_remote: true,
        resume_session_id: None,
        diagnostics: None,
    }
}

async fn run_remote(
    endpoint: &str,
    transport: AcpTransport,
    host: Arc<RecordingHost>,
) -> anyhow::Result<super::AcpRunOutcome> {
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    run_prompt(
        remote_launch(endpoint, transport),
        "do the task".into(),
        host,
        cancel_rx,
    )
    .await
}

#[tokio::test]
async fn streamable_http_runs_a_prompt_with_sse_routing_and_permission() {
    let handle = TestServerHandle::start(TestServerConfig {
        prompt_requires_permission: true,
        prompt_streams_messages: true,
        ..Default::default()
    })
    .await;
    let host = Arc::new(RecordingHost::default());
    let outcome = tokio::time::timeout(
        Duration::from_secs(20),
        run_remote(&handle.endpoint, AcpTransport::StreamableHttp, host.clone()),
    )
    .await
    .expect("prompt timed out")
    .expect("prompt failed");
    assert_eq!(outcome.stop_reason, "end_turn");
    assert_eq!(outcome.transport, AcpTransport::StreamableHttp);
    assert_eq!(outcome.agent_name, "test-acp-http");
    assert_eq!(host.permissions.load(Ordering::SeqCst), 1);
    assert_eq!(host.tool_calls.load(Ordering::SeqCst), 1);
    let messages = {
        let messages = host.messages.lock().expect("messages");
        messages.clone()
    };
    assert!(messages.contains("working"), "{messages}");
    assert!(messages.contains("approved"), "{messages}");
    assert!(messages.contains("done"), "{messages}");
    assert!(handle.server.counters.initialize.load(Ordering::SeqCst) >= 1);
    assert_eq!(handle.server.counters.session_new.load(Ordering::SeqCst), 1);
    let records = handle.server.records();
    assert!(
        records
            .iter()
            .any(|record| record.route == "POST" && record.connection_id.is_some()),
        "POSTs after initialize must carry Acp-Connection-Id: {records:?}"
    );
    assert!(
        records
            .iter()
            .any(|record| record.method == "session/prompt" && record.session_id.is_some()),
        "prompt POST must carry Acp-Session-Id: {records:?}"
    );
    assert!(
        records
            .iter()
            .any(|record| record.method == "GET" && record.session_id.is_some()),
        "session GET must carry Acp-Session-Id: {records:?}"
    );
    // The transport deletes the connection during shutdown. Allow the
    // detached cleanup task to run.
    for _ in 0..100 {
        if handle.server.counters.deletes.load(Ordering::SeqCst) > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        handle.server.counters.deletes.load(Ordering::SeqCst) > 0,
        "connection was not DELETEd"
    );
}

#[tokio::test]
async fn streamable_http_forwards_cookies_and_explicit_headers() {
    unsafe {
        std::env::set_var(TEST_AUTH_ENV, "Bearer integration-token");
    }
    let handle = TestServerHandle::start(TestServerConfig {
        expected_auth: Some("Bearer integration-token".into()),
        expected_header: Some(("x-project".into(), "demo".into())),
        require_cookie: true,
        ..Default::default()
    })
    .await;
    let mut launch = remote_launch(&handle.endpoint, AcpTransport::StreamableHttp);
    launch.auth_header_env = Some(TEST_AUTH_ENV.into());
    launch.headers = std::collections::BTreeMap::from([("x-project".into(), "demo".into())]);
    let host = Arc::new(RecordingHost::default());
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let outcome = tokio::time::timeout(
        Duration::from_secs(20),
        run_prompt(launch, "cookies".into(), host, cancel_rx),
    )
    .await
    .expect("prompt timed out")
    .expect("prompt failed");
    assert_eq!(outcome.stop_reason, "end_turn");
    assert!(handle.server.counters.auth_requests.load(Ordering::SeqCst) >= 3);
    assert!(
        handle
            .server
            .counters
            .custom_header_requests
            .load(Ordering::SeqCst)
            >= 3
    );
    assert!(
        handle
            .server
            .counters
            .cookie_requests
            .load(Ordering::SeqCst)
            >= 2,
        "cookie jar did not forward the initialize cookie"
    );
    unsafe {
        std::env::remove_var(TEST_AUTH_ENV);
    }
}

#[tokio::test]
async fn streamable_http_rejects_wrong_authentication_without_echoing_it() {
    unsafe {
        std::env::set_var(TEST_AUTH_ENV_BAD, "Bearer wrong");
    }
    let handle = TestServerHandle::start(TestServerConfig {
        expected_auth: Some("Bearer right".into()),
        ..Default::default()
    })
    .await;
    let mut launch = remote_launch(&handle.endpoint, AcpTransport::StreamableHttp);
    launch.auth_header_env = Some(TEST_AUTH_ENV_BAD.into());
    let host = Arc::new(RecordingHost::default());
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let error = tokio::time::timeout(
        Duration::from_secs(20),
        run_prompt(launch, "auth".into(), host, cancel_rx),
    )
    .await
    .expect("prompt timed out")
    .expect_err("wrong auth must fail");
    let text = error.to_string();
    assert!(text.contains("401"), "{text}");
    assert!(!text.contains("Bearer wrong"), "secret leaked: {text}");
    unsafe {
        std::env::remove_var(TEST_AUTH_ENV_BAD);
    }
}

#[tokio::test]
async fn streamable_http_rejects_a_malformed_sse_event() {
    let handle = TestServerHandle::start(TestServerConfig {
        malformed_connection_stream: true,
        ..Default::default()
    })
    .await;
    let host = Arc::new(RecordingHost::default());
    let error = tokio::time::timeout(
        Duration::from_secs(20),
        run_remote(&handle.endpoint, AcpTransport::StreamableHttp, host),
    )
    .await
    .expect("prompt timed out")
    .expect_err("malformed SSE must fail");
    let text = error.to_string();
    assert!(
        text.contains("malformed") || text.contains("invalid"),
        "{text}"
    );
}

#[tokio::test]
async fn streamable_http_reports_a_connect_timeout() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind silent server");
    let address = listener.local_addr().expect("silent server address");
    // Accept the TCP connection but never answer the HTTP request.
    let accept = tokio::spawn(async move {
        if let Ok((socket, _)) = listener.accept().await {
            tokio::time::sleep(Duration::from_secs(30)).await;
            drop(socket);
        }
    });
    let mut launch = remote_launch(
        &format!("http://{address}/acp"),
        AcpTransport::StreamableHttp,
    );
    launch.connect_timeout = Duration::from_secs(1);
    let host = Arc::new(RecordingHost::default());
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let error = tokio::time::timeout(
        Duration::from_secs(10),
        run_prompt(launch, "timeout".into(), host, cancel_rx),
    )
    .await
    .expect("client did not honor the connect timeout")
    .expect_err("silent server must fail");
    let text = error.to_string().to_ascii_lowercase();
    assert!(
        text.contains("timed out") || text.contains("timeout"),
        "{text}"
    );
    accept.abort();
}

#[tokio::test]
async fn streamable_http_reports_an_unknown_session_stream() {
    let handle = TestServerHandle::start(TestServerConfig {
        fail_session_stream: true,
        ..Default::default()
    })
    .await;
    let host = Arc::new(RecordingHost::default());
    let error = tokio::time::timeout(
        Duration::from_secs(20),
        run_remote(&handle.endpoint, AcpTransport::StreamableHttp, host),
    )
    .await
    .expect("prompt timed out")
    .expect_err("unknown session stream must fail");
    let text = error.to_string();
    assert!(text.contains("404") || text.contains("session"), "{text}");
}

#[tokio::test]
async fn one_http_connection_hosts_multiple_sessions() {
    let handle = TestServerHandle::start(TestServerConfig::default()).await;
    let config = crate::acp::RemoteTransportConfig::from_launch(&remote_launch(
        &handle.endpoint,
        AcpTransport::StreamableHttp,
    ))
    .expect("transport config");
    let transport = crate::acp::RemoteTransport::new(config);
    let result = Client
        .builder()
        .connect_with(transport, async |connection| {
            let initialized = connection
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;
            assert_eq!(initialized.protocol_version, ProtocolVersion::V1);
            let first = connection
                .send_request(NewSessionRequest::new("/tmp/first"))
                .block_task()
                .await?;
            let second = connection
                .send_request(NewSessionRequest::new("/tmp/second"))
                .block_task()
                .await?;
            assert_ne!(first.session_id, second.session_id);
            let first_prompt = connection
                .send_request(PromptRequest::new(
                    first.session_id,
                    vec![ContentBlock::Text(TextContent::new("first".to_string()))],
                ))
                .block_task()
                .await?;
            let second_prompt = connection
                .send_request(PromptRequest::new(
                    second.session_id,
                    vec![ContentBlock::Text(TextContent::new("second".to_string()))],
                ))
                .block_task()
                .await?;
            assert_eq!(first_prompt.stop_reason, second_prompt.stop_reason);
            Ok::<(), agent_client_protocol::Error>(())
        })
        .await;
    result.expect("multi-session connection");
    assert_eq!(handle.server.counters.session_new.load(Ordering::SeqCst), 2);
    let records = handle.server.records();
    let session_streams = records
        .iter()
        .filter(|record| record.method == "GET" && record.session_id.is_some())
        .count();
    assert_eq!(
        session_streams, 2,
        "one session stream per session: {records:?}"
    );
}

#[tokio::test]
async fn streamable_http_probe_reports_transport_and_capabilities() {
    let handle = TestServerHandle::start(TestServerConfig {
        load_session: true,
        ..Default::default()
    })
    .await;
    let outcome = tokio::time::timeout(
        Duration::from_secs(20),
        probe(remote_launch(
            &handle.endpoint,
            AcpTransport::StreamableHttp,
        )),
    )
    .await
    .expect("probe timed out")
    .expect("probe failed");
    assert_eq!(outcome.transport, AcpTransport::StreamableHttp);
    assert_eq!(outcome.agent_name, "test-acp-http");
    assert_eq!(outcome.protocol_version, 1);
    assert!(outcome.session_created);
    assert!(outcome.load_session);
    assert!(outcome.capabilities.load_session);
    assert_eq!(outcome.http_version.as_deref(), Some("HTTP/1.1"));
    assert!(outcome.messages_sent >= 2);
    assert!(outcome.messages_received >= 1);
    assert!(outcome.streams_opened >= 1);
}

#[tokio::test]
async fn probe_can_skip_session_creation() {
    let handle = TestServerHandle::start(TestServerConfig::default()).await;
    let outcome = probe_with_options(
        remote_launch(&handle.endpoint, AcpTransport::StreamableHttp),
        false,
    )
    .await
    .expect("transport-only probe");
    assert_eq!(outcome.agent_name, "test-acp-http");
    assert!(!outcome.session_created);
    assert_eq!(handle.server.counters.session_new.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn streamable_http_resumes_a_session_with_session_load() {
    let handle = TestServerHandle::start(TestServerConfig {
        load_session: true,
        ..Default::default()
    })
    .await;
    let host = Arc::new(RecordingHost::default());
    let first = run_remote(&handle.endpoint, AcpTransport::StreamableHttp, host.clone())
        .await
        .expect("initial prompt failed");
    assert!(!first.resumed);
    let mut launch = remote_launch(&handle.endpoint, AcpTransport::StreamableHttp);
    launch.resume_session_id = Some(first.session_id.clone());
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let second = tokio::time::timeout(
        Duration::from_secs(20),
        run_prompt(launch, "resume".into(), host, cancel_rx),
    )
    .await
    .expect("resume timed out")
    .expect("resume failed");
    assert!(second.resumed);
    assert_eq!(second.session_id, first.session_id);
    assert_eq!(
        handle.server.counters.session_load.load(Ordering::SeqCst),
        1
    );
}

#[tokio::test]
async fn streamable_http_cancels_a_running_prompt() {
    let handle = TestServerHandle::start(TestServerConfig {
        hold_prompt_until_cancel: true,
        ..Default::default()
    })
    .await;
    let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
    let host = Arc::new(RecordingHost {
        started: Mutex::new(Some(started_tx)),
        ..Default::default()
    });
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let run = tokio::spawn(async move {
        run_prompt(
            remote_launch(&handle.endpoint, AcpTransport::StreamableHttp),
            "cancel me".into(),
            host,
            cancel_rx,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(10), started_rx.recv())
        .await
        .expect("agent did not start")
        .expect("start signal dropped");
    cancel_tx.send_replace(true);
    let outcome = tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .expect("cancel did not finish")
        .unwrap()
        .expect("cancel failed");
    assert!(outcome.cancelled);
    assert_eq!(outcome.stop_reason, "cancelled");
}

#[tokio::test]
async fn remote_transport_does_not_spawn_the_stdio_command() {
    let marker = std::env::temp_dir().join(format!(
        "verlinpark-acp-remote-marker-{}",
        uuid::Uuid::now_v7()
    ));
    let script = std::env::temp_dir().join(format!(
        "verlinpark-acp-remote-script-{}",
        uuid::Uuid::now_v7()
    ));
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n: > '{}'\n",
            marker.to_string_lossy().replace('\'', "'\\''")
        ),
    )
    .expect("write marker script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&script)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).expect("script permissions");
    }
    let handle = TestServerHandle::start(TestServerConfig::default()).await;
    let mut launch = remote_launch(&handle.endpoint, AcpTransport::StreamableHttp);
    launch.command = script.to_string_lossy().to_string();
    let host = Arc::new(RecordingHost::default());
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    run_prompt(launch, "remote only".into(), host, cancel_rx)
        .await
        .expect("remote prompt failed");
    assert!(!marker.exists(), "remote transport spawned a child process");
    let _ = std::fs::remove_file(&script);
}

#[tokio::test]
async fn streamable_http_accepts_a_direct_200_session_response() {
    let handle = TestServerHandle::start(TestServerConfig {
        direct_session_new_response: true,
        ..Default::default()
    })
    .await;
    let host = Arc::new(RecordingHost::default());
    let outcome = run_remote(&handle.endpoint, AcpTransport::StreamableHttp, host)
        .await
        .expect("direct 200 session/new must work");
    assert_eq!(outcome.stop_reason, "end_turn");
    assert_eq!(handle.server.counters.session_new.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn streamable_http_fails_when_the_session_stream_closes() {
    let handle = TestServerHandle::start(TestServerConfig {
        close_session_stream: true,
        ..Default::default()
    })
    .await;
    let host = Arc::new(RecordingHost::default());
    let error = tokio::time::timeout(
        Duration::from_secs(20),
        run_remote(&handle.endpoint, AcpTransport::StreamableHttp, host),
    )
    .await
    .expect("prompt timed out")
    .expect_err("a closed session stream must fail");
    let text = error.to_string();
    assert!(text.contains("closed") || text.contains("stream"), "{text}");
}

#[tokio::test]
async fn streamable_http_accepts_a_body_only_connection_id() {
    let handle = TestServerHandle::start(TestServerConfig {
        omit_connection_id_header: true,
        ..Default::default()
    })
    .await;
    let host = Arc::new(RecordingHost::default());
    let outcome = run_remote(&handle.endpoint, AcpTransport::StreamableHttp, host)
        .await
        .expect("body-only connection id must work");
    assert_eq!(outcome.stop_reason, "end_turn");
}

#[tokio::test]
async fn streamable_http_requires_a_connection_id_somewhere() {
    let handle = TestServerHandle::start(TestServerConfig {
        omit_connection_id_header: true,
        omit_connection_id_body: true,
        ..Default::default()
    })
    .await;
    let host = Arc::new(RecordingHost::default());
    let error = run_remote(&handle.endpoint, AcpTransport::StreamableHttp, host)
        .await
        .expect_err("missing connection id must fail");
    assert!(
        error.to_string().to_lowercase().contains("connection"),
        "{error}"
    );
}

#[tokio::test]
async fn streamable_http_reports_session_new_and_prompt_http_errors() {
    for (config, expected) in [
        (
            TestServerConfig {
                session_new_status: Some(404),
                ..Default::default()
            },
            404,
        ),
        (
            TestServerConfig {
                prompt_status: Some(500),
                ..Default::default()
            },
            500,
        ),
    ] {
        let handle = TestServerHandle::start(config).await;
        let host = Arc::new(RecordingHost::default());
        let error = run_remote(&handle.endpoint, AcpTransport::StreamableHttp, host)
            .await
            .expect_err("HTTP error must fail the run");
        assert!(error.to_string().contains(&expected.to_string()), "{error}");
    }
}

#[tokio::test]
async fn streamable_http_reports_connection_stream_status() {
    let handle = TestServerHandle::start(TestServerConfig {
        get_status: Some(503),
        ..Default::default()
    })
    .await;
    let host = Arc::new(RecordingHost::default());
    let error = run_remote(&handle.endpoint, AcpTransport::StreamableHttp, host)
        .await
        .expect_err("GET status must fail");
    assert!(error.to_string().contains("503"), "{error}");
}

#[tokio::test]
async fn streamable_http_delete_failure_does_not_fail_a_finished_prompt() {
    let handle = TestServerHandle::start(TestServerConfig {
        delete_status: Some(500),
        ..Default::default()
    })
    .await;
    let host = Arc::new(RecordingHost::default());
    run_remote(&handle.endpoint, AcpTransport::StreamableHttp, host)
        .await
        .expect("prompt should succeed even when DELETE fails");
    for _ in 0..100 {
        if handle.server.counters.deletes.load(Ordering::SeqCst) > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(handle.server.counters.deletes.load(Ordering::SeqCst) > 0);
}

async fn raw_ws_failure(config: TestServerConfig) -> String {
    let handle = TestServerHandle::start(config).await;
    let launch = remote_launch(&handle.websocket_endpoint(), AcpTransport::Websocket);
    let transport_config =
        crate::acp::RemoteTransportConfig::from_launch(&launch).expect("ws config");
    let (mut channel, driver) = agent_client_protocol::ConnectTo::<Client>::into_channel_and_future(
        crate::acp::RemoteTransport::new(transport_config),
    );
    let task = tokio::spawn(driver);
    let initialize: agent_client_protocol::jsonrpcmsg::Message = serde_json::from_value(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {"protocolVersion": 1, "clientCapabilities": {}}
    }))
    .expect("initialize message");
    channel
        .tx
        .unbounded_send(Ok(initialize))
        .expect("send initialize");
    let mut error = String::new();
    for _ in 0..4 {
        match tokio::time::timeout(Duration::from_secs(5), channel.rx.next()).await {
            Ok(Some(Ok(_))) => {}
            Ok(Some(Err(value))) => {
                error = value.to_string();
                break;
            }
            Ok(None) => {
                error = "transport channel closed".into();
                break;
            }
            Err(_) => {
                error = "timed out waiting for the transport error".into();
                break;
            }
        }
    }
    task.abort();
    error
}

#[tokio::test]
async fn websocket_rejects_malformed_and_batch_server_frames() {
    for (config, expected) in [
        (
            TestServerConfig {
                ws_malformed: true,
                ..Default::default()
            },
            "malformed",
        ),
        (
            TestServerConfig {
                ws_batch: true,
                ..Default::default()
            },
            "batch",
        ),
    ] {
        let error = raw_ws_failure(config).await;
        assert!(
            error.to_lowercase().contains(expected),
            "expected {expected}: {error}"
        );
    }
}

#[tokio::test]
async fn websocket_reports_a_server_close_before_prompt() {
    let handle = TestServerHandle::start(TestServerConfig {
        ws_close_after_session_new: true,
        ..Default::default()
    })
    .await;
    let host = Arc::new(RecordingHost::default());
    let endpoint = handle.websocket_endpoint();
    let error = tokio::time::timeout(
        Duration::from_secs(20),
        run_remote(&endpoint, AcpTransport::Websocket, host),
    )
    .await
    .expect("ws prompt timed out")
    .expect_err("closed socket must fail");
    let text = error.to_string().to_lowercase();
    assert!(
        text.contains("close")
            || text.contains("websocket")
            || text.contains("receiver")
            || text.contains("send failed"),
        "{error}"
    );
}

#[tokio::test]
async fn websocket_runs_a_prompt_with_permission_and_clean_close() {
    let handle = TestServerHandle::start(TestServerConfig {
        prompt_requires_permission: true,
        load_session: true,
        ..Default::default()
    })
    .await;
    let host = Arc::new(RecordingHost::default());
    let endpoint = handle.websocket_endpoint();
    let outcome = tokio::time::timeout(
        Duration::from_secs(20),
        run_remote(&endpoint, AcpTransport::Websocket, host.clone()),
    )
    .await
    .expect("websocket prompt timed out")
    .expect("websocket prompt failed");
    assert_eq!(outcome.stop_reason, "end_turn");
    assert_eq!(outcome.transport, AcpTransport::Websocket);
    assert_eq!(outcome.agent_name, "test-acp-ws");
    assert_eq!(host.permissions.load(Ordering::SeqCst), 1);
    assert!(host.messages.lock().expect("messages").contains("approved"));
}

#[tokio::test]
async fn websocket_forwards_authentication_and_explicit_headers() {
    unsafe {
        std::env::set_var(TEST_AUTH_ENV_WS, "Bearer ws-integration-token");
    }
    let handle = TestServerHandle::start(TestServerConfig {
        expected_auth: Some("Bearer ws-integration-token".into()),
        expected_header: Some(("x-project".into(), "demo".into())),
        ..Default::default()
    })
    .await;
    let mut launch = remote_launch(&handle.websocket_endpoint(), AcpTransport::Websocket);
    launch.auth_header_env = Some(TEST_AUTH_ENV_WS.into());
    launch.headers = std::collections::BTreeMap::from([("x-project".into(), "demo".into())]);
    let host = Arc::new(RecordingHost::default());
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let outcome = tokio::time::timeout(
        Duration::from_secs(20),
        run_prompt(launch, "ws headers".into(), host, cancel_rx),
    )
    .await
    .expect("ws prompt timed out")
    .expect("ws prompt failed");
    assert_eq!(outcome.stop_reason, "end_turn");
    assert!(handle.server.counters.auth_requests.load(Ordering::SeqCst) >= 1);
    assert!(
        handle
            .server
            .counters
            .custom_header_requests
            .load(Ordering::SeqCst)
            >= 1
    );
    unsafe {
        std::env::remove_var(TEST_AUTH_ENV_WS);
    }
}

#[tokio::test]
async fn websocket_rejects_wrong_authentication() {
    unsafe {
        std::env::set_var(TEST_AUTH_ENV_WS_BAD, "Bearer ws-wrong");
    }
    let handle = TestServerHandle::start(TestServerConfig {
        expected_auth: Some("Bearer ws-right".into()),
        ..Default::default()
    })
    .await;
    let mut launch = remote_launch(&handle.websocket_endpoint(), AcpTransport::Websocket);
    launch.auth_header_env = Some(TEST_AUTH_ENV_WS_BAD.into());
    let host = Arc::new(RecordingHost::default());
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let error = tokio::time::timeout(
        Duration::from_secs(10),
        run_prompt(launch, "ws auth".into(), host, cancel_rx),
    )
    .await
    .expect("wrong ws auth must not hang")
    .expect_err("wrong ws auth must fail");
    let text = error.to_string();
    assert!(text.contains("401"), "{text}");
    assert!(!text.contains("ws-wrong"), "{text}");
    unsafe {
        std::env::remove_var(TEST_AUTH_ENV_WS_BAD);
    }
}

#[tokio::test]
async fn websocket_cancels_a_running_prompt() {
    let handle = TestServerHandle::start(TestServerConfig {
        hold_prompt_until_cancel: true,
        ..Default::default()
    })
    .await;
    let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
    let host = Arc::new(RecordingHost {
        started: Mutex::new(Some(started_tx)),
        ..Default::default()
    });
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let endpoint = handle.websocket_endpoint();
    let run = tokio::spawn(async move {
        run_prompt(
            remote_launch(&endpoint, AcpTransport::Websocket),
            "cancel me".into(),
            host,
            cancel_rx,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(10), started_rx.recv())
        .await
        .expect("ws agent did not start")
        .expect("ws start signal dropped");
    cancel_tx.send_replace(true);
    let outcome = tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .expect("ws cancel did not finish")
        .unwrap()
        .expect("ws cancel failed");
    assert!(outcome.cancelled);
    assert_eq!(outcome.stop_reason, "cancelled");
}

#[tokio::test]
async fn websocket_resumes_a_session_with_session_load() {
    let handle = TestServerHandle::start(TestServerConfig {
        load_session: true,
        ..Default::default()
    })
    .await;
    let host = Arc::new(RecordingHost::default());
    let endpoint = handle.websocket_endpoint();
    let first = run_remote(&endpoint, AcpTransport::Websocket, host.clone())
        .await
        .expect("initial ws prompt");
    assert!(!first.resumed);
    let mut launch = remote_launch(&endpoint, AcpTransport::Websocket);
    launch.resume_session_id = Some(first.session_id.clone());
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let second = run_prompt(launch, "resume".into(), host, cancel_rx)
        .await
        .expect("ws resume");
    assert!(second.resumed);
    assert_eq!(second.session_id, first.session_id);
}

#[tokio::test]
async fn websocket_probe_reports_capabilities() {
    let handle = TestServerHandle::start(TestServerConfig {
        load_session: true,
        ..Default::default()
    })
    .await;
    let endpoint = handle.websocket_endpoint();
    let outcome = probe(remote_launch(&endpoint, AcpTransport::Websocket))
        .await
        .expect("ws probe failed");
    assert_eq!(outcome.transport, AcpTransport::Websocket);
    assert_eq!(outcome.agent_name, "test-acp-ws");
    assert!(outcome.load_session);
}

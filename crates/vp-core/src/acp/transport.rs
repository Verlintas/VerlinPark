//! ACP Streamable HTTP and WebSocket transport adapter.
//!
//! The ACP SDK speaks JSON-RPC over a [`Channel`]. This module builds a
//! [`ConnectTo<Agent>`] component whose driver translates that channel to the
//! ACP RFD transport:
//!
//! * `streamable_http`: one `POST /acp` per client message (`initialize`
//!   returns 200 with a connection id, everything else returns 202), one
//!   connection-scoped GET SSE stream, and one session-scoped GET SSE stream
//!   per `sessionId`. Server messages are correlated by JSON-RPC id and routed
//!   back into the same channel. `DELETE /acp` releases the connection.
//! * `websocket`: one text-frame channel on the same endpoint. `initialize` is
//!   the first message; binary frames are ignored; close is clean.
//!
//! This is intentionally a *transport*: it does not add retries, replay, or
//! hidden model routing. A dropped connection is reported to the ACP layer; v1
//! does not replay in-flight messages. Sessions can be resumed by the caller
//! with `session/load` when the agent advertises that capability.
//!
//! Remote agents are trusted external services. This module never spawns a
//! process, never applies the local Seatbelt sandbox, and never writes to the
//! local worktree.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_client_protocol::jsonrpcmsg::{Id, Message as RpcMessage};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::util::internal_error;
use agent_client_protocol::{Agent, Channel, Client, ConnectTo};
use anyhow::{Context, Result, anyhow, bail};
use futures_channel::mpsc as futures_mpsc;
use futures_util::future::{Either, select};
use futures_util::{SinkExt, StreamExt};
use reqwest::cookie::Jar;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderName, HeaderValue};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use vp_protocol::types::AcpTransport;

use super::AcpLaunch;
use super::config::{resolve_remote_headers, validate_acp_endpoint, validate_headers};

/// Wire header carrying the ACP connection identity.
pub(crate) const HEADER_CONNECTION_ID: &str = "acp-connection-id";
/// Wire header carrying the ACP session identity for session-scoped requests.
pub(crate) const HEADER_SESSION_ID: &str = "acp-session-id";
/// Wire header carrying the negotiated ACP protocol version (optional today).
pub(crate) const HEADER_PROTOCOL_VERSION: &str = "acp-protocol-version";

/// Default bound for one JSON-RPC message in either direction.
pub(crate) const DEFAULT_MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
/// Default bound for one SSE event payload.
pub(crate) const DEFAULT_MAX_SSE_EVENT_BYTES: usize = 8 * 1024 * 1024;
/// Default bound for one WebSocket frame/message.
pub(crate) const DEFAULT_MAX_WS_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
/// How long a best-effort DELETE/Close may take during shutdown.
const CLOSE_GRACE: Duration = Duration::from_secs(2);

/// Observable transport counters shared with a probe or diagnostic surface.
///
/// No header values, URLs with userinfo, or tokens are recorded here. The HTTP
/// version is the response protocol version reported by the client stack.
#[derive(Debug, Default)]
pub(crate) struct TransportDiagnostics {
    http_version: Mutex<Option<String>>,
    messages_sent: AtomicU64,
    messages_received: AtomicU64,
    streams_opened: AtomicU64,
}

impl TransportDiagnostics {
    pub fn http_version(&self) -> Option<String> {
        self.http_version
            .lock()
            .ok()
            .and_then(|value| value.clone())
    }

    pub fn messages_sent(&self) -> u64 {
        self.messages_sent.load(Ordering::Relaxed)
    }

    pub fn messages_received(&self) -> u64 {
        self.messages_received.load(Ordering::Relaxed)
    }

    pub fn streams_opened(&self) -> u64 {
        self.streams_opened.load(Ordering::Relaxed)
    }

    fn set_http_version(&self, version: reqwest::Version) {
        let label = match version {
            reqwest::Version::HTTP_09 => "HTTP/0.9",
            reqwest::Version::HTTP_10 => "HTTP/1.0",
            reqwest::Version::HTTP_11 => "HTTP/1.1",
            reqwest::Version::HTTP_2 => "HTTP/2",
            reqwest::Version::HTTP_3 => "HTTP/3",
            _ => "unknown",
        };
        if let Ok(mut value) = self.http_version.lock() {
            *value = Some(label.to_string());
        }
    }
}

/// Fully resolved remote transport configuration.
///
/// Secrets are resolved into `headers` at construction and never rendered by
/// `Debug`: the manual implementation below redacts header values.
#[derive(Clone)]
pub(crate) struct RemoteTransportConfig {
    pub transport: AcpTransport,
    pub endpoint: String,
    pub headers: Vec<(HeaderName, HeaderValue)>,
    pub connect_timeout: Duration,
    pub max_message_bytes: usize,
    pub max_sse_event_bytes: usize,
    pub max_ws_message_bytes: usize,
    /// Optional idle timeout for a live SSE/WebSocket stream. Disabled by
    /// default because a reasoning agent may legitimately be quiet; tests and
    /// embedders can set a bound.
    pub stream_idle_timeout: Option<Duration>,
    pub user_agent: String,
    pub diagnostics: Arc<TransportDiagnostics>,
}

impl std::fmt::Debug for RemoteTransportConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteTransportConfig")
            .field("transport", &self.transport)
            .field("endpoint", &self.endpoint)
            .field("header_names", &self.header_names())
            .field("connect_timeout", &self.connect_timeout)
            .field("max_message_bytes", &self.max_message_bytes)
            .field("max_sse_event_bytes", &self.max_sse_event_bytes)
            .field("max_ws_message_bytes", &self.max_ws_message_bytes)
            .field("stream_idle_timeout", &self.stream_idle_timeout)
            .field("user_agent", &self.user_agent)
            .finish()
    }
}

impl RemoteTransportConfig {
    /// Resolve a launch into a transport configuration.
    ///
    /// Validation is repeated here even though the RPC layer validates at
    /// add/update time: a stored row or a direct library caller must not be
    /// able to bypass the remote opt-in or the URL/header rules.
    pub fn from_launch(launch: &AcpLaunch) -> Result<Self> {
        if !launch.transport.is_remote() {
            bail!("ACP transport is stdio, not a remote profile");
        }
        if !launch.allow_remote {
            bail!(
                "remote ACP agent {} is not allowed; set allow_remote=true after reviewing the trust model",
                launch.name
            );
        }
        let endpoint = launch
            .endpoint
            .as_deref()
            .ok_or_else(|| anyhow!("remote ACP agent {} has no endpoint", launch.name))?;
        let endpoint = validate_acp_endpoint(launch.transport, endpoint)?;
        // Re-validate the header map at connect time. A hand-edited row must
        // not be able to override transport-managed headers or exceed bounds.
        let headers = validate_headers(&launch.headers)?;
        let resolved =
            resolve_remote_headers(launch.auth_header_env.as_deref(), &headers, |name| {
                std::env::var(name).ok()
            })?;
        let connect_timeout = launch
            .connect_timeout
            .clamp(Duration::from_secs(1), Duration::from_secs(300));
        Ok(Self {
            transport: launch.transport,
            endpoint,
            headers: resolved,
            connect_timeout,
            max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
            max_sse_event_bytes: DEFAULT_MAX_SSE_EVENT_BYTES,
            max_ws_message_bytes: DEFAULT_MAX_WS_MESSAGE_BYTES,
            stream_idle_timeout: None,
            user_agent: format!("VerlinPark/{}", env!("CARGO_PKG_VERSION")),
            diagnostics: launch
                .diagnostics
                .clone()
                .unwrap_or_else(|| Arc::new(TransportDiagnostics::default())),
        })
    }

    fn header_names(&self) -> Vec<&str> {
        self.headers.iter().map(|(name, _)| name.as_str()).collect()
    }
}

/// A [`ConnectTo<Client>`] component backed by the remote ACP transport.
///
/// The builder is created from the `Client` role, so the transport component is
/// typed against `Client` and connects the local client to the remote `Agent`.
pub(crate) struct RemoteTransport {
    config: RemoteTransportConfig,
}

impl RemoteTransport {
    pub fn new(config: RemoteTransportConfig) -> Self {
        Self { config }
    }
}

impl ConnectTo<Client> for RemoteTransport {
    async fn connect_to(
        self,
        client: impl ConnectTo<Agent>,
    ) -> std::result::Result<(), agent_client_protocol::Error> {
        let (channel, driver) = ConnectTo::<Client>::into_channel_and_future(self);
        match select(Box::pin(client.connect_to(channel)), driver).await {
            Either::Left((result, _)) | Either::Right((result, _)) => result,
        }
    }

    fn into_channel_and_future(
        self,
    ) -> (
        Channel,
        futures_util::future::BoxFuture<
            'static,
            std::result::Result<(), agent_client_protocol::Error>,
        >,
    ) {
        let (caller, driver_side) = Channel::duplex();
        let future = Box::pin(async move {
            let result = match self.config.transport {
                AcpTransport::StreamableHttp => run_http(self.config, driver_side).await,
                AcpTransport::Websocket => run_websocket(self.config, driver_side).await,
                AcpTransport::Stdio => Err(anyhow!("stdio is not a remote ACP transport")),
            };
            match result {
                Ok(()) => Ok(()),
                Err(error) => {
                    let detail = bounded_error(&format!("{error:#}"));
                    Err(internal_error(format!(
                        "remote ACP transport failed: {detail}"
                    )))
                }
            }
        });
        (caller, future)
    }
}

/// One pending client request that will be answered on an SSE stream.
#[derive(Debug, Clone)]
struct PendingOutgoing {
    method: String,
}

/// Events produced by SSE/WebSocket readers and consumed by the driver.
enum InboundEvent {
    Message(RpcMessage),
    Failure(String),
}

/// Shared identity bookkeeping for one HTTP connection.
struct HttpState {
    connection_id: Mutex<Option<String>>,
    sessions: Mutex<HashSet<String>>,
    pending: Mutex<HashMap<String, PendingOutgoing>>,
    inbound_requests: Mutex<HashMap<String, String>>,
}

impl HttpState {
    fn new() -> Self {
        Self {
            connection_id: Mutex::new(None),
            sessions: Mutex::new(HashSet::new()),
            pending: Mutex::new(HashMap::new()),
            inbound_requests: Mutex::new(HashMap::new()),
        }
    }

    fn connection_id(&self) -> Option<String> {
        self.connection_id
            .lock()
            .ok()
            .and_then(|value| value.clone())
    }

    fn set_connection_id(&self, connection_id: String) {
        if let Ok(mut value) = self.connection_id.lock() {
            *value = Some(connection_id);
        }
    }

    fn has_session(&self, session_id: &str) -> bool {
        self.sessions
            .lock()
            .map(|sessions| sessions.contains(session_id))
            .unwrap_or(false)
    }

    fn register_session(&self, session_id: &str) -> bool {
        self.sessions
            .lock()
            .map(|mut sessions| sessions.insert(session_id.to_string()))
            .unwrap_or(false)
    }

    fn track_pending(&self, id: &Id, method: &str) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.insert(
                id_key(id),
                PendingOutgoing {
                    method: method.to_string(),
                },
            );
        }
    }

    fn take_pending(&self, id: &Id) -> Option<PendingOutgoing> {
        self.pending.lock().ok()?.remove(&id_key(id))
    }

    fn forget_pending(&self, id: &Id) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&id_key(id));
        }
    }

    fn record_inbound_request(&self, id: &Id, session_id: &str) {
        if let Ok(mut requests) = self.inbound_requests.lock() {
            requests.insert(id_key(id), session_id.to_string());
        }
    }

    fn take_inbound_session(&self, id: &Id) -> Option<String> {
        self.inbound_requests.lock().ok()?.remove(&id_key(id))
    }
}

async fn run_http(config: RemoteTransportConfig, channel: Channel) -> Result<()> {
    let Channel {
        mut rx,
        tx: incoming_tx,
    } = channel;
    let jar = Arc::new(Jar::default());
    let client = match reqwest::Client::builder()
        .cookie_provider(jar)
        .connect_timeout(config.connect_timeout)
        .user_agent(config.user_agent.clone())
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            let error = anyhow!("building ACP HTTP client: {error}");
            let _ = incoming_tx
                .unbounded_send(Err(internal_error(bounded_error(&format!("{error:#}")))));
            return Err(error);
        }
    };
    let state = Arc::new(HttpState::new());
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<InboundEvent>();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    // A guard is used instead of relying on the driver running to
    // completion: the ACP SDK owns the transport future and may drop it as
    // soon as the client's main closure returns. The guard performs the
    // best-effort DELETE from a detached task.
    let _close_guard = HttpCloseGuard::new(client.clone(), config.clone(), state.clone());
    let mut initialized = false;
    let mut result: Result<()> = Ok(());

    loop {
        tokio::select! {
            outgoing = rx.next() => {
                let Some(outgoing) = outgoing else {
                    break;
                };
                let message = match outgoing {
                    Ok(message) => message,
                    Err(error) => {
                        result = Err(anyhow!("ACP JSON-RPC layer error: {error}"));
                        break;
                    }
                };
                if let Err(error) = handle_http_outgoing(
                    &client,
                    &config,
                    &state,
                    &message,
                    &mut initialized,
                    &mut tasks,
                    &shutdown_rx,
                    &event_tx,
                    &incoming_tx,
                ).await {
                    result = Err(error);
                    break;
                }
            }
            event = event_rx.recv() => {
                let Some(event) = event else {
                    result = Err(anyhow!("ACP transport event loop closed unexpectedly"));
                    break;
                };
                match event {
                    InboundEvent::Failure(detail) => {
                        result = Err(anyhow!("{detail}"));
                        break;
                    }
                    InboundEvent::Message(message) => {
                        if let Err(error) = handle_inbound_http_message(
                            &config,
                            &client,
                            &state,
                            message,
                            &mut tasks,
                            &shutdown_rx,
                            &event_tx,
                            &incoming_tx,
                        ).await {
                            result = Err(error);
                            break;
                        }
                    }
                }
            }
        }
    }

    let _ = shutdown_tx.send(true);
    for task in &tasks {
        task.abort();
    }
    if let Err(error) = &result {
        let _ =
            incoming_tx.unbounded_send(Err(internal_error(bounded_error(&format!("{error:#}")))));
    }
    result
}

#[allow(clippy::too_many_arguments)]
async fn handle_http_outgoing(
    client: &reqwest::Client,
    config: &RemoteTransportConfig,
    state: &Arc<HttpState>,
    message: &RpcMessage,
    initialized: &mut bool,
    tasks: &mut Vec<tokio::task::JoinHandle<()>>,
    shutdown: &watch::Receiver<bool>,
    event_tx: &mpsc::UnboundedSender<InboundEvent>,
    incoming_tx: &futures_mpsc::UnboundedSender<
        std::result::Result<RpcMessage, agent_client_protocol::Error>,
    >,
) -> Result<()> {
    let serialized = serialize_message(message, config.max_message_bytes)?;
    let is_request = matches!(message, RpcMessage::Request(_));
    let method = message_method(message);
    let id = message_id(message);
    let session_id = outgoing_session_id(state, message);

    if !*initialized {
        if method != Some("initialize") || !is_request {
            bail!("the first remote ACP message must be an initialize request");
        }
        let message = post_initialize(client, config, state, serialized).await?;
        *initialized = true;
        if let RpcMessage::Response(response) = &message
            && response.error.is_some()
        {
            // The agent rejected initialization. Deliver the error and stop:
            // there is no negotiated connection to use.
            let _ = incoming_tx.unbounded_send(Ok(message));
            bail!("ACP agent rejected initialize");
        }
        // The connection-scoped stream must be live before the caller sends
        // session/new; otherwise its response could race the GET.
        open_stream(
            client,
            config,
            state,
            tasks,
            shutdown,
            event_tx,
            StreamKind::Connection,
        )
        .await?;
        let _ = incoming_tx.unbounded_send(Ok(message));
        return Ok(());
    }

    if state.connection_id().is_none() {
        bail!("remote ACP connection has no connection id");
    }
    if let Some(method) = method {
        require_session_for_method(method, session_id.as_deref())?;
    }
    if let Some(session_id) = session_id.as_deref()
        && !state.has_session(session_id)
    {
        // A session-scoped operation can arrive for a session created on a
        // previous connection (session/load). Open its stream before the POST
        // so the response cannot race us.
        open_stream(
            client,
            config,
            state,
            tasks,
            shutdown,
            event_tx,
            StreamKind::Session(session_id.to_string()),
        )
        .await?;
        state.register_session(session_id);
    }

    let request_id = id.clone();
    if let (Some(id), Some(method)) = (request_id.as_ref(), method)
        && is_request
    {
        state.track_pending(id, method);
    }
    let post = post_message(client, config, state, serialized, session_id.as_deref()).await;
    match post {
        Ok(PostReply::Accepted) => Ok(()),
        Ok(PostReply::Direct(message)) => {
            handle_inbound_http_message(
                config,
                client,
                state,
                message,
                tasks,
                shutdown,
                event_tx,
                incoming_tx,
            )
            .await
        }
        Err(error) => {
            if let Some(id) = request_id.as_ref() {
                state.forget_pending(id);
            }
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_inbound_http_message(
    config: &RemoteTransportConfig,
    client: &reqwest::Client,
    state: &Arc<HttpState>,
    message: RpcMessage,
    tasks: &mut Vec<tokio::task::JoinHandle<()>>,
    shutdown: &watch::Receiver<bool>,
    event_tx: &mpsc::UnboundedSender<InboundEvent>,
    incoming_tx: &futures_mpsc::UnboundedSender<
        std::result::Result<RpcMessage, agent_client_protocol::Error>,
    >,
) -> Result<()> {
    match &message {
        RpcMessage::Response(response) => {
            if let Some(id) = response.id.as_ref()
                && let Some(pending) = state.take_pending(id)
                && pending.method == "session/new"
                && response.error.is_none()
                && let Some(session_id) = response_session_id(response)
                && !state.has_session(&session_id)
            {
                open_stream(
                    client,
                    config,
                    state,
                    tasks,
                    shutdown,
                    event_tx,
                    StreamKind::Session(session_id.clone()),
                )
                .await?;
                state.register_session(&session_id);
            }
        }
        RpcMessage::Request(request) => {
            if let Some(id) = request.id.as_ref()
                && let Some(session_id) = params_session_id(&request.params)
            {
                state.record_inbound_request(id, &session_id);
            }
        }
    }
    config
        .diagnostics
        .messages_received
        .fetch_add(1, Ordering::Relaxed);
    let _ = incoming_tx.unbounded_send(Ok(message));
    Ok(())
}

enum PostReply {
    Accepted,
    Direct(RpcMessage),
}

#[derive(Clone)]
enum StreamKind {
    Connection,
    Session(String),
}

impl StreamKind {
    fn label(&self) -> String {
        match self {
            Self::Connection => "connection".to_string(),
            Self::Session(session_id) => format!("session {session_id}"),
        }
    }

    fn session_id(&self) -> Option<&str> {
        match self {
            Self::Connection => None,
            Self::Session(session_id) => Some(session_id),
        }
    }
}

async fn post_initialize(
    client: &reqwest::Client,
    config: &RemoteTransportConfig,
    state: &HttpState,
    body: Vec<u8>,
) -> Result<RpcMessage> {
    let mut request = client
        .post(&config.endpoint)
        .header(ACCEPT, "application/json")
        .header(CONTENT_TYPE, "application/json")
        .header(HEADER_PROTOCOL_VERSION, ProtocolVersion::V1.to_string())
        .body(body)
        .timeout(config.connect_timeout);
    for (name, value) in &config.headers {
        request = request.header(name, value);
    }
    let response = request
        .send()
        .await
        .context("sending ACP initialize request")?;
    config
        .diagnostics
        .messages_sent
        .fetch_add(1, Ordering::Relaxed);
    let status = response.status();
    let version = response.version();
    config.diagnostics.set_http_version(version);
    if !status.is_success() {
        return Err(http_status_error("initialize", status));
    }
    let header_connection_id = response
        .headers()
        .get(HEADER_CONNECTION_ID)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let text = response
        .text()
        .await
        .context("reading ACP initialize response")?;
    if text.len() > config.max_message_bytes {
        bail!(
            "ACP initialize response exceeded {} bytes",
            config.max_message_bytes
        );
    }
    let value: serde_json::Value =
        serde_json::from_str(&text).context("ACP initialize response is not JSON")?;
    if value.is_array() {
        bail!("ACP transport does not accept JSON-RPC batches");
    }
    let body_connection_id = value
        .get("result")
        .and_then(|result| result.get("connectionId"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let connection_id = header_connection_id.or(body_connection_id).ok_or_else(|| {
        anyhow!("ACP server did not return an {HEADER_CONNECTION_ID} header or connectionId")
    })?;
    state.set_connection_id(connection_id);
    config
        .diagnostics
        .messages_received
        .fetch_add(1, Ordering::Relaxed);
    let message = parse_message_value(value, config.max_message_bytes)?;
    tracing::debug!(
        http_version = ?version,
        streamable_http = true,
        "ACP initialize completed"
    );
    Ok(message)
}

async fn post_message(
    client: &reqwest::Client,
    config: &RemoteTransportConfig,
    state: &HttpState,
    body: Vec<u8>,
    session_id: Option<&str>,
) -> Result<PostReply> {
    let connection_id = state
        .connection_id()
        .ok_or_else(|| anyhow!("remote ACP POST attempted before initialize"))?;
    let mut request = client
        .post(&config.endpoint)
        .header(ACCEPT, "application/json")
        .header(CONTENT_TYPE, "application/json")
        .header(HEADER_CONNECTION_ID, &connection_id)
        .header(HEADER_PROTOCOL_VERSION, ProtocolVersion::V1.to_string())
        .body(body)
        .timeout(config.connect_timeout);
    if let Some(session_id) = session_id {
        request = request.header(HEADER_SESSION_ID, session_id);
    }
    for (name, value) in &config.headers {
        request = request.header(name, value);
    }
    let response = request.send().await.context("sending ACP message")?;
    config.diagnostics.set_http_version(response.version());
    config
        .diagnostics
        .messages_sent
        .fetch_add(1, Ordering::Relaxed);
    let status = response.status();
    if status.as_u16() == 202 || status.as_u16() == 204 {
        return Ok(PostReply::Accepted);
    }
    if status == reqwest::StatusCode::OK {
        let text = response
            .text()
            .await
            .context("reading direct ACP response")?;
        if text.trim().is_empty() {
            return Ok(PostReply::Accepted);
        }
        if text.len() > config.max_message_bytes {
            bail!(
                "ACP direct response exceeded {} bytes",
                config.max_message_bytes
            );
        }
        let value: serde_json::Value =
            serde_json::from_str(&text).context("ACP direct response is not JSON")?;
        let message = parse_message_value(value, config.max_message_bytes)?;
        return Ok(PostReply::Direct(message));
    }
    Err(http_status_error("ACP request", status))
}

fn http_status_error(context: &str, status: reqwest::StatusCode) -> anyhow::Error {
    // Never echo a response body: a misconfigured proxy or malicious origin
    // could reflect the submitted Authorization value. Status and request
    // context are enough to diagnose the failure.
    anyhow!("{context} failed with HTTP {status}")
}

/// Best-effort connection cleanup that also runs when the driver is dropped.
struct HttpCloseGuard {
    client: reqwest::Client,
    config: RemoteTransportConfig,
    state: Arc<HttpState>,
}

impl HttpCloseGuard {
    fn new(client: reqwest::Client, config: RemoteTransportConfig, state: Arc<HttpState>) -> Self {
        Self {
            client,
            config,
            state,
        }
    }
}

impl Drop for HttpCloseGuard {
    fn drop(&mut self) {
        let Some(connection_id) = self.state.connection_id() else {
            return;
        };
        let client = self.client.clone();
        let config = self.config.clone();
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(async move {
            close_http_connection(&client, &config, &connection_id).await;
        });
    }
}

async fn close_http_connection(
    client: &reqwest::Client,
    config: &RemoteTransportConfig,
    connection_id: &str,
) {
    let mut request = client
        .delete(&config.endpoint)
        .header(HEADER_CONNECTION_ID, connection_id)
        .timeout(CLOSE_GRACE);
    for (name, value) in &config.headers {
        request = request.header(name, value);
    }
    match request.send().await {
        Ok(response) => {
            tracing::debug!(status = %response.status(), "ACP connection DELETE completed");
        }
        Err(error) => {
            tracing::debug!(%error, "ACP connection DELETE failed during shutdown");
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn open_stream(
    client: &reqwest::Client,
    config: &RemoteTransportConfig,
    state: &Arc<HttpState>,
    tasks: &mut Vec<tokio::task::JoinHandle<()>>,
    shutdown: &watch::Receiver<bool>,
    event_tx: &mpsc::UnboundedSender<InboundEvent>,
    kind: StreamKind,
) -> Result<()> {
    let connection_id = state
        .connection_id()
        .ok_or_else(|| anyhow!("cannot open an ACP stream before initialize"))?;
    let mut request = client
        .get(&config.endpoint)
        .header(ACCEPT, "text/event-stream")
        .header(HEADER_CONNECTION_ID, &connection_id)
        .header(HEADER_PROTOCOL_VERSION, ProtocolVersion::V1.to_string());
    if let Some(session_id) = kind.session_id() {
        request = request.header(HEADER_SESSION_ID, session_id);
    }
    for (name, value) in &config.headers {
        request = request.header(name, value);
    }
    let send = request.send();
    let response = tokio::time::timeout(config.connect_timeout, send)
        .await
        .map_err(|_| {
            anyhow!(
                "opening ACP {} stream timed out after {:?}",
                kind.label(),
                config.connect_timeout
            )
        })?
        .with_context(|| format!("opening ACP {} stream", kind.label()))?;
    let status = response.status();
    if status != reqwest::StatusCode::OK {
        return Err(http_status_error(
            &format!("opening ACP {} stream", kind.label()),
            status,
        ));
    }
    config
        .diagnostics
        .streams_opened
        .fetch_add(1, Ordering::Relaxed);
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !content_type.starts_with("text/event-stream") {
        bail!(
            "ACP {} stream returned unsupported content type '{content_type}'",
            kind.label()
        );
    }

    let (ready_tx, ready_rx) = oneshot::channel::<std::result::Result<(), String>>();
    let label = kind.label();
    let event_tx = event_tx.clone();
    let shutdown = shutdown.clone();
    let max_event = config.max_sse_event_bytes;
    let max_message = config.max_message_bytes;
    let idle = config.stream_idle_timeout;
    let handle = tokio::spawn(async move {
        let _ = ready_tx.send(Ok(()));
        let mut decoder = SseDecoder::new(max_event);
        let mut stream = response.bytes_stream();
        loop {
            if *shutdown.borrow() {
                break;
            }
            let next = if let Some(idle) = idle {
                match tokio::time::timeout(idle, stream.next()).await {
                    Ok(next) => next,
                    Err(_) => {
                        let _ = event_tx.send(InboundEvent::Failure(format!(
                            "ACP {label} stream was idle for {idle:?}"
                        )));
                        break;
                    }
                }
            } else {
                stream.next().await
            };
            match next {
                Some(Ok(chunk)) => match decoder.push(&chunk) {
                    Ok(events) => {
                        if !forward_sse_events(events, &event_tx, &label, max_message) {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = event_tx.send(InboundEvent::Failure(format!(
                            "malformed ACP {label} SSE stream: {error}"
                        )));
                        break;
                    }
                },
                Some(Err(error)) => {
                    let _ = event_tx.send(InboundEvent::Failure(format!(
                        "ACP {label} stream read failed: {error}"
                    )));
                    break;
                }
                None => {
                    if !*shutdown.borrow() {
                        match decoder.finish() {
                            Ok(events) => {
                                let _ = forward_sse_events(events, &event_tx, &label, max_message);
                            }
                            Err(error) => {
                                let _ = event_tx.send(InboundEvent::Failure(format!(
                                    "malformed ACP {label} SSE stream: {error}"
                                )));
                                break;
                            }
                        }
                        let _ = event_tx.send(InboundEvent::Failure(format!(
                            "ACP {label} stream closed unexpectedly"
                        )));
                    }
                    break;
                }
            }
        }
    });
    tasks.push(handle);
    match tokio::time::timeout(config.connect_timeout, ready_rx).await {
        Ok(Ok(Ok(()))) => Ok(()),
        Ok(Ok(Err(error))) => Err(anyhow!("{error}")),
        Ok(Err(_)) => Err(anyhow!("ACP {} stream task stopped early", kind.label())),
        Err(_) => Err(anyhow!(
            "ACP {} stream did not become ready within {:?}",
            kind.label(),
            config.connect_timeout
        )),
    }
}

fn forward_sse_events(
    events: Vec<String>,
    event_tx: &mpsc::UnboundedSender<InboundEvent>,
    label: &str,
    max_message: usize,
) -> bool {
    for event in events {
        match parse_message_text(&event, max_message) {
            Ok(message) => {
                let _ = event_tx.send(InboundEvent::Message(message));
            }
            Err(error) => {
                let _ = event_tx.send(InboundEvent::Failure(format!(
                    "malformed ACP {label} event: {error}"
                )));
                return false;
            }
        }
    }
    true
}

async fn run_websocket(config: RemoteTransportConfig, channel: Channel) -> Result<()> {
    let Channel {
        mut rx,
        tx: incoming_tx,
    } = channel;
    let handshake = async {
        let mut request = config
            .endpoint
            .as_str()
            .into_client_request()
            .context("building ACP WebSocket request")?;
        for (name, value) in &config.headers {
            request.headers_mut().insert(name.clone(), value.clone());
        }
        let max = config.max_ws_message_bytes;
        let ws_config = WebSocketConfig::default()
            .max_message_size(Some(max))
            .max_frame_size(Some(max));
        let connect = tokio_tungstenite::connect_async_with_config(request, Some(ws_config), false);
        let (socket, response) = tokio::time::timeout(config.connect_timeout, connect)
            .await
            .map_err(|_| {
                anyhow!(
                    "ACP WebSocket handshake timed out after {:?}",
                    config.connect_timeout
                )
            })?
            .context("ACP WebSocket handshake failed")?;
        Ok::<_, anyhow::Error>((socket, response))
    }
    .await;
    let (socket, response) = match handshake {
        Ok(value) => value,
        Err(error) => {
            let _ = incoming_tx
                .unbounded_send(Err(internal_error(bounded_error(&format!("{error:#}")))));
            return Err(error);
        }
    };
    if let Some(connection_id) = response
        .headers()
        .get(HEADER_CONNECTION_ID)
        .and_then(|value| value.to_str().ok())
    {
        tracing::debug!(connection_id, "ACP WebSocket connection established");
    }
    let (mut sink, mut stream) = socket.split();
    let max = config.max_ws_message_bytes;
    let mut initialized = false;
    let mut result: Result<()> = Ok(());
    let mut clean = false;

    loop {
        tokio::select! {
            outgoing = rx.next() => {
                let Some(outgoing) = outgoing else {
                    clean = true;
                    break;
                };
                let message = match outgoing {
                    Ok(message) => message,
                    Err(error) => {
                        result = Err(anyhow!("ACP JSON-RPC layer error: {error}"));
                        break;
                    }
                };
                if !initialized {
                    if message_method(&message) != Some("initialize")
                        || !matches!(message, RpcMessage::Request(_))
                    {
                        result = Err(anyhow!(
                            "the first remote ACP WebSocket message must be an initialize request"
                        ));
                        break;
                    }
                    initialized = true;
                }
                let text = match serialize_message_text(&message, max) {
                    Ok(text) => text,
                    Err(error) => {
                        result = Err(error);
                        break;
                    }
                };
                if let Err(error) = sink.send(WsMessage::Text(text.into())).await {
                    result = Err(anyhow!("sending ACP WebSocket message failed: {error}"));
                    break;
                }
            }
            incoming = next_ws_message(&mut stream, config.stream_idle_timeout) => {
                match incoming {
                    Ok(Some(WsMessage::Text(text))) => {
                        if text.len() > max {
                            result = Err(anyhow!(
                                "ACP WebSocket message exceeded {max} bytes"
                            ));
                            break;
                        }
                        match parse_message_text(text.as_str(), max) {
                            Ok(message) => {
                                let _ = incoming_tx.unbounded_send(Ok(message));
                            }
                            Err(error) => {
                                result = Err(anyhow!(
                                    "malformed ACP WebSocket message: {error}"
                                ));
                                break;
                            }
                        }
                    }
                    Ok(Some(WsMessage::Binary(_))) => {
                        // The RFD says binary frames are ignored.
                        tracing::debug!("ignoring binary ACP WebSocket frame");
                    }
                    Ok(Some(WsMessage::Ping(payload))) => {
                        if let Err(error) = sink.send(WsMessage::Pong(payload)).await {
                            result = Err(anyhow!(
                                "replying to ACP WebSocket ping failed: {error}"
                            ));
                            break;
                        }
                    }
                    Ok(Some(WsMessage::Pong(_))) | Ok(Some(WsMessage::Frame(_))) => {}
                    Ok(Some(WsMessage::Close(frame))) => {
                        tracing::debug!(?frame, "ACP WebSocket closed by server");
                        clean = true;
                        break;
                    }
                    Ok(None) => {
                        clean = true;
                        break;
                    }
                    Err(error) => {
                        result = Err(anyhow!("ACP WebSocket read failed: {error}"));
                        break;
                    }
                }
            }
        }
    }

    if clean {
        let _ = tokio::time::timeout(CLOSE_GRACE, sink.send(WsMessage::Close(None))).await;
    }
    if let Err(error) = &result {
        let _ =
            incoming_tx.unbounded_send(Err(internal_error(bounded_error(&format!("{error:#}")))));
    }
    result
}

async fn next_ws_message<S>(stream: &mut S, idle: Option<Duration>) -> Result<Option<WsMessage>>
where
    S: futures_util::Stream<
            Item = std::result::Result<WsMessage, tokio_tungstenite::tungstenite::Error>,
        > + Unpin,
{
    let next = stream.next();
    if let Some(idle) = idle {
        let item = tokio::time::timeout(idle, next)
            .await
            .map_err(|_| anyhow!("ACP WebSocket was idle for {idle:?}"))?;
        item.transpose()
            .map_err(|error| anyhow!("ACP WebSocket read failed: {error}"))
    } else {
        next.await
            .transpose()
            .map_err(|error| anyhow!("ACP WebSocket read failed: {error}"))
    }
}

fn serialize_message(message: &RpcMessage, max: usize) -> Result<Vec<u8>> {
    let text = serialize_message_text(message, max)?;
    Ok(text.into_bytes())
}

fn serialize_message_text(message: &RpcMessage, max: usize) -> Result<String> {
    let text = serde_json::to_string(message).context("serializing ACP message")?;
    if text.len() > max {
        bail!("outgoing ACP message exceeded {max} bytes");
    }
    Ok(text)
}

/// Parse a JSON-RPC message, rejecting batches and malformed frames with a
/// bounded error string.
pub(crate) fn parse_message_text(
    text: &str,
    max: usize,
) -> std::result::Result<RpcMessage, String> {
    if text.len() > max {
        return Err(format!("message exceeded {max} bytes"));
    }
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("empty message".to_string());
    }
    if trimmed.starts_with('[') {
        return Err("JSON-RPC batches are not supported".to_string());
    }
    let value: serde_json::Value =
        serde_json::from_str(trimmed).map_err(|error| format!("invalid JSON: {error}"))?;
    parse_message_value(value, max).map_err(|error| error.to_string())
}

fn parse_message_value(value: serde_json::Value, max: usize) -> Result<RpcMessage> {
    if value.is_array() {
        bail!("ACP transport does not accept JSON-RPC batches");
    }
    let message: RpcMessage = serde_json::from_value(value).context("not a JSON-RPC message")?;
    match &message {
        RpcMessage::Request(request) => {
            if request.method.trim().is_empty() {
                bail!("JSON-RPC request has an empty method");
            }
        }
        RpcMessage::Response(response) => {
            if response.result.is_none() && response.error.is_none() {
                bail!("JSON-RPC response has neither result nor error");
            }
        }
    }
    let serialized = serde_json::to_string(&message).unwrap_or_default();
    if serialized.len() > max {
        bail!("JSON-RPC message exceeded {max} bytes");
    }
    Ok(message)
}

fn message_method(message: &RpcMessage) -> Option<&str> {
    match message {
        RpcMessage::Request(request) => Some(request.method.as_str()),
        RpcMessage::Response(_) => None,
    }
}

fn message_id(message: &RpcMessage) -> Option<Id> {
    match message {
        RpcMessage::Request(request) => request.id.clone(),
        RpcMessage::Response(response) => response.id.clone(),
    }
}

fn params_session_id(params: &Option<agent_client_protocol::jsonrpcmsg::Params>) -> Option<String> {
    let agent_client_protocol::jsonrpcmsg::Params::Object(map) = params.as_ref()? else {
        return None;
    };
    map.get("sessionId")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
}

fn response_session_id(response: &agent_client_protocol::jsonrpcmsg::Response) -> Option<String> {
    response
        .result
        .as_ref()?
        .get("sessionId")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
}

fn outgoing_session_id(state: &HttpState, message: &RpcMessage) -> Option<String> {
    match message {
        RpcMessage::Request(request) => params_session_id(&request.params),
        RpcMessage::Response(response) => response
            .id
            .as_ref()
            .and_then(|id| state.take_inbound_session(id)),
    }
}

/// Methods that the RFD requires to carry `Acp-Session-Id`.
pub(crate) fn method_requires_session_header(method: &str) -> bool {
    matches!(
        method,
        "session/prompt"
            | "session/cancel"
            | "session/close"
            | "session/delete"
            | "session/fork"
            | "session/load"
            | "session/resume"
            | "session/set_config_option"
            | "session/set_mode"
            | "session/set_model"
    )
}

fn id_key(id: &Id) -> String {
    match id {
        Id::String(value) => format!("s:{value}"),
        Id::Number(value) => format!("n:{value}"),
        Id::Null => "null".to_string(),
    }
}

fn bounded_error(text: &str) -> String {
    let mut text = text.replace(['\n', '\r', '\0'], " ");
    text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.len() <= 512 {
        return text;
    }
    let mut end = 512;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… (truncated)", &text[..end])
}

/// Incremental SSE decoder with bounded line and event sizes.
///
/// It intentionally implements only the fields ACP uses (`data`, `event`,
/// `id`, `retry`, comments). Events are dispatched on a blank line; a trailing
/// complete event without a blank line is dispatched at end of stream.
pub(crate) struct SseDecoder {
    buffer: Vec<u8>,
    event_data: String,
    event_name: Option<String>,
    max_event_bytes: usize,
    started: bool,
}

impl SseDecoder {
    pub fn new(max_event_bytes: usize) -> Self {
        Self {
            buffer: Vec::new(),
            event_data: String::new(),
            event_name: None,
            max_event_bytes,
            started: false,
        }
    }

    /// Feed a byte chunk and return fully dispatched event payloads.
    pub fn push(&mut self, chunk: &[u8]) -> std::result::Result<Vec<String>, String> {
        self.buffer.extend_from_slice(chunk);
        // A UTF-8 BOM at the very start of a stream is not an SSE field. Wait
        // for enough bytes before deciding so a split BOM is still recognized.
        if !self.started {
            const BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];
            if self.buffer.len() >= BOM.len() {
                if self.buffer.starts_with(&BOM) {
                    self.buffer.drain(..BOM.len());
                }
                self.started = true;
            } else if !BOM.starts_with(&self.buffer) {
                self.started = true;
            }
        }
        let mut events = Vec::new();
        loop {
            let Some(newline) = self.buffer.iter().position(|byte| *byte == b'\n') else {
                // A single line that is still incomplete must be bounded.
                if self.buffer.len() > self.max_event_bytes {
                    return Err(format!("SSE line exceeded {} bytes", self.max_event_bytes));
                }
                return Ok(events);
            };
            let mut line: Vec<u8> = self.buffer.drain(..=newline).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line =
                String::from_utf8(line).map_err(|_| "SSE line is not valid UTF-8".to_string())?;
            self.process_line(&line, &mut events)?;
        }
    }

    /// Flush at end of stream. Returns a final complete event when one is
    /// pending without a terminating blank line.
    pub fn finish(&mut self) -> std::result::Result<Vec<String>, String> {
        let mut events = Vec::new();
        if !self.buffer.is_empty() {
            let line = String::from_utf8(std::mem::take(&mut self.buffer))
                .map_err(|_| "SSE trailing line is not valid UTF-8".to_string())?;
            self.process_line(line.trim_end_matches('\r'), &mut events)?;
        }
        if let Some(event) = self.take_event() {
            events.push(event);
        }
        Ok(events)
    }

    fn process_line(
        &mut self,
        line: &str,
        events: &mut Vec<String>,
    ) -> std::result::Result<(), String> {
        if line.is_empty() {
            if let Some(event) = self.take_event() {
                events.push(event);
            }
            return Ok(());
        }
        if line.starts_with(':') {
            return Ok(());
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "data" => {
                self.event_data.push_str(value);
                self.event_data.push('\n');
                if self.event_data.len() > self.max_event_bytes {
                    return Err(format!("SSE event exceeded {} bytes", self.max_event_bytes));
                }
            }
            "event" => {
                if value.len() > 256 {
                    return Err("SSE event name exceeded 256 bytes".to_string());
                }
                self.event_name = Some(value.to_string());
            }
            // `id` and `retry` are valid SSE fields but ACP v1 does not use
            // resumability, so they are accepted and ignored.
            "id" | "retry" => {}
            _ => {}
        }
        Ok(())
    }

    fn take_event(&mut self) -> Option<String> {
        let name = self.event_name.take();
        let mut data = std::mem::take(&mut self.event_data);
        if data.is_empty() {
            return None;
        }
        if data.ends_with('\n') {
            data.pop();
        }
        if data.is_empty() {
            return None;
        }
        // ACP does not define named events; parse any event carrying a JSON
        // object. A named `error` event is still surfaced as its payload so a
        // server cannot hide a failure behind an event name.
        let _ = name;
        Some(data)
    }
}

/// Validate that a method that requires a session header actually received a
/// session id from its params. HTTP keeps `Acp-Session-Id` aligned with the
/// JSON-RPC `sessionId`; a mismatch is rejected rather than guessed.
fn require_session_for_method(method: &str, session_id: Option<&str>) -> Result<()> {
    if method_requires_session_header(method) && session_id.is_none() {
        bail!("ACP method {method} requires params.sessionId");
    }
    Ok(())
}

#[cfg(test)]
mod tests;

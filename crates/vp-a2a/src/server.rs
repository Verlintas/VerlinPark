//! Loopback HTTP/SSE server and host callback boundary.
//!
//! The server binds exactly one literal loopback address. Every route is
//! bearer-authenticated. The only execution path for an inbound message is a
//! caller-supplied [`A2aHandler`]; this module contains no shell, filesystem,
//! browser, network proxy, or tool dispatcher.

use std::convert::Infallible;
use std::fmt;
use std::future::Future;
use std::net::SocketAddr;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_stream::stream;
use axum::body::{Body, to_bytes};
use axum::extract::{Path, Query, State};
use axum::http::header::{AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE, WWW_AUTHENTICATE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::FutureExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

use crate::error::{A2aError, A2aResult};
use crate::jsonrpc::{
    AGENT_CARD_PATH, JsonRpcId, JsonRpcRequest, JsonRpcResponse, METHOD_MESSAGE_SEND,
    METHOD_MESSAGE_STREAM, METHOD_TASKS_CANCEL, METHOD_TASKS_GET, METHOD_TASKS_RESUBSCRIBE,
    MessageSendConfiguration, RPC_PATH, SendMessageParams, TaskEventsQuery, TaskIdParams,
    TaskQueryParams, TaskResubscribeParams,
};
use crate::limits::{A2aLimits, MAX_OUTPUT_MODES};
use crate::policy::{A2aPolicy, A2aSecretToken, constant_time_eq};
use crate::store::{A2aCancellation, BeginOutcome, TaskStore, TaskSubscription};
use crate::url::{A2aUrl, is_exact_loopback};
use crate::wire::{A2aTask, A2aTaskUpdate, Message, validate_id, validate_metadata};

const TASK_EVENTS_ROUTE: &str = "/a2a/tasks/{id}/events";
const LAST_EVENT_ID: &str = "last-event-id";

/// Everything a host callback is allowed to know about one inbound run.
///
/// It contains no bearer token, no local path and no tool authority. The
/// callback must perform any side effect itself under its own policy; a
/// returned [`A2aTaskUpdate`] is only a lifecycle proposal.
#[derive(Debug, Clone)]
pub struct A2aRequestContext {
    pub task_id: String,
    pub context_id: String,
    pub message: Message,
    /// Snapshot after the inbound message was appended and the task moved to
    /// `working`.
    pub task: A2aTask,
    pub is_new_task: bool,
    pub cancellation: A2aCancellation,
    pub request_id: Option<JsonRpcId>,
}

impl A2aRequestContext {
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }
}

/// Host callback trait. It is the only way an inbound task can execute
/// anything, and it is never called by the store, the client, or a background
/// sweeper.
pub trait A2aHandler: Send + Sync {
    fn handle(
        &self,
        request: A2aRequestContext,
    ) -> impl Future<Output = A2aResult<A2aTaskUpdate>> + Send;
}

impl<F, Fut> A2aHandler for F
where
    F: Fn(A2aRequestContext) -> Fut + Send + Sync,
    Fut: Future<Output = A2aResult<A2aTaskUpdate>> + Send,
{
    fn handle(
        &self,
        request: A2aRequestContext,
    ) -> impl Future<Output = A2aResult<A2aTaskUpdate>> + Send {
        self(request)
    }
}

impl<H> A2aHandler for std::sync::Arc<H>
where
    H: A2aHandler,
{
    fn handle(
        &self,
        request: A2aRequestContext,
    ) -> impl Future<Output = A2aResult<A2aTaskUpdate>> + Send {
        (**self).handle(request)
    }
}

async fn invoke_handler<H>(handler: &H, context: A2aRequestContext) -> A2aResult<A2aTaskUpdate>
where
    H: A2aHandler,
{
    match AssertUnwindSafe(handler.handle(context))
        .catch_unwind()
        .await
    {
        Ok(result) => result,
        // A panicking host callback is isolated to this run and converted
        // into a failed task. No panic payload or host detail is exposed.
        Err(_) => Err(A2aError::HandlerFailed),
    }
}

/// Entry point for the loopback facade.
pub struct A2aServer;

impl A2aServer {
    /// Validate policy, bind loopback, and serve the card, JSON-RPC and SSE
    /// routes. Binding never falls back to a wildcard address.
    pub async fn start<H>(mut policy: A2aPolicy, handler: H) -> A2aResult<A2aStartHandle>
    where
        H: A2aHandler + 'static,
    {
        policy.validate()?;
        let listen = policy.listen_addr().socket_addr();
        if !is_exact_loopback(listen.ip()) {
            return Err(A2aError::LoopbackRequired {
                address: listen.to_string(),
            });
        }
        let token = policy.take_token().ok_or_else(|| A2aError::ConfigInvalid {
            reason: "an enabled facade requires exactly one bearer token".into(),
        })?;
        let token_hash = token.hash();
        let limits = policy.limits().clone();
        let card_spec = policy.card_spec().clone();

        let listener = tokio::net::TcpListener::bind(listen)
            .await
            .map_err(|error| A2aError::Transport {
                reason: format!("loopback bind failed: {}", error.kind()),
            })?;
        let local_addr = listener.local_addr().map_err(|error| A2aError::Transport {
            reason: format!("loopback address unavailable: {}", error.kind()),
        })?;
        if !is_exact_loopback(local_addr.ip()) || local_addr.port() == 0 {
            return Err(A2aError::LoopbackRequired {
                address: local_addr.to_string(),
            });
        }
        let url = A2aUrl::for_server(local_addr)?;
        let card = card_spec.card_at(&url);
        card.validate(&limits)?;
        // A host could accidentally put its bearer token into a skill or
        // extension. Refuse to serve a card that repeats the secret.
        let card_bytes = serde_json::to_vec(&card).map_err(|_| A2aError::Internal)?;
        if token.echo_checked() && contains_bytes(&card_bytes, token.expose().as_bytes()) {
            return Err(A2aError::ConfigInvalid {
                reason: "agent card must not contain the bearer token".into(),
            });
        }
        let store = Arc::new(TaskStore::new(limits.clone())?);
        let state = Arc::new(ServerState {
            store,
            handler: Arc::new(handler),
            card: card.clone(),
            token_hash,
            limits,
        });

        let app = Router::new()
            .route(AGENT_CARD_PATH, get(card_handler::<H>))
            .route(RPC_PATH, post(rpc_handler::<H>))
            .route(TASK_EVENTS_ROUTE, get(events_handler::<H>))
            .fallback(fallback_handler)
            .with_state(state);

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let join = tokio::spawn(async move {
            let mut shutdown_rx = shutdown_rx;
            let server = axum::serve(listener, app).with_graceful_shutdown(async move {
                let _ = shutdown_rx.changed().await;
            });
            let _ = server.await;
        });

        Ok(A2aStartHandle {
            local_addr,
            card,
            token: Mutex::new(Some(token)),
            shutdown: Some(shutdown_tx),
            join: Some(join),
        })
    }
}

struct ServerState<H> {
    store: Arc<TaskStore>,
    handler: Arc<H>,
    card: crate::wire::AgentCard,
    token_hash: [u8; 32],
    limits: A2aLimits,
}

impl<H> ServerState<H> {
    fn limits(&self) -> &A2aLimits {
        &self.limits
    }

    fn authenticate(&self, headers: &HeaderMap) -> A2aResult<()> {
        let mut values = headers.get_all(AUTHORIZATION).iter();
        let Some(value) = values.next() else {
            return Err(A2aError::Unauthorized);
        };
        if values.next().is_some() {
            return Err(A2aError::Unauthorized);
        }
        let value = value.to_str().map_err(|_| A2aError::Unauthorized)?;
        let (scheme, token) = value.split_once(' ').ok_or(A2aError::Unauthorized)?;
        if !scheme.eq_ignore_ascii_case("bearer") {
            return Err(A2aError::Unauthorized);
        }
        if token.is_empty()
            || token.len() > 512
            || token.bytes().any(|byte| !(0x21..=0x7e).contains(&byte))
        {
            return Err(A2aError::Unauthorized);
        }
        let digest = Sha256::digest(token.as_bytes());
        let mut candidate = [0u8; 32];
        candidate.copy_from_slice(&digest);
        if constant_time_eq(&candidate, &self.token_hash) {
            Ok(())
        } else {
            Err(A2aError::Unauthorized)
        }
    }
}

/// Handle to a running facade. The bearer token is available exactly once
/// through [`A2aStartHandle::take_token`].
pub struct A2aStartHandle {
    local_addr: SocketAddr,
    card: crate::wire::AgentCard,
    token: Mutex<Option<A2aSecretToken>>,
    shutdown: Option<watch::Sender<bool>>,
    join: Option<JoinHandle<()>>,
}

impl A2aStartHandle {
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn port(&self) -> u16 {
        self.local_addr.port()
    }

    pub fn url(&self) -> &str {
        &self.card.url
    }

    pub fn base_url(&self) -> &str {
        &self.card.url
    }

    pub fn card(&self) -> &crate::wire::AgentCard {
        &self.card
    }

    /// Take the plaintext bearer token. It is removed from the handle and is
    /// never available again. The server itself keeps only its SHA-256 hash.
    pub fn take_token(&self) -> Option<String> {
        let mut guard = lock_token(&self.token);
        guard.take().map(A2aSecretToken::into_string)
    }

    pub fn token_available(&self) -> bool {
        lock_token(&self.token).is_some()
    }

    /// Alias for [`A2aStartHandle::take_token`].
    pub fn take_bearer_token(&self) -> Option<String> {
        self.take_token()
    }

    /// Alias for [`A2aStartHandle::take_token`]; still one-time.
    pub fn token(&self) -> Option<String> {
        self.take_token()
    }

    /// Gracefully stop accepting requests and wait for the HTTP server task.
    pub async fn shutdown(mut self) -> A2aResult<()> {
        if let Some(sender) = self.shutdown.take() {
            let _ = sender.send(true);
        }
        if let Some(mut join) = self.join.take() {
            match tokio::time::timeout(Duration::from_secs(2), &mut join).await {
                Ok(result) => result.map_err(|_| A2aError::Internal)?,
                Err(_) => {
                    // An idle SSE client must not be able to hold the
                    // process open forever during an explicit shutdown.
                    join.abort();
                    let _ = join.await;
                }
            }
        }
        Ok(())
    }
}

impl fmt::Debug for A2aStartHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("A2aStartHandle")
            .field("local_addr", &self.local_addr)
            .field("url", &self.card.url)
            .field("card_name", &self.card.name)
            .field(
                "token",
                &if self.token_available() {
                    "<available>"
                } else {
                    "<taken>"
                },
            )
            .finish()
    }
}

impl Drop for A2aStartHandle {
    fn drop(&mut self) {
        if let Some(sender) = self.shutdown.take() {
            let _ = sender.send(true);
        }
        if let Some(join) = self.join.take() {
            join.abort();
        }
    }
}

async fn card_handler<H>(State(state): State<Arc<ServerState<H>>>, headers: HeaderMap) -> Response
where
    H: A2aHandler + 'static,
{
    if let Err(error) = state.authenticate(&headers) {
        return auth_error_response(error);
    }
    let mut response = Json(state.card.clone()).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn rpc_handler<H>(
    State(state): State<Arc<ServerState<H>>>,
    headers: HeaderMap,
    body: Body,
) -> Response
where
    H: A2aHandler + 'static,
{
    if let Err(error) = state.authenticate(&headers) {
        return auth_error_response(error);
    }
    if let Some(content_type) = headers.get(CONTENT_TYPE) {
        let content_type = content_type.to_str().unwrap_or_default();
        if !content_type.starts_with("application/json") && !content_type.contains("+json") {
            return jsonrpc_error_response(
                A2aError::ContentTypeNotSupported,
                JsonRpcId::Null,
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
            );
        }
    }
    let body_limit = state.limits.max_body_bytes.saturating_add(1);
    let body = match to_bytes(body, body_limit).await {
        Ok(body) => body,
        Err(_) => {
            return jsonrpc_error_response(
                A2aError::BoundsExceeded {
                    field: "body",
                    max: state.limits.max_body_bytes,
                    actual: body_limit,
                },
                JsonRpcId::Null,
                StatusCode::PAYLOAD_TOO_LARGE,
            );
        }
    };
    if body.len() > state.limits.max_body_bytes {
        return jsonrpc_error_response(
            A2aError::BoundsExceeded {
                field: "body",
                max: state.limits.max_body_bytes,
                actual: body.len(),
            },
            JsonRpcId::Null,
            StatusCode::PAYLOAD_TOO_LARGE,
        );
    }
    let request = match parse_request(&body) {
        Ok(request) => request,
        Err(error) => {
            return jsonrpc_error_response(error, JsonRpcId::Null, StatusCode::OK);
        }
    };
    let id = request.id.clone();
    if request.jsonrpc != crate::jsonrpc::JSONRPC_VERSION {
        return jsonrpc_error_response(
            A2aError::InvalidRequest {
                reason: "jsonrpc must be \"2.0\"".into(),
            },
            id,
            StatusCode::OK,
        );
    }
    if id.is_null() {
        return jsonrpc_error_response(
            A2aError::InvalidRequest {
                reason: "notifications are not supported".into(),
            },
            id,
            StatusCode::OK,
        );
    }
    if id.bounded_len() > crate::limits::MAX_ID_BYTES {
        return jsonrpc_error_response(
            A2aError::InvalidRequest {
                reason: "JSON-RPC id is too long".into(),
            },
            id,
            StatusCode::OK,
        );
    }
    if request.method.is_empty() || request.method.len() > 128 {
        return jsonrpc_error_response(
            A2aError::InvalidRequest {
                reason: "method is empty or too long".into(),
            },
            id,
            StatusCode::OK,
        );
    }

    let last_event_id = if request.method == METHOD_TASKS_RESUBSCRIBE {
        match last_event_id_header(&headers) {
            Ok(value) => value,
            Err(error) => return jsonrpc_error_response(error, id, StatusCode::OK),
        }
    } else {
        None
    };

    let result = dispatch_rpc::<H>(&state, &request, last_event_id).await;
    match result {
        Ok(RpcReply::Json(value)) => jsonrpc_success_response(id, value),
        Ok(RpcReply::Sse(subscription)) => sse_response(subscription, state.limits.clone()),
        Err(error) => jsonrpc_error_response(error, id, StatusCode::OK),
    }
}

enum RpcReply {
    Json(Value),
    Sse(TaskSubscription),
}

async fn dispatch_rpc<H>(
    state: &Arc<ServerState<H>>,
    request: &JsonRpcRequest,
    header_last_event_id: Option<u64>,
) -> A2aResult<RpcReply>
where
    H: A2aHandler + 'static,
{
    let params = request
        .params
        .clone()
        .ok_or_else(|| A2aError::InvalidParams {
            reason: "params are required".into(),
        })?;
    match request.method.as_str() {
        METHOD_MESSAGE_SEND => {
            let params: SendMessageParams =
                serde_json::from_value(params).map_err(|_| A2aError::InvalidParams {
                    reason: "invalid message/send parameters".into(),
                })?;
            validate_send_params(&params, state.limits())?;
            let blocking = params
                .configuration
                .as_ref()
                .map(MessageSendConfiguration::blocking)
                .unwrap_or(true);
            let history_limit = params
                .configuration
                .as_ref()
                .and_then(|configuration| configuration.history_length);
            let outcome = state
                .store
                .begin(params.message, Some(request.id.clone()), false)?;
            match outcome {
                BeginOutcome::Duplicate { task, .. } => Ok(RpcReply::Json(to_json(
                    apply_history_limit(task, history_limit, &state.limits)?,
                )?)),
                BeginOutcome::Run(run) if !blocking => {
                    // Non-blocking send still goes through the one host
                    // callback path; it simply returns the accepted working
                    // snapshot while the callback runs in the background.
                    let context = run.request_context();
                    let snapshot = context.task.clone();
                    let handler = Arc::clone(&state.handler);
                    tokio::spawn(async move {
                        match invoke_handler(&*handler, context).await {
                            Ok(update) => {
                                let _ = run.complete(update);
                            }
                            Err(_) => {
                                let _ = run.fail();
                            }
                        }
                    });
                    Ok(RpcReply::Json(to_json(apply_history_limit(
                        snapshot,
                        history_limit,
                        &state.limits,
                    )?)?))
                }
                BeginOutcome::Run(run) => {
                    let context = run.request_context();
                    let task_id = context.task_id.clone();
                    let result = match invoke_handler(&*state.handler, context).await {
                        Ok(update) => run.complete(update),
                        Err(_) => Ok(run.fail()),
                    };
                    let task = match result {
                        Ok(task) => task,
                        Err(_) => state.store.get(&task_id, None)?,
                    };
                    Ok(RpcReply::Json(to_json(apply_history_limit(
                        task,
                        history_limit,
                        &state.limits,
                    )?)?))
                }
            }
        }
        METHOD_MESSAGE_STREAM => {
            let params: SendMessageParams =
                serde_json::from_value(params).map_err(|_| A2aError::InvalidParams {
                    reason: "invalid message/stream parameters".into(),
                })?;
            validate_send_params(&params, state.limits())?;
            let outcome = state
                .store
                .begin(params.message, Some(request.id.clone()), true)?;
            match outcome {
                BeginOutcome::Duplicate {
                    subscription: Some(subscription),
                    ..
                } => Ok(RpcReply::Sse(subscription)),
                BeginOutcome::Duplicate {
                    subscription: None, ..
                } => Err(A2aError::Internal),
                BeginOutcome::Run(mut run) => {
                    let subscription = run.take_subscription().ok_or(A2aError::Internal)?;
                    let context = run.request_context();
                    let handler = Arc::clone(&state.handler);
                    tokio::spawn(async move {
                        match invoke_handler(&*handler, context).await {
                            Ok(update) => {
                                // A late completion after cancel is a no-op;
                                // an invalid host update is converted to a
                                // failed task by RunStart's drop guard.
                                let _ = run.complete(update);
                            }
                            Err(_) => {
                                let _ = run.fail();
                            }
                        }
                    });
                    Ok(RpcReply::Sse(subscription))
                }
            }
        }
        METHOD_TASKS_GET => {
            let params: TaskQueryParams =
                serde_json::from_value(params).map_err(|_| A2aError::InvalidParams {
                    reason: "invalid tasks/get parameters".into(),
                })?;
            validate_task_query(&params, state.limits())?;
            let task = state.store.get(
                &params.id,
                params.history_length.map(|length| length as usize),
            )?;
            Ok(RpcReply::Json(to_json(task)?))
        }
        METHOD_TASKS_CANCEL => {
            let params: TaskIdParams =
                serde_json::from_value(params).map_err(|_| A2aError::InvalidParams {
                    reason: "invalid tasks/cancel parameters".into(),
                })?;
            validate_task_id_params(&params, state.limits())?;
            let task = state.store.cancel(&params.id)?;
            Ok(RpcReply::Json(to_json(task)?))
        }
        METHOD_TASKS_RESUBSCRIBE => {
            let params: TaskResubscribeParams =
                serde_json::from_value(params).map_err(|_| A2aError::InvalidParams {
                    reason: "invalid tasks/resubscribe parameters".into(),
                })?;
            validate_id("task.id", &params.id)?;
            validate_metadata(&params.metadata, "params.metadata")?;
            let after_id = params.last_event_id.or(header_last_event_id);
            let subscription = state.store.subscribe(&params.id, after_id)?;
            Ok(RpcReply::Sse(subscription))
        }
        other => Err(A2aError::MethodNotFound {
            method: bounded_method(other),
        }),
    }
}

async fn events_handler<H>(
    State(state): State<Arc<ServerState<H>>>,
    Path(task_id): Path<String>,
    Query(query): Query<TaskEventsQuery>,
    headers: HeaderMap,
) -> Response
where
    H: A2aHandler + 'static,
{
    if let Err(error) = state.authenticate(&headers) {
        return auth_error_response(error);
    }
    if let Err(error) = validate_id("task.id", &task_id) {
        return jsonrpc_error_response(error, JsonRpcId::Null, StatusCode::BAD_REQUEST);
    }
    let after_id = match last_event_id_header(&headers) {
        Ok(header) => header.or(query.last_event_id),
        Err(error) => {
            return jsonrpc_error_response(error, JsonRpcId::Null, StatusCode::BAD_REQUEST);
        }
    };
    match state.store.subscribe(&task_id, after_id) {
        Ok(subscription) => sse_response(subscription, state.limits.clone()),
        Err(error) => {
            let status =
                StatusCode::from_u16(error.http_status()).unwrap_or(StatusCode::BAD_REQUEST);
            jsonrpc_error_response(error, JsonRpcId::Null, status)
        }
    }
}

fn sse_response(subscription: TaskSubscription, _limits: A2aLimits) -> Response {
    let TaskSubscription {
        replay,
        mut receiver,
        close_after_replay,
    } = subscription;
    let stream = stream! {
        let mut last_sent = 0u64;
        for envelope in replay {
            last_sent = envelope.id;
            let Some(event) = encode_sse(envelope) else {
                return;
            };
            yield Ok::<Event, Infallible>(event);
        }
        if close_after_replay {
            return;
        }
        loop {
            match receiver.recv().await {
                Ok(envelope) => {
                    if envelope.id <= last_sent {
                        continue;
                    }
                    last_sent = envelope.id;
                    let is_final = envelope.is_final;
                    let Some(event) = encode_sse(envelope) else {
                        return;
                    };
                    yield Ok::<Event, Infallible>(event);
                    if is_final {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => return,
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    };
    let keep_alive = KeepAlive::new()
        .interval(Duration::from_secs(15))
        .text("keep-alive");
    Sse::new(stream).keep_alive(keep_alive).into_response()
}

fn encode_sse(envelope: crate::store::EventEnvelope) -> Option<Event> {
    let kind = envelope.event.kind();
    let data = serde_json::to_string(&envelope.event).ok()?;
    Some(
        Event::default()
            .id(envelope.id.to_string())
            .event(kind)
            .data(data),
    )
}

fn parse_request(bytes: &[u8]) -> A2aResult<JsonRpcRequest> {
    if let Some(first) = bytes.iter().find(|byte| !byte.is_ascii_whitespace())
        && *first == b'['
    {
        return Err(A2aError::InvalidRequest {
            reason: "JSON-RPC batches are not supported".into(),
        });
    }
    serde_json::from_slice(bytes).map_err(|error| A2aError::ParseError {
        reason: format!(
            "invalid JSON at line {} column {}",
            error.line(),
            error.column()
        ),
    })
}

fn jsonrpc_success_response(id: JsonRpcId, result: Value) -> Response {
    (StatusCode::OK, Json(JsonRpcResponse::success(id, result))).into_response()
}

fn jsonrpc_error_response(error: A2aError, id: JsonRpcId, status: StatusCode) -> Response {
    (status, Json(error.to_json_rpc(id))).into_response()
}

fn auth_error_response(error: A2aError) -> Response {
    let mut response = jsonrpc_error_response(error, JsonRpcId::Null, StatusCode::UNAUTHORIZED);
    response.headers_mut().insert(
        WWW_AUTHENTICATE,
        HeaderValue::from_static("Bearer realm=\"vp-a2a\""),
    );
    response
}

fn to_json(value: impl serde::Serialize) -> A2aResult<Value> {
    serde_json::to_value(value).map_err(|_| A2aError::Internal)
}

fn apply_history_limit(
    mut task: A2aTask,
    limit: Option<u32>,
    limits: &A2aLimits,
) -> A2aResult<A2aTask> {
    if let Some(limit) = limit {
        let limit = (limit as usize).min(limits.max_task_history);
        let start = task.history.len().saturating_sub(limit);
        task.history = task.history[start..].to_vec();
    }
    Ok(task)
}

fn validate_send_params(params: &SendMessageParams, limits: &A2aLimits) -> A2aResult<()> {
    limits.validate_message(&params.message)?;
    validate_metadata(&params.metadata, "params.metadata")?;
    if let Some(configuration) = &params.configuration {
        validate_configuration(configuration)?;
    }
    Ok(())
}

fn validate_configuration(configuration: &MessageSendConfiguration) -> A2aResult<()> {
    if configuration.push_notification_config.is_some() {
        return Err(A2aError::PushNotificationNotSupported);
    }
    if configuration.accepted_output_modes.len() > MAX_OUTPUT_MODES {
        return Err(A2aError::BoundsExceeded {
            field: "configuration.acceptedOutputModes",
            max: MAX_OUTPUT_MODES,
            actual: configuration.accepted_output_modes.len(),
        });
    }
    for mode in &configuration.accepted_output_modes {
        if mode.is_empty() || mode.len() > 128 || mode.bytes().any(|b| b.is_ascii_control()) {
            return Err(A2aError::InvalidParams {
                reason: "accepted output mode is invalid".into(),
            });
        }
    }
    Ok(())
}

fn validate_task_query(params: &TaskQueryParams, _limits: &A2aLimits) -> A2aResult<()> {
    validate_id("task.id", &params.id)?;
    validate_metadata(&params.metadata, "params.metadata")
}

fn validate_task_id_params(params: &TaskIdParams, _limits: &A2aLimits) -> A2aResult<()> {
    validate_id("task.id", &params.id)?;
    validate_metadata(&params.metadata, "params.metadata")
}

fn last_event_id_header(headers: &HeaderMap) -> A2aResult<Option<u64>> {
    let Some(value) = headers.get(LAST_EVENT_ID) else {
        return Ok(None);
    };
    let value = value.to_str().map_err(|_| A2aError::InvalidParams {
        reason: "Last-Event-ID must be ASCII".into(),
    })?;
    if value.is_empty() {
        return Err(A2aError::InvalidParams {
            reason: "Last-Event-ID must not be empty".into(),
        });
    }
    value
        .parse::<u64>()
        .map(Some)
        .map_err(|_| A2aError::InvalidParams {
            reason: "Last-Event-ID must be an unsigned integer".into(),
        })
}

fn bounded_method(value: &str) -> String {
    value.chars().take(128).collect()
}

async fn fallback_handler() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({"error": "not_found"}))).into_response()
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn lock_token<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::TaskState;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn policy() -> A2aPolicy {
        A2aPolicy::new("test", "test facade", "1.0.0")
            .with_bearer_token("test-token-0123456789")
            .expect("token")
            .enable()
    }

    #[test]
    fn token_accessor_is_one_time_and_debug_redacts() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let handler = |_request: A2aRequestContext| async {
                Ok::<_, A2aError>(A2aTaskUpdate::new(TaskState::Completed))
            };
            let handle = A2aServer::start(policy(), handler).await.expect("start");
            let debug = format!("{handle:?}");
            assert!(!debug.contains("test-token-0123456789"));
            assert!(debug.contains("<available>"));
            assert_eq!(
                handle.take_token().as_deref(),
                Some("test-token-0123456789")
            );
            assert!(!handle.token_available());
            assert_eq!(handle.take_token(), None);
            handle.shutdown().await.expect("shutdown");
        });
    }

    #[test]
    fn rejected_public_bind_does_not_open_a_listener() {
        // A2aListenAddr cannot be constructed with a public address, so the
        // policy parser is the enforcement point.
        assert!("0.0.0.0:0".parse::<crate::url::A2aListenAddr>().is_err());
    }

    #[test]
    fn local_addr_is_loopback() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let calls = Arc::new(AtomicUsize::new(0));
            let calls_for_handler = Arc::clone(&calls);
            let handler = move |_request: A2aRequestContext| {
                let calls = Arc::clone(&calls_for_handler);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, A2aError>(A2aTaskUpdate::new(TaskState::Completed))
                }
            };
            let handle = A2aServer::start(policy(), handler).await.expect("start");
            assert!(is_exact_loopback(handle.local_addr().ip()));
            assert_ne!(handle.local_addr().port(), 0);
            assert!(handle.url().starts_with("http://127.0.0.1:"));
            handle.shutdown().await.expect("shutdown");
        });
    }
}

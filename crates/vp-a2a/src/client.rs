//! Loopback-only A2A client.
//!
//! Redirects are disabled and URLs are parsed as literal loopback addresses.
//! Retries are limited to connect failures (where no HTTP request could have
//! been accepted) and, for read-only GETs, timeouts. A task-creating POST is
//! never retried after a response timeout.

use std::fmt;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use async_stream::try_stream;
use futures_util::{Stream, StreamExt};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use reqwest::{Client, Response};
use serde_json::Value;

use crate::error::{A2aError, A2aResult};
use crate::jsonrpc::{
    AGENT_CARD_PATH, JSONRPC_VERSION, JsonRpcId, JsonRpcRequest, JsonRpcResponse,
    METHOD_MESSAGE_SEND, METHOD_MESSAGE_STREAM, METHOD_TASKS_CANCEL, METHOD_TASKS_GET,
    METHOD_TASKS_RESUBSCRIBE, SendMessageParams, TaskIdParams, TaskQueryParams,
    TaskResubscribeParams,
};
use crate::limits::A2aLimits;
use crate::policy::A2aSecretToken;
use crate::sse::SseDecoder;
use crate::url::A2aUrl;
use crate::wire::{A2aEventFrame, A2aTask, AgentCard, Message, validate_id};

const MAX_ATTEMPTS: usize = 3;

/// A bounded client-side SSE stream. Dropping it drops the HTTP response and
/// therefore the connection; it never reconnects automatically.
pub struct A2aEventStream {
    inner: Pin<Box<dyn Stream<Item = A2aResult<A2aEventFrame>> + Send>>,
}

impl Stream for A2aEventStream {
    type Item = A2aResult<A2aEventFrame>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

/// Client for one loopback A2A facade. The bearer token is redacted from
/// `Debug`, never appears in an error or URL, and is zeroized on drop.
pub struct A2aClient {
    base: A2aUrl,
    http: Client,
    token: A2aSecretToken,
    limits: A2aLimits,
    next_id: AtomicU64,
}

impl fmt::Debug for A2aClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("A2aClient")
            .field("base", &self.base)
            .field("token", &"<redacted>")
            .field("limits", &self.limits)
            .finish()
    }
}

impl A2aClient {
    /// Build a client. `base_url` must be an `http(s)://127.0.0.1:port` or
    /// `http(s)://[::1]:port` URL with a root path.
    pub fn new(base_url: impl AsRef<str>, token: impl Into<String>) -> A2aResult<Self> {
        let base = A2aUrl::parse(base_url.as_ref())?;
        let token = A2aSecretToken::new(token)?;
        let http = Client::builder()
            // A loopback facade must not be routed through a system proxy or
            // redirected to a public origin.
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| A2aError::ConfigInvalid {
                reason: "HTTP client setup failed".into(),
            })?;
        Ok(Self {
            base,
            http,
            token,
            limits: A2aLimits::default(),
            next_id: AtomicU64::new(1),
        })
    }

    /// Alias for [`A2aClient::new`], useful when the base URL is already
    /// known to be loopback.
    pub fn connect(base_url: impl AsRef<str>, token: impl Into<String>) -> A2aResult<Self> {
        Self::new(base_url, token)
    }

    pub fn base_url(&self) -> &A2aUrl {
        &self.base
    }

    pub fn limits(&self) -> &A2aLimits {
        &self.limits
    }

    pub fn with_limits(mut self, limits: A2aLimits) -> A2aResult<Self> {
        limits.validate()?;
        self.limits = limits;
        Ok(self)
    }

    pub fn with_timeout(mut self, timeout: Duration) -> A2aResult<Self> {
        if timeout.is_zero() {
            return Err(A2aError::ConfigInvalid {
                reason: "client timeout must be greater than zero".into(),
            });
        }
        self.http = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .connect_timeout(Duration::from_secs(5).min(timeout))
            .build()
            .map_err(|_| A2aError::ConfigInvalid {
                reason: "HTTP client setup failed".into(),
            })?;
        Ok(self)
    }

    /// Fetch and validate the Agent Card. Its URL is rechecked as loopback.
    pub async fn card(&self) -> A2aResult<AgentCard> {
        let url = self.base.join(AGENT_CARD_PATH);
        let response = self.get_with_retry(&url).await?;
        let status = response.status();
        let body = read_bounded(response, self.limits.max_body_bytes).await?;
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(A2aError::Unauthorized);
        }
        if status == reqwest::StatusCode::PAYLOAD_TOO_LARGE {
            return Err(A2aError::BoundsExceeded {
                field: "agentCard",
                max: self.limits.max_body_bytes,
                actual: body.len(),
            });
        }
        if !status.is_success() {
            return Err(self.status_error(status, &body));
        }
        self.reject_echo(&body)?;
        let card = AgentCard::decode(&body, &self.limits)?;
        Ok(card)
    }

    /// Alias for [`A2aClient::card`].
    pub async fn agent_card(&self) -> A2aResult<AgentCard> {
        self.card().await
    }

    /// Send a blocking message. The server returns the settled task.
    pub async fn send(&self, message: Message) -> A2aResult<A2aTask> {
        self.limits.validate_message(&message)?;
        let params = SendMessageParams::new(message);
        let result = self
            .call(
                METHOD_MESSAGE_SEND,
                serde_json::to_value(params).map_err(|_| A2aError::Internal)?,
                false,
            )
            .await?;
        self.decode_task(result)
    }

    /// Send without waiting for the host callback to settle. The response is
    /// the accepted `working` snapshot; poll [`A2aClient::get_task`] or use
    /// [`A2aClient::subscribe`] for completion.
    pub async fn send_non_blocking(&self, message: Message) -> A2aResult<A2aTask> {
        self.limits.validate_message(&message)?;
        let mut params = SendMessageParams::new(message);
        params.configuration = Some(crate::jsonrpc::MessageSendConfiguration {
            blocking: Some(false),
            ..Default::default()
        });
        let result = self
            .call(
                METHOD_MESSAGE_SEND,
                serde_json::to_value(params).map_err(|_| A2aError::Internal)?,
                false,
            )
            .await?;
        self.decode_task(result)
    }

    /// Open an SSE stream for `message/stream`. The returned stream starts
    /// with the server's retained submitted/working frames and ends after a
    /// final status frame (or at the bounded transport limit).
    pub async fn stream(&self, message: Message) -> A2aResult<A2aEventStream> {
        self.limits.validate_message(&message)?;
        let params = SendMessageParams::new(message);
        self.post_sse(
            METHOD_MESSAGE_STREAM,
            serde_json::to_value(params).map_err(|_| A2aError::Internal)?,
            None,
        )
        .await
    }

    /// Read a task snapshot.
    pub async fn get_task(&self, task_id: &str) -> A2aResult<A2aTask> {
        validate_id("task.id", task_id)?;
        let params = TaskQueryParams::new(task_id);
        let result = self
            .call(
                METHOD_TASKS_GET,
                serde_json::to_value(params).map_err(|_| A2aError::Internal)?,
                true,
            )
            .await?;
        self.decode_task(result)
    }

    /// Request cancellation.
    pub async fn cancel(&self, task_id: &str) -> A2aResult<A2aTask> {
        validate_id("task.id", task_id)?;
        let params = TaskIdParams::new(task_id);
        let result = self
            .call(
                METHOD_TASKS_CANCEL,
                serde_json::to_value(params).map_err(|_| A2aError::Internal)?,
                false,
            )
            .await?;
        self.decode_task(result)
    }

    /// Resubscribe using the JSON-RPC method. `last_event_id` replays retained
    /// frames strictly after that id.
    pub async fn resubscribe(
        &self,
        task_id: &str,
        last_event_id: Option<u64>,
    ) -> A2aResult<A2aEventStream> {
        validate_id("task.id", task_id)?;
        let mut params = TaskResubscribeParams::new(task_id);
        params.last_event_id = last_event_id;
        self.post_sse(
            METHOD_TASKS_RESUBSCRIBE,
            serde_json::to_value(params).map_err(|_| A2aError::Internal)?,
            Some(task_id.to_string()),
        )
        .await
    }

    /// Subscribe through the GET events route. This is also the disconnect
    /// recovery path for a stream whose POST response was lost.
    pub async fn subscribe(
        &self,
        task_id: &str,
        last_event_id: Option<u64>,
    ) -> A2aResult<A2aEventStream> {
        validate_id("task.id", task_id)?;
        let path = format!("/a2a/tasks/{}/events", encode_path_segment(task_id));
        let response = self
            .get_stream(&self.base.join(&path), last_event_id)
            .await?;
        let expected = Some(task_id.to_string());
        self.event_stream(response, expected)
    }

    async fn call(&self, method: &str, params: Value, idempotent: bool) -> A2aResult<Value> {
        let request = JsonRpcRequest::new(self.next_request_id(), method, params);
        let body = serde_json::to_vec(&request).map_err(|_| A2aError::Internal)?;
        if body.len() > self.limits.max_body_bytes {
            return Err(A2aError::BoundsExceeded {
                field: "body",
                max: self.limits.max_body_bytes,
                actual: body.len(),
            });
        }
        let response = self
            .post_json(&self.base.join("/a2a"), &body, idempotent, false)
            .await?;
        let status = response.status();
        let response_limit = self.limits.max_task_response_bytes();
        let bytes = read_bounded(response, response_limit).await?;
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(A2aError::Unauthorized);
        }
        if status == reqwest::StatusCode::PAYLOAD_TOO_LARGE {
            return Err(A2aError::BoundsExceeded {
                field: "response",
                max: response_limit,
                actual: bytes.len(),
            });
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(A2aError::StoreFull);
        }
        self.reject_echo(&bytes)?;
        let response: JsonRpcResponse =
            serde_json::from_slice(&bytes).map_err(|_| A2aError::InvalidAgentResponse {
                reason: "JSON-RPC response is not valid JSON".into(),
            })?;
        if response.jsonrpc != JSONRPC_VERSION {
            return Err(A2aError::InvalidAgentResponse {
                reason: "JSON-RPC response version is not 2.0".into(),
            });
        }
        if response.id != request.id {
            return Err(A2aError::InvalidAgentResponse {
                reason: "JSON-RPC response id does not match the request".into(),
            });
        }
        match (response.result, response.error) {
            (Some(_), Some(_)) => Err(A2aError::InvalidAgentResponse {
                reason: "JSON-RPC response has both result and error".into(),
            }),
            (None, Some(error)) => Err(self.sanitize_error(A2aError::from_json_rpc(&error))),
            (Some(result), None) if status.is_success() => Ok(result),
            (Some(_), None) => Err(A2aError::Protocol {
                reason: format!("HTTP status {}", status.as_u16()),
            }),
            (None, None) => Err(A2aError::InvalidAgentResponse {
                reason: "JSON-RPC response has neither result nor error".into(),
            }),
        }
    }

    async fn post_sse(
        &self,
        method: &str,
        params: Value,
        expected_task_id: Option<String>,
    ) -> A2aResult<A2aEventStream> {
        let request = JsonRpcRequest::new(self.next_request_id(), method, params);
        let body = serde_json::to_vec(&request).map_err(|_| A2aError::Internal)?;
        if body.len() > self.limits.max_body_bytes {
            return Err(A2aError::BoundsExceeded {
                field: "body",
                max: self.limits.max_body_bytes,
                actual: body.len(),
            });
        }
        let response = self
            .post_json(&self.base.join("/a2a"), &body, false, true)
            .await?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if status.is_success() && content_type.starts_with("text/event-stream") {
            return self.event_stream(response, expected_task_id);
        }
        // Validation failures for streaming methods are returned as a normal
        // JSON-RPC error body, sometimes with HTTP 200. Preserve the typed
        // error instead of reporting a generic bad content type.
        let bytes = read_bounded(response, self.limits.max_body_bytes).await?;
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(A2aError::Unauthorized);
        }
        if !status.is_success() {
            return Err(self.status_error(status, &bytes));
        }
        self.reject_echo(&bytes)?;
        if let Ok(response) = serde_json::from_slice::<JsonRpcResponse>(&bytes)
            && let Some(error) = response.error
        {
            return Err(self.sanitize_error(A2aError::from_json_rpc(&error)));
        }
        Err(A2aError::InvalidAgentResponse {
            reason: "stream response is not text/event-stream".into(),
        })
    }

    async fn get_stream(&self, url: &str, last_event_id: Option<u64>) -> A2aResult<Response> {
        let mut last_error = None;
        for attempt in 0..MAX_ATTEMPTS {
            let mut request = self
                .http
                .get(url)
                .header(ACCEPT, "text/event-stream")
                .header(AUTHORIZATION, self.auth_header()?);
            if let Some(id) = last_event_id {
                request = request.header("last-event-id", id.to_string());
            }
            match request.send().await {
                Ok(response) => {
                    let status = response.status();
                    let content_type = response
                        .headers()
                        .get(CONTENT_TYPE)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default();
                    if status.is_success() && content_type.starts_with("text/event-stream") {
                        return Ok(response);
                    }
                    let bytes = read_bounded(response, self.limits.max_body_bytes).await?;
                    if status == reqwest::StatusCode::UNAUTHORIZED
                        || status == reqwest::StatusCode::FORBIDDEN
                    {
                        return Err(A2aError::Unauthorized);
                    }
                    if !status.is_success() {
                        return Err(self.status_error(status, &bytes));
                    }
                    self.reject_echo(&bytes)?;
                    if let Ok(response) = serde_json::from_slice::<JsonRpcResponse>(&bytes)
                        && let Some(error) = response.error
                    {
                        return Err(self.sanitize_error(A2aError::from_json_rpc(&error)));
                    }
                    return Err(A2aError::InvalidAgentResponse {
                        reason: "event response is not text/event-stream".into(),
                    });
                }
                Err(error) => {
                    let retryable = error.is_connect() || error.is_timeout();
                    last_error = Some(error);
                    if attempt + 1 < MAX_ATTEMPTS && retryable {
                        tokio::time::sleep(backoff(attempt)).await;
                        continue;
                    }
                    break;
                }
            }
        }
        Err(self.transport_error(last_error))
    }

    async fn get_with_retry(&self, url: &str) -> A2aResult<Response> {
        let mut last_error = None;
        for attempt in 0..MAX_ATTEMPTS {
            let request = self
                .http
                .get(url)
                .header(ACCEPT, "application/json")
                .header(AUTHORIZATION, self.auth_header()?);
            match request.send().await {
                Ok(response) => return Ok(response),
                Err(error) => {
                    let retryable = error.is_connect() || error.is_timeout();
                    last_error = Some(error);
                    if attempt + 1 < MAX_ATTEMPTS && retryable {
                        tokio::time::sleep(backoff(attempt)).await;
                        continue;
                    }
                    break;
                }
            }
        }
        Err(self.transport_error(last_error))
    }

    async fn post_json(
        &self,
        url: &str,
        body: &[u8],
        idempotent: bool,
        accept_sse: bool,
    ) -> A2aResult<Response> {
        let accept = if accept_sse {
            "text/event-stream"
        } else {
            "application/json"
        };
        let mut last_error = None;
        for attempt in 0..MAX_ATTEMPTS {
            let request = self
                .http
                .post(url)
                .header(ACCEPT, accept)
                .header(CONTENT_TYPE, "application/json")
                .header(AUTHORIZATION, self.auth_header()?)
                .body(body.to_vec());
            match request.send().await {
                Ok(response) => return Ok(response),
                Err(error) => {
                    // A connect failure means the request was not accepted.
                    // A timeout may have happened after acceptance, so it is
                    // retried only for explicitly idempotent calls (never for
                    // message/send, message/stream, or tasks/cancel).
                    let retryable = error.is_connect() || (idempotent && error.is_timeout());
                    last_error = Some(error);
                    if attempt + 1 < MAX_ATTEMPTS && retryable {
                        tokio::time::sleep(backoff(attempt)).await;
                        continue;
                    }
                    break;
                }
            }
        }
        Err(self.transport_error(last_error))
    }

    fn event_stream(
        &self,
        response: Response,
        expected_task_id: Option<String>,
    ) -> A2aResult<A2aEventStream> {
        let limits = self.limits.clone();
        let token = self.token.clone();
        let stream = try_stream! {
            let mut bytes = response.bytes_stream();
            let mut decoder = SseDecoder::new(&limits, expected_task_id);
            while let Some(chunk) = bytes.next().await {
                let chunk = chunk.map_err(|error| {
                    let mut reason = error.to_string();
                    if reason.chars().count() > 256 {
                        reason = reason.chars().take(256).collect();
                    }
                    A2aError::Transport {
                        reason: token.redact(&reason),
                    }
                })?;
                for frame in decoder.push(&chunk)? {
                    if token.echo_checked() && frame_contains_secret(&frame, token.expose()) {
                        Err::<(), _>(A2aError::Protocol {
                            reason: "remote stream contained a credential".into(),
                        })?;
                    }
                    yield frame;
                }
            }
            if let Some(frame) = decoder.finish()? {
                if token.echo_checked() && frame_contains_secret(&frame, token.expose()) {
                    Err::<(), _>(A2aError::Protocol {
                        reason: "remote stream contained a credential".into(),
                    })?;
                }
                yield frame;
            }
        };
        Ok(A2aEventStream {
            inner: Box::pin(stream),
        })
    }

    fn reject_echo(&self, bytes: &[u8]) -> A2aResult<()> {
        if self.token.echo_checked() && contains_bytes(bytes, self.token.expose().as_bytes()) {
            return Err(A2aError::Protocol {
                reason: "remote response contained a credential".into(),
            });
        }
        Ok(())
    }

    fn decode_task(&self, value: Value) -> A2aResult<A2aTask> {
        let bytes = serde_json::to_vec(&value).map_err(|_| A2aError::Internal)?;
        self.reject_echo(&bytes)?;
        let task: A2aTask =
            serde_json::from_value(value).map_err(|_| A2aError::InvalidAgentResponse {
                reason: "agent returned an invalid task projection".into(),
            })?;
        task.validate(&self.limits)
            .map_err(|_| A2aError::InvalidAgentResponse {
                reason: "agent returned an out-of-bounds task projection".into(),
            })?;
        Ok(task)
    }

    fn status_error(&self, status: reqwest::StatusCode, body: &[u8]) -> A2aError {
        if self.reject_echo(body).is_err() {
            return A2aError::Protocol {
                reason: "remote error contained a credential".into(),
            };
        }
        if status == reqwest::StatusCode::PAYLOAD_TOO_LARGE {
            return A2aError::BoundsExceeded {
                field: "response",
                max: self.limits.max_body_bytes,
                actual: body.len(),
            };
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return A2aError::StoreFull;
        }
        if let Ok(response) = serde_json::from_slice::<JsonRpcResponse>(body)
            && let Some(error) = response.error
        {
            return self.sanitize_error(A2aError::from_json_rpc(&error));
        }
        A2aError::Protocol {
            reason: format!("HTTP status {}", status.as_u16()),
        }
    }

    fn sanitize_error(&self, error: A2aError) -> A2aError {
        let rendered = error.to_string();
        if self.token.echo_checked() && rendered.contains(self.token.expose()) {
            A2aError::Protocol {
                reason: "remote error contained a credential and was redacted".into(),
            }
        } else {
            error
        }
    }

    fn transport_error(&self, error: Option<reqwest::Error>) -> A2aError {
        let Some(error) = error else {
            return A2aError::Transport {
                reason: "request failed".into(),
            };
        };
        let mut reason = error.to_string();
        if reason.chars().count() > 256 {
            reason = reason.chars().take(256).collect();
        }
        A2aError::Transport {
            reason: self.token.redact(&reason),
        }
    }

    fn auth_header(&self) -> A2aResult<HeaderValue> {
        let value = format!("Bearer {}", self.token.expose());
        HeaderValue::from_str(&value).map_err(|_| A2aError::ConfigInvalid {
            reason: "bearer token cannot be encoded as an HTTP header".into(),
        })
    }

    fn next_request_id(&self) -> JsonRpcId {
        let value = self.next_id.fetch_add(1, Ordering::SeqCst);
        JsonRpcId::Number(value as i64)
    }
}

async fn read_bounded(mut response: Response, max: usize) -> A2aResult<Vec<u8>> {
    if let Some(length) = response.content_length()
        && length > max as u64
    {
        return Err(A2aError::BoundsExceeded {
            field: "response",
            max,
            actual: usize::try_from(length).unwrap_or(usize::MAX),
        });
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| A2aError::Transport {
            reason: error.to_string().chars().take(256).collect(),
        })?
    {
        let next = body.len().saturating_add(chunk.len());
        if next > max {
            return Err(A2aError::BoundsExceeded {
                field: "response",
                max,
                actual: next,
            });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn backoff(attempt: usize) -> Duration {
    Duration::from_millis(50 * (attempt as u64 + 1))
}

fn frame_contains_secret(frame: &A2aEventFrame, token: &str) -> bool {
    serde_json::to_vec(&frame.event)
        .map(|bytes| contains_bytes(&bytes, token.as_bytes()))
        .unwrap_or(false)
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn encode_path_segment(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            output.push(byte as char);
        } else {
            output.push('%');
            output.push_str(&format!("{byte:02X}"));
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_urls_are_rejected() {
        let error =
            A2aClient::new("http://example.com:8080", "token-0123456789").expect_err("public URL");
        assert_eq!(error.code(), crate::error::A2aErrorCode::LoopbackRequired);
        assert!(A2aClient::new("http://127.0.0.1:8080", "token-0123456789").is_ok());
    }

    #[test]
    fn debug_redacts_the_token() {
        let client = A2aClient::new("http://127.0.0.1:8080", "token-0123456789").expect("client");
        let debug = format!("{client:?}");
        assert!(!debug.contains("token-0123456789"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn path_segments_are_encoded() {
        assert_eq!(encode_path_segment("task-1"), "task-1");
        assert_eq!(encode_path_segment("a/b"), "a%2Fb");
    }
}

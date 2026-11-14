//! MCP HTTP transports.
//!
//! Two related wire formats are supported:
//!
//! * **Streamable HTTP** (`McpTransport::Http`) posts JSON-RPC to a single
//!   endpoint and accepts either a JSON body or an SSE response. The server may
//!   return an `Mcp-Session-Id` header that must be sent on later requests.
//! * **Legacy HTTP+SSE** (`McpTransport::Sse`) opens a long-lived GET stream,
//!   waits for its `endpoint` event, posts JSON-RPC to that endpoint, and reads
//!   responses from the stream.
//!
//! Both transports share the same conservative message classifier: responses
//! matching the active request are returned, notifications are ignored, and
//! unsupported server-initiated requests are answered with JSON-RPC -32601.
//! Header values are expanded from the daemon environment before use and are
//! never logged.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use eventsource_stream::{Event as SseEvent, EventStreamError, Eventsource};
use futures_util::{StreamExt, stream::BoxStream};
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue};
use reqwest::{Client, Response, StatusCode, Url};
use serde_json::{Value, json};
use vp_protocol::types::McpServer;

use super::{McpSessionInfo, timeout_plain, transport};

const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_SSE_EVENT_BYTES: usize = 8 * 1024 * 1024;
const ACK_TIMEOUT: Duration = Duration::from_secs(5);

type SseStream =
    BoxStream<'static, std::result::Result<SseEvent, EventStreamError<reqwest::Error>>>;

/// MCP Streamable HTTP client session.
pub(super) struct HttpSession {
    name: String,
    client: Client,
    endpoint: Url,
    headers: HeaderMap,
    session_id: Option<String>,
    protocol_version: Option<String>,
    next_id: u64,
    alive: bool,
    info: McpSessionInfo,
}

impl HttpSession {
    pub(super) async fn connect(server: &McpServer) -> Result<Self> {
        let endpoint = parse_endpoint(server)?;
        let headers = super::resolve_http_headers(&server.headers)?;
        let client = http_client(&endpoint)?;
        let mut session = Self {
            name: server.name.clone(),
            client,
            endpoint,
            headers,
            session_id: None,
            protocol_version: None,
            next_id: 1,
            alive: true,
            info: McpSessionInfo::default(),
        };
        let initialized = session
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "VerlinPark",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            )
            .await?;
        let version = initialized
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or("2025-06-18")
            .to_string();
        session.protocol_version = Some(version);
        session.info = McpSessionInfo::from_initialize(&initialized);
        session
            .notify("notifications/initialized", json!({}))
            .await?;
        Ok(session)
    }

    pub(super) fn is_alive(&mut self) -> bool {
        self.alive
    }

    pub(super) fn info(&self) -> McpSessionInfo {
        self.info.clone()
    }

    pub(super) async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let message = json!({"jsonrpc": "2.0", "method": method, "params": params});
        self.post(message, false, None).await.map(|_| ())
    }

    pub(super) async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.request_preemptible(method, params, None, None).await
    }

    pub(super) async fn request_preemptible(
        &mut self,
        method: &str,
        params: Value,
        token: Option<&vp_providers::PreemptToken>,
        handler: Option<super::elicitation::ServerRequestHandler>,
    ) -> Result<Value> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let response = match token {
            Some(token) => {
                tokio::select! {
                    _ = token.cancelled() => {
                        let _ = self
                            .notify(
                                "notifications/cancelled",
                                json!({
                                    "requestId": id,
                                    "reason": format!("preempted while running {method}"),
                                }),
                            )
                            .await;
                        bail!("MCP {method} was preempted");
                    }
                    response = self.post(message, true, handler.clone()) => response,
                }
            }
            None => self.post(message, true, handler).await,
        };
        response?.ok_or_else(|| transport(format!("MCP {method} was accepted without a response")))
    }

    pub(super) async fn close(&mut self) {
        self.alive = false;
        let Some(session_id) = self.session_id.as_deref() else {
            return;
        };
        let Ok(mut request) = self
            .client
            .delete(self.endpoint.clone())
            .header("Mcp-Session-Id", session_id)
            .build()
        else {
            return;
        };
        if let Some(version) = self.protocol_version.as_deref()
            && let Ok(value) = HeaderValue::from_str(version)
        {
            request.headers_mut().insert("MCP-Protocol-Version", value);
        }
        let _ = tokio::time::timeout(ACK_TIMEOUT, self.client.execute(request)).await;
    }

    async fn post(
        &mut self,
        message: Value,
        expect_response: bool,
        handler: Option<super::elicitation::ServerRequestHandler>,
    ) -> Result<Option<Value>> {
        let mut request = self
            .client
            .post(self.endpoint.clone())
            .header(ACCEPT, "application/json, text/event-stream")
            .header(CONTENT_TYPE, "application/json")
            .headers(self.headers.clone())
            .json(&message);
        if let Some(session_id) = self.session_id.as_deref()
            && let Ok(value) = HeaderValue::from_str(session_id)
        {
            request = request.header("Mcp-Session-Id", value);
        }
        if let Some(version) = self.protocol_version.as_deref()
            && let Ok(value) = HeaderValue::from_str(version)
        {
            request = request.header("MCP-Protocol-Version", value);
        }
        let response = timeout_plain("HTTP request", request.send())
            .await?
            .map_err(|error| transport(format!("MCP HTTP {} failed: {error}", self.name)))?;
        if let Some(session_id) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
        {
            self.session_id = Some(session_id.to_string());
        }
        let status = response.status();
        if matches!(status, StatusCode::NOT_FOUND | StatusCode::GONE) {
            self.alive = false;
            return Err(transport(format!(
                "MCP HTTP {} session expired (HTTP {status})",
                self.name
            )));
        }
        let is_sse = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.to_ascii_lowercase().starts_with("text/event-stream"));
        if status == StatusCode::ACCEPTED && !is_sse {
            if expect_response {
                return Err(transport(format!(
                    "MCP HTTP {} accepted a request without a response body",
                    self.name
                )));
            }
            return Ok(None);
        }
        if !status.is_success() {
            let retryable = status.is_server_error()
                || status == StatusCode::TOO_MANY_REQUESTS
                || status == StatusCode::REQUEST_TIMEOUT;
            let message = format!("MCP HTTP {} returned HTTP {status}", self.name);
            if retryable {
                return Err(transport(message));
            }
            bail!("{message}");
        }
        if is_sse {
            if !expect_response {
                return Ok(None);
            }
            return self
                .read_sse(
                    response,
                    Some(json!({"id": self.next_id.saturating_sub(1)})),
                    handler,
                )
                .await;
        }
        let body = read_body(response).await?;
        if body.trim().is_empty() {
            if expect_response {
                return Err(transport(format!(
                    "MCP HTTP {} returned an empty response body",
                    self.name
                )));
            }
            return Ok(None);
        }
        if !expect_response {
            return Ok(None);
        }
        let value: Value = serde_json::from_str(&body)
            .with_context(|| format!("parsing MCP HTTP {} response", self.name))?;
        self.handle_messages(
            value,
            Some(json!({"id": self.next_id.saturating_sub(1)})),
            handler,
        )
        .await
    }

    async fn read_sse(
        &mut self,
        response: Response,
        expected: Option<Value>,
        handler: Option<super::elicitation::ServerRequestHandler>,
    ) -> Result<Option<Value>> {
        let mut events = response.bytes_stream().eventsource();
        loop {
            let event = match timeout_plain("HTTP SSE response", events.next()).await? {
                Some(Ok(event)) => event,
                Some(Err(error)) => {
                    self.alive = false;
                    return Err(transport(format!(
                        "MCP HTTP {} SSE stream failed: {error}",
                        self.name
                    )));
                }
                None => {
                    self.alive = false;
                    return Err(transport(format!(
                        "MCP HTTP {} SSE stream ended before a response",
                        self.name
                    )));
                }
            };
            if !is_rpc_event(&event) {
                continue;
            }
            if event.data.len() > MAX_SSE_EVENT_BYTES {
                bail!(
                    "MCP HTTP {} SSE event exceeds {} MiB",
                    self.name,
                    MAX_SSE_EVENT_BYTES / 1024 / 1024
                );
            }
            if event.data.trim().is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(&event.data)
                .with_context(|| format!("parsing MCP HTTP {} SSE event", self.name))?;
            if let Some(result) = self
                .handle_messages(value, expected.clone(), handler.clone())
                .await?
            {
                return Ok(Some(result));
            }
        }
    }

    async fn handle_messages(
        &mut self,
        value: Value,
        expected: Option<Value>,
        handler: Option<super::elicitation::ServerRequestHandler>,
    ) -> Result<Option<Value>> {
        let incoming = classify_messages(value, expected.as_ref())?;
        for request in &incoming.server_requests {
            let id = request.get("id").cloned().unwrap_or(Value::Null);
            let method = request
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let response = match (method, handler.as_ref()) {
                ("elicitation/create", Some(handler)) => match handler(request.clone()).await {
                    Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                    Err(error) => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {"code": -32603, "message": format!("{error:#}")}
                    }),
                },
                _ => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32601,
                        "message": "VerlinPark does not implement this server-initiated MCP method"
                    }
                }),
            };
            self.post_response(response).await?;
        }
        Ok(incoming.response)
    }

    async fn post_response(&mut self, message: Value) -> Result<()> {
        let mut request = self
            .client
            .post(self.endpoint.clone())
            .header(ACCEPT, "application/json, text/event-stream")
            .header(CONTENT_TYPE, "application/json")
            .headers(self.headers.clone())
            .json(&message);
        if let Some(session_id) = self.session_id.as_deref()
            && let Ok(value) = HeaderValue::from_str(session_id)
        {
            request = request.header("Mcp-Session-Id", value);
        }
        if let Some(version) = self.protocol_version.as_deref()
            && let Ok(value) = HeaderValue::from_str(version)
        {
            request = request.header("MCP-Protocol-Version", value);
        }
        if let Ok(response) = tokio::time::timeout(ACK_TIMEOUT, request.send()).await
            && let Ok(response) = response
        {
            let _ = tokio::time::timeout(ACK_TIMEOUT, response.bytes()).await;
        }
        Ok(())
    }
}

/// Legacy MCP HTTP+SSE client session.
pub(super) struct SseSession {
    name: String,
    client: Client,
    post_url: Url,
    headers: HeaderMap,
    events: SseStream,
    next_id: u64,
    alive: bool,
    info: McpSessionInfo,
}

impl SseSession {
    pub(super) async fn connect(server: &McpServer) -> Result<Self> {
        let endpoint = parse_endpoint(server)?;
        let headers = super::resolve_http_headers(&server.headers)?;
        let client = http_client(&endpoint)?;
        let request = client
            .get(endpoint.clone())
            .header(ACCEPT, "text/event-stream")
            .headers(headers.clone());
        let response = timeout_plain("SSE connect", request.send())
            .await?
            .map_err(|error| transport(format!("MCP SSE {} failed: {error}", server.name)))?;
        let status = response.status();
        if !status.is_success() {
            let message = format!("MCP SSE {} returned HTTP {status}", server.name);
            if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
                return Err(transport(message));
            }
            bail!("{message}");
        }
        let mut session = Self {
            name: server.name.clone(),
            client,
            post_url: endpoint.clone(),
            headers,
            events: response.bytes_stream().eventsource().boxed(),
            next_id: 1,
            alive: true,
            info: McpSessionInfo::default(),
        };
        session.await_endpoint(endpoint).await?;
        let initialized = session
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "VerlinPark",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            )
            .await?;
        session.info = McpSessionInfo::from_initialize(&initialized);
        session
            .notify("notifications/initialized", json!({}))
            .await?;
        Ok(session)
    }

    pub(super) fn is_alive(&mut self) -> bool {
        self.alive
    }

    pub(super) fn info(&self) -> McpSessionInfo {
        self.info.clone()
    }

    async fn await_endpoint(&mut self, base: Url) -> Result<()> {
        for _ in 0..32 {
            let event = self.next_event().await?;
            if event.data.len() > MAX_SSE_EVENT_BYTES {
                bail!("MCP SSE endpoint event exceeds the configured limit");
            }
            let data = event.data.trim();
            if data.is_empty() {
                continue;
            }
            if event.event != "endpoint" {
                continue;
            }
            let post_url = base
                .join(data)
                .with_context(|| format!("MCP SSE endpoint is not a valid URL: {data}"))?;
            if post_url.scheme() != base.scheme()
                || post_url.host_str() != base.host_str()
                || post_url.port_or_known_default() != base.port_or_known_default()
            {
                bail!("MCP SSE endpoint must stay on the configured origin");
            }
            self.post_url = post_url;
            return Ok(());
        }
        bail!("MCP SSE server did not announce an endpoint event")
    }

    pub(super) async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let message = json!({"jsonrpc": "2.0", "method": method, "params": params});
        self.post(message, false, None).await.map(|_| ())
    }

    pub(super) async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.request_preemptible(method, params, None, None).await
    }

    pub(super) async fn request_preemptible(
        &mut self,
        method: &str,
        params: Value,
        token: Option<&vp_providers::PreemptToken>,
        handler: Option<super::elicitation::ServerRequestHandler>,
    ) -> Result<Value> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let response = match token {
            Some(token) => {
                tokio::select! {
                    _ = token.cancelled() => {
                        let _ = self
                            .notify(
                                "notifications/cancelled",
                                json!({
                                    "requestId": id,
                                    "reason": format!("preempted while running {method}"),
                                }),
                            )
                            .await;
                        bail!("MCP SSE {method} was preempted");
                    }
                    response = self.post(message, true, handler.clone()) => response,
                }
            }
            None => self.post(message, true, handler).await,
        };
        response?
            .ok_or_else(|| transport(format!("MCP SSE {method} was accepted without a response")))
    }

    pub(super) async fn close(&mut self) {
        self.alive = false;
        self.events = futures_util::stream::empty().boxed();
    }

    async fn post(
        &mut self,
        message: Value,
        expect_response: bool,
        handler: Option<super::elicitation::ServerRequestHandler>,
    ) -> Result<Option<Value>> {
        let response = timeout_plain(
            "SSE POST",
            self.client
                .post(self.post_url.clone())
                .header(ACCEPT, "application/json, text/event-stream")
                .header(CONTENT_TYPE, "application/json")
                .headers(self.headers.clone())
                .json(&message)
                .send(),
        )
        .await?
        .map_err(|error| transport(format!("MCP SSE {} POST failed: {error}", self.name)))?;
        let status = response.status();
        if matches!(status, StatusCode::NOT_FOUND | StatusCode::GONE) {
            self.alive = false;
            return Err(transport(format!(
                "MCP SSE {} endpoint expired (HTTP {status})",
                self.name
            )));
        }
        let is_sse = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.to_ascii_lowercase().starts_with("text/event-stream"));
        if status == StatusCode::ACCEPTED && !is_sse {
            if expect_response {
                return self
                    .read_response(
                        Some(json!({"id": self.next_id.saturating_sub(1)})),
                        handler.clone(),
                    )
                    .await;
            }
            return Ok(None);
        }
        if !status.is_success() {
            let retryable = status.is_server_error()
                || status == StatusCode::TOO_MANY_REQUESTS
                || status == StatusCode::REQUEST_TIMEOUT;
            let message = format!("MCP SSE {} POST returned HTTP {status}", self.name);
            if retryable {
                return Err(transport(message));
            }
            bail!("{message}");
        }
        if is_sse {
            if !expect_response {
                return Ok(None);
            }
            let events = response.bytes_stream().eventsource();
            return self
                .read_response_from(
                    events,
                    Some(json!({"id": self.next_id.saturating_sub(1)})),
                    handler,
                )
                .await;
        }
        let body = read_body(response).await?;
        if body.trim().is_empty() {
            if expect_response {
                return self
                    .read_response(
                        Some(json!({"id": self.next_id.saturating_sub(1)})),
                        handler.clone(),
                    )
                    .await;
            }
            return Ok(None);
        }
        if !expect_response {
            return Ok(None);
        }
        let value: Value = serde_json::from_str(&body)
            .with_context(|| format!("parsing MCP SSE {} POST response", self.name))?;
        self.handle_messages(
            value,
            Some(json!({"id": self.next_id.saturating_sub(1)})),
            handler,
        )
        .await
    }

    async fn read_response(
        &mut self,
        expected: Option<Value>,
        handler: Option<super::elicitation::ServerRequestHandler>,
    ) -> Result<Option<Value>> {
        loop {
            let event = self.next_event().await?;
            if !is_rpc_event(&event) {
                continue;
            }
            if event.data.len() > MAX_SSE_EVENT_BYTES {
                bail!("MCP SSE event exceeds the configured limit");
            }
            if !is_rpc_event(&event) || event.data.trim().is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(&event.data)
                .with_context(|| format!("parsing MCP SSE {} event", self.name))?;
            if let Some(result) = self
                .handle_messages(value, expected.clone(), handler.clone())
                .await?
            {
                return Ok(Some(result));
            }
        }
    }

    async fn read_response_from<S>(
        &mut self,
        events: S,
        expected: Option<Value>,
        handler: Option<super::elicitation::ServerRequestHandler>,
    ) -> Result<Option<Value>>
    where
        S: futures_util::Stream<
                Item = std::result::Result<SseEvent, EventStreamError<reqwest::Error>>,
            > + Send,
    {
        let mut events = std::pin::pin!(events);
        loop {
            let event = match timeout_plain("SSE response", events.next()).await? {
                Some(Ok(event)) => event,
                Some(Err(error)) => {
                    self.alive = false;
                    return Err(transport(format!(
                        "MCP SSE {} stream failed: {error}",
                        self.name
                    )));
                }
                None => {
                    self.alive = false;
                    return Err(transport(format!(
                        "MCP SSE {} stream ended before a response",
                        self.name
                    )));
                }
            };
            if !is_rpc_event(&event) || event.data.trim().is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(&event.data)
                .with_context(|| format!("parsing MCP SSE {} event", self.name))?;
            if let Some(result) = self
                .handle_messages(value, expected.clone(), handler.clone())
                .await?
            {
                return Ok(Some(result));
            }
        }
    }

    async fn handle_messages(
        &mut self,
        value: Value,
        expected: Option<Value>,
        handler: Option<super::elicitation::ServerRequestHandler>,
    ) -> Result<Option<Value>> {
        let incoming = classify_messages(value, expected.as_ref())?;
        for request in &incoming.server_requests {
            let id = request.get("id").cloned().unwrap_or(Value::Null);
            let method = request
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let response = match (method, handler.as_ref()) {
                ("elicitation/create", Some(handler)) => match handler(request.clone()).await {
                    Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                    Err(error) => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {"code": -32603, "message": format!("{error:#}")}
                    }),
                },
                _ => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32601,
                        "message": "VerlinPark does not implement this server-initiated MCP method"
                    }
                }),
            };
            let _ = timeout_plain(
                "SSE ack",
                self.client
                    .post(self.post_url.clone())
                    .header(CONTENT_TYPE, "application/json")
                    .headers(self.headers.clone())
                    .json(&response)
                    .send(),
            )
            .await;
        }
        Ok(incoming.response)
    }

    async fn next_event(&mut self) -> Result<SseEvent> {
        match timeout_plain("SSE event", self.events.next()).await? {
            Some(Ok(event)) => Ok(event),
            Some(Err(error)) => {
                self.alive = false;
                Err(transport(format!(
                    "MCP SSE {} stream failed: {error}",
                    self.name
                )))
            }
            None => {
                self.alive = false;
                Err(transport(format!(
                    "MCP SSE {} stream ended unexpectedly",
                    self.name
                )))
            }
        }
    }
}

fn is_rpc_event(event: &SseEvent) -> bool {
    event.event.is_empty() || event.event == "message"
}

struct Incoming {
    response: Option<Value>,
    server_requests: Vec<Value>,
}

fn classify_messages(value: Value, expected: Option<&Value>) -> Result<Incoming> {
    let mut incoming = Incoming {
        response: None,
        server_requests: Vec::new(),
    };
    classify_value(value, expected, &mut incoming)?;
    Ok(incoming)
}

fn classify_value(value: Value, expected: Option<&Value>, incoming: &mut Incoming) -> Result<()> {
    match value {
        Value::Array(items) => {
            for item in items {
                classify_value(item, expected, incoming)?;
            }
        }
        Value::Object(object) => {
            if object.get("method").and_then(Value::as_str).is_some() {
                if object.contains_key("id") {
                    incoming.server_requests.push(Value::Object(object));
                }
                return Ok(());
            }
            let Some(id) = object.get("id") else {
                return Ok(());
            };
            if expected.is_some_and(|expected| expected.get("id") != Some(id)) {
                return Ok(());
            }
            if let Some(error) = object.get("error") {
                let message = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error");
                bail!("MCP request failed: {message}");
            }
            incoming.response = Some(
                object
                    .get("result")
                    .cloned()
                    .ok_or_else(|| anyhow!("MCP response has no result"))?,
            );
        }
        _ => bail!("MCP response is not a JSON-RPC object"),
    }
    Ok(())
}

fn parse_endpoint(server: &McpServer) -> Result<Url> {
    let url = server
        .url
        .as_deref()
        .ok_or_else(|| anyhow!("MCP HTTP server {} has no url", server.name))?;
    let url =
        Url::parse(url).with_context(|| format!("parsing MCP HTTP URL for {}", server.name))?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("MCP HTTP URL must use http or https");
    }
    Ok(url)
}

fn http_client(endpoint: &Url) -> Result<Client> {
    let allowed = endpoint.clone();
    Client::builder()
        .user_agent(format!("VerlinPark/{}", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 3 {
                attempt.stop()
            } else {
                let same_origin = attempt.url().scheme() == allowed.scheme()
                    && attempt.url().host_str() == allowed.host_str()
                    && attempt.url().port_or_known_default() == allowed.port_or_known_default();
                if same_origin {
                    attempt.follow()
                } else {
                    attempt.stop()
                }
            }
        }))
        .build()
        .context("building MCP HTTP client")
}

async fn read_body(response: Response) -> Result<String> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BODY_BYTES as u64)
    {
        bail!(
            "MCP HTTP response exceeds {} MiB",
            MAX_BODY_BYTES / 1024 / 1024
        );
    }
    let bytes = timeout_plain("HTTP response body", response.bytes())
        .await?
        .map_err(|error| transport(format!("MCP HTTP response body failed: {error}")))?;
    if bytes.len() > MAX_BODY_BYTES {
        bail!(
            "MCP HTTP response exceeds {} MiB",
            MAX_BODY_BYTES / 1024 / 1024
        );
    }
    String::from_utf8(bytes.to_vec()).context("MCP HTTP response is not UTF-8")
}

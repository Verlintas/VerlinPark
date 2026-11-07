mod http;
mod stdio;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use reqwest::Url;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use vp_protocol::types::{
    McpProbeResult, McpPrompt, McpPromptListResult, McpPromptMessage, McpReadResourceResult,
    McpResource, McpResourceContent, McpResourceListResult, McpResourceTemplate, McpServer,
    McpServerConfig, McpTool, McpTransport,
};

const MCP_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_PAGES: usize = 50;
const MAX_ITEMS: usize = 2_000;
const MAX_NAME_CHARS: usize = 80;
const MAX_URL_CHARS: usize = 2_048;
const MAX_HEADERS: usize = 32;
const MAX_HEADER_NAME_CHARS: usize = 128;
const MAX_HEADER_VALUE_CHARS: usize = 4_096;
const MAX_ENV_ENTRIES: usize = 64;
const MAX_ARGUMENTS: usize = 64;
const MAX_ARGUMENT_CHARS: usize = 4_096;
const RESERVED_HEADERS: &[&str] = &[
    "accept",
    "content-type",
    "content-length",
    "host",
    "connection",
    "transfer-encoding",
    "mcp-session-id",
    "mcp-protocol-version",
    "last-event-id",
];
const SENSITIVE_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "x-api-key",
    "api-key",
    "x-auth-token",
    "x-access-token",
];

/// A transport failure (closed pipe, process exit, timeout) as opposed to a
/// JSON-RPC error returned by a healthy MCP server. Tool calls are never
/// retried, but a dead connection can be discarded so the next call reconnects.
#[derive(Debug)]
struct TransportError(String);

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for TransportError {}

fn transport(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(TransportError(message.into()))
}

fn is_transport(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| cause.is::<TransportError>())
}

#[derive(Debug, Clone, Default)]
pub(super) struct McpSessionInfo {
    server_name: String,
    server_version: String,
    protocol_version: String,
}

impl McpSessionInfo {
    fn from_initialize(value: &Value) -> Self {
        let server = value.get("serverInfo");
        Self {
            server_name: server
                .and_then(|server| server.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            server_version: server
                .and_then(|server| server.get("version"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            protocol_version: value
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        }
    }
}

enum McpSession {
    Stdio(stdio::StdioSession),
    Http(http::HttpSession),
    Sse(http::SseSession),
}

impl McpSession {
    async fn connect(server: &McpServer) -> Result<Self> {
        match server.transport {
            McpTransport::Stdio => Ok(Self::Stdio(stdio::StdioSession::connect(server).await?)),
            McpTransport::Http => Ok(Self::Http(http::HttpSession::connect(server).await?)),
            McpTransport::Sse => Ok(Self::Sse(http::SseSession::connect(server).await?)),
        }
    }

    fn is_alive(&mut self) -> bool {
        match self {
            Self::Stdio(session) => session.is_alive(),
            Self::Http(session) => session.is_alive(),
            Self::Sse(session) => session.is_alive(),
        }
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        match self {
            Self::Stdio(session) => session.request(method, params).await,
            Self::Http(session) => session.request(method, params).await,
            Self::Sse(session) => session.request(method, params).await,
        }
    }

    fn info(&self) -> McpSessionInfo {
        match self {
            Self::Stdio(session) => session.info(),
            Self::Http(session) => session.info(),
            Self::Sse(session) => session.info(),
        }
    }

    async fn close(&mut self) {
        match self {
            Self::Stdio(session) => session.close().await,
            Self::Http(session) => session.close().await,
            Self::Sse(session) => session.close().await,
        }
    }
}

/// Validate and normalize a user-supplied MCP server configuration.
///
/// The daemon stores the normalized form so transport-specific fields cannot
/// become ambiguous. Sensitive HTTP headers must use `${ENV_VAR}` indirection;
/// this keeps API keys out of SQLite and out of logs.
pub(crate) fn normalize_config(mut config: McpServerConfig) -> Result<McpServerConfig> {
    config.name = config.name.trim().to_string();
    if config.name.is_empty() {
        bail!("MCP server name is required");
    }
    if config.name.chars().count() > MAX_NAME_CHARS {
        bail!("MCP server name must be at most {MAX_NAME_CHARS} characters");
    }
    config.command = config.command.trim().to_string();
    config.args = config
        .args
        .into_iter()
        .map(|argument| argument.trim_end().to_string())
        .filter(|argument| !argument.is_empty())
        .collect();
    if config.args.len() > MAX_ARGUMENTS {
        bail!("MCP server may have at most {MAX_ARGUMENTS} arguments");
    }
    if config
        .args
        .iter()
        .any(|argument| argument.chars().count() > MAX_ARGUMENT_CHARS)
    {
        bail!("MCP server arguments must be at most {MAX_ARGUMENT_CHARS} characters");
    }
    if config.env.len() > MAX_ENV_ENTRIES {
        bail!("MCP server may have at most {MAX_ENV_ENTRIES} environment entries");
    }
    for (key, value) in &config.env {
        if key.trim().is_empty() || key.len() > MAX_HEADER_NAME_CHARS {
            bail!("MCP environment variable names must be non-empty and bounded");
        }
        if value.chars().count() > MAX_HEADER_VALUE_CHARS {
            bail!("MCP environment values must be at most {MAX_HEADER_VALUE_CHARS} characters");
        }
    }
    match config.transport {
        McpTransport::Stdio => {
            if config.command.is_empty() {
                bail!("MCP stdio server command is required");
            }
            if config.url.is_some() || !config.headers.is_empty() {
                bail!("MCP stdio servers cannot use url or headers");
            }
        }
        McpTransport::Http | McpTransport::Sse => {
            if !config.command.is_empty() || !config.args.is_empty() || !config.env.is_empty() {
                bail!("MCP HTTP/SSE servers do not use command, args, or env");
            }
            let url = config
                .url
                .as_deref()
                .ok_or_else(|| anyhow!("MCP HTTP/SSE server url is required"))?;
            config.url = Some(normalize_url(url)?);
            config.headers = normalize_headers(config.headers)?;
        }
    }
    Ok(config)
}

fn normalize_url(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        bail!("MCP server url is required");
    }
    if value.chars().count() > MAX_URL_CHARS {
        bail!("MCP server url must be at most {MAX_URL_CHARS} characters");
    }
    let url = Url::parse(value).map_err(|_| anyhow!("MCP server url is not a valid URL"))?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("MCP server url must use http or https");
    }
    if url.host_str().is_none() {
        bail!("MCP server url must include a host");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("MCP server url must not contain credentials");
    }
    if url.fragment().is_some() {
        bail!("MCP server url must not contain a fragment");
    }
    Ok(url.to_string())
}

fn normalize_headers(headers: BTreeMap<String, String>) -> Result<BTreeMap<String, String>> {
    if headers.len() > MAX_HEADERS {
        bail!("MCP server may have at most {MAX_HEADERS} headers");
    }
    let mut normalized = BTreeMap::new();
    for (name, value) in headers {
        let name = name.trim().to_string();
        if name.is_empty() || name.chars().count() > MAX_HEADER_NAME_CHARS {
            bail!("MCP header names must be non-empty and bounded");
        }
        let parsed = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| anyhow!("MCP header name is not valid: {name}"))?;
        let lower = parsed.as_str().to_string();
        if RESERVED_HEADERS.contains(&lower.as_str()) {
            bail!("MCP header {name} is reserved by the transport");
        }
        let value = value.trim().to_string();
        if value.chars().count() > MAX_HEADER_VALUE_CHARS {
            bail!("MCP header {name} must be at most {MAX_HEADER_VALUE_CHARS} characters");
        }
        if value.contains('\n') || value.contains('\r') {
            bail!("MCP header {name} must not contain line breaks");
        }
        validate_env_template(&value).map_err(|error| anyhow!("MCP header {name}: {error}"))?;
        if SENSITIVE_HEADERS.contains(&lower.as_str()) && !value.contains("${") {
            bail!(
                "MCP header {name} must use ${{ENV_VAR}} indirection instead of storing a secret"
            );
        }
        if normalized.insert(lower, value).is_some() {
            bail!("MCP header {name} is configured more than once");
        }
    }
    Ok(normalized)
}

fn validate_env_template(value: &str) -> Result<()> {
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            bail!("unterminated ${{ENV_VAR}} reference");
        };
        let name = &after[..end];
        if name.is_empty()
            || name.len() > MAX_HEADER_NAME_CHARS
            || !name.chars().enumerate().all(|(index, ch)| {
                ch == '_' || ch.is_ascii_alphanumeric() && (index > 0 || !ch.is_ascii_digit())
            })
            || name.chars().next().is_some_and(|ch| ch.is_ascii_digit())
        {
            bail!("invalid environment variable reference");
        }
        rest = &after[end + 1..];
    }
    Ok(())
}

fn expand_env_template_with(
    value: &str,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<String> {
    validate_env_template(value)?;
    let mut result = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        result.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after
            .find('}')
            .ok_or_else(|| anyhow!("unterminated environment reference"))?;
        let name = &after[..end];
        let replacement =
            lookup(name).ok_or_else(|| anyhow!("environment variable {name} is not set"))?;
        result.push_str(&replacement);
        rest = &after[end + 1..];
    }
    result.push_str(rest);
    if result.contains('\n') || result.contains('\r') {
        bail!("expanded header value contains a line break");
    }
    Ok(result)
}

pub(crate) fn build_http_headers(headers: &BTreeMap<String, String>) -> Result<HeaderMap> {
    let mut map = HeaderMap::new();
    for (name, value) in headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| anyhow!("MCP header name is not valid: {name}"))?;
        let value = expand_env_template_with(value, |variable| std::env::var(variable).ok())?;
        let value = HeaderValue::from_str(&value)
            .map_err(|_| anyhow!("MCP header {name} has an invalid value"))?;
        map.append(name, value);
    }
    Ok(map)
}

fn resolve_http_headers(headers: &BTreeMap<String, String>) -> Result<HeaderMap> {
    build_http_headers(headers)
}

struct McpConnection {
    server: McpServer,
    session: Mutex<McpSession>,
    last_used: StdMutex<Instant>,
}

impl McpConnection {
    async fn info(&self) -> McpSessionInfo {
        self.session.lock().await.info()
    }

    async fn close(&self) {
        self.session.lock().await.close().await;
    }

    fn touch(&self) {
        if let Ok(mut last_used) = self.last_used.lock() {
            *last_used = Instant::now();
        }
    }

    fn idle_for(&self) -> Duration {
        self.last_used
            .lock()
            .map(|last_used| last_used.elapsed())
            .unwrap_or_default()
    }
}

/// Keeps one transport session per configured server. Calls to the same server
/// are serialized because stdio and legacy SSE carry a single JSON-RPC stream;
/// Streamable HTTP also benefits from serialized requests and a shared session
/// id. Connections are torn down after an idle period or when the config changes.
pub struct McpManager {
    connections: Mutex<HashMap<String, Arc<McpConnection>>>,
    idle: Duration,
}

impl McpManager {
    pub fn new(idle: Duration) -> Self {
        Self {
            connections: Mutex::new(HashMap::new()),
            idle,
        }
    }

    pub fn from_env() -> Self {
        let seconds = std::env::var("VERLINPARK_MCP_IDLE_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(300)
            .max(1);
        Self::new(Duration::from_secs(seconds))
    }

    pub async fn list_tools(&self, server: &McpServer) -> Result<Vec<McpTool>> {
        ensure_enabled(server)?;
        match self.list_tools_once(server).await {
            Ok(tools) => Ok(tools),
            Err(error) if is_transport(&error) => {
                // tools/list is idempotent, so one reconnect is safe.
                self.list_tools_once(server).await
            }
            Err(error) => Err(error),
        }
    }

    pub async fn call_tool(
        &self,
        server: &McpServer,
        tool: &str,
        arguments: Value,
    ) -> Result<Value> {
        ensure_enabled(server)?;
        if tool.trim().is_empty() {
            bail!("MCP tool name is required");
        }
        let connection = self.connection(server).await?;
        let result = self
            .request_on(
                &connection,
                "tools/call",
                json!({"name": tool, "arguments": arguments}),
            )
            .await;
        if let Err(error) = &result
            && is_transport(error)
        {
            // Never retry a tool call: it may have side effects even when the
            // response was lost. Reconnect on the next call instead.
            self.invalidate_connection(&server.id, &connection).await;
        }
        result
    }

    pub async fn list_resources(&self, server: &McpServer) -> Result<McpResourceListResult> {
        ensure_enabled(server)?;
        let mut resources = Vec::new();
        let mut cursor: Option<String> = None;
        let mut seen = HashSet::new();
        let mut complete = false;
        for _ in 0..MAX_PAGES {
            let params = cursor
                .as_ref()
                .map(|cursor| json!({ "cursor": cursor }))
                .unwrap_or_else(|| json!({}));
            let result = self.read_request(server, "resources/list", params).await?;
            resources.extend(parse_resources(&result)?);
            if resources.len() > MAX_ITEMS {
                bail!(
                    "MCP server {} exposes more than {MAX_ITEMS} resources",
                    server.name
                );
            }
            match result.get("nextCursor").and_then(Value::as_str) {
                Some(next) if !next.is_empty() => {
                    if !seen.insert(next.to_string()) {
                        bail!("MCP resources/list cursor repeated: {next}");
                    }
                    cursor = Some(next.to_string());
                }
                _ => {
                    complete = true;
                    break;
                }
            }
        }
        if !complete {
            bail!("MCP resources/list exceeded {MAX_PAGES} pages");
        }
        let templates = match self.list_resource_templates(server).await {
            Ok(templates) => templates,
            Err(error) => {
                tracing::debug!(%error, "MCP resources/templates/list unavailable");
                Vec::new()
            }
        };
        Ok(McpResourceListResult {
            server_id: server.id.clone(),
            resources,
            templates,
        })
    }

    pub async fn read_resource(
        &self,
        server: &McpServer,
        uri: &str,
    ) -> Result<McpReadResourceResult> {
        ensure_enabled(server)?;
        if uri.trim().is_empty() {
            bail!("MCP resource uri is required");
        }
        let result = self
            .read_request(server, "resources/read", json!({ "uri": uri }))
            .await?;
        let contents = result
            .get("contents")
            .cloned()
            .ok_or_else(|| anyhow!("MCP resources/read response has no contents"))?
            .as_array()
            .ok_or_else(|| anyhow!("MCP resources/read contents is not an array"))?
            .iter()
            .map(|item| McpResourceContent {
                uri: item
                    .get("uri")
                    .and_then(Value::as_str)
                    .unwrap_or(uri)
                    .to_string(),
                mime_type: item
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                text: item.get("text").and_then(Value::as_str).map(str::to_string),
                blob: item.get("blob").and_then(Value::as_str).map(str::to_string),
            })
            .collect();
        Ok(McpReadResourceResult {
            server_id: server.id.clone(),
            uri: uri.to_string(),
            contents,
        })
    }

    pub async fn list_prompts(&self, server: &McpServer) -> Result<McpPromptListResult> {
        ensure_enabled(server)?;
        let mut prompts = Vec::new();
        let mut cursor: Option<String> = None;
        let mut seen = HashSet::new();
        let mut complete = false;
        for _ in 0..MAX_PAGES {
            let params = cursor
                .as_ref()
                .map(|cursor| json!({ "cursor": cursor }))
                .unwrap_or_else(|| json!({}));
            let result = self.read_request(server, "prompts/list", params).await?;
            prompts.extend(parse_prompts(&result)?);
            if prompts.len() > MAX_ITEMS {
                bail!(
                    "MCP server {} exposes more than {MAX_ITEMS} prompts",
                    server.name
                );
            }
            match result.get("nextCursor").and_then(Value::as_str) {
                Some(next) if !next.is_empty() => {
                    if !seen.insert(next.to_string()) {
                        bail!("MCP prompts/list cursor repeated: {next}");
                    }
                    cursor = Some(next.to_string());
                }
                _ => {
                    complete = true;
                    break;
                }
            }
        }
        if !complete {
            bail!("MCP prompts/list exceeded {MAX_PAGES} pages");
        }
        Ok(McpPromptListResult {
            server_id: server.id.clone(),
            prompts,
        })
    }

    pub async fn get_prompt(
        &self,
        server: &McpServer,
        name: &str,
        arguments: std::collections::BTreeMap<String, String>,
    ) -> Result<vp_protocol::types::McpGetPromptResult> {
        ensure_enabled(server)?;
        if name.trim().is_empty() {
            bail!("MCP prompt name is required");
        }
        let result = self
            .read_request(
                server,
                "prompts/get",
                json!({ "name": name, "arguments": arguments }),
            )
            .await?;
        let messages = result
            .get("messages")
            .and_then(Value::as_array)
            .map(|messages| {
                messages
                    .iter()
                    .map(|message| McpPromptMessage {
                        role: message
                            .get("role")
                            .and_then(Value::as_str)
                            .unwrap_or("user")
                            .to_string(),
                        content: message.get("content").cloned().unwrap_or(Value::Null),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(vp_protocol::types::McpGetPromptResult {
            server_id: server.id.clone(),
            name: name.to_string(),
            description: result
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            messages,
        })
    }

    pub async fn probe(&self, server: &McpServer) -> Result<McpProbeResult> {
        ensure_enabled(server)?;
        let connection = self.connection(server).await?;
        let info = connection.info().await;
        let tools = match self.request_on(&connection, "tools/list", json!({})).await {
            Ok(tools) => tools,
            Err(error) => {
                if is_transport(&error) {
                    self.invalidate_connection(&server.id, &connection).await;
                }
                return Err(error);
            }
        };
        let tool_count = array_len(&tools, "tools");
        let (resources_supported, resource_count) = match self
            .request_on(&connection, "resources/list", json!({}))
            .await
        {
            Ok(resources) => (true, array_len(&resources, "resources")),
            Err(error) => {
                if is_transport(&error) {
                    self.invalidate_connection(&server.id, &connection).await;
                }
                (false, 0)
            }
        };
        let (prompts_supported, prompt_count) = match self
            .request_on(&connection, "prompts/list", json!({}))
            .await
        {
            Ok(prompts) => (true, array_len(&prompts, "prompts")),
            Err(error) => {
                if is_transport(&error) {
                    self.invalidate_connection(&server.id, &connection).await;
                }
                (false, 0)
            }
        };
        Ok(McpProbeResult {
            server_id: server.id.clone(),
            name: server.name.clone(),
            transport: server.transport,
            server_name: info.server_name,
            server_version: info.server_version,
            protocol_version: info.protocol_version,
            tools: tool_count,
            resources: resource_count,
            prompts: prompt_count,
            resources_supported,
            prompts_supported,
        })
    }

    pub async fn invalidate(&self, server_id: &str) {
        let removed = self.connections.lock().await.remove(server_id);
        if let Some(connection) = removed {
            connection.close().await;
        }
    }

    pub async fn invalidate_all(&self) {
        let connections = {
            let mut connections = self.connections.lock().await;
            std::mem::take(&mut *connections)
        };
        for connection in connections.into_values() {
            connection.close().await;
        }
    }

    pub async fn sweep_idle(&self) -> usize {
        let idle = self.idle;
        let stale = {
            let mut connections = self.connections.lock().await;
            let mut stale = Vec::new();
            let mut active = HashMap::new();
            for (id, connection) in connections.drain() {
                if connection.idle_for() < idle {
                    active.insert(id, connection);
                } else {
                    stale.push(connection);
                }
            }
            *connections = active;
            stale
        };
        let removed = stale.len();
        for connection in stale {
            connection.close().await;
        }
        removed
    }

    pub async fn connection_count(&self) -> usize {
        self.connections.lock().await.len()
    }

    async fn connection(&self, server: &McpServer) -> Result<Arc<McpConnection>> {
        self.sweep_idle().await;
        let existing = self.connections.lock().await.get(&server.id).cloned();
        if let Some(connection) = existing {
            if connection.server == *server {
                let alive = connection.session.lock().await.is_alive();
                if alive {
                    connection.touch();
                    return Ok(connection);
                }
            }
            let removed = {
                let mut connections = self.connections.lock().await;
                if connections
                    .get(&server.id)
                    .is_some_and(|current| Arc::ptr_eq(current, &connection))
                {
                    connections.remove(&server.id)
                } else {
                    None
                }
            };
            if let Some(removed) = removed {
                removed.close().await;
            }
        }
        let session = McpSession::connect(server).await?;
        let fresh = Arc::new(McpConnection {
            server: server.clone(),
            session: Mutex::new(session),
            last_used: StdMutex::new(Instant::now()),
        });
        let mut connections = self.connections.lock().await;
        if let Some(connection) = connections.get(&server.id)
            && connection.server == *server
        {
            let existing = connection.clone();
            connection.touch();
            drop(connections);
            fresh.close().await;
            return Ok(existing);
        }
        connections.insert(server.id.clone(), fresh.clone());
        Ok(fresh)
    }

    async fn request_on(
        &self,
        connection: &McpConnection,
        method: &str,
        params: Value,
    ) -> Result<Value> {
        let mut session = connection.session.lock().await;
        let result = timeout(method, session.request(method, params)).await;
        drop(session);
        if result.is_ok() {
            connection.touch();
        }
        result
    }

    async fn read_request(&self, server: &McpServer, method: &str, params: Value) -> Result<Value> {
        match self.read_request_once(server, method, params.clone()).await {
            Ok(result) => Ok(result),
            Err(error) if is_transport(&error) => {
                self.read_request_once(server, method, params).await
            }
            Err(error) => Err(error),
        }
    }

    async fn read_request_once(
        &self,
        server: &McpServer,
        method: &str,
        params: Value,
    ) -> Result<Value> {
        let connection = self.connection(server).await?;
        let result = self.request_on(&connection, method, params).await;
        if let Err(error) = &result
            && is_transport(error)
        {
            self.invalidate_connection(&server.id, &connection).await;
        }
        result
    }

    async fn list_resource_templates(
        &self,
        server: &McpServer,
    ) -> Result<Vec<McpResourceTemplate>> {
        let mut templates = Vec::new();
        let mut cursor: Option<String> = None;
        let mut seen = HashSet::new();
        let mut complete = false;
        for _ in 0..MAX_PAGES {
            let params = cursor
                .as_ref()
                .map(|cursor| json!({ "cursor": cursor }))
                .unwrap_or_else(|| json!({}));
            let result = self
                .read_request(server, "resources/templates/list", params)
                .await?;
            templates.extend(parse_resource_templates(&result)?);
            if templates.len() > MAX_ITEMS {
                bail!(
                    "MCP server {} exposes more than {MAX_ITEMS} resource templates",
                    server.name
                );
            }
            match result.get("nextCursor").and_then(Value::as_str) {
                Some(next) if !next.is_empty() => {
                    if !seen.insert(next.to_string()) {
                        bail!("MCP resources/templates/list cursor repeated: {next}");
                    }
                    cursor = Some(next.to_string());
                }
                _ => {
                    complete = true;
                    break;
                }
            }
        }
        if !complete {
            bail!("MCP resources/templates/list exceeded {MAX_PAGES} pages");
        }
        Ok(templates)
    }

    async fn list_tools_once(&self, server: &McpServer) -> Result<Vec<McpTool>> {
        let connection = self.connection(server).await?;
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        let mut seen = HashSet::new();
        for _ in 0..MAX_PAGES {
            let params = cursor
                .as_ref()
                .map(|cursor| json!({ "cursor": cursor }))
                .unwrap_or_else(|| json!({}));
            let result = self.request_on(&connection, "tools/list", params).await;
            let result = match result {
                Ok(result) => result,
                Err(error) => {
                    if is_transport(&error) {
                        self.invalidate_connection(&server.id, &connection).await;
                    }
                    return Err(error);
                }
            };
            tools.extend(parse_tools(&result)?);
            if tools.len() > MAX_ITEMS {
                bail!(
                    "MCP server {} exposes more than {MAX_ITEMS} tools",
                    server.name
                );
            }
            match result.get("nextCursor").and_then(Value::as_str) {
                Some(next) if !next.is_empty() => {
                    if !seen.insert(next.to_string()) {
                        bail!("MCP tools/list cursor repeated: {next}");
                    }
                    cursor = Some(next.to_string());
                }
                _ => return Ok(tools),
            }
        }
        bail!("MCP tools/list exceeded {MAX_PAGES} pages")
    }

    async fn invalidate_connection(&self, server_id: &str, connection: &Arc<McpConnection>) {
        let removed = {
            let mut connections = self.connections.lock().await;
            if connections
                .get(server_id)
                .is_some_and(|current| Arc::ptr_eq(current, connection))
            {
                connections.remove(server_id)
            } else {
                None
            }
        };
        if let Some(removed) = removed {
            removed.close().await;
        }
    }
}

fn array_len(value: &Value, field: &str) -> usize {
    value
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0)
}

fn ensure_enabled(server: &McpServer) -> Result<()> {
    if !server.enabled {
        bail!("MCP server {} is disabled", server.name);
    }
    Ok(())
}

fn parse_resources(result: &Value) -> Result<Vec<McpResource>> {
    result
        .get("resources")
        .cloned()
        .ok_or_else(|| anyhow!("MCP resources/list response has no resources"))?
        .as_array()
        .ok_or_else(|| anyhow!("MCP resources/list resources is not an array"))?
        .iter()
        .map(|resource| {
            Ok(McpResource {
                uri: resource
                    .get("uri")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("MCP resource has no uri"))?
                    .to_string(),
                name: resource
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                description: resource
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                mime_type: resource
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
        })
        .collect()
}

fn parse_resource_templates(result: &Value) -> Result<Vec<McpResourceTemplate>> {
    result
        .get("resourceTemplates")
        .cloned()
        .ok_or_else(|| anyhow!("MCP resources/templates/list response has no templates"))?
        .as_array()
        .ok_or_else(|| anyhow!("MCP resourceTemplates is not an array"))?
        .iter()
        .map(|template| {
            Ok(McpResourceTemplate {
                uri_template: template
                    .get("uriTemplate")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("MCP resource template has no uriTemplate"))?
                    .to_string(),
                name: template
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                description: template
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                mime_type: template
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
        })
        .collect()
}

fn parse_prompts(result: &Value) -> Result<Vec<McpPrompt>> {
    result
        .get("prompts")
        .cloned()
        .ok_or_else(|| anyhow!("MCP prompts/list response has no prompts"))?
        .as_array()
        .ok_or_else(|| anyhow!("MCP prompts/list prompts is not an array"))?
        .iter()
        .map(|prompt| {
            Ok(McpPrompt {
                name: prompt
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("MCP prompt has no name"))?
                    .to_string(),
                description: prompt
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                arguments: prompt
                    .get("arguments")
                    .and_then(Value::as_array)
                    .map(|arguments| {
                        arguments
                            .iter()
                            .filter_map(|argument| {
                                Some(vp_protocol::types::McpPromptArgument {
                                    name: argument.get("name")?.as_str()?.to_string(),
                                    description: argument
                                        .get("description")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default()
                                        .to_string(),
                                    required: argument
                                        .get("required")
                                        .and_then(Value::as_bool)
                                        .unwrap_or(false),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        })
        .collect()
}

fn parse_tools(result: &Value) -> Result<Vec<McpTool>> {
    result
        .get("tools")
        .cloned()
        .ok_or_else(|| anyhow!("MCP tools/list response has no tools"))?
        .as_array()
        .ok_or_else(|| anyhow!("MCP tools/list tools is not an array"))?
        .iter()
        .map(|tool| {
            let name = tool
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("MCP tool has no name"))?;
            Ok(McpTool {
                name: name.to_string(),
                description: tool
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                input_schema: tool
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object"})),
            })
        })
        .collect()
}

async fn timeout<T>(
    label: &str,
    future: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    timeout_plain(label, future).await?
}

async fn timeout_plain<T>(label: &str, future: impl std::future::Future<Output = T>) -> Result<T> {
    match tokio::time::timeout(MCP_TIMEOUT, future).await {
        Ok(result) => Ok(result),
        Err(_) => Err(transport(format!(
            "MCP {label} timed out after {}s",
            MCP_TIMEOUT.as_secs()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::convert::Infallible;
    use std::sync::atomic::{AtomicBool, Ordering};

    use axum::Router;
    use axum::body::Body;
    use axum::extract::State as AxumState;
    use axum::http::{HeaderMap, StatusCode, header};
    use axum::response::sse::{Event, Sse};
    use axum::response::{IntoResponse, Response};
    use axum::routing::{get, post};
    use tempfile::TempDir;
    use tokio::sync::mpsc;
    use tokio_stream::wrappers::UnboundedReceiverStream;

    type TestSseSender = mpsc::UnboundedSender<Result<Event, Infallible>>;

    #[derive(Clone)]
    struct TestMcpState {
        requests: Arc<StdMutex<Vec<TestRequest>>>,
        sse_tx: Arc<tokio::sync::Mutex<Option<TestSseSender>>>,
        session_id: Arc<StdMutex<Option<String>>>,
        expire_tools_once: Arc<AtomicBool>,
        require_session: bool,
        sse_mode: bool,
    }

    #[derive(Debug)]
    struct TestRequest {
        method: String,
        session: Option<String>,
        x_test: Option<String>,
    }

    async fn start_test_server(
        require_session: bool,
        sse_mode: bool,
        legacy: bool,
    ) -> (String, Arc<TestMcpState>, tokio::task::JoinHandle<()>) {
        let state = Arc::new(TestMcpState {
            requests: Arc::new(StdMutex::new(Vec::new())),
            sse_tx: Arc::new(tokio::sync::Mutex::new(None)),
            session_id: Arc::new(StdMutex::new(None)),
            expire_tools_once: Arc::new(AtomicBool::new(false)),
            require_session,
            sse_mode,
        });
        let app = if legacy {
            Router::new()
                .route("/sse", get(legacy_sse))
                .route("/message", post(legacy_post))
        } else {
            Router::new()
                .route("/acp", post(acp_json))
                .route("/acp-sse", post(acp_sse))
        }
        .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{address}"), state, handle)
    }

    async fn acp_json(
        AxumState(state): AxumState<Arc<TestMcpState>>,
        headers: HeaderMap,
        body: String,
    ) -> Response {
        handle_http_rpc(&state, &headers, &body, false).await
    }

    async fn acp_sse(
        AxumState(state): AxumState<Arc<TestMcpState>>,
        headers: HeaderMap,
        body: String,
    ) -> Response {
        handle_http_rpc(&state, &headers, &body, true).await
    }

    async fn handle_http_rpc(
        state: &Arc<TestMcpState>,
        headers: &HeaderMap,
        body: &str,
        force_sse: bool,
    ) -> Response {
        let message: Value = serde_json::from_str(body).unwrap();
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let session = headers
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let x_test = headers
            .get("x-test")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        state.requests.lock().unwrap().push(TestRequest {
            method: method.clone(),
            session: session.clone(),
            x_test,
        });
        if method == "initialize" {
            *state.session_id.lock().unwrap() = Some("test-session".to_string());
            return json_response(
                StatusCode::OK,
                json!({
                    "jsonrpc": "2.0",
                    "id": message.get("id").cloned().unwrap_or(Value::Null),
                    "result": {
                        "protocolVersion": "2025-06-18",
                        "capabilities": {},
                        "serverInfo": {"name": "test", "version": "1"}
                    }
                }),
                Some("test-session"),
            );
        }
        if state.require_session && session.as_deref() != Some("test-session") {
            return StatusCode::NOT_FOUND.into_response();
        }
        if method == "tools/list" && state.expire_tools_once.swap(false, Ordering::SeqCst) {
            *state.session_id.lock().unwrap() = None;
            return StatusCode::NOT_FOUND.into_response();
        }
        if method == "notifications/initialized" {
            return StatusCode::ACCEPTED.into_response();
        }
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        let result = match method.as_str() {
            "tools/list" => json!({
                "tools": [{
                    "name": "echo",
                    "description": "Echo text",
                    "inputSchema": {"type": "object"}
                }]
            }),
            "tools/call" => json!({
                "content": [{
                    "type": "text",
                    "text": message
                        .get("params")
                        .and_then(|params| params.get("arguments"))
                        .and_then(|arguments| arguments.get("text"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                }]
            }),
            "resources/list" => json!({"resources": []}),
            "prompts/list" => json!({"prompts": []}),
            _ => json!({}),
        };
        if force_sse || (state.sse_mode && method == "tools/list") {
            let notification = json!({
                "jsonrpc": "2.0",
                "method": "notifications/message",
                "params": {"level": "info", "data": "working"}
            });
            let response = json!({"jsonrpc": "2.0", "id": id, "result": result});
            let body = format!(
                "event: message\ndata: {}\n\nevent: message\ndata: {}\n\n",
                notification, response
            );
            return Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/event-stream")
                .body(Body::from(body))
                .unwrap();
        }
        json_response(
            StatusCode::OK,
            json!({"jsonrpc": "2.0", "id": id, "result": result}),
            None,
        )
    }

    fn json_response(status: StatusCode, value: Value, session: Option<&str>) -> Response {
        let mut builder = Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(session) = session {
            builder = builder.header("mcp-session-id", session);
        }
        builder.body(Body::from(value.to_string())).unwrap()
    }

    async fn legacy_sse(AxumState(state): AxumState<Arc<TestMcpState>>) -> impl IntoResponse {
        let (tx, rx) = mpsc::unbounded_channel();
        *state.sse_tx.lock().await = Some(tx.clone());
        let _ = tx.send(Ok(Event::default().event("endpoint").data("/message")));
        Sse::new(UnboundedReceiverStream::new(rx))
    }

    async fn legacy_post(
        AxumState(state): AxumState<Arc<TestMcpState>>,
        body: String,
    ) -> StatusCode {
        let message: Value = serde_json::from_str(&body).unwrap();
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        state.requests.lock().unwrap().push(TestRequest {
            method: method.clone(),
            session: None,
            x_test: None,
        });
        if method == "notifications/initialized" {
            return StatusCode::ACCEPTED;
        }
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        let result = match method.as_str() {
            "initialize" => json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "serverInfo": {"name": "legacy-test", "version": "1"}
            }),
            "tools/list" => json!({
                "tools": [{
                    "name": "echo",
                    "description": "Echo text",
                    "inputSchema": {"type": "object"}
                }]
            }),
            "tools/call" => json!({
                "content": [{
                    "type": "text",
                    "text": message
                        .get("params")
                        .and_then(|params| params.get("arguments"))
                        .and_then(|arguments| arguments.get("text"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                }]
            }),
            _ => json!({}),
        };
        let response = json!({"jsonrpc": "2.0", "id": id, "result": result});
        if let Some(tx) = state.sse_tx.lock().await.as_ref() {
            let _ = tx.send(Ok(Event::default()
                .event("message")
                .data(response.to_string())));
        }
        StatusCode::ACCEPTED
    }

    fn http_server(url: &str, headers: BTreeMap<String, String>) -> McpServer {
        McpServer {
            id: "http-server".to_string(),
            name: "http".to_string(),
            transport: McpTransport::Http,
            command: String::new(),
            args: Vec::new(),
            env: BTreeMap::new(),
            url: Some(url.to_string()),
            headers,
            enabled: true,
            created_at: "2026-11-08T00:00:00Z".to_string(),
        }
    }

    fn legacy_server(url: &str) -> McpServer {
        McpServer {
            id: "sse-server".to_string(),
            name: "sse".to_string(),
            transport: McpTransport::Sse,
            command: String::new(),
            args: Vec::new(),
            env: BTreeMap::new(),
            url: Some(url.to_string()),
            headers: BTreeMap::new(),
            enabled: true,
            created_at: "2026-11-08T00:00:00Z".to_string(),
        }
    }

    #[tokio::test]
    async fn streamable_http_lists_and_calls_tools() {
        let (base, state, server) = start_test_server(true, false, false).await;
        let mut headers = BTreeMap::new();
        headers.insert("x-test".to_string(), "verlinpark".to_string());
        let config = http_server(&format!("{base}/acp"), headers);
        let manager = McpManager::new(Duration::from_secs(60));
        let tools = manager.list_tools(&config).await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
        let result = manager
            .call_tool(&config, "echo", json!({"text": "hello http"}))
            .await
            .unwrap();
        assert_eq!(result["content"][0]["text"], "hello http");
        let probe = manager.probe(&config).await.unwrap();
        assert_eq!(probe.transport, McpTransport::Http);
        assert_eq!(probe.server_name, "test");
        assert_eq!(probe.protocol_version, "2025-06-18");
        assert_eq!(probe.tools, 1);
        assert!(probe.resources_supported);
        assert!(probe.prompts_supported);
        assert_eq!(probe.resources, 0);
        assert_eq!(probe.prompts, 0);
        {
            let requests = state.requests.lock().unwrap();
            assert_eq!(requests[0].method, "initialize");
            assert!(requests[0].session.is_none());
            assert!(
                requests
                    .iter()
                    .all(|request| request.x_test.as_deref() == Some("verlinpark"))
            );
            assert!(
                requests
                    .iter()
                    .skip(1)
                    .all(|request| { request.session.as_deref() == Some("test-session") })
            );
            assert!(
                requests
                    .iter()
                    .any(|request| request.method == "tools/list")
            );
            assert!(
                requests
                    .iter()
                    .any(|request| request.method == "tools/call")
            );
        }
        manager.invalidate(&config.id).await;
        server.abort();
    }

    #[tokio::test]
    async fn streamable_http_reads_sse_responses() {
        let (base, _state, server) = start_test_server(false, true, false).await;
        let config = http_server(&format!("{base}/acp-sse"), BTreeMap::new());
        let manager = McpManager::new(Duration::from_secs(60));
        let tools = manager.list_tools(&config).await.unwrap();
        assert_eq!(tools[0].name, "echo");
        manager.invalidate(&config.id).await;
        server.abort();
    }

    #[tokio::test]
    async fn legacy_sse_lists_and_calls_tools() {
        let (base, state, server) = start_test_server(false, false, true).await;
        let config = legacy_server(&format!("{base}/sse"));
        let manager = McpManager::new(Duration::from_secs(60));
        let tools = manager.list_tools(&config).await.unwrap();
        assert_eq!(tools[0].name, "echo");
        let result = manager
            .call_tool(&config, "echo", json!({"text": "hello sse"}))
            .await
            .unwrap();
        assert_eq!(result["content"][0]["text"], "hello sse");
        {
            let requests = state.requests.lock().unwrap();
            assert!(
                requests
                    .iter()
                    .any(|request| request.method == "initialize")
            );
            assert!(
                requests
                    .iter()
                    .any(|request| request.method == "tools/list")
            );
            assert!(
                requests
                    .iter()
                    .any(|request| request.method == "tools/call")
            );
        }
        manager.invalidate(&config.id).await;
        server.abort();
    }

    #[tokio::test]
    async fn streamable_http_reconnects_after_session_expiry() {
        let (base, state, server) = start_test_server(true, false, false).await;
        state.expire_tools_once.store(true, Ordering::SeqCst);
        let config = http_server(&format!("{base}/acp"), BTreeMap::new());
        let manager = McpManager::new(Duration::from_secs(60));
        let tools = manager.list_tools(&config).await.unwrap();
        assert_eq!(tools[0].name, "echo");
        {
            let requests = state.requests.lock().unwrap();
            assert_eq!(
                requests
                    .iter()
                    .filter(|request| request.method == "initialize")
                    .count(),
                2
            );
            assert_eq!(
                requests
                    .iter()
                    .filter(|request| request.method == "tools/list")
                    .count(),
                2
            );
        }
        manager.invalidate(&config.id).await;
        server.abort();
    }

    #[tokio::test]
    async fn streamable_http_missing_header_env_fails_before_network() {
        let config = http_server(
            "http://127.0.0.1:9/acp",
            BTreeMap::from([(
                "authorization".to_string(),
                "Bearer ${VERLINPARK_TEST_MISSING_MCP_TOKEN_9F3A}".to_string(),
            )]),
        );
        let manager = McpManager::new(Duration::from_secs(60));
        let error = manager.list_tools(&config).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("VERLINPARK_TEST_MISSING_MCP_TOKEN_9F3A")
        );
        assert!(!error.to_string().contains("Bearer"));
    }

    #[tokio::test]
    async fn stdio_rejects_server_initiated_requests_without_hanging() {
        let dir = TempDir::new().unwrap();
        let script = dir.path().join("requesting-server.py");
        std::fs::write(
            &script,
            r#"#!/usr/bin/env python3
import json, sys

def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()

def reply(req, result):
    send({"jsonrpc": "2.0", "id": req.get("id"), "result": result})

while True:
    line = sys.stdin.readline()
    if not line:
        break
    if not line.strip():
        continue
    req = json.loads(line)
    method = req.get("method")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": "roots-1", "method": "roots/list", "params": {}})
        while True:
            response = json.loads(sys.stdin.readline())
            if response.get("id") == "roots-1":
                assert response["error"]["code"] == -32601
                break
        reply(req, {"protocolVersion": "2024-11-05", "capabilities": {}, "serverInfo": {"name": "requesting", "version": "1"}})
    elif method == "tools/list":
        reply(req, {"tools": [{"name": "echo", "description": "Echo", "inputSchema": {}}]})
    elif method == "notifications/initialized":
        pass
"#,
        )
        .unwrap();
        let config = McpServer {
            id: "requesting-server".to_string(),
            name: "requesting".to_string(),
            transport: McpTransport::Stdio,
            command: "python3".to_string(),
            args: vec![script.to_string_lossy().to_string()],
            env: BTreeMap::new(),
            url: None,
            headers: BTreeMap::new(),
            enabled: true,
            created_at: "2026-11-08T00:00:00Z".to_string(),
        };
        let manager = McpManager::new(Duration::from_secs(60));
        let tools = manager.list_tools(&config).await.unwrap();
        assert_eq!(tools[0].name, "echo");
        manager.invalidate(&config.id).await;
    }

    #[test]
    fn normalizes_http_configs_and_rejects_ambiguous_or_secret_fields() {
        let mut config = McpServerConfig {
            name: "remote".into(),
            transport: McpTransport::Http,
            command: String::new(),
            args: Vec::new(),
            env: BTreeMap::new(),
            url: Some("https://mcp.example.test/acp".into()),
            headers: BTreeMap::from([("Authorization".into(), "Bearer ${MCP_TOKEN}".into())]),
            enabled: true,
        };
        let normalized = normalize_config(config.clone()).unwrap();
        assert_eq!(
            normalized.headers.get("authorization").map(String::as_str),
            Some("Bearer ${MCP_TOKEN}")
        );
        config.command = "runner".into();
        assert!(normalize_config(config).is_err());
        let mut invalid = normalized.clone();
        invalid.url = Some("file:///tmp/mcp".into());
        assert!(normalize_config(invalid).is_err());
        let mut credentials = normalized.clone();
        credentials.url = Some("https://user:pass@example.test/acp".into());
        assert!(normalize_config(credentials).is_err());
        let mut static_secret = normalized.clone();
        static_secret
            .headers
            .insert("authorization".into(), "Bearer fixed".into());
        assert!(normalize_config(static_secret).is_err());
        let mut missing_reference = normalized;
        missing_reference.headers.insert(
            "x-api-key".into(),
            "Bearer ${VERLINPARK_TEST_MISSING_TOKEN}".into(),
        );
        assert!(normalize_config(missing_reference).is_ok());
        assert!(
            expand_env_template_with("Bearer ${VERLINPARK_TEST_MISSING_TOKEN}", |_| None).is_err()
        );
        assert_eq!(
            expand_env_template_with("Bearer ${TOKEN}", |name| {
                (name == "TOKEN").then(|| "value".to_string())
            })
            .unwrap(),
            "Bearer value"
        );
    }

    #[test]
    fn acp_conversion_includes_http_and_sse_transports() {
        let mut headers = BTreeMap::new();
        headers.insert("x-test".to_string(), "verlinpark".to_string());
        let http = http_server("https://mcp.example.test/acp", headers);
        let converted = crate::acp::mcp_server(&http).unwrap();
        match converted {
            agent_client_protocol::schema::McpServer::Http(server) => {
                assert_eq!(server.name, "http");
                assert_eq!(server.url, "https://mcp.example.test/acp");
                assert_eq!(server.headers[0].name, "x-test");
                assert_eq!(server.headers[0].value, "verlinpark");
            }
            _ => panic!("expected HTTP MCP server"),
        }
        let legacy = legacy_server("https://mcp.example.test/sse");
        match crate::acp::mcp_server(&legacy).unwrap() {
            agent_client_protocol::schema::McpServer::Sse(server) => {
                assert_eq!(server.url, "https://mcp.example.test/sse");
            }
            _ => panic!("expected SSE MCP server"),
        }
    }

    fn mock_server(dir: &TempDir) -> McpServer {
        let script = dir.path().join("server.py");
        std::fs::write(
            &script,
            r#"
import os, sys, json
log = os.environ.get("MOCK_LOG")
if log:
    with open(log, "a") as handle:
        handle.write("start\n")
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    message = json.loads(line)
    if "id" not in message:
        continue
    method = message.get("method")
    if method == "initialize":
        result = {"protocolVersion": "2024-11-05", "capabilities": {}, "serverInfo": {"name": "mock", "version": "1"}}
    elif method == "tools/list":
        if os.environ.get("MOCK_PAGINATE") == "1":
            if message.get("params", {}).get("cursor") is None:
                result = {"tools": [{"name": "first", "description": "First page", "inputSchema": {"type": "object"}}], "nextCursor": "page2"}
            else:
                result = {"tools": [{"name": "second", "description": "Second page", "inputSchema": {"type": "object"}}]}
        else:
            result = {"tools": [{"name": "echo", "description": "Echo text", "inputSchema": {"type": "object"}}]}
    elif method == "tools/call":
        result = {"content": [{"type": "text", "text": message.get("params", {}).get("arguments", {}).get("text", "")}]}
    elif method == "resources/list":
        result = {"resources": [{"uri": "file:///notes.md", "name": "notes", "description": "Project notes", "mimeType": "text/markdown"}]}
    elif method == "resources/templates/list":
        result = {"resourceTemplates": [{"uriTemplate": "file:///{path}", "name": "file", "description": "Read a file", "mimeType": "text/plain"}]}
    elif method == "resources/read":
        result = {"contents": [{"uri": message.get("params", {}).get("uri"), "mimeType": "text/markdown", "text": "hello resource"}]}
    elif method == "prompts/list":
        result = {"prompts": [{"name": "review", "description": "Review code", "arguments": [{"name": "target", "description": "Path", "required": True}]}]}
    elif method == "prompts/get":
        target = message.get("params", {}).get("arguments", {}).get("target", "")
        result = {"description": "Review template", "messages": [{"role": "user", "content": {"type": "text", "text": "Review " + target}}]}
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": message.get("id"), "result": result}), flush=True)
"#,
        )
        .unwrap();
        McpServer {
            id: "server-1".to_string(),
            name: "mock".to_string(),
            transport: McpTransport::Stdio,
            command: "python3".to_string(),
            args: vec![script.to_string_lossy().to_string()],
            env: BTreeMap::new(),
            url: None,
            headers: BTreeMap::new(),
            enabled: true,
            created_at: "2026-10-31T00:00:00Z".to_string(),
        }
    }

    fn startup_count(path: &std::path::Path) -> usize {
        std::fs::read_to_string(path)
            .map(|text| text.lines().filter(|line| *line == "start").count())
            .unwrap_or(0)
    }

    #[tokio::test]
    async fn reuses_one_process_across_list_and_call() {
        let dir = TempDir::new().unwrap();
        let log = dir.path().join("starts.log");
        let mut server = mock_server(&dir);
        server
            .env
            .insert("MOCK_LOG".to_string(), log.to_string_lossy().to_string());
        let manager = McpManager::new(Duration::from_secs(60));
        let tools = manager.list_tools(&server).await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
        assert_eq!(tools[0].description, "Echo text");
        let result = manager
            .call_tool(&server, "echo", json!({"text": "hello mcp"}))
            .await
            .unwrap();
        assert_eq!(result["content"][0]["text"], "hello mcp");
        assert_eq!(manager.connection_count().await, 1);
        assert_eq!(startup_count(&log), 1);
    }

    #[tokio::test]
    async fn reads_paginated_tool_lists() {
        let dir = TempDir::new().unwrap();
        let mut server = mock_server(&dir);
        server
            .env
            .insert("MOCK_PAGINATE".to_string(), "1".to_string());
        let manager = McpManager::new(Duration::from_secs(60));
        let tools = manager.list_tools(&server).await.unwrap();
        assert_eq!(
            tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
    }

    #[tokio::test]
    async fn sweeps_idle_connections() {
        let dir = TempDir::new().unwrap();
        let log = dir.path().join("starts.log");
        let mut server = mock_server(&dir);
        server
            .env
            .insert("MOCK_LOG".to_string(), log.to_string_lossy().to_string());
        let manager = McpManager::new(Duration::from_millis(50));
        manager.list_tools(&server).await.unwrap();
        assert_eq!(manager.sweep_idle().await, 0);
        tokio::time::sleep(Duration::from_millis(120)).await;
        assert_eq!(manager.sweep_idle().await, 1);
        assert_eq!(manager.connection_count().await, 0);
        manager.list_tools(&server).await.unwrap();
        assert_eq!(startup_count(&log), 2);
    }

    #[tokio::test]
    async fn lists_and_reads_resources_and_prompts() {
        let dir = TempDir::new().unwrap();
        let server = mock_server(&dir);
        let manager = McpManager::new(Duration::from_secs(60));
        let resources = manager.list_resources(&server).await.unwrap();
        assert_eq!(resources.resources.len(), 1);
        assert_eq!(resources.resources[0].uri, "file:///notes.md");
        assert_eq!(
            resources.resources[0].mime_type.as_deref(),
            Some("text/markdown")
        );
        assert_eq!(resources.templates.len(), 1);
        assert_eq!(resources.templates[0].uri_template, "file:///{path}");

        let content = manager
            .read_resource(&server, "file:///notes.md")
            .await
            .unwrap();
        assert_eq!(content.contents.len(), 1);
        assert_eq!(content.contents[0].text.as_deref(), Some("hello resource"));

        let prompts = manager.list_prompts(&server).await.unwrap();
        assert_eq!(prompts.prompts.len(), 1);
        assert_eq!(prompts.prompts[0].name, "review");
        assert!(prompts.prompts[0].arguments[0].required);

        let mut arguments = std::collections::BTreeMap::new();
        arguments.insert("target".to_string(), "src/main.rs".to_string());
        let prompt = manager
            .get_prompt(&server, "review", arguments)
            .await
            .unwrap();
        assert_eq!(prompt.description, "Review template");
        assert_eq!(prompt.messages.len(), 1);
        assert_eq!(prompt.messages[0].content["text"], "Review src/main.rs");
    }

    #[tokio::test]
    async fn invalidating_a_server_forces_a_reconnect() {
        let dir = TempDir::new().unwrap();
        let log = dir.path().join("starts.log");
        let mut server = mock_server(&dir);
        server
            .env
            .insert("MOCK_LOG".to_string(), log.to_string_lossy().to_string());
        let manager = McpManager::new(Duration::from_secs(60));
        manager.list_tools(&server).await.unwrap();
        manager.invalidate(&server.id).await;
        assert_eq!(manager.connection_count().await, 0);
        manager.list_tools(&server).await.unwrap();
        assert_eq!(startup_count(&log), 2);
    }

    #[tokio::test]
    async fn refuses_a_disabled_mcp_server() {
        let dir = TempDir::new().unwrap();
        let mut server = mock_server(&dir);
        server.enabled = false;
        let manager = McpManager::new(Duration::from_secs(60));
        assert!(manager.list_tools(&server).await.is_err());
        assert!(manager.call_tool(&server, "echo", json!({})).await.is_err());
    }
}

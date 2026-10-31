use std::collections::{HashMap, HashSet};
use std::fmt;
use std::process::Stdio;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use vp_protocol::types::{
    McpPrompt, McpPromptListResult, McpPromptMessage, McpReadResourceResult, McpResource,
    McpResourceContent, McpResourceListResult, McpResourceTemplate, McpServer, McpTool,
};

const MCP_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_PAGES: usize = 50;
const MAX_ITEMS: usize = 2_000;

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

struct McpSession {
    name: String,
    child: Child,
    pid: Option<u32>,
    stdin: ChildStdin,
    lines: tokio::io::Lines<BufReader<ChildStdout>>,
    next_id: u64,
}

impl McpSession {
    async fn connect(server: &McpServer) -> Result<Self> {
        let mut command = Command::new(&server.command);
        command
            .args(&server.args)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        for key in [
            "PATH", "HOME", "USER", "LOGNAME", "LANG", "LC_ALL", "TMPDIR", "TZ",
        ] {
            if let Ok(value) = std::env::var(key) {
                command.env(key, value);
            }
        }
        for (key, value) in &server.env {
            command.env(key, value);
        }
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command
            .spawn()
            .with_context(|| format!("starting MCP server {}", server.name))?;
        let pid = child.id();
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("MCP server {} has no stdin", server.name))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("MCP server {} has no stdout", server.name))?;
        let mut session = Self {
            name: server.name.clone(),
            child,
            pid,
            stdin,
            lines: BufReader::new(stdout).lines(),
            next_id: 1,
        };
        timeout(
            "initialize",
            session.request(
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {"name": "VerlinPark", "version": env!("CARGO_PKG_VERSION")}
                }),
            ),
        )
        .await?;
        session
            .notify("notifications/initialized", json!({}))
            .await?;
        Ok(session)
    }

    fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let message = json!({"jsonrpc": "2.0", "method": method, "params": params});
        let mut line = serde_json::to_vec(&message)?;
        line.push(b'\n');
        self.stdin
            .write_all(&line)
            .await
            .map_err(|error| transport(format!("MCP {method} write failed: {error}")))?;
        self.stdin
            .flush()
            .await
            .map_err(|error| transport(format!("MCP {method} flush failed: {error}")))?;
        Ok(())
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let mut line = serde_json::to_vec(&message)?;
        line.push(b'\n');
        self.stdin
            .write_all(&line)
            .await
            .map_err(|error| transport(format!("MCP {method} write failed: {error}")))?;
        self.stdin
            .flush()
            .await
            .map_err(|error| transport(format!("MCP {method} flush failed: {error}")))?;
        loop {
            let line = timeout("response", async {
                self.lines
                    .next_line()
                    .await
                    .map_err(|error| transport(format!("MCP {method} read failed: {error}")))
            })
            .await?
            .ok_or_else(|| transport(format!("MCP server {} closed stdout", self.name)))?;
            if line.len() > 2 * 1024 * 1024 {
                bail!("MCP response line exceeds 2 MiB");
            }
            if line.trim().is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(&line)
                .with_context(|| format!("parsing MCP {method} response"))?;
            if value.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = value.get("error") {
                bail!(
                    "MCP {method} failed: {}",
                    error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error")
                );
            }
            return value
                .get("result")
                .cloned()
                .ok_or_else(|| anyhow!("MCP {method} response has no result"));
        }
    }
}

impl Drop for McpSession {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.pid.and_then(|pid| i32::try_from(pid).ok()) {
            // SAFETY: negative pid targets the process group created above.
            // ESRCH is the expected no-op when the group already exited.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
        let _ = self.child.start_kill();
    }
}

struct McpConnection {
    server: McpServer,
    session: Mutex<McpSession>,
    last_used: StdMutex<Instant>,
}

impl McpConnection {
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

/// Keeps one stdio process per configured server. Calls to the same server are
/// serialized because the MCP stdio transport carries a single JSON-RPC stream.
/// Connections are torn down after an idle period or when the config changes.
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

    pub async fn invalidate(&self, server_id: &str) {
        self.connections.lock().await.remove(server_id);
    }

    pub async fn invalidate_all(&self) {
        self.connections.lock().await.clear();
    }

    pub async fn sweep_idle(&self) -> usize {
        let mut connections = self.connections.lock().await;
        let before = connections.len();
        let idle = self.idle;
        connections.retain(|_, connection| connection.idle_for() < idle);
        before - connections.len()
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
            let mut connections = self.connections.lock().await;
            if connections
                .get(&server.id)
                .is_some_and(|current| Arc::ptr_eq(current, &connection))
            {
                connections.remove(&server.id);
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
            connection.touch();
            return Ok(connection.clone());
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
        let mut connections = self.connections.lock().await;
        if connections
            .get(server_id)
            .is_some_and(|current| Arc::ptr_eq(current, connection))
        {
            connections.remove(server_id);
        }
    }
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
    match tokio::time::timeout(MCP_TIMEOUT, future).await {
        Ok(result) => result,
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
    use tempfile::TempDir;

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
            command: "python3".to_string(),
            args: vec![script.to_string_lossy().to_string()],
            env: BTreeMap::new(),
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

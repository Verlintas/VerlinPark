//! MCP stdio transport.

use std::process::Stdio;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use vp_protocol::types::McpServer;

use super::{McpSessionInfo, timeout, transport};

/// One long-lived MCP server process speaking newline-delimited JSON-RPC.
pub(super) struct StdioSession {
    name: String,
    child: Child,
    pid: Option<u32>,
    stdin: ChildStdin,
    lines: tokio::io::Lines<BufReader<ChildStdout>>,
    next_id: u64,
    info: McpSessionInfo,
}

impl StdioSession {
    pub(super) async fn connect(server: &McpServer) -> Result<Self> {
        if server.command.trim().is_empty() {
            bail!("MCP stdio server {} has no command", server.name);
        }
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
            info: McpSessionInfo::default(),
        };
        let initialized = timeout(
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
        session.info = McpSessionInfo::from_initialize(&initialized);
        session
            .notify("notifications/initialized", json!({}))
            .await?;
        Ok(session)
    }

    pub(super) fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub(super) fn info(&self) -> McpSessionInfo {
        self.info.clone()
    }

    pub(super) async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
        .await
        .map_err(|error| transport(format!("MCP {method} write failed: {error}")))
    }

    pub(super) async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.request_preemptible(method, params, None).await
    }

    pub(super) async fn request_preemptible(
        &mut self,
        method: &str,
        params: Value,
        token: Option<&vp_providers::PreemptToken>,
    ) -> Result<Value> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .await
        .map_err(|error| transport(format!("MCP {method} write failed: {error}")))?;
        loop {
            if let Some(token) = token
                && token.is_preempted()
            {
                self.cancel_request(id, method).await;
                bail!("MCP {method} was preempted");
            }
            let read = timeout("response", async {
                self.lines
                    .next_line()
                    .await
                    .map_err(|error| transport(format!("MCP {method} read failed: {error}")))
            });
            let line = match token {
                Some(token) => {
                    tokio::select! {
                        _ = token.cancelled() => {
                            self.cancel_request(id, method).await;
                            bail!("MCP {method} was preempted");
                        }
                        line = read => line?,
                    }
                }
                None => read.await?,
            }
            .ok_or_else(|| transport(format!("MCP server {} closed stdout", self.name)))?;
            if line.len() > 2 * 1024 * 1024 {
                bail!("MCP response line exceeds 2 MiB");
            }
            if line.trim().is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(&line)
                .with_context(|| format!("parsing MCP {method} response"))?;
            if value.get("method").is_some() {
                if let Some(request_id) = value.get("id").cloned() {
                    self.write_message(&json!({
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "error": {
                            "code": -32601,
                            "message": "VerlinPark does not implement server-initiated MCP methods"
                        }
                    }))
                    .await
                    .map_err(|error| {
                        transport(format!("MCP {method} rejection failed: {error}"))
                    })?;
                }
                continue;
            }
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

    /// Best-effort protocol cancellation. The request is never replayed and a
    /// server that does not implement cancellation simply ignores the
    /// notification.
    async fn cancel_request(&mut self, id: u64, method: &str) {
        let _ = self
            .write_message(&json!({
                "jsonrpc": "2.0",
                "method": "notifications/cancelled",
                "params": {
                    "requestId": id,
                    "reason": format!("preempted while running {method}"),
                }
            }))
            .await;
    }

    async fn write_message(&mut self, message: &Value) -> Result<()> {
        let mut line = serde_json::to_vec(message)?;
        line.push(b'\n');
        self.stdin
            .write_all(&line)
            .await
            .map_err(|error| anyhow!("write failed: {error}"))?;
        self.stdin
            .flush()
            .await
            .map_err(|error| anyhow!("flush failed: {error}"))?;
        Ok(())
    }

    pub(super) async fn close(&mut self) {
        let _ = self.child.start_kill();
    }
}

impl Drop for StdioSession {
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

use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use vp_protocol::types::{McpServer, McpTool};

const MCP_TIMEOUT: Duration = Duration::from_secs(60);

struct McpSession {
    child: Child,
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
        let mut child = command
            .spawn()
            .with_context(|| format!("starting MCP server {}", server.name))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("MCP server {} has no stdin", server.name))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("MCP server {} has no stdout", server.name))?;
        let mut session = Self {
            child,
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

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let message = json!({"jsonrpc": "2.0", "method": method, "params": params});
        let mut line = serde_json::to_vec(&message)?;
        line.push(b'\n');
        self.stdin.write_all(&line).await?;
        self.stdin.flush().await?;
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
        self.stdin.write_all(&line).await?;
        self.stdin.flush().await?;
        loop {
            let line = timeout("response", async {
                self.lines.next_line().await.map_err(anyhow::Error::from)
            })
            .await?
            .ok_or_else(|| anyhow!("MCP server closed stdout"))?;
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
        let _ = self.child.start_kill();
    }
}

async fn timeout<T>(
    label: &str,
    future: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    tokio::time::timeout(MCP_TIMEOUT, future)
        .await
        .map_err(|_| anyhow!("MCP {label} timed out after {}s", MCP_TIMEOUT.as_secs()))?
}

pub async fn list_tools(server: &McpServer) -> Result<Vec<McpTool>> {
    if !server.enabled {
        bail!("MCP server {} is disabled", server.name);
    }
    let mut session = McpSession::connect(server).await?;
    let result = timeout("tools/list", session.request("tools/list", json!({}))).await?;
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

pub async fn call_tool(server: &McpServer, tool: &str, arguments: Value) -> Result<Value> {
    if !server.enabled {
        bail!("MCP server {} is disabled", server.name);
    }
    if tool.trim().is_empty() {
        bail!("MCP tool name is required");
    }
    let mut session = McpSession::connect(server).await?;
    timeout(
        "tools/call",
        session.request("tools/call", json!({"name": tool, "arguments": arguments})),
    )
    .await
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
import sys, json
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
        result = {"tools": [{"name": "echo", "description": "Echo text", "inputSchema": {"type": "object"}}]}
    elif method == "tools/call":
        result = {"content": [{"type": "text", "text": message.get("params", {}).get("arguments", {}).get("text", "")}]}
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

    #[tokio::test]
    async fn lists_and_calls_mcp_stdio_tools() {
        let dir = TempDir::new().unwrap();
        let server = mock_server(&dir);
        let tools = list_tools(&server).await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
        assert_eq!(tools[0].description, "Echo text");
        let result = call_tool(&server, "echo", json!({"text": "hello mcp"}))
            .await
            .unwrap();
        assert_eq!(result["content"][0]["text"], "hello mcp");
    }

    #[tokio::test]
    async fn refuses_a_disabled_mcp_server() {
        let dir = TempDir::new().unwrap();
        let mut server = mock_server(&dir);
        server.enabled = false;
        assert!(list_tools(&server).await.is_err());
        assert!(call_tool(&server, "echo", json!({})).await.is_err());
    }
}

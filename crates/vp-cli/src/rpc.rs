//! Daemon discovery and JSON-RPC transport helpers for the CLI.

use std::fmt;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use vp_protocol::discovery::Discovery;
use vp_protocol::rpc::RpcResponse;
use vp_store::paths;

const DEFAULT_RPC_TIMEOUT_SECS: u64 = 300;
const DEFAULT_RPC_CONNECT_TIMEOUT_SECS: u64 = 10;

static RPC_TIMEOUT_SECS: AtomicU64 = AtomicU64::new(DEFAULT_RPC_TIMEOUT_SECS);
static RPC_CONNECT_TIMEOUT_SECS: AtomicU64 = AtomicU64::new(DEFAULT_RPC_CONNECT_TIMEOUT_SECS);

/// A typed transport/RPC failure. The CLI preserves the JSON-RPC code, the
/// daemon's typed `data.code`, and the hint instead of flattening them into an
/// English-only error.
#[derive(Debug)]
pub(crate) struct CliRpcError {
    pub(crate) kind: &'static str,
    pub(crate) code: Option<i32>,
    pub(crate) typed_code: Option<String>,
    pub(crate) message: String,
    pub(crate) hint: Option<String>,
    pub(crate) data: Option<Value>,
}

impl fmt::Display for CliRpcError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.message)?;
        if let Some(code) = self.code {
            write!(formatter, " (code {code})")?;
        }
        if let Some(code) = &self.typed_code {
            write!(formatter, " [{code}]")?;
        }
        if let Some(hint) = &self.hint {
            write!(formatter, "\nhint: {hint}")?;
        }
        Ok(())
    }
}

impl std::error::Error for CliRpcError {}

impl CliRpcError {
    pub(crate) fn from_rpc(error: vp_protocol::rpc::RpcError) -> Self {
        let typed_code = error
            .data
            .as_ref()
            .and_then(|data| data.get("code"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let hint = error
            .data
            .as_ref()
            .and_then(|data| data.get("hint"))
            .and_then(Value::as_str)
            .filter(|hint| !hint.trim().is_empty())
            .map(str::to_string);
        Self {
            kind: "rpc",
            code: Some(error.code),
            typed_code,
            message: error.message,
            hint,
            data: error.data,
        }
    }

    fn transport(error: &reqwest::Error, timeout_secs: u64) -> Self {
        let kind = if error.is_timeout() {
            "timeout"
        } else if error.is_connect() {
            "connect"
        } else {
            "transport"
        };
        let message = if error.is_timeout() {
            format!("daemon RPC timed out after {timeout_secs}s")
        } else if error.is_connect() {
            format!("could not connect to the daemon: {error}")
        } else {
            format!("daemon RPC transport failed: {error}")
        };
        Self {
            kind,
            code: None,
            typed_code: None,
            message,
            hint: error.is_timeout().then(|| {
                "The daemon may be wedged or restarting; pass --rpc-timeout-secs to raise the bound."
                    .to_string()
            }),
            data: None,
        }
    }

    fn protocol(message: impl Into<String>) -> Self {
        Self {
            kind: "protocol",
            code: None,
            typed_code: None,
            message: message.into(),
            hint: None,
            data: None,
        }
    }

    fn http(status: reqwest::StatusCode, body: String) -> Self {
        let body = truncate_body(body);
        Self {
            kind: "http",
            code: Some(status.as_u16() as i32),
            typed_code: None,
            message: format!("daemon returned HTTP {status}: {body}"),
            hint: None,
            data: None,
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        json!({
            "error": {
                "kind": self.kind,
                "code": self.code,
                "typed_code": self.typed_code,
                "message": self.message,
                "hint": self.hint,
                "data": self.data,
            }
        })
    }
}

fn truncate_body(mut body: String) -> String {
    const MAX: usize = 4096;
    if body.len() > MAX {
        let mut end = MAX;
        while end > 0 && !body.is_char_boundary(end) {
            end -= 1;
        }
        body.truncate(end);
        body.push('…');
    }
    body
}

/// Set the bounded transport timeouts once per invocation. Values are
/// validated locally so a typo cannot silently disable the timeout.
pub(crate) fn configure_timeouts(request_secs: u64, connect_secs: u64) -> Result<()> {
    if !(1..=3600).contains(&request_secs) {
        bail!("--rpc-timeout-secs must be between 1 and 3600");
    }
    if !(1..=3600).contains(&connect_secs) {
        bail!("--rpc-connect-timeout-secs must be between 1 and 3600");
    }
    RPC_TIMEOUT_SECS.store(request_secs, Ordering::Relaxed);
    RPC_CONNECT_TIMEOUT_SECS.store(connect_secs, Ordering::Relaxed);
    Ok(())
}

fn request_timeout() -> Duration {
    Duration::from_secs(RPC_TIMEOUT_SECS.load(Ordering::Relaxed))
}

fn connect_timeout() -> Duration {
    Duration::from_secs(RPC_CONNECT_TIMEOUT_SECS.load(Ordering::Relaxed))
}

pub(crate) fn read_discovery(data_dir: Option<&Path>) -> Result<Discovery> {
    let path = match data_dir {
        Some(dir) => dir.join("run/daemon.json"),
        None => paths::discovery_path(),
    };
    let data = std::fs::read(&path).with_context(|| {
        format!(
            "daemon discovery file not found at {}. Start verlinpadd first (just daemon).",
            path.display()
        )
    })?;
    let discovery: Discovery =
        serde_json::from_slice(&data).context("parsing daemon discovery file")?;
    Ok(discovery)
}

pub(crate) async fn resolve_project(
    discovery: &Discovery,
    requested: Option<&str>,
) -> Result<String> {
    let projects = rpc(discovery, "project.list", json!({})).await?;
    let projects = projects
        .get("projects")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(requested) = requested {
        if let Some(project) = projects.iter().find(|project| {
            project.get("id").and_then(Value::as_str) == Some(requested)
                || project
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| name.eq_ignore_ascii_case(requested))
        }) && let Some(id) = project.get("id").and_then(Value::as_str)
        {
            return Ok(id.to_string());
        }
        // Allow an explicit id that the daemon can resolve later.
        return Ok(requested.to_string());
    }
    match projects.as_slice() {
        [project] => project
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("project list returned a malformed project")),
        [] => bail!("no projects configured; pass --project"),
        _ => bail!("multiple projects configured; pass --project"),
    }
}

pub(crate) fn read_gate_token(
    token: Option<String>,
    token_stdin: bool,
    token_file: Option<&Path>,
) -> Result<String> {
    const MAX_TOKEN_BYTES: u64 = 4 * 1024;
    let raw = if let Some(token) = token {
        eprintln!(
            "warning: --token is visible to local process listings; prefer --token-stdin or --token-file"
        );
        token
    } else if token_stdin {
        let mut value = String::new();
        std::io::stdin()
            .take(MAX_TOKEN_BYTES + 1)
            .read_to_string(&mut value)
            .context("reading the gate token from stdin")?;
        if value.len() as u64 > MAX_TOKEN_BYTES {
            bail!("gate token input exceeds {MAX_TOKEN_BYTES} bytes");
        }
        value
    } else if let Some(path) = token_file {
        let metadata = std::fs::metadata(path)
            .with_context(|| format!("reading gate token file {}", path.display()))?;
        if metadata.len() > MAX_TOKEN_BYTES {
            bail!("gate token file exceeds {MAX_TOKEN_BYTES} bytes");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                bail!(
                    "gate token file {} is group/world accessible; chmod 600 it first",
                    path.display()
                );
            }
        }
        std::fs::read_to_string(path)
            .with_context(|| format!("reading gate token file {}", path.display()))?
    } else {
        bail!("provide --token-stdin, --token-file, or --token");
    };
    let token = raw.trim().to_string();
    if token.is_empty() {
        bail!("gate token is empty");
    }
    Ok(token)
}

pub(crate) async fn rpc(discovery: &Discovery, method: &str, params: Value) -> Result<Value> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": "cli",
        "method": method,
        "params": params,
    });
    let timeout_secs = RPC_TIMEOUT_SECS.load(Ordering::Relaxed);
    let client = reqwest::Client::builder()
        .connect_timeout(connect_timeout())
        .timeout(request_timeout())
        .build()
        .map_err(|error| {
            CliRpcError::protocol(format!("building the daemon HTTP client: {error}"))
        })?;
    let response = client
        .post(discovery.rpc_url())
        .bearer_auth(&discovery.token)
        .json(&body)
        .send()
        .await
        .map_err(|error| CliRpcError::transport(&error, timeout_secs))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|error| CliRpcError::transport(&error, timeout_secs))?;
    if !status.is_success() {
        return Err(CliRpcError::http(status, text).into());
    }
    let response: RpcResponse = serde_json::from_str(&text).map_err(|error| {
        CliRpcError::protocol(format!("parsing the daemon JSON-RPC response: {error}"))
    })?;
    if let Some(error) = response.error {
        return Err(CliRpcError::from_rpc(error).into());
    }
    Ok(response.result.unwrap_or(Value::Null))
}

#[cfg(test)]
pub(crate) fn format_rpc_error(error: &vp_protocol::rpc::RpcError) -> String {
    let mut message = format!("{} (code {})", error.message, error.code);
    if let Some(data) = &error.data {
        if let Some(code) = data.get("code").and_then(Value::as_str) {
            message.push_str(&format!(" [{code}]"));
        }
        if let Some(hint) = data.get("hint").and_then(Value::as_str)
            && !hint.trim().is_empty()
        {
            message.push_str(&format!("\nhint: {hint}"));
        }
    }
    message
}

/// Line-oriented confirmation for destructive/privileged local operations.
/// EOF, a blank line, or anything other than `y`/`yes` is a refusal. The CLI
/// never reads from a keyboard for a background command.
pub(crate) fn confirm_cli_action(prompt: &str) -> Result<bool> {
    let stdin = std::io::stdin();
    let mut stdin = stdin.lock();
    confirm_action_with(prompt, &mut stdin)
}

pub(crate) fn confirm_action_with<R: std::io::BufRead>(
    prompt: &str,
    reader: &mut R,
) -> Result<bool> {
    eprint!("{prompt} [y/N] ");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    let read = reader.read_line(&mut line)?;
    if read == 0 {
        return Ok(false);
    }
    let answer = line.trim().to_ascii_lowercase();
    Ok(answer == "y" || answer == "yes")
}

pub(crate) async fn mcp_server_enabled(discovery: &Discovery, server_id: &str) -> Result<bool> {
    let value = rpc(discovery, "mcp.server.list", json!({})).await?;
    value
        .get("servers")
        .and_then(Value::as_array)
        .and_then(|servers| {
            servers
                .iter()
                .find(|server| server.get("id").and_then(Value::as_str) == Some(server_id))
        })
        .and_then(|server| server.get("enabled").and_then(Value::as_bool))
        .ok_or_else(|| anyhow::anyhow!("MCP server not found: {server_id}"))
}

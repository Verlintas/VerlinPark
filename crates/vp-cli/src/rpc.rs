//! Daemon discovery and JSON-RPC transport helpers for the CLI.

use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use vp_protocol::discovery::Discovery;
use vp_protocol::rpc::RpcResponse;
use vp_store::paths;

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
    let response = reqwest::Client::new()
        .post(discovery.rpc_url())
        .bearer_auth(&discovery.token)
        .json(&body)
        .send()
        .await
        .with_context(|| format!("connecting to {}", discovery.rpc_url()))?;
    let status = response.status();
    let text = response.text().await?;
    if !status.is_success() {
        bail!("daemon returned HTTP {status}: {text}");
    }
    let response: RpcResponse = serde_json::from_str(&text)?;
    if let Some(error) = response.error {
        bail!("{}", format_rpc_error(&error));
    }
    Ok(response.result.unwrap_or(Value::Null))
}

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
    eprint!("{prompt} [y/N] ");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    let read = std::io::stdin().read_line(&mut line)?;
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

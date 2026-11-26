//! Read-only Custos client used by the `custos` CLI and tests.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::discovery::{self, CustosDiscovery};
use crate::protocol::{RpcRequest, RpcResponse};

pub fn default_data_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("VERLINPARK_DATA_DIR") {
        return PathBuf::from(path);
    }
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("VerlinPark")
}

pub fn resolve_socket(data_dir: &Path, explicit: Option<&Path>) -> PathBuf {
    if let Some(path) = explicit {
        return path.to_path_buf();
    }
    if let Ok(Some(discovery)) = discovery::read_discovery(data_dir) {
        return PathBuf::from(discovery.socket);
    }
    discovery::default_socket_path(data_dir)
}

pub async fn call(socket: &Path, method: &str, params: Value) -> Result<Value> {
    let mut stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("connecting to {}", socket.display()))?;
    let request = RpcRequest {
        id: Some(1),
        method: method.to_string(),
        params,
    };
    let mut bytes = serde_json::to_vec(&request)?;
    bytes.push(b'\n');
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let read = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        reader.read_line(&mut line),
    )
    .await
    .context("custos response timeout")??;
    if read == 0 {
        bail!("custos closed the connection without a response");
    }
    let response: RpcResponse = serde_json::from_str(&line).context("parsing custos response")?;
    if !response.ok {
        let error = response
            .error
            .map(|error| format!("{}: {}", error.code, error.message))
            .unwrap_or_else(|| "unknown custos error".to_string());
        bail!("{error}");
    }
    response
        .result
        .ok_or_else(|| anyhow::anyhow!("custos response has no result"))
}

pub async fn ping(socket: &Path) -> Result<Value> {
    call(socket, "ping", json!({})).await
}

pub async fn status(socket: &Path) -> Result<Value> {
    call(socket, "status", json!({})).await
}

pub async fn health(socket: &Path) -> Result<Value> {
    call(socket, "health", json!({})).await
}

pub async fn charter(socket: &Path) -> Result<Value> {
    call(socket, "charter", json!({})).await
}

/// Explicit user-consented local diagnostic export. No caller-supplied path.
pub async fn diagnostics(socket: &Path) -> Result<Value> {
    call(socket, "diagnostics.generate", json!({})).await
}

pub async fn discovery(data_dir: &Path) -> Result<Option<CustosDiscovery>> {
    discovery::read_discovery(data_dir)
}

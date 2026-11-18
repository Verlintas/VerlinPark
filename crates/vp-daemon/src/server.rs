use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use axum::http::{HeaderMap, header};
use base64::Engine as Base64Engine;
use fs2::FileExt;
use rand::RngCore;
use vp_protocol::discovery::Discovery;
use vp_protocol::types::ServerInfo;
use vp_protocol::{APP_VERSION, PROTOCOL_VERSION};
use vp_store::paths;

use crate::AppState;

pub(crate) async fn server_info(state: &AppState) -> Result<ServerInfo> {
    let projects = state.engine.list_projects().await?;
    let sessions = state.engine.list_sessions(None).await?;
    Ok(ServerInfo {
        pid: state.pid,
        version: APP_VERSION.to_string(),
        protocol: PROTOCOL_VERSION,
        data_dir: state.data_dir.to_string_lossy().to_string(),
        started_at: state.started_at.clone(),
        sessions: sessions.len(),
        projects: projects.len(),
    })
}

pub(crate) fn authorized(headers: &HeaderMap, token: &str) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|candidate| constant_time_eq(candidate, token))
}

pub(crate) fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (left, right)| acc | (left ^ right))
        == 0
}

pub(crate) fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn discovery_path(data_dir: &Path) -> PathBuf {
    data_dir.join("run").join("daemon.json")
}

pub(crate) fn acquire_instance_lock(data_dir: &Path) -> Result<std::fs::File> {
    let run_dir = data_dir.join("run");
    paths::ensure_private_dir(&run_dir)?;
    let path = run_dir.join("daemon.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    paths::ensure_private_file(&path)?;
    file.try_lock_exclusive().map_err(|_| {
        anyhow::anyhow!(
            "another verlinpadd is already running for {}",
            data_dir.display()
        )
    })?;
    Ok(file)
}

pub(crate) fn write_discovery(data_dir: &Path, discovery: &Discovery) -> Result<()> {
    let path = discovery_path(data_dir);
    let parent = path.parent().context("discovery path has no parent")?;
    paths::ensure_private_dir(parent)?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, serde_json::to_vec_pretty(discovery)?)?;
    paths::ensure_private_file(&temp)?;
    std::fs::rename(&temp, &path)?;
    Ok(())
}

pub(crate) fn remove_discovery(data_dir: &Path) {
    let _ = std::fs::remove_file(discovery_path(data_dir));
}

pub(crate) async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut signal) = signal(SignalKind::terminate()) {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

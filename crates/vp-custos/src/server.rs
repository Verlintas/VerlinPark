//! Read-only Custos admin server.
//!
//! This first slice exposes only `status`, `health`, `charter`, and `ping` on a
//! 0600 Unix-domain socket. There is no mutation method, token, TCP listener,
//! or agent-facing surface.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::capacity::MachineCapacity;
use crate::charter;
use crate::diagnostics::{self, BundleMode};
use crate::discovery;
use crate::probe::{self, ProbeReport};
use crate::process_probe::{ProcessCache, ProcessReport};
use crate::protocol::{
    CUSTOS_PROTOCOL_VERSION, CUSTOS_VERSION, CustosRole, CustosState, Dimension, DimensionState,
    Health, PingResult, RpcRequest, RpcResponse, Status, VaultView,
};
use crate::ring::{self, Heartbeat};
use crate::vault::{self, VaultState};

const PROBE_CACHE_TTL: Duration = Duration::from_secs(2);
const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_REQUESTS_PER_CONNECTION: usize = 32;

type VaultCache = Arc<Mutex<Option<(Instant, Option<VaultView>)>>>;

#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub data_dir: PathBuf,
    pub socket_path: PathBuf,
    pub role: CustosRole,
    pub observe_only: bool,
}

impl ServerConfig {
    pub fn new(data_dir: PathBuf, role: CustosRole, observe_only: bool) -> Self {
        let socket_path = discovery::default_socket_path(&data_dir);
        Self {
            data_dir,
            socket_path,
            role,
            observe_only,
        }
    }

    pub fn with_socket(mut self, socket_path: PathBuf) -> Self {
        self.socket_path = socket_path;
        self
    }
}

#[derive(Clone)]
struct AppState {
    data_dir: PathBuf,
    role: CustosRole,
    observe_only: bool,
    started_at: Instant,
    started_at_rfc: String,
    pid: u32,
    seq: Arc<AtomicU64>,
    cache: Arc<Mutex<Option<(Instant, ProbeReport)>>>,
    process_cache: ProcessCache,
    vault_cache: VaultCache,
}

pub async fn run(config: ServerConfig) -> Result<()> {
    if config.role != CustosRole::Core {
        bail!("custosd observe-only slice only runs the core role");
    }
    if !config.observe_only {
        bail!("custosd observe-only slice refuses non-observe mode");
    }
    fs::create_dir_all(&config.data_dir)
        .with_context(|| format!("creating {}", config.data_dir.display()))?;
    let custos_dir = discovery::custos_dir(&config.data_dir);
    fs::create_dir_all(&custos_dir)
        .with_context(|| format!("creating {}", custos_dir.display()))?;
    set_dir_mode(&custos_dir)?;
    if let Some(parent) = config.socket_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        set_dir_mode(parent)?;
    }
    remove_stale_socket(&config.socket_path)?;
    let listener = UnixListener::bind(&config.socket_path)
        .with_context(|| format!("binding {}", config.socket_path.display()))?;
    set_socket_mode(&config.socket_path)?;

    let started_at_rfc = vp_protocol::now_rfc3339();
    let state = AppState {
        data_dir: config.data_dir.clone(),
        role: config.role,
        observe_only: config.observe_only,
        started_at: Instant::now(),
        started_at_rfc: started_at_rfc.clone(),
        pid: std::process::id(),
        seq: Arc::new(AtomicU64::new(1)),
        cache: Arc::new(Mutex::new(None)),
        process_cache: ProcessCache::new(),
        vault_cache: Arc::new(Mutex::new(None)),
    };
    write_core_heartbeat(&state)
        .await
        .context("writing initial custos heartbeat")?;
    let discovery = discovery::write_discovery(
        &config.data_dir,
        &config.socket_path,
        config.role,
        &started_at_rfc,
    )?;
    info!(
        socket = %config.socket_path.display(),
        pid = discovery.pid,
        role = discovery.role,
        "custosd observe-only ready"
    );

    let heartbeat_state = state.clone();
    let heartbeat_task = tokio::spawn(async move {
        let mut ticks = 0u64;
        let mut last_attempt = std::collections::HashMap::new();
        let paths = ring::launchd_paths(&heartbeat_state.data_dir);
        let runner = crate::launchd::SystemLaunchctl;
        loop {
            if let Err(error) = write_core_heartbeat(&heartbeat_state).await {
                warn!(%error, "could not write custos heartbeat");
            }
            ticks = ticks.saturating_add(1);
            if ticks.is_multiple_of(5)
                && let Some(paths) = paths.as_ref()
            {
                for restart in ring::restart_stale_peers(
                    &heartbeat_state.data_dir,
                    CustosRole::Core,
                    paths,
                    &runner,
                    &mut last_attempt,
                ) {
                    if restart.success {
                        info!(peer = restart.role.as_str(), "custos restarted stale peer");
                    } else {
                        warn!(peer = restart.role.as_str(), detail = %restart.detail, "custos peer restart failed");
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });

    let result = accept_loop(listener, state).await;
    heartbeat_task.abort();
    let _ = fs::remove_file(&config.socket_path);
    let _ = fs::remove_file(ring::heartbeat_path(&config.data_dir, CustosRole::Core));
    discovery::remove_discovery(&config.data_dir);
    result
}

async fn accept_loop(listener: UnixListener, state: AppState) -> Result<()> {
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("custosd shutdown requested");
                break;
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _addr)) => {
                        let state = state.clone();
                        tokio::spawn(async move {
                            if let Err(error) = handle_connection(stream, state).await {
                                warn!(%error, "custos client connection failed");
                            }
                        });
                    }
                    Err(error) => {
                        warn!(%error, "custos accept failed");
                    }
                }
            }
        }
    }
    Ok(())
}

async fn handle_connection(stream: UnixStream, state: AppState) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    for _ in 0..MAX_REQUESTS_PER_CONNECTION {
        line.clear();
        let read = tokio::time::timeout(Duration::from_secs(15), reader.read_line(&mut line)).await;
        let read = match read {
            Ok(Ok(read)) => read,
            Ok(Err(error)) => return Err(error.into()),
            Err(_) => break,
        };
        if read == 0 {
            break;
        }
        if line.len() > MAX_REQUEST_BYTES {
            let response = RpcResponse::error(None, "REQUEST_TOO_LARGE", "request exceeds 64 KiB");
            write_response(&mut write_half, &response).await?;
            break;
        }
        let request: RpcRequest = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(error) => {
                let response =
                    RpcResponse::error(None, "INVALID_REQUEST", format!("invalid JSON: {error}"));
                write_response(&mut write_half, &response).await?;
                continue;
            }
        };
        let response = dispatch(&state, request).await;
        write_response(&mut write_half, &response).await?;
    }
    Ok(())
}

async fn write_response(
    write_half: &mut tokio::net::unix::OwnedWriteHalf,
    response: &RpcResponse,
) -> Result<()> {
    let mut bytes = serde_json::to_vec(response)?;
    bytes.push(b'\n');
    write_half.write_all(&bytes).await?;
    write_half.flush().await?;
    Ok(())
}

async fn dispatch(state: &AppState, request: RpcRequest) -> RpcResponse {
    match request.method.as_str() {
        "ping" => RpcResponse::ok(
            request.id,
            serde_json::to_value(ping(state)).unwrap_or(Value::Null),
        ),
        "status" => {
            let report = probe_cached(state).await;
            let processes = process_report(state);
            let vault = vault_view(state);
            RpcResponse::ok(
                request.id,
                serde_json::to_value(build_status(state, &report, &processes, vault.as_ref()))
                    .unwrap_or(Value::Null),
            )
        }
        "health" => {
            let report = probe_cached(state).await;
            let processes = process_report(state);
            let vault = vault_view(state);
            let health = Health {
                schema: crate::protocol::HEALTH_SCHEMA.to_string(),
                status: build_status(state, &report, &processes, vault.as_ref()),
                probe: report.info.clone(),
                processes: processes.samples.clone(),
                vault: vault.clone(),
            };
            RpcResponse::ok(
                request.id,
                serde_json::to_value(health).unwrap_or(Value::Null),
            )
        }
        "charter" => RpcResponse::ok(
            request.id,
            serde_json::to_value(charter::view(state.observe_only)).unwrap_or(Value::Null),
        ),
        "diagnostics.generate" => {
            let data_dir = state.data_dir.clone();
            match tokio::task::spawn_blocking(move || {
                diagnostics::generate(&data_dir, BundleMode::Both)
            })
            .await
            {
                Ok(Ok(result)) => {
                    let value = crate::protocol::DiagnosticsBundleResult {
                        schema: "verlinpark.custos_diagnostics/v1".to_string(),
                        report_id: result.report_id,
                        generated_at: result.generated_at,
                        directory: result.directory.to_string_lossy().to_string(),
                        standard_path: result
                            .standard_path
                            .map(|path| path.to_string_lossy().to_string()),
                        forensic_path: result
                            .forensic_path
                            .map(|path| path.to_string_lossy().to_string()),
                        bytes: result.bytes,
                    };
                    RpcResponse::ok(
                        request.id,
                        serde_json::to_value(value).unwrap_or(Value::Null),
                    )
                }
                Ok(Err(error)) => {
                    RpcResponse::error(request.id, "DIAGNOSTICS_FAILED", format!("{error:#}"))
                }
                Err(error) => RpcResponse::error(
                    request.id,
                    "DIAGNOSTICS_FAILED",
                    format!("diagnostics task failed: {error}"),
                ),
            }
        }
        method => RpcResponse::error(
            request.id,
            "METHOD_NOT_FOUND",
            format!("unknown read-only method: {method}"),
        ),
    }
}

fn ping(state: &AppState) -> PingResult {
    PingResult {
        schema: crate::protocol::PING_SCHEMA.to_string(),
        version: CUSTOS_VERSION.to_string(),
        protocol: CUSTOS_PROTOCOL_VERSION,
        pid: state.pid,
        role: state.role.as_str().to_string(),
        observe_only: state.observe_only,
        started_at: state.started_at_rfc.clone(),
        uptime_secs: state.started_at.elapsed().as_secs(),
    }
}

async fn probe_cached(state: &AppState) -> ProbeReport {
    {
        let cache = state.cache.lock().await;
        if let Some((at, report)) = cache.as_ref()
            && at.elapsed() <= PROBE_CACHE_TTL
        {
            return report.clone();
        }
    }
    let data_dir = state.data_dir.clone();
    let report = tokio::task::spawn_blocking(move || probe::probe(&data_dir))
        .await
        .unwrap_or_else(|error| probe::ProbeReport {
            counts: Default::default(),
            info: crate::protocol::ProbeInfo {
                db_present: false,
                db_open: false,
                user_version: None,
                journal_mode: None,
                events_total: 0,
                event_age_secs: None,
                table_counts: Default::default(),
                missing_tables: Vec::new(),
                errors: vec![format!("probe task failed: {error}")],
                probe_ms: 0,
            },
        });
    let mut cache = state.cache.lock().await;
    *cache = Some((Instant::now(), report.clone()));
    report
}

fn build_status(
    state: &AppState,
    report: &ProbeReport,
    processes: &ProcessReport,
    vault: Option<&VaultView>,
) -> Status {
    let ring = ring::snapshot(&state.data_dir);
    let dimensions = dimensions(state, report, &ring, processes, vault);
    let overall = overall_state(&dimensions);
    let mut counts = report.counts.clone();
    counts.processes_tracked = processes.tracked;
    counts.processes_pressure = processes.pressure;
    counts.processes_shadow_stop = processes.shadow_stop;
    Status {
        schema: crate::protocol::STATUS_SCHEMA.to_string(),
        generated_at: vp_protocol::now_rfc3339(),
        state: overall,
        observe_only: state.observe_only,
        role: state.role.as_str().to_string(),
        protocol: CUSTOS_PROTOCOL_VERSION,
        version: CUSTOS_VERSION.to_string(),
        charter_hash: charter::hash(),
        charter_status: charter::CHARTER_STATUS.to_string(),
        dimensions,
        counts,
        ring,
        last_action: None,
    }
}

fn process_report(state: &AppState) -> ProcessReport {
    let capacity = MachineCapacity::detect(&state.data_dir);
    state.process_cache.get(&state.data_dir, &capacity)
}

fn dimensions(
    state: &AppState,
    report: &ProbeReport,
    ring: &crate::protocol::RingView,
    processes: &ProcessReport,
    vault: Option<&VaultView>,
) -> Vec<Dimension> {
    let data_state = if !report.info.db_present {
        DimensionState::Watch
    } else if !report.info.db_open {
        DimensionState::Degraded
    } else if report.info.user_version != Some(probe::EXPECTED_SCHEMA_VERSION) {
        DimensionState::Critical
    } else {
        DimensionState::Ok
    };
    let data_detail = if !report.info.db_present {
        "database missing".to_string()
    } else if !report.info.db_open {
        "database present but not readable".to_string()
    } else if report.info.user_version != Some(probe::EXPECTED_SCHEMA_VERSION) {
        format!(
            "schema version {:?}, expected {}",
            report.info.user_version,
            probe::EXPECTED_SCHEMA_VERSION
        )
    } else {
        format!(
            "schema {} · wal {} bytes",
            report.info.user_version.unwrap_or_default(),
            report.counts.wal_bytes
        )
    };
    let runtime_state = if discovery_exists(&state.data_dir) {
        DimensionState::Ok
    } else {
        DimensionState::Watch
    };
    let leases_state = if report.counts.leases_expired > 0 {
        DimensionState::Degraded
    } else {
        DimensionState::Ok
    };
    let queue_state = if report.counts.merges_conflict > 0 || report.counts.tasks_blocked > 0 {
        DimensionState::Degraded
    } else if report.counts.merges_queued > 0 || report.counts.tasks_pending > 0 {
        DimensionState::Watch
    } else {
        DimensionState::Ok
    };
    let effects_state = if report.counts.effects_unknown > 0 {
        DimensionState::Degraded
    } else {
        DimensionState::Ok
    };
    let signal_state = match report.counts.event_age_secs {
        None => DimensionState::Watch,
        Some(age) if age > 300 => DimensionState::Degraded,
        Some(age) if age > 60 => DimensionState::Watch,
        Some(_) => DimensionState::Ok,
    };
    let ring_state = if ring.quorum || (ring.fresh == 1 && ring.mode == "single_role") {
        DimensionState::Ok
    } else if ring.fresh > 0 {
        DimensionState::Watch
    } else {
        DimensionState::Degraded
    };
    vec![
        Dimension {
            key: "runtime".to_string(),
            state: runtime_state,
            detail: if discovery_exists(&state.data_dir) {
                "daemon discovery present".to_string()
            } else {
                "daemon discovery missing".to_string()
            },
        },
        Dimension {
            key: "data".to_string(),
            state: data_state,
            detail: data_detail,
        },
        Dimension {
            key: "leases".to_string(),
            state: leases_state,
            detail: format!(
                "{} live · {} expired",
                report.counts.leases_live, report.counts.leases_expired
            ),
        },
        Dimension {
            key: "queues".to_string(),
            state: queue_state,
            detail: format!(
                "{} tasks pending · {} merges queued · {} conflicts",
                report.counts.tasks_pending,
                report.counts.merges_queued,
                report.counts.merges_conflict
            ),
        },
        Dimension {
            key: "effects".to_string(),
            state: effects_state,
            detail: format!("{} unknown", report.counts.effects_unknown),
        },
        Dimension {
            key: "signal".to_string(),
            state: signal_state,
            detail: match report.counts.event_age_secs {
                Some(age) => format!("last event {age}s ago"),
                None => "no events yet".to_string(),
            },
        },
        Dimension {
            key: "ring".to_string(),
            state: ring_state,
            detail: format!(
                "{} fresh · {} stale · {} missing",
                ring.fresh, ring.stale, ring.missing
            ),
        },
        Dimension {
            key: "processes".to_string(),
            state: if processes.pressure > 0 {
                DimensionState::Watch
            } else {
                DimensionState::Ok
            },
            detail: format!(
                "{} tracked · {} pressure · {} shadow",
                processes.tracked, processes.pressure, processes.shadow_stop
            ),
        },
        Dimension {
            key: "vault".to_string(),
            state: vault
                .map(|value| match value.state.as_str() {
                    "ok" => DimensionState::Ok,
                    "not_initialized" => DimensionState::Watch,
                    "drift" => DimensionState::Degraded,
                    "vault_drift" => DimensionState::Critical,
                    _ => DimensionState::Unknown,
                })
                .unwrap_or(DimensionState::Unknown),
            detail: vault
                .map(|value| value.detail.clone())
                .unwrap_or_else(|| "vault path unavailable".to_string()),
        },
    ]
}

fn vault_view(state: &AppState) -> Option<VaultView> {
    if let Ok(cache) = state.vault_cache.try_lock()
        && let Some((at, view)) = cache.as_ref()
        && at.elapsed() <= Duration::from_secs(30)
    {
        return view.clone();
    }
    let paths = ring::launchd_paths(&state.data_dir)?;
    let report = vault::verify(&state.data_dir, &paths, &paths.binary).ok()?;
    let files_ok = report
        .files
        .iter()
        .filter(|file| file.target_state == "ok")
        .count() as u32;
    let files_drift = report
        .files
        .iter()
        .filter(|file| file.target_state == "drift")
        .count() as u32;
    let files_missing = report
        .files
        .iter()
        .filter(|file| file.target_state == "missing")
        .count() as u32;
    let detail = match report.state {
        VaultState::Ok => format!("{files_ok} files verified"),
        VaultState::NotInitialized => "vault not initialized".to_string(),
        VaultState::Drift => format!("{files_drift} drift · {files_missing} missing"),
        VaultState::VaultDrift => "vault baseline drift".to_string(),
    };
    let view = VaultView {
        state: vault_state_str(report.state).to_string(),
        signature: report.signature,
        manifest_present: std::path::Path::new(&report.manifest_path).is_file(),
        files_ok,
        files_drift,
        files_missing,
        build_hash: report.build_hash,
        charter_hash: report.charter_hash,
        detail,
    };
    if let Ok(mut cache) = state.vault_cache.try_lock() {
        *cache = Some((Instant::now(), Some(view.clone())));
    }
    Some(view)
}

fn vault_state_str(state: VaultState) -> &'static str {
    match state {
        VaultState::Ok => "ok",
        VaultState::NotInitialized => "not_initialized",
        VaultState::Drift => "drift",
        VaultState::VaultDrift => "vault_drift",
    }
}

fn overall_state(dimensions: &[Dimension]) -> CustosState {
    if dimensions
        .iter()
        .any(|dimension| dimension.state == DimensionState::Critical)
    {
        CustosState::Critical
    } else if dimensions
        .iter()
        .any(|dimension| dimension.state == DimensionState::Degraded)
    {
        CustosState::Degraded
    } else if dimensions
        .iter()
        .any(|dimension| dimension.state == DimensionState::Watch)
    {
        CustosState::Watch
    } else {
        CustosState::Healthy
    }
}

fn discovery_exists(data_dir: &Path) -> bool {
    data_dir.join("run").join("daemon.json").is_file()
}

async fn write_core_heartbeat(state: &AppState) -> Result<()> {
    let seq = state.seq.fetch_add(1, Ordering::Relaxed);
    let heartbeat = Heartbeat::new(
        CustosRole::Core,
        state.pid,
        state.started_at_rfc.clone(),
        vp_protocol::now_rfc3339(),
        seq,
        discovery::build_hash(),
        charter::hash(),
        state.observe_only,
    );
    let data_dir = state.data_dir.clone();
    tokio::task::spawn_blocking(move || ring::write_heartbeat(&data_dir, &heartbeat))
        .await
        .context("heartbeat task failed")??;
    Ok(())
}

fn remove_stale_socket(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if std::os::unix::net::UnixStream::connect(path).is_ok() {
        bail!("custos socket is already live: {}", path.display());
    }
    fs::remove_file(path).with_context(|| format!("removing stale {}", path.display()))?;
    Ok(())
}

fn set_dir_mode(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn set_socket_mode(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::CustosRole;

    #[test]
    fn config_defaults_to_private_socket() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = ServerConfig::new(dir.path().to_path_buf(), CustosRole::Core, true);
        assert!(config.socket_path.ends_with("custos/custos.sock"));
    }

    #[test]
    fn overall_state_is_worst_dimension() {
        let dimensions = vec![
            Dimension {
                key: "a".into(),
                state: DimensionState::Ok,
                detail: String::new(),
            },
            Dimension {
                key: "b".into(),
                state: DimensionState::Degraded,
                detail: String::new(),
            },
        ];
        assert_eq!(overall_state(&dimensions), CustosState::Degraded);
    }
}

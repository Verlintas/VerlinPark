//! Local, user-initiated diagnostic bundles.
//!
//! This module implements Article 8 of `docs/CUSTOS_CHARTER_V0.md` as a local
//! file generator only:
//!
//! - no network, upload, email, provider call, or support-ticket surface;
//! - no writes to the VerlinPark data directory (SQLite is opened read-only);
//! - two bounded ZIPs (`standard` and `forensic`) with `0600` file permissions
//!   in a `0700` output directory;
//! - secrets, prompts, messages, tool output, file content, screen/DOM/
//!   accessibility data, argv values, full absolute paths, stack samples,
//!   memory dumps, and raw database rows are never collected.
//!
//! The manifest lists every other file with its SHA-256 and byte count and a
//! simple hash-chain head over those entries. The manifest deliberately does
//! not contain a hash of itself.
//!
//! The only user-facing entry point is [`generate`]; it resolves
//! `VERLINPARK_DIAGNOSTICS_DIR` (tests) or `~/Downloads/VerlinPark Diagnostics`
//! and never accepts a destination from a caller. [`generate_to`] is a
//! lower-level hook for tests and trusted host integrations that have already
//! resolved a directory.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::capacity::{self, EffectiveCaps, MachineCapacity, MemoryPressure};
use crate::charter;
use crate::discovery;
use crate::probe::{self, ProbeReport};
use crate::protocol::{
    CUSTOS_PROTOCOL_VERSION, CUSTOS_VERSION, Counts, CustosState, Dimension, DimensionState,
    ProbeInfo, RingView, STATUS_SCHEMA, Status,
};
use crate::ring;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

/// Environment override used by tests (and trusted host tooling) to keep a
/// diagnostic run out of the real user Downloads directory.
const DIAGNOSTICS_DIR_ENV: &str = "VERLINPARK_DIAGNOSTICS_DIR";
const DIAGNOSTICS_FOLDER_NAME: &str = "VerlinPark Diagnostics";

const MANIFEST_SCHEMA: &str = "verlinpark.custos_diagnostics/v1";
const HEALTH_SCHEMA: &str = "verlinpark.custos_diagnostics_health/v1";
const DB_STATS_SCHEMA: &str = "verlinpark.custos_diagnostics_db_stats/v1";
const QUEUE_SCHEMA: &str = "verlinpark.custos_diagnostics_queue/v1";
const LEASES_SCHEMA: &str = "verlinpark.custos_diagnostics_leases/v1";
const RUNS_SCHEMA: &str = "verlinpark.custos_diagnostics_runs/v1";
const WORKTREES_SCHEMA: &str = "verlinpark.custos_diagnostics_worktrees/v1";
const BACKUP_SCHEMA: &str = "verlinpark.custos_diagnostics_backup/v1";
const CONFIG_SCHEMA: &str = "verlinpark.custos_diagnostics_config/v1";
const TABLE_COUNTS_SCHEMA: &str = "verlinpark.custos_diagnostics_table_counts/v1";
const EVENT_METADATA_SCHEMA: &str = "verlinpark.custos_diagnostics_event_metadata/v1";
const PROCESSES_SCHEMA: &str = "verlinpark.custos_diagnostics_processes/v1";
const CAPACITY_SCHEMA: &str = "verlinpark.custos_diagnostics_capacity/v1";
const RING_SCHEMA: &str = "verlinpark.custos_diagnostics_ring/v1";

const STANDARD_MAX_BYTES: u64 = 8 * 1024 * 1024;
const FORENSIC_MAX_BYTES: u64 = 32 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;
const MAX_LOG_FILES: usize = 32;
const MAX_EVENT_TYPES: usize = 64;
const MAX_WORKTREE_PROJECTS: usize = 64;
const MAX_WORKTREE_SESSIONS: usize = 64;
const MAX_WORKTREE_CHILDREN: usize = 256;
const MAX_ENV_NAMES: usize = 128;
const MAX_LABEL_CHARS: usize = 120;

const EXCLUSIONS: &[&str] = &[
    "secrets",
    "provider_keys",
    "tokens",
    "prompts",
    "messages",
    "model_output",
    "tool_arguments",
    "tool_results",
    "file_contents",
    "diffs",
    "screen_captures",
    "browser_dom",
    "accessibility_text",
    "cookies",
    "full_absolute_paths",
    "process_argv",
    "stack_samples",
    "memory_dumps",
    "raw_database_rows",
    "full_environment_values",
];

const SENSITIVE_MARKERS: &[&str] = &[
    "key",
    "token",
    "secret",
    "password",
    "passwd",
    "authorization",
    "cookie",
    "credential",
    "bearer",
    "prompt",
    "tool_args",
    "tool_result",
    "private_key",
    "session_key",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BundleMode {
    Standard,
    Forensic,
    Both,
}

impl BundleMode {
    pub fn includes_standard(self) -> bool {
        matches!(self, Self::Standard | Self::Both)
    }

    pub fn includes_forensic(self) -> bool {
        matches!(self, Self::Forensic | Self::Both)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiagnosticsResult {
    pub report_id: String,
    pub generated_at: String,
    pub directory: PathBuf,
    pub standard_path: Option<PathBuf>,
    pub forensic_path: Option<PathBuf>,
    /// Generated ZIP sizes keyed by `standard` / `forensic`.
    pub bytes: BTreeMap<String, u64>,
}

/// Generate standard and/or forensic bundles in the user Downloads directory
/// (or `VERLINPARK_DIAGNOSTICS_DIR` when set).
pub fn generate(data_dir: &Path, mode: BundleMode) -> Result<DiagnosticsResult> {
    let (output_dir, origin) = resolve_output_dir(std::env::var_os(DIAGNOSTICS_DIR_ENV))?;
    generate_inner(data_dir, &output_dir, mode, origin)
}

/// Lower-level entry point with an explicit, already trusted output directory.
///
/// Tests use this instead of mutating process environment variables. The
/// user-facing path is [`generate`], which never accepts a caller destination.
pub fn generate_to(
    data_dir: &Path,
    output_dir: &Path,
    mode: BundleMode,
) -> Result<DiagnosticsResult> {
    generate_inner(data_dir, output_dir, mode, OutputOrigin::Explicit)
}

fn generate_inner(
    data_dir: &Path,
    output_dir: &Path,
    mode: BundleMode,
    origin: OutputOrigin,
) -> Result<DiagnosticsResult> {
    fs::create_dir_all(output_dir).context("creating diagnostics output directory")?;
    set_dir_mode(output_dir)?;

    let generated_at = vp_protocol::now_rfc3339();
    let report_id = new_report_id();
    let context = collect(data_dir, report_id.clone(), generated_at.clone());
    let stamp = compact_timestamp(&generated_at);
    let short = short_report(&report_id);

    let mut standard_path = None;
    if mode.includes_standard() {
        let files = build_contents(&context, "standard", origin, false)?;
        enforce_bundle_cap(&files, STANDARD_MAX_BYTES)?;
        let path = unique_output_path(
            output_dir,
            &format!("verlinpark-diagnostics-standard-{stamp}-{short}.zip"),
        )?;
        write_zip_file(&path, &files)?;
        standard_path = Some(path);
    }

    let mut forensic_path = None;
    if mode.includes_forensic() {
        let files = build_contents(&context, "forensic", origin, true)?;
        enforce_bundle_cap(&files, FORENSIC_MAX_BYTES)?;
        let path = unique_output_path(
            output_dir,
            &format!("verlinpark-diagnostics-forensic-{stamp}-{short}.zip"),
        )?;
        write_zip_file(&path, &files)?;
        forensic_path = Some(path);
    }

    let mut bytes = BTreeMap::new();
    if let Some(path) = &standard_path {
        bytes.insert("standard".to_string(), archive_bytes(path)?);
    }
    if let Some(path) = &forensic_path {
        bytes.insert("forensic".to_string(), archive_bytes(path)?);
    }

    Ok(DiagnosticsResult {
        report_id,
        generated_at,
        directory: output_dir.to_path_buf(),
        standard_path,
        forensic_path,
        bytes,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutputOrigin {
    UserDownloads,
    EnvironmentOverride,
    Explicit,
}

impl OutputOrigin {
    fn as_str(self) -> &'static str {
        match self {
            Self::UserDownloads => "user_downloads",
            Self::EnvironmentOverride => "environment_override",
            Self::Explicit => "explicit",
        }
    }
}

fn resolve_output_dir(override_dir: Option<OsString>) -> Result<(PathBuf, OutputOrigin)> {
    if let Some(value) = override_dir
        && !value.is_empty()
    {
        return Ok((PathBuf::from(value), OutputOrigin::EnvironmentOverride));
    }
    let downloads = dirs::download_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join("Downloads")))
        .ok_or_else(|| anyhow::anyhow!("could not locate the user Downloads directory"))?;
    Ok((
        downloads.join(DIAGNOSTICS_FOLDER_NAME),
        OutputOrigin::UserDownloads,
    ))
}

struct Collected {
    data_dir: PathBuf,
    report: ProbeReport,
    ring: RingView,
    capacity: MachineCapacity,
    caps: EffectiveCaps,
    daemon_version: Option<String>,
    custos_role: Option<String>,
    report_id: String,
    generated_at: String,
}

fn collect(data_dir: &Path, report_id: String, generated_at: String) -> Collected {
    let report = probe::probe(data_dir);
    let ring = ring::snapshot(data_dir);
    let capacity = MachineCapacity::detect(data_dir);
    let caps = EffectiveCaps::from_capacity(&capacity);
    let daemon_version = read_daemon_version(data_dir);
    let custos_role = discovery::read_discovery(data_dir)
        .ok()
        .flatten()
        .map(|discovery| discovery.role);
    Collected {
        data_dir: data_dir.to_path_buf(),
        report,
        ring,
        capacity,
        caps,
        daemon_version,
        custos_role,
        report_id,
        generated_at,
    }
}

fn build_contents(
    context: &Collected,
    kind: &str,
    origin: OutputOrigin,
    forensic: bool,
) -> Result<BTreeMap<String, Vec<u8>>> {
    let status = status_snapshot(context);
    let probe = sanitized_probe(context);
    let system = system_summary(context);
    let generated_at = &context.generated_at;
    let mut files = BTreeMap::new();

    insert_json(&mut files, "summary.json", &status)?;
    insert_json(
        &mut files,
        "health.json",
        &health_value(&status, &probe.info, generated_at)?,
    )?;
    insert_json(&mut files, "db_stats.json", &db_stats(context))?;
    insert_json(
        &mut files,
        "queue.json",
        &queue_json(
            &context.report.counts,
            &probe.info.table_counts,
            generated_at,
        ),
    )?;
    insert_json(
        &mut files,
        "leases.json",
        &leases_json(&context.report.counts, generated_at),
    )?;
    insert_json(
        &mut files,
        "runs.json",
        &runs_json(
            &probe.info.table_counts,
            &context.report.counts,
            generated_at,
        ),
    )?;
    insert_json(
        &mut files,
        "worktrees.json",
        &worktree_summary(&context.data_dir, generated_at),
    )?;
    insert_json(&mut files, "backup.json", &backup_json(generated_at))?;
    insert_json(
        &mut files,
        "config_redacted.json",
        &config_json(context, origin, generated_at),
    )?;
    insert_file(
        &mut files,
        "system.txt",
        system_text(context, &system).into_bytes(),
    )?;
    insert_file(&mut files, "README.txt", readme_text(kind).into_bytes())?;

    if forensic {
        insert_json(
            &mut files,
            "table_counts.json",
            &table_counts_json(&probe.info.table_counts, generated_at),
        )?;
        insert_json(&mut files, "event_metadata.json", &event_metadata(context))?;
        insert_json(
            &mut files,
            "processes.json",
            &processes_json(
                &context.report.counts,
                &probe.info.table_counts,
                generated_at,
            ),
        )?;
        insert_json(
            &mut files,
            "capacity.json",
            &capacity_json(context, generated_at),
        )?;
        let logs_dir = context.data_dir.join("custos").join("logs");
        insert_file(
            &mut files,
            "log_tail.out.txt",
            build_log_tail(&logs_dir, false),
        )?;
        insert_file(
            &mut files,
            "log_tail.err.txt",
            build_log_tail(&logs_dir, true),
        )?;
        insert_json(&mut files, "ring.json", &ring_json(context, generated_at))?;
    }

    let manifest = build_manifest(context, kind, &system, &files)?;
    insert_file(&mut files, "manifest.json", manifest)?;
    Ok(files)
}

struct SystemSummary {
    os: String,
    hardware: String,
    uname: String,
}

fn system_summary(context: &Collected) -> SystemSummary {
    let uname = fixed_command("uname", &["-s", "-r", "-m"]);
    let os = format!("{} {}", std::env::consts::OS, std::env::consts::ARCH);
    let hardware = format!(
        "{} logical cores · {:.1} GiB memory · {:.1} GiB disk free of {:.1} GiB",
        context.capacity.logical_cores,
        context.capacity.memory_gib(),
        context.capacity.disk_free_gib(),
        context.capacity.disk_total_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
    );
    SystemSummary {
        os,
        hardware,
        uname,
    }
}

fn fixed_command(program: &str, args: &[&str]) -> String {
    match std::process::Command::new(program).args(args).output() {
        Ok(output) if output.status.success() => {
            scrub_text(&String::from_utf8_lossy(&output.stdout))
                .trim()
                .to_string()
        }
        _ => "unavailable".to_string(),
    }
}

fn system_text(context: &Collected, system: &SystemSummary) -> String {
    format!(
        "VerlinPark Custos diagnostics system summary\n\
generated_at: {}\n\
os: {}\n\
arch: {}\n\
uname: {}\n\
logical_cores: {}\n\
physical_memory_bytes: {}\n\
physical_memory_gib: {:.2}\n\
disk_free_bytes: {}\n\
disk_total_bytes: {}\n\
disk_free_gib: {:.2}\n\
disk_total_gib: {:.2}\n\
memory_pressure: {}\n\
thermal: {}\n\
low_power: {}\n\
note: hostname, user paths, and environment values are not collected\n",
        context.generated_at,
        system.os,
        std::env::consts::ARCH,
        system.uname,
        context.capacity.logical_cores,
        context.capacity.physical_memory_bytes,
        context.capacity.memory_gib(),
        context.capacity.disk_free_bytes,
        context.capacity.disk_total_bytes,
        context.capacity.disk_free_gib(),
        context.capacity.disk_total_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
        memory_pressure_label(context.capacity.memory_pressure),
        context.capacity.thermal.as_str(),
        context.capacity.low_power,
    )
}

fn readme_text(kind: &str) -> String {
    format!(
        "VerlinPark Custos diagnostic bundle\n\
bundle_kind: {kind}\n\
\n\
This file was generated locally by Custos, the sealed VerlinPark integrity\n\
guardian. It was never uploaded, emailed, or sent to a provider. There is no\n\
network operation in this bundle generator.\n\
\n\
Included: coarse health state, capacity and database metadata, status counts,\n\
lease/run/queue counts, worktree basenames, redacted configuration names, and\n\
(for forensic bundles) event-type counts, known-process states, ring heartbeats,\n\
and secret/path-scrubbed log tails.\n\
\n\
Never collected: provider API keys, auth headers, cookies, Keychain items,\n\
private keys, prompts, system messages, conversation bodies, model outputs,\n\
tool arguments/results, file contents, diffs/patches, screen captures, browser\n\
DOM or accessibility text, clipboard contents, full environment values, raw\n\
database rows containing user/agent content, full absolute paths, process argv\n\
values, stack samples, thread stacks, and memory dumps.\n\
\n\
Redaction: user home-directory prefixes are reduced to ~; absolute paths are\n\
reduced to basenames; lines mentioning keys, tokens, secrets, passwords,\n\
authorization, cookies, credentials, bearer values, prompts, or tool output\n\
are dropped. Environment variable names may be listed, but never their\n\
values.\n\
\n\
You can delete this bundle at any time. Custos keeps no copy after generation.\n"
    )
}

fn status_snapshot(context: &Collected) -> Status {
    let dimensions = dimensions(context);
    let state = overall_state(&dimensions);
    Status {
        schema: STATUS_SCHEMA.to_string(),
        generated_at: context.generated_at.clone(),
        state,
        observe_only: true,
        role: "core".to_string(),
        protocol: CUSTOS_PROTOCOL_VERSION,
        version: CUSTOS_VERSION.to_string(),
        charter_hash: charter::hash(),
        charter_status: charter::CHARTER_STATUS.to_string(),
        dimensions,
        counts: context.report.counts.clone(),
        ring: context.ring.clone(),
        last_action: None,
    }
}

fn dimensions(context: &Collected) -> Vec<Dimension> {
    let info = &context.report.info;
    let counts = &context.report.counts;

    let data_state = if !info.db_present {
        DimensionState::Watch
    } else if !info.db_open {
        DimensionState::Degraded
    } else if info.user_version != Some(probe::EXPECTED_SCHEMA_VERSION) {
        DimensionState::Critical
    } else {
        DimensionState::Ok
    };
    let data_detail = if !info.db_present {
        "database missing".to_string()
    } else if !info.db_open {
        "database present but not readable".to_string()
    } else if info.user_version != Some(probe::EXPECTED_SCHEMA_VERSION) {
        format!(
            "schema version {:?}, expected {}",
            info.user_version,
            probe::EXPECTED_SCHEMA_VERSION
        )
    } else {
        format!(
            "schema {} · wal {} bytes",
            info.user_version.unwrap_or_default(),
            counts.wal_bytes
        )
    };

    let runtime_present = context.data_dir.join("run").join("daemon.json").is_file();
    let leases_state = if counts.leases_expired > 0 {
        DimensionState::Degraded
    } else {
        DimensionState::Ok
    };
    let queue_state = if counts.merges_conflict > 0 || counts.tasks_blocked > 0 {
        DimensionState::Degraded
    } else if counts.merges_queued > 0 || counts.tasks_pending > 0 {
        DimensionState::Watch
    } else {
        DimensionState::Ok
    };
    let effects_state = if counts.effects_unknown > 0 {
        DimensionState::Degraded
    } else {
        DimensionState::Ok
    };
    let signal_state = match counts.event_age_secs {
        None => DimensionState::Watch,
        Some(age) if age > 300 => DimensionState::Degraded,
        Some(age) if age > 60 => DimensionState::Watch,
        Some(_) => DimensionState::Ok,
    };
    let ring_state =
        if context.ring.quorum || (context.ring.fresh == 1 && context.ring.mode == "single_role") {
            DimensionState::Ok
        } else if context.ring.fresh > 0 {
            DimensionState::Watch
        } else {
            DimensionState::Degraded
        };

    vec![
        Dimension {
            key: "runtime".to_string(),
            state: if runtime_present {
                DimensionState::Ok
            } else {
                DimensionState::Watch
            },
            detail: if runtime_present {
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
                counts.leases_live, counts.leases_expired
            ),
        },
        Dimension {
            key: "queues".to_string(),
            state: queue_state,
            detail: format!(
                "{} tasks pending · {} merges queued · {} conflicts",
                counts.tasks_pending, counts.merges_queued, counts.merges_conflict
            ),
        },
        Dimension {
            key: "effects".to_string(),
            state: effects_state,
            detail: format!("{} unknown", counts.effects_unknown),
        },
        Dimension {
            key: "signal".to_string(),
            state: signal_state,
            detail: match counts.event_age_secs {
                Some(age) => format!("last event {age}s ago"),
                None => "no events yet".to_string(),
            },
        },
        Dimension {
            key: "ring".to_string(),
            state: ring_state,
            detail: format!(
                "{} fresh · {} stale · {} missing",
                context.ring.fresh, context.ring.stale, context.ring.missing
            ),
        },
    ]
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

fn sanitized_probe(context: &Collected) -> ProbeReport {
    let mut probe = context.report.clone();
    probe.info.errors = probe
        .info
        .errors
        .iter()
        .map(|error| scrub_text(error))
        .collect();
    probe.info.table_counts = sanitized_table_counts(&probe.info.table_counts);
    probe
}

fn health_value(status: &Status, probe: &ProbeInfo, generated_at: &str) -> Result<Value> {
    let mut value = serde_json::to_value(probe).context("serializing probe metadata")?;
    if let Value::Object(map) = &mut value {
        map.insert("schema".to_string(), json!(HEALTH_SCHEMA));
        map.insert("generated_at".to_string(), json!(generated_at));
        map.insert("state".to_string(), json!(status.state.as_str()));
    }
    Ok(value)
}

fn db_stats(context: &Collected) -> Value {
    let info = &context.report.info;
    let counts = &context.report.counts;
    let db_basename = probe::database_path(&context.data_dir)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "verlinpark.db".to_string());
    json!({
        "schema": DB_STATS_SCHEMA,
        "generated_at": context.generated_at.clone(),
        "db_basename": db_basename,
        "db_present": info.db_present,
        "db_open": info.db_open,
        "user_version": info.user_version,
        "journal_mode": info.journal_mode.clone(),
        "db_bytes": counts.db_bytes,
        "wal_bytes": counts.wal_bytes,
        "disk_free_bytes": counts.disk_free_bytes,
        "disk_total_bytes": counts.disk_total_bytes,
        "quick_check": "not_probed",
        "note": "file sizes, schema metadata, and probe results only; no database rows are read"
    })
}

fn queue_json(
    counts: &Counts,
    tables: &BTreeMap<String, BTreeMap<String, u64>>,
    generated_at: &str,
) -> Value {
    json!({
        "schema": QUEUE_SCHEMA,
        "generated_at": generated_at,
        "tasks": {
            "pending": counts.tasks_pending,
            "running": counts.tasks_running,
            "blocked": counts.tasks_blocked,
            "by_status": tables.get("plan_tasks")
        },
        "merges": {
            "queued": counts.merges_queued,
            "conflict": counts.merges_conflict,
            "by_status": tables.get("merge_queue")
        },
        "approvals": {
            "pending": counts.approvals_pending,
            "by_status": tables.get("approvals")
        },
        "effects": {
            "unknown": counts.effects_unknown,
            "by_state": tables.get("effect_receipts")
        },
        "note": "status counts only; no payloads, ids, owners, or prompts"
    })
}

fn leases_json(counts: &Counts, generated_at: &str) -> Value {
    json!({
        "schema": LEASES_SCHEMA,
        "generated_at": generated_at,
        "live": counts.leases_live,
        "expired": counts.leases_expired,
        "note": "counts only; lease tokens, owners, and paths are never included"
    })
}

fn runs_json(
    tables: &BTreeMap<String, BTreeMap<String, u64>>,
    counts: &Counts,
    generated_at: &str,
) -> Value {
    json!({
        "schema": RUNS_SCHEMA,
        "generated_at": generated_at,
        "running": counts.runs_running,
        "interrupted": counts.runs_interrupted,
        "by_status": tables.get("session_runs"),
        "note": "run status counts only; no prompts, messages, or tool output"
    })
}

fn worktree_summary(data_dir: &Path, generated_at: &str) -> Value {
    let root = data_dir.join("worktrees");
    if !root.is_dir() {
        return json!({
            "schema": WORKTREES_SCHEMA,
            "generated_at": generated_at,
            "available": false,
            "project_count": 0,
            "worktree_count": 0,
            "projects": [],
            "truncated": false,
            "note": "worktrees directory not present; basenames and counts only"
        });
    }

    let mut projects = Vec::new();
    let mut total_worktrees = 0u64;
    let mut truncated = false;
    if let Ok(entries) = fs::read_dir(&root) {
        for entry in entries.flatten() {
            if projects.len() >= MAX_WORKTREE_PROJECTS {
                truncated = true;
                break;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            let raw_name = entry.file_name().to_string_lossy().to_string();
            let name = redact_label(&raw_name);
            let mut sessions = 0u32;
            let mut worktrees = 0u32;
            if let Ok(session_entries) = fs::read_dir(entry.path()) {
                for session in session_entries.flatten() {
                    if sessions as usize >= MAX_WORKTREE_SESSIONS {
                        truncated = true;
                        break;
                    }
                    if !session
                        .file_type()
                        .map(|kind| kind.is_dir())
                        .unwrap_or(false)
                    {
                        continue;
                    }
                    sessions += 1;
                    if let Ok(tree_entries) = fs::read_dir(session.path()) {
                        for tree in tree_entries.flatten() {
                            if worktrees as usize >= MAX_WORKTREE_CHILDREN {
                                truncated = true;
                                break;
                            }
                            if tree.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                                worktrees += 1;
                            }
                        }
                    }
                }
            }
            total_worktrees = total_worktrees.saturating_add(u64::from(worktrees));
            projects.push(json!({
                "project": name,
                "sessions": sessions,
                "worktrees": worktrees
            }));
        }
    }

    json!({
        "schema": WORKTREES_SCHEMA,
        "generated_at": generated_at,
        "available": true,
        "project_count": projects.len(),
        "worktree_count": total_worktrees,
        "projects": projects,
        "truncated": truncated,
        "note": "project and session basenames with directory counts only; no absolute paths"
    })
}

fn backup_json(generated_at: &str) -> Value {
    json!({
        "schema": BACKUP_SCHEMA,
        "generated_at": generated_at,
        "status": "not_probed",
        "verify": "not_run",
        "repository_generation": null,
        "reason": "backup repository verification is unavailable in the observe-only diagnostics slice",
        "note": "no keys, passphrases, ciphertext, or repository paths are collected"
    })
}

fn config_json(context: &Collected, origin: OutputOrigin, generated_at: &str) -> Value {
    let mut env_names: Vec<String> = std::env::vars_os()
        .filter_map(|(name, _value)| name.into_string().ok())
        .filter(|name| name.starts_with("VERLINPARK_") || name.starts_with("CUSTOS_"))
        .collect();
    env_names.sort();
    env_names.dedup();
    env_names.truncate(MAX_ENV_NAMES);

    json!({
        "schema": CONFIG_SCHEMA,
        "generated_at": generated_at,
        "charter_version": charter::CHARTER_VERSION,
        "charter_status": charter::CHARTER_STATUS,
        "observe_only": true,
        "custos_version": CUSTOS_VERSION,
        "custos_protocol": CUSTOS_PROTOCOL_VERSION,
        "custos_role": context.custos_role.clone(),
        "daemon_version": context.daemon_version.clone(),
        "output_origin": origin.as_str(),
        "env_names_present": env_names,
        "environment_note": "environment variable names only; values are never read into the bundle",
        "path_note": "absolute data and output paths are never included"
    })
}

fn table_counts_json(
    tables: &BTreeMap<String, BTreeMap<String, u64>>,
    generated_at: &str,
) -> Value {
    json!({
        "schema": TABLE_COUNTS_SCHEMA,
        "generated_at": generated_at,
        "tables": tables,
        "note": "probe status counts only; no table rows or values"
    })
}

fn event_metadata(context: &Collected) -> Value {
    let info = &context.report.info;
    if info.missing_tables.iter().any(|table| table == "events") {
        return json!({
            "schema": EVENT_METADATA_SCHEMA,
            "generated_at": context.generated_at.clone(),
            "available": false,
            "reason": "events table missing",
            "events_total": 0,
            "event_age_secs": Value::Null,
            "by_type": {},
            "truncated": false
        });
    }

    let path = probe::database_path(&context.data_dir);
    let Ok(connection) = open_read_only(&path) else {
        return json!({
            "schema": EVENT_METADATA_SCHEMA,
            "generated_at": context.generated_at.clone(),
            "available": false,
            "reason": "database not readable for event metadata",
            "events_total": info.events_total,
            "event_age_secs": info.event_age_secs,
            "by_type": {},
            "truncated": false
        });
    };

    let mut by_type = BTreeMap::new();
    let mut truncated = false;
    let query = connection.prepare(
        "SELECT type, COUNT(*) FROM events \
         GROUP BY type ORDER BY COUNT(*) DESC LIMIT 65",
    );
    let Ok(mut statement) = query else {
        return json!({
            "schema": EVENT_METADATA_SCHEMA,
            "generated_at": context.generated_at.clone(),
            "available": false,
            "reason": "event metadata query unavailable",
            "events_total": info.events_total,
            "event_age_secs": info.event_age_secs,
            "by_type": {},
            "truncated": false
        });
    };
    if let Ok(rows) = statement.query_map([], |row| {
        let kind: Option<String> = row.get(0)?;
        let count: i64 = row.get(1)?;
        Ok((
            kind.unwrap_or_else(|| "(null)".to_string()),
            count.max(0) as u64,
        ))
    }) {
        for (index, row) in rows.flatten().enumerate() {
            if index >= MAX_EVENT_TYPES {
                truncated = true;
                break;
            }
            let (kind, count) = row;
            if let Some(kind) = redact_key(&kind) {
                *by_type.entry(kind).or_insert(0u64) += count;
            }
        }
    }

    json!({
        "schema": EVENT_METADATA_SCHEMA,
        "generated_at": context.generated_at.clone(),
        "available": true,
        "events_total": info.events_total,
        "event_age_secs": info.event_age_secs,
        "by_type": by_type,
        "truncated": truncated,
        "note": "event type counts only; payloads are never selected"
    })
}

fn processes_json(
    counts: &Counts,
    tables: &BTreeMap<String, BTreeMap<String, u64>>,
    generated_at: &str,
) -> Value {
    json!({
        "schema": PROCESSES_SCHEMA,
        "generated_at": generated_at,
        "known_processes": {
            "live": counts.processes_live,
            "by_state": tables.get("supervised_processes")
        },
        "note": "known-process states and counts only; no argv, environment, paths, or resource samples"
    })
}

fn capacity_json(context: &Collected, generated_at: &str) -> Value {
    json!({
        "schema": CAPACITY_SCHEMA,
        "generated_at": generated_at,
        "machine": context.capacity,
        "effective_caps": context.caps,
        "capacity_multiplier": capacity::capacity_multiplier(&context.capacity),
        "formula_version": "v0",
        "note": "deterministic local capacity profile; no process arguments or user paths"
    })
}

fn ring_json(context: &Collected, generated_at: &str) -> Value {
    let mut heartbeats = Vec::new();
    for role in ring::ALL_ROLES {
        match ring::read_heartbeat(&context.data_dir, role) {
            Ok(Some((heartbeat, age_secs))) => {
                heartbeats.push(json!({
                    "role": heartbeat.role,
                    "pid": heartbeat.pid,
                    "started_at": heartbeat.started_at,
                    "updated_at": heartbeat.updated_at,
                    "seq": heartbeat.seq,
                    "build_hash": heartbeat.build_hash,
                    "charter_hash": heartbeat.charter_hash,
                    "observe_only": heartbeat.observe_only,
                    "age_secs": age_secs
                }));
            }
            Ok(None) => {}
            Err(_) => {
                heartbeats.push(json!({
                    "role": role.as_str(),
                    "state": "invalid"
                }));
            }
        }
    }
    json!({
        "schema": RING_SCHEMA,
        "generated_at": generated_at,
        "snapshot": context.ring,
        "heartbeats": heartbeats,
        "note": "role heartbeat metadata only; no socket paths or tokens"
    })
}

#[derive(Clone, Debug, Serialize)]
struct ManifestFile {
    path: String,
    bytes: u64,
    sha256: String,
}

fn build_manifest(
    context: &Collected,
    kind: &str,
    system: &SystemSummary,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<Vec<u8>> {
    let mut entries = Vec::with_capacity(files.len());
    let mut chain_input = String::new();
    for (name, bytes) in files {
        let hash = sha256_hex(bytes);
        chain_input.push_str(name);
        chain_input.push('\0');
        chain_input.push_str(&hash);
        chain_input.push('\n');
        entries.push(ManifestFile {
            path: name.clone(),
            bytes: bytes.len() as u64,
            sha256: hash,
        });
    }
    let chain_head = format!("sha256:{}", sha256_hex(chain_input.as_bytes()));
    let forensic = kind == "forensic";
    let value = json!({
        "schema": MANIFEST_SCHEMA,
        "report_id": context.report_id,
        "generated_at": context.generated_at,
        "bundle_kind": kind,
        "app_version": Value::Null,
        "daemon_version": context.daemon_version.clone(),
        "custos_version": CUSTOS_VERSION,
        "custos_protocol": CUSTOS_PROTOCOL_VERSION,
        "custos_role": context.custos_role.clone(),
        "build_hash": discovery::build_hash(),
        "charter_hash": charter::hash(),
        "charter_version": charter::CHARTER_VERSION,
        "charter_status": charter::CHARTER_STATUS,
        "os": system.os,
        "hardware": system.hardware,
        "redaction_profile": kind,
        "exclusions": EXCLUSIONS,
        "files": entries,
        "chain_head": chain_head,
        "limits": {
            "max_bundle_bytes": if forensic { FORENSIC_MAX_BYTES } else { STANDARD_MAX_BYTES },
            "max_file_bytes": MAX_FILE_BYTES,
            "log_tail_max_bytes": MAX_LOG_BYTES
        },
        "note": "manifest hashes cover every file except manifest.json itself; chain_head is a simple SHA-256 hash chain over the listed entries"
    });
    let mut bytes = serde_json::to_vec_pretty(&value).context("serializing manifest")?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn insert_json<T: Serialize + ?Sized>(
    files: &mut BTreeMap<String, Vec<u8>>,
    name: &str,
    value: &T,
) -> Result<()> {
    let mut bytes =
        serde_json::to_vec_pretty(value).with_context(|| format!("serializing {name}"))?;
    bytes.push(b'\n');
    insert_file(files, name, bytes)
}

fn insert_file(files: &mut BTreeMap<String, Vec<u8>>, name: &str, bytes: Vec<u8>) -> Result<()> {
    if bytes.len() as u64 > MAX_FILE_BYTES {
        bail!("diagnostic file {name} exceeds the per-file byte cap");
    }
    files.insert(name.to_string(), bytes);
    Ok(())
}

fn enforce_bundle_cap(files: &BTreeMap<String, Vec<u8>>, cap: u64) -> Result<()> {
    let total = files.values().map(|bytes| bytes.len() as u64).sum::<u64>();
    if total > cap {
        bail!("diagnostic bundle contents exceed the bundle byte cap");
    }
    Ok(())
}

fn build_log_tail(dir: &Path, want_err: bool) -> Vec<u8> {
    let mut raw = String::new();
    let mut remaining = MAX_LOG_BYTES as usize;
    for (path, name, is_err) in log_files(dir) {
        if is_err != want_err {
            continue;
        }
        if remaining <= 128 {
            break;
        }
        let header = format!("--- {} ---\n", redact_label(&name));
        if header.len() >= remaining {
            break;
        }
        raw.push_str(&header);
        remaining -= header.len();
        let before = raw.len();
        read_tail(&path, remaining as u64, &mut raw);
        if raw.len() == before {
            raw.push_str("[unreadable log]\n");
        }
        remaining = (MAX_LOG_BYTES as usize).saturating_sub(raw.len());
    }
    if raw.is_empty() {
        let stream = if want_err { "stderr" } else { "stdout" };
        raw = format!("no {stream} log files found under custos/logs\n");
    }

    let mut text = scrub_text(&raw);
    if text.len() > MAX_LOG_BYTES as usize {
        let mut cutoff = MAX_LOG_BYTES as usize;
        while cutoff > 0 && !text.is_char_boundary(cutoff) {
            cutoff -= 1;
        }
        text.truncate(cutoff);
    }
    text.into_bytes()
}

fn log_files(dir: &Path) -> Vec<(PathBuf, String, bool)> {
    let mut files = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return files;
    };
    for entry in entries.flatten() {
        if files.len() >= MAX_LOG_FILES {
            break;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        // Skip symlinks: diagnostics never follows a log link outside the
        // daemon-owned directory.
        if !file_type.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let lower = name.to_ascii_lowercase();
        if !lower.ends_with(".log") {
            continue;
        }
        let is_err = lower.contains("err") || lower.contains("stderr") || lower.contains("error");
        files.push((entry.path(), name, is_err));
    }
    files.sort_by(|left, right| left.1.cmp(&right.1));
    files
}

fn read_tail(path: &Path, max: u64, output: &mut String) {
    if max == 0 {
        return;
    }
    let Ok(mut file) = File::open(path) else {
        return;
    };
    let Ok(metadata) = file.metadata() else {
        return;
    };
    if !metadata.is_file() {
        return;
    }
    let length = metadata.len();
    let truncated = length > max;
    if truncated
        && file
            .seek(SeekFrom::Start(length.saturating_sub(max)))
            .is_err()
    {
        return;
    }
    let mut bytes = Vec::new();
    if file.take(max).read_to_end(&mut bytes).is_err() {
        return;
    }
    if truncated {
        output.push_str("[truncated to the last 2 MiB]\n");
    }
    output.push_str(&String::from_utf8_lossy(&bytes));
}

fn read_daemon_version(data_dir: &Path) -> Option<String> {
    let path = data_dir.join("run").join("daemon.json");
    let bytes = read_bounded(&path, 64 * 1024)?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    let version = value.get("version")?.as_str()?;
    let version = scrub_text(version).trim().to_string();
    if version.is_empty() {
        None
    } else {
        Some(version.chars().take(80).collect())
    }
}

fn read_bounded(path: &Path, max: u64) -> Option<Vec<u8>> {
    // Reject symlinks so a diagnostics run cannot be pointed at an unrelated
    // file through the daemon discovery path.
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() {
        return None;
    }
    let file = File::open(path).ok()?;
    let mut reader = file.take(max);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

fn open_read_only(path: &Path) -> Result<Connection> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(Duration::from_millis(500))?;
    connection.execute_batch("PRAGMA query_only = ON;")?;
    Ok(connection)
}

fn sanitized_table_counts(
    tables: &BTreeMap<String, BTreeMap<String, u64>>,
) -> BTreeMap<String, BTreeMap<String, u64>> {
    let mut sanitized = BTreeMap::new();
    for (table, values) in tables {
        let Some(table) = redact_key(table) else {
            continue;
        };
        let mut counts = BTreeMap::new();
        for (status, count) in values {
            if let Some(status) = redact_key(status) {
                counts.insert(status, *count);
            }
        }
        sanitized.insert(table, counts);
    }
    sanitized
}

fn redact_key(value: &str) -> Option<String> {
    if is_sensitive_line(value) {
        return None;
    }
    let cleaned = scrub_line(value);
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.chars().take(160).collect())
    }
}

fn redact_label(value: &str) -> String {
    if is_sensitive_line(value) {
        return "[redacted]".to_string();
    }
    scrub_line(value)
        .trim()
        .chars()
        .take(MAX_LABEL_CHARS)
        .collect()
}

fn memory_pressure_label(value: MemoryPressure) -> &'static str {
    match value {
        MemoryPressure::Normal => "normal",
        MemoryPressure::Warning => "warning",
        MemoryPressure::Critical => "critical",
    }
}

fn new_report_id() -> String {
    let mut bytes = [0u8; 8];
    let filled = File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .is_ok();
    if !filled {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let seed = format!(
            "{}-{}-{:?}",
            now.as_nanos(),
            std::process::id(),
            std::thread::current().id()
        );
        let digest = Sha256::digest(seed.as_bytes());
        bytes.copy_from_slice(&digest[..8]);
    }
    format!("cdr_{}", hex(&bytes))
}

fn short_report(report_id: &str) -> String {
    let hex_part = report_id.strip_prefix("cdr_").unwrap_or(report_id);
    let start = hex_part.len().saturating_sub(8);
    hex_part[start..].to_string()
}

fn compact_timestamp(generated_at: &str) -> String {
    let digits: String = generated_at
        .chars()
        .filter(|character| character.is_ascii_digit())
        .collect();
    if digits.len() >= 14 {
        digits[..14].to_string()
    } else if digits.is_empty() {
        format!(
            "{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        )
    } else {
        digits
    }
}

fn unique_output_path(dir: &Path, file_name: &str) -> Result<PathBuf> {
    let candidate = dir.join(file_name);
    if !candidate.exists() {
        return Ok(candidate);
    }
    let (stem, extension) = match file_name.rsplit_once('.') {
        Some(parts) => parts,
        None => (file_name, "zip"),
    };
    for index in 1..=1000 {
        let candidate = dir.join(format!("{stem}-{index}.{extension}"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    bail!("could not choose a unique diagnostics file name")
}

fn archive_bytes(path: &Path) -> Result<u64> {
    Ok(fs::metadata(path)
        .context("reading diagnostics archive metadata")?
        .len())
}

fn write_zip_file(path: &Path, files: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .context("diagnostics archive path has no file name")?;
    let temporary = path.with_file_name(format!(".{file_name}.partial"));

    let result = write_zip_partial(&temporary, files);
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    set_file_mode(&temporary)?;
    fs::rename(&temporary, path).context("finalizing diagnostics archive")?;
    set_file_mode(path)?;
    Ok(())
}

fn write_zip_partial(path: &Path, files: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options.open(path).context("creating diagnostics archive")?;
    let mut archive = ZipWriter::new(file);
    for (name, bytes) in files {
        let entry_options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .unix_permissions(0o600);
        archive
            .start_file(name.as_str(), entry_options)
            .with_context(|| format!("writing diagnostics entry {name}"))?;
        archive
            .write_all(bytes)
            .with_context(|| format!("writing diagnostics entry {name}"))?;
    }
    let file = archive.finish().context("finishing diagnostics archive")?;
    file.sync_all().context("syncing diagnostics archive")?;
    Ok(())
}

#[cfg(unix)]
fn set_dir_mode(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .context("setting diagnostics directory permissions")?;
    Ok(())
}

#[cfg(not(unix))]
fn set_dir_mode(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_file_mode(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .context("setting diagnostics file permissions")?;
    Ok(())
}

#[cfg(not(unix))]
fn set_file_mode(_path: &Path) -> Result<()> {
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn scrub_text(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for (index, line) in input.split('\n').enumerate() {
        if index > 0 {
            output.push('\n');
        }
        if is_sensitive_line(line) {
            output.push_str("[redacted-line]");
            continue;
        }
        output.push_str(&scrub_line(line));
    }
    output
}

fn scrub_line(line: &str) -> String {
    let line = scrub_urls(line);
    scrub_absolute_paths(&line)
}

fn is_sensitive_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    SENSITIVE_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
}

fn scrub_urls(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut output = String::with_capacity(line.len());
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        let Some(relative) = line[cursor..].find("://") else {
            output.push_str(&line[cursor..]);
            break;
        };
        let scheme_end = cursor + relative;
        let mut scheme_start = scheme_end;
        while scheme_start > cursor {
            let byte = bytes[scheme_start - 1];
            if byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'.' | b'-') {
                scheme_start -= 1;
            } else {
                break;
            }
        }
        if scheme_start == scheme_end {
            output.push_str(&line[cursor..scheme_end + 3]);
            cursor = scheme_end + 3;
            continue;
        }
        let mut url_end = scheme_end + 3;
        while url_end < bytes.len() {
            let byte = bytes[url_end];
            if byte.is_ascii_whitespace()
                || matches!(
                    byte,
                    b'"' | b'\'' | b'<' | b'>' | b')' | b']' | b'}' | b',' | b';'
                )
            {
                break;
            }
            url_end += 1;
        }
        output.push_str(&line[cursor..scheme_start]);
        output.push_str("[redacted-url]");
        cursor = url_end;
    }
    output
}

fn scrub_absolute_paths(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut output = String::with_capacity(line.len());
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        if bytes[cursor] == b'/' && is_path_start(bytes, cursor) {
            let mut end = cursor;
            while end < bytes.len() && !is_path_end(bytes[end]) {
                end += 1;
            }
            while end > cursor && matches!(bytes[end - 1], b'.' | b':' | b'!' | b'?') {
                end -= 1;
            }
            output.push_str(&path_replacement(&line[cursor..end]));
            cursor = end;
        } else {
            let Some(character) = line[cursor..].chars().next() else {
                break;
            };
            output.push(character);
            cursor += character.len_utf8();
        }
    }
    output
}

fn is_path_start(bytes: &[u8], index: usize) -> bool {
    if index == 0 {
        return true;
    }
    matches!(
        bytes[index - 1],
        b' ' | b'\t'
            | b'\r'
            | b'\n'
            | b'"'
            | b'\''
            | b'='
            | b'('
            | b'['
            | b'{'
            | b','
            | b':'
            | b';'
            | b'<'
            | b'>'
            | b'|'
    )
}

fn is_path_end(byte: u8) -> bool {
    byte.is_ascii_whitespace()
        || matches!(
            byte,
            b'"' | b'\'' | b')' | b']' | b'}' | b',' | b';' | b'|' | b'<' | b'>'
        )
}

fn path_replacement(span: &str) -> String {
    let parts: Vec<&str> = span.split('/').filter(|part| !part.is_empty()).collect();
    let Some(base) = parts.last() else {
        return "[path]".to_string();
    };
    if span == "/Users" || span.starts_with("/Users/") {
        if parts.len() <= 2 {
            "~".to_string()
        } else {
            format!("~/{base}")
        }
    } else {
        format!(".../{base}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::CustosRole;
    use std::os::unix::fs::PermissionsExt as TestPermissionsExt;
    use zip::ZipArchive;

    const SECRET_VALUE: &str = "SUPERSECRET-123";
    const DAEMON_TOKEN: &str = "SECRET-DAEMON-TOKEN";
    const PROMPT_MARKER: &str = "PROMPT-MARKER-9f3a";
    const LOG_TOKEN: &str = "DO-NOT-LEAK";

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::TempDir::new().unwrap();
        let data_dir = root.path().to_path_buf();

        let connection = Connection::open(probe::database_path(&data_dir)).unwrap();
        connection
            .execute_batch(
                "
                PRAGMA user_version = 1;
                CREATE TABLE sessions(status TEXT);
                CREATE TABLE events(type TEXT, created_at TEXT);
                CREATE TABLE runtime_leases(released_at TEXT, expires_at TEXT);
                CREATE TABLE session_runs(status TEXT);
                CREATE TABLE plan_tasks(status TEXT);
                CREATE TABLE merge_queue(status TEXT);
                CREATE TABLE effect_receipts(state TEXT);
                CREATE TABLE approvals(status TEXT);
                CREATE TABLE supervised_processes(state TEXT);
                INSERT INTO sessions VALUES ('running'), ('paused');
                INSERT INTO events VALUES ('agent.message', datetime('now'));
                INSERT INTO events VALUES ('agent.message', datetime('now'));
                INSERT INTO events VALUES ('tool.finished', datetime('now'));
                INSERT INTO runtime_leases VALUES (NULL, datetime('now', '+10 minutes'));
                INSERT INTO runtime_leases VALUES (NULL, datetime('now', '-10 minutes'));
                INSERT INTO session_runs VALUES ('running'), ('interrupted');
                INSERT INTO plan_tasks VALUES ('pending'), ('pending'), ('running'), ('blocked');
                INSERT INTO merge_queue VALUES ('queued'), ('conflict');
                INSERT INTO effect_receipts VALUES ('unknown'), ('done');
                INSERT INTO approvals VALUES ('pending');
                INSERT INTO supervised_processes VALUES ('running'), ('exited');
                ",
            )
            .unwrap();
        drop(connection);

        let logs = data_dir.join("custos").join("logs");
        fs::create_dir_all(&logs).unwrap();
        fs::write(
            logs.join("custosd.out.log"),
            format!(
                "custosd core ready\n\
                 api_key={SECRET_VALUE}\n\
                 prompt: {PROMPT_MARKER}\n\
                 opened {}/verlinpark.db\n\
                 worktree /Users/alice/Private/notes.txt\n",
                data_dir.display()
            ),
        )
        .unwrap();
        fs::write(
            logs.join("custosd.err.log"),
            format!("error reading /private/var/tmp/scratch.log\ntoken={LOG_TOKEN}\n"),
        )
        .unwrap();

        let run = data_dir.join("run");
        fs::create_dir_all(&run).unwrap();
        fs::write(
            run.join("daemon.json"),
            format!(
                r#"{{"pid":42,"port":1234,"token":"{DAEMON_TOKEN}","protocol":1,"version":"0.1.0-test","started_at":"2026-11-26T00:00:00Z"}}"#
            ),
        )
        .unwrap();

        let heartbeat = ring::Heartbeat::new(
            CustosRole::Core,
            42,
            "2026-11-26T00:00:00Z".to_string(),
            "2026-11-26T00:00:00Z".to_string(),
            1,
            "sha256:dev".to_string(),
            charter::hash(),
            true,
        );
        ring::write_heartbeat(&data_dir, &heartbeat).unwrap();

        let worktree = data_dir
            .join("worktrees")
            .join("sample-project")
            .join("session-a")
            .join("leader");
        fs::create_dir_all(&worktree).unwrap();

        (root, data_dir)
    }

    fn output_dir(root: &tempfile::TempDir) -> PathBuf {
        root.path().join("VerlinPark Diagnostics")
    }

    fn read_zip(path: &Path) -> BTreeMap<String, Vec<u8>> {
        let file = File::open(path).unwrap();
        let mut archive = ZipArchive::new(file).unwrap();
        let mut files = BTreeMap::new();
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).unwrap();
            let name = entry.name().to_string();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            files.insert(name, bytes);
        }
        files
    }

    fn manifest(files: &BTreeMap<String, Vec<u8>>) -> Value {
        serde_json::from_slice(files.get("manifest.json").unwrap()).unwrap()
    }

    fn verify_manifest_hashes(files: &BTreeMap<String, Vec<u8>>) -> Value {
        let value = manifest(files);
        let entries = value["files"].as_array().unwrap();
        assert!(!entries.is_empty());
        for entry in entries {
            let path = entry["path"].as_str().unwrap();
            assert_ne!(path, "manifest.json");
            let bytes = files.get(path).unwrap();
            assert_eq!(entry["bytes"].as_u64().unwrap(), bytes.len() as u64);
            assert_eq!(entry["sha256"].as_str().unwrap(), sha256_hex(bytes));
        }
        assert!(value["chain_head"].as_str().unwrap().starts_with("sha256:"));
        value
    }

    fn all_text(files: &BTreeMap<String, Vec<u8>>) -> String {
        files
            .values()
            .map(|bytes| String::from_utf8_lossy(bytes))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn both_bundles_have_manifest_hashes_and_private_permissions() {
        let (_root, data_dir) = fixture();
        let output_root = tempfile::TempDir::new().unwrap();
        let out = output_dir(&output_root);

        let result = generate_to(&data_dir, &out, BundleMode::Both).unwrap();
        let standard_path = result.standard_path.clone().unwrap();
        let forensic_path = result.forensic_path.clone().unwrap();

        assert!(result.report_id.starts_with("cdr_"));
        assert_eq!(result.directory, out);
        assert!(standard_path.is_file());
        assert!(forensic_path.is_file());
        assert_eq!(
            result.bytes.get("standard").copied(),
            Some(fs::metadata(&standard_path).unwrap().len())
        );
        assert_eq!(
            result.bytes.get("forensic").copied(),
            Some(fs::metadata(&forensic_path).unwrap().len())
        );
        assert!(
            standard_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("verlinpark-diagnostics-standard-")
        );
        assert!(
            forensic_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("verlinpark-diagnostics-forensic-")
        );

        assert_eq!(
            fs::metadata(&out).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&standard_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&forensic_path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let standard = read_zip(&standard_path);
        let standard_manifest = verify_manifest_hashes(&standard);
        assert_eq!(standard_manifest["schema"], MANIFEST_SCHEMA);
        assert_eq!(standard_manifest["bundle_kind"], "standard");
        assert_eq!(standard_manifest["report_id"], result.report_id);
        assert_eq!(standard_manifest["redaction_profile"], "standard");
        assert!(
            standard_manifest["exclusions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == "prompts")
        );
        assert!(
            standard_manifest["exclusions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == "full_absolute_paths")
        );

        let forensic = read_zip(&forensic_path);
        let forensic_manifest = verify_manifest_hashes(&forensic);
        assert_eq!(forensic_manifest["bundle_kind"], "forensic");
        assert_eq!(forensic_manifest["redaction_profile"], "forensic");
        assert_eq!(forensic_manifest["report_id"], result.report_id);
    }

    #[test]
    fn standard_bundle_excludes_forensic_only_files() {
        let (_root, data_dir) = fixture();
        let output_root = tempfile::TempDir::new().unwrap();
        let result =
            generate_to(&data_dir, &output_dir(&output_root), BundleMode::Standard).unwrap();

        assert!(result.forensic_path.is_none());
        assert_eq!(result.bytes.len(), 1);
        let standard = read_zip(result.standard_path.as_ref().unwrap());
        for required in [
            "manifest.json",
            "summary.json",
            "health.json",
            "db_stats.json",
            "queue.json",
            "leases.json",
            "runs.json",
            "worktrees.json",
            "backup.json",
            "config_redacted.json",
            "system.txt",
            "README.txt",
        ] {
            assert!(standard.contains_key(required), "missing {required}");
        }
        for forensic_only in [
            "table_counts.json",
            "event_metadata.json",
            "processes.json",
            "capacity.json",
            "log_tail.out.txt",
            "log_tail.err.txt",
            "ring.json",
        ] {
            assert!(
                !standard.contains_key(forensic_only),
                "standard must not contain {forensic_only}"
            );
        }
        let summary: Value = serde_json::from_slice(standard.get("summary.json").unwrap()).unwrap();
        assert_eq!(summary["counts"]["sessions_total"], 2);
        assert!(summary["state"].as_str().is_some());
        let health: Value = serde_json::from_slice(standard.get("health.json").unwrap()).unwrap();
        assert_eq!(health["db_open"], true);
        assert_eq!(health["errors"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn forensic_bundle_adds_capacity_ring_and_event_metadata() {
        let (_root, data_dir) = fixture();
        let output_root = tempfile::TempDir::new().unwrap();
        let result =
            generate_to(&data_dir, &output_dir(&output_root), BundleMode::Forensic).unwrap();

        assert!(result.standard_path.is_none());
        let forensic = read_zip(result.forensic_path.as_ref().unwrap());
        for name in [
            "capacity.json",
            "ring.json",
            "table_counts.json",
            "event_metadata.json",
            "processes.json",
            "log_tail.out.txt",
            "log_tail.err.txt",
        ] {
            assert!(forensic.contains_key(name), "missing {name}");
        }

        let table_counts: Value =
            serde_json::from_slice(forensic.get("table_counts.json").unwrap()).unwrap();
        assert_eq!(table_counts["tables"]["plan_tasks"]["pending"], 2);
        assert_eq!(table_counts["tables"]["plan_tasks"]["blocked"], 1);

        let events: Value =
            serde_json::from_slice(forensic.get("event_metadata.json").unwrap()).unwrap();
        assert_eq!(events["available"], true);
        assert_eq!(events["by_type"]["agent.message"], 2);
        assert_eq!(events["by_type"]["tool.finished"], 1);

        let processes: Value =
            serde_json::from_slice(forensic.get("processes.json").unwrap()).unwrap();
        assert_eq!(
            processes["known_processes"]["by_state"]["running"],
            Value::from(1)
        );
        assert_eq!(processes["known_processes"]["live"], Value::from(1));

        let capacity: Value =
            serde_json::from_slice(forensic.get("capacity.json").unwrap()).unwrap();
        assert!(capacity["machine"]["logical_cores"].as_u64().unwrap_or(0) >= 1);
        assert!(
            capacity["effective_caps"]["rss_hard_bytes"]
                .as_u64()
                .unwrap_or(0)
                > 0
        );

        let ring: Value = serde_json::from_slice(forensic.get("ring.json").unwrap()).unwrap();
        assert!(ring["snapshot"]["fresh"].as_u64().unwrap_or(0) >= 1);
        assert!(
            ring["heartbeats"]
                .as_array()
                .unwrap()
                .iter()
                .any(|heartbeat| heartbeat["role"] == "core")
        );

        let worktrees: Value =
            serde_json::from_slice(forensic.get("worktrees.json").unwrap()).unwrap();
        assert_eq!(worktrees["available"], true);
        assert_eq!(worktrees["project_count"], 1);
        assert_eq!(worktrees["projects"][0]["project"], "sample-project");
    }

    #[test]
    fn bundles_are_private_and_scrubbed() {
        let (_root, data_dir) = fixture();
        let output_root = tempfile::TempDir::new().unwrap();
        let result = generate_to(&data_dir, &output_dir(&output_root), BundleMode::Both).unwrap();

        let standard_text = all_text(&read_zip(result.standard_path.as_ref().unwrap()));
        let forensic_text = all_text(&read_zip(result.forensic_path.as_ref().unwrap()));
        let data_dir_text = data_dir.display().to_string();

        for text in [&standard_text, &forensic_text] {
            assert!(!text.contains(&data_dir_text));
            assert!(!text.contains("/Users/"));
            assert!(!text.contains(SECRET_VALUE));
            assert!(!text.contains(DAEMON_TOKEN));
            assert!(!text.contains(PROMPT_MARKER));
            assert!(!text.contains(LOG_TOKEN));
        }
        assert!(!forensic_text.contains("alice"));
        assert!(forensic_text.contains("~/notes.txt"));
        assert!(forensic_text.contains("custosd core ready"));
        assert!(forensic_text.contains("scratch.log"));
        assert!(standard_text.contains("never uploaded"));
        assert!(standard_text.contains("provider API keys"));
    }

    #[test]
    fn standard_mode_writes_one_bundle() {
        let (_root, data_dir) = fixture();
        let output_root = tempfile::TempDir::new().unwrap();
        let out = output_dir(&output_root);
        let result = generate_to(&data_dir, &out, BundleMode::Standard).unwrap();

        let zips = fs::read_dir(&out)
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "zip")
            })
            .count();
        assert_eq!(zips, 1);
        assert_eq!(result.bytes.len(), 1);
        assert!(result.bytes.contains_key("standard"));
    }

    #[test]
    fn scrubber_replaces_home_urls_and_sensitive_lines() {
        let scrubbed = scrub_text(
            "open /Users/alice/Project/file.rs\n\
             fetch https://user:pass@example.com/path?q=1\n\
             api_key=hunter2\n\
             keep /opt/homebrew/bin/vp\n",
        );
        assert!(scrubbed.contains("~/file.rs"));
        assert!(scrubbed.contains("[redacted-url]"));
        assert!(scrubbed.contains("[redacted-line]"));
        assert!(scrubbed.contains(".../vp"));
        assert!(!scrubbed.contains("alice"));
        assert!(!scrubbed.contains("hunter2"));
        assert!(!scrubbed.contains("user:pass"));
    }

    #[test]
    fn output_dir_resolution_uses_override_without_writing() {
        let root = tempfile::TempDir::new().unwrap();
        let (resolved, origin) =
            resolve_output_dir(Some(root.path().as_os_str().to_os_string())).unwrap();
        assert_eq!(resolved, root.path());
        assert_eq!(origin, OutputOrigin::EnvironmentOverride);

        let (default, default_origin) = resolve_output_dir(None).unwrap();
        assert!(default.ends_with(DIAGNOSTICS_FOLDER_NAME));
        assert_eq!(default_origin, OutputOrigin::UserDownloads);
        assert!(!default.exists() || default.is_dir());
    }
}

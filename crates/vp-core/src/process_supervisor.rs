//! Durable process supervisor.
//!
//! Long-lived processes (dev servers, watchers, one-off slow builds) are owned
//! by the daemon rather than by one provider run. A `supervised_processes` row
//! records the immutable spawn description, the process identity evidence, the
//! lifecycle state, and a daemon-owned bounded log. Lifecycle transitions are
//! event-backed; heartbeat, last-access, and output byte/line counters are
//! operational observation state and are updated without events.
//!
//! Restart adoption is deliberately conservative. A row is adopted only when
//! the recorded pid is still alive, its `ps` start time matches, and the child
//! environment carries the exact per-spawn nonce and command hash written at
//! spawn time. Partial evidence moves the row to `unknown`; an absent pid or a
//! reused pid moves it to `lost`. We never adopt a PID on partial evidence and
//! never restart a command automatically.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use regex::Regex;
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;
use vp_protocol::types::{
    Event, ProcessLogsResult, ProcessStopResult, ProcessSupervisorStatus, SupervisedProcess,
};
use vp_store::Store;

use crate::tools::{ToolContext, safe_existing_path, sandboxed_command};

pub const PROCESS_STARTING_EVENT: &str = "process.starting";
pub const PROCESS_STARTED_EVENT: &str = "process.started";
pub const PROCESS_ADOPTED_EVENT: &str = "process.adopted";
pub const PROCESS_EXITED_EVENT: &str = "process.exited";
pub const PROCESS_LOST_EVENT: &str = "process.lost";
pub const PROCESS_UNKNOWN_EVENT: &str = "process.unknown";
pub const PROCESS_STOPPED_EVENT: &str = "process.stopped";
pub const PROCESS_PRUNED_EVENT: &str = "process.pruned";

/// Per-session active-process cap. It matches the historical per-worktree cap.
pub const MAX_PROCESSES_PER_SESSION: u32 = 8;
pub const MAX_COMMAND_BYTES: usize = 16_384;
pub const MAX_EXPLICIT_ENV_ENTRIES: usize = 32;
pub const MAX_ENV_VALUE_BYTES: usize = 4_096;
pub const MIN_OUTPUT_BYTES: u64 = 4_096;
pub const MAX_OUTPUT_BYTES: u64 = 4 * 1024 * 1024;
pub const MIN_OUTPUT_LINES: u64 = 100;
pub const MAX_OUTPUT_LINES: u64 = 50_000;
pub const MAX_LOG_PAGE_BYTES: usize = 1024 * 1024;
pub const MAX_LOG_PAGE_LINES: usize = 20_000;
pub const MAX_LOG_READ_BYTES: usize = 8 * 1024 * 1024;

const MAX_IDLE_TTL_SECS: u64 = 86_400;
const DEFAULT_RETENTION_SECS: u64 = 86_400;
const MONITOR_INTERVAL: Duration = Duration::from_millis(500);
const REATTACH_INTERVAL: Duration = Duration::from_millis(1_000);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(100);
const MAX_SIGNAL_NAME_CHARS: usize = 8;

const ENV_PROCESS_ID: &str = "VERLINPARK_PROCESS_ID";
const ENV_PROCESS_NONCE: &str = "VERLINPARK_PROCESS_NONCE";
const ENV_COMMAND_HASH: &str = "VERLINPARK_PROCESS_COMMAND_HASH";
const ENV_SUPERVISOR: &str = "VERLINPARK_SUPERVISED_PROCESS";

/// Immutable spawn request accepted by the supervisor. Field values are
/// validated before any row or file is created.
#[derive(Debug, Clone)]
pub struct StartSpec {
    pub command: String,
    pub name: Option<String>,
    pub cwd: Option<String>,
    pub env: BTreeMap<String, String>,
    pub idle_ttl_secs: u64,
    pub max_output_bytes: u64,
    pub max_output_lines: u64,
}

impl Default for StartSpec {
    fn default() -> Self {
        Self {
            command: String::new(),
            name: None,
            cwd: None,
            env: BTreeMap::new(),
            idle_ttl_secs: 3_600,
            max_output_bytes: 1_048_576,
            max_output_lines: 20_000,
        }
    }
}

/// Result of a bounded wait.
#[derive(Debug, Clone)]
pub struct ProcessWaitOutcome {
    pub process: SupervisedProcess,
    pub matched: bool,
    pub timed_out: bool,
    pub elapsed_ms: u64,
    pub evidence: String,
}

#[derive(Debug, Clone, Default)]
pub struct ReconcileReport {
    pub adopted: usize,
    pub lost: usize,
    pub unknown: usize,
    pub skipped_imported: usize,
    pub skipped_live: usize,
}

#[derive(Debug, Clone, Default)]
pub struct SweepReport {
    pub idle_stopped: usize,
    pub lost: usize,
    pub unknown: usize,
    pub pruned: usize,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
struct StopIntent {
    signal: i32,
}

#[derive(Debug, Clone)]
struct OutputTracker {
    offset: u64,
    lines: u64,
    total_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Evidence {
    Match,
    Absent,
    PidReused,
    Partial(String),
}

#[derive(Debug, Clone)]
struct ProcessIdentity {
    start_time: Option<String>,
    argv: Vec<String>,
    nonce: Option<String>,
    command_hash: Option<String>,
}

type ChildSlot = Arc<StdMutex<Option<tokio::process::Child>>>;

pub struct ProcessSupervisor {
    store: Store,
    instance: String,
    data_dir: PathBuf,
    retention: Duration,
    children: StdMutex<HashMap<String, ChildSlot>>,
    monitored: StdMutex<HashSet<String>>,
    stop_intents: StdMutex<HashMap<String, StopIntent>>,
    output_trackers: StdMutex<HashMap<String, OutputTracker>>,
}

impl std::fmt::Debug for ProcessSupervisor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProcessSupervisor")
            .field("instance", &self.instance)
            .field("data_dir", &self.data_dir)
            .finish_non_exhaustive()
    }
}

impl ProcessSupervisor {
    pub fn new(store: Store) -> Self {
        let retention_secs = std::env::var("VERLINPARK_PROCESS_RETENTION_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_RETENTION_SECS)
            .clamp(60, 30 * 86_400);
        let data_dir = store.data_dir().to_path_buf();
        if let Err(error) = vp_store::paths::ensure_private_dir(&data_dir.join("processes")) {
            tracing::warn!(%error, "failed to create supervised-process directory");
        }
        Self {
            instance: store.process_owner_instance(),
            store,
            data_dir,
            retention: Duration::from_secs(retention_secs),
            children: StdMutex::new(HashMap::new()),
            monitored: StdMutex::new(HashSet::new()),
            stop_intents: StdMutex::new(HashMap::new()),
            output_trackers: StdMutex::new(HashMap::new()),
        }
    }

    pub fn instance(&self) -> &str {
        &self.instance
    }

    pub fn retention(&self) -> Duration {
        self.retention
    }

    fn process_root(&self) -> PathBuf {
        self.data_dir.join("processes")
    }

    fn managed_output_path(&self, path: &str) -> Option<PathBuf> {
        if path.is_empty() {
            return None;
        }
        let base = self.process_root().canonicalize().ok()?;
        let candidate = PathBuf::from(path).canonicalize().ok()?;
        candidate.starts_with(&base).then_some(candidate)
    }

    /// Spawn a durable, sandboxed process and return its live row immediately.
    pub async fn start(
        self: &Arc<Self>,
        session_id: &str,
        context: &ToolContext,
        spec: &StartSpec,
    ) -> Result<SupervisedProcess> {
        let session = self
            .store
            .get_session(session_id)?
            .ok_or_else(|| anyhow!("session not found: {session_id}"))?;
        if session.status == "imported" {
            bail!("imported sessions are read-only archives");
        }
        if let Some(context_session) = context.session_id.as_deref()
            && context_session != session_id
        {
            bail!("process context belongs to a different session");
        }
        let policy = self.store.get_process_policy(&session.project_id)?;
        let mut effective = spec.clone();
        let (idle_ttl_secs, max_output_bytes, max_output_lines) = policy.clamp(
            spec.idle_ttl_secs,
            spec.max_output_bytes,
            spec.max_output_lines,
        );
        effective.idle_ttl_secs = idle_ttl_secs;
        effective.max_output_bytes = max_output_bytes;
        effective.max_output_lines = max_output_lines;
        validate_start_spec(&effective)?;
        let spec = &effective;
        if self.store.count_active_supervised_processes(session_id)? >= policy.max_processes {
            bail!(
                "process limit is {} per session; stop an existing process first",
                policy.max_processes
            );
        }
        let worktree = context
            .root
            .canonicalize()
            .with_context(|| format!("resolving worktree {}", context.root.display()))?;
        let cwd = match spec.cwd.as_deref() {
            Some(relative) => {
                let path = safe_existing_path(&worktree, relative)?;
                if !path.is_dir() {
                    bail!("cwd must be a directory: {relative}");
                }
                path
            }
            None => worktree.clone(),
        };
        let id = format!("psup_{}", &Uuid::now_v7().simple().to_string()[..12]);
        let nonce = Uuid::now_v7().simple().to_string();
        let started_at = vp_protocol::now_rfc3339();
        let process_dir = self.process_root().join(session_id).join(&id);
        vp_store::paths::ensure_private_dir(&process_dir)?;
        let output_path = process_dir.join("output.log");
        {
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&output_path)
                .context("creating supervised-process output file")?;
            vp_store::paths::ensure_private_file(&output_path)?;
            drop(file);
        }

        let name = spec
            .name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| {
                spec.command
                    .split_whitespace()
                    .next()
                    .unwrap_or("process")
                    .to_string()
            });
        if name.chars().count() > 200 {
            bail!("process name exceeds 200 characters");
        }

        let mut command = sandboxed_command(context).await?;
        command
            .stdin(Stdio::null())
            .current_dir(&cwd)
            .kill_on_drop(false);
        for (key, value) in &spec.env {
            command.env(key, value);
        }
        let env_hash = effective_env_hash(&command, &spec.env);
        let command_hash = spawn_command_hash(
            &spec.command,
            &cwd,
            &env_hash,
            context.network_allowed,
            context.restrict_home,
            context.read_jail,
            context.git_branch.as_deref(),
        );
        // The trailing `exit $?` is deliberate: it prevents the shell from
        // exec-optimizing the final command, so the supervised pid keeps the
        // reserved nonce/command-hash positional parameters for its lifetime.
        // macOS hides the environment of platform binaries from
        // KERN_PROCARGS2, so argv is the portable identity carrier; the same
        // values are also written to the environment for non-platform children.
        command
            .arg("/bin/sh")
            .arg("-c")
            .arg(format!("{}\nexit $?", spec.command))
            .arg("verlinpark-process")
            .arg(format!("--verlinpark-nonce={nonce}"))
            .arg(format!("--verlinpark-command-hash={command_hash}"));

        let mut starting = SupervisedProcess {
            id: id.clone(),
            session_id: session_id.to_string(),
            name,
            command: spec.command.clone(),
            argv: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                spec.command.clone(),
            ],
            cwd: cwd.to_string_lossy().to_string(),
            worktree: worktree.to_string_lossy().to_string(),
            env_hash,
            command_hash: command_hash.clone(),
            state: "starting".to_string(),
            pid: None,
            pgid: None,
            nonce: nonce.clone(),
            started_at: started_at.clone(),
            process_started_at: None,
            heartbeat_at: started_at.clone(),
            finished_at: None,
            exit_code: None,
            signal: None,
            output_path: output_path.to_string_lossy().to_string(),
            output_bytes: 0,
            output_lines: 0,
            total_output_bytes: 0,
            output_rotations: 0,
            max_output_bytes: spec.max_output_bytes,
            max_output_lines: spec.max_output_lines,
            owner_instance: self.instance.clone(),
            retention_deadline: None,
            idle_ttl_secs: spec.idle_ttl_secs,
            last_access_at: started_at.clone(),
            detail: None,
        };
        // Write the intent row before spawning so a crash cannot leave an
        // orphan process with no durable handle. It is reconciled to `lost` on
        // restart if the spawn never completed.
        self.store.insert_supervised_process_with_event(
            session_id,
            &starting,
            PROCESS_STARTING_EVENT,
        )?;

        let stdout_file = OpenOptions::new()
            .append(true)
            .open(&output_path)
            .context("opening supervised-process output file")?;
        let stderr_file = stdout_file
            .try_clone()
            .context("cloning supervised-process output file")?;
        command
            .stdout(Stdio::from(stdout_file))
            .stderr(Stdio::from(stderr_file))
            .env(ENV_SUPERVISOR, "1")
            .env(ENV_PROCESS_ID, &id)
            .env(ENV_PROCESS_NONCE, &nonce)
            .env(ENV_COMMAND_HASH, &command_hash);

        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let reason = format!("spawn failed: {error}");
                starting.state = "lost".to_string();
                starting.finished_at = Some(vp_protocol::now_rfc3339());
                starting.retention_deadline = Some(self.retention_deadline());
                starting.detail = Some(reason.clone());
                if let Err(update) = self
                    .store
                    .update_supervised_process_with_event(&starting, PROCESS_LOST_EVENT)
                {
                    tracing::warn!(%update, "failed to persist spawn failure");
                }
                let _ = std::fs::remove_dir_all(&process_dir);
                return Err(anyhow!("{reason}"));
            }
        };
        let pid = child
            .id()
            .map(i64::from)
            .ok_or_else(|| anyhow!("supervised process has no pid"))?;
        let identity = observe_process_identity(pid as i32);
        let mut running = starting.clone();
        running.state = "running".to_string();
        running.pid = Some(pid);
        running.pgid = Some(pid);
        running.process_started_at = identity.as_ref().and_then(|value| value.start_time.clone());
        running.heartbeat_at = vp_protocol::now_rfc3339();
        running.last_access_at = running.heartbeat_at.clone();
        if identity.is_none() {
            running.detail = Some("process identity was not observable at spawn".to_string());
        }
        self.store
            .update_supervised_process_with_event(&running, PROCESS_STARTED_EVENT)?;

        let slot: ChildSlot = Arc::new(StdMutex::new(Some(child)));
        if let Ok(mut children) = self.children.lock() {
            children.insert(id.clone(), slot.clone());
        }
        if let Ok(mut trackers) = self.output_trackers.lock() {
            trackers.insert(
                id.clone(),
                OutputTracker {
                    offset: 0,
                    lines: 0,
                    total_bytes: 0,
                },
            );
        }
        self.spawn_child_monitor(running.clone(), slot);
        Ok(running)
    }

    pub fn get(&self, session_id: &str, process_id: &str) -> Result<SupervisedProcess> {
        self.store
            .get_supervised_process(session_id, process_id)?
            .ok_or_else(|| anyhow!("unknown process id: {process_id}"))
    }

    pub fn list(
        &self,
        session_id: &str,
        include_finished: bool,
        limit: u32,
    ) -> Result<Vec<SupervisedProcess>> {
        self.store
            .list_supervised_processes(session_id, include_finished, limit)
    }

    pub fn active_count(&self, session_id: &str) -> Result<u32> {
        self.store.count_active_supervised_processes(session_id)
    }

    pub fn policy_for_session(&self, session_id: &str) -> vp_protocol::types::ProcessPolicy {
        self.store
            .get_session(session_id)
            .ok()
            .flatten()
            .and_then(|session| self.store.get_process_policy(&session.project_id).ok())
            .unwrap_or_default()
    }

    /// Send a signal to the process group without declaring a stop intent. The
    /// monitor records a natural exit as `exited`; use [`Self::stop`] when the
    /// operator intent is a stop.
    pub fn signal(&self, session_id: &str, process_id: &str, name: &str) -> Result<bool> {
        let row = self.get(session_id, process_id)?;
        if is_terminal(&row.state) {
            return Ok(false);
        }
        self.ensure_signal_evidence(&row)?;
        let signal = parse_signal(name)?;
        let pid = row.pgid.or(row.pid);
        Ok(kill_group(pid.map(|value| value as i32), signal))
    }

    fn child_owned(&self, process_id: &str) -> bool {
        self.children
            .lock()
            .map(|children| children.contains_key(process_id))
            .unwrap_or(false)
    }

    /// Explicit signals are only allowed on a process this daemon spawned or a
    /// process whose full restart identity still matches. Unknown/partial rows
    /// are never signaled.
    fn ensure_signal_evidence(&self, row: &SupervisedProcess) -> Result<()> {
        if self.child_owned(&row.id) {
            return Ok(());
        }
        match verify_row_identity(row) {
            Evidence::Match => Ok(()),
            Evidence::Absent => bail!(
                "process {} is already gone; its exit status is not observable",
                row.id
            ),
            Evidence::PidReused => bail!(
                "refusing to signal process {}: its pid was reused by another process",
                row.id
            ),
            Evidence::Partial(reason) => bail!(
                "refusing to signal process {}: identity evidence is incomplete ({reason})",
                row.id
            ),
        }
    }

    /// Mark operator/agent access for the idle TTL. This is telemetry only.
    pub fn touch(&self, row: &mut SupervisedProcess) {
        row.last_access_at = vp_protocol::now_rfc3339();
        if let Err(error) = self.store.record_supervised_process_telemetry(row) {
            tracing::debug!(%error, process_id = %row.id, "failed to record process access");
        }
    }

    pub fn read_logs(
        &self,
        session_id: &str,
        process_id: &str,
        offset: Option<u64>,
        max_bytes: Option<u32>,
        max_lines: Option<u32>,
        from_end: bool,
    ) -> Result<ProcessLogsResult> {
        let mut row = self.get(session_id, process_id)?;
        self.touch(&mut row);
        let max_bytes = max_bytes
            .map(|value| value as usize)
            .unwrap_or(65_536)
            .clamp(1, MAX_LOG_PAGE_BYTES);
        let max_lines = max_lines
            .map(|value| value as usize)
            .unwrap_or(MAX_LOG_PAGE_LINES)
            .clamp(1, MAX_LOG_PAGE_LINES);
        let mut bytes = Vec::new();
        let mut file_len = 0usize;
        if let Some(path) = self.managed_output_path(&row.output_path)
            && let Ok(mut file) = File::open(&path)
        {
            file_len = file.metadata().map(|meta| meta.len() as usize).unwrap_or(0);
            if file_len > MAX_LOG_READ_BYTES {
                let start = file_len - MAX_LOG_READ_BYTES;
                let _ = file.seek(SeekFrom::Start(start as u64));
                let mut bounded = Vec::new();
                file.take(MAX_LOG_READ_BYTES as u64)
                    .read_to_end(&mut bounded)?;
                bytes = bounded;
            } else {
                file.read_to_end(&mut bytes)?;
            }
        }
        if bytes.is_empty() {
            return Ok(ProcessLogsResult {
                session_id: session_id.to_string(),
                process_id: process_id.to_string(),
                state: row.state,
                content: String::new(),
                offset: 0,
                next_offset: 0,
                total_bytes: row.output_bytes,
                eof: true,
                truncated: false,
                rotated: row.output_rotations > 0,
            });
        }
        let start = if from_end {
            let requested = offset
                .map(|value| value as usize)
                .unwrap_or(max_bytes)
                .clamp(1, MAX_LOG_READ_BYTES);
            let candidate = bytes.len().saturating_sub(requested);
            let aligned = line_aligned_start(&bytes, candidate);
            if aligned < bytes.len() {
                aligned
            } else {
                candidate
            }
        } else {
            offset.unwrap_or(0).min(bytes.len() as u64) as usize
        };
        let (content, end, truncated) = take_log_page(&bytes[start..], max_bytes, max_lines);
        let next_offset = if from_end {
            bytes.len()
        } else {
            start.saturating_add(end)
        };
        Ok(ProcessLogsResult {
            session_id: session_id.to_string(),
            process_id: process_id.to_string(),
            state: row.state,
            content,
            offset: start as u64,
            next_offset: next_offset as u64,
            total_bytes: file_len as u64,
            eof: next_offset >= bytes.len(),
            truncated,
            rotated: row.output_rotations > 0,
        })
    }

    /// Explicit stop: SIGTERM to the process group, bounded wait, then SIGKILL.
    /// No command is ever restarted.
    pub async fn stop(
        &self,
        session_id: &str,
        process_id: &str,
        signal: Option<&str>,
        grace_secs: Option<u64>,
        reason: &str,
    ) -> Result<ProcessStopResult> {
        let session = self
            .store
            .get_session(session_id)?
            .ok_or_else(|| anyhow!("session not found: {session_id}"))?;
        if session.status == "imported" {
            bail!("imported sessions are read-only archives");
        }
        let mut row = self.get(session_id, process_id)?;
        if is_terminal(&row.state) {
            return Ok(ProcessStopResult {
                forced: row.signal == Some(libc::SIGKILL),
                process: row,
                waited_ms: 0,
            });
        }
        self.ensure_signal_evidence(&row)?;
        let signal = parse_signal(signal.unwrap_or("TERM"))?;
        let grace = Duration::from_secs(grace_secs.unwrap_or(5).clamp(1, 30));
        let pid = row
            .pgid
            .or(row.pid)
            .ok_or_else(|| anyhow!("process {process_id} has no pgid or pid"))?;
        self.set_stop_intent(process_id, signal);
        let started = Instant::now();
        let sent = kill_group(Some(pid as i32), signal);
        row.detail = Some(reason.to_string());
        let mut forced = signal == libc::SIGKILL;
        let mut terminal = wait_for_terminal(&self.store, session_id, process_id, grace).await?;
        if !terminal && !forced {
            self.set_stop_intent(process_id, libc::SIGKILL);
            kill_group(Some(pid as i32), libc::SIGKILL);
            forced = true;
            terminal =
                wait_for_terminal(&self.store, session_id, process_id, Duration::from_secs(2))
                    .await?;
        }
        let mut final_row = self.get(session_id, process_id)?;
        if !terminal {
            // The kill could not be confirmed. Do not pretend the process is
            // stopped; report the observed state and let the monitor resolve it.
            bail!(
                "process {process_id} did not stop within the bounded deadline; sent={sent} forced={forced} state={}",
                final_row.state
            );
        }
        if final_row.state == "stopped" {
            final_row.detail = Some(reason.to_string());
        }
        Ok(ProcessStopResult {
            forced,
            process: final_row,
            waited_ms: started.elapsed().as_millis() as u64,
        })
    }

    /// Bounded wait for exit or a regex log match.
    pub async fn wait(
        &self,
        session_id: &str,
        process_id: &str,
        timeout_secs: u64,
        pattern: Option<&Regex>,
    ) -> Result<ProcessWaitOutcome> {
        let timeout_secs = timeout_secs.clamp(1, 3_600);
        let started = Instant::now();
        let deadline = started + Duration::from_secs(timeout_secs);
        loop {
            let mut row = self.get(session_id, process_id)?;
            if let Some(pattern) = pattern {
                let tail = self.tail_text(&row, 256 * 1024);
                if pattern.is_match(&tail) {
                    self.touch(&mut row);
                    return Ok(ProcessWaitOutcome {
                        process: row,
                        matched: true,
                        timed_out: false,
                        elapsed_ms: started.elapsed().as_millis() as u64,
                        evidence: tail.chars().take(4_000).collect(),
                    });
                }
            }
            if is_terminal(&row.state) {
                self.touch(&mut row);
                let evidence = self.tail_text(&row, 4_000);
                return Ok(ProcessWaitOutcome {
                    process: row,
                    matched: false,
                    timed_out: false,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                    evidence,
                });
            }
            if Instant::now() >= deadline {
                self.touch(&mut row);
                return Ok(ProcessWaitOutcome {
                    process: row,
                    matched: false,
                    timed_out: true,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                    evidence: String::new(),
                });
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn tail_text(&self, row: &SupervisedProcess, max_bytes: usize) -> String {
        let Some(path) = self.managed_output_path(&row.output_path) else {
            return String::new();
        };
        let Ok(metadata) = std::fs::metadata(&path) else {
            return String::new();
        };
        let len = metadata.len();
        let start = len.saturating_sub(max_bytes as u64);
        let Ok(mut file) = File::open(&path) else {
            return String::new();
        };
        if file.seek(SeekFrom::Start(start)).is_err() {
            return String::new();
        }
        let mut bytes = Vec::new();
        if file.take(max_bytes as u64).read_to_end(&mut bytes).is_err() {
            return String::new();
        }
        String::from_utf8_lossy(&bytes).to_string()
    }

    /// Reconcile unfinished rows after a daemon restart. Adoption requires the
    /// full identity tuple; partial evidence is `unknown`, absent/reused is
    /// `lost`. Imported archive sessions are never touched.
    pub async fn reconcile(self: &Arc<Self>) -> Result<ReconcileReport> {
        let mut report = ReconcileReport::default();
        let rows = self.store.list_unfinished_supervised_processes()?;
        for mut row in rows {
            if self.is_monitored(&row.id) {
                report.skipped_live += 1;
                continue;
            }
            let session = match self.store.get_session(&row.session_id)? {
                Some(session) => session,
                None => {
                    self.finalize_unobserved(&mut row, "lost", "session no longer exists")?;
                    report.lost += 1;
                    continue;
                }
            };
            if session.status == "imported" {
                report.skipped_imported += 1;
                continue;
            }
            if row.pid.is_none() {
                self.finalize_unobserved(&mut row, "lost", "process never recorded a pid")?;
                report.lost += 1;
                continue;
            }
            match verify_row_identity(&row) {
                Evidence::Match => {
                    let identity = row.pid.and_then(|pid| observe_process_identity(pid as i32));
                    row.state = "running".to_string();
                    row.owner_instance = self.instance.clone();
                    row.process_started_at = identity
                        .as_ref()
                        .and_then(|value| value.start_time.clone())
                        .or_else(|| row.process_started_at.clone());
                    row.heartbeat_at = vp_protocol::now_rfc3339();
                    row.detail = Some("adopted after daemon restart".to_string());
                    self.store
                        .update_supervised_process_with_event(&row, PROCESS_ADOPTED_EVENT)?;
                    self.spawn_reattach_monitor(row.clone());
                    report.adopted += 1;
                }
                Evidence::Absent => {
                    self.finalize_unobserved(
                        &mut row,
                        "lost",
                        "process was already gone at daemon restart; exit status is not observable",
                    )?;
                    report.lost += 1;
                }
                Evidence::PidReused => {
                    self.finalize_unobserved(
                        &mut row,
                        "lost",
                        "pid was reused; refusing to adopt the new process",
                    )?;
                    report.lost += 1;
                }
                Evidence::Partial(reason) => {
                    row.state = "unknown".to_string();
                    row.owner_instance = self.instance.clone();
                    row.heartbeat_at = vp_protocol::now_rfc3339();
                    row.detail = Some(reason);
                    self.store
                        .update_supervised_process_with_event(&row, PROCESS_UNKNOWN_EVENT)?;
                    self.spawn_reattach_monitor(row.clone());
                    report.unknown += 1;
                }
            }
        }
        Ok(report)
    }

    /// Stop live processes idle beyond their per-row TTL and prune terminal rows
    /// whose retention deadline has passed. A process is never killed unless its
    /// full identity still matches.
    pub async fn sweep(self: &Arc<Self>, idle_enabled: bool) -> Result<SweepReport> {
        let mut report = SweepReport::default();
        let now = vp_protocol::now_rfc3339();
        if idle_enabled {
            let live = self.store.list_live_supervised_processes()?;
            for mut row in live {
                let Some(idle_secs) = seconds_since(&row.last_access_at) else {
                    continue;
                };
                if idle_secs < row.idle_ttl_secs {
                    continue;
                }
                let policy = self.policy_for_session(&row.session_id);
                if row.state == "unknown" || row.state == "starting" {
                    self.finalize_unobserved(
                        &mut row,
                        "lost",
                        "unverified process idled out; no signal was sent",
                    )?;
                    report.lost += 1;
                    continue;
                }
                if !policy.idle_sweep_enabled {
                    continue;
                }
                match verify_row_identity(&row) {
                    Evidence::Match => {
                        match self
                            .stop(
                                &row.session_id,
                                &row.id,
                                Some("TERM"),
                                Some(5),
                                "idle sweep",
                            )
                            .await
                        {
                            Ok(_) => report.idle_stopped += 1,
                            Err(error) => report.errors.push(format!("{}: {error}", row.id)),
                        }
                    }
                    Evidence::Absent => {
                        self.finalize_unobserved(
                            &mut row,
                            "lost",
                            "process disappeared before the idle sweep",
                        )?;
                        report.lost += 1;
                    }
                    Evidence::PidReused => {
                        self.finalize_unobserved(
                            &mut row,
                            "lost",
                            "pid was reused before the idle sweep",
                        )?;
                        report.lost += 1;
                    }
                    Evidence::Partial(reason) => {
                        row.state = "unknown".to_string();
                        row.heartbeat_at = now.clone();
                        row.detail = Some(reason);
                        self.store
                            .update_supervised_process_with_event(&row, PROCESS_UNKNOWN_EVENT)?;
                        report.unknown += 1;
                    }
                }
            }
        }
        for row in self.store.list_expired_supervised_processes(&now)? {
            if let Some(path) = self.managed_output_path(&row.output_path)
                && let Some(parent) = path.parent()
            {
                let _ = std::fs::remove_dir_all(parent);
            }
            match self.store.prune_supervised_process_with_event(
                &row.session_id,
                &row.id,
                "retention",
            ) {
                Ok(Some(_)) => {
                    self.forget_stop_intent(&row.id);
                    report.pruned += 1;
                }
                Ok(None) => {}
                Err(error) => report.errors.push(format!("{}: {error}", row.id)),
            }
        }
        Ok(report)
    }

    /// Stop every live process owned by a session. Used before discarding a
    /// session or removing its worktree.
    pub async fn stop_session(&self, session_id: &str, reason: &str) -> usize {
        let rows = self
            .store
            .list_supervised_processes(session_id, false, 200)
            .unwrap_or_default();
        let mut stopped = 0;
        for row in rows {
            if self
                .stop(session_id, &row.id, Some("TERM"), Some(2), reason)
                .await
                .is_ok()
            {
                stopped += 1;
            } else {
                tracing::warn!(
                    process_id = %row.id,
                    "leaving a process running because its identity could not be verified"
                );
            }
        }
        stopped
    }

    pub fn status(&self) -> Result<ProcessSupervisorStatus> {
        let counts = self.store.supervised_process_counts()?;
        let count = |state: &str| {
            counts
                .iter()
                .find(|entry| entry.key == state)
                .map(|entry| entry.count as u32)
                .unwrap_or(0)
        };
        Ok(ProcessSupervisorStatus {
            instance: self.instance.clone(),
            running: count("running"),
            starting: count("starting"),
            unknown: count("unknown"),
            exited: count("exited"),
            stopped: count("stopped"),
            lost: count("lost"),
            output_bytes: self.store.supervised_process_output_bytes()?,
        })
    }

    /// All unfinished rows for diagnostics. Used by replay tests and the daemon
    /// startup log; never includes output bodies.
    pub fn unfinished(&self) -> Result<Vec<SupervisedProcess>> {
        self.store.list_unfinished_supervised_processes()
    }

    fn retention_deadline(&self) -> String {
        let finished = time::OffsetDateTime::now_utc()
            + time::Duration::seconds(self.retention.as_secs().min(i64::MAX as u64) as i64);
        finished
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| vp_protocol::now_rfc3339())
    }

    fn set_stop_intent(&self, process_id: &str, signal: i32) {
        if let Ok(mut intents) = self.stop_intents.lock() {
            intents.insert(process_id.to_string(), StopIntent { signal });
        }
    }

    fn stop_signal(&self, process_id: &str) -> Option<i32> {
        self.stop_intents
            .lock()
            .ok()
            .and_then(|intents| intents.get(process_id).map(|intent| intent.signal))
    }

    fn is_monitored(&self, process_id: &str) -> bool {
        self.monitored
            .lock()
            .map(|monitored| monitored.contains(process_id))
            .unwrap_or(false)
    }

    fn mark_monitored(&self, process_id: &str) {
        if let Ok(mut monitored) = self.monitored.lock() {
            monitored.insert(process_id.to_string());
        }
    }

    fn unmark_monitored(&self, process_id: &str) {
        if let Ok(mut monitored) = self.monitored.lock() {
            monitored.remove(process_id);
        }
        if let Ok(mut children) = self.children.lock() {
            children.remove(process_id);
        }
        if let Ok(mut trackers) = self.output_trackers.lock() {
            trackers.remove(process_id);
        }
    }

    fn forget_stop_intent(&self, process_id: &str) {
        if let Ok(mut intents) = self.stop_intents.lock() {
            intents.remove(process_id);
        }
    }

    fn spawn_child_monitor(self: &Arc<Self>, mut row: SupervisedProcess, slot: ChildSlot) {
        self.mark_monitored(&row.id);
        let supervisor = Arc::clone(self);
        tokio::spawn(async move {
            let mut last_heartbeat = Instant::now();
            let mut last_rotation_check = Instant::now();
            loop {
                tokio::time::sleep(MONITOR_INTERVAL).await;
                let status = {
                    let Ok(mut guard) = slot.lock() else {
                        break;
                    };
                    let Some(child) = guard.as_mut() else {
                        break;
                    };
                    match child.try_wait() {
                        Ok(status) => status,
                        Err(_) => break,
                    }
                };
                if let Some(status) = status {
                    let intent = supervisor.stop_signal(&row.id);
                    supervisor.refresh_output(&mut row, last_rotation_check.elapsed());
                    let now = vp_protocol::now_rfc3339();
                    row.heartbeat_at = now.clone();
                    row.finished_at = Some(now.clone());
                    row.retention_deadline = Some(supervisor.retention_deadline());
                    row.exit_code = status.code();
                    row.signal = intent.or_else(|| status.signal());
                    row.state = if intent.is_some() {
                        "stopped".to_string()
                    } else {
                        "exited".to_string()
                    };
                    if let Some(intent) = intent {
                        row.detail = Some(format!("stopped by signal {intent}"));
                    }
                    let kind = if row.state == "stopped" {
                        PROCESS_STOPPED_EVENT
                    } else {
                        PROCESS_EXITED_EVENT
                    };
                    if let Err(error) = supervisor
                        .store
                        .update_supervised_process_with_event(&row, kind)
                    {
                        tracing::warn!(%error, process_id = %row.id, "failed to persist process exit");
                    }
                    supervisor.forget_stop_intent(&row.id);
                    break;
                }
                supervisor.refresh_output(&mut row, last_rotation_check.elapsed());
                last_rotation_check = Instant::now();
                if last_heartbeat.elapsed() >= HEARTBEAT_INTERVAL {
                    last_heartbeat = Instant::now();
                    row.heartbeat_at = vp_protocol::now_rfc3339();
                    if let Err(error) = supervisor.store.record_supervised_process_telemetry(&row) {
                        tracing::debug!(%error, process_id = %row.id, "process heartbeat failed");
                    }
                }
            }
            supervisor.unmark_monitored(&row.id);
        });
    }

    fn spawn_reattach_monitor(self: &Arc<Self>, mut row: SupervisedProcess) {
        self.mark_monitored(&row.id);
        let supervisor = Arc::clone(self);
        tokio::spawn(async move {
            let mut last_heartbeat = Instant::now();
            loop {
                tokio::time::sleep(REATTACH_INTERVAL).await;
                let current = match supervisor
                    .store
                    .get_supervised_process(&row.session_id, &row.id)
                {
                    Ok(Some(current)) => current,
                    _ => break,
                };
                if is_terminal(&current.state) {
                    break;
                }
                row = current;
                match verify_row_identity(&row) {
                    Evidence::Match => {
                        if row.state != "running" {
                            row.state = "running".to_string();
                            row.owner_instance = supervisor.instance.clone();
                            row.detail = Some("adopted after daemon restart".to_string());
                            if let Err(error) = supervisor
                                .store
                                .update_supervised_process_with_event(&row, PROCESS_ADOPTED_EVENT)
                            {
                                tracing::warn!(%error, process_id = %row.id, "failed to adopt process");
                            }
                        }
                        supervisor.refresh_output(&mut row, REATTACH_INTERVAL);
                        if last_heartbeat.elapsed() >= HEARTBEAT_INTERVAL {
                            last_heartbeat = Instant::now();
                            row.heartbeat_at = vp_protocol::now_rfc3339();
                            if let Err(error) =
                                supervisor.store.record_supervised_process_telemetry(&row)
                            {
                                tracing::debug!(%error, process_id = %row.id, "process heartbeat failed");
                            }
                        }
                    }
                    Evidence::Absent => {
                        let intent = supervisor.stop_signal(&row.id);
                        let now = vp_protocol::now_rfc3339();
                        row.heartbeat_at = now.clone();
                        row.finished_at = Some(now);
                        row.retention_deadline = Some(supervisor.retention_deadline());
                        if let Some(signal) = intent {
                            row.state = "stopped".to_string();
                            row.signal = Some(signal);
                            row.detail = Some(format!("stopped by signal {signal}"));
                            let _ = supervisor
                                .store
                                .update_supervised_process_with_event(&row, PROCESS_STOPPED_EVENT);
                        } else {
                            row.state = "lost".to_string();
                            row.detail = Some(
                                "process disappeared while adopted; exit status is not observable"
                                    .to_string(),
                            );
                            let _ = supervisor
                                .store
                                .update_supervised_process_with_event(&row, PROCESS_LOST_EVENT);
                        }
                        break;
                    }
                    Evidence::PidReused => {
                        let now = vp_protocol::now_rfc3339();
                        row.state = "lost".to_string();
                        row.heartbeat_at = now.clone();
                        row.finished_at = Some(now);
                        row.retention_deadline = Some(supervisor.retention_deadline());
                        row.detail =
                            Some("pid was reused; refusing to signal the new process".to_string());
                        let _ = supervisor
                            .store
                            .update_supervised_process_with_event(&row, PROCESS_LOST_EVENT);
                        break;
                    }
                    Evidence::Partial(reason) => {
                        if row.state != "unknown" {
                            row.state = "unknown".to_string();
                            row.heartbeat_at = vp_protocol::now_rfc3339();
                            row.detail = Some(reason);
                            let _ = supervisor
                                .store
                                .update_supervised_process_with_event(&row, PROCESS_UNKNOWN_EVENT);
                        } else if last_heartbeat.elapsed() >= HEARTBEAT_INTERVAL {
                            last_heartbeat = Instant::now();
                            row.heartbeat_at = vp_protocol::now_rfc3339();
                            if let Err(error) =
                                supervisor.store.record_supervised_process_telemetry(&row)
                            {
                                tracing::debug!(%error, process_id = %row.id, "unknown process heartbeat failed");
                            }
                        }
                    }
                }
            }
            supervisor.unmark_monitored(&row.id);
        });
    }

    fn finalize_unobserved(
        &self,
        row: &mut SupervisedProcess,
        state: &str,
        reason: &str,
    ) -> Result<()> {
        let now = vp_protocol::now_rfc3339();
        row.state = state.to_string();
        row.heartbeat_at = now.clone();
        row.finished_at = Some(now);
        row.retention_deadline = Some(self.retention_deadline());
        row.detail = Some(reason.to_string());
        let kind = match state {
            "unknown" => PROCESS_UNKNOWN_EVENT,
            "lost" => PROCESS_LOST_EVENT,
            _ => PROCESS_LOST_EVENT,
        };
        self.store.update_supervised_process_with_event(row, kind)?;
        Ok(())
    }

    /// Read newly appended bytes, count lines, and rotate down to the retained
    /// caps. Rotation rewrites the append-only file with its tail; the child's
    /// O_APPEND descriptor continues at the new end. This is best-effort and
    /// can lose a few bytes that race the rewrite.
    fn refresh_output(&self, row: &mut SupervisedProcess, _elapsed: Duration) {
        let Some(path) = self.managed_output_path(&row.output_path) else {
            return;
        };
        let Ok(metadata) = std::fs::metadata(&path) else {
            return;
        };
        let len = metadata.len();
        let mut tracker = self
            .output_trackers
            .lock()
            .ok()
            .and_then(|trackers| trackers.get(&row.id).cloned())
            .unwrap_or(OutputTracker {
                offset: len,
                lines: row.output_lines,
                total_bytes: row.total_output_bytes,
            });
        if len < tracker.offset {
            tracker.offset = 0;
            tracker.lines = 0;
        }
        if len > tracker.offset
            && let Ok(mut file) = File::open(&path)
            && file.seek(SeekFrom::Start(tracker.offset)).is_ok()
        {
            let mut read_total = 0usize;
            let mut buffer = [0u8; 8192];
            loop {
                match file.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        tracker.lines = tracker
                            .lines
                            .saturating_add(count_newlines(&buffer[..read]));
                        tracker.total_bytes = tracker.total_bytes.saturating_add(read as u64);
                        tracker.offset = tracker.offset.saturating_add(read as u64);
                        read_total += read;
                        if read_total >= MAX_LOG_READ_BYTES {
                            break;
                        }
                    }
                }
            }
        }
        row.output_bytes = len;
        row.output_lines = tracker.lines;
        row.total_output_bytes = tracker.total_bytes;
        if len > row.max_output_bytes || tracker.lines > row.max_output_lines {
            match rotate_log(&path, row.max_output_bytes, row.max_output_lines) {
                Ok((retained_bytes, retained_lines)) => {
                    row.output_bytes = retained_bytes;
                    row.output_lines = retained_lines;
                    row.output_rotations = row.output_rotations.saturating_add(1);
                    tracker.offset = retained_bytes;
                    tracker.lines = retained_lines;
                }
                Err(error) => {
                    tracing::debug!(%error, process_id = %row.id, "process output rotation failed");
                }
            }
        }
        if let Ok(mut trackers) = self.output_trackers.lock() {
            trackers.insert(row.id.clone(), tracker);
        }
    }
}

fn is_terminal(state: &str) -> bool {
    matches!(state, "exited" | "stopped" | "lost")
}

fn validate_start_spec(spec: &StartSpec) -> Result<()> {
    let command = spec.command.trim();
    if command.is_empty() {
        bail!("command is required");
    }
    if command.len() > MAX_COMMAND_BYTES {
        bail!("command exceeds the 16 KiB limit");
    }
    if command.chars().any(|ch| ch == '\0') {
        bail!("command must not contain NUL");
    }
    if spec.env.len() > MAX_EXPLICIT_ENV_ENTRIES {
        bail!("env accepts at most {MAX_EXPLICIT_ENV_ENTRIES} entries");
    }
    for (key, value) in &spec.env {
        if !valid_env_key(key) {
            bail!("invalid environment variable name: {key}");
        }
        if key.starts_with("VERLINPARK_") {
            bail!("environment variable {key} is reserved by the supervisor");
        }
        if value.len() > MAX_ENV_VALUE_BYTES || value.contains('\0') {
            bail!("environment variable {key} is too large or contains NUL");
        }
    }
    if spec.idle_ttl_secs == 0 || spec.idle_ttl_secs > MAX_IDLE_TTL_SECS {
        bail!("timeout_secs must be 1..{MAX_IDLE_TTL_SECS}");
    }
    if !(MIN_OUTPUT_BYTES..=MAX_OUTPUT_BYTES).contains(&spec.max_output_bytes) {
        bail!("max_output_bytes must be {MIN_OUTPUT_BYTES}..{MAX_OUTPUT_BYTES}");
    }
    if !(MIN_OUTPUT_LINES..=MAX_OUTPUT_LINES).contains(&spec.max_output_lines) {
        bail!("max_output_lines must be {MIN_OUTPUT_LINES}..{MAX_OUTPUT_LINES}");
    }
    Ok(())
}

fn valid_env_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 128
        && key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        && key
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
}

fn effective_env_hash(
    command: &tokio::process::Command,
    explicit: &BTreeMap<String, String>,
) -> String {
    let mut entries = BTreeMap::new();
    for (key, value) in command.as_std().get_envs() {
        let key = key.to_string_lossy().to_string();
        let value = value
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_default();
        entries.insert(key, value);
    }
    for (key, value) in explicit {
        entries.insert(key.clone(), value.clone());
    }
    let mut hasher = Sha256::new();
    for (key, value) in entries {
        hasher.update(key.as_bytes());
        hasher.update(b"=");
        hasher.update(value.as_bytes());
        hasher.update(b"\n");
    }
    format!("{:x}", hasher.finalize())
}

fn spawn_command_hash(
    command: &str,
    cwd: &Path,
    env_hash: &str,
    network_allowed: bool,
    restrict_home: bool,
    read_jail: bool,
    git_branch: Option<&str>,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"verlinpark-process-v1\n");
    hasher.update(command.as_bytes());
    hasher.update(b"\n");
    hasher.update(cwd.to_string_lossy().as_bytes());
    hasher.update(b"\n");
    hasher.update(env_hash.as_bytes());
    hasher.update(b"\n");
    hasher.update(if network_allowed {
        &b"network=allow"[..]
    } else {
        &b"network=deny"[..]
    });
    hasher.update(b"\n");
    hasher.update(if restrict_home {
        &b"home=jail"[..]
    } else {
        &b"home=open"[..]
    });
    hasher.update(b"\n");
    hasher.update(if read_jail {
        &b"read=jail"[..]
    } else {
        &b"read=open"[..]
    });
    hasher.update(b"\n");
    hasher.update(git_branch.unwrap_or("").as_bytes());
    format!("{:x}", hasher.finalize())
}

fn parse_signal(name: &str) -> Result<i32> {
    if name.chars().count() > MAX_SIGNAL_NAME_CHARS {
        bail!("unsupported signal: {name}");
    }
    match name.trim().to_ascii_uppercase().as_str() {
        "TERM" => Ok(libc::SIGTERM),
        "KILL" => Ok(libc::SIGKILL),
        "INT" => Ok(libc::SIGINT),
        "HUP" => Ok(libc::SIGHUP),
        other => bail!("unsupported signal: {other}"),
    }
}

fn kill_group(pid: Option<i32>, signal: i32) -> bool {
    let Some(pid) = pid else {
        return false;
    };
    #[cfg(unix)]
    unsafe {
        libc::kill(-pid, signal) == 0
    }
    #[cfg(not(unix))]
    {
        let _ = signal;
        false
    }
}

fn count_newlines(bytes: &[u8]) -> u64 {
    bytes.iter().filter(|byte| **byte == b'\n').count() as u64
}

fn seconds_since(timestamp: &str) -> Option<u64> {
    let parsed =
        time::OffsetDateTime::parse(timestamp, &time::format_description::well_known::Rfc3339)
            .ok()?;
    let now = time::OffsetDateTime::now_utc();
    let delta = now - parsed;
    u64::try_from(delta.whole_seconds().max(0)).ok()
}

async fn wait_for_terminal(
    store: &Store,
    session_id: &str,
    process_id: &str,
    deadline: Duration,
) -> Result<bool> {
    let started = Instant::now();
    loop {
        let Some(row) = store.get_supervised_process(session_id, process_id)? else {
            return Ok(true);
        };
        if is_terminal(&row.state) {
            return Ok(true);
        }
        if started.elapsed() >= deadline {
            return Ok(false);
        }
        tokio::time::sleep(STOP_POLL_INTERVAL).await;
    }
}

fn line_aligned_start(bytes: &[u8], candidate: usize) -> usize {
    if candidate == 0 || candidate >= bytes.len() {
        return candidate.min(bytes.len());
    }
    match bytes[candidate..].iter().position(|byte| *byte == b'\n') {
        Some(position) => candidate + position + 1,
        None => bytes.len(),
    }
}

fn take_log_page(bytes: &[u8], max_bytes: usize, max_lines: usize) -> (String, usize, bool) {
    let mut end = bytes.len().min(max_bytes);
    let mut lines = 0usize;
    for (index, byte) in bytes[..end].iter().enumerate() {
        if *byte == b'\n' {
            lines += 1;
            if lines >= max_lines {
                end = index + 1;
                break;
            }
        }
    }
    let truncated = end < bytes.len();
    (
        String::from_utf8_lossy(&bytes[..end]).to_string(),
        end,
        truncated,
    )
}

/// Keep the newest half-cap of bytes and lines. The file is rewritten in place
/// so the child's append descriptor continues writing at the new end.
fn rotate_log(path: &Path, max_bytes: u64, max_lines: u64) -> Result<(u64, u64)> {
    let metadata = std::fs::metadata(path)?;
    let len = metadata.len();
    let keep_bytes = (max_bytes / 2).max(MIN_OUTPUT_BYTES) as usize;
    let keep_lines = (max_lines / 2).max(MIN_OUTPUT_LINES).min(usize::MAX as u64) as usize;
    let start = len.saturating_sub(keep_bytes as u64);
    let mut data = Vec::new();
    {
        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(start))?;
        file.take((len - start).min(MAX_LOG_READ_BYTES as u64))
            .read_to_end(&mut data)?;
    }
    if start > 0
        && let Some(position) = data.iter().position(|byte| *byte == b'\n')
    {
        let without_partial = data.split_off(position + 1);
        if !without_partial.is_empty() {
            data = without_partial;
        }
    }
    // Bound retained lines even when the byte cap is generous.
    let newline_positions = data
        .iter()
        .enumerate()
        .filter(|(_, byte)| **byte == b'\n')
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if newline_positions.len() > keep_lines {
        let cut = newline_positions[newline_positions.len() - keep_lines] + 1;
        data.drain(..cut.min(data.len()));
    }
    let mut file = OpenOptions::new().write(true).truncate(true).open(path)?;
    file.write_all(&data)?;
    file.flush()?;
    let bytes = data.len() as u64;
    let lines = count_newlines(&data);
    Ok((bytes, lines))
}

fn process_alive(pid: i32) -> bool {
    #[cfg(unix)]
    unsafe {
        if libc::kill(pid, 0) == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

fn process_start_time(pid: i32) -> Option<String> {
    let output = std::process::Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!normalized.is_empty()).then_some(normalized)
}

fn observe_process_identity(pid: i32) -> Option<ProcessIdentity> {
    if !process_alive(pid) {
        return None;
    }
    let start_time = process_start_time(pid);
    let (argv, env) = process_argv_env(pid).unwrap_or_default();
    let nonce = env
        .get(ENV_PROCESS_NONCE)
        .cloned()
        .or_else(|| argv_marker(&argv, "--verlinpark-nonce="));
    let command_hash = env
        .get(ENV_COMMAND_HASH)
        .cloned()
        .or_else(|| argv_marker(&argv, "--verlinpark-command-hash="));
    Some(ProcessIdentity {
        start_time,
        argv,
        nonce,
        command_hash,
    })
}

fn argv_marker(argv: &[String], prefix: &str) -> Option<String> {
    argv.iter()
        .find_map(|arg| arg.strip_prefix(prefix).map(str::to_string))
}

/// Read the process argv and environment with `KERN_PROCARGS2`. Environment
/// values are never persisted; only the supervisor nonce and command hash are
/// compared.
#[cfg(target_os = "macos")]
fn process_argv_env(pid: i32) -> Option<(Vec<String>, HashMap<String, String>)> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size: libc::size_t = 0;
    // SAFETY: the sysctl MIB and size pointer are valid for the duration of the
    // call. A zero size query is the documented way to size the buffer.
    unsafe {
        if libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        ) != 0
        {
            return None;
        }
    }
    if !(4..=1024 * 1024).contains(&size) {
        return None;
    }
    let mut buffer = vec![0u8; size];
    let mut actual = size;
    // SAFETY: `buffer` is writable for `actual` bytes and the MIB is unchanged.
    unsafe {
        if libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buffer.as_mut_ptr() as *mut libc::c_void,
            &mut actual,
            std::ptr::null_mut(),
            0,
        ) != 0
        {
            return None;
        }
    }
    buffer.truncate(actual);
    if buffer.len() < 8 {
        return None;
    }
    let argc = i32::from_ne_bytes(buffer[0..4].try_into().ok()?) as usize;
    let rest = &buffer[4..];
    let mut index = rest.iter().position(|byte| *byte == 0)? + 1;
    while index < rest.len() && rest[index] == 0 {
        index += 1;
    }
    let mut strings = Vec::new();
    let mut current = Vec::new();
    for byte in &rest[index..] {
        if *byte == 0 {
            if !current.is_empty() {
                strings.push(String::from_utf8_lossy(&current).to_string());
                current.clear();
            }
        } else {
            current.push(*byte);
        }
    }
    if !current.is_empty() {
        strings.push(String::from_utf8_lossy(&current).to_string());
    }
    let argv = strings.iter().take(argc).cloned().collect::<Vec<_>>();
    let env = strings
        .iter()
        .skip(argc)
        .filter_map(|entry| {
            entry
                .split_once('=')
                .map(|(key, value)| (key.to_string(), value.to_string()))
        })
        .collect::<HashMap<_, _>>();
    Some((argv, env))
}

#[cfg(not(target_os = "macos"))]
fn process_argv_env(_pid: i32) -> Option<(Vec<String>, HashMap<String, String>)> {
    None
}

fn verify_row_identity(row: &SupervisedProcess) -> Evidence {
    let Some(pid) = row.pid else {
        return Evidence::Absent;
    };
    let Ok(pid) = i32::try_from(pid) else {
        return Evidence::Partial("pid is out of range".to_string());
    };
    if !process_alive(pid) {
        return Evidence::Absent;
    }
    let Some(identity) = observe_process_identity(pid) else {
        return Evidence::Partial("process identity is not observable".to_string());
    };
    let (Some(expected_start), Some(actual_start)) = (
        row.process_started_at.as_deref(),
        identity.start_time.as_deref(),
    ) else {
        return Evidence::Partial("process start time is unavailable".to_string());
    };
    if expected_start != actual_start {
        return Evidence::PidReused;
    }
    if identity.nonce.as_deref() != Some(row.nonce.as_str()) {
        return Evidence::Partial("process nonce is missing or mismatched".to_string());
    }
    if identity.command_hash.as_deref() != Some(row.command_hash.as_str()) {
        return Evidence::Partial("process command hash is missing or mismatched".to_string());
    }
    if identity.argv.is_empty() {
        return Evidence::Partial("process argv is unavailable".to_string());
    }
    Evidence::Match
}
/// Rebuild supervised-process lifecycle rows from `process.*` events. Returns
/// `None` when any lifecycle payload is malformed so repair fails closed instead
/// of silently deleting rows.
pub fn processes_from_events(events: &[Event]) -> Option<Vec<SupervisedProcess>> {
    let mut rows: BTreeMap<String, SupervisedProcess> = BTreeMap::new();
    for event in events {
        match event.kind.as_str() {
            PROCESS_STARTING_EVENT
            | PROCESS_STARTED_EVENT
            | PROCESS_ADOPTED_EVENT
            | PROCESS_EXITED_EVENT
            | PROCESS_LOST_EVENT
            | PROCESS_UNKNOWN_EVENT
            | PROCESS_STOPPED_EVENT => {
                let row =
                    serde_json::from_value::<SupervisedProcess>(event.payload.clone()).ok()?;
                rows.insert(row.id.clone(), row);
            }
            PROCESS_PRUNED_EVENT => {
                let id = event.payload.get("process_id")?.as_str()?;
                rows.remove(id);
            }
            _ => {}
        }
    }
    let mut rows = rows.into_values().collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        left.started_at
            .cmp(&right.started_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    Some(rows)
}

/// Compare replay-reconstructed lifecycle state with the live projection.
/// Heartbeat, last-access, and output counters are operational telemetry and
/// are deliberately excluded.
pub fn processes_match(projected: &[SupervisedProcess], expected: &[SupervisedProcess]) -> bool {
    if projected.len() != expected.len() {
        return false;
    }
    let mut projected = projected
        .iter()
        .map(|row| (&row.id, row))
        .collect::<HashMap<_, _>>();
    for expected in expected {
        let Some(projected) = projected.remove(&expected.id) else {
            return false;
        };
        if projected.session_id != expected.session_id
            || projected.name != expected.name
            || projected.command != expected.command
            || projected.argv != expected.argv
            || projected.cwd != expected.cwd
            || projected.worktree != expected.worktree
            || projected.env_hash != expected.env_hash
            || projected.command_hash != expected.command_hash
            || projected.state != expected.state
            || projected.pid != expected.pid
            || projected.pgid != expected.pgid
            || projected.nonce != expected.nonce
            || projected.started_at != expected.started_at
            || projected.process_started_at != expected.process_started_at
            || projected.finished_at != expected.finished_at
            || projected.exit_code != expected.exit_code
            || projected.signal != expected.signal
            || projected.output_path != expected.output_path
            || projected.max_output_bytes != expected.max_output_bytes
            || projected.max_output_lines != expected.max_output_lines
            || projected.owner_instance != expected.owner_instance
            || projected.retention_deadline != expected.retention_deadline
            || projected.idle_ttl_secs != expected.idle_ttl_secs
            || projected.detail != expected.detail
        {
            return false;
        }
    }
    projected.is_empty()
}

/// Convert a set of status counts into a compact diagnostic row.
pub fn status_line(status: &ProcessSupervisorStatus) -> String {
    format!(
        "processes: {} running, {} starting, {} unknown, {} exited, {} stopped, {} lost, {} bytes",
        status.running,
        status.starting,
        status.unknown,
        status.exited,
        status.stopped,
        status.lost,
        status.output_bytes
    )
}

/// Small deterministic helper used by the tool to report a bounded list.
pub fn process_ids(rows: &[SupervisedProcess]) -> BTreeSet<String> {
    rows.iter().map(|row| row.id.clone()).collect()
}

/// Exposed for tests and diagnostics: does the row still match its full
/// identity evidence?
pub fn identity_matches(row: &SupervisedProcess) -> bool {
    verify_row_identity(row) == Evidence::Match
}

/// Exposed for tests: classify a row without mutating it.
pub fn identity_state(row: &SupervisedProcess) -> &'static str {
    match verify_row_identity(row) {
        Evidence::Match => "match",
        Evidence::Absent => "absent",
        Evidence::PidReused => "pid_reused",
        Evidence::Partial(_) => "partial",
    }
}

/// Test/diagnostic helper: JSON view of a supervisor row without output bodies.
pub fn process_summary(row: &SupervisedProcess) -> serde_json::Value {
    json!({
        "process_id": row.id,
        "state": row.state,
        "pid": row.pid,
        "exit_code": row.exit_code,
        "signal": row.signal,
        "output_bytes": row.output_bytes,
        "output_lines": row.output_lines,
        "rotations": row.output_rotations,
        "owner_instance": row.owner_instance,
        "retention_deadline": row.retention_deadline,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::TempDir;
    use vp_store::Store;

    fn now_minus(seconds: i64) -> String {
        let value = time::OffsetDateTime::now_utc() - time::Duration::seconds(seconds);
        value
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap()
    }

    fn test_root() -> TempDir {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("home")).unwrap();
        dir
    }

    fn store_with_session(root: &Path) -> (TempDir, Store, String) {
        let data = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(root.to_string_lossy().as_ref(), None)
            .unwrap();
        let session = store
            .create_session(&project.id, "durable process supervisor test")
            .unwrap();
        (data, store, session.id)
    }

    fn context(root: &Path, data: &Path, store: &Store, session: &str) -> ToolContext {
        ToolContext {
            root: root.to_path_buf(),
            project_root: root.to_path_buf(),
            sandbox_root: data.join("sandbox").join(session),
            secret_roots: Vec::new(),
            network_allowed: true,
            home: root.join("home"),
            restrict_home: false,
            read_jail: false,
            git_branch: None,
            remote: None,
            session_id: Some(session.to_string()),
            process_supervisor: Some(Arc::new(ProcessSupervisor::new(store.clone()))),
        }
    }

    async fn start_sleep(
        supervisor: &Arc<ProcessSupervisor>,
        context: &ToolContext,
        session: &str,
        seconds: u64,
    ) -> SupervisedProcess {
        supervisor
            .start(
                session,
                context,
                &StartSpec {
                    command: format!("sleep {seconds}"),
                    name: Some("sleeper".to_string()),
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap()
    }

    fn fake_row(
        session: &str,
        root: &Path,
        id: &str,
        state: &str,
        pid: Option<i64>,
    ) -> SupervisedProcess {
        SupervisedProcess {
            id: id.to_string(),
            session_id: session.to_string(),
            name: "fake".to_string(),
            command: "sleep 30".to_string(),
            argv: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "sleep 30".to_string(),
            ],
            cwd: root.to_string_lossy().to_string(),
            worktree: root.to_string_lossy().to_string(),
            env_hash: "env".to_string(),
            command_hash: "command".to_string(),
            state: state.to_string(),
            pid,
            pgid: pid,
            nonce: "nonce".to_string(),
            started_at: now_minus(10),
            process_started_at: Some("Mon Jan 1 00:00:00 2001".to_string()),
            heartbeat_at: now_minus(1),
            finished_at: None,
            exit_code: None,
            signal: None,
            output_path: String::new(),
            output_bytes: 0,
            output_lines: 0,
            total_output_bytes: 0,
            output_rotations: 0,
            max_output_bytes: 1_048_576,
            max_output_lines: 20_000,
            owner_instance: "dead-instance:1".to_string(),
            retention_deadline: None,
            idle_ttl_secs: 3_600,
            last_access_at: now_minus(1),
            detail: None,
        }
    }

    #[tokio::test]
    async fn start_returns_immediately_and_records_sandbox_identity() {
        let dir = test_root();
        let (data, store, session) = store_with_session(dir.path());
        let supervisor = Arc::new(ProcessSupervisor::new(store.clone()));
        let context = context(dir.path(), data.path(), &store, &session);
        let started = Instant::now();
        let row = start_sleep(&supervisor, &context, &session, 30).await;
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "start blocked for {:?}",
            started.elapsed()
        );
        assert_eq!(row.state, "running");
        assert!(row.pid.unwrap() > 0);
        assert_eq!(row.pgid, row.pid);
        assert_eq!(
            row.worktree,
            dir.path().canonicalize().unwrap().to_string_lossy()
        );
        assert!(!row.env_hash.is_empty());
        assert!(!row.command_hash.is_empty());
        assert!(!row.output_path.is_empty());
        assert!(Path::new(&row.output_path).is_file());
        let events = store.list_all_events(&session).unwrap();
        assert!(
            events
                .iter()
                .any(|event| event.kind == PROCESS_STARTING_EVENT)
        );
        assert!(
            events
                .iter()
                .any(|event| event.kind == PROCESS_STARTED_EVENT)
        );
        let stopped = supervisor
            .stop(&session, &row.id, Some("TERM"), Some(2), "test cleanup")
            .await
            .unwrap();
        assert_eq!(stopped.process.state, "stopped");
        assert!(!stopped.forced);
    }

    #[tokio::test]
    async fn child_cannot_write_outside_worktree_and_env_is_allowlisted() {
        let dir = test_root();
        let (data, store, session) = store_with_session(dir.path());
        let supervisor = Arc::new(ProcessSupervisor::new(store.clone()));
        let context = context(dir.path(), data.path(), &store, &session);
        let outside = TempDir::new().unwrap();
        let outside_file = outside.path().join("escape.txt");
        let inside_file = dir.path().join("inside.txt");
        let mut env = BTreeMap::new();
        env.insert("EXPLICIT_VALUE".to_string(), "visible".to_string());
        let row = supervisor
            .start(
                &session,
                &context,
                &StartSpec {
                    command: format!(
                        "printf 'explicit=%s secret=%s\\n' \"$EXPLICIT_VALUE\" \"${{VERLINPARK_TEST_SECRET-unset}}\"; echo inside > {}; echo outside > {} 2>/dev/null || echo denied",
                        inside_file.display(),
                        outside_file.display()
                    ),
                    env,
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap();
        wait_until_terminal(&supervisor, &session, &row.id, 10).await;
        let log = supervisor
            .read_logs(&session, &row.id, None, None, None, true)
            .unwrap();
        assert!(log.content.contains("explicit=visible"));
        assert!(log.content.contains("secret=unset"));
        assert!(log.content.contains("denied"));
        assert!(inside_file.is_file());
        assert!(!outside_file.exists());
    }

    async fn wait_until_terminal(
        supervisor: &ProcessSupervisor,
        session: &str,
        id: &str,
        timeout_secs: u64,
    ) {
        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
        loop {
            let row = supervisor.get(session, id).unwrap();
            if is_terminal(&row.state) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "process did not exit: {:?}",
                row.state
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn rejects_reserved_env_and_invalid_limits() {
        let dir = test_root();
        let (data, store, session) = store_with_session(dir.path());
        let supervisor = Arc::new(ProcessSupervisor::new(store.clone()));
        let context = context(dir.path(), data.path(), &store, &session);
        let mut env = BTreeMap::new();
        env.insert("VERLINPARK_PROCESS_NONCE".to_string(), "spoof".to_string());
        let error = supervisor
            .start(
                &session,
                &context,
                &StartSpec {
                    command: "true".to_string(),
                    env,
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("reserved"));
        let error = supervisor
            .start(
                &session,
                &context,
                &StartSpec {
                    command: "true".to_string(),
                    max_output_bytes: 1,
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("max_output_bytes"));
    }

    #[tokio::test]
    async fn output_rotation_keeps_caps_and_pages_logs() {
        let dir = test_root();
        let (data, store, session) = store_with_session(dir.path());
        let supervisor = Arc::new(ProcessSupervisor::new(store.clone()));
        let context = context(dir.path(), data.path(), &store, &session);
        let row = supervisor
            .start(
                &session,
                &context,
                &StartSpec {
                    command: "i=0; while [ $i -lt 4000 ]; do echo line-$i-abcdefghijklmnopqrstuvwxyz; i=$((i+1)); done".to_string(),
                    max_output_bytes: 8_192,
                    max_output_lines: 500,
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap();
        wait_until_terminal(&supervisor, &session, &row.id, 15).await;
        // Allow the monitor to perform a final rotation pass.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let row = supervisor.get(&session, &row.id).unwrap();
        assert!(row.output_rotations >= 1, "row={row:?}");
        assert!(row.output_bytes <= row.max_output_bytes + 8_192);
        assert!(row.total_output_bytes > row.output_bytes);
        let page = supervisor
            .read_logs(&session, &row.id, None, Some(512), Some(5), false)
            .unwrap();
        assert!(page.content.len() <= 512);
        assert!(page.content.lines().count() <= 6);
        assert!(page.truncated);
        let tail = supervisor
            .read_logs(&session, &row.id, None, Some(256), Some(10), true)
            .unwrap();
        assert!(tail.content.len() <= 256);
        assert!(!tail.content.is_empty());
    }

    #[tokio::test]
    async fn stop_escalates_after_ignored_term() {
        let dir = test_root();
        let (data, store, session) = store_with_session(dir.path());
        let supervisor = Arc::new(ProcessSupervisor::new(store.clone()));
        let context = context(dir.path(), data.path(), &store, &session);
        let row = supervisor
            .start(
                &session,
                &context,
                &StartSpec {
                    command: "trap '' TERM; while :; do :; done".to_string(),
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap();
        let started = Instant::now();
        tokio::time::sleep(Duration::from_millis(500)).await;
        let result = supervisor
            .stop(&session, &row.id, Some("TERM"), Some(1), "test escalation")
            .await
            .unwrap();
        assert!(result.forced);
        assert_eq!(result.process.state, "stopped");
        assert_eq!(result.process.signal, Some(libc::SIGKILL));
        assert!(started.elapsed() < Duration::from_secs(6));
    }

    #[test]
    fn pid_reuse_and_partial_evidence_are_rejected() {
        let dir = test_root();
        let (_data, _store, session) = store_with_session(dir.path());
        let mut row = fake_row(&session, dir.path(), "psup_fake", "running", Some(1));
        // PID 1 is alive, but the recorded start time cannot match.
        assert_eq!(identity_state(&row), "pid_reused");
        assert!(!identity_matches(&row));
        // A live but unrelated process has no supervisor nonce.
        row.pid = Some(std::process::id() as i64);
        row.process_started_at = process_start_time(std::process::id() as i32);
        assert_eq!(identity_state(&row), "partial");
        assert!(!identity_matches(&row));
    }

    #[test]
    fn live_child_with_wrong_nonce_is_unknown_not_adopted() {
        let dir = test_root();
        let (_data, _store, session) = store_with_session(dir.path());
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "sleep 5"])
            .env(ENV_PROCESS_NONCE, "wrong-nonce")
            .env(ENV_COMMAND_HASH, "wrong-hash")
            .spawn()
            .unwrap();
        let pid = child.id() as i64;
        let mut row = fake_row(&session, dir.path(), "psup_live", "running", Some(pid));
        row.process_started_at = process_start_time(pid as i32);
        row.nonce = "expected-nonce".to_string();
        assert_eq!(identity_state(&row), "partial");
        let _ = child.kill();
        let _ = child.wait();
    }

    #[tokio::test]
    async fn restart_reconciles_lost_unknown_and_adopted_rows() {
        let dir = test_root();
        let (data, store, session) = store_with_session(dir.path());
        let supervisor = Arc::new(ProcessSupervisor::new(store.clone()));
        let context = context(dir.path(), data.path(), &store, &session);
        let live = start_sleep(&supervisor, &context, &session, 20).await;

        let dead = fake_row(&session, dir.path(), "psup_dead", "running", Some(999_999));
        store
            .insert_supervised_process_with_event(&session, &dead, PROCESS_STARTED_EVENT)
            .unwrap();
        let mut unknown_child = std::process::Command::new("/bin/sh")
            .args(["-c", "sleep 10"])
            .spawn()
            .unwrap();
        let unknown_pid = unknown_child.id() as i64;
        let mut unknown = fake_row(
            &session,
            dir.path(),
            "psup_unknown",
            "running",
            Some(unknown_pid),
        );
        unknown.process_started_at = process_start_time(unknown_pid as i32);
        store
            .insert_supervised_process_with_event(&session, &unknown, PROCESS_STARTED_EVENT)
            .unwrap();
        let never_spawned = fake_row(&session, dir.path(), "psup_starting", "starting", None);
        store
            .insert_supervised_process_with_event(&session, &never_spawned, PROCESS_STARTING_EVENT)
            .unwrap();

        let restarted = Arc::new(ProcessSupervisor::new(store.clone()));
        let report = restarted.reconcile().await.unwrap();
        assert!(report.adopted >= 1, "report={report:?}");
        assert!(report.lost >= 1, "report={report:?}");
        assert!(report.unknown >= 1, "report={report:?}");
        let adopted = restarted.get(&session, &live.id).unwrap();
        assert_eq!(adopted.state, "running");
        assert!(adopted.detail.as_deref().unwrap_or("").contains("adopted"));
        assert_eq!(restarted.get(&session, "psup_dead").unwrap().state, "lost");
        assert_eq!(
            restarted.get(&session, "psup_unknown").unwrap().state,
            "unknown"
        );
        assert_eq!(
            restarted.get(&session, "psup_starting").unwrap().state,
            "lost"
        );
        supervisor
            .stop(&session, &live.id, Some("KILL"), Some(1), "test cleanup")
            .await
            .unwrap();
        let _ = unknown_child.kill();
        let _ = unknown_child.wait();
    }

    #[tokio::test]
    async fn idle_sweep_stops_and_prunes_by_policy() {
        let dir = test_root();
        let (data, store, session) = store_with_session(dir.path());
        let supervisor = Arc::new(ProcessSupervisor::new(store.clone()));
        let context = context(dir.path(), data.path(), &store, &session);
        let row = supervisor
            .start(
                &session,
                &context,
                &StartSpec {
                    command: "sleep 30".to_string(),
                    idle_ttl_secs: 1,
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap();
        // Let the sandbox exec settle so the argv identity markers are visible
        // before the sweep re-verifies the process.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut stale = supervisor.get(&session, &row.id).unwrap();
        stale.last_access_at = now_minus(5);
        store.record_supervised_process_telemetry(&stale).unwrap();
        let report = supervisor.sweep(true).await.unwrap();
        assert!(report.idle_stopped >= 1, "report={report:?}");
        assert_eq!(supervisor.get(&session, &row.id).unwrap().state, "stopped");

        let mut expired = supervisor.get(&session, &row.id).unwrap();
        expired.retention_deadline = Some(now_minus(1));
        store
            .update_supervised_process_with_event(&expired, PROCESS_STOPPED_EVENT)
            .unwrap();
        let report = supervisor.sweep(false).await.unwrap();
        assert!(report.pruned >= 1, "report={report:?}");
        assert!(supervisor.get(&session, &row.id).is_err());
        let events = store.list_all_events(&session).unwrap();
        assert!(
            events
                .iter()
                .any(|event| event.kind == PROCESS_PRUNED_EVENT)
        );
    }

    #[tokio::test]
    async fn replay_and_repair_reconstruct_lifecycle_without_output_bodies() {
        let dir = test_root();
        let (data, store, session) = store_with_session(dir.path());
        let supervisor = Arc::new(ProcessSupervisor::new(store.clone()));
        let context = context(dir.path(), data.path(), &store, &session);
        let row = supervisor
            .start(
                &session,
                &context,
                &StartSpec {
                    command: "printf 'hello-%s\\n' replay".to_string(),
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap();
        wait_until_terminal(&supervisor, &session, &row.id, 10).await;
        let events = store.list_all_events(&session).unwrap();
        for event in events
            .iter()
            .filter(|event| event.kind.starts_with("process."))
        {
            assert!(
                !event.payload.to_string().contains("hello-replay"),
                "output body leaked into {}",
                event.kind
            );
        }
        let projected = store
            .list_supervised_processes(&session, true, 100)
            .unwrap();
        let replayed = processes_from_events(&events).unwrap();
        assert!(processes_match(&projected, &replayed));
        store
            .replace_supervised_processes(&session, &replayed)
            .unwrap();
        let restored = store
            .list_supervised_processes(&session, true, 100)
            .unwrap();
        assert!(processes_match(&restored, &replayed));
        assert_eq!(restored[0].state, "exited");
    }

    #[tokio::test]
    async fn cap_and_unknown_ids_are_enforced() {
        let dir = test_root();
        let (data, store, session) = store_with_session(dir.path());
        let supervisor = Arc::new(ProcessSupervisor::new(store.clone()));
        let context = context(dir.path(), data.path(), &store, &session);
        let mut rows = Vec::new();
        for index in 0..MAX_PROCESSES_PER_SESSION {
            rows.push(
                supervisor
                    .start(
                        &session,
                        &context,
                        &StartSpec {
                            command: format!("sleep 20; echo done-{index}"),
                            ..StartSpec::default()
                        },
                    )
                    .await
                    .unwrap(),
            );
        }
        let error = supervisor
            .start(
                &session,
                &context,
                &StartSpec {
                    command: "true".to_string(),
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("process limit"));
        assert!(supervisor.get(&session, "psup_missing").is_err());
        for row in rows {
            let _ = supervisor
                .stop(&session, &row.id, Some("KILL"), Some(1), "test cleanup")
                .await;
        }
    }

    #[tokio::test]
    async fn session_stop_stops_all_live_processes() {
        let dir = test_root();
        let (data, store, session) = store_with_session(dir.path());
        let supervisor = Arc::new(ProcessSupervisor::new(store.clone()));
        let context = context(dir.path(), data.path(), &store, &session);
        let first = start_sleep(&supervisor, &context, &session, 20).await;
        let second = start_sleep(&supervisor, &context, &session, 20).await;
        assert_eq!(supervisor.stop_session(&session, "test cleanup").await, 2);
        assert_eq!(
            supervisor.get(&session, &first.id).unwrap().state,
            "stopped"
        );
        assert_eq!(
            supervisor.get(&session, &second.id).unwrap().state,
            "stopped"
        );
    }
}

#[cfg(test)]
mod replay_tests {
    use super::*;
    use tempfile::TempDir;
    use vp_store::Store;

    #[tokio::test]
    async fn engine_replay_and_rebuild_include_supervised_processes() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir_all(root.path().join("home")).unwrap();
        let data = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(root.path().to_str().unwrap(), Some("replay"))
            .unwrap();
        let session = store.create_session(&project.id, "process replay").unwrap();
        store
            .append_event(
                &session.id,
                "session.created",
                serde_json::json!({"goal": "process replay"}),
            )
            .unwrap();
        let engine = crate::Engine::new(store.clone());
        let supervisor = Arc::clone(engine.process_supervisor());
        let context = ToolContext {
            root: root.path().to_path_buf(),
            project_root: root.path().to_path_buf(),
            sandbox_root: data.path().join("sandbox").join(&session.id),
            secret_roots: Vec::new(),
            network_allowed: true,
            home: root.path().join("home"),
            restrict_home: false,
            read_jail: false,
            git_branch: None,
            remote: None,
            session_id: Some(session.id.clone()),
            process_supervisor: Some(Arc::clone(&supervisor)),
        };
        let row = supervisor
            .start(
                &session.id,
                &context,
                &StartSpec {
                    command: "printf 'replay-evidence\\n'".to_string(),
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let current = supervisor.get(&session.id, &row.id).unwrap();
            if matches!(current.state.as_str(), "exited" | "stopped" | "lost") {
                break;
            }
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let report = engine.replay_session(&session.id).await.unwrap();
        assert!(report.processes_match, "mismatches={:?}", report.mismatches);
        assert!(report.consistent, "mismatches={:?}", report.mismatches);
        let rebuilt = engine.rebuild_session(&session.id).await.unwrap();
        assert!(
            rebuilt.processes_match,
            "mismatches={:?}",
            rebuilt.mismatches
        );
        let rows = store
            .list_supervised_processes(&session.id, true, 10)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, row.id);
        assert_eq!(rows[0].state, "exited");
    }
}

#[cfg(test)]
mod supervisor_edge_tests {
    use super::*;
    use tempfile::TempDir;
    use vp_store::Store;

    fn root() -> TempDir {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("home")).unwrap();
        dir
    }

    fn setup() -> (TempDir, TempDir, Store, String) {
        let worktree = root();
        let data = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(worktree.path().to_str().unwrap(), None)
            .unwrap();
        let session = store.create_session(&project.id, "edges").unwrap();
        (worktree, data, store, session.id)
    }

    fn context(worktree: &Path, data: &Path, store: &Store, session: &str) -> ToolContext {
        ToolContext {
            root: worktree.to_path_buf(),
            project_root: worktree.to_path_buf(),
            sandbox_root: data.join("sandbox").join(session),
            secret_roots: Vec::new(),
            network_allowed: true,
            home: worktree.join("home"),
            restrict_home: false,
            read_jail: false,
            git_branch: None,
            remote: None,
            session_id: Some(session.to_string()),
            process_supervisor: Some(Arc::new(ProcessSupervisor::new(store.clone()))),
        }
    }

    async fn wait_terminal(supervisor: &ProcessSupervisor, session: &str, id: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let row = supervisor.get(session, id).unwrap();
            if is_terminal(&row.state) {
                return;
            }
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn env_and_command_hashes_are_deterministic_and_sensitive() {
        let (worktree, data, store, session) = setup();
        let supervisor = Arc::new(ProcessSupervisor::new(store.clone()));
        let context = context(worktree.path(), data.path(), &store, &session);
        let mut env = BTreeMap::new();
        env.insert("A".to_string(), "1".to_string());
        let first = supervisor
            .start(
                &session,
                &context,
                &StartSpec {
                    command: "true".to_string(),
                    env: env.clone(),
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap();
        let second = supervisor
            .start(
                &session,
                &context,
                &StartSpec {
                    command: "true".to_string(),
                    env: env.clone(),
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(first.env_hash, second.env_hash);
        assert_eq!(first.command_hash, second.command_hash);
        let mut different_env = BTreeMap::new();
        different_env.insert("A".to_string(), "2".to_string());
        let third = supervisor
            .start(
                &session,
                &context,
                &StartSpec {
                    command: "true".to_string(),
                    env: different_env,
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap();
        assert_ne!(first.env_hash, third.env_hash);
        assert_ne!(first.command_hash, third.command_hash);
        let fourth = supervisor
            .start(
                &session,
                &context,
                &StartSpec {
                    command: "true".to_string(),
                    name: Some("different name".to_string()),
                    env,
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap();
        // A display name does not change the immutable command identity.
        assert_eq!(first.env_hash, fourth.env_hash);
        assert_eq!(first.command_hash, fourth.command_hash);
        for row in [first, second, third, fourth] {
            wait_terminal(&supervisor, &session, &row.id).await;
        }
    }

    #[tokio::test]
    async fn logs_page_by_offset_and_report_rotation() {
        let (worktree, data, store, session) = setup();
        let supervisor = Arc::new(ProcessSupervisor::new(store.clone()));
        let context = context(worktree.path(), data.path(), &store, &session);
        let row = supervisor
            .start(
                &session,
                &context,
                &StartSpec {
                    command: "printf '0123456789\\nabcdefghij\\n'".to_string(),
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap();
        wait_terminal(&supervisor, &session, &row.id).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let first = supervisor
            .read_logs(&session, &row.id, Some(0), Some(4), Some(10), false)
            .unwrap();
        assert_eq!(first.content, "0123");
        assert_eq!(first.next_offset, 4);
        assert!(!first.eof);
        let second = supervisor
            .read_logs(
                &session,
                &row.id,
                Some(first.next_offset),
                Some(4),
                Some(10),
                false,
            )
            .unwrap();
        assert_eq!(second.content, "4567");
        assert_eq!(second.offset, 4);
        let tail = supervisor
            .read_logs(&session, &row.id, None, Some(6), Some(10), true)
            .unwrap();
        assert!(tail.eof);
        assert!(tail.content.ends_with('\n'));
        assert!(tail.total_bytes >= 22);
    }

    #[tokio::test]
    async fn stop_kills_the_whole_process_group() {
        let (worktree, data, store, session) = setup();
        let supervisor = Arc::new(ProcessSupervisor::new(store.clone()));
        let context = context(worktree.path(), data.path(), &store, &session);
        let row = supervisor
            .start(
                &session,
                &context,
                &StartSpec {
                    command: "sleep 30 & echo child=$!; wait".to_string(),
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let child_pid = loop {
            let logs = supervisor
                .read_logs(&session, &row.id, None, None, None, true)
                .unwrap();
            if let Some(pid) = logs
                .content
                .lines()
                .find_map(|line| line.strip_prefix("child="))
                .and_then(|pid| pid.trim().parse::<i32>().ok())
            {
                break pid;
            }
            assert!(Instant::now() < deadline, "child pid was not logged");
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        assert!(process_alive(child_pid));
        let result = supervisor
            .stop(
                &session,
                &row.id,
                Some("KILL"),
                Some(2),
                "test group cleanup",
            )
            .await
            .unwrap();
        assert_eq!(result.process.state, "stopped");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!process_alive(child_pid), "child survived the group signal");
    }

    #[tokio::test]
    async fn wait_reports_exit_and_timeout_without_guessing() {
        let (worktree, data, store, session) = setup();
        let supervisor = Arc::new(ProcessSupervisor::new(store.clone()));
        let context = context(worktree.path(), data.path(), &store, &session);
        let quick = supervisor
            .start(
                &session,
                &context,
                &StartSpec {
                    command: "printf 'never-ready\\n'; exit 7".to_string(),
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap();
        let pattern = Regex::new("^ready").unwrap();
        let outcome = supervisor
            .wait(&session, &quick.id, 5, Some(&pattern))
            .await
            .unwrap();
        assert!(!outcome.matched);
        assert!(!outcome.timed_out);
        assert_eq!(outcome.process.state, "exited");
        assert_eq!(outcome.process.exit_code, Some(7));
        assert!(outcome.evidence.contains("never-ready"));

        let slow = supervisor
            .start(
                &session,
                &context,
                &StartSpec {
                    command: "sleep 10".to_string(),
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap();
        let outcome = supervisor.wait(&session, &slow.id, 1, None).await.unwrap();
        assert!(outcome.timed_out);
        assert_eq!(outcome.process.state, "running");
        supervisor
            .stop(&session, &slow.id, Some("KILL"), Some(1), "cleanup")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn exit_sets_finished_and_retention_deadline() {
        let (worktree, data, store, session) = setup();
        let supervisor = Arc::new(ProcessSupervisor::new(store.clone()));
        let context = context(worktree.path(), data.path(), &store, &session);
        let row = supervisor
            .start(
                &session,
                &context,
                &StartSpec {
                    command: "exit 3".to_string(),
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap();
        wait_terminal(&supervisor, &session, &row.id).await;
        let row = supervisor.get(&session, &row.id).unwrap();
        assert_eq!(row.state, "exited");
        assert_eq!(row.exit_code, Some(3));
        assert!(row.finished_at.is_some());
        assert!(row.retention_deadline.is_some());
        let status = supervisor.status().unwrap();
        assert!(status.exited >= 1);
        assert!(status.output_bytes < 4_096);
    }

    #[tokio::test]
    async fn rejects_context_session_mismatch_and_imported_sessions() {
        let (worktree, data, store, session) = setup();
        let supervisor = Arc::new(ProcessSupervisor::new(store.clone()));
        let mut context = context(worktree.path(), data.path(), &store, &session);
        context.session_id = Some("another-session".to_string());
        let error = supervisor
            .start(
                &session,
                &context,
                &StartSpec {
                    command: "true".to_string(),
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("different session"));
        context.session_id = Some(session.clone());

        store.update_session_status(&session, "imported").unwrap();
        let error = supervisor
            .start(
                &session,
                &context,
                &StartSpec {
                    command: "true".to_string(),
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("imported"));
    }
}

#[cfg(test)]
mod supervisor_safety_tests {
    use super::*;
    use tempfile::TempDir;
    use vp_store::Store;

    fn setup() -> (TempDir, TempDir, Store, String) {
        let worktree = TempDir::new().unwrap();
        std::fs::create_dir_all(worktree.path().join("home")).unwrap();
        std::fs::create_dir_all(worktree.path().join("sub")).unwrap();
        let data = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(worktree.path().to_str().unwrap(), None)
            .unwrap();
        let session = store.create_session(&project.id, "safety").unwrap();
        (worktree, data, store, session.id)
    }

    fn context(worktree: &Path, data: &Path, store: &Store, session: &str) -> ToolContext {
        ToolContext {
            root: worktree.to_path_buf(),
            project_root: worktree.to_path_buf(),
            sandbox_root: data.join("sandbox").join(session),
            secret_roots: Vec::new(),
            network_allowed: true,
            home: worktree.join("home"),
            restrict_home: false,
            read_jail: false,
            git_branch: None,
            remote: None,
            session_id: Some(session.to_string()),
            process_supervisor: Some(Arc::new(ProcessSupervisor::new(store.clone()))),
        }
    }

    async fn terminal(supervisor: &ProcessSupervisor, session: &str, id: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !is_terminal(&supervisor.get(session, id).unwrap().state) {
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn cwd_is_worktree_relative_and_survives_replay() {
        let (worktree, data, store, session) = setup();
        let supervisor = Arc::new(ProcessSupervisor::new(store.clone()));
        let context = context(worktree.path(), data.path(), &store, &session);
        let row = supervisor
            .start(
                &session,
                &context,
                &StartSpec {
                    command: "pwd".to_string(),
                    cwd: Some("sub".to_string()),
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap();
        terminal(&supervisor, &session, &row.id).await;
        let expected = worktree.path().join("sub").canonicalize().unwrap();
        assert_eq!(row.cwd, expected.to_string_lossy());
        let logs = supervisor
            .read_logs(&session, &row.id, None, None, None, true)
            .unwrap();
        assert!(logs.content.contains(expected.to_string_lossy().as_ref()));
        let events = store.list_all_events(&session).unwrap();
        let replayed = processes_from_events(&events).unwrap();
        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0].cwd, row.cwd);
    }

    #[tokio::test]
    async fn log_reads_ignore_paths_outside_the_managed_root() {
        let (worktree, _data, store, session) = setup();
        let supervisor = Arc::new(ProcessSupervisor::new(store.clone()));
        let outside = TempDir::new().unwrap();
        let secret = outside.path().join("secret.log");
        std::fs::write(&secret, "do-not-read").unwrap();
        let mut row = fake_stopped_row(&session, worktree.path(), "psup_outside");
        row.output_path = secret.to_string_lossy().to_string();
        store
            .insert_supervised_process_with_event(&session, &row, PROCESS_STARTED_EVENT)
            .unwrap();
        let result = supervisor
            .read_logs(&session, &row.id, None, None, None, true)
            .unwrap();
        assert!(result.content.is_empty());
        assert_eq!(result.state, "stopped");
    }

    fn fake_stopped_row(session: &str, root: &Path, id: &str) -> SupervisedProcess {
        SupervisedProcess {
            id: id.to_string(),
            session_id: session.to_string(),
            name: "fake".to_string(),
            command: "sleep 1".to_string(),
            argv: vec!["/bin/sh".into(), "-c".into(), "sleep 1".into()],
            cwd: root.to_string_lossy().to_string(),
            worktree: root.to_string_lossy().to_string(),
            env_hash: "env".to_string(),
            command_hash: "command".to_string(),
            state: "stopped".to_string(),
            pid: None,
            pgid: None,
            nonce: "nonce".to_string(),
            started_at: vp_protocol::now_rfc3339(),
            process_started_at: None,
            heartbeat_at: vp_protocol::now_rfc3339(),
            finished_at: Some(vp_protocol::now_rfc3339()),
            exit_code: Some(0),
            signal: Some(15),
            output_path: String::new(),
            output_bytes: 0,
            output_lines: 0,
            total_output_bytes: 0,
            output_rotations: 0,
            max_output_bytes: 1_048_576,
            max_output_lines: 20_000,
            owner_instance: "dead:1".to_string(),
            retention_deadline: Some("2099-01-01T00:00:00Z".to_string()),
            idle_ttl_secs: 3_600,
            last_access_at: vp_protocol::now_rfc3339(),
            detail: None,
        }
    }

    #[tokio::test]
    async fn stop_refuses_an_unverified_unknown_row() {
        let (worktree, _data, store, session) = setup();
        let supervisor = Arc::new(ProcessSupervisor::new(store.clone()));
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "sleep 10"])
            .spawn()
            .unwrap();
        let pid = child.id() as i64;
        let mut row = fake_stopped_row(&session, worktree.path(), "psup_unknown_stop");
        row.state = "unknown".to_string();
        row.pid = Some(pid);
        row.pgid = Some(pid);
        row.process_started_at = process_start_time(pid as i32);
        row.nonce = "wrong".to_string();
        store
            .insert_supervised_process_with_event(&session, &row, PROCESS_STARTED_EVENT)
            .unwrap();
        let error = supervisor
            .stop(&session, &row.id, Some("TERM"), Some(1), "should refuse")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("refusing to signal"), "{error}");
        assert!(process_alive(pid as i32));
        let _ = child.kill();
        let _ = child.wait();
    }

    #[tokio::test]
    async fn stop_on_a_terminal_row_is_a_noop() {
        let (worktree, _data, store, session) = setup();
        let supervisor = Arc::new(ProcessSupervisor::new(store.clone()));
        let row = fake_stopped_row(&session, worktree.path(), "psup_terminal");
        store
            .insert_supervised_process_with_event(&session, &row, PROCESS_STARTED_EVENT)
            .unwrap();
        let result = supervisor
            .stop(&session, &row.id, Some("KILL"), Some(1), "noop")
            .await
            .unwrap();
        assert_eq!(result.process.state, "stopped");
        assert_eq!(result.waited_ms, 0);
        assert!(!result.forced || row.signal == Some(libc::SIGKILL));
    }

    #[tokio::test]
    async fn start_time_and_command_hash_mismatches_are_not_adopted() {
        let (worktree, _data, _store, session) = setup();
        let mut child = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                "sleep 10\nexit $?",
                "sh",
                "--verlinpark-nonce=expected",
                "--verlinpark-command-hash=expected-hash",
            ])
            .spawn()
            .unwrap();
        let pid = child.id() as i64;
        let start = process_start_time(pid as i32);
        let mut row = fake_stopped_row(&session, worktree.path(), "psup_evidence");
        row.state = "running".to_string();
        row.pid = Some(pid);
        row.pgid = Some(pid);
        row.nonce = "expected".to_string();
        row.command_hash = "expected-hash".to_string();
        row.process_started_at = start.clone();
        assert_eq!(identity_state(&row), "match");
        row.process_started_at = Some("Thu Jan 1 00:00:00 1970".to_string());
        assert_eq!(identity_state(&row), "pid_reused");
        row.process_started_at = start;
        row.command_hash = "other-hash".to_string();
        assert_eq!(identity_state(&row), "partial");
        row.command_hash = "expected-hash".to_string();
        row.nonce = "other-nonce".to_string();
        assert_eq!(identity_state(&row), "partial");
        let _ = child.kill();
        let _ = child.wait();
    }

    #[tokio::test]
    async fn output_line_cap_rotates_and_reports_retained_lines() {
        let (worktree, data, store, session) = setup();
        let supervisor = Arc::new(ProcessSupervisor::new(store.clone()));
        let context = context(worktree.path(), data.path(), &store, &session);
        let row = supervisor
            .start(
                &session,
                &context,
                &StartSpec {
                    command: "i=0; while [ $i -lt 1500 ]; do echo L$i; i=$((i+1)); done"
                        .to_string(),
                    max_output_bytes: 1_048_576,
                    max_output_lines: 200,
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap();
        terminal(&supervisor, &session, &row.id).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let row = supervisor.get(&session, &row.id).unwrap();
        assert!(row.output_rotations >= 1);
        assert!(row.output_lines <= 200 + 64, "lines={}", row.output_lines);
        let page = supervisor
            .read_logs(&session, &row.id, None, None, Some(25), true)
            .unwrap();
        assert!(page.content.lines().count() <= 26);
    }

    #[tokio::test]
    async fn terminal_rows_do_not_count_toward_the_active_cap() {
        let (worktree, data, store, session) = setup();
        let supervisor = Arc::new(ProcessSupervisor::new(store.clone()));
        let context = context(worktree.path(), data.path(), &store, &session);
        let mut rows = Vec::new();
        for index in 0..MAX_PROCESSES_PER_SESSION {
            rows.push(
                supervisor
                    .start(
                        &session,
                        &context,
                        &StartSpec {
                            command: format!("true # {index}"),
                            ..StartSpec::default()
                        },
                    )
                    .await
                    .unwrap(),
            );
        }
        for row in &rows {
            terminal(&supervisor, &session, &row.id).await;
        }
        assert_eq!(supervisor.active_count(&session).unwrap(), 0);
        let next = supervisor
            .start(
                &session,
                &context,
                &StartSpec {
                    command: "true".to_string(),
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap();
        terminal(&supervisor, &session, &next.id).await;
    }
}

#[cfg(test)]
mod lifecycle_replay_tests {
    use super::*;
    use tempfile::TempDir;
    use vp_protocol::types::Event;
    use vp_store::Store;

    fn setup() -> (TempDir, TempDir, Store, String) {
        let worktree = TempDir::new().unwrap();
        std::fs::create_dir_all(worktree.path().join("home")).unwrap();
        let data = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(worktree.path().to_str().unwrap(), None)
            .unwrap();
        let session = store.create_session(&project.id, "lifecycle").unwrap();
        (worktree, data, store, session.id)
    }

    fn context(worktree: &Path, data: &Path, store: &Store, session: &str) -> ToolContext {
        ToolContext {
            root: worktree.to_path_buf(),
            project_root: worktree.to_path_buf(),
            sandbox_root: data.join("sandbox").join(session),
            secret_roots: Vec::new(),
            network_allowed: true,
            home: worktree.join("home"),
            restrict_home: false,
            read_jail: false,
            git_branch: None,
            remote: None,
            session_id: Some(session.to_string()),
            process_supervisor: Some(Arc::new(ProcessSupervisor::new(store.clone()))),
        }
    }

    async fn wait_terminal(supervisor: &ProcessSupervisor, session: &str, id: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !is_terminal(&supervisor.get(session, id).unwrap().state) {
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn lifecycle_events_are_ordered_and_pruning_removes_replay_rows() {
        let (worktree, data, store, session) = setup();
        let supervisor = Arc::new(ProcessSupervisor::new(store.clone()));
        let context = context(worktree.path(), data.path(), &store, &session);
        let row = supervisor
            .start(
                &session,
                &context,
                &StartSpec {
                    command: "printf 'done\\n'".to_string(),
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap();
        wait_terminal(&supervisor, &session, &row.id).await;
        let events = store.list_all_events(&session).unwrap();
        let kinds = events
            .iter()
            .filter(|event| event.kind.starts_with("process."))
            .map(|event| event.kind.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                PROCESS_STARTING_EVENT,
                PROCESS_STARTED_EVENT,
                PROCESS_EXITED_EVENT
            ]
        );
        let replay = processes_from_events(&events).unwrap();
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].state, "exited");
        let pruned = store
            .prune_supervised_process_with_event(&session, &row.id, "test")
            .unwrap()
            .unwrap();
        assert_eq!(pruned.kind, PROCESS_PRUNED_EVENT);
        let events = store.list_all_events(&session).unwrap();
        assert!(processes_from_events(&events).unwrap().is_empty());
    }

    #[tokio::test]
    async fn retention_prune_removes_the_output_directory() {
        let (worktree, data, store, session) = setup();
        let supervisor = Arc::new(ProcessSupervisor::new(store.clone()));
        let context = context(worktree.path(), data.path(), &store, &session);
        let row = supervisor
            .start(
                &session,
                &context,
                &StartSpec {
                    command: "printf 'retained\\n'".to_string(),
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap();
        wait_terminal(&supervisor, &session, &row.id).await;
        let output = PathBuf::from(&row.output_path);
        let directory = output.parent().unwrap().to_path_buf();
        assert!(output.is_file());
        let mut expired = supervisor.get(&session, &row.id).unwrap();
        expired.retention_deadline = Some("2000-01-01T00:00:00Z".to_string());
        store
            .update_supervised_process_with_event(&expired, PROCESS_EXITED_EVENT)
            .unwrap();
        let report = supervisor.sweep(false).await.unwrap();
        assert_eq!(report.pruned, 1);
        assert!(!directory.exists(), "output directory survived prune");
        assert!(supervisor.get(&session, &row.id).is_err());
    }

    #[tokio::test]
    async fn malformed_lifecycle_payloads_fail_closed() {
        let event = Event {
            session_id: "ses".to_string(),
            seq: 1,
            kind: PROCESS_STARTED_EVENT.to_string(),
            payload: serde_json::json!({"not": "a process"}),
            created_at: vp_protocol::now_rfc3339(),
        };
        assert!(processes_from_events(&[event]).is_none());
        let pruned = Event {
            session_id: "ses".to_string(),
            seq: 2,
            kind: PROCESS_PRUNED_EVENT.to_string(),
            payload: serde_json::json!({"reason": "missing id"}),
            created_at: vp_protocol::now_rfc3339(),
        };
        assert!(processes_from_events(&[pruned]).is_none());
    }

    #[tokio::test]
    async fn list_is_session_scoped_and_status_tracks_counts() {
        let (worktree, data, store, session) = setup();
        let project_id = store.get_session(&session).unwrap().unwrap().project_id;
        let other = store.create_session(&project_id, "other").unwrap();
        let supervisor = Arc::new(ProcessSupervisor::new(store.clone()));
        let context = context(worktree.path(), data.path(), &store, &session);
        let row = supervisor
            .start(
                &session,
                &context,
                &StartSpec {
                    command: "printf 'scoped\\n'; sleep 10".to_string(),
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(supervisor.list(&session, true, 10).unwrap().len(), 1);
        assert!(supervisor.list(&other.id, true, 10).unwrap().is_empty());
        let status = supervisor.status().unwrap();
        assert_eq!(status.running, 1);
        assert_eq!(status.instance, supervisor.instance());
        supervisor
            .stop(&session, &row.id, Some("KILL"), Some(1), "cleanup")
            .await
            .unwrap();
        let status = supervisor.status().unwrap();
        assert_eq!(status.running, 0);
        assert_eq!(status.stopped, 1);
    }

    #[tokio::test]
    async fn log_page_caps_are_clamped() {
        let (worktree, data, store, session) = setup();
        let supervisor = Arc::new(ProcessSupervisor::new(store.clone()));
        let context = context(worktree.path(), data.path(), &store, &session);
        let row = supervisor
            .start(
                &session,
                &context,
                &StartSpec {
                    command: "printf 'abc\\n'".to_string(),
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap();
        wait_terminal(&supervisor, &session, &row.id).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let page = supervisor
            .read_logs(
                &session,
                &row.id,
                Some(0),
                Some(u32::MAX),
                Some(u32::MAX),
                false,
            )
            .unwrap();
        assert!(page.content.len() <= MAX_LOG_PAGE_BYTES);
        assert!(page.content.lines().count() <= MAX_LOG_PAGE_LINES);
    }

    #[tokio::test]
    async fn a_single_huge_line_is_rotated_without_losing_all_output() {
        let (worktree, data, store, session) = setup();
        let supervisor = Arc::new(ProcessSupervisor::new(store.clone()));
        let context = context(worktree.path(), data.path(), &store, &session);
        let row = supervisor
            .start(
                &session,
                &context,
                &StartSpec {
                    command: "head -c 20000 /dev/zero | tr '\\0' x".to_string(),
                    max_output_bytes: 4_096,
                    max_output_lines: 100,
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap();
        wait_terminal(&supervisor, &session, &row.id).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let row = supervisor.get(&session, &row.id).unwrap();
        assert!(row.output_rotations >= 1);
        assert!(row.output_bytes <= 8_192, "bytes={}", row.output_bytes);
        let page = supervisor
            .read_logs(&session, &row.id, None, None, None, true)
            .unwrap();
        assert!(!page.content.is_empty());
        assert!(page.content.chars().all(|ch| ch == 'x' || ch == '\n'));
    }

    #[tokio::test]
    async fn invalid_commands_and_large_env_values_are_rejected() {
        let (worktree, data, store, session) = setup();
        let supervisor = Arc::new(ProcessSupervisor::new(store.clone()));
        let context = context(worktree.path(), data.path(), &store, &session);
        let error = supervisor
            .start(
                &session,
                &context,
                &StartSpec {
                    command: "echo \0".to_string(),
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("NUL"));
        let mut env = BTreeMap::new();
        env.insert("LARGE".to_string(), "x".repeat(4_097));
        let error = supervisor
            .start(
                &session,
                &context,
                &StartSpec {
                    command: "true".to_string(),
                    env,
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("too large"));
    }
}

#[cfg(test)]
mod reattach_monitor_tests {
    use super::*;
    use tempfile::TempDir;
    use vp_store::Store;

    fn setup() -> (TempDir, TempDir, Store, String) {
        let worktree = TempDir::new().unwrap();
        std::fs::create_dir_all(worktree.path().join("home")).unwrap();
        let data = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(worktree.path().to_str().unwrap(), None)
            .unwrap();
        let session = store.create_session(&project.id, "monitor").unwrap();
        (worktree, data, store, session.id)
    }

    fn fake_row(
        session: &str,
        root: &Path,
        id: &str,
        pid: i64,
        start: Option<String>,
        nonce: &str,
    ) -> SupervisedProcess {
        SupervisedProcess {
            id: id.to_string(),
            session_id: session.to_string(),
            name: "reattached".to_string(),
            command: "sleep 10".to_string(),
            argv: vec!["/bin/sh".into(), "-c".into(), "sleep 10".into()],
            cwd: root.to_string_lossy().to_string(),
            worktree: root.to_string_lossy().to_string(),
            env_hash: "env".to_string(),
            command_hash: "hash".to_string(),
            state: "running".to_string(),
            pid: Some(pid),
            pgid: Some(pid),
            nonce: nonce.to_string(),
            started_at: vp_protocol::now_rfc3339(),
            process_started_at: start,
            heartbeat_at: vp_protocol::now_rfc3339(),
            finished_at: None,
            exit_code: None,
            signal: None,
            output_path: String::new(),
            output_bytes: 0,
            output_lines: 0,
            total_output_bytes: 0,
            output_rotations: 0,
            max_output_bytes: 1_048_576,
            max_output_lines: 20_000,
            owner_instance: "old:1".to_string(),
            retention_deadline: None,
            idle_ttl_secs: 3_600,
            last_access_at: vp_protocol::now_rfc3339(),
            detail: None,
        }
    }

    async fn wait_state(supervisor: &ProcessSupervisor, session: &str, id: &str, expected: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let row = supervisor.get(session, id).unwrap();
            if row.state == expected {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "expected {expected}, saw {}",
                row.state
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    #[tokio::test]
    async fn unknown_row_resolves_to_lost_when_the_pid_disappears() {
        let (worktree, _data, store, session) = setup();
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "sleep 10"])
            .spawn()
            .unwrap();
        let pid = child.id() as i64;
        let row = fake_row(
            &session,
            worktree.path(),
            "psup_unknown_monitor",
            pid,
            process_start_time(pid as i32),
            "wrong-nonce",
        );
        store
            .insert_supervised_process_with_event(&session, &row, PROCESS_STARTED_EVENT)
            .unwrap();
        let supervisor = Arc::new(ProcessSupervisor::new(store.clone()));
        let report = supervisor.reconcile().await.unwrap();
        assert_eq!(report.unknown, 1);
        assert_eq!(supervisor.get(&session, &row.id).unwrap().state, "unknown");
        let _ = child.kill();
        let _ = child.wait();
        wait_state(&supervisor, &session, &row.id, "lost").await;
        assert!(
            supervisor
                .get(&session, &row.id)
                .unwrap()
                .detail
                .unwrap_or_default()
                .contains("disappeared")
        );
    }

    #[tokio::test]
    async fn adopted_row_resolves_to_lost_when_the_pid_disappears() {
        let (worktree, _data, store, session) = setup();
        let mut child = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                "sleep 10\nexit $?",
                "sh",
                "--verlinpark-nonce=nonce",
                "--verlinpark-command-hash=hash",
            ])
            .spawn()
            .unwrap();
        let pid = child.id() as i64;
        let row = fake_row(
            &session,
            worktree.path(),
            "psup_adopted_monitor",
            pid,
            process_start_time(pid as i32),
            "nonce",
        );
        store
            .insert_supervised_process_with_event(&session, &row, PROCESS_STARTED_EVENT)
            .unwrap();
        let supervisor = Arc::new(ProcessSupervisor::new(store.clone()));
        let report = supervisor.reconcile().await.unwrap();
        assert_eq!(report.adopted, 1);
        assert_eq!(supervisor.get(&session, &row.id).unwrap().state, "running");
        let _ = child.kill();
        let _ = child.wait();
        wait_state(&supervisor, &session, &row.id, "lost").await;
    }
}

#[cfg(test)]
mod process_policy_tests {
    use super::*;
    use tempfile::TempDir;
    use vp_store::Store;

    fn setup() -> (TempDir, TempDir, Store, String, String) {
        let worktree = TempDir::new().unwrap();
        std::fs::create_dir_all(worktree.path().join("home")).unwrap();
        let data = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(worktree.path().to_str().unwrap(), None)
            .unwrap();
        let session = store.create_session(&project.id, "policy").unwrap();
        (worktree, data, store, project.id, session.id)
    }

    fn context(worktree: &Path, data: &Path, store: &Store, session: &str) -> ToolContext {
        ToolContext {
            root: worktree.to_path_buf(),
            project_root: worktree.to_path_buf(),
            sandbox_root: data.join("sandbox").join(session),
            secret_roots: Vec::new(),
            network_allowed: true,
            home: worktree.join("home"),
            restrict_home: false,
            read_jail: false,
            git_branch: None,
            remote: None,
            session_id: Some(session.to_string()),
            process_supervisor: Some(Arc::new(ProcessSupervisor::new(store.clone()))),
        }
    }

    fn now_minus(seconds: i64) -> String {
        let value = time::OffsetDateTime::now_utc() - time::Duration::seconds(seconds);
        value
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap()
    }

    #[tokio::test]
    async fn project_policy_clamps_caps_ttl_and_active_count() {
        let (worktree, data, store, project, session) = setup();
        store
            .set_process_policy(
                &project,
                &vp_protocol::types::ProcessPolicy {
                    idle_sweep_enabled: true,
                    max_processes: 2,
                    idle_ttl_secs: 60,
                    max_output_bytes: 8_192,
                    max_output_lines: 100,
                },
            )
            .unwrap();
        let supervisor = Arc::new(ProcessSupervisor::new(store.clone()));
        let context = context(worktree.path(), data.path(), &store, &session);
        let first = supervisor
            .start(
                &session,
                &context,
                &StartSpec {
                    command: "sleep 20".to_string(),
                    idle_ttl_secs: 3_600,
                    max_output_bytes: 1_048_576,
                    max_output_lines: 20_000,
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(first.idle_ttl_secs, 60);
        assert_eq!(first.max_output_bytes, 8_192);
        assert_eq!(first.max_output_lines, 100);
        let second = supervisor
            .start(
                &session,
                &context,
                &StartSpec {
                    command: "sleep 20".to_string(),
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap();
        let error = supervisor
            .start(
                &session,
                &context,
                &StartSpec {
                    command: "true".to_string(),
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("process limit is 2"), "{error}");
        assert_eq!(supervisor.policy_for_session(&session).max_processes, 2);
        for row in [first, second] {
            supervisor
                .stop(&session, &row.id, Some("KILL"), Some(1), "cleanup")
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn disabled_idle_sweep_does_not_signal_a_running_process() {
        let (worktree, data, store, project, session) = setup();
        store
            .set_process_policy(
                &project,
                &vp_protocol::types::ProcessPolicy {
                    idle_sweep_enabled: false,
                    ..vp_protocol::types::ProcessPolicy::default()
                },
            )
            .unwrap();
        let supervisor = Arc::new(ProcessSupervisor::new(store.clone()));
        let context = context(worktree.path(), data.path(), &store, &session);
        let row = supervisor
            .start(
                &session,
                &context,
                &StartSpec {
                    command: "sleep 20".to_string(),
                    idle_ttl_secs: 60,
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap();
        let mut stale = supervisor.get(&session, &row.id).unwrap();
        stale.last_access_at = now_minus(120);
        store.record_supervised_process_telemetry(&stale).unwrap();
        let report = supervisor.sweep(true).await.unwrap();
        assert_eq!(report.idle_stopped, 0, "report={report:?}");
        assert_eq!(supervisor.get(&session, &row.id).unwrap().state, "running");
        supervisor
            .stop(&session, &row.id, Some("KILL"), Some(1), "cleanup")
            .await
            .unwrap();
    }
}

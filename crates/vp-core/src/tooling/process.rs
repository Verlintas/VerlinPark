//! Long-lived sandboxed processes for dev servers, watchers, and interactive
//! tools. Processes are scoped to one agent worktree, capped, and swept by an
//! idle TTL. They intentionally do not survive a daemon restart in v1.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use regex::Regex;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::ChildStdin;
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use crate::tools::{
    AgentTool, ToolContext, ToolOutput, safe_existing_path, sandboxed_command, truncate,
};

const MAX_PROCESSES_PER_WORKTREE: usize = 8;
const MAX_STREAM_BYTES: usize = 2 * 1024 * 1024;
const DEFAULT_IDLE_TTL_SECS: u64 = 3_600;
const COMPLETED_RETENTION_SECS: u64 = 600;
const READ_MAX_BYTES: usize = 1024 * 1024;

pub struct ProcessTool;
pub struct WaitFor;

#[derive(Debug, Default)]
struct StreamBuffer {
    bytes: Vec<u8>,
    base: u64,
    total: u64,
    truncated: bool,
}

impl StreamBuffer {
    fn push(&mut self, bytes: &[u8]) {
        self.total = self.total.saturating_add(bytes.len() as u64);
        self.bytes.extend_from_slice(bytes);
        if self.bytes.len() > MAX_STREAM_BYTES {
            let excess = self.bytes.len() - MAX_STREAM_BYTES;
            self.bytes.drain(..excess);
            self.base = self.base.saturating_add(excess as u64);
            self.truncated = true;
        }
    }

    fn read_from(&self, cursor: u64, max_bytes: usize) -> (String, u64, bool, bool) {
        let skipped = cursor < self.base;
        let start = if skipped {
            0
        } else {
            (cursor - self.base) as usize
        };
        if start >= self.bytes.len() {
            return (String::new(), self.total, skipped, false);
        }
        let end = (start + max_bytes).min(self.bytes.len());
        let text = String::from_utf8_lossy(&self.bytes[start..end]).to_string();
        let next = self.base.saturating_add(end as u64);
        let more = end < self.bytes.len();
        (text, next, skipped, more)
    }

    fn tail(&self, max_bytes: usize) -> String {
        let start = self.bytes.len().saturating_sub(max_bytes);
        String::from_utf8_lossy(&self.bytes[start..]).to_string()
    }
}

#[derive(Debug, Default)]
struct ProcessOutput {
    stdout: StreamBuffer,
    stderr: StreamBuffer,
}

struct ProcessState {
    id: String,
    name: String,
    command: String,
    root: PathBuf,
    pid: Option<i32>,
    stdin: Option<ChildStdin>,
    output: Arc<Mutex<ProcessOutput>>,
    done: bool,
    exit_code: Option<i32>,
    started: Instant,
    last_access: Instant,
    idle_ttl: Duration,
    stdout_cursor: u64,
    stderr_cursor: u64,
}

type SharedProcess = Arc<AsyncMutex<ProcessState>>;

static PROCESSES: OnceLock<Mutex<HashMap<String, SharedProcess>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<String, SharedProcess>> {
    PROCESSES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Kill every tracked process whose worktree is about to be removed.
pub(crate) fn stop_worktree_processes(root: &Path) {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let Ok(mut processes) = registry().lock() else {
        return;
    };
    processes.retain(|_, process| {
        let Ok(guard) = process.try_lock() else {
            return true;
        };
        if guard.root == root {
            kill_group(guard.pid, libc::SIGKILL);
            false
        } else {
            true
        }
    });
}

fn process_root(context: &ToolContext) -> PathBuf {
    context
        .root
        .canonicalize()
        .unwrap_or_else(|_| context.root.clone())
}

fn ensure_sweeper() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        tokio::spawn(async {
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
                reap_all();
            }
        });
    });
}

fn reap_all() {
    let Ok(mut processes) = registry().lock() else {
        return;
    };
    let now = Instant::now();
    processes.retain(|_, process| {
        let Ok(guard) = process.try_lock() else {
            return true;
        };
        if !guard.done && now.duration_since(guard.last_access) > guard.idle_ttl {
            kill_group(guard.pid, libc::SIGKILL);
            return false;
        }
        if guard.done
            && now.duration_since(guard.started) > Duration::from_secs(COMPLETED_RETENTION_SECS)
        {
            return false;
        }
        true
    });
}

fn count_for_root(root: &Path) -> usize {
    let Ok(processes) = registry().lock() else {
        return 0;
    };
    processes
        .values()
        .filter(|process| {
            process
                .try_lock()
                .map(|guard| guard.root == root && !guard.done)
                .unwrap_or(true)
        })
        .count()
}

async fn get_process(context: &ToolContext, id: &str) -> Result<SharedProcess> {
    let root = process_root(context);
    let process = registry()
        .lock()
        .ok()
        .and_then(|processes| processes.get(id).cloned())
        .ok_or_else(|| anyhow!("unknown process id: {id}"))?;
    let matches = {
        let guard = process.lock().await;
        guard.root == root
    };
    if !matches {
        bail!("process does not belong to this worktree");
    }
    Ok(process)
}

#[async_trait]
impl AgentTool for ProcessTool {
    fn name(&self) -> &'static str {
        "process"
    }

    fn description(&self) -> &'static str {
        "Start, inspect, read, write, signal, wait for, or stop a long-lived sandboxed process such as a dev server or watcher."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["start", "list", "status", "read", "write", "signal", "wait", "stop"]},
                "command": {"type": "string", "description": "Shell command for action=start"},
                "name": {"type": "string", "description": "Human label; default is the first command word"},
                "cwd": {"type": "string", "description": "Worktree-relative working directory"},
                "env": {"type": "object", "description": "Explicit environment variables; max 32 keys"},
                "process_id": {"type": "string", "description": "Required for status/read/write/signal/wait/stop"},
                "input": {"type": "string", "description": "stdin text for action=write"},
                "newline": {"type": "boolean", "description": "Append newline to stdin; default true"},
                "signal": {"type": "string", "enum": ["TERM", "KILL", "INT", "HUP"], "description": "Default TERM"},
                "pattern": {"type": "string", "description": "Regex pattern for action=wait"},
                "timeout_secs": {"type": "integer", "minimum": 1, "maximum": 3600, "description": "Wait or idle TTL; default 30 for wait, 3600 for start"},
                "max_bytes": {"type": "integer", "minimum": 1, "maximum": 1048576, "description": "Default 65536"},
                "reset": {"type": "boolean", "description": "Read from the beginning of the retained buffer"}
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let action = input
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("action is required"))?;
        ensure_sweeper();
        let root = process_root(context);
        reap_all();
        match action {
            "start" => start_process(context, &input, &root).await,
            "list" => list_processes(&root).await,
            "status" => status_process(context, &input).await,
            "read" => read_process(context, &input).await,
            "write" => write_process(context, &input).await,
            "signal" => signal_process(context, &input).await,
            "wait" => wait_process(context, &input).await,
            "stop" => stop_process(context, &input).await,
            _ => Ok(ToolOutput::error_structured(
                format!("unsupported process action: {action}"),
                json!({"code": "INVALID_ARGUMENT", "action": action}),
            )),
        }
    }
}

async fn start_process(context: &ToolContext, input: &Value, root: &Path) -> Result<ToolOutput> {
    let command = input
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .ok_or_else(|| anyhow!("command is required for action=start"))?;
    if command.len() > 16_384 {
        return Ok(ToolOutput::error_structured(
            "command exceeds the 16 KiB limit",
            json!({"code": "INVALID_ARGUMENT"}),
        ));
    }
    if count_for_root(root) >= MAX_PROCESSES_PER_WORKTREE {
        return Ok(ToolOutput::error_structured(
            format!(
                "process limit is {MAX_PROCESSES_PER_WORKTREE} per worktree; stop an existing process first"
            ),
            json!({"code": "PROCESS_LIMIT", "limit": MAX_PROCESSES_PER_WORKTREE}),
        ));
    }
    let name = input
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            command
                .split_whitespace()
                .next()
                .unwrap_or("process")
                .to_string()
        });
    let idle_ttl = input
        .get("timeout_secs")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_IDLE_TTL_SECS)
        .clamp(1, 86_400);
    let cwd = if let Some(relative) = input.get("cwd").and_then(Value::as_str) {
        let path = safe_existing_path(root, relative)?;
        if !path.is_dir() {
            return Ok(ToolOutput::error_structured(
                "cwd must be a directory",
                json!({"code": "INVALID_ARGUMENT", "cwd": relative}),
            ));
        }
        Some(path)
    } else {
        None
    };
    let mut process = sandboxed_command(context).await?;
    process
        .arg("/bin/sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = &cwd {
        process.current_dir(cwd);
    }
    if let Some(env) = input.get("env").and_then(Value::as_object) {
        if env.len() > 32 {
            return Ok(ToolOutput::error_structured(
                "env accepts at most 32 entries",
                json!({"code": "INVALID_ARGUMENT", "entries": env.len()}),
            ));
        }
        for (key, value) in env {
            if !valid_env_key(key) {
                return Ok(ToolOutput::error_structured(
                    format!("invalid environment variable name: {key}"),
                    json!({"code": "INVALID_ARGUMENT", "key": key}),
                ));
            }
            let Some(value) = value.as_str() else {
                return Ok(ToolOutput::error_structured(
                    format!("environment variable {key} must be a string"),
                    json!({"code": "INVALID_ARGUMENT", "key": key}),
                ));
            };
            if value.len() > 4_096 || value.contains('\0') {
                return Ok(ToolOutput::error_structured(
                    format!("environment variable {key} is too large or contains NUL"),
                    json!({"code": "INVALID_ARGUMENT", "key": key}),
                ));
            }
            process.env(key, value);
        }
    }
    let mut child = process.spawn().context("spawning sandboxed process")?;
    let pid = child.id().and_then(|pid| i32::try_from(pid).ok());
    let stdin = child.stdin.take();
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("process stdout pipe is unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("process stderr pipe is unavailable"))?;
    let output = Arc::new(Mutex::new(ProcessOutput::default()));
    tokio::spawn(pump_stream(stdout, output.clone(), true));
    tokio::spawn(pump_stream(stderr, output.clone(), false));
    let id = format!("proc_{}", &Uuid::now_v7().simple().to_string()[..12]);
    let state = Arc::new(AsyncMutex::new(ProcessState {
        id: id.clone(),
        name: name.clone(),
        command: command.to_string(),
        root: root.to_path_buf(),
        pid,
        stdin,
        output: output.clone(),
        done: false,
        exit_code: None,
        started: Instant::now(),
        last_access: Instant::now(),
        idle_ttl: Duration::from_secs(idle_ttl),
        stdout_cursor: 0,
        stderr_cursor: 0,
    }));
    let wait_state = state.clone();
    tokio::spawn(async move {
        let status = child.wait().await;
        let mut guard = wait_state.lock().await;
        guard.done = true;
        guard.exit_code = status.ok().and_then(|status| status.code());
        guard.pid = None;
        guard.stdin = None;
    });
    if let Ok(mut processes) = registry().lock() {
        processes.insert(id.clone(), state);
    }
    Ok(ToolOutput::ok_structured(
        format!(
            "started {} ({}){}",
            id,
            name,
            cwd.map(|cwd| format!(" in {}", cwd.display()))
                .unwrap_or_default()
        ),
        json!({
            "process_id": id,
            "name": name,
            "command": command,
            "pid": pid,
            "status": "running",
            "idle_ttl_secs": idle_ttl,
            "next_actions": [format!("process action=wait process_id={id} pattern=<ready>")],
        }),
    ))
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

async fn pump_stream<R>(mut reader: R, output: Arc<Mutex<ProcessOutput>>, stdout: bool)
where
    R: AsyncRead + Unpin,
{
    let mut buffer = [0u8; 8192];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                if let Ok(mut output) = output.lock() {
                    if stdout {
                        output.stdout.push(&buffer[..read]);
                    } else {
                        output.stderr.push(&buffer[..read]);
                    }
                }
            }
        }
    }
}

async fn list_processes(root: &Path) -> Result<ToolOutput> {
    let processes = registry()
        .lock()
        .map(|processes| processes.values().cloned().collect::<Vec<SharedProcess>>())
        .unwrap_or_default();
    let mut entries = Vec::new();
    for process in processes {
        let guard = process.lock().await;
        if guard.root != root {
            continue;
        }
        entries.push(json!({
            "process_id": guard.id,
            "name": guard.name,
            "command": guard.command,
            "pid": guard.pid,
            "done": guard.done,
            "exit_code": guard.exit_code,
            "duration_ms": guard.started.elapsed().as_millis() as u64,
        }));
    }
    entries.sort_by(|left, right| {
        left["process_id"]
            .as_str()
            .cmp(&right["process_id"].as_str())
    });
    let rendered = entries
        .iter()
        .map(|entry| {
            format!(
                "{} {} · {} · {}",
                entry["process_id"].as_str().unwrap_or("?"),
                entry["name"].as_str().unwrap_or("?"),
                if entry["done"].as_bool().unwrap_or(false) {
                    format!("exited {}", entry["exit_code"].as_i64().unwrap_or(-1))
                } else {
                    "running".to_string()
                },
                entry["command"].as_str().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(ToolOutput::ok_structured(
        truncate(rendered),
        json!({
            "processes": entries,
            "count": entries.len(),
            "limit": MAX_PROCESSES_PER_WORKTREE,
        }),
    ))
}

async fn status_process(context: &ToolContext, input: &Value) -> Result<ToolOutput> {
    let id = process_id(input)?;
    let process = get_process(context, id).await?;
    let mut guard = process.lock().await;
    guard.last_access = Instant::now();
    let output_bytes = guard
        .output
        .lock()
        .map(|output| {
            (
                output.stdout.total,
                output.stderr.total,
                output.stdout.truncated || output.stderr.truncated,
            )
        })
        .unwrap_or((0, 0, false));
    Ok(ToolOutput::ok_structured(
        format!(
            "{} {} · {}{}ms",
            guard.id,
            guard.name,
            if guard.done {
                format!("exited {} · ", guard.exit_code.unwrap_or(-1))
            } else {
                "running · ".to_string()
            },
            guard.started.elapsed().as_millis()
        ),
        json!({
            "process_id": guard.id,
            "name": guard.name,
            "command": guard.command,
            "pid": guard.pid,
            "done": guard.done,
            "exit_code": guard.exit_code,
            "duration_ms": guard.started.elapsed().as_millis() as u64,
            "stdout_bytes": output_bytes.0,
            "stderr_bytes": output_bytes.1,
            "output_truncated": output_bytes.2,
            "stdout_cursor": guard.stdout_cursor,
            "stderr_cursor": guard.stderr_cursor,
        }),
    ))
}

async fn read_process(context: &ToolContext, input: &Value) -> Result<ToolOutput> {
    let id = process_id(input)?;
    let max_bytes = input
        .get("max_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(65_536)
        .clamp(1, READ_MAX_BYTES as u64) as usize;
    let reset = input.get("reset").and_then(Value::as_bool).unwrap_or(false);
    let process = get_process(context, id).await?;
    let mut guard = process.lock().await;
    guard.last_access = Instant::now();
    if reset {
        guard.stdout_cursor = 0;
        guard.stderr_cursor = 0;
    }
    let stdout_cursor = guard.stdout_cursor;
    let stderr_cursor = guard.stderr_cursor;
    let (stdout, next_stdout, skipped_stdout, more_stdout) = guard
        .output
        .lock()
        .map(|output| output.stdout.read_from(stdout_cursor, max_bytes))
        .unwrap_or_default();
    let (stderr, next_stderr, skipped_stderr, more_stderr) = guard
        .output
        .lock()
        .map(|output| output.stderr.read_from(stderr_cursor, max_bytes))
        .unwrap_or_default();
    guard.stdout_cursor = next_stdout;
    guard.stderr_cursor = next_stderr;
    let mut text = String::new();
    if skipped_stdout || skipped_stderr {
        text.push_str("[older output was dropped from the ring buffer]\n");
    }
    if !stdout.is_empty() {
        text.push_str("[stdout]\n");
        text.push_str(&stdout);
        if !stdout.ends_with('\n') {
            text.push('\n');
        }
    }
    if !stderr.is_empty() {
        text.push_str("[stderr]\n");
        text.push_str(&stderr);
        if !stderr.ends_with('\n') {
            text.push('\n');
        }
    }
    if text.is_empty() {
        text.push_str("(no new output)");
    }
    let output = guard.output.clone();
    let tail = output
        .lock()
        .map(|output| format!("{}{}", output.stdout.tail(2_000), output.stderr.tail(2_000)))
        .unwrap_or_default();
    Ok(ToolOutput::ok_structured(
        truncate(text),
        json!({
            "process_id": guard.id,
            "done": guard.done,
            "exit_code": guard.exit_code,
            "stdout": stdout,
            "stderr": stderr,
            "stdout_cursor": guard.stdout_cursor,
            "stderr_cursor": guard.stderr_cursor,
            "more_stdout": more_stdout,
            "more_stderr": more_stderr,
            "skipped": skipped_stdout || skipped_stderr,
            "tail": tail,
        }),
    ))
}

async fn write_process(context: &ToolContext, input: &Value) -> Result<ToolOutput> {
    let id = process_id(input)?;
    let text = input
        .get("input")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("input is required for action=write"))?;
    let newline = input
        .get("newline")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let process = get_process(context, id).await?;
    let mut guard = process.lock().await;
    guard.last_access = Instant::now();
    let Some(stdin) = guard.stdin.as_mut() else {
        return Ok(ToolOutput::error_structured(
            "process stdin is closed",
            json!({"code": "PROCESS_EXITED", "process_id": guard.id, "exit_code": guard.exit_code}),
        ));
    };
    let mut payload = text.as_bytes().to_vec();
    if newline {
        payload.push(b'\n');
    }
    stdin.write_all(&payload).await?;
    stdin.flush().await?;
    Ok(ToolOutput::ok_structured(
        format!("wrote {} byte(s) to {}", payload.len(), guard.id),
        json!({
            "process_id": guard.id,
            "bytes": payload.len(),
        }),
    ))
}

async fn signal_process(context: &ToolContext, input: &Value) -> Result<ToolOutput> {
    let id = process_id(input)?;
    let signal = match input
        .get("signal")
        .and_then(Value::as_str)
        .unwrap_or("TERM")
        .to_ascii_uppercase()
        .as_str()
    {
        "TERM" => libc::SIGTERM,
        "KILL" => libc::SIGKILL,
        "INT" => libc::SIGINT,
        "HUP" => libc::SIGHUP,
        other => {
            return Ok(ToolOutput::error_structured(
                format!("unsupported signal: {other}"),
                json!({"code": "INVALID_ARGUMENT", "signal": other}),
            ));
        }
    };
    let process = get_process(context, id).await?;
    let mut guard = process.lock().await;
    guard.last_access = Instant::now();
    if guard.done {
        return Ok(ToolOutput::ok_structured(
            format!(
                "{} already exited {}",
                guard.id,
                guard.exit_code.unwrap_or(-1)
            ),
            json!({"process_id": guard.id, "done": true, "exit_code": guard.exit_code}),
        ));
    }
    let sent = kill_group(guard.pid, signal);
    Ok(ToolOutput::ok_structured(
        format!("sent signal {} to {}", signal, guard.id),
        json!({
            "process_id": guard.id,
            "signal": signal,
            "sent": sent,
        }),
    ))
}

async fn wait_process(context: &ToolContext, input: &Value) -> Result<ToolOutput> {
    let id = process_id(input)?;
    let timeout_secs = input
        .get("timeout_secs")
        .and_then(Value::as_u64)
        .unwrap_or(30)
        .clamp(1, 3_600);
    let pattern = input
        .get("pattern")
        .and_then(Value::as_str)
        .map(|pattern| {
            Regex::new(pattern).map_err(|error| anyhow!("invalid wait pattern: {error}"))
        })
        .transpose()?;
    let process = get_process(context, id).await?;
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let started = Instant::now();
    loop {
        {
            let mut guard = process.lock().await;
            guard.last_access = Instant::now();
            let matched = pattern.as_ref().and_then(|pattern| {
                guard.output.lock().ok().and_then(|output| {
                    let tail = format!(
                        "{}\n{}",
                        output.stdout.tail(256 * 1024),
                        output.stderr.tail(256 * 1024)
                    );
                    pattern.is_match(&tail).then_some(tail)
                })
            });
            if let Some(tail) = matched {
                return Ok(ToolOutput::ok_structured(
                    format!("pattern matched after {}ms", started.elapsed().as_millis()),
                    json!({
                        "process_id": guard.id,
                        "matched": true,
                        "elapsed_ms": started.elapsed().as_millis() as u64,
                        "evidence": tail.chars().take(4_000).collect::<String>(),
                    }),
                ));
            }
            if guard.done {
                let tail = guard
                    .output
                    .lock()
                    .map(|output| {
                        format!("{}{}", output.stdout.tail(2_000), output.stderr.tail(2_000))
                    })
                    .unwrap_or_default();
                return Ok(ToolOutput::error_structured(
                    format!(
                        "{} exited {} before the condition matched",
                        guard.id,
                        guard.exit_code.unwrap_or(-1)
                    ),
                    json!({
                        "code": "PROCESS_EXITED",
                        "process_id": guard.id,
                        "exit_code": guard.exit_code,
                        "tail": tail,
                    }),
                ));
            }
        }
        if Instant::now() >= deadline {
            return Ok(ToolOutput::error_structured(
                format!("wait timed out after {timeout_secs}s"),
                json!({
                    "code": "WAIT_TIMEOUT",
                    "process_id": id,
                    "timeout_secs": timeout_secs,
                }),
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn stop_process(context: &ToolContext, input: &Value) -> Result<ToolOutput> {
    let id = process_id(input)?;
    let process = get_process(context, id).await?;
    let pid = {
        let mut guard = process.lock().await;
        guard.last_access = Instant::now();
        if guard.done {
            return Ok(ToolOutput::ok_structured(
                format!(
                    "{} already exited {}",
                    guard.id,
                    guard.exit_code.unwrap_or(-1)
                ),
                json!({"process_id": guard.id, "done": true, "exit_code": guard.exit_code}),
            ));
        }
        guard.pid
    };
    let sent = kill_group(pid, libc::SIGTERM);
    for _ in 0..20 {
        {
            let guard = process.lock().await;
            if guard.done {
                return Ok(ToolOutput::ok_structured(
                    format!(
                        "{} stopped with exit {}",
                        guard.id,
                        guard.exit_code.unwrap_or(-1)
                    ),
                    json!({"process_id": guard.id, "done": true, "exit_code": guard.exit_code, "sent": sent}),
                ));
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    kill_group(pid, libc::SIGKILL);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let guard = process.lock().await;
    Ok(ToolOutput::ok_structured(
        format!("{} killed", guard.id),
        json!({"process_id": guard.id, "done": guard.done, "exit_code": guard.exit_code, "sent": sent, "forced": true}),
    ))
}

fn process_id(input: &Value) -> Result<&str> {
    input
        .get("process_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| anyhow!("process_id is required"))
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

#[async_trait]
impl AgentTool for WaitFor {
    fn name(&self) -> &'static str {
        "wait_for"
    }

    fn description(&self) -> &'static str {
        "Wait deterministically for a local TCP port, a localhost URL, or a process log pattern. No sleep guessing."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "port": {"type": "integer", "minimum": 1, "maximum": 65535, "description": "Local TCP port"},
                "url": {"type": "string", "description": "Localhost http(s) URL"},
                "process_id": {"type": "string", "description": "Process to inspect for a log pattern"},
                "pattern": {"type": "string", "description": "Regex pattern when process_id is used"},
                "timeout_secs": {"type": "integer", "minimum": 1, "maximum": 3600, "description": "Default 30"},
                "interval_ms": {"type": "integer", "minimum": 50, "maximum": 5000, "description": "Default 200"}
            }
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let timeout_secs = input
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(30)
            .clamp(1, 3_600);
        let interval = input
            .get("interval_ms")
            .and_then(Value::as_u64)
            .unwrap_or(200)
            .clamp(50, 5_000);
        let port = input
            .get("port")
            .and_then(Value::as_u64)
            .map(|port| port as u16);
        let url = input.get("url").and_then(Value::as_str);
        let process_id = input.get("process_id").and_then(Value::as_str);
        let pattern = input.get("pattern").and_then(Value::as_str);
        if port.is_none() && url.is_none() && process_id.is_none() {
            return Ok(ToolOutput::error_structured(
                "wait_for requires port, url, or process_id with pattern",
                json!({"code": "INVALID_ARGUMENT"}),
            ));
        }
        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
        let started = Instant::now();
        let mut last_error = String::new();
        loop {
            if let Some(port) = port {
                match tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
                    Ok(_) => {
                        return Ok(ToolOutput::ok_structured(
                            format!("port {port} is accepting connections"),
                            json!({
                                "ready": true,
                                "kind": "port",
                                "port": port,
                                "elapsed_ms": started.elapsed().as_millis() as u64,
                            }),
                        ));
                    }
                    Err(error) => last_error = error.to_string(),
                }
            }
            if let Some(url) = url {
                if !is_local_url(url) {
                    return Ok(ToolOutput::error_structured(
                        "wait_for url must be localhost, 127.0.0.1, or [::1]",
                        json!({"code": "NETWORK_DENIED", "url": url}),
                    ));
                }
                match reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(Duration::from_secs(2))
                    .build()
                {
                    Ok(client) => match client.get(url).send().await {
                        Ok(response) => {
                            let status = response.status().as_u16();
                            return Ok(ToolOutput::ok_structured(
                                format!("{url} responded {status}"),
                                json!({
                                    "ready": true,
                                    "kind": "url",
                                    "url": url,
                                    "status": status,
                                    "elapsed_ms": started.elapsed().as_millis() as u64,
                                }),
                            ));
                        }
                        Err(error) => last_error = error.to_string(),
                    },
                    Err(error) => last_error = error.to_string(),
                }
            }
            if let (Some(process_id), Some(pattern)) = (process_id, pattern) {
                let regex = Regex::new(pattern)
                    .map_err(|error| anyhow!("invalid wait pattern: {error}"))?;
                let process = get_process(context, process_id).await?;
                let guard = process.lock().await;
                let matched = guard.output.lock().ok().and_then(|output| {
                    let tail = format!(
                        "{}\n{}",
                        output.stdout.tail(256 * 1024),
                        output.stderr.tail(256 * 1024)
                    );
                    regex.is_match(&tail).then_some(tail)
                });
                if let Some(tail) = matched {
                    return Ok(ToolOutput::ok_structured(
                        format!(
                            "process pattern matched after {}ms",
                            started.elapsed().as_millis()
                        ),
                        json!({
                            "ready": true,
                            "kind": "process_pattern",
                            "process_id": process_id,
                            "elapsed_ms": started.elapsed().as_millis() as u64,
                            "evidence": tail.chars().take(4_000).collect::<String>(),
                        }),
                    ));
                }
                if guard.done {
                    return Ok(ToolOutput::error_structured(
                        format!(
                            "{} exited {} before the pattern matched",
                            guard.id,
                            guard.exit_code.unwrap_or(-1)
                        ),
                        json!({
                            "code": "PROCESS_EXITED",
                            "process_id": guard.id,
                            "exit_code": guard.exit_code,
                        }),
                    ));
                }
            }
            if Instant::now() >= deadline {
                return Ok(ToolOutput::error_structured(
                    format!("wait_for timed out after {timeout_secs}s"),
                    json!({
                        "code": "WAIT_TIMEOUT",
                        "timeout_secs": timeout_secs,
                        "last_error": last_error,
                    }),
                ));
            }
            tokio::time::sleep(Duration::from_millis(interval)).await;
        }
    }
}

fn is_local_url(url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(url) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    matches!(
        host.trim_matches(['[', ']']).to_ascii_lowercase().as_str(),
        "localhost" | "127.0.0.1" | "::1"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn context(root: &Path) -> ToolContext {
        ToolContext {
            root: root.to_path_buf(),
            project_root: root.to_path_buf(),
            sandbox_root: root.join(".sandbox"),
            secret_roots: Vec::new(),
            network_allowed: true,
            home: root.join("home"),
            restrict_home: false,
            read_jail: false,
            git_branch: None,
        }
    }

    #[tokio::test]
    async fn starts_reads_and_stops_a_process() {
        let dir = TempDir::new().unwrap();
        let context = context(dir.path());
        let started = ProcessTool
            .execute(
                &context,
                json!({"action": "start", "command": "printf 'ready\\n'; sleep 30"}),
            )
            .await
            .unwrap();
        assert!(!started.is_error, "{}", started.content);
        let id = started.structured.unwrap()["process_id"]
            .as_str()
            .unwrap()
            .to_string();
        let waited = WaitFor
            .execute(
                &context,
                json!({"process_id": id, "pattern": "ready", "timeout_secs": 5}),
            )
            .await
            .unwrap();
        assert!(!waited.is_error, "{}", waited.content);
        let read = ProcessTool
            .execute(&context, json!({"action": "read", "process_id": id}))
            .await
            .unwrap();
        assert!(!read.is_error, "{}", read.content);
        assert!(read.content.contains("ready"));
        let stopped = ProcessTool
            .execute(&context, json!({"action": "stop", "process_id": id}))
            .await
            .unwrap();
        assert!(!stopped.is_error, "{}", stopped.content);
    }

    #[tokio::test]
    async fn rejects_cwd_escape_and_unknown_processes() {
        let dir = TempDir::new().unwrap();
        let context = context(dir.path());
        let escape = ProcessTool
            .execute(
                &context,
                json!({"action": "start", "command": "true", "cwd": "../"}),
            )
            .await;
        assert!(escape.is_err());
        let unknown = ProcessTool
            .execute(
                &context,
                json!({"action": "status", "process_id": "proc_missing"}),
            )
            .await;
        assert!(unknown.is_err());
    }
}

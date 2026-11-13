//! Bounded WKWebView browser helper hosted by the daemon.
//!
//! The browser is a separate Apple-signed-platform helper process, not an agent
//! shell command. The daemon launches it under a dedicated Seatbelt profile
//! that permits normal reads and localhost network access but only allows
//! writes under a runtime scratch directory. The helper uses a non-persistent
//! WebKit data store, refuses downloads and new windows, and only navigates to
//! localhost HTTP(S) or worktree file URLs.
//!
//! The `ComputerLease` row serializes this surface across sessions. It is not a
//! worktree write lease: the browser never writes into the worktree; screenshots
//! are written by the daemon through the normal path jail and atomic-write
//! helper.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::timeout;
use uuid::Uuid;

use crate::Engine;
use crate::gui::{self, GuiAcquire, GuiActionOutcome, GuiSurface};
use crate::tooling::fs::{atomic_write, sha256_hex};
use crate::tools::{ToolOutput, truncate};

const HELPER_BINARY: &str = "VerlinParkBrowserHelper";
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
const DEFAULT_HELPER_IDLE_SECS: u64 = 600;
const MAX_SCREENSHOT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_NAVIGATION_URL_BYTES: usize = 4_096;
const MAX_EVALUATE_BYTES: usize = 64 * 1024;
const ACTION_TIMEOUT: Duration = Duration::from_secs(75);
const READY_TIMEOUT: Duration = Duration::from_secs(15);

/// Browser writes are allowed only under SCRATCH. The profile is deliberately
/// narrow: no daemon paths, no worktree writes, and no Apple event delivery.
const BROWSER_SANDBOX_PROFILE: &str = r#"(version 1)
(allow default)
(allow network*)
(allow network-outbound)
(allow network-inbound)
(allow mach*)
(allow ipc*)
(deny file-write* (require-not (require-any
  (subpath (param "SCRATCH"))
  (literal "/dev/null")
  (literal "/dev/zero")
)))
(deny file-write* (subpath (param "WORKTREE")))
(deny appleevent-send)
"#;

/// Browser helper availability. Unlike the computer status path this never
/// spawns a helper; the browser helper has no TCC permission prompt of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserHelperStatus {
    pub platform_supported: bool,
    pub helper_available: bool,
    pub helper_path: Option<String>,
    pub sandbox_available: bool,
    pub warm_helpers: usize,
    pub error: Option<String>,
}

/// An action requested by an agent. Kept separate from JSON parsing so the
/// same target can be reused by future GUI surfaces.
pub struct BrowserTarget<'a> {
    pub session_id: &'a str,
    pub worktree: &'a Path,
    pub agent_id: &'a str,
    pub agent_name: &'a str,
}

#[derive(Debug, Clone)]
struct BrowserProtocolError {
    code: String,
    message: String,
}

impl std::fmt::Display for BrowserProtocolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for BrowserProtocolError {}

struct BrowserHelper {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    scratch: PathBuf,
    next_id: u64,
    last_used: Arc<AtomicU64>,
}

impl BrowserHelper {
    async fn spawn(helper_path: &Path, scratch: &Path, worktree: &Path) -> Result<Self> {
        if !Path::new(SANDBOX_EXEC).is_file() {
            bail!("browser helper sandbox is unavailable; refusing to run unsandboxed");
        }
        tokio::fs::create_dir_all(scratch.join("home")).await?;
        tokio::fs::create_dir_all(scratch.join("tmp")).await?;
        tokio::fs::create_dir_all(scratch.join("cache")).await?;
        tokio::fs::create_dir_all(scratch.join("screenshots")).await?;
        let scratch = scratch
            .canonicalize()
            .with_context(|| format!("resolving browser scratch {}", scratch.display()))?;
        let worktree = worktree
            .canonicalize()
            .with_context(|| format!("resolving browser worktree {}", worktree.display()))?;
        let mut command = Command::new(SANDBOX_EXEC);
        command
            .arg("-D")
            .arg(format!("SCRATCH={}", scratch.display()))
            .arg("-D")
            .arg(format!("WORKTREE={}", worktree.display()))
            .arg("-p")
            .arg(BROWSER_SANDBOX_PROFILE)
            .arg(helper_path)
            .arg("--scratch")
            .arg(&scratch)
            .arg("--worktree")
            .arg(&worktree)
            .current_dir(&worktree)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOME", scratch.join("home"))
            .env("TMPDIR", scratch.join("tmp"))
            .env("TMP", scratch.join("tmp"))
            .env("TEMP", scratch.join("tmp"))
            .env("CFFIXED_USER_HOME", scratch.join("home"))
            .env("XDG_CACHE_HOME", scratch.join("cache"))
            .env("LANG", "en_US.UTF-8")
            .env("LC_ALL", "en_US.UTF-8")
            .env("VERLINPARK_BROWSER_HELPER_SANDBOX", "seatbelt");
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command
            .spawn()
            .with_context(|| format!("starting browser helper {}", helper_path.display()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("browser helper has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("browser helper has no stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("browser helper has no stderr"))?;
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "vp_core::browser", "{}", line);
            }
        });
        let mut helper = Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            scratch,
            next_id: 1,
            last_used: Arc::new(AtomicU64::new(unix_now())),
        };
        let mut ready = String::new();
        let read = timeout(READY_TIMEOUT, helper.stdout.read_line(&mut ready)).await;
        let ready_ok = matches!(read, Ok(Ok(count)) if count > 0)
            && serde_json::from_str::<Value>(ready.trim())
                .ok()
                .and_then(|value| {
                    value
                        .get("event")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .as_deref()
                == Some("ready");
        if !ready_ok {
            let mut stderr = String::new();
            if let Ok(Ok(_)) = timeout(Duration::from_secs(1), helper.child.wait()).await {
                stderr.push_str("helper exited during startup");
            }
            let _ = helper.child.start_kill();
            bail!("browser helper did not become ready ({stderr})");
        }
        Ok(helper)
    }

    fn touch(&self) {
        self.last_used.store(unix_now(), Ordering::SeqCst);
    }

    async fn request(&mut self, action: &str, params: Value, deadline: Duration) -> Result<Value> {
        self.touch();
        let id = self.next_id.to_string();
        self.next_id = self.next_id.saturating_add(1);
        let request = json!({"id": id, "action": action, "params": params});
        let mut line = serde_json::to_vec(&request)?;
        line.push(b'\n');
        self.stdin.write_all(&line).await?;
        self.stdin.flush().await?;
        loop {
            let mut response = String::new();
            let read = timeout(deadline, self.stdout.read_line(&mut response)).await;
            let count = match read {
                Ok(Ok(count)) => count,
                Ok(Err(error)) => return Err(anyhow!("reading browser helper response: {error}")),
                Err(_) => {
                    return Err(anyhow!(
                        "browser helper timed out after {} seconds",
                        deadline.as_secs()
                    ));
                }
            };
            if count == 0 {
                bail!("browser helper exited before responding");
            }
            let trimmed = response.trim();
            if trimmed.is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(trimmed)
                .with_context(|| format!("invalid browser helper response: {trimmed}"))?;
            if value.get("event").is_some() {
                continue;
            }
            if value.get("id").and_then(Value::as_str) != Some(id.as_str()) {
                continue;
            }
            if value.get("ok").and_then(Value::as_bool) == Some(true) {
                return Ok(value.get("result").cloned().unwrap_or(Value::Null));
            }
            let code = value
                .pointer("/error/code")
                .and_then(Value::as_str)
                .unwrap_or("BROWSER_ERROR")
                .to_string();
            let message = value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("browser helper returned an error")
                .to_string();
            return Err(BrowserProtocolError { code, message }.into());
        }
    }

    async fn shutdown(&mut self) {
        let _ = timeout(
            Duration::from_secs(2),
            self.request("close", json!({}), Duration::from_secs(2)),
        )
        .await;
        let _ = timeout(Duration::from_secs(2), self.child.wait()).await;
        let _ = self.child.start_kill();
        let _ = tokio::fs::remove_dir_all(&self.scratch).await;
    }
}

impl Drop for BrowserHelper {
    fn drop(&mut self) {
        // `kill_on_drop` handles the sandbox-exec child. WebKit's XPC services
        // are owned by that helper and exit when it exits; the lease is left to
        // its TTL if the manager did not run the explicit shutdown path.
    }
}

#[derive(Debug, Clone, Default)]
struct BrowserSnapshotState {
    generation: u64,
    valid: bool,
    tree: Option<Value>,
    text: String,
}

pub struct BrowserManager {
    helpers: Mutex<HashMap<String, Arc<AsyncMutex<BrowserHelper>>>>,
    snapshots: Mutex<HashMap<String, BrowserSnapshotState>>,
    spawn_lock: AsyncMutex<()>,
    data_dir: PathBuf,
    #[cfg(test)]
    helper_override: Option<PathBuf>,
}

impl BrowserManager {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            helpers: Mutex::new(HashMap::new()),
            snapshots: Mutex::new(HashMap::new()),
            spawn_lock: AsyncMutex::new(()),
            data_dir,
            #[cfg(test)]
            helper_override: None,
        }
    }

    #[cfg(test)]
    fn with_helper(data_dir: PathBuf, helper: PathBuf) -> Self {
        Self {
            helpers: Mutex::new(HashMap::new()),
            snapshots: Mutex::new(HashMap::new()),
            spawn_lock: AsyncMutex::new(()),
            data_dir,
            helper_override: Some(helper),
        }
    }

    fn snapshot_generation(&self, key: &str) -> u64 {
        self.snapshots
            .lock()
            .ok()
            .and_then(|snapshots| snapshots.get(key).map(|state| state.generation))
            .unwrap_or(0)
    }

    fn invalidate_snapshot(&self, key: &str) {
        if let Ok(mut snapshots) = self.snapshots.lock()
            && let Some(state) = snapshots.get_mut(key)
        {
            state.generation = state.generation.saturating_add(1);
            state.valid = false;
            state.tree = None;
            state.text.clear();
        }
    }

    fn next_snapshot(&self, key: &str) -> (u64, Option<(u64, Option<Value>, String)>) {
        let mut snapshots = self.snapshots.lock().ok();
        let state = snapshots
            .as_mut()
            .map(|map| map.entry(key.to_string()).or_default());
        let Some(state) = state else {
            return (0, None);
        };
        let previous = state
            .valid
            .then(|| (state.generation, state.tree.clone(), state.text.clone()));
        state.generation = state.generation.saturating_add(1);
        state.valid = false;
        (state.generation, previous)
    }

    fn store_snapshot(&self, key: &str, generation: u64, tree: Option<Value>, text: String) {
        if let Ok(mut snapshots) = self.snapshots.lock() {
            snapshots.insert(
                key.to_string(),
                BrowserSnapshotState {
                    generation,
                    valid: tree.is_some(),
                    tree,
                    text,
                },
            );
        }
    }

    fn helper_path(&self) -> Result<PathBuf> {
        #[cfg(test)]
        if let Some(path) = &self.helper_override {
            return Ok(path.clone());
        }
        find_helper_binary()
    }

    pub async fn action(
        &self,
        engine: &Engine,
        target: &BrowserTarget<'_>,
        input: Value,
    ) -> Result<ToolOutput> {
        let action = input
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("snapshot")
            .trim()
            .to_ascii_lowercase();
        if !matches!(
            action.as_str(),
            "open"
                | "snapshot"
                | "click"
                | "type"
                | "press"
                | "select"
                | "wait_for"
                | "screenshot"
                | "console"
                | "evaluate"
                | "close"
        ) {
            return Ok(ToolOutput::error_structured(
                format!("unsupported browser action: {action}"),
                json!({"code": "INVALID_ARGUMENT", "action": action}),
            ));
        }
        let key = helper_key(target);
        if action == "close" {
            self.invalidate_snapshot(&key);
            let helper = self
                .helpers
                .lock()
                .ok()
                .and_then(|mut map| map.remove(&key));
            let had_helper = helper.is_some();
            if let Some(helper) = helper {
                {
                    let mut helper = helper.lock().await;
                    helper.shutdown().await;
                }
                let scratch = self.helper_scratch(target);
                let _ = tokio::fs::remove_dir_all(&scratch).await;
                if let Some(parent) = scratch.parent() {
                    let _ = tokio::fs::remove_dir(parent).await;
                }
            }
            let released = engine.gui_end_session(target.session_id, "close").await;
            return Ok(if had_helper {
                ToolOutput::ok_structured(
                    "browser surface closed",
                    json!({"action": "close", "closed": true, "lease_released": released}),
                )
            } else {
                ToolOutput::ok_structured(
                    "no browser surface was open",
                    json!({"action": "close", "closed": false, "lease_released": released}),
                )
            });
        }

        let params = match self.params_for(&action, target, &input) {
            Ok(params) => params,
            Err(error) => {
                return Ok(ToolOutput::error_structured(
                    format!("browser {action} rejected: {error}"),
                    json!({
                        "code": "INVALID_ARGUMENT",
                        "action": action,
                        "message": error.to_string(),
                    }),
                ));
            }
        };
        if matches!(action.as_str(), "click" | "type" | "press" | "select")
            && let Some(requested) = input.get("snapshot_generation").and_then(Value::as_u64)
        {
            let current = self.snapshot_generation(&key);
            if snapshot_generation_is_stale(requested, current) {
                return Ok(ToolOutput::error_structured(
                    format!(
                        "browser {action} rejected: snapshot generation {requested} is stale; current generation is {current}"
                    ),
                    json!({
                        "code": "STALE_REF",
                        "action": action,
                        "retryable": true,
                        "auto_retry": false,
                        "fresh_snapshot_required": true,
                        "snapshot_generation": current,
                        "provided_generation": requested,
                        "hint": "take a fresh browser snapshot and use its generation and refs",
                    }),
                ));
            }
        }
        let ticket = match engine
            .gui_begin_action(
                GuiSurface::Browser,
                target.session_id,
                target.agent_id,
                target.agent_name,
            )
            .await
        {
            Ok(GuiAcquire::Acquired(ticket)) => ticket,
            Ok(GuiAcquire::Busy(busy)) => {
                return Ok(ToolOutput::error_structured(
                    busy.message.clone(),
                    busy.to_structured(&action),
                ));
            }
            Err(error) => {
                return Ok(ToolOutput::error_structured(
                    format!("browser {action} unavailable: {error}"),
                    json!({
                        "code": "GUI_LEASE_FAILED",
                        "action": action,
                        "message": error.to_string(),
                        "lease_surface": gui::GUI_SURFACE,
                    }),
                ));
            }
        };
        let mut outcome = GuiActionOutcome::new(action.clone());
        let helper = match self.ensure_helper(target, &key).await {
            Ok(helper) => helper,
            Err(error) => {
                let message = error.to_string();
                let code = if message.contains("is not installed") {
                    "BROWSER_HELPER_UNAVAILABLE"
                } else if message.contains("sandbox is unavailable") {
                    "BROWSER_SANDBOX_UNAVAILABLE"
                } else if message.contains("did not become ready") {
                    "BROWSER_HELPER_START_FAILED"
                } else {
                    "BROWSER_HELPER_FAILED"
                };
                outcome.result = "error".to_string();
                outcome.error_code = Some(code.to_string());
                engine.gui_finish_action(&ticket, outcome).await;
                return Ok(ToolOutput::error_structured(
                    format!("browser {action} unavailable: {message}"),
                    json!({
                        "code": code,
                        "action": action,
                        "message": message,
                        "retryable": false,
                        "lease_surface": gui::GUI_SURFACE,
                    }),
                ));
            }
        };
        let deadline = action_deadline(&action, &input);
        let started = std::time::Instant::now();
        let token = crate::preemption::current_token();
        let mut preempted = None;
        let result = {
            let mut helper = helper.lock().await;
            match token {
                Some(token) => {
                    tokio::select! {
                        _ = token.cancelled() => {
                            helper.shutdown().await;
                            preempted = Some(token.snapshot().unwrap_or_else(|| {
                                crate::preemption::fallback_snapshot(
                                    crate::preemption::Phase::Tool,
                                    Some("browser"),
                                )
                            }));
                            Err(anyhow::anyhow!("browser action was preempted"))
                        }
                        result = helper.request(&action, params, deadline) => result,
                    }
                }
                None => helper.request(&action, params, deadline).await,
            }
        };
        if let Some(snapshot) = preempted {
            self.invalidate_snapshot(&key);
            outcome.latency_ms = started.elapsed().as_millis() as u64;
            outcome.result = "preempted".to_string();
            outcome.error_code = Some("PREEMPTED".to_string());
            engine.gui_finish_action(&ticket, outcome).await;
            engine.gui_end_session(target.session_id, "preempted").await;
            return Ok(crate::preemption::tool_output("browser", &snapshot));
        }
        outcome.latency_ms = started.elapsed().as_millis() as u64;
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                let protocol_error = error.downcast_ref::<BrowserProtocolError>();
                let code = protocol_error
                    .map(|error| error.code.clone())
                    .unwrap_or_else(|| "BROWSER_ERROR".to_string());
                let keep_helper = protocol_error.is_some();
                if !keep_helper {
                    // A timed-out or crashed helper is removed so the next
                    // action starts from a clean process. Script or element
                    // errors keep the page alive for a retry.
                    let removed = self
                        .helpers
                        .lock()
                        .ok()
                        .and_then(|mut map| map.remove(&key));
                    if let Some(helper) = removed {
                        let mut helper = helper.lock().await;
                        helper.shutdown().await;
                    }
                    self.invalidate_snapshot(&key);
                }
                outcome.result = "error".to_string();
                outcome.error_code = Some(code.clone());
                outcome.target_window = non_empty(string_param(&input, "title", 200));
                engine.gui_finish_action(&ticket, outcome).await;
                return Ok(ToolOutput::error_structured(
                    format!("browser {action} failed: {error}"),
                    json!({
                        "code": code,
                        "action": action,
                        "message": error.to_string(),
                        "retryable": false,
                        "auto_retry": false,
                        "lease_surface": gui::GUI_SURFACE,
                    }),
                ));
            }
        };
        let output = match self.render(action.clone(), &result, target, &input).await {
            Ok(output) => output,
            Err(error) => {
                outcome.result = "error".to_string();
                outcome.error_code = Some("BROWSER_RENDER_FAILED".to_string());
                engine.gui_finish_action(&ticket, outcome).await;
                return Ok(ToolOutput::error_structured(
                    format!("browser {action} failed: {error}"),
                    json!({
                        "code": "BROWSER_RENDER_FAILED",
                        "action": action,
                        "message": error.to_string(),
                        "retryable": false,
                    }),
                ));
            }
        };
        outcome.target_window = result
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| non_empty(string_param(&input, "title", 200)));
        outcome.result = if output.is_error { "error" } else { "ok" }.to_string();
        outcome.error_code = output
            .structured
            .as_ref()
            .and_then(|value| value.get("code"))
            .and_then(Value::as_str)
            .map(str::to_string);
        engine.gui_finish_action(&ticket, outcome).await;
        Ok(output)
    }

    async fn ensure_helper(
        &self,
        target: &BrowserTarget<'_>,
        key: &str,
    ) -> Result<Arc<AsyncMutex<BrowserHelper>>> {
        if let Some(helper) = self.existing_helper(key).await {
            return Ok(helper);
        }
        let _spawn = self.spawn_lock.lock().await;
        if let Some(helper) = self.existing_helper(key).await {
            return Ok(helper);
        }
        let helper_path = self.helper_path()?;
        if !Path::new(SANDBOX_EXEC).is_file() {
            bail!("browser helper sandbox is unavailable on this platform");
        }
        let scratch = self.helper_scratch(target);
        let helper = match BrowserHelper::spawn(&helper_path, &scratch, target.worktree).await {
            Ok(helper) => helper,
            Err(error) => {
                let _ = tokio::fs::remove_dir_all(&scratch).await;
                return Err(error);
            }
        };
        let helper = Arc::new(AsyncMutex::new(helper));
        if let Ok(mut map) = self.helpers.lock() {
            map.insert(key.to_string(), helper.clone());
        }
        Ok(helper)
    }

    /// Warm helpers survive GUI lease release. A crashed child is removed so
    /// the next action spawns a fresh process and reacquires the lease.
    async fn existing_helper(&self, key: &str) -> Option<Arc<AsyncMutex<BrowserHelper>>> {
        let helper = self
            .helpers
            .lock()
            .ok()
            .and_then(|map| map.get(key).cloned())?;
        let alive = {
            let mut helper = helper.lock().await;
            helper.touch();
            helper.child.try_wait().ok().flatten().is_none()
        };
        if alive {
            return Some(helper);
        }
        let stale = self.helpers.lock().ok().and_then(|mut map| map.remove(key));
        if let Some(stale) = stale {
            let mut helper = stale.lock().await;
            helper.shutdown().await;
            self.invalidate_snapshot(key);
        }
        None
    }

    pub fn warm_helpers(&self) -> usize {
        self.helpers.lock().map(|map| map.len()).unwrap_or(0)
    }

    /// Cached-free inspection of helper installation and sandbox availability.
    /// This never spawns a helper.
    pub fn status(&self) -> BrowserHelperStatus {
        let helper = self.helper_path();
        BrowserHelperStatus {
            platform_supported: cfg!(target_os = "macos"),
            helper_available: helper.is_ok(),
            helper_path: helper.ok().map(|path| path.to_string_lossy().to_string()),
            sandbox_available: Path::new(SANDBOX_EXEC).is_file(),
            warm_helpers: self.warm_helpers(),
            error: if cfg!(target_os = "macos") {
                None
            } else {
                Some("browser helper requires macOS".to_string())
            },
        }
    }

    fn helper_scratch(&self, target: &BrowserTarget<'_>) -> PathBuf {
        let digest = sha256_hex(target.worktree.to_string_lossy().as_bytes());
        self.data_dir
            .join("sandbox")
            .join(target.session_id)
            .join("browser-helper")
            .join(&digest[..16])
    }

    fn params_for(&self, action: &str, target: &BrowserTarget<'_>, input: &Value) -> Result<Value> {
        match action {
            "open" => {
                let raw = input
                    .get("url")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("url is required for browser open"))?;
                let url = validate_navigation_url(raw, target.worktree)?;
                Ok(json!({"url": url}))
            }
            "snapshot" => Ok(json!({
                "max_nodes": input.get("max_nodes").and_then(Value::as_u64).unwrap_or(800).clamp(1, 1_200),
                "max_text_chars": input.get("max_chars").and_then(Value::as_u64).unwrap_or(12_000).clamp(200, 24_000),
            })),
            "click" | "type" | "press" | "select" => {
                let mut params = json!({
                    "ref": string_param(input, "ref", 80),
                    "selector": string_param(input, "selector", 500),
                });
                match action {
                    "type" => {
                        let text = input
                            .get("text")
                            .and_then(Value::as_str)
                            .ok_or_else(|| anyhow!("text is required for browser type"))?;
                        if text.len() > 100_000 {
                            bail!("browser type text exceeds 100 KiB");
                        }
                        params["text"] = json!(text);
                        params["clear"] =
                            json!(input.get("clear").and_then(Value::as_bool).unwrap_or(true));
                    }
                    "press" => {
                        let key = input
                            .get("key")
                            .and_then(Value::as_str)
                            .ok_or_else(|| anyhow!("key is required for browser press"))?;
                        if key.len() > 40 {
                            bail!("browser key is too long");
                        }
                        params["key"] = json!(key);
                    }
                    "select" => {
                        let value = input
                            .get("value")
                            .and_then(Value::as_str)
                            .ok_or_else(|| anyhow!("value is required for browser select"))?;
                        if value.len() > 10_000 {
                            bail!("browser select value is too long");
                        }
                        params["value"] = json!(value);
                    }
                    _ => {}
                }
                Ok(params)
            }
            "wait_for" => {
                let condition = input
                    .get("condition")
                    .and_then(Value::as_str)
                    .unwrap_or("selector")
                    .to_ascii_lowercase();
                if !matches!(condition.as_str(), "selector" | "text" | "url") {
                    bail!("browser wait condition must be selector, text, or url");
                }
                let value = input
                    .get("value")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("value is required for browser wait_for"))?;
                if value.len() > 2_000 {
                    bail!("browser wait value is too long");
                }
                Ok(json!({
                    "condition": condition,
                    "value": value,
                    "timeout_ms": input
                        .get("timeout_ms")
                        .and_then(Value::as_u64)
                        .unwrap_or(5_000)
                        .clamp(50, 60_000),
                }))
            }
            "screenshot" => {
                if let Some(path) = input.get("path").and_then(Value::as_str)
                    && !path.trim().is_empty()
                {
                    safe_relative_path(path)?;
                }
                Ok(json!({
                    "full_page": input.get("full_page").and_then(Value::as_bool).unwrap_or(false),
                }))
            }
            "console" => Ok(json!({
                "clear": input.get("clear").and_then(Value::as_bool).unwrap_or(false),
            })),
            "evaluate" => {
                let script = input
                    .get("script")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("script is required for browser evaluate"))?;
                if script.is_empty() {
                    bail!("browser evaluate script is empty");
                }
                if script.len() > MAX_EVALUATE_BYTES {
                    bail!("browser evaluate script exceeds 64 KiB");
                }
                Ok(json!({"script": script}))
            }
            _ => Ok(json!({})),
        }
    }

    async fn render(
        &self,
        action: String,
        result: &Value,
        target: &BrowserTarget<'_>,
        input: &Value,
    ) -> Result<ToolOutput> {
        match action.as_str() {
            "open" => {
                let url = result.get("url").and_then(Value::as_str).unwrap_or("?");
                let title = result.get("title").and_then(Value::as_str).unwrap_or("");
                let summary = if title.is_empty() {
                    format!("opened {url}")
                } else {
                    format!("opened {url} — {title}")
                };
                Ok(ToolOutput::ok_structured(
                    summary.clone(),
                    json!({
                        "ok": true,
                        "action": "open",
                        "summary": summary,
                        "url": url,
                        "title": title,
                        "next_actions": ["browser snapshot"],
                    }),
                ))
            }
            "snapshot" => {
                let key = helper_key(target);
                let (generation, previous) = self.next_snapshot(&key);
                let url = result.get("url").and_then(Value::as_str).unwrap_or("");
                let title = result.get("title").and_then(Value::as_str).unwrap_or("");
                let tree = result.get("tree").cloned();
                let text = result
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let max_chars = input
                    .get("max_chars")
                    .and_then(Value::as_u64)
                    .unwrap_or(12_000) as usize;
                let delta_requested = input.get("delta").and_then(Value::as_bool).unwrap_or(true);
                let delta = if delta_requested {
                    previous
                        .as_ref()
                        .map(|(base_generation, previous_tree, previous_text)| {
                            snapshot_delta(
                                *base_generation,
                                generation,
                                previous_tree.as_ref(),
                                previous_text,
                                &text,
                                tree.as_ref(),
                            )
                        })
                } else {
                    None
                };
                self.store_snapshot(&key, generation, tree.clone(), text.clone());
                if let Some(delta) = delta {
                    let summary = delta_summary(&delta, generation, url, title);
                    let content =
                        truncate(summary.chars().take(max_chars.max(500)).collect::<String>());
                    let mut structured = delta;
                    structured["ok"] = json!(true);
                    structured["action"] = json!("snapshot");
                    structured["url"] = json!(url);
                    structured["title"] = json!(title);
                    structured["node_count"] =
                        result.get("node_count").cloned().unwrap_or(Value::Null);
                    structured["truncated"] = result
                        .get("truncated")
                        .cloned()
                        .unwrap_or(Value::Bool(false));
                    return Ok(ToolOutput::ok_structured(content, structured));
                }
                let mut text_output = String::new();
                text_output.push_str(&format!("{title} ({url}) snapshot {generation}\n"));
                if let Some(tree) = tree.as_ref() {
                    render_tree(tree, 0, &mut text_output);
                }
                if !text.is_empty() {
                    text_output.push_str("\nVisible text:\n");
                    text_output.push_str(&text);
                }
                let content = truncate(
                    text_output
                        .chars()
                        .take(max_chars.max(500))
                        .collect::<String>(),
                );
                Ok(ToolOutput::ok_structured(
                    content,
                    json!({
                        "ok": true,
                        "action": "snapshot",
                        "delta": false,
                        "base_generation": Value::Null,
                        "generation": generation,
                        "url": url,
                        "title": title,
                        "node_count": result.get("node_count").cloned().unwrap_or(Value::Null),
                        "truncated": result.get("truncated").cloned().unwrap_or(Value::Bool(false)),
                        "tree": result.get("tree").cloned().unwrap_or(Value::Null),
                        "text": result.get("text").cloned().unwrap_or(Value::Null),
                    }),
                ))
            }
            "click" | "type" | "press" | "select" => {
                let summary = result
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                    .map(|name| format!("{action} {name}"))
                    .unwrap_or_else(|| format!("{action} completed"));
                Ok(ToolOutput::ok_structured(
                    summary.clone(),
                    json!({
                        "ok": true,
                        "action": action,
                        "summary": summary,
                        "result": result,
                        "next_actions": ["browser snapshot"],
                    }),
                ))
            }
            "wait_for" => {
                let summary = format!("browser wait_for matched: {result}");
                Ok(ToolOutput::ok_structured(
                    truncate(summary),
                    json!({"ok": true, "action": "wait_for", "result": result}),
                ))
            }
            "screenshot" => self.render_screenshot(result, target, input).await,
            "console" => {
                let entries = result
                    .get("entries")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let mut text = String::new();
                for entry in entries.iter().take(200) {
                    let level = entry.get("level").and_then(Value::as_str).unwrap_or("log");
                    let message = entry.get("text").and_then(Value::as_str).unwrap_or("");
                    text.push_str(&format!("[{level}] {message}\n"));
                }
                if text.is_empty() {
                    text.push_str("(no console entries)");
                }
                let count = result.get("count").and_then(Value::as_u64).unwrap_or(0);
                Ok(ToolOutput::ok_structured(
                    truncate(text),
                    json!({
                        "ok": true,
                        "action": "console",
                        "count": count,
                        "dropped": result.get("dropped").cloned().unwrap_or(Value::Null),
                        "entries": entries,
                    }),
                ))
            }
            "evaluate" => {
                let value = result.get("value").cloned().unwrap_or(Value::Null);
                let rendered =
                    serde_json::to_string_pretty(&value).unwrap_or_else(|_| "null".into());
                Ok(ToolOutput::ok_structured(
                    truncate(rendered.clone()),
                    json!({"ok": true, "action": "evaluate", "value": value}),
                ))
            }
            _ => Ok(ToolOutput::ok_structured(
                format!("browser {action} completed"),
                json!({"ok": true, "action": action, "result": result}),
            )),
        }
    }

    async fn render_screenshot(
        &self,
        result: &Value,
        target: &BrowserTarget<'_>,
        input: &Value,
    ) -> Result<ToolOutput> {
        let source = result
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("browser helper did not return a screenshot path"))?;
        let relative = input
            .get("path")
            .and_then(Value::as_str)
            .filter(|path| !path.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                format!(
                    ".verlinpark/browser/screenshot-{}.png",
                    &Uuid::now_v7().simple().to_string()[..16]
                )
            });
        let relative = safe_relative_path(&relative)?;
        let root = target
            .worktree
            .canonicalize()
            .with_context(|| format!("resolving worktree {}", target.worktree.display()))?;
        let declared = root.join(&relative);
        let parent = declared
            .parent()
            .ok_or_else(|| anyhow!("screenshot path has no parent directory"))?;
        tokio::fs::create_dir_all(parent).await?;
        let canonical_parent = parent
            .canonicalize()
            .with_context(|| format!("resolving screenshot directory {}", parent.display()))?;
        if !canonical_parent.starts_with(&root) {
            bail!("screenshot path escapes the worktree through a symlink");
        }
        let target_path = canonical_parent.join(
            declared
                .file_name()
                .ok_or_else(|| anyhow!("screenshot path has no file name"))?,
        );
        let source = PathBuf::from(source);
        let source = source
            .canonicalize()
            .with_context(|| format!("resolving browser screenshot {}", source.display()))?;
        // The helper's screenshot must be inside its own scratch directory. The
        // manager does not trust a path supplied by page script or the helper.
        let helper_scratch = self.helper_scratch(target);
        let helper_scratch = helper_scratch
            .canonicalize()
            .unwrap_or_else(|_| helper_scratch.clone());
        if !source.starts_with(&helper_scratch) {
            bail!("browser screenshot path escaped the helper scratch directory");
        }
        let metadata = tokio::fs::metadata(&source).await?;
        if metadata.len() > MAX_SCREENSHOT_BYTES {
            bail!(
                "browser screenshot exceeds {} MiB",
                MAX_SCREENSHOT_BYTES / 1024 / 1024
            );
        }
        let bytes = tokio::fs::read(&source).await?;
        let write_path = target_path.clone();
        let write_bytes = bytes.clone();
        tokio::task::spawn_blocking(move || atomic_write(&write_path, &write_bytes)).await??;
        let _ = tokio::fs::remove_file(&source).await;
        let sha = sha256_hex(&bytes);
        let relative_text = relative.to_string_lossy().replace('\\', "/");
        let width = result.get("width").and_then(Value::as_u64).unwrap_or(0);
        let height = result.get("height").and_then(Value::as_u64).unwrap_or(0);
        let summary = format!(
            "screenshot saved to {relative_text} ({width}x{height}, {} bytes)",
            bytes.len()
        );
        Ok(ToolOutput::ok_structured(
            summary.clone(),
            json!({
                "ok": true,
                "action": "screenshot",
                "summary": summary,
                "path": relative_text,
                "bytes": bytes.len(),
                "sha256": sha,
                "width": width,
                "height": height,
                "full_page": result.get("full_page").cloned().unwrap_or(Value::Bool(false)),
            }),
        ))
    }

    /// Stop helpers idle longer than `idle`. Called by the daemon sweeper.
    pub async fn sweep_idle(&self, idle: Duration) -> usize {
        let now = unix_now();
        let stale = {
            let Ok(mut map) = self.helpers.lock() else {
                return 0;
            };
            let keys = map
                .iter()
                .filter(|(_, helper)| {
                    let last = helper
                        .try_lock()
                        .map(|helper| helper.last_used.load(Ordering::SeqCst))
                        .unwrap_or(now);
                    now.saturating_sub(last) >= idle.as_secs()
                })
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>();
            keys.into_iter()
                .filter_map(|key| map.remove(&key).map(|helper| (key, helper)))
                .collect::<Vec<_>>()
        };
        let mut stopped = 0usize;
        for (key, helper) in stale {
            self.invalidate_snapshot(&key);
            let mut helper = helper.lock().await;
            helper.shutdown().await;
            stopped += 1;
        }
        stopped
    }

    /// Stop every helper owned by a session. Called before worktree/scratch
    /// cleanup so a child never keeps a removed directory open.
    pub async fn stop_session(&self, session_id: &str) -> usize {
        let prefix = format!("{session_id}\n");
        let stale = {
            let Ok(mut map) = self.helpers.lock() else {
                return 0;
            };
            let keys = map
                .keys()
                .filter(|key| key.starts_with(&prefix))
                .cloned()
                .collect::<Vec<_>>();
            keys.into_iter()
                .filter_map(|key| map.remove(&key).map(|helper| (key, helper)))
                .collect::<Vec<_>>()
        };
        let mut stopped = 0usize;
        for (key, helper) in stale {
            self.invalidate_snapshot(&key);
            let mut helper = helper.lock().await;
            helper.shutdown().await;
            stopped += 1;
        }
        stopped
    }

    pub fn idle_default() -> Duration {
        let seconds = std::env::var("VERLINPARK_BROWSER_IDLE_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(DEFAULT_HELPER_IDLE_SECS);
        Duration::from_secs(seconds)
    }
}

fn helper_key(target: &BrowserTarget<'_>) -> String {
    format!(
        "{}\n{}",
        target.session_id,
        target.worktree.to_string_lossy()
    )
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn non_empty(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

fn string_param(input: &Value, name: &str, max: usize) -> String {
    input
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .chars()
        .take(max)
        .collect()
}

fn action_deadline(action: &str, input: &Value) -> Duration {
    if action == "wait_for" {
        let wait = input
            .get("timeout_ms")
            .and_then(Value::as_u64)
            .unwrap_or(5_000)
            .clamp(50, 60_000);
        return Duration::from_millis(wait).saturating_add(Duration::from_secs(15));
    }
    ACTION_TIMEOUT
}

fn snapshot_generation_is_stale(requested: u64, current: u64) -> bool {
    current == 0 || requested != current
}

fn node_summary(node: &Value) -> Value {
    json!({
        "role": node.get("role").cloned().unwrap_or(Value::Null),
        "name": node.get("name").cloned().unwrap_or(Value::Null),
        "text": node.get("text").cloned().unwrap_or(Value::Null),
        "tag": node.get("tag").cloned().unwrap_or(Value::Null),
        "value": node.get("value").cloned().unwrap_or(Value::Null),
        "checked": node.get("checked").cloned().unwrap_or(Value::Null),
        "disabled": node.get("disabled").cloned().unwrap_or(Value::Null),
    })
}

fn collect_refs(node: &Value, refs: &mut std::collections::BTreeMap<String, Value>) {
    if let Some(reference) = node
        .get("ref")
        .and_then(Value::as_str)
        .filter(|reference| !reference.is_empty())
    {
        refs.insert(reference.to_string(), node_summary(node));
    }
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for child in children.iter().take(2_000) {
            collect_refs(child, refs);
        }
    }
}

fn line_delta(previous: &str, current: &str) -> Value {
    if previous == current {
        return json!({"changed": false, "removed_lines": [], "added_lines": [], "truncated": false});
    }
    let previous_lines = previous
        .lines()
        .take(4_000)
        .map(|line| line.chars().take(400).collect::<String>())
        .collect::<Vec<_>>();
    let current_lines = current
        .lines()
        .take(4_000)
        .map(|line| line.chars().take(400).collect::<String>())
        .collect::<Vec<_>>();
    let mut prefix = 0usize;
    while prefix < previous_lines.len()
        && prefix < current_lines.len()
        && previous_lines[prefix] == current_lines[prefix]
    {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < previous_lines.len().saturating_sub(prefix)
        && suffix < current_lines.len().saturating_sub(prefix)
        && previous_lines[previous_lines.len() - 1 - suffix]
            == current_lines[current_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let removed = previous_lines[prefix..previous_lines.len().saturating_sub(suffix)]
        .iter()
        .take(100)
        .cloned()
        .collect::<Vec<_>>();
    let added = current_lines[prefix..current_lines.len().saturating_sub(suffix)]
        .iter()
        .take(100)
        .cloned()
        .collect::<Vec<_>>();
    let removed_total = previous_lines.len().saturating_sub(prefix + suffix);
    let added_total = current_lines.len().saturating_sub(prefix + suffix);
    json!({
        "changed": true,
        "prefix_lines": prefix,
        "suffix_lines": suffix,
        "removed_lines": removed,
        "added_lines": added,
        "truncated": removed_total > 100 || added_total > 100 || previous.lines().count() > 4_000 || current.lines().count() > 4_000,
    })
}

fn snapshot_delta(
    base_generation: u64,
    generation: u64,
    previous_tree: Option<&Value>,
    previous_text: &str,
    current_text: &str,
    current_tree: Option<&Value>,
) -> Value {
    let mut previous_refs = std::collections::BTreeMap::new();
    if let Some(tree) = previous_tree {
        collect_refs(tree, &mut previous_refs);
    }
    let mut current_refs = std::collections::BTreeMap::new();
    if let Some(tree) = current_tree {
        collect_refs(tree, &mut current_refs);
    }
    let new_refs = current_refs
        .keys()
        .filter(|reference| !previous_refs.contains_key(*reference))
        .take(200)
        .cloned()
        .collect::<Vec<_>>();
    let removed_refs = previous_refs
        .keys()
        .filter(|reference| !current_refs.contains_key(*reference))
        .take(200)
        .cloned()
        .collect::<Vec<_>>();
    let changed_refs = current_refs
        .iter()
        .filter(|(reference, summary)| {
            previous_refs
                .get(*reference)
                .is_some_and(|previous| previous != *summary)
        })
        .map(|(reference, _)| reference.clone())
        .take(200)
        .collect::<Vec<_>>();
    json!({
        "delta": true,
        "generation": generation,
        "base_generation": base_generation,
        "new_refs": new_refs,
        "removed_refs": removed_refs,
        "changed_refs": changed_refs,
        "refs_truncated": previous_refs.len().saturating_add(current_refs.len()) > 400,
        "text_delta": line_delta(previous_text, current_text),
    })
}

fn delta_summary(delta: &Value, generation: u64, url: &str, title: &str) -> String {
    let mut text = format!("{title} ({url}) snapshot {generation} (delta)\n");
    for (label, key) in [
        ("New refs", "new_refs"),
        ("Removed refs", "removed_refs"),
        ("Changed refs", "changed_refs"),
    ] {
        let values = delta
            .get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !values.is_empty() {
            text.push_str(label);
            text.push_str(": ");
            text.push_str(
                &values
                    .iter()
                    .filter_map(Value::as_str)
                    .take(20)
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            text.push('\n');
        }
    }
    if let Some(text_delta) = delta.get("text_delta") {
        let removed = text_delta
            .get("removed_lines")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let added = text_delta
            .get("added_lines")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !removed.is_empty() || !added.is_empty() {
            text.push_str("Text changes:\n");
            for line in removed.iter().filter_map(Value::as_str).take(20) {
                text.push_str("- ");
                text.push_str(line);
                text.push('\n');
            }
            for line in added.iter().filter_map(Value::as_str).take(20) {
                text.push_str("+ ");
                text.push_str(line);
                text.push('\n');
            }
        }
    }
    text
}

fn render_tree(node: &Value, depth: usize, output: &mut String) {
    if depth > 40 || output.len() > 32 * 1024 {
        return;
    }
    let role = node
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("generic");
    let name = node.get("name").and_then(Value::as_str).unwrap_or("");
    let text = node.get("text").and_then(Value::as_str).unwrap_or("");
    let reference = node.get("ref").and_then(Value::as_str).unwrap_or("");
    let indent = "  ".repeat(depth.min(20));
    output.push_str(&indent);
    if !reference.is_empty() {
        output.push('[');
        output.push_str(reference);
        output.push_str("] ");
    }
    output.push_str(role);
    if !name.is_empty() {
        output.push_str(&format!(" \"{name}\""));
    }
    if !text.is_empty() && text != name {
        let text = text.chars().take(120).collect::<String>();
        output.push_str(&format!(" — {text}"));
    }
    if let Some(value) = node.get("value").and_then(Value::as_str)
        && !value.is_empty()
        && value != name
        && value != "***"
    {
        let value = value.chars().take(80).collect::<String>();
        output.push_str(&format!(" value=\"{value}\""));
    }
    if node.get("checked").and_then(Value::as_bool) == Some(true) {
        output.push_str(" checked");
    }
    if node.get("disabled").and_then(Value::as_bool) == Some(true) {
        output.push_str(" disabled");
    }
    output.push('\n');
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for child in children {
            render_tree(child, depth + 1, output);
        }
    }
}

/// Validate a browser navigation URL. This is the daemon-side policy; the
/// helper repeats the check as defense in depth.
pub fn validate_navigation_url(raw: &str, worktree: &Path) -> Result<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > MAX_NAVIGATION_URL_BYTES {
        bail!("browser URL is empty or too long");
    }
    if raw == "about:blank" {
        return Ok(raw.to_string());
    }
    let url = reqwest::Url::parse(raw).context("invalid browser URL")?;
    if url.username() != "" || url.password().is_some() {
        bail!("browser URL credentials are not allowed");
    }
    match url.scheme() {
        "http" | "https" => {
            let host = url
                .host_str()
                .ok_or_else(|| anyhow!("browser URL has no host"))?
                .trim_matches(['[', ']'])
                .to_ascii_lowercase();
            if !matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1") {
                bail!("browser navigation is limited to localhost, 127.0.0.1, and [::1]");
            }
            Ok(url.to_string())
        }
        "file" => {
            let path = url
                .to_file_path()
                .map_err(|_| anyhow!("invalid browser file URL"))?;
            let root = worktree
                .canonicalize()
                .with_context(|| format!("resolving worktree {}", worktree.display()))?;
            let path = path
                .canonicalize()
                .with_context(|| format!("resolving browser file URL {}", path.display()))?;
            if !path.starts_with(&root) {
                bail!("browser file URL leaves the worktree");
            }
            if path.strip_prefix(&root).ok().is_some_and(|relative| {
                relative
                    .components()
                    .any(|component| component.as_os_str() == ".git")
            }) {
                bail!("browser file URL points into Git metadata");
            }
            if !path.is_file() {
                bail!("browser file URL is not a regular file");
            }
            Ok(reqwest::Url::from_file_path(&path)
                .map_err(|_| anyhow!("cannot encode browser file URL"))?
                .to_string())
        }
        other => bail!("browser navigation does not support the {other} scheme"),
    }
}

fn safe_relative_path(raw: &str) -> Result<PathBuf> {
    let path = Path::new(raw);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        bail!("screenshot path must be relative and stay inside the worktree");
    }
    if path.components().any(|component| {
        component
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(".git")
    }) {
        bail!("screenshot path cannot target Git metadata");
    }
    Ok(path.to_path_buf())
}

fn find_helper_binary() -> Result<PathBuf> {
    if let Some(value) = std::env::var_os("VERLINPARK_BROWSER_HELPER") {
        let path = PathBuf::from(value);
        if is_executable(&path) {
            return Ok(path);
        }
        bail!(
            "VERLINPARK_BROWSER_HELPER does not point to an executable: {}",
            path.display()
        );
    }
    let mut candidates = Vec::new();
    if let Ok(current) = std::env::current_exe()
        && let Some(parent) = current.parent()
    {
        candidates.push(parent.join(HELPER_BINARY));
        if let Some(grandparent) = parent.parent() {
            candidates.push(grandparent.join(HELPER_BINARY));
        }
    }
    if let Ok(current) = std::env::current_dir() {
        candidates.push(
            current
                .join("apps/macos/build/Build/Products/Debug")
                .join(HELPER_BINARY),
        );
        candidates.push(
            current
                .join("apps/macos/build/Build/Products/Release")
                .join(HELPER_BINARY),
        );
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if let Some(root) = manifest.parent().and_then(Path::parent) {
        candidates.push(
            root.join("apps/macos/build/Build/Products/Debug")
                .join(HELPER_BINARY),
        );
        candidates.push(
            root.join("apps/macos/build/Build/Products/Release")
                .join(HELPER_BINARY),
        );
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&paths) {
            candidates.push(directory.join(HELPER_BINARY));
        }
    }
    for candidate in candidates {
        if is_executable(&candidate) {
            return Ok(candidate);
        }
    }
    bail!(
        "VerlinParkBrowserHelper is not installed; build the macOS helper target or set VERLINPARK_BROWSER_HELPER"
    )
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.is_file()
            && std::fs::metadata(path)
                .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn validates_local_and_worktree_urls() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let page = root.join("index.html");
        std::fs::write(&page, "<html></html>").unwrap();
        assert!(
            validate_navigation_url("http://127.0.0.1:3000", &root)
                .unwrap()
                .contains("127.0.0.1")
        );
        assert!(validate_navigation_url("http://localhost:3000/app", &root).is_ok());
        assert!(validate_navigation_url("http://[::1]:3000", &root).is_ok());
        assert!(validate_navigation_url(&format!("file://{}", page.display()), &root).is_ok());
        assert!(validate_navigation_url("http://example.com", &root).is_err());
        assert!(validate_navigation_url("file:///etc/passwd", &root).is_err());
        assert!(validate_navigation_url("https://user:pass@localhost", &root).is_err());
        assert!(validate_navigation_url("data:text/html,hello", &root).is_err());
    }

    #[test]
    fn rejects_screenshot_path_escapes_and_git_metadata() {
        assert!(safe_relative_path(".verlinpark/browser/a.png").is_ok());
        assert!(safe_relative_path("../a.png").is_err());
        assert!(safe_relative_path("/tmp/a.png").is_err());
        assert!(safe_relative_path(".git/a.png").is_err());
    }

    #[test]
    fn renders_a_bounded_tree() {
        let tree = json!({
            "ref": null,
            "role": "document",
            "name": "",
            "text": "",
            "children": [
                {"ref": "e1", "role": "button", "name": "Press", "text": "Press", "children": []}
            ]
        });
        let mut output = String::new();
        render_tree(&tree, 0, &mut output);
        assert!(output.contains("document"));
        assert!(output.contains("[e1] button \"Press\""));
    }
}

#[cfg(test)]
mod gui_status_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn browser_status_is_inspection_only_and_reports_platform_state() {
        let data = TempDir::new().unwrap();
        let manager = BrowserManager::new(data.path().to_path_buf());
        let status = manager.status();
        assert_eq!(status.platform_supported, cfg!(target_os = "macos"));
        assert_eq!(status.warm_helpers, 0);
        assert_eq!(status.sandbox_available, Path::new(SANDBOX_EXEC).is_file());
        if !status.helper_available {
            assert!(status.helper_path.is_none());
        }
    }

    #[tokio::test]
    async fn browser_initially_has_no_warm_helpers() {
        let data = TempDir::new().unwrap();
        let manager = BrowserManager::new(data.path().to_path_buf());
        assert_eq!(manager.warm_helpers(), 0);
        assert_eq!(manager.stop_session("missing-session").await, 0);
        assert_eq!(
            BrowserManager::idle_default().as_secs(),
            DEFAULT_HELPER_IDLE_SECS
        );
    }
}

#[cfg(test)]
mod gui_contention_tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;
    use vp_protocol::types::GuiPolicy;
    use vp_store::Store;

    fn init_repo(path: &Path) {
        let status = Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::write(path.join("README.md"), "browser lease\n").unwrap();
        let status = Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success());
        let status = Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "init",
            ])
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn write_script(path: &Path, body: &str) {
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
    }

    #[tokio::test]
    async fn close_without_a_surface_reports_not_closed_and_releases_any_lease() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("browser close".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session(project.id, "close".into(), None)
            .await
            .unwrap();
        let worktree = PathBuf::from(session.worktree.clone().unwrap());
        let manager = BrowserManager::new(data.path().to_path_buf());
        let target = BrowserTarget {
            session_id: &session.id,
            worktree: &worktree,
            agent_id: "agent",
            agent_name: "Agent",
        };
        let policy = vp_protocol::types::GuiPolicy::default();
        assert!(matches!(
            engine
                .gui_begin_action_with_policy(
                    &policy,
                    GuiSurface::Browser,
                    &session.id,
                    "agent",
                    "Agent",
                )
                .await
                .unwrap(),
            GuiAcquire::Acquired(_)
        ));
        assert_eq!(engine.list_computer_leases().await.unwrap().len(), 1);
        let output = manager
            .action(&engine, &target, json!({"action": "close"}))
            .await
            .unwrap();
        assert!(!output.is_error);
        assert_eq!(output.structured.as_ref().unwrap()["closed"], false);
        assert_eq!(output.structured.as_ref().unwrap()["lease_released"], true);
        assert!(engine.list_computer_leases().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn crashed_browser_helper_is_removed_and_respawned() {
        if !Path::new(SANDBOX_EXEC).is_file() {
            eprintln!("skipping: sandbox-exec is not available");
            return;
        }
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let helper_path = data.path().join("fake-crashing-browser-helper.sh");
        write_script(
            &helper_path,
            r#"#!/bin/sh
printf '%s' "$$" > "$2/pid"
printf '{"event":"ready","version":"1","pid":%s}
' "$$"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"action":"snapshot"'*)
      printf '{"id":"%s","ok":true,"result":{"url":"file:///tmp/index.html","title":"Fake","tree":{"ref":"e1","role":"link","name":"Home","children":[]},"text":"hello","node_count":1,"truncated":false}}
' "$id"
      ;;
    *'"action":"close"'*)
      printf '{"id":"%s","ok":true,"result":{"closed":true}}
' "$id"
      exit 0
      ;;
    *)
      printf '{"id":"%s","ok":true,"result":{}}
' "$id"
      ;;
  esac
done
"#,
        );
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("browser crash".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session(project.id, "crash".into(), None)
            .await
            .unwrap();
        let worktree = PathBuf::from(session.worktree.clone().unwrap());
        let manager = BrowserManager::with_helper(data.path().to_path_buf(), helper_path);
        let target = BrowserTarget {
            session_id: &session.id,
            worktree: &worktree,
            agent_id: "agent",
            agent_name: "Agent",
        };
        let first = manager
            .action(&engine, &target, json!({"action": "snapshot"}))
            .await
            .unwrap();
        assert!(!first.is_error, "{}", first.content);
        let scratch = manager.helper_scratch(&target);
        let first_pid: i32 = std::fs::read_to_string(scratch.join("pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // SAFETY: the test owns the helper child and sends a normal SIGKILL.
        unsafe {
            libc::kill(first_pid, libc::SIGKILL);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        let second = manager
            .action(&engine, &target, json!({"action": "snapshot"}))
            .await
            .unwrap();
        assert!(!second.is_error, "{}", second.content);
        let second_pid: i32 = std::fs::read_to_string(scratch.join("pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_ne!(first_pid, second_pid);
        assert_eq!(manager.warm_helpers(), 1);
        manager
            .action(&engine, &target, json!({"action": "close"}))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn browser_snapshot_delta_and_generation_stale_ref_are_runtime_checked() {
        if !Path::new(SANDBOX_EXEC).is_file() {
            eprintln!("skipping: sandbox-exec is not available");
            return;
        }
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let browser_path = data.path().join("fake-delta-browser-helper.sh");
        write_script(
            &browser_path,
            r#"#!/bin/sh
count=0
printf '{"event":"ready","version":"1","pid":%s}\n' "$$"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"action":"snapshot"'*)
      count=$((count+1))
      if [ "$count" = "1" ]; then
        printf '{"id":"%s","ok":true,"result":{"url":"file:///tmp/index.html","title":"Fake","tree":{"ref":"e1","role":"link","name":"Home","children":[]},"text":"Home","node_count":1,"truncated":false}}\n' "$id"
      else
        printf '{"id":"%s","ok":true,"result":{"url":"file:///tmp/index.html","title":"Fake","tree":{"ref":"e1","role":"link","name":"Home page","children":[{"ref":"e2","role":"button","name":"Save","children":[]}]},"text":"Home page","node_count":2,"truncated":false}}\n' "$id"
      fi
      ;;
    *'"action":"click"'*)
      printf '{"id":"%s","ok":true,"result":{"name":"Home","clicked":true}}\n' "$id"
      ;;
    *'"action":"close"'*)
      printf '{"id":"%s","ok":true,"result":{"closed":true}}\n' "$id"
      exit 0
      ;;
    *)
      printf '{"id":"%s","ok":true,"result":{}}\n' "$id"
      ;;
  esac
done
"#,
        );
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("browser delta".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session(project.id, "delta".into(), None)
            .await
            .unwrap();
        let worktree = PathBuf::from(session.worktree.clone().unwrap());
        let manager = BrowserManager::with_helper(data.path().to_path_buf(), browser_path);
        let target = BrowserTarget {
            session_id: &session.id,
            worktree: &worktree,
            agent_id: "agent",
            agent_name: "Agent",
        };
        let first = manager
            .action(&engine, &target, json!({"action": "snapshot"}))
            .await
            .unwrap();
        assert!(!first.is_error, "{}", first.content);
        let first_structured = first.structured.as_ref().unwrap();
        assert_eq!(first_structured["delta"], false);
        assert_eq!(first_structured["generation"], 1);
        let second = manager
            .action(&engine, &target, json!({"action": "snapshot"}))
            .await
            .unwrap();
        assert!(!second.is_error, "{}", second.content);
        let second_structured = second.structured.as_ref().unwrap();
        assert_eq!(second_structured["delta"], true);
        assert_eq!(second_structured["generation"], 2);
        assert_eq!(second_structured["base_generation"], 1);
        assert!(
            second_structured["new_refs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == "e2")
        );
        assert!(
            second_structured["changed_refs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == "e1")
        );
        assert_eq!(second_structured["text_delta"]["changed"], true);
        assert!(second_structured.get("tree").is_none());
        let stale = manager
            .action(
                &engine,
                &target,
                json!({"action": "click", "ref": "e1", "snapshot_generation": 1}),
            )
            .await
            .unwrap();
        assert!(stale.is_error);
        assert_eq!(stale.structured.as_ref().unwrap()["code"], "STALE_REF");
        assert_eq!(stale.structured.as_ref().unwrap()["snapshot_generation"], 2);
        let fresh = manager
            .action(
                &engine,
                &target,
                json!({"action": "click", "ref": "e1", "snapshot_generation": 2}),
            )
            .await
            .unwrap();
        assert!(!fresh.is_error, "{}", fresh.content);
        manager
            .action(&engine, &target, json!({"action": "close"}))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn browser_and_computer_contend_for_one_gui_lease() {
        if !Path::new(SANDBOX_EXEC).is_file() {
            eprintln!("skipping: sandbox-exec is not available");
            return;
        }
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let browser_path = data.path().join("fake-browser-helper.sh");
        write_script(
            &browser_path,
            r#"#!/bin/sh
echo spawn >> "$0.spawns"
printf '{"event":"ready","version":"1","pid":%s}\n' "$$"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"action":"snapshot"'*)
      printf '{"id":"%s","ok":true,"result":{"url":"file:///tmp/index.html","title":"Fake","tree":{"ref":"e1","role":"link","name":"Home","children":[]},"text":"hello","node_count":1,"truncated":false}}\n' "$id"
      ;;
    *'"action":"close"'*)
      printf '{"id":"%s","ok":true,"result":{"closed":true}}\n' "$id"
      exit 0
      ;;
    *)
      printf '{"id":"%s","ok":true,"result":{}}\n' "$id"
      ;;
  esac
done
"#,
        );
        let computer_path = data.path().join("fake-computer-helper.sh");
        write_script(
            &computer_path,
            r#"#!/bin/sh
echo spawn >> "$0.spawns"
printf '{"event":"ready","version":"1","pid":%s}\n' "$$"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"action":"apps"'*)
      printf '{"id":"%s","ok":true,"result":{"apps":[{"name":"Fake","pid":1,"frontmost":true}],"count":1}}\n' "$id"
      ;;
    *'"action":"close"'*)
      printf '{"id":"%s","ok":true,"result":{"closed":true}}\n' "$id"
      exit 0
      ;;
    *)
      printf '{"id":"%s","ok":true,"result":{}}\n' "$id"
      ;;
  esac
done
"#,
        );
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("browser computer".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session(project.id.clone(), "contend".into(), None)
            .await
            .unwrap();
        let other = engine
            .create_session(project.id.clone(), "other".into(), None)
            .await
            .unwrap();
        engine
            .set_gui_policy(
                &project.id,
                GuiPolicy {
                    grace_secs: 30,
                    wait_ms: 60,
                    ..GuiPolicy::default()
                },
            )
            .await
            .unwrap();
        let worktree = PathBuf::from(session.worktree.clone().unwrap());
        let other_worktree = PathBuf::from(other.worktree.clone().unwrap());
        let browsers = BrowserManager::with_helper(data.path().to_path_buf(), browser_path.clone());
        let computers = crate::computer::ComputerManager::with_helper(
            data.path().to_path_buf(),
            computer_path.clone(),
        );
        let browser_target = BrowserTarget {
            session_id: &session.id,
            worktree: &worktree,
            agent_id: "browser-agent",
            agent_name: "Browser Agent",
        };
        let computer_target = crate::computer::ComputerTarget {
            session_id: &other.id,
            worktree: &other_worktree,
            agent_id: "computer-agent",
            agent_name: "Computer Agent",
            task_id: None,
        };

        let browser = browsers
            .action(&engine, &browser_target, json!({"action": "snapshot"}))
            .await
            .unwrap();
        assert!(!browser.is_error, "{}", browser.content);
        assert_eq!(browsers.warm_helpers(), 1);
        assert_eq!(engine.list_computer_leases().await.unwrap().len(), 1);

        let blocked = computers
            .action(&engine, &computer_target, json!({"action": "apps"}))
            .await
            .unwrap();
        assert!(blocked.is_error);
        let structured = blocked.structured.as_ref().unwrap();
        assert_eq!(structured["code"], "GUI_BUSY");
        assert_eq!(structured["holder"]["agent_name"], "Browser Agent");

        assert!(engine.gui_end_session(&session.id, "handoff").await);
        assert_eq!(browsers.warm_helpers(), 1);
        let computer = computers
            .action(&engine, &computer_target, json!({"action": "apps"}))
            .await
            .unwrap();
        assert!(!computer.is_error, "{}", computer.content);
        assert_eq!(computers.warm_helpers(), 1);
        assert_eq!(engine.list_computer_leases().await.unwrap().len(), 1);

        let blocked_browser = browsers
            .action(&engine, &browser_target, json!({"action": "snapshot"}))
            .await
            .unwrap();
        assert!(blocked_browser.is_error);
        assert_eq!(
            blocked_browser.structured.as_ref().unwrap()["code"],
            "GUI_BUSY"
        );

        let closed_computer = computers
            .action(&engine, &computer_target, json!({"action": "close"}))
            .await
            .unwrap();
        assert!(!closed_computer.is_error);
        assert_eq!(computers.warm_helpers(), 0);
        assert!(engine.list_computer_leases().await.unwrap().is_empty());
        assert_eq!(browsers.warm_helpers(), 1, "browser helper stays warm");
        engine.gui_end_session(&other.id, "cleanup").await;

        let closed_browser = browsers
            .action(&engine, &browser_target, json!({"action": "close"}))
            .await
            .unwrap();
        assert!(!closed_browser.is_error);
        assert_eq!(browsers.warm_helpers(), 0);
        assert_eq!(browsers.stop_session(&session.id).await, 0);
    }

    #[tokio::test]
    async fn preemption_cancels_a_hanging_browser_rpc_and_releases_the_lease() {
        if !Path::new(SANDBOX_EXEC).is_file() {
            eprintln!("skipping: sandbox-exec is not available");
            return;
        }
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let helper_path = data.path().join("fake-slow-browser-helper.sh");
        write_script(
            &helper_path,
            r#"#!/bin/sh
printf '{"event":"ready","version":"1","pid":%s}
' "$$"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"action":"snapshot"'*)
      sleep 30
      printf '{"id":"%s","ok":true,"result":{}}
' "$id"
      ;;
    *'"action":"close"'*)
      exit 0
      ;;
  esac
done
"#,
        );
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("browser preempt".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session(project.id, "preempt".into(), None)
            .await
            .unwrap();
        let worktree = PathBuf::from(session.worktree.clone().unwrap());
        let manager = BrowserManager::with_helper(data.path().to_path_buf(), helper_path);
        let target = BrowserTarget {
            session_id: &session.id,
            worktree: &worktree,
            agent_id: "agent",
            agent_name: "Agent",
        };
        let token = crate::preemption::Token::disabled();
        let trigger = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            trigger.trigger(
                crate::preemption::Reason::Explicit,
                crate::preemption::Phase::Tool,
                true,
            );
        });
        let started = std::time::Instant::now();
        let output = crate::preemption::scope_token(token, async {
            manager
                .action(&engine, &target, json!({"action": "snapshot"}))
                .await
        })
        .await
        .unwrap();
        assert!(output.is_error, "{}", output.content);
        assert!(started.elapsed() < Duration::from_secs(10));
        let snapshot = crate::preemption::output_snapshot(&output).expect("typed preemption");
        assert_eq!(snapshot.reason, crate::preemption::Reason::Explicit);
        assert_eq!(snapshot.phase, crate::preemption::Phase::Tool);
        assert!(snapshot.side_effects_unknown);
        assert!(
            engine.list_computer_leases().await.unwrap().is_empty(),
            "a preempted browser action must release the GUI lease"
        );
    }

    #[test]
    fn delta_reports_new_removed_changed_refs_and_text_delta() {
        let previous = json!({
            "ref": "e1", "role": "root", "name": "Root",
            "children": [
                {"ref": "e2", "role": "link", "name": "Home", "text": "Home", "children": []},
                {"ref": "e3", "role": "button", "name": "Save", "children": []}
            ]
        });
        let current = json!({
            "ref": "e1", "role": "root", "name": "Root",
            "children": [
                {"ref": "e2", "role": "link", "name": "Home page", "text": "Home page", "children": []},
                {"ref": "e4", "role": "button", "name": "Cancel", "children": []}
            ]
        });
        let delta = snapshot_delta(
            1,
            2,
            Some(&previous),
            "Home\nSave",
            "Home page\nCancel",
            Some(&current),
        );
        assert_eq!(delta["delta"], true);
        assert_eq!(delta["base_generation"], 1);
        assert_eq!(delta["generation"], 2);
        assert_eq!(delta["new_refs"], json!(["e4"]));
        assert_eq!(delta["removed_refs"], json!(["e3"]));
        assert_eq!(delta["changed_refs"], json!(["e2"]));
        assert_eq!(delta["text_delta"]["changed"], true);
        assert!(
            delta["text_delta"]["removed_lines"]
                .as_array()
                .unwrap()
                .iter()
                .any(|line| line == "Save")
        );
        assert!(
            delta["text_delta"]["added_lines"]
                .as_array()
                .unwrap()
                .iter()
                .any(|line| line == "Cancel")
        );
        assert_eq!(line_delta("same", "same")["changed"], false);
    }

    #[test]
    fn snapshot_generation_staleness_is_explicit() {
        assert!(snapshot_generation_is_stale(1, 0));
        assert!(snapshot_generation_is_stale(1, 2));
        assert!(!snapshot_generation_is_stale(2, 2));
    }
}

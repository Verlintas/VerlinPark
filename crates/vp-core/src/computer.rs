//! Native macOS computer helper hosted by the daemon.
//!
//! The computer helper is deliberately different from the browser helper. It
//! drives the real macOS Accessibility tree and posts CGEvents, so it cannot be
//! confined by the Seatbelt write profile used for shell and WebKit. It runs as
//! a daemon-owned child with the logged-in user's authority. The safety boundary
//! is therefore:
//!
//! - the daemon, not the agent, launches and owns the helper process;
//! - every action passes through the normal approval policy before dispatch;
//! - the `computer` [`ComputerLease`] serializes the surface across sessions and
//!   fails fast instead of queueing;
//! - request/response frames, snapshot depth/elements/bytes, text, points, keys,
//!   windows, apps, and screenshots are bounded;
//! - screenshots are written to daemon-owned scratch and published as artifacts,
//!   never returned as unbounded inline model data.
//!
//! The helper is not a sandbox. Accessibility, CGEvent, and ScreenCaptureKit are
//! host-level services. See ADR-0158.
//!
//! [`ComputerLease`]: crate::ComputerLeaseGuard

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::timeout;
use uuid::Uuid;
use vp_protocol::types::{ComputerPermissions, ComputerStatusResult};

use crate::Engine;
use crate::gui::{self, GuiAcquire, GuiActionOutcome, GuiSurface};
use crate::tooling::fs::sha256_hex;
use crate::tools::{ToolOutput, truncate};

const HELPER_BINARY: &str = "VerlinParkComputerHelper";
const DEFAULT_HELPER_IDLE_SECS: u64 = 600;
const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;
const MAX_SCREENSHOT_BYTES: u64 = 5 * 1024 * 1024;
const MAX_SNAPSHOT_CHARS: usize = 60_000;
const MAX_TEXT_BYTES: usize = 10_000;
const READY_TIMEOUT: Duration = Duration::from_secs(20);
const DEFAULT_ACTION_TIMEOUT: Duration = Duration::from_secs(15);
const SCREENSHOT_TIMEOUT: Duration = Duration::from_secs(30);

/// An agent-visible target for a computer action.
pub struct ComputerTarget<'a> {
    pub session_id: &'a str,
    pub worktree: &'a Path,
    pub agent_id: &'a str,
    pub agent_name: &'a str,
    pub task_id: Option<&'a str>,
}

#[derive(Debug, Clone)]
struct ComputerProtocolError {
    code: String,
    message: String,
    details: Option<Value>,
}

impl std::fmt::Display for ComputerProtocolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ComputerProtocolError {}

#[derive(Debug, Deserialize)]
struct HelperMessage {
    id: Option<String>,
    ok: Option<bool>,
    result: Option<Value>,
    error: Option<HelperErrorPayload>,
    event: Option<String>,
}

#[derive(Debug, Deserialize)]
struct HelperErrorPayload {
    code: String,
    message: String,
    #[serde(default)]
    details: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct HelperPermissionResult {
    accessibility_trusted: bool,
    screen_recording: bool,
    detail: Option<String>,
}

struct ComputerHelper {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    scratch: PathBuf,
    next_id: u64,
    pid: Option<i32>,
    last_used: Arc<AtomicU64>,
    /// Last generation reported by the helper. Used for diagnostics and to
    /// attach the expected generation to a snapshot result. The helper remains
    /// the authority: it rejects a stale ref even if Rust forwards a newer
    /// generation.
    last_snapshot_generation: u64,
}

impl ComputerHelper {
    async fn spawn(helper_path: &Path, scratch: &Path) -> Result<Self> {
        tokio::fs::create_dir_all(scratch.join("home")).await?;
        tokio::fs::create_dir_all(scratch.join("tmp")).await?;
        tokio::fs::create_dir_all(scratch.join("screenshots")).await?;
        let scratch = scratch
            .canonicalize()
            .with_context(|| format!("resolving computer scratch {}", scratch.display()))?;
        let mut command = Command::new(helper_path);
        command
            .arg("--scratch")
            .arg(&scratch)
            .arg("--parent-pid")
            .arg(std::process::id().to_string())
            .current_dir(&scratch)
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
            .env("LANG", "en_US.UTF-8")
            .env("LC_ALL", "en_US.UTF-8")
            .env("VERLINPARK_COMPUTER_HELPER", "1");
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command
            .spawn()
            .with_context(|| format!("starting computer helper {}", helper_path.display()))?;
        let pid = child.id().and_then(|pid| i32::try_from(pid).ok());
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("computer helper has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("computer helper has no stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("computer helper has no stderr"))?;
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "vp_core::computer", "{}", line);
            }
        });
        let mut helper = Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            scratch,
            next_id: 1,
            pid,
            last_used: Arc::new(AtomicU64::new(unix_now())),
            last_snapshot_generation: 0,
        };
        let ready = timeout(
            READY_TIMEOUT,
            read_bounded_frame(&mut helper.stdout, MAX_FRAME_BYTES),
        )
        .await;
        let ready = match ready {
            Ok(Ok(Some(line))) => serde_json::from_str::<Value>(&line).ok(),
            _ => None,
        };
        let ready_ok = ready
            .as_ref()
            .and_then(|value| value.get("event").and_then(Value::as_str))
            == Some("ready");
        if !ready_ok {
            let mut detail = String::from("helper did not emit a ready event");
            if let Ok(Ok(status)) = timeout(Duration::from_secs(1), helper.child.wait()).await {
                detail = format!("helper exited during startup with {status}");
            }
            helper.kill();
            bail!("computer helper did not become ready ({detail})");
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
            let frame = timeout(
                deadline,
                read_bounded_frame(&mut self.stdout, MAX_FRAME_BYTES),
            )
            .await;
            let frame = match frame {
                Ok(result) => result?,
                Err(_) => {
                    bail!(
                        "computer helper timed out after {} seconds",
                        deadline.as_secs()
                    );
                }
            };
            let Some(frame) = frame else {
                bail!("computer helper exited before responding");
            };
            let trimmed = frame.trim();
            if trimmed.is_empty() {
                continue;
            }
            let message: HelperMessage = serde_json::from_str(trimmed)
                .with_context(|| format!("invalid computer helper response: {trimmed}"))?;
            if message.event.is_some() {
                continue;
            }
            if message.id.as_deref() != Some(id.as_str()) {
                continue;
            }
            if message.ok == Some(true) {
                return Ok(message.result.unwrap_or(Value::Null));
            }
            let error = message.error.unwrap_or(HelperErrorPayload {
                code: "COMPUTER_ERROR".to_string(),
                message: "computer helper returned an error".to_string(),
                details: None,
            });
            return Err(ComputerProtocolError {
                code: error.code,
                message: error.message,
                details: error.details,
            }
            .into());
        }
    }

    fn kill(&mut self) {
        if let Some(pid) = self.pid {
            // SAFETY: `process_group(0)` makes the helper a process-group
            // leader. ESRCH is the expected no-op after an exit.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
        let _ = self.child.start_kill();
    }

    async fn shutdown(&mut self) {
        let _ = timeout(
            Duration::from_secs(2),
            self.request("close", json!({}), Duration::from_secs(2)),
        )
        .await;
        let _ = timeout(Duration::from_secs(2), self.child.wait()).await;
        self.kill();
        let _ = self.child.wait().await;
        self.pid = None;
        let _ = tokio::fs::remove_dir_all(&self.scratch).await;
    }
}

impl Drop for ComputerHelper {
    fn drop(&mut self) {
        if let Some(pid) = self.pid {
            // SAFETY: negative pid targets the helper's process group.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
    }
}

pub struct ComputerManager {
    helpers: Mutex<HashMap<String, Arc<AsyncMutex<ComputerHelper>>>>,
    spawn_lock: AsyncMutex<()>,
    status_cache: AsyncMutex<Option<CachedStatus>>,
    status_lock: AsyncMutex<()>,
    data_dir: PathBuf,
    #[cfg(test)]
    helper_override: Option<PathBuf>,
}

struct CachedStatus {
    result: ComputerStatusResult,
    fetched: std::time::Instant,
}

impl ComputerManager {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            helpers: Mutex::new(HashMap::new()),
            spawn_lock: AsyncMutex::new(()),
            status_cache: AsyncMutex::new(None),
            status_lock: AsyncMutex::new(()),
            data_dir,
            #[cfg(test)]
            helper_override: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_helper(data_dir: PathBuf, helper: PathBuf) -> Self {
        Self {
            helpers: Mutex::new(HashMap::new()),
            spawn_lock: AsyncMutex::new(()),
            status_cache: AsyncMutex::new(None),
            status_lock: AsyncMutex::new(()),
            data_dir,
            helper_override: Some(helper),
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
        target: &ComputerTarget<'_>,
        input: Value,
    ) -> Result<ToolOutput> {
        let action = match normalize_action(&input) {
            Ok(action) => action,
            Err(error) => {
                return Ok(ToolOutput::error_structured(
                    format!("computer action rejected: {error}"),
                    json!({"code": "INVALID_ARGUMENT", "message": error.to_string()}),
                ));
            }
        };
        let key = helper_key(target);
        if action == "close" {
            let output = self.close(&key).await;
            engine.gui_end_session(target.session_id, "close").await;
            return Ok(output);
        }
        let policy = engine
            .gui_policy_for_session(target.session_id)
            .await
            .unwrap_or_default();
        let mut params = match self.params_for(action.as_str(), target, &input) {
            Ok(params) => params,
            Err(error) => {
                return Ok(ToolOutput::error_structured(
                    format!("computer {action} rejected: {error}"),
                    json!({
                        "code": "INVALID_ARGUMENT",
                        "action": action,
                        "message": error.to_string(),
                    }),
                ));
            }
        };
        if action != "permissions" {
            gui::helper_sensitive_params(&policy, &input, &mut params);
        }
        let ticket = match engine
            .gui_begin_action_with_policy(
                &policy,
                GuiSurface::Computer,
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
                    format!("computer {action} unavailable: {error}"),
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
        let output = self
            .run_action(
                engine,
                target,
                &key,
                &action,
                &params,
                &policy,
                &input,
                &mut outcome,
            )
            .await;
        engine.gui_finish_action(&ticket, outcome).await;
        Ok(output)
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_action(
        &self,
        engine: &Engine,
        target: &ComputerTarget<'_>,
        key: &str,
        action: &str,
        params: &Value,
        policy: &vp_protocol::types::GuiPolicy,
        input: &Value,
        outcome: &mut GuiActionOutcome,
    ) -> ToolOutput {
        let helper = match self.ensure_helper(target, key).await {
            Ok(helper) => helper,
            Err(error) => {
                let message = error.to_string();
                let code = if message.contains("is not installed") {
                    "COMPUTER_HELPER_UNAVAILABLE"
                } else if message.contains("did not become ready") {
                    "COMPUTER_HELPER_START_FAILED"
                } else {
                    "COMPUTER_HELPER_FAILED"
                };
                outcome.result = "error".to_string();
                outcome.error_code = Some(code.to_string());
                return ToolOutput::error_structured(
                    format!("computer {action} unavailable: {message}"),
                    json!({
                        "code": code,
                        "action": action,
                        "message": message,
                        "retryable": false,
                        "lease_surface": gui::GUI_SURFACE,
                    }),
                );
            }
        };
        let deadline = action_deadline(action, input);
        let started = std::time::Instant::now();
        let result = {
            let mut helper = helper.lock().await;
            helper.request(action, params.clone(), deadline).await
        };
        outcome.latency_ms = started.elapsed().as_millis() as u64;
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                let protocol_error = error.downcast_ref::<ComputerProtocolError>();
                let code = protocol_error
                    .map(|error| error.code.clone())
                    .unwrap_or_else(|| "COMPUTER_ERROR".to_string());
                let keep_helper = protocol_error.is_some();
                if !keep_helper {
                    let removed = self.helpers.lock().ok().and_then(|mut map| map.remove(key));
                    if let Some(helper) = removed {
                        let mut helper = helper.lock().await;
                        helper.shutdown().await;
                    }
                }
                outcome.result = "error".to_string();
                outcome.error_code = Some(code.clone());
                outcome.target_app = string_param(input, "app", 200);
                outcome.target_bundle_id = string_param(input, "bundle_id", 200);
                outcome.target_window = string_param(input, "window_title", 200);
                return ToolOutput::error_structured(
                    format!("computer {action} failed: {error}"),
                    protocol_error_output(action, protocol_error, &error),
                );
            }
        };
        if action == "snapshot"
            && let Some(generation) = result.get("snapshot_generation").and_then(Value::as_u64)
        {
            let mut helper = helper.lock().await;
            helper.last_snapshot_generation = generation;
        }
        let helper_generation = {
            let helper = helper.lock().await;
            helper.last_snapshot_generation
        };
        let output = match self
            .render(engine, action.to_string(), &result, target, input)
            .await
        {
            Ok(output) => output,
            Err(error) => {
                outcome.result = "error".to_string();
                outcome.error_code = Some("COMPUTER_RENDER_FAILED".to_string());
                return ToolOutput::error_structured(
                    format!("computer {action} failed: {error}"),
                    json!({
                        "code": "COMPUTER_RENDER_FAILED",
                        "action": action,
                        "message": error.to_string(),
                        "retryable": false,
                    }),
                );
            }
        };
        let target_info = result.get("target").cloned().unwrap_or(Value::Null);
        let bundle_id = target_info
            .get("bundle_id")
            .and_then(Value::as_str)
            .or_else(|| result.get("bundle_id").and_then(Value::as_str))
            .or_else(|| input.get("bundle_id").and_then(Value::as_str))
            .map(str::to_string);
        let app = target_info
            .get("app")
            .and_then(Value::as_str)
            .or_else(|| result.get("app").and_then(Value::as_str))
            .map(str::to_string);
        let window = target_info
            .get("window_title")
            .and_then(Value::as_str)
            .or_else(|| result.get("window_title").and_then(Value::as_str))
            .map(str::to_string);
        let sensitive = target_info
            .get("sensitive")
            .and_then(Value::as_bool)
            .unwrap_or_else(|| {
                gui::is_sensitive_target(
                    policy,
                    bundle_id.as_deref(),
                    app.as_deref(),
                    window.as_deref(),
                )
            });
        outcome.target_app = app;
        outcome.target_bundle_id = bundle_id;
        outcome.target_window = if sensitive { None } else { window };
        outcome.sensitive_target = sensitive;
        outcome.snapshot_generation = result
            .get("snapshot_generation")
            .and_then(Value::as_u64)
            .or_else(|| result.get("generation").and_then(Value::as_u64))
            .unwrap_or(helper_generation);
        outcome.result = if output.is_error { "error" } else { "ok" }.to_string();
        outcome.error_code = output
            .structured
            .as_ref()
            .and_then(|value| value.get("code"))
            .and_then(Value::as_str)
            .map(str::to_string);
        output
    }

    async fn close(&self, key: &str) -> ToolOutput {
        let helper = self.helpers.lock().ok().and_then(|mut map| map.remove(key));
        if let Some(helper) = helper {
            {
                let mut helper = helper.lock().await;
                helper.shutdown().await;
            }
            return ToolOutput::ok_structured(
                "computer surface closed",
                json!({"action": "close", "closed": true}),
            );
        }
        ToolOutput::ok_structured(
            "no computer surface was open",
            json!({"action": "close", "closed": false}),
        )
    }

    async fn ensure_helper(
        &self,
        target: &ComputerTarget<'_>,
        key: &str,
    ) -> Result<Arc<AsyncMutex<ComputerHelper>>> {
        if let Some(helper) = self.existing_helper(key).await {
            return Ok(helper);
        }
        let _spawn = self.spawn_lock.lock().await;
        if let Some(helper) = self.existing_helper(key).await {
            return Ok(helper);
        }
        let helper_path = self.helper_path()?;
        let scratch = self.helper_scratch(target);
        let helper = match ComputerHelper::spawn(&helper_path, &scratch).await {
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

    /// Reuse a warm helper even when the GUI lease was released by grace. The
    /// helper process is independent of the lease; a crashed child is removed
    /// so the next call respawns it.
    async fn existing_helper(&self, key: &str) -> Option<Arc<AsyncMutex<ComputerHelper>>> {
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
        }
        None
    }

    pub fn warm_helpers(&self) -> usize {
        self.helpers.lock().map(|map| map.len()).unwrap_or(0)
    }

    fn helper_scratch(&self, target: &ComputerTarget<'_>) -> PathBuf {
        let digest = sha256_hex(target.worktree.to_string_lossy().as_bytes());
        self.data_dir
            .join("sandbox")
            .join(target.session_id)
            .join("computer-helper")
            .join(&digest[..16])
    }

    fn params_for(
        &self,
        action: &str,
        _target: &ComputerTarget<'_>,
        input: &Value,
    ) -> Result<Value> {
        match action {
            "permissions" => Ok(json!({})),
            "apps" => Ok(json!({
                "include_background": bool_param(input, "include_background").unwrap_or(false),
            })),
            "displays" => Ok(json!({})),
            "windows" => {
                let mut params = json!({
                    "include_offscreen": bool_param(input, "include_offscreen").unwrap_or(false),
                });
                if let Some(bundle_id) = string_param(input, "bundle_id", 200) {
                    params["bundle_id"] = json!(bundle_id);
                }
                if let Some(pid) = optional_u64(input, "pid")? {
                    if pid == 0 || pid > i32::MAX as u64 {
                        bail!("pid is out of range");
                    }
                    params["pid"] = json!(pid);
                }
                Ok(params)
            }
            "focus" => {
                let mut params = json!({});
                let bundle_id = string_param(input, "bundle_id", 200);
                let pid = optional_u64(input, "pid")?;
                if bundle_id.is_none() && pid.is_none() {
                    bail!("focus requires bundle_id or pid");
                }
                if let Some(bundle_id) = bundle_id {
                    params["bundle_id"] = json!(bundle_id);
                }
                if let Some(pid) = pid {
                    if pid == 0 || pid > i32::MAX as u64 {
                        bail!("pid is out of range");
                    }
                    params["pid"] = json!(pid);
                }
                if let Some(title) = string_param(input, "window_title", 200) {
                    params["window_title"] = json!(title);
                }
                if let Some(window_id) = optional_u64(input, "window_id")? {
                    if window_id == 0 || window_id > u32::MAX as u64 {
                        bail!("window_id is out of range");
                    }
                    params["window_id"] = json!(window_id);
                }
                Ok(params)
            }
            "snapshot" => {
                let mut params = json!({
                    "max_depth": optional_u64(input, "max_depth")?.unwrap_or(10).clamp(1, 20),
                    "max_elements": optional_u64(input, "max_elements")?.unwrap_or(400).clamp(1, 1_200),
                    "max_bytes": optional_u64(input, "max_bytes")?.unwrap_or(131_072).clamp(1_024, 262_144),
                });
                if let Some(bundle_id) = string_param(input, "bundle_id", 200) {
                    params["bundle_id"] = json!(bundle_id);
                }
                if let Some(pid) = optional_u64(input, "pid")? {
                    if pid == 0 || pid > i32::MAX as u64 {
                        bail!("pid is out of range");
                    }
                    params["pid"] = json!(pid);
                }
                if let Some(window_id) = optional_u64(input, "window_id")? {
                    if window_id == 0 || window_id > u32::MAX as u64 {
                        bail!("window_id is out of range");
                    }
                    params["window_id"] = json!(window_id);
                }
                Ok(params)
            }
            "screenshot" => {
                let target_kind = string_param(input, "target", 20)
                    .unwrap_or_else(|| "window".to_string())
                    .to_ascii_lowercase();
                if !matches!(target_kind.as_str(), "window" | "screen") {
                    bail!("screenshot target must be window or screen");
                }
                let mut params = json!({
                    "target": target_kind,
                    "max_width": optional_u64(input, "max_width")?.unwrap_or(1_600).clamp(320, 2_560),
                    "max_height": optional_u64(input, "max_height")?.unwrap_or(1_200).clamp(320, 2_560),
                    "output": format!("screenshots/computer-{}.png", short_id()),
                });
                if let Some(index) = optional_u64(input, "display_index")? {
                    if index > 15 {
                        bail!("display_index must be between 0 and 15");
                    }
                    params["display_index"] = json!(index);
                }
                if let Some(id) = optional_u64(input, "display_id")? {
                    if id == 0 || id > u32::MAX as u64 {
                        bail!("display_id is out of range");
                    }
                    params["display_id"] = json!(id);
                }
                if let Some(window_id) = optional_u64(input, "window_id")? {
                    if window_id == 0 || window_id > u32::MAX as u64 {
                        bail!("window_id is out of range");
                    }
                    params["window_id"] = json!(window_id);
                }
                Ok(params)
            }
            "click" => {
                let mut params = json!({});
                let reference = string_param(input, "ref", 64);
                let x = optional_f64(input, "x")?;
                let y = optional_f64(input, "y")?;
                match (reference, x, y) {
                    (Some(reference), None, None) => {
                        validate_ref(&reference)?;
                        attach_ref_generation(&reference, &mut params);
                        params["ref"] = json!(reference);
                    }
                    (None, Some(x), Some(y)) => {
                        validate_normalized(x, "x")?;
                        validate_normalized(y, "y")?;
                        params["x"] = json!(x);
                        params["y"] = json!(y);
                    }
                    (Some(_), _, _) => bail!("click accepts either ref or x/y, not both"),
                    (None, _, _) => bail!("click requires ref or both x and y"),
                }
                let button = string_param(input, "button", 20)
                    .unwrap_or_else(|| "left".to_string())
                    .to_ascii_lowercase();
                if !matches!(button.as_str(), "left" | "right") {
                    bail!("click button must be left or right");
                }
                params["button"] = json!(button);
                params["click_count"] =
                    json!(optional_u64(input, "click_count")?.unwrap_or(1).clamp(1, 2));
                apply_window_target(input, &mut params)?;
                Ok(params)
            }
            "type" => {
                let text = required_string(input, "text")?;
                if text.len() > MAX_TEXT_BYTES {
                    bail!("computer type text exceeds {MAX_TEXT_BYTES} bytes");
                }
                if text.contains('\0') {
                    bail!("computer type text contains a NUL character");
                }
                let mut params = json!({"text": text});
                if let Some(reference) = string_param(input, "ref", 64) {
                    validate_ref(&reference)?;
                    attach_ref_generation(&reference, &mut params);
                    params["ref"] = json!(reference);
                }
                apply_window_target(input, &mut params)?;
                Ok(params)
            }
            "key" => {
                let key = required_string(input, "key")?;
                if key.len() > 32 {
                    bail!("computer key is too long");
                }
                validate_key(&key)?;
                let modifiers = modifiers_param(input)?;
                let destructive = destructive_key_combo(&key, &modifiers);
                let mut params = json!({
                    "key": key,
                    "modifiers": modifiers,
                    // This path is reached only after the agent loop's
                    // permission decision. The helper additionally refuses a
                    // destructive shortcut when this flag is absent.
                    "allow_destructive": destructive,
                });
                apply_window_target(input, &mut params)?;
                Ok(params)
            }
            "scroll" => {
                let dx = optional_i64(input, "dx")?.unwrap_or(0);
                let dy = optional_i64(input, "dy")?.unwrap_or(0);
                if dx == 0 && dy == 0 {
                    bail!("scroll requires a non-zero dx or dy");
                }
                if dx.abs() > 1_000 || dy.abs() > 1_000 {
                    bail!("scroll delta exceeds 1000");
                }
                let unit = string_param(input, "unit", 20)
                    .unwrap_or_else(|| "line".to_string())
                    .to_ascii_lowercase();
                if !matches!(unit.as_str(), "line" | "pixel") {
                    bail!("scroll unit must be line or pixel");
                }
                let mut params = json!({"dx": dx, "dy": dy, "unit": unit});
                if let Some(reference) = string_param(input, "ref", 64) {
                    validate_ref(&reference)?;
                    attach_ref_generation(&reference, &mut params);
                    params["ref"] = json!(reference);
                }
                apply_window_target(input, &mut params)?;
                Ok(params)
            }
            "drag" => {
                let mut params = json!({});
                for (name, key) in [
                    ("from_x", "from_x"),
                    ("from_y", "from_y"),
                    ("to_x", "to_x"),
                    ("to_y", "to_y"),
                ] {
                    let value = optional_f64(input, name)?
                        .ok_or_else(|| anyhow!("drag requires {name}"))?;
                    validate_normalized(value, name)?;
                    params[key] = json!(value);
                }
                params["duration_ms"] = json!(
                    optional_u64(input, "duration_ms")?
                        .unwrap_or(300)
                        .clamp(0, 2_000)
                );
                params["steps"] = json!(optional_u64(input, "steps")?.unwrap_or(12).clamp(1, 60));
                apply_window_target(input, &mut params)?;
                Ok(params)
            }
            "wait" => {
                let condition = string_param(input, "condition", 40)
                    .unwrap_or_else(|| "ref".to_string())
                    .to_ascii_lowercase();
                let mut params = json!({
                    "condition": condition,
                    "timeout_ms": optional_u64(input, "timeout_ms")?.unwrap_or(5_000).clamp(50, 30_000),
                    "interval_ms": optional_u64(input, "interval_ms")?.unwrap_or(100).clamp(50, 1_000),
                });
                match condition.as_str() {
                    "ref" => {
                        let reference = required_string(input, "ref")?;
                        validate_ref(&reference)?;
                        attach_ref_generation(&reference, &mut params);
                        params["ref"] = json!(reference);
                    }
                    "element" => {
                        let role = string_param(input, "role", 80);
                        let title = string_param(input, "title", 200);
                        let identifier = string_param(input, "identifier", 200);
                        if role.is_none() && title.is_none() && identifier.is_none() {
                            bail!("element wait requires role, title, or identifier");
                        }
                        if let Some(role) = role {
                            params["role"] = json!(role);
                        }
                        if let Some(title) = title {
                            params["title"] = json!(title);
                        }
                        if let Some(identifier) = identifier {
                            params["identifier"] = json!(identifier);
                        }
                    }
                    "window" => {
                        params["title"] = json!(required_string(input, "title")?);
                    }
                    "app_frontmost" => {
                        let bundle_id = string_param(input, "bundle_id", 200);
                        let pid = optional_u64(input, "pid")?;
                        if bundle_id.is_none() && pid.is_none() {
                            bail!("app_frontmost wait requires bundle_id or pid");
                        }
                        if let Some(bundle_id) = bundle_id {
                            params["bundle_id"] = json!(bundle_id);
                        }
                        if let Some(pid) = pid {
                            if pid == 0 || pid > i32::MAX as u64 {
                                bail!("pid is out of range");
                            }
                            params["pid"] = json!(pid);
                        }
                    }
                    other => bail!("unsupported wait condition: {other}"),
                }
                Ok(params)
            }
            other => bail!("unsupported computer action: {other}"),
        }
    }

    async fn render(
        &self,
        engine: &Engine,
        action: String,
        result: &Value,
        target: &ComputerTarget<'_>,
        input: &Value,
    ) -> Result<ToolOutput> {
        match action.as_str() {
            "permissions" => {
                let accessibility = result
                    .get("accessibility_trusted")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let screen = result
                    .get("screen_recording")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let mut text = format!(
                    "Accessibility: {}\nScreen Recording: {}",
                    if accessibility {
                        "granted"
                    } else {
                        "not granted"
                    },
                    if screen { "granted" } else { "not granted" },
                );
                let detail = result
                    .get("detail")
                    .and_then(Value::as_str)
                    .map(|value| value.chars().take(2_000).collect::<String>());
                if let Some(detail) = &detail {
                    text.push('\n');
                    text.push_str(detail);
                }
                Ok(ToolOutput::ok_structured(
                    text,
                    json!({
                        "ok": true,
                        "action": "permissions",
                        "accessibility_trusted": accessibility,
                        "screen_recording": screen,
                        "detail": detail,
                    }),
                ))
            }
            "apps" => {
                let validated = match validate_apps_result(result) {
                    Ok(validated) => validated,
                    Err(error) => {
                        return Ok(ToolOutput::error_structured(
                            format!("computer apps rejected: {error}"),
                            json!({
                                "code": "APPS_REJECTED",
                                "action": "apps",
                                "message": error.to_string(),
                            }),
                        ));
                    }
                };
                let mut text = String::new();
                for app in &validated.apps {
                    let bundle = app.bundle_id.as_deref().unwrap_or("?");
                    text.push_str(&format!(
                        "{}{} ({bundle}) pid {}\n",
                        if app.frontmost { "* " } else { "  " },
                        app.name,
                        app.pid
                    ));
                }
                if text.is_empty() {
                    text.push_str("(no running applications)");
                }
                Ok(ToolOutput::ok_structured(
                    truncate(text),
                    json!({
                        "ok": true,
                        "action": "apps",
                        "apps": validated.apps.iter().map(ValidatedApp::to_value).collect::<Vec<_>>(),
                        "count": validated.apps.len(),
                        "truncated": validated.truncated,
                    }),
                ))
            }
            "displays" => {
                let displays = result
                    .get("displays")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let mut text = String::new();
                for display in displays.iter().take(16) {
                    let index = display.get("index").and_then(Value::as_u64).unwrap_or(0);
                    let id = display.get("id").and_then(Value::as_u64).unwrap_or(0);
                    let width = display.get("width").and_then(Value::as_u64).unwrap_or(0);
                    let height = display.get("height").and_then(Value::as_u64).unwrap_or(0);
                    let name = display.get("name").and_then(Value::as_str).unwrap_or("");
                    let main = display
                        .get("main")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    text.push_str(&format!(
                        "[{index}] id {id} {width}x{height}{} {name}\n",
                        if main { " *main" } else { "" }
                    ));
                }
                if text.is_empty() {
                    text.push_str("(no displays)");
                }
                Ok(ToolOutput::ok_structured(
                    truncate(text),
                    json!({
                        "ok": true,
                        "action": "displays",
                        "displays": displays.iter().take(16).cloned().collect::<Vec<_>>(),
                        "count": displays.len().min(16),
                        "truncated": displays.len() > 16,
                    }),
                ))
            }
            "windows" => {
                let validated = match validate_windows_result(result) {
                    Ok(validated) => validated,
                    Err(error) => {
                        return Ok(ToolOutput::error_structured(
                            format!("computer windows rejected: {error}"),
                            json!({
                                "code": "WINDOWS_REJECTED",
                                "action": "windows",
                                "message": error.to_string(),
                            }),
                        ));
                    }
                };
                let mut text = String::new();
                for window in &validated.windows {
                    let bounds = window.bounds.clone().unwrap_or(Value::Null);
                    text.push_str(&format!(
                        "[{}] {} — {} {}{}\n",
                        window.id,
                        window.app,
                        window.title,
                        render_bounds(&bounds),
                        if window.focused { " focused" } else { "" }
                    ));
                }
                if text.is_empty() {
                    text.push_str("(no windows)");
                }
                Ok(ToolOutput::ok_structured(
                    truncate(text),
                    json!({
                        "ok": true,
                        "action": "windows",
                        "windows": validated.windows.iter().map(ValidatedWindow::to_value).collect::<Vec<_>>(),
                        "count": validated.windows.len(),
                        "truncated": validated.truncated,
                    }),
                ))
            }
            "snapshot" => {
                let limits = SnapshotLimits::from_input(input)?;
                let validated = match validate_snapshot_result(result, limits) {
                    Ok(validated) => validated,
                    Err(error) => {
                        return Ok(ToolOutput::error_structured(
                            format!("computer snapshot rejected: {error}"),
                            json!({
                                "code": "SNAPSHOT_REJECTED",
                                "action": "snapshot",
                                "message": error.to_string(),
                            }),
                        ));
                    }
                };
                let mut text = format!("{} — {}\n", validated.app, validated.window_title);
                render_snapshot_tree(&validated.tree, 0, &mut text);
                let max_chars = optional_u64(input, "max_chars")?
                    .unwrap_or(MAX_SNAPSHOT_CHARS as u64)
                    .clamp(500, MAX_SNAPSHOT_CHARS as u64) as usize;
                let content = truncate(text.chars().take(max_chars).collect::<String>());
                let snapshot_generation = result
                    .get("snapshot_generation")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                Ok(ToolOutput::ok_structured(
                    content,
                    json!({
                        "ok": true,
                        "action": "snapshot",
                        "app": validated.app,
                        "bundle_id": validated.bundle_id,
                        "pid": validated.pid,
                        "window_title": validated.window_title,
                        "window_id": validated.window_id,
                        "node_count": validated.node_count,
                        "bytes": validated.bytes,
                        "truncated": validated.truncated,
                        "generation": result.get("generation").cloned().unwrap_or(Value::Null),
                        "snapshot_generation": snapshot_generation,
                        "ref_format": "ax-g<generation>-<hash>",
                        "stale_ref_policy": "reject_without_retry",
                        "tree": validated.tree,
                    }),
                ))
            }
            "screenshot" => self.render_screenshot(engine, result, target, input).await,
            "focus" => {
                let summary = format!(
                    "focused {} ({})",
                    result
                        .get("bundle_id")
                        .and_then(Value::as_str)
                        .unwrap_or("app"),
                    result
                        .get("window_title")
                        .and_then(Value::as_str)
                        .unwrap_or("window")
                );
                Ok(ToolOutput::ok_structured(
                    summary.clone(),
                    json!({"ok": true, "action": "focus", "summary": summary, "result": result}),
                ))
            }
            "wait" => {
                let summary = format!(
                    "computer wait matched after {} ms",
                    result
                        .get("elapsed_ms")
                        .and_then(Value::as_u64)
                        .unwrap_or(0)
                );
                Ok(ToolOutput::ok_structured(
                    summary,
                    json!({"ok": true, "action": "wait", "result": result}),
                ))
            }
            "click" | "type" | "key" | "scroll" | "drag" => {
                let summary = format!("computer {action} completed");
                Ok(ToolOutput::ok_structured(
                    summary.clone(),
                    json!({
                        "ok": true,
                        "action": action,
                        "summary": summary,
                        "result": result,
                        "next_actions": ["computer snapshot"],
                    }),
                ))
            }
            _ => Ok(ToolOutput::ok_structured(
                format!("computer {action} completed"),
                json!({"ok": true, "action": action, "result": result}),
            )),
        }
    }

    async fn render_screenshot(
        &self,
        engine: &Engine,
        result: &Value,
        target: &ComputerTarget<'_>,
        input: &Value,
    ) -> Result<ToolOutput> {
        let source = result
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("computer helper did not return a screenshot path"))?;
        let source = PathBuf::from(source);
        let source = source
            .canonicalize()
            .with_context(|| format!("resolving computer screenshot {}", source.display()))?;
        let file_metadata = tokio::fs::metadata(&source).await?;
        validate_screenshot_source(&source, &self.helper_scratch(target), file_metadata.len())?;
        let bytes = tokio::fs::read(&source).await?;
        let metadata = match validate_screenshot_metadata(result, &bytes) {
            Ok(metadata) => metadata,
            Err(error) => {
                let _ = tokio::fs::remove_file(&source).await;
                return Err(error);
            }
        };
        let name = string_param(input, "name", 200).unwrap_or_else(|| {
            format!(
                "computer-screenshot-{}.png",
                &Uuid::now_v7().simple().to_string()[..16]
            )
        });
        let artifact = engine
            .publish_artifact_binary(
                target.session_id,
                target.agent_id,
                target.agent_name,
                target.task_id.map(str::to_string),
                &name,
                "screenshot",
                "image/png",
                &bytes,
            )
            .await?;
        let _ = tokio::fs::remove_file(&source).await;
        let path = artifact.path.clone();
        let summary = format!(
            "screenshot saved as artifact {} ({}x{}, {} bytes)",
            artifact.id,
            metadata.width,
            metadata.height,
            bytes.len()
        );
        Ok(ToolOutput::ok_structured(
            summary.clone(),
            json!({
                "ok": true,
                "action": "screenshot",
                "summary": summary,
                "artifact_id": artifact.id,
                "path": path,
                "bytes": bytes.len(),
                "sha256": metadata.sha256,
                "width": metadata.width,
                "height": metadata.height,
                "target": metadata.target,
                "window_id": metadata.window_id,
            }),
        ))
    }

    /// Check the helper binary and its TCC permissions without taking the GUI
    /// lease. This is an inspection path for Settings/Manage Dash; it never
    /// clicks, types, focuses, or captures a screenshot.
    ///
    /// The result is cached for `policy.status_ttl_secs` so a dashboard refresh
    /// does not spawn a temporary helper on every poll. `refresh` bypasses the
    /// cache explicitly.
    pub async fn status(
        &self,
        policy: &vp_protocol::types::GuiPolicy,
        refresh: bool,
    ) -> ComputerStatusResult {
        if !refresh && let Some(cached) = self.status_cache.lock().await.as_ref() {
            let age = cached.fetched.elapsed();
            if age < Duration::from_secs(policy.status_ttl_secs) {
                let mut result = cached.result.clone();
                result.cached = true;
                result.cache_age_ms = Some(age.as_millis() as u64);
                result.grace_secs = policy.grace_secs;
                result.wait_ms = policy.wait_ms;
                return result;
            }
        }
        let _guard = self.status_lock.lock().await;
        if !refresh && let Some(cached) = self.status_cache.lock().await.as_ref() {
            let age = cached.fetched.elapsed();
            if age < Duration::from_secs(policy.status_ttl_secs) {
                let mut result = cached.result.clone();
                result.cached = true;
                result.cache_age_ms = Some(age.as_millis() as u64);
                result.grace_secs = policy.grace_secs;
                result.wait_ms = policy.wait_ms;
                return result;
            }
        }
        let mut result = self.status_with_helper(self.helper_path()).await;
        result.cached = false;
        result.cache_age_ms = Some(0);
        result.grace_secs = policy.grace_secs;
        result.wait_ms = policy.wait_ms;
        result.snapshot_generations = true;
        *self.status_cache.lock().await = Some(CachedStatus {
            result: result.clone(),
            fetched: std::time::Instant::now(),
        });
        result
    }

    /// Bypass the cache and spawn the helper. Tests use this directly.
    async fn status_with_helper(&self, helper_path: Result<PathBuf>) -> ComputerStatusResult {
        let platform_supported = cfg!(target_os = "macos");
        let helper_path = match helper_path {
            Ok(path) => path,
            Err(error) => {
                return ComputerStatusResult {
                    platform_supported,
                    helper_available: false,
                    helper_path: None,
                    permissions: None,
                    permissions_checked: false,
                    lease_surface: gui::GUI_SURFACE.to_string(),
                    error: Some(error.to_string()),
                    ..ComputerStatusResult::default()
                };
            }
        };
        let scratch = self
            .data_dir
            .join("computer-status")
            .join(Uuid::now_v7().to_string());
        let mut helper = match ComputerHelper::spawn(&helper_path, &scratch).await {
            Ok(helper) => helper,
            Err(error) => {
                let _ = tokio::fs::remove_dir_all(&scratch).await;
                return ComputerStatusResult {
                    platform_supported,
                    helper_available: true,
                    helper_path: Some(helper_path.to_string_lossy().to_string()),
                    permissions: None,
                    permissions_checked: false,
                    lease_surface: gui::GUI_SURFACE.to_string(),
                    error: Some(error.to_string()),
                    ..ComputerStatusResult::default()
                };
            }
        };
        let permissions = match helper
            .request("permissions", json!({}), Duration::from_secs(10))
            .await
        {
            Ok(value) => serde_json::from_value::<HelperPermissionResult>(value)
                .ok()
                .map(|result| ComputerPermissions {
                    accessibility: result.accessibility_trusted,
                    screen_recording: result.screen_recording,
                    detail: result.detail,
                }),
            Err(_) => None,
        };
        helper.shutdown().await;
        let _ = tokio::fs::remove_dir_all(&scratch).await;
        ComputerStatusResult {
            platform_supported,
            helper_available: true,
            helper_path: Some(helper_path.to_string_lossy().to_string()),
            permissions_checked: permissions.is_some(),
            permissions,
            lease_surface: gui::GUI_SURFACE.to_string(),
            error: None,
            ..ComputerStatusResult::default()
        }
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
                .filter_map(|key| map.remove(&key))
                .collect::<Vec<_>>()
        };
        let mut stopped = 0usize;
        for helper in stale {
            let mut helper = helper.lock().await;
            helper.shutdown().await;
            stopped += 1;
        }
        stopped
    }

    /// Stop every helper owned by a session before worktree/scratch cleanup.
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
                .filter_map(|key| map.remove(&key))
                .collect::<Vec<_>>()
        };
        let mut stopped = 0usize;
        for helper in stale {
            let mut helper = helper.lock().await;
            helper.shutdown().await;
            stopped += 1;
        }
        stopped
    }

    pub fn idle_default() -> Duration {
        let seconds = std::env::var("VERLINPARK_COMPUTER_IDLE_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(DEFAULT_HELPER_IDLE_SECS);
        Duration::from_secs(seconds)
    }
}

/// Structured helper failure. STALE_REF and SENSITIVE_TARGET are explicit
/// contracts: neither is retried automatically, and a stale ref always carries
/// the helper's current generation so the agent can take a fresh snapshot.
fn protocol_error_output(
    action: &str,
    protocol: Option<&ComputerProtocolError>,
    error: &anyhow::Error,
) -> Value {
    let code = protocol
        .map(|error| error.code.as_str())
        .unwrap_or("COMPUTER_ERROR");
    let details = protocol.and_then(|error| error.details.clone());
    let generation = details
        .as_ref()
        .and_then(|value| value.get("snapshot_generation"))
        .and_then(Value::as_u64);
    let hint = details
        .as_ref()
        .and_then(|value| value.get("hint"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| match code {
            "STALE_REF" => Some(
                "Take a fresh computer snapshot and use the new ref. The action was not retried."
                    .to_string(),
            ),
            "SENSITIVE_TARGET" => Some(
                "Sensitive targets require an explicit bundle_id/window target and operator approval, or a project policy that permits the target."
                    .to_string(),
            ),
            "TARGET_MISMATCH" => Some(
                "The named target does not match the element or window that would receive the action. Name the actual bundle/window."
                    .to_string(),
            ),
            _ => None,
        });
    json!({
        "ok": false,
        "code": code,
        "action": action,
        "message": error.to_string(),
        "retryable": code == "STALE_REF",
        "auto_retry": false,
        "fresh_snapshot_required": code == "STALE_REF",
        "snapshot_generation": generation,
        "hint": hint,
        "details": details,
        "lease_surface": gui::GUI_SURFACE,
    })
}

fn helper_key(target: &ComputerTarget<'_>) -> String {
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

fn short_id() -> String {
    Uuid::now_v7().simple().to_string()[..16].to_string()
}

fn normalize_action(input: &Value) -> Result<String> {
    let action = input
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("snapshot")
        .trim()
        .to_ascii_lowercase();
    if !matches!(
        action.as_str(),
        "permissions"
            | "apps"
            | "windows"
            | "displays"
            | "focus"
            | "snapshot"
            | "screenshot"
            | "click"
            | "type"
            | "key"
            | "scroll"
            | "drag"
            | "wait"
            | "close"
    ) {
        bail!("unsupported computer action: {action}");
    }
    Ok(action)
}

fn string_param(input: &Value, name: &str, max: usize) -> Option<String> {
    input
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.chars().take(max).collect())
}

fn required_string(input: &Value, name: &str) -> Result<String> {
    let value = input
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("{name} is required"))?;
    if value.trim().is_empty() {
        bail!("{name} is empty");
    }
    Ok(value.to_string())
}

fn optional_u64(input: &Value, name: &str) -> Result<Option<u64>> {
    match input.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| anyhow!("{name} must be a non-negative integer")),
    }
}

fn optional_i64(input: &Value, name: &str) -> Result<Option<i64>> {
    match input.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_i64()
            .map(Some)
            .ok_or_else(|| anyhow!("{name} must be an integer")),
    }
}

fn optional_f64(input: &Value, name: &str) -> Result<Option<f64>> {
    match input.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_f64()
            .filter(|value| value.is_finite())
            .map(Some)
            .ok_or_else(|| anyhow!("{name} must be a finite number")),
    }
}

fn bool_param(input: &Value, name: &str) -> Option<bool> {
    input.get(name).and_then(Value::as_bool)
}

fn validate_normalized(value: f64, name: &str) -> Result<()> {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        bail!("{name} must be a finite normalized coordinate between 0 and 1");
    }
    Ok(())
}

fn validate_ref(reference: &str) -> Result<()> {
    if !reference.starts_with("ax-") || reference.len() > 64 {
        bail!("ref must be an element reference such as ax-00000007-0123456789abcdef");
    }
    let suffix = &reference[3..];
    // Current helper refs bind the snapshot generation:
    //   ax-<8 hex generation>-<16 hex hash>
    // Legacy deterministic refs without a generation remain readable so a
    // warm helper from an older build does not turn every ref into a protocol
    // error. The helper still checks membership in its latest snapshot.
    if let Some((generation, hash)) = suffix.split_once('-') {
        let generation_ok =
            generation.len() == 8 && generation.chars().all(|value| value.is_ascii_hexdigit());
        let hash_ok =
            (16..=32).contains(&hash.len()) && hash.chars().all(|value| value.is_ascii_hexdigit());
        if generation_ok && hash_ok {
            return Ok(());
        }
        bail!("ref must be an element reference such as ax-00000007-0123456789abcdef");
    }
    if !(8..=32).contains(&suffix.len()) || !suffix.chars().all(|value| value.is_ascii_hexdigit()) {
        bail!("ref must be an element reference such as ax-00000007-0123456789abcdef");
    }
    Ok(())
}

/// Parse the snapshot generation embedded in a current ref. Legacy refs return
/// `None`; the helper remains authoritative and rejects them when they are not
/// in its latest snapshot.
fn ref_generation(reference: &str) -> Option<u64> {
    let suffix = reference.strip_prefix("ax-")?;
    let (generation, _hash) = suffix.split_once('-')?;
    u64::from_str_radix(generation, 16).ok()
}

fn modifiers_param(input: &Value) -> Result<Vec<String>> {
    let Some(value) = input.get("modifiers") else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let values = value
        .as_array()
        .ok_or_else(|| anyhow!("modifiers must be an array"))?;
    if values.len() > 4 {
        bail!("at most four modifiers are supported");
    }
    let mut result = Vec::new();
    for value in values {
        let raw = value
            .as_str()
            .ok_or_else(|| anyhow!("modifiers must be strings"))?;
        let normalized = normalize_modifier(raw)?;
        if !result.contains(&normalized) {
            result.push(normalized);
        }
    }
    Ok(result)
}

fn validate_key(raw: &str) -> Result<()> {
    let key = raw.trim().to_ascii_lowercase();
    const KEYS: &[&str] = &[
        "return",
        "enter",
        "tab",
        "space",
        "delete",
        "backspace",
        "escape",
        "esc",
        "forwarddelete",
        "left",
        "right",
        "down",
        "up",
        "home",
        "end",
        "pageup",
        "pagedown",
        "f1",
        "f2",
        "f3",
        "f4",
        "f5",
        "f6",
        "f7",
        "f8",
        "f9",
        "f10",
        "f11",
        "f12",
        "a",
        "s",
        "d",
        "f",
        "h",
        "g",
        "z",
        "x",
        "c",
        "v",
        "b",
        "q",
        "w",
        "e",
        "r",
        "y",
        "t",
        "1",
        "2",
        "3",
        "4",
        "6",
        "5",
        "=",
        "9",
        "7",
        "-",
        "8",
        "0",
        "]",
        "o",
        "u",
        "[",
        "i",
        "p",
        "l",
        "j",
        "'",
        "k",
        ";",
        "\\",
        ",",
        "/",
        "n",
        "m",
        ".",
        "`",
    ];
    if KEYS.contains(&key.as_str()) {
        Ok(())
    } else {
        bail!("unsupported key: {raw}")
    }
}

fn normalize_modifier(raw: &str) -> Result<String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "cmd" | "command" | "meta" | "super" => Ok("command".to_string()),
        "shift" => Ok("shift".to_string()),
        "alt" | "option" | "opt" => Ok("option".to_string()),
        "ctrl" | "control" => Ok("control".to_string()),
        other => bail!("unsupported key modifier: {other}"),
    }
}

/// Inspect a raw agent `key` input for the destructive-shortcut policy. This is
/// the form used by the permission broker before approval.
pub(crate) fn destructive_key_input(input: &Value) -> bool {
    let key = input.get("key").and_then(Value::as_str).unwrap_or_default();
    let modifiers = modifiers_param(input).unwrap_or_default();
    destructive_key_combo(key, &modifiers)
}

/// The same destructive-shortcut policy as the Swift helper. The daemon uses
/// this before approval; the helper repeats it before posting the event.
pub fn destructive_key_combo(key: &str, modifiers: &[String]) -> bool {
    let key = key.trim().to_ascii_lowercase();
    let command = modifiers.iter().any(|value| value == "command");
    let shift = modifiers.iter().any(|value| value == "shift");
    let option = modifiers.iter().any(|value| value == "option");
    let control = modifiers.iter().any(|value| value == "control");
    if command && key == "q" {
        return true;
    }
    if command && shift && matches!(key.as_str(), "delete" | "backspace" | "forwarddelete") {
        return true;
    }
    if control && command && key == "q" {
        return true;
    }
    if command && option && key == "escape" {
        return true;
    }
    false
}

fn attach_ref_generation(reference: &str, params: &mut Value) {
    if let Some(generation) = ref_generation(reference) {
        params["ref_generation"] = json!(generation);
    }
}

fn apply_window_target(input: &Value, params: &mut Value) -> Result<()> {
    if let Some(bundle_id) = string_param(input, "bundle_id", 200) {
        params["bundle_id"] = json!(bundle_id);
    }
    if let Some(title) = string_param(input, "window_title", 200) {
        params["window_title"] = json!(title);
    }
    if let Some(window_id) = optional_u64(input, "window_id")? {
        if window_id == 0 || window_id > u32::MAX as u64 {
            bail!("window_id is out of range");
        }
        params["window_id"] = json!(window_id);
    }
    if let Some(pid) = optional_u64(input, "pid")? {
        if pid == 0 || pid > i32::MAX as u64 {
            bail!("pid is out of range");
        }
        params["pid"] = json!(pid);
    }
    Ok(())
}

fn action_deadline(action: &str, input: &Value) -> Duration {
    match action {
        "screenshot" => SCREENSHOT_TIMEOUT,
        "wait" => {
            let wait = input
                .get("timeout_ms")
                .and_then(Value::as_u64)
                .unwrap_or(5_000)
                .clamp(50, 30_000);
            Duration::from_millis(wait).saturating_add(Duration::from_secs(10))
        }
        "snapshot" => Duration::from_secs(20),
        _ => DEFAULT_ACTION_TIMEOUT,
    }
}

#[derive(Debug, Clone, PartialEq)]
struct ScreenshotMetadata {
    width: u64,
    height: u64,
    sha256: String,
    target: String,
    window_id: Option<u64>,
}

fn validate_screenshot_metadata(result: &Value, bytes: &[u8]) -> Result<ScreenshotMetadata> {
    if bytes.is_empty() {
        bail!("computer screenshot is empty");
    }
    let sha256 = sha256_hex(bytes);
    if let Some(reported) = result.get("sha256").and_then(Value::as_str)
        && reported != sha256
    {
        bail!("computer screenshot digest did not match the helper response");
    }
    let width = result.get("width").and_then(Value::as_u64).unwrap_or(0);
    let height = result.get("height").and_then(Value::as_u64).unwrap_or(0);
    if width == 0 || height == 0 || width > 4_096 || height > 4_096 {
        bail!("computer screenshot dimensions are missing or out of range");
    }
    let target = result
        .get("target")
        .and_then(Value::as_str)
        .unwrap_or("window")
        .to_ascii_lowercase();
    if !matches!(target.as_str(), "window" | "screen") {
        bail!("computer screenshot target is not window or screen");
    }
    let window_id = result.get("window_id").and_then(Value::as_u64);
    Ok(ScreenshotMetadata {
        width,
        height,
        sha256,
        target,
        window_id,
    })
}

#[derive(Debug, Clone)]
struct ValidatedApp {
    bundle_id: Option<String>,
    name: String,
    pid: i64,
    frontmost: bool,
    hidden: bool,
    activation_policy: String,
}

impl ValidatedApp {
    fn from_value(value: &Value) -> Option<Self> {
        let name = value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .chars()
            .take(200)
            .collect::<String>();
        let pid = value.get("pid").and_then(Value::as_i64)?;
        if pid <= 0 {
            return None;
        }
        Some(Self {
            bundle_id: value
                .get("bundle_id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(|value| value.chars().take(200).collect()),
            name,
            pid,
            frontmost: value
                .get("frontmost")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            hidden: value
                .get("hidden")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            activation_policy: value
                .get("activation_policy")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .chars()
                .take(40)
                .collect(),
        })
    }

    fn to_value(&self) -> Value {
        json!({
            "bundle_id": self.bundle_id,
            "name": self.name,
            "pid": self.pid,
            "frontmost": self.frontmost,
            "hidden": self.hidden,
            "activation_policy": self.activation_policy,
        })
    }
}

struct ValidatedApps {
    apps: Vec<ValidatedApp>,
    truncated: bool,
}

fn validate_apps_result(result: &Value) -> Result<ValidatedApps> {
    let values = result
        .get("apps")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("computer apps result did not include an apps array"))?;
    let truncated = values.len() > 200;
    let apps = values
        .iter()
        .take(200)
        .filter_map(ValidatedApp::from_value)
        .collect::<Vec<_>>();
    Ok(ValidatedApps { apps, truncated })
}

#[derive(Debug, Clone)]
struct ValidatedWindow {
    id: u64,
    pid: i64,
    app: String,
    title: String,
    bounds: Option<Value>,
    layer: i64,
    focused: bool,
    on_screen: bool,
}

impl ValidatedWindow {
    fn from_value(value: &Value) -> Option<Self> {
        let id = value.get("id").and_then(Value::as_u64)?;
        if id == 0 {
            return None;
        }
        Some(Self {
            id,
            pid: value.get("pid").and_then(Value::as_i64).unwrap_or(0),
            app: value
                .get("app")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .chars()
                .take(200)
                .collect(),
            title: value
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .chars()
                .take(200)
                .collect(),
            bounds: validated_frame(value.get("bounds")),
            layer: value.get("layer").and_then(Value::as_i64).unwrap_or(0),
            focused: value
                .get("focused")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            on_screen: value
                .get("on_screen")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }

    fn to_value(&self) -> Value {
        json!({
            "id": self.id,
            "pid": self.pid,
            "app": self.app,
            "title": self.title,
            "bounds": self.bounds,
            "layer": self.layer,
            "focused": self.focused,
            "on_screen": self.on_screen,
        })
    }
}

struct ValidatedWindows {
    windows: Vec<ValidatedWindow>,
    truncated: bool,
}

fn validate_windows_result(result: &Value) -> Result<ValidatedWindows> {
    let values = result
        .get("windows")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("computer windows result did not include a windows array"))?;
    let truncated = values.len() > 100;
    let windows = values
        .iter()
        .take(100)
        .filter_map(ValidatedWindow::from_value)
        .collect::<Vec<_>>();
    Ok(ValidatedWindows { windows, truncated })
}

#[derive(Debug, Clone, Copy)]
struct SnapshotLimits {
    max_depth: usize,
    max_elements: usize,
    max_bytes: usize,
}

impl SnapshotLimits {
    fn from_input(input: &Value) -> Result<Self> {
        Ok(Self {
            max_depth: optional_u64(input, "max_depth")?.unwrap_or(10).clamp(1, 20) as usize,
            max_elements: optional_u64(input, "max_elements")?
                .unwrap_or(400)
                .clamp(1, 1_200) as usize,
            max_bytes: optional_u64(input, "max_bytes")?
                .unwrap_or(131_072)
                .clamp(1_024, 262_144) as usize,
        })
    }
}

struct SnapshotBudgetRust {
    limits: SnapshotLimits,
    elements: usize,
    estimated_bytes: usize,
    truncated: bool,
}

impl SnapshotBudgetRust {
    fn new(limits: SnapshotLimits) -> Self {
        Self {
            limits,
            elements: 0,
            estimated_bytes: 0,
            truncated: false,
        }
    }

    fn admit(&mut self, depth: usize, bytes: usize) -> bool {
        if depth > self.limits.max_depth {
            self.truncated = true;
            return false;
        }
        if self.elements >= self.limits.max_elements {
            self.truncated = true;
            return false;
        }
        if self.estimated_bytes.saturating_add(bytes) > self.limits.max_bytes {
            self.truncated = true;
            return false;
        }
        self.elements += 1;
        self.estimated_bytes = self.estimated_bytes.saturating_add(bytes);
        true
    }
}

#[derive(Debug, Clone)]
struct ValidatedSnapshot {
    app: String,
    bundle_id: Option<String>,
    pid: Option<i64>,
    window_title: String,
    window_id: Option<u64>,
    tree: Value,
    node_count: usize,
    bytes: usize,
    truncated: bool,
}

fn validate_snapshot_result(result: &Value, limits: SnapshotLimits) -> Result<ValidatedSnapshot> {
    let tree = result
        .get("tree")
        .ok_or_else(|| anyhow!("computer snapshot did not include a tree"))?;
    let mut budget = SnapshotBudgetRust::new(limits);
    let tree = sanitize_snapshot_node(tree, 0, &mut budget)
        .ok_or_else(|| anyhow!("computer snapshot tree is empty or over budget"))?;
    let bytes = serde_json::to_vec(&tree)
        .context("serializing validated computer snapshot")?
        .len();
    if bytes > limits.max_bytes {
        bail!(
            "computer snapshot result exceeds the {} byte cap",
            limits.max_bytes
        );
    }
    Ok(ValidatedSnapshot {
        app: bounded_string(result.get("app"), 200),
        bundle_id: result
            .get("bundle_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(|value| value.chars().take(200).collect()),
        pid: result.get("pid").and_then(Value::as_i64),
        window_title: bounded_string(result.get("window_title"), 200),
        window_id: result.get("window_id").and_then(Value::as_u64),
        tree,
        node_count: budget.elements,
        bytes,
        truncated: budget.truncated
            || result
                .get("truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
    })
}

fn sanitize_snapshot_node(
    node: &Value,
    depth: usize,
    budget: &mut SnapshotBudgetRust,
) -> Option<Value> {
    let role = node
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("AXUnknown");
    let role = role.chars().take(80).collect::<String>();
    let subrole = node
        .get("subrole")
        .and_then(Value::as_str)
        .map(|value| value.chars().take(80).collect::<String>());
    let identifier = node
        .get("identifier")
        .and_then(Value::as_str)
        .map(|value| value.chars().take(200).collect::<String>());
    let title = bounded_string(node.get("title"), 200);
    let label = node
        .get("label")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(200).collect::<String>());
    let mut value = node
        .get("value")
        .and_then(Value::as_str)
        .map(|value| value.chars().take(200).collect::<String>());
    let secure = role == "AXSecureTextField"
        || role == "AXSecureTextArea"
        || subrole.as_deref() == Some("AXSecureTextField")
        || subrole.as_deref() == Some("AXSecureTextArea");
    if secure && value.is_some() {
        value = Some("***".to_string());
    }
    let reference = node
        .get("ref")
        .and_then(Value::as_str)
        .filter(|reference| validate_ref(reference).is_ok())
        .map(str::to_string);
    let estimated = role.len()
        + subrole.as_ref().map(String::len).unwrap_or(0)
        + identifier.as_ref().map(String::len).unwrap_or(0)
        + title.len()
        + label.as_ref().map(String::len).unwrap_or(0)
        + value.as_ref().map(String::len).unwrap_or(0)
        + reference.as_ref().map(String::len).unwrap_or(0)
        + 96;
    if !budget.admit(depth, estimated) {
        return None;
    }

    let mut object = serde_json::Map::new();
    object.insert("role".to_string(), json!(role));
    if let Some(reference) = reference {
        object.insert("ref".to_string(), json!(reference));
    }
    if let Some(subrole) = subrole {
        object.insert("subrole".to_string(), json!(subrole));
    }
    if let Some(identifier) = identifier {
        object.insert("identifier".to_string(), json!(identifier));
    }
    if !title.is_empty() {
        object.insert("title".to_string(), json!(title));
    }
    if let Some(label) = label {
        object.insert("label".to_string(), json!(label));
    }
    if let Some(value) = value {
        object.insert("value".to_string(), json!(value));
    }
    for (key, name) in [
        ("enabled", "enabled"),
        ("focused", "focused"),
        ("selected", "selected"),
    ] {
        if let Some(value) = node.get(key).and_then(Value::as_bool) {
            object.insert(name.to_string(), json!(value));
        }
    }
    if let Some(frame) = validated_frame(node.get("frame")) {
        object.insert("frame".to_string(), frame);
    }

    let mut truncated = node
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let children = node.get("children").and_then(Value::as_array);
    if depth >= budget.limits.max_depth {
        if children.is_some_and(|children| !children.is_empty()) {
            truncated = true;
            budget.truncated = true;
        }
    } else if let Some(children) = children {
        if children.len() > 200 {
            truncated = true;
        }
        let mut kept = Vec::new();
        for child in children.iter().take(200) {
            if let Some(child) = sanitize_snapshot_node(child, depth + 1, budget) {
                kept.push(child);
            } else {
                truncated = true;
            }
        }
        if !kept.is_empty() {
            object.insert("children".to_string(), Value::Array(kept));
        }
    }
    if truncated {
        object.insert("truncated".to_string(), Value::Bool(true));
    }
    Some(Value::Object(object))
}

fn validated_frame(frame: Option<&Value>) -> Option<Value> {
    let frame = frame?.as_object()?;
    let x = frame.get("x").and_then(Value::as_f64)?;
    let y = frame.get("y").and_then(Value::as_f64)?;
    let width = frame.get("width").and_then(Value::as_f64)?;
    let height = frame.get("height").and_then(Value::as_f64)?;
    if !x.is_finite() || !y.is_finite() || !width.is_finite() || !height.is_finite() {
        return None;
    }
    Some(json!({"x": x, "y": y, "width": width, "height": height}))
}

fn bounded_string(value: Option<&Value>, max: usize) -> String {
    value
        .and_then(Value::as_str)
        .map(|value| value.chars().take(max).collect())
        .unwrap_or_default()
}

fn render_bounds(bounds: &Value) -> String {
    let x = bounds.get("x").and_then(Value::as_f64).unwrap_or(0.0);
    let y = bounds.get("y").and_then(Value::as_f64).unwrap_or(0.0);
    let width = bounds.get("width").and_then(Value::as_f64).unwrap_or(0.0);
    let height = bounds.get("height").and_then(Value::as_f64).unwrap_or(0.0);
    format!("({x:.0},{y:.0} {width:.0}x{height:.0})")
}

fn render_snapshot_tree(node: &Value, depth: usize, output: &mut String) {
    if depth > 40 || output.len() > MAX_SNAPSHOT_CHARS {
        return;
    }
    let role = node
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("AXUnknown");
    let title = node.get("title").and_then(Value::as_str).unwrap_or("");
    let label = node.get("label").and_then(Value::as_str).unwrap_or("");
    let value = node.get("value").and_then(Value::as_str).unwrap_or("");
    let reference = node.get("ref").and_then(Value::as_str).unwrap_or("");
    let indent = "  ".repeat(depth.min(20));
    output.push_str(&indent);
    if !reference.is_empty() {
        output.push('[');
        output.push_str(reference);
        output.push_str("] ");
    }
    output.push_str(role);
    if !title.is_empty() {
        output.push_str(&format!(" \"{title}\""));
    }
    if !label.is_empty() && label != title {
        let label = label.chars().take(120).collect::<String>();
        output.push_str(&format!(" — {label}"));
    }
    if !value.is_empty() && value != title && value != "***" {
        let value = value.chars().take(80).collect::<String>();
        output.push_str(&format!(" value=\"{value}\""));
    } else if value == "***" {
        output.push_str(" value=\"***\"");
    }
    if node.get("focused").and_then(Value::as_bool) == Some(true) {
        output.push_str(" focused");
    }
    if node.get("enabled").and_then(Value::as_bool) == Some(false) {
        output.push_str(" disabled");
    }
    if let Some(frame) = node.get("frame") {
        output.push(' ');
        output.push_str(&render_bounds(frame));
    }
    if node.get("truncated").and_then(Value::as_bool) == Some(true) {
        output.push_str(" …");
    }
    output.push('\n');
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for child in children {
            render_snapshot_tree(child, depth + 1, output);
        }
    }
}

fn validate_screenshot_source(source: &Path, scratch: &Path, bytes: u64) -> Result<()> {
    if bytes == 0 {
        bail!("computer screenshot is empty");
    }
    if bytes > MAX_SCREENSHOT_BYTES {
        bail!(
            "computer screenshot exceeds {} MiB",
            MAX_SCREENSHOT_BYTES / 1024 / 1024
        );
    }
    let scratch = scratch
        .canonicalize()
        .with_context(|| format!("resolving computer scratch {}", scratch.display()))?;
    if !source.starts_with(&scratch) {
        bail!("computer screenshot path escaped the helper scratch directory");
    }
    Ok(())
}

async fn read_bounded_frame<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> Result<Option<String>> {
    let mut output = Vec::new();
    loop {
        let (chunk, newline) = {
            let available = reader.fill_buf().await?;
            if available.is_empty() {
                if output.is_empty() {
                    return Ok(None);
                }
                bail!("computer helper closed in the middle of a response frame");
            }
            match available.iter().position(|byte| *byte == b'\n') {
                Some(position) => (available[..=position].to_vec(), true),
                None => (available.to_vec(), false),
            }
        };
        reader.consume(chunk.len());
        if output.len() + chunk.len() > max {
            bail!("computer helper response frame exceeds {max} bytes");
        }
        if newline {
            output.extend_from_slice(&chunk[..chunk.len().saturating_sub(1)]);
            if output.last() == Some(&b'\r') {
                output.pop();
            }
            return Ok(Some(
                String::from_utf8(output).context("computer helper frame is not UTF-8")?,
            ));
        }
        output.extend_from_slice(&chunk);
    }
}

fn find_helper_binary() -> Result<PathBuf> {
    let explicit = std::env::var_os("VERLINPARK_COMPUTER_HELPER");
    let mut candidates = Vec::new();
    if let Ok(current) = std::env::current_exe()
        && let Some(parent) = current.parent()
    {
        candidates.push(parent.join(HELPER_BINARY));
        if let Some(parent) = parent.parent() {
            candidates.push(parent.join("Helpers").join(HELPER_BINARY));
            candidates.push(parent.join(HELPER_BINARY));
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
    let path_env = std::env::var_os("PATH");
    find_helper_binary_in(explicit.as_deref(), candidates, path_env.as_deref())
}

fn find_helper_binary_in(
    explicit: Option<&OsStr>,
    mut candidates: Vec<PathBuf>,
    path_env: Option<&OsStr>,
) -> Result<PathBuf> {
    if let Some(explicit) = explicit {
        let path = PathBuf::from(explicit);
        if is_executable(&path) {
            return Ok(path);
        }
        bail!(
            "VERLINPARK_COMPUTER_HELPER does not point to an executable: {}",
            path.display()
        );
    }
    if let Some(paths) = path_env {
        for directory in std::env::split_paths(paths) {
            candidates.push(directory.join(HELPER_BINARY));
        }
    }
    for candidate in candidates {
        if is_executable(&candidate) {
            return Ok(candidate);
        }
    }
    bail!(
        "VerlinParkComputerHelper is not installed; build the macOS helper target or set VERLINPARK_COMPUTER_HELPER"
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
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;
    use tokio::io::AsyncWriteExt;
    use vp_store::Store;

    fn target<'a>(session: &'a str, worktree: &'a Path) -> ComputerTarget<'a> {
        ComputerTarget {
            session_id: session,
            worktree,
            agent_id: "agent-1",
            agent_name: "Agent One",
            task_id: None,
        }
    }

    fn manager() -> (ComputerManager, TempDir) {
        let data = TempDir::new().unwrap();
        (ComputerManager::new(data.path().to_path_buf()), data)
    }

    fn init_repo(path: &Path) {
        std::fs::write(path.join("README.md"), "hello\n").unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "test@verlinpark.local"],
            vec!["config", "user.name", "VerlinPark Test"],
            vec!["add", "."],
            vec!["commit", "-q", "-m", "initial"],
        ] {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(path)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success());
        }
    }

    fn write_script(path: &Path, body: &str) {
        std::fs::write(path, body).unwrap();
        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).unwrap();
    }

    #[test]
    fn normalizes_and_rejects_actions() {
        assert_eq!(
            normalize_action(&json!({"action": " Snapshot "})).unwrap(),
            "snapshot"
        );
        assert_eq!(normalize_action(&json!({})).unwrap(), "snapshot");
        assert!(normalize_action(&json!({"action": "shell"})).is_err());
        assert!(normalize_action(&json!({"action": "open"})).is_err());
    }

    #[test]
    fn click_parameters_are_strictly_one_target() {
        let data = TempDir::new().unwrap();
        let (manager, _data) = manager();
        let target = target("ses", data.path());
        let by_ref = manager
            .params_for(
                "click",
                &target,
                &json!({"ref": "ax-0123456789abcdef", "click_count": 9}),
            )
            .unwrap();
        assert_eq!(by_ref["ref"], "ax-0123456789abcdef");
        assert_eq!(by_ref["click_count"], 2);
        let by_point = manager
            .params_for(
                "click",
                &target,
                &json!({"x": 0.25, "y": 0.75, "button": "right"}),
            )
            .unwrap();
        assert_eq!(by_point["x"], 0.25);
        assert_eq!(by_point["button"], "right");
        assert!(
            manager
                .params_for(
                    "click",
                    &target,
                    &json!({"x": 0.1, "y": 0.2, "ref": "ax-0123456789abcdef"})
                )
                .is_err()
        );
        assert!(
            manager
                .params_for("click", &target, &json!({"x": f64::NAN, "y": 0.2}))
                .is_err()
        );
        assert!(
            manager
                .params_for("click", &target, &json!({"x": 1.2, "y": 0.2}))
                .is_err()
        );
        assert!(
            manager
                .params_for("click", &target, &json!({"ref": "e1"}))
                .is_err()
        );
    }

    #[test]
    fn type_and_key_parameters_are_bounded() {
        let data = TempDir::new().unwrap();
        let (manager, _data) = manager();
        let target = target("ses", data.path());
        let typed = manager
            .params_for("type", &target, &json!({"text": "hello"}))
            .unwrap();
        assert_eq!(typed["text"], "hello");
        assert!(
            manager
                .params_for(
                    "type",
                    &target,
                    &json!({"text": "a".repeat(MAX_TEXT_BYTES + 1)})
                )
                .is_err()
        );
        assert!(
            manager
                .params_for("type", &target, &json!({"text": "bad\u{0}"}))
                .is_err()
        );
        let key = manager
            .params_for("key", &target, &json!({"key": "q", "modifiers": ["cmd"]}))
            .unwrap();
        assert_eq!(key["key"], "q");
        assert_eq!(key["modifiers"], json!(["command"]));
        assert_eq!(key["allow_destructive"], true);
        assert!(
            manager
                .params_for("key", &target, &json!({"key": "volumeup"}))
                .is_err()
        );
        assert!(
            manager
                .params_for("key", &target, &json!({"key": "q", "modifiers": ["hyper"]}))
                .is_err()
        );
        assert!(
            manager
                .params_for(
                    "key",
                    &target,
                    &json!({"key": "q", "modifiers": ["cmd", "shift", "alt", "ctrl", "fn"]})
                )
                .is_err()
        );
    }

    #[test]
    fn destructive_key_combinations_are_detected() {
        assert!(destructive_key_combo("q", &["command".to_string()]));
        assert!(destructive_key_combo(
            "delete",
            &["command".to_string(), "shift".to_string()]
        ));
        assert!(destructive_key_combo(
            "escape",
            &["command".to_string(), "option".to_string()]
        ));
        assert!(destructive_key_combo(
            "q",
            &["control".to_string(), "command".to_string()]
        ));
        assert!(!destructive_key_combo("q", &[]));
        assert!(!destructive_key_combo("w", &["command".to_string()]));
    }

    #[test]
    fn scroll_and_drag_parameters_are_bounded() {
        let data = TempDir::new().unwrap();
        let (manager, _data) = manager();
        let target = target("ses", data.path());
        let scroll = manager
            .params_for(
                "scroll",
                &target,
                &json!({"dx": 5, "dy": -3, "unit": "pixel"}),
            )
            .unwrap();
        assert_eq!(scroll["dx"], 5);
        assert!(
            manager
                .params_for("scroll", &target, &json!({"dx": 0, "dy": 0}))
                .is_err()
        );
        assert!(
            manager
                .params_for("scroll", &target, &json!({"dy": 1_001}))
                .is_err()
        );
        let drag = manager
            .params_for(
                "drag",
                &target,
                &json!({
                    "from_x": 0.1,
                    "from_y": 0.2,
                    "to_x": 0.8,
                    "to_y": 0.9,
                    "duration_ms": 3_000,
                    "steps": 100,
                }),
            )
            .unwrap();
        assert_eq!(drag["duration_ms"], 2_000);
        assert_eq!(drag["steps"], 60);
        assert!(
            manager
                .params_for(
                    "drag",
                    &target,
                    &json!({"from_x": 0.1, "from_y": 0.2, "to_x": 0.8})
                )
                .is_err()
        );
    }

    #[test]
    fn snapshot_and_wait_parameters_are_clamped() {
        let data = TempDir::new().unwrap();
        let (manager, _data) = manager();
        let target = target("ses", data.path());
        let snapshot = manager
            .params_for(
                "snapshot",
                &target,
                &json!({"max_depth": 100, "max_elements": 100_000, "max_bytes": 1_000_000}),
            )
            .unwrap();
        assert_eq!(snapshot["max_depth"], 20);
        assert_eq!(snapshot["max_elements"], 1_200);
        assert_eq!(snapshot["max_bytes"], 262_144);
        let wait = manager
            .params_for(
                "wait",
                &target,
                &json!({"condition": "element", "role": "AXButton", "timeout_ms": 10}),
            )
            .unwrap();
        assert_eq!(wait["timeout_ms"], 50);
        assert!(
            manager
                .params_for("wait", &target, &json!({"condition": "element"}))
                .is_err()
        );
        assert!(
            manager
                .params_for("wait", &target, &json!({"condition": "ref", "ref": "e1"}))
                .is_err()
        );
        assert!(
            manager
                .params_for("wait", &target, &json!({"condition": "app_frontmost"}))
                .is_err()
        );
    }

    #[test]
    fn screenshot_output_is_generated_in_helper_scratch() {
        let data = TempDir::new().unwrap();
        let (manager, _data) = manager();
        let target = target("ses", data.path());
        let params = manager
            .params_for(
                "screenshot",
                &target,
                &json!({"target": "screen", "max_width": 10_000, "max_height": 10}),
            )
            .unwrap();
        assert_eq!(params["target"], "screen");
        assert_eq!(params["max_width"], 2_560);
        assert_eq!(params["max_height"], 320);
        let output = params["output"].as_str().unwrap();
        assert!(output.starts_with("screenshots/computer-"));
        assert!(output.ends_with(".png"));
        assert!(
            manager
                .params_for("screenshot", &target, &json!({"target": "desktop"}))
                .is_err()
        );
    }

    #[test]
    fn screenshot_source_rejects_a_symlink_that_leaves_scratch() {
        let data = TempDir::new().unwrap();
        let scratch = data.path().join("scratch");
        let outside = data.path().join("outside");
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let target = outside.join("a.png");
        std::fs::write(&target, b"png").unwrap();
        let link = scratch.join("screenshots");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let source = link.join("a.png");
        let source = source.canonicalize().unwrap();
        let error = validate_screenshot_source(&source, &scratch, 3).unwrap_err();
        assert!(error.to_string().contains("escaped"));
    }

    #[test]
    fn screenshot_source_must_be_below_scratch_and_bounded() {
        let data = TempDir::new().unwrap();
        let scratch = data.path().join("scratch");
        std::fs::create_dir_all(scratch.join("screenshots")).unwrap();
        let inside = scratch.join("screenshots/a.png");
        std::fs::write(&inside, b"png").unwrap();
        let inside = inside.canonicalize().unwrap();
        validate_screenshot_source(&inside, &scratch, 3).unwrap();
        let outside = data.path().join("outside.png");
        std::fs::write(&outside, b"png").unwrap();
        let outside = outside.canonicalize().unwrap();
        assert!(validate_screenshot_source(&outside, &scratch, 3).is_err());
        assert!(validate_screenshot_source(&inside, &scratch, 0).is_err());
        assert!(validate_screenshot_source(&inside, &scratch, MAX_SCREENSHOT_BYTES + 1).is_err());
    }

    #[tokio::test]
    async fn bounded_frames_parse_and_reject_oversized_lines() {
        let (client, mut server) = tokio::io::duplex(1_024);
        server.write_all(b"{\"id\":\"1\"}\n").await.unwrap();
        let mut reader = BufReader::new(client);
        let frame = read_bounded_frame(&mut reader, 1_024).await.unwrap();
        assert_eq!(frame.as_deref(), Some("{\"id\":\"1\"}"));

        let (client, mut server) = tokio::io::duplex(1_024);
        server.write_all(b"0123456789\n").await.unwrap();
        let mut reader = BufReader::new(client);
        let error = read_bounded_frame(&mut reader, 8).await.unwrap_err();
        assert!(error.to_string().contains("exceeds"));
    }

    #[test]
    fn helper_response_parses_success_error_and_events() {
        let success: HelperMessage =
            serde_json::from_str(r#"{"id":"1","ok":true,"result":{"apps":[]}}"#).unwrap();
        assert_eq!(success.ok, Some(true));
        assert_eq!(success.result.unwrap()["apps"], json!([]));
        let error: HelperMessage = serde_json::from_str(
            r#"{"id":"2","ok":false,"error":{"code":"STALE_REF","message":"gone"}}"#,
        )
        .unwrap();
        assert_eq!(error.error.unwrap().code, "STALE_REF");
        let event: HelperMessage =
            serde_json::from_str(r#"{"event":"ready","version":"1"}"#).unwrap();
        assert_eq!(event.event.as_deref(), Some("ready"));
    }

    #[test]
    fn helper_path_resolution_is_explicit_and_fails_closed() {
        let data = TempDir::new().unwrap();
        let helper = data.path().join(HELPER_BINARY);
        write_script(&helper, "#!/bin/sh\nexit 0\n");
        let found = find_helper_binary_in(None, vec![helper.clone()], None).unwrap();
        assert_eq!(found, helper);
        let error = find_helper_binary_in(
            Some(OsStr::new("/definitely/missing/VerlinParkComputerHelper")),
            vec![helper],
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("VERLINPARK_COMPUTER_HELPER"));
        let error =
            find_helper_binary_in(None, Vec::new(), Some(OsStr::new("/definitely/missing")))
                .unwrap_err();
        assert!(error.to_string().contains("is not installed"));
    }

    #[test]
    fn helper_path_resolution_scans_path_entries() {
        let data = TempDir::new().unwrap();
        let bin = data.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let helper = bin.join(HELPER_BINARY);
        write_script(&helper, "#!/bin/sh\nexit 0\n");
        let found = find_helper_binary_in(None, Vec::new(), Some(bin.as_os_str())).unwrap();
        assert_eq!(found, helper);
    }

    #[test]
    fn render_snapshot_is_bounded_and_shows_redaction() {
        let tree = json!({
            "ref": "ax-1111111111111111",
            "role": "AXWindow",
            "title": "Main",
            "children": [
                {
                    "ref": "ax-2222222222222222",
                    "role": "AXTextField",
                    "title": "Password",
                    "value": "***",
                    "focused": true,
                    "children": []
                }
            ]
        });
        let mut text = String::new();
        render_snapshot_tree(&tree, 0, &mut text);
        assert!(text.contains("[ax-1111111111111111] AXWindow \"Main\""));
        assert!(text.contains("value=\"***\""));
        assert!(text.contains("focused"));
    }

    #[test]
    fn screenshot_metadata_requires_digest_dimensions_and_target() {
        let bytes = b"fake png bytes";
        let sha = sha256_hex(bytes);
        let metadata = validate_screenshot_metadata(
            &json!({
                "sha256": sha,
                "width": 640,
                "height": 480,
                "target": "window",
                "window_id": 12
            }),
            bytes,
        )
        .unwrap();
        assert_eq!(metadata.width, 640);
        assert_eq!(metadata.height, 480);
        assert_eq!(metadata.window_id, Some(12));
        assert_eq!(metadata.target, "window");

        let error = validate_screenshot_metadata(
            &json!({"sha256": "wrong", "width": 1, "height": 1}),
            bytes,
        )
        .unwrap_err();
        assert!(error.to_string().contains("digest"));
        let error =
            validate_screenshot_metadata(&json!({"width": 0, "height": 1}), bytes).unwrap_err();
        assert!(error.to_string().contains("dimensions"));
        let error = validate_screenshot_metadata(
            &json!({"width": 1, "height": 1, "target": "desktop"}),
            bytes,
        );
        assert!(error.is_err());
        assert!(validate_screenshot_metadata(&json!({}), b"").is_err());
    }

    #[tokio::test]
    async fn manager_marks_truncated_app_and_window_lists() {
        let data = TempDir::new().unwrap();
        let worktree = TempDir::new().unwrap();
        let (manager, _data) = manager();
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let target = target("ses", worktree.path());
        let apps = (1..=250)
            .map(|index| json!({"name": format!("app-{index}"), "pid": index}))
            .collect::<Vec<_>>();
        let output = manager
            .render(
                &engine,
                "apps".to_string(),
                &json!({"apps": apps}),
                &target,
                &json!({}),
            )
            .await
            .unwrap();
        assert!(!output.is_error);
        assert_eq!(output.structured.as_ref().unwrap()["count"], 200);
        assert_eq!(output.structured.as_ref().unwrap()["truncated"], true);
        let windows = (1..=150)
            .map(|index| json!({"id": index, "pid": 7, "app": "Example", "title": "W"}))
            .collect::<Vec<_>>();
        let output = manager
            .render(
                &engine,
                "windows".to_string(),
                &json!({"windows": windows}),
                &target,
                &json!({}),
            )
            .await
            .unwrap();
        assert_eq!(output.structured.as_ref().unwrap()["count"], 100);
        assert_eq!(output.structured.as_ref().unwrap()["truncated"], true);
    }

    #[test]
    fn app_and_window_results_are_typed_and_bounded() {
        let apps = validate_apps_result(&json!({
            "apps": [
                {
                    "bundle_id": "com.example.App",
                    "name": "Example",
                    "pid": 7,
                    "frontmost": true,
                    "hidden": false,
                    "activation_policy": "regular"
                },
                {"name": "Bad pid", "pid": 0}
            ]
        }))
        .unwrap();
        assert_eq!(apps.apps.len(), 1);
        assert_eq!(apps.apps[0].bundle_id.as_deref(), Some("com.example.App"));
        assert!(apps.apps[0].frontmost);
        assert!(!apps.truncated);

        let many = (1..=250)
            .map(|index| json!({"name": format!("app-{index}"), "pid": index}))
            .collect::<Vec<_>>();
        let apps = validate_apps_result(&json!({"apps": many})).unwrap();
        assert_eq!(apps.apps.len(), 200);
        assert!(apps.truncated);

        let windows = validate_windows_result(&json!({
            "windows": [
                {
                    "id": 12,
                    "pid": 7,
                    "app": "Example",
                    "title": "Main",
                    "bounds": {"x": 1.0, "y": 2.0, "width": 300.0, "height": 200.0},
                    "layer": 0,
                    "focused": true,
                    "on_screen": true
                },
                {"id": 0, "pid": 7},
                {
                    "id": 13,
                    "pid": 7,
                    "app": "Example",
                    "title": "Bad frame",
                    "bounds": {"x": 1.0, "y": 2.0, "width": "wide", "height": 2.0}
                }
            ]
        }))
        .unwrap();
        assert_eq!(windows.windows.len(), 2);
        assert!(windows.windows[0].bounds.is_some());
        assert!(windows.windows[1].bounds.is_none());
        assert!(windows.windows[0].focused);
        assert!(!windows.truncated);
        assert!(validate_apps_result(&json!({})).is_err());
        assert!(validate_windows_result(&json!({})).is_err());
    }

    #[test]
    fn snapshot_validation_redacts_secure_values_and_drops_invalid_refs() {
        let result = json!({
            "app": "Example",
            "bundle_id": "com.example.App",
            "pid": 7,
            "window_title": "Main",
            "window_id": 12,
            "tree": {
                "ref": "not-a-ref",
                "role": "AXWindow",
                "title": "Main",
                "children": [
                    {
                        "ref": "ax-0123456789abcdef",
                        "role": "AXTextField",
                        "subrole": "AXSecureTextField",
                        "title": "Password",
                        "value": "hunter2",
                        "frame": {"x": 1, "y": 2, "width": 30, "height": 4},
                        "children": []
                    },
                    {
                        "ref": "ax-fedcba9876543210",
                        "role": "AXStaticText",
                        "title": "",
                        "value": "Visible",
                        "frame": {"x": 1, "y": 2, "width": "bad", "height": 4},
                        "children": []
                    }
                ]
            }
        });
        let validated = validate_snapshot_result(
            &result,
            SnapshotLimits {
                max_depth: 10,
                max_elements: 100,
                max_bytes: 131_072,
            },
        )
        .unwrap();
        let tree = validated.tree;
        assert!(tree.get("ref").is_none());
        let children = tree["children"].as_array().unwrap();
        assert_eq!(children[0]["value"], "***");
        assert_eq!(children[0]["ref"], "ax-0123456789abcdef");
        assert!(children[1].get("frame").is_none());
        assert_eq!(children[1]["value"], "Visible");
    }

    #[test]
    fn snapshot_validation_caps_depth_elements_and_bytes() {
        let mut node = json!({"role": "AXStaticText", "title": "leaf", "children": []});
        for index in 0..30 {
            node = json!({
                "role": "AXGroup",
                "title": format!("level-{index}"),
                "children": [node]
            });
        }
        let result = json!({"app": "Deep", "tree": node});
        let validated = validate_snapshot_result(
            &result,
            SnapshotLimits {
                max_depth: 3,
                max_elements: 4,
                max_bytes: 4_096,
            },
        )
        .unwrap();
        assert!(validated.truncated);
        assert!(validated.node_count <= 4);
        assert!(validated.tree.to_string().contains("\"truncated\":true"));
        let limits = SnapshotLimits::from_input(&json!({
            "max_depth": 1_000, "max_elements": 1_000_000, "max_bytes": 1_000_000
        }))
        .unwrap();
        assert_eq!(limits.max_depth, 20);
        assert_eq!(limits.max_elements, 1_200);
        assert_eq!(limits.max_bytes, 262_144);
    }

    #[test]
    fn snapshot_validation_requires_a_tree_and_keeps_a_byte_cap() {
        let error = validate_snapshot_result(
            &json!({"app": "No tree"}),
            SnapshotLimits {
                max_depth: 10,
                max_elements: 100,
                max_bytes: 10_000,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("did not include a tree"));
        let many = (0..200)
            .map(|index| {
                json!({
                    "role": "AXButton",
                    "title": format!("button-{index}-{}", "x".repeat(100)),
                    "children": []
                })
            })
            .collect::<Vec<_>>();
        let result = json!({"app": "Many", "tree": {"role": "AXWindow", "children": many}});
        let validated = validate_snapshot_result(
            &result,
            SnapshotLimits {
                max_depth: 10,
                max_elements: 1_200,
                max_bytes: 1_024,
            },
        )
        .unwrap();
        assert!(validated.truncated);
        assert!(validated.bytes <= 1_024);
        assert!(validated.node_count < 200);
    }

    #[test]
    fn action_deadlines_cover_wait_and_screenshot() {
        assert_eq!(
            action_deadline("wait", &json!({"timeout_ms": 30_000})),
            Duration::from_secs(40)
        );
        assert_eq!(
            action_deadline("screenshot", &json!({})),
            SCREENSHOT_TIMEOUT
        );
        assert_eq!(
            action_deadline("snapshot", &json!({})),
            Duration::from_secs(20)
        );
    }

    #[tokio::test]
    async fn helper_error_responses_are_typed_protocol_errors() {
        let data = TempDir::new().unwrap();
        let helper_path = data.path().join("fake-error-helper.sh");
        write_script(
            &helper_path,
            r#"#!/bin/sh
printf '{"event":"ready","version":"1","pid":%s}\n' "$$"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"action":"close"'*)
      printf '{"id":"%s","ok":true,"result":{"closed":true}}\n' "$id"
      exit 0
      ;;
    *)
      printf '{"id":"%s","ok":false,"error":{"code":"STALE_REF","message":"gone"}}\n' "$id"
      ;;
  esac
done
"#,
        );
        let scratch = data.path().join("scratch");
        let mut helper = ComputerHelper::spawn(&helper_path, &scratch).await.unwrap();
        let error = helper
            .request("apps", json!({}), Duration::from_secs(2))
            .await
            .unwrap_err();
        let protocol = error
            .downcast_ref::<ComputerProtocolError>()
            .expect("typed protocol error");
        assert_eq!(protocol.code, "STALE_REF");
        assert_eq!(protocol.message, "gone");
        helper.shutdown().await;
    }

    #[tokio::test]
    async fn status_reports_a_helper_that_never_becomes_ready() {
        let data = TempDir::new().unwrap();
        let manager = ComputerManager::new(data.path().to_path_buf());
        let helper_path = data.path().join("fake-broken-helper.sh");
        write_script(&helper_path, "#!/bin/sh\nexit 7\n");
        let status = manager.status_with_helper(Ok(helper_path)).await;
        assert!(status.helper_available);
        assert!(!status.permissions_checked);
        assert!(status.permissions.is_none());
        assert!(
            status
                .error
                .as_deref()
                .unwrap_or("")
                .contains("did not become ready")
        );
    }

    #[tokio::test]
    async fn helper_request_timeout_is_reported_without_a_partial_frame() {
        let data = TempDir::new().unwrap();
        let helper_path = data.path().join("fake-slow-helper.sh");
        write_script(
            &helper_path,
            "#!/bin/sh\nprintf '{\"event\":\"ready\",\"version\":\"1\",\"pid\":%s}\\n' \"$$\"\nsleep 5\n",
        );
        let scratch = data.path().join("scratch");
        let mut helper = ComputerHelper::spawn(&helper_path, &scratch).await.unwrap();
        let error = helper
            .request("apps", json!({}), Duration::from_millis(100))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        helper.kill();
    }

    #[tokio::test]
    async fn bounded_frames_strip_crlf_and_report_mid_frame_eof() {
        let (client, mut server) = tokio::io::duplex(1_024);
        server.write_all(b"{\"id\":\"1\"}\r\n").await.unwrap();
        let mut reader = BufReader::new(client);
        let frame = read_bounded_frame(&mut reader, 1_024).await.unwrap();
        assert_eq!(frame.as_deref(), Some("{\"id\":\"1\"}"));

        let (client, mut server) = tokio::io::duplex(1_024);
        server.write_all(b"{\"id\":").await.unwrap();
        drop(server);
        let mut reader = BufReader::new(client);
        let error = read_bounded_frame(&mut reader, 1_024).await.unwrap_err();
        assert!(error.to_string().contains("middle"));
    }

    #[test]
    fn element_ref_and_string_bounds_are_strict() {
        assert!(validate_ref("ax-0123456789abcdef").is_ok());
        assert!(validate_ref("ax-0123456789ABCDEF").is_ok());
        assert!(validate_ref("ax-0123456").is_err());
        assert!(validate_ref("ax-0123456789abcdef0").is_ok());
        assert!(validate_ref("AX-0123456789abcdef").is_err());
        assert!(validate_ref(&format!("ax-{}", "a".repeat(33))).is_err());
        assert!(validate_ref("ax-0123456789abcdeg").is_err());
        let value = json!({"text": "🙂🙂🙂"});
        assert_eq!(string_param(&value, "text", 2).as_deref(), Some("🙂🙂"));
        assert_eq!(
            required_string(&json!({"text": " "}), "text")
                .unwrap_err()
                .to_string(),
            "text is empty"
        );
        assert!(optional_u64(&json!({"x": -1}), "x").is_err());
        assert!(optional_f64(&json!({"x": "nan"}), "x").is_err());
    }

    #[tokio::test]
    async fn helper_lifecycle_speaks_json_lines_and_releases_cleanly() {
        let data = TempDir::new().unwrap();
        let helper_path = data.path().join("fake-helper.sh");
        write_script(
            &helper_path,
            r#"#!/bin/sh
SCRATCH="$2"
printf '{"event":"ready","version":"1","pid":%s,"scratch":"%s"}\n' "$$" "$SCRATCH"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"action":"permissions"'*)
      printf '{"id":"%s","ok":true,"result":{"accessibility_trusted":true,"screen_recording":false}}\n' "$id"
      ;;
    *'"action":"close"'*)
      printf '{"id":"%s","ok":true,"result":{"closed":true}}\n' "$id"
      exit 0
      ;;
    *)
      printf '{"id":"%s","ok":true,"result":{"echo":true}}\n' "$id"
      ;;
  esac
done
"#,
        );
        let scratch = data.path().join("scratch");
        let mut helper = ComputerHelper::spawn(&helper_path, &scratch).await.unwrap();
        let result = helper
            .request("permissions", json!({}), Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(result["accessibility_trusted"], true);
        assert_eq!(result["screen_recording"], false);
        helper.shutdown().await;
        assert!(!scratch.exists());
    }

    #[tokio::test]
    #[ignore = "requires the built macOS computer helper and a logged-in GUI session"]
    async fn computer_helper_reports_tcc_permissions_when_installed() {
        let data = TempDir::new().unwrap();
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let status = engine.computer_status().await;
        assert!(status.platform_supported);
        assert!(status.permissions_checked, "{status:?}");
        let permissions = status.permissions.expect("permissions payload");
        println!(
            "Accessibility={} ScreenRecording={} detail={:?}",
            permissions.accessibility, permissions.screen_recording, permissions.detail
        );
    }

    #[tokio::test]
    async fn status_reports_an_unusable_explicit_helper_path() {
        let data = TempDir::new().unwrap();
        let manager = ComputerManager::new(data.path().to_path_buf());
        let missing = data.path().join("not-a-helper");
        std::fs::write(&missing, b"#!/bin/sh\n").unwrap();
        let status = manager
            .status_with_helper(find_helper_binary_in(
                Some(missing.as_os_str()),
                Vec::new(),
                None,
            ))
            .await;
        assert!(!status.helper_available);
        assert!(!status.permissions_checked);
        assert!(
            status
                .error
                .as_deref()
                .unwrap_or("")
                .contains("does not point to an executable")
        );
    }

    #[tokio::test]
    async fn status_reports_missing_helper_and_fake_permissions() {
        let data = TempDir::new().unwrap();
        let manager = ComputerManager::new(data.path().to_path_buf());
        let missing = manager
            .status_with_helper(Err(anyhow!("helper is not installed")))
            .await;
        assert!(missing.platform_supported);
        assert!(!missing.helper_available);
        assert!(!missing.permissions_checked);
        assert!(
            missing
                .error
                .as_deref()
                .unwrap_or("")
                .contains("not installed")
        );

        let helper_path = data.path().join("fake-status-helper.sh");
        write_script(
            &helper_path,
            r#"#!/bin/sh
printf '{"event":"ready","version":"1","pid":%s}\n' "$$"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"action":"permissions"'*)
      printf '{"id":"%s","ok":true,"result":{"accessibility_trusted":true,"screen_recording":true,"detail":"ok"}}\n' "$id"
      ;;
    *'"action":"close"'*)
      printf '{"id":"%s","ok":true,"result":{"closed":true}}\n' "$id"
      exit 0
      ;;
  esac
done
"#,
        );
        let status = manager.status_with_helper(Ok(helper_path)).await;
        assert!(status.helper_available);
        assert!(status.permissions_checked);
        let permissions = status.permissions.expect("permissions");
        assert!(permissions.accessibility);
        assert!(permissions.screen_recording);
        assert_eq!(permissions.detail.as_deref(), Some("ok"));
        assert!(status.error.is_none());
    }

    #[tokio::test]
    async fn manager_renders_apps_windows_snapshot_and_permissions() {
        let data = TempDir::new().unwrap();
        let worktree = TempDir::new().unwrap();
        let (manager, _data) = manager();
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let target = target("ses", worktree.path());
        let apps = manager
            .render(
                &engine,
                "apps".to_string(),
                &json!({
                    "apps": [
                        {"name": "Example", "bundle_id": "com.example.App", "pid": 7, "frontmost": true}
                    ],
                    "count": 1
                }),
                &target,
                &json!({}),
            )
            .await
            .unwrap();
        assert!(!apps.is_error);
        assert!(apps.content.contains("* Example (com.example.App) pid 7"));

        let windows = manager
            .render(
                &engine,
                "windows".to_string(),
                &json!({
                    "windows": [
                        {
                            "id": 12,
                            "app": "Example",
                            "title": "Main",
                            "bounds": {"x": 10.0, "y": 20.0, "width": 300.0, "height": 200.0},
                            "focused": true
                        }
                    ],
                    "count": 1
                }),
                &target,
                &json!({}),
            )
            .await
            .unwrap();
        assert!(!windows.is_error);
        assert!(windows.content.contains("[12] Example — Main"));
        assert!(windows.content.contains("focused"));

        let permissions = manager
            .render(
                &engine,
                "permissions".to_string(),
                &json!({
                    "accessibility_trusted": false,
                    "screen_recording": true,
                    "detail": "enable Accessibility"
                }),
                &target,
                &json!({}),
            )
            .await
            .unwrap();
        assert!(permissions.content.contains("Accessibility: not granted"));
        assert!(permissions.content.contains("Screen Recording: granted"));

        let snapshot = manager
            .render(
                &engine,
                "snapshot".to_string(),
                &json!({
                    "app": "Example",
                    "bundle_id": "com.example.App",
                    "pid": 7,
                    "window_title": "Main",
                    "window_id": 12,
                    "node_count": 2,
                    "bytes": 200,
                    "truncated": false,
                    "tree": {
                        "ref": "ax-1111111111111111",
                        "role": "AXWindow",
                        "title": "Main",
                        "children": []
                    }
                }),
                &target,
                &json!({"max_chars": 5_000}),
            )
            .await
            .unwrap();
        assert!(snapshot.content.contains("Example — Main"));
        assert!(snapshot.content.contains("AXWindow"));
        assert_eq!(snapshot.structured.as_ref().unwrap()["node_count"], 1);
    }

    #[tokio::test]
    async fn manager_rejects_malformed_apps_and_windows_results() {
        let data = TempDir::new().unwrap();
        let worktree = TempDir::new().unwrap();
        let (manager, _data) = manager();
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let target = target("ses", worktree.path());
        let apps = manager
            .render(&engine, "apps".to_string(), &json!({}), &target, &json!({}))
            .await
            .unwrap();
        assert!(apps.is_error);
        assert_eq!(apps.structured.as_ref().unwrap()["code"], "APPS_REJECTED");
        let windows = manager
            .render(
                &engine,
                "windows".to_string(),
                &json!({"windows": "not an array"}),
                &target,
                &json!({}),
            )
            .await
            .unwrap();
        assert!(windows.is_error);
        assert_eq!(
            windows.structured.as_ref().unwrap()["code"],
            "WINDOWS_REJECTED"
        );
    }

    #[tokio::test]
    async fn screenshot_renderer_rejects_bad_digest_dimensions_and_path_escape() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("computer screenshot errors".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session(project.id, "screenshot errors".into(), None)
            .await
            .unwrap();
        let worktree = PathBuf::from(session.worktree.clone().unwrap());
        let manager = ComputerManager::new(data.path().to_path_buf());
        let target = ComputerTarget {
            session_id: &session.id,
            worktree: &worktree,
            agent_id: "agent-1",
            agent_name: "Agent One",
            task_id: None,
        };
        let scratch = manager.helper_scratch(&target);
        std::fs::create_dir_all(scratch.join("screenshots")).unwrap();
        let source = scratch.join("screenshots/bad.png");
        std::fs::write(&source, b"png").unwrap();
        let error = manager
            .render_screenshot(
                &engine,
                &json!({"path": source.to_string_lossy(), "width": 10, "height": 10, "sha256": "nope"}),
                &target,
                &json!({}),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("digest"));
        assert!(!source.exists());

        std::fs::write(&source, b"png").unwrap();
        let error = manager
            .render_screenshot(
                &engine,
                &json!({"path": source.to_string_lossy(), "width": 0, "height": 10}),
                &target,
                &json!({}),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("dimensions"));

        let outside = data.path().join("outside.png");
        std::fs::write(&outside, b"png").unwrap();
        let error = manager
            .render_screenshot(
                &engine,
                &json!({"path": outside.to_string_lossy(), "width": 10, "height": 10}),
                &target,
                &json!({}),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("escaped"));

        std::fs::write(&source, vec![0u8; MAX_SCREENSHOT_BYTES as usize + 1]).unwrap();
        let error = manager
            .render_screenshot(
                &engine,
                &json!({"path": source.to_string_lossy(), "width": 10, "height": 10}),
                &target,
                &json!({}),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("exceeds"));
    }

    #[tokio::test]
    async fn manager_renders_focus_wait_and_input_results() {
        let data = TempDir::new().unwrap();
        let worktree = TempDir::new().unwrap();
        let (manager, _data) = manager();
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let target = target("ses", worktree.path());
        let focus = manager
            .render(
                &engine,
                "focus".to_string(),
                &json!({"bundle_id": "com.example.App", "window_title": "Main"}),
                &target,
                &json!({}),
            )
            .await
            .unwrap();
        assert!(focus.content.contains("focused com.example.App"));
        let wait = manager
            .render(
                &engine,
                "wait".to_string(),
                &json!({"matched": true, "elapsed_ms": 42}),
                &target,
                &json!({}),
            )
            .await
            .unwrap();
        assert!(wait.content.contains("42 ms"));
        for action in ["click", "type", "key", "scroll", "drag"] {
            let output = manager
                .render(
                    &engine,
                    action.to_string(),
                    &json!({"ok": true}),
                    &target,
                    &json!({}),
                )
                .await
                .unwrap();
            assert!(!output.is_error);
            assert!(output.content.contains(action));
            assert_eq!(output.structured.as_ref().unwrap()["action"], action);
        }
    }

    #[tokio::test]
    async fn screenshot_renderer_publishes_a_bounded_binary_artifact() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("computer screenshot".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session(project.id, "screenshot".into(), None)
            .await
            .unwrap();
        let worktree = PathBuf::from(session.worktree.clone().unwrap());
        let manager = ComputerManager::new(data.path().to_path_buf());
        let target = ComputerTarget {
            session_id: &session.id,
            worktree: &worktree,
            agent_id: "agent-1",
            agent_name: "Agent One",
            task_id: None,
        };
        let scratch = manager.helper_scratch(&target);
        std::fs::create_dir_all(scratch.join("screenshots")).unwrap();
        let source = scratch.join("screenshots/test.png");
        let bytes = b"\x89PNG\r\n\x1a\ntest";
        std::fs::write(&source, bytes).unwrap();
        let sha = sha256_hex(bytes);
        let output = manager
            .render_screenshot(
                &engine,
                &json!({
                    "path": source.to_string_lossy(),
                    "width": 640,
                    "height": 480,
                    "sha256": sha,
                    "target": "window"
                }),
                &target,
                &json!({"name": "evidence.png"}),
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        let structured = output.structured.unwrap();
        assert_eq!(structured["width"], 640);
        assert_eq!(structured["height"], 480);
        assert_eq!(structured["bytes"], bytes.len());
        assert!(structured["artifact_id"].as_str().is_some());
        assert!(
            artifact_path(&engine, &session.id, "evidence.png")
                .await
                .exists()
        );
        assert!(!source.exists());
    }

    async fn artifact_path(engine: &Engine, session_id: &str, name: &str) -> PathBuf {
        let artifacts = engine.list_artifacts(session_id).await.unwrap();
        let artifact = artifacts
            .into_iter()
            .find(|artifact| artifact.name == name)
            .expect("artifact");
        PathBuf::from(artifact.path.expect("artifact path"))
    }

    #[tokio::test]
    async fn manager_close_without_a_helper_is_a_noop() {
        let (manager, _data) = manager();
        let output = manager.close("missing-session\nmissing-worktree").await;
        assert!(!output.is_error);
        assert_eq!(output.structured.as_ref().unwrap()["closed"], json!(false));
    }

    #[test]
    fn wait_conditions_require_their_specific_fields() {
        let data = TempDir::new().unwrap();
        let (manager, _data) = manager();
        let target = target("ses", data.path());
        let element = manager
            .params_for(
                "wait",
                &target,
                &json!({"condition": "element", "identifier": "save", "interval_ms": 1}),
            )
            .unwrap();
        assert_eq!(element["identifier"], "save");
        assert_eq!(element["interval_ms"], 50);
        let window = manager
            .params_for(
                "wait",
                &target,
                &json!({"condition": "window", "title": "Save"}),
            )
            .unwrap();
        assert_eq!(window["title"], "Save");
        let frontmost = manager
            .params_for(
                "wait",
                &target,
                &json!({"condition": "app_frontmost", "pid": 7}),
            )
            .unwrap();
        assert_eq!(frontmost["pid"], 7);
        assert!(
            manager
                .params_for("wait", &target, &json!({"condition": "window"}))
                .is_err()
        );
        assert!(
            manager
                .params_for("wait", &target, &json!({"condition": "unknown"}))
                .is_err()
        );
        assert!(
            manager
                .params_for("wait", &target, &json!({"condition": "ref"}))
                .is_err()
        );
    }

    #[tokio::test]
    async fn permissions_renderer_caps_helper_detail() {
        let data = TempDir::new().unwrap();
        let worktree = TempDir::new().unwrap();
        let (manager, _data) = manager();
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let target = target("ses", worktree.path());
        let output = manager
            .render(
                &engine,
                "permissions".to_string(),
                &json!({
                    "accessibility_trusted": true,
                    "screen_recording": false,
                    "detail": "x".repeat(10_000)
                }),
                &target,
                &json!({}),
            )
            .await
            .unwrap();
        assert!(!output.is_error);
        assert!(output.content.len() < 3_000);
        assert_eq!(
            output.structured.as_ref().unwrap()["detail"]
                .as_str()
                .unwrap()
                .len(),
            2_000
        );
    }

    #[test]
    fn target_parameters_reject_zero_ids_and_missing_apps() {
        let data = TempDir::new().unwrap();
        let (manager, _data) = manager();
        let target = target("ses", data.path());
        assert_eq!(
            manager
                .params_for("permissions", &target, &json!({}))
                .unwrap(),
            json!({})
        );
        let apps = manager
            .params_for("apps", &target, &json!({"include_background": true}))
            .unwrap();
        assert_eq!(apps["include_background"], true);
        let windows = manager
            .params_for("windows", &target, &json!({"bundle_id": "com.example.App"}))
            .unwrap();
        assert_eq!(windows["bundle_id"], "com.example.App");
        assert!(
            manager
                .params_for("windows", &target, &json!({"pid": 0}))
                .is_err()
        );
        assert!(
            manager
                .params_for("windows", &target, &json!({"pid": 99_999_999_999u64}))
                .is_err()
        );
        assert!(manager.params_for("focus", &target, &json!({})).is_err());
        assert!(
            manager
                .params_for("snapshot", &target, &json!({"window_id": 0}))
                .is_err()
        );
        assert!(
            manager
                .params_for("screenshot", &target, &json!({"window_id": 0}))
                .is_err()
        );
        let focused = manager
            .params_for(
                "focus",
                &target,
                &json!({"bundle_id": "com.example.App", "window_title": "Main", "window_id": 9}),
            )
            .unwrap();
        assert_eq!(focused["window_title"], "Main");
        assert_eq!(focused["window_id"], 9);
    }

    #[test]
    fn modifier_aliases_and_window_targets_normalize() {
        assert_eq!(normalize_modifier("cmd").unwrap(), "command");
        assert_eq!(normalize_modifier("meta").unwrap(), "command");
        assert_eq!(normalize_modifier("alt").unwrap(), "option");
        assert_eq!(normalize_modifier("ctrl").unwrap(), "control");
        assert!(normalize_modifier("hyper").is_err());
        let data = TempDir::new().unwrap();
        let (manager, _data) = manager();
        let target = target("ses", data.path());
        let params = manager
            .params_for(
                "click",
                &target,
                &json!({"x": 0.5, "y": 0.5, "bundle_id": "com.example.App", "pid": 7, "window_id": 8}),
            )
            .unwrap();
        assert_eq!(params["bundle_id"], "com.example.App");
        assert_eq!(params["pid"], 7);
        assert_eq!(params["window_id"], 8);
        assert!(
            manager
                .params_for(
                    "click",
                    &target,
                    &json!({"x": 0.5, "y": 0.5, "window_id": 0})
                )
                .is_err()
        );
    }

    #[test]
    fn helper_scratch_keys_are_deterministic_and_session_scoped() {
        let data = TempDir::new().unwrap();
        let manager = ComputerManager::new(data.path().to_path_buf());
        let first = target("session-a", Path::new("/tmp/worktree"));
        let second = target("session-b", Path::new("/tmp/worktree"));
        let first_scratch = manager.helper_scratch(&first);
        let again = manager.helper_scratch(&first);
        let second_scratch = manager.helper_scratch(&second);
        assert_eq!(first_scratch, again);
        assert_ne!(first_scratch, second_scratch);
        assert!(first_scratch.starts_with(data.path().join("sandbox/session-a")));
    }

    #[tokio::test]
    async fn manager_actions_run_through_the_fake_helper_and_release_the_lease() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let helper_path = data.path().join("fake-action-helper.sh");
        write_script(
            &helper_path,
            r#"#!/bin/sh
printf '{"event":"ready","version":"1","pid":%s}\n' "$$"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"action":"permissions"'*)
      printf '{"id":"%s","ok":true,"result":{"accessibility_trusted":true,"screen_recording":true}}\n' "$id"
      ;;
    *'"action":"apps"'*)
      printf '{"id":"%s","ok":true,"result":{"apps":[{"bundle_id":"com.example.Fake","name":"Fake","pid":1,"frontmost":true,"hidden":false,"activation_policy":"regular"}],"count":1}}\n' "$id"
      ;;
    *'"action":"windows"'*)
      printf '{"id":"%s","ok":true,"result":{"windows":[{"id":9,"pid":1,"app":"Fake","title":"Main","bounds":{"x":10.0,"y":20.0,"width":300.0,"height":200.0},"layer":0,"focused":true,"on_screen":true}],"count":1}}\n' "$id"
      ;;
    *'"action":"snapshot"'*)
      printf '{"id":"%s","ok":true,"result":{"app":"Fake","bundle_id":"com.example.Fake","pid":1,"window_title":"Main","window_id":9,"tree":{"ref":"ax-0123456789abcdef","role":"AXWindow","title":"Main","children":[{"ref":"ax-fedcba9876543210","role":"AXButton","title":"Go","children":[]}]},"truncated":false}}\n' "$id"
      ;;
    *'"action":"focus"'*Stale*)
      printf '{"id":"%s","ok":false,"error":{"code":"STALE_REF","message":"window is gone"}}\n' "$id"
      ;;
    *'"action":"click"'*|*'"action":"type"'*|*'"action":"key"'*|*'"action":"scroll"'*|*'"action":"drag"'*)
      printf '{"id":"%s","ok":true,"result":{"ok":true}}\n' "$id"
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
                Some("computer actions".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session(project.id, "actions".into(), None)
            .await
            .unwrap();
        let worktree = PathBuf::from(session.worktree.clone().unwrap());
        let manager = ComputerManager::with_helper(data.path().to_path_buf(), helper_path.clone());
        let target = ComputerTarget {
            session_id: &session.id,
            worktree: &worktree,
            agent_id: "agent-1",
            agent_name: "Agent One",
            task_id: Some("task-1"),
        };

        let invalid = manager
            .action(&engine, &target, json!({"action": "shell"}))
            .await
            .unwrap();
        assert!(invalid.is_error);
        assert_eq!(
            invalid.structured.as_ref().unwrap()["code"],
            "INVALID_ARGUMENT"
        );

        let apps = manager
            .action(&engine, &target, json!({"action": "apps"}))
            .await
            .unwrap();
        assert!(!apps.is_error, "{}", apps.content);
        assert!(apps.content.contains("Fake (com.example.Fake)"));
        assert_eq!(engine.list_computer_leases().await.unwrap().len(), 1);

        let windows = manager
            .action(&engine, &target, json!({"action": "windows"}))
            .await
            .unwrap();
        assert!(!windows.is_error, "{}", windows.content);
        assert!(windows.content.contains("[9] Fake — Main"));

        let snapshot = manager
            .action(&engine, &target, json!({"action": "snapshot"}))
            .await
            .unwrap();
        assert!(!snapshot.is_error, "{}", snapshot.content);
        assert!(snapshot.content.contains("AXButton \"Go\""));
        assert_eq!(snapshot.structured.as_ref().unwrap()["node_count"], 2);

        let click = manager
            .action(
                &engine,
                &target,
                json!({"action": "click", "ref": "ax-0123456789abcdef"}),
            )
            .await
            .unwrap();
        assert!(!click.is_error, "{}", click.content);
        let key = manager
            .action(
                &engine,
                &target,
                json!({"action": "key", "key": "q", "modifiers": ["cmd"]}),
            )
            .await
            .unwrap();
        assert!(!key.is_error, "{}", key.content);

        let stale = manager
            .action(
                &engine,
                &target,
                json!({"action": "focus", "bundle_id": "com.example.Stale"}),
            )
            .await
            .unwrap();
        assert!(stale.is_error);
        assert_eq!(stale.structured.as_ref().unwrap()["code"], "STALE_REF");
        // A structured helper error keeps the process and the lease alive.
        assert_eq!(engine.list_computer_leases().await.unwrap().len(), 1);

        let closed = manager
            .action(&engine, &target, json!({"action": "close"}))
            .await
            .unwrap();
        assert!(!closed.is_error);
        assert_eq!(closed.structured.as_ref().unwrap()["closed"], json!(true));
        // close releases the guard explicitly rather than waiting for TTL.
        assert!(engine.list_computer_leases().await.unwrap().is_empty());
        assert!(!manager.helper_scratch(&target).exists());
    }

    #[tokio::test]
    async fn computer_surface_lease_is_fail_fast() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("computer lease".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session(project.id, "lease".into(), None)
            .await
            .unwrap();
        let first = engine
            .acquire_computer_lease(
                "computer",
                &session.id,
                "agent-a",
                "Agent A",
                Duration::from_secs(60),
            )
            .await
            .unwrap();
        let error = match engine
            .acquire_computer_lease(
                "computer",
                &session.id,
                "agent-b",
                "Agent B",
                Duration::from_secs(60),
            )
            .await
        {
            Ok(_) => panic!("the second computer lease must fail fast"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("busy"));
        first.release().await;
        assert!(engine.list_computer_leases().await.unwrap().is_empty());
    }
}

#[cfg(test)]
mod gui_session_tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;
    use vp_store::Store;

    fn init_repo(path: &Path) {
        let status = Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::write(path.join("README.md"), "gui session\n").unwrap();
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

    fn manager() -> (ComputerManager, TempDir) {
        let data = TempDir::new().unwrap();
        (ComputerManager::new(data.path().to_path_buf()), data)
    }

    fn target<'a>(session: &'a str, worktree: &'a Path) -> ComputerTarget<'a> {
        ComputerTarget {
            session_id: session,
            worktree,
            agent_id: "agent-1",
            agent_name: "Agent One",
            task_id: None,
        }
    }

    fn spawn_count(path: &Path) -> usize {
        std::fs::read_to_string(format!("{}.spawns", path.display()))
            .map(|text| text.lines().count())
            .unwrap_or(0)
    }

    fn request_log(path: &Path) -> Vec<String> {
        std::fs::read_to_string(format!("{}.requests", path.display()))
            .map(|text| text.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn status_cache_avoids_spawning_a_helper_per_refresh() {
        let data = TempDir::new().unwrap();
        let helper_path = data.path().join("fake-status-cache-helper.sh");
        write_script(
            &helper_path,
            r#"#!/bin/sh
echo spawn >> "$0.spawns"
printf '{"event":"ready","version":"1","pid":%s}\n' "$$"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"action":"permissions"'*)
      printf '{"id":"%s","ok":true,"result":{"accessibility_trusted":true,"screen_recording":true,"detail":"ok"}}\n' "$id"
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
        let manager = ComputerManager::with_helper(data.path().to_path_buf(), helper_path.clone());
        let policy = vp_protocol::types::GuiPolicy {
            status_ttl_secs: 60,
            ..Default::default()
        };
        let first = manager.status(&policy, false).await;
        assert!(first.permissions_checked);
        assert!(!first.cached);
        assert_eq!(first.grace_secs, policy.grace_secs);
        assert_eq!(first.wait_ms, policy.wait_ms);
        assert_eq!(spawn_count(&helper_path), 1);

        let cached = manager.status(&policy, false).await;
        assert!(cached.cached);
        assert!(cached.cache_age_ms.is_some());
        assert!(cached.permissions_checked);
        assert_eq!(spawn_count(&helper_path), 1);

        let refreshed = manager.status(&policy, true).await;
        assert!(!refreshed.cached);
        assert_eq!(spawn_count(&helper_path), 2);
    }

    #[tokio::test]
    async fn warm_helper_survives_lease_grace_release_and_reacquires() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let helper_path = data.path().join("fake-warm-helper.sh");
        write_script(
            &helper_path,
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
                Some("warm helper".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session(project.id, "warm".into(), None)
            .await
            .unwrap();
        let worktree = PathBuf::from(session.worktree.clone().unwrap());
        let manager = ComputerManager::with_helper(data.path().to_path_buf(), helper_path.clone());
        let target = ComputerTarget {
            session_id: &session.id,
            worktree: &worktree,
            agent_id: "agent-1",
            agent_name: "Agent One",
            task_id: None,
        };
        let first = manager
            .action(&engine, &target, json!({"action": "apps"}))
            .await
            .unwrap();
        assert!(!first.is_error, "{}", first.content);
        assert_eq!(spawn_count(&helper_path), 1);
        assert_eq!(manager.warm_helpers(), 1);
        assert_eq!(engine.list_computer_leases().await.unwrap().len(), 1);
        assert!(engine.gui_end_session(&session.id, "test_release").await);
        assert!(engine.list_computer_leases().await.unwrap().is_empty());
        assert_eq!(manager.warm_helpers(), 1);
        assert_eq!(spawn_count(&helper_path), 1);

        let second = manager
            .action(&engine, &target, json!({"action": "apps"}))
            .await
            .unwrap();
        assert!(!second.is_error, "{}", second.content);
        assert_eq!(spawn_count(&helper_path), 1);
        assert_eq!(engine.list_computer_leases().await.unwrap().len(), 1);
        let closed = manager
            .action(&engine, &target, json!({"action": "close"}))
            .await
            .unwrap();
        assert!(!closed.is_error);
        assert!(engine.list_computer_leases().await.unwrap().is_empty());
        assert_eq!(manager.warm_helpers(), 0);
    }

    #[tokio::test]
    async fn crashed_helper_is_removed_and_respawned() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let helper_path = data.path().join("fake-crash-helper.sh");
        write_script(
            &helper_path,
            r#"#!/bin/sh
echo spawn >> "$0.spawns"
printf '{"event":"ready","version":"1","pid":%s}\n' "$$"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"action":"apps"'*)
      printf '{"id":"%s","ok":true,"result":{"apps":[],"count":0}}\n' "$id"
      exit 0
      ;;
    *'"action":"close"'*)
      printf '{"id":"%s","ok":true,"result":{"closed":true}}\n' "$id"
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
                Some("crash helper".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session(project.id, "crash".into(), None)
            .await
            .unwrap();
        let worktree = PathBuf::from(session.worktree.clone().unwrap());
        let manager = ComputerManager::with_helper(data.path().to_path_buf(), helper_path.clone());
        let target = ComputerTarget {
            session_id: &session.id,
            worktree: &worktree,
            agent_id: "agent-1",
            agent_name: "Agent One",
            task_id: None,
        };
        let first = manager
            .action(&engine, &target, json!({"action": "apps"}))
            .await
            .unwrap();
        assert!(!first.is_error, "{}", first.content);
        tokio::time::sleep(Duration::from_millis(80)).await;
        let second = manager
            .action(&engine, &target, json!({"action": "apps"}))
            .await
            .unwrap();
        assert!(!second.is_error, "{}", second.content);
        assert_eq!(spawn_count(&helper_path), 2);
        assert_eq!(manager.warm_helpers(), 1);
    }

    #[tokio::test]
    async fn stale_ref_error_is_structured_and_never_auto_retried() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let helper_path = data.path().join("fake-stale-helper.sh");
        write_script(
            &helper_path,
            r#"#!/bin/sh
printf '{"event":"ready","version":"1","pid":%s}\n' "$$"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"action":"click"'*)
      echo click >> "$0.clicks"
      printf '{"id":"%s","ok":false,"error":{"code":"STALE_REF","message":"ref is from generation 3","details":{"snapshot_generation":7,"hint":"take a fresh snapshot","auto_retry":false}}}\n' "$id"
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
                Some("stale".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session(project.id, "stale".into(), None)
            .await
            .unwrap();
        let worktree = PathBuf::from(session.worktree.clone().unwrap());
        let manager = ComputerManager::with_helper(data.path().to_path_buf(), helper_path.clone());
        let target = ComputerTarget {
            session_id: &session.id,
            worktree: &worktree,
            agent_id: "agent-1",
            agent_name: "Agent One",
            task_id: None,
        };
        let output = manager
            .action(
                &engine,
                &target,
                json!({"action": "click", "ref": "ax-00000003-0123456789abcdef"}),
            )
            .await
            .unwrap();
        assert!(output.is_error);
        let structured = output.structured.as_ref().unwrap();
        assert_eq!(structured["code"], "STALE_REF");
        assert_eq!(structured["snapshot_generation"], 7);
        assert_eq!(structured["auto_retry"], false);
        assert_eq!(structured["fresh_snapshot_required"], true);
        assert!(
            structured["hint"]
                .as_str()
                .unwrap()
                .contains("fresh snapshot")
        );
        assert_eq!(
            std::fs::read_to_string(format!("{}.clicks", helper_path.display()))
                .unwrap()
                .lines()
                .count(),
            1,
            "the daemon must not retry a stale-ref input action"
        );
        let timeline = engine.gui_timeline(Some(&session.id), 10).await.unwrap();
        assert_eq!(timeline.len(), 1);
        assert_eq!(timeline[0].action, "click");
        assert_eq!(timeline[0].error_code.as_deref(), Some("STALE_REF"));
    }

    #[tokio::test]
    async fn sensitive_policy_and_target_metadata_reach_the_helper() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let helper_path = data.path().join("fake-sensitive-helper.sh");
        write_script(
            &helper_path,
            r#"#!/bin/sh
printf '{"event":"ready","version":"1","pid":%s}\n' "$$"
while IFS= read -r line; do
  echo "$line" >> "$0.requests"
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"action":"snapshot"'*)
      printf '{"id":"%s","ok":true,"result":{"app":"System Settings","bundle_id":"com.apple.systempreferences","pid":1,"window_title":"Privacy & Security","tree":{"ref":"ax-00000001-1111111111111111","role":"AXWindow","title":"Privacy & Security","children":[]},"snapshot_generation":1,"generation":1,"truncated":false,"target":{"app":"System Settings","bundle_id":"com.apple.systempreferences","window_title":"Privacy & Security","sensitive":true}}}\n' "$id"
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
                Some("sensitive".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session(project.id.clone(), "sensitive".into(), None)
            .await
            .unwrap();
        let worktree = PathBuf::from(session.worktree.clone().unwrap());
        let manager = ComputerManager::with_helper(data.path().to_path_buf(), helper_path.clone());
        let target = ComputerTarget {
            session_id: &session.id,
            worktree: &worktree,
            agent_id: "agent-1",
            agent_name: "Agent One",
            task_id: None,
        };
        let output = manager
            .action(
                &engine,
                &target,
                json!({
                    "action": "snapshot",
                    "bundle_id": "com.apple.systempreferences",
                    "window_title": "Privacy & Security"
                }),
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        let structured = output.structured.as_ref().unwrap();
        assert_eq!(structured["snapshot_generation"], 1);
        assert_eq!(structured["stale_ref_policy"], "reject_without_retry");
        let requests = request_log(&helper_path);
        let snapshot_request = requests
            .iter()
            .find(|line| line.contains("\"action\":\"snapshot\""))
            .expect("snapshot request");
        assert!(snapshot_request.contains("\"sensitive_mode\":\"deny\""));
        assert!(snapshot_request.contains("\"sensitive_ack\":true"));
        assert!(snapshot_request.contains("com.apple.securityagent"));
        assert!(snapshot_request.contains("system settings"));
        let timeline = engine.gui_timeline(Some(&session.id), 10).await.unwrap();
        assert_eq!(timeline.len(), 1);
        assert!(timeline[0].sensitive_target);
        assert!(timeline[0].target_window.is_none());
        assert_eq!(timeline[0].snapshot_generation, 1);
        assert_eq!(
            timeline[0].target_bundle_id.as_deref(),
            Some("com.apple.systempreferences")
        );
        let _ = project;
    }

    #[tokio::test]
    async fn status_cache_also_caches_a_helper_start_failure() {
        let data = TempDir::new().unwrap();
        let helper_path = data.path().join("fake-broken-status-helper.sh");
        write_script(
            &helper_path,
            "#!/bin/sh\necho spawn >> \"$0.spawns\"\nexit 7\n",
        );
        let manager = ComputerManager::with_helper(data.path().to_path_buf(), helper_path.clone());
        let policy = vp_protocol::types::GuiPolicy {
            status_ttl_secs: 60,
            ..Default::default()
        };
        let first = manager.status(&policy, false).await;
        assert!(first.helper_available);
        assert!(first.permissions.is_none());
        assert!(!first.cached);
        assert!(
            first
                .error
                .as_deref()
                .unwrap_or("")
                .contains("did not become ready")
        );
        let second = manager.status(&policy, false).await;
        assert!(second.cached);
        assert!(
            second
                .error
                .as_deref()
                .unwrap_or("")
                .contains("did not become ready")
        );
        assert_eq!(spawn_count(&helper_path), 1);
    }

    #[tokio::test]
    async fn concurrent_status_refreshes_share_one_helper_spawn() {
        let data = TempDir::new().unwrap();
        let helper_path = data.path().join("fake-concurrent-status-helper.sh");
        write_script(
            &helper_path,
            r#"#!/bin/sh
echo spawn >> "$0.spawns"
printf '{"event":"ready","version":"1","pid":%s}\n' "$$"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"action":"permissions"'*)
      printf '{"id":"%s","ok":true,"result":{"accessibility_trusted":true,"screen_recording":false}}\n' "$id"
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
        let manager = Arc::new(ComputerManager::with_helper(
            data.path().to_path_buf(),
            helper_path.clone(),
        ));
        let policy = vp_protocol::types::GuiPolicy {
            status_ttl_secs: 60,
            ..Default::default()
        };
        let mut tasks = Vec::new();
        for _ in 0..6 {
            let manager = manager.clone();
            let policy = policy.clone();
            tasks.push(tokio::spawn(
                async move { manager.status(&policy, false).await },
            ));
        }
        let mut results = Vec::new();
        for task in tasks {
            results.push(task.await.unwrap());
        }
        assert_eq!(spawn_count(&helper_path), 1);
        assert!(results.iter().all(|status| status.permissions_checked));
        assert!(results.iter().any(|status| !status.cached));
        assert_eq!(results.iter().filter(|status| status.cached).count(), 5);
    }

    #[test]
    fn generation_bearing_refs_parse_but_invalid_forms_do_not() {
        assert!(validate_ref("ax-00000007-0123456789abcdef").is_ok());
        assert_eq!(ref_generation("ax-00000007-0123456789abcdef"), Some(7));
        assert_eq!(ref_generation("ax-0123456789abcdef"), None);
        assert!(validate_ref("ax-0000000-0123456789abcdef").is_err());
        assert!(validate_ref("ax-00000007-0123456789abcdeg").is_err());
        assert!(validate_ref("ax-00000007-0123456789abcd").is_err());
        assert!(validate_ref("ax-00000007-0123456789abcdef00").is_ok());
        assert!(ref_generation("not-a-ref").is_none());
    }

    #[tokio::test]
    async fn close_without_a_warm_helper_still_releases_the_gui_lease() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("close lease".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session(project.id, "close".into(), None)
            .await
            .unwrap();
        let worktree = PathBuf::from(session.worktree.clone().unwrap());
        let manager = ComputerManager::new(data.path().to_path_buf());
        let policy = vp_protocol::types::GuiPolicy::default();
        let ticket = engine
            .gui_begin_action_with_policy(
                &policy,
                GuiSurface::Computer,
                &session.id,
                "agent",
                "Agent",
            )
            .await
            .unwrap();
        assert!(matches!(ticket, GuiAcquire::Acquired(_)));
        assert_eq!(engine.list_computer_leases().await.unwrap().len(), 1);
        let output = manager
            .action(
                &engine,
                &ComputerTarget {
                    session_id: &session.id,
                    worktree: &worktree,
                    agent_id: "agent",
                    agent_name: "Agent",
                    task_id: None,
                },
                json!({"action": "close"}),
            )
            .await
            .unwrap();
        assert!(!output.is_error);
        assert_eq!(output.structured.as_ref().unwrap()["closed"], false);
        assert!(engine.list_computer_leases().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn gui_busy_includes_holder_and_retry_hint() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("busy".into()),
            )
            .await
            .unwrap();
        let first = engine
            .create_session(project.id.clone(), "holder".into(), None)
            .await
            .unwrap();
        let second = engine
            .create_session(project.id, "waiter".into(), None)
            .await
            .unwrap();
        let policy = vp_protocol::types::GuiPolicy {
            grace_secs: 30,
            wait_ms: 100,
            ..Default::default()
        };
        let held = engine
            .gui_begin_action_with_policy(&policy, GuiSurface::Computer, &first.id, "a", "Agent A")
            .await
            .unwrap();
        assert!(matches!(held, GuiAcquire::Acquired(_)));
        let output = engine
            .computers
            .action(
                &engine,
                &ComputerTarget {
                    session_id: &second.id,
                    worktree: Path::new("/tmp/missing-worktree"),
                    agent_id: "b",
                    agent_name: "Agent B",
                    task_id: None,
                },
                json!({"action": "apps"}),
            )
            .await
            .unwrap();
        assert!(output.is_error);
        let structured = output.structured.as_ref().unwrap();
        assert_eq!(structured["code"], "GUI_BUSY");
        assert_eq!(structured["holder"]["session_id"], first.id);
        assert!(structured["retry_after_ms"].as_u64().unwrap() > 0);
        assert_eq!(structured["lease_surface"], "gui");
    }

    #[test]
    fn displays_action_and_screenshot_display_selectors_are_bounded() {
        assert_eq!(
            normalize_action(&json!({"action": "displays"})).unwrap(),
            "displays"
        );
        let (manager, _data) = manager();
        let target = target("ses", Path::new("/tmp/worktree"));
        let params = manager
            .params_for(
                "screenshot",
                &target,
                &json!({
                    "target": "screen",
                    "display_index": 2,
                    "display_id": 7,
                    "max_width": 800,
                    "max_height": 600
                }),
            )
            .unwrap();
        assert_eq!(params["display_index"], 2);
        assert_eq!(params["display_id"], 7);
        assert_eq!(params["target"], "screen");
        assert!(
            manager
                .params_for("screenshot", &target, &json!({"display_index": 16}))
                .is_err()
        );
        assert!(
            manager
                .params_for("screenshot", &target, &json!({"display_id": 0}))
                .is_err()
        );
        assert_eq!(
            manager.params_for("displays", &target, &json!({})).unwrap(),
            json!({})
        );
    }

    #[tokio::test]
    async fn manager_renders_displays_with_a_bound() {
        let data = TempDir::new().unwrap();
        let worktree = TempDir::new().unwrap();
        let (manager, _data) = manager();
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let target = target("ses", worktree.path());
        let displays = (0..20)
            .map(|index| {
                json!({
                    "index": index,
                    "id": index + 1,
                    "width": 1920,
                    "height": 1080,
                    "name": format!("Display {index}"),
                    "main": index == 0
                })
            })
            .collect::<Vec<_>>();
        let output = manager
            .render(
                &engine,
                "displays".to_string(),
                &json!({"displays": displays}),
                &target,
                &json!({}),
            )
            .await
            .unwrap();
        assert!(!output.is_error);
        assert!(
            output
                .content
                .contains("[0] id 1 1920x1080 *main Display 0")
        );
        let structured = output.structured.as_ref().unwrap();
        assert_eq!(structured["count"], 16);
        assert_eq!(structured["truncated"], true);
        assert_eq!(structured["displays"].as_array().unwrap().len(), 16);
    }

    #[tokio::test]
    async fn wait_results_report_the_observation_path() {
        let data = TempDir::new().unwrap();
        let worktree = TempDir::new().unwrap();
        let (manager, _data) = manager();
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let target = target("ses", worktree.path());
        let output = manager
            .render(
                &engine,
                "wait".to_string(),
                &json!({"matched": true, "elapsed_ms": 42, "observed_by": "ax_observer"}),
                &target,
                &json!({}),
            )
            .await
            .unwrap();
        assert!(!output.is_error);
        assert_eq!(
            output.structured.as_ref().unwrap()["result"]["observed_by"],
            "ax_observer"
        );

        let timeout = manager
            .render(
                &engine,
                "wait".to_string(),
                &json!({"matched": false}),
                &target,
                &json!({}),
            )
            .await;
        // The renderer treats a helper success frame as a match; the helper
        // returns TIMEOUT instead. This asserts the render path stays bounded.
        assert!(timeout.is_ok());
    }
}

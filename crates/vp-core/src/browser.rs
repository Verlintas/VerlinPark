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

use crate::ComputerLeaseGuard;
use crate::Engine;
use crate::tooling::fs::{atomic_write, sha256_hex};
use crate::tools::{ToolOutput, truncate};

const HELPER_BINARY: &str = "VerlinParkBrowserHelper";
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
const DEFAULT_HELPER_IDLE_SECS: u64 = 600;
const LEASE_TTL_SECS: u64 = 120;
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
    lease: Option<ComputerLeaseGuard>,
    next_id: u64,
    last_used: Arc<AtomicU64>,
}

impl BrowserHelper {
    async fn spawn(
        helper_path: &Path,
        scratch: &Path,
        worktree: &Path,
        lease: ComputerLeaseGuard,
    ) -> Result<Self> {
        let mut lease = Some(lease);
        let result = Self::spawn_inner(helper_path, scratch, worktree, &mut lease).await;
        if result.is_err()
            && let Some(lease) = lease.take()
        {
            lease.release().await;
        }
        result
    }

    async fn spawn_inner(
        helper_path: &Path,
        scratch: &Path,
        worktree: &Path,
        lease: &mut Option<ComputerLeaseGuard>,
    ) -> Result<Self> {
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
            lease: lease.take(),
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
            if let Some(lease) = helper.lease.take() {
                lease.release().await;
            }
            bail!("browser helper did not become ready ({stderr})");
        }
        Ok(helper)
    }

    fn touch(&self) {
        self.last_used.store(unix_now(), Ordering::SeqCst);
    }

    fn lease_id(&self) -> Option<String> {
        self.lease
            .as_ref()
            .map(|lease| lease.lease_id().to_string())
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
        if let Some(lease) = self.lease.take() {
            lease.release().await;
        }
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

pub struct BrowserManager {
    helpers: Mutex<HashMap<String, Arc<AsyncMutex<BrowserHelper>>>>,
    spawn_lock: AsyncMutex<()>,
    data_dir: PathBuf,
}

impl BrowserManager {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            helpers: Mutex::new(HashMap::new()),
            spawn_lock: AsyncMutex::new(()),
            data_dir,
        }
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
            let helper = self
                .helpers
                .lock()
                .ok()
                .and_then(|mut map| map.remove(&key));
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
                return Ok(ToolOutput::ok_structured(
                    "browser surface closed",
                    json!({"action": "close", "closed": true}),
                ));
            }
            return Ok(ToolOutput::ok_structured(
                "no browser surface was open",
                json!({"action": "close", "closed": false}),
            ));
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
        let helper = match self.ensure_helper(engine, target, &key).await {
            Ok(helper) => helper,
            Err(error) => {
                let message = error.to_string();
                let code = if message.contains("busy") {
                    "COMPUTER_BUSY"
                } else if message.contains("is not installed") {
                    "BROWSER_HELPER_UNAVAILABLE"
                } else if message.contains("sandbox is unavailable") {
                    "BROWSER_SANDBOX_UNAVAILABLE"
                } else {
                    "BROWSER_HELPER_FAILED"
                };
                return Ok(ToolOutput::error_structured(
                    format!("browser {action} unavailable: {message}"),
                    json!({
                        "code": code,
                        "action": action,
                        "message": message,
                    }),
                ));
            }
        };
        let deadline = action_deadline(&action, &input);
        let result = {
            let mut helper = helper.lock().await;
            helper.request(&action, params, deadline).await
        };
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
                    // action starts from a clean process and lease. Script or
                    // element errors keep the page alive for a retry.
                    let removed = self
                        .helpers
                        .lock()
                        .ok()
                        .and_then(|mut map| map.remove(&key));
                    if let Some(helper) = removed {
                        let mut helper = helper.lock().await;
                        helper.shutdown().await;
                    }
                }
                return Ok(ToolOutput::error_structured(
                    format!("browser {action} failed: {error}"),
                    json!({
                        "code": code,
                        "action": action,
                        "message": error.to_string(),
                    }),
                ));
            }
        };
        self.render(action, &result, target, &input).await
    }

    async fn ensure_helper(
        &self,
        engine: &Engine,
        target: &BrowserTarget<'_>,
        key: &str,
    ) -> Result<Arc<AsyncMutex<BrowserHelper>>> {
        if let Some(helper) = self.existing_helper(engine, key).await? {
            return Ok(helper);
        }
        let _spawn = self.spawn_lock.lock().await;
        if let Some(helper) = self.existing_helper(engine, key).await? {
            return Ok(helper);
        }
        let helper_path = find_helper_binary()?;
        if !Path::new(SANDBOX_EXEC).is_file() {
            bail!("browser helper sandbox is unavailable on this platform");
        }
        let lease = engine
            .acquire_computer_lease(
                "browser",
                target.session_id,
                target.agent_id,
                target.agent_name,
                Duration::from_secs(LEASE_TTL_SECS),
            )
            .await?;
        let scratch = self.helper_scratch(target);
        let helper =
            match BrowserHelper::spawn(&helper_path, &scratch, target.worktree, lease).await {
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

    /// Return an existing helper only if its ComputerLease row still exists.
    /// An operator release or a sweep that removed the row invalidates the
    /// helper instead of allowing two sessions to drive the same surface.
    async fn existing_helper(
        &self,
        engine: &Engine,
        key: &str,
    ) -> Result<Option<Arc<AsyncMutex<BrowserHelper>>>> {
        let Some(helper) = self
            .helpers
            .lock()
            .ok()
            .and_then(|map| map.get(key).cloned())
        else {
            return Ok(None);
        };
        let lease_id = {
            let helper = helper.lock().await;
            helper.touch();
            helper.lease_id()
        };
        if let Some(lease_id) = lease_id
            && engine.computer_lease_active(&lease_id).await?
        {
            return Ok(Some(helper));
        }
        let stale = self.helpers.lock().ok().and_then(|mut map| map.remove(key));
        if let Some(stale) = stale {
            let mut helper = stale.lock().await;
            helper.shutdown().await;
        }
        Ok(None)
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
                let url = result.get("url").and_then(Value::as_str).unwrap_or("");
                let title = result.get("title").and_then(Value::as_str).unwrap_or("");
                let mut text = String::new();
                text.push_str(&format!("{title} ({url})\n"));
                if let Some(tree) = result.get("tree") {
                    render_tree(tree, 0, &mut text);
                }
                if let Some(body) = result.get("text").and_then(Value::as_str)
                    && !body.is_empty()
                {
                    text.push_str("\nVisible text:\n");
                    text.push_str(body);
                }
                let max_chars = input
                    .get("max_chars")
                    .and_then(Value::as_u64)
                    .unwrap_or(12_000) as usize;
                let content = truncate(text.chars().take(max_chars.max(500)).collect());
                Ok(ToolOutput::ok_structured(
                    content,
                    json!({
                        "ok": true,
                        "action": "snapshot",
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

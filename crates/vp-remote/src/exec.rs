//! Bounded remote command execution.
//!
//! Commands run through the remote user's `/bin/sh`, in a fresh environment, in
//! a process group. Timeouts and cancellations kill the whole group, not just
//! the direct child. Output is capped per stream while the remainder is
//! drained, so a chatty process cannot fill the helper's memory or block on a
//! full pipe.

use std::collections::BTreeSet;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::Notify;
use tokio::time::{sleep, timeout};

use crate::jail::PathJail;
use crate::protocol::{
    ExecParams, ExecResult, MAX_ENV_VALUE_BYTES, MAX_ENV_VARS, MAX_EXEC_OUTPUT_BYTES,
    MAX_EXEC_TIMEOUT_MS, RemoteError, RemoteErrorCode,
};

/// Names the helper always permits from the request environment. They are
/// safe build-tool variables and cannot inject a dynamic loader or provider
/// credential. Projects can widen this with `--env-allow`.
pub const DEFAULT_ENV_ALLOW: &[&str] = &[
    "PATH",
    "HOME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    "TMPDIR",
    "USER",
    "LOGNAME",
    "CI",
    "CARGO_HOME",
    "CARGO_TARGET_DIR",
    "RUSTUP_HOME",
    "RUSTFLAGS",
    "NODE_ENV",
    "NODE_PATH",
    "NPM_CONFIG_CACHE",
    "PYTHONPATH",
    "VIRTUAL_ENV",
    "GOFLAGS",
    "GOPATH",
    "GOCACHE",
    "JAVA_HOME",
];

const MIN_OUTPUT_CAP: u64 = 1024;
const MAX_COMMAND_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone)]
pub struct ExecControl {
    cancelled: Arc<AtomicBool>,
    notify: Arc<Notify>,
    pgid: Arc<Mutex<Option<i32>>>,
}

impl Default for ExecControl {
    fn default() -> Self {
        Self::new()
    }
}

impl ExecControl {
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(Notify::new()),
            pgid: Arc::new(Mutex::new(None)),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Ok(pgid) = self.pgid.lock()
            && let Some(pgid) = *pgid
        {
            kill_process_group(pgid);
        }
        self.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    fn register(&self, pid: u32) {
        if let Ok(mut pgid) = self.pgid.lock() {
            *pgid = Some(pid as i32);
        }
        if self.is_cancelled() {
            kill_process_group(pid as i32);
        }
    }
}

fn kill_process_group(pgid: i32) {
    if pgid <= 1 {
        return;
    }
    // Negative pid targets the process group created with process_group(0).
    // SAFETY: kill(2) with a negative pid has no memory-safety requirements.
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
}

pub fn allowed_env_names(extra: &[String]) -> BTreeSet<String> {
    let mut allowed = DEFAULT_ENV_ALLOW
        .iter()
        .map(|name| (*name).to_string())
        .collect::<BTreeSet<_>>();
    for name in extra {
        allowed.insert(name.clone());
    }
    allowed
}

fn validate_env(params: &ExecParams, allowed: &BTreeSet<String>) -> Result<(), RemoteError> {
    if params.env.len() > MAX_ENV_VARS {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            format!("at most {MAX_ENV_VARS} environment variables may be supplied"),
        ));
    }
    for (name, value) in &params.env {
        if !allowed.contains(name) {
            return Err(RemoteError::new(
                RemoteErrorCode::EnvNotAllowed,
                format!("environment variable {name} is not on the remote helper allowlist"),
            )
            .with_hint("configure it with --env-allow on the helper, or remove it"));
        }
        if value.contains('\0') {
            return Err(RemoteError::new(
                RemoteErrorCode::InvalidParams,
                format!("environment variable {name} contains NUL"),
            ));
        }
        if value.len() > MAX_ENV_VALUE_BYTES {
            return Err(RemoteError::new(
                RemoteErrorCode::InvalidParams,
                format!("environment variable {name} exceeds {MAX_ENV_VALUE_BYTES} bytes"),
            ));
        }
        if matches!(
            name.as_str(),
            "LD_PRELOAD"
                | "LD_LIBRARY_PATH"
                | "DYLD_INSERT_LIBRARIES"
                | "DYLD_LIBRARY_PATH"
                | "BASH_ENV"
                | "ENV"
                | "PROMPT_COMMAND"
        ) {
            return Err(RemoteError::new(
                RemoteErrorCode::EnvNotAllowed,
                format!("environment variable {name} is never allowed"),
            ));
        }
    }
    Ok(())
}

pub fn validate_exec(params: &ExecParams) -> Result<(), RemoteError> {
    if params.command.trim().is_empty() {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            "command must not be empty",
        ));
    }
    if params.command.len() > MAX_COMMAND_BYTES {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            format!("command exceeds {MAX_COMMAND_BYTES} bytes"),
        ));
    }
    if params.command.contains('\0') {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            "command must not contain NUL",
        ));
    }
    if params.timeout_ms == 0 || params.timeout_ms > MAX_EXEC_TIMEOUT_MS {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            format!("timeout_ms must be 1..={MAX_EXEC_TIMEOUT_MS}"),
        ));
    }
    let cap = params.output_cap_bytes;
    if !(MIN_OUTPUT_CAP..=MAX_EXEC_OUTPUT_BYTES).contains(&cap) {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            format!("output_cap_bytes must be {MIN_OUTPUT_CAP}..={MAX_EXEC_OUTPUT_BYTES}"),
        ));
    }
    Ok(())
}

async fn read_capped<R: AsyncRead + Unpin>(
    mut reader: R,
    cap: u64,
) -> std::io::Result<(String, bool)> {
    let cap = cap as usize;
    let mut output = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut truncated = false;
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        if output.len() < cap {
            let take = (cap - output.len()).min(count);
            output.extend_from_slice(&buffer[..take]);
            if take < count {
                truncated = true;
            }
        } else {
            truncated = true;
        }
    }
    Ok((String::from_utf8_lossy(&output).to_string(), truncated))
}

fn safe_environment(command: &mut Command, params: &ExecParams, cwd: &std::path::Path) {
    command.env_clear();
    for name in DEFAULT_ENV_ALLOW {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    if std::env::var_os("PATH").is_none() {
        command.env(
            "PATH",
            "/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin",
        );
    }
    command.env("PWD", cwd);
    for (name, value) in &params.env {
        command.env(name, value);
    }
}

pub async fn run_exec(
    jail: &PathJail,
    params: &ExecParams,
    env_allow: &[String],
    control: &ExecControl,
) -> Result<ExecResult, RemoteError> {
    validate_exec(params)?;
    let allowed = allowed_env_names(env_allow);
    validate_env(params, &allowed)?;
    let cwd = match params.cwd.as_deref() {
        Some(path) => {
            let resolved = jail.resolve_existing(path)?;
            if !resolved.absolute.is_dir() {
                return Err(RemoteError::new(
                    RemoteErrorCode::NotADirectory,
                    format!("exec cwd is not a directory: {path}"),
                ));
            }
            resolved.absolute
        }
        None => jail.root().to_path_buf(),
    };
    let started = Instant::now();
    let wait = Duration::from_millis(params.timeout_ms);
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(&params.command)
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    safe_environment(&mut command, params, &cwd);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }
    let mut child = command.spawn().map_err(|error| {
        RemoteError::new(
            RemoteErrorCode::Internal,
            format!("cannot start remote command: {error}"),
        )
    })?;
    if let Some(pid) = child.id() {
        control.register(pid);
    }
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let cap = params.output_cap_bytes;
    let stdout_task = tokio::spawn(async move {
        match stdout {
            Some(stdout) => read_capped(stdout, cap).await,
            None => Ok((String::new(), false)),
        }
    });
    let stderr_task = tokio::spawn(async move {
        match stderr {
            Some(stderr) => read_capped(stderr, cap).await,
            None => Ok((String::new(), false)),
        }
    });

    let mut timed_out = false;
    let mut cancelled = false;
    let status = tokio::select! {
        _ = control.notify.notified() => {
            cancelled = true;
            if let Ok(pgid) = control.pgid.lock()
                && let Some(pgid) = *pgid
            {
                kill_process_group(pgid);
            }
            let _ = child.start_kill();
            child.wait().await.ok()
        }
        _ = sleep(wait) => {
            timed_out = true;
            if let Ok(pgid) = control.pgid.lock()
                && let Some(pgid) = *pgid
            {
                kill_process_group(pgid);
            }
            let _ = child.start_kill();
            child.wait().await.ok()
        }
        status = child.wait() => status.ok(),
    };

    let (stdout_value, stdout_truncated) = join_reader(stdout_task).await;
    let (stderr_value, stderr_truncated) = join_reader(stderr_task).await;
    Ok(ExecResult {
        exit_code: status.and_then(|status| status.code()),
        stdout: stdout_value,
        stderr: stderr_value,
        stdout_truncated,
        stderr_truncated,
        timed_out,
        cancelled,
        duration_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        operation_id: params.operation_id.clone(),
        replayed: false,
    })
}

async fn join_reader(
    task: tokio::task::JoinHandle<std::io::Result<(String, bool)>>,
) -> (String, bool) {
    match timeout(Duration::from_secs(3), task).await {
        Ok(Ok(Ok(value))) => value,
        Ok(Ok(Err(error))) => (format!("<failed to read command output: {error}>"), true),
        Ok(Err(error)) => (format!("<command output task failed: {error}>"), true),
        Err(_) => (
            "<command output was not drained before the deadline>".to_string(),
            true,
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    fn setup() -> (TempDir, PathJail) {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        let jail = PathJail::new(dir.path()).unwrap();
        (dir, jail)
    }

    fn params(command: &str) -> ExecParams {
        ExecParams {
            command: command.to_string(),
            cwd: None,
            env: Default::default(),
            timeout_ms: 5_000,
            output_cap_bytes: 64 * 1024,
            operation_id: None,
        }
    }

    #[tokio::test]
    async fn captures_output_and_exit_code() {
        let (_dir, jail) = setup();
        let result = run_exec(
            &jail,
            &params("printf hello; printf err >&2; exit 3"),
            &[],
            &ExecControl::new(),
        )
        .await
        .unwrap();
        assert_eq!(result.exit_code, Some(3));
        assert_eq!(result.stdout, "hello");
        assert_eq!(result.stderr, "err");
        assert!(!result.timed_out);
    }

    #[tokio::test]
    async fn output_is_capped_and_drained() {
        let (_dir, jail) = setup();
        let mut params = params("yes x | head -c 200000");
        params.output_cap_bytes = 1024;
        let result = run_exec(&jail, &params, &[], &ExecControl::new())
            .await
            .unwrap();
        assert!(result.stdout_truncated);
        assert!(result.stdout.len() <= 1024);
    }

    #[tokio::test]
    async fn timeout_kills_the_process_group() {
        let (_dir, jail) = setup();
        let mut params = params("sleep 30 & wait");
        params.timeout_ms = 150;
        let started = Instant::now();
        let result = run_exec(&jail, &params, &[], &ExecControl::new())
            .await
            .unwrap();
        assert!(result.timed_out);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn cancellation_kills_a_running_command() {
        let (_dir, jail) = setup();
        let control = ExecControl::new();
        let clone = control.clone();
        tokio::spawn(async move {
            sleep(Duration::from_millis(100)).await;
            clone.cancel();
        });
        let start = Instant::now();
        let result = run_exec(&jail, &params("sleep 30 & wait"), &[], &control)
            .await
            .unwrap();
        assert!(result.cancelled || result.timed_out);
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn cwd_must_stay_inside_the_jail() {
        let (_dir, jail) = setup();
        let mut params = params("pwd");
        params.cwd = Some("../outside".to_string());
        let error = run_exec(&jail, &params, &[], &ExecControl::new())
            .await
            .unwrap_err();
        assert_eq!(error.code, RemoteErrorCode::PathEscape);
    }

    #[tokio::test]
    async fn requested_environment_is_allowlisted() {
        let (_dir, jail) = setup();
        let mut params = params("printf $CUSTOM");
        params.env.insert("CUSTOM".into(), "yes".into());
        let error = run_exec(&jail, &params, &[], &ExecControl::new())
            .await
            .unwrap_err();
        assert_eq!(error.code, RemoteErrorCode::EnvNotAllowed);
        let result = run_exec(&jail, &params, &["CUSTOM".to_string()], &ExecControl::new())
            .await
            .unwrap();
        assert_eq!(result.stdout, "yes");
    }

    #[tokio::test]
    async fn dangerous_environment_names_are_never_allowed() {
        let (_dir, jail) = setup();
        let mut params = params("true");
        params
            .env
            .insert("LD_PRELOAD".into(), "/tmp/evil.so".into());
        let error = run_exec(
            &jail,
            &params,
            &["LD_PRELOAD".to_string()],
            &ExecControl::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, RemoteErrorCode::EnvNotAllowed);
    }

    #[tokio::test]
    async fn provider_environment_is_not_inherited() {
        let (_dir, jail) = setup();
        // The helper's own environment is not rendered unless it is one of the
        // safe names; a provider key cannot leak through an inherited child.
        let result = run_exec(
            &jail,
            &params("env | grep -E 'OPENAI|ANTHROPIC' || true"),
            &[],
            &ExecControl::new(),
        )
        .await
        .unwrap();
        assert!(result.stdout.trim().is_empty());
    }
}

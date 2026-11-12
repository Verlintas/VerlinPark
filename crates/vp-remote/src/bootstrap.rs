//! Remote helper bootstrap.
//!
//! A helper must exist on the remote host before the JSON-RPC client can
//! connect. Bootstrap therefore cannot use the helper protocol itself: it
//! speaks a tiny line-oriented shell protocol over SSH. The local daemon
//! inspects the executable header, hashes the bytes, runs a bounded preflight,
//! streams the payload to a private temporary file, executes the temporary
//! binary's `--describe` mode to prove the architecture and protocol, and only
//! then atomically renames it into place.
//!
//! The remote host is trusted infrastructure (ADR-0156). Bootstrap never uses
//! sudo: it requires an install path under the remote user's `$HOME`, creates
//! private directories, and refuses a path it cannot write as that user. It is
//! an explicit, approval-classified operator action, never an automatic
//! install (ADR-0187).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::time::timeout;

use crate::protocol::{MAX_FRAME_BYTES, REMOTE_PROTOCOL_VERSION, RemoteError, RemoteErrorCode};
use crate::transport::{SshConfig, shell_quote, validate_ssh_host, validate_ssh_user};

/// The helper version this daemon expects. The remote agent and this crate are
/// released from the same workspace, so their package version identifies the
/// compatible helper build.
pub const HELPER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Default private install location. It is expanded against the remote user's
/// `$HOME`; the daemon never guesses or writes another user's home.
pub const DEFAULT_INSTALL_PATH: &str = "~/.verlinpark/bin/vp-remote-agent";
pub const MIN_BOOTSTRAP_BYTES: u64 = 1_024;
pub const MAX_BOOTSTRAP_BYTES: u64 = 128 * 1024 * 1024;
pub const DEFAULT_BOOTSTRAP_CHUNK_BYTES: u64 = 256 * 1024;
pub const MIN_BOOTSTRAP_CHUNK_BYTES: u64 = 4 * 1024;
pub const MAX_BOOTSTRAP_CHUNK_BYTES: u64 = 4 * 1024 * 1024;
pub const DEFAULT_BOOTSTRAP_TIMEOUT_SECS: u64 = 120;
pub const MAX_BOOTSTRAP_TIMEOUT_SECS: u64 = 900;
pub const BOOTSTRAP_DISK_MARGIN_BYTES: u64 = 1024 * 1024;
pub const MAX_INSTALL_PATH_CHARS: usize = 1_024;
pub const MAX_BOOTSTRAP_OUTPUT_BYTES: u64 = 256 * 1024;
pub const PREFLIGHT_HEADER: &str = "VP_BOOTSTRAP_PREFLIGHT_V1";
pub const FINALIZE_HEADER: &str = "VP_BOOTSTRAP_FINALIZE_V1";
pub const UPLOAD_HEADER: &str = "VP_BOOTSTRAP_UPLOAD_V1";
pub const CLEANUP_HEADER: &str = "VP_BOOTSTRAP_CLEANUP_V1";
pub const ERROR_HEADER: &str = "VP_BOOTSTRAP_ERROR_V1";

/// The uploaded executable's target, read from its own header. Comparing this
/// with the remote `uname` prevents shipping a macOS helper to Linux or an
/// arm64 helper to an x86_64 host. The temporary binary must also execute
/// successfully before it can replace anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BinaryTarget {
    pub os: &'static str,
    pub arch: &'static str,
}

impl BinaryTarget {
    pub fn is_supported(self) -> bool {
        matches!(self.os, "macos" | "linux") && matches!(self.arch, "arm64" | "x86_64")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapPhase {
    Preflight,
    Upload,
    Finalize,
    Cleanup,
}

impl BootstrapPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preflight => "preflight",
            Self::Upload => "upload",
            Self::Finalize => "finalize",
            Self::Cleanup => "cleanup",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapRequest {
    pub install_path: String,
    pub expected_sha256: Option<String>,
    pub expected_version: String,
    pub expected_protocol_version: u32,
    pub force: bool,
    pub max_bytes: u64,
    pub chunk_bytes: u64,
    pub timeout: Duration,
}

impl Default for BootstrapRequest {
    fn default() -> Self {
        Self {
            install_path: DEFAULT_INSTALL_PATH.to_string(),
            expected_sha256: None,
            expected_version: HELPER_VERSION.to_string(),
            expected_protocol_version: REMOTE_PROTOCOL_VERSION,
            force: false,
            max_bytes: MAX_BOOTSTRAP_BYTES,
            chunk_bytes: DEFAULT_BOOTSTRAP_CHUNK_BYTES,
            timeout: Duration::from_secs(DEFAULT_BOOTSTRAP_TIMEOUT_SECS),
        }
    }
}

impl BootstrapRequest {
    /// Clamp all bounds so a caller cannot request an unbounded upload or an
    /// unbounded remote wait.
    pub fn normalized(mut self) -> Self {
        self.max_bytes = self
            .max_bytes
            .clamp(MIN_BOOTSTRAP_BYTES, MAX_BOOTSTRAP_BYTES);
        self.chunk_bytes = self
            .chunk_bytes
            .clamp(MIN_BOOTSTRAP_CHUNK_BYTES, MAX_BOOTSTRAP_CHUNK_BYTES);
        let seconds = self.timeout.as_secs().clamp(1, MAX_BOOTSTRAP_TIMEOUT_SECS);
        self.timeout = Duration::from_secs(seconds);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapPreflight {
    pub path: String,
    pub remote_os: String,
    pub remote_arch: String,
    pub exists: bool,
    pub existing_sha256: Option<String>,
    pub existing_bytes: u64,
    pub free_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapInstallation {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
    pub replaced: bool,
    pub helper_version: String,
    pub protocol_version: u32,
    pub target_os: String,
    pub target_arch: String,
    pub remote_os: String,
    pub remote_arch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapOutcome {
    pub install_path: String,
    pub sha256: String,
    pub bytes: u64,
    pub helper_version: String,
    pub protocol_version: u32,
    pub target_os: String,
    pub target_arch: String,
    pub remote_os: String,
    pub remote_arch: String,
    pub replaced: bool,
    /// Always false. The installer has no code path that invokes sudo.
    pub used_sudo: bool,
    pub free_bytes_before: u64,
    pub chunks: u32,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapCommandOutput {
    pub phase: BootstrapPhase,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapUploadOutput {
    pub bytes_written: u64,
    pub chunks: u32,
    pub command: BootstrapCommandOutput,
}

/// Transport used by bootstrap. The production implementation is SSH; tests
/// use a fake helper over stdio and an in-memory transport. Keeping this
/// separate from [`crate::RemoteTransport`] is deliberate: bootstrap must work
/// when no helper is installed, so it cannot depend on the JSON-RPC handshake.
#[async_trait]
pub trait BootstrapTransport: Send + Sync {
    fn description(&self) -> String;
    async fn run(
        &self,
        phase: BootstrapPhase,
        script: &str,
        wait: Duration,
    ) -> Result<BootstrapCommandOutput, RemoteError>;
    async fn upload(
        &self,
        script: &str,
        payload: &[u8],
        chunk_bytes: u64,
        wait: Duration,
    ) -> Result<BootstrapUploadOutput, RemoteError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshBootstrapConfig {
    pub host: String,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity_file: Option<String>,
    pub connect_timeout_secs: u64,
    pub ssh_args: Vec<String>,
}

impl SshBootstrapConfig {
    pub fn from_ssh(config: &SshConfig) -> Self {
        Self {
            host: config.host.clone(),
            user: config.user.clone(),
            port: config.port,
            identity_file: config.identity_file.clone(),
            connect_timeout_secs: config.connect_timeout_secs,
            ssh_args: config.ssh_args.clone(),
        }
    }

    pub fn validate(&self) -> Result<(), RemoteError> {
        validate_ssh_host(&self.host)?;
        if let Some(user) = self.user.as_deref() {
            validate_ssh_user(user)?;
        }
        if self.port == Some(0) {
            return Err(RemoteError::new(
                RemoteErrorCode::InvalidParams,
                "ssh port must be between 1 and 65535",
            ));
        }
        if !(1..=300).contains(&self.connect_timeout_secs) {
            return Err(RemoteError::new(
                RemoteErrorCode::InvalidParams,
                "bootstrap connect timeout must be 1..=300 seconds",
            ));
        }
        if self.ssh_args.len() > 32 {
            return Err(RemoteError::new(
                RemoteErrorCode::InvalidParams,
                "at most 32 extra ssh arguments are allowed",
            ));
        }
        for argument in &self.ssh_args {
            if argument.is_empty()
                || argument.len() > 512
                || argument.contains('\0')
                || argument.contains('\n')
                || argument.contains('\r')
            {
                return Err(RemoteError::new(
                    RemoteErrorCode::InvalidParams,
                    "each extra ssh argument must be 1..=512 bytes without control characters",
                ));
            }
        }
        Ok(())
    }

    /// Build the exact SSH argv for one bootstrap phase. The remote command is
    /// one `sh -c` script, single-quoted for the remote shell, and no local
    /// shell is invoked.
    pub fn argv(&self, script: &str) -> Result<Vec<String>, RemoteError> {
        self.validate()?;
        if script.len() as u64 > MAX_FRAME_BYTES {
            return Err(RemoteError::new(
                RemoteErrorCode::InvalidParams,
                "bootstrap script exceeds the frame limit",
            ));
        }
        let mut args = vec![
            "-T".to_string(),
            "-o".to_string(),
            "BatchMode=yes".to_string(),
            "-o".to_string(),
            format!("ConnectTimeout={}", self.connect_timeout_secs),
        ];
        if let Some(port) = self.port {
            args.push("-p".to_string());
            args.push(port.to_string());
        }
        if let Some(identity) = self.identity_file.as_deref() {
            args.push("-i".to_string());
            args.push(identity.to_string());
        }
        args.extend(self.ssh_args.iter().cloned());
        if let Some(user) = self.user.as_deref() {
            args.push("-l".to_string());
            args.push(user.to_string());
        }
        args.push(self.host.clone());
        args.push(format!("sh -c {}", shell_quote(script)));
        Ok(args)
    }
}

/// SSH bootstrap transport. It shares the host/identity/argument policy of the
/// ordinary SSH transport but never needs a helper token.
#[derive(Debug, Clone)]
pub struct SshBootstrapTransport {
    config: SshBootstrapConfig,
}

impl SshBootstrapTransport {
    pub fn new(config: SshBootstrapConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &SshBootstrapConfig {
        &self.config
    }
}

#[async_trait]
impl BootstrapTransport for SshBootstrapTransport {
    fn description(&self) -> String {
        format!(
            "ssh {}:{}",
            self.config.user.as_deref().unwrap_or("user"),
            self.config.host
        )
    }

    async fn run(
        &self,
        phase: BootstrapPhase,
        script: &str,
        wait: Duration,
    ) -> Result<BootstrapCommandOutput, RemoteError> {
        let argv = self.config.argv(script)?;
        run_capture("ssh", &argv, phase, wait, None).await
    }

    async fn upload(
        &self,
        script: &str,
        payload: &[u8],
        chunk_bytes: u64,
        wait: Duration,
    ) -> Result<BootstrapUploadOutput, RemoteError> {
        let argv = self.config.argv(script)?;
        let chunk_bytes = chunk_bytes.clamp(MIN_BOOTSTRAP_CHUNK_BYTES, MAX_BOOTSTRAP_CHUNK_BYTES);
        let chunks = payload.len().div_ceil(chunk_bytes as usize) as u32;
        let command = run_capture(
            "ssh",
            &argv,
            BootstrapPhase::Upload,
            wait,
            Some((payload, chunk_bytes)),
        )
        .await?;
        Ok(BootstrapUploadOutput {
            bytes_written: payload.len() as u64,
            chunks,
            command,
        })
    }
}

/// Stdio bootstrap transport used by fake-host tests and embedders. It runs an
/// arbitrary program/argv and passes the phase as the final argument. The
/// production daemon uses [`SshBootstrapTransport`].
#[derive(Debug, Clone)]
pub struct StdioBootstrapTransport {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub description: String,
}

impl StdioBootstrapTransport {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        let program = program.into();
        let description = program.display().to_string();
        Self {
            program,
            args: Vec::new(),
            description,
        }
    }

    pub fn arg(mut self, value: impl Into<String>) -> Self {
        self.args.push(value.into());
        self
    }

    pub fn described_as(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }
}

#[async_trait]
impl BootstrapTransport for StdioBootstrapTransport {
    fn description(&self) -> String {
        self.description.clone()
    }

    async fn run(
        &self,
        phase: BootstrapPhase,
        script: &str,
        wait: Duration,
    ) -> Result<BootstrapCommandOutput, RemoteError> {
        let mut argv = self.args.clone();
        argv.push(phase.as_str().to_string());
        argv.push(script.to_string());
        run_capture(&self.program.to_string_lossy(), &argv, phase, wait, None).await
    }

    async fn upload(
        &self,
        script: &str,
        payload: &[u8],
        chunk_bytes: u64,
        wait: Duration,
    ) -> Result<BootstrapUploadOutput, RemoteError> {
        let mut argv = self.args.clone();
        argv.push(BootstrapPhase::Upload.as_str().to_string());
        argv.push(script.to_string());
        let chunk_bytes = chunk_bytes.clamp(MIN_BOOTSTRAP_CHUNK_BYTES, MAX_BOOTSTRAP_CHUNK_BYTES);
        let chunks = payload.len().div_ceil(chunk_bytes as usize) as u32;
        let command = run_capture(
            &self.program.to_string_lossy(),
            &argv,
            BootstrapPhase::Upload,
            wait,
            Some((payload, chunk_bytes)),
        )
        .await?;
        Ok(BootstrapUploadOutput {
            bytes_written: payload.len() as u64,
            chunks,
            command,
        })
    }
}

async fn run_capture(
    program: &str,
    argv: &[String],
    phase: BootstrapPhase,
    wait: Duration,
    payload: Option<(&[u8], u64)>,
) -> Result<BootstrapCommandOutput, RemoteError> {
    let started = Instant::now();
    let mut command = Command::new(program);
    command
        .args(argv)
        .stdin(if payload.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command.env_clear();
    for name in [
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "TERM",
        "TMPDIR",
        "SSH_AUTH_SOCK",
        "SSH_AGENT_PID",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let mut child = command.spawn().map_err(|error| {
        RemoteError::new(
            RemoteErrorCode::TransportClosed,
            format!("cannot start bootstrap transport {program}: {error}"),
        )
        .with_hint("verify ssh is installed and the host/identity configuration is correct")
    })?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_task =
        tokio::spawn(async move { read_capped(stdout, MAX_BOOTSTRAP_OUTPUT_BYTES).await });
    let stderr_task =
        tokio::spawn(async move { read_capped(stderr, MAX_BOOTSTRAP_OUTPUT_BYTES).await });

    let mut chunks = 0u32;
    if let Some((bytes, chunk_bytes)) = payload {
        let Some(mut stdin) = child.stdin.take() else {
            let _ = child.start_kill();
            return Err(RemoteError::new(
                RemoteErrorCode::BootstrapUploadFailed,
                "bootstrap transport did not expose stdin",
            ));
        };
        let chunk = chunk_bytes.max(1) as usize;
        for slice in bytes.chunks(chunk) {
            if let Err(error) = stdin.write_all(slice).await {
                let _ = child.start_kill();
                return Err(RemoteError::new(
                    RemoteErrorCode::BootstrapUploadFailed,
                    format!("bootstrap upload failed while streaming the helper: {error}"),
                )
                .with_hint(
                    "the remote host closed the upload; retry after checking disk and SSH",
                ));
            }
            if let Err(error) = stdin.flush().await {
                let _ = child.start_kill();
                return Err(RemoteError::new(
                    RemoteErrorCode::BootstrapUploadFailed,
                    format!("bootstrap upload flush failed: {error}"),
                ));
            }
            chunks = chunks.saturating_add(1);
        }
        drop(stdin);
    }

    let status = match timeout(wait, child.wait()).await {
        Ok(result) => result,
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(RemoteError::new(
                RemoteErrorCode::Timeout,
                format!(
                    "bootstrap {} did not complete within {wait:?}; the SSH process was killed",
                    phase.as_str()
                ),
            )
            .with_hint("raise the bounded bootstrap timeout or check SSH connectivity"));
        }
    }
    .map_err(|error| {
        RemoteError::new(
            RemoteErrorCode::TransportClosed,
            format!("bootstrap {} process failed: {error}", phase.as_str()),
        )
    })?;
    let (stdout, stdout_truncated) = join_reader(stdout_task).await;
    let (stderr, stderr_truncated) = join_reader(stderr_task).await;
    Ok(BootstrapCommandOutput {
        phase,
        stdout,
        stderr,
        exit_code: status.code(),
        duration_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        truncated: stdout_truncated || stderr_truncated,
    })
}

async fn read_capped<R: AsyncRead + Unpin>(
    reader: Option<R>,
    cap: u64,
) -> Result<(String, bool), std::io::Error> {
    let Some(mut reader) = reader else {
        return Ok((String::new(), false));
    };
    let mut output = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut truncated = false;
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        if output.len() < cap as usize {
            let take = ((cap as usize) - output.len()).min(count);
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

async fn join_reader(
    task: tokio::task::JoinHandle<Result<(String, bool), std::io::Error>>,
) -> (String, bool) {
    match task.await {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => (format!("<bootstrap output read failed: {error}>"), true),
        Err(error) => (format!("<bootstrap output task failed: {error}>"), true),
    }
}

/// Read the target OS/architecture from an executable header. This is a
/// deliberately small parser: it supports Mach-O and ELF, the two formats this
/// project ships. Unknown formats fail closed instead of guessing from the
/// daemon's own target.
pub fn inspect_binary_target(bytes: &[u8]) -> Result<BinaryTarget, RemoteError> {
    if bytes.len() < 20 {
        return Err(unsupported_binary("helper file is too small to inspect"));
    }
    if bytes.starts_with(&[0x7f, b'E', b'L', b'F']) {
        let data = bytes[5];
        let machine = match data {
            1 => u16::from_le_bytes([bytes[18], bytes[19]]),
            2 => u16::from_be_bytes([bytes[18], bytes[19]]),
            _ => {
                return Err(unsupported_binary(
                    "ELF data encoding is not little/big endian",
                ));
            }
        };
        let arch = match machine {
            0x3e => "x86_64",
            0xb7 => "arm64",
            other => {
                return Err(unsupported_binary(format!(
                    "unsupported ELF machine 0x{other:04x}"
                )));
            }
        };
        return Ok(BinaryTarget { os: "linux", arch });
    }
    let magic = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    match magic {
        0xfeedfacf | 0xfeedface => {
            let cpu = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
            Ok(BinaryTarget {
                os: "macos",
                arch: mach_cpu_arch(cpu)?,
            })
        }
        0xcffaedfe | 0xcefaedfe => {
            let cpu = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
            Ok(BinaryTarget {
                os: "macos",
                arch: mach_cpu_arch(cpu)?,
            })
        }
        0xcafebabe | 0xbebafeca => Err(unsupported_binary(
            "universal Mach-O helpers are not supported; upload a single-architecture helper",
        )),
        _ => Err(unsupported_binary(
            "helper is not a Mach-O or ELF executable",
        )),
    }
}

fn mach_cpu_arch(cpu: u32) -> Result<&'static str, RemoteError> {
    match cpu {
        0x0100_000c | 12 => Ok("arm64"),
        0x0100_0007 | 7 => Ok("x86_64"),
        other => Err(unsupported_binary(format!(
            "unsupported Mach-O CPU type 0x{other:08x}"
        ))),
    }
}

fn unsupported_binary(message: impl Into<String>) -> RemoteError {
    RemoteError::new(RemoteErrorCode::BootstrapUnsupported, message).with_hint(
        "build vp-remote-agent for the remote host's OS and architecture; bootstrap rejects unknown executables",
    )
}

/// Normalize `uname -s` output.
pub fn normalize_remote_os(value: &str) -> Option<&'static str> {
    match value.trim() {
        "Darwin" | "darwin" | "macOS" | "macos" => Some("macos"),
        "Linux" | "linux" => Some("linux"),
        _ => None,
    }
}

/// Normalize `uname -m` output.
pub fn normalize_remote_arch(value: &str) -> Option<&'static str> {
    match value.trim() {
        "arm64" | "aarch64" | "arm64e" => Some("arm64"),
        "x86_64" | "amd64" | "x64" => Some("x86_64"),
        _ => None,
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// Validate an install path before it becomes a shell fragment. The remote
/// script independently enforces `$HOME` containment; this is the local half
/// that guarantees the string cannot inject shell syntax.
pub fn validate_install_path(path: &str) -> Result<String, RemoteError> {
    let path = path.trim();
    if path.is_empty() {
        return Err(RemoteError::new(
            RemoteErrorCode::BootstrapPathRejected,
            "install path must not be empty",
        )
        .with_hint("use the default ~/.verlinpark/bin/vp-remote-agent"));
    }
    if path.chars().count() > MAX_INSTALL_PATH_CHARS {
        return Err(RemoteError::new(
            RemoteErrorCode::BootstrapPathRejected,
            format!("install path exceeds {MAX_INSTALL_PATH_CHARS} characters"),
        ));
    }
    if path.ends_with('/') {
        return Err(RemoteError::new(
            RemoteErrorCode::BootstrapPathRejected,
            "install path must name a file, not a directory",
        ));
    }
    let rest = if let Some(rest) = path.strip_prefix("~/") {
        if rest.is_empty() {
            return Err(RemoteError::new(
                RemoteErrorCode::BootstrapPathRejected,
                "install path must name a file under the remote home",
            ));
        }
        rest
    } else if path.starts_with('/') {
        path.trim_start_matches('/')
    } else {
        return Err(RemoteError::new(
            RemoteErrorCode::BootstrapPathRejected,
            "install path must start with ~/ or / and identify a remote file",
        )
        .with_hint("the helper is installed as the SSH user; no sudo path is supported"));
    };
    if rest.is_empty() {
        return Err(RemoteError::new(
            RemoteErrorCode::BootstrapPathRejected,
            "install path must name a file",
        ));
    }
    if path.chars().any(|character| {
        !(character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-' | '/' | '~'))
    }) {
        return Err(RemoteError::new(
            RemoteErrorCode::BootstrapPathRejected,
            "install path may contain only ASCII letters, digits, '.', '_', '-', and '/'",
        )
        .with_hint("paths with spaces, quotes, or shell metacharacters are rejected"));
    }
    if rest
        .split('/')
        .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(RemoteError::new(
            RemoteErrorCode::BootstrapPathRejected,
            "install path must not contain empty, '.' or '..' components",
        ));
    }
    if rest.len() > 900 {
        return Err(RemoteError::new(
            RemoteErrorCode::BootstrapPathRejected,
            "install path is too deep for the bounded temporary path",
        ));
    }
    Ok(path.to_string())
}

pub fn validate_expected_version(value: &str) -> Result<String, RemoteError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 128 {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            "expected helper version must be 1..=128 characters",
        ));
    }
    if !value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "._+-".contains(character))
    {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            "expected helper version may contain only ASCII letters, digits, '.', '_', '+', and '-'",
        ));
    }
    Ok(value.to_string())
}

pub fn validate_sha256(value: &str) -> Result<String, RemoteError> {
    let value = value.trim().to_ascii_lowercase();
    if value.len() != 64 || !value.chars().all(|character| character.is_ascii_hexdigit()) {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            "expected SHA-256 must be exactly 64 hexadecimal characters",
        ));
    }
    Ok(value)
}

fn temp_path(install_path: &str, token: &str) -> String {
    format!("{install_path}.part-{token}")
}

fn bootstrap_token() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::SeqCst);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{}-{}-{}", std::process::id(), nanos, counter)
}

fn script_header() -> &'static str {
    "set -u\n"
}

pub fn preflight_script(install_path: &str) -> String {
    let path = shell_quote(install_path);
    format!(
        "{}path={path}\n\
         case \"$path\" in\n\
           \"~/\"*) path=\"$HOME/${{path#\\~/}}\" ;;\n\
         esac\n\
         case \"$path\" in\n\
           \"$HOME\"/*) ;;\n\
           *) printf '%s\\n' '{error}' 'code=BOOTSTRAP_PATH_REJECTED' 'message=install path must be inside the remote user home'; exit 2 ;;\n\
         esac\n\
         dir=$(dirname \"$path\" 2>/dev/null) || {{ printf '%s\\n' '{error}' 'code=BOOTSTRAP_PATH_REJECTED' 'message=invalid install directory'; exit 2; }}\n\
         mkdir -p \"$dir\" 2>/dev/null || {{ printf '%s\\n' '{error}' 'code=BOOTSTRAP_PATH_REJECTED' 'message=cannot create the install directory'; exit 2; }}\n\
         if [ ! -w \"$dir\" ]; then printf '%s\\n' '{error}' 'code=BOOTSTRAP_PATH_REJECTED' 'message=install directory is not writable by the SSH user'; exit 2; fi\n\
         case \"$dir\" in \"$HOME/.verlinpark\"/*) chmod 700 \"$dir\" 2>/dev/null || true ;; esac\n\
         if ! command -v sha256sum >/dev/null 2>&1 && ! command -v shasum >/dev/null 2>&1; then printf '%s\\n' '{error}' 'code=BOOTSTRAP_UNSUPPORTED' 'message=no SHA-256 tool (sha256sum or shasum) is installed'; exit 2; fi\n\
         hash_file() {{ if command -v sha256sum >/dev/null 2>&1; then sha256sum \"$1\"; else shasum -a 256 \"$1\"; fi; }}\n\
         exists=0; existing_sha=; existing_bytes=0\n\
         if [ -e \"$path\" ]; then exists=1; existing_bytes=$(wc -c < \"$path\" 2>/dev/null | tr -d ' '); existing_sha=$(hash_file \"$path\" 2>/dev/null | awk '{{print $1}}'); fi\n\
         os=$(uname -s 2>/dev/null || printf unknown)\n\
         arch=$(uname -m 2>/dev/null || printf unknown)\n\
         free_kb=$(df -Pk \"$dir\" 2>/dev/null | awk 'NR==2 {{print $4}}')\n\
         case \"$free_kb\" in ''|*[!0-9]*) free_bytes=0 ;; *) free_bytes=$((free_kb * 1024)) ;; esac\n\
         printf '%s\\n' '{success}'\n\
         printf 'path=%s\\n' \"$path\"\n\
         printf 'os=%s\\n' \"$os\"\n\
         printf 'arch=%s\\n' \"$arch\"\n\
         printf 'exists=%s\\n' \"$exists\"\n\
         printf 'existing_sha256=%s\\n' \"$existing_sha\"\n\
         printf 'existing_bytes=%s\\n' \"$existing_bytes\"\n\
         printf 'free_bytes=%s\\n' \"$free_bytes\"\n",
        script_header(),
        error = ERROR_HEADER,
        success = PREFLIGHT_HEADER,
    )
}

pub fn upload_script(install_path: &str, token: &str) -> String {
    let temp = shell_quote(&temp_path(install_path, token));
    format!(
        "{}\ntmp={temp}\ncase \"$tmp\" in\n  \"~/\"*) tmp=\"$HOME/${{tmp#\\~/}}\" ;;\nesac\numask 077\ncat > \"$tmp\" || exit 1\nprintf '%s\\n' '{header}'\n",
        script_header(),
        header = UPLOAD_HEADER,
    )
}

pub fn finalize_script(
    install_path: &str,
    token: &str,
    expected_sha256: &str,
    expected_version: &str,
    expected_protocol_version: u32,
    force: bool,
) -> String {
    let path = shell_quote(install_path);
    let temp = shell_quote(&temp_path(install_path, token));
    let sha = shell_quote(expected_sha256);
    let version = shell_quote(expected_version);
    let force = if force { "1" } else { "0" };
    format!(
        "{}path={path}\ntmp={temp}\n\
         case \"$path\" in\n\
           \"~/\"*) path=\"$HOME/${{path#\\~/}}\" ;;\n\
         esac\n\
         case \"$tmp\" in\n\
           \"~/\"*) tmp=\"$HOME/${{tmp#\\~/}}\" ;;\n\
         esac\n\
         case \"$path\" in\n\
           \"$HOME\"/*) ;;\n\
           *) printf '%s\\n' '{error}' 'code=BOOTSTRAP_PATH_REJECTED' 'message=install path must be inside the remote user home'; exit 2 ;;\n\
         esac\n\
         if [ ! -f \"$tmp\" ]; then printf '%s\\n' '{error}' 'code=BOOTSTRAP_INSTALL_FAILED' 'message=uploaded temporary file is missing'; exit 2; fi\n\
         chmod 700 \"$tmp\" 2>/dev/null || {{ rm -f \"$tmp\"; printf '%s\\n' '{error}' 'code=BOOTSTRAP_INSTALL_FAILED' 'message=cannot make the temporary helper executable'; exit 2; }}\n\
         hash_file() {{ if command -v sha256sum >/dev/null 2>&1; then sha256sum \"$1\"; else shasum -a 256 \"$1\"; fi; }}\n\
         actual=$(hash_file \"$tmp\" 2>/dev/null | awk '{{print $1}}')\n\
         if [ \"$actual\" != {sha} ]; then rm -f \"$tmp\"; printf '%s\\n' '{error}' 'code=BOOTSTRAP_CHECKSUM_MISMATCH' 'message=uploaded helper SHA-256 does not match the local binary'; exit 2; fi\n\
         describe=$(\"$tmp\" remote-agent --describe 2>/dev/null) || {{ rm -f \"$tmp\"; printf '%s\\n' '{error}' 'code=BOOTSTRAP_ARCH_MISMATCH' 'message=uploaded helper cannot execute on this host'; exit 2; }}\n\
         case \"$describe\" in \\{{*\\}}) ;; *) rm -f \"$tmp\"; printf '%s\\n' '{error}' 'code=BOOTSTRAP_INSTALL_FAILED' 'message=uploaded helper did not return a describe object'; exit 2 ;; esac\n\
         protocol=$(printf '%s\\n' \"$describe\" | sed -n 's/.*\"protocol_version\":\\([0-9][0-9]*\\).*/\\1/p' | head -n 1)\n\
         helper_version=$(printf '%s\\n' \"$describe\" | sed -n 's/.*\"helper_version\":\"\\([A-Za-z0-9._+-]*\\)\".*/\\1/p' | head -n 1)\n\
         target_os=$(printf '%s\\n' \"$describe\" | sed -n 's/.*\"target_os\":\"\\([A-Za-z0-9._+-]*\\)\".*/\\1/p' | head -n 1)\n\
         target_arch=$(printf '%s\\n' \"$describe\" | sed -n 's/.*\"target_arch\":\"\\([A-Za-z0-9._+-]*\\)\".*/\\1/p' | head -n 1)\n\
         if [ \"$protocol\" != '{expected_protocol_version}' ]; then rm -f \"$tmp\"; printf '%s\\n' '{error}' 'code=BOOTSTRAP_PROTOCOL_MISMATCH' \"message=helper protocol $protocol does not match {expected_protocol_version}\"; exit 2; fi\n\
         if [ \"$helper_version\" != {version} ]; then rm -f \"$tmp\"; printf '%s\\n' '{error}' 'code=BOOTSTRAP_VERSION_MISMATCH' \"message=helper version $helper_version does not match {expected_version}\"; exit 2; fi\n\
         replaced=0\n\
         if [ -e \"$path\" ]; then\n\
           if [ '{force}' != \"1\" ]; then rm -f \"$tmp\"; printf '%s\\n' '{error}' 'code=BOOTSTRAP_EXISTS' 'message=helper already exists; pass force to replace it'; exit 2; fi\n\
           replaced=1\n\
         fi\n\
         mv -f \"$tmp\" \"$path\" || {{ rm -f \"$tmp\"; printf '%s\\n' '{error}' 'code=BOOTSTRAP_INSTALL_FAILED' 'message=atomic rename into the install path failed'; exit 2; }}\n\
         chmod 700 \"$path\" 2>/dev/null || {{ printf '%s\\n' '{error}' 'code=BOOTSTRAP_INSTALL_FAILED' 'message=cannot restrict the installed helper to mode 700'; exit 2; }}\n\
         bytes=$(wc -c < \"$path\" | tr -d ' ')\n\
         os=$(uname -s 2>/dev/null || printf unknown)\n\
         arch=$(uname -m 2>/dev/null || printf unknown)\n\
         printf '%s\\n' '{success}'\n\
         printf 'path=%s\\n' \"$path\"\n\
         printf 'sha256=%s\\n' \"$actual\"\n\
         printf 'bytes=%s\\n' \"$bytes\"\n\
         printf 'replaced=%s\\n' \"$replaced\"\n\
         printf 'protocol=%s\\n' \"$protocol\"\n\
         printf 'version=%s\\n' \"$helper_version\"\n\
         printf 'target_os=%s\\n' \"$target_os\"\n\
         printf 'target_arch=%s\\n' \"$target_arch\"\n\
         printf 'os=%s\\n' \"$os\"\n\
         printf 'arch=%s\\n' \"$arch\"\n",
        script_header(),
        error = ERROR_HEADER,
        success = FINALIZE_HEADER,
    )
}

pub fn cleanup_script(install_path: &str, token: &str) -> String {
    let temp = shell_quote(&temp_path(install_path, token));
    format!(
        "{}\ntmp={temp}\ncase \"$tmp\" in\n  \"~/\"*) tmp=\"$HOME/${{tmp#\\~/}}\" ;;\nesac\nrm -f \"$tmp\"\nprintf '%s\\n' '{header}'\n",
        script_header(),
        header = CLEANUP_HEADER,
    )
}

/// Run the complete bootstrap sequence against an explicit transport. The host
/// label is only used for diagnostics; credentials never appear here.
pub async fn bootstrap_helper(
    transport: &dyn BootstrapTransport,
    request: BootstrapRequest,
    helper_bytes: &[u8],
) -> Result<BootstrapOutcome, RemoteError> {
    let request = request.normalized();
    let install_path = validate_install_path(&request.install_path)?;
    let expected_version = validate_expected_version(&request.expected_version)?;
    let expected_sha = request
        .expected_sha256
        .as_deref()
        .map(validate_sha256)
        .transpose()?;
    if helper_bytes.len() as u64 > request.max_bytes {
        return Err(RemoteError::new(
            RemoteErrorCode::BootstrapUnsupported,
            format!(
                "helper is {} bytes, above the {} byte bootstrap cap",
                helper_bytes.len(),
                request.max_bytes
            ),
        )
        .with_hint("raise the bounded bootstrap byte cap only after reviewing the artifact"));
    }
    if (helper_bytes.len() as u64) < MIN_BOOTSTRAP_BYTES {
        return Err(RemoteError::new(
            RemoteErrorCode::BootstrapUnsupported,
            "helper file is too small to be a vp-remote-agent executable",
        ));
    }
    let local_sha = sha256_hex(helper_bytes);
    if let Some(expected) = expected_sha.as_deref()
        && expected != local_sha
    {
        return Err(RemoteError::new(
            RemoteErrorCode::BootstrapChecksumMismatch,
            format!(
                "local helper SHA-256 {local_sha} does not match the pinned checksum {expected}"
            ),
        )
        .with_hint("rebuild the helper or update the expected checksum; nothing was uploaded"));
    }
    let target = inspect_binary_target(helper_bytes)?;

    let started = Instant::now();
    let preflight = run_preflight(transport, &install_path, request.timeout).await?;
    check_preflight(
        &preflight,
        helper_bytes.len() as u64,
        &target,
        &request,
        &local_sha,
    )?;

    let token = bootstrap_token();
    let upload = upload_script(&install_path, &token);
    let upload_result = transport
        .upload(&upload, helper_bytes, request.chunk_bytes, request.timeout)
        .await;
    let upload_result = match upload_result {
        Ok(value) => value,
        Err(error) => {
            best_effort_cleanup(transport, &install_path, &token, request.timeout).await;
            return Err(error);
        }
    };
    if upload_result.command.exit_code != Some(0) {
        best_effort_cleanup(transport, &install_path, &token, request.timeout).await;
        return Err(RemoteError::new(
            RemoteErrorCode::BootstrapUploadFailed,
            format!(
                "remote upload exited with {:?}: {}",
                upload_result.command.exit_code,
                short_output(&upload_result.command)
            ),
        )
        .with_hint("check remote disk space and write permission for the install directory"));
    }
    if let Some(error) = parse_error_output(&upload_result.command) {
        best_effort_cleanup(transport, &install_path, &token, request.timeout).await;
        return Err(error);
    }

    let finalize = finalize_script(
        &install_path,
        &token,
        &local_sha,
        &expected_version,
        request.expected_protocol_version,
        request.force,
    );
    let finalize_result = transport
        .run(BootstrapPhase::Finalize, &finalize, request.timeout)
        .await;
    let finalize_result = match finalize_result {
        Ok(value) => value,
        Err(error) => {
            best_effort_cleanup(transport, &install_path, &token, request.timeout).await;
            return Err(error);
        }
    };
    if let Some(error) = parse_error_output(&finalize_result) {
        best_effort_cleanup(transport, &install_path, &token, request.timeout).await;
        return Err(error);
    }
    if finalize_result.exit_code != Some(0) {
        best_effort_cleanup(transport, &install_path, &token, request.timeout).await;
        return Err(RemoteError::new(
            RemoteErrorCode::BootstrapInstallFailed,
            format!(
                "remote install exited with {:?}: {}",
                finalize_result.exit_code,
                short_output(&finalize_result)
            ),
        ));
    }
    let installation = parse_finalize(&finalize_result)?;
    if installation.sha256 != local_sha {
        return Err(RemoteError::new(
            RemoteErrorCode::BootstrapChecksumMismatch,
            "the installed helper SHA-256 does not match the uploaded bytes",
        )
        .with_hint("remove the installed helper and retry; the bytes were corrupted in transit"));
    }
    if installation.bytes != helper_bytes.len() as u64 {
        return Err(RemoteError::new(
            RemoteErrorCode::BootstrapChecksumMismatch,
            format!(
                "installed helper is {} bytes, local helper is {} bytes",
                installation.bytes,
                helper_bytes.len()
            ),
        ));
    }
    if installation.protocol_version != request.expected_protocol_version {
        return Err(RemoteError::new(
            RemoteErrorCode::BootstrapProtocolMismatch,
            format!(
                "installed helper speaks protocol {}, expected {}",
                installation.protocol_version, request.expected_protocol_version
            ),
        )
        .with_hint("build the helper from the same workspace revision as the daemon"));
    }
    if installation.helper_version != expected_version {
        return Err(RemoteError::new(
            RemoteErrorCode::BootstrapVersionMismatch,
            format!(
                "installed helper version {}, expected {}",
                installation.helper_version, expected_version
            ),
        ));
    }
    let remote_os = normalize_remote_os(&installation.remote_os).ok_or_else(|| {
        RemoteError::new(
            RemoteErrorCode::BootstrapUnsupported,
            format!(
                "unsupported remote operating system: {}",
                installation.remote_os
            ),
        )
    })?;
    let remote_arch = normalize_remote_arch(&installation.remote_arch).ok_or_else(|| {
        RemoteError::new(
            RemoteErrorCode::BootstrapUnsupported,
            format!(
                "unsupported remote architecture: {}",
                installation.remote_arch
            ),
        )
    })?;
    if remote_os != target.os || remote_arch != target.arch {
        return Err(RemoteError::new(
            RemoteErrorCode::BootstrapArchMismatch,
            format!(
                "helper target {}/{} does not match remote host {}/{}",
                target.os, target.arch, remote_os, remote_arch
            ),
        ));
    }
    if normalize_remote_os(&installation.target_os) != Some(target.os)
        || normalize_remote_arch(&installation.target_arch) != Some(target.arch)
    {
        return Err(RemoteError::new(
            RemoteErrorCode::BootstrapArchMismatch,
            "the installed binary's self-described target changed after the rename",
        ));
    }
    Ok(BootstrapOutcome {
        install_path: installation.path,
        sha256: installation.sha256,
        bytes: installation.bytes,
        helper_version: installation.helper_version,
        protocol_version: installation.protocol_version,
        target_os: installation.target_os,
        target_arch: installation.target_arch,
        remote_os: installation.remote_os,
        remote_arch: installation.remote_arch,
        replaced: installation.replaced,
        used_sudo: false,
        free_bytes_before: preflight.free_bytes,
        chunks: upload_result.chunks,
        duration_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
    })
}

async fn run_preflight(
    transport: &dyn BootstrapTransport,
    install_path: &str,
    wait: Duration,
) -> Result<BootstrapPreflight, RemoteError> {
    let script = preflight_script(install_path);
    let output = transport
        .run(BootstrapPhase::Preflight, &script, wait)
        .await?;
    if let Some(error) = parse_error_output(&output) {
        return Err(error);
    }
    if output.exit_code != Some(0) {
        return Err(RemoteError::new(
            RemoteErrorCode::BootstrapInstallFailed,
            format!(
                "remote bootstrap preflight exited with {:?}: {}",
                output.exit_code,
                short_output(&output)
            ),
        ));
    }
    parse_preflight(&output)
}

fn check_preflight(
    preflight: &BootstrapPreflight,
    helper_bytes: u64,
    target: &BinaryTarget,
    request: &BootstrapRequest,
    local_sha: &str,
) -> Result<(), RemoteError> {
    let remote_os = normalize_remote_os(&preflight.remote_os).ok_or_else(|| {
        RemoteError::new(
            RemoteErrorCode::BootstrapUnsupported,
            format!(
                "unsupported remote operating system: {}",
                preflight.remote_os
            ),
        )
    })?;
    let remote_arch = normalize_remote_arch(&preflight.remote_arch).ok_or_else(|| {
        RemoteError::new(
            RemoteErrorCode::BootstrapUnsupported,
            format!("unsupported remote architecture: {}", preflight.remote_arch),
        )
    })?;
    if remote_os != target.os || remote_arch != target.arch {
        return Err(RemoteError::new(
            RemoteErrorCode::BootstrapArchMismatch,
            format!(
                "local helper target is {}/{}, remote host is {}/{}; nothing was uploaded",
                target.os, target.arch, remote_os, remote_arch
            ),
        )
        .with_hint("cross-compile vp-remote-agent for the remote OS/architecture"));
    }
    if preflight.exists && !request.force {
        return Err(RemoteError::new(
            RemoteErrorCode::BootstrapExists,
            format!(
                "helper already exists at {}; replacement requires explicit force",
                preflight.path
            ),
        )
        .with_hint("pass force after confirming the installed helper is yours to replace"));
    }
    if preflight.exists && !request.force && preflight.existing_sha256.as_deref() == Some(local_sha)
    {
        return Err(RemoteError::new(
            RemoteErrorCode::BootstrapExists,
            "an identical helper is already installed; force is still required to replace it",
        ));
    }
    if preflight.free_bytes > 0
        && preflight.free_bytes < helper_bytes.saturating_add(BOOTSTRAP_DISK_MARGIN_BYTES)
    {
        return Err(RemoteError::new(
            RemoteErrorCode::BootstrapDisk,
            format!(
                "remote filesystem has {} free bytes, need at least {}",
                preflight.free_bytes,
                helper_bytes.saturating_add(BOOTSTRAP_DISK_MARGIN_BYTES)
            ),
        )
        .with_hint("free space in the remote home or choose a different install path"));
    }
    Ok(())
}

async fn best_effort_cleanup(
    transport: &dyn BootstrapTransport,
    install_path: &str,
    token: &str,
    wait: Duration,
) {
    let script = cleanup_script(install_path, token);
    let _ = transport
        .run(
            BootstrapPhase::Cleanup,
            &script,
            wait.min(Duration::from_secs(10)),
        )
        .await;
}

fn parse_error_output(output: &BootstrapCommandOutput) -> Option<RemoteError> {
    let source = output.stdout.lines().chain(output.stderr.lines());
    let first = source.clone().next()?;
    if first.trim() != ERROR_HEADER {
        return None;
    }
    let mut code = None;
    let mut message = String::new();
    for line in output
        .stdout
        .lines()
        .chain(output.stderr.lines())
        .skip_while(|line| line.trim() != ERROR_HEADER)
        .skip(1)
    {
        if let Some(value) = line.strip_prefix("code=") {
            code = RemoteErrorCode::parse(value.trim());
        } else if let Some(value) = line.strip_prefix("message=") {
            message = value.trim().to_string();
        }
    }
    let code = code.unwrap_or(RemoteErrorCode::BootstrapInstallFailed);
    if message.is_empty() {
        message = format!("remote bootstrap {} failed", output.phase.as_str());
    }
    Some(RemoteError::new(code, message))
}

pub fn parse_preflight(output: &BootstrapCommandOutput) -> Result<BootstrapPreflight, RemoteError> {
    let mut lines = output.stdout.lines();
    let header = lines.next().unwrap_or("").trim();
    if header != PREFLIGHT_HEADER {
        return Err(RemoteError::new(
            RemoteErrorCode::BootstrapInstallFailed,
            format!(
                "unexpected bootstrap preflight response: {}",
                short_output(output)
            ),
        ));
    }
    let mut values = std::collections::BTreeMap::new();
    for line in lines {
        if let Some((key, value)) = line.split_once('=') {
            values.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    let path = values.get("path").cloned().unwrap_or_default();
    if path.is_empty() {
        return Err(RemoteError::new(
            RemoteErrorCode::BootstrapInstallFailed,
            "preflight did not report the expanded install path",
        ));
    }
    let exists = matches!(values.get("exists").map(String::as_str), Some("1"));
    let existing_sha256 = values
        .get("existing_sha256")
        .filter(|value| !value.is_empty())
        .cloned();
    let existing_bytes = values
        .get("existing_bytes")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let free_bytes = values
        .get("free_bytes")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    Ok(BootstrapPreflight {
        path,
        remote_os: values.get("os").cloned().unwrap_or_default(),
        remote_arch: values.get("arch").cloned().unwrap_or_default(),
        exists,
        existing_sha256,
        existing_bytes,
        free_bytes,
    })
}

pub fn parse_finalize(
    output: &BootstrapCommandOutput,
) -> Result<BootstrapInstallation, RemoteError> {
    let mut lines = output.stdout.lines();
    let header = lines.next().unwrap_or("").trim();
    if header != FINALIZE_HEADER {
        return Err(RemoteError::new(
            RemoteErrorCode::BootstrapInstallFailed,
            format!(
                "unexpected bootstrap finalize response: {}",
                short_output(output)
            ),
        ));
    }
    let mut values = std::collections::BTreeMap::new();
    for line in lines {
        if let Some((key, value)) = line.split_once('=') {
            values.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    let path = values.get("path").cloned().unwrap_or_default();
    let sha256 = values.get("sha256").cloned().unwrap_or_default();
    let bytes = values
        .get("bytes")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let replaced = matches!(values.get("replaced").map(String::as_str), Some("1"));
    let protocol_version = values
        .get("protocol")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let helper_version = values.get("version").cloned().unwrap_or_default();
    let target_os = values.get("target_os").cloned().unwrap_or_default();
    let target_arch = values.get("target_arch").cloned().unwrap_or_default();
    let remote_os = values.get("os").cloned().unwrap_or_default();
    let remote_arch = values.get("arch").cloned().unwrap_or_default();
    if path.is_empty() || sha256.is_empty() || helper_version.is_empty() {
        return Err(RemoteError::new(
            RemoteErrorCode::BootstrapInstallFailed,
            "finalize response is missing the installed path, checksum, or version",
        ));
    }
    if target_os.is_empty() || target_arch.is_empty() {
        return Err(RemoteError::new(
            RemoteErrorCode::BootstrapInstallFailed,
            "finalize response is missing the helper target metadata",
        ));
    }
    Ok(BootstrapInstallation {
        path,
        sha256,
        bytes,
        replaced,
        helper_version,
        protocol_version,
        target_os,
        target_arch,
        remote_os,
        remote_arch,
    })
}

fn short_output(output: &BootstrapCommandOutput) -> String {
    let mut text = output.stdout.trim().to_string();
    if !output.stderr.trim().is_empty() {
        if !text.is_empty() {
            text.push_str(" | ");
        }
        text.push_str(output.stderr.trim());
    }
    const LIMIT: usize = 1_000;
    if text.chars().count() > LIMIT {
        let truncated = text.chars().take(LIMIT).collect::<String>();
        format!("{truncated}…")
    } else if text.is_empty() {
        "<no output>".to_string()
    } else {
        text
    }
}

pub fn helper_path_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("VERLINPARK_REMOTE_AGENT") {
        candidates.push(PathBuf::from(path));
    }
    if let Ok(current) = std::env::current_exe()
        && let Some(parent) = current.parent()
    {
        candidates.push(parent.join("vp-remote-agent"));
        candidates.push(parent.join("Helpers").join("vp-remote-agent"));
    }
    candidates
}

/// Read the helper that will be uploaded. Explicit paths are verified as
/// regular files; no PATH search happens silently.
pub fn read_helper_binary(path: &Path) -> Result<Vec<u8>, RemoteError> {
    let metadata = std::fs::metadata(path).map_err(|error| {
        RemoteError::new(
            RemoteErrorCode::BootstrapNotInstalled,
            format!("cannot read local helper {}: {error}", path.display()),
        )
        .with_hint("build the workspace or set VERLINPARK_REMOTE_AGENT to the helper binary")
    })?;
    if !metadata.is_file() {
        return Err(RemoteError::new(
            RemoteErrorCode::BootstrapNotInstalled,
            format!("local helper is not a regular file: {}", path.display()),
        ));
    }
    if metadata.len() > MAX_BOOTSTRAP_BYTES {
        return Err(RemoteError::new(
            RemoteErrorCode::BootstrapUnsupported,
            format!(
                "local helper is {} bytes, above the {} byte bootstrap cap",
                metadata.len(),
                MAX_BOOTSTRAP_BYTES
            ),
        ));
    }
    std::fs::read(path).map_err(|error| {
        RemoteError::new(
            RemoteErrorCode::BootstrapNotInstalled,
            format!("cannot read local helper {}: {error}", path.display()),
        )
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use super::*;

    const FAKE_SCRIPT: &str = "#!/bin/sh\nexit 0\n";
    const VERSION: &str = "0.1.0-dev";

    fn fake_macho() -> Vec<u8> {
        let mut bytes = vec![0u8; 4096];
        bytes[0..4].copy_from_slice(&0xfeedfacfu32.to_be_bytes());
        bytes[4..8].copy_from_slice(&0x0100_000cu32.to_le_bytes());
        bytes
    }

    fn fake_elf() -> Vec<u8> {
        let mut bytes = vec![0u8; 4096];
        bytes[0..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
        bytes[4] = 2;
        bytes[5] = 1;
        bytes[18..20].copy_from_slice(&0x3eu16.to_le_bytes());
        bytes
    }

    fn request() -> BootstrapRequest {
        BootstrapRequest::default()
    }

    fn command(phase: BootstrapPhase, stdout: &str) -> BootstrapCommandOutput {
        BootstrapCommandOutput {
            phase,
            stdout: stdout.to_string(),
            stderr: String::new(),
            exit_code: Some(0),
            duration_ms: 1,
            truncated: false,
        }
    }

    fn preflight_output(
        os: &str,
        arch: &str,
        exists: bool,
        sha: Option<&str>,
        free: u64,
    ) -> String {
        format!(
            "{PREFLIGHT_HEADER}\npath=/home/user/.verlinpark/bin/vp-remote-agent\nos={os}\narch={arch}\nexists={}\nexisting_sha256={}\nexisting_bytes={}\nfree_bytes={free}\n",
            if exists { 1 } else { 0 },
            sha.unwrap_or(""),
            if exists { 4096 } else { 0 },
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn finalize_output(
        sha: &str,
        bytes: u64,
        protocol: u32,
        version: &str,
        target_os: &str,
        target_arch: &str,
        os: &str,
        arch: &str,
        replaced: bool,
    ) -> String {
        format!(
            "{FINALIZE_HEADER}\npath=/home/user/.verlinpark/bin/vp-remote-agent\nsha256={sha}\nbytes={bytes}\nreplaced={}\nprotocol={protocol}\nversion={version}\ntarget_os={target_os}\ntarget_arch={target_arch}\nos={os}\narch={arch}\n",
            if replaced { 1 } else { 0 },
        )
    }

    #[derive(Default)]
    struct FakeTransport {
        preflight: Mutex<Option<Result<BootstrapCommandOutput, RemoteError>>>,
        upload: Mutex<Option<Result<BootstrapUploadOutput, RemoteError>>>,
        finalize: Mutex<Option<Result<BootstrapCommandOutput, RemoteError>>>,
        phases: Mutex<Vec<String>>,
        uploads: Mutex<Vec<Vec<u8>>>,
        uploaded_chunks: Mutex<Option<u32>>,
    }

    impl FakeTransport {
        fn new(preflight: BootstrapCommandOutput, finalize: BootstrapCommandOutput) -> Self {
            let transport = Self::default();
            *transport.preflight.lock().unwrap() = Some(Ok(preflight));
            *transport.finalize.lock().unwrap() = Some(Ok(finalize));
            *transport.upload.lock().unwrap() = Some(Ok(BootstrapUploadOutput {
                bytes_written: 0,
                chunks: 0,
                command: command(BootstrapPhase::Upload, UPLOAD_HEADER),
            }));
            transport
        }

        fn with_upload_error(self, error: RemoteError) -> Self {
            *self.upload.lock().unwrap() = Some(Err(error));
            self
        }

        fn phases(&self) -> Vec<String> {
            self.phases.lock().unwrap().clone()
        }

        fn upload_count(&self) -> usize {
            self.uploads.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl BootstrapTransport for FakeTransport {
        fn description(&self) -> String {
            "fake bootstrap host".to_string()
        }

        async fn run(
            &self,
            phase: BootstrapPhase,
            _script: &str,
            _wait: Duration,
        ) -> Result<BootstrapCommandOutput, RemoteError> {
            self.phases.lock().unwrap().push(phase.as_str().to_string());
            match phase {
                BootstrapPhase::Preflight => {
                    self.preflight.lock().unwrap().take().unwrap_or_else(|| {
                        Ok(command(
                            BootstrapPhase::Preflight,
                            &preflight_output("Linux", "x86_64", false, None, 1 << 30),
                        ))
                    })
                }
                BootstrapPhase::Finalize => {
                    self.finalize.lock().unwrap().take().unwrap_or_else(|| {
                        Ok(command(
                            BootstrapPhase::Finalize,
                            &finalize_output(
                                &sha256_hex(&fake_elf()),
                                fake_elf().len() as u64,
                                1,
                                VERSION,
                                "linux",
                                "x86_64",
                                "Linux",
                                "x86_64",
                                false,
                            ),
                        ))
                    })
                }
                BootstrapPhase::Cleanup => Ok(command(
                    BootstrapPhase::Cleanup,
                    &format!("{CLEANUP_HEADER}\n"),
                )),
                BootstrapPhase::Upload => unreachable!("uploads use upload()"),
            }
        }

        async fn upload(
            &self,
            _script: &str,
            payload: &[u8],
            chunk_bytes: u64,
            _wait: Duration,
        ) -> Result<BootstrapUploadOutput, RemoteError> {
            self.phases.lock().unwrap().push("upload".to_string());
            self.uploads.lock().unwrap().push(payload.to_vec());
            *self.uploaded_chunks.lock().unwrap() = Some(payload.len().div_ceil(
                chunk_bytes.clamp(MIN_BOOTSTRAP_CHUNK_BYTES, MAX_BOOTSTRAP_CHUNK_BYTES) as usize,
            ) as u32);
            self.upload.lock().unwrap().take().unwrap_or_else(|| {
                Ok(BootstrapUploadOutput {
                    bytes_written: payload.len() as u64,
                    chunks: 1,
                    command: command(BootstrapPhase::Upload, UPLOAD_HEADER),
                })
            })
        }
    }

    #[tokio::test]
    async fn successful_bootstrap_uploads_chunks_and_verifies_everything() {
        let helper = fake_elf();
        let transport = FakeTransport::new(
            command(
                BootstrapPhase::Preflight,
                &preflight_output("Linux", "x86_64", false, None, 1 << 30),
            ),
            command(
                BootstrapPhase::Finalize,
                &finalize_output(
                    &sha256_hex(&helper),
                    helper.len() as u64,
                    1,
                    VERSION,
                    "linux",
                    "x86_64",
                    "Linux",
                    "x86_64",
                    false,
                ),
            ),
        );
        let outcome = bootstrap_helper(&transport, request(), &helper)
            .await
            .unwrap();
        assert_eq!(outcome.sha256, sha256_hex(&helper));
        assert_eq!(outcome.helper_version, VERSION);
        assert_eq!(outcome.protocol_version, 1);
        assert_eq!(outcome.target_arch, "x86_64");
        assert!(!outcome.replaced);
        assert!(!outcome.used_sudo);
        assert_eq!(transport.phases(), vec!["preflight", "upload", "finalize"]);
        assert_eq!(transport.upload_count(), 1);
        assert_eq!(transport.uploads.lock().unwrap()[0], helper);
    }

    #[tokio::test]
    async fn architecture_mismatch_fails_before_upload() {
        let helper = fake_elf();
        let transport = FakeTransport::new(
            command(
                BootstrapPhase::Preflight,
                &preflight_output("Darwin", "arm64", false, None, 1 << 30),
            ),
            command(
                BootstrapPhase::Finalize,
                &finalize_output(
                    &sha256_hex(&helper),
                    helper.len() as u64,
                    1,
                    VERSION,
                    "linux",
                    "x86_64",
                    "Darwin",
                    "arm64",
                    false,
                ),
            ),
        );
        let error = bootstrap_helper(&transport, request(), &helper)
            .await
            .unwrap_err();
        assert_eq!(error.code, RemoteErrorCode::BootstrapArchMismatch);
        assert_eq!(transport.phases(), vec!["preflight"]);
        assert_eq!(transport.upload_count(), 0);
    }

    #[tokio::test]
    async fn pinned_checksum_mismatch_fails_before_any_remote_call() {
        let mut request = request();
        request.expected_sha256 = Some("0".repeat(64));
        let transport = FakeTransport::new(
            command(
                BootstrapPhase::Preflight,
                &preflight_output("Linux", "x86_64", false, None, 1 << 30),
            ),
            command(
                BootstrapPhase::Finalize,
                &finalize_output(
                    &sha256_hex(&fake_elf()),
                    fake_elf().len() as u64,
                    1,
                    VERSION,
                    "linux",
                    "x86_64",
                    "Linux",
                    "x86_64",
                    false,
                ),
            ),
        );
        let error = bootstrap_helper(&transport, request, &fake_elf())
            .await
            .unwrap_err();
        assert_eq!(error.code, RemoteErrorCode::BootstrapChecksumMismatch);
        assert!(transport.phases().is_empty());
    }

    #[tokio::test]
    async fn existing_helper_requires_force_and_nothing_is_uploaded() {
        let helper = fake_elf();
        let sha = sha256_hex(&helper);
        let transport = FakeTransport::new(
            command(
                BootstrapPhase::Preflight,
                &preflight_output("Linux", "x86_64", true, Some(&sha), 1 << 30),
            ),
            command(
                BootstrapPhase::Finalize,
                &finalize_output(
                    &sha,
                    helper.len() as u64,
                    1,
                    VERSION,
                    "linux",
                    "x86_64",
                    "Linux",
                    "x86_64",
                    true,
                ),
            ),
        );
        let error = bootstrap_helper(&transport, request(), &helper)
            .await
            .unwrap_err();
        assert_eq!(error.code, RemoteErrorCode::BootstrapExists);
        assert!(transport.phases() == vec!["preflight"]);
        assert_eq!(transport.upload_count(), 0);
    }

    #[tokio::test]
    async fn force_allows_replacement_and_reports_it() {
        let helper = fake_elf();
        let sha = sha256_hex(&helper);
        let transport = FakeTransport::new(
            command(
                BootstrapPhase::Preflight,
                &preflight_output(
                    "Linux",
                    "x86_64",
                    true,
                    Some("b".repeat(64).as_str()),
                    1 << 30,
                ),
            ),
            command(
                BootstrapPhase::Finalize,
                &finalize_output(
                    &sha,
                    helper.len() as u64,
                    1,
                    VERSION,
                    "linux",
                    "x86_64",
                    "Linux",
                    "x86_64",
                    true,
                ),
            ),
        );
        let mut request = request();
        request.force = true;
        let outcome = bootstrap_helper(&transport, request, &helper)
            .await
            .unwrap();
        assert!(outcome.replaced);
    }

    #[tokio::test]
    async fn insufficient_disk_fails_before_upload() {
        let helper = fake_elf();
        let transport = FakeTransport::new(
            command(
                BootstrapPhase::Preflight,
                &preflight_output("Linux", "x86_64", false, None, 512),
            ),
            command(BootstrapPhase::Finalize, ""),
        );
        let error = bootstrap_helper(&transport, request(), &helper)
            .await
            .unwrap_err();
        assert_eq!(error.code, RemoteErrorCode::BootstrapDisk);
        assert_eq!(transport.upload_count(), 0);
    }

    #[tokio::test]
    async fn finalize_version_and_protocol_mismatches_are_typed() {
        let helper = fake_elf();
        let sha = sha256_hex(&helper);
        let transport = FakeTransport::new(
            command(
                BootstrapPhase::Preflight,
                &preflight_output("Linux", "x86_64", false, None, 1 << 30),
            ),
            command(
                BootstrapPhase::Finalize,
                &finalize_output(
                    &sha,
                    helper.len() as u64,
                    2,
                    VERSION,
                    "linux",
                    "x86_64",
                    "Linux",
                    "x86_64",
                    false,
                ),
            ),
        );
        let error = bootstrap_helper(&transport, request(), &helper)
            .await
            .unwrap_err();
        assert_eq!(error.code, RemoteErrorCode::BootstrapProtocolMismatch);

        let transport = FakeTransport::new(
            command(
                BootstrapPhase::Preflight,
                &preflight_output("Linux", "x86_64", false, None, 1 << 30),
            ),
            command(
                BootstrapPhase::Finalize,
                &finalize_output(
                    &sha,
                    helper.len() as u64,
                    1,
                    "9.9.9",
                    "linux",
                    "x86_64",
                    "Linux",
                    "x86_64",
                    false,
                ),
            ),
        );
        let error = bootstrap_helper(&transport, request(), &helper)
            .await
            .unwrap_err();
        assert_eq!(error.code, RemoteErrorCode::BootstrapVersionMismatch);
    }

    #[tokio::test]
    async fn remote_error_lines_are_parsed_and_cleanup_runs_on_upload_failure() {
        let helper = fake_elf();
        let transport = FakeTransport::new(
            command(
                BootstrapPhase::Preflight,
                &preflight_output("Linux", "x86_64", false, None, 1 << 30),
            ),
            command(BootstrapPhase::Finalize, ""),
        )
        .with_upload_error(RemoteError::new(
            RemoteErrorCode::BootstrapUploadFailed,
            "connection lost",
        ));
        let error = bootstrap_helper(&transport, request(), &helper)
            .await
            .unwrap_err();
        assert_eq!(error.code, RemoteErrorCode::BootstrapUploadFailed);
        assert!(transport.phases().contains(&"cleanup".to_string()));
    }

    #[tokio::test]
    async fn finalize_error_line_wins_over_exit_code() {
        let helper = fake_elf();
        let mut output = command(
            BootstrapPhase::Finalize,
            &format!(
                "{ERROR_HEADER}\ncode=BOOTSTRAP_EXISTS\nmessage=helper already exists; pass force to replace it\n"
            ),
        );
        output.exit_code = Some(2);
        let transport = FakeTransport::new(
            command(
                BootstrapPhase::Preflight,
                &preflight_output("Linux", "x86_64", false, None, 1 << 30),
            ),
            output,
        );
        let error = bootstrap_helper(&transport, request(), &helper)
            .await
            .unwrap_err();
        assert_eq!(error.code, RemoteErrorCode::BootstrapExists);
    }

    #[test]
    fn mach_o_and_elf_headers_are_inspected() {
        assert_eq!(
            inspect_binary_target(&fake_macho()).unwrap(),
            BinaryTarget {
                os: "macos",
                arch: "arm64"
            }
        );
        assert_eq!(
            inspect_binary_target(&fake_elf()).unwrap(),
            BinaryTarget {
                os: "linux",
                arch: "x86_64"
            }
        );
        let mut elf_arm = fake_elf();
        elf_arm[18..20].copy_from_slice(&0xb7u16.to_le_bytes());
        assert_eq!(inspect_binary_target(&elf_arm).unwrap().arch, "arm64");
        assert_eq!(
            inspect_binary_target(FAKE_SCRIPT.as_bytes())
                .unwrap_err()
                .code,
            RemoteErrorCode::BootstrapUnsupported
        );
    }

    #[test]
    fn install_paths_are_restricted_to_user_owned_locations() {
        assert!(validate_install_path(DEFAULT_INSTALL_PATH).is_ok());
        assert!(validate_install_path("/home/user/.verlinpark/bin/vp-remote-agent").is_ok());
        assert!(validate_install_path("relative/path").is_err());
        assert!(validate_install_path("~/../escape").is_err());
        assert!(validate_install_path("~/bin/../escape").is_err());
        assert!(validate_install_path("~/bin/with space").is_err());
        assert!(validate_install_path("~/bin/$HOME").is_err());
        assert!(validate_install_path("~/").is_err());
        assert!(validate_install_path("~/bin/").is_err());
    }

    #[test]
    fn scripts_never_use_sudo_and_are_atomic() {
        let finalize = finalize_script(
            DEFAULT_INSTALL_PATH,
            "token",
            &"a".repeat(64),
            VERSION,
            1,
            false,
        );
        assert!(!finalize.contains("sudo"));
        assert!(!finalize.contains("umask"));
        assert!(finalize.contains("chmod 700"));
        assert!(finalize.contains("mv -f"));
        assert!(finalize.contains("BOOTSTRAP_EXISTS"));
        assert!(finalize.contains("BOOTSTRAP_CHECKSUM_MISMATCH"));
        assert!(finalize.contains("BOOTSTRAP_ARCH_MISMATCH"));
        assert!(finalize.contains("remote-agent --describe"));
        assert!(finalize.contains("\"$HOME\"/*"));
        let upload = upload_script(DEFAULT_INSTALL_PATH, "token");
        assert!(!upload.contains("sudo"));
        assert!(upload.contains("umask 077"));
        assert!(upload.contains("cat >"));
        let preflight = preflight_script(DEFAULT_INSTALL_PATH);
        assert!(!preflight.contains("sudo"));
        assert!(preflight.contains("sha256sum"));
        assert!(preflight.contains("free_bytes"));
    }

    #[test]
    fn finalize_script_rejects_checksum_version_protocol_and_exec_failures() {
        let dir = tempfile::TempDir::new().unwrap();
        let home = dir.path();
        let install = home.join("bin/vp-remote-agent");
        std::fs::create_dir_all(install.parent().unwrap()).unwrap();
        let temp = home.join("bin/vp-remote-agent.part-token");
        let run = |script: &str| {
            std::process::Command::new("/bin/sh")
                .args(["-c", script])
                .env("HOME", home)
                .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
                .output()
                .expect("run finalize")
        };
        let good_helper = "#!/bin/sh\nprintf '%s\\n' '{\"helper_version\":\"0.1.0-dev\",\"protocol_version\":1,\"target_os\":\"macos\",\"target_arch\":\"arm64\"}'\n";
        let good_sha = sha256_hex(good_helper.as_bytes());

        std::fs::write(&temp, good_helper).unwrap();
        let output = run(&finalize_script(
            "~/bin/vp-remote-agent",
            "token",
            &"b".repeat(64),
            VERSION,
            1,
            false,
        ));
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("BOOTSTRAP_CHECKSUM_MISMATCH"),
            "stdout={stdout}"
        );
        assert!(!install.exists());
        assert!(!temp.exists());

        std::fs::write(&temp, good_helper).unwrap();
        let output = run(&finalize_script(
            "~/bin/vp-remote-agent",
            "token",
            &good_sha,
            "9.9.9",
            1,
            false,
        ));
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("BOOTSTRAP_VERSION_MISMATCH"),
            "stdout={stdout}"
        );
        assert!(!install.exists());

        std::fs::write(&temp, good_helper).unwrap();
        let output = run(&finalize_script(
            "~/bin/vp-remote-agent",
            "token",
            &good_sha,
            VERSION,
            2,
            false,
        ));
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("BOOTSTRAP_PROTOCOL_MISMATCH"),
            "stdout={stdout}"
        );
        assert!(!install.exists());

        let broken_helper = "#!/bin/sh\nexit 9\n";
        let broken_sha = sha256_hex(broken_helper.as_bytes());
        std::fs::write(&temp, broken_helper).unwrap();
        let output = run(&finalize_script(
            "~/bin/vp-remote-agent",
            "token",
            &broken_sha,
            VERSION,
            1,
            false,
        ));
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("BOOTSTRAP_ARCH_MISMATCH"),
            "stdout={stdout}"
        );
        assert!(!install.exists());
    }

    #[test]
    fn upload_and_cleanup_scripts_expand_the_private_temporary_path() {
        let dir = tempfile::TempDir::new().unwrap();
        let home = dir.path();
        std::fs::create_dir_all(home.join("bin")).unwrap();
        let script = upload_script("~/bin/vp-remote-agent", "token");
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", &script])
            .env("HOME", home)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("run upload");
        {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(b"payload").unwrap();
        }
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        let temp = home.join("bin/vp-remote-agent.part-token");
        assert_eq!(std::fs::read(&temp).unwrap(), b"payload");
        assert!(String::from_utf8_lossy(&output.stdout).contains(UPLOAD_HEADER));
        let cleanup = cleanup_script("~/bin/vp-remote-agent", "token");
        let output = std::process::Command::new("/bin/sh")
            .args(["-c", &cleanup])
            .env("HOME", home)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(!temp.exists());
        assert!(String::from_utf8_lossy(&output.stdout).contains(CLEANUP_HEADER));
    }

    #[test]
    fn finalize_script_installs_atomically_and_never_replaces_without_force() {
        let dir = tempfile::TempDir::new().unwrap();
        let home = dir.path();
        let install = home.join("bin/vp-remote-agent");
        std::fs::create_dir_all(install.parent().unwrap()).unwrap();
        let fake_helper = "#!/bin/sh\nprintf '%s\\n' '{\"helper_version\":\"0.1.0-dev\",\"protocol_version\":1,\"target_os\":\"macos\",\"target_arch\":\"arm64\"}'\n";
        let temp = home.join("bin/vp-remote-agent.part-token");
        std::fs::write(&temp, fake_helper).unwrap();
        let sha = sha256_hex(fake_helper.as_bytes());
        let script = finalize_script("~/bin/vp-remote-agent", "token", &sha, VERSION, 1, false);
        let output = std::process::Command::new("/bin/sh")
            .args(["-c", &script])
            .env("HOME", home)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .output()
            .expect("run finalize");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "finalize failed: stdout={stdout} stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.starts_with(FINALIZE_HEADER), "stdout={stdout}");
        assert!(stdout.contains(&format!("sha256={sha}")));
        assert!(stdout.contains("version=0.1.0-dev"));
        assert!(stdout.contains("replaced=0"));
        assert_eq!(std::fs::read(&install).unwrap(), fake_helper.as_bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&install).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }

        std::fs::write(&temp, fake_helper).unwrap();
        let blocked = std::process::Command::new("/bin/sh")
            .args(["-c", &script])
            .env("HOME", home)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .output()
            .expect("run finalize without force");
        let blocked_stdout = String::from_utf8_lossy(&blocked.stdout);
        assert!(!blocked.status.success());
        assert!(blocked_stdout.contains(ERROR_HEADER));
        assert!(blocked_stdout.contains("BOOTSTRAP_EXISTS"));
        assert!(!temp.exists());

        std::fs::write(&temp, fake_helper).unwrap();
        let forced = finalize_script("~/bin/vp-remote-agent", "token", &sha, VERSION, 1, true);
        let output = std::process::Command::new("/bin/sh")
            .args(["-c", &forced])
            .env("HOME", home)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .output()
            .expect("run forced finalize");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "stdout={stdout}");
        assert!(stdout.contains("replaced=1"));
    }

    #[test]
    fn every_generated_script_is_valid_posix_shell() {
        let scripts = [
            preflight_script(DEFAULT_INSTALL_PATH),
            upload_script(DEFAULT_INSTALL_PATH, "token"),
            finalize_script(
                DEFAULT_INSTALL_PATH,
                "token",
                &"a".repeat(64),
                VERSION,
                1,
                true,
            ),
            cleanup_script(DEFAULT_INSTALL_PATH, "token"),
        ];
        for script in scripts {
            let status = std::process::Command::new("/bin/sh")
                .args(["-n", "-c", &script])
                .status()
                .expect("run sh -n");
            assert!(
                status.success(),
                "invalid generated script:
{script}"
            );
        }
    }

    #[test]
    fn preflight_script_runs_locally_with_a_private_home() {
        let dir = tempfile::TempDir::new().unwrap();
        let home = dir.path();
        let script = preflight_script("~/bin/vp-remote-agent");
        let output = std::process::Command::new("/bin/sh")
            .args(["-c", &script])
            .env("HOME", home)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .output()
            .expect("run preflight");
        assert!(
            output.status.success(),
            "preflight failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.starts_with(PREFLIGHT_HEADER),
            "stdout={stdout} stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.contains(&format!(
            "path={}",
            home.join("bin/vp-remote-agent").display()
        )));
        assert!(stdout.contains("exists=0"));
        assert!(stdout.contains("free_bytes="));
    }

    #[test]
    fn ssh_argv_has_no_sudo_and_quotes_the_script() {
        let config = SshBootstrapConfig {
            host: "build.example.com".into(),
            user: Some("agent".into()),
            port: Some(2222),
            identity_file: Some("/home/agent/.ssh/id_ed25519".into()),
            connect_timeout_secs: 12,
            ssh_args: vec![],
        };
        let argv = config.argv("printf 'hello world'\n").unwrap();
        assert_eq!(argv[0], "-T");
        assert!(argv.contains(&"BatchMode=yes".to_string()));
        assert!(!argv.contains(&"BuildMode=yes".to_string()));
        assert!(!argv.iter().any(|value| value.contains("sudo")));
        assert!(argv.last().unwrap().contains("sh -c"));
        assert!(argv.last().unwrap().contains("'printf"));
        let mut bad = config;
        bad.host = "example.com; touch /tmp/pwned".into();
        assert!(bad.argv("true").is_err());
    }

    #[test]
    fn line_protocol_parsers_are_bounded_and_reject_garbage() {
        let preflight = parse_preflight(&command(
            BootstrapPhase::Preflight,
            &preflight_output("Linux", "x86_64", true, Some(&"a".repeat(64)), 4096),
        ))
        .unwrap();
        assert_eq!(preflight.remote_os, "Linux");
        assert!(preflight.exists);
        assert_eq!(preflight.free_bytes, 4096);
        let bad = parse_preflight(&command(BootstrapPhase::Preflight, "garbage\n"));
        assert!(bad.is_err());
        let finalize = parse_finalize(&command(
            BootstrapPhase::Finalize,
            &finalize_output(
                &"b".repeat(64),
                4096,
                1,
                VERSION,
                "linux",
                "x86_64",
                "Linux",
                "x86_64",
                false,
            ),
        ))
        .unwrap();
        assert_eq!(finalize.protocol_version, 1);
        assert_eq!(finalize.helper_version, VERSION);
        assert!(parse_finalize(&command(BootstrapPhase::Finalize, "")).is_err());
    }

    #[tokio::test]
    async fn stdio_fake_host_runs_the_whole_chunked_sequence() {
        let dir = tempfile::TempDir::new().unwrap();
        let helper = fake_elf();
        let sha = sha256_hex(&helper);
        let payload_path = dir.path().join("payload.bin");
        let script_path = dir.path().join("fake-host.sh");
        let script = format!(
            "#!/bin/sh\nphase=\"$1\"\ncase \"$phase\" in\n\
             preflight) printf '%s\\n' '{PREFLIGHT_HEADER}' 'path=/home/user/.verlinpark/bin/vp-remote-agent' 'os=Linux' 'arch=x86_64' 'exists=0' 'existing_sha256=' 'existing_bytes=0' 'free_bytes=1073741824' ;;\n\
             upload) cat > '{}'; printf '%s\\n' '{UPLOAD_HEADER}' ;;\n\
             finalize) bytes=$(wc -c < '{}' | tr -d ' '); printf '%s\\n' '{FINALIZE_HEADER}' 'path=/home/user/.verlinpark/bin/vp-remote-agent' 'sha256={sha}' \"bytes=$bytes\" 'replaced=0' 'protocol=1' 'version={VERSION}' 'target_os=linux' 'target_arch=x86_64' 'os=Linux' 'arch=x86_64' ;;\n\
             cleanup) printf '%s\\n' '{CLEANUP_HEADER}' ;;\n\
             *) exit 9 ;;\n\
             esac\n",
            payload_path.display(),
            payload_path.display(),
        );
        std::fs::write(&script_path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let transport = StdioBootstrapTransport::new("/bin/sh").arg(script_path.to_string_lossy());
        let outcome = bootstrap_helper(&transport, request(), &helper)
            .await
            .unwrap();
        assert_eq!(outcome.sha256, sha);
        assert_eq!(outcome.bytes, helper.len() as u64);
        assert_eq!(std::fs::read(&payload_path).unwrap(), helper);
    }

    #[test]
    fn target_normalization_is_explicit() {
        assert_eq!(normalize_remote_os("Darwin"), Some("macos"));
        assert_eq!(normalize_remote_os("Linux"), Some("linux"));
        assert_eq!(normalize_remote_os("FreeBSD"), None);
        assert_eq!(normalize_remote_arch("aarch64"), Some("arm64"));
        assert_eq!(normalize_remote_arch("x86_64"), Some("x86_64"));
        assert_eq!(normalize_remote_arch("riscv64"), None);
        let mut map: BTreeMap<&str, &str> = BTreeMap::new();
        map.insert("os", "linux");
        assert_eq!(map.get("os"), Some(&"linux"));
    }
}

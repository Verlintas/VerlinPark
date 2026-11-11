//! Child-process transports.
//!
//! `StdioTransport` launches a helper directly. `SshTransport` launches the
//! system `ssh` client with an explicit argument vector; it never invokes a
//! local shell. OpenSSH still interprets the remote command with the remote
//! user's shell, so every remote argument is single-quoted for that shell.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::timeout;

use crate::protocol::{RemoteError, RemoteErrorCode};

const STDERR_TAIL_BYTES: usize = 8 * 1024;

#[async_trait]
pub trait RemoteTransport: Send + Sync {
    fn description(&self) -> String;
    async fn spawn(&self) -> Result<RemoteProcess, RemoteError>;
}

pub struct RemoteProcess {
    pub stdin: Option<ChildStdin>,
    pub stdout: Option<ChildStdout>,
    pub child: Arc<AsyncMutex<Option<Child>>>,
    pub stderr_tail: Arc<Mutex<String>>,
    pub description: String,
}

impl RemoteProcess {
    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.stdin.take()
    }

    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.stdout.take()
    }

    pub fn stderr_tail(&self) -> String {
        self.stderr_tail
            .lock()
            .map(|tail| tail.clone())
            .unwrap_or_default()
    }

    pub async fn shutdown(&self, wait: Duration) {
        let mut guard = self.child.lock().await;
        let Some(mut child) = guard.take() else {
            return;
        };
        match timeout(wait, child.wait()).await {
            Ok(_) => {}
            Err(_) => {
                let _ = child.start_kill();
                let _ = child.wait().await;
            }
        }
    }

    pub async fn kill(&self) {
        let mut guard = self.child.lock().await;
        if let Some(child) = guard.as_mut() {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
    }
}

impl Drop for RemoteProcess {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.child.try_lock()
            && let Some(child) = guard.as_mut()
        {
            let _ = child.start_kill();
        }
    }
}

async fn spawn_command(
    mut command: Command,
    description: String,
) -> Result<RemoteProcess, RemoteError> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|error| {
        RemoteError::new(
            RemoteErrorCode::TransportClosed,
            format!("cannot start {description}: {error}"),
        )
        .with_hint("install the helper or correct the configured program path")
    })?;
    let stdin = child.stdin.take();
    let stdout = child.stdout.take().ok_or_else(|| {
        RemoteError::new(
            RemoteErrorCode::TransportClosed,
            format!("{description} did not expose stdout"),
        )
    })?;
    let stderr = child.stderr.take();
    let stderr_tail = Arc::new(Mutex::new(String::new()));
    if let Some(mut stderr) = stderr {
        let tail = stderr_tail.clone();
        tokio::spawn(async move {
            let mut buffer = [0u8; 4096];
            loop {
                match stderr.read(&mut buffer).await {
                    Ok(0) => break,
                    Ok(count) => {
                        if let Ok(mut tail) = tail.lock() {
                            tail.push_str(&String::from_utf8_lossy(&buffer[..count]));
                            if tail.len() > STDERR_TAIL_BYTES {
                                let drain = tail.len() - STDERR_TAIL_BYTES;
                                let boundary = tail
                                    .char_indices()
                                    .map(|(index, _)| index)
                                    .find(|index| *index >= drain)
                                    .unwrap_or(drain);
                                tail.drain(..boundary);
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    }
    Ok(RemoteProcess {
        stdin,
        stdout: Some(stdout),
        child: Arc::new(AsyncMutex::new(Some(child))),
        stderr_tail,
        description,
    })
}

fn base_child_env(command: &mut Command, include_ssh_agent: bool) {
    command.env_clear();
    for name in [
        "PATH", "HOME", "USER", "LOGNAME", "LANG", "LC_ALL", "LC_CTYPE", "TERM", "TMPDIR",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    if include_ssh_agent {
        for name in ["SSH_AUTH_SOCK", "SSH_AGENT_PID"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct StdioTransport {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: Option<PathBuf>,
    pub description: String,
}

impl StdioTransport {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        let program = program.into();
        let description = program.display().to_string();
        Self {
            program,
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
            description,
        }
    }

    pub fn arg(mut self, value: impl Into<String>) -> Self {
        self.args.push(value.into());
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn described_as(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }
}

#[async_trait]
impl RemoteTransport for StdioTransport {
    fn description(&self) -> String {
        self.description.clone()
    }

    async fn spawn(&self) -> Result<RemoteProcess, RemoteError> {
        if self.program.as_os_str().is_empty() {
            return Err(RemoteError::new(
                RemoteErrorCode::InvalidParams,
                "helper program path must not be empty",
            ));
        }
        let mut command = Command::new(&self.program);
        command.args(&self.args);
        if let Some(cwd) = self.cwd.as_ref() {
            command.current_dir(cwd);
        }
        base_child_env(&mut command, false);
        for (key, value) in &self.env {
            if key.is_empty() || key.contains('=') || key.contains('\0') {
                return Err(RemoteError::new(
                    RemoteErrorCode::InvalidParams,
                    format!("invalid helper environment key: {key:?}"),
                ));
            }
            if value.contains('\0') {
                return Err(RemoteError::new(
                    RemoteErrorCode::InvalidParams,
                    format!("helper environment value for {key} contains NUL"),
                ));
            }
            command.env(key, value);
        }
        spawn_command(command, self.description()).await
    }
}

/// SSH transport configuration. The daemon's `ExecutionHost` is mapped into
/// this type; keeping it here makes the argv construction directly testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshConfig {
    pub host: String,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity_file: Option<String>,
    pub helper_path: String,
    pub root: String,
    pub token_env: Option<String>,
    pub token_file: Option<String>,
    pub ledger_path: Option<String>,
    pub env_allow: Vec<String>,
    pub connect_timeout_secs: u64,
    pub ssh_args: Vec<String>,
    pub max_frame_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct SshTransport {
    pub config: SshConfig,
}

impl SshTransport {
    pub fn new(config: SshConfig) -> Self {
        Self { config }
    }

    pub fn validate(config: &SshConfig) -> Result<(), RemoteError> {
        validate_ssh_host(&config.host)?;
        if let Some(user) = config.user.as_deref() {
            validate_ssh_user(user)?;
        }
        if config.port == Some(0) {
            return Err(RemoteError::new(
                RemoteErrorCode::InvalidParams,
                "ssh port must be between 1 and 65535",
            ));
        }
        if config.root.is_empty() || !config.root.starts_with('/') {
            return Err(RemoteError::new(
                RemoteErrorCode::InvalidParams,
                "remote root must be an absolute path",
            ));
        }
        if config.root.contains('\0') || config.root.contains('\n') {
            return Err(RemoteError::new(
                RemoteErrorCode::InvalidParams,
                "remote root must not contain NUL or newline characters",
            ));
        }
        if config.helper_path.trim().is_empty() {
            return Err(RemoteError::new(
                RemoteErrorCode::InvalidParams,
                "remote helper path must not be empty",
            ));
        }
        if config.helper_path.contains('\0') || config.helper_path.contains('\n') {
            return Err(RemoteError::new(
                RemoteErrorCode::InvalidParams,
                "remote helper path must not contain NUL or newline characters",
            ));
        }
        if config.token_env.is_none() && config.token_file.is_none() {
            return Err(RemoteError::new(
                RemoteErrorCode::InvalidParams,
                "ssh execution requires token_env or token_file so the helper can authenticate",
            ));
        }
        if let Some(name) = config.token_env.as_deref() {
            validate_env_name(name)?;
        }
        if let Some(path) = config.token_file.as_deref() {
            validate_remote_path(path, "token_file")?;
        }
        if let Some(path) = config.ledger_path.as_deref() {
            validate_remote_path(path, "ledger_path")?;
        }
        if config.ssh_args.len() > 32 {
            return Err(RemoteError::new(
                RemoteErrorCode::InvalidParams,
                "at most 32 extra ssh arguments are allowed",
            ));
        }
        for argument in &config.ssh_args {
            if argument.is_empty() || argument.len() > 512 {
                return Err(RemoteError::new(
                    RemoteErrorCode::InvalidParams,
                    "each extra ssh argument must be 1..=512 bytes",
                ));
            }
            if argument.contains('\0') || argument.contains('\n') || argument.contains('\r') {
                return Err(RemoteError::new(
                    RemoteErrorCode::InvalidParams,
                    "extra ssh arguments must not contain control characters",
                ));
            }
        }
        if config.connect_timeout_secs == 0 || config.connect_timeout_secs > 300 {
            return Err(RemoteError::new(
                RemoteErrorCode::InvalidParams,
                "connect timeout must be 1..=300 seconds",
            ));
        }
        Ok(())
    }

    /// Build the exact local argv passed to `ssh`. This never invokes a local
    /// shell and every remote-side argument is quoted for the remote shell.
    pub fn build_argv(config: &SshConfig) -> Result<Vec<String>, RemoteError> {
        Self::validate(config)?;
        let mut args = vec![
            "-T".to_string(),
            "-o".to_string(),
            "BatchMode=yes".to_string(),
            "-o".to_string(),
            format!(
                "ConnectTimeout={}",
                config.connect_timeout_secs.clamp(1, 300)
            ),
        ];
        if let Some(port) = config.port {
            args.push("-p".to_string());
            args.push(port.to_string());
        }
        if let Some(identity) = config.identity_file.as_deref() {
            args.push("-i".to_string());
            args.push(identity.to_string());
        }
        args.extend(config.ssh_args.iter().cloned());
        if let Some(user) = config.user.as_deref() {
            args.push("-l".to_string());
            args.push(user.to_string());
        }
        args.push(config.host.clone());

        let mut remote = vec![
            config.helper_path.clone(),
            "remote-agent".to_string(),
            "--root".to_string(),
            config.root.clone(),
            "--max-frame-bytes".to_string(),
            config.max_frame_bytes.to_string(),
        ];
        if let Some(name) = config.token_env.as_deref() {
            remote.push("--token-env".to_string());
            remote.push(name.to_string());
        }
        if let Some(path) = config.token_file.as_deref() {
            remote.push("--token-file".to_string());
            remote.push(path.to_string());
        }
        if let Some(path) = config.ledger_path.as_deref() {
            remote.push("--ledger".to_string());
            remote.push(path.to_string());
        }
        if !config.env_allow.is_empty() {
            remote.push("--env-allow".to_string());
            remote.push(config.env_allow.join(","));
        }
        args.push(
            remote
                .iter()
                .map(|value| shell_quote(value))
                .collect::<Vec<_>>()
                .join(" "),
        );
        Ok(args)
    }
}

#[async_trait]
impl RemoteTransport for SshTransport {
    fn description(&self) -> String {
        format!(
            "ssh {}:{}",
            self.config.user.as_deref().unwrap_or("user"),
            self.config.host
        )
    }

    async fn spawn(&self) -> Result<RemoteProcess, RemoteError> {
        let argv = Self::build_argv(&self.config)?;
        let mut command = Command::new("ssh");
        command.args(&argv);
        base_child_env(&mut command, true);
        spawn_command(command, self.description()).await
    }
}

pub fn validate_ssh_host(host: &str) -> Result<(), RemoteError> {
    if host.is_empty() || host.len() > 255 {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            "ssh host must be 1..=255 characters",
        ));
    }
    if host.starts_with('-') {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            "ssh host must not begin with '-'",
        ));
    }
    if host.chars().any(|character| {
        character.is_whitespace()
            || character.is_control()
            || matches!(character, ';' | '&' | '|' | '`' | '$' | '\'' | '"' | '\\')
    }) {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            "ssh host contains characters that cannot be passed safely",
        ));
    }
    Ok(())
}

pub fn validate_ssh_user(user: &str) -> Result<(), RemoteError> {
    if user.is_empty() || user.len() > 128 {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            "ssh user must be 1..=128 characters",
        ));
    }
    if user.starts_with('-')
        || user
            .chars()
            .any(|character| !(character.is_ascii_alphanumeric() || "._-".contains(character)))
    {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            "ssh user may contain only ASCII letters, digits, '.', '_', and '-'",
        ));
    }
    Ok(())
}

pub fn validate_env_name(name: &str) -> Result<(), RemoteError> {
    if name.is_empty() || name.len() > 128 {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            "environment variable name must be 1..=128 characters",
        ));
    }
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            "environment variable name must not be empty",
        ));
    };
    if !(first.is_ascii_alphabetic() || first == '_')
        || !characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            format!("invalid environment variable name: {name}"),
        ));
    }
    Ok(())
}

fn validate_remote_path(path: &str, label: &str) -> Result<(), RemoteError> {
    if path.is_empty() || !path.starts_with('/') {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            format!("{label} must be an absolute path on the remote host"),
        ));
    }
    if path.contains('\0') || path.contains('\n') || path.contains('\r') {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            format!("{label} must not contain control characters"),
        ));
    }
    Ok(())
}

/// Quote one argument for a POSIX remote shell. Values made only of safe
/// characters are left alone; everything else is single-quoted with the
/// standard `'\''` escape. This is the only place a remote command string is
/// constructed.
pub fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._/:=@+-".contains(character))
    {
        return value.to_string();
    }
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for character in value.chars() {
        if character == '\'' {
            quoted.push_str("'\"'\"'");
        } else {
            quoted.push(character);
        }
    }
    quoted.push('\'');
    quoted
}

pub fn which_helper() -> PathBuf {
    if let Some(path) = std::env::var_os("VERLINPARK_REMOTE_AGENT") {
        return PathBuf::from(path);
    }
    if let Ok(current) = std::env::current_exe()
        && let Some(parent) = current.parent()
    {
        let sibling = parent.join("vp-remote-agent");
        if sibling.is_file() {
            return sibling;
        }
        let helper = parent.join("Helpers").join("vp-remote-agent");
        if helper.is_file() {
            return helper;
        }
    }
    PathBuf::from("vp-remote-agent")
}

pub fn resolve_ssh_identity(path: &str) -> Option<PathBuf> {
    let expanded = if let Some(rest) = path.strip_prefix("~/") {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join(rest))
    } else {
        Some(PathBuf::from(path))
    }?;
    if expanded.is_file() {
        Some(expanded)
    } else {
        None
    }
}

pub fn assert_identity_exists(path: &Path) -> Result<(), RemoteError> {
    if path.is_file() {
        Ok(())
    } else {
        Err(RemoteError::new(
            RemoteErrorCode::PathNotFound,
            format!("ssh identity file does not exist: {}", path.display()),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> SshConfig {
        SshConfig {
            host: "build.example.com".into(),
            user: Some("agent".into()),
            port: Some(2222),
            identity_file: Some("/home/agent/.ssh/id_ed25519".into()),
            helper_path: "/usr/local/bin/vp-remote-agent".into(),
            root: "/srv/project".into(),
            token_env: Some("VERLINPARK_REMOTE_TOKEN".into()),
            token_file: None,
            ledger_path: Some("/home/agent/.verlinpark/ledger.jsonl".into()),
            env_allow: vec!["CI".into()],
            connect_timeout_secs: 12,
            ssh_args: vec!["-o".into(), "StrictHostKeyChecking=yes".into()],
            max_frame_bytes: 16 * 1024 * 1024,
        }
    }

    #[test]
    fn ssh_argv_is_structured_and_shell_quotes_only_the_remote_command() {
        let argv = SshTransport::build_argv(&config()).unwrap();
        assert_eq!(argv[0], "-T");
        assert!(argv.contains(&"BatchMode=yes".to_string()));
        assert!(argv.contains(&"ConnectTimeout=12".to_string()));
        assert!(argv.contains(&"2222".to_string()));
        assert!(argv.contains(&"agent".to_string()));
        assert!(argv.contains(&"build.example.com".to_string()));
        let remote = argv.last().unwrap();
        assert!(remote.contains("vp-remote-agent remote-agent"));
        assert!(remote.contains("--root /srv/project"));
        assert!(remote.contains("--token-env VERLINPARK_REMOTE_TOKEN"));
        assert!(remote.contains("--ledger /home/agent/.verlinpark/ledger.jsonl"));
    }

    #[test]
    fn ssh_argv_quotes_paths_with_spaces() {
        let mut config = config();
        config.root = "/srv/my project".into();
        config.helper_path = "/opt/Verlin Park/vp-remote-agent".into();
        config.token_file = Some("/home/agent/.config/vp token".into());
        config.token_env = None;
        let argv = SshTransport::build_argv(&config).unwrap();
        let remote = argv.last().unwrap();
        assert!(remote.contains("'/srv/my project'"));
        assert!(remote.contains("'/opt/Verlin Park/vp-remote-agent'"));
        assert!(remote.contains("'/home/agent/.config/vp token'"));
    }

    #[test]
    fn ssh_host_injection_is_rejected() {
        let mut config = config();
        config.host = "example.com; touch /tmp/pwned".into();
        let error = SshTransport::build_argv(&config).unwrap_err();
        assert_eq!(error.code, RemoteErrorCode::InvalidParams);
        config.host = "-oProxyCommand=evil".into();
        assert!(SshTransport::build_argv(&config).is_err());
    }

    #[test]
    fn ssh_user_injection_is_rejected() {
        let mut config = config();
        config.user = Some("agent;id".into());
        assert!(SshTransport::build_argv(&config).is_err());
    }

    #[test]
    fn ssh_requires_a_token_source() {
        let mut config = config();
        config.token_env = None;
        config.token_file = None;
        let error = SshTransport::build_argv(&config).unwrap_err();
        assert_eq!(error.code, RemoteErrorCode::InvalidParams);
    }

    #[test]
    fn shell_quote_handles_apostrophes() {
        assert_eq!(shell_quote("plain"), "plain");
        assert_eq!(shell_quote("with space"), "'with space'");
        assert_eq!(shell_quote("it's"), "'it'\"'\"'s'");
        assert_eq!(shell_quote("$HOME;rm"), "'$HOME;rm'");
    }

    #[test]
    fn remote_root_must_be_absolute() {
        let mut config = config();
        config.root = "relative/path".into();
        assert!(SshTransport::build_argv(&config).is_err());
    }

    #[test]
    fn extra_ssh_arguments_are_bounded() {
        let mut config = config();
        config.ssh_args = (0..40).map(|index| format!("-arg{index}")).collect();
        assert!(SshTransport::build_argv(&config).is_err());
    }

    #[test]
    fn ipv6_host_and_user_are_accepted() {
        let mut config = config();
        config.host = "2001:db8::1".into();
        config.user = Some("deploy_user-1".into());
        assert!(SshTransport::build_argv(&config).is_ok());
    }

    #[test]
    fn identity_and_remote_paths_are_quoted() {
        let mut config = config();
        config.identity_file = Some("/home/agent/.ssh/id with space".into());
        config.ledger_path = Some("/home/agent/.verlinpark/led ger.jsonl".into());
        let argv = SshTransport::build_argv(&config).unwrap();
        assert!(argv.contains(&"/home/agent/.ssh/id with space".to_string()));
        let remote = argv.last().unwrap();
        assert!(remote.contains("'/home/agent/.verlinpark/led ger.jsonl'"));
    }

    #[test]
    fn token_environment_names_are_validated() {
        assert!(validate_env_name("VERLINPARK_REMOTE_TOKEN").is_ok());
        assert!(validate_env_name("_TOKEN").is_ok());
        assert!(validate_env_name("TOKEN-WITH-DASH").is_err());
        assert!(validate_env_name("1TOKEN").is_err());
        assert!(validate_env_name("").is_err());
        let mut config = config();
        config.token_env = Some("BAD-NAME".into());
        assert!(SshTransport::build_argv(&config).is_err());
    }

    #[test]
    fn connect_timeout_and_remote_paths_are_bounded() {
        let mut cfg = config();
        cfg.connect_timeout_secs = 0;
        assert!(SshTransport::build_argv(&cfg).is_err());
        cfg = config();
        cfg.token_file = Some("relative/path".into());
        assert!(SshTransport::build_argv(&cfg).is_err());
        cfg = config();
        cfg.ledger_path = Some("/tmp/line\nbreak".into());
        assert!(SshTransport::build_argv(&cfg).is_err());
        cfg = config();
        cfg.helper_path = "/tmp/line\nbreak".into();
        assert!(SshTransport::build_argv(&cfg).is_err());
    }

    #[test]
    fn ssh_argv_uses_batch_mode_and_no_pseudo_tty() {
        let argv = SshTransport::build_argv(&config()).unwrap();
        assert_eq!(argv[0], "-T");
        assert!(argv.iter().any(|value| value == "BatchMode=yes"));
        assert!(!argv.iter().any(|value| value.contains("ProxyCommand")));
    }

    #[test]
    fn stdio_transport_reports_missing_program() {
        let transport = StdioTransport::new("/definitely/not/a/helper");
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let error = match runtime.block_on(transport.spawn()) {
            Ok(_) => panic!("missing helper unexpectedly spawned"),
            Err(error) => error,
        };
        assert_eq!(error.code, RemoteErrorCode::TransportClosed);
    }
}

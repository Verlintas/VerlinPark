//! Project execution-host configuration and remote-workspace RPC DTOs.
//!
//! The default remains `local`, so existing projects and old protocol clients
//! are unchanged. Remote hosts are explicit, project-scoped configuration; the
//! daemon never reroutes an existing local tool because a host exists.

use serde::{Deserialize, Serialize};

pub const DEFAULT_HELPER_PATH: &str = "vp-remote-agent";
pub const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 15;
pub const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 120;
pub const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 300;
pub const DEFAULT_MAX_CONCURRENCY: u32 = 4;
pub const MAX_REMOTE_CONCURRENCY: u32 = 32;
pub const MAX_SSH_ARGS: usize = 32;

fn default_helper_path() -> String {
    DEFAULT_HELPER_PATH.to_string()
}

fn default_connect_timeout() -> u64 {
    DEFAULT_CONNECT_TIMEOUT_SECS
}

fn default_request_timeout() -> u64 {
    DEFAULT_REQUEST_TIMEOUT_SECS
}

fn default_idle_timeout() -> u64 {
    DEFAULT_IDLE_TIMEOUT_SECS
}

fn default_max_concurrency() -> u32 {
    DEFAULT_MAX_CONCURRENCY
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionHostMode {
    #[default]
    Local,
    Ssh,
}

impl ExecutionHostMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Ssh => "ssh",
        }
    }
}

/// Project-scoped execution host. Secrets are never stored here: `token_env`
/// names an environment variable and `token_file` is a remote path, so the
/// daemon database and project manifest contain no bearer token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionHost {
    #[serde(default)]
    pub mode: ExecutionHostMode,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub identity_file: Option<String>,
    /// Absolute canonical root on the remote host. File operations are jailed
    /// to this directory.
    #[serde(default)]
    pub root: String,
    #[serde(default = "default_helper_path")]
    pub helper_path: String,
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout_secs: u64,
    #[serde(default = "default_request_timeout")]
    pub request_timeout_secs: u64,
    /// Reconnect after this much idle time even if the helper still looks
    /// healthy. Zero keeps a connection until it fails or is invalidated.
    #[serde(default = "default_idle_timeout")]
    pub idle_timeout_secs: u64,
    #[serde(default = "default_max_concurrency")]
    pub max_concurrency: u32,
    #[serde(default)]
    pub ssh_args: Vec<String>,
    #[serde(default)]
    pub token_env: Option<String>,
    #[serde(default)]
    pub token_file: Option<String>,
    /// Optional append-only helper ledger path on the remote host. When empty,
    /// the helper uses its own default (or in-memory with `--no-ledger`).
    #[serde(default)]
    pub ledger_path: Option<String>,
    /// Extra environment names the remote exec helper may accept. This only
    /// widens the helper's allowlist; dangerous loader variables stay denied.
    #[serde(default)]
    pub env_allow: Vec<String>,
}

impl Default for ExecutionHost {
    fn default() -> Self {
        Self {
            mode: ExecutionHostMode::Local,
            host: String::new(),
            user: None,
            port: None,
            identity_file: None,
            root: String::new(),
            helper_path: default_helper_path(),
            connect_timeout_secs: DEFAULT_CONNECT_TIMEOUT_SECS,
            request_timeout_secs: DEFAULT_REQUEST_TIMEOUT_SECS,
            idle_timeout_secs: DEFAULT_IDLE_TIMEOUT_SECS,
            max_concurrency: DEFAULT_MAX_CONCURRENCY,
            ssh_args: Vec::new(),
            token_env: None,
            token_file: None,
            ledger_path: None,
            env_allow: Vec::new(),
        }
    }
}

impl ExecutionHost {
    pub fn is_remote(&self) -> bool {
        self.mode == ExecutionHostMode::Ssh
    }

    pub fn label(&self) -> String {
        if !self.is_remote() {
            return "local".to_string();
        }
        let user = self.user.as_deref().unwrap_or("user");
        format!("{user}@{}:{}", self.host, self.root)
    }

    /// Validate configuration before it is stored or used. Keeping this in the
    /// protocol crate makes the RPC and the store reject the same bounds.
    pub fn validate(&self) -> Result<(), String> {
        match self.mode {
            ExecutionHostMode::Local => {
                if !self.root.trim().is_empty() {
                    return Err("local execution hosts must not set a remote root".to_string());
                }
                if !self.host.trim().is_empty() {
                    return Err("local execution hosts must not set an ssh host".to_string());
                }
                return Ok(());
            }
            ExecutionHostMode::Ssh => {}
        }
        if self.host.trim().is_empty() {
            return Err("ssh execution host requires a host".to_string());
        }
        if self.host.len() > 255 {
            return Err("ssh host exceeds 255 characters".to_string());
        }
        if self.host.starts_with('-')
            || self.host.chars().any(|character| {
                character.is_whitespace()
                    || character.is_control()
                    || matches!(character, ';' | '&' | '|' | '`' | '$' | '\'' | '"' | '\\')
            })
        {
            return Err("ssh host contains characters that cannot be passed safely".to_string());
        }
        if let Some(user) = self.user.as_deref() {
            if user.is_empty() || user.len() > 128 {
                return Err("ssh user must be 1..=128 characters".to_string());
            }
            if user.starts_with('-')
                || user.chars().any(|character| {
                    !(character.is_ascii_alphanumeric() || "._-".contains(character))
                })
            {
                return Err(
                    "ssh user may contain only ASCII letters, digits, '.', '_', and '-'"
                        .to_string(),
                );
            }
        }
        if self.port == Some(0) {
            return Err("ssh port must be between 1 and 65535".to_string());
        }
        if self.root.is_empty() || !self.root.starts_with('/') {
            return Err("remote root must be an absolute path".to_string());
        }
        if self.root.chars().count() > 4096 {
            return Err("remote root exceeds 4096 characters".to_string());
        }
        if self.root.contains('\0') || self.root.contains('\n') || self.root.contains('\r') {
            return Err("remote root must not contain control characters".to_string());
        }
        if self.root.split('/').any(|component| component == "..") {
            return Err("remote root must not contain '..' components".to_string());
        }
        if self.helper_path.trim().is_empty() {
            return Err("remote helper path must not be empty".to_string());
        }
        if self.helper_path.len() > 1024
            || self.helper_path.contains('\0')
            || self.helper_path.contains('\n')
        {
            return Err(
                "remote helper path must be at most 1024 bytes without control characters"
                    .to_string(),
            );
        }
        if !(1..=300).contains(&self.connect_timeout_secs) {
            return Err("connect timeout must be 1..=300 seconds".to_string());
        }
        if !(1..=3600).contains(&self.request_timeout_secs) {
            return Err("request timeout must be 1..=3600 seconds".to_string());
        }
        if self.idle_timeout_secs > 86_400 {
            return Err("idle timeout must be 0..=86400 seconds".to_string());
        }
        if !(1..=MAX_REMOTE_CONCURRENCY).contains(&self.max_concurrency) {
            return Err(format!(
                "remote max_concurrency must be 1..={MAX_REMOTE_CONCURRENCY}"
            ));
        }
        if self.ssh_args.len() > MAX_SSH_ARGS {
            return Err(format!(
                "at most {MAX_SSH_ARGS} extra ssh arguments are allowed"
            ));
        }
        for argument in &self.ssh_args {
            if argument.is_empty()
                || argument.len() > 512
                || argument.contains('\0')
                || argument.contains('\n')
                || argument.contains('\r')
            {
                return Err(
                    "each extra ssh argument must be 1..=512 bytes without control characters"
                        .to_string(),
                );
            }
        }
        if let Some(path) = self.identity_file.as_deref()
            && (path.is_empty() || path.len() > 4096 || path.contains('\0') || path.contains('\n'))
        {
            return Err(
                "identity_file must be a non-empty path without control characters".to_string(),
            );
        }
        if self.token_env.is_none() {
            return Err(
                "ssh execution requires token_env so the daemon can read the auth token; token_file only tells the remote helper where its copy lives"
                    .to_string(),
            );
        }
        if let Some(name) = self.token_env.as_deref() {
            let mut characters = name.chars();
            let Some(first) = characters.next() else {
                return Err("token_env must not be empty".to_string());
            };
            if !(first.is_ascii_alphabetic() || first == '_')
                || !characters
                    .all(|character| character.is_ascii_alphanumeric() || character == '_')
            {
                return Err("token_env is not a valid environment variable name".to_string());
            }
        }
        if let Some(path) = self.token_file.as_deref()
            && (!path.starts_with('/') || path.contains('\0') || path.contains('\n'))
        {
            return Err("token_file must be an absolute remote path".to_string());
        }
        if let Some(path) = self.ledger_path.as_deref()
            && (!path.starts_with('/') || path.contains('\0') || path.contains('\n'))
        {
            return Err("ledger_path must be an absolute remote path".to_string());
        }
        if self.env_allow.len() > 64 {
            return Err("at most 64 extra environment names are allowed".to_string());
        }
        for name in &self.env_allow {
            let mut characters = name.chars();
            let Some(first) = characters.next() else {
                return Err("env_allow names must not be empty".to_string());
            };
            if name.len() > 128
                || !(first.is_ascii_alphabetic() || first == '_')
                || !characters
                    .all(|character| character.is_ascii_alphanumeric() || character == '_')
            {
                return Err(format!("invalid env_allow name: {name}"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectExecutionHost {
    pub project_id: String,
    pub config: ExecutionHost,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HostListParams {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostListResult {
    pub hosts: Vec<ProjectExecutionHost>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostSetParams {
    pub project_id: String,
    pub config: ExecutionHost,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostProjectParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostProbeParams {
    pub project_id: String,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RemoteStatusParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteStatusResult {
    pub project_id: String,
    pub mode: ExecutionHostMode,
    pub configured: bool,
    pub connected: bool,
    pub root: String,
    pub host: Option<String>,
    pub helper_path: String,
    pub capabilities: Vec<String>,
    pub helper_version: Option<String>,
    pub last_error: Option<String>,
    pub last_connected_at: Option<String>,
    pub inflight: u32,
    pub max_concurrency: u32,
    pub idle_timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteProbeResult {
    pub project_id: String,
    pub connected: bool,
    pub protocol_version: u32,
    pub helper_version: String,
    pub root: String,
    pub os: String,
    pub arch: String,
    pub git_version: Option<String>,
    pub capabilities: Vec<String>,
    pub round_trip_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteLedgerParams {
    pub project_id: String,
    #[serde(default = "default_ledger_limit")]
    pub limit: u32,
}

fn default_ledger_limit() -> u32 {
    20
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteLedgerEntry {
    pub operation_id: String,
    pub method: String,
    pub success: bool,
    pub completed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteLedgerResult {
    pub project_id: String,
    pub entries: usize,
    pub max_entries: usize,
    pub persisted: bool,
    pub file_bytes: u64,
    pub tail: Vec<RemoteLedgerEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteWorktreeListParams {
    pub project_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteWorktree {
    pub path: String,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub head: Option<String>,
    #[serde(default)]
    pub detached: bool,
    #[serde(default)]
    pub locked: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteWorktreeListResult {
    pub project_id: String,
    pub worktrees: Vec<RemoteWorktree>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote() -> ExecutionHost {
        ExecutionHost {
            mode: ExecutionHostMode::Ssh,
            host: "build.example.com".into(),
            user: Some("agent".into()),
            port: Some(2222),
            identity_file: Some("/home/agent/.ssh/id_ed25519".into()),
            root: "/srv/project".into(),
            helper_path: "/usr/local/bin/vp-remote-agent".into(),
            token_env: Some("VERLINPARK_REMOTE_TOKEN".into()),
            ..ExecutionHost::default()
        }
    }

    #[test]
    fn local_is_the_default_and_backward_compatible() {
        let config: ExecutionHost = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(config.mode, ExecutionHostMode::Local);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn remote_requires_host_root_and_a_token_source() {
        let mut config = remote();
        assert!(config.validate().is_ok());
        config.host.clear();
        assert!(config.validate().is_err());
        config = remote();
        config.root = "relative".into();
        assert!(config.validate().is_err());
        config = remote();
        config.root = "/srv/../etc".into();
        assert!(config.validate().is_err());
        config = remote();
        config.token_env = None;
        assert!(config.validate().is_err());
    }

    #[test]
    fn ssh_injection_is_rejected() {
        let mut config = remote();
        config.host = "host; rm -rf /".into();
        assert!(config.validate().is_err());
        config = remote();
        config.user = Some("agent;id".into());
        assert!(config.validate().is_err());
    }

    #[test]
    fn bounds_are_validated() {
        let mut config = remote();
        config.connect_timeout_secs = 0;
        assert!(config.validate().is_err());
        config = remote();
        config.max_concurrency = MAX_REMOTE_CONCURRENCY + 1;
        assert!(config.validate().is_err());
        config = remote();
        config.ssh_args = (0..MAX_SSH_ARGS + 1)
            .map(|index| format!("-o{index}"))
            .collect();
        assert!(config.validate().is_err());
        config = remote();
        config.env_allow = vec!["BAD-NAME".into()];
        assert!(config.validate().is_err());
    }

    #[test]
    fn labels_are_readable_and_unknown_fields_are_ignored() {
        let mut value = serde_json::to_value(remote()).unwrap();
        value["future_field"] = serde_json::json!({"ignored": true});
        let config: ExecutionHost = serde_json::from_value(value).unwrap();
        assert_eq!(config.label(), "agent@build.example.com:/srv/project");
        assert_eq!(ExecutionHost::default().label(), "local");
        assert!(config.is_remote());
        assert!(!ExecutionHost::default().is_remote());
    }

    #[test]
    fn local_cannot_carry_remote_fields() {
        let config = ExecutionHost {
            root: "/tmp".into(),
            ..ExecutionHost::default()
        };
        assert!(config.validate().is_err());
    }
}

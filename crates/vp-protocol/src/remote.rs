//! Project execution-host configuration and remote-workspace RPC DTOs.
//!
//! The default remains `local`, so existing projects and old protocol clients
//! are unchanged. Remote hosts are explicit, project-scoped configuration; the
//! daemon never reroutes an existing local tool because a host exists.

use serde::{Deserialize, Serialize};

pub const DEFAULT_HELPER_PATH: &str = "vp-remote-agent";
/// Default hard cap for a Git bundle exported by a remote helper. The daemon
/// also enforces this as a policy value; the helper receives the effective cap
/// in the create request and refuses a bundle that grows past it.
pub const DEFAULT_MAX_BUNDLE_BYTES: u64 = 256 * 1024 * 1024;
/// Lower bound exists so tests and small repositories can configure a visible
/// cap without disabling the check.
pub const MIN_MAX_BUNDLE_BYTES: u64 = 1024;
/// Upper bound keeps one export from exhausting the helper filesystem.
pub const MAX_MAX_BUNDLE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const DEFAULT_BUNDLE_CHUNK_BYTES: u64 = 1024 * 1024;
pub const MAX_BUNDLE_CHUNK_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_REMOTE_ALLOWED_HOSTS: usize = 32;
pub const MAX_REMOTE_BRANCH_CHARS: usize = 255;
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

/// Project-scoped policy for importing a remote branch into the local
/// Verifier and merge queue. Disabled by default; enabling requires an explicit
/// host allowlist, because a helper on a trusted host can export any branch in
/// its root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteIntegrationPolicy {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_max_bundle_bytes")]
    pub max_bundle_bytes: u64,
    #[serde(default = "default_true")]
    pub require_verifier: bool,
    /// Exact SSH host names (or `user@host`) that may be integrated. There is
    /// no wildcard: an empty list cannot be enabled.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    /// Keep `refs/verlinpark/remote/...` after a successful integration for
    /// operator inspection. Retained refs are never fetched or merged
    /// automatically.
    #[serde(default)]
    pub keep_import_refs: bool,
}

fn default_max_bundle_bytes() -> u64 {
    DEFAULT_MAX_BUNDLE_BYTES
}

fn default_true() -> bool {
    true
}

impl Default for RemoteIntegrationPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            max_bundle_bytes: DEFAULT_MAX_BUNDLE_BYTES,
            require_verifier: true,
            allowed_hosts: Vec::new(),
            keep_import_refs: false,
        }
    }
}

impl RemoteIntegrationPolicy {
    pub fn validate(&self) -> Result<(), String> {
        if !(MIN_MAX_BUNDLE_BYTES..=MAX_MAX_BUNDLE_BYTES).contains(&self.max_bundle_bytes) {
            return Err(format!(
                "max_bundle_bytes must be {MIN_MAX_BUNDLE_BYTES}..={MAX_MAX_BUNDLE_BYTES}"
            ));
        }
        if self.allowed_hosts.len() > MAX_REMOTE_ALLOWED_HOSTS {
            return Err(format!(
                "at most {MAX_REMOTE_ALLOWED_HOSTS} allowed hosts are permitted"
            ));
        }
        for host in &self.allowed_hosts {
            validate_allowed_host(host)?;
        }
        if self.enabled && self.allowed_hosts.is_empty() {
            return Err(
                "remote integration requires at least one explicit allowed host when enabled"
                    .to_string(),
            );
        }
        Ok(())
    }

    /// Whether a configured execution host is explicitly allowed. Matching is
    /// exact against the host name, `user@host`, or the full host label; no
    /// substring or wildcard match is accepted.
    pub fn allows_host(&self, config: &ExecutionHost) -> bool {
        if !config.is_remote() {
            return false;
        }
        let host = config.host.trim();
        let user_host = config.user.as_deref().map(|user| format!("{user}@{host}"));
        let label = config.label();
        self.allowed_hosts.iter().any(|allowed| {
            let allowed = allowed.trim();
            !allowed.is_empty()
                && (allowed.eq_ignore_ascii_case(host)
                    || user_host
                        .as_deref()
                        .is_some_and(|value| value.eq_ignore_ascii_case(allowed))
                    || allowed.eq_ignore_ascii_case(&label))
        })
    }

    pub fn is_empty_policy(&self) -> bool {
        *self == Self::default()
    }
}

fn validate_allowed_host(host: &str) -> Result<(), String> {
    if host.is_empty() || host.len() > 255 {
        return Err("allowed host entries must be 1..=255 characters".to_string());
    }
    if host.chars().any(|character| {
        character.is_whitespace()
            || character.is_control()
            || matches!(
                character,
                ';' | '&' | '|' | '`' | '$' | '\'' | '"' | '\\' | '/' | '*' | '?'
            )
    }) {
        return Err(format!(
            "allowed host contains characters that cannot match a configured host: {host}"
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RemoteIntegrationPolicyParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteIntegrationPolicyResult {
    pub project_id: String,
    pub policy: RemoteIntegrationPolicy,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetRemoteIntegrationPolicyParams {
    pub project_id: String,
    pub policy: RemoteIntegrationPolicy,
    /// Explicit operator confirmation. Enabling remote integration without
    /// this flag fails closed even when the policy body is valid.
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RemoteIntegrationParams {
    pub project_id: String,
    pub session_id: String,
    /// Branch name in the remote project checkout (without `refs/heads/`).
    pub branch: String,
    /// Optional assertion of the exact remote host. When set it must match the
    /// project execution host; the policy allowlist is checked regardless.
    #[serde(default)]
    pub host: Option<String>,
    /// Optional recorded remote head. The bundle's exported head must equal
    /// this value; when omitted the helper's reported head is used.
    #[serde(default)]
    pub expected_commit: Option<String>,
    /// Optional local prerequisite commit for a thin bundle. The helper exports
    /// `<branch> --not <base>`, and the local import still verifies the bundle
    /// against this repository. Omit it for a self-contained bundle.
    #[serde(default)]
    pub base_commit: Option<String>,
    /// Optional plan task to attach the merge-queue entry to.
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub objective: Option<String>,
    /// Explicit operator confirmation for a host-level integration action.
    #[serde(default)]
    pub confirm: bool,
    /// Overrides `policy.keep_import_refs` for this integration.
    #[serde(default)]
    pub keep_import_refs: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteBundleSummary {
    pub host: String,
    pub branch: String,
    pub head: String,
    pub sha256: String,
    pub bytes: u64,
    pub chunks: u32,
    pub chunk_bytes: u64,
    /// Wall-clock time reported by the daemon after the last chunk arrived.
    pub imported_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteIntegrationVerification {
    pub required: bool,
    pub performed: bool,
    #[serde(default)]
    pub approved: Option<bool>,
    #[serde(default)]
    pub summary: Option<String>,
    /// Local verification runs against an isolated clone of the imported
    /// commit. It does not reproduce the remote host, installed tools,
    /// environment variables, or network.
    pub environment_caveat: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteIntegrationResult {
    pub project_id: String,
    pub session_id: String,
    pub entry: crate::types::MergeQueueEntry,
    pub bundle: RemoteBundleSummary,
    pub verification: RemoteIntegrationVerification,
    #[serde(default)]
    pub imported_ref: Option<String>,
    pub refs_pruned: bool,
    #[serde(default)]
    pub cleanup_error: Option<String>,
    pub environment_caveat: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RemoteIntegrationStatusParams {
    pub project_id: String,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteIntegrationQueueEntry {
    pub entry_id: String,
    pub session_id: String,
    pub status: String,
    pub commit: String,
    pub branch: String,
    #[serde(default)]
    pub remote_host: Option<String>,
    #[serde(default)]
    pub remote_branch: Option<String>,
    #[serde(default)]
    pub bundle_sha256: Option<String>,
    #[serde(default)]
    pub imported_ref: Option<String>,
    #[serde(default)]
    pub verifier_approved: Option<bool>,
    #[serde(default)]
    pub verifier_summary: Option<String>,
    #[serde(default)]
    pub environment_caveat: Option<String>,
    pub attempt: u32,
    #[serde(default)]
    pub detail: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RemoteIntegrationCleanupParams {
    pub project_id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    /// Restrict cleanup to one queue entry.
    #[serde(default)]
    pub entry_id: Option<String>,
    /// Also remove refs/worktrees for retryable (`conflict`/`failed`/
    /// `interrupted`) entries. Without this flag those refs are retained so an
    /// operator can retry the exact imported branch.
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteIntegrationCleanupResult {
    pub project_id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    pub removed_refs: Vec<String>,
    pub removed_worktrees: Vec<String>,
    pub skipped: Vec<String>,
    pub entries_examined: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteIntegrationStatusResult {
    pub project_id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    pub policy: RemoteIntegrationPolicy,
    pub policy_updated_at: String,
    pub entries: Vec<RemoteIntegrationQueueEntry>,
    pub total: usize,
    pub pending: usize,
    pub merged: usize,
    pub conflicts: usize,
    pub failed: usize,
    /// Number of entries whose temporary ref is still present because the
    /// integration failed or the policy keeps refs.
    pub retained_refs: usize,
    pub verifier_required: bool,
    pub environment_caveat: String,
}

/// Stable environment statement attached to every remote verification and
/// status result. Keeping it in one constant prevents a UI or CLI from
/// implying that the remote host was reproduced.
pub const REMOTE_VERIFICATION_ENVIRONMENT_CAVEAT: &str = "Local verification checks the imported commit in an isolated clone on this daemon host. It does not reproduce the remote host's OS, installed tools, environment variables, network, or uncommitted files.";

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

    #[test]
    fn remote_integration_is_disabled_by_default_and_bounded() {
        let policy = RemoteIntegrationPolicy::default();
        assert!(!policy.enabled);
        assert!(policy.require_verifier);
        assert_eq!(policy.max_bundle_bytes, DEFAULT_MAX_BUNDLE_BYTES);
        assert!(policy.allowed_hosts.is_empty());
        assert!(policy.validate().is_ok());
        assert!(policy.is_empty_policy());

        let decoded: RemoteIntegrationPolicy =
            serde_json::from_value(serde_json::json!({})).expect("legacy policy must decode");
        assert_eq!(decoded, policy);

        let mut invalid = policy.clone();
        invalid.max_bundle_bytes = MIN_MAX_BUNDLE_BYTES - 1;
        assert!(invalid.validate().is_err());
        invalid.max_bundle_bytes = MAX_MAX_BUNDLE_BYTES + 1;
        assert!(invalid.validate().is_err());
        invalid.max_bundle_bytes = DEFAULT_MAX_BUNDLE_BYTES;
        invalid.allowed_hosts = (0..=MAX_REMOTE_ALLOWED_HOSTS)
            .map(|index| format!("host{index}"))
            .collect();
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn enabling_remote_integration_requires_an_explicit_host_allowlist() {
        let mut policy = RemoteIntegrationPolicy {
            enabled: true,
            ..RemoteIntegrationPolicy::default()
        };
        assert!(policy.validate().is_err());
        policy.allowed_hosts = vec!["build.example.com".into()];
        assert!(policy.validate().is_ok());
        let config = remote();
        assert!(policy.allows_host(&config));
        policy.allowed_hosts = vec!["agent@build.example.com".into()];
        assert!(policy.allows_host(&config));
        policy.allowed_hosts = vec!["agent@build.example.com:/srv/project".into()];
        assert!(policy.allows_host(&config));
        policy.allowed_hosts = vec!["build".into()];
        assert!(!policy.allows_host(&config));
        policy.allowed_hosts = vec!["*.example.com".into()];
        assert!(policy.validate().is_err());
        policy.allowed_hosts = vec!["host;rm".into()];
        assert!(policy.validate().is_err());
        assert!(!policy.allows_host(&ExecutionHost::default()));
        policy.allowed_hosts = vec!["BUILD.EXAMPLE.COM".into()];
        assert!(policy.allows_host(&config));
        policy.allowed_hosts = vec!["Agent@Build.Example.Com".into()];
        assert!(policy.allows_host(&config));
        policy.allowed_hosts = vec!["agent@build.example.com:/srv/project".into()];
        assert!(policy.allows_host(&config));
        policy.allowed_hosts = vec!["build.example.com:2222".into()];
        assert!(!policy.allows_host(&config));
    }

    #[test]
    fn remote_integration_status_and_cleanup_round_trip() {
        let status = RemoteIntegrationStatusResult {
            project_id: "p1".into(),
            session_id: Some("s1".into()),
            policy: RemoteIntegrationPolicy {
                enabled: true,
                max_bundle_bytes: 4096,
                require_verifier: true,
                allowed_hosts: vec!["build.example.com".into()],
                keep_import_refs: true,
            },
            policy_updated_at: "2026-11-11T00:00:00Z".into(),
            entries: vec![RemoteIntegrationQueueEntry {
                entry_id: "e1".into(),
                session_id: "s1".into(),
                status: "conflict".into(),
                commit: "a".repeat(40),
                branch: "verlinpark/s1--remote-1".into(),
                remote_host: Some("build.example.com".into()),
                remote_branch: Some("feature/x".into()),
                bundle_sha256: Some("b".repeat(64)),
                imported_ref: Some("refs/verlinpark/remote/s1/i1".into()),
                verifier_approved: Some(true),
                verifier_summary: Some("approved".into()),
                environment_caveat: Some("local only".into()),
                attempt: 2,
                detail: Some("conflict".into()),
                created_at: "2026-11-11T00:00:00Z".into(),
                updated_at: "2026-11-11T00:01:00Z".into(),
            }],
            total: 1,
            pending: 0,
            merged: 0,
            conflicts: 1,
            failed: 0,
            retained_refs: 1,
            verifier_required: true,
            environment_caveat: REMOTE_VERIFICATION_ENVIRONMENT_CAVEAT.into(),
        };
        let decoded: RemoteIntegrationStatusResult =
            serde_json::from_value(serde_json::to_value(&status).unwrap()).unwrap();
        assert_eq!(decoded.entries[0].verifier_approved, Some(true));
        assert_eq!(
            decoded.entries[0].remote_branch.as_deref(),
            Some("feature/x")
        );
        assert_eq!(decoded.retained_refs, 1);
        let cleanup = RemoteIntegrationCleanupParams {
            project_id: "p1".into(),
            session_id: None,
            entry_id: Some("e1".into()),
            force: true,
        };
        let decoded: RemoteIntegrationCleanupParams =
            serde_json::from_value(serde_json::to_value(&cleanup).unwrap()).unwrap();
        assert!(decoded.force);
        assert!(decoded.session_id.is_none());
        let result = RemoteIntegrationCleanupResult {
            project_id: "p1".into(),
            session_id: None,
            removed_refs: vec!["refs/verlinpark/remote/s1/i1".into()],
            removed_worktrees: Vec::new(),
            skipped: vec!["e2: requires force".into()],
            entries_examined: 2,
        };
        let decoded: RemoteIntegrationCleanupResult =
            serde_json::from_value(serde_json::to_value(&result).unwrap()).unwrap();
        assert_eq!(decoded.removed_refs.len(), 1);
        assert_eq!(decoded.entries_examined, 2);
    }

    #[test]
    fn remote_integration_requests_default_to_unconfirmed_and_local_settings() {
        let params: RemoteIntegrationParams = serde_json::from_value(serde_json::json!({
            "project_id": "p1",
            "session_id": "s1",
            "branch": "feature/remote"
        }))
        .unwrap();
        assert!(!params.confirm);
        assert!(params.host.is_none());
        assert!(params.expected_commit.is_none());
        assert!(params.base_commit.is_none());
        assert!(params.keep_import_refs.is_none());
        let status: RemoteIntegrationStatusParams =
            serde_json::from_value(serde_json::json!({"project_id": "p1"})).unwrap();
        assert!(status.session_id.is_none());
    }
}

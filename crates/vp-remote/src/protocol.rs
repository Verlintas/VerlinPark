//! Wire protocol for remote workspaces.
//!
//! The protocol is newline-delimited JSON-RPC 2.0. One line is one complete
//! request, response, or notification. Frames are bounded and the handshake is
//! mandatory: a helper refuses every operation until `hello` has authenticated
//! the client and agreed on the protocol version and root.
//!
//! The types here deliberately stay independent of the daemon's provider and
//! session model. A helper only needs this crate, `tokio`, and the Git/POSIX
//! tools installed on the remote host.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use vp_protocol::rpc::RpcError;

/// Version of the remote-workspace protocol. This is separate from the
/// VerlinPark daemon protocol version: a daemon upgrade may keep speaking the
/// same helper protocol.
pub const REMOTE_PROTOCOL_VERSION: u32 = 1;

/// Absolute maximum frame size. The helper and client both refuse larger
/// frames before parsing them.
pub const MAX_FRAME_BYTES: u64 = 16 * 1024 * 1024;
pub const DEFAULT_MAX_FRAME_BYTES: u64 = MAX_FRAME_BYTES;

/// Bounds for individual operations. These are protocol limits, not policy:
/// a project may configure stricter limits on the daemon side.
pub const MAX_READ_BYTES: u64 = 8 * 1024 * 1024;
pub const DEFAULT_READ_BYTES: u64 = 1024 * 1024;
pub const MAX_WRITE_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_DIR_ENTRIES: u32 = 5_000;
pub const DEFAULT_DIR_ENTRIES: u32 = 500;
pub const MAX_EXEC_OUTPUT_BYTES: u64 = 4 * 1024 * 1024;
pub const DEFAULT_EXEC_OUTPUT_BYTES: u64 = 64 * 1024;
pub const MAX_EXEC_TIMEOUT_MS: u64 = 60 * 60 * 1000;
pub const DEFAULT_EXEC_TIMEOUT_MS: u64 = 120 * 1000;
pub const MAX_GIT_OUTPUT_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_GIT_PATHS: usize = 100;
pub const MAX_ENV_VARS: usize = 32;
pub const MAX_ENV_VALUE_BYTES: usize = 8 * 1024;
pub const MAX_OPERATION_ID_CHARS: usize = 160;

pub const METHOD_HELLO: &str = "hello";
pub const METHOD_PROBE: &str = "probe";
pub const METHOD_HEALTH: &str = "health";
pub const METHOD_STAT: &str = "stat";
pub const METHOD_LIST_DIR: &str = "list_dir";
pub const METHOD_READ_FILE: &str = "read_file";
pub const METHOD_WRITE_FILE: &str = "write_file";
pub const METHOD_MKDIR: &str = "mkdir";
pub const METHOD_REMOVE: &str = "remove";
pub const METHOD_RENAME: &str = "rename";
pub const METHOD_EXEC: &str = "exec";
pub const METHOD_GIT_STATUS: &str = "git_status";
pub const METHOD_GIT_DIFF: &str = "git_diff";
pub const METHOD_GIT_LOG: &str = "git_log";
pub const METHOD_GIT_REV_PARSE: &str = "git_rev_parse";
pub const METHOD_GIT_WORKTREE_ADD: &str = "git_worktree_add";
pub const METHOD_GIT_WORKTREE_LIST: &str = "git_worktree_list";
pub const METHOD_GIT_WORKTREE_REMOVE: &str = "git_worktree_remove";
pub const METHOD_LEDGER_STATS: &str = "ledger_stats";
pub const METHOD_CANCEL: &str = "cancel";
pub const METHOD_SHUTDOWN: &str = "shutdown";

pub const CAP_FS_READ: &str = "fs.read";
pub const CAP_FS_WRITE: &str = "fs.write";
pub const CAP_EXEC: &str = "exec";
pub const CAP_GIT: &str = "git";
pub const CAP_GIT_WORKTREE: &str = "git.worktree";
pub const CAP_CANCEL: &str = "cancel";
pub const CAP_LEDGER: &str = "ledger";
pub const CAP_SHA256: &str = "sha256";
pub const CAP_BASE64: &str = "base64";

/// Every capability a version-1 helper must advertise. The client checks this
/// set before it exposes a workspace, so a partial helper fails at connect
/// instead of at the first tool call.
pub const REQUIRED_CAPABILITIES: &[&str] = &[
    CAP_FS_READ,
    CAP_FS_WRITE,
    CAP_EXEC,
    CAP_GIT,
    CAP_GIT_WORKTREE,
    CAP_CANCEL,
    CAP_LEDGER,
    CAP_SHA256,
    CAP_BASE64,
];

/// Error vocabulary shared by the helper and the daemon. The string code is
/// stable and appears in `error.data.code`; the JSON-RPC numeric code remains
/// compatible with generic clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RemoteErrorCode {
    InvalidRequest,
    InvalidParams,
    Unauthorized,
    AlreadyAuthenticated,
    ProtocolVersionMismatch,
    RootMismatch,
    UnsupportedOperation,
    PathEscape,
    PathNotFound,
    NotADirectory,
    NotAFile,
    PermissionDenied,
    FileTooLarge,
    InvalidEncoding,
    ShaMismatch,
    PreconditionRequired,
    AlreadyExists,
    DestinationExists,
    DirectoryNotEmpty,
    EnvNotAllowed,
    OutputLimit,
    Timeout,
    Cancelled,
    GitUnavailable,
    GitFailed,
    OperationIdRequired,
    OperationIdConflict,
    OperationInProgress,
    LedgerError,
    TransportClosed,
    ShutDown,
    InvalidResponse,
    Internal,
}

impl RemoteErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "INVALID_REQUEST",
            Self::InvalidParams => "INVALID_PARAMS",
            Self::Unauthorized => "UNAUTHORIZED",
            Self::AlreadyAuthenticated => "ALREADY_AUTHENTICATED",
            Self::ProtocolVersionMismatch => "PROTOCOL_VERSION_MISMATCH",
            Self::RootMismatch => "ROOT_MISMATCH",
            Self::UnsupportedOperation => "UNSUPPORTED_OPERATION",
            Self::PathEscape => "PATH_ESCAPE",
            Self::PathNotFound => "PATH_NOT_FOUND",
            Self::NotADirectory => "NOT_A_DIRECTORY",
            Self::NotAFile => "NOT_A_FILE",
            Self::PermissionDenied => "PERMISSION_DENIED",
            Self::FileTooLarge => "FILE_TOO_LARGE",
            Self::InvalidEncoding => "INVALID_ENCODING",
            Self::ShaMismatch => "SHA_MISMATCH",
            Self::PreconditionRequired => "PRECONDITION_REQUIRED",
            Self::AlreadyExists => "ALREADY_EXISTS",
            Self::DestinationExists => "DESTINATION_EXISTS",
            Self::DirectoryNotEmpty => "DIRECTORY_NOT_EMPTY",
            Self::EnvNotAllowed => "ENV_NOT_ALLOWED",
            Self::OutputLimit => "OUTPUT_LIMIT",
            Self::Timeout => "TIMEOUT",
            Self::Cancelled => "CANCELLED",
            Self::GitUnavailable => "GIT_UNAVAILABLE",
            Self::GitFailed => "GIT_FAILED",
            Self::OperationIdRequired => "OPERATION_ID_REQUIRED",
            Self::OperationIdConflict => "OPERATION_ID_CONFLICT",
            Self::OperationInProgress => "OPERATION_IN_PROGRESS",
            Self::LedgerError => "LEDGER_ERROR",
            Self::TransportClosed => "TRANSPORT_CLOSED",
            Self::ShutDown => "SHUTDOWN",
            Self::InvalidResponse => "INVALID_RESPONSE",
            Self::Internal => "INTERNAL",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        serde_json::from_value(Value::String(value.to_string())).ok()
    }

    /// Whether a caller may safely retry with the same operation id.
    pub fn retryable(self) -> bool {
        matches!(
            self,
            Self::Timeout
                | Self::Cancelled
                | Self::OperationInProgress
                | Self::TransportClosed
                | Self::Internal
                | Self::OutputLimit
        )
    }

    pub fn json_rpc_code(self) -> i32 {
        match self {
            Self::InvalidRequest => -32600,
            Self::InvalidParams => -32602,
            Self::Unauthorized => -32001,
            Self::UnsupportedOperation => -32601,
            Self::PathNotFound => -32004,
            _ => -32010,
        }
    }
}

/// A remote error with an actionable message. The helper never returns a bare
/// generic failure when it can name the rejected path, capability, or limit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteError {
    pub code: RemoteErrorCode,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
}

impl RemoteError {
    pub fn new(code: RemoteErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: code.retryable(),
            hint: None,
            operation_id: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn with_operation(mut self, operation_id: impl Into<String>) -> Self {
        self.operation_id = Some(operation_id.into());
        self
    }

    pub fn to_rpc_error(&self) -> RpcError {
        RpcError::with_data(
            self.code.json_rpc_code(),
            self.message.clone(),
            serde_json::to_value(self).unwrap_or(Value::Null),
        )
    }
}

impl std::fmt::Display for RemoteError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for RemoteError {}

impl From<RpcError> for RemoteError {
    fn from(error: RpcError) -> Self {
        if let Some(data) = error.data.as_ref()
            && let Ok(remote) = serde_json::from_value::<RemoteError>(data.clone())
        {
            return remote;
        }
        let code = match error.code {
            -32600 => RemoteErrorCode::InvalidRequest,
            -32601 => RemoteErrorCode::UnsupportedOperation,
            -32602 => RemoteErrorCode::InvalidParams,
            -32001 => RemoteErrorCode::Unauthorized,
            -32004 => RemoteErrorCode::PathNotFound,
            _ => RemoteErrorCode::Internal,
        };
        RemoteError::new(code, error.message)
    }
}

pub fn validate_operation_id(value: &str) -> Result<(), RemoteError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(RemoteError::new(
            RemoteErrorCode::OperationIdRequired,
            "operation_id must not be empty",
        ));
    }
    if value.chars().count() > MAX_OPERATION_ID_CHARS {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            format!("operation_id exceeds {MAX_OPERATION_ID_CHARS} characters"),
        ));
    }
    if !value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "-_.:/".contains(character))
    {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            "operation_id may contain only ASCII letters, digits, '-', '_', '.', ':', and '/'",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HelloParams {
    #[serde(default)]
    pub protocol_version: u32,
    #[serde(default)]
    pub client_version: String,
    #[serde(default)]
    pub auth_token: String,
    /// Canonical root the client expects. A mismatch is a hard refusal so a
    /// misconfigured project cannot silently operate on the wrong directory.
    #[serde(default)]
    pub root: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HelloResult {
    pub protocol_version: u32,
    pub helper_version: String,
    pub root: String,
    pub capabilities: Vec<String>,
    pub max_frame_bytes: u64,
    #[serde(default)]
    pub env_allow: Vec<String>,
    #[serde(default)]
    pub server_started_at: String,
}

impl HelloResult {
    pub fn has_capability(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|value| value == capability)
    }

    pub fn missing_required(&self) -> Vec<&'static str> {
        REQUIRED_CAPABILITIES
            .iter()
            .copied()
            .filter(|capability| !self.has_capability(capability))
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProbeParams {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProbeResult {
    pub protocol_version: u32,
    pub helper_version: String,
    pub root: String,
    pub pid: u32,
    pub os: String,
    pub arch: String,
    pub uptime_secs: u64,
    pub git_version: Option<String>,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StatParams {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StatResult {
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
    #[serde(default)]
    pub mode: u32,
    #[serde(default)]
    pub modified_unix_ms: Option<i64>,
    #[serde(default)]
    pub symlink_target: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListDirParams {
    #[serde(default = "default_dot")]
    pub path: String,
    #[serde(default = "default_true")]
    pub include_hidden: bool,
    #[serde(default = "default_dir_entries")]
    pub limit: u32,
    #[serde(default)]
    pub cursor: u32,
}

fn default_dot() -> String {
    ".".to_string()
}

fn default_true() -> bool {
    true
}

fn default_dir_entries() -> u32 {
    DEFAULT_DIR_ENTRIES
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
    #[serde(default)]
    pub mode: u32,
    #[serde(default)]
    pub symlink_target: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListDirResult {
    pub path: String,
    pub entries: Vec<DirEntry>,
    pub total: u32,
    pub next_cursor: Option<u32>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReadEncoding {
    #[default]
    Auto,
    Utf8,
    Base64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReadFileParams {
    pub path: String,
    /// Zero-based byte offset. The caller is responsible for using a byte
    /// offset that makes sense for a text file; a non-boundary offset falls
    /// back to base64 in `auto` mode.
    #[serde(default)]
    pub offset: u64,
    #[serde(default = "default_read_bytes")]
    pub limit: u64,
    #[serde(default)]
    pub encoding: ReadEncoding,
}

fn default_read_bytes() -> u64 {
    DEFAULT_READ_BYTES
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReadFileResult {
    pub path: String,
    pub sha256: String,
    pub size: u64,
    pub offset: u64,
    pub bytes_read: u64,
    pub encoding: ReadEncoding,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub base64: Option<String>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WriteEncoding {
    #[default]
    Utf8,
    Base64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WriteFileParams {
    pub path: String,
    pub content: String,
    #[serde(default)]
    pub encoding: WriteEncoding,
    /// SHA-256 of the file the writer expects to replace. Required when the
    /// file already exists unless `allow_overwrite` is explicitly true.
    #[serde(default)]
    pub expected_sha256: Option<String>,
    #[serde(default = "default_true")]
    pub create_parents: bool,
    #[serde(default)]
    pub allow_overwrite: bool,
    #[serde(default)]
    pub mode: Option<u32>,
    #[serde(default)]
    pub operation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WriteFileResult {
    pub path: String,
    pub sha256: String,
    pub size: u64,
    pub created: bool,
    pub replaced: bool,
    #[serde(default)]
    pub operation_id: Option<String>,
    #[serde(default)]
    pub replayed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MkdirParams {
    pub path: String,
    #[serde(default = "default_true")]
    pub recursive: bool,
    #[serde(default)]
    pub operation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MkdirResult {
    pub path: String,
    pub created: bool,
    #[serde(default)]
    pub operation_id: Option<String>,
    #[serde(default)]
    pub replayed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoveParams {
    pub path: String,
    #[serde(default)]
    pub recursive: bool,
    #[serde(default)]
    pub missing_ok: bool,
    #[serde(default)]
    pub operation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoveResult {
    pub path: String,
    pub removed: bool,
    #[serde(default)]
    pub operation_id: Option<String>,
    #[serde(default)]
    pub replayed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RenameParams {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub overwrite: bool,
    #[serde(default)]
    pub create_parents: bool,
    #[serde(default)]
    pub operation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RenameResult {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub operation_id: Option<String>,
    #[serde(default)]
    pub replayed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecParams {
    pub command: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default = "default_exec_timeout")]
    pub timeout_ms: u64,
    #[serde(default = "default_exec_output")]
    pub output_cap_bytes: u64,
    #[serde(default)]
    pub operation_id: Option<String>,
}

fn default_exec_timeout() -> u64 {
    DEFAULT_EXEC_TIMEOUT_MS
}

fn default_exec_output() -> u64 {
    DEFAULT_EXEC_OUTPUT_BYTES
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecResult {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub timed_out: bool,
    pub cancelled: bool,
    pub duration_ms: u64,
    #[serde(default)]
    pub operation_id: Option<String>,
    #[serde(default)]
    pub replayed: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiffScope {
    #[default]
    Unstaged,
    Staged,
    Base,
    Commit,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitStatusParams {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitStatusEntry {
    pub status: String,
    pub path: String,
    #[serde(default)]
    pub original_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitStatusResult {
    pub branch: Option<String>,
    pub head: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u64,
    pub behind: u64,
    pub clean: bool,
    pub entries: Vec<GitStatusEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitDiffParams {
    #[serde(default)]
    pub scope: DiffScope,
    #[serde(default)]
    pub base: Option<String>,
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default = "default_diff_context")]
    pub context: u32,
    #[serde(default)]
    pub stat_only: bool,
}

fn default_diff_context() -> u32 {
    3
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitDiffResult {
    pub scope: DiffScope,
    pub patch: String,
    pub stat: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitLogParams {
    #[serde(default = "default_log_limit")]
    pub limit: u32,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub since: Option<String>,
}

fn default_log_limit() -> u32 {
    20
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitLogEntry {
    pub hash: String,
    pub short_hash: String,
    pub author: String,
    pub email: String,
    pub timestamp: i64,
    pub subject: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitLogResult {
    pub entries: Vec<GitLogEntry>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitRevParseParams {
    #[serde(default = "default_head")]
    pub rev: String,
    /// Resolve `rev^{commit}` instead of a raw object.
    #[serde(default = "default_true")]
    pub commit: bool,
}

fn default_head() -> String {
    "HEAD".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitRevParseResult {
    pub rev: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitWorktreeAddParams {
    /// Root-relative worktree path. Absolute paths and `..` are rejected.
    pub path: String,
    pub branch: String,
    #[serde(default)]
    pub start_from: Option<String>,
    #[serde(default = "default_true")]
    pub create_branch: bool,
    #[serde(default)]
    pub operation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeEntry {
    pub path: String,
    #[serde(default)]
    pub head: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub bare: bool,
    #[serde(default)]
    pub detached: bool,
    #[serde(default)]
    pub locked: Option<String>,
    #[serde(default)]
    pub prunable: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitWorktreeListParams {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitWorktreeListResult {
    pub entries: Vec<WorktreeEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitWorktreeAddResult {
    pub entry: WorktreeEntry,
    #[serde(default)]
    pub operation_id: Option<String>,
    #[serde(default)]
    pub replayed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitWorktreeRemoveParams {
    pub path: String,
    #[serde(default)]
    pub force: bool,
    #[serde(default)]
    pub operation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitWorktreeRemoveResult {
    pub path: String,
    pub removed: bool,
    #[serde(default)]
    pub operation_id: Option<String>,
    #[serde(default)]
    pub replayed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LedgerStatsParams {
    #[serde(default = "default_ledger_tail")]
    pub limit: u32,
}

fn default_ledger_tail() -> u32 {
    20
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LedgerEntrySummary {
    pub operation_id: String,
    pub method: String,
    pub success: bool,
    pub completed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LedgerStatsResult {
    pub entries: usize,
    pub max_entries: usize,
    pub persisted: bool,
    pub file_bytes: u64,
    pub tail: Vec<LedgerEntrySummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CancelParams {
    pub id: Value,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShutdownParams {
    #[serde(default)]
    pub reason: Option<String>,
}

/// Application-level event emitted by a helper. The daemon currently logs
/// these; they are not model input.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteEvent {
    pub kind: String,
    pub message: String,
    #[serde(default)]
    pub operation_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_round_trip() {
        for code in [
            RemoteErrorCode::PathEscape,
            RemoteErrorCode::ShaMismatch,
            RemoteErrorCode::OperationIdConflict,
            RemoteErrorCode::ProtocolVersionMismatch,
        ] {
            let json = serde_json::to_value(code).unwrap();
            let parsed = RemoteErrorCode::parse(json.as_str().unwrap()).unwrap();
            assert_eq!(parsed, code);
        }
    }

    #[test]
    fn error_data_is_structured() {
        let error = RemoteError::new(RemoteErrorCode::PathEscape, "outside root")
            .with_hint("use a project-relative path")
            .with_operation("op-1");
        let rpc = error.to_rpc_error();
        assert_eq!(rpc.code, -32010);
        let decoded: RemoteError = serde_json::from_value(rpc.data.unwrap()).unwrap();
        assert_eq!(decoded, error);
    }

    #[test]
    fn operation_ids_are_bounded() {
        assert!(validate_operation_id("session:task:write-1").is_ok());
        assert!(validate_operation_id("").is_err());
        assert!(validate_operation_id("with space").is_err());
        assert!(validate_operation_id(&"x".repeat(200)).is_err());
    }

    #[test]
    fn read_defaults_are_safe() {
        let params: ReadFileParams = serde_json::from_value(serde_json::json!({
            "path": "a.txt"
        }))
        .unwrap();
        assert_eq!(params.offset, 0);
        assert_eq!(params.limit, DEFAULT_READ_BYTES);
        assert_eq!(params.encoding, ReadEncoding::Auto);
    }

    #[test]
    fn hello_checks_required_capabilities() {
        let hello = HelloResult {
            protocol_version: REMOTE_PROTOCOL_VERSION,
            helper_version: "test".into(),
            root: "/tmp".into(),
            capabilities: REQUIRED_CAPABILITIES
                .iter()
                .map(|value| value.to_string())
                .collect(),
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
            env_allow: Vec::new(),
            server_started_at: String::new(),
        };
        assert!(hello.missing_required().is_empty());
        let mut partial = hello.clone();
        partial.capabilities.retain(|value| value != CAP_EXEC);
        assert_eq!(partial.missing_required(), vec![CAP_EXEC]);
    }

    #[test]
    fn write_defaults_are_safe() {
        let params: WriteFileParams = serde_json::from_value(serde_json::json!({
            "path": "a.txt",
            "content": "hello"
        }))
        .unwrap();
        assert_eq!(params.encoding, WriteEncoding::Utf8);
        assert!(params.create_parents);
        assert!(!params.allow_overwrite);
        assert!(params.expected_sha256.is_none());
    }
}

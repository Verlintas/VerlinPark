//! Helper-side protocol server.
//!
//! The server is rooted at one canonical directory, authenticates exactly one
//! `hello`, and then dispatches bounded operations. Mutating operations are
//! idempotent through the operation ledger. All file paths go through the path
//! jail; all Git commands are structured; exec runs in a killable process
//! group with an allowlisted environment.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use base64::Engine as Base64Engine;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::Notify;
use vp_protocol::rpc::RpcResponse;

use crate::bundle::{self, BundleRegistry};
use crate::exec::{ExecControl, allowed_env_names, run_exec, validate_exec};
use crate::framing::{read_frame, write_frame};
use crate::git;
use crate::jail::PathJail;
use crate::ledger::{LedgerEntry, LedgerReplay, OperationLedger};
use crate::protocol::*;

const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const MIN_AUTH_TOKEN_CHARS: usize = 16;
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub root: PathBuf,
    pub auth_token: String,
    pub ledger_path: Option<PathBuf>,
    pub max_frame_bytes: usize,
    pub env_allow: Vec<String>,
    pub helper_version: String,
    pub ledger_entries: usize,
}

impl ServerConfig {
    pub fn new(
        root: impl Into<PathBuf>,
        auth_token: impl Into<String>,
    ) -> Result<Self, RemoteError> {
        let token = auth_token.into();
        if token.chars().count() < MIN_AUTH_TOKEN_CHARS {
            return Err(RemoteError::new(
                RemoteErrorCode::InvalidParams,
                format!("remote auth token must be at least {MIN_AUTH_TOKEN_CHARS} characters"),
            ));
        }
        Ok(Self {
            root: root.into(),
            auth_token: token,
            ledger_path: None,
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES as usize,
            env_allow: Vec::new(),
            helper_version: env!("CARGO_PKG_VERSION").to_string(),
            ledger_entries: crate::ledger::DEFAULT_LEDGER_ENTRIES,
        })
    }

    pub fn with_ledger(mut self, path: impl Into<PathBuf>) -> Self {
        self.ledger_path = Some(path.into());
        self
    }
}

struct ServerState {
    jail: PathJail,
    auth_token: String,
    max_frame_bytes: usize,
    env_allow: Vec<String>,
    helper_version: String,
    started: Instant,
    started_at: String,
    ledger: Arc<Mutex<OperationLedger>>,
    bundles: AsyncMutex<BundleRegistry>,
    cancels: Arc<Mutex<HashMap<String, ExecControl>>>,
    shutdown: Arc<Notify>,
}

impl ServerState {
    fn new(config: ServerConfig) -> Result<Self, RemoteError> {
        let jail = PathJail::new(&config.root)?;
        let allowed = allowed_env_names(&config.env_allow);
        for name in &config.env_allow {
            crate::transport::validate_env_name(name)?;
        }
        let _ = allowed;
        let ledger = match config.ledger_path {
            Some(path) => OperationLedger::open(path, config.ledger_entries)?,
            None => OperationLedger::in_memory(config.ledger_entries),
        };
        Ok(Self {
            jail,
            auth_token: config.auth_token,
            max_frame_bytes: config.max_frame_bytes.clamp(1024, MAX_FRAME_BYTES as usize),
            env_allow: config.env_allow,
            helper_version: config.helper_version,
            started: Instant::now(),
            started_at: vp_protocol::now_rfc3339(),
            ledger: Arc::new(Mutex::new(ledger)),
            bundles: AsyncMutex::new(BundleRegistry::new()),
            cancels: Arc::new(Mutex::new(HashMap::new())),
            shutdown: Arc::new(Notify::new()),
        })
    }

    fn capabilities(&self) -> Vec<String> {
        let mut capabilities = REQUIRED_CAPABILITIES
            .iter()
            .map(|value| (*value).to_string())
            .collect::<Vec<_>>();
        capabilities.push(CAP_GIT_BUNDLE.to_string());
        capabilities
    }

    fn probe(&self) -> ProbeResult {
        ProbeResult {
            protocol_version: REMOTE_PROTOCOL_VERSION,
            helper_version: self.helper_version.clone(),
            root: self.jail.root().to_string_lossy().to_string(),
            pid: std::process::id(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            uptime_secs: self.started.elapsed().as_secs(),
            git_version: git_version(),
            capabilities: self.capabilities(),
        }
    }
}

fn git_version() -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Run the helper over stdin/stdout. Protocol output is the only thing written
/// to stdout; logs and diagnostics go to stderr.
pub async fn serve_stdio(config: ServerConfig) -> Result<(), RemoteError> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    serve(stdin, stdout, config).await
}

pub async fn serve<R, W>(reader: R, writer: W, config: ServerConfig) -> Result<(), RemoteError>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let state = Arc::new(ServerState::new(config)?);
    let writer = Arc::new(AsyncMutex::new(writer));
    let mut reader = BufReader::new(reader);
    let mut authenticated = false;
    loop {
        let frame = tokio::select! {
            _ = state.shutdown.notified() => break,
            frame = read_frame(&mut reader, state.max_frame_bytes) => {
                match frame {
                    Ok(Some(frame)) => frame,
                    Ok(None) => break,
                    Err(error) => {
                        if error.kind() == io::ErrorKind::InvalidData {
                            let remote = RemoteError::new(
                                RemoteErrorCode::InvalidRequest,
                                error.to_string(),
                            );
                            let _ = write_response(
                                &writer,
                                state.max_frame_bytes,
                                RpcResponse::error(None, remote.to_rpc_error()),
                            )
                            .await;
                        }
                        return Err(RemoteError::new(
                            RemoteErrorCode::TransportClosed,
                            format!("remote helper input failed: {error}"),
                        ));
                    }
                }
            }
        };
        let value: Value = match serde_json::from_slice(&frame) {
            Ok(value) => value,
            Err(error) => {
                let remote = RemoteError::new(
                    RemoteErrorCode::InvalidRequest,
                    format!("malformed JSON-RPC frame: {error}"),
                );
                write_response(
                    &writer,
                    state.max_frame_bytes,
                    RpcResponse::error(None, remote.to_rpc_error()),
                )
                .await?;
                continue;
            }
        };
        let method = value
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let id = value.get("id").cloned();
        let params = value.get("params").cloned().unwrap_or_else(|| json!({}));
        let Some(id) = id else {
            // Notifications never receive a response.
            if method == METHOD_CANCEL {
                if let Ok(cancel) = serde_json::from_value::<CancelParams>(params) {
                    cancel_operation(&state, &cancel);
                }
            } else if method == METHOD_SHUTDOWN {
                state.shutdown.notify_waiters();
            }
            continue;
        };
        if method.is_empty() {
            let remote = RemoteError::new(
                RemoteErrorCode::InvalidRequest,
                "JSON-RPC request is missing a method",
            );
            write_response(
                &writer,
                state.max_frame_bytes,
                RpcResponse::error(Some(id), remote.to_rpc_error()),
            )
            .await?;
            continue;
        }
        if method == METHOD_HELLO {
            if authenticated {
                let remote = RemoteError::new(
                    RemoteErrorCode::AlreadyAuthenticated,
                    "this connection already completed the handshake",
                );
                write_response(
                    &writer,
                    state.max_frame_bytes,
                    RpcResponse::error(Some(id), remote.to_rpc_error()),
                )
                .await?;
                continue;
            }
            match handshake(&state, params) {
                Ok(result) => {
                    authenticated = true;
                    write_response(
                        &writer,
                        state.max_frame_bytes,
                        RpcResponse::success(Some(id), &result),
                    )
                    .await?;
                }
                Err(error) => {
                    write_response(
                        &writer,
                        state.max_frame_bytes,
                        RpcResponse::error(Some(id), error.to_rpc_error()),
                    )
                    .await?;
                    return Err(error);
                }
            }
            continue;
        }
        if !authenticated {
            let remote = RemoteError::new(
                RemoteErrorCode::Unauthorized,
                "the hello handshake must succeed before any operation",
            );
            write_response(
                &writer,
                state.max_frame_bytes,
                RpcResponse::error(Some(id), remote.to_rpc_error()),
            )
            .await?;
            return Err(remote);
        }
        if method == METHOD_SHUTDOWN {
            write_response(
                &writer,
                state.max_frame_bytes,
                RpcResponse::success(
                    Some(id),
                    json!({"ok": true, "reason": params.get("reason")}),
                ),
            )
            .await?;
            state.shutdown.notify_waiters();
            break;
        }
        let state = state.clone();
        let writer = writer.clone();
        tokio::spawn(async move {
            let id_value = id.clone();
            let response = handle_request(&state, &id_value, &method, params).await;
            let response = match response {
                Ok(result) => RpcResponse::success(Some(id_value), result),
                Err(error) => RpcResponse::error(Some(id_value), error.to_rpc_error()),
            };
            if let Err(error) = write_response(&writer, state.max_frame_bytes, response).await {
                tracing::warn!(%error, "failed to write remote helper response");
            }
        });
    }
    // A clean shutdown or client disconnect must not leave a command running
    // in the remote user's process group after the helper exits, nor a
    // temporary bundle/export ref on disk.
    cancel_all(&state);
    {
        let mut bundles = state.bundles.lock().await;
        bundle::cleanup_all(&state.jail, &mut bundles).await;
    }
    Ok(())
}

fn cancel_all(state: &ServerState) {
    if let Ok(cancels) = state.cancels.lock() {
        for control in cancels.values() {
            control.cancel();
        }
    }
}

async fn write_response<W: AsyncWrite + Unpin>(
    writer: &Arc<AsyncMutex<W>>,
    max_frame_bytes: usize,
    response: RpcResponse,
) -> Result<(), RemoteError> {
    let mut writer = writer.lock().await;
    write_frame(&mut *writer, &response, max_frame_bytes)
        .await
        .map_err(|error| {
            RemoteError::new(
                RemoteErrorCode::TransportClosed,
                format!("cannot write remote helper response: {error}"),
            )
        })
}

fn handshake(state: &ServerState, params: Value) -> Result<HelloResult, RemoteError> {
    let params: HelloParams = parse_params(params, METHOD_HELLO)?;
    if params.protocol_version != REMOTE_PROTOCOL_VERSION {
        return Err(RemoteError::new(
            RemoteErrorCode::ProtocolVersionMismatch,
            format!(
                "client speaks remote protocol {}, helper requires {}",
                params.protocol_version, REMOTE_PROTOCOL_VERSION
            ),
        )
        .with_hint("upgrade the daemon or the vp-remote-agent helper"));
    }
    if !constant_time_eq(params.auth_token.as_bytes(), state.auth_token.as_bytes()) {
        return Err(RemoteError::new(
            RemoteErrorCode::Unauthorized,
            "remote auth token was rejected",
        )
        .with_hint("configure the helper token and the project host token source"));
    }
    let canonical_error = |expected: &str| {
        RemoteError::new(
            RemoteErrorCode::RootMismatch,
            format!(
                "helper root is {}, but the client requested {}",
                state.jail.root().display(),
                expected
            ),
        )
        .with_hint("update the project execution host root")
    };
    if params.root.is_empty() {
        return Err(canonical_error("<empty>"));
    }
    if params.root != state.jail.root().to_string_lossy() {
        return Err(canonical_error(&params.root));
    }
    Ok(HelloResult {
        protocol_version: REMOTE_PROTOCOL_VERSION,
        helper_version: state.helper_version.clone(),
        root: state.jail.root().to_string_lossy().to_string(),
        capabilities: state.capabilities(),
        max_frame_bytes: state.max_frame_bytes as u64,
        env_allow: state.env_allow.clone(),
        server_started_at: state.started_at.clone(),
    })
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (left, right) in left.iter().zip(right.iter()) {
        difference |= left ^ right;
    }
    difference == 0
}

fn parse_params<T: DeserializeOwned>(params: Value, method: &str) -> Result<T, RemoteError> {
    serde_json::from_value(params).map_err(|error| {
        RemoteError::new(
            RemoteErrorCode::InvalidParams,
            format!("invalid {method} params: {error}"),
        )
    })
}

fn is_mutating(method: &str) -> bool {
    matches!(
        method,
        METHOD_WRITE_FILE
            | METHOD_MKDIR
            | METHOD_REMOVE
            | METHOD_RENAME
            | METHOD_EXEC
            | METHOD_GIT_WORKTREE_ADD
            | METHOD_GIT_WORKTREE_REMOVE
            | METHOD_GIT_BUNDLE_CREATE
            | METHOD_GIT_BUNDLE_DELETE
    )
}

fn operation_id_from(params: &Value, method: &str) -> Result<String, RemoteError> {
    let Some(operation_id) = params.get("operation_id").and_then(Value::as_str) else {
        return Err(RemoteError::new(
            RemoteErrorCode::OperationIdRequired,
            format!("{method} requires operation_id for idempotent retries"),
        )
        .with_hint("supply a stable id for this mutation; reuse it only for an exact retry"));
    };
    validate_operation_id(operation_id)?;
    Ok(operation_id.to_string())
}

async fn handle_request(
    state: &Arc<ServerState>,
    id: &Value,
    method: &str,
    params: Value,
) -> Result<Value, RemoteError> {
    if is_mutating(method) {
        handle_mutating(state, id, method, params).await
    } else {
        dispatch(state, method, params).await
    }
}

async fn handle_mutating(
    state: &Arc<ServerState>,
    id: &Value,
    method: &str,
    params: Value,
) -> Result<Value, RemoteError> {
    let operation_id = operation_id_from(&params, method)?;
    let request_hash = OperationLedger::request_hash(method, &params);
    let begin = {
        let mut ledger = state.ledger.lock().map_err(|_| {
            RemoteError::new(RemoteErrorCode::LedgerError, "operation ledger is poisoned")
        })?;
        ledger.begin(&operation_id, method, &request_hash)?
    };
    match begin {
        crate::ledger::LedgerBegin::Replay(replay) => {
            return match replay {
                LedgerReplay::Result(mut value) => {
                    if let Some(object) = value.as_object_mut() {
                        object.insert("replayed".to_string(), Value::Bool(true));
                    }
                    Ok(value)
                }
                LedgerReplay::Error(error) => Err(error),
            };
        }
        crate::ledger::LedgerBegin::Conflict => {
            return Err(RemoteError::new(
                RemoteErrorCode::OperationIdConflict,
                format!(
                    "operation_id {operation_id} was already used with a different {method} request"
                ),
            )
            .with_hint("use a new operation id for a new mutation"));
        }
        crate::ledger::LedgerBegin::InProgress => {
            return Err(RemoteError::new(
                RemoteErrorCode::OperationInProgress,
                format!("operation_id {operation_id} is already running"),
            ));
        }
        crate::ledger::LedgerBegin::Fresh => {}
    }

    let started_at = vp_protocol::now_rfc3339();
    let cancel = ExecControl::new();
    if method == METHOD_EXEC {
        state
            .cancels
            .lock()
            .map_err(|_| {
                RemoteError::new(RemoteErrorCode::Internal, "cancel registry is poisoned")
            })?
            .insert(value_key(id), cancel.clone());
    }
    let result = execute_mutation(state, method, params, cancel.clone()).await;
    if method == METHOD_EXEC
        && let Ok(mut cancels) = state.cancels.lock()
    {
        cancels.remove(&value_key(id));
    }
    match result {
        Ok(value) => {
            let entry = LedgerEntry {
                operation_id: operation_id.clone(),
                method: method.to_string(),
                request_hash,
                success: true,
                result: Some(value.clone()),
                error: None,
                started_at,
                completed_at: vp_protocol::now_rfc3339(),
            };
            if let Ok(mut ledger) = state.ledger.lock() {
                ledger.complete(entry)?;
            }
            Ok(value)
        }
        Err(error) => {
            if let Ok(mut ledger) = state.ledger.lock() {
                let _ = ledger.abandon(&operation_id, method, &request_hash, error.clone());
            }
            Err(error)
        }
    }
}

async fn execute_mutation(
    state: &Arc<ServerState>,
    method: &str,
    params: Value,
    cancel: ExecControl,
) -> Result<Value, RemoteError> {
    match method {
        METHOD_WRITE_FILE => {
            let params: WriteFileParams = parse_params(params, method)?;
            let result = write_file(&state.jail, &params).await?;
            to_value(result)
        }
        METHOD_MKDIR => {
            let params: MkdirParams = parse_params(params, method)?;
            let result = mkdir(&state.jail, &params).await?;
            to_value(result)
        }
        METHOD_REMOVE => {
            let params: RemoveParams = parse_params(params, method)?;
            let result = remove(&state.jail, &params).await?;
            to_value(result)
        }
        METHOD_RENAME => {
            let params: RenameParams = parse_params(params, method)?;
            let result = rename(&state.jail, &params).await?;
            to_value(result)
        }
        METHOD_EXEC => {
            let params: ExecParams = parse_params(params, method)?;
            validate_exec(&params)?;
            let result = run_exec(&state.jail, &params, &state.env_allow, &cancel).await?;
            to_value(result)
        }
        METHOD_GIT_WORKTREE_ADD => {
            let params: GitWorktreeAddParams = parse_params(params, method)?;
            let result = git::worktree_add(&state.jail, &params).await?;
            to_value(result)
        }
        METHOD_GIT_WORKTREE_REMOVE => {
            let params: GitWorktreeRemoveParams = parse_params(params, method)?;
            let result = git::worktree_remove(&state.jail, &params).await?;
            to_value(result)
        }
        METHOD_GIT_BUNDLE_CREATE => {
            let params: GitBundleCreateParams = parse_params(params, method)?;
            let mut bundles = state.bundles.lock().await;
            let result = bundle::create(&state.jail, &mut bundles, &params).await?;
            to_value(result)
        }
        METHOD_GIT_BUNDLE_DELETE => {
            let params: GitBundleDeleteParams = parse_params(params, method)?;
            let mut bundles = state.bundles.lock().await;
            let result = bundle::delete(&state.jail, &mut bundles, &params).await?;
            to_value(result)
        }
        _ => Err(RemoteError::new(
            RemoteErrorCode::UnsupportedOperation,
            format!("unsupported mutating method: {method}"),
        )),
    }
}

fn to_value<T: Serialize>(value: T) -> Result<Value, RemoteError> {
    serde_json::to_value(value).map_err(|error| {
        RemoteError::new(
            RemoteErrorCode::Internal,
            format!("cannot encode operation result: {error}"),
        )
    })
}

async fn dispatch(
    state: &Arc<ServerState>,
    method: &str,
    params: Value,
) -> Result<Value, RemoteError> {
    match method {
        METHOD_PROBE | METHOD_HEALTH => to_value(state.probe()),
        METHOD_STAT => {
            let params: StatParams = parse_params(params, method)?;
            to_value(stat(&state.jail, &params).await?)
        }
        METHOD_LIST_DIR => {
            let params: ListDirParams = parse_params(params, method)?;
            to_value(list_dir(&state.jail, &params).await?)
        }
        METHOD_READ_FILE => {
            let params: ReadFileParams = parse_params(params, method)?;
            to_value(read_file(&state.jail, &params).await?)
        }
        METHOD_GIT_STATUS => {
            let _: GitStatusParams = parse_params(params, method)?;
            to_value(git::status(&state.jail).await?)
        }
        METHOD_GIT_DIFF => {
            let params: GitDiffParams = parse_params(params, method)?;
            to_value(git::diff(&state.jail, &params).await?)
        }
        METHOD_GIT_LOG => {
            let params: GitLogParams = parse_params(params, method)?;
            to_value(git::log(&state.jail, &params).await?)
        }
        METHOD_GIT_REV_PARSE => {
            let params: GitRevParseParams = parse_params(params, method)?;
            to_value(git::rev_parse(&state.jail, &params).await?)
        }
        METHOD_GIT_WORKTREE_LIST => {
            let _: GitWorktreeListParams = parse_params(params, method)?;
            to_value(git::worktree_list(&state.jail).await?)
        }
        METHOD_GIT_BUNDLE_READ => {
            let params: GitBundleReadParams = parse_params(params, method)?;
            let mut bundles = state.bundles.lock().await;
            to_value(bundle::read(&mut bundles, &params).await?)
        }
        METHOD_LEDGER_STATS => {
            let params: LedgerStatsParams = parse_params(params, method)?;
            let limit = params.limit.clamp(1, 200) as usize;
            let ledger = state.ledger.lock().map_err(|_| {
                RemoteError::new(RemoteErrorCode::LedgerError, "operation ledger is poisoned")
            })?;
            let stats = ledger.stats();
            to_value(LedgerStatsResult {
                entries: stats.entries,
                max_entries: stats.max_entries,
                persisted: stats.persisted,
                file_bytes: stats.file_bytes,
                tail: ledger.tail(limit),
            })
        }
        _ => Err(RemoteError::new(
            RemoteErrorCode::UnsupportedOperation,
            format!("unsupported remote operation: {method}"),
        )
        .with_hint("upgrade both the helper and the daemon if this method is new")),
    }
}

fn cancel_operation(state: &ServerState, cancel: &CancelParams) {
    let key = value_key(&cancel.id);
    if let Ok(cancels) = state.cancels.lock()
        && let Some(control) = cancels.get(&key)
    {
        tracing::info!(
            operation = %key,
            reason = cancel.reason.as_deref().unwrap_or("client cancellation"),
            "cancelling remote command"
        );
        control.cancel();
    }
}

fn value_key(value: &Value) -> String {
    match value {
        Value::String(value) => format!("\"{value}\""),
        other => other.to_string(),
    }
}

async fn stat(jail: &PathJail, params: &StatParams) -> Result<StatResult, RemoteError> {
    let resolved = jail.resolve_final(&params.path)?;
    let metadata = tokio::fs::symlink_metadata(&resolved.absolute)
        .await
        .map_err(|error| {
            RemoteError::new(
                RemoteErrorCode::PathNotFound,
                format!("cannot stat {}: {error}", resolved.relative),
            )
        })?;
    let kind = if metadata.is_dir() {
        EntryKind::Directory
    } else if metadata.is_file() {
        EntryKind::File
    } else if metadata.file_type().is_symlink() {
        EntryKind::Symlink
    } else {
        EntryKind::Other
    };
    let symlink_target = if kind == EntryKind::Symlink {
        tokio::fs::read_link(&resolved.absolute)
            .await
            .ok()
            .map(|path| path.to_string_lossy().to_string())
    } else {
        None
    };
    Ok(StatResult {
        path: resolved.relative,
        kind,
        size: metadata.len(),
        mode: unix_mode(&metadata),
        modified_unix_ms: modified_millis(&metadata),
        symlink_target,
    })
}

async fn list_dir(jail: &PathJail, params: &ListDirParams) -> Result<ListDirResult, RemoteError> {
    if params.limit == 0 || params.limit > MAX_DIR_ENTRIES {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            format!("list limit must be 1..={MAX_DIR_ENTRIES}"),
        ));
    }
    let resolved = jail.resolve_existing(&params.path)?;
    if !resolved.absolute.is_dir() {
        return Err(RemoteError::new(
            RemoteErrorCode::NotADirectory,
            format!("not a directory: {}", resolved.relative),
        ));
    }
    let mut entries = Vec::new();
    let mut reader = tokio::fs::read_dir(&resolved.absolute)
        .await
        .map_err(|error| {
            RemoteError::new(
                RemoteErrorCode::PermissionDenied,
                format!("cannot list {}: {error}", resolved.relative),
            )
        })?;
    while let Some(entry) = reader.next_entry().await.map_err(|error| {
        RemoteError::new(
            RemoteErrorCode::Internal,
            format!("cannot read directory entry: {error}"),
        )
    })? {
        let name = entry.file_name().to_string_lossy().to_string();
        if !params.include_hidden && name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let metadata = tokio::fs::symlink_metadata(&path).await.map_err(|error| {
            RemoteError::new(
                RemoteErrorCode::Internal,
                format!("cannot inspect {}: {error}", path.display()),
            )
        })?;
        let file_type = metadata.file_type();
        let kind = if file_type.is_symlink() {
            EntryKind::Symlink
        } else if file_type.is_dir() {
            EntryKind::Directory
        } else if file_type.is_file() {
            EntryKind::File
        } else {
            EntryKind::Other
        };
        let symlink_target = if kind == EntryKind::Symlink {
            tokio::fs::read_link(&path)
                .await
                .ok()
                .map(|target| target.to_string_lossy().to_string())
        } else {
            None
        };
        let relative = jail
            .relative_path(&path)
            .unwrap_or_else(|| resolved.relative.clone());
        entries.push(DirEntry {
            name,
            path: relative,
            kind,
            size: metadata.len(),
            mode: unix_mode(&metadata),
            symlink_target,
        });
    }
    entries.sort_by(|left, right| {
        entry_rank(&left.kind)
            .cmp(&entry_rank(&right.kind))
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.name.cmp(&right.name))
    });
    let total = entries.len() as u32;
    let cursor = params.cursor.min(total) as usize;
    let limit = params.limit as usize;
    let page = entries
        .into_iter()
        .skip(cursor)
        .take(limit)
        .collect::<Vec<_>>();
    let next = (cursor + page.len() < total as usize).then_some((cursor + page.len()) as u32);
    Ok(ListDirResult {
        path: resolved.relative,
        entries: page,
        total,
        next_cursor: next,
        truncated: next.is_some(),
    })
}

fn entry_rank(kind: &EntryKind) -> u8 {
    match kind {
        EntryKind::Directory => 0,
        EntryKind::File => 1,
        EntryKind::Symlink => 2,
        EntryKind::Other => 3,
    }
}

async fn read_file(
    jail: &PathJail,
    params: &ReadFileParams,
) -> Result<ReadFileResult, RemoteError> {
    if params.limit == 0 || params.limit > MAX_READ_BYTES {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            format!("read limit must be 1..={MAX_READ_BYTES}"),
        ));
    }
    let resolved = jail.resolve_existing(&params.path)?;
    let metadata = tokio::fs::metadata(&resolved.absolute)
        .await
        .map_err(|error| {
            RemoteError::new(
                RemoteErrorCode::PathNotFound,
                format!("cannot read {}: {error}", resolved.relative),
            )
        })?;
    if !metadata.is_file() {
        return Err(RemoteError::new(
            RemoteErrorCode::NotAFile,
            format!("not a regular file: {}", resolved.relative),
        ));
    }
    let size = metadata.len();
    if params.offset > size {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            format!(
                "read offset {} exceeds file size {} for {}",
                params.offset, size, resolved.relative
            ),
        ));
    }
    let path = resolved.absolute.clone();
    let sha256 = tokio::task::spawn_blocking(move || sha256_file(&path))
        .await
        .map_err(|error| {
            RemoteError::new(
                RemoteErrorCode::Internal,
                format!("hash task failed: {error}"),
            )
        })??;
    let mut file = tokio::fs::File::open(&resolved.absolute)
        .await
        .map_err(|error| {
            RemoteError::new(
                RemoteErrorCode::PermissionDenied,
                format!("cannot open {}: {error}", resolved.relative),
            )
        })?;
    file.seek(std::io::SeekFrom::Start(params.offset))
        .await
        .map_err(|error| {
            RemoteError::new(
                RemoteErrorCode::Internal,
                format!("cannot seek {}: {error}", resolved.relative),
            )
        })?;
    let mut bytes = Vec::new();
    file.take(params.limit)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| {
            RemoteError::new(
                RemoteErrorCode::Internal,
                format!("cannot read {}: {error}", resolved.relative),
            )
        })?;
    let bytes_read = bytes.len() as u64;
    let (encoding, text, base64) = match params.encoding {
        ReadEncoding::Auto => match String::from_utf8(bytes) {
            Ok(text) => (ReadEncoding::Utf8, Some(text), None),
            Err(error) => (
                ReadEncoding::Base64,
                None,
                Some(base64::engine::general_purpose::STANDARD.encode(error.into_bytes())),
            ),
        },
        ReadEncoding::Utf8 => match String::from_utf8(bytes) {
            Ok(text) => (ReadEncoding::Utf8, Some(text), None),
            Err(_) => {
                return Err(RemoteError::new(
                    RemoteErrorCode::InvalidEncoding,
                    format!(
                        "{} is not valid UTF-8 at the requested offset",
                        resolved.relative
                    ),
                )
                .with_hint("use encoding=base64 or a byte offset on a character boundary"));
            }
        },
        ReadEncoding::Base64 => (
            ReadEncoding::Base64,
            None,
            Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
        ),
    };
    Ok(ReadFileResult {
        path: resolved.relative,
        sha256,
        size,
        offset: params.offset,
        bytes_read,
        encoding,
        text,
        base64,
        truncated: params.offset.saturating_add(bytes_read) < size,
    })
}

fn sha256_file(path: &Path) -> Result<String, RemoteError> {
    let mut file = std::fs::File::open(path).map_err(|error| {
        RemoteError::new(
            RemoteErrorCode::PermissionDenied,
            format!("cannot open {} for hashing: {error}", path.display()),
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let count = std::io::Read::read(&mut file, &mut buffer).map_err(|error| {
            RemoteError::new(
                RemoteErrorCode::Internal,
                format!("cannot hash {}: {error}", path.display()),
            )
        })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

async fn write_file(
    jail: &PathJail,
    params: &WriteFileParams,
) -> Result<WriteFileResult, RemoteError> {
    let bytes = match params.encoding {
        WriteEncoding::Utf8 => params.content.as_bytes().to_vec(),
        WriteEncoding::Base64 => base64::engine::general_purpose::STANDARD
            .decode(params.content.as_bytes())
            .map_err(|error| {
                RemoteError::new(
                    RemoteErrorCode::InvalidEncoding,
                    format!("write content is not valid base64: {error}"),
                )
            })?,
    };
    if bytes.len() as u64 > MAX_WRITE_BYTES {
        return Err(RemoteError::new(
            RemoteErrorCode::FileTooLarge,
            format!(
                "write content is {} bytes; the limit is {MAX_WRITE_BYTES}",
                bytes.len()
            ),
        ));
    }
    let expected = params
        .expected_sha256
        .as_deref()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty());
    if let Some(expected) = expected.as_deref()
        && (expected.len() != 64
            || !expected
                .chars()
                .all(|character| character.is_ascii_hexdigit()))
    {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            "expected_sha256 must be a 64-character hex SHA-256",
        ));
    }
    let resolved = jail.resolve_write(&params.path)?;
    let target = resolved.target.clone();
    if let Some(parent) = target.parent()
        && !parent.exists()
    {
        if params.create_parents {
            tokio::fs::create_dir_all(parent).await.map_err(|error| {
                RemoteError::new(
                    RemoteErrorCode::Internal,
                    format!("cannot create {}: {error}", parent.display()),
                )
            })?;
        } else {
            return Err(RemoteError::new(
                RemoteErrorCode::PathNotFound,
                format!("parent directory for {} does not exist", params.path),
            )
            .with_hint("set create_parents=true"));
        }
    }
    let existing_sha = if resolved.exists {
        let metadata = tokio::fs::metadata(&target).await.map_err(|error| {
            RemoteError::new(
                RemoteErrorCode::Internal,
                format!("cannot inspect {}: {error}", params.path),
            )
        })?;
        if !metadata.is_file() {
            return Err(RemoteError::new(
                RemoteErrorCode::NotAFile,
                format!("write target is not a regular file: {}", params.path),
            ));
        }
        let path = target.clone();
        Some(
            tokio::task::spawn_blocking(move || sha256_file(&path))
                .await
                .map_err(|error| {
                    RemoteError::new(
                        RemoteErrorCode::Internal,
                        format!("hash task failed: {error}"),
                    )
                })??,
        )
    } else {
        None
    };
    match (existing_sha.as_deref(), expected.as_deref()) {
        (Some(current), Some(expected)) if current != expected => {
            return Err(RemoteError::new(
                RemoteErrorCode::ShaMismatch,
                format!("{} changed since it was read", params.path),
            )
            .with_hint("read the file again and retry with the current sha256"));
        }
        (Some(_), None) if !params.allow_overwrite => {
            return Err(RemoteError::new(
                RemoteErrorCode::PreconditionRequired,
                format!(
                    "{} already exists; expected_sha256 is required",
                    params.path
                ),
            )
            .with_hint(
                "read the file first, or set allow_overwrite=true for an intentional replacement",
            ));
        }
        (None, Some(expected)) if expected != EMPTY_SHA256 => {
            return Err(RemoteError::new(
                RemoteErrorCode::ShaMismatch,
                format!(
                    "{} does not exist, but expected_sha256 was supplied",
                    params.path
                ),
            )
            .with_hint("omit expected_sha256 when creating a file"));
        }
        _ => {}
    }
    let created = existing_sha.is_none();
    let replaced = !created;
    let sha256 = {
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        format!("{:x}", hasher.finalize())
    };
    let temp_name = format!(
        ".{}.vp-remote-{}-{}.tmp",
        target
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string()),
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::SeqCst)
    );
    let temp = target
        .parent()
        .map(|parent| parent.join(&temp_name))
        .ok_or_else(|| {
            RemoteError::new(RemoteErrorCode::InvalidParams, "write target has no parent")
        })?;
    let write_result = async {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .await
            .map_err(|error| {
                RemoteError::new(
                    RemoteErrorCode::Internal,
                    format!("cannot create atomic temp file: {error}"),
                )
            })?;
        file.write_all(&bytes).await.map_err(|error| {
            RemoteError::new(
                RemoteErrorCode::Internal,
                format!("cannot write atomic temp file: {error}"),
            )
        })?;
        file.sync_all().await.map_err(|error| {
            RemoteError::new(
                RemoteErrorCode::Internal,
                format!("cannot sync atomic temp file: {error}"),
            )
        })?;
        if let Some(mode) = params.mode {
            if mode > 0o777 {
                return Err(RemoteError::new(
                    RemoteErrorCode::InvalidParams,
                    "mode must be between 0 and 0777",
                ));
            }
            let permissions = std::fs::Permissions::from_mode(mode);
            tokio::fs::set_permissions(&temp, permissions)
                .await
                .map_err(|error| {
                    RemoteError::new(
                        RemoteErrorCode::Internal,
                        format!("cannot set file mode: {error}"),
                    )
                })?;
        }
        tokio::fs::rename(&temp, &target).await.map_err(|error| {
            RemoteError::new(
                RemoteErrorCode::Internal,
                format!("cannot atomically replace {}: {error}", params.path),
            )
        })
    }
    .await;
    if let Err(error) = write_result {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(error);
    }
    Ok(WriteFileResult {
        path: params.path.clone(),
        sha256,
        size: bytes.len() as u64,
        created,
        replaced,
        operation_id: params.operation_id.clone(),
        replayed: false,
    })
}

async fn mkdir(jail: &PathJail, params: &MkdirParams) -> Result<MkdirResult, RemoteError> {
    let resolved = jail.resolve_write(&params.path)?;
    if resolved.exists {
        let metadata = tokio::fs::metadata(&resolved.target)
            .await
            .map_err(|error| {
                RemoteError::new(
                    RemoteErrorCode::Internal,
                    format!("cannot inspect {}: {error}", params.path),
                )
            })?;
        if metadata.is_dir() {
            return Ok(MkdirResult {
                path: params.path.clone(),
                created: false,
                operation_id: params.operation_id.clone(),
                replayed: false,
            });
        }
        return Err(RemoteError::new(
            RemoteErrorCode::AlreadyExists,
            format!("path exists and is not a directory: {}", params.path),
        ));
    }
    let result = if params.recursive {
        tokio::fs::create_dir_all(&resolved.target).await
    } else {
        tokio::fs::create_dir(&resolved.target).await
    };
    result.map_err(|error| {
        RemoteError::new(
            if error.kind() == std::io::ErrorKind::NotFound {
                RemoteErrorCode::PathNotFound
            } else if error.kind() == std::io::ErrorKind::AlreadyExists {
                RemoteErrorCode::AlreadyExists
            } else {
                RemoteErrorCode::Internal
            },
            format!("cannot create directory {}: {error}", params.path),
        )
    })?;
    Ok(MkdirResult {
        path: params.path.clone(),
        created: true,
        operation_id: params.operation_id.clone(),
        replayed: false,
    })
}

async fn remove(jail: &PathJail, params: &RemoveParams) -> Result<RemoveResult, RemoteError> {
    let resolved = match jail.resolve_mutation(&params.path) {
        Ok(resolved) => resolved,
        Err(error) if params.missing_ok && matches!(error.code, RemoteErrorCode::PathNotFound) => {
            return Ok(RemoveResult {
                path: params.path.clone(),
                removed: false,
                operation_id: params.operation_id.clone(),
                replayed: false,
            });
        }
        Err(error) => return Err(error),
    };
    let metadata = match tokio::fs::symlink_metadata(&resolved.absolute).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && params.missing_ok => {
            return Ok(RemoveResult {
                path: params.path.clone(),
                removed: false,
                operation_id: params.operation_id.clone(),
                replayed: false,
            });
        }
        Err(error) => {
            return Err(RemoteError::new(
                RemoteErrorCode::PathNotFound,
                format!("cannot inspect {}: {error}", params.path),
            ));
        }
    };
    let result = if metadata.is_dir() {
        if params.recursive {
            tokio::fs::remove_dir_all(&resolved.absolute).await
        } else {
            tokio::fs::remove_dir(&resolved.absolute).await
        }
    } else {
        tokio::fs::remove_file(&resolved.absolute).await
    };
    result.map_err(|error| {
        RemoteError::new(
            if error.kind() == std::io::ErrorKind::DirectoryNotEmpty {
                RemoteErrorCode::DirectoryNotEmpty
            } else {
                RemoteErrorCode::Internal
            },
            format!("cannot remove {}: {error}", params.path),
        )
    })?;
    Ok(RemoveResult {
        path: params.path.clone(),
        removed: true,
        operation_id: params.operation_id.clone(),
        replayed: false,
    })
}

async fn rename(jail: &PathJail, params: &RenameParams) -> Result<RenameResult, RemoteError> {
    let from = jail.resolve_mutation(&params.from)?;
    let to = jail.resolve_write(&params.to)?;
    if to.exists && !params.overwrite {
        return Err(RemoteError::new(
            RemoteErrorCode::DestinationExists,
            format!("destination already exists: {}", params.to),
        )
        .with_hint("set overwrite=true to replace it atomically"));
    }
    if let Some(parent) = to.target.parent()
        && !parent.exists()
    {
        if params.create_parents {
            tokio::fs::create_dir_all(parent).await.map_err(|error| {
                RemoteError::new(
                    RemoteErrorCode::Internal,
                    format!("cannot create {}: {error}", parent.display()),
                )
            })?;
        } else {
            return Err(RemoteError::new(
                RemoteErrorCode::PathNotFound,
                format!("destination parent does not exist: {}", params.to),
            )
            .with_hint("set create_parents=true"));
        }
    }
    tokio::fs::rename(&from.absolute, &to.target)
        .await
        .map_err(|error| {
            RemoteError::new(
                RemoteErrorCode::Internal,
                format!("cannot rename {} to {}: {error}", params.from, params.to),
            )
        })?;
    Ok(RenameResult {
        from: params.from.clone(),
        to: params.to.clone(),
        operation_id: params.operation_id.clone(),
        replayed: false,
    })
}

fn unix_mode(metadata: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.mode() & 0o7777
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        0
    }
}

fn modified_millis(metadata: &std::fs::Metadata) -> Option<i64> {
    let modified = metadata.modified().ok()?;
    let duration = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(duration.as_millis().min(i64::MAX as u128) as i64)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;
    use tokio::io::duplex;

    use super::*;
    use crate::protocol::{HelloParams, WriteEncoding};

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn config(dir: &TempDir) -> ServerConfig {
        ServerConfig::new(dir.path(), TOKEN).unwrap()
    }

    async fn round_trip<R, W>(reader: R, writer: W, config: ServerConfig) -> Result<(), RemoteError>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        serve(reader, writer, config).await
    }

    #[test]
    fn server_requires_a_real_token() {
        let error = ServerConfig::new("/tmp", "short").unwrap_err();
        assert_eq!(error.code, RemoteErrorCode::InvalidParams);
    }

    #[test]
    fn handshake_rejects_wrong_token_and_version() {
        let dir = TempDir::new().unwrap();
        let state = ServerState::new(config(&dir)).unwrap();
        let mut params = HelloParams {
            protocol_version: REMOTE_PROTOCOL_VERSION,
            client_version: "test".into(),
            auth_token: "wrong-token-wrong-token".into(),
            root: dir
                .path()
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .to_string(),
        };
        let error = handshake(&state, serde_json::to_value(&params).unwrap()).unwrap_err();
        assert_eq!(error.code, RemoteErrorCode::Unauthorized);
        params.auth_token = TOKEN.into();
        params.protocol_version = 2;
        let error = handshake(&state, serde_json::to_value(&params).unwrap()).unwrap_err();
        assert_eq!(error.code, RemoteErrorCode::ProtocolVersionMismatch);
        params.protocol_version = REMOTE_PROTOCOL_VERSION;
        params.root = "/wrong".into();
        let error = handshake(&state, serde_json::to_value(&params).unwrap()).unwrap_err();
        assert_eq!(error.code, RemoteErrorCode::RootMismatch);
        params.root = dir
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let hello = handshake(&state, serde_json::to_value(&params).unwrap()).unwrap();
        assert!(hello.missing_required().is_empty());
    }

    #[tokio::test]
    async fn files_are_written_atomically_and_sha_checked() {
        let dir = TempDir::new().unwrap();
        let jail = PathJail::new(dir.path()).unwrap();
        let first = write_file(
            &jail,
            &WriteFileParams {
                path: "src/a.txt".into(),
                content: "one".into(),
                encoding: WriteEncoding::Utf8,
                expected_sha256: None,
                create_parents: true,
                allow_overwrite: false,
                mode: Some(0o644),
                operation_id: Some("op-1".into()),
            },
        )
        .await
        .unwrap();
        assert!(first.created);
        let error = write_file(
            &jail,
            &WriteFileParams {
                path: "src/a.txt".into(),
                content: "two".into(),
                encoding: WriteEncoding::Utf8,
                expected_sha256: None,
                create_parents: true,
                allow_overwrite: false,
                mode: None,
                operation_id: Some("op-2".into()),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, RemoteErrorCode::PreconditionRequired);
        let error = write_file(
            &jail,
            &WriteFileParams {
                path: "src/a.txt".into(),
                content: "two".into(),
                encoding: WriteEncoding::Utf8,
                expected_sha256: Some("0".repeat(64)),
                create_parents: true,
                allow_overwrite: false,
                mode: None,
                operation_id: Some("op-3".into()),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, RemoteErrorCode::ShaMismatch);
        let second = write_file(
            &jail,
            &WriteFileParams {
                path: "src/a.txt".into(),
                content: "two".into(),
                encoding: WriteEncoding::Utf8,
                expected_sha256: Some(first.sha256),
                create_parents: true,
                allow_overwrite: false,
                mode: None,
                operation_id: Some("op-4".into()),
            },
        )
        .await
        .unwrap();
        assert!(second.replaced);
        assert_eq!(
            fs::read_to_string(dir.path().join("src/a.txt")).unwrap(),
            "two"
        );
    }

    #[tokio::test]
    async fn read_file_supports_offsets_and_base64() {
        let dir = TempDir::new().unwrap();
        let jail = PathJail::new(dir.path()).unwrap();
        fs::write(dir.path().join("bin"), [0u8, 159, 146, 150]).unwrap();
        let result = read_file(
            &jail,
            &ReadFileParams {
                path: "bin".into(),
                offset: 0,
                limit: 2,
                encoding: ReadEncoding::Auto,
            },
        )
        .await
        .unwrap();
        assert_eq!(result.encoding, ReadEncoding::Base64);
        assert_eq!(result.base64.as_deref(), Some("AJ8="));
        assert!(result.truncated);
        let text = read_file(
            &jail,
            &ReadFileParams {
                path: "bin".into(),
                offset: 1,
                limit: 2,
                encoding: ReadEncoding::Utf8,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(text.code, RemoteErrorCode::InvalidEncoding);
    }

    #[tokio::test]
    async fn write_rejects_directories_and_invalid_sha() {
        let dir = TempDir::new().unwrap();
        let jail = PathJail::new(dir.path()).unwrap();
        fs::create_dir(dir.path().join("d")).unwrap();
        let directory = write_file(
            &jail,
            &WriteFileParams {
                path: "d".into(),
                content: "x".into(),
                encoding: WriteEncoding::Utf8,
                expected_sha256: None,
                create_parents: true,
                allow_overwrite: false,
                mode: None,
                operation_id: Some("op".into()),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(directory.code, RemoteErrorCode::NotAFile);
        let invalid = write_file(
            &jail,
            &WriteFileParams {
                path: "a".into(),
                content: "x".into(),
                encoding: WriteEncoding::Utf8,
                expected_sha256: Some("not-a-sha".into()),
                create_parents: true,
                allow_overwrite: false,
                mode: None,
                operation_id: Some("op-2".into()),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(invalid.code, RemoteErrorCode::InvalidParams);
    }

    #[tokio::test]
    async fn write_base64_limits_and_mode_are_applied() {
        let dir = TempDir::new().unwrap();
        let jail = PathJail::new(dir.path()).unwrap();
        let result = write_file(
            &jail,
            &WriteFileParams {
                path: "bin".into(),
                content: base64::engine::general_purpose::STANDARD.encode([0u8, 1, 255]),
                encoding: WriteEncoding::Base64,
                expected_sha256: None,
                create_parents: true,
                allow_overwrite: false,
                mode: Some(0o640),
                operation_id: Some("op".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(result.size, 3);
        assert_eq!(fs::read(dir.path().join("bin")).unwrap(), [0u8, 1, 255]);
        assert_eq!(
            fs::metadata(dir.path().join("bin"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
        let invalid = write_file(
            &jail,
            &WriteFileParams {
                path: "bad".into(),
                content: "***".into(),
                encoding: WriteEncoding::Base64,
                expected_sha256: None,
                create_parents: true,
                allow_overwrite: false,
                mode: None,
                operation_id: Some("op-2".into()),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(invalid.code, RemoteErrorCode::InvalidEncoding);
    }

    #[tokio::test]
    async fn mkdir_and_rename_validate_conflicts() {
        let dir = TempDir::new().unwrap();
        let jail = PathJail::new(dir.path()).unwrap();
        let created = mkdir(
            &jail,
            &MkdirParams {
                path: "a/b".into(),
                recursive: true,
                operation_id: Some("m1".into()),
            },
        )
        .await
        .unwrap();
        assert!(created.created);
        let existing = mkdir(
            &jail,
            &MkdirParams {
                path: "a/b".into(),
                recursive: false,
                operation_id: Some("m2".into()),
            },
        )
        .await
        .unwrap();
        assert!(!existing.created);
        fs::write(dir.path().join("a/b/one"), "one").unwrap();
        fs::write(dir.path().join("a/b/two"), "two").unwrap();
        let conflict = rename(
            &jail,
            &RenameParams {
                from: "a/b/one".into(),
                to: "a/b/two".into(),
                overwrite: false,
                create_parents: false,
                operation_id: Some("r1".into()),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(conflict.code, RemoteErrorCode::DestinationExists);
        rename(
            &jail,
            &RenameParams {
                from: "a/b/one".into(),
                to: "a/b/two".into(),
                overwrite: true,
                create_parents: false,
                operation_id: Some("r2".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("a/b/two")).unwrap(),
            "one"
        );
    }

    #[tokio::test]
    async fn remove_respects_recursive_and_missing_ok() {
        let dir = TempDir::new().unwrap();
        let jail = PathJail::new(dir.path()).unwrap();
        fs::create_dir_all(dir.path().join("d/sub")).unwrap();
        fs::write(dir.path().join("d/sub/file"), "x").unwrap();
        let error = remove(
            &jail,
            &RemoveParams {
                path: "d".into(),
                recursive: false,
                missing_ok: false,
                operation_id: Some("rm1".into()),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, RemoteErrorCode::DirectoryNotEmpty);
        let missing = remove(
            &jail,
            &RemoveParams {
                path: "missing".into(),
                recursive: false,
                missing_ok: true,
                operation_id: Some("rm2".into()),
            },
        )
        .await
        .unwrap();
        assert!(!missing.removed);
        let removed = remove(
            &jail,
            &RemoveParams {
                path: "d".into(),
                recursive: true,
                missing_ok: false,
                operation_id: Some("rm3".into()),
            },
        )
        .await
        .unwrap();
        assert!(removed.removed);
    }

    #[tokio::test]
    async fn read_and_list_bounds_are_enforced() {
        let dir = TempDir::new().unwrap();
        let jail = PathJail::new(dir.path()).unwrap();
        fs::write(dir.path().join("f"), "hello").unwrap();
        let offset = read_file(
            &jail,
            &ReadFileParams {
                path: "f".into(),
                offset: 99,
                limit: 10,
                encoding: ReadEncoding::Auto,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(offset.code, RemoteErrorCode::InvalidParams);
        let zero = read_file(
            &jail,
            &ReadFileParams {
                path: "f".into(),
                offset: 0,
                limit: 0,
                encoding: ReadEncoding::Auto,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(zero.code, RemoteErrorCode::InvalidParams);
        let list = list_dir(
            &jail,
            &ListDirParams {
                path: ".".into(),
                include_hidden: true,
                limit: 0,
                cursor: 0,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(list.code, RemoteErrorCode::InvalidParams);
        let escape = list_dir(
            &jail,
            &ListDirParams {
                path: "../".into(),
                include_hidden: true,
                limit: 10,
                cursor: 0,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(escape.code, RemoteErrorCode::PathEscape);
    }

    #[tokio::test]
    async fn list_dir_paginates_and_filters_hidden() {
        let dir = TempDir::new().unwrap();
        let jail = PathJail::new(dir.path()).unwrap();
        fs::create_dir(dir.path().join("b")).unwrap();
        fs::write(dir.path().join("a.txt"), "a").unwrap();
        fs::write(dir.path().join(".hidden"), "h").unwrap();
        let page = list_dir(
            &jail,
            &ListDirParams {
                path: ".".into(),
                include_hidden: false,
                limit: 1,
                cursor: 0,
            },
        )
        .await
        .unwrap();
        assert_eq!(page.total, 2);
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].kind, EntryKind::Directory);
        assert_eq!(page.next_cursor, Some(1));
        let second = list_dir(
            &jail,
            &ListDirParams {
                path: ".".into(),
                include_hidden: false,
                limit: 10,
                cursor: 1,
            },
        )
        .await
        .unwrap();
        assert_eq!(second.entries[0].name, "a.txt");
        assert!(second.next_cursor.is_none());
    }

    #[tokio::test]
    async fn remove_does_not_follow_the_final_symlink() {
        let dir = TempDir::new().unwrap();
        let jail = PathJail::new(dir.path()).unwrap();
        fs::write(dir.path().join("target"), "keep").unwrap();
        std::os::unix::fs::symlink("target", dir.path().join("link")).unwrap();
        remove(
            &jail,
            &RemoveParams {
                path: "link".into(),
                recursive: false,
                missing_ok: false,
                operation_id: Some("op".into()),
            },
        )
        .await
        .unwrap();
        assert!(!dir.path().join("link").exists());
        assert!(dir.path().join("target").exists());
    }

    #[tokio::test]
    async fn serve_refuses_operations_before_hello() {
        use crate::framing::{read_frame, write_frame};
        use tokio::io::BufReader;
        let dir = TempDir::new().unwrap();
        let (mut client, server) = duplex(64 * 1024);
        let (server_reader, server_writer) = tokio::io::split(server);
        let handle = tokio::spawn(round_trip(server_reader, server_writer, config(&dir)));
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "stat",
            "params": {"path": "."}
        });
        write_frame(&mut client, &request, 64 * 1024).await.unwrap();
        let mut reader = BufReader::new(client);
        let frame = read_frame(&mut reader, 64 * 1024).await.unwrap().unwrap();
        let response: RpcResponse = serde_json::from_slice(&frame).unwrap();
        let error = response.error.unwrap();
        let data: RemoteError = serde_json::from_value(error.data.unwrap()).unwrap();
        assert_eq!(data.code, RemoteErrorCode::Unauthorized);
        assert!(handle.await.unwrap().is_err());
    }
}

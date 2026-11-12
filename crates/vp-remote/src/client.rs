//! JSON-RPC client for a remote helper.
//!
//! The client owns one child process, one bounded writer, and one reader task.
//! Requests are multiplexed by id; a timeout always sends a `cancel`
//! notification so the helper can kill a running process group. There is no
//! silent fallback to local tools when the transport fails.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use vp_protocol::rpc::{RpcNotification, RpcRequest, RpcResponse};

use crate::framing::{read_frame, write_frame};
use crate::protocol::*;
use crate::transport::{RemoteProcess, RemoteTransport};

type PendingMap =
    Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<Result<Value, RemoteError>>>>>;

#[derive(Debug, Clone)]
pub struct RemoteClientConfig {
    pub request_timeout: Duration,
    pub handshake_timeout: Duration,
    pub max_frame_bytes: usize,
}

impl Default for RemoteClientConfig {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(120),
            handshake_timeout: Duration::from_secs(20),
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES as usize,
        }
    }
}

type ClientWriter = Box<dyn AsyncWrite + Unpin + Send>;

pub struct RemoteClient {
    writer: Arc<AsyncMutex<Option<ClientWriter>>>,
    process: Option<Arc<RemoteProcess>>,
    pending: PendingMap,
    next_id: AtomicU64,
    closed: Arc<AtomicBool>,
    reader: AsyncMutex<Option<JoinHandle<()>>>,
    config: RemoteClientConfig,
    hello: HelloResult,
    description: String,
}

impl RemoteClient {
    pub async fn connect(
        transport: &dyn RemoteTransport,
        hello: HelloParams,
        config: RemoteClientConfig,
    ) -> Result<Self, RemoteError> {
        let process = transport.spawn().await?;
        let description = process.description.clone();
        connect_inner(process, hello, config, description).await
    }
}

/// Construct a client around an already-connected process. Kept private to the
/// module so the public API can only connect through the handshake path.
async fn connect_inner(
    mut process: RemoteProcess,
    hello: HelloParams,
    config: RemoteClientConfig,
    description: String,
) -> Result<RemoteClient, RemoteError> {
    let stdin = process.take_stdin().ok_or_else(|| {
        RemoteError::new(
            RemoteErrorCode::TransportClosed,
            format!("{description} did not expose stdin"),
        )
    })?;
    let stdout = process.take_stdout().ok_or_else(|| {
        RemoteError::new(
            RemoteErrorCode::TransportClosed,
            format!("{description} did not expose stdout"),
        )
    })?;
    connect_io(
        stdout,
        Box::new(stdin),
        hello,
        config,
        description,
        Some(Arc::new(process)),
    )
    .await
}

/// Connect a client to any async reader/writer pair. The server side can be the
/// packaged helper over a pipe, an in-process `serve` future over a duplex for
/// contract tests, or an embedding transport. The same bounded framing,
/// handshake, capability, and cancellation rules apply.
pub async fn connect_stream<R, W>(
    reader: R,
    writer: W,
    hello: HelloParams,
    config: RemoteClientConfig,
) -> Result<RemoteClient, RemoteError>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    connect_io(
        reader,
        Box::new(writer),
        hello,
        config,
        "in-process remote helper stream".to_string(),
        None,
    )
    .await
}

async fn connect_io<R>(
    stdout: R,
    stdin: ClientWriter,
    hello: HelloParams,
    config: RemoteClientConfig,
    description: String,
    process: Option<Arc<RemoteProcess>>,
) -> Result<RemoteClient, RemoteError>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
    let closed = Arc::new(AtomicBool::new(false));
    let writer = Arc::new(AsyncMutex::new(Some(stdin)));
    let max_frame_bytes = config.max_frame_bytes.clamp(1024, MAX_FRAME_BYTES as usize);
    let reader_pending = pending.clone();
    let reader_closed = closed.clone();
    let reader = tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        loop {
            match read_frame(&mut reader, max_frame_bytes).await {
                Ok(Some(frame)) => match serde_json::from_slice::<RpcResponse>(&frame) {
                    Ok(response) => {
                        if let Some(id) = response.id.as_ref() {
                            let key = value_key(id);
                            let sender = reader_pending
                                .lock()
                                .ok()
                                .and_then(|mut pending| pending.remove(&key));
                            if let Some(sender) = sender {
                                let outcome = match (response.result, response.error) {
                                    (Some(result), None) => Ok(result),
                                    (_, Some(error)) => Err(RemoteError::from(error)),
                                    (None, None) => Err(RemoteError::new(
                                        RemoteErrorCode::InvalidRequest,
                                        "helper returned neither result nor error",
                                    )),
                                };
                                let _ = sender.send(outcome);
                            }
                        } else if let Ok(notification) =
                            serde_json::from_slice::<RpcNotification>(&frame)
                            && notification.method == "event"
                        {
                            tracing::debug!(
                                target: "vp_remote",
                                event = %notification.params,
                                "remote helper event"
                            );
                        }
                    }
                    Err(error) => {
                        fail_pending(
                            &reader_pending,
                            RemoteError::new(
                                RemoteErrorCode::InvalidResponse,
                                format!("malformed JSON-RPC response: {error}"),
                            ),
                        );
                        reader_closed.store(true, Ordering::SeqCst);
                        break;
                    }
                },
                Ok(None) => {
                    fail_pending(
                        &reader_pending,
                        RemoteError::new(
                            RemoteErrorCode::TransportClosed,
                            "remote helper closed stdout",
                        ),
                    );
                    reader_closed.store(true, Ordering::SeqCst);
                    break;
                }
                Err(error) => {
                    fail_pending(
                        &reader_pending,
                        RemoteError::new(
                            RemoteErrorCode::TransportClosed,
                            format!("remote helper stream failed: {error}"),
                        ),
                    );
                    reader_closed.store(true, Ordering::SeqCst);
                    break;
                }
            }
        }
    });

    let mut client = RemoteClient {
        writer,
        process,
        pending,
        next_id: AtomicU64::new(1),
        closed,
        reader: AsyncMutex::new(Some(reader)),
        config,
        hello: HelloResult {
            protocol_version: 0,
            helper_version: String::new(),
            root: String::new(),
            capabilities: Vec::new(),
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
            env_allow: Vec::new(),
            server_started_at: String::new(),
        },
        description,
    };

    let response: HelloResult = client
        .call(METHOD_HELLO, &hello, client.config.handshake_timeout)
        .await?;
    if response.protocol_version != REMOTE_PROTOCOL_VERSION {
        client.kill().await;
        return Err(RemoteError::new(
            RemoteErrorCode::ProtocolVersionMismatch,
            format!(
                "helper speaks remote protocol {}, daemon requires {}",
                response.protocol_version, REMOTE_PROTOCOL_VERSION
            ),
        )
        .with_hint("install a matching vp-remote-agent version"));
    }
    if !hello.root.is_empty() && response.root != hello.root {
        client.kill().await;
        return Err(RemoteError::new(
            RemoteErrorCode::RootMismatch,
            format!(
                "helper is rooted at {}, but the host config expects {}",
                response.root, hello.root
            ),
        ));
    }
    let missing = response.missing_required();
    if !missing.is_empty() {
        client.kill().await;
        return Err(RemoteError::new(
            RemoteErrorCode::UnsupportedOperation,
            format!(
                "helper is missing required capabilities: {}",
                missing.join(", ")
            ),
        ));
    }
    client.hello = response;
    Ok(client)
}

fn value_key(value: &Value) -> String {
    match value {
        Value::String(value) => format!("\"{value}\""),
        other => other.to_string(),
    }
}

fn fail_pending(pending: &PendingMap, error: RemoteError) {
    let drained = pending
        .lock()
        .map(|mut pending| {
            pending
                .drain()
                .map(|(_, sender)| sender)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for sender in drained {
        let _ = sender.send(Err(error.clone()));
    }
}

impl RemoteClient {
    pub async fn connect_process(
        process: RemoteProcess,
        hello: HelloParams,
        config: RemoteClientConfig,
    ) -> Result<Self, RemoteError> {
        let description = process.description.clone();
        connect_inner(process, hello, config, description).await
    }

    pub fn hello(&self) -> &HelloResult {
        &self.hello
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn stderr_tail(&self) -> String {
        self.process
            .as_ref()
            .map(|process| process.stderr_tail())
            .unwrap_or_default()
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub fn max_frame_bytes(&self) -> usize {
        self.config
            .max_frame_bytes
            .min(self.hello.max_frame_bytes as usize)
            .max(1024)
    }

    pub async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        params: &impl Serialize,
        wait: Duration,
    ) -> Result<T, RemoteError> {
        let value = self.call_value(method, params, wait).await?;
        serde_json::from_value(value).map_err(|error| {
            RemoteError::new(
                RemoteErrorCode::InvalidResponse,
                format!("cannot decode {method} response: {error}"),
            )
        })
    }

    pub async fn call_value(
        &self,
        method: &str,
        params: &impl Serialize,
        wait: Duration,
    ) -> Result<Value, RemoteError> {
        if self.is_closed() {
            return Err(RemoteError::new(
                RemoteErrorCode::TransportClosed,
                format!("remote helper is not connected: {}", self.stderr_tail()),
            ));
        }
        let params = serde_json::to_value(params).map_err(|error| {
            RemoteError::new(
                RemoteErrorCode::InvalidParams,
                format!("cannot encode {method} params: {error}"),
            )
        })?;
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let key = id.to_string();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.pending
            .lock()
            .map_err(|_| {
                RemoteError::new(RemoteErrorCode::Internal, "client pending map is poisoned")
            })?
            .insert(key.clone(), sender);
        let request = RpcRequest::new(id, method, params);
        {
            let mut writer = self.writer.lock().await;
            let Some(stdin) = writer.as_mut() else {
                self.closed.store(true, Ordering::SeqCst);
                return Err(RemoteError::new(
                    RemoteErrorCode::TransportClosed,
                    "remote helper stdin is closed",
                ));
            };
            let frame_limit = self.max_frame_bytes();
            if let Err(error) = timeout(
                self.config.request_timeout.min(wait),
                write_frame(stdin, &request, frame_limit),
            )
            .await
            {
                let _ = self.pending.lock().map(|mut pending| pending.remove(&key));
                return Err(RemoteError::new(
                    RemoteErrorCode::Timeout,
                    format!("timed out writing {method} to the remote helper: {error}"),
                ));
            }
        }
        match timeout(wait, receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(RemoteError::new(
                RemoteErrorCode::TransportClosed,
                "remote helper connection closed before the response arrived",
            )),
            Err(_) => {
                let _ = self.pending.lock().map(|mut pending| pending.remove(&key));
                let _ = self
                    .notify(
                        METHOD_CANCEL,
                        &CancelParams {
                            id: Value::from(id),
                            reason: Some(format!("{method} timed out after {wait:?}")),
                        },
                        Duration::from_secs(2),
                    )
                    .await;
                Err(RemoteError::new(
                    RemoteErrorCode::Timeout,
                    format!(
                        "{method} did not complete within {wait:?}; a cancellation was requested"
                    ),
                )
                .with_hint("the helper kills a running exec process group on cancellation"))
            }
        }
    }

    pub async fn notify(
        &self,
        method: &str,
        params: &impl Serialize,
        wait: Duration,
    ) -> Result<(), RemoteError> {
        let params = serde_json::to_value(params).map_err(|error| {
            RemoteError::new(
                RemoteErrorCode::InvalidParams,
                format!("cannot encode {method} notification: {error}"),
            )
        })?;
        let notification = RpcNotification {
            jsonrpc: "2.0".to_string(),
            method: method.to_string(),
            params,
        };
        let mut writer = self.writer.lock().await;
        let Some(stdin) = writer.as_mut() else {
            return Err(RemoteError::new(
                RemoteErrorCode::TransportClosed,
                "remote helper stdin is closed",
            ));
        };
        timeout(
            wait,
            write_frame(stdin, &notification, self.max_frame_bytes()),
        )
        .await
        .map_err(|_| {
            RemoteError::new(
                RemoteErrorCode::Timeout,
                format!("timed out sending {method} notification"),
            )
        })?
        .map_err(|error| {
            RemoteError::new(
                RemoteErrorCode::TransportClosed,
                format!("cannot send {method} notification: {error}"),
            )
        })
    }

    pub async fn probe(&self) -> Result<ProbeResult, RemoteError> {
        self.call(METHOD_PROBE, &ProbeParams {}, self.config.request_timeout)
            .await
    }

    pub async fn health(&self) -> Result<ProbeResult, RemoteError> {
        self.call(METHOD_HEALTH, &ProbeParams {}, self.config.request_timeout)
            .await
    }

    pub async fn stat(&self, params: &StatParams) -> Result<StatResult, RemoteError> {
        self.call(METHOD_STAT, params, self.config.request_timeout)
            .await
    }

    pub async fn list_dir(&self, params: &ListDirParams) -> Result<ListDirResult, RemoteError> {
        self.call(METHOD_LIST_DIR, params, self.config.request_timeout)
            .await
    }

    pub async fn read_file(&self, params: &ReadFileParams) -> Result<ReadFileResult, RemoteError> {
        self.call(METHOD_READ_FILE, params, self.config.request_timeout)
            .await
    }

    pub async fn write_file(
        &self,
        params: &WriteFileParams,
    ) -> Result<WriteFileResult, RemoteError> {
        self.call(METHOD_WRITE_FILE, params, self.config.request_timeout)
            .await
    }

    pub async fn mkdir(&self, params: &MkdirParams) -> Result<MkdirResult, RemoteError> {
        self.call(METHOD_MKDIR, params, self.config.request_timeout)
            .await
    }

    pub async fn remove(&self, params: &RemoveParams) -> Result<RemoveResult, RemoteError> {
        self.call(METHOD_REMOVE, params, self.config.request_timeout)
            .await
    }

    pub async fn rename(&self, params: &RenameParams) -> Result<RenameResult, RemoteError> {
        self.call(METHOD_RENAME, params, self.config.request_timeout)
            .await
    }

    pub async fn exec(&self, params: &ExecParams) -> Result<ExecResult, RemoteError> {
        self.call(METHOD_EXEC, params, self.config.request_timeout)
            .await
    }

    /// Exec with an explicit bounded wait. Remote test commands may legitimately
    /// run longer than the ordinary control-request timeout; the helper still
    /// receives a finite `timeout_ms` and the client still sends a cancel on
    /// expiry.
    pub async fn exec_with_timeout(
        &self,
        params: &ExecParams,
        wait: Duration,
    ) -> Result<ExecResult, RemoteError> {
        let wait = wait.min(Duration::from_millis(
            MAX_EXEC_TIMEOUT_MS.saturating_add(30_000),
        ));
        self.call(METHOD_EXEC, params, wait).await
    }

    pub async fn git_status(&self) -> Result<GitStatusResult, RemoteError> {
        self.call(
            METHOD_GIT_STATUS,
            &GitStatusParams {},
            self.config.request_timeout,
        )
        .await
    }

    pub async fn git_diff(&self, params: &GitDiffParams) -> Result<GitDiffResult, RemoteError> {
        self.call(METHOD_GIT_DIFF, params, self.config.request_timeout)
            .await
    }

    pub async fn git_log(&self, params: &GitLogParams) -> Result<GitLogResult, RemoteError> {
        self.call(METHOD_GIT_LOG, params, self.config.request_timeout)
            .await
    }

    pub async fn git_rev_parse(
        &self,
        params: &GitRevParseParams,
    ) -> Result<GitRevParseResult, RemoteError> {
        self.call(METHOD_GIT_REV_PARSE, params, self.config.request_timeout)
            .await
    }

    pub async fn git_worktree_add(
        &self,
        params: &GitWorktreeAddParams,
    ) -> Result<GitWorktreeAddResult, RemoteError> {
        self.call(METHOD_GIT_WORKTREE_ADD, params, self.config.request_timeout)
            .await
    }

    pub async fn git_worktree_list(&self) -> Result<GitWorktreeListResult, RemoteError> {
        self.call(
            METHOD_GIT_WORKTREE_LIST,
            &GitWorktreeListParams {},
            self.config.request_timeout,
        )
        .await
    }

    pub async fn git_worktree_remove(
        &self,
        params: &GitWorktreeRemoveParams,
    ) -> Result<GitWorktreeRemoveResult, RemoteError> {
        self.call(
            METHOD_GIT_WORKTREE_REMOVE,
            params,
            self.config.request_timeout,
        )
        .await
    }

    pub fn supports_git_bundle(&self) -> bool {
        self.hello.has_capability(CAP_GIT_BUNDLE)
    }

    fn require_git_bundle(&self) -> Result<(), RemoteError> {
        if self.supports_git_bundle() {
            Ok(())
        } else {
            Err(RemoteError::new(
                RemoteErrorCode::UnsupportedOperation,
                "the remote helper does not advertise the git.bundle capability",
            )
            .with_hint("upgrade vp-remote-agent before integrating a remote branch"))
        }
    }

    /// Bundle generation can legitimately take longer than an ordinary file
    /// read. The bound stays finite and the helper kills Git on timeout.
    fn bundle_timeout(&self) -> Duration {
        self.config
            .request_timeout
            .max(Duration::from_secs(300))
            .min(Duration::from_secs(600))
    }

    pub async fn git_bundle_create(
        &self,
        params: &GitBundleCreateParams,
    ) -> Result<GitBundleCreateResult, RemoteError> {
        self.require_git_bundle()?;
        self.call(METHOD_GIT_BUNDLE_CREATE, params, self.bundle_timeout())
            .await
    }

    pub async fn git_bundle_read(
        &self,
        params: &GitBundleReadParams,
    ) -> Result<GitBundleReadResult, RemoteError> {
        self.require_git_bundle()?;
        self.call(METHOD_GIT_BUNDLE_READ, params, self.bundle_timeout())
            .await
    }

    pub async fn git_bundle_delete(
        &self,
        params: &GitBundleDeleteParams,
    ) -> Result<GitBundleDeleteResult, RemoteError> {
        self.require_git_bundle()?;
        self.call(
            METHOD_GIT_BUNDLE_DELETE,
            params,
            self.config.request_timeout,
        )
        .await
    }

    pub async fn ledger_stats(
        &self,
        params: &LedgerStatsParams,
    ) -> Result<LedgerStatsResult, RemoteError> {
        self.call(METHOD_LEDGER_STATS, params, self.config.request_timeout)
            .await
    }

    pub async fn shutdown(self) -> Result<(), RemoteError> {
        let _ = self
            .call::<Value>(
                METHOD_SHUTDOWN,
                &ShutdownParams {
                    reason: Some("daemon requested shutdown".to_string()),
                },
                Duration::from_secs(5),
            )
            .await;
        self.kill().await;
        Ok(())
    }

    pub async fn kill(&self) {
        self.closed.store(true, Ordering::SeqCst);
        {
            let mut writer = self.writer.lock().await;
            if let Some(mut stdin) = writer.take() {
                let _ = stdin.shutdown().await;
            }
        }
        if let Some(reader) = self.reader.lock().await.take() {
            reader.abort();
        }
        if let Some(process) = self.process.as_ref() {
            process.shutdown(Duration::from_secs(2)).await;
        }
    }
}

impl Drop for RemoteClient {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
        if let Ok(mut reader) = self.reader.try_lock()
            && let Some(reader) = reader.take()
        {
            reader.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_ids_stay_stable() {
        assert_eq!(value_key(&Value::from(7)), "7");
        assert_eq!(value_key(&Value::from("abc")), "\"abc\"");
    }

    #[test]
    fn default_config_has_bounded_limits() {
        let config = RemoteClientConfig::default();
        assert_eq!(config.max_frame_bytes, DEFAULT_MAX_FRAME_BYTES as usize);
        assert!(config.request_timeout >= Duration::from_secs(1));
    }
}

#[cfg(test)]
mod error_mapping_tests {
    use super::*;

    #[test]
    fn rpc_errors_without_remote_data_map_to_standard_codes() {
        let error = RemoteError::from(vp_protocol::rpc::RpcError::new(-32001, "unauthorized"));
        assert_eq!(error.code, RemoteErrorCode::Unauthorized);
        let error = RemoteError::from(vp_protocol::rpc::RpcError::new(-32004, "missing"));
        assert_eq!(error.code, RemoteErrorCode::PathNotFound);
        let error = RemoteError::from(vp_protocol::rpc::RpcError::new(-32601, "unknown"));
        assert_eq!(error.code, RemoteErrorCode::UnsupportedOperation);
        let error = RemoteError::from(vp_protocol::rpc::RpcError::new(-1, "other"));
        assert_eq!(error.code, RemoteErrorCode::Internal);
    }
}

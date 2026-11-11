//! Project-scoped remote workspace manager.
//!
//! The manager owns at most one helper connection per project/config
//! fingerprint, reconnects after idle periods, caches capabilities, and bounds
//! concurrent calls. It never routes an existing local tool: the agent must
//! call one of the explicit `remote_*` tools, and those tools only appear when
//! the project has an `ssh` execution host.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::Engine;
use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex as AsyncMutex, Semaphore};
use vp_protocol::now_rfc3339;
use vp_protocol::remote::{
    ExecutionHost, ExecutionHostMode, ProjectExecutionHost, REMOTE_VERIFICATION_ENVIRONMENT_CAVEAT,
    RemoteIntegrationCleanupParams, RemoteIntegrationCleanupResult, RemoteIntegrationPolicy,
    RemoteIntegrationPolicyResult, RemoteIntegrationQueueEntry, RemoteIntegrationStatusResult,
    RemoteLedgerEntry, RemoteLedgerResult, RemoteProbeResult, RemoteStatusResult, RemoteWorktree,
    RemoteWorktreeListResult,
};
use vp_protocol::types::ShellPolicy;
use vp_remote::protocol::{
    ExecParams, ExecResult, GitDiffParams, GitDiffResult, GitLogParams, GitLogResult,
    GitRevParseParams, GitRevParseResult, GitStatusResult, GitWorktreeAddParams,
    GitWorktreeAddResult, GitWorktreeRemoveParams, GitWorktreeRemoveResult, HelloParams,
    LedgerStatsParams, ListDirParams, ListDirResult, MkdirParams, MkdirResult, ProbeResult,
    REMOTE_PROTOCOL_VERSION, ReadFileParams, ReadFileResult, RemoveParams, RemoveResult,
    RenameParams, RenameResult, StatParams, StatResult, WriteFileParams, WriteFileResult,
};
use vp_remote::{
    RemoteClient, RemoteClientConfig, RemoteErrorCode, RemoteTransport, SshConfig, SshTransport,
    which_helper,
};

const MAX_WORKSPACES: usize = 32;

#[derive(Debug, Clone, Default)]
struct Health {
    connected: bool,
    last_error: Option<String>,
    last_connected_at: Option<String>,
    helper_version: Option<String>,
    capabilities: Vec<String>,
}

struct Connection {
    client: Arc<RemoteClient>,
    hello: vp_remote::HelloResult,
    connected_at: String,
    last_used: Instant,
}

pub struct RemoteWorkspace {
    project_id: String,
    config: ExecutionHost,
    connection: AsyncMutex<Option<Connection>>,
    connect_lock: AsyncMutex<()>,
    health: Mutex<Health>,
    semaphore: Arc<Semaphore>,
    inflight: AtomicUsize,
    last_used: Mutex<Instant>,
}

impl std::fmt::Debug for RemoteWorkspace {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteWorkspace")
            .field("project_id", &self.project_id)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl RemoteWorkspace {
    fn new(project_id: String, config: ExecutionHost) -> Self {
        let permits = config.max_concurrency.max(1) as usize;
        Self {
            project_id,
            config,
            connection: AsyncMutex::new(None),
            connect_lock: AsyncMutex::new(()),
            health: Mutex::new(Health::default()),
            semaphore: Arc::new(Semaphore::new(permits)),
            inflight: AtomicUsize::new(0),
            last_used: Mutex::new(Instant::now()),
        }
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    pub fn config(&self) -> &ExecutionHost {
        &self.config
    }

    fn idle_timeout(&self) -> Option<Duration> {
        (self.config.idle_timeout_secs > 0)
            .then(|| Duration::from_secs(self.config.idle_timeout_secs))
    }

    fn request_timeout(&self) -> Duration {
        Duration::from_secs(self.config.request_timeout_secs.clamp(1, 3600))
    }

    async fn current_client(&self) -> Result<Arc<RemoteClient>> {
        let mut guard = self.connection.lock().await;
        if let Some(connection) = guard.as_mut() {
            let expired = self
                .idle_timeout()
                .is_some_and(|idle| connection.last_used.elapsed() >= idle);
            if !connection.client.is_closed()
                && !(expired && self.inflight.load(Ordering::SeqCst) == 0)
            {
                connection.last_used = Instant::now();
                return Ok(connection.client.clone());
            }
        }
        drop(guard);
        self.connect().await
    }

    async fn connect(&self) -> Result<Arc<RemoteClient>> {
        let _connect = self.connect_lock.lock().await;
        {
            let mut guard = self.connection.lock().await;
            if let Some(connection) = guard.as_mut() {
                let expired = self
                    .idle_timeout()
                    .is_some_and(|idle| connection.last_used.elapsed() >= idle);
                if !connection.client.is_closed()
                    && !(expired && self.inflight.load(Ordering::SeqCst) == 0)
                {
                    connection.last_used = Instant::now();
                    return Ok(connection.client.clone());
                }
            }
            if let Some(old) = guard.take() {
                old.client.kill().await;
            }
        }
        let result = self.connect_inner().await;
        match result {
            Ok(connection) => {
                let client = connection.client.clone();
                let capabilities = connection.hello.capabilities.clone();
                let helper_version = connection.hello.helper_version.clone();
                let connected_at = connection.connected_at.clone();
                *self.connection.lock().await = Some(connection);
                if let Ok(mut health) = self.health.lock() {
                    health.connected = true;
                    health.last_error = None;
                    health.last_connected_at = Some(connected_at);
                    health.helper_version = Some(helper_version);
                    health.capabilities = capabilities;
                }
                Ok(client)
            }
            Err(error) => {
                if let Ok(mut health) = self.health.lock() {
                    health.connected = false;
                    health.last_error = Some(error.to_string());
                }
                Err(error)
            }
        }
    }

    async fn connect_inner(&self) -> Result<Connection> {
        let token_env = self.config.token_env.as_deref().ok_or_else(|| {
            anyhow!(
                "remote host for project {} has no token_env; the daemon needs the token to authenticate the helper",
                self.project_id
            )
        })?;
        let token = std::env::var(token_env).with_context(|| {
            format!(
                "reading remote auth token from daemon environment variable {token_env} for project {}",
                self.project_id
            )
        })?;
        let token = token.trim().to_string();
        if token.chars().count() < 16 {
            bail!("remote auth token from {token_env} must contain at least 16 characters");
        }
        let mut transport_config = SshConfig {
            host: self.config.host.clone(),
            user: self.config.user.clone(),
            port: self.config.port,
            identity_file: self.config.identity_file.clone(),
            helper_path: self.config.helper_path.clone(),
            root: self.config.root.clone(),
            token_env: self.config.token_env.clone(),
            token_file: self.config.token_file.clone(),
            ledger_path: self.config.ledger_path.clone(),
            env_allow: self.config.env_allow.clone(),
            connect_timeout_secs: self.config.connect_timeout_secs,
            ssh_args: self.config.ssh_args.clone(),
            max_frame_bytes: vp_remote::MAX_FRAME_BYTES,
        };
        if let Some(identity) = transport_config.identity_file.as_deref()
            && identity.contains("~/")
            && let Some(resolved) = vp_remote::transport::resolve_ssh_identity(identity)
        {
            transport_config.identity_file = Some(resolved.to_string_lossy().to_string());
        }
        let transport = SshTransport::new(transport_config);
        let hello = HelloParams {
            protocol_version: REMOTE_PROTOCOL_VERSION,
            client_version: env!("CARGO_PKG_VERSION").to_string(),
            auth_token: token,
            root: self.config.root.clone(),
        };
        let client = RemoteClient::connect(
            &transport as &dyn RemoteTransport,
            hello,
            RemoteClientConfig {
                request_timeout: self.request_timeout(),
                handshake_timeout: Duration::from_secs(
                    self.config.connect_timeout_secs.clamp(1, 300),
                ),
                max_frame_bytes: vp_remote::MAX_FRAME_BYTES as usize,
            },
        )
        .await
        .map_err(|error| anyhow!("remote helper handshake failed: {error}"))?;
        client
            .probe()
            .await
            .map_err(|error| anyhow!("remote helper connected but probe failed: {error}"))?;
        Ok(Connection {
            hello: client.hello().clone(),
            client: Arc::new(client),
            connected_at: now_rfc3339(),
            last_used: Instant::now(),
        })
    }

    pub async fn invalidate(&self) {
        let old = {
            let mut guard = self.connection.lock().await;
            guard.take()
        };
        if let Some(connection) = old {
            connection.client.kill().await;
        }
        if let Ok(mut health) = self.health.lock() {
            health.connected = false;
        }
    }

    async fn call<T, F>(&self, call: F) -> Result<T>
    where
        F: FnOnce(
            Arc<RemoteClient>,
        ) -> futures_util::future::BoxFuture<
            'static,
            std::result::Result<T, vp_remote::RemoteError>,
        >,
    {
        let permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|_| anyhow!("remote workspace semaphore is closed"))?;
        // Resolve the connection before counting this call as in-flight so an
        // idle-expired connection can be replaced when no other call is using
        // it. Once resolved, the count protects it from a concurrent reconnect.
        let client = match self.current_client().await {
            Ok(client) => client,
            Err(error) => {
                drop(permit);
                return Err(error);
            }
        };
        self.inflight.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut last) = self.last_used.lock() {
            *last = Instant::now();
        }
        let result = call(client).await;
        self.inflight.fetch_sub(1, Ordering::SeqCst);
        drop(permit);
        match result {
            Ok(value) => Ok(value),
            Err(error) => {
                if let Ok(mut health) = self.health.lock() {
                    health.last_error = Some(error.to_string());
                    if error.code == RemoteErrorCode::TransportClosed {
                        health.connected = false;
                    }
                }
                if error.code == RemoteErrorCode::TransportClosed {
                    self.invalidate().await;
                }
                Err(anyhow!(error))
            }
        }
    }

    pub async fn probe(&self) -> Result<RemoteProbeResult> {
        let started = Instant::now();
        let client = self.current_client().await?;
        let probe = client.probe().await.map_err(|error| anyhow!(error))?;
        Ok(RemoteProbeResult {
            project_id: self.project_id.clone(),
            connected: true,
            protocol_version: probe.protocol_version,
            helper_version: probe.helper_version,
            root: probe.root,
            os: probe.os,
            arch: probe.arch,
            git_version: probe.git_version,
            capabilities: probe.capabilities,
            round_trip_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        })
    }

    pub async fn status(&self) -> RemoteStatusResult {
        let connection = self.connection.lock().await;
        let health = self
            .health
            .lock()
            .map(|health| health.clone())
            .unwrap_or_default();
        let connected = connection
            .as_ref()
            .is_some_and(|connection| !connection.client.is_closed());
        let helper_version = connection
            .as_ref()
            .map(|connection| connection.hello.helper_version.clone())
            .or(health.helper_version);
        let capabilities = connection
            .as_ref()
            .map(|connection| connection.hello.capabilities.clone())
            .unwrap_or(health.capabilities);
        RemoteStatusResult {
            project_id: self.project_id.clone(),
            mode: self.config.mode,
            configured: self.config.is_remote(),
            connected,
            root: self.config.root.clone(),
            host: self.config.is_remote().then(|| self.config.host.clone()),
            helper_path: self.config.helper_path.clone(),
            capabilities,
            helper_version,
            last_error: health.last_error,
            last_connected_at: health.last_connected_at,
            inflight: self.inflight.load(Ordering::SeqCst) as u32,
            max_concurrency: self.config.max_concurrency,
            idle_timeout_secs: self.config.idle_timeout_secs,
        }
    }

    pub async fn stat(&self, params: StatParams) -> Result<StatResult> {
        self.call(move |client| Box::pin(async move { client.stat(&params).await }))
            .await
    }

    pub async fn list_dir(&self, params: ListDirParams) -> Result<ListDirResult> {
        self.call(move |client| Box::pin(async move { client.list_dir(&params).await }))
            .await
    }

    pub async fn read_file(&self, params: ReadFileParams) -> Result<ReadFileResult> {
        self.call(move |client| Box::pin(async move { client.read_file(&params).await }))
            .await
    }

    pub async fn write_file(&self, params: WriteFileParams) -> Result<WriteFileResult> {
        self.call(move |client| Box::pin(async move { client.write_file(&params).await }))
            .await
    }

    pub async fn mkdir(&self, params: MkdirParams) -> Result<MkdirResult> {
        self.call(move |client| Box::pin(async move { client.mkdir(&params).await }))
            .await
    }

    pub async fn remove(&self, params: RemoveParams) -> Result<RemoveResult> {
        self.call(move |client| Box::pin(async move { client.remove(&params).await }))
            .await
    }

    pub async fn rename(&self, params: RenameParams) -> Result<RenameResult> {
        self.call(move |client| Box::pin(async move { client.rename(&params).await }))
            .await
    }

    pub async fn exec(&self, params: ExecParams) -> Result<ExecResult> {
        self.call(move |client| Box::pin(async move { client.exec(&params).await }))
            .await
    }

    pub async fn git_status(&self) -> Result<GitStatusResult> {
        self.call(|client| Box::pin(async move { client.git_status().await }))
            .await
    }

    pub async fn git_diff(&self, params: GitDiffParams) -> Result<GitDiffResult> {
        self.call(move |client| Box::pin(async move { client.git_diff(&params).await }))
            .await
    }

    pub async fn git_log(&self, params: GitLogParams) -> Result<GitLogResult> {
        self.call(move |client| Box::pin(async move { client.git_log(&params).await }))
            .await
    }

    pub async fn git_rev_parse(&self, params: GitRevParseParams) -> Result<GitRevParseResult> {
        self.call(move |client| Box::pin(async move { client.git_rev_parse(&params).await }))
            .await
    }

    pub async fn worktree_add(&self, params: GitWorktreeAddParams) -> Result<GitWorktreeAddResult> {
        self.call(move |client| Box::pin(async move { client.git_worktree_add(&params).await }))
            .await
    }

    pub async fn worktree_list(&self) -> Result<Vec<RemoteWorktree>> {
        let result = self
            .call(|client| Box::pin(async move { client.git_worktree_list().await }))
            .await?;
        Ok(result
            .entries
            .into_iter()
            .map(|entry| RemoteWorktree {
                path: entry.path,
                branch: entry.branch,
                head: entry.head,
                detached: entry.detached,
                locked: entry.locked,
            })
            .collect())
    }

    pub async fn worktree_remove(
        &self,
        params: GitWorktreeRemoveParams,
    ) -> Result<GitWorktreeRemoveResult> {
        self.call(move |client| Box::pin(async move { client.git_worktree_remove(&params).await }))
            .await
    }

    pub async fn git_worktree_remove(
        &self,
        params: GitWorktreeRemoveParams,
    ) -> Result<GitWorktreeRemoveResult> {
        self.worktree_remove(params).await
    }

    pub async fn supports_git_bundle(&self) -> Result<bool> {
        let client = self.current_client().await?;
        Ok(client.supports_git_bundle())
    }

    pub async fn git_bundle_create(
        &self,
        params: vp_remote::GitBundleCreateParams,
    ) -> Result<vp_remote::GitBundleCreateResult> {
        self.call(move |client| Box::pin(async move { client.git_bundle_create(&params).await }))
            .await
    }

    pub async fn git_bundle_read(
        &self,
        params: vp_remote::GitBundleReadParams,
    ) -> Result<vp_remote::GitBundleReadResult> {
        self.call(move |client| Box::pin(async move { client.git_bundle_read(&params).await }))
            .await
    }

    pub async fn git_bundle_delete(
        &self,
        params: vp_remote::GitBundleDeleteParams,
    ) -> Result<vp_remote::GitBundleDeleteResult> {
        self.call(move |client| Box::pin(async move { client.git_bundle_delete(&params).await }))
            .await
    }

    pub async fn health(&self) -> Result<ProbeResult> {
        let client = self.current_client().await?;
        client.health().await.map_err(|error| anyhow!(error))
    }

    pub async fn ledger_stats(&self, limit: u32) -> Result<vp_remote::LedgerStatsResult> {
        self.call(move |client| {
            Box::pin(async move {
                client
                    .ledger_stats(&LedgerStatsParams {
                        limit: limit.clamp(1, 200),
                    })
                    .await
            })
        })
        .await
    }
}

pub struct RemoteManager {
    data_dir: std::path::PathBuf,
    helper_path: std::path::PathBuf,
    workspaces: Mutex<HashMap<String, Arc<RemoteWorkspace>>>,
}

impl RemoteManager {
    pub fn new(data_dir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            helper_path: which_helper(),
            workspaces: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_helper_path(
        data_dir: impl Into<std::path::PathBuf>,
        helper_path: impl Into<std::path::PathBuf>,
    ) -> Self {
        Self {
            data_dir: data_dir.into(),
            helper_path: helper_path.into(),
            workspaces: Mutex::new(HashMap::new()),
        }
    }

    pub fn helper_path(&self) -> &std::path::Path {
        &self.helper_path
    }

    pub fn data_dir(&self) -> &std::path::Path {
        &self.data_dir
    }

    pub async fn workspace(
        &self,
        project_id: &str,
        config: &ExecutionHost,
    ) -> Result<Arc<RemoteWorkspace>> {
        config
            .validate()
            .map_err(|error| anyhow!("invalid execution host for project {project_id}: {error}"))?;
        if !config.is_remote() {
            bail!("project {project_id} uses a local execution host; remote tools are unavailable");
        }
        let fingerprint = config_fingerprint(project_id, config)?;
        let mut workspaces = self
            .workspaces
            .lock()
            .map_err(|_| anyhow!("remote workspace registry is poisoned"))?;
        if let Some(workspace) = workspaces.get(&fingerprint) {
            return Ok(workspace.clone());
        }
        if workspaces.len() >= MAX_WORKSPACES {
            let oldest = workspaces
                .iter()
                .filter_map(|(key, workspace)| {
                    workspace.last_used.lock().ok().map(|last| {
                        (
                            key.clone(),
                            *last,
                            workspace.inflight.load(Ordering::SeqCst),
                        )
                    })
                })
                .filter(|(_, _, inflight)| *inflight == 0)
                .min_by_key(|(_, last, _)| *last)
                .map(|(key, _, _)| key);
            if let Some(key) = oldest {
                workspaces.remove(&key);
            } else {
                bail!("too many active remote workspaces; close a session or wait for idle");
            }
        }
        let workspace = Arc::new(RemoteWorkspace::new(project_id.to_string(), config.clone()));
        workspaces.insert(fingerprint, workspace.clone());
        Ok(workspace)
    }

    pub async fn status(
        &self,
        project_id: &str,
        config: &ExecutionHost,
    ) -> Result<RemoteStatusResult> {
        if !config.is_remote() {
            return Ok(RemoteStatusResult {
                project_id: project_id.to_string(),
                mode: ExecutionHostMode::Local,
                configured: false,
                connected: false,
                root: String::new(),
                host: None,
                helper_path: self.helper_path.to_string_lossy().to_string(),
                capabilities: Vec::new(),
                helper_version: None,
                last_error: None,
                last_connected_at: None,
                inflight: 0,
                max_concurrency: config.max_concurrency,
                idle_timeout_secs: config.idle_timeout_secs,
            });
        }
        let workspace = self.workspace(project_id, config).await?;
        Ok(workspace.status().await)
    }

    pub async fn invalidate_project(&self, project_id: &str) {
        let workspaces = self
            .workspaces
            .lock()
            .map(|workspaces| {
                workspaces
                    .values()
                    .filter(|workspace| workspace.project_id == project_id)
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for workspace in workspaces {
            workspace.invalidate().await;
        }
    }

    pub async fn shutdown(&self) {
        let workspaces = self
            .workspaces
            .lock()
            .map(|workspaces| workspaces.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for workspace in workspaces {
            workspace.invalidate().await;
        }
        if let Ok(mut workspaces) = self.workspaces.lock() {
            workspaces.clear();
        }
    }
}

fn config_fingerprint(project_id: &str, config: &ExecutionHost) -> Result<String> {
    let encoded = serde_json::to_vec(config).context("serializing execution host config")?;
    let mut hasher = Sha256::new();
    hasher.update(project_id.as_bytes());
    hasher.update([0u8]);
    hasher.update(encoded);
    Ok(format!("{project_id}:{:x}", hasher.finalize()))
}

/// Tool-facing access bundle. It carries the configured host and the project
/// shell policy, but not a connection: the manager resolves the connection on
/// each operation so a config update or idle reconnect is observed.
#[derive(Clone)]
pub struct RemoteToolAccess {
    pub manager: Arc<RemoteManager>,
    pub project_id: String,
    pub config: ExecutionHost,
    pub shell_policy: ShellPolicy,
}

impl std::fmt::Debug for RemoteToolAccess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteToolAccess")
            .field("project_id", &self.project_id)
            .field("config", &self.config)
            .field("shell_policy", &self.shell_policy)
            .finish_non_exhaustive()
    }
}

impl RemoteToolAccess {
    pub fn label(&self) -> String {
        self.config.label()
    }

    pub async fn workspace(&self) -> Result<Arc<RemoteWorkspace>> {
        self.manager.workspace(&self.project_id, &self.config).await
    }

    pub fn ensure_project(&self, requested: Option<&str>) -> Result<()> {
        if let Some(requested) = requested
            && requested != self.project_id
        {
            bail!(
                "remote tool is bound to project {}; refusing project_id {}",
                self.project_id,
                requested
            );
        }
        if !self.config.is_remote() {
            bail!(
                "project {} has no ssh execution host; remote tools are unavailable",
                self.project_id
            );
        }
        Ok(())
    }

    pub fn validate_path(&self, path: &str) -> Result<String> {
        vp_remote::PathJail::validate_relative(path).map_err(|error| anyhow!(error))
    }

    pub fn ensure_exec_allowed(&self) -> Result<()> {
        if self.shell_policy.network == "deny" {
            bail!(
                "remote exec is denied by the project ShellPolicy network=deny; use remote_read/remote_list or allow network for this host"
            );
        }
        if self.shell_policy.restrict_home
            || self.shell_policy.read_jail
            || !self.shell_policy.read_deny.is_empty()
        {
            bail!(
                "remote exec cannot enforce the project read restrictions (restrict_home/read_jail/read_deny); use remote_read/remote_list or relax the shell policy"
            );
        }
        Ok(())
    }
}

/// Build the SshConfig without touching the network. Exposed for diagnostics
/// and tests so argument construction can be inspected.
pub fn ssh_config(config: &ExecutionHost) -> Result<SshConfig> {
    config
        .validate()
        .map_err(|error| anyhow!("invalid execution host: {error}"))?;
    if !config.is_remote() {
        bail!("execution host is local");
    }
    Ok(SshConfig {
        host: config.host.clone(),
        user: config.user.clone(),
        port: config.port,
        identity_file: config.identity_file.clone(),
        helper_path: config.helper_path.clone(),
        root: config.root.clone(),
        token_env: config.token_env.clone(),
        token_file: config.token_file.clone(),
        ledger_path: config.ledger_path.clone(),
        env_allow: config.env_allow.clone(),
        connect_timeout_secs: config.connect_timeout_secs,
        ssh_args: config.ssh_args.clone(),
        max_frame_bytes: vp_remote::MAX_FRAME_BYTES,
    })
}

impl Engine {
    pub async fn get_remote_integration_policy(
        &self,
        project_id: &str,
    ) -> Result<RemoteIntegrationPolicy> {
        let store = self.store.clone();
        let project_id_owned = project_id.to_string();
        let exists = tokio::task::spawn_blocking({
            let store = store.clone();
            let project_id = project_id_owned.clone();
            move || store.get_project(&project_id)
        })
        .await??
        .is_some();
        if !exists {
            bail!("project not found: {project_id}");
        }
        tokio::task::spawn_blocking(move || store.get_remote_integration_policy(&project_id_owned))
            .await?
    }

    pub async fn remote_integration_policy_result(
        &self,
        project_id: &str,
    ) -> Result<RemoteIntegrationPolicyResult> {
        let policy = self.get_remote_integration_policy(project_id).await?;
        let store = self.store.clone();
        let project_id_owned = project_id.to_string();
        let updated_at = tokio::task::spawn_blocking(move || {
            store.remote_integration_policy_updated_at(&project_id_owned)
        })
        .await??
        .unwrap_or_default();
        Ok(RemoteIntegrationPolicyResult {
            project_id: project_id.to_string(),
            policy,
            updated_at,
        })
    }

    /// Persist an opt-in policy. Enabling remote integration requires operator
    /// confirmation even though the caller is the local operator: this keeps a
    /// scripted or accidental enable from silently widening host access.
    pub async fn set_remote_integration_policy(
        &self,
        project_id: &str,
        policy: RemoteIntegrationPolicy,
        confirm: bool,
    ) -> Result<RemoteIntegrationPolicyResult> {
        policy
            .validate()
            .map_err(|error| anyhow!("invalid remote integration policy: {error}"))?;
        if policy.enabled && !confirm {
            return Err(crate::remote_integration::RemoteIntegrationFailure::new(
                crate::tooling::errors::ToolErrorCode::REMOTE_INTEGRATION_APPROVAL_REQUIRED,
                "enabling remote integration requires explicit operator confirmation",
            )
            .into());
        }
        let store = self.store.clone();
        let project_id_owned = project_id.to_string();
        let project_id_check = project_id_owned.clone();
        let exists = tokio::task::spawn_blocking({
            let store = store.clone();
            move || store.get_project(&project_id_check)
        })
        .await??
        .is_some();
        if !exists {
            bail!("project not found: {project_id}");
        }
        let policy_for_store = policy.clone();
        let project_id_for_store = project_id_owned.clone();
        tokio::task::spawn_blocking(move || {
            store.set_remote_integration_policy(&project_id_for_store, &policy_for_store)?;
            store.record_project_manifest()
        })
        .await??;
        let updated_at = {
            let store = self.store.clone();
            tokio::task::spawn_blocking(move || {
                store.remote_integration_policy_updated_at(&project_id_owned)
            })
            .await??
            .unwrap_or_default()
        };
        Ok(RemoteIntegrationPolicyResult {
            project_id: project_id.to_string(),
            policy,
            updated_at,
        })
    }

    pub async fn remote_integration_status(
        &self,
        project_id: &str,
        session_id: Option<String>,
    ) -> Result<RemoteIntegrationStatusResult> {
        let store = self.store.clone();
        let project_id_owned = project_id.to_string();
        let session_filter = session_id.clone();
        let project_id_check = project_id_owned.clone();
        let (project, policy, updated_at, entries) = tokio::task::spawn_blocking(move || {
            let project = store
                .get_project(&project_id_check)?
                .ok_or_else(|| anyhow!("project not found: {project_id_check}"))?;
            if let Some(session_id) = session_filter.as_deref() {
                let session = store
                    .get_session(session_id)?
                    .ok_or_else(|| anyhow!("session not found: {session_id}"))?;
                if session.project_id != project_id_check {
                    anyhow::bail!(
                        "session {session_id} does not belong to project {project_id_check}"
                    );
                }
            }
            let policy = store.get_remote_integration_policy(&project_id_check)?;
            let updated_at = store
                .remote_integration_policy_updated_at(&project_id_check)?
                .unwrap_or_default();
            let entries =
                store.list_remote_merge_queue(&project_id_check, session_filter.as_deref())?;
            Ok::<_, anyhow::Error>((project, policy, updated_at, entries))
        })
        .await??;
        let project_root = std::path::PathBuf::from(&project.root);
        let retained_refs = entries
            .iter()
            .filter(|entry| {
                entry.imported_ref.as_deref().is_some_and(|reference| {
                    crate::remote_integration::import_ref_exists(&project_root, reference)
                })
            })
            .count();
        let mut pending = 0usize;
        let mut merged = 0usize;
        let mut conflicts = 0usize;
        let mut failed = 0usize;
        let total = entries.len();
        let entries = entries
            .into_iter()
            .map(|entry| {
                match entry.status.as_str() {
                    "queued" | "merging" | "checking" => pending += 1,
                    "merged" => merged += 1,
                    "conflict" => conflicts += 1,
                    "failed" | "interrupted" => failed += 1,
                    _ => {}
                }
                RemoteIntegrationQueueEntry {
                    entry_id: entry.id,
                    session_id: entry.session_id,
                    status: entry.status,
                    commit: entry.commit,
                    branch: entry.branch,
                    remote_host: entry.remote_host,
                    remote_branch: entry.remote_branch,
                    bundle_sha256: entry.bundle_sha256,
                    imported_ref: entry.imported_ref,
                    verifier_approved: entry.verifier_approved,
                    verifier_summary: entry.verifier_summary,
                    environment_caveat: entry.environment_caveat,
                    attempt: entry.attempt,
                    detail: entry.detail,
                    created_at: entry.created_at,
                    updated_at: entry.updated_at,
                }
            })
            .collect();
        Ok(RemoteIntegrationStatusResult {
            project_id: project_id.to_string(),
            session_id,
            policy: policy.clone(),
            policy_updated_at: updated_at,
            entries,
            total,
            pending,
            merged,
            conflicts,
            failed,
            retained_refs,
            verifier_required: policy.require_verifier,
            environment_caveat: REMOTE_VERIFICATION_ENVIRONMENT_CAVEAT.to_string(),
        })
    }

    /// Remove temporary remote-integration refs, optionally with their local
    /// worktrees. Retryable entries are protected unless `force` is set so an
    /// operator cannot accidentally destroy the only copy of a conflicted
    /// remote branch. Active integrations are never touched.
    pub async fn remote_integration_cleanup(
        &self,
        params: RemoteIntegrationCleanupParams,
    ) -> Result<RemoteIntegrationCleanupResult> {
        let store = self.store.clone();
        let project_id = params.project_id.clone();
        let session_filter = params.session_id.clone();
        let entry_filter = params.entry_id.clone();
        let project_id_check = project_id.clone();
        let (project, entries, agents) = tokio::task::spawn_blocking(move || {
            let project = store
                .get_project(&project_id_check)?
                .ok_or_else(|| anyhow!("project not found: {project_id_check}"))?;
            if let Some(session_id) = session_filter.as_deref() {
                let session = store
                    .get_session(session_id)?
                    .ok_or_else(|| anyhow!("session not found: {session_id}"))?;
                if session.project_id != project_id_check {
                    anyhow::bail!(
                        "session {session_id} does not belong to project {project_id_check}"
                    );
                }
            }
            if let Some(entry_id) = entry_filter.as_deref()
                && store.get_merge_queue_entry(entry_id)?.is_none()
            {
                anyhow::bail!("merge queue entry not found: {entry_id}");
            }
            let entries =
                store.list_remote_merge_queue(&project_id_check, session_filter.as_deref())?;
            let mut agents = std::collections::HashMap::new();
            for session in store.list_sessions(Some(&project_id_check))? {
                for agent in store.list_agents(&session.id)? {
                    agents.insert(agent.id.clone(), agent);
                }
            }
            Ok::<_, anyhow::Error>((project, entries, agents))
        })
        .await??;
        let root = std::path::PathBuf::from(&project.root);
        let mut removed_refs = Vec::new();
        let mut removed_worktrees = Vec::new();
        let mut skipped = Vec::new();
        let mut examined = 0usize;
        for entry in entries {
            if let Some(entry_id) = params.entry_id.as_deref()
                && entry.id != entry_id
            {
                continue;
            }
            examined += 1;
            if matches!(entry.status.as_str(), "queued" | "merging" | "checking") {
                skipped.push(format!(
                    "{}: integration is active ({})",
                    entry.id, entry.status
                ));
                continue;
            }
            if !params.force
                && matches!(entry.status.as_str(), "conflict" | "failed" | "interrupted")
            {
                skipped.push(format!(
                    "{}: retryable entry ({}) requires force",
                    entry.id, entry.status
                ));
                continue;
            }
            let Some(reference) = entry.imported_ref.clone() else {
                skipped.push(format!("{}: no imported ref", entry.id));
                continue;
            };
            if !crate::remote_integration::import_ref_exists(&root, &reference) {
                skipped.push(format!("{}: imported ref already absent", entry.id));
                continue;
            }
            let root_for_delete = root.clone();
            let reference_for_delete = reference.clone();
            match tokio::task::spawn_blocking(move || {
                crate::remote_integration::delete_import_ref(
                    &root_for_delete,
                    &reference_for_delete,
                )
            })
            .await?
            {
                Ok(()) => removed_refs.push(reference.clone()),
                Err(error) => {
                    skipped.push(format!("{}: {error:#}", entry.id));
                    continue;
                }
            }
            if params.force
                && let Some(agent) = agents.get(&entry.agent_id)
                && let (Some(path), Some(branch)) =
                    (agent.worktree.as_deref(), agent.branch.as_deref())
            {
                let root_for_worktree = root.clone();
                let path = std::path::PathBuf::from(path);
                let path_label = path.to_string_lossy().to_string();
                let branch = branch.to_string();
                if let Ok(Ok(())) = tokio::task::spawn_blocking(move || {
                    crate::remote_integration::remove_remote_worktree(
                        &root_for_worktree,
                        &path,
                        &branch,
                    )
                    .map_err(anyhow::Error::new)
                })
                .await
                {
                    removed_worktrees.push(path_label);
                }
            }
            self.append_event(
                &entry.session_id,
                "remote.integration.refs_pruned",
                serde_json::json!({
                    "entry_id": entry.id,
                    "imported_ref": reference,
                    "status": entry.status,
                    "force": params.force,
                }),
            )
            .await?;
        }
        Ok(RemoteIntegrationCleanupResult {
            project_id,
            session_id: params.session_id,
            removed_refs,
            removed_worktrees,
            skipped,
            entries_examined: examined,
        })
    }

    pub async fn list_execution_hosts(&self) -> Result<Vec<ProjectExecutionHost>> {
        let store = self.store.clone();
        let projects = tokio::task::spawn_blocking(move || store.list_projects()).await??;
        let mut hosts = Vec::with_capacity(projects.len());
        for project in projects {
            let store = self.store.clone();
            let project_id = project.id.clone();
            let (config, updated_at) = tokio::task::spawn_blocking(move || {
                Ok::<_, anyhow::Error>((
                    store.get_execution_host(&project_id)?,
                    store.execution_host_updated_at(&project_id)?,
                ))
            })
            .await??;
            hosts.push(ProjectExecutionHost {
                project_id: project.id,
                config,
                updated_at: updated_at.unwrap_or_default(),
            });
        }
        Ok(hosts)
    }

    pub async fn get_execution_host(&self, project_id: &str) -> Result<ExecutionHost> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        let config = tokio::task::spawn_blocking({
            let store = store.clone();
            let project_id = project_id.clone();
            move || store.get_execution_host(&project_id)
        })
        .await??;
        let store_check = store;
        let project_id_for_check = project_id.clone();
        let exists =
            tokio::task::spawn_blocking(move || store_check.get_project(&project_id_for_check))
                .await??
                .is_some();
        if !exists {
            bail!("project not found: {project_id}");
        }
        config
            .validate()
            .map_err(|error| anyhow!("stored execution host is invalid: {error}"))?;
        Ok(config)
    }

    pub async fn set_execution_host(
        &self,
        project_id: &str,
        config: ExecutionHost,
    ) -> Result<ExecutionHost> {
        config
            .validate()
            .map_err(|error| anyhow!("invalid execution host: {error}"))?;
        let store = self.store.clone();
        let project_id_owned = project_id.to_string();
        let exists = tokio::task::spawn_blocking({
            let store = store.clone();
            let project_id = project_id_owned.clone();
            move || store.get_project(&project_id)
        })
        .await??
        .is_some();
        if !exists {
            bail!("project not found: {project_id}");
        }
        let config_for_store = config.clone();
        tokio::task::spawn_blocking(move || {
            store.set_execution_host(&project_id_owned, &config_for_store)
        })
        .await??;
        self.remote.invalidate_project(project_id).await;
        Ok(config)
    }

    pub async fn remove_execution_host(&self, project_id: &str) -> Result<bool> {
        let store = self.store.clone();
        let project_id_owned = project_id.to_string();
        let exists = tokio::task::spawn_blocking({
            let store = store.clone();
            let project_id = project_id_owned.clone();
            move || store.get_project(&project_id)
        })
        .await??
        .is_some();
        if !exists {
            bail!("project not found: {project_id}");
        }
        let removed =
            tokio::task::spawn_blocking(move || store.remove_execution_host(&project_id_owned))
                .await??;
        self.remote.invalidate_project(project_id).await;
        Ok(removed)
    }

    pub async fn probe_execution_host(
        &self,
        project_id: &str,
        timeout_secs: Option<u64>,
    ) -> Result<RemoteProbeResult> {
        let config = self.get_execution_host(project_id).await?;
        if !config.is_remote() {
            bail!("project {project_id} uses a local execution host");
        }
        let workspace = self.remote.workspace(project_id, &config).await?;
        let wait = Duration::from_secs(
            timeout_secs
                .unwrap_or(config.connect_timeout_secs)
                .clamp(1, 300),
        );
        tokio::time::timeout(wait, workspace.probe())
            .await
            .map_err(|_| anyhow!("remote host probe timed out after {wait:?}"))?
    }

    pub async fn remote_status(&self, project_id: &str) -> Result<RemoteStatusResult> {
        let config = self.get_execution_host(project_id).await?;
        self.remote.status(project_id, &config).await
    }

    /// Resolve the configured remote workspace for an explicit operation. This
    /// never appears as a local tool and does not connect until a method is
    /// called on the returned handle.
    pub async fn remote_workspace(&self, project_id: &str) -> Result<Arc<RemoteWorkspace>> {
        let config = self.get_execution_host(project_id).await?;
        if !config.is_remote() {
            bail!("project {project_id} uses a local execution host");
        }
        self.remote.workspace(project_id, &config).await
    }

    pub async fn remote_ledger(&self, project_id: &str, limit: u32) -> Result<RemoteLedgerResult> {
        let config = self.get_execution_host(project_id).await?;
        if !config.is_remote() {
            bail!("project {project_id} uses a local execution host");
        }
        let workspace = self.remote.workspace(project_id, &config).await?;
        let stats = workspace.ledger_stats(limit).await?;
        Ok(RemoteLedgerResult {
            project_id: project_id.to_string(),
            entries: stats.entries,
            max_entries: stats.max_entries,
            persisted: stats.persisted,
            file_bytes: stats.file_bytes,
            tail: stats
                .tail
                .into_iter()
                .map(|entry| RemoteLedgerEntry {
                    operation_id: entry.operation_id,
                    method: entry.method,
                    success: entry.success,
                    completed_at: entry.completed_at,
                })
                .collect(),
        })
    }

    pub async fn remote_worktree_list(&self, project_id: &str) -> Result<RemoteWorktreeListResult> {
        let config = self.get_execution_host(project_id).await?;
        if !config.is_remote() {
            bail!("project {project_id} uses a local execution host");
        }
        let workspace = self.remote.workspace(project_id, &config).await?;
        Ok(RemoteWorktreeListResult {
            project_id: project_id.to_string(),
            worktrees: workspace.worktree_list().await?,
        })
    }

    /// Build the tool access bundle for one agent run. Local projects get
    /// `None`, so remote tools fail explicitly rather than falling back to the
    /// local filesystem.
    pub async fn remote_access(&self, project_id: &str) -> Result<Option<RemoteToolAccess>> {
        let config = self.get_execution_host(project_id).await?;
        if !config.is_remote() {
            return Ok(None);
        }
        let policy = self.get_shell_policy(project_id).await?;
        Ok(Some(RemoteToolAccess {
            manager: self.remote.clone(),
            project_id: project_id.to_string(),
            config,
            shell_policy: policy,
        }))
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;
    use vp_protocol::remote::ExecutionHostMode;

    use super::*;

    fn config() -> ExecutionHost {
        ExecutionHost {
            mode: ExecutionHostMode::Ssh,
            host: "example.com".into(),
            user: Some("agent".into()),
            root: "/srv/project".into(),
            token_env: Some("VERLINPARK_REMOTE_TOKEN_FOR_TEST".into()),
            ..ExecutionHost::default()
        }
    }

    #[tokio::test]
    async fn local_projects_get_no_workspace() {
        let dir = TempDir::new().unwrap();
        let manager = RemoteManager::new(dir.path());
        let error = manager
            .workspace("p1", &ExecutionHost::default())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("local execution host"));
    }

    #[tokio::test]
    async fn missing_daemon_token_fails_before_spawning_ssh() {
        let dir = TempDir::new().unwrap();
        let manager = RemoteManager::new(dir.path());
        let config = config();
        let workspace = manager.workspace("p1", &config).await.unwrap();
        let error = workspace.probe().await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("VERLINPARK_REMOTE_TOKEN_FOR_TEST")
        );
    }

    #[tokio::test]
    async fn the_same_config_reuses_one_workspace_and_a_change_creates_another() {
        let dir = TempDir::new().unwrap();
        let manager = RemoteManager::new(dir.path());
        let first = manager.workspace("p1", &config()).await.unwrap();
        let again = manager.workspace("p1", &config()).await.unwrap();
        assert!(Arc::ptr_eq(&first, &again));
        let mut changed = config();
        changed.port = Some(2222);
        let other = manager.workspace("p1", &changed).await.unwrap();
        assert!(!Arc::ptr_eq(&first, &other));
        assert_eq!(other.config().port, Some(2222));
    }

    #[tokio::test]
    async fn status_reports_a_local_project_without_connecting() {
        let dir = TempDir::new().unwrap();
        let manager = RemoteManager::new(dir.path());
        let status = manager
            .status("p1", &ExecutionHost::default())
            .await
            .unwrap();
        assert_eq!(status.mode, ExecutionHostMode::Local);
        assert!(!status.configured);
        assert!(!status.connected);
    }

    #[test]
    fn tool_access_rejects_cross_project_and_absolute_paths() {
        let dir = TempDir::new().unwrap();
        let manager = Arc::new(RemoteManager::new(dir.path()));
        let access = RemoteToolAccess {
            manager,
            project_id: "p1".into(),
            config: config(),
            shell_policy: ShellPolicy::default(),
        };
        assert!(access.ensure_project(Some("p1")).is_ok());
        assert!(access.ensure_project(Some("p2")).is_err());
        assert!(access.validate_path("a/b").is_ok());
        assert!(access.validate_path("../etc").is_err());
        assert!(access.validate_path("/etc/passwd").is_err());
    }

    #[test]
    fn exec_policy_fails_closed_for_unenforceable_read_restrictions() {
        let dir = TempDir::new().unwrap();
        let manager = Arc::new(RemoteManager::new(dir.path()));
        let mut access = RemoteToolAccess {
            manager,
            project_id: "p1".into(),
            config: config(),
            shell_policy: ShellPolicy::default(),
        };
        assert!(access.ensure_exec_allowed().is_ok());
        access.shell_policy.network = "deny".into();
        assert!(access.ensure_exec_allowed().is_err());
        access.shell_policy.network = "allow".into();
        access.shell_policy.read_deny = vec!["/home".into()];
        assert!(access.ensure_exec_allowed().is_err());
    }

    #[tokio::test]
    async fn engine_persists_host_config_and_builds_tool_access() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let store = vp_store::Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("remote"))
            .unwrap();
        let engine = Engine::new(store);
        assert_eq!(
            engine.get_execution_host(&project.id).await.unwrap(),
            ExecutionHost::default()
        );
        assert!(engine.remote_access(&project.id).await.unwrap().is_none());
        let config = config();
        engine
            .set_execution_host(&project.id, config.clone())
            .await
            .unwrap();
        assert_eq!(
            engine.get_execution_host(&project.id).await.unwrap(),
            config
        );
        let hosts = engine.list_execution_hosts().await.unwrap();
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].project_id, project.id);
        assert_eq!(hosts[0].config, config);
        let access = engine.remote_access(&project.id).await.unwrap().unwrap();
        assert_eq!(access.project_id, project.id);
        assert_eq!(access.config, config);
        let status = engine.remote_status(&project.id).await.unwrap();
        assert_eq!(status.mode, ExecutionHostMode::Ssh);
        assert!(status.configured);
        assert!(!status.connected);
        assert!(engine.remove_execution_host(&project.id).await.unwrap());
        assert!(engine.remote_access(&project.id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn engine_rejects_invalid_host_config_and_local_probe() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let store = vp_store::Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("remote"))
            .unwrap();
        let engine = Engine::new(store);
        let mut invalid = config();
        invalid.root = "relative".into();
        let error = engine
            .set_execution_host(&project.id, invalid)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("absolute"));
        let error = engine
            .probe_execution_host(&project.id, Some(1))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("local execution host"));
    }

    #[test]
    fn fingerprints_include_project_and_config() {
        let first = config_fingerprint("p1", &config()).unwrap();
        let second = config_fingerprint("p2", &config()).unwrap();
        assert_ne!(first, second);
        let mut changed = config();
        changed.host = "other.example.com".into();
        assert_ne!(first, config_fingerprint("p1", &changed).unwrap());
    }
}

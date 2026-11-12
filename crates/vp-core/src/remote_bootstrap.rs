//! Explicit remote-helper bootstrap.
//!
//! The daemon wraps the bounded SSH installer in [`vp_remote::bootstrap`] with
//! project policy, operator confirmation, a persisted install record, and the
//! Q3 typed failure contract. Bootstrap is a host-state action: it requires
//! `confirm`, never runs from a scheduler, and never rewrites the project's
//! execution-host configuration.

use anyhow::{Result, anyhow, bail};
use serde_json::json;
use vp_protocol::now_rfc3339;
use vp_protocol::remote::{
    DEFAULT_REMOTE_BOOTSTRAP_PATH, ExecutionHost, REMOTE_BOOTSTRAP_ENVIRONMENT_CAVEAT,
    REMOTE_BOOTSTRAP_RISK, RemoteBootstrapParams, RemoteBootstrapRecord, RemoteBootstrapResult,
    RemoteBootstrapStatusParams, RemoteBootstrapStatusResult,
};
use vp_remote::bootstrap::{
    BootstrapRequest, DEFAULT_BOOTSTRAP_CHUNK_BYTES, DEFAULT_BOOTSTRAP_TIMEOUT_SECS,
    HELPER_VERSION, SshBootstrapConfig, SshBootstrapTransport, bootstrap_helper,
    read_helper_binary,
};
use vp_remote::{RemoteError, RemoteErrorCode};

use crate::Engine;
use crate::permissions::RiskLevel;
use crate::tooling::errors::{ToolErrorCode, ToolFailure};

/// Typed bootstrap failure carried through `anyhow` and rendered as RPC error
/// data. It uses the same bounded Q3 contract as remote integration.
#[derive(Debug, Clone)]
pub struct RemoteBootstrapFailure {
    pub failure: Box<ToolFailure>,
}

impl RemoteBootstrapFailure {
    pub fn new(code: ToolErrorCode, message: impl Into<String>) -> Self {
        let failure = ToolFailure::new(code, message).with_tool("remote_bootstrap");
        Self {
            failure: Box::new(failure),
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        let failure = *self.failure;
        self.failure = Box::new(failure.with_hint(hint));
        self
    }

    pub fn with_evidence(mut self, evidence: impl Into<String>) -> Self {
        let failure = *self.failure;
        self.failure = Box::new(failure.with_evidence(evidence));
        self
    }

    pub fn with_retryable(mut self, retryable: bool) -> Self {
        let failure = *self.failure;
        self.failure = Box::new(failure.with_retryable(retryable));
        self
    }

    pub fn code(&self) -> &'static str {
        self.failure.code.as_str()
    }

    pub fn data(&self) -> serde_json::Value {
        serde_json::to_value(self.failure.as_ref()).unwrap_or_else(|_| {
            json!({
                "code": self.code(),
                "message": self.failure.message,
                "retryable": self.failure.retryable,
                "hint": self.failure.hint,
            })
        })
    }
}

impl std::fmt::Display for RemoteBootstrapFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "error[{}]: {}",
            self.code(),
            self.failure.message
        )
    }
}

impl std::error::Error for RemoteBootstrapFailure {}

pub fn failure_data(error: &anyhow::Error) -> Option<serde_json::Value> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<RemoteBootstrapFailure>())
        .map(RemoteBootstrapFailure::data)
}

/// Bootstrap is always host-state risk. Keeping the classifier next to the
/// operation prevents a future tool registration from silently downgrading it.
pub fn bootstrap_risk() -> RiskLevel {
    RiskLevel::HostState
}

fn classify_remote_error(error: &RemoteError) -> ToolErrorCode {
    match error.code {
        RemoteErrorCode::BootstrapPathRejected => ToolErrorCode::REMOTE_BOOTSTRAP_PATH_REJECTED,
        RemoteErrorCode::BootstrapExists => ToolErrorCode::REMOTE_BOOTSTRAP_EXISTS,
        RemoteErrorCode::BootstrapArchMismatch => ToolErrorCode::REMOTE_BOOTSTRAP_ARCH_MISMATCH,
        RemoteErrorCode::BootstrapChecksumMismatch => {
            ToolErrorCode::REMOTE_BOOTSTRAP_CHECKSUM_MISMATCH
        }
        RemoteErrorCode::BootstrapVersionMismatch => {
            ToolErrorCode::REMOTE_BOOTSTRAP_VERSION_MISMATCH
        }
        RemoteErrorCode::BootstrapProtocolMismatch => {
            ToolErrorCode::REMOTE_BOOTSTRAP_PROTOCOL_MISMATCH
        }
        RemoteErrorCode::BootstrapDisk => ToolErrorCode::REMOTE_BOOTSTRAP_DISK,
        RemoteErrorCode::BootstrapUnsupported => ToolErrorCode::REMOTE_BOOTSTRAP_UNSUPPORTED,
        RemoteErrorCode::BootstrapUploadFailed => ToolErrorCode::REMOTE_BOOTSTRAP_UPLOAD_FAILED,
        RemoteErrorCode::BootstrapInstallFailed => ToolErrorCode::REMOTE_BOOTSTRAP_INSTALL_FAILED,
        RemoteErrorCode::BootstrapNotInstalled => ToolErrorCode::REMOTE_BOOTSTRAP_NOT_INSTALLED,
        RemoteErrorCode::Timeout => ToolErrorCode::REMOTE_TIMEOUT,
        RemoteErrorCode::TransportClosed | RemoteErrorCode::Unauthorized => {
            ToolErrorCode::REMOTE_UNAVAILABLE
        }
        RemoteErrorCode::InvalidParams | RemoteErrorCode::InvalidRequest => {
            ToolErrorCode::REMOTE_BOOTSTRAP_FAILED
        }
        _ => ToolErrorCode::REMOTE_BOOTSTRAP_FAILED,
    }
}

pub fn wrap_remote_error(error: RemoteError) -> RemoteBootstrapFailure {
    let code = classify_remote_error(&error);
    let retryable = error.retryable;
    let mut failure = RemoteBootstrapFailure::new(code, error.message.clone())
        .with_evidence(format!("remote_code={}", error.code.as_str()))
        .with_retryable(retryable);
    if let Some(hint) = error.hint.as_deref() {
        failure = failure.with_hint(hint);
    }
    failure
}

/// Resolve the install path without mutating project configuration. An
/// explicit request wins; otherwise a configured absolute/tilde helper path is
/// reused so bootstrap installs exactly where the client will look.
pub fn resolve_install_path(config: &ExecutionHost, requested: Option<&str>) -> String {
    if let Some(requested) = requested {
        return requested.trim().to_string();
    }
    let configured = config.helper_path.trim();
    if configured.starts_with("~/") || configured.starts_with('/') {
        return configured.to_string();
    }
    DEFAULT_REMOTE_BOOTSTRAP_PATH.to_string()
}

fn expand_for_compare(path: &str) -> &str {
    path.strip_prefix("~/").unwrap_or(path)
}

/// Whether the configured helper path names the just-installed file. A bare
/// program name is not treated as a match because PATH lookup was never
/// verified.
pub fn helper_path_matches(configured: &str, installed: &str, requested: &str) -> bool {
    let configured = configured.trim();
    if configured.is_empty() {
        return false;
    }
    if configured == installed || configured == requested {
        return true;
    }
    if configured.starts_with("~/") {
        let rest = expand_for_compare(configured);
        return installed.ends_with(&format!("/{rest}")) || installed == rest;
    }
    false
}

impl Engine {
    /// Explicit, approval-classified helper install. The operator must pass
    /// `confirm: true`; a local project or a network-denied ShellPolicy fails
    /// before any SSH process is spawned.
    pub async fn remote_bootstrap(
        &self,
        params: RemoteBootstrapParams,
    ) -> Result<RemoteBootstrapResult> {
        if !params.confirm {
            return Err(RemoteBootstrapFailure::new(
                ToolErrorCode::REMOTE_BOOTSTRAP_APPROVAL_REQUIRED,
                "remote helper bootstrap requires explicit operator confirmation",
            )
            .with_hint(
                "repeat `vp remote-bootstrap --confirm` after reviewing the host and install path",
            )
            .into());
        }
        let config = self.get_execution_host(&params.project_id).await?;
        if !config.is_remote() {
            return Err(RemoteBootstrapFailure::new(
                ToolErrorCode::REMOTE_NOT_CONFIGURED,
                format!(
                    "project {} uses a local execution host; remote bootstrap is unavailable",
                    params.project_id
                ),
            )
            .into());
        }
        let shell_policy = self.get_shell_policy(&params.project_id).await?;
        if shell_policy.network == "deny" {
            return Err(RemoteBootstrapFailure::new(
                ToolErrorCode::REMOTE_NETWORK_DENIED,
                "remote helper bootstrap is denied by the project ShellPolicy network=deny",
            )
            .with_hint("bootstrap speaks SSH and cannot be confined by the local Seatbelt policy; relax network only for a host you trust")
            .into());
        }
        if shell_policy.restrict_home
            || shell_policy.read_jail
            || !shell_policy.read_deny.is_empty()
        {
            return Err(RemoteBootstrapFailure::new(
                ToolErrorCode::REMOTE_BOOTSTRAP_FAILED,
                "remote helper bootstrap cannot enforce the project read restrictions",
            )
            .with_hint("bootstrap needs to inspect the install directory and existing helper; relax restrict_home/read_jail/read_deny for this host")
            .into());
        }
        if let Some(session_id) = params.session_id.as_deref() {
            let store = self.store.clone();
            let session_id = session_id.to_string();
            let lookup_id = session_id.clone();
            let project_id = params.project_id.clone();
            let session = tokio::task::spawn_blocking(move || store.get_session(&lookup_id))
                .await??
                .ok_or_else(|| anyhow!("session not found: {session_id}"))?;
            if session.project_id != project_id {
                bail!(
                    "session {} does not belong to project {}",
                    session.id,
                    params.project_id
                );
            }
        }

        let install_path = resolve_install_path(&config, params.install_path.as_deref());
        let requested_path = params.install_path.clone();
        let helper_path = self.remote_manager().helper_path().to_path_buf();
        let helper_bytes = read_helper_binary(&helper_path).map_err(wrap_remote_error)?;
        let timeout_secs = params
            .timeout_secs
            .unwrap_or(DEFAULT_BOOTSTRAP_TIMEOUT_SECS)
            .clamp(1, 900);
        let mut ssh = SshBootstrapConfig {
            host: config.host.clone(),
            user: config.user.clone(),
            port: config.port,
            identity_file: config.identity_file.clone(),
            connect_timeout_secs: config.connect_timeout_secs,
            ssh_args: config.ssh_args.clone(),
        };
        if let Some(identity) = ssh.identity_file.as_deref()
            && identity.contains("~/")
            && let Some(resolved) = vp_remote::transport::resolve_ssh_identity(identity)
        {
            ssh.identity_file = Some(resolved.to_string_lossy().to_string());
        }
        let request = BootstrapRequest {
            install_path: install_path.clone(),
            expected_sha256: params.expected_sha256.clone(),
            expected_version: params
                .expected_version
                .clone()
                .unwrap_or_else(|| HELPER_VERSION.to_string()),
            expected_protocol_version: vp_remote::REMOTE_PROTOCOL_VERSION,
            force: params.force,
            max_bytes: vp_remote::bootstrap::MAX_BOOTSTRAP_BYTES,
            chunk_bytes: DEFAULT_BOOTSTRAP_CHUNK_BYTES,
            timeout: std::time::Duration::from_secs(timeout_secs),
        };
        let transport = SshBootstrapTransport::new(ssh);
        let outcome = bootstrap_helper(&transport, request, &helper_bytes)
            .await
            .map_err(wrap_remote_error)?;

        let now = now_rfc3339();
        let record = RemoteBootstrapRecord {
            project_id: params.project_id.clone(),
            host: config.host.clone(),
            install_path: outcome.install_path.clone(),
            sha256: outcome.sha256.clone(),
            bytes: outcome.bytes,
            helper_version: outcome.helper_version.clone(),
            protocol_version: outcome.protocol_version,
            target_os: outcome.target_os.clone(),
            target_arch: outcome.target_arch.clone(),
            installed_at: now.clone(),
            updated_at: now.clone(),
        };
        let store = self.store.clone();
        let record_for_store = record.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            store.upsert_remote_bootstrap_record(&record_for_store)?;
            store.record_project_manifest()
        })
        .await??;
        let host_path_mismatch = !helper_path_matches(
            &config.helper_path,
            &outcome.install_path,
            requested_path.as_deref().unwrap_or(&install_path),
        );
        if let Some(session_id) = params.session_id.as_deref() {
            self.append_event(
                session_id,
                "remote.bootstrap.completed",
                json!({
                    "project_id": params.project_id,
                    "host": config.host,
                    "install_path": outcome.install_path,
                    "sha256": outcome.sha256,
                    "bytes": outcome.bytes,
                    "helper_version": outcome.helper_version,
                    "protocol_version": outcome.protocol_version,
                    "target_os": outcome.target_os,
                    "target_arch": outcome.target_arch,
                    "replaced": outcome.replaced,
                    "used_sudo": false,
                    "risk": REMOTE_BOOTSTRAP_RISK,
                    "confirmation_required": true,
                    "host_path_mismatch": host_path_mismatch,
                    "environment_caveat": REMOTE_BOOTSTRAP_ENVIRONMENT_CAVEAT,
                }),
            )
            .await?;
        }
        self.remote_manager()
            .invalidate_project(&params.project_id)
            .await;
        Ok(RemoteBootstrapResult {
            project_id: params.project_id,
            host: config.host,
            install_path: outcome.install_path,
            sha256: outcome.sha256,
            bytes: outcome.bytes,
            helper_version: outcome.helper_version,
            protocol_version: outcome.protocol_version,
            target_os: outcome.target_os,
            target_arch: outcome.target_arch,
            remote_os: outcome.remote_os,
            remote_arch: outcome.remote_arch,
            replaced: outcome.replaced,
            used_sudo: outcome.used_sudo,
            chunks: outcome.chunks,
            duration_ms: outcome.duration_ms,
            installed_at: now,
            risk: REMOTE_BOOTSTRAP_RISK.to_string(),
            approval_required: true,
            confirmation_required: true,
            host_path_mismatch,
            environment_caveat: REMOTE_BOOTSTRAP_ENVIRONMENT_CAVEAT.to_string(),
        })
    }

    pub async fn remote_bootstrap_status(
        &self,
        params: RemoteBootstrapStatusParams,
    ) -> Result<RemoteBootstrapStatusResult> {
        let config = self.get_execution_host(&params.project_id).await?;
        if !config.is_remote() {
            return Ok(RemoteBootstrapStatusResult {
                project_id: params.project_id,
                configured: false,
                host: None,
                install_path: DEFAULT_REMOTE_BOOTSTRAP_PATH.to_string(),
                configured_helper_path: config.helper_path.clone(),
                path_matches_config: false,
                record: None,
                risk: REMOTE_BOOTSTRAP_RISK.to_string(),
                approval_required: true,
                environment_caveat: REMOTE_BOOTSTRAP_ENVIRONMENT_CAVEAT.to_string(),
            });
        }
        let store = self.store.clone();
        let project_id = params.project_id.clone();
        let records =
            tokio::task::spawn_blocking(move || store.list_remote_bootstrap_records(&project_id))
                .await??;
        let record = records
            .iter()
            .find(|record| record.host == config.host && record.install_path == config.helper_path)
            .or_else(|| records.iter().find(|record| record.host == config.host))
            .cloned();
        let install_path = resolve_install_path(&config, None);
        let path_matches_config = record.as_ref().is_some_and(|record| {
            helper_path_matches(&config.helper_path, &record.install_path, &install_path)
        });
        Ok(RemoteBootstrapStatusResult {
            project_id: params.project_id,
            configured: true,
            host: Some(config.host),
            install_path,
            configured_helper_path: config.helper_path,
            path_matches_config,
            record,
            risk: REMOTE_BOOTSTRAP_RISK.to_string(),
            approval_required: true,
            environment_caveat: REMOTE_BOOTSTRAP_ENVIRONMENT_CAVEAT.to_string(),
        })
    }
}

/// The daemon RPC uses this to distinguish an ordinary validation error from a
/// typed bootstrap failure. Other errors keep their normal message.
pub fn failure_from_anyhow(error: &anyhow::Error) -> Option<serde_json::Value> {
    failure_data(error)
}

pub fn validate_bootstrap_confirm(confirm: bool) -> Result<()> {
    if confirm {
        Ok(())
    } else {
        Err(RemoteBootstrapFailure::new(
            ToolErrorCode::REMOTE_BOOTSTRAP_APPROVAL_REQUIRED,
            "remote helper bootstrap requires explicit operator confirmation",
        )
        .into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vp_protocol::remote::ExecutionHostMode;

    fn remote_host() -> ExecutionHost {
        ExecutionHost {
            mode: ExecutionHostMode::Ssh,
            host: "build.example.com".into(),
            user: Some("agent".into()),
            root: "/srv/project".into(),
            helper_path: "~/.verlinpark/bin/vp-remote-agent".into(),
            token_env: Some("VERLINPARK_REMOTE_TOKEN_FOR_TEST".into()),
            ..ExecutionHost::default()
        }
    }

    #[test]
    fn bootstrap_is_host_state_and_always_approval_classified() {
        assert_eq!(bootstrap_risk(), RiskLevel::HostState);
        assert_eq!(bootstrap_risk().as_str(), REMOTE_BOOTSTRAP_RISK);
        assert!(bootstrap_risk().requires_human());
    }

    #[test]
    fn install_path_prefers_explicit_then_configured_then_default() {
        let config = remote_host();
        assert_eq!(
            resolve_install_path(&config, Some("~/custom/helper")),
            "~/custom/helper"
        );
        assert_eq!(
            resolve_install_path(&config, None),
            "~/.verlinpark/bin/vp-remote-agent"
        );
        let mut local = config.clone();
        local.helper_path = "vp-remote-agent".into();
        assert_eq!(
            resolve_install_path(&local, None),
            DEFAULT_REMOTE_BOOTSTRAP_PATH
        );
        local.helper_path = "/opt/verlinpark/vp-remote-agent".into();
        assert_eq!(
            resolve_install_path(&local, None),
            "/opt/verlinpark/vp-remote-agent"
        );
    }

    #[test]
    fn helper_path_matching_never_guesses_path_lookup() {
        assert!(helper_path_matches(
            "~/.verlinpark/bin/vp-remote-agent",
            "/home/agent/.verlinpark/bin/vp-remote-agent",
            "~/.verlinpark/bin/vp-remote-agent"
        ));
        assert!(helper_path_matches(
            "/home/agent/.verlinpark/bin/vp-remote-agent",
            "/home/agent/.verlinpark/bin/vp-remote-agent",
            "/home/agent/.verlinpark/bin/vp-remote-agent"
        ));
        assert!(!helper_path_matches(
            "vp-remote-agent",
            "/home/agent/.verlinpark/bin/vp-remote-agent",
            "~/.verlinpark/bin/vp-remote-agent"
        ));
        assert!(!helper_path_matches(
            "~/other/helper",
            "/home/agent/.verlinpark/bin/vp-remote-agent",
            "~/.verlinpark/bin/vp-remote-agent"
        ));
    }

    #[test]
    fn remote_errors_map_to_stable_bootstrap_codes() {
        for (remote, tool) in [
            (
                RemoteErrorCode::BootstrapPathRejected,
                ToolErrorCode::REMOTE_BOOTSTRAP_PATH_REJECTED,
            ),
            (
                RemoteErrorCode::BootstrapArchMismatch,
                ToolErrorCode::REMOTE_BOOTSTRAP_ARCH_MISMATCH,
            ),
            (
                RemoteErrorCode::BootstrapChecksumMismatch,
                ToolErrorCode::REMOTE_BOOTSTRAP_CHECKSUM_MISMATCH,
            ),
            (
                RemoteErrorCode::BootstrapProtocolMismatch,
                ToolErrorCode::REMOTE_BOOTSTRAP_PROTOCOL_MISMATCH,
            ),
            (
                RemoteErrorCode::BootstrapDisk,
                ToolErrorCode::REMOTE_BOOTSTRAP_DISK,
            ),
            (
                RemoteErrorCode::BootstrapUploadFailed,
                ToolErrorCode::REMOTE_BOOTSTRAP_UPLOAD_FAILED,
            ),
        ] {
            assert_eq!(classify_remote_error(&RemoteError::new(remote, "x")), tool);
        }
        let wrapped = wrap_remote_error(
            RemoteError::new(RemoteErrorCode::BootstrapExists, "already there")
                .with_hint("pass force"),
        );
        assert_eq!(wrapped.code(), "REMOTE_BOOTSTRAP_EXISTS");
        assert!(wrapped.failure.hint.as_str().contains("force"));
    }

    #[test]
    fn confirmation_failure_is_typed() {
        let error = validate_bootstrap_confirm(false).unwrap_err();
        let failure = error
            .downcast_ref::<RemoteBootstrapFailure>()
            .expect("typed failure");
        assert_eq!(failure.code(), "REMOTE_BOOTSTRAP_APPROVAL_REQUIRED");
        assert_eq!(
            failure.data()["code"].as_str(),
            Some("REMOTE_BOOTSTRAP_APPROVAL_REQUIRED")
        );
        assert!(validate_bootstrap_confirm(true).is_ok());
    }

    #[tokio::test]
    async fn bootstrap_requires_confirmation_before_touching_the_host() {
        let data = tempfile::TempDir::new().unwrap();
        let repo = tempfile::TempDir::new().unwrap();
        let store = vp_store::Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("remote"))
            .unwrap();
        let engine = Engine::new(store);
        let error = engine
            .remote_bootstrap(RemoteBootstrapParams {
                project_id: project.id,
                confirm: false,
                ..RemoteBootstrapParams::default()
            })
            .await
            .unwrap_err();
        assert!(failure_data(&error).is_some());
        assert!(
            format!("{error:#}").contains("explicit operator confirmation"),
            "unexpected error: {error:#}"
        );
    }

    #[tokio::test]
    async fn local_projects_have_no_bootstrap_target_and_status_is_honest() {
        let data = tempfile::TempDir::new().unwrap();
        let repo = tempfile::TempDir::new().unwrap();
        let store = vp_store::Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("local"))
            .unwrap();
        let engine = Engine::new(store);
        let error = engine
            .remote_bootstrap(RemoteBootstrapParams {
                project_id: project.id.clone(),
                confirm: true,
                ..RemoteBootstrapParams::default()
            })
            .await
            .unwrap_err();
        let data = failure_data(&error).expect("typed failure");
        assert_eq!(data["code"].as_str(), Some("REMOTE_NOT_CONFIGURED"));
        let status = engine
            .remote_bootstrap_status(RemoteBootstrapStatusParams {
                project_id: project.id,
            })
            .await
            .unwrap();
        assert!(!status.configured);
        assert!(status.record.is_none());
        assert_eq!(status.risk, "host_state");
    }

    #[tokio::test]
    async fn network_deny_refuses_bootstrap_before_spawning_ssh() {
        let data = tempfile::TempDir::new().unwrap();
        let repo = tempfile::TempDir::new().unwrap();
        let store = vp_store::Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("remote"))
            .unwrap();
        let engine = Engine::new(store);
        engine
            .set_execution_host(&project.id, remote_host())
            .await
            .unwrap();
        let mut policy = engine.get_shell_policy(&project.id).await.unwrap();
        policy.network = "deny".into();
        engine.set_shell_policy(&project.id, policy).await.unwrap();
        let error = engine
            .remote_bootstrap(RemoteBootstrapParams {
                project_id: project.id,
                confirm: true,
                ..RemoteBootstrapParams::default()
            })
            .await
            .unwrap_err();
        let data = failure_data(&error).expect("typed failure");
        assert_eq!(data["code"].as_str(), Some("REMOTE_NETWORK_DENIED"));
    }

    #[tokio::test]
    async fn bootstrap_status_reports_the_last_record_without_connecting() {
        let data = tempfile::TempDir::new().unwrap();
        let repo = tempfile::TempDir::new().unwrap();
        let store = vp_store::Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("remote"))
            .unwrap();
        let now = now_rfc3339();
        let record = RemoteBootstrapRecord {
            project_id: project.id.clone(),
            host: "build.example.com".into(),
            install_path: "/home/agent/.verlinpark/bin/vp-remote-agent".into(),
            sha256: "a".repeat(64),
            bytes: 4096,
            helper_version: HELPER_VERSION.into(),
            protocol_version: vp_remote::REMOTE_PROTOCOL_VERSION,
            target_os: "linux".into(),
            target_arch: "x86_64".into(),
            installed_at: now.clone(),
            updated_at: now,
        };
        store.upsert_remote_bootstrap_record(&record).unwrap();
        let engine = Engine::new(store);
        engine
            .set_execution_host(&project.id, remote_host())
            .await
            .unwrap();
        let status = engine
            .remote_bootstrap_status(RemoteBootstrapStatusParams {
                project_id: project.id,
            })
            .await
            .unwrap();
        assert!(status.configured);
        assert!(status.record.is_some());
        assert!(status.path_matches_config);
        assert!(!status.environment_caveat.is_empty());
    }
}

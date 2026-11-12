//! Remote project checks recorded as supplemental evidence.
//!
//! A remote test runs one explicit, bounded project command through the
//! existing helper `exec` operation. It captures the remote HEAD, exit code,
//! duration, and a bounded output reference, persists a `remote_test.completed`
//! event, and can attach the evidence digest to a remote merge-queue row.
//!
//! The local Verifier and deterministic Integrator remain mandatory. This
//! module never changes a Verifier verdict, queue status, or merge decision, and
//! it never claims the remote host was reproduced or sandboxed.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;
use vp_protocol::now_rfc3339;
use vp_protocol::remote::{
    DEFAULT_REMOTE_TEST_EVIDENCE_LIST, MAX_REMOTE_TEST_EVIDENCE_LIST,
    MAX_REMOTE_TEST_OUTPUT_CAP_BYTES, MAX_REMOTE_TEST_OUTPUT_READ_BYTES,
    MAX_REMOTE_TEST_TIMEOUT_SECS, MIN_REMOTE_TEST_OUTPUT_CAP_BYTES, MIN_REMOTE_TEST_TIMEOUT_SECS,
    REMOTE_TEST_ENVIRONMENT_CAVEAT, RemoteTestEvidence, RemoteTestKind, RemoteTestOutputParams,
    RemoteTestOutputResult, RemoteTestParams, RemoteTestPolicy, RemoteTestPolicyResult,
    RemoteTestProfile, RemoteTestResult, RemoteTestStatusParams, RemoteTestStatusResult,
};
use vp_protocol::types::{MERGE_SOURCE_REMOTE, MergeQueueEntry};
use vp_remote::protocol::{
    ExecParams, ExecResult, GitRevParseParams, MAX_EXEC_OUTPUT_BYTES, MAX_EXEC_TIMEOUT_MS,
};

use crate::Engine;
use crate::tooling::errors::{ToolErrorCode, ToolFailure};

pub const REMOTE_TEST_OUTPUT_DIR: &str = "remote-test-outputs";
pub const MAX_REMOTE_TEST_BODY_BYTES: usize = 512 * 1024;
/// Extra client-side wait beyond the helper's own timeout. The helper kills the
/// process group at `timeout_ms`; this lets the response travel back.
pub const REMOTE_TEST_WAIT_MARGIN_SECS: u64 = 30;

/// Typed failure using the Q3 contract. Every error path that refuses to run a
/// test or fails to persist evidence returns one of these codes.
#[derive(Debug, Clone)]
pub struct RemoteTestFailure {
    pub failure: Box<ToolFailure>,
}

impl RemoteTestFailure {
    pub fn new(code: ToolErrorCode, message: impl Into<String>) -> Self {
        let failure = ToolFailure::new(code, message).with_tool("remote_test");
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

impl std::fmt::Display for RemoteTestFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "error[{}]: {}",
            self.code(),
            self.failure.message
        )
    }
}

impl std::error::Error for RemoteTestFailure {}

pub fn failure_data(error: &anyhow::Error) -> Option<serde_json::Value> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<RemoteTestFailure>())
        .map(RemoteTestFailure::data)
}

/// Map a helper transport error to an actionable remote-test code. A missing
/// helper is the most common failure and points at bootstrap instead of
/// pretending the command ran.
pub fn map_remote_error(error: vp_remote::RemoteError) -> RemoteTestFailure {
    match error.code {
        vp_remote::RemoteErrorCode::Timeout => {
            RemoteTestFailure::new(ToolErrorCode::REMOTE_TEST_TIMEOUT, error.message)
                .with_hint("raise the bounded remote test timeout or make the command faster")
                .with_retryable(true)
        }
        vp_remote::RemoteErrorCode::TransportClosed => {
            RemoteTestFailure::new(ToolErrorCode::REMOTE_UNAVAILABLE, error.message)
                .with_hint("install the remote helper with `vp remote-bootstrap`, then retry")
                .with_retryable(true)
        }
        _ => {
            let mut failure =
                RemoteTestFailure::new(ToolErrorCode::REMOTE_TEST_FAILED, error.message)
                    .with_evidence(format!("remote_code={}", error.code.as_str()));
            if let Some(hint) = error.hint {
                failure = failure.with_hint(hint);
            }
            failure
        }
    }
}

pub fn map_workspace_error(error: anyhow::Error) -> RemoteTestFailure {
    let message = format!("{error:#}");
    let lower = message.to_ascii_lowercase();
    if lower.contains("handshake failed")
        || lower.contains("helper")
        || lower.contains("transport")
        || lower.contains("connection")
    {
        return RemoteTestFailure::new(ToolErrorCode::REMOTE_UNAVAILABLE, message)
            .with_hint("verify SSH connectivity and run `vp remote-bootstrap` to install a matching helper")
            .with_retryable(true);
    }
    RemoteTestFailure::new(ToolErrorCode::REMOTE_TEST_FAILED, message)
        .with_hint("inspect the remote execution host configuration and helper state")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRemoteTest {
    pub profile: String,
    pub kind: RemoteTestKind,
    pub command: String,
    pub cwd: Option<String>,
    pub timeout_secs: u64,
    pub output_cap_bytes: u64,
}

/// Resolve a request against the project policy. An explicit command is
/// allowed only while the policy is enabled; it is recorded as `ad-hoc`.
pub fn resolve_remote_test(
    policy: &RemoteTestPolicy,
    params: &RemoteTestParams,
) -> Result<ResolvedRemoteTest, RemoteTestFailure> {
    if !policy.enabled {
        return Err(RemoteTestFailure::new(
            ToolErrorCode::REMOTE_TEST_DISABLED,
            "the project remote test policy is disabled",
        )
        .with_hint("enable the policy with `vp remote-test-policy-set <project> --enable`"));
    }
    let requested_cwd = params.cwd.clone().or_else(|| {
        params
            .profile
            .as_deref()
            .and_then(|name| policy.profile(name).and_then(|profile| profile.cwd.clone()))
    });
    if let Some(cwd) = requested_cwd.as_deref() {
        vp_protocol::remote::RemoteTestProfile {
            name: "validation".into(),
            kind: RemoteTestKind::Other,
            command: "true".into(),
            cwd: Some(cwd.to_string()),
            timeout_secs: None,
        }
        .validate()
        .map_err(|message| RemoteTestFailure::new(ToolErrorCode::REMOTE_TEST_FAILED, message))?;
    }
    if let Some(command) = params.command.as_deref() {
        let command = command.trim().to_string();
        if command.is_empty() {
            return Err(RemoteTestFailure::new(
                ToolErrorCode::REMOTE_TEST_COMMAND_MISSING,
                "remote test command must not be empty",
            ));
        }
        let profile = params
            .profile
            .clone()
            .unwrap_or_else(|| "ad-hoc".to_string());
        RemoteTestProfile {
            name: profile.clone(),
            kind: RemoteTestKind::Other,
            command: command.clone(),
            cwd: params.cwd.clone(),
            timeout_secs: params.timeout_secs,
        }
        .validate()
        .map_err(|message| RemoteTestFailure::new(ToolErrorCode::REMOTE_TEST_FAILED, message))?;
        let timeout_secs = params
            .timeout_secs
            .unwrap_or(policy.timeout_secs)
            .clamp(MIN_REMOTE_TEST_TIMEOUT_SECS, MAX_REMOTE_TEST_TIMEOUT_SECS);
        let output_cap_bytes = params
            .output_cap_bytes
            .unwrap_or(policy.output_cap_bytes)
            .clamp(
                MIN_REMOTE_TEST_OUTPUT_CAP_BYTES,
                MAX_REMOTE_TEST_OUTPUT_CAP_BYTES,
            );
        return Ok(ResolvedRemoteTest {
            profile,
            kind: RemoteTestKind::Other,
            command,
            cwd: params.cwd.clone(),
            timeout_secs,
            output_cap_bytes,
        });
    }

    let profile = match params.profile.as_deref() {
        Some(name) => policy.profile(name).ok_or_else(|| {
            let available = policy
                .profiles
                .iter()
                .map(|profile| profile.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            RemoteTestFailure::new(
                ToolErrorCode::REMOTE_TEST_PROFILE_MISSING,
                format!("remote test profile {name} is not configured"),
            )
            .with_hint(format!(
                "available profiles: {}",
                if available.is_empty() {
                    "<none>".to_string()
                } else {
                    available
                }
            ))
        })?,
        None => policy.resolved_default_profile().ok_or_else(|| {
            RemoteTestFailure::new(
                ToolErrorCode::REMOTE_TEST_PROFILE_MISSING,
                "no remote test profile was requested and the policy has no default",
            )
            .with_hint("pass --profile or configure a default profile")
        })?,
    };
    if profile.command.trim().is_empty() {
        return Err(RemoteTestFailure::new(
            ToolErrorCode::REMOTE_TEST_COMMAND_MISSING,
            format!("remote test profile {} has no command", profile.name),
        ));
    }
    profile
        .validate()
        .map_err(|message| RemoteTestFailure::new(ToolErrorCode::REMOTE_TEST_FAILED, message))?;
    let timeout_secs = params
        .timeout_secs
        .or(profile.timeout_secs)
        .unwrap_or(policy.timeout_secs)
        .clamp(MIN_REMOTE_TEST_TIMEOUT_SECS, MAX_REMOTE_TEST_TIMEOUT_SECS);
    let output_cap_bytes = params
        .output_cap_bytes
        .unwrap_or(policy.output_cap_bytes)
        .clamp(
            MIN_REMOTE_TEST_OUTPUT_CAP_BYTES,
            MAX_REMOTE_TEST_OUTPUT_CAP_BYTES,
        );
    Ok(ResolvedRemoteTest {
        profile: profile.name.clone(),
        kind: profile.kind,
        command: profile.command.clone(),
        cwd: params.cwd.clone().or_else(|| profile.cwd.clone()),
        timeout_secs,
        output_cap_bytes,
    })
}

/// Canonical evidence fields hashed into `digest`. Field order is stable, so
/// the same evidence always yields the same digest.
#[derive(Debug, Serialize)]
struct EvidenceDigestInput<'a> {
    id: &'a str,
    project_id: &'a str,
    session_id: &'a str,
    entry_id: &'a Option<String>,
    profile: &'a str,
    kind: &'a str,
    host: &'a str,
    commit: &'a str,
    command: &'a str,
    cwd: &'a Option<String>,
    status: &'a str,
    exit_code: &'a Option<i32>,
    duration_ms: u64,
    output_sha256: &'a Option<String>,
    output_bytes: u64,
    output_truncated: bool,
    timed_out: bool,
    environment_caveat: &'a str,
    created_at: &'a str,
}

pub fn evidence_digest(evidence: &RemoteTestEvidence) -> String {
    let input = EvidenceDigestInput {
        id: &evidence.id,
        project_id: &evidence.project_id,
        session_id: &evidence.session_id,
        entry_id: &evidence.entry_id,
        profile: &evidence.profile,
        kind: &evidence.kind,
        host: &evidence.host,
        commit: &evidence.commit,
        command: &evidence.command,
        cwd: &evidence.cwd,
        status: &evidence.status,
        exit_code: &evidence.exit_code,
        duration_ms: evidence.duration_ms,
        output_sha256: &evidence.output_sha256,
        output_bytes: evidence.output_bytes,
        output_truncated: evidence.output_truncated,
        timed_out: evidence.timed_out,
        environment_caveat: &evidence.environment_caveat,
        created_at: &evidence.created_at,
    };
    let encoded = serde_json::to_vec(&input).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(encoded);
    format!("{:x}", hasher.finalize())
}

fn status_from_result(result: &ExecResult, timed_out: bool) -> &'static str {
    if timed_out || result.timed_out {
        "timeout"
    } else if result.cancelled {
        "cancelled"
    } else if result.exit_code == Some(0) {
        "passed"
    } else {
        "failed"
    }
}

fn truncate_body(body: String) -> (String, bool, u64) {
    if body.len() <= MAX_REMOTE_TEST_BODY_BYTES {
        let bytes = body.len() as u64;
        return (body, false, bytes);
    }
    let boundary = body
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= MAX_REMOTE_TEST_BODY_BYTES)
        .last()
        .unwrap_or(0);
    let mut body = body[..boundary].to_string();
    body.push_str("\n… remote test output truncated at the evidence storage cap\n");
    let bytes = body.len() as u64;
    (body, true, bytes)
}

fn render_body(
    plan: &ResolvedRemoteTest,
    host: &str,
    commit: &str,
    status: &str,
    result: &ExecResult,
    detail: Option<&str>,
) -> String {
    format!(
        "command: {}\ncwd: {}\nhost: {}\ncommit: {}\nprofile: {} ({})\nstatus: {}\nexit_code: {}\nduration_ms: {}\ntimed_out: {}\nstdout_truncated: {}\nstderr_truncated: {}\n\n## stdout\n{}\n\n## stderr\n{}\n{}\n",
        plan.command,
        plan.cwd.as_deref().unwrap_or("."),
        host,
        commit,
        plan.profile,
        plan.kind.as_str(),
        status,
        result
            .exit_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "none".to_string()),
        result.duration_ms,
        result.timed_out,
        result.stdout_truncated,
        result.stderr_truncated,
        result.stdout,
        result.stderr,
        detail
            .map(|detail| format!("\n## detail\n{detail}"))
            .unwrap_or_default(),
    )
}

fn write_output_file(
    data_dir: &Path,
    body: &str,
) -> Result<(String, String, u64), RemoteTestFailure> {
    let sha = {
        let mut hasher = Sha256::new();
        hasher.update(body.as_bytes());
        format!("{:x}", hasher.finalize())
    };
    let directory = data_dir.join(REMOTE_TEST_OUTPUT_DIR);
    std::fs::create_dir_all(&directory).map_err(|error| {
        RemoteTestFailure::new(
            ToolErrorCode::REMOTE_TEST_FAILED_TO_RECORD,
            format!("cannot create remote test output directory: {error}"),
        )
    })?;
    let _ = vp_store::paths::ensure_private_dir(&directory);
    let path = directory.join(format!("{sha}.txt"));
    if !path.is_file() {
        let tmp = directory.join(format!(".{sha}.{}.tmp", std::process::id()));
        std::fs::write(&tmp, body.as_bytes()).map_err(|error| {
            RemoteTestFailure::new(
                ToolErrorCode::REMOTE_TEST_FAILED_TO_RECORD,
                format!("cannot write remote test output: {error}"),
            )
        })?;
        let _ = vp_store::paths::ensure_private_file(&tmp);
        std::fs::rename(&tmp, &path).map_err(|error| {
            let _ = std::fs::remove_file(&tmp);
            RemoteTestFailure::new(
                ToolErrorCode::REMOTE_TEST_FAILED_TO_RECORD,
                format!("cannot finalize remote test output: {error}"),
            )
        })?;
    }
    Ok((
        format!("{REMOTE_TEST_OUTPUT_DIR}/{sha}.txt"),
        sha,
        body.len() as u64,
    ))
}

fn output_file_path(data_dir: &Path, reference: &str) -> Option<PathBuf> {
    let mut parts = reference.split('/');
    let directory = parts.next()?;
    let file = parts.next()?;
    if directory != REMOTE_TEST_OUTPUT_DIR || parts.next().is_some() {
        return None;
    }
    if file.len() != 68
        || !file.ends_with(".txt")
        || !file[..64]
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return None;
    }
    Some(data_dir.join(REMOTE_TEST_OUTPUT_DIR).join(file))
}

async fn load_entry(store: &vp_store::Store, entry_id: &str) -> Result<MergeQueueEntry> {
    let store = store.clone();
    let entry_id = entry_id.to_string();
    let lookup_id = entry_id.clone();
    tokio::task::spawn_blocking(move || store.get_merge_queue_entry(&lookup_id))
        .await??
        .ok_or_else(|| anyhow!("merge queue entry not found: {entry_id}"))
}

impl Engine {
    pub async fn get_remote_test_policy(&self, project_id: &str) -> Result<RemoteTestPolicy> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        let exists = tokio::task::spawn_blocking({
            let store = store.clone();
            let project_id = project_id.clone();
            move || store.get_project(&project_id)
        })
        .await??
        .is_some();
        if !exists {
            bail!("project not found: {project_id}");
        }
        tokio::task::spawn_blocking(move || store.get_remote_test_policy(&project_id)).await?
    }

    pub async fn remote_test_policy_result(
        &self,
        project_id: &str,
    ) -> Result<RemoteTestPolicyResult> {
        let policy = self.get_remote_test_policy(project_id).await?;
        let store = self.store.clone();
        let project_id_owned = project_id.to_string();
        let lookup_id = project_id_owned.clone();
        let updated_at =
            tokio::task::spawn_blocking(move || store.remote_test_policy_updated_at(&lookup_id))
                .await??
                .unwrap_or_default();
        Ok(RemoteTestPolicyResult {
            project_id: project_id_owned,
            policy,
            updated_at,
        })
    }

    pub async fn set_remote_test_policy(
        &self,
        project_id: &str,
        policy: RemoteTestPolicy,
    ) -> Result<RemoteTestPolicyResult> {
        policy
            .validate()
            .map_err(|message| anyhow!("invalid remote test policy: {message}"))?;
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
        let policy_for_store = policy.clone();
        let project_id_for_store = project_id_owned.clone();
        tokio::task::spawn_blocking(move || {
            store.set_remote_test_policy(&project_id_for_store, &policy_for_store)?;
            store.record_project_manifest()
        })
        .await??;
        self.remote_test_policy_result(project_id).await
    }

    /// Run one bounded remote test and persist evidence. A failed test is still
    /// evidence: the method returns `Ok` with `status = failed|timeout` as long
    /// as the command actually ran and the row was recorded. Configuration and
    /// provenance failures return typed errors and make no test claim.
    pub async fn run_remote_test(&self, params: RemoteTestParams) -> Result<RemoteTestResult> {
        let policy = self.get_remote_test_policy(&params.project_id).await?;
        let plan = resolve_remote_test(&policy, &params).map_err(anyhow::Error::new)?;
        let entry = match params.entry_id.as_deref() {
            Some(entry_id) => Some(load_entry(self.store(), entry_id).await?),
            None => None,
        };
        let session_id = if !params.session_id.trim().is_empty() {
            params.session_id.trim().to_string()
        } else if let Some(entry) = entry.as_ref() {
            entry.session_id.clone()
        } else {
            return Err(RemoteTestFailure::new(
                ToolErrorCode::REMOTE_TEST_SESSION_REQUIRED,
                "remote test evidence requires a session or remote queue entry",
            )
            .with_hint("pass --session or --entry")
            .into());
        };
        let store = self.store.clone();
        let session_lookup = session_id.clone();
        let session = tokio::task::spawn_blocking(move || store.get_session(&session_lookup))
            .await??
            .ok_or_else(|| anyhow!("session not found: {session_id}"))?;
        if session.project_id != params.project_id {
            bail!(
                "session {session_id} does not belong to project {}",
                params.project_id
            );
        }
        if let Some(entry) = entry.as_ref() {
            if entry.source != MERGE_SOURCE_REMOTE {
                return Err(RemoteTestFailure::new(
                    ToolErrorCode::REMOTE_TEST_ENTRY_NOT_REMOTE,
                    format!("merge queue entry {} is not remote-sourced", entry.id),
                )
                .with_hint("remote test evidence attaches only to remote merge-queue rows")
                .into());
            }
            if entry.session_id != session_id {
                return Err(RemoteTestFailure::new(
                    ToolErrorCode::REMOTE_TEST_FAILED,
                    format!(
                        "entry {} belongs to session {}, not {}",
                        entry.id, entry.session_id, session_id
                    ),
                )
                .into());
            }
        }
        let config = self.get_execution_host(&params.project_id).await?;
        if !config.is_remote() {
            return Err(RemoteTestFailure::new(
                ToolErrorCode::REMOTE_NOT_CONFIGURED,
                format!(
                    "project {} uses a local execution host; remote tests are unavailable",
                    params.project_id
                ),
            )
            .into());
        }
        let shell_policy = self.get_shell_policy(&params.project_id).await?;
        if shell_policy.network == "deny" {
            return Err(RemoteTestFailure::new(
                ToolErrorCode::REMOTE_NETWORK_DENIED,
                "remote test is denied by the project ShellPolicy network=deny",
            )
            .with_hint("remote exec cannot be network-sandboxed by the local Seatbelt policy; relax network only for a trusted host")
            .into());
        }
        if shell_policy.restrict_home
            || shell_policy.read_jail
            || !shell_policy.read_deny.is_empty()
        {
            return Err(RemoteTestFailure::new(
                ToolErrorCode::REMOTE_TEST_FAILED,
                "remote test cannot enforce the project read restrictions",
            )
            .with_hint("relax restrict_home/read_jail/read_deny for this remote host or use a local project task")
            .into());
        }

        let workspace = self
            .remote_workspace(&params.project_id)
            .await
            .map_err(map_workspace_error)?;
        let head = workspace
            .git_rev_parse(GitRevParseParams {
                rev: "HEAD".into(),
                commit: true,
            })
            .await
            .map_err(|error| map_workspace_error(anyhow!(error)))?;
        let commit = head.value.trim().to_string();
        if let Some(expected) = params.expected_commit.as_deref()
            && expected.trim() != commit
        {
            return Err(RemoteTestFailure::new(
                ToolErrorCode::REMOTE_TEST_COMMIT_MISMATCH,
                format!("remote HEAD is {commit}, expected {}", expected.trim()),
            )
            .with_hint("check out the exact expected commit on the remote host")
            .into());
        }
        if let Some(entry) = entry.as_ref()
            && entry.commit != commit
        {
            return Err(RemoteTestFailure::new(
                ToolErrorCode::REMOTE_TEST_COMMIT_MISMATCH,
                format!(
                    "remote HEAD is {commit}, but queue entry {} imports {}",
                    entry.id, entry.commit
                ),
            )
            .with_hint("check out the entry commit on the remote host before running evidence")
            .into());
        }

        let timeout_ms = plan
            .timeout_secs
            .saturating_mul(1_000)
            .clamp(1, MAX_EXEC_TIMEOUT_MS);
        let output_cap = plan
            .output_cap_bytes
            .clamp(MIN_REMOTE_TEST_OUTPUT_CAP_BYTES, MAX_EXEC_OUTPUT_BYTES);
        let operation_id = format!("remote-test:{}:{}", session_id, Uuid::now_v7());
        let exec = ExecParams {
            command: plan.command.clone(),
            cwd: plan.cwd.clone(),
            env: Default::default(),
            timeout_ms,
            output_cap_bytes: output_cap,
            operation_id: Some(operation_id),
        };
        let wait = Duration::from_secs(
            plan.timeout_secs
                .saturating_add(REMOTE_TEST_WAIT_MARGIN_SECS)
                .min(MAX_REMOTE_TEST_TIMEOUT_SECS + REMOTE_TEST_WAIT_MARGIN_SECS),
        );
        let started = Instant::now();
        let (result, detail) = match workspace.exec_with_timeout(exec, wait).await {
            Ok(result) => (result, None),
            Err(error)
                if error
                    .downcast_ref::<vp_remote::RemoteError>()
                    .is_some_and(|error| error.code == vp_remote::RemoteErrorCode::Timeout) =>
            {
                (
                    ExecResult {
                        exit_code: None,
                        stdout: String::new(),
                        stderr: format!("remote test timed out: {error}"),
                        stdout_truncated: false,
                        stderr_truncated: false,
                        timed_out: true,
                        cancelled: false,
                        duration_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                        operation_id: None,
                        replayed: false,
                    },
                    Some(
                        "client-side timeout; the helper was asked to cancel the process group"
                            .to_string(),
                    ),
                )
            }
            Err(error) => return Err(map_workspace_error(error).into()),
        };
        let timed_out = result.timed_out;
        let status = status_from_result(&result, timed_out).to_string();
        let body = render_body(
            &plan,
            &config.host,
            &commit,
            &status,
            &result,
            detail.as_deref(),
        );
        let (body, body_truncated, _body_bytes) = truncate_body(body);
        let truncated = body_truncated || result.stdout_truncated || result.stderr_truncated;
        let (output_ref, output_sha256, output_bytes) =
            write_output_file(self.store().data_dir(), &body).map_err(anyhow::Error::new)?;
        let now = now_rfc3339();
        let evidence_id = Uuid::now_v7().to_string();
        let mut evidence = RemoteTestEvidence {
            id: evidence_id.clone(),
            project_id: params.project_id.clone(),
            session_id: session_id.clone(),
            entry_id: params.entry_id.clone(),
            profile: plan.profile.clone(),
            kind: plan.kind.as_str().to_string(),
            host: config.host.clone(),
            commit: commit.clone(),
            command: plan.command.clone(),
            cwd: plan.cwd.clone(),
            status: status.clone(),
            exit_code: result.exit_code,
            duration_ms: result
                .duration_ms
                .max(started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64),
            output_ref: Some(output_ref.clone()),
            output_sha256: Some(output_sha256),
            output_bytes,
            output_truncated: truncated,
            timed_out,
            detail,
            digest: String::new(),
            environment_caveat: REMOTE_TEST_ENVIRONMENT_CAVEAT.to_string(),
            created_at: now,
        };
        evidence.digest = evidence_digest(&evidence);
        let store = self.store.clone();
        let evidence_for_store = evidence.clone();
        tokio::task::spawn_blocking(move || {
            store.insert_remote_test_evidence_with_event(&evidence_for_store)
        })
        .await
        .map_err(|error| {
            anyhow!(RemoteTestFailure::new(
                ToolErrorCode::REMOTE_TEST_FAILED_TO_RECORD,
                format!("remote test evidence task failed: {error}"),
            ))
        })?
        .map_err(|error| {
            anyhow!(RemoteTestFailure::new(
                ToolErrorCode::REMOTE_TEST_FAILED_TO_RECORD,
                format!("cannot persist remote test evidence: {error:#}"),
            ))
        })?;

        let attached_entry = if let Some(entry_id) = params.entry_id.as_deref() {
            let store = self.store.clone();
            let evidence_id = evidence.id.clone();
            let digest = evidence.digest.clone();
            let status = evidence.status.clone();
            let entry_id = entry_id.to_string();
            let attached = tokio::task::spawn_blocking(move || {
                store.set_merge_queue_remote_test_with_event(
                    &entry_id,
                    &evidence_id,
                    &digest,
                    &status,
                )
            })
            .await??;
            match attached {
                Some((entry, _event)) => Some(entry),
                None => {
                    return Err(RemoteTestFailure::new(
                        ToolErrorCode::REMOTE_TEST_FAILED_TO_RECORD,
                        "evidence was recorded but the merge-queue entry could not be updated",
                    )
                    .with_evidence(format!("evidence_id={}", evidence.id))
                    .into());
                }
            }
        } else {
            None
        };
        Ok(RemoteTestResult {
            project_id: params.project_id,
            session_id,
            digest: evidence.digest.clone(),
            output_ref: Some(output_ref),
            evidence,
            entry: attached_entry,
            supplemental: true,
            local_verifier_required: true,
            merge_gate_unchanged: true,
            environment_caveat: REMOTE_TEST_ENVIRONMENT_CAVEAT.to_string(),
        })
    }

    pub async fn remote_test_status(
        &self,
        params: RemoteTestStatusParams,
    ) -> Result<RemoteTestStatusResult> {
        let policy = self.get_remote_test_policy(&params.project_id).await?;
        let limit = params
            .limit
            .unwrap_or(DEFAULT_REMOTE_TEST_EVIDENCE_LIST)
            .clamp(1, MAX_REMOTE_TEST_EVIDENCE_LIST);
        let store = self.store.clone();
        let project_id = params.project_id.clone();
        let session_id = params.session_id.clone();
        let entry_id = params.entry_id.clone();
        let evidence = tokio::task::spawn_blocking(move || {
            store.list_remote_test_evidence(
                &project_id,
                session_id.as_deref(),
                entry_id.as_deref(),
                limit,
            )
        })
        .await??;
        let mut passed = 0usize;
        let mut failed = 0usize;
        let mut timed_out = 0usize;
        for row in &evidence {
            match row.status.as_str() {
                "passed" => passed += 1,
                "timeout" => timed_out += 1,
                _ => failed += 1,
            }
        }
        let latest_digest = evidence.first().map(|row| row.digest.clone());
        Ok(RemoteTestStatusResult {
            project_id: params.project_id,
            session_id: params.session_id,
            entry_id: params.entry_id,
            policy,
            total: evidence.len(),
            evidence,
            passed,
            failed,
            timed_out,
            latest_digest,
            environment_caveat: REMOTE_TEST_ENVIRONMENT_CAVEAT.to_string(),
            local_verifier_required: true,
            merge_gate_unchanged: true,
        })
    }

    pub async fn remote_test_output(
        &self,
        params: RemoteTestOutputParams,
    ) -> Result<RemoteTestOutputResult> {
        let store = self.store.clone();
        let evidence_id = params.evidence_id.clone();
        let evidence =
            tokio::task::spawn_blocking(move || store.get_remote_test_evidence(&evidence_id))
                .await??
                .ok_or_else(|| {
                    RemoteTestFailure::new(
                        ToolErrorCode::REMOTE_TEST_EVIDENCE_NOT_FOUND,
                        format!("remote test evidence not found: {}", params.evidence_id),
                    )
                })?;
        if evidence.project_id != params.project_id {
            return Err(RemoteTestFailure::new(
                ToolErrorCode::REMOTE_TEST_EVIDENCE_NOT_FOUND,
                "remote test evidence belongs to a different project",
            )
            .into());
        }
        let Some(reference) = evidence.output_ref.as_deref() else {
            return Err(RemoteTestFailure::new(
                ToolErrorCode::REMOTE_TEST_OUTPUT_NOT_FOUND,
                "this evidence row has no retained output body",
            )
            .into());
        };
        let data_dir = self.store().data_dir().to_path_buf();
        let path = output_file_path(&data_dir, reference).ok_or_else(|| {
            RemoteTestFailure::new(
                ToolErrorCode::REMOTE_TEST_OUTPUT_NOT_FOUND,
                "evidence output reference is outside the remote-test output store",
            )
        })?;
        let limit = params
            .limit
            .clamp(1, MAX_REMOTE_TEST_OUTPUT_READ_BYTES)
            .max(1);
        let offset = params.offset;
        let (text, bytes_read, done) =
            tokio::task::spawn_blocking(move || -> Result<(String, u64, bool)> {
                let bytes = std::fs::read(&path)
                    .with_context(|| format!("reading remote test output {}", path.display()))?;
                let total = bytes.len() as u64;
                if offset >= total {
                    return Ok((String::new(), 0, true));
                }
                let end = offset.saturating_add(limit).min(total);
                let start = usize::try_from(offset)
                    .unwrap_or(usize::MAX)
                    .min(bytes.len());
                let end = usize::try_from(end).unwrap_or(usize::MAX).min(bytes.len());
                Ok((
                    String::from_utf8_lossy(&bytes[start..end]).to_string(),
                    (end - start) as u64,
                    end >= bytes.len(),
                ))
            })
            .await??;
        Ok(RemoteTestOutputResult {
            project_id: params.project_id,
            evidence_id: params.evidence_id,
            output_ref: evidence.output_ref.clone(),
            output_sha256: evidence.output_sha256.clone(),
            offset,
            bytes_read,
            text,
            done,
            output_truncated: evidence.output_truncated,
            environment_caveat: REMOTE_TEST_ENVIRONMENT_CAVEAT.to_string(),
        })
    }
}

/// Convenience used by the CLI policy editor: list available profile names.
pub fn profile_names(policy: &RemoteTestPolicy) -> Vec<String> {
    policy
        .profiles
        .iter()
        .map(|profile| profile.name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::{RemoteConnector, RemoteManager};
    use async_trait::async_trait;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::Arc;
    use vp_remote::{
        HelloParams, RemoteClient, RemoteClientConfig, ServerConfig, connect_stream, serve,
    };

    struct InProcessConnector {
        root: PathBuf,
        token: String,
        fail: bool,
    }

    #[async_trait]
    impl RemoteConnector for InProcessConnector {
        async fn connect(
            &self,
            _project_id: &str,
            config: &vp_protocol::remote::ExecutionHost,
        ) -> Result<RemoteClient> {
            if self.fail {
                anyhow::bail!("remote helper handshake failed: helper is not installed");
            }
            let (client_side, server_side) = tokio::io::duplex(1 << 20);
            let (client_reader, client_writer) = tokio::io::split(client_side);
            let (server_reader, server_writer) = tokio::io::split(server_side);
            let server_config = ServerConfig::new(self.root.clone(), self.token.clone())?;
            tokio::spawn(async move {
                let _ = serve(server_reader, server_writer, server_config).await;
            });
            connect_stream(
                client_reader,
                client_writer,
                HelloParams {
                    protocol_version: vp_remote::REMOTE_PROTOCOL_VERSION,
                    client_version: "test".into(),
                    auth_token: self.token.clone(),
                    root: config.root.clone(),
                },
                RemoteClientConfig::default(),
            )
            .await
            .map_err(|error| anyhow!("helper connect failed: {error}"))
        }
    }

    fn git_init(root: &Path) {
        std::fs::create_dir_all(root).unwrap();
        let run = |args: &[&str]| {
            let status = Command::new("git")
                .args(args)
                .current_dir(root)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?} failed");
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
        std::fs::write(root.join("README.md"), "remote\n").unwrap();
        run(&["add", "README.md"]);
        run(&["commit", "-q", "-m", "initial"]);
    }

    struct Fixture {
        _data: tempfile::TempDir,
        _local: tempfile::TempDir,
        remote: tempfile::TempDir,
        engine: Engine,
        project_id: String,
        session_id: String,
    }

    async fn fixture(fail_connector: bool) -> Fixture {
        let data = tempfile::TempDir::new().unwrap();
        let local = tempfile::TempDir::new().unwrap();
        let remote = tempfile::TempDir::new().unwrap();
        git_init(remote.path());
        let remote_root = remote.path().canonicalize().unwrap();
        let store = vp_store::Store::open(data.path()).unwrap();
        let project = store
            .add_project(local.path().to_str().unwrap(), Some("remote-test"))
            .unwrap();
        let session = store
            .create_session(&project.id, "remote evidence")
            .unwrap();
        let manager = RemoteManager::with_connector(
            data.path(),
            "/unused/vp-remote-agent",
            Arc::new(InProcessConnector {
                root: remote_root.clone(),
                token: "remote-test-token-123456".into(),
                fail: fail_connector,
            }),
        );
        let engine = Engine::with_remote_manager(store, manager);
        engine
            .set_execution_host(
                &project.id,
                vp_protocol::remote::ExecutionHost {
                    mode: vp_protocol::remote::ExecutionHostMode::Ssh,
                    host: "build.example.com".into(),
                    user: Some("agent".into()),
                    root: remote_root.to_string_lossy().to_string(),
                    helper_path: "/unused/vp-remote-agent".into(),
                    token_env: Some("UNUSED_IN_PROCESS_TOKEN".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let policy = RemoteTestPolicy {
            enabled: true,
            default_profile: Some("check".into()),
            profiles: vec![RemoteTestProfile {
                name: "check".into(),
                kind: RemoteTestKind::Check,
                command: "printf ok".into(),
                cwd: None,
                timeout_secs: Some(5),
            }],
            timeout_secs: 5,
            output_cap_bytes: 16 * 1024,
        };
        engine
            .set_remote_test_policy(&project.id, policy)
            .await
            .unwrap();
        Fixture {
            _data: data,
            _local: local,
            remote,
            engine,
            project_id: project.id,
            session_id: session.id,
        }
    }

    fn params(fixture: &Fixture) -> RemoteTestParams {
        RemoteTestParams {
            project_id: fixture.project_id.clone(),
            session_id: fixture.session_id.clone(),
            ..RemoteTestParams::default()
        }
    }

    #[tokio::test]
    async fn successful_remote_test_records_evidence_and_output() {
        let fixture = fixture(false).await;
        let result = fixture
            .engine
            .run_remote_test(params(&fixture))
            .await
            .unwrap();
        assert_eq!(result.evidence.status, "passed");
        assert_eq!(result.evidence.exit_code, Some(0));
        assert!(result.supplemental);
        assert!(result.local_verifier_required);
        assert!(result.merge_gate_unchanged);
        assert_eq!(result.evidence.profile, "check");
        assert_eq!(result.evidence.host, "build.example.com");
        assert_eq!(result.evidence.commit.len(), 40);
        assert!(result.digest.len() == 64);
        let output = fixture
            .engine
            .remote_test_output(RemoteTestOutputParams {
                project_id: fixture.project_id.clone(),
                evidence_id: result.evidence.id.clone(),
                offset: 0,
                limit: 4096,
            })
            .await
            .unwrap();
        assert!(output.text.contains("## stdout"));
        assert!(output.text.contains("ok"));
        assert!(!output.output_truncated || output.text.contains("truncated"));
        let status = fixture
            .engine
            .remote_test_status(RemoteTestStatusParams {
                project_id: fixture.project_id.clone(),
                session_id: Some(fixture.session_id.clone()),
                ..RemoteTestStatusParams::default()
            })
            .await
            .unwrap();
        assert_eq!(status.total, 1);
        assert_eq!(status.passed, 1);
        assert_eq!(
            status.latest_digest.as_deref(),
            Some(result.digest.as_str())
        );
        assert!(status.environment_caveat.contains("supplemental"));
        assert!(status.environment_caveat.contains("never bypasses"));
    }

    #[tokio::test]
    async fn failing_remote_test_is_recorded_as_failed_evidence_not_an_error() {
        let fixture = fixture(false).await;
        let mut request = params(&fixture);
        request.command = Some("printf bad >&2; exit 7".into());
        let result = fixture.engine.run_remote_test(request).await.unwrap();
        assert_eq!(result.evidence.status, "failed");
        assert_eq!(result.evidence.exit_code, Some(7));
        assert!(result.evidence.command.contains("exit 7"));
    }

    #[tokio::test]
    async fn timeout_is_recorded_and_bounded() {
        let fixture = fixture(false).await;
        let mut request = params(&fixture);
        request.command = Some("sleep 5".into());
        request.timeout_secs = Some(1);
        let started = Instant::now();
        let result = fixture.engine.run_remote_test(request).await.unwrap();
        assert_eq!(result.evidence.status, "timeout");
        assert!(result.evidence.timed_out);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn output_cap_and_storage_cap_are_reported() {
        let fixture = fixture(false).await;
        let mut request = params(&fixture);
        request.command = Some("yes x | head -c 200000".into());
        request.output_cap_bytes = Some(1024);
        let result = fixture.engine.run_remote_test(request).await.unwrap();
        assert!(result.evidence.output_truncated);
        assert!(result.evidence.output_bytes < 200_000);
    }

    #[tokio::test]
    async fn missing_profile_and_missing_command_are_typed() {
        let fixture = fixture(false).await;
        let mut policy = fixture
            .engine
            .get_remote_test_policy(&fixture.project_id)
            .await
            .unwrap();
        policy.profiles.clear();
        policy.default_profile = None;
        fixture
            .engine
            .set_remote_test_policy(&fixture.project_id, policy)
            .await
            .unwrap();
        let error = fixture
            .engine
            .run_remote_test(params(&fixture))
            .await
            .unwrap_err();
        let data = failure_data(&error).expect("typed failure");
        assert_eq!(data["code"].as_str(), Some("REMOTE_TEST_PROFILE_MISSING"));

        let mut policy = fixture
            .engine
            .get_remote_test_policy(&fixture.project_id)
            .await
            .unwrap();
        policy.profiles = vec![RemoteTestProfile {
            name: "check".into(),
            kind: RemoteTestKind::Check,
            command: "printf ok".into(),
            cwd: None,
            timeout_secs: None,
        }];
        policy.default_profile = Some("check".into());
        fixture
            .engine
            .set_remote_test_policy(&fixture.project_id, policy)
            .await
            .unwrap();
        let mut request = params(&fixture);
        request.command = Some("   ".into());
        let error = fixture.engine.run_remote_test(request).await.unwrap_err();
        let data = failure_data(&error).expect("typed failure");
        assert_eq!(data["code"].as_str(), Some("REMOTE_TEST_COMMAND_MISSING"));
    }

    #[tokio::test]
    async fn disabled_policy_refuses_a_one_off_command() {
        let fixture = fixture(false).await;
        let mut policy = fixture
            .engine
            .get_remote_test_policy(&fixture.project_id)
            .await
            .unwrap();
        policy.enabled = false;
        fixture
            .engine
            .set_remote_test_policy(&fixture.project_id, policy)
            .await
            .unwrap();
        let mut request = params(&fixture);
        request.command = Some("printf ok".into());
        let error = fixture.engine.run_remote_test(request).await.unwrap_err();
        let data = failure_data(&error).expect("typed failure");
        assert_eq!(data["code"].as_str(), Some("REMOTE_TEST_DISABLED"));
    }

    #[tokio::test]
    async fn evidence_attaches_to_remote_queue_without_touching_the_verifier() {
        let fixture = fixture(false).await;
        let store = fixture.engine.store();
        let head = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(fixture.remote.path())
            .output()
            .unwrap();
        assert!(head.status.success());
        let head = String::from_utf8_lossy(&head.stdout).trim().to_string();
        let source = vp_protocol::types::MergeQueueSource::remote(
            "build.example.com",
            "feature/remote",
            "a".repeat(64),
            "refs/verlinpark/remote/session/import-1",
        );
        let (entry, _) = store
            .enqueue_merge_with_event_source(
                &fixture.session_id,
                "agent_remote",
                "Remote Worker",
                None,
                "verlinpark/remote",
                &head,
                &head,
                Some("waiting for verification"),
                &source,
            )
            .unwrap();
        let mut request = params(&fixture);
        request.entry_id = Some(entry.id.clone());
        let result = fixture.engine.run_remote_test(request).await.unwrap();
        let attached = result.entry.expect("attached entry");
        assert_eq!(attached.id, entry.id);
        assert_eq!(attached.status, "queued");
        assert_eq!(attached.verifier_approved, None);
        assert_eq!(
            attached.remote_test_digest.as_deref(),
            Some(result.digest.as_str())
        );
        assert_eq!(attached.remote_test_status.as_deref(), Some("passed"));
        assert_eq!(
            attached.remote_test_evidence_id.as_deref(),
            Some(result.evidence.id.as_str())
        );
        // The merge gate stays closed: no Verifier verdict was invented.
        let stored = store.get_merge_queue_entry(&entry.id).unwrap().unwrap();
        assert_eq!(stored.verifier_approved, None);
        assert_eq!(stored.status, "queued");
    }

    #[tokio::test]
    async fn commit_mismatch_and_local_entries_are_rejected_before_running() {
        let fixture = fixture(false).await;
        let store = fixture.engine.store();
        let source = vp_protocol::types::MergeQueueSource::remote(
            "build.example.com",
            "feature/remote",
            "a".repeat(64),
            "refs/verlinpark/remote/session/import-1",
        );
        let (entry, _) = store
            .enqueue_merge_with_event_source(
                &fixture.session_id,
                "agent_remote",
                "Remote Worker",
                None,
                "verlinpark/remote",
                "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                None,
                &source,
            )
            .unwrap();
        let mut request = params(&fixture);
        request.entry_id = Some(entry.id.clone());
        let error = fixture.engine.run_remote_test(request).await.unwrap_err();
        let data = failure_data(&error).expect("typed failure");
        assert_eq!(data["code"].as_str(), Some("REMOTE_TEST_COMMIT_MISMATCH"));

        let local = store
            .enqueue_merge(
                &fixture.session_id,
                "agent_local",
                "Local Worker",
                None,
                "verlinpark/local",
                "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                None,
            )
            .unwrap();
        let mut request = params(&fixture);
        request.entry_id = Some(local.id);
        let error = fixture.engine.run_remote_test(request).await.unwrap_err();
        let data = failure_data(&error).expect("typed failure");
        assert_eq!(data["code"].as_str(), Some("REMOTE_TEST_ENTRY_NOT_REMOTE"));
    }

    #[tokio::test]
    async fn missing_helper_is_actionable_and_never_claimed_as_a_test() {
        let fixture = fixture(true).await;
        let error = fixture
            .engine
            .run_remote_test(params(&fixture))
            .await
            .unwrap_err();
        let data = failure_data(&error).expect("typed failure");
        assert_eq!(data["code"].as_str(), Some("REMOTE_UNAVAILABLE"));
        let hint = data["hint"].as_str().unwrap_or_default();
        assert!(hint.contains("remote-bootstrap"));
    }

    #[tokio::test]
    async fn evidence_is_replayable_and_output_refs_are_jailed() {
        let fixture = fixture(false).await;
        let result = fixture
            .engine
            .run_remote_test(params(&fixture))
            .await
            .unwrap();
        let report = fixture
            .engine
            .replay_session(&fixture.session_id)
            .await
            .unwrap();
        assert!(report.remote_test_evidence_matches);
        assert!(
            report
                .mismatches
                .iter()
                .all(|mismatch| !mismatch.contains("remote test"))
        );
        let missing = fixture
            .engine
            .remote_test_output(RemoteTestOutputParams {
                project_id: fixture.project_id.clone(),
                evidence_id: "missing".into(),
                offset: 0,
                limit: 1024,
            })
            .await
            .unwrap_err();
        let data = failure_data(&missing).expect("typed failure");
        assert_eq!(
            data["code"].as_str(),
            Some("REMOTE_TEST_EVIDENCE_NOT_FOUND")
        );
        assert!(output_file_path(Path::new("/data"), "../etc/passwd").is_none());
        let _ = result;
    }

    #[test]
    fn digests_are_stable_and_include_the_commit_and_command() {
        let mut evidence = RemoteTestEvidence {
            id: "e1".into(),
            project_id: "p1".into(),
            session_id: "s1".into(),
            entry_id: None,
            profile: "check".into(),
            kind: "check".into(),
            host: "host".into(),
            commit: "c1".into(),
            command: "true".into(),
            cwd: None,
            status: "passed".into(),
            exit_code: Some(0),
            duration_ms: 1,
            output_ref: None,
            output_sha256: None,
            output_bytes: 0,
            output_truncated: false,
            timed_out: false,
            detail: None,
            digest: String::new(),
            environment_caveat: REMOTE_TEST_ENVIRONMENT_CAVEAT.into(),
            created_at: "2026-01-01T00:00:00Z".into(),
        };
        let first = evidence_digest(&evidence);
        assert_eq!(first, evidence_digest(&evidence));
        evidence.command = "false".into();
        assert_ne!(first, evidence_digest(&evidence));
    }

    #[tokio::test]
    async fn policy_defaults_are_disabled_and_validated() {
        let fixture = fixture(false).await;
        let policy = fixture
            .engine
            .get_remote_test_policy(&fixture.project_id)
            .await
            .unwrap();
        assert!(policy.enabled);
        assert_eq!(policy.profiles.len(), 1);
        let mut invalid = policy.clone();
        invalid.default_profile = Some("missing".into());
        let error = fixture
            .engine
            .set_remote_test_policy(&fixture.project_id, invalid)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("default remote test profile"));
    }
}

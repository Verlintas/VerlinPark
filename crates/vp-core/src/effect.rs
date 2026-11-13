//! Local effect receipts and postcondition probes.
//!
//! A receipt is **local evidence**, not provider exactly-once. The runtime
//! writes `intent` before a call can change the world, then writes `done`,
//! `failed`, or `unknown` from a postcondition probe after the call. A `done`
//! receipt replays its stored evidence instead of re-executing; `unknown`
//! blocks auto-retry and merge integration until an operator reconciles it.
//!
//! The module deliberately distinguishes what it can prove:
//!
//! - workspace writes and Git commits are probed against the local filesystem
//!   and Git object store;
//! - remote writes use the helper's post-write SHA-256 (the remote stat/hash)
//!   and never claim that a remote service deduplicated the call;
//! - HTTP responses prove the local observation of a response, not a remote
//!   side effect;
//! - raw shell and MCP mutation have no stable postcondition and therefore
//!   settle as `unknown` unless the call declares an explicit probe;
//! - deploy/webhook targets are modeled for future producers but no current
//!   runtime action invents a success receipt for them.
//!
//! Everything here narrows existing behavior. Approval, Seatbelt, path jail,
//! write scope, Verifier, Integrator, capacity, and budget gates are unchanged.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Component, Path};

use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use vp_protocol::types::{EffectCounts, EffectPathEvidence, EffectPolicy, EffectShellMode};
pub use vp_protocol::types::{EffectEvidence, EffectReceipt, EffectState, EffectTarget};

use crate::approvals::canonical_json;
use crate::permissions::RiskLevel;
use crate::tools::ToolOutput;

/// Stable event kinds. Each payload contains `{"receipt": <EffectReceipt>}`
/// and, for a reconcile, the bounded operator label.
pub use vp_protocol::types::{
    EFFECT_FAILED_EVENT, EFFECT_INTENT_EVENT, EFFECT_RECONCILED_EVENT, EFFECT_SETTLED_EVENT,
    EFFECT_UNKNOWN_EVENT,
};

/// Maximum canonical target key length.
const MAX_TARGET_KEY_CHARS: usize = 512;
/// Maximum number of path evidence rows persisted for one receipt.
pub const MAX_EFFECT_PATHS: usize = 32;
/// Maximum path length persisted in evidence.
const MAX_EFFECT_PATH_CHARS: usize = 512;
/// Maximum SHA-256 digest length accepted from a tool or operator.
const MAX_HASH_CHARS: usize = 128;
/// Maximum short note persisted in evidence.
const MAX_EFFECT_NOTE_CHARS: usize = 500;
/// Maximum external id (PR URL, remote commit) persisted.
pub const MAX_EXTERNAL_ID_CHARS: usize = 2_048;
/// Maximum bytes of one file hashed by a probe. Larger files are still probed
/// for existence, but their content hash is reported as unavailable.
const MAX_PROBED_FILE_BYTES: u64 = 256 * 1024 * 1024;
/// Maximum number of changed files inspected from one apply_patch result.
const MAX_PATCH_PROBE_FILES: usize = 32;
/// Maximum canonical JSON bytes hashed for a response digest.
const MAX_RESPONSE_HASH_BYTES: usize = 256 * 1024;

/// Canonical effect-kind names. They are persisted and are part of the wire
/// contract; renaming one requires a migration and an ADR note.
pub mod kinds {
    pub const WORKSPACE_WRITE: &str = "workspace_write";
    pub const WORKSPACE_PATCH: &str = "workspace_patch";
    pub const WORKSPACE_MANAGE: &str = "workspace_manage";
    pub const PROJECT_TASK: &str = "project_task";
    pub const GIT_COMMIT: &str = "git_commit";
    pub const GIT_MERGE: &str = "git_merge";
    pub const GIT_PUSH: &str = "git_push";
    pub const FORGE_PR: &str = "forge_pr";
    pub const REMOTE_WRITE: &str = "remote_write";
    pub const REMOTE_EXEC: &str = "remote_exec";
    pub const REMOTE_INTEGRATION: &str = "remote_integration";
    pub const HTTP_REQUEST: &str = "http_request";
    pub const SHELL: &str = "shell";
    pub const MCP_TOOL: &str = "mcp_tool";
    pub const DEPLOY: &str = "deploy";
    pub const WEBHOOK: &str = "webhook";
    pub const UNKNOWN: &str = "unknown";
}

/// Stable probe identifiers persisted in `evidence.probe`.
pub mod probes {
    pub const WORKSPACE_SHA256: &str = "workspace_sha256";
    pub const WORKSPACE_ABSENCE: &str = "workspace_absence";
    pub const WORKSPACE_PATCH: &str = "workspace_patch_paths";
    pub const WORKSPACE_MANAGE: &str = "workspace_manage";
    pub const PROJECT_TASK_RESULT: &str = "project_task_result";
    pub const GIT_REV_PARSE: &str = "git_rev_parse";
    pub const GIT_LS_REMOTE: &str = "git_ls_remote";
    pub const GIT_MERGE_HEAD: &str = "git_merge_head";
    pub const FORGE_RESPONSE: &str = "forge_response";
    pub const REMOTE_HELPER_HASH: &str = "remote_helper_hash";
    pub const HTTP_STATUS: &str = "http_status_body_hash";
    pub const DECLARED_FILE_HASH: &str = "declared_file_hash";
    pub const DECLARED_FILE_ABSENT: &str = "declared_file_absent";
    pub const DECLARED_GIT_HEAD: &str = "declared_git_head";
    pub const DECLARED_GIT_BRANCH: &str = "declared_git_branch";
    pub const NO_PROBE: &str = "no_postcondition_probe";
    pub const TOOL_ERROR: &str = "tool_error";
    pub const OPERATOR_RECONCILE: &str = "operator_reconcile";
}

/// Stable evidence reasons. They explain a probe result without copying tool
/// output, command text, or raw URLs into an event.
pub mod reasons {
    pub const TOOL_REPORTED_FAILURE: &str = "tool_reported_failure";
    pub const POSTCONDITION_MISSING: &str = "postcondition_missing";
    pub const HASH_MISMATCH: &str = "hash_mismatch";
    pub const PROBE_INCOMPLETE: &str = "probe_incomplete";
    pub const NO_POSTCONDITION: &str = "no_postcondition";
    pub const DECLARED_PROBE: &str = "declared_probe";
    pub const OPERATOR_RECONCILED: &str = "operator_reconciled";
    pub const SERVER_ERROR: &str = "server_error";
    pub const TRANSPORT_UNKNOWN: &str = "transport_unknown";
    pub const DRY_RUN: &str = "dry_run";
    pub const REMOTE_HELPER: &str = "remote_helper";
}

/// Honest caveat attached to probes whose evidence stops short of the effect.
pub const HTTP_CAVEAT: &str = "the response proves the local client observed an HTTP status; it does not prove a remote side effect";
pub const REMOTE_CAVEAT: &str = "the remote helper reported the post-write hash; this is helper evidence, not remote exactly-once";
pub const PROJECT_TASK_CAVEAT: &str =
    "the task result is local evidence of command completion, not proof of an external side effect";

/// One explicit postcondition declared by a caller. It is the only way a raw
/// shell or MCP effect can settle as `done` without a runtime-owned probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclaredProbe {
    FileHash { path: String, sha256: String },
    FileAbsent { path: String },
    GitHead { commit: String },
    GitBranch { branch: String },
}

impl DeclaredProbe {
    pub fn parse(input: &Value) -> Result<Option<Self>> {
        let Some(value) = input.get("effect_probe") else {
            return Ok(None);
        };
        if value.is_null() {
            return Ok(None);
        }
        let object = value
            .as_object()
            .ok_or_else(|| anyhow!("effect_probe must be an object"))?;
        let kind = object
            .get("kind")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|kind| !kind.is_empty())
            .ok_or_else(|| anyhow!("effect_probe.kind is required"))?;
        match kind {
            "file_hash" => {
                let path = bounded_string(object.get("path"), MAX_EFFECT_PATH_CHARS, "path")?;
                let sha256 = bounded_string(object.get("sha256"), MAX_HASH_CHARS, "sha256")?;
                validate_sha256(&sha256)?;
                Ok(Some(Self::FileHash { path, sha256 }))
            }
            "file_absent" => {
                let path = bounded_string(object.get("path"), MAX_EFFECT_PATH_CHARS, "path")?;
                Ok(Some(Self::FileAbsent { path }))
            }
            "git_head" => {
                let commit = bounded_string(object.get("commit"), MAX_HASH_CHARS, "commit")?;
                Ok(Some(Self::GitHead { commit }))
            }
            "git_branch" => {
                let branch = bounded_string(object.get("branch"), 200, "branch")?;
                Ok(Some(Self::GitBranch { branch }))
            }
            other => Err(anyhow!("unsupported effect_probe.kind: {other}")),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::FileHash { .. } => probes::DECLARED_FILE_HASH,
            Self::FileAbsent { .. } => probes::DECLARED_FILE_ABSENT,
            Self::GitHead { .. } => probes::DECLARED_GIT_HEAD,
            Self::GitBranch { .. } => probes::DECLARED_GIT_BRANCH,
        }
    }
}

/// The probe family selected before execution. It is intentionally narrow:
/// unknown tools never get a plan and therefore never produce a receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectFamily {
    WorkspaceWrite {
        path: String,
    },
    WorkspacePatch,
    WorkspaceManage {
        action: String,
    },
    ProjectTask,
    RemoteWrite {
        path: String,
    },
    RemoteExec,
    Http {
        method: String,
        read_only: bool,
    },
    Shell {
        declared: Option<DeclaredProbe>,
    },
    Mcp {
        server: Option<String>,
        tool: Option<String>,
    },
}

/// A deterministic pre-execution effect plan. `target` and `request_hash`
/// define idempotency; the family selects the postcondition probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolEffectPlan {
    pub tool: String,
    pub target: EffectTarget,
    pub request_hash: String,
    pub provider_key: Option<String>,
    pub family: EffectFamily,
}

impl ToolEffectPlan {
    pub fn label(&self) -> String {
        self.target.label()
    }
}

/// Result of the pre-execution lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectPrepare {
    /// No prior row (or only a failed attempt): write a new intent and execute.
    Proceed(EffectReceipt),
    /// A completed receipt exists: return its evidence without executing.
    Replay(EffectReceipt),
    /// An `intent` or `unknown` row exists: block execution until reconciled.
    Blocked(EffectReceipt),
}

impl EffectPrepare {
    pub fn receipt(&self) -> &EffectReceipt {
        match self {
            Self::Proceed(receipt) | Self::Replay(receipt) | Self::Blocked(receipt) => receipt,
        }
    }
}

/// Build the intent receipt for a planned call. A failed prior attempt gets a
/// new attempt number; every other prior state is handled by `EffectPrepare`
/// before this is called.
pub fn intent_receipt(
    session_id: &str,
    task_id: Option<&str>,
    plan: &ToolEffectPlan,
    attempt: u64,
) -> EffectReceipt {
    EffectReceipt {
        id: uuid::Uuid::now_v7().to_string(),
        session_id: session_id.to_string(),
        task_id: task_id.map(str::to_string),
        attempt: attempt.max(1),
        tool: plan.tool.clone(),
        target: plan.target.clone(),
        request_hash: plan.request_hash.clone(),
        state: EffectState::Intent,
        provider_key: plan.provider_key.clone(),
        external_id: None,
        response_hash: None,
        evidence: EffectEvidence::declared(format!("probe:{}", plan.family_label())),
        created_at: vp_protocol::now_rfc3339(),
        resolved_at: None,
    }
}

/// Apply a probe to its intent receipt. The state and bounded evidence are
/// copied; the attempt identity never changes.
pub fn settled_receipt(mut intent: EffectReceipt, probe: EffectProbe) -> EffectReceipt {
    intent.state = probe.state;
    intent.evidence = probe.evidence;
    intent.external_id = probe.external_id;
    intent.response_hash = probe.response_hash;
    intent.resolved_at = Some(vp_protocol::now_rfc3339());
    intent
}

impl ToolEffectPlan {
    pub fn family_label(&self) -> &'static str {
        match &self.family {
            EffectFamily::WorkspaceWrite { .. } => "workspace_write",
            EffectFamily::WorkspacePatch => "workspace_patch",
            EffectFamily::WorkspaceManage { .. } => "workspace_manage",
            EffectFamily::ProjectTask => "project_task",
            EffectFamily::RemoteWrite { .. } => "remote_write",
            EffectFamily::RemoteExec => "remote_exec",
            EffectFamily::Http { .. } => "http",
            EffectFamily::Shell { .. } => "shell",
            EffectFamily::Mcp { .. } => "mcp",
        }
    }
}

/// Result of one postcondition probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectProbe {
    pub state: EffectState,
    pub evidence: EffectEvidence,
    pub external_id: Option<String>,
    pub response_hash: Option<String>,
}

impl EffectProbe {
    pub fn done(evidence: EffectEvidence) -> Self {
        Self {
            state: EffectState::Done,
            evidence,
            external_id: None,
            response_hash: None,
        }
    }

    pub fn failed(evidence: EffectEvidence) -> Self {
        Self {
            state: EffectState::Failed,
            evidence,
            external_id: None,
            response_hash: None,
        }
    }

    pub fn unknown(evidence: EffectEvidence) -> Self {
        Self {
            state: EffectState::Unknown,
            evidence,
            external_id: None,
            response_hash: None,
        }
    }

    pub fn with_external_id(mut self, value: Option<String>) -> Self {
        self.external_id = value
            .map(|value| {
                value
                    .chars()
                    .take(MAX_EXTERNAL_ID_CHARS)
                    .collect::<String>()
            })
            .filter(|value| !value.is_empty());
        self
    }

    pub fn with_response_hash(mut self, value: Option<String>) -> Self {
        self.response_hash = value.filter(|value| !value.is_empty());
        self
    }
}

/// Classify a tool call into an effect plan. Returning `None` means the call
/// is a read, the policy disabled the family, or the call is a dry run; none
/// of those creates a receipt.
///
/// `risk` is the deterministic permission risk already computed for the call.
/// `dynamic_mcp` is true for a server-advertised `mcp_<server>_<tool>` binding.
pub fn classify_tool_effect(
    name: &str,
    input: &Value,
    risk: RiskLevel,
    dynamic_mcp: bool,
    policy: &EffectPolicy,
) -> Result<Option<ToolEffectPlan>> {
    classify_tool_effect_for_host(name, input, risk, dynamic_mcp, policy, None)
}

/// Host-aware classification for remote tools. The configured execution host
/// is part of the target identity so a receipt from host A cannot replay a
/// write intended for host B.
pub fn classify_tool_effect_for_host(
    name: &str,
    input: &Value,
    risk: RiskLevel,
    dynamic_mcp: bool,
    policy: &EffectPolicy,
    remote_label: Option<&str>,
) -> Result<Option<ToolEffectPlan>> {
    if !policy.enabled || input_is_dry_run(input) {
        return Ok(None);
    }
    let remote_host = remote_label
        .map(|label| label.chars().take(200).collect::<String>())
        .unwrap_or_else(|| "remote".to_string());
    let plan = match name {
        "write_file" | "edit_file" if policy.track_workspace_writes => {
            let path = canonical_target_path(
                input
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("{name} requires a path"))?,
            )
            .ok_or_else(|| anyhow!("{name} path must be worktree-relative"))?;
            let content_hash = if name == "write_file" {
                input
                    .get("content")
                    .and_then(Value::as_str)
                    .map(|content| sha256_bytes(content.as_bytes()))
            } else {
                let old = input
                    .get("old_string")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let new = input
                    .get("new_string")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                Some(sha256_bytes(
                    format!(
                        "{}\u{0}{}",
                        sha256_bytes(old.as_bytes()),
                        sha256_bytes(new.as_bytes())
                    )
                    .as_bytes(),
                ))
            };
            Some(ToolEffectPlan {
                tool: name.to_string(),
                target: EffectTarget::new(kinds::WORKSPACE_WRITE, &path),
                request_hash: request_hash(
                    name,
                    json!({
                        "path": path,
                        "content_hash": content_hash,
                        "expected_sha256": input.get("expected_sha256"),
                        "replace_all": input.get("replace_all").and_then(Value::as_bool).unwrap_or(false),
                    }),
                )?,
                provider_key: Some("workspace".to_string()),
                family: EffectFamily::WorkspaceWrite { path },
            })
        }
        "apply_patch" if policy.track_workspace_writes => {
            let patch = input
                .get("patch")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("apply_patch requires a patch"))?;
            let paths = patch_target_paths(patch);
            let key = if paths.is_empty() {
                format!("patch-{}", short_hash(&sha256_bytes(patch.as_bytes())))
            } else if paths.len() > 8 {
                format!(
                    "{} files-{}",
                    paths.len(),
                    short_hash(&sha256_bytes(paths.join("\n").as_bytes()))
                )
            } else {
                paths.join(",")
            };
            Some(ToolEffectPlan {
                tool: name.to_string(),
                target: EffectTarget::new(kinds::WORKSPACE_PATCH, key),
                request_hash: request_hash(
                    name,
                    json!({
                        "patch_hash": sha256_bytes(patch.as_bytes()),
                        "paths": paths,
                        "create_files": input.get("create_files").and_then(Value::as_bool).unwrap_or(true),
                        "delete_files": input.get("delete_files").and_then(Value::as_bool).unwrap_or(false),
                    }),
                )?,
                provider_key: Some("workspace".to_string()),
                family: EffectFamily::WorkspacePatch,
            })
        }
        "file_manage" if policy.track_workspace_writes => {
            let action = input
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let source = input
                .get("source")
                .and_then(Value::as_str)
                .and_then(canonical_target_path);
            let destination = input
                .get("destination")
                .and_then(Value::as_str)
                .and_then(canonical_target_path);
            let key = match (&source, &destination) {
                (Some(source), Some(destination)) => format!("{action}:{source}->{destination}"),
                (Some(source), None) => format!("{action}:{source}"),
                (None, Some(destination)) => format!("{action}:{destination}"),
                (None, None) => format!("{action}:{}", short_hash(&canonical_json(input))),
            };
            Some(ToolEffectPlan {
                tool: name.to_string(),
                target: EffectTarget::new(kinds::WORKSPACE_MANAGE, key),
                request_hash: request_hash(
                    name,
                    json!({
                        "action": action,
                        "source": source,
                        "destination": destination,
                        "recursive": input.get("recursive").and_then(Value::as_bool).unwrap_or(false),
                        "overwrite": input.get("overwrite").and_then(Value::as_bool).unwrap_or(false),
                        "expected_hashes": input.get("expected_hashes"),
                    }),
                )?,
                provider_key: Some("workspace".to_string()),
                family: EffectFamily::WorkspaceManage { action },
            })
        }
        "run_project_task" => {
            let kind = input.get("kind").and_then(Value::as_str).unwrap_or("task");
            let command = input
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Some(ToolEffectPlan {
                tool: name.to_string(),
                target: EffectTarget::new(kinds::PROJECT_TASK, kind),
                request_hash: request_hash(
                    name,
                    json!({
                        "kind": kind,
                        "command_hash": if command.is_empty() { None } else { Some(sha256_bytes(command.as_bytes())) },
                        "package": input.get("package"),
                        "filter": input.get("filter"),
                        "paths": bounded_array(input.get("paths")),
                        "changed_only": input.get("changed_only").and_then(Value::as_bool).unwrap_or(false),
                        "cache": input.get("cache"),
                    }),
                )?,
                provider_key: Some("workspace".to_string()),
                family: EffectFamily::ProjectTask,
            })
        }
        "remote_write" => {
            let path = canonical_remote_path(
                input
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("remote_write requires a path"))?,
            )?;
            let content_hash = input
                .get("content")
                .and_then(Value::as_str)
                .map(|content| sha256_bytes(content.as_bytes()));
            Some(ToolEffectPlan {
                tool: name.to_string(),
                target: EffectTarget::new(kinds::REMOTE_WRITE, format!("{remote_host}:{path}")),
                request_hash: request_hash(
                    name,
                    json!({
                        "host": remote_host,
                        "path": path,
                        "content_hash": content_hash,
                        "encoding": input.get("encoding"),
                        "expected_sha256": input.get("expected_sha256"),
                        "allow_overwrite": input.get("allow_overwrite").and_then(Value::as_bool).unwrap_or(false),
                    }),
                )?,
                provider_key: Some(format!("remote:{remote_host}")),
                family: EffectFamily::RemoteWrite { path },
            })
        }
        "remote_exec" | "remote_git" | "remote_worktree" => {
            let command_hash = input
                .get("command")
                .or_else(|| input.get("args"))
                .map(|value| sha256_bytes(canonical_json(value).as_bytes()))
                .unwrap_or_else(|| sha256_bytes(canonical_json(input).as_bytes()));
            let action = input
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or("exec");
            Some(ToolEffectPlan {
                tool: name.to_string(),
                target: EffectTarget::new(
                    kinds::REMOTE_EXEC,
                    format!("{remote_host}:{action}:{}", short_hash(&command_hash)),
                ),
                request_hash: request_hash(
                    name,
                    json!({
                        "host": remote_host,
                        "action": action,
                        "command_hash": command_hash,
                        "cwd": input.get("cwd"),
                    }),
                )?,
                provider_key: Some(format!("remote:{remote_host}")),
                family: EffectFamily::RemoteExec,
            })
        }
        "local_http" | "fetch_url" => {
            let method = input
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("GET")
                .to_ascii_uppercase();
            let read_only = matches!(method.as_str(), "GET" | "HEAD");
            let tracked = if read_only {
                policy.track_read_http
            } else {
                policy.track_mutating_http
            };
            if !tracked {
                return Ok(None);
            }
            let raw = input
                .get("url")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("{name} requires a url"))?;
            let canonical = canonical_http_target(raw).ok_or_else(|| {
                anyhow!("{name} url must be an absolute http(s) URL without credentials")
            })?;
            let body_hash = input
                .get("json")
                .map(|value| sha256_bytes(canonical_json(value).as_bytes()))
                .or_else(|| {
                    input
                        .get("body")
                        .and_then(Value::as_str)
                        .map(|body| sha256_bytes(body.as_bytes()))
                });
            // The persisted target stays query-free (queries can carry
            // tokens), but the request identity must distinguish
            // `?job=a` from `?job=b`.
            let query_hash = reqwest::Url::parse(raw)
                .ok()
                .and_then(|url| url.query().map(|query| sha256_bytes(query.as_bytes())));
            Some(ToolEffectPlan {
                tool: name.to_string(),
                target: EffectTarget::new(kinds::HTTP_REQUEST, format!("{method}:{canonical}")),
                request_hash: request_hash(
                    name,
                    json!({
                        "method": method,
                        "url": canonical,
                        "query_hash": query_hash,
                        "body_hash": body_hash
                    }),
                )?,
                provider_key: Some("http".to_string()),
                family: EffectFamily::Http { method, read_only },
            })
        }
        "shell" => {
            if risk == RiskLevel::ReadOnly {
                return Ok(None);
            }
            if risk == RiskLevel::WorkspaceWrite
                && policy.shell_mode == EffectShellMode::ExternalOnly
            {
                // The shell sandbox confines these writes to the worktree; the
                // Verifier and Integrator review the resulting diff. A receipt
                // would only add an unprovable external claim.
                return Ok(None);
            }
            let command = input
                .get("command")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("shell requires a command"))?;
            let declared = DeclaredProbe::parse(input)?;
            Some(ToolEffectPlan {
                tool: name.to_string(),
                target: EffectTarget::new(
                    kinds::SHELL,
                    format!("cmd-{}", short_hash(&sha256_bytes(command.as_bytes()))),
                ),
                request_hash: request_hash(
                    name,
                    json!({
                        "command_hash": sha256_bytes(command.as_bytes()),
                        "risk": risk.as_str(),
                        "declared_probe": declared.as_ref().map(|probe| probe.label()),
                    }),
                )?,
                provider_key: Some("workspace".to_string()),
                family: EffectFamily::Shell { declared },
            })
        }
        "mcp" if policy.track_mcp_mutations => {
            let action = input
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or("list")
                .to_ascii_lowercase();
            if action != "call" {
                // Inspection (list/read/get) is read-only and creates no effect.
                return Ok(None);
            }
            let server = input
                .get("server")
                .or_else(|| input.get("server_id"))
                .and_then(Value::as_str)
                .map(|value| value.chars().take(120).collect::<String>());
            let tool = input
                .get("tool")
                .and_then(Value::as_str)
                .map(|value| value.chars().take(120).collect::<String>());
            Some(ToolEffectPlan {
                tool: name.to_string(),
                target: EffectTarget::new(
                    kinds::MCP_TOOL,
                    format!(
                        "{}:{}",
                        server.as_deref().unwrap_or("mcp"),
                        tool.as_deref().unwrap_or("call")
                    ),
                ),
                request_hash: request_hash(
                    name,
                    json!({
                        "server": server,
                        "tool": tool,
                        "arguments_hash": input.get("arguments").map(|value| sha256_bytes(canonical_json(value).as_bytes())),
                    }),
                )?,
                provider_key: Some("mcp".to_string()),
                family: EffectFamily::Mcp { server, tool },
            })
        }
        _ if dynamic_mcp && policy.track_mcp_mutations => {
            let binding = name
                .strip_prefix("mcp_")
                .unwrap_or(name)
                .chars()
                .take(200)
                .collect::<String>();
            Some(ToolEffectPlan {
                tool: name.to_string(),
                target: EffectTarget::new(kinds::MCP_TOOL, &binding),
                request_hash: request_hash(
                    name,
                    json!({"arguments_hash": sha256_bytes(canonical_json(input).as_bytes())}),
                )?,
                provider_key: Some("mcp".to_string()),
                family: EffectFamily::Mcp {
                    server: None,
                    tool: Some(binding),
                },
            })
        }
        _ => None,
    };
    Ok(plan)
}

/// Run the postcondition probe for a completed tool call. The probe never
/// executes the effect again and never invents evidence.
pub async fn probe_tool_effect(
    plan: &ToolEffectPlan,
    input: &Value,
    output: &ToolOutput,
    worktree: &Path,
) -> EffectProbe {
    let structured = output.structured.as_ref();
    let response_hash = structured.map(|value| {
        let encoded = canonical_json(value);
        sha256_bytes(encoded.as_bytes())
    });
    let failure_code = output
        .failure
        .as_ref()
        .map(|failure| failure.code.as_str().to_string());
    let base = EffectProbe {
        state: EffectState::Failed,
        evidence: EffectEvidence {
            probe: probes::TOOL_ERROR.to_string(),
            outcome: "tool_error".to_string(),
            reason: Some(reasons::TOOL_REPORTED_FAILURE.to_string()),
            tool_error_code: failure_code.clone(),
            ..EffectEvidence::default()
        },
        external_id: None,
        response_hash: response_hash.clone(),
    };
    if output.is_error {
        // A preempted/deadlined tool may have applied a partial effect. It is
        // never safe to record that as a plain failure.
        if failure_code.as_deref().is_some_and(|code| {
            matches!(
                code,
                "PREEMPTED" | "DEADLINE_EXCEEDED" | "COMMAND_TIMEOUT" | "TIMEOUT"
            )
        }) {
            let mut evidence = base.evidence;
            evidence.outcome = "unverifiable".to_string();
            evidence.reason = Some(reasons::TRANSPORT_UNKNOWN.to_string());
            evidence.note = Some(
                "the tool was interrupted before its postcondition could be observed".to_string(),
            );
            return EffectProbe {
                state: EffectState::Unknown,
                evidence,
                external_id: None,
                response_hash,
            };
        }
        return base;
    }

    match &plan.family {
        EffectFamily::WorkspaceWrite { path } => {
            probe_workspace_write(path, structured, worktree, response_hash)
        }
        EffectFamily::WorkspacePatch => probe_workspace_patch(structured, worktree, response_hash),
        EffectFamily::WorkspaceManage { action } => {
            probe_workspace_manage(action, input, structured, worktree, response_hash)
        }
        EffectFamily::ProjectTask => probe_project_task(structured, response_hash),
        EffectFamily::RemoteWrite { path } => probe_remote_write(path, structured, response_hash),
        EffectFamily::RemoteExec => EffectProbe {
            state: EffectState::Unknown,
            evidence: EffectEvidence {
                probe: probes::NO_PROBE.to_string(),
                outcome: "unverifiable".to_string(),
                reason: Some(reasons::NO_POSTCONDITION.to_string()),
                caveat: Some(
                    "remote exec has no stable local postcondition; inspect the remote host"
                        .to_string(),
                ),
                ..EffectEvidence::default()
            },
            external_id: None,
            response_hash,
        },
        EffectFamily::Http { method, read_only } => {
            probe_http(method, *read_only, structured, response_hash)
        }
        EffectFamily::Shell { declared } => probe_shell(declared.as_ref(), worktree, response_hash),
        EffectFamily::Mcp { server, tool } => EffectProbe {
            state: EffectState::Unknown,
            evidence: EffectEvidence {
                probe: probes::NO_PROBE.to_string(),
                outcome: "unverifiable".to_string(),
                reason: Some(reasons::NO_POSTCONDITION.to_string()),
                external_id: None,
                note: Some(format!(
                    "mutating MCP call {}:{} has no stable local postcondition",
                    server.as_deref().unwrap_or("mcp"),
                    tool.as_deref().unwrap_or("call")
                )),
                caveat: Some(
                    "the MCP server may have applied the call; reconcile with server-side evidence"
                        .to_string(),
                ),
                ..EffectEvidence::default()
            },
            external_id: None,
            response_hash,
        },
    }
}

fn probe_workspace_write(
    relative: &str,
    structured: Option<&Value>,
    worktree: &Path,
    response_hash: Option<String>,
) -> EffectProbe {
    let path = worktree.join(relative);
    let actual = sha256_file_bounded(&path);
    let exists = path.exists();
    let mut evidence = EffectEvidence {
        probe: probes::WORKSPACE_SHA256.to_string(),
        outcome: "observed".to_string(),
        sha256: actual.clone(),
        paths: vec![EffectPathEvidence {
            path: relative.to_string(),
            status: if exists { "modified" } else { "missing" }.to_string(),
            exists,
            sha256: actual.clone(),
        }],
        ..EffectEvidence::default()
    };
    if !exists {
        evidence.outcome = "mismatch".to_string();
        evidence.reason = Some(reasons::POSTCONDITION_MISSING.to_string());
        return EffectProbe::failed(evidence).with_response_hash(response_hash);
    }
    let expected = structured
        .and_then(|value| value.get("sha256").or_else(|| value.get("after_sha256")))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    if let (Some(expected), Some(actual)) = (expected, actual.as_deref()) {
        if expected != actual {
            evidence.outcome = "mismatch".to_string();
            evidence.reason = Some(reasons::HASH_MISMATCH.to_string());
            return EffectProbe::unknown(evidence).with_response_hash(response_hash);
        }
        evidence.note = Some("content hash matches the tool-reported after hash".to_string());
    }
    EffectProbe::done(evidence).with_response_hash(response_hash)
}

fn probe_workspace_patch(
    structured: Option<&Value>,
    worktree: &Path,
    response_hash: Option<String>,
) -> EffectProbe {
    let Some(files) = structured
        .and_then(|value| value.get("files"))
        .and_then(Value::as_array)
    else {
        return EffectProbe::unknown(EffectEvidence {
            probe: probes::WORKSPACE_PATCH.to_string(),
            outcome: "unverifiable".to_string(),
            reason: Some(reasons::PROBE_INCOMPLETE.to_string()),
            ..EffectEvidence::default()
        })
        .with_response_hash(response_hash);
    };
    let mut paths = Vec::new();
    let mut mismatch = false;
    let mut missing = false;
    for file in files.iter().take(MAX_PATCH_PROBE_FILES) {
        let Some(relative) = file.get("path").and_then(Value::as_str) else {
            continue;
        };
        let Some(relative) = canonical_target_path(relative) else {
            mismatch = true;
            continue;
        };
        let status = file
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("modified");
        let path = worktree.join(&relative);
        let exists = path.exists();
        if status == "deleted" {
            if exists {
                mismatch = true;
            }
            paths.push(EffectPathEvidence {
                path: relative,
                status: "deleted".to_string(),
                exists,
                sha256: None,
            });
            continue;
        }
        let actual = sha256_file_bounded(&path);
        if !exists {
            missing = true;
        }
        let expected = file
            .get("after_sha256")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty());
        if let (Some(expected), Some(actual)) = (expected, actual.as_deref())
            && expected != actual
        {
            mismatch = true;
        }
        paths.push(EffectPathEvidence {
            path: relative,
            status: status.to_string(),
            exists,
            sha256: actual,
        });
    }
    if paths.is_empty() {
        return EffectProbe::unknown(EffectEvidence {
            probe: probes::WORKSPACE_PATCH.to_string(),
            outcome: "unverifiable".to_string(),
            reason: Some(reasons::PROBE_INCOMPLETE.to_string()),
            ..EffectEvidence::default()
        })
        .with_response_hash(response_hash);
    }
    let mut evidence = EffectEvidence {
        probe: probes::WORKSPACE_PATCH.to_string(),
        outcome: "observed".to_string(),
        paths,
        ..EffectEvidence::default()
    };
    if mismatch || missing {
        evidence.outcome = "mismatch".to_string();
        evidence.reason = Some(if missing {
            reasons::POSTCONDITION_MISSING.to_string()
        } else {
            reasons::HASH_MISMATCH.to_string()
        });
        return EffectProbe::unknown(evidence).with_response_hash(response_hash);
    }
    if files.len() > MAX_PATCH_PROBE_FILES {
        evidence.note = Some(format!(
            "{} additional changed path(s) were not probed",
            files.len() - MAX_PATCH_PROBE_FILES
        ));
        evidence.outcome = "mismatch".to_string();
        evidence.reason = Some(reasons::PROBE_INCOMPLETE.to_string());
        return EffectProbe::unknown(evidence).with_response_hash(response_hash);
    }
    EffectProbe::done(evidence).with_response_hash(response_hash)
}

fn probe_workspace_manage(
    action: &str,
    input: &Value,
    structured: Option<&Value>,
    worktree: &Path,
    response_hash: Option<String>,
) -> EffectProbe {
    let source = input
        .get("source")
        .and_then(Value::as_str)
        .and_then(canonical_target_path);
    let destination = input
        .get("destination")
        .and_then(Value::as_str)
        .and_then(canonical_target_path);
    let mut paths = Vec::new();
    let mut ok;
    match action {
        "mkdir" => {
            let Some(destination) = destination else {
                return EffectProbe::unknown(EffectEvidence::unverifiable(
                    probes::WORKSPACE_MANAGE,
                    reasons::PROBE_INCOMPLETE,
                ))
                .with_response_hash(response_hash);
            };
            let path = worktree.join(&destination);
            let exists = path.is_dir();
            ok = exists;
            paths.push(EffectPathEvidence {
                path: destination,
                status: if exists { "created" } else { "missing" }.to_string(),
                exists,
                sha256: None,
            });
        }
        "delete" => {
            let Some(source) = source else {
                return EffectProbe::unknown(EffectEvidence::unverifiable(
                    probes::WORKSPACE_MANAGE,
                    reasons::PROBE_INCOMPLETE,
                ))
                .with_response_hash(response_hash);
            };
            let path = worktree.join(&source);
            let exists = path.exists();
            ok = !exists;
            paths.push(EffectPathEvidence {
                path: source,
                status: if exists { "unexpected" } else { "deleted" }.to_string(),
                exists,
                sha256: None,
            });
        }
        "move" | "copy" => {
            let (Some(source), Some(destination)) = (source, destination) else {
                return EffectProbe::unknown(EffectEvidence::unverifiable(
                    probes::WORKSPACE_MANAGE,
                    reasons::PROBE_INCOMPLETE,
                ))
                .with_response_hash(response_hash);
            };
            let source_path = worktree.join(&source);
            let destination_path = worktree.join(&destination);
            let destination_sha = sha256_file_bounded(&destination_path);
            let destination_exists = destination_path.exists();
            ok = destination_exists;
            if action == "move" {
                let source_exists = source_path.exists();
                if source_exists {
                    ok = false;
                }
                paths.push(EffectPathEvidence {
                    path: source,
                    status: if source_exists {
                        "unexpected"
                    } else {
                        "deleted"
                    }
                    .to_string(),
                    exists: source_exists,
                    sha256: None,
                });
            } else {
                let source_sha = sha256_file_bounded(&source_path);
                if source_sha.is_some() && source_sha != destination_sha {
                    ok = false;
                }
            }
            paths.push(EffectPathEvidence {
                path: destination,
                status: if destination_exists {
                    "created"
                } else {
                    "missing"
                }
                .to_string(),
                exists: destination_exists,
                sha256: destination_sha,
            });
        }
        _ => {
            return EffectProbe::unknown(EffectEvidence::unverifiable(
                probes::WORKSPACE_MANAGE,
                reasons::NO_POSTCONDITION,
            ))
            .with_response_hash(response_hash);
        }
    }
    let mut evidence = EffectEvidence {
        probe: probes::WORKSPACE_MANAGE.to_string(),
        outcome: "observed".to_string(),
        paths,
        ..EffectEvidence::default()
    };
    if let Some(note) = structured
        .and_then(|value| value.get("warnings"))
        .and_then(Value::as_array)
        .and_then(|warnings| warnings.first())
        .and_then(Value::as_str)
    {
        evidence.note = Some(note.chars().take(MAX_EFFECT_NOTE_CHARS).collect());
    }
    if !ok {
        evidence.outcome = "mismatch".to_string();
        evidence.reason = Some(reasons::POSTCONDITION_MISSING.to_string());
        return EffectProbe::failed(evidence).with_response_hash(response_hash);
    }
    EffectProbe::done(evidence).with_response_hash(response_hash)
}

fn probe_project_task(structured: Option<&Value>, response_hash: Option<String>) -> EffectProbe {
    let exit_code = structured
        .and_then(|value| value.get("exit_code"))
        .and_then(Value::as_i64)
        .or_else(|| {
            structured
                .and_then(|value| value.get("ok"))
                .and_then(Value::as_bool)
                .map(|ok| if ok { 0 } else { 1 })
        });
    let ok = structured
        .and_then(|value| value.get("ok"))
        .and_then(Value::as_bool)
        .unwrap_or(exit_code == Some(0));
    let mut evidence = EffectEvidence {
        probe: probes::PROJECT_TASK_RESULT.to_string(),
        outcome: if ok { "observed" } else { "mismatch" }.to_string(),
        exit_code,
        sha256: response_hash.clone(),
        caveat: Some(PROJECT_TASK_CAVEAT.to_string()),
        ..EffectEvidence::default()
    };
    if let Some(cache_hit) = structured
        .and_then(|value| value.get("cache_hit"))
        .and_then(Value::as_bool)
    {
        evidence.note = Some(format!("cache_hit={cache_hit}"));
    }
    if ok && exit_code.unwrap_or(0) == 0 {
        EffectProbe::done(evidence).with_response_hash(response_hash)
    } else {
        if evidence.reason.is_none() {
            evidence.reason = Some(reasons::POSTCONDITION_MISSING.to_string());
        }
        EffectProbe::failed(evidence).with_response_hash(response_hash)
    }
}

fn probe_remote_write(
    path: &str,
    structured: Option<&Value>,
    response_hash: Option<String>,
) -> EffectProbe {
    let sha = structured
        .and_then(|value| value.get("sha256"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    let Some(sha) = sha else {
        return EffectProbe::unknown(EffectEvidence {
            probe: probes::REMOTE_HELPER_HASH.to_string(),
            outcome: "unverifiable".to_string(),
            reason: Some(reasons::PROBE_INCOMPLETE.to_string()),
            note: Some(format!("remote path {path}")),
            caveat: Some(REMOTE_CAVEAT.to_string()),
            ..EffectEvidence::default()
        })
        .with_response_hash(response_hash);
    };
    let mut evidence = EffectEvidence {
        probe: probes::REMOTE_HELPER_HASH.to_string(),
        outcome: "observed".to_string(),
        sha256: Some(sha.to_string()),
        external_id: Some(sha.to_string()),
        note: Some(format!("remote path {path}")),
        caveat: Some(REMOTE_CAVEAT.to_string()),
        ..EffectEvidence::default()
    };
    if let Some(replayed) = structured
        .and_then(|value| value.get("replayed"))
        .and_then(Value::as_bool)
    {
        evidence.replayed = replayed;
        if replayed {
            evidence.note = Some(format!(
                "remote path {path}; the helper reported an idempotent replay"
            ));
        }
    }
    EffectProbe::done(evidence)
        .with_external_id(Some(sha.to_string()))
        .with_response_hash(response_hash)
}

fn probe_http(
    method: &str,
    read_only: bool,
    structured: Option<&Value>,
    response_hash: Option<String>,
) -> EffectProbe {
    let status = structured
        .and_then(|value| value.get("status"))
        .and_then(Value::as_u64)
        .and_then(|value| u16::try_from(value).ok());
    let body_hash = structured
        .and_then(|value| value.get("sha256"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| response_hash.clone());
    let mut evidence = EffectEvidence {
        probe: probes::HTTP_STATUS.to_string(),
        outcome: "observed".to_string(),
        http_status: status,
        sha256: body_hash,
        caveat: Some(HTTP_CAVEAT.to_string()),
        note: Some(format!("{method} observation")),
        ..EffectEvidence::default()
    };
    if read_only {
        evidence.note = Some(format!("{method} read observation"));
    }
    match status {
        Some(status) if (200..300).contains(&status) => {
            EffectProbe::done(evidence).with_response_hash(response_hash)
        }
        Some(status) if (300..400).contains(&status) => {
            if read_only {
                EffectProbe::done(evidence).with_response_hash(response_hash)
            } else {
                // A 3xx without following the redirect does not prove the
                // mutation reached its target.
                evidence.outcome = "unverifiable".to_string();
                evidence.reason = Some("redirect_not_followed".to_string());
                evidence.note = Some(format!(
                    "HTTP {status} redirect was not followed; the effect is unknown"
                ));
                EffectProbe::unknown(evidence).with_response_hash(response_hash)
            }
        }
        Some(status) if (400..500).contains(&status) => {
            evidence.outcome = "mismatch".to_string();
            evidence.reason = Some("client_error".to_string());
            EffectProbe::failed(evidence).with_response_hash(response_hash)
        }
        Some(status) => {
            evidence.outcome = "unverifiable".to_string();
            evidence.reason = Some(reasons::SERVER_ERROR.to_string());
            evidence.note = Some(format!(
                "HTTP {status} does not prove whether the effect was applied"
            ));
            EffectProbe::unknown(evidence).with_response_hash(response_hash)
        }
        None => {
            evidence.outcome = "unverifiable".to_string();
            evidence.reason = Some(reasons::TRANSPORT_UNKNOWN.to_string());
            EffectProbe::unknown(evidence).with_response_hash(response_hash)
        }
    }
}

fn probe_shell(
    declared: Option<&DeclaredProbe>,
    worktree: &Path,
    response_hash: Option<String>,
) -> EffectProbe {
    let Some(declared) = declared else {
        return EffectProbe::unknown(EffectEvidence {
            probe: probes::NO_PROBE.to_string(),
            outcome: "unverifiable".to_string(),
            reason: Some(reasons::NO_POSTCONDITION.to_string()),
            note: Some(
                "raw shell has no postcondition; declare effect_probe to settle as done"
                    .to_string(),
            ),
            ..EffectEvidence::default()
        })
        .with_response_hash(response_hash);
    };
    match declared {
        DeclaredProbe::FileHash { path, sha256 } => {
            let Some(relative) = canonical_target_path(path) else {
                return EffectProbe::unknown(EffectEvidence::unverifiable(
                    declared.label(),
                    "declared path is not worktree-relative",
                ))
                .with_response_hash(response_hash);
            };
            let actual = sha256_file_bounded(&worktree.join(&relative));
            let exists = worktree.join(&relative).exists();
            let mut evidence = EffectEvidence {
                probe: declared.label().to_string(),
                outcome: "observed".to_string(),
                sha256: actual.clone(),
                paths: vec![EffectPathEvidence {
                    path: relative,
                    status: if exists { "modified" } else { "missing" }.to_string(),
                    exists,
                    sha256: actual.clone(),
                }],
                ..EffectEvidence::default()
            };
            if exists && actual.as_deref() == Some(sha256.as_str()) {
                EffectProbe::done(evidence).with_response_hash(response_hash)
            } else {
                evidence.outcome = "mismatch".to_string();
                evidence.reason = Some(if exists {
                    reasons::HASH_MISMATCH.to_string()
                } else {
                    reasons::POSTCONDITION_MISSING.to_string()
                });
                EffectProbe::failed(evidence).with_response_hash(response_hash)
            }
        }
        DeclaredProbe::FileAbsent { path } => {
            let Some(relative) = canonical_target_path(path) else {
                return EffectProbe::unknown(EffectEvidence::unverifiable(
                    declared.label(),
                    "declared path is not worktree-relative",
                ))
                .with_response_hash(response_hash);
            };
            let exists = worktree.join(&relative).exists();
            let mut evidence = EffectEvidence {
                probe: declared.label().to_string(),
                outcome: "observed".to_string(),
                paths: vec![EffectPathEvidence {
                    path: relative,
                    status: if exists { "unexpected" } else { "deleted" }.to_string(),
                    exists,
                    sha256: None,
                }],
                ..EffectEvidence::default()
            };
            if exists {
                evidence.outcome = "mismatch".to_string();
                evidence.reason = Some(reasons::POSTCONDITION_MISSING.to_string());
                EffectProbe::failed(evidence).with_response_hash(response_hash)
            } else {
                EffectProbe::done(evidence).with_response_hash(response_hash)
            }
        }
        DeclaredProbe::GitHead { commit } => match git_head(worktree) {
            Some(actual) if actual == *commit || actual.starts_with(commit) => {
                EffectProbe::done(EffectEvidence {
                    probe: declared.label().to_string(),
                    outcome: "observed".to_string(),
                    commit: Some(actual),
                    ..EffectEvidence::default()
                })
                .with_response_hash(response_hash)
            }
            Some(actual) => EffectProbe::failed(EffectEvidence {
                probe: declared.label().to_string(),
                outcome: "mismatch".to_string(),
                reason: Some(reasons::HASH_MISMATCH.to_string()),
                commit: Some(actual),
                ..EffectEvidence::default()
            })
            .with_response_hash(response_hash),
            None => EffectProbe::unknown(EffectEvidence::unverifiable(
                declared.label(),
                "git rev-parse failed",
            ))
            .with_response_hash(response_hash),
        },
        DeclaredProbe::GitBranch { branch } => match git_branch(worktree) {
            Some(actual) if actual == *branch => EffectProbe::done(EffectEvidence {
                probe: declared.label().to_string(),
                outcome: "observed".to_string(),
                branch: Some(actual),
                ..EffectEvidence::default()
            })
            .with_response_hash(response_hash),
            Some(actual) => EffectProbe::failed(EffectEvidence {
                probe: declared.label().to_string(),
                outcome: "mismatch".to_string(),
                reason: Some(reasons::HASH_MISMATCH.to_string()),
                branch: Some(actual),
                ..EffectEvidence::default()
            })
            .with_response_hash(response_hash),
            None => EffectProbe::unknown(EffectEvidence::unverifiable(
                declared.label(),
                "git branch --show-current failed",
            ))
            .with_response_hash(response_hash),
        },
    }
}

/// Project one session's events into effect receipts. `None` means at least
/// one effect payload was malformed, so replay must report drift rather than
/// silently dropping rows.
pub fn receipts_from_events(events: &[vp_protocol::types::Event]) -> Option<Vec<EffectReceipt>> {
    let mut rows: BTreeMap<String, EffectReceipt> = BTreeMap::new();
    for event in events {
        if !matches!(
            event.kind.as_str(),
            EFFECT_INTENT_EVENT
                | EFFECT_SETTLED_EVENT
                | EFFECT_FAILED_EVENT
                | EFFECT_UNKNOWN_EVENT
                | EFFECT_RECONCILED_EVENT
        ) {
            continue;
        }
        let receipt = event.payload.get("receipt")?;
        let receipt = serde_json::from_value::<EffectReceipt>(receipt.clone()).ok()?;
        if receipt.session_id != event.session_id {
            return None;
        }
        rows.insert(receipt.id.clone(), receipt);
    }
    let mut values = rows.into_values().collect::<Vec<_>>();
    values.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    Some(values)
}

/// Compare projected and replayed receipts by durable identity and state.
pub fn receipts_match(projected: &[EffectReceipt], expected: &[EffectReceipt]) -> bool {
    if projected.len() != expected.len() {
        return false;
    }
    projected.iter().all(|row| {
        expected
            .iter()
            .find(|candidate| candidate.id == row.id)
            .is_some_and(|candidate| {
                candidate.state == row.state
                    && candidate.attempt == row.attempt
                    && candidate.target == row.target
                    && candidate.request_hash == row.request_hash
                    && candidate.evidence == row.evidence
                    && candidate.external_id == row.external_id
                    && candidate.response_hash == row.response_hash
            })
    })
}

/// Count receipts and mark the rows that block merge under `policy`.
pub fn counts(receipts: &[EffectReceipt], policy: &EffectPolicy) -> EffectCounts {
    let mut counts = EffectCounts {
        total: receipts.len() as u64,
        ..EffectCounts::default()
    };
    for receipt in receipts {
        match receipt.state {
            EffectState::Intent => counts.intent += 1,
            EffectState::Done => counts.done += 1,
            EffectState::Failed => counts.failed += 1,
            EffectState::Unknown => counts.unknown += 1,
        }
    }
    counts.blocking = if policy.enabled && policy.unknown_blocks_merge {
        counts.intent + counts.unknown
    } else {
        0
    };
    counts
}

/// Rows that block integration under `policy`. `done` and `failed` do not
/// block (the existing Verifier/Integrator gates still apply).
pub fn blocking_receipts<'a>(
    receipts: &'a [EffectReceipt],
    policy: &EffectPolicy,
) -> Vec<&'a EffectReceipt> {
    if !policy.enabled || !policy.unknown_blocks_merge {
        return Vec::new();
    }
    receipts
        .iter()
        .filter(|receipt| receipt.state.blocks_merge())
        .collect()
}

/// Typed integration block. Callers can downcast this to distinguish an
/// unsettled effect from an ordinary merge failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectBlocked {
    pub session_id: String,
    pub task_id: Option<String>,
    pub receipt_ids: Vec<String>,
    pub message: String,
}

impl std::fmt::Display for EffectBlocked {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for EffectBlocked {}

/// One bounded, secret-free block message shared by the merge queue, landing,
/// and RPC surfaces. It never includes command text or file content.
pub fn block_message(receipts: &[&EffectReceipt]) -> String {
    let intent = receipts
        .iter()
        .filter(|receipt| receipt.state == EffectState::Intent)
        .count();
    let unknown = receipts
        .iter()
        .filter(|receipt| receipt.state == EffectState::Unknown)
        .count();
    let samples = receipts
        .iter()
        .take(3)
        .map(|receipt| receipt.target.label())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "EFFECT_UNKNOWN: {intent} intent and {unknown} unknown effect receipt(s) block integration ({samples}); reconcile them explicitly before merging"
    )
}

/// Build the bounded evidence for an operator reconcile. A `done` decision
/// requires at least one hash, commit, external id, HTTP status, or a note of
/// at least eight characters; a bare "looks fine" cannot mark an effect done.
pub fn reconcile_evidence(
    decision: EffectState,
    input: &vp_protocol::types::EffectReconcileEvidence,
) -> Result<EffectEvidence> {
    if !matches!(decision, EffectState::Done | EffectState::Failed) {
        return Err(anyhow!("effect.reconcile decision must be done or failed"));
    }
    let evidence = EffectEvidence {
        probe: input
            .probe
            .as_deref()
            .map(str::trim)
            .filter(|probe| !probe.is_empty())
            .map(|probe| probe.chars().take(80).collect())
            .unwrap_or_else(|| probes::OPERATOR_RECONCILE.to_string()),
        outcome: "reconciled".to_string(),
        reason: Some(reasons::OPERATOR_RECONCILED.to_string()),
        sha256: sanitize_optional_hash(input.sha256.as_deref()),
        commit: sanitize_optional_hash(input.commit.as_deref()),
        external_id: sanitize_external_id(input.external_id.as_deref()),
        http_status: input.http_status,
        note: input
            .note
            .as_deref()
            .map(str::trim)
            .filter(|note| !note.is_empty())
            .map(|note| note.chars().take(MAX_EFFECT_NOTE_CHARS).collect()),
        ..EffectEvidence::default()
    };
    if decision == EffectState::Done && !evidence.has_resolution_evidence() {
        return Err(anyhow!(
            "marking an effect done requires bounded evidence: sha256, commit, external_id, http_status, or a note of at least 8 characters"
        ));
    }
    Ok(evidence)
}

/// Validate a stored receipt before it is inserted. This is the final guard
/// against a malformed caller or a corrupt bundle row.
pub fn validate_receipt(receipt: &EffectReceipt) -> Result<()> {
    if receipt.id.trim().is_empty() {
        return Err(anyhow!("effect receipt id is required"));
    }
    if receipt.session_id.trim().is_empty() {
        return Err(anyhow!("effect receipt session_id is required"));
    }
    if receipt.tool.trim().is_empty() {
        return Err(anyhow!("effect receipt tool is required"));
    }
    if receipt.target.kind.trim().is_empty() {
        return Err(anyhow!("effect receipt target kind is required"));
    }
    if receipt.target.canonical().chars().count() > MAX_TARGET_KEY_CHARS * 2 {
        return Err(anyhow!("effect receipt target is too long"));
    }
    if receipt.request_hash.trim().is_empty() {
        return Err(anyhow!("effect receipt request_hash is required"));
    }
    if let Some(external) = &receipt.external_id
        && external.chars().count() > MAX_EXTERNAL_ID_CHARS
    {
        return Err(anyhow!("effect receipt external_id is too long"));
    }
    if receipt.evidence.paths.len() > MAX_EFFECT_PATHS {
        return Err(anyhow!(
            "effect receipt has more than {MAX_EFFECT_PATHS} path evidence rows"
        ));
    }
    Ok(())
}

/// Human-readable summary for CLI/UI. It never includes file contents.
pub fn receipt_summary(receipt: &EffectReceipt) -> String {
    let mut summary = format!(
        "{} {} attempt {} {}",
        receipt.id,
        receipt.state.as_str(),
        receipt.attempt,
        receipt.target.label()
    );
    if let Some(hash) = receipt
        .evidence
        .sha256
        .as_deref()
        .or(receipt.external_id.as_deref())
        .or(receipt.evidence.commit.as_deref())
    {
        summary.push_str(&format!(" @ {}", short_hash(hash)));
    }
    if let Some(reason) = receipt.evidence.reason.as_deref() {
        summary.push_str(&format!(" ({reason})"));
    }
    summary
}

fn input_is_dry_run(input: &Value) -> bool {
    input
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Public canonical request hash for runtime-owned effects (Git push, forge
/// PR, landing, remote integration). Secrets must be hashed before they reach
/// `fields`.
pub fn canonical_request_hash(tool: &str, fields: Value) -> Result<String> {
    request_hash(tool, fields)
}

/// Build an intent-shaped receipt for a runtime action. The caller writes it
/// with `begin_effect` before the action and settles it afterward.
pub fn runtime_receipt(
    session_id: &str,
    task_id: Option<&str>,
    tool: &str,
    target: EffectTarget,
    request_hash: String,
    provider_key: Option<String>,
) -> EffectReceipt {
    EffectReceipt {
        id: uuid::Uuid::now_v7().to_string(),
        session_id: session_id.to_string(),
        task_id: task_id.map(str::to_string),
        attempt: 1,
        tool: tool.to_string(),
        target,
        request_hash,
        state: EffectState::Intent,
        provider_key,
        external_id: None,
        response_hash: None,
        evidence: EffectEvidence::declared("runtime"),
        created_at: vp_protocol::now_rfc3339(),
        resolved_at: None,
    }
}

fn request_hash(tool: &str, fields: Value) -> Result<String> {
    let encoded = canonical_json(&json!({"tool": tool, "fields": fields}));
    if encoded.len() > 64 * 1024 {
        return Err(anyhow!("effect request identity is too large"));
    }
    Ok(sha256_bytes(encoded.as_bytes()))
}

/// Scope an effect identity to one agent run.
///
/// Replaying a completed effect across agents is wrong: a Verifier issuing the
/// same `run_project_task` as a Worker must execute it in its own detached
/// clone, not receive the Worker's stale output. The run-journal fingerprint
/// guard still handles duplicate writes within the same agent.
pub fn scope_request_hash(base: &str, agent_id: &str, worktree_head: &str) -> String {
    // The worktree head is part of the context: after a reset or a new
    // commit, the same logical write is a new effect and must execute again.
    sha256_bytes(format!("{base}\u{1f}{agent_id}\u{1f}{worktree_head}").as_bytes())
}

fn bounded_string(value: Option<&Value>, max_chars: usize, field: &str) -> Result<String> {
    let value = value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("effect_probe.{field} is required"))?;
    if value.chars().count() > max_chars {
        return Err(anyhow!("effect_probe.{field} is too long"));
    }
    Ok(value.to_string())
}

fn validate_sha256(value: &str) -> Result<()> {
    if value.len() != 64 || !value.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(anyhow!(
            "effect_probe.sha256 must be a 64-character hex digest"
        ));
    }
    Ok(())
}

fn sanitize_optional_hash(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(MAX_HASH_CHARS).collect())
}

fn sanitize_external_id(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(MAX_EXTERNAL_ID_CHARS).collect())
}

fn bounded_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .take(64)
                .map(|item| item.chars().take(MAX_EFFECT_PATH_CHARS).collect())
                .collect()
        })
        .unwrap_or_default()
}

/// Convert an absolute or relative tool path to a bounded worktree-relative
/// key. Absolute paths are rejected: the caller must not persist host paths.
pub fn canonical_target_path(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > MAX_EFFECT_PATH_CHARS {
        return None;
    }
    let path = Path::new(value);
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                let part = part.to_string_lossy();
                if part == ".git" {
                    return None;
                }
                parts.push(part.to_string());
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if parts.is_empty() {
        return Some(".".to_string());
    }
    Some(parts.join("/"))
}

/// Remote paths are absolute paths inside the configured remote root. Keep the
/// shape but reject traversal segments and control characters.
pub fn canonical_remote_path(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty()
        || value.chars().count() > MAX_EFFECT_PATH_CHARS
        || value.chars().any(|ch| ch.is_control())
    {
        return Err(anyhow!(
            "remote path is empty, too long, or contains control characters"
        ));
    }
    let normalized = value.replace('\\', "/");
    if normalized
        .split('/')
        .any(|segment| segment == ".." || segment == ".git")
    {
        return Err(anyhow!("remote path must not contain '..' or '.git'"));
    }
    Ok(normalized)
}

/// Canonical HTTP target without credentials, query, or fragment. The query is
/// dropped because it can carry tokens; the request hash is the right place
/// for a body identity, not the persisted target.
pub fn canonical_http_target(value: &str) -> Option<String> {
    let mut url = reqwest::Url::parse(value.trim()).ok()?;
    let scheme = url.scheme().to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    let port = url.port();
    url.set_username("").ok()?;
    url.set_password(None).ok()?;
    url.set_query(None);
    url.set_fragment(None);
    let mut target = host;
    if let Some(port) = port {
        target.push_str(&format!(":{port}"));
    }
    target.push_str(url.path());
    if target.chars().count() > MAX_TARGET_KEY_CHARS {
        let prefix = target
            .chars()
            .take(MAX_TARGET_KEY_CHARS)
            .collect::<String>();
        return Some(format!(
            "{prefix}/{}",
            short_hash(&sha256_bytes(value.as_bytes()))
        ));
    }
    Some(target)
}

fn patch_target_paths(patch: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in patch.lines() {
        let candidate = line
            .strip_prefix("+++ ")
            .or_else(|| line.strip_prefix("--- "));
        let Some(candidate) = candidate else {
            continue;
        };
        let candidate = candidate
            .trim()
            .split('\t')
            .next()
            .unwrap_or_default()
            .trim();
        if candidate == "/dev/null" {
            continue;
        }
        let candidate = candidate
            .strip_prefix("b/")
            .or_else(|| candidate.strip_prefix("a/"))
            .unwrap_or(candidate);
        if let Some(path) = canonical_target_path(candidate)
            && !paths.contains(&path)
        {
            paths.push(path);
        }
        if paths.len() >= 64 {
            break;
        }
    }
    paths
}

fn sha256_file_bounded(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_PROBED_FILE_BYTES {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut reader = std::io::BufReader::new(file);
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    Some(
        digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
    )
}

fn git_head(worktree: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn git_branch(worktree: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["branch", "--show-current"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!value.is_empty()).then_some(value)
}

pub fn short_hash(value: &str) -> String {
    value.chars().take(12).collect()
}

/// SHA-256 over raw bytes. `approvals::sha256_hex` is string-only; this helper
/// hashes file content and canonical JSON bytes without a lossy conversion.
pub fn sha256_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Hash a bounded canonical tool result for `response_hash`. Oversized results
/// are hashed by their content bytes rather than expanded in memory.
pub fn response_digest(output: &ToolOutput) -> Option<String> {
    if output.content.len() > MAX_RESPONSE_HASH_BYTES {
        let limit = MAX_RESPONSE_HASH_BYTES;
        let boundary = (0..=limit)
            .rev()
            .find(|index| output.content.is_char_boundary(*index))
            .unwrap_or(0);
        return Some(sha256_bytes(&output.content.as_bytes()[..boundary]));
    }
    Some(sha256_bytes(output.content.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn policy() -> EffectPolicy {
        EffectPolicy::default()
    }

    fn ok_output(structured: Value) -> ToolOutput {
        ToolOutput::ok_structured("ok", structured)
    }

    fn error_output(code: crate::tooling::errors::ToolErrorCode) -> ToolOutput {
        ToolOutput::error_coded("test", code, "failed")
    }

    #[test]
    fn classifies_workspace_write_with_canonical_target_and_stable_hash() {
        let input = json!({"path": "./src/lib.rs", "content": "hello"});
        let plan = classify_tool_effect(
            "write_file",
            &input,
            RiskLevel::WorkspaceWrite,
            false,
            &policy(),
        )
        .unwrap()
        .expect("plan");
        assert_eq!(plan.target.kind, kinds::WORKSPACE_WRITE);
        assert_eq!(plan.target.key, "src/lib.rs");
        assert_eq!(plan.provider_key.as_deref(), Some("workspace"));
        let again = classify_tool_effect(
            "write_file",
            &input,
            RiskLevel::WorkspaceWrite,
            false,
            &policy(),
        )
        .unwrap()
        .expect("plan");
        assert_eq!(plan.request_hash, again.request_hash);
        let changed = classify_tool_effect(
            "write_file",
            &json!({"path": "src/lib.rs", "content": "hello!"}),
            RiskLevel::WorkspaceWrite,
            false,
            &policy(),
        )
        .unwrap()
        .expect("plan");
        assert_ne!(plan.request_hash, changed.request_hash);
    }

    #[test]
    fn rejects_absolute_and_parent_paths_in_target() {
        assert!(canonical_target_path("/etc/passwd").is_none());
        assert!(canonical_target_path("../outside").is_none());
        assert!(canonical_target_path("src/.git/config").is_none());
        assert_eq!(
            canonical_target_path("./src/lib.rs").as_deref(),
            Some("src/lib.rs")
        );
    }

    #[test]
    fn dry_run_and_disabled_policy_create_no_effect() {
        assert!(
            classify_tool_effect(
                "write_file",
                &json!({"path": "a", "content": "x", "dry_run": true}),
                RiskLevel::WorkspaceWrite,
                false,
                &policy()
            )
            .unwrap()
            .is_none()
        );
        let mut disabled = policy();
        disabled.enabled = false;
        assert!(
            classify_tool_effect(
                "write_file",
                &json!({"path": "a", "content": "x"}),
                RiskLevel::WorkspaceWrite,
                false,
                &disabled
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn shell_policy_external_only_skips_workspace_confined_shell() {
        let input = json!({"command": "cargo test"});
        let plan =
            classify_tool_effect("shell", &input, RiskLevel::WorkspaceWrite, false, &policy())
                .unwrap();
        assert!(plan.is_none());
        let plan = classify_tool_effect("shell", &input, RiskLevel::External, false, &policy())
            .unwrap()
            .expect("external shell plan");
        assert!(matches!(
            plan.family,
            EffectFamily::Shell { declared: None }
        ));
        assert_eq!(plan.target.kind, kinds::SHELL);
        assert!(!plan.target.key.contains("cargo test"));
        let mut all = policy();
        all.shell_mode = EffectShellMode::All;
        let plan =
            classify_tool_effect("shell", &input, RiskLevel::WorkspaceWrite, false, &all).unwrap();
        assert!(plan.is_some());
    }

    #[test]
    fn read_only_shell_creates_no_effect() {
        let plan = classify_tool_effect(
            "shell",
            &json!({"command": "ls"}),
            RiskLevel::ReadOnly,
            false,
            &policy(),
        )
        .unwrap();
        assert!(plan.is_none());
    }

    #[test]
    fn http_target_drops_credentials_query_and_fragment() {
        let target = canonical_http_target("https://user:secret@example.com:8443/a?token=x#frag")
            .expect("target");
        assert_eq!(target, "example.com:8443/a");
        assert!(!target.contains("secret"));
        assert!(canonical_http_target("file:///etc/passwd").is_none());
    }

    #[test]
    fn mutating_local_http_is_tracked_but_get_is_not_by_default() {
        let post = classify_tool_effect(
            "local_http",
            &json!({"method": "POST", "url": "http://127.0.0.1:8080/x", "body": "a"}),
            RiskLevel::WorkspaceWrite,
            false,
            &policy(),
        )
        .unwrap()
        .expect("post plan");
        assert!(matches!(
            post.family,
            EffectFamily::Http {
                ref method,
                read_only: false
            } if method == "POST"
        ));
        let get = classify_tool_effect(
            "local_http",
            &json!({"method": "GET", "url": "http://127.0.0.1:8080/x"}),
            RiskLevel::ReadOnly,
            false,
            &policy(),
        )
        .unwrap();
        assert!(get.is_none());
        let mut tracking = policy();
        tracking.track_read_http = true;
        assert!(
            classify_tool_effect(
                "local_http",
                &json!({"method": "GET", "url": "http://127.0.0.1:8080/x"}),
                RiskLevel::ReadOnly,
                false,
                &tracking
            )
            .unwrap()
            .is_some()
        );
    }

    #[test]
    fn remote_effect_identity_includes_the_configured_host() {
        let input = json!({"path": "out/app.txt", "content": "deploy"});
        let first = classify_tool_effect_for_host(
            "remote_write",
            &input,
            RiskLevel::External,
            false,
            &policy(),
            Some("ssh://build-a.example.com"),
        )
        .unwrap()
        .expect("plan a");
        let second = classify_tool_effect_for_host(
            "remote_write",
            &input,
            RiskLevel::External,
            false,
            &policy(),
            Some("ssh://build-b.example.com"),
        )
        .unwrap()
        .expect("plan b");
        assert_ne!(first.target, second.target);
        assert_ne!(first.request_hash, second.request_hash);
        assert!(first.target.key.contains("build-a.example.com"));
    }

    #[test]
    fn dynamic_mcp_is_unknown_without_a_probe() {
        let plan = classify_tool_effect(
            "mcp_docs_create_issue",
            &json!({"title": "x"}),
            RiskLevel::External,
            true,
            &policy(),
        )
        .unwrap()
        .expect("mcp plan");
        assert!(matches!(plan.family, EffectFamily::Mcp { .. }));
    }

    #[tokio::test]
    async fn workspace_write_probe_matches_hash_and_detects_mismatch() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
        let hash = sha256_bytes(b"hello\n");
        let plan = classify_tool_effect(
            "write_file",
            &json!({"path": "a.txt", "content": "hello\n"}),
            RiskLevel::WorkspaceWrite,
            false,
            &policy(),
        )
        .unwrap()
        .unwrap();
        let probe = probe_tool_effect(
            &plan,
            &json!({"path": "a.txt"}),
            &ok_output(json!({"sha256": hash})),
            dir.path(),
        )
        .await;
        assert_eq!(probe.state, EffectState::Done);
        assert_eq!(probe.evidence.probe, probes::WORKSPACE_SHA256);

        let probe = probe_tool_effect(
            &plan,
            &json!({"path": "a.txt"}),
            &ok_output(json!({"sha256": "0".repeat(64)})),
            dir.path(),
        )
        .await;
        assert_eq!(probe.state, EffectState::Unknown);
        assert_eq!(
            probe.evidence.reason.as_deref(),
            Some(reasons::HASH_MISMATCH)
        );
    }

    #[tokio::test]
    async fn workspace_write_probe_reports_missing_postcondition_as_failed() {
        let dir = TempDir::new().unwrap();
        let plan = classify_tool_effect(
            "write_file",
            &json!({"path": "missing.txt", "content": "x"}),
            RiskLevel::WorkspaceWrite,
            false,
            &policy(),
        )
        .unwrap()
        .unwrap();
        let probe = probe_tool_effect(
            &plan,
            &json!({}),
            &ok_output(json!({"sha256": sha256_bytes(b"x")})),
            dir.path(),
        )
        .await;
        assert_eq!(probe.state, EffectState::Failed);
        assert_eq!(
            probe.evidence.reason.as_deref(),
            Some(reasons::POSTCONDITION_MISSING)
        );
    }

    #[tokio::test]
    async fn apply_patch_probe_checks_every_changed_path() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a"), "one\n").unwrap();
        let a_hash = sha256_bytes(b"one\n");
        let output = ok_output(json!({
            "files": [
                {"path": "a", "status": "modified", "after_sha256": a_hash},
                {"path": "b", "status": "deleted"}
            ]
        }));
        let plan = ToolEffectPlan {
            tool: "apply_patch".to_string(),
            target: EffectTarget::new(kinds::WORKSPACE_PATCH, "a,b"),
            request_hash: "r".to_string(),
            provider_key: None,
            family: EffectFamily::WorkspacePatch,
        };
        let probe = probe_tool_effect(&plan, &json!({}), &output, dir.path()).await;
        assert_eq!(probe.state, EffectState::Done);
        assert_eq!(probe.evidence.paths.len(), 2);

        std::fs::write(dir.path().join("b"), "still here").unwrap();
        let probe = probe_tool_effect(&plan, &json!({}), &output, dir.path()).await;
        assert_eq!(probe.state, EffectState::Unknown);
        assert_eq!(
            probe.evidence.reason.as_deref(),
            Some(reasons::HASH_MISMATCH)
        );
    }

    #[tokio::test]
    async fn shell_probe_defaults_unknown_and_honors_declared_file_hash() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("out.txt"), "done\n").unwrap();
        let hash = sha256_bytes(b"done\n");
        let plan = classify_tool_effect(
            "shell",
            &json!({"command": "make deploy"}),
            RiskLevel::External,
            false,
            &policy(),
        )
        .unwrap()
        .unwrap();
        let probe = probe_tool_effect(&plan, &json!({}), &ok_output(json!({})), dir.path()).await;
        assert_eq!(probe.state, EffectState::Unknown);
        assert_eq!(probe.evidence.probe, probes::NO_PROBE);

        let declared = json!({
            "command": "make deploy",
            "effect_probe": {"kind": "file_hash", "path": "out.txt", "sha256": hash}
        });
        let plan = classify_tool_effect("shell", &declared, RiskLevel::External, false, &policy())
            .unwrap()
            .unwrap();
        let probe = probe_tool_effect(&plan, &declared, &ok_output(json!({})), dir.path()).await;
        assert_eq!(probe.state, EffectState::Done);
        assert_eq!(probe.evidence.probe, probes::DECLARED_FILE_HASH);
    }

    #[tokio::test]
    async fn http_probe_labels_observation_and_unknown_server_error() {
        let plan = classify_tool_effect(
            "local_http",
            &json!({"method": "POST", "url": "http://127.0.0.1:1/x"}),
            RiskLevel::WorkspaceWrite,
            false,
            &policy(),
        )
        .unwrap()
        .unwrap();
        let done = probe_tool_effect(
            &plan,
            &json!({}),
            &ok_output(json!({"status": 201, "sha256": "a"})),
            Path::new("/"),
        )
        .await;
        assert_eq!(done.state, EffectState::Done);
        assert_eq!(done.evidence.http_status, Some(201));
        assert!(
            done.evidence
                .caveat
                .as_deref()
                .unwrap()
                .contains("does not prove")
        );

        let unknown = probe_tool_effect(
            &plan,
            &json!({}),
            &ok_output(json!({"status": 503})),
            Path::new("/"),
        )
        .await;
        assert_eq!(unknown.state, EffectState::Unknown);

        let failed = probe_tool_effect(
            &plan,
            &json!({}),
            &ok_output(json!({"status": 422})),
            Path::new("/"),
        )
        .await;
        assert_eq!(failed.state, EffectState::Failed);
    }

    #[tokio::test]
    async fn tool_error_settles_failed_without_fabricating_success() {
        let dir = TempDir::new().unwrap();
        let plan = classify_tool_effect(
            "write_file",
            &json!({"path": "a", "content": "x"}),
            RiskLevel::WorkspaceWrite,
            false,
            &policy(),
        )
        .unwrap()
        .unwrap();
        let probe = probe_tool_effect(
            &plan,
            &json!({}),
            &error_output(crate::tooling::errors::ToolErrorCode::WRITE_FAILED),
            dir.path(),
        )
        .await;
        assert_eq!(probe.state, EffectState::Failed);
        assert_eq!(probe.evidence.probe, probes::TOOL_ERROR);
        assert_eq!(
            probe.evidence.tool_error_code.as_deref(),
            Some("WRITE_FAILED")
        );
    }

    #[test]
    fn reconcile_requires_bounded_evidence_for_done() {
        let empty = vp_protocol::types::EffectReconcileEvidence::default();
        assert!(reconcile_evidence(EffectState::Done, &empty).is_err());
        let weak = vp_protocol::types::EffectReconcileEvidence {
            note: Some("ok".to_string()),
            ..Default::default()
        };
        assert!(reconcile_evidence(EffectState::Done, &weak).is_err());
        let strong = vp_protocol::types::EffectReconcileEvidence {
            sha256: Some("a".repeat(64)),
            ..Default::default()
        };
        let evidence = reconcile_evidence(EffectState::Done, &strong).unwrap();
        assert!(evidence.has_resolution_evidence());
        assert_eq!(evidence.outcome, "reconciled");
        assert!(reconcile_evidence(EffectState::Intent, &strong).is_err());
        let failed = vp_protocol::types::EffectReconcileEvidence::default();
        assert!(reconcile_evidence(EffectState::Failed, &failed).is_ok());
    }

    #[test]
    fn counts_and_blockers_follow_policy() {
        let receipts = vec![
            EffectReceipt {
                id: "1".into(),
                state: EffectState::Done,
                ..Default::default()
            },
            EffectReceipt {
                id: "2".into(),
                state: EffectState::Unknown,
                ..Default::default()
            },
            EffectReceipt {
                id: "3".into(),
                state: EffectState::Intent,
                ..Default::default()
            },
            EffectReceipt {
                id: "4".into(),
                state: EffectState::Failed,
                ..Default::default()
            },
        ];
        let summary = counts(&receipts, &policy());
        assert_eq!(summary.total, 4);
        assert_eq!(summary.blocking, 2);
        let blockers = blocking_receipts(&receipts, &policy());
        assert_eq!(blockers.len(), 2);
        assert!(block_message(&blockers).starts_with("EFFECT_UNKNOWN: 1 intent and 1 unknown"));

        let mut observing = policy();
        observing.unknown_blocks_merge = false;
        assert_eq!(counts(&receipts, &observing).blocking, 0);
        assert!(blocking_receipts(&receipts, &observing).is_empty());
    }

    #[test]
    fn events_project_receipts_and_detect_drift() {
        let receipt = EffectReceipt {
            id: "e1".into(),
            session_id: "s1".into(),
            tool: "write_file".into(),
            target: EffectTarget::new(kinds::WORKSPACE_WRITE, "a"),
            request_hash: "r".into(),
            state: EffectState::Done,
            evidence: EffectEvidence::observed(probes::WORKSPACE_SHA256),
            ..Default::default()
        };
        let event = vp_protocol::types::Event {
            session_id: "s1".into(),
            seq: 1,
            kind: EFFECT_SETTLED_EVENT.into(),
            payload: json!({"receipt": receipt}),
            created_at: "2026-11-12T00:00:00Z".into(),
        };
        let projected = receipts_from_events(&[event]).expect("projection");
        assert_eq!(projected.len(), 1);
        assert!(receipts_match(&projected, std::slice::from_ref(&receipt)));
        let mut drifted = receipt;
        drifted.state = EffectState::Unknown;
        assert!(!receipts_match(&projected, &[drifted]));
    }

    #[test]
    fn malformed_event_payload_is_a_projection_failure() {
        let event = vp_protocol::types::Event {
            session_id: "s1".into(),
            seq: 1,
            kind: EFFECT_INTENT_EVENT.into(),
            payload: json!({"receipt": {"id": "missing state type"}}),
            created_at: "2026-11-12T00:00:00Z".into(),
        };
        assert!(receipts_from_events(&[event]).is_none());
    }

    #[test]
    fn policy_validation_bounds_the_stale_grace() {
        let mut value = EffectPolicy::default();
        assert!(value.normalized().is_ok());
        value.stale_intent_grace_secs = 0;
        assert!(value.normalized().is_err());
        value.stale_intent_grace_secs = EffectPolicy::MAX_STALE_GRACE_SECS + 1;
        assert!(value.normalized().is_err());
    }

    #[test]
    fn patch_paths_are_extracted_without_reading_content() {
        let patch =
            "--- a/one.rs\n+++ b/one.rs\n@@ -1 +1 @@\n-a\n+b\n--- /dev/null\n+++ b/two.rs\n";
        assert_eq!(patch_target_paths(patch), vec!["one.rs", "two.rs"]);
    }

    #[test]
    fn receipt_summary_is_bounded_and_contains_no_content() {
        let receipt = EffectReceipt {
            id: "e1".into(),
            state: EffectState::Unknown,
            attempt: 2,
            target: EffectTarget::new(kinds::SHELL, "cmd-abcdef"),
            evidence: EffectEvidence {
                reason: Some(reasons::NO_POSTCONDITION.into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let summary = receipt_summary(&receipt);
        assert!(summary.contains("unknown"));
        assert!(summary.contains("attempt 2"));
        assert!(summary.contains("no_postcondition"));
    }
}

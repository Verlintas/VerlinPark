//! Runtime version pinning and compatibility wire types (ADR-0196).
//!
//! These types are additive. Every field is `serde(default)` so an older
//! client, bundle, or event decodes without changing resume behavior. The
//! daemon owns the authoritative snapshot; this crate only defines the stable
//! JSON shape shared by RPC, CLI, bundles, and the SwiftUI client.

use serde::{Deserialize, Serialize};

/// Canonical identity of the runtime and the deterministic inputs a plan or
/// run was created with.
///
/// The snapshot is *observability and resume safety*, not an authorization
/// gate. It never contains prompts, provider keys, command text, file content,
/// or model output.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeSnapshot {
    /// Snapshot schema version. A change is an intentional wire/identity
    /// change and must be recorded in an ADR.
    #[serde(default)]
    pub schema_version: u32,
    /// Workspace crate version (`vp-protocol::APP_VERSION`).
    #[serde(default)]
    pub runtime_version: String,
    /// Best available compile-time build identity. This is not a signature.
    #[serde(default)]
    pub runtime_build: String,
    /// SHA-256 of the canonical project policy bundle used by the plan/run.
    #[serde(default)]
    pub policy_hash: String,
    /// Identity hash of the selected frozen tool-bundle membership.
    #[serde(default)]
    pub tool_bundle_hash: String,
    /// Instruction/prompt layout version used by the session.
    #[serde(default)]
    pub prompt_layout_version: u32,
    /// TeamPlan revision the snapshot was captured at.
    #[serde(default)]
    pub plan_revision: u32,
    /// SHA-256 of the canonical plan contract material.
    #[serde(default)]
    pub contract_hash: String,
    #[serde(default)]
    pub created_at: String,
}

impl RuntimeSnapshot {
    pub fn is_empty(&self) -> bool {
        self.schema_version == 0
            && self.runtime_version.is_empty()
            && self.runtime_build.is_empty()
            && self.policy_hash.is_empty()
            && self.tool_bundle_hash.is_empty()
            && self.plan_revision == 0
            && self.contract_hash.is_empty()
    }
}

/// One immutable project policy bundle. `json` is canonical JSON and `hash`
/// is its SHA-256. Rows are content-addressed and deduplicated per project.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicySnapshot {
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub json: String,
    #[serde(default)]
    pub created_at: String,
}

/// Compatibility classification for resuming a stored plan or durable run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompatibilityStatus {
    /// The current runtime, policy, bundle, layout, revision, and contract
    /// match the stored snapshot.
    #[default]
    Compatible,
    /// A version/policy/bundle/layout changed, but the stored plan and its
    /// contracts are still structurally valid. Resume proceeds with a visible
    /// `runtime.compatibility_drift` event. No model or tool routing changes.
    Drift,
    /// The plan revision, task DAG, contract, or plan existence no longer
    /// matches. Resume fails closed until an operator runs `plan.migrate`.
    Incompatible,
}

impl CompatibilityStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Compatible => "compatible",
            Self::Drift => "drift",
            Self::Incompatible => "incompatible",
        }
    }

    pub fn is_incompatible(self) -> bool {
        matches!(self, Self::Incompatible)
    }
}

/// One bounded field difference. Stored/current values are short diagnostic
/// strings; they never contain prompts, keys, command text, or file content.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompatibilityChange {
    #[serde(default)]
    pub field: String,
    #[serde(default)]
    pub stored: String,
    #[serde(default)]
    pub current: String,
    /// `drift` or `incompatible`.
    #[serde(default)]
    pub severity: String,
}

/// Bounded compatibility report. `changed` and `warnings` are capped by the
/// daemon; a report never grows with the size of a plan or event log.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompatibilityReport {
    #[serde(default)]
    pub status: CompatibilityStatus,
    /// `plan` or `run`.
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub run_id: Option<String>,
    /// Stored plan revision. For a legacy row this is `0`.
    #[serde(default)]
    pub plan_revision: u32,
    /// Age of the stored snapshot in seconds. Zero when unknown/legacy.
    #[serde(default)]
    pub snapshot_age_secs: u64,
    /// Bounded human-readable form of `snapshot_age_secs` (`"3h 4m"`). The
    /// numeric field remains authoritative.
    #[serde(default)]
    pub snapshot_age: String,
    #[serde(default)]
    pub snapshot_created_at: Option<String>,
    #[serde(default)]
    pub stored: Option<RuntimeSnapshot>,
    #[serde(default)]
    pub current: RuntimeSnapshot,
    #[serde(default)]
    pub changed: Vec<CompatibilityChange>,
    #[serde(default)]
    pub warnings: Vec<String>,
    /// True when an operator must run `plan.migrate` before resume.
    #[serde(default)]
    pub migration_required: bool,
    #[serde(default)]
    pub checked_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionCompatibilityParams {
    pub session_id: String,
    /// Omit to inspect the stored plan snapshot. Set to inspect one run.
    #[serde(default)]
    pub run_id: Option<String>,
}

/// `plan.migrate` requires an explicit operator acknowledgement. The RPC does
/// not create a pending GUI approval row; it classifies the action as
/// `host_state` and refuses to run without `approved = true`.
#[derive(Debug, Clone, Deserialize)]
pub struct PlanMigrateParams {
    pub session_id: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub expected_revision: Option<u32>,
    #[serde(default)]
    pub approved: bool,
    #[serde(default)]
    pub approved_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanMigrateResult {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub migrated: bool,
    #[serde(default)]
    pub plan_revision: u32,
    #[serde(default)]
    pub previous_snapshot: Option<RuntimeSnapshot>,
    #[serde(default)]
    pub snapshot: RuntimeSnapshot,
    #[serde(default)]
    pub compatibility: CompatibilityReport,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub event_seq: u64,
}

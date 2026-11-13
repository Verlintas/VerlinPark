//! Runtime version pinning, policy snapshots, compatibility, and explicit plan
//! migration (ADR-0196).
//!
//! A plan or durable run records the deterministic identity it started under:
//! runtime version/build, project policy hash, frozen tool-bundle membership,
//! prompt layout, plan revision, and plan contract hash. Resume compares the
//! stored identity with the current one:
//!
//! - `compatible` proceeds;
//! - `drift` proceeds with a visible `runtime.compatibility_drift` event;
//! - `incompatible` fails closed and requires an explicit, approval-classified
//!   `plan.migrate`.
//!
//! Nothing here rewrites a plan, task, budget, approval, sandbox, Verifier,
//! merge, capacity, or budget rule. A snapshot is audit/resume metadata, not an
//! authorization gate. `plan.migrate` revalidates the stored plan and updates
//! only the snapshot; it never edits task ids, dependencies, scopes, or
//! budgets.

use std::collections::{HashMap, HashSet};

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use vp_protocol::now_rfc3339;
use vp_protocol::runtime::{
    CompatibilityChange, CompatibilityReport, CompatibilityStatus, PlanMigrateParams,
    PlanMigrateResult, PolicySnapshot, RuntimeSnapshot,
};
use vp_protocol::types::{
    AttentionPolicy, CachePolicy, ContractPolicy, CriticalPathPolicy, EffectPolicy, GatePolicy,
    OrchestrationPolicy, Session, SessionRun, SessionRunPolicy, ShellPolicy, TaskSchedulerPolicy,
    TeamPlan,
};
use vp_store::Store;

use crate::Engine;
use crate::approvals::{canonical_json, sha256_hex};
use crate::tool_bundle;

/// Snapshot schema version. Bumping this changes plan/run identity and must be
/// recorded in an ADR.
pub const RUNTIME_SNAPSHOT_VERSION: u32 = 1;
/// Canonical policy-bundle version. It is part of the policy hash so a change
/// to the captured policy set creates a new snapshot instead of a false match.
pub const POLICY_SNAPSHOT_VERSION: u32 = 1;
/// Canonical plan-contract material version.
pub const PLAN_CONTRACT_HASH_VERSION: &str = "verlinpark.plan_contract/v1";

pub const POLICY_SNAPSHOT_EVENT: &str = "policy.snapshot";
pub const COMPATIBILITY_DRIFT_EVENT: &str = "runtime.compatibility_drift";
pub const PLAN_MIGRATED_EVENT: &str = "plan.migrated";

/// Stable typed error codes for compatibility and migration. They are rendered
/// with the Q3 `{code,message,retryable,hint}` shape by the daemon.
pub mod codes {
    pub const RUNTIME_INCOMPATIBLE: &str = "RUNTIME_INCOMPATIBLE";
    pub const PLAN_MIGRATION_APPROVAL_REQUIRED: &str = "PLAN_MIGRATION_APPROVAL_REQUIRED";
    pub const PLAN_MIGRATION_SESSION_ACTIVE: &str = "PLAN_MIGRATION_SESSION_ACTIVE";
    pub const PLAN_MIGRATION_NO_PLAN: &str = "PLAN_MIGRATION_NO_PLAN";
    pub const PLAN_MIGRATION_REVISION_CONFLICT: &str = "PLAN_MIGRATION_REVISION_CONFLICT";
    pub const PLAN_MIGRATION_INVALID: &str = "PLAN_MIGRATION_INVALID";
    pub const PLAN_MIGRATION_NOT_FOUND: &str = "PLAN_MIGRATION_NOT_FOUND";
}

pub fn error_hint(code: &str) -> (&'static str, bool) {
    match code {
        codes::RUNTIME_INCOMPATIBLE => (
            "Run `vp plan-migrate <session> --yes` (RPC `plan.migrate`) after reviewing the changed plan revision or contract. Do not edit the stored plan by hand.",
            false,
        ),
        codes::PLAN_MIGRATION_APPROVAL_REQUIRED => (
            "Migration is a host-state operator action. Re-run with explicit approval (`--yes` or `approved: true`).",
            false,
        ),
        codes::PLAN_MIGRATION_SESSION_ACTIVE => (
            "Wait for the session to become idle, failed, cancelled, or draft; migration never runs on an active session.",
            true,
        ),
        codes::PLAN_MIGRATION_NO_PLAN => (
            "Submit or restore a TeamPlan before migrating runtime identity.",
            false,
        ),
        codes::PLAN_MIGRATION_REVISION_CONFLICT => (
            "Refresh the session and pass the current `expected_revision`; the plan changed after your read.",
            true,
        ),
        codes::PLAN_MIGRATION_INVALID => (
            "The stored plan failed structural revalidation. Restore a valid revision or fix the plan with `team_plan.revise`; migration never rewrites tasks.",
            false,
        ),
        codes::PLAN_MIGRATION_NOT_FOUND => (
            "Refresh the session id; the session no longer exists.",
            false,
        ),
        _ => ("Refresh the session and retry.", false),
    }
}

#[derive(Debug, Clone)]
pub struct RuntimePinError {
    pub code: &'static str,
    pub message: String,
    pub report: Option<CompatibilityReport>,
}

impl RuntimePinError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            report: None,
        }
    }

    pub fn with_report(mut self, report: CompatibilityReport) -> Self {
        self.report = Some(report);
        self
    }

    pub fn data(&self) -> Value {
        let (hint, retryable) = error_hint(self.code);
        let mut data = json!({
            "code": self.code,
            "message": self.message,
            "retryable": retryable,
            "hint": hint,
        });
        if let (Some(report), Some(object)) = (&self.report, data.as_object_mut()) {
            object.insert(
                "compatibility".into(),
                serde_json::to_value(report).unwrap_or(Value::Null),
            );
        }
        data
    }
}

impl std::fmt::Display for RuntimePinError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "error[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for RuntimePinError {}

/// Best available compile-time build identity. This is a label, not a
/// signature; packages set `VERLINPARK_BUILD` at build time.
pub fn runtime_build() -> String {
    option_env!("VERLINPARK_BUILD")
        .map(str::to_string)
        .unwrap_or_else(|| format!("{}/{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")))
}

/// The project policies captured in one immutable policy bundle. The set is
/// deliberately limited to policies that affect plan scheduling, contract
/// derivation, approvals, caching, or resume. It contains no provider keys,
/// headers, prompts, file content, or MCP secrets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicySnapshotInputs {
    pub shell: ShellPolicy,
    pub contract: ContractPolicy,
    pub effect: EffectPolicy,
    pub gate: GatePolicy,
    pub cache: CachePolicy,
    pub orchestration: OrchestrationPolicy,
    pub scaling: vp_protocol::scaling::ScalingPolicy,
    pub task_scheduler: TaskSchedulerPolicy,
    pub session_run: SessionRunPolicy,
    pub critical_path: CriticalPathPolicy,
    pub attention: AttentionPolicy,
}

impl PolicySnapshotInputs {
    pub fn canonical_value(&self) -> Value {
        json!({
            "version": POLICY_SNAPSHOT_VERSION,
            "shell": self.shell,
            "contract": self.contract,
            "effect": self.effect,
            "gate": self.gate,
            "cache": self.cache,
            "orchestration": self.orchestration,
            "scaling": self.scaling,
            "task_scheduler": self.task_scheduler,
            "session_run": self.session_run,
            "critical_path": self.critical_path,
            "attention": self.attention,
        })
    }

    pub fn canonical_json(&self) -> String {
        canonical_json(&self.canonical_value())
    }

    pub fn hash(&self) -> String {
        sha256_hex(&self.canonical_json())
    }
}

/// Read all policy inputs in one blocking store call. This is intentionally
/// all-or-nothing: a missing/corrupt policy row fails the snapshot rather than
/// silently pinning defaults.
pub fn read_policy_inputs(store: &Store, project_id: &str) -> Result<PolicySnapshotInputs> {
    Ok(PolicySnapshotInputs {
        shell: store.get_shell_policy(project_id)?,
        contract: store.get_contract_policy(project_id)?,
        effect: store.get_effect_policy(project_id)?,
        gate: store.get_gate_policy(project_id)?,
        cache: store.get_cache_policy(project_id)?,
        orchestration: store.get_orchestration_policy(project_id)?,
        scaling: store.get_scaling_policy(project_id)?,
        task_scheduler: store.get_task_scheduler_policy(project_id)?,
        session_run: store.get_session_run_policy(project_id)?,
        critical_path: store.get_critical_path_policy(project_id)?,
        attention: store.get_attention_policy(project_id)?,
    })
}

/// Canonical plan-contract material. It covers the trusted Leader-authored
/// task definitions and the contract policy, not prompts or provider output.
pub fn plan_contract_hash(
    plan: &TeamPlan,
    contract_policy: &ContractPolicy,
    shell_network: &str,
) -> String {
    let material = json!({
        "version": PLAN_CONTRACT_HASH_VERSION,
        "goal": plan.goal,
        "departments": plan.departments,
        "tasks": plan.tasks,
        "contract_policy": contract_policy,
        "shell_network": shell_network,
    });
    sha256_hex(&canonical_json(&material))
}

pub fn leader_tool_bundle_hash() -> String {
    let selection =
        tool_bundle::select_bundle(tool_bundle::ROLE_LEADER, tool_bundle::PROFILE_GENERIC, None);
    tool_bundle::bundle_contract_hash(&selection)
}

pub fn role_tool_bundle_hash(role: &str, profile: &str, task_kind: Option<&str>) -> String {
    let selection = tool_bundle::select_bundle(role, profile, task_kind);
    tool_bundle::bundle_contract_hash(&selection)
}

pub fn parse_snapshot(raw: &str) -> Option<RuntimeSnapshot> {
    serde_json::from_str::<RuntimeSnapshot>(raw)
        .ok()
        .filter(|snapshot| !snapshot.is_empty())
}

pub fn snapshot_age_secs(snapshot: &RuntimeSnapshot, now: OffsetDateTime) -> u64 {
    OffsetDateTime::parse(&snapshot.created_at, &Rfc3339)
        .ok()
        .map(|created| (now - created).whole_seconds().max(0) as u64)
        .unwrap_or(0)
}

/// Bounded human-readable age. It is derived from the numeric field so a
/// client never has to parse a duration.
pub fn humanize_age(secs: u64) -> String {
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let minutes = (secs % 3_600) / 60;
    let seconds = secs % 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

fn bounded(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

fn push_change(
    changed: &mut Vec<CompatibilityChange>,
    field: &str,
    stored: &str,
    current: &str,
    severity: &str,
) {
    if changed.len() >= 32 {
        return;
    }
    changed.push(CompatibilityChange {
        field: field.to_string(),
        stored: bounded(stored, 160),
        current: bounded(current, 160),
        severity: severity.to_string(),
    });
}

fn push_warning(warnings: &mut Vec<String>, warning: impl Into<String>) {
    if warnings.len() >= 32 {
        return;
    }
    warnings.push(warning.into());
}

/// Compare a stored snapshot against the current runtime identity.
///
/// `plan_valid` is computed by the caller from the live plan/revision/contract.
/// A missing stored snapshot is legacy drift, not an incompatible plan: older
/// sessions must remain resumable after an upgrade. The report is bounded and
/// never contains prompts or tool argument values.
pub fn compare_snapshots(
    target: &str,
    session_id: &str,
    run_id: Option<&str>,
    stored: Option<&RuntimeSnapshot>,
    current: &RuntimeSnapshot,
    plan_valid: bool,
    now: OffsetDateTime,
) -> CompatibilityReport {
    let mut changed = Vec::new();
    let mut warnings = Vec::new();
    let mut incompatible = false;
    let mut drift = false;
    if let Some(stored) = stored {
        macro_rules! compare {
            ($field:literal, $left:expr, $right:expr, $severity:literal, $warning:literal) => {
                if $left != $right {
                    let left = format!("{}", $left);
                    let right = format!("{}", $right);
                    push_change(&mut changed, $field, &left, &right, $severity);
                    push_warning(&mut warnings, $warning);
                    if $severity == "incompatible" {
                        incompatible = true;
                    } else {
                        drift = true;
                    }
                }
            };
        }
        compare!(
            "schema_version",
            stored.schema_version,
            current.schema_version,
            "incompatible",
            "schema_version_changed"
        );
        compare!(
            "runtime_version",
            stored.runtime_version,
            current.runtime_version,
            "drift",
            "runtime_version_changed"
        );
        compare!(
            "runtime_build",
            stored.runtime_build,
            current.runtime_build,
            "drift",
            "runtime_build_changed"
        );
        compare!(
            "policy_hash",
            stored.policy_hash,
            current.policy_hash,
            "drift",
            "policy_changed"
        );
        compare!(
            "tool_bundle_hash",
            stored.tool_bundle_hash,
            current.tool_bundle_hash,
            "drift",
            "tool_bundle_changed"
        );
        compare!(
            "prompt_layout_version",
            stored.prompt_layout_version,
            current.prompt_layout_version,
            "drift",
            "prompt_layout_changed"
        );
        compare!(
            "plan_revision",
            stored.plan_revision,
            current.plan_revision,
            "incompatible",
            "plan_revision_changed"
        );
        // A contract change at the same revision is a distinct incompatible
        // condition. If the revision changed, the revision entry already
        // requires migration; avoid duplicate noisy entries.
        if stored.plan_revision == current.plan_revision {
            compare!(
                "contract_hash",
                stored.contract_hash,
                current.contract_hash,
                "incompatible",
                "contract_changed"
            );
        }
    } else {
        drift = true;
        push_warning(
            &mut warnings,
            "snapshot_missing: this plan/run predates runtime pinning; resume is allowed but identity was not verified",
        );
    }
    if !plan_valid {
        incompatible = true;
        push_warning(
            &mut warnings,
            "stored_plan_invalid: the plan revision, task DAG, or contract no longer matches the stored snapshot",
        );
    }
    let status = if incompatible {
        CompatibilityStatus::Incompatible
    } else if drift {
        CompatibilityStatus::Drift
    } else {
        CompatibilityStatus::Compatible
    };
    let snapshot_age_secs = stored
        .map(|snapshot| snapshot_age_secs(snapshot, now))
        .unwrap_or(0);
    CompatibilityReport {
        status,
        target: target.to_string(),
        session_id: session_id.to_string(),
        run_id: run_id.map(str::to_string),
        plan_revision: stored.map(|snapshot| snapshot.plan_revision).unwrap_or(0),
        snapshot_age_secs,
        snapshot_age: humanize_age(snapshot_age_secs),
        snapshot_created_at: stored.map(|snapshot| snapshot.created_at.clone()),
        stored: stored.cloned(),
        current: current.clone(),
        changed,
        warnings,
        migration_required: status == CompatibilityStatus::Incompatible || stored.is_none(),
        checked_at: now_rfc3339(),
    }
}

/// Structural revalidation for `plan.migrate`.
///
/// This calls the same trusted plan validator used at submission, adds a
/// dependency-cycle check (the submission validator accepts forward
/// references but does not prove acyclicity), and derives one contract per
/// task to prove the current policy can still hash every scope. It never
/// mutates the plan.
pub fn validate_plan_for_migration(
    plan: &TeamPlan,
    contract_policy: &ContractPolicy,
    shell_network: &str,
    project_id: &str,
    session_id: &str,
) -> Result<()> {
    if plan.departments.is_empty() {
        return Err(anyhow!(codes::PLAN_MIGRATION_INVALID));
    }
    crate::agent::validate_team_plan(plan, plan.departments.len())
        .map_err(|error| anyhow!("{}: {error}", codes::PLAN_MIGRATION_INVALID))?;
    detect_dependency_cycles(plan)
        .map_err(|error| anyhow!("{}: {error}", codes::PLAN_MIGRATION_INVALID))?;
    if contract_policy.enabled {
        let normalized = contract_policy
            .normalized()
            .map_err(|error| anyhow!("{}: {error}", codes::PLAN_MIGRATION_INVALID))?;
        for task in &plan.tasks {
            let department = plan
                .departments
                .iter()
                .find(|department| department.name == task.department)
                .ok_or_else(|| {
                    anyhow!(
                        "{}: task {} references an unknown department",
                        codes::PLAN_MIGRATION_INVALID,
                        task.id
                    )
                })?;
            let profile = task
                .profile
                .as_deref()
                .unwrap_or(department.profile.as_str());
            let selection = tool_bundle::select_bundle("worker", profile, Some(&task.kind));
            let allowed_tools = tool_bundle::bundle_tool_names(
                selection.kind,
                selection.role.as_str(),
                selection.profile.as_str(),
            )
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
            crate::contract::derive_contract(crate::contract::ContractDerivation {
                project_id: project_id.to_string(),
                session_id: session_id.to_string(),
                task_id: Some(task.id.clone()),
                plan_revision: 0,
                role: "worker".to_string(),
                profile: profile.to_string(),
                goal: plan.goal.clone(),
                task_kind: Some(task.kind.clone()),
                write_scope: task.write_scope.clone(),
                acceptance: task.acceptance.clone(),
                allowed_tools,
                policy: normalized.clone(),
                shell_network: shell_network.to_string(),
            })
            .map_err(|error| {
                anyhow!(
                    "{}: task {} contract cannot be derived: {error}",
                    codes::PLAN_MIGRATION_INVALID,
                    task.id
                )
            })?;
        }
    }
    Ok(())
}

fn detect_dependency_cycles(plan: &TeamPlan) -> Result<()> {
    let ids = plan
        .tasks
        .iter()
        .map(|task| task.id.as_str())
        .collect::<HashSet<_>>();
    let deps = plan
        .tasks
        .iter()
        .map(|task| {
            (
                task.id.as_str(),
                task.deps
                    .iter()
                    .map(String::as_str)
                    .filter(|dep| ids.contains(dep))
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<HashMap<_, _>>();
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum State {
        Unseen,
        Visiting,
        Done,
    }
    fn visit<'a>(
        node: &'a str,
        deps: &HashMap<&'a str, Vec<&'a str>>,
        states: &mut HashMap<&'a str, State>,
    ) -> Result<()> {
        match states.get(node).copied().unwrap_or(State::Unseen) {
            State::Done => return Ok(()),
            State::Visiting => {
                return Err(anyhow!("dependency cycle includes task {node}"));
            }
            State::Unseen => {}
        }
        states.insert(node, State::Visiting);
        if let Some(edges) = deps.get(node) {
            for dep in edges {
                visit(dep, deps, states)?;
            }
        }
        states.insert(node, State::Done);
        Ok(())
    }
    let mut states = HashMap::new();
    for task in &plan.tasks {
        visit(task.id.as_str(), &deps, &mut states)?;
    }
    Ok(())
}

/// Reconstruct the last plan submitted/revised at or before `seq`.
pub fn plan_at(events: &[vp_protocol::types::Event], seq: u64) -> Option<(TeamPlan, u64)> {
    let mut latest = None;
    for event in events.iter().filter(|event| event.seq <= seq) {
        if !matches!(
            event.kind.as_str(),
            "team_plan.submitted" | "team_plan.revised"
        ) {
            continue;
        }
        if let Some(plan) = crate::orchestration::plan::plan_from_event(event) {
            latest = Some((plan, event.seq));
        }
    }
    latest
}

/// Policy rows referenced by `policy.snapshot` events. Project id is carried in
/// each payload; a malformed row is ignored rather than guessed.
pub fn policy_snapshots_from_events(events: &[vp_protocol::types::Event]) -> Vec<PolicySnapshot> {
    let mut rows = Vec::new();
    let mut seen = HashSet::new();
    for event in events {
        if event.kind != POLICY_SNAPSHOT_EVENT {
            continue;
        }
        let Some(hash) = event.payload.get("hash").and_then(Value::as_str) else {
            continue;
        };
        let Some(project_id) = event.payload.get("project_id").and_then(Value::as_str) else {
            continue;
        };
        let Some(json) = event.payload.get("json").and_then(Value::as_str) else {
            continue;
        };
        if !seen.insert((project_id.to_string(), hash.to_string())) {
            continue;
        }
        rows.push(PolicySnapshot {
            project_id: project_id.to_string(),
            hash: hash.to_string(),
            json: json.to_string(),
            created_at: event.created_at.clone(),
        });
    }
    rows
}

impl Engine {
    pub async fn policy_inputs(&self, project_id: &str) -> Result<PolicySnapshotInputs> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking(move || read_policy_inputs(&store, &project_id)).await?
    }

    /// Ensure the immutable policy row exists and emit exactly one
    /// `policy.snapshot` audit event per (project, hash). The event is scoped to
    /// the session that first observed the bundle so replay and bundles can
    /// reconstruct the projection.
    pub async fn ensure_policy_snapshot(
        &self,
        session_id: &str,
        project_id: &str,
        inputs: &PolicySnapshotInputs,
    ) -> Result<(String, String)> {
        let json = inputs.canonical_json();
        let hash = sha256_hex(&json);
        let store = self.store.clone();
        let session = session_id.to_string();
        let project = project_id.to_string();
        let hash_for_store = hash.clone();
        let json_for_store = json.clone();
        let event = tokio::task::spawn_blocking(move || {
            store.ensure_policy_snapshot_with_event(
                &session,
                &project,
                &hash_for_store,
                &json_for_store,
            )
        })
        .await??;
        if let Some(event) = event {
            let _ = self.events.send(event);
        }
        Ok((hash, json))
    }

    /// Build and pin the snapshot for a stored plan. This is called when a plan
    /// is submitted or revised, and by `plan.migrate`. For plans created before
    /// C4 it backfills the snapshot on the next explicit submission/revision,
    /// never silently on resume.
    pub async fn pin_plan_snapshot(
        &self,
        session: &Session,
        plan: &TeamPlan,
        plan_revision: u32,
    ) -> Result<RuntimeSnapshot> {
        let inputs = self.policy_inputs(&session.project_id).await?;
        let (policy_hash, _) = self
            .ensure_policy_snapshot(&session.id, &session.project_id, &inputs)
            .await?;
        Ok(plan_snapshot_for(
            session,
            plan,
            plan_revision,
            &inputs,
            &policy_hash,
        ))
    }

    /// Build and pin the snapshot for a durable run. The run's role and plan
    /// task determine the tool-bundle membership; a Leader has no task.
    pub async fn pin_run_snapshot(
        &self,
        session: &Session,
        plan_revision: u32,
        role: &str,
        task_id: Option<&str>,
    ) -> Result<RuntimeSnapshot> {
        let inputs = self.policy_inputs(&session.project_id).await?;
        let (policy_hash, _) = self
            .ensure_policy_snapshot(&session.id, &session.project_id, &inputs)
            .await?;
        let plan = self.get_team_plan(&session.id).await?;
        let task = plan.as_ref().and_then(|plan| {
            task_id.and_then(|task_id| plan.tasks.iter().find(|task| task.id == task_id))
        });
        let profile = task
            .and_then(|task| task.profile.as_deref())
            .or_else(|| {
                plan.as_ref().and_then(|plan| {
                    task.and_then(|task| {
                        plan.departments
                            .iter()
                            .find(|department| department.name == task.department)
                            .map(|department| department.profile.as_str())
                    })
                })
            })
            .unwrap_or(tool_bundle::PROFILE_GENERIC);
        let task_kind = task.map(|task| task.kind.as_str());
        Ok(pin_snapshot(
            session,
            plan_revision,
            &inputs,
            &policy_hash,
            &role_tool_bundle_hash(role, profile, task_kind),
            plan.as_ref(),
        ))
    }

    /// Compare the stored plan snapshot with the current runtime. Read-only:
    /// this method never inserts a policy snapshot or appends an event.
    pub async fn plan_compatibility(&self, session_id: &str) -> Result<CompatibilityReport> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow!(codes::PLAN_MIGRATION_NOT_FOUND))?;
        let plan = self.get_team_plan(session_id).await?;
        let plan_revision = self.current_plan_revision(session_id).await?;
        let inputs = self.policy_inputs(&session.project_id).await?;
        let current = current_plan_snapshot(&session, plan.as_ref(), plan_revision, &inputs);
        let store = self.store.clone();
        let id = session_id.to_string();
        let stored_raw =
            tokio::task::spawn_blocking(move || store.get_plan_runtime_snapshot(&id)).await??;
        let stored = stored_raw.as_deref().and_then(parse_snapshot);
        let plan_valid = plan.is_some()
            && stored
                .as_ref()
                .map(|snapshot| snapshot.plan_revision == current.plan_revision)
                .unwrap_or(true)
            && stored
                .as_ref()
                .map(|snapshot| snapshot.contract_hash == current.contract_hash)
                .unwrap_or(true);
        Ok(compare_snapshots(
            "plan",
            session_id,
            None,
            stored.as_ref(),
            &current,
            plan_valid,
            OffsetDateTime::now_utc(),
        ))
    }

    /// Compare one durable run snapshot with the current runtime. This checks
    /// the run's plan revision, not merely the latest stored plan.
    pub async fn run_compatibility(
        &self,
        session_id: &str,
        run_id: &str,
    ) -> Result<CompatibilityReport> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow!(codes::PLAN_MIGRATION_NOT_FOUND))?;
        let store = self.store.clone();
        let requested = run_id.to_string();
        let run = tokio::task::spawn_blocking(move || store.get_session_run(&requested))
            .await??
            .ok_or_else(|| anyhow!("run not found: {run_id}"))?;
        if run.session_id != session_id {
            anyhow::bail!("run {run_id} belongs to another session");
        }
        let plan = self.get_team_plan(session_id).await?;
        let plan_revision = self.current_plan_revision(session_id).await?;
        let inputs = self.policy_inputs(&session.project_id).await?;
        let current = current_run_snapshot(&session, &run, plan.as_ref(), plan_revision, &inputs);
        let stored = run.runtime_snapshot.as_ref();
        let plan_valid = run_plan_valid(&run, stored, &current, plan.as_ref());
        Ok(compare_snapshots(
            "run",
            session_id,
            Some(&run.run_id),
            stored,
            &current,
            plan_valid,
            OffsetDateTime::now_utc(),
        ))
    }

    /// Compatibility summary for the RPC surface. `run_id = None` inspects the
    /// plan snapshot; otherwise the run snapshot.
    pub async fn session_compatibility(
        &self,
        session_id: &str,
        run_id: Option<&str>,
    ) -> Result<CompatibilityReport> {
        match run_id {
            Some(run_id) => self.run_compatibility(session_id, run_id).await,
            None => self.plan_compatibility(session_id).await,
        }
    }

    /// Append one bounded drift observation before a resume proceeds. This is
    /// the only mutation a drift path performs.
    pub async fn record_compatibility_drift(&self, report: &CompatibilityReport) -> Result<()> {
        let payload = json!({
            "status": report.status.as_str(),
            "target": report.target,
            "run_id": report.run_id,
            "plan_revision": report.plan_revision,
            "snapshot_age_secs": report.snapshot_age_secs,
            "changed": report.changed.iter().take(16).map(|change| json!({
                "field": change.field,
                "stored": change.stored,
                "current": change.current,
                "severity": change.severity,
            })).collect::<Vec<_>>(),
            "warnings": report.warnings.iter().take(16).cloned().collect::<Vec<_>>(),
        });
        self.append_event(&report.session_id, COMPATIBILITY_DRIFT_EVENT, payload)
            .await?;
        Ok(())
    }

    /// Explicit, approval-classified plan migration. It revalidates the stored
    /// plan and updates only the runtime snapshot. Task ids, dependencies,
    /// write scopes, budgets, and contracts are never rewritten.
    pub async fn migrate_plan(&self, params: PlanMigrateParams) -> Result<PlanMigrateResult> {
        let reason = params
            .reason
            .as_deref()
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
            .unwrap_or("runtime identity migration")
            .chars()
            .take(500)
            .collect::<String>();
        if !params.approved {
            return Err(RuntimePinError::new(
                codes::PLAN_MIGRATION_APPROVAL_REQUIRED,
                "plan.migrate requires explicit operator approval because it changes the pinned host-state snapshot",
            )
            .into());
        }
        let store = self.store.clone();
        let id = params.session_id.clone();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| {
                RuntimePinError::new(
                    codes::PLAN_MIGRATION_NOT_FOUND,
                    format!("session not found: {}", params.session_id),
                )
            })?;
        crate::ensure_session_quiescent(&session, "migrate the plan runtime snapshot").map_err(
            |error| {
                RuntimePinError::new(codes::PLAN_MIGRATION_SESSION_ACTIVE, format!("{error:#}"))
            },
        )?;
        let plan = self.get_team_plan(&session.id).await?.ok_or_else(|| {
            RuntimePinError::new(codes::PLAN_MIGRATION_NO_PLAN, "session has no TeamPlan")
        })?;
        let inputs = self.policy_inputs(&session.project_id).await?;
        if let Some(expected) = params.expected_revision {
            let current = self.current_plan_revision(&session.id).await?;
            if expected != current {
                return Err(RuntimePinError::new(
                    codes::PLAN_MIGRATION_REVISION_CONFLICT,
                    format!("plan revision conflict: expected {expected}, current {current}"),
                )
                .into());
            }
        }
        validate_plan_for_migration(
            &plan,
            &inputs.contract,
            &inputs.shell.network,
            &session.project_id,
            &session.id,
        )
        .map_err(|error| {
            RuntimePinError::new(codes::PLAN_MIGRATION_INVALID, format!("{error:#}"))
        })?;
        let plan_revision = self.current_plan_revision(&session.id).await?;
        let (policy_hash, _) = self
            .ensure_policy_snapshot(&session.id, &session.project_id, &inputs)
            .await?;
        let snapshot = plan_snapshot_for(&session, &plan, plan_revision, &inputs, &policy_hash);
        let store = self.store.clone();
        let previous_raw = {
            let store = store.clone();
            let id = session.id.clone();
            tokio::task::spawn_blocking(move || store.get_plan_runtime_snapshot(&id)).await??
        };
        let previous = previous_raw.as_deref().and_then(parse_snapshot);
        let compatibility = compare_snapshots(
            "plan",
            &session.id,
            None,
            previous.as_ref(),
            &snapshot,
            true,
            OffsetDateTime::now_utc(),
        );
        let mut warnings = compatibility.warnings.clone();
        // Effect/gate references are reported, never changed. A migration
        // must not reconcile a receipt, cancel a gate, or alter a budget.
        if let Ok(effects) = self.session_effects(&session.id, None, false, 100).await
            && (effects.counts.intent > 0 || effects.counts.unknown > 0)
        {
            push_warning(
                &mut warnings,
                format!(
                    "unresolved_effects: {} intent and {} unknown receipt(s) remain subject to effect.reconcile and merge blocking",
                    effects.counts.intent, effects.counts.unknown
                ),
            );
        }
        if let Ok(gates) = self.list_gates(&session.id, Some("open"), Some(100)).await
            && gates.counts.open > 0
        {
            push_warning(
                &mut warnings,
                format!(
                    "open_gates: {} external gate(s) remain open and still wake their continuations",
                    gates.counts.open
                ),
            );
        }
        let payload = json!({
            "reason": reason,
            "approval_risk": "host_state",
            "approved_by": params
                .approved_by
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(|name| name.chars().take(120).collect::<String>()),
            "plan_revision": plan_revision,
            "previous_snapshot": previous,
            "snapshot": snapshot,
            "changed": compatibility.changed,
            "warnings": warnings,
        });
        let snapshot_json = serde_json::to_string(&snapshot)?;
        // Repin interrupted/failed runs in the same explicit migration. A run
        // that pinned the old revision can otherwise never pass resume
        // preflight, even though the operator just approved the new snapshot.
        let runs_store = self.store.clone();
        let runs_session_id = session.id.clone();
        let runs =
            tokio::task::spawn_blocking(move || runs_store.list_all_session_runs(&runs_session_id))
                .await??;
        let run_snapshots = runs
            .into_iter()
            .filter(|run| {
                run.runtime_snapshot.is_some()
                    && matches!(run.status.as_str(), "interrupted" | "failed")
            })
            .filter_map(|run| {
                let current =
                    current_run_snapshot(&session, &run, Some(&plan), plan_revision, &inputs);
                serde_json::to_string(&current)
                    .ok()
                    .map(|json| (run.run_id, json, plan_revision))
            })
            .collect::<Vec<_>>();
        let store = self.store.clone();
        let id = session.id.clone();
        let payload_for_store = payload.clone();
        let event = tokio::task::spawn_blocking(move || {
            store.migrate_plan_runtime_snapshot_with_event(
                &id,
                &snapshot_json,
                &run_snapshots,
                payload_for_store,
            )
        })
        .await??;
        let _ = self.events.send(event.clone());
        Ok(PlanMigrateResult {
            session_id: session.id,
            migrated: true,
            plan_revision,
            previous_snapshot: previous,
            snapshot,
            compatibility,
            warnings,
            event_seq: event.seq,
        })
    }
}

fn plan_snapshot_for(
    session: &Session,
    plan: &TeamPlan,
    plan_revision: u32,
    inputs: &PolicySnapshotInputs,
    policy_hash: &str,
) -> RuntimeSnapshot {
    RuntimeSnapshot {
        schema_version: RUNTIME_SNAPSHOT_VERSION,
        runtime_version: vp_protocol::APP_VERSION.to_string(),
        runtime_build: runtime_build(),
        policy_hash: policy_hash.to_string(),
        tool_bundle_hash: leader_tool_bundle_hash(),
        prompt_layout_version: vp_protocol::types::normalize_prompt_layout(session.prompt_layout),
        plan_revision,
        contract_hash: plan_contract_hash(plan, &inputs.contract, &inputs.shell.network),
        created_at: now_rfc3339(),
    }
}

fn current_plan_snapshot(
    session: &Session,
    plan: Option<&TeamPlan>,
    plan_revision: u32,
    inputs: &PolicySnapshotInputs,
) -> RuntimeSnapshot {
    let contract_hash = plan
        .map(|plan| plan_contract_hash(plan, &inputs.contract, &inputs.shell.network))
        .unwrap_or_default();
    RuntimeSnapshot {
        schema_version: RUNTIME_SNAPSHOT_VERSION,
        runtime_version: vp_protocol::APP_VERSION.to_string(),
        runtime_build: runtime_build(),
        policy_hash: inputs.hash(),
        tool_bundle_hash: leader_tool_bundle_hash(),
        prompt_layout_version: vp_protocol::types::normalize_prompt_layout(session.prompt_layout),
        plan_revision,
        contract_hash,
        created_at: now_rfc3339(),
    }
}

fn current_run_snapshot(
    session: &Session,
    run: &SessionRun,
    plan: Option<&TeamPlan>,
    plan_revision: u32,
    inputs: &PolicySnapshotInputs,
) -> RuntimeSnapshot {
    let task = plan.as_ref().and_then(|plan| {
        run.task_id
            .as_deref()
            .and_then(|task_id| plan.tasks.iter().find(|task| task.id == task_id))
    });
    let profile = task
        .and_then(|task| task.profile.as_deref())
        .or_else(|| {
            plan.as_ref().and_then(|plan| {
                task.and_then(|task| {
                    plan.departments
                        .iter()
                        .find(|department| department.name == task.department)
                        .map(|department| department.profile.as_str())
                })
            })
        })
        .unwrap_or(tool_bundle::PROFILE_GENERIC);
    let contract_hash = plan
        .map(|plan| plan_contract_hash(plan, &inputs.contract, &inputs.shell.network))
        .unwrap_or_default();
    RuntimeSnapshot {
        schema_version: RUNTIME_SNAPSHOT_VERSION,
        runtime_version: vp_protocol::APP_VERSION.to_string(),
        runtime_build: runtime_build(),
        policy_hash: inputs.hash(),
        tool_bundle_hash: role_tool_bundle_hash(
            &run.agent_role,
            profile,
            task.map(|task| task.kind.as_str()),
        ),
        prompt_layout_version: vp_protocol::types::normalize_prompt_layout(session.prompt_layout),
        plan_revision,
        contract_hash,
        created_at: now_rfc3339(),
    }
}

fn pin_snapshot(
    session: &Session,
    plan_revision: u32,
    inputs: &PolicySnapshotInputs,
    policy_hash: &str,
    tool_bundle_hash: &str,
    plan: Option<&TeamPlan>,
) -> RuntimeSnapshot {
    RuntimeSnapshot {
        schema_version: RUNTIME_SNAPSHOT_VERSION,
        runtime_version: vp_protocol::APP_VERSION.to_string(),
        runtime_build: runtime_build(),
        policy_hash: policy_hash.to_string(),
        tool_bundle_hash: tool_bundle_hash.to_string(),
        prompt_layout_version: vp_protocol::types::normalize_prompt_layout(session.prompt_layout),
        plan_revision,
        contract_hash: plan
            .map(|plan| plan_contract_hash(plan, &inputs.contract, &inputs.shell.network))
            .unwrap_or_default(),
        created_at: now_rfc3339(),
    }
}

fn run_plan_valid(
    run: &SessionRun,
    stored: Option<&RuntimeSnapshot>,
    current: &RuntimeSnapshot,
    plan: Option<&TeamPlan>,
) -> bool {
    if run.plan_revision > 0 && plan.is_none() {
        return false;
    }
    if run.plan_revision > 0 && run.plan_revision != current.plan_revision {
        return false;
    }
    if let Some(stored) = stored {
        if stored.plan_revision != current.plan_revision {
            return false;
        }
        if stored.contract_hash != current.contract_hash {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    use tempfile::TempDir;
    use vp_protocol::types::{PlanDepartment, PlanTask, Project};
    use vp_store::Store;

    fn init_repo(path: &std::path::Path) {
        std::fs::write(path.join("README.md"), "hello\n").unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "test@verlinpark.local"],
            vec!["config", "user.name", "VerlinPark Test"],
            vec!["add", "."],
            vec!["commit", "-q", "-m", "initial"],
        ] {
            let status = Command::new("git")
                .arg("-C")
                .arg(path)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success());
        }
    }

    struct Fixture {
        _data: TempDir,
        _repo: TempDir,
        engine: Engine,
        project: Project,
        session: Session,
    }

    fn fixture() -> Fixture {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("pin"))
            .unwrap();
        let session = store.create_session(&project.id, "pin the plan").unwrap();
        let engine = Engine::new(store);
        Fixture {
            _data: data,
            _repo: repo,
            engine,
            project,
            session,
        }
    }

    fn policy_inputs() -> PolicySnapshotInputs {
        use serde::de::DeserializeOwned;
        fn defaults<T: DeserializeOwned>() -> T {
            serde_json::from_value(serde_json::json!({})).unwrap()
        }
        PolicySnapshotInputs {
            shell: defaults(),
            contract: defaults(),
            effect: defaults(),
            gate: defaults(),
            cache: defaults(),
            orchestration: defaults(),
            scaling: defaults(),
            task_scheduler: defaults(),
            session_run: defaults(),
            critical_path: defaults(),
            attention: defaults(),
        }
    }

    fn task(id: &str, deps: &[&str]) -> PlanTask {
        PlanTask {
            id: id.to_string(),
            department: "impl".to_string(),
            title: id.to_string(),
            kind: "coding".to_string(),
            priority: 0,
            profile: Some("coding".to_string()),
            deps: deps.iter().map(|dep| dep.to_string()).collect(),
            write_scope: vec![format!("src/{id}.rs")],
            acceptance: vec!["done".to_string()],
            token_budget: Some(1000),
            cost_budget_usd: None,
            parent_task_id: None,
            fanout_key: None,
            fanout_join_for: None,
        }
    }

    fn plan() -> TeamPlan {
        TeamPlan {
            goal: "pin".to_string(),
            departments: vec![PlanDepartment {
                name: "impl".to_string(),
                manager_title: "Implementation".to_string(),
                profile: "coding".to_string(),
                objective: "build".to_string(),
                token_budget: Some(10_000),
                cost_budget_usd: Some(1.0),
            }],
            tasks: vec![task("T1", &[]), task("T2", &["T1"])],
        }
    }

    #[test]
    fn policy_hash_is_canonical_and_changes_with_a_policy() {
        let first = policy_inputs();
        let second = policy_inputs();
        assert_eq!(first.hash(), second.hash());
        assert_eq!(first.canonical_json(), second.canonical_json());
        let mut changed = policy_inputs();
        changed.orchestration.max_attempts = changed.orchestration.max_attempts.saturating_add(1);
        assert_ne!(first.hash(), changed.hash());
        let mut contract_changed = policy_inputs();
        contract_changed.contract.mode = vp_protocol::types::ContractMode::Advisory;
        assert_ne!(first.hash(), contract_changed.hash());
    }

    #[test]
    fn plan_contract_hash_covers_tasks_and_scope() {
        let policy = ContractPolicy::default();
        let plan = plan();
        let base = plan_contract_hash(&plan, &policy, "allow");
        let mut changed = plan.clone();
        changed.tasks[0].write_scope = vec!["src/other.rs".to_string()];
        assert_ne!(base, plan_contract_hash(&changed, &policy, "allow"));
        assert_ne!(base, plan_contract_hash(&plan, &policy, "deny"));
    }

    #[test]
    fn migration_revalidation_finds_cycles_and_preserves_the_plan() {
        let policy = ContractPolicy::default();
        let mut cyclic = plan();
        cyclic.tasks[0].deps = vec!["T2".to_string()];
        let error =
            validate_plan_for_migration(&cyclic, &policy, "allow", "prj", "ses").unwrap_err();
        assert!(format!("{error:#}").contains(codes::PLAN_MIGRATION_INVALID));
        assert_eq!(cyclic.tasks[0].deps, vec!["T2".to_string()]);
        validate_plan_for_migration(&plan(), &policy, "allow", "prj", "ses").unwrap();
    }

    #[test]
    fn compatibility_states_are_bounded_and_explicit() {
        let now = OffsetDateTime::now_utc();
        let stored = RuntimeSnapshot {
            schema_version: RUNTIME_SNAPSHOT_VERSION,
            runtime_version: "0.1.0".into(),
            runtime_build: "build-a".into(),
            policy_hash: "policy-a".into(),
            tool_bundle_hash: "bundle-a".into(),
            prompt_layout_version: 3,
            plan_revision: 4,
            contract_hash: "contract-a".into(),
            created_at: now_rfc3339(),
        };
        let current = stored.clone();
        let report = compare_snapshots("plan", "ses", None, Some(&stored), &current, true, now);
        assert_eq!(report.status, CompatibilityStatus::Compatible);
        assert!(report.changed.is_empty());

        let mut drift = stored.clone();
        drift.policy_hash = "policy-b".into();
        drift.runtime_build = "build-b".into();
        let report = compare_snapshots("plan", "ses", None, Some(&stored), &drift, true, now);
        assert_eq!(report.status, CompatibilityStatus::Drift);
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning == "policy_changed")
        );
        assert!(!report.migration_required);

        let mut incompatible = stored.clone();
        incompatible.plan_revision = 5;
        incompatible.contract_hash = "contract-b".into();
        let report = compare_snapshots(
            "plan",
            "ses",
            None,
            Some(&stored),
            &incompatible,
            false,
            now,
        );
        assert_eq!(report.status, CompatibilityStatus::Incompatible);
        assert!(report.migration_required);
        assert!(report.changed.len() <= 32);
    }

    #[test]
    fn legacy_missing_snapshot_is_drift_not_incompatible() {
        let now = OffsetDateTime::now_utc();
        let current = RuntimeSnapshot {
            schema_version: RUNTIME_SNAPSHOT_VERSION,
            runtime_version: "0.1.0".into(),
            runtime_build: "build".into(),
            policy_hash: "policy".into(),
            tool_bundle_hash: "bundle".into(),
            prompt_layout_version: 3,
            plan_revision: 1,
            contract_hash: "contract".into(),
            created_at: now_rfc3339(),
        };
        let report = compare_snapshots("plan", "ses", None, None, &current, true, now);
        assert_eq!(report.status, CompatibilityStatus::Drift);
        assert!(report.migration_required);
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.starts_with("snapshot_missing"))
        );
    }

    #[tokio::test]
    async fn plan_and_run_snapshots_persist_and_are_stable() {
        let fixture = fixture();
        let plan = plan();
        let snapshot = fixture
            .engine
            .pin_plan_snapshot(&fixture.session, &plan, 1)
            .await
            .unwrap();
        let snapshot_json = serde_json::to_string(&snapshot).unwrap();
        let plan_json = serde_json::to_string(&plan).unwrap();
        let store = fixture.engine.store().clone();
        let session_id = fixture.session.id.clone();
        tokio::task::spawn_blocking(move || {
            store.set_team_plan_with_runtime_snapshot_and_event(
                &session_id,
                &plan_json,
                Some(&snapshot_json),
            )
        })
        .await
        .unwrap()
        .unwrap();
        let stored = fixture
            .engine
            .store()
            .get_plan_runtime_snapshot(&fixture.session.id)
            .unwrap()
            .unwrap();
        assert_eq!(
            parse_snapshot(&stored).unwrap().policy_hash,
            snapshot.policy_hash
        );
        let first_policy = snapshot.policy_hash.clone();
        let second = fixture
            .engine
            .pin_plan_snapshot(&fixture.session, &plan, 1)
            .await
            .unwrap();
        assert_eq!(first_policy, second.policy_hash);

        let run_snapshot = fixture
            .engine
            .pin_run_snapshot(&fixture.session, 1, "leader", None)
            .await
            .unwrap();
        let run = SessionRun {
            run_id: "run-pin".to_string(),
            session_id: fixture.session.id.clone(),
            project_id: fixture.project.id.clone(),
            agent_id: "agent".to_string(),
            agent_name: "Leader".to_string(),
            agent_role: "leader".to_string(),
            task_id: None,
            department: None,
            objective: "pin".to_string(),
            provider: "test".to_string(),
            model: "test".to_string(),
            status: "running".to_string(),
            attempt: 1,
            resumed_from_run_id: None,
            resume_root_id: "run-pin".to_string(),
            top_level: true,
            started_at: now_rfc3339(),
            updated_at: now_rfc3339(),
            ended_at: None,
            budget: Default::default(),
            worktree_head: String::new(),
            plan_revision: 1,
            runtime_snapshot: Some(run_snapshot.clone()),
            last_event_seq: 0,
            failure: None,
        };
        fixture.engine.store().start_session_run(&run).unwrap();
        let stored_run = fixture
            .engine
            .store()
            .get_session_run("run-pin")
            .unwrap()
            .unwrap();
        assert_eq!(
            stored_run.runtime_snapshot.unwrap().policy_hash,
            run_snapshot.policy_hash
        );
    }

    #[tokio::test]
    async fn resume_compatibility_is_compatible_then_drift_then_incompatible() {
        let fixture = fixture();
        let plan = plan();
        fixture
            .engine
            .set_team_plan(&fixture.session.id, &plan)
            .await
            .unwrap();
        let report = fixture
            .engine
            .plan_compatibility(&fixture.session.id)
            .await
            .unwrap();
        assert_eq!(report.status, CompatibilityStatus::Compatible);

        // A policy-only change is visible drift and keeps the same contract.
        let mut policy = fixture
            .engine
            .orchestration_policy(&fixture.project.id)
            .await
            .unwrap();
        policy.max_attempts = policy.max_attempts.saturating_add(1);
        fixture
            .engine
            .set_orchestration_policy(&fixture.project.id, &policy)
            .await
            .unwrap();
        let drift = fixture
            .engine
            .plan_compatibility(&fixture.session.id)
            .await
            .unwrap();
        assert_eq!(drift.status, CompatibilityStatus::Drift);
        assert!(
            drift
                .changed
                .iter()
                .any(|change| change.field == "policy_hash")
        );
        fixture
            .engine
            .record_compatibility_drift(&drift)
            .await
            .unwrap();
        let events = fixture
            .engine
            .all_events(&fixture.session.id)
            .await
            .unwrap();
        assert!(
            events
                .iter()
                .any(|event| event.kind == COMPATIBILITY_DRIFT_EVENT)
        );

        // Rewriting the stored plan without a new snapshot changes the
        // contract while the revision stays the same: fail closed.
        let mut rewritten = plan.clone();
        rewritten.tasks[0].write_scope = vec!["src/other.rs".to_string()];
        fixture
            .engine
            .store()
            .set_team_plan(
                &fixture.session.id,
                &serde_json::to_string(&rewritten).unwrap(),
            )
            .unwrap();
        let incompatible = fixture
            .engine
            .plan_compatibility(&fixture.session.id)
            .await
            .unwrap();
        assert_eq!(incompatible.status, CompatibilityStatus::Incompatible);
        assert!(incompatible.migration_required);
    }

    #[tokio::test]
    async fn migration_requires_approval_and_never_rewrites_tasks() {
        let fixture = fixture();
        let plan = plan();
        fixture
            .engine
            .set_team_plan(&fixture.session.id, &plan)
            .await
            .unwrap();
        let before_plan = fixture
            .engine
            .store()
            .get_team_plan(&fixture.session.id)
            .unwrap()
            .unwrap();
        let before_snapshot = fixture
            .engine
            .store()
            .get_plan_runtime_snapshot(&fixture.session.id)
            .unwrap();
        let event_count = fixture
            .engine
            .all_events(&fixture.session.id)
            .await
            .unwrap()
            .len();

        let refused = fixture
            .engine
            .migrate_plan(PlanMigrateParams {
                session_id: fixture.session.id.clone(),
                reason: Some("test".to_string()),
                expected_revision: None,
                approved: false,
                approved_by: None,
            })
            .await
            .unwrap_err();
        let pin = refused.downcast_ref::<RuntimePinError>().unwrap();
        assert_eq!(pin.code, codes::PLAN_MIGRATION_APPROVAL_REQUIRED);
        assert_eq!(
            fixture
                .engine
                .store()
                .get_plan_runtime_snapshot(&fixture.session.id)
                .unwrap(),
            before_snapshot
        );

        let migrated = fixture
            .engine
            .migrate_plan(PlanMigrateParams {
                session_id: fixture.session.id.clone(),
                reason: Some("operator re-pin".to_string()),
                expected_revision: Some(1),
                approved: true,
                approved_by: Some("test".to_string()),
            })
            .await
            .unwrap();
        assert!(migrated.migrated);
        assert_eq!(migrated.snapshot.plan_revision, 1);
        assert_eq!(
            fixture
                .engine
                .store()
                .get_team_plan(&fixture.session.id)
                .unwrap()
                .unwrap(),
            before_plan
        );
        let after_events = fixture
            .engine
            .all_events(&fixture.session.id)
            .await
            .unwrap();
        assert!(after_events.len() > event_count);
        let event = after_events
            .iter()
            .rev()
            .find(|event| event.kind == PLAN_MIGRATED_EVENT)
            .unwrap();
        assert_eq!(event.payload["approval_risk"], "host_state");
        assert_eq!(event.payload["snapshot"]["plan_revision"], 1);
    }

    #[tokio::test]
    async fn invalid_migration_leaves_the_snapshot_untouched() {
        let fixture = fixture();
        let mut plan = plan();
        plan.tasks[0].deps = vec!["T2".to_string()];
        fixture
            .engine
            .store()
            .set_team_plan(&fixture.session.id, &serde_json::to_string(&plan).unwrap())
            .unwrap();
        let before = fixture
            .engine
            .store()
            .get_plan_runtime_snapshot(&fixture.session.id)
            .unwrap();
        let event_count = fixture
            .engine
            .all_events(&fixture.session.id)
            .await
            .unwrap()
            .len();
        let error = fixture
            .engine
            .migrate_plan(PlanMigrateParams {
                session_id: fixture.session.id.clone(),
                reason: None,
                expected_revision: None,
                approved: true,
                approved_by: None,
            })
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains(codes::PLAN_MIGRATION_INVALID));
        assert_eq!(
            fixture
                .engine
                .store()
                .get_plan_runtime_snapshot(&fixture.session.id)
                .unwrap(),
            before
        );
        assert_eq!(
            fixture
                .engine
                .all_events(&fixture.session.id)
                .await
                .unwrap()
                .len(),
            event_count
        );
    }

    #[tokio::test]
    async fn controller_replay_is_read_only_and_detects_a_changed_decision() {
        let fixture = fixture();
        let plan = plan();
        let store = fixture.engine.store().clone();
        let session_id = fixture.session.id.clone();
        let plan_json = serde_json::to_string(&plan).unwrap();
        store.set_team_plan(&session_id, &plan_json).unwrap();
        store
            .append_event(
                &session_id,
                "team_plan.submitted",
                serde_json::json!({"plan": plan}),
            )
            .unwrap();
        store
            .append_event(
                &session_id,
                "plan_task.started",
                serde_json::json!({"task_id": "T1"}),
            )
            .unwrap();
        store
            .append_event(
                &session_id,
                "plan_task.finished",
                serde_json::json!({"task_id": "T1", "task_status": "failed", "reason": "boom"}),
            )
            .unwrap();
        store
            .append_event(
                &session_id,
                "orchestration.decision",
                serde_json::json!({
                    "decision_id": "d1",
                    "action": "retry",
                    "task_id": "T1",
                    "detail": "retry",
                    "attempt": 1,
                    "agent_id": "orchestrator",
                }),
            )
            .unwrap();
        let before_events = fixture.engine.all_events(&session_id).await.unwrap().len();
        let report = fixture
            .engine
            .replay_orchestration(&session_id, Some(200))
            .await
            .unwrap();
        assert!(report.controller_equivalent_only);
        assert!(report.matched >= 1);
        assert!(!report.truncated);
        assert_eq!(
            fixture.engine.all_events(&session_id).await.unwrap().len(),
            before_events
        );
        let plan_after = fixture
            .engine
            .store()
            .get_team_plan(&session_id)
            .unwrap()
            .unwrap();
        assert_eq!(plan_after, plan_json);

        // A recorded action that the current controller does not produce is a
        // bounded mismatch, not a retry or a plan mutation.
        store
            .append_event(
                &session_id,
                "orchestration.decision",
                serde_json::json!({
                    "decision_id": "d2",
                    "action": "cancel",
                    "task_id": "T2",
                    "detail": "manual",
                    "attempt": 0,
                    "agent_id": "orchestrator",
                }),
            )
            .unwrap();
        let report = fixture
            .engine
            .replay_orchestration(&session_id, Some(200))
            .await
            .unwrap();
        assert!(report.mismatched >= 1);
        assert!(report.entries.iter().any(|entry| {
            entry.status == vp_protocol::replay::ControllerReplayStatus::Mismatched
                && entry.seq > 0
                && !entry.changes.is_empty()
        }));

        let truncated = fixture
            .engine
            .replay_orchestration(&session_id, Some(1))
            .await
            .unwrap();
        assert!(truncated.truncated);
        assert_eq!(truncated.examined, 1);
        let again = fixture
            .engine
            .replay_orchestration(&session_id, Some(1))
            .await
            .unwrap();
        assert_eq!(truncated.entries, again.entries);
    }
}

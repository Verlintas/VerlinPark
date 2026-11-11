//! Deterministic organization scaling wire types (ADR-0172).
//!
//! Scaling is a project-scoped, opt-in policy over the existing four-tier
//! organization. The daemon never creates Managers or rewrites a TeamPlan; it
//! only adjusts the Worker/Labour slot ceilings used by the existing delegation
//! path. All fields are additive and defaulted so older clients and manifests
//! keep decoding.

use serde::{Deserialize, Serialize};

/// Runtime hard ceilings. A policy can never exceed these even when a client
/// sends a larger value.
pub const HARD_MAX_WORKERS: u32 = 10;
pub const HARD_MAX_LABOUR: u32 = 40;
/// A single deterministic decision may move a role by at most this many slots,
/// even if a client asks for a larger policy step.
pub const HARD_MAX_STEP: u32 = 10;
/// Maximum tasks-per-ready-queue threshold accepted by the policy.
pub const MAX_EXPAND_READY_THRESHOLD: u32 = 100;
/// Longest expansion cooldown accepted by the policy (24 hours).
pub const MAX_COOLDOWN_SECS: u64 = 86_400;
/// Longest critical-delay accepted by the policy (24 hours).
pub const MAX_CRITICAL_DELAY_SECS: u64 = 86_400;
/// Longest idle window accepted before scale-down (7 days).
pub const MAX_SCALE_DOWN_IDLE_SECS: u64 = 604_800;

fn default_min_workers() -> u32 {
    1
}

fn default_max_workers() -> u32 {
    4
}

fn default_min_labour() -> u32 {
    1
}

fn default_max_labour() -> u32 {
    8
}

fn default_expand_ready_threshold() -> u32 {
    2
}

fn default_expand_critical_delay_secs() -> u64 {
    120
}

fn default_scale_down_idle_secs() -> u64 {
    600
}

fn default_cooldown_secs() -> u64 {
    120
}

fn default_max_step() -> u32 {
    2
}

/// Project-scoped deterministic scaling policy.
///
/// `enabled` is deliberately false by default. An operator opts in per project;
/// the controller then evaluates on the daemon tick and applies only bounded
/// Worker/Labour target changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScalingPolicy {
    #[serde(default)]
    pub enabled: bool,
    /// Scale-down floor for Worker slots. It is not a forced pre-expansion:
    /// slots are still only raised when ready work and the other gates support it.
    #[serde(default = "default_min_workers")]
    pub min_workers: u32,
    /// Hard ceiling for this project's Worker slots. Clamped to the runtime cap.
    #[serde(default = "default_max_workers")]
    pub max_workers: u32,
    /// Scale-down floor for Labour slots.
    #[serde(default = "default_min_labour")]
    pub min_labour: u32,
    /// Hard ceiling for this project's Labour slots. Clamped to the runtime cap.
    #[serde(default = "default_max_labour")]
    pub max_labour: u32,
    /// Minimum number of runnable ready tasks before a Worker expansion is
    /// considered. One freshly queued task does not churn the team.
    #[serde(default = "default_expand_ready_threshold")]
    pub expand_ready_threshold: u32,
    /// Hysteresis before pre-expanding an idle/failed plan. A running plan with
    /// saturated slots may expand immediately; a non-running plan waits until
    /// its oldest ready task has been queued this long.
    #[serde(default = "default_expand_critical_delay_secs")]
    pub expand_critical_delay_secs: u64,
    /// How long a role must remain below its target before scale-down.
    #[serde(default = "default_scale_down_idle_secs")]
    pub scale_down_idle_secs: u64,
    /// Minimum time between applied expansion/scale-down decisions for the same
    /// plan revision. A new plan revision resets the cooldown.
    #[serde(default = "default_cooldown_secs")]
    pub cooldown_secs: u64,
    /// Maximum slots moved by one decision.
    #[serde(default = "default_max_step")]
    pub max_step: u32,
    /// Optional expansion guard. When set, the controller requires a known
    /// budget headroom ratio at or above this value. `None` disables the check;
    /// an unknown ratio with a configured threshold fails closed.
    #[serde(default)]
    pub budget_headroom_ratio: Option<f64>,
}

impl Default for ScalingPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            min_workers: default_min_workers(),
            max_workers: default_max_workers(),
            min_labour: default_min_labour(),
            max_labour: default_max_labour(),
            expand_ready_threshold: default_expand_ready_threshold(),
            expand_critical_delay_secs: default_expand_critical_delay_secs(),
            scale_down_idle_secs: default_scale_down_idle_secs(),
            cooldown_secs: default_cooldown_secs(),
            max_step: default_max_step(),
            budget_headroom_ratio: None,
        }
    }
}

impl ScalingPolicy {
    /// Clamp user input into the only ranges the daemon enforces. `set` still
    /// validates first so an out-of-range request is rejected instead of
    /// silently changing behavior.
    pub fn normalized(mut self) -> Self {
        self.min_workers = self.min_workers.clamp(1, HARD_MAX_WORKERS);
        self.max_workers = self.max_workers.clamp(self.min_workers, HARD_MAX_WORKERS);
        self.min_labour = self.min_labour.clamp(1, HARD_MAX_LABOUR);
        self.max_labour = self.min_labour.max(self.max_labour.min(HARD_MAX_LABOUR));
        self.expand_ready_threshold = self
            .expand_ready_threshold
            .clamp(1, MAX_EXPAND_READY_THRESHOLD);
        self.expand_critical_delay_secs =
            self.expand_critical_delay_secs.min(MAX_CRITICAL_DELAY_SECS);
        self.scale_down_idle_secs = self.scale_down_idle_secs.min(MAX_SCALE_DOWN_IDLE_SECS);
        self.cooldown_secs = self.cooldown_secs.min(MAX_COOLDOWN_SECS);
        self.max_step = self.max_step.clamp(1, HARD_MAX_STEP);
        self.budget_headroom_ratio = self
            .budget_headroom_ratio
            .filter(|value| value.is_finite())
            .map(|value| value.clamp(0.0, 1.0));
        self
    }

    /// Reject a policy the daemon cannot safely apply. Validation is separate
    /// from normalization so an explicit RPC/CLI mistake is visible.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if !(1..=HARD_MAX_WORKERS).contains(&self.min_workers) {
            return Err(format!(
                "min_workers must be between 1 and {HARD_MAX_WORKERS}"
            ));
        }
        if !(self.min_workers..=HARD_MAX_WORKERS).contains(&self.max_workers) {
            return Err(format!(
                "max_workers must be between min_workers and {HARD_MAX_WORKERS}"
            ));
        }
        if !(1..=HARD_MAX_LABOUR).contains(&self.min_labour) {
            return Err(format!(
                "min_labour must be between 1 and {HARD_MAX_LABOUR}"
            ));
        }
        if !(self.min_labour..=HARD_MAX_LABOUR).contains(&self.max_labour) {
            return Err(format!(
                "max_labour must be between min_labour and {HARD_MAX_LABOUR}"
            ));
        }
        if !(1..=MAX_EXPAND_READY_THRESHOLD).contains(&self.expand_ready_threshold) {
            return Err(format!(
                "expand_ready_threshold must be between 1 and {MAX_EXPAND_READY_THRESHOLD}"
            ));
        }
        if self.expand_critical_delay_secs > MAX_CRITICAL_DELAY_SECS {
            return Err(format!(
                "expand_critical_delay_secs must be at most {MAX_CRITICAL_DELAY_SECS}"
            ));
        }
        if self.scale_down_idle_secs > MAX_SCALE_DOWN_IDLE_SECS {
            return Err(format!(
                "scale_down_idle_secs must be at most {MAX_SCALE_DOWN_IDLE_SECS}"
            ));
        }
        if self.cooldown_secs > MAX_COOLDOWN_SECS {
            return Err(format!("cooldown_secs must be at most {MAX_COOLDOWN_SECS}"));
        }
        if !(1..=HARD_MAX_STEP).contains(&self.max_step) {
            return Err(format!("max_step must be between 1 and {HARD_MAX_STEP}"));
        }
        if let Some(ratio) = self.budget_headroom_ratio
            && (!ratio.is_finite() || !(0.0..=1.0).contains(&ratio))
        {
            return Err("budget_headroom_ratio must be between 0.0 and 1.0".to_string());
        }
        Ok(())
    }

    /// Stable one-line description used by logs and the dashboard.
    pub fn summary(&self) -> String {
        format!(
            "{} · Workers {}-{} · Labour {}-{} · threshold {} · cooldown {}s · step {}",
            if self.enabled { "enabled" } else { "disabled" },
            self.min_workers,
            self.max_workers,
            self.min_labour,
            self.max_labour,
            self.expand_ready_threshold,
            self.cooldown_secs,
            self.max_step
        )
    }
}

/// One plan task observed by the pure controller. It intentionally contains
/// only durable scheduling data; no model, prompt, or provider information.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScalingTaskObservation {
    pub task_id: String,
    #[serde(default)]
    pub department: String,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub deps: Vec<String>,
    #[serde(default)]
    pub write_scope: Vec<String>,
    /// RFC3339 enqueue/retry time used for the critical-delay hysteresis.
    #[serde(default)]
    pub ready_since: Option<String>,
}

/// Deterministic controller action.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScalingAction {
    #[default]
    Hold,
    ExpandWorkers,
    ExpandLabour,
    ExpandWorkersAndLabour,
    ScaleDownLabour,
    ScaleDownWorkers,
}

impl ScalingAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hold => "hold",
            Self::ExpandWorkers => "expand_workers",
            Self::ExpandLabour => "expand_labour",
            Self::ExpandWorkersAndLabour => "expand_workers_and_labour",
            Self::ScaleDownLabour => "scale_down_labour",
            Self::ScaleDownWorkers => "scale_down_workers",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "hold" => Some(Self::Hold),
            "expand_workers" | "expand-workers" => Some(Self::ExpandWorkers),
            "expand_labour" | "expand-labour" => Some(Self::ExpandLabour),
            "expand_workers_and_labour" | "expand-workers-and-labour" => {
                Some(Self::ExpandWorkersAndLabour)
            }
            "scale_down_labour" | "scale-down-labour" => Some(Self::ScaleDownLabour),
            "scale_down_workers" | "scale-down-workers" => Some(Self::ScaleDownWorkers),
            _ => None,
        }
    }

    pub fn is_expansion(self) -> bool {
        matches!(
            self,
            Self::ExpandWorkers | Self::ExpandLabour | Self::ExpandWorkersAndLabour
        )
    }

    pub fn is_scale_down(self) -> bool {
        matches!(self, Self::ScaleDownLabour | Self::ScaleDownWorkers)
    }
}

/// One evaluated decision. The projection/event payload is this exact shape.
/// `applied` is false for previews and holds; the tick sets it after the slot
/// targets have been committed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScalingDecision {
    #[serde(default)]
    pub decision_id: String,
    /// Idempotency key for one plan revision/tick.
    #[serde(default)]
    pub decision_key: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub plan_revision: u64,
    #[serde(default)]
    pub action: ScalingAction,
    #[serde(default)]
    pub current_workers: u32,
    #[serde(default)]
    pub current_labour: u32,
    #[serde(default)]
    pub target_workers: u32,
    #[serde(default)]
    pub target_labour: u32,
    #[serde(default)]
    pub active_workers: u32,
    #[serde(default)]
    pub active_labour: u32,
    #[serde(default)]
    pub ready_tasks: u32,
    #[serde(default)]
    pub running_tasks: u32,
    #[serde(default)]
    pub inputs_hash: String,
    #[serde(default)]
    pub reasons: Vec<String>,
    #[serde(default)]
    pub manager_recommended: bool,
    #[serde(default)]
    pub manager_evidence: Option<String>,
    #[serde(default)]
    pub applied: bool,
    #[serde(default)]
    pub idle_since_workers: Option<String>,
    #[serde(default)]
    pub idle_since_labour: Option<String>,
    #[serde(default)]
    pub created_at: String,
}

impl ScalingDecision {
    /// Stable, bounded reason line for logs and the CLI.
    pub fn reason_line(&self) -> String {
        if self.reasons.is_empty() {
            return "no reasons".to_string();
        }
        self.reasons.join(" | ")
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScalingPolicyParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetScalingPolicyParams {
    pub project_id: String,
    pub policy: ScalingPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScalingPolicyResult {
    pub project_id: String,
    pub policy: ScalingPolicy,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScalingPreviewParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScalingPreviewResult {
    pub session_id: String,
    pub project_id: String,
    pub policy: ScalingPolicy,
    /// Empty when the policy is disabled: preview still reports why it is a no-op.
    pub decision: ScalingDecision,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ScalingStatusParams {
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScalingSessionStatus {
    pub session_id: String,
    pub project_id: String,
    pub goal: String,
    pub status: String,
    pub plan_revision: u64,
    pub plan_tasks: usize,
    pub ready_tasks: usize,
    pub running_tasks: usize,
    pub completed_tasks: usize,
    pub current_workers: u32,
    pub current_labour: u32,
    pub active_workers: u32,
    pub active_labour: u32,
    #[serde(default)]
    pub latest: Option<ScalingDecision>,
    #[serde(default)]
    pub idle_since_workers: Option<String>,
    #[serde(default)]
    pub idle_since_labour: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScalingProjectStatus {
    pub project_id: String,
    pub project_name: String,
    pub policy: ScalingPolicy,
    #[serde(default)]
    pub sessions: Vec<ScalingSessionStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScalingStatusResult {
    pub global_worker_cap: usize,
    pub global_labour_cap: usize,
    pub global_worker_available: usize,
    pub global_labour_available: usize,
    #[serde(default)]
    pub projects: Vec<ScalingProjectStatus>,
}

/// One daemon-tick summary. It is operational telemetry, not a second source of
/// truth; decisions are persisted individually.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScalingTickResult {
    pub projects: usize,
    pub sessions: usize,
    pub evaluated: usize,
    pub persisted: usize,
    pub applied: usize,
    pub holds: usize,
    pub expanded: usize,
    pub scaled_down: usize,
    pub manager_recommendations: usize,
    #[serde(default)]
    pub errors: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_disabled_and_bounded() {
        let policy = ScalingPolicy::default();
        assert!(!policy.enabled);
        assert_eq!(policy.min_workers, 1);
        assert_eq!(policy.max_workers, 4);
        assert_eq!(policy.min_labour, 1);
        assert_eq!(policy.max_labour, 8);
        assert_eq!(policy.expand_ready_threshold, 2);
        assert_eq!(policy.cooldown_secs, 120);
        assert_eq!(policy.max_step, 2);
        assert_eq!(policy.budget_headroom_ratio, None);
        policy.validate().unwrap();
    }

    #[test]
    fn legacy_policy_json_decodes_with_additive_defaults() {
        let policy: ScalingPolicy =
            serde_json::from_str(r#"{"enabled":true}"#).expect("legacy policy");
        assert!(policy.enabled);
        assert_eq!(policy.max_workers, 4);
        assert_eq!(policy.scale_down_idle_secs, 600);
        assert_eq!(policy.expand_critical_delay_secs, 120);
        assert_eq!(policy.max_step, 2);
    }

    #[test]
    fn normalization_clamps_without_silently_accepting_invalid_input() {
        let policy = ScalingPolicy {
            enabled: true,
            min_workers: 0,
            max_workers: 99,
            min_labour: 0,
            max_labour: 99,
            expand_ready_threshold: 0,
            expand_critical_delay_secs: u64::MAX,
            scale_down_idle_secs: u64::MAX,
            cooldown_secs: u64::MAX,
            max_step: 0,
            budget_headroom_ratio: Some(f64::NAN),
        };
        assert!(policy.validate().is_err());
        let normalized = policy.clone().normalized();
        assert_eq!(normalized.min_workers, 1);
        assert_eq!(normalized.max_workers, HARD_MAX_WORKERS);
        assert_eq!(normalized.min_labour, 1);
        assert_eq!(normalized.max_labour, HARD_MAX_LABOUR);
        assert_eq!(normalized.expand_ready_threshold, 1);
        assert_eq!(
            normalized.expand_critical_delay_secs,
            MAX_CRITICAL_DELAY_SECS
        );
        assert_eq!(normalized.scale_down_idle_secs, MAX_SCALE_DOWN_IDLE_SECS);
        assert_eq!(normalized.cooldown_secs, MAX_COOLDOWN_SECS);
        assert_eq!(normalized.max_step, 1);
        assert_eq!(normalized.budget_headroom_ratio, None);
        normalized.validate().unwrap();
    }

    #[test]
    fn rejects_crossed_min_max_and_bad_ratio() {
        let mut policy = ScalingPolicy {
            min_workers: 5,
            max_workers: 3,
            ..Default::default()
        };
        assert!(policy.validate().is_err());
        policy.max_workers = 5;
        policy.budget_headroom_ratio = Some(1.5);
        assert!(policy.validate().is_err());
        policy.budget_headroom_ratio = Some(-0.1);
        assert!(policy.validate().is_err());
        policy.budget_headroom_ratio = Some(0.25);
        policy.validate().unwrap();
    }

    #[test]
    fn action_wire_values_round_trip() {
        for action in [
            ScalingAction::Hold,
            ScalingAction::ExpandWorkers,
            ScalingAction::ExpandLabour,
            ScalingAction::ExpandWorkersAndLabour,
            ScalingAction::ScaleDownLabour,
            ScalingAction::ScaleDownWorkers,
        ] {
            assert_eq!(ScalingAction::parse(action.as_str()), Some(action));
            let encoded = serde_json::to_string(&action).unwrap();
            let decoded: ScalingAction = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded, action);
        }
        assert_eq!(ScalingAction::parse("bogus"), None);
        assert!(ScalingAction::ExpandWorkers.is_expansion());
        assert!(ScalingAction::ScaleDownWorkers.is_scale_down());
        assert!(!ScalingAction::Hold.is_expansion());
    }

    #[test]
    fn decision_decodes_legacy_payload_with_only_required_core() {
        let json = r#"{
            "session_id":"s1",
            "project_id":"p1",
            "action":"hold",
            "target_workers":3,
            "target_labour":6
        }"#;
        let decision: ScalingDecision = serde_json::from_str(json).unwrap();
        assert_eq!(decision.action, ScalingAction::Hold);
        assert_eq!(decision.target_workers, 3);
        assert!(decision.reasons.is_empty());
        assert!(!decision.applied);
        assert_eq!(decision.plan_revision, 0);
    }

    #[test]
    fn policy_result_and_tick_are_serde_stable() {
        let result = ScalingPolicyResult {
            project_id: "p1".into(),
            policy: ScalingPolicy {
                enabled: true,
                ..Default::default()
            },
        };
        let encoded = serde_json::to_value(&result).unwrap();
        assert_eq!(encoded["policy"]["enabled"], serde_json::json!(true));
        assert_eq!(encoded["policy"]["max_workers"], serde_json::json!(4));
        let tick = ScalingTickResult {
            projects: 1,
            sessions: 2,
            evaluated: 2,
            persisted: 2,
            applied: 1,
            holds: 1,
            expanded: 1,
            scaled_down: 0,
            manager_recommendations: 0,
            errors: Vec::new(),
        };
        let round: ScalingTickResult =
            serde_json::from_value(serde_json::to_value(&tick).unwrap()).unwrap();
        assert_eq!(round, tick);
    }
}

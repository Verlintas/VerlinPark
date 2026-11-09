//! Deterministic multi-agent orchestration projections and policy.
//!
//! The runtime owns scheduling; this module only derives policy, plan
//! revisions, dependency readiness, retry timing, and the snapshot consumed by
//! Collab/CLI. It never calls a model.

pub mod controller;
pub mod plan;
pub mod policy;
pub mod snapshot;

pub use controller::{
    ControllerAction, ControllerInput, plan_actions, session_has_actionable_work,
};
pub use plan::{
    PlanDiff, blocked_by, diff_plan, ensure_revision_safe, latest_result_summary, plan_revision,
    ready_tasks, revisions_from_events, status_map,
};
pub use policy::{OrchestrationPolicyError, retry_delay, should_auto_retry, validate_policy};
pub use snapshot::{OrchestrationInput, build_snapshot, coordination_notes, decisions_from_events};

#[cfg(test)]
mod tests;

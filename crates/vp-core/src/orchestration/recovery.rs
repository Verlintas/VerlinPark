//! Conservative, policy-gated stall recovery.
//!
//! Observation remains the default. When a project opts into `escalate` or
//! `retry`, this module decides whether a repeated stall observation should
//! produce one new recovery transition. It never calls a model, never widens a
//! write scope, and never bypasses approval, Verifier, merge, capacity, or
//! budget gates. The runtime performs the actual cooperative cancellation,
//! retry, and plan resume through the existing task lifecycle paths.

use std::collections::BTreeMap;

use vp_protocol::types::{
    Event, OrchestrationPolicy, OrchestrationRecovery, StallRecoveryMode, TaskStall,
};

pub const RECOVERY_REQUESTED: &str = "requested";
pub const RECOVERY_WAITING_CANCEL: &str = "waiting_cancel";
pub const RECOVERY_RETRIED: &str = "retried";
pub const RECOVERY_ESCALATED: &str = "escalated";
pub const RECOVERY_FAILED: &str = "failed";
pub const RECOVERY_UNNECESSARY: &str = "unnecessary";

pub const RECOVERY_EVENT_REQUESTED: &str = "orchestration.recovery_requested";
pub const RECOVERY_EVENT_WAITING_CANCEL: &str = "orchestration.recovery_waiting_cancel";
pub const RECOVERY_EVENT_RETRIED: &str = "orchestration.recovery_retried";
pub const RECOVERY_EVENT_ESCALATED: &str = "orchestration.recovery_escalated";
pub const RECOVERY_EVENT_FAILED: &str = "orchestration.recovery_failed";
pub const RECOVERY_EVENT_UNNECESSARY: &str = "orchestration.recovery_unnecessary";

/// One deterministic choice for a stall observation. The caller applies it
/// through the event store; this function has no side effects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StallRecoveryDecision {
    /// No recovery mutation for this observation.
    None,
    /// Record one escalation for this stall window. The task keeps running.
    Escalate,
    /// Request cooperative cancellation and schedule one recovery retry after
    /// the task and session become terminal/quiescent.
    Retry,
    /// The configured retry budget is exhausted; fail and escalate instead of
    /// starting another cancellation/retry loop.
    Exhausted { reason: String },
}

/// Decide whether one fresh stall observation should advance recovery.
///
/// The task must still be `running` or `claimed`, the observation must be at
/// or above `stall_min_observations`, and the same stall window must not have
/// already produced the same transition. Terminal recovery rows are final, so
/// a failed or unnecessary task never loops.
pub fn decide_stall_recovery(
    policy: &OrchestrationPolicy,
    recovery: Option<&OrchestrationRecovery>,
    stall: &TaskStall,
) -> StallRecoveryDecision {
    if policy.stall_recovery == StallRecoveryMode::Observe {
        return StallRecoveryDecision::None;
    }
    if stall.stall_count < policy.stall_min_observations.max(1) {
        return StallRecoveryDecision::None;
    }
    if !matches!(stall.status.as_str(), "running" | "claimed") {
        return StallRecoveryDecision::None;
    }
    if let Some(recovery) = recovery {
        if matches!(
            recovery.status.as_str(),
            RECOVERY_FAILED | RECOVERY_UNNECESSARY
        ) {
            return StallRecoveryDecision::None;
        }
        if matches!(
            recovery.status.as_str(),
            RECOVERY_REQUESTED | RECOVERY_WAITING_CANCEL
        ) {
            // A request or cooperative cancellation is already in flight.
            return StallRecoveryDecision::None;
        }
        // `stall_count` is cumulative per task/plan revision. A recovery row
        // records the last count that produced a transition, so a later
        // transition needs a fresh window of `stall_min_observations`, not one
        // extra event after an earlier recovery.
        if stall.stall_count
            < recovery
                .last_stall_count
                .saturating_add(policy.stall_min_observations.max(1))
        {
            return StallRecoveryDecision::None;
        }
    }
    match policy.stall_recovery {
        StallRecoveryMode::Observe => StallRecoveryDecision::None,
        StallRecoveryMode::Escalate => StallRecoveryDecision::Escalate,
        StallRecoveryMode::Retry => {
            let attempts = recovery.map(|recovery| recovery.attempts).unwrap_or(0);
            if attempts >= policy.stall_max_attempts {
                StallRecoveryDecision::Exhausted {
                    reason: format!(
                        "stall recovery exhausted {attempts}/{} attempt(s)",
                        policy.stall_max_attempts
                    ),
                }
            } else {
                StallRecoveryDecision::Retry
            }
        }
    }
}

/// Rebuild the recovery projection from the latest recovery event per
/// `(session_id, plan_seq, task_id)`. Event payloads carry the full row.
pub fn recoveries_from_events(events: &[Event]) -> Vec<OrchestrationRecovery> {
    let mut latest = BTreeMap::new();
    for event in events {
        if !event.kind.starts_with("orchestration.recovery_") {
            continue;
        }
        let Ok(recovery) = serde_json::from_value::<OrchestrationRecovery>(event.payload.clone())
        else {
            continue;
        };
        latest.insert(
            (
                recovery.session_id.clone(),
                recovery.plan_seq,
                recovery.task_id.clone(),
            ),
            recovery,
        );
    }
    latest.into_values().collect()
}

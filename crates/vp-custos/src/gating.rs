//! Pure CUSTOS Observer/Advanced gate.
//!
//! This module does not mutate state or enforce by itself. Daemon/UI callers
//! use the deterministic decision to block or acknowledge risky work.

use vp_protocol::custos::{
    CustosGate, CustosGateDecision, CustosGateReason, CustosTier, ProjectCustosPolicy,
};

/// Evaluate one risk reason. High-value projects always require Advanced;
/// Observer can be acknowledged for unattended/scheduler/large-plan work.
pub fn evaluate(
    tier: CustosTier,
    policy: &ProjectCustosPolicy,
    reason: CustosGateReason,
) -> CustosGate {
    if tier == CustosTier::Advanced {
        return CustosGate {
            decision: CustosGateDecision::Allow,
            tier,
            reason,
            message: "CUSTOS Advanced is installed; the local guardian is active.".to_string(),
        };
    }
    if reason == CustosGateReason::HighValueProject {
        return CustosGate {
            decision: CustosGateDecision::RequireAdvanced,
            tier,
            reason,
            message: "High-value projects require CUSTOS Advanced; Observer cannot contain a runaway process or restore corrupted state.".to_string(),
        };
    }
    if policy.observer_unattended_ack {
        return CustosGate {
            decision: CustosGateDecision::Allow,
            tier,
            reason,
            message: "CUSTOS Observer acknowledged for this project; only observation, diagnostics, and notifications are active.".to_string(),
        };
    }
    CustosGate {
        decision: CustosGateDecision::RequireObserverAcknowledgement,
        tier,
        reason,
        message: format!(
            "This {} action is eligible for CUSTOS Advanced. Install Advanced or explicitly acknowledge that Observer cannot stop a runaway process or recover state.",
            reason.as_str()
        ),
    }
}

/// Convenience for a plan with `task_count` tasks: a plan above the project
/// threshold is gated as `LargePlan`; otherwise it is allowed without a gate.
pub fn evaluate_plan(
    tier: CustosTier,
    policy: &ProjectCustosPolicy,
    task_count: usize,
) -> Option<CustosGate> {
    policy
        .is_large_plan(task_count)
        .then(|| evaluate(tier, policy, CustosGateReason::LargePlan))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> ProjectCustosPolicy {
        ProjectCustosPolicy::default()
    }

    #[test]
    fn advanced_allows_all_risk_reasons() {
        for reason in [
            CustosGateReason::Unattended,
            CustosGateReason::SchedulerAutoDispatch,
            CustosGateReason::LargePlan,
            CustosGateReason::HighValueProject,
        ] {
            let gate = evaluate(CustosTier::Advanced, &policy(), reason);
            assert!(gate.allowed(), "{reason:?}: {gate:?}");
        }
    }

    #[test]
    fn observer_requires_advanced_for_high_value_even_with_ack() {
        let mut policy = policy();
        policy.observer_unattended_ack = true;
        let gate = evaluate(
            CustosTier::Observer,
            &policy,
            CustosGateReason::HighValueProject,
        );
        assert_eq!(gate.decision, CustosGateDecision::RequireAdvanced);
        assert!(!gate.allowed());
    }

    #[test]
    fn observer_requires_ack_for_risky_work_then_allows_it() {
        for reason in [
            CustosGateReason::Unattended,
            CustosGateReason::SchedulerAutoDispatch,
            CustosGateReason::LargePlan,
        ] {
            let gate = evaluate(CustosTier::Observer, &policy(), reason);
            assert_eq!(
                gate.decision,
                CustosGateDecision::RequireObserverAcknowledgement,
                "{reason:?}"
            );
            assert!(!gate.allowed());
        }

        let mut acknowledged = policy();
        acknowledged.observer_unattended_ack = true;
        for reason in [
            CustosGateReason::Unattended,
            CustosGateReason::SchedulerAutoDispatch,
            CustosGateReason::LargePlan,
        ] {
            let gate = evaluate(CustosTier::Observer, &acknowledged, reason);
            assert!(gate.allowed(), "{reason:?}: {gate:?}");
        }
    }

    #[test]
    fn plan_threshold_is_strictly_greater_than() {
        let policy = policy(); // threshold 25
        assert!(evaluate_plan(CustosTier::Observer, &policy, 25).is_none());
        let gate = evaluate_plan(CustosTier::Observer, &policy, 26).unwrap();
        assert_eq!(
            gate.decision,
            CustosGateDecision::RequireObserverAcknowledgement
        );
    }
}

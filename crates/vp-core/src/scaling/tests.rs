use super::*;

use tempfile::TempDir;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use vp_protocol::types::{PlanDepartment, PlanTask, TeamPlan};

fn now() -> OffsetDateTime {
    OffsetDateTime::parse("2026-11-11T12:00:00Z", &Rfc3339).unwrap()
}

fn at(seconds_ago: i64) -> OffsetDateTime {
    now() - time::Duration::seconds(seconds_ago)
}

fn policy() -> ScalingPolicy {
    ScalingPolicy {
        enabled: true,
        min_workers: 1,
        max_workers: 6,
        min_labour: 1,
        max_labour: 12,
        expand_ready_threshold: 2,
        expand_critical_delay_secs: 0,
        scale_down_idle_secs: 60,
        cooldown_secs: 30,
        max_step: 2,
        budget_headroom_ratio: None,
    }
}

fn observation() -> ScalingObservation {
    let mut observation = ScalingObservation::new("ses-1", "proj-1");
    observation.session_status = "running".to_string();
    observation.plan_revision = 1;
    observation.current_workers = 2;
    observation.current_labour = 4;
    observation.active_workers = 2;
    observation.active_labour = 2;
    observation.global_worker_available = 10;
    observation.global_labour_available = 10;
    observation.now = now();
    observation
}

fn task(
    id: &str,
    department: &str,
    priority: i32,
    status: &str,
    deps: &[&str],
    scope: &[&str],
    ready_since: Option<OffsetDateTime>,
) -> ScalingTaskObservation {
    ScalingTaskObservation {
        task_id: id.to_string(),
        department: department.to_string(),
        priority,
        status: status.to_string(),
        deps: deps.iter().map(|dep| dep.to_string()).collect(),
        write_scope: scope.iter().map(|scope| scope.to_string()).collect(),
        ready_since: ready_since.map(|value| value.format(&Rfc3339).unwrap()),
    }
}

fn pending(id: &str, priority: i32, scope: &[&str]) -> ScalingTaskObservation {
    task(id, "impl", priority, "pending", &[], scope, Some(at(120)))
}

fn running(id: &str, priority: i32, scope: &[&str]) -> ScalingTaskObservation {
    task(id, "impl", priority, "running", &[], scope, Some(at(120)))
}

fn reasons(decision: &ScalingDecision) -> String {
    decision.reasons.join(" | ")
}

#[test]
fn disabled_policy_is_a_noop() {
    let mut observation = observation();
    observation.tasks = (0..6)
        .map(|i| pending(&format!("T{i}"), 0, &[&format!("src/{i}")]))
        .collect();
    let mut policy = policy();
    policy.enabled = false;
    let decision = evaluate(&policy, &observation);
    assert_eq!(decision.action, ScalingAction::Hold);
    assert_eq!(decision.target_workers, 2);
    assert_eq!(decision.target_labour, 4);
    assert!(reasons(&decision).contains("policy_disabled"));
    assert!(!decision.applied);
}

#[test]
fn expands_workers_when_saturated_with_ready_demand() {
    let mut observation = observation();
    observation.current_labour = 4;
    observation.tasks = vec![
        pending("T1", 20, &["src/a"]),
        pending("T2", 10, &["src/b"]),
        pending("T3", 0, &["src/c"]),
    ];
    let decision = evaluate(&policy(), &observation);
    assert_eq!(decision.action, ScalingAction::ExpandWorkers);
    assert_eq!(decision.current_workers, 2);
    assert_eq!(decision.target_workers, 3);
    assert_eq!(decision.target_labour, 4);
    assert!(reasons(&decision).contains("expand_workers:2->3"));
    assert!(!decision.manager_recommended);
    assert!(!decision.applied);
}

#[test]
fn combined_expansion_is_bounded_and_explained() {
    let mut observation = observation();
    observation.current_workers = 1;
    observation.current_labour = 1;
    observation.active_workers = 1;
    observation.active_labour = 1;
    observation.tasks = vec![
        pending("T1", 0, &["src/a"]),
        pending("T2", 0, &["src/b"]),
        pending("T3", 0, &["src/c"]),
    ];
    let decision = evaluate(&policy(), &observation);
    assert_eq!(decision.action, ScalingAction::ExpandWorkersAndLabour);
    assert_eq!(decision.target_workers, 3);
    assert_eq!(decision.target_labour, 3);
    assert!(reasons(&decision).contains("expand_workers:1->3"));
    assert!(reasons(&decision).contains("expand_labour:1->3"));
}

#[test]
fn ready_threshold_prevents_one_task_churn() {
    let mut observation = observation();
    observation.current_workers = 2;
    observation.active_workers = 2;
    observation.current_labour = 4;
    observation.tasks = vec![
        running("R1", 0, &["src/r1"]),
        running("R2", 0, &["src/r2"]),
        pending("T1", 0, &["src/a"]),
    ];
    let decision = evaluate(&policy(), &observation);
    assert_eq!(decision.action, ScalingAction::Hold);
    assert!(reasons(&decision).contains("below_expand_threshold:1/2"));
    assert!(reasons(&decision).contains("plan_demand_waiting_for_threshold"));
}

#[test]
fn idle_plan_waits_for_critical_delay() {
    let mut observation = observation();
    observation.session_status = "idle".to_string();
    observation.current_workers = 1;
    observation.active_workers = 0;
    observation.current_labour = 3;
    observation.tasks = vec![
        task("T1", "impl", 0, "pending", &[], &["src/a"], Some(at(30))),
        task("T2", "impl", 0, "pending", &[], &["src/b"], Some(at(40))),
        task("T3", "impl", 0, "pending", &[], &["src/c"], Some(at(50))),
    ];
    let mut policy = policy();
    policy.expand_critical_delay_secs = 120;
    let decision = evaluate(&policy, &observation);
    assert_eq!(decision.action, ScalingAction::Hold);
    assert!(reasons(&decision).contains("critical_delay_wait:50/120s"));

    observation.tasks = vec![
        task("T1", "impl", 0, "pending", &[], &["src/a"], Some(at(300))),
        task("T2", "impl", 0, "pending", &[], &["src/b"], Some(at(300))),
        task("T3", "impl", 0, "pending", &[], &["src/c"], Some(at(300))),
    ];
    let decision = evaluate(&policy, &observation);
    assert_eq!(decision.action, ScalingAction::ExpandWorkers);
    assert_eq!(decision.target_workers, 3);
}

#[test]
fn cooldown_blocks_expansion_and_revision_resets_it() {
    let mut observation = observation();
    observation.current_labour = 4;
    observation.tasks = vec![
        pending("T1", 0, &["src/a"]),
        pending("T2", 0, &["src/b"]),
        pending("T3", 0, &["src/c"]),
    ];
    observation.last_action_at = Some(at(10));
    observation.last_plan_revision = Some(1);
    let decision = evaluate(&policy(), &observation);
    assert_eq!(decision.action, ScalingAction::Hold);
    assert!(reasons(&decision).contains("cooldown_active:10/30s"));

    observation.plan_revision = 2;
    let decision = evaluate(&policy(), &observation);
    assert_eq!(decision.action, ScalingAction::ExpandWorkers);
    assert!(reasons(&decision).contains("plan_revision_changed_cooldown_reset"));
}

#[test]
fn max_step_bounds_expansion_even_with_large_policy_target() {
    let mut observation = observation();
    observation.current_workers = 1;
    observation.active_workers = 1;
    observation.current_labour = 6;
    observation.tasks = (0..6)
        .map(|i| pending(&format!("T{i}"), 0, &[&format!("src/{i}")]))
        .collect();
    let mut policy = policy();
    policy.max_workers = 10;
    policy.max_step = 2;
    let decision = evaluate(&policy, &observation);
    assert_eq!(decision.action, ScalingAction::ExpandWorkers);
    assert_eq!(decision.target_workers, 3);
    assert!(reasons(&decision).contains("max_step_applied"));
}

#[test]
fn global_worker_capacity_blocks_expansion() {
    let mut observation = observation();
    observation.current_labour = 4;
    observation.global_worker_available = 0;
    observation.tasks = vec![
        pending("T1", 0, &["src/a"]),
        pending("T2", 0, &["src/b"]),
        pending("T3", 0, &["src/c"]),
    ];
    let decision = evaluate(&policy(), &observation);
    assert_eq!(decision.action, ScalingAction::Hold);
    assert!(reasons(&decision).contains("global_worker_capacity_blocked"));
}

#[test]
fn global_labour_capacity_blocks_only_labour() {
    let mut observation = observation();
    observation.current_workers = 4;
    observation.active_workers = 4;
    observation.current_labour = 1;
    observation.active_labour = 1;
    observation.global_worker_available = 10;
    observation.global_labour_available = 0;
    observation.tasks = vec![
        pending("T1", 0, &["src/a"]),
        pending("T2", 0, &["src/b"]),
        pending("T3", 0, &["src/c"]),
        pending("T4", 0, &["src/d"]),
    ];
    let decision = evaluate(&policy(), &observation);
    assert_eq!(decision.action, ScalingAction::Hold);
    assert!(reasons(&decision).contains("global_labour_capacity_blocked"));
}

#[test]
fn budget_headroom_gate_fails_closed_when_unknown() {
    let mut observation = observation();
    observation.current_labour = 4;
    observation.tasks = vec![
        pending("T1", 0, &["src/a"]),
        pending("T2", 0, &["src/b"]),
        pending("T3", 0, &["src/c"]),
    ];
    let mut policy = policy();
    policy.budget_headroom_ratio = Some(0.25);
    observation.budget_headroom_ratio = None;
    let decision = evaluate(&policy, &observation);
    assert_eq!(decision.action, ScalingAction::Hold);
    assert!(reasons(&decision).contains("budget_headroom_unknown"));

    observation.budget_headroom_ratio = Some(0.1);
    let decision = evaluate(&policy, &observation);
    assert_eq!(decision.action, ScalingAction::Hold);
    assert!(reasons(&decision).contains("budget_headroom_below_threshold:0.100<0.250"));

    observation.budget_headroom_ratio = Some(0.5);
    let decision = evaluate(&policy, &observation);
    assert_eq!(decision.action, ScalingAction::ExpandWorkers);
    assert!(reasons(&decision).contains("budget_headroom_ok:0.500"));
}

#[test]
fn plan_demand_bounds_the_target_and_never_inflates_from_scope_conflicts() {
    let mut observation = observation();
    observation.current_workers = 1;
    observation.active_workers = 1;
    observation.current_labour = 4;
    observation.tasks = vec![
        pending("T1", 0, &["src/app"]),
        pending("T2", 0, &["src/app/routes"]),
        pending("T3", 0, &["src/lib"]),
    ];
    let decision = evaluate(&policy(), &observation);
    // T1 and T2 overlap; only T1 plus T3 can run concurrently.
    assert_eq!(decision.action, ScalingAction::ExpandWorkers);
    assert_eq!(decision.target_workers, 2);
    assert_eq!(decision.ready_tasks, 3);
}

#[test]
fn dependency_and_retry_time_gate_readiness() {
    let mut observation = observation();
    observation.current_labour = 4;
    observation.tasks = vec![
        pending("parent", 0, &["src/a"]),
        task(
            "child",
            "impl",
            0,
            "pending",
            &["parent"],
            &["src/b"],
            Some(at(120)),
        ),
        task(
            "retry",
            "impl",
            0,
            "failed",
            &[],
            &["src/c"],
            Some(now() + time::Duration::seconds(300)),
        ),
    ];
    let decision = evaluate(&policy(), &observation);
    assert_eq!(decision.ready_tasks, 1);
    assert!(reasons(&decision).contains("ready_order:parent"));
}

#[test]
fn deterministic_tie_break_is_priority_department_task_id() {
    let mut observation = observation();
    observation.current_workers = 4;
    observation.active_workers = 4;
    observation.current_labour = 6;
    observation.tasks = vec![
        task("z", "beta", 10, "pending", &[], &["src/z"], Some(at(120))),
        task("a", "beta", 10, "pending", &[], &["src/a"], Some(at(120))),
        task(
            "high",
            "gamma",
            50,
            "pending",
            &[],
            &["src/high"],
            Some(at(120)),
        ),
        task(
            "alpha",
            "alpha",
            10,
            "pending",
            &[],
            &["src/alpha"],
            Some(at(120)),
        ),
    ];
    let decision = evaluate(&policy(), &observation);
    let reasons = reasons(&decision);
    assert!(
        reasons.contains("ready_order:high>alpha>a>z"),
        "unexpected order: {reasons}"
    );
}

#[test]
fn scale_down_waits_for_the_idle_window() {
    let mut observation = observation();
    observation.current_workers = 4;
    observation.current_labour = 6;
    observation.active_workers = 1;
    observation.active_labour = 1;
    observation.tasks = vec![running("R1", 0, &["src/r1"])];
    observation.idle_since_workers = Some(at(10));
    observation.idle_since_labour = Some(at(10));
    let decision = evaluate(&policy(), &observation);
    assert_eq!(decision.action, ScalingAction::Hold);
    assert!(reasons(&decision).contains("scale_down_labour_waiting:10s"));
    assert!(reasons(&decision).contains("scale_down_workers_waiting:10s"));
}

#[test]
fn scale_down_reduces_labour_before_workers() {
    let mut observation = observation();
    observation.current_workers = 5;
    observation.current_labour = 6;
    observation.active_workers = 1;
    observation.active_labour = 1;
    observation.tasks = vec![running("R1", 0, &["src/r1"])];
    observation.idle_since_workers = Some(at(120));
    observation.idle_since_labour = Some(at(120));
    let decision = evaluate(&policy(), &observation);
    assert_eq!(decision.action, ScalingAction::ScaleDownLabour);
    assert_eq!(decision.target_labour, 4);
    assert_eq!(decision.target_workers, 5);
    assert!(reasons(&decision).contains("scale_down_labour:6->4"));
    assert!(reasons(&decision).contains("scale_down_workers_pending_next_tick"));

    let mut second = observation.clone();
    second.current_labour = 1;
    second.idle_since_labour = Some(at(120));
    second.idle_since_workers = Some(at(120));
    let decision = evaluate(&policy(), &second);
    assert_eq!(decision.action, ScalingAction::ScaleDownWorkers);
    assert_eq!(decision.target_workers, 3);
    assert_eq!(decision.target_labour, 1);
}

#[test]
fn plan_revision_change_resets_idle_carry_and_waits_again() {
    let mut observation = observation();
    observation.current_workers = 5;
    observation.current_labour = 1;
    observation.active_workers = 1;
    observation.active_labour = 0;
    observation.plan_revision = 2;
    observation.last_plan_revision = Some(1);
    observation.idle_since_workers = Some(at(120));
    observation.tasks = vec![running("R1", 0, &["src/r1"])];
    let decision = evaluate(&policy(), &observation);
    assert_eq!(decision.action, ScalingAction::Hold);
    assert!(reasons(&decision).contains("scale_down_workers_waiting:0s"));
    assert_eq!(
        decision.idle_since_workers.as_deref(),
        Some(now().format(&Rfc3339).unwrap().as_str())
    );
}

#[test]
fn scale_down_never_goes_below_active_workers_or_plan_demand() {
    let mut observation = observation();
    observation.current_workers = 5;
    observation.current_labour = 1;
    observation.active_workers = 4;
    observation.active_labour = 0;
    observation.tasks = vec![
        running("R1", 0, &["src/r1"]),
        running("R2", 0, &["src/r2"]),
        running("R3", 0, &["src/r3"]),
        running("R4", 0, &["src/r4"]),
    ];
    observation.idle_since_workers = Some(at(120));
    let decision = evaluate(&policy(), &observation);
    assert_eq!(decision.action, ScalingAction::ScaleDownWorkers);
    assert_eq!(decision.target_workers, 4);
    assert!(decision.target_workers as usize >= observation.active_workers);

    let mut demand = observation.clone();
    demand.current_workers = 8;
    demand.active_workers = 0;
    demand.current_labour = 1;
    demand.tasks = vec![
        pending("T1", 0, &["src/a"]),
        pending("T2", 0, &["src/b"]),
        pending("T3", 0, &["src/c"]),
    ];
    demand.idle_since_workers = Some(at(120));
    let decision = evaluate(&policy(), &demand);
    assert_eq!(decision.action, ScalingAction::ScaleDownWorkers);
    assert_eq!(decision.target_workers, 6);
}

#[test]
fn scale_down_stops_at_policy_minimums() {
    let mut observation = observation();
    observation.current_workers = 2;
    observation.current_labour = 2;
    observation.active_workers = 0;
    observation.active_labour = 0;
    observation.tasks = vec![running("R1", 0, &["src/r1"])];
    observation.idle_since_workers = Some(at(120));
    observation.idle_since_labour = Some(at(120));
    let mut policy = policy();
    policy.min_workers = 2;
    policy.min_labour = 2;
    let decision = evaluate(&policy, &observation);
    assert_eq!(decision.action, ScalingAction::Hold);
}

#[test]
fn inputs_hash_is_stable_and_sensitive_to_observed_pressure() {
    let mut observation = observation();
    observation.current_labour = 4;
    observation.tasks = vec![
        pending("T1", 0, &["src/a"]),
        pending("T2", 0, &["src/b"]),
        pending("T3", 0, &["src/c"]),
    ];
    let first = evaluate(&policy(), &observation);
    let second = evaluate(&policy(), &observation);
    assert_eq!(first.inputs_hash, second.inputs_hash);
    assert_eq!(first.decision_key, first.inputs_hash);
    assert_eq!(first.inputs_hash.len(), 64);

    let mut changed = observation.clone();
    changed.active_workers = 1;
    changed.active_labour = 1;
    changed.current_workers = 1;
    let third = evaluate(&policy(), &changed);
    assert_ne!(first.inputs_hash, third.inputs_hash);
}

#[test]
fn manager_recommendation_is_evidence_without_a_manager_action() {
    let mut observation = observation();
    observation.current_workers = 2;
    observation.active_workers = 2;
    observation.current_labour = 2;
    observation.tasks = (0..5)
        .map(|i| pending(&format!("T{i}"), 0, &[&format!("src/{i}")]))
        .collect();
    let mut policy = policy();
    policy.max_workers = 2;
    policy.max_step = 2;
    let decision = evaluate(&policy, &observation);
    assert!(decision.manager_recommended);
    assert!(decision.manager_evidence.is_some());
    assert_eq!(decision.target_workers, 2);
    assert_ne!(decision.action, ScalingAction::ExpandWorkers);
    assert!(reasons(&decision).contains("manager_recommended"));
}

#[test]
fn scale_down_holds_when_plan_demand_is_above_current() {
    let mut observation = observation();
    observation.current_workers = 2;
    observation.current_labour = 4;
    observation.active_workers = 2;
    observation.tasks = vec![
        pending("T1", 0, &["src/a"]),
        pending("T2", 0, &["src/b"]),
        pending("T3", 0, &["src/c"]),
        pending("T4", 0, &["src/d"]),
    ];
    observation.idle_since_workers = Some(at(120));
    observation.idle_since_labour = Some(at(120));
    let decision = evaluate(&policy(), &observation);
    assert_eq!(decision.action, ScalingAction::ExpandWorkers);
    assert_eq!(decision.target_workers, 4);
}

#[test]
fn slots_reserve_release_and_apply_never_drop_active_agents() {
    let slots = ScalingSlots::new(2, 1);
    assert_eq!(slots.targets(), (2, 1));
    assert_eq!(slots.try_reserve(crate::agent::AgentRole::Worker), Some(1));
    assert_eq!(slots.try_reserve(crate::agent::AgentRole::Worker), Some(2));
    assert_eq!(slots.try_reserve(crate::agent::AgentRole::Worker), None);
    assert_eq!(slots.try_reserve(crate::agent::AgentRole::Labour), Some(1));
    assert_eq!(slots.try_reserve(crate::agent::AgentRole::Labour), None);

    let applied = slots.apply(1, 1);
    assert_eq!(applied.workers, 2);
    assert_eq!(applied.labour, 1);
    assert_eq!(slots.active(), (2, 1));
    assert!(slots.managed());

    slots.release(crate::agent::AgentRole::Worker);
    assert_eq!(slots.active(), (1, 1));
    assert_eq!(slots.available(crate::agent::AgentRole::Worker), 1);
    assert_eq!(slots.try_reserve(crate::agent::AgentRole::Worker), Some(2));
}

#[test]
fn slots_legacy_restore_does_not_override_a_managed_target() {
    let slots = ScalingSlots::new(2, 2);
    slots.restore_target_if_unmanaged(crate::agent::AgentRole::Worker, 4);
    assert_eq!(slots.targets().0, 4);
    slots.enable_managed();
    slots.restore_target_if_unmanaged(crate::agent::AgentRole::Worker, 1);
    assert_eq!(slots.targets().0, 4);
    slots.raise_target(crate::agent::AgentRole::Worker, 6);
    assert_eq!(slots.targets().0, 6);
    slots.apply(2, 2);
    assert_eq!(slots.targets(), (2, 2));
}

#[test]
fn coordinator_reuses_slot_state_per_session() {
    let coordinator = ScalingCoordinator::default();
    let first = coordinator.slots_for("ses-1", 2, 3);
    let second = coordinator.slots_for("ses-1", 9, 9);
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(second.targets(), (2, 3));
    assert_eq!(coordinator.len(), 1);
    assert_eq!(coordinator.slots_for("ses-2", 1, 1).targets(), (1, 1));
    assert_eq!(coordinator.len(), 2);
}

fn test_engine() -> (
    TempDir,
    TempDir,
    Engine,
    vp_protocol::types::Project,
    Session,
) {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let store = Store::open(data.path()).unwrap();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("scaling"))
        .unwrap();
    let session = store.create_session(&project.id, "scale a plan").unwrap();
    let engine = Engine::new(store);
    (data, repo, engine, project, session)
}

fn plan(tasks: Vec<PlanTask>) -> TeamPlan {
    TeamPlan {
        goal: "scale".into(),
        departments: vec![PlanDepartment {
            name: "impl".into(),
            manager_title: "Implementation".into(),
            profile: "coding".into(),
            objective: "build".into(),
            token_budget: None,
            cost_budget_usd: None,
        }],
        tasks,
    }
}

fn plan_task(id: &str, priority: i32, scope: &str) -> PlanTask {
    PlanTask {
        id: id.into(),
        department: "impl".into(),
        title: id.into(),
        kind: "coding".into(),
        priority,
        profile: None,
        deps: Vec::new(),
        write_scope: vec![scope.into()],
        acceptance: vec!["done".into()],
        token_budget: None,
        cost_budget_usd: None,
    }
}

fn capacity(workers: usize, labour: usize) -> CapacityInfo {
    CapacityInfo {
        worker_limit: 12,
        worker_available: workers,
        labour_limit: 24,
        labour_available: labour,
    }
}

fn decision(
    session_id: &str,
    project_id: &str,
    key: &str,
    action: ScalingAction,
) -> ScalingDecision {
    ScalingDecision {
        decision_id: String::new(),
        decision_key: key.to_string(),
        session_id: session_id.to_string(),
        project_id: project_id.to_string(),
        plan_revision: 1,
        action,
        current_workers: 2,
        current_labour: 4,
        target_workers: 4,
        target_labour: 4,
        active_workers: 2,
        active_labour: 2,
        ready_tasks: 3,
        running_tasks: 2,
        inputs_hash: "abc123".to_string(),
        reasons: vec!["expand_workers:2->4".to_string()],
        manager_recommended: false,
        manager_evidence: None,
        applied: true,
        idle_since_workers: None,
        idle_since_labour: None,
        replay_inputs: None,
        created_at: "2026-11-11T12:00:00Z".to_string(),
    }
}

#[tokio::test]
async fn policy_round_trips_and_normalizes_bounds() {
    let (_data, _repo, engine, project, _session) = test_engine();
    let initial = engine.get_scaling_policy(&project.id).await.unwrap();
    assert!(!initial.policy.enabled);
    let policy = ScalingPolicy {
        enabled: true,
        min_workers: 2,
        max_workers: 5,
        min_labour: 3,
        max_labour: 9,
        expand_ready_threshold: 3,
        expand_critical_delay_secs: 30,
        scale_down_idle_secs: 300,
        cooldown_secs: 45,
        max_step: 3,
        budget_headroom_ratio: Some(0.2),
    };
    let stored = engine
        .set_scaling_policy(&project.id, policy.clone())
        .await
        .unwrap();
    assert_eq!(stored.policy, policy);
    let read = engine.get_scaling_policy(&project.id).await.unwrap();
    assert_eq!(read.policy, policy);
    assert!(engine.store().has_scaling_policy(&project.id).unwrap());

    let invalid = ScalingPolicy {
        min_workers: 6,
        max_workers: 2,
        ..policy
    };
    let error = engine
        .set_scaling_policy(&project.id, invalid)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("SCALING_POLICY_INVALID"));
}

#[tokio::test]
async fn decision_recording_is_idempotent_and_event_backed() {
    let (_data, _repo, engine, project, session) = test_engine();
    let input = decision(
        &session.id,
        &project.id,
        "rev1:tick1",
        ScalingAction::ExpandWorkers,
    );
    let first = engine.record_scaling_decision(input.clone()).await.unwrap();
    let first = first.expect("first insert");
    assert_eq!(first.decision_key, "rev1:tick1");
    assert!(!first.decision_id.is_empty());
    let second = engine.record_scaling_decision(input).await.unwrap();
    assert!(second.is_none());
    let decisions = engine
        .list_scaling_decisions(&session.id, 10)
        .await
        .unwrap();
    assert_eq!(decisions.len(), 1);
    let events = engine
        .list_events(session.id.clone(), 0, 100)
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "scaling.decision")
            .count(),
        1
    );
    let latest = engine
        .latest_scaling_decision(&session.id)
        .await
        .unwrap()
        .expect("latest");
    assert_eq!(latest.decision_id, first.decision_id);
    assert_eq!(latest.target_workers, 4);
}

#[tokio::test]
async fn replay_and_repair_rebuild_scaling_decisions_from_events() {
    let (_data, _repo, engine, project, session) = test_engine();
    engine
        .record_scaling_decision(decision(
            &session.id,
            &project.id,
            "rev1:tick1",
            ScalingAction::ExpandWorkers,
        ))
        .await
        .unwrap();
    let report = engine.replay_session(&session.id).await.unwrap();
    assert!(report.scaling_decisions_match, "{:?}", report.mismatches);

    let store = engine.store().clone();
    let events = store.list_all_events(&session.id).unwrap();
    store
        .replace_scaling_decisions(&session.id, &[], &events)
        .unwrap();
    let broken = engine.replay_session(&session.id).await.unwrap();
    assert!(!broken.scaling_decisions_match);
    assert!(
        broken
            .mismatches
            .iter()
            .any(|mismatch| mismatch.contains("scaling decisions"))
    );
    let repaired = engine.repair_session(&session.id).await.unwrap();
    assert!(repaired.scaling_decisions_match);
    assert_eq!(
        engine
            .list_scaling_decisions(&session.id, 10)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn bundle_import_rebuilds_scaling_decisions_from_events() {
    let (data, repo, engine, project, session) = test_engine();
    engine
        .record_scaling_decision(decision(
            &session.id,
            &project.id,
            "rev1:tick1",
            ScalingAction::ExpandWorkers,
        ))
        .await
        .unwrap();
    let bundle = engine.export_session(&session.id).await.unwrap();
    assert!(
        bundle
            .events
            .iter()
            .any(|event| event.kind == "scaling.decision")
    );

    let import_data = TempDir::new().unwrap();
    let imported_engine = Engine::new(Store::open(import_data.path()).unwrap());
    imported_engine.import_session(bundle).await.unwrap();
    let decisions = imported_engine
        .list_scaling_decisions(&session.id, 10)
        .await
        .unwrap();
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].action, ScalingAction::ExpandWorkers);
    let report = imported_engine.replay_session(&session.id).await.unwrap();
    assert!(report.scaling_decisions_match, "{:?}", report.mismatches);
    drop(data);
    drop(repo);
}

#[tokio::test]
async fn enabled_policy_bounds_the_starting_slot_target() {
    let (_data, _repo, engine, project, session) = test_engine();
    let policy = ScalingPolicy {
        enabled: true,
        min_workers: 3,
        max_workers: 5,
        min_labour: 2,
        max_labour: 6,
        ..ScalingPolicy::default()
    };
    engine
        .set_scaling_policy(&project.id, policy)
        .await
        .unwrap();
    let slots = engine.scaling_slots(&session.id, &project.id, 10, 20);
    assert_eq!(slots.targets(), (5, 6));

    let other = engine
        .store()
        .create_session(&project.id, "disabled")
        .unwrap();
    engine
        .set_scaling_policy(&project.id, ScalingPolicy::default())
        .await
        .unwrap();
    let disabled = engine.scaling_slots(&other.id, &project.id, 10, 20);
    assert_eq!(disabled.targets(), (10, 20));
    assert!(!disabled.managed());
}

#[tokio::test]
async fn disabling_a_policy_releases_managed_slot_targets() {
    let (_data, _repo, engine, project, session) = test_engine();
    let enabled = ScalingPolicy {
        enabled: true,
        min_workers: 3,
        max_workers: 5,
        ..ScalingPolicy::default()
    };
    engine
        .set_scaling_policy(&project.id, enabled)
        .await
        .unwrap();
    let slots = engine.scaling_slots(&session.id, &project.id, 10, 20);
    assert!(slots.managed());
    assert_eq!(slots.targets().0, 5);
    engine
        .set_scaling_policy(&project.id, ScalingPolicy::default())
        .await
        .unwrap();
    assert!(engine.scaling_slots_existing(&session.id).is_none());
    let reset = engine.scaling_slots(&session.id, &project.id, 10, 20);
    assert!(!reset.managed());
    assert_eq!(reset.targets(), (10, 20));
}

#[tokio::test]
async fn policy_change_clamps_existing_targets_immediately() {
    let (_data, _repo, engine, project, session) = test_engine();
    engine
        .set_scaling_policy(
            &project.id,
            ScalingPolicy {
                enabled: true,
                min_workers: 3,
                max_workers: 5,
                min_labour: 2,
                max_labour: 6,
                ..ScalingPolicy::default()
            },
        )
        .await
        .unwrap();
    let slots = engine.scaling_slots(&session.id, &project.id, 10, 20);
    assert_eq!(slots.targets(), (5, 6));
    engine
        .set_scaling_policy(
            &project.id,
            ScalingPolicy {
                enabled: true,
                min_workers: 1,
                max_workers: 3,
                min_labour: 1,
                max_labour: 4,
                ..ScalingPolicy::default()
            },
        )
        .await
        .unwrap();
    let same = engine.scaling_slots(&session.id, &project.id, 10, 20);
    assert!(Arc::ptr_eq(&slots, &same));
    assert_eq!(same.targets(), (3, 4));
}

#[tokio::test]
async fn preview_does_not_persist_or_apply() {
    let (_data, _repo, engine, project, session) = test_engine();
    let policy = ScalingPolicy {
        enabled: true,
        expand_critical_delay_secs: 0,
        ..ScalingPolicy::default()
    };
    engine
        .set_scaling_policy(&project.id, policy)
        .await
        .unwrap();
    engine
        .set_team_plan(
            &session.id,
            &plan(vec![
                plan_task("T1", 0, "src/a"),
                plan_task("T2", 0, "src/b"),
                plan_task("T3", 0, "src/c"),
            ]),
        )
        .await
        .unwrap();
    engine
        .store()
        .update_session_status(&session.id, "idle")
        .unwrap();
    let preview = preview(&engine, &session.id, &capacity(10, 10), 2, 2)
        .await
        .unwrap();
    assert_eq!(
        preview.decision.action,
        ScalingAction::ExpandWorkersAndLabour
    );
    assert!(!preview.decision.applied);
    assert!(
        engine
            .list_scaling_decisions(&session.id, 10)
            .await
            .unwrap()
            .is_empty()
    );
    let slots = engine.scaling_slots(&session.id, &project.id, 2, 2);
    assert_eq!(slots.targets(), (2, 2));
    assert_eq!(slots.active(), (0, 0));
}

#[tokio::test]
async fn tick_persists_and_applies_once_per_plan_revision_and_key() {
    let (_data, _repo, engine, project, session) = test_engine();
    let policy = ScalingPolicy {
        enabled: true,
        max_workers: 6,
        max_labour: 8,
        expand_critical_delay_secs: 0,
        cooldown_secs: 0,
        max_step: 2,
        ..ScalingPolicy::default()
    };
    engine
        .set_scaling_policy(&project.id, policy)
        .await
        .unwrap();
    engine
        .set_team_plan(
            &session.id,
            &plan(vec![
                plan_task("T1", 0, "src/a"),
                plan_task("T2", 0, "src/b"),
                plan_task("T3", 0, "src/c"),
                plan_task("T4", 0, "src/d"),
            ]),
        )
        .await
        .unwrap();
    engine
        .store()
        .update_session_status(&session.id, "idle")
        .unwrap();

    let first = run_tick(
        &engine,
        &capacity(10, 10),
        2,
        2,
        Some(&project.id),
        Some("tick1".to_string()),
        true,
    )
    .await
    .unwrap();
    assert_eq!(first.evaluated, 1);
    assert_eq!(first.persisted, 1);
    assert_eq!(first.expanded, 1);
    let slots = engine.scaling_slots(&session.id, &project.id, 2, 2);
    assert_eq!(slots.targets(), (4, 4));
    assert!(slots.managed());
    let latest = engine
        .latest_scaling_decision(&session.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.action, ScalingAction::ExpandWorkersAndLabour);
    assert!(latest.applied);

    let repeat = run_tick(
        &engine,
        &capacity(10, 10),
        2,
        2,
        Some(&project.id),
        Some("tick1".to_string()),
        true,
    )
    .await
    .unwrap();
    assert_eq!(repeat.persisted, 0);
    assert_eq!(
        engine
            .list_scaling_decisions(&session.id, 10)
            .await
            .unwrap()
            .len(),
        1
    );

    let hold = run_tick(
        &engine,
        &capacity(10, 10),
        2,
        2,
        Some(&project.id),
        Some("tick2".to_string()),
        true,
    )
    .await
    .unwrap();
    assert_eq!(hold.persisted, 1);
    let duplicate_hold = run_tick(
        &engine,
        &capacity(10, 10),
        2,
        2,
        Some(&project.id),
        Some("tick3".to_string()),
        true,
    )
    .await
    .unwrap();
    let after = engine
        .list_scaling_decisions(&session.id, 10)
        .await
        .unwrap();
    assert_eq!(
        duplicate_hold.persisted,
        0,
        "{:?}",
        after
            .iter()
            .map(|d| (
                d.action.as_str(),
                d.target_workers,
                d.target_labour,
                d.idle_since_workers.clone(),
                d.idle_since_labour.clone(),
            ))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        engine
            .list_scaling_decisions(&session.id, 10)
            .await
            .unwrap()
            .len(),
        2
    );
    let report = engine.replay_session(&session.id).await.unwrap();
    assert!(report.scaling_decisions_match, "{:?}", report.mismatches);
}

#[tokio::test]
async fn tick_is_a_noop_when_policy_is_disabled() {
    let (_data, _repo, engine, project, session) = test_engine();
    engine
        .set_team_plan(&session.id, &plan(vec![plan_task("T1", 0, "src/a")]))
        .await
        .unwrap();
    let result = run_tick(
        &engine,
        &capacity(10, 10),
        2,
        2,
        Some(&project.id),
        Some("tick1".to_string()),
        true,
    )
    .await
    .unwrap();
    assert_eq!(result.evaluated, 0);
    assert_eq!(result.persisted, 0);
    assert!(
        engine
            .list_scaling_decisions(&session.id, 10)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn status_reports_policy_targets_and_latest_decision() {
    let (_data, _repo, engine, project, session) = test_engine();
    let policy = ScalingPolicy {
        enabled: true,
        max_workers: 6,
        ..ScalingPolicy::default()
    };
    engine
        .set_scaling_policy(&project.id, policy)
        .await
        .unwrap();
    engine
        .set_team_plan(&session.id, &plan(vec![plan_task("T1", 0, "src/a")]))
        .await
        .unwrap();
    engine
        .record_scaling_decision(decision(
            &session.id,
            &project.id,
            "rev1:tick1",
            ScalingAction::ExpandWorkers,
        ))
        .await
        .unwrap();
    let status = status(
        &engine,
        Some(&project.id),
        None,
        Some(10),
        &capacity(9, 20),
        2,
        4,
    )
    .unwrap();
    assert_eq!(status.global_worker_available, 9);
    assert_eq!(status.projects.len(), 1);
    let project_status = &status.projects[0];
    assert!(project_status.policy.enabled);
    assert_eq!(project_status.sessions.len(), 1);
    let session_status = &project_status.sessions[0];
    assert_eq!(session_status.current_workers, 4);
    assert_eq!(session_status.current_labour, 4);
    assert_eq!(
        session_status
            .latest
            .as_ref()
            .map(|decision| decision.action),
        Some(ScalingAction::ExpandWorkers)
    );
}

#[tokio::test]
async fn db_rebuild_restores_scaling_policy_and_decisions() {
    let (data, _repo, engine, project, session) = test_engine();
    engine
        .append_event(
            &session.id,
            "session.created",
            serde_json::json!({
                "project_id": project.id,
                "goal": "scale a plan",
                "created_at": session.created_at,
            }),
        )
        .await
        .unwrap();
    let policy = ScalingPolicy {
        enabled: true,
        max_workers: 5,
        budget_headroom_ratio: Some(0.3),
        ..ScalingPolicy::default()
    };
    engine
        .set_scaling_policy(&project.id, policy.clone())
        .await
        .unwrap();
    engine
        .record_scaling_decision(decision(
            &session.id,
            &project.id,
            "rev1:tick1",
            ScalingAction::ExpandWorkers,
        ))
        .await
        .unwrap();
    engine.record_project_manifest().await.unwrap();
    let mirror = engine
        .store()
        .read_event_mirror(&session.id)
        .unwrap()
        .unwrap();
    assert!(
        mirror.iter().any(|event| event.kind == "session.created"),
        "mirror kinds: {:?}",
        mirror
            .iter()
            .map(|event| event.kind.as_str())
            .collect::<Vec<_>>()
    );

    let report = crate::rebuild_database(data.path(), false).await.unwrap();
    assert_eq!(report.projects, 1);
    assert_eq!(report.sessions, 1, "{:?}", report.skipped);
    let store = Store::open(data.path()).unwrap();
    assert_eq!(store.get_scaling_policy(&project.id).unwrap(), policy);
    assert_eq!(
        store.list_scaling_decisions(&session.id, 10).unwrap().len(),
        1
    );
}

#[test]
fn observation_hash_is_deterministic_across_map_iteration_order() {
    let mut first = observation();
    first.current_labour = 4;
    first.tasks = vec![
        pending("b", 0, &["src/b"]),
        pending("a", 0, &["src/a"]),
        pending("c", 0, &["src/c"]),
    ];
    let mut second = first.clone();
    second
        .tasks
        .sort_by(|left, right| right.task_id.cmp(&left.task_id));
    let a = evaluate(&policy(), &first);
    let b = evaluate(&policy(), &second);
    assert_eq!(a.inputs_hash, b.inputs_hash);
}

#[test]
fn evaluate_never_returns_zero_or_above_hard_caps() {
    let mut observation = observation();
    observation.current_workers = 10;
    observation.current_labour = 40;
    observation.active_workers = 10;
    observation.active_labour = 40;
    observation.tasks = (0..40)
        .map(|i| pending(&format!("T{i}"), 0, &[&format!("src/{i}")]))
        .collect();
    let mut policy = policy();
    policy.max_workers = 10;
    policy.max_labour = 40;
    policy.max_step = 10;
    let decision = evaluate(&policy, &observation);
    assert_eq!(decision.target_workers, 10);
    assert_eq!(decision.target_labour, 40);
    assert!(decision.manager_recommended);
}

#[test]
fn analyze_plan_tasks_exposes_demand_without_allocating_agents() {
    let mut observation = observation();
    observation.tasks = vec![
        running("R1", 0, &["src/r1"]),
        pending("T1", 10, &["src/a"]),
        pending("T2", 5, &["src/b"]),
    ];
    let (demand, ready, oldest) = analyze_plan_tasks(&observation.tasks, now());
    assert_eq!(demand, 3);
    assert_eq!(ready, 2);
    assert_eq!(oldest, 120);
    let decision = evaluate(&policy(), &observation);
    assert_eq!(decision.ready_tasks, 2);
    assert_eq!(decision.running_tasks, 1);
}

#[test]
fn audit_reason_bound_is_enforced() {
    let mut observation = observation();
    observation.tasks = (0..100)
        .map(|i| {
            pending(
                &format!("task-with-a-long-id-{i}"),
                0,
                &[&format!("src/{i}")],
            )
        })
        .collect();
    // Force many reasons by using a policy that produces gate/waiting notes.
    let mut policy = policy();
    policy.max_workers = 6;
    let decision = evaluate(&policy, &observation);
    assert!(decision.reasons.len() <= MAX_REASONS);
    assert!(!decision.reasons.is_empty());
}

#[test]
fn error_catalog_has_scaling_codes() {
    assert!(ToolErrorCode::SCALING_POLICY_INVALID.is_known());
    assert!(ToolErrorCode::SCALING_STATE_CONFLICT.is_known());
    assert_eq!(
        ToolErrorCode::SCALING_POLICY_INVALID.as_str(),
        "SCALING_POLICY_INVALID"
    );
}

#[test]
fn elapsed_seconds_never_wraps_for_future_timestamps() {
    assert_eq!(elapsed_secs(now(), now() + time::Duration::seconds(10)), 0);
    assert_eq!(elapsed_secs(now(), now() - time::Duration::seconds(10)), 10);
}

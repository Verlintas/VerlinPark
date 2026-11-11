use std::collections::HashMap;

use vp_protocol::types::{Event, PlanDepartment, PlanTask, TeamPlan};

use super::*;

fn task(id: &str, priority: i32, deps: &[&str], budget: Option<u64>) -> PlanTask {
    PlanTask {
        id: id.to_string(),
        department: "impl".to_string(),
        title: id.to_string(),
        kind: "coding".to_string(),
        priority,
        profile: None,
        deps: deps.iter().map(|dep| (*dep).to_string()).collect(),
        write_scope: vec![id.to_string()],
        acceptance: vec!["done".to_string()],
        token_budget: budget,
        cost_budget_usd: None,
    }
}

fn plan(tasks: Vec<PlanTask>) -> TeamPlan {
    TeamPlan {
        goal: "governor".to_string(),
        departments: vec![PlanDepartment {
            name: "impl".to_string(),
            manager_title: "Implementation".to_string(),
            profile: "coding".to_string(),
            objective: "build".to_string(),
            token_budget: None,
            cost_budget_usd: None,
        }],
        tasks,
    }
}

fn statuses(entries: &[(&str, &str)]) -> HashMap<String, String> {
    entries
        .iter()
        .map(|(task, status)| ((*task).to_string(), (*status).to_string()))
        .collect()
}

struct Fixture {
    tasks: Vec<PlanTask>,
    enqueued: Vec<String>,
    statuses: HashMap<String, String>,
}

impl Fixture {
    fn new(tasks: Vec<PlanTask>) -> Self {
        let enqueued = (0..tasks.len())
            .map(|index| format!("2026-11-11T00:00:{index:02}Z"))
            .collect();
        Self {
            tasks,
            enqueued,
            statuses: HashMap::new(),
        }
    }

    fn with_statuses(mut self, entries: &[(&str, &str)]) -> Self {
        self.statuses = statuses(entries);
        self
    }

    fn plan(&self) -> TeamPlan {
        plan(self.tasks.clone())
    }

    fn candidates(&self, priorities: &[i64]) -> Vec<GovernorCandidate<'_>> {
        self.tasks
            .iter()
            .enumerate()
            .filter(|(_, task)| {
                !self
                    .statuses
                    .get(&task.id)
                    .map(|status| crate::critical_path::is_terminal_status(status))
                    .unwrap_or(false)
            })
            .map(|(index, task)| GovernorCandidate {
                task,
                effective_priority: priorities.get(index).copied().unwrap_or(0),
                enqueued_at: self.enqueued[index].as_str(),
            })
            .collect()
    }
}

fn policy(enabled: bool) -> CriticalPathPolicy {
    CriticalPathPolicy {
        enabled,
        ..CriticalPathPolicy::default()
    }
}

fn evaluate(
    fixture: &Fixture,
    priorities: &[i64],
    policy: &CriticalPathPolicy,
    budget: GovernorBudgetView,
    remaining_override: Option<u64>,
) -> Result<GovernorAdmission, GovernorFailure> {
    let candidates = fixture.candidates(priorities);
    let plan = fixture.plan();
    super::evaluate(GovernorEvaluation {
        session_id: "session-1",
        project_id: "project-1",
        plan_revision: 1,
        plan: &plan,
        statuses: &fixture.statuses,
        policy,
        candidates: &candidates,
        budget,
        remaining_override,
    })
}

#[test]
fn disabled_policy_is_a_no_op_and_preserves_candidate_order() {
    let fixture = Fixture::new(vec![
        task("low", -5, &[], Some(10_000)),
        task("high", 20, &[], Some(10_000)),
    ]);
    let admission = evaluate(
        &fixture,
        &[0, 0],
        &policy(false),
        GovernorBudgetView::new(Some(10_000), 9_900, 0),
        None,
    )
    .unwrap();
    assert!(!admission.enabled);
    assert_eq!(admission.reserve.state, "disabled");
    assert_eq!(admission.admitted, vec!["low", "high"]);
    assert!(admission.deferred.is_empty());
    assert!(
        admission
            .decisions
            .iter()
            .all(|decision| decision.reason == GovernorReason::Disabled)
    );
}

#[test]
fn reserve_ratio_clamps_and_boundaries_are_exact() {
    let fixture = Fixture::new(vec![
        task("a", 1, &[], Some(4_000)),
        task("side", 1, &[], Some(1_000)),
    ]);
    let mut zero = policy(true);
    zero.reserve_ratio = 0.0;
    let reserve = reserve_math(
        &zero,
        GovernorBudgetView::new(Some(10_000), 0, 0),
        Some(10_000),
        4_000,
        false,
    );
    assert_eq!(reserve.ratio_reserve_tokens, 0);
    assert_eq!(reserve.protected_tokens, 4_000);

    let mut quarter = policy(true);
    quarter.reserve_ratio = 0.25;
    let reserve = reserve_math(
        &quarter,
        GovernorBudgetView::new(Some(10_000), 0, 0),
        Some(10_000),
        4_000,
        false,
    );
    assert_eq!(reserve.ratio_reserve_tokens, 2_500);
    assert_eq!(reserve.protected_tokens, 6_500);
    assert_eq!(reserve.headroom_tokens, Some(3_500));

    let mut full = policy(true);
    full.reserve_ratio = 1.0;
    let reserve = reserve_math(
        &full,
        GovernorBudgetView::new(Some(10_000), 0, 0),
        Some(10_000),
        4_000,
        false,
    );
    assert_eq!(reserve.protected_tokens, 10_000);
    assert_eq!(reserve.headroom_tokens, Some(0));

    let mut clamped = policy(true);
    clamped.reserve_ratio = 3.0;
    let normalized = clamped.clone().normalized();
    assert_eq!(normalized.reserve_ratio, 1.0);
    clamped.reserve_ratio = f64::NAN;
    assert_eq!(clamped.normalized().reserve_ratio, 0.25);

    let admission = evaluate(
        &fixture,
        &[0, 0],
        &full,
        GovernorBudgetView::new(Some(10_000), 0, 0),
        None,
    )
    .unwrap();
    assert!(admission.deferred.iter().any(|deferral| {
        deferral.task_id == "side" && deferral.reason == GovernorReason::Reserve
    }));
}

#[test]
fn throttle_boundary_is_inclusive_and_only_with_remaining_path() {
    let mut throttle = policy(true);
    throttle.throttle_at_ratio = 0.35;
    let budget = GovernorBudgetView::new(Some(10_000), 0, 0);
    let at = reserve_math(&throttle, budget, Some(3_500), 5_000, false);
    assert!(at.throttled);
    assert_eq!(at.state, "throttled");
    let above = reserve_math(&throttle, budget, Some(3_501), 5_000, false);
    assert!(!above.throttled);
    let no_path = reserve_math(&throttle, budget, Some(100), 0, true);
    assert!(!no_path.throttled);
    assert_eq!(no_path.state, "complete");
}

#[test]
fn critical_tasks_are_admitted_under_pressure_before_non_critical_work() {
    let fixture = Fixture::new(vec![
        task("critical", 1, &[], Some(5_000)),
        task("child", 1, &["critical"], Some(5_000)),
        task("side", 100, &[], Some(2_000)),
    ]);
    let mut throttle = policy(true);
    throttle.throttle_at_ratio = 0.9;
    let admission = evaluate(
        &fixture,
        &[0, 0, 100],
        &throttle,
        GovernorBudgetView::new(Some(20_000), 5_000, 0),
        None,
    )
    .unwrap();
    assert!(admission.reserve.throttled);
    assert_eq!(admission.decisions[0].task_id, "critical");
    assert_eq!(admission.decisions[0].reason, GovernorReason::CriticalAdmit);
    assert_eq!(admission.decisions[1].task_id, "child");
    assert_eq!(admission.decisions[1].reason, GovernorReason::CriticalAdmit);
    assert!(admission.admitted.contains(&"critical".to_string()));
    assert!(admission.deferred.iter().any(|deferral| {
        deferral.task_id == "side" && deferral.reason == GovernorReason::Throttle
    }));
}

#[test]
fn non_critical_work_is_deferred_with_a_reserve_code() {
    let fixture = Fixture::new(vec![
        task("critical", 1, &[], Some(5_000)),
        task("side", 1, &[], Some(4_000)),
    ]);
    let mut reserve = policy(true);
    reserve.reserve_ratio = 0.25;
    reserve.throttle_at_ratio = 0.0;
    let admission = evaluate(
        &fixture,
        &[0, 0],
        &reserve,
        GovernorBudgetView::new(Some(10_000), 0, 0),
        None,
    )
    .unwrap();
    // protected = 5,000 + 2,500 = 7,500, headroom = 2,500. The 4,000 side task
    // would breach the pool.
    assert_eq!(
        admission.reserve.path_weight_tokens, 5_000,
        "path={:?}",
        admission.path
    );
    assert_eq!(
        admission.reserve.protected_tokens, 7_500,
        "reserve={:?}",
        admission.reserve
    );
    let side = admission
        .deferred
        .iter()
        .find(|deferral| deferral.task_id == "side")
        .unwrap();
    assert_eq!(side.reason, GovernorReason::Reserve);
    assert_eq!(side.reason.code(), "BUDGET_RESERVED");
    // The critical task was admitted first, so the remaining view has already
    // dropped by its declared weight.
    assert_eq!(side.remaining_tokens, Some(5_000));
}

#[test]
fn reserve_never_exceeds_remaining_budget() {
    let fixture = Fixture::new(vec![task("critical", 1, &[], Some(50_000))]);
    let admission = evaluate(
        &fixture,
        &[0],
        &policy(true),
        GovernorBudgetView::new(Some(10_000), 0, 0),
        None,
    )
    .unwrap();
    assert_eq!(admission.reserve.protected_tokens, 10_000);
    assert_eq!(admission.reserve.headroom_tokens, Some(0));
}

#[test]
fn min_task_budget_exhausts_even_a_critical_task() {
    let fixture = Fixture::new(vec![task("critical", 1, &[], Some(5_000))]);
    let mut exhaust = policy(true);
    exhaust.min_task_budget = 2_000;
    let admission = evaluate(
        &fixture,
        &[0],
        &exhaust,
        GovernorBudgetView::new(Some(10_000), 9_000, 0),
        None,
    )
    .unwrap();
    assert_eq!(admission.deferred.len(), 1);
    assert_eq!(admission.deferred[0].reason, GovernorReason::Exhausted);
    assert_eq!(admission.deferred[0].reason.code(), "BUDGET_EXHAUSTED");
    assert_eq!(admission.reserve.state, "exhausted");
}

#[test]
fn completed_path_admits_remaining_work_without_a_ratio_reserve() {
    let fixture = Fixture::new(vec![
        task("done", 1, &[], Some(90_000)),
        task("tail", 1, &["done"], Some(5_000)),
    ])
    .with_statuses(&[("done", "completed")]);
    let mut reserve = policy(true);
    reserve.reserve_ratio = 1.0;
    let admission = evaluate(
        &fixture,
        &[0, 0],
        &reserve,
        GovernorBudgetView::new(Some(20_000), 19_000, 0),
        None,
    )
    .unwrap();
    // The done task is not a candidate; tail is the only remaining path and is
    // critical, so it is admitted with critical_admit.
    assert_eq!(admission.path.task_ids, vec!["tail"]);
    assert_eq!(admission.decisions.len(), 1);
    assert!(admission.decisions[0].admitted);
}

#[test]
fn unbounded_budget_admits_everything_without_reserve_math() {
    let fixture = Fixture::new(vec![task("a", 1, &[], Some(5_000))]);
    let admission = evaluate(
        &fixture,
        &[0],
        &policy(true),
        GovernorBudgetView::new(None, 0, 0),
        None,
    )
    .unwrap();
    assert_eq!(admission.reserve.state, "no_budget");
    assert_eq!(admission.decisions[0].reason, GovernorReason::NoBudget);
    assert!(admission.deferred.is_empty());
}

#[test]
fn critical_boost_and_priority_are_deterministic_ties() {
    let fixture = Fixture::new(vec![
        task("a", 1, &[], Some(1_000)),
        task("b", 1, &[], Some(1_000)),
        task("c", 1, &[], Some(1_000)),
    ]);
    let mut reserve = policy(true);
    reserve.critical_boost = 50;
    // All are independent, so only one is ever on the path. The path is the
    // lexicographically first tied task, `a`, and its boost must sort first.
    let admission = evaluate(
        &fixture,
        &[10, 20, 30],
        &reserve,
        GovernorBudgetView::new(Some(10_000), 0, 0),
        None,
    )
    .unwrap();
    assert_eq!(admission.decisions[0].task_id, "a");
    assert!(admission.decisions[0].critical);
}

#[test]
fn release_reconsiders_deferred_tasks_deterministically() {
    let fixture = Fixture::new(vec![
        task("critical", 1, &[], Some(5_000)),
        task("side", 1, &[], Some(4_000)),
    ]);
    let mut reserve = policy(true);
    reserve.throttle_at_ratio = 0.0;
    let first = evaluate(
        &fixture,
        &[0, 0],
        &reserve,
        GovernorBudgetView::new(Some(10_000), 0, 0),
        None,
    )
    .unwrap();
    assert!(
        first
            .deferred
            .iter()
            .any(|deferral| deferral.task_id == "side")
    );
    let completed = Fixture::new(vec![
        task("critical", 1, &[], Some(5_000)),
        task("side", 1, &[], Some(4_000)),
    ])
    .with_statuses(&[("critical", "completed")]);
    let second = evaluate(
        &completed,
        &[0, 0],
        &reserve,
        GovernorBudgetView::new(Some(10_000), 5_000, 0),
        None,
    )
    .unwrap();
    assert_eq!(second.path.task_ids, vec!["side"]);
    assert_eq!(second.admitted, vec!["side"]);
    // The same release evaluated twice produces the same decision.
    let third = evaluate(
        &completed,
        &[0, 0],
        &reserve,
        GovernorBudgetView::new(Some(10_000), 5_000, 0),
        None,
    )
    .unwrap();
    assert_eq!(second.inputs_hash, third.inputs_hash);
    assert_eq!(second.decisions, third.decisions);
}

#[test]
fn declared_task_budgets_drive_admission_not_declared_priority() {
    let fixture = Fixture::new(vec![
        task("critical", 1, &[], Some(5_000)),
        task("cheap", 1, &[], Some(500)),
        task("expensive", 1, &[], Some(5_000)),
    ]);
    let mut reserve = policy(true);
    reserve.reserve_ratio = 0.0;
    reserve.throttle_at_ratio = 0.0;
    let admission = evaluate(
        &fixture,
        &[0, 0, 0],
        &reserve,
        GovernorBudgetView::new(Some(9_000), 0, 0),
        None,
    )
    .unwrap();
    assert!(
        admission.admitted.contains(&"cheap".to_string()),
        "{:?}",
        admission.decisions
    );
    assert!(
        admission.deferred.iter().any(|deferral| {
            deferral.task_id == "expensive" && deferral.reason == GovernorReason::Reserve
        }),
        "decisions={:?}",
        admission.decisions
    );
}

#[test]
fn deferrals_keep_the_deterministic_priority_order() {
    let fixture = Fixture::new(vec![
        task("critical", 1, &[], Some(5_000)),
        task("high-side", 50, &[], Some(3_000)),
        task("low-side", 10, &[], Some(3_000)),
    ]);
    let mut reserve = policy(true);
    reserve.reserve_ratio = 0.0;
    reserve.throttle_at_ratio = 0.0;
    let admission = evaluate(
        &fixture,
        &[0, 50, 10],
        &reserve,
        GovernorBudgetView::new(Some(10_000), 0, 0),
        None,
    )
    .unwrap();
    // Path weight 5,000 leaves 5,000 free. The higher-priority side task is
    // considered first and consumes the free headroom; the lower-priority one
    // is deferred. The order is stable across repeated evaluations.
    assert_eq!(
        admission
            .decisions
            .iter()
            .map(|decision| decision.task_id.as_str())
            .collect::<Vec<_>>(),
        vec!["high-side", "critical", "low-side"]
    );
    assert!(admission.admitted.contains(&"high-side".to_string()));
    let low = admission
        .deferred
        .iter()
        .find(|deferral| deferral.task_id == "low-side")
        .unwrap();
    assert_eq!(low.reason, GovernorReason::Reserve);
    let repeat = evaluate(
        &fixture,
        &[0, 50, 10],
        &reserve,
        GovernorBudgetView::new(Some(10_000), 0, 0),
        None,
    )
    .unwrap();
    assert_eq!(admission.decisions, repeat.decisions);
    assert_eq!(admission.inputs_hash, repeat.inputs_hash);
}

#[test]
fn plan_revision_change_resets_the_path_identity() {
    let fixture = Fixture::new(vec![
        task("a", 1, &[], Some(5_000)),
        task("b", 1, &["a"], Some(5_000)),
    ]);
    let plan = fixture.plan();
    let candidates = fixture.candidates(&[0, 0]);
    let first = super::evaluate(GovernorEvaluation {
        session_id: "session-1",
        project_id: "project-1",
        plan_revision: 1,
        plan: &plan,
        statuses: &fixture.statuses,
        policy: &policy(true),
        candidates: &candidates,
        budget: GovernorBudgetView::new(Some(20_000), 0, 0),
        remaining_override: None,
    })
    .unwrap();
    let second = super::evaluate(GovernorEvaluation {
        session_id: "session-1",
        project_id: "project-1",
        plan_revision: 2,
        plan: &plan,
        statuses: &fixture.statuses,
        policy: &policy(true),
        candidates: &candidates,
        budget: GovernorBudgetView::new(Some(20_000), 0, 0),
        remaining_override: None,
    })
    .unwrap();
    assert_ne!(first.inputs_hash, second.inputs_hash);
    assert_eq!(first.path.plan_revision, 1);
    assert_eq!(second.path.plan_revision, 2);
    assert_eq!(first.path.task_ids, second.path.task_ids);
}

#[test]
fn malformed_decision_events_are_skipped_without_failing_history() {
    let events = vec![
        Event {
            session_id: "s1".to_string(),
            seq: 1,
            kind: DECISION_EVENT_KIND.to_string(),
            payload: serde_json::json!({"not": "a decision"}),
            created_at: "2026-11-11T00:00:00Z".to_string(),
        },
        Event {
            session_id: "s1".to_string(),
            seq: 2,
            kind: "unrelated.event".to_string(),
            payload: serde_json::json!({}),
            created_at: "2026-11-11T00:00:00Z".to_string(),
        },
        Event {
            session_id: "s1".to_string(),
            seq: 3,
            kind: DECISION_EVENT_KIND.to_string(),
            payload: serde_json::json!({
                "decision_id": "gov-3",
                "action": "defer",
                "reason": "throttle",
                "code": "BUDGET_THROTTLED"
            }),
            created_at: "2026-11-11T00:00:01Z".to_string(),
        },
    ];
    let decisions = decisions_from_events(&events, 100);
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].created_at, "2026-11-11T00:00:01Z");
    assert_eq!(decisions[0].code, "BUDGET_THROTTLED");
}

#[test]
fn admission_payload_bounds_long_paths_and_deferred_lists() {
    let fixture = Fixture::new(
        (0..MAX_PREVIEW_DECISIONS)
            .map(|index| {
                task(
                    &format!("task-{index:03}"),
                    1,
                    &[],
                    Some(1_000 + index as u64),
                )
            })
            .collect(),
    );
    let priorities = vec![0; MAX_PREVIEW_DECISIONS];
    let admission = evaluate(
        &fixture,
        &priorities,
        &policy(true),
        GovernorBudgetView::new(Some(10_000), 0, 0),
        None,
    )
    .unwrap();
    let payload = admission_payload("session-1", "gov-1", &admission, "2026-11-11T00:00:00Z");
    assert!(payload["path"].as_array().unwrap().len() <= 100);
    assert!(payload["admitted"].as_array().unwrap().len() <= 100);
    assert!(payload["deferred"].as_array().unwrap().len() <= 100);
    assert!(payload["detail"].as_str().unwrap().chars().count() <= 1_000);
}

#[test]
fn zero_boost_and_zero_throttle_are_explicit_no_ops() {
    let fixture = Fixture::new(vec![
        task("critical", 1, &[], Some(5_000)),
        task("side", 100, &[], Some(5_000)),
    ]);
    let mut relaxed = policy(true);
    relaxed.critical_boost = 0;
    relaxed.throttle_at_ratio = 0.0;
    relaxed.reserve_ratio = 0.0;
    let admission = evaluate(
        &fixture,
        &[0, 100],
        &relaxed,
        GovernorBudgetView::new(Some(20_000), 0, 0),
        None,
    )
    .unwrap();
    assert!(!admission.reserve.throttled);
    assert_eq!(admission.reserve.protected_tokens, 5_000);
    // Without the boost the high-priority side task sorts first; both fit.
    assert_eq!(admission.decisions[0].task_id, "side");
    assert!(admission.admitted.contains(&"critical".to_string()));
    assert!(admission.deferred.is_empty());
}

#[test]
fn throttle_ratio_one_throttles_while_the_path_remains() {
    let fixture = Fixture::new(vec![
        task("critical", 1, &[], Some(5_000)),
        task("side", 1, &[], Some(1_000)),
    ]);
    let mut harsh = policy(true);
    harsh.throttle_at_ratio = 1.0;
    let admission = evaluate(
        &fixture,
        &[0, 0],
        &harsh,
        GovernorBudgetView::new(Some(20_000), 0, 0),
        None,
    )
    .unwrap();
    assert!(admission.reserve.throttled);
    assert_eq!(admission.reserve.state, "throttled");
    assert!(admission.admitted.contains(&"critical".to_string()));
    assert!(
        admission
            .deferred
            .iter()
            .any(|deferral| deferral.task_id == "side"
                && deferral.reason == GovernorReason::Throttle)
    );
}

#[test]
fn min_task_budget_zero_admits_with_a_tiny_remainder() {
    let fixture = Fixture::new(vec![task("critical", 1, &[], Some(5_000))]);
    let mut floor = policy(true);
    floor.min_task_budget = 0;
    let admission = evaluate(
        &fixture,
        &[0],
        &floor,
        GovernorBudgetView::new(Some(10_000), 9_999, 0),
        None,
    )
    .unwrap();
    assert_eq!(admission.decisions[0].reason, GovernorReason::CriticalAdmit);
    assert_eq!(admission.decisions[0].remaining_after_tokens, Some(0));
}

#[test]
fn plan_cycles_and_unknown_dependencies_fail_with_typed_codes() {
    let cycle = Fixture::new(vec![task("a", 1, &["b"], None), task("b", 1, &["a"], None)]);
    let error = evaluate(
        &cycle,
        &[0, 0],
        &policy(true),
        GovernorBudgetView::new(Some(10_000), 0, 0),
        None,
    )
    .unwrap_err();
    assert_eq!(error.code(), "BUDGET_PATH_CYCLE");
    assert!(!error.retryable);

    let unknown = Fixture::new(vec![task("a", 1, &["missing"], None)]);
    let error = evaluate(
        &unknown,
        &[0],
        &policy(true),
        GovernorBudgetView::new(Some(10_000), 0, 0),
        None,
    )
    .unwrap_err();
    assert_eq!(error.code(), "BUDGET_PATH_UNKNOWN_TASK");
}

#[test]
fn preview_override_and_inputs_hash_are_deterministic() {
    let fixture = Fixture::new(vec![task("a", 1, &[], Some(5_000))]);
    let first = evaluate(
        &fixture,
        &[0],
        &policy(true),
        GovernorBudgetView::new(Some(10_000), 0, 0),
        Some(2_000),
    )
    .unwrap();
    let second = evaluate(
        &fixture,
        &[0],
        &policy(true),
        GovernorBudgetView::new(Some(10_000), 0, 0),
        Some(2_000),
    )
    .unwrap();
    assert_eq!(first.inputs_hash, second.inputs_hash);
    assert_eq!(first.decisions, second.decisions);
    assert_eq!(first.reserve.remaining_tokens, Some(2_000));
    let third = evaluate(
        &fixture,
        &[0],
        &policy(true),
        GovernorBudgetView::new(Some(10_000), 0, 0),
        Some(3_000),
    )
    .unwrap();
    assert_ne!(first.inputs_hash, third.inputs_hash);
}

#[test]
fn policy_changes_move_the_admission_hash() {
    let fixture = Fixture::new(vec![task("a", 1, &[], Some(5_000))]);
    let first = evaluate(
        &fixture,
        &[0],
        &policy(true),
        GovernorBudgetView::new(Some(10_000), 0, 0),
        None,
    )
    .unwrap();
    let mut changed = policy(true);
    changed.critical_boost = 0;
    let second = evaluate(
        &fixture,
        &[0],
        &changed,
        GovernorBudgetView::new(Some(10_000), 0, 0),
        None,
    )
    .unwrap();
    assert_ne!(first.inputs_hash, second.inputs_hash);
}

#[test]
fn decisions_cap_bounded_history_and_parse_events() {
    let fixture = Fixture::new(vec![task("a", 1, &[], Some(5_000))]);
    let admission = evaluate(
        &fixture,
        &[0],
        &policy(true),
        GovernorBudgetView::new(Some(10_000), 0, 0),
        None,
    )
    .unwrap();
    let payload = admission_payload("session-1", "gov-1", &admission, "2026-11-11T00:00:00Z");
    let events = (0..150)
        .map(|index| Event {
            session_id: "session-1".to_string(),
            seq: index as u64 + 1,
            kind: DECISION_EVENT_KIND.to_string(),
            payload: payload.clone(),
            created_at: "2026-11-11T00:00:00Z".to_string(),
        })
        .collect::<Vec<_>>();
    let decisions = decisions_from_events(&events, 5);
    assert_eq!(decisions.len(), 5);
    assert_eq!(decisions[0].session_id, "session-1");
    assert_eq!(decisions[0].action, "admission");
    assert!(decisions[0].path.contains(&"a".to_string()));
    assert_eq!(decisions[0].inputs_hash, admission.inputs_hash);
}

#[test]
fn denial_payload_carries_the_typed_reason_and_bounds() {
    let path = vec!["a".to_string(), "b".to_string()];
    let payload = denial_payload(GovernorDenial {
        session_id: "session-1",
        decision_id: "gov-2",
        plan_revision: 3,
        task_id: "side",
        task_critical: false,
        reason: GovernorReason::Reserve,
        requested_tokens: 1_234,
        limit_tokens: Some(10_000),
        spent_tokens: 5_000,
        reserved_tokens: 100,
        remaining_tokens: Some(4_900),
        protected_tokens: 4_000,
        path: &path,
        path_weight_tokens: 3_500,
        inputs_hash: "hash",
        detail: "x".repeat(2_000),
        created_at: "2026-11-11T00:00:00Z",
    });
    assert_eq!(payload["action"], "defer");
    assert_eq!(payload["reason"], "reserve");
    assert_eq!(payload["code"], "BUDGET_RESERVED");
    assert_eq!(payload["requested_tokens"], 1_234);
    assert_eq!(payload["protected_tokens"], 4_000);
    assert!(payload["detail"].as_str().unwrap().chars().count() <= 1_000);
}

#[test]
fn run_context_gate_is_absent_for_disabled_or_completed_paths() {
    let mut context = GovernorRunContext {
        enabled: true,
        policy: policy(true),
        task_id: Some("side".to_string()),
        critical: false,
        path_task_ids: vec!["critical".to_string()],
        path_weight_tokens: 5_000,
        plan_revision: 1,
        inputs_hash: "hash".to_string(),
    };
    let gate = context.gate(Some(10_000), Some(10_000)).unwrap();
    assert!(!gate.critical);
    assert_eq!(gate.protected_tokens, 7_500);
    context.path_weight_tokens = 0;
    assert!(context.gate(Some(10_000), Some(10_000)).is_none());
    context.enabled = false;
    assert!(context.gate(Some(10_000), Some(10_000)).is_none());
    let mut leader = context.clone();
    leader.enabled = true;
    leader.path_weight_tokens = 5_000;
    leader.task_id = None;
    assert!(leader.gate(Some(10_000), Some(10_000)).is_none());
}

#[test]
fn critical_run_context_gate_marks_the_reservation_as_critical() {
    let context = GovernorRunContext {
        enabled: true,
        policy: policy(true),
        task_id: Some("critical".to_string()),
        critical: true,
        path_task_ids: vec!["critical".to_string()],
        path_weight_tokens: 5_000,
        plan_revision: 1,
        inputs_hash: "hash".to_string(),
    };
    let gate = context.gate(Some(10_000), Some(10_000)).unwrap();
    assert!(gate.critical);
    assert_eq!(gate.task_id.as_deref(), Some("critical"));
}

#[test]
fn admission_detail_is_bounded_and_explainable() {
    let fixture = Fixture::new(vec![
        task("a", 1, &[], Some(5_000)),
        task("b", 1, &["a"], Some(5_000)),
    ]);
    let admission = evaluate(
        &fixture,
        &[0, 0],
        &policy(true),
        GovernorBudgetView::new(Some(20_000), 2_000, 100),
        None,
    )
    .unwrap();
    let detail = admission.detail();
    assert!(detail.contains("a -> b"));
    assert!(detail.contains("protected"));
    assert!(detail.chars().count() < 500);
    assert_eq!(admission.reserved_tokens(), 100);
}

impl GovernorAdmission {
    fn reserved_tokens(&self) -> u64 {
        self.reserve.reserved_tokens
    }
}

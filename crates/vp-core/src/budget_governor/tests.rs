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
        observed_tokens: &HashMap::new(),
        evaluated_at: "2026-11-12T00:00:00Z",
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
        observed_tokens: &HashMap::new(),
        evaluated_at: "2026-11-12T00:00:00Z",
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
        observed_tokens: &HashMap::new(),
        evaluated_at: "2026-11-12T00:00:00Z",
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
        mode: CriticalPathMode::Single,
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
        path_count: 1,
        path_allocation_tokens: None,
        path_deficit_tokens: 0,
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
        path_count: 1,
        path_allocation_tokens: None,
        path_deficit_tokens: 0,
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

fn multi_policy() -> CriticalPathPolicy {
    CriticalPathPolicy {
        enabled: true,
        reserve_ratio: 0.25,
        throttle_at_ratio: 0.0,
        critical_boost: 0,
        min_task_budget: 100,
        mode: CriticalPathMode::Multi,
        max_paths: 4,
        ..CriticalPathPolicy::default()
    }
}

fn evaluate_multi(
    fixture: &Fixture,
    priorities: &[i64],
    policy: &CriticalPathPolicy,
    budget: GovernorBudgetView,
    observed: &HashMap<String, u64>,
    evaluated_at: &str,
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
        remaining_override: None,
        observed_tokens: observed,
        evaluated_at,
    })
}

#[test]
fn multi_mode_builds_two_independent_paths_with_union_membership() {
    let fixture = Fixture::new(vec![
        task("a1", 1, &[], Some(5_000)),
        task("a2", 1, &["a1"], Some(5_000)),
        task("b1", 1, &[], Some(3_000)),
        task("b2", 1, &["b1"], Some(3_000)),
    ]);
    let admission = evaluate_multi(
        &fixture,
        &[0, 0, 0, 0],
        &multi_policy(),
        GovernorBudgetView::new(Some(30_000), 0, 0),
        &HashMap::new(),
        "2026-11-11T00:00:03Z",
    )
    .unwrap();
    assert!(admission.enabled);
    assert_eq!(admission.mode, CriticalPathMode::Multi);
    let multi = admission.multi.as_ref().unwrap();
    assert_eq!(admission.path.task_ids, vec!["a1", "a2"]);
    assert_eq!(multi.paths.len(), 2);
    assert_eq!(multi.paths[0].task_ids, vec!["a1", "a2"]);
    assert_eq!(multi.paths[1].task_ids, vec!["b1", "b2"]);
    assert_eq!(multi.path_weight_tokens, 16_000);
    assert_eq!(multi.allocated_tokens, 16_000);
    assert_eq!(multi.ratio_reserve_tokens, 7_500);
    assert_eq!(multi.protected_tokens, 23_500);
    assert_eq!(admission.reserve.mode, CriticalPathMode::Multi);
    assert_eq!(admission.reserve.path_count, 2);
    assert_eq!(admission.reserve.allocated_tokens, 16_000);
    assert_eq!(admission.path_infos().len(), 2);
    assert!(admission.decisions.iter().all(|decision| decision.admitted));
    assert!(
        admission.decisions.iter().all(|decision| {
            decision.reason == GovernorReason::CriticalAdmit && decision.critical
        })
    );
    // Both independent chains are critical, so neither can starve the other.
    assert_eq!(admission.admitted.len(), 4);
    assert!(admission.detail().contains("2 paths"));
}

#[test]
fn multi_mode_keeps_a_third_path_non_critical_and_deferred() {
    let fixture = Fixture::new(vec![
        task("heavy", 1, &[], Some(10_000)),
        task("medium", 1, &[], Some(8_000)),
        task("side", 1, &[], Some(1_000)),
    ]);
    let mut policy = multi_policy();
    policy.max_paths = 2;
    let admission = evaluate_multi(
        &fixture,
        &[0, 0, 0],
        &policy,
        GovernorBudgetView::new(Some(20_000), 0, 0),
        &HashMap::new(),
        "2026-11-11T00:00:03Z",
    )
    .unwrap();
    let multi = admission.multi.as_ref().unwrap();
    assert_eq!(multi.paths.len(), 2);
    assert_eq!(multi.paths[0].task_ids, vec!["heavy"]);
    assert_eq!(multi.paths[1].task_ids, vec!["medium"]);
    let side = admission
        .decisions
        .iter()
        .find(|decision| decision.task_id == "side")
        .unwrap();
    assert!(!side.critical);
    assert!(!side.admitted);
    assert_eq!(side.reason, GovernorReason::Reserve);
    assert_eq!(side.path_id, None);
    assert_eq!(side.path_deficit_tokens, 0);
    let deferral = admission
        .deferred
        .iter()
        .find(|deferral| deferral.task_id == "side")
        .unwrap();
    assert_eq!(deferral.reason.code(), "BUDGET_RESERVED");
    // The first two admissions consume the selected path demand and the
    // ratio buffer protects the rest, leaving no free headroom.
    assert_eq!(deferral.protected_tokens, 20_000);
}

#[test]
fn multi_mode_reports_path_deficit_on_critical_decisions() {
    let fixture = Fixture::new(vec![
        task("a", 1, &[], Some(10_000)),
        task("b", 1, &[], Some(10_000)),
    ]);
    let mut policy = multi_policy();
    policy.reserve_ratio = 0.0;
    policy.min_task_budget = 0;
    let admission = evaluate_multi(
        &fixture,
        &[0, 0],
        &policy,
        GovernorBudgetView::new(Some(10_000), 0, 0),
        &HashMap::new(),
        "2026-11-11T00:00:03Z",
    )
    .unwrap();
    let multi = admission.multi.as_ref().unwrap();
    assert_eq!(multi.allocated_tokens, 10_000);
    assert!(
        multi
            .paths
            .iter()
            .all(|path| path.allocation_tokens == 5_000)
    );
    assert!(multi.paths.iter().all(|path| path.deficit_tokens == 5_000));
    for decision in &admission.decisions {
        assert!(decision.critical);
        assert!(decision.path_id.is_some());
        assert_eq!(decision.path_deficit_tokens, 5_000);
        assert_eq!(decision.reason, GovernorReason::CriticalAdmit);
    }
    assert!(admission.deferred.is_empty());
}

#[test]
fn single_mode_is_unchanged_and_reports_one_compatibility_row() {
    let fixture = Fixture::new(vec![
        task("critical", 1, &[], Some(5_000)),
        task("side", 1, &[], Some(4_000)),
    ]);
    let single = evaluate(
        &fixture,
        &[0, 0],
        &policy(true),
        GovernorBudgetView::new(Some(10_000), 0, 0),
        None,
    )
    .unwrap();
    assert_eq!(single.mode, CriticalPathMode::Single);
    assert!(single.multi.is_none());
    assert_eq!(single.reserve.mode, CriticalPathMode::Single);
    assert_eq!(single.reserve.path_count, 1);
    assert_eq!(single.reserve.allocated_tokens, 5_000);
    assert_eq!(single.reserve.floor_tokens, 0);
    let rows = single.path_infos();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].task_ids, single.path.task_ids);
    assert_eq!(rows[0].allocation_tokens, 5_000);
    assert!(
        single
            .decisions
            .iter()
            .all(|decision| decision.path_deficit_tokens == 0)
    );
    let critical = single
        .decisions
        .iter()
        .find(|decision| decision.task_id == "critical")
        .unwrap();
    assert_eq!(critical.path_id, Some(0));
    assert_eq!(critical.path_ids, vec![0]);
    let side = single
        .decisions
        .iter()
        .find(|decision| decision.task_id == "side")
        .unwrap();
    assert_eq!(side.path_id, None);
    assert!(side.path_ids.is_empty());
    assert!(
        single
            .decisions
            .iter()
            .all(|decision| decision.starvation_credit == 0)
    );
}

#[test]
fn observed_spend_can_reorder_paths_without_changing_hard_budgets() {
    let fixture = Fixture::new(vec![
        task("declared-heavy", 1, &[], Some(5_000)),
        task("observed-heavy", 1, &[], Some(4_000)),
    ]);
    let mut policy = multi_policy();
    policy.observed_weight_ratio = 1.0;
    let mut observed = HashMap::new();
    observed.insert("observed-heavy".to_string(), 8_000u64);
    let admission = evaluate_multi(
        &fixture,
        &[0, 0],
        &policy,
        GovernorBudgetView::new(Some(30_000), 0, 0),
        &observed,
        "2026-11-11T00:00:03Z",
    )
    .unwrap();
    let multi = admission.multi.as_ref().unwrap();
    // Observed spend is clamped to 2x declared: 4,000 becomes 8,000, which now
    // outranks the 5,000 declared path.
    assert_eq!(multi.paths[0].task_ids, vec!["observed-heavy"]);
    assert_eq!(multi.paths[0].weight_tokens, 8_000);
    assert_eq!(multi.paths[0].declared_weight_tokens, 4_000);
    assert_eq!(multi.paths[0].observed_tokens, 8_000);
    assert_eq!(multi.paths[1].task_ids, vec!["declared-heavy"]);
    assert_eq!(multi.paths[1].weight_tokens, 5_000);
    // Hard session budget is untouched by observed blending.
    assert_eq!(admission.reserve.limit_tokens, Some(30_000));
    assert_eq!(admission.reserve.spent_tokens, 0);
}

#[test]
fn observed_spend_moves_the_input_hash() {
    let fixture = Fixture::new(vec![
        task("a", 1, &[], Some(5_000)),
        task("b", 1, &[], Some(5_000)),
    ]);
    let mut policy = multi_policy();
    policy.observed_weight_ratio = 0.5;
    let first = evaluate_multi(
        &fixture,
        &[0, 0],
        &policy,
        GovernorBudgetView::new(Some(30_000), 0, 0),
        &HashMap::new(),
        "2026-11-11T00:00:03Z",
    )
    .unwrap();
    let mut observed = HashMap::new();
    observed.insert("b".to_string(), 9_000u64);
    let second = evaluate_multi(
        &fixture,
        &[0, 0],
        &policy,
        GovernorBudgetView::new(Some(30_000), 0, 0),
        &observed,
        "2026-11-11T00:00:03Z",
    )
    .unwrap();
    assert_ne!(first.inputs_hash, second.inputs_hash);
    assert_eq!(first.reserve.limit_tokens, second.reserve.limit_tokens);
}

#[test]
fn starvation_credit_reorders_deferred_side_work_after_the_threshold() {
    let tasks = vec![
        task("a", 1, &[], Some(5_000)),
        task("b", 1, &[], Some(5_000)),
        task("side", 1, &[], Some(1_000)),
    ];
    let plan = plan(tasks.clone());
    let mut policy = multi_policy();
    policy.max_paths = 2;
    policy.starvation_after_secs = 900;
    policy.critical_boost = 0;
    // `side` has a lower effective priority and has waited one hour; the two
    // protected paths were enqueued five minutes ago and receive no credit.
    let candidates = vec![
        GovernorCandidate {
            task: &tasks[0],
            effective_priority: 0,
            enqueued_at: "2026-11-11T00:55:00Z",
        },
        GovernorCandidate {
            task: &tasks[1],
            effective_priority: 0,
            enqueued_at: "2026-11-11T00:55:00Z",
        },
        GovernorCandidate {
            task: &tasks[2],
            effective_priority: -10,
            enqueued_at: "2026-11-11T00:00:00Z",
        },
    ];
    let aged = super::evaluate(GovernorEvaluation {
        session_id: "session-1",
        project_id: "project-1",
        plan_revision: 1,
        plan: &plan,
        statuses: &HashMap::new(),
        policy: &policy,
        candidates: &candidates,
        budget: GovernorBudgetView::new(Some(30_000), 0, 0),
        remaining_override: None,
        observed_tokens: &HashMap::new(),
        evaluated_at: "2026-11-11T01:00:00Z",
    })
    .unwrap();
    let side = aged
        .decisions
        .iter()
        .find(|decision| decision.task_id == "side")
        .unwrap();
    // Four full 900-second intervals grant 20 points, enough to lift -10
    // above the two zero-priority protected tasks.
    assert_eq!(side.starvation_credit, 20);
    assert_eq!(aged.decisions[0].task_id, "side");
    assert!(side.admitted);

    policy.starvation_after_secs = 0;
    let fresh = super::evaluate(GovernorEvaluation {
        session_id: "session-1",
        project_id: "project-1",
        plan_revision: 1,
        plan: &plan,
        statuses: &HashMap::new(),
        policy: &policy,
        candidates: &candidates,
        budget: GovernorBudgetView::new(Some(30_000), 0, 0),
        remaining_override: None,
        observed_tokens: &HashMap::new(),
        evaluated_at: "2026-11-11T01:00:00Z",
    })
    .unwrap();
    assert_ne!(fresh.decisions[0].task_id, "side");
    let side = fresh
        .decisions
        .iter()
        .find(|decision| decision.task_id == "side")
        .unwrap();
    assert_eq!(side.starvation_credit, 0);
}

#[test]
fn multi_mode_plan_revision_resets_the_table_and_hash() {
    let fixture = Fixture::new(vec![
        task("a", 1, &[], Some(5_000)),
        task("b", 1, &[], Some(5_000)),
    ]);
    let plan = fixture.plan();
    let candidates = fixture.candidates(&[0, 0]);
    let first = super::evaluate(GovernorEvaluation {
        session_id: "session-1",
        project_id: "project-1",
        plan_revision: 1,
        plan: &plan,
        statuses: &fixture.statuses,
        policy: &multi_policy(),
        candidates: &candidates,
        budget: GovernorBudgetView::new(Some(20_000), 0, 0),
        remaining_override: None,
        observed_tokens: &HashMap::new(),
        evaluated_at: "2026-11-11T00:00:03Z",
    })
    .unwrap();
    let second = super::evaluate(GovernorEvaluation {
        session_id: "session-1",
        project_id: "project-1",
        plan_revision: 2,
        plan: &plan,
        statuses: &fixture.statuses,
        policy: &multi_policy(),
        candidates: &candidates,
        budget: GovernorBudgetView::new(Some(20_000), 0, 0),
        remaining_override: None,
        observed_tokens: &HashMap::new(),
        evaluated_at: "2026-11-11T00:00:03Z",
    })
    .unwrap();
    assert_ne!(first.inputs_hash, second.inputs_hash);
    assert_eq!(first.reserve.path_count, second.reserve.path_count);
}

#[test]
fn multi_mode_completed_plan_releases_the_reserve() {
    let fixture = Fixture::new(vec![
        task("done", 1, &[], Some(90_000)),
        task("tail", 1, &["done"], Some(5_000)),
    ])
    .with_statuses(&[("done", "completed")]);
    let mut policy = multi_policy();
    policy.reserve_ratio = 1.0;
    let admission = evaluate_multi(
        &fixture,
        &[0, 0],
        &policy,
        GovernorBudgetView::new(Some(20_000), 19_000, 0),
        &HashMap::new(),
        "2026-11-11T00:00:03Z",
    )
    .unwrap();
    assert_eq!(admission.multi.as_ref().unwrap().paths.len(), 1);
    assert_eq!(admission.decisions.len(), 1);
    assert!(admission.decisions[0].admitted);
    assert_eq!(admission.reserve.mode, CriticalPathMode::Multi);
    assert!(admission.deferred.is_empty());
}

#[test]
fn multi_mode_unbounded_budget_keeps_the_table_but_admits_without_reserve() {
    let fixture = Fixture::new(vec![
        task("a", 1, &[], Some(5_000)),
        task("b", 1, &[], Some(3_000)),
    ]);
    let admission = evaluate_multi(
        &fixture,
        &[0, 0],
        &multi_policy(),
        GovernorBudgetView::new(None, 0, 0),
        &HashMap::new(),
        "2026-11-11T00:00:03Z",
    )
    .unwrap();
    let multi = admission.multi.as_ref().unwrap();
    assert_eq!(multi.paths.len(), 2);
    assert_eq!(multi.allocated_tokens, 0);
    assert_eq!(admission.reserve.state, "no_budget");
    assert_eq!(admission.decisions[0].reason, GovernorReason::NoBudget);
    assert!(admission.deferred.is_empty());
}

#[test]
fn multi_admission_event_carries_the_path_table_and_deferred_order() {
    let fixture = Fixture::new(vec![
        task("heavy", 1, &[], Some(10_000)),
        task("medium", 1, &[], Some(8_000)),
        task("side", 1, &[], Some(1_000)),
    ]);
    let mut policy = multi_policy();
    policy.max_paths = 2;
    let admission = evaluate_multi(
        &fixture,
        &[0, 0, 0],
        &policy,
        GovernorBudgetView::new(Some(20_000), 0, 0),
        &HashMap::new(),
        "2026-11-11T00:00:03Z",
    )
    .unwrap();
    let payload = admission_payload("session-1", "gov-1", &admission, "2026-11-11T00:00:03Z");
    assert_eq!(payload["mode"], "multi");
    assert_eq!(payload["paths"].as_array().unwrap().len(), 2);
    assert_eq!(payload["paths"][0]["task_ids"][0], "heavy");
    assert_eq!(payload["paths"][1]["allocation_tokens"], 8_000);
    assert_eq!(payload["allocated_tokens"], 18_000);
    assert_eq!(payload["deferred_order"][0], "side");
    assert_eq!(payload["reserve"]["mode"], Value::Null);
}

#[test]
fn multi_mode_throttle_defers_a_non_path_task_with_a_typed_code() {
    let fixture = Fixture::new(vec![
        task("a", 1, &[], Some(4_000)),
        task("b", 1, &[], Some(4_000)),
        task("side", 100, &[], Some(1_000)),
    ]);
    let mut policy = multi_policy();
    policy.max_paths = 2;
    policy.throttle_at_ratio = 0.9;
    let admission = evaluate_multi(
        &fixture,
        &[0, 0, 100],
        &policy,
        GovernorBudgetView::new(Some(100_000), 80_000, 0),
        &HashMap::new(),
        "2026-11-11T00:00:03Z",
    )
    .unwrap();
    assert!(admission.reserve.throttled);
    let side = admission
        .deferred
        .iter()
        .find(|deferral| deferral.task_id == "side")
        .unwrap();
    assert_eq!(side.reason, GovernorReason::Throttle);
    assert_eq!(side.reason.code(), "BUDGET_THROTTLED");
    assert!(!side.critical);
}

#[test]
fn multi_mode_min_task_budget_exhausts_even_protected_paths() {
    let fixture = Fixture::new(vec![
        task("a", 1, &[], Some(4_000)),
        task("b", 1, &[], Some(4_000)),
    ]);
    let mut policy = multi_policy();
    policy.min_task_budget = 2_000;
    let admission = evaluate_multi(
        &fixture,
        &[0, 0],
        &policy,
        GovernorBudgetView::new(Some(10_000), 9_500, 0),
        &HashMap::new(),
        "2026-11-11T00:00:03Z",
    )
    .unwrap();
    assert_eq!(admission.reserve.state, "exhausted");
    assert_eq!(admission.deferred.len(), 2);
    assert!(
        admission
            .deferred
            .iter()
            .all(|deferral| deferral.reason.code() == "BUDGET_EXHAUSTED")
    );
    assert_eq!(admission.multi.as_ref().unwrap().paths.len(), 2);
}

#[test]
fn multi_mode_disabled_policy_keeps_path_ids_on_decisions() {
    let fixture = Fixture::new(vec![
        task("a", 1, &[], Some(4_000)),
        task("b", 1, &[], Some(2_000)),
    ]);
    let mut policy = multi_policy();
    policy.enabled = false;
    let admission = evaluate_multi(
        &fixture,
        &[0, 0],
        &policy,
        GovernorBudgetView::new(Some(10_000), 0, 0),
        &HashMap::new(),
        "2026-11-11T00:00:03Z",
    )
    .unwrap();
    assert!(!admission.enabled);
    assert_eq!(admission.mode, CriticalPathMode::Multi);
    assert_eq!(admission.multi.as_ref().unwrap().paths.len(), 2);
    assert!(admission.decisions.iter().all(|decision| {
        decision.admitted
            && decision.reason == GovernorReason::Disabled
            && !decision.path_ids.is_empty()
    }));
    assert_eq!(admission.reserve.state, "disabled");
    assert!(admission.deferred.is_empty());
}

#[test]
fn shared_prefix_decisions_report_all_member_paths() {
    let fixture = Fixture::new(vec![
        task("root", 1, &[], Some(1_000)),
        task("a", 1, &["root"], Some(4_000)),
        task("b", 1, &["root"], Some(3_000)),
    ]);
    let admission = evaluate_multi(
        &fixture,
        &[0, 0, 0],
        &multi_policy(),
        GovernorBudgetView::new(Some(20_000), 0, 0),
        &HashMap::new(),
        "2026-11-11T00:00:03Z",
    )
    .unwrap();
    let root = admission
        .decisions
        .iter()
        .find(|decision| decision.task_id == "root")
        .unwrap();
    assert_eq!(root.path_ids, vec![0, 1]);
    assert_eq!(root.path_id, Some(0));
    let a = admission
        .decisions
        .iter()
        .find(|decision| decision.task_id == "a")
        .unwrap();
    let b = admission
        .decisions
        .iter()
        .find(|decision| decision.task_id == "b")
        .unwrap();
    assert_eq!(a.path_ids, vec![0]);
    assert_eq!(b.path_ids, vec![1]);
}

#[test]
fn multi_mode_pressure_admits_protected_paths_before_a_high_priority_side_task() {
    let fixture = Fixture::new(vec![
        task("a", 1, &[], Some(4_000)),
        task("b", 1, &[], Some(4_000)),
        task("side", 100, &[], Some(1_000)),
    ]);
    let mut policy = multi_policy();
    policy.max_paths = 2;
    policy.throttle_at_ratio = 0.95;
    policy.min_task_budget = 0;
    let admission = evaluate_multi(
        &fixture,
        &[0, 0, 100],
        &policy,
        GovernorBudgetView::new(Some(10_000), 0, 0),
        &HashMap::new(),
        "2026-11-11T00:00:03Z",
    )
    .unwrap();
    assert_eq!(admission.reserve.protected_tokens, 10_000);
    assert_eq!(admission.reserve.remaining_tokens, Some(10_000));
    assert!(admission.decisions[0].critical);
    assert!(admission.decisions[1].critical);
    assert_eq!(admission.decisions[2].task_id, "side");
    assert!(!admission.decisions[2].admitted);
    assert_eq!(admission.decisions[2].reason, GovernorReason::Reserve);
    assert!(admission.admitted.contains(&"a".to_string()));
    assert!(admission.admitted.contains(&"b".to_string()));
}

#[test]
fn multi_admission_event_path_table_is_bounded() {
    let mut tasks = Vec::new();
    for index in 0..20 {
        tasks.push(task(&format!("t{index:02}"), 1, &[], Some(1_000 + index)));
    }
    let fixture = Fixture::new(tasks);
    let mut policy = multi_policy();
    policy.max_paths = 16;
    let admission = evaluate_multi(
        &fixture,
        &[0; 20],
        &policy,
        GovernorBudgetView::new(Some(1_000_000), 0, 0),
        &HashMap::new(),
        "2026-11-11T00:00:03Z",
    )
    .unwrap();
    assert_eq!(admission.multi.as_ref().unwrap().paths.len(), 16);
    let payload = admission_payload("session-1", "gov-1", &admission, "now");
    assert_eq!(payload["paths"].as_array().unwrap().len(), 16);
    assert_eq!(payload["departments"].as_array().unwrap().len(), 1);
    assert_eq!(payload["paths"][0]["task_ids"][0], "t19");
}

#[test]
fn multi_denial_payload_carries_mode_and_an_empty_table() {
    let path = vec!["a".to_string()];
    let payload = denial_payload(GovernorDenial {
        session_id: "session-1",
        decision_id: "gov-2",
        plan_revision: 3,
        mode: CriticalPathMode::Multi,
        task_id: "side",
        task_critical: false,
        reason: GovernorReason::Reserve,
        requested_tokens: 1_000,
        limit_tokens: Some(10_000),
        spent_tokens: 5_000,
        reserved_tokens: 0,
        remaining_tokens: Some(5_000),
        protected_tokens: 4_000,
        path: &path,
        path_weight_tokens: 3_500,
        inputs_hash: "hash",
        detail: "multi deferral".to_string(),
        created_at: "2026-11-11T00:00:00Z",
    });
    assert_eq!(payload["mode"], "multi");
    assert!(payload["paths"].as_array().unwrap().is_empty());
    assert!(payload["departments"].as_array().unwrap().is_empty());
    assert_eq!(payload["deferred_order"][0], "side");
}

#[test]
fn observed_spend_quarter_blend_is_exact() {
    let fixture = Fixture::new(vec![task("a", 1, &[], Some(4_000))]);
    let mut policy = multi_policy();
    policy.observed_weight_ratio = 0.25;
    let mut observed = HashMap::new();
    observed.insert("a".to_string(), 8_000u64);
    let admission = evaluate_multi(
        &fixture,
        &[0],
        &policy,
        GovernorBudgetView::new(Some(20_000), 0, 0),
        &observed,
        "2026-11-11T00:00:03Z",
    )
    .unwrap();
    let path = &admission.multi.as_ref().unwrap().paths[0];
    assert_eq!(path.declared_weight_tokens, 4_000);
    assert_eq!(path.observed_tokens, 8_000);
    // 4,000 * 0.75 + 8,000 * 0.25 = 5,000.
    assert_eq!(path.weight_tokens, 5_000);
}

#[test]
fn multi_mode_orders_by_path_deficit_before_priority() {
    let fixture = Fixture::new(vec![
        task("heavy", 1, &[], Some(10_000)),
        task("light", 1, &[], Some(4_000)),
    ]);
    let mut policy = multi_policy();
    policy.reserve_ratio = 0.0;
    policy.min_task_budget = 0;
    // Pool 10,000 => allocations 7,143 / 2,857 and deficits 2,857 / 1,143.
    // `light` has the higher priority but the heavier path sorts first.
    let admission = evaluate_multi(
        &fixture,
        &[0, 100],
        &policy,
        GovernorBudgetView::new(Some(10_000), 0, 0),
        &HashMap::new(),
        "2026-11-11T00:00:03Z",
    )
    .unwrap();
    let multi = admission.multi.as_ref().unwrap();
    assert_eq!(multi.paths[0].deficit_tokens, 2_857);
    assert_eq!(multi.paths[1].deficit_tokens, 1_143);
    assert_eq!(admission.decisions[0].task_id, "heavy");
    assert_eq!(admission.decisions[0].path_deficit_tokens, 2_857);
    assert_eq!(admission.decisions[1].task_id, "light");
    assert_eq!(admission.decisions[1].path_deficit_tokens, 1_143);
    assert!(admission.deferred_order().is_empty());
}

#[test]
fn multi_mode_critical_tasks_bypass_the_throttle_zone() {
    let fixture = Fixture::new(vec![
        task("a", 1, &[], Some(4_000)),
        task("b", 1, &[], Some(4_000)),
        task("side", 1, &[], Some(1_000)),
    ]);
    let mut policy = multi_policy();
    policy.max_paths = 2;
    policy.throttle_at_ratio = 1.0;
    policy.min_task_budget = 0;
    let admission = evaluate_multi(
        &fixture,
        &[0, 0, 0],
        &policy,
        GovernorBudgetView::new(Some(10_000), 5_000, 0),
        &HashMap::new(),
        "2026-11-11T00:00:03Z",
    )
    .unwrap();
    assert!(admission.reserve.throttled);
    assert!(admission.admitted.contains(&"a".to_string()));
    assert!(admission.admitted.contains(&"b".to_string()));
    assert!(!admission.admitted.contains(&"side".to_string()));
    let side = admission
        .deferred
        .iter()
        .find(|deferral| deferral.task_id == "side")
        .unwrap();
    assert_eq!(side.reason, GovernorReason::Throttle);
}

#[test]
fn multi_mode_deferred_order_lists_every_deferral_once() {
    let fixture = Fixture::new(vec![
        task("a", 1, &[], Some(6_000)),
        task("b", 1, &[], Some(5_000)),
        task("c", 1, &[], Some(1_000)),
        task("d", 1, &[], Some(1_000)),
    ]);
    let mut policy = multi_policy();
    policy.max_paths = 2;
    policy.min_task_budget = 0;
    let admission = evaluate_multi(
        &fixture,
        &[0, 0, 0, 0],
        &policy,
        GovernorBudgetView::new(Some(10_000), 0, 0),
        &HashMap::new(),
        "2026-11-11T00:00:03Z",
    )
    .unwrap();
    assert_eq!(admission.deferred.len(), 2);
    let order = admission.deferred_order();
    assert_eq!(order.len(), 2);
    assert_eq!(order[0], "c");
    assert_eq!(order[1], "d");
    assert_eq!(
        order,
        admission
            .deferral_infos()
            .iter()
            .map(|item| item.task_id.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn multi_mode_complete_plan_has_no_path_table_and_admits_the_tail() {
    let fixture = Fixture::new(vec![
        task("done", 1, &[], Some(9_000)),
        task("tail", 1, &["done"], Some(1_000)),
    ])
    .with_statuses(&[("done", "completed")]);
    let mut policy = multi_policy();
    policy.reserve_ratio = 1.0;
    let admission = evaluate_multi(
        &fixture,
        &[0, 0],
        &policy,
        GovernorBudgetView::new(Some(10_000), 9_500, 0),
        &HashMap::new(),
        "2026-11-11T00:00:03Z",
    )
    .unwrap();
    let multi = admission.multi.as_ref().unwrap();
    // `tail` remains, so the plan is not complete yet; only `done` was
    // removed. The table has one path and the ratio reserve is bounded.
    assert_eq!(multi.paths.len(), 1);
    assert_eq!(multi.paths[0].task_ids, vec!["tail"]);
    assert_eq!(admission.decisions.len(), 1);
    assert!(admission.decisions[0].admitted);
    let complete = run_complete_multi(&fixture);
    assert!(complete.complete);
    assert!(complete.paths.is_empty());
}

fn run_complete_multi(fixture: &Fixture) -> super::MultiPathAllocation {
    let mut statuses = fixture.statuses.clone();
    statuses.insert("tail".to_string(), "completed".to_string());
    let plan = fixture.plan();
    crate::multi_path::compute_multi_path(crate::multi_path::MultiPathInput {
        session_id: "session-1",
        project_id: "project-1",
        plan_revision: 1,
        plan: &plan,
        statuses: &statuses,
        remaining_budget_tokens: Some(10_000),
        policy: &multi_policy(),
        observed_tokens: &HashMap::new(),
    })
    .unwrap()
}

#[test]
fn multi_admission_event_carries_department_floors() {
    let fixture = Fixture::new(vec![
        task("a", 1, &[], Some(4_000)),
        task("b", 1, &[], Some(2_000)),
    ]);
    let mut policy = multi_policy();
    policy.department_min_ratio = 0.5;
    let admission = evaluate_multi(
        &fixture,
        &[0, 0],
        &policy,
        GovernorBudgetView::new(Some(10_000), 0, 0),
        &HashMap::new(),
        "2026-11-11T00:00:03Z",
    )
    .unwrap();
    let payload = admission_payload("session-1", "gov-1", &admission, "now");
    let departments = payload["departments"].as_array().unwrap();
    assert_eq!(departments.len(), 1);
    assert_eq!(departments[0]["department"], "impl");
    assert!(departments[0]["floor_tokens"].as_u64().unwrap() > 0);
}

#[test]
fn policy_mode_switch_moves_the_inputs_hash() {
    let fixture = Fixture::new(vec![
        task("a", 1, &[], Some(5_000)),
        task("b", 1, &[], Some(5_000)),
    ]);
    let mut policy = multi_policy();
    policy.mode = CriticalPathMode::Multi;
    let multi = evaluate_multi(
        &fixture,
        &[0, 0],
        &policy,
        GovernorBudgetView::new(Some(20_000), 0, 0),
        &HashMap::new(),
        "2026-11-11T00:00:03Z",
    )
    .unwrap();
    policy.mode = CriticalPathMode::Single;
    let single = evaluate_multi(
        &fixture,
        &[0, 0],
        &policy,
        GovernorBudgetView::new(Some(20_000), 0, 0),
        &HashMap::new(),
        "2026-11-11T00:00:03Z",
    )
    .unwrap();
    assert_ne!(multi.inputs_hash, single.inputs_hash);
    assert_eq!(single.mode, CriticalPathMode::Single);
}

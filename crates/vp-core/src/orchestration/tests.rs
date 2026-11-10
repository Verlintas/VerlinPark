use std::collections::HashMap;

use vp_protocol::types::{
    DependencyFailurePolicy, Event, OrchestrationPolicy, PlanDepartment, PlanTask, ScheduledTask,
    Session, TeamPlan,
};

use super::controller::{ControllerAction, ControllerInput, plan_actions};
use super::plan::{diff_plan, ensure_revision_safe, status_map};
use super::policy::{retry_delay, validate_policy};
use super::snapshot::{OrchestrationInput, build_snapshot};

fn task(id: &str, deps: &[&str]) -> PlanTask {
    PlanTask {
        id: id.to_string(),
        department: "engineering".to_string(),
        title: format!("Task {id}"),
        kind: "implement".to_string(),
        priority: 0,
        profile: None,
        deps: deps.iter().map(|dep| dep.to_string()).collect(),
        write_scope: vec![format!("src/{id}")],
        acceptance: vec!["works".to_string()],
        token_budget: None,
        cost_budget_usd: None,
    }
}

fn plan(tasks: Vec<PlanTask>) -> TeamPlan {
    TeamPlan {
        goal: "ship".to_string(),
        departments: vec![PlanDepartment {
            name: "engineering".to_string(),
            manager_title: "Engineering Manager".to_string(),
            profile: "coding".to_string(),
            objective: "build".to_string(),
            token_budget: None,
            cost_budget_usd: None,
        }],
        tasks,
    }
}

fn session() -> Session {
    Session {
        id: "session-1".to_string(),
        project_id: "project-1".to_string(),
        goal: "ship".to_string(),
        status: "running".to_string(),
        priority: 0,
        worktree: None,
        branch: None,
        token_budget: None,
        cost_budget_usd: None,
        time_budget_secs: None,
        unlimited: false,
        cost_spent_usd: 0.0,
        base_commit: None,
        tokens_in: 0,
        tokens_out: 0,
        created_at: "2026-01-01T00:00:00Z".to_string(),
    }
}

fn row(task: &PlanTask, status: &str, attempts: u32) -> ScheduledTask {
    ScheduledTask {
        session_id: "session-1".to_string(),
        project_id: "project-1".to_string(),
        plan_seq: 1,
        task_id: task.id.clone(),
        department: task.department.clone(),
        title: task.title.clone(),
        priority: task.priority,
        status: status.to_string(),
        deps: task.deps.clone(),
        write_scope: task.write_scope.clone(),
        attempts,
        enqueued_at: "2026-01-01T00:00:00Z".to_string(),
        started_at: None,
        finished_at: None,
        last_error: None,
        next_attempt_at: None,
    }
}

fn event(seq: u64, kind: &str, payload: serde_json::Value) -> Event {
    Event {
        session_id: "session-1".to_string(),
        seq,
        kind: kind.to_string(),
        payload,
        created_at: format!("2026-01-01T00:00:{seq:02}Z"),
    }
}

fn policy() -> OrchestrationPolicy {
    OrchestrationPolicy {
        auto_retry_failed: true,
        max_attempts: 3,
        retry_backoff_secs: 10,
        retry_priority_boost: 10,
        dependency_failure: DependencyFailurePolicy::Block,
        escalate_after_retries: true,
        auto_resolve_blockers: true,
    }
}

#[test]
fn policy_validation_bounds_attempts_and_backoff() {
    let mut policy = policy();
    assert!(validate_policy(&policy).is_ok());
    policy.max_attempts = 0;
    assert!(validate_policy(&policy).is_err());
    policy.max_attempts = 11;
    assert!(validate_policy(&policy).is_err());
    policy.max_attempts = 2;
    policy.retry_backoff_secs = super::policy::MAX_RETRY_BACKOFF_SECS + 1;
    assert!(validate_policy(&policy).is_err());
    policy.retry_backoff_secs = 10;
    policy.retry_priority_boost = 101;
    assert!(validate_policy(&policy).is_err());
}

#[test]
fn retry_delay_is_exponential_and_capped() {
    let mut policy = policy();
    assert_eq!(retry_delay(&policy, 1).as_secs(), 10);
    assert_eq!(retry_delay(&policy, 2).as_secs(), 20);
    assert_eq!(retry_delay(&policy, 3).as_secs(), 40);
    policy.retry_backoff_secs = 100_000;
    assert_eq!(
        retry_delay(&policy, 10).as_secs(),
        super::policy::MAX_RETRY_BACKOFF_SECS
    );
    policy.retry_backoff_secs = 0;
    assert_eq!(retry_delay(&policy, 5).as_secs(), 0);
}

#[test]
fn plan_diff_reports_add_remove_and_change() {
    let mut first = task("a", &[]);
    let removed = task("b", &[]);
    let old = plan(vec![first.clone(), removed]);
    first.priority = 9;
    let added = task("c", &["a"]);
    let new = plan(vec![first, added]);
    let diff = diff_plan(&old, &new);
    assert_eq!(diff.added, vec!["c"]);
    assert_eq!(diff.removed, vec!["b"]);
    assert_eq!(diff.changed, vec!["a"]);
}

#[test]
fn revision_cannot_remove_active_or_completed_tasks() {
    let active = task("a", &[]);
    let done = task("b", &[]);
    let old = plan(vec![active.clone(), done.clone()]);
    let statuses = HashMap::from([
        ("a".to_string(), "running".to_string()),
        ("b".to_string(), "completed".to_string()),
    ]);
    assert!(ensure_revision_safe(&old, &plan(vec![done.clone()]), &statuses).is_err());
    assert!(ensure_revision_safe(&old, &plan(vec![active]), &statuses).is_err());
    // Failed work may be replaced.
    let failed_statuses = HashMap::from([("a".to_string(), "failed".to_string())]);
    assert!(ensure_revision_safe(&old, &plan(vec![done]), &failed_statuses).is_ok());
}

#[test]
fn status_map_applies_new_lifecycle_events() {
    let tasks = plan(vec![task("a", &[])]);
    let events = vec![event(
        2,
        "plan_task.retry_scheduled",
        serde_json::json!({
            "task_id": "a",
            "task_status": "pending",
            "next_attempt_at": "2026-01-01T00:00:30Z"
        }),
    )];
    let statuses = status_map(&tasks, &[], &events);
    assert_eq!(statuses.get("a").map(String::as_str), Some("pending"));

    let events = vec![
        event(
            2,
            "plan_task.blocked",
            serde_json::json!({
                "task_id": "a",
                "task_status": "blocked"
            }),
        ),
        event(
            3,
            "plan_task.unblocked",
            serde_json::json!({
                "task_id": "a",
                "task_status": "pending"
            }),
        ),
    ];
    assert_eq!(
        status_map(&tasks, &[], &events)
            .get("a")
            .map(String::as_str),
        Some("pending")
    );
}

#[test]
fn controller_retries_a_failed_task_with_budget() {
    let task = task("a", &[]);
    let plan = plan(vec![task.clone()]);
    let rows = vec![row(&task, "failed", 1)];
    let actions = plan_actions(ControllerInput {
        session: &session(),
        plan: &plan,
        scheduled: &rows,
        events: &[],
        notes: &[],
        policy: &policy(),
        budget_exhausted: false,
    });
    assert!(matches!(
        &actions[0],
        ControllerAction::Retry { task_id, delay_secs: 10, .. } if task_id == "a"
    ));
    assert!(
        !actions
            .iter()
            .any(|action| matches!(action, ControllerAction::Escalate { .. }))
    );
}

#[test]
fn controller_does_not_retry_after_the_session_budget_is_exhausted() {
    let task = task("a", &[]);
    let plan = plan(vec![task.clone()]);
    let rows = vec![row(&task, "failed", 1)];
    let mut session = session();
    session.token_budget = Some(100);
    session.tokens_in = 80;
    session.tokens_out = 20;
    let actions = plan_actions(ControllerInput {
        session: &session,
        plan: &plan,
        scheduled: &rows,
        events: &[],
        notes: &[],
        policy: &policy(),
        budget_exhausted: true,
    });
    assert!(
        !actions
            .iter()
            .any(|action| matches!(action, ControllerAction::Retry { .. }))
    );
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, ControllerAction::Escalate { .. }))
    );
}

#[test]
fn controller_does_not_retry_when_retry_is_already_scheduled() {
    let task = task("a", &[]);
    let plan = plan(vec![task.clone()]);
    let mut failed = row(&task, "failed", 1);
    failed.next_attempt_at = Some("2026-01-01T00:01:00Z".to_string());
    let actions = plan_actions(ControllerInput {
        session: &session(),
        plan: &plan,
        scheduled: &[failed],
        events: &[],
        notes: &[],
        policy: &policy(),
        budget_exhausted: false,
    });
    assert!(actions.is_empty());
}

#[test]
fn controller_escalates_exhausted_task_and_blocks_dependents() {
    let first = task("a", &[]);
    let second = task("b", &["a"]);
    let plan = plan(vec![first.clone(), second.clone()]);
    let rows = vec![row(&first, "failed", 3), row(&second, "pending", 0)];
    let actions = plan_actions(ControllerInput {
        session: &session(),
        plan: &plan,
        scheduled: &rows,
        events: &[],
        notes: &[],
        policy: &policy(),
        budget_exhausted: false,
    });
    assert!(actions.iter().any(|action| matches!(
        action,
        ControllerAction::Escalate { task_id, attempt: 3, .. } if task_id == "a"
    )));
    assert!(actions.iter().any(|action| matches!(
        action,
        ControllerAction::Block { task_id, dependency, .. } if task_id == "b" && dependency == "a"
    )));
}

#[test]
fn controller_cancel_policy_cancels_dependents() {
    let first = task("a", &[]);
    let second = task("b", &["a"]);
    let plan = plan(vec![first.clone(), second.clone()]);
    let rows = vec![row(&first, "failed", 3), row(&second, "pending", 0)];
    let mut policy = policy();
    policy.dependency_failure = DependencyFailurePolicy::Cancel;
    let actions = plan_actions(ControllerInput {
        session: &session(),
        plan: &plan,
        scheduled: &rows,
        events: &[],
        notes: &[],
        policy: &policy,
        budget_exhausted: false,
    });
    assert!(actions.iter().any(|action| matches!(
        action,
        ControllerAction::Cancel { task_id, .. } if task_id == "b"
    )));
}

#[test]
fn controller_unblocks_stale_blocked_tasks() {
    let first = task("a", &[]);
    let second = task("b", &["a"]);
    let plan = plan(vec![first.clone(), second.clone()]);
    let rows = vec![row(&first, "completed", 1), row(&second, "blocked", 0)];
    let actions = plan_actions(ControllerInput {
        session: &session(),
        plan: &plan,
        scheduled: &rows,
        events: &[],
        notes: &[],
        policy: &policy(),
        budget_exhausted: false,
    });
    assert!(actions.iter().any(|action| matches!(
        action,
        ControllerAction::Unblock { task_id, .. } if task_id == "b"
    )));
}

#[test]
fn controller_resolves_blockers_for_completed_tasks() {
    let task = task("a", &[]);
    let plan = plan(vec![task.clone()]);
    let rows = vec![row(&task, "completed", 1)];
    let events = vec![event(
        2,
        "coordination.note",
        serde_json::json!({
            "note_id": "note-1",
            "action": "blocker",
            "task_id": "a",
            "message": "schema is unclear",
            "agent_id": "w1",
            "agent_name": "Worker One"
        }),
    )];
    let notes = super::snapshot::coordination_notes(&events);
    let actions = plan_actions(ControllerInput {
        session: &session(),
        plan: &plan,
        scheduled: &rows,
        events: &events,
        notes: &notes,
        policy: &policy(),
        budget_exhausted: false,
    });
    assert!(actions.iter().any(|action| matches!(
        action,
        ControllerAction::ResolveBlockers { task_id, count: 1 } if task_id == "a"
    )));
}

#[test]
fn snapshot_reports_metrics_blockers_notes_and_decisions() {
    let first = task("a", &[]);
    let second = task("b", &["a"]);
    let plan = plan(vec![first.clone(), second.clone()]);
    let rows = vec![row(&first, "completed", 1), row(&second, "failed", 2)];
    let events = vec![
        event(
            1,
            "team_plan.submitted",
            serde_json::json!({
                "plan": plan,
                "task_count": 2,
                "author": "leader"
            }),
        ),
        event(
            2,
            "plan_task.finished",
            serde_json::json!({
                "task_id": "a",
                "task_status": "completed",
                "summary": "done"
            }),
        ),
        event(
            3,
            "plan_task.finished",
            serde_json::json!({
                "task_id": "b",
                "task_status": "failed",
                "reason": "compile error"
            }),
        ),
        event(
            4,
            "coordination.note",
            serde_json::json!({
                "note_id": "n1",
                "action": "blocker",
                "task_id": "b",
                "message": "blocked on a",
                "agent_id": "w1",
                "agent_name": "Worker One"
            }),
        ),
        event(
            5,
            "orchestration.decision",
            serde_json::json!({
                "decision_id": "d1",
                "action": "retry",
                "task_id": "b",
                "detail": "attempt 2 failed"
            }),
        ),
    ];
    let session = session();
    let notes = super::snapshot::coordination_notes(&events);
    let decisions = super::snapshot::decisions_from_events(&events);
    let snapshot = build_snapshot(OrchestrationInput {
        session: &session,
        plan: Some(&plan),
        scheduled: &rows,
        agents: &[],
        events: &events,
        merge_queue: &[],
        questions: &[],
        notes: &notes,
        decisions: &decisions,
        policy: &policy(),
    });
    assert_eq!(snapshot.plan_revision, 1);
    assert_eq!(snapshot.metrics.total_tasks, 2);
    assert_eq!(snapshot.metrics.completed_tasks, 1);
    assert_eq!(snapshot.metrics.failed_tasks, 1);
    assert_eq!(snapshot.notes.len(), 1);
    assert_eq!(snapshot.decisions.len(), 1);
    assert!(
        snapshot
            .blockers
            .iter()
            .any(|blocker| blocker.kind == "coordination")
    );
    assert_eq!(
        snapshot
            .tasks
            .iter()
            .find(|task| task.task_id == "a")
            .and_then(|task| task.result_summary.clone()),
        Some("done".to_string())
    );
    assert!(
        snapshot
            .tasks
            .iter()
            .find(|task| task.task_id == "b")
            .is_some_and(|task| task.blocked_by.is_empty())
    );
}

#[test]
fn snapshot_marks_dependents_blocked_by_unmet_dependencies() {
    let first = task("a", &[]);
    let second = task("b", &["a"]);
    let plan = plan(vec![first.clone(), second.clone()]);
    let rows = vec![row(&first, "failed", 3), row(&second, "pending", 0)];
    let session = session();
    let snapshot = build_snapshot(OrchestrationInput {
        session: &session,
        plan: Some(&plan),
        scheduled: &rows,
        agents: &[],
        events: &[],
        merge_queue: &[],
        questions: &[],
        notes: &[],
        decisions: &[],
        policy: &policy(),
    });
    let second = snapshot
        .tasks
        .iter()
        .find(|task| task.task_id == "b")
        .expect("task b");
    assert!(!second.ready);
    assert_eq!(second.blocked_by, vec!["a (failed)"]);
}

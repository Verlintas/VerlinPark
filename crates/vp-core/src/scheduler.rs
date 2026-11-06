use std::collections::{HashMap, HashSet};
use std::env;
use std::time::Duration;

use anyhow::Result;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use vp_protocol::types::{PlanTask, ScheduledTask, Session, TaskQueueResult, TeamPlan, WriteLease};
use vp_store::Store;

/// Persistent task-DAG queue. Task definitions come from the submitted
/// TeamPlan; live status, attempt counts, enqueue time, and write scopes are
/// projections that survive daemon restarts and plan resumes.
#[derive(Clone)]
pub struct TaskScheduler {
    store: Store,
    aging: Duration,
}

#[derive(Debug, Clone)]
pub struct PendingSession {
    pub session_id: String,
    pub project_id: String,
    pub priority: i32,
    pub pending: usize,
    pub task_priority: i32,
    pub enqueued_at: String,
    pub effective_priority: i64,
}

impl TaskScheduler {
    pub fn new(store: Store) -> Self {
        Self::new_with_aging(store, task_aging())
    }

    pub fn new_with_aging(store: Store, aging: Duration) -> Self {
        Self { store, aging }
    }

    /// Rebuild the task projection from the latest `team_plan.submitted`
    /// event and the plan lifecycle events after it. Returns the task rows
    /// and the plan submission sequence.
    pub async fn sync_plan(
        &self,
        session: &Session,
        plan: &TeamPlan,
    ) -> Result<(u64, Vec<ScheduledTask>)> {
        let store = self.store.clone();
        let session_id = session.id.clone();
        let project_id = session.project_id.clone();
        let plan = plan.clone();
        tokio::task::spawn_blocking(move || {
            let plan_seq = store.latest_event_seq(&session_id, "team_plan.submitted")?;
            let tasks = store.sync_plan_tasks(&session_id, &project_id, plan_seq, &plan)?;
            Ok((plan_seq, tasks))
        })
        .await?
    }

    /// Return ready tasks in deterministic priority order. Higher effective
    /// priority first; aging adds one point per interval so low-priority ready
    /// work cannot starve behind a permanently high-priority sibling. Ties use
    /// enqueue time then task id.
    pub async fn ready_tasks(
        &self,
        session: &Session,
        plan_seq: u64,
        plan: &TeamPlan,
        limit: usize,
    ) -> Result<Vec<PlanTask>> {
        let store = self.store.clone();
        let session_id = session.id.clone();
        let priority = session.priority;
        let aging = self.aging;
        let plan = plan.clone();
        tokio::task::spawn_blocking(move || {
            let rows = store.list_plan_tasks(None, Some(&session_id))?;
            let rows = rows
                .into_iter()
                .filter(|row| row.plan_seq == plan_seq)
                .collect::<Vec<_>>();
            let statuses = rows
                .iter()
                .map(|row| (row.task_id.clone(), row.status.clone()))
                .collect::<HashMap<_, _>>();
            let completed = statuses
                .iter()
                .filter(|(_, status)| status.as_str() == "completed")
                .map(|(task_id, _)| task_id.clone())
                .collect::<HashSet<_>>();
            let now = OffsetDateTime::now_utc();
            let mut ready = rows
                .into_iter()
                .filter(|row| is_retryable_status(&row.status))
                .filter(|row| row.deps.iter().all(|dep| completed.contains(dep)))
                .map(|row| (task_effective_priority(priority, &row, now, aging), row))
                .collect::<Vec<_>>();
            // Deterministic order: effective priority, then oldest enqueue
            // time, then task id.
            ready.sort_by(|(left_priority, left), (right_priority, right)| {
                right_priority
                    .cmp(left_priority)
                    .then_with(|| left.enqueued_at.cmp(&right.enqueued_at))
                    .then_with(|| left.task_id.cmp(&right.task_id))
            });
            let definitions = plan
                .tasks
                .iter()
                .map(|task| (task.id.clone(), task.clone()))
                .collect::<HashMap<_, _>>();
            Ok(ready
                .into_iter()
                .filter_map(|(_, row)| definitions.get(&row.task_id).cloned())
                .take(limit.max(1))
                .collect())
        })
        .await?
    }

    pub async fn claim(
        &self,
        session_id: &str,
        plan_seq: u64,
        task_ids: &[String],
    ) -> Result<Vec<ScheduledTask>> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let task_ids = task_ids.to_vec();
        tokio::task::spawn_blocking(move || {
            store.claim_plan_tasks(&session_id, plan_seq, &task_ids)
        })
        .await?
    }

    pub async fn finish(
        &self,
        session_id: &str,
        plan_seq: u64,
        task_id: &str,
        status: &str,
        detail: Option<&str>,
    ) -> Result<Option<ScheduledTask>> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let task_id = task_id.to_string();
        let status = status.to_string();
        let detail = detail.map(ToOwned::to_owned);
        tokio::task::spawn_blocking(move || {
            store.finish_plan_task(&session_id, plan_seq, &task_id, &status, detail.as_deref())
        })
        .await?
    }

    /// Pick the next idle/failed/cancelled session with retryable tasks for
    /// the opt-in daemon scheduler. Effective priority combines the session
    /// priority with task age and prefers a different session from the last
    /// selection on ties.
    pub async fn next_pending_session(
        &self,
        last_session: Option<&str>,
        excluded: &[String],
    ) -> Result<Option<PendingSession>> {
        let store = self.store.clone();
        let aging = self.aging;
        let last_session = last_session.map(ToOwned::to_owned);
        let excluded = excluded.to_vec();
        tokio::task::spawn_blocking(move || {
            let now = OffsetDateTime::now_utc();
            let mut candidates = store
                .pending_task_sessions()?
                .into_iter()
                .filter(|(session_id, _, _, _, _, _)| !excluded.contains(session_id))
                .map(
                    |(session_id, project_id, priority, pending, enqueued_at, task_priority)| {
                        let effective_priority = session_effective_priority(
                            priority.saturating_add(task_priority).clamp(-100, 100),
                            &enqueued_at,
                            now,
                            aging,
                        );
                        PendingSession {
                            session_id,
                            project_id,
                            priority,
                            pending,
                            task_priority,
                            enqueued_at,
                            effective_priority,
                        }
                    },
                )
                .collect::<Vec<_>>();
            if candidates.is_empty() {
                return Ok(None);
            }
            candidates.sort_by(|left, right| {
                right
                    .effective_priority
                    .cmp(&left.effective_priority)
                    .then_with(|| left.enqueued_at.cmp(&right.enqueued_at))
                    .then_with(|| left.session_id.cmp(&right.session_id))
            });
            let highest = candidates[0].effective_priority;
            let tied = candidates
                .iter()
                .filter(|candidate| candidate.effective_priority == highest)
                .collect::<Vec<_>>();
            let picked = tied
                .iter()
                .find(|candidate| Some(candidate.session_id.as_str()) != last_session.as_deref())
                .copied()
                .unwrap_or(tied[0]);
            Ok(Some(picked.clone()))
        })
        .await?
    }

    pub async fn queue(
        &self,
        project_id: Option<String>,
        session_id: Option<String>,
    ) -> Result<TaskQueueResult> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            let tasks = store.list_plan_tasks(project_id.as_deref(), session_id.as_deref())?;
            let leases = store.list_write_leases(project_id.as_deref(), session_id.as_deref())?;
            Ok(TaskQueueResult { tasks, leases })
        })
        .await?
    }

    pub async fn leases(
        &self,
        project_id: Option<String>,
        session_id: Option<String>,
    ) -> Result<Vec<WriteLease>> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            store.list_write_leases(project_id.as_deref(), session_id.as_deref())
        })
        .await?
    }
}

fn task_aging() -> Duration {
    let seconds = env::var("VERLINPARK_PRIORITY_AGING_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(60)
        .min(86_400);
    if seconds == 0 {
        Duration::ZERO
    } else {
        Duration::from_secs(seconds)
    }
}

fn is_retryable_status(status: &str) -> bool {
    matches!(
        status,
        "pending" | "interrupted" | "failed" | "blocked" | "cancelled"
    )
}

fn session_effective_priority(
    priority: i32,
    enqueued_at: &str,
    now: OffsetDateTime,
    aging: Duration,
) -> i64 {
    if aging.is_zero() {
        return i64::from(priority);
    }
    let age = OffsetDateTime::parse(enqueued_at, &Rfc3339)
        .map(|enqueued| (now - enqueued).whole_seconds().max(0))
        .unwrap_or(0);
    i64::from(priority) + (age / aging.as_secs().max(1) as i64).min(100)
}

fn task_effective_priority(
    session_priority: i32,
    task: &ScheduledTask,
    now: OffsetDateTime,
    aging: Duration,
) -> i64 {
    let base = session_priority
        .saturating_add(task.priority)
        .clamp(-100, 100);
    if aging.is_zero() {
        return i64::from(base);
    }
    let age = OffsetDateTime::parse(&task.enqueued_at, &Rfc3339)
        .map(|enqueued| (now - enqueued).whole_seconds().max(0))
        .unwrap_or(0);
    i64::from(base) + (age / aging.as_secs().max(1) as i64).min(100)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use vp_protocol::types::{PlanDepartment, Project};

    fn store() -> (TempDir, Store, Project, Session) {
        let dir = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("scheduler"))
            .unwrap();
        let session = store.create_session(&project.id, "schedule").unwrap();
        store
            .append_event(&session.id, "team_plan.submitted", serde_json::json!({}))
            .unwrap();
        (dir, store, project, session)
    }

    fn plan(tasks: Vec<PlanTask>) -> TeamPlan {
        TeamPlan {
            goal: "schedule".into(),
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

    fn task(id: &str, priority: i32, deps: Vec<&str>) -> PlanTask {
        PlanTask {
            id: id.into(),
            department: "impl".into(),
            title: id.into(),
            kind: "coding".into(),
            priority,
            profile: None,
            deps: deps.into_iter().map(ToOwned::to_owned).collect(),
            write_scope: vec![id.into()],
            acceptance: vec!["done".into()],
            token_budget: None,
            cost_budget_usd: None,
        }
    }

    #[tokio::test]
    async fn orders_ready_tasks_and_respects_dependencies() {
        let (_dir, store, _project, session) = store();
        let scheduler = TaskScheduler::new_with_aging(store, Duration::ZERO);
        let plan = plan(vec![
            task("low", -5, vec![]),
            task("high", 20, vec![]),
            task("child", 100, vec!["high"]),
        ]);
        let (seq, rows) = scheduler.sync_plan(&session, &plan).await.unwrap();
        assert_eq!(rows.len(), 3);
        let ready = scheduler
            .ready_tasks(&session, seq, &plan, 10)
            .await
            .unwrap();
        assert_eq!(
            ready
                .iter()
                .map(|task| task.id.as_str())
                .collect::<Vec<_>>(),
            vec!["high", "low"]
        );
        let claimed = scheduler
            .claim(&session.id, seq, &["high".into()])
            .await
            .unwrap();
        assert_eq!(claimed[0].status, "running");
        assert_eq!(claimed[0].attempts, 1);
        scheduler
            .finish(&session.id, seq, "high", "completed", None)
            .await
            .unwrap();
        let ready = scheduler
            .ready_tasks(&session, seq, &plan, 10)
            .await
            .unwrap();
        assert_eq!(
            ready
                .iter()
                .map(|task| task.id.as_str())
                .collect::<Vec<_>>(),
            vec!["child", "low"]
        );
    }

    #[tokio::test]
    async fn picks_pending_sessions_with_round_robin_and_exclusions() {
        let (_dir, store, project, first) = store();
        let second = store.create_session(&project.id, "second").unwrap();
        store
            .append_event(&second.id, "team_plan.submitted", serde_json::json!({}))
            .unwrap();
        store.update_session_status(&first.id, "idle").unwrap();
        store.update_session_status(&second.id, "idle").unwrap();
        let scheduler = TaskScheduler::new_with_aging(store, Duration::ZERO);
        let plan = plan(vec![task("T1", 0, vec![])]);
        scheduler.sync_plan(&first, &plan).await.unwrap();
        scheduler.sync_plan(&second, &plan).await.unwrap();

        let picked = scheduler
            .next_pending_session(Some(&first.id), &[])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(picked.session_id, second.id);
        let picked = scheduler
            .next_pending_session(Some(&second.id), std::slice::from_ref(&second.id))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(picked.session_id, first.id);
    }

    #[tokio::test]
    async fn pending_session_selection_uses_high_task_priority() {
        let (_dir, store, project, first) = store();
        let second = store.create_session(&project.id, "high task").unwrap();
        store
            .append_event(&second.id, "team_plan.submitted", serde_json::json!({}))
            .unwrap();
        store.update_session_status(&first.id, "idle").unwrap();
        store.update_session_status(&second.id, "idle").unwrap();
        let scheduler = TaskScheduler::new_with_aging(store, Duration::ZERO);
        scheduler
            .sync_plan(&first, &plan(vec![task("low", -20, vec![])]))
            .await
            .unwrap();
        scheduler
            .sync_plan(&second, &plan(vec![task("high", 50, vec![])]))
            .await
            .unwrap();
        let picked = scheduler
            .next_pending_session(None, &[])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(picked.session_id, second.id);
        assert_eq!(picked.task_priority, 50);
    }

    #[tokio::test]
    async fn retries_failed_tasks_but_skips_completed_dependencies() {
        let (_dir, store, _project, session) = store();
        let scheduler = TaskScheduler::new_with_aging(store, Duration::ZERO);
        let plan = plan(vec![task("a", 0, vec![]), task("b", 0, vec!["a"])]);
        let (seq, _) = scheduler.sync_plan(&session, &plan).await.unwrap();
        scheduler
            .claim(&session.id, seq, &["a".into()])
            .await
            .unwrap();
        scheduler
            .finish(&session.id, seq, "a", "failed", Some("tests failed"))
            .await
            .unwrap();
        let ready = scheduler
            .ready_tasks(&session, seq, &plan, 10)
            .await
            .unwrap();
        assert_eq!(
            ready
                .iter()
                .map(|task| task.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a"]
        );
        let queue = scheduler
            .queue(None, Some(session.id.clone()))
            .await
            .unwrap();
        assert_eq!(queue.tasks[0].last_error.as_deref(), Some("tests failed"));
    }
}

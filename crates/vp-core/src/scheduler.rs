use std::collections::{HashMap, HashSet};
use std::env;
use std::time::Duration;

use anyhow::Result;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::sync::watch;
use vp_protocol::types::{
    GovernorDeferralInfo, IntakeTask, PlanTask, ScheduledTask, Session, TaskQueueResult, TeamPlan,
    WriteLease,
};
use vp_store::Store;

use crate::budget_governor::{
    GovernorAdmission, GovernorBudgetView, GovernorCandidate, MAX_PREVIEW_DECISIONS, evaluate,
};

/// Persistent task-DAG queue. Task definitions come from the submitted
/// TeamPlan; live status, attempt counts, enqueue time, and write scopes are
/// projections that survive daemon restarts and plan resumes.
#[derive(Clone)]
pub struct TaskScheduler {
    store: Store,
    aging: Duration,
    state: watch::Sender<u64>,
    global_enabled: bool,
    default_cooldown_secs: u64,
}

#[derive(Debug, Clone)]
pub struct PendingIntake {
    pub task: IntakeTask,
    pub effective_priority: i64,
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
    pub cooldown_secs: u64,
}

/// Result of governed ready-task selection. When the policy is disabled (the
/// default), `governor` is `None`, `deferred` is empty, and `ready` keeps the
/// existing priority/aging order exactly.
#[derive(Debug, Clone)]
pub struct ReadyTaskSelection {
    pub ready: Vec<PlanTask>,
    pub deferred: Vec<GovernorDeferralInfo>,
    pub governor: Option<GovernorAdmission>,
}

impl TaskScheduler {
    pub fn new(store: Store) -> Self {
        Self::new_with_aging_and_policy(
            store,
            task_aging(),
            task_scheduler_global_default(),
            task_scheduler_cooldown_default(),
        )
    }

    pub fn new_with_aging(store: Store, aging: Duration) -> Self {
        Self::new_with_aging_and_policy(store, aging, false, 60)
    }

    pub fn new_with_aging_and_policy(
        store: Store,
        aging: Duration,
        global_enabled: bool,
        default_cooldown_secs: u64,
    ) -> Self {
        let (state, _) = watch::channel(0);
        Self {
            store,
            aging,
            state,
            global_enabled,
            default_cooldown_secs: default_cooldown_secs.max(1),
        }
    }

    /// Wake workers waiting for a task pause/cancel/resume transition.
    pub fn notify_state(&self) {
        self.state
            .send_modify(|value| *value = value.wrapping_add(1));
    }

    pub fn subscribe_state(&self) -> watch::Receiver<u64> {
        self.state.subscribe()
    }

    pub async fn plan_task_status(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<Option<String>> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let task_id = task_id.to_string();
        tokio::task::spawn_blocking(move || store.plan_task_status(&session_id, &task_id)).await?
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
    ///
    /// When the project's critical-path policy is enabled, this applies the
    /// budget governor: critical-path tasks are admitted first under pressure,
    /// and non-critical tasks over the protected reserve remain queued.
    pub async fn ready_tasks(
        &self,
        session: &Session,
        plan_seq: u64,
        plan: &TeamPlan,
        limit: usize,
    ) -> Result<Vec<PlanTask>> {
        Ok(self
            .ready_tasks_governed(session, plan_seq, plan, limit, None)
            .await?
            .ready)
    }

    /// Governed variant used by the plan runner. `budget` should carry the
    /// live session limit/spend and in-flight reservation total; `None` falls
    /// back to the session projection (spend only, no in-flight reservations).
    pub async fn ready_tasks_governed(
        &self,
        session: &Session,
        plan_seq: u64,
        plan: &TeamPlan,
        limit: usize,
        budget: Option<GovernorBudgetView>,
    ) -> Result<ReadyTaskSelection> {
        let store = self.store.clone();
        let session_id = session.id.clone();
        let project_id = session.project_id.clone();
        let priority = session.priority;
        let aging = self.aging;
        let plan = plan.clone();
        let session_budget = GovernorBudgetView::new(
            session.token_budget,
            session.tokens_in.saturating_add(session.tokens_out),
            0,
        );
        let unlimited = session.unlimited;
        tokio::task::spawn_blocking(move || {
            let rows = store.list_plan_tasks(None, Some(&session_id))?;
            let orchestration_policy = store.get_orchestration_policy(&project_id)?;
            let critical_policy = store.get_critical_path_policy(&project_id)?;
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
                .filter(|row| {
                    row.next_attempt_at
                        .as_deref()
                        .map(|next| {
                            time::OffsetDateTime::parse(
                                next,
                                &time::format_description::well_known::Rfc3339,
                            )
                            .map(|next| next <= now)
                            .unwrap_or(true)
                        })
                        .unwrap_or(true)
                })
                .map(|row| {
                    let retry_boost = if row.attempts > 0
                        && matches!(row.status.as_str(), "failed" | "pending")
                    {
                        i64::from(orchestration_policy.retry_priority_boost)
                    } else {
                        0
                    };
                    (
                        task_effective_priority(priority, &row, now, aging) + retry_boost,
                        row,
                    )
                })
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
            let fallback = ready
                .iter()
                .filter_map(|(_, row)| definitions.get(&row.task_id).cloned())
                .take(limit.max(1))
                .collect::<Vec<_>>();
            if unlimited || !critical_policy.enabled {
                return Ok(ReadyTaskSelection {
                    ready: fallback,
                    deferred: Vec::new(),
                    governor: None,
                });
            }
            let budget = budget.unwrap_or(session_budget);
            let plan_revision = store.plan_revision_count(&session_id)?;
            let observed_tokens = if critical_policy.mode
                == vp_protocol::types::CriticalPathMode::Multi
                && critical_policy.observed_weight_ratio > 0.0
            {
                store.usage_tokens_by_task(&session_id)?
            } else {
                HashMap::new()
            };
            let evaluated_at = now
                .format(&Rfc3339)
                .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string());
            let candidates = ready
                .iter()
                .filter_map(|(effective_priority, row)| {
                    definitions.get(&row.task_id).map(|task| GovernorCandidate {
                        task,
                        effective_priority: *effective_priority,
                        enqueued_at: row.enqueued_at.as_str(),
                    })
                })
                .take(MAX_PREVIEW_DECISIONS)
                .collect::<Vec<_>>();
            let admission = evaluate(crate::budget_governor::GovernorEvaluation {
                session_id: &session_id,
                project_id: &project_id,
                plan_revision,
                plan: &plan,
                statuses: &statuses,
                policy: &critical_policy,
                candidates: &candidates,
                budget,
                remaining_override: None,
                observed_tokens: &observed_tokens,
                evaluated_at: &evaluated_at,
            })?;
            let selected = admission
                .admitted
                .iter()
                .filter_map(|task_id| definitions.get(task_id).cloned())
                .take(limit.max(1))
                .collect::<Vec<_>>();
            let deferred = admission.deferral_infos();
            Ok(ReadyTaskSelection {
                ready: selected,
                deferred,
                governor: Some(admission),
            })
        })
        .await?
    }

    pub async fn claim(
        &self,
        session_id: &str,
        plan_seq: u64,
        task_ids: &[String],
    ) -> Result<(Vec<ScheduledTask>, Vec<vp_protocol::types::Event>)> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let task_ids = task_ids.to_vec();
        tokio::task::spawn_blocking(move || {
            store.claim_plan_tasks_with_events(&session_id, plan_seq, &task_ids)
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
        let global_enabled = self.global_enabled;
        let default_cooldown_secs = self.default_cooldown_secs;
        let last_session = last_session.map(ToOwned::to_owned);
        let excluded = excluded.to_vec();
        tokio::task::spawn_blocking(move || {
            let now = OffsetDateTime::now_utc();
            let mut candidates = store
                .pending_task_sessions(global_enabled, default_cooldown_secs)?
                .into_iter()
                .filter(|(session_id, _, _, _, _, _, _)| !excluded.contains(session_id))
                .map(
                    |(
                        session_id,
                        project_id,
                        priority,
                        pending,
                        enqueued_at,
                        task_priority,
                        cooldown_secs,
                    )| {
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
                            cooldown_secs,
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

    /// Pick a standalone intake item for the autonomous dispatcher. The
    /// project policy may opt out, cap concurrency, or be disabled entirely.
    /// Candidates are ordered by task priority plus age, with a project
    /// round-robin tie-break, and retries receive the orchestration policy's
    /// priority boost without resetting their attempt budget.
    pub async fn next_pending_intake(
        &self,
        last_project: Option<&str>,
        excluded: &[String],
    ) -> Result<Option<PendingIntake>> {
        let store = self.store.clone();
        let aging = self.aging;
        let global_enabled = self.global_enabled;
        let last_project = last_project.map(ToOwned::to_owned);
        let excluded = excluded.to_vec();
        tokio::task::spawn_blocking(move || {
            let now = OffsetDateTime::now_utc();
            let now_text = now.format(&Rfc3339)?;
            let tasks = store.runnable_intake_tasks(&now_text)?;
            let mut candidates = Vec::new();
            let mut status_cache: HashMap<String, HashMap<String, String>> = HashMap::new();
            for task in tasks {
                if excluded.contains(&task.id) {
                    continue;
                }
                if !status_cache.contains_key(&task.project_id) {
                    let statuses = store
                        .list_intake_tasks(Some(&task.project_id), None, None)?
                        .into_iter()
                        .map(|row| (row.id, row.status))
                        .collect::<HashMap<_, _>>();
                    status_cache.insert(task.project_id.clone(), statuses);
                }
                let dependencies_ready = status_cache
                    .get(&task.project_id)
                    .map(|statuses| {
                        task.deps.iter().all(|dep| {
                            matches!(
                                statuses.get(dep).map(String::as_str),
                                Some("completed") | Some("skipped")
                            )
                        })
                    })
                    .unwrap_or(false);
                if !dependencies_ready {
                    continue;
                }
                let policy = store.get_task_scheduler_policy(&task.project_id)?;
                if !global_enabled && !store.has_task_scheduler_policy(&task.project_id)? {
                    continue;
                }
                if !policy.enabled || !policy.include_intake {
                    continue;
                }
                let active = store.count_active_intake_tasks(&task.project_id)?;
                if active >= policy.max_intake_parallel {
                    continue;
                }
                let orchestration = store.get_orchestration_policy(&task.project_id)?;
                if matches!(task.status.as_str(), "failed" | "interrupted")
                    && (!orchestration.auto_retry_failed || task.attempts >= task.max_attempts)
                {
                    continue;
                }
                let retry_boost = if task.attempts > 0
                    && matches!(task.status.as_str(), "failed" | "interrupted")
                {
                    i64::from(orchestration.retry_priority_boost)
                } else {
                    0
                };
                let effective_priority = intake_effective_priority(&task, now, aging) + retry_boost;
                candidates.push(PendingIntake {
                    task,
                    effective_priority,
                });
            }
            if candidates.is_empty() {
                return Ok(None);
            }
            candidates.sort_by(|left, right| {
                right
                    .effective_priority
                    .cmp(&left.effective_priority)
                    .then_with(|| left.task.created_at.cmp(&right.task.created_at))
                    .then_with(|| left.task.id.cmp(&right.task.id))
            });
            let highest = candidates[0].effective_priority;
            let tied = candidates
                .iter()
                .filter(|candidate| candidate.effective_priority == highest)
                .collect::<Vec<_>>();
            let picked = tied
                .iter()
                .find(|candidate| {
                    Some(candidate.task.project_id.as_str()) != last_project.as_deref()
                })
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
            let progress =
                store.list_plan_task_progress(project_id.as_deref(), session_id.as_deref())?;
            Ok(TaskQueueResult {
                tasks,
                leases,
                progress,
            })
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

fn task_scheduler_global_default() -> bool {
    // The dispatcher is default-on; `0`/`false`/`no` is the explicit opt-out
    // for operators who want a strictly manual workspace.
    env::var("VERLINPARK_TASK_SCHEDULER")
        .ok()
        .map(|value| {
            !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(true)
}

fn task_scheduler_cooldown_default() -> u64 {
    env::var("VERLINPARK_TASK_SCHEDULER_COOLDOWN_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(60)
        .clamp(1, 86_400)
}

pub(crate) fn task_aging() -> Duration {
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

pub(crate) fn is_retryable_status(status: &str) -> bool {
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

fn intake_effective_priority(task: &IntakeTask, now: OffsetDateTime, aging: Duration) -> i64 {
    let base = i64::from(task.priority.clamp(-100, 100));
    if aging.is_zero() {
        return base;
    }
    let age = OffsetDateTime::parse(&task.created_at, &Rfc3339)
        .map(|created| (now - created).whole_seconds().max(0))
        .unwrap_or(0);
    base + (age / aging.as_secs().max(1) as i64).min(100)
}

pub(crate) fn task_effective_priority(
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
        let (claimed, events) = scheduler
            .claim(&session.id, seq, &["high".into()])
            .await
            .unwrap();
        assert_eq!(claimed[0].status, "running");
        assert_eq!(claimed[0].attempts, 1);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "plan_task.started");
        assert_eq!(events[0].payload["task_id"], "high");
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
    async fn auto_resume_requires_a_project_policy() {
        let (_dir, store, project, session) = store();
        store.update_session_status(&session.id, "idle").unwrap();
        let scheduler = TaskScheduler::new_with_aging(store.clone(), Duration::ZERO);
        scheduler
            .sync_plan(&session, &plan(vec![task("T1", 0, vec![])]))
            .await
            .unwrap();
        assert!(
            scheduler
                .next_pending_session(None, &[])
                .await
                .unwrap()
                .is_none()
        );
        store
            .set_task_scheduler_policy(
                &project.id,
                &vp_protocol::types::TaskSchedulerPolicy {
                    enabled: true,
                    cooldown_secs: 20,
                    dispatch_tasks: false,
                    include_intake: true,
                    max_intake_parallel: 2,
                },
            )
            .unwrap();
        let picked = scheduler
            .next_pending_session(None, &[])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(picked.session_id, session.id);
        assert_eq!(picked.cooldown_secs, 20);
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
        let scheduler = TaskScheduler::new_with_aging(store.clone(), Duration::ZERO);
        store
            .set_task_scheduler_policy(
                &project.id,
                &vp_protocol::types::TaskSchedulerPolicy {
                    enabled: true,
                    cooldown_secs: 60,
                    dispatch_tasks: false,
                    include_intake: true,
                    max_intake_parallel: 2,
                },
            )
            .unwrap();
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
        let scheduler = TaskScheduler::new_with_aging(store.clone(), Duration::ZERO);
        store
            .set_task_scheduler_policy(
                &project.id,
                &vp_protocol::types::TaskSchedulerPolicy {
                    enabled: true,
                    cooldown_secs: 60,
                    dispatch_tasks: false,
                    include_intake: true,
                    max_intake_parallel: 2,
                },
            )
            .unwrap();
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
    async fn critical_path_policy_default_is_disabled_and_preserves_existing_order() {
        let (_dir, store, _project, _session) = store();
        let project = store.list_projects().unwrap().remove(0);
        let session = store
            .create_session_with_limits(&project.id, "governed", Some(10_000), None, None)
            .unwrap();
        store
            .append_event(&session.id, "team_plan.submitted", serde_json::json!({}))
            .unwrap();
        let scheduler = TaskScheduler::new_with_aging(store.clone(), Duration::ZERO);
        let plan = plan(vec![task("critical", 0, vec![]), task("side", 100, vec![])]);
        let (seq, _) = scheduler.sync_plan(&session, &plan).await.unwrap();
        let selection = scheduler
            .ready_tasks_governed(&session, seq, &plan, 10, None)
            .await
            .unwrap();
        assert!(selection.governor.is_none());
        assert!(selection.deferred.is_empty());
        assert_eq!(
            selection
                .ready
                .iter()
                .map(|task| task.id.as_str())
                .collect::<Vec<_>>(),
            vec!["side", "critical"]
        );
    }

    #[tokio::test]
    async fn governor_defers_non_critical_work_over_the_reserve() {
        let (_dir, store, _project, _session) = store();
        let project = store.list_projects().unwrap().remove(0);
        let session = store
            .create_session_with_limits(&project.id, "governed", Some(10_000), None, None)
            .unwrap();
        store
            .append_event(&session.id, "team_plan.submitted", serde_json::json!({}))
            .unwrap();
        store
            .set_critical_path_policy(
                &project.id,
                &vp_protocol::types::CriticalPathPolicy {
                    enabled: true,
                    reserve_ratio: 0.25,
                    throttle_at_ratio: 0.0,
                    critical_boost: 25,
                    min_task_budget: 100,
                    ..vp_protocol::types::CriticalPathPolicy::default()
                },
            )
            .unwrap();
        let scheduler = TaskScheduler::new_with_aging(store.clone(), Duration::ZERO);
        let mut critical = task("critical", 0, vec![]);
        critical.token_budget = Some(8_000);
        let mut side = task("side", 100, vec![]);
        side.token_budget = Some(8_000);
        let plan = plan(vec![critical, side]);
        let (seq, _) = scheduler.sync_plan(&session, &plan).await.unwrap();
        let selection = scheduler
            .ready_tasks_governed(&session, seq, &plan, 10, None)
            .await
            .unwrap();
        let governor = selection.governor.as_ref().unwrap();
        assert!(governor.reserve.protected_tokens >= 10_000);
        assert_eq!(selection.ready.len(), 1);
        assert_eq!(selection.ready[0].id, "critical");
        assert_eq!(selection.deferred.len(), 1);
        assert_eq!(selection.deferred[0].task_id, "side");
        assert_eq!(selection.deferred[0].reason, "reserve");
        assert_eq!(selection.deferred[0].code, "BUDGET_RESERVED");
    }

    #[tokio::test]
    async fn governor_admits_critical_work_first_under_pressure() {
        let (_dir, store, _project, _session) = store();
        let project = store.list_projects().unwrap().remove(0);
        let session = store
            .create_session_with_limits(&project.id, "governed", Some(10_000), None, None)
            .unwrap();
        store
            .append_event(&session.id, "team_plan.submitted", serde_json::json!({}))
            .unwrap();
        store
            .set_critical_path_policy(
                &project.id,
                &vp_protocol::types::CriticalPathPolicy {
                    enabled: true,
                    reserve_ratio: 0.5,
                    throttle_at_ratio: 0.0,
                    critical_boost: 0,
                    min_task_budget: 100,
                    ..vp_protocol::types::CriticalPathPolicy::default()
                },
            )
            .unwrap();
        let scheduler = TaskScheduler::new_with_aging(store.clone(), Duration::ZERO);
        let mut critical = task("critical", -50, vec![]);
        critical.token_budget = Some(9_000);
        let mut side = task("side", 100, vec![]);
        side.token_budget = Some(9_000);
        let plan = plan(vec![side, critical]);
        let (seq, _) = scheduler.sync_plan(&session, &plan).await.unwrap();
        let selection = scheduler
            .ready_tasks_governed(&session, seq, &plan, 10, None)
            .await
            .unwrap();
        assert_eq!(selection.ready[0].id, "critical");
        assert_eq!(
            selection.governor.as_ref().unwrap().decisions[0]
                .reason
                .as_str(),
            "critical_admit"
        );
    }

    #[tokio::test]
    async fn multi_path_policy_admits_several_chains_and_defers_the_rest() {
        let (_dir, store, _project, session) = store();
        let project = store.list_projects().unwrap().remove(0);
        store
            .set_critical_path_policy(
                &project.id,
                &vp_protocol::types::CriticalPathPolicy {
                    enabled: true,
                    reserve_ratio: 0.25,
                    throttle_at_ratio: 0.0,
                    critical_boost: 0,
                    min_task_budget: 0,
                    mode: vp_protocol::types::CriticalPathMode::Multi,
                    max_paths: 2,
                    ..vp_protocol::types::CriticalPathPolicy::default()
                },
            )
            .unwrap();
        let scheduler = TaskScheduler::new_with_aging(store.clone(), Duration::ZERO);
        let mut heavy = task("heavy", 0, vec![]);
        heavy.token_budget = Some(10_000);
        let mut medium = task("medium", 0, vec![]);
        medium.token_budget = Some(8_000);
        let mut side = task("side", 0, vec![]);
        side.token_budget = Some(1_000);
        let plan = plan(vec![side, medium, heavy]);
        let (seq, _) = scheduler.sync_plan(&session, &plan).await.unwrap();
        let selection = scheduler
            .ready_tasks_governed(
                &session,
                seq,
                &plan,
                10,
                Some(GovernorBudgetView::new(Some(10_000), 0, 0)),
            )
            .await
            .unwrap();
        let governor = selection.governor.as_ref().unwrap();
        assert_eq!(governor.mode, vp_protocol::types::CriticalPathMode::Multi);
        assert_eq!(governor.path_infos().len(), 2);
        assert_eq!(
            selection
                .ready
                .iter()
                .map(|task| task.id.as_str())
                .collect::<Vec<_>>(),
            vec!["heavy", "medium"]
        );
        assert_eq!(selection.deferred.len(), 1);
        assert_eq!(selection.deferred[0].task_id, "side");
        assert_eq!(selection.deferred[0].code, "BUDGET_RESERVED");
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

#[cfg(test)]
mod intake_tests {
    use super::*;
    use tempfile::TempDir;
    use vp_store::store::NewIntakeTask;

    fn store() -> (TempDir, Store, vp_protocol::types::Project) {
        let dir = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("intake"))
            .unwrap();
        (dir, store, project)
    }

    fn draft(project_id: &str, title: &str, priority: i32) -> NewIntakeTask {
        NewIntakeTask {
            project_id: project_id.to_string(),
            title: title.to_string(),
            instructions: format!("Do {title}."),
            profile: "coding".to_string(),
            kind: "coding".to_string(),
            priority,
            write_scope: vec!["src".to_string()],
            acceptance: vec!["done".to_string()],
            deps: Vec::new(),
            token_budget: None,
            cost_budget_usd: None,
            time_budget_secs: None,
            max_attempts: 2,
            origin: "test".to_string(),
            labels: Vec::new(),
        }
    }

    fn scheduler(store: Store) -> TaskScheduler {
        TaskScheduler::new_with_aging_and_policy(store, Duration::ZERO, true, 60)
    }

    #[tokio::test]
    async fn selects_intake_by_priority_and_round_robins_projects() {
        let (_dir, store, project) = store();
        let repo = TempDir::new().unwrap();
        let second_project = store
            .add_project(repo.path().to_str().unwrap(), Some("second"))
            .unwrap();
        let low = store
            .create_intake_task(&draft(&project.id, "low", -5))
            .unwrap();
        let high = store
            .create_intake_task(&draft(&project.id, "high", 20))
            .unwrap();
        let peer = store
            .create_intake_task(&draft(&second_project.id, "peer", 20))
            .unwrap();
        let scheduler = scheduler(store);
        let picked = scheduler
            .next_pending_intake(None, &[])
            .await
            .unwrap()
            .unwrap();
        assert!(picked.task.id == high.id || picked.task.id == peer.id);
        assert_eq!(picked.effective_priority, 20);
        let picked = scheduler
            .next_pending_intake(Some(&project.id), &[])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(picked.task.id, peer.id);
        let picked = scheduler
            .next_pending_intake(Some(&second_project.id), std::slice::from_ref(&low.id))
            .await
            .unwrap()
            .unwrap();
        assert_ne!(picked.task.id, low.id);
    }

    #[tokio::test]
    async fn intake_policy_can_exclude_and_limit_parallelism() {
        let (_dir, store, project) = store();
        let first = store
            .create_intake_task(&draft(&project.id, "first", 10))
            .unwrap();
        store
            .create_intake_task(&draft(&project.id, "second", 5))
            .unwrap();
        let scheduler = scheduler(store.clone());
        store
            .set_task_scheduler_policy(
                &project.id,
                &vp_protocol::types::TaskSchedulerPolicy {
                    enabled: true,
                    cooldown_secs: 60,
                    dispatch_tasks: false,
                    include_intake: false,
                    max_intake_parallel: 1,
                },
            )
            .unwrap();
        assert!(
            scheduler
                .next_pending_intake(None, &[])
                .await
                .unwrap()
                .is_none()
        );
        store
            .set_task_scheduler_policy(
                &project.id,
                &vp_protocol::types::TaskSchedulerPolicy {
                    enabled: true,
                    cooldown_secs: 60,
                    dispatch_tasks: false,
                    include_intake: true,
                    max_intake_parallel: 1,
                },
            )
            .unwrap();
        let claimed = store.claim_intake_task(&first.id).unwrap().unwrap();
        assert_eq!(claimed.status, "dispatching");
        let started = store
            .start_intake_task(&first.id, "session-1", "T1")
            .unwrap();
        assert_eq!(started.status, "running");
        assert!(
            scheduler
                .next_pending_intake(None, &[])
                .await
                .unwrap()
                .is_none()
        );
        store
            .settle_intake_task(&first.id, "completed", None, None, None)
            .unwrap();
        let picked = scheduler
            .next_pending_intake(None, &[])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(picked.task.title, "second");
    }

    #[tokio::test]
    async fn intake_dependencies_gate_selection() {
        let (_dir, store, project) = store();
        let first = store
            .create_intake_task(&draft(&project.id, "first", 0))
            .unwrap();
        let mut child = draft(&project.id, "child", 100);
        child.deps = vec![first.id.clone()];
        let child = store.create_intake_task(&child).unwrap();
        let scheduler = scheduler(store.clone());
        let picked = scheduler
            .next_pending_intake(None, &[])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(picked.task.id, first.id);
        store.claim_intake_task(&first.id).unwrap();
        store
            .start_intake_task(&first.id, "session-1", "T1")
            .unwrap();
        store
            .settle_intake_task(&first.id, "completed", None, None, None)
            .unwrap();
        let picked = scheduler
            .next_pending_intake(None, &[])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(picked.task.id, child.id);
    }

    #[tokio::test]
    async fn automatic_retry_respects_the_orchestration_policy() {
        let (_dir, store, project) = store();
        let task = store
            .create_intake_task(&draft(&project.id, "no auto retry", 0))
            .unwrap();
        store.claim_intake_task(&task.id).unwrap();
        store
            .settle_intake_task(&task.id, "failed", None, Some("boom"), None)
            .unwrap();
        store
            .set_orchestration_policy(
                &project.id,
                &vp_protocol::types::OrchestrationPolicy {
                    auto_retry_failed: false,
                    max_attempts: 3,
                    retry_backoff_secs: 0,
                    ..Default::default()
                },
            )
            .unwrap();
        let scheduler = scheduler(store.clone());
        assert!(
            scheduler
                .next_pending_intake(None, &[])
                .await
                .unwrap()
                .is_none()
        );
        store.retry_intake_task(&task.id).unwrap();
        assert!(
            scheduler
                .next_pending_intake(None, &[])
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn retry_backoff_and_priority_boost_are_honored() {
        let (_dir, store, project) = store();
        let task = store
            .create_intake_task(&draft(&project.id, "retry me", 0))
            .unwrap();
        store.claim_intake_task(&task.id).unwrap();
        store
            .settle_intake_task(&task.id, "failed", None, Some("boom"), None)
            .unwrap();
        store
            .set_orchestration_policy(
                &project.id,
                &vp_protocol::types::OrchestrationPolicy {
                    auto_retry_failed: true,
                    max_attempts: 3,
                    retry_backoff_secs: 30,
                    retry_priority_boost: 10,
                    ..Default::default()
                },
            )
            .unwrap();
        let scheduler = scheduler(store.clone());
        let picked = scheduler
            .next_pending_intake(None, &[])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(picked.effective_priority, 10);
        store
            .settle_intake_task(
                &task.id,
                "failed",
                None,
                Some("wait"),
                Some("2999-01-01T00:00:00Z"),
            )
            .unwrap();
        assert!(
            scheduler
                .next_pending_intake(None, &[])
                .await
                .unwrap()
                .is_none()
        );
    }
}

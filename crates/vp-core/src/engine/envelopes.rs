//! Opt-in runtime envelope reallocation for task-scoped runs.
//!
//! An explicit project policy may let a Worker/Labour run use token/cost budget
//! capacity that already-completed tasks in the same department did not spend.
//! The result is a runtime effective plan used only for `PlanBudgetScope`; the
//! stored TeamPlan, department/session/project caps, approvals, Verifier, and
//! Integrator are never mutated. Each reallocation is one bounded event.

use super::*;

use std::collections::{HashMap, HashSet};
use vp_protocol::types::TeamPlan;

pub(crate) const SESSION_ENVELOPE_REALLOCATED_EVENT: &str = "session.envelope.reallocated";
pub(crate) const ENVELOPE_REALLOCATION_SCHEMA: &str = "verlinpark.envelope_reallocation/v2";
const MAX_ENVELOPE_EVENTS: u32 = 1_000;
const MAX_ENVELOPE_SOURCES: usize = 100;
const ENVELOPE_REALLOCATION_REASON: &str = "completed_task_unused_envelope";
const ENVELOPE_REALLOCATION_POLICY: &str = "auto_reallocate_envelopes";

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct EffectiveBudget {
    pub(crate) tokens: Option<u64>,
    pub(crate) cost_usd: Option<f64>,
}

#[derive(Debug, serde::Deserialize)]
struct ReallocationEvent {
    #[serde(default)]
    schema: String,
    #[serde(default)]
    target_task_id: String,
    #[serde(default)]
    plan_fingerprint: Option<String>,
    #[serde(default)]
    old_token_budget: Option<u64>,
    #[serde(default)]
    old_cost_budget_usd: Option<f64>,
    #[serde(default)]
    new_token_budget: Option<u64>,
    #[serde(default)]
    new_cost_budget_usd: Option<f64>,
    #[serde(default)]
    sources: Vec<ReallocationSource>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct ReallocationSource {
    #[serde(default)]
    task_id: String,
    #[serde(default)]
    released_tokens: u64,
    #[serde(default)]
    released_cost_usd: f64,
}

#[derive(Debug, Clone, Copy, Default)]
struct ConsumedBudget {
    tokens: u64,
    cost_usd: f64,
}

/// Fold bounded reallocation events over the stored plan budgets. Returns the
/// effective budgets, consumed per-source capacity, and targets that already
/// received a valid override. Stale events (from a prior plan revision) still
/// consume their sources but do not widen the revised plan.
fn fold_reallocation_events(
    plan: &TeamPlan,
    events: Vec<Event>,
) -> (
    HashMap<String, EffectiveBudget>,
    HashMap<String, ConsumedBudget>,
    HashSet<String>,
) {
    let mut effective = plan
        .tasks
        .iter()
        .map(|task| {
            (
                task.id.clone(),
                EffectiveBudget {
                    tokens: task.token_budget.filter(|budget| *budget > 0),
                    cost_usd: task
                        .cost_budget_usd
                        .filter(|budget| budget.is_finite() && *budget > 0.0),
                },
            )
        })
        .collect::<HashMap<_, _>>();
    let mut consumed: HashMap<String, ConsumedBudget> = HashMap::new();
    let mut latest_targets = HashSet::new();
    for event in events {
        let Ok(payload) = serde_json::from_value::<ReallocationEvent>(event.payload) else {
            continue;
        };
        if payload.schema != ENVELOPE_REALLOCATION_SCHEMA
            || payload.target_task_id.trim().is_empty()
        {
            continue;
        }
        // Source capacity is consumed per dimension and stays consumed even
        // when the target plan changed.
        let mut source_ids = Vec::new();
        for source in payload.sources.iter().take(MAX_ENVELOPE_SOURCES) {
            if source.task_id.trim().is_empty() {
                continue;
            }
            source_ids.push(source.task_id.clone());
            let entry = consumed.entry(source.task_id.clone()).or_default();
            entry.tokens = entry.tokens.saturating_add(source.released_tokens);
            if source.released_cost_usd.is_finite() && source.released_cost_usd > 0.0 {
                entry.cost_usd += source.released_cost_usd;
            }
        }
        let fingerprint = plan_fingerprint(plan, &payload.target_task_id, &source_ids);
        if payload.plan_fingerprint.as_deref() != Some(fingerprint.as_str()) {
            continue;
        }
        let current = effective
            .get(&payload.target_task_id)
            .copied()
            .unwrap_or_default();
        if payload.old_token_budget != current.tokens
            || payload.old_cost_budget_usd != current.cost_usd
        {
            continue;
        }
        let reallocated = EffectiveBudget {
            tokens: payload.new_token_budget.filter(|budget| *budget > 0),
            cost_usd: payload
                .new_cost_budget_usd
                .filter(|budget| budget.is_finite() && *budget > 0.0),
        };
        if reallocated.tokens.is_none() && reallocated.cost_usd.is_none() {
            continue;
        }
        latest_targets.insert(payload.target_task_id.clone());
        if let Some(entry) = effective.get_mut(&payload.target_task_id) {
            if reallocated.tokens.is_some() {
                entry.tokens = reallocated.tokens;
            }
            if reallocated.cost_usd.is_some() {
                entry.cost_usd = reallocated.cost_usd;
            }
        }
    }
    (effective, consumed, latest_targets)
}

/// Fingerprint of the target task's budget and its department cap. Unrelated
/// task edits must not invalidate an overlay; a target or department budget
/// revision must.
pub(crate) fn plan_fingerprint(
    plan: &TeamPlan,
    target_task_id: &str,
    source_task_ids: &[String],
) -> String {
    let Some(task) = plan.tasks.iter().find(|task| task.id == target_task_id) else {
        return "missing-target".to_string();
    };
    let department = plan
        .departments
        .iter()
        .find(|department| department.name == task.department);
    let mut parts = vec![format!(
        "t:{}:{}:{:?}:{:?}",
        task.id,
        task.department,
        task.token_budget,
        task.cost_budget_usd.map(|value| value.to_bits())
    )];
    if let Some(department) = department {
        parts.push(format!(
            "d:{}:{:?}:{:?}",
            department.name,
            department.token_budget,
            department.cost_budget_usd.map(|value| value.to_bits())
        ));
    }
    let mut sources = source_task_ids.to_vec();
    sources.sort();
    sources.dedup();
    for source_id in sources {
        if let Some(source) = plan.tasks.iter().find(|task| task.id == source_id) {
            parts.push(format!(
                "s:{}:{}:{:?}:{:?}",
                source.id,
                source.department,
                source.token_budget,
                source.cost_budget_usd.map(|value| value.to_bits())
            ));
        } else {
            parts.push(format!("s:{source_id}:missing"));
        }
    }
    crate::sha256_hex(parts.join("\n").as_bytes())
}

/// Only normal implementation runs may append new reallocations. Verifier,
/// Scout, Manager, and auto-approve maintenance runs may read the overlay.
pub(crate) fn may_append_envelope_reallocation(
    role: crate::agent::AgentRole,
    auto_approve: bool,
) -> bool {
    matches!(
        role,
        crate::agent::AgentRole::Worker | crate::agent::AgentRole::Labour
    ) && !auto_approve
}

impl Engine {
    /// Read-only effective budgets for the allocation preview. Historical
    /// reallocation events still apply even when the policy is currently off;
    /// the stored plan and caps are never changed.
    pub(crate) async fn runtime_envelope_budgets(
        &self,
        session: &Session,
        plan: &TeamPlan,
    ) -> Result<HashMap<String, EffectiveBudget>> {
        let events = self
            .recent_events_of_kind(
                &session.id,
                SESSION_ENVELOPE_REALLOCATED_EVENT,
                MAX_ENVELOPE_EVENTS,
            )
            .await?;
        Ok(fold_reallocation_events(plan, events).0)
    }

    /// Read-only overlay for any task-scoped role (including Verifier/Scout).
    /// Historical reallocation events still apply when the policy is off, so a
    /// task that already spent reallocated capacity can still be verified.
    pub(crate) async fn runtime_effective_plan(
        &self,
        session: &Session,
        plan: &TeamPlan,
        task_id: &str,
    ) -> Result<Option<TeamPlan>> {
        let events = self
            .recent_events_of_kind(
                &session.id,
                SESSION_ENVELOPE_REALLOCATED_EVENT,
                MAX_ENVELOPE_EVENTS,
            )
            .await?;
        let (effective, _consumed, latest_targets) = fold_reallocation_events(plan, events);
        if !latest_targets.contains(task_id) {
            return Ok(None);
        }
        let Some(budget) = effective.get(task_id).copied() else {
            return Ok(None);
        };
        let mut runtime_plan = plan.clone();
        apply_effective_budget(&mut runtime_plan, task_id, budget);
        Ok(Some(runtime_plan))
    }

    /// Return a runtime-only plan whose target task budget includes unused
    /// completed-task capacity in the same department, or `None` when the
    /// policy is off, the target has no positive budget, nothing is available,
    /// the session is an imported archive, or the target is unmanaged.
    ///
    /// A target that already has a reallocation event replays its latest new
    /// budgets without appending another event. Sources are single-use across
    /// the bounded event window; a completed task that previously received a
    /// reallocated budget is itself a source with that effective budget.
    pub(crate) async fn effective_plan_for_budget(
        &self,
        session: &Session,
        plan: &TeamPlan,
        task_id: &str,
    ) -> Result<Option<TeamPlan>> {
        if session.status == "imported" {
            return Ok(None);
        }
        let policy = self
            .get_project_run_policy(&session.project_id)
            .await?
            .policy;
        if !policy.auto_reallocate_envelopes {
            return Ok(None);
        }
        let Some(target) = plan.tasks.iter().find(|task| task.id == task_id) else {
            return Ok(None);
        };
        let old_token = target.token_budget.filter(|budget| *budget > 0);
        let old_cost = target
            .cost_budget_usd
            .filter(|budget| budget.is_finite() && *budget > 0.0);
        if old_token.is_none() && old_cost.is_none() {
            return Ok(None);
        }

        // Serialize the event read + source selection + append per session.
        // Concurrent task runs in one session otherwise could both consume the
        // same completed source or append two events for one target; other
        // sessions/projects remain parallel.
        let envelope_lock = self.workspace_run_lock(&format!("envelope:{}", session.id));
        let _guard = envelope_lock.lock().await;
        let events = self
            .recent_events_of_kind(
                &session.id,
                SESSION_ENVELOPE_REALLOCATED_EVENT,
                MAX_ENVELOPE_EVENTS,
            )
            .await?;
        let (effective, consumed, latest_targets) = fold_reallocation_events(plan, events);
        let (target_actual_tokens, target_actual_cost, _, _) =
            self.usage_spent(&session.id, Some(task_id), None).await?;

        // Idempotent replay: the target already received a reallocation and
        // still has headroom. If that cap is exhausted, fall through to allow
        // a new top-up event from sources that have not been consumed yet.
        if latest_targets.contains(task_id) {
            let current = effective.get(task_id).copied().unwrap_or_default();
            let token_exhausted = current
                .tokens
                .is_some_and(|cap| target_actual_tokens >= cap);
            let cost_exhausted = current
                .cost_usd
                .is_some_and(|cap| target_actual_cost >= cap);
            if !token_exhausted && !cost_exhausted {
                let mut runtime_plan = plan.clone();
                apply_effective_budget(&mut runtime_plan, task_id, current);
                return Ok(Some(runtime_plan));
            }
        }

        let statuses = self
            .task_queue(None, Some(session.id.clone()))
            .await?
            .tasks
            .into_iter()
            .map(|task| (task.task_id, task.status))
            .collect::<HashMap<_, _>>();
        if statuses
            .get(task_id)
            .is_some_and(|status| matches!(status.as_str(), "completed" | "skipped" | "cancelled"))
        {
            return Ok(None);
        }
        let department = plan
            .departments
            .iter()
            .find(|department| department.name == target.department);
        let department_tokens = department
            .and_then(|department| department.token_budget)
            .filter(|budget| *budget > 0);
        let department_cost = department
            .and_then(|department| department.cost_budget_usd)
            .filter(|budget| budget.is_finite() && *budget > 0.0);

        // A retry may already have overshot the stored task cap. The new cap
        // must cover observed spend, or the event would be immediately
        // unusable and would consume sources for nothing.
        let base_token = old_token.map(|old| old.max(target_actual_tokens));
        let base_cost = old_cost.map(|old| old.max(target_actual_cost.max(0.0)));
        let token_room = base_token
            .map(|base| {
                department_tokens
                    .map(|cap| cap.saturating_sub(base))
                    .unwrap_or(u64::MAX)
            })
            .unwrap_or(0);
        let cost_room = base_cost
            .map(|base| {
                department_cost
                    .map(|cap| (cap - base).max(0.0))
                    .unwrap_or(f64::MAX)
            })
            .unwrap_or(0.0);
        let mut released_tokens = 0u64;
        let mut released_cost = 0.0f64;
        let mut sources = Vec::new();
        let mut source_ids = Vec::new();
        for candidate in &plan.tasks {
            if candidate.id == task_id || candidate.department != target.department {
                continue;
            }
            if statuses.get(&candidate.id).map(String::as_str) != Some("completed") {
                continue;
            }
            if released_tokens >= token_room && released_cost >= cost_room {
                break;
            }
            let effective_budget = effective.get(&candidate.id).copied().unwrap_or_default();
            let cap_tokens = old_token.and(effective_budget.tokens);
            let cap_cost = old_cost.and(effective_budget.cost_usd);
            if cap_tokens.is_none() && cap_cost.is_none() {
                continue;
            }
            let already = consumed.get(&candidate.id).copied().unwrap_or_default();
            let (actual_tokens, actual_cost, _, _) = self
                .usage_spent(&session.id, Some(&candidate.id), None)
                .await?;
            let available_tokens = cap_tokens
                .map(|cap| {
                    cap.saturating_sub(actual_tokens)
                        .saturating_sub(already.tokens)
                })
                .unwrap_or(0);
            let available_cost = cap_cost
                .map(|cap| (cap - actual_cost.max(0.0) - already.cost_usd).max(0.0))
                .filter(|cost| cost.is_finite())
                .unwrap_or(0.0);
            let take_tokens = available_tokens.min(token_room.saturating_sub(released_tokens));
            let take_cost = available_cost.min((cost_room - released_cost).max(0.0));
            if take_tokens == 0 && take_cost <= 0.0 {
                continue;
            }
            released_tokens = released_tokens.saturating_add(take_tokens);
            released_cost += take_cost;
            source_ids.push(candidate.id.clone());
            sources.push(serde_json::json!({
                "task_id": candidate.id,
                "released_tokens": take_tokens,
                "released_cost_usd": take_cost,
            }));
            if sources.len() >= MAX_ENVELOPE_SOURCES {
                break;
            }
        }
        if sources.is_empty() {
            return Ok(None);
        }

        let new_token = base_token.map(|base| base.saturating_add(released_tokens));
        let new_cost = base_cost.map(|base| {
            let proposed = base + released_cost;
            if proposed.is_finite() { proposed } else { base }
        });
        if new_token == old_token && new_cost == old_cost {
            return Ok(None);
        }

        self.append_event(
            &session.id,
            SESSION_ENVELOPE_REALLOCATED_EVENT,
            serde_json::json!({
                "schema": ENVELOPE_REALLOCATION_SCHEMA,
                "target_task_id": target.id,
                "department": target.department,
                "plan_fingerprint": plan_fingerprint(plan, &target.id, &source_ids),
                "old_token_budget": old_token,
                "new_token_budget": new_token,
                "old_cost_budget_usd": old_cost,
                "new_cost_budget_usd": new_cost,
                "sources": sources,
                "released_tokens": released_tokens,
                "released_cost_usd": released_cost,
                "applied_tokens": new_token
                    .zip(old_token)
                    .map(|(new, old)| new.saturating_sub(old))
                    .unwrap_or(0),
                "applied_cost_usd": new_cost
                    .zip(old_cost)
                    .map(|(new, old)| (new - old).max(0.0))
                    .unwrap_or(0.0),
                "generated_at": now_rfc3339(),
                "reason": ENVELOPE_REALLOCATION_REASON,
                "policy": ENVELOPE_REALLOCATION_POLICY,
            }),
        )
        .await?;

        let mut runtime_plan = plan.clone();
        if let Some(task) = runtime_plan
            .tasks
            .iter_mut()
            .find(|task| task.id == task_id)
        {
            if old_token.is_some() {
                task.token_budget = new_token;
            }
            if old_cost.is_some() {
                task.cost_budget_usd = new_cost;
            }
        }
        Ok(Some(runtime_plan))
    }
}

fn apply_effective_budget(plan: &mut TeamPlan, task_id: &str, budget: EffectiveBudget) {
    let Some(task) = plan.tasks.iter_mut().find(|task| task.id == task_id) else {
        return;
    };
    if let Some(tokens) = budget.tokens {
        task.token_budget = Some(tokens);
    }
    if let Some(cost) = budget.cost_usd {
        task.cost_budget_usd = Some(cost);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;
    use tempfile::TempDir;
    use vp_protocol::types::{
        PlanDepartment, PlanTask, ProjectRunPolicy, SetProjectRunPolicyParams, TeamPlan,
    };

    fn init_repo(path: &Path) {
        std::fs::write(path.join("README.md"), "hello\n").unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "test@verlinpark.local"],
            vec!["config", "user.name", "VerlinPark Test"],
            vec!["add", "."],
            vec!["commit", "-q", "-m", "initial"],
        ] {
            let status = Command::new("git")
                .arg("-C")
                .arg(path)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success());
        }
    }

    fn department(name: &str, tokens: Option<u64>, cost: Option<f64>) -> PlanDepartment {
        PlanDepartment {
            name: name.into(),
            manager_title: format!("{name} manager"),
            profile: "coding".into(),
            objective: "build".into(),
            token_budget: tokens,
            cost_budget_usd: cost,
        }
    }

    fn task(id: &str, department: &str, tokens: Option<u64>, cost: Option<f64>) -> PlanTask {
        PlanTask {
            id: id.into(),
            department: department.into(),
            title: format!("Task {id}"),
            kind: "coding".into(),
            priority: 1,
            profile: Some("coding".into()),
            deps: Vec::new(),
            write_scope: vec![format!("src/{id}.rs")],
            acceptance: vec!["tests pass".into()],
            token_budget: tokens,
            cost_budget_usd: cost,
            parent_task_id: None,
            fanout_key: None,
            fanout_join_for: None,
        }
    }

    fn plan(departments: Vec<PlanDepartment>, tasks: Vec<PlanTask>) -> TeamPlan {
        TeamPlan {
            goal: "envelopes".into(),
            departments,
            tasks,
        }
    }

    async fn set_auto_reallocation(engine: &Engine, project_id: &str, enabled: bool) {
        let current = engine
            .get_project_run_policy(project_id)
            .await
            .unwrap()
            .policy;
        engine
            .set_project_run_policy(SetProjectRunPolicyParams {
                project_id: project_id.to_string(),
                policy: ProjectRunPolicy {
                    auto_reallocate_envelopes: enabled,
                    ..current
                },
            })
            .await
            .unwrap();
    }

    async fn setup(plan: &TeamPlan, enabled: bool) -> (TempDir, TempDir, Engine, Session) {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("envelopes"))
            .unwrap();
        let session = store.create_session(&project.id, "envelopes").unwrap();
        let engine = Engine::new(store);
        engine.submit_team_plan(&session.id, plan).await.unwrap();
        set_auto_reallocation(&engine, &project.id, enabled).await;
        (data, repo, engine, session)
    }

    fn complete_task(engine: &Engine, session_id: &str, task_id: &str) {
        let store = engine.store();
        let plan_seq = store
            .latest_event_seq(session_id, "team_plan.submitted")
            .unwrap();
        store
            .claim_plan_tasks_with_events(session_id, plan_seq, &[task_id.to_string()])
            .unwrap();
        store
            .finish_plan_task(session_id, plan_seq, task_id, "completed", None)
            .unwrap()
            .expect("task must transition to completed");
        store
            .append_event(
                session_id,
                "plan_task.finished",
                serde_json::json!({
                    "task_id": task_id,
                    "task_status": "completed",
                    "summary": "done",
                }),
            )
            .unwrap();
    }

    async fn record_usage(
        engine: &Engine,
        session_id: &str,
        task_id: &str,
        department: &str,
        input: u64,
        output: u64,
        cost: f64,
    ) {
        engine
            .record_agent_usage(
                session_id,
                "worker-1",
                "Worker",
                "worker",
                Some(task_id.to_string()),
                Some(department.to_string()),
                "test-model",
                input,
                output,
                false,
                cost,
            )
            .await
            .unwrap();
    }

    async fn event_count(engine: &Engine, session_id: &str) -> u64 {
        engine
            .count_events_of_kind(session_id, SESSION_ENVELOPE_REALLOCATED_EVENT)
            .await
            .unwrap()
    }

    #[test]
    fn only_normal_implementation_runs_may_append_reallocations() {
        assert!(may_append_envelope_reallocation(
            crate::agent::AgentRole::Worker,
            false
        ));
        assert!(may_append_envelope_reallocation(
            crate::agent::AgentRole::Labour,
            false
        ));
        assert!(!may_append_envelope_reallocation(
            crate::agent::AgentRole::Worker,
            true
        ));
        assert!(!may_append_envelope_reallocation(
            crate::agent::AgentRole::Verifier,
            false
        ));
        assert!(!may_append_envelope_reallocation(
            crate::agent::AgentRole::Scout,
            false
        ));
    }

    #[tokio::test]
    async fn completed_task_releases_unused_token_and_cost_budget_once() {
        let plan = plan(
            vec![department("impl", None, None)],
            vec![
                task("T1", "impl", Some(5_000), Some(1.0)),
                task("T2", "impl", Some(1_000), Some(0.2)),
            ],
        );
        let (_data, _repo, engine, session) = setup(&plan, true).await;
        complete_task(&engine, &session.id, "T1");
        record_usage(&engine, &session.id, "T1", "impl", 1_000, 500, 0.25).await;

        let runtime = engine
            .effective_plan_for_budget(&session, &plan, "T2")
            .await
            .unwrap()
            .expect("completed T1 releases unused capacity");
        let target = runtime.tasks.iter().find(|task| task.id == "T2").unwrap();
        assert_eq!(target.token_budget, Some(4_500));
        assert_eq!(target.cost_budget_usd, Some(0.95));
        assert_eq!(event_count(&engine, &session.id).await, 1);

        let stored = engine.get_team_plan(&session.id).await.unwrap().unwrap();
        let stored_target = stored.tasks.iter().find(|task| task.id == "T2").unwrap();
        assert_eq!(stored_target.token_budget, Some(1_000));
        assert_eq!(stored_target.cost_budget_usd, Some(0.2));

        let event = engine
            .latest_event_of_kind(&session.id, SESSION_ENVELOPE_REALLOCATED_EVENT)
            .await
            .unwrap()
            .expect("reallocation event");
        assert_eq!(event.payload["schema"], ENVELOPE_REALLOCATION_SCHEMA);
        assert_eq!(event.payload["target_task_id"], "T2");
        assert_eq!(event.payload["department"], "impl");
        assert_eq!(event.payload["old_token_budget"], 1_000);
        assert_eq!(event.payload["new_token_budget"], 4_500);
        assert_eq!(event.payload["released_tokens"], 3_500);
        assert_eq!(event.payload["sources"][0]["task_id"], "T1");
        assert_eq!(event.payload["reason"], ENVELOPE_REALLOCATION_REASON);
        assert_eq!(event.payload["policy"], ENVELOPE_REALLOCATION_POLICY);

        let replay = engine
            .effective_plan_for_budget(&session, &plan, "T2")
            .await
            .unwrap()
            .expect("the event replays idempotently");
        assert_eq!(replay, runtime);
        assert_eq!(
            event_count(&engine, &session.id).await,
            1,
            "a repeated call must not append a second event"
        );
    }

    #[tokio::test]
    async fn read_only_overlay_survives_policy_disable_for_verifier_and_scout() {
        let plan = plan(
            vec![department("impl", None, None)],
            vec![
                task("T1", "impl", Some(5_000), None),
                task("T2", "impl", Some(1_000), None),
            ],
        );
        let (_data, _repo, engine, session) = setup(&plan, true).await;
        complete_task(&engine, &session.id, "T1");
        engine
            .effective_plan_for_budget(&session, &plan, "T2")
            .await
            .unwrap()
            .expect("new reallocation");
        set_auto_reallocation(&engine, &session.project_id, false).await;
        let runtime = engine
            .runtime_effective_plan(&session, &plan, "T2")
            .await
            .unwrap()
            .expect("recorded overlay must remain visible when policy is off");
        assert_eq!(
            runtime
                .tasks
                .iter()
                .find(|task| task.id == "T2")
                .unwrap()
                .token_budget,
            Some(6_000)
        );
        assert_eq!(event_count(&engine, &session.id).await, 1);
    }

    #[tokio::test]
    async fn dimensions_without_a_target_cap_are_not_invented() {
        let plan = plan(
            vec![department("impl", None, None)],
            vec![
                task("T1", "impl", Some(5_000), Some(1.0)),
                task("T2", "impl", None, Some(0.2)),
            ],
        );
        let (_data, _repo, engine, session) = setup(&plan, true).await;
        complete_task(&engine, &session.id, "T1");
        record_usage(&engine, &session.id, "T1", "impl", 1_000, 0, 0.25).await;
        let runtime = engine
            .effective_plan_for_budget(&session, &plan, "T2")
            .await
            .unwrap()
            .expect("the target has a cost budget");
        let target = runtime.tasks.iter().find(|task| task.id == "T2").unwrap();
        assert_eq!(target.token_budget, None);
        assert_eq!(target.cost_budget_usd, Some(0.95));
        let event = engine
            .latest_event_of_kind(&session.id, SESSION_ENVELOPE_REALLOCATED_EVENT)
            .await
            .unwrap()
            .expect("reallocation event");
        assert_eq!(event.payload["new_token_budget"], serde_json::Value::Null);
        assert_eq!(event.payload["sources"][0]["released_tokens"], 0);
    }

    #[tokio::test]
    async fn used_sources_are_not_reused_and_reallocated_targets_become_effective_sources() {
        let plan = plan(
            vec![department("impl", None, None)],
            vec![
                task("T1", "impl", Some(4_000), None),
                task("T2", "impl", Some(1_000), None),
                task("T3", "impl", Some(1_000), None),
            ],
        );
        let (_data, _repo, engine, session) = setup(&plan, true).await;
        complete_task(&engine, &session.id, "T1");
        record_usage(&engine, &session.id, "T1", "impl", 1_000, 0, 0.0).await;
        let runtime = engine
            .effective_plan_for_budget(&session, &plan, "T2")
            .await
            .unwrap()
            .expect("T1 funds T2");
        assert_eq!(
            runtime
                .tasks
                .iter()
                .find(|task| task.id == "T2")
                .unwrap()
                .token_budget,
            Some(4_000)
        );

        complete_task(&engine, &session.id, "T2");
        record_usage(&engine, &session.id, "T2", "impl", 500, 0, 0.0).await;
        let runtime = engine
            .effective_plan_for_budget(&session, &plan, "T3")
            .await
            .unwrap()
            .expect("completed T2 uses its reallocated effective budget");
        let target = runtime.tasks.iter().find(|task| task.id == "T3").unwrap();
        assert_eq!(target.token_budget, Some(4_500));
        let event = engine
            .latest_event_of_kind(&session.id, SESSION_ENVELOPE_REALLOCATED_EVENT)
            .await
            .unwrap()
            .expect("second reallocation event");
        let sources = event.payload["sources"].as_array().unwrap();
        assert_eq!(sources.len(), 1, "T1 is already used and cannot be reused");
        assert_eq!(sources[0]["task_id"], "T2");
        assert_eq!(sources[0]["released_tokens"], 3_500);
        assert_eq!(event_count(&engine, &session.id).await, 2);
    }

    #[tokio::test]
    async fn a_retry_envelope_covers_already_observed_spend() {
        let plan = plan(
            vec![department("impl", None, None)],
            vec![
                task("T1", "impl", Some(1_000), None),
                task("T2", "impl", Some(1_000), None),
            ],
        );
        let (_data, _repo, engine, session) = setup(&plan, true).await;
        complete_task(&engine, &session.id, "T1");
        record_usage(&engine, &session.id, "T2", "impl", 2_500, 0, 0.0).await;
        let runtime = engine
            .effective_plan_for_budget(&session, &plan, "T2")
            .await
            .unwrap()
            .expect("retry gets a usable cap");
        assert_eq!(
            runtime
                .tasks
                .iter()
                .find(|task| task.id == "T2")
                .unwrap()
                .token_budget,
            Some(3_500),
            "base must cover observed 2.5k plus released 1k"
        );
        let event = engine
            .latest_event_of_kind(&session.id, SESSION_ENVELOPE_REALLOCATED_EVENT)
            .await
            .unwrap()
            .expect("event");
        assert_eq!(event.payload["applied_tokens"], 2_500);
        assert_eq!(event.payload["released_tokens"], 1_000);
    }

    #[tokio::test]
    async fn an_exhausted_overlay_can_be_topped_up_from_new_sources() {
        let plan = plan(
            vec![department("impl", None, None)],
            vec![
                task("T1", "impl", Some(5_000), None),
                task("T2", "impl", Some(1_000), None),
                task("T3", "impl", Some(1_000), None),
            ],
        );
        let (_data, _repo, engine, session) = setup(&plan, true).await;
        complete_task(&engine, &session.id, "T1");
        let first = engine
            .effective_plan_for_budget(&session, &plan, "T2")
            .await
            .unwrap()
            .expect("first reallocation");
        assert_eq!(
            first
                .tasks
                .iter()
                .find(|task| task.id == "T2")
                .unwrap()
                .token_budget,
            Some(6_000)
        );
        record_usage(&engine, &session.id, "T2", "impl", 6_000, 0, 0.0).await;
        complete_task(&engine, &session.id, "T3");

        let topped_up = engine
            .effective_plan_for_budget(&session, &plan, "T2")
            .await
            .unwrap()
            .expect("new T3 capacity tops up the exhausted target");
        assert_eq!(
            topped_up
                .tasks
                .iter()
                .find(|task| task.id == "T2")
                .unwrap()
                .token_budget,
            Some(7_000)
        );
        assert_eq!(event_count(&engine, &session.id).await, 2);
    }

    #[tokio::test]
    async fn source_dimensions_are_consumed_independently() {
        let plan = plan(
            vec![department("impl", None, None)],
            vec![
                task("T1", "impl", Some(5_000), Some(1.0)),
                task("T2", "impl", None, Some(0.2)),
                task("T3", "impl", Some(1_000), None),
            ],
        );
        let (_data, _repo, engine, session) = setup(&plan, true).await;
        complete_task(&engine, &session.id, "T1");
        record_usage(&engine, &session.id, "T1", "impl", 1_000, 0, 0.25).await;

        let cost_only = engine
            .effective_plan_for_budget(&session, &plan, "T2")
            .await
            .unwrap()
            .expect("cost-only target uses cost capacity");
        assert_eq!(
            cost_only
                .tasks
                .iter()
                .find(|task| task.id == "T2")
                .unwrap()
                .cost_budget_usd,
            Some(0.95)
        );
        let event = engine
            .latest_event_of_kind(&session.id, SESSION_ENVELOPE_REALLOCATED_EVENT)
            .await
            .unwrap()
            .expect("cost event");
        assert_eq!(event.payload["sources"][0]["released_tokens"], 0);
        assert_eq!(event.payload["sources"][0]["released_cost_usd"], 0.75);

        let token_only = engine
            .effective_plan_for_budget(&session, &plan, "T3")
            .await
            .unwrap()
            .expect("token capacity was not burned by the cost-only target");
        assert_eq!(
            token_only
                .tasks
                .iter()
                .find(|task| task.id == "T3")
                .unwrap()
                .token_budget,
            Some(5_000)
        );
    }

    #[tokio::test]
    async fn department_isolation_prevents_cross_department_sources() {
        let plan = plan(
            vec![
                department("impl", Some(6_000), None),
                department("other", Some(10_000), None),
            ],
            vec![
                task("T1", "impl", Some(3_000), None),
                task("T2", "impl", Some(1_000), None),
                task("T3", "other", Some(2_000), None),
            ],
        );
        let (_data, _repo, engine, session) = setup(&plan, true).await;
        complete_task(&engine, &session.id, "T1");
        complete_task(&engine, &session.id, "T3");
        let runtime = engine
            .effective_plan_for_budget(&session, &plan, "T2")
            .await
            .unwrap()
            .expect("same-department completed capacity is available");
        assert_eq!(
            runtime
                .tasks
                .iter()
                .find(|task| task.id == "T2")
                .unwrap()
                .token_budget,
            Some(4_000)
        );
        let event = engine
            .latest_event_of_kind(&session.id, SESSION_ENVELOPE_REALLOCATED_EVENT)
            .await
            .unwrap()
            .expect("reallocation event");
        let sources = event.payload["sources"].as_array().unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0]["task_id"], "T1");
    }

    #[tokio::test]
    async fn department_budget_caps_the_runtime_target_budget() {
        let stored_plan = plan(
            vec![department("impl", Some(6_000), None)],
            vec![
                task("T1", "impl", Some(3_000), None),
                task("T2", "impl", Some(1_000), None),
            ],
        );
        let (_data, _repo, engine, session) = setup(&stored_plan, true).await;
        complete_task(&engine, &session.id, "T1");

        // A synthetic plan exercises the cap independently of TeamPlan
        // validation; the task queue status still comes from the stored plan.
        let capped_plan = plan(
            vec![department("impl", Some(6_000), None)],
            vec![
                task("T1", "impl", Some(6_000), None),
                task("T2", "impl", Some(1_000), None),
            ],
        );
        let runtime = engine
            .effective_plan_for_budget(&session, &capped_plan, "T2")
            .await
            .unwrap()
            .expect("capacity is capped, not rejected");
        assert_eq!(
            runtime
                .tasks
                .iter()
                .find(|task| task.id == "T2")
                .unwrap()
                .token_budget,
            Some(6_000)
        );
        let event = engine
            .latest_event_of_kind(&session.id, SESSION_ENVELOPE_REALLOCATED_EVENT)
            .await
            .unwrap()
            .expect("reallocation event");
        assert_eq!(event.payload["new_token_budget"], 6_000);
        assert_eq!(
            event.payload["sources"][0]["released_tokens"], 5_000,
            "only the department-cap room is applied, not the source's full 6k"
        );
        assert_eq!(event.payload["released_tokens"], 5_000);
    }

    #[tokio::test]
    async fn a_plan_revision_cannot_replay_a_wider_old_override() {
        let original = plan(
            vec![department("impl", None, None)],
            vec![
                task("T1", "impl", Some(5_000), None),
                task("T2", "impl", Some(1_000), None),
            ],
        );
        let (_data, _repo, engine, session) = setup(&original, true).await;
        complete_task(&engine, &session.id, "T1");
        let runtime = engine
            .effective_plan_for_budget(&session, &original, "T2")
            .await
            .unwrap()
            .expect("T1 funds T2");
        assert_eq!(
            runtime
                .tasks
                .iter()
                .find(|task| task.id == "T2")
                .unwrap()
                .token_budget,
            Some(6_000)
        );

        let unrelated_changed = plan(
            vec![department("impl", None, None)],
            vec![
                task("T1", "impl", Some(5_000), None),
                task("T2", "impl", Some(1_000), None),
                task("T3", "impl", Some(1_000), None),
            ],
        );
        let replay = engine
            .effective_plan_for_budget(&session, &unrelated_changed, "T2")
            .await
            .unwrap()
            .expect("an unrelated task edit must not invalidate T2's overlay");
        assert_eq!(
            replay
                .tasks
                .iter()
                .find(|task| task.id == "T2")
                .unwrap()
                .token_budget,
            Some(6_000)
        );

        let source_changed = plan(
            vec![department("impl", None, None)],
            vec![
                task("T1", "impl", Some(4_000), None),
                task("T2", "impl", Some(1_000), None),
                task("T3", "impl", Some(1_000), None),
            ],
        );
        assert!(
            engine
                .effective_plan_for_budget(&session, &source_changed, "T2")
                .await
                .unwrap()
                .is_none(),
            "a source-budget revision must invalidate the old target overlay"
        );

        let narrowed = plan(
            vec![department("impl", None, None)],
            vec![
                task("T1", "impl", Some(5_000), None),
                task("T2", "impl", Some(500), None),
            ],
        );
        assert!(
            engine
                .effective_plan_for_budget(&session, &narrowed, "T2")
                .await
                .unwrap()
                .is_none(),
            "the old 6k override must not widen the revised 500-token plan"
        );
        assert_eq!(event_count(&engine, &session.id).await, 1);

        let department_changed = plan(
            vec![department("impl", Some(3_000), None)],
            vec![
                task("T1", "impl", Some(5_000), None),
                task("T2", "impl", Some(1_000), None),
            ],
        );
        assert!(
            engine
                .effective_plan_for_budget(&session, &department_changed, "T2")
                .await
                .unwrap()
                .is_none(),
            "a department-cap revision must invalidate the old runtime override"
        );
        assert_eq!(event_count(&engine, &session.id).await, 1);
    }

    #[tokio::test]
    async fn policy_off_no_target_budget_and_imported_sessions_return_none() {
        let stored_plan = plan(
            vec![department("impl", None, None)],
            vec![
                task("T1", "impl", Some(5_000), None),
                task("T2", "impl", Some(1_000), None),
            ],
        );
        let (_data, _repo, engine, session) = setup(&stored_plan, false).await;
        complete_task(&engine, &session.id, "T1");
        assert!(
            engine
                .effective_plan_for_budget(&session, &stored_plan, "T2")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(event_count(&engine, &session.id).await, 0);

        let (_data, _repo, engine, session) = setup(&stored_plan, true).await;
        complete_task(&engine, &session.id, "T1");
        let no_budget = plan(
            vec![department("impl", None, None)],
            vec![
                task("T1", "impl", Some(5_000), None),
                task("T2", "impl", None, None),
            ],
        );
        assert!(
            engine
                .effective_plan_for_budget(&session, &no_budget, "T2")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(event_count(&engine, &session.id).await, 0);

        engine
            .store()
            .update_session_status(&session.id, "imported")
            .unwrap();
        let imported = engine
            .store()
            .get_session(&session.id)
            .unwrap()
            .expect("session");
        assert!(
            engine
                .effective_plan_for_budget(&imported, &stored_plan, "T2")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(event_count(&engine, &session.id).await, 0);
    }
}

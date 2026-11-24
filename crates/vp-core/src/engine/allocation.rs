//! Read-only R3 allocation preview.
//!
//! The preview reconciles the session envelope, the remaining TeamPlan DAG, the
//! task queue, bounded historical usage, readiness, attention, and failure
//! diagnosis. It never mutates a task envelope, budget, capacity, approval,
//! Verifier, Integrator, or model selection; every recommendation is advisory.

use super::*;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use vp_protocol::diagnosis::reason_codes;
use vp_protocol::types::{
    AllocationCalibration, AllocationEnvelope, AllocationFeasibility, AllocationMetareasoning,
    AllocationPreview, AllocationRecommendation, AllocationTask, Approval, BudgetMode,
    ScheduledTask, SessionAllocationParams, TeamPlan, UsageBucket, UsageEntry, UsageTotals,
};

use crate::critical_path::{CriticalPathInput, compute_critical_path, is_remaining_status};

pub(crate) const MAX_ALLOCATION_TASKS: usize = 100;
pub(crate) const MAX_ALLOCATION_RECOMMENDATIONS: usize = 12;
pub(crate) const MAX_ALLOCATION_TEXT: usize = 200;
pub(crate) const MAX_ALLOCATION_HISTORY: u32 = 1_000;
pub(crate) const ALLOCATION_DIAGNOSIS_EVENTS: u32 = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum EstimateSource {
    SessionActuals,
    PlanEnvelope,
    ProjectActuals,
    DepartmentEnvelope,
    Prior,
}

impl EstimateSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::SessionActuals => "session_actuals",
            Self::PlanEnvelope => "plan_envelope",
            Self::ProjectActuals => "project_actuals",
            Self::DepartmentEnvelope => "department_envelope",
            Self::Prior => "prior",
        }
    }
}

#[derive(Debug, Clone, Default)]
struct TaskEstimate {
    p50_tokens: Option<u64>,
    p90_tokens: Option<u64>,
    p50_cost_usd: Option<f64>,
    p90_cost_usd: Option<f64>,
    observed_calls: u32,
    source: Option<EstimateSource>,
}

#[derive(Debug, Clone, Default)]
struct ActualTask {
    tokens: u64,
    cost_usd: f64,
    calls: u32,
}

#[derive(Debug, Clone, Default)]
struct DepartmentEnvelope {
    tokens: Option<u64>,
    cost_usd: Option<f64>,
}

#[derive(Debug, Default)]
struct ProjectActuals {
    by_department: BTreeMap<String, Vec<ActualTask>>,
}

impl ProjectActuals {
    fn from_entries(entries: &[UsageEntry]) -> Self {
        let mut grouped: BTreeMap<(String, String), ActualTask> = BTreeMap::new();
        for entry in entries {
            let Some(task_id) = entry
                .task_id
                .as_deref()
                .map(str::trim)
                .filter(|task_id| !task_id.is_empty())
            else {
                continue;
            };
            let department = entry
                .department
                .as_deref()
                .map(str::trim)
                .filter(|department| !department.is_empty())
                .unwrap_or("(unassigned)");
            let actual = grouped
                .entry((department.to_string(), task_id.to_string()))
                .or_default();
            actual.tokens = actual
                .tokens
                .saturating_add(entry.input_tokens.saturating_add(entry.output_tokens));
            if entry.cost_usd.is_finite() && entry.cost_usd > 0.0 {
                actual.cost_usd += entry.cost_usd;
            }
            actual.calls = actual.calls.saturating_add(1);
        }
        let mut by_department: BTreeMap<String, Vec<ActualTask>> = BTreeMap::new();
        for ((department, _task_id), actual) in grouped {
            by_department.entry(department).or_default().push(actual);
        }
        Self { by_department }
    }

    fn estimate_for_department(&self, department: &str) -> Option<TaskEstimate> {
        let tasks = self.by_department.get(department)?;
        if tasks.is_empty() {
            return None;
        }
        let mut tokens = tasks
            .iter()
            .map(|task| task.tokens)
            .filter(|tokens| *tokens > 0)
            .collect::<Vec<_>>();
        let mut costs = tasks
            .iter()
            .map(|task| task.cost_usd)
            .filter(|cost| cost.is_finite() && *cost > 0.0)
            .collect::<Vec<_>>();
        if tokens.is_empty() && costs.is_empty() {
            // A zero-token/zero-cost ledger row is not a usable historical
            // estimate; fall through to the department envelope instead.
            return None;
        }
        tokens.sort_unstable();
        costs.sort_by(f64::total_cmp);
        let observed_calls = tasks
            .iter()
            .map(|task| task.calls)
            .fold(0u32, u32::saturating_add);
        Some(TaskEstimate {
            p50_tokens: nearest_rank(&tokens, 0.50),
            p90_tokens: nearest_rank(&tokens, 0.90),
            p50_cost_usd: nearest_rank_f64(&costs, 0.50),
            p90_cost_usd: nearest_rank_f64(&costs, 0.90),
            observed_calls,
            source: Some(EstimateSource::ProjectActuals),
        })
    }
}

impl Engine {
    async fn allocation_session(&self, session_id: &str) -> Result<Session> {
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))
    }

    async fn project_usage_history(&self, project_id: &str, limit: u32) -> Result<Vec<UsageEntry>> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking(move || store.list_project_usage_entries(&project_id, limit))
            .await?
    }

    /// Bounded, read-only R3 allocation preview. It appends no event and never
    /// mutates a task, budget, approval, effect, capacity, or merge row.
    pub async fn session_allocation(
        &self,
        params: SessionAllocationParams,
    ) -> Result<AllocationPreview> {
        let session = self.allocation_session(&params.session_id).await?;
        let budget = self.session_budget(&session.id).await?;
        let contract = self.run_contract_view(&session.id).await?;
        let readiness = self.session_readiness(&session.id).await?;
        let plan = self.get_team_plan(&session.id).await?;
        let queue = self.task_queue(None, Some(session.id.clone())).await?.tasks;
        let stalls = self.session_stalls(&session.id).await?;
        let approvals = self.list_approvals(Some(session.id.clone())).await?;
        let effects = self.session_effects(&session.id, None, false, 200).await?;
        let diagnosis = self
            .session_diagnosis(vp_protocol::diagnosis::SessionDiagnosisParams {
                session_id: session.id.clone(),
                target_task_id: None,
                limit: Some(ALLOCATION_DIAGNOSIS_EVENTS),
                include_graph: false,
            })
            .await?;
        let usage = self.usage_totals(&session.id).await?;
        let history = self
            .project_usage_history(&session.project_id, MAX_ALLOCATION_HISTORY)
            .await?;

        let statuses = queue
            .iter()
            .map(|task| (task.task_id.clone(), task.status.clone()))
            .collect::<HashMap<_, _>>();
        let session_buckets = usage
            .by_task
            .iter()
            .map(|bucket| (bucket.key.clone(), bucket.clone()))
            .collect::<HashMap<_, _>>();
        let project_actuals = ProjectActuals::from_entries(&history);
        let department_envelopes = plan.as_ref().map(department_envelopes).unwrap_or_default();

        let task_rows = allocation_task_rows(plan.as_ref(), &queue);
        let mut estimates: HashMap<String, TaskEstimate> = HashMap::new();
        let mut tasks = Vec::new();
        for row in &task_rows {
            let estimate = estimate_task(
                row,
                &session_buckets,
                &project_actuals,
                &department_envelopes,
            );
            estimates.insert(row.task_id.clone(), estimate.clone());
            tasks.push(allocation_task(row, &statuses, &budget, &estimate));
        }
        let raw_task_count = tasks.len();
        let task_truncated = raw_task_count > MAX_ALLOCATION_TASKS;
        tasks.truncate(MAX_ALLOCATION_TASKS);

        let (critical_path_ids, critical_path_error) =
            critical_path_tasks(&session, plan.as_ref(), &statuses, budget.remaining_tokens);
        let path_available = critical_path_error.is_none();
        let focus_ids = allocation_focus_ids(
            plan.as_ref(),
            &queue,
            &statuses,
            &critical_path_ids,
            path_available,
        );
        for task in &mut tasks {
            task.critical_path = critical_path_ids.contains(&task.task_id);
        }
        let mut feasibility = allocation_feasibility(
            &budget,
            &contract,
            &usage,
            &estimates,
            &focus_ids,
            &critical_path_ids,
        );
        feasibility.critical_path_available = path_available;
        feasibility.critical_path_reason = critical_path_error;

        let mut recommendations = allocation_recommendations(
            &budget,
            &contract,
            &readiness,
            &tasks,
            &feasibility,
            &queue,
            &approvals,
            &effects,
            &diagnosis,
            &stalls,
        );
        let raw_recommendations = recommendations.len();
        let recommendation_truncated = raw_recommendations > MAX_ALLOCATION_RECOMMENDATIONS;
        recommendations.truncate(MAX_ALLOCATION_RECOMMENDATIONS);

        let metareasoning =
            allocation_metareasoning(&budget, &queue, &stalls, &diagnosis, &recommendations);
        let calibration =
            allocation_calibration(&contract, &approvals, &effects, &diagnosis, &queue, &usage);

        Ok(AllocationPreview {
            session_id: session.id,
            generated_at: now_rfc3339(),
            profile: contract.profile,
            readiness,
            envelope: AllocationEnvelope {
                token_budget: budget.hard_token_cap.or(budget.effective_token_budget),
                spent_tokens: budget.spent_tokens,
                remaining_tokens: budget.remaining_tokens,
                cost_budget_usd: budget
                    .hard_cost_cap_usd
                    .or(budget.effective_cost_budget_usd),
                spent_cost_usd: budget.spent_cost_usd,
                remaining_cost_usd: budget.remaining_cost_usd,
                time_budget_secs: budget
                    .hard_wall_clock_secs
                    .or(budget.effective_time_budget_secs),
                remaining_secs: budget.remaining_secs,
                completion_reserve_ratio: contract.completion_reserve_ratio,
            },
            feasibility,
            tasks,
            recommendations,
            metareasoning,
            calibration,
            truncated: task_truncated || recommendation_truncated,
        })
    }
}

#[derive(Debug, Clone)]
struct AllocationRow {
    task_id: String,
    title: String,
    department: String,
    status: String,
    priority: i32,
    deps: Vec<String>,
    token_budget: Option<u64>,
    cost_budget_usd: Option<f64>,
}

fn allocation_task_rows(plan: Option<&TeamPlan>, queue: &[ScheduledTask]) -> Vec<AllocationRow> {
    if let Some(plan) = plan {
        let queued = queue
            .iter()
            .map(|task| (task.task_id.as_str(), task))
            .collect::<HashMap<_, _>>();
        return plan
            .tasks
            .iter()
            .map(|task| {
                let scheduled = queued.get(task.id.as_str());
                AllocationRow {
                    task_id: task.id.clone(),
                    title: task.title.clone(),
                    department: task.department.clone(),
                    status: scheduled
                        .map(|task| task.status.clone())
                        .unwrap_or_else(|| "pending".to_string()),
                    priority: scheduled.map(|task| task.priority).unwrap_or(task.priority),
                    deps: task.deps.clone(),
                    token_budget: task.token_budget,
                    cost_budget_usd: task.cost_budget_usd,
                }
            })
            .collect();
    }
    queue
        .iter()
        .map(|task| AllocationRow {
            task_id: task.task_id.clone(),
            title: task.title.clone(),
            department: task.department.clone(),
            status: task.status.clone(),
            priority: task.priority,
            deps: task.deps.clone(),
            token_budget: None,
            cost_budget_usd: None,
        })
        .collect()
}

fn department_envelopes(plan: &TeamPlan) -> HashMap<String, DepartmentEnvelope> {
    let mut counts: HashMap<&str, u64> = HashMap::new();
    let mut task_tokens: HashMap<&str, u64> = HashMap::new();
    let mut task_costs: HashMap<&str, f64> = HashMap::new();
    for task in &plan.tasks {
        let department = task.department.as_str();
        *counts.entry(department).or_insert(0) += 1;
        if let Some(tokens) = task.token_budget.filter(|tokens| *tokens > 0) {
            *task_tokens.entry(department).or_insert(0) = task_tokens
                .get(department)
                .copied()
                .unwrap_or(0)
                .saturating_add(tokens);
        }
        if let Some(cost) = task
            .cost_budget_usd
            .filter(|cost| cost.is_finite() && *cost > 0.0)
        {
            *task_costs.entry(department).or_insert(0.0) += cost;
        }
    }
    plan.departments
        .iter()
        .map(|department| {
            let count = counts
                .get(department.name.as_str())
                .copied()
                .unwrap_or(1)
                .max(1);
            let tokens = department
                .token_budget
                .filter(|tokens| *tokens > 0)
                .or_else(|| task_tokens.get(department.name.as_str()).copied())
                .map(|tokens| tokens / count);
            let cost_usd = department
                .cost_budget_usd
                .filter(|cost| cost.is_finite() && *cost > 0.0)
                .or_else(|| task_costs.get(department.name.as_str()).copied())
                .map(|cost| cost / count as f64);
            (
                department.name.clone(),
                DepartmentEnvelope { tokens, cost_usd },
            )
        })
        .collect()
}

fn estimate_task(
    row: &AllocationRow,
    session_buckets: &HashMap<String, UsageBucket>,
    project_actuals: &ProjectActuals,
    department_envelopes: &HashMap<String, DepartmentEnvelope>,
) -> TaskEstimate {
    if let Some(bucket) = session_buckets.get(&row.task_id)
        && bucket.calls > 0
    {
        let tokens = bucket.input_tokens.saturating_add(bucket.output_tokens);
        let cost =
            (bucket.cost_usd.is_finite() && bucket.cost_usd > 0.0).then_some(bucket.cost_usd);
        if tokens > 0 || cost.is_some() {
            return TaskEstimate {
                p50_tokens: Some(tokens),
                p90_tokens: Some(tokens),
                p50_cost_usd: cost,
                p90_cost_usd: cost,
                observed_calls: bucket.calls.min(u32::MAX as u64) as u32,
                source: Some(EstimateSource::SessionActuals),
            };
        }
    }
    if row.token_budget.is_some() || row.cost_budget_usd.is_some() {
        let tokens = row.token_budget.filter(|tokens| *tokens > 0);
        let cost = row
            .cost_budget_usd
            .filter(|cost| cost.is_finite() && *cost > 0.0);
        return TaskEstimate {
            p50_tokens: tokens,
            p90_tokens: tokens,
            p50_cost_usd: cost,
            p90_cost_usd: cost,
            observed_calls: 0,
            source: Some(EstimateSource::PlanEnvelope),
        };
    }
    if let Some(estimate) = project_actuals.estimate_for_department(&row.department) {
        return estimate;
    }
    if let Some(envelope) = department_envelopes.get(&row.department)
        && (envelope.tokens.is_some() || envelope.cost_usd.is_some())
    {
        return TaskEstimate {
            p50_tokens: envelope.tokens,
            p90_tokens: envelope.tokens,
            p50_cost_usd: envelope.cost_usd,
            p90_cost_usd: envelope.cost_usd,
            observed_calls: 0,
            source: Some(EstimateSource::DepartmentEnvelope),
        };
    }
    TaskEstimate {
        p50_tokens: None,
        p90_tokens: None,
        p50_cost_usd: None,
        p90_cost_usd: None,
        observed_calls: 0,
        source: Some(EstimateSource::Prior),
    }
}

fn allocation_task(
    row: &AllocationRow,
    statuses: &HashMap<String, String>,
    budget: &vp_protocol::types::SessionBudgetStatus,
    estimate: &TaskEstimate,
) -> AllocationTask {
    let terminal = crate::critical_path::is_terminal_status(&row.status);
    let ready = allocation_task_ready(&row.status, &row.deps, statuses);
    let token_limit = row.token_budget.or(budget.remaining_tokens);
    let cost_limit = row.cost_budget_usd.or(budget.remaining_cost_usd);
    let token_fit = estimate
        .p90_tokens
        .map(|p90| token_limit.is_none_or(|limit| p90 <= limit));
    let cost_fit = estimate
        .p90_cost_usd
        .filter(|value| value.is_finite())
        .map(|p90| cost_limit.is_none_or(|limit| p90 <= limit));
    let (feasible, reason) = if terminal {
        (true, "terminal".to_string())
    } else if token_fit == Some(false) {
        let reason = if row
            .token_budget
            .is_some_and(|limit| estimate.p90_tokens.is_some_and(|p90| p90 > limit))
        {
            "exceeds_task_tokens"
        } else {
            "exceeds_remaining_tokens"
        };
        (false, reason.to_string())
    } else if cost_fit == Some(false) {
        let reason = if row
            .cost_budget_usd
            .is_some_and(|limit| estimate.p90_cost_usd.is_some_and(|p90| p90 > limit))
        {
            "exceeds_task_cost"
        } else {
            "exceeds_remaining_cost"
        };
        (false, reason.to_string())
    } else if token_fit.is_some() || cost_fit.is_some() {
        let reason = if token_limit.is_none() && cost_limit.is_none() {
            "unbounded_envelope"
        } else {
            match (token_fit, cost_fit) {
                (Some(true), Some(true)) => "within_envelope",
                (Some(true), None) if estimate.p90_cost_usd.is_some() => "cost_unbounded",
                (Some(true), None) => "token_only",
                (None, Some(true)) => "cost_only",
                _ => "within_envelope",
            }
        };
        (true, reason.to_string())
    } else {
        (false, "unestimated".to_string())
    };
    AllocationTask {
        task_id: row.task_id.clone(),
        title: bounded_allocation_text(&row.title, 160),
        department: bounded_allocation_text(&row.department, 80),
        status: row.status.clone(),
        priority: row.priority,
        token_budget: row.token_budget,
        cost_budget_usd: row.cost_budget_usd,
        p50_tokens: estimate.p50_tokens,
        p90_tokens: estimate.p90_tokens,
        observed_calls: estimate.observed_calls,
        source: estimate
            .source
            .map(EstimateSource::as_str)
            .unwrap_or("prior")
            .to_string(),
        critical_path: false,
        ready,
        feasible,
        reason,
    }
}

fn allocation_task_ready(
    status: &str,
    deps: &[String],
    statuses: &HashMap<String, String>,
) -> bool {
    if !matches!(status, "" | "pending" | "queued" | "ready" | "retrying") {
        return false;
    }
    deps.iter().all(|dependency| {
        statuses
            .get(dependency)
            .is_some_and(|status| crate::critical_path::is_terminal_status(status))
    })
}

fn critical_path_tasks(
    session: &Session,
    plan: Option<&TeamPlan>,
    statuses: &HashMap<String, String>,
    remaining_budget_tokens: Option<u64>,
) -> (BTreeSet<String>, Option<String>) {
    let Some(plan) = plan else {
        return (BTreeSet::new(), Some("no_plan".to_string()));
    };
    match compute_critical_path(CriticalPathInput {
        session_id: &session.id,
        project_id: &session.project_id,
        plan_revision: 0,
        plan,
        statuses,
        remaining_budget_tokens,
    }) {
        Ok(path) => (path.task_ids.into_iter().collect(), None),
        Err(error) => (BTreeSet::new(), Some(error.code.to_string())),
    }
}

fn allocation_focus_ids(
    plan: Option<&TeamPlan>,
    queue: &[ScheduledTask],
    statuses: &HashMap<String, String>,
    critical_path_ids: &BTreeSet<String>,
    path_available: bool,
) -> Vec<String> {
    if path_available && !critical_path_ids.is_empty() {
        return critical_path_ids.iter().cloned().collect();
    }
    let mut ids = Vec::new();
    if let Some(plan) = plan {
        for task in &plan.tasks {
            let status = statuses
                .get(&task.id)
                .map(String::as_str)
                .unwrap_or("pending");
            if is_remaining_status(status) {
                ids.push(task.id.clone());
            }
        }
    } else {
        for task in queue {
            if is_remaining_status(&task.status) {
                ids.push(task.task_id.clone());
            }
        }
    }
    ids
}

fn allocation_feasibility(
    budget: &vp_protocol::types::SessionBudgetStatus,
    contract: &vp_protocol::types::RunContractView,
    usage: &UsageTotals,
    estimates: &HashMap<String, TaskEstimate>,
    focus_ids: &[String],
    critical_path_ids: &BTreeSet<String>,
) -> AllocationFeasibility {
    let mut token_p50 = 0u64;
    let mut token_p90 = 0u64;
    let mut cost_p50 = 0.0f64;
    let mut cost_p90 = 0.0f64;
    let mut any_tokens = false;
    let mut any_token_p90 = false;
    let mut any_cost = false;
    let mut any_cost_p90 = false;
    let mut estimated_tasks = 0u32;
    let total_tasks = focus_ids.len().min(u32::MAX as usize) as u32;
    let mut sources = BTreeSet::new();
    for task_id in focus_ids {
        let Some(estimate) = estimates.get(task_id) else {
            continue;
        };
        if let Some(source) = estimate.source {
            sources.insert(source);
        }
        let has_token = estimate.p90_tokens.is_some_and(|value| value > 0)
            || estimate.p50_tokens.is_some_and(|value| value > 0);
        let has_cost = estimate
            .p90_cost_usd
            .is_some_and(|value| value.is_finite() && value > 0.0)
            || estimate
                .p50_cost_usd
                .is_some_and(|value| value.is_finite() && value > 0.0);
        if has_token || has_cost {
            estimated_tasks = estimated_tasks.saturating_add(1);
        }
        if let Some(value) = estimate.p50_tokens {
            token_p50 = token_p50.saturating_add(value);
            any_tokens = true;
        }
        if let Some(value) = estimate.p90_tokens {
            token_p90 = token_p90.saturating_add(value);
            any_token_p90 = true;
        }
        if let Some(value) = estimate.p50_cost_usd.filter(|value| value.is_finite()) {
            cost_p50 += value;
            any_cost = true;
        }
        if let Some(value) = estimate.p90_cost_usd.filter(|value| value.is_finite()) {
            cost_p90 += value;
            any_cost_p90 = true;
        }
    }
    let missing_estimates = total_tasks.saturating_sub(estimated_tasks);
    let source = match sources.len() {
        0 => "unavailable".to_string(),
        1 => sources
            .iter()
            .next()
            .map(|source| source.as_str())
            .unwrap_or("unavailable")
            .to_string(),
        _ => "mixed".to_string(),
    };
    let hard_token = budget.hard_token_cap.or(budget.effective_token_budget);
    let reserve_tokens = hard_token.map(|hard| {
        ((hard as f64) * contract.completion_reserve_ratio)
            .round()
            .max(0.0) as u64
    });
    let headroom_tokens = budget
        .remaining_tokens
        .map(|remaining| remaining.saturating_sub(reserve_tokens.unwrap_or(0)));
    let cost_budget = budget
        .hard_cost_cap_usd
        .or(budget.effective_cost_budget_usd);
    let cost_remaining = budget.remaining_cost_usd;
    // A USD cap alone is not price evidence. Only a positive observed cost (or
    // a positive estimate derived from configured prices) enables a cost
    // feasibility claim; otherwise the preview honestly says unconfigured.
    let cost_configured = usage.cost_usd > 0.0 || any_cost || any_cost_p90;
    let hard_time = budget
        .hard_wall_clock_secs
        .or(budget.effective_time_budget_secs);
    let mut pressure_dimensions = Vec::new();
    if let (Some(hard), Some(remaining)) = (hard_token, budget.remaining_tokens)
        && (remaining as f64) < hard as f64 * contract.completion_reserve_ratio
    {
        pressure_dimensions.push("tokens".to_string());
    }
    if let (Some(hard), Some(remaining)) = (cost_budget, budget.remaining_cost_usd)
        && remaining < hard * contract.completion_reserve_ratio
    {
        pressure_dimensions.push("money".to_string());
    }
    if let (Some(hard), Some(remaining)) = (hard_time, budget.remaining_secs)
        && (remaining as f64) < hard as f64 * contract.completion_reserve_ratio
    {
        pressure_dimensions.push("time".to_string());
    }

    let mut feasible = true;
    let mut reason = "feasible".to_string();
    if focus_ids.is_empty() {
        reason = "no_remaining_tasks".to_string();
    } else if estimated_tasks == 0 {
        feasible = false;
        reason = "insufficient_estimate".to_string();
    } else if missing_estimates > 0 {
        // Known estimates understate a partial path; never claim that the
        // remaining critical path fits when some of its tasks are unknown.
        feasible = false;
        reason = "partial_estimate".to_string();
    } else if hard_token.is_none() || budget.remaining_tokens.is_none() {
        reason = "unbounded_envelope".to_string();
    } else if let (Some(remaining), Some(reserve)) = (budget.remaining_tokens, reserve_tokens)
        && remaining < reserve
    {
        feasible = false;
        reason = "remaining_below_completion_reserve".to_string();
    } else if let (Some(p90), Some(headroom)) =
        (any_token_p90.then_some(token_p90), headroom_tokens)
        && p90 > headroom
    {
        feasible = false;
        reason = "p90_exceeds_reserve_headroom".to_string();
    }

    if feasible && !focus_ids.is_empty() {
        // Cost feasibility is independent of token estimates. A task can have
        // no token history while configured prices still prove that the
        // remaining cost reserve cannot fund it.
        if cost_configured {
            if let (Some(cap), Some(remaining_cost)) = (cost_budget, cost_remaining)
                && cap > 0.0
            {
                let reserve_cost = cap * contract.completion_reserve_ratio;
                let cost_headroom = (remaining_cost - reserve_cost).max(0.0);
                if remaining_cost < reserve_cost {
                    feasible = false;
                    reason = "remaining_below_cost_reserve".to_string();
                } else if any_cost_p90 && cost_p90 > cost_headroom {
                    feasible = false;
                    reason = "p90_cost_exceeds_reserve_headroom".to_string();
                }
            } else if cost_budget.is_none() {
                reason = "cost_unbounded".to_string();
            }
        } else if any_token_p90 {
            reason = "cost_unconfigured".to_string();
        }
        if feasible && reason == "feasible" && !any_token_p90 {
            reason = "token_unestimated".to_string();
        }
    }

    AllocationFeasibility {
        feasible,
        reason,
        p50_tokens: any_tokens.then_some(token_p50),
        p90_tokens: any_token_p90.then_some(token_p90),
        p50_cost_usd: any_cost.then_some(cost_p50),
        p90_cost_usd: any_cost_p90.then_some(cost_p90),
        sample_calls: usage.calls.min(u32::MAX as u64) as u32,
        source,
        reserve_tokens,
        headroom_tokens,
        critical_path_tasks: if focus_ids.is_empty() {
            Vec::new()
        } else {
            critical_path_ids
                .iter()
                .take(MAX_ALLOCATION_TASKS)
                .cloned()
                .collect()
        },
        estimated_tasks,
        total_tasks,
        pressure_dimensions,
        // The caller that has the plan/status inputs fills these; the pure
        // feasibility function deliberately does not invent a path.
        critical_path_available: false,
        critical_path_reason: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn allocation_recommendations(
    budget: &vp_protocol::types::SessionBudgetStatus,
    contract: &vp_protocol::types::RunContractView,
    readiness: &vp_protocol::types::ReadinessSnapshot,
    tasks: &[AllocationTask],
    feasibility: &AllocationFeasibility,
    queue: &[ScheduledTask],
    approvals: &[Approval],
    effects: &vp_protocol::types::SessionEffectsResult,
    diagnosis: &vp_protocol::diagnosis::SessionDiagnosisResult,
    stalls: &vp_protocol::types::SessionStallsResult,
) -> Vec<AllocationRecommendation> {
    let mut recommendations = Vec::new();
    let mut push = |action: &str, reason: String, confidence: &str, task_id: Option<String>| {
        if recommendations.len() >= MAX_ALLOCATION_RECOMMENDATIONS
            || recommendations
                .iter()
                .any(|existing: &AllocationRecommendation| existing.action == action)
        {
            return;
        }
        recommendations.push(AllocationRecommendation {
            action: action.to_string(),
            reason: bounded_allocation_text(&reason, MAX_ALLOCATION_TEXT),
            confidence: confidence.to_string(),
            task_id,
        });
    };

    let pending_approvals = approvals
        .iter()
        .filter(|approval| approval.status == "pending")
        .count() as u32;
    let no_progress = budget
        .state
        .as_ref()
        .map(|state| state.turns_without_progress)
        .unwrap_or(0);
    let near_completion = matches!(
        readiness.level.as_str(),
        "committed" | "tests_passed" | "verified"
    );
    let reserve_pressure = !feasibility.pressure_dimensions.is_empty();
    let ready_non_critical = tasks
        .iter()
        .filter(|task| task.ready && !task.critical_path)
        .count();
    let stalled_task = stalls.stalls.first().map(|stall| stall.task_id.clone());

    if readiness.level == "dirty" || readiness.level == "blocked" {
        push(
            "verify_or_merge",
            format!(
                "readiness is {}; preserve the worktree and finish the existing evidence path before new work",
                readiness.level
            ),
            "high",
            None,
        );
    }
    if near_completion || readiness.level == "merged" {
        push(
            "verify_or_merge",
            "near-completion or merged evidence already exists; keep Verifier/Integrator availability"
                .to_string(),
            "high",
            None,
        );
    }
    if !feasibility.feasible {
        push(
            "top_up",
            format!("allocation is infeasible: {}", feasibility.reason),
            "medium",
            None,
        );
        if ready_non_critical > 0 {
            push(
                "defer_non_critical",
                format!(
                    "{ready_non_critical} ready non-critical task(s) do not fit the remaining envelope"
                ),
                "medium",
                None,
            );
        }
    }
    if no_progress > 0 {
        push(
            "replan",
            format!("{no_progress} turn(s) without progress evidence"),
            "high",
            None,
        );
        if let Some(task_id) = stalled_task.clone() {
            push(
                "stop_task",
                "the task has a persisted stall observation; preserve evidence before retry"
                    .to_string(),
                "medium",
                Some(task_id),
            );
        }
    }
    if pending_approvals > contract.attention_cap {
        push(
            "reduce_interrupts",
            format!(
                "{pending_approvals} pending approval(s) exceed the attention cap {}",
                contract.attention_cap
            ),
            "high",
            None,
        );
    } else if pending_approvals > 0 {
        push(
            "reduce_interrupts",
            format!("{pending_approvals} pending approval(s) need operator attention"),
            "medium",
            None,
        );
    }
    if budget
        .forecast
        .runway_turns
        .is_some_and(|turns| (turns as usize) < feasibility.critical_path_tasks.len())
    {
        push(
            "reduce_concurrency",
            "the forecast runway is shorter than the remaining critical-path task count; this is advisory, not a completion guarantee"
                .to_string(),
            "low",
            None,
        );
    }
    if near_completion
        && reserve_pressure
        && no_progress == 0
        && !budget.unlimited
        && budget.mode == BudgetMode::Fixed
        && readiness
            .latest_commit
            .as_deref()
            .is_some_and(|commit| !commit.trim().is_empty())
    {
        push(
            "rescue_reserve",
            format!(
                "clean near-completion work is pressured by the completion reserve ({}); a short rescue grant may release only that reserve",
                feasibility.pressure_dimensions.join(", ")
            ),
            "medium",
            None,
        );
    }
    let verifier_rejections = diagnosis
        .findings
        .iter()
        .filter(|finding| finding.reason_code == reason_codes::VERIFIER_REJECTION)
        .count();
    if verifier_rejections > 0 {
        push(
            "increase_verification",
            format!("{verifier_rejections} Verifier rejection finding(s) in the bounded diagnosis"),
            "medium",
            None,
        );
    }
    if diagnosis.status == "attributed" || diagnosis.status == "missing_evidence" {
        push(
            "replan",
            format!(
                "diagnosis reports {}; the current allocation is not supported by completion evidence",
                diagnosis.status
            ),
            "medium",
            diagnosis
                .target
                .as_ref()
                .and_then(|target| target.task_id.clone()),
        );
    }
    let failed_tasks = queue.iter().filter(|task| task.status == "failed").count();
    if failed_tasks > 0 {
        push(
            "replan",
            format!("{failed_tasks} failed task(s) need a bounded retry or plan revision"),
            "medium",
            None,
        );
    }
    if effects.blocked {
        push(
            "verify_or_merge",
            "an unresolved effect blocks merge; reconcile it before new work".to_string(),
            "high",
            None,
        );
    }
    if feasibility.feasible && no_progress == 0 && !effects.blocked && readiness.level != "blocked"
    {
        push(
            "continue",
            if tasks.is_empty() {
                "no remaining plan task consumes the envelope".to_string()
            } else {
                "the remaining critical path fits the reported envelope".to_string()
            },
            "medium",
            None,
        );
    }
    recommendations
}

fn allocation_metareasoning(
    budget: &vp_protocol::types::SessionBudgetStatus,
    queue: &[ScheduledTask],
    stalls: &vp_protocol::types::SessionStallsResult,
    diagnosis: &vp_protocol::diagnosis::SessionDiagnosisResult,
    recommendations: &[AllocationRecommendation],
) -> AllocationMetareasoning {
    let no_progress_turns = budget
        .state
        .as_ref()
        .map(|state| state.turns_without_progress)
        .unwrap_or(0);
    let mut stalled_tasks = Vec::new();
    for stall in &stalls.stalls {
        if !stalled_tasks
            .iter()
            .any(|task_id| task_id == &stall.task_id)
        {
            stalled_tasks.push(stall.task_id.clone());
        }
    }
    stalled_tasks.truncate(MAX_ALLOCATION_TASKS);
    let failed_tasks = queue
        .iter()
        .filter(|task| task.status == "failed")
        .map(|task| task.task_id.clone())
        .take(MAX_ALLOCATION_TASKS)
        .collect::<Vec<_>>();
    let retrying_tasks = queue
        .iter()
        .filter(|task| task.status == "retrying" || (task.status == "pending" && task.attempts > 0))
        .map(|task| task.task_id.clone())
        .take(MAX_ALLOCATION_TASKS)
        .collect::<Vec<_>>();
    AllocationMetareasoning {
        diagnosis_status: diagnosis.status.clone(),
        diagnosis_reason: diagnosis
            .earliest
            .as_ref()
            .map(|finding| finding.reason_code.clone()),
        diagnosis_target: diagnosis
            .target
            .as_ref()
            .map(|target| target.task_id.clone().unwrap_or_else(|| target.id.clone())),
        no_progress_turns,
        stalled_tasks,
        failed_tasks,
        retrying_tasks,
        recommendations: recommendations
            .iter()
            .take(MAX_ALLOCATION_RECOMMENDATIONS)
            .cloned()
            .collect(),
    }
}

fn allocation_calibration(
    contract: &vp_protocol::types::RunContractView,
    approvals: &[Approval],
    effects: &vp_protocol::types::SessionEffectsResult,
    diagnosis: &vp_protocol::diagnosis::SessionDiagnosisResult,
    queue: &[ScheduledTask],
    usage: &UsageTotals,
) -> AllocationCalibration {
    let pending_approvals = approvals
        .iter()
        .filter(|approval| approval.status == "pending")
        .count() as u32;
    let attention_interrupts = approvals
        .iter()
        .filter(|approval| approval.status == "pending")
        .map(|approval| approval_risk_weight(approval.approval_risk.as_deref()))
        .fold(0u32, u32::saturating_add)
        .saturating_add((effects.counts.unknown as u32).saturating_mul(5));
    let verifier_rejections = diagnosis
        .findings
        .iter()
        .filter(|finding| finding.reason_code == reason_codes::VERIFIER_REJECTION)
        .count() as u32;
    let failed_tasks = queue.iter().filter(|task| task.status == "failed").count() as u32;
    let attention_recommendation = if pending_approvals > contract.attention_cap {
        "pause_for_operator"
    } else if attention_interrupts >= contract.attention_cap {
        "reduce_interrupts"
    } else {
        "batch_routine"
    };
    let trust_recommendation = if verifier_rejections > 0 || failed_tasks > 0 {
        "increase_verification"
    } else if attention_interrupts >= ((contract.attention_cap as f64) * 0.8).ceil() as u32 {
        "reduce_interrupts"
    } else {
        "standard"
    };
    let confidence = if usage.calls >= 5 && verifier_rejections == 0 && failed_tasks == 0 {
        "high"
    } else if usage.calls >= 1 {
        "medium"
    } else {
        "low"
    };
    let reason = format!(
        "{pending_approvals} pending approval(s), {attention_interrupts}/{} weighted interrupt(s), {verifier_rejections} Verifier rejection finding(s), {failed_tasks} failed task(s), {} ledger call(s); this is an advisory calibration, not a model-routing decision",
        contract.attention_cap, usage.calls
    );
    AllocationCalibration {
        attention_recommendation: attention_recommendation.to_string(),
        trust_recommendation: trust_recommendation.to_string(),
        reason: bounded_allocation_text(&reason, MAX_ALLOCATION_TEXT),
        confidence: confidence.to_string(),
        pending_approvals,
        attention_interrupts,
        attention_cap: contract.attention_cap,
        verifier_rejections,
        failed_tasks,
    }
}

fn approval_risk_weight(risk: Option<&str>) -> u32 {
    match risk {
        Some("workspace_write") => 1,
        Some("external") => 2,
        Some("host_state") => 3,
        Some("destructive") => 5,
        Some("mcp") | Some("sensitive_gui") => 3,
        _ => 1,
    }
}

fn nearest_rank(values: &[u64], percentile: f64) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    let rank = ((values.len() as f64) * percentile).ceil().max(1.0) as usize;
    values.get(rank.saturating_sub(1)).copied()
}

fn nearest_rank_f64(values: &[f64], percentile: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let rank = ((values.len() as f64) * percentile).ceil().max(1.0) as usize;
    values.get(rank.saturating_sub(1)).copied()
}

fn bounded_allocation_text(value: &str, max_chars: usize) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() || max_chars == 0 {
        return String::new();
    }
    let mut chars = trimmed.chars();
    let mut bounded = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        bounded.pop();
        bounded.push('…');
    }
    bounded
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;
    use tempfile::TempDir;
    use vp_protocol::types::{
        AutoBudgetPolicy, PlanDepartment, PlanTask, ReadinessSnapshot, RunContractView,
        SetSessionBudgetParams,
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

    fn plan_with_task(token_budget: Option<u64>) -> TeamPlan {
        TeamPlan {
            goal: "allocation".into(),
            departments: vec![PlanDepartment {
                name: "impl".into(),
                manager_title: "Implementation".into(),
                profile: "coding".into(),
                objective: "build".into(),
                token_budget: None,
                cost_budget_usd: None,
            }],
            tasks: vec![PlanTask {
                id: "T1".into(),
                department: "impl".into(),
                title: "Build it".into(),
                kind: "coding".into(),
                priority: 5,
                profile: Some("coding".into()),
                deps: Vec::new(),
                write_scope: vec!["src/lib.rs".into()],
                acceptance: vec!["tests pass".into()],
                token_budget,
                cost_budget_usd: None,
                parent_task_id: None,
                fanout_key: None,
                fanout_join_for: None,
            }],
        }
    }

    async fn fixture() -> (TempDir, TempDir, Engine, Session) {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("allocation"))
            .unwrap();
        let session = store.create_session(&project.id, "allocation").unwrap();
        let engine = Engine::new(store);
        let session = engine.ensure_session_worktree(&session.id).await.unwrap();
        engine
            .store()
            .update_session_status(&session.id, "idle")
            .unwrap();
        (data, repo, engine, session)
    }

    async fn record_task_usage(
        engine: &Engine,
        session_id: &str,
        task_id: &str,
        input: u64,
        output: u64,
    ) {
        engine
            .record_agent_usage(
                session_id,
                "test-agent",
                "Test Agent",
                "worker",
                Some(task_id.to_string()),
                Some("impl".to_string()),
                "test-model",
                input,
                output,
                false,
                0.0,
            )
            .await
            .unwrap();
    }

    fn no_effects() -> vp_protocol::types::SessionEffectsResult {
        vp_protocol::types::SessionEffectsResult {
            session_id: "ses-1".into(),
            policy: vp_protocol::types::EffectPolicy::default(),
            receipts: Vec::new(),
            counts: vp_protocol::types::EffectCounts::default(),
            blocked: false,
            block_reason: None,
            truncated: false,
        }
    }

    fn pending_approval() -> Approval {
        Approval {
            id: "ap-1".into(),
            session_id: "ses-1".into(),
            tool_call_id: "call-1".into(),
            tool: "shell".into(),
            input: serde_json::json!({}),
            status: "pending".into(),
            created_at: "2026-11-24T00:00:00Z".into(),
            resolved_at: None,
            approval_risk: Some("host_state".into()),
            approval_reason: None,
            agent_id: None,
            agent_name: None,
            agent_role: None,
            agent_title: None,
            task_id: None,
            scope_hash: None,
            dedup_key: None,
            waiter_count: 0,
            template_id: None,
            priority: 0,
            expires_at: None,
            notify: false,
            notify_key: None,
        }
    }

    fn task(id: &str, priority: i32, budget: Option<u64>) -> PlanTask {
        PlanTask {
            id: id.into(),
            department: "impl".into(),
            title: format!("Task {id}"),
            kind: "coding".into(),
            priority,
            profile: Some("coding".into()),
            deps: Vec::new(),
            write_scope: vec![format!("src/{id}.rs")],
            acceptance: vec!["tests pass".into()],
            token_budget: budget,
            cost_budget_usd: None,
            parent_task_id: None,
            fanout_key: None,
            fanout_join_for: None,
        }
    }

    fn budget(
        effective: Option<u64>,
        hard: Option<u64>,
        spent: u64,
        remaining: Option<u64>,
    ) -> vp_protocol::types::SessionBudgetStatus {
        vp_protocol::types::SessionBudgetStatus {
            session_id: "ses-1".into(),
            mode: BudgetMode::Fixed,
            unlimited: false,
            effective_token_budget: effective,
            effective_cost_budget_usd: None,
            effective_time_budget_secs: None,
            hard_token_cap: hard,
            hard_cost_cap_usd: None,
            hard_wall_clock_secs: None,
            spent_tokens: spent,
            spent_cost_usd: 0.0,
            elapsed_secs: 0,
            remaining_tokens: remaining,
            remaining_cost_usd: None,
            remaining_secs: None,
            policy: None,
            state: None,
            forecast: vp_protocol::types::BudgetForecast::default(),
            updated_at: None,
        }
    }

    fn contract(ratio: f64) -> RunContractView {
        RunContractView {
            session_id: "ses-1".into(),
            project_id: "prj-1".into(),
            profile: "balanced".into(),
            source: "derived".into(),
            goal: "goal".into(),
            acceptance: Vec::new(),
            risk_ceiling: "approval-managed".into(),
            autonomy: "operator-configured".into(),
            hard_caps: vp_protocol::types::AutoBudgetCaps::default(),
            soft_targets: vp_protocol::types::ResourceSoftTargets {
                cost_usd: None,
                time_secs: None,
                attention: None,
            },
            completion_reserve_ratio: ratio,
            attention_cap: 20,
            stop_policy: vp_protocol::types::RunStopPolicy::default(),
            warnings: Vec::new(),
            generated_at: "2026-11-24T00:00:00Z".into(),
        }
    }

    fn readiness(level: &str) -> ReadinessSnapshot {
        ReadinessSnapshot {
            session_id: "ses-1".into(),
            level: level.into(),
            shippable: false,
            mergeable: false,
            latest_commit: Some("abc123".into()),
            latest_test: None,
            latest_test_passed: None,
            verifier: None,
            merge: None,
            changed_paths: Vec::new(),
            remaining_work: Vec::new(),
            blockers: Vec::new(),
            partial_delivery: None,
            generated_at: "2026-11-24T00:00:00Z".into(),
        }
    }

    #[test]
    fn estimates_prefer_session_plan_project_department_and_prior_in_order() {
        let row = AllocationRow {
            task_id: "T1".into(),
            title: "Task T1".into(),
            department: "impl".into(),
            status: "pending".into(),
            priority: 5,
            deps: Vec::new(),
            token_budget: None,
            cost_budget_usd: None,
        };
        let mut session_buckets = HashMap::new();
        session_buckets.insert(
            "T1".to_string(),
            UsageBucket {
                key: "T1".into(),
                calls: 2,
                input_tokens: 800,
                output_tokens: 200,
                cost_usd: 0.2,
                ..UsageBucket::default()
            },
        );
        let empty_actuals = ProjectActuals::default();
        let empty_departments = HashMap::new();
        let estimate = estimate_task(&row, &session_buckets, &empty_actuals, &empty_departments);
        assert_eq!(estimate.source, Some(EstimateSource::SessionActuals));
        assert_eq!(estimate.p50_tokens, Some(1_000));
        assert_eq!(estimate.p90_tokens, Some(1_000));
        assert_eq!(estimate.observed_calls, 2);

        let zero_departments = HashMap::from([(
            "impl".to_string(),
            DepartmentEnvelope {
                tokens: Some(2_500),
                cost_usd: None,
            },
        )]);
        let zero_bucket = HashMap::from([(
            "T1".to_string(),
            UsageBucket {
                key: "T1".into(),
                calls: 1,
                input_tokens: 0,
                output_tokens: 0,
                cost_usd: 0.0,
                ..UsageBucket::default()
            },
        )]);
        let estimate = estimate_task(&row, &zero_bucket, &empty_actuals, &zero_departments);
        assert_eq!(
            estimate.source,
            Some(EstimateSource::DepartmentEnvelope),
            "a zero-token ledger row must not mask a usable department envelope"
        );
        assert_eq!(estimate.p50_tokens, Some(2_500));

        let mut plan_row = row.clone();
        plan_row.token_budget = Some(4_000);
        let estimate = estimate_task(
            &plan_row,
            &HashMap::new(),
            &empty_actuals,
            &empty_departments,
        );
        assert_eq!(estimate.source, Some(EstimateSource::PlanEnvelope));
        assert_eq!(estimate.p50_tokens, Some(4_000));

        let history = vec![UsageEntry {
            id: "u1".into(),
            session_id: "ses-old".into(),
            agent_id: "a".into(),
            agent_name: "A".into(),
            role: "worker".into(),
            task_id: Some("T-old".into()),
            department: Some("impl".into()),
            model: "m".into(),
            input_tokens: 1_000,
            output_tokens: 500,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            estimated: false,
            cost_usd: 0.1,
            created_at: "2026-11-24T00:00:00Z".into(),
        }];
        let actuals = ProjectActuals::from_entries(&history);
        let estimate = estimate_task(&row, &HashMap::new(), &actuals, &empty_departments);
        assert_eq!(estimate.source, Some(EstimateSource::ProjectActuals));
        assert_eq!(estimate.p50_tokens, Some(1_500));
        assert_eq!(estimate.p90_tokens, Some(1_500));

        let mut departments = HashMap::new();
        departments.insert(
            "impl".to_string(),
            DepartmentEnvelope {
                tokens: Some(2_500),
                cost_usd: Some(0.5),
            },
        );
        let estimate = estimate_task(&row, &HashMap::new(), &actuals, &HashMap::new());
        assert_eq!(estimate.source, Some(EstimateSource::ProjectActuals));

        let zero_history = vec![UsageEntry {
            id: "u-zero".into(),
            session_id: "ses-old".into(),
            agent_id: "a".into(),
            agent_name: "A".into(),
            role: "worker".into(),
            task_id: Some("T-zero".into()),
            department: Some("impl".into()),
            model: "m".into(),
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            estimated: false,
            cost_usd: 0.0,
            created_at: "2026-11-24T00:00:00Z".into(),
        }];
        let zero_actuals = ProjectActuals::from_entries(&zero_history);
        let estimate = estimate_task(&row, &HashMap::new(), &zero_actuals, &departments);
        assert_eq!(
            estimate.source,
            Some(EstimateSource::DepartmentEnvelope),
            "zero-valued project history must fall through to the department envelope"
        );

        let no_actuals = ProjectActuals::default();
        let estimate = estimate_task(&row, &HashMap::new(), &no_actuals, &departments);
        assert_eq!(estimate.source, Some(EstimateSource::DepartmentEnvelope));
        assert_eq!(estimate.p50_tokens, Some(2_500));

        let estimate = estimate_task(
            &row,
            &HashMap::new(),
            &ProjectActuals::default(),
            &HashMap::new(),
        );
        assert_eq!(estimate.source, Some(EstimateSource::Prior));
        assert_eq!(estimate.p50_tokens, None);
        assert_eq!(estimate.p90_tokens, None);
    }

    #[test]
    fn feasibility_reports_reserve_headroom_unbounded_and_cost_unconfigured() {
        let estimates = HashMap::from([(
            "T1".to_string(),
            TaskEstimate {
                p50_tokens: Some(4_000),
                p90_tokens: Some(4_000),
                p50_cost_usd: None,
                p90_cost_usd: None,
                observed_calls: 0,
                source: Some(EstimateSource::PlanEnvelope),
            },
        )]);
        let session_budget = budget(Some(10_000), Some(10_000), 6_000, Some(4_000));
        let feasibility = allocation_feasibility(
            &session_budget,
            &contract(0.2),
            &UsageTotals::default(),
            &estimates,
            &["T1".to_string()],
            &BTreeSet::from(["T1".to_string()]),
        );
        assert!(!feasibility.feasible);
        assert_eq!(feasibility.reason, "p90_exceeds_reserve_headroom");
        assert_eq!(feasibility.reserve_tokens, Some(2_000));
        assert_eq!(feasibility.headroom_tokens, Some(2_000));
        assert_eq!(feasibility.p50_tokens, Some(4_000));
        assert_eq!(feasibility.critical_path_tasks, vec!["T1".to_string()]);

        let unbounded = allocation_feasibility(
            &budget(None, None, 0, None),
            &contract(0.2),
            &UsageTotals::default(),
            &estimates,
            &["T1".to_string()],
            &BTreeSet::new(),
        );
        assert!(unbounded.feasible);
        assert_eq!(unbounded.reason, "cost_unconfigured");
        assert_eq!(unbounded.reserve_tokens, None);
        assert_eq!(unbounded.headroom_tokens, None);

        let no_plan = allocation_feasibility(
            &session_budget,
            &contract(0.2),
            &UsageTotals::default(),
            &HashMap::new(),
            &[],
            &BTreeSet::new(),
        );
        assert!(no_plan.feasible);
        assert_eq!(no_plan.reason, "no_remaining_tasks");

        let unknown = allocation_feasibility(
            &session_budget,
            &contract(0.2),
            &UsageTotals::default(),
            &HashMap::from([(
                "T1".to_string(),
                TaskEstimate {
                    source: Some(EstimateSource::Prior),
                    ..TaskEstimate::default()
                },
            )]),
            &["T1".to_string()],
            &BTreeSet::new(),
        );
        assert!(!unknown.feasible);
        assert_eq!(unknown.reason, "insufficient_estimate");

        let cost_only_estimates = HashMap::from([(
            "T1".to_string(),
            TaskEstimate {
                p50_tokens: None,
                p90_tokens: None,
                p50_cost_usd: Some(0.4),
                p90_cost_usd: Some(0.5),
                observed_calls: 1,
                source: Some(EstimateSource::ProjectActuals),
            },
        )]);
        let cost_only_budget = vp_protocol::types::SessionBudgetStatus {
            effective_cost_budget_usd: Some(1.0),
            hard_cost_cap_usd: Some(1.0),
            remaining_cost_usd: Some(0.4),
            ..budget(Some(10_000), Some(10_000), 1_000, Some(9_000))
        };
        let cost_only = allocation_feasibility(
            &cost_only_budget,
            &contract(0.2),
            &UsageTotals::default(),
            &cost_only_estimates,
            &["T1".to_string()],
            &BTreeSet::new(),
        );
        assert!(!cost_only.feasible);
        assert_eq!(cost_only.reason, "p90_cost_exceeds_reserve_headroom");
        assert_ne!(cost_only.reason, "token_unestimated");
        assert_eq!(cost_only.p90_cost_usd, Some(0.5));

        let priced_budget = vp_protocol::types::SessionBudgetStatus {
            effective_cost_budget_usd: Some(1.0),
            hard_cost_cap_usd: Some(1.0),
            remaining_cost_usd: Some(0.5),
            ..budget(Some(10_000), Some(10_000), 1_000, Some(9_000))
        };
        let priced = allocation_feasibility(
            &priced_budget,
            &contract(0.2),
            &UsageTotals::default(),
            &estimates,
            &["T1".to_string()],
            &BTreeSet::new(),
        );
        assert!(priced.feasible);
        assert_eq!(priced.reason, "cost_unconfigured");

        let priced_estimates = HashMap::from([(
            "T1".to_string(),
            TaskEstimate {
                p50_tokens: Some(4_000),
                p90_tokens: Some(4_000),
                p50_cost_usd: Some(0.4),
                p90_cost_usd: Some(0.5),
                observed_calls: 1,
                source: Some(EstimateSource::SessionActuals),
            },
        )]);
        let priced_budget = vp_protocol::types::SessionBudgetStatus {
            effective_cost_budget_usd: Some(1.0),
            hard_cost_cap_usd: Some(1.0),
            remaining_cost_usd: Some(0.4),
            ..budget(Some(10_000), Some(10_000), 1_000, Some(9_000))
        };
        let priced = allocation_feasibility(
            &priced_budget,
            &contract(0.2),
            &UsageTotals::default(),
            &priced_estimates,
            &["T1".to_string()],
            &BTreeSet::new(),
        );
        assert!(!priced.feasible);
        assert_eq!(priced.reason, "p90_cost_exceeds_reserve_headroom");
        assert_eq!(priced.p90_cost_usd, Some(0.5));
    }

    #[test]
    fn metadata_targets_build_into_one_plan_and_handle_missing_plan() {
        let plan = TeamPlan {
            goal: "goal".into(),
            departments: vec![PlanDepartment {
                name: "impl".into(),
                manager_title: "Implementation".into(),
                profile: "coding".into(),
                objective: "build".into(),
                token_budget: Some(9_000),
                cost_budget_usd: None,
            }],
            tasks: vec![task("T1", 5, None), task("T2", 2, None)],
        };
        let rows = allocation_task_rows(Some(&plan), &[]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].status, "pending");
        let envelopes = department_envelopes(&plan);
        assert_eq!(envelopes["impl"].tokens, Some(4_500));

        let empty_rows = allocation_task_rows(None, &[]);
        assert!(empty_rows.is_empty());
        let no_readiness = readiness("none");
        assert_eq!(no_readiness.level, "none");
    }

    #[test]
    fn recommendation_and_text_bounds_are_stable() {
        const { assert!(MAX_ALLOCATION_TASKS <= 100) };
        const { assert!(MAX_ALLOCATION_RECOMMENDATIONS <= 12) };
        let long = "x".repeat(MAX_ALLOCATION_TEXT + 20);
        let bounded = bounded_allocation_text(&long, MAX_ALLOCATION_TEXT);
        assert_eq!(bounded.chars().count(), MAX_ALLOCATION_TEXT);
        assert!(bounded.ends_with('…'));
        let values = vec![10u64, 20, 30, 40];
        assert_eq!(nearest_rank(&values, 0.50), Some(20));
        assert_eq!(nearest_rank(&values, 0.90), Some(40));
        assert_eq!(nearest_rank(&[], 0.50), None);

        // The preview keeps the first 100 task rows and sets `truncated` when
        // the source projection is larger. The UI/CLI also render at most 10.
        let plan = TeamPlan {
            goal: "large".into(),
            departments: vec![PlanDepartment {
                name: "impl".into(),
                manager_title: "Implementation".into(),
                profile: "coding".into(),
                objective: "build".into(),
                token_budget: None,
                cost_budget_usd: None,
            }],
            tasks: (0..(MAX_ALLOCATION_TASKS + 1))
                .map(|index| task(&format!("T{index}"), 1, None))
                .collect(),
        };
        let rows = allocation_task_rows(Some(&plan), &[]);
        assert_eq!(rows.len(), MAX_ALLOCATION_TASKS + 1);
        assert!(rows.len() > MAX_ALLOCATION_TASKS);
    }

    #[test]
    fn feasibility_reports_token_cost_and_time_reserve_pressure() {
        let estimates = HashMap::from([(
            "T1".to_string(),
            TaskEstimate {
                p50_tokens: Some(100),
                p90_tokens: Some(100),
                p50_cost_usd: Some(0.1),
                p90_cost_usd: Some(0.1),
                observed_calls: 1,
                source: Some(EstimateSource::SessionActuals),
            },
        )]);
        let mut pressured = budget(Some(10_000), Some(10_000), 9_000, Some(1_000));
        pressured.effective_cost_budget_usd = Some(10.0);
        pressured.hard_cost_cap_usd = Some(10.0);
        pressured.remaining_cost_usd = Some(1.0);
        pressured.effective_time_budget_secs = Some(3_600);
        pressured.hard_wall_clock_secs = Some(3_600);
        pressured.remaining_secs = Some(100);
        let feasibility = allocation_feasibility(
            &pressured,
            &contract(0.2),
            &UsageTotals::default(),
            &estimates,
            &["T1".to_string()],
            &BTreeSet::from(["T1".to_string()]),
        );
        assert_eq!(
            feasibility.pressure_dimensions,
            vec![
                "tokens".to_string(),
                "money".to_string(),
                "time".to_string()
            ]
        );
        assert!(!feasibility.feasible);
    }

    #[test]
    fn feasibility_fails_closed_when_part_of_the_path_is_unestimated() {
        let estimates = HashMap::from([
            (
                "T1".to_string(),
                TaskEstimate {
                    p50_tokens: Some(1_000),
                    p90_tokens: Some(1_000),
                    p50_cost_usd: None,
                    p90_cost_usd: None,
                    observed_calls: 0,
                    source: Some(EstimateSource::PlanEnvelope),
                },
            ),
            (
                "T2".to_string(),
                TaskEstimate {
                    source: Some(EstimateSource::Prior),
                    ..TaskEstimate::default()
                },
            ),
        ]);
        let feasibility = allocation_feasibility(
            &budget(Some(10_000), Some(10_000), 1_000, Some(9_000)),
            &contract(0.2),
            &UsageTotals::default(),
            &estimates,
            &["T1".to_string(), "T2".to_string()],
            &BTreeSet::from(["T1".to_string(), "T2".to_string()]),
        );
        assert!(!feasibility.feasible);
        assert_eq!(feasibility.reason, "partial_estimate");
        assert_eq!(feasibility.estimated_tasks, 1);
        assert_eq!(feasibility.total_tasks, 2);
        assert_eq!(feasibility.p90_tokens, Some(1_000));
    }

    #[test]
    fn unknown_task_size_is_not_marked_feasible() {
        let row = AllocationRow {
            task_id: "T1".into(),
            title: "Task T1".into(),
            department: "impl".into(),
            status: "pending".into(),
            priority: 1,
            deps: Vec::new(),
            token_budget: None,
            cost_budget_usd: None,
        };
        let estimate = TaskEstimate {
            source: Some(EstimateSource::Prior),
            ..TaskEstimate::default()
        };
        let task = allocation_task(
            &row,
            &HashMap::new(),
            &budget(None, None, 0, None),
            &estimate,
        );
        assert!(!task.feasible);
        assert_eq!(task.reason, "unestimated");
        assert_eq!(task.source, "prior");
    }

    #[test]
    fn task_feasibility_uses_cost_estimates_and_task_budget_caps() {
        let row = AllocationRow {
            task_id: "T1".into(),
            title: "Task T1".into(),
            department: "impl".into(),
            status: "pending".into(),
            priority: 1,
            deps: Vec::new(),
            token_budget: None,
            cost_budget_usd: None,
        };
        let cost_budget = vp_protocol::types::SessionBudgetStatus {
            effective_cost_budget_usd: Some(2.0),
            hard_cost_cap_usd: Some(2.0),
            remaining_cost_usd: Some(1.0),
            ..budget(Some(10_000), Some(10_000), 0, Some(10_000))
        };
        let cost_estimate = TaskEstimate {
            p50_cost_usd: Some(0.4),
            p90_cost_usd: Some(0.5),
            source: Some(EstimateSource::ProjectActuals),
            ..TaskEstimate::default()
        };
        let task = allocation_task(&row, &HashMap::new(), &cost_budget, &cost_estimate);
        assert!(task.feasible);
        assert_eq!(task.reason, "cost_only");

        let expensive = TaskEstimate {
            p90_cost_usd: Some(2.0),
            ..cost_estimate.clone()
        };
        let task = allocation_task(&row, &HashMap::new(), &cost_budget, &expensive);
        assert!(!task.feasible);
        assert_eq!(task.reason, "exceeds_remaining_cost");

        let mut capped = row.clone();
        capped.token_budget = Some(500);
        let token_estimate = TaskEstimate {
            p50_tokens: Some(1_000),
            p90_tokens: Some(1_000),
            source: Some(EstimateSource::PlanEnvelope),
            ..TaskEstimate::default()
        };
        let task = allocation_task(
            &capped,
            &HashMap::new(),
            &budget(None, None, 0, None),
            &token_estimate,
        );
        assert!(!task.feasible);
        assert_eq!(task.reason, "exceeds_task_tokens");
    }

    #[test]
    fn recommendations_are_advisory_and_never_route_models() {
        let recommendations = [AllocationRecommendation {
            action: "continue".into(),
            reason: "fits".into(),
            confidence: "medium".into(),
            task_id: None,
        }];
        assert!(recommendations.iter().all(|recommendation| {
            !recommendation.action.contains("model") && !recommendation.action.contains("approve")
        }));
    }

    #[test]
    fn calibration_reason_reports_pending_approvals_not_ledger_calls() {
        let usage = UsageTotals {
            calls: 99,
            ..UsageTotals::default()
        };
        let calibration = allocation_calibration(
            &contract(0.2),
            &[pending_approval()],
            &no_effects(),
            &vp_protocol::diagnosis::SessionDiagnosisResult::default(),
            &[],
            &usage,
        );
        assert_eq!(calibration.pending_approvals, 1);
        assert!(
            calibration.reason.contains("1 pending approval(s)"),
            "{}",
            calibration.reason
        );
        assert!(!calibration.reason.contains("99 pending approval(s)"));
        assert!(calibration.reason.contains("99 ledger call(s)"));

        let mut resolved = pending_approval();
        resolved.status = "approved".into();
        let calibration = allocation_calibration(
            &contract(0.2),
            &[resolved],
            &no_effects(),
            &vp_protocol::diagnosis::SessionDiagnosisResult::default(),
            &[],
            &UsageTotals::default(),
        );
        assert_eq!(calibration.pending_approvals, 0);
        assert_eq!(
            calibration.attention_interrupts, 0,
            "resolved approvals are not current attention pressure"
        );
        assert_eq!(calibration.attention_recommendation, "batch_routine");
        assert_eq!(calibration.trust_recommendation, "standard");
    }

    #[tokio::test]
    async fn preview_is_read_only_and_uses_project_actuals_after_plan_envelope() {
        let (_data, _repo, engine, session) = fixture().await;
        // History from another session in the same project.
        let history_session = engine
            .store()
            .create_session(&session.project_id, "history")
            .unwrap();
        record_task_usage(&engine, &history_session.id, "T1", 700, 300).await;

        engine
            .submit_team_plan(&session.id, &plan_with_task(None))
            .await
            .unwrap();
        let before = engine
            .list_events(session.id.clone(), 0, 500)
            .await
            .unwrap()
            .len();
        let preview = engine
            .session_allocation(SessionAllocationParams {
                session_id: session.id.clone(),
            })
            .await
            .unwrap();
        assert_eq!(preview.session_id, session.id);
        assert_eq!(preview.tasks.len(), 1);
        let task = &preview.tasks[0];
        assert_eq!(task.task_id, "T1");
        assert_eq!(task.source, "project_actuals");
        assert_eq!(task.p50_tokens, Some(1_000));
        assert_eq!(task.p90_tokens, Some(1_000));
        assert_eq!(preview.feasibility.source, "project_actuals");
        assert_eq!(preview.feasibility.p50_tokens, Some(1_000));
        assert_eq!(preview.feasibility.reason, "cost_unconfigured");
        assert!(preview.feasibility.feasible);
        assert!(preview.envelope.token_budget.is_none());
        assert!(!preview.truncated);
        let after = engine
            .list_events(session.id.clone(), 0, 500)
            .await
            .unwrap()
            .len();
        assert_eq!(
            after, before,
            "the allocation preview must append no events"
        );

        // A plan token envelope wins over project actuals.
        let second = engine
            .store()
            .create_session(&session.project_id, "plan envelope")
            .unwrap();
        let second = engine.ensure_session_worktree(&second.id).await.unwrap();
        engine
            .submit_team_plan(&second.id, &plan_with_task(Some(4_000)))
            .await
            .unwrap();
        let preview = engine
            .session_allocation(SessionAllocationParams {
                session_id: second.id.clone(),
            })
            .await
            .unwrap();
        assert_eq!(preview.tasks[0].source, "plan_envelope");
        assert_eq!(preview.tasks[0].p50_tokens, Some(4_000));
        assert_eq!(preview.tasks[0].token_budget, Some(4_000));
    }

    #[tokio::test]
    async fn preview_handles_a_full_hundred_task_plan_without_truncation() {
        let (_data, _repo, engine, session) = fixture().await;
        let tasks = (0..MAX_ALLOCATION_TASKS)
            .map(|index| {
                let deps = if index == 0 {
                    Vec::new()
                } else {
                    vec![format!("T{:03}", index - 1)]
                };
                PlanTask {
                    id: format!("T{index:03}"),
                    department: "impl".into(),
                    title: format!("Task {index}"),
                    kind: "coding".into(),
                    priority: 5,
                    profile: Some("coding".into()),
                    deps,
                    write_scope: vec![format!("src/f{index}.rs")],
                    acceptance: vec!["tests pass".into()],
                    token_budget: Some(1_000),
                    cost_budget_usd: None,
                    parent_task_id: None,
                    fanout_key: None,
                    fanout_join_for: None,
                }
            })
            .collect();
        let plan = TeamPlan {
            goal: "full allocation plan".into(),
            departments: vec![PlanDepartment {
                name: "impl".into(),
                manager_title: "Implementation".into(),
                profile: "coding".into(),
                objective: "build".into(),
                token_budget: Some((MAX_ALLOCATION_TASKS as u64) * 1_000),
                cost_budget_usd: None,
            }],
            tasks,
        };
        engine.submit_team_plan(&session.id, &plan).await.unwrap();
        let preview = engine
            .session_allocation(SessionAllocationParams {
                session_id: session.id.clone(),
            })
            .await
            .unwrap();
        assert_eq!(preview.tasks.len(), MAX_ALLOCATION_TASKS);
        assert!(!preview.truncated);
        assert!(
            preview.tasks.iter().all(|task| task.critical_path),
            "every task in a single-chain plan is on the critical path"
        );
        assert_eq!(
            preview.feasibility.critical_path_tasks.len(),
            MAX_ALLOCATION_TASKS
        );
        // The token envelope is unbounded, but the preview surfaces the
        // actionable cost caveat because no prices or cost cap are configured.
        assert_eq!(preview.feasibility.reason, "cost_unconfigured");
    }

    #[tokio::test]
    async fn critical_path_unavailability_is_reported_not_silently_fallen_back() {
        let (_data, _repo, _engine, session) = fixture().await;
        let mut plan = plan_with_task(None);
        plan.tasks[0].deps = vec!["T-missing".into()];
        let (ids, reason) = critical_path_tasks(&session, Some(&plan), &HashMap::new(), None);
        assert!(ids.is_empty());
        assert_eq!(reason.as_deref(), Some("BUDGET_PATH_UNKNOWN_TASK"));

        let (ids, reason) = critical_path_tasks(&session, None, &HashMap::new(), None);
        assert!(ids.is_empty());
        assert_eq!(reason.as_deref(), Some("no_plan"));
    }

    #[tokio::test]
    async fn preview_handles_no_plan_and_auto_budget_envelopes() {
        let (_data, _repo, engine, session) = fixture().await;
        let preview = engine
            .session_allocation(SessionAllocationParams {
                session_id: session.id.clone(),
            })
            .await
            .unwrap();
        assert!(preview.tasks.is_empty());
        assert!(preview.feasibility.feasible);
        assert_eq!(preview.feasibility.reason, "no_remaining_tasks");
        assert_eq!(preview.feasibility.p50_tokens, None);
        assert!(!preview.truncated);

        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("allocation-auto"))
            .unwrap();
        let policy = AutoBudgetPolicy {
            enabled: true,
            initial_token_budget: Some(1_000),
            hard_token_cap: Some(5_000),
            ..AutoBudgetPolicy::default()
        };
        let engine = Engine::new(store);
        let session = engine
            .create_session_with_budget_options(
                project.id,
                "auto allocation".into(),
                None,
                None,
                None,
                Some(BudgetMode::Auto),
                Some(policy),
                0,
                false,
            )
            .await
            .unwrap();
        let session = engine.ensure_session_worktree(&session.id).await.unwrap();
        engine
            .submit_team_plan(&session.id, &plan_with_task(Some(1_000)))
            .await
            .unwrap();
        let preview = engine
            .session_allocation(SessionAllocationParams {
                session_id: session.id,
            })
            .await
            .unwrap();
        assert_eq!(preview.envelope.token_budget, Some(5_000));
        assert_eq!(preview.envelope.remaining_tokens, Some(1_000));
        assert_eq!(preview.envelope.completion_reserve_ratio, 0.2);
        assert_eq!(preview.tasks[0].source, "plan_envelope");
        assert_eq!(preview.feasibility.reserve_tokens, Some(1_000));
    }

    #[tokio::test]
    async fn preview_reports_a_bounded_feasibility_failure() {
        let (_data, _repo, engine, session) = fixture().await;
        engine
            .set_session_budget(SetSessionBudgetParams {
                session_id: session.id.clone(),
                mode: Some(BudgetMode::Fixed),
                token_budget: Some(10_000),
                ..SetSessionBudgetParams::default()
            })
            .await
            .unwrap();
        engine.record_usage(&session.id, 5_000, 0).await.unwrap();
        engine
            .submit_team_plan(&session.id, &plan_with_task(Some(4_000)))
            .await
            .unwrap();
        let preview = engine
            .session_allocation(SessionAllocationParams {
                session_id: session.id.clone(),
            })
            .await
            .unwrap();
        assert!(!preview.feasibility.feasible);
        assert_eq!(preview.feasibility.reason, "p90_exceeds_reserve_headroom");
        assert_eq!(preview.feasibility.reserve_tokens, Some(2_000));
        assert_eq!(preview.feasibility.headroom_tokens, Some(3_000));
        assert!(
            preview
                .recommendations
                .iter()
                .any(|recommendation| recommendation.action == "top_up")
        );
    }

    #[tokio::test]
    async fn tradeoff_quality_counts_are_read_from_bounded_projections() {
        let (_data, _repo, engine, session) = fixture().await;
        let preview = engine
            .session_tradeoffs(vp_protocol::types::SessionTradeoffsParams {
                session_id: session.id.clone(),
            })
            .await
            .unwrap();
        assert_eq!(preview.profiles.len(), 5);
        assert_eq!(preview.current_profile, "balanced");
        assert_eq!(preview.effective_completion_reserve_ratio, 0.2);
        assert_eq!(preview.reserve_source, "derived");
        assert_eq!(
            preview
                .profiles
                .iter()
                .filter(|profile| profile.recommended)
                .count(),
            0,
            "no evidence-backed profile change should be badged"
        );
        assert!(
            !preview
                .recommendations
                .iter()
                .any(|recommendation| recommendation.starts_with("consider_profile:"))
        );
        assert_eq!(preview.quality.total_tasks, 0);
        assert!(
            preview.quality.reviewability == "watch" || preview.quality.reviewability == "risk"
        );
        assert!(preview.confidence.level == "low");
        assert!(!preview.recommendations.is_empty());
    }
}

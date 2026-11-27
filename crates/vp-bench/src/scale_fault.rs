//! Deterministic scale and fault coverage for release readiness.
//!
//! This module has two deliberately separate layers:
//!
//! * **Real runtime scale**: a 100-task `TeamPlan` is written through the real
//!   `vp-core` plan revision path (which runs `validate_team_plan`), projected
//!   into the real SQLite task scheduler, and driven through `TaskScheduler`
//!   claim/finish transitions with synthetic completions. It does not start 100
//!   Worker agents, execute tool calls, spend provider tokens, or merge Git
//!   branches. The dispatch loop is bounded and deterministic.
//! * **Fault injection**: provider stream retry, provider output truncation, and
//!   cooperative cancellation run a real `AgentService` session with an
//!   in-process fault-injecting fake provider. Duplicate tool-call replay and
//!   lost-task/batch-abort settlement are deterministic harness models of
//!   runtime policies that are private to `vp-core`; they are labeled as such
//!   and are not live-runtime proofs.
//!
//! Nothing here performs network I/O, reads provider credentials, or requires a
//! daemon. Every case is intended to run in CI in a few seconds.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use vp_core::Engine;
use vp_core::agent::{AgentConfig, AgentService, CapacityController, ScopeLocks};
use vp_core::scheduler::TaskScheduler;
use vp_core::tools::ToolRegistry;
use vp_protocol::types::{
    ApprovalMode, PlanDepartment, PlanTask, RoleModelConfig, RoleModels, TeamPlan,
};
use vp_providers::{ChatRequest, EventStream, Provider, ProviderError, StreamEvent};
use vp_store::Store;

use crate::fixture::{BenchWorkspace, FixturePlan, materialize_repo};
use crate::provider::{FakeProvider, FakeUsageModel, ScriptAction};
use crate::scenario::FixtureKind;

/// Stable schema id for scale/fault reports.
pub const SCALE_FAULT_SCHEMA: &str = "verlinpark.bench.scale-fault.v1";
/// The fixed scale-plan task count used by the release-readiness suite.
pub const SCALE_TASK_COUNT: usize = 100;
/// Dependency layers in the fixed scale plan.
pub const SCALE_LAYERS: usize = 10;
/// Tasks per dependency layer.
pub const SCALE_TASKS_PER_LAYER: usize = 10;
/// Departments in the fixed scale plan.
pub const SCALE_DEPARTMENTS: usize = 4;
/// Maximum Worker cap accepted by the runtime and this harness.
pub const SCALE_MAX_WORKERS: usize = 10;

/// One named pass/fail assertion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchCheck {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

impl BenchCheck {
    fn new(name: impl Into<String>, passed: bool, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            passed,
            detail: detail.into(),
        }
    }
}

fn all_passed(checks: &[BenchCheck]) -> bool {
    !checks.is_empty() && checks.iter().all(|check| check.passed)
}

fn check_counts(checks: &[BenchCheck]) -> (usize, usize, usize) {
    let passed = checks.iter().filter(|check| check.passed).count();
    (checks.len(), passed, checks.len() - passed)
}

/// One deterministic scheduler dispatch batch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScaleBatch {
    pub index: usize,
    pub task_ids: Vec<String>,
    /// Flattened task write scopes; tasks in one batch are pairwise disjoint.
    pub write_scopes: Vec<String>,
}

/// Result of the 100-task plan validation and scheduled-dispatch harness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScaleReport {
    pub schema: String,
    pub task_count: usize,
    pub max_workers: usize,
    pub dependency_layers: usize,
    pub department_count: usize,
    pub batch_count: usize,
    pub peak_batch: usize,
    pub completed_tasks: usize,
    pub batches: Vec<ScaleBatch>,
    pub checks: Vec<BenchCheck>,
    pub checks_total: usize,
    pub checks_passed: usize,
    pub checks_failed: usize,
    pub passed: bool,
    pub caveats: Vec<String>,
}

/// Boundary classification for every fault case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FaultBoundary {
    /// A real `AgentService` run with an in-process fake provider.
    RealRuntimeProvider,
    /// A deterministic model of a runtime policy that is private to `vp-core`.
    HarnessModel,
}

impl FaultBoundary {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RealRuntimeProvider => "real-runtime-fake-provider",
            Self::HarnessModel => "harness-model-not-live-runtime",
        }
    }
}

/// One fault-injection case, including its honest execution boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FaultCase {
    pub id: String,
    pub title: String,
    pub boundary: FaultBoundary,
    pub boundary_note: String,
    pub attempts: usize,
    pub final_state: String,
    pub merged: bool,
    pub checks: Vec<BenchCheck>,
    pub passed: bool,
}

impl FaultCase {
    fn from_checks(
        id: &str,
        title: &str,
        boundary: FaultBoundary,
        boundary_note: &str,
        outcome: FaultOutcome,
        checks: Vec<BenchCheck>,
    ) -> Self {
        let passed = all_passed(&checks);
        Self {
            id: id.to_string(),
            title: title.to_string(),
            boundary,
            boundary_note: boundary_note.to_string(),
            attempts: outcome.attempts,
            final_state: outcome.final_state,
            merged: outcome.merged,
            checks,
            passed,
        }
    }

    fn failed(
        id: &str,
        title: &str,
        boundary: FaultBoundary,
        boundary_note: &str,
        error: &anyhow::Error,
    ) -> Self {
        Self::from_checks(
            id,
            title,
            boundary,
            boundary_note,
            FaultOutcome {
                attempts: 0,
                final_state: "harness_error".to_string(),
                merged: false,
            },
            vec![BenchCheck::new(
                "case_executed",
                false,
                format!("{error:#}"),
            )],
        )
    }
}

struct FaultOutcome {
    attempts: usize,
    final_state: String,
    merged: bool,
}

/// Result of the deterministic fault matrix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FaultReport {
    pub schema: String,
    pub cases: Vec<FaultCase>,
    pub case_count: usize,
    pub cases_passed: usize,
    pub cases_failed: usize,
    pub checks_total: usize,
    pub checks_passed: usize,
    pub checks_failed: usize,
    pub passed: bool,
    pub caveats: Vec<String>,
}

impl FaultReport {
    fn new(cases: Vec<FaultCase>) -> Self {
        let checks = cases
            .iter()
            .flat_map(|case| case.checks.iter().cloned())
            .collect::<Vec<_>>();
        let (checks_total, checks_passed, checks_failed) = check_counts(&checks);
        let cases_passed = cases.iter().filter(|case| case.passed).count();
        let passed = !cases.is_empty() && cases.len() == cases_passed && checks_failed == 0;
        Self {
            schema: SCALE_FAULT_SCHEMA.to_string(),
            case_count: cases.len(),
            cases_passed,
            cases_failed: cases.len() - cases_passed,
            cases,
            checks_total,
            checks_passed,
            checks_failed,
            passed,
            caveats: fault_caveats(),
        }
    }
}

/// Validate the worker cap accepted by the scale harness.
pub fn validate_workers(workers: usize) -> Result<()> {
    if workers == 0 || workers > SCALE_MAX_WORKERS {
        bail!("workers must be 1-{SCALE_MAX_WORKERS}");
    }
    Ok(())
}

/// Run the real `vp-core` 100-task validation and scheduler-dispatch harness.
///
/// This performs no provider call and starts no Worker agent. It uses the
/// production plan validator, task projection, and `TaskScheduler` state
/// transitions, then marks claimed tasks completed synthetically.
pub async fn run_scale_report(workers: usize) -> Result<ScaleReport> {
    validate_workers(workers)?;
    let workspace = BenchWorkspace::create()?;
    let fixture = FixturePlan::new(FixtureKind::Parallel)
        .file("README.md", "deterministic scale fixture; no live work\n");
    materialize_repo(&fixture, &workspace)?;
    let store = Store::open(workspace.data_dir())?;
    let engine = Engine::new(store.clone());
    let project = engine
        .add_project(
            workspace.repo().to_string_lossy().to_string(),
            Some("vp-bench scale fixture".to_string()),
        )
        .await?;
    let session = engine
        .create_session(
            project.id.clone(),
            "deterministic 100-task scale harness".to_string(),
            None,
        )
        .await?;

    let mut checks = Vec::new();
    let seed = seed_plan();
    engine.submit_team_plan(&session.id, &seed).await?;
    let revision = engine.current_plan_revision(&session.id).await?;
    checks.push(BenchCheck::new(
        "seed_plan_persisted",
        revision == 1,
        format!("plan revision {revision}"),
    ));

    let plan = scale_plan();
    let mut too_large = plan.clone();
    too_large.tasks.push(extra_task("T-EXTRA"));
    let error = engine
        .revise_team_plan(
            &session.id,
            too_large,
            "scale 101-task validation",
            "scale-harness",
            Some(revision),
            SCALE_DEPARTMENTS,
        )
        .await
        .err()
        .context("a 101-task plan was accepted by the real vp-core validator")?;
    checks.push(BenchCheck::new(
        "plan_validation_rejects_101_tasks",
        error.to_string().contains("at most 100 tasks"),
        error.to_string(),
    ));
    checks.push(
        revision_unchanged(&engine, &session.id, revision, "after_101_task_rejection").await?,
    );

    let mut duplicate = plan.clone();
    duplicate.tasks[1].id = plan.tasks[0].id.clone();
    let error = engine
        .revise_team_plan(
            &session.id,
            duplicate,
            "scale duplicate-id validation",
            "scale-harness",
            Some(revision),
            SCALE_DEPARTMENTS,
        )
        .await
        .err()
        .context("a duplicate-id plan was accepted by the real vp-core validator")?;
    checks.push(BenchCheck::new(
        "plan_validation_rejects_duplicate_ids",
        error.to_string().contains("duplicate task id"),
        error.to_string(),
    ));
    checks.push(
        revision_unchanged(&engine, &session.id, revision, "after_duplicate_rejection").await?,
    );

    let mut cyclic = plan.clone();
    cyclic.tasks[0].deps = vec![
        cyclic
            .tasks
            .last()
            .map(|task| task.id.clone())
            .unwrap_or_default(),
    ];
    let error = engine
        .revise_team_plan(
            &session.id,
            cyclic,
            "scale cycle validation",
            "scale-harness",
            Some(revision),
            SCALE_DEPARTMENTS,
        )
        .await
        .err()
        .context("a cyclic plan was accepted by the real vp-core validator")?;
    checks.push(BenchCheck::new(
        "plan_validation_rejects_cycles",
        error.to_string().contains("cycle"),
        error.to_string(),
    ));
    checks.push(revision_unchanged(&engine, &session.id, revision, "after_cycle_rejection").await?);

    let revised = engine
        .revise_team_plan(
            &session.id,
            plan.clone(),
            "scale 100-task plan",
            "scale-harness",
            Some(revision),
            SCALE_DEPARTMENTS,
        )
        .await?;
    checks.push(BenchCheck::new(
        "plan_validation_accepts_100_tasks",
        revised.plan.tasks.len() == SCALE_TASK_COUNT && revised.revision.revision == revision + 1,
        format!(
            "{} tasks at revision {}",
            revised.plan.tasks.len(),
            revised.revision.revision
        ),
    ));

    let queue = engine
        .task_queue(Some(project.id.clone()), Some(session.id.clone()))
        .await?;
    let rows = queue.tasks;
    let plan_ids = plan
        .tasks
        .iter()
        .map(|task| task.id.clone())
        .collect::<BTreeSet<_>>();
    let row_ids = rows
        .iter()
        .map(|row| row.task_id.clone())
        .collect::<BTreeSet<_>>();
    checks.push(BenchCheck::new(
        "queue_projection_has_all_100_tasks",
        rows.len() == SCALE_TASK_COUNT && row_ids == plan_ids,
        format!("{} projected rows, {} plan ids", rows.len(), plan_ids.len()),
    ));
    checks.push(BenchCheck::new(
        "queue_projection_has_no_duplicate_ids",
        row_ids.len() == rows.len(),
        format!("{} unique ids for {} rows", row_ids.len(), rows.len()),
    ));
    let deps_match = plan.tasks.iter().all(|task| {
        rows.iter()
            .find(|row| row.task_id == task.id)
            .map(|row| row.deps == task.deps)
            .unwrap_or(false)
    });
    checks.push(BenchCheck::new(
        "queue_dependencies_match_the_plan",
        deps_match,
        "every projected row carries the plan dependency list",
    ));
    let acyclic = acyclic(&plan);
    checks.push(BenchCheck::new(
        "dependency_graph_is_acyclic",
        acyclic,
        format!("{} tasks, {} layers", plan_ids.len(), SCALE_LAYERS),
    ));

    let plan_seq = rows
        .iter()
        .map(|row| row.plan_seq)
        .max()
        .unwrap_or_default();
    let scheduler = TaskScheduler::new(store.clone());
    let mut completed = BTreeSet::new();
    let mut batches = Vec::new();
    let mut progress_guard = 0usize;
    while completed.len() < SCALE_TASK_COUNT {
        progress_guard += 1;
        if progress_guard > SCALE_TASK_COUNT + SCALE_LAYERS {
            bail!(
                "scale dispatch made no progress after {} batches with {}/{} tasks completed",
                progress_guard,
                completed.len(),
                SCALE_TASK_COUNT
            );
        }
        let ready = scheduler
            .ready_tasks(&session, plan_seq, &plan, SCALE_TASK_COUNT)
            .await?;
        if ready.is_empty() {
            bail!(
                "scale dispatch has {} ready tasks with {}/{} completed; remaining tasks are lost or blocked",
                ready.len(),
                completed.len(),
                SCALE_TASK_COUNT
            );
        }
        let dependencies_ready = ready
            .iter()
            .all(|task| task.deps.iter().all(|dep| completed.contains(dep)));
        if !dependencies_ready {
            bail!("the real scheduler returned a task whose dependencies are not satisfied");
        }
        let batch = select_non_overlapping_batch(ready, workers);
        if batch.is_empty() {
            bail!("the non-overlap batch selector returned no tasks");
        }
        if batch.len() > workers {
            bail!(
                "batch size {} exceeds the configured Worker cap {workers}",
                batch.len()
            );
        }
        if !pairwise_disjoint(&batch) {
            bail!("the selected batch contains overlapping write scopes");
        }
        let ids = batch.iter().map(|task| task.id.clone()).collect::<Vec<_>>();
        let (claimed, _) = scheduler.claim(&session.id, plan_seq, &ids).await?;
        if claimed.len() != ids.len() {
            bail!(
                "scheduler claimed {} of {} selected tasks",
                claimed.len(),
                ids.len()
            );
        }
        for task_id in &ids {
            let finished = scheduler
                .finish(
                    &session.id,
                    plan_seq,
                    task_id,
                    "completed",
                    Some("scale harness synthetic completion"),
                )
                .await?;
            if finished.is_none() {
                bail!("scheduler could not complete claimed task {task_id}");
            }
            completed.insert(task_id.clone());
        }
        batches.push(ScaleBatch {
            index: batches.len(),
            task_ids: ids,
            write_scopes: batch
                .iter()
                .flat_map(|task| task.write_scope.clone())
                .collect(),
        });
    }

    let final_queue = engine
        .task_queue(Some(project.id.clone()), Some(session.id.clone()))
        .await?;
    let final_rows = final_queue.tasks;
    let completed_rows = final_rows
        .iter()
        .filter(|row| row.status == "completed")
        .count();
    checks.push(BenchCheck::new(
        "all_100_tasks_completed",
        completed.len() == SCALE_TASK_COUNT
            && completed_rows == SCALE_TASK_COUNT
            && completed == plan_ids,
        format!(
            "{} scheduler completions, {} completed rows",
            completed.len(),
            completed_rows
        ),
    ));
    let dispatched = batches
        .iter()
        .flat_map(|batch| batch.task_ids.iter().cloned())
        .collect::<BTreeSet<_>>();
    checks.push(BenchCheck::new(
        "no_task_lost_or_dispatched_twice",
        dispatched == plan_ids
            && batches
                .iter()
                .map(|batch| batch.task_ids.len())
                .sum::<usize>()
                == SCALE_TASK_COUNT,
        format!(
            "{} dispatched ids for {} plan ids",
            dispatched.len(),
            plan_ids.len()
        ),
    ));
    let peak_batch = batches
        .iter()
        .map(|batch| batch.task_ids.len())
        .max()
        .unwrap_or(0);
    checks.push(BenchCheck::new(
        "dispatch_respects_the_worker_cap",
        peak_batch <= workers,
        format!("peak batch {peak_batch}, cap {workers}"),
    ));
    checks.push(BenchCheck::new(
        "dependency_batches_are_used",
        batches.len() == expected_batch_count(workers)
            && peak_batch == workers.min(SCALE_TASKS_PER_LAYER),
        format!(
            "{} batches ({} expected), peak {peak_batch}",
            batches.len(),
            expected_batch_count(workers)
        ),
    ));
    let scopes_disjoint = batches.iter().all(|batch| {
        let tasks = batch
            .task_ids
            .iter()
            .filter_map(|id| plan.tasks.iter().find(|task| &task.id == id))
            .cloned()
            .collect::<Vec<_>>();
        pairwise_disjoint(&tasks)
    });
    checks.push(BenchCheck::new(
        "no_overlapping_write_scope_in_a_batch",
        scopes_disjoint,
        "every dispatch batch has pairwise-disjoint normalized write scopes",
    ));

    let (checks_total, checks_passed, checks_failed) = check_counts(&checks);
    let passed = checks_failed == 0 && checks_total > 0;
    Ok(ScaleReport {
        schema: SCALE_FAULT_SCHEMA.to_string(),
        task_count: SCALE_TASK_COUNT,
        max_workers: workers,
        dependency_layers: SCALE_LAYERS,
        department_count: SCALE_DEPARTMENTS,
        batch_count: batches.len(),
        peak_batch,
        completed_tasks: completed.len(),
        batches,
        checks,
        checks_total,
        checks_passed,
        checks_failed,
        passed,
        caveats: scale_caveats(),
    })
}

/// Run the fault matrix.
///
/// The provider cases use a real `AgentService` session and an in-process fake
/// provider. The duplicate-tool and batch-abort cases are harness models and
/// say so in their boundary fields.
pub async fn run_fault_report() -> Result<FaultReport> {
    let mut cases = Vec::new();
    cases.push(
        run_retry_before_output_case()
            .await
            .unwrap_or_else(|error| {
                FaultCase::failed(
                    "provider-stream-failure-before-output",
                    "Provider stream failure before output is retried without partial persistence",
                    FaultBoundary::RealRuntimeProvider,
                    "real AgentService run with an in-process fake provider",
                    &error,
                )
            }),
    );
    cases.push(run_truncation_case().await.unwrap_or_else(|error| {
        FaultCase::failed(
            "provider-output-truncation",
            "Provider output truncation is bounded and fails closed without a merge",
            FaultBoundary::RealRuntimeProvider,
            "real AgentService run with an in-process fake provider",
            &error,
        )
    }));
    cases.push(run_cancellation_case().await.unwrap_or_else(|error| {
        FaultCase::failed(
            "abort-boundary-cancellation",
            "Cooperative cancellation stops the run without merging incomplete work",
            FaultBoundary::RealRuntimeProvider,
            "real AgentService run with an in-process fake provider",
            &error,
        )
    }));
    cases.push(duplicate_tool_call_case());
    cases.push(lost_task_batch_abort_case());
    Ok(FaultReport::new(cases))
}

async fn revision_unchanged(
    engine: &Engine,
    session_id: &str,
    expected: u32,
    suffix: &str,
) -> Result<BenchCheck> {
    let current = engine.current_plan_revision(session_id).await?;
    Ok(BenchCheck::new(
        format!("rejected_revision_does_not_change_the_plan_{suffix}"),
        current == expected,
        format!("revision {current}, expected {expected}"),
    ))
}

fn seed_plan() -> TeamPlan {
    TeamPlan {
        goal: "seed the scale session".to_string(),
        departments: vec![scale_department(0)],
        tasks: vec![PlanTask {
            id: "SEED".to_string(),
            department: scale_department_name(0),
            title: "Seed task".to_string(),
            kind: "coding".to_string(),
            priority: 0,
            profile: Some("coding".to_string()),
            deps: Vec::new(),
            write_scope: vec!["src/seed.txt".to_string()],
            acceptance: vec!["seed exists".to_string()],
            token_budget: None,
            cost_budget_usd: None,
            parent_task_id: None,
            fanout_key: None,
            fanout_join_for: None,
        }],
    }
}

fn scale_department(index: usize) -> PlanDepartment {
    PlanDepartment {
        name: scale_department_name(index),
        manager_title: format!("Scale Department {index}"),
        profile: "coding".to_string(),
        objective: format!("Implement deterministic scale layer slices for department {index}"),
        token_budget: None,
        cost_budget_usd: None,
    }
}

fn scale_department_name(index: usize) -> String {
    format!("scale-{index}")
}

fn scale_plan() -> TeamPlan {
    let mut tasks = Vec::with_capacity(SCALE_TASK_COUNT);
    for layer in 0..SCALE_LAYERS {
        for index in 0..SCALE_TASKS_PER_LAYER {
            let id = scale_task_id(layer, index);
            let deps = if layer == 0 {
                Vec::new()
            } else {
                (0..SCALE_TASKS_PER_LAYER)
                    .map(|previous| scale_task_id(layer - 1, previous))
                    .collect()
            };
            let scope = format!("src/layer-{layer}/task-{index}.txt");
            tasks.push(PlanTask {
                id,
                department: scale_department_name(layer % SCALE_DEPARTMENTS),
                title: format!("Scale layer {layer} task {index}"),
                kind: "coding".to_string(),
                priority: 0,
                profile: Some("coding".to_string()),
                deps,
                write_scope: vec![scope],
                acceptance: vec![format!("layer {layer} task {index} is complete")],
                token_budget: None,
                cost_budget_usd: None,
                parent_task_id: None,
                fanout_key: None,
                fanout_join_for: None,
            });
        }
    }
    TeamPlan {
        goal: "deterministic 100-task scale plan".to_string(),
        departments: (0..SCALE_DEPARTMENTS).map(scale_department).collect(),
        tasks,
    }
}

fn scale_task_id(layer: usize, index: usize) -> String {
    format!("T{layer:02}{index:02}")
}

fn expected_batch_count(workers: usize) -> usize {
    SCALE_LAYERS * SCALE_TASKS_PER_LAYER.div_ceil(workers)
}

fn extra_task(id: &str) -> PlanTask {
    PlanTask {
        id: id.to_string(),
        department: scale_department_name(0),
        title: "Extra task".to_string(),
        kind: "coding".to_string(),
        priority: 0,
        profile: Some("coding".to_string()),
        deps: Vec::new(),
        write_scope: vec![format!("src/extra/{id}.txt")],
        acceptance: vec!["extra task is complete".to_string()],
        token_budget: None,
        cost_budget_usd: None,
        parent_task_id: None,
        fanout_key: None,
        fanout_join_for: None,
    }
}

fn acyclic(plan: &TeamPlan) -> bool {
    let mut indegree = BTreeMap::new();
    let mut dependents: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for task in &plan.tasks {
        indegree.insert(task.id.clone(), task.deps.len());
        for dep in &task.deps {
            dependents
                .entry(dep.clone())
                .or_default()
                .push(task.id.clone());
        }
    }
    let mut ready = indegree
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(id, _)| id.clone())
        .collect::<VecDeque<_>>();
    let mut visited = 0usize;
    while let Some(id) = ready.pop_front() {
        visited += 1;
        if let Some(children) = dependents.get(&id) {
            for child in children {
                if let Some(degree) = indegree.get_mut(child) {
                    *degree = degree.saturating_sub(1);
                    if *degree == 0 {
                        ready.push_back(child.clone());
                    }
                }
            }
        }
    }
    visited == plan.tasks.len()
}

/// Mirror of the documented `vp-core` ready-batch scope filter. `vp-core` keeps
/// these helpers crate-private, so the harness uses the same normalization and
/// overlap rules explicitly.
fn normalized_scope(scope: &str) -> String {
    scope
        .trim()
        .trim_start_matches("./")
        .trim_end_matches("/**")
        .trim_end_matches('/')
        .to_string()
}

fn scopes_overlap(left: &[String], right: &[String]) -> bool {
    if left.is_empty() || right.is_empty() {
        return true;
    }
    let unrestricted = |scope: &str| matches!(normalized_scope(scope).as_str(), "*" | "**");
    if left.iter().any(|scope| unrestricted(scope)) || right.iter().any(|scope| unrestricted(scope))
    {
        return true;
    }
    left.iter().any(|left| {
        let left = normalized_scope(left);
        right.iter().any(|right| {
            let right = normalized_scope(right);
            left == right
                || left.starts_with(&format!("{right}/"))
                || right.starts_with(&format!("{left}/"))
        })
    })
}

fn select_non_overlapping_batch(ready: Vec<PlanTask>, limit: usize) -> Vec<PlanTask> {
    let mut batch = Vec::new();
    for task in ready {
        if batch.len() >= limit {
            break;
        }
        let scope = task.write_scope.clone();
        if !batch.is_empty() {
            if scope.is_empty() {
                continue;
            }
            let overlaps = batch
                .iter()
                .any(|existing: &PlanTask| scopes_overlap(&scope, &existing.write_scope));
            if overlaps {
                continue;
            }
        }
        batch.push(task);
    }
    batch
}

fn pairwise_disjoint(tasks: &[PlanTask]) -> bool {
    for (index, left) in tasks.iter().enumerate() {
        for right in tasks.iter().skip(index + 1) {
            if scopes_overlap(&left.write_scope, &right.write_scope) {
                return false;
            }
        }
    }
    true
}

fn scale_caveats() -> Vec<String> {
    vec![
        "Real vp-core TeamPlan validation and SQLite task-scheduler projection are exercised, but \
         dispatch uses synthetic claim/finish transitions; no Worker agent, tool call, Git merge, \
         provider request, or token spend occurs."
            .to_string(),
        "Batch scope filtering mirrors the documented vp-core ready-batch rules because that \
         helper is crate-private; the ready set itself comes from the real TaskScheduler."
            .to_string(),
        "The 10-Worker cap is the configured batch ceiling, not a measurement of 10 concurrent \
         live agents."
            .to_string(),
        "No network I/O, provider credentials, or daemon are used.".to_string(),
    ]
}

fn fault_caveats() -> Vec<String> {
    vec![
        "Provider retry, output truncation, and cancellation run a real AgentService session with \
         an in-process fault-injecting fake provider; no network or provider key is used."
            .to_string(),
        "Duplicate tool-call replay and lost-task/batch-abort settlement are deterministic harness \
         models of private vp-core policies, not live-runtime proofs."
            .to_string(),
        "Cancellation is cooperative and the direct Leader fixture never enters the Worker commit \
         merge path; 'no merge' means the session merge queue stayed empty."
            .to_string(),
    ]
}

// ---------------------------------------------------------------------------
// Real-runtime fault rig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FaultKind {
    RetryBeforeFirstToken,
    Truncate,
    Slow,
}

struct FaultProvider {
    id: String,
    kind: FaultKind,
    calls: AtomicUsize,
    delegate: Option<Arc<FakeProvider>>,
}

impl FaultProvider {
    fn new(kind: FaultKind, delegate: Option<Arc<FakeProvider>>) -> Self {
        Self {
            id: "vp-bench-fault-fake".to_string(),
            kind,
            calls: AtomicUsize::new(0),
            delegate,
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Provider for FaultProvider {
    fn id(&self) -> &str {
        &self.id
    }

    async fn stream_chat(
        &self,
        request: ChatRequest,
    ) -> std::result::Result<EventStream, ProviderError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        match self.kind {
            FaultKind::RetryBeforeFirstToken if call == 0 => {
                let events: Vec<std::result::Result<StreamEvent, ProviderError>> = vec![Err(
                    ProviderError::Stream("transient reset before first token".to_string()),
                )];
                Ok(Box::pin(futures_util::stream::iter(events)) as EventStream)
            }
            FaultKind::RetryBeforeFirstToken => {
                let delegate = self.delegate.as_ref().ok_or_else(|| {
                    ProviderError::Stream("retry fault has no delegate provider".to_string())
                })?;
                delegate.stream_chat(request).await
            }
            FaultKind::Truncate => {
                let events: Vec<std::result::Result<StreamEvent, ProviderError>> = vec![
                    Ok(StreamEvent::TextDelta(
                        "partial output before the output limit".to_string(),
                    )),
                    Err(ProviderError::Stream(
                        "provider reported finish_reason `length` before completion".to_string(),
                    )),
                ];
                Ok(Box::pin(futures_util::stream::iter(events)) as EventStream)
            }
            FaultKind::Slow => {
                let head = futures_util::stream::once(async move {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    Ok::<StreamEvent, ProviderError>(StreamEvent::TextDelta(
                        "late output".to_string(),
                    ))
                });
                let tail = futures_util::stream::iter(vec![Ok::<StreamEvent, ProviderError>(
                    StreamEvent::Done,
                )]);
                Ok(Box::pin(head.chain(tail)) as EventStream)
            }
        }
    }
}

struct FaultRig {
    _workspace: BenchWorkspace,
    engine: Engine,
    project_id: String,
    session_id: String,
}

impl FaultRig {
    async fn create(label: &str) -> Result<Self> {
        let workspace = BenchWorkspace::create()?;
        let fixture = FixturePlan::new(FixtureKind::Parallel).file(
            "README.md",
            format!("deterministic fault fixture: {label}\n"),
        );
        materialize_repo(&fixture, &workspace)?;
        let store = Store::open(workspace.data_dir())?;
        let engine = Engine::new(store);
        let project = engine
            .add_project(
                workspace.repo().to_string_lossy().to_string(),
                Some(format!("vp-bench fault fixture {label}")),
            )
            .await?;
        let session = engine
            .create_session(
                project.id.clone(),
                format!("fault injection fixture {label}"),
                None,
            )
            .await?;
        engine
            .create_leader_agent(&session.id, "fault-fake-1")
            .await?;
        Ok(Self {
            _workspace: workspace,
            engine,
            project_id: project.id,
            session_id: session.id,
        })
    }

    fn config(&self, provider: Arc<dyn Provider>) -> AgentConfig {
        let role = RoleModelConfig {
            model: Some("fault-fake-1".to_string()),
            input_usd_per_million: Some(1.0),
            output_usd_per_million: Some(2.0),
            cache_read_usd_per_million: Some(0.1),
            cache_write_usd_per_million: Some(0.2),
            context_window: None,
            context_budget: None,
        };
        AgentConfig {
            provider,
            provider_name: "vp-bench-fault-fake".to_string(),
            model: "fault-fake-1".to_string(),
            role_models: RoleModels {
                leader: role.clone(),
                manager: role.clone(),
                worker: role.clone(),
                verifier: role.clone(),
                labour: role.clone(),
                scout: role,
            },
            base_url: None,
            max_turns: Some(8),
            max_tokens: Some(1_024),
            max_managers: 1,
            max_workers: 1,
            max_labour: 1,
            auto_expand_workers: false,
            integration_command: None,
            tools: ToolRegistry::default_tools(),
            max_active_sessions: 2,
            scope_locks: Arc::new(ScopeLocks::with_store(self.engine.store().clone())),
            capacity: Arc::new(CapacityController::new(1, 1)),
            provider_idle_timeout_secs: 30,
            question_timeout_secs: 30,
            approval_mode: ApprovalMode::Unattended,
            approval_timeout_secs: 30,
            compact_after_tokens: 0,
            compact_keep_messages: 8,
            compact_keep_tokens: 15_000,
            compact_prepare_ratio: 0.6,
            context_window: 0,
        }
    }

    async fn status(&self) -> Result<String> {
        let sessions = self
            .engine
            .list_sessions(Some(self.project_id.clone()))
            .await?;
        sessions
            .into_iter()
            .find(|session| session.id == self.session_id)
            .map(|session| session.status)
            .context("fault session disappeared")
    }

    async fn wait_terminal(&self, timeout: Duration) -> Result<String> {
        let deadline = Instant::now() + timeout;
        loop {
            let status = self.status().await?;
            if matches!(status.as_str(), "idle" | "failed" | "cancelled") {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                bail!("fault session did not reach a terminal state within {timeout:?}");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn wait_started(&self, timeout: Duration) -> Result<String> {
        let deadline = Instant::now() + timeout;
        loop {
            let status = self.status().await?;
            if !matches!(status.as_str(), "created" | "queued") {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Ok(status);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn events(&self) -> Result<Vec<vp_protocol::types::Event>> {
        self.engine
            .list_events(self.session_id.clone(), 0, 5_000)
            .await
    }

    async fn merge_count(&self) -> Result<usize> {
        Ok(self.engine.list_merge_queue(&self.session_id).await?.len())
    }
}

fn event_count(events: &[vp_protocol::types::Event], kind: &str) -> usize {
    events.iter().filter(|event| event.kind == kind).count()
}

async fn run_retry_before_output_case() -> Result<FaultCase> {
    let rig = FaultRig::create("retry").await?;
    let delegate = Arc::new(FakeProvider::sequence(
        "fault-fake-1",
        vec![vec![ScriptAction::text("retry succeeded")]],
        FakeUsageModel::benchmark_default(),
        4,
    ));
    let provider = Arc::new(FaultProvider::new(
        FaultKind::RetryBeforeFirstToken,
        Some(delegate),
    ));
    let service = AgentService::configured(rig.engine.clone(), rig.config(provider.clone()));
    service
        .start(&rig.session_id, "Reply with one short sentence.")
        .await?;
    let status = rig.wait_terminal(Duration::from_secs(15)).await?;
    let events = rig.events().await?;
    let retries = event_count(&events, "agent.stream_retry");
    let merges = rig.merge_count().await?;
    let calls = provider.calls();
    let checks = vec![
        BenchCheck::new(
            "retry_was_attempted",
            calls == 2 && retries >= 1,
            format!("{calls} provider calls, {retries} agent.stream_retry events"),
        ),
        BenchCheck::new(
            "run_recovered_to_idle",
            status == "idle",
            format!("terminal status {status}"),
        ),
        BenchCheck::new(
            "no_partial_turn_was_merged",
            merges == 0,
            format!("{merges} merge queue entries"),
        ),
        BenchCheck::new(
            "no_tool_call_executed",
            event_count(&events, "tool.started") == 0,
            "the failed attempt produced no tool call",
        ),
    ];
    Ok(FaultCase::from_checks(
        "provider-stream-failure-before-output",
        "Provider stream failure before output is retried without partial persistence",
        FaultBoundary::RealRuntimeProvider,
        "real AgentService run with an in-process fake provider",
        FaultOutcome {
            attempts: calls,
            final_state: status,
            merged: merges > 0,
        },
        checks,
    ))
}

async fn run_truncation_case() -> Result<FaultCase> {
    let rig = FaultRig::create("truncation").await?;
    let provider = Arc::new(FaultProvider::new(FaultKind::Truncate, None));
    let service = AgentService::configured(rig.engine.clone(), rig.config(provider.clone()));
    service
        .start(
            &rig.session_id,
            "Produce a response that will be truncated.",
        )
        .await?;
    let status = rig.wait_terminal(Duration::from_secs(15)).await?;
    let events = rig.events().await?;
    let retries = event_count(&events, "agent.stream_retry");
    let truncation_recoveries = event_count(&events, "agent.output_truncated");
    let merges = rig.merge_count().await?;
    let calls = provider.calls();
    let checks = vec![
        BenchCheck::new(
            "truncation_is_not_a_stream_retry",
            retries == 0,
            format!("{retries} agent.stream_retry events"),
        ),
        BenchCheck::new(
            "bounded_truncation_recovery_ran",
            (1..=2).contains(&truncation_recoveries) && calls == truncation_recoveries + 1,
            format!("{truncation_recoveries} recoveries after {calls} provider calls"),
        ),
        BenchCheck::new(
            "run_fails_closed",
            status == "failed",
            format!("terminal status {status}"),
        ),
        BenchCheck::new(
            "no_incomplete_work_was_merged",
            merges == 0,
            format!("{merges} merge queue entries"),
        ),
        BenchCheck::new(
            "no_tool_call_executed",
            event_count(&events, "tool.started") == 0,
            "the truncated attempt produced no tool call",
        ),
    ];
    Ok(FaultCase::from_checks(
        "provider-output-truncation",
        "Provider output truncation is bounded and fails closed without a merge",
        FaultBoundary::RealRuntimeProvider,
        "real AgentService run with an in-process fake provider",
        FaultOutcome {
            attempts: calls,
            final_state: status,
            merged: merges > 0,
        },
        checks,
    ))
}

async fn run_cancellation_case() -> Result<FaultCase> {
    let rig = FaultRig::create("cancellation").await?;
    let provider = Arc::new(FaultProvider::new(FaultKind::Slow, None));
    let service = AgentService::configured(rig.engine.clone(), rig.config(provider.clone()));
    service
        .start(
            &rig.session_id,
            "Wait before replying to the cancellation test.",
        )
        .await?;
    let started = rig.wait_started(Duration::from_secs(2)).await?;
    let cancellation_requested = if matches!(started.as_str(), "idle" | "failed" | "cancelled") {
        false
    } else {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut requested = false;
        while Instant::now() < deadline {
            if service.cancel(&rig.session_id) {
                requested = true;
                break;
            }
            if matches!(
                rig.status().await?.as_str(),
                "idle" | "failed" | "cancelled"
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        requested
    };
    let status = rig.wait_terminal(Duration::from_secs(5)).await?;
    let events = rig.events().await?;
    let merges = rig.merge_count().await?;
    let calls = provider.calls();
    let checks = vec![
        BenchCheck::new(
            "run_was_active_before_cancel",
            !matches!(started.as_str(), "idle" | "failed" | "cancelled"),
            format!("status before cancel: {started}"),
        ),
        BenchCheck::new(
            "cancellation_was_requested",
            cancellation_requested,
            format!("cooperative cancel accepted: {cancellation_requested}"),
        ),
        BenchCheck::new(
            "run_ended_cancelled",
            status == "cancelled",
            format!("terminal status {status}"),
        ),
        BenchCheck::new(
            "no_incomplete_work_was_merged",
            merges == 0,
            format!("{merges} merge queue entries"),
        ),
        BenchCheck::new(
            "no_tool_call_executed",
            event_count(&events, "tool.started") == 0,
            "the slow provider produced no tool call before cancellation",
        ),
    ];
    Ok(FaultCase::from_checks(
        "abort-boundary-cancellation",
        "Cooperative cancellation stops the run without merging incomplete work",
        FaultBoundary::RealRuntimeProvider,
        "real AgentService run with an in-process fake provider",
        FaultOutcome {
            attempts: calls,
            final_state: status,
            merged: merges > 0,
        },
        checks,
    ))
}

// ---------------------------------------------------------------------------
// Harness-level policies (private runtime logic)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReplayOutcome {
    replayed: bool,
    executions: usize,
}

#[derive(Debug, Default)]
struct ReplayGuard {
    executed: BTreeSet<String>,
    results: BTreeMap<String, Value>,
    executions: usize,
}

impl ReplayGuard {
    fn execute(&mut self, tool: &str, arguments: &Value) -> ReplayOutcome {
        let key = format!("{tool}:{}", canonical_json(arguments));
        if let Some(_cached) = self.results.get(&key) {
            return ReplayOutcome {
                replayed: true,
                executions: self.executions,
            };
        }
        self.executed.insert(key.clone());
        self.results.insert(key, json!({"status": "ok"}));
        self.executions += 1;
        ReplayOutcome {
            replayed: false,
            executions: self.executions,
        }
    }

    fn executions(&self) -> usize {
        self.executions
    }

    fn request_count(&self) -> usize {
        self.executed.len()
    }

    fn cached_value(&self, tool: &str, arguments: &Value) -> Option<&Value> {
        self.results
            .get(&format!("{tool}:{}", canonical_json(arguments)))
    }
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut entries = map.iter().collect::<Vec<_>>();
            entries.sort_by_key(|(left, _)| *left);
            let fields = entries
                .into_iter()
                .map(|(key, value)| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap_or_default(),
                        canonical_json(value)
                    )
                })
                .collect::<Vec<_>>();
            format!("{{{}}}", fields.join(","))
        }
        Value::Array(items) => {
            let items = items.iter().map(canonical_json).collect::<Vec<_>>();
            format!("[{}]", items.join(","))
        }
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn duplicate_tool_call_case() -> FaultCase {
    let arguments = json!({"path": "src/scale.txt", "content": "same bytes"});
    let mut guard = ReplayGuard::default();
    let first = guard.execute("write_file", &arguments);
    let second = guard.execute("write_file", &arguments);
    let different = guard.execute(
        "write_file",
        &json!({"path": "src/scale.txt", "content": "different bytes"}),
    );
    let third = guard.execute("write_file", &arguments);
    let cached = guard.cached_value("write_file", &arguments).cloned();
    let checks = vec![
        BenchCheck::new(
            "first_call_executes",
            !first.replayed && first.executions == 1,
            format!("first outcome {first:?}"),
        ),
        BenchCheck::new(
            "identical_call_replays",
            second.replayed && second.executions == 1,
            format!("second outcome {second:?}"),
        ),
        BenchCheck::new(
            "different_input_executes_again",
            !different.replayed && different.executions == 2,
            format!("different outcome {different:?}"),
        ),
        BenchCheck::new(
            "replay_still_returns_the_recorded_result",
            third.replayed && third.executions == 2 && cached.is_some(),
            "the third identical call replayed the first result",
        ),
        BenchCheck::new(
            "no_duplicate_side_effect",
            guard.executions() == 2 && guard.request_count() == 2,
            format!(
                "{} executions for {} distinct requests",
                guard.executions(),
                guard.request_count()
            ),
        ),
    ];
    FaultCase::from_checks(
        "duplicate-tool-call-replay-guard",
        "Duplicate tool calls replay the recorded result instead of executing twice",
        FaultBoundary::HarnessModel,
        "deterministic harness model of the private vp-core run-journal duplicate-write gate",
        FaultOutcome {
            attempts: 3,
            final_state: "replayed".to_string(),
            merged: false,
        },
        checks,
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BatchAbortOutcome {
    statuses: BTreeMap<String, String>,
    merged: BTreeSet<String>,
    reason: String,
    lost: BTreeSet<String>,
}

fn run_batch_abort_model(omit_task: Option<&str>) -> BatchAbortOutcome {
    let expected = ["T1", "T2", "T3", "T4", "T5"];
    let mut statuses = BTreeMap::from([
        ("T1".to_string(), "completed".to_string()),
        ("T2".to_string(), "failed".to_string()),
        ("T3".to_string(), "running".to_string()),
        ("T4".to_string(), "pending".to_string()),
        ("T5".to_string(), "pending".to_string()),
    ]);
    let mut merged = BTreeSet::from(["T1".to_string()]);
    // T2 failed inside the batch: settle its non-terminal sibling T3, block a
    // dependent T4, and leave the independent pending T5 alone.
    statuses.insert("T3".to_string(), "cancelled".to_string());
    statuses.insert("T4".to_string(), "blocked".to_string());
    if let Some(task) = omit_task {
        statuses.remove(task);
    }
    let lost = expected
        .iter()
        .filter(|id| !statuses.contains_key(**id))
        .map(|id| (*id).to_string())
        .collect::<BTreeSet<_>>();
    merged.remove("T2");
    merged.remove("T3");
    BatchAbortOutcome {
        statuses,
        merged,
        reason: "batch aborted: task T2 failed".to_string(),
        lost,
    }
}

fn lost_task_batch_abort_case() -> FaultCase {
    let first = run_batch_abort_model(None);
    let second = run_batch_abort_model(None);
    let lost_fixture = run_batch_abort_model(Some("T4"));
    let incomplete_merge = first
        .statuses
        .iter()
        .any(|(id, status)| status != "completed" && first.merged.contains(id));
    let checks = vec![
        BenchCheck::new(
            "failed_task_is_not_merged",
            first.statuses.get("T2").map(String::as_str) == Some("failed")
                && !first.merged.contains("T2"),
            "T2 failed and stayed out of the merge set",
        ),
        BenchCheck::new(
            "running_sibling_is_cancelled",
            first.statuses.get("T3").map(String::as_str) == Some("cancelled")
                && !first.merged.contains("T3"),
            "T3 settled cancelled instead of being silently dropped",
        ),
        BenchCheck::new(
            "dependent_task_is_blocked",
            first.statuses.get("T4").map(String::as_str) == Some("blocked"),
            "T4 was blocked because its dependency failed",
        ),
        BenchCheck::new(
            "independent_task_is_untouched",
            first.statuses.get("T5").map(String::as_str) == Some("pending"),
            "T5 stayed pending for the next batch",
        ),
        BenchCheck::new(
            "completed_sibling_is_preserved",
            first.statuses.get("T1").map(String::as_str) == Some("completed")
                && first.merged.contains("T1"),
            "T1 remained completed and merged",
        ),
        BenchCheck::new(
            "batch_abort_is_deterministic",
            first == second && first.reason == second.reason,
            format!("reason: {}", first.reason),
        ),
        BenchCheck::new(
            "lost_task_is_detected_not_ignored",
            lost_fixture.lost == BTreeSet::from(["T4".to_string()]),
            format!("lost set: {:?}", lost_fixture.lost),
        ),
        BenchCheck::new(
            "no_incomplete_task_is_marked_merged",
            !incomplete_merge,
            "only completed tasks may be in the merge set",
        ),
    ];
    FaultCase::from_checks(
        "lost-task-and-batch-abort",
        "A lost task or failed batch settles siblings deterministically without merging incomplete work",
        FaultBoundary::HarnessModel,
        "deterministic harness model of the documented plan-batch settlement policy",
        FaultOutcome {
            attempts: 0,
            final_state: "settled".to_string(),
            merged: false,
        },
        checks,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn scale_plan_has_expected_shape_and_no_overlap_inside_layers() {
        let plan = scale_plan();
        assert_eq!(plan.tasks.len(), SCALE_TASK_COUNT);
        assert_eq!(plan.departments.len(), SCALE_DEPARTMENTS);
        let ids = plan
            .tasks
            .iter()
            .map(|task| task.id.clone())
            .collect::<HashSet<_>>();
        assert_eq!(ids.len(), SCALE_TASK_COUNT);
        assert!(acyclic(&plan));
        for layer in 0..SCALE_LAYERS {
            let tasks = (0..SCALE_TASKS_PER_LAYER)
                .map(|index| {
                    plan.tasks
                        .iter()
                        .find(|task| task.id == scale_task_id(layer, index))
                        .unwrap()
                        .clone()
                })
                .collect::<Vec<_>>();
            assert!(pairwise_disjoint(&tasks));
        }
    }

    #[test]
    fn scope_overlap_matches_runtime_style_prefix_rules() {
        assert!(scopes_overlap(
            &["src".to_string()],
            &["src/a.txt".to_string()]
        ));
        assert!(scopes_overlap(
            &["src/a.txt".to_string()],
            &["src/a.txt".to_string()]
        ));
        assert!(!scopes_overlap(
            &["src/a.txt".to_string()],
            &["src/b.txt".to_string()]
        ));
        assert!(scopes_overlap(
            &["**".to_string()],
            &["src/a.txt".to_string()]
        ));
        assert!(scopes_overlap(&[], &["src/a.txt".to_string()]));
    }

    #[test]
    fn canonical_json_ignores_object_key_order() {
        assert_eq!(
            canonical_json(&json!({"b": 2, "a": 1})),
            canonical_json(&json!({"a": 1, "b": 2}))
        );
    }
}

//! Deterministic critical-path math for the budget governor.
//!
//! The path is the heaviest remaining chain in the stored TeamPlan DAG. Task
//! weight is the declared token budget when present, otherwise a bounded
//! priority weight. Completed and skipped tasks are removed from the remaining
//! graph; their dependents treat them as satisfied. Cancelled tasks are also
//! removed because an operator explicitly abandoned that branch.
//!
//! Unknown dependencies, duplicate task ids, cycles, and oversized plans fail
//! closed. The result includes a content hash so a governor decision can be
//! replayed and compared without trusting wall-clock time.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use vp_protocol::types::TeamPlan;

/// Maximum plan nodes considered by the path computation. `TeamPlan` validation
/// already caps plans at 100 tasks; this is a defense-in-depth bound.
pub const MAX_PATH_TASKS: usize = 100;
/// Maximum dependency edges considered by the path computation.
pub const MAX_PATH_EDGES: usize = 1_000;
/// Weight used for a task with no declared token budget and a priority <= 0.
pub const PRIORITY_WEIGHT_UNIT_TOKENS: u64 = 1_000;
/// Declared task budgets are capped so arithmetic stays in bounded ranges.
pub const MAX_TASK_WEIGHT_TOKENS: u64 = 100_000_000;

/// Inputs to one pure critical-path computation.
pub struct CriticalPathInput<'a> {
    pub session_id: &'a str,
    pub project_id: &'a str,
    pub plan_revision: u32,
    pub plan: &'a TeamPlan,
    pub statuses: &'a HashMap<String, String>,
    /// Remaining session budget at decision time. It is carried into the
    /// result for explainability and hashed into the input identity.
    pub remaining_budget_tokens: Option<u64>,
}

/// Stable result of one computation. Every field is derived from the inputs;
/// no clock, random id, or environment value is included.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CriticalPath {
    pub session_id: String,
    pub project_id: String,
    pub plan_revision: u32,
    pub task_ids: Vec<String>,
    pub weight_tokens: u64,
    pub remaining_budget_tokens: Option<u64>,
    pub complete: bool,
    pub task_count: usize,
    pub edge_count: usize,
    pub inputs_hash: String,
}

/// Typed failure with a stable Q3 error-contract code. The governor maps this
/// into its own bounded failure shape; the code never changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CriticalPathError {
    pub code: &'static str,
    pub message: String,
    pub hint: &'static str,
}

impl std::fmt::Display for CriticalPathError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for CriticalPathError {}

impl CriticalPathError {
    pub const CYCLE: &'static str = "BUDGET_PATH_CYCLE";
    pub const UNKNOWN_DEPENDENCY: &'static str = "BUDGET_PATH_UNKNOWN_TASK";
    pub const DUPLICATE_TASK: &'static str = "BUDGET_PATH_DUPLICATE_TASK";
    pub const TOO_LARGE: &'static str = "BUDGET_PATH_TOO_LARGE";

    fn cycle(path: &[String], repeated: &str) -> Self {
        Self {
            code: Self::CYCLE,
            message: format!(
                "plan dependency cycle through {}: {} -> {}",
                repeated,
                path.join(" -> "),
                repeated
            ),
            hint: "Remove the dependency cycle from the plan before enabling the budget governor.",
        }
    }

    fn unknown(task_id: &str, dependency: &str, status: Option<&str>) -> Self {
        Self {
            code: Self::UNKNOWN_DEPENDENCY,
            message: format!(
                "task {task_id} depends on unknown task {dependency}{}",
                status
                    .map(|status| format!(" (status {status})"))
                    .unwrap_or_default()
            ),
            hint: "Revise the plan so every dependency names a task in the same revision, or mark it completed/skipped.",
        }
    }

    fn duplicate(task_id: &str) -> Self {
        Self {
            code: Self::DUPLICATE_TASK,
            message: format!("plan task id {task_id} is declared more than once"),
            hint: "Give every task a unique id before relying on critical-path admission.",
        }
    }

    fn too_large(kind: &str, actual: usize, limit: usize) -> Self {
        Self {
            code: Self::TOO_LARGE,
            message: format!("plan {kind} count {actual} exceeds the governor limit {limit}"),
            hint: "Split the plan or reduce dependency edges; the governor fails closed on oversized DAGs.",
        }
    }

    fn self_dependency(task_id: &str) -> Self {
        Self {
            code: Self::CYCLE,
            message: format!("task {task_id} depends on itself"),
            hint: "Remove the self dependency; it is a one-node cycle.",
        }
    }
}

/// Completed and skipped tasks no longer consume remaining path weight.
pub fn is_terminal_status(status: &str) -> bool {
    matches!(status, "completed" | "skipped")
}

/// Tasks that contribute remaining path weight. Cancelled work is excluded
/// because an operator explicitly abandoned it; failed/blocked/interrupted work
/// remains because it is retryable and may still block the plan.
pub fn is_remaining_status(status: &str) -> bool {
    !is_terminal_status(status) && status != "cancelled"
}

/// Weight of one task. Declared token budgets win. Otherwise priority is
/// multiplied by a fixed unit, with zero/negative priority clamped to one unit
/// so every remaining node has positive weight.
pub fn task_weight_tokens(task: &vp_protocol::types::PlanTask) -> u64 {
    if let Some(budget) = task.token_budget.filter(|budget| *budget > 0) {
        return budget.min(MAX_TASK_WEIGHT_TOKENS);
    }
    let priority = task.priority.clamp(-100, 100);
    let units = if priority <= 0 { 1 } else { priority as u64 };
    units
        .saturating_mul(PRIORITY_WEIGHT_UNIT_TOKENS)
        .min(MAX_TASK_WEIGHT_TOKENS)
}

struct Node<'a> {
    task_id: &'a str,
    weight: u64,
    dependents: Vec<usize>,
}

/// Compute the heaviest remaining path to any sink.
///
/// Tie-breaking is deterministic: a heavier path wins; equal path weights are
/// compared by the lexicographic task-id sequence, so the smaller id sequence
/// wins. Dependency edges point from a dependency to its dependent, and a path
/// is a dependency-respecting chain.
pub fn compute_critical_path(
    input: CriticalPathInput<'_>,
) -> Result<CriticalPath, CriticalPathError> {
    if input.plan.tasks.len() > MAX_PATH_TASKS {
        return Err(CriticalPathError::too_large(
            "task",
            input.plan.tasks.len(),
            MAX_PATH_TASKS,
        ));
    }
    let mut unique = HashSet::new();
    for task in &input.plan.tasks {
        if !unique.insert(task.id.as_str()) {
            return Err(CriticalPathError::duplicate(&task.id));
        }
    }

    let index_by_id = input
        .plan
        .tasks
        .iter()
        .enumerate()
        .map(|(index, task)| (task.id.as_str(), index))
        .collect::<HashMap<_, _>>();

    let remaining = input
        .plan
        .tasks
        .iter()
        .enumerate()
        .filter(|(_, task)| {
            input
                .statuses
                .get(&task.id)
                .map(|status| is_remaining_status(status))
                .unwrap_or(true)
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let remaining_set = remaining.iter().copied().collect::<HashSet<_>>();

    let mut nodes = input
        .plan
        .tasks
        .iter()
        .map(|task| Node {
            task_id: task.id.as_str(),
            weight: task_weight_tokens(task),
            dependents: Vec::new(),
        })
        .collect::<Vec<_>>();

    let mut edge_count = 0usize;
    for (index, task) in input.plan.tasks.iter().enumerate() {
        if !remaining_set.contains(&index) {
            continue;
        }
        for dependency in &task.deps {
            edge_count = edge_count.saturating_add(1);
            if edge_count > MAX_PATH_EDGES {
                return Err(CriticalPathError::too_large(
                    "dependency edge",
                    edge_count,
                    MAX_PATH_EDGES,
                ));
            }
            if dependency == &task.id {
                return Err(CriticalPathError::self_dependency(&task.id));
            }
            let Some(dep_index) = index_by_id.get(dependency.as_str()).copied() else {
                let status = input.statuses.get(dependency).map(String::as_str);
                if status.is_some_and(is_terminal_status) {
                    // A removed or historical dependency that is already
                    // satisfied does not block the remaining graph.
                    continue;
                }
                return Err(CriticalPathError::unknown(&task.id, dependency, status));
            };
            if remaining_set.contains(&dep_index) {
                nodes[dep_index].dependents.push(index);
            }
        }
    }

    // Kahn's algorithm both proves acyclicity and gives a deterministic
    // evaluation order for the memoized path DP.
    let mut indegree = vec![0usize; nodes.len()];
    for node in &nodes {
        for dependent in &node.dependents {
            indegree[*dependent] = indegree[*dependent].saturating_add(1);
        }
    }
    let mut ready = remaining
        .iter()
        .copied()
        .filter(|index| indegree[*index] == 0)
        .collect::<Vec<_>>();
    ready.sort_by(|left, right| nodes[*left].task_id.cmp(nodes[*right].task_id));
    let mut topo = Vec::with_capacity(remaining.len());
    let mut cursor = 0usize;
    while cursor < ready.len() {
        let index = ready[cursor];
        cursor += 1;
        topo.push(index);
        let mut dependents = nodes[index].dependents.clone();
        dependents.sort_by(|left, right| nodes[*left].task_id.cmp(nodes[*right].task_id));
        for dependent in dependents {
            indegree[dependent] = indegree[dependent].saturating_sub(1);
            if indegree[dependent] == 0 {
                ready.push(dependent);
            }
        }
    }
    if topo.len() != remaining.len() {
        let mut cyclic = remaining
            .iter()
            .copied()
            .filter(|index| indegree[*index] > 0)
            .map(|index| nodes[index].task_id.to_string())
            .collect::<Vec<_>>();
        cyclic.sort();
        let repeated = cyclic.first().cloned().unwrap_or_else(|| "?".to_string());
        return Err(CriticalPathError::cycle(&cyclic, &repeated));
    }

    // best[index] = (weight, path starting at index). Processing in reverse
    // topological order guarantees every dependent is already resolved.
    let mut best: Vec<Option<(u64, Vec<String>)>> = vec![None; nodes.len()];
    for index in topo.iter().rev() {
        if !remaining_set.contains(index) {
            continue;
        }
        let mut winner: Option<(u64, Vec<String>)> = None;
        for dependent in &nodes[*index].dependents {
            let Some((weight, path)) = best[*dependent].clone() else {
                continue;
            };
            let candidate_weight = nodes[*index].weight.saturating_add(weight);
            let mut candidate_path = Vec::with_capacity(path.len().saturating_add(1));
            candidate_path.push(nodes[*index].task_id.to_string());
            candidate_path.extend(path);
            winner = choose_path(winner, candidate_weight, candidate_path);
        }
        if winner.is_none() {
            winner = Some((
                nodes[*index].weight,
                vec![nodes[*index].task_id.to_string()],
            ));
        }
        best[*index] = winner;
    }

    let mut global: Option<(u64, Vec<String>)> = None;
    for index in &remaining {
        let Some((weight, path)) = best[*index].clone() else {
            continue;
        };
        global = choose_path(global, weight, path);
    }

    let (weight_tokens, task_ids) = global.unwrap_or((0, Vec::new()));
    let complete = remaining.is_empty();
    let inputs_hash = hash_inputs(&input, &task_ids, weight_tokens, edge_count);
    Ok(CriticalPath {
        session_id: input.session_id.to_string(),
        project_id: input.project_id.to_string(),
        plan_revision: input.plan_revision,
        task_ids,
        weight_tokens,
        remaining_budget_tokens: input.remaining_budget_tokens,
        complete,
        task_count: input.plan.tasks.len(),
        edge_count,
        inputs_hash,
    })
}

fn choose_path(
    current: Option<(u64, Vec<String>)>,
    weight: u64,
    path: Vec<String>,
) -> Option<(u64, Vec<String>)> {
    match current {
        None => Some((weight, path)),
        Some((current_weight, current_path)) => {
            if weight > current_weight || (weight == current_weight && path < current_path) {
                Some((weight, path))
            } else {
                Some((current_weight, current_path))
            }
        }
    }
}

fn hash_inputs(
    input: &CriticalPathInput<'_>,
    task_ids: &[String],
    weight_tokens: u64,
    edge_count: usize,
) -> String {
    let mut material = String::new();
    material.push_str("critical_path.v1\n");
    material.push_str(input.session_id);
    material.push('\n');
    material.push_str(input.project_id);
    material.push('\n');
    material.push_str(&input.plan_revision.to_string());
    material.push('\n');
    material.push_str(
        &input
            .remaining_budget_tokens
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unbounded".to_string()),
    );
    material.push('\n');
    material.push_str(&edge_count.to_string());
    material.push('\n');
    material.push_str(&weight_tokens.to_string());
    material.push('\n');
    material.push_str(&task_ids.join(","));
    material.push('\n');
    let mut tasks = input.plan.tasks.iter().collect::<Vec<_>>();
    tasks.sort_by(|left, right| left.id.cmp(&right.id));
    for task in tasks {
        material.push_str(&task.id);
        material.push('|');
        material.push_str(&task.department);
        material.push('|');
        material.push_str(&task.priority.to_string());
        material.push('|');
        material.push_str(
            &task
                .token_budget
                .map(|budget| budget.to_string())
                .unwrap_or_else(|| "-".to_string()),
        );
        material.push('|');
        material.push_str(
            input
                .statuses
                .get(&task.id)
                .map(String::as_str)
                .unwrap_or("pending"),
        );
        material.push('|');
        let mut deps = task.deps.clone();
        deps.sort();
        material.push_str(&deps.join(","));
        material.push('\n');
    }
    crate::sha256_hex(material.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vp_protocol::types::{PlanDepartment, PlanTask};

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
            parent_task_id: None,
            fanout_key: None,
            fanout_join_for: None,
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

    fn path(
        plan: &TeamPlan,
        statuses: &HashMap<String, String>,
        revision: u32,
        remaining: Option<u64>,
    ) -> Result<CriticalPath, CriticalPathError> {
        compute_critical_path(CriticalPathInput {
            session_id: "session-1",
            project_id: "project-1",
            plan_revision: revision,
            plan,
            statuses,
            remaining_budget_tokens: remaining,
        })
    }

    #[test]
    fn linear_chain_returns_the_whole_path() {
        let plan = plan(vec![
            task("a", 1, &[], None),
            task("b", 1, &["a"], None),
            task("c", 1, &["b"], None),
        ]);
        let path = path(&plan, &HashMap::new(), 1, Some(10_000)).unwrap();
        assert_eq!(path.task_ids, vec!["a", "b", "c"]);
        assert_eq!(path.weight_tokens, 3_000);
        assert!(!path.complete);
        assert_eq!(path.task_count, 3);
        assert_eq!(path.edge_count, 2);
        assert_eq!(path.remaining_budget_tokens, Some(10_000));
        assert_eq!(path.plan_revision, 1);
    }

    #[test]
    fn branching_plan_picks_the_heaviest_branch() {
        let plan = plan(vec![
            task("root", 1, &[], Some(1_000)),
            task("light", 1, &["root"], Some(2_000)),
            task("heavy", 1, &["root"], Some(50_000)),
            task("heavy-child", 1, &["heavy"], Some(20_000)),
        ]);
        let path = path(&plan, &HashMap::new(), 1, None).unwrap();
        assert_eq!(path.task_ids, vec!["root", "heavy", "heavy-child"]);
        assert_eq!(path.weight_tokens, 71_000);
    }

    #[test]
    fn equal_weights_tie_break_by_lexicographic_task_sequence() {
        let plan = plan(vec![
            task("b", 1, &[], Some(10_000)),
            task("a", 1, &[], Some(10_000)),
        ]);
        let path = path(&plan, &HashMap::new(), 1, None).unwrap();
        assert_eq!(path.task_ids, vec!["a"]);
    }

    #[test]
    fn equal_chain_weights_compare_full_sequences() {
        let plan = plan(vec![
            task("a", 1, &[], Some(5_000)),
            task("a-child", 1, &["a"], Some(5_000)),
            task("b", 1, &[], Some(10_000)),
        ]);
        let path = path(&plan, &HashMap::new(), 1, None).unwrap();
        assert_eq!(path.task_ids, vec!["a", "a-child"]);
    }

    #[test]
    fn declared_token_budget_beats_priority_weight() {
        let plan = plan(vec![task("a", 100, &[], Some(7_000))]);
        let path = path(&plan, &HashMap::new(), 1, None).unwrap();
        assert_eq!(path.weight_tokens, 7_000);
    }

    #[test]
    fn zero_priority_uses_one_fixed_unit() {
        let plan = plan(vec![
            task("zero", 0, &[], None),
            task("negative", -50, &[], None),
            task("high", 50, &[], None),
        ]);
        let weighted = plan
            .tasks
            .iter()
            .map(task_weight_tokens)
            .collect::<Vec<_>>();
        assert_eq!(weighted, vec![1_000, 1_000, 50_000]);
    }

    #[test]
    fn completed_and_skipped_tasks_are_removed_and_satisfy_dependents() {
        let plan = plan(vec![
            task("done", 1, &[], Some(90_000)),
            task("skipped", 1, &["done"], Some(90_000)),
            task("open", 1, &["skipped"], Some(5_000)),
        ]);
        let status = statuses(&[("done", "completed"), ("skipped", "skipped")]);
        let path = path(&plan, &status, 1, None).unwrap();
        assert_eq!(path.task_ids, vec!["open"]);
        assert_eq!(path.weight_tokens, 5_000);
    }

    #[test]
    fn cancelled_tasks_are_excluded_but_failed_tasks_remain() {
        let plan = plan(vec![
            task("cancelled", 1, &[], Some(90_000)),
            task("failed", 1, &[], Some(7_000)),
            task("pending", 1, &[], Some(5_000)),
        ]);
        let status = statuses(&[("cancelled", "cancelled"), ("failed", "failed")]);
        let path = path(&plan, &status, 1, None).unwrap();
        assert_eq!(path.task_ids, vec!["failed"]);
    }

    #[test]
    fn all_terminal_plan_is_complete_with_zero_weight() {
        let plan = plan(vec![task("a", 1, &[], None)]);
        let status = statuses(&[("a", "completed")]);
        let path = path(&plan, &status, 4, Some(10)).unwrap();
        assert!(path.complete);
        assert!(path.task_ids.is_empty());
        assert_eq!(path.weight_tokens, 0);
    }

    #[test]
    fn empty_plan_is_complete() {
        let path = path(&plan(Vec::new()), &HashMap::new(), 1, None).unwrap();
        assert!(path.complete);
        assert!(path.task_ids.is_empty());
    }

    #[test]
    fn cycle_is_rejected_with_a_stable_code() {
        let plan = plan(vec![
            task("a", 1, &["c"], None),
            task("b", 1, &["a"], None),
            task("c", 1, &["b"], None),
        ]);
        let error = path(&plan, &HashMap::new(), 1, None).unwrap_err();
        assert_eq!(error.code, CriticalPathError::CYCLE);
        assert!(error.message.contains("cycle"));
    }

    #[test]
    fn self_dependency_is_a_cycle() {
        let plan = plan(vec![task("a", 1, &["a"], None)]);
        let error = path(&plan, &HashMap::new(), 1, None).unwrap_err();
        assert_eq!(error.code, CriticalPathError::CYCLE);
    }

    #[test]
    fn unknown_dependency_fails_closed_unless_satisfied() {
        let plan = plan(vec![task("a", 1, &["missing"], None)]);
        let error = path(&plan, &HashMap::new(), 1, None).unwrap_err();
        assert_eq!(error.code, CriticalPathError::UNKNOWN_DEPENDENCY);
        let satisfied = statuses(&[("missing", "completed")]);
        let path = path(&plan, &satisfied, 1, None).unwrap();
        assert_eq!(path.task_ids, vec!["a"]);
    }

    #[test]
    fn duplicate_task_ids_fail_closed() {
        let plan = plan(vec![task("a", 1, &[], None), task("a", 2, &[], None)]);
        let error = path(&plan, &HashMap::new(), 1, None).unwrap_err();
        assert_eq!(error.code, CriticalPathError::DUPLICATE_TASK);
    }

    #[test]
    fn oversized_plans_fail_closed() {
        let mut tasks = Vec::new();
        for index in 0..=MAX_PATH_TASKS {
            tasks.push(task(&format!("t{index}"), 1, &[], None));
        }
        let error = path(&plan(tasks), &HashMap::new(), 1, None).unwrap_err();
        assert_eq!(error.code, CriticalPathError::TOO_LARGE);
    }

    #[test]
    fn too_many_edges_fail_closed() {
        // 100 tasks where each task depends on every earlier task: about 4,950
        // edges, well above the 1,000-edge bound and still under the node cap.
        let mut tasks = vec![task("t0", 1, &[], None)];
        for index in 1..MAX_PATH_TASKS {
            let deps = (0..index).map(|dep| format!("t{dep}")).collect::<Vec<_>>();
            let mut entry = task(&format!("t{index}"), 1, &[], None);
            entry.deps = deps;
            tasks.push(entry);
        }
        let error = path(&plan(tasks), &HashMap::new(), 1, None).unwrap_err();
        assert_eq!(error.code, CriticalPathError::TOO_LARGE);
    }

    #[test]
    fn revision_and_status_changes_move_the_input_hash() {
        let plan = plan(vec![task("a", 1, &[], None), task("b", 1, &[], None)]);
        let first = path(&plan, &HashMap::new(), 1, Some(100)).unwrap();
        let second = path(&plan, &HashMap::new(), 2, Some(100)).unwrap();
        assert_ne!(first.inputs_hash, second.inputs_hash);
        let status = statuses(&[("a", "completed")]);
        let third = path(&plan, &status, 1, Some(100)).unwrap();
        assert_ne!(first.inputs_hash, third.inputs_hash);
        let fourth = path(&plan, &HashMap::new(), 1, Some(200)).unwrap();
        assert_ne!(first.inputs_hash, fourth.inputs_hash);
    }

    #[test]
    fn recomputation_is_deterministic() {
        let plan = plan(vec![
            task("a", 1, &[], Some(10_000)),
            task("b", 1, &["a"], Some(10_000)),
            task("c", 50, &[], Some(10_000)),
        ]);
        let first = path(&plan, &HashMap::new(), 3, Some(1_000)).unwrap();
        let second = path(&plan, &HashMap::new(), 3, Some(1_000)).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.inputs_hash.len(), 64);
    }

    #[test]
    fn weights_are_capped_for_declared_budgets() {
        let plan = plan(vec![task("a", 1, &[], Some(u64::MAX))]);
        let path = path(&plan, &HashMap::new(), 1, None).unwrap();
        assert_eq!(path.weight_tokens, MAX_TASK_WEIGHT_TOKENS);
    }

    #[test]
    fn wide_dag_selects_the_heaviest_root_and_is_stable() {
        let mut tasks = Vec::new();
        for index in 0..MAX_PATH_TASKS {
            let budget = if index == 37 {
                90_000
            } else {
                1_000 + (index as u64 * 10)
            };
            tasks.push(task(&format!("t{index:02}"), 1, &[], Some(budget)));
        }
        let plan = plan(tasks);
        let first = path(&plan, &HashMap::new(), 7, Some(1_000_000)).unwrap();
        let second = path(&plan, &HashMap::new(), 7, Some(1_000_000)).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.task_ids, vec!["t37"]);
        assert_eq!(first.weight_tokens, 90_000);
        assert_eq!(first.task_count, MAX_PATH_TASKS);
    }

    #[test]
    fn diamond_picks_the_heavier_intermediate_branch() {
        let plan = plan(vec![
            task("start", 1, &[], Some(1_000)),
            task("cheap", 1, &["start"], Some(2_000)),
            task("costly", 1, &["start"], Some(20_000)),
            task("end", 1, &["cheap", "costly"], Some(1_000)),
        ]);
        let path = path(&plan, &HashMap::new(), 1, None).unwrap();
        assert_eq!(path.task_ids, vec!["start", "costly", "end"]);
        assert_eq!(path.weight_tokens, 22_000);
    }

    #[test]
    fn zero_budget_falls_back_to_priority_weight() {
        let plan = plan(vec![task("a", 0, &[], Some(0))]);
        let path = path(&plan, &HashMap::new(), 1, None).unwrap();
        assert_eq!(path.weight_tokens, PRIORITY_WEIGHT_UNIT_TOKENS);
    }

    #[test]
    fn unknown_dependency_with_non_terminal_status_reports_the_status() {
        let plan = plan(vec![task("a", 1, &["ghost"], None)]);
        let status = statuses(&[("ghost", "running")]);
        let error = path(&plan, &status, 1, None).unwrap_err();
        assert_eq!(error.code, CriticalPathError::UNKNOWN_DEPENDENCY);
        assert!(error.message.contains("running"));
    }

    #[test]
    fn input_hash_is_independent_of_task_declaration_order() {
        let first = plan(vec![
            task("a", 1, &[], Some(1_000)),
            task("b", 1, &["a"], Some(2_000)),
        ]);
        let second = plan(vec![
            task("b", 1, &["a"], Some(2_000)),
            task("a", 1, &[], Some(1_000)),
        ]);
        let left = path(&first, &HashMap::new(), 1, Some(10)).unwrap();
        let right = path(&second, &HashMap::new(), 1, Some(10)).unwrap();
        assert_eq!(left.inputs_hash, right.inputs_hash);
        assert_eq!(left.task_ids, right.task_ids);
    }

    #[test]
    fn maximum_depth_chain_returns_every_node() {
        let mut tasks = Vec::new();
        for index in 0..MAX_PATH_TASKS {
            let deps = if index == 0 {
                Vec::new()
            } else {
                vec![format!("t{:02}", index - 1)]
            };
            let mut entry = task(&format!("t{index:02}"), 1, &[], Some(1_000));
            entry.deps = deps;
            tasks.push(entry);
        }
        let path = path(&plan(tasks), &HashMap::new(), 1, Some(500_000)).unwrap();
        assert_eq!(path.task_ids.len(), MAX_PATH_TASKS);
        assert_eq!(path.weight_tokens, (MAX_PATH_TASKS as u64) * 1_000);
        assert_eq!(path.edge_count, MAX_PATH_TASKS - 1);
    }

    #[test]
    fn a_deep_chain_beats_a_single_heavy_task() {
        let mut tasks = vec![task("heavy", 1, &[], Some(40_000))];
        let mut previous = String::new();
        for index in 0..5 {
            let id = format!("chain-{index}");
            let mut entry = task(&id, 1, &[], Some(10_000));
            if !previous.is_empty() {
                entry.deps = vec![previous.clone()];
            }
            previous = id;
            tasks.push(entry);
        }
        let path = path(&plan(tasks), &HashMap::new(), 1, None).unwrap();
        assert_eq!(path.task_ids.len(), 5);
        assert_eq!(path.weight_tokens, 50_000);
        assert_eq!(path.task_ids.first().map(String::as_str), Some("chain-0"));
    }

    #[test]
    fn unknown_status_is_treated_as_remaining() {
        let plan = plan(vec![task("a", 1, &[], None)]);
        let status = statuses(&[("a", "queued")]);
        let path = path(&plan, &status, 1, None).unwrap();
        assert_eq!(path.task_ids, vec!["a"]);
    }
}

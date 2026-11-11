//! Deterministic multi-path budget allocation for the critical-path governor.
//!
//! F5 protects one heaviest remaining chain. That is safe but unfair: a plan
//! with several independent blocking chains can spend the reserve on the
//! heaviest chain while a smaller department starves. This module adds an
//! additive, pure allocator on top of the same plan/status/budget inputs:
//!
//! * enumerate bounded root-to-leaf paths in the remaining TeamPlan DAG;
//! * weight each path by declared task budgets, optionally blended with
//!   observed task spend from the usage ledger (clamped);
//! * apply deterministic per-department floors, then water-fill the remaining
//!   reserve across the selected paths by weight;
//! * report each path's weight, floor, allocation, and unfunded deficit.
//!
//! The allocator never writes state, never changes a hard budget, and never
//! chooses a provider/model. `single` mode does not call it at all, so the
//! original F5 admission path remains byte-for-byte compatible.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use vp_protocol::types::{CriticalPathPolicy, GovernorPathInfo};

use crate::critical_path::{
    CriticalPathError, CriticalPathInput, MAX_PATH_TASKS, compute_critical_path,
    is_remaining_status, task_weight_tokens,
};

/// Hard cap on enumerated maximal paths. Plan validation already bounds the
/// DAG at 100 nodes and 1,000 edges; path count can still be exponential, so
/// enumeration is deterministic and truncated at this bound. Truncation is
/// reported instead of silently pretending the table is complete.
pub const MAX_ENUMERATED_PATHS: usize = 4_096;
/// Maximum selected paths, matching `CriticalPathPolicy::MAX_MAX_PATHS`.
pub const MAX_SELECTED_PATHS: usize = 16;
/// Minimum selected paths. Two paths are required for a meaningful table.
pub const MIN_SELECTED_PATHS: usize = 2;
/// Maximum department rows surfaced by preview/status/events. A plan can name
/// more departments than this; rows are sorted by weight then name so the
/// bound is deterministic and the heaviest departments stay visible.
pub const MAX_DEPARTMENT_ROWS: usize = 32;
/// Per-path weight bound, matching the single-path governor bound.
pub const MAX_PATH_WEIGHT_TOKENS: u64 = 100_000_000;
/// Priority credit granted per full starvation interval.
pub const STARVATION_PRIORITY_PER_INTERVAL: i64 = 5;
/// Maximum priority credit from starvation aging.
pub const MAX_STARVATION_CREDIT: i64 = 100;

/// One enumerated root-to-leaf path with all derived weights.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathAllocation {
    /// Deterministic rank in the table, 0 = heaviest.
    pub path_id: u32,
    pub task_ids: Vec<String>,
    /// Sorted unique departments touched by the path.
    pub departments: Vec<String>,
    /// Blended weight used for selection and water-filling.
    pub weight_tokens: u64,
    /// Declared task-budget weight before observed-spend blending.
    pub declared_weight_tokens: u64,
    /// Clamped observed task spend along the path.
    pub observed_tokens: u64,
    /// Department-floor share granted before weighted water-filling.
    pub floor_tokens: u64,
    /// Reserve actually protected for this path.
    pub allocation_tokens: u64,
    /// Unfunded demand after allocation.
    pub deficit_tokens: u64,
}

impl PathAllocation {
    pub fn info(&self) -> GovernorPathInfo {
        GovernorPathInfo {
            path_id: self.path_id,
            task_ids: self.task_ids.clone(),
            departments: self.departments.clone(),
            weight_tokens: self.weight_tokens,
            declared_weight_tokens: self.declared_weight_tokens,
            observed_tokens: self.observed_tokens,
            floor_tokens: self.floor_tokens,
            allocation_tokens: self.allocation_tokens,
            deficit_tokens: self.deficit_tokens,
        }
    }
}

/// One department's share of the multi-path reserve before path split.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepartmentFloor {
    pub department: String,
    /// Sum of selected path weights touching this department (each path once).
    pub weight_tokens: u64,
    /// Department floor inside the protected pool.
    pub floor_tokens: u64,
    /// Selected path ranks that touch the department.
    pub path_ids: Vec<u32>,
}

impl DepartmentFloor {
    pub fn info(&self) -> vp_protocol::types::GovernorDepartmentInfo {
        vp_protocol::types::GovernorDepartmentInfo {
            department: self.department.clone(),
            weight_tokens: self.weight_tokens,
            floor_tokens: self.floor_tokens,
            path_ids: self.path_ids.clone(),
        }
    }
}

/// Complete deterministic result of one multi-path computation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MultiPathAllocation {
    pub session_id: String,
    pub project_id: String,
    pub plan_revision: u32,
    /// No remaining tasks and no protected paths.
    pub complete: bool,
    pub task_count: usize,
    pub edge_count: usize,
    pub paths: Vec<PathAllocation>,
    /// Per-department floors derived while allocating the reserve.
    pub departments: Vec<DepartmentFloor>,
    /// Number of maximal paths enumerated before selection.
    pub enumerated_paths: usize,
    /// Enumeration hit `MAX_ENUMERATED_PATHS` before exhausting the DAG.
    pub truncated: bool,
    /// Sum of selected path weights.
    pub path_weight_tokens: u64,
    pub ratio_reserve_tokens: u64,
    /// Total reserve held back from non-critical work.
    pub protected_tokens: u64,
    /// Sum of path allocations (never above `protected_tokens`).
    pub allocated_tokens: u64,
    /// Sum of department floors inside the allocations.
    pub floor_tokens: u64,
    pub inputs_hash: String,
}

impl MultiPathAllocation {
    /// Deterministic protected membership: every task on a selected path.
    pub fn critical_task_ids(&self) -> HashSet<&str> {
        self.paths
            .iter()
            .flat_map(|path| path.task_ids.iter().map(String::as_str))
            .collect()
    }

    /// Path with the largest unfunded demand. Ties use the lower path id so
    /// the result is stable.
    pub fn most_starved_path(&self) -> Option<&PathAllocation> {
        self.paths
            .iter()
            .max_by(|left, right| {
                left.deficit_tokens
                    .cmp(&right.deficit_tokens)
                    .then_with(|| right.path_id.cmp(&left.path_id))
            })
            .filter(|path| path.deficit_tokens > 0)
    }

    /// Highest deficit among selected paths containing `task_id`. A task on
    /// several paths (shared prefix) inherits its least-funded path.
    pub fn task_deficit(&self, task_id: &str) -> u64 {
        self.paths
            .iter()
            .filter(|path| path.task_ids.iter().any(|id| id == task_id))
            .map(|path| path.deficit_tokens)
            .max()
            .unwrap_or(0)
    }

    /// Lowest path id containing `task_id`.
    pub fn task_path_id(&self, task_id: &str) -> Option<u32> {
        self.task_path_ids(task_id).into_iter().next()
    }

    /// Every selected path containing `task_id`, in table order. A shared
    /// prefix task can belong to several protected paths.
    pub fn task_path_ids(&self, task_id: &str) -> Vec<u32> {
        self.paths
            .iter()
            .filter(|path| path.task_ids.iter().any(|id| id == task_id))
            .map(|path| path.path_id)
            .collect()
    }

    /// Bounded table rows for protocol/UI surfaces.
    pub fn path_infos(&self) -> Vec<GovernorPathInfo> {
        self.paths
            .iter()
            .take(MAX_SELECTED_PATHS)
            .map(PathAllocation::info)
            .collect()
    }

    /// Bounded department-floor rows for preview/status/events.
    pub fn department_infos(&self) -> Vec<vp_protocol::types::GovernorDepartmentInfo> {
        self.departments
            .iter()
            .take(MAX_DEPARTMENT_ROWS)
            .map(DepartmentFloor::info)
            .collect()
    }

    /// One-line explanation for events and the CLI.
    pub fn detail(&self) -> String {
        let paths = self
            .paths
            .iter()
            .take(3)
            .map(|path| {
                format!(
                    "#{} [{}] {}→{}",
                    path.path_id,
                    path.task_ids.join("->"),
                    path.weight_tokens,
                    path.allocation_tokens
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        let suffix = if self.paths.len() > 3 {
            format!(" +{} more", self.paths.len() - 3)
        } else {
            String::new()
        };
        format!(
            "{} paths · allocated {} of protected {} · {paths}{suffix}",
            self.paths.len(),
            self.allocated_tokens,
            self.protected_tokens
        )
    }
}

/// Inputs to one pure multi-path computation.
pub struct MultiPathInput<'a> {
    pub session_id: &'a str,
    pub project_id: &'a str,
    pub plan_revision: u32,
    pub plan: &'a vp_protocol::types::TeamPlan,
    pub statuses: &'a HashMap<String, String>,
    /// Remaining session budget at decision time.
    pub remaining_budget_tokens: Option<u64>,
    pub policy: &'a CriticalPathPolicy,
    /// Observed task spend (input+output tokens) from the usage ledger.
    pub observed_tokens: &'a HashMap<String, u64>,
}

/// Compute the bounded multi-path table and reserve allocation.
///
/// Validation is delegated to the F5 single-path computation first, so a
/// duplicate id, unknown dependency, cycle, or oversized DAG fails closed with
/// exactly the same `BUDGET_PATH_*` codes. The multi-path pass then walks the
/// already-validated remaining graph.
pub fn compute_multi_path(
    input: MultiPathInput<'_>,
) -> Result<MultiPathAllocation, CriticalPathError> {
    if input.plan.tasks.len() > MAX_PATH_TASKS {
        // Defensive duplicate of the F5 bound; also keeps the enumeration
        // stack depth and table size clearly bounded for readers.
        return Err(CriticalPathError {
            code: "BUDGET_PATH_TOO_LARGE",
            message: format!(
                "plan task count {} exceeds the governor limit {MAX_PATH_TASKS}",
                input.plan.tasks.len()
            ),
            hint: "Split the plan or reduce dependency edges; the governor fails closed on oversized DAGs.",
        });
    }
    // The single-path call is the validation + compatibility anchor. Its
    // inputs hash is reused in the multi-path identity.
    let single = compute_critical_path(CriticalPathInput {
        session_id: input.session_id,
        project_id: input.project_id,
        plan_revision: input.plan_revision,
        plan: input.plan,
        statuses: input.statuses,
        remaining_budget_tokens: input.remaining_budget_tokens,
    })?;
    let policy = input.policy.clone().normalized();
    let (mut paths, enumerated_paths, truncated) = enumerate_paths(input.plan, input.statuses);
    if paths.is_empty() {
        return Ok(MultiPathAllocation {
            session_id: input.session_id.to_string(),
            project_id: input.project_id.to_string(),
            plan_revision: input.plan_revision,
            complete: true,
            task_count: input.plan.tasks.len(),
            edge_count: single.edge_count,
            paths: Vec::new(),
            departments: Vec::new(),
            enumerated_paths,
            truncated,
            path_weight_tokens: 0,
            ratio_reserve_tokens: 0,
            protected_tokens: 0,
            allocated_tokens: 0,
            floor_tokens: 0,
            inputs_hash: multi_hash(&input, &single, &[], &[], 0, 0, 0),
        });
    }

    // Weight every enumerated path before selecting the top K. Selection is
    // stable: heavier first, then lexicographic task-id sequence.
    let task_by_id = input
        .plan
        .tasks
        .iter()
        .map(|task| (task.id.as_str(), task))
        .collect::<HashMap<_, _>>();
    let mut weighted = paths
        .drain(..)
        .map(|task_ids| {
            let mut declared = 0u64;
            let mut observed = 0u64;
            let mut departments = BTreeSet::new();
            for task_id in &task_ids {
                if let Some(task) = task_by_id.get(task_id.as_str()) {
                    declared = declared.saturating_add(task_weight_tokens(task));
                    observed = observed.saturating_add(
                        input
                            .observed_tokens
                            .get(task_id)
                            .copied()
                            .unwrap_or(0)
                            .min(MAX_PATH_WEIGHT_TOKENS),
                    );
                    departments.insert(task.department.clone());
                }
            }
            let declared = declared.min(MAX_PATH_WEIGHT_TOKENS);
            let observed = observed.min(MAX_PATH_WEIGHT_TOKENS);
            let blended = blend_weight_tokens(declared, observed, policy.observed_weight_ratio);
            WeightedPath {
                task_ids,
                departments: departments.into_iter().collect(),
                weight_tokens: blended,
                declared_weight_tokens: declared,
                observed_tokens: observed,
            }
        })
        .collect::<Vec<_>>();
    weighted.sort_by(|left, right| {
        right
            .weight_tokens
            .cmp(&left.weight_tokens)
            .then_with(|| left.task_ids.cmp(&right.task_ids))
    });
    let max_paths = (policy.max_paths as usize)
        .clamp(MIN_SELECTED_PATHS, MAX_SELECTED_PATHS)
        .min(weighted.len());
    weighted.truncate(max_paths);

    let path_weight_tokens = weighted
        .iter()
        .fold(0u64, |total, path| total.saturating_add(path.weight_tokens));
    let remaining = input.remaining_budget_tokens;
    let ratio_reserve_tokens = remaining
        .map(|remaining| ratio_ceil(remaining, policy.reserve_ratio))
        .unwrap_or(0);
    // A completed path protects nothing. The pool is bounded by both the
    // session remainder and the selected path demand plus the ratio buffer.
    let protects = !single.complete && path_weight_tokens > 0;
    let protected_tokens = if protects {
        remaining
            .map(|remaining| {
                path_weight_tokens
                    .saturating_add(ratio_reserve_tokens)
                    .min(remaining)
            })
            .unwrap_or(0)
    } else {
        0
    };
    let weights = weighted
        .iter()
        .map(|path| path.weight_tokens)
        .collect::<Vec<_>>();
    let (floors, departments) =
        department_floors(&weighted, protected_tokens, policy.department_min_ratio);
    let allocations = water_fill(&weights, &floors, protected_tokens);
    let floor_tokens = floors.iter().copied().sum::<u64>();
    let allocated_tokens = allocations.iter().copied().sum::<u64>();
    let paths = weighted
        .into_iter()
        .enumerate()
        .map(|(index, path)| {
            let allocation = allocations.get(index).copied().unwrap_or(0);
            PathAllocation {
                path_id: index.min(u32::MAX as usize) as u32,
                task_ids: path.task_ids,
                departments: path.departments,
                weight_tokens: path.weight_tokens,
                declared_weight_tokens: path.declared_weight_tokens,
                observed_tokens: path.observed_tokens,
                floor_tokens: floors.get(index).copied().unwrap_or(0),
                allocation_tokens: allocation,
                deficit_tokens: path.weight_tokens.saturating_sub(allocation),
            }
        })
        .collect::<Vec<_>>();
    let inputs_hash = multi_hash(
        &input,
        &single,
        &paths,
        &departments,
        floor_tokens,
        allocated_tokens,
        protected_tokens,
    );
    Ok(MultiPathAllocation {
        session_id: input.session_id.to_string(),
        project_id: input.project_id.to_string(),
        plan_revision: input.plan_revision,
        complete: single.complete,
        task_count: input.plan.tasks.len(),
        edge_count: single.edge_count,
        paths,
        departments,
        enumerated_paths,
        truncated,
        path_weight_tokens,
        ratio_reserve_tokens,
        protected_tokens,
        allocated_tokens,
        floor_tokens,
        inputs_hash,
    })
}

#[derive(Debug, Clone)]
struct WeightedPath {
    task_ids: Vec<String>,
    departments: Vec<String>,
    weight_tokens: u64,
    declared_weight_tokens: u64,
    observed_tokens: u64,
}

/// Enumerate maximal remaining root-to-leaf paths in deterministic order.
///
/// A root is a remaining task with no remaining dependency; a leaf is a
/// remaining task with no remaining dependent. Completed/skipped/cancelled
/// tasks are removed and their dependents are treated as roots. The DFS walks
/// roots and dependents in task-id order, so the enumeration itself has no
/// hash-map iteration order.
fn enumerate_paths(
    plan: &vp_protocol::types::TeamPlan,
    statuses: &HashMap<String, String>,
) -> (Vec<Vec<String>>, usize, bool) {
    let index_by_id = plan
        .tasks
        .iter()
        .enumerate()
        .map(|(index, task)| (task.id.as_str(), index))
        .collect::<HashMap<_, _>>();
    let remaining = plan
        .tasks
        .iter()
        .enumerate()
        .filter(|(_, task)| {
            statuses
                .get(&task.id)
                .map(|status| is_remaining_status(status))
                .unwrap_or(true)
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let remaining_set = remaining.iter().copied().collect::<HashSet<_>>();
    let mut deps_remaining = vec![0usize; plan.tasks.len()];
    let mut dependents = vec![Vec::<usize>::new(); plan.tasks.len()];
    for index in &remaining {
        for dependency in &plan.tasks[*index].deps {
            if let Some(dep_index) = index_by_id.get(dependency.as_str()).copied()
                && remaining_set.contains(&dep_index)
            {
                deps_remaining[*index] = deps_remaining[*index].saturating_add(1);
                dependents[dep_index].push(*index);
            }
        }
    }
    for list in &mut dependents {
        list.sort_by(|left, right| plan.tasks[*left].id.cmp(&plan.tasks[*right].id));
    }
    let mut roots = remaining
        .iter()
        .copied()
        .filter(|index| deps_remaining[*index] == 0)
        .collect::<Vec<_>>();
    roots.sort_by(|left, right| plan.tasks[*left].id.cmp(&plan.tasks[*right].id));

    let mut paths = Vec::new();
    let mut truncated = false;
    let mut stack = Vec::with_capacity(MAX_PATH_TASKS);
    for root in roots {
        if truncated {
            break;
        }
        walk_paths(
            root,
            plan,
            &dependents,
            &mut stack,
            &mut paths,
            &mut truncated,
        );
    }
    let enumerated = paths.len();
    (paths, enumerated, truncated)
}

fn walk_paths(
    index: usize,
    plan: &vp_protocol::types::TeamPlan,
    dependents: &[Vec<usize>],
    stack: &mut Vec<usize>,
    paths: &mut Vec<Vec<String>>,
    truncated: &mut bool,
) {
    if *truncated {
        return;
    }
    stack.push(index);
    if dependents[index].is_empty() {
        if paths.len() >= MAX_ENUMERATED_PATHS {
            *truncated = true;
            stack.pop();
            return;
        }
        paths.push(
            stack
                .iter()
                .map(|index| plan.tasks[*index].id.clone())
                .collect(),
        );
    } else {
        for dependent in &dependents[index] {
            walk_paths(*dependent, plan, dependents, stack, paths, truncated);
            if *truncated {
                break;
            }
        }
    }
    stack.pop();
}

/// Observed spend is clamped to this multiple of declared demand. Without a
/// bound, one runaway path could dominate every future decision; with it,
/// observed spend is evidence, not an unbounded multiplier.
pub const OBSERVED_WEIGHT_MAX_MULTIPLIER: u64 = 2;

/// Blend declared path weight with observed ledger spend.
///
/// `observed_weight_ratio` is converted to basis points so the result is an
/// integer and deterministic across platforms. A path with no observed spend
/// keeps its declared weight: an unstarted path is never punished for having
/// no ledger rows yet. Observed spend is clamped to
/// `OBSERVED_WEIGHT_MAX_MULTIPLIER * declared`, so a runaway path can at most
/// double its blended weight. A non-empty path always keeps a positive weight.
pub fn blend_weight_tokens(declared: u64, observed: u64, observed_weight_ratio: f64) -> u64 {
    let basis_points = ratio_basis_points(observed_weight_ratio);
    let declared = declared.min(MAX_PATH_WEIGHT_TOKENS);
    let observed = observed.min(MAX_PATH_WEIGHT_TOKENS);
    if basis_points == 0 || observed == 0 {
        return declared.max(1);
    }
    let upper_bound = declared
        .saturating_mul(OBSERVED_WEIGHT_MAX_MULTIPLIER)
        .max(1);
    let observed = observed.min(upper_bound);
    let declared_share = u128::from(declared) * u128::from(10_000 - basis_points);
    let observed_share = u128::from(observed) * u128::from(basis_points);
    let blended = (declared_share.saturating_add(observed_share)) / 10_000;
    (blended.min(u128::from(MAX_PATH_WEIGHT_TOKENS)) as u64).max(1)
}

/// Deterministic per-department floors expressed as per-path minimums.
///
/// A department's share is proportional to the total weight of paths that
/// touch it, and each department floor is split across those paths by weight.
/// Integer division can only under-allocate, so the sum of path floors is at
/// most `reserve_pool`. A path floor is capped at its own demand; capped
/// tokens go back to the weighted water-fill instead of being lost.
///
/// The returned department rows are explainability data. They are sorted by
/// weight then name and bounded by `MAX_DEPARTMENT_ROWS`; the floor sum is
/// computed from all departments before the display bound.
fn department_floors(
    paths: &[WeightedPath],
    reserve_pool: u64,
    department_min_ratio: f64,
) -> (Vec<u64>, Vec<DepartmentFloor>) {
    let mut floors = vec![0u64; paths.len()];
    if paths.is_empty() {
        return (floors, Vec::new());
    }
    let basis_points = ratio_basis_points(department_min_ratio);
    let total_weight = paths
        .iter()
        .fold(0u64, |total, path| total.saturating_add(path.weight_tokens));
    if total_weight == 0 {
        return (floors, Vec::new());
    }
    let mut department_weights = BTreeMap::<String, u64>::new();
    for path in paths {
        for department in &path.departments {
            let entry = department_weights.entry(department.clone()).or_insert(0);
            *entry = entry.saturating_add(path.weight_tokens);
        }
    }
    let mut department_floor_map = BTreeMap::<String, u64>::new();
    if basis_points > 0 && reserve_pool > 0 {
        let mut scaled_floors = department_weights
            .iter()
            .map(|(department, weight)| {
                let floor =
                    (u128::from(reserve_pool) * u128::from(basis_points) * u128::from(*weight))
                        / (10_000u128 * u128::from(total_weight));
                (department.clone(), floor.min(u128::from(u64::MAX)) as u64)
            })
            .collect::<Vec<_>>();
        let department_total = scaled_floors
            .iter()
            .fold(0u64, |total, (_, floor)| total.saturating_add(*floor));
        if department_total > reserve_pool && department_total > 0 {
            // Floors can only consume the pool. Scale down proportionally and
            // leave the fractional remainder for weighted water-filling.
            for (_, floor) in &mut scaled_floors {
                *floor = ((u128::from(*floor) * u128::from(reserve_pool))
                    / u128::from(department_total))
                .min(u128::from(u64::MAX)) as u64;
            }
        }
        department_floor_map = scaled_floors.into_iter().collect();
    }
    for (index, path) in paths.iter().enumerate() {
        let mut floor = 0u64;
        for department in &path.departments {
            let department_weight = department_weights.get(department).copied().unwrap_or(0);
            let department_floor = department_floor_map.get(department).copied().unwrap_or(0);
            if department_weight == 0 {
                continue;
            }
            let share = (u128::from(department_floor) * u128::from(path.weight_tokens)
                / u128::from(department_weight))
            .min(u128::from(u64::MAX)) as u64;
            floor = floor.saturating_add(share);
        }
        floors[index] = floor.min(path.weight_tokens).min(reserve_pool);
    }
    let floor_total = floors.iter().copied().sum::<u64>();
    if floor_total > reserve_pool && floor_total > 0 {
        for floor in &mut floors {
            *floor = ((u128::from(*floor) * u128::from(reserve_pool)) / u128::from(floor_total))
                .min(u128::from(u64::MAX)) as u64;
        }
    }
    let mut departments = department_weights
        .iter()
        .map(|(department, weight)| DepartmentFloor {
            department: department.clone(),
            weight_tokens: *weight,
            floor_tokens: department_floor_map.get(department).copied().unwrap_or(0),
            path_ids: paths
                .iter()
                .enumerate()
                .filter(|(_, path)| path.departments.iter().any(|value| value == department))
                .map(|(index, _)| index.min(u32::MAX as usize) as u32)
                .collect(),
        })
        .collect::<Vec<_>>();
    departments.sort_by(|left, right| {
        right
            .weight_tokens
            .cmp(&left.weight_tokens)
            .then_with(|| left.department.cmp(&right.department))
    });
    departments.truncate(MAX_DEPARTMENT_ROWS);
    (floors, departments)
}

/// Deterministic integer water-filling.
///
/// Each path starts at its floor. The remaining pool is repeatedly distributed
/// proportionally to path weight; a path whose proportional share exceeds its
/// unfunded demand is capped at that demand and the leftover is redistributed
/// in the next round. A final largest-remainder pass consumes rounding
/// leftovers with path id as the tie-break. The result always sums to
/// `min(pool, sum(weights))` when floors are within demand.
pub fn water_fill(weights: &[u64], floors: &[u64], pool: u64) -> Vec<u64> {
    let mut allocations = weights
        .iter()
        .enumerate()
        .map(|(index, weight)| floors.get(index).copied().unwrap_or(0).min(*weight))
        .collect::<Vec<_>>();
    // A caller-supplied floor set can exceed the pool. Scale it down rather
    // than returning an allocation above the protected reserve; the governor
    // itself never produces such floors, but the pure helper must be safe on
    // its own.
    let floor_total = allocations.iter().copied().sum::<u64>();
    if floor_total > pool && floor_total > 0 {
        for allocation in &mut allocations {
            *allocation = ((u128::from(*allocation) * u128::from(pool)) / u128::from(floor_total))
                .min(u128::from(u64::MAX)) as u64;
        }
    }
    let mut free = pool.saturating_sub(allocations.iter().copied().sum::<u64>());
    if free == 0 {
        return allocations;
    }
    // Bound the redistribution loop by the number of paths plus one. Every
    // round either saturates at least one path or distributes the remainder.
    for _ in 0..=weights.len() {
        if free == 0 {
            break;
        }
        let active = weights
            .iter()
            .enumerate()
            .filter(|(index, weight)| allocations[*index] < **weight)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if active.is_empty() {
            break;
        }
        let total_weight = active
            .iter()
            .fold(0u128, |total, index| total + u128::from(weights[*index]));
        if total_weight == 0 {
            // Degenerate all-zero weights: distribute one token per path in
            // path-id order so the pool is not silently lost.
            for index in active {
                if free == 0 {
                    break;
                }
                if allocations[index] < weights[index] {
                    allocations[index] += 1;
                    free -= 1;
                }
            }
            break;
        }
        let mut capped = false;
        for index in &active {
            let capacity = weights[*index].saturating_sub(allocations[*index]);
            let exact = u128::from(free) * u128::from(weights[*index]) / total_weight;
            let share = exact.min(u128::from(capacity)) as u64;
            allocations[*index] += share;
            if share == capacity {
                capped = true;
            }
        }
        free = pool.saturating_sub(allocations.iter().copied().sum::<u64>());
        if free == 0 {
            break;
        }
        if capped {
            // Redistribute the leftover to still-unsaturated paths.
            continue;
        }
        // No path was capped, so the leftover is integer rounding. Give one
        // token each by largest remainder, then path id.
        let mut order = active;
        order.sort_by(|left, right| {
            let left_remainder = u128::from(free) * u128::from(weights[*left]) % total_weight;
            let right_remainder = u128::from(free) * u128::from(weights[*right]) % total_weight;
            right_remainder
                .cmp(&left_remainder)
                .then_with(|| left.cmp(right))
        });
        for index in order {
            if free == 0 {
                break;
            }
            if allocations[index] < weights[index] {
                allocations[index] += 1;
                free -= 1;
            }
        }
        break;
    }
    allocations
}

/// Deterministic starvation credit for one queued task.
///
/// `starvation_after_secs = 0` disables aging. Otherwise every completed
/// interval grants `STARVATION_PRIORITY_PER_INTERVAL` points, capped at
/// `MAX_STARVATION_CREDIT`. Invalid timestamps yield zero credit instead of
/// guessing from wall-clock time; the caller supplies both timestamps.
pub fn starvation_credit(enqueued_at: &str, now: &str, starvation_after_secs: u64) -> i64 {
    if starvation_after_secs == 0 {
        return 0;
    }
    let Ok(enqueued) =
        time::OffsetDateTime::parse(enqueued_at, &time::format_description::well_known::Rfc3339)
    else {
        return 0;
    };
    let Ok(now) = time::OffsetDateTime::parse(now, &time::format_description::well_known::Rfc3339)
    else {
        return 0;
    };
    let age = (now - enqueued).whole_seconds().max(0) as u64;
    if age < starvation_after_secs {
        return 0;
    }
    let intervals = age / starvation_after_secs;
    (intervals as i64)
        .saturating_mul(STARVATION_PRIORITY_PER_INTERVAL)
        .min(MAX_STARVATION_CREDIT)
}

fn ratio_basis_points(ratio: f64) -> u64 {
    if !ratio.is_finite() {
        return 0;
    }
    (ratio.clamp(0.0, 1.0) * 10_000.0).round() as u64
}

fn ratio_ceil(value: u64, ratio: f64) -> u64 {
    let basis_points = ratio_basis_points(ratio);
    if basis_points == 0 {
        return 0;
    }
    value.saturating_mul(basis_points).div_ceil(10_000)
}

fn multi_hash(
    input: &MultiPathInput<'_>,
    single: &crate::critical_path::CriticalPath,
    paths: &[PathAllocation],
    departments: &[DepartmentFloor],
    floor_tokens: u64,
    allocated_tokens: u64,
    protected_tokens: u64,
) -> String {
    let policy = input.policy.clone().normalized();
    let mut material = String::new();
    material.push_str("multi_path.v1\n");
    material.push_str(input.session_id);
    material.push('\n');
    material.push_str(input.project_id);
    material.push('\n');
    material.push_str(&input.plan_revision.to_string());
    material.push('\n');
    material.push_str(&single.inputs_hash);
    material.push('\n');
    material.push_str(policy.mode.as_str());
    material.push('\n');
    material.push_str(&policy.max_paths.to_string());
    material.push('\n');
    material.push_str(&ratio_basis_points(policy.department_min_ratio).to_string());
    material.push('\n');
    material.push_str(&ratio_basis_points(policy.observed_weight_ratio).to_string());
    material.push('\n');
    material.push_str(&policy.starvation_after_secs.to_string());
    material.push('\n');
    material.push_str(&floor_tokens.to_string());
    material.push('\n');
    material.push_str(&allocated_tokens.to_string());
    material.push('\n');
    material.push_str(&protected_tokens.to_string());
    material.push('\n');
    for path in paths {
        material.push_str(&path.path_id.to_string());
        material.push('|');
        material.push_str(&path.task_ids.join(","));
        material.push('|');
        material.push_str(&path.departments.join(","));
        material.push('|');
        material.push_str(&path.weight_tokens.to_string());
        material.push('|');
        material.push_str(&path.declared_weight_tokens.to_string());
        material.push('|');
        material.push_str(&path.observed_tokens.to_string());
        material.push('|');
        material.push_str(&path.floor_tokens.to_string());
        material.push('|');
        material.push_str(&path.allocation_tokens.to_string());
        material.push('|');
        material.push_str(&path.deficit_tokens.to_string());
        material.push('\n');
    }
    for department in departments {
        material.push_str(&department.department);
        material.push('|');
        material.push_str(&department.weight_tokens.to_string());
        material.push('|');
        material.push_str(&department.floor_tokens.to_string());
        material.push('|');
        let path_ids = department
            .path_ids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        material.push_str(&path_ids);
        material.push('\n');
    }
    let mut observed = input.observed_tokens.iter().collect::<Vec<_>>();
    observed.sort_by(|left, right| left.0.cmp(right.0));
    for (task_id, tokens) in observed {
        material.push_str(task_id);
        material.push('|');
        material.push_str(&tokens.to_string());
        material.push('\n');
    }
    crate::sha256_hex(material.as_bytes())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use vp_protocol::types::{CriticalPathMode, PlanDepartment, PlanTask, TeamPlan};

    use super::*;

    fn task(id: &str, department: &str, deps: &[&str], budget: u64) -> PlanTask {
        PlanTask {
            id: id.to_string(),
            department: department.to_string(),
            title: id.to_string(),
            kind: "coding".to_string(),
            priority: 1,
            profile: None,
            deps: deps.iter().map(|dep| (*dep).to_string()).collect(),
            write_scope: vec![id.to_string()],
            acceptance: vec!["done".to_string()],
            token_budget: Some(budget),
            cost_budget_usd: None,
        }
    }

    fn plan(tasks: Vec<PlanTask>) -> TeamPlan {
        TeamPlan {
            goal: "multi".to_string(),
            departments: vec![
                PlanDepartment {
                    name: "impl".to_string(),
                    manager_title: "Implementation".to_string(),
                    profile: "coding".to_string(),
                    objective: "build".to_string(),
                    token_budget: None,
                    cost_budget_usd: None,
                },
                PlanDepartment {
                    name: "qa".to_string(),
                    manager_title: "Verification".to_string(),
                    profile: "verification".to_string(),
                    objective: "verify".to_string(),
                    token_budget: None,
                    cost_budget_usd: None,
                },
            ],
            tasks,
        }
    }

    fn multi_policy() -> CriticalPathPolicy {
        CriticalPathPolicy {
            enabled: true,
            mode: CriticalPathMode::Multi,
            reserve_ratio: 0.25,
            throttle_at_ratio: 0.0,
            max_paths: 4,
            ..CriticalPathPolicy::default()
        }
    }

    fn run(
        plan: &TeamPlan,
        statuses: &HashMap<String, String>,
        policy: &CriticalPathPolicy,
        remaining: Option<u64>,
        observed: &HashMap<String, u64>,
    ) -> MultiPathAllocation {
        compute_multi_path(MultiPathInput {
            session_id: "s1",
            project_id: "p1",
            plan_revision: 1,
            plan,
            statuses,
            remaining_budget_tokens: remaining,
            policy,
            observed_tokens: observed,
        })
        .unwrap()
    }

    #[test]
    fn two_independent_chains_are_both_protected_and_weighted() {
        let plan = plan(vec![
            task("a1", "impl", &[], 5_000),
            task("a2", "impl", &["a1"], 5_000),
            task("b1", "qa", &[], 2_000),
            task("b2", "qa", &["b1"], 2_000),
        ]);
        let allocation = run(
            &plan,
            &HashMap::new(),
            &multi_policy(),
            Some(20_000),
            &HashMap::new(),
        );
        assert_eq!(allocation.paths.len(), 2);
        assert_eq!(allocation.paths[0].task_ids, vec!["a1", "a2"]);
        assert_eq!(allocation.paths[0].weight_tokens, 10_000);
        assert_eq!(allocation.paths[1].task_ids, vec!["b1", "b2"]);
        assert_eq!(allocation.paths[1].weight_tokens, 4_000);
        // Loose budget: both paths are funded to their full demand and the
        // ratio buffer remains unallocated.
        assert_eq!(allocation.paths[0].allocation_tokens, 10_000);
        assert_eq!(allocation.paths[1].allocation_tokens, 4_000);
        assert_eq!(allocation.allocated_tokens, 14_000);
        assert_eq!(allocation.path_weight_tokens, 14_000);
        assert_eq!(allocation.ratio_reserve_tokens, 5_000);
        assert_eq!(allocation.protected_tokens, 19_000);
        assert_eq!(allocation.paths[0].deficit_tokens, 0);
        assert_eq!(allocation.paths[1].deficit_tokens, 0);
    }

    #[test]
    fn tight_budget_water_fills_proportionally_to_weight() {
        let plan = plan(vec![
            task("heavy-a", "impl", &[], 6_000),
            task("heavy-b", "impl", &["heavy-a"], 6_000),
            task("light", "qa", &[], 2_000),
        ]);
        // path weights 12,000 and 2,000; ratio reserve 0 so the pool is the
        // full remaining budget of 7,000.
        let mut policy = multi_policy();
        policy.reserve_ratio = 0.0;
        let allocation = run(
            &plan,
            &HashMap::new(),
            &policy,
            Some(7_000),
            &HashMap::new(),
        );
        assert_eq!(allocation.protected_tokens, 7_000);
        assert_eq!(allocation.paths[0].allocation_tokens, 6_000);
        assert_eq!(allocation.paths[1].allocation_tokens, 1_000);
        assert_eq!(allocation.allocated_tokens, 7_000);
        assert_eq!(allocation.paths[0].deficit_tokens, 6_000);
        assert_eq!(allocation.paths[1].deficit_tokens, 1_000);
    }

    #[test]
    fn water_fill_caps_at_demand_and_redistributes() {
        // Proportional fill: 10% and 90% of the 5,000 pool.
        let allocations = water_fill(&[1_000, 9_000], &[0, 0], 5_000);
        assert_eq!(allocations, vec![500, 4_500]);
        let exact = water_fill(&[1_000, 9_000], &[0, 0], 10_000);
        assert_eq!(exact, vec![1_000, 9_000]);
        // The small path saturates at 100 and the rest flows to the large one.
        let capped = water_fill(&[100, 900], &[0, 0], 5_000);
        assert_eq!(capped, vec![100, 900]);
        let partial = water_fill(&[100, 900], &[0, 0], 500);
        assert_eq!(partial, vec![50, 450]);
        let floors = water_fill(&[1_000, 9_000], &[500, 0], 2_000);
        assert_eq!(floors, vec![650, 1_350]);
        let zero = water_fill(&[0, 0], &[0, 0], 10);
        assert_eq!(zero, vec![0, 0]);
        let over = water_fill(&[100], &[0], 1_000);
        assert_eq!(over, vec![100]);
    }

    #[test]
    fn water_fill_never_exceeds_demand_or_pool_and_sums_exactly() {
        let cases: &[(&[u64], u64)] = &[
            (&[1, 2, 3, 4], 0),
            (&[1, 2, 3, 4], 1),
            (&[1, 2, 3, 4], 5),
            (&[7, 7, 7], 10),
            (&[100, 1], 50),
            (&[100, 1], 1_000),
            (&[9_999, 1, 1], 5_000),
            (&[1, 1, 1, 1, 1, 1, 1, 1], 3),
            (&[u64::MAX / 4, u64::MAX / 4], 10_000),
        ];
        for (weights, pool) in cases {
            let allocations = water_fill(weights, &vec![0; weights.len()], *pool);
            let total_demand = weights.iter().copied().sum::<u64>();
            assert_eq!(
                allocations.iter().copied().sum::<u64>(),
                (*pool).min(total_demand),
                "weights {weights:?} pool {pool}"
            );
            for (index, allocation) in allocations.iter().enumerate() {
                assert!(*allocation <= weights[index]);
            }
            // Re-running must produce identical allocations.
            assert_eq!(
                allocations,
                water_fill(weights, &vec![0; weights.len()], *pool)
            );
        }
    }

    #[test]
    fn water_fill_preserves_floors_and_caps_at_demand() {
        let allocations = water_fill(&[100, 200, 300], &[10, 20, 30], 60);
        assert_eq!(allocations.iter().sum::<u64>(), 60);
        assert!(allocations[0] >= 10);
        assert!(allocations[1] >= 20);
        assert!(allocations[2] >= 30);
        assert!(
            allocations
                .iter()
                .zip([100, 200, 300])
                .all(|(a, w)| *a <= w)
        );
        let capped = water_fill(&[10, 10], &[10, 10], 100);
        assert_eq!(capped, vec![10, 10]);
    }

    #[test]
    fn water_fill_rounding_leftover_is_deterministic() {
        let first = water_fill(&[1, 1, 1], &[0, 0, 0], 2);
        assert_eq!(first, vec![1, 1, 0]);
        let second = water_fill(&[1, 1, 1], &[0, 0, 0], 2);
        assert_eq!(first, second);
        let three = water_fill(&[1, 1, 1], &[0, 0, 0], 4);
        assert_eq!(three.iter().sum::<u64>(), 3);
    }

    #[test]
    fn department_floors_apply_before_weighted_fill() {
        let plan = plan(vec![
            task("a1", "impl", &[], 9_000),
            task("a2", "impl", &["a1"], 9_000),
            task("b1", "qa", &[], 1_000),
            task("b2", "qa", &["b1"], 1_000),
        ]);
        let mut policy = multi_policy();
        policy.reserve_ratio = 0.0;
        policy.department_min_ratio = 0.25;
        let allocation = run(
            &plan,
            &HashMap::new(),
            &policy,
            Some(10_000),
            &HashMap::new(),
        );
        // Reserve pool 10,000. Weighted shares would be 9,000 / 1,000; the
        // 25% department floor lifts qa before the remaining pool is filled.
        assert_eq!(allocation.paths[0].floor_tokens, 2_250);
        assert_eq!(allocation.paths[1].floor_tokens, 250);
        assert!(allocation.paths[1].allocation_tokens >= 625);
        assert_eq!(allocation.allocated_tokens, 10_000);
        assert_eq!(allocation.floor_tokens, 2_500);
    }

    #[test]
    fn department_table_reports_weight_floors_and_path_ids() {
        let plan = plan(vec![
            task("impl-1", "impl", &[], 6_000),
            task("impl-2", "impl", &["impl-1"], 4_000),
            task("qa-1", "qa", &[], 2_000),
        ]);
        let mut policy = multi_policy();
        policy.reserve_ratio = 0.0;
        policy.department_min_ratio = 0.5;
        let allocation = run(
            &plan,
            &HashMap::new(),
            &policy,
            Some(10_000),
            &HashMap::new(),
        );
        assert_eq!(allocation.departments.len(), 2);
        assert_eq!(allocation.departments[0].department, "impl");
        assert_eq!(allocation.departments[0].weight_tokens, 10_000);
        assert_eq!(allocation.departments[0].floor_tokens, 4_166);
        assert_eq!(allocation.departments[0].path_ids, vec![0]);
        assert_eq!(allocation.departments[1].department, "qa");
        assert_eq!(allocation.departments[1].weight_tokens, 2_000);
        assert_eq!(allocation.departments[1].floor_tokens, 833);
        assert_eq!(allocation.departments[1].path_ids, vec![1]);
        let rows = allocation.department_infos();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].department, "impl");
        assert_eq!(rows[1].path_ids, vec![1]);
        assert!(allocation.detail().contains("paths"));
    }

    #[test]
    fn shared_departments_count_each_path_once() {
        let plan = plan(vec![
            task("root", "impl", &[], 1_000),
            task("a", "impl", &["root"], 4_000),
            task("b", "qa", &["root"], 2_000),
        ]);
        let mut policy = multi_policy();
        policy.reserve_ratio = 0.0;
        policy.department_min_ratio = 0.25;
        let allocation = run(
            &plan,
            &HashMap::new(),
            &policy,
            Some(10_000),
            &HashMap::new(),
        );
        // Both paths touch impl through the shared root, but each path is
        // counted once: impl weight = 5,000 + 3,000 = 8,000, qa = 3,000.
        assert_eq!(allocation.departments.len(), 2);
        assert_eq!(allocation.departments[0].department, "impl");
        assert_eq!(allocation.departments[0].weight_tokens, 8_000);
        assert_eq!(allocation.departments[0].path_ids, vec![0, 1]);
        assert_eq!(allocation.departments[1].department, "qa");
        assert_eq!(allocation.departments[1].weight_tokens, 3_000);
        assert_eq!(allocation.departments[1].path_ids, vec![1]);
    }

    #[test]
    fn department_table_is_empty_without_paths_and_zero_without_floors() {
        let empty = run(
            &plan(Vec::new()),
            &HashMap::new(),
            &multi_policy(),
            Some(10),
            &HashMap::new(),
        );
        assert!(empty.departments.is_empty());
        let plan = plan(vec![task("a", "impl", &[], 1_000)]);
        let allocation = run(
            &plan,
            &HashMap::new(),
            &multi_policy(),
            Some(10_000),
            &HashMap::new(),
        );
        assert_eq!(allocation.departments.len(), 1);
        assert_eq!(allocation.departments[0].floor_tokens, 0);
        assert_eq!(allocation.departments[0].weight_tokens, 1_000);
    }

    #[test]
    fn department_floors_scale_down_to_the_pool() {
        let plan = plan(vec![
            task("a", "impl", &[], 1_000),
            task("b", "qa", &[], 1_000),
            task("c", "ops", &[], 1_000),
        ]);
        let mut policy = multi_policy();
        policy.reserve_ratio = 0.0;
        policy.department_min_ratio = 1.0;
        let allocation = run(
            &plan,
            &HashMap::new(),
            &policy,
            Some(2_000),
            &HashMap::new(),
        );
        assert!(allocation.floor_tokens <= 2_000);
        assert_eq!(allocation.allocated_tokens, 2_000);
        assert!(
            allocation
                .paths
                .iter()
                .all(|path| path.floor_tokens <= 1_000)
        );
    }

    #[test]
    fn observed_spend_blends_and_clamps() {
        let plan = plan(vec![
            task("spent", "impl", &[], 2_000),
            task("unstarted", "impl", &[], 4_000),
        ]);
        let mut observed = HashMap::new();
        observed.insert("spent".to_string(), 8_000u64);
        let mut policy = multi_policy();
        policy.reserve_ratio = 0.0;
        policy.observed_weight_ratio = 0.5;
        let allocation = run(&plan, &HashMap::new(), &policy, Some(20_000), &observed);
        // Observed is clamped to 2x declared (4,000) before blending:
        // (2,000 * 0.5) + (4,000 * 0.5) = 3,000. The unstarted path keeps its
        // full declared 4,000 and stays first.
        assert_eq!(allocation.paths[0].task_ids, vec!["unstarted"]);
        assert_eq!(allocation.paths[0].weight_tokens, 4_000);
        assert_eq!(allocation.paths[1].task_ids, vec!["spent"]);
        assert_eq!(allocation.paths[1].weight_tokens, 3_000);
        assert_eq!(allocation.paths[1].declared_weight_tokens, 2_000);
        assert_eq!(allocation.paths[1].observed_tokens, 8_000);
    }

    #[test]
    fn observed_spend_ratio_zero_keeps_declared_weights() {
        assert_eq!(blend_weight_tokens(5_000, 99_000, 0.0), 5_000);
        assert_eq!(blend_weight_tokens(5_000, 0, 1.0), 5_000);
        assert_eq!(blend_weight_tokens(5_000, 99_000, 1.0), 10_000);
        assert_eq!(blend_weight_tokens(0, 0, 0.5), 1);
        assert_eq!(
            blend_weight_tokens(u64::MAX, u64::MAX, 0.5),
            MAX_PATH_WEIGHT_TOKENS
        );
        assert_eq!(blend_weight_tokens(10, 20, f64::NAN), 10);
    }

    #[test]
    fn path_ids_and_ties_are_deterministic() {
        let plan = plan(vec![
            task("b", "impl", &[], 1_000),
            task("a", "impl", &[], 1_000),
        ]);
        let first = run(
            &plan,
            &HashMap::new(),
            &multi_policy(),
            Some(10_000),
            &HashMap::new(),
        );
        let second = run(
            &plan,
            &HashMap::new(),
            &multi_policy(),
            Some(10_000),
            &HashMap::new(),
        );
        assert_eq!(first, second);
        assert_eq!(first.paths[0].task_ids, vec!["a"]);
        assert_eq!(first.paths[1].task_ids, vec!["b"]);
    }

    #[test]
    fn max_paths_bounds_the_table() {
        let mut tasks = Vec::new();
        for index in 0..8 {
            tasks.push(task(&format!("t{index}"), "impl", &[], 1_000 + index));
        }
        let mut policy = multi_policy();
        policy.max_paths = 3;
        let allocation = run(
            &plan(tasks),
            &HashMap::new(),
            &policy,
            Some(100_000),
            &HashMap::new(),
        );
        assert_eq!(allocation.paths.len(), 3);
        assert_eq!(allocation.enumerated_paths, 8);
        assert_eq!(allocation.paths[0].task_ids, vec!["t7"]);
        assert_eq!(allocation.paths[2].task_ids, vec!["t5"]);
    }

    #[test]
    fn revision_resets_membership_and_hash() {
        let plan = plan(vec![
            task("a", "impl", &[], 1_000),
            task("b", "impl", &[], 1_000),
        ]);
        let allocation = run(
            &plan,
            &HashMap::new(),
            &multi_policy(),
            Some(10_000),
            &HashMap::new(),
        );
        let mut statuses = HashMap::new();
        statuses.insert("a".to_string(), "completed".to_string());
        let after = run(
            &plan,
            &statuses,
            &multi_policy(),
            Some(10_000),
            &HashMap::new(),
        );
        assert_ne!(allocation.inputs_hash, after.inputs_hash);
        assert_eq!(after.paths.len(), 1);
        assert_eq!(after.paths[0].task_ids, vec!["b"]);
    }

    #[test]
    fn empty_and_complete_plans_have_no_paths() {
        let empty = run(
            &plan(Vec::new()),
            &HashMap::new(),
            &multi_policy(),
            Some(10),
            &HashMap::new(),
        );
        assert!(empty.complete);
        assert!(empty.paths.is_empty());
        assert_eq!(empty.protected_tokens, 0);
        let plan = plan(vec![task("a", "impl", &[], 1_000)]);
        let mut statuses = HashMap::new();
        statuses.insert("a".to_string(), "completed".to_string());
        let complete = run(&plan, &statuses, &multi_policy(), Some(10), &HashMap::new());
        assert!(complete.complete);
        assert!(complete.paths.is_empty());
    }

    #[test]
    fn unbounded_budget_reports_weights_without_allocations() {
        let plan = plan(vec![
            task("a", "impl", &[], 5_000),
            task("b", "qa", &[], 2_000),
        ]);
        let allocation = run(
            &plan,
            &HashMap::new(),
            &multi_policy(),
            None,
            &HashMap::new(),
        );
        assert_eq!(allocation.protected_tokens, 0);
        assert_eq!(allocation.allocated_tokens, 0);
        assert_eq!(allocation.paths[0].weight_tokens, 5_000);
        assert_eq!(allocation.paths[0].deficit_tokens, 5_000);
    }

    #[test]
    fn completed_and_cancelled_tasks_leave_the_remaining_paths() {
        let plan = plan(vec![
            task("a", "impl", &[], 1_000),
            task("b", "impl", &["a"], 2_000),
            task("c", "qa", &[], 3_000),
            task("d", "qa", &["c"], 4_000),
        ]);
        let mut statuses = HashMap::new();
        statuses.insert("a".to_string(), "completed".to_string());
        statuses.insert("c".to_string(), "cancelled".to_string());
        let allocation = run(
            &plan,
            &statuses,
            &multi_policy(),
            Some(10_000),
            &HashMap::new(),
        );
        assert_eq!(allocation.paths.len(), 2);
        // `a` is completed so `b` becomes a root; `c` is cancelled so `d`
        // also becomes a root. The cancelled branch itself is not protected.
        assert!(
            allocation
                .paths
                .iter()
                .any(|path| path.task_ids == vec!["b"])
        );
        assert!(
            allocation
                .paths
                .iter()
                .any(|path| path.task_ids == vec!["d"])
        );
        assert!(
            !allocation
                .paths
                .iter()
                .any(|path| path.task_ids.contains(&"c".to_string()))
        );
        let mut statuses = HashMap::new();
        statuses.insert("a".to_string(), "completed".to_string());
        statuses.insert("b".to_string(), "completed".to_string());
        statuses.insert("c".to_string(), "cancelled".to_string());
        let allocation = run(
            &plan,
            &statuses,
            &multi_policy(),
            Some(10_000),
            &HashMap::new(),
        );
        assert_eq!(allocation.paths.len(), 1);
        assert_eq!(allocation.paths[0].task_ids, vec!["d"]);
    }

    #[test]
    fn diamond_paths_are_enumerated_in_lexicographic_order() {
        let plan = plan(vec![
            task("start", "impl", &[], 1_000),
            task("left", "impl", &["start"], 2_000),
            task("right", "qa", &["start"], 3_000),
            task("end", "qa", &["left", "right"], 1_000),
        ]);
        let (paths, enumerated, truncated) = enumerate_paths(&plan, &HashMap::new());
        assert_eq!(enumerated, 2);
        assert!(!truncated);
        assert_eq!(
            paths,
            vec![
                vec!["start".to_string(), "left".to_string(), "end".to_string()],
                vec!["start".to_string(), "right".to_string(), "end".to_string()],
            ]
        );
        let allocation = run(
            &plan,
            &HashMap::new(),
            &multi_policy(),
            Some(20_000),
            &HashMap::new(),
        );
        assert_eq!(allocation.paths.len(), 2);
        assert_eq!(allocation.paths[0].task_ids[1], "right");
        assert_eq!(allocation.paths[1].task_ids[1], "left");
        assert_eq!(allocation.paths[0].departments, vec!["impl", "qa"]);
    }

    #[test]
    fn declaration_order_does_not_move_the_multi_path_hash() {
        let first = plan(vec![
            task("a", "impl", &[], 3_000),
            task("b", "qa", &[], 2_000),
        ]);
        let second = plan(vec![
            task("b", "qa", &[], 2_000),
            task("a", "impl", &[], 3_000),
        ]);
        let left = run(
            &first,
            &HashMap::new(),
            &multi_policy(),
            Some(10_000),
            &HashMap::new(),
        );
        let right = run(
            &second,
            &HashMap::new(),
            &multi_policy(),
            Some(10_000),
            &HashMap::new(),
        );
        assert_eq!(left.inputs_hash, right.inputs_hash);
        assert_eq!(left.paths, right.paths);
        assert_eq!(left.departments, right.departments);
    }

    #[test]
    fn fairness_policy_fields_move_the_input_hash() {
        let plan = plan(vec![
            task("a", "impl", &[], 3_000),
            task("b", "qa", &[], 2_000),
        ]);
        let base = run(
            &plan,
            &HashMap::new(),
            &multi_policy(),
            Some(10_000),
            &HashMap::new(),
        );
        let mut changed = multi_policy();
        changed.department_min_ratio = 0.5;
        let departments = run(
            &plan,
            &HashMap::new(),
            &changed,
            Some(10_000),
            &HashMap::new(),
        );
        assert_ne!(base.inputs_hash, departments.inputs_hash);
        changed = multi_policy();
        changed.observed_weight_ratio = 0.5;
        let observed = run(
            &plan,
            &HashMap::new(),
            &changed,
            Some(10_000),
            &HashMap::new(),
        );
        assert_ne!(base.inputs_hash, observed.inputs_hash);
        changed = multi_policy();
        changed.max_paths = 2;
        let paths = run(
            &plan,
            &HashMap::new(),
            &changed,
            Some(10_000),
            &HashMap::new(),
        );
        assert_ne!(base.inputs_hash, paths.inputs_hash);
        changed = multi_policy();
        changed.starvation_after_secs = 60;
        let starved = run(
            &plan,
            &HashMap::new(),
            &changed,
            Some(10_000),
            &HashMap::new(),
        );
        assert_ne!(base.inputs_hash, starved.inputs_hash);
        changed = multi_policy();
        changed.reserve_ratio = 0.5;
        let reserve = run(
            &plan,
            &HashMap::new(),
            &changed,
            Some(10_000),
            &HashMap::new(),
        );
        assert_ne!(base.inputs_hash, reserve.inputs_hash);
    }

    #[test]
    fn max_paths_ties_use_the_lexicographic_task_sequence() {
        let plan = plan(vec![
            task("c", "impl", &[], 1_000),
            task("a", "impl", &[], 1_000),
            task("b", "impl", &[], 1_000),
        ]);
        let mut policy = multi_policy();
        policy.max_paths = 2;
        let allocation = run(
            &plan,
            &HashMap::new(),
            &policy,
            Some(10_000),
            &HashMap::new(),
        );
        assert_eq!(allocation.enumerated_paths, 3);
        assert_eq!(allocation.paths.len(), 2);
        assert_eq!(allocation.paths[0].task_ids, vec!["a"]);
        assert_eq!(allocation.paths[1].task_ids, vec!["b"]);
        assert_eq!(allocation.departments.len(), 1);
        assert_eq!(allocation.departments[0].path_ids, vec![0, 1]);
        assert_eq!(allocation.departments[0].weight_tokens, 2_000);
    }

    #[test]
    fn observed_spend_for_unknown_tasks_is_ignored() {
        let plan = plan(vec![task("known", "impl", &[], 2_000)]);
        let mut observed = HashMap::new();
        observed.insert("known".to_string(), 1_000u64);
        observed.insert("ghost".to_string(), 99_000u64);
        let mut policy = multi_policy();
        policy.observed_weight_ratio = 1.0;
        let allocation = run(&plan, &HashMap::new(), &policy, Some(10_000), &observed);
        assert_eq!(allocation.paths[0].observed_tokens, 1_000);
        assert_eq!(allocation.paths[0].weight_tokens, 1_000);
        assert_eq!(allocation.paths[0].declared_weight_tokens, 2_000);
    }

    #[test]
    fn path_weights_are_capped_for_extreme_declared_budgets() {
        let mut tasks = Vec::new();
        let mut previous: Option<String> = None;
        for index in 0..10 {
            let id = format!("t{index}");
            let mut entry = task(&id, "impl", &[], MAX_PATH_WEIGHT_TOKENS / 4);
            if let Some(previous) = &previous {
                entry.deps = vec![previous.clone()];
            }
            previous = Some(id);
            tasks.push(entry);
        }
        let allocation = run(
            &plan(tasks),
            &HashMap::new(),
            &multi_policy(),
            Some(1_000),
            &HashMap::new(),
        );
        assert_eq!(
            allocation.paths[0].declared_weight_tokens,
            MAX_PATH_WEIGHT_TOKENS
        );
        assert_eq!(allocation.paths[0].weight_tokens, MAX_PATH_WEIGHT_TOKENS);
        assert_eq!(
            allocation.paths[0].deficit_tokens,
            MAX_PATH_WEIGHT_TOKENS - 1_000
        );
    }

    #[test]
    fn shared_prefix_tasks_report_every_path_id() {
        let plan = plan(vec![
            task("root", "impl", &[], 1_000),
            task("a", "impl", &["root"], 2_000),
            task("b", "qa", &["root"], 3_000),
        ]);
        let allocation = run(
            &plan,
            &HashMap::new(),
            &multi_policy(),
            Some(10_000),
            &HashMap::new(),
        );
        assert_eq!(allocation.task_path_ids("root"), vec![0, 1]);
        assert_eq!(allocation.task_path_id("root"), Some(0));
        assert_eq!(allocation.task_path_ids("a"), vec![1]);
        assert_eq!(allocation.task_path_ids("missing"), Vec::<u32>::new());
    }

    #[test]
    fn department_rows_are_bounded_for_a_wide_single_path() {
        let mut tasks = Vec::new();
        let mut previous: Option<String> = None;
        for index in 0..40 {
            let id = format!("t{index}");
            let department = format!("dept-{index:02}");
            let mut entry = task(&id, &department, &[], 1_000);
            if let Some(previous) = &previous {
                entry.deps = vec![previous.clone()];
            }
            previous = Some(id);
            tasks.push(entry);
        }
        let mut policy = multi_policy();
        policy.department_min_ratio = 0.5;
        let allocation = run(
            &plan(tasks),
            &HashMap::new(),
            &policy,
            Some(100_000),
            &HashMap::new(),
        );
        assert_eq!(allocation.departments.len(), MAX_DEPARTMENT_ROWS);
        // Sorted by weight (all equal here) then department name.
        assert_eq!(allocation.departments[0].department, "dept-00");
        assert_eq!(allocation.departments[31].department, "dept-31");
        assert!(
            !allocation
                .departments
                .iter()
                .any(|row| row.department == "dept-39")
        );
    }

    #[test]
    fn starvation_credit_ages_after_the_threshold() {
        assert_eq!(
            starvation_credit("2026-11-12T00:00:00Z", "2026-11-12T00:10:00Z", 900),
            0
        );
        assert_eq!(
            starvation_credit("2026-11-12T00:00:00Z", "2026-11-12T00:15:00Z", 900),
            STARVATION_PRIORITY_PER_INTERVAL
        );
        assert_eq!(
            starvation_credit("2026-11-12T00:00:00Z", "2026-11-12T01:00:00Z", 900),
            20
        );
        assert_eq!(
            starvation_credit("2026-11-12T00:00:00Z", "2026-12-12T00:00:00Z", 900),
            MAX_STARVATION_CREDIT
        );
        assert_eq!(
            starvation_credit("2026-11-12T00:00:00Z", "2026-12-12T00:00:00Z", 0),
            0
        );
        assert_eq!(
            starvation_credit("not-a-time", "2026-11-12T00:00:00Z", 900),
            0
        );
        assert_eq!(
            starvation_credit("2026-11-13T00:00:00Z", "2026-11-12T00:00:00Z", 900),
            0
        );
    }

    #[test]
    fn membership_deficit_and_path_lookup_are_deterministic() {
        let plan = plan(vec![
            task("root", "impl", &[], 1_000),
            task("a", "impl", &["root"], 5_000),
            task("b", "qa", &["root"], 1_000),
        ]);
        let mut policy = multi_policy();
        policy.reserve_ratio = 0.0;
        let allocation = run(
            &plan,
            &HashMap::new(),
            &policy,
            Some(4_000),
            &HashMap::new(),
        );
        assert_eq!(allocation.paths.len(), 2);
        // root is shared by both paths and inherits the largest deficit.
        assert!(allocation.critical_task_ids().contains("root"));
        assert_eq!(allocation.task_path_id("a"), Some(0));
        assert_eq!(allocation.task_path_id("b"), Some(1));
        assert_eq!(
            allocation.task_deficit("root"),
            allocation.paths[0].deficit_tokens
        );
        assert!(allocation.most_starved_path().is_some());
        assert!(allocation.detail().contains("paths"));
    }

    #[test]
    fn enumeration_cap_is_reported() {
        // A diamond ladder produces 2^depth maximal paths. Sixteen levels is
        // 65,536 paths, so the cap must truncate deterministically.
        let mut tasks = vec![task("start", "impl", &[], 1_000)];
        let mut previous = vec!["start".to_string()];
        for level in 0..16 {
            let mut next = Vec::new();
            for branch in 0..2 {
                let left = format!("l{level}-{branch}");
                let right = format!("r{level}-{branch}");
                for id in [&left, &right] {
                    let deps = previous.iter().map(String::as_str).collect::<Vec<_>>();
                    tasks.push(task(id, "impl", &deps, 1_000));
                    next.push(id.clone());
                }
            }
            previous = next;
        }
        // 33 nodes and ~544 edges stay under the F5 DAG bounds, but the
        // diamond ladder has 2^16 maximal paths, so enumeration must hit the
        // path cap and report truncation instead of exploding.
        let generated = plan(tasks);
        assert!(generated.tasks.len() < MAX_PATH_TASKS);
        let (paths, enumerated, truncated) = enumerate_paths(&generated, &HashMap::new());
        assert_eq!(paths.len(), MAX_ENUMERATED_PATHS);
        assert_eq!(enumerated, MAX_ENUMERATED_PATHS);
        assert!(truncated);
        let (again, _, again_truncated) = enumerate_paths(&generated, &HashMap::new());
        assert_eq!(paths, again);
        assert_eq!(truncated, again_truncated);
    }

    #[test]
    fn all_cancelled_plan_has_no_remaining_paths() {
        let plan = plan(vec![
            task("a", "impl", &[], 5_000),
            task("b", "impl", &["a"], 5_000),
        ]);
        let mut statuses = HashMap::new();
        statuses.insert("a".to_string(), "cancelled".to_string());
        statuses.insert("b".to_string(), "cancelled".to_string());
        let allocation = run(
            &plan,
            &statuses,
            &multi_policy(),
            Some(10_000),
            &HashMap::new(),
        );
        assert!(allocation.complete);
        assert!(allocation.paths.is_empty());
        assert!(allocation.departments.is_empty());
        assert_eq!(allocation.protected_tokens, 0);
        assert_eq!(allocation.allocated_tokens, 0);
    }

    #[test]
    fn running_and_paused_tasks_remain_in_the_protected_graph() {
        let plan = plan(vec![
            task("running", "impl", &[], 3_000),
            task("paused", "qa", &["running"], 2_000),
            task("failed", "qa", &[], 1_000),
        ]);
        let mut statuses = HashMap::new();
        statuses.insert("running".to_string(), "running".to_string());
        statuses.insert("paused".to_string(), "paused".to_string());
        statuses.insert("failed".to_string(), "failed".to_string());
        let allocation = run(
            &plan,
            &statuses,
            &multi_policy(),
            Some(10_000),
            &HashMap::new(),
        );
        assert_eq!(allocation.paths.len(), 2);
        assert!(
            allocation
                .paths
                .iter()
                .any(|path| path.task_ids == vec!["running", "paused"])
        );
        assert!(
            allocation
                .paths
                .iter()
                .any(|path| path.task_ids == vec!["failed"])
        );
    }

    #[test]
    fn max_paths_is_clamped_to_the_protocol_minimum() {
        let plan = plan(vec![
            task("a", "impl", &[], 3_000),
            task("b", "impl", &[], 2_000),
            task("c", "impl", &[], 1_000),
        ]);
        let mut policy = multi_policy();
        policy.max_paths = 0;
        let normalized = policy.clone().normalized();
        assert_eq!(normalized.max_paths, MIN_SELECTED_PATHS as u32);
        let allocation = run(
            &plan,
            &HashMap::new(),
            &normalized,
            Some(10_000),
            &HashMap::new(),
        );
        assert_eq!(allocation.paths.len(), 2);
        assert_eq!(allocation.paths[0].task_ids, vec!["a"]);
        assert_eq!(allocation.paths[1].task_ids, vec!["b"]);
    }

    #[test]
    fn remaining_budget_moves_the_multi_path_hash() {
        let plan = plan(vec![
            task("a", "impl", &[], 3_000),
            task("b", "impl", &[], 2_000),
        ]);
        let first = run(
            &plan,
            &HashMap::new(),
            &multi_policy(),
            Some(10_000),
            &HashMap::new(),
        );
        let second = run(
            &plan,
            &HashMap::new(),
            &multi_policy(),
            Some(11_000),
            &HashMap::new(),
        );
        assert_ne!(first.inputs_hash, second.inputs_hash);
        assert_eq!(first.paths, second.paths);
        let unbounded = run(
            &plan,
            &HashMap::new(),
            &multi_policy(),
            None,
            &HashMap::new(),
        );
        assert_ne!(first.inputs_hash, unbounded.inputs_hash);
    }

    #[test]
    fn observed_spend_map_moves_the_multi_path_hash() {
        let plan = plan(vec![task("a", "impl", &[], 3_000)]);
        let mut policy = multi_policy();
        policy.observed_weight_ratio = 0.5;
        let first = run(
            &plan,
            &HashMap::new(),
            &policy,
            Some(10_000),
            &HashMap::new(),
        );
        let mut observed = HashMap::new();
        observed.insert("a".to_string(), 1_000u64);
        let second = run(&plan, &HashMap::new(), &policy, Some(10_000), &observed);
        assert_ne!(first.inputs_hash, second.inputs_hash);
        assert_eq!(first.paths[0].weight_tokens, 3_000);
        assert_eq!(second.paths[0].weight_tokens, 2_000);
    }

    #[test]
    fn department_floor_scaling_respects_the_pool_across_ratios() {
        let plan = plan(vec![
            task("a", "impl", &[], 9_000),
            task("b", "qa", &[], 600),
            task("c", "ops", &[], 400),
        ]);
        for ratio in [0.0, 0.1, 0.5, 1.0] {
            for pool in [1u64, 100, 1_000, 10_000, 100_000] {
                let mut policy = multi_policy();
                policy.reserve_ratio = 0.0;
                policy.department_min_ratio = ratio;
                let allocation = run(&plan, &HashMap::new(), &policy, Some(pool), &HashMap::new());
                assert!(
                    allocation.floor_tokens <= allocation.protected_tokens,
                    "ratio {ratio} pool {pool} floors {} protected {}",
                    allocation.floor_tokens,
                    allocation.protected_tokens
                );
                assert_eq!(
                    allocation.allocated_tokens,
                    allocation
                        .protected_tokens
                        .min(allocation.path_weight_tokens)
                );
                for path in &allocation.paths {
                    assert!(path.floor_tokens <= path.weight_tokens);
                    assert!(path.allocation_tokens >= path.floor_tokens);
                    assert!(path.allocation_tokens <= path.weight_tokens);
                }
            }
        }
    }

    #[test]
    fn department_ratio_one_shared_prefix_counts_each_path_once() {
        let plan = plan(vec![
            task("root", "impl", &[], 1_000),
            task("a", "impl", &["root"], 4_000),
            task("b", "qa", &["root"], 2_000),
        ]);
        let mut policy = multi_policy();
        policy.reserve_ratio = 0.0;
        policy.department_min_ratio = 1.0;
        let allocation = run(
            &plan,
            &HashMap::new(),
            &policy,
            Some(8_000),
            &HashMap::new(),
        );
        // The pool equals total path demand. Every path is fully funded, so
        // observed floors cannot exceed demand even at ratio 1.0.
        assert_eq!(allocation.allocated_tokens, 8_000);
        assert!(allocation.floor_tokens <= allocation.allocated_tokens);
        assert!(allocation.paths.iter().all(|path| path.deficit_tokens == 0));
        assert_eq!(allocation.departments[0].department, "impl");
        assert_eq!(allocation.departments[0].path_ids, vec![0, 1]);
    }

    #[test]
    fn detail_is_bounded_for_a_sixteen_path_table() {
        let mut tasks = Vec::new();
        for index in 0..20 {
            tasks.push(task(&format!("t{index:02}"), "impl", &[], 1_000 + index));
        }
        let mut policy = multi_policy();
        policy.max_paths = 16;
        let allocation = run(
            &plan(tasks),
            &HashMap::new(),
            &policy,
            Some(100_000),
            &HashMap::new(),
        );
        assert_eq!(allocation.paths.len(), 16);
        let detail = allocation.detail();
        assert!(detail.contains("16 paths"));
        assert!(detail.contains("+13 more"));
        assert!(detail.chars().count() < 1_000);
    }

    #[test]
    fn oversized_generation_still_fails_closed_in_the_public_entry_point() {
        let mut tasks = Vec::new();
        for index in 0..=MAX_PATH_TASKS {
            tasks.push(task(&format!("t{index}"), "impl", &[], 1_000));
        }
        let error = compute_multi_path(MultiPathInput {
            session_id: "s1",
            project_id: "p1",
            plan_revision: 1,
            plan: &plan(tasks),
            statuses: &HashMap::new(),
            remaining_budget_tokens: Some(1_000),
            policy: &multi_policy(),
            observed_tokens: &HashMap::new(),
        })
        .unwrap_err();
        assert_eq!(error.code, "BUDGET_PATH_TOO_LARGE");
    }

    #[test]
    fn edges_are_still_bounded() {
        let mut tasks = vec![task("t0", "impl", &[], 1_000)];
        for index in 1..MAX_PATH_TASKS {
            let deps = (0..index).map(|dep| format!("t{dep}")).collect::<Vec<_>>();
            let mut entry = task(&format!("t{index}"), "impl", &[], 1_000);
            entry.deps = deps;
            tasks.push(entry);
        }
        let error = compute_multi_path(MultiPathInput {
            session_id: "s1",
            project_id: "p1",
            plan_revision: 1,
            plan: &plan(tasks),
            statuses: &HashMap::new(),
            remaining_budget_tokens: Some(1_000),
            policy: &multi_policy(),
            observed_tokens: &HashMap::new(),
        })
        .unwrap_err();
        assert_eq!(error.code, "BUDGET_PATH_TOO_LARGE");
    }
}

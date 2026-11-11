//! Deterministic budget admission for plan tasks.
//!
//! The governor is a pure decision layer on top of the existing hard-budget
//! registry. It never increases a limit, never changes provider/model routing,
//! and never writes usage. It only decides whether a *new* request may start
//! now:
//!
//! * the critical path's remaining weight plus a configurable ratio of the
//!   remaining session budget is protected from non-critical work;
//! * inside the throttle zone (or when the free headroom is smaller than the
//!   request), a non-critical request is deferred with a typed reason;
//! * critical-path tasks are admitted first under pressure and bypass the
//!   reserve pool, but still obey the session/project/department hard budgets;
//! * a completed path, an unbounded session, or a disabled policy changes
//!   nothing.
//!
//! Decisions are event-backed by `budget.governor.decision`; preview mode
//! evaluates the same pure function with an optional remaining-token override
//! and writes nothing.

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};
use vp_protocol::types::{
    BudgetGovernorDecision, CriticalPathInfo, CriticalPathMode, CriticalPathPolicy, Event,
    GovernorDeferralInfo, GovernorFailureInfo, GovernorReserveInfo, GovernorTaskDecisionInfo,
    PlanTask, TeamPlan,
};

use crate::budget::GovernorGate;
use crate::critical_path::{
    CriticalPath, CriticalPathError, CriticalPathInput, compute_critical_path, task_weight_tokens,
};
use crate::multi_path::{
    MultiPathAllocation, MultiPathInput, compute_multi_path, starvation_credit,
};

pub const DECISION_EVENT_KIND: &str = "budget.governor.decision";
pub const MAX_DECISION_HISTORY: usize = 100;
pub const MAX_PREVIEW_DECISIONS: usize = 100;

/// Remaining-budget view for one evaluation. The hard limits stay in
/// `SessionBudget`/`BudgetRegistry`; this is read-only input math.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GovernorBudgetView {
    pub limit_tokens: Option<u64>,
    pub spent_tokens: u64,
    pub reserved_tokens: u64,
}

impl GovernorBudgetView {
    pub fn new(limit_tokens: Option<u64>, spent_tokens: u64, reserved_tokens: u64) -> Self {
        Self {
            limit_tokens,
            spent_tokens,
            reserved_tokens,
        }
    }

    pub fn remaining_tokens(&self) -> Option<u64> {
        self.limit_tokens.map(|limit| {
            limit
                .saturating_sub(self.spent_tokens)
                .saturating_sub(self.reserved_tokens)
        })
    }
}

/// One ready task candidate. `effective_priority` is the scheduler's existing
/// aging/retry-aware priority; the governor only adds a critical boost.
pub struct GovernorCandidate<'a> {
    pub task: &'a PlanTask,
    pub effective_priority: i64,
    pub enqueued_at: &'a str,
}

/// Stable reserve explanation. No field depends on the clock.
#[derive(Debug, Clone, PartialEq)]
pub struct GovernorReserve {
    /// `disabled`, `no_budget`, `complete`, `exhausted`, `throttled`, `active`.
    pub state: String,
    pub reserve_ratio: f64,
    pub throttle_at_ratio: f64,
    pub critical_boost: i32,
    pub min_task_budget: u64,
    pub limit_tokens: Option<u64>,
    pub spent_tokens: u64,
    pub reserved_tokens: u64,
    pub remaining_tokens: Option<u64>,
    pub path_weight_tokens: u64,
    pub ratio_reserve_tokens: u64,
    pub protected_tokens: u64,
    pub headroom_tokens: Option<u64>,
    pub throttled: bool,
    /// `single` or `multi`.
    pub mode: CriticalPathMode,
    /// Protected path count (0 or 1 in single mode).
    pub path_count: usize,
    /// Sum of path allocations inside `protected_tokens`.
    pub allocated_tokens: u64,
    /// Sum of per-department/path floors inside the allocations.
    pub floor_tokens: u64,
    /// Maximal paths enumerated before selection.
    pub enumerated_paths: usize,
    /// Enumeration was truncated at the bound.
    pub paths_truncated: bool,
}

impl GovernorReserve {
    pub fn info(&self) -> GovernorReserveInfo {
        GovernorReserveInfo {
            state: self.state.clone(),
            reserve_ratio: self.reserve_ratio,
            throttle_at_ratio: self.throttle_at_ratio,
            critical_boost: self.critical_boost,
            min_task_budget: self.min_task_budget,
            limit_tokens: self.limit_tokens,
            spent_tokens: self.spent_tokens,
            reserved_tokens: self.reserved_tokens,
            remaining_tokens: self.remaining_tokens,
            path_weight_tokens: self.path_weight_tokens,
            ratio_reserve_tokens: self.ratio_reserve_tokens,
            protected_tokens: self.protected_tokens,
            headroom_tokens: self.headroom_tokens,
            throttled: self.throttled,
            mode: self.mode.as_str().to_string(),
            path_count: self.path_count.min(u32::MAX as usize) as u32,
            allocated_tokens: self.allocated_tokens,
            floor_tokens: self.floor_tokens,
            enumerated_paths: self.enumerated_paths.min(u32::MAX as usize) as u32,
            paths_truncated: self.paths_truncated,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GovernorReason {
    CriticalAdmit,
    Admit,
    Reserve,
    Throttle,
    Exhausted,
    Disabled,
    NoBudget,
    PathComplete,
}

impl GovernorReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CriticalAdmit => "critical_admit",
            Self::Admit => "admit",
            Self::Reserve => "reserve",
            Self::Throttle => "throttle",
            Self::Exhausted => "exhausted",
            Self::Disabled => "disabled",
            Self::NoBudget => "no_budget",
            Self::PathComplete => "path_complete",
        }
    }

    /// Stable Q3 error-contract code for a deferral. Admission reasons have no
    /// error code.
    pub fn code(self) -> &'static str {
        match self {
            Self::Reserve => "BUDGET_RESERVED",
            Self::Throttle => "BUDGET_THROTTLED",
            Self::Exhausted => "BUDGET_EXHAUSTED",
            _ => "",
        }
    }

    pub fn is_deferral(self) -> bool {
        matches!(self, Self::Reserve | Self::Throttle | Self::Exhausted)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GovernorTaskDecision {
    pub task_id: String,
    pub department: String,
    pub critical: bool,
    pub weight_tokens: u64,
    pub order: u32,
    pub admitted: bool,
    pub reason: GovernorReason,
    pub remaining_after_tokens: Option<u64>,
    pub path_id: Option<u32>,
    pub path_ids: Vec<u32>,
    pub path_deficit_tokens: u64,
    pub starvation_credit: i64,
}

impl GovernorTaskDecision {
    pub fn info(&self) -> GovernorTaskDecisionInfo {
        GovernorTaskDecisionInfo {
            task_id: self.task_id.clone(),
            department: self.department.clone(),
            critical: self.critical,
            weight_tokens: self.weight_tokens,
            order: self.order,
            action: if self.admitted { "admit" } else { "defer" }.to_string(),
            reason: self.reason.as_str().to_string(),
            code: self.reason.code().to_string(),
            remaining_after_tokens: self.remaining_after_tokens,
            path_id: self.path_id,
            path_ids: self.path_ids.clone(),
            path_deficit_tokens: self.path_deficit_tokens,
            starvation_credit: self.starvation_credit,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernorDeferral {
    pub task_id: String,
    pub department: String,
    pub critical: bool,
    pub weight_tokens: u64,
    pub reason: GovernorReason,
    pub remaining_tokens: Option<u64>,
    pub protected_tokens: u64,
    pub path_id: Option<u32>,
    pub path_ids: Vec<u32>,
    pub path_deficit_tokens: u64,
    pub starvation_credit: i64,
}

impl GovernorDeferral {
    pub fn info(&self) -> GovernorDeferralInfo {
        GovernorDeferralInfo {
            task_id: self.task_id.clone(),
            department: self.department.clone(),
            critical: self.critical,
            weight_tokens: self.weight_tokens,
            reason: self.reason.as_str().to_string(),
            code: self.reason.code().to_string(),
            remaining_tokens: self.remaining_tokens,
            protected_tokens: self.protected_tokens,
            path_id: self.path_id,
            path_ids: self.path_ids.clone(),
            path_deficit_tokens: self.path_deficit_tokens,
            starvation_credit: self.starvation_credit,
        }
    }
}

/// Complete deterministic result of one evaluation.
#[derive(Debug, Clone, PartialEq)]
pub struct GovernorAdmission {
    pub enabled: bool,
    pub mode: CriticalPathMode,
    pub path: CriticalPath,
    /// Multi-path table when `mode = multi`; `None` in single mode.
    pub multi: Option<MultiPathAllocation>,
    pub reserve: GovernorReserve,
    pub decisions: Vec<GovernorTaskDecision>,
    pub admitted: Vec<String>,
    pub deferred: Vec<GovernorDeferral>,
    pub inputs_hash: String,
}

impl GovernorAdmission {
    /// A bounded one-line explanation for events and CLI output.
    pub fn detail(&self) -> String {
        if let Some(multi) = &self.multi {
            return multi.detail();
        }
        let path = if self.path.task_ids.is_empty() {
            "none".to_string()
        } else {
            self.path.task_ids.join(" -> ")
        };
        let remaining = self
            .reserve
            .remaining_tokens
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unbounded".to_string());
        format!(
            "path [{path}] weight {} · protected {} of {} remaining · admitted {} · deferred {}",
            self.reserve.path_weight_tokens,
            self.reserve.protected_tokens,
            remaining,
            self.admitted.len(),
            self.deferred.len()
        )
    }

    pub fn path_infos(&self) -> Vec<vp_protocol::types::GovernorPathInfo> {
        match &self.multi {
            Some(multi) => multi.path_infos(),
            None if !self.path.task_ids.is_empty() => {
                vec![vp_protocol::types::GovernorPathInfo {
                    path_id: 0,
                    task_ids: self.path.task_ids.clone(),
                    departments: Vec::new(),
                    weight_tokens: self.path.weight_tokens,
                    declared_weight_tokens: self.path.weight_tokens,
                    observed_tokens: 0,
                    floor_tokens: 0,
                    allocation_tokens: self.reserve.allocated_tokens,
                    deficit_tokens: self
                        .path
                        .weight_tokens
                        .saturating_sub(self.reserve.allocated_tokens),
                }]
            }
            None => Vec::new(),
        }
    }

    pub fn department_infos(&self) -> Vec<vp_protocol::types::GovernorDepartmentInfo> {
        self.multi
            .as_ref()
            .map(MultiPathAllocation::department_infos)
            .unwrap_or_default()
    }

    /// Deterministic deferred task ids in decision order.
    pub fn deferred_order(&self) -> Vec<String> {
        self.deferred
            .iter()
            .map(|deferral| deferral.task_id.clone())
            .collect()
    }

    pub fn complete(&self) -> bool {
        self.multi
            .as_ref()
            .map(|multi| multi.complete)
            .unwrap_or(self.path.complete)
    }

    pub fn reason(&self) -> GovernorReason {
        if let Some(deferral) = self.deferred.first() {
            return deferral.reason;
        }
        if !self.enabled {
            return GovernorReason::Disabled;
        }
        if self.reserve.remaining_tokens.is_none() {
            return GovernorReason::NoBudget;
        }
        if self.complete() || self.reserve.path_weight_tokens == 0 {
            return GovernorReason::PathComplete;
        }
        GovernorReason::Admit
    }
}

/// Typed failure using the bounded Q3 error contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernorFailure {
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
    pub hint: &'static str,
}

impl GovernorFailure {
    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn info(&self) -> GovernorFailureInfo {
        GovernorFailureInfo {
            code: self.code.to_string(),
            message: self.message.clone(),
            retryable: self.retryable,
            hint: self.hint.to_string(),
        }
    }
}

impl std::fmt::Display for GovernorFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for GovernorFailure {}

impl From<CriticalPathError> for GovernorFailure {
    fn from(error: CriticalPathError) -> Self {
        Self {
            code: error.code,
            message: error.message,
            retryable: false,
            hint: error.hint,
        }
    }
}

/// Inputs to one pure governor evaluation.
pub struct GovernorEvaluation<'a> {
    pub session_id: &'a str,
    pub project_id: &'a str,
    pub plan_revision: u32,
    pub plan: &'a TeamPlan,
    pub statuses: &'a HashMap<String, String>,
    pub policy: &'a CriticalPathPolicy,
    pub candidates: &'a [GovernorCandidate<'a>],
    pub budget: GovernorBudgetView,
    /// Preview-only override. `Some` replaces the ledger-derived remaining
    /// tokens so a caller can ask "what would happen with N tokens left?".
    pub remaining_override: Option<u64>,
    /// Observed input+output tokens per task from the usage ledger. Only used
    /// by `mode = multi` with a positive `observed_weight_ratio`.
    pub observed_tokens: &'a HashMap<String, u64>,
    /// Deterministic evaluation instant for starvation aging. The pure
    /// evaluator never reads the clock itself.
    pub evaluated_at: &'a str,
}

/// Evaluate the plan and ordered candidates. This function performs no I/O and
/// does not mutate any state.
pub fn evaluate(input: GovernorEvaluation<'_>) -> Result<GovernorAdmission, GovernorFailure> {
    let policy = input.policy.clone().normalized();
    let remaining = input
        .remaining_override
        .or_else(|| input.budget.remaining_tokens());
    let path = compute_critical_path(CriticalPathInput {
        session_id: input.session_id,
        project_id: input.project_id,
        plan_revision: input.plan_revision,
        plan: input.plan,
        statuses: input.statuses,
        remaining_budget_tokens: remaining,
    })?;
    // Multi mode computes the bounded path table and its water-filled
    // allocation. Single mode leaves `multi = None` and every later branch
    // keeps the original F5 ordering and reserve math.
    let multi = if policy.mode == CriticalPathMode::Multi {
        Some(compute_multi_path(MultiPathInput {
            session_id: input.session_id,
            project_id: input.project_id,
            plan_revision: input.plan_revision,
            plan: input.plan,
            statuses: input.statuses,
            remaining_budget_tokens: remaining,
            policy: &policy,
            observed_tokens: input.observed_tokens,
        })?)
    } else {
        None
    };
    let mut reserve = reserve_math(
        &policy,
        input.budget,
        remaining,
        multi
            .as_ref()
            .map(|multi| multi.path_weight_tokens)
            .unwrap_or(path.weight_tokens),
        multi
            .as_ref()
            .map(|multi| multi.complete)
            .unwrap_or(path.complete),
    );
    if let Some(multi) = multi.as_ref() {
        reserve.mode = CriticalPathMode::Multi;
        reserve.path_count = multi.paths.len();
        reserve.allocated_tokens = multi.allocated_tokens;
        reserve.floor_tokens = multi.floor_tokens;
        reserve.enumerated_paths = multi.enumerated_paths;
        reserve.paths_truncated = multi.truncated;
        reserve.ratio_reserve_tokens = multi.ratio_reserve_tokens;
        reserve.protected_tokens = multi.protected_tokens;
        reserve.headroom_tokens =
            remaining.map(|remaining| remaining.saturating_sub(multi.protected_tokens));
    } else {
        reserve.mode = CriticalPathMode::Single;
        reserve.path_count = usize::from(!path.task_ids.is_empty());
        reserve.allocated_tokens = path.weight_tokens.min(reserve.protected_tokens);
        reserve.floor_tokens = 0;
        reserve.enumerated_paths = reserve.path_count;
        reserve.paths_truncated = false;
    }
    let path_ids = multi
        .as_ref()
        .map(|multi| {
            multi
                .critical_task_ids()
                .into_iter()
                .map(str::to_string)
                .collect::<HashSet<String>>()
        })
        .unwrap_or_else(|| path.task_ids.iter().cloned().collect::<HashSet<String>>());
    let inputs_hash = multi
        .as_ref()
        .map(|multi| multi.inputs_hash.clone())
        .unwrap_or_else(|| admission_hash(&policy, &input, &path, &reserve));
    let complete = multi
        .as_ref()
        .map(|multi| multi.complete)
        .unwrap_or(path.complete);
    let total_path_weight = multi
        .as_ref()
        .map(|multi| multi.path_weight_tokens)
        .unwrap_or(path.weight_tokens);

    if !policy.enabled {
        let decisions = input
            .candidates
            .iter()
            .enumerate()
            .map(|(order, candidate)| {
                let critical = path_ids.contains(&candidate.task.id);
                GovernorTaskDecision {
                    task_id: candidate.task.id.clone(),
                    department: candidate.task.department.clone(),
                    critical,
                    weight_tokens: task_weight_tokens(candidate.task),
                    order: order.min(u32::MAX as usize) as u32,
                    admitted: true,
                    reason: GovernorReason::Disabled,
                    remaining_after_tokens: remaining,
                    path_id: single_path_id(multi.as_ref(), &candidate.task.id, critical),
                    path_ids: single_path_ids(multi.as_ref(), &candidate.task.id, critical),
                    path_deficit_tokens: multi
                        .as_ref()
                        .map(|multi| multi.task_deficit(&candidate.task.id))
                        .unwrap_or(0),
                    starvation_credit: 0,
                }
            })
            .collect::<Vec<_>>();
        return Ok(GovernorAdmission {
            enabled: false,
            mode: policy.mode,
            path,
            multi,
            reserve,
            admitted: decisions
                .iter()
                .map(|decision| decision.task_id.clone())
                .collect(),
            decisions,
            deferred: Vec::new(),
            inputs_hash,
        });
    }

    let pressure = reserve.throttled
        || reserve
            .remaining_tokens
            .is_some_and(|remaining| remaining <= reserve.protected_tokens);
    let multi_mode = policy.mode == CriticalPathMode::Multi;
    let mut ordered = input
        .candidates
        .iter()
        .enumerate()
        .map(|(input_order, candidate)| {
            let critical = path_ids.contains(&candidate.task.id);
            let path_id = single_path_id(multi.as_ref(), &candidate.task.id, critical);
            let path_ids = single_path_ids(multi.as_ref(), &candidate.task.id, critical);
            let path_deficit_tokens = multi
                .as_ref()
                .map(|multi| multi.task_deficit(&candidate.task.id))
                .unwrap_or(0);
            // Single mode intentionally keeps starvation aging at zero so the
            // original F5 ordering is byte-for-byte identical.
            let starvation_credit = if multi_mode {
                starvation_credit(
                    candidate.enqueued_at,
                    input.evaluated_at,
                    policy.starvation_after_secs,
                )
            } else {
                0
            };
            let boosted = candidate.effective_priority
                + i64::from(if critical {
                    policy.critical_boost.max(0)
                } else {
                    0
                })
                + starvation_credit;
            (
                input_order,
                candidate,
                critical,
                path_id,
                path_deficit_tokens,
                starvation_credit,
                boosted,
                path_ids,
            )
        })
        .collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        let left_pressure = pressure && left.2;
        let right_pressure = pressure && right.2;
        right_pressure
            .cmp(&left_pressure)
            // Multi-path ordering is path deficit first, then priority/age.
            // Single mode keeps the original priority-first ordering.
            .then_with(|| {
                if multi_mode {
                    right.4.cmp(&left.4)
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .then_with(|| right.6.cmp(&left.6))
            .then_with(|| left.1.enqueued_at.cmp(right.1.enqueued_at))
            .then_with(|| left.1.task.id.cmp(&right.1.task.id))
    });
    ordered.truncate(MAX_PREVIEW_DECISIONS.min(MAX_DECISION_HISTORY));

    let mut current = remaining;
    let mut free_headroom =
        remaining.map(|remaining| remaining.saturating_sub(reserve.protected_tokens));
    let mut decisions = Vec::with_capacity(ordered.len());
    let mut admitted = Vec::new();
    let mut deferred = Vec::new();
    for (
        order,
        (_, candidate, critical, path_id, path_deficit_tokens, starvation_credit, _, path_ids),
    ) in ordered.into_iter().enumerate()
    {
        let task = candidate.task;
        let weight = task_weight_tokens(task);
        let order = order.min(u32::MAX as usize) as u32;
        let decision = if let Some(remaining_now) = current {
            if remaining_now < policy.min_task_budget {
                let decision = GovernorTaskDecision {
                    task_id: task.id.clone(),
                    department: task.department.clone(),
                    critical,
                    weight_tokens: weight,
                    order,
                    admitted: false,
                    reason: GovernorReason::Exhausted,
                    remaining_after_tokens: Some(remaining_now),
                    path_id,
                    path_ids: path_ids.clone(),
                    path_deficit_tokens,
                    starvation_credit,
                };
                deferred.push(GovernorDeferral {
                    task_id: task.id.clone(),
                    department: task.department.clone(),
                    critical,
                    weight_tokens: weight,
                    reason: GovernorReason::Exhausted,
                    remaining_tokens: Some(remaining_now),
                    protected_tokens: reserve.protected_tokens,
                    path_id,
                    path_ids: path_ids.clone(),
                    path_deficit_tokens,
                    starvation_credit,
                });
                decision
            } else if critical {
                // Critical admission consumes the protected pool. The free
                // headroom also shrinks if the critical request is heavier
                // than the protected amount, so total remaining can never be
                // oversubscribed by a later non-critical admission.
                current = Some(remaining_now.saturating_sub(weight));
                free_headroom = free_headroom.map(|free| free.min(current.unwrap_or(0)));
                GovernorTaskDecision {
                    task_id: task.id.clone(),
                    department: task.department.clone(),
                    critical,
                    weight_tokens: weight,
                    order,
                    admitted: true,
                    reason: GovernorReason::CriticalAdmit,
                    remaining_after_tokens: current,
                    path_id,
                    path_ids: path_ids.clone(),
                    path_deficit_tokens,
                    starvation_credit,
                }
            } else if reserve.throttled {
                let decision = GovernorTaskDecision {
                    task_id: task.id.clone(),
                    department: task.department.clone(),
                    critical,
                    weight_tokens: weight,
                    order,
                    admitted: false,
                    reason: GovernorReason::Throttle,
                    remaining_after_tokens: Some(remaining_now),
                    path_id,
                    path_ids: path_ids.clone(),
                    path_deficit_tokens,
                    starvation_credit,
                };
                deferred.push(GovernorDeferral {
                    task_id: task.id.clone(),
                    department: task.department.clone(),
                    critical,
                    weight_tokens: weight,
                    reason: GovernorReason::Throttle,
                    remaining_tokens: Some(remaining_now),
                    protected_tokens: reserve.protected_tokens,
                    path_id,
                    path_ids: path_ids.clone(),
                    path_deficit_tokens,
                    starvation_credit,
                });
                decision
            } else if weight > free_headroom.unwrap_or(u64::MAX) {
                let decision = GovernorTaskDecision {
                    task_id: task.id.clone(),
                    department: task.department.clone(),
                    critical,
                    weight_tokens: weight,
                    order,
                    admitted: false,
                    reason: GovernorReason::Reserve,
                    remaining_after_tokens: Some(remaining_now),
                    path_id,
                    path_ids: path_ids.clone(),
                    path_deficit_tokens,
                    starvation_credit,
                };
                deferred.push(GovernorDeferral {
                    task_id: task.id.clone(),
                    department: task.department.clone(),
                    critical,
                    weight_tokens: weight,
                    reason: GovernorReason::Reserve,
                    remaining_tokens: Some(remaining_now),
                    protected_tokens: reserve.protected_tokens,
                    path_id,
                    path_ids: path_ids.clone(),
                    path_deficit_tokens,
                    starvation_credit,
                });
                decision
            } else {
                current = Some(remaining_now.saturating_sub(weight));
                free_headroom = free_headroom.map(|free| free.saturating_sub(weight));
                GovernorTaskDecision {
                    task_id: task.id.clone(),
                    department: task.department.clone(),
                    critical,
                    weight_tokens: weight,
                    order,
                    admitted: true,
                    reason: GovernorReason::Admit,
                    remaining_after_tokens: current,
                    path_id,
                    path_ids: path_ids.clone(),
                    path_deficit_tokens,
                    starvation_credit,
                }
            }
        } else {
            GovernorTaskDecision {
                task_id: task.id.clone(),
                department: task.department.clone(),
                critical,
                weight_tokens: weight,
                order,
                admitted: true,
                reason: GovernorReason::NoBudget,
                remaining_after_tokens: None,
                path_id,
                path_ids: path_ids.clone(),
                path_deficit_tokens,
                starvation_credit,
            }
        };
        if decision.admitted {
            admitted.push(decision.task_id.clone());
        }
        decisions.push(decision);
    }

    // A plan with no remaining path has nothing to protect. Admit ready work
    // in deterministic order instead of keeping a ratio reserve forever.
    if complete || total_path_weight == 0 {
        for decision in &mut decisions {
            if !decision.admitted && decision.reason != GovernorReason::Exhausted {
                decision.admitted = true;
                decision.reason = GovernorReason::PathComplete;
            }
        }
        admitted = decisions
            .iter()
            .filter(|decision| decision.admitted)
            .map(|decision| decision.task_id.clone())
            .collect();
        // Keep only deferrals that are still deferred after the path released;
        // an exhausted floor is not silently admitted.
        deferred.retain(|deferral| {
            decisions
                .iter()
                .any(|decision| decision.task_id == deferral.task_id && !decision.admitted)
        });
    }

    Ok(GovernorAdmission {
        enabled: true,
        mode: policy.mode,
        path,
        multi,
        reserve,
        decisions,
        admitted,
        deferred,
        inputs_hash,
    })
}

/// Pure reserve math shared by scheduler admission, provider pre-flight, and
/// status/preview rendering.
pub fn reserve_math(
    policy: &CriticalPathPolicy,
    budget: GovernorBudgetView,
    remaining_tokens: Option<u64>,
    path_weight_tokens: u64,
    complete: bool,
) -> GovernorReserve {
    let policy = policy.clone().normalized();
    let remaining = remaining_tokens;
    let enabled = policy.enabled;
    let ratio_reserve_tokens = remaining
        .map(|remaining| ratio_ceil(remaining, policy.reserve_ratio))
        .unwrap_or(0);
    // A completed path protects nothing: a ratio reserve with no unfinished
    // critical work would starve the tail of the plan for no benefit.
    let protects = enabled && !complete && path_weight_tokens > 0;
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
    let headroom_tokens = remaining.map(|remaining| remaining.saturating_sub(protected_tokens));
    let throttled = protects
        && remaining.is_some_and(|remaining| {
            budget.limit_tokens.is_some_and(|limit| {
                limit > 0 && remaining <= ratio_floor(limit, policy.throttle_at_ratio)
            })
        });
    let state = if !enabled {
        "disabled"
    } else if remaining.is_none() {
        "no_budget"
    } else if complete || path_weight_tokens == 0 {
        "complete"
    } else if remaining.is_some_and(|remaining| remaining < policy.min_task_budget) {
        "exhausted"
    } else if throttled {
        "throttled"
    } else {
        "active"
    };
    GovernorReserve {
        state: state.to_string(),
        reserve_ratio: policy.reserve_ratio,
        throttle_at_ratio: policy.throttle_at_ratio,
        critical_boost: policy.critical_boost,
        min_task_budget: policy.min_task_budget,
        limit_tokens: budget.limit_tokens,
        spent_tokens: budget.spent_tokens,
        reserved_tokens: budget.reserved_tokens,
        remaining_tokens: remaining,
        path_weight_tokens,
        ratio_reserve_tokens,
        protected_tokens,
        headroom_tokens,
        throttled,
        mode: policy.mode,
        path_count: 0,
        allocated_tokens: protected_tokens,
        floor_tokens: 0,
        enumerated_paths: 0,
        paths_truncated: false,
    }
}

fn scale_basis_points(ratio: f64) -> u64 {
    if !ratio.is_finite() {
        return 0;
    }
    (ratio.clamp(0.0, 1.0) * 10_000.0).round() as u64
}

fn ratio_ceil(value: u64, ratio: f64) -> u64 {
    let basis_points = scale_basis_points(ratio);
    if basis_points == 0 {
        return 0;
    }
    value.saturating_mul(basis_points).div_ceil(10_000)
}

fn ratio_floor(value: u64, ratio: f64) -> u64 {
    let basis_points = scale_basis_points(ratio);
    value.saturating_mul(basis_points) / 10_000
}

fn admission_hash(
    policy: &CriticalPathPolicy,
    input: &GovernorEvaluation<'_>,
    path: &CriticalPath,
    reserve: &GovernorReserve,
) -> String {
    let mut material = String::new();
    material.push_str("governor.v1\n");
    material.push_str(&path.inputs_hash);
    material.push('\n');
    material.push_str(if policy.enabled { "on" } else { "off" });
    material.push('\n');
    material.push_str(&format!("{:.6}", policy.reserve_ratio));
    material.push('\n');
    material.push_str(&format!("{:.6}", policy.throttle_at_ratio));
    material.push('\n');
    material.push_str(&policy.critical_boost.to_string());
    material.push('\n');
    material.push_str(&policy.min_task_budget.to_string());
    material.push('\n');
    material.push_str(
        &input
            .budget
            .limit_tokens
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string()),
    );
    material.push('\n');
    material.push_str(&input.budget.spent_tokens.to_string());
    material.push('\n');
    material.push_str(&input.budget.reserved_tokens.to_string());
    material.push('\n');
    material.push_str(
        &reserve
            .remaining_tokens
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string()),
    );
    material.push('\n');
    material.push_str(&reserve.protected_tokens.to_string());
    material.push('\n');
    let mut candidates = input.candidates.iter().collect::<Vec<_>>();
    candidates.sort_by(|left, right| left.task.id.cmp(&right.task.id));
    for candidate in candidates {
        material.push_str(&candidate.task.id);
        material.push('|');
        material.push_str(&candidate.effective_priority.to_string());
        material.push('|');
        material.push_str(candidate.enqueued_at);
        material.push('\n');
    }
    crate::sha256_hex(material.as_bytes())
}

/// Build the persisted event payload for one admission evaluation.
pub fn admission_payload(
    session_id: &str,
    decision_id: &str,
    admission: &GovernorAdmission,
    created_at: &str,
) -> Value {
    let reason = admission.reason();
    json!({
        "decision_id": decision_id,
        "session_id": session_id,
        "plan_revision": admission.path.plan_revision,
        "action": "admission",
        "task_id": Value::Null,
        "critical": false,
        "reason": reason.as_str(),
        "code": reason.code(),
        "detail": bounded(&admission.detail(), 1_000),
        "path": admission.path.task_ids.iter().take(100).cloned().collect::<Vec<_>>(),
        "path_weight_tokens": admission.path.weight_tokens,
        "limit_tokens": admission.reserve.limit_tokens,
        "spent_tokens": admission.reserve.spent_tokens,
        "reserved_tokens": admission.reserve.reserved_tokens,
        "remaining_tokens": admission.reserve.remaining_tokens,
        "protected_tokens": admission.reserve.protected_tokens,
        "requested_tokens": admission
            .deferred
            .iter()
            .map(|deferral| deferral.weight_tokens)
            .sum::<u64>()
            .checked_add(
                admission
                    .decisions
                    .iter()
                    .filter(|decision| decision.admitted)
                    .map(|decision| decision.weight_tokens)
                    .sum::<u64>(),
            ),
        "admitted": admission.admitted.iter().take(100).cloned().collect::<Vec<_>>(),
        "deferred": admission
            .deferred
            .iter()
            .take(100)
            .map(|deferral| serde_json::to_value(deferral.info()).unwrap_or(Value::Null))
            .collect::<Vec<_>>(),
        "inputs_hash": admission.inputs_hash,
        "created_at": created_at,
        "mode": admission.mode.as_str(),
        "paths": admission
            .path_infos()
            .into_iter()
            .take(crate::multi_path::MAX_SELECTED_PATHS)
            .map(|path| serde_json::to_value(path).unwrap_or(Value::Null))
            .collect::<Vec<_>>(),
        "departments": admission
            .department_infos()
            .into_iter()
            .take(crate::multi_path::MAX_DEPARTMENT_ROWS)
            .map(|department| serde_json::to_value(department).unwrap_or(Value::Null))
            .collect::<Vec<_>>(),
        "allocated_tokens": admission.reserve.allocated_tokens,
        "deferred_order": admission.deferred_order(),
    })
}

pub struct GovernorDenial<'a> {
    pub session_id: &'a str,
    pub decision_id: &'a str,
    pub plan_revision: u32,
    pub mode: CriticalPathMode,
    pub task_id: &'a str,
    pub task_critical: bool,
    pub reason: GovernorReason,
    pub requested_tokens: u64,
    pub limit_tokens: Option<u64>,
    pub spent_tokens: u64,
    pub reserved_tokens: u64,
    pub remaining_tokens: Option<u64>,
    pub protected_tokens: u64,
    pub path: &'a [String],
    pub path_weight_tokens: u64,
    pub inputs_hash: &'a str,
    pub detail: String,
    pub created_at: &'a str,
}

/// Build the persisted event payload for one provider-call denial. This is the
/// point where a non-critical task is deferred without aborting the session.
pub fn denial_payload(denial: GovernorDenial<'_>) -> Value {
    json!({
        "decision_id": denial.decision_id,
        "session_id": denial.session_id,
        "plan_revision": denial.plan_revision,
        "action": "defer",
        "task_id": denial.task_id,
        "critical": denial.task_critical,
        "reason": denial.reason.as_str(),
        "code": denial.reason.code(),
        "detail": bounded(&denial.detail, 1_000),
        "path": denial.path.iter().take(100).cloned().collect::<Vec<_>>(),
        "path_weight_tokens": denial.path_weight_tokens,
        "limit_tokens": denial.limit_tokens,
        "spent_tokens": denial.spent_tokens,
        "reserved_tokens": denial.reserved_tokens,
        "remaining_tokens": denial.remaining_tokens,
        "protected_tokens": denial.protected_tokens,
        "requested_tokens": denial.requested_tokens,
        "admitted": Vec::<String>::new(),
        "deferred": Vec::<GovernorDeferralInfo>::new(),
        "inputs_hash": denial.inputs_hash,
        "created_at": denial.created_at,
        // A provider-call denial is task-scoped and does not carry a fresh
        // path table; the admission event owns it. These keys keep the event
        // shape stable for replay clients.
        "mode": denial.mode.as_str(),
        "paths": Vec::<Value>::new(),
        "departments": Vec::<Value>::new(),
        "allocated_tokens": 0u64,
        "deferred_order": vec![denial.task_id],
    })
}

/// Parse the bounded newest events into typed decision records. Unknown or
/// malformed payloads are skipped rather than failing a status read.
pub fn decisions_from_events(events: &[Event], limit: usize) -> Vec<BudgetGovernorDecision> {
    let limit = limit.clamp(1, MAX_DECISION_HISTORY);
    let mut decisions = events
        .iter()
        .filter(|event| event.kind == DECISION_EVENT_KIND)
        .filter_map(|event| {
            // A decision without an action is malformed audit noise, not a
            // zero-valued admission.
            event.payload.get("action")?.as_str()?;
            let mut decision =
                serde_json::from_value::<BudgetGovernorDecision>(event.payload.clone()).ok()?;
            if decision.session_id.is_empty() {
                decision.session_id = event.session_id.clone();
            }
            if decision.decision_id.is_empty() {
                decision.decision_id = format!("gov-{}", event.seq);
            }
            if decision.created_at.is_empty() {
                decision.created_at = event.created_at.clone();
            }
            Some(decision)
        })
        .collect::<Vec<_>>();
    if decisions.len() > limit {
        decisions.drain(0..decisions.len() - limit);
    }
    decisions
}

impl CriticalPath {
    pub fn info(&self) -> CriticalPathInfo {
        CriticalPathInfo {
            plan_revision: self.plan_revision,
            task_ids: self.task_ids.clone(),
            weight_tokens: self.weight_tokens,
            remaining_budget_tokens: self.remaining_budget_tokens,
            complete: self.complete,
            task_count: self.task_count.min(u32::MAX as usize) as u32,
            edge_count: self.edge_count.min(u32::MAX as usize) as u32,
            inputs_hash: self.inputs_hash.clone(),
        }
    }
}

impl GovernorAdmission {
    pub fn decision_infos(&self) -> Vec<GovernorTaskDecisionInfo> {
        self.decisions
            .iter()
            .map(GovernorTaskDecision::info)
            .collect()
    }

    pub fn deferral_infos(&self) -> Vec<GovernorDeferralInfo> {
        self.deferred.iter().map(GovernorDeferral::info).collect()
    }
}

/// Static per-run context used at provider pre-flight. It is computed once at
/// run start; a plan revision or status change is picked up by the next run or
/// scheduler tick, not mid-stream.
#[derive(Debug, Clone, PartialEq)]
pub struct GovernorRunContext {
    pub enabled: bool,
    pub policy: CriticalPathPolicy,
    pub task_id: Option<String>,
    pub critical: bool,
    /// Union of every protected path's task ids (bounded by the path table).
    pub path_task_ids: Vec<String>,
    /// Total selected path weight in multi mode, the F5 path weight otherwise.
    pub path_weight_tokens: u64,
    /// Protected path count (0/1 in single mode).
    pub path_count: usize,
    /// Reserve allocated to the runner's path, when the task is protected.
    pub path_allocation_tokens: Option<u64>,
    /// Unfunded demand of the runner's path, when known.
    pub path_deficit_tokens: u64,
    pub plan_revision: u32,
    pub inputs_hash: String,
}

impl GovernorRunContext {
    /// Build the reservation gate for one provider call. No gate is returned
    /// when the policy is disabled, the task is unknown (Leader/Manager), or
    /// the critical path is already complete.
    pub fn gate(
        &self,
        remaining_tokens: Option<u64>,
        limit_tokens: Option<u64>,
    ) -> Option<GovernorGate> {
        if !self.enabled || self.path_weight_tokens == 0 {
            return None;
        }
        let task_id = self.task_id.clone()?;
        let budget = GovernorBudgetView::new(limit_tokens, 0, 0);
        let reserve = reserve_math(
            &self.policy,
            budget,
            remaining_tokens,
            self.path_weight_tokens,
            false,
        );
        // The multi-path pool is bounded by remaining and by the sum of path
        // demand plus the ratio buffer. reserve_math derives exactly that
        // bound from `path_weight_tokens`, so the provider pre-flight clamp
        // is identical to single mode.
        Some(GovernorGate {
            enabled: true,
            task_id: Some(task_id),
            critical: self.critical,
            remaining_tokens,
            protected_tokens: reserve.protected_tokens,
            throttled: reserve.throttled,
            min_task_budget: self.policy.min_task_budget,
            policy_fingerprint: self.inputs_hash.clone(),
        })
    }
}

/// Path membership for one candidate. Multi mode uses the selected path
/// table; single mode reports the compatibility row (`#0`) for critical tasks
/// so per-decision fields agree with `path_infos`.
fn single_path_id(
    multi: Option<&MultiPathAllocation>,
    task_id: &str,
    critical: bool,
) -> Option<u32> {
    match multi {
        Some(multi) => multi.task_path_id(task_id),
        None if critical => Some(0),
        None => None,
    }
}

fn single_path_ids(multi: Option<&MultiPathAllocation>, task_id: &str, critical: bool) -> Vec<u32> {
    match multi {
        Some(multi) => multi.task_path_ids(task_id),
        None if critical => vec![0],
        None => Vec::new(),
    }
}

fn bounded(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let mut truncated = value
        .chars()
        .take(max.saturating_sub(1))
        .collect::<String>();
    truncated.push('…');
    truncated
}

#[cfg(test)]
mod tests;

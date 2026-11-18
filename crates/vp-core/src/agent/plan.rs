use super::*;

mod execute;
mod memory;
mod roundtable;
mod support;
mod validate;
mod worker;

// Flat sibling visibility, unchanged from the pre-split single module. The
// agent parent glob-imports this module, so these re-exports keep the existing
// `crate::agent::...` call sites working without widening any API.
pub(super) use execute::execute_team_plan;
pub(super) use memory::record_task_memory;
pub(crate) use memory::{record_integration_resolution_memory, record_verification_memory};
pub(super) use roundtable::deliberate_team_plan;
pub(super) use support::{
    ensure_plan_manager, latest_plan_results, record_scheduled_task_finish, select_plan_batch,
    task_profile,
};
pub(crate) use validate::validate_team_plan;
pub(super) use worker::{finish_labour, finish_worker, parse_verdict};

pub(super) enum RoundtableResult {
    Done(String),
    Cancelled,
    BudgetExceeded(BudgetStop),
}

pub(super) enum PlanRunResult {
    Done(String),
    Cancelled,
    BudgetExceeded(BudgetStop),
    Deferred(DeferredWait),
}

pub(super) enum PlanAbort {
    Cancelled,
    Budget(BudgetStop),
    Deferred(DeferredWait),
}

impl PlanAbort {
    pub(super) fn task_status(&self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::Budget(_) => "blocked",
            Self::Deferred(_) => "blocked",
        }
    }

    pub(super) fn reason(&self) -> String {
        match self {
            Self::Cancelled => "session cancelled".to_string(),
            Self::Budget(stop) => stop.summary(),
            Self::Deferred(wait) => {
                format!("waiting for external gate {}", wait.gate_id)
            }
        }
    }
}

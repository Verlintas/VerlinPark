//! Deterministic controller decisions for a running TeamPlan.
//!
//! The controller never calls a model. It reads the persisted task queue and
//! coordination notes, then emits a small set of idempotent transitions:
//! retry, block, unblock, cancel, escalate, and automatic blocker resolution.
//! The runtime executes those decisions through the event store so every
//! choice is replayable.

use std::collections::{HashMap, HashSet};

use vp_protocol::types::{
    DependencyFailurePolicy, Event, OrchestrationNote, OrchestrationPolicy, ScheduledTask, Session,
    TeamPlan,
};

use super::plan::{blocked_by, status_map};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControllerAction {
    Retry {
        task_id: String,
        delay_secs: u64,
        reason: String,
    },
    Block {
        task_id: String,
        dependency: String,
        reason: String,
    },
    Unblock {
        task_id: String,
        reason: String,
    },
    Cancel {
        task_id: String,
        dependency: String,
        reason: String,
    },
    Escalate {
        task_id: String,
        attempt: u32,
        message: String,
    },
    ResolveBlockers {
        task_id: String,
        count: usize,
    },
}

impl ControllerAction {
    pub fn action(&self) -> &'static str {
        match self {
            Self::Retry { .. } => "retry",
            Self::Block { .. } => "block",
            Self::Unblock { .. } => "unblock",
            Self::Cancel { .. } => "cancel",
            Self::Escalate { .. } => "escalate",
            Self::ResolveBlockers { .. } => "resolve_blockers",
        }
    }

    pub fn task_id(&self) -> &str {
        match self {
            Self::Retry { task_id, .. }
            | Self::Block { task_id, .. }
            | Self::Unblock { task_id, .. }
            | Self::Cancel { task_id, .. }
            | Self::Escalate { task_id, .. }
            | Self::ResolveBlockers { task_id, .. } => task_id,
        }
    }

    pub fn detail(&self) -> String {
        match self {
            Self::Retry { reason, .. } | Self::Unblock { reason, .. } => reason.clone(),
            Self::Block {
                dependency, reason, ..
            }
            | Self::Cancel {
                dependency, reason, ..
            } => {
                format!("{reason} (dependency {dependency})")
            }
            Self::Escalate { message, .. } => message.clone(),
            Self::ResolveBlockers { count, .. } => {
                format!("resolved {count} coordination blocker(s)")
            }
        }
    }

    pub fn attempt(&self) -> u32 {
        match self {
            Self::Escalate { attempt, .. } => *attempt,
            _ => 0,
        }
    }
}

pub struct ControllerInput<'a> {
    pub session: &'a Session,
    pub plan: &'a TeamPlan,
    pub scheduled: &'a [ScheduledTask],
    pub events: &'a [Event],
    pub notes: &'a [OrchestrationNote],
    pub policy: &'a OrchestrationPolicy,
    /// A session-level budget guard. Retries are not scheduled after the
    /// configured token or USD budget is exhausted.
    pub budget_exhausted: bool,
}

/// Derive the next deterministic actions. This function is pure and is the
/// primary unit-test surface for orchestration behavior.
pub fn plan_actions(input: ControllerInput<'_>) -> Vec<ControllerAction> {
    let statuses = status_map(input.plan, input.scheduled, input.events);
    let rows = input
        .scheduled
        .iter()
        .map(|row| (row.task_id.as_str(), row))
        .collect::<HashMap<_, _>>();
    let completed = statuses
        .iter()
        .filter(|(_, status)| matches!(status.as_str(), "completed" | "skipped"))
        .map(|(task_id, _)| task_id.as_str())
        .collect::<HashSet<_>>();
    let escalated = escalated_attempts(input.events);
    let open_blockers = input
        .notes
        .iter()
        .filter(|note| note.status == "open" && note.action == "blocker")
        .collect::<Vec<_>>();
    let mut actions = Vec::new();

    // 1. Dependencies that were blocked by an earlier failure but whose
    //    upstream work has since completed can be released.
    for task in &input.plan.tasks {
        let status = statuses
            .get(&task.id)
            .map(String::as_str)
            .unwrap_or("pending");
        if status != "blocked" {
            continue;
        }
        if task.deps.iter().all(|dep| completed.contains(dep.as_str())) {
            actions.push(ControllerAction::Unblock {
                task_id: task.id.clone(),
                reason: "all upstream dependencies completed".to_string(),
            });
        }
    }

    // 2. Failed tasks: retry while budget remains, otherwise apply the
    //    dependency-failure policy and escalate once.
    for task in &input.plan.tasks {
        let Some(row) = rows.get(task.id.as_str()) else {
            continue;
        };
        let status = statuses
            .get(&task.id)
            .map(String::as_str)
            .unwrap_or("pending");
        if status != "failed" || row.next_attempt_at.is_some() {
            continue;
        }
        if input.policy.auto_retry_failed
            && !input.budget_exhausted
            && row.attempts < input.policy.max_attempts
        {
            let delay = super::retry_delay(input.policy, row.attempts);
            actions.push(ControllerAction::Retry {
                task_id: task.id.clone(),
                delay_secs: delay.as_secs(),
                reason: format!(
                    "attempt {} failed: {}",
                    row.attempts,
                    row.last_error
                        .as_deref()
                        .unwrap_or("no error detail recorded")
                ),
            });
            continue;
        }
        let reason = row
            .last_error
            .clone()
            .unwrap_or_else(|| "task failed without an error detail".to_string());
        if input.policy.escalate_after_retries
            && !escalated.contains(&(task.id.clone(), row.attempts))
        {
            actions.push(ControllerAction::Escalate {
                task_id: task.id.clone(),
                attempt: row.attempts,
                message: format!(
                    "{} exhausted {} attempt(s): {}",
                    task.id,
                    row.attempts.max(1),
                    reason
                ),
            });
        }
        let dependents = direct_dependents(input.plan, &task.id);
        for dependent in dependents {
            let dependent_status = statuses
                .get(&dependent)
                .map(String::as_str)
                .unwrap_or("pending");
            if matches!(
                dependent_status,
                "completed" | "skipped" | "cancelled" | "running" | "cancelling"
            ) {
                continue;
            }
            let dependency_reason = format!("dependency {} failed", task.id);
            match input.policy.dependency_failure {
                DependencyFailurePolicy::Block => {
                    if matches!(
                        dependent_status,
                        "pending" | "interrupted" | "retry_scheduled"
                    ) {
                        actions.push(ControllerAction::Block {
                            task_id: dependent.to_string(),
                            dependency: task.id.clone(),
                            reason: dependency_reason,
                        });
                    }
                }
                DependencyFailurePolicy::Cancel => {
                    actions.push(ControllerAction::Cancel {
                        task_id: dependent.to_string(),
                        dependency: task.id.clone(),
                        reason: dependency_reason,
                    });
                }
                DependencyFailurePolicy::Retry => {}
            }
        }
    }

    // 3. Completed tasks release coordination blockers that referenced them.
    if input.policy.auto_resolve_blockers {
        let mut seen = HashSet::new();
        for note in &open_blockers {
            let Some(task_id) = note.task_id.as_deref() else {
                continue;
            };
            if !completed.contains(task_id) || !seen.insert(task_id.to_string()) {
                continue;
            }
            let count = open_blockers
                .iter()
                .filter(|other| other.task_id.as_deref() == Some(task_id))
                .count();
            actions.push(ControllerAction::ResolveBlockers {
                task_id: task_id.to_string(),
                count,
            });
        }
    }

    // 4. Blocked rows with no failed dependency and no unmet dependency are
    //    stale and should be released (for example after a plan revision).
    for row in input.scheduled {
        if row.status != "blocked" || actions.iter().any(|action| {
            matches!(action, ControllerAction::Unblock { task_id, .. } if task_id == &row.task_id)
        }) {
            continue;
        }
        let Some(task) = input.plan.tasks.iter().find(|task| task.id == row.task_id) else {
            continue;
        };
        let unmet = blocked_by(task, &statuses);
        if unmet.is_empty() {
            actions.push(ControllerAction::Unblock {
                task_id: row.task_id.clone(),
                reason: "no unmet dependencies remain".to_string(),
            });
        }
    }

    actions
}

fn direct_dependents(plan: &TeamPlan, task_id: &str) -> Vec<String> {
    plan.tasks
        .iter()
        .filter(|task| task.deps.iter().any(|dep| dep == task_id))
        .map(|task| task.id.clone())
        .collect()
}

fn escalated_attempts(events: &[Event]) -> HashSet<(String, u32)> {
    events
        .iter()
        .filter(|event| event.kind == "orchestration.decision")
        .filter(|event| {
            event
                .payload
                .get("action")
                .and_then(serde_json::Value::as_str)
                == Some("escalate")
        })
        .filter_map(|event| {
            let task_id = event
                .payload
                .get("task_id")
                .and_then(serde_json::Value::as_str)?
                .to_string();
            let attempt = event
                .payload
                .get("attempt")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as u32;
            Some((task_id, attempt))
        })
        .collect()
}

/// A compact description used by the daemon and CLI when a controller pass
/// changes nothing.
pub fn session_has_actionable_work(input: &ControllerInput<'_>) -> bool {
    let statuses = status_map(input.plan, input.scheduled, input.events);
    let has_pending = statuses.values().any(|status| {
        matches!(
            status.as_str(),
            "pending" | "failed" | "blocked" | "interrupted"
        )
    });
    let has_live = statuses.values().any(|status| {
        matches!(
            status.as_str(),
            "running" | "queued" | "paused" | "pausing" | "cancelling"
        )
    });
    let _ = input.session;
    has_pending || has_live
}

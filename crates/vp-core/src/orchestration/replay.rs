//! Read-only controller replay (ADR-0196).
//!
//! Replay reconstructs the deterministic inputs of recorded orchestration,
//! scaling, scheduler-dispatch, and plan-resume decisions from the event log,
//! feeds them to the current pure controllers, and diffs the result.
//!
//! It never calls a provider, executes a tool, appends an event, or mutates a
//! plan/task/budget/capacity/approval/Verifier/merge row. A mismatch is
//! diagnostic evidence, never a retry, cancellation, merge gate, or proof that
//! a model behaved differently. Results are "controller-equivalent only".

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use vp_protocol::now_rfc3339;
use vp_protocol::replay::{
    ControllerReplayDiff, ControllerReplayEntry, ControllerReplayReport, ControllerReplayStatus,
};
use vp_protocol::scaling::{ScalingDecision, ScalingReplayInputs};
use vp_protocol::types::{
    CriticalPathPolicy, Event, OrchestrationPolicy, ScheduledTask, Session, TeamPlan,
};

use crate::Engine;
use crate::orchestration::{ControllerInput, coordination_notes, plan_actions};

pub const MAX_REPLAY_DECISIONS: u32 = 2_000;
pub const DEFAULT_REPLAY_DECISIONS: u32 = 200;

const REPLAY_KINDS: &[&str] = &[
    "orchestration.decision",
    "scaling.decision",
    "plan_task.started",
    "agent.plan_resumed",
    "agent.task_dispatched",
];

#[derive(Debug, Clone)]
struct TaskState {
    status: String,
    attempts: u32,
    started_at: Option<String>,
    finished_at: Option<String>,
    last_error: Option<String>,
    next_attempt_at: Option<String>,
}

impl Default for TaskState {
    fn default() -> Self {
        Self {
            status: "pending".to_string(),
            attempts: 0,
            started_at: None,
            finished_at: None,
            last_error: None,
            next_attempt_at: None,
        }
    }
}

/// Reconstruct the durable task projection at one point in the event log.
/// This mirrors `Store::sync_plan_tasks`; both use plan lifecycle events as the
/// source of truth, so replay does not depend on today's mutated rows.
fn scheduled_at(
    plan: &TeamPlan,
    project_id: &str,
    plan_seq: u64,
    enqueued_at: &str,
    events: &[Event],
    before_seq: u64,
) -> Vec<ScheduledTask> {
    let known = plan
        .tasks
        .iter()
        .map(|task| task.id.as_str())
        .collect::<HashSet<_>>();
    let mut states = HashMap::<String, TaskState>::new();
    for event in events
        .iter()
        .filter(|event| event.seq > plan_seq && event.seq < before_seq)
    {
        if !event.kind.starts_with("plan_task.") {
            continue;
        }
        let Some(task_id) = event
            .payload
            .get("task_id")
            .and_then(Value::as_str)
            .filter(|task_id| known.contains(task_id))
        else {
            continue;
        };
        let state = states.entry(task_id.to_string()).or_default();
        match event.kind.as_str() {
            "plan_task.started" => {
                if state.status != "completed" {
                    state.status = "interrupted".to_string();
                    state.finished_at = None;
                    state.last_error = None;
                }
                state.attempts = state.attempts.saturating_add(1);
                state.started_at = Some(event.created_at.clone());
            }
            "plan_task.finished" => {
                let status = event
                    .payload
                    .get("task_status")
                    .and_then(Value::as_str)
                    .unwrap_or("failed");
                state.status = status.to_string();
                state.finished_at = Some(event.created_at.clone());
                state.next_attempt_at = None;
                state.last_error = if status == "completed" {
                    None
                } else {
                    event
                        .payload
                        .get("reason")
                        .and_then(Value::as_str)
                        .or_else(|| event.payload.get("summary").and_then(Value::as_str))
                        .map(str::to_string)
                };
            }
            "plan_task.retry_scheduled" => {
                state.status = "pending".to_string();
                state.finished_at = None;
                state.next_attempt_at = event
                    .payload
                    .get("next_attempt_at")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                state.last_error = event
                    .payload
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            "plan_task.interrupted" => {
                state.status = "interrupted".to_string();
                state.finished_at = Some(event.created_at.clone());
                state.last_error = event
                    .payload
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            "plan_task.retried" => {
                state.status = "pending".to_string();
                state.finished_at = None;
                state.last_error = None;
            }
            "plan_task.cancelling" => {
                state.status = "cancelling".to_string();
                state.finished_at = None;
                state.last_error = event
                    .payload
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            "plan_task.pausing" => {
                state.status = "pausing".to_string();
                state.finished_at = None;
                state.last_error = event
                    .payload
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            "plan_task.paused" => {
                state.status = "paused".to_string();
                state.finished_at = None;
                state.last_error = event
                    .payload
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            "plan_task.resumed" => {
                state.status = "running".to_string();
                state.finished_at = None;
                state.last_error = None;
            }
            "plan_task.skipped" => {
                state.status = "skipped".to_string();
                state.finished_at = Some(event.created_at.clone());
                state.last_error = event
                    .payload
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            "plan_task.blocked" => {
                state.status = "blocked".to_string();
                state.finished_at = None;
                state.last_error = event
                    .payload
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            "plan_task.unblocked" => {
                state.status = "pending".to_string();
                state.finished_at = None;
                state.last_error = None;
                state.next_attempt_at = None;
            }
            "plan_task.cancelled" => {
                state.status = "cancelled".to_string();
                state.finished_at = Some(event.created_at.clone());
                state.last_error = event
                    .payload
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            _ => {}
        }
    }
    plan.tasks
        .iter()
        .map(|task| {
            let state = states.get(&task.id);
            ScheduledTask {
                session_id: String::new(),
                project_id: project_id.to_string(),
                plan_seq,
                task_id: task.id.clone(),
                department: task.department.clone(),
                title: task.title.clone(),
                priority: task.priority,
                status: state
                    .map(|state| state.status.clone())
                    .unwrap_or_else(|| "pending".to_string()),
                deps: task.deps.clone(),
                write_scope: task.write_scope.clone(),
                attempts: state.map(|state| state.attempts).unwrap_or(0),
                enqueued_at: enqueued_at.to_string(),
                started_at: state.and_then(|state| state.started_at.clone()),
                finished_at: state.and_then(|state| state.finished_at.clone()),
                last_error: state.and_then(|state| state.last_error.clone()),
                next_attempt_at: state.and_then(|state| state.next_attempt_at.clone()),
            }
        })
        .collect()
}

fn budget_exhausted_at(session: &Session, events: &[Event], before_seq: u64) -> bool {
    if session.unlimited {
        return false;
    }
    let (tokens_in, tokens_out, cost) = events
        .iter()
        .filter(|event| event.seq < before_seq && event.kind == "agent.usage")
        .fold((0u64, 0u64, 0f64), |(input, output, cost), event| {
            (
                input.saturating_add(
                    event
                        .payload
                        .get("input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                ),
                output.saturating_add(
                    event
                        .payload
                        .get("output_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                ),
                cost + event
                    .payload
                    .get("cost_usd")
                    .and_then(Value::as_f64)
                    .filter(|cost| cost.is_finite() && *cost > 0.0)
                    .unwrap_or(0.0),
            )
        });
    let tokens = tokens_in.saturating_add(tokens_out);
    session.token_budget.is_some_and(|limit| tokens >= limit)
        || session.cost_budget_usd.is_some_and(|limit| cost >= limit)
}

fn changed(
    field: &str,
    recorded: impl Into<String>,
    recomputed: impl Into<String>,
) -> ControllerReplayDiff {
    let mut recorded = recorded.into();
    let mut recomputed = recomputed.into();
    if recorded.chars().count() > 200 {
        recorded = recorded.chars().take(200).collect();
    }
    if recomputed.chars().count() > 200 {
        recomputed = recomputed.chars().take(200).collect();
    }
    ControllerReplayDiff {
        field: field.to_string(),
        recorded,
        recomputed,
    }
}

fn action_label(action: &crate::orchestration::ControllerAction) -> String {
    format!("{}:{}", action.action(), action.task_id())
}

fn replay_orchestration_decision(
    session: &Session,
    events: &[Event],
    event: &Event,
    policy: &OrchestrationPolicy,
) -> ControllerReplayEntry {
    let Some((plan, plan_seq)) = crate::runtime_pin::plan_at(events, event.seq) else {
        return ControllerReplayEntry {
            seq: event.seq,
            kind: event.kind.clone(),
            recorded_at: Some(event.created_at.clone()),
            status: ControllerReplayStatus::Skipped,
            recorded_action: event
                .payload
                .get("action")
                .and_then(Value::as_str)
                .map(str::to_string),
            task_id: event
                .payload
                .get("task_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            reason: Some("no TeamPlan was in effect at this decision".to_string()),
            ..Default::default()
        };
    };
    let recorded_action = event
        .payload
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let recorded_task = event
        .payload
        .get("task_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let plan_event = events.iter().find(|candidate| candidate.seq == plan_seq);
    let enqueued_at = plan_event
        .map(|event| event.created_at.clone())
        .unwrap_or_else(|| event.created_at.clone());
    let scheduled = scheduled_at(
        &plan,
        &session.project_id,
        plan_seq,
        &enqueued_at,
        events,
        event.seq,
    );
    let prior = events
        .iter()
        .filter(|candidate| candidate.seq < event.seq)
        .cloned()
        .collect::<Vec<_>>();
    let notes = coordination_notes(&prior);
    let actions = plan_actions(ControllerInput {
        session,
        plan: &plan,
        scheduled: &scheduled,
        events: &prior,
        notes: &notes,
        policy,
        budget_exhausted: budget_exhausted_at(session, &prior, event.seq),
    });
    let labels = actions.iter().map(action_label).collect::<Vec<_>>();
    let matched = actions.iter().any(|action| {
        action.action() == recorded_action
            && recorded_task
                .as_deref()
                .is_none_or(|task_id| action.task_id() == task_id)
    });
    let mut changes = Vec::new();
    if !matched {
        changes.push(changed(
            "action",
            format!(
                "{recorded_action}:{}",
                recorded_task.as_deref().unwrap_or("(session)")
            ),
            labels
                .iter()
                .take(8)
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
        ));
        if labels.is_empty() {
            changes.push(changed("recomputed", "no controller action", "none"));
        }
    }
    ControllerReplayEntry {
        seq: event.seq,
        kind: event.kind.clone(),
        recorded_at: Some(event.created_at.clone()),
        status: if matched {
            ControllerReplayStatus::Matched
        } else {
            ControllerReplayStatus::Mismatched
        },
        recorded_action: Some(recorded_action),
        recomputed_action: actions.first().map(action_label),
        task_id: recorded_task,
        changes,
        reason: if matched {
            None
        } else {
            Some("current deterministic controller produced a different action set".to_string())
        },
    }
}

fn scaling_observation(
    decision: &ScalingDecision,
    inputs: &ScalingReplayInputs,
) -> Option<crate::scaling::ScalingObservation> {
    let mut observation = crate::scaling::observation_from_replay_inputs(inputs)?;
    observation.session_id = decision.session_id.clone();
    observation.project_id = decision.project_id.clone();
    Some(observation)
}

fn replay_scaling_decision(event: &Event) -> ControllerReplayEntry {
    let decision = match serde_json::from_value::<ScalingDecision>(event.payload.clone()) {
        Ok(decision) => decision,
        Err(error) => {
            return ControllerReplayEntry {
                seq: event.seq,
                kind: event.kind.clone(),
                recorded_at: Some(event.created_at.clone()),
                status: ControllerReplayStatus::Skipped,
                reason: Some(format!("malformed scaling decision: {error}")),
                ..Default::default()
            };
        }
    };
    let Some(raw_inputs) = decision.replay_inputs.clone() else {
        return ControllerReplayEntry {
            seq: event.seq,
            kind: event.kind.clone(),
            recorded_at: Some(event.created_at.clone()),
            status: ControllerReplayStatus::Skipped,
            recorded_action: Some(decision.action.as_str().to_string()),
            task_id: None,
            reason: Some(
                "scaling_inputs_not_recorded: this decision predates replayable scaling inputs"
                    .to_string(),
            ),
            ..Default::default()
        };
    };
    let inputs = match serde_json::from_value::<ScalingReplayInputs>(raw_inputs) {
        Ok(inputs) => inputs,
        Err(error) => {
            return ControllerReplayEntry {
                seq: event.seq,
                kind: event.kind.clone(),
                recorded_at: Some(event.created_at.clone()),
                status: ControllerReplayStatus::Skipped,
                recorded_action: Some(decision.action.as_str().to_string()),
                reason: Some(format!("invalid scaling replay inputs: {error}")),
                ..Default::default()
            };
        }
    };
    let Some(observation) = scaling_observation(&decision, &inputs) else {
        return ControllerReplayEntry {
            seq: event.seq,
            kind: event.kind.clone(),
            recorded_at: Some(event.created_at.clone()),
            status: ControllerReplayStatus::Skipped,
            recorded_action: Some(decision.action.as_str().to_string()),
            reason: Some("scaling replay inputs contain an invalid timestamp".to_string()),
            ..Default::default()
        };
    };
    let recomputed = crate::scaling::evaluate(&inputs.policy, &observation);
    let mut changes = Vec::new();
    let action_matches = recomputed.action == decision.action;
    if !action_matches {
        changes.push(changed(
            "action",
            decision.action.as_str(),
            recomputed.action.as_str(),
        ));
    }
    // Applied decisions may have been clamped by live slots; report the
    // difference without treating the normal clamp as a controller mismatch.
    if decision.target_workers != recomputed.target_workers {
        changes.push(changed(
            "target_workers",
            decision.target_workers.to_string(),
            recomputed.target_workers.to_string(),
        ));
    }
    if decision.target_labour != recomputed.target_labour {
        changes.push(changed(
            "target_labour",
            decision.target_labour.to_string(),
            recomputed.target_labour.to_string(),
        ));
    }
    ControllerReplayEntry {
        seq: event.seq,
        kind: event.kind.clone(),
        recorded_at: Some(event.created_at.clone()),
        status: if action_matches {
            ControllerReplayStatus::Matched
        } else {
            ControllerReplayStatus::Mismatched
        },
        recorded_action: Some(decision.action.as_str().to_string()),
        recomputed_action: Some(recomputed.action.as_str().to_string()),
        task_id: None,
        changes,
        reason: if action_matches {
            None
        } else {
            Some("current pure scaling controller produced a different action".to_string())
        },
    }
}

fn ready_task_order(
    plan: &TeamPlan,
    scheduled: &[ScheduledTask],
    session: &Session,
    now: OffsetDateTime,
) -> Vec<String> {
    let completed = scheduled
        .iter()
        .filter(|row| matches!(row.status.as_str(), "completed" | "skipped"))
        .map(|row| row.task_id.clone())
        .collect::<HashSet<_>>();
    let aging = crate::scheduler::task_aging();
    let mut ready = scheduled
        .iter()
        .filter(|row| crate::scheduler::is_retryable_status(&row.status))
        .filter(|row| row.deps.iter().all(|dep| completed.contains(dep)))
        .filter(|row| {
            row.next_attempt_at
                .as_deref()
                .map(|next| {
                    OffsetDateTime::parse(next, &Rfc3339)
                        .map(|next| next <= now)
                        .unwrap_or(true)
                })
                .unwrap_or(true)
        })
        .map(|row| {
            (
                crate::scheduler::task_effective_priority(session.priority, row, now, aging),
                row,
            )
        })
        .collect::<Vec<_>>();
    ready.sort_by(|(left_priority, left), (right_priority, right)| {
        right_priority
            .cmp(left_priority)
            .then_with(|| left.enqueued_at.cmp(&right.enqueued_at))
            .then_with(|| left.task_id.cmp(&right.task_id))
    });
    ready
        .into_iter()
        .map(|(_, row)| row.task_id.clone())
        .filter(|task_id| plan.tasks.iter().any(|task| task.id == *task_id))
        .collect()
}

fn replay_dispatch_decision(
    session: &Session,
    events: &[Event],
    event: &Event,
    critical_policy: &CriticalPathPolicy,
) -> ControllerReplayEntry {
    let Some((plan, plan_seq)) = crate::runtime_pin::plan_at(events, event.seq) else {
        return ControllerReplayEntry {
            seq: event.seq,
            kind: event.kind.clone(),
            recorded_at: Some(event.created_at.clone()),
            status: ControllerReplayStatus::Skipped,
            recorded_action: Some("dispatch".to_string()),
            task_id: event
                .payload
                .get("task_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            reason: Some("no TeamPlan was in effect at this dispatch".to_string()),
            ..Default::default()
        };
    };
    if critical_policy.enabled {
        return ControllerReplayEntry {
            seq: event.seq,
            kind: event.kind.clone(),
            recorded_at: Some(event.created_at.clone()),
            status: ControllerReplayStatus::Skipped,
            recorded_action: Some("dispatch".to_string()),
            task_id: event
                .payload
                .get("task_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            reason: Some(
                "critical_path_governor_active: historical governor inputs are not recorded"
                    .to_string(),
            ),
            ..Default::default()
        };
    }
    let task_id = event
        .payload
        .get("task_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let plan_event = events.iter().find(|candidate| candidate.seq == plan_seq);
    let enqueued_at = plan_event
        .map(|event| event.created_at.clone())
        .unwrap_or_else(|| event.created_at.clone());
    let scheduled = scheduled_at(
        &plan,
        &session.project_id,
        plan_seq,
        &enqueued_at,
        events,
        event.seq,
    );
    let now = OffsetDateTime::parse(&event.created_at, &Rfc3339)
        .unwrap_or_else(|_| OffsetDateTime::now_utc());
    let ready = ready_task_order(&plan, &scheduled, session, now);
    let matched = ready.iter().any(|candidate| candidate == &task_id);
    let mut changes = Vec::new();
    if !matched {
        changes.push(changed(
            "dispatch_readiness",
            task_id.clone(),
            ready
                .iter()
                .take(12)
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    ControllerReplayEntry {
        seq: event.seq,
        kind: event.kind.clone(),
        recorded_at: Some(event.created_at.clone()),
        status: if matched {
            ControllerReplayStatus::Matched
        } else {
            ControllerReplayStatus::Mismatched
        },
        recorded_action: Some("dispatch".to_string()),
        recomputed_action: ready.first().map(|task_id| format!("dispatch:{task_id}")),
        task_id: Some(task_id),
        changes,
        reason: if matched {
            None
        } else {
            Some("task was not in the deterministic ready set at dispatch time".to_string())
        },
    }
}

fn replay_resume_decision(
    session: &Session,
    events: &[Event],
    event: &Event,
    task_limit: Option<u64>,
) -> ControllerReplayEntry {
    let Some((plan, plan_seq)) = crate::runtime_pin::plan_at(events, event.seq) else {
        return ControllerReplayEntry {
            seq: event.seq,
            kind: event.kind.clone(),
            recorded_at: Some(event.created_at.clone()),
            status: ControllerReplayStatus::Skipped,
            recorded_action: Some("resume".to_string()),
            reason: Some("no TeamPlan was in effect at this resume".to_string()),
            ..Default::default()
        };
    };
    let plan_event = events.iter().find(|candidate| candidate.seq == plan_seq);
    let enqueued_at = plan_event
        .map(|event| event.created_at.clone())
        .unwrap_or_else(|| event.created_at.clone());
    let scheduled = scheduled_at(
        &plan,
        &session.project_id,
        plan_seq,
        &enqueued_at,
        events,
        event.seq,
    );
    let remaining = scheduled
        .iter()
        .filter(|row| !matches!(row.status.as_str(), "completed" | "skipped"))
        .map(|row| row.task_id.clone())
        .collect::<Vec<_>>();
    let matched = !remaining.is_empty();
    let mut changes = Vec::new();
    if !matched {
        changes.push(changed("remaining_tasks", "0", "0"));
    }
    ControllerReplayEntry {
        seq: event.seq,
        kind: event.kind.clone(),
        recorded_at: Some(event.created_at.clone()),
        status: if matched {
            ControllerReplayStatus::Matched
        } else {
            ControllerReplayStatus::Mismatched
        },
        recorded_action: Some(format!(
            "resume:{}",
            task_limit
                .map(|limit| format!("limit={limit}"))
                .unwrap_or_else(|| "plan".to_string())
        )),
        recomputed_action: Some(format!("remaining={}", remaining.len())),
        task_id: None,
        changes,
        reason: if matched {
            None
        } else {
            Some("resume was recorded but the reconstructed plan had no remaining work".to_string())
        },
    }
}

/// Pure replay over one session log. `current_policy` and `critical_policy`
/// are the current project policies; each scaling decision carries its own
/// recorded policy, while orchestration decisions use the current policy (the
/// report makes that limitation visible).
pub(crate) fn replay(
    session: &Session,
    events: &[Event],
    policy: &OrchestrationPolicy,
    critical_policy: &CriticalPathPolicy,
    limit: u32,
    event_window_truncated: bool,
) -> ControllerReplayReport {
    let limit = limit.clamp(1, MAX_REPLAY_DECISIONS);
    let decision_events = events
        .iter()
        .filter(|event| REPLAY_KINDS.contains(&event.kind.as_str()))
        .collect::<Vec<_>>();
    let total_recorded = decision_events.len() as u64;
    let truncated = event_window_truncated || decision_events.len() > limit as usize;
    let selected = decision_events
        .iter()
        .skip(decision_events.len().saturating_sub(limit as usize))
        .copied()
        .collect::<Vec<_>>();
    let mut entries = Vec::with_capacity(selected.len());
    for event in selected {
        let entry = match event.kind.as_str() {
            "orchestration.decision" => {
                replay_orchestration_decision(session, events, event, policy)
            }
            "scaling.decision" => replay_scaling_decision(event),
            "plan_task.started" => {
                replay_dispatch_decision(session, events, event, critical_policy)
            }
            "agent.plan_resumed" | "agent.task_dispatched" => replay_resume_decision(
                session,
                events,
                event,
                event.payload.get("task_limit").and_then(Value::as_u64),
            ),
            other => ControllerReplayEntry {
                seq: event.seq,
                kind: other.to_string(),
                recorded_at: Some(event.created_at.clone()),
                status: ControllerReplayStatus::Skipped,
                reason: Some("unsupported replay event".to_string()),
                ..Default::default()
            },
        };
        entries.push(entry);
    }
    let matched = entries
        .iter()
        .filter(|entry| entry.status == ControllerReplayStatus::Matched)
        .count() as u64;
    let mismatched = entries
        .iter()
        .filter(|entry| entry.status == ControllerReplayStatus::Mismatched)
        .count() as u64;
    let skipped = entries
        .iter()
        .filter(|entry| entry.status == ControllerReplayStatus::Skipped)
        .count() as u64;
    let mut warnings = Vec::new();
    if event_window_truncated {
        warnings.push(
            "replay read a bounded event window; older plan/task state is not included".to_string(),
        );
    }
    if truncated {
        warnings.push(format!(
            "report covers the newest {limit} decision event(s) out of {total_recorded}"
        ));
    }
    if mismatched > 0 {
        warnings.push(
            "controller-equivalent only: a mismatch is diagnostic and never retries, cancels, merges, or rewrites a plan"
                .to_string(),
        );
    }
    ControllerReplayReport {
        session_id: session.id.clone(),
        limit,
        window_start_seq: entries.first().map(|entry| entry.seq).unwrap_or(0),
        window_end_seq: entries.last().map(|entry| entry.seq).unwrap_or(0),
        total_recorded,
        examined: entries.len() as u64,
        matched,
        mismatched,
        skipped,
        truncated,
        entries,
        policy_hash: String::new(),
        controller_equivalent_only: true,
        generated_at: now_rfc3339(),
        warnings,
    }
}

/// Read-only engine method. The entire operation runs in one blocking store
/// call; no provider, tool, event append, or projection mutation is involved.
pub async fn replay_orchestration(
    engine: &Engine,
    session_id: &str,
    limit: Option<u32>,
) -> Result<ControllerReplayReport> {
    let store = engine.store().clone();
    let session_id = session_id.to_string();
    let limit = limit.unwrap_or(DEFAULT_REPLAY_DECISIONS);
    tokio::task::spawn_blocking(move || -> Result<ControllerReplayReport> {
        let session = store
            .get_session(&session_id)?
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        // Replay reconstructs state from a bounded newest-event window. Reading
        // the full log was superlinear and decoded every payload under the
        // global SQLite lock; truncation is now explicit.
        const REPLAY_EVENT_WINDOW: u32 = 20_000;
        let events = store.list_recent_events_window(&session_id, REPLAY_EVENT_WINDOW)?;
        let total_events = store.event_count(&session_id)?;
        let event_window_truncated = total_events > events.len() as u64;
        let policy = store.get_orchestration_policy(&session.project_id)?;
        let critical = store.get_critical_path_policy(&session.project_id)?;
        let policy_hash =
            crate::runtime_pin::read_policy_inputs(&store, &session.project_id)?.hash();
        let mut report = replay(
            &session,
            &events,
            &policy,
            &critical,
            limit,
            event_window_truncated,
        );
        report.policy_hash = policy_hash;
        Ok(report)
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use vp_protocol::scaling::ScalingAction;
    use vp_protocol::types::{PlanDepartment, PlanTask};

    fn task(id: &str, deps: &[&str]) -> PlanTask {
        PlanTask {
            id: id.to_string(),
            department: "impl".to_string(),
            title: id.to_string(),
            kind: "coding".to_string(),
            priority: 0,
            profile: Some("coding".to_string()),
            deps: deps.iter().map(|dep| dep.to_string()).collect(),
            write_scope: vec![format!("src/{id}.rs")],
            acceptance: vec!["done".to_string()],
            token_budget: None,
            cost_budget_usd: None,
            parent_task_id: None,
            fanout_key: None,
            fanout_join_for: None,
        }
    }

    fn plan() -> TeamPlan {
        TeamPlan {
            goal: "replay".to_string(),
            departments: vec![PlanDepartment {
                name: "impl".to_string(),
                manager_title: "Implementation".to_string(),
                profile: "coding".to_string(),
                objective: "build".to_string(),
                token_budget: None,
                cost_budget_usd: None,
            }],
            tasks: vec![task("T1", &[]), task("T2", &["T1"])],
        }
    }

    fn event(seq: u64, kind: &str, payload: Value) -> Event {
        Event {
            session_id: "ses-1".to_string(),
            seq,
            kind: kind.to_string(),
            payload,
            created_at: format!("2026-11-12T00:00:{:02}Z", seq % 60),
        }
    }

    fn session() -> Session {
        Session {
            id: "ses-1".to_string(),
            project_id: "prj-1".to_string(),
            goal: "replay".to_string(),
            status: "idle".to_string(),
            priority: 0,
            worktree: None,
            branch: None,
            token_budget: None,
            cost_budget_usd: None,
            time_budget_secs: None,
            unlimited: false,
            cost_spent_usd: 0.0,
            base_commit: None,
            tokens_in: 0,
            tokens_out: 0,
            created_at: "2026-11-12T00:00:00Z".to_string(),
            prompt_layout: 3,
        }
    }

    #[test]
    fn reconstruction_matches_plan_events() {
        let plan = plan();
        let events = vec![
            event(1, "team_plan.submitted", json!({"plan": plan})),
            event(2, "plan_task.started", json!({"task_id": "T1"})),
            event(
                3,
                "plan_task.finished",
                json!({"task_id": "T1", "task_status": "completed"}),
            ),
        ];
        let rows = scheduled_at(&plan, "prj-1", 1, "2026-11-12T00:00:01Z", &events, 4);
        assert_eq!(rows[0].status, "completed");
        assert_eq!(rows[0].attempts, 1);
        assert_eq!(rows[1].status, "pending");
    }

    #[test]
    fn dispatch_replay_detects_an_unready_start() {
        let plan = plan();
        let events = vec![
            event(1, "team_plan.submitted", json!({"plan": plan.clone()})),
            event(2, "plan_task.started", json!({"task_id": "T2"})),
        ];
        let entry = replay_dispatch_decision(
            &session(),
            &events,
            &events[1],
            &CriticalPathPolicy::default(),
        );
        assert_eq!(entry.status, ControllerReplayStatus::Mismatched);
        assert!(
            entry
                .changes
                .iter()
                .any(|change| change.field == "dispatch_readiness")
        );
    }

    #[test]
    fn ready_order_respects_dependencies_and_priority() {
        let mut plan = plan();
        plan.tasks[0].priority = 5;
        let rows = scheduled_at(
            &plan,
            "prj-1",
            1,
            "2026-11-12T00:00:00Z",
            &[event(
                1,
                "team_plan.submitted",
                json!({"plan": plan.clone()}),
            )],
            2,
        );
        let ready = ready_task_order(
            &plan,
            &rows,
            &session(),
            OffsetDateTime::parse("2026-11-12T00:10:00Z", &Rfc3339).unwrap(),
        );
        assert_eq!(ready, vec!["T1".to_string()]);
    }

    #[test]
    fn old_scaling_decisions_are_skipped_not_guessed() {
        let mut decision = ScalingDecision {
            session_id: "ses-1".to_string(),
            project_id: "prj-1".to_string(),
            action: ScalingAction::ExpandWorkers,
            ..Default::default()
        };
        decision.replay_inputs = None;
        let event = event(
            4,
            "scaling.decision",
            serde_json::to_value(decision).unwrap(),
        );
        let entry = replay_scaling_decision(&event);
        assert_eq!(entry.status, ControllerReplayStatus::Skipped);
        assert!(
            entry
                .reason
                .unwrap()
                .contains("scaling_inputs_not_recorded")
        );
    }

    #[test]
    fn scaling_replay_matches_and_detects_a_changed_action() {
        let policy: vp_protocol::scaling::ScalingPolicy =
            serde_json::from_value(json!({})).unwrap();
        let inputs = ScalingReplayInputs {
            policy: policy.clone(),
            session_status: "running".to_string(),
            plan_revision: 1,
            tasks: vec![vp_protocol::scaling::ScalingTaskObservation {
                task_id: "T1".to_string(),
                department: "impl".to_string(),
                priority: 0,
                status: "pending".to_string(),
                deps: Vec::new(),
                write_scope: vec!["src/T1.rs".to_string()],
                ready_since: Some("2026-11-12T00:00:00Z".to_string()),
            }],
            current_workers: 2,
            current_labour: 4,
            active_workers: 0,
            active_labour: 0,
            global_worker_available: 10,
            global_labour_available: 40,
            budget_headroom_ratio: None,
            last_action_at: None,
            last_plan_revision: Some(1),
            idle_since_workers: None,
            idle_since_labour: None,
            now: "2026-11-12T00:10:00Z".to_string(),
            hard_worker_cap: 10,
            hard_labour_cap: 40,
        };
        let observation = crate::scaling::observation_from_replay_inputs(&inputs).unwrap();
        let recomputed = crate::scaling::evaluate(&policy, &observation);
        let mut decision = recomputed.clone();
        decision.session_id = "ses-1".to_string();
        decision.project_id = "prj-1".to_string();
        decision.replay_inputs = Some(serde_json::to_value(&inputs).unwrap());
        let recorded_event = event(
            5,
            "scaling.decision",
            serde_json::to_value(decision).unwrap(),
        );
        let entry = replay_scaling_decision(&recorded_event);
        assert_eq!(entry.status, ControllerReplayStatus::Matched);
        assert_eq!(
            entry.recorded_action.as_deref(),
            Some(recomputed.action.as_str())
        );

        let mut changed = recomputed;
        changed.action = ScalingAction::ScaleDownLabour;
        changed.session_id = "ses-1".to_string();
        changed.project_id = "prj-1".to_string();
        changed.replay_inputs = Some(serde_json::to_value(&inputs).unwrap());
        let recorded_event = event(
            6,
            "scaling.decision",
            serde_json::to_value(changed).unwrap(),
        );
        let entry = replay_scaling_decision(&recorded_event);
        assert_eq!(entry.status, ControllerReplayStatus::Mismatched);
        assert!(entry.changes.iter().any(|change| change.field == "action"));
    }
}

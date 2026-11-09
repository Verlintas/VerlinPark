use std::collections::{HashMap, HashSet};

use anyhow::{Result, anyhow};
use vp_protocol::types::{Event, PlanRevisionInfo, PlanTask, ScheduledTask, TeamPlan};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlanDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<String>,
}

pub fn diff_plan(old: &TeamPlan, new: &TeamPlan) -> PlanDiff {
    let old_by_id = old
        .tasks
        .iter()
        .map(|task| (task.id.as_str(), task))
        .collect::<HashMap<_, _>>();
    let new_by_id = new
        .tasks
        .iter()
        .map(|task| (task.id.as_str(), task))
        .collect::<HashMap<_, _>>();
    let mut added = new
        .tasks
        .iter()
        .filter(|task| !old_by_id.contains_key(task.id.as_str()))
        .map(|task| task.id.clone())
        .collect::<Vec<_>>();
    let mut removed = old
        .tasks
        .iter()
        .filter(|task| !new_by_id.contains_key(task.id.as_str()))
        .map(|task| task.id.clone())
        .collect::<Vec<_>>();
    let mut changed = new
        .tasks
        .iter()
        .filter(|task| {
            old_by_id
                .get(task.id.as_str())
                .is_some_and(|old_task| *old_task != *task)
        })
        .map(|task| task.id.clone())
        .collect::<Vec<_>>();
    added.sort();
    removed.sort();
    changed.sort();
    PlanDiff {
        added,
        removed,
        changed,
    }
}

/// Validate that a plan revision does not delete or rewrite work that has
/// already started or completed. Failed/blocked/pending work may be revised.
pub fn ensure_revision_safe(
    old: &TeamPlan,
    new: &TeamPlan,
    statuses: &HashMap<String, String>,
) -> Result<()> {
    let diff = diff_plan(old, new);
    let old_by_id = old
        .tasks
        .iter()
        .map(|task| (task.id.as_str(), task))
        .collect::<HashMap<_, _>>();
    let new_by_id = new
        .tasks
        .iter()
        .map(|task| (task.id.as_str(), task))
        .collect::<HashMap<_, _>>();
    let protected = |task_id: &str| {
        matches!(
            statuses.get(task_id).map(String::as_str),
            Some(
                "running"
                    | "queued"
                    | "paused"
                    | "pausing"
                    | "cancelling"
                    | "waiting_input"
                    | "completed"
                    | "skipped"
            )
        )
    };
    for task_id in &diff.removed {
        if protected(task_id) {
            return Err(anyhow!(
                "cannot remove task {task_id}: it is active or already completed"
            ));
        }
    }
    for task_id in &diff.changed {
        if protected(task_id) {
            return Err(anyhow!(
                "cannot rewrite task {task_id}: it is active or already completed"
            ));
        }
        let old_task = old_by_id[task_id.as_str()];
        let new_task = new_by_id[task_id.as_str()];
        if old_task.department != new_task.department
            && !matches!(
                statuses.get(task_id).map(String::as_str),
                Some("pending" | "failed" | "blocked" | "cancelled" | "interrupted")
            )
        {
            return Err(anyhow!(
                "cannot move task {task_id} to another department while active"
            ));
        }
    }
    Ok(())
}

pub fn plan_revision(events: &[Event]) -> u32 {
    events
        .iter()
        .filter(|event| {
            matches!(
                event.kind.as_str(),
                "team_plan.submitted" | "team_plan.revised"
            )
        })
        .count()
        .min(u32::MAX as usize) as u32
}

pub fn revisions_from_events(events: &[Event]) -> Vec<PlanRevisionInfo> {
    let mut revisions = Vec::new();
    let mut revision = 0u32;
    for event in events {
        if !matches!(
            event.kind.as_str(),
            "team_plan.submitted" | "team_plan.revised"
        ) {
            continue;
        }
        revision = revision.saturating_add(1);
        let plan = plan_from_event(event);
        let task_count = plan
            .as_ref()
            .map(|plan| plan.tasks.len())
            .or_else(|| {
                event
                    .payload
                    .get("task_count")
                    .and_then(serde_json::Value::as_u64)
                    .map(|count| count as usize)
            })
            .unwrap_or(0);
        let reason = event
            .payload
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .filter(|reason| !reason.trim().is_empty());
        let author = event
            .payload
            .get("author")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("leader")
            .to_string();
        let list = |key: &str| {
            event
                .payload
                .get(key)
                .and_then(serde_json::Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        revisions.push(PlanRevisionInfo {
            revision: event
                .payload
                .get("revision")
                .and_then(serde_json::Value::as_u64)
                .map(|value| value as u32)
                .unwrap_or(revision),
            seq: event.seq,
            kind: event.kind.clone(),
            reason,
            author,
            task_count,
            added: list("added"),
            removed: list("removed"),
            changed: list("changed"),
            created_at: event.created_at.clone(),
        });
    }
    revisions
}

pub fn plan_from_event(event: &Event) -> Option<TeamPlan> {
    event
        .payload
        .get("plan")
        .cloned()
        .or_else(|| {
            if event.kind == "team_plan.submitted" && event.payload.get("tasks").is_some() {
                Some(event.payload.clone())
            } else {
                None
            }
        })
        .and_then(|value| serde_json::from_value(value).ok())
}

/// Effective status projection from task rows plus plan lifecycle events.
pub fn status_map(
    plan: &TeamPlan,
    scheduled: &[ScheduledTask],
    events: &[Event],
) -> HashMap<String, String> {
    let mut statuses = plan
        .tasks
        .iter()
        .map(|task| (task.id.clone(), "pending".to_string()))
        .collect::<HashMap<_, _>>();
    for event in events {
        if !event.kind.starts_with("plan_task.") {
            continue;
        }
        let Some(task_id) = event
            .payload
            .get("task_id")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let status = match event.kind.as_str() {
            "plan_task.started" => "running",
            "plan_task.retry_scheduled" | "plan_task.retried" => "pending",
            "plan_task.interrupted" => "interrupted",
            "plan_task.skipped" => "skipped",
            "plan_task.blocked" => "blocked",
            "plan_task.unblocked" => "pending",
            "plan_task.cancelled" => "cancelled",
            "plan_task.cancelling" => "cancelling",
            "plan_task.pausing" => "pausing",
            "plan_task.paused" => "paused",
            "plan_task.resumed" => "running",
            _ => event
                .payload
                .get("task_status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("pending"),
        };
        statuses.insert(task_id.to_string(), status.to_string());
    }
    // Task rows are the authoritative scheduling projection. Events are a
    // fallback for rows that have not been synced yet, because a finish can be
    // recorded in the row and event in separate transactions during recovery.
    for task in scheduled {
        statuses.insert(task.task_id.clone(), task.status.clone());
    }
    statuses
}

pub fn blocked_by(task: &PlanTask, statuses: &HashMap<String, String>) -> Vec<String> {
    task.deps
        .iter()
        .filter(|dep| {
            !matches!(
                statuses.get(*dep).map(String::as_str),
                Some("completed" | "skipped")
            )
        })
        .map(|dep| {
            format!(
                "{} ({})",
                dep,
                statuses.get(dep).map(String::as_str).unwrap_or("pending")
            )
        })
        .collect()
}

pub fn ready_tasks<'a>(
    plan: &'a TeamPlan,
    statuses: &HashMap<String, String>,
    limit: usize,
) -> Vec<&'a PlanTask> {
    plan.tasks
        .iter()
        .filter(|task| {
            let status = statuses
                .get(&task.id)
                .map(String::as_str)
                .unwrap_or("pending");
            matches!(
                status,
                "pending" | "interrupted" | "failed" | "cancelled" | "retry_scheduled"
            ) && task.deps.iter().all(|dep| {
                matches!(
                    statuses.get(dep).map(String::as_str),
                    Some("completed" | "skipped")
                )
            })
        })
        .take(limit.max(1))
        .collect()
}

pub fn latest_result_summary(events: &[Event], task_id: &str) -> Option<String> {
    events.iter().rev().find_map(|event| {
        if event.kind != "plan_task.finished"
            || event
                .payload
                .get("task_id")
                .and_then(serde_json::Value::as_str)
                != Some(task_id)
        {
            return None;
        }
        event
            .payload
            .get("summary")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                event
                    .payload
                    .get("reason")
                    .and_then(serde_json::Value::as_str)
            })
            .map(str::to_string)
    })
}

pub fn task_ids(plan: &TeamPlan) -> HashSet<String> {
    plan.tasks.iter().map(|task| task.id.clone()).collect()
}

//! Cross-project management snapshot for the macOS Manage Dash.
//!
//! This is a read-only aggregation over sessions, approvals, questions, task
//! queues, checkpoints, merge queues, and fork metadata. It deliberately does
//! not run Doctor, fork audit, or provider calls; those remain explicit actions.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::Result;
use vp_protocol::types::{
    DashboardAlert, DashboardCount, DashboardProjectSummary, DashboardResult, DashboardSummary,
    MergeQueueEntry,
};

use crate::Engine;

const MAX_ALERTS: usize = 100;

impl Engine {
    pub async fn dashboard_snapshot(&self, project_id: Option<&str>) -> Result<DashboardResult> {
        let projects = self.list_projects().await?;
        if let Some(project_id) = project_id
            && !projects.iter().any(|project| project.id == project_id)
        {
            anyhow::bail!("project not found: {project_id}");
        }
        let sessions = self.list_sessions(project_id.map(str::to_string)).await?;
        let session_ids = sessions
            .iter()
            .map(|session| session.id.clone())
            .collect::<HashSet<_>>();
        let session_projects = sessions
            .iter()
            .map(|session| (session.id.clone(), session.project_id.clone()))
            .collect::<HashMap<_, _>>();
        let session_goals = sessions
            .iter()
            .map(|session| (session.id.clone(), session.goal.clone()))
            .collect::<HashMap<_, _>>();
        let mut project_summaries = projects
            .iter()
            .map(|project| {
                (
                    project.id.clone(),
                    DashboardProjectSummary {
                        project_id: project.id.clone(),
                        name: project.name.clone(),
                        sessions: 0,
                        running_sessions: 0,
                        queued_sessions: 0,
                        failed_sessions: 0,
                        pending_approvals: 0,
                        pending_questions: 0,
                        tasks: 0,
                        running_tasks: 0,
                        checkpoints: 0,
                        dirty_checkpoints: 0,
                        fork_roots: 0,
                        fork_sessions: 0,
                    },
                )
            })
            .collect::<HashMap<_, _>>();

        let mut sessions_by_status: BTreeMap<String, usize> = BTreeMap::new();
        for session in &sessions {
            *sessions_by_status
                .entry(session.status.clone())
                .or_default() += 1;
            if let Some(summary) = project_summaries.get_mut(&session.project_id) {
                summary.sessions += 1;
                match session.status.as_str() {
                    "running" => summary.running_sessions += 1,
                    "queued" => summary.queued_sessions += 1,
                    "failed" => summary.failed_sessions += 1,
                    _ => {}
                }
            }
        }
        let running_sessions = sessions_by_status.get("running").copied().unwrap_or(0);
        let queued_sessions = sessions_by_status.get("queued").copied().unwrap_or(0);

        let queue = self
            .task_queue(project_id.map(str::to_string), None)
            .await?;
        let mut tasks_by_status: BTreeMap<String, usize> = BTreeMap::new();
        for task in &queue.tasks {
            *tasks_by_status.entry(task.status.clone()).or_default() += 1;
            if let Some(summary) = project_summaries.get_mut(&task.project_id) {
                summary.tasks += 1;
                if task.status == "running" {
                    summary.running_tasks += 1;
                }
            }
        }
        let running_tasks = tasks_by_status.get("running").copied().unwrap_or(0);

        let approvals = self
            .list_approvals(None)
            .await?
            .into_iter()
            .filter(|approval| {
                approval.status == "pending" && session_ids.contains(&approval.session_id)
            })
            .collect::<Vec<_>>();
        let questions = self
            .list_questions(None)
            .await?
            .questions
            .into_iter()
            .filter(|question| {
                question.status == "pending" && session_ids.contains(&question.session_id)
            })
            .collect::<Vec<_>>();

        let mut checkpoints = 0usize;
        let mut dirty_checkpoints = 0usize;
        for session in &sessions {
            let active = self.active_checkpoints(&session.id).await?;
            let dirty = active.iter().filter(|checkpoint| checkpoint.dirty).count();
            checkpoints = checkpoints.saturating_add(active.len());
            dirty_checkpoints = dirty_checkpoints.saturating_add(dirty);
            if let Some(summary) = project_summaries.get_mut(&session.project_id) {
                summary.checkpoints += active.len();
                summary.dirty_checkpoints += dirty;
            }
        }

        let mut fork_sessions = 0usize;
        for session in &sessions {
            let store = self.store().clone();
            let id = session.id.clone();
            let events = tokio::task::spawn_blocking(move || {
                store.list_events_by_kind(&id, "session.created", 1)
            })
            .await??;
            let parent = events
                .first()
                .and_then(|event| event.payload.get("forked_from_session_id"))
                .and_then(serde_json::Value::as_str);
            if parent.is_some_and(|parent| session_ids.contains(parent)) {
                fork_sessions += 1;
                if let Some(summary) = project_summaries.get_mut(&session.project_id) {
                    summary.fork_sessions += 1;
                }
            }
        }
        let fork_roots = sessions.len().saturating_sub(fork_sessions);

        let mut alerts = Vec::new();
        for approval in &approvals {
            if let Some(project_id) = session_projects.get(&approval.session_id)
                && let Some(summary) = project_summaries.get_mut(project_id)
            {
                summary.pending_approvals += 1;
            }
            alerts.push(alert(
                AlertKey {
                    id: format!("approval:{}", approval.id),
                    kind: "approval",
                    severity: "warning",
                    action: "inbox",
                },
                format!("Approval pending: {}", approval.tool),
                format!(
                    "{} · tool call {}",
                    session_goals
                        .get(&approval.session_id)
                        .map(String::as_str)
                        .unwrap_or("session"),
                    approval.tool_call_id
                ),
                session_projects.get(&approval.session_id).cloned(),
                Some(approval.session_id.clone()),
            ));
        }
        for question in &questions {
            if let Some(project_id) = session_projects.get(&question.session_id)
                && let Some(summary) = project_summaries.get_mut(project_id)
            {
                summary.pending_questions += 1;
            }
            alerts.push(alert(
                AlertKey {
                    id: format!("question:{}", question.id),
                    kind: "question",
                    severity: "warning",
                    action: "inbox",
                },
                "Question waiting for an answer".to_string(),
                format!(
                    "{} · {}",
                    session_goals
                        .get(&question.session_id)
                        .map(String::as_str)
                        .unwrap_or("session"),
                    clip(&question.question, 160)
                ),
                session_projects.get(&question.session_id).cloned(),
                Some(question.session_id.clone()),
            ));
        }
        for session in &sessions {
            if session.status == "failed" {
                alerts.push(alert(
                    AlertKey {
                        id: format!("session-failed:{}", session.id),
                        kind: "session_failed",
                        severity: "critical",
                        action: "session",
                    },
                    "Session failed".to_string(),
                    format!("{} · {}", session.goal, session.status),
                    Some(session.project_id.clone()),
                    Some(session.id.clone()),
                ));
            } else if session.status == "queued" {
                alerts.push(alert(
                    AlertKey {
                        id: format!("session-queued:{}", session.id),
                        kind: "session_queued",
                        severity: "info",
                        action: "session",
                    },
                    "Session queued for admission".to_string(),
                    session.goal.clone(),
                    Some(session.project_id.clone()),
                    Some(session.id.clone()),
                ));
            }
        }
        for task in &queue.tasks {
            if matches!(task.status.as_str(), "failed" | "blocked" | "interrupted") {
                let severity = if task.status == "failed" {
                    "critical"
                } else {
                    "warning"
                };
                alerts.push(alert(
                    AlertKey {
                        id: format!("task:{}:{}", task.session_id, task.task_id),
                        kind: "task",
                        severity,
                        action: "team",
                    },
                    format!("Task {}: {}", task.status, task.title),
                    task.last_error
                        .clone()
                        .unwrap_or_else(|| format!("{} · {}", task.department, task.task_id)),
                    Some(task.project_id.clone()),
                    Some(task.session_id.clone()),
                ));
            }
        }
        for session in &sessions {
            let entries = self.list_merge_queue(&session.id).await?;
            for entry in entries {
                if matches!(entry.status.as_str(), "conflict" | "failed" | "interrupted") {
                    alerts.push(merge_alert(
                        &entry,
                        session,
                        session_projects.get(&session.id).cloned(),
                    ));
                }
            }
        }
        let severity_rank = |severity: &str| match severity {
            "critical" => 0,
            "warning" => 1,
            _ => 2,
        };
        alerts.sort_by(|left, right| {
            severity_rank(&left.severity)
                .cmp(&severity_rank(&right.severity))
                .then_with(|| left.id.cmp(&right.id))
        });
        let alerts_truncated = alerts.len() > MAX_ALERTS;
        alerts.truncate(MAX_ALERTS);

        let mut project_summaries = project_summaries
            .into_values()
            .filter(|summary| project_id.is_none_or(|project_id| summary.project_id == project_id))
            .collect::<Vec<_>>();
        for summary in &mut project_summaries {
            summary.fork_roots = summary.sessions.saturating_sub(summary.fork_sessions);
        }
        project_summaries.sort_by(|left, right| {
            activity_score(right)
                .cmp(&activity_score(left))
                .then_with(|| left.name.cmp(&right.name))
        });

        Ok(DashboardResult {
            summary: DashboardSummary {
                generated_at: vp_protocol::now_rfc3339(),
                project_id: project_id.map(str::to_string),
                projects: if project_id.is_some() {
                    1
                } else {
                    projects.len()
                },
                sessions: sessions.len(),
                sessions_by_status: counts(sessions_by_status),
                pending_approvals: approvals.len(),
                pending_questions: questions.len(),
                running_sessions,
                queued_sessions,
                tasks: queue.tasks.len(),
                tasks_by_status: counts(tasks_by_status),
                running_tasks,
                write_leases: queue.leases.len(),
                checkpoints,
                dirty_checkpoints,
                fork_roots,
                fork_sessions,
                project_summaries,
                agent: None,
            },
            alerts,
            alerts_truncated,
        })
    }
}

fn activity_score(summary: &DashboardProjectSummary) -> usize {
    summary.pending_approvals
        + summary.pending_questions
        + summary.running_sessions
        + summary.queued_sessions
        + summary.failed_sessions
        + summary.running_tasks
}

fn counts(values: BTreeMap<String, usize>) -> Vec<DashboardCount> {
    values
        .into_iter()
        .map(|(label, count)| DashboardCount { label, count })
        .collect()
}

struct AlertKey<'a> {
    id: String,
    kind: &'a str,
    severity: &'a str,
    action: &'a str,
}

fn alert(
    key: AlertKey<'_>,
    title: String,
    detail: String,
    project_id: Option<String>,
    session_id: Option<String>,
) -> DashboardAlert {
    DashboardAlert {
        id: key.id,
        kind: key.kind.to_string(),
        severity: key.severity.to_string(),
        title,
        detail: clip(&detail, 240),
        project_id,
        session_id,
        action: key.action.to_string(),
    }
}

fn merge_alert(
    entry: &MergeQueueEntry,
    session: &vp_protocol::types::Session,
    project_id: Option<String>,
) -> DashboardAlert {
    alert(
        AlertKey {
            id: format!("merge:{}", entry.id),
            kind: "merge_queue",
            severity: "critical",
            action: "git",
        },
        format!("Merge queue {}: {}", entry.status, entry.branch),
        entry
            .detail
            .clone()
            .unwrap_or_else(|| format!("{} · attempt {}", session.goal, entry.attempt)),
        project_id,
        Some(session.id.clone()),
    )
}

fn clip(value: &str, max: usize) -> String {
    let value = value.trim();
    if value.chars().count() <= max {
        value.to_string()
    } else {
        value.chars().take(max).collect::<String>() + "…"
    }
}

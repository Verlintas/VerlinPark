//! Deterministic helpers for deferred session continuations.
//!
//! The store owns persistence and the agent service owns the scheduler tick;
//! this module only contains the pure rules so creation validation, wake
//! evaluation, and prompt rendering are easy to test without a provider.

use anyhow::{Result, anyhow};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use vp_protocol::types::{ContinuationOutcome, ScheduledTask};

pub const MAX_BODY_CHARS: usize = 4_000;
pub const MAX_WATCH_TASKS: usize = 100;
pub const MAX_DEADLINE_SECS: i64 = 30 * 24 * 60 * 60;
pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;
pub const MAX_CONTEXT_NOTE_CHARS: usize = 2_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeEvaluation {
    /// `watch` or `deadline`.
    pub reason: String,
    pub outcomes: Vec<ContinuationOutcome>,
}

/// A watched task is settled once it reaches any terminal plan-task status.
/// `interrupted` counts as terminal failure for wake purposes.
pub fn is_terminal_task_status(status: &str) -> bool {
    matches!(
        status,
        "completed" | "failed" | "cancelled" | "skipped" | "interrupted"
    )
}

pub fn validate_body(body: &str) -> Result<String> {
    let body = body.trim();
    if body.is_empty() || body.chars().count() > MAX_BODY_CHARS {
        return Err(anyhow!("continuation body must be 1-4000 chars"));
    }
    Ok(body.to_string())
}

pub fn normalize_watch_mode(value: Option<&str>) -> Result<String> {
    let mode = value.unwrap_or("all").trim().to_ascii_lowercase();
    match mode.as_str() {
        "all" | "any" => Ok(mode),
        _ => Err(anyhow!("watch mode must be 'all' or 'any'")),
    }
}

pub fn normalize_max_attempts(value: Option<u32>) -> u32 {
    value
        .unwrap_or(DEFAULT_MAX_ATTEMPTS)
        .clamp(1, DEFAULT_MAX_ATTEMPTS)
}

/// Validate a relative or absolute deadline and return an RFC3339 timestamp.
/// Deadlines must be in the future and no more than 30 days away.
pub fn resolve_deadline(
    after_secs: Option<u64>,
    at: Option<&str>,
    now: OffsetDateTime,
) -> Result<Option<String>> {
    if after_secs.is_some() && at.is_some() {
        return Err(anyhow!(
            "pass either after_secs or an RFC3339 deadline, not both"
        ));
    }
    let deadline = if let Some(seconds) = after_secs {
        if seconds == 0 {
            return Err(anyhow!("deadline must be in the future"));
        }
        let seconds = i64::try_from(seconds).map_err(|_| anyhow!("deadline is too far away"))?;
        if seconds > MAX_DEADLINE_SECS {
            return Err(anyhow!("deadline must be within 30 days"));
        }
        now + time::Duration::seconds(seconds)
    } else if let Some(at) = at {
        let parsed = OffsetDateTime::parse(at.trim(), &Rfc3339)
            .map_err(|_| anyhow!("deadline must be an RFC3339 timestamp"))?;
        if parsed <= now {
            return Err(anyhow!("deadline must be in the future"));
        }
        if (parsed - now).whole_seconds() > MAX_DEADLINE_SECS {
            return Err(anyhow!("deadline must be within 30 days"));
        }
        parsed
    } else {
        return Ok(None);
    };
    deadline
        .format(&Rfc3339)
        .map(Some)
        .map_err(|error| anyhow!("formatting deadline: {error}"))
}

/// Evaluate whether a pending continuation should wake now. Watch tasks win
/// over a simultaneous deadline so an explicit task signal carries its
/// outcome summary; otherwise whichever condition is true provides the wake.
pub fn evaluate_wake(
    watch_task_ids: &[String],
    watch_mode: &str,
    deadline_at: Option<&str>,
    tasks: &[ScheduledTask],
    now: OffsetDateTime,
) -> Option<WakeEvaluation> {
    let outcomes = snapshot_outcomes(watch_task_ids, tasks);
    if !outcomes.is_empty() {
        let settled = match watch_mode {
            "any" => outcomes.iter().any(|outcome| {
                is_terminal_task_status(&outcome.status) || outcome.status == "missing"
            }),
            _ => outcomes.iter().all(|outcome| {
                is_terminal_task_status(&outcome.status) || outcome.status == "missing"
            }),
        };
        if settled {
            return Some(WakeEvaluation {
                reason: "watch".to_string(),
                outcomes,
            });
        }
    }
    let deadline_due = deadline_at
        .and_then(|deadline| OffsetDateTime::parse(deadline, &Rfc3339).ok())
        .is_some_and(|deadline| deadline <= now);
    if deadline_due {
        return Some(WakeEvaluation {
            reason: "deadline".to_string(),
            outcomes,
        });
    }
    None
}

/// Snapshot watched tasks in the operator's original watch order. A task that
/// disappeared with a plan revision is reported as `missing` and treated as
/// settled so a continuation cannot hang forever on a stale id.
pub fn snapshot_outcomes(
    watch_task_ids: &[String],
    tasks: &[ScheduledTask],
) -> Vec<ContinuationOutcome> {
    watch_task_ids
        .iter()
        .map(|task_id| {
            let current = tasks
                .iter()
                .filter(|task| &task.task_id == task_id)
                .max_by_key(|task| task.plan_seq);
            match current {
                Some(task) => ContinuationOutcome {
                    task_id: task_id.clone(),
                    title: task.title.clone(),
                    status: task.status.clone(),
                },
                None => ContinuationOutcome {
                    task_id: task_id.clone(),
                    title: String::new(),
                    status: "missing".to_string(),
                },
            }
        })
        .collect()
}

/// Render the operator prompt. The prefix is a bounded context note; the
/// deferred body follows as ordinary user content. Nothing here is injected
/// into a system prompt.
pub fn render_prompt(
    id: &str,
    body: &str,
    reason: &str,
    outcomes: &[ContinuationOutcome],
) -> String {
    let mut note = format!(
        "[Deferred continuation #{}]\nDeferred reason: {}\n",
        id, reason
    );
    if outcomes.is_empty() {
        note.push_str("Watched tasks: none\n");
    } else {
        note.push_str("Watched tasks:\n");
        for outcome in outcomes {
            let title = truncate_chars(&outcome.title, 120);
            if title.is_empty() {
                note.push_str(&format!("- {}: {}\n", outcome.task_id, outcome.status));
            } else {
                note.push_str(&format!(
                    "- {} \"{}\": {}\n",
                    outcome.task_id, title, outcome.status
                ));
            }
        }
    }
    note = truncate_chars(&note, MAX_CONTEXT_NOTE_CHARS);
    note.push_str("\nOperator prompt:\n");
    note.push_str(body);
    note
}

fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let mut truncated = value
        .chars()
        .take(limit.saturating_sub(1))
        .collect::<String>();
    truncated.push('…');
    truncated
}

//! Pure helpers for session assignments, identity leases, and the project
//! agent inbox.
//!
//! The store owns transactions; this module validates the append-only
//! coordination stream, derives the projection, compares it with stored rows,
//! and renders the bounded summaries used by an optional wake continuation.
//! Nothing here calls a provider, starts a run, holds a Worker slot, or changes
//! the role/profile execution gate.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use vp_protocol::types::{
    AGENT_ASSIGNMENT_EVENT_KINDS, AGENT_INBOX_EVENT_KINDS, AGENT_INBOX_MAX_SUMMARY_CHARS,
    AGENT_INBOX_STATUSES, AgentAssignment, AgentCoordinationError, AgentCoordinationEvent,
    AgentCoordinationReplayReport, AgentInboxMessage, agent_identity_role,
};

pub use vp_protocol::types::agent_coordination_codes as codes;

fn typed(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    AgentCoordinationError::new(code, message).into()
}

fn event_assignment(event: &AgentCoordinationEvent) -> Result<AgentAssignment> {
    if !AGENT_ASSIGNMENT_EVENT_KINDS.contains(&event.kind.as_str()) {
        return Err(typed(
            codes::COORDINATION_REPLAY_INVALID,
            format!("unknown assignment event kind: {}", event.kind),
        ));
    }
    let assignment: AgentAssignment =
        serde_json::from_value(event.payload.get("assignment").cloned().ok_or_else(|| {
            typed(
                codes::COORDINATION_REPLAY_INVALID,
                format!("assignment event {} has no snapshot", event.id),
            )
        })?)
        .map_err(|error| typed(codes::COORDINATION_REPLAY_INVALID, error.to_string()))?;
    if agent_identity_role(&assignment.role).is_none() {
        return Err(typed(
            codes::COORDINATION_REPLAY_INVALID,
            format!("assignment event {} has an invalid role", event.id),
        ));
    }
    if !matches!(assignment.status.as_str(), "active" | "released" | "queued") {
        return Err(typed(
            codes::COORDINATION_REPLAY_INVALID,
            format!("assignment event {} has an invalid status", event.id),
        ));
    }
    match event.kind.as_str() {
        "agent.assignment.created" | "agent.assignment.renewed"
            if assignment.status != "active" =>
        {
            return Err(typed(
                codes::COORDINATION_REPLAY_INVALID,
                format!(
                    "assignment event {} does not carry an active snapshot",
                    event.id
                ),
            ));
        }
        "agent.assignment.released" | "agent.assignment.expired"
            if assignment.status != "released" =>
        {
            return Err(typed(
                codes::COORDINATION_REPLAY_INVALID,
                format!(
                    "assignment event {} does not carry a released snapshot",
                    event.id
                ),
            ));
        }
        _ => {}
    }
    if assignment.session_id.trim().is_empty() || assignment.identity_id.trim().is_empty() {
        return Err(typed(
            codes::COORDINATION_REPLAY_INVALID,
            format!("assignment event {} has an empty key", event.id),
        ));
    }
    if assignment.assigned_at.trim().is_empty() || assignment.heartbeat_at.trim().is_empty() {
        return Err(typed(
            codes::COORDINATION_REPLAY_INVALID,
            format!("assignment event {} is missing timestamps", event.id),
        ));
    }
    Ok(assignment)
}

fn event_inbox_message(event: &AgentCoordinationEvent) -> Result<AgentInboxMessage> {
    if !AGENT_INBOX_EVENT_KINDS.contains(&event.kind.as_str()) {
        return Err(typed(
            codes::COORDINATION_REPLAY_INVALID,
            format!("unknown inbox event kind: {}", event.kind),
        ));
    }
    let message: AgentInboxMessage =
        serde_json::from_value(event.payload.get("message").cloned().ok_or_else(|| {
            typed(
                codes::COORDINATION_REPLAY_INVALID,
                format!("inbox event {} has no snapshot", event.id),
            )
        })?)
        .map_err(|error| typed(codes::COORDINATION_REPLAY_INVALID, error.to_string()))?;
    if message.id.trim().is_empty() || message.project_id.trim().is_empty() {
        return Err(typed(
            codes::COORDINATION_REPLAY_INVALID,
            format!("inbox event {} has an empty key", event.id),
        ));
    }
    if !AGENT_INBOX_STATUSES.contains(&message.status.as_str()) {
        return Err(typed(
            codes::COORDINATION_REPLAY_INVALID,
            format!("inbox event {} has an invalid status", event.id),
        ));
    }
    if message.project_id != event.project_id {
        return Err(typed(
            codes::COORDINATION_REPLAY_INVALID,
            format!(
                "inbox event {} carries a snapshot for another project",
                event.id
            ),
        ));
    }
    match event.kind.as_str() {
        "agent.inbox.message" if message.status != "unread" => {
            return Err(typed(
                codes::COORDINATION_REPLAY_INVALID,
                format!("inbox message event {} is not unread", event.id),
            ));
        }
        "agent.inbox.read" if !matches!(message.status.as_str(), "read" | "done") => {
            return Err(typed(
                codes::COORDINATION_REPLAY_INVALID,
                format!("inbox read event {} is not read/done", event.id),
            ));
        }
        "agent.inbox.done" if message.status != "done" => {
            return Err(typed(
                codes::COORDINATION_REPLAY_INVALID,
                format!("inbox done event {} is not done", event.id),
            ));
        }
        "agent.inbox.expired" if message.status != "expired" => {
            return Err(typed(
                codes::COORDINATION_REPLAY_INVALID,
                format!("inbox expired event {} is not expired", event.id),
            ));
        }
        _ => {}
    }
    Ok(message)
}

fn validate_event_order(previous: Option<u64>, event: &AgentCoordinationEvent) -> Result<()> {
    if event.seq == 0 {
        return Err(typed(
            codes::COORDINATION_REPLAY_INVALID,
            format!("coordination event {} has seq 0", event.id),
        ));
    }
    match previous {
        Some(previous) if previous > event.seq => Err(typed(
            codes::COORDINATION_REPLAY_INVALID,
            format!(
                "coordination event stream is out of order at seq {}",
                event.seq
            ),
        )),
        Some(previous) if previous == event.seq => Err(typed(
            codes::COORDINATION_REPLAY_INVALID,
            format!("coordination event stream has duplicate seq {}", event.seq),
        )),
        _ => Ok(()),
    }
}

/// Fold the append-only project stream into the latest assignment row per
/// `(session_id, identity_id)`. Events must be supplied in project `seq` order.
pub fn assignments_from_events(events: &[AgentCoordinationEvent]) -> Result<Vec<AgentAssignment>> {
    let mut latest: BTreeMap<(String, String), AgentAssignment> = BTreeMap::new();
    let mut previous = None;
    for event in events {
        validate_event_order(previous, event)?;
        previous = Some(event.seq);
        if !AGENT_ASSIGNMENT_EVENT_KINDS.contains(&event.kind.as_str()) {
            continue;
        }
        let assignment = event_assignment(event)?;
        latest.insert(
            (
                assignment.session_id.clone(),
                assignment.identity_id.clone(),
            ),
            assignment,
        );
    }
    Ok(latest.into_values().collect())
}

/// Fold the append-only project stream into the latest inbox row per id.
pub fn inbox_messages_from_events(
    events: &[AgentCoordinationEvent],
) -> Result<Vec<AgentInboxMessage>> {
    let mut latest: BTreeMap<String, AgentInboxMessage> = BTreeMap::new();
    let mut previous = None;
    for event in events {
        validate_event_order(previous, event)?;
        previous = Some(event.seq);
        if !AGENT_INBOX_EVENT_KINDS.contains(&event.kind.as_str()) {
            continue;
        }
        let message = event_inbox_message(event)?;
        latest.insert(message.id.clone(), message);
    }
    Ok(latest.into_values().collect())
}

/// Validate one project's coordination stream and return both projections.
pub fn project_from_events(
    events: &[AgentCoordinationEvent],
) -> Result<(Vec<AgentAssignment>, Vec<AgentInboxMessage>)> {
    let assignments = assignments_from_events(events)?;
    let inbox = inbox_messages_from_events(events)?;
    Ok((assignments, inbox))
}

fn comparable_assignment(assignment: &AgentAssignment) -> AgentAssignment {
    // `lease_active` is a read-time derivation. Everything else, including the
    // lease expiry, is authoritative projection state.
    let mut assignment = assignment.clone();
    assignment.lease_active = false;
    assignment
}

/// Compare event-derived rows with stored rows. The internal lease token is not
/// part of the public assignment type, so it cannot cause a false mismatch.
pub fn compare_coordination_projection(
    expected_assignments: &[AgentAssignment],
    actual_assignments: &[AgentAssignment],
    expected_inbox: &[AgentInboxMessage],
    actual_inbox: &[AgentInboxMessage],
) -> Vec<String> {
    let mut mismatches = Vec::new();
    let expected_assignments = expected_assignments
        .iter()
        .map(|assignment| {
            (
                (
                    assignment.session_id.clone(),
                    assignment.identity_id.clone(),
                ),
                comparable_assignment(assignment),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let actual_assignments = actual_assignments
        .iter()
        .map(|assignment| {
            (
                (
                    assignment.session_id.clone(),
                    assignment.identity_id.clone(),
                ),
                comparable_assignment(assignment),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for key in expected_assignments
        .keys()
        .chain(actual_assignments.keys())
        .cloned()
        .collect::<BTreeSet<_>>()
    {
        match (expected_assignments.get(&key), actual_assignments.get(&key)) {
            (Some(expected), Some(actual)) if expected == actual => {}
            (Some(_), Some(actual)) => mismatches.push(format!(
                "assignment {}:{}: projection status {} does not match replay",
                key.0, key.1, actual.status
            )),
            (Some(_), None) => mismatches.push(format!(
                "assignment {}:{} exists in events without a row",
                key.0, key.1
            )),
            (None, Some(_)) => mismatches.push(format!(
                "assignment {}:{} exists without an event",
                key.0, key.1
            )),
            (None, None) => {}
        }
    }
    let expected_inbox = expected_inbox
        .iter()
        .map(|message| (message.id.clone(), message))
        .collect::<BTreeMap<_, _>>();
    let actual_inbox = actual_inbox
        .iter()
        .map(|message| (message.id.clone(), message))
        .collect::<BTreeMap<_, _>>();
    for id in expected_inbox
        .keys()
        .chain(actual_inbox.keys())
        .cloned()
        .collect::<BTreeSet<_>>()
    {
        match (expected_inbox.get(&id), actual_inbox.get(&id)) {
            (Some(expected), Some(actual)) if expected == actual => {}
            (Some(_), Some(actual)) => mismatches.push(format!(
                "inbox {id}: projection status {} does not match replay",
                actual.status
            )),
            (Some(_), None) => mismatches.push(format!("inbox {id}: event exists without a row")),
            (None, Some(_)) => mismatches.push(format!("inbox {id}: row exists without an event")),
            (None, None) => {}
        }
    }
    mismatches
}

pub fn replay_report(
    project_id: &str,
    assignments: usize,
    inbox: usize,
    events: usize,
    repaired: bool,
    mismatches: Vec<String>,
) -> AgentCoordinationReplayReport {
    AgentCoordinationReplayReport {
        project_id: project_id.to_string(),
        assignments: assignments as u64,
        inbox: inbox as u64,
        events: events as u64,
        consistent: mismatches.is_empty(),
        repaired,
        mismatches,
    }
}

/// Sessions that may receive an optional coordination wake. A running, queued,
/// paused, imported, or landed session is never woken by an assignment/inbox
/// side effect.
pub fn wake_eligible_status(status: &str) -> bool {
    matches!(status, "idle" | "failed" | "cancelled")
}

/// Whether a stored assignment's lease is live at `now`.
pub fn assignment_is_live_at(assignment: &AgentAssignment, now: OffsetDateTime) -> bool {
    assignment.status == "active"
        && assignment
            .lease_expires_at
            .as_deref()
            .and_then(|value| OffsetDateTime::parse(value, &Rfc3339).ok())
            .is_some_and(|expires| expires > now)
}

fn bounded_summary(payload: &Value, kind: &str) -> String {
    for key in [
        "summary", "title", "body", "question", "decision", "message",
    ] {
        if let Some(value) = payload.get(key).and_then(Value::as_str) {
            let normalized = value
                .chars()
                .map(|character| {
                    if character.is_control() {
                        ' '
                    } else {
                        character
                    }
                })
                .collect::<String>();
            let normalized = normalized.trim();
            if !normalized.is_empty() {
                return truncate_chars(normalized, AGENT_INBOX_MAX_SUMMARY_CHARS);
            }
        }
    }
    truncate_chars(kind, AGENT_INBOX_MAX_SUMMARY_CHARS)
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

/// Render the bounded body of an inbox wake continuation. This is operator
/// content, never a system prompt, and never a provider call by itself.
pub fn render_inbox_wake_body(messages: &[AgentInboxMessage]) -> String {
    let unread = messages.iter().filter(|m| m.status == "unread").count();
    let total = messages.len();
    let mut body =
        format!("[inbox] {total} unread project message(s) are waiting; {unread} still unread.\n");
    for message in messages.iter().take(5) {
        let label = if message.from_label.is_empty() {
            "operator"
        } else {
            message.from_label.as_str()
        };
        body.push_str(&format!(
            "- [{}] {} — {}\n",
            truncate_chars(&message.kind, 40),
            truncate_chars(label, 80),
            bounded_summary(&message.payload, &message.kind)
        ));
    }
    if total > 5 {
        body.push_str(&format!("- … and {} more\n", total - 5));
    }
    body.push_str("Read with `agent.inbox.list` before acting; this wake is cooperative only.\n");
    truncate_chars(&body, 2_000)
}

pub fn render_assignment_wake_body(assignment: &AgentAssignment) -> String {
    format!(
        "[inbox] assignment: identity {} is assigned to this session as {} until the identity lease expires ({}).\nThis wake is cooperative only; approvals, sandboxing, Verifier, and budgets are unchanged.\n",
        truncate_chars(&assignment.identity_id, 128),
        truncate_chars(&assignment.role, 32),
        assignment
            .lease_expires_at
            .as_deref()
            .unwrap_or("lease expiry not set")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn assignment(status: &str) -> AgentAssignment {
        AgentAssignment {
            session_id: "ses-1".into(),
            project_id: "prj-1".into(),
            identity_id: "id-1".into(),
            role: "worker".into(),
            status: status.into(),
            assigned_at: "2026-11-13T00:00:00Z".into(),
            released_at: None,
            heartbeat_at: "2026-11-13T00:00:00Z".into(),
            lease_expires_at: Some("2026-11-13T00:10:00Z".into()),
            lease_active: false,
        }
    }

    fn message(status: &str) -> AgentInboxMessage {
        AgentInboxMessage {
            id: "msg-1".into(),
            project_id: "prj-1".into(),
            identity_id: "id-1".into(),
            session_id: None,
            from_identity_id: None,
            from_label: "operator".into(),
            kind: "message".into(),
            payload: json!({"summary": "check the build"}),
            status: status.into(),
            created_at: "2026-11-13T00:00:00Z".into(),
            read_at: None,
            expires_at: None,
        }
    }

    fn event(id: &str, seq: u64, kind: &str, payload: Value) -> AgentCoordinationEvent {
        AgentCoordinationEvent {
            id: id.into(),
            project_id: "prj-1".into(),
            seq,
            kind: kind.into(),
            payload,
            created_at: "2026-11-13T00:00:00Z".into(),
        }
    }

    #[test]
    fn folds_assignment_and_inbox_latest_snapshots() {
        let active = assignment("active");
        let mut released = active.clone();
        released.status = "released".into();
        let events = vec![
            event(
                "ev-1",
                1,
                "agent.assignment.created",
                json!({"assignment": active}),
            ),
            event(
                "ev-2",
                2,
                "agent.assignment.released",
                json!({"assignment": released}),
            ),
            event(
                "ev-3",
                3,
                "agent.inbox.message",
                json!({"message": message("unread")}),
            ),
        ];
        let (assignments, inbox) = project_from_events(&events).unwrap();
        assert_eq!(assignments.len(), 1);
        assert_eq!(assignments[0].status, "released");
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].status, "unread");
    }

    #[test]
    fn rejects_duplicate_sequence_and_mismatched_snapshot_kind() {
        let active = assignment("active");
        let duplicate = vec![
            event(
                "ev-1",
                1,
                "agent.assignment.created",
                json!({"assignment": active}),
            ),
            event(
                "ev-2",
                1,
                "agent.assignment.renewed",
                json!({"assignment": active}),
            ),
        ];
        let error = assignments_from_events(&duplicate).unwrap_err();
        let typed = error
            .downcast_ref::<AgentCoordinationError>()
            .expect("typed error");
        assert_eq!(typed.code, codes::COORDINATION_REPLAY_INVALID);

        let wrong = vec![event(
            "ev-1",
            1,
            "agent.assignment.released",
            json!({"assignment": active}),
        )];
        assert!(assignments_from_events(&wrong).is_err());
    }

    #[test]
    fn comparison_ignores_the_time_relative_lease_flag() {
        let expected = assignment("active");
        let mut actual = expected.clone();
        actual.lease_active = true;
        assert!(
            compare_coordination_projection(
                std::slice::from_ref(&expected),
                &[actual],
                &[message("unread")],
                &[message("unread")]
            )
            .is_empty()
        );

        let mut changed = expected.clone();
        changed.status = "released".into();
        let mismatches =
            compare_coordination_projection(&[expected], &[changed], &[], &[message("unread")]);
        assert_eq!(mismatches.len(), 2);
        assert!(mismatches.iter().any(|line| line.contains("assignment")));
        assert!(mismatches.iter().any(|line| line.contains("inbox")));
    }

    #[test]
    fn wake_body_is_bounded_and_marks_cooperative_scope() {
        let messages = (0..8)
            .map(|index| {
                let mut message = message("unread");
                message.id = format!("msg-{index}");
                message.payload = json!({"summary": "x".repeat(1_000)});
                message
            })
            .collect::<Vec<_>>();
        let body = render_inbox_wake_body(&messages);
        assert!(body.starts_with("[inbox] 8 unread"));
        assert!(body.contains("… and 3 more"));
        assert!(body.contains("cooperative only"));
        assert!(body.chars().count() <= 2_000);

        let body = render_assignment_wake_body(&assignment("active"));
        assert!(body.starts_with("[inbox] assignment: identity id-1"));
        assert!(body.contains("cooperative only"));
    }

    #[test]
    fn only_idle_failed_and_cancelled_sessions_are_eligible_for_a_wake() {
        assert!(wake_eligible_status("idle"));
        assert!(wake_eligible_status("failed"));
        assert!(wake_eligible_status("cancelled"));
        assert!(!wake_eligible_status("running"));
        assert!(!wake_eligible_status("queued"));
        assert!(!wake_eligible_status("paused"));
        assert!(!wake_eligible_status("imported"));
        assert!(!wake_eligible_status("landed"));
    }
}

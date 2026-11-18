//! Session assignment, identity lease, and project-scoped agent inbox.
//!
//! These rows are durable project state, not run state. An assignment binds a
//! project identity to a session and carries a short lease; an inbox row is a
//! pull-based message to a Leader/Manager/Worker identity. Labour, Verifier, and
//! Scout have no project identity and therefore no project inbox.
//!
//! Every transition commits its projection row and one append-only
//! `agent_coordination_events` row in the same transaction. Events are
//! project-scoped because an identity lease is a cross-session invariant and an
//! inbox row belongs to an identity, not to the sender session. The event `id`
//! is portable across databases while `seq` is a per-project replay order.
//!
//! The identity lease is same-database SQLite coordination. It is not a
//! distributed lock, not cross-host consensus, and never a security credential.

use std::collections::{BTreeSet, HashMap};

use anyhow::{Result, anyhow};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;
use vp_protocol::types::{
    AGENT_ASSIGNMENT_EVENT_KINDS, AGENT_ASSIGNMENT_MAX_LIST, AGENT_ASSIGNMENT_STATUSES,
    AGENT_COORDINATION_MAX_EVENTS, AGENT_IDENTITY_ROLES, AGENT_INBOX_EVENT_KINDS,
    AGENT_INBOX_KINDS, AGENT_INBOX_MAX_LIST, AGENT_INBOX_STATUSES, AgentAssignment,
    AgentCoordinationError, AgentCoordinationEvent, AgentInboxCounts, AgentInboxMessage,
    agent_coordination_codes, agent_identity_role, normalize_assignment_lease_secs,
    normalize_inbox_ttl_secs, validate_agent_inbox_payload,
};

use super::{Store, now_rfc3339};

/// Hard upper bound on how many coordination events one replay read returns.
pub const AGENT_COORDINATION_EVENT_LIMIT: u32 = AGENT_COORDINATION_MAX_EVENTS;

/// Bounded result of one assignment mutation. `events` contains every event
/// committed by the transaction, including an expiry event for a stolen stale
/// lease. The caller may log them; they are already persisted.
#[derive(Debug, Clone)]
pub struct AssignmentMutation {
    pub assignment: AgentAssignment,
    pub changed: bool,
    pub events: Vec<AgentCoordinationEvent>,
}

fn coordination_error(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    AgentCoordinationError::new(code, message).into()
}

fn parse_time(value: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(value, &Rfc3339).ok()
}

fn format_time(value: OffsetDateTime) -> Result<String> {
    value
        .format(&Rfc3339)
        .map_err(|error| anyhow!("formatting coordination time: {error}"))
}

fn lease_expiry(now: OffsetDateTime, lease_secs: u64) -> Result<String> {
    format_time(now + time::Duration::seconds(lease_secs as i64))
}

fn new_lease_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

/// Imported archives and landed sessions are final. New assignments and
/// renewals are rejected; an existing row can still be released by an
/// operator for cleanup.
fn ensure_session_open_tx(tx: &Transaction<'_>, session_id: &str) -> Result<()> {
    let status: Option<String> = tx
        .query_row(
            "SELECT status FROM sessions WHERE id = ?1",
            params![session_id],
            |row| row.get(0),
        )
        .optional()?;
    match status.as_deref() {
        Some("imported") => Err(coordination_error(
            agent_coordination_codes::ASSIGNMENT_INVALID,
            "an imported session is a read-only archive and cannot receive an assignment",
        )),
        Some("landed") => Err(coordination_error(
            agent_coordination_codes::ASSIGNMENT_INVALID,
            "a landed session is final and cannot receive an assignment",
        )),
        _ => Ok(()),
    }
}

fn project_for_session_tx(tx: &Transaction<'_>, session_id: &str) -> Result<String> {
    tx.query_row(
        "SELECT project_id FROM sessions WHERE id = ?1",
        params![session_id],
        |row| row.get(0),
    )
    .optional()?
    .ok_or_else(|| {
        coordination_error(
            agent_coordination_codes::ASSIGNMENT_NOT_FOUND,
            format!("session not found: {session_id}"),
        )
    })
}

struct IdentityRow {
    project_id: String,
    role: String,
    status: String,
    origin: String,
}

fn identity_row_tx(tx: &Transaction<'_>, identity_id: &str) -> Result<IdentityRow> {
    tx.query_row(
        "SELECT project_id, role, status, origin FROM agent_identities WHERE id = ?1",
        params![identity_id],
        |row| {
            Ok(IdentityRow {
                project_id: row.get(0)?,
                role: row.get(1)?,
                status: row.get(2)?,
                origin: row.get(3)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| {
        coordination_error(
            agent_coordination_codes::ASSIGNMENT_NOT_FOUND,
            format!("agent identity not found: {identity_id}"),
        )
    })
}

fn ensure_identity_assignable(identity: &IdentityRow) -> Result<&'static str> {
    let role = agent_identity_role(&identity.role).ok_or_else(|| {
        coordination_error(
            agent_coordination_codes::ASSIGNMENT_ROLE_MISMATCH,
            format!(
                "identity role must be leader, manager, or worker: {}",
                identity.role
            ),
        )
    })?;
    if identity.origin != "local" {
        return Err(coordination_error(
            agent_coordination_codes::ASSIGNMENT_IDENTITY_UNAVAILABLE,
            "an imported identity is read-only until an explicit rehome",
        ));
    }
    match identity.status.as_str() {
        "retired" => Err(coordination_error(
            agent_coordination_codes::ASSIGNMENT_IDENTITY_RETIRED,
            "a retired identity cannot be assigned to a session",
        )),
        "paused" => Err(coordination_error(
            agent_coordination_codes::ASSIGNMENT_IDENTITY_UNAVAILABLE,
            "a paused identity cannot be assigned; resume it first",
        )),
        "active" | "idle" => Ok(role),
        other => Err(coordination_error(
            agent_coordination_codes::ASSIGNMENT_INVALID,
            format!("identity has an unsupported status: {other}"),
        )),
    }
}

pub(crate) fn map_assignment(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentAssignment> {
    let status: String = row.get(4)?;
    let lease_expires_at: Option<String> = row.get(8)?;
    let now = OffsetDateTime::now_utc();
    let lease_active = status == "active"
        && lease_expires_at
            .as_deref()
            .and_then(parse_time)
            .is_some_and(|expires| expires > now);
    Ok(AgentAssignment {
        session_id: row.get(0)?,
        project_id: row.get(1)?,
        identity_id: row.get(2)?,
        role: row.get(3)?,
        status,
        assigned_at: row.get(5)?,
        released_at: row.get(6)?,
        heartbeat_at: row.get(7)?,
        lease_expires_at,
        lease_active,
    })
}

pub(crate) const ASSIGNMENT_SELECT: &str =
    "SELECT a.session_id, s.project_id, a.identity_id, a.role, a.status, a.assigned_at,
            a.released_at, a.heartbeat_at, a.lease_expires_at
     FROM session_agent_assignments a
     JOIN sessions s ON s.id = a.session_id";

fn get_assignment_conn(
    conn: &rusqlite::Connection,
    session_id: &str,
    identity_id: &str,
) -> Result<Option<AgentAssignment>> {
    conn.query_row(
        &format!("{ASSIGNMENT_SELECT} WHERE a.session_id = ?1 AND a.identity_id = ?2"),
        params![session_id, identity_id],
        map_assignment,
    )
    .optional()
    .map_err(Into::into)
}

pub(crate) fn get_assignment_tx(
    tx: &Transaction<'_>,
    session_id: &str,
    identity_id: &str,
) -> Result<Option<AgentAssignment>> {
    tx.query_row(
        &format!("{ASSIGNMENT_SELECT} WHERE a.session_id = ?1 AND a.identity_id = ?2"),
        params![session_id, identity_id],
        map_assignment,
    )
    .optional()
    .map_err(Into::into)
}

pub(crate) fn insert_assignment_event_tx(
    tx: &Transaction<'_>,
    project_id: &str,
    kind: &str,
    assignment: &AgentAssignment,
    extra: Option<(&str, Value)>,
) -> Result<AgentCoordinationEvent> {
    let mut payload = serde_json::json!({"assignment": assignment});
    if let Some((key, value)) = extra
        && let Some(object) = payload.as_object_mut()
    {
        object.insert(key.to_string(), value);
    }
    insert_coordination_event_tx(tx, project_id, kind, &payload)
}

fn insert_inbox_event_tx(
    tx: &Transaction<'_>,
    project_id: &str,
    kind: &str,
    message: &AgentInboxMessage,
) -> Result<AgentCoordinationEvent> {
    insert_coordination_event_tx(
        tx,
        project_id,
        kind,
        &serde_json::json!({"message": message}),
    )
}

pub(crate) fn insert_coordination_event_tx(
    tx: &Transaction<'_>,
    project_id: &str,
    kind: &str,
    payload: &Value,
) -> Result<AgentCoordinationEvent> {
    if !AGENT_ASSIGNMENT_EVENT_KINDS.contains(&kind) && !AGENT_INBOX_EVENT_KINDS.contains(&kind) {
        return Err(coordination_error(
            agent_coordination_codes::INVALID_COORDINATION_EVENT,
            format!("unknown coordination event kind: {kind}"),
        ));
    }
    let seq: i64 = tx.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM agent_coordination_events WHERE project_id = ?1",
        params![project_id],
        |row| row.get(0),
    )?;
    let event = AgentCoordinationEvent {
        id: Uuid::now_v7().to_string(),
        project_id: project_id.to_string(),
        seq: seq.max(1) as u64,
        kind: kind.to_string(),
        payload: payload.clone(),
        created_at: now_rfc3339(),
    };
    tx.execute(
        "INSERT INTO agent_coordination_events (id, project_id, seq, kind, payload, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            event.id,
            event.project_id,
            event.seq as i64,
            event.kind,
            serde_json::to_string(&event.payload)?,
            event.created_at
        ],
    )?;
    Ok(event)
}

/// Expire one identity's active assignment if its lease is due. Returns the
/// committed event, if any. The caller must already hold an immediate
/// transaction.
fn expire_identity_if_due_tx(
    tx: &Transaction<'_>,
    identity_id: &str,
    now: OffsetDateTime,
) -> Result<Option<AgentCoordinationEvent>> {
    let row: Option<(String, String, String, Option<String>)> = tx
        .query_row(
            "SELECT a.session_id, s.project_id, a.status, a.lease_expires_at
             FROM session_agent_assignments a
             JOIN sessions s ON s.id = a.session_id
             WHERE a.identity_id = ?1 AND a.status = 'active'",
            params![identity_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((session_id, project_id, status, expires_at)) = row else {
        return Ok(None);
    };
    if status != "active" {
        return Ok(None);
    }
    let due = expires_at
        .as_deref()
        .and_then(parse_time)
        .is_none_or(|expires| expires <= now);
    if !due {
        return Ok(None);
    }
    let now_text = format_time(now)?;
    tx.execute(
        "UPDATE session_agent_assignments
         SET status = 'released', released_at = ?1, lease_token = NULL
         WHERE session_id = ?2 AND identity_id = ?3 AND status = 'active'",
        params![now_text, session_id, identity_id],
    )?;
    let assignment = get_assignment_tx(tx, &session_id, identity_id)?
        .ok_or_else(|| anyhow!("assignment disappeared during expiry"))?;
    let event = insert_assignment_event_tx(
        tx,
        &project_id,
        vp_protocol::types::AGENT_ASSIGNMENT_EXPIRED_EVENT,
        &assignment,
        Some(("reason", serde_json::json!("lease_expired"))),
    )?;
    Ok(Some(event))
}

fn validate_operator_label(operator: Option<&str>) -> Result<Option<String>> {
    let Some(operator) = operator.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if operator.chars().count() > 200 || operator.chars().any(char::is_control) {
        return Err(coordination_error(
            agent_coordination_codes::ASSIGNMENT_INVALID,
            "operator label must be 1-200 printable characters",
        ));
    }
    Ok(Some(operator.to_string()))
}

mod assignments;
mod inbox;
mod replay;

pub(crate) fn insert_assignment_row_tx(
    tx: &Transaction<'_>,
    assignment: &AgentAssignment,
) -> Result<()> {
    let token = if assignment.status == "active" && assignment.lease_expires_at.is_some() {
        Some(new_lease_token())
    } else {
        None
    };
    insert_assignment_row_with_token_tx(tx, assignment, token.as_deref())
}

fn insert_assignment_row_with_token_tx(
    tx: &Transaction<'_>,
    assignment: &AgentAssignment,
    token: Option<&str>,
) -> Result<()> {
    tx.execute(
        "INSERT INTO session_agent_assignments
         (session_id, identity_id, role, status, lease_token, lease_expires_at,
          assigned_at, released_at, heartbeat_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            assignment.session_id,
            assignment.identity_id,
            assignment.role,
            assignment.status,
            token,
            assignment.lease_expires_at,
            assignment.assigned_at,
            assignment.released_at,
            assignment.heartbeat_at
        ],
    )?;
    Ok(())
}

pub(crate) fn insert_inbox_row_tx(tx: &Transaction<'_>, message: &AgentInboxMessage) -> Result<()> {
    tx.execute(
        "INSERT INTO agent_inbox
         (id, project_id, identity_id, session_id, from_identity_id, kind,
          payload_json, status, created_at, read_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            message.id,
            message.project_id,
            message.identity_id,
            message.session_id,
            message.from_identity_id,
            message.kind,
            serde_json::to_string(&message.payload)?,
            message.status,
            message.created_at,
            message.read_at,
            message.expires_at
        ],
    )?;
    Ok(())
}

fn map_inbox_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentInboxMessage> {
    let payload_json: String = row.get(6)?;
    let payload = serde_json::from_str(&payload_json).unwrap_or(Value::Null);
    Ok(AgentInboxMessage {
        id: row.get(0)?,
        project_id: row.get(1)?,
        identity_id: row.get(2)?,
        session_id: row.get(3)?,
        from_identity_id: row.get(4)?,
        from_label: row
            .get::<_, Option<String>>(4)?
            .unwrap_or_else(|| "operator".to_string()),
        kind: row.get(5)?,
        payload,
        status: row.get(7)?,
        created_at: row.get(8)?,
        read_at: row.get(9)?,
        expires_at: row.get(10)?,
    })
}

fn map_coordination_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentCoordinationEvent> {
    let payload_json: String = row.get(4)?;
    Ok(AgentCoordinationEvent {
        id: row.get(0)?,
        project_id: row.get(1)?,
        seq: row.get::<_, i64>(2)?.max(0) as u64,
        kind: row.get(3)?,
        payload: serde_json::from_str(&payload_json).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        created_at: row.get(5)?,
    })
}

pub(crate) fn query_inbox_tx(
    tx: &Transaction<'_>,
    message_id: &str,
) -> Result<Option<AgentInboxMessage>> {
    tx.query_row(
        "SELECT id, project_id, identity_id, session_id, from_identity_id, kind,
                payload_json, status, created_at, read_at, expires_at
         FROM agent_inbox WHERE id = ?1",
        params![message_id],
        map_inbox_message,
    )
    .optional()
    .map_err(Into::into)
}

fn inbox_counts_conn(
    conn: &rusqlite::Connection,
    project_id: Option<&str>,
    identity_id: Option<&str>,
    session_id: Option<&str>,
) -> Result<AgentInboxCounts> {
    let mut counts = AgentInboxCounts::default();
    let mut statement = conn.prepare(
        "SELECT status, COUNT(*) FROM agent_inbox
         WHERE (?1 IS NULL OR project_id = ?1)
           AND (?2 IS NULL OR identity_id = ?2)
           AND (?3 IS NULL OR session_id = ?3)
         GROUP BY status",
    )?;
    let rows = statement.query_map(params![project_id, identity_id, session_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (status, count) = row?;
        let count = count.max(0) as u64;
        match status.as_str() {
            "unread" => counts.unread = count,
            "read" => counts.read = count,
            "done" => counts.done = count,
            "expired" => counts.expired = count,
            _ => {}
        }
    }
    Ok(counts)
}

fn validate_assignment_snapshot(assignment: &AgentAssignment) -> Result<()> {
    if assignment.session_id.trim().is_empty() || assignment.identity_id.trim().is_empty() {
        return Err(coordination_error(
            agent_coordination_codes::ASSIGNMENT_INVALID,
            "assignment snapshot has an empty session or identity id",
        ));
    }
    if agent_identity_role(&assignment.role).is_none() {
        return Err(coordination_error(
            agent_coordination_codes::ASSIGNMENT_ROLE_MISMATCH,
            format!("assignment has an invalid role: {}", assignment.role),
        ));
    }
    if !AGENT_ASSIGNMENT_STATUSES.contains(&assignment.status.as_str()) {
        return Err(coordination_error(
            agent_coordination_codes::ASSIGNMENT_INVALID,
            format!("assignment has an invalid status: {}", assignment.status),
        ));
    }
    if assignment.assigned_at.trim().is_empty() || assignment.heartbeat_at.trim().is_empty() {
        return Err(coordination_error(
            agent_coordination_codes::ASSIGNMENT_INVALID,
            "assignment snapshot is missing its timestamps",
        ));
    }
    Ok(())
}

fn validate_inbox_snapshot(message: &AgentInboxMessage) -> Result<()> {
    if message.id.trim().is_empty() || message.identity_id.trim().is_empty() {
        return Err(coordination_error(
            agent_coordination_codes::INBOX_INVALID,
            "inbox snapshot has an empty id or identity id",
        ));
    }
    if !AGENT_INBOX_KINDS.contains(&message.kind.as_str()) {
        return Err(coordination_error(
            agent_coordination_codes::INBOX_INVALID,
            format!("inbox snapshot has an invalid kind: {}", message.kind),
        ));
    }
    if !AGENT_INBOX_STATUSES.contains(&message.status.as_str()) {
        return Err(coordination_error(
            agent_coordination_codes::INBOX_INVALID,
            format!("inbox snapshot has an invalid status: {}", message.status),
        ));
    }
    validate_agent_inbox_payload(&message.payload)?;
    if message.created_at.trim().is_empty() {
        return Err(coordination_error(
            agent_coordination_codes::INBOX_INVALID,
            "inbox snapshot is missing created_at",
        ));
    }
    Ok(())
}

fn validate_coordination_event(event: &AgentCoordinationEvent) -> Result<()> {
    if event.id.trim().is_empty() || event.project_id.trim().is_empty() {
        return Err(coordination_error(
            agent_coordination_codes::COORDINATION_REPLAY_INVALID,
            "coordination event has an empty id or project id",
        ));
    }
    if !AGENT_ASSIGNMENT_EVENT_KINDS.contains(&event.kind.as_str())
        && !AGENT_INBOX_EVENT_KINDS.contains(&event.kind.as_str())
    {
        return Err(coordination_error(
            agent_coordination_codes::COORDINATION_REPLAY_INVALID,
            format!("unknown coordination event kind: {}", event.kind),
        ));
    }
    if event.seq == 0 {
        return Err(coordination_error(
            agent_coordination_codes::COORDINATION_REPLAY_INVALID,
            "coordination event seq must be positive",
        ));
    }
    if event.created_at.trim().is_empty() {
        return Err(coordination_error(
            agent_coordination_codes::COORDINATION_REPLAY_INVALID,
            "coordination event is missing created_at",
        ));
    }
    Ok(())
}

/// Release every active assignment for one identity inside an existing
/// transaction. Used by S6 promotion, transfer, and retirement so a role change
/// or terminal state is never persisted while a session still holds the
/// identity. Each release appends its normal coordination event.
pub(crate) fn release_identity_assignments_tx(
    tx: &Transaction<'_>,
    identity_id: &str,
    operator: &str,
    reason: Option<&str>,
    now_text: &str,
) -> Result<Vec<AgentCoordinationEvent>> {
    let rows = {
        let mut statement = tx.prepare(
            "SELECT a.session_id, a.identity_id, s.project_id
             FROM session_agent_assignments a
             JOIN sessions s ON s.id = a.session_id
             WHERE a.identity_id = ?1 AND a.status = 'active'
             ORDER BY a.assigned_at ASC, a.session_id ASC",
        )?;
        statement
            .query_map(params![identity_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let mut events = Vec::with_capacity(rows.len());
    for (session_id, identity, project_id) in rows {
        tx.execute(
            "UPDATE session_agent_assignments
             SET status = 'released', released_at = ?1, lease_token = NULL,
                 lease_expires_at = NULL
             WHERE session_id = ?2 AND identity_id = ?3 AND status = 'active'",
            params![now_text, session_id, identity],
        )?;
        let assignment = get_assignment_tx(tx, &session_id, &identity)?.ok_or_else(|| {
            coordination_error(
                agent_coordination_codes::ASSIGNMENT_NOT_FOUND,
                "assignment disappeared during forced release",
            )
        })?;
        events.push(insert_assignment_event_tx(
            tx,
            &project_id,
            vp_protocol::types::AGENT_ASSIGNMENT_RELEASED_EVENT,
            &assignment,
            Some((
                "operator",
                serde_json::json!({
                    "operator": true,
                    "label": operator,
                    "reason": reason,
                    "forced": true,
                }),
            )),
        )?);
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use vp_protocol::types::{AgentIdentityPolicy, CreateAgentIdentityParams, Project, Session};

    fn test_store() -> (TempDir, Store) {
        let data = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        (data, store)
    }

    fn project(store: &Store) -> Project {
        let repo = TempDir::new().unwrap();
        let repo_path = repo.path().to_string_lossy().to_string();
        std::mem::forget(repo);
        store.add_project(&repo_path, Some("coordination")).unwrap()
    }

    fn identity(store: &Store, project_id: &str, role: &str) -> String {
        let policy = AgentIdentityPolicy {
            enabled: true,
            ..AgentIdentityPolicy::default()
        };
        store
            .set_agent_identity_policy(project_id, &policy)
            .unwrap();
        let (identity, _) = store
            .create_agent_identity(&CreateAgentIdentityParams {
                project_id: project_id.to_string(),
                role: role.to_string(),
                title: Some(format!("{role} title")),
                ..CreateAgentIdentityParams::default()
            })
            .unwrap();
        identity.id
    }

    fn session(store: &Store, project_id: &str, goal: &str) -> Session {
        store.create_session(project_id, goal).unwrap()
    }

    #[test]
    fn one_active_assignment_per_identity_and_idempotent_assign() {
        let (_data, store) = test_store();
        let project = project(&store);
        let identity_id = identity(&store, &project.id, "worker");
        let session_a = session(&store, &project.id, "a");
        let session_b = session(&store, &project.id, "b");

        let first = store
            .create_session_agent_assignment(&session_a.id, &identity_id, None, None, None)
            .unwrap();
        assert!(first.changed);
        assert_eq!(first.assignment.status, "active");
        assert!(first.assignment.lease_active);
        assert_eq!(first.events.len(), 1);

        let same = store
            .create_session_agent_assignment(&session_a.id, &identity_id, None, None, None)
            .unwrap();
        assert!(!same.changed);
        // The row is not re-created, but the live lease is renewed and that
        // renewal is event-backed.
        assert_eq!(same.events.len(), 1);
        assert!(same.assignment.lease_active);

        let conflict = store
            .create_session_agent_assignment(&session_b.id, &identity_id, None, None, None)
            .unwrap_err();
        let typed = conflict
            .downcast_ref::<AgentCoordinationError>()
            .expect("typed coordination error");
        assert_eq!(typed.code, agent_coordination_codes::ASSIGNMENT_CONFLICT);

        store
            .release_session_agent_assignment(&session_a.id, &identity_id, true, None)
            .unwrap();
        let reassigned = store
            .create_session_agent_assignment(&session_b.id, &identity_id, None, None, None)
            .unwrap();
        assert!(reassigned.changed);
        assert_eq!(reassigned.assignment.session_id, session_b.id);
    }

    #[test]
    fn expired_lease_is_reclaimed_and_recorded() {
        let (_data, store) = test_store();
        let project = project(&store);
        let identity_id = identity(&store, &project.id, "worker");
        let session_a = session(&store, &project.id, "a");
        let session_b = session(&store, &project.id, "b");
        store
            .create_session_agent_assignment(&session_a.id, &identity_id, None, Some(60), None)
            .unwrap();
        let conn = store.lock().unwrap();
        conn.execute(
            "UPDATE session_agent_assignments
             SET lease_expires_at = '2000-01-01T00:00:00Z'
             WHERE session_id = ?1 AND identity_id = ?2",
            params![session_a.id, identity_id],
        )
        .unwrap();
        drop(conn);
        let assigned = store
            .create_session_agent_assignment(&session_b.id, &identity_id, None, None, None)
            .unwrap();
        assert!(assigned.changed);
        assert_eq!(assigned.assignment.session_id, session_b.id);
        let released = store
            .get_session_agent_assignment(&session_a.id, &identity_id)
            .unwrap()
            .expect("released row remains as evidence");
        assert_eq!(released.status, "released");
    }

    #[test]
    fn inbox_rejects_unsupported_project_roles() {
        let (_data, store) = test_store();
        let project = project(&store);
        let worker = identity(&store, &project.id, "worker");
        let now = now_rfc3339();
        let mut fake = Vec::new();
        for role in ["labour", "verifier", "scout"] {
            let id = format!("fake-{role}");
            store
                .lock()
                .unwrap()
                .execute(
                    "INSERT INTO agent_identities
                     (id, project_id, role, status, generation, created_at, updated_at,
                      policy_json, origin)
                     VALUES (?1, ?2, ?3, 'active', 1, ?4, ?4, '{}', 'local')",
                    params![id, project.id, role, now],
                )
                .unwrap();
            fake.push((role.to_string(), id));
        }
        for (role, id) in &fake {
            let error = store
                .send_agent_inbox_message(
                    &project.id,
                    id,
                    None,
                    None,
                    "message",
                    &serde_json::json!({"body": "no project inbox"}),
                    None,
                )
                .unwrap_err();
            let typed = error
                .downcast_ref::<AgentCoordinationError>()
                .expect("typed coordination error");
            assert_eq!(
                typed.code,
                agent_coordination_codes::INBOX_RECIPIENT_INVALID,
                "role {role} must not receive a project inbox row"
            );
        }
        let error = store
            .send_agent_inbox_message(
                &project.id,
                &worker,
                None,
                Some(&fake[0].1),
                "message",
                &serde_json::json!({"body": "bad sender"}),
                None,
            )
            .unwrap_err();
        let typed = error
            .downcast_ref::<AgentCoordinationError>()
            .expect("typed coordination error");
        assert_eq!(typed.code, agent_coordination_codes::INBOX_SENDER_INVALID);
    }

    #[test]
    fn inbox_send_list_read_ack_and_expiry() {
        let (_data, store) = test_store();
        let project = project(&store);
        let recipient = identity(&store, &project.id, "worker");
        let sender = identity(&store, &project.id, "manager");
        let message = store
            .send_agent_inbox_message(
                &project.id,
                &recipient,
                None,
                Some(&sender),
                "handoff",
                &serde_json::json!({"summary": "review the change"}),
                None,
            )
            .unwrap();
        assert_eq!(message.from_label, "manager title");
        assert_eq!(message.status, "unread");
        let (messages, counts) = store
            .list_agent_inbox_messages(Some(&project.id), Some(&recipient), None, None, true, None)
            .unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(counts.unread, 1);

        let (read, changed) = store
            .mark_agent_inbox_read(&message.id, Some(&project.id), Some(&recipient))
            .unwrap();
        assert!(changed);
        assert_eq!(read.status, "read");
        let unread_first = store
            .send_agent_inbox_message(
                &project.id,
                &recipient,
                None,
                None,
                "message",
                &serde_json::json!({"summary": "newer unread"}),
                None,
            )
            .unwrap();
        let (ordered, _) = store
            .list_agent_inbox_messages(Some(&project.id), Some(&recipient), None, None, false, None)
            .unwrap();
        assert_eq!(ordered[0].id, unread_first.id);
        assert_eq!(ordered[0].status, "unread");
        let (again, changed) = store
            .mark_agent_inbox_read(&message.id, None, None)
            .unwrap();
        assert!(!changed);
        assert_eq!(again.status, "read");
        let (done, changed) = store
            .ack_agent_inbox_message(&message.id, Some(&project.id), None)
            .unwrap();
        assert!(changed);
        assert_eq!(done.status, "done");

        let expiring = store
            .send_agent_inbox_message(
                &project.id,
                &recipient,
                None,
                None,
                "message",
                &serde_json::json!({"body": "short lived"}),
                Some(60),
            )
            .unwrap();
        let conn = store.lock().unwrap();
        conn.execute(
            "UPDATE agent_inbox SET expires_at = '2000-01-01T00:00:00Z' WHERE id = ?1",
            params![expiring.id],
        )
        .unwrap();
        drop(conn);
        let error = store
            .mark_agent_inbox_read(&expiring.id, None, None)
            .unwrap_err();
        let typed = error
            .downcast_ref::<AgentCoordinationError>()
            .expect("typed coordination error");
        assert_eq!(typed.code, agent_coordination_codes::INBOX_EXPIRED);
        let row = store
            .get_agent_inbox_message(&expiring.id)
            .unwrap()
            .unwrap();
        assert_eq!(row.status, "expired");
    }
}

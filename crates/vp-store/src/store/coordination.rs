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

fn map_assignment(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentAssignment> {
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

const ASSIGNMENT_SELECT: &str =
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

fn get_assignment_tx(
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

fn insert_assignment_event_tx(
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

impl Store {
    // ---------------------------------------------------------------------
    // Assignments and identity leases
    // ---------------------------------------------------------------------

    pub fn get_session_agent_assignment(
        &self,
        session_id: &str,
        identity_id: &str,
    ) -> Result<Option<AgentAssignment>> {
        let conn = self.lock()?;
        get_assignment_conn(&conn, session_id, identity_id)
    }

    /// Bounded assignment listing. At least one selector is required so this
    /// cannot become an unbounded project-wide scan.
    pub fn list_session_agent_assignments(
        &self,
        session_id: Option<&str>,
        identity_id: Option<&str>,
        project_id: Option<&str>,
        status: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<AgentAssignment>> {
        if session_id.is_none() && identity_id.is_none() && project_id.is_none() {
            return Err(coordination_error(
                agent_coordination_codes::ASSIGNMENT_INVALID,
                "list assignments requires a session, identity, or project filter",
            ));
        }
        if let Some(status) = status
            && !AGENT_ASSIGNMENT_STATUSES.contains(&status)
        {
            return Err(coordination_error(
                agent_coordination_codes::ASSIGNMENT_INVALID,
                format!("assignment status must be active, released, or queued: {status}"),
            ));
        }
        let limit = limit
            .unwrap_or(AGENT_ASSIGNMENT_MAX_LIST)
            .clamp(1, AGENT_ASSIGNMENT_MAX_LIST) as i64;
        let conn = self.lock()?;
        let mut statement = conn.prepare(&format!(
            "{ASSIGNMENT_SELECT}
             WHERE (?1 IS NULL OR a.session_id = ?1)
               AND (?2 IS NULL OR a.identity_id = ?2)
               AND (?3 IS NULL OR s.project_id = ?3)
               AND (?4 IS NULL OR a.status = ?4)
             ORDER BY a.assigned_at DESC, a.session_id ASC, a.identity_id ASC
             LIMIT ?5"
        ))?;
        let rows = statement.query_map(
            params![session_id, identity_id, project_id, status, limit],
            map_assignment,
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Assign a project identity to a session.
    ///
    /// Idempotent when the same session already holds a live assignment for the
    /// identity. A different session with a live lease is a typed conflict. An
    /// expired lease is released and recorded as `agent.assignment.expired`
    /// before the new assignment is created.
    pub fn create_session_agent_assignment(
        &self,
        session_id: &str,
        identity_id: &str,
        requested_role: Option<&str>,
        lease_secs: Option<u64>,
        operator: Option<&str>,
    ) -> Result<AssignmentMutation> {
        let lease_secs = normalize_assignment_lease_secs(lease_secs)?;
        let operator = validate_operator_label(operator)?;
        let now = OffsetDateTime::now_utc();
        let now_text = format_time(now)?;
        let expires = lease_expiry(now, lease_secs)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let project_id = project_for_session_tx(&tx, session_id)?;
        ensure_session_open_tx(&tx, session_id)?;
        let identity = identity_row_tx(&tx, identity_id)?;
        if identity.project_id != project_id {
            return Err(coordination_error(
                agent_coordination_codes::ASSIGNMENT_PROJECT_MISMATCH,
                "identity and session belong to different projects",
            ));
        }
        let role = ensure_identity_assignable(&identity)?;
        if let Some(requested) = requested_role
            && requested != role
        {
            return Err(coordination_error(
                agent_coordination_codes::ASSIGNMENT_ROLE_MISMATCH,
                format!("requested role {requested} does not match identity role {role}"),
            ));
        }
        let policy = super::agent_identity_policy_tx(&tx, &project_id)?;
        if !policy.enabled {
            return Err(coordination_error(
                agent_coordination_codes::ASSIGNMENT_POLICY_DISABLED,
                "project agent identities are disabled; enable the project policy before assigning",
            ));
        }

        let mut events = Vec::new();
        if let Some(event) = expire_identity_if_due_tx(&tx, identity_id, now)? {
            events.push(event);
        }
        let other_session: Option<String> = tx
            .query_row(
                "SELECT session_id FROM session_agent_assignments
                 WHERE identity_id = ?1 AND status = 'active'",
                params![identity_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(other_session) = other_session {
            if other_session == session_id {
                let existing = get_assignment_tx(&tx, session_id, identity_id)?
                    .ok_or_else(|| anyhow!("assignment disappeared during idempotent assign"))?;
                tx.commit()?;
                return Ok(AssignmentMutation {
                    assignment: existing,
                    changed: false,
                    events,
                });
            }
            return Err(coordination_error(
                agent_coordination_codes::ASSIGNMENT_CONFLICT,
                format!(
                    "identity {identity_id} already has an active assignment in session {other_session}"
                ),
            ));
        }

        tx.execute(
            "INSERT INTO session_agent_assignments
             (session_id, identity_id, role, status, lease_token, lease_expires_at,
              assigned_at, released_at, heartbeat_at)
             VALUES (?1, ?2, ?3, 'active', ?4, ?5, ?6, NULL, ?6)
             ON CONFLICT(session_id, identity_id) DO UPDATE SET
                role = excluded.role,
                status = 'active',
                lease_token = excluded.lease_token,
                lease_expires_at = excluded.lease_expires_at,
                assigned_at = excluded.assigned_at,
                released_at = NULL,
                heartbeat_at = excluded.heartbeat_at",
            params![
                session_id,
                identity_id,
                role,
                new_lease_token(),
                expires,
                now_text
            ],
        )?;
        let assignment = get_assignment_tx(&tx, session_id, identity_id)?
            .ok_or_else(|| anyhow!("assignment disappeared after creation"))?;
        let event = insert_assignment_event_tx(
            &tx,
            &project_id,
            vp_protocol::types::AGENT_ASSIGNMENT_CREATED_EVENT,
            &assignment,
            Some(("operator", serde_json::json!(operator))),
        )?;
        events.push(event);
        tx.commit()?;
        Ok(AssignmentMutation {
            assignment,
            changed: true,
            events,
        })
    }

    /// Heartbeat/renew the live lease. Renewal cannot revive an expired lease;
    /// it records the expiry and returns `ASSIGNMENT_LEASE_EXPIRED`.
    pub fn renew_session_agent_assignment(
        &self,
        session_id: &str,
        identity_id: &str,
        lease_secs: Option<u64>,
        operator: Option<&str>,
    ) -> Result<AssignmentMutation> {
        let lease_secs = normalize_assignment_lease_secs(lease_secs)?;
        let operator = validate_operator_label(operator)?;
        let now = OffsetDateTime::now_utc();
        let now_text = format_time(now)?;
        let expires = lease_expiry(now, lease_secs)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let project_id = project_for_session_tx(&tx, session_id)?;
        ensure_session_open_tx(&tx, session_id)?;
        let identity = identity_row_tx(&tx, identity_id)?;
        if identity.project_id != project_id {
            return Err(coordination_error(
                agent_coordination_codes::ASSIGNMENT_PROJECT_MISMATCH,
                "identity and session belong to different projects",
            ));
        }
        if let Some(event) = expire_identity_if_due_tx(&tx, identity_id, now)? {
            tx.commit()?;
            let _ = event;
            return Err(coordination_error(
                agent_coordination_codes::ASSIGNMENT_LEASE_EXPIRED,
                "the identity lease already expired; assign the identity again",
            ));
        }
        let existing = get_assignment_tx(&tx, session_id, identity_id)?.ok_or_else(|| {
            coordination_error(
                agent_coordination_codes::ASSIGNMENT_NOT_FOUND,
                "session does not hold an assignment for this identity",
            )
        })?;
        if existing.status != "active" {
            return Err(coordination_error(
                agent_coordination_codes::ASSIGNMENT_LEASE_EXPIRED,
                format!(
                    "the assignment is {} and cannot be renewed",
                    existing.status
                ),
            ));
        }
        if !existing.lease_active {
            return Err(coordination_error(
                agent_coordination_codes::ASSIGNMENT_LEASE_EXPIRED,
                "the identity lease is not live",
            ));
        }
        tx.execute(
            "UPDATE session_agent_assignments
             SET lease_token = ?1, lease_expires_at = ?2, heartbeat_at = ?3
             WHERE session_id = ?4 AND identity_id = ?5 AND status = 'active'",
            params![
                new_lease_token(),
                expires,
                now_text,
                session_id,
                identity_id
            ],
        )?;
        let assignment = get_assignment_tx(&tx, session_id, identity_id)?
            .ok_or_else(|| anyhow!("assignment disappeared during renewal"))?;
        let event = insert_assignment_event_tx(
            &tx,
            &project_id,
            vp_protocol::types::AGENT_ASSIGNMENT_RENEWED_EVENT,
            &assignment,
            Some(("operator", serde_json::json!(operator))),
        )?;
        tx.commit()?;
        Ok(AssignmentMutation {
            assignment,
            changed: true,
            events: vec![event],
        })
    }

    /// Explicit release. The caller must name the owning session; there is no
    /// cross-session release path. Repeated release is an idempotent no-op.
    pub fn release_session_agent_assignment(
        &self,
        session_id: &str,
        identity_id: &str,
        operator: bool,
        reason: Option<&str>,
    ) -> Result<AssignmentMutation> {
        let reason = reason
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
            .map(str::to_string);
        if let Some(reason) = &reason
            && (reason.chars().count() > 500 || reason.chars().any(char::is_control))
        {
            return Err(coordination_error(
                agent_coordination_codes::ASSIGNMENT_INVALID,
                "release reason must be 1-500 printable characters",
            ));
        }
        let now_text = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let project_id = project_for_session_tx(&tx, session_id)?;
        let existing = get_assignment_tx(&tx, session_id, identity_id)?.ok_or_else(|| {
            coordination_error(
                agent_coordination_codes::ASSIGNMENT_NOT_FOUND,
                "session does not hold an assignment for this identity",
            )
        })?;
        if existing.status != "active" {
            tx.commit()?;
            return Ok(AssignmentMutation {
                assignment: existing,
                changed: false,
                events: Vec::new(),
            });
        }
        tx.execute(
            "UPDATE session_agent_assignments
             SET status = 'released', released_at = ?1, lease_token = NULL,
                 lease_expires_at = NULL
             WHERE session_id = ?2 AND identity_id = ?3 AND status = 'active'",
            params![now_text, session_id, identity_id],
        )?;
        let assignment = get_assignment_tx(&tx, session_id, identity_id)?
            .ok_or_else(|| anyhow!("assignment disappeared during release"))?;
        let event = insert_assignment_event_tx(
            &tx,
            &project_id,
            vp_protocol::types::AGENT_ASSIGNMENT_RELEASED_EVENT,
            &assignment,
            Some((
                "operator",
                serde_json::json!({
                    "operator": operator,
                    "reason": reason,
                }),
            )),
        )?;
        tx.commit()?;
        Ok(AssignmentMutation {
            assignment,
            changed: true,
            events: vec![event],
        })
    }

    /// Expire every due active lease. Startup recovery and the periodic sweeper
    /// use the same operation; each expired row emits one bounded event.
    pub fn expire_session_agent_assignments(
        &self,
        limit: Option<u32>,
    ) -> Result<Vec<AgentCoordinationEvent>> {
        self.expire_session_agent_assignments_at(OffsetDateTime::now_utc(), limit)
    }

    pub fn expire_session_agent_assignments_at(
        &self,
        now: OffsetDateTime,
        limit: Option<u32>,
    ) -> Result<Vec<AgentCoordinationEvent>> {
        let limit = limit.unwrap_or(100).clamp(1, 1_000) as i64;
        let now_text = format_time(now)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let rows = {
            let mut statement = tx.prepare(
                "SELECT a.session_id, a.identity_id, s.project_id
                 FROM session_agent_assignments a
                 JOIN sessions s ON s.id = a.session_id
                 WHERE a.status = 'active'
                   AND (a.lease_expires_at IS NULL
                        OR julianday(a.lease_expires_at) <= julianday(?1))
                 ORDER BY a.lease_expires_at ASC, a.session_id ASC
                 LIMIT ?2",
            )?;
            statement
                .query_map(params![now_text, limit], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut events = Vec::with_capacity(rows.len());
        for (session_id, identity_id, project_id) in rows {
            tx.execute(
                "UPDATE session_agent_assignments
                 SET status = 'released', released_at = ?1, lease_token = NULL
                 WHERE session_id = ?2 AND identity_id = ?3 AND status = 'active'",
                params![now_text, session_id, identity_id],
            )?;
            let assignment = get_assignment_tx(&tx, &session_id, &identity_id)?
                .ok_or_else(|| anyhow!("assignment disappeared during lease sweep"))?;
            events.push(insert_assignment_event_tx(
                &tx,
                &project_id,
                vp_protocol::types::AGENT_ASSIGNMENT_EXPIRED_EVENT,
                &assignment,
                Some(("reason", serde_json::json!("lease_sweep"))),
            )?);
        }
        tx.commit()?;
        Ok(events)
    }

    // ---------------------------------------------------------------------
    // Project-scoped agent inbox
    // ---------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn insert_inbox_message_tx(
        tx: &Transaction<'_>,
        project_id: &str,
        identity_id: &str,
        session_id: Option<&str>,
        from_identity_id: Option<&str>,
        kind: &str,
        payload: &Value,
        expires_at: Option<&str>,
    ) -> Result<AgentInboxMessage> {
        let message = AgentInboxMessage {
            id: Uuid::now_v7().to_string(),
            project_id: project_id.to_string(),
            identity_id: identity_id.to_string(),
            session_id: session_id.map(str::to_string),
            from_identity_id: from_identity_id.map(str::to_string),
            from_label: from_identity_id.unwrap_or("operator").to_string(),
            kind: kind.to_string(),
            payload: payload.clone(),
            status: "unread".to_string(),
            created_at: now_rfc3339(),
            read_at: None,
            expires_at: expires_at.map(str::to_string),
        };
        tx.execute(
            "INSERT INTO agent_inbox
             (id, project_id, identity_id, session_id, from_identity_id, kind,
              payload_json, status, created_at, read_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, ?10)",
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
                message.expires_at
            ],
        )?;
        Ok(message)
    }

    /// Send one project-scoped inbox row. Recipient and sender must belong to
    /// the same project and have a supported durable role. Sender absent means
    /// the stable `operator` label. No row is created on validation failure.
    #[allow(clippy::too_many_arguments)]
    pub fn send_agent_inbox_message(
        &self,
        project_id: &str,
        identity_id: &str,
        session_id: Option<&str>,
        from_identity_id: Option<&str>,
        kind: &str,
        payload: &Value,
        expires_in_secs: Option<u64>,
    ) -> Result<AgentInboxMessage> {
        if project_id.trim().is_empty() || identity_id.trim().is_empty() {
            return Err(coordination_error(
                agent_coordination_codes::INBOX_INVALID,
                "project_id and identity_id are required",
            ));
        }
        let kind = kind.trim().to_ascii_lowercase();
        if !AGENT_INBOX_KINDS.contains(&kind.as_str()) {
            return Err(coordination_error(
                agent_coordination_codes::INBOX_INVALID,
                format!(
                    "kind must be one of task, message, question, handoff, decision, review: {kind}"
                ),
            ));
        }
        validate_agent_inbox_payload(payload)?;
        let ttl = normalize_inbox_ttl_secs(expires_in_secs)?;
        let now = OffsetDateTime::now_utc();
        let expires_at = match ttl {
            Some(seconds) => Some(format_time(now + time::Duration::seconds(seconds as i64))?),
            None => None,
        };
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let recipient = identity_row_tx(&tx, identity_id)?;
        if recipient.project_id != project_id {
            return Err(coordination_error(
                agent_coordination_codes::INBOX_PROJECT_MISMATCH,
                "recipient identity belongs to another project",
            ));
        }
        let recipient_role = agent_identity_role(&recipient.role).ok_or_else(|| {
            coordination_error(
                agent_coordination_codes::INBOX_RECIPIENT_INVALID,
                format!(
                    "identity role has no project inbox (Labour/Verifier/Scout are task-scoped): {}",
                    recipient.role
                ),
            )
        })?;
        debug_assert!(AGENT_IDENTITY_ROLES.contains(&recipient_role));
        if recipient.status == "retired" {
            return Err(coordination_error(
                agent_coordination_codes::INBOX_RECIPIENT_RETIRED,
                "a retired identity cannot receive project inbox messages",
            ));
        }
        if let Some(sender_id) = from_identity_id {
            let sender = identity_row_tx(&tx, sender_id)?;
            if sender.project_id != project_id {
                return Err(coordination_error(
                    agent_coordination_codes::INBOX_SENDER_INVALID,
                    "sender identity belongs to another project",
                ));
            }
            if agent_identity_role(&sender.role).is_none() {
                return Err(coordination_error(
                    agent_coordination_codes::INBOX_SENDER_INVALID,
                    "sender has no durable project role",
                ));
            }
            if sender.status == "retired" {
                return Err(coordination_error(
                    agent_coordination_codes::INBOX_SENDER_INVALID,
                    "a retired identity cannot send project inbox messages",
                ));
            }
        }
        if let Some(session_id) = session_id {
            let session_project: Option<String> = tx
                .query_row(
                    "SELECT project_id FROM sessions WHERE id = ?1",
                    params![session_id],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(session_project) = session_project else {
                return Err(coordination_error(
                    agent_coordination_codes::INBOX_NOT_FOUND,
                    format!("session not found: {session_id}"),
                ));
            };
            if session_project != project_id {
                return Err(coordination_error(
                    agent_coordination_codes::INBOX_PROJECT_MISMATCH,
                    "session belongs to another project",
                ));
            }
            let active_session: Option<String> = tx
                .query_row(
                    "SELECT session_id FROM session_agent_assignments
                     WHERE identity_id = ?1 AND status = 'active'",
                    params![identity_id],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(active_session) = active_session
                && active_session != session_id
            {
                return Err(coordination_error(
                    agent_coordination_codes::INBOX_CONFLICT,
                    format!(
                        "identity is actively assigned to session {active_session}, not {session_id}"
                    ),
                ));
            }
        }
        let message = Self::insert_inbox_message_tx(
            &tx,
            project_id,
            identity_id,
            session_id,
            from_identity_id,
            &kind,
            payload,
            expires_at.as_deref(),
        )?;
        insert_inbox_event_tx(
            &tx,
            project_id,
            vp_protocol::types::AGENT_INBOX_MESSAGE_EVENT,
            &message,
        )?;
        tx.commit()?;
        Ok(message)
    }

    /// Bounded inbox listing. Unread rows sort first, then newest first.
    pub fn list_agent_inbox_messages(
        &self,
        project_id: Option<&str>,
        identity_id: Option<&str>,
        session_id: Option<&str>,
        status: Option<&str>,
        unread_only: bool,
        limit: Option<u32>,
    ) -> Result<(Vec<AgentInboxMessage>, AgentInboxCounts)> {
        if project_id.is_none() && identity_id.is_none() && session_id.is_none() {
            return Err(coordination_error(
                agent_coordination_codes::INBOX_INVALID,
                "list inbox requires a project, identity, or session filter",
            ));
        }
        if let Some(status) = status
            && !AGENT_INBOX_STATUSES.contains(&status)
        {
            return Err(coordination_error(
                agent_coordination_codes::INBOX_INVALID,
                format!("inbox status must be unread, read, done, or expired: {status}"),
            ));
        }
        let limit = limit
            .unwrap_or(AGENT_INBOX_MAX_LIST)
            .clamp(1, AGENT_INBOX_MAX_LIST) as i64;
        let status_filter = if unread_only { Some("unread") } else { status };
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT id, project_id, identity_id, session_id, from_identity_id, kind,
                    payload_json, status, created_at, read_at, expires_at
             FROM agent_inbox
             WHERE (?1 IS NULL OR project_id = ?1)
               AND (?2 IS NULL OR identity_id = ?2)
               AND (?3 IS NULL OR session_id = ?3)
               AND (?4 IS NULL OR status = ?4)
             ORDER BY CASE status
                        WHEN 'unread' THEN 0
                        WHEN 'read' THEN 1
                        WHEN 'done' THEN 2
                        ELSE 3
                      END,
                      created_at DESC, id ASC
             LIMIT ?5",
        )?;
        let rows = statement.query_map(
            params![project_id, identity_id, session_id, status_filter, limit],
            map_inbox_message,
        )?;
        let messages = rows.collect::<Result<Vec<_>, _>>()?;
        let counts = inbox_counts_conn(&conn, project_id, identity_id, session_id)?;
        Ok((messages, counts))
    }

    pub fn get_agent_inbox_message(&self, message_id: &str) -> Result<Option<AgentInboxMessage>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, project_id, identity_id, session_id, from_identity_id, kind,
                    payload_json, status, created_at, read_at, expires_at
             FROM agent_inbox WHERE id = ?1",
            params![message_id],
            map_inbox_message,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Mark one message read. Idempotent for `read` and `done`; a due TTL is
    /// recorded as `expired` and returned as a typed failure.
    pub fn mark_agent_inbox_read(
        &self,
        message_id: &str,
        project_id: Option<&str>,
        identity_id: Option<&str>,
    ) -> Result<(AgentInboxMessage, bool)> {
        self.transition_agent_inbox(
            message_id,
            project_id,
            identity_id,
            vp_protocol::types::AGENT_INBOX_READ_EVENT,
            false,
        )
    }

    /// Acknowledge one message as done. Accepts `unread` or `read`, and is
    /// idempotent for `done`.
    pub fn ack_agent_inbox_message(
        &self,
        message_id: &str,
        project_id: Option<&str>,
        identity_id: Option<&str>,
    ) -> Result<(AgentInboxMessage, bool)> {
        self.transition_agent_inbox(
            message_id,
            project_id,
            identity_id,
            vp_protocol::types::AGENT_INBOX_DONE_EVENT,
            true,
        )
    }

    fn transition_agent_inbox(
        &self,
        message_id: &str,
        project_id: Option<&str>,
        identity_id: Option<&str>,
        event_kind: &str,
        ack: bool,
    ) -> Result<(AgentInboxMessage, bool)> {
        let now = OffsetDateTime::now_utc();
        let now_text = format_time(now)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = query_inbox_tx(&tx, message_id)?.ok_or_else(|| {
            coordination_error(
                agent_coordination_codes::INBOX_NOT_FOUND,
                format!("inbox message not found: {message_id}"),
            )
        })?;
        if let Some(project_id) = project_id
            && existing.project_id != project_id
        {
            return Err(coordination_error(
                agent_coordination_codes::INBOX_PROJECT_MISMATCH,
                "inbox message belongs to another project",
            ));
        }
        if let Some(identity_id) = identity_id
            && existing.identity_id != identity_id
        {
            return Err(coordination_error(
                agent_coordination_codes::INBOX_PROJECT_MISMATCH,
                "inbox message belongs to another identity",
            ));
        }
        if existing.status == "expired" {
            return Err(coordination_error(
                agent_coordination_codes::INBOX_EXPIRED,
                "inbox message has expired",
            ));
        }
        let due = existing
            .expires_at
            .as_deref()
            .and_then(parse_time)
            .is_some_and(|expires| expires <= now);
        if due {
            tx.execute(
                "UPDATE agent_inbox SET status = 'expired' WHERE id = ?1",
                params![message_id],
            )?;
            let message = query_inbox_tx(&tx, message_id)?
                .ok_or_else(|| anyhow!("inbox message disappeared during expiry"))?;
            insert_inbox_event_tx(
                &tx,
                &message.project_id,
                vp_protocol::types::AGENT_INBOX_EXPIRED_EVENT,
                &message,
            )?;
            tx.commit()?;
            return Err(coordination_error(
                agent_coordination_codes::INBOX_EXPIRED,
                "inbox message has expired",
            ));
        }
        let target = if ack { "done" } else { "read" };
        if existing.status == target || (existing.status == "done" && !ack) {
            tx.commit()?;
            return Ok((existing, false));
        }
        if existing.status != "unread" && existing.status != "read" {
            return Err(coordination_error(
                agent_coordination_codes::INBOX_CONFLICT,
                format!(
                    "inbox message cannot move from {} to {target}",
                    existing.status
                ),
            ));
        }
        tx.execute(
            "UPDATE agent_inbox
             SET status = ?1, read_at = COALESCE(read_at, ?2)
             WHERE id = ?3 AND status IN ('unread', 'read')",
            params![target, now_text, message_id],
        )?;
        let message = query_inbox_tx(&tx, message_id)?
            .ok_or_else(|| anyhow!("inbox message disappeared during transition"))?;
        insert_inbox_event_tx(&tx, &message.project_id, event_kind, &message)?;
        tx.commit()?;
        Ok((message, true))
    }

    /// Expire unread/read rows whose TTL passed. One bounded event per row.
    pub fn expire_due_agent_inbox(
        &self,
        limit: Option<u32>,
    ) -> Result<Vec<AgentCoordinationEvent>> {
        self.expire_due_agent_inbox_at(OffsetDateTime::now_utc(), limit)
    }

    pub fn expire_due_agent_inbox_at(
        &self,
        now: OffsetDateTime,
        limit: Option<u32>,
    ) -> Result<Vec<AgentCoordinationEvent>> {
        let limit = limit.unwrap_or(100).clamp(1, 1_000) as i64;
        let now_text = format_time(now)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ids = {
            let mut statement = tx.prepare(
                "SELECT id FROM agent_inbox
                 WHERE status IN ('unread', 'read')
                   AND expires_at IS NOT NULL
                   AND julianday(expires_at) <= julianday(?1)
                 ORDER BY expires_at ASC, id ASC
                 LIMIT ?2",
            )?;
            statement
                .query_map(params![now_text, limit], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut events = Vec::with_capacity(ids.len());
        for id in ids {
            tx.execute(
                "UPDATE agent_inbox SET status = 'expired'
                 WHERE id = ?1 AND status IN ('unread', 'read')",
                params![id],
            )?;
            let message = query_inbox_tx(&tx, &id)?
                .ok_or_else(|| anyhow!("inbox message disappeared during expiry sweep"))?;
            events.push(insert_inbox_event_tx(
                &tx,
                &message.project_id,
                vp_protocol::types::AGENT_INBOX_EXPIRED_EVENT,
                &message,
            )?);
        }
        tx.commit()?;
        Ok(events)
    }

    // ---------------------------------------------------------------------
    // Replay, repair, manifest, and bundle snapshots
    // ---------------------------------------------------------------------

    pub fn list_agent_coordination_events_for_project(
        &self,
        project_id: &str,
        limit: Option<u32>,
    ) -> Result<Vec<AgentCoordinationEvent>> {
        let limit = limit
            .unwrap_or(AGENT_COORDINATION_EVENT_LIMIT)
            .clamp(1, AGENT_COORDINATION_EVENT_LIMIT) as i64;
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT id, project_id, seq, kind, payload, created_at
             FROM agent_coordination_events
             WHERE project_id = ?1
             ORDER BY seq ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![project_id, limit], map_coordination_event)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Full bounded project projection for replay comparison. Normal read APIs
    /// clamp tighter; replay must see every row that the event stream can
    /// derive or a repair could otherwise hide drift.
    pub fn list_agent_assignments_for_replay(
        &self,
        project_id: &str,
    ) -> Result<Vec<AgentAssignment>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(&format!(
            "{ASSIGNMENT_SELECT}
             WHERE s.project_id = ?1
             ORDER BY a.session_id ASC, a.identity_id ASC
             LIMIT ?2"
        ))?;
        let rows = statement.query_map(
            params![project_id, AGENT_COORDINATION_EVENT_LIMIT as i64],
            map_assignment,
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn list_agent_inbox_for_replay(&self, project_id: &str) -> Result<Vec<AgentInboxMessage>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT id, project_id, identity_id, session_id, from_identity_id, kind,
                    payload_json, status, created_at, read_at, expires_at
             FROM agent_inbox
             WHERE project_id = ?1
             ORDER BY created_at ASC, id ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![project_id, AGENT_COORDINATION_EVENT_LIMIT as i64],
            map_inbox_message,
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn insert_agent_coordination_event_snapshot(
        &self,
        event: &AgentCoordinationEvent,
    ) -> Result<()> {
        validate_coordination_event(event)?;
        let conn = self.lock()?;
        conn.execute(
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
        Ok(())
    }

    pub fn insert_session_agent_assignment_snapshot(
        &self,
        assignment: &AgentAssignment,
    ) -> Result<()> {
        validate_assignment_snapshot(assignment)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let identity = identity_row_tx(&tx, &assignment.identity_id)?;
        if identity.project_id != assignment.project_id {
            return Err(coordination_error(
                agent_coordination_codes::ASSIGNMENT_PROJECT_MISMATCH,
                "assignment snapshot references an identity in another project",
            ));
        }
        insert_assignment_row_tx(&tx, assignment)?;
        tx.commit()?;
        Ok(())
    }

    pub fn insert_agent_inbox_snapshot(&self, message: &AgentInboxMessage) -> Result<()> {
        validate_inbox_snapshot(message)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let identity = identity_row_tx(&tx, &message.identity_id)?;
        if identity.project_id != message.project_id {
            return Err(coordination_error(
                agent_coordination_codes::INBOX_PROJECT_MISMATCH,
                "inbox snapshot references an identity in another project",
            ));
        }
        insert_inbox_row_tx(&tx, message)?;
        tx.commit()?;
        Ok(())
    }

    /// Replace the project projection from replayed events. Never appends an
    /// event. Live lease tokens are preserved by `(session, identity)`; newly
    /// derived active rows receive a fresh internal token.
    pub fn replace_agent_coordination_projection(
        &self,
        project_id: &str,
        assignments: &[AgentAssignment],
        inbox: &[AgentInboxMessage],
    ) -> Result<()> {
        let identity_projects = {
            let conn = self.lock()?;
            let mut statement =
                conn.prepare("SELECT id, project_id FROM agent_identities WHERE project_id = ?1")?;
            let rows = statement.query_map(params![project_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<Result<HashMap<_, _>, _>>()?
        };
        let mut active_identities = BTreeSet::new();
        for assignment in assignments {
            if identity_projects.get(&assignment.identity_id) != Some(&project_id.to_string()) {
                return Err(coordination_error(
                    agent_coordination_codes::ASSIGNMENT_PROJECT_MISMATCH,
                    format!(
                        "replayed assignment {}:{} references an identity outside the project",
                        assignment.session_id, assignment.identity_id
                    ),
                ));
            }
            if assignment.status == "active"
                && !active_identities.insert(assignment.identity_id.clone())
            {
                return Err(coordination_error(
                    agent_coordination_codes::ASSIGNMENT_CONFLICT,
                    format!(
                        "replayed projection has two active assignments for identity {}",
                        assignment.identity_id
                    ),
                ));
            }
        }
        for message in inbox {
            if message.project_id != project_id {
                return Err(coordination_error(
                    agent_coordination_codes::INBOX_PROJECT_MISMATCH,
                    format!(
                        "replayed inbox message {} belongs to another project",
                        message.id
                    ),
                ));
            }
            if identity_projects.get(&message.identity_id) != Some(&project_id.to_string()) {
                return Err(coordination_error(
                    agent_coordination_codes::INBOX_RECIPIENT_INVALID,
                    format!(
                        "replayed inbox message {} references an identity outside the project",
                        message.id
                    ),
                ));
            }
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let tokens = {
            let mut statement = tx.prepare(
                "SELECT a.session_id, a.identity_id, a.lease_token
                 FROM session_agent_assignments a
                 JOIN agent_identities i ON i.id = a.identity_id
                 WHERE i.project_id = ?1 AND a.status = 'active' AND a.lease_token IS NOT NULL",
            )?;
            statement
                .query_map(params![project_id], |row| {
                    Ok((
                        (row.get::<_, String>(0)?, row.get::<_, String>(1)?),
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<Result<HashMap<_, _>, _>>()?
        };
        tx.execute(
            "DELETE FROM session_agent_assignments
             WHERE identity_id IN (SELECT id FROM agent_identities WHERE project_id = ?1)",
            params![project_id],
        )?;
        tx.execute(
            "DELETE FROM agent_inbox WHERE project_id = ?1",
            params![project_id],
        )?;
        for assignment in assignments {
            let existing_token = tokens
                .get(&(
                    assignment.session_id.clone(),
                    assignment.identity_id.clone(),
                ))
                .cloned();
            let token = match existing_token {
                Some(token) => Some(token),
                None if assignment.status == "active" && assignment.lease_expires_at.is_some() => {
                    Some(new_lease_token())
                }
                None => None,
            };
            insert_assignment_row_with_token_tx(&tx, assignment, token.as_deref())?;
        }
        for message in inbox {
            insert_inbox_row_tx(&tx, message)?;
        }
        tx.commit()?;
        Ok(())
    }
}

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
        payload: serde_json::from_str(&payload_json).unwrap_or(Value::Null),
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
        assert!(same.events.is_empty());

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
        assert_eq!(message.from_label, sender);
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

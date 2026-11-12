//! External gate persistence and the atomic continuation wake.
//!
//! A gate is a local, one-shot wait handle. `gate.open` returns a token once;
//! the store keeps only its SHA-256 verifier. Resolving, expiring, or
//! cancelling a gate and making the watching continuation ready commit in one
//! transaction, so a crash cannot record a terminal gate while leaving a
//! pending wait orphaned. The gate transition itself is an event; the
//! continuation ready event and the `gate.continuation_ready` observation are
//! committed with it.
//!
//! This module never sees a raw token. Callers verify the token hash in the
//! agent layer and pass the expected hash into an atomic compare-and-set.

use anyhow::{Result, anyhow};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use vp_protocol::now_rfc3339;
use vp_protocol::types::{
    Event, ExternalGate, GATE_CANCELLED_EVENT, GATE_CONTINUATION_READY_EVENT, GATE_EXPIRED_EVENT,
    GATE_OPENED_EVENT, GATE_RESOLVED_EVENT, GateCounts, GateOutcome, GateRecord, GateState,
    SessionContinuation,
};

use super::{
    SESSION_CONTINUATION_COLUMNS, Store, insert_event_tx, map_session_continuation,
    query_session_continuation_tx,
};

const MAX_GATE_PAYLOAD_BYTES: usize = 16 * 1024;

/// Result of a terminal gate transition. `changed` is false only for an exact
/// idempotent replay or a sweep touching a row that was already terminal.
#[derive(Debug, Clone)]
pub struct GateTransition {
    pub gate: GateRecord,
    pub changed: bool,
    pub events: Vec<Event>,
    pub continuation: Option<SessionContinuation>,
}

impl Store {
    /// Persist a newly opened gate and its event atomically. The caller owns
    /// token generation and hashing; the store only accepts the verifier.
    pub fn create_gate_with_event(&self, record: &GateRecord) -> Result<(GateRecord, Event)> {
        validate_gate_record(record)?;
        if record.gate.state != GateState::Open {
            return Err(anyhow!("a new external gate must start in state open"));
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let session_project: Option<String> = tx
            .query_row(
                "SELECT project_id FROM sessions WHERE id = ?1",
                params![record.gate.session_id],
                |row| row.get(0),
            )
            .optional()?;
        if session_project.is_none() {
            return Err(anyhow!("session not found: {}", record.gate.session_id));
        }
        insert_gate_tx(&tx, record)?;
        let event = insert_event_tx(
            &tx,
            &record.gate.session_id,
            GATE_OPENED_EVENT,
            gate_event_payload(record)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((record.clone(), event))
    }

    pub fn get_gate(&self, id: &str) -> Result<Option<GateRecord>> {
        let conn = self.lock()?;
        gate_by_id_tx(&conn, id)
    }

    pub fn list_gates(
        &self,
        session_id: &str,
        state: Option<GateState>,
        limit: u32,
    ) -> Result<Vec<GateRecord>> {
        let limit = limit.clamp(1, 500) as i64;
        let state = state.map(|state| state.as_str());
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, session_id, task_id, kind, label, token_hash, state, outcome,
                    payload_json, resolution_note, created_at, expires_at, resolved_at,
                    resolved_by
             FROM external_gates
             WHERE session_id = ?1 AND (?2 IS NULL OR state = ?2)
             ORDER BY created_at DESC, id DESC
             LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![session_id, state, limit], map_gate_record)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn gate_counts(&self, session_id: &str) -> Result<GateCounts> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT state, COUNT(*) FROM external_gates WHERE session_id = ?1 GROUP BY state",
        )?;
        let rows = stmt
            .query_map(params![session_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut counts = GateCounts::default();
        for (state, count) in rows {
            let count = count.max(0) as u64;
            counts.total = counts.total.saturating_add(count);
            match state.as_str() {
                "open" => counts.open = count,
                "resolved" => counts.resolved = count,
                "expired" => counts.expired = count,
                "cancelled" => counts.cancelled = count,
                _ => {}
            }
        }
        Ok(counts)
    }

    /// Open gates whose expiry is due. The sweep is bounded and never returns
    /// rows that another process already transitioned.
    pub fn list_expired_open_gates(&self, now: &str, limit: u32) -> Result<Vec<GateRecord>> {
        let limit = limit.clamp(1, 500) as i64;
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, session_id, task_id, kind, label, token_hash, state, outcome,
                    payload_json, resolution_note, created_at, expires_at, resolved_at,
                    resolved_by
             FROM external_gates
             WHERE state = 'open'
               AND expires_at IS NOT NULL
               AND julianday(expires_at) <= julianday(?1)
             ORDER BY expires_at ASC, id ASC
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![now, limit], map_gate_record)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Resolve an open gate and wake its watching continuation atomically.
    /// `token_hash` is the verifier of the caller's token; it must match the
    /// stored verifier even for an idempotent replay.
    pub fn resolve_gate(
        &self,
        gate_id: &str,
        token_hash: &str,
        outcome: GateOutcome,
        note: Option<&str>,
        resolved_by: Option<&str>,
    ) -> Result<GateTransition> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = gate_by_id_tx(&tx, gate_id)?
            .ok_or_else(|| anyhow!("GATE_NOT_FOUND: gate {gate_id} does not exist"))?;
        if !constant_time_hash_eq(&existing.token_hash, token_hash) {
            return Err(anyhow!(
                "GATE_TOKEN_INVALID: the gate token does not authorize resolution"
            ));
        }
        match existing.gate.state {
            GateState::Open => {}
            GateState::Resolved => {
                if existing.gate.outcome == Some(outcome) {
                    return Ok(GateTransition {
                        gate: existing,
                        changed: false,
                        events: Vec::new(),
                        continuation: None,
                    });
                }
                return Err(anyhow!(
                    "GATE_CONFLICT: gate {gate_id} was already resolved with a different outcome"
                ));
            }
            GateState::Expired => {
                return Err(anyhow!("GATE_EXPIRED: gate {gate_id} expired"));
            }
            GateState::Cancelled => {
                return Err(anyhow!("GATE_CANCELLED: gate {gate_id} was cancelled"));
            }
        }
        let note = bounded_note(note)?;
        let resolved_by = bounded_actor(resolved_by)?;
        let changed = tx.execute(
            "UPDATE external_gates
             SET state = 'resolved', outcome = ?2, resolution_note = ?3,
                 resolved_at = ?4, resolved_by = ?5
             WHERE id = ?1 AND state = 'open' AND token_hash = ?6",
            params![
                gate_id,
                outcome.as_str(),
                note,
                now,
                resolved_by,
                token_hash
            ],
        )?;
        if changed == 0 {
            return Err(anyhow!(
                "GATE_CONFLICT: gate {gate_id} changed while resolving; retry with a fresh status"
            ));
        }
        let mut gate = gate_by_id_tx(&tx, gate_id)?
            .ok_or_else(|| anyhow!("gate disappeared during resolution: {gate_id}"))?;
        gate.gate.resolution_note = note;
        gate.gate.resolved_by = resolved_by;
        let mut events = Vec::new();
        events.push(insert_event_tx(
            &tx,
            &gate.gate.session_id,
            GATE_RESOLVED_EVENT,
            gate_event_payload(&gate)?,
        )?);
        let continuation = wake_gate_continuation_tx(&tx, &gate, &mut events)?;
        tx.commit()?;
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok(GateTransition {
            gate,
            changed: true,
            events,
            continuation,
        })
    }

    /// Expire an open gate. Expiry is terminal and wakes the wait with the
    /// `expired` state; it never resolves a pending approval or a plan task.
    pub fn expire_gate(&self, gate_id: &str) -> Result<GateTransition> {
        self.terminate_gate(gate_id, GateState::Expired, None, None)
    }

    /// Cancel an open gate. Cancellation is terminal; the continuation wakes
    /// so the agent can report the cancellation instead of hanging.
    pub fn cancel_gate(
        &self,
        gate_id: &str,
        reason: Option<&str>,
        cancelled_by: Option<&str>,
    ) -> Result<GateTransition> {
        self.terminate_gate(gate_id, GateState::Cancelled, reason, cancelled_by)
    }

    fn terminate_gate(
        &self,
        gate_id: &str,
        state: GateState,
        reason: Option<&str>,
        actor: Option<&str>,
    ) -> Result<GateTransition> {
        if state == GateState::Open || state == GateState::Resolved {
            return Err(anyhow!("internal: invalid terminal gate state"));
        }
        let now = now_rfc3339();
        let note = bounded_note(reason)?;
        let actor = bounded_actor(actor)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = gate_by_id_tx(&tx, gate_id)?
            .ok_or_else(|| anyhow!("GATE_NOT_FOUND: gate {gate_id} does not exist"))?;
        if existing.gate.state == state {
            return Ok(GateTransition {
                gate: existing,
                changed: false,
                events: Vec::new(),
                continuation: None,
            });
        }
        if existing.gate.state != GateState::Open {
            return Err(anyhow!(
                "GATE_CONFLICT: gate {gate_id} is {} and cannot become {}",
                existing.gate.state.as_str(),
                state.as_str()
            ));
        }
        let changed = tx.execute(
            "UPDATE external_gates
             SET state = ?2, resolution_note = ?3, resolved_at = ?4, resolved_by = ?5
             WHERE id = ?1 AND state = 'open'",
            params![gate_id, state.as_str(), note, now, actor],
        )?;
        if changed == 0 {
            return Err(anyhow!(
                "GATE_CONFLICT: gate {gate_id} changed while transitioning; retry with a fresh status"
            ));
        }
        let mut gate = gate_by_id_tx(&tx, gate_id)?
            .ok_or_else(|| anyhow!("gate disappeared during transition: {gate_id}"))?;
        gate.gate.resolution_note = note;
        gate.gate.resolved_by = actor;
        let kind = match state {
            GateState::Expired => GATE_EXPIRED_EVENT,
            GateState::Cancelled => GATE_CANCELLED_EVENT,
            GateState::Open | GateState::Resolved => unreachable!("validated above"),
        };
        let mut events = vec![insert_event_tx(
            &tx,
            &gate.gate.session_id,
            kind,
            gate_event_payload(&gate)?,
        )?];
        let continuation = wake_gate_continuation_tx(&tx, &gate, &mut events)?;
        tx.commit()?;
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok(GateTransition {
            gate,
            changed: true,
            events,
            continuation,
        })
    }

    /// Replace one session's gate projection from replayed events. Used by
    /// replay/repair, bundle import, and `vp db-rebuild`.
    pub fn replace_gates(&self, session_id: &str, records: &[GateRecord]) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM external_gates WHERE session_id = ?1",
            params![session_id],
        )?;
        for record in records {
            if record.gate.session_id != session_id {
                return Err(anyhow!(
                    "external gate {} belongs to a different session",
                    record.gate.id
                ));
            }
            validate_gate_record(record)?;
            insert_gate_tx(&tx, record)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The continuation currently watching a gate, if any. It is used by
    /// `gate.status`; terminal rows remain visible for diagnostics.
    pub fn gate_continuation(&self, gate_id: &str) -> Result<Option<SessionContinuation>> {
        let conn = self.lock()?;
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {SESSION_CONTINUATION_COLUMNS}
                     FROM session_continuations
                     WHERE watch_gate_id = ?1
                     ORDER BY created_at DESC, id DESC
                     LIMIT 1"
                ),
                params![gate_id],
                map_session_continuation,
            )
            .optional()?)
    }

    /// True while a continuation is pending, ready, or delivering for a gate.
    /// The partial unique index is the hard guard; this method provides a
    /// typed early error for the tool/operator surface.
    pub fn has_open_gate_watch(&self, gate_id: &str) -> Result<bool> {
        let conn = self.lock()?;
        let found: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM session_continuations
                 WHERE watch_gate_id = ?1
                   AND status IN ('pending', 'ready', 'delivering')
                 LIMIT 1",
                params![gate_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }
}

fn wake_gate_continuation_tx(
    tx: &Transaction<'_>,
    record: &GateRecord,
    events: &mut Vec<Event>,
) -> Result<Option<SessionContinuation>> {
    let now = now_rfc3339();
    let gate_id = &record.gate.id;
    let pending = {
        let mut stmt = tx.prepare(&format!(
            "SELECT {SESSION_CONTINUATION_COLUMNS}
             FROM session_continuations
             WHERE watch_gate_id = ?1 AND status = 'pending'
             ORDER BY created_at ASC, id ASC
             LIMIT 1"
        ))?;
        stmt.query_row(params![gate_id], map_session_continuation)
            .optional()?
    };
    let Some(pending) = pending else {
        return Ok(None);
    };
    let changed = tx.execute(
        "UPDATE session_continuations
         SET status = 'ready', wake_reason = 'gate', ready_at = ?1, updated_at = ?1,
             next_attempt_at = NULL, lease_owner = NULL, lease_token = NULL,
             lease_expires_at = NULL
         WHERE id = ?2 AND status = 'pending'",
        params![now, pending.id],
    )?;
    if changed == 0 {
        return Ok(None);
    }
    let continuation = query_session_continuation_tx(tx, &pending.id)?
        .ok_or_else(|| anyhow!("continuation disappeared during gate wake: {}", pending.id))?;
    events.push(insert_event_tx(
        tx,
        &continuation.session_id,
        "session.continuation.ready",
        serde_json::to_value(&continuation)?,
    )?);
    events.push(insert_event_tx(
        tx,
        &continuation.session_id,
        GATE_CONTINUATION_READY_EVENT,
        serde_json::json!({
            "gate_id": record.gate.id,
            "continuation_id": continuation.id,
            "state": record.gate.state.as_str(),
            "outcome": record.gate.outcome.map(|outcome| outcome.as_str()),
        }),
    )?);
    Ok(Some(continuation))
}

fn gate_event_payload(record: &GateRecord) -> Result<serde_json::Value> {
    Ok(serde_json::json!({
        "gate": serde_json::to_value(&record.gate)?,
        "token_hash": record.token_hash,
    }))
}

pub(super) fn validate_gate_record(record: &GateRecord) -> Result<()> {
    if record.gate.id.trim().is_empty() {
        return Err(anyhow!("external gate id is required"));
    }
    if record.gate.session_id.trim().is_empty() {
        return Err(anyhow!("external gate session_id is required"));
    }
    if record.gate.label.trim().is_empty() || record.gate.label.chars().count() > 200 {
        return Err(anyhow!("external gate label must be 1-200 characters"));
    }
    if record.token_hash.len() != 64
        || !record
            .token_hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(anyhow!(
            "external gate token_hash must be a 64-character SHA-256 hex digest"
        ));
    }
    let payload = serde_json::to_vec(&record.gate.payload)?;
    if payload.len() > MAX_GATE_PAYLOAD_BYTES {
        return Err(anyhow!("external gate payload is too large"));
    }
    if let Some(note) = &record.gate.resolution_note
        && note.chars().count() > 2_000
    {
        return Err(anyhow!("external gate resolution note is too long"));
    }
    Ok(())
}

fn gate_by_id_tx(conn: &rusqlite::Connection, id: &str) -> Result<Option<GateRecord>> {
    Ok(conn
        .query_row(
            "SELECT id, session_id, task_id, kind, label, token_hash, state, outcome,
                    payload_json, resolution_note, created_at, expires_at, resolved_at,
                    resolved_by
             FROM external_gates WHERE id = ?1",
            params![id],
            map_gate_record,
        )
        .optional()?)
}

pub(super) fn insert_gate_tx(tx: &Transaction<'_>, record: &GateRecord) -> Result<()> {
    tx.execute(
        "INSERT INTO external_gates
         (id, session_id, task_id, kind, label, token_hash, state, outcome,
          payload_json, resolution_note, created_at, expires_at, resolved_at, resolved_by)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            record.gate.id,
            record.gate.session_id,
            record.gate.task_id,
            record.gate.kind.as_str(),
            record.gate.label,
            record.token_hash,
            record.gate.state.as_str(),
            record.gate.outcome.map(|outcome| outcome.as_str()),
            serde_json::to_string(&record.gate.payload)?,
            record.gate.resolution_note,
            record.gate.created_at,
            record.gate.expires_at,
            record.gate.resolved_at,
            record.gate.resolved_by
        ],
    )?;
    Ok(())
}

fn map_gate_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<GateRecord> {
    let kind = row.get::<_, String>(3)?;
    let state = row.get::<_, String>(6)?;
    let outcome = row.get::<_, Option<String>>(7)?;
    let payload = row.get::<_, String>(8)?;
    Ok(GateRecord {
        gate: ExternalGate {
            id: row.get(0)?,
            session_id: row.get(1)?,
            task_id: row.get(2)?,
            kind: vp_protocol::types::GateKind::parse(&kind).unwrap_or_default(),
            label: row.get(4)?,
            // An unknown stored state fails closed as terminal. A corrupt row
            // must never be treated as an open, resolvable gate.
            state: GateState::parse(&state).unwrap_or(GateState::Cancelled),
            outcome: outcome.and_then(|outcome| GateOutcome::parse(&outcome)),
            payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
            resolution_note: row.get(9)?,
            created_at: row.get(10)?,
            expires_at: row.get(11)?,
            resolved_at: row.get(12)?,
            resolved_by: row.get(13)?,
        },
        token_hash: row.get(5)?,
    })
}

fn bounded_note(note: Option<&str>) -> Result<Option<String>> {
    let Some(note) = note else {
        return Ok(None);
    };
    let note = note.trim();
    if note.is_empty() {
        return Ok(None);
    }
    if note.chars().count() > 2_000 {
        return Err(anyhow!("gate resolution note must be at most 2000 chars"));
    }
    Ok(Some(note.to_string()))
}

fn bounded_actor(actor: Option<&str>) -> Result<Option<String>> {
    let Some(actor) = actor else {
        return Ok(None);
    };
    let actor = actor.trim();
    if actor.is_empty() {
        return Ok(None);
    }
    if actor.chars().count() > 200 {
        return Err(anyhow!("gate actor label must be at most 200 chars"));
    }
    Ok(Some(actor.to_string()))
}

fn constant_time_hash_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (left, right) in left.as_bytes().iter().zip(right.as_bytes()) {
        difference |= left ^ right;
    }
    difference == 0
}

/// Replay gate records from `gate.opened`/`gate.resolved`/`gate.expired`/
/// `gate.cancelled`. `None` means a gate payload was malformed, so replay
/// reports drift instead of silently dropping the gate (and its one-shot
/// verifier).
pub fn gates_from_events(events: &[Event]) -> Option<Vec<GateRecord>> {
    let mut rows: std::collections::BTreeMap<String, GateRecord> =
        std::collections::BTreeMap::new();
    for event in events {
        if !matches!(
            event.kind.as_str(),
            GATE_OPENED_EVENT | GATE_RESOLVED_EVENT | GATE_EXPIRED_EVENT | GATE_CANCELLED_EVENT
        ) {
            continue;
        }
        let gate = event.payload.get("gate")?;
        let gate = serde_json::from_value::<ExternalGate>(gate.clone()).ok()?;
        let token_hash = event
            .payload
            .get("token_hash")
            .and_then(serde_json::Value::as_str)?;
        if gate.session_id != event.session_id {
            return None;
        }
        rows.insert(
            gate.id.clone(),
            GateRecord {
                gate,
                token_hash: token_hash.to_string(),
            },
        );
    }
    let mut rows = rows.into_values().collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        left.gate
            .created_at
            .cmp(&right.gate.created_at)
            .then_with(|| left.gate.id.cmp(&right.gate.id))
    });
    Some(rows)
}

impl Store {
    /// Convenience used by bundle import and tests. It delegates to the same
    /// validated replacement path as replay repair.
    pub fn import_gate_records(&self, session_id: &str, records: &[GateRecord]) -> Result<()> {
        self.replace_gates(session_id, records)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (Store, String, String, TempDir) {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("gates"))
            .unwrap();
        let session = store.create_session(&project.id, "gates").unwrap();
        (store, project.id, session.id, data)
    }

    fn record(session_id: &str, id: &str, hash: &str) -> GateRecord {
        GateRecord {
            gate: ExternalGate {
                id: id.to_string(),
                session_id: session_id.to_string(),
                kind: vp_protocol::types::GateKind::Ci,
                label: "unit tests".to_string(),
                state: GateState::Open,
                created_at: "2026-11-12T00:00:00Z".to_string(),
                ..ExternalGate::default()
            },
            token_hash: hash.to_string(),
        }
    }

    #[test]
    fn gate_round_trips_and_rejects_bad_verifier() {
        let (store, _, session, _) = setup();
        let hash = "a".repeat(64);
        let (stored, event) = store
            .create_gate_with_event(&record(&session, "g1", &hash))
            .unwrap();
        assert_eq!(stored.gate.state, GateState::Open);
        assert_eq!(event.kind, GATE_OPENED_EVENT);
        let fetched = store.get_gate("g1").unwrap().unwrap();
        assert_eq!(fetched.token_hash, hash);
        assert!(
            !serde_json::to_string(&fetched.gate)
                .unwrap()
                .contains("token_hash")
        );
        assert!(
            store
                .create_gate_with_event(&record(&session, "g2", "short"))
                .is_err()
        );
    }

    #[test]
    fn resolve_is_one_shot_idempotent_and_wakes_one_continuation() {
        let (store, project, session, _) = setup();
        let hash = "b".repeat(64);
        store
            .create_gate_with_event(&record(&session, "g1", &hash))
            .unwrap();
        let (continuation, _) = store
            .create_session_continuation_with_event(&super::super::NewSessionContinuation {
                session_id: session.clone(),
                project_id: project,
                body: "resume after ci".into(),
                watch_task_ids: Vec::new(),
                watch_mode: "all".into(),
                watch_gate_id: Some("g1".into()),
                deadline_at: None,
                max_attempts: 3,
                series_id: None,
                parent_continuation_id: None,
                recurrence_every_secs: None,
                recurrence_max_runs: 1,
                run_count: 0,
            })
            .unwrap();

        let transition = store
            .resolve_gate(
                "g1",
                &hash,
                GateOutcome::Resolved,
                Some("green"),
                Some("ci"),
            )
            .unwrap();
        assert!(transition.changed);
        assert_eq!(transition.gate.gate.state, GateState::Resolved);
        assert_eq!(
            transition.continuation.as_ref().unwrap().id,
            continuation.id
        );
        assert_eq!(transition.continuation.as_ref().unwrap().status, "ready");
        assert!(
            transition
                .events
                .iter()
                .any(|event| event.kind == GATE_CONTINUATION_READY_EVENT)
        );

        let replay = store
            .resolve_gate("g1", &hash, GateOutcome::Resolved, None, None)
            .unwrap();
        assert!(!replay.changed);
        assert!(replay.events.is_empty());
        assert!(
            store
                .resolve_gate("g1", &hash, GateOutcome::Rejected, None, None)
                .is_err()
        );
        assert!(
            store
                .resolve_gate("g1", &"c".repeat(64), GateOutcome::Resolved, None, None)
                .is_err()
        );
    }

    #[test]
    fn expire_and_cancel_are_terminal_and_wake_the_wait() {
        let (store, project, session, _) = setup();
        for id in ["g-expire", "g-cancel"] {
            store
                .create_gate_with_event(&record(&session, id, &"d".repeat(64)))
                .unwrap();
            store
                .create_session_continuation_with_event(&super::super::NewSessionContinuation {
                    session_id: session.clone(),
                    project_id: project.clone(),
                    body: format!("wait {id}"),
                    watch_task_ids: Vec::new(),
                    watch_mode: "all".into(),
                    watch_gate_id: Some(id.to_string()),
                    deadline_at: None,
                    max_attempts: 3,
                    series_id: None,
                    parent_continuation_id: None,
                    recurrence_every_secs: None,
                    recurrence_max_runs: 1,
                    run_count: 0,
                })
                .unwrap();
        }
        let expired = store.expire_gate("g-expire").unwrap();
        assert!(expired.changed);
        assert_eq!(expired.gate.gate.state, GateState::Expired);
        assert_eq!(
            expired.continuation.unwrap().wake_reason.as_deref(),
            Some("gate")
        );
        let cancelled = store
            .cancel_gate("g-cancel", Some("operator"), Some("cli"))
            .unwrap();
        assert!(cancelled.changed);
        assert_eq!(cancelled.gate.gate.state, GateState::Cancelled);
        assert_eq!(
            cancelled.gate.gate.resolution_note.as_deref(),
            Some("operator")
        );
        assert!(
            store
                .resolve_gate(
                    "g-expire",
                    &"d".repeat(64),
                    GateOutcome::Resolved,
                    None,
                    None
                )
                .is_err()
        );
        assert!(!store.cancel_gate("g-cancel", None, None).unwrap().changed);
        assert!(store.expire_gate("g-cancel").is_err());
    }

    #[test]
    fn replay_rebuilds_gates_and_counts() {
        let (store, _, session, _) = setup();
        let hash = "e".repeat(64);
        let (_, opened) = store
            .create_gate_with_event(&record(&session, "g1", &hash))
            .unwrap();
        let transition = store
            .resolve_gate("g1", &hash, GateOutcome::Resolved, None, Some("ci"))
            .unwrap();
        let resolved_event = transition
            .events
            .into_iter()
            .find(|event| event.kind == GATE_RESOLVED_EVENT)
            .unwrap();
        let events = vec![opened, resolved_event];
        let replayed = gates_from_events(&events).unwrap();
        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0].gate.state, GateState::Resolved);
        assert_eq!(replayed[0].token_hash, hash);

        store.replace_gates(&session, &replayed).unwrap();
        let counts = store.gate_counts(&session).unwrap();
        assert_eq!(counts.total, 1);
        assert_eq!(counts.resolved, 1);
    }

    #[test]
    fn replay_reports_malformed_gate_payloads_instead_of_dropping_them() {
        let (store, _, session, _) = setup();
        let (_, opened) = store
            .create_gate_with_event(&record(&session, "g1", &"a".repeat(64)))
            .unwrap();
        let mut malformed = opened.clone();
        malformed.payload = serde_json::json!({
            "gate": {
                "id": "g1",
                "session_id": session,
                "kind": "ci",
                "label": "unit tests",
                "state": "open"
            }
        });
        assert!(gates_from_events(&[malformed]).is_none());
        assert!(gates_from_events(&[opened]).is_some());
    }

    #[test]
    fn gate_continuation_returns_the_watching_row_across_resolution() {
        let (store, project, session, _) = setup();
        let hash = "a".repeat(64);
        store
            .create_gate_with_event(&record(&session, "g1", &hash))
            .unwrap();
        let (continuation, _) = store
            .create_session_continuation_with_event(&super::super::NewSessionContinuation {
                session_id: session.clone(),
                project_id: project,
                body: "wait".into(),
                watch_task_ids: Vec::new(),
                watch_mode: "all".into(),
                watch_gate_id: Some("g1".into()),
                deadline_at: None,
                max_attempts: 3,
                series_id: None,
                parent_continuation_id: None,
                recurrence_every_secs: None,
                recurrence_max_runs: 1,
                run_count: 0,
            })
            .unwrap();
        let watched = store.gate_continuation("g1").unwrap().unwrap();
        assert_eq!(watched.id, continuation.id);
        assert_eq!(watched.status, "pending");
        store
            .resolve_gate("g1", &hash, GateOutcome::Resolved, None, Some("ci"))
            .unwrap();
        let watched = store.gate_continuation("g1").unwrap().unwrap();
        assert_eq!(watched.status, "ready");
        assert_eq!(watched.wake_reason.as_deref(), Some("gate"));
        assert!(store.gate_continuation("missing").unwrap().is_none());
    }

    #[test]
    fn gate_policy_defaults_off_and_persists() {
        let (store, project, _, _) = setup();
        assert!(!store.get_gate_policy(&project).unwrap().loopback_resolve);
        store
            .set_gate_policy(
                &project,
                &vp_protocol::types::GatePolicy {
                    loopback_resolve: true,
                },
            )
            .unwrap();
        assert!(store.get_gate_policy(&project).unwrap().loopback_resolve);
    }

    #[test]
    fn expired_gate_selection_is_bounded_and_ordered() {
        let (store, _, session, _) = setup();
        for (id, expires_at) in [
            ("g-late", "2026-11-12T02:00:00Z"),
            ("g-early", "2026-11-12T00:30:00Z"),
            ("g-never", ""),
        ] {
            let mut record = record(&session, id, &"f".repeat(64));
            record.gate.expires_at = (!expires_at.is_empty()).then(|| expires_at.to_string());
            store.create_gate_with_event(&record).unwrap();
        }
        let due = store
            .list_expired_open_gates("2026-11-12T01:00:00Z", 10)
            .unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].gate.id, "g-early");
        assert!(
            store
                .list_expired_open_gates("2026-11-12T00:00:00Z", 10)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn gate_replacement_rejects_a_foreign_session_or_bad_hash() {
        let (store, _, session, _) = setup();
        let mut foreign = record("another-session", "g1", &"a".repeat(64));
        assert!(store.replace_gates(&session, &[foreign.clone()]).is_err());
        foreign.gate.session_id = session.clone();
        foreign.token_hash = "short".into();
        assert!(store.replace_gates(&session, &[foreign]).is_err());
    }

    #[test]
    fn bundle_import_copies_gate_records_and_rejects_a_foreign_gate() {
        let (store, _project, session, _) = setup();
        assert!(store.import_gate_records(&session, &[]).is_ok());
        let first = record(&session, "g1", &"a".repeat(64));
        store.import_gate_records(&session, &[first]).unwrap();
        assert_eq!(store.list_gates(&session, None, 10).unwrap().len(), 1);

        let foreign = record("other", "g2", &"b".repeat(64));
        assert!(store.import_gate_records(&session, &[foreign]).is_err());
    }
}

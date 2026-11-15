//! Coordination notes, orchestration decisions, scaling decisions, and recovery attempts.

use super::*;

impl Store {
    /// Record a coordination note and update its projection in one
    /// transaction. Unblock events resolve an existing note (or every open
    /// blocker for a task) and do not create a new note row.
    #[allow(clippy::too_many_arguments)]
    pub fn record_coordination_note_with_event(
        &self,
        session_id: &str,
        action: &str,
        severity: &str,
        task_id: Option<&str>,
        target_agent_id: Option<&str>,
        target_note_id: Option<&str>,
        agent_id: &str,
        agent_name: &str,
        agent_role: Option<&str>,
        message: &str,
    ) -> Result<Event> {
        let note_id = Uuid::now_v7().to_string();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let payload = serde_json::json!({
            "note_id": note_id,
            "action": action,
            "severity": severity,
            "task_id": task_id,
            "target_agent_id": target_agent_id,
            "target_note_id": target_note_id,
            "message": message,
            "agent_id": agent_id,
            "agent_name": agent_name,
            "agent_role": agent_role,
        });
        let event = insert_event_tx(&tx, session_id, "coordination.note", payload)?;
        if action == "unblock" {
            let changed = if let Some(target_note_id) = target_note_id {
                tx.execute(
                    "UPDATE coordination_notes
                     SET status = 'resolved', resolved_at = ?1
                     WHERE note_id = ?2 AND session_id = ?3 AND status = 'open'",
                    params![event.created_at, target_note_id, session_id],
                )?
            } else if let Some(task_id) = task_id {
                tx.execute(
                    "UPDATE coordination_notes
                     SET status = 'resolved', resolved_at = ?1
                     WHERE task_id = ?2 AND session_id = ?3 AND status = 'open'",
                    params![event.created_at, task_id, session_id],
                )?
            } else {
                return Err(anyhow!("an unblock note needs a task id or target note id"));
            };
            if changed == 0 {
                return Err(anyhow!("no open coordination blocker matched the unblock"));
            }
        }
        if action != "unblock" {
            let status = if action == "blocker" { "open" } else { "info" };
            tx.execute(
                "INSERT INTO coordination_notes
                 (note_id, session_id, seq, action, severity, status, task_id,
                  target_agent_id, agent_id, agent_name, message, created_at, resolved_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, NULL)",
                params![
                    note_id,
                    session_id,
                    event.seq as i64,
                    action,
                    severity,
                    status,
                    task_id,
                    target_agent_id,
                    agent_id,
                    agent_name,
                    message,
                    event.created_at
                ],
            )?;
        }
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(event)
    }

    pub fn list_coordination_notes(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<Vec<OrchestrationNote>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT note_id, action, severity, status, task_id, target_agent_id,
                    agent_id, agent_name, message, created_at, resolved_at
             FROM coordination_notes
             WHERE session_id = ?1
             ORDER BY seq DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            params![session_id, i64::from(limit.clamp(1, 2_000))],
            |row| {
                Ok(OrchestrationNote {
                    note_id: row.get(0)?,
                    action: row.get(1)?,
                    severity: row.get(2)?,
                    status: row.get(3)?,
                    task_id: row.get(4)?,
                    target_agent_id: row.get(5)?,
                    agent_id: row.get(6)?,
                    agent_name: row.get(7)?,
                    message: row.get(8)?,
                    created_at: row.get(9)?,
                    resolved_at: row.get(10)?,
                })
            },
        )?;
        let mut notes = rows.collect::<Result<Vec<_>, _>>()?;
        notes.reverse();
        Ok(notes)
    }

    /// Notes that matter to one agent: direct targets, notes on its task,
    /// and shared handoffs/decisions/escalations. This keeps a Worker's brief
    /// focused instead of replaying every note in the session.
    pub fn list_coordination_notes_for_agent(
        &self,
        session_id: &str,
        agent_id: &str,
        task_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<OrchestrationNote>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT note_id, action, severity, status, task_id, target_agent_id,
                    agent_id, agent_name, message, created_at, resolved_at
             FROM coordination_notes
             WHERE session_id = ?1
               AND (
                    target_agent_id = ?2
                    OR (?3 IS NOT NULL AND task_id = ?3)
                    OR (target_agent_id IS NULL AND action IN ('handoff', 'decision', 'escalate'))
               )
             ORDER BY seq DESC
             LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![
                session_id,
                agent_id,
                task_id,
                i64::from(limit.clamp(1, 500))
            ],
            |row| {
                Ok(OrchestrationNote {
                    note_id: row.get(0)?,
                    action: row.get(1)?,
                    severity: row.get(2)?,
                    status: row.get(3)?,
                    task_id: row.get(4)?,
                    target_agent_id: row.get(5)?,
                    agent_id: row.get(6)?,
                    agent_name: row.get(7)?,
                    message: row.get(8)?,
                    created_at: row.get(9)?,
                    resolved_at: row.get(10)?,
                })
            },
        )?;
        let mut notes = rows.collect::<Result<Vec<_>, _>>()?;
        notes.reverse();
        Ok(notes)
    }

    pub fn replace_coordination_notes(
        &self,
        session_id: &str,
        notes: &[OrchestrationNote],
        events: &[Event],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM coordination_notes WHERE session_id = ?1",
            params![session_id],
        )?;
        let seq_by_note = events
            .iter()
            .filter(|event| event.kind == "coordination.note")
            .filter_map(|event| {
                let note_id = event.payload.get("note_id")?.as_str()?;
                Some((note_id.to_string(), event.seq))
            })
            .collect::<std::collections::HashMap<_, _>>();
        for note in notes {
            tx.execute(
                "INSERT INTO coordination_notes
                 (note_id, session_id, seq, action, severity, status, task_id,
                  target_agent_id, agent_id, agent_name, message, created_at, resolved_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    note.note_id,
                    session_id,
                    *seq_by_note.get(&note.note_id).unwrap_or(&0) as i64,
                    note.action,
                    note.severity,
                    note.status,
                    note.task_id,
                    note.target_agent_id,
                    note.agent_id,
                    note.agent_name,
                    note.message,
                    note.created_at,
                    note.resolved_at
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn record_orchestration_decision_with_event(
        &self,
        session_id: &str,
        action: &str,
        task_id: Option<&str>,
        detail: &str,
        attempt: u32,
        agent_id: &str,
    ) -> Result<Event> {
        let decision_id = Uuid::now_v7().to_string();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "orchestration.decision",
            serde_json::json!({
                "decision_id": decision_id,
                "action": action,
                "task_id": task_id,
                "detail": detail,
                "attempt": attempt,
                "agent_id": agent_id,
            }),
        )?;
        tx.execute(
            "INSERT INTO orchestration_decisions
             (decision_id, session_id, seq, action, task_id, detail, attempt, agent_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                decision_id,
                session_id,
                event.seq as i64,
                action,
                task_id,
                detail,
                i64::from(attempt),
                agent_id,
                event.created_at
            ],
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(event)
    }

    pub fn list_orchestration_decisions(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<Vec<OrchestrationDecision>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT decision_id, action, task_id, detail, attempt, agent_id, created_at
             FROM orchestration_decisions
             WHERE session_id = ?1
             ORDER BY seq DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            params![session_id, i64::from(limit.clamp(1, 2_000))],
            |row| {
                Ok(OrchestrationDecision {
                    decision_id: row.get(0)?,
                    action: row.get(1)?,
                    task_id: row.get(2)?,
                    detail: row.get(3)?,
                    attempt: row.get::<_, i64>(4)?.max(0) as u32,
                    agent_id: row.get(5)?,
                    created_at: row.get(6)?,
                })
            },
        )?;
        let mut decisions = rows.collect::<Result<Vec<_>, _>>()?;
        decisions.reverse();
        Ok(decisions)
    }

    pub fn replace_orchestration_decisions(
        &self,
        session_id: &str,
        decisions: &[OrchestrationDecision],
        events: &[Event],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM orchestration_decisions WHERE session_id = ?1",
            params![session_id],
        )?;
        let seq_by_decision = events
            .iter()
            .filter(|event| event.kind == "orchestration.decision")
            .filter_map(|event| {
                let decision_id = event.payload.get("decision_id")?.as_str()?;
                Some((decision_id.to_string(), event.seq))
            })
            .collect::<std::collections::HashMap<_, _>>();
        for decision in decisions {
            tx.execute(
                "INSERT INTO orchestration_decisions
                 (decision_id, session_id, seq, action, task_id, detail, attempt, agent_id, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    decision.decision_id,
                    session_id,
                    *seq_by_decision.get(&decision.decision_id).unwrap_or(&0) as i64,
                    decision.action,
                    decision.task_id,
                    decision.detail,
                    i64::from(decision.attempt),
                    decision.agent_id,
                    decision.created_at
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Persist one deterministic scaling decision and its `scaling.decision`
    /// event atomically. The unique `(session_id, decision_key)` constraint makes
    /// a repeated daemon tick idempotent: the second call returns `None` and
    /// appends no event.
    pub fn record_scaling_decision_with_event(
        &self,
        decision: &ScalingDecision,
    ) -> Result<Option<(ScalingDecision, Event)>> {
        if decision.session_id.is_empty() || decision.decision_key.is_empty() {
            anyhow::bail!("scaling decision needs a session id and decision key");
        }
        let mut stored = decision.clone();
        if stored.decision_id.is_empty() {
            stored.decision_id = Uuid::now_v7().to_string();
        }
        if stored.created_at.is_empty() {
            stored.created_at = now_rfc3339();
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let exists = tx.query_row(
            "SELECT COUNT(*) FROM scaling_decisions
             WHERE session_id = ?1 AND decision_key = ?2",
            params![stored.session_id, stored.decision_key],
            |row| row.get::<_, i64>(0),
        )? > 0;
        if exists {
            return Ok(None);
        }
        let event = insert_event_tx(
            &tx,
            &stored.session_id,
            "scaling.decision",
            serde_json::to_value(&stored)?,
        )?;
        insert_scaling_decision_tx(&tx, &stored, event.seq)?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((stored, event)))
    }

    pub fn list_scaling_decisions(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<Vec<ScalingDecision>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT decision_id, decision_key, project_id, session_id, seq, plan_revision, action,
                    current_workers, current_labour, target_workers, target_labour,
                    active_workers, active_labour, ready_tasks, running_tasks, inputs_hash,
                    reasons, manager_recommended, manager_evidence, applied,
                    idle_since_workers, idle_since_labour, created_at, replay_inputs_json
             FROM scaling_decisions
             WHERE session_id = ?1
             ORDER BY seq DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            params![session_id, i64::from(limit.clamp(1, 10_000))],
            map_scaling_decision,
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn latest_scaling_decision(&self, session_id: &str) -> Result<Option<ScalingDecision>> {
        Ok(self
            .list_scaling_decisions(session_id, 1)?
            .into_iter()
            .next())
    }

    pub fn has_scaling_decision(&self, session_id: &str, decision_key: &str) -> Result<bool> {
        let conn = self.lock()?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM scaling_decisions
             WHERE session_id = ?1 AND decision_key = ?2",
            params![session_id, decision_key],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count > 0)
    }

    /// Replay/export path without an arbitrary limit. A decision is a bounded
    /// row and the tick suppresses repeated Holds, so this stays bounded by
    /// real state transitions rather than wall-clock ticks.
    pub fn list_all_scaling_decisions(&self, session_id: &str) -> Result<Vec<ScalingDecision>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT decision_id, decision_key, project_id, session_id, seq, plan_revision, action,
                    current_workers, current_labour, target_workers, target_labour,
                    active_workers, active_labour, ready_tasks, running_tasks, inputs_hash,
                    reasons, manager_recommended, manager_evidence, applied,
                    idle_since_workers, idle_since_labour, created_at, replay_inputs_json
             FROM scaling_decisions
             WHERE session_id = ?1
             ORDER BY seq DESC",
        )?;
        let rows = stmt.query_map(params![session_id], map_scaling_decision)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn replace_scaling_decisions(
        &self,
        session_id: &str,
        decisions: &[ScalingDecision],
        events: &[Event],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM scaling_decisions WHERE session_id = ?1",
            params![session_id],
        )?;
        let seq_by_decision = events
            .iter()
            .filter(|event| event.kind == "scaling.decision")
            .filter_map(|event| {
                let decision_id = event.payload.get("decision_id")?.as_str()?;
                Some((decision_id.to_string(), event.seq))
            })
            .collect::<std::collections::HashMap<_, _>>();
        for decision in decisions {
            if decision.decision_id.is_empty() {
                continue;
            }
            insert_scaling_decision_tx(
                &tx,
                decision,
                *seq_by_decision.get(&decision.decision_id).unwrap_or(&0),
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn auto_resolve_attempts(&self, entry_id: &str) -> Result<u32> {
        let conn = self.lock()?;
        let attempts = conn
            .query_row(
                "SELECT attempts FROM merge_auto_attempts WHERE entry_id = ?1",
                params![entry_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        Ok(attempts as u32)
    }

    pub fn bump_auto_resolve_attempts(&self, entry_id: &str) -> Result<u32> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO merge_auto_attempts (entry_id, attempts) VALUES (?1, 1)
             ON CONFLICT(entry_id) DO UPDATE SET attempts = attempts + 1",
            params![entry_id],
        )?;
        let attempts = conn.query_row(
            "SELECT attempts FROM merge_auto_attempts WHERE entry_id = ?1",
            params![entry_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(attempts as u32)
    }

    /// Sessions whose plan has failed or blocked rows that the deterministic
    /// controller may be able to resolve. This is a bounded scan for the
    /// daemon tick, not a scheduler priority queue.
    pub fn orchestration_candidate_sessions(&self, limit: u32) -> Result<Vec<String>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT DISTINCT s.id
             FROM sessions s
             JOIN plan_tasks t ON t.session_id = s.id
             WHERE s.team_plan IS NOT NULL
               AND t.status IN ('failed', 'blocked')
             ORDER BY s.created_at ASC, s.id ASC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![i64::from(limit.clamp(1, 500))], |row| {
            row.get::<_, String>(0)
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    // Runtime lease acquisition, fencing, renewal, release, and cleanup live in
    // `store::lease` so the ownership state machine stays in one auditable module.
}

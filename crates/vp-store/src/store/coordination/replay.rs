use super::*;

impl Store {
    // ---------------------------------------------------------------------
    // Replay, repair, manifest, and bundle snapshots
    // ---------------------------------------------------------------------

    pub fn list_agent_coordination_events_for_project(
        &self,
        project_id: &str,
        limit: Option<u32>,
    ) -> Result<Vec<AgentCoordinationEvent>> {
        let limit = limit
            .map(|value| value.clamp(1, AGENT_COORDINATION_EVENT_LIMIT) as i64)
            .unwrap_or(-1);
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

    /// Unread inbox counts grouped by recipient identity. This is a bounded
    /// aggregate for the S7 org chart: it returns counts only, never row bodies.
    pub fn unread_agent_inbox_counts_for_project(
        &self,
        project_id: &str,
    ) -> Result<HashMap<String, u64>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT identity_id, COUNT(*) FROM agent_inbox
             WHERE project_id = ?1 AND status = 'unread'
             GROUP BY identity_id",
        )?;
        let rows = statement.query_map(params![project_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut counts = HashMap::new();
        for row in rows {
            let (identity_id, count) = row?;
            if count > 0 {
                counts.insert(identity_id, count as u64);
            }
        }
        Ok(counts)
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

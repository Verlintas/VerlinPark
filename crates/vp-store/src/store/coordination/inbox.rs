use super::*;

impl Store {
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
        // Display label only. The stable sender id stays in from_identity_id;
        // the label prefers the identity title so the inbox does not show a
        // raw UUID to an operator.
        let from_label = match from_identity_id {
            None => "operator".to_string(),
            Some(id) => tx
                .query_row(
                    "SELECT COALESCE(NULLIF(TRIM(title), ''), id)
                     FROM agent_identities WHERE id = ?1",
                    params![id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .unwrap_or_else(|| id.to_string()),
        };
        let message = AgentInboxMessage {
            id: Uuid::now_v7().to_string(),
            project_id: project_id.to_string(),
            identity_id: identity_id.to_string(),
            session_id: session_id.map(str::to_string),
            from_identity_id: from_identity_id.map(str::to_string),
            from_label,
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
}

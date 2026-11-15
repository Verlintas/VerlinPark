//! Session mailbox policy and messages.

use super::*;

impl Store {
    #[allow(clippy::too_many_arguments)]
    pub fn create_agent_message_with_event(
        &self,
        session_id: &str,
        from_agent_id: &str,
        from_name: &str,
        to_agent_id: &str,
        to_name: &str,
        kind: &str,
        body: &str,
        payload: &serde_json::Value,
    ) -> Result<(AgentMessage, Event)> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let message = AgentMessage {
            id: Uuid::now_v7().to_string(),
            session_id: session_id.to_string(),
            from_agent_id: from_agent_id.to_string(),
            from_name: from_name.to_string(),
            to_agent_id: to_agent_id.to_string(),
            to_name: to_name.to_string(),
            kind: kind.to_string(),
            body: body.to_string(),
            payload: payload.clone(),
            status: "unread".to_string(),
            created_at: now_rfc3339(),
            read_at: None,
        };
        tx.execute(
            "INSERT INTO agent_messages
             (id, session_id, from_agent_id, from_name, to_agent_id, to_name,
              kind, body, payload, status, created_at, read_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, NULL)",
            params![
                message.id,
                message.session_id,
                message.from_agent_id,
                message.from_name,
                message.to_agent_id,
                message.to_name,
                message.kind,
                message.body,
                serde_json::to_string(&message.payload)?,
                message.status,
                message.created_at
            ],
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "mailbox.message_sent",
            serde_json::to_value(&message)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((message, event))
    }

    pub fn list_agent_messages(
        &self,
        session_id: &str,
        to_agent_id: Option<&str>,
        unread_only: bool,
    ) -> Result<Vec<AgentMessage>> {
        let conn = self.lock()?;
        let mut sql = String::from(
            "SELECT id, session_id, from_agent_id, from_name, to_agent_id, to_name,
                    kind, body, payload, status, created_at, read_at
             FROM agent_messages WHERE session_id = ?1",
        );
        if to_agent_id.is_some() {
            sql.push_str(" AND to_agent_id = ?2");
        }
        if unread_only {
            sql.push_str(" AND status = 'unread'");
        }
        sql.push_str(" ORDER BY created_at ASC, id ASC");
        let mut stmt = conn.prepare(&sql)?;
        let rows = match to_agent_id {
            Some(agent_id) => stmt.query_map(params![session_id, agent_id], map_agent_message)?,
            None => stmt.query_map(params![session_id], map_agent_message)?,
        };
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn mark_agent_message_read(
        &self,
        message_id: &str,
    ) -> Result<Option<(AgentMessage, Option<Event>)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let now = now_rfc3339();
        let changed = tx.execute(
            "UPDATE agent_messages SET status = 'read', read_at = ?1
             WHERE id = ?2 AND status = 'unread'",
            params![now, message_id],
        )?;
        let message = tx
            .query_row(
                "SELECT id, session_id, from_agent_id, from_name, to_agent_id, to_name,
                        kind, body, payload, status, created_at, read_at
                 FROM agent_messages WHERE id = ?1",
                params![message_id],
                map_agent_message,
            )
            .optional()?;
        let Some(message) = message else {
            tx.commit()?;
            return Ok(None);
        };
        if changed == 0 {
            tx.commit()?;
            return Ok(Some((message, None)));
        }
        let event = insert_event_tx(
            &tx,
            &message.session_id,
            "mailbox.message_read",
            serde_json::json!({
                "message_id": message.id,
                "to_agent_id": message.to_agent_id,
                "read_at": message.read_at,
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((message, Some(event))))
    }

    pub fn get_mailbox_policy(&self, project_id: &str) -> Result<MailboxPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT max_age_secs, max_count, delete_unread, auto_gc
                 FROM project_mailbox_policies WHERE project_id = ?1",
                params![project_id],
                |row| {
                    Ok(MailboxPolicy {
                        max_age_secs: row.get::<_, Option<i64>>(0)?.map(|value| value as u64),
                        max_count: row.get::<_, Option<i64>>(1)?.map(|value| value as u64),
                        delete_unread: row.get::<_, i64>(2)? != 0,
                        auto_gc: row.get::<_, i64>(3)? != 0,
                    })
                },
            )
            .optional()?;
        Ok(stored.unwrap_or_default())
    }

    pub fn set_mailbox_policy(&self, project_id: &str, policy: &MailboxPolicy) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_mailbox_policies
                (project_id, max_age_secs, max_count, delete_unread, auto_gc, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(project_id) DO UPDATE SET
                max_age_secs = excluded.max_age_secs,
                max_count = excluded.max_count,
                delete_unread = excluded.delete_unread,
                auto_gc = excluded.auto_gc,
                updated_at = excluded.updated_at",
            params![
                project_id,
                policy.max_age_secs.map(|value| value as i64),
                policy.max_count.map(|value| value as i64),
                if policy.delete_unread { 1 } else { 0 },
                if policy.auto_gc { 1 } else { 0 },
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn list_agent_messages_for_project(&self, project_id: &str) -> Result<Vec<AgentMessage>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT m.id, m.session_id, m.from_agent_id, m.from_name, m.to_agent_id,
                    m.to_name, m.kind, m.body, m.payload, m.status, m.created_at, m.read_at
             FROM agent_messages m
             JOIN sessions s ON s.id = m.session_id
             WHERE s.project_id = ?1
             ORDER BY m.created_at ASC, m.id ASC",
        )?;
        let rows = stmt.query_map(params![project_id], map_agent_message)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn delete_agent_message(&self, message_id: &str) -> Result<bool> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "DELETE FROM agent_messages WHERE id = ?1",
            params![message_id],
        )?;
        Ok(changed > 0)
    }

    pub fn delete_agent_message_with_event(&self, message_id: &str) -> Result<Option<Event>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let message = tx
            .query_row(
                "SELECT session_id, from_agent_id, from_name, to_agent_id, to_name, kind
                 FROM agent_messages WHERE id = ?1",
                params![message_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((session_id, from_agent_id, from_name, to_agent_id, to_name, kind)) = message
        else {
            return Ok(None);
        };
        tx.execute(
            "DELETE FROM agent_messages WHERE id = ?1",
            params![message_id],
        )?;
        let event = insert_event_tx(
            &tx,
            &session_id,
            "mailbox.pruned",
            serde_json::json!({
                "message_id": message_id,
                "from_agent_id": from_agent_id,
                "from_name": from_name,
                "to_agent_id": to_agent_id,
                "to_name": to_name,
                "kind": kind,
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some(event))
    }
}

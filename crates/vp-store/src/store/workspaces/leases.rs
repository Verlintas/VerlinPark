use super::*;

impl Store {
    /// Expire every due workspace lease. The periodic daemon sweep and startup
    /// reconciliation use the same operation.
    pub fn expire_agent_workspace_leases(
        &self,
        limit: Option<u32>,
    ) -> Result<Vec<AgentWorkspaceEvent>> {
        self.expire_agent_workspace_leases_at(OffsetDateTime::now_utc(), limit)
    }

    pub fn expire_agent_workspace_leases_at(
        &self,
        now: OffsetDateTime,
        limit: Option<u32>,
    ) -> Result<Vec<AgentWorkspaceEvent>> {
        let limit = limit.unwrap_or(100).clamp(1, 1_000) as i64;
        let now_text = format_time(now)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let rows = {
            let mut statement = tx.prepare(
                "SELECT id FROM agent_workspaces
                 WHERE status = 'active' AND lease_token IS NOT NULL
                   AND (lease_expires_at IS NULL
                        OR julianday(lease_expires_at) <= julianday(?1))
                 ORDER BY lease_expires_at ASC, id ASC
                 LIMIT ?2",
            )?;
            statement
                .query_map(params![now_text, limit], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut events = Vec::with_capacity(rows.len());
        for id in rows {
            if let Some(event) = expire_workspace_if_due_tx(&tx, &id, now)? {
                events.push(event);
            }
        }
        tx.commit()?;
        Ok(events)
    }

    /// Release workspace leases for sessions that are no longer live. Called on
    /// startup and by the periodic coordination sweep so a daemon crash cannot
    /// pin a component forever.
    pub fn reconcile_agent_workspace_leases(
        &self,
        limit: Option<u32>,
    ) -> Result<Vec<AgentWorkspaceEvent>> {
        let limit = limit.unwrap_or(100).clamp(1, 1_000) as i64;
        let now = OffsetDateTime::now_utc();
        let now_text = format_time(now)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let rows = {
            let mut statement = tx.prepare(
                "SELECT w.id, b.session_id, s.status
                 FROM agent_workspaces w
                 JOIN session_worker_workspaces b ON b.workspace_id = w.id
                      AND b.status = 'active'
                 JOIN sessions s ON s.id = b.session_id
                 WHERE w.status = 'active'
                   AND s.status NOT IN ('running', 'queued', 'paused', 'waiting_input')
                 ORDER BY w.updated_at ASC
                 LIMIT ?1",
            )?;
            statement
                .query_map(params![limit], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut events = Vec::new();
        for (workspace_id, session_id, status) in rows {
            tx.execute(
                "UPDATE session_worker_workspaces
                 SET status = 'released', lease_token = NULL, lease_expires_at = NULL,
                     updated_at = ?1
                 WHERE workspace_id = ?2 AND session_id = ?3 AND status = 'active'",
                params![now_text, workspace_id, session_id],
            )?;
            tx.execute(
                "UPDATE agent_workspaces
                 SET lease_token = NULL, lease_expires_at = NULL, updated_at = ?1
                 WHERE id = ?2",
                params![now_text, workspace_id],
            )?;
            let workspace = get_workspace_tx(&tx, &workspace_id)?
                .ok_or_else(|| anyhow!("workspace disappeared during reconciliation"))?;
            let binding: Option<SessionWorkerWorkspace> = tx
                .query_row(
                    &format!(
                        "{SESSION_WORKSPACE_SELECT} WHERE workspace_id = ?1 AND session_id = ?2"
                    ),
                    params![workspace_id, session_id],
                    map_session_workspace,
                )
                .optional()?;
            events.push(insert_workspace_event_tx(
                &tx,
                &workspace.project_id,
                vp_protocol::types::AGENT_WORKSPACE_RELEASED_EVENT,
                &workspace,
                binding.as_ref(),
                Some((
                    "reason",
                    serde_json::json!({
                        "reason": "session_inactive",
                        "session_status": status,
                    }),
                )),
            )?);
        }
        drop(tx);
        drop(conn);
        let mut expired = self.expire_agent_workspace_leases(Some(limit.max(1) as u32))?;
        events.append(&mut expired);
        Ok(events)
    }
}

//! Session/run agent projection rows.

use super::*;

impl Store {
    pub fn create_leader_agent(&self, session_id: &str, model: &str) -> Result<String> {
        Ok(self
            .create_agent(session_id, "leader", "Leader", None, None, model)?
            .id)
    }

    pub fn create_agent(
        &self,
        session_id: &str,
        role: &str,
        name: &str,
        title: Option<&str>,
        parent_id: Option<&str>,
        model: &str,
    ) -> Result<AgentRecord> {
        let agent = AgentRecord {
            id: Uuid::now_v7().to_string(),
            session_id: session_id.to_string(),
            role: role.to_string(),
            name: name.to_string(),
            title: title.map(ToOwned::to_owned),
            identity_id: None,
            workspace_id: None,
            parent_workspace_id: None,
            profile: None,
            parent_id: parent_id.map(ToOwned::to_owned),
            task_id: None,
            department: None,
            worktree: None,
            branch: None,
            status: "running".to_string(),
            model: model.to_string(),
            created_at: now_rfc3339(),
        };
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO agents
             (id, session_id, role, name, title, parent_id, worktree, branch, status, model, created_at,
              task_id, department)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                agent.id,
                agent.session_id,
                agent.role,
                agent.name,
                agent.title,
                agent.parent_id,
                agent.worktree,
                agent.branch,
                agent.status,
                agent.model,
                agent.created_at,
                agent.task_id,
                agent.department
            ],
        )?;
        Ok(agent)
    }

    pub fn create_agent_with_snapshot(
        &self,
        session_id: &str,
        role: &str,
        name: &str,
        title: Option<&str>,
        parent_id: Option<&str>,
        model: &str,
    ) -> Result<(AgentRecord, Event)> {
        let agent = AgentRecord {
            id: Uuid::now_v7().to_string(),
            session_id: session_id.to_string(),
            role: role.to_string(),
            name: name.to_string(),
            title: title.map(ToOwned::to_owned),
            identity_id: None,
            workspace_id: None,
            parent_workspace_id: None,
            profile: None,
            parent_id: parent_id.map(ToOwned::to_owned),
            task_id: None,
            department: None,
            worktree: None,
            branch: None,
            status: "running".to_string(),
            model: model.to_string(),
            created_at: now_rfc3339(),
        };
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO agents
             (id, session_id, role, name, title, parent_id, worktree, branch, status, model, created_at,
              task_id, department)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                agent.id,
                agent.session_id,
                agent.role,
                agent.name,
                agent.title,
                agent.parent_id,
                agent.worktree,
                agent.branch,
                agent.status,
                agent.model,
                agent.created_at,
                agent.task_id,
                agent.department
            ],
        )?;
        let event = insert_event_tx(
            &tx,
            &agent.session_id,
            "agent.snapshot",
            serde_json::to_value(&agent)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((agent, event))
    }

    pub fn create_leader_agent_with_snapshot(
        &self,
        session_id: &str,
        model: &str,
    ) -> Result<(AgentRecord, Event)> {
        self.create_agent_with_snapshot(session_id, "leader", "Leader", None, None, model)
    }

    pub fn set_agent_worktree_with_snapshot(
        &self,
        id: &str,
        worktree: &str,
        branch: &str,
    ) -> Result<Option<(AgentRecord, Event)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE agents SET worktree = ?1, branch = ?2 WHERE id = ?3",
            params![worktree, branch, id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let agent = get_agent_tx(&tx, id)?;
        let event = insert_event_tx(
            &tx,
            &agent.session_id,
            "agent.snapshot",
            serde_json::to_value(&agent)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((agent, event)))
    }

    pub fn set_agent_workspace_binding_with_snapshot(
        &self,
        id: &str,
        workspace_id: Option<&str>,
        parent_workspace_id: Option<&str>,
    ) -> Result<Option<(AgentRecord, Event)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE agents SET workspace_id = ?1, parent_workspace_id = ?2 WHERE id = ?3",
            params![workspace_id, parent_workspace_id, id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let agent = get_agent_tx(&tx, id)?;
        let event = insert_event_tx(
            &tx,
            &agent.session_id,
            "agent.workspace_binding",
            serde_json::json!({
                "agent_id": id,
                "workspace_id": workspace_id,
                "parent_workspace_id": parent_workspace_id,
                "agent": agent,
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((agent, event)))
    }

    /// Persist the project identity resolved for one run. The `agents` row is
    /// the durable run→identity binding S6 attribution prefers; without this
    /// write the column stayed NULL for every real run and performance silently
    /// fell back to role/department heuristics.
    pub fn set_agent_identity_with_snapshot(
        &self,
        id: &str,
        identity_id: Option<&str>,
    ) -> Result<Option<(AgentRecord, Event)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE agents SET identity_id = ?1 WHERE id = ?2",
            params![identity_id, id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let agent = get_agent_tx(&tx, id)?;
        let event = insert_event_tx(
            &tx,
            &agent.session_id,
            "agent.identity_binding",
            serde_json::json!({
                "agent_id": id,
                "identity_id": identity_id,
                "agent": agent,
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((agent, event)))
    }

    pub fn set_agent_profile_with_snapshot(
        &self,
        id: &str,
        profile: &str,
    ) -> Result<Option<(AgentRecord, Event)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE agents SET profile = ?1 WHERE id = ?2",
            params![profile, id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let agent = get_agent_tx(&tx, id)?;
        let event = insert_event_tx(
            &tx,
            &agent.session_id,
            "agent.snapshot",
            serde_json::to_value(&agent)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((agent, event)))
    }

    pub fn set_agent_worktree(&self, id: &str, worktree: &str, branch: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE agents SET worktree = ?1, branch = ?2 WHERE id = ?3",
            params![worktree, branch, id],
        )?;
        Ok(())
    }

    pub fn set_agent_profile(&self, id: &str, profile: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE agents SET profile = ?1 WHERE id = ?2",
            params![profile, id],
        )?;
        Ok(())
    }

    pub fn update_agent_status(&self, id: &str, status: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE agents SET status = ?1 WHERE id = ?2",
            params![status, id],
        )?;
        Ok(())
    }

    pub fn update_agent_status_with_event(&self, id: &str, status: &str) -> Result<Event> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let session_id: String = tx
            .query_row(
                "SELECT session_id FROM agents WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| anyhow!("agent not found: {id}"))?;
        tx.execute(
            "UPDATE agents SET status = ?1 WHERE id = ?2",
            params![status, id],
        )?;
        let agent = get_agent_tx(&tx, id)?;
        let event = insert_event_tx(
            &tx,
            &session_id,
            "agent.updated",
            serde_json::json!({"agent_id": id, "status": status, "agent": agent}),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(event)
    }

    pub fn set_agent_task_with_event(
        &self,
        id: &str,
        task_id: Option<&str>,
        department: Option<&str>,
    ) -> Result<Event> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let session_id: String = tx
            .query_row(
                "SELECT session_id FROM agents WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| anyhow!("agent not found: {id}"))?;
        tx.execute(
            "UPDATE agents SET task_id = ?1, department = ?2 WHERE id = ?3",
            params![task_id, department, id],
        )?;
        let agent = get_agent_tx(&tx, id)?;
        let event = insert_event_tx(
            &tx,
            &session_id,
            "agent.updated",
            serde_json::json!({"agent_id": id, "agent": agent}),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(event)
    }

    pub fn list_agents(&self, session_id: &str) -> Result<Vec<AgentRecord>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, session_id, role, name, title, profile, parent_id, task_id, department,
                    worktree, branch, status, model, created_at, identity_id, workspace_id,
                    parent_workspace_id
             FROM agents WHERE session_id = ?1 ORDER BY created_at ASC, rowid ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok(AgentRecord {
                id: row.get(0)?,
                session_id: row.get(1)?,
                role: row.get(2)?,
                name: row.get(3)?,
                title: row.get(4)?,
                identity_id: row.get(14)?,
                workspace_id: row.get(15)?,
                parent_workspace_id: row.get(16)?,
                profile: row.get(5)?,
                parent_id: row.get(6)?,
                task_id: row.get(7)?,
                department: row.get(8)?,
                worktree: row.get(9)?,
                branch: row.get(10)?,
                status: row.get(11)?,
                model: row.get(12)?,
                created_at: row.get(13)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    // ---------------------------------------------------------------------
    // Project-scoped agent identities
    // ---------------------------------------------------------------------

    pub fn interrupt_running_agents(&self) -> Result<usize> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE agents SET status = 'failed' WHERE status IN ('running', 'waiting_input')",
            [],
        )?;
        Ok(changed)
    }
}

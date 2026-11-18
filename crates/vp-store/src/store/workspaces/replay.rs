use super::*;

impl Store {
    // ---------------------------------------------------------------------
    // Replay, repair, manifest, and bundle snapshots
    // ---------------------------------------------------------------------

    pub fn list_agent_workspace_events_for_project(
        &self,
        project_id: &str,
        limit: Option<u32>,
    ) -> Result<Vec<AgentWorkspaceEvent>> {
        let limit = limit
            .unwrap_or(AGENT_WORKSPACE_EVENT_LIMIT)
            .clamp(1, AGENT_WORKSPACE_EVENT_LIMIT) as i64;
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT id, project_id, seq, kind, payload, created_at
             FROM agent_workspace_events
             WHERE project_id = ?1
             ORDER BY seq ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![project_id, limit], map_workspace_event)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn list_agent_workspaces_for_replay(
        &self,
        project_id: &str,
    ) -> Result<Vec<AgentWorkspace>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(&format!(
            "{WORKSPACE_SELECT} WHERE project_id = ?1 ORDER BY id ASC LIMIT ?2"
        ))?;
        let rows = statement.query_map(
            params![project_id, AGENT_WORKSPACE_EVENT_LIMIT as i64],
            map_workspace,
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn list_session_workspaces_for_replay(
        &self,
        project_id: &str,
    ) -> Result<Vec<SessionWorkerWorkspace>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT s.id, s.workspace_id, s.session_id, s.identity_id, s.worktree_path,
                    s.branch, s.base_commit, s.status, s.created_at, s.updated_at,
                    s.lease_token, s.lease_expires_at
             FROM session_worker_workspaces s
             JOIN agent_workspaces w ON w.id = s.workspace_id
             WHERE w.project_id = ?1
             ORDER BY s.session_id ASC, s.identity_id ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![project_id, AGENT_WORKSPACE_EVENT_LIMIT as i64],
            map_session_workspace,
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn insert_agent_workspace_event_snapshot(&self, event: &AgentWorkspaceEvent) -> Result<()> {
        if !AGENT_WORKSPACE_EVENT_KINDS.contains(&event.kind.as_str()) {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_INVALID_EVENT,
                format!("unknown workspace event kind: {}", event.kind),
            ));
        }
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO agent_workspace_events (id, project_id, seq, kind, payload, created_at)
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

    pub fn insert_agent_workspace_snapshot(&self, workspace: &AgentWorkspace) -> Result<()> {
        validate_workspace_row(workspace)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let project_exists: Option<String> = tx
            .query_row(
                "SELECT id FROM projects WHERE id = ?1",
                params![workspace.project_id],
                |row| row.get(0),
            )
            .optional()?;
        if project_exists.is_none() {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_NOT_FOUND,
                format!("project not found: {}", workspace.project_id),
            ));
        }
        insert_workspace_row_tx(&tx, workspace)?;
        tx.commit()?;
        Ok(())
    }

    pub fn insert_session_worker_workspace_snapshot(
        &self,
        session: &SessionWorkerWorkspace,
    ) -> Result<()> {
        validate_session_row(session)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let workspace = get_workspace_tx(&tx, &session.workspace_id)?.ok_or_else(|| {
            workspace_error(
                agent_workspace_codes::WORKSPACE_NOT_FOUND,
                format!("workspace not found: {}", session.workspace_id),
            )
        })?;
        if workspace.identity_id != session.identity_id {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_CONFLICT,
                "session workspace identity does not match its workspace",
            ));
        }
        insert_session_row_tx(&tx, session)?;
        tx.commit()?;
        Ok(())
    }

    /// Replace the project projection from replayed events. Never appends an
    /// event. All rows are derived; live lease tokens are not replayed and are
    /// cleared, so callers must only repair a quiescent project.
    pub fn replace_agent_workspace_projection(
        &self,
        project_id: &str,
        workspaces: &[AgentWorkspace],
        sessions: &[SessionWorkerWorkspace],
    ) -> Result<()> {
        let mut workspace_ids = BTreeSet::new();
        for workspace in workspaces {
            if workspace.project_id != project_id {
                return Err(workspace_error(
                    agent_workspace_codes::WORKSPACE_PROJECT_MISMATCH,
                    format!(
                        "replayed workspace {} belongs to another project",
                        workspace.id
                    ),
                ));
            }
            validate_workspace_row(workspace)?;
            workspace_ids.insert(workspace.id.clone());
        }
        for session in sessions {
            if !workspace_ids.contains(&session.workspace_id) {
                return Err(workspace_error(
                    agent_workspace_codes::WORKSPACE_CONFLICT,
                    format!(
                        "replayed session workspace {} references a missing workspace",
                        session.id
                    ),
                ));
            }
            validate_session_row(session)?;
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "DELETE FROM session_worker_workspaces WHERE workspace_id IN
                (SELECT id FROM agent_workspaces WHERE project_id = ?1)",
            params![project_id],
        )?;
        tx.execute(
            "DELETE FROM agent_workspaces WHERE project_id = ?1",
            params![project_id],
        )?;
        for workspace in workspaces {
            insert_workspace_row_tx(&tx, workspace)?;
        }
        for session in sessions {
            insert_session_row_tx(&tx, session)?;
        }
        tx.commit()?;
        Ok(())
    }
}

use super::*;

impl Store {
    /// Record one internal Labour→Worker merge. The Git outcome is passed in by
    /// the engine; this method only persists bounded evidence and never treats
    /// a failed merge as a success.
    #[allow(clippy::too_many_arguments)]
    pub fn record_workspace_labour_merge(
        &self,
        workspace_id: &str,
        session_id: &str,
        agent_id: &str,
        task_id: Option<&str>,
        commit: &str,
        pre_merge: &str,
        post_merge: &str,
        merged: bool,
        paths: &[String],
        detail: &str,
    ) -> Result<AgentWorkspaceEvent> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let project_id = project_for_session_tx(&tx, session_id)?;
        let workspace = get_workspace_tx(&tx, workspace_id)?.ok_or_else(|| {
            workspace_error(
                agent_workspace_codes::WORKSPACE_NOT_FOUND,
                format!("workspace not found: {workspace_id}"),
            )
        })?;
        if workspace.project_id != project_id {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_PROJECT_MISMATCH,
                "workspace and session belong to different projects",
            ));
        }
        let binding = get_session_workspace_tx(&tx, session_id, &workspace.identity_id)?;
        let bounded_paths = paths
            .iter()
            .take(vp_protocol::types::AGENT_WORKSPACE_MAX_PATHS)
            .cloned()
            .collect::<Vec<_>>();
        let event = insert_workspace_event_tx(
            &tx,
            &project_id,
            vp_protocol::types::AGENT_WORKSPACE_LABOUR_MERGED_EVENT,
            &workspace,
            binding.as_ref(),
            Some((
                "merge",
                serde_json::json!({
                    "agent_id": agent_id,
                    "task_id": task_id,
                    "commit": commit,
                    "pre_merge": pre_merge,
                    "post_merge": post_merge,
                    "merged": merged,
                    "paths": bounded_paths,
                    "detail": detail.chars().take(2_000).collect::<String>(),
                }),
            )),
        )?;
        tx.commit()?;
        Ok(event)
    }

    /// Persist the accepted component-branch commit after an explicit land.
    /// Git has already succeeded; this transition is the durable evidence.
    pub fn record_workspace_landed(
        &self,
        workspace_id: &str,
        session_id: &str,
        previous_component_commit: &str,
        landed_commit: &str,
        commits: &[String],
        operator: Option<&str>,
    ) -> Result<AgentWorkspaceEvent> {
        let operator = validate_operator(operator)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let project_id = project_for_session_tx(&tx, session_id)?;
        let workspace = get_workspace_tx(&tx, workspace_id)?.ok_or_else(|| {
            workspace_error(
                agent_workspace_codes::WORKSPACE_NOT_FOUND,
                format!("workspace not found: {workspace_id}"),
            )
        })?;
        if workspace.project_id != project_id {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_PROJECT_MISMATCH,
                "workspace and session belong to different projects",
            ));
        }
        ensure_local_active(&workspace)?;
        let now_text = now_rfc3339();
        tx.execute(
            "UPDATE agent_workspaces
             SET base_commit = ?1, last_session_id = ?2, updated_at = ?3,
                 lease_token = NULL, lease_expires_at = NULL
             WHERE id = ?4",
            params![landed_commit, session_id, now_text, workspace_id],
        )?;
        tx.execute(
            "UPDATE session_worker_workspaces
             SET status = 'released', lease_token = NULL, lease_expires_at = NULL,
                 updated_at = ?1
             WHERE session_id = ?2 AND identity_id = ?3 AND status = 'active'",
            params![now_text, session_id, workspace.identity_id],
        )?;
        let workspace = get_workspace_tx(&tx, workspace_id)?
            .ok_or_else(|| anyhow!("workspace disappeared during land"))?;
        let binding = get_session_workspace_tx(&tx, session_id, &workspace.identity_id)?;
        let event = insert_workspace_event_tx(
            &tx,
            &project_id,
            vp_protocol::types::AGENT_WORKSPACE_LANDED_EVENT,
            &workspace,
            binding.as_ref(),
            Some((
                "land",
                serde_json::json!({
                    "previous_component_commit": previous_component_commit,
                    "landed_commit": landed_commit,
                    "commits": commits
                        .iter()
                        .take(vp_protocol::types::AGENT_WORKSPACE_MAX_PATHS)
                        .cloned()
                        .collect::<Vec<_>>(),
                    "operator": operator,
                }),
            )),
        )?;
        tx.commit()?;
        Ok(event)
    }

    /// Archive a workspace after release. A live lease or an active binding
    /// fails closed; evidence (branch, commits, paths, events) is preserved.
    pub fn archive_agent_workspace(
        &self,
        workspace_id: &str,
        retire: bool,
        operator: Option<&str>,
        reason: Option<&str>,
    ) -> Result<WorkspaceMutation> {
        let operator = validate_operator(operator)?;
        let reason = validate_reason(reason)?;
        let now = OffsetDateTime::now_utc();
        let now_text = format_time(now)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let workspace = get_workspace_tx(&tx, workspace_id)?.ok_or_else(|| {
            workspace_error(
                agent_workspace_codes::WORKSPACE_NOT_FOUND,
                format!("workspace not found: {workspace_id}"),
            )
        })?;
        ensure_local_active(&workspace)?;
        if let Some(owner) = live_lease_owner_tx(&tx, workspace_id, now)? {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_BUSY,
                format!(
                    "workspace {} still has a live lease{}; release it before archiving",
                    workspace_id,
                    owner
                        .as_deref()
                        .map(|session| format!(" (session {session})"))
                        .unwrap_or_default()
                ),
            ));
        }
        let active_binding: Option<String> = tx
            .query_row(
                "SELECT session_id FROM session_worker_workspaces
                 WHERE workspace_id = ?1 AND status = 'active'",
                params![workspace_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(session_id) = active_binding {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_BUSY,
                format!(
                    "workspace {} still has an active session binding ({session_id}); release it first",
                    workspace_id
                ),
            ));
        }
        let status = if retire { "retired" } else { "archived" };
        tx.execute(
            "UPDATE agent_workspaces
             SET status = ?1, lease_token = NULL, lease_expires_at = NULL, updated_at = ?2
             WHERE id = ?3",
            params![status, now_text, workspace_id],
        )?;
        tx.execute(
            "UPDATE session_worker_workspaces
             SET status = 'archived', lease_token = NULL, lease_expires_at = NULL, updated_at = ?1
             WHERE workspace_id = ?2 AND status = 'released'",
            params![now_text, workspace_id],
        )?;
        let workspace = get_workspace_tx(&tx, workspace_id)?
            .ok_or_else(|| anyhow!("workspace disappeared during archive"))?;
        let kind = if retire {
            vp_protocol::types::AGENT_WORKSPACE_RETIRED_EVENT
        } else {
            vp_protocol::types::AGENT_WORKSPACE_ARCHIVED_EVENT
        };
        let event = insert_workspace_event_tx(
            &tx,
            &workspace.project_id,
            kind,
            &workspace,
            None,
            Some((
                "operator",
                serde_json::json!({
                    "operator": operator,
                    "reason": reason,
                }),
            )),
        )?;
        tx.commit()?;
        Ok(WorkspaceMutation {
            workspace,
            session: None,
            changed: true,
            events: vec![event],
        })
    }
}

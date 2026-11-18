use super::*;

impl Store {
    // ---------------------------------------------------------------------
    // Workspace rows and leases
    // ---------------------------------------------------------------------

    pub fn get_agent_workspace(&self, workspace_id: &str) -> Result<Option<AgentWorkspace>> {
        let conn = self.lock()?;
        conn.query_row(
            &format!("{WORKSPACE_SELECT} WHERE id = ?1"),
            params![workspace_id],
            map_workspace,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn get_session_worker_workspace(
        &self,
        session_id: &str,
        identity_id: &str,
    ) -> Result<Option<SessionWorkerWorkspace>> {
        let conn = self.lock()?;
        conn.query_row(
            &format!("{SESSION_WORKSPACE_SELECT} WHERE session_id = ?1 AND identity_id = ?2"),
            params![session_id, identity_id],
            map_session_workspace,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn list_agent_workspaces(
        &self,
        project_id: Option<&str>,
        identity_id: Option<&str>,
        status: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<AgentWorkspace>> {
        if project_id.is_none() && identity_id.is_none() {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_INVALID,
                "list workspaces requires a project or identity filter",
            ));
        }
        if let Some(status) = status
            && !AGENT_WORKSPACE_STATUSES.contains(&status)
        {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_INVALID,
                format!("workspace status must be active, archived, or retired: {status}"),
            ));
        }
        let limit = limit
            .unwrap_or(AGENT_WORKSPACE_MAX_LIST)
            .clamp(1, AGENT_WORKSPACE_MAX_LIST) as i64;
        let conn = self.lock()?;
        let mut statement = conn.prepare(&format!(
            "{WORKSPACE_SELECT}
             WHERE (?1 IS NULL OR project_id = ?1)
               AND (?2 IS NULL OR identity_id = ?2)
               AND (?3 IS NULL OR status = ?3)
             ORDER BY updated_at DESC, id ASC
             LIMIT ?4"
        ))?;
        let rows = statement.query_map(
            params![project_id, identity_id, status, limit],
            map_workspace,
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn list_session_worker_workspaces(
        &self,
        session_id: &str,
    ) -> Result<Vec<SessionWorkerWorkspace>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(&format!(
            "{SESSION_WORKSPACE_SELECT} WHERE session_id = ?1 ORDER BY created_at ASC, id ASC"
        ))?;
        let rows = statement.query_map(params![session_id], map_session_workspace)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn list_session_workspaces_for_workspace(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<SessionWorkerWorkspace>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(&format!(
            "{SESSION_WORKSPACE_SELECT} WHERE workspace_id = ?1 ORDER BY created_at ASC, id ASC"
        ))?;
        let rows = statement.query_map(params![workspace_id], map_session_workspace)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Create or reuse one active workspace and acquire its lease.
    ///
    /// Idempotent for the same identity/component. A live lease held by another
    /// session, or a live component lease held by another workspace, is a typed
    /// `WORKSPACE_BUSY` conflict. An expired lease is recorded as
    /// `agent.workspace.lease_expired` before the new lease is acquired.
    pub fn prepare_agent_workspace(
        &self,
        request: PrepareWorkspaceRequest<'_>,
    ) -> Result<WorkspaceMutation> {
        let lease_secs = normalize_workspace_lease_secs(request.lease_secs)?;
        let operator = validate_operator(request.operator)?;
        let component = match request.component {
            Some(component) => normalize_workspace_component(component)?,
            None => {
                let conn = self.lock()?;
                conn.query_row(
                    "SELECT component FROM agent_identities WHERE id = ?1",
                    params![request.identity_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten()
                .unwrap_or_else(|| "worker".to_string())
            }
        };
        let component = normalize_workspace_component(&component)?;
        let expected_branch = component_branch_name(request.identity_id, &component);
        if request.branch != expected_branch {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_BRANCH_INVALID,
                "branch must be the deterministic component branch",
            ));
        }
        if !std::path::Path::new(request.worktree_path).is_absolute() {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_WORKTREE_INVALID,
                "workspace worktree path must be absolute",
            ));
        }
        if request.base_commit.trim().is_empty() {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_INVALID,
                "workspace base commit is required",
            ));
        }
        let now = OffsetDateTime::now_utc();
        let now_text = format_time(now)?;
        let expires = request
            .session_id
            .map(|_| lease_expiry(now, lease_secs))
            .transpose()?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let identity = identity_row_tx(&tx, request.identity_id)?;
        ensure_identity_workspacable(&identity, request.project_id)?;
        let policy = agent_identity_policy_tx(&tx, request.project_id)?;
        if !policy.enabled {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_POLICY_DISABLED,
                "project agent identities are disabled; enable the policy before creating workspaces",
            ));
        }
        if let Some(session_id) = request.session_id {
            if project_for_session_tx(&tx, session_id)? != request.project_id {
                return Err(workspace_error(
                    agent_workspace_codes::WORKSPACE_PROJECT_MISMATCH,
                    "session and workspace belong to different projects",
                ));
            }
            ensure_session_open_tx(&tx, session_id)?;
            ensure_active_assignment_tx(&tx, session_id, request.identity_id)?;
        }
        let existing = active_workspace_for_identity_tx(
            &tx,
            request.project_id,
            request.identity_id,
            &component,
        )?;
        let mut events =
            expire_workspaces_for_component_tx(&tx, request.project_id, &component, now)?;
        if let Some(holder) = component_lease_holder_tx(
            &tx,
            request.project_id,
            &component,
            existing
                .as_ref()
                .map(|workspace| workspace.id.as_str())
                .unwrap_or(""),
            now,
        )? {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_BUSY,
                format!(
                    "component {} is leased by workspace {}{}",
                    component,
                    holder.0,
                    holder
                        .1
                        .as_deref()
                        .map(|session| format!(" (session {session})"))
                        .unwrap_or_default()
                ),
            ));
        }
        let mut created = false;
        let workspace = match existing {
            Some(workspace) => {
                if workspace.origin != "local" {
                    return Err(workspace_error(
                        agent_workspace_codes::WORKSPACE_IMPORTED_READONLY,
                        "an imported workspace is read-only",
                    ));
                }
                if let Some(owner) = live_lease_owner_tx(&tx, &workspace.id, now)? {
                    match (owner.as_deref(), request.session_id) {
                        (Some(owner), Some(session)) if owner == session => {}
                        (Some(owner), _) => {
                            return Err(workspace_error(
                                agent_workspace_codes::WORKSPACE_BUSY,
                                format!(
                                    "workspace {} is leased by session {}",
                                    workspace.id, owner
                                ),
                            ));
                        }
                        (None, Some(_)) => {
                            return Err(workspace_error(
                                agent_workspace_codes::WORKSPACE_BUSY,
                                format!("workspace {} is leased by an operator", workspace.id),
                            ));
                        }
                        (None, None) => {}
                    }
                }
                tx.execute(
                    "UPDATE agent_workspaces
                     SET branch = ?1, base_commit = ?2, worktree_path = ?3, updated_at = ?4,
                         last_session_id = COALESCE(?5, last_session_id),
                         lease_token = ?6, lease_expires_at = ?7
                     WHERE id = ?8",
                    params![
                        request.branch,
                        request.base_commit,
                        request.worktree_path,
                        now_text,
                        request.session_id,
                        request.session_id.map(|_| new_lease_token()),
                        expires,
                        workspace.id
                    ],
                )?;
                get_workspace_tx(&tx, &workspace.id)?
                    .ok_or_else(|| anyhow!("workspace disappeared during prepare"))?
            }
            None => {
                let workspace = AgentWorkspace {
                    id: Uuid::now_v7().to_string(),
                    project_id: request.project_id.to_string(),
                    identity_id: request.identity_id.to_string(),
                    component: component.clone(),
                    branch: request.branch.to_string(),
                    base_commit: request.base_commit.to_string(),
                    worktree_path: request.worktree_path.to_string(),
                    status: "active".to_string(),
                    created_at: now_text.clone(),
                    updated_at: now_text.clone(),
                    last_session_id: request.session_id.map(str::to_string),
                    lease_active: request.session_id.is_some(),
                    lease_expires_at: expires.clone(),
                    origin: "local".to_string(),
                };
                validate_workspace_row(&workspace)?;
                tx.execute(
                    "INSERT INTO agent_workspaces
                     (id, project_id, identity_id, component, branch, base_commit, worktree_path,
                      status, created_at, updated_at, last_session_id, lease_token,
                      lease_expires_at, origin)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 'local')",
                    params![
                        workspace.id,
                        workspace.project_id,
                        workspace.identity_id,
                        workspace.component,
                        workspace.branch,
                        workspace.base_commit,
                        workspace.worktree_path,
                        workspace.status,
                        workspace.created_at,
                        workspace.updated_at,
                        workspace.last_session_id,
                        request.session_id.map(|_| new_lease_token()),
                        expires
                    ],
                )?;
                created = true;
                let stored = get_workspace_tx(&tx, &workspace.id)?
                    .ok_or_else(|| anyhow!("workspace disappeared after creation"))?;
                events.push(insert_workspace_event_tx(
                    &tx,
                    request.project_id,
                    vp_protocol::types::AGENT_WORKSPACE_CREATED_EVENT,
                    &stored,
                    None,
                    Some(("operator", serde_json::json!(operator))),
                )?);
                stored
            }
        };
        tx.commit()?;
        Ok(WorkspaceMutation {
            workspace,
            session: None,
            changed: created,
            events,
        })
    }
}

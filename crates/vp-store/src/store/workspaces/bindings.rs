use super::*;

impl Store {
    /// Attach a prepared workspace to a session. The active assignment is
    /// required; the binding is one row per `(session, identity)` and is
    /// idempotently reactivated on a later run.
    pub fn attach_session_workspace(
        &self,
        request: AttachWorkspaceRequest<'_>,
    ) -> Result<WorkspaceMutation> {
        let lease_secs = normalize_workspace_lease_secs(request.lease_secs)?;
        let operator = validate_operator(request.operator)?;
        let now = OffsetDateTime::now_utc();
        let now_text = format_time(now)?;
        let expires = lease_expiry(now, lease_secs)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_session_open_tx(&tx, request.session_id)?;
        let project_id = project_for_session_tx(&tx, request.session_id)?;
        let workspace = get_workspace_tx(&tx, request.workspace_id)?.ok_or_else(|| {
            workspace_error(
                agent_workspace_codes::WORKSPACE_NOT_FOUND,
                format!("workspace not found: {}", request.workspace_id),
            )
        })?;
        if workspace.project_id != project_id {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_PROJECT_MISMATCH,
                "workspace and session belong to different projects",
            ));
        }
        ensure_local_active(&workspace)?;
        ensure_active_assignment_tx(&tx, request.session_id, &workspace.identity_id)?;
        let identity = identity_row_tx(&tx, &workspace.identity_id)?;
        ensure_identity_workspacable(&identity, &project_id)?;
        let expected_branch =
            session_workspace_branch_name(request.session_id, &workspace.identity_id);
        if request.branch != expected_branch {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_BRANCH_INVALID,
                "branch must be the deterministic session workspace branch",
            ));
        }
        if !std::path::Path::new(request.worktree_path).is_absolute() {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_WORKTREE_INVALID,
                "session worktree path must be absolute",
            ));
        }
        let mut events =
            expire_workspaces_for_component_tx(&tx, &project_id, &workspace.component, now)?;
        if let Some(event) = expire_workspace_if_due_tx(&tx, &workspace.id, now)? {
            events.push(event);
        }
        if let Some(owner) = live_lease_owner_tx(&tx, &workspace.id, now)? {
            match owner.as_deref() {
                Some(owner) if owner == request.session_id => {}
                Some(owner) => {
                    return Err(workspace_error(
                        agent_workspace_codes::WORKSPACE_BUSY,
                        format!("workspace {} is leased by session {}", workspace.id, owner),
                    ));
                }
                None => {
                    return Err(workspace_error(
                        agent_workspace_codes::WORKSPACE_BUSY,
                        format!("workspace {} is leased by an operator", workspace.id),
                    ));
                }
            }
        }
        if let Some(holder) =
            component_lease_holder_tx(&tx, &project_id, &workspace.component, &workspace.id, now)?
        {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_BUSY,
                format!(
                    "component {} is leased by workspace {}{}",
                    workspace.component,
                    holder.0,
                    holder
                        .1
                        .as_deref()
                        .map(|session| format!(" (session {session})"))
                        .unwrap_or_default()
                ),
            ));
        }
        let existing = get_session_workspace_tx(&tx, request.session_id, &workspace.identity_id)?;
        if let Some(existing) = &existing
            && existing.workspace_id != workspace.id
            && existing.status == "active"
        {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_CONFLICT,
                format!(
                    "session already has an active workspace {} for this identity",
                    existing.workspace_id
                ),
            ));
        }
        // The binding and its workspace share one lease token. Release can
        // then be owner-fenced: a stale session cannot clear a workspace lease
        // that a different session has since acquired.
        let lease_token = new_lease_token();
        match existing {
            Some(existing) => {
                tx.execute(
                    "UPDATE session_worker_workspaces
                     SET workspace_id = ?1, worktree_path = ?2, branch = ?3, base_commit = ?4,
                         status = 'active', updated_at = ?5, lease_token = ?6, lease_expires_at = ?7
                     WHERE id = ?8",
                    params![
                        workspace.id,
                        request.worktree_path,
                        request.branch,
                        request.base_commit,
                        now_text,
                        lease_token.clone(),
                        expires,
                        existing.id
                    ],
                )?;
            }
            None => {
                let binding = SessionWorkerWorkspace {
                    id: Uuid::now_v7().to_string(),
                    workspace_id: workspace.id.clone(),
                    session_id: request.session_id.to_string(),
                    identity_id: workspace.identity_id.clone(),
                    worktree_path: request.worktree_path.to_string(),
                    branch: request.branch.to_string(),
                    base_commit: request.base_commit.to_string(),
                    status: "active".to_string(),
                    created_at: now_text.clone(),
                    updated_at: now_text.clone(),
                    lease_active: true,
                    lease_expires_at: Some(expires.clone()),
                };
                validate_session_row(&binding)?;
                tx.execute(
                    "INSERT INTO session_worker_workspaces
                     (id, workspace_id, session_id, identity_id, worktree_path, branch,
                      base_commit, status, created_at, updated_at, lease_token, lease_expires_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active', ?8, ?9, ?10, ?11)",
                    params![
                        binding.id,
                        binding.workspace_id,
                        binding.session_id,
                        binding.identity_id,
                        binding.worktree_path,
                        binding.branch,
                        binding.base_commit,
                        binding.created_at,
                        binding.updated_at,
                        lease_token.clone(),
                        expires
                    ],
                )?;
            }
        }
        tx.execute(
            "UPDATE agent_workspaces
             SET lease_token = ?1, lease_expires_at = ?2, last_session_id = ?3, updated_at = ?4
             WHERE id = ?5",
            params![
                lease_token,
                expires,
                request.session_id,
                now_text,
                workspace.id
            ],
        )?;
        let workspace = get_workspace_tx(&tx, &workspace.id)?
            .ok_or_else(|| anyhow!("workspace disappeared during attach"))?;
        let binding = get_session_workspace_tx(&tx, request.session_id, &workspace.identity_id)?
            .ok_or_else(|| anyhow!("session workspace disappeared during attach"))?;
        events.push(insert_workspace_event_tx(
            &tx,
            &project_id,
            vp_protocol::types::AGENT_WORKSPACE_SESSION_ATTACHED_EVENT,
            &workspace,
            Some(&binding),
            Some(("operator", serde_json::json!(operator))),
        )?);
        tx.commit()?;
        Ok(WorkspaceMutation {
            workspace,
            session: Some(binding),
            changed: true,
            events,
        })
    }

    /// Explicitly release one session binding and its workspace lease. This is
    /// idempotent and never archives or deletes evidence.
    pub fn release_session_worker_workspace(
        &self,
        session_id: &str,
        identity_id: Option<&str>,
        reason: &str,
        operator: Option<&str>,
    ) -> Result<Vec<AgentWorkspaceEvent>> {
        let operator = validate_operator(operator)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let project_id = project_for_session_tx(&tx, session_id)?;
        let bindings = {
            let mut statement = tx.prepare(&format!(
                "{SESSION_WORKSPACE_SELECT}
                 WHERE session_id = ?1 AND status = 'active'
                   AND (?2 IS NULL OR identity_id = ?2)"
            ))?;
            statement
                .query_map(params![session_id, identity_id], map_session_workspace)?
                .collect::<Result<Vec<_>, _>>()?
        };
        let now_text = now_rfc3339();
        let mut events = Vec::new();
        for binding in bindings {
            let binding_token: Option<String> = tx.query_row(
                "SELECT lease_token FROM session_worker_workspaces WHERE id = ?1",
                params![binding.id],
                |row| row.get(0),
            )?;
            tx.execute(
                "UPDATE session_worker_workspaces
                 SET status = 'released', lease_token = NULL, lease_expires_at = NULL,
                     updated_at = ?1
                 WHERE id = ?2 AND status = 'active'",
                params![now_text, binding.id],
            )?;
            // Owner-fenced: only clear the workspace lease when this binding's
            // token still owns it (or this session is the recorded owner).
            // A stale session ending after expiry/reclaim must not clear the
            // new session's live lease.
            tx.execute(
                "UPDATE agent_workspaces
                 SET lease_token = NULL, lease_expires_at = NULL, updated_at = ?1
                 WHERE id = ?2 AND project_id = ?3
                   AND (lease_token = ?4 OR last_session_id = ?5)",
                params![
                    now_text,
                    binding.workspace_id,
                    project_id,
                    binding_token,
                    session_id
                ],
            )?;
            let released = get_session_workspace_tx(&tx, session_id, &binding.identity_id)?
                .ok_or_else(|| anyhow!("session workspace disappeared during release"))?;
            let workspace = get_workspace_tx(&tx, &binding.workspace_id)?.ok_or_else(|| {
                anyhow!(
                    "workspace disappeared during release: {}",
                    binding.workspace_id
                )
            })?;
            events.push(insert_workspace_event_tx(
                &tx,
                &project_id,
                vp_protocol::types::AGENT_WORKSPACE_RELEASED_EVENT,
                &workspace,
                Some(&released),
                Some((
                    "operator",
                    serde_json::json!({
                        "operator": operator,
                        "reason": reason,
                    }),
                )),
            )?);
        }
        tx.commit()?;
        Ok(events)
    }

    /// Heartbeat one session's active Worker workspace binding(s).
    ///
    /// A plan task can outlive the short lease that was taken when the run
    /// attached. The run loop calls this at turn/tool boundaries. Renewal
    /// rotates the binding token and the workspace token together, never
    /// revives an already-expired lease, and never renews a workspace whose
    /// token now belongs to a different session.
    pub fn renew_session_worker_workspaces(
        &self,
        session_id: &str,
        identity_id: Option<&str>,
        lease_secs: Option<u64>,
        operator: Option<&str>,
    ) -> Result<Vec<WorkspaceMutation>> {
        let lease_secs = normalize_workspace_lease_secs(lease_secs)?;
        let operator = validate_operator(operator)?;
        let now = OffsetDateTime::now_utc();
        let now_text = format_time(now)?;
        let expires = lease_expiry(now, lease_secs)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_session_open_tx(&tx, session_id)?;
        let project_id = project_for_session_tx(&tx, session_id)?;
        let bindings = {
            let mut statement = tx.prepare(&format!(
                "{SESSION_WORKSPACE_SELECT}
                 WHERE session_id = ?1 AND status = 'active'
                   AND (?2 IS NULL OR identity_id = ?2)"
            ))?;
            statement
                .query_map(params![session_id, identity_id], map_session_workspace)?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut mutations = Vec::with_capacity(bindings.len());
        for binding in bindings {
            let (binding_token, binding_expiry): (Option<String>, Option<String>) = tx.query_row(
                "SELECT lease_token, lease_expires_at
                     FROM session_worker_workspaces WHERE id = ?1",
                params![binding.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let live = binding_expiry
                .as_deref()
                .and_then(parse_time)
                .is_some_and(|expiry| expiry > now);
            if !live {
                return Err(workspace_error(
                    agent_workspace_codes::WORKSPACE_LEASE_EXPIRED,
                    "the Worker workspace lease expired before it could be renewed; re-attach the workspace",
                ));
            }
            let workspace = get_workspace_tx(&tx, &binding.workspace_id)?.ok_or_else(|| {
                workspace_error(
                    agent_workspace_codes::WORKSPACE_NOT_FOUND,
                    format!("workspace not found: {}", binding.workspace_id),
                )
            })?;
            ensure_local_active(&workspace)?;
            let workspace_token: Option<String> = tx.query_row(
                "SELECT lease_token FROM agent_workspaces WHERE id = ?1",
                params![workspace.id],
                |row| row.get(0),
            )?;
            if workspace_token != binding_token {
                return Err(workspace_error(
                    agent_workspace_codes::WORKSPACE_CONFLICT,
                    "workspace lease is owned by another session or operator; re-attach before renewing",
                ));
            }
            let token = new_lease_token();
            tx.execute(
                "UPDATE session_worker_workspaces
                 SET lease_token = ?1, lease_expires_at = ?2, updated_at = ?3
                 WHERE id = ?4 AND status = 'active'",
                params![token, expires, now_text, binding.id],
            )?;
            tx.execute(
                "UPDATE agent_workspaces
                 SET lease_token = ?1, lease_expires_at = ?2, updated_at = ?3
                 WHERE id = ?4 AND status = 'active'",
                params![token, expires, now_text, workspace.id],
            )?;
            let workspace = get_workspace_tx(&tx, &workspace.id)?
                .ok_or_else(|| anyhow!("workspace disappeared during lease renewal"))?;
            let binding = get_session_workspace_tx(&tx, session_id, &workspace.identity_id)?
                .ok_or_else(|| anyhow!("session workspace disappeared during lease renewal"))?;
            let event = insert_workspace_event_tx(
                &tx,
                &project_id,
                vp_protocol::types::AGENT_WORKSPACE_LEASE_RENEWED_EVENT,
                &workspace,
                Some(&binding),
                Some((
                    "operator",
                    serde_json::json!({
                        "operator": operator,
                        "reason": "run_heartbeat",
                    }),
                )),
            )?;
            mutations.push(WorkspaceMutation {
                workspace,
                session: Some(binding),
                changed: true,
                events: vec![event],
            });
        }
        tx.commit()?;
        Ok(mutations)
    }
}

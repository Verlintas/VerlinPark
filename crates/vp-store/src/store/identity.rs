//! Project-scoped agent identities and identity events.

use super::*;

impl Store {
    pub fn get_agent_identity_policy(&self, project_id: &str) -> Result<AgentIdentityPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT policy FROM project_agent_identity_policies WHERE project_id = ?1",
                params![project_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        match stored {
            Some(raw) => {
                let policy: AgentIdentityPolicy =
                    serde_json::from_str(&raw).context("parsing stored agent identity policy")?;
                policy.normalized().map_err(|error| {
                    AgentIdentityError::new(agent_identity_codes::IDENTITY_POLICY_INVALID, error)
                        .into()
                })
            }
            None => Ok(AgentIdentityPolicy::default()),
        }
    }

    pub fn set_agent_identity_policy(
        &self,
        project_id: &str,
        policy: &AgentIdentityPolicy,
    ) -> Result<()> {
        let policy = policy.normalized().map_err(|error| {
            AgentIdentityError::new(agent_identity_codes::IDENTITY_POLICY_INVALID, error)
        })?;
        let encoded = serde_json::to_string(&policy)?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_agent_identity_policies (project_id, policy, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(project_id) DO UPDATE SET
                policy = excluded.policy,
                updated_at = excluded.updated_at",
            params![project_id, encoded, now_rfc3339()],
        )?;
        Ok(())
    }

    /// Create one explicit project identity and its `created` event in the same
    /// transaction. This never creates a session, run, inbox, workspace, or
    /// memory row.
    pub fn create_agent_identity(
        &self,
        params: &CreateAgentIdentityParams,
    ) -> Result<(AgentIdentity, AgentIdentityEvent)> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let identity = create_agent_identity_tx(&tx, params)?;
        let event = insert_agent_identity_event_tx(
            &tx,
            &identity,
            vp_protocol::types::AGENT_IDENTITY_CREATED_EVENT,
            serde_json::json!({
                "identity": identity,
                "changed_fields": [],
                "previous_generation": 0,
                "generation": identity.generation,
            }),
        )?;
        tx.commit()?;
        Ok((identity, event))
    }

    pub fn get_agent_identity(&self, identity_id: &str) -> Result<Option<AgentIdentity>> {
        let conn = self.lock()?;
        conn.query_row(
            &format!("SELECT {AGENT_IDENTITY_COLUMNS} FROM agent_identities WHERE id = ?1"),
            params![identity_id],
            map_agent_identity,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn list_agent_identities(
        &self,
        project_id: &str,
        role: Option<&str>,
        status: Option<&str>,
    ) -> Result<Vec<AgentIdentity>> {
        if let Some(role) = role
            && agent_identity_role(role).is_none()
        {
            return Err(AgentIdentityError::new(
                agent_identity_codes::IDENTITY_ROLE_INVALID,
                format!("role must be one of leader, manager, worker: {role}"),
            )
            .into());
        }
        if let Some(status) = status
            && !AGENT_IDENTITY_STATUSES.contains(&status)
        {
            return Err(AgentIdentityError::new(
                agent_identity_codes::IDENTITY_INVALID,
                format!("status must be one of active, idle, paused, retired: {status}"),
            )
            .into());
        }
        let conn = self.lock()?;
        let mut statement = conn.prepare(&format!(
            "SELECT {AGENT_IDENTITY_COLUMNS}
             FROM agent_identities
             WHERE project_id = ?1
               AND (?2 IS NULL OR role = ?2)
               AND (?3 IS NULL OR status = ?3)
             ORDER BY created_at ASC, id ASC"
        ))?;
        let rows = statement.query_map(params![project_id, role, status], map_agent_identity)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Apply the S1 update surface. `None` means no field changed and no event
    /// was appended. A role/department/component/title/profile/model change
    /// increments `generation` exactly once; retired rows are terminal except
    /// for successor bookkeeping.
    pub fn update_agent_identity(
        &self,
        params: &UpdateAgentIdentityParams,
    ) -> Result<Option<(AgentIdentity, AgentIdentityEvent)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let existing = get_agent_identity_tx(&tx, &params.identity_id)?;
        ensure_identity_mutable(&existing)?;
        let mut changed_fields = Vec::new();
        let mut next = existing.clone();

        if existing.status == "retired" {
            if params.role.is_some()
                || params.department.is_some()
                || params.component.is_some()
                || params.title.is_some()
                || params.profile.is_some()
                || params.model.is_some()
                || params.parent_identity_id.is_some()
                || params.policy.is_some()
            {
                return Err(AgentIdentityError::new(
                    agent_identity_codes::IDENTITY_RETIRED,
                    "a retired identity cannot change role, profile, model, hierarchy, or policy",
                )
                .into());
            }
            if let Some(successor) = &params.successor_id {
                let successor = normalize_identity_text(
                    successor.as_deref(),
                    "successor_id",
                    AGENT_IDENTITY_MAX_ID_CHARS,
                )?;
                if successor.as_deref() != existing.successor_id.as_deref() {
                    if let Some(successor_id) = &successor {
                        validate_identity_successor_tx(
                            &tx,
                            &existing.project_id,
                            &existing.id,
                            successor_id,
                        )?;
                    }
                    next.successor_id = successor;
                    changed_fields.push("successor_id");
                }
            }
            if changed_fields.is_empty() {
                return Ok(None);
            }
            next.updated_at = now_rfc3339();
            update_agent_identity_row_tx(&tx, &next)?;
            let event = insert_agent_identity_event_tx(
                &tx,
                &next,
                vp_protocol::types::AGENT_IDENTITY_UPDATED_EVENT,
                serde_json::json!({
                    "identity": next,
                    "changed_fields": changed_fields,
                    "previous_generation": existing.generation,
                    "generation": next.generation,
                }),
            )?;
            tx.commit()?;
            return Ok(Some((next, event)));
        }

        let next_role = match &params.role {
            Some(role) => agent_identity_role(role.trim())
                .ok_or_else(|| {
                    AgentIdentityError::new(
                        agent_identity_codes::IDENTITY_ROLE_INVALID,
                        format!("role must be one of leader, manager, worker: {role}"),
                    )
                })?
                .to_string(),
            None => existing.role.clone(),
        };
        if next_role != existing.role {
            ensure_identity_role_capacity_tx(
                &tx,
                &existing.project_id,
                &next_role,
                Some(&existing.id),
            )?;
            next.role = next_role.clone();
            changed_fields.push("role");
        }

        apply_identity_field(
            &mut next.department,
            params.department.as_ref(),
            "department",
            AGENT_IDENTITY_MAX_DEPARTMENT_CHARS,
            &mut changed_fields,
        )?;
        apply_identity_field(
            &mut next.component,
            params.component.as_ref(),
            "component",
            AGENT_IDENTITY_MAX_COMPONENT_CHARS,
            &mut changed_fields,
        )?;
        apply_identity_field(
            &mut next.title,
            params.title.as_ref(),
            "title",
            AGENT_IDENTITY_MAX_TITLE_CHARS,
            &mut changed_fields,
        )?;
        apply_identity_field(
            &mut next.profile,
            params.profile.as_ref(),
            "profile",
            AGENT_IDENTITY_MAX_DEPARTMENT_CHARS,
            &mut changed_fields,
        )?;
        apply_identity_field(
            &mut next.model,
            params.model.as_ref(),
            "model",
            AGENT_IDENTITY_MAX_MODEL_CHARS,
            &mut changed_fields,
        )?;
        if let Some(profile) = &next.profile
            && !AGENT_IDENTITY_PROFILE_VALUES.contains(&profile.as_str())
        {
            return Err(AgentIdentityError::new(
                agent_identity_codes::IDENTITY_INVALID,
                format!(
                    "profile must be one of coding, research, verification, generic: {profile}"
                ),
            )
            .into());
        }

        if let Some(parent) = &params.parent_identity_id {
            let parent = normalize_identity_text(
                parent.as_deref(),
                "parent_identity_id",
                AGENT_IDENTITY_MAX_ID_CHARS,
            )?;
            if parent.as_deref() != existing.parent_identity_id.as_deref() {
                next.parent_identity_id = parent;
                changed_fields.push("parent_identity_id");
            }
        }
        if let Some(successor) = &params.successor_id {
            let successor = normalize_identity_text(
                successor.as_deref(),
                "successor_id",
                AGENT_IDENTITY_MAX_ID_CHARS,
            )?;
            if successor.as_deref() != existing.successor_id.as_deref() {
                next.successor_id = successor;
                changed_fields.push("successor_id");
            }
        }
        if let Some(policy) = &params.policy {
            let policy = normalize_identity_policy(policy)?;
            if policy != existing.policy {
                next.policy = policy;
                changed_fields.push("policy");
            }
        }

        // Revalidate the resulting hierarchy, not only the fields that were
        // supplied. A role change can invalidate an unchanged parent.
        validate_identity_parent_tx(
            &tx,
            &existing.project_id,
            &next.role,
            next.parent_identity_id.as_deref(),
        )?;
        if let Some(successor) = &next.successor_id {
            validate_identity_successor_tx(&tx, &existing.project_id, &existing.id, successor)?;
        }

        if changed_fields.is_empty() {
            return Ok(None);
        }
        let generation_fields = [
            "role",
            "department",
            "component",
            "title",
            "profile",
            "model",
        ];
        let generation_changed = changed_fields
            .iter()
            .any(|field| generation_fields.contains(field));
        if generation_changed {
            next.generation = existing.generation.saturating_add(1);
        }
        next.updated_at = now_rfc3339();
        update_agent_identity_row_tx(&tx, &next)?;
        let event = insert_agent_identity_event_tx(
            &tx,
            &next,
            vp_protocol::types::AGENT_IDENTITY_UPDATED_EVENT,
            serde_json::json!({
                "identity": next,
                "changed_fields": changed_fields,
                "previous_generation": existing.generation,
                "generation": next.generation,
            }),
        )?;
        tx.commit()?;
        Ok(Some((next, event)))
    }

    pub fn pause_agent_identity(
        &self,
        identity_id: &str,
    ) -> Result<Option<(AgentIdentity, AgentIdentityEvent)>> {
        self.transition_agent_identity(
            identity_id,
            "active",
            "paused",
            vp_protocol::types::AGENT_IDENTITY_PAUSED_EVENT,
        )
    }

    pub fn resume_agent_identity(
        &self,
        identity_id: &str,
    ) -> Result<Option<(AgentIdentity, AgentIdentityEvent)>> {
        self.transition_agent_identity(
            identity_id,
            "paused",
            "active",
            vp_protocol::types::AGENT_IDENTITY_RESUMED_EVENT,
        )
    }

    /// Pause/resume share exactly one legal source status. A no-op transition
    /// returns `None` and never appends a duplicate lifecycle event.
    pub(super) fn transition_agent_identity(
        &self,
        identity_id: &str,
        from: &str,
        to: &str,
        event_kind: &str,
    ) -> Result<Option<(AgentIdentity, AgentIdentityEvent)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let existing = get_agent_identity_tx(&tx, identity_id)?;
        ensure_identity_mutable(&existing)?;
        if existing.status == "retired" {
            return Err(AgentIdentityError::new(
                agent_identity_codes::IDENTITY_RETIRED,
                format!("identity {identity_id} is retired and terminal"),
            )
            .into());
        }
        if existing.status == to {
            return Ok(None);
        }
        // Pause is a cooperative status change: a live assignment remains
        // until the run ends, but prompt selection already requires an active
        // identity. Retirement is the terminal operation that must refuse
        // live work unless S6 explicitly releases it.
        if existing.status != from {
            return Err(AgentIdentityError::new(
                agent_identity_codes::IDENTITY_CONFLICT,
                format!(
                    "identity {identity_id} cannot move from {} to {to}",
                    existing.status
                ),
            )
            .into());
        }
        let mut next = existing.clone();
        next.status = to.to_string();
        next.updated_at = now_rfc3339();
        update_agent_identity_row_tx(&tx, &next)?;
        let event = insert_agent_identity_event_tx(
            &tx,
            &next,
            event_kind,
            serde_json::json!({
                "identity": next,
                "changed_fields": ["status"],
                "previous_generation": existing.generation,
                "generation": next.generation,
            }),
        )?;
        tx.commit()?;
        Ok(Some((next, event)))
    }

    /// Retirement is terminal. Retired rows retain every event and may only
    /// receive a successor reference; they are never deleted.
    pub fn retire_agent_identity(
        &self,
        params: &RetireAgentIdentityParams,
    ) -> Result<Option<(AgentIdentity, AgentIdentityEvent)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let existing = get_agent_identity_tx(&tx, &params.identity_id)?;
        ensure_identity_mutable(&existing)?;
        let successor = normalize_identity_text(
            params.successor_id.as_deref(),
            "successor_id",
            AGENT_IDENTITY_MAX_ID_CHARS,
        )?;
        if existing.status == "retired" {
            if successor.is_some() && successor != existing.successor_id {
                return Err(AgentIdentityError::new(
                    agent_identity_codes::IDENTITY_CONFLICT,
                    format!(
                        "retired identity {} already has a different successor",
                        existing.id
                    ),
                )
                .into());
            }
            return Ok(None);
        }
        if identity_has_live_work_tx(&tx, &existing.id)? {
            return Err(AgentIdentityError::new(
                agent_identity_codes::IDENTITY_BUSY,
                format!(
                    "identity {} still owns an active assignment or workspace; use the S6 retirement path to release live work explicitly",
                    existing.id
                ),
            )
            .into());
        }
        if let Some(successor_id) = &successor {
            validate_identity_successor_tx(&tx, &existing.project_id, &existing.id, successor_id)?;
        }
        let mut next = existing.clone();
        next.status = "retired".to_string();
        next.updated_at = now_rfc3339();
        next.retired_at = Some(next.updated_at.clone());
        if successor.is_some() {
            next.successor_id = successor;
        }
        let mut changed_fields = vec!["status"];
        if next.successor_id != existing.successor_id {
            changed_fields.push("successor_id");
        }
        update_agent_identity_row_tx(&tx, &next)?;
        let event = insert_agent_identity_event_tx(
            &tx,
            &next,
            vp_protocol::types::AGENT_IDENTITY_RETIRED_EVENT,
            serde_json::json!({
                "identity": next,
                "changed_fields": changed_fields,
                "previous_generation": existing.generation,
                "generation": next.generation,
            }),
        )?;
        tx.commit()?;
        Ok(Some((next, event)))
    }

    pub fn list_agent_identity_events_for_project(
        &self,
        project_id: &str,
        limit: Option<u32>,
    ) -> Result<Vec<AgentIdentityEvent>> {
        let conn = self.lock()?;
        let limit = limit.unwrap_or(100_000).clamp(1, 100_000) as i64;
        let mut statement = conn.prepare(
            "SELECT project_id, identity_id, seq, kind, payload, created_at
             FROM agent_identity_events
             WHERE project_id = ?1
             ORDER BY created_at ASC, identity_id ASC, seq ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![project_id, limit], map_agent_identity_event)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn list_agent_identity_events(
        &self,
        identity_id: &str,
        limit: Option<u32>,
    ) -> Result<Vec<AgentIdentityEvent>> {
        let conn = self.lock()?;
        let limit = limit.unwrap_or(10_000).clamp(1, 100_000) as i64;
        let mut statement = conn.prepare(
            "SELECT project_id, identity_id, seq, kind, payload, created_at
             FROM agent_identity_events
             WHERE identity_id = ?1
             ORDER BY seq ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![identity_id, limit], map_agent_identity_event)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Rebuild the identity projection from event-derived rows. This appends no
    /// events and is only called by explicit replay repair or archive rebuild.
    pub fn replace_agent_identity_projection(
        &self,
        project_id: &str,
        identities: &[AgentIdentity],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        for identity in identities {
            if identity.project_id != project_id {
                return Err(AgentIdentityError::new(
                    agent_identity_codes::IDENTITY_PROJECT_MISMATCH,
                    format!(
                        "identity {} belongs to project {}, not {project_id}",
                        identity.id, identity.project_id
                    ),
                )
                .into());
            }
        }
        // Repair is deliberately non-destructive for live work. The previous
        // implementation deleted every projection row and relied on
        // INSERT OR REPLACE, whose delete half cascaded through S2
        // assignments, the S2 inbox, S4 workspaces/bindings, and S5 identity
        // memory. A generation mismatch could therefore erase live
        // coordination state and still report a consistent projection.
        //
        // Now expected rows are upserted, only rows absent from the event
        // stream are deleted, and any touched identity that still owns an
        // active assignment or workspace fails the whole transaction closed.
        let existing = {
            let mut statement = tx.prepare(&format!(
                "SELECT {AGENT_IDENTITY_COLUMNS} FROM agent_identities
                 WHERE project_id = ?1 ORDER BY id ASC"
            ))?;
            statement
                .query_map(params![project_id], map_agent_identity)?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut expected = std::collections::BTreeMap::new();
        for identity in identities {
            if expected.insert(identity.id.as_str(), identity).is_some() {
                return Err(AgentIdentityError::new(
                    agent_identity_codes::IDENTITY_CONFLICT,
                    format!("replayed identity {} appears more than once", identity.id),
                )
                .into());
            }
        }
        let actual = existing
            .iter()
            .map(|identity| (identity.id.as_str(), identity))
            .collect::<std::collections::BTreeMap<_, _>>();
        for identity in identities {
            let changed = actual
                .get(identity.id.as_str())
                .is_some_and(|current| *current != identity);
            if !changed && actual.contains_key(identity.id.as_str()) {
                continue;
            }
            if identity_has_live_work_tx(&tx, &identity.id)? {
                return Err(AgentIdentityError::new(
                    agent_identity_codes::IDENTITY_REPAIR_LIVE_WORK,
                    format!(
                        "identity {} still owns an active assignment or workspace; end that work before repairing its projection",
                        identity.id
                    ),
                )
                .into());
            }
            insert_agent_identity_row_tx(&tx, identity)?;
        }
        for identity in &existing {
            if expected.contains_key(identity.id.as_str()) {
                continue;
            }
            // Imported rows are archive evidence. Local events cannot prove
            // they are wrong, so repair never deletes them.
            if identity.origin == "imported" {
                continue;
            }
            if identity_has_live_work_tx(&tx, &identity.id)? {
                return Err(AgentIdentityError::new(
                    agent_identity_codes::IDENTITY_REPAIR_LIVE_WORK,
                    format!(
                        "identity {} is absent from the event stream but still owns active work; refusing to delete it",
                        identity.id
                    ),
                )
                .into());
            }
            tx.execute(
                "DELETE FROM agent_identities WHERE id = ?1 AND project_id = ?2",
                params![identity.id, project_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn insert_agent_identity_snapshot(&self, identity: &AgentIdentity) -> Result<()> {
        let conn = self.lock()?;
        insert_agent_identity_row_conn(&conn, identity)
    }

    pub fn insert_agent_identity_event_snapshot(&self, event: &AgentIdentityEvent) -> Result<()> {
        let conn = self.lock()?;
        insert_agent_identity_event_snapshot_conn(&conn, event)
    }
}

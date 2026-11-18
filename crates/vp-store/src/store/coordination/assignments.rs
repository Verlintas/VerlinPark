use super::*;

impl Store {
    // ---------------------------------------------------------------------
    // Assignments and identity leases
    // ---------------------------------------------------------------------

    pub fn get_session_agent_assignment(
        &self,
        session_id: &str,
        identity_id: &str,
    ) -> Result<Option<AgentAssignment>> {
        let conn = self.lock()?;
        get_assignment_conn(&conn, session_id, identity_id)
    }

    /// Bounded assignment listing. At least one selector is required so this
    /// cannot become an unbounded project-wide scan.
    pub fn list_session_agent_assignments(
        &self,
        session_id: Option<&str>,
        identity_id: Option<&str>,
        project_id: Option<&str>,
        status: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<AgentAssignment>> {
        if session_id.is_none() && identity_id.is_none() && project_id.is_none() {
            return Err(coordination_error(
                agent_coordination_codes::ASSIGNMENT_INVALID,
                "list assignments requires a session, identity, or project filter",
            ));
        }
        if let Some(status) = status
            && !AGENT_ASSIGNMENT_STATUSES.contains(&status)
        {
            return Err(coordination_error(
                agent_coordination_codes::ASSIGNMENT_INVALID,
                format!("assignment status must be active, released, or queued: {status}"),
            ));
        }
        let limit = limit
            .unwrap_or(AGENT_ASSIGNMENT_MAX_LIST)
            .clamp(1, AGENT_ASSIGNMENT_MAX_LIST) as i64;
        let conn = self.lock()?;
        let mut statement = conn.prepare(&format!(
            "{ASSIGNMENT_SELECT}
             WHERE (?1 IS NULL OR a.session_id = ?1)
               AND (?2 IS NULL OR a.identity_id = ?2)
               AND (?3 IS NULL OR s.project_id = ?3)
               AND (?4 IS NULL OR a.status = ?4)
             ORDER BY a.assigned_at DESC, a.session_id ASC, a.identity_id ASC
             LIMIT ?5"
        ))?;
        let rows = statement.query_map(
            params![session_id, identity_id, project_id, status, limit],
            map_assignment,
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Assign a project identity to a session.
    ///
    /// Idempotent when the same session already holds a live assignment for the
    /// identity. A different session with a live lease is a typed conflict. An
    /// expired lease is released and recorded as `agent.assignment.expired`
    /// before the new assignment is created.
    pub fn create_session_agent_assignment(
        &self,
        session_id: &str,
        identity_id: &str,
        requested_role: Option<&str>,
        lease_secs: Option<u64>,
        operator: Option<&str>,
    ) -> Result<AssignmentMutation> {
        let lease_secs = normalize_assignment_lease_secs(lease_secs)?;
        let operator = validate_operator_label(operator)?;
        let now = OffsetDateTime::now_utc();
        let now_text = format_time(now)?;
        let expires = lease_expiry(now, lease_secs)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let project_id = project_for_session_tx(&tx, session_id)?;
        ensure_session_open_tx(&tx, session_id)?;
        let identity = identity_row_tx(&tx, identity_id)?;
        if identity.project_id != project_id {
            return Err(coordination_error(
                agent_coordination_codes::ASSIGNMENT_PROJECT_MISMATCH,
                "identity and session belong to different projects",
            ));
        }
        let role = ensure_identity_assignable(&identity)?;
        if let Some(requested) = requested_role
            && requested != role
        {
            return Err(coordination_error(
                agent_coordination_codes::ASSIGNMENT_ROLE_MISMATCH,
                format!("requested role {requested} does not match identity role {role}"),
            ));
        }
        let policy = super::super::agent_identity_policy_tx(&tx, &project_id)?;
        if !policy.enabled {
            return Err(coordination_error(
                agent_coordination_codes::ASSIGNMENT_POLICY_DISABLED,
                "project agent identities are disabled; enable the project policy before assigning",
            ));
        }

        let mut events = Vec::new();
        if let Some(event) = expire_identity_if_due_tx(&tx, identity_id, now)? {
            events.push(event);
        }
        let other_session: Option<String> = tx
            .query_row(
                "SELECT session_id FROM session_agent_assignments
                 WHERE identity_id = ?1 AND status = 'active'",
                params![identity_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(other_session) = other_session {
            if other_session == session_id {
                // A repeated assign is not a new assignment, but it must keep
                // the lease alive. Returning the old row unchanged let an
                // operator "renew" a run while the 600s lease still expired
                // underneath it.
                tx.execute(
                    "UPDATE session_agent_assignments
                     SET lease_token = ?1, lease_expires_at = ?2, heartbeat_at = ?3
                     WHERE session_id = ?4 AND identity_id = ?5 AND status = 'active'",
                    params![
                        new_lease_token(),
                        expires,
                        now_text,
                        session_id,
                        identity_id
                    ],
                )?;
                let renewed = get_assignment_tx(&tx, session_id, identity_id)?
                    .ok_or_else(|| anyhow!("assignment disappeared during idempotent assign"))?;
                let event = insert_assignment_event_tx(
                    &tx,
                    &project_id,
                    vp_protocol::types::AGENT_ASSIGNMENT_RENEWED_EVENT,
                    &renewed,
                    Some((
                        "operator",
                        serde_json::json!({
                            "operator": operator,
                            "reason": "idempotent_assign",
                        }),
                    )),
                )?;
                events.push(event);
                tx.commit()?;
                return Ok(AssignmentMutation {
                    assignment: renewed,
                    // No new assignment row was created; only the live lease
                    // was extended. Callers still receive the renewed event.
                    changed: false,
                    events,
                });
            }
            return Err(coordination_error(
                agent_coordination_codes::ASSIGNMENT_CONFLICT,
                format!(
                    "identity {identity_id} already has an active assignment in session {other_session}"
                ),
            ));
        }

        tx.execute(
            "INSERT INTO session_agent_assignments
             (session_id, identity_id, role, status, lease_token, lease_expires_at,
              assigned_at, released_at, heartbeat_at)
             VALUES (?1, ?2, ?3, 'active', ?4, ?5, ?6, NULL, ?6)
             ON CONFLICT(session_id, identity_id) DO UPDATE SET
                role = excluded.role,
                status = 'active',
                lease_token = excluded.lease_token,
                lease_expires_at = excluded.lease_expires_at,
                assigned_at = excluded.assigned_at,
                released_at = NULL,
                heartbeat_at = excluded.heartbeat_at",
            params![
                session_id,
                identity_id,
                role,
                new_lease_token(),
                expires,
                now_text
            ],
        )?;
        let assignment = get_assignment_tx(&tx, session_id, identity_id)?
            .ok_or_else(|| anyhow!("assignment disappeared after creation"))?;
        let event = insert_assignment_event_tx(
            &tx,
            &project_id,
            vp_protocol::types::AGENT_ASSIGNMENT_CREATED_EVENT,
            &assignment,
            Some(("operator", serde_json::json!(operator))),
        )?;
        events.push(event);
        tx.commit()?;
        Ok(AssignmentMutation {
            assignment,
            changed: true,
            events,
        })
    }

    /// Heartbeat/renew the live lease. Renewal cannot revive an expired lease;
    /// it records the expiry and returns `ASSIGNMENT_LEASE_EXPIRED`.
    pub fn renew_session_agent_assignment(
        &self,
        session_id: &str,
        identity_id: &str,
        lease_secs: Option<u64>,
        operator: Option<&str>,
    ) -> Result<AssignmentMutation> {
        let lease_secs = normalize_assignment_lease_secs(lease_secs)?;
        let operator = validate_operator_label(operator)?;
        let now = OffsetDateTime::now_utc();
        let now_text = format_time(now)?;
        let expires = lease_expiry(now, lease_secs)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let project_id = project_for_session_tx(&tx, session_id)?;
        ensure_session_open_tx(&tx, session_id)?;
        let identity = identity_row_tx(&tx, identity_id)?;
        if identity.project_id != project_id {
            return Err(coordination_error(
                agent_coordination_codes::ASSIGNMENT_PROJECT_MISMATCH,
                "identity and session belong to different projects",
            ));
        }
        if let Some(event) = expire_identity_if_due_tx(&tx, identity_id, now)? {
            tx.commit()?;
            let _ = event;
            return Err(coordination_error(
                agent_coordination_codes::ASSIGNMENT_LEASE_EXPIRED,
                "the identity lease already expired; assign the identity again",
            ));
        }
        let existing = get_assignment_tx(&tx, session_id, identity_id)?.ok_or_else(|| {
            coordination_error(
                agent_coordination_codes::ASSIGNMENT_NOT_FOUND,
                "session does not hold an assignment for this identity",
            )
        })?;
        if existing.status != "active" {
            return Err(coordination_error(
                agent_coordination_codes::ASSIGNMENT_LEASE_EXPIRED,
                format!(
                    "the assignment is {} and cannot be renewed",
                    existing.status
                ),
            ));
        }
        if !existing.lease_active {
            return Err(coordination_error(
                agent_coordination_codes::ASSIGNMENT_LEASE_EXPIRED,
                "the identity lease is not live",
            ));
        }
        tx.execute(
            "UPDATE session_agent_assignments
             SET lease_token = ?1, lease_expires_at = ?2, heartbeat_at = ?3
             WHERE session_id = ?4 AND identity_id = ?5 AND status = 'active'",
            params![
                new_lease_token(),
                expires,
                now_text,
                session_id,
                identity_id
            ],
        )?;
        let assignment = get_assignment_tx(&tx, session_id, identity_id)?
            .ok_or_else(|| anyhow!("assignment disappeared during renewal"))?;
        let event = insert_assignment_event_tx(
            &tx,
            &project_id,
            vp_protocol::types::AGENT_ASSIGNMENT_RENEWED_EVENT,
            &assignment,
            Some(("operator", serde_json::json!(operator))),
        )?;
        tx.commit()?;
        Ok(AssignmentMutation {
            assignment,
            changed: true,
            events: vec![event],
        })
    }

    /// Explicit release. The caller must name the owning session; there is no
    /// cross-session release path. Repeated release is an idempotent no-op.
    pub fn release_session_agent_assignment(
        &self,
        session_id: &str,
        identity_id: &str,
        operator: bool,
        reason: Option<&str>,
    ) -> Result<AssignmentMutation> {
        let reason = reason
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
            .map(str::to_string);
        if let Some(reason) = &reason
            && (reason.chars().count() > 500 || reason.chars().any(char::is_control))
        {
            return Err(coordination_error(
                agent_coordination_codes::ASSIGNMENT_INVALID,
                "release reason must be 1-500 printable characters",
            ));
        }
        let now_text = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let project_id = project_for_session_tx(&tx, session_id)?;
        let existing = get_assignment_tx(&tx, session_id, identity_id)?.ok_or_else(|| {
            coordination_error(
                agent_coordination_codes::ASSIGNMENT_NOT_FOUND,
                "session does not hold an assignment for this identity",
            )
        })?;
        if existing.status != "active" {
            tx.commit()?;
            return Ok(AssignmentMutation {
                assignment: existing,
                changed: false,
                events: Vec::new(),
            });
        }
        tx.execute(
            "UPDATE session_agent_assignments
             SET status = 'released', released_at = ?1, lease_token = NULL,
                 lease_expires_at = NULL
             WHERE session_id = ?2 AND identity_id = ?3 AND status = 'active'",
            params![now_text, session_id, identity_id],
        )?;
        let assignment = get_assignment_tx(&tx, session_id, identity_id)?
            .ok_or_else(|| anyhow!("assignment disappeared during release"))?;
        let event = insert_assignment_event_tx(
            &tx,
            &project_id,
            vp_protocol::types::AGENT_ASSIGNMENT_RELEASED_EVENT,
            &assignment,
            Some((
                "operator",
                serde_json::json!({
                    "operator": operator,
                    "reason": reason,
                }),
            )),
        )?;
        tx.commit()?;
        Ok(AssignmentMutation {
            assignment,
            changed: true,
            events: vec![event],
        })
    }

    /// Expire every due active lease. Startup recovery and the periodic sweeper
    /// use the same operation; each expired row emits one bounded event.
    pub fn expire_session_agent_assignments(
        &self,
        limit: Option<u32>,
    ) -> Result<Vec<AgentCoordinationEvent>> {
        self.expire_session_agent_assignments_at(OffsetDateTime::now_utc(), limit)
    }

    pub fn expire_session_agent_assignments_at(
        &self,
        now: OffsetDateTime,
        limit: Option<u32>,
    ) -> Result<Vec<AgentCoordinationEvent>> {
        let limit = limit.unwrap_or(100).clamp(1, 1_000) as i64;
        let now_text = format_time(now)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let rows = {
            let mut statement = tx.prepare(
                "SELECT a.session_id, a.identity_id, s.project_id
                 FROM session_agent_assignments a
                 JOIN sessions s ON s.id = a.session_id
                 WHERE a.status = 'active'
                   AND s.status NOT IN ('running', 'queued', 'paused', 'waiting_input')
                   AND (a.lease_expires_at IS NULL
                        OR julianday(a.lease_expires_at) <= julianday(?1))
                 ORDER BY a.lease_expires_at ASC, a.session_id ASC
                 LIMIT ?2",
            )?;
            statement
                .query_map(params![now_text, limit], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut events = Vec::with_capacity(rows.len());
        for (session_id, identity_id, project_id) in rows {
            tx.execute(
                "UPDATE session_agent_assignments
                 SET status = 'released', released_at = ?1, lease_token = NULL
                 WHERE session_id = ?2 AND identity_id = ?3 AND status = 'active'",
                params![now_text, session_id, identity_id],
            )?;
            let assignment = get_assignment_tx(&tx, &session_id, &identity_id)?
                .ok_or_else(|| anyhow!("assignment disappeared during lease sweep"))?;
            events.push(insert_assignment_event_tx(
                &tx,
                &project_id,
                vp_protocol::types::AGENT_ASSIGNMENT_EXPIRED_EVENT,
                &assignment,
                Some(("reason", serde_json::json!("lease_sweep"))),
            )?);
        }
        tx.commit()?;
        Ok(events)
    }
}

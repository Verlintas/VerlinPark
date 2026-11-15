//! Approval rows, grants, templates, attention policy, audit, and resolution.

use super::*;

impl Store {
    pub fn list_approval_grants(&self, project_id: &str) -> Result<Vec<ApprovalGrant>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, project_id, tool, path_prefix, include_children, created_by, created_at
             FROM approval_grants WHERE project_id = ?1
             ORDER BY created_at DESC, id ASC",
        )?;
        let rows = stmt.query_map(params![project_id], |row| {
            Ok(ApprovalGrant {
                id: row.get(0)?,
                project_id: row.get(1)?,
                tool: row.get(2)?,
                path_prefix: row.get(3)?,
                include_children: row.get::<_, i64>(4)? != 0,
                created_by: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn create_approval_grant(
        &self,
        project_id: &str,
        tool: &str,
        path_prefix: &str,
        include_children: bool,
        created_by: &str,
    ) -> Result<ApprovalGrant> {
        let grant = ApprovalGrant {
            id: Uuid::now_v7().to_string(),
            project_id: project_id.to_string(),
            tool: tool.to_string(),
            path_prefix: path_prefix.to_string(),
            include_children,
            created_by: created_by.to_string(),
            created_at: now_rfc3339(),
        };
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO approval_grants
             (id, project_id, tool, path_prefix, include_children, created_by, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                grant.id,
                grant.project_id,
                grant.tool,
                grant.path_prefix,
                if grant.include_children { 1 } else { 0 },
                grant.created_by,
                grant.created_at
            ],
        )?;
        Ok(grant)
    }

    pub fn delete_approval_grant(&self, id: &str) -> Result<bool> {
        let conn = self.lock()?;
        Ok(conn.execute("DELETE FROM approval_grants WHERE id = ?1", params![id])? > 0)
    }

    pub fn restore_approval_grants(
        &self,
        project_id: &str,
        grants: &[ApprovalGrant],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM approval_grants WHERE project_id = ?1",
            params![project_id],
        )?;
        for grant in grants {
            tx.execute(
                "INSERT INTO approval_grants
                 (id, project_id, tool, path_prefix, include_children, created_by, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    grant.id,
                    project_id,
                    grant.tool,
                    grant.path_prefix,
                    if grant.include_children { 1 } else { 0 },
                    grant.created_by,
                    grant.created_at
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn list_approval_templates(&self, project_id: &str) -> Result<Vec<ApprovalTemplate>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, project_id, name, enabled, tool_pattern, risk_levels, path_globs,
                    action, expires_at, max_uses, used_count, created_by, created_at, updated_at
             FROM approval_templates
             WHERE project_id = ?1
             ORDER BY created_at ASC, id ASC
             LIMIT 100",
        )?;
        let rows = stmt.query_map(params![project_id], map_approval_template)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_approval_template(&self, id: &str) -> Result<Option<ApprovalTemplate>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, project_id, name, enabled, tool_pattern, risk_levels, path_globs,
                    action, expires_at, max_uses, used_count, created_by, created_at, updated_at
             FROM approval_templates WHERE id = ?1",
            params![id],
            map_approval_template,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn create_approval_template(&self, template: &ApprovalTemplate) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO approval_templates
             (id, project_id, name, enabled, tool_pattern, risk_levels, path_globs, action,
              expires_at, max_uses, used_count, created_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                template.id,
                template.project_id,
                template.name,
                if template.enabled { 1 } else { 0 },
                template.tool_pattern,
                serde_json::to_string(&template.risk_levels)?,
                serde_json::to_string(&template.path_globs)?,
                template.action,
                template.expires_at,
                template.max_uses.map(i64::from),
                i64::from(template.used_count),
                template.created_by,
                template.created_at,
                template
                    .updated_at
                    .as_deref()
                    .unwrap_or(&template.created_at),
            ],
        )?;
        Ok(())
    }

    /// Replace a template definition. `used_count` is preserved by the caller
    /// so lowering `max_uses` cannot silently reset consumption.
    pub fn update_approval_template(&self, template: &ApprovalTemplate) -> Result<bool> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE approval_templates SET
                name = ?2,
                enabled = ?3,
                tool_pattern = ?4,
                risk_levels = ?5,
                path_globs = ?6,
                action = ?7,
                expires_at = ?8,
                max_uses = ?9,
                used_count = ?10,
                updated_at = ?11
             WHERE id = ?1",
            params![
                template.id,
                template.name,
                if template.enabled { 1 } else { 0 },
                template.tool_pattern,
                serde_json::to_string(&template.risk_levels)?,
                serde_json::to_string(&template.path_globs)?,
                template.action,
                template.expires_at,
                template.max_uses.map(i64::from),
                i64::from(template.used_count),
                template.updated_at,
            ],
        )?;
        Ok(changed > 0)
    }

    pub fn delete_approval_template(&self, id: &str) -> Result<bool> {
        let conn = self.lock()?;
        Ok(conn.execute("DELETE FROM approval_templates WHERE id = ?1", params![id])? > 0)
    }

    pub fn restore_approval_templates(
        &self,
        project_id: &str,
        templates: &[ApprovalTemplate],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM approval_templates WHERE project_id = ?1",
            params![project_id],
        )?;
        for template in templates {
            tx.execute(
                "INSERT INTO approval_templates
                 (id, project_id, name, enabled, tool_pattern, risk_levels, path_globs, action,
                  expires_at, max_uses, used_count, created_by, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    template.id,
                    project_id,
                    template.name,
                    if template.enabled { 1 } else { 0 },
                    template.tool_pattern,
                    serde_json::to_string(&template.risk_levels)?,
                    serde_json::to_string(&template.path_globs)?,
                    template.action,
                    template.expires_at,
                    template.max_uses.map(i64::from),
                    i64::from(template.used_count),
                    template.created_by,
                    template.created_at,
                    template
                        .updated_at
                        .as_deref()
                        .unwrap_or(&template.created_at),
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Atomically consume one use of an active template. Returns the remaining
    /// uses (`None` means unlimited) or `None` when the template is disabled,
    /// expired, or exhausted. The single SQLite writer lock and conditional
    /// `UPDATE` make concurrent consumption deterministic.
    pub fn consume_approval_template_use(
        &self,
        id: &str,
        now: &str,
    ) -> Result<Option<Option<u32>>> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE approval_templates
             SET used_count = used_count + 1, updated_at = ?2
             WHERE id = ?1
               AND enabled = 1
               AND (expires_at IS NULL OR expires_at > ?2)
               AND (max_uses IS NULL OR used_count < max_uses)",
            params![id, now],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let (used_count, max_uses) = conn.query_row(
            "SELECT used_count, max_uses FROM approval_templates WHERE id = ?1",
            params![id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
        )?;
        let used = used_count.max(0) as u32;
        Ok(Some(
            max_uses.map(|max| (max.max(0) as u32).saturating_sub(used)),
        ))
    }

    pub fn get_attention_policy(&self, project_id: &str) -> Result<AttentionPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT policy FROM project_attention_policies WHERE project_id = ?1",
                params![project_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(stored
            .and_then(|raw| serde_json::from_str::<AttentionPolicy>(&raw).ok())
            .unwrap_or_default()
            .normalized())
    }

    pub fn set_attention_policy(&self, project_id: &str, policy: &AttentionPolicy) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_attention_policies (project_id, policy, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(project_id) DO UPDATE SET
                policy = excluded.policy,
                updated_at = excluded.updated_at",
            params![
                project_id,
                serde_json::to_string(&policy.clone().normalized())?,
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn attention_policy_for_session(&self, session_id: &str) -> Result<AttentionPolicy> {
        let conn = self.lock()?;
        let project_id = conn
            .query_row(
                "SELECT project_id FROM sessions WHERE id = ?1",
                params![session_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        drop(conn);
        match project_id {
            Some(project_id) => self.get_attention_policy(&project_id),
            None => Ok(AttentionPolicy::default()),
        }
    }

    pub fn list_enabled_attention_policies(&self) -> Result<Vec<(String, AttentionPolicy)>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT p.id, pap.policy
             FROM projects p
             LEFT JOIN project_attention_policies pap ON pap.project_id = p.id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?;
        let mut policies = Vec::new();
        for row in rows {
            let (project_id, raw) = row?;
            let policy = raw
                .and_then(|raw| serde_json::from_str::<AttentionPolicy>(&raw).ok())
                .unwrap_or_default()
                .normalized();
            if policy.enabled {
                policies.push((project_id, policy));
            }
        }
        Ok(policies)
    }

    pub fn insert_approval_audit(&self, event: &ApprovalAuditEvent) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO approval_audit (id, project_id, kind, payload, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                event.id,
                event.project_id,
                event.kind,
                serde_json::to_string(&event.payload)?,
                event.created_at
            ],
        )?;
        Ok(())
    }

    pub fn list_approval_audit(
        &self,
        project_id: &str,
        limit: Option<u32>,
    ) -> Result<Vec<ApprovalAuditEvent>> {
        let limit = i64::from(limit.unwrap_or(200).clamp(1, 100_000));
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, project_id, kind, payload, created_at
             FROM approval_audit
             WHERE project_id = ?1
             ORDER BY created_at DESC, id DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![project_id, limit], |row| {
            Ok(ApprovalAuditEvent {
                id: row.get(0)?,
                project_id: row.get(1)?,
                kind: row.get(2)?,
                payload: serde_json::from_str(&row.get::<_, String>(3)?)
                    .unwrap_or(serde_json::Value::Null),
                created_at: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn restore_approval_audit(
        &self,
        project_id: &str,
        events: &[ApprovalAuditEvent],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM approval_audit WHERE project_id = ?1",
            params![project_id],
        )?;
        for event in events {
            tx.execute(
                "INSERT OR REPLACE INTO approval_audit (id, project_id, kind, payload, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    event.id,
                    project_id,
                    event.kind,
                    serde_json::to_string(&event.payload)?,
                    event.created_at
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Resolve one approval and return every event it produced. The row is
    /// updated only when it is still pending; a second resolution reports the
    /// current status instead of overwriting a decision.
    pub fn resolve_approval_with_events(
        &self,
        approval_id: &str,
        approved: bool,
    ) -> Result<Option<(Approval, Vec<Event>)>> {
        let status = if approved { "approved" } else { "denied" };
        let resolved_at = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE approvals SET status = ?1, resolved_at = ?2
             WHERE id = ?3 AND status = 'pending'",
            params![status, resolved_at, approval_id],
        )?;
        if changed == 0 {
            tx.commit()?;
            return Ok(None);
        }
        let approval = tx.query_row(
            &format!("{APPROVAL_SELECT} WHERE a.id = ?1"),
            params![approval_id],
            map_approval,
        )?;
        let event = insert_event_tx(
            &tx,
            &approval.session_id,
            "approval.resolved",
            serde_json::json!({
                "approval_id": approval.id,
                "approved": approved,
                "resolved_at": approval.resolved_at,
                "waiter_count": approval.waiter_count,
                "status": approval.status,
                "tool": approval.tool,
                "approval_risk": approval.approval_risk,
                "agent_id": approval.agent_id,
                "agent_name": approval.agent_name,
                "agent_role": approval.agent_role,
            }),
        )?;
        tx.commit()?;
        let events = vec![event];
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok(Some((approval, events)))
    }

    pub fn resolve_approval(&self, approval_id: &str, approved: bool) -> Result<Option<Approval>> {
        let resolved = self.resolve_approval_with_events(approval_id, approved)?;
        Ok(resolved.map(|(approval, _)| approval))
    }

    /// Resolve an explicit list. Every item is independent: a missing row or a
    /// row already resolved does not abort the others. The method never applies
    /// a blanket decision to ids the caller did not name.
    pub fn resolve_approvals_with_events(
        &self,
        decisions: &[(String, bool)],
    ) -> Result<Vec<ApprovalResolveOutcome>> {
        let mut outcomes = Vec::with_capacity(decisions.len());
        for (approval_id, approved) in decisions {
            let resolved = self.resolve_approval_with_events(approval_id, *approved)?;
            match resolved {
                Some((approval, events)) => {
                    outcomes.push(ApprovalResolveOutcome {
                        approval_id: approval_id.clone(),
                        approval: Some(approval),
                        events,
                        status: None,
                        code: None,
                    });
                }
                None => {
                    let current = self.get_approval(approval_id)?;
                    let (status, code) = match current.as_ref() {
                        Some(approval) => (Some(approval.status.clone()), "APPROVAL_NOT_PENDING"),
                        None => (None, "APPROVAL_NOT_FOUND"),
                    };
                    outcomes.push(ApprovalResolveOutcome {
                        approval_id: approval_id.clone(),
                        approval: current,
                        events: Vec::new(),
                        status,
                        code: Some(code.to_string()),
                    });
                }
            }
        }
        Ok(outcomes)
    }

    pub fn get_approval(&self, approval_id: &str) -> Result<Option<Approval>> {
        let conn = self.lock()?;
        conn.query_row(
            &format!("{APPROVAL_SELECT} WHERE a.id = ?1"),
            params![approval_id],
            map_approval,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Create an approval, coalescing identical pending requests. The caller
    /// supplies the canonical fingerprint because the inline input is still
    /// stored verbatim for review; no command text is ever hashed into the
    /// event stream.
    #[allow(clippy::too_many_arguments)]
    pub fn create_approval_deduplicated(
        &self,
        session_id: &str,
        tool_call_id: &str,
        tool: &str,
        input: &serde_json::Value,
        agent: Option<&serde_json::Value>,
        approval_risk: Option<&str>,
        approval_reason: Option<&str>,
        dedup_key: Option<&str>,
        scope_hash: Option<&str>,
        priority: i32,
        expires_at: Option<&str>,
        coalesce_window_secs: u64,
        notify_coalesce_secs: u64,
        max_pending_per_session: u32,
        boost_risk: &str,
    ) -> Result<ApprovalCreateOutcome> {
        let now = OffsetDateTime::now_utc();
        let now_string = now_rfc3339();
        let agent_fields = approval_agent_fields(agent);
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let project_id = tx
            .query_row(
                "SELECT project_id FROM sessions WHERE id = ?1",
                params![session_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| anyhow!("session not found: {session_id}"))?;

        let mut events = Vec::new();
        let mut expired = Vec::new();

        if let (Some(dedup_key), true) = (dedup_key, coalesce_window_secs > 0) {
            let cutoff = now - time::Duration::seconds(coalesce_window_secs as i64);
            let cutoff = cutoff
                .format(&Rfc3339)
                .map_err(|error| anyhow!("formatting coalesce cutoff: {error}"))?;
            let existing = tx
                .query_row(
                    &format!(
                        "{APPROVAL_SELECT} WHERE a.session_id = ?1 AND a.status = 'pending' \
                         AND a.dedup_key = ?2 AND a.created_at >= ?3 \
                         ORDER BY a.created_at DESC, a.id DESC LIMIT 1"
                    ),
                    params![session_id, dedup_key, cutoff],
                    map_approval,
                )
                .optional()?;
            if let Some(mut approval) = existing {
                approval.waiter_count = approval.waiter_count.saturating_add(1);
                tx.execute(
                    "UPDATE approvals SET waiter_count = ?1 WHERE id = ?2",
                    params![i64::from(approval.waiter_count), approval.id],
                )?;
                let event = insert_event_tx(
                    &tx,
                    session_id,
                    "approval.coalesced",
                    serde_json::json!({
                        "approval_id": approval.id,
                        "approval_tool_call_id": approval.tool_call_id,
                        "tool_call_id": tool_call_id,
                        "tool": approval.tool,
                        "approval_risk": approval.approval_risk,
                        "dedup_key": approval.dedup_key,
                        "scope_hash": approval.scope_hash,
                        "waiter_count": approval.waiter_count,
                        "created_at": approval.created_at,
                        "coalesced_at": now_string,
                        "agent_id": agent_fields.agent_id,
                        "agent_name": agent_fields.agent_name,
                        "agent_role": agent_fields.agent_role,
                        "agent_title": agent_fields.agent_title,
                        "task_id": agent_fields.task_id,
                    }),
                )?;
                events.push(event);
                tx.commit()?;
                for event in &events {
                    if let Err(error) = self.append_event_mirror(event) {
                        tracing::warn!(%error, "failed to append event mirror");
                    }
                }
                return Ok(ApprovalCreateOutcome {
                    approval,
                    events,
                    coalesced: true,
                    expired,
                });
            }
        }

        // Expire the oldest lowest-risk pending rows when the session queue is
        // over the attention cap. Never approve them: expiry denies the waiter.
        if max_pending_per_session > 0 {
            let pending: Vec<(String, Option<String>, String)> = {
                let mut stmt = tx.prepare(
                    "SELECT id, approval_risk, created_at FROM approvals
                     WHERE session_id = ?1 AND status = 'pending'",
                )?;
                let rows = stmt.query_map(params![session_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?;
                rows.collect::<Result<Vec<_>, _>>()?
            };
            let mut kept = pending.len() as u32;
            if kept >= max_pending_per_session {
                let mut candidates = pending;
                candidates.sort_by(|left, right| {
                    approval_risk_rank(left.1.as_deref())
                        .cmp(&approval_risk_rank(right.1.as_deref()))
                        .then_with(|| left.2.cmp(&right.2))
                        .then_with(|| left.0.cmp(&right.0))
                });
                for (id, _, _) in candidates {
                    if kept < max_pending_per_session {
                        break;
                    }
                    if let Some((approval, event)) =
                        expire_approval_tx(&tx, &id, "max_pending", &now_string)?
                    {
                        events.push(event);
                        expired.push(approval);
                        kept = kept.saturating_sub(1);
                    }
                }
            }
        }

        let notify_key = Some(format!(
            "{}|{}|{}",
            session_id,
            tool,
            approval_risk.unwrap_or("unknown")
        ));
        let boosted = risk_at_or_above(approval_risk, boost_risk);
        let notify = if boosted || notify_coalesce_secs == 0 {
            true
        } else {
            let cutoff = now - time::Duration::seconds(notify_coalesce_secs as i64);
            let cutoff = cutoff
                .format(&Rfc3339)
                .map_err(|error| anyhow!("formatting notify cutoff: {error}"))?;
            let recent: i64 = tx.query_row(
                "SELECT COUNT(*) FROM approvals
                 WHERE session_id = ?1 AND tool = ?2
                   AND COALESCE(approval_risk, 'unknown') = ?3
                   AND created_at >= ?4 AND notify = 1",
                params![session_id, tool, approval_risk.unwrap_or("unknown"), cutoff],
                |row| row.get(0),
            )?;
            recent == 0
        };

        let approval = Approval {
            id: Uuid::now_v7().to_string(),
            session_id: session_id.to_string(),
            tool_call_id: tool_call_id.to_string(),
            tool: tool.to_string(),
            input: input.clone(),
            status: "pending".to_string(),
            created_at: now_string.clone(),
            resolved_at: None,
            approval_risk: approval_risk.map(str::to_string),
            approval_reason: approval_reason.map(str::to_string),
            agent_id: agent_fields.agent_id.clone(),
            agent_name: agent_fields.agent_name.clone(),
            agent_role: agent_fields.agent_role.clone(),
            agent_title: agent_fields.agent_title.clone(),
            task_id: agent_fields.task_id.clone(),
            scope_hash: scope_hash.map(str::to_string),
            dedup_key: dedup_key.map(str::to_string),
            waiter_count: 1,
            template_id: None,
            priority,
            expires_at: expires_at.map(str::to_string),
            notify,
            notify_key,
        };
        tx.execute(
            "INSERT INTO approvals
             (id, session_id, tool_call_id, tool, input, status, created_at, resolved_at,
              approval_risk, approval_reason, agent_id, agent_name, agent_role, agent_title,
              task_id, scope_hash, dedup_key, waiter_count, template_id, priority, expires_at,
              notify, notify_key)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                     ?17, ?18, ?19, ?20, ?21, ?22, ?23)",
            params![
                approval.id,
                approval.session_id,
                approval.tool_call_id,
                approval.tool,
                serde_json::to_string(&approval.input)?,
                approval.status,
                approval.created_at,
                approval.resolved_at,
                approval.approval_risk,
                approval.approval_reason,
                approval.agent_id,
                approval.agent_name,
                approval.agent_role,
                approval.agent_title,
                approval.task_id,
                approval.scope_hash,
                approval.dedup_key,
                i64::from(approval.waiter_count),
                approval.template_id,
                i64::from(approval.priority),
                approval.expires_at,
                if approval.notify { 1 } else { 0 },
                approval.notify_key,
            ],
        )?;
        let payload = serde_json::to_value(&approval)?;
        let created = insert_event_tx(&tx, session_id, "approval.created", payload.clone())?;
        // Keep the pre-H1 event name so older clients, replay snapshots, and
        // tests that already look for `approval.requested` continue to work.
        let requested = insert_event_tx(&tx, session_id, "approval.requested", payload)?;
        events.push(created);
        events.push(requested);
        tx.commit()?;
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        let _ = project_id;
        Ok(ApprovalCreateOutcome {
            approval,
            events,
            coalesced: false,
            expired,
        })
    }

    /// Pending approvals in deterministic attention order. The queue never
    /// mutates state; stale expiry is a separate, auditable sweep.
    pub fn list_pending_approvals(
        &self,
        session_id: Option<&str>,
        project_id: Option<&str>,
    ) -> Result<Vec<Approval>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {APPROVAL_COLUMNS} FROM approvals a
             JOIN sessions s ON s.id = a.session_id
             WHERE a.status = 'pending'
               AND (?1 IS NULL OR a.session_id = ?1)
               AND (?2 IS NULL OR s.project_id = ?2)
             ORDER BY a.created_at ASC, a.id ASC
             LIMIT 5000"
        ))?;
        let rows = stmt.query_map(params![session_id, project_id], map_approval)?;
        let mut approvals = rows.collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        drop(conn);
        for approval in &mut approvals {
            approval.priority = self.approval_criticality(approval)?;
        }
        approvals.sort_by(compare_approval_attention);
        Ok(approvals)
    }

    /// Live blocked-task criticality for one approval. A task priority is a
    /// stable base; a running/ready task adds pressure because the approval is
    /// what stops it from progressing.
    pub fn approval_criticality(&self, approval: &Approval) -> Result<i32> {
        let Some(task_id) = approval.task_id.as_deref() else {
            return Ok(0);
        };
        self.approval_task_priority(&approval.session_id, task_id)
    }

    pub fn approval_task_priority(&self, session_id: &str, task_id: &str) -> Result<i32> {
        let conn = self.lock()?;
        let row = conn
            .query_row(
                "SELECT priority, status FROM plan_tasks
                 WHERE session_id = ?1 AND task_id = ?2
                 ORDER BY plan_seq DESC LIMIT 1",
                params![session_id, task_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        Ok(row
            .map(|(priority, status)| {
                let bonus = match status.as_str() {
                    "running" => 30,
                    "ready" | "queued" => 20,
                    "blocked" => 10,
                    "failed" => -10,
                    _ => 0,
                };
                (priority as i32 + bonus).clamp(-100, 200)
            })
            .unwrap_or(0))
    }

    /// Mark pending approvals past their computed stale deadline as `expired`
    /// and return the rows plus their events. Expiry never approves a call.
    pub fn expire_stale_approvals(
        &self,
        now: OffsetDateTime,
    ) -> Result<Vec<(Approval, Vec<Event>)>> {
        let now_string = now
            .format(&Rfc3339)
            .map_err(|error| anyhow!("formatting expiry time: {error}"))?;
        let policies = self
            .list_enabled_attention_policies()?
            .into_iter()
            .collect::<HashMap<_, _>>();
        let candidates = {
            let conn = self.lock()?;
            let mut stmt = conn.prepare(&format!(
                "SELECT {APPROVAL_COLUMNS}, s.project_id FROM approvals a
                 JOIN sessions s ON s.id = a.session_id
                 WHERE a.status = 'pending'
                 ORDER BY a.created_at ASC, a.id ASC
                 LIMIT 5000"
            ))?;
            let rows = stmt.query_map([], |row| {
                Ok((map_approval(row)?, row.get::<_, String>(23)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut expired = Vec::new();
        for (approval, project_id) in candidates {
            let Some(policy) = policies.get(&project_id) else {
                continue;
            };
            if !policy.enabled {
                continue;
            }
            let boosted = risk_at_or_above(
                approval.approval_risk.as_deref(),
                &policy.priority_boost_risk,
            );
            let multiplier = if boosted { 4 } else { 1 };
            let deadline = approval
                .expires_at
                .as_deref()
                .and_then(parse_rfc3339)
                .or_else(|| {
                    parse_rfc3339(&approval.created_at).map(|created| {
                        created
                            + time::Duration::seconds(
                                policy.stale_after_secs.saturating_mul(multiplier) as i64,
                            )
                    })
                });
            let Some(deadline) = deadline else {
                continue;
            };
            if deadline > now {
                continue;
            }
            let mut conn = self.lock()?;
            let tx = conn.transaction()?;
            if let Some((approval, event)) =
                expire_approval_tx(&tx, &approval.id, "stale", &now_string)?
            {
                tx.commit()?;
                if let Err(error) = self.append_event_mirror(&event) {
                    tracing::warn!(%error, "failed to append event mirror");
                }
                expired.push((approval, vec![event]));
            } else {
                tx.commit()?;
            }
        }
        Ok(expired)
    }

    pub fn create_approval(
        &self,
        session_id: &str,
        tool_call_id: &str,
        tool: &str,
        input: &serde_json::Value,
        approval_risk: Option<&str>,
        approval_reason: Option<&str>,
    ) -> Result<Approval> {
        let approval = Approval {
            id: Uuid::now_v7().to_string(),
            session_id: session_id.to_string(),
            tool_call_id: tool_call_id.to_string(),
            tool: tool.to_string(),
            input: input.clone(),
            status: "pending".to_string(),
            created_at: now_rfc3339(),
            resolved_at: None,
            approval_risk: approval_risk.map(str::to_string),
            approval_reason: approval_reason.map(str::to_string),
            agent_id: None,
            agent_name: None,
            agent_role: None,
            agent_title: None,
            task_id: None,
            scope_hash: None,
            dedup_key: None,
            waiter_count: 1,
            template_id: None,
            priority: 0,
            expires_at: None,
            notify: true,
            notify_key: None,
        };
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO approvals
             (id, session_id, tool_call_id, tool, input, status, created_at, approval_risk, approval_reason)
             VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?7, ?8)",
            params![
                approval.id,
                approval.session_id,
                approval.tool_call_id,
                approval.tool,
                serde_json::to_string(&approval.input)?,
                approval.created_at,
                approval.approval_risk,
                approval.approval_reason
            ],
        )?;
        Ok(approval)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_approval_with_event(
        &self,
        session_id: &str,
        tool_call_id: &str,
        tool: &str,
        input: &serde_json::Value,
        agent: Option<&serde_json::Value>,
        approval_risk: Option<&str>,
        approval_reason: Option<&str>,
    ) -> Result<(Approval, Event)> {
        let approval = Approval {
            id: Uuid::now_v7().to_string(),
            session_id: session_id.to_string(),
            tool_call_id: tool_call_id.to_string(),
            tool: tool.to_string(),
            input: input.clone(),
            status: "pending".to_string(),
            created_at: now_rfc3339(),
            resolved_at: None,
            approval_risk: approval_risk.map(str::to_string),
            approval_reason: approval_reason.map(str::to_string),
            agent_id: None,
            agent_name: None,
            agent_role: None,
            agent_title: None,
            task_id: None,
            scope_hash: None,
            dedup_key: None,
            waiter_count: 1,
            template_id: None,
            priority: 0,
            expires_at: None,
            notify: true,
            notify_key: None,
        };
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO approvals
             (id, session_id, tool_call_id, tool, input, status, created_at, approval_risk, approval_reason)
             VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?7, ?8)",
            params![
                approval.id,
                approval.session_id,
                approval.tool_call_id,
                approval.tool,
                serde_json::to_string(&approval.input)?,
                approval.created_at,
                approval.approval_risk,
                approval.approval_reason
            ],
        )?;
        let mut payload = serde_json::to_value(&approval)?;
        if let (Some(payload), Some(agent)) = (payload.as_object_mut(), agent)
            && let Some(agent) = agent.as_object()
        {
            for (key, value) in agent {
                payload.insert(key.clone(), value.clone());
            }
        }
        let event = insert_event_tx(&tx, session_id, "approval.requested", payload)?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((approval, event))
    }

    pub fn resolve_approval_with_event(
        &self,
        approval_id: &str,
        approved: bool,
    ) -> Result<Option<(Approval, Event)>> {
        let Some((approval, events)) = self.resolve_approval_with_events(approval_id, approved)?
        else {
            return Ok(None);
        };
        let Some(event) = events.into_iter().next() else {
            return Ok(None);
        };
        Ok(Some((approval, event)))
    }

    pub fn list_approvals(&self, session_id: Option<&str>) -> Result<Vec<Approval>> {
        let conn = self.lock()?;
        let sql = match session_id {
            Some(_) => {
                "SELECT a.id, a.session_id, a.tool_call_id, a.tool, a.input, a.status, a.created_at, a.resolved_at, a.approval_risk, a.approval_reason, a.agent_id, a.agent_name, a.agent_role, a.agent_title, a.task_id, a.scope_hash, a.dedup_key, a.waiter_count, a.template_id, a.priority, a.expires_at, a.notify, a.notify_key FROM approvals a WHERE a.session_id = ?1 ORDER BY a.created_at ASC, a.id ASC"
            }
            None => {
                "SELECT a.id, a.session_id, a.tool_call_id, a.tool, a.input, a.status, a.created_at, a.resolved_at, a.approval_risk, a.approval_reason, a.agent_id, a.agent_name, a.agent_role, a.agent_title, a.task_id, a.scope_hash, a.dedup_key, a.waiter_count, a.template_id, a.priority, a.expires_at, a.notify, a.notify_key FROM approvals a ORDER BY a.created_at ASC, a.id ASC"
            }
        };
        let mut stmt = conn.prepare(sql)?;
        let mut rows = match session_id {
            Some(id) => stmt.query(params![id])?,
            None => stmt.query([])?,
        };
        let mut approvals = Vec::new();
        while let Some(row) = rows.next()? {
            approvals.push(map_approval(row)?);
        }
        Ok(approvals)
    }
}

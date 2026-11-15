//! Preemption and GUI policy, computer leases, and cross-session write leases.

use super::*;

impl Store {
    pub fn get_preemption_policy(&self, project_id: &str) -> Result<PreemptionPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT enabled, provider_grace_ms, tool_grace_ms,
                        kill_escalation_ms, tool_deadline_secs,
                        checkpoint_on_preempt
                 FROM project_preemption_policies WHERE project_id = ?1",
                params![project_id],
                |row| {
                    Ok(PreemptionPolicy {
                        enabled: row.get::<_, i64>(0)? != 0,
                        provider_grace_ms: row.get::<_, i64>(1)?.max(0) as u64,
                        tool_grace_ms: row.get::<_, i64>(2)?.max(0) as u64,
                        kill_escalation_ms: row.get::<_, i64>(3)?.max(0) as u64,
                        tool_deadline_secs: row
                            .get::<_, Option<i64>>(4)?
                            .map(|value| value.max(1) as u64),
                        checkpoint_on_preempt: row.get::<_, i64>(5)? != 0,
                    })
                },
            )
            .optional()?;
        match stored {
            Some(policy) => {
                policy
                    .validate()
                    .map_err(|error| anyhow!("stored preemption policy is invalid: {error}"))?;
                Ok(policy)
            }
            None => Ok(PreemptionPolicy::default()),
        }
    }

    pub fn set_preemption_policy(&self, project_id: &str, policy: &PreemptionPolicy) -> Result<()> {
        policy
            .validate()
            .map_err(|error| anyhow!("invalid preemption policy: {error}"))?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_preemption_policies
             (project_id, enabled, provider_grace_ms, tool_grace_ms,
              kill_escalation_ms, tool_deadline_secs, checkpoint_on_preempt, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(project_id) DO UPDATE SET
                enabled = excluded.enabled,
                provider_grace_ms = excluded.provider_grace_ms,
                tool_grace_ms = excluded.tool_grace_ms,
                kill_escalation_ms = excluded.kill_escalation_ms,
                tool_deadline_secs = excluded.tool_deadline_secs,
                checkpoint_on_preempt = excluded.checkpoint_on_preempt,
                updated_at = excluded.updated_at",
            params![
                project_id,
                if policy.enabled { 1 } else { 0 },
                policy.provider_grace_ms as i64,
                policy.tool_grace_ms as i64,
                policy.kill_escalation_ms as i64,
                policy.tool_deadline_secs.map(|value| value as i64),
                if policy.checkpoint_on_preempt { 1 } else { 0 },
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    /// Project-wide terminal preemption summary. Only authoritative
    /// `session.preempted` events with `terminal: true` are counted; provider
    /// and tool sub-events are diagnostics. Rows are bounded by `limit`.
    pub fn preemption_overview(
        &self,
        project_id: &str,
        limit: u32,
    ) -> Result<(
        Vec<PreemptionSessionSummary>,
        Vec<MetricCount>,
        Vec<MetricCount>,
    )> {
        let limit = limit.clamp(1, 500);
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT e.session_id, s.goal, s.status, e.payload, e.created_at
             FROM events e
             JOIN sessions s ON s.id = e.session_id
             WHERE e.type = 'session.preempted' AND s.project_id = ?1
             ORDER BY julianday(e.created_at) DESC, e.seq DESC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![project_id, i64::from(limit) * 20], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let mut sessions: Vec<PreemptionSessionSummary> = Vec::new();
        let mut phases: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
        let mut reasons: std::collections::BTreeMap<String, u64> =
            std::collections::BTreeMap::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for row in rows {
            let (session_id, goal, status, payload, created_at) = row?;
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&payload) else {
                continue;
            };
            if value.get("terminal").and_then(serde_json::Value::as_bool) == Some(false) {
                continue;
            }
            let reason = value
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let phase = value
                .get("phase")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            *phases.entry(phase.clone()).or_default() += 1;
            *reasons.entry(reason.clone()).or_default() += 1;
            if !seen.insert(session_id.clone()) {
                if let Some(existing) = sessions
                    .iter_mut()
                    .find(|summary| summary.session_id == session_id)
                {
                    existing.terminal_count = existing.terminal_count.saturating_add(1);
                }
                continue;
            }
            sessions.push(PreemptionSessionSummary {
                session_id,
                goal,
                status,
                terminal_count: 1,
                last_reason: reason,
                last_phase: phase,
                last_tool: value
                    .get("tool")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                last_at: created_at,
            });
            if sessions.len() >= limit as usize {
                break;
            }
        }
        Ok((
            sessions,
            phases
                .into_iter()
                .map(|(key, count)| MetricCount { key, count })
                .collect(),
            reasons
                .into_iter()
                .map(|(key, count)| MetricCount { key, count })
                .collect(),
        ))
    }

    /// Project GUI session policy. A missing row decodes as the bounded
    /// default: 45s grace, 15s wait, 30s status cache, sensitive targets
    /// denied. Older databases gain the table on open.
    pub fn get_gui_policy(&self, project_id: &str) -> Result<GuiPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT policy FROM project_gui_policies WHERE project_id = ?1",
                params![project_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(stored
            .and_then(|raw| serde_json::from_str::<GuiPolicy>(&raw).ok())
            .unwrap_or_default()
            .normalized())
    }

    pub fn set_gui_policy(&self, project_id: &str, policy: &GuiPolicy) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_gui_policies (project_id, policy, updated_at)
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

    /// Resolve the GUI policy for the project that owns a session without
    /// loading the whole session row in the caller.
    pub fn gui_policy_for_session(&self, session_id: &str) -> Result<GuiPolicy> {
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
            Some(project_id) => self.get_gui_policy(&project_id),
            None => Ok(GuiPolicy::default()),
        }
    }

    /// Acquire the daemon-wide lease for an interactive surface. Expired
    /// leases are removed and audited in the same transaction; an unexpired
    /// lease on the same surface returns the current holder without changing
    /// it. The caller must hold and renew the returned lease ID.
    pub fn acquire_computer_lease(
        &self,
        surface: &str,
        owner_id: &str,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        ttl_secs: u64,
    ) -> Result<ComputerLeaseAttempt> {
        let surface = surface.trim();
        if surface.is_empty() || surface.len() > 64 {
            anyhow::bail!("computer lease surface must be between 1 and 64 characters");
        }
        if owner_id.trim().is_empty() || agent_id.trim().is_empty() {
            anyhow::bail!("computer lease owner and agent are required");
        }
        let now = time::OffsetDateTime::now_utc();
        let now_text = now
            .format(&time::format_description::well_known::Rfc3339)
            .context("formatting computer lease time")?;
        let expires = now + time::Duration::seconds(ttl_secs.min(86_400) as i64);
        let expires_text = expires
            .format(&time::format_description::well_known::Rfc3339)
            .context("formatting computer lease expiry")?;
        let mut events = Vec::new();
        let outcome = {
            let mut conn = self.lock()?;
            let tx = conn.transaction()?;
            let expired = {
                let mut stmt = tx.prepare(
                    "SELECT lease_id, surface, owner_id, session_id, agent_id, agent_name,
                            acquired_at, heartbeat_at, expires_at, generation
                     FROM computer_leases WHERE julianday(expires_at) <= julianday(?1)
                     ORDER BY expires_at ASC, lease_id ASC",
                )?;
                let rows = stmt.query_map(params![now_text], map_computer_lease)?;
                rows.collect::<Result<Vec<_>, _>>()?
            };
            for lease in &expired {
                tx.execute(
                    "DELETE FROM computer_leases WHERE lease_id = ?1",
                    params![&lease.lease_id],
                )?;
                events.push(insert_event_tx(
                    &tx,
                    &lease.session_id,
                    "computer.lease.expired",
                    serde_json::json!({
                        "lease": lease,
                        "reason": "expired before a new acquisition",
                    }),
                )?);
            }
            // Browser, computer, and gui are one physical surface. A legacy
            // row from an older daemon stays authoritative until it expires,
            // so a new `gui` acquisition cannot drive the GUI concurrently
            // with an old helper. Operators can release a stuck row.
            let holder = tx
                .query_row(
                    "SELECT lease_id, surface, owner_id, session_id, agent_id, agent_name,
                            acquired_at, heartbeat_at, expires_at, generation
                     FROM computer_leases
                     WHERE surface = ?1 OR surface IN ('gui', 'browser', 'computer')
                     ORDER BY acquired_at ASC, lease_id ASC LIMIT 1",
                    params![surface],
                    map_computer_lease,
                )
                .optional()?;
            if let Some(holder) = holder {
                events.push(insert_event_tx(
                    &tx,
                    session_id,
                    "computer.lease.denied",
                    serde_json::json!({
                        "surface": surface,
                        "holder": holder,
                        "owner_id": owner_id,
                    }),
                )?);
                tx.commit()?;
                ComputerLeaseAttempt::Busy {
                    holder: Some(holder),
                    events,
                }
            } else {
                let stored_generation = tx
                    .query_row(
                        "SELECT value FROM store_meta WHERE key = 'computer_lease_generation'",
                        [],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(0);
                let table_generation = tx.query_row(
                    "SELECT COALESCE(MAX(generation), 0) FROM computer_leases",
                    [],
                    |row| row.get::<_, i64>(0),
                )? as u64;
                let generation = stored_generation.max(table_generation) + 1;
                tx.execute(
                    "INSERT INTO store_meta (key, value)
                     VALUES ('computer_lease_generation', ?1)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![generation.to_string()],
                )?;
                let lease = ComputerLease {
                    lease_id: Uuid::now_v7().to_string(),
                    surface: surface.to_string(),
                    owner_id: owner_id.to_string(),
                    session_id: session_id.to_string(),
                    agent_id: agent_id.to_string(),
                    agent_name: agent_name.to_string(),
                    acquired_at: now_text.clone(),
                    heartbeat_at: now_text.clone(),
                    expires_at: expires_text,
                    generation,
                };
                tx.execute(
                    "INSERT INTO computer_leases
                     (lease_id, surface, owner_id, session_id, agent_id, agent_name,
                      acquired_at, heartbeat_at, expires_at, generation)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        &lease.lease_id,
                        &lease.surface,
                        &lease.owner_id,
                        &lease.session_id,
                        &lease.agent_id,
                        &lease.agent_name,
                        &lease.acquired_at,
                        &lease.heartbeat_at,
                        &lease.expires_at,
                        lease.generation as i64,
                    ],
                )?;
                events.push(insert_event_tx(
                    &tx,
                    &lease.session_id,
                    "computer.lease.acquired",
                    serde_json::to_value(&lease)?,
                )?);
                tx.commit()?;
                ComputerLeaseAttempt::Acquired { lease, events }
            }
        };
        let mirror_events = match &outcome {
            ComputerLeaseAttempt::Acquired { events, .. }
            | ComputerLeaseAttempt::Busy { events, .. } => events.clone(),
        };
        for event in &mirror_events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append computer lease event mirror");
            }
        }
        Ok(outcome)
    }

    pub fn renew_computer_lease(
        &self,
        lease_id: &str,
        owner_id: &str,
        ttl_secs: u64,
    ) -> Result<bool> {
        let now = time::OffsetDateTime::now_utc();
        let now_text = now
            .format(&time::format_description::well_known::Rfc3339)
            .context("formatting computer lease time")?;
        let expires = now + time::Duration::seconds(ttl_secs.min(86_400) as i64);
        let expires_text = expires
            .format(&time::format_description::well_known::Rfc3339)
            .context("formatting computer lease expiry")?;
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE computer_leases
             SET heartbeat_at = ?1, expires_at = ?2
             WHERE lease_id = ?3 AND owner_id = ?4",
            params![now_text, expires_text, lease_id, owner_id],
        )?;
        Ok(changed > 0)
    }

    /// Release a computer lease. `owner_id` is enforced for runtime guards;
    /// `None` lets an operator clear a stuck lease through the CLI/RPC.
    pub fn release_computer_lease(
        &self,
        lease_id: &str,
        owner_id: Option<&str>,
    ) -> Result<Option<Event>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let lease = tx
            .query_row(
                "SELECT lease_id, surface, owner_id, session_id, agent_id, agent_name,
                        acquired_at, heartbeat_at, expires_at, generation
                 FROM computer_leases WHERE lease_id = ?1",
                params![lease_id],
                map_computer_lease,
            )
            .optional()?;
        let Some(lease) = lease else {
            tx.commit()?;
            return Ok(None);
        };
        if let Some(owner_id) = owner_id
            && lease.owner_id != owner_id
        {
            tx.commit()?;
            return Ok(None);
        }
        tx.execute(
            "DELETE FROM computer_leases WHERE lease_id = ?1",
            params![lease_id],
        )?;
        let event = insert_event_tx(
            &tx,
            &lease.session_id,
            "computer.lease.released",
            serde_json::json!({
                "lease": lease,
                "reason": if owner_id.is_some() { "released by owner" } else { "released by operator" },
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append computer lease release mirror");
        }
        Ok(Some(event))
    }

    pub fn list_computer_leases(&self) -> Result<Vec<ComputerLease>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT lease_id, surface, owner_id, session_id, agent_id, agent_name,
                    acquired_at, heartbeat_at, expires_at, generation
             FROM computer_leases ORDER BY surface ASC, acquired_at ASC",
        )?;
        let rows = stmt.query_map([], map_computer_lease)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn computer_lease(&self, lease_id: &str) -> Result<Option<ComputerLease>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT lease_id, surface, owner_id, session_id, agent_id, agent_name,
                    acquired_at, heartbeat_at, expires_at, generation
             FROM computer_leases WHERE lease_id = ?1",
            params![lease_id],
            map_computer_lease,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Lowest unexpired lease on the shared physical GUI surface, including
    /// legacy `browser`/`computer` rows. The coordinator uses this to produce
    /// a typed busy error with a holder and retry hint.
    pub fn gui_lease_holder(&self) -> Result<Option<ComputerLease>> {
        let now = now_rfc3339();
        let conn = self.lock()?;
        conn.query_row(
            "SELECT lease_id, surface, owner_id, session_id, agent_id, agent_name,
                    acquired_at, heartbeat_at, expires_at, generation
             FROM computer_leases
             WHERE surface IN ('gui', 'browser', 'computer')
               AND julianday(expires_at) > julianday(?1)
             ORDER BY acquired_at ASC, lease_id ASC LIMIT 1",
            params![now],
            map_computer_lease,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Retire legacy per-helper lease rows at daemon startup. Helper processes
    /// are children of the daemon, so no legacy helper survives a restart. The
    /// rows are still audited rather than deleted silently.
    pub fn clear_legacy_gui_leases(&self, reason: &str) -> Result<(usize, Vec<Event>)> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let legacy = {
            let mut stmt = tx.prepare(
                "SELECT lease_id, surface, owner_id, session_id, agent_id, agent_name,
                        acquired_at, heartbeat_at, expires_at, generation
                 FROM computer_leases WHERE surface IN ('browser', 'computer')
                 ORDER BY acquired_at ASC, lease_id ASC",
            )?;
            let rows = stmt.query_map([], map_computer_lease)?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut events = Vec::new();
        for lease in &legacy {
            tx.execute(
                "DELETE FROM computer_leases WHERE lease_id = ?1",
                params![&lease.lease_id],
            )?;
            events.push(insert_event_tx(
                &tx,
                &lease.session_id,
                "computer.lease.expired",
                serde_json::json!({
                    "lease": lease,
                    "reason": reason,
                    "legacy_surface": true,
                }),
            )?);
        }
        tx.commit()?;
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append legacy GUI lease expiry mirror");
            }
        }
        Ok((legacy.len(), events))
    }

    /// Remove expired computer leases and return their audit events. The
    /// engine broadcasts the events after the transaction commits.
    pub fn clear_expired_computer_leases(&self) -> Result<(usize, Vec<Event>)> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let expired = {
            let mut stmt = tx.prepare(
                "SELECT lease_id, surface, owner_id, session_id, agent_id, agent_name,
                        acquired_at, heartbeat_at, expires_at, generation
                 FROM computer_leases WHERE julianday(expires_at) <= julianday(?1)
                 ORDER BY expires_at ASC, lease_id ASC",
            )?;
            let rows = stmt.query_map(params![now], map_computer_lease)?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut events = Vec::new();
        for lease in &expired {
            tx.execute(
                "DELETE FROM computer_leases WHERE lease_id = ?1",
                params![&lease.lease_id],
            )?;
            events.push(insert_event_tx(
                &tx,
                &lease.session_id,
                "computer.lease.expired",
                serde_json::json!({
                    "lease": lease,
                    "reason": "expired during lease sweep",
                }),
            )?);
        }
        tx.commit()?;
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append computer lease expiry mirror");
            }
        }
        Ok((expired.len(), events))
    }

    pub fn try_acquire_write_lease(
        &self,
        project_id: &str,
        session_id: &str,
        agent_id: &str,
        scopes: &[String],
    ) -> Result<bool> {
        let scopes = scopes
            .iter()
            .map(|scope| normalize_write_scope(scope))
            .collect::<Vec<_>>();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let conflicts = {
            let mut stmt = tx.prepare(
                "SELECT session_id, scopes FROM write_leases
                 WHERE project_id = ?1 AND agent_id <> ?2",
            )?;
            let rows = stmt.query_map(params![project_id, agent_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            let mut conflicts = false;
            for row in rows {
                let (owner_session, stored) = row?;
                if owner_session == session_id {
                    continue;
                }
                let stored = serde_json::from_str::<Vec<String>>(&stored).unwrap_or_default();
                if write_scopes_overlap(&scopes, &stored) {
                    conflicts = true;
                    break;
                }
            }
            conflicts
        };
        if conflicts {
            tx.commit()?;
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO write_leases (agent_id, project_id, session_id, scopes, acquired_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(agent_id) DO UPDATE SET
                project_id = excluded.project_id,
                session_id = excluded.session_id,
                scopes = excluded.scopes,
                acquired_at = excluded.acquired_at",
            params![
                agent_id,
                project_id,
                session_id,
                serde_json::to_string(&scopes)?,
                now_rfc3339()
            ],
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub fn release_write_lease(&self, agent_id: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "DELETE FROM write_leases WHERE agent_id = ?1",
            params![agent_id],
        )?;
        Ok(())
    }

    pub fn list_write_leases(
        &self,
        project_id: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<Vec<WriteLease>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT project_id, session_id, agent_id, scopes, acquired_at
             FROM write_leases
             WHERE (?1 IS NULL OR project_id = ?1) AND (?2 IS NULL OR session_id = ?2)
             ORDER BY project_id ASC, session_id ASC, agent_id ASC",
        )?;
        let rows = stmt.query_map(params![project_id, session_id], |row| {
            let scopes = row.get::<_, String>(3)?;
            Ok(WriteLease {
                project_id: row.get(0)?,
                session_id: row.get(1)?,
                agent_id: row.get(2)?,
                scopes: serde_json::from_str(&scopes).unwrap_or_default(),
                acquired_at: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

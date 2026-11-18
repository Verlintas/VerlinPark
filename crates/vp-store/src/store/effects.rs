//! Effect receipts, settlement, reconciliation, and stale-intent sweeps.

use super::*;

impl Store {
    /// Insert a pre-execution intent, or return the existing logical receipt.
    /// The partial unique index makes `intent`/`done`/`unknown` idempotent; a
    /// `failed` row gets a new attempt row. The `effect.intent` event and the
    /// projection row commit together.
    pub fn begin_effect(
        &self,
        receipt: &EffectReceipt,
    ) -> Result<(EffectReceipt, Vec<Event>, bool)> {
        validate_effect_state(&receipt.state, &[EffectState::Intent])?;
        validate_effect_receipt(receipt)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let existing = latest_effect_tx(
            &tx,
            &receipt.session_id,
            receipt.task_id.as_deref(),
            &receipt.tool,
            &receipt.target.canonical(),
            &receipt.request_hash,
        )?;
        if let Some(existing) = existing {
            match existing.state {
                EffectState::Done | EffectState::Unknown | EffectState::Intent => {
                    drop(tx);
                    return Ok((existing, Vec::new(), false));
                }
                EffectState::Failed => {
                    let mut next = receipt.clone();
                    next.id = Uuid::now_v7().to_string();
                    next.attempt = existing.attempt.saturating_add(1).max(1);
                    next.created_at = now_rfc3339();
                    next.resolved_at = None;
                    insert_effect_tx(&tx, &next)?;
                    let event = insert_event_tx(
                        &tx,
                        &next.session_id,
                        vp_protocol::types::EFFECT_INTENT_EVENT,
                        serde_json::json!({"receipt": serde_json::to_value(&next)?}),
                    )?;
                    tx.commit()?;
                    let mirror = event.clone();
                    if let Err(error) = self.append_event_mirror(&mirror) {
                        tracing::warn!(%error, "failed to append event mirror");
                    }
                    return Ok((next, vec![event], true));
                }
            }
        }
        let mut next = receipt.clone();
        if next.attempt == 0 {
            next.attempt = 1;
        }
        match insert_effect_tx(&tx, &next) {
            Ok(()) => {}
            Err(error) if is_constraint_violation(&error) => {
                // A concurrent writer won the idempotency race. Return its
                // receipt; never emit a second intent event.
                let winner = latest_effect_tx(
                    &tx,
                    &receipt.session_id,
                    receipt.task_id.as_deref(),
                    &receipt.tool,
                    &receipt.target.canonical(),
                    &receipt.request_hash,
                )?
                .ok_or_else(|| anyhow!("effect receipt conflicted but no row was found"))?;
                drop(tx);
                return Ok((winner, Vec::new(), false));
            }
            Err(error) => return Err(error),
        }
        let event = insert_event_tx(
            &tx,
            &next.session_id,
            vp_protocol::types::EFFECT_INTENT_EVENT,
            serde_json::json!({"receipt": serde_json::to_value(&next)?}),
        )?;
        tx.commit()?;
        let mirror = event.clone();
        if let Err(error) = self.append_event_mirror(&mirror) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((next, vec![event], true))
    }

    /// Insert a receipt that is already settled (a runtime action observed by
    /// the caller after the fact). The row and its `effect.*` event commit in
    /// one transaction; an existing logical receipt is returned unchanged.
    pub fn record_settled_effect(
        &self,
        receipt: &EffectReceipt,
    ) -> Result<(EffectReceipt, Option<Event>, bool)> {
        validate_effect_state(
            &receipt.state,
            &[EffectState::Done, EffectState::Failed, EffectState::Unknown],
        )?;
        validate_effect_receipt(receipt)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let existing = latest_effect_tx(
            &tx,
            &receipt.session_id,
            receipt.task_id.as_deref(),
            &receipt.tool,
            &receipt.target.canonical(),
            &receipt.request_hash,
        )?;
        if let Some(existing) = existing {
            drop(tx);
            return Ok((existing, None, false));
        }
        let mut stored = receipt.clone();
        stored.resolved_at.get_or_insert_with(now_rfc3339);
        match insert_effect_tx(&tx, &stored) {
            Ok(()) => {}
            Err(error) if is_constraint_violation(&error) => {
                let winner = latest_effect_tx(
                    &tx,
                    &receipt.session_id,
                    receipt.task_id.as_deref(),
                    &receipt.tool,
                    &receipt.target.canonical(),
                    &receipt.request_hash,
                )?
                .ok_or_else(|| anyhow!("effect receipt conflicted but no row was found"))?;
                drop(tx);
                return Ok((winner, None, false));
            }
            Err(error) => return Err(error),
        }
        let event = insert_effect_event_tx(&tx, &stored)?;
        tx.commit()?;
        let mirror = event.clone();
        if let Err(error) = self.append_event_mirror(&mirror) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((stored, Some(event), true))
    }

    /// Settle one intent row. `done` is terminal; `unknown` is terminal until
    /// an explicit reconcile; `failed` is terminal for that attempt. A stale
    /// or mismatched transition is rejected instead of overwritten.
    pub fn settle_effect(
        &self,
        receipt: &EffectReceipt,
    ) -> Result<(EffectReceipt, Vec<Event>, bool)> {
        validate_effect_state(
            &receipt.state,
            &[EffectState::Done, EffectState::Failed, EffectState::Unknown],
        )?;
        validate_effect_receipt(receipt)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let now = now_rfc3339();
        let mut stored = receipt.clone();
        stored.resolved_at = Some(now.clone());
        // A live completion may replace the sweep's synthetic `unknown` with
        // the real probe result. Genuine unknown receipts stay terminal and
        // still require explicit reconciliation. The sweep marker is a
        // dedicated column, never a substring of free-form evidence.
        let changed = tx.execute(
            "UPDATE effect_receipts
             SET state = ?2, provider_key = COALESCE(?3, provider_key),
                 external_id = COALESCE(?4, external_id),
                 response_hash = COALESCE(?5, response_hash),
                 evidence_json = ?6, resolved_at = ?7, settled_by_sweep = 0
             WHERE id = ?1
               AND (state = 'intent'
                    OR (state = 'unknown' AND settled_by_sweep = 1))",
            params![
                stored.id,
                stored.state.as_str(),
                stored.provider_key,
                stored.external_id,
                stored.response_hash,
                serde_json::to_string(&stored.evidence)?,
                stored.resolved_at
            ],
        )?;
        if changed == 0 {
            let existing = effect_by_id_tx(&tx, &stored.id)?
                .ok_or_else(|| anyhow!("effect receipt not found: {}", stored.id))?;
            if existing.state == stored.state {
                drop(tx);
                return Ok((existing, Vec::new(), false));
            }
            return Err(anyhow!(
                "effect receipt {} cannot transition from {} to {}; use effect.reconcile for unknown/intent rows",
                existing.id,
                existing.state.as_str(),
                stored.state.as_str()
            ));
        }
        let event = insert_effect_event_tx(&tx, &stored)?;
        tx.commit()?;
        let mirror = event.clone();
        if let Err(error) = self.append_event_mirror(&mirror) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((stored, vec![event], changed > 0))
    }

    pub fn get_effect(&self, id: &str) -> Result<Option<EffectReceipt>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, session_id, task_id, attempt, tool, target, request_hash, state,
                    provider_key, external_id, response_hash, evidence_json, created_at,
                    resolved_at
             FROM effect_receipts WHERE id = ?1",
            params![id],
            map_effect_receipt,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Latest row for one logical effect identity. `task_id = None` matches the
    /// session-scoped row, not every task row.
    pub fn latest_effect(
        &self,
        session_id: &str,
        task_id: Option<&str>,
        tool: &str,
        target: &str,
        request_hash: &str,
    ) -> Result<Option<EffectReceipt>> {
        let conn = self.lock()?;
        query_latest_effect(&conn, session_id, task_id, tool, target, request_hash)
    }

    pub fn list_effect_receipts(
        &self,
        session_id: &str,
        task_id: Option<&str>,
        include_settled: bool,
        limit: u32,
    ) -> Result<Vec<EffectReceipt>> {
        let limit = limit.clamp(1, 500) as i64;
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT id, session_id, task_id, attempt, tool, target, request_hash, state,
                    provider_key, external_id, response_hash, evidence_json, created_at,
                    resolved_at
             FROM effect_receipts
             WHERE session_id = ?1
               AND (?2 IS NULL OR task_id = ?2)
               AND (?3 = 1 OR state IN ('intent', 'unknown'))
             ORDER BY created_at DESC, id DESC
             LIMIT ?4",
        )?;
        let rows = statement
            .query_map(
                params![
                    session_id,
                    task_id,
                    if include_settled { 1 } else { 0 },
                    limit
                ],
                map_effect_receipt,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Count unresolved receipts without the 500-row display cap. The effects
    /// report uses this to fail closed when truncation could hide a blocker.
    pub fn count_unresolved_effect_receipts(
        &self,
        session_id: &str,
        task_id: Option<&str>,
    ) -> Result<u64> {
        let conn = self.lock()?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM effect_receipts
             WHERE session_id = ?1
               AND (?2 IS NULL OR task_id = ?2)
               AND state IN ('intent', 'unknown')",
            params![session_id, task_id],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as u64)
    }

    pub fn replace_effect_receipts(&self, session_id: &str, rows: &[EffectReceipt]) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM effect_receipts WHERE session_id = ?1",
            params![session_id],
        )?;
        for row in rows {
            if row.session_id != session_id {
                return Err(anyhow!(
                    "effect receipt {} belongs to a different session",
                    row.id
                ));
            }
            validate_effect_receipt(row)?;
            insert_effect_tx(&tx, row)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Resolve an `intent`/`unknown`/`failed` row explicitly. The event and the
    /// projection update commit together. A `done` row is terminal and can
    /// never be reconciled to another state.
    pub fn reconcile_effect(
        &self,
        effect_id: &str,
        state: EffectState,
        evidence: &EffectEvidence,
        operator: Option<&str>,
    ) -> Result<(EffectReceipt, Vec<Event>, bool)> {
        validate_effect_state(&state, &[EffectState::Done, EffectState::Failed])?;
        if state == EffectState::Done && !evidence.has_resolution_evidence() {
            return Err(anyhow!("marking an effect done requires bounded evidence"));
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let existing = effect_by_id_tx(&tx, effect_id)?
            .ok_or_else(|| anyhow!("effect receipt not found: {effect_id}"))?;
        if existing.state == state {
            drop(tx);
            return Ok((existing, Vec::new(), false));
        }
        if existing.state == EffectState::Done {
            return Err(anyhow!(
                "effect receipt {effect_id} is already done and terminal"
            ));
        }
        let now = now_rfc3339();
        let mut stored = existing;
        stored.state = state;
        stored.resolved_at = Some(now);
        stored.evidence = evidence.clone();
        if let Some(external_id) = &evidence.external_id {
            stored.external_id = Some(external_id.clone());
        }
        if let Some(commit) = &evidence.commit {
            stored.external_id = Some(commit.clone());
        }
        if let Some(sha256) = &evidence.sha256 {
            stored.response_hash = Some(sha256.clone());
        }
        tx.execute(
            "UPDATE effect_receipts
             SET state = ?2, evidence_json = ?3, resolved_at = ?4,
                 external_id = COALESCE(?5, external_id),
                 response_hash = COALESCE(?6, response_hash)
             WHERE id = ?1",
            params![
                stored.id,
                stored.state.as_str(),
                serde_json::to_string(&stored.evidence)?,
                stored.resolved_at,
                stored.external_id,
                stored.response_hash
            ],
        )?;
        let event = insert_event_tx(
            &tx,
            &stored.session_id,
            vp_protocol::types::EFFECT_RECONCILED_EVENT,
            serde_json::json!({
                "receipt": serde_json::to_value(&stored)?,
                "operator": bounded_label(operator, 120),
            }),
        )?;
        tx.commit()?;
        let mirror = event.clone();
        if let Err(error) = self.append_event_mirror(&mirror) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((stored, vec![event], true))
    }

    /// Mark stale `intent` rows `unknown` after the project's grace period.
    /// This is bounded per call and never retries or resolves an effect.
    pub fn sweep_stale_effect_intents(
        &self,
        project_id: &str,
        grace_secs: u64,
        limit: u32,
        now: &time::OffsetDateTime,
    ) -> Result<Vec<Event>> {
        let cutoff = *now - time::Duration::seconds(grace_secs.min(86_400) as i64);
        let limit = limit.clamp(1, 1_000) as i64;
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        // Never sweep an intent owned by a live session. The grace period is a
        // post-restart recovery bound, not a wall-clock timeout on a tool that
        // is still running; a long `cargo test` must be allowed to settle.
        let mut statement = tx.prepare(
            "SELECT e.id, e.session_id, e.created_at
             FROM effect_receipts e
             JOIN sessions s ON s.id = e.session_id
             WHERE s.project_id = ?1 AND e.state = 'intent'
               AND s.status NOT IN ('running', 'queued', 'paused', 'waiting_input')
             ORDER BY e.created_at ASC
             LIMIT ?2",
        )?;
        let candidates = statement
            .query_map(params![project_id, limit], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        let mut events = Vec::new();
        for (id, _session_id, created_at) in candidates {
            let Ok(created) = time::OffsetDateTime::parse(
                &created_at,
                &time::format_description::well_known::Rfc3339,
            ) else {
                continue;
            };
            if created > cutoff {
                continue;
            }
            let Some(mut stored) = effect_by_id_tx(&tx, &id)? else {
                continue;
            };
            stored.state = EffectState::Unknown;
            stored.resolved_at = Some(now_rfc3339());
            stored.evidence = EffectEvidence {
                probe: "stale_intent_sweep".to_string(),
                outcome: "unverifiable".to_string(),
                reason: Some("stale_intent_after_restart".to_string()),
                note: Some(format!(
                    "intent was not settled within {grace_secs}s; the daemon cannot prove whether the effect happened"
                )),
                ..EffectEvidence::default()
            };
            tx.execute(
                "UPDATE effect_receipts
                 SET state = 'unknown', evidence_json = ?2, resolved_at = ?3,
                     settled_by_sweep = 1
                 WHERE id = ?1 AND state = 'intent'",
                params![
                    stored.id,
                    serde_json::to_string(&stored.evidence)?,
                    stored.resolved_at
                ],
            )?;
            events.push(insert_effect_event_tx(&tx, &stored)?);
        }
        tx.commit()?;
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok(events)
    }
}

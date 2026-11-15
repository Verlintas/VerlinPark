//! Cache policy, instruction blobs, and prompt-prefix telemetry.

use super::*;

impl Store {
    pub fn get_cache_policy(&self, project_id: &str) -> Result<CachePolicy> {
        let conn = self.lock()?;
        let stored: Option<String> = conn
            .query_row(
                "SELECT policy FROM project_cache_policies WHERE project_id = ?1",
                params![project_id],
                |row| row.get(0),
            )
            .optional()?;
        match stored {
            Some(encoded) => {
                serde_json::from_str(&encoded).context("parsing stored project cache policy")
            }
            None => Ok(CachePolicy::default()),
        }
    }

    pub fn set_cache_policy(&self, project_id: &str, policy: &CachePolicy) -> Result<()> {
        let encoded = serde_json::to_string(policy).context("serializing project cache policy")?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_cache_policies (project_id, policy, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(project_id) DO UPDATE SET
                policy = excluded.policy,
                updated_at = excluded.updated_at",
            params![project_id, encoded, now_rfc3339()],
        )?;
        Ok(())
    }

    /// Insert a content-addressed instruction body. Identical text is
    /// deduplicated; the original creation timestamp is preserved. If a row
    /// with the same hash already exists but its text differs, the row is
    /// repaired from the verified text (the hash is computed from `text` here,
    /// so the new body is authoritative). The text is bounded here as well as
    /// in the core materializer so an imported bundle or a future caller cannot
    /// store an unbounded system prompt.
    pub fn put_instruction_blob(&self, text: &str) -> Result<InstructionBlob> {
        if text.len() > MAX_INSTRUCTION_BLOB_BYTES {
            return Err(anyhow!(
                "instruction blob is {} bytes; the hard limit is {} bytes",
                text.len(),
                MAX_INSTRUCTION_BLOB_BYTES
            ));
        }
        let hash = format!("{:x}", Sha256::digest(text.as_bytes()));
        let now = now_rfc3339();
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO instruction_blobs (hash, text, created_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(hash) DO UPDATE SET text = excluded.text
             WHERE instruction_blobs.text != excluded.text",
            params![hash, text, now],
        )?;
        let created_at: String = conn.query_row(
            "SELECT created_at FROM instruction_blobs WHERE hash = ?1",
            params![hash],
            |row| row.get(0),
        )?;
        Ok(InstructionBlob {
            hash,
            text: text.to_string(),
            created_at,
        })
    }

    /// Insert a blob from an explicit export bundle after verifying its hash.
    /// A mismatched hash is rejected rather than overwriting the correct body.
    pub fn import_instruction_blob(&self, blob: &InstructionBlob) -> Result<()> {
        if blob.hash.len() != 64 || !blob.hash.chars().all(|value| value.is_ascii_hexdigit()) {
            return Err(anyhow!("instruction blob hash is not sha256 hex"));
        }
        if blob.text.len() > MAX_INSTRUCTION_BLOB_BYTES {
            return Err(anyhow!(
                "instruction blob {} is {} bytes; the hard limit is {} bytes",
                blob.hash,
                blob.text.len(),
                MAX_INSTRUCTION_BLOB_BYTES
            ));
        }
        let actual = format!("{:x}", Sha256::digest(blob.text.as_bytes()));
        if actual != blob.hash {
            return Err(anyhow!(
                "instruction blob {} hash does not match its text",
                blob.hash
            ));
        }
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO instruction_blobs (hash, text, created_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(hash) DO UPDATE SET text = excluded.text
             WHERE instruction_blobs.text != excluded.text",
            params![blob.hash, blob.text, blob.created_at],
        )?;
        Ok(())
    }

    pub fn get_instruction_blob(&self, hash: &str) -> Result<Option<InstructionBlob>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT hash, text, created_at FROM instruction_blobs WHERE hash = ?1",
            params![hash],
            |row| {
                Ok(InstructionBlob {
                    hash: row.get(0)?,
                    text: row.get(1)?,
                    created_at: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn delete_instruction_blob(&self, hash: &str) -> Result<bool> {
        let conn = self.lock()?;
        let removed = conn.execute(
            "DELETE FROM instruction_blobs WHERE hash = ?1",
            params![hash],
        )?;
        Ok(removed > 0)
    }

    pub fn list_session_instructions(
        &self,
        scope_id: &str,
    ) -> Result<Vec<SessionInstructionState>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT session_id, key, hash, ordinal, tail, updated_at
             FROM session_instruction_state
             WHERE session_id = ?1
             ORDER BY ordinal ASC, key ASC",
        )?;
        let rows = stmt.query_map(params![scope_id], map_session_instruction)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// List the Leader state and every task-scoped state for one session.
    /// Scope ids are `<session_id>#<agent_id>`; matching is parameterized and
    /// bounded by the caller's session key space.
    pub fn list_instruction_states_for_session(
        &self,
        session_id: &str,
    ) -> Result<Vec<SessionInstructionState>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT session_id, key, hash, ordinal, tail, updated_at
             FROM session_instruction_state
             WHERE session_id = ?1 OR session_id LIKE ?2
             ORDER BY session_id ASC, ordinal ASC, key ASC",
        )?;
        let prefix = format!("{session_id}#%");
        let rows = stmt.query_map(params![session_id, prefix], map_session_instruction)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Replace all ordered rows for one scope. Used by materialization and
    /// replay repair; rows not present in the new set are deleted.
    pub fn replace_session_instructions(
        &self,
        scope_id: &str,
        rows: &[SessionInstructionState],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM session_instruction_state WHERE session_id = ?1",
            params![scope_id],
        )?;
        for row in rows {
            if row.session_id != scope_id {
                return Err(anyhow!(
                    "instruction state row belongs to {}, expected scope {}",
                    row.session_id,
                    scope_id
                ));
            }
            tx.execute(
                "INSERT INTO session_instruction_state
                 (session_id, key, hash, ordinal, tail, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    row.session_id,
                    row.key,
                    row.hash,
                    row.ordinal as i64,
                    if row.tail { 1 } else { 0 },
                    row.updated_at
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn upsert_session_instruction(&self, row: &SessionInstructionState) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO session_instruction_state
             (session_id, key, hash, ordinal, tail, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(session_id, key) DO UPDATE SET
                hash = excluded.hash,
                ordinal = excluded.ordinal,
                tail = excluded.tail,
                updated_at = excluded.updated_at",
            params![
                row.session_id,
                row.key,
                row.hash,
                row.ordinal as i64,
                if row.tail { 1 } else { 0 },
                row.updated_at
            ],
        )?;
        Ok(())
    }

    pub fn delete_session_instruction(&self, scope_id: &str, key: &str) -> Result<bool> {
        let conn = self.lock()?;
        let removed = conn.execute(
            "DELETE FROM session_instruction_state WHERE session_id = ?1 AND key = ?2",
            params![scope_id, key],
        )?;
        Ok(removed > 0)
    }

    /// Append a `context.cache` event and fold its provider observation into
    /// the cache-epoch projection in one transaction.
    ///
    /// Cross-session fields are computed *before* the event payload is
    /// persisted, so replay derives exactly the same observation from the
    /// event stream. This is intentionally not a post-hoc projection update:
    /// an event without its observation, or an observation without its event,
    /// cannot survive a commit.
    pub fn append_cache_call_event(
        &self,
        session_id: &str,
        kind: &str,
        mut payload: serde_json::Value,
        call: &PromptPrefixCall,
    ) -> Result<(Event, PromptPrefixRecord)> {
        if kind != "context.cache" {
            return Err(anyhow!(
                "append_cache_call_event only accepts context.cache, got {kind}"
            ));
        }
        if call.session_id != session_id {
            return Err(anyhow!(
                "cache observation session {} does not match event session {}",
                call.session_id,
                session_id
            ));
        }
        if !cache_epoch_hex(&call.cache_epoch) {
            return Err(anyhow!(
                "cache epoch must be 64 lowercase hex characters: {}",
                call.cache_epoch
            ));
        }
        if call.input_tokens == 0 {
            return Err(anyhow!(
                "cache observation for {} has zero input tokens",
                call.session_id
            ));
        }
        let session = self
            .get_session(session_id)?
            .ok_or_else(|| anyhow!("session not found: {session_id}"))?;
        if session.project_id != call.project_id {
            return Err(anyhow!(
                "cache observation project {} does not match session project {}",
                call.project_id,
                session.project_id
            ));
        }
        let now = now_rfc3339();
        let (event, record) = {
            let mut conn = self.lock()?;
            let tx = conn.transaction()?;
            let record = record_prompt_prefix_call_tx(&tx, call, &now)?;
            if let Some(object) = payload.as_object_mut() {
                object.insert(
                    "cache_epoch".into(),
                    serde_json::json!(record.aggregate.cache_epoch),
                );
                object.insert(
                    "cache_epoch_reused_from_sessions".into(),
                    serde_json::json!(record.prior_sessions),
                );
                object.insert(
                    "cache_epoch_session_count".into(),
                    serde_json::json!(record.aggregate.session_count),
                );
                object.insert(
                    "cache_epoch_call_count".into(),
                    serde_json::json!(record.aggregate.call_count),
                );
                object.insert(
                    "cross_session_first_call".into(),
                    serde_json::json!(record.first_session_call),
                );
                object.insert(
                    "cross_session_cache_read_tokens".into(),
                    serde_json::json!(if record.first_session_call && record.prior_sessions > 0 {
                        call.cache_read_tokens.min(call.input_tokens)
                    } else {
                        0
                    }),
                );
            }
            let event = insert_event_at_tx(&tx, session_id, kind, payload, &now)?;
            tx.commit()?;
            (event, record)
        };
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((event, record))
    }

    /// Record one provider call without an event. Used by tests and by
    /// maintenance paths that already own the event transaction.
    pub fn record_prompt_prefix_call(&self, call: &PromptPrefixCall) -> Result<PromptPrefixRecord> {
        if !cache_epoch_hex(&call.cache_epoch) {
            return Err(anyhow!(
                "cache epoch must be 64 lowercase hex characters: {}",
                call.cache_epoch
            ));
        }
        if call.input_tokens == 0 {
            return Err(anyhow!("cache observation has zero input tokens"));
        }
        let session = self
            .get_session(&call.session_id)?
            .ok_or_else(|| anyhow!("session not found: {}", call.session_id))?;
        if session.project_id != call.project_id {
            return Err(anyhow!(
                "cache observation project {} does not match session project {}",
                call.project_id,
                session.project_id
            ));
        }
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let record = record_prompt_prefix_call_tx(&tx, call, &now)?;
        tx.commit()?;
        Ok(record)
    }

    pub fn prompt_prefix_aggregate(
        &self,
        project_id: &str,
        cache_epoch: &str,
    ) -> Result<Option<PromptPrefixAggregate>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT project_id, cache_epoch, model, tool_bundle, tool_bundle_hash,
                    core_hash, project_hash, core_sections, project_sections,
                    first_seen, last_seen, observed_input_tokens,
                    observed_cache_read_tokens, session_count, call_count
             FROM prompt_prefixes
             WHERE project_id = ?1 AND cache_epoch = ?2",
            params![project_id, cache_epoch],
            map_prompt_prefix_aggregate,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn prompt_prefix_observation(
        &self,
        project_id: &str,
        cache_epoch: &str,
        session_id: &str,
    ) -> Result<Option<PromptPrefixObservation>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT project_id, cache_epoch, session_id, model, tool_bundle,
                    tool_bundle_hash, core_hash, project_hash, core_sections,
                    project_sections, first_seen, last_seen,
                    observed_input_tokens, observed_cache_read_tokens, calls
             FROM prompt_prefix_observations
             WHERE project_id = ?1 AND cache_epoch = ?2 AND session_id = ?3",
            params![project_id, cache_epoch, session_id],
            map_prompt_prefix_observation,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn list_prompt_prefix_observations(
        &self,
        project_id: &str,
        session_id: &str,
    ) -> Result<Vec<PromptPrefixObservation>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT project_id, cache_epoch, session_id, model, tool_bundle,
                    tool_bundle_hash, core_hash, project_hash, core_sections,
                    project_sections, first_seen, last_seen,
                    observed_input_tokens, observed_cache_read_tokens, calls
             FROM prompt_prefix_observations
             WHERE project_id = ?1 AND session_id = ?2
             ORDER BY cache_epoch ASC",
        )?;
        let rows = statement.query_map(
            params![project_id, session_id],
            map_prompt_prefix_observation,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Bounded project-wide epoch aggregates, newest last-seen first.
    pub fn list_prompt_prefix_aggregates(
        &self,
        project_id: &str,
        limit: u32,
    ) -> Result<Vec<PromptPrefixAggregate>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT project_id, cache_epoch, model, tool_bundle, tool_bundle_hash,
                    core_hash, project_hash, core_sections, project_sections,
                    first_seen, last_seen, observed_input_tokens,
                    observed_cache_read_tokens, session_count, call_count
             FROM prompt_prefixes
             WHERE project_id = ?1
             ORDER BY last_seen DESC, cache_epoch ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![project_id, i64::from(limit.clamp(1, 1_000))],
            map_prompt_prefix_aggregate,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn count_prompt_prefix_aggregates(&self, project_id: &str) -> Result<u64> {
        let conn = self.lock()?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM prompt_prefixes WHERE project_id = ?1",
            params![project_id],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as u64)
    }

    /// Bounded other sessions that observed one epoch. Titles come from the
    /// session row; no message or prompt body is read.
    pub fn list_prompt_prefix_reuse_sessions(
        &self,
        project_id: &str,
        cache_epoch: &str,
        exclude_session: Option<&str>,
        limit: u32,
    ) -> Result<Vec<CacheEpochReuseSession>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT o.session_id, COALESCE(s.goal, ''), o.first_seen, o.last_seen,
                    o.calls, o.observed_input_tokens, o.observed_cache_read_tokens
             FROM prompt_prefix_observations o
             LEFT JOIN sessions s ON s.id = o.session_id
             WHERE o.project_id = ?1 AND o.cache_epoch = ?2
               AND (?3 IS NULL OR o.session_id != ?3)
             ORDER BY o.last_seen DESC, o.session_id ASC
             LIMIT ?4",
        )?;
        let rows = statement.query_map(
            params![
                project_id,
                cache_epoch,
                exclude_session,
                i64::from(limit.clamp(1, 200))
            ],
            |row| {
                let input: i64 = row.get(5)?;
                let read: i64 = row.get(6)?;
                Ok(CacheEpochReuseSession {
                    session_id: row.get(0)?,
                    title: row.get(1)?,
                    first_seen: row.get(2)?,
                    last_seen: row.get(3)?,
                    calls: row.get::<_, i64>(4)?.max(0) as u64,
                    observed_input_tokens: input.max(0) as u64,
                    observed_cache_read_tokens: read.max(0) as u64,
                    cache_hit_ratio: if input <= 0 {
                        0.0
                    } else {
                        (read.max(0) as f64 / input as f64).clamp(0.0, 1.0)
                    },
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Group observations by frozen tool bundle for a project report.
    pub fn prompt_prefix_bundle_buckets(
        &self,
        project_id: &str,
    ) -> Result<Vec<CacheEpochBundleBucket>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT tool_bundle,
                    COUNT(DISTINCT cache_epoch),
                    COUNT(DISTINCT session_id),
                    COALESCE(SUM(calls), 0),
                    COALESCE(SUM(observed_input_tokens), 0),
                    COALESCE(SUM(observed_cache_read_tokens), 0)
             FROM prompt_prefix_observations
             WHERE project_id = ?1
             GROUP BY tool_bundle
             ORDER BY tool_bundle ASC",
        )?;
        let rows = statement.query_map(params![project_id], |row| {
            let input: i64 = row.get(4)?;
            let read: i64 = row.get(5)?;
            Ok(CacheEpochBundleBucket {
                tool_bundle: row.get(0)?,
                epochs: row.get::<_, i64>(1)?.max(0) as u64,
                sessions: row.get::<_, i64>(2)?.max(0) as u64,
                calls: row.get::<_, i64>(3)?.max(0) as u64,
                observed_input_tokens: input.max(0) as u64,
                observed_cache_read_tokens: read.max(0) as u64,
                cache_hit_ratio: if input <= 0 {
                    0.0
                } else {
                    (read.max(0) as f64 / input as f64).clamp(0.0, 1.0)
                },
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Prune old cache-epoch observations for one project.
    ///
    /// The newest `keep_epochs` epochs are retained, and an epoch with any
    /// observation attached to an active session is protected even when it is
    /// older than the cutoff. Deleting an observation only removes derived
    /// telemetry; a later provider call recreates it with fresh bytes.
    pub fn prune_prompt_prefixes(
        &self,
        project_id: &str,
        cutoff: &str,
        keep_epochs: u32,
        dry_run: bool,
    ) -> Result<PromptPrefixPruneOutcome> {
        let keep_epochs = keep_epochs.clamp(1, 1_000);
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let keep = {
            let mut statement = tx.prepare(
                "SELECT cache_epoch FROM prompt_prefixes
                 WHERE project_id = ?1
                 ORDER BY last_seen DESC, cache_epoch ASC
                 LIMIT ?2",
            )?;
            let rows = statement.query_map(params![project_id, i64::from(keep_epochs)], |row| {
                row.get::<_, String>(0)
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let protected = {
            let mut statement = tx.prepare(
                "SELECT DISTINCT o.cache_epoch
                 FROM prompt_prefix_observations o
                 JOIN sessions s ON s.id = o.session_id
                 WHERE o.project_id = ?1 AND o.last_seen < ?2
                   AND s.status IN ('running', 'queued', 'paused', 'waiting_input')
                 ORDER BY o.cache_epoch ASC",
            )?;
            let rows =
                statement.query_map(params![project_id, cutoff], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let keep_set = keep
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let protected_set = protected
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let total_epochs: i64 = tx.query_row(
            "SELECT COUNT(*) FROM prompt_prefixes WHERE project_id = ?1",
            params![project_id],
            |row| row.get(0),
        )?;
        let candidates = {
            let mut statement = tx.prepare(
                "SELECT cache_epoch FROM prompt_prefixes
                 WHERE project_id = ?1 AND last_seen < ?2
                 ORDER BY last_seen ASC, cache_epoch ASC",
            )?;
            let rows =
                statement.query_map(params![project_id, cutoff], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut removed_epochs = 0u64;
        let mut removed_observations = 0u64;
        for epoch in candidates {
            if keep_set.contains(&epoch) || protected_set.contains(&epoch) {
                continue;
            }
            let observations: i64 = tx.query_row(
                "SELECT COUNT(*) FROM prompt_prefix_observations
                 WHERE project_id = ?1 AND cache_epoch = ?2",
                params![project_id, epoch],
                |row| row.get(0),
            )?;
            if !dry_run {
                tx.execute(
                    "DELETE FROM prompt_prefix_observations
                     WHERE project_id = ?1 AND cache_epoch = ?2",
                    params![project_id, epoch],
                )?;
                tx.execute(
                    "DELETE FROM prompt_prefixes
                     WHERE project_id = ?1 AND cache_epoch = ?2",
                    params![project_id, epoch],
                )?;
            }
            removed_epochs = removed_epochs.saturating_add(1);
            removed_observations = removed_observations.saturating_add(observations.max(0) as u64);
        }
        tx.commit()?;
        let total_epochs = total_epochs.max(0) as u64;
        Ok(PromptPrefixPruneOutcome {
            cutoff: cutoff.to_string(),
            removed_epochs,
            removed_observations,
            kept_epochs: keep.len() as u64,
            remaining_epochs: total_epochs.saturating_sub(removed_epochs),
            protected_epochs: protected.len() as u64,
        })
    }

    pub fn prompt_prefix_totals(&self, project_id: &str) -> Result<PromptPrefixTotals> {
        let conn = self.lock()?;
        let (sessions, calls, input, read): (i64, i64, i64, i64) = conn.query_row(
            "SELECT COUNT(DISTINCT session_id), COALESCE(SUM(calls), 0),
                    COALESCE(SUM(observed_input_tokens), 0),
                    COALESCE(SUM(observed_cache_read_tokens), 0)
             FROM prompt_prefix_observations
             WHERE project_id = ?1",
            params![project_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        Ok(PromptPrefixTotals {
            total_sessions: sessions.max(0) as u64,
            total_calls: calls.max(0) as u64,
            observed_input_tokens: input.max(0) as u64,
            observed_cache_read_tokens: read.max(0) as u64,
        })
    }

    /// Replace one session's observations and recompute every aggregate row
    /// for the project from the observation table. Used by replay repair and
    /// database rebuild; rows for other sessions are preserved.
    pub fn replace_prompt_prefix_observations(
        &self,
        project_id: &str,
        session_id: &str,
        rows: &[PromptPrefixObservation],
    ) -> Result<usize> {
        for row in rows {
            if row.project_id != project_id || row.session_id != session_id {
                return Err(anyhow!(
                    "prompt-prefix observation {}:{} does not belong to {project_id}:{session_id}",
                    row.project_id,
                    row.session_id
                ));
            }
            if !cache_epoch_hex(&row.cache_epoch) {
                return Err(anyhow!(
                    "prompt-prefix observation has an invalid cache epoch"
                ));
            }
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        replace_prompt_prefix_observations_tx(&tx, project_id, session_id, rows)?;
        tx.commit()?;
        Ok(rows.len())
    }

    /// Recompute every aggregate row for a project from observations. This is
    /// the idempotent repair primitive; it never reads session configuration.
    pub fn recompute_prompt_prefix_aggregates(&self, project_id: &str) -> Result<usize> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let count = rebuild_prompt_prefix_aggregates_tx(&tx, project_id)?;
        tx.commit()?;
        Ok(count)
    }

    /// Epochs whose aggregate row does not match the observation table.
    /// Missing aggregates count as inconsistent when observations exist.
    pub fn prompt_prefix_inconsistent_epochs(
        &self,
        project_id: &str,
        epochs: &[String],
    ) -> Result<Vec<String>> {
        let conn = self.lock()?;
        let mut inconsistent = Vec::new();
        for epoch in epochs {
            let computed = computed_prompt_prefix_aggregate(&conn, project_id, epoch)?;
            let stored = conn
                .query_row(
                    "SELECT project_id, cache_epoch, model, tool_bundle, tool_bundle_hash,
                            core_hash, project_hash, core_sections, project_sections,
                            first_seen, last_seen, observed_input_tokens,
                            observed_cache_read_tokens, session_count, call_count
                     FROM prompt_prefixes
                     WHERE project_id = ?1 AND cache_epoch = ?2",
                    params![project_id, epoch],
                    map_prompt_prefix_aggregate,
                )
                .optional()?;
            let matches = match (stored, computed) {
                (Some(stored), Some(computed)) => prompt_prefix_aggregate_equal(&stored, &computed),
                (None, None) => true,
                _ => false,
            };
            if !matches {
                inconsistent.push(epoch.clone());
            }
        }
        Ok(inconsistent)
    }
}

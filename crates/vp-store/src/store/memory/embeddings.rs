use super::*;

impl Store {
    /// Projects with a configured embedding policy. Disabled-but-configured
    /// rows are returned so `memory.embeddings.status` can explain the state;
    /// the daemon reconciler skips them without any network call.
    pub fn list_memory_embedding_policies(&self) -> Result<Vec<(String, MemoryEmbeddingPolicy)>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT project_id, embedding_json FROM project_memory_policies
             WHERE embedding_json IS NOT NULL ORDER BY project_id ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?;
        let mut policies = Vec::new();
        for row in rows {
            let (project_id, raw) = row?;
            if let Some(policy) = decode_memory_embedding_policy(raw)
                && policy.is_configured()
            {
                policies.push((project_id, policy));
            }
        }
        Ok(policies)
    }

    /// Bounded candidate window for the embedding reconciler. Includes rows
    /// whose embedding is missing or belongs to another model/dimension, rows
    /// with a pending content-update job, and due failed retries. The caller
    /// compares content hashes and skips unchanged rows before any network
    /// call.
    pub fn memory_embedding_index_candidates(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        max_attempts: u32,
        limit: u32,
    ) -> Result<Vec<MemoryEmbeddingIndexCandidate>> {
        let now = now_rfc3339();
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT m.id, m.session_id, m.project_id, m.agent_id, m.agent_name, m.kind,
                    m.title, m.body, m.tags, m.created_at, m.status, m.source, m.evidence,
                    m.supersedes, m.superseded_by, m.pinned, m.use_count, m.last_used_at,
                    m.last_verified_at, m.verify_after, m.stale_reason,
                    e.model, e.dimensions, e.content_hash,
                    j.status, j.model, j.attempts, j.updated_at
             FROM memories m
             LEFT JOIN memory_embeddings e ON e.memory_id = m.id
             LEFT JOIN memory_embedding_jobs j ON j.memory_id = m.id
             WHERE m.project_id = ?1
               AND m.status IN ('active', 'stale')
               AND (
                   (
                       e.memory_id IS NULL
                       AND (
                           j.memory_id IS NULL
                           OR j.status != 'failed'
                           OR j.model != ?2
                           OR (
                               j.attempts < ?4
                               AND j.next_attempt_at IS NOT NULL
                               AND j.next_attempt_at <= ?5
                           )
                       )
                   )
                   OR e.model != ?2
                   OR e.dimensions != ?3
                   OR substr(e.vector, 1, 4) != x'5650454d'
                   OR substr(e.vector, 5, 1) != x'01'
                   OR length(e.vector) != 9 + 4 * ?3
                   OR j.status = 'pending'
                   OR (
                       j.status = 'failed'
                       AND j.attempts < ?4
                       AND j.next_attempt_at IS NOT NULL
                       AND j.next_attempt_at <= ?5
                   )
               )
             ORDER BY COALESCE(j.updated_at, m.created_at) ASC, m.id ASC
             LIMIT ?6",
        )?;
        let rows = stmt.query_map(
            params![
                project_id,
                model,
                i64::from(dimensions),
                i64::from(max_attempts),
                now,
                i64::from(limit.clamp(1, 512))
            ],
            |row| {
                Ok(MemoryEmbeddingIndexCandidate {
                    memory: map_memory_row(row)?,
                    embedded_model: row.get(21)?,
                    embedded_dimensions: row
                        .get::<_, Option<i64>>(22)?
                        .and_then(|value| u32::try_from(value).ok()),
                    embedded_content_hash: row.get(23)?,
                    job_status: row.get(24)?,
                    job_model: row.get(25)?,
                    job_attempts: row
                        .get::<_, Option<i64>>(26)?
                        .and_then(|value| u32::try_from(value).ok())
                        .unwrap_or(0),
                    job_updated_at: row.get(27)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Active/stale memories with a current-model embedding row. `limit` is
    /// clamped to 256; vector bytes are returned as-is and decoded by the
    /// caller under the documented versioned encoding.
    #[allow(clippy::too_many_arguments)]
    pub fn memory_semantic_candidates(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        include_stale: bool,
        limit: u32,
    ) -> Result<Vec<MemorySemanticCandidate>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT m.id, m.session_id, m.project_id, m.agent_id, m.agent_name, m.kind,
                    m.title, m.body, m.tags, m.created_at, m.status, m.source, m.evidence,
                    m.supersedes, m.superseded_by, m.pinned, m.use_count, m.last_used_at,
                    m.last_verified_at, m.verify_after, m.stale_reason,
                    e.model, e.dimensions, e.content_hash, e.vector
             FROM memories m
             JOIN memory_embeddings e ON e.memory_id = m.id
             WHERE m.project_id = ?1
               AND e.model = ?2
               AND e.dimensions = ?3
               AND (
                   m.status = 'active'
                   OR (?4 = 1 AND m.status = 'stale')
               )
             ORDER BY m.created_at DESC, m.id ASC
             LIMIT ?5",
        )?;
        let rows = stmt.query_map(
            params![
                project_id,
                model,
                i64::from(dimensions),
                if include_stale { 1 } else { 0 },
                i64::from(limit.clamp(1, 256))
            ],
            |row| {
                Ok(MemorySemanticCandidate {
                    memory: map_memory_row(row)?,
                    model: row.get(21)?,
                    dimensions: u32::try_from(row.get::<_, i64>(22)?).unwrap_or(u32::MAX),
                    content_hash: row.get(23)?,
                    vector: row.get(24)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Increment the attempt counter before a network call. If the row was
    /// selected because its embedding was missing, this creates the job row.
    pub fn mark_memory_embedding_attempted(
        &self,
        memory_id: &str,
        model: &str,
        reset_attempts: bool,
    ) -> Result<u32> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let now = now_rfc3339();
        tx.execute(
            "INSERT INTO memory_embedding_jobs
             (memory_id, model, status, attempts, last_error, next_attempt_at, updated_at)
             VALUES (?1, ?2, 'pending', 1, NULL, NULL, ?4)
             ON CONFLICT(memory_id) DO UPDATE SET
                model = excluded.model,
                status = 'pending',
                attempts = CASE WHEN ?3 = 1
                    THEN 1
                    ELSE memory_embedding_jobs.attempts + 1
                END,
                last_error = NULL,
                next_attempt_at = NULL,
                updated_at = excluded.updated_at",
            params![memory_id, model, if reset_attempts { 1 } else { 0 }, now],
        )?;
        let attempts: i64 = tx.query_row(
            "SELECT attempts FROM memory_embedding_jobs WHERE memory_id = ?1",
            params![memory_id],
            |row| row.get(0),
        )?;
        tx.commit()?;
        Ok(u32::try_from(attempts).unwrap_or(u32::MAX))
    }

    /// Persist one successful vector plus its job transition and audit event.
    /// `vector` must already be validated and encoded by `vp-core`.
    #[allow(clippy::too_many_arguments)]
    pub fn record_memory_embedding_success(
        &self,
        memory_id: &str,
        model: &str,
        dimensions: u32,
        content_hash: &str,
        vector: &[u8],
    ) -> Result<Option<Event>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(memory) = get_memory_tx(&tx, memory_id)? else {
            tx.commit()?;
            return Ok(None);
        };
        let now = now_rfc3339();
        tx.execute(
            "INSERT INTO memory_embeddings
             (memory_id, model, dimensions, content_hash, vector, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
             ON CONFLICT(memory_id) DO UPDATE SET
                model = excluded.model,
                dimensions = excluded.dimensions,
                content_hash = excluded.content_hash,
                vector = excluded.vector,
                updated_at = excluded.updated_at",
            params![
                memory_id,
                model,
                i64::from(dimensions),
                content_hash,
                vector,
                now
            ],
        )?;
        // The vector may have changed cluster membership. Removing the old
        // assignment keeps recall from scoring a stale cluster until the
        // bounded maintenance pass assigns the new vector.
        tx.execute(
            "DELETE FROM memory_embedding_assignments WHERE memory_id = ?1",
            params![memory_id],
        )?;
        tx.execute(
            "INSERT INTO memory_embedding_jobs
             (memory_id, model, status, attempts, last_error, next_attempt_at, updated_at)
             VALUES (?1, ?2, 'indexed', 0, NULL, NULL, ?3)
             ON CONFLICT(memory_id) DO UPDATE SET
                model = excluded.model,
                status = 'indexed',
                attempts = 0,
                last_error = NULL,
                next_attempt_at = NULL,
                updated_at = excluded.updated_at",
            params![memory_id, model, now],
        )?;
        let event = insert_event_tx(
            &tx,
            &memory.session_id,
            "memory.embedding.indexed",
            serde_json::json!({
                "memory_id": memory.id,
                "project_id": memory.project_id,
                "model": model,
                "dimensions": dimensions,
                "content_hash": content_hash,
            }),
        )?;
        tx.commit()?;
        drop(conn);
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some(event))
    }

    /// Record a failed attempt. `next_attempt_at = None` marks the retry budget
    /// exhausted; the row stays visible as `failed` until an explicit rebuild.
    pub fn record_memory_embedding_failure(
        &self,
        memory_id: &str,
        model: &str,
        attempts: u32,
        error: &str,
        next_attempt_at: Option<&str>,
    ) -> Result<Option<Event>> {
        let error = truncate_chars(error.trim(), 2_000);
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(memory) = get_memory_tx(&tx, memory_id)? else {
            tx.commit()?;
            return Ok(None);
        };
        let now = now_rfc3339();
        tx.execute(
            "INSERT INTO memory_embedding_jobs
             (memory_id, model, status, attempts, last_error, next_attempt_at, updated_at)
             VALUES (?1, ?2, 'failed', ?3, ?4, ?5, ?6)
             ON CONFLICT(memory_id) DO UPDATE SET
                model = excluded.model,
                status = 'failed',
                attempts = excluded.attempts,
                last_error = excluded.last_error,
                next_attempt_at = excluded.next_attempt_at,
                updated_at = excluded.updated_at",
            params![
                memory_id,
                model,
                i64::from(attempts),
                error,
                next_attempt_at,
                now
            ],
        )?;
        let event = insert_event_tx(
            &tx,
            &memory.session_id,
            "memory.embedding.failed",
            serde_json::json!({
                "memory_id": memory.id,
                "project_id": memory.project_id,
                "model": model,
                "attempts": attempts,
                "error": error,
                "next_attempt_at": next_attempt_at,
            }),
        )?;
        tx.commit()?;
        drop(conn);
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some(event))
    }

    /// Mark a job skipped without a network call: unchanged content,
    /// archived/superseded memory, or an explicitly disabled lane.
    pub fn record_memory_embedding_skip(
        &self,
        memory_id: &str,
        model: &str,
        reason: &str,
    ) -> Result<Option<Event>> {
        let reason = truncate_chars(reason.trim(), 500);
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(memory) = get_memory_tx(&tx, memory_id)? else {
            tx.commit()?;
            return Ok(None);
        };
        let now = now_rfc3339();
        tx.execute(
            "INSERT INTO memory_embedding_jobs
             (memory_id, model, status, attempts, last_error, next_attempt_at, updated_at)
             VALUES (?1, ?2, 'skipped', 0, NULL, NULL, ?3)
             ON CONFLICT(memory_id) DO UPDATE SET
                model = excluded.model,
                status = 'skipped',
                attempts = 0,
                last_error = NULL,
                next_attempt_at = NULL,
                updated_at = excluded.updated_at",
            params![memory_id, model, now],
        )?;
        let event = insert_event_tx(
            &tx,
            &memory.session_id,
            "memory.embedding.skipped",
            serde_json::json!({
                "memory_id": memory.id,
                "project_id": memory.project_id,
                "model": model,
                "reason": reason,
            }),
        )?;
        tx.commit()?;
        drop(conn);
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some(event))
    }

    /// Aggregate call/token accounting. This table is intentionally separate
    /// from `usage_ledger`: embedding calls are project infrastructure, not a
    /// session's task spend, and are never charged to a session budget.
    #[allow(clippy::too_many_arguments)]
    pub fn record_memory_embedding_usage(
        &self,
        project_id: &str,
        model: &str,
        calls: u64,
        input_tokens: u64,
        estimated_tokens: u64,
        bytes: u64,
        provider_reported: bool,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO memory_embedding_usage
             (id, project_id, model, calls, input_tokens, estimated_tokens, bytes,
              provider_reported, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                Uuid::now_v7().to_string(),
                project_id,
                model,
                i64::try_from(calls).unwrap_or(i64::MAX),
                i64::try_from(input_tokens).unwrap_or(i64::MAX),
                i64::try_from(estimated_tokens).unwrap_or(i64::MAX),
                i64::try_from(bytes).unwrap_or(i64::MAX),
                if provider_reported { 1 } else { 0 },
                now_rfc3339()
            ],
        )?;
        Ok(())
    }
}

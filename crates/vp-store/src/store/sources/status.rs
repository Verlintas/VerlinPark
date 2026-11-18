use super::*;

impl Store {
    /// Separate source embedding accounting. This is project infrastructure,
    /// not session task spend, and is never charged to a session budget.
    #[allow(clippy::too_many_arguments)]
    pub fn record_source_embedding_usage(
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
            "INSERT INTO source_embedding_usage
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

    pub fn source_embedding_integrity(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
    ) -> Result<SourceEmbeddingIntegrity> {
        let conn = self.lock()?;
        let structurally_valid = "substr(v.vector, 1, 4) = x'5650454d'
             AND substr(v.vector, 5, 1) = x'01'
             AND length(v.vector) = 9 + 4 * ?3";
        let valid: i64 = conn.query_row(
            &format!(
                "SELECT COUNT(*)
                 FROM source_embeddings v
                 JOIN source_chunks c ON c.id = v.chunk_id
                 JOIN sources s ON s.id = v.source_id
                 JOIN source_embedding_state st ON st.source_id = v.source_id
                 WHERE v.project_id = ?1 AND v.model = ?2 AND v.dimensions = ?3
                   AND s.status = 'indexed'
                   AND v.status = 'current' AND v.generation = st.generation
                   AND {structurally_valid}"
            ),
            params![project_id, model, i64::from(dimensions)],
            |row| row.get(0),
        )?;
        let invalid: i64 = conn.query_row(
            &format!(
                "SELECT COUNT(*)
                 FROM source_embeddings v
                 JOIN sources s ON s.id = v.source_id
                 JOIN source_embedding_state st ON st.source_id = v.source_id
                 WHERE v.project_id = ?1 AND v.model = ?2 AND v.dimensions = ?3
                   AND s.status = 'indexed'
                   AND v.status = 'current' AND v.generation = st.generation
                   AND NOT ({structurally_valid})"
            ),
            params![project_id, model, i64::from(dimensions)],
            |row| row.get(0),
        )?;
        let stale: i64 = conn.query_row(
            "SELECT COUNT(*)
             FROM source_embeddings v
             JOIN source_embedding_state st ON st.source_id = v.source_id
             WHERE v.project_id = ?1
               AND (v.status = 'stale' OR v.generation != st.generation)",
            params![project_id],
            |row| row.get(0),
        )?;
        let orphan: i64 = conn.query_row(
            "SELECT COUNT(*)
             FROM source_embeddings v
             LEFT JOIN source_chunks c ON c.id = v.chunk_id
             WHERE v.project_id = ?1 AND c.id IS NULL",
            params![project_id],
            |row| row.get(0),
        )?;
        let generation_mismatched: i64 = conn.query_row(
            "SELECT COUNT(*)
             FROM source_embeddings v
             JOIN source_embedding_state st ON st.source_id = v.source_id
             WHERE v.project_id = ?1 AND v.generation != st.generation",
            params![project_id],
            |row| row.get(0),
        )?;
        Ok(SourceEmbeddingIntegrity {
            valid_vectors: u32::try_from(valid).unwrap_or(u32::MAX),
            invalid_vectors: u32::try_from(invalid).unwrap_or(u32::MAX),
            stale_vectors: u32::try_from(stale).unwrap_or(u32::MAX),
            orphan_vectors: u32::try_from(orphan).unwrap_or(u32::MAX),
            generation_mismatched: u32::try_from(generation_mismatched).unwrap_or(u32::MAX),
        })
    }

    /// Health snapshot for the source semantic lane. Counts are bounded SQL
    /// aggregates; recent jobs are capped at 20 and no vector, embedded text,
    /// endpoint value, or credential is included.
    pub fn source_embedding_status(
        &self,
        project_id: &str,
        policy: Option<&SourceEmbeddingPolicy>,
    ) -> Result<SourceEmbeddingStatusResult> {
        let enabled = policy.is_some_and(|policy| policy.enabled);
        let configured = policy.is_some_and(SourceEmbeddingPolicy::is_configured);
        let model = policy.and_then(|policy| policy.model.clone());
        let dimensions = policy.map_or(1536, |policy| policy.dimensions);
        let batch_size = policy.map_or(8, |policy| policy.batch_size);
        let max_chunks = policy.map_or(200, |policy| policy.max_chunks_per_source);
        let retention_days = policy.map_or(30, |policy| policy.retention_days);
        let endpoint_env = policy.and_then(SourceEmbeddingPolicy::endpoint_env_name);
        let api_key_env = policy.and_then(SourceEmbeddingPolicy::api_key_env_name);
        let conn = self.lock()?;
        let indexable: i64 = conn.query_row(
            "SELECT COUNT(*)
             FROM source_chunks c
             JOIN sources s ON s.id = c.source_id
             WHERE c.project_id = ?1 AND s.status = 'indexed' AND c.chunk_no <= ?2",
            params![project_id, i64::from(max_chunks.clamp(1, 500))],
            |row| row.get(0),
        )?;
        let model_for_counts = model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty());
        let (indexed, invalid) = match model_for_counts {
            Some(model) => conn.query_row(
                "SELECT
                     COALESCE(SUM(CASE WHEN
                         v.status = 'current' AND v.generation = st.generation
                         AND substr(v.vector, 1, 4) = x'5650454d'
                         AND substr(v.vector, 5, 1) = x'01'
                         AND length(v.vector) = 9 + 4 * ?3
                     THEN 1 ELSE 0 END), 0),
                     COALESCE(SUM(CASE WHEN
                         v.status = 'current' AND v.generation = st.generation
                         AND (
                             substr(v.vector, 1, 4) != x'5650454d'
                             OR substr(v.vector, 5, 1) != x'01'
                             OR length(v.vector) != 9 + 4 * ?3
                         )
                     THEN 1 ELSE 0 END), 0)
                 FROM source_embeddings v
                 JOIN source_chunks c ON c.id = v.chunk_id
                 JOIN sources s ON s.id = v.source_id
                 JOIN source_embedding_state st ON st.source_id = v.source_id
                 WHERE v.project_id = ?1
                   AND s.status = 'indexed'
                   AND c.chunk_no <= ?4
                   AND v.model = ?2
                   AND v.dimensions = ?3",
                params![
                    project_id,
                    model,
                    i64::from(dimensions),
                    i64::from(max_chunks.clamp(1, 500))
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?,
            None => (0, 0),
        };
        let stale: i64 = conn.query_row(
            "SELECT COUNT(*)
             FROM source_embeddings v
             JOIN source_embedding_state st ON st.source_id = v.source_id
             WHERE v.project_id = ?1
               AND (v.status = 'stale' OR v.generation != st.generation)",
            params![project_id],
            |row| row.get(0),
        )?;
        let count_jobs = |status: &str| -> Result<u32> {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM source_embedding_jobs
                 WHERE project_id = ?1 AND status = ?2
                   AND (?3 IS NULL OR model = ?3)",
                params![project_id, status, model_for_counts],
                |row| row.get(0),
            )?;
            Ok(u32::try_from(count).unwrap_or(u32::MAX))
        };
        let pending_jobs = count_jobs("pending")?;
        let running_jobs = count_jobs("running")?;
        let failed_jobs = count_jobs("failed")?;
        let dead_jobs = count_jobs("dead")?;
        let done_jobs = count_jobs("done")?;
        let last_error: Option<String> = conn
            .query_row(
                "SELECT last_error FROM source_embedding_jobs
                 WHERE project_id = ?1 AND status IN ('failed', 'dead')
                   AND last_error IS NOT NULL
                   AND (?2 IS NULL OR model = ?2)
                 ORDER BY updated_at DESC, source_id ASC LIMIT 1",
                params![project_id, model_for_counts],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let usage = conn.query_row(
            "SELECT COALESCE(SUM(calls), 0), COALESCE(SUM(input_tokens), 0),
                    COALESCE(SUM(estimated_tokens), 0), COALESCE(SUM(bytes), 0),
                    COALESCE(SUM(provider_reported), 0)
             FROM source_embedding_usage WHERE project_id = ?1",
            params![project_id],
            |row| {
                Ok(SourceEmbeddingUsage {
                    calls: u64::try_from(row.get::<_, i64>(0)?).unwrap_or(u64::MAX),
                    input_tokens: u64::try_from(row.get::<_, i64>(1)?).unwrap_or(u64::MAX),
                    estimated_tokens: u64::try_from(row.get::<_, i64>(2)?).unwrap_or(u64::MAX),
                    bytes: u64::try_from(row.get::<_, i64>(3)?).unwrap_or(u64::MAX),
                    provider_reported_calls: u64::try_from(row.get::<_, i64>(4)?)
                        .unwrap_or(u64::MAX),
                })
            },
        )?;
        let mut stmt = conn.prepare(
            "SELECT j.source_id, s.title, j.status, j.attempts, j.generation,
                    j.chunks_total, j.chunks_done, j.last_error, j.next_attempt_at,
                    j.updated_at
             FROM source_embedding_jobs j
             JOIN sources s ON s.id = j.source_id
             WHERE j.project_id = ?1
               AND (?2 IS NULL OR j.model = ?2)
             ORDER BY j.updated_at DESC, j.source_id ASC
             LIMIT 20",
        )?;
        let recent_jobs = stmt
            .query_map(params![project_id, model_for_counts], |row| {
                Ok(SourceEmbeddingJobStatus {
                    source_id: row.get(0)?,
                    source_title: row.get(1)?,
                    status: row.get(2)?,
                    attempts: u32::try_from(row.get::<_, i64>(3)?).unwrap_or(u32::MAX),
                    generation: u64::try_from(row.get::<_, i64>(4)?).unwrap_or(0),
                    chunks_total: u32::try_from(row.get::<_, i64>(5)?).unwrap_or(u32::MAX),
                    chunks_done: u32::try_from(row.get::<_, i64>(6)?).unwrap_or(u32::MAX),
                    last_error: row.get(7)?,
                    next_attempt_at: row.get(8)?,
                    updated_at: row.get(9)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        drop(conn);
        let integrity = self.source_embedding_integrity(
            project_id,
            model_for_counts.unwrap_or(""),
            dimensions,
        )?;
        let indexable_chunks = u32::try_from(indexable).unwrap_or(u32::MAX);
        let indexed_chunks = u32::try_from(indexed).unwrap_or(u32::MAX);
        let invalid_chunks = u32::try_from(invalid).unwrap_or(u32::MAX);
        Ok(SourceEmbeddingStatusResult {
            project_id: project_id.to_string(),
            enabled,
            configured,
            endpoint_env: endpoint_env.map(str::to_string),
            endpoint_env_present: false,
            api_key_env: api_key_env.map(str::to_string),
            api_key_env_present: false,
            model,
            dimensions,
            batch_size,
            max_chunks_per_source: max_chunks,
            retention_days,
            indexable_chunks,
            indexed_chunks,
            stale_chunks: u32::try_from(stale).unwrap_or(u32::MAX),
            invalid_chunks,
            pending_jobs,
            running_jobs,
            failed_jobs,
            dead_jobs,
            done_jobs,
            coverage: if indexable_chunks == 0 {
                0.0
            } else {
                f64::from(indexed_chunks) / f64::from(indexable_chunks)
            },
            semantic_unavailable: None,
            last_error: last_error.map(|error| truncate_chars(&error, 2_000)),
            usage,
            recent_jobs,
            integrity,
        })
    }

    /// Remove every derived source vector, job, and generation row for a
    /// project. Usage history is accounting, not derived content, and is kept.
    pub fn clear_source_embeddings(&self, project_id: &str) -> Result<(u32, u32)> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let vectors = tx.execute(
            "DELETE FROM source_embeddings WHERE project_id = ?1",
            params![project_id],
        )?;
        let jobs = tx.execute(
            "DELETE FROM source_embedding_jobs WHERE project_id = ?1",
            params![project_id],
        )?;
        tx.execute(
            "DELETE FROM source_embedding_state WHERE project_id = ?1",
            params![project_id],
        )?;
        // The IVF index is derived from these vectors; clear it in the same
        // transaction so a later search cannot probe a stale generation.
        tx.execute(
            "DELETE FROM source_ann_assignments WHERE project_id = ?1",
            params![project_id],
        )?;
        tx.execute(
            "DELETE FROM source_ann_index WHERE project_id = ?1",
            params![project_id],
        )?;
        tx.commit()?;
        Ok((
            u32::try_from(vectors).unwrap_or(u32::MAX),
            u32::try_from(jobs).unwrap_or(u32::MAX),
        ))
    }

    /// Queue every indexed source for a fresh generation. Explicit operator
    /// action only; enabling a policy does not call this automatically.
    pub fn enqueue_source_embedding_backlog(
        &self,
        project_id: &str,
        model: &str,
        max_chunks: u32,
    ) -> Result<u32> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let mut stmt = tx.prepare(
            "SELECT id, chunk_count FROM sources
             WHERE project_id = ?1 AND status = 'indexed'
             ORDER BY id ASC LIMIT 1000",
        )?;
        let rows = stmt
            .query_map(params![project_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    u32::try_from(row.get::<_, i64>(1)?).unwrap_or(u32::MAX),
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        let mut enqueued = 0u32;
        for (source_id, chunk_count) in rows {
            begin_source_embedding_generation_tx(
                &tx,
                &source_id,
                project_id,
                chunk_count as usize,
                Some((model, max_chunks)),
            )?;
            enqueued = enqueued.saturating_add(1);
        }
        tx.commit()?;
        Ok(enqueued)
    }

    /// Retention sweep for derived source rows. Zero retention disables the
    /// sweep. Usage history is never pruned here.
    pub fn sweep_source_embedding_retention(
        &self,
        project_id: &str,
        retention_days: u32,
    ) -> Result<(u32, u32)> {
        if retention_days == 0 {
            return Ok((0, 0));
        }
        let cutoff = (OffsetDateTime::now_utc()
            - time::Duration::days(i64::from(retention_days.min(3_650))))
        .format(&Rfc3339)
        .unwrap_or_else(|_| now_rfc3339());
        let conn = self.lock()?;
        let vectors = conn.execute(
            "DELETE FROM source_embeddings
             WHERE project_id = ?1
               AND updated_at < ?2
               AND (
                    status = 'stale'
                    OR NOT EXISTS (SELECT 1 FROM source_chunks c WHERE c.id = chunk_id)
                    OR generation < COALESCE(
                        (SELECT generation FROM source_embedding_state st
                         WHERE st.source_id = source_embeddings.source_id), 1)
               )",
            params![project_id, cutoff],
        )?;
        let jobs = conn.execute(
            "DELETE FROM source_embedding_jobs
             WHERE project_id = ?1 AND status = 'dead' AND updated_at < ?2",
            params![project_id, cutoff],
        )?;
        Ok((
            u32::try_from(vectors).unwrap_or(u32::MAX),
            u32::try_from(jobs).unwrap_or(u32::MAX),
        ))
    }

    pub(super) fn source_dir(&self, project_id: &str) -> PathBuf {
        self.data_dir.join("sources").join(project_id)
    }
}

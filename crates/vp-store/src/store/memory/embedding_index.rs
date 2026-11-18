use super::*;

impl Store {
    /// Health snapshot for the semantic lane. Counts are bounded SQL
    /// aggregates; recent jobs are capped at 20, and no vector or credential
    /// is included.
    pub fn memory_embedding_status(
        &self,
        project_id: &str,
        policy: Option<&MemoryEmbeddingPolicy>,
    ) -> Result<MemoryEmbeddingsStatusResult> {
        let enabled = policy.is_some_and(|policy| policy.enabled);
        let configured = policy.is_some_and(MemoryEmbeddingPolicy::is_configured);
        let model = policy.and_then(|policy| policy.model.clone());
        let dimensions = policy.map_or(1536, |policy| policy.dimensions);
        let conn = self.lock()?;
        let indexable: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memories
             WHERE project_id = ?1 AND status IN ('active', 'stale')",
            params![project_id],
            |row| row.get(0),
        )?;
        let (indexed, invalid): (i64, i64) = match model.as_deref() {
            Some(model) => conn.query_row(
                "SELECT
                     COALESCE(SUM(CASE WHEN
                         substr(e.vector, 1, 4) = x'5650454d'
                         AND substr(e.vector, 5, 1) = x'01'
                         AND length(e.vector) = 9 + 4 * ?3
                     THEN 1 ELSE 0 END), 0),
                     COALESCE(SUM(CASE WHEN
                         substr(e.vector, 1, 4) != x'5650454d'
                         OR substr(e.vector, 5, 1) != x'01'
                         OR length(e.vector) != 9 + 4 * ?3
                     THEN 1 ELSE 0 END), 0)
                 FROM memories m
                 JOIN memory_embeddings e ON e.memory_id = m.id
                 LEFT JOIN memory_embedding_jobs j ON j.memory_id = m.id
                 WHERE m.project_id = ?1
                   AND m.status IN ('active', 'stale')
                   AND e.model = ?2 AND e.dimensions = ?3
                   AND (j.status IS NULL OR j.status NOT IN ('pending', 'failed'))",
                params![project_id, model, i64::from(dimensions)],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?,
            None => (0, 0),
        };
        let count_jobs = |status: &str| -> Result<u32> {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM memory_embedding_jobs j
                 JOIN memories m ON m.id = j.memory_id
                 WHERE m.project_id = ?1 AND j.status = ?2
                   AND (?3 IS NULL OR j.model = ?3)",
                params![project_id, status, model.as_deref()],
                |row| row.get(0),
            )?;
            Ok(u32::try_from(count).unwrap_or(u32::MAX))
        };
        let pending = count_jobs("pending")?;
        let failed = count_jobs("failed")?;
        let skipped = count_jobs("skipped")?;
        let last_error: Option<String> = conn
            .query_row(
                "SELECT j.last_error FROM memory_embedding_jobs j
                 JOIN memories m ON m.id = j.memory_id
                 WHERE m.project_id = ?1 AND j.status = 'failed' AND j.last_error IS NOT NULL
                   AND (?2 IS NULL OR j.model = ?2)
                 ORDER BY j.updated_at DESC, j.memory_id ASC LIMIT 1",
                params![project_id, model.as_deref()],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let usage = conn.query_row(
            "SELECT COALESCE(SUM(calls), 0), COALESCE(SUM(input_tokens), 0),
                    COALESCE(SUM(estimated_tokens), 0), COALESCE(SUM(bytes), 0),
                    COALESCE(SUM(provider_reported), 0)
             FROM memory_embedding_usage WHERE project_id = ?1",
            params![project_id],
            |row| {
                Ok(MemoryEmbeddingUsage {
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
            "SELECT m.id, m.title, j.status, j.attempts, j.last_error, j.next_attempt_at,
                    j.updated_at
             FROM memory_embedding_jobs j
             JOIN memories m ON m.id = j.memory_id
             WHERE m.project_id = ?1
               AND (?2 IS NULL OR j.model = ?2)
             ORDER BY j.updated_at DESC, m.id ASC
             LIMIT 20",
        )?;
        let recent = stmt
            .query_map(params![project_id, model.as_deref()], |row| {
                Ok(MemoryEmbeddingJobStatus {
                    memory_id: row.get(0)?,
                    title: row.get(1)?,
                    status: row.get(2)?,
                    attempts: u32::try_from(row.get::<_, i64>(3)?).unwrap_or(u32::MAX),
                    last_error: row.get(4)?,
                    next_attempt_at: row.get(5)?,
                    updated_at: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let indexable = u32::try_from(indexable).unwrap_or(u32::MAX);
        let indexed = u32::try_from(indexed).unwrap_or(u32::MAX);
        let invalid = u32::try_from(invalid).unwrap_or(u32::MAX);
        Ok(MemoryEmbeddingsStatusResult {
            project_id: project_id.to_string(),
            enabled,
            configured,
            model,
            dimensions,
            indexable,
            indexed,
            invalid,
            pending,
            failed,
            skipped,
            coverage: if indexable == 0 {
                0.0
            } else {
                f64::from(indexed) / f64::from(indexable)
            },
            last_error: last_error.map(|error| truncate_chars(&error, 2_000)),
            usage,
            recent_jobs: recent,
            index: None,
        })
    }

    /// Remove every derived embedding and job row for a project. Usage history
    /// is accounting, not derived content, and is intentionally retained.
    pub fn clear_memory_embeddings(&self, project_id: &str) -> Result<u32> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let embeddings = tx.execute(
            "DELETE FROM memory_embeddings
             WHERE memory_id IN (SELECT id FROM memories WHERE project_id = ?1)",
            params![project_id],
        )?;
        let jobs = tx.execute(
            "DELETE FROM memory_embedding_jobs
             WHERE memory_id IN (SELECT id FROM memories WHERE project_id = ?1)",
            params![project_id],
        )?;
        // The ANN index is derived state too. The explicit rebuild/clear path
        // is also the privacy cleanup path, so it must remove centroids and
        // assignments along with vectors and jobs.
        let assignments = tx.execute(
            "DELETE FROM memory_embedding_assignments
             WHERE memory_id IN (SELECT id FROM memories WHERE project_id = ?1)",
            params![project_id],
        )?;
        let index = tx.execute(
            "DELETE FROM memory_embedding_index WHERE project_id = ?1",
            params![project_id],
        )?;
        tx.commit()?;
        Ok(u32::try_from(
            embeddings
                .saturating_add(jobs)
                .saturating_add(assignments)
                .saturating_add(index),
        )
        .unwrap_or(u32::MAX))
    }

    /// Queue every active/stale memory for indexing. Existing jobs are reset
    /// so an explicit rebuild clears exhausted retry budgets.
    pub fn enqueue_memory_embedding_backlog(&self, project_id: &str, model: &str) -> Result<u32> {
        let conn = self.lock()?;
        let now = now_rfc3339();
        let changed = conn.execute(
            "INSERT INTO memory_embedding_jobs
             (memory_id, model, status, attempts, last_error, next_attempt_at, updated_at)
             SELECT id, ?2, 'pending', 0, NULL, NULL, ?3
             FROM memories
             WHERE project_id = ?1 AND status IN ('active', 'stale') AND true
             ON CONFLICT(memory_id) DO UPDATE SET
                model = excluded.model,
                status = 'pending',
                attempts = 0,
                last_error = NULL,
                next_attempt_at = NULL,
                updated_at = excluded.updated_at",
            params![project_id, model, now],
        )?;
        Ok(u32::try_from(changed).unwrap_or(u32::MAX))
    }

    /// Current derived IVF index row for one project. `None` means no index
    /// has ever been built (or it was explicitly removed). The centroid blob
    /// is returned only to `vp-core`, which owns validation and decoding; it
    /// must never be copied into an RPC, event, log, or UI payload.
    pub fn memory_embedding_index_row(
        &self,
        project_id: &str,
    ) -> Result<Option<MemoryEmbeddingIndexRow>> {
        let conn = self.lock()?;
        let row = conn
            .query_row(
                "SELECT project_id, model, dimensions, generation, nlist, cluster_count,
                        vector_count, centroids, updated_at, last_error, fallback_count,
                        last_fallback_at, last_fallback_reason
                 FROM memory_embedding_index WHERE project_id = ?1",
                params![project_id],
                |row| {
                    Ok(MemoryEmbeddingIndexRow {
                        project_id: row.get(0)?,
                        model: row.get(1)?,
                        dimensions: u32::try_from(row.get::<_, i64>(2)?).unwrap_or(u32::MAX),
                        generation: u64::try_from(row.get::<_, i64>(3)?).unwrap_or(0),
                        nlist: u32::try_from(row.get::<_, i64>(4)?).unwrap_or(u32::MAX),
                        cluster_count: u32::try_from(row.get::<_, i64>(5)?).unwrap_or(u32::MAX),
                        vector_count: u32::try_from(row.get::<_, i64>(6)?).unwrap_or(u32::MAX),
                        centroids: row.get(7)?,
                        updated_at: row.get(8)?,
                        last_error: row.get(9)?,
                        fallback_count: u64::try_from(row.get::<_, i64>(10)?).unwrap_or(u64::MAX),
                        last_fallback_at: row.get(11)?,
                        last_fallback_reason: row.get(12)?,
                    })
                },
            )
            .optional()?;
        if let Some(row) = &row
            && row.centroids.len() > MAX_MEMORY_INDEX_BLOB_BYTES
        {
            anyhow::bail!(
                "stored memory embedding index exceeds the {MAX_MEMORY_INDEX_BLOB_BYTES}-byte ceiling"
            );
        }
        Ok(row)
    }

    /// Live vector and assignment counts. A vector counts only when the
    /// memory is active/stale, the embedding belongs to the requested
    /// model/dimensions, and its header passes the same structural checks as
    /// status. Assignments are counted only for the requested generation.
    pub fn memory_embedding_index_counts(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        generation: u64,
        cluster_count: u32,
    ) -> Result<MemoryEmbeddingIndexCounts> {
        let conn = self.lock()?;
        let (vectors, assigned): (i64, i64) = conn.query_row(
            "SELECT
                 (SELECT COUNT(*)
                    FROM memories m
                    JOIN memory_embeddings e ON e.memory_id = m.id
                   WHERE m.project_id = ?1
                     AND m.status IN ('active', 'stale')
                     AND e.model = ?2
                     AND e.dimensions = ?3
                     AND substr(e.vector, 1, 4) = x'5650454d'
                     AND substr(e.vector, 5, 1) = x'01'
                     AND length(e.vector) = 9 + 4 * ?3),
                 (SELECT COUNT(*)
                    FROM memory_embedding_assignments a
                    JOIN memories m ON m.id = a.memory_id
                    JOIN memory_embeddings e ON e.memory_id = m.id
                   WHERE m.project_id = ?1
                     AND a.generation = ?4
                     AND a.cluster >= 0
                     AND a.cluster < ?5
                     AND m.status IN ('active', 'stale')
                     AND e.model = ?2
                     AND e.dimensions = ?3
                     AND substr(e.vector, 1, 4) = x'5650454d'
                     AND substr(e.vector, 5, 1) = x'01'
                     AND length(e.vector) = 9 + 4 * ?3)",
            params![
                project_id,
                model,
                i64::from(dimensions),
                i64::try_from(generation).unwrap_or(i64::MAX),
                i64::from(cluster_count)
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(MemoryEmbeddingIndexCounts {
            vector_count: u32::try_from(vectors).unwrap_or(u32::MAX),
            assigned_vectors: u32::try_from(assigned).unwrap_or(u32::MAX),
        })
    }

    /// Audit assignments against one generation, the current model/dimensions,
    /// and the centroid count. Counts are bounded SQL aggregates; no vector is
    /// read.
    pub fn memory_embedding_assignment_audit(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        generation: u64,
        cluster_count: u32,
    ) -> Result<MemoryEmbeddingAssignmentAudit> {
        let conn = self.lock()?;
        let (total, stale, out_of_range, missing): (i64, i64, i64, i64) = conn.query_row(
            "SELECT
                 COUNT(*),
                 COALESCE(SUM(CASE WHEN a.generation != ?4 THEN 1 ELSE 0 END), 0),
                 COALESCE(SUM(CASE WHEN a.cluster < 0 OR a.cluster >= ?5 THEN 1 ELSE 0 END), 0),
                 COALESCE(SUM(CASE WHEN a.generation = ?4
                                       AND a.cluster >= 0 AND a.cluster < ?5
                                       AND (e.memory_id IS NULL
                                            OR e.model != ?2
                                            OR e.dimensions != ?3
                                            OR substr(e.vector, 1, 4) != x'5650454d'
                                            OR substr(e.vector, 5, 1) != x'01'
                                            OR length(e.vector) != 9 + 4 * ?3
                                            OR m.status NOT IN ('active', 'stale'))
                                  THEN 1 ELSE 0 END), 0)
             FROM memory_embedding_assignments a
             JOIN memories m ON m.id = a.memory_id
             LEFT JOIN memory_embeddings e ON e.memory_id = a.memory_id
             WHERE m.project_id = ?1",
            params![
                project_id,
                model,
                i64::from(dimensions),
                i64::try_from(generation).unwrap_or(i64::MAX),
                i64::from(cluster_count)
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        Ok(MemoryEmbeddingAssignmentAudit {
            total: u32::try_from(total).unwrap_or(u32::MAX),
            stale_generation: u32::try_from(stale).unwrap_or(u32::MAX),
            out_of_range: u32::try_from(out_of_range).unwrap_or(u32::MAX),
            missing_vector: u32::try_from(missing).unwrap_or(u32::MAX),
        })
    }

    /// Delete assignments that are not usable by the current generation. This
    /// is bounded by the number of assignment rows for one project and is only
    /// called from maintenance/rebuild, never from recall.
    pub fn delete_invalid_memory_embedding_assignments(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        generation: u64,
        cluster_count: u32,
    ) -> Result<u32> {
        let conn = self.lock()?;
        let deleted = conn.execute(
            "DELETE FROM memory_embedding_assignments
             WHERE memory_id IN (
                 SELECT a.memory_id
                 FROM memory_embedding_assignments a
                 JOIN memories m ON m.id = a.memory_id
                 LEFT JOIN memory_embeddings e ON e.memory_id = a.memory_id
                 WHERE m.project_id = ?1
                   AND (a.generation != ?4
                        OR a.cluster < 0
                        OR a.cluster >= ?5
                        OR e.memory_id IS NULL
                        OR e.model != ?2
                        OR e.dimensions != ?3
                        OR substr(e.vector, 1, 4) != x'5650454d'
                        OR substr(e.vector, 5, 1) != x'01'
                        OR length(e.vector) != 9 + 4 * ?3
                        OR m.status NOT IN ('active', 'stale'))
             )",
            params![
                project_id,
                model,
                i64::from(dimensions),
                i64::try_from(generation).unwrap_or(i64::MAX),
                i64::from(cluster_count)
            ],
        )?;
        Ok(u32::try_from(deleted).unwrap_or(u32::MAX))
    }

    /// Deterministic bounded sample of current vectors for a k-means rebuild.
    ///
    /// Rows are ordered by a prefix of their SHA-256 content hash, so the
    /// sample is stable for an unchanged database and does not simply take the
    /// first memory ids. `limit` is additionally capped by `max_bytes`, which
    /// bounds the transient allocation regardless of embedding dimension.
    pub fn memory_embedding_vectors_for_index(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        limit: u32,
        max_bytes: usize,
    ) -> Result<Vec<MemorySemanticCandidate>> {
        let row_bytes = 9usize.saturating_add(dimensions as usize * 4).max(1);
        let rows_by_bytes = max_bytes / row_bytes;
        let effective_limit = (limit as usize).min(rows_by_bytes.max(1)).clamp(1, 16_384);
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
               AND m.status IN ('active', 'stale')
               AND e.model = ?2
               AND e.dimensions = ?3
               AND substr(e.vector, 1, 4) = x'5650454d'
               AND substr(e.vector, 5, 1) = x'01'
               AND length(e.vector) = 9 + 4 * ?3
             ORDER BY substr(e.content_hash, 1, 16) ASC, m.id ASC
             LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![
                project_id,
                model,
                i64::from(dimensions),
                i64::try_from(effective_limit).unwrap_or(i64::MAX)
            ],
            map_memory_semantic_candidate,
        )?;
        let mut candidates = Vec::new();
        let mut bytes = 0usize;
        for row in rows {
            let candidate = row?;
            bytes = bytes.saturating_add(candidate.vector.len());
            if bytes > max_bytes && !candidates.is_empty() {
                break;
            }
            candidates.push(candidate);
        }
        Ok(candidates)
    }

    /// Current vectors that have no assignment, or whose assignment belongs
    /// to an older generation. Used by bounded incremental maintenance and by
    /// explicit rebuilds. Ordered by memory id for deterministic batches.
    pub fn memory_embedding_unassigned_vectors(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        generation: u64,
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
             LEFT JOIN memory_embedding_assignments a ON a.memory_id = m.id
             WHERE m.project_id = ?1
               AND m.status IN ('active', 'stale')
               AND e.model = ?2
               AND e.dimensions = ?3
               AND substr(e.vector, 1, 4) = x'5650454d'
               AND substr(e.vector, 5, 1) = x'01'
               AND length(e.vector) = 9 + 4 * ?3
               AND (a.memory_id IS NULL OR a.generation != ?4)
             ORDER BY m.id ASC
             LIMIT ?5",
        )?;
        let rows = stmt.query_map(
            params![
                project_id,
                model,
                i64::from(dimensions),
                i64::try_from(generation).unwrap_or(i64::MAX),
                i64::from(limit.clamp(1, 1_024))
            ],
            map_memory_semantic_candidate,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Vectors assigned to the probed clusters in one index generation.
    /// `limit` is a hard candidate window; `vp-core` scores at most that many
    /// vectors exactly and then keeps the policy candidate budget.
    #[allow(clippy::too_many_arguments)]
    pub fn memory_ivf_semantic_candidates(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        generation: u64,
        clusters: &[u32],
        include_stale: bool,
        limit: u32,
    ) -> Result<Vec<MemorySemanticCandidate>> {
        if clusters.is_empty() {
            return Ok(Vec::new());
        }
        let mut sql = String::from(
            "SELECT m.id, m.session_id, m.project_id, m.agent_id, m.agent_name, m.kind,
                    m.title, m.body, m.tags, m.created_at, m.status, m.source, m.evidence,
                    m.supersedes, m.superseded_by, m.pinned, m.use_count, m.last_used_at,
                    m.last_verified_at, m.verify_after, m.stale_reason,
                    e.model, e.dimensions, e.content_hash, e.vector
             FROM memory_embedding_assignments a
             JOIN memories m ON m.id = a.memory_id
             JOIN memory_embeddings e ON e.memory_id = m.id
             WHERE m.project_id = ?
               AND a.generation = ?
               AND e.model = ?
               AND e.dimensions = ?
               AND (? = 1 OR m.status = 'active')
               AND m.status IN ('active', 'stale')
               AND substr(e.vector, 1, 4) = x'5650454d'
               AND substr(e.vector, 5, 1) = x'01'
               AND length(e.vector) = 9 + 4 * ?
               AND a.cluster IN (",
        );
        let placeholders = std::iter::repeat_n("?", clusters.len())
            .collect::<Vec<_>>()
            .join(",");
        sql.push_str(&placeholders);
        sql.push_str(
            ")
             ORDER BY m.created_at DESC, m.id ASC
             LIMIT ?",
        );
        let mut values = vec![
            rusqlite::types::Value::Text(project_id.to_string()),
            rusqlite::types::Value::Integer(i64::try_from(generation).unwrap_or(i64::MAX)),
            rusqlite::types::Value::Text(model.to_string()),
            rusqlite::types::Value::Integer(i64::from(dimensions)),
            rusqlite::types::Value::Integer(if include_stale { 1 } else { 0 }),
            rusqlite::types::Value::Integer(i64::from(dimensions)),
        ];
        values.extend(
            clusters
                .iter()
                .map(|cluster| rusqlite::types::Value::Integer(i64::from(*cluster))),
        );
        values.push(rusqlite::types::Value::Integer(i64::from(
            limit.clamp(1, 4_096),
        )));
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(values),
            map_memory_semantic_candidate,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Persist a complete centroid generation transactionally. The returned
    /// generation is strictly monotonic even if two builds race: a request
    /// below the stored generation is bumped to `stored + 1`. Assignments from
    /// older generations are removed in the same transaction, so a recall
    /// either sees the new generation or falls back; it cannot mix them.
    #[allow(clippy::too_many_arguments)]
    pub fn save_memory_embedding_index(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        requested_generation: u64,
        nlist: u32,
        cluster_count: u32,
        vector_count: u32,
        centroids: &[u8],
    ) -> Result<u64> {
        if centroids.is_empty() {
            anyhow::bail!("refusing to persist an empty centroid blob");
        }
        if centroids.len() > MAX_MEMORY_INDEX_BLOB_BYTES {
            anyhow::bail!("centroid blob exceeds the {MAX_MEMORY_INDEX_BLOB_BYTES}-byte ceiling");
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let current: Option<i64> = tx
            .query_row(
                "SELECT generation FROM memory_embedding_index WHERE project_id = ?1",
                params![project_id],
                |row| row.get(0),
            )
            .optional()?;
        let floor = current
            .and_then(|value| u64::try_from(value).ok())
            .unwrap_or(0)
            .saturating_add(1);
        let generation = requested_generation.max(floor).max(1);
        let generation = i64::try_from(generation).unwrap_or(i64::MAX);
        let now = now_rfc3339();
        tx.execute(
            "INSERT INTO memory_embedding_index
             (project_id, model, dimensions, generation, nlist, cluster_count, vector_count,
              centroids, updated_at, last_error, fallback_count, last_fallback_at,
              last_fallback_reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL,
                     COALESCE((SELECT fallback_count FROM memory_embedding_index WHERE project_id = ?1), 0),
                     (SELECT last_fallback_at FROM memory_embedding_index WHERE project_id = ?1),
                     (SELECT last_fallback_reason FROM memory_embedding_index WHERE project_id = ?1))
             ON CONFLICT(project_id) DO UPDATE SET
                model = excluded.model,
                dimensions = excluded.dimensions,
                generation = excluded.generation,
                nlist = excluded.nlist,
                cluster_count = excluded.cluster_count,
                vector_count = excluded.vector_count,
                centroids = excluded.centroids,
                updated_at = excluded.updated_at,
                last_error = NULL",
            params![
                project_id,
                model,
                i64::from(dimensions),
                generation,
                i64::from(nlist),
                i64::from(cluster_count),
                i64::from(vector_count),
                centroids,
                now
            ],
        )?;
        tx.execute(
            "DELETE FROM memory_embedding_assignments
             WHERE generation != ?1
               AND memory_id IN (SELECT id FROM memories WHERE project_id = ?2)",
            params![generation, project_id],
        )?;
        tx.commit()?;
        Ok(u64::try_from(generation).unwrap_or(u64::MAX))
    }

    /// Persist cluster assignments for the current generation. If another
    /// build committed a newer generation first, this batch is discarded and
    /// returns 0 instead of writing assignments that recall would ignore.
    pub fn save_memory_embedding_assignments(
        &self,
        project_id: &str,
        generation: u64,
        assignments: &[(String, u32)],
    ) -> Result<u32> {
        if assignments.is_empty() {
            return Ok(0);
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let current: Option<i64> = tx
            .query_row(
                "SELECT generation FROM memory_embedding_index WHERE project_id = ?1",
                params![project_id],
                |row| row.get(0),
            )
            .optional()?;
        if current.and_then(|value| u64::try_from(value).ok()) != Some(generation) {
            tx.commit()?;
            return Ok(0);
        }
        let now = now_rfc3339();
        let generation = i64::try_from(generation).unwrap_or(i64::MAX);
        let mut written = 0u32;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO memory_embedding_assignments
                 (memory_id, generation, cluster, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(memory_id) DO UPDATE SET
                    generation = excluded.generation,
                    cluster = excluded.cluster,
                    updated_at = excluded.updated_at",
            )?;
            for (memory_id, cluster) in assignments {
                stmt.execute(params![memory_id, generation, i64::from(*cluster), now])?;
                written = written.saturating_add(1);
            }
        }
        tx.commit()?;
        Ok(written)
    }

    /// Remove the derived index and all of its assignments for one project.
    /// Memories and stored embeddings are untouched.
    pub fn delete_memory_embedding_index(&self, project_id: &str) -> Result<u32> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let assignments = tx.execute(
            "DELETE FROM memory_embedding_assignments
             WHERE memory_id IN (SELECT id FROM memories WHERE project_id = ?1)",
            params![project_id],
        )?;
        let index = tx.execute(
            "DELETE FROM memory_embedding_index WHERE project_id = ?1",
            params![project_id],
        )?;
        tx.commit()?;
        Ok(u32::try_from(index.saturating_add(assignments)).unwrap_or(u32::MAX))
    }

    /// Record a sanitized build/maintenance error. A valid previous generation
    /// is preserved: recall keeps using it only if model/dimensions/nlist still
    /// match, otherwise it fails closed to the exact scan.
    pub fn record_memory_embedding_index_error(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        nlist: u32,
        error: &str,
    ) -> Result<()> {
        let error = truncate_chars(error.trim(), 2_000);
        let conn = self.lock()?;
        let now = now_rfc3339();
        conn.execute(
            "INSERT INTO memory_embedding_index
             (project_id, model, dimensions, generation, nlist, cluster_count, vector_count,
              centroids, updated_at, last_error)
             VALUES (?1, ?2, ?3, 0, ?4, 0, 0, x'', ?5, ?6)
             ON CONFLICT(project_id) DO UPDATE SET
                last_error = excluded.last_error,
                updated_at = excluded.updated_at",
            params![
                project_id,
                model,
                i64::from(dimensions),
                i64::from(nlist),
                now,
                error
            ],
        )?;
        Ok(())
    }

    /// Increment the diagnostic fallback counter. This is the only write the
    /// recall read path performs, and only when an IVF probe actually falls
    /// back; a scan-mode project never touches these rows.
    ///
    /// A generation-0 diagnostic row is created when no index exists so
    /// "below minimum" and "not built" fallbacks are observable in status.
    /// It has an empty centroid blob and can never be used by recall.
    #[allow(clippy::too_many_arguments)]
    pub fn note_memory_embedding_index_fallback(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        nlist: u32,
        reason: &str,
    ) -> Result<()> {
        let reason = truncate_chars(reason.trim(), 500);
        let conn = self.lock()?;
        let now = now_rfc3339();
        conn.execute(
            "INSERT INTO memory_embedding_index
             (project_id, model, dimensions, generation, nlist, cluster_count, vector_count,
              centroids, updated_at, last_error, fallback_count, last_fallback_at,
              last_fallback_reason)
             VALUES (?1, ?2, ?3, 0, ?4, 0, 0, x'', ?5, NULL, 1, ?5, ?6)
             ON CONFLICT(project_id) DO UPDATE SET
                fallback_count = memory_embedding_index.fallback_count + 1,
                last_fallback_at = excluded.last_fallback_at,
                last_fallback_reason = excluded.last_fallback_reason",
            params![
                project_id,
                model,
                i64::from(dimensions),
                i64::from(nlist),
                now,
                reason
            ],
        )?;
        Ok(())
    }
}

use super::*;

impl Store {
    /// Opt-in source embedding policy. Absence means disabled with defaults;
    /// a malformed JSON row is ignored rather than enabling network access.
    pub fn get_source_embedding_policy(&self, project_id: &str) -> Result<SourceEmbeddingPolicy> {
        let conn = self.lock()?;
        let raw: Option<String> = conn
            .query_row(
                "SELECT policy FROM project_source_embedding_policies WHERE project_id = ?1",
                params![project_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(raw
            .and_then(|raw| decode_source_embedding_policy(&raw))
            .unwrap_or_default())
    }

    pub fn set_source_embedding_policy(
        &self,
        project_id: &str,
        policy: &SourceEmbeddingPolicy,
    ) -> Result<()> {
        let raw = serde_json::to_string(policy)?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_source_embedding_policies (project_id, policy, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(project_id) DO UPDATE SET
                policy = excluded.policy,
                updated_at = excluded.updated_at",
            params![project_id, raw, now_rfc3339()],
        )?;
        Ok(())
    }

    /// Projects whose source embedding lane is explicitly enabled and fully
    /// configured. A configured-but-disabled policy is never returned.
    pub fn list_enabled_source_embedding_policies(
        &self,
    ) -> Result<Vec<(String, SourceEmbeddingPolicy)>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT project_id, policy FROM project_source_embedding_policies
             ORDER BY project_id ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut policies = Vec::new();
        for row in rows {
            let (project_id, raw) = row?;
            let Some(policy) = decode_source_embedding_policy(&raw) else {
                continue;
            };
            if policy.is_active() {
                policies.push((project_id, policy));
            }
        }
        Ok(policies)
    }

    /// Read the additive retrieval v2 policy. Missing or malformed rows decode
    /// as the documented defaults (ANN and expansion off, deterministic
    /// rerank); a malformed row never enables ANN implicitly.
    pub fn get_source_retrieval_policy(&self, project_id: &str) -> Result<SourceRetrievalPolicy> {
        let conn = self.lock()?;
        let raw: Option<String> = conn
            .query_row(
                "SELECT policy FROM project_source_retrieval_policies WHERE project_id = ?1",
                params![project_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(raw
            .and_then(|raw| serde_json::from_str::<SourceRetrievalPolicy>(&raw).ok())
            .unwrap_or_default())
    }

    pub fn set_source_retrieval_policy(
        &self,
        project_id: &str,
        policy: &SourceRetrievalPolicy,
    ) -> Result<()> {
        let raw = serde_json::to_string(policy)?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_source_retrieval_policies (project_id, policy, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(project_id) DO UPDATE SET
                policy = excluded.policy,
                updated_at = excluded.updated_at",
            params![project_id, raw, now_rfc3339()],
        )?;
        Ok(())
    }

    /// Current derived source embedding generation for one source. Missing
    /// state is generation zero so the first insert becomes generation one.
    pub fn source_embedding_generation(&self, source_id: &str) -> Result<u64> {
        let conn = self.lock()?;
        let generation: Option<i64> = conn
            .query_row(
                "SELECT generation FROM source_embedding_state WHERE source_id = ?1",
                params![source_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(generation.unwrap_or(0).max(0) as u64)
    }

    /// One bounded chunk plan for a source generation. Returns every chunk in
    /// the cap plus its existing vector metadata; the engine filters by the
    /// exact embedding-text hash before making a request.
    pub fn source_embedding_chunks(
        &self,
        source_id: &str,
        max_chunks: u32,
    ) -> Result<Vec<SourceEmbeddingChunkRow>> {
        let conn = self.lock()?;
        let max_chunks = max_chunks.clamp(1, 500) as i64;
        let mut stmt = conn.prepare(
            "SELECT c.source_id, s.title, c.id, c.chunk_no, c.heading, c.content,
                    c.token_count,
                    COALESCE(st.generation, 1),
                    v.model, v.dimensions, v.generation, v.status, v.content_hash
             FROM source_chunks c
             JOIN sources s ON s.id = c.source_id
             LEFT JOIN source_embedding_state st ON st.source_id = c.source_id
             LEFT JOIN source_embeddings v ON v.chunk_id = c.id
             WHERE c.source_id = ?1
               AND c.chunk_no <= ?2
             ORDER BY c.chunk_no ASC",
        )?;
        let rows = stmt.query_map(params![source_id, max_chunks], |row| {
            Ok(SourceEmbeddingChunkRow {
                source_id: row.get(0)?,
                source_title: row.get(1)?,
                chunk_id: row.get(2)?,
                chunk_no: u32::try_from(row.get::<_, i64>(3)?).unwrap_or(u32::MAX),
                heading: row.get(4)?,
                content: row.get(5)?,
                token_count: u32::try_from(row.get::<_, i64>(6)?).unwrap_or(u32::MAX),
                generation: u64::try_from(row.get::<_, i64>(7)?).unwrap_or(0),
                vector_model: row.get(8)?,
                vector_dimensions: row
                    .get::<_, Option<i64>>(9)?
                    .and_then(|value| u32::try_from(value).ok()),
                vector_generation: row
                    .get::<_, Option<i64>>(10)?
                    .and_then(|value| u64::try_from(value).ok()),
                vector_status: row.get(11)?,
                vector_content_hash: row.get(12)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Jobs that are due now. A running job is reclaimed only when its
    /// heartbeat is older than the supplied cutoff, so a crashed reconciler
    /// cannot strand a source forever.
    pub fn due_source_embedding_jobs(
        &self,
        project_id: &str,
        model: &str,
        max_attempts: u32,
        now: &str,
        stale_running_before: &str,
        limit: u32,
    ) -> Result<Vec<SourceEmbeddingJobRow>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT j.source_id, s.title, j.project_id, j.generation, j.model, j.status,
                    j.attempts, j.chunks_total, j.chunks_done, j.last_error,
                    j.next_attempt_at, j.updated_at
             FROM source_embedding_jobs j
             JOIN sources s ON s.id = j.source_id
             JOIN source_embedding_state st ON st.source_id = j.source_id
             WHERE j.project_id = ?1
               AND j.model = ?2
               AND j.generation = st.generation
               AND (
                    j.status = 'pending'
                    OR (
                        j.status = 'failed'
                        AND j.attempts < ?3
                        AND (j.next_attempt_at IS NULL OR j.next_attempt_at <= ?4)
                    )
                    OR (j.status = 'running' AND j.updated_at <= ?5)
               )
             ORDER BY COALESCE(j.next_attempt_at, j.updated_at) ASC, j.source_id ASC
             LIMIT ?6",
        )?;
        let rows = stmt.query_map(
            params![
                project_id,
                model,
                i64::from(max_attempts),
                now,
                stale_running_before,
                i64::from(limit.clamp(1, 16))
            ],
            |row| {
                Ok(SourceEmbeddingJobRow {
                    source_id: row.get(0)?,
                    source_title: row.get(1)?,
                    project_id: row.get(2)?,
                    generation: u64::try_from(row.get::<_, i64>(3)?).unwrap_or(0),
                    model: row.get(4)?,
                    status: row.get(5)?,
                    attempts: u32::try_from(row.get::<_, i64>(6)?).unwrap_or(u32::MAX),
                    chunks_total: u32::try_from(row.get::<_, i64>(7)?).unwrap_or(u32::MAX),
                    chunks_done: u32::try_from(row.get::<_, i64>(8)?).unwrap_or(u32::MAX),
                    last_error: row.get(9)?,
                    next_attempt_at: row.get(10)?,
                    updated_at: row.get(11)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Mark a due job as running before the network call. The generation guard
    /// prevents a stale job from overwriting a newer snapshot's state.
    pub fn mark_source_embedding_job_running(
        &self,
        source_id: &str,
        generation: u64,
    ) -> Result<bool> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE source_embedding_jobs
             SET status = 'running', updated_at = ?3
             WHERE source_id = ?1 AND generation = ?2 AND status != 'dead'",
            params![
                source_id,
                i64::try_from(generation).unwrap_or(i64::MAX),
                now_rfc3339()
            ],
        )?;
        Ok(changed > 0)
    }

    pub fn record_source_embedding_progress(
        &self,
        source_id: &str,
        generation: u64,
        chunks_done: u32,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE source_embedding_jobs
             SET chunks_done = MAX(chunks_done, ?3), updated_at = ?4
             WHERE source_id = ?1 AND generation = ?2",
            params![
                source_id,
                i64::try_from(generation).unwrap_or(i64::MAX),
                i64::from(chunks_done),
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    /// Return a job to `pending` after a successful partial batch. This is not
    /// a failure: the next tick continues with the remaining chunks without
    /// consuming a retry attempt.
    pub fn mark_source_embedding_job_pending(
        &self,
        source_id: &str,
        generation: u64,
        chunks_done: u32,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE source_embedding_jobs
             SET status = 'pending',
                 chunks_done = MAX(chunks_done, ?3),
                 last_error = NULL,
                 next_attempt_at = NULL,
                 updated_at = ?4
             WHERE source_id = ?1 AND generation = ?2 AND status != 'dead'",
            params![
                source_id,
                i64::try_from(generation).unwrap_or(i64::MAX),
                i64::from(chunks_done),
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    /// Persist one successful vector only while its generation is still the
    /// current snapshot. A concurrent refresh advances the state first, so a
    /// late response is ignored instead of resurrecting superseded content.
    #[allow(clippy::too_many_arguments)]
    pub fn record_source_embedding_success(
        &self,
        source_id: &str,
        chunk_id: &str,
        chunk_no: u32,
        generation: u64,
        model: &str,
        dimensions: u32,
        content_hash: &str,
        vector: &[u8],
    ) -> Result<bool> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let current: Option<i64> = tx
            .query_row(
                "SELECT generation FROM source_embedding_state WHERE source_id = ?1",
                params![source_id],
                |row| row.get(0),
            )
            .optional()?;
        if current.map(|value| value.max(0) as u64) != Some(generation) {
            tx.commit()?;
            return Ok(false);
        }
        let project_id: String = tx.query_row(
            "SELECT project_id FROM sources WHERE id = ?1",
            params![source_id],
            |row| row.get(0),
        )?;
        let now = now_rfc3339();
        let changed = tx.execute(
            "INSERT INTO source_embeddings
             (chunk_id, project_id, source_id, chunk_no, generation, model, dimensions,
              content_hash, vector, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'current', ?10, ?10)
             ON CONFLICT(chunk_id) DO UPDATE SET
                project_id = excluded.project_id,
                source_id = excluded.source_id,
                chunk_no = excluded.chunk_no,
                generation = excluded.generation,
                model = excluded.model,
                dimensions = excluded.dimensions,
                content_hash = excluded.content_hash,
                vector = excluded.vector,
                status = 'current',
                updated_at = excluded.updated_at
             WHERE excluded.generation >= source_embeddings.generation",
            params![
                chunk_id,
                project_id,
                source_id,
                i64::from(chunk_no),
                i64::try_from(generation).unwrap_or(i64::MAX),
                model,
                i64::from(dimensions),
                content_hash,
                vector,
                now
            ],
        )?;
        tx.commit()?;
        Ok(changed > 0)
    }

    /// Record a failed tick. `next_attempt_at = None` or `dead = true` marks a
    /// dead-letter row that stays visible until reindex/retention pruning.
    #[allow(clippy::too_many_arguments)]
    pub fn record_source_embedding_failure(
        &self,
        source_id: &str,
        generation: u64,
        attempts: u32,
        error: &str,
        next_attempt_at: Option<&str>,
        dead: bool,
    ) -> Result<()> {
        let error = truncate_chars(error.trim(), 2_000);
        let conn = self.lock()?;
        conn.execute(
            "UPDATE source_embedding_jobs
             SET status = ?3,
                 attempts = ?4,
                 last_error = ?5,
                 next_attempt_at = ?6,
                 updated_at = ?7
             WHERE source_id = ?1 AND generation = ?2",
            params![
                source_id,
                i64::try_from(generation).unwrap_or(i64::MAX),
                if dead { "dead" } else { "failed" },
                i64::from(attempts),
                error,
                if dead { None } else { next_attempt_at },
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn record_source_embedding_job_done(
        &self,
        source_id: &str,
        generation: u64,
        chunks_done: u32,
    ) -> Result<bool> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE source_embedding_jobs
             SET status = 'done',
                 chunks_done = MAX(chunks_done, ?3),
                 last_error = NULL,
                 next_attempt_at = NULL,
                 updated_at = ?4
             WHERE source_id = ?1 AND generation = ?2",
            params![
                source_id,
                i64::try_from(generation).unwrap_or(i64::MAX),
                i64::from(chunks_done),
                now_rfc3339()
            ],
        )?;
        Ok(changed > 0)
    }

    /// Older-generation or status-stale vectors retained for retention. Used
    /// only to distinguish an empty corpus from an out-of-date one.
    pub fn source_embedding_stale_count(&self, project_id: &str) -> Result<u32> {
        let conn = self.lock()?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*)
             FROM source_embeddings v
             JOIN source_embedding_state st ON st.source_id = v.source_id
             WHERE v.project_id = ?1
               AND (v.status = 'stale' OR v.generation != st.generation)",
            params![project_id],
            |row| row.get(0),
        )?;
        Ok(u32::try_from(count).unwrap_or(u32::MAX))
    }

    /// Count current-generation vectors that can serve an exact cosine scan.
    /// Used to distinguish "no semantic evidence" from "the lane is not ready"
    /// without decoding vectors in status/search preflight.
    pub fn source_embedding_ready_count(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        max_chunks: u32,
    ) -> Result<u32> {
        let conn = self.lock()?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*)
             FROM source_embeddings v
             JOIN source_chunks c ON c.id = v.chunk_id
             JOIN sources s ON s.id = v.source_id
             JOIN source_embedding_state st ON st.source_id = v.source_id
             WHERE v.project_id = ?1
               AND s.status = 'indexed'
               AND c.chunk_no <= ?2
               AND v.status = 'current'
               AND v.generation = st.generation
               AND v.model = ?3
               AND v.dimensions = ?4
               AND substr(v.vector, 1, 4) = x'5650454d'
               AND substr(v.vector, 5, 1) = x'01'
               AND length(v.vector) = 9 + 4 * ?4",
            params![
                project_id,
                i64::from(max_chunks.clamp(1, 500)),
                model,
                i64::from(dimensions)
            ],
            |row| row.get(0),
        )?;
        Ok(u32::try_from(count).unwrap_or(u32::MAX))
    }

    /// Exact, bounded cosine-scan candidate set. Rows are ordered by source and
    /// chunk so the engine's tie-breaking is deterministic. No vector is
    /// serialized outside this internal struct.
    #[allow(clippy::too_many_arguments)]
    pub fn source_semantic_candidates(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        max_chunks: u32,
        source_ids: &[String],
        collections: &[String],
        labels: &[String],
        limit: u32,
    ) -> Result<Vec<SourceSemanticCandidate>> {
        let source_ids = serde_json::to_string(source_ids)?;
        let collections = serde_json::to_string(collections)?;
        let labels = serde_json::to_string(labels)?;
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT c.source_id, s.title, s.collection, s.labels, c.id, c.chunk_no,
                    c.heading, c.content, v.model, v.dimensions, v.content_hash, v.vector,
                    s.updated_at
             FROM source_embeddings v
             JOIN source_chunks c ON c.id = v.chunk_id
             JOIN sources s ON s.id = v.source_id
             JOIN source_embedding_state st ON st.source_id = v.source_id
             WHERE v.project_id = ?1
               AND s.status = 'indexed'
               AND c.chunk_no <= ?4
               AND v.status = 'current'
               AND v.generation = st.generation
               AND v.model = ?2
               AND v.dimensions = ?3
               AND substr(v.vector, 1, 4) = x'5650454d'
               AND substr(v.vector, 5, 1) = x'01'
               AND length(v.vector) = 9 + 4 * ?3
               AND (?5 = '[]' OR c.source_id IN (SELECT value FROM json_each(?5)))
               AND (?6 = '[]' OR COALESCE(s.collection, '') IN (SELECT value FROM json_each(?6)))
               AND (?7 = '[]' OR EXISTS (
                   SELECT 1 FROM json_each(s.labels) requested
                   WHERE requested.value IN (SELECT value FROM json_each(?7))
               ))
             ORDER BY c.source_id ASC, c.chunk_no ASC
             LIMIT ?8",
        )?;
        let rows = stmt.query_map(
            params![
                project_id,
                model,
                i64::from(dimensions),
                i64::from(max_chunks.clamp(1, 500)),
                source_ids,
                collections,
                labels,
                i64::from(limit.clamp(1, 512))
            ],
            |row| {
                let labels_blob: String = row.get(3)?;
                Ok(SourceSemanticCandidate {
                    source_id: row.get(0)?,
                    source_title: row.get(1)?,
                    collection: row.get(2)?,
                    labels: serde_json::from_str(&labels_blob).unwrap_or_default(),
                    chunk_id: row.get(4)?,
                    chunk_no: u32::try_from(row.get::<_, i64>(5)?).unwrap_or(u32::MAX),
                    heading: row.get(6)?,
                    content: row.get(7)?,
                    model: row.get(8)?,
                    dimensions: u32::try_from(row.get::<_, i64>(9)?).unwrap_or(u32::MAX),
                    content_hash: row.get(10)?,
                    vector: row.get(11)?,
                    source_updated_at: row.get(12).ok(),
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

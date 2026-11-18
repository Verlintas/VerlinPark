use super::*;

impl Store {
    /// Read one derived source ANN index row. `centroids` is opaque versioned
    /// bytes and is never serialized by an RPC/UI path.
    pub fn source_ann_index(&self, project_id: &str) -> Result<Option<SourceAnnIndexRow>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT project_id, model, dimensions, nlist, centroid_count, vector_count,
                    generation, centroids, last_error, created_at, updated_at
             FROM source_ann_index WHERE project_id = ?1",
            params![project_id],
            |row| {
                Ok(SourceAnnIndexRow {
                    project_id: row.get(0)?,
                    model: row.get(1)?,
                    dimensions: u32::try_from(row.get::<_, i64>(2)?).unwrap_or(u32::MAX),
                    nlist: u32::try_from(row.get::<_, i64>(3)?).unwrap_or(u32::MAX),
                    centroid_count: u32::try_from(row.get::<_, i64>(4)?).unwrap_or(u32::MAX),
                    vector_count: u32::try_from(row.get::<_, i64>(5)?).unwrap_or(u32::MAX),
                    generation: u64::try_from(row.get::<_, i64>(6)?).unwrap_or(0),
                    centroids: row.get(7)?,
                    last_error: row.get(8)?,
                    created_at: row.get(9)?,
                    updated_at: row.get(10)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    /// Live counts for one project/model/dimension/index-generation scope.
    /// Every count is bounded SQL aggregate state; no vector is decoded.
    #[allow(clippy::too_many_arguments)]
    pub fn source_ann_counts(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        max_chunks: u32,
        generation: u64,
        centroid_count: u32,
    ) -> Result<SourceAnnCounts> {
        let conn = self.lock()?;
        let valid = "v.status = 'current'
             AND v.generation = st.generation
             AND v.model = ?2
             AND v.dimensions = ?3
             AND substr(v.vector, 1, 4) = x'5650454d'
             AND substr(v.vector, 5, 1) = x'01'
             AND length(v.vector) = 9 + 4 * ?3
             AND c.chunk_no <= ?4
             AND s.status = 'indexed'";
        let vector_count: i64 = conn.query_row(
            &format!(
                "SELECT COUNT(*)
                 FROM source_embeddings v
                 JOIN source_chunks c ON c.id = v.chunk_id
                 JOIN sources s ON s.id = v.source_id
                 JOIN source_embedding_state st ON st.source_id = v.source_id
                 WHERE v.project_id = ?1 AND {valid}"
            ),
            params![
                project_id,
                model,
                i64::from(dimensions),
                i64::from(max_chunks.clamp(1, 500))
            ],
            |row| row.get(0),
        )?;
        let assigned_vectors: i64 = conn.query_row(
            &format!(
                "SELECT COUNT(*)
                 FROM source_ann_assignments a
                 JOIN source_embeddings v ON v.chunk_id = a.chunk_id
                 JOIN source_chunks c ON c.id = a.chunk_id
                 JOIN sources s ON s.id = c.source_id
                 JOIN source_embedding_state st ON st.source_id = v.source_id
                 WHERE a.project_id = ?1
                   AND a.model = ?2
                   AND a.dimensions = ?3
                   AND a.generation = ?5
                   AND a.cluster >= 0
                   AND a.cluster < ?6
                   AND v.project_id = ?1
                   AND {valid}"
            ),
            params![
                project_id,
                model,
                i64::from(dimensions),
                i64::from(max_chunks.clamp(1, 500)),
                i64::try_from(generation).unwrap_or(i64::MAX),
                i64::from(centroid_count)
            ],
            |row| row.get(0),
        )?;
        let total_assignments: i64 = conn.query_row(
            "SELECT COUNT(*) FROM source_ann_assignments
             WHERE project_id = ?1 AND model = ?2 AND dimensions = ?3 AND generation = ?4",
            params![
                project_id,
                model,
                i64::from(dimensions),
                i64::try_from(generation).unwrap_or(i64::MAX)
            ],
            |row| row.get(0),
        )?;
        let invalid_assignments: i64 = conn.query_row(
            &format!(
                "SELECT COUNT(*)
                 FROM source_ann_assignments a
                 WHERE a.project_id = ?1
                   AND a.model = ?2
                   AND a.dimensions = ?3
                   AND a.generation = ?5
                   AND (
                       a.cluster < 0
                       OR a.cluster >= ?6
                       OR NOT EXISTS (
                           SELECT 1
                           FROM source_embeddings v
                           JOIN source_chunks c ON c.id = v.chunk_id
                           JOIN sources s ON s.id = c.source_id
                           JOIN source_embedding_state st ON st.source_id = v.source_id
                           WHERE v.chunk_id = a.chunk_id AND {valid}
                       )
                   )"
            ),
            params![
                project_id,
                model,
                i64::from(dimensions),
                i64::from(max_chunks.clamp(1, 500)),
                i64::try_from(generation).unwrap_or(i64::MAX),
                i64::from(centroid_count)
            ],
            |row| row.get(0),
        )?;
        Ok(SourceAnnCounts {
            vector_count: u32::try_from(vector_count).unwrap_or(u32::MAX),
            assigned_vectors: u32::try_from(assigned_vectors).unwrap_or(u32::MAX),
            invalid_assignments: u32::try_from(invalid_assignments).unwrap_or(u32::MAX),
            total_assignments: u32::try_from(total_assignments).unwrap_or(u32::MAX),
        })
    }

    /// Persist a rebuilt index generation. The transaction deletes all prior
    /// assignments, increments the generation monotonically, and fails closed
    /// when `expected_generation` no longer matches. Returns `None` on a stale
    /// rebuild fence.
    #[allow(clippy::too_many_arguments)]
    pub fn save_source_ann_index(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        nlist: u32,
        centroid_count: u32,
        vector_count: u32,
        centroids: &[u8],
        expected_generation: Option<u64>,
    ) -> Result<Option<u64>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let current: i64 = tx
            .query_row(
                "SELECT generation FROM source_ann_index WHERE project_id = ?1",
                params![project_id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let current = u64::try_from(current.max(0)).unwrap_or(0);
        if let Some(expected) = expected_generation
            && current != expected
        {
            tx.commit()?;
            return Ok(None);
        }
        let next = current.saturating_add(1);
        tx.execute(
            "DELETE FROM source_ann_assignments WHERE project_id = ?1",
            params![project_id],
        )?;
        let now = now_rfc3339();
        tx.execute(
            "INSERT INTO source_ann_index
             (project_id, model, dimensions, nlist, centroid_count, vector_count,
              generation, centroids, last_error, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?9)
             ON CONFLICT(project_id) DO UPDATE SET
                model = excluded.model,
                dimensions = excluded.dimensions,
                nlist = excluded.nlist,
                centroid_count = excluded.centroid_count,
                vector_count = excluded.vector_count,
                generation = excluded.generation,
                centroids = excluded.centroids,
                last_error = NULL,
                updated_at = excluded.updated_at",
            params![
                project_id,
                model,
                i64::from(dimensions),
                i64::from(nlist),
                i64::from(centroid_count),
                i64::from(vector_count),
                i64::try_from(next).unwrap_or(i64::MAX),
                centroids,
                now
            ],
        )?;
        tx.commit()?;
        Ok(Some(next))
    }

    /// Delete the derived index and every assignment for one project. Returns
    /// the number of assignment rows removed.
    pub fn delete_source_ann(&self, project_id: &str) -> Result<u32> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let assignments = tx.execute(
            "DELETE FROM source_ann_assignments WHERE project_id = ?1",
            params![project_id],
        )?;
        tx.execute(
            "DELETE FROM source_ann_index WHERE project_id = ?1",
            params![project_id],
        )?;
        tx.commit()?;
        Ok(u32::try_from(assignments).unwrap_or(u32::MAX))
    }

    /// Record a sanitized rebuild error on an existing row. A missing row is
    /// not created: absence already reports `not_built`.
    pub fn record_source_ann_error(&self, project_id: &str, error: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE source_ann_index
             SET last_error = ?2, updated_at = ?3
             WHERE project_id = ?1",
            params![project_id, error, now_rfc3339()],
        )?;
        Ok(())
    }

    /// Bounded, stable training sample ordered by chunk id. The window function
    /// caps cumulative vector bytes so a high-dimensional corpus cannot
    /// allocate an unbounded training buffer.
    #[allow(clippy::too_many_arguments)]
    pub fn source_ann_training_vectors(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        max_chunks: u32,
        max_vectors: u32,
        max_bytes: u64,
    ) -> Result<Vec<SourceAnnVectorRow>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT chunk_id, vector FROM (
                 SELECT c.id AS chunk_id, v.vector AS vector,
                        SUM(length(v.vector)) OVER (ORDER BY c.id ASC) AS total_bytes
                 FROM source_embeddings v
                 JOIN source_chunks c ON c.id = v.chunk_id
                 JOIN sources s ON s.id = v.source_id
                 JOIN source_embedding_state st ON st.source_id = v.source_id
                 WHERE v.project_id = ?1
                   AND v.status = 'current'
                   AND v.generation = st.generation
                   AND v.model = ?2
                   AND v.dimensions = ?3
                   AND substr(v.vector, 1, 4) = x'5650454d'
                   AND substr(v.vector, 5, 1) = x'01'
                   AND length(v.vector) = 9 + 4 * ?3
                   AND c.chunk_no <= ?4
                   AND s.status = 'indexed'
             )
             WHERE total_bytes <= ?5
             ORDER BY chunk_id ASC
             LIMIT ?6",
        )?;
        let rows = stmt.query_map(
            params![
                project_id,
                model,
                i64::from(dimensions),
                i64::from(max_chunks.clamp(1, 500)),
                i64::try_from(max_bytes.min(i64::MAX as u64)).unwrap_or(i64::MAX),
                i64::from(max_vectors.clamp(1, 65_536))
            ],
            |row| {
                Ok(SourceAnnVectorRow {
                    chunk_id: row.get(0)?,
                    vector: row.get(1)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Current-generation vectors with no assignment for this index generation.
    pub fn source_ann_unassigned_vectors(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        max_chunks: u32,
        generation: u64,
        limit: u32,
    ) -> Result<Vec<SourceAnnVectorRow>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT c.id, v.vector
             FROM source_embeddings v
             JOIN source_chunks c ON c.id = v.chunk_id
             JOIN sources s ON s.id = v.source_id
             JOIN source_embedding_state st ON st.source_id = v.source_id
             LEFT JOIN source_ann_assignments a
                ON a.chunk_id = c.id
               AND a.project_id = ?1
               AND a.model = ?2
               AND a.dimensions = ?3
               AND a.generation = ?5
             WHERE v.project_id = ?1
               AND v.status = 'current'
               AND v.generation = st.generation
               AND v.model = ?2
               AND v.dimensions = ?3
               AND substr(v.vector, 1, 4) = x'5650454d'
               AND substr(v.vector, 5, 1) = x'01'
               AND length(v.vector) = 9 + 4 * ?3
               AND c.chunk_no <= ?4
               AND s.status = 'indexed'
               AND a.chunk_id IS NULL
             ORDER BY c.id ASC
             LIMIT ?6",
        )?;
        let rows = stmt.query_map(
            params![
                project_id,
                model,
                i64::from(dimensions),
                i64::from(max_chunks.clamp(1, 500)),
                i64::try_from(generation).unwrap_or(i64::MAX),
                i64::from(limit.clamp(1, 512))
            ],
            |row| {
                Ok(SourceAnnVectorRow {
                    chunk_id: row.get(0)?,
                    vector: row.get(1)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Persist a bounded assignment batch. Returns `false` when a concurrent
    /// rebuild advanced the generation; the caller stops instead of writing
    /// stale assignments.
    pub fn save_source_ann_assignments(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        generation: u64,
        assignments: &[(String, u32)],
    ) -> Result<bool> {
        if assignments.is_empty() {
            return Ok(true);
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let current: Option<i64> = tx
            .query_row(
                "SELECT generation FROM source_ann_index WHERE project_id = ?1",
                params![project_id],
                |row| row.get(0),
            )
            .optional()?;
        if current.map(|value| value.max(0) as u64) != Some(generation) {
            tx.commit()?;
            return Ok(false);
        }
        let now = now_rfc3339();
        {
            let mut stmt = tx.prepare(
                "INSERT INTO source_ann_assignments
                 (chunk_id, project_id, model, dimensions, generation, cluster, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(chunk_id) DO UPDATE SET
                    project_id = excluded.project_id,
                    model = excluded.model,
                    dimensions = excluded.dimensions,
                    generation = excluded.generation,
                    cluster = excluded.cluster,
                    updated_at = excluded.updated_at
                 WHERE source_ann_assignments.project_id = excluded.project_id
                   AND source_ann_assignments.generation <= excluded.generation",
            )?;
            for (chunk_id, cluster) in assignments {
                stmt.execute(params![
                    chunk_id,
                    project_id,
                    model,
                    i64::from(dimensions),
                    i64::try_from(generation).unwrap_or(i64::MAX),
                    i64::from(*cluster),
                    now
                ])?;
            }
        }
        tx.commit()?;
        Ok(true)
    }

    /// Bounded ANN candidate window. The window function gives every probed
    /// cluster a fair per-cluster share before the caller's total cap. All
    /// source filters, current-generation checks, and vector structural checks
    /// match the exact scan.
    #[allow(clippy::too_many_arguments)]
    pub fn source_ann_candidates(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        max_chunks: u32,
        generation: u64,
        clusters: &[u32],
        source_ids: &[String],
        collections: &[String],
        labels: &[String],
        per_cluster_limit: u32,
    ) -> Result<Vec<SourceSemanticCandidate>> {
        if clusters.is_empty() {
            return Ok(Vec::new());
        }
        let clusters = serde_json::to_string(clusters)?;
        let source_ids = serde_json::to_string(source_ids)?;
        let collections = serde_json::to_string(collections)?;
        let labels = serde_json::to_string(labels)?;
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT source_id, source_title, collection, labels, chunk_id, chunk_no,
                    heading, content, model, dimensions, content_hash, vector,
                    source_updated_at
             FROM (
                 SELECT c.source_id AS source_id,
                        s.title AS source_title,
                        s.collection AS collection,
                        s.labels AS labels,
                        c.id AS chunk_id,
                        c.chunk_no AS chunk_no,
                        c.heading AS heading,
                        c.content AS content,
                        v.model AS model,
                        v.dimensions AS dimensions,
                        v.content_hash AS content_hash,
                        v.vector AS vector,
                        s.updated_at AS source_updated_at,
                        ROW_NUMBER() OVER (
                            PARTITION BY a.cluster
                            ORDER BY c.source_id ASC, c.chunk_no ASC
                        ) AS cluster_rank
                 FROM source_ann_assignments a
                 JOIN source_embeddings v ON v.chunk_id = a.chunk_id
                 JOIN source_chunks c ON c.id = a.chunk_id
                 JOIN sources s ON s.id = c.source_id
                 JOIN source_embedding_state st ON st.source_id = v.source_id
                 WHERE a.project_id = ?1
                   AND a.model = ?2
                   AND a.dimensions = ?3
                   AND a.generation = ?4
                   AND a.cluster IN (SELECT value FROM json_each(?5))
                   AND v.project_id = ?1
                   AND v.status = 'current'
                   AND v.generation = st.generation
                   AND v.model = ?2
                   AND v.dimensions = ?3
                   AND substr(v.vector, 1, 4) = x'5650454d'
                   AND substr(v.vector, 5, 1) = x'01'
                   AND length(v.vector) = 9 + 4 * ?3
                   AND c.chunk_no <= ?6
                   AND s.status = 'indexed'
                   AND (?7 = '[]' OR c.source_id IN (SELECT value FROM json_each(?7)))
                   AND (?8 = '[]' OR COALESCE(s.collection, '') IN (SELECT value FROM json_each(?8)))
                   AND (?9 = '[]' OR EXISTS (
                       SELECT 1 FROM json_each(s.labels) requested
                       WHERE requested.value IN (SELECT value FROM json_each(?9))
                   ))
             )
             WHERE cluster_rank <= ?10
             ORDER BY source_id ASC, chunk_no ASC",
        )?;
        let rows = stmt.query_map(
            params![
                project_id,
                model,
                i64::from(dimensions),
                i64::try_from(generation).unwrap_or(i64::MAX),
                clusters,
                i64::from(max_chunks.clamp(1, 500)),
                source_ids,
                collections,
                labels,
                i64::from(per_cluster_limit.clamp(1, 256))
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

    /// Structural audit for assignment rows in one index generation.
    #[allow(clippy::too_many_arguments)]
    pub fn source_ann_assignment_audit(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        max_chunks: u32,
        generation: u64,
        centroid_count: u32,
    ) -> Result<SourceAnnAssignmentAudit> {
        let counts = self.source_ann_counts(
            project_id,
            model,
            dimensions,
            max_chunks,
            generation,
            centroid_count,
        )?;
        Ok(SourceAnnAssignmentAudit {
            total: counts.total_assignments,
            invalid: counts.invalid_assignments,
            repaired: 0,
        })
    }

    /// Delete invalid assignment rows for one index generation. This is a
    /// bounded repair, never a recall-path operation.
    #[allow(clippy::too_many_arguments)]
    pub fn delete_invalid_source_ann_assignments(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        max_chunks: u32,
        generation: u64,
        centroid_count: u32,
    ) -> Result<u32> {
        let conn = self.lock()?;
        let valid = "v.status = 'current'
             AND v.generation = st.generation
             AND v.model = ?2
             AND v.dimensions = ?3
             AND substr(v.vector, 1, 4) = x'5650454d'
             AND substr(v.vector, 5, 1) = x'01'
             AND length(v.vector) = 9 + 4 * ?3
             AND c.chunk_no <= ?4
             AND s.status = 'indexed'";
        let changed = conn.execute(
            &format!(
                "DELETE FROM source_ann_assignments
                 WHERE project_id = ?1
                   AND model = ?2
                   AND dimensions = ?3
                   AND generation = ?5
                   AND (
                       cluster < 0
                       OR cluster >= ?6
                       OR NOT EXISTS (
                           SELECT 1
                           FROM source_embeddings v
                           JOIN source_chunks c ON c.id = v.chunk_id
                           JOIN sources s ON s.id = c.source_id
                           JOIN source_embedding_state st ON st.source_id = v.source_id
                           WHERE v.chunk_id = source_ann_assignments.chunk_id
                             AND {valid}
                       )
                   )"
            ),
            params![
                project_id,
                model,
                i64::from(dimensions),
                i64::from(max_chunks.clamp(1, 500)),
                i64::try_from(generation).unwrap_or(i64::MAX),
                i64::from(centroid_count)
            ],
        )?;
        Ok(u32::try_from(changed).unwrap_or(u32::MAX))
    }
}

use super::*;

impl Store {
    const SOURCE_COLUMNS: &'static str = "id, project_id, kind, title, uri, mime_type, status, \
        size_bytes, sha256, revision, chunk_count, char_count, token_count, error, labels, \
        created_at, updated_at, indexed_at, collection";

    pub fn insert_source(&self, source: &Source, chunks: &[SourceChunk]) -> Result<Source> {
        let labels = serde_json::to_string(&source.labels)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO sources
             (id, project_id, kind, collection, title, uri, mime_type, status, size_bytes,
              sha256, revision, chunk_count, char_count, token_count, error, labels,
              created_at, updated_at, indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                     ?15, ?16, ?17, ?18, ?19)
             ON CONFLICT(id) DO UPDATE SET
                kind = excluded.kind,
                collection = excluded.collection,
                title = excluded.title,
                uri = excluded.uri,
                mime_type = excluded.mime_type,
                status = excluded.status,
                size_bytes = excluded.size_bytes,
                sha256 = excluded.sha256,
                revision = excluded.revision,
                chunk_count = excluded.chunk_count,
                char_count = excluded.char_count,
                token_count = excluded.token_count,
                error = excluded.error,
                labels = excluded.labels,
                updated_at = excluded.updated_at,
                indexed_at = excluded.indexed_at",
            params![
                source.id,
                source.project_id,
                source.kind,
                source.collection,
                source.title,
                source.uri,
                source.mime_type,
                source.status,
                source.size_bytes as i64,
                source.sha256,
                source.revision as i64,
                source.chunk_count as i64,
                source.char_count as i64,
                source.token_count as i64,
                source.error,
                labels,
                source.created_at,
                source.updated_at,
                source.indexed_at,
            ],
        )?;
        tx.execute(
            "DELETE FROM source_chunks WHERE source_id = ?1",
            params![source.id],
        )?;
        for chunk in chunks {
            tx.execute(
                "INSERT INTO source_chunks
                 (id, project_id, source_id, chunk_no, heading, content, start_offset,
                  end_offset, token_count, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    chunk.id,
                    source.project_id,
                    chunk.source_id,
                    chunk.chunk_no as i64,
                    chunk.heading,
                    chunk.content,
                    chunk.start_offset as i64,
                    chunk.end_offset as i64,
                    chunk.token_count as i64,
                    source.updated_at,
                ],
            )?;
        }
        // A new snapshot always advances the derived embedding generation. This
        // happens even while the lane is disabled so a later enable/reindex
        // cannot accidentally serve vectors from superseded chunk text. When
        // the project has an active policy, one bounded job is queued for the
        // new generation; a job failure never fails this source write.
        let policy = configured_source_embedding_policy_tx(&tx, &source.project_id)?;
        let queue = policy.as_ref().and_then(|policy| {
            policy
                .model
                .as_deref()
                .map(|model| (model, policy.max_chunks_per_source))
        });
        begin_source_embedding_generation_tx(
            &tx,
            &source.id,
            &source.project_id,
            chunks.len(),
            queue,
        )?;
        tx.commit()?;
        drop(conn);
        self.get_source(&source.id)?
            .ok_or_else(|| anyhow!("source disappeared after insert"))
    }

    pub fn get_source(&self, source_id: &str) -> Result<Option<Source>> {
        let conn = self.lock()?;
        conn.query_row(
            &format!("SELECT {} FROM sources WHERE id = ?1", Self::SOURCE_COLUMNS),
            params![source_id],
            map_source,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn list_sources(
        &self,
        project_id: &str,
        query: Option<&str>,
        status: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<Source>> {
        let conn = self.lock()?;
        let limit = limit.unwrap_or(200).clamp(1, 1_000) as i64;
        let query = query.map(|value| format!("%{}%", escape_like(value)));
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM sources
             WHERE project_id = ?1
               AND (?2 IS NULL OR status = ?2)
               AND (?3 IS NULL OR title LIKE ?3 ESCAPE '\\' OR uri LIKE ?3 ESCAPE '\\')
             ORDER BY updated_at DESC, id DESC LIMIT ?4",
            Self::SOURCE_COLUMNS
        ))?;
        let rows = stmt.query_map(params![project_id, status, query, limit], map_source)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn source_chunks(
        &self,
        source_id: &str,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<SourceChunk>> {
        let conn = self.lock()?;
        let offset = offset as i64;
        let limit = limit.clamp(1, 200) as i64;
        let mut stmt = conn.prepare(
            "SELECT id, source_id, chunk_no, heading, content, start_offset, end_offset,
                    token_count
             FROM source_chunks WHERE source_id = ?1
             ORDER BY chunk_no ASC LIMIT ?2 OFFSET ?3",
        )?;
        let rows = stmt.query_map(params![source_id, limit, offset], map_source_chunk)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn source_chunk_count(&self, source_id: &str) -> Result<usize> {
        let conn = self.lock()?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM source_chunks WHERE source_id = ?1",
            params![source_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count.max(0) as usize)
    }

    pub fn delete_source(&self, source_id: &str) -> Result<bool> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM source_ann_assignments
             WHERE chunk_id IN (SELECT id FROM source_chunks WHERE source_id = ?1)",
            params![source_id],
        )?;
        let changed = tx.execute("DELETE FROM sources WHERE id = ?1", params![source_id])?;
        tx.commit()?;
        Ok(changed > 0)
    }

    pub fn source_stats(&self, project_id: &str) -> Result<SourceStats> {
        let conn = self.lock()?;
        let mut stats = conn.query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(CASE WHEN status = 'indexed' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(chunk_count), 0),
                    COALESCE(SUM(size_bytes), 0),
                    COALESCE(SUM(token_count), 0)
             FROM sources WHERE project_id = ?1",
            params![project_id],
            |row| {
                Ok(SourceStats {
                    sources: row.get::<_, i64>(0)?.max(0) as usize,
                    indexed: row.get::<_, i64>(1)?.max(0) as usize,
                    failed: row.get::<_, i64>(2)?.max(0) as usize,
                    chunks: row.get::<_, i64>(3)?.max(0) as usize,
                    bytes: row.get::<_, i64>(4)?.max(0) as u64,
                    tokens: row.get::<_, i64>(5)?.max(0) as u64,
                })
            },
        )?;
        let chunk_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM source_chunks WHERE project_id = ?1",
            params![project_id],
            |row| row.get(0),
        )?;
        stats.chunks = chunk_count.max(0) as usize;
        Ok(stats)
    }

    /// List project collections with aggregate counts. Sources without a
    /// collection form a final `name: null` group. Labels are the distinct
    /// labels used by sources in the group.
    pub fn list_source_collections(&self, project_id: &str) -> Result<Vec<SourceCollection>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT NULLIF(COALESCE(NULLIF(collection, ''), ''), '') AS name,
                    COUNT(*),
                    COALESCE(SUM(chunk_count), 0),
                    COALESCE(SUM(size_bytes), 0),
                    COALESCE(SUM(token_count), 0),
                    COALESCE(GROUP_CONCAT(labels, char(31)), '[]')
             FROM sources
             WHERE project_id = ?1
             GROUP BY NULLIF(COALESCE(collection, ''), '')
             ORDER BY name IS NULL, name COLLATE NOCASE ASC",
        )?;
        let rows = stmt.query_map(params![project_id], |row| {
            let labels_blob: String = row.get(5)?;
            let mut labels = Vec::new();
            for value in labels_blob.split('\u{1f}') {
                if let Ok(values) = serde_json::from_str::<Vec<String>>(value) {
                    for label in values {
                        if !labels.iter().any(|existing| existing == &label) {
                            labels.push(label);
                        }
                    }
                }
            }
            labels.sort();
            Ok(SourceCollection {
                name: row.get(0)?,
                sources: row.get::<_, i64>(1)?.max(0) as usize,
                chunks: row.get::<_, i64>(2)?.max(0) as usize,
                bytes: row.get::<_, i64>(3)?.max(0) as u64,
                tokens: row.get::<_, i64>(4)?.max(0) as u64,
                labels,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn set_source_collection(
        &self,
        source_id: &str,
        collection: Option<&str>,
    ) -> Result<Option<Source>> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE sources SET collection = ?2, updated_at = ?3 WHERE id = ?1",
            params![source_id, collection, now_rfc3339()],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        drop(conn);
        self.get_source(source_id)
    }

    pub fn set_source_labels(&self, source_id: &str, labels: &[String]) -> Result<Option<Source>> {
        let labels = serde_json::to_string(labels)?;
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE sources SET labels = ?2, updated_at = ?3 WHERE id = ?1",
            params![source_id, labels, now_rfc3339()],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        drop(conn);
        self.get_source(source_id)
    }

    /// Update source metadata without touching chunks or the FTS index. The
    /// caller is responsible for keeping the durable snapshot in sync. A title
    /// change is part of the exact embedding text, so it advances the derived
    /// generation and queues a fresh bounded job when the lane is active.
    pub fn update_source_metadata(&self, source: &Source) -> Result<Source> {
        let labels = serde_json::to_string(&source.labels)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let previous_title: Option<String> = tx
            .query_row(
                "SELECT title FROM sources WHERE id = ?1",
                params![source.id],
                |row| row.get(0),
            )
            .optional()?;
        let changed = tx.execute(
            "UPDATE sources
             SET title = ?2, collection = ?3, labels = ?4, updated_at = ?5
             WHERE id = ?1",
            params![
                source.id,
                source.title,
                source.collection,
                labels,
                source.updated_at
            ],
        )?;
        if changed == 0 {
            anyhow::bail!("source not found: {}", source.id);
        }
        if previous_title.as_deref() != Some(source.title.as_str()) {
            let chunk_count: i64 = tx.query_row(
                "SELECT COUNT(*) FROM source_chunks WHERE source_id = ?1",
                params![source.id],
                |row| row.get(0),
            )?;
            let policy = configured_source_embedding_policy_tx(&tx, &source.project_id)?;
            let queue = policy.as_ref().and_then(|policy| {
                policy
                    .model
                    .as_deref()
                    .map(|model| (model, policy.max_chunks_per_source))
            });
            begin_source_embedding_generation_tx(
                &tx,
                &source.id,
                &source.project_id,
                usize::try_from(chunk_count).unwrap_or(usize::MAX),
                queue,
            )?;
        }
        tx.commit()?;
        drop(conn);
        self.get_source(&source.id)?
            .ok_or_else(|| anyhow!("source disappeared after metadata update"))
    }
}

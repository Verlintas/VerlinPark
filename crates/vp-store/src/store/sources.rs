//! Project sources, skills, snapshots, and embedding/ANN indexes.

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

    /// Search chunks with FTS5 (all terms, then any term) and a bounded literal
    /// fallback for CJK, short queries, or an FTS miss. Literal results are
    /// fused with FTS results for mixed-script queries instead of discarding
    /// one side. Returns `(hits, degraded)` where degraded marks an any-term
    /// relaxation or a fallback scan.
    pub fn search_source_chunks(
        &self,
        project_id: &str,
        source_ids: &[String],
        query: &SourceSearchQuery,
        limit: Option<u32>,
    ) -> Result<(Vec<SourceSearchHit>, bool)> {
        let limit = limit.unwrap_or(8).clamp(1, 50) as usize;
        let source_ids = serde_json::to_string(source_ids)?;
        let collections = serde_json::to_string(&query.collections)?;
        let labels = serde_json::to_string(&query.labels)?;
        let conn = self.lock()?;
        let filter = " AND (?3 = '[]' OR c.source_id IN (SELECT value FROM json_each(?3)))
               AND (?4 = '[]' OR COALESCE(s.collection, '') IN (SELECT value FROM json_each(?4)))
               AND (?5 = '[]' OR EXISTS (
                   SELECT 1 FROM json_each(s.labels) requested
                   WHERE requested.value IN (SELECT value FROM json_each(?5))
               ))";
        let sql = format!(
            "SELECT c.source_id, s.title, s.collection, s.labels, c.id, c.chunk_no,
                    c.heading, c.content, bm25(source_chunks_fts) AS rank, s.updated_at
             FROM source_chunks_fts
             JOIN source_chunks c ON c.rowid = source_chunks_fts.rowid
             JOIN sources s ON s.id = c.source_id
             WHERE source_chunks_fts MATCH ?1
               AND c.project_id = ?2
               AND s.status = 'indexed'{filter}
             ORDER BY rank ASC
             LIMIT ?6"
        );
        let mut fused: HashMap<String, SourceSearchHit> = HashMap::new();
        let mut degraded = false;
        let mut needs_literal_fallback = query.prefer_like;
        for (index, fts) in [&query.all, &query.any].into_iter().enumerate() {
            if fts.is_empty() {
                continue;
            }
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(
                params![
                    fts,
                    project_id,
                    source_ids,
                    collections,
                    labels,
                    limit as i64
                ],
                |row| {
                    let rank: f64 = row.get(8).unwrap_or(0.0);
                    let labels_blob: String = row.get(3)?;
                    Ok(SourceSearchHit {
                        source_id: row.get(0)?,
                        source_title: row.get(1)?,
                        collection: row.get(2)?,
                        labels: serde_json::from_str(&labels_blob).unwrap_or_default(),
                        chunk_id: row.get(4)?,
                        chunk_no: row.get::<_, i64>(5)?.max(0) as u32,
                        heading: row.get(6)?,
                        content: row.get(7)?,
                        score: (-rank).max(0.0),
                        source_updated_at: row.get(9).ok(),
                        ..SourceSearchHit::default()
                    })
                },
            );
            let Ok(rows) = rows else {
                degraded = true;
                needs_literal_fallback = true;
                break;
            };
            let hits = rows.collect::<Result<Vec<_>, _>>()?;
            if hits.is_empty() {
                continue;
            }
            // An any-term hit is a relaxation, not an exact match.
            if index == 1 {
                degraded = true;
            }
            for hit in hits {
                fused.entry(hit.chunk_id.clone()).or_insert(hit);
            }
            // The all-terms query already found something; do not widen it a
            // second time unless the caller explicitly wants a literal merge.
            break;
        }

        if fused.is_empty() {
            needs_literal_fallback = true;
        }
        if !needs_literal_fallback {
            let mut hits = fused.into_values().collect::<Vec<_>>();
            hits.sort_by(|left, right| {
                right
                    .score
                    .partial_cmp(&left.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| left.chunk_no.cmp(&right.chunk_no))
            });
            hits.truncate(limit);
            return Ok((hits, degraded));
        }

        // CJK and mixed-script queries always run the literal pass too. FTS5's
        // unicode61 tokenizer can treat a whole CJK run as one token, while a
        // mixed English/CJK query still benefits from the English FTS hits.
        degraded = true;
        let raw = query.raw.trim();
        let mut patterns = Vec::new();
        for value in std::iter::once(raw).chain(query.terms.iter().map(String::as_str)) {
            if value.is_empty() {
                continue;
            }
            let pattern = format!("%{}%", escape_like(value));
            if !patterns.contains(&pattern) {
                patterns.push(pattern);
            }
            if patterns.len() >= 40 {
                break;
            }
        }
        if patterns.is_empty() {
            return Ok((fused.into_values().collect(), degraded));
        }
        let patterns = serde_json::to_string(&patterns)?;
        let like_sql = "SELECT c.source_id, s.title, s.collection, s.labels, c.id, c.chunk_no,
                    c.heading, c.content, s.updated_at
             FROM source_chunks c
             JOIN sources s ON s.id = c.source_id
             WHERE c.project_id = ?1
               AND s.status = 'indexed'
               AND (?2 = '[]' OR c.source_id IN (SELECT value FROM json_each(?2)))
               AND (?3 = '[]' OR COALESCE(s.collection, '') IN (SELECT value FROM json_each(?3)))
               AND (?4 = '[]' OR EXISTS (
                   SELECT 1 FROM json_each(s.labels) requested
                   WHERE requested.value IN (SELECT value FROM json_each(?4))
               ))
               AND EXISTS (
                   SELECT 1 FROM json_each(?5) pattern
                   WHERE c.content LIKE pattern.value ESCAPE '\\'
                      OR c.heading LIKE pattern.value ESCAPE '\\'
               )
             ORDER BY c.chunk_no ASC
             LIMIT 500";
        let mut stmt = conn.prepare(like_sql)?;
        let needle = raw.to_lowercase();
        let rows = stmt.query_map(
            params![project_id, source_ids, collections, labels, patterns],
            |row| {
                let labels_blob: String = row.get(3)?;
                Ok(SourceSearchHit {
                    source_id: row.get(0)?,
                    source_title: row.get(1)?,
                    collection: row.get(2)?,
                    labels: serde_json::from_str(&labels_blob).unwrap_or_default(),
                    chunk_id: row.get(4)?,
                    chunk_no: row.get::<_, i64>(5)?.max(0) as u32,
                    heading: row.get(6)?,
                    content: row.get(7)?,
                    score: 0.0,
                    source_updated_at: row.get(8).ok(),
                    ..SourceSearchHit::default()
                })
            },
        )?;
        for row in rows {
            let mut hit = row?;
            let content = hit.content.to_lowercase();
            let heading = hit.heading.as_deref().unwrap_or("").to_lowercase();
            let raw_matches = content.matches(&needle).count() as f64;
            let raw_heading_matches = heading.matches(&needle).count() as f64;
            let term_matches = query
                .terms
                .iter()
                .map(|term| content.matches(term).count() + heading.matches(term).count())
                .sum::<usize>() as f64;
            if raw_matches == 0.0 && raw_heading_matches == 0.0 && term_matches == 0.0 {
                continue;
            }
            let score = raw_matches * 4.0 + raw_heading_matches * 8.0 + term_matches;
            if let Some(existing) = fused.get_mut(&hit.chunk_id) {
                existing.score = existing.score.max(score);
            } else {
                hit.score = score;
                fused.insert(hit.chunk_id.clone(), hit);
            }
        }
        let mut hits = fused.into_values().collect::<Vec<_>>();
        hits.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.chunk_no.cmp(&right.chunk_no))
        });
        hits.truncate(limit);
        Ok((hits, degraded))
    }

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

    /// Persist the durable source snapshot before/after the SQLite projection.
    /// A failed append must not be treated as a successful source add.
    pub fn write_source_snapshot(&self, snapshot: &SourceSnapshot) -> Result<()> {
        let dir = self.source_dir(&snapshot.source.project_id);
        paths::ensure_private_dir(&dir)?;
        let path = dir.join(format!("{}.json", snapshot.source.id));
        let temp = dir.join(format!(".tmp-{}.json", Uuid::now_v7()));
        let bytes = serde_json::to_vec(snapshot)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .with_context(|| format!("creating source snapshot {}", temp.display()))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, &path)
            .with_context(|| format!("replacing source snapshot {}", path.display()))?;
        paths::ensure_private_file(&path)?;
        Ok(())
    }

    pub fn read_source_snapshot(
        &self,
        project_id: &str,
        source_id: &str,
    ) -> Result<Option<SourceSnapshot>> {
        let path = self
            .source_dir(project_id)
            .join(format!("{}.json", source_id));
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
                format!("parsing source snapshot {}", path.display())
            })?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn delete_source_snapshot(&self, project_id: &str, source_id: &str) -> Result<()> {
        let path = self
            .source_dir(project_id)
            .join(format!("{}.json", source_id));
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).with_context(|| format!("removing {}", path.display())),
        }
    }

    /// All durable snapshots, used by `db-rebuild` and source recovery.
    pub fn list_source_snapshots(&self) -> Result<Vec<SourceSnapshot>> {
        let root = self.data_dir.join("sources");
        let mut snapshots = Vec::new();
        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error).with_context(|| format!("reading {}", root.display())),
        };
        for project in entries {
            let project = match project {
                Ok(project) => project,
                Err(_) => continue,
            };
            if !project
                .file_type()
                .map(|kind| kind.is_dir())
                .unwrap_or(false)
            {
                continue;
            }
            let files = match std::fs::read_dir(project.path()) {
                Ok(files) => files,
                Err(_) => continue,
            };
            for file in files {
                let file = match file {
                    Ok(file) => file,
                    Err(_) => continue,
                };
                if file.path().extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                let bytes = match std::fs::read(file.path()) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        tracing::warn!(%error, path = %file.path().display(), "could not read source snapshot");
                        continue;
                    }
                };
                match serde_json::from_slice::<SourceSnapshot>(&bytes) {
                    Ok(snapshot) => snapshots.push(snapshot),
                    Err(error) => {
                        tracing::warn!(%error, path = %file.path().display(), "skipping malformed source snapshot");
                    }
                }
            }
        }
        Ok(snapshots)
    }

    const SKILL_COLUMNS: &'static str = "id, project_id, name, slug, description, when_to_use, \
        version, instructions, tool_allow, tool_deny, profiles, roles, enabled, source, sha256, \
        created_at, updated_at";

    pub fn list_skills(
        &self,
        project_id: Option<&str>,
        include_disabled: bool,
    ) -> Result<Vec<Skill>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM skills
             WHERE (?1 IS NULL OR project_id = ?1)
               AND (?2 = 1 OR enabled = 1)
             ORDER BY name COLLATE NOCASE ASC, id ASC",
            Self::SKILL_COLUMNS
        ))?;
        let rows = stmt.query_map(
            params![project_id, if include_disabled { 1 } else { 0 }],
            map_skill,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_skill(&self, id: &str) -> Result<Option<Skill>> {
        let conn = self.lock()?;
        conn.query_row(
            &format!("SELECT {} FROM skills WHERE id = ?1", Self::SKILL_COLUMNS),
            params![id],
            map_skill,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn get_skill_by_slug(&self, project_id: &str, slug: &str) -> Result<Option<Skill>> {
        let conn = self.lock()?;
        conn.query_row(
            &format!(
                "SELECT {} FROM skills WHERE project_id = ?1 AND slug = ?2",
                Self::SKILL_COLUMNS
            ),
            params![project_id, slug],
            map_skill,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn insert_skill(&self, skill: &Skill) -> Result<Skill> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO skills
             (id, project_id, name, slug, description, when_to_use, version, instructions,
              tool_allow, tool_deny, profiles, roles, enabled, source, sha256, created_at,
              updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                     ?16, ?17)
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                slug = excluded.slug,
                description = excluded.description,
                when_to_use = excluded.when_to_use,
                version = excluded.version,
                instructions = excluded.instructions,
                tool_allow = excluded.tool_allow,
                tool_deny = excluded.tool_deny,
                profiles = excluded.profiles,
                roles = excluded.roles,
                enabled = excluded.enabled,
                source = excluded.source,
                sha256 = excluded.sha256,
                updated_at = excluded.updated_at",
            params![
                skill.id,
                skill.project_id,
                skill.name,
                skill.slug,
                skill.description,
                skill.when_to_use,
                skill.version,
                skill.instructions,
                serde_json::to_string(&skill.tool_allow)?,
                serde_json::to_string(&skill.tool_deny)?,
                serde_json::to_string(&skill.profiles)?,
                serde_json::to_string(&skill.roles)?,
                if skill.enabled { 1 } else { 0 },
                skill.source,
                skill.sha256,
                skill.created_at,
                skill.updated_at,
            ],
        )?;
        drop(conn);
        self.get_skill(&skill.id)?
            .ok_or_else(|| anyhow!("skill disappeared after insert"))
    }

    pub fn delete_skill(&self, id: &str) -> Result<bool> {
        let conn = self.lock()?;
        let changed = conn.execute("DELETE FROM skills WHERE id = ?1", params![id])?;
        Ok(changed > 0)
    }

    pub fn set_skill_enabled(&self, id: &str, enabled: bool) -> Result<Option<Skill>> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE skills SET enabled = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, if enabled { 1 } else { 0 }, now_rfc3339()],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        drop(conn);
        self.get_skill(id)
    }
}

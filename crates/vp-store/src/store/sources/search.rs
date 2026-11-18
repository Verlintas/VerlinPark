use super::*;

impl Store {
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
}

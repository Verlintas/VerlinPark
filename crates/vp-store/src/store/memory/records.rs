use super::*;

impl Store {
    /// Compatibility shim for callers that predate evidence-backed memory.
    /// Delegates with source `agent`, no evidence, and dedupe enabled. When the
    /// create is idempotent no event is written, so the existing
    /// `memory.created` event is returned if one exists (or a non-persisted
    /// payload-only placeholder for imported rows).
    #[allow(clippy::too_many_arguments)]
    pub fn create_memory_with_event(
        &self,
        session_id: &str,
        project_id: &str,
        agent_id: &str,
        agent_name: &str,
        kind: &str,
        title: &str,
        body: &str,
        tags: &[String],
    ) -> Result<(MemoryItem, Event)> {
        let outcome = self.create_memory_with_metadata_event(NewMemory {
            session_id,
            project_id,
            agent_id,
            agent_name,
            kind,
            title,
            body,
            tags,
            source: "agent",
            evidence: &[],
            pinned: false,
            verify_after: None,
            dedupe: true,
            auto_supersede: true,
        })?;
        let event = outcome
            .events
            .iter()
            .find(|event| event.kind == "memory.created")
            .cloned()
            .or_else(|| outcome.events.into_iter().next())
            .unwrap_or_else(|| Event {
                session_id: outcome.memory.session_id.clone(),
                seq: 0,
                kind: "memory.created".to_string(),
                payload: serde_json::to_value(&outcome.memory).unwrap_or_default(),
                created_at: outcome.memory.created_at.clone(),
            });
        Ok((outcome.memory, event))
    }

    /// Create a memory with provenance in one transaction. A normalized
    /// (kind, title) key may have at most one active row: an exact duplicate is
    /// idempotent, and conflicting content supersedes the previous row when
    /// both `dedupe` and `auto_supersede` are set.
    pub fn create_memory_with_metadata_event(
        &self,
        new: NewMemory<'_>,
    ) -> Result<MemoryCreateOutcome> {
        let new_id = Uuid::now_v7().to_string();
        let now = now_rfc3339();
        let mut supersedes = Vec::new();
        let mut events = Vec::new();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        if new.dedupe
            && let Some(mut existing) =
                active_memory_for_key_tx(&tx, new.project_id, new.kind, new.title)?
        {
            let same_content = existing.body.trim() == new.body.trim()
                && existing.evidence == new.evidence
                && existing.source == new.source;
            if same_content || !new.auto_supersede {
                tx.commit()?;
                return Ok(MemoryCreateOutcome {
                    memory: existing,
                    created: false,
                    events: Vec::new(),
                });
            }
            existing.status = "superseded".to_string();
            existing.superseded_by = Some(new_id.clone());
            update_memory_projection_tx(&tx, &existing)?;
            let event = insert_event_tx(
                &tx,
                &existing.session_id,
                "memory.superseded",
                serde_json::to_value(&existing)?,
            )?;
            supersedes.push(existing.id.clone());
            events.push(event);
        }
        let memory = MemoryItem {
            id: new_id,
            session_id: new.session_id.to_string(),
            project_id: new.project_id.to_string(),
            agent_id: new.agent_id.to_string(),
            agent_name: new.agent_name.to_string(),
            kind: new.kind.to_string(),
            title: new.title.to_string(),
            body: new.body.to_string(),
            tags: new.tags.to_vec(),
            created_at: now,
            status: "active".to_string(),
            source: new.source.to_string(),
            evidence: new.evidence.to_vec(),
            supersedes,
            superseded_by: None,
            pinned: new.pinned,
            use_count: 0,
            last_used_at: None,
            last_verified_at: None,
            verify_after: new.verify_after.map(str::to_string),
            stale_reason: None,
        };
        insert_memory_tx(&tx, &memory)?;
        let event = insert_event_tx(
            &tx,
            &memory.session_id,
            "memory.created",
            serde_json::to_value(&memory)?,
        )?;
        events.push(event);
        tx.commit()?;
        drop(conn);
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok(MemoryCreateOutcome {
            memory,
            created: true,
            events,
        })
    }

    pub fn list_memories(
        &self,
        project_id: &str,
        query: Option<&str>,
        kind: Option<&str>,
        limit: u32,
    ) -> Result<Vec<MemoryItem>> {
        self.list_memories_filtered(project_id, query, kind, None, false, limit)
    }

    /// Active memories by default; `stale` only when `include_stale` is set.
    /// An exact `status` filter takes precedence over `include_stale`.
    #[allow(clippy::too_many_arguments)]
    pub fn list_memories_filtered(
        &self,
        project_id: &str,
        query: Option<&str>,
        kind: Option<&str>,
        status: Option<&str>,
        include_stale: bool,
        limit: u32,
    ) -> Result<Vec<MemoryItem>> {
        if let Some(query) = query
            && let Some(query) = fts_query(query)
        {
            match self.list_memories_fts(project_id, &query, kind, status, include_stale, limit) {
                Ok(memories) if !memories.is_empty() => return Ok(memories),
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "memory FTS search failed; falling back to LIKE");
                }
            }
        }
        let conn = self.lock()?;
        let pattern = query.map(|query| format!("%{query}%"));
        let mut stmt = conn.prepare(
            "SELECT id, session_id, project_id, agent_id, agent_name, kind, title, body,
                    tags, created_at, status, source, evidence, supersedes, superseded_by,
                    pinned, use_count, last_used_at, last_verified_at, verify_after, stale_reason
             FROM memories m
             WHERE m.project_id = ?1
               AND (?2 IS NULL OR m.title LIKE ?2 OR m.body LIKE ?2 OR m.tags LIKE ?2)
               AND (?3 IS NULL OR m.kind = ?3)
               AND (
                   (?4 IS NOT NULL AND m.status = ?4)
                   OR (?4 IS NULL AND (m.status = 'active' OR (?5 = 1 AND m.status = 'stale')))
               )
             ORDER BY m.created_at DESC, m.id ASC
             LIMIT ?6",
        )?;
        let rows = stmt.query_map(
            params![
                project_id,
                pattern,
                kind,
                status,
                if include_stale { 1 } else { 0 },
                limit.clamp(1, 500) as i64
            ],
            map_memory_row,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn list_memories_fts(
        &self,
        project_id: &str,
        query: &str,
        kind: Option<&str>,
        status: Option<&str>,
        include_stale: bool,
        limit: u32,
    ) -> Result<Vec<MemoryItem>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT m.id, m.session_id, m.project_id, m.agent_id, m.agent_name, m.kind,
                    m.title, m.body, m.tags, m.created_at, m.status, m.source, m.evidence,
                    m.supersedes, m.superseded_by, m.pinned, m.use_count, m.last_used_at,
                    m.last_verified_at, m.verify_after, m.stale_reason
             FROM memories_fts
             JOIN memories m ON m.rowid = memories_fts.rowid
             WHERE memories_fts MATCH ?1
               AND m.project_id = ?2
               AND (?3 IS NULL OR m.kind = ?3)
               AND (
                   (?4 IS NOT NULL AND m.status = ?4)
                   OR (?4 IS NULL AND (m.status = 'active' OR (?5 = 1 AND m.status = 'stale')))
               )
             ORDER BY bm25(memories_fts), m.created_at DESC, m.id ASC
             LIMIT ?6",
        )?;
        let rows = stmt.query_map(
            params![
                query,
                project_id,
                kind,
                status,
                if include_stale { 1 } else { 0 },
                limit.clamp(1, 500) as i64
            ],
            map_memory_row,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Ranked candidates for deterministic retrieval. Up to 200 rows; status
    /// filtering matches `list_memories_filtered`.
    #[allow(clippy::too_many_arguments)]
    pub fn memory_candidates(
        &self,
        project_id: &str,
        query: &str,
        kind: Option<&str>,
        kinds: &[String],
        include_stale: bool,
        limit: u32,
    ) -> Result<Vec<MemoryCandidate>> {
        let mut candidates = Vec::new();
        if let Some(fts) = fts_query(query) {
            match self.memory_candidates_fts(project_id, &fts, kind, include_stale, limit) {
                Ok(found) if !found.is_empty() => candidates = found,
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "memory candidate FTS search failed; falling back to LIKE");
                }
            }
        }
        if candidates.is_empty() {
            candidates =
                self.memory_candidates_like(project_id, query, kind, include_stale, limit)?;
        }
        if !kinds.is_empty() {
            candidates.retain(|candidate| kinds.iter().any(|kind| kind == &candidate.memory.kind));
        }
        candidates.truncate(limit.clamp(1, 200) as usize);
        Ok(candidates)
    }

    pub(super) fn memory_candidates_fts(
        &self,
        project_id: &str,
        query: &str,
        kind: Option<&str>,
        include_stale: bool,
        limit: u32,
    ) -> Result<Vec<MemoryCandidate>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT m.id, m.session_id, m.project_id, m.agent_id, m.agent_name, m.kind,
                    m.title, m.body, m.tags, m.created_at, m.status, m.source, m.evidence,
                    m.supersedes, m.superseded_by, m.pinned, m.use_count, m.last_used_at,
                    m.last_verified_at, m.verify_after, m.stale_reason,
                    bm25(memories_fts, 3.0, 1.0, 2.0) AS rank
             FROM memories_fts
             JOIN memories m ON m.rowid = memories_fts.rowid
             WHERE memories_fts MATCH ?1
               AND m.project_id = ?2
               AND (?3 IS NULL OR m.kind = ?3)
               AND (m.status = 'active' OR (?4 = 1 AND m.status = 'stale'))
             ORDER BY rank ASC, m.created_at DESC, m.id ASC
             LIMIT ?5",
        )?;
        let rows = stmt.query_map(
            params![
                query,
                project_id,
                kind,
                if include_stale { 1 } else { 0 },
                limit.clamp(1, 200) as i64
            ],
            |row| {
                let rank: f64 = row.get(21)?;
                let relevance = (-rank).max(0.0);
                Ok(MemoryCandidate {
                    memory: map_memory_row(row)?,
                    lexical: if relevance <= 0.0 {
                        0.01
                    } else {
                        relevance / (1.0 + relevance)
                    },
                    fts: true,
                    semantic: None,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub(super) fn memory_candidates_like(
        &self,
        project_id: &str,
        query: &str,
        kind: Option<&str>,
        include_stale: bool,
        limit: u32,
    ) -> Result<Vec<MemoryCandidate>> {
        let conn = self.lock()?;
        let pattern = format!("%{}%", query.trim());
        let mut stmt = conn.prepare(
            "SELECT id, session_id, project_id, agent_id, agent_name, kind, title, body,
                    tags, created_at, status, source, evidence, supersedes, superseded_by,
                    pinned, use_count, last_used_at, last_verified_at, verify_after, stale_reason
             FROM memories m
             WHERE m.project_id = ?1
               AND (?2 = '%%' OR m.title LIKE ?2 OR m.body LIKE ?2 OR m.tags LIKE ?2)
               AND (?3 IS NULL OR m.kind = ?3)
               AND (m.status = 'active' OR (?4 = 1 AND m.status = 'stale'))
             ORDER BY m.created_at DESC, m.id ASC
             LIMIT ?5",
        )?;
        let rows = stmt.query_map(
            params![
                project_id,
                pattern,
                kind,
                if include_stale { 1 } else { 0 },
                limit.clamp(1, 200) as i64
            ],
            |row| {
                let memory = map_memory_row(row)?;
                Ok(MemoryCandidate {
                    lexical: like_memory_lexical(&memory, query),
                    memory,
                    fts: false,
                    semantic: None,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_memory(&self, id: &str) -> Result<Option<MemoryItem>> {
        let conn = self.lock()?;
        get_memory_tx(&conn, id)
    }

    /// Structural supplement for `memory.search`: active/stale rows whose task
    /// evidence or tag exactly matches `task_id`, even when the lexical query
    /// does not. This keeps task-specific memories eligible for the structural
    /// boost in ranking.
    pub fn memory_task_candidates(
        &self,
        project_id: &str,
        task_id: &str,
        include_stale: bool,
        limit: u32,
    ) -> Result<Vec<MemoryItem>> {
        let task_id = task_id.trim();
        if task_id.is_empty() {
            return Ok(Vec::new());
        }
        let pattern = format!("%{}%", escape_like(task_id));
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, session_id, project_id, agent_id, agent_name, kind, title, body,
                    tags, created_at, status, source, evidence, supersedes, superseded_by,
                    pinned, use_count, last_used_at, last_verified_at, verify_after, stale_reason
             FROM memories m
             WHERE m.project_id = ?1
               AND (m.status = 'active' OR (?2 = 1 AND m.status = 'stale'))
               AND (m.tags LIKE ?3 ESCAPE '\\' OR m.evidence LIKE ?3 ESCAPE '\\')
             ORDER BY m.created_at DESC, m.id ASC
             LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![
                project_id,
                if include_stale { 1 } else { 0 },
                pattern,
                limit.clamp(1, 200) as i64
            ],
            map_memory_row,
        )?;
        let mut candidates =
            rows.collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .filter(|memory| {
                    memory.evidence.iter().any(|evidence| {
                        evidence.kind == "task" && evidence.reference.trim() == task_id
                    }) || memory.tags.iter().any(|tag| tag.trim() == task_id)
                })
                .collect::<Vec<_>>();
        candidates.truncate(limit.clamp(1, 200) as usize);
        Ok(candidates)
    }

    pub fn list_session_memories(&self, session_id: &str) -> Result<Vec<MemoryItem>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, session_id, project_id, agent_id, agent_name, kind, title, body,
                    tags, created_at, status, source, evidence, supersedes, superseded_by,
                    pinned, use_count, last_used_at, last_verified_at, verify_after, stale_reason
             FROM memories WHERE session_id = ?1 ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![session_id], map_memory_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn replace_session_memories(
        &self,
        session_id: &str,
        memories: &[MemoryItem],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        replace_session_memories_tx(&tx, session_id, memories)?;
        tx.commit()?;
        Ok(())
    }

    pub fn replace_session_memory_conflicts(
        &self,
        session_id: &str,
        conflicts: &[MemoryConflict],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        replace_session_memory_conflicts_tx(&tx, session_id, conflicts)?;
        tx.commit()?;
        Ok(())
    }

    pub fn delete_memories_with_events(&self, ids: &[String]) -> Result<Vec<(MemoryItem, Event)>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let mut removed = Vec::new();
        for id in ids {
            let Some(memory) = get_memory_tx(&tx, id)? else {
                continue;
            };
            // A conflict cannot be resolved once either side is gone. The
            // `memory.deleted` event removes the memory from replay, and this
            // tombstone keeps the conflict projection consistent too.
            tx.execute(
                "DELETE FROM memory_conflicts WHERE memory_a_id = ?1 OR memory_b_id = ?1",
                params![id],
            )?;
            // Explicit cleanup keeps behavior correct even for databases opened
            // without foreign-key enforcement.
            tx.execute(
                "DELETE FROM memory_embeddings WHERE memory_id = ?1",
                params![id],
            )?;
            tx.execute(
                "DELETE FROM memory_embedding_jobs WHERE memory_id = ?1",
                params![id],
            )?;
            tx.execute(
                "DELETE FROM memory_embedding_assignments WHERE memory_id = ?1",
                params![id],
            )?;
            tx.execute("DELETE FROM memories WHERE id = ?1", params![id])?;
            let event = insert_event_tx(
                &tx,
                &memory.session_id,
                "memory.deleted",
                serde_json::to_value(&memory)?,
            )?;
            removed.push((memory, event));
        }
        tx.commit()?;
        drop(conn);
        for (_, event) in &removed {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok(removed)
    }

    /// Full-item update. `memory.updated` carries the entire updated row so
    /// replay is last-write-wins.
    pub fn update_memory_with_event(
        &self,
        id: &str,
        status: Option<&str>,
        pinned: Option<bool>,
        tags: Option<&[String]>,
    ) -> Result<Option<(MemoryItem, Event)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut memory) = get_memory_tx(&tx, id)? else {
            return Ok(None);
        };
        let previous_status = memory.status.clone();
        if let Some(status) = status {
            memory.status = status.to_string();
            if status == "stale" {
                if memory.stale_reason.is_none() {
                    memory.stale_reason = Some("operator marked stale".to_string());
                }
            } else {
                memory.stale_reason = None;
            }
        }
        if let Some(pinned) = pinned {
            memory.pinned = pinned;
        }
        if let Some(tags) = tags {
            memory.tags = tags.to_vec();
        }
        update_memory_projection_tx(&tx, &memory)?;
        let reactivated = memory.status == "active"
            && matches!(previous_status.as_str(), "archived" | "superseded");
        if tags.is_some() || reactivated {
            enqueue_memory_embedding_job_tx(&tx, &memory)?;
        }
        let event = insert_event_tx(
            &tx,
            &memory.session_id,
            "memory.updated",
            serde_json::to_value(&memory)?,
        )?;
        tx.commit()?;
        drop(conn);
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((memory, event)))
    }

    pub fn mark_memory_verified_with_event(
        &self,
        id: &str,
        verified_at: &str,
        verify_after: Option<&str>,
    ) -> Result<Option<(MemoryItem, Event)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut memory) = get_memory_tx(&tx, id)? else {
            return Ok(None);
        };
        memory.status = "active".to_string();
        memory.stale_reason = None;
        memory.last_verified_at = Some(verified_at.to_string());
        memory.verify_after = verify_after.map(str::to_string);
        update_memory_projection_tx(&tx, &memory)?;
        let event = insert_event_tx(
            &tx,
            &memory.session_id,
            "memory.verified",
            serde_json::to_value(&memory)?,
        )?;
        tx.commit()?;
        drop(conn);
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((memory, event)))
    }

    pub fn mark_memory_stale_with_event(
        &self,
        id: &str,
        reason: &str,
    ) -> Result<Option<(MemoryItem, Event)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut memory) = get_memory_tx(&tx, id)? else {
            return Ok(None);
        };
        memory.status = "stale".to_string();
        memory.stale_reason = Some(reason.to_string());
        update_memory_projection_tx(&tx, &memory)?;
        let event = insert_event_tx(
            &tx,
            &memory.session_id,
            "memory.stale",
            serde_json::to_value(&memory)?,
        )?;
        tx.commit()?;
        drop(conn);
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((memory, event)))
    }

    /// Observed usage. Idempotent per (memory, session): replaying the same
    /// selection never increments `use_count` twice.
    pub fn record_memory_use_with_event(
        &self,
        id: &str,
        using_session_id: &str,
    ) -> Result<Option<(MemoryItem, Event)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut memory) = get_memory_tx(&tx, id)? else {
            return Ok(None);
        };
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO memory_usage (memory_id, session_id, used_at)
             VALUES (?1, ?2, ?3)",
            params![id, using_session_id, now_rfc3339()],
        )?;
        if inserted == 0 {
            tx.commit()?;
            return Ok(None);
        }
        memory.use_count = memory.use_count.saturating_add(1);
        memory.last_used_at = Some(now_rfc3339());
        update_memory_projection_tx(&tx, &memory)?;
        let event = insert_event_tx(
            &tx,
            &memory.session_id,
            "memory.used",
            serde_json::to_value(&memory)?,
        )?;
        tx.commit()?;
        drop(conn);
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((memory, event)))
    }

    /// Active, non-pinned rows whose explicit TTL expired, or (when the
    /// override is supplied) rows older than the cutoff.
    pub fn memory_sweep_candidates(
        &self,
        project_id: &str,
        older_than_days: Option<u32>,
    ) -> Result<Vec<MemoryItem>> {
        let now = time::OffsetDateTime::now_utc();
        let cutoff = older_than_days.map(|days| now - time::Duration::days(i64::from(days)));
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, session_id, project_id, agent_id, agent_name, kind, title, body,
                    tags, created_at, status, source, evidence, supersedes, superseded_by,
                    pinned, use_count, last_used_at, last_verified_at, verify_after, stale_reason
             FROM memories
             WHERE project_id = ?1 AND status = 'active' AND pinned = 0
             ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![project_id], map_memory_row)?;
        let mut candidates = Vec::new();
        for row in rows {
            let memory = row?;
            let expired = memory.verify_after.as_deref().is_some_and(|verify_after| {
                parse_rfc3339(verify_after).is_some_and(|verify_after| verify_after < now)
            });
            let too_old = cutoff.is_some_and(|cutoff| {
                parse_rfc3339(&memory.created_at).is_some_and(|created_at| created_at < cutoff)
            });
            if expired || too_old {
                candidates.push(memory);
            }
        }
        Ok(candidates)
    }

    pub fn memory_stats(&self, project_id: &str) -> Result<MemoryStatsSnapshot> {
        let conn = self.lock()?;
        let total = conn.query_row(
            "SELECT COUNT(*) FROM memories WHERE project_id = ?1",
            params![project_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(MemoryStatsSnapshot {
            total: u32::try_from(total).unwrap_or(u32::MAX),
            by_status: memory_count_group(&conn, project_id, "status")?,
            by_source: memory_count_group(&conn, project_id, "source")?,
            by_kind: memory_count_group(&conn, project_id, "kind")?,
        })
    }
}

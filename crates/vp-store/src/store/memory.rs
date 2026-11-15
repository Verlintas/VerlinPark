//! Project memory, conflicts, and embedding/IVF indexes.

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

    /// List deterministic memory conflicts. `status` filters exactly (`open` or
    /// `resolved`); without a limit the newest 500 are returned.
    pub fn list_memory_conflicts(
        &self,
        project_id: &str,
        status: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<MemoryConflict>> {
        let conn = self.lock()?;
        list_memory_conflicts_conn(&conn, project_id, status, limit)
    }

    pub fn get_memory_conflict(&self, id: &str) -> Result<Option<MemoryConflict>> {
        let conn = self.lock()?;
        get_memory_conflict_tx(&conn, id)
    }

    pub fn list_session_memory_conflicts(&self, session_id: &str) -> Result<Vec<MemoryConflict>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {MEMORY_CONFLICT_COLUMNS}
             FROM memory_conflicts WHERE session_id = ?1
             ORDER BY scope_kind ASC, scope_value ASC, memory_a_id ASC, memory_b_id ASC,
                      detected_at DESC, id ASC"
        ))?;
        let rows = stmt.query_map(params![session_id], map_memory_conflict_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Detect deterministic scope conflicts. New rows are inserted as `open`
    /// with one `memory.conflict.detected` event in the same transaction. An
    /// existing id (including a resolved row) is never recreated. Detection
    /// never mutates either memory.
    pub fn scan_memory_conflicts(&self, project_id: &str) -> Result<MemoryConflictScan> {
        let detected_at = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let memories = active_memories_with_evidence_tx(&tx, project_id, MAX_CONFLICT_MEMORIES)?;
        let candidates = detect_memory_conflicts(project_id, &memories, &detected_at);
        let mut scan = MemoryConflictScan::default();
        for conflict in candidates {
            let inserted = tx.execute(
                "INSERT OR IGNORE INTO memory_conflicts
                 (id, project_id, session_id, memory_a_id, memory_b_id, memory_a_title,
                  memory_b_title, memory_a_kind, memory_b_kind, memory_a_excerpt,
                  memory_b_excerpt, scope_kind, scope_value, reason, status, detected_at,
                  resolved_at, resolution, note)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                         ?16, ?17, ?18, ?19)",
                params![
                    &conflict.id,
                    &conflict.project_id,
                    &conflict.session_id,
                    &conflict.memory_a_id,
                    &conflict.memory_b_id,
                    &conflict.memory_a_title,
                    &conflict.memory_b_title,
                    &conflict.memory_a_kind,
                    &conflict.memory_b_kind,
                    &conflict.memory_a_excerpt,
                    &conflict.memory_b_excerpt,
                    &conflict.scope_kind,
                    &conflict.scope_value,
                    &conflict.reason,
                    &conflict.status,
                    &conflict.detected_at,
                    &conflict.resolved_at,
                    &conflict.resolution,
                    &conflict.note,
                ],
            )?;
            if inserted == 0 {
                continue;
            }
            let event = insert_event_tx(
                &tx,
                &conflict.session_id,
                "memory.conflict.detected",
                serde_json::to_value(&conflict)?,
            )?;
            scan.new.push(conflict);
            scan.events.push(event);
        }
        scan.open = list_memory_conflicts_conn(&tx, project_id, Some("open"), None)?;
        tx.commit()?;
        drop(conn);
        for event in &scan.events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok(scan)
    }

    /// Resolve one conflict explicitly. The memory mutation, the conflict row,
    /// and every event commit atomically. `keep_both` changes no memory.
    pub fn resolve_memory_conflict(
        &self,
        project_id: &str,
        id: &str,
        action: &str,
        note: Option<&str>,
    ) -> Result<MemoryConflictResolution> {
        if !matches!(action, "keep_a" | "keep_b" | "keep_both" | "archive_both") {
            return Err(anyhow!(
                "memory conflict action must be keep_a, keep_b, keep_both, or archive_both"
            ));
        }
        let note = note
            .map(str::trim)
            .filter(|note| !note.is_empty())
            .map(str::to_string);
        if note
            .as_deref()
            .is_some_and(|note| note.chars().count() > 2_000)
        {
            return Err(anyhow!(
                "memory conflict note is limited to 2000 characters"
            ));
        }
        let resolved_at = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut conflict) = get_memory_conflict_tx(&tx, id)? else {
            return Err(anyhow!(
                "memory conflict {id} not found: memory no longer exists or the conflict was removed with a deleted memory"
            ));
        };
        if conflict.project_id != project_id {
            return Err(anyhow!(
                "memory conflict {id} belongs to project {}, not {project_id}",
                conflict.project_id
            ));
        }
        if conflict.status == "resolved" {
            return Err(anyhow!("memory conflict {id} is already resolved"));
        }
        let Some(mut memory_a) = get_memory_tx(&tx, &conflict.memory_a_id)? else {
            return Err(anyhow!(
                "memory conflict {id} references memory {} which no longer exists",
                conflict.memory_a_id
            ));
        };
        let Some(mut memory_b) = get_memory_tx(&tx, &conflict.memory_b_id)? else {
            return Err(anyhow!(
                "memory conflict {id} references memory {} which no longer exists",
                conflict.memory_b_id
            ));
        };
        if memory_a.project_id != project_id || memory_b.project_id != project_id {
            return Err(anyhow!(
                "memory conflict {id} references memories from a different project"
            ));
        }

        let mut events = Vec::new();
        match action {
            "keep_a" => {
                memory_b.status = "superseded".to_string();
                memory_b.superseded_by = Some(memory_a.id.clone());
                update_memory_projection_tx(&tx, &memory_b)?;
                events.push(insert_event_tx(
                    &tx,
                    &memory_b.session_id,
                    "memory.superseded",
                    serde_json::to_value(&memory_b)?,
                )?);
            }
            "keep_b" => {
                memory_a.status = "superseded".to_string();
                memory_a.superseded_by = Some(memory_b.id.clone());
                update_memory_projection_tx(&tx, &memory_a)?;
                events.push(insert_event_tx(
                    &tx,
                    &memory_a.session_id,
                    "memory.superseded",
                    serde_json::to_value(&memory_a)?,
                )?);
            }
            "archive_both" => {
                for memory in [&mut memory_a, &mut memory_b] {
                    memory.status = "archived".to_string();
                    update_memory_projection_tx(&tx, memory)?;
                    events.push(insert_event_tx(
                        &tx,
                        &memory.session_id,
                        "memory.updated",
                        serde_json::to_value(&*memory)?,
                    )?);
                }
            }
            "keep_both" => {}
            _ => unreachable!("validated above"),
        }

        conflict.status = "resolved".to_string();
        conflict.resolved_at = Some(resolved_at);
        conflict.resolution = Some(action.to_string());
        conflict.note = note;
        update_memory_conflict_tx(&tx, &conflict)?;
        events.push(insert_event_tx(
            &tx,
            &conflict.session_id,
            "memory.conflict.resolved",
            serde_json::to_value(&conflict)?,
        )?);

        tx.commit()?;
        drop(conn);
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok(MemoryConflictResolution {
            conflict,
            memories: vec![memory_a, memory_b],
            events,
        })
    }

    /// Operator-facing health snapshot. `verification_due` counts active,
    /// non-pinned rows whose explicit TTL has elapsed.
    pub fn memory_health(&self, project_id: &str) -> Result<MemoryHealthResult> {
        let now = time::OffsetDateTime::now_utc();
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT kind, status, evidence, pinned, last_verified_at, verify_after
             FROM memories WHERE project_id = ?1 ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![project_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)? != 0,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?;
        let mut health = MemoryHealthResult {
            project_id: project_id.to_string(),
            total: 0,
            active: 0,
            stale: 0,
            superseded: 0,
            archived: 0,
            pinned: 0,
            evidence_covered: 0,
            verified: 0,
            verification_due: 0,
            open_conflicts: 0,
            by_kind: Vec::new(),
        };
        let mut kinds: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
        for row in rows {
            let (kind, status, evidence, pinned, last_verified_at, verify_after) = row?;
            health.total += 1;
            match status.as_str() {
                "active" => health.active += 1,
                "stale" => health.stale += 1,
                "superseded" => health.superseded += 1,
                "archived" => health.archived += 1,
                _ => {}
            }
            if pinned {
                health.pinned += 1;
            }
            if status == "active"
                && !serde_json::from_str::<Vec<MemoryEvidence>>(&evidence)
                    .unwrap_or_default()
                    .is_empty()
            {
                health.evidence_covered += 1;
            }
            if last_verified_at.is_some() {
                health.verified += 1;
            }
            if status == "active"
                && !pinned
                && verify_after.as_deref().is_some_and(|verify_after| {
                    parse_rfc3339(verify_after).is_some_and(|verify_after| verify_after < now)
                })
            {
                health.verification_due += 1;
            }
            *kinds.entry(kind).or_insert(0) += 1;
        }
        health.by_kind = kinds
            .into_iter()
            .map(|(key, count)| MemoryStatCount { key, count })
            .collect();
        let open_conflicts = conn.query_row(
            "SELECT COUNT(*) FROM memory_conflicts WHERE project_id = ?1 AND status = 'open'",
            params![project_id],
            |row| row.get::<_, i64>(0),
        )?;
        health.open_conflicts = u32::try_from(open_conflicts).unwrap_or(u32::MAX);
        Ok(health)
    }

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

    pub fn get_memory_policy(&self, project_id: &str) -> Result<MemoryPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT enabled, max_items, max_age_days, task_result_verify_days,
                        default_verify_days, auto_supersede, brief_max_chars, sweep_stale,
                        embedding_json
                 FROM project_memory_policies WHERE project_id = ?1",
                params![project_id],
                map_memory_policy_row,
            )
            .optional()?;
        Ok(stored.unwrap_or_default())
    }

    pub fn set_memory_policy(&self, project_id: &str, policy: &MemoryPolicy) -> Result<()> {
        let conn = self.lock()?;
        let embedding_json = match &policy.embedding {
            Some(embedding) => Some(serde_json::to_string(embedding)?),
            None => None,
        };
        conn.execute(
            "INSERT INTO project_memory_policies
             (project_id, enabled, max_items, max_age_days, task_result_verify_days,
              default_verify_days, auto_supersede, brief_max_chars, sweep_stale,
              embedding_json, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(project_id) DO UPDATE SET
                enabled = excluded.enabled,
                max_items = excluded.max_items,
                max_age_days = excluded.max_age_days,
                task_result_verify_days = excluded.task_result_verify_days,
                default_verify_days = excluded.default_verify_days,
                auto_supersede = excluded.auto_supersede,
                brief_max_chars = excluded.brief_max_chars,
                sweep_stale = excluded.sweep_stale,
                embedding_json = excluded.embedding_json,
                updated_at = excluded.updated_at",
            params![
                project_id,
                if policy.enabled { 1 } else { 0 },
                policy.max_items.map(i64::from),
                policy.max_age_days.map(i64::from),
                optional_count_to_db(policy.task_result_verify_days),
                optional_count_to_db(policy.default_verify_days),
                if policy.auto_supersede { 1 } else { 0 },
                optional_count_to_db(policy.brief_max_chars),
                if policy.sweep_stale { 1 } else { 0 },
                embedding_json,
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn list_enabled_memory_policies(&self) -> Result<Vec<(String, MemoryPolicy)>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT project_id, enabled, max_items, max_age_days, task_result_verify_days,
                    default_verify_days, auto_supersede, brief_max_chars, sweep_stale,
                    embedding_json
             FROM project_memory_policies WHERE enabled = 1",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                MemoryPolicy {
                    enabled: row.get::<_, i64>(1)? != 0,
                    max_items: row
                        .get::<_, Option<i64>>(2)?
                        .and_then(|value| u32::try_from(value).ok()),
                    max_age_days: row
                        .get::<_, Option<i64>>(3)?
                        .and_then(|value| u32::try_from(value).ok()),
                    task_result_verify_days: optional_count_from_db(row.get::<_, i64>(4)?),
                    default_verify_days: optional_count_from_db(row.get::<_, i64>(5)?),
                    auto_supersede: row.get::<_, i64>(6)? != 0,
                    brief_max_chars: optional_count_from_db(row.get::<_, i64>(7)?),
                    sweep_stale: row.get::<_, i64>(8)? != 0,
                    embedding: decode_memory_embedding_policy(row.get::<_, Option<String>>(9)?),
                },
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Projects that opted into daemon staleness sweeping. Independent of the
    /// retention (`enabled`) toggle.
    pub fn list_memory_sweep_policies(&self) -> Result<Vec<(String, MemoryPolicy)>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT project_id, enabled, max_items, max_age_days, task_result_verify_days,
                    default_verify_days, auto_supersede, brief_max_chars, sweep_stale,
                    embedding_json
             FROM project_memory_policies WHERE sweep_stale = 1",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                MemoryPolicy {
                    enabled: row.get::<_, i64>(1)? != 0,
                    max_items: row
                        .get::<_, Option<i64>>(2)?
                        .and_then(|value| u32::try_from(value).ok())
                        .filter(|value| *value > 0),
                    max_age_days: row
                        .get::<_, Option<i64>>(3)?
                        .and_then(|value| u32::try_from(value).ok())
                        .filter(|value| *value > 0),
                    task_result_verify_days: optional_count_from_db(row.get::<_, i64>(4)?),
                    default_verify_days: optional_count_from_db(row.get::<_, i64>(5)?),
                    auto_supersede: row.get::<_, i64>(6)? != 0,
                    brief_max_chars: optional_count_from_db(row.get::<_, i64>(7)?),
                    sweep_stale: row.get::<_, i64>(8)? != 0,
                    embedding: decode_memory_embedding_policy(row.get::<_, Option<String>>(9)?),
                },
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn memory_ids_for_prune(
        &self,
        project_id: &str,
        older_than_days: Option<u32>,
        keep_latest: Option<u32>,
    ) -> Result<Vec<String>> {
        if older_than_days.is_none() && keep_latest.is_none() {
            return Ok(Vec::new());
        }
        let cutoff = older_than_days
            .map(|days| time::OffsetDateTime::now_utc() - time::Duration::days(i64::from(days)));
        let conn = self.lock()?;
        // Pinning is explicit operator intent to retain a memory. Prune must
        // never silently delete a pinned row, and `keep_latest` counts only
        // unpinned rows so pinning one item cannot push another pinned item
        // out of the budget.
        let mut stmt = conn.prepare(
            "SELECT id, created_at FROM memories
             WHERE project_id = ?1 AND pinned = 0 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(params![project_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut ids = Vec::new();
        for (index, row) in rows.enumerate() {
            let (id, created_at) = row?;
            let beyond_keep = keep_latest.is_some_and(|keep| index >= keep as usize);
            let old = cutoff.is_some_and(|cutoff| {
                parse_rfc3339(&created_at).is_some_and(|created_at| created_at < cutoff)
            });
            if beyond_keep || old {
                ids.push(id);
            }
        }
        Ok(ids)
    }
}

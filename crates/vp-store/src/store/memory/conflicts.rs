use super::*;

impl Store {
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
}

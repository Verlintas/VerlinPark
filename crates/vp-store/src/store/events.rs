//! Event append, mirror, queries, search, and store metrics.

use super::*;

impl Store {
    pub fn append_event(
        &self,
        session_id: &str,
        kind: &str,
        payload: serde_json::Value,
    ) -> Result<Event> {
        if self.get_session(session_id)?.is_none() {
            return Err(anyhow!("session not found: {session_id}"));
        }

        let event = {
            let mut conn = self.lock()?;
            let tx = conn.transaction()?;
            let event = insert_event_tx(&tx, session_id, kind, payload)?;
            tx.commit()?;
            event
        };

        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(event)
    }

    pub(super) fn append_event_mirror(&self, event: &Event) -> Result<()> {
        validate_storage_id("session", &event.session_id)?;
        let dir = self.data_dir.join("sessions").join(&event.session_id);
        paths::ensure_private_dir(&dir)?;
        let path = dir.join("events.jsonl");
        let mut line = serde_json::to_vec(event)?;
        line.push(b'\n');
        let _guard = self
            .mirror_lock
            .lock()
            .map_err(|_| anyhow!("event mirror mutex poisoned"))?;
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        file.write_all(&line)?;
        file.flush()?;
        Ok(())
    }

    pub fn read_event_mirror(&self, session_id: &str) -> Result<Option<Vec<Event>>> {
        validate_storage_id("session", session_id)?;
        let path = self
            .data_dir
            .join("sessions")
            .join(session_id)
            .join("events.jsonl");
        if !path.is_file() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("reading event mirror {}", path.display()))?;
        let mut events = Vec::new();
        for (index, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let event = serde_json::from_str::<Event>(line).with_context(|| {
                format!("parsing event mirror {} line {}", path.display(), index + 1)
            })?;
            events.push(event);
        }
        // Concurrent writers may commit SQLite in a different order than the
        // mirror append lock serializes lines. The event sequence is the
        // canonical order, so normalize on read before log-status/restore.
        events.sort_by_key(|event| event.seq);
        Ok(Some(events))
    }

    pub fn replace_events(&self, session_id: &str, events: &[Event]) -> Result<()> {
        if events.iter().any(|event| event.session_id != session_id) {
            return Err(anyhow!(
                "event mirror contains an event for a different session"
            ));
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM events WHERE session_id = ?1",
            params![session_id],
        )?;
        for event in events {
            tx.execute(
                "INSERT INTO events (session_id, seq, type, payload, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    event.session_id,
                    event.seq as i64,
                    event.kind,
                    serde_json::to_string(&event.payload)?,
                    event.created_at
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn latest_event_seq(&self, session_id: &str, kind: &str) -> Result<u64> {
        let conn = self.lock()?;
        let seq = conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM events WHERE session_id = ?1 AND type = ?2",
            params![session_id, kind],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(seq.max(0) as u64)
    }

    /// Newest event sequence for a session, used as a cheap cache key for
    /// derived snapshots. Returns zero for a session with no events.
    pub fn latest_session_seq(&self, session_id: &str) -> Result<u64> {
        let conn = self.lock()?;
        let seq = conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM events WHERE session_id = ?1",
            params![session_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(seq.max(0) as u64)
    }

    /// Latest events for a session in ascending order. Used by coordination
    /// views/tools that need the recent board without loading full history.
    pub fn recent_events(&self, session_id: &str, limit: u32) -> Result<Vec<Event>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT session_id, seq, type, payload, created_at
             FROM events WHERE session_id = ?1
             ORDER BY seq DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![session_id, limit.clamp(1, 2_000)], |row| {
            let payload: String = row.get(3)?;
            Ok(Event {
                session_id: row.get(0)?,
                seq: row.get::<_, i64>(1)? as u64,
                kind: row.get(2)?,
                payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
                created_at: row.get(4)?,
            })
        })?;
        let mut events = rows.collect::<Result<Vec<_>, _>>()?;
        events.reverse();
        Ok(events)
    }

    /// Newest bounded event window in ascending sequence order. This is used
    /// by read-only diagnosis, which must never load an unbounded session log.
    /// The payload is the same JSON already stored for mirror/replay; callers
    /// read only allowlisted structural fields.
    pub fn list_recent_events_window(&self, session_id: &str, limit: u32) -> Result<Vec<Event>> {
        // Read payload strings under the global SQLite lock, then decode after
        // releasing it. Diagnosis/replay windows can be tens of thousands of
        // rows; parsing them while holding the lock stalled every other session.
        let rows = {
            let conn = self.lock()?;
            let mut statement = conn.prepare(
                "SELECT session_id, seq, type, payload, created_at
                 FROM events WHERE session_id = ?1
                 ORDER BY seq DESC LIMIT ?2",
            )?;
            statement
                .query_map(
                    params![session_id, i64::from(limit.clamp(1, 50_000))],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                        ))
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut events = rows
            .into_iter()
            .map(|(session_id, seq, kind, payload, created_at)| Event {
                session_id,
                seq: seq.max(0) as u64,
                kind,
                payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
                created_at,
            })
            .collect::<Vec<_>>();
        events.reverse();
        Ok(events)
    }

    /// Total event rows for a session. Diagnosis compares this with the
    /// returned bounded window so truncation is visible in the report.
    pub fn event_count(&self, session_id: &str) -> Result<u64> {
        let conn = self.lock()?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM events WHERE session_id = ?1",
            params![session_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count.max(0) as u64)
    }

    /// Bounded GUI action timeline. Entries are derived from `gui.action`
    /// events; only action/target/generation/latency/result metadata is
    /// stored. Screen content and typed text are never included.
    pub fn recent_gui_actions(
        &self,
        session_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<GuiActionInfo>> {
        let limit = limit.clamp(1, 500);
        let conn = self.lock()?;
        let mut actions = Vec::new();
        match session_id {
            Some(session_id) => {
                let mut statement = conn.prepare(
                    "SELECT payload FROM events
                     WHERE session_id = ?1 AND type = 'gui.action'
                     ORDER BY seq DESC LIMIT ?2",
                )?;
                let rows = statement
                    .query_map(params![session_id, limit], |row| row.get::<_, String>(0))?;
                for payload in rows {
                    if let Ok(action) = serde_json::from_str::<GuiActionInfo>(&payload?) {
                        actions.push(action);
                    }
                }
            }
            None => {
                let mut statement = conn.prepare(
                    "SELECT payload FROM events
                     WHERE type = 'gui.action'
                     ORDER BY julianday(created_at) DESC, rowid DESC LIMIT ?1",
                )?;
                let rows = statement.query_map(params![limit], |row| row.get::<_, String>(0))?;
                for payload in rows {
                    if let Ok(action) = serde_json::from_str::<GuiActionInfo>(&payload?) {
                        actions.push(action);
                    }
                }
            }
        }
        actions.reverse();
        Ok(actions)
    }

    /// Count `gui.action` rows and how many are strictly older than `cutoff`.
    /// The query joins sessions only for project scoping; no payload is read.
    pub fn gui_action_retention_stats(
        &self,
        project_id: &str,
        cutoff: Option<&str>,
    ) -> Result<GuiActionRetentionStats> {
        let conn = self.lock()?;
        let (total, oldest_at, newest_at): (i64, Option<String>, Option<String>) = conn.query_row(
            "SELECT COUNT(*), MIN(e.created_at), MAX(e.created_at)
                 FROM events e JOIN sessions s ON s.id = e.session_id
                 WHERE s.project_id = ?1 AND e.type = 'gui.action'",
            params![project_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let eligible = match cutoff {
            Some(cutoff) => conn.query_row(
                "SELECT COUNT(*)
                 FROM events e JOIN sessions s ON s.id = e.session_id
                 WHERE s.project_id = ?1 AND e.type = 'gui.action'
                   AND julianday(e.created_at) < julianday(?2)",
                params![project_id, cutoff],
                |row| row.get::<_, i64>(0),
            )?,
            None => 0,
        };
        Ok(GuiActionRetentionStats {
            total: total.max(0) as u64,
            eligible: eligible.max(0) as u64,
            oldest_at,
            newest_at,
        })
    }

    /// Bounded audit rows for `gui.timeline_pruned` events. Only the count,
    /// cutoff, and retention days are returned; no pruned payload is read.
    pub fn gui_timeline_prune_audit(
        &self,
        project_id: &str,
        limit: u32,
    ) -> Result<(Vec<vp_protocol::types::GuiTimelinePrunedInfo>, u64)> {
        let limit = limit.clamp(1, 1_000);
        let conn = self.lock()?;
        let total: i64 = conn.query_row(
            "SELECT COUNT(*)
             FROM events e JOIN sessions s ON s.id = e.session_id
             WHERE s.project_id = ?1 AND e.type = 'gui.timeline_pruned'",
            params![project_id],
            |row| row.get(0),
        )?;
        let mut statement = conn.prepare(
            "SELECT e.session_id, e.seq, e.payload, e.created_at
             FROM events e JOIN sessions s ON s.id = e.session_id
             WHERE s.project_id = ?1 AND e.type = 'gui.timeline_pruned'
             ORDER BY e.created_at DESC, e.seq DESC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![project_id, i64::from(limit)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut entries = Vec::new();
        for row in rows {
            let (session_id, seq, payload, created_at) = row?;
            let payload: serde_json::Value = serde_json::from_str(&payload).unwrap_or_default();
            entries.push(vp_protocol::types::GuiTimelinePrunedInfo {
                session_id,
                seq: seq.max(0) as u64,
                retention_days: payload
                    .get("retention_days")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0)
                    .min(u64::from(u32::MAX)) as u32,
                cutoff: payload
                    .get("cutoff")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                pruned: payload
                    .get("pruned")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                created_at,
            });
        }
        Ok((entries, total.max(0) as u64))
    }

    /// Bounded, audited retention prune for the GUI action timeline.
    ///
    /// Only `events.type = 'gui.action'` rows are eligible. A per-session JSONL
    /// mirror is rewritten in the same critical section as the SQLite delete
    /// so `session.log_status` does not report drift from normal retention. A
    /// session whose mirror cannot be read or atomically rewritten is skipped
    /// before deletion: retention never guesses. The appended
    /// `gui.timeline_pruned` audit event is the only new event and carries
    /// counts, a cutoff, and the project id, never screen content.
    pub fn prune_gui_action_events(
        &self,
        project_id: &str,
        cutoff: &str,
        retention_days: u32,
        limit: u32,
        dry_run: bool,
    ) -> Result<GuiTimelinePruneOutcome> {
        let limit = limit.clamp(1, 10_000);
        let cutoff_parsed = OffsetDateTime::parse(cutoff, &Rfc3339)
            .with_context(|| format!("invalid GUI retention cutoff: {cutoff}"))?;
        let cutoff = cutoff_parsed
            .format(&Rfc3339)
            .context("formatting GUI retention cutoff")?;
        let conn = self.lock()?;
        let mut candidate_statement = conn.prepare(
            "SELECT e.session_id, e.seq
             FROM events e JOIN sessions s ON s.id = e.session_id
             WHERE s.project_id = ?1 AND e.type = 'gui.action'
               AND julianday(e.created_at) < julianday(?2)
             ORDER BY e.created_at ASC, e.session_id ASC, e.seq ASC
             LIMIT ?3",
        )?;
        let candidates = candidate_statement
            .query_map(params![project_id, cutoff, i64::from(limit)], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(candidate_statement);
        let total_eligible: i64 = conn.query_row(
            "SELECT COUNT(*)
             FROM events e JOIN sessions s ON s.id = e.session_id
             WHERE s.project_id = ?1 AND e.type = 'gui.action'
               AND julianday(e.created_at) < julianday(?2)",
            params![project_id, cutoff],
            |row| row.get(0),
        )?;
        let remaining_before: i64 = conn.query_row(
            "SELECT COUNT(*)
             FROM events e JOIN sessions s ON s.id = e.session_id
             WHERE s.project_id = ?1 AND e.type = 'gui.action'",
            params![project_id],
            |row| row.get(0),
        )?;
        drop(conn);
        let truncated = total_eligible > candidates.len() as i64;
        if dry_run {
            let mut sessions = candidates
                .iter()
                .map(|(session, _)| session.clone())
                .collect::<Vec<_>>();
            sessions.sort();
            sessions.dedup();
            return Ok(GuiTimelinePruneOutcome {
                project_id: project_id.to_string(),
                retention_days,
                cutoff,
                dry_run: true,
                pruned: candidates.len() as u64,
                remaining: remaining_before.max(0) as u64,
                sessions: sessions.len() as u64,
                skipped_sessions: 0,
                skipped_events: 0,
                other_kinds_touched: 0,
                limit,
                truncated,
                reason: "dry run: no event or mirror was changed".to_string(),
                affected_sessions: sessions.into_iter().take(32).collect(),
            });
        }
        let mut grouped: std::collections::BTreeMap<String, Vec<u64>> =
            std::collections::BTreeMap::new();
        for (session_id, seq) in candidates {
            grouped.entry(session_id).or_default().push(seq);
        }
        let mut pruned = 0u64;
        let mut skipped_sessions = 0u64;
        let mut skipped_events = 0u64;
        let mut affected = Vec::new();
        for (session_id, seqs) in grouped {
            match self.prune_gui_actions_for_session(
                &session_id,
                &seqs,
                project_id,
                &cutoff,
                retention_days,
            ) {
                Ok(count) => {
                    pruned += count;
                    if count > 0 {
                        affected.push(session_id);
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, session_id = %session_id, "GUI timeline prune skipped a session");
                    skipped_sessions += 1;
                    skipped_events += seqs.len() as u64;
                }
            }
        }
        let remaining = remaining_before.saturating_sub(pruned as i64).max(0) as u64;
        affected.sort();
        Ok(GuiTimelinePruneOutcome {
            project_id: project_id.to_string(),
            retention_days,
            cutoff,
            dry_run: false,
            pruned,
            remaining,
            sessions: affected.len() as u64,
            skipped_sessions,
            skipped_events,
            other_kinds_touched: 0,
            limit,
            truncated,
            reason: "gui.action retention prune".to_string(),
            affected_sessions: affected.into_iter().take(32).collect(),
        })
    }

    pub(super) fn prune_gui_actions_for_session(
        &self,
        session_id: &str,
        seqs: &[u64],
        project_id: &str,
        cutoff: &str,
        retention_days: u32,
    ) -> Result<u64> {
        if seqs.is_empty() {
            return Ok(0);
        }
        validate_storage_id("session", session_id)?;
        let mirror_path = self
            .data_dir
            .join("sessions")
            .join(session_id)
            .join("events.jsonl");
        // Lock order is database then mirror, matching every append path that
        // commits an event and then appends its mirror line. Reversing it here
        // would deadlock against a concurrent lifecycle event.
        let mut conn = self.lock()?;
        let _guard = self
            .mirror_lock
            .lock()
            .map_err(|_| anyhow!("event mirror mutex poisoned"))?;
        let mirror_text = std::fs::read_to_string(&mirror_path).ok();
        let mirror_present = mirror_text.is_some();
        let mut mirror_events = match &mirror_text {
            Some(text) => {
                let mut events = Vec::new();
                for line in text.lines() {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    let event = serde_json::from_str::<Event>(line)
                        .with_context(|| format!("parsing event mirror {session_id}"))?;
                    events.push(event);
                }
                events.sort_by_key(|event| event.seq);
                events
            }
            None => Vec::new(),
        };
        let removed = seqs
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        mirror_events.retain(|event| !(event.kind == "gui.action" && removed.contains(&event.seq)));

        let tx = conn.transaction()?;
        let mut removed_rows = Vec::new();
        for seq in seqs {
            let row = tx
                .query_row(
                    "SELECT payload, created_at FROM events
                     WHERE session_id = ?1 AND seq = ?2 AND type = 'gui.action'",
                    params![session_id, *seq as i64],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?;
            let Some((payload, created_at)) = row else {
                continue;
            };
            removed_rows.push((*seq, payload, created_at));
        }
        if removed_rows.is_empty() {
            tx.rollback()?;
            return Ok(0);
        }
        let audit = insert_event_tx(
            &tx,
            session_id,
            "gui.timeline_pruned",
            serde_json::json!({
                "project_id": project_id,
                "retention_days": retention_days,
                "cutoff": cutoff,
                "pruned": removed_rows.len() as u64,
                "kind": "gui.action",
            }),
        )?;
        for (seq, _, _) in &removed_rows {
            tx.execute(
                "DELETE FROM events
                 WHERE session_id = ?1 AND seq = ?2 AND type = 'gui.action'",
                params![session_id, *seq as i64],
            )?;
        }
        mirror_events.push(audit.clone());
        mirror_events.sort_by_key(|event| event.seq);
        let new_mirror = mirror_events
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()?
            .join("\n")
            + "\n";
        let previous_mirror = mirror_text;
        let rewrite = (|| -> Result<()> {
            if !mirror_present {
                // Imported/legacy sessions may have no JSONL mirror. Retention
                // still prunes the authoritative table; it never fabricates a
                // partial mirror that would look complete.
                return Ok(());
            }
            let dir = mirror_path
                .parent()
                .ok_or_else(|| anyhow!("event mirror path has no parent"))?;
            paths::ensure_private_dir(dir)?;
            let temp = dir.join(format!(".events.jsonl.prune-{}", Uuid::now_v7().simple()));
            {
                let mut file = OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&temp)
                    .context("creating pruned event mirror")?;
                file.write_all(new_mirror.as_bytes())?;
                file.flush()?;
            }
            paths::ensure_private_file(&temp)?;
            std::fs::rename(&temp, &mirror_path)
                .with_context(|| format!("replacing event mirror {}", mirror_path.display()))?;
            Ok(())
        })();
        if let Err(error) = rewrite {
            tx.rollback()?;
            return Err(error);
        }
        match tx.commit() {
            Ok(()) => Ok(removed_rows.len() as u64),
            Err(error) => {
                // Best-effort restore: the mirror rename happened first, so a
                // failed commit must not leave a mirror that is missing rows
                // the database still retains.
                if let Some(previous) = previous_mirror {
                    let _ = std::fs::write(&mirror_path, previous);
                }
                Err(error.into())
            }
        }
    }

    /// Newest events of one kind, oldest-first in the returned vector. Used by
    /// bounded cache telemetry, where the latest call is what matters.
    pub fn recent_events_by_kind(
        &self,
        session_id: &str,
        kind: &str,
        limit: u32,
    ) -> Result<Vec<Event>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT session_id, seq, type, payload, created_at
             FROM events WHERE session_id = ?1 AND type = ?2
             ORDER BY seq DESC LIMIT ?3",
        )?;
        let rows =
            statement.query_map(params![session_id, kind, limit.clamp(1, 10_000)], |row| {
                let payload: String = row.get(3)?;
                Ok(Event {
                    session_id: row.get(0)?,
                    seq: row.get::<_, i64>(1)? as u64,
                    kind: row.get(2)?,
                    payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
                    created_at: row.get(4)?,
                })
            })?;
        let mut events = rows.collect::<Result<Vec<_>, _>>()?;
        events.reverse();
        Ok(events)
    }

    /// Task ids that ever recorded a gate block for this gate. Callers must
    /// still validate the task's current row and latest block event before
    /// releasing it.
    pub fn plan_task_ids_with_gate_block(
        &self,
        session_id: &str,
        gate_id: &str,
    ) -> Result<Vec<String>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT DISTINCT json_extract(payload, '$.task_id')
             FROM events
             WHERE session_id = ?1
               AND type = 'plan_task.finished'
               AND json_extract(payload, '$.task_status') = 'blocked'
               AND json_extract(payload, '$.reason') = 'waiting for external gate'
               AND json_extract(payload, '$.gate_id') = ?2",
        )?;
        let rows = statement.query_map(params![session_id, gate_id], |row| {
            row.get::<_, Option<String>>(0)
        })?;
        let mut tasks = Vec::new();
        for task in rows {
            if let Some(task) = task?
                && !task.is_empty()
            {
                tasks.push(task);
            }
        }
        tasks.sort();
        tasks.dedup();
        Ok(tasks)
    }

    /// The newest finish/block event for one task. A gate release must consult
    /// this instead of an older gate event that a later block superseded.
    pub fn latest_plan_task_block_event(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<Option<Event>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT session_id, seq, type, payload, created_at
             FROM events
             WHERE session_id = ?1
               AND type IN ('plan_task.finished', 'plan_task.blocked')
               AND json_extract(payload, '$.task_id') = ?2
             ORDER BY seq DESC LIMIT 1",
        )?;
        let mut rows = statement.query_map(params![session_id, task_id], |row| {
            let payload: String = row.get(3)?;
            Ok(Event {
                session_id: row.get(0)?,
                seq: row.get::<_, i64>(1)? as u64,
                kind: row.get(2)?,
                payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
                created_at: row.get(4)?,
            })
        })?;
        Ok(rows.next().transpose()?)
    }

    pub fn list_events_by_kind(
        &self,
        session_id: &str,
        kind: &str,
        limit: u32,
    ) -> Result<Vec<Event>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT session_id, seq, type, payload, created_at
             FROM events WHERE session_id = ?1 AND type = ?2
             ORDER BY seq ASC LIMIT ?3",
        )?;
        let rows =
            statement.query_map(params![session_id, kind, limit.clamp(1, 10_000)], |row| {
                let payload: String = row.get(3)?;
                Ok(Event {
                    session_id: row.get(0)?,
                    seq: row.get::<_, i64>(1)? as u64,
                    kind: row.get(2)?,
                    payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
                    created_at: row.get(4)?,
                })
            })?;
        let mut events = Vec::new();
        for event in rows {
            events.push(event?);
        }
        Ok(events)
    }

    /// Bounded cross-session observation of one cache epoch. The query scans
    /// the newest `context.cache` events across sessions and filters in Rust;
    /// it never reads prompt text and never claims a provider cache hit. The
    /// limit is clamped so a dashboard refresh cannot load an unbounded log.
    ///
    /// Returns `(distinct_sessions_with_reads, hit_tokens, matched_calls)`.
    pub fn cache_epoch_cross_session(
        &self,
        epoch: &str,
        exclude_session: &str,
        limit: u32,
    ) -> Result<(u64, u64, u64)> {
        if epoch.is_empty() {
            return Ok((0, 0, 0));
        }
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT session_id, payload
             FROM events
             WHERE type = 'context.cache' AND session_id <> ?1
             ORDER BY created_at DESC, seq DESC
             LIMIT ?2",
        )?;
        let rows =
            statement.query_map(params![exclude_session, limit.clamp(1, 10_000)], |row| {
                let payload: String = row.get(1)?;
                Ok((
                    row.get::<_, String>(0)?,
                    serde_json::from_str::<serde_json::Value>(&payload)
                        .unwrap_or(serde_json::Value::Null),
                ))
            })?;
        let mut sessions = std::collections::BTreeSet::new();
        let mut hit_tokens = 0u64;
        let mut calls = 0u64;
        for row in rows {
            let (session_id, payload) = row?;
            let Some(candidate) = payload
                .get("cache_epoch")
                .and_then(serde_json::Value::as_str)
                .or_else(|| {
                    payload
                        .get("prompt_prefix_hash")
                        .and_then(serde_json::Value::as_str)
                })
            else {
                continue;
            };
            if candidate != epoch {
                continue;
            }
            calls = calls.saturating_add(1);
            let read = payload
                .get("cache_read_tokens")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            if read > 0 {
                sessions.insert(session_id);
                hit_tokens = hit_tokens.saturating_add(read);
            }
        }
        Ok((sessions.len() as u64, hit_tokens, calls))
    }

    /// Bounded newest-first project cache events for the read-only overview.
    /// Only `context.cache` rows belonging to the project's sessions are read;
    /// payloads never contain prompt text. The caller receives `(events,
    /// truncated)`.
    pub fn project_cache_events(&self, project_id: &str, limit: u32) -> Result<(Vec<Event>, bool)> {
        let limit = limit.clamp(1, 50_000);
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT e.session_id, e.seq, e.type, e.payload, e.created_at
             FROM events e
             JOIN sessions s ON s.id = e.session_id
             WHERE s.project_id = ?1 AND e.type = 'context.cache'
             ORDER BY e.created_at DESC, e.seq DESC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![project_id, limit + 1], |row| {
            let payload: String = row.get(3)?;
            Ok(Event {
                session_id: row.get(0)?,
                seq: row.get::<_, i64>(1)? as u64,
                kind: row.get(2)?,
                payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
                created_at: row.get(4)?,
            })
        })?;
        let mut events = rows.collect::<Result<Vec<_>, _>>()?;
        let truncated = events.len() > limit as usize;
        events.truncate(limit as usize);
        events.reverse();
        Ok((events, truncated))
    }

    /// Newest-first bounded window of one event kind. Used by the read-only
    /// tool-activity projection; callers never load an unbounded session log.
    pub fn list_recent_events_by_kind(
        &self,
        session_id: &str,
        kind: &str,
        limit: u32,
    ) -> Result<Vec<Event>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT session_id, seq, type, payload, created_at
             FROM events WHERE session_id = ?1 AND type = ?2
             ORDER BY seq DESC LIMIT ?3",
        )?;
        let rows =
            statement.query_map(params![session_id, kind, limit.clamp(1, 1_000)], |row| {
                let payload: String = row.get(3)?;
                Ok(Event {
                    session_id: row.get(0)?,
                    seq: row.get::<_, i64>(1)? as u64,
                    kind: row.get(2)?,
                    payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
                    created_at: row.get(4)?,
                })
            })?;
        let mut events = rows.collect::<Result<Vec<_>, _>>()?;
        events.sort_by_key(|event| std::cmp::Reverse(event.seq));
        Ok(events)
    }

    pub fn event_count_by_kind(&self, session_id: &str, kind: &str) -> Result<u64> {
        let conn = self.lock()?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM events WHERE session_id = ?1 AND type = ?2",
            params![session_id, kind],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count.max(0) as u64)
    }

    pub fn latest_event_by_kind(&self, session_id: &str, kind: &str) -> Result<Option<Event>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT session_id, seq, type, payload, created_at
             FROM events WHERE session_id = ?1 AND type = ?2
             ORDER BY seq DESC LIMIT 1",
            params![session_id, kind],
            |row| {
                let payload: String = row.get(3)?;
                Ok(Event {
                    session_id: row.get(0)?,
                    seq: row.get::<_, i64>(1)? as u64,
                    kind: row.get(2)?,
                    payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
                    created_at: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn list_all_events(&self, session_id: &str) -> Result<Vec<Event>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT session_id, seq, type, payload, created_at
             FROM events WHERE session_id = ?1 ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            let payload: String = row.get(3)?;
            Ok(Event {
                session_id: row.get(0)?,
                seq: row.get::<_, i64>(1)? as u64,
                kind: row.get(2)?,
                payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
                created_at: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn list_events(&self, session_id: &str, since_seq: u64, limit: u32) -> Result<Vec<Event>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT session_id, seq, type, payload, created_at
             FROM events
             WHERE session_id = ?1 AND seq >= ?2
             ORDER BY seq ASC LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            params![session_id, since_seq as i64, limit.clamp(1, 1000) as i64],
            |row| {
                let payload: String = row.get(3)?;
                Ok(Event {
                    session_id: row.get(0)?,
                    seq: row.get::<_, i64>(1)? as u64,
                    kind: row.get(2)?,
                    payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
                    created_at: row.get(4)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Search session messages and event payloads. Returns up to `limit` rows,
    /// newest first. Callers that need to know whether more rows exist should
    /// pass `limit + 1`.
    /// Search message content and raw event payloads. The FTS5 index handles
    /// word/prefix queries; a plain `LIKE` scan is retained as a fallback for
    /// short or punctuation-only queries and substring-only matches.
    pub fn search(
        &self,
        query: &str,
        project_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<SearchHit>> {
        let limit = limit.clamp(1, 500);
        // unicode61 groups CJK runs into single tokens, so a substring query
        // such as "流程" would miss "部署流程" unless it is a token prefix.
        // Keep the substring scan for non-ASCII queries to preserve recall.
        if query.is_ascii() {
            let hits = self.search_fts(query, project_id, limit as i64)?;
            if !hits.is_empty() {
                return Ok(hits);
            }
        }
        self.search_like(query, project_id, limit)
    }

    pub(super) fn search_fts(
        &self,
        query: &str,
        project_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<SearchHit>> {
        let Some(fts) = fts_query(query) else {
            return Ok(Vec::new());
        };
        let conn = self.lock()?;
        let mut hits = Vec::new();

        let mut message_stmt = conn.prepare(
            "SELECT m.id, m.session_id, s.project_id, s.goal, m.role, m.content, m.created_at
             FROM messages_fts
             JOIN messages m ON m.rowid = messages_fts.rowid
             JOIN sessions s ON s.id = m.session_id
             WHERE messages_fts MATCH ?1
               AND (?2 IS NULL OR s.project_id = ?2)
             ORDER BY m.created_at DESC LIMIT ?3",
        )?;
        let messages = message_stmt.query_map(params![fts, project_id, limit], |row| {
            let content: String = row.get(5)?;
            Ok(SearchHit {
                id: row.get(0)?,
                session_id: row.get(1)?,
                project_id: row.get(2)?,
                session_goal: row.get(3)?,
                source: "message".to_string(),
                role: Some(row.get(4)?),
                kind: None,
                seq: None,
                snippet: search_snippet(&content, query),
                created_at: row.get(6)?,
            })
        })?;
        for hit in messages {
            hits.push(hit?);
        }

        let mut event_stmt = conn.prepare(
            "SELECT e.session_id, s.project_id, s.goal, e.type, e.payload, e.seq, e.created_at
             FROM events_fts
             JOIN events e ON e.rowid = events_fts.rowid
             JOIN sessions s ON s.id = e.session_id
             WHERE events_fts MATCH ?1
               AND (?2 IS NULL OR s.project_id = ?2)
             ORDER BY e.created_at DESC LIMIT ?3",
        )?;
        let events = event_stmt.query_map(params![fts, project_id, limit], |row| {
            let payload: String = row.get(4)?;
            let session_id: String = row.get(0)?;
            let seq = row.get::<_, i64>(5)? as u64;
            Ok(SearchHit {
                id: format!("{session_id}:{seq}"),
                session_id,
                project_id: row.get(1)?,
                session_goal: row.get(2)?,
                source: "event".to_string(),
                role: None,
                kind: Some(row.get(3)?),
                seq: Some(seq),
                snippet: search_snippet(&payload, query),
                created_at: row.get(6)?,
            })
        })?;
        for hit in events {
            hits.push(hit?);
        }

        hits.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        hits.truncate(limit as usize);
        Ok(hits)
    }

    pub(super) fn search_like(
        &self,
        query: &str,
        project_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<SearchHit>> {
        let pattern = format!("%{}%", escape_like(query));
        let limit = limit.clamp(1, 500) as i64;
        let conn = self.lock()?;

        let mut hits = Vec::new();

        let mut message_stmt = conn.prepare(
            "SELECT m.id, m.session_id, s.project_id, s.goal, m.role, m.content, m.created_at
             FROM messages m JOIN sessions s ON s.id = m.session_id
             WHERE m.content LIKE ?1 ESCAPE '\\'
               AND (?2 IS NULL OR s.project_id = ?2)
             ORDER BY m.created_at DESC LIMIT ?3",
        )?;
        let messages = message_stmt.query_map(params![pattern, project_id, limit], |row| {
            let content: String = row.get(5)?;
            Ok(SearchHit {
                id: row.get(0)?,
                session_id: row.get(1)?,
                project_id: row.get(2)?,
                session_goal: row.get(3)?,
                source: "message".to_string(),
                role: Some(row.get(4)?),
                kind: None,
                seq: None,
                snippet: search_snippet(&content, query),
                created_at: row.get(6)?,
            })
        })?;
        for hit in messages {
            hits.push(hit?);
        }

        let mut event_stmt = conn.prepare(
            "SELECT e.session_id, s.project_id, s.goal, e.type, e.payload, e.seq, e.created_at
             FROM events e JOIN sessions s ON s.id = e.session_id
             WHERE e.payload LIKE ?1 ESCAPE '\\'
               AND (?2 IS NULL OR s.project_id = ?2)
             ORDER BY e.created_at DESC LIMIT ?3",
        )?;
        let events = event_stmt.query_map(params![pattern, project_id, limit], |row| {
            let payload: String = row.get(4)?;
            let session_id: String = row.get(0)?;
            let seq = row.get::<_, i64>(5)? as u64;
            Ok(SearchHit {
                id: format!("{session_id}:{seq}"),
                session_id,
                project_id: row.get(1)?,
                session_goal: row.get(2)?,
                source: "event".to_string(),
                role: None,
                kind: Some(row.get(3)?),
                seq: Some(seq),
                snippet: search_snippet(&payload, query),
                created_at: row.get(6)?,
            })
        })?;
        for hit in events {
            hits.push(hit?);
        }

        hits.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        hits.truncate(limit as usize);
        Ok(hits)
    }

    pub fn integrity_check(&self) -> Result<String> {
        let conn = self.lock()?;
        conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .map_err(Into::into)
    }

    pub fn project_count(&self) -> Result<usize> {
        let conn = self.lock()?;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    pub fn session_count(&self) -> Result<usize> {
        let conn = self.lock()?;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    /// Read-only aggregate counts for daemon metrics. This deliberately returns
    /// only bounded status/kind label sets and numeric totals: no session ids,
    /// task ids, goals, paths, model names, or agent names.
    pub fn daemon_metrics_snapshot(&self) -> Result<DaemonMetricsSnapshot> {
        let conn = self.lock()?;
        let projects: i64 =
            conn.query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0))?;
        let usage = conn.query_row(
            "SELECT COALESCE(SUM(input_tokens), 0),
                    COALESCE(SUM(output_tokens), 0),
                    COALESCE(SUM(cache_read_tokens), 0),
                    COALESCE(SUM(cache_write_tokens), 0),
                    COALESCE(SUM(cost_usd), 0)
             FROM usage_ledger",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?.max(0) as u64,
                    row.get::<_, i64>(1)?.max(0) as u64,
                    row.get::<_, i64>(2)?.max(0) as u64,
                    row.get::<_, i64>(3)?.max(0) as u64,
                    row.get::<_, f64>(4)?,
                ))
            },
        )?;
        let (tokens_in, tokens_out, cache_read_tokens, cache_write_tokens, cost_spent_usd) = usage;
        let (preemptions_by_phase, preemptions_by_reason) = preemption_metric_counts(&conn)?;
        let preamble_payload = conn
            .query_row(
                "SELECT payload FROM events
                 WHERE type = 'context.prompt_prefix'
                 ORDER BY created_at DESC, seq DESC LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let preamble_tokens_estimate = preamble_payload
            .and_then(|payload| serde_json::from_str::<serde_json::Value>(&payload).ok())
            .and_then(|payload| {
                payload
                    .get("preamble_tokens_estimate")
                    .and_then(serde_json::Value::as_u64)
                    .or_else(|| {
                        payload
                            .get("stable_prefix_chars")
                            .and_then(serde_json::Value::as_u64)
                            .map(|chars| chars / 4)
                    })
            });
        Ok(DaemonMetricsSnapshot {
            projects: projects.max(0) as u64,
            sessions: bounded_metric_counts(&conn, "sessions", "status")?,
            plan_tasks: bounded_metric_counts(&conn, "plan_tasks", "status")?,
            merge_queue: bounded_metric_counts(&conn, "merge_queue", "status")?,
            intake: bounded_metric_counts(&conn, "intake_tasks", "status")?,
            continuations: bounded_metric_counts(&conn, "session_continuations", "status")?,
            recoveries: bounded_metric_counts(&conn, "orchestration_recoveries", "status")?,
            memories: bounded_metric_counts(&conn, "memories", "status")?,
            memory_conflicts: bounded_metric_counts(&conn, "memory_conflicts", "status")?,
            runtime_leases: bounded_metric_counts(&conn, "runtime_leases", "kind")?,
            supervised_processes: bounded_metric_counts(&conn, "supervised_processes", "state")?,
            supervised_process_output_bytes: conn
                .query_row(
                    "SELECT COALESCE(SUM(output_bytes), 0) FROM supervised_processes",
                    [],
                    |row| row.get::<_, i64>(0),
                )?
                .max(0) as u64,
            preemptions_by_phase,
            preemptions_by_reason,
            tokens_in,
            tokens_out,
            cache_read_tokens,
            cache_write_tokens,
            preamble_tokens_estimate,
            cost_spent_usd: if cost_spent_usd.is_finite() {
                cost_spent_usd.max(0.0)
            } else {
                0.0
            },
        })
    }

    pub fn event_exists(&self, session_id: &str, seq: u64) -> Result<bool> {
        let conn = self.lock()?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM events WHERE session_id = ?1 AND seq = ?2",
            params![session_id, seq as i64],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count > 0)
    }
}

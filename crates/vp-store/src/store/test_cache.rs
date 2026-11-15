//! Content-addressed test-result cache.

use super::*;

impl Store {
    pub fn get_test_cache_policy(&self, project_id: &str) -> Result<TestCachePolicy> {
        let conn = self.lock()?;
        let stored: Option<String> = conn
            .query_row(
                "SELECT policy FROM project_test_cache_policies WHERE project_id = ?1",
                params![project_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(stored
            .and_then(|policy| serde_json::from_str::<TestCachePolicy>(&policy).ok())
            .unwrap_or_default()
            .normalized())
    }

    pub fn set_test_cache_policy(&self, project_id: &str, policy: &TestCachePolicy) -> Result<()> {
        let normalized = policy.clone().normalized();
        let text = serde_json::to_string(&normalized).context("serializing test-cache policy")?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_test_cache_policies (project_id, policy, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(project_id) DO UPDATE SET
                policy = excluded.policy,
                updated_at = excluded.updated_at",
            params![project_id, text, now_rfc3339()],
        )?;
        Ok(())
    }

    pub fn get_test_result(&self, id: &str) -> Result<Option<StoredTestResult>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, project_id, session_id, kind, command, command_source,
                    key_version, cache_key, inputs_hash, impact_generation, status,
                    exit_code, duration_ms, output_ref, output_bytes, covered_paths,
                    warnings, warning_count, failure_count, result_json, created_at,
                    expires_at, hit_count, last_hit_at
             FROM test_results WHERE id = ?1",
            params![id],
            map_stored_test_result,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn find_test_result_by_key(
        &self,
        project_id: &str,
        cache_key: &str,
    ) -> Result<Option<StoredTestResult>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, project_id, session_id, kind, command, command_source,
                    key_version, cache_key, inputs_hash, impact_generation, status,
                    exit_code, duration_ms, output_ref, output_bytes, covered_paths,
                    warnings, warning_count, failure_count, result_json, created_at,
                    expires_at, hit_count, last_hit_at
             FROM test_results WHERE project_id = ?1 AND cache_key = ?2",
            params![project_id, cache_key],
            map_stored_test_result,
        )
        .optional()
        .map_err(Into::into)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn list_test_results(
        &self,
        project_id: &str,
        session_id: Option<&str>,
        kind: Option<&str>,
        status: Option<&str>,
        include_expired: bool,
        limit: usize,
        now: &str,
    ) -> Result<Vec<TestCacheEntry>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, project_id, session_id, kind, command, command_source,
                    key_version, cache_key, inputs_hash, impact_generation, status,
                    exit_code, duration_ms, output_ref, output_bytes, covered_paths,
                    warnings, warning_count, failure_count, result_json, created_at,
                    expires_at, hit_count, last_hit_at
             FROM test_results
             WHERE project_id = ?1
               AND (?2 IS NULL OR session_id = ?2)
               AND (?3 IS NULL OR kind = ?3)
               AND (?4 IS NULL OR status = ?4)
               AND (?5 = 1 OR expires_at > ?6)
             ORDER BY created_at DESC, id ASC
             LIMIT ?7",
        )?;
        let rows = stmt.query_map(
            params![
                project_id,
                session_id,
                kind,
                status,
                if include_expired { 1 } else { 0 },
                now,
                limit.clamp(1, 10_000) as i64
            ],
            |row| map_stored_test_result(row).map(|stored| stored.entry),
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn count_test_results(
        &self,
        project_id: &str,
        session_id: Option<&str>,
        kind: Option<&str>,
        status: Option<&str>,
        include_expired: bool,
        now: &str,
    ) -> Result<u64> {
        let conn = self.lock()?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM test_results
             WHERE project_id = ?1
               AND (?2 IS NULL OR session_id = ?2)
               AND (?3 IS NULL OR kind = ?3)
               AND (?4 IS NULL OR status = ?4)
               AND (?5 = 1 OR expires_at > ?6)",
            params![
                project_id,
                session_id,
                kind,
                status,
                if include_expired { 1 } else { 0 },
                now
            ],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count.max(0) as u64)
    }

    pub fn test_cache_stats(&self, project_id: &str, now: &str) -> Result<TestCacheStats> {
        let policy = self.get_test_cache_policy(project_id)?;
        let conn = self.lock()?;
        let (entries, passed, failed, error, expired, total_hits, output_bytes) = conn.query_row(
            "SELECT COUNT(*),
                        COALESCE(SUM(CASE WHEN status = 'passed' THEN 1 ELSE 0 END), 0),
                        COALESCE(SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END), 0),
                        COALESCE(SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END), 0),
                        COALESCE(SUM(CASE WHEN expires_at <= ?2 THEN 1 ELSE 0 END), 0),
                        COALESCE(SUM(hit_count), 0),
                        COALESCE(SUM(output_bytes), 0)
                 FROM test_results WHERE project_id = ?1",
            params![project_id, now],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?.max(0) as u64,
                    row.get::<_, i64>(1)?.max(0) as u64,
                    row.get::<_, i64>(2)?.max(0) as u64,
                    row.get::<_, i64>(3)?.max(0) as u64,
                    row.get::<_, i64>(4)?.max(0) as u64,
                    row.get::<_, i64>(5)?.max(0) as u64,
                    row.get::<_, i64>(6)?.max(0) as u64,
                ))
            },
        )?;
        let mut by_kind = std::collections::BTreeMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT kind, COUNT(*) FROM test_results
                 WHERE project_id = ?1 GROUP BY kind ORDER BY kind ASC",
            )?;
            for row in stmt.query_map(params![project_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?.max(0) as u64,
                ))
            })? {
                let (kind, count) = row?;
                by_kind.insert(kind, count);
            }
        }
        let (oldest_created_at, newest_created_at) = conn.query_row(
            "SELECT MIN(created_at), MAX(created_at) FROM test_results WHERE project_id = ?1",
            params![project_id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                ))
            },
        )?;
        Ok(TestCacheStats {
            project_id: project_id.to_string(),
            entries,
            passed,
            failed,
            error,
            expired,
            total_hits,
            output_bytes,
            by_kind,
            oldest_created_at,
            newest_created_at,
            policy,
            generated_at: now.to_string(),
        })
    }

    /// Insert or replace one content-addressed result. Replacing a different
    /// row with the same key emits `test_cache.invalidated` for the old owner,
    /// then `test_cache.stored` for the new row. Entries beyond
    /// `policy.max_entries` are evicted oldest-first with a `test_cache.pruned`
    /// event per affected session. All events and projections commit together.
    pub fn record_test_result_with_events(
        &self,
        row: &TestCacheEntry,
        result_json: &str,
        max_entries: u32,
    ) -> Result<(TestCacheEntry, Vec<Event>)> {
        if row.project_id.trim().is_empty()
            || row.session_id.trim().is_empty()
            || row.cache_key.trim().is_empty()
        {
            anyhow::bail!("test result needs a project, session, and cache key");
        }
        let mut stored = row.clone();
        if stored.id.trim().is_empty() {
            stored.id = Uuid::now_v7().to_string();
        }
        if stored.created_at.trim().is_empty() {
            stored.created_at = now_rfc3339();
        }
        let preview = bounded_test_result_preview(result_json);
        let mut events = Vec::new();

        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let project: Option<String> = tx
            .query_row(
                "SELECT project_id FROM sessions WHERE id = ?1",
                params![stored.session_id],
                |row| row.get(0),
            )
            .optional()?;
        match project {
            Some(project_id) if project_id == stored.project_id => {}
            Some(_) => anyhow::bail!(
                "test result session {} belongs to a different project",
                stored.session_id
            ),
            None => anyhow::bail!("session not found: {}", stored.session_id),
        }
        let existing: Option<(String, String)> = tx
            .query_row(
                "SELECT id, session_id FROM test_results
                 WHERE project_id = ?1 AND cache_key = ?2",
                params![stored.project_id, stored.cache_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((existing_id, existing_session)) = existing {
            let event = insert_event_tx(
                &tx,
                &existing_session,
                "test_cache.invalidated",
                serde_json::json!({
                    "result_id": existing_id,
                    "cache_key": stored.cache_key,
                    "reason": "replaced_by_refresh",
                }),
            )?;
            events.push(event);
            tx.execute(
                "DELETE FROM test_results WHERE id = ?1",
                params![existing_id],
            )?;
        }
        let event = insert_event_tx(
            &tx,
            &stored.session_id,
            "test_cache.stored",
            serde_json::json!({
                "entry": stored,
                "result_preview": preview,
            }),
        )?;
        events.push(event);
        insert_test_result_tx(&tx, &stored, result_json)?;

        let max_entries = max_entries.clamp(1, 10_000) as i64;
        let overflow = {
            let mut stmt = tx.prepare(
                "SELECT id, session_id FROM test_results
                 WHERE project_id = ?1
                 ORDER BY created_at DESC, id ASC
                 LIMIT -1 OFFSET ?2",
            )?;
            let rows = stmt.query_map(params![stored.project_id, max_entries], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        if !overflow.is_empty() {
            let mut grouped: std::collections::BTreeMap<String, Vec<String>> =
                std::collections::BTreeMap::new();
            for (id, session_id) in overflow {
                grouped.entry(session_id).or_default().push(id);
            }
            for (session_id, ids) in grouped {
                events.extend(emit_test_cache_prune_events_tx(
                    &tx,
                    &session_id,
                    &ids,
                    "max_entries",
                )?);
                for id in ids {
                    tx.execute("DELETE FROM test_results WHERE id = ?1", params![id])?;
                }
            }
        }
        tx.commit()?;
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok((stored, events))
    }

    /// Record a cache hit against the owner row and append a `test_cache.hit`
    /// audit event to the requesting session. Hit counters are operational
    /// telemetry; replay never requires them to match the projection.
    pub fn record_test_result_hit(
        &self,
        result_id: &str,
        requesting_session_id: &str,
        now: &str,
    ) -> Result<Option<Event>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let owner: Option<(String, i64)> = tx
            .query_row(
                "SELECT session_id, hit_count FROM test_results WHERE id = ?1",
                params![result_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((owner_session, hits)) = owner else {
            return Ok(None);
        };
        let hit_count = hits.max(0) as u64 + 1;
        tx.execute(
            "UPDATE test_results SET hit_count = ?1, last_hit_at = ?2 WHERE id = ?3",
            params![hit_count as i64, now, result_id],
        )?;
        let event = insert_event_tx(
            &tx,
            requesting_session_id,
            "test_cache.hit",
            serde_json::json!({
                "result_id": result_id,
                "owner_session_id": owner_session,
                "hit_count": hit_count,
                "hit_at": now,
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some(event))
    }

    /// Prune cache metadata by age, expiry, or kind. `older_than_secs` is
    /// measured from `created_at`; when absent, the project retention policy is
    /// used. The output body is not touched here: tool-output retention owns
    /// body lifetime independently.
    pub fn prune_test_results(
        &self,
        project_id: &str,
        older_than_secs: Option<u64>,
        expired_only: bool,
        kind: Option<&str>,
        dry_run: bool,
        now: &str,
    ) -> Result<TestCacheSweep> {
        let policy = self.get_test_cache_policy(project_id)?;
        let cutoff = if expired_only {
            now.to_string()
        } else {
            let seconds =
                older_than_secs.unwrap_or_else(|| u64::from(policy.retention_days) * 86_400);
            match parse_rfc3339(now) {
                Some(now_time) => now_time
                    .saturating_sub(time::Duration::seconds(seconds.min(i64::MAX as u64) as i64))
                    .format(&Rfc3339)
                    .unwrap_or_else(|_| now.to_string()),
                None => now.to_string(),
            }
        };
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let mut stmt = tx.prepare(
            "SELECT id, session_id FROM test_results
             WHERE project_id = ?1
               AND (
                    (?2 = 1 AND expires_at <= ?5)
                    OR (?2 = 0 AND (created_at <= ?3 OR expires_at <= ?5))
               )
               AND (?4 IS NULL OR kind = ?4)
             ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt
            .query_map(
                params![
                    project_id,
                    if expired_only { 1 } else { 0 },
                    cutoff,
                    kind,
                    now
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        let mut sweep = TestCacheSweep {
            removed_ids: rows.iter().map(|(id, _)| id.clone()).collect(),
            ..TestCacheSweep::default()
        };
        if !dry_run {
            let mut grouped: std::collections::BTreeMap<String, Vec<String>> =
                std::collections::BTreeMap::new();
            for (id, session_id) in &rows {
                grouped
                    .entry(session_id.clone())
                    .or_default()
                    .push(id.clone());
            }
            sweep.affected_sessions = grouped.len() as u64;
            for (session_id, ids) in grouped {
                let events = emit_test_cache_prune_events_tx(
                    &tx,
                    &session_id,
                    &ids,
                    if expired_only { "expired" } else { "retention" },
                )?;
                sweep.events.extend(events);
                for id in ids {
                    tx.execute("DELETE FROM test_results WHERE id = ?1", params![id])?;
                }
            }
        } else {
            let mut sessions = std::collections::BTreeSet::new();
            for (_, session_id) in &rows {
                sessions.insert(session_id.clone());
            }
            sweep.affected_sessions = sessions.len() as u64;
        }
        tx.commit()?;
        drop(conn);
        for event in &sweep.events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        sweep.remaining = self.count_test_results(project_id, None, kind, None, true, now)?;
        Ok(sweep)
    }

    /// Clear matching rows. The daemon requires explicit operator confirmation
    /// before calling this; the store itself does not silently clear.
    pub fn clear_test_results(
        &self,
        project_id: &str,
        session_id: Option<&str>,
        kind: Option<&str>,
        now: &str,
    ) -> Result<TestCacheSweep> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let mut stmt = tx.prepare(
            "SELECT id, session_id FROM test_results
             WHERE project_id = ?1
               AND (?2 IS NULL OR session_id = ?2)
               AND (?3 IS NULL OR kind = ?3)
             ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt
            .query_map(params![project_id, session_id, kind], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        let mut sweep = TestCacheSweep {
            removed_ids: rows.iter().map(|(id, _)| id.clone()).collect(),
            ..TestCacheSweep::default()
        };
        let mut grouped: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        for (id, owner_session) in &rows {
            grouped
                .entry(owner_session.clone())
                .or_default()
                .push(id.clone());
        }
        sweep.affected_sessions = grouped.len() as u64;
        for (owner_session, ids) in grouped {
            let events = emit_test_cache_prune_events_tx(&tx, &owner_session, &ids, "cleared")?;
            sweep.events.extend(events);
            for id in ids {
                tx.execute("DELETE FROM test_results WHERE id = ?1", params![id])?;
            }
        }
        tx.commit()?;
        drop(conn);
        for event in &sweep.events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        sweep.remaining = self.count_test_results(project_id, None, None, None, true, now)?;
        Ok(sweep)
    }

    /// Replace one session's metadata rows from the event stream. Used by
    /// `session.replay {repair:true}`, bundle import, and `db-rebuild`.
    /// Output bodies are intentionally not reconstructed.
    pub fn replace_test_results(&self, session_id: &str, entries: &[TestCacheEntry]) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM test_results WHERE session_id = ?1",
            params![session_id],
        )?;
        for entry in entries {
            insert_test_result_tx(&tx, entry, "{}")?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Recent metadata rows used by historical impact mapping. This deliberately
    /// loads a bounded window and filters covered paths in Rust; no unbounded
    /// JSON-table scan can be triggered by a crafted path.
    pub fn test_results_for_paths(
        &self,
        project_id: &str,
        changed_paths: &[String],
        now: &str,
        limit: usize,
    ) -> Result<Vec<TestCacheEntry>> {
        if changed_paths.is_empty() {
            return Ok(Vec::new());
        }
        let entries = self.list_test_results(
            project_id,
            None,
            None,
            None,
            false,
            limit.clamp(1, 2_000),
            now,
        )?;
        Ok(entries
            .into_iter()
            .filter(|entry| entry.status == "passed")
            .filter(|entry| {
                entry.covered_paths.iter().any(|covered| {
                    changed_paths.iter().any(|changed| {
                        covered == changed
                            || covered.starts_with(&format!("{changed}/"))
                            || changed.starts_with(&format!("{covered}/"))
                    })
                })
            })
            .collect())
    }
}

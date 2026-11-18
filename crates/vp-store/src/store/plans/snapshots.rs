use super::*;

impl Store {
    pub fn set_team_plan(&self, session_id: &str, plan: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE sessions SET team_plan = ?1 WHERE id = ?2",
            params![plan, session_id],
        )?;
        Ok(())
    }

    pub fn set_team_plan_with_event(&self, session_id: &str, plan: &str) -> Result<Event> {
        self.set_team_plan_with_runtime_snapshot_and_event(session_id, plan, None)
    }

    /// Persist a submitted plan, its optional runtime snapshot, and the
    /// `team_plan.submitted` event atomically. The snapshot JSON is also
    /// embedded in the event payload so replay/repair can reconstruct it.
    pub fn set_team_plan_with_runtime_snapshot_and_event(
        &self,
        session_id: &str,
        plan: &str,
        runtime_snapshot_json: Option<&str>,
    ) -> Result<Event> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE sessions
             SET team_plan = ?1,
                 plan_runtime_snapshot = COALESCE(?3, plan_runtime_snapshot)
             WHERE id = ?2",
            params![plan, session_id, runtime_snapshot_json],
        )?;
        if changed == 0 {
            return Err(anyhow!("session not found: {session_id}"));
        }
        let mut payload = serde_json::from_str(plan).unwrap_or(serde_json::Value::Null);
        if let (Some(snapshot_json), Some(object)) =
            (runtime_snapshot_json, payload.as_object_mut())
            && let Ok(snapshot) = serde_json::from_str::<serde_json::Value>(snapshot_json)
        {
            object.insert("runtime_snapshot".into(), snapshot);
        }
        let event = insert_event_tx(&tx, session_id, "team_plan.submitted", payload)?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(event)
    }

    /// Persist the first `team_plan.submitted` for a session only when no plan
    /// is stored yet. `Ok(None)` means a plan already exists and nothing was
    /// written; callers must use the explicit revision path instead of
    /// replacing the plan and resetting the task projection.
    pub fn set_team_plan_if_absent_with_runtime_snapshot_and_event(
        &self,
        session_id: &str,
        plan: &str,
        runtime_snapshot_json: Option<&str>,
    ) -> Result<Option<Event>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let existing = tx
            .query_row(
                "SELECT team_plan FROM sessions WHERE id = ?1",
                params![session_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?;
        let Some(existing) = existing else {
            return Err(anyhow!("session not found: {session_id}"));
        };
        let existing_submissions: i64 = tx.query_row(
            "SELECT COUNT(*) FROM events
             WHERE session_id = ?1 AND type = 'team_plan.submitted'",
            params![session_id],
            |row| row.get(0),
        )?;
        if existing.is_some() || existing_submissions > 0 {
            return Ok(None);
        }
        let changed = tx.execute(
            "UPDATE sessions
             SET team_plan = ?1,
                 plan_runtime_snapshot = COALESCE(?3, plan_runtime_snapshot)
             WHERE id = ?2",
            params![plan, session_id, runtime_snapshot_json],
        )?;
        if changed == 0 {
            return Err(anyhow!("session not found: {session_id}"));
        }
        let mut payload = serde_json::from_str(plan).unwrap_or(serde_json::Value::Null);
        if let (Some(snapshot_json), Some(object)) =
            (runtime_snapshot_json, payload.as_object_mut())
            && let Ok(snapshot) = serde_json::from_str::<serde_json::Value>(snapshot_json)
        {
            object.insert("runtime_snapshot".into(), snapshot);
        }
        let event = insert_event_tx(&tx, session_id, "team_plan.submitted", payload)?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some(event))
    }

    /// Legacy fixture/import helper for a plan revision without a runtime
    /// snapshot. It does not compare revisions; production writers must use
    /// the runtime-snapshot or explicit compare-and-set method.
    pub fn set_team_plan_revision_with_event(
        &self,
        session_id: &str,
        plan: &str,
        kind: &str,
        payload: serde_json::Value,
    ) -> Result<Event> {
        if !matches!(kind, "team_plan.submitted" | "team_plan.revised") {
            return Err(anyhow!("invalid team plan event kind: {kind}"));
        }
        self.write_team_plan_revision_unchecked(session_id, plan, kind, payload, None)
    }

    fn write_team_plan_revision_unchecked(
        &self,
        session_id: &str,
        plan: &str,
        kind: &str,
        payload: serde_json::Value,
        runtime_snapshot_json: Option<&str>,
    ) -> Result<Event> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let event = Self::insert_team_plan_revision_tx(
            &tx,
            session_id,
            plan,
            kind,
            payload,
            runtime_snapshot_json,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(event)
    }

    /// Number of plan lifecycle events (`team_plan.submitted|revised`) in the
    /// same transaction as a revision write. This is the authoritative
    /// revision number; it does not depend on a bounded recent-event window.
    fn plan_revision_count_tx(tx: &Transaction<'_>, session_id: &str) -> Result<u32> {
        let count = tx.query_row(
            "SELECT COUNT(*) FROM events
             WHERE session_id = ?1 AND type IN ('team_plan.submitted', 'team_plan.revised')",
            params![session_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count.clamp(0, i64::from(u32::MAX)) as u32)
    }

    fn insert_team_plan_revision_tx(
        tx: &Transaction<'_>,
        session_id: &str,
        plan: &str,
        kind: &str,
        mut payload: serde_json::Value,
        runtime_snapshot_json: Option<&str>,
    ) -> Result<Event> {
        let changed = tx.execute(
            "UPDATE sessions
             SET team_plan = ?1,
                 plan_runtime_snapshot = COALESCE(?3, plan_runtime_snapshot)
             WHERE id = ?2",
            params![plan, session_id, runtime_snapshot_json],
        )?;
        if changed == 0 {
            return Err(anyhow!("session not found: {session_id}"));
        }
        if let (Some(snapshot_json), Some(object)) =
            (runtime_snapshot_json, payload.as_object_mut())
            && let Ok(snapshot) = serde_json::from_str::<serde_json::Value>(snapshot_json)
        {
            object.insert("runtime_snapshot".into(), snapshot);
        }
        insert_event_tx(tx, session_id, kind, payload)
    }

    /// Persist a plan revision, its optional runtime snapshot, and the plan
    /// lifecycle event atomically. The snapshot is stored on the session row
    /// and embedded in the event payload.
    ///
    /// When the payload declares its new `revision`, the write is a
    /// compare-and-set against the transaction-local revision count so two
    /// concurrent writers cannot both record the same revision number. A
    /// payload without revision metadata keeps the legacy unchecked write for
    /// fixture/import compatibility; every production fan-out/revision caller
    /// supplies a revision.
    pub fn set_team_plan_revision_with_runtime_snapshot_and_event(
        &self,
        session_id: &str,
        plan: &str,
        kind: &str,
        payload: serde_json::Value,
        runtime_snapshot_json: Option<&str>,
    ) -> Result<Event> {
        if !matches!(kind, "team_plan.submitted" | "team_plan.revised") {
            return Err(anyhow!("invalid team plan event kind: {kind}"));
        }
        let Some(new_revision) = payload.get("revision").and_then(serde_json::Value::as_u64) else {
            return self.write_team_plan_revision_unchecked(
                session_id,
                plan,
                kind,
                payload,
                runtime_snapshot_json,
            );
        };
        let expected = new_revision.saturating_sub(1).min(u64::from(u32::MAX)) as u32;
        match self.compare_and_set_team_plan_revision_with_runtime_snapshot_and_event(
            session_id,
            plan,
            kind,
            payload,
            runtime_snapshot_json,
            expected,
        )? {
            Some(event) => Ok(event),
            None => {
                let latest = self.plan_revision_count(session_id)?;
                Err(anyhow!(
                    "PLAN_REVISION_CONFLICT: plan revision conflict: revision {new_revision} is stale; current revision is {latest}"
                ))
            }
        }
    }

    /// Compare-and-set revision write. Reads the plan revision count inside a
    /// `BEGIN IMMEDIATE` transaction and applies the plan only when it still
    /// equals `expected_revision`. `Ok(None)` means a newer revision won; the
    /// stored plan and event log are left untouched.
    #[allow(clippy::too_many_arguments)]
    pub fn compare_and_set_team_plan_revision_with_runtime_snapshot_and_event(
        &self,
        session_id: &str,
        plan: &str,
        kind: &str,
        payload: serde_json::Value,
        runtime_snapshot_json: Option<&str>,
        expected_revision: u32,
    ) -> Result<Option<Event>> {
        if !matches!(kind, "team_plan.submitted" | "team_plan.revised") {
            return Err(anyhow!("invalid team plan event kind: {kind}"));
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if Self::plan_revision_count_tx(&tx, session_id)? != expected_revision {
            // Dropping the transaction rolls back without changing the plan.
            return Ok(None);
        }
        let event = Self::insert_team_plan_revision_tx(
            &tx,
            session_id,
            plan,
            kind,
            payload,
            runtime_snapshot_json,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some(event))
    }

    /// Full plan lifecycle history (`team_plan.submitted|revised`) in sequence
    /// order. Revision numbering and history must not depend on the bounded
    /// recent-event window.
    pub fn plan_lifecycle_events(&self, session_id: &str) -> Result<Vec<Event>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT session_id, seq, type, payload, created_at
             FROM events
             WHERE session_id = ?1
               AND type IN ('team_plan.submitted', 'team_plan.revised')
             ORDER BY seq ASC",
        )?;
        let rows = statement.query_map(params![session_id], |row| {
            let payload: String = row.get(3)?;
            Ok(Event {
                session_id: row.get(0)?,
                seq: row.get::<_, i64>(1)? as u64,
                kind: row.get(2)?,
                payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
                created_at: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Update only the stored plan runtime snapshot and append `plan.migrated`.
    /// The plan JSON, task rows, contracts, budgets, effect receipts, and gates
    /// are deliberately untouched.
    pub fn migrate_plan_runtime_snapshot_with_event(
        &self,
        session_id: &str,
        runtime_snapshot_json: &str,
        run_snapshots: &[(String, String, u32)],
        payload: serde_json::Value,
    ) -> Result<Event> {
        let snapshot: vp_protocol::runtime::RuntimeSnapshot =
            serde_json::from_str(runtime_snapshot_json)
                .map_err(|error| anyhow!("invalid runtime snapshot: {error}"))?;
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE sessions SET plan_runtime_snapshot = ?1 WHERE id = ?2",
            params![runtime_snapshot_json, session_id],
        )?;
        if changed == 0 {
            return Err(anyhow!("session not found: {session_id}"));
        }
        // An interrupted/failed run pinned to the old revision could never
        // resume after an explicit plan migration: preflight rejects the
        // revision mismatch and `plan.migrate` otherwise touches only the
        // plan snapshot. Rewrite those run pins in the same transaction.
        for (run_id, run_snapshot_json, plan_revision) in run_snapshots {
            tx.execute(
                "UPDATE session_runs
                 SET runtime_snapshot_json = ?1, plan_revision = ?2, updated_at = ?3
                 WHERE run_id = ?4 AND session_id = ?5
                   AND status IN ('interrupted', 'failed')",
                params![
                    run_snapshot_json,
                    *plan_revision as i64,
                    now_rfc3339(),
                    run_id,
                    session_id
                ],
            )?;
        }
        let mut payload = payload;
        if let Some(object) = payload.as_object_mut() {
            object.insert("snapshot".into(), serde_json::to_value(&snapshot)?);
        }
        let event = insert_event_tx(&tx, session_id, "plan.migrated", payload)?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(event)
    }

    pub fn get_plan_runtime_snapshot(&self, session_id: &str) -> Result<Option<String>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT plan_runtime_snapshot FROM sessions WHERE id = ?1",
            params![session_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map(|value| value.flatten())
        .map_err(Into::into)
    }

    /// Insert one immutable policy bundle and append `policy.snapshot` in the
    /// same transaction when the row is new. Returns `None` when the exact
    /// bundle already exists for the project.
    pub fn ensure_policy_snapshot_with_event(
        &self,
        session_id: &str,
        project_id: &str,
        hash: &str,
        json: &str,
    ) -> Result<Option<Event>> {
        if hash.is_empty() || json.is_empty() {
            return Err(anyhow!("policy snapshot needs a hash and canonical json"));
        }
        let canonical_hash = format!("{:x}", Sha256::digest(json.as_bytes()));
        if canonical_hash != hash {
            return Err(anyhow!("policy snapshot hash does not match its json"));
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let exists: bool = tx
            .query_row(
                "SELECT 1 FROM policy_snapshots WHERE project_id = ?1 AND hash = ?2",
                params![project_id, hash],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if exists {
            return Ok(None);
        }
        let created_at = now_rfc3339();
        tx.execute(
            "INSERT INTO policy_snapshots (project_id, hash, json, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![project_id, hash, json, created_at],
        )?;
        let payload = serde_json::json!({
            "project_id": project_id,
            "hash": hash,
            "json": json,
            "created_at": created_at,
        });
        let event = insert_event_tx(&tx, session_id, "policy.snapshot", payload)?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some(event))
    }

    pub fn get_policy_snapshot(
        &self,
        project_id: &str,
        hash: &str,
    ) -> Result<Option<vp_protocol::runtime::PolicySnapshot>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT project_id, hash, json, created_at
             FROM policy_snapshots WHERE project_id = ?1 AND hash = ?2",
            params![project_id, hash],
            |row| {
                Ok(vp_protocol::runtime::PolicySnapshot {
                    project_id: row.get(0)?,
                    hash: row.get(1)?,
                    json: row.get(2)?,
                    created_at: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn list_policy_snapshots(
        &self,
        project_id: &str,
        limit: u32,
    ) -> Result<Vec<vp_protocol::runtime::PolicySnapshot>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT project_id, hash, json, created_at
             FROM policy_snapshots WHERE project_id = ?1
             ORDER BY created_at ASC, hash ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            params![project_id, i64::from(limit.clamp(1, 10_000))],
            |row| {
                Ok(vp_protocol::runtime::PolicySnapshot {
                    project_id: row.get(0)?,
                    hash: row.get(1)?,
                    json: row.get(2)?,
                    created_at: row.get(3)?,
                })
            },
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Snapshot insertion for `db-rebuild` and bundle import. Existing rows are
    /// preserved; the hash is verified so a malformed archive cannot poison
    /// the policy projection.
    pub fn insert_policy_snapshot_snapshot(
        &self,
        snapshot: &vp_protocol::runtime::PolicySnapshot,
    ) -> Result<()> {
        let actual = format!("{:x}", Sha256::digest(snapshot.json.as_bytes()));
        if actual != snapshot.hash {
            return Err(anyhow!(
                "policy snapshot {} does not match its canonical json",
                snapshot.hash
            ));
        }
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO policy_snapshots (project_id, hash, json, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(project_id, hash) DO NOTHING",
            params![
                snapshot.project_id,
                snapshot.hash,
                snapshot.json,
                snapshot.created_at
            ],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use tempfile::TempDir;

    fn store_with_session() -> (TempDir, Store, Session) {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("plans"))
            .unwrap();
        let session = store.create_session(&project.id, "revision cas").unwrap();
        (data, store, session)
    }

    #[test]
    fn stale_revision_writer_cannot_overwrite_a_newer_plan() {
        let (_data, store, session) = store_with_session();
        store
            .set_team_plan_with_runtime_snapshot_and_event(
                &session.id,
                r#"{"goal":"submitted"}"#,
                None,
            )
            .unwrap();
        let current = store.plan_revision_count(&session.id).unwrap();
        assert_eq!(current, 1);

        let applied = store
            .compare_and_set_team_plan_revision_with_runtime_snapshot_and_event(
                &session.id,
                r#"{"goal":"revision two"}"#,
                "team_plan.revised",
                serde_json::json!({"revision": 2, "reason": "first writer"}),
                None,
                1,
            )
            .unwrap()
            .unwrap();
        assert_eq!(applied.kind, "team_plan.revised");
        assert_eq!(store.plan_revision_count(&session.id).unwrap(), 2);

        let stale = store
            .compare_and_set_team_plan_revision_with_runtime_snapshot_and_event(
                &session.id,
                r#"{"goal":"stale writer"}"#,
                "team_plan.revised",
                serde_json::json!({"revision": 2, "reason": "stale writer"}),
                None,
                1,
            )
            .unwrap();
        assert!(stale.is_none());
        assert_eq!(
            store.get_team_plan(&session.id).unwrap().as_deref(),
            Some(r#"{"goal":"revision two"}"#)
        );
        assert_eq!(store.plan_revision_count(&session.id).unwrap(), 2);

        // The payload-revision path used by fan-out has the same guard.
        let error = store
            .set_team_plan_revision_with_runtime_snapshot_and_event(
                &session.id,
                r#"{"goal":"fanout stale"}"#,
                "team_plan.revised",
                serde_json::json!({"revision": 2, "reason": "stale fanout"}),
                None,
            )
            .unwrap_err();
        assert!(
            error.to_string().contains("PLAN_REVISION_CONFLICT"),
            "{error}"
        );
        assert_eq!(
            store.get_team_plan(&session.id).unwrap().as_deref(),
            Some(r#"{"goal":"revision two"}"#)
        );
    }

    #[test]
    fn revision_metadata_without_a_submission_is_not_a_bootstrap_bypass() {
        let (_data, store, session) = store_with_session();
        let error = store
            .set_team_plan_revision_with_runtime_snapshot_and_event(
                &session.id,
                r#"{"goal":"no baseline"}"#,
                "team_plan.revised",
                serde_json::json!({"revision": 2, "reason": "stale bootstrap"}),
                None,
            )
            .unwrap_err();
        assert!(
            error.to_string().contains("PLAN_REVISION_CONFLICT"),
            "{error}"
        );
        assert!(store.get_team_plan(&session.id).unwrap().is_none());
        assert_eq!(store.plan_revision_count(&session.id).unwrap(), 0);
    }

    #[test]
    fn concurrent_revision_writers_with_the_same_expected_revision_have_one_winner() {
        let (_data, store, session) = store_with_session();
        store
            .set_team_plan_with_runtime_snapshot_and_event(
                &session.id,
                r#"{"goal":"submitted"}"#,
                None,
            )
            .unwrap();
        let store = Arc::new(store);
        let barrier = Arc::new(Barrier::new(2));
        let mut handles = Vec::new();
        for author in ["writer-a", "writer-b"] {
            let store = store.clone();
            let barrier = barrier.clone();
            let session_id = session.id.clone();
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                store
                    .compare_and_set_team_plan_revision_with_runtime_snapshot_and_event(
                        &session_id,
                        &format!(r#"{{"goal":"{author}"}}"#),
                        "team_plan.revised",
                        serde_json::json!({"revision": 2, "reason": author}),
                        None,
                        1,
                    )
                    .unwrap()
            }));
        }
        let results = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|event| event.is_some()).count(), 1);
        assert_eq!(store.plan_revision_count(&session.id).unwrap(), 2);
        let events = store.plan_lifecycle_events(&session.id).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].kind, "team_plan.revised");
        assert_eq!(events[1].payload["revision"], 2);
    }

    #[test]
    fn plan_lifecycle_history_is_not_limited_by_the_recent_event_window() {
        let (_data, store, session) = store_with_session();
        store
            .set_team_plan_with_runtime_snapshot_and_event(
                &session.id,
                r#"{"goal":"submitted"}"#,
                None,
            )
            .unwrap();
        for index in 0..2_100 {
            store
                .append_event(
                    &session.id,
                    "tool.finished",
                    serde_json::json!({"index": index}),
                )
                .unwrap();
        }
        store
            .compare_and_set_team_plan_revision_with_runtime_snapshot_and_event(
                &session.id,
                r#"{"goal":"revision two"}"#,
                "team_plan.revised",
                serde_json::json!({"revision": 2}),
                None,
                1,
            )
            .unwrap()
            .unwrap();
        assert_eq!(store.plan_revision_count(&session.id).unwrap(), 2);
        let events = store.plan_lifecycle_events(&session.id).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "team_plan.submitted");
        assert_eq!(events[1].payload["revision"], 2);
    }
}

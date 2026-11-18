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

    /// Persist a plan revision and its event atomically. The payload is
    /// supplied by the orchestration layer so the event carries the revision,
    /// author, reason, and task diff.
    pub fn set_team_plan_revision_with_event(
        &self,
        session_id: &str,
        plan: &str,
        kind: &str,
        payload: serde_json::Value,
    ) -> Result<Event> {
        self.set_team_plan_revision_with_runtime_snapshot_and_event(
            session_id, plan, kind, payload, None,
        )
    }

    /// Persist a plan revision, its optional runtime snapshot, and the plan
    /// lifecycle event atomically. The snapshot is stored on the session row
    /// and embedded in the event payload.
    pub fn set_team_plan_revision_with_runtime_snapshot_and_event(
        &self,
        session_id: &str,
        plan: &str,
        kind: &str,
        mut payload: serde_json::Value,
        runtime_snapshot_json: Option<&str>,
    ) -> Result<Event> {
        if !matches!(kind, "team_plan.submitted" | "team_plan.revised") {
            return Err(anyhow!("invalid team plan event kind: {kind}"));
        }
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
        if let (Some(snapshot_json), Some(object)) =
            (runtime_snapshot_json, payload.as_object_mut())
            && let Ok(snapshot) = serde_json::from_str::<serde_json::Value>(snapshot_json)
        {
            object.insert("runtime_snapshot".into(), snapshot);
        }
        let event = insert_event_tx(&tx, session_id, kind, payload)?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(event)
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

//! Team plans, plan tasks, stalls, and stall recovery.

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

    pub fn get_team_plan(&self, session_id: &str) -> Result<Option<String>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT team_plan FROM sessions WHERE id = ?1",
            params![session_id],
            |row| row.get(0),
        )
        .optional()
        .map(|value| value.flatten())
        .map_err(Into::into)
    }

    /// Rebuild the persistent task-DAG projection from the latest plan and the
    /// plan lifecycle events. This is intentionally idempotent: a plan resume
    /// can repair stale queue rows before it schedules work.
    pub fn sync_plan_tasks(
        &self,
        session_id: &str,
        project_id: &str,
        plan_seq: u64,
        plan: &TeamPlan,
    ) -> Result<Vec<ScheduledTask>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        struct TaskEventState {
            status: String,
            attempts: u32,
            started_at: Option<String>,
            finished_at: Option<String>,
            last_error: Option<String>,
            next_attempt_at: Option<String>,
        }
        let previous = {
            let mut stmt = tx.prepare(
                "SELECT task_id, plan_seq, enqueued_at FROM plan_tasks
                 WHERE session_id = ?1 ORDER BY plan_seq ASC",
            )?;
            let rows = stmt.query_map(params![session_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let plan_enqueued_at = tx
            .query_row(
                "SELECT created_at FROM events WHERE session_id = ?1 AND seq = ?2",
                params![session_id, plan_seq as i64],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .unwrap_or_else(|| now.clone());
        let mut states = std::collections::HashMap::<String, TaskEventState>::new();
        {
            let mut stmt = tx.prepare(
                "SELECT type, payload, created_at FROM events
                 WHERE session_id = ?1 AND seq > ?2
                   AND type IN ('plan_task.started', 'plan_task.finished',
                                'plan_task.interrupted', 'plan_task.retried',
                                'plan_task.skipped', 'plan_task.cancelling',
                                'plan_task.pausing', 'plan_task.paused', 'plan_task.resumed',
                                'plan_task.retry_scheduled', 'plan_task.blocked',
                                'plan_task.unblocked', 'plan_task.cancelled')
                 ORDER BY seq ASC",
            )?;
            let rows = stmt.query_map(params![session_id, plan_seq as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (kind, payload, created_at) = row?;
                let payload: serde_json::Value = serde_json::from_str(&payload).unwrap_or_default();
                let Some(task_id) = payload["task_id"].as_str() else {
                    continue;
                };
                let state = states.entry(task_id.to_string()).or_insert(TaskEventState {
                    status: "pending".to_string(),
                    attempts: 0,
                    started_at: None,
                    finished_at: None,
                    last_error: None,
                    next_attempt_at: None,
                });
                match kind.as_str() {
                    "plan_task.started" => {
                        if state.status != "completed" {
                            state.status = "interrupted".to_string();
                            state.finished_at = None;
                            state.last_error = None;
                        }
                        state.attempts = state.attempts.saturating_add(1);
                        state.started_at = Some(created_at);
                    }
                    "plan_task.finished" => {
                        let status = payload["task_status"].as_str().unwrap_or("failed");
                        state.status = status.to_string();
                        state.finished_at = Some(created_at);
                        state.next_attempt_at = None;
                        state.last_error = if status == "completed" {
                            None
                        } else {
                            payload["reason"]
                                .as_str()
                                .or_else(|| payload["summary"].as_str())
                                .map(str::to_string)
                        };
                    }
                    "plan_task.retry_scheduled" => {
                        state.status = "pending".to_string();
                        state.finished_at = None;
                        state.next_attempt_at =
                            payload["next_attempt_at"].as_str().map(str::to_string);
                        state.last_error = payload["reason"].as_str().map(str::to_string);
                    }
                    "plan_task.interrupted" => {
                        state.status = "interrupted".to_string();
                        state.finished_at = Some(created_at);
                        state.last_error = payload["reason"].as_str().map(str::to_string);
                    }
                    "plan_task.retried" => {
                        state.status = "pending".to_string();
                        state.finished_at = None;
                        state.last_error = None;
                    }
                    "plan_task.cancelling" => {
                        state.status = "cancelling".to_string();
                        state.finished_at = None;
                        state.last_error = payload["reason"].as_str().map(str::to_string);
                    }
                    "plan_task.pausing" => {
                        state.status = "pausing".to_string();
                        state.finished_at = None;
                        state.last_error = payload["reason"].as_str().map(str::to_string);
                    }
                    "plan_task.paused" => {
                        state.status = "paused".to_string();
                        state.finished_at = None;
                        state.last_error = payload["reason"].as_str().map(str::to_string);
                    }
                    "plan_task.resumed" => {
                        state.status = "running".to_string();
                        state.finished_at = None;
                        state.last_error = None;
                    }
                    "plan_task.skipped" => {
                        state.status = "skipped".to_string();
                        state.finished_at = Some(created_at);
                        state.last_error = payload["reason"].as_str().map(str::to_string);
                    }
                    "plan_task.blocked" => {
                        state.status = "blocked".to_string();
                        state.finished_at = None;
                        state.last_error = payload["reason"].as_str().map(str::to_string);
                    }
                    "plan_task.unblocked" => {
                        state.status = "pending".to_string();
                        state.finished_at = None;
                        state.last_error = None;
                        state.next_attempt_at = None;
                    }
                    "plan_task.cancelled" => {
                        state.status = "cancelled".to_string();
                        state.finished_at = Some(created_at);
                        state.last_error = payload["reason"].as_str().map(str::to_string);
                    }
                    _ => {}
                }
            }
        }
        let previous_map = previous
            .into_iter()
            .filter(|(_, previous_seq, _)| *previous_seq == plan_seq as i64)
            .map(|(task_id, _, enqueued_at)| (task_id, enqueued_at))
            .collect::<std::collections::HashMap<_, _>>();
        tx.execute(
            "DELETE FROM plan_tasks WHERE session_id = ?1",
            params![session_id],
        )?;
        // Progress is operational telemetry for the current revision; rows
        // from an older plan revision would never match a live task again.
        tx.execute(
            "DELETE FROM plan_task_progress WHERE session_id = ?1 AND plan_seq <> ?2",
            params![session_id, plan_seq as i64],
        )?;
        for task in &plan.tasks {
            let state = states.get(&task.id);
            let status = state
                .map(|state| state.status.as_str())
                .unwrap_or("pending");
            let attempts = state.map(|state| state.attempts).unwrap_or(0);
            let started_at = state.and_then(|state| state.started_at.as_deref());
            let finished_at = state.and_then(|state| state.finished_at.as_deref());
            let last_error = state.and_then(|state| state.last_error.as_deref());
            let enqueued_at = previous_map
                .get(&task.id)
                .cloned()
                .unwrap_or_else(|| plan_enqueued_at.clone());
            tx.execute(
                "INSERT INTO plan_tasks
                 (session_id, plan_seq, task_id, project_id, department, title, priority,
                  status, deps, write_scope, attempts, last_error, next_attempt_at, enqueued_at,
                  started_at, finished_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
                params![
                    session_id,
                    plan_seq as i64,
                    task.id,
                    project_id,
                    task.department,
                    task.title,
                    task.priority,
                    status,
                    serde_json::to_string(&task.deps)?,
                    serde_json::to_string(&task.write_scope)?,
                    i64::from(attempts),
                    last_error,
                    state.and_then(|state| state.next_attempt_at.as_deref()),
                    enqueued_at,
                    started_at,
                    finished_at,
                    now
                ],
            )?;
        }
        tx.commit()?;
        drop(conn);
        self.list_plan_tasks(Some(project_id), Some(session_id))
    }

    pub fn list_plan_tasks(
        &self,
        project_id: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<Vec<ScheduledTask>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT session_id, project_id, plan_seq, task_id, department, title, priority,
                    status, deps, write_scope, attempts, enqueued_at, started_at, finished_at,
                    last_error, next_attempt_at
             FROM plan_tasks
             WHERE (?1 IS NULL OR project_id = ?1) AND (?2 IS NULL OR session_id = ?2)
             ORDER BY project_id ASC, session_id ASC, plan_seq ASC, task_id ASC",
        )?;
        let rows = stmt.query_map(params![project_id, session_id], map_scheduled_task)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Operational progress rows, joined against the current plan-task
    /// projection so rows from an older plan revision are not reported.
    pub fn list_plan_task_progress(
        &self,
        project_id: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<Vec<PlanTaskProgress>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT p.session_id, p.project_id, p.plan_seq, p.task_id,
                    p.last_progress_at, p.stall_count, p.last_turn, p.last_tool, p.updated_at
             FROM plan_task_progress p
             JOIN plan_tasks t
               ON t.session_id = p.session_id AND t.plan_seq = p.plan_seq AND t.task_id = p.task_id
             WHERE (?1 IS NULL OR p.project_id = ?1) AND (?2 IS NULL OR p.session_id = ?2)
             ORDER BY p.session_id ASC, p.plan_seq ASC, p.task_id ASC",
        )?;
        let rows = stmt.query_map(params![project_id, session_id], map_plan_task_progress)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Record a bounded turn/tool-boundary observation for a task-scoped run.
    /// Returns false when the task is no longer active, so a late tool result
    /// cannot resurrect progress for a finished task.
    pub fn touch_plan_task_progress(
        &self,
        session_id: &str,
        task_id: &str,
        turn: u32,
        tool: Option<&str>,
    ) -> Result<bool> {
        let now = now_rfc3339();
        let conn = self.lock()?;
        let active = conn
            .query_row(
                "SELECT plan_seq, project_id FROM plan_tasks
                 WHERE session_id = ?1 AND task_id = ?2
                   AND status IN ('running', 'claimed')
                 ORDER BY plan_seq DESC LIMIT 1",
                params![session_id, task_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((plan_seq, project_id)) = active else {
            return Ok(false);
        };
        conn.execute(
            "INSERT INTO plan_task_progress
             (session_id, plan_seq, task_id, project_id, last_progress_at, stall_count,
              last_turn, last_tool, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7, ?5)
             ON CONFLICT(session_id, plan_seq, task_id) DO UPDATE SET
                last_progress_at = excluded.last_progress_at,
                last_turn = excluded.last_turn,
                last_tool = COALESCE(excluded.last_tool, plan_task_progress.last_tool),
                updated_at = excluded.updated_at",
            params![
                session_id,
                plan_seq,
                task_id,
                project_id,
                now,
                i64::from(turn),
                tool
            ],
        )?;
        Ok(true)
    }

    /// Observability-only stall sweep. It finds running/claimed plan tasks
    /// whose last observation (progress, attempt start, or enqueue time) is
    /// older than the threshold, emits one `orchestration.stall` event per
    /// threshold window, increments `stall_count`, and refreshes the
    /// observation timestamp. It never cancels, retries, or edits the task.
    pub fn sweep_task_stalls(&self, threshold: std::time::Duration) -> Result<Vec<TaskStall>> {
        self.sweep_task_stalls_with_events(threshold)
            .map(|(stalls, _)| stalls)
    }

    /// Same sweep, but also returns the inserted `orchestration.stall` events
    /// so the caller can broadcast them to live subscribers.
    pub fn sweep_task_stalls_with_events(
        &self,
        threshold: std::time::Duration,
    ) -> Result<(Vec<TaskStall>, Vec<Event>)> {
        self.sweep_task_stalls_with_events_at(threshold, time::OffsetDateTime::now_utc())
    }

    pub fn sweep_task_stalls_at(
        &self,
        threshold: std::time::Duration,
        now: time::OffsetDateTime,
    ) -> Result<Vec<TaskStall>> {
        self.sweep_task_stalls_with_events_at(threshold, now)
            .map(|(stalls, _)| stalls)
    }

    pub fn sweep_task_stalls_with_events_at(
        &self,
        threshold: std::time::Duration,
        now: time::OffsetDateTime,
    ) -> Result<(Vec<TaskStall>, Vec<Event>)> {
        if threshold.is_zero() {
            return Ok((Vec::new(), Vec::new()));
        }
        let threshold_nanos = threshold.as_nanos().min(i64::MAX as u128) as i64;
        let threshold = time::Duration::nanoseconds(threshold_nanos);
        let cutoff = now - threshold;
        let now_string = now
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| now_rfc3339());
        struct Candidate {
            session_id: String,
            project_id: String,
            plan_seq: i64,
            task_id: String,
            department: String,
            title: String,
            status: String,
            attempts: u32,
            observed_at: String,
            stall_count: u32,
            last_turn: Option<u32>,
            last_tool: Option<String>,
        }
        let candidates = {
            let conn = self.lock()?;
            let mut stmt = conn.prepare(
                "SELECT t.session_id, t.project_id, t.plan_seq, t.task_id, t.department,
                        t.title, t.status, t.attempts,
                        COALESCE(p.last_progress_at, t.started_at, t.enqueued_at),
                        COALESCE(p.stall_count, 0), p.last_turn, p.last_tool
                 FROM plan_tasks t
                 LEFT JOIN plan_task_progress p
                   ON p.session_id = t.session_id AND p.plan_seq = t.plan_seq
                  AND p.task_id = t.task_id
                 WHERE t.status IN ('running', 'claimed')
                 ORDER BY t.session_id ASC, t.plan_seq ASC, t.task_id ASC",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(Candidate {
                    session_id: row.get(0)?,
                    project_id: row.get(1)?,
                    plan_seq: row.get(2)?,
                    task_id: row.get(3)?,
                    department: row.get(4)?,
                    title: row.get(5)?,
                    status: row.get(6)?,
                    attempts: row.get::<_, i64>(7)?.max(0) as u32,
                    observed_at: row.get(8)?,
                    stall_count: row.get::<_, i64>(9)?.max(0) as u32,
                    last_turn: row
                        .get::<_, Option<i64>>(10)?
                        .and_then(|value| u32::try_from(value).ok()),
                    last_tool: row.get(11)?,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut stalls = Vec::new();
        let mut events = Vec::new();
        {
            let mut conn = self.lock()?;
            let tx = conn.transaction()?;
            for candidate in candidates {
                let Ok(observed) = time::OffsetDateTime::parse(
                    &candidate.observed_at,
                    &time::format_description::well_known::Rfc3339,
                ) else {
                    continue;
                };
                if observed > cutoff {
                    continue;
                }
                let still_active: bool = tx
                    .query_row(
                        "SELECT 1 FROM plan_tasks
                         WHERE session_id = ?1 AND plan_seq = ?2 AND task_id = ?3
                           AND status IN ('running', 'claimed')",
                        params![candidate.session_id, candidate.plan_seq, candidate.task_id],
                        |_| Ok(true),
                    )
                    .optional()?
                    .unwrap_or(false);
                if !still_active {
                    continue;
                }
                let age_secs = (now - observed).whole_seconds().max(0) as u64;
                let stall_count = candidate.stall_count.saturating_add(1);
                tx.execute(
                    "INSERT INTO plan_task_progress
                     (session_id, plan_seq, task_id, project_id, last_progress_at, stall_count,
                      last_turn, last_tool, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?5)
                     ON CONFLICT(session_id, plan_seq, task_id) DO UPDATE SET
                        last_progress_at = excluded.last_progress_at,
                        stall_count = excluded.stall_count,
                        last_turn = COALESCE(excluded.last_turn, plan_task_progress.last_turn),
                        last_tool = COALESCE(excluded.last_tool, plan_task_progress.last_tool),
                        updated_at = excluded.updated_at",
                    params![
                        candidate.session_id,
                        candidate.plan_seq,
                        candidate.task_id,
                        candidate.project_id,
                        now_string,
                        i64::from(stall_count),
                        candidate.last_turn.map(i64::from),
                        candidate.last_tool
                    ],
                )?;
                let stall = TaskStall {
                    session_id: candidate.session_id.clone(),
                    project_id: candidate.project_id,
                    plan_seq: candidate.plan_seq.max(0) as u64,
                    task_id: candidate.task_id,
                    department: candidate.department,
                    title: candidate.title,
                    status: candidate.status,
                    attempts: candidate.attempts,
                    age_secs,
                    stall_count,
                    last_progress_at: candidate.observed_at,
                    last_turn: candidate.last_turn,
                    last_tool: candidate.last_tool,
                    observed_at: now_string.clone(),
                };
                events.push(insert_event_tx(
                    &tx,
                    &stall.session_id,
                    "orchestration.stall",
                    serde_json::to_value(&stall)?,
                )?);
                stalls.push(stall);
            }
            tx.commit()?;
        }
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok((stalls, events))
    }

    /// Sessions with retryable DAG tasks that are idle/failed/cancelled and
    /// therefore safe for the opt-in global resume scheduler to pick up.
    pub fn pending_task_sessions(
        &self,
        global_enabled: bool,
        default_cooldown_secs: u64,
    ) -> Result<Vec<PendingTaskSessionRow>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT t.session_id, t.project_id, s.priority, COUNT(*), MIN(t.enqueued_at),
                    MAX(t.priority), COALESCE(p.cooldown_secs, ?2)
             FROM plan_tasks t
             JOIN sessions s ON s.id = t.session_id
             LEFT JOIN project_task_scheduler_policies p ON p.project_id = t.project_id
             WHERE t.status IN ('pending', 'interrupted', 'failed', 'cancelled')
               AND s.status IN ('idle', 'failed', 'cancelled')
               AND (t.next_attempt_at IS NULL OR t.next_attempt_at <= ?3)
               AND COALESCE(p.enabled, ?1) = 1
             GROUP BY t.session_id, t.project_id, s.priority, p.cooldown_secs
             ORDER BY s.priority DESC, MIN(t.enqueued_at) ASC, t.session_id ASC",
        )?;
        let rows = stmt.query_map(
            params![
                if global_enabled { 1 } else { 0 },
                default_cooldown_secs as i64,
                now_rfc3339()
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i32>(2)?,
                    row.get::<_, i64>(3)?.max(0) as usize,
                    row.get::<_, String>(4)?,
                    row.get::<_, i32>(5)?,
                    row.get::<_, i64>(6)?.max(1) as u64,
                ))
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn claim_plan_tasks(
        &self,
        session_id: &str,
        plan_seq: u64,
        task_ids: &[String],
    ) -> Result<Vec<ScheduledTask>> {
        let (tasks, _events) = self.claim_plan_tasks_with_events(session_id, plan_seq, task_ids)?;
        Ok(tasks)
    }

    /// Claim tasks and append the `plan_task.started` events in the same
    /// transaction. A claim without its event would leave attempts/status
    /// ahead of the event log and force a replay repair after a crash.
    pub fn claim_plan_tasks_with_events(
        &self,
        session_id: &str,
        plan_seq: u64,
        task_ids: &[String],
    ) -> Result<(Vec<ScheduledTask>, Vec<Event>)> {
        let now = now_rfc3339();
        let (claimed, events) = {
            let mut conn = self.lock()?;
            let tx = conn.transaction()?;
            let mut claimed = Vec::new();
            let mut events = Vec::new();
            for task_id in task_ids {
                let changed = tx.execute(
                    "UPDATE plan_tasks
                     SET status = 'running', attempts = attempts + 1, started_at = ?1,
                         finished_at = NULL, last_error = NULL, next_attempt_at = NULL,
                         updated_at = ?1
                     WHERE session_id = ?2 AND plan_seq = ?3 AND task_id = ?4
                       AND status IN ('pending', 'interrupted', 'failed', 'blocked', 'cancelled')
                       AND (next_attempt_at IS NULL OR next_attempt_at <= ?1)",
                    params![now, session_id, plan_seq as i64, task_id],
                )?;
                if changed == 0 {
                    continue;
                }
                tx.execute(
                    "INSERT INTO plan_task_progress
                     (session_id, plan_seq, task_id, project_id, last_progress_at, stall_count,
                      last_turn, last_tool, updated_at)
                     SELECT session_id, plan_seq, task_id, project_id, ?1, 0, NULL, NULL, ?1
                     FROM plan_tasks
                     WHERE session_id = ?2 AND plan_seq = ?3 AND task_id = ?4
                     ON CONFLICT(session_id, plan_seq, task_id) DO UPDATE SET
                        last_progress_at = excluded.last_progress_at,
                        last_turn = NULL,
                        last_tool = NULL,
                        updated_at = excluded.updated_at",
                    params![now, session_id, plan_seq as i64, task_id],
                )?;
                let task = tx.query_row(
                    "SELECT session_id, project_id, plan_seq, task_id, department, title, priority,
                            status, deps, write_scope, attempts, enqueued_at, started_at, finished_at,
                            last_error, next_attempt_at
                     FROM plan_tasks
                     WHERE session_id = ?1 AND plan_seq = ?2 AND task_id = ?3",
                    params![session_id, plan_seq as i64, task_id],
                    map_scheduled_task,
                )?;
                events.push(insert_event_tx(
                    &tx,
                    session_id,
                    "plan_task.started",
                    serde_json::json!({
                        "task_id": task.task_id,
                        "task_status": "running",
                        "department": task.department,
                        "title": task.title,
                        "attempts": task.attempts,
                    }),
                )?);
                claimed.push(task);
            }
            tx.commit()?;
            (claimed, events)
        };
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok((claimed, events))
    }

    pub fn finish_plan_task(
        &self,
        session_id: &str,
        plan_seq: u64,
        task_id: &str,
        status: &str,
        detail: Option<&str>,
    ) -> Result<Option<ScheduledTask>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE plan_tasks
             SET status = ?1, last_error = ?2, finished_at = ?3, next_attempt_at = NULL,
                 updated_at = ?3
             WHERE session_id = ?4 AND plan_seq = ?5 AND task_id = ?6",
            params![status, detail, now, session_id, plan_seq as i64, task_id],
        )?;
        let task = tx
            .query_row(
                "SELECT session_id, project_id, plan_seq, task_id, department, title, priority,
                        status, deps, write_scope, attempts, enqueued_at, started_at, finished_at,
                        last_error, next_attempt_at
                 FROM plan_tasks
                 WHERE session_id = ?1 AND plan_seq = ?2 AND task_id = ?3",
                params![session_id, plan_seq as i64, task_id],
                map_scheduled_task,
            )
            .optional()?;
        tx.commit()?;
        Ok(task)
    }

    /// Schedule an automatic retry: the task returns to pending with a
    /// next_attempt_at gate and an event that replay/repair can reconstruct.
    pub fn schedule_plan_task_retry_with_event(
        &self,
        session_id: &str,
        plan_seq: u64,
        task_id: &str,
        next_attempt_at: &str,
        reason: &str,
    ) -> Result<Option<(ScheduledTask, Event)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE plan_tasks
             SET status = 'pending', finished_at = NULL, last_error = ?1,
                 next_attempt_at = ?2, updated_at = ?3
             WHERE session_id = ?4 AND plan_seq = ?5 AND task_id = ?6
               AND status IN ('failed', 'blocked', 'interrupted', 'cancelled')",
            params![
                reason,
                next_attempt_at,
                now,
                session_id,
                plan_seq as i64,
                task_id
            ],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let task = tx.query_row(
            "SELECT session_id, project_id, plan_seq, task_id, department, title, priority,
                    status, deps, write_scope, attempts, enqueued_at, started_at, finished_at,
                    last_error, next_attempt_at
             FROM plan_tasks WHERE session_id = ?1 AND plan_seq = ?2 AND task_id = ?3",
            params![session_id, plan_seq, task_id],
            map_scheduled_task,
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "plan_task.retry_scheduled",
            serde_json::json!({
                "task_id": task_id,
                "task_status": "pending",
                "attempt": task.attempts,
                "next_attempt_at": next_attempt_at,
                "reason": reason,
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((task, event)))
    }

    /// Move a task into `blocked` because an upstream dependency failed
    /// permanently. Returns `None` when the task has already moved on.
    pub fn block_plan_task_with_event(
        &self,
        session_id: &str,
        plan_seq: u64,
        task_id: &str,
        reason: &str,
        dependency: &str,
    ) -> Result<Option<(ScheduledTask, Event)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE plan_tasks
             SET status = 'blocked', last_error = ?1, finished_at = NULL,
                 next_attempt_at = NULL, updated_at = ?2
             WHERE session_id = ?3 AND plan_seq = ?4 AND task_id = ?5
               AND status IN ('pending', 'interrupted', 'retry_scheduled')",
            params![reason, now, session_id, plan_seq as i64, task_id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let task = tx.query_row(
            "SELECT session_id, project_id, plan_seq, task_id, department, title, priority,
                    status, deps, write_scope, attempts, enqueued_at, started_at, finished_at,
                    last_error, next_attempt_at
             FROM plan_tasks WHERE session_id = ?1 AND plan_seq = ?2 AND task_id = ?3",
            params![session_id, plan_seq as i64, task_id],
            map_scheduled_task,
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "plan_task.blocked",
            serde_json::json!({
                "task_id": task_id,
                "task_status": "blocked",
                "dependency": dependency,
                "reason": reason,
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((task, event)))
    }

    /// Return a blocked task to pending once all dependencies are satisfied.
    pub fn unblock_plan_task_with_event(
        &self,
        session_id: &str,
        plan_seq: u64,
        task_id: &str,
        reason: &str,
    ) -> Result<Option<(ScheduledTask, Event)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE plan_tasks
             SET status = 'pending', last_error = NULL, finished_at = NULL,
                 next_attempt_at = NULL, updated_at = ?1
             WHERE session_id = ?2 AND plan_seq = ?3 AND task_id = ?4
               AND status = 'blocked'",
            params![now, session_id, plan_seq as i64, task_id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let task = tx.query_row(
            "SELECT session_id, project_id, plan_seq, task_id, department, title, priority,
                    status, deps, write_scope, attempts, enqueued_at, started_at, finished_at,
                    last_error, next_attempt_at
             FROM plan_tasks WHERE session_id = ?1 AND plan_seq = ?2 AND task_id = ?3",
            params![session_id, plan_seq as i64, task_id],
            map_scheduled_task,
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "plan_task.unblocked",
            serde_json::json!({
                "task_id": task_id,
                "task_status": "pending",
                "reason": reason,
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((task, event)))
    }

    /// Cancel a pending/blocked task, used by the dependency-failure policy.
    pub fn cancel_plan_task_with_event(
        &self,
        session_id: &str,
        plan_seq: u64,
        task_id: &str,
        reason: &str,
        dependency: &str,
    ) -> Result<Option<(ScheduledTask, Event)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE plan_tasks
             SET status = 'cancelled', last_error = ?1, finished_at = ?2,
                 next_attempt_at = NULL, updated_at = ?2
             WHERE session_id = ?3 AND plan_seq = ?4 AND task_id = ?5
               AND status IN ('pending', 'blocked', 'interrupted', 'failed', 'retry_scheduled')",
            params![reason, now, session_id, plan_seq as i64, task_id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let task = tx.query_row(
            "SELECT session_id, project_id, plan_seq, task_id, department, title, priority,
                    status, deps, write_scope, attempts, enqueued_at, started_at, finished_at,
                    last_error, next_attempt_at
             FROM plan_tasks WHERE session_id = ?1 AND plan_seq = ?2 AND task_id = ?3",
            params![session_id, plan_seq as i64, task_id],
            map_scheduled_task,
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "plan_task.cancelled",
            serde_json::json!({
                "task_id": task_id,
                "task_status": "cancelled",
                "dependency": dependency,
                "reason": reason,
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((task, event)))
    }

    /// Retry a terminal/blocked task after the session is no longer active.
    /// The task returns to `pending`; the next plan resume or global scheduler
    /// tick can claim it. The projection update and event are atomic.
    pub fn retry_plan_task_with_event(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<(ScheduledTask, Event)> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let (plan_seq, status, session_status) = tx
            .query_row(
                "SELECT t.plan_seq, t.status, s.status
                 FROM plan_tasks t JOIN sessions s ON s.id = t.session_id
                 WHERE t.session_id = ?1 AND t.task_id = ?2
                 ORDER BY t.plan_seq DESC LIMIT 1",
                params![session_id, task_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| anyhow!("task not found: {task_id}"))?;
        if !matches!(
            session_status.as_str(),
            "idle" | "failed" | "cancelled" | "draft"
        ) {
            return Err(anyhow!(
                "session {session_id} is {session_status}; stop it before retrying a task"
            ));
        }
        if !matches!(
            status.as_str(),
            "failed" | "blocked" | "interrupted" | "cancelled" | "skipped"
        ) {
            return Err(anyhow!("task {task_id} is {status}, not retryable"));
        }
        tx.execute(
            "UPDATE plan_tasks
             SET status = 'pending', finished_at = NULL, last_error = NULL,
                 next_attempt_at = NULL, updated_at = ?1
             WHERE session_id = ?2 AND plan_seq = ?3 AND task_id = ?4",
            params![now, session_id, plan_seq, task_id],
        )?;
        let task = tx.query_row(
            "SELECT session_id, project_id, plan_seq, task_id, department, title, priority,
                    status, deps, write_scope, attempts, enqueued_at, started_at, finished_at,
                    last_error, next_attempt_at
             FROM plan_tasks WHERE session_id = ?1 AND plan_seq = ?2 AND task_id = ?3",
            params![session_id, plan_seq, task_id],
            map_scheduled_task,
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "plan_task.retried",
            serde_json::json!({
                "task_id": task_id,
                "task_status": "pending",
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((task, event))
    }

    pub fn get_orchestration_recovery(
        &self,
        session_id: &str,
        plan_seq: u64,
        task_id: &str,
    ) -> Result<Option<OrchestrationRecovery>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT session_id, project_id, plan_seq, task_id, department, action, status,
                    attempts, last_stall_count, requested_at, updated_at, last_error,
                    next_action_at
             FROM orchestration_recoveries
             WHERE session_id = ?1 AND plan_seq = ?2 AND task_id = ?3",
            params![session_id, plan_seq as i64, task_id],
            map_orchestration_recovery,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Every recovery row for the session, including rows from older plan
    /// revisions. Replay compares this historical set with the event stream.
    pub fn list_orchestration_recoveries(
        &self,
        project_id: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<Vec<OrchestrationRecovery>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT session_id, project_id, plan_seq, task_id, department, action, status,
                    attempts, last_stall_count, requested_at, updated_at, last_error,
                    next_action_at
             FROM orchestration_recoveries
             WHERE (?1 IS NULL OR project_id = ?1) AND (?2 IS NULL OR session_id = ?2)
             ORDER BY session_id ASC, plan_seq ASC, task_id ASC",
        )?;
        let rows = stmt.query_map(params![project_id, session_id], map_orchestration_recovery)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Recovery rows joined against the current plan-task projection so rows
    /// from an older plan revision are not reported by the live surfaces.
    pub fn list_current_orchestration_recoveries(
        &self,
        project_id: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<Vec<OrchestrationRecovery>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT r.session_id, r.project_id, r.plan_seq, r.task_id, r.department,
                    r.action, r.status, r.attempts, r.last_stall_count, r.requested_at,
                    r.updated_at, r.last_error, r.next_action_at
             FROM orchestration_recoveries r
             JOIN plan_tasks t
               ON t.session_id = r.session_id AND t.plan_seq = r.plan_seq
              AND t.task_id = r.task_id
             WHERE (?1 IS NULL OR r.project_id = ?1) AND (?2 IS NULL OR r.session_id = ?2)
             ORDER BY r.session_id ASC, r.plan_seq ASC, r.task_id ASC",
        )?;
        let rows = stmt.query_map(params![project_id, session_id], map_orchestration_recovery)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Recovery requests that still wait for cooperative cancellation and
    /// session quiescence. This is the bounded scan for the recovery tick.
    pub fn list_active_orchestration_recoveries(
        &self,
        limit: u32,
    ) -> Result<Vec<OrchestrationRecovery>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT r.session_id, r.project_id, r.plan_seq, r.task_id, r.department,
                    r.action, r.status, r.attempts, r.last_stall_count, r.requested_at,
                    r.updated_at, r.last_error, r.next_action_at
             FROM orchestration_recoveries r
             JOIN plan_tasks t
               ON t.session_id = r.session_id AND t.plan_seq = r.plan_seq
              AND t.task_id = r.task_id
             WHERE r.status IN ('requested', 'waiting_cancel')
             ORDER BY r.updated_at ASC, r.requested_at ASC, r.session_id ASC,
                      r.plan_seq ASC, r.task_id ASC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(
            params![i64::from(limit.clamp(1, 500))],
            map_orchestration_recovery,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn replace_orchestration_recoveries(
        &self,
        session_id: &str,
        recoveries: &[OrchestrationRecovery],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM orchestration_recoveries WHERE session_id = ?1",
            params![session_id],
        )?;
        for recovery in recoveries {
            if recovery.session_id != session_id {
                return Err(anyhow!(
                    "recovery row {} belongs to a different session",
                    recovery.task_id
                ));
            }
            upsert_orchestration_recovery(&tx, recovery)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Record one per-window escalation. The task keeps running; this only
    /// writes the recovery projection, its event, and the caller's optional
    /// coordination note.
    pub fn escalate_stall_recovery(
        &self,
        stall: &TaskStall,
        reason: &str,
    ) -> Result<Option<(OrchestrationRecovery, Event)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let existing = recovery_row_tx(&tx, &stall.session_id, stall.plan_seq, &stall.task_id)?;
        if let Some(existing) = &existing
            && matches!(
                existing.status.as_str(),
                "requested" | "waiting_cancel" | "failed" | "unnecessary"
            )
        {
            return Ok(None);
        }
        if existing
            .as_ref()
            .is_some_and(|existing| existing.last_stall_count >= stall.stall_count)
        {
            return Ok(None);
        }
        let recovery = OrchestrationRecovery {
            session_id: stall.session_id.clone(),
            project_id: stall.project_id.clone(),
            plan_seq: stall.plan_seq,
            task_id: stall.task_id.clone(),
            department: stall.department.clone(),
            action: "escalate".to_string(),
            status: "escalated".to_string(),
            attempts: existing.as_ref().map(|row| row.attempts).unwrap_or(0),
            last_stall_count: stall.stall_count,
            requested_at: existing
                .as_ref()
                .map(|row| row.requested_at.clone())
                .unwrap_or_else(|| now.clone()),
            updated_at: now,
            last_error: Some(reason.to_string()),
            next_action_at: None,
        };
        upsert_orchestration_recovery(&tx, &recovery)?;
        let event = insert_event_tx(
            &tx,
            &recovery.session_id,
            "orchestration.recovery_escalated",
            serde_json::to_value(&recovery)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((recovery, event)))
    }

    /// Persist a recovery intent before the caller requests cooperative task
    /// cancellation. Returns `None` when another request is already in flight,
    /// the stall window was already handled, or the task left `running`/
    /// `claimed` before the transaction started.
    pub fn begin_stall_retry_recovery(
        &self,
        stall: &TaskStall,
        max_attempts: u32,
        grace_secs: u64,
        reason: &str,
    ) -> Result<Option<(OrchestrationRecovery, Event)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let still_active: bool = tx
            .query_row(
                "SELECT 1 FROM plan_tasks
                 WHERE session_id = ?1 AND plan_seq = ?2 AND task_id = ?3
                   AND status IN ('running', 'claimed')",
                params![stall.session_id, stall.plan_seq as i64, stall.task_id],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !still_active {
            return Ok(None);
        }
        let existing = recovery_row_tx(&tx, &stall.session_id, stall.plan_seq, &stall.task_id)?;
        if let Some(existing) = &existing {
            if matches!(
                existing.status.as_str(),
                "requested" | "waiting_cancel" | "failed" | "unnecessary"
            ) {
                return Ok(None);
            }
            if existing.last_stall_count >= stall.stall_count || existing.attempts >= max_attempts {
                return Ok(None);
            }
        }
        if max_attempts == 0 {
            return Ok(None);
        }
        let attempts = existing
            .as_ref()
            .map(|row| row.attempts)
            .unwrap_or(0)
            .saturating_add(1);
        let deadline = time::OffsetDateTime::now_utc()
            + time::Duration::seconds(grace_secs.min(86_400) as i64);
        let next_action_at = deadline
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| now.clone());
        let recovery = OrchestrationRecovery {
            session_id: stall.session_id.clone(),
            project_id: stall.project_id.clone(),
            plan_seq: stall.plan_seq,
            task_id: stall.task_id.clone(),
            department: stall.department.clone(),
            action: "retry".to_string(),
            status: "requested".to_string(),
            attempts,
            last_stall_count: stall.stall_count,
            requested_at: now.clone(),
            updated_at: now,
            last_error: Some(reason.to_string()),
            next_action_at: Some(next_action_at),
        };
        upsert_orchestration_recovery(&tx, &recovery)?;
        let event = insert_event_tx(
            &tx,
            &recovery.session_id,
            "orchestration.recovery_requested",
            serde_json::to_value(&recovery)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((recovery, event)))
    }

    pub fn mark_stall_recovery_waiting_cancel(
        &self,
        session_id: &str,
        plan_seq: u64,
        task_id: &str,
        reason: &str,
    ) -> Result<Option<(OrchestrationRecovery, Event)>> {
        self.transition_stall_recovery(
            session_id,
            plan_seq,
            task_id,
            &["requested"],
            "waiting_cancel",
            "orchestration.recovery_waiting_cancel",
            Some(reason),
            false,
        )
    }

    pub fn finish_stall_recovery_unnecessary(
        &self,
        session_id: &str,
        plan_seq: u64,
        task_id: &str,
        reason: &str,
    ) -> Result<Option<(OrchestrationRecovery, Event)>> {
        self.transition_stall_recovery(
            session_id,
            plan_seq,
            task_id,
            &["requested", "waiting_cancel"],
            "unnecessary",
            "orchestration.recovery_unnecessary",
            Some(reason),
            true,
        )
    }

    pub fn complete_stall_recovery_retry(
        &self,
        session_id: &str,
        plan_seq: u64,
        task_id: &str,
    ) -> Result<Option<(OrchestrationRecovery, Event)>> {
        self.transition_stall_recovery(
            session_id,
            plan_seq,
            task_id,
            &["requested", "waiting_cancel"],
            "retried",
            "orchestration.recovery_retried",
            None,
            true,
        )
    }

    pub fn fail_stall_recovery(
        &self,
        session_id: &str,
        plan_seq: u64,
        task_id: &str,
        reason: &str,
    ) -> Result<Option<(OrchestrationRecovery, Event)>> {
        self.transition_stall_recovery(
            session_id,
            plan_seq,
            task_id,
            &["requested", "waiting_cancel", "retried"],
            "failed",
            "orchestration.recovery_failed",
            Some(reason),
            true,
        )
    }

    /// Fail a stall recovery that never got a request row, for example when
    /// `stall_max_attempts` is zero or the session budget is already exhausted.
    /// The event is still durable so replay and operators can see why no retry
    /// was attempted.
    pub fn fail_stall_recovery_for_stall(
        &self,
        stall: &TaskStall,
        reason: &str,
    ) -> Result<Option<(OrchestrationRecovery, Event)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let existing = recovery_row_tx(&tx, &stall.session_id, stall.plan_seq, &stall.task_id)?;
        let Some(existing) = existing else {
            let recovery = OrchestrationRecovery {
                session_id: stall.session_id.clone(),
                project_id: stall.project_id.clone(),
                plan_seq: stall.plan_seq,
                task_id: stall.task_id.clone(),
                department: stall.department.clone(),
                action: "retry".to_string(),
                status: "failed".to_string(),
                attempts: 0,
                last_stall_count: stall.stall_count,
                requested_at: now.clone(),
                updated_at: now,
                last_error: Some(reason.to_string()),
                next_action_at: None,
            };
            upsert_orchestration_recovery(&tx, &recovery)?;
            let event = insert_event_tx(
                &tx,
                &recovery.session_id,
                "orchestration.recovery_failed",
                serde_json::to_value(&recovery)?,
            )?;
            tx.commit()?;
            if let Err(error) = self.append_event_mirror(&event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
            return Ok(Some((recovery, event)));
        };
        if !matches!(
            existing.status.as_str(),
            "requested" | "waiting_cancel" | "retried"
        ) {
            return Ok(None);
        }
        let recovery = OrchestrationRecovery {
            status: "failed".to_string(),
            updated_at: now,
            last_error: Some(reason.to_string()),
            next_action_at: None,
            ..existing
        };
        upsert_orchestration_recovery(&tx, &recovery)?;
        let event = insert_event_tx(
            &tx,
            &recovery.session_id,
            "orchestration.recovery_failed",
            serde_json::to_value(&recovery)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((recovery, event)))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn transition_stall_recovery(
        &self,
        session_id: &str,
        plan_seq: u64,
        task_id: &str,
        from: &[&str],
        status: &str,
        event_kind: &str,
        last_error: Option<&str>,
        clear_next_action: bool,
    ) -> Result<Option<(OrchestrationRecovery, Event)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut recovery) = recovery_row_tx(&tx, session_id, plan_seq, task_id)? else {
            return Ok(None);
        };
        if !from.contains(&recovery.status.as_str()) {
            return Ok(None);
        }
        recovery.status = status.to_string();
        recovery.updated_at = now;
        recovery.last_error = last_error.map(str::to_string);
        if clear_next_action {
            recovery.next_action_at = None;
        }
        upsert_orchestration_recovery(&tx, &recovery)?;
        let event = insert_event_tx(
            &tx,
            session_id,
            event_kind,
            serde_json::to_value(&recovery)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((recovery, event)))
    }

    /// Request a cooperative pause of a running plan task. The task enters
    /// `pausing`; the worker transitions it to `paused` at its next boundary.
    pub fn pause_plan_task_with_event(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<(ScheduledTask, Event)> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let (plan_seq, status, session_status) = tx
            .query_row(
                "SELECT t.plan_seq, t.status, s.status
                 FROM plan_tasks t JOIN sessions s ON s.id = t.session_id
                 WHERE t.session_id = ?1 AND t.task_id = ?2
                 ORDER BY t.plan_seq DESC LIMIT 1",
                params![session_id, task_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| anyhow!("task not found: {task_id}"))?;
        if session_status != "running" {
            return Err(anyhow!(
                "session {session_id} is {session_status}; pause is only available while running"
            ));
        }
        if status != "running" {
            return Err(anyhow!("task {task_id} is {status}, not running"));
        }
        tx.execute(
            "UPDATE plan_tasks
             SET status = 'pausing', last_error = 'pause requested by user', updated_at = ?1
             WHERE session_id = ?2 AND plan_seq = ?3 AND task_id = ?4",
            params![now, session_id, plan_seq, task_id],
        )?;
        let task = tx.query_row(
            "SELECT session_id, project_id, plan_seq, task_id, department, title, priority,
                    status, deps, write_scope, attempts, enqueued_at, started_at, finished_at,
                    last_error, next_attempt_at
             FROM plan_tasks WHERE session_id = ?1 AND plan_seq = ?2 AND task_id = ?3",
            params![session_id, plan_seq, task_id],
            map_scheduled_task,
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "plan_task.pausing",
            serde_json::json!({
                "task_id": task_id,
                "task_status": "pausing",
                "reason": "pause requested by user",
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((task, event))
    }

    pub fn mark_plan_task_paused_with_event(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<Option<(ScheduledTask, Event)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let (plan_seq, status) = tx
            .query_row(
                "SELECT plan_seq, status FROM plan_tasks
                 WHERE session_id = ?1 AND task_id = ?2
                 ORDER BY plan_seq DESC LIMIT 1",
                params![session_id, task_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
            .ok_or_else(|| anyhow!("task not found: {task_id}"))?;
        if status != "pausing" {
            return Ok(None);
        }
        tx.execute(
            "UPDATE plan_tasks
             SET status = 'paused', last_error = 'paused by user', updated_at = ?1
             WHERE session_id = ?2 AND plan_seq = ?3 AND task_id = ?4",
            params![now, session_id, plan_seq, task_id],
        )?;
        let task = tx.query_row(
            "SELECT session_id, project_id, plan_seq, task_id, department, title, priority,
                    status, deps, write_scope, attempts, enqueued_at, started_at, finished_at,
                    last_error, next_attempt_at
             FROM plan_tasks WHERE session_id = ?1 AND plan_seq = ?2 AND task_id = ?3",
            params![session_id, plan_seq, task_id],
            map_scheduled_task,
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "plan_task.paused",
            serde_json::json!({
                "task_id": task_id,
                "task_status": "paused",
                "reason": "paused by user",
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((task, event)))
    }

    pub fn resume_plan_task_with_event(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<(ScheduledTask, Event)> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let (plan_seq, status, session_status) = tx
            .query_row(
                "SELECT t.plan_seq, t.status, s.status
                 FROM plan_tasks t JOIN sessions s ON s.id = t.session_id
                 WHERE t.session_id = ?1 AND t.task_id = ?2
                 ORDER BY t.plan_seq DESC LIMIT 1",
                params![session_id, task_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| anyhow!("task not found: {task_id}"))?;
        if !matches!(session_status.as_str(), "running" | "paused") {
            return Err(anyhow!(
                "session {session_id} is {session_status}; resume is only available while active"
            ));
        }
        if !matches!(status.as_str(), "pausing" | "paused") {
            return Err(anyhow!("task {task_id} is {status}, not paused"));
        }
        tx.execute(
            "UPDATE plan_tasks
             SET status = 'running', last_error = NULL, updated_at = ?1
             WHERE session_id = ?2 AND plan_seq = ?3 AND task_id = ?4",
            params![now, session_id, plan_seq, task_id],
        )?;
        let task = tx.query_row(
            "SELECT session_id, project_id, plan_seq, task_id, department, title, priority,
                    status, deps, write_scope, attempts, enqueued_at, started_at, finished_at,
                    last_error, next_attempt_at
             FROM plan_tasks WHERE session_id = ?1 AND plan_seq = ?2 AND task_id = ?3",
            params![session_id, plan_seq, task_id],
            map_scheduled_task,
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "plan_task.resumed",
            serde_json::json!({
                "task_id": task_id,
                "task_status": "running",
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((task, event))
    }

    /// Request cooperative cancellation of a running plan task. The worker
    /// checks this status at turn/tool boundaries and exits without aborting
    /// sibling tasks.
    pub fn request_plan_task_cancel_with_event(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<(ScheduledTask, Event)> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let (plan_seq, status) = tx
            .query_row(
                "SELECT plan_seq, status FROM plan_tasks
                 WHERE session_id = ?1 AND task_id = ?2
                 ORDER BY plan_seq DESC LIMIT 1",
                params![session_id, task_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
            .ok_or_else(|| anyhow!("task not found: {task_id}"))?;
        if !matches!(status.as_str(), "running" | "pausing" | "paused") {
            return Err(anyhow!("task {task_id} is {status}, not cancellable"));
        }
        tx.execute(
            "UPDATE plan_tasks
             SET status = 'cancelling', last_error = 'cancel requested by user', updated_at = ?1
             WHERE session_id = ?2 AND plan_seq = ?3 AND task_id = ?4",
            params![now, session_id, plan_seq, task_id],
        )?;
        let task = tx.query_row(
            "SELECT session_id, project_id, plan_seq, task_id, department, title, priority,
                    status, deps, write_scope, attempts, enqueued_at, started_at, finished_at,
                    last_error, next_attempt_at
             FROM plan_tasks WHERE session_id = ?1 AND plan_seq = ?2 AND task_id = ?3",
            params![session_id, plan_seq, task_id],
            map_scheduled_task,
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "plan_task.cancelling",
            serde_json::json!({
                "task_id": task_id,
                "task_status": "cancelling",
                "reason": "cancel requested by user",
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((task, event))
    }

    pub fn plan_task_status(&self, session_id: &str, task_id: &str) -> Result<Option<String>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT status FROM plan_tasks
             WHERE session_id = ?1 AND task_id = ?2
             ORDER BY plan_seq DESC LIMIT 1",
            params![session_id, task_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(Into::into)
    }

    /// Skip a queued/blocked task for the current plan revision. A skipped task
    /// is terminal for scheduling and is not picked up by the global scheduler;
    /// downstream dependencies remain blocked rather than silently running.
    pub fn skip_plan_task_with_event(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<(ScheduledTask, Event)> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let (plan_seq, status, session_status) = tx
            .query_row(
                "SELECT t.plan_seq, t.status, s.status
                 FROM plan_tasks t JOIN sessions s ON s.id = t.session_id
                 WHERE t.session_id = ?1 AND t.task_id = ?2
                 ORDER BY t.plan_seq DESC LIMIT 1",
                params![session_id, task_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| anyhow!("task not found: {task_id}"))?;
        if !matches!(
            session_status.as_str(),
            "idle" | "failed" | "cancelled" | "draft"
        ) {
            return Err(anyhow!(
                "session {session_id} is {session_status}; stop it before skipping a task"
            ));
        }
        if !matches!(
            status.as_str(),
            "pending" | "failed" | "blocked" | "interrupted" | "cancelled"
        ) {
            return Err(anyhow!("task {task_id} is {status}, not skippable"));
        }
        tx.execute(
            "UPDATE plan_tasks
             SET status = 'skipped', last_error = 'skipped by user',
                 finished_at = ?1, updated_at = ?1
             WHERE session_id = ?2 AND plan_seq = ?3 AND task_id = ?4",
            params![now, session_id, plan_seq, task_id],
        )?;
        let task = tx.query_row(
            "SELECT session_id, project_id, plan_seq, task_id, department, title, priority,
                    status, deps, write_scope, attempts, enqueued_at, started_at, finished_at,
                    last_error, next_attempt_at
             FROM plan_tasks WHERE session_id = ?1 AND plan_seq = ?2 AND task_id = ?3",
            params![session_id, plan_seq, task_id],
            map_scheduled_task,
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "plan_task.skipped",
            serde_json::json!({
                "task_id": task_id,
                "task_status": "skipped",
                "reason": "skipped by user",
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((task, event))
    }

    /// Mark in-flight task rows as interrupted after a daemon restart. Returns
    /// the rows that changed so the caller can append audit events.
    pub fn interrupt_plan_tasks(&self) -> Result<Vec<ScheduledTask>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let interrupted = {
            let mut stmt = tx.prepare(
                "SELECT session_id, project_id, plan_seq, task_id, department, title, priority,
                        status, deps, write_scope, attempts, enqueued_at, started_at, finished_at,
                        last_error, next_attempt_at
                 FROM plan_tasks WHERE status IN ('running', 'claimed', 'cancelling', 'pausing', 'paused')
                 ORDER BY session_id ASC, plan_seq ASC, task_id ASC",
            )?;
            let rows = stmt.query_map([], map_scheduled_task)?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        tx.execute(
            "UPDATE plan_tasks
             SET status = 'interrupted',
                 last_error = 'daemon restarted while the task was active',
                 finished_at = ?1, updated_at = ?1
             WHERE status IN ('running', 'claimed', 'cancelling', 'pausing', 'paused')",
            params![now],
        )?;
        tx.execute("DELETE FROM write_leases", [])?;
        tx.commit()?;
        Ok(interrupted)
    }

    /// Clear task rows left running by an aborted plan run. Recovery uses the
    /// global variant; this one is called when a session reaches a terminal
    /// state during normal operation.
    pub fn interrupt_session_plan_tasks(
        &self,
        session_id: &str,
        reason: &str,
    ) -> Result<Vec<ScheduledTask>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let interrupted = {
            let mut stmt = tx.prepare(
                "SELECT session_id, project_id, plan_seq, task_id, department, title, priority,
                        status, deps, write_scope, attempts, enqueued_at, started_at, finished_at,
                        last_error, next_attempt_at
                 FROM plan_tasks
                 WHERE session_id = ?1 AND status IN ('running', 'claimed', 'cancelling', 'pausing', 'paused')
                 ORDER BY plan_seq ASC, task_id ASC",
            )?;
            let rows = stmt.query_map(params![session_id], map_scheduled_task)?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        tx.execute(
            "UPDATE plan_tasks
             SET status = 'interrupted', last_error = ?1, finished_at = ?2, updated_at = ?2
             WHERE session_id = ?3 AND status IN ('running', 'claimed', 'cancelling', 'pausing', 'paused')",
            params![reason, now, session_id],
        )?;
        tx.commit()?;
        Ok(interrupted)
    }

    pub fn plan_task_exists(&self, project_id: &str, task_id: &str) -> Result<bool> {
        let conn = self.lock()?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM plan_tasks WHERE project_id = ?1 AND task_id = ?2",
            params![project_id, task_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count > 0)
    }

    pub fn latest_merged_entry_for_task(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<Option<MergeQueueEntry>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash,
                    base_commit, pre_merge_commit, status, attempt, detail, source, remote_host, remote_branch, bundle_sha256, imported_ref, verifier_approved, verifier_summary, environment_caveat, remote_test_evidence_id, remote_test_digest, remote_test_status, created_at, updated_at
             FROM merge_queue
             WHERE session_id = ?1 AND task_id = ?2 AND status = 'merged'
             ORDER BY updated_at DESC, created_at DESC, id ASC
             LIMIT 1",
            params![session_id, task_id],
            map_merge_queue_entry,
        )
        .optional()
        .map_err(Into::into)
    }
}

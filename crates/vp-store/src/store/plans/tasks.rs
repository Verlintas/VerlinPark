use super::*;

impl Store {
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
        let additions = {
            let mut stmt = tx.prepare(
                "SELECT seq, payload FROM events
                 WHERE session_id = ?1
                   AND type IN ('team_plan.submitted', 'team_plan.revised')
                 ORDER BY seq ASC",
            )?;
            let rows = stmt.query_map(params![session_id], |row| {
                Ok((row.get::<_, i64>(0)? as u64, row.get::<_, String>(1)?))
            })?;
            let mut additions = std::collections::HashMap::new();
            for row in rows {
                let (seq, payload) = row?;
                let Ok(payload) = serde_json::from_str::<serde_json::Value>(&payload) else {
                    continue;
                };
                for task_id in payload["added"].as_array().into_iter().flatten() {
                    if let Some(task_id) = task_id.as_str() {
                        additions.insert(task_id.to_string(), seq);
                    }
                }
            }
            additions
        };
        let mut states = std::collections::HashMap::<String, TaskEventState>::new();
        {
            let mut stmt = tx.prepare(
                "SELECT seq, type, payload, created_at FROM events
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
                    row.get::<_, i64>(0)? as u64,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?;
            for row in rows {
                let (seq, kind, payload, created_at) = row?;
                let payload: serde_json::Value = serde_json::from_str(&payload).unwrap_or_default();
                let Some(task_id) = payload["task_id"].as_str() else {
                    continue;
                };
                if additions
                    .get(task_id)
                    .is_some_and(|added_seq| seq <= *added_seq)
                {
                    continue;
                }
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
}

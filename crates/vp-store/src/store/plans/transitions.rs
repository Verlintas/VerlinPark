use super::*;

impl Store {
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

    /// End an `ask_user` wait by returning the session to `running`.
    ///
    /// The transition is conditional on the session still being in
    /// `waiting_input`. If an operator paused or cancelled the session while
    /// the question was pending, that state wins and no `session.updated`
    /// event is appended; otherwise the answer would silently un-pause a
    /// session whose run control is still paused.
    pub fn resume_session_after_question_with_event(
        &self,
        session_id: &str,
    ) -> Result<Option<Event>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE sessions SET status = 'running'
             WHERE id = ?1 AND status = 'waiting_input'",
            params![session_id],
        )?;
        if changed == 0 {
            tx.commit()?;
            return Ok(None);
        }
        let event = insert_event_tx(
            &tx,
            session_id,
            "session.updated",
            serde_json::json!({"status": "running"}),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some(event))
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
        // Terminal outcomes are final for a late/duplicate finish. Retrying
        // them is an explicit operator action (`retry_plan_task_with_event`),
        // never an implicit overwrite by an older writer. A task that is
        // already `cancelling` may still record its real outcome: a concurrent
        // completion can win the race (and the caller reports `merged`), while
        // the merge boundary independently re-checks cancellation.
        let changed = tx.execute(
            "UPDATE plan_tasks
             SET status = ?1, last_error = ?2, finished_at = ?3, next_attempt_at = NULL,
                 updated_at = ?3
             WHERE session_id = ?4 AND plan_seq = ?5 AND task_id = ?6
               AND status NOT IN ('completed', 'skipped', 'cancelled')",
            params![status, detail, now, session_id, plan_seq as i64, task_id],
        )?;
        if changed == 0 {
            // Missing row, a terminal task, or a cancellation that must not be
            // overwritten: the caller keeps the current projection and must
            // not emit a `plan_task.finished` event for this write.
            tx.commit()?;
            return Ok(None);
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use vp_protocol::types::{PlanDepartment, PlanTask};

    fn test_store() -> (TempDir, Store, TempDir) {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        (data, store, repo)
    }

    fn task(id: &str, deps: Vec<String>) -> PlanTask {
        PlanTask {
            id: id.into(),
            department: "impl".into(),
            title: id.into(),
            kind: "coding".into(),
            priority: 0,
            profile: None,
            deps,
            write_scope: vec![format!("src/{id}")],
            acceptance: vec!["done".into()],
            token_budget: None,
            cost_budget_usd: None,
            parent_task_id: None,
            fanout_key: None,
            fanout_join_for: None,
        }
    }

    fn plan() -> TeamPlan {
        TeamPlan {
            goal: "guarded transitions".into(),
            departments: vec![PlanDepartment {
                name: "impl".into(),
                manager_title: "Implementation".into(),
                profile: "coding".into(),
                objective: "build".into(),
                token_budget: None,
                cost_budget_usd: None,
            }],
            tasks: vec![
                task("T1", vec![]),
                task("T2", vec![]),
                task("T3", vec![]),
                task("T4", vec![]),
            ],
        }
    }

    fn submitted(store: &Store, project: &Project, session: &Session) -> u64 {
        let event = store
            .append_event(
                &session.id,
                "team_plan.submitted",
                serde_json::json!({"goal": "guarded transitions"}),
            )
            .unwrap();
        let plan = plan();
        store
            .set_team_plan(&session.id, &serde_json::to_string(&plan).unwrap())
            .unwrap();
        store
            .sync_plan_tasks(&session.id, &project.id, event.seq, &plan)
            .unwrap();
        event.seq
    }

    fn task_status(store: &Store, session: &Session, id: &str) -> ScheduledTask {
        store
            .list_plan_tasks(None, Some(&session.id))
            .unwrap()
            .into_iter()
            .find(|task| task.task_id == id)
            .unwrap()
    }

    #[test]
    fn late_finish_cannot_overwrite_terminal_tasks() {
        let (_data, store, repo) = test_store();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("demo"))
            .unwrap();
        let session = store.create_session(&project.id, "guards").unwrap();
        let plan_seq = submitted(&store, &project, &session);

        store
            .claim_plan_tasks(&session.id, plan_seq, &["T1".into(), "T4".into()])
            .unwrap();
        store
            .finish_plan_task(&session.id, plan_seq, "T1", "completed", None)
            .unwrap()
            .unwrap();
        let late = store
            .finish_plan_task(&session.id, plan_seq, "T1", "failed", Some("late"))
            .unwrap();
        assert!(late.is_none());
        let t1 = task_status(&store, &session, "T1");
        assert_eq!(t1.status, "completed");
        assert_eq!(t1.attempts, 1);

        store.skip_plan_task_with_event(&session.id, "T2").unwrap();
        let skipped = store
            .finish_plan_task(&session.id, plan_seq, "T2", "failed", Some("late"))
            .unwrap();
        assert!(skipped.is_none());
        assert_eq!(task_status(&store, &session, "T2").status, "skipped");

        store
            .cancel_plan_task_with_event(&session.id, plan_seq, "T3", "dependency failed", "T1")
            .unwrap();
        let cancelled = store
            .finish_plan_task(&session.id, plan_seq, "T3", "completed", None)
            .unwrap();
        assert!(cancelled.is_none());
        assert_eq!(task_status(&store, &session, "T3").status, "cancelled");

        store
            .request_plan_task_cancel_with_event(&session.id, "T4")
            .unwrap();
        // A real completion can win the cancel race. The merge boundary still
        // re-checks cancellation before landing work; once the task is
        // completed, a later late writer cannot overwrite it.
        let completed = store
            .finish_plan_task(&session.id, plan_seq, "T4", "completed", None)
            .unwrap();
        assert!(completed.is_some());
        assert_eq!(task_status(&store, &session, "T4").status, "completed");
        let late = store
            .finish_plan_task(&session.id, plan_seq, "T4", "failed", Some("late"))
            .unwrap();
        assert!(late.is_none());
        assert_eq!(task_status(&store, &session, "T4").status, "completed");
    }

    #[test]
    fn ask_user_resume_does_not_overwrite_a_paused_session() {
        let (_data, store, repo) = test_store();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("demo"))
            .unwrap();
        let session = store.create_session(&project.id, "ask").unwrap();
        store
            .update_session_status_with_event(&session.id, "waiting_input")
            .unwrap();
        store
            .update_session_status_with_event(&session.id, "paused")
            .unwrap();

        let event = store
            .resume_session_after_question_with_event(&session.id)
            .unwrap();
        assert!(event.is_none());
        assert_eq!(
            store.get_session(&session.id).unwrap().unwrap().status,
            "paused"
        );

        store
            .update_session_status_with_event(&session.id, "waiting_input")
            .unwrap();
        let event = store
            .resume_session_after_question_with_event(&session.id)
            .unwrap()
            .unwrap();
        assert_eq!(event.kind, "session.updated");
        assert_eq!(event.payload["status"], "running");
        assert_eq!(
            store.get_session(&session.id).unwrap().unwrap().status,
            "running"
        );
    }
}

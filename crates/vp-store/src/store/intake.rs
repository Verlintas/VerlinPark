//! Project-scoped task-inbox items.

use super::*;

impl Store {
    /// Project-scoped standalone task intake. Intake items are not attached
    /// to a TeamPlan until the dispatcher synthesizes a one-task plan for them.
    pub fn list_intake_tasks(
        &self,
        project_id: Option<&str>,
        status: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<IntakeTask>> {
        let conn = self.lock()?;
        let limit = limit.unwrap_or(500).clamp(1, 2_000) as i64;
        let mut stmt = conn.prepare(
            "SELECT id, project_id, title, instructions, profile, kind, priority,
                    write_scope, acceptance, token_budget, cost_budget_usd,
                    time_budget_secs, status, attempts, max_attempts, next_attempt_at,
                    session_id, task_id, last_error, result_summary, origin, labels,
                    created_at, updated_at, deps
             FROM intake_tasks
             WHERE (?1 IS NULL OR project_id = ?1)
               AND (?2 IS NULL OR status = ?2)
             ORDER BY created_at DESC, id DESC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![project_id, status, limit], map_intake_task)?;
        let mut tasks = rows.collect::<Result<Vec<_>, _>>()?;
        tasks.sort_by(|left, right| {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| left.created_at.cmp(&right.created_at))
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(tasks)
    }

    pub fn get_intake_task(&self, id: &str) -> Result<Option<IntakeTask>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, project_id, title, instructions, profile, kind, priority,
                    write_scope, acceptance, token_budget, cost_budget_usd,
                    time_budget_secs, status, attempts, max_attempts, next_attempt_at,
                    session_id, task_id, last_error, result_summary, origin, labels,
                    created_at, updated_at, deps
             FROM intake_tasks WHERE id = ?1",
            params![id],
            map_intake_task,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Insert a new queued intake item. The engine validates the title,
    /// profile, scopes, and budgets before calling this store method.
    pub fn create_intake_task(&self, task: &NewIntakeTask) -> Result<IntakeTask> {
        let id = Uuid::now_v7().to_string();
        let now = now_rfc3339();
        let write_scope = serde_json::to_string(&task.write_scope)?;
        let acceptance = serde_json::to_string(&task.acceptance)?;
        let labels = serde_json::to_string(&task.labels)?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO intake_tasks
             (id, project_id, title, instructions, profile, kind, priority, write_scope,
              acceptance, deps, token_budget, cost_budget_usd, time_budget_secs, status, attempts,
              max_attempts, next_attempt_at, session_id, task_id, last_error, result_summary,
              origin, labels, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                     'queued', 0, ?14, NULL, NULL, NULL, NULL, NULL, ?15, ?16, ?17, ?17)",
            params![
                id,
                task.project_id,
                task.title,
                task.instructions,
                task.profile,
                task.kind,
                task.priority,
                write_scope,
                acceptance,
                serde_json::to_string(&task.deps)?,
                task.token_budget.map(|value| value as i64),
                task.cost_budget_usd,
                task.time_budget_secs.map(|value| value as i64),
                task.max_attempts as i64,
                task.origin,
                labels,
                now,
            ],
        )?;
        drop(conn);
        self.get_intake_task(&id)?
            .ok_or_else(|| anyhow!("created intake task disappeared"))
    }

    /// Replace the editable fields of a non-active intake item. Editing a
    /// failed/cancelled item re-queues it with a fresh manual attempt budget,
    /// matching the "edit then run" workflow in the task inbox.
    pub fn update_intake_task(&self, id: &str, task: &NewIntakeTask) -> Result<Option<IntakeTask>> {
        let conn = self.lock()?;
        let now = now_rfc3339();
        let changed = conn.execute(
            "UPDATE intake_tasks
             SET title = ?2, instructions = ?3, profile = ?4, kind = ?5, priority = ?6,
                 write_scope = ?7, acceptance = ?8, deps = ?9, token_budget = ?10,
                 cost_budget_usd = ?11, time_budget_secs = ?12, max_attempts = ?13,
                 labels = ?14, status = 'queued', attempts = 0, next_attempt_at = NULL,
                 last_error = NULL, result_summary = NULL, updated_at = ?15
             WHERE id = ?1 AND status NOT IN ('dispatching', 'running', 'completed')",
            params![
                id,
                task.title,
                task.instructions,
                task.profile,
                task.kind,
                task.priority,
                serde_json::to_string(&task.write_scope)?,
                serde_json::to_string(&task.acceptance)?,
                serde_json::to_string(&task.deps)?,
                task.token_budget.map(|value| value as i64),
                task.cost_budget_usd,
                task.time_budget_secs.map(|value| value as i64),
                task.max_attempts as i64,
                serde_json::to_string(&task.labels)?,
                now,
            ],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        drop(conn);
        self.get_intake_task(id)
    }

    /// Atomically take a runnable intake item. This is the only path that
    /// increments `attempts`, so two scheduler ticks or a manual dispatch
    /// cannot claim the same item.
    pub fn claim_intake_task(&self, id: &str) -> Result<Option<IntakeTask>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let current = tx
            .query_row(
                "SELECT status, attempts, max_attempts FROM intake_tasks WHERE id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)? as u32,
                        row.get::<_, i64>(2)? as u32,
                    ))
                },
            )
            .optional()?;
        let Some((status, attempts, max_attempts)) = current else {
            return Ok(None);
        };
        if !matches!(status.as_str(), "queued" | "failed" | "interrupted")
            || attempts >= max_attempts
        {
            return Ok(None);
        }
        let changed = tx.execute(
            "UPDATE intake_tasks
             SET status = 'dispatching', attempts = attempts + 1,
                 next_attempt_at = NULL, last_error = NULL, updated_at = ?2
             WHERE id = ?1 AND status = ?3 AND attempts = ?4",
            params![id, now, status, attempts as i64],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        tx.commit()?;
        drop(conn);
        self.get_intake_task(id)
    }

    pub fn start_intake_task(
        &self,
        id: &str,
        session_id: &str,
        task_id: &str,
    ) -> Result<IntakeTask> {
        let conn = self.lock()?;
        let now = now_rfc3339();
        let changed = conn.execute(
            "UPDATE intake_tasks
             SET status = 'running', session_id = ?2, task_id = ?3,
                 result_summary = NULL, updated_at = ?4
             WHERE id = ?1 AND status = 'dispatching'",
            params![id, session_id, task_id, now],
        )?;
        if changed == 0 {
            anyhow::bail!("intake task {id} is not dispatching");
        }
        drop(conn);
        self.get_intake_task(id)?
            .ok_or_else(|| anyhow!("intake task {id} disappeared"))
    }

    /// Settle a dispatch or run. `next_attempt_at` is supplied by the caller
    /// when the project orchestration policy wants an automatic retry.
    pub fn settle_intake_task(
        &self,
        id: &str,
        status: &str,
        result_summary: Option<&str>,
        last_error: Option<&str>,
        next_attempt_at: Option<&str>,
    ) -> Result<Option<IntakeTask>> {
        if !matches!(
            status,
            "queued" | "failed" | "cancelled" | "completed" | "interrupted" | "blocked"
        ) {
            anyhow::bail!("invalid intake status: {status}");
        }
        let conn = self.lock()?;
        conn.execute(
            "UPDATE intake_tasks
             SET status = ?2, result_summary = COALESCE(?3, result_summary),
                 last_error = ?4, next_attempt_at = ?5, updated_at = ?6
             WHERE id = ?1",
            params![
                id,
                status,
                result_summary,
                last_error,
                next_attempt_at,
                now_rfc3339()
            ],
        )?;
        drop(conn);
        self.get_intake_task(id)
    }

    /// Manual retry resets the attempt budget; automatic retries never do.
    pub fn retry_intake_task(&self, id: &str) -> Result<Option<IntakeTask>> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE intake_tasks
             SET status = 'queued', attempts = 0, next_attempt_at = NULL,
                 last_error = NULL, result_summary = NULL, updated_at = ?2
             WHERE id = ?1 AND status IN ('failed', 'interrupted', 'cancelled', 'blocked')",
            params![id, now_rfc3339()],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        drop(conn);
        self.get_intake_task(id)
    }

    pub fn cancel_intake_task(&self, id: &str) -> Result<Option<IntakeTask>> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE intake_tasks
             SET status = 'cancelled', next_attempt_at = NULL, updated_at = ?2
             WHERE id = ?1 AND status NOT IN ('running', 'dispatching', 'completed', 'cancelled')",
            params![id, now_rfc3339()],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        drop(conn);
        self.get_intake_task(id)
    }

    pub fn delete_intake_task(&self, id: &str) -> Result<bool> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "DELETE FROM intake_tasks
             WHERE id = ?1 AND status NOT IN ('running', 'dispatching')",
            params![id],
        )?;
        Ok(changed > 0)
    }

    pub fn count_active_intake_tasks(&self, project_id: &str) -> Result<u32> {
        let conn = self.lock()?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM intake_tasks
             WHERE project_id = ?1 AND status IN ('dispatching', 'running')",
            params![project_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count.max(0) as u32)
    }

    /// Runnable intake rows, oldest first. The scheduler applies priority,
    /// aging, project policy, and round-robin ordering in memory.
    pub fn runnable_intake_tasks(&self, now: &str) -> Result<Vec<IntakeTask>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, project_id, title, instructions, profile, kind, priority,
                    write_scope, acceptance, token_budget, cost_budget_usd,
                    time_budget_secs, status, attempts, max_attempts, next_attempt_at,
                    session_id, task_id, last_error, result_summary, origin, labels,
                    created_at, updated_at, deps
             FROM intake_tasks
             WHERE status IN ('queued', 'failed', 'interrupted')
               AND attempts < max_attempts
               AND (next_attempt_at IS NULL OR next_attempt_at <= ?1)
             ORDER BY created_at ASC, id ASC
             LIMIT 1000",
        )?;
        let rows = stmt.query_map(params![now], map_intake_task)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn insert_intake_task_snapshot(&self, task: &IntakeTask) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO intake_tasks
             (id, project_id, title, instructions, profile, kind, priority, write_scope,
              acceptance, deps, token_budget, cost_budget_usd, time_budget_secs, status,
              attempts, max_attempts, next_attempt_at, session_id, task_id, last_error,
              result_summary, origin, labels, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                     ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)
             ON CONFLICT(id) DO UPDATE SET
                title = excluded.title,
                instructions = excluded.instructions,
                profile = excluded.profile,
                kind = excluded.kind,
                priority = excluded.priority,
                write_scope = excluded.write_scope,
                acceptance = excluded.acceptance,
                deps = excluded.deps,
                token_budget = excluded.token_budget,
                cost_budget_usd = excluded.cost_budget_usd,
                time_budget_secs = excluded.time_budget_secs,
                status = excluded.status,
                attempts = excluded.attempts,
                max_attempts = excluded.max_attempts,
                next_attempt_at = excluded.next_attempt_at,
                session_id = excluded.session_id,
                task_id = excluded.task_id,
                last_error = excluded.last_error,
                result_summary = excluded.result_summary,
                origin = excluded.origin,
                labels = excluded.labels,
                updated_at = excluded.updated_at",
            params![
                task.id,
                task.project_id,
                task.title,
                task.instructions,
                task.profile,
                task.kind,
                task.priority,
                serde_json::to_string(&task.write_scope)?,
                serde_json::to_string(&task.acceptance)?,
                serde_json::to_string(&task.deps)?,
                task.token_budget.map(|value| value as i64),
                task.cost_budget_usd,
                task.time_budget_secs.map(|value| value as i64),
                task.status,
                task.attempts as i64,
                task.max_attempts as i64,
                task.next_attempt_at,
                task.session_id,
                task.task_id,
                task.last_error,
                task.result_summary,
                task.origin,
                serde_json::to_string(&task.labels)?,
                task.created_at,
                task.updated_at,
            ],
        )?;
        Ok(())
    }
}

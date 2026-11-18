use super::*;

impl Store {
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

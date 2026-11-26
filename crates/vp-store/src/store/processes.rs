//! Process policy and the durable supervised-process ledger.

use super::*;

impl Store {
    pub fn get_process_policy(&self, project_id: &str) -> Result<ProcessPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT idle_sweep_enabled, max_processes, idle_ttl_secs,
                        max_output_bytes, max_output_lines, restart_on_boot,
                        restart_max_attempts, restart_backoff_secs
                 FROM project_process_policies WHERE project_id = ?1",
                params![project_id],
                |row| {
                    Ok(ProcessPolicy {
                        idle_sweep_enabled: row.get::<_, i64>(0)? != 0,
                        max_processes: row.get::<_, i64>(1)?.max(1) as u32,
                        idle_ttl_secs: row.get::<_, i64>(2)?.max(1) as u64,
                        max_output_bytes: row.get::<_, i64>(3)?.max(1) as u64,
                        max_output_lines: row.get::<_, i64>(4)?.max(1) as u64,
                        restart_on_boot: row.get::<_, i64>(5)? != 0,
                        restart_max_attempts: row.get::<_, i64>(6)?.clamp(0, 10) as u32,
                        restart_backoff_secs: row.get::<_, i64>(7)?.clamp(0, 300) as u64,
                    })
                },
            )
            .optional()?;
        stored.map_or_else(
            || Ok(ProcessPolicy::default()),
            |policy| {
                policy
                    .validate()
                    .map_err(|error| anyhow!("stored process policy is invalid: {error}"))?;
                Ok(policy)
            },
        )
    }

    pub fn set_process_policy(&self, project_id: &str, policy: &ProcessPolicy) -> Result<()> {
        policy
            .validate()
            .map_err(|error| anyhow!("invalid process policy: {error}"))?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_process_policies
             (project_id, idle_sweep_enabled, max_processes, idle_ttl_secs,
              max_output_bytes, max_output_lines, restart_on_boot,
              restart_max_attempts, restart_backoff_secs, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(project_id) DO UPDATE SET
                idle_sweep_enabled = excluded.idle_sweep_enabled,
                max_processes = excluded.max_processes,
                idle_ttl_secs = excluded.idle_ttl_secs,
                max_output_bytes = excluded.max_output_bytes,
                max_output_lines = excluded.max_output_lines,
                restart_on_boot = excluded.restart_on_boot,
                restart_max_attempts = excluded.restart_max_attempts,
                restart_backoff_secs = excluded.restart_backoff_secs,
                updated_at = excluded.updated_at",
            params![
                project_id,
                if policy.idle_sweep_enabled { 1 } else { 0 },
                i64::from(policy.max_processes),
                policy.idle_ttl_secs as i64,
                policy.max_output_bytes as i64,
                policy.max_output_lines as i64,
                if policy.restart_on_boot { 1 } else { 0 },
                i64::from(policy.restart_max_attempts),
                policy.restart_backoff_secs as i64,
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    /// Insert a `process.starting` row and its event in one transaction.
    /// The event is written before the projection so a crash cannot leave a
    /// process row with no lifecycle audit trail.
    /// Read back the optional owner mapping for tests/diagnostics.
    #[allow(clippy::type_complexity)]
    pub fn supervised_process_owner(
        &self,
        process_id: &str,
    ) -> Result<Option<(String, Option<String>, Option<String>)>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT session_id, agent_id, task_id FROM supervised_process_owners \
             WHERE process_id = ?1",
        )?;
        let mut rows = statement.query([process_id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some((row.get(0)?, row.get(1)?, row.get(2)?)))
    }

    /// Record which agent/task started a durable process. This is a separate
    /// table so the supervised-process row and its event schema stay stable.
    pub fn set_supervised_process_owner(
        &self,
        process_id: &str,
        session_id: &str,
        agent_id: Option<&str>,
        task_id: Option<&str>,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO supervised_process_owners \
             (process_id, session_id, agent_id, task_id, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(process_id) DO UPDATE SET \
             session_id = excluded.session_id, agent_id = excluded.agent_id, \
             task_id = excluded.task_id, updated_at = excluded.updated_at",
            rusqlite::params![
                process_id,
                session_id,
                agent_id,
                task_id,
                vp_protocol::now_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn insert_supervised_process_with_event(
        &self,
        session_id: &str,
        process: &SupervisedProcess,
        event_kind: &str,
    ) -> Result<Event> {
        if process.session_id != session_id {
            return Err(anyhow!(
                "supervised process belongs to a different session: {}",
                process.session_id
            ));
        }
        if !matches!(event_kind, "process.starting" | "process.started") {
            return Err(anyhow!(
                "invalid supervised-process insert event: {event_kind}"
            ));
        }
        validate_supervised_process_row(process)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let event = insert_event_tx(&tx, session_id, event_kind, serde_json::to_value(process)?)?;
        insert_supervised_process_tx(&tx, process)?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(event)
    }

    /// Update every durable field of a supervised process and append its
    /// lifecycle event in one transaction.
    pub fn update_supervised_process_with_event(
        &self,
        process: &SupervisedProcess,
        event_kind: &str,
    ) -> Result<Event> {
        validate_supervised_process_row(process)?;
        if !matches!(
            event_kind,
            "process.started"
                | "process.adopted"
                | "process.restarting"
                | "process.restarted"
                | "process.restart_blocked"
                | "process.exited"
                | "process.lost"
                | "process.unknown"
                | "process.stopped"
        ) {
            return Err(anyhow!(
                "invalid supervised-process transition event: {event_kind}"
            ));
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let event = insert_event_tx(
            &tx,
            &process.session_id,
            event_kind,
            serde_json::to_value(process)?,
        )?;
        update_supervised_process_tx(&tx, process)?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(event)
    }

    /// Persist heartbeat/last-access/output telemetry without an event. These
    /// fields are observation state; lifecycle transitions remain event-backed.
    pub fn record_supervised_process_telemetry(&self, process: &SupervisedProcess) -> Result<()> {
        if process.session_id.is_empty() || process.id.is_empty() {
            return Err(anyhow!("supervised process id and session are required"));
        }
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE supervised_processes
             SET heartbeat_at = ?2, last_access_at = ?3, output_bytes = ?4,
                 output_lines = ?5, total_output_bytes = ?6, output_rotations = ?7
             WHERE session_id = ?8 AND id = ?1",
            params![
                process.id,
                process.heartbeat_at,
                process.last_access_at,
                process.output_bytes as i64,
                process.output_lines as i64,
                process.total_output_bytes as i64,
                process.output_rotations as i64,
                process.session_id,
            ],
        )?;
        if changed == 0 {
            return Err(anyhow!("supervised process not found: {}", process.id));
        }
        Ok(())
    }

    /// Mark a row terminal and delete it as part of retention. The
    /// `process.pruned` event carries only the id/reason; output bodies are
    /// operational state and are never part of replay content.
    pub fn prune_supervised_process_with_event(
        &self,
        session_id: &str,
        process_id: &str,
        reason: &str,
    ) -> Result<Option<Event>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let state: Option<String> = tx
            .query_row(
                "SELECT state FROM supervised_processes WHERE session_id = ?1 AND id = ?2",
                params![session_id, process_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(state) = state else {
            return Ok(None);
        };
        if matches!(
            state.as_str(),
            "starting" | "running" | "restarting" | "unknown"
        ) {
            return Err(anyhow!(
                "refusing to prune a live supervised process: {process_id} ({state})"
            ));
        }
        let event = insert_event_tx(
            &tx,
            session_id,
            "process.pruned",
            serde_json::json!({
                "process_id": process_id,
                "reason": reason,
                "state": state,
            }),
        )?;
        tx.execute(
            "DELETE FROM supervised_processes WHERE session_id = ?1 AND id = ?2",
            params![session_id, process_id],
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some(event))
    }

    pub fn get_supervised_process(
        &self,
        session_id: &str,
        process_id: &str,
    ) -> Result<Option<SupervisedProcess>> {
        let conn = self.lock()?;
        Ok(conn
            .query_row(
                "SELECT id, session_id, name, command, argv, cwd, worktree,
                        env_hash, command_hash, state, pid, pgid, nonce,
                        started_at, process_started_at, heartbeat_at, finished_at,
                        exit_code, signal, output_path, output_bytes, output_lines,
                        total_output_bytes, output_rotations, max_output_bytes,
                        max_output_lines, owner_instance,
                        retention_deadline, idle_ttl_secs, last_access_at,
                    restart_attempts, restart_blocked_reason, detail
                 FROM supervised_processes
                 WHERE session_id = ?1 AND id = ?2",
                params![session_id, process_id],
                map_supervised_process,
            )
            .optional()?)
    }

    pub fn list_supervised_processes(
        &self,
        session_id: &str,
        include_finished: bool,
        limit: u32,
    ) -> Result<Vec<SupervisedProcess>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT id, session_id, name, command, argv, cwd, worktree,
                    env_hash, command_hash, state, pid, pgid, nonce,
                    started_at, process_started_at, heartbeat_at, finished_at,
                    exit_code, signal, output_path, output_bytes, output_lines,
                    total_output_bytes, output_rotations, max_output_bytes,
                    max_output_lines, owner_instance,
                    retention_deadline, idle_ttl_secs, last_access_at,
                    restart_attempts, restart_blocked_reason, detail
             FROM supervised_processes
             WHERE session_id = ?1
               AND (?2 = 1 OR state IN ('starting', 'running', 'restarting', 'unknown'))
             ORDER BY started_at DESC, id DESC
             LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![
                session_id,
                if include_finished { 1 } else { 0 },
                i64::from(limit.clamp(1, 500))
            ],
            map_supervised_process,
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Unfinished rows examined by startup reconciliation.
    pub fn list_unfinished_supervised_processes(&self) -> Result<Vec<SupervisedProcess>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT id, session_id, name, command, argv, cwd, worktree,
                    env_hash, command_hash, state, pid, pgid, nonce,
                    started_at, process_started_at, heartbeat_at, finished_at,
                    exit_code, signal, output_path, output_bytes, output_lines,
                    total_output_bytes, output_rotations, max_output_bytes,
                    max_output_lines, owner_instance,
                    retention_deadline, idle_ttl_secs, last_access_at,
                    restart_attempts, restart_blocked_reason, detail
             FROM supervised_processes
             WHERE state IN ('starting', 'running', 'restarting', 'unknown')
             ORDER BY started_at, id",
        )?;
        let rows = statement.query_map([], map_supervised_process)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Running/starting/unknown rows for the idle sweep. A row in `restarting`
    /// has no live process and is intentionally excluded.
    pub fn list_live_supervised_processes(&self) -> Result<Vec<SupervisedProcess>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT id, session_id, name, command, argv, cwd, worktree,
                    env_hash, command_hash, state, pid, pgid, nonce,
                    started_at, process_started_at, heartbeat_at, finished_at,
                    exit_code, signal, output_path, output_bytes, output_lines,
                    total_output_bytes, output_rotations, max_output_bytes,
                    max_output_lines, owner_instance,
                    retention_deadline, idle_ttl_secs, last_access_at,
                    restart_attempts, restart_blocked_reason, detail
             FROM supervised_processes
             WHERE state IN ('starting', 'running', 'unknown')
             ORDER BY started_at, id",
        )?;
        let rows = statement.query_map([], map_supervised_process)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Terminal rows whose retention deadline has passed.
    pub fn list_expired_supervised_processes(&self, now: &str) -> Result<Vec<SupervisedProcess>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT id, session_id, name, command, argv, cwd, worktree,
                    env_hash, command_hash, state, pid, pgid, nonce,
                    started_at, process_started_at, heartbeat_at, finished_at,
                    exit_code, signal, output_path, output_bytes, output_lines,
                    total_output_bytes, output_rotations, max_output_bytes,
                    max_output_lines, owner_instance,
                    retention_deadline, idle_ttl_secs, last_access_at,
                    restart_attempts, restart_blocked_reason, detail
             FROM supervised_processes
             WHERE state IN ('exited', 'stopped', 'lost', 'blocked')
               AND retention_deadline IS NOT NULL
               AND retention_deadline <= ?1
             ORDER BY retention_deadline, id",
        )?;
        let rows = statement.query_map(params![now], map_supervised_process)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn count_active_supervised_processes(&self, session_id: &str) -> Result<u32> {
        let conn = self.lock()?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM supervised_processes
             WHERE session_id = ?1 AND state IN ('starting', 'running', 'restarting', 'unknown')",
            params![session_id],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as u32)
    }

    pub fn supervised_process_counts(&self) -> Result<Vec<MetricCount>> {
        let conn = self.lock()?;
        bounded_metric_counts(&conn, "supervised_processes", "state")
    }

    pub fn supervised_process_output_bytes(&self) -> Result<u64> {
        let conn = self.lock()?;
        let total: i64 = conn.query_row(
            "SELECT COALESCE(SUM(output_bytes), 0) FROM supervised_processes",
            [],
            |row| row.get(0),
        )?;
        Ok(total.max(0) as u64)
    }

    /// Replace the event-backed supervised-process projection during
    /// replay/repair/bundle import. Operational heartbeat/output telemetry is
    /// rebuilt from the event snapshot; it is observation state, not durable
    /// lifecycle state.
    pub fn replace_supervised_processes(
        &self,
        session_id: &str,
        processes: &[SupervisedProcess],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        replace_supervised_processes_tx(&tx, session_id, processes)?;
        tx.commit()?;
        Ok(())
    }
}

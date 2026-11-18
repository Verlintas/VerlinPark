use super::*;

impl Store {
    /// Atomically repair all in-flight state after a daemon restart and return
    /// the session ids plus the events that must be broadcast.
    pub fn recover_state(&self) -> Result<(Vec<String>, Vec<Event>)> {
        let now = now_rfc3339();
        let process_instance = self.process_owner_instance();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let sessions = {
            let mut stmt = tx.prepare(
                "SELECT id FROM sessions
                 WHERE (status IN ('running', 'paused', 'waiting_input')
                    OR (status = 'queued' AND (
                        EXISTS (
                            SELECT 1 FROM session_runs r
                            WHERE r.session_id = sessions.id AND r.status = 'running'
                        )
                        OR EXISTS (
                            SELECT 1 FROM events e
                            WHERE e.session_id = sessions.id
                              AND e.type = 'agent.queued'
                              AND e.payload LIKE '%\"resumed_run_id\"%'
                        )
                    )))
                   AND NOT EXISTS (
                       SELECT 1 FROM runtime_leases l
                       WHERE l.lease_key = 'session:' || sessions.id
                         AND l.released_at IS NULL
                         AND l.owner_instance <> ''
                         AND l.owner_instance <> ?1
                         AND julianday(l.expires_at) > julianday(?2)
                   )",
            )?;
            let rows = stmt.query_map(params![process_instance, now], |row| {
                row.get::<_, String>(0)
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        tx.execute(
            "UPDATE sessions SET status = 'failed'
             WHERE (status IN ('running', 'paused', 'waiting_input')
                OR (status = 'queued' AND (
                    EXISTS (
                        SELECT 1 FROM session_runs r
                        WHERE r.session_id = sessions.id AND r.status = 'running'
                    )
                    OR EXISTS (
                        SELECT 1 FROM events e
                        WHERE e.session_id = sessions.id
                          AND e.type = 'agent.queued'
                          AND e.payload LIKE '%\"resumed_run_id\"%'
                    )
                )))
               AND NOT EXISTS (
                   SELECT 1 FROM runtime_leases l
                   WHERE l.lease_key = 'session:' || sessions.id
                     AND l.released_at IS NULL
                     AND l.owner_instance <> ''
                     AND l.owner_instance <> ?1
                     AND julianday(l.expires_at) > julianday(?2)
               )",
            params![process_instance, now],
        )?;
        let agents = {
            let mut stmt = tx.prepare(
                "SELECT id, session_id FROM agents
                 WHERE status IN ('running', 'waiting_input', 'queued')
                   AND NOT EXISTS (
                       SELECT 1 FROM runtime_leases l
                       WHERE l.lease_key = 'session:' || agents.session_id
                         AND l.released_at IS NULL
                         AND l.owner_instance <> ''
                         AND l.owner_instance <> ?1
                         AND julianday(l.expires_at) > julianday(?2)
                   )",
            )?;
            let rows = stmt.query_map(params![process_instance, now], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        tx.execute(
            "UPDATE agents SET status = 'failed'
             WHERE status IN ('running', 'waiting_input', 'queued')
               AND NOT EXISTS (
                   SELECT 1 FROM runtime_leases l
                   WHERE l.lease_key = 'session:' || agents.session_id
                     AND l.released_at IS NULL
                     AND l.owner_instance <> ''
                     AND l.owner_instance <> ?1
                     AND julianday(l.expires_at) > julianday(?2)
               )",
            params![process_instance, now],
        )?;
        let approvals = {
            let mut stmt = tx.prepare(
                "SELECT id, session_id FROM approvals
                 WHERE status = 'pending'
                   AND NOT EXISTS (
                       SELECT 1 FROM runtime_leases l
                       WHERE l.lease_key = 'session:' || approvals.session_id
                         AND l.released_at IS NULL
                         AND l.owner_instance <> ''
                         AND l.owner_instance <> ?1
                         AND julianday(l.expires_at) > julianday(?2)
                   )",
            )?;
            let rows = stmt.query_map(params![process_instance, now], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        tx.execute(
            "UPDATE approvals SET status = 'denied', resolved_at = ?1
             WHERE status = 'pending'
               AND NOT EXISTS (
                   SELECT 1 FROM runtime_leases l
                   WHERE l.lease_key = 'session:' || approvals.session_id
                     AND l.released_at IS NULL
                     AND l.owner_instance <> ''
                     AND l.owner_instance <> ?2
                     AND julianday(l.expires_at) > julianday(?1)
               )",
            params![now, process_instance],
        )?;
        let expired_questions = {
            let mut stmt = tx.prepare(
                "SELECT id, session_id FROM questions
                 WHERE status = 'pending'
                   AND NOT EXISTS (
                       SELECT 1 FROM runtime_leases l
                       WHERE l.lease_key = 'session:' || questions.session_id
                         AND l.released_at IS NULL
                         AND l.owner_instance <> ''
                         AND l.owner_instance <> ?1
                         AND julianday(l.expires_at) > julianday(?2)
                   )",
            )?;
            let rows = stmt.query_map(params![process_instance, now], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        tx.execute(
            "UPDATE questions SET status = 'expired', answered_at = ?1
             WHERE status = 'pending'
               AND NOT EXISTS (
                   SELECT 1 FROM runtime_leases l
                   WHERE l.lease_key = 'session:' || questions.session_id
                     AND l.released_at IS NULL
                     AND l.owner_instance <> ''
                     AND l.owner_instance <> ?2
                     AND julianday(l.expires_at) > julianday(?1)
               )",
            params![now, process_instance],
        )?;
        let queue_ids = {
            let mut stmt = tx.prepare(
                "SELECT id FROM merge_queue
                 WHERE status IN ('queued', 'merging', 'checking')
                   AND NOT EXISTS (
                       SELECT 1 FROM runtime_leases l
                       WHERE l.lease_key = 'integration:' || merge_queue.session_id
                         AND l.released_at IS NULL
                         AND l.owner_instance <> ''
                         AND l.owner_instance <> ?1
                         AND julianday(l.expires_at) > julianday(?2)
                   )",
            )?;
            let rows = stmt.query_map(params![process_instance, now], |row| {
                row.get::<_, String>(0)
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        tx.execute(
            "UPDATE merge_queue
             SET status = 'interrupted',
                 detail = 'daemon restarted during integration',
                 updated_at = ?1
             WHERE status IN ('queued', 'merging', 'checking')
               AND NOT EXISTS (
                   SELECT 1 FROM runtime_leases l
                   WHERE l.lease_key = 'integration:' || merge_queue.session_id
                     AND l.released_at IS NULL
                     AND l.owner_instance <> ''
                     AND l.owner_instance <> ?2
                     AND julianday(l.expires_at) > julianday(?1)
               )",
            params![now, process_instance],
        )?;
        let interrupted_tasks = {
            let mut stmt = tx.prepare(
                "SELECT session_id, task_id FROM plan_tasks
                 WHERE status IN ('running', 'claimed', 'cancelling', 'pausing', 'paused')
                   AND NOT EXISTS (
                       SELECT 1 FROM runtime_leases l
                       WHERE l.lease_key = 'session:' || plan_tasks.session_id
                         AND l.released_at IS NULL
                         AND l.owner_instance <> ''
                         AND l.owner_instance <> ?1
                         AND julianday(l.expires_at) > julianday(?2)
                   )
                 ORDER BY session_id ASC, plan_seq ASC, task_id ASC",
            )?;
            let rows = stmt.query_map(params![process_instance, now], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let interrupted_steering = {
            let mut stmt = tx.prepare(
                "SELECT id, session_id, agent_id, task_id, body, status, created_at, delivered_at
                 FROM steering_messages
                 WHERE status = 'pending'
                   AND NOT EXISTS (
                       SELECT 1 FROM runtime_leases l
                       WHERE l.lease_key = 'session:' || steering_messages.session_id
                         AND l.released_at IS NULL
                         AND l.owner_instance <> ''
                         AND l.owner_instance <> ?1
                         AND julianday(l.expires_at) > julianday(?2)
                   )
                 ORDER BY created_at ASC, id ASC",
            )?;
            let rows = stmt.query_map(params![process_instance, now], map_steering)?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        tx.execute(
            "UPDATE steering_messages SET status = 'cancelled'
             WHERE status = 'pending'
               AND NOT EXISTS (
                   SELECT 1 FROM runtime_leases l
                   WHERE l.lease_key = 'session:' || steering_messages.session_id
                     AND l.released_at IS NULL
                     AND l.owner_instance <> ''
                     AND l.owner_instance <> ?1
                     AND julianday(l.expires_at) > julianday(?2)
               )",
            params![process_instance, now],
        )?;
        let legacy_continuations = {
            let mut stmt = tx.prepare(&format!(
                "SELECT {SESSION_CONTINUATION_COLUMNS}
                 FROM session_continuations
                 WHERE status = 'delivering' AND lease_expires_at IS NULL
                 ORDER BY created_at ASC, id ASC"
            ))?;
            let rows = stmt.query_map([], map_session_continuation)?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        tx.execute(
            "UPDATE session_continuations
             SET status = 'failed',
                 last_error = 'daemon restarted while delivering; not retried automatically',
                 next_attempt_at = NULL, updated_at = ?1,
                 lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL
             WHERE status = 'delivering' AND lease_expires_at IS NULL",
            params![now],
        )?;
        let expired_continuations = {
            let mut stmt = tx.prepare(&format!(
                "SELECT {SESSION_CONTINUATION_COLUMNS}
                 FROM session_continuations
                 WHERE status = 'delivering'
                   AND lease_expires_at IS NOT NULL
                   AND julianday(lease_expires_at) <= julianday(?1)
                 ORDER BY created_at ASC, id ASC"
            ))?;
            let rows = stmt.query_map(params![now], map_session_continuation)?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        tx.execute(
            "UPDATE session_continuations
             SET status = 'ready',
                 last_error = 'delivery claim expired before acknowledgement; reclaimed',
                 next_attempt_at = NULL, updated_at = ?1,
                 lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL
             WHERE status = 'delivering'
               AND lease_expires_at IS NOT NULL
               AND julianday(lease_expires_at) <= julianday(?1)",
            params![now],
        )?;
        tx.execute(
            "UPDATE plan_tasks
             SET status = 'interrupted',
                 last_error = 'daemon restarted while the task was active',
                 finished_at = ?1, updated_at = ?1
             WHERE status IN ('running', 'claimed', 'cancelling', 'pausing', 'paused')
               AND NOT EXISTS (
                   SELECT 1 FROM runtime_leases l
                   WHERE l.lease_key = 'session:' || plan_tasks.session_id
                     AND l.released_at IS NULL
                     AND l.owner_instance <> ''
                     AND l.owner_instance <> ?2
                     AND julianday(l.expires_at) > julianday(?1)
               )",
            params![now, process_instance],
        )?;
        tx.execute(
            "UPDATE intake_tasks
             SET status = 'failed',
                 last_error = 'daemon restarted while the intake task was active',
                 next_attempt_at = NULL, updated_at = ?1
             WHERE status IN ('dispatching', 'running')
               AND (session_id IS NULL OR NOT EXISTS (
                   SELECT 1 FROM runtime_leases l
                   WHERE l.lease_key = 'session:' || intake_tasks.session_id
                     AND l.released_at IS NULL
                     AND l.owner_instance <> ''
                     AND l.owner_instance <> ?2
                     AND julianday(l.expires_at) > julianday(?1)
               ))",
            params![now, process_instance],
        )?;
        // Clear cross-session write scopes only for sessions this recovery is
        // interrupting. A session held by another live instance keeps its
        // write leases; deleting them would allow overlapping writes.
        tx.execute(
            "DELETE FROM write_leases
             WHERE NOT EXISTS (
                 SELECT 1 FROM runtime_leases l
                 WHERE l.lease_key = 'session:' || write_leases.session_id
                   AND l.released_at IS NULL
                   AND l.owner_instance <> ''
                   AND l.owner_instance <> ?1
                   AND julianday(l.expires_at) > julianday(?2)
             )",
            params![process_instance, now],
        )?;
        // Only this process's leases (and pre-fencing legacy rows) are
        // cleared. Another daemon sharing the data directory keeps its live
        // ownership; its expired rows are swept later by the normal sweep.
        // Record a token-free audit row for each so a restart does not erase
        // the ownership trail.
        let own_leases = {
            let mut stmt = tx.prepare(&format!(
                "SELECT {RUNTIME_LEASE_COLUMNS}
                 FROM runtime_leases
                 WHERE owner_instance = ?1 OR owner_instance = ''
                 ORDER BY lease_key ASC"
            ))?;
            let rows = stmt.query_map(params![process_instance], map_runtime_lease)?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        for lease in &own_leases {
            crate::store::lease::insert_audit_tx(
                &tx,
                &lease.lease_key,
                &lease.kind,
                "startup_cleanup",
                &lease.owner_id,
                &lease.owner_instance,
                lease.fencing_token,
                Some("daemon restarted while the lease row was held"),
                &now,
            )?;
        }
        tx.execute(
            "DELETE FROM runtime_leases
             WHERE owner_instance = ?1 OR owner_instance = ''",
            params![process_instance],
        )?;
        let interrupted_computer_leases = {
            let mut stmt = tx.prepare(
                "SELECT c.lease_id, c.surface, c.owner_id, c.session_id, c.agent_id,
                        c.agent_name, c.acquired_at, c.heartbeat_at, c.expires_at, c.generation
                 FROM computer_leases c
                 WHERE julianday(c.expires_at) <= julianday(?1)
                    OR NOT EXISTS (
                        SELECT 1 FROM runtime_leases l
                        WHERE l.lease_key = 'session:' || c.session_id
                          AND l.released_at IS NULL
                          AND l.owner_instance <> ''
                          AND l.owner_instance <> ?2
                          AND julianday(l.expires_at) > julianday(?1)
                    )
                 ORDER BY c.acquired_at ASC, c.lease_id ASC",
            )?;
            let rows = stmt.query_map(params![now, process_instance], map_computer_lease)?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        for lease in &interrupted_computer_leases {
            tx.execute(
                "DELETE FROM computer_leases WHERE lease_id = ?1",
                params![&lease.lease_id],
            )?;
        }

        let mut events = Vec::new();
        for lease in &interrupted_computer_leases {
            events.push(insert_event_tx(
                &tx,
                &lease.session_id,
                "computer.lease.expired",
                serde_json::json!({
                    "lease": lease,
                    "reason": "daemon restarted while the computer surface was leased",
                }),
            )?);
        }
        for session_id in &sessions {
            events.push(insert_event_tx(
                &tx,
                session_id,
                "session.updated",
                serde_json::json!({"status": "failed"}),
            )?);
            events.push(insert_event_tx(
                &tx,
                session_id,
                "agent.failed",
                serde_json::json!({
                    "error": "The daemon restarted while this run was active."
                }),
            )?);
        }
        for (question_id, session_id) in &expired_questions {
            let question = tx.query_row(
                "SELECT id, session_id, agent_id, agent_name, question, options, status,
                        answer, created_at, answered_at FROM questions WHERE id = ?1",
                params![question_id],
                map_question,
            )?;
            events.push(insert_event_tx(
                &tx,
                session_id,
                "question.expired",
                serde_json::to_value(&question)?,
            )?);
        }
        for (agent_id, session_id) in &agents {
            let agent = get_agent_tx(&tx, agent_id)?;
            events.push(insert_event_tx(
                &tx,
                session_id,
                "agent.updated",
                serde_json::json!({"agent": agent}),
            )?);
        }
        for (session_id, task_id) in &interrupted_tasks {
            events.push(insert_event_tx(
                &tx,
                session_id,
                "plan_task.interrupted",
                serde_json::json!({
                    "task_id": task_id,
                    "task_status": "interrupted",
                    "reason": "daemon restarted while the task was active",
                }),
            )?);
        }
        for steering in &interrupted_steering {
            let mut cancelled = steering.clone();
            cancelled.status = "cancelled".to_string();
            let mut payload = serde_json::to_value(&cancelled)?;
            if let Some(object) = payload.as_object_mut() {
                object.insert(
                    "reason".into(),
                    serde_json::json!("daemon restarted before delivery"),
                );
            }
            events.push(insert_event_tx(
                &tx,
                &cancelled.session_id,
                "steering.cancelled",
                payload,
            )?);
        }
        for continuation in &legacy_continuations {
            let mut failed = continuation.clone();
            failed.status = "failed".to_string();
            failed.last_error =
                Some("daemon restarted while delivering; not retried automatically".to_string());
            failed.next_attempt_at = None;
            failed.updated_at = now.clone();
            failed.lease_owner = None;
            failed.lease_expires_at = None;
            let mut payload = serde_json::to_value(&failed)?;
            if let Some(object) = payload.as_object_mut() {
                object.insert(
                    "reason".into(),
                    serde_json::json!(
                        "daemon restarted while delivering; not retried automatically"
                    ),
                );
            }
            events.push(insert_event_tx(
                &tx,
                &failed.session_id,
                "session.continuation.failed",
                payload,
            )?);
        }
        for continuation in &expired_continuations {
            let mut reclaimed = continuation.clone();
            reclaimed.status = "ready".to_string();
            reclaimed.last_error =
                Some("delivery claim expired before acknowledgement; reclaimed".to_string());
            reclaimed.next_attempt_at = None;
            reclaimed.updated_at = now.clone();
            reclaimed.lease_owner = None;
            reclaimed.lease_expires_at = None;
            let mut payload = serde_json::to_value(&reclaimed)?;
            if let Some(object) = payload.as_object_mut() {
                object.insert(
                    "reason".into(),
                    serde_json::json!("delivery claim expired before acknowledgement"),
                );
            }
            events.push(insert_event_tx(
                &tx,
                &reclaimed.session_id,
                "session.continuation.reclaimed",
                payload,
            )?);
        }
        for (approval_id, session_id) in &approvals {
            events.push(insert_event_tx(
                &tx,
                session_id,
                "approval.resolved",
                serde_json::json!({
                    "approval_id": approval_id,
                    "approved": false,
                    "resolved_at": now,
                }),
            )?);
        }
        for id in &queue_ids {
            let entry = tx.query_row(
                "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, pre_merge_commit, status, attempt, detail, source, remote_host, remote_branch, bundle_sha256, imported_ref, verifier_approved, verifier_summary, environment_caveat, remote_test_evidence_id, remote_test_digest, remote_test_status, created_at, updated_at
                 FROM merge_queue WHERE id = ?1",
                params![id],
                map_merge_queue_entry,
            )?;
            events.push(insert_event_tx(
                &tx,
                &entry.session_id,
                "merge_queue.updated",
                serde_json::json!({"entry": entry}),
            )?);
        }
        tx.commit()?;
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok((sessions, events))
    }
}

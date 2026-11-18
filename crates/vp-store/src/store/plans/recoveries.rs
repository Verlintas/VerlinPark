use super::*;

impl Store {
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
}

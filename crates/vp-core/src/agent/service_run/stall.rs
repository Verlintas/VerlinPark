use super::*;

impl AgentService {
    /// Advance pending stall-recovery requests. This is deliberately separate
    /// from the task scheduler: it first waits for the cancelled task to become
    /// terminal and for the session to become quiescent, then reuses the normal
    /// `retry_plan_task` + `resume_plan` path once. It never widens a budget,
    /// capacity, write-scope, approval, Verifier, or merge gate.
    pub async fn reconcile_stall_recoveries(&self) -> Result<usize> {
        let store = self.engine.store().clone();
        let rows =
            tokio::task::spawn_blocking(move || store.list_active_orchestration_recoveries(200))
                .await??;
        let mut advanced = 0usize;
        for recovery in rows {
            if self.reconcile_stall_recovery(recovery).await? {
                advanced = advanced.saturating_add(1);
            }
        }
        Ok(advanced)
    }

    pub(in crate::agent) async fn reconcile_stall_recovery(
        &self,
        recovery: OrchestrationRecovery,
    ) -> Result<bool> {
        let store = self.engine.store().clone();
        let session_id = recovery.session_id.clone();
        let Some(session) =
            tokio::task::spawn_blocking(move || store.get_session(&session_id)).await??
        else {
            return Ok(false);
        };
        let policy = self
            .engine
            .orchestration_policy(&session.project_id)
            .await?;
        if policy.stall_recovery != StallRecoveryMode::Retry {
            // The project policy changed after the request. Leave the row
            // untouched rather than mutating scheduling state under observe.
            return Ok(false);
        }
        let store = self.engine.store().clone();
        let session_id = recovery.session_id.clone();
        let tasks =
            tokio::task::spawn_blocking(move || store.list_plan_tasks(None, Some(&session_id)))
                .await??;
        let Some(task) = tasks
            .iter()
            .find(|task| task.plan_seq == recovery.plan_seq && task.task_id == recovery.task_id)
        else {
            return Ok(false);
        };
        let session_terminal = matches!(
            session.status.as_str(),
            "idle" | "failed" | "cancelled" | "draft"
        );
        let now = time::OffsetDateTime::now_utc();
        let grace_expired = recovery
            .next_action_at
            .as_deref()
            .and_then(|value| {
                time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
                    .ok()
            })
            .is_some_and(|deadline| now >= deadline);

        // A crash can land after the durable intent but before the cancel
        // request. Re-issue the same cooperative cancellation once the task
        // row is observed again.
        if recovery.status == "requested" && matches!(task.status.as_str(), "running" | "claimed") {
            match self
                .engine
                .request_plan_task_cancel(&recovery.session_id, &recovery.task_id)
                .await
            {
                Ok(_) => {
                    self.mark_recovery_waiting_cancel(
                        &recovery,
                        "cancellation requested; waiting for the task and session to settle",
                    )
                    .await?;
                    return Ok(true);
                }
                Err(error) => {
                    if grace_expired {
                        self.fail_recovery(
                            &recovery,
                            &format!(
                                "stall recovery grace expired before cancellation could be re-requested: {error:#}"
                            ),
                        )
                        .await?;
                        return Ok(true);
                    }
                    return Ok(false);
                }
            }
        }

        match task.status.as_str() {
            "completed" | "skipped" => {
                self.finish_recovery_unnecessary(&recovery, "task completed before recovery retry")
                    .await?;
                Ok(true)
            }
            "cancelled" | "failed" | "interrupted" => {
                if !session_terminal {
                    if grace_expired {
                        self.fail_recovery(
                            &recovery,
                            "stall recovery grace expired before the session became idle",
                        )
                        .await?;
                        return Ok(true);
                    }
                    return Ok(false);
                }
                if crate::session_budget_exhausted(&session) {
                    self.fail_recovery(
                        &recovery,
                        "session budget is exhausted; the stalled task was not resumed",
                    )
                    .await?;
                    return Ok(true);
                }
                if recovery.attempts > policy.stall_max_attempts {
                    self.fail_recovery(&recovery, "stall recovery retry budget is exhausted")
                        .await?;
                    return Ok(true);
                }
                match self
                    .engine
                    .retry_plan_task(&recovery.session_id, &recovery.task_id)
                    .await
                {
                    Ok(_) => {
                        self.complete_recovery_retry(&recovery).await?;
                        match self.resume_plan(&recovery.session_id).await {
                            Ok(true) => {}
                            Ok(false) => tracing::warn!(
                                session_id = %recovery.session_id,
                                task_id = %recovery.task_id,
                                "stall recovery retried a task; dispatch is deferred by the resource governor"
                            ),
                            Err(error) => tracing::warn!(
                                %error,
                                session_id = %recovery.session_id,
                                task_id = %recovery.task_id,
                                "stall recovery retried a task but the plan resume failed"
                            ),
                        }
                    }
                    Err(error) => {
                        self.fail_recovery(
                            &recovery,
                            &format!("stall recovery retry failed: {error:#}"),
                        )
                        .await?;
                    }
                }
                Ok(true)
            }
            "pending" if session_terminal => {
                // A crash can land after `plan_task.retried` and before the
                // recovery row is finalized. Treat the pending task as the
                // already-applied retry, finalize the row, and resume once.
                if crate::session_budget_exhausted(&session) {
                    self.fail_recovery(
                        &recovery,
                        "session budget is exhausted; the stalled task was not resumed",
                    )
                    .await?;
                    return Ok(true);
                }
                self.complete_recovery_retry(&recovery).await?;
                match self.resume_plan(&recovery.session_id).await {
                    Ok(true) => {}
                    Ok(false) => tracing::warn!(
                        session_id = %recovery.session_id,
                        task_id = %recovery.task_id,
                        "stall recovery retry is pending; dispatch is deferred by the resource governor"
                    ),
                    Err(error) => tracing::warn!(
                        %error,
                        session_id = %recovery.session_id,
                        task_id = %recovery.task_id,
                        "stall recovery resume after a partial retry failed"
                    ),
                }
                Ok(true)
            }
            _ => {
                if grace_expired {
                    self.fail_recovery(
                        &recovery,
                        "stall recovery grace expired before the task became terminal",
                    )
                    .await?;
                    return Ok(true);
                }
                Ok(false)
            }
        }
    }

    pub(in crate::agent) async fn complete_recovery_retry(
        &self,
        recovery: &OrchestrationRecovery,
    ) -> Result<()> {
        let store = self.engine.store().clone();
        let session_id = recovery.session_id.clone();
        let task_id = recovery.task_id.clone();
        let plan_seq = recovery.plan_seq;
        let result = tokio::task::spawn_blocking(move || {
            store.complete_stall_recovery_retry(&session_id, plan_seq, &task_id)
        })
        .await??;
        if let Some((_row, event)) = result {
            self.engine.broadcast_event(event);
        }
        Ok(())
    }

    pub(in crate::agent) async fn mark_recovery_waiting_cancel(
        &self,
        recovery: &OrchestrationRecovery,
        reason: &str,
    ) -> Result<()> {
        let store = self.engine.store().clone();
        let session_id = recovery.session_id.clone();
        let task_id = recovery.task_id.clone();
        let plan_seq = recovery.plan_seq;
        let reason = reason.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.mark_stall_recovery_waiting_cancel(&session_id, plan_seq, &task_id, &reason)
        })
        .await??;
        if let Some((_row, event)) = result {
            self.engine.broadcast_event(event);
        }
        Ok(())
    }

    pub(in crate::agent) async fn finish_recovery_unnecessary(
        &self,
        recovery: &OrchestrationRecovery,
        reason: &str,
    ) -> Result<()> {
        let store = self.engine.store().clone();
        let session_id = recovery.session_id.clone();
        let task_id = recovery.task_id.clone();
        let plan_seq = recovery.plan_seq;
        let reason = reason.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.finish_stall_recovery_unnecessary(&session_id, plan_seq, &task_id, &reason)
        })
        .await??;
        if let Some((_row, event)) = result {
            self.engine.broadcast_event(event);
        }
        Ok(())
    }

    pub(in crate::agent) async fn fail_recovery(
        &self,
        recovery: &OrchestrationRecovery,
        reason: &str,
    ) -> Result<()> {
        let store = self.engine.store().clone();
        let session_id = recovery.session_id.clone();
        let task_id = recovery.task_id.clone();
        let plan_seq = recovery.plan_seq;
        let reason_for_store = reason.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.fail_stall_recovery(&session_id, plan_seq, &task_id, &reason_for_store)
        })
        .await??;
        if let Some((_row, event)) = result {
            self.engine.broadcast_event(event);
            self.engine
                .record_stall_escalation(&recovery.session_id, &recovery.task_id, reason)
                .await?;
        }
        Ok(())
    }
}

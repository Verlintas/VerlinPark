use super::*;

impl Engine {
    /// Bounded turn/tool progress observation for a task-scoped run.
    pub async fn touch_task_progress(
        &self,
        session_id: &str,
        task_id: Option<&str>,
        turn: u32,
        tool: Option<&str>,
    ) -> Result<bool> {
        let Some(task_id) = task_id else {
            return Ok(false);
        };
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let task_id = task_id.to_string();
        let tool = tool.map(str::to_string);
        tokio::task::spawn_blocking(move || {
            store.touch_plan_task_progress(&session_id, &task_id, turn, tool.as_deref())
        })
        .await?
    }

    /// Run one stall sweep. Observation is always recorded; when the project
    /// policy opts into `escalate` or `retry`, the same fresh observations may
    /// advance a bounded recovery. Zero disables the sweep and recovery.
    pub async fn sweep_task_stalls(&self, threshold: std::time::Duration) -> Result<usize> {
        self.sweep_task_stalls_at(threshold, time::OffsetDateTime::now_utc())
            .await
    }

    /// Deterministic-time variant used by tests and replay repairs.
    pub async fn sweep_task_stalls_at(
        &self,
        threshold: std::time::Duration,
        now: time::OffsetDateTime,
    ) -> Result<usize> {
        if threshold.is_zero() {
            return Ok(0);
        }
        let store = self.store.clone();
        let (stalls, events) = tokio::task::spawn_blocking(move || {
            store.sweep_task_stalls_with_events_at(threshold, now)
        })
        .await??;
        for event in events {
            self.broadcast_event(event);
        }
        self.evaluate_stall_recoveries(&stalls).await?;
        Ok(stalls.len())
    }

    async fn evaluate_stall_recoveries(&self, stalls: &[TaskStall]) -> Result<()> {
        for stall in stalls {
            let policy = self.orchestration_policy(&stall.project_id).await?;
            let store = self.store.clone();
            let session_id = stall.session_id.clone();
            let plan_seq = stall.plan_seq;
            let task_id = stall.task_id.clone();
            let recovery = tokio::task::spawn_blocking(move || {
                store.get_orchestration_recovery(&session_id, plan_seq, &task_id)
            })
            .await??;
            let decision =
                crate::orchestration::decide_stall_recovery(&policy, recovery.as_ref(), stall);
            match decision {
                crate::orchestration::StallRecoveryDecision::None => {}
                crate::orchestration::StallRecoveryDecision::Escalate => {
                    let reason = format!(
                        "{} produced {} stall observation(s); latest observation was {}s old",
                        stall.task_id, stall.stall_count, stall.age_secs
                    );
                    let session_id = stall.session_id.clone();
                    let task_id = stall.task_id.clone();
                    let store = self.store.clone();
                    let owned_stall = stall.clone();
                    let reason_for_store = reason.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        store.escalate_stall_recovery(&owned_stall, &reason_for_store)
                    })
                    .await??;
                    let Some((_recovery, event)) = result else {
                        continue;
                    };
                    self.broadcast_event(event);
                    self.record_stall_escalation(&session_id, &task_id, &reason)
                        .await?;
                }
                crate::orchestration::StallRecoveryDecision::Retry => {
                    self.request_stall_recovery_retry(stall, &policy).await?;
                }
                crate::orchestration::StallRecoveryDecision::Exhausted { reason } => {
                    self.record_stall_recovery_failure(stall, &reason).await?;
                }
            }
        }
        Ok(())
    }

    async fn request_stall_recovery_retry(
        &self,
        stall: &TaskStall,
        policy: &OrchestrationPolicy,
    ) -> Result<()> {
        let store = self.store.clone();
        let session_id = stall.session_id.clone();
        let session = tokio::task::spawn_blocking(move || store.get_session(&session_id))
            .await??
            .ok_or_else(|| anyhow!("session not found: {}", stall.session_id))?;
        if session_budget_exhausted(&session) {
            self.record_stall_recovery_failure(
                stall,
                "session budget is exhausted; automatic stall recovery did not start",
            )
            .await?;
            return Ok(());
        }
        let reason = format!(
            "{} produced {} stall observation(s); latest observation was {}s old",
            stall.task_id, stall.stall_count, stall.age_secs
        );
        let store = self.store.clone();
        let owned = stall.clone();
        let reason_for_store = reason.clone();
        let max_attempts = policy.stall_max_attempts;
        let grace_secs = policy.stall_grace_secs;
        let requested = tokio::task::spawn_blocking(move || {
            store.begin_stall_retry_recovery(&owned, max_attempts, grace_secs, &reason_for_store)
        })
        .await??;
        let Some((_recovery, event)) = requested else {
            return Ok(());
        };
        self.broadcast_event(event);
        match self
            .request_plan_task_cancel(&stall.session_id, &stall.task_id)
            .await
        {
            Ok(_) => {
                self.finish_stall_recovery_cancel(
                    stall,
                    "cancellation requested; waiting for the task and session to settle",
                )
                .await?;
            }
            Err(error) => {
                let store = self.store.clone();
                let session_id = stall.session_id.clone();
                let task_id = stall.task_id.clone();
                let status = tokio::task::spawn_blocking(move || {
                    store.plan_task_status(&session_id, &task_id)
                })
                .await??;
                match status.as_deref() {
                    Some("completed" | "skipped") | None => {
                        self.finish_stall_recovery_unnecessary(
                            stall,
                            "task completed before the cancellation request took effect",
                        )
                        .await?;
                    }
                    Some("cancelled" | "interrupted" | "failed" | "cancelling") => {
                        self.finish_stall_recovery_cancel(
                            stall,
                            "the task ended before cancellation was delivered; waiting for the session to settle",
                        )
                        .await?;
                    }
                    Some(_) => {
                        self.record_stall_recovery_failure(
                            stall,
                            &format!("cooperative cancellation failed: {error:#}"),
                        )
                        .await?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn finish_stall_recovery_cancel(&self, stall: &TaskStall, reason: &str) -> Result<()> {
        let store = self.store.clone();
        let session_id = stall.session_id.clone();
        let task_id = stall.task_id.clone();
        let plan_seq = stall.plan_seq;
        let reason = reason.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.mark_stall_recovery_waiting_cancel(&session_id, plan_seq, &task_id, &reason)
        })
        .await??;
        if let Some((_recovery, event)) = result {
            self.broadcast_event(event);
        }
        Ok(())
    }

    async fn finish_stall_recovery_unnecessary(
        &self,
        stall: &TaskStall,
        reason: &str,
    ) -> Result<()> {
        let store = self.store.clone();
        let session_id = stall.session_id.clone();
        let task_id = stall.task_id.clone();
        let plan_seq = stall.plan_seq;
        let reason = reason.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.finish_stall_recovery_unnecessary(&session_id, plan_seq, &task_id, &reason)
        })
        .await??;
        if let Some((_recovery, event)) = result {
            self.broadcast_event(event);
        }
        Ok(())
    }

    pub(crate) async fn record_stall_recovery_failure(
        &self,
        stall: &TaskStall,
        reason: &str,
    ) -> Result<()> {
        let store = self.store.clone();
        let owned = stall.clone();
        let reason_for_store = reason.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.fail_stall_recovery_for_stall(&owned, &reason_for_store)
        })
        .await??;
        if let Some((_recovery, event)) = result {
            self.broadcast_event(event);
            self.record_stall_escalation(&stall.session_id, &stall.task_id, reason)
                .await?;
        }
        Ok(())
    }

    pub(crate) async fn record_stall_escalation(
        &self,
        session_id: &str,
        task_id: &str,
        reason: &str,
    ) -> Result<()> {
        self.record_coordination_note(
            session_id,
            "escalate",
            "critical",
            Some(task_id),
            None,
            None,
            "orchestrator",
            "Orchestrator",
            Some("orchestrator"),
            &format!("Automatic stall recovery escalation for {task_id}: {reason}"),
        )
        .await?;
        Ok(())
    }

    pub async fn orchestration_recoveries(
        &self,
        session_id: &str,
    ) -> Result<OrchestrationRecoveriesResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let recoveries = tokio::task::spawn_blocking(move || {
            store.list_current_orchestration_recoveries(None, Some(&id))
        })
        .await??;
        Ok(OrchestrationRecoveriesResult {
            session_id: session_id.to_string(),
            recoveries,
        })
    }

    pub async fn session_stalls(&self, session_id: &str) -> Result<SessionStallsResult> {
        let threshold_secs = task_stall_threshold_secs();
        let store = self.store.clone();
        let id = session_id.to_string();
        let progress =
            tokio::task::spawn_blocking(move || store.list_plan_task_progress(None, Some(&id)))
                .await??;
        let events = self.recent_events(session_id, 5_000).await?;
        let mut stalls = events
            .into_iter()
            .filter(|event| event.kind == "orchestration.stall")
            .filter_map(|event| serde_json::from_value::<TaskStall>(event.payload).ok())
            .collect::<Vec<_>>();
        stalls.truncate(200);
        Ok(SessionStallsResult {
            session_id: session_id.to_string(),
            threshold_secs,
            progress,
            stalls,
        })
    }
}

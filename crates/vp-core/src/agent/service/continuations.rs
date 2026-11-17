use super::*;

impl AgentService {
    /// Evaluate deferred continuations and start at most one normal session
    /// run per session in this pass. The scheduler loop calls this on every
    /// tick; it shares the same `AgentService` and `start` path as operator
    /// prompts, so approvals, sandboxing, capacity, and budgets are unchanged.
    pub async fn reconcile_session_continuations(&self) -> Result<usize> {
        let store = self.engine.store().clone();
        let open =
            tokio::task::spawn_blocking(move || store.list_open_session_continuations()).await??;
        let now = time::OffsetDateTime::now_utc();
        let mut started = 0usize;
        let mut attempted_sessions = HashSet::new();
        let mut task_cache: HashMap<String, Vec<ScheduledTask>> = HashMap::new();

        for continuation in open {
            if attempted_sessions.contains(&continuation.session_id) {
                continue;
            }
            let store = self.engine.store().clone();
            let session_id = continuation.session_id.clone();
            let session =
                tokio::task::spawn_blocking(move || store.get_session(&session_id)).await??;
            let Some(session) = session else {
                self.engine
                    .fail_session_continuation(&continuation.id, "session not found")
                    .await?;
                continue;
            };
            match session.status.as_str() {
                "imported" => {
                    self.engine
                        .cancel_session_continuation_with_reason(
                            &continuation.id,
                            "session is an imported read-only archive",
                        )
                        .await?;
                    continue;
                }
                "landed" => {
                    self.engine
                        .cancel_session_continuation_with_reason(
                            &continuation.id,
                            "session is landed and final",
                        )
                        .await?;
                    continue;
                }
                _ => {}
            }
            let mut continuation = continuation;
            let gate_record =
                crate::gate::gate_for_continuation(&self.engine, &continuation).await?;
            if continuation.watch_gate_id.is_some() && gate_record.is_none() {
                self.engine
                    .fail_session_continuation(
                        &continuation.id,
                        "the watched external gate no longer exists",
                    )
                    .await?;
                continue;
            }
            let gate_terminal = gate_record
                .as_ref()
                .is_some_and(|record| record.gate.is_terminal());
            if continuation.status == "pending" {
                let tasks = if let Some(tasks) = task_cache.get(&continuation.session_id) {
                    tasks.clone()
                } else {
                    let tasks = self
                        .engine
                        .task_queue(None, Some(continuation.session_id.clone()))
                        .await?
                        .tasks;
                    task_cache.insert(continuation.session_id.clone(), tasks.clone());
                    tasks
                };
                let Some(wake) = crate::continuation::evaluate_wake_with_gate(
                    &continuation.watch_task_ids,
                    &continuation.watch_mode,
                    continuation.deadline_at.as_deref(),
                    &tasks,
                    gate_terminal,
                    now,
                ) else {
                    continue;
                };
                let Some(ready) = self
                    .engine
                    .mark_session_continuation_ready(&continuation.id, &wake.reason, &wake.outcomes)
                    .await?
                else {
                    continue;
                };
                continuation = ready;
            }
            if !matches!(continuation.status.as_str(), "ready" | "delivering") {
                continue;
            }
            if let Some(next_attempt_at) = continuation.next_attempt_at.as_deref()
                && time::OffsetDateTime::parse(
                    next_attempt_at,
                    &time::format_description::well_known::Rfc3339,
                )
                .is_ok_and(|next| next > now)
            {
                continue;
            }

            match session.status.as_str() {
                "queued" | "running" | "paused" | "waiting_input" => continue,
                "idle" | "failed" | "cancelled" => {}
                _ => continue,
            }
            // Stall recovery owns the next plan resume when it is already
            // cancelling or waiting for quiescence. Let it complete first so a
            // ready continuation cannot race the recovered task.
            let recovery_active = self
                .engine
                .orchestration_recoveries(&continuation.session_id)
                .await?
                .recoveries
                .iter()
                .any(|recovery| matches!(recovery.status.as_str(), "requested" | "waiting_cancel"));
            if recovery_active {
                continue;
            }

            attempted_sessions.insert(continuation.session_id.clone());
            let Some(claim) = self
                .engine
                .claim_session_continuation_for_delivery(
                    &continuation.id,
                    continuation_claim_secs(),
                )
                .await?
            else {
                // Another reconciliation pass owns a live claim. It is now
                // `delivering`; this pass must not start a duplicate run.
                continue;
            };
            let continuation = claim.continuation.clone();
            let gate_summary = gate_record.as_ref().map(crate::gate::prompt_summary);
            let reason = continuation.wake_reason.as_deref().unwrap_or("watch");
            let prompt = crate::continuation::render_prompt(&crate::continuation::PromptContext {
                id: &continuation.id,
                series_id: &continuation.series_id,
                run_count: continuation.run_count,
                recurrence_every_secs: continuation.recurrence_every_secs,
                recurrence_max_runs: continuation.recurrence_max_runs,
                body: &continuation.body,
                reason,
                outcomes: &continuation.outcomes,
                gate: gate_summary.as_ref(),
            });
            // The operation receipt binds this continuation to the normal
            // start path. If the daemon crashed after starting but before the
            // claim was acknowledged, a reclaimed owner sees `unknown` and
            // fails closed instead of starting a second run.
            let operation_id = format!("continuation-delivery:{}", continuation.id);
            match self
                .start_with_operation(
                    &continuation.session_id,
                    &prompt,
                    Some(operation_id.as_str()),
                )
                .await
            {
                Ok(()) => match self.engine.ack_session_continuation_delivered(&claim).await {
                    Ok(Some(_)) => {
                        started = started.saturating_add(1);
                    }
                    Ok(None) => {
                        tracing::warn!(
                            continuation_id = %continuation.id,
                            "continuation delivered but its acknowledgement was not applied"
                        );
                    }
                    Err(crate::SessionContinuationAckError::Lost { .. }) => {
                        tracing::warn!(
                            continuation_id = %continuation.id,
                            "continuation delivery claim was reclaimed before acknowledgement"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(
                            %error,
                            continuation_id = %continuation.id,
                            "continuation delivery acknowledgement failed"
                        );
                    }
                },
                Err(error) => {
                    let error = format!("start failed: {error}");
                    let attempts = continuation.attempts.saturating_add(1);
                    if attempts >= continuation.max_attempts.max(1) {
                        match self
                            .engine
                            .ack_session_continuation_failed(&claim, &error)
                            .await
                        {
                            Ok(_) => {}
                            Err(crate::SessionContinuationAckError::Lost { .. }) => {
                                tracing::warn!(
                                    continuation_id = %continuation.id,
                                    "stale continuation owner could not record failure"
                                );
                            }
                            Err(error) => {
                                tracing::warn!(
                                    %error,
                                    continuation_id = %continuation.id,
                                    "continuation failure acknowledgement failed"
                                );
                            }
                        }
                    } else {
                        let next = now
                            + time::Duration::seconds(continuation_retry_backoff_secs(attempts));
                        let next_attempt_at = next
                            .format(&time::format_description::well_known::Rfc3339)
                            .map_err(|error| anyhow!("formatting retry time: {error}"))?;
                        match self
                            .engine
                            .ack_session_continuation_retry(&claim, &error, &next_attempt_at)
                            .await
                        {
                            Ok(_) => {}
                            Err(crate::SessionContinuationAckError::Lost { .. }) => {
                                tracing::warn!(
                                    continuation_id = %continuation.id,
                                    "stale continuation owner could not schedule a retry"
                                );
                            }
                            Err(error) => {
                                tracing::warn!(
                                    %error,
                                    continuation_id = %continuation.id,
                                    "continuation retry acknowledgement failed"
                                );
                            }
                        }
                    }
                }
            }
        }
        Ok(started)
    }
}

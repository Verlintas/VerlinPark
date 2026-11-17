use super::*;

impl Engine {
    /// Defer an operator prompt until watched plan tasks settle or a deadline
    /// arrives. This is a persisted session-level continuation, not a plan
    /// dependency: it never changes task ordering, approvals, budgets, or
    /// write scopes.
    #[allow(clippy::too_many_arguments)]
    pub async fn defer_session(
        &self,
        session_id: &str,
        body: &str,
        watch_task_ids: Vec<String>,
        watch_mode: Option<&str>,
        deadline_at: Option<&str>,
        after_secs: Option<u64>,
        max_attempts: Option<u32>,
        every_secs: Option<u64>,
        max_runs: Option<u32>,
        operation_id: Option<&str>,
    ) -> Result<SessionDeferResult> {
        self.defer_session_watching_gate(
            session_id,
            body,
            watch_task_ids,
            watch_mode,
            deadline_at,
            after_secs,
            max_attempts,
            every_secs,
            max_runs,
            None,
            operation_id,
        )
        .await
    }

    /// Defer an operator prompt until watched plan tasks settle, a deadline
    /// arrives, or an existing external gate resolves, expires, or is
    /// cancelled. A gate watch is additive; it does not change approvals,
    /// budgets, write scopes, the Verifier, or the merge queue.
    #[allow(clippy::too_many_arguments)]
    pub async fn defer_session_watching_gate(
        &self,
        session_id: &str,
        body: &str,
        watch_task_ids: Vec<String>,
        watch_mode: Option<&str>,
        deadline_at: Option<&str>,
        after_secs: Option<u64>,
        max_attempts: Option<u32>,
        every_secs: Option<u64>,
        max_runs: Option<u32>,
        watch_gate_id: Option<&str>,
        operation_id: Option<&str>,
    ) -> Result<SessionDeferResult> {
        let mut payload = serde_json::json!({
            "session_id": session_id,
            "body": body,
            "watch_task_ids": &watch_task_ids,
            "watch_mode": watch_mode,
            "deadline_at": deadline_at,
            "after_secs": after_secs,
            "max_attempts": max_attempts,
            "every_secs": every_secs,
            "max_runs": max_runs,
        });
        if let Some(watch_gate_id) = watch_gate_id {
            payload["watch_gate_id"] = serde_json::json!(watch_gate_id);
        }
        let receipt = match crate::operation::begin(
            self.store(),
            session_id,
            operation_id,
            "session.defer",
            &payload,
        )
        .await?
        {
            crate::operation::OperationStart::Replay(result) => {
                return crate::operation::replay(&result);
            }
            crate::operation::OperationStart::Proceed(receipt) => receipt,
        };
        let result = self
            .defer_session_inner(
                session_id,
                body,
                watch_task_ids,
                watch_mode,
                deadline_at,
                after_secs,
                max_attempts,
                every_secs,
                max_runs,
                watch_gate_id,
            )
            .await;
        match &result {
            Ok(value) => {
                if let Err(error) = crate::operation::complete(&receipt, value).await {
                    return Err(error.context(
                        "continuation was created but the session operation receipt could not be completed",
                    ));
                }
            }
            Err(error) => {
                crate::operation::fail(&receipt, &format!("{error:#}")).await;
            }
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn defer_session_inner(
        &self,
        session_id: &str,
        body: &str,
        watch_task_ids: Vec<String>,
        watch_mode: Option<&str>,
        deadline_at: Option<&str>,
        after_secs: Option<u64>,
        max_attempts: Option<u32>,
        every_secs: Option<u64>,
        max_runs: Option<u32>,
        watch_gate_id: Option<&str>,
    ) -> Result<SessionDeferResult> {
        let body = crate::continuation::validate_body(body)?;
        let watch_mode = crate::continuation::normalize_watch_mode(watch_mode)?;
        let recurrence = crate::continuation::normalize_recurrence(every_secs, max_runs)?;
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        if session.status == "imported" {
            anyhow::bail!("imported sessions are read-only archives");
        }
        if session.status == "landed" {
            anyhow::bail!("landed sessions are final; create a new session or fork to continue");
        }

        let mut seen = std::collections::HashSet::new();
        let mut watch_task_ids = watch_task_ids
            .into_iter()
            .map(|task_id| task_id.trim().to_string())
            .filter(|task_id| !task_id.is_empty())
            .collect::<Vec<_>>();
        watch_task_ids.retain(|task_id| seen.insert(task_id.clone()));
        if watch_task_ids.len() > crate::continuation::MAX_WATCH_TASKS {
            anyhow::bail!(
                "a continuation may watch at most {} tasks",
                crate::continuation::MAX_WATCH_TASKS
            );
        }
        for task_id in &watch_task_ids {
            crate::worktree::validate_task_id(task_id)?;
        }
        let watch_gate_id = watch_gate_id
            .map(str::trim)
            .filter(|gate_id| !gate_id.is_empty())
            .map(str::to_string);
        if let Some(gate_id) = &watch_gate_id {
            if gate_id.chars().count() > 200 || gate_id.chars().any(char::is_control) {
                return Err(crate::gate::gate_error(
                    crate::gate::codes::GATE_INVALID,
                    "gate_id is invalid",
                ));
            }
            let store = self.store.clone();
            let lookup = gate_id.clone();
            let record = tokio::task::spawn_blocking(move || store.get_gate(&lookup))
                .await??
                .ok_or_else(|| {
                    crate::gate::gate_error(
                        crate::gate::codes::GATE_NOT_FOUND,
                        format!("gate {gate_id} does not exist"),
                    )
                })?;
            if record.gate.session_id != session_id {
                return Err(crate::gate::gate_error(
                    crate::gate::codes::GATE_NOT_FOUND,
                    "the gate does not belong to this session",
                ));
            }
            if !record.gate.is_open() {
                return Err(crate::gate::gate_error(
                    crate::gate::codes::GATE_INVALID,
                    format!(
                        "gate {gate_id} is already {}; read its outcome instead of waiting",
                        record.gate.state.as_str()
                    ),
                ));
            }
            let store = self.store.clone();
            let lookup = gate_id.clone();
            if tokio::task::spawn_blocking(move || store.has_open_gate_watch(&lookup)).await?? {
                return Err(crate::gate::gate_error(
                    crate::gate::codes::GATE_ALREADY_WATCHED,
                    format!("gate {gate_id} already has an open continuation watch"),
                ));
            }
        }
        let deadline_at = crate::continuation::resolve_deadline(
            after_secs,
            deadline_at,
            time::OffsetDateTime::now_utc(),
        )?;
        if watch_task_ids.is_empty() && deadline_at.is_none() && watch_gate_id.is_none() {
            anyhow::bail!("a continuation needs a watch task, a deadline, or a gate");
        }

        if !watch_task_ids.is_empty() {
            let plan = self.get_team_plan(session_id).await?;
            let Some(plan) = plan else {
                anyhow::bail!("session has no stored TeamPlan to watch");
            };
            let plan_ids = plan
                .tasks
                .iter()
                .map(|task| task.id.as_str())
                .collect::<std::collections::HashSet<_>>();
            if let Some(missing) = watch_task_ids
                .iter()
                .find(|task_id| !plan_ids.contains(task_id.as_str()))
            {
                anyhow::bail!("task {missing} is not in the stored TeamPlan");
            }
            let store = self.store.clone();
            let id = session_id.to_string();
            let tasks = tokio::task::spawn_blocking(move || store.list_plan_tasks(None, Some(&id)))
                .await??;
            for task_id in &watch_task_ids {
                let Some(task) = tasks
                    .iter()
                    .filter(|task| &task.task_id == task_id)
                    .max_by_key(|task| task.plan_seq)
                else {
                    anyhow::bail!("task {task_id} has no scheduling state yet");
                };
                if crate::continuation::is_terminal_task_status(&task.status) {
                    anyhow::bail!(
                        "task {task_id} is already {}; read its outcome instead of watching it",
                        task.status
                    );
                }
            }
        }

        let store = self.store.clone();
        let session_id = session_id.to_string();
        let project_id = session.project_id.clone();
        let max_attempts = crate::continuation::normalize_max_attempts(max_attempts);
        let gate_for_error = watch_gate_id.clone();
        let created = tokio::task::spawn_blocking(move || {
            store.create_session_continuation_with_event(&NewSessionContinuation {
                session_id,
                project_id,
                body,
                watch_task_ids,
                watch_mode,
                watch_gate_id,
                deadline_at,
                max_attempts,
                series_id: None,
                parent_continuation_id: None,
                recurrence_every_secs: recurrence.every_secs,
                recurrence_max_runs: recurrence.max_runs,
                run_count: 0,
            })
        })
        .await?;
        let (continuation, event) = match created {
            Ok(created) => created,
            Err(error) => {
                if let Some(gate_id) = gate_for_error
                    && format!("{error:#}").contains("UNIQUE constraint failed")
                {
                    return Err(crate::gate::gate_error(
                        crate::gate::codes::GATE_ALREADY_WATCHED,
                        format!("gate {gate_id} already has an open continuation watch"),
                    ));
                }
                return Err(error);
            }
        };
        let _ = self.events.send(event);
        Ok(SessionDeferResult { continuation })
    }

    pub async fn session_continuations(
        &self,
        session_id: &str,
        status: Option<&str>,
        limit: Option<u32>,
    ) -> Result<SessionContinuationsResult> {
        if let Some(status) = status
            && !matches!(
                status,
                "pending" | "ready" | "delivering" | "delivered" | "cancelled" | "failed"
            )
        {
            anyhow::bail!(
                "continuation status must be pending, ready, delivering, delivered, cancelled, or failed"
            );
        }
        let store = self.store.clone();
        let id = session_id.to_string();
        let status = status.map(str::to_string);
        let continuations = tokio::task::spawn_blocking(move || {
            store.list_session_continuations(&id, status.as_deref(), limit)
        })
        .await??;
        Ok(SessionContinuationsResult {
            session_id: session_id.to_string(),
            continuations,
        })
    }

    /// Token-free status for one continuation delivery claim. The scheduler
    /// and operators use it to distinguish a live foreign claim from an
    /// expired row that is safe to reclaim.
    pub async fn session_continuation_status(
        &self,
        id: &str,
    ) -> Result<SessionContinuationStatusResult> {
        let store = self.store.clone();
        let id_owned = id.to_string();
        let continuation =
            tokio::task::spawn_blocking(move || store.get_session_continuation(&id_owned))
                .await??
                .ok_or_else(|| anyhow!("continuation not found: {id}"))?;
        let store = self.store.clone();
        let id_owned = id.to_string();
        let claim =
            tokio::task::spawn_blocking(move || store.session_continuation_claim_status(&id_owned))
                .await??;
        Ok(SessionContinuationStatusResult {
            continuation,
            claim: claim.map(|claim| SessionContinuationClaimInfo {
                status: claim.status,
                owner_id: claim.owner_id,
                expires_at: claim.expires_at,
                delivery_attempt: claim.delivery_attempt,
                live: claim.live,
            }),
        })
    }

    /// Explicitly reclaim an expired delivery claim. This is the same
    /// operation startup recovery and the scheduler use; it cannot reclaim a
    /// live claim or a terminal row.
    pub async fn reclaim_session_continuation(
        &self,
        id: &str,
        reason: Option<&str>,
    ) -> Result<SessionContinuationReclaimResult> {
        let reason = reason
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
            .unwrap_or("claim expired before acknowledgement");
        let reclaimed = self
            .reclaim_expired_session_continuation(id, reason)
            .await?;
        Ok(SessionContinuationReclaimResult {
            reclaimed: reclaimed.is_some(),
            continuation: reclaimed,
        })
    }

    pub async fn cancel_session_continuation(
        &self,
        id: &str,
    ) -> Result<SessionContinuationCancelResult> {
        let store = self.store.clone();
        let continuation_id = id.to_string();
        let reason = "cancelled by operator".to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.cancel_session_continuation_with_event(&continuation_id, &reason)
        })
        .await??;
        let Some((continuation, event)) = result else {
            anyhow::bail!("continuation {id} is not pending or ready");
        };
        let _ = self.events.send(event);
        Ok(SessionContinuationCancelResult {
            cancelled: true,
            continuation,
        })
    }

    /// Transition a pending row to ready and broadcast the event. Scheduler
    /// internals use this; replay reconstructs the same row from the payload.
    pub async fn mark_session_continuation_ready(
        &self,
        id: &str,
        reason: &str,
        outcomes: &[ContinuationOutcome],
    ) -> Result<Option<SessionContinuation>> {
        let store = self.store.clone();
        let id = id.to_string();
        let reason = reason.to_string();
        let outcomes = outcomes.to_vec();
        let result = tokio::task::spawn_blocking(move || {
            store.mark_session_continuation_ready_with_event(&id, &reason, &outcomes)
        })
        .await??;
        let Some((continuation, event)) = result else {
            return Ok(None);
        };
        let _ = self.events.send(event);
        Ok(Some(continuation))
    }

    /// Atomically claim a ready row, or reclaim an expired delivery claim,
    /// with a new process owner and token. The scheduler uses this path and
    /// acknowledges with the returned claim. A live foreign claim returns
    /// `None` and is never touched.
    pub async fn claim_session_continuation_for_delivery(
        &self,
        id: &str,
        ttl_secs: u64,
    ) -> Result<Option<SessionContinuationClaim>> {
        let store = self.store.clone();
        let id = id.to_string();
        let owner_id = self.runtime_id.clone();
        let claim = tokio::task::spawn_blocking(move || {
            store.claim_session_continuation_for_delivery_with_event(&id, &owner_id, ttl_secs)
        })
        .await??;
        let Some(claim) = claim else {
            return Ok(None);
        };
        let _ = self.events.send(claim.event.clone());
        Ok(Some(claim))
    }

    /// Backwards-compatible claim used by tests and older internal callers. It
    /// records the token in the Engine's in-memory claim map so the un-fenced
    /// acknowledgement helpers stay safe within this process. New code should
    /// call `claim_session_continuation_for_delivery` and pass the claim.
    pub async fn claim_session_continuation(
        &self,
        id: &str,
    ) -> Result<Option<SessionContinuation>> {
        let Some(claim) = self
            .claim_session_continuation_for_delivery(id, 120)
            .await?
        else {
            return Ok(None);
        };
        let continuation = claim.continuation.clone();
        if let Ok(mut claims) = self.continuation_claims.lock() {
            claims.insert(id.to_string(), claim);
        }
        Ok(Some(continuation))
    }

    fn take_continuation_claim(&self, id: &str) -> Option<SessionContinuationClaim> {
        self.continuation_claims
            .lock()
            .ok()
            .and_then(|mut claims| claims.remove(id))
    }

    /// Fenced delivery acknowledgement. A stale owner receives a typed
    /// `SessionContinuationAckError::Lost` and no terminal row is written.
    pub async fn ack_session_continuation_delivered(
        &self,
        claim: &SessionContinuationClaim,
    ) -> std::result::Result<Option<SessionContinuation>, SessionContinuationAckError> {
        let store = self.store.clone();
        let id = claim.continuation.id.clone();
        let owner = claim.owner_id.clone();
        let token = claim.token().clone();
        let result = tokio::task::spawn_blocking(move || {
            store.mark_session_continuation_delivered_with_event(&id, &owner, &token)
        })
        .await
        .map_err(|error| SessionContinuationAckError::Internal(error.to_string()))??;
        let Some(delivery) = result else {
            return Ok(None);
        };
        for event in delivery.events {
            let _ = self.events.send(event);
        }
        Ok(Some(delivery.delivered))
    }

    pub async fn ack_session_continuation_retry(
        &self,
        claim: &SessionContinuationClaim,
        error: &str,
        next_attempt_at: &str,
    ) -> std::result::Result<Option<SessionContinuation>, SessionContinuationAckError> {
        let store = self.store.clone();
        let id = claim.continuation.id.clone();
        let owner = claim.owner_id.clone();
        let token = claim.token().clone();
        let error = error.to_string();
        let next_attempt_at = next_attempt_at.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.retry_session_continuation_with_event(
                &id,
                &owner,
                &token,
                &error,
                &next_attempt_at,
            )
        })
        .await
        .map_err(|error| SessionContinuationAckError::Internal(error.to_string()))??;
        let Some((continuation, event)) = result else {
            return Ok(None);
        };
        let _ = self.events.send(event);
        Ok(Some(continuation))
    }

    pub async fn ack_session_continuation_failed(
        &self,
        claim: &SessionContinuationClaim,
        error: &str,
    ) -> std::result::Result<Option<SessionContinuation>, SessionContinuationAckError> {
        let store = self.store.clone();
        let id = claim.continuation.id.clone();
        let owner = claim.owner_id.clone();
        let token = claim.token().clone();
        let error = error.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.fail_session_continuation_with_event(&id, &owner, &token, &error)
        })
        .await
        .map_err(|error| SessionContinuationAckError::Internal(error.to_string()))??;
        let Some((continuation, event)) = result else {
            return Ok(None);
        };
        let _ = self.events.send(event);
        Ok(Some(continuation))
    }

    /// Idempotent legacy acknowledgement. A repeated terminal write or a stale
    /// in-memory claim returns `Ok(None)` instead of rewriting the row.
    pub async fn mark_session_continuation_delivered(
        &self,
        id: &str,
    ) -> Result<Option<SessionContinuation>> {
        let Some(claim) = self.take_continuation_claim(id) else {
            return Ok(None);
        };
        match self.ack_session_continuation_delivered(&claim).await {
            Ok(delivered) => Ok(delivered),
            Err(SessionContinuationAckError::Lost { .. }) => Ok(None),
            Err(error) => Err(anyhow!(error)),
        }
    }

    pub async fn retry_session_continuation(
        &self,
        id: &str,
        error: &str,
        next_attempt_at: &str,
    ) -> Result<Option<SessionContinuation>> {
        let Some(claim) = self.take_continuation_claim(id) else {
            return Ok(None);
        };
        match self
            .ack_session_continuation_retry(&claim, error, next_attempt_at)
            .await
        {
            Ok(retried) => Ok(retried),
            Err(SessionContinuationAckError::Lost { .. }) => Ok(None),
            Err(error) => Err(anyhow!(error)),
        }
    }

    pub async fn fail_session_continuation(
        &self,
        id: &str,
        error: &str,
    ) -> Result<Option<SessionContinuation>> {
        if let Some(claim) = self.take_continuation_claim(id) {
            return match self.ack_session_continuation_failed(&claim, error).await {
                Ok(failed) => Ok(failed),
                Err(SessionContinuationAckError::Lost { .. }) => Ok(None),
                Err(error) => Err(anyhow!(error)),
            };
        }
        self.fail_unclaimed_session_continuation(id, error).await
    }

    /// Fail a pending/ready row, or an expired delivery claim, without a token.
    /// A live foreign claim is left untouched.
    pub async fn fail_unclaimed_session_continuation(
        &self,
        id: &str,
        error: &str,
    ) -> Result<Option<SessionContinuation>> {
        let store = self.store.clone();
        let id = id.to_string();
        let error = error.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.fail_unclaimed_session_continuation_with_event(&id, &error)
        })
        .await??;
        let Some((continuation, event)) = result else {
            return Ok(None);
        };
        let _ = self.events.send(event);
        Ok(Some(continuation))
    }

    /// Reclaim one expired claim at startup or from an operator drill. The
    /// delivery operation receipt makes a retry safe when the old process may
    /// already have started a run.
    pub async fn reclaim_expired_session_continuation(
        &self,
        id: &str,
        reason: &str,
    ) -> Result<Option<SessionContinuation>> {
        let store = self.store.clone();
        let id = id.to_string();
        let reason = reason.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.reclaim_expired_session_continuation_with_event(&id, &reason)
        })
        .await??;
        let Some((continuation, event)) = result else {
            return Ok(None);
        };
        let _ = self.events.send(event);
        Ok(Some(continuation))
    }

    pub async fn cancel_session_continuation_with_reason(
        &self,
        id: &str,
        reason: &str,
    ) -> Result<Option<SessionContinuation>> {
        let store = self.store.clone();
        let id = id.to_string();
        let reason = reason.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.cancel_session_continuation_with_event(&id, &reason)
        })
        .await??;
        let Some((continuation, event)) = result else {
            return Ok(None);
        };
        let _ = self.events.send(event);
        Ok(Some(continuation))
    }
}

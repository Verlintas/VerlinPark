//! Durable session run journal and run lineage.

use super::*;

impl Store {
    pub fn get_session_run_policy(&self, project_id: &str) -> Result<SessionRunPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT auto_resume, max_attempts, max_age_hours
                 FROM project_session_run_policies WHERE project_id = ?1",
                params![project_id],
                |row| {
                    Ok(SessionRunPolicy {
                        auto_resume: row.get::<_, i64>(0)? != 0,
                        max_attempts: row.get::<_, i64>(1)?.clamp(1, 3) as u32,
                        max_age_hours: row.get::<_, i64>(2)?.clamp(1, 168) as u32,
                    })
                },
            )
            .optional()?;
        Ok(stored.unwrap_or_default())
    }

    pub fn has_session_run_policy(&self, project_id: &str) -> Result<bool> {
        let conn = self.lock()?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM project_session_run_policies WHERE project_id = ?1",
            params![project_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count > 0)
    }

    pub fn set_session_run_policy(
        &self,
        project_id: &str,
        policy: &SessionRunPolicy,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_session_run_policies
             (project_id, auto_resume, max_attempts, max_age_hours, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(project_id) DO UPDATE SET
                auto_resume = excluded.auto_resume,
                max_attempts = excluded.max_attempts,
                max_age_hours = excluded.max_age_hours,
                updated_at = excluded.updated_at",
            params![
                project_id,
                if policy.auto_resume { 1 } else { 0 },
                policy.max_attempts.clamp(1, 3) as i64,
                policy.max_age_hours.clamp(1, 168) as i64,
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    /// Insert a run row and its `session.run.started` event atomically.
    pub fn start_session_run(&self, run: &SessionRun) -> Result<(SessionRun, Event)> {
        validate_run_status(&run.status, &["running"])?;
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let project_id: String = tx
            .query_row(
                "SELECT project_id FROM sessions WHERE id = ?1",
                params![run.session_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| anyhow!("session not found: {}", run.session_id))?;
        if project_id != run.project_id {
            return Err(anyhow!(
                "run project {} does not match session project {project_id}",
                run.project_id
            ));
        }
        let event_seq = next_event_seq_tx(&tx, &run.session_id)?;
        let mut stored = run.clone();
        stored.last_event_seq = event_seq;
        insert_session_run_tx(&tx, &stored)?;
        let event = insert_event_tx(
            &tx,
            &stored.session_id,
            "session.run.started",
            serde_json::json!({"run": serde_json::to_value(&stored)?}),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((stored, event))
    }

    /// Record a checkpoint at a safe turn boundary. The cursor values are the
    /// durable event sequence and message-event count observed by the caller
    /// immediately before the checkpoint event was appended.
    pub fn record_run_checkpoint(
        &self,
        checkpoint: &RunCheckpoint,
    ) -> Result<(RunCheckpoint, Event)> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let mut stored = checkpoint.clone();
        if stored.seq == 0 {
            stored.seq = tx.query_row(
                "SELECT COALESCE(MAX(seq), 0) + 1 FROM run_checkpoints WHERE run_id = ?1",
                params![stored.run_id],
                |row| row.get::<_, i64>(0),
            )? as u64;
        }
        let event_seq = next_event_seq_tx(&tx, &stored.session_id)?;
        tx.execute(
            "INSERT INTO run_checkpoints
             (checkpoint_id, run_id, session_id, seq, turn, message_cursor, event_cursor,
              budget_json, worktree_head, plan_revision, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                stored.checkpoint_id,
                stored.run_id,
                stored.session_id,
                stored.seq as i64,
                i64::from(stored.turn),
                stored.message_cursor as i64,
                stored.event_cursor as i64,
                serde_json::to_string(&stored.budget)?,
                stored.worktree_head,
                i64::from(stored.plan_revision),
                stored.created_at
            ],
        )?;
        tx.execute(
            "UPDATE session_runs SET last_event_seq = ?2 WHERE run_id = ?1",
            params![stored.run_id, event_seq as i64],
        )?;
        let event = insert_event_tx(
            &tx,
            &stored.session_id,
            "session.run.checkpointed",
            serde_json::json!({"checkpoint": serde_json::to_value(&stored)?}),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((stored, event))
    }

    /// Insert a step intent and, for a tool call, its operational tool-call row.
    /// Both journal events commit in the same transaction.
    pub fn record_run_step_intent(
        &self,
        step: &RunStep,
        tool: Option<&RunToolCall>,
    ) -> Result<(RunStep, Vec<Event>)> {
        let (stored, events, _) = self.record_run_step_intent_with_effect(step, tool, None)?;
        Ok((stored, events))
    }

    /// Insert a step/tool intent and, when provided, the effect intent in the
    /// same SQLite transaction. The returned `EffectBegin.inserted` flag is
    /// false when the same logical effect already exists; the caller must then
    /// inspect its state instead of executing again.
    pub fn record_run_step_intent_with_effect(
        &self,
        step: &RunStep,
        tool: Option<&RunToolCall>,
        effect: Option<&EffectReceipt>,
    ) -> Result<(RunStep, Vec<Event>, Option<EffectBegin>)> {
        validate_step_status(&step.status, &["intent"])?;
        if let Some(effect) = effect {
            validate_effect_state(&effect.state, &[EffectState::Intent])?;
            validate_effect_receipt(effect)?;
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let mut stored = step.clone();
        if stored.seq == 0 {
            stored.seq = tx.query_row(
                "SELECT COALESCE(MAX(seq), 0) + 1 FROM run_steps WHERE run_id = ?1",
                params![stored.run_id],
                |row| row.get::<_, i64>(0),
            )? as u64;
        }
        insert_run_step_tx(&tx, &stored)?;
        if let Some(tool) = tool
            && (tool.step_id != stored.step_id || tool.run_id != stored.run_id)
        {
            return Err(anyhow!("tool call does not match its step intent"));
        }
        let step_event = insert_event_tx(
            &tx,
            &stored.session_id,
            "session.run.step_intent",
            serde_json::json!({"step": serde_json::to_value(&stored)?}),
        )?;
        let mut events = vec![step_event];
        if let Some(tool) = tool {
            insert_run_tool_call_tx(&tx, tool)?;
            events.push(insert_event_tx(
                &tx,
                &stored.session_id,
                "session.run.tool_intent",
                serde_json::json!({"tool": serde_json::to_value(tool)?}),
            )?);
        }
        let effect_begin = if let Some(effect) = effect {
            let existing = latest_effect_tx(
                &tx,
                &effect.session_id,
                effect.task_id.as_deref(),
                &effect.tool,
                &effect.target.canonical(),
                &effect.request_hash,
            )?;
            match existing {
                Some(existing)
                    if matches!(
                        existing.state,
                        EffectState::Intent | EffectState::Done | EffectState::Unknown
                    ) =>
                {
                    Some(EffectBegin {
                        receipt: existing,
                        inserted: false,
                    })
                }
                Some(existing) if existing.state == EffectState::Failed => {
                    let mut next = effect.clone();
                    next.id = Uuid::now_v7().to_string();
                    next.attempt = existing.attempt.saturating_add(1).max(1);
                    next.created_at = now_rfc3339();
                    next.resolved_at = None;
                    insert_effect_tx(&tx, &next)?;
                    events.push(insert_event_tx(
                        &tx,
                        &next.session_id,
                        vp_protocol::types::EFFECT_INTENT_EVENT,
                        serde_json::json!({"receipt": serde_json::to_value(&next)?}),
                    )?);
                    Some(EffectBegin {
                        receipt: next,
                        inserted: true,
                    })
                }
                Some(_) => None,
                None => {
                    let mut next = effect.clone();
                    if next.attempt == 0 {
                        next.attempt = 1;
                    }
                    insert_effect_tx(&tx, &next)?;
                    events.push(insert_event_tx(
                        &tx,
                        &next.session_id,
                        vp_protocol::types::EFFECT_INTENT_EVENT,
                        serde_json::json!({"receipt": serde_json::to_value(&next)?}),
                    )?);
                    Some(EffectBegin {
                        receipt: next,
                        inserted: true,
                    })
                }
            }
        } else {
            None
        };
        let last_seq = events.last().map(|event| event.seq).unwrap_or(0);
        tx.execute(
            "UPDATE session_runs SET last_event_seq = ?2 WHERE run_id = ?1",
            params![stored.run_id, last_seq as i64],
        )?;
        tx.commit()?;
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok((stored, events, effect_begin))
    }

    /// Complete or fail a step. For a tool step the operational tool row is
    /// updated in the same transaction and a bounded result/error is attached.
    pub fn record_run_step_completed(
        &self,
        step: &RunStep,
        tool: Option<&RunToolCall>,
    ) -> Result<(RunStep, Option<RunToolCall>, Vec<Event>)> {
        self.record_run_step_completed_with_effect(step, tool, None, None)
    }

    /// Complete a step/tool call and settle its effect in one transaction.
    /// When `tool_finished` is provided, the caller's `tool.finished` event is
    /// appended in the same transaction so the receipt, the journal, and the
    /// observable tool completion cannot disagree after a crash.
    pub fn record_run_step_completed_with_effect(
        &self,
        step: &RunStep,
        tool: Option<&RunToolCall>,
        effect: Option<&EffectReceipt>,
        tool_finished: Option<(&str, serde_json::Value)>,
    ) -> Result<(RunStep, Option<RunToolCall>, Vec<Event>)> {
        validate_step_status(&step.status, &["completed", "failed", "unknown"])?;
        if let Some(effect) = effect {
            validate_effect_state(
                &effect.state,
                &[EffectState::Done, EffectState::Failed, EffectState::Unknown],
            )?;
            validate_effect_receipt(effect)?;
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE run_steps
             SET status = ?2, input_hash = ?3, output_hash = ?4, payload_json = ?5,
                 updated_at = ?6
             WHERE step_id = ?1",
            params![
                step.step_id,
                step.status,
                step.input_hash,
                step.output_hash,
                serde_json::to_string(&step.payload)?,
                step.updated_at
            ],
        )?;
        if changed == 0 {
            return Err(anyhow!("run step not found: {}", step.step_id));
        }
        let mut events = vec![insert_event_tx(
            &tx,
            &step.session_id,
            "session.run.step_completed",
            serde_json::json!({"step": serde_json::to_value(step)?}),
        )?];
        let mut stored_tool = None;
        if let Some(tool) = tool {
            let changed = tx.execute(
                "UPDATE run_tool_calls
                 SET status = ?3, result_json = ?4, error = ?5, updated_at = ?6
                 WHERE run_id = ?1 AND tool_call_id = ?2",
                params![
                    tool.run_id,
                    tool.tool_call_id,
                    tool.status,
                    tool.result
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()?,
                    tool.error,
                    tool.updated_at
                ],
            )?;
            if changed == 0 {
                return Err(anyhow!(
                    "run tool call not found: {} / {}",
                    tool.run_id,
                    tool.tool_call_id
                ));
            }
            events.push(insert_event_tx(
                &tx,
                &step.session_id,
                "session.run.tool_completed",
                serde_json::json!({"tool": serde_json::to_value(tool)?}),
            )?);
            stored_tool = Some(tool.clone());
        }
        if let Some(effect) = effect {
            let now = now_rfc3339();
            let mut stored = effect.clone();
            stored.resolved_at = Some(now);
            let changed = tx.execute(
                "UPDATE effect_receipts
                 SET state = ?2, provider_key = COALESCE(?3, provider_key),
                     external_id = COALESCE(?4, external_id),
                     response_hash = COALESCE(?5, response_hash),
                     evidence_json = ?6, resolved_at = ?7
                 WHERE id = ?1 AND state = 'intent'",
                params![
                    stored.id,
                    stored.state.as_str(),
                    stored.provider_key,
                    stored.external_id,
                    stored.response_hash,
                    serde_json::to_string(&stored.evidence)?,
                    stored.resolved_at
                ],
            )?;
            if changed == 0 {
                let existing = effect_by_id_tx(&tx, &stored.id)?
                    .ok_or_else(|| anyhow!("effect receipt not found: {}", stored.id))?;
                if existing.state != stored.state {
                    return Err(anyhow!(
                        "effect receipt {} cannot transition from {} to {} in a tool completion",
                        existing.id,
                        existing.state.as_str(),
                        stored.state.as_str()
                    ));
                }
            }
            events.push(insert_effect_event_tx(&tx, &stored)?);
        }
        if let Some((kind, payload)) = tool_finished {
            events.push(insert_event_tx(&tx, &step.session_id, kind, payload)?);
        }
        let last_seq = events.last().map(|event| event.seq).unwrap_or(0);
        tx.execute(
            "UPDATE session_runs SET last_event_seq = ?2 WHERE run_id = ?1",
            params![step.run_id, last_seq as i64],
        )?;
        tx.commit()?;
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok((step.clone(), stored_tool, events))
    }

    /// Finish a run. `completed`, `failed`, `interrupted`, and `deferred` are
    /// the terminal statuses; cancellation is recorded as `interrupted` with a
    /// `cancelled` failure kind so it cannot be resumed implicitly. `deferred`
    /// is a clean external-gate handoff and is never treated as resumable.
    pub fn finish_session_run(
        &self,
        run_id: &str,
        status: &str,
        failure: Option<&vp_protocol::types::RunFailure>,
    ) -> Result<(SessionRun, Event)> {
        validate_run_status(status, &["completed", "failed", "interrupted", "deferred"])?;
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let session_id: String = tx
            .query_row(
                "SELECT session_id FROM session_runs WHERE run_id = ?1",
                params![run_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| anyhow!("run not found: {run_id}"))?;
        let event_seq = next_event_seq_tx(&tx, &session_id)?;
        let changed = tx.execute(
            "UPDATE session_runs
             SET status = ?2, failure_json = ?3, ended_at = ?4, updated_at = ?4,
                 last_event_seq = ?5
             WHERE run_id = ?1 AND status = 'running'",
            params![
                run_id,
                status,
                failure.map(serde_json::to_string).transpose()?,
                now,
                event_seq as i64
            ],
        )?;
        if changed == 0 {
            return Err(anyhow!(
                "run {run_id} is not running; terminal status cannot be overwritten"
            ));
        }
        if status != "completed" {
            // A replay claim is only final once the resumed attempt completed.
            // If it failed or was interrupted, release the claim so the next
            // attempt can return the journaled result instead of re-running a
            // write whose result never reached the persisted conversation.
            tx.execute(
                "UPDATE run_tool_calls
                 SET consumed_by_run_id = NULL, consumed_at = NULL, updated_at = ?2
                 WHERE consumed_by_run_id = ?1",
                params![run_id, now],
            )?;
        }
        let run = load_session_run_tx(&tx, run_id)?;
        let (kind, payload) = match status {
            "completed" => (
                "session.run.completed",
                serde_json::json!({"run": serde_json::to_value(&run)?}),
            ),
            "failed" => (
                "session.run.failed",
                serde_json::json!({"run": serde_json::to_value(&run)?}),
            ),
            "deferred" => (
                "session.run.deferred",
                serde_json::json!({"run": serde_json::to_value(&run)?}),
            ),
            _ => (
                "session.run.interrupted",
                serde_json::json!({"run": serde_json::to_value(&run)?}),
            ),
        };
        let event = insert_event_tx(&tx, &session_id, kind, payload)?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((run, event))
    }

    /// Mark the previous run `resumed` and insert the linked new attempt in one
    /// transaction, emitting both `session.run.resumed` and
    /// `session.run.started`.
    pub fn mark_session_run_resumed(
        &self,
        previous_run_id: &str,
        new_run: &SessionRun,
    ) -> Result<(SessionRun, Event, Event)> {
        validate_run_status(&new_run.status, &["running"])?;
        if new_run.resumed_from_run_id.as_deref() != Some(previous_run_id) {
            return Err(anyhow!(
                "resumed run must point at {previous_run_id} as its source"
            ));
        }
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let previous = load_session_run_tx(&tx, previous_run_id)?;
        if !matches!(previous.status.as_str(), "interrupted" | "failed") {
            return Err(anyhow!(
                "run {previous_run_id} is {} and cannot be resumed",
                previous.status
            ));
        }
        if previous
            .failure
            .as_ref()
            .is_some_and(|failure| matches!(failure.kind.as_str(), "cancelled" | "superseded"))
        {
            return Err(anyhow!(
                "run {previous_run_id} is {} and cannot be resumed",
                previous
                    .failure
                    .as_ref()
                    .map(|failure| failure.kind.as_str())
                    .unwrap_or("cancelled")
            ));
        }
        if previous.session_id != new_run.session_id {
            return Err(anyhow!("resumed run belongs to a different session"));
        }
        let resumed_seq = next_event_seq_tx(&tx, &new_run.session_id)?;
        let started_seq = resumed_seq.saturating_add(1);
        tx.execute(
            "UPDATE session_runs
             SET status = 'resumed', updated_at = ?2, last_event_seq = ?3
             WHERE run_id = ?1",
            params![previous_run_id, now, resumed_seq as i64],
        )?;
        let mut stored_new = new_run.clone();
        stored_new.last_event_seq = started_seq;
        insert_session_run_tx(&tx, &stored_new)?;
        let previous = load_session_run_tx(&tx, previous_run_id)?;
        let resumed = insert_event_tx(
            &tx,
            &new_run.session_id,
            "session.run.resumed",
            serde_json::json!({
                "previous_run": previous,
                "run_id": stored_new.run_id,
                "attempt": stored_new.attempt,
            }),
        )?;
        let started = insert_event_tx(
            &tx,
            &new_run.session_id,
            "session.run.started",
            serde_json::json!({"run": stored_new}),
        )?;
        tx.commit()?;
        for event in [&resumed, &started] {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok((previous, resumed, started))
    }

    /// A new top-level prompt is an explicit abandonment of older unresolved
    /// attempts in the same session. They are not deleted (their journal stays
    /// inspectable); their failure kind becomes `superseded`, which resume
    /// preflight refuses. Status is preserved so replay remains truthful about
    /// whether the prior attempt was interrupted or failed.
    pub fn supersede_resumable_session_runs(
        &self,
        session_id: &str,
        new_run_id: &str,
    ) -> Result<Vec<Event>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let mut stmt = tx.prepare(
            "SELECT run_id, status, failure_json FROM session_runs
             WHERE session_id = ?1 AND top_level = 1
               AND status IN ('interrupted', 'failed')
             ORDER BY started_at ASC, run_id ASC",
        )?;
        let rows = stmt
            .query_map(params![session_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        let mut events = Vec::new();
        for (run_id, status, failure_json) in rows {
            let failure = failure_json
                .as_deref()
                .and_then(|json| serde_json::from_str::<vp_protocol::types::RunFailure>(json).ok());
            if failure
                .as_ref()
                .is_some_and(|failure| matches!(failure.kind.as_str(), "cancelled" | "superseded"))
            {
                continue;
            }
            let previous_kind = failure
                .as_ref()
                .map(|failure| failure.kind.as_str())
                .unwrap_or("interrupted");
            let superseded = serde_json::to_string(&vp_protocol::types::RunFailure {
                kind: "superseded".to_string(),
                message: format!(
                    "superseded by new prompt run {new_run_id} (previous failure: {previous_kind})"
                ),
            })?;
            let event_seq = next_event_seq_tx(&tx, session_id)?;
            tx.execute(
                "UPDATE session_runs
                 SET failure_json = ?2, updated_at = ?3, last_event_seq = ?4
                 WHERE run_id = ?1 AND status = ?5",
                params![run_id, superseded, now, event_seq as i64, status],
            )?;
            let run = load_session_run_tx(&tx, &run_id)?;
            let kind = if status == "failed" {
                "session.run.failed"
            } else {
                "session.run.interrupted"
            };
            events.push(insert_event_tx(
                &tx,
                session_id,
                kind,
                serde_json::json!({"run": run}),
            )?);
        }
        tx.commit()?;
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok(events)
    }

    /// Startup recovery. Every `running` run becomes `interrupted`; tool-call
    /// intents become `unknown` because the process cannot know whether the
    /// side effect landed. Model-step intents stay `intent` and are labeled
    /// retryable by the resume classifier.
    pub fn interrupt_running_session_runs(&self) -> Result<(Vec<SessionRun>, Vec<Event>)> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let mut stmt = tx.prepare(
            "SELECT run_id FROM session_runs WHERE status = 'running' ORDER BY started_at ASC, run_id ASC",
        )?;
        let run_ids = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        let mut cancel_stmt =
            tx.prepare("SELECT payload FROM events WHERE type = 'session.run.cancel_requested'")?;
        let cancelled_runs = cancel_stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .filter_map(Result::ok)
            .filter_map(|payload| {
                serde_json::from_str::<serde_json::Value>(&payload)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("run_id")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                    })
            })
            .collect::<std::collections::HashSet<_>>();
        drop(cancel_stmt);
        tx.execute(
            "UPDATE run_tool_calls
             SET status = 'unknown', error = COALESCE(error, 'daemon stopped before the tool outcome was committed'),
                 consumed_by_run_id = NULL, consumed_at = NULL, updated_at = ?1
             WHERE status = 'intent'",
            params![now],
        )?;
        tx.execute(
            "UPDATE run_tool_calls
             SET consumed_by_run_id = NULL, consumed_at = NULL, updated_at = ?1
             WHERE consumed_by_run_id IS NOT NULL",
            params![now],
        )?;
        let mut runs = Vec::new();
        let mut events = Vec::new();
        for run_id in run_ids {
            let session_id: String = tx.query_row(
                "SELECT session_id FROM session_runs WHERE run_id = ?1",
                params![run_id],
                |row| row.get(0),
            )?;
            let (kind, message) = if cancelled_runs.contains(&run_id) {
                (
                    "cancelled",
                    "the operator requested cancellation before the daemon restarted",
                )
            } else {
                (
                    "interrupted",
                    "the daemon restarted while this run was active",
                )
            };
            let failure = serde_json::to_string(&vp_protocol::types::RunFailure {
                kind: kind.to_string(),
                message: message.to_string(),
            })?;
            let event_seq = next_event_seq_tx(&tx, &session_id)?;
            tx.execute(
                "UPDATE session_runs
                 SET status = 'interrupted', failure_json = ?2, ended_at = ?3, updated_at = ?3,
                     last_event_seq = ?4
                 WHERE run_id = ?1 AND status = 'running'",
                params![run_id, failure, now, event_seq as i64],
            )?;
            let run = load_session_run_tx(&tx, &run_id)?;
            events.push(insert_event_tx(
                &tx,
                &run.session_id,
                "session.run.interrupted",
                serde_json::json!({"run": serde_json::to_value(&run)?}),
            )?);
            runs.push(run);
        }
        tx.commit()?;
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok((runs, events))
    }

    /// Latest event sequence and the number of persisted `message.created`
    /// events. Checkpoints store these cursors without reading the message or
    /// event bodies into memory.
    pub fn session_has_imported_event(&self, session_id: &str) -> Result<bool> {
        let conn = self.lock()?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM events WHERE session_id = ?1 AND type IN ('session.imported', 'session.rehydrated')",
            params![session_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count > 0)
    }

    pub fn plan_revision_count(&self, session_id: &str) -> Result<u32> {
        let conn = self.lock()?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM events
             WHERE session_id = ?1 AND type IN ('team_plan.submitted', 'team_plan.revised')",
            params![session_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count.clamp(0, i64::from(u32::MAX)) as u32)
    }

    pub fn journal_cursors(&self, session_id: &str) -> Result<(u64, u64)> {
        let conn = self.lock()?;
        let (event_cursor, message_cursor) = conn.query_row(
            "SELECT COALESCE((SELECT MAX(seq) FROM events WHERE session_id = ?1), 0),
                    COALESCE((SELECT COUNT(*) FROM messages WHERE session_id = ?1), 0)",
            params![session_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?.max(0) as u64,
                    row.get::<_, i64>(1)?.max(0) as u64,
                ))
            },
        )?;
        Ok((event_cursor, message_cursor))
    }

    pub fn list_all_session_runs(&self, session_id: &str) -> Result<Vec<SessionRun>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT run_id, session_id, project_id, agent_id, agent_name, agent_role, task_id,
                    department, objective, provider, model, status, attempt, resumed_from_run_id,
                    resume_root_id, top_level, started_at, updated_at, ended_at, budget_json,
                    worktree_head, plan_revision, last_event_seq, failure_json, runtime_snapshot_json
             FROM session_runs WHERE session_id = ?1
             ORDER BY started_at ASC, run_id ASC",
        )?;
        let rows = stmt.query_map(params![session_id], map_session_run)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn list_session_runs(&self, session_id: &str, limit: u32) -> Result<Vec<SessionRun>> {
        let limit = limit.clamp(1, 500) as i64;
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT run_id, session_id, project_id, agent_id, agent_name, agent_role, task_id,
                    department, objective, provider, model, status, attempt, resumed_from_run_id,
                    resume_root_id, top_level, started_at, updated_at, ended_at, budget_json,
                    worktree_head, plan_revision, last_event_seq, failure_json, runtime_snapshot_json
             FROM session_runs WHERE session_id = ?1
             ORDER BY started_at DESC, run_id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![session_id, limit], map_session_run)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn list_resumable_session_runs(&self, limit: u32) -> Result<Vec<SessionRun>> {
        let limit = limit.clamp(1, 500) as i64;
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT run_id, session_id, project_id, agent_id, agent_name, agent_role, task_id,
                    department, objective, provider, model, status, attempt, resumed_from_run_id,
                    resume_root_id, top_level, started_at, updated_at, ended_at, budget_json,
                    worktree_head, plan_revision, last_event_seq, failure_json, runtime_snapshot_json
             FROM session_runs
             WHERE top_level = 1 AND status IN ('interrupted', 'failed')
               AND (failure_json IS NULL
                    OR (failure_json NOT LIKE '%\"kind\":\"cancelled\"%'
                        AND failure_json NOT LIKE '%\"kind\":\"superseded\"%'))
             ORDER BY started_at DESC, run_id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], map_session_run)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// True when a recorded `session.run.cancel_requested` is newer than every
    /// terminal run event in the session and at least one run is still active.
    /// A cancel for an already-terminal run is stale and does not block a later
    /// integration. This is the persisted, cross-process view of the in-memory
    /// run control used by the deterministic Integrator.
    pub fn session_run_cancel_requested(&self, session_id: &str) -> Result<bool> {
        let conn = self.lock()?;
        let cancel_seq: i64 = conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM events
             WHERE session_id = ?1 AND type = 'session.run.cancel_requested'",
            params![session_id],
            |row| row.get(0),
        )?;
        if cancel_seq == 0 {
            return Ok(false);
        }
        let terminal_seq: i64 = conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM events
             WHERE session_id = ?1
               AND type IN (
                   'session.run.completed',
                   'session.run.failed',
                   'session.run.interrupted',
                   'session.run.deferred'
               )",
            params![session_id],
            |row| row.get(0),
        )?;
        if terminal_seq >= cancel_seq {
            return Ok(false);
        }
        let running: i64 = conn.query_row(
            "SELECT COUNT(*) FROM session_runs WHERE session_id = ?1 AND status = 'running'",
            params![session_id],
            |row| row.get(0),
        )?;
        Ok(running > 0)
    }

    pub fn get_session_run(&self, run_id: &str) -> Result<Option<SessionRun>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT run_id, session_id, project_id, agent_id, agent_name, agent_role, task_id,
                    department, objective, provider, model, status, attempt, resumed_from_run_id,
                    resume_root_id, top_level, started_at, updated_at, ended_at, budget_json,
                    worktree_head, plan_revision, last_event_seq, failure_json, runtime_snapshot_json
             FROM session_runs WHERE run_id = ?1",
            params![run_id],
            map_session_run,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn list_run_steps(&self, run_id: &str) -> Result<Vec<RunStep>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT step_id, run_id, session_id, seq, turn, kind, status, input_hash,
                    output_hash, payload_json, created_at, updated_at
             FROM run_steps WHERE run_id = ?1 ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map(params![run_id], map_run_step)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn list_run_tool_calls(&self, run_id: &str) -> Result<Vec<RunToolCall>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT run_id, session_id, step_id, tool_call_id, tool_name, input_fingerprint,
                    status, result_json, error, consumed_by_run_id, consumed_at, created_at,
                    updated_at
             FROM run_tool_calls WHERE run_id = ?1 ORDER BY created_at ASC, rowid ASC",
        )?;
        let rows = stmt.query_map(params![run_id], map_run_tool_call)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn list_run_checkpoints(&self, run_id: &str) -> Result<Vec<RunCheckpoint>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT checkpoint_id, run_id, session_id, seq, turn, message_cursor, event_cursor,
                    budget_json, worktree_head, plan_revision, created_at
             FROM run_checkpoints WHERE run_id = ?1 ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map(params![run_id], map_run_checkpoint)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn list_session_run_steps(&self, session_id: &str) -> Result<Vec<RunStep>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT step_id, run_id, session_id, seq, turn, kind, status, input_hash,
                    output_hash, payload_json, created_at, updated_at
             FROM run_steps WHERE session_id = ?1 ORDER BY run_id ASC, seq ASC",
        )?;
        let rows = stmt.query_map(params![session_id], map_run_step)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn list_lineage_run_checkpoints(&self, run_id: &str) -> Result<Vec<RunCheckpoint>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT c.checkpoint_id, c.run_id, c.session_id, c.seq, c.turn,
                    c.message_cursor, c.event_cursor, c.budget_json, c.worktree_head,
                    c.plan_revision, c.created_at
             FROM run_checkpoints c
             JOIN session_runs r ON r.run_id = c.run_id
             WHERE COALESCE(NULLIF(r.resume_root_id, ''), r.run_id) = (
                 SELECT COALESCE(NULLIF(resume_root_id, ''), run_id)
                 FROM session_runs WHERE run_id = ?1
             )
             ORDER BY c.created_at ASC, c.run_id ASC, c.seq ASC",
        )?;
        let rows = stmt.query_map(params![run_id], map_run_checkpoint)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn list_session_run_checkpoints(&self, session_id: &str) -> Result<Vec<RunCheckpoint>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT checkpoint_id, run_id, session_id, seq, turn, message_cursor, event_cursor,
                    budget_json, worktree_head, plan_revision, created_at
             FROM run_checkpoints WHERE session_id = ?1 ORDER BY run_id ASC, seq ASC",
        )?;
        let rows = stmt.query_map(params![session_id], map_run_checkpoint)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Every tool row in the resume lineage whose root is the root of
    /// `run_id`. Used to build the bounded journal note and the duplicate-write
    /// gate. Results are newest-first but the caller re-orders for display.
    pub fn list_run_lineage_tool_calls(&self, run_id: &str) -> Result<Vec<RunToolCall>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT tc.run_id, tc.session_id, tc.step_id, tc.tool_call_id, tc.tool_name,
                    tc.input_fingerprint, tc.status, tc.result_json, tc.error,
                    tc.consumed_by_run_id, tc.consumed_at, tc.created_at, tc.updated_at
             FROM run_tool_calls tc
             JOIN session_runs r ON r.run_id = tc.run_id
             WHERE COALESCE(NULLIF(r.resume_root_id, ''), r.run_id) = (
                 SELECT COALESCE(NULLIF(resume_root_id, ''), run_id)
                 FROM session_runs WHERE run_id = ?1
             )
             ORDER BY tc.created_at ASC, tc.rowid ASC",
        )?;
        let rows = stmt.query_map(params![run_id], map_run_tool_call)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Find one completed journaled tool result in the resume lineage without
    /// consuming it. A replay is journaled as its own completed row in the
    /// current attempt, so an attempt that fails after the replay can find the
    /// original (or successor) row again instead of re-executing the write.
    /// `current_run_id` is excluded so one attempt cannot replay its own rows.
    pub fn find_completed_tool_result(
        &self,
        run_id: &str,
        tool_name: &str,
        input_fingerprint: &str,
        current_run_id: &str,
    ) -> Result<Option<RunToolCall>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT tc.run_id, tc.session_id, tc.step_id, tc.tool_call_id, tc.tool_name,
                    tc.input_fingerprint, tc.status, tc.result_json, tc.error,
                    tc.consumed_by_run_id, tc.consumed_at, tc.created_at, tc.updated_at
             FROM run_tool_calls tc
             JOIN session_runs r ON r.run_id = tc.run_id
             WHERE COALESCE(NULLIF(r.resume_root_id, ''), r.run_id) = (
                 SELECT COALESCE(NULLIF(resume_root_id, ''), run_id)
                 FROM session_runs WHERE run_id = ?1
             )
               AND tc.run_id <> ?4
               AND tc.status = 'completed'
               AND tc.tool_name = ?2
               AND tc.input_fingerprint = ?3
             ORDER BY tc.created_at ASC, tc.rowid ASC
             LIMIT 1",
            params![run_id, tool_name, input_fingerprint, current_run_id],
            map_run_tool_call,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Legacy consume-once claim. New journal replays use
    /// [`Store::find_completed_tool_result`] and record a successor completed
    /// row instead of consuming the source row, so a post-replay failure can
    /// replay again rather than re-executing the write. The claim path remains
    /// for older daemon data and focused tests.
    pub fn claim_completed_tool_result(
        &self,
        run_id: &str,
        tool_name: &str,
        input_fingerprint: &str,
        new_run_id: &str,
    ) -> Result<Option<RunToolCall>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let candidate: Option<i64> = tx
            .query_row(
                "SELECT tc.rowid
                 FROM run_tool_calls tc
                 JOIN session_runs r ON r.run_id = tc.run_id
                 WHERE COALESCE(NULLIF(r.resume_root_id, ''), r.run_id) = (
                     SELECT COALESCE(NULLIF(resume_root_id, ''), run_id)
                     FROM session_runs WHERE run_id = ?1
                 )
                   AND tc.run_id <> ?4
                   AND tc.status = 'completed'
                   AND tc.tool_name = ?2
                   AND tc.input_fingerprint = ?3
                   AND tc.consumed_by_run_id IS NULL
                 ORDER BY tc.created_at ASC, tc.rowid ASC
                 LIMIT 1",
                params![run_id, tool_name, input_fingerprint, new_run_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(rowid) = candidate else {
            return Ok(None);
        };
        let changed = tx.execute(
            "UPDATE run_tool_calls
             SET consumed_by_run_id = ?2, consumed_at = ?3, updated_at = ?3
             WHERE rowid = ?1 AND consumed_by_run_id IS NULL",
            params![rowid, new_run_id, now],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let tool = tx.query_row(
            "SELECT run_id, session_id, step_id, tool_call_id, tool_name, input_fingerprint,
                    status, result_json, error, consumed_by_run_id, consumed_at, created_at,
                    updated_at
             FROM run_tool_calls WHERE rowid = ?1",
            params![rowid],
            map_run_tool_call,
        )?;
        tx.commit()?;
        Ok(Some(tool))
    }

    /// Replace event-backed run/step/checkpoint projections from replay.
    /// Operational tool-call rows are preserved; a run that still has tool
    /// rows is never dropped just because it is absent from the expected set.
    pub fn replace_run_journal_projections(
        &self,
        session_id: &str,
        runs: &[SessionRun],
        steps: &[RunStep],
        checkpoints: &[RunCheckpoint],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        replace_run_journal_projections_tx(&tx, session_id, runs, steps, checkpoints)?;
        tx.commit()?;
        Ok(())
    }
}

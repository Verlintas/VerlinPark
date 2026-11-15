//! Context checkpoints, checkpoint retention, and progress rounds.

use super::*;

impl Store {
    pub fn get_checkpoint_policy(&self, project_id: &str) -> Result<CheckpointRetentionPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT enabled, older_than_secs, keep_latest
                 FROM project_checkpoint_policies WHERE project_id = ?1",
                params![project_id],
                |row| {
                    Ok(CheckpointRetentionPolicy {
                        enabled: row.get::<_, i64>(0)? != 0,
                        older_than_secs: row.get::<_, i64>(1)? as u64,
                        keep_latest: row.get::<_, i64>(2)? as u32,
                    })
                },
            )
            .optional()?;
        Ok(stored.unwrap_or_default())
    }

    pub fn set_checkpoint_policy(
        &self,
        project_id: &str,
        policy: &CheckpointRetentionPolicy,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_checkpoint_policies
             (project_id, enabled, older_than_secs, keep_latest)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(project_id) DO UPDATE SET
                enabled = excluded.enabled,
                older_than_secs = excluded.older_than_secs,
                keep_latest = excluded.keep_latest",
            params![
                project_id,
                i64::from(policy.enabled),
                policy.older_than_secs as i64,
                policy.keep_latest as i64
            ],
        )?;
        Ok(())
    }

    pub fn list_enabled_checkpoint_policies(
        &self,
    ) -> Result<Vec<(String, CheckpointRetentionPolicy)>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT project_id, enabled, older_than_secs, keep_latest
             FROM project_checkpoint_policies WHERE enabled = 1",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                CheckpointRetentionPolicy {
                    enabled: row.get::<_, i64>(1)? != 0,
                    older_than_secs: row.get::<_, i64>(2)? as u64,
                    keep_latest: row.get::<_, i64>(3)? as u32,
                },
            ))
        })?;
        let mut policies = Vec::new();
        for row in rows {
            policies.push(row?);
        }
        Ok(policies)
    }

    pub fn get_context_checkpoint_policy(
        &self,
        project_id: &str,
    ) -> Result<ContextCheckpointPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT enabled, max_checkpoints, max_age_days
                 FROM project_context_checkpoint_policies WHERE project_id = ?1",
                params![project_id],
                |row| {
                    Ok(ContextCheckpointPolicy {
                        enabled: row.get::<_, i64>(0)? != 0,
                        max_checkpoints: row.get::<_, i64>(1)?.clamp(1, 100) as u32,
                        max_age_days: row.get::<_, i64>(2)?.clamp(0, 365) as u32,
                    })
                },
            )
            .optional()?;
        Ok(stored.unwrap_or_default())
    }

    pub fn set_context_checkpoint_policy(
        &self,
        project_id: &str,
        policy: &ContextCheckpointPolicy,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_context_checkpoint_policies
             (project_id, enabled, max_checkpoints, max_age_days)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(project_id) DO UPDATE SET
                enabled = excluded.enabled,
                max_checkpoints = excluded.max_checkpoints,
                max_age_days = excluded.max_age_days",
            params![
                project_id,
                i64::from(policy.enabled),
                i64::from(policy.max_checkpoints),
                i64::from(policy.max_age_days)
            ],
        )?;
        Ok(())
    }

    pub fn list_enabled_context_checkpoint_policies(
        &self,
    ) -> Result<Vec<(String, ContextCheckpointPolicy)>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT project_id, enabled, max_checkpoints, max_age_days
             FROM project_context_checkpoint_policies WHERE enabled = 1",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                ContextCheckpointPolicy {
                    enabled: row.get::<_, i64>(1)? != 0,
                    max_checkpoints: row.get::<_, i64>(2)?.clamp(1, 100) as u32,
                    max_age_days: row.get::<_, i64>(3)?.clamp(0, 365) as u32,
                },
            ))
        })?;
        let mut policies = Vec::new();
        for row in rows {
            policies.push(row?);
        }
        Ok(policies)
    }

    /// Record a manual checkpoint row and its full-payload event in one
    /// transaction. The event is inserted before the projection row so a
    /// crash between the two statements cannot leave a projection with no
    /// audit event.
    pub fn record_context_checkpoint_with_event(
        &self,
        session_id: &str,
        checkpoint: &ContextCheckpoint,
    ) -> Result<Event> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let mut checkpoint = checkpoint.clone();
        if checkpoint.session_id != session_id {
            return Err(anyhow!(
                "context checkpoint belongs to a different session: {}",
                checkpoint.session_id
            ));
        }
        checkpoint.message_cursor = context_message_cursor_tx(
            &tx,
            session_id,
            checkpoint.through_message_id.as_deref(),
            checkpoint.message_cursor,
        )?;
        validate_context_checkpoint_row(&checkpoint)?;
        let payload = serde_json::to_value(&checkpoint)?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "context.checkpointed",
            payload_with_checkpoint_id(payload, &checkpoint.id)?,
        )?;
        insert_context_checkpoint_tx(&tx, &checkpoint)?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(event)
    }

    /// Append a successful compaction and its automatic checkpoint in one
    /// transaction. The compaction event is first (it is the compatibility
    /// boundary used by `load_conversation`), then the full checkpoint event,
    /// then the projection row.
    pub fn append_context_compaction_with_checkpoint(
        &self,
        session_id: &str,
        mut compaction_payload: serde_json::Value,
        checkpoint: &ContextCheckpoint,
    ) -> Result<(Event, Event)> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let mut checkpoint = checkpoint.clone();
        if checkpoint.session_id != session_id {
            return Err(anyhow!(
                "context checkpoint belongs to a different session: {}",
                checkpoint.session_id
            ));
        }
        if let Some(object) = compaction_payload.as_object_mut() {
            object.insert(
                "context_checkpoint_id".into(),
                serde_json::json!(checkpoint.id),
            );
        }
        let compaction = insert_event_tx(&tx, session_id, "context.compacted", compaction_payload)?;
        if checkpoint.source_compaction_seq.is_none() {
            checkpoint.source_compaction_seq = Some(compaction.seq);
        }
        checkpoint.message_cursor = context_message_cursor_tx(
            &tx,
            session_id,
            checkpoint.through_message_id.as_deref(),
            checkpoint.message_cursor,
        )?;
        validate_context_checkpoint_row(&checkpoint)?;
        let payload = serde_json::to_value(&checkpoint)?;
        let checkpoint_event = insert_event_tx(
            &tx,
            session_id,
            "context.checkpointed",
            payload_with_checkpoint_id(payload, &checkpoint.id)?,
        )?;
        insert_context_checkpoint_tx(&tx, &checkpoint)?;
        tx.commit()?;
        for event in [&compaction, &checkpoint_event] {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok((compaction, checkpoint_event))
    }

    /// Append a reconstructed compaction boundary and the restore audit event.
    /// No checkpoint row, message row, or worktree state is modified.
    pub fn append_context_restore_events(
        &self,
        session_id: &str,
        compaction_payload: serde_json::Value,
        mut restored_payload: serde_json::Value,
    ) -> Result<(Event, Event)> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let compaction = insert_event_tx(&tx, session_id, "context.compacted", compaction_payload)?;
        if let Some(object) = restored_payload.as_object_mut() {
            object.insert("compaction_seq".into(), serde_json::json!(compaction.seq));
        }
        let restored = insert_event_tx(&tx, session_id, "context.restored", restored_payload)?;
        tx.commit()?;
        for event in [&compaction, &restored] {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok((compaction, restored))
    }

    pub fn list_context_checkpoints(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<Vec<ContextCheckpoint>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT id, session_id, project_id, label, mode, summary,
                    through_message_id, message_cursor, plan_revision, provider,
                    model, prompt_prefix_hash, worktree_head, tokens_before,
                    tokens_after, structured_state, source_compaction_seq,
                    automatic, created_at
             FROM context_checkpoints
             WHERE session_id = ?1
             ORDER BY julianday(created_at) DESC, id DESC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![session_id, i64::from(limit.clamp(1, 10_000))],
            map_context_checkpoint,
        )?;
        // The limit means "most recent N"; callers receive them oldest-first
        // so prune, replay comparison, and the UI all share one ordering.
        let mut rows = rows.collect::<Result<Vec<_>, _>>()?;
        rows.reverse();
        Ok(rows)
    }

    pub fn get_context_checkpoint(
        &self,
        session_id: &str,
        checkpoint_id: &str,
    ) -> Result<Option<ContextCheckpoint>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, session_id, project_id, label, mode, summary,
                    through_message_id, message_cursor, plan_revision, provider,
                    model, prompt_prefix_hash, worktree_head, tokens_before,
                    tokens_after, structured_state, source_compaction_seq,
                    automatic, created_at
             FROM context_checkpoints
             WHERE session_id = ?1 AND id = ?2",
            params![session_id, checkpoint_id],
            map_context_checkpoint,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn context_checkpoint_count(&self, session_id: &str) -> Result<u64> {
        let conn = self.lock()?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM context_checkpoints WHERE session_id = ?1",
            params![session_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count.max(0) as u64)
    }

    pub fn latest_context_checkpoint(&self, session_id: &str) -> Result<Option<ContextCheckpoint>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, session_id, project_id, label, mode, summary,
                    through_message_id, message_cursor, plan_revision, provider,
                    model, prompt_prefix_hash, worktree_head, tokens_before,
                    tokens_after, structured_state, source_compaction_seq,
                    automatic, created_at
             FROM context_checkpoints
             WHERE session_id = ?1
             ORDER BY created_at DESC, id DESC LIMIT 1",
            params![session_id],
            map_context_checkpoint,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Remove one checkpoint row and append its deletion event atomically.
    /// Messages and worktree state are untouched.
    pub fn delete_context_checkpoint_with_event(
        &self,
        session_id: &str,
        checkpoint_id: &str,
        reason: &str,
    ) -> Result<Option<Event>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let existing = tx
            .query_row(
                "SELECT id, session_id, project_id, label, mode, summary,
                        through_message_id, message_cursor, plan_revision, provider,
                        model, prompt_prefix_hash, worktree_head, tokens_before,
                        tokens_after, structured_state, source_compaction_seq,
                        automatic, created_at
                 FROM context_checkpoints WHERE session_id = ?1 AND id = ?2",
                params![session_id, checkpoint_id],
                map_context_checkpoint,
            )
            .optional()?;
        let Some(checkpoint) = existing else {
            return Ok(None);
        };
        let event = insert_event_tx(
            &tx,
            session_id,
            "context.checkpoint.deleted",
            serde_json::json!({
                "checkpoint_id": checkpoint_id,
                "label": checkpoint.label,
                "reason": reason,
                "checkpoint": checkpoint,
            }),
        )?;
        tx.execute(
            "DELETE FROM context_checkpoints WHERE session_id = ?1 AND id = ?2",
            params![session_id, checkpoint_id],
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some(event))
    }

    /// Replace the event-backed checkpoint projection during replay/repair.
    pub fn replace_context_checkpoints(
        &self,
        session_id: &str,
        checkpoints: &[ContextCheckpoint],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        replace_context_checkpoints_tx(&tx, session_id, checkpoints)?;
        tx.commit()?;
        Ok(())
    }

    pub fn insert_progress_round_with_event(
        &self,
        session_id: &str,
        plan_revision: u64,
        round: &ProgressRound,
    ) -> Result<(ProgressRound, Event)> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO progress_rounds
             (session_id, plan_revision, round_id, agent_id, agent_name, task_id,
              satisfied_json, in_loop_json, progressing_json, blockers_json,
              next_task, instruction, evidence_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                session_id,
                plan_revision as i64,
                round.round_id,
                round.agent_id,
                round.agent_name,
                round.task_id,
                serde_json::to_string(&round.satisfied)?,
                serde_json::to_string(&round.in_loop)?,
                serde_json::to_string(&round.progressing)?,
                serde_json::to_string(&round.blockers)?,
                round.next_task,
                round.instruction,
                serde_json::to_string(&round.evidence)?,
                round.created_at.clone().unwrap_or_else(now_rfc3339),
            ],
        )?;
        let stored =
            self.progress_round_by_key_tx(&tx, session_id, plan_revision, &round.round_id)?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "progress.round",
            serde_json::json!({
                "plan_revision": plan_revision,
                "round": stored,
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append progress round event mirror");
        }
        Ok((stored, event))
    }

    pub fn list_progress_rounds(
        &self,
        session_id: &str,
        plan_revision: Option<u64>,
        limit: u32,
    ) -> Result<(Vec<ProgressRound>, bool)> {
        let limit = limit.clamp(1, MAX_PROGRESS_ROUNDS) as i64;
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT round_id, agent_id, agent_name, task_id,
                    satisfied_json, in_loop_json, progressing_json, blockers_json,
                    next_task, instruction, evidence_json, created_at
             FROM progress_rounds
             WHERE session_id = ?1 AND (?2 IS NULL OR plan_revision = ?2)
             ORDER BY seq DESC
             LIMIT ?3",
        )?;
        let mut rows = stmt
            .query_map(
                params![
                    session_id,
                    plan_revision.map(|value| value as i64),
                    limit + 1
                ],
                map_progress_round,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let truncated = rows.len() as i64 > limit;
        rows.truncate(limit as usize);
        rows.reverse();
        Ok((rows, truncated))
    }

    pub fn list_progress_rows(&self, session_id: &str) -> Result<Vec<(u64, ProgressRound)>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT plan_revision, round_id, agent_id, agent_name, task_id,
                    satisfied_json, in_loop_json, progressing_json, blockers_json,
                    next_task, instruction, evidence_json, created_at
             FROM progress_rounds
             WHERE session_id = ?1
             ORDER BY seq ASC",
        )?;
        stmt.query_map(params![session_id], map_progress_row)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn latest_progress_revision(&self, session_id: &str) -> Result<Option<u64>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT plan_revision FROM progress_rounds
             WHERE session_id = ?1 ORDER BY seq DESC LIMIT 1",
            params![session_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map(|value| value.map(|value| value.max(0) as u64))
        .map_err(Into::into)
    }

    pub fn replace_progress_rounds(
        &self,
        session_id: &str,
        rounds: &[(u64, ProgressRound)],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM progress_rounds WHERE session_id = ?1",
            params![session_id],
        )?;
        for (plan_revision, round) in rounds {
            tx.execute(
                "INSERT INTO progress_rounds
                 (session_id, plan_revision, round_id, agent_id, agent_name, task_id,
                  satisfied_json, in_loop_json, progressing_json, blockers_json,
                  next_task, instruction, evidence_json, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    session_id,
                    *plan_revision as i64,
                    round.round_id,
                    round.agent_id,
                    round.agent_name,
                    round.task_id,
                    serde_json::to_string(&round.satisfied)?,
                    serde_json::to_string(&round.in_loop)?,
                    serde_json::to_string(&round.progressing)?,
                    serde_json::to_string(&round.blockers)?,
                    round.next_task,
                    round.instruction,
                    serde_json::to_string(&round.evidence)?,
                    round.created_at.clone().unwrap_or_else(now_rfc3339),
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub(super) fn progress_round_by_key_tx(
        &self,
        tx: &Transaction<'_>,
        session_id: &str,
        plan_revision: u64,
        round_id: &str,
    ) -> Result<ProgressRound> {
        tx.query_row(
            "SELECT round_id, agent_id, agent_name, task_id,
                    satisfied_json, in_loop_json, progressing_json, blockers_json,
                    next_task, instruction, evidence_json, created_at
             FROM progress_rounds
             WHERE session_id = ?1 AND plan_revision = ?2 AND round_id = ?3",
            params![session_id, plan_revision as i64, round_id],
            map_progress_round,
        )
        .map_err(Into::into)
    }

    pub fn list_questions(&self, session_id: Option<&str>) -> Result<Vec<Question>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, session_id, agent_id, agent_name, question, options, status,
                    answer, created_at, answered_at
             FROM questions
             WHERE (?1 IS NULL OR session_id = ?1)
             ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(params![session_id], map_question)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn list_session_questions(&self, session_id: &str) -> Result<Vec<Question>> {
        self.list_questions(Some(session_id))
    }

    pub fn replace_session_questions(
        &self,
        session_id: &str,
        questions: &[Question],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM questions WHERE session_id = ?1",
            params![session_id],
        )?;
        for question in questions {
            tx.execute(
                "INSERT INTO questions
                 (id, session_id, agent_id, agent_name, question, options, status, answer,
                  created_at, answered_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    question.id,
                    question.session_id,
                    question.agent_id,
                    question.agent_name,
                    question.question,
                    serde_json::to_string(&question.options)?,
                    question.status,
                    question.answer,
                    question.created_at,
                    question.answered_at
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}

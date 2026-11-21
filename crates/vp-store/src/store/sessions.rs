//! Sessions, questions, session operations, steering, and continuations.

use super::*;

/// Maximum admitted session goal size. The goal is rendered inside the 64 KiB
/// `session/context` instruction section together with fixed metadata; a goal
/// at or above that section cap would make every run of the session fail
/// closed with no edit surface. Reject it at creation instead.
pub const MAX_SESSION_GOAL_BYTES: usize = 32 * 1024;
/// Maximum admitted user prompt size. Prompts are persisted both as a message
/// and in the `agent.queued` payload; an unbounded prompt grows the event log
/// and can crowd out the conversation budget.
pub const MAX_SESSION_PROMPT_BYTES: usize = 64 * 1024;

/// Stable local validation failure for session input bounds. It carries a
/// code so callers/tests can distinguish admission rejection from an
/// infrastructure error without parsing prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionValidationError {
    pub code: &'static str,
    pub message: String,
}

impl SessionValidationError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for SessionValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for SessionValidationError {}

fn validate_goal_size(goal: &str) -> Result<()> {
    if goal.len() > MAX_SESSION_GOAL_BYTES {
        return Err(SessionValidationError::new(
            "INVALID_ARGUMENT",
            format!(
                "session goal is {} bytes; the limit is {} bytes",
                goal.len(),
                MAX_SESSION_GOAL_BYTES
            ),
        )
        .into());
    }
    Ok(())
}

fn validate_user_prompt_size(role: &str, content: &str) -> Result<()> {
    if role == "user" && content.len() > MAX_SESSION_PROMPT_BYTES {
        return Err(SessionValidationError::new(
            "INVALID_ARGUMENT",
            format!(
                "session prompt is {} bytes; the limit is {} bytes",
                content.len(),
                MAX_SESSION_PROMPT_BYTES
            ),
        )
        .into());
    }
    Ok(())
}

impl Store {
    pub fn create_session(&self, project_id: &str, goal: &str) -> Result<Session> {
        self.create_session_with_limits(project_id, goal, None, None, None)
    }

    pub fn create_session_with_limits(
        &self,
        project_id: &str,
        goal: &str,
        token_budget: Option<u64>,
        cost_budget_usd: Option<f64>,
        time_budget_secs: Option<u64>,
    ) -> Result<Session> {
        self.create_session_with_limits_and_priority(
            project_id,
            goal,
            token_budget,
            cost_budget_usd,
            time_budget_secs,
            0,
        )
    }

    pub fn create_session_with_limits_and_priority(
        &self,
        project_id: &str,
        goal: &str,
        token_budget: Option<u64>,
        cost_budget_usd: Option<f64>,
        time_budget_secs: Option<u64>,
        priority: i32,
    ) -> Result<Session> {
        self.insert_session(
            Uuid::now_v7().to_string(),
            project_id,
            goal,
            None,
            None,
            token_budget,
            cost_budget_usd,
            time_budget_secs,
            false,
            None,
            priority,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_session_with_worktree(
        &self,
        id: &str,
        project_id: &str,
        goal: &str,
        worktree: &str,
        branch: &str,
        token_budget: Option<u64>,
        cost_budget_usd: Option<f64>,
        time_budget_secs: Option<u64>,
        unlimited: bool,
        base_commit: &str,
        priority: i32,
    ) -> Result<Session> {
        self.insert_session(
            id.to_string(),
            project_id,
            goal,
            Some(worktree.to_string()),
            Some(branch.to_string()),
            token_budget,
            cost_budget_usd,
            time_budget_secs,
            unlimited,
            Some(base_commit.to_string()),
            priority,
        )
    }

    /// Create a session row and its `session.created` event in one
    /// transaction. This closes the crash window where a session exists but
    /// `db-rebuild` (which reads the event mirror) skips it.
    #[allow(clippy::too_many_arguments)]
    pub fn create_session_with_worktree_and_event(
        &self,
        id: &str,
        project_id: &str,
        goal: &str,
        worktree: &str,
        branch: &str,
        token_budget: Option<u64>,
        cost_budget_usd: Option<f64>,
        time_budget_secs: Option<u64>,
        unlimited: bool,
        base_commit: &str,
        priority: i32,
    ) -> Result<(Session, Event)> {
        self.create_session_with_worktree_and_event_payload(
            id,
            project_id,
            goal,
            worktree,
            branch,
            token_budget,
            cost_budget_usd,
            time_budget_secs,
            unlimited,
            base_commit,
            priority,
            None,
        )
    }

    /// Same atomic session+`session.created` transaction, with optional extra
    /// top-level event fields. Fork lineage (`forked_from_*`) belongs in the
    /// event, so the session row and its replay authority commit together.
    #[allow(clippy::too_many_arguments)]
    pub fn create_session_with_worktree_and_event_payload(
        &self,
        id: &str,
        project_id: &str,
        goal: &str,
        worktree: &str,
        branch: &str,
        token_budget: Option<u64>,
        cost_budget_usd: Option<f64>,
        time_budget_secs: Option<u64>,
        unlimited: bool,
        base_commit: &str,
        priority: i32,
        extra_payload: Option<serde_json::Value>,
    ) -> Result<(Session, Event)> {
        let budget_mode = if unlimited {
            BudgetMode::Unlimited
        } else {
            BudgetMode::Fixed
        };
        self.create_session_with_worktree_and_event_budget(
            id,
            project_id,
            goal,
            worktree,
            branch,
            token_budget,
            cost_budget_usd,
            time_budget_secs,
            unlimited,
            base_commit,
            priority,
            budget_mode,
            None,
            extra_payload,
        )
    }

    /// Atomic session creation with an optional auto-budget row. The session
    /// row, `session.created` event, and `session_auto_budgets` row commit in
    /// one transaction, so a crash cannot leave an auto session without its
    /// hard caps or an orphan policy row.
    #[allow(clippy::too_many_arguments)]
    pub fn create_session_with_worktree_and_event_budget(
        &self,
        id: &str,
        project_id: &str,
        goal: &str,
        worktree: &str,
        branch: &str,
        token_budget: Option<u64>,
        cost_budget_usd: Option<f64>,
        time_budget_secs: Option<u64>,
        unlimited: bool,
        base_commit: &str,
        priority: i32,
        budget_mode: BudgetMode,
        auto_budget: Option<&SessionAutoBudgetRecord>,
        extra_payload: Option<serde_json::Value>,
    ) -> Result<(Session, Event)> {
        if auto_budget.is_some_and(|record| record.session_id != id) {
            return Err(anyhow!(
                "session auto-budget row id does not match the session id"
            ));
        }
        if auto_budget.is_some() && budget_mode != BudgetMode::Auto {
            return Err(anyhow!(
                "a session auto-budget row requires budget mode auto"
            ));
        }
        let session = self.build_session(
            id.to_string(),
            project_id,
            goal,
            Some(worktree.to_string()),
            Some(branch.to_string()),
            token_budget,
            cost_budget_usd,
            time_budget_secs,
            unlimited,
            Some(base_commit.to_string()),
            priority,
        )?;
        let mut payload = session_created_payload(&session, budget_mode);
        if let Some(record) = auto_budget
            && let Some(object) = payload.as_object_mut()
        {
            object.insert("auto_budget".to_string(), serde_json::to_value(record)?);
        }
        if let Some(extra) = extra_payload {
            match (payload.as_object_mut(), extra) {
                (Some(payload), serde_json::Value::Object(extra)) => {
                    for (key, value) in extra {
                        payload.insert(key, value);
                    }
                }
                (Some(_), _) | (None, _) => {
                    return Err(anyhow!("session.created payload must be an object"));
                }
            }
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        insert_session_row(&tx, &session)?;
        if let Some(record) = auto_budget {
            crate::store::auto_budget::insert_session_auto_budget_tx(&tx, record)?;
        }
        let event = insert_event_tx(&tx, &session.id, "session.created", payload)?;
        tx.commit()?;
        drop(conn);
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((session, event))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn insert_session(
        &self,
        id: String,
        project_id: &str,
        goal: &str,
        worktree: Option<String>,
        branch: Option<String>,
        token_budget: Option<u64>,
        cost_budget_usd: Option<f64>,
        time_budget_secs: Option<u64>,
        unlimited: bool,
        base_commit: Option<String>,
        priority: i32,
    ) -> Result<Session> {
        let session = self.build_session(
            id,
            project_id,
            goal,
            worktree,
            branch,
            token_budget,
            cost_budget_usd,
            time_budget_secs,
            unlimited,
            base_commit,
            priority,
        )?;
        let conn = self.lock()?;
        insert_session_row(&conn, &session)?;
        Ok(session)
    }

    #[allow(clippy::too_many_arguments)]
    fn build_session(
        &self,
        id: String,
        project_id: &str,
        goal: &str,
        worktree: Option<String>,
        branch: Option<String>,
        token_budget: Option<u64>,
        cost_budget_usd: Option<f64>,
        time_budget_secs: Option<u64>,
        unlimited: bool,
        base_commit: Option<String>,
        priority: i32,
    ) -> Result<Session> {
        if self.get_project(project_id)?.is_none() {
            return Err(anyhow!("project not found: {project_id}"));
        }
        let goal = goal.trim();
        if goal.is_empty() {
            return Err(anyhow!("session goal must not be empty"));
        }
        validate_goal_size(goal)?;
        // New sessions inherit the project layout default. Existing rows keep
        // the `2` default added by the schema migration, so upgrading the
        // daemon never rewrites their prompt bytes.
        let prompt_layout = self
            .get_cache_policy(project_id)
            .map(|policy| policy.session_prompt_layout())
            .unwrap_or(3);
        Ok(Session {
            id,
            project_id: project_id.to_string(),
            goal: goal.to_string(),
            status: "draft".to_string(),
            priority,
            worktree,
            branch,
            token_budget,
            cost_budget_usd,
            time_budget_secs,
            unlimited,
            cost_spent_usd: 0.0,
            base_commit,
            tokens_in: 0,
            tokens_out: 0,
            prompt_layout,
            created_at: now_rfc3339(),
        })
    }

    /// Persist a session-scoped prompt-layout override. The caller emits the
    /// `session.prompt_layout` event; this method only updates the projection.
    pub fn set_session_prompt_layout(&self, session_id: &str, prompt_layout: u32) -> Result<()> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE sessions SET prompt_layout = ?1 WHERE id = ?2",
            params![prompt_layout as i64, session_id],
        )?;
        if changed == 0 {
            return Err(anyhow!("session not found: {session_id}"));
        }
        Ok(())
    }

    pub fn set_session_prompt_layout_with_event(
        &self,
        session_id: &str,
        prompt_layout: u32,
    ) -> Result<Event> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE sessions SET prompt_layout = ?1 WHERE id = ?2",
            params![prompt_layout as i64, session_id],
        )?;
        if changed == 0 {
            return Err(anyhow!("session not found: {session_id}"));
        }
        let event = insert_event_tx(
            &tx,
            session_id,
            "session.prompt_layout",
            serde_json::json!({"prompt_layout": prompt_layout}),
        )?;
        tx.commit()?;
        drop(conn);
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(event)
    }

    pub fn set_session_priority(&self, session_id: &str, priority: i32) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE sessions SET priority = ?1 WHERE id = ?2",
            params![priority, session_id],
        )?;
        Ok(())
    }

    pub fn set_session_priority_with_event(
        &self,
        session_id: &str,
        priority: i32,
    ) -> Result<Event> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE sessions SET priority = ?1 WHERE id = ?2",
            params![priority, session_id],
        )?;
        if changed == 0 {
            return Err(anyhow!("session not found: {session_id}"));
        }
        let event = insert_event_tx(
            &tx,
            session_id,
            "session.priority",
            serde_json::json!({"priority": priority}),
        )?;
        tx.commit()?;
        drop(conn);
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(event)
    }

    pub fn set_session_unlimited_with_event(
        &self,
        session_id: &str,
        unlimited: bool,
    ) -> Result<Event> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE sessions SET unlimited = ?1 WHERE id = ?2",
            params![if unlimited { 1 } else { 0 }, session_id],
        )?;
        if changed == 0 {
            return Err(anyhow!("session not found: {session_id}"));
        }
        if !unlimited {
            // The compatibility flag restores the stored mode. An explicit
            // `mode=unlimited` row has no underlying mode to restore, so it
            // falls back to fixed; an auto row keeps its auto mode and can be
            // resumed under the same caps.
            tx.execute(
                "UPDATE session_auto_budgets SET mode = 'fixed', updated_at = ?1
                 WHERE session_id = ?2 AND mode = 'unlimited'",
                params![now_rfc3339(), session_id],
            )?;
        }
        let event = insert_event_tx(
            &tx,
            session_id,
            "session.budget_mode_changed",
            serde_json::json!({
                "unlimited": unlimited,
                "budget_mode": if unlimited { "unlimited" } else { "limited" },
            }),
        )?;
        tx.commit()?;
        drop(conn);
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(event)
    }

    pub fn set_session_worktree(
        &self,
        session_id: &str,
        worktree: &str,
        branch: &str,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE sessions SET worktree = ?1, branch = ?2 WHERE id = ?3",
            params![worktree, branch, session_id],
        )?;
        Ok(())
    }

    pub fn create_question_with_event(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        question: &str,
        options: &[String],
    ) -> Result<(Question, Event)> {
        let item = Question {
            id: Uuid::now_v7().to_string(),
            session_id: session_id.to_string(),
            agent_id: agent_id.to_string(),
            agent_name: agent_name.to_string(),
            question: question.to_string(),
            options: options.to_vec(),
            status: "pending".to_string(),
            answer: None,
            created_at: now_rfc3339(),
            answered_at: None,
        };
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO questions
             (id, session_id, agent_id, agent_name, question, options, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7)",
            params![
                item.id,
                item.session_id,
                item.agent_id,
                item.agent_name,
                item.question,
                serde_json::to_string(&item.options)?,
                item.created_at
            ],
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "question.requested",
            serde_json::to_value(&item)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((item, event))
    }

    pub fn answer_question_with_event(
        &self,
        question_id: &str,
        answer: &str,
    ) -> Result<Option<(Question, Event)>> {
        let answered_at = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE questions SET status = 'answered', answer = ?1, answered_at = ?2
             WHERE id = ?3 AND status = 'pending'",
            params![answer, answered_at, question_id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let mut question = tx.query_row(
            "SELECT id, session_id, agent_id, agent_name, question, options, status,
                    answer, created_at, answered_at FROM questions WHERE id = ?1",
            params![question_id],
            map_question,
        )?;
        let event = insert_event_tx(
            &tx,
            &question.session_id,
            "question.answered",
            serde_json::to_value(&question)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        question.status = "answered".to_string();
        Ok(Some((question, event)))
    }

    pub fn close_question_with_event(
        &self,
        question_id: &str,
        status: &str,
    ) -> Result<Option<(Question, Event)>> {
        if !matches!(status, "cancelled" | "expired") {
            return Err(anyhow!("question status must be cancelled or expired"));
        }
        let answered_at = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE questions SET status = ?1, answered_at = ?2
             WHERE id = ?3 AND status = 'pending'",
            params![status, answered_at, question_id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let question = tx.query_row(
            "SELECT id, session_id, agent_id, agent_name, question, options, status,
                    answer, created_at, answered_at FROM questions WHERE id = ?1",
            params![question_id],
            map_question,
        )?;
        let event = insert_event_tx(
            &tx,
            &question.session_id,
            if status == "cancelled" {
                "question.cancelled"
            } else {
                "question.expired"
            },
            serde_json::to_value(&question)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((question, event)))
    }

    pub fn list_sessions(&self, project_id: Option<&str>) -> Result<Vec<Session>> {
        let conn = self.lock()?;
        let mut sessions = Vec::new();
        match project_id {
            Some(project_id) => {
                let mut stmt = conn.prepare(
                    "SELECT id, project_id, goal, status, worktree, branch, token_budget, cost_budget_usd, time_budget_secs, unlimited, cost_spent_usd, base_commit, tokens_in, tokens_out, created_at, priority, prompt_layout
                     FROM sessions WHERE project_id = ?1
                     ORDER BY created_at DESC",
                )?;
                let rows = stmt.query_map(params![project_id], map_session)?;
                sessions.extend(rows.collect::<Result<Vec<_>, _>>()?);
            }
            None => {
                let mut stmt = conn.prepare(
                    "SELECT id, project_id, goal, status, worktree, branch, token_budget, cost_budget_usd, time_budget_secs, unlimited, cost_spent_usd, base_commit, tokens_in, tokens_out, created_at, priority, prompt_layout
                     FROM sessions ORDER BY created_at DESC",
                )?;
                let rows = stmt.query_map([], map_session)?;
                sessions.extend(rows.collect::<Result<Vec<_>, _>>()?);
            }
        }
        Ok(sessions)
    }

    pub fn get_session(&self, id: &str) -> Result<Option<Session>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, project_id, goal, status, worktree, branch, token_budget, cost_budget_usd, time_budget_secs, unlimited, cost_spent_usd, base_commit, tokens_in, tokens_out, created_at, priority, prompt_layout
             FROM sessions WHERE id = ?1",
            params![id],
            map_session,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn update_session_status(&self, session_id: &str, status: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE sessions SET status = ?1 WHERE id = ?2",
            params![status, session_id],
        )?;
        Ok(())
    }

    pub fn rehydrate_session_with_events(
        &self,
        session_id: &str,
        worktree: &str,
        branch: &str,
    ) -> Result<Vec<Event>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE sessions
             SET worktree = ?1, branch = ?2, status = 'idle'
             WHERE id = ?3 AND status = 'imported'",
            params![worktree, branch, session_id],
        )?;
        if changed == 0 {
            return Err(anyhow!(
                "session is not an imported archive or does not exist: {session_id}"
            ));
        }
        let status_event = insert_event_tx(
            &tx,
            session_id,
            "session.updated",
            serde_json::json!({"status": "idle"}),
        )?;
        let rehydrated = insert_event_tx(
            &tx,
            session_id,
            "session.rehydrated",
            serde_json::json!({"worktree": worktree, "branch": branch}),
        )?;
        tx.commit()?;
        let events = vec![status_event, rehydrated];
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok(events)
    }

    pub fn update_session_status_with_event(
        &self,
        session_id: &str,
        status: &str,
    ) -> Result<Event> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let current: String = tx
            .query_row(
                "SELECT status FROM sessions WHERE id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| anyhow!("session not found: {session_id}"))?;
        if !allowed_session_transition(&current, status) {
            return Err(anyhow!(
                "invalid session status transition: {current} -> {status}"
            ));
        }
        let changed = tx.execute(
            "UPDATE sessions SET status = ?1 WHERE id = ?2",
            params![status, session_id],
        )?;
        if changed == 0 {
            return Err(anyhow!("session not found: {session_id}"));
        }
        let event = insert_event_tx(
            &tx,
            session_id,
            "session.updated",
            serde_json::json!({"status": status}),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(event)
    }

    pub fn append_message_with_event(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        tool_calls: &serde_json::Value,
        tool_call_id: Option<&str>,
    ) -> Result<(StoredMessage, Event)> {
        validate_user_prompt_size(role, content)?;
        let message = StoredMessage {
            id: Uuid::now_v7().to_string(),
            session_id: session_id.to_string(),
            role: role.to_string(),
            content: content.to_string(),
            tool_calls: tool_calls.clone(),
            tool_call_id: tool_call_id.map(ToOwned::to_owned),
            created_at: now_rfc3339(),
        };
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO messages
             (id, session_id, role, content, tool_calls, tool_call_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                message.id,
                message.session_id,
                message.role,
                message.content,
                serde_json::to_string(&message.tool_calls)?,
                message.tool_call_id,
                message.created_at
            ],
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "message.created",
            serde_json::to_value(&message)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((message, event))
    }

    pub fn append_message(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        tool_calls: &serde_json::Value,
        tool_call_id: Option<&str>,
    ) -> Result<StoredMessage> {
        validate_user_prompt_size(role, content)?;
        let message = StoredMessage {
            id: Uuid::now_v7().to_string(),
            session_id: session_id.to_string(),
            role: role.to_string(),
            content: content.to_string(),
            tool_calls: tool_calls.clone(),
            tool_call_id: tool_call_id.map(ToOwned::to_owned),
            created_at: now_rfc3339(),
        };
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO messages
             (id, session_id, role, content, tool_calls, tool_call_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                message.id,
                message.session_id,
                message.role,
                message.content,
                serde_json::to_string(&message.tool_calls)?,
                message.tool_call_id,
                message.created_at
            ],
        )?;
        Ok(message)
    }

    pub fn list_messages(&self, session_id: &str) -> Result<Vec<StoredMessage>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, session_id, role, content, tool_calls, tool_call_id, created_at
             FROM messages WHERE session_id = ?1 ORDER BY julianday(created_at) ASC, rowid ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            let tool_calls: String = row.get(4)?;
            Ok(StoredMessage {
                id: row.get(0)?,
                session_id: row.get(1)?,
                role: row.get(2)?,
                content: row.get(3)?,
                tool_calls: serde_json::from_str(&tool_calls)
                    .unwrap_or_else(|_| serde_json::json!([])),
                tool_call_id: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Atomically claim an operation receipt for one session. The comparison
    /// and insert/update happen in one immediate transaction so two callers
    /// cannot both start the same operation.
    pub fn claim_session_operation(
        &self,
        session_id: &str,
        operation_id: &str,
        method: &str,
        fingerprint: &str,
    ) -> Result<Claim> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let existing = tx
            .query_row(
                "SELECT method, fingerprint, status, result_json
                 FROM session_operations
                 WHERE session_id = ?1 AND operation_id = ?2",
                params![session_id, operation_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((existing_method, existing_fingerprint, status, result_json)) = existing else {
            tx.execute(
                "INSERT INTO session_operations
                 (session_id, operation_id, method, fingerprint, status, result_json, error,
                  created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 'pending', NULL, NULL, ?5, ?5)",
                params![session_id, operation_id, method, fingerprint, now],
            )?;
            tx.commit()?;
            return Ok(Claim::New);
        };
        if existing_method != method || existing_fingerprint != fingerprint {
            return Ok(Claim::Conflict);
        }
        match status.as_str() {
            "completed" => {
                let result = result_json.ok_or_else(|| {
                    anyhow!(
                        "session operation {operation_id} is completed but has no stored result"
                    )
                })?;
                Ok(Claim::Replay(result))
            }
            "pending" => Ok(Claim::InProgress),
            "unknown" => Ok(Claim::Unknown),
            "failed" => {
                tx.execute(
                    "UPDATE session_operations
                     SET status = 'pending', result_json = NULL, error = NULL, updated_at = ?3
                     WHERE session_id = ?1 AND operation_id = ?2 AND status = 'failed'",
                    params![session_id, operation_id, now],
                )?;
                tx.commit()?;
                Ok(Claim::New)
            }
            other => Err(anyhow!(
                "session operation {operation_id} has invalid status {other}"
            )),
        }
    }

    pub fn complete_session_operation(
        &self,
        session_id: &str,
        operation_id: &str,
        result_json: &str,
    ) -> Result<()> {
        let now = now_rfc3339();
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE session_operations
             SET status = 'completed', result_json = ?3, error = NULL, updated_at = ?4
             WHERE session_id = ?1 AND operation_id = ?2 AND status = 'pending'",
            params![session_id, operation_id, result_json, now],
        )?;
        if changed > 0 {
            return Ok(());
        }
        let status = conn
            .query_row(
                "SELECT status FROM session_operations
                 WHERE session_id = ?1 AND operation_id = ?2",
                params![session_id, operation_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        match status {
            Some(status) => Err(anyhow!(
                "session operation {operation_id} is {status}; cannot mark it completed"
            )),
            None => Err(anyhow!("session operation {operation_id} was not found")),
        }
    }

    pub fn fail_session_operation(
        &self,
        session_id: &str,
        operation_id: &str,
        error: &str,
    ) -> Result<()> {
        let now = now_rfc3339();
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE session_operations
             SET status = 'failed', result_json = NULL, error = ?3, updated_at = ?4
             WHERE session_id = ?1 AND operation_id = ?2 AND status = 'pending'",
            params![session_id, operation_id, error, now],
        )?;
        if changed > 0 {
            return Ok(());
        }
        let status = conn
            .query_row(
                "SELECT status FROM session_operations
                 WHERE session_id = ?1 AND operation_id = ?2",
                params![session_id, operation_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        match status {
            Some(status) => Err(anyhow!(
                "session operation {operation_id} is {status}; cannot mark it failed"
            )),
            None => Err(anyhow!("session operation {operation_id} was not found")),
        }
    }

    /// Startup recovery: a `pending` row means the previous daemon stopped
    /// while the operation was in flight. Its side effects may or may not
    /// have landed, so fail closed until an operator inspects the session.
    pub fn mark_pending_session_operations_unknown(&self) -> Result<usize> {
        let now = now_rfc3339();
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE session_operations
             SET status = 'unknown',
                 error = 'daemon restarted while the operation was pending; outcome unknown',
                 updated_at = ?1
             WHERE status = 'pending'",
            params![now],
        )?;
        Ok(changed)
    }

    /// Persist an operator steering request and its creation event in one
    /// transaction. The engine validates the target run before calling this.
    pub fn create_steering_with_event(
        &self,
        session_id: &str,
        agent_id: &str,
        task_id: Option<&str>,
        body: &str,
    ) -> Result<(SteeringMessage, Event)> {
        let steering = SteeringMessage {
            id: Uuid::now_v7().to_string(),
            session_id: session_id.to_string(),
            agent_id: agent_id.to_string(),
            task_id: task_id.map(str::to_string),
            body: body.to_string(),
            status: "pending".to_string(),
            created_at: now_rfc3339(),
            delivered_at: None,
        };
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let exists: bool = tx
            .query_row(
                "SELECT 1 FROM sessions WHERE id = ?1",
                params![session_id],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !exists {
            return Err(anyhow!("session not found: {session_id}"));
        }
        tx.execute(
            "INSERT INTO steering_messages
             (id, session_id, agent_id, task_id, body, status, created_at, delivered_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)",
            params![
                steering.id,
                steering.session_id,
                steering.agent_id,
                steering.task_id,
                steering.body,
                steering.status,
                steering.created_at
            ],
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "steering.created",
            serde_json::to_value(&steering)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((steering, event))
    }

    pub fn list_steering(
        &self,
        session_id: &str,
        status: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<SteeringMessage>> {
        let conn = self.lock()?;
        let limit = limit.unwrap_or(500).clamp(1, 2_000) as i64;
        let mut stmt = conn.prepare(
            "SELECT id, session_id, agent_id, task_id, body, status, created_at, delivered_at
             FROM steering_messages
             WHERE session_id = ?1 AND (?2 IS NULL OR status = ?2)
             ORDER BY created_at ASC, id ASC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![session_id, status, limit], map_steering)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn cancel_steering_with_event(&self, id: &str) -> Result<(SteeringMessage, Event)> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let steering = tx
            .query_row(
                "SELECT id, session_id, agent_id, task_id, body, status, created_at, delivered_at
                 FROM steering_messages WHERE id = ?1",
                params![id],
                map_steering,
            )
            .optional()?
            .ok_or_else(|| anyhow!("steering message not found: {id}"))?;
        if steering.status != "pending" {
            anyhow::bail!("steering message {id} is already {}", steering.status);
        }
        tx.execute(
            "UPDATE steering_messages SET status = 'cancelled' WHERE id = ?1 AND status = 'pending'",
            params![id],
        )?;
        let mut cancelled = steering;
        cancelled.status = "cancelled".to_string();
        let event = insert_event_tx(
            &tx,
            &cancelled.session_id,
            "steering.cancelled",
            serde_json::to_value(&cancelled)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((cancelled, event))
    }

    /// Cancel everything still pending for a run that is ending. This is the
    /// terminal-run cleanup path: an undelivered instruction must not leak to
    /// a later run that happens to reuse the same agent row.
    pub fn cancel_pending_steering_for_agent_with_events(
        &self,
        session_id: &str,
        agent_id: &str,
        reason: &str,
    ) -> Result<Vec<Event>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let pending = {
            let mut stmt = tx.prepare(
                "SELECT id, session_id, agent_id, task_id, body, status, created_at, delivered_at
                 FROM steering_messages
                 WHERE session_id = ?1 AND agent_id = ?2 AND status = 'pending'
                 ORDER BY created_at ASC, id ASC",
            )?;
            let rows = stmt.query_map(params![session_id, agent_id], map_steering)?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut events = Vec::new();
        for steering in pending {
            tx.execute(
                "UPDATE steering_messages SET status = 'cancelled' WHERE id = ?1 AND status = 'pending'",
                params![steering.id],
            )?;
            let mut cancelled = steering;
            cancelled.status = "cancelled".to_string();
            let mut payload = serde_json::to_value(&cancelled)?;
            if let Some(object) = payload.as_object_mut() {
                object.insert("reason".into(), serde_json::json!(reason));
            }
            events.push(insert_event_tx(
                &tx,
                &cancelled.session_id,
                "steering.cancelled",
                payload,
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

    /// Deliver every pending instruction for the current agent (and task, when
    /// scoped) in creation order. Row update, optional persisted user message,
    /// and both audit events commit together.
    pub fn deliver_pending_steering_with_events(
        &self,
        session_id: &str,
        agent_id: &str,
        task_id: Option<&str>,
        persist_messages: bool,
    ) -> Result<(Vec<DeliveredSteering>, Vec<Event>)> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let pending = {
            let mut stmt = tx.prepare(
                "SELECT id, session_id, agent_id, task_id, body, status, created_at, delivered_at
                 FROM steering_messages
                 WHERE session_id = ?1 AND agent_id = ?2 AND status = 'pending'
                   AND (task_id IS NULL OR task_id = ?3)
                 ORDER BY created_at ASC, id ASC",
            )?;
            let rows = stmt.query_map(params![session_id, agent_id, task_id], map_steering)?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut delivered = Vec::new();
        let mut events = Vec::new();
        for steering in pending {
            let delivered_at = now_rfc3339();
            let changed = tx.execute(
                "UPDATE steering_messages
                 SET status = 'delivered', delivered_at = ?1
                 WHERE id = ?2 AND status = 'pending'",
                params![delivered_at, steering.id],
            )?;
            if changed == 0 {
                continue;
            }
            let mut steering = steering;
            steering.status = "delivered".to_string();
            steering.delivered_at = Some(delivered_at);
            let content = steering_content(&steering);
            let message = if persist_messages {
                let message = StoredMessage {
                    id: Uuid::now_v7().to_string(),
                    session_id: session_id.to_string(),
                    role: "user".to_string(),
                    content: content.clone(),
                    tool_calls: serde_json::json!([]),
                    tool_call_id: None,
                    created_at: now_rfc3339(),
                };
                tx.execute(
                    "INSERT INTO messages
                     (id, session_id, role, content, tool_calls, tool_call_id, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6)",
                    params![
                        message.id,
                        message.session_id,
                        message.role,
                        message.content,
                        serde_json::to_string(&message.tool_calls)?,
                        message.created_at
                    ],
                )?;
                events.push(insert_event_tx(
                    &tx,
                    session_id,
                    "message.created",
                    serde_json::to_value(&message)?,
                )?);
                Some(message)
            } else {
                None
            };
            events.push(insert_event_tx(
                &tx,
                session_id,
                "steering.delivered",
                serde_json::to_value(&steering)?,
            )?);
            delivered.push(DeliveredSteering {
                steering,
                message,
                content,
            });
        }
        tx.commit()?;
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok((delivered, events))
    }

    /// Replace the steering projection from replayed events. Used by
    /// `session.replay {repair}` and `vp db-rebuild`.
    pub fn replace_steering_messages(
        &self,
        session_id: &str,
        messages: &[SteeringMessage],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM steering_messages WHERE session_id = ?1",
            params![session_id],
        )?;
        for message in messages {
            if message.session_id != session_id {
                return Err(anyhow!("steering message belongs to a different session"));
            }
            tx.execute(
                "INSERT INTO steering_messages
                 (id, session_id, agent_id, task_id, body, status, created_at, delivered_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    message.id,
                    message.session_id,
                    message.agent_id,
                    message.task_id,
                    message.body,
                    message.status,
                    message.created_at,
                    message.delivered_at
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Persist a deferred continuation and its creation event atomically. The
    /// engine validates body, watch ids, task states, recurrence, and the
    /// deadline before calling this. The first row of a series owns the series
    /// id; a child row is only created by a successful delivery.
    pub fn create_session_continuation_with_event(
        &self,
        new: &NewSessionContinuation,
    ) -> Result<(SessionContinuation, Event)> {
        let now = now_rfc3339();
        let id = Uuid::now_v7().to_string();
        let series_id = new.series_id.clone().unwrap_or_else(|| id.clone());
        let continuation = SessionContinuation {
            id,
            session_id: new.session_id.clone(),
            project_id: new.project_id.clone(),
            body: new.body.clone(),
            watch_task_ids: new.watch_task_ids.clone(),
            watch_mode: new.watch_mode.clone(),
            watch_gate_id: new.watch_gate_id.clone(),
            deadline_at: new.deadline_at.clone(),
            status: "pending".to_string(),
            attempts: 0,
            max_attempts: new.max_attempts.max(1),
            wake_reason: None,
            outcomes: Vec::new(),
            last_error: None,
            next_attempt_at: None,
            created_at: now.clone(),
            ready_at: None,
            delivered_at: None,
            updated_at: now,
            series_id,
            parent_continuation_id: new.parent_continuation_id.clone(),
            recurrence_every_secs: new.recurrence_every_secs,
            recurrence_max_runs: new.recurrence_max_runs.max(1),
            run_count: new.run_count,
            lease_owner: None,
            lease_expires_at: None,
            delivery_attempt: 0,
        };
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let actual_project: String = tx
            .query_row(
                "SELECT project_id FROM sessions WHERE id = ?1",
                params![continuation.session_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| anyhow!("session not found: {}", continuation.session_id))?;
        if actual_project != continuation.project_id {
            return Err(anyhow!(
                "session {} belongs to {actual_project}",
                continuation.session_id
            ));
        }
        insert_session_continuation_tx(&tx, &continuation)?;
        let event = insert_event_tx(
            &tx,
            &continuation.session_id,
            "session.continuation.created",
            serde_json::to_value(&continuation)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((continuation, event))
    }

    /// Read one continuation by id. This is an operational lookup used by the
    /// status RPC; it does not change ownership.
    pub fn get_session_continuation(&self, id: &str) -> Result<Option<SessionContinuation>> {
        let conn = self.lock()?;
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {SESSION_CONTINUATION_COLUMNS}
                     FROM session_continuations WHERE id = ?1"
                ),
                params![id],
                map_session_continuation,
            )
            .optional()?)
    }

    pub fn list_session_continuations(
        &self,
        session_id: &str,
        status: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<SessionContinuation>> {
        let conn = self.lock()?;
        let limit = limit.unwrap_or(500).clamp(1, 2_000) as i64;
        let mut stmt = conn.prepare(&format!(
            "SELECT {SESSION_CONTINUATION_COLUMNS}
             FROM session_continuations
             WHERE session_id = ?1 AND (?2 IS NULL OR status = ?2)
             ORDER BY created_at ASC, id ASC
             LIMIT ?3"
        ))?;
        let rows = stmt.query_map(params![session_id, status, limit], map_session_continuation)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// All pending and ready rows across sessions, plus delivering rows whose
    /// claim has expired. The scheduler tick evaluates this snapshot; it never
    /// sees terminal rows and never touches a live foreign delivery claim.
    pub fn list_open_session_continuations(&self) -> Result<Vec<SessionContinuation>> {
        let now = now_rfc3339();
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {SESSION_CONTINUATION_COLUMNS}
             FROM session_continuations
             WHERE status IN ('pending', 'ready')
                OR (status = 'delivering'
                    AND lease_expires_at IS NOT NULL
                    AND julianday(lease_expires_at) <= julianday(?1))
             ORDER BY created_at ASC, id ASC"
        ))?;
        let rows = stmt.query_map(params![now], map_session_continuation)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn mark_session_continuation_ready_with_event(
        &self,
        id: &str,
        reason: &str,
        outcomes: &[ContinuationOutcome],
    ) -> Result<Option<(SessionContinuation, Event)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE session_continuations
             SET status = 'ready', wake_reason = ?1, outcomes = ?2, ready_at = ?3,
                 next_attempt_at = NULL, updated_at = ?3,
                 lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL
             WHERE id = ?4 AND status = 'pending'",
            params![reason, serde_json::to_string(outcomes)?, now, id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let continuation = query_session_continuation_tx(&tx, id)?
            .ok_or_else(|| anyhow!("continuation disappeared during ready transition: {id}"))?;
        let event = insert_event_tx(
            &tx,
            &continuation.session_id,
            "session.continuation.ready",
            serde_json::to_value(&continuation)?,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((continuation, event)))
    }

    // Claim/ack/fail/retry transitions live in `store::continuation_lease`;
    // they are token-fenced and require an explicit delivery owner.

    pub fn cancel_session_continuation_with_event(
        &self,
        id: &str,
        reason: &str,
    ) -> Result<Option<(SessionContinuation, Event)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE session_continuations
             SET status = 'cancelled', last_error = ?1, next_attempt_at = NULL, updated_at = ?2,
                 lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL
             WHERE id = ?3 AND status IN ('pending', 'ready')",
            params![reason, now, id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let continuation = query_session_continuation_tx(&tx, id)?
            .ok_or_else(|| anyhow!("continuation disappeared during cancellation: {id}"))?;
        let mut payload = serde_json::to_value(&continuation)?;
        if let Some(object) = payload.as_object_mut() {
            object.insert("reason".into(), serde_json::json!(reason));
        }
        let event = insert_event_tx(
            &tx,
            &continuation.session_id,
            "session.continuation.cancelled",
            payload,
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((continuation, event)))
    }

    /// Replace the continuation projection from replayed events. Used by
    /// `session.replay {repair}` and `vp db-rebuild`.
    pub fn replace_session_continuations(
        &self,
        session_id: &str,
        continuations: &[SessionContinuation],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM session_continuations WHERE session_id = ?1",
            params![session_id],
        )?;
        for continuation in continuations {
            if continuation.session_id != session_id {
                return Err(anyhow!("continuation belongs to a different session"));
            }
            insert_session_continuation_tx(&tx, continuation)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn set_todos(&self, session_id: &str, items: &[TodoItem]) -> Result<()> {
        if self.get_session(session_id)?.is_none() {
            return Err(anyhow!("session not found: {session_id}"));
        }
        let updated_at = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM todos WHERE session_id = ?1",
            params![session_id],
        )?;
        for (position, item) in items.iter().enumerate() {
            tx.execute(
                "INSERT INTO todos (session_id, position, title, status, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    session_id,
                    position as i64,
                    item.title,
                    item.status,
                    updated_at
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn set_todos_with_event(&self, session_id: &str, items: &[TodoItem]) -> Result<Event> {
        let updated_at = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let exists: bool = tx
            .query_row(
                "SELECT 1 FROM sessions WHERE id = ?1",
                params![session_id],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !exists {
            return Err(anyhow!("session not found: {session_id}"));
        }
        tx.execute(
            "DELETE FROM todos WHERE session_id = ?1",
            params![session_id],
        )?;
        for (position, item) in items.iter().enumerate() {
            tx.execute(
                "INSERT INTO todos (session_id, position, title, status, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    session_id,
                    position as i64,
                    item.title,
                    item.status,
                    updated_at
                ],
            )?;
        }
        let event = insert_event_tx(
            &tx,
            session_id,
            "todo.updated",
            serde_json::json!({"items": items}),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(event)
    }

    pub fn list_todos(&self, session_id: &str) -> Result<Vec<TodoItem>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT title, status FROM todos WHERE session_id = ?1 ORDER BY position ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok(TodoItem {
                title: row.get(0)?,
                status: row.get(1)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}

fn insert_session_row(conn: &Connection, session: &Session) -> Result<()> {
    conn.execute(
        "INSERT INTO sessions
         (id, project_id, goal, status, priority, worktree, branch, token_budget,
          cost_budget_usd, time_budget_secs, unlimited, cost_spent_usd, base_commit,
          tokens_in, tokens_out, prompt_layout, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0, ?12, 0, 0, ?13, ?14)",
        params![
            session.id,
            session.project_id,
            session.goal,
            session.status,
            session.priority,
            session.worktree,
            session.branch,
            session.token_budget,
            session.cost_budget_usd,
            session.time_budget_secs,
            if session.unlimited { 1 } else { 0 },
            session.base_commit,
            session.prompt_layout as i64,
            session.created_at
        ],
    )?;
    Ok(())
}

fn session_created_payload(session: &Session, budget_mode: BudgetMode) -> serde_json::Value {
    serde_json::json!({
        "goal": session.goal,
        "project_id": session.project_id,
        "worktree": session.worktree,
        "branch": session.branch,
        "priority": session.priority,
        "token_budget": session.token_budget,
        "cost_budget_usd": session.cost_budget_usd,
        "time_budget_secs": session.time_budget_secs,
        "unlimited": session.unlimited,
        "budget_mode": budget_mode.as_str(),
        "base_commit": session.base_commit,
        "prompt_layout": session.prompt_layout,
        "created_at": session.created_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fixture() -> (TempDir, TempDir, Store, Project) {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("sessions"))
            .unwrap();
        (data, repo, store, project)
    }

    #[test]
    fn oversized_goal_is_rejected_before_a_session_is_created() {
        let (_data, _repo, store, project) = fixture();
        let exact = "g".repeat(MAX_SESSION_GOAL_BYTES);
        store.create_session(&project.id, &exact).unwrap();
        let oversized = "g".repeat(MAX_SESSION_GOAL_BYTES + 1);
        let error = store.create_session(&project.id, &oversized).unwrap_err();
        let typed = error.downcast_ref::<SessionValidationError>().unwrap();
        assert_eq!(typed.code, "INVALID_ARGUMENT");
        assert!(typed.message.contains("goal"));
        assert_eq!(store.list_sessions(Some(&project.id)).unwrap().len(), 1);
    }

    #[test]
    fn oversized_user_prompt_is_rejected_before_a_message_is_created() {
        let (_data, _repo, store, project) = fixture();
        let session = store.create_session(&project.id, "prompt bound").unwrap();
        let exact = "p".repeat(MAX_SESSION_PROMPT_BYTES);
        store
            .append_message(&session.id, "user", &exact, &serde_json::json!([]), None)
            .unwrap();
        let oversized = "p".repeat(MAX_SESSION_PROMPT_BYTES + 1);
        let error = store
            .append_message(
                &session.id,
                "user",
                &oversized,
                &serde_json::json!([]),
                None,
            )
            .unwrap_err();
        let typed = error.downcast_ref::<SessionValidationError>().unwrap();
        assert_eq!(typed.code, "INVALID_ARGUMENT");
        assert!(typed.message.contains("prompt"));
        assert_eq!(store.list_messages(&session.id).unwrap().len(), 1);
    }
}

//! Project programs and milestones.

use super::*;

impl Store {
    pub fn list_programs(
        &self,
        project_id: Option<&str>,
        status: Option<ProgramStatus>,
        limit: Option<u32>,
    ) -> Result<Vec<Program>> {
        let conn = self.lock()?;
        let limit = limit.unwrap_or(500).clamp(1, 2_000) as i64;
        let mut stmt = conn.prepare(&format!(
            "SELECT {PROGRAM_COLUMNS}
             FROM programs
             WHERE (?1 IS NULL OR project_id = ?1)
               AND (?2 IS NULL OR status = ?2)
             ORDER BY created_at DESC, id DESC
             LIMIT ?3"
        ))?;
        let rows = stmt.query_map(
            params![project_id, status.map(ProgramStatus::as_str), limit],
            map_program,
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_program(&self, id: &str) -> Result<Option<Program>> {
        let conn = self.lock()?;
        conn.query_row(
            &format!("SELECT {PROGRAM_COLUMNS} FROM programs WHERE id = ?1"),
            params![id],
            map_program,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn list_program_milestones(&self, program_id: &str) -> Result<Vec<ProgramMilestone>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {PROGRAM_MILESTONE_COLUMNS}
             FROM program_milestones
             WHERE program_id = ?1
             ORDER BY created_at ASC, key ASC, id ASC"
        ))?;
        let rows = stmt.query_map(params![program_id], map_program_milestone)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn list_program_milestones_for_project(
        &self,
        project_id: &str,
    ) -> Result<Vec<ProgramMilestone>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {}
             FROM program_milestones m
             JOIN programs p ON p.id = m.program_id
             WHERE p.project_id = ?1
             ORDER BY m.created_at ASC, m.key ASC, m.id ASC",
            PROGRAM_MILESTONE_COLUMNS
                .split(", ")
                .map(|column| format!("m.{column}"))
                .collect::<Vec<_>>()
                .join(", ")
        ))?;
        let rows = stmt.query_map(params![project_id], map_program_milestone)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_program_milestone(&self, id: &str) -> Result<Option<ProgramMilestone>> {
        let conn = self.lock()?;
        conn.query_row(
            &format!("SELECT {PROGRAM_MILESTONE_COLUMNS} FROM program_milestones WHERE id = ?1"),
            params![id],
            map_program_milestone,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn create_program(&self, input: &NewProgram) -> Result<(Program, ProgramEvent)> {
        let id = Uuid::now_v7().to_string();
        let now = now_rfc3339();
        let program = Program {
            id,
            project_id: input.project_id.clone(),
            goal: input.goal.clone(),
            status: input.status,
            policy: input.policy.clone(),
            token_budget: input.token_budget,
            cost_budget_usd: input.cost_budget_usd,
            reason: None,
            archived: input.archived,
            created_at: now.clone(),
            updated_at: now,
            completed_at: None,
        };
        let payload = serde_json::to_value(&program)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        if tx.query_row(
            "SELECT COUNT(*) FROM projects WHERE id = ?1",
            params![program.project_id],
            |row| row.get::<_, i64>(0),
        )? == 0
        {
            anyhow::bail!("project not found: {}", program.project_id);
        }
        let event = insert_program_event_tx(
            &tx,
            &program.project_id,
            &program.id,
            "program.created",
            &payload,
        )?;
        tx.execute(
            "INSERT INTO programs
             (id, project_id, goal, status, policy, token_budget, cost_budget_usd, reason,
              archived, created_at, updated_at, completed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                program.id,
                program.project_id,
                program.goal,
                program.status.as_str(),
                serde_json::to_string(&program.policy)?,
                program.token_budget.map(|value| value as i64),
                program.cost_budget_usd,
                program.reason,
                if program.archived { 1 } else { 0 },
                program.created_at,
                program.updated_at,
                program.completed_at,
            ],
        )?;
        tx.commit()?;
        Ok((program, event))
    }

    pub fn update_program(
        &self,
        id: &str,
        update: &ProgramUpdate,
    ) -> Result<Option<(Program, ProgramEvent)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut program) = tx
            .query_row(
                &format!("SELECT {PROGRAM_COLUMNS} FROM programs WHERE id = ?1"),
                params![id],
                map_program,
            )
            .optional()?
        else {
            return Ok(None);
        };
        if program.archived || program.status.is_terminal() {
            anyhow::bail!("program {id} is archived or terminal and cannot be updated");
        }
        program.goal = update.goal.clone();
        program.policy = update.policy.clone();
        program.token_budget = update.token_budget;
        program.cost_budget_usd = update.cost_budget_usd;
        program.updated_at = now;
        let payload = serde_json::to_value(&program)?;
        let event =
            insert_program_event_tx(&tx, &program.project_id, id, "program.updated", &payload)?;
        tx.execute(
            "UPDATE programs
             SET goal = ?2, policy = ?3, token_budget = ?4, cost_budget_usd = ?5, updated_at = ?6
             WHERE id = ?1",
            params![
                id,
                program.goal,
                serde_json::to_string(&program.policy)?,
                program.token_budget.map(|value| value as i64),
                program.cost_budget_usd,
                program.updated_at,
            ],
        )?;
        tx.commit()?;
        Ok(Some((program, event)))
    }

    pub fn pause_program(
        &self,
        id: &str,
        reason: Option<&str>,
    ) -> Result<Option<(Program, ProgramEvent)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut program) = tx
            .query_row(
                &format!("SELECT {PROGRAM_COLUMNS} FROM programs WHERE id = ?1"),
                params![id],
                map_program,
            )
            .optional()?
        else {
            return Ok(None);
        };
        if program.status == ProgramStatus::Paused {
            return Ok(None);
        }
        if program.status != ProgramStatus::Active {
            anyhow::bail!("program {id} is not active");
        }
        program.status = ProgramStatus::Paused;
        program.reason = reason.map(str::to_string);
        program.updated_at = now_rfc3339();
        let payload = serde_json::to_value(&program)?;
        let event =
            insert_program_event_tx(&tx, &program.project_id, id, "program.paused", &payload)?;
        tx.execute(
            "UPDATE programs SET status = ?2, reason = ?3, updated_at = ?4 WHERE id = ?1",
            params![
                id,
                program.status.as_str(),
                program.reason,
                program.updated_at
            ],
        )?;
        tx.commit()?;
        Ok(Some((program, event)))
    }

    pub fn resume_program(&self, id: &str) -> Result<Option<(Program, ProgramEvent)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut program) = tx
            .query_row(
                &format!("SELECT {PROGRAM_COLUMNS} FROM programs WHERE id = ?1"),
                params![id],
                map_program,
            )
            .optional()?
        else {
            return Ok(None);
        };
        if program.archived {
            anyhow::bail!("program {id} is an imported archive and cannot be activated");
        }
        if program.status.is_terminal() {
            anyhow::bail!("program {id} is terminal and cannot be resumed");
        }
        if program.status == ProgramStatus::Active {
            return Ok(None);
        }
        program.status = ProgramStatus::Active;
        program.reason = None;
        program.updated_at = now_rfc3339();
        let payload = serde_json::to_value(&program)?;
        let event =
            insert_program_event_tx(&tx, &program.project_id, id, "program.resumed", &payload)?;
        tx.execute(
            "UPDATE programs SET status = ?2, reason = NULL, updated_at = ?3 WHERE id = ?1",
            params![id, program.status.as_str(), program.updated_at],
        )?;
        tx.commit()?;
        Ok(Some((program, event)))
    }

    pub fn complete_program(
        &self,
        id: &str,
        reason: &str,
    ) -> Result<Option<(Program, ProgramEvent)>> {
        self.finish_program(id, ProgramStatus::Completed, reason)
    }

    pub fn fail_program(&self, id: &str, reason: &str) -> Result<Option<(Program, ProgramEvent)>> {
        self.finish_program(id, ProgramStatus::Failed, reason)
    }

    pub(super) fn finish_program(
        &self,
        id: &str,
        status: ProgramStatus,
        reason: &str,
    ) -> Result<Option<(Program, ProgramEvent)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut program) = tx
            .query_row(
                &format!("SELECT {PROGRAM_COLUMNS} FROM programs WHERE id = ?1"),
                params![id],
                map_program,
            )
            .optional()?
        else {
            return Ok(None);
        };
        if program.status.is_terminal() {
            return Ok(None);
        }
        if program.archived {
            anyhow::bail!("program {id} is an imported archive");
        }
        let now = now_rfc3339();
        program.status = status;
        program.reason = Some(reason.to_string());
        program.updated_at = now.clone();
        program.completed_at = Some(now.clone());
        let payload = serde_json::to_value(&program)?;
        let kind = match status {
            ProgramStatus::Completed => "program.completed",
            ProgramStatus::Failed => "program.failed",
            _ => anyhow::bail!("invalid terminal program status: {status}"),
        };
        let event = insert_program_event_tx(&tx, &program.project_id, id, kind, &payload)?;
        tx.execute(
            "UPDATE programs
             SET status = ?2, reason = ?3, completed_at = ?4, updated_at = ?4
             WHERE id = ?1",
            params![id, program.status.as_str(), program.reason, now],
        )?;
        tx.commit()?;
        Ok(Some((program, event)))
    }

    pub fn delete_program(&self, id: &str) -> Result<bool> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(program) = tx
            .query_row(
                &format!("SELECT {PROGRAM_COLUMNS} FROM programs WHERE id = ?1"),
                params![id],
                map_program,
            )
            .optional()?
        else {
            return Ok(false);
        };
        let active = tx.query_row(
            "SELECT COUNT(*) FROM program_milestones
             WHERE program_id = ?1 AND status IN ('running', 'verifying')",
            params![id],
            |row| row.get::<_, i64>(0),
        )?;
        if active > 0 {
            anyhow::bail!("program {id} still has running or verifying milestones");
        }
        if !program.status.is_terminal() && program.status != ProgramStatus::Draft {
            anyhow::bail!("only draft or terminal programs can be deleted");
        }
        let payload = serde_json::json!({"id": id});
        insert_program_event_tx(&tx, &program.project_id, id, "program.deleted", &payload)?;
        tx.execute("DELETE FROM programs WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(true)
    }

    pub fn create_program_milestone(
        &self,
        input: &NewProgramMilestone,
    ) -> Result<(ProgramMilestone, ProgramEvent)> {
        let now = now_rfc3339();
        let id = Uuid::now_v7().to_string();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(program) = tx
            .query_row(
                &format!("SELECT {PROGRAM_COLUMNS} FROM programs WHERE id = ?1"),
                params![input.program_id],
                map_program,
            )
            .optional()?
        else {
            anyhow::bail!("program not found: {}", input.program_id);
        };
        if program.archived || program.status.is_terminal() {
            anyhow::bail!("program {} is archived or terminal", program.id);
        }
        if tx.query_row(
            "SELECT COUNT(*) FROM program_milestones WHERE program_id = ?1 AND key = ?2",
            params![input.program_id, input.key],
            |row| row.get::<_, i64>(0),
        )? > 0
        {
            anyhow::bail!("milestone key '{}' already exists", input.key);
        }
        let milestone = ProgramMilestone {
            id,
            program_id: input.program_id.clone(),
            key: input.key.clone(),
            title: input.title.clone(),
            objective: input.objective.clone(),
            acceptance: input.acceptance.clone(),
            deps: input.deps.clone(),
            priority: input.priority,
            status: ProgramMilestoneStatus::Pending,
            session_id: None,
            plan_revision: None,
            attempts: 0,
            max_attempts: input.max_attempts,
            next_attempt_at: None,
            result: None,
            last_error: None,
            profile: input.profile.clone(),
            write_scope: input.write_scope.clone(),
            token_budget: input.token_budget,
            cost_budget_usd: input.cost_budget_usd,
            created_at: now.clone(),
            updated_at: now,
            completed_at: None,
        };
        let payload = serde_json::to_value(&milestone)?;
        let event = insert_program_event_tx(
            &tx,
            &program.project_id,
            &program.id,
            "program.milestone.created",
            &payload,
        )?;
        tx.execute(
            "INSERT INTO program_milestones
             (id, program_id, key, title, acceptance, deps, priority, status, session_id,
              plan_revision, attempts, max_attempts, next_attempt_at, result, last_error,
              write_scope, objective, profile, token_budget, cost_budget_usd, created_at,
              updated_at, completed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, NULL, 0, ?9, NULL, NULL, NULL,
                     ?10, ?11, ?12, ?13, ?14, ?15, ?16, NULL)",
            params![
                milestone.id,
                milestone.program_id,
                milestone.key,
                milestone.title,
                serde_json::to_string(&milestone.acceptance)?,
                serde_json::to_string(&milestone.deps)?,
                milestone.priority,
                milestone.status.as_str(),
                milestone.max_attempts as i64,
                serde_json::to_string(&milestone.write_scope)?,
                milestone.objective,
                milestone.profile,
                milestone.token_budget.map(|value| value as i64),
                milestone.cost_budget_usd,
                milestone.created_at,
                milestone.updated_at,
            ],
        )?;
        tx.commit()?;
        Ok((milestone, event))
    }

    pub fn update_program_milestone(
        &self,
        id: &str,
        update: &ProgramMilestoneUpdate,
    ) -> Result<Option<(ProgramMilestone, ProgramEvent)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut milestone) = tx
            .query_row(
                &format!(
                    "SELECT {PROGRAM_MILESTONE_COLUMNS} FROM program_milestones WHERE id = ?1"
                ),
                params![id],
                map_program_milestone,
            )
            .optional()?
        else {
            return Ok(None);
        };
        if matches!(
            milestone.status,
            ProgramMilestoneStatus::Running
                | ProgramMilestoneStatus::Verifying
                | ProgramMilestoneStatus::Completed
        ) {
            anyhow::bail!(
                "milestone {id} is {} and cannot be edited",
                milestone.status
            );
        }
        if tx.query_row(
            "SELECT COUNT(*) FROM program_milestones
                 WHERE program_id = ?1 AND key = ?2 AND id <> ?3",
            params![milestone.program_id, update.key, id],
            |row| row.get::<_, i64>(0),
        )? > 0
        {
            anyhow::bail!("milestone key '{}' already exists", update.key);
        }
        milestone.key = update.key.clone();
        milestone.title = update.title.clone();
        milestone.objective = update.objective.clone();
        milestone.acceptance = update.acceptance.clone();
        milestone.deps = update.deps.clone();
        milestone.priority = update.priority;
        milestone.max_attempts = update.max_attempts;
        milestone.profile = update.profile.clone();
        milestone.write_scope = update.write_scope.clone();
        milestone.token_budget = update.token_budget;
        milestone.cost_budget_usd = update.cost_budget_usd;
        milestone.updated_at = now;
        let payload = serde_json::to_value(&milestone)?;
        let project_id: String = tx.query_row(
            "SELECT project_id FROM programs WHERE id = ?1",
            params![milestone.program_id],
            |row| row.get(0),
        )?;
        let event = insert_program_event_tx(
            &tx,
            &project_id,
            &milestone.program_id,
            "program.milestone.updated",
            &payload,
        )?;
        tx.execute(
            "UPDATE program_milestones
             SET key = ?2, title = ?3, objective = ?4, acceptance = ?5, deps = ?6,
                 priority = ?7, max_attempts = ?8, profile = ?9, write_scope = ?10,
                 token_budget = ?11, cost_budget_usd = ?12, updated_at = ?13
             WHERE id = ?1",
            params![
                id,
                milestone.key,
                milestone.title,
                milestone.objective,
                serde_json::to_string(&milestone.acceptance)?,
                serde_json::to_string(&milestone.deps)?,
                milestone.priority,
                milestone.max_attempts as i64,
                milestone.profile,
                serde_json::to_string(&milestone.write_scope)?,
                milestone.token_budget.map(|value| value as i64),
                milestone.cost_budget_usd,
                milestone.updated_at,
            ],
        )?;
        tx.commit()?;
        Ok(Some((milestone, event)))
    }

    pub fn mark_program_milestone_ready(
        &self,
        id: &str,
    ) -> Result<Option<(ProgramMilestone, ProgramEvent)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut milestone) = tx
            .query_row(
                &format!(
                    "SELECT {PROGRAM_MILESTONE_COLUMNS} FROM program_milestones WHERE id = ?1"
                ),
                params![id],
                map_program_milestone,
            )
            .optional()?
        else {
            return Ok(None);
        };
        if !matches!(
            milestone.status,
            ProgramMilestoneStatus::Pending | ProgramMilestoneStatus::Failed
        ) || milestone.attempts >= milestone.max_attempts
        {
            return Ok(None);
        }
        milestone.status = ProgramMilestoneStatus::Ready;
        milestone.next_attempt_at = None;
        milestone.updated_at = now;
        let payload = serde_json::to_value(&milestone)?;
        let project_id: String = tx.query_row(
            "SELECT project_id FROM programs WHERE id = ?1",
            params![milestone.program_id],
            |row| row.get(0),
        )?;
        let event = insert_program_event_tx(
            &tx,
            &project_id,
            &milestone.program_id,
            "program.milestone.updated",
            &payload,
        )?;
        tx.execute(
            "UPDATE program_milestones
             SET status = 'ready', next_attempt_at = NULL, updated_at = ?2
             WHERE id = ?1",
            params![id, milestone.updated_at],
        )?;
        tx.commit()?;
        Ok(Some((milestone, event)))
    }

    /// Atomically claim a milestone for dispatch. The caller creates the
    /// session only after this succeeds, so two scheduler ticks cannot launch
    /// duplicate sessions for the same milestone.
    pub fn claim_program_milestone_start(
        &self,
        id: &str,
    ) -> Result<Option<(ProgramMilestone, ProgramEvent)>> {
        let now = OffsetDateTime::now_utc();
        let now_text = now.format(&Rfc3339)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(program) = tx
            .query_row(
                "SELECT p.id, p.project_id, p.status, p.archived
                 FROM programs p
                 JOIN program_milestones m ON m.program_id = p.id
                 WHERE m.id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)? != 0,
                    ))
                },
            )
            .optional()?
        else {
            return Ok(None);
        };
        if program.2 != ProgramStatus::Active.as_str() || program.3 {
            return Ok(None);
        }
        let Some(mut milestone) = tx
            .query_row(
                &format!(
                    "SELECT {PROGRAM_MILESTONE_COLUMNS} FROM program_milestones WHERE id = ?1"
                ),
                params![id],
                map_program_milestone,
            )
            .optional()?
        else {
            return Ok(None);
        };
        let retry_due = milestone
            .next_attempt_at
            .as_deref()
            .and_then(|value| OffsetDateTime::parse(value, &Rfc3339).ok())
            .is_some_and(|next| next <= now);
        let runnable = matches!(
            milestone.status,
            ProgramMilestoneStatus::Pending
                | ProgramMilestoneStatus::Ready
                | ProgramMilestoneStatus::Failed
        );
        if !runnable || milestone.attempts >= milestone.max_attempts {
            return Ok(None);
        }
        if milestone.status == ProgramMilestoneStatus::Failed
            && milestone.next_attempt_at.is_some()
            && !retry_due
        {
            return Ok(None);
        }
        let statuses = tx
            .prepare("SELECT id, status FROM program_milestones WHERE program_id = ?1")?
            .query_map(params![milestone.program_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<HashMap<_, _>, _>>()?;
        let deps_ready = milestone.deps.iter().all(|dep| {
            statuses.get(dep).map(String::as_str)
                == Some(ProgramMilestoneStatus::Completed.as_str())
        });
        if !deps_ready {
            return Ok(None);
        }
        milestone.status = ProgramMilestoneStatus::Running;
        milestone.session_id = None;
        milestone.plan_revision = None;
        milestone.attempts = milestone.attempts.saturating_add(1);
        milestone.next_attempt_at = None;
        milestone.result = None;
        milestone.last_error = None;
        milestone.updated_at = now_text;
        milestone.completed_at = None;
        let payload = serde_json::to_value(&milestone)?;
        let event = insert_program_event_tx(
            &tx,
            &program.1,
            &milestone.program_id,
            "program.milestone.started",
            &payload,
        )?;
        tx.execute(
            "UPDATE program_milestones
             SET status = 'running', session_id = NULL, plan_revision = NULL,
                 attempts = ?2, next_attempt_at = NULL, result = NULL, last_error = NULL,
                 completed_at = NULL, updated_at = ?3
             WHERE id = ?1",
            params![id, milestone.attempts as i64, milestone.updated_at],
        )?;
        tx.commit()?;
        Ok(Some((milestone, event)))
    }

    pub fn attach_program_milestone_session(
        &self,
        id: &str,
        session_id: &str,
        plan_revision: u32,
    ) -> Result<Option<(ProgramMilestone, ProgramEvent)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut milestone) = tx
            .query_row(
                &format!(
                    "SELECT {PROGRAM_MILESTONE_COLUMNS} FROM program_milestones WHERE id = ?1"
                ),
                params![id],
                map_program_milestone,
            )
            .optional()?
        else {
            return Ok(None);
        };
        if milestone.status != ProgramMilestoneStatus::Running {
            anyhow::bail!("milestone {id} is not running");
        }
        milestone.session_id = Some(session_id.to_string());
        milestone.plan_revision = Some(plan_revision.max(1));
        milestone.updated_at = now_rfc3339();
        let payload = serde_json::to_value(&milestone)?;
        let project_id: String = tx.query_row(
            "SELECT project_id FROM programs WHERE id = ?1",
            params![milestone.program_id],
            |row| row.get(0),
        )?;
        let event = insert_program_event_tx(
            &tx,
            &project_id,
            &milestone.program_id,
            "program.milestone.updated",
            &payload,
        )?;
        tx.execute(
            "UPDATE program_milestones
             SET session_id = ?2, plan_revision = ?3, updated_at = ?4
             WHERE id = ?1",
            params![id, session_id, plan_revision as i64, milestone.updated_at],
        )?;
        // The link ledger is written in the same transaction as the event and
        // projection. It survives retries, so program budgets keep counting
        // every attempt instead of only the current session.
        tx.execute(
            "INSERT OR IGNORE INTO program_milestone_sessions
             (program_id, milestone_id, session_id, attempt, dispatched_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                milestone.program_id,
                id,
                session_id,
                milestone.attempts as i64,
                milestone.updated_at,
            ],
        )?;
        tx.commit()?;
        Ok(Some((milestone, event)))
    }

    pub fn mark_program_milestone_verifying(
        &self,
        id: &str,
    ) -> Result<Option<(ProgramMilestone, ProgramEvent)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut milestone) = tx
            .query_row(
                &format!(
                    "SELECT {PROGRAM_MILESTONE_COLUMNS} FROM program_milestones WHERE id = ?1"
                ),
                params![id],
                map_program_milestone,
            )
            .optional()?
        else {
            return Ok(None);
        };
        if milestone.status != ProgramMilestoneStatus::Running {
            return Ok(None);
        }
        milestone.status = ProgramMilestoneStatus::Verifying;
        milestone.updated_at = now_rfc3339();
        let payload = serde_json::to_value(&milestone)?;
        let project_id: String = tx.query_row(
            "SELECT project_id FROM programs WHERE id = ?1",
            params![milestone.program_id],
            |row| row.get(0),
        )?;
        let event = insert_program_event_tx(
            &tx,
            &project_id,
            &milestone.program_id,
            "program.milestone.verified",
            &payload,
        )?;
        tx.execute(
            "UPDATE program_milestones SET status = 'verifying', updated_at = ?2 WHERE id = ?1",
            params![id, milestone.updated_at],
        )?;
        tx.commit()?;
        Ok(Some((milestone, event)))
    }

    pub fn complete_program_milestone(
        &self,
        id: &str,
        result: &str,
    ) -> Result<Option<(ProgramMilestone, ProgramEvent)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut milestone) = tx
            .query_row(
                &format!(
                    "SELECT {PROGRAM_MILESTONE_COLUMNS} FROM program_milestones WHERE id = ?1"
                ),
                params![id],
                map_program_milestone,
            )
            .optional()?
        else {
            return Ok(None);
        };
        if !matches!(
            milestone.status,
            ProgramMilestoneStatus::Verifying | ProgramMilestoneStatus::Running
        ) {
            return Ok(None);
        }
        milestone.status = ProgramMilestoneStatus::Completed;
        milestone.result = Some(result.trim().chars().take(8_000).collect());
        milestone.last_error = None;
        milestone.next_attempt_at = None;
        milestone.completed_at = Some(now.clone());
        milestone.updated_at = now.clone();
        let payload = serde_json::to_value(&milestone)?;
        let project_id: String = tx.query_row(
            "SELECT project_id FROM programs WHERE id = ?1",
            params![milestone.program_id],
            |row| row.get(0),
        )?;
        let event = insert_program_event_tx(
            &tx,
            &project_id,
            &milestone.program_id,
            "program.milestone.completed",
            &payload,
        )?;
        tx.execute(
            "UPDATE program_milestones
             SET status = 'completed', result = ?2, last_error = NULL, next_attempt_at = NULL,
                 completed_at = ?3, updated_at = ?3
             WHERE id = ?1",
            params![id, milestone.result, now],
        )?;
        tx.commit()?;
        Ok(Some((milestone, event)))
    }

    /// Record a failed attempt. When `next_attempt_at` is set the row stays
    /// `failed` until the backoff becomes due; otherwise the caller follows up
    /// with `block_program_milestone` so the milestone becomes permanently
    /// terminal.
    pub fn fail_program_milestone(
        &self,
        id: &str,
        reason: &str,
        next_attempt_at: Option<&str>,
    ) -> Result<Option<(ProgramMilestone, Vec<ProgramEvent>)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut milestone) = tx
            .query_row(
                &format!(
                    "SELECT {PROGRAM_MILESTONE_COLUMNS} FROM program_milestones WHERE id = ?1"
                ),
                params![id],
                map_program_milestone,
            )
            .optional()?
        else {
            return Ok(None);
        };
        if matches!(
            milestone.status,
            ProgramMilestoneStatus::Completed
                | ProgramMilestoneStatus::Blocked
                | ProgramMilestoneStatus::Cancelled
        ) {
            return Ok(None);
        }
        milestone.status = ProgramMilestoneStatus::Failed;
        milestone.last_error = Some(reason.trim().chars().take(4_000).collect());
        milestone.next_attempt_at = next_attempt_at.map(str::to_string);
        milestone.result = None;
        milestone.completed_at = None;
        milestone.updated_at = now.clone();
        let payload = serde_json::to_value(&milestone)?;
        let project_id: String = tx.query_row(
            "SELECT project_id FROM programs WHERE id = ?1",
            params![milestone.program_id],
            |row| row.get(0),
        )?;
        let mut events = vec![insert_program_event_tx(
            &tx,
            &project_id,
            &milestone.program_id,
            "program.milestone.failed",
            &payload,
        )?];
        if next_attempt_at.is_some() {
            events.push(insert_program_event_tx(
                &tx,
                &project_id,
                &milestone.program_id,
                "program.milestone.retried",
                &payload,
            )?);
        }
        tx.execute(
            "UPDATE program_milestones
             SET status = 'failed', last_error = ?2, next_attempt_at = ?3,
                 result = NULL, completed_at = NULL, updated_at = ?4
             WHERE id = ?1",
            params![id, milestone.last_error, next_attempt_at, now],
        )?;
        tx.commit()?;
        Ok(Some((milestone, events)))
    }

    pub fn block_program_milestone(
        &self,
        id: &str,
        reason: &str,
    ) -> Result<Option<(ProgramMilestone, ProgramEvent)>> {
        self.set_program_milestone_terminal(
            id,
            ProgramMilestoneStatus::Blocked,
            reason,
            "program.milestone.blocked",
        )
    }

    pub fn cancel_program_milestone(
        &self,
        id: &str,
        reason: &str,
        skipped: bool,
    ) -> Result<Option<(ProgramMilestone, ProgramEvent)>> {
        self.set_program_milestone_terminal(
            id,
            ProgramMilestoneStatus::Cancelled,
            reason,
            if skipped {
                "program.milestone.skipped"
            } else {
                "program.milestone.cancelled"
            },
        )
    }

    pub(super) fn set_program_milestone_terminal(
        &self,
        id: &str,
        status: ProgramMilestoneStatus,
        reason: &str,
        kind: &str,
    ) -> Result<Option<(ProgramMilestone, ProgramEvent)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut milestone) = tx
            .query_row(
                &format!(
                    "SELECT {PROGRAM_MILESTONE_COLUMNS} FROM program_milestones WHERE id = ?1"
                ),
                params![id],
                map_program_milestone,
            )
            .optional()?
        else {
            return Ok(None);
        };
        if matches!(
            milestone.status,
            ProgramMilestoneStatus::Completed | ProgramMilestoneStatus::Cancelled
        ) {
            return Ok(None);
        }
        milestone.status = status;
        milestone.last_error = Some(reason.trim().chars().take(4_000).collect());
        milestone.next_attempt_at = None;
        milestone.completed_at = Some(now.clone());
        milestone.updated_at = now.clone();
        let payload = serde_json::to_value(&milestone)?;
        let project_id: String = tx.query_row(
            "SELECT project_id FROM programs WHERE id = ?1",
            params![milestone.program_id],
            |row| row.get(0),
        )?;
        let event =
            insert_program_event_tx(&tx, &project_id, &milestone.program_id, kind, &payload)?;
        tx.execute(
            "UPDATE program_milestones
             SET status = ?2, last_error = ?3, next_attempt_at = NULL,
                 completed_at = ?4, updated_at = ?4
             WHERE id = ?1",
            params![id, status.as_str(), milestone.last_error, now],
        )?;
        tx.commit()?;
        Ok(Some((milestone, event)))
    }

    /// Explicit operator retry. Resets the automatic attempt budget but keeps
    /// the configured maximum; the next controller tick revalidates every
    /// dependency, budget, and admission gate.
    pub fn retry_program_milestone(
        &self,
        id: &str,
    ) -> Result<Option<(ProgramMilestone, ProgramEvent)>> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let Some(mut milestone) = tx
            .query_row(
                &format!(
                    "SELECT {PROGRAM_MILESTONE_COLUMNS} FROM program_milestones WHERE id = ?1"
                ),
                params![id],
                map_program_milestone,
            )
            .optional()?
        else {
            return Ok(None);
        };
        if !matches!(
            milestone.status,
            ProgramMilestoneStatus::Failed
                | ProgramMilestoneStatus::Blocked
                | ProgramMilestoneStatus::Cancelled
        ) {
            return Ok(None);
        }
        milestone.status = ProgramMilestoneStatus::Pending;
        milestone.attempts = 0;
        milestone.next_attempt_at = None;
        milestone.last_error = None;
        milestone.result = None;
        milestone.session_id = None;
        milestone.plan_revision = None;
        milestone.completed_at = None;
        milestone.updated_at = now.clone();
        let payload = serde_json::to_value(&milestone)?;
        let project_id: String = tx.query_row(
            "SELECT project_id FROM programs WHERE id = ?1",
            params![milestone.program_id],
            |row| row.get(0),
        )?;
        let event = insert_program_event_tx(
            &tx,
            &project_id,
            &milestone.program_id,
            "program.milestone.retried",
            &payload,
        )?;
        tx.execute(
            "UPDATE program_milestones
             SET status = 'pending', attempts = 0, next_attempt_at = NULL, last_error = NULL,
                 result = NULL, session_id = NULL, plan_revision = NULL, completed_at = NULL,
                 updated_at = ?2
             WHERE id = ?1",
            params![id, now],
        )?;
        tx.commit()?;
        Ok(Some((milestone, event)))
    }

    pub fn list_programs_for_session(&self, session_id: &str) -> Result<Vec<Program>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT DISTINCT {}
             FROM programs p
             JOIN program_milestones m ON m.program_id = p.id
             WHERE m.session_id = ?1
                OR EXISTS (
                    SELECT 1 FROM program_milestone_sessions l
                    WHERE l.milestone_id = m.id AND l.session_id = ?1
                )
             ORDER BY p.created_at ASC, p.id ASC",
            PROGRAM_COLUMNS
                .split(", ")
                .map(|column| format!("p.{column}"))
                .collect::<Vec<_>>()
                .join(", ")
        ))?;
        let rows = stmt.query_map(params![session_id], map_program)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn append_program_event(
        &self,
        project_id: &str,
        program_id: &str,
        kind: &str,
        payload: &serde_json::Value,
    ) -> Result<ProgramEvent> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let event = insert_program_event_tx(&tx, project_id, program_id, kind, payload)?;
        tx.commit()?;
        Ok(event)
    }

    pub fn list_program_events(
        &self,
        program_id: &str,
        limit: Option<u32>,
    ) -> Result<Vec<ProgramEvent>> {
        let conn = self.lock()?;
        let limit = limit.unwrap_or(1_000).clamp(1, 10_000) as i64;
        let mut stmt = conn.prepare(
            "SELECT program_id, project_id, seq, kind, payload, created_at
             FROM program_events
             WHERE program_id = ?1
             ORDER BY seq DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![program_id, limit], map_program_event)?;
        let mut events = rows.collect::<Result<Vec<_>, _>>()?;
        events.reverse();
        Ok(events)
    }

    pub fn list_program_events_for_project(
        &self,
        project_id: &str,
        limit: Option<u32>,
    ) -> Result<Vec<ProgramEvent>> {
        let conn = self.lock()?;
        let limit = limit
            .map(|value| value.clamp(1, 100_000) as i64)
            .unwrap_or(-1);
        let mut stmt = conn.prepare(
            "SELECT program_id, project_id, seq, kind, payload, created_at
             FROM program_events
             WHERE project_id = ?1
             ORDER BY created_at ASC, program_id ASC, seq ASC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![project_id, limit], map_program_event)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn program_usage(&self, program_id: &str) -> Result<ProgramUsage> {
        let conn = self.lock()?;
        let (tokens, cost) = conn.query_row(
            "SELECT COALESCE(SUM(s.tokens_in + s.tokens_out), 0),
                    COALESCE(SUM(s.cost_spent_usd), 0.0)
             FROM sessions s
             WHERE s.id IN (
                 SELECT session_id FROM program_milestone_sessions WHERE program_id = ?1
                 UNION
                 SELECT session_id FROM program_milestones
                 WHERE program_id = ?1 AND session_id IS NOT NULL
             )",
            params![program_id],
            |row| Ok((row.get::<_, i64>(0)?.max(0) as u64, row.get::<_, f64>(1)?)),
        )?;
        Ok(ProgramUsage {
            token_spent: tokens,
            cost_spent_usd: cost,
        })
    }

    /// Rebuild the append-only dispatch ledger from program events. This is
    /// idempotent and is used by bundle import and `db-rebuild`; live attaches
    /// write the same rows transactionally. `program.milestone.updated` is the
    /// attach event and carries the full milestone row.
    pub fn rebuild_program_session_links(&self, program_id: &str) -> Result<usize> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let inserted = Self::rebuild_program_session_links_tx(&tx, program_id)?;
        tx.commit()?;
        Ok(inserted)
    }

    pub(super) fn rebuild_program_session_links_tx(
        tx: &Transaction<'_>,
        program_id: &str,
    ) -> Result<usize> {
        let mut inserted = 0usize;
        {
            let mut stmt = tx.prepare(
                "SELECT created_at, payload FROM program_events
                 WHERE program_id = ?1 AND kind = 'program.milestone.updated'
                 ORDER BY seq ASC",
            )?;
            let rows = stmt.query_map(params![program_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (created_at, payload) = row?;
                let Ok(value) = serde_json::from_str::<serde_json::Value>(&payload) else {
                    continue;
                };
                let Some(session_id) = value.get("session_id").and_then(|value| value.as_str())
                else {
                    continue;
                };
                let Some(milestone_id) = value.get("id").and_then(|value| value.as_str()) else {
                    continue;
                };
                let attempt = value
                    .get("attempts")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0)
                    .min(i64::MAX as u64) as i64;
                inserted += tx.execute(
                    "INSERT OR IGNORE INTO program_milestone_sessions
                     (program_id, milestone_id, session_id, attempt, dispatched_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![program_id, milestone_id, session_id, attempt, created_at],
                )?;
            }
        }
        {
            let mut stmt = tx.prepare(
                "SELECT id, session_id, attempts, updated_at FROM program_milestones
                 WHERE program_id = ?1 AND session_id IS NOT NULL",
            )?;
            let rows = stmt.query_map(params![program_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?;
            for row in rows {
                let (milestone_id, session_id, attempt, updated_at) = row?;
                inserted += tx.execute(
                    "INSERT OR IGNORE INTO program_milestone_sessions
                     (program_id, milestone_id, session_id, attempt, dispatched_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![program_id, milestone_id, session_id, attempt, updated_at],
                )?;
            }
        }
        Ok(inserted)
    }

    /// Aggregate spend for one milestone across every attempt. The current
    /// `program_milestones.session_id` is included as a fallback so legacy rows
    /// created before the dispatch ledger are still counted once.
    pub fn program_milestone_usage(&self, milestone_id: &str) -> Result<ProgramUsage> {
        let conn = self.lock()?;
        let (tokens, cost) = conn.query_row(
            "SELECT COALESCE(SUM(s.tokens_in + s.tokens_out), 0),
                    COALESCE(SUM(s.cost_spent_usd), 0.0)
             FROM sessions s
             WHERE s.id IN (
                 SELECT session_id FROM program_milestone_sessions
                 WHERE milestone_id = ?1
                 UNION
                 SELECT session_id FROM program_milestones
                 WHERE id = ?1 AND session_id IS NOT NULL
             )",
            params![milestone_id],
            |row| Ok((row.get::<_, i64>(0)?.max(0) as u64, row.get::<_, f64>(1)?)),
        )?;
        Ok(ProgramUsage {
            token_spent: tokens,
            cost_spent_usd: cost,
        })
    }

    pub fn count_active_program_milestones(&self, program_id: &str) -> Result<u32> {
        let conn = self.lock()?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM program_milestones
             WHERE program_id = ?1 AND status IN ('running', 'verifying')",
            params![program_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count.max(0) as u32)
    }

    pub fn insert_program_snapshot(&self, program: &Program) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO programs
             (id, project_id, goal, status, policy, token_budget, cost_budget_usd, reason,
              archived, created_at, updated_at, completed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(id) DO UPDATE SET
                goal = excluded.goal,
                status = excluded.status,
                policy = excluded.policy,
                token_budget = excluded.token_budget,
                cost_budget_usd = excluded.cost_budget_usd,
                reason = excluded.reason,
                archived = excluded.archived,
                updated_at = excluded.updated_at,
                completed_at = excluded.completed_at",
            params![
                program.id,
                program.project_id,
                program.goal,
                program.status.as_str(),
                serde_json::to_string(&program.policy)?,
                program.token_budget.map(|value| value as i64),
                program.cost_budget_usd,
                program.reason,
                if program.archived { 1 } else { 0 },
                program.created_at,
                program.updated_at,
                program.completed_at,
            ],
        )?;
        Ok(())
    }

    pub fn insert_program_milestone_snapshot(&self, milestone: &ProgramMilestone) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO program_milestones
             (id, program_id, key, title, acceptance, deps, priority, status, session_id,
              plan_revision, attempts, max_attempts, next_attempt_at, result, last_error,
              write_scope, objective, profile, token_budget, cost_budget_usd, created_at,
              updated_at, completed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                     ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)
             ON CONFLICT(id) DO UPDATE SET
                key = excluded.key,
                title = excluded.title,
                acceptance = excluded.acceptance,
                deps = excluded.deps,
                priority = excluded.priority,
                status = excluded.status,
                session_id = excluded.session_id,
                plan_revision = excluded.plan_revision,
                attempts = excluded.attempts,
                max_attempts = excluded.max_attempts,
                next_attempt_at = excluded.next_attempt_at,
                result = excluded.result,
                last_error = excluded.last_error,
                write_scope = excluded.write_scope,
                objective = excluded.objective,
                profile = excluded.profile,
                token_budget = excluded.token_budget,
                cost_budget_usd = excluded.cost_budget_usd,
                updated_at = excluded.updated_at,
                completed_at = excluded.completed_at",
            params![
                milestone.id,
                milestone.program_id,
                milestone.key,
                milestone.title,
                serde_json::to_string(&milestone.acceptance)?,
                serde_json::to_string(&milestone.deps)?,
                milestone.priority,
                milestone.status.as_str(),
                milestone.session_id,
                milestone.plan_revision.map(|value| value as i64),
                milestone.attempts as i64,
                milestone.max_attempts as i64,
                milestone.next_attempt_at,
                milestone.result,
                milestone.last_error,
                serde_json::to_string(&milestone.write_scope)?,
                milestone.objective,
                milestone.profile,
                milestone.token_budget.map(|value| value as i64),
                milestone.cost_budget_usd,
                milestone.created_at,
                milestone.updated_at,
                milestone.completed_at,
            ],
        )?;
        Ok(())
    }

    pub fn insert_program_event_snapshot(&self, event: &ProgramEvent) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT OR REPLACE INTO program_events
             (project_id, program_id, seq, kind, payload, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                event.project_id,
                event.program_id,
                event.seq as i64,
                event.kind,
                serde_json::to_string(&event.payload)?,
                event.created_at,
            ],
        )?;
        Ok(())
    }

    /// Replace the whole program projection with an event-derived expectation.
    /// This is used by explicit replay repair and never appends new events.
    pub fn replace_program_projection(
        &self,
        program_id: &str,
        program: Option<&Program>,
        milestones: &[ProgramMilestone],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let expected_ids = milestones
            .iter()
            .map(|milestone| milestone.id.clone())
            .collect::<std::collections::HashSet<_>>();
        let existing = tx
            .prepare("SELECT id FROM program_milestones WHERE program_id = ?1")?
            .query_map(params![program_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        for id in existing {
            if !expected_ids.contains(&id) {
                tx.execute("DELETE FROM program_milestones WHERE id = ?1", params![id])?;
            }
        }
        match program {
            Some(program) => {
                tx.execute(
                    "INSERT INTO programs
                     (id, project_id, goal, status, policy, token_budget, cost_budget_usd, reason,
                      archived, created_at, updated_at, completed_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                     ON CONFLICT(id) DO UPDATE SET
                        project_id = excluded.project_id,
                        goal = excluded.goal,
                        status = excluded.status,
                        policy = excluded.policy,
                        token_budget = excluded.token_budget,
                        cost_budget_usd = excluded.cost_budget_usd,
                        reason = excluded.reason,
                        archived = excluded.archived,
                        updated_at = excluded.updated_at,
                        completed_at = excluded.completed_at",
                    params![
                        program.id,
                        program.project_id,
                        program.goal,
                        program.status.as_str(),
                        serde_json::to_string(&program.policy)?,
                        program.token_budget.map(|value| value as i64),
                        program.cost_budget_usd,
                        program.reason,
                        if program.archived { 1 } else { 0 },
                        program.created_at,
                        program.updated_at,
                        program.completed_at,
                    ],
                )?;
                for milestone in milestones {
                    Self::insert_program_milestone_tx(&tx, milestone)?;
                }
            }
            None => {
                tx.execute("DELETE FROM programs WHERE id = ?1", params![program_id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub(super) fn insert_program_milestone_tx(
        tx: &Transaction<'_>,
        milestone: &ProgramMilestone,
    ) -> Result<()> {
        tx.execute(
            "INSERT INTO program_milestones
             (id, program_id, key, title, acceptance, deps, priority, status, session_id,
              plan_revision, attempts, max_attempts, next_attempt_at, result, last_error,
              write_scope, objective, profile, token_budget, cost_budget_usd, created_at,
              updated_at, completed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                     ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)
             ON CONFLICT(id) DO UPDATE SET
                key = excluded.key,
                title = excluded.title,
                acceptance = excluded.acceptance,
                deps = excluded.deps,
                priority = excluded.priority,
                status = excluded.status,
                session_id = excluded.session_id,
                plan_revision = excluded.plan_revision,
                attempts = excluded.attempts,
                max_attempts = excluded.max_attempts,
                next_attempt_at = excluded.next_attempt_at,
                result = excluded.result,
                last_error = excluded.last_error,
                write_scope = excluded.write_scope,
                objective = excluded.objective,
                profile = excluded.profile,
                token_budget = excluded.token_budget,
                cost_budget_usd = excluded.cost_budget_usd,
                updated_at = excluded.updated_at,
                completed_at = excluded.completed_at",
            params![
                milestone.id,
                milestone.program_id,
                milestone.key,
                milestone.title,
                serde_json::to_string(&milestone.acceptance)?,
                serde_json::to_string(&milestone.deps)?,
                milestone.priority,
                milestone.status.as_str(),
                milestone.session_id,
                milestone.plan_revision.map(|value| value as i64),
                milestone.attempts as i64,
                milestone.max_attempts as i64,
                milestone.next_attempt_at,
                milestone.result,
                milestone.last_error,
                serde_json::to_string(&milestone.write_scope)?,
                milestone.objective,
                milestone.profile,
                milestone.token_budget.map(|value| value as i64),
                milestone.cost_budget_usd,
                milestone.created_at,
                milestone.updated_at,
                milestone.completed_at,
            ],
        )?;
        Ok(())
    }
}

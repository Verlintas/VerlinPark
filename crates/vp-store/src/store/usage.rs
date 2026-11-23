//! Project budgets, session usage ledger, and cost accounting.

use super::*;
use vp_protocol::types::UsageTotals;

impl Store {
    pub fn get_project_budget(&self, project_id: &str) -> Result<ProjectBudget> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT token_budget, cost_budget_usd
                 FROM project_budgets WHERE project_id = ?1",
                params![project_id],
                |row| {
                    Ok(ProjectBudget {
                        token_budget: row.get::<_, Option<i64>>(0)?.map(|value| value as u64),
                        cost_budget_usd: row.get(1)?,
                    })
                },
            )
            .optional()?;
        Ok(stored.unwrap_or_default())
    }

    pub fn set_project_budget(&self, project_id: &str, budget: &ProjectBudget) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_budgets
                (project_id, token_budget, cost_budget_usd, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(project_id) DO UPDATE SET
                token_budget = excluded.token_budget,
                cost_budget_usd = excluded.cost_budget_usd,
                updated_at = excluded.updated_at",
            params![
                project_id,
                budget.token_budget.map(|value| value as i64),
                budget.cost_budget_usd,
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn project_usage(&self, project_id: &str) -> Result<(u64, f64)> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT COALESCE(SUM(tokens_in + tokens_out), 0),
                    COALESCE(SUM(cost_spent_usd), 0)
             FROM sessions WHERE project_id = ?1",
            params![project_id],
            |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, f64>(1)?)),
        )
        .map_err(Into::into)
    }

    pub fn project_usage_by_session(&self, project_id: &str) -> Result<Vec<ProjectUsageSession>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT s.id, s.goal, s.status, s.tokens_in, s.tokens_out, s.cost_spent_usd,
                    (SELECT COUNT(*) FROM usage_ledger u WHERE u.session_id = s.id),
                    COALESCE((SELECT SUM(cache_read_tokens) FROM usage_ledger u
                              WHERE u.session_id = s.id), 0),
                    COALESCE((SELECT SUM(cache_write_tokens) FROM usage_ledger u
                              WHERE u.session_id = s.id), 0)
             FROM sessions s
             WHERE s.project_id = ?1
             ORDER BY s.created_at DESC",
        )?;
        let rows = stmt.query_map(params![project_id], |row| {
            let input_tokens = row.get::<_, i64>(3)? as u64;
            let cache_read_tokens = row.get::<_, i64>(7)? as u64;
            let cache_write_tokens = row.get::<_, i64>(8)? as u64;
            Ok(ProjectUsageSession {
                session_id: row.get(0)?,
                goal: row.get(1)?,
                status: row.get(2)?,
                calls: row.get::<_, i64>(6)? as u64,
                input_tokens,
                output_tokens: row.get::<_, i64>(4)? as u64,
                cache_read_tokens,
                cache_write_tokens,
                cache_hit_ratio: cache_hit_ratio(
                    input_tokens,
                    cache_read_tokens,
                    cache_write_tokens,
                ),
                cost_usd: row.get(5)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn add_session_usage(&self, session_id: &str, input: u64, output: u64) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE sessions
             SET tokens_in = tokens_in + ?1, tokens_out = tokens_out + ?2
             WHERE id = ?3",
            params![input as i64, output as i64, session_id],
        )?;
        Ok(())
    }

    pub fn add_session_cost_with_event(&self, session_id: &str, cost_usd: f64) -> Result<Event> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE sessions SET cost_spent_usd = cost_spent_usd + ?1 WHERE id = ?2",
            params![cost_usd, session_id],
        )?;
        if changed == 0 {
            return Err(anyhow!("session not found: {session_id}"));
        }
        let total = tx.query_row(
            "SELECT cost_spent_usd FROM sessions WHERE id = ?1",
            params![session_id],
            |row| row.get::<_, f64>(0),
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "agent.cost",
            serde_json::json!({
                "amount_usd": cost_usd,
                "total_usd": total,
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(event)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_usage_entry_with_events(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        role: &str,
        task_id: Option<&str>,
        department: Option<&str>,
        model: &str,
        input: u64,
        output: u64,
        cache_read: u64,
        cache_write: u64,
        estimated: bool,
        cost_usd: f64,
    ) -> Result<(UsageEntry, Vec<Event>)> {
        let cache_read = cache_read.min(input);
        let cache_write = cache_write.min(input.saturating_sub(cache_read));
        let entry = UsageEntry {
            id: Uuid::now_v7().to_string(),
            session_id: session_id.to_string(),
            agent_id: agent_id.to_string(),
            agent_name: agent_name.to_string(),
            role: role.to_string(),
            task_id: task_id.map(str::to_string),
            department: department.map(str::to_string),
            model: model.to_string(),
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: cache_read,
            cache_write_tokens: cache_write,
            estimated,
            cost_usd,
            created_at: now_rfc3339(),
        };
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE sessions
             SET tokens_in = tokens_in + ?1,
                 tokens_out = tokens_out + ?2,
                 cost_spent_usd = cost_spent_usd + ?3
             WHERE id = ?4",
            params![input as i64, output as i64, cost_usd, session_id],
        )?;
        if changed == 0 {
            return Err(anyhow!("session not found: {session_id}"));
        }
        tx.execute(
            "INSERT INTO usage_ledger
             (id, session_id, agent_id, agent_name, role, task_id, department, model,
              input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
              estimated, cost_usd, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                entry.id,
                entry.session_id,
                entry.agent_id,
                entry.agent_name,
                entry.role,
                entry.task_id,
                entry.department,
                entry.model,
                entry.input_tokens as i64,
                entry.output_tokens as i64,
                entry.cache_read_tokens as i64,
                entry.cache_write_tokens as i64,
                if entry.estimated { 1 } else { 0 },
                entry.cost_usd,
                entry.created_at
            ],
        )?;
        let usage_event = insert_event_tx(
            &tx,
            session_id,
            "agent.usage",
            serde_json::json!({
                "input_tokens": input,
                "output_tokens": output,
                "cache_read_tokens": entry.cache_read_tokens,
                "cache_write_tokens": entry.cache_write_tokens,
                "estimated": estimated,
                "agent_id": agent_id,
                "agent_name": agent_name,
                "agent_role": role,
                "usage_id": entry.id,
                "task_id": task_id,
                "department": department,
                "model": model,
                "cost_usd": cost_usd,
            }),
        )?;
        let mut events = vec![usage_event];
        if cost_usd > 0.0 {
            let total = tx.query_row(
                "SELECT cost_spent_usd FROM sessions WHERE id = ?1",
                params![session_id],
                |row| row.get::<_, f64>(0),
            )?;
            events.push(insert_event_tx(
                &tx,
                session_id,
                "agent.cost",
                serde_json::json!({
                    "amount_usd": cost_usd,
                    "total_usd": total,
                    "agent_id": agent_id,
                    "agent_name": agent_name,
                    "agent_role": role,
                    "task_id": task_id,
                    "department": department,
                    "model": model,
                }),
            )?);
        }
        tx.commit()?;
        for event in &events {
            if let Err(error) = self.append_event_mirror(event) {
                tracing::warn!(%error, "failed to append event mirror");
            }
        }
        Ok((entry, events))
    }

    /// Session-wide aggregates without materializing per-call ledger rows.
    /// Bucket breakdowns are intentionally empty; callers that need them use
    /// `usage_ledger`.
    pub fn usage_totals(&self, session_id: &str) -> Result<UsageTotals> {
        let conn = self.lock()?;
        let (calls, input, output, cache_read, cache_write, cost) = conn.query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(input_tokens), 0),
                    COALESCE(SUM(output_tokens), 0),
                    COALESCE(SUM(cache_read_tokens), 0),
                    COALESCE(SUM(cache_write_tokens), 0),
                    COALESCE(SUM(cost_usd), 0)
             FROM usage_ledger WHERE session_id = ?1",
            params![session_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)? as u64,
                    row.get::<_, i64>(1)? as u64,
                    row.get::<_, i64>(2)? as u64,
                    row.get::<_, i64>(3)? as u64,
                    row.get::<_, i64>(4)? as u64,
                    row.get::<_, f64>(5)?,
                ))
            },
        )?;
        Ok(UsageTotals {
            calls,
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: cache_read,
            cache_write_tokens: cache_write,
            cache_hit_ratio: if input == 0 {
                0.0
            } else {
                (cache_read as f64 / input as f64).clamp(0.0, 1.0)
            },
            cost_usd: cost,
            by_role: Vec::new(),
            by_model: Vec::new(),
            by_agent: Vec::new(),
            by_task: Vec::new(),
            by_department: Vec::new(),
        })
    }

    pub fn usage_spent(
        &self,
        session_id: &str,
        task_id: Option<&str>,
        department: Option<&str>,
    ) -> Result<(u64, f64, u64, f64)> {
        let conn = self.lock()?;
        let (task_tokens, task_cost) = if let Some(task_id) = task_id {
            conn.query_row(
                "SELECT COALESCE(SUM(input_tokens + output_tokens), 0),
                        COALESCE(SUM(cost_usd), 0)
                 FROM usage_ledger WHERE session_id = ?1 AND task_id = ?2",
                params![session_id, task_id],
                |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, f64>(1)?)),
            )?
        } else {
            (0, 0.0)
        };
        let (department_tokens, department_cost) = if let Some(department) = department {
            conn.query_row(
                "SELECT COALESCE(SUM(input_tokens + output_tokens), 0),
                        COALESCE(SUM(cost_usd), 0)
                 FROM usage_ledger WHERE session_id = ?1 AND department = ?2",
                params![session_id, department],
                |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, f64>(1)?)),
            )?
        } else {
            (0, 0.0)
        };
        Ok((task_tokens, task_cost, department_tokens, department_cost))
    }

    /// Observed input+output tokens per plan task for one session. The
    /// multi-path governor uses this read-only view to blend actual spend into
    /// path weights; it never writes usage or treats an estimate as fact.
    /// Tasks without ledger rows are absent, so callers must default to zero.
    pub fn usage_tokens_by_task(&self, session_id: &str) -> Result<HashMap<String, u64>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT task_id, COALESCE(SUM(input_tokens + output_tokens), 0)
             FROM usage_ledger
             WHERE session_id = ?1 AND task_id IS NOT NULL
             GROUP BY task_id",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
        })?;
        let mut tokens = HashMap::new();
        for row in rows {
            let (task_id, count) = row?;
            tokens.insert(task_id, count);
        }
        Ok(tokens)
    }

    /// Aggregate one agent's usage directly in SQL. The inspector must not
    /// load a long session's whole ledger just to show one Agent's totals.
    pub fn agent_usage_totals(
        &self,
        session_id: &str,
        agent_id: &str,
    ) -> Result<vp_protocol::types::AgentUsageTotals> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(input_tokens), 0),
                    COALESCE(SUM(output_tokens), 0),
                    COALESCE(SUM(cache_read_tokens), 0),
                    COALESCE(SUM(cache_write_tokens), 0),
                    COALESCE(SUM(cost_usd), 0)
             FROM usage_ledger
             WHERE session_id = ?1 AND agent_id = ?2",
            params![session_id, agent_id],
            |row| {
                Ok(vp_protocol::types::AgentUsageTotals {
                    calls: row.get::<_, i64>(0)?.max(0) as u64,
                    input_tokens: row.get::<_, i64>(1)?.max(0) as u64,
                    output_tokens: row.get::<_, i64>(2)?.max(0) as u64,
                    cache_read_tokens: row.get::<_, i64>(3)?.max(0) as u64,
                    cache_write_tokens: row.get::<_, i64>(4)?.max(0) as u64,
                    cost_usd: row.get::<_, f64>(5)?.max(0.0),
                })
            },
        )
        .map_err(Into::into)
    }

    pub fn list_usage_entries(&self, session_id: &str) -> Result<Vec<UsageEntry>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, session_id, agent_id, agent_name, role, task_id, department,
                    model, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                    estimated, cost_usd, created_at
             FROM usage_ledger
             WHERE session_id = ?1
             ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok(UsageEntry {
                id: row.get(0)?,
                session_id: row.get(1)?,
                agent_id: row.get(2)?,
                agent_name: row.get(3)?,
                role: row.get(4)?,
                task_id: row.get(5)?,
                department: row.get(6)?,
                model: row.get(7)?,
                input_tokens: row.get::<_, i64>(8)? as u64,
                output_tokens: row.get::<_, i64>(9)? as u64,
                cache_read_tokens: row.get::<_, i64>(10)? as u64,
                cache_write_tokens: row.get::<_, i64>(11)? as u64,
                estimated: row.get::<_, i64>(12)? != 0,
                cost_usd: row.get(13)?,
                created_at: row.get(14)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Bounded forecast read. Returns the newest `limit` calls in oldest-first
    /// order so callers cannot accidentally load a long session's full ledger.
    /// The hard ceiling matches the auto-budget policy bound.
    pub fn list_recent_usage_entries(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<Vec<UsageEntry>> {
        let limit = limit.clamp(1, 1_000);
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, session_id, agent_id, agent_name, role, task_id, department,
                    model, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                    estimated, cost_usd, created_at
             FROM usage_ledger
             WHERE session_id = ?1
             ORDER BY created_at DESC, id DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![session_id, limit as i64], |row| {
            Ok(UsageEntry {
                id: row.get(0)?,
                session_id: row.get(1)?,
                agent_id: row.get(2)?,
                agent_name: row.get(3)?,
                role: row.get(4)?,
                task_id: row.get(5)?,
                department: row.get(6)?,
                model: row.get(7)?,
                input_tokens: row.get::<_, i64>(8)? as u64,
                output_tokens: row.get::<_, i64>(9)? as u64,
                cache_read_tokens: row.get::<_, i64>(10)? as u64,
                cache_write_tokens: row.get::<_, i64>(11)? as u64,
                estimated: row.get::<_, i64>(12)? != 0,
                cost_usd: row.get(13)?,
                created_at: row.get(14)?,
            })
        })?;
        let mut entries = rows.collect::<Result<Vec<_>, _>>()?;
        entries.reverse();
        Ok(entries)
    }

    pub fn replace_usage_ledger(&self, session_id: &str, entries: &[UsageEntry]) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM usage_ledger WHERE session_id = ?1",
            params![session_id],
        )?;
        for entry in entries {
            tx.execute(
                "INSERT INTO usage_ledger
                 (id, session_id, agent_id, agent_name, role, task_id, department, model,
                  input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                  estimated, cost_usd, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    entry.id,
                    session_id,
                    entry.agent_id,
                    entry.agent_name,
                    entry.role,
                    entry.task_id,
                    entry.department,
                    entry.model,
                    entry.input_tokens as i64,
                    entry.output_tokens as i64,
                    entry.cache_read_tokens as i64,
                    entry.cache_write_tokens as i64,
                    if entry.estimated { 1 } else { 0 },
                    entry.cost_usd,
                    entry.created_at
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn add_session_usage_with_event(
        &self,
        session_id: &str,
        input: u64,
        output: u64,
        estimated: bool,
    ) -> Result<Event> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE sessions
             SET tokens_in = tokens_in + ?1, tokens_out = tokens_out + ?2
             WHERE id = ?3",
            params![input as i64, output as i64, session_id],
        )?;
        if changed == 0 {
            return Err(anyhow!("session not found: {session_id}"));
        }
        let event = insert_event_tx(
            &tx,
            session_id,
            "agent.usage",
            serde_json::json!({
                "input_tokens": input,
                "output_tokens": output,
                "estimated": estimated,
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(event)
    }
}

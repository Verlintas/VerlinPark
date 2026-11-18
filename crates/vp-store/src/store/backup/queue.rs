use super::*;

impl Store {
    pub fn queued_session_prompts(&self) -> Result<Vec<(String, String, String, i32)>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT e.session_id, s.project_id, s.priority, e.payload
             FROM events e JOIN sessions s ON s.id = e.session_id
             WHERE s.status = 'queued' AND e.type = 'agent.queued'
             ORDER BY e.session_id, e.seq DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i32>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut prompts = std::collections::HashMap::new();
        for row in rows {
            let (session_id, project_id, priority, payload) = row?;
            if prompts.contains_key(&session_id) {
                continue;
            }
            let payload: serde_json::Value = serde_json::from_str(&payload).unwrap_or_default();
            // A durable resume already created its linked run row. Re-running
            // the queued journal note as a plain prompt would bypass the
            // duplicate-write gate; startup recovery marks that session failed
            // and leaves it to the explicit/opt-in resume path.
            if payload.get("resumed_run_id").is_some() {
                continue;
            }
            if let Some(prompt) = payload.get("prompt").and_then(serde_json::Value::as_str)
                && !prompt.trim().is_empty()
            {
                prompts.insert(session_id, (project_id, prompt.to_string(), priority));
            }
        }
        let mut prompts = prompts
            .into_iter()
            .map(|(session_id, (project_id, prompt, priority))| {
                (session_id, project_id, prompt, priority)
            })
            .collect::<Vec<_>>();
        prompts.sort_by(|left, right| right.3.cmp(&left.3).then_with(|| left.0.cmp(&right.0)));
        Ok(prompts)
    }

    pub fn interrupt_running_sessions(&self) -> Result<Vec<String>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare("SELECT id FROM sessions WHERE status IN ('running', 'paused', 'queued')")?;
        let ids = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        conn.execute(
            "UPDATE sessions SET status = 'failed' WHERE status IN ('running', 'paused', 'queued')",
            [],
        )?;
        Ok(ids)
    }
}

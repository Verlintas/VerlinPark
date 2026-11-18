//! Task scheduler, scaling, orchestration, integration, and critical-path policies.

use super::*;

impl Store {
    pub fn get_task_scheduler_policy(&self, project_id: &str) -> Result<TaskSchedulerPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT enabled, cooldown_secs, dispatch_tasks, include_intake, max_intake_parallel
                 FROM project_task_scheduler_policies WHERE project_id = ?1",
                params![project_id],
                |row| {
                    Ok(TaskSchedulerPolicy {
                        enabled: row.get::<_, i64>(0)? != 0,
                        cooldown_secs: row.get::<_, i64>(1)?.max(1) as u64,
                        dispatch_tasks: row.get::<_, i64>(2).unwrap_or(0) != 0,
                        include_intake: row.get::<_, i64>(3).unwrap_or(1) != 0,
                        max_intake_parallel: row.get::<_, i64>(4).unwrap_or(2).clamp(1, 10) as u32,
                    })
                },
            )
            .optional()?;
        Ok(stored.unwrap_or_default())
    }

    pub fn has_task_scheduler_policy(&self, project_id: &str) -> Result<bool> {
        let conn = self.lock()?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM project_task_scheduler_policies WHERE project_id = ?1",
            params![project_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count > 0)
    }

    pub fn set_task_scheduler_policy(
        &self,
        project_id: &str,
        policy: &TaskSchedulerPolicy,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_task_scheduler_policies
             (project_id, enabled, cooldown_secs, dispatch_tasks, include_intake,
              max_intake_parallel, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(project_id) DO UPDATE SET
                enabled = excluded.enabled,
                cooldown_secs = excluded.cooldown_secs,
                dispatch_tasks = excluded.dispatch_tasks,
                include_intake = excluded.include_intake,
                max_intake_parallel = excluded.max_intake_parallel,
                updated_at = excluded.updated_at",
            params![
                project_id,
                if policy.enabled { 1 } else { 0 },
                policy.cooldown_secs as i64,
                if policy.dispatch_tasks { 1 } else { 0 },
                if policy.include_intake { 1 } else { 0 },
                policy.max_intake_parallel.clamp(1, 10) as i64,
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn get_scaling_policy(&self, project_id: &str) -> Result<ScalingPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT policy FROM project_scaling_policies WHERE project_id = ?1",
                params![project_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(stored
            .and_then(|raw| serde_json::from_str::<ScalingPolicy>(&raw).ok())
            .unwrap_or_default()
            .normalized())
    }

    pub fn has_scaling_policy(&self, project_id: &str) -> Result<bool> {
        let conn = self.lock()?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM project_scaling_policies WHERE project_id = ?1",
            params![project_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count > 0)
    }

    pub fn set_scaling_policy(&self, project_id: &str, policy: &ScalingPolicy) -> Result<()> {
        let encoded = serde_json::to_string(&policy.clone().normalized())
            .context("serializing scaling policy")?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_scaling_policies (project_id, policy, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(project_id) DO UPDATE SET
                policy = excluded.policy,
                updated_at = excluded.updated_at",
            params![project_id, encoded, now_rfc3339()],
        )?;
        Ok(())
    }

    pub fn get_orchestration_policy(&self, project_id: &str) -> Result<OrchestrationPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT policy FROM project_orchestration_policies WHERE project_id = ?1",
                params![project_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        match stored {
            // A corrupt row must fail closed. `OrchestrationPolicy::default()`
            // is active (auto retry/escalate), so silently decoding to it would
            // enable automation the operator never configured.
            Some(raw) => serde_json::from_str(&raw).context("parsing stored orchestration policy"),
            None => Ok(OrchestrationPolicy::default()),
        }
    }

    pub fn set_orchestration_policy(
        &self,
        project_id: &str,
        policy: &OrchestrationPolicy,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_orchestration_policies (project_id, policy, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(project_id) DO UPDATE SET
                policy = excluded.policy,
                updated_at = excluded.updated_at",
            params![project_id, serde_json::to_string(policy)?, now_rfc3339()],
        )?;
        Ok(())
    }

    /// Critical-path budget policy. A missing row decodes as the disabled
    /// default; stored and returned values are normalized into the daemon's
    /// effective ranges.
    pub fn get_critical_path_policy(&self, project_id: &str) -> Result<CriticalPathPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT policy FROM project_critical_path_policies WHERE project_id = ?1",
                params![project_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(stored
            .and_then(|raw| serde_json::from_str::<CriticalPathPolicy>(&raw).ok())
            .unwrap_or_default()
            .normalized())
    }

    pub fn set_critical_path_policy(
        &self,
        project_id: &str,
        policy: &CriticalPathPolicy,
    ) -> Result<()> {
        let normalized = policy.clone().normalized();
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_critical_path_policies (project_id, policy, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(project_id) DO UPDATE SET
                policy = excluded.policy,
                updated_at = excluded.updated_at",
            params![
                project_id,
                serde_json::to_string(&normalized)?,
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn has_critical_path_policy(&self, project_id: &str) -> Result<bool> {
        let conn = self.lock()?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM project_critical_path_policies WHERE project_id = ?1",
            params![project_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count > 0)
    }

    pub fn get_integration_policy(&self, project_id: &str) -> Result<IntegrationPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT auto_resolve, max_auto_attempts, max_resolve_tokens
                 FROM project_integration_policies WHERE project_id = ?1",
                params![project_id],
                |row| {
                    Ok(IntegrationPolicy {
                        auto_resolve: row.get::<_, i64>(0)? != 0,
                        max_auto_attempts: row.get::<_, i64>(1)? as u32,
                        max_resolve_tokens: row.get::<_, Option<i64>>(2)?.map(|v| v as u64),
                    })
                },
            )
            .optional()?;
        Ok(stored.unwrap_or_default())
    }

    pub fn set_integration_policy(
        &self,
        project_id: &str,
        policy: &IntegrationPolicy,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_integration_policies
                (project_id, auto_resolve, max_auto_attempts, max_resolve_tokens, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(project_id) DO UPDATE SET
                auto_resolve = excluded.auto_resolve,
                max_auto_attempts = excluded.max_auto_attempts,
                max_resolve_tokens = excluded.max_resolve_tokens,
                updated_at = excluded.updated_at",
            params![
                project_id,
                if policy.auto_resolve { 1 } else { 0 },
                policy.max_auto_attempts as i64,
                policy.max_resolve_tokens.map(|value| value as i64),
                now_rfc3339()
            ],
        )?;
        Ok(())
    }
}

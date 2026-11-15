//! Worktree retention policy.

use super::*;

impl Store {
    pub fn get_worktree_policy(&self, project_id: &str) -> Result<WorktreeRetentionPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT enabled, older_than_secs, delete_branches, keep_failed
                 FROM project_worktree_policies WHERE project_id = ?1",
                params![project_id],
                |row| {
                    Ok(WorktreeRetentionPolicy {
                        enabled: row.get::<_, i64>(0)? != 0,
                        older_than_secs: row.get::<_, i64>(1)? as u64,
                        delete_branches: row.get::<_, i64>(2)? != 0,
                        keep_failed: row.get::<_, i64>(3)? != 0,
                    })
                },
            )
            .optional()?;
        Ok(stored.unwrap_or_default())
    }

    pub fn set_worktree_policy(
        &self,
        project_id: &str,
        policy: &WorktreeRetentionPolicy,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_worktree_policies
             (project_id, enabled, older_than_secs, delete_branches, keep_failed)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(project_id) DO UPDATE SET
                enabled = excluded.enabled,
                older_than_secs = excluded.older_than_secs,
                delete_branches = excluded.delete_branches,
                keep_failed = excluded.keep_failed",
            params![
                project_id,
                if policy.enabled { 1 } else { 0 },
                policy.older_than_secs as i64,
                if policy.delete_branches { 1 } else { 0 },
                if policy.keep_failed { 1 } else { 0 }
            ],
        )?;
        Ok(())
    }

    pub fn list_enabled_worktree_policies(&self) -> Result<Vec<(String, WorktreeRetentionPolicy)>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT project_id, enabled, older_than_secs, delete_branches, keep_failed
             FROM project_worktree_policies WHERE enabled != 0",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get(0)?,
                WorktreeRetentionPolicy {
                    enabled: true,
                    older_than_secs: row.get::<_, i64>(2)? as u64,
                    delete_branches: row.get::<_, i64>(3)? != 0,
                    keep_failed: row.get::<_, i64>(4)? != 0,
                },
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

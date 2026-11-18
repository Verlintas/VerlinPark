use super::*;

impl Store {
    pub fn get_memory_policy(&self, project_id: &str) -> Result<MemoryPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT enabled, max_items, max_age_days, task_result_verify_days,
                        default_verify_days, auto_supersede, brief_max_chars, sweep_stale,
                        embedding_json
                 FROM project_memory_policies WHERE project_id = ?1",
                params![project_id],
                map_memory_policy_row,
            )
            .optional()?;
        Ok(stored.unwrap_or_default())
    }

    pub fn set_memory_policy(&self, project_id: &str, policy: &MemoryPolicy) -> Result<()> {
        let conn = self.lock()?;
        let embedding_json = match &policy.embedding {
            Some(embedding) => Some(serde_json::to_string(embedding)?),
            None => None,
        };
        conn.execute(
            "INSERT INTO project_memory_policies
             (project_id, enabled, max_items, max_age_days, task_result_verify_days,
              default_verify_days, auto_supersede, brief_max_chars, sweep_stale,
              embedding_json, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(project_id) DO UPDATE SET
                enabled = excluded.enabled,
                max_items = excluded.max_items,
                max_age_days = excluded.max_age_days,
                task_result_verify_days = excluded.task_result_verify_days,
                default_verify_days = excluded.default_verify_days,
                auto_supersede = excluded.auto_supersede,
                brief_max_chars = excluded.brief_max_chars,
                sweep_stale = excluded.sweep_stale,
                embedding_json = excluded.embedding_json,
                updated_at = excluded.updated_at",
            params![
                project_id,
                if policy.enabled { 1 } else { 0 },
                policy.max_items.map(i64::from),
                policy.max_age_days.map(i64::from),
                optional_count_to_db(policy.task_result_verify_days),
                optional_count_to_db(policy.default_verify_days),
                if policy.auto_supersede { 1 } else { 0 },
                optional_count_to_db(policy.brief_max_chars),
                if policy.sweep_stale { 1 } else { 0 },
                embedding_json,
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn list_enabled_memory_policies(&self) -> Result<Vec<(String, MemoryPolicy)>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT project_id, enabled, max_items, max_age_days, task_result_verify_days,
                    default_verify_days, auto_supersede, brief_max_chars, sweep_stale,
                    embedding_json
             FROM project_memory_policies WHERE enabled = 1",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                MemoryPolicy {
                    enabled: row.get::<_, i64>(1)? != 0,
                    max_items: row
                        .get::<_, Option<i64>>(2)?
                        .and_then(|value| u32::try_from(value).ok()),
                    max_age_days: row
                        .get::<_, Option<i64>>(3)?
                        .and_then(|value| u32::try_from(value).ok()),
                    task_result_verify_days: optional_count_from_db(row.get::<_, i64>(4)?),
                    default_verify_days: optional_count_from_db(row.get::<_, i64>(5)?),
                    auto_supersede: row.get::<_, i64>(6)? != 0,
                    brief_max_chars: optional_count_from_db(row.get::<_, i64>(7)?),
                    sweep_stale: row.get::<_, i64>(8)? != 0,
                    embedding: decode_memory_embedding_policy(row.get::<_, Option<String>>(9)?),
                },
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Projects that opted into daemon staleness sweeping. Independent of the
    /// retention (`enabled`) toggle.
    pub fn list_memory_sweep_policies(&self) -> Result<Vec<(String, MemoryPolicy)>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT project_id, enabled, max_items, max_age_days, task_result_verify_days,
                    default_verify_days, auto_supersede, brief_max_chars, sweep_stale,
                    embedding_json
             FROM project_memory_policies WHERE sweep_stale = 1",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                MemoryPolicy {
                    enabled: row.get::<_, i64>(1)? != 0,
                    max_items: row
                        .get::<_, Option<i64>>(2)?
                        .and_then(|value| u32::try_from(value).ok())
                        .filter(|value| *value > 0),
                    max_age_days: row
                        .get::<_, Option<i64>>(3)?
                        .and_then(|value| u32::try_from(value).ok())
                        .filter(|value| *value > 0),
                    task_result_verify_days: optional_count_from_db(row.get::<_, i64>(4)?),
                    default_verify_days: optional_count_from_db(row.get::<_, i64>(5)?),
                    auto_supersede: row.get::<_, i64>(6)? != 0,
                    brief_max_chars: optional_count_from_db(row.get::<_, i64>(7)?),
                    sweep_stale: row.get::<_, i64>(8)? != 0,
                    embedding: decode_memory_embedding_policy(row.get::<_, Option<String>>(9)?),
                },
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn memory_ids_for_prune(
        &self,
        project_id: &str,
        older_than_days: Option<u32>,
        keep_latest: Option<u32>,
    ) -> Result<Vec<String>> {
        if older_than_days.is_none() && keep_latest.is_none() {
            return Ok(Vec::new());
        }
        let cutoff = older_than_days
            .map(|days| time::OffsetDateTime::now_utc() - time::Duration::days(i64::from(days)));
        let conn = self.lock()?;
        // Pinning is explicit operator intent to retain a memory. Prune must
        // never silently delete a pinned row, and `keep_latest` counts only
        // unpinned rows so pinning one item cannot push another pinned item
        // out of the budget.
        let mut stmt = conn.prepare(
            "SELECT id, created_at FROM memories
             WHERE project_id = ?1 AND pinned = 0 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(params![project_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut ids = Vec::new();
        for (index, row) in rows.enumerate() {
            let (id, created_at) = row?;
            let beyond_keep = keep_latest.is_some_and(|keep| index >= keep as usize);
            let old = cutoff.is_some_and(|cutoff| {
                parse_rfc3339(&created_at).is_some_and(|created_at| created_at < cutoff)
            });
            if beyond_keep || old {
                ids.push(id);
            }
        }
        Ok(ids)
    }
}

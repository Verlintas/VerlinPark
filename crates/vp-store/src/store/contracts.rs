//! Task contracts and per-contract usage accounting.

use super::*;

impl Store {
    pub fn insert_contract(&self, row: &StoredContractRow) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO contracts
             (contract_id, project_id, session_id, task_id, plan_revision, role, hash,
              source_hash, contract, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(contract_id) DO UPDATE SET
                contract = excluded.contract,
                hash = excluded.hash,
                source_hash = excluded.source_hash",
            params![
                row.contract_id,
                row.project_id,
                row.session_id,
                row.task_id,
                row.plan_revision as i64,
                row.role,
                row.hash,
                row.source_hash,
                row.contract,
                row.created_at
            ],
        )?;
        Ok(())
    }

    pub fn list_contracts_for_session(&self, session_id: &str) -> Result<Vec<StoredContractRow>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT contract_id, project_id, session_id, task_id, plan_revision, role,
                    hash, source_hash, contract, created_at
             FROM contracts WHERE session_id = ?1
             ORDER BY created_at, contract_id",
        )?;
        let rows = statement
            .query_map(params![session_id], map_stored_contract)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn latest_contract(
        &self,
        session_id: &str,
        task_id: Option<&str>,
    ) -> Result<Option<StoredContractRow>> {
        let conn = self.lock()?;
        let row = match task_id {
            Some(task_id) => conn
                .query_row(
                    "SELECT contract_id, project_id, session_id, task_id, plan_revision, role,
                            hash, source_hash, contract, created_at
                     FROM contracts WHERE session_id = ?1 AND task_id = ?2
                     ORDER BY created_at DESC, contract_id DESC LIMIT 1",
                    params![session_id, task_id],
                    map_stored_contract,
                )
                .optional()?,
            None => conn
                .query_row(
                    "SELECT contract_id, project_id, session_id, task_id, plan_revision, role,
                            hash, source_hash, contract, created_at
                     FROM contracts WHERE session_id = ?1 AND task_id IS NULL
                     ORDER BY created_at DESC, contract_id DESC LIMIT 1",
                    params![session_id],
                    map_stored_contract,
                )
                .optional()?,
        };
        Ok(row)
    }

    pub fn replace_contracts(&self, session_id: &str, rows: &[StoredContractRow]) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM contracts WHERE session_id = ?1",
            params![session_id],
        )?;
        for row in rows {
            tx.execute(
                "INSERT INTO contracts
                 (contract_id, project_id, session_id, task_id, plan_revision, role, hash,
                  source_hash, contract, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    row.contract_id,
                    row.project_id,
                    row.session_id,
                    row.task_id,
                    row.plan_revision as i64,
                    row.role,
                    row.hash,
                    row.source_hash,
                    row.contract,
                    row.created_at
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get_contract_usage(&self, contract_id: &str) -> Result<ContractUsageRow> {
        let conn = self.lock()?;
        let row = conn
            .query_row(
                "SELECT contract_id, project_id, session_id, task_id, writes, bytes,
                        commits, updated_at
                 FROM contract_usage WHERE contract_id = ?1",
                params![contract_id],
                map_contract_usage,
            )
            .optional()?;
        Ok(row.unwrap_or_else(|| ContractUsageRow {
            contract_id: contract_id.to_string(),
            ..ContractUsageRow::default()
        }))
    }

    pub fn list_contract_usage_for_session(
        &self,
        session_id: &str,
    ) -> Result<Vec<ContractUsageRow>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT contract_id, project_id, session_id, task_id, writes, bytes, commits,
                    updated_at
             FROM contract_usage WHERE session_id = ?1 ORDER BY contract_id",
        )?;
        let rows = statement
            .query_map(params![session_id], map_contract_usage)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn latest_session_for_project(&self, project_id: &str) -> Result<Option<String>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id FROM sessions WHERE project_id = ?1
             ORDER BY created_at DESC, id DESC LIMIT 1",
            params![project_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_contract_usage(
        &self,
        contract_id: &str,
        project_id: &str,
        session_id: &str,
        task_id: Option<&str>,
        writes: u64,
        bytes: u64,
        commits: u64,
    ) -> Result<ContractUsageRow> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO contract_usage
             (contract_id, project_id, session_id, task_id, writes, bytes, commits, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(contract_id) DO UPDATE SET
                writes = contract_usage.writes + excluded.writes,
                bytes = contract_usage.bytes + excluded.bytes,
                commits = contract_usage.commits + excluded.commits,
                updated_at = excluded.updated_at",
            params![
                contract_id,
                project_id,
                session_id,
                task_id,
                writes as i64,
                bytes as i64,
                commits as i64,
                now_rfc3339()
            ],
        )?;
        let row = conn.query_row(
            "SELECT contract_id, project_id, session_id, task_id, writes, bytes,
                    commits, updated_at
             FROM contract_usage WHERE contract_id = ?1",
            params![contract_id],
            map_contract_usage,
        )?;
        Ok(row)
    }

    pub fn replace_contract_usage(
        &self,
        session_id: &str,
        rows: &[ContractUsageRow],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM contract_usage WHERE session_id = ?1",
            params![session_id],
        )?;
        for row in rows {
            tx.execute(
                "INSERT INTO contract_usage
                 (contract_id, project_id, session_id, task_id, writes, bytes, commits,
                  updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    row.contract_id,
                    row.project_id,
                    row.session_id,
                    row.task_id,
                    row.writes as i64,
                    row.bytes as i64,
                    row.commits as i64,
                    row.updated_at
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}

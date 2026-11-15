//! Spilled tool-output blobs and their retention.

use super::*;

impl Store {
    pub(super) fn tool_output_blob_dir(&self) -> PathBuf {
        self.data_dir.join("tool-outputs")
    }

    /// Resolve a validated content hash to the blob path. The only accepted
    /// input is a 64-character lowercase hex SHA-256; there is no user path
    /// parameter anywhere in this API.
    pub fn tool_output_blob_path(&self, sha256: &str) -> Result<PathBuf> {
        if sha256.len() != 64
            || !sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(anyhow!("invalid tool-output content hash"));
        }
        Ok(self.tool_output_blob_dir().join(format!("{sha256}.txt")))
    }

    pub(super) fn write_tool_output_blob(&self, sha256: &str, content: &[u8]) -> Result<PathBuf> {
        let dir = self.tool_output_blob_dir();
        if let Ok(metadata) = std::fs::symlink_metadata(&dir)
            && metadata.file_type().is_symlink()
        {
            return Err(anyhow!(
                "tool-output directory {} is a symlink; refusing to follow",
                dir.display()
            ));
        }
        paths::ensure_private_dir(&dir)
            .with_context(|| format!("creating tool-output directory {}", dir.display()))?;
        let path = self.tool_output_blob_path(sha256)?;
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(anyhow!(
                        "tool-output blob {} is not a regular file",
                        path.display()
                    ));
                }
                let existing = std::fs::read(&path).with_context(|| {
                    format!("reading existing tool-output blob {}", path.display())
                })?;
                let existing_sha = format!("{:x}", Sha256::digest(&existing));
                if existing_sha != sha256 {
                    return Err(anyhow!(
                        "tool-output blob {} content does not match its hash",
                        path.display()
                    ));
                }
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspecting tool-output blob {}", path.display()));
            }
        }
        let temp = dir.join(format!(".{sha256}.{}.tmp", Uuid::now_v7()));
        let result = (|| -> Result<()> {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options
                .open(&temp)
                .with_context(|| format!("creating tool-output blob {}", temp.display()))?;
            file.write_all(content)
                .with_context(|| format!("writing tool-output blob {}", temp.display()))?;
            file.sync_all()
                .with_context(|| format!("syncing tool-output blob {}", temp.display()))?;
            drop(file);
            paths::ensure_private_file(&temp)?;
            std::fs::rename(&temp, &path).with_context(|| {
                format!(
                    "renaming tool-output blob {} to {}",
                    temp.display(),
                    path.display()
                )
            })?;
            paths::ensure_private_file(&path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result.map(|()| path)
    }

    /// Persist one full tool output and its daemon-owned handle. The caller
    /// supplies a bounded preview; `content` is the exact full text that must
    /// survive until retention expires.
    #[allow(clippy::too_many_arguments)]
    pub fn spill_tool_output(
        &self,
        project_id: &str,
        session_id: &str,
        tool_call_id: Option<&str>,
        tool_name: &str,
        content: &str,
        preview: &str,
        retention_secs: u64,
    ) -> Result<StoredToolOutput> {
        if content.len() > MAX_TOOL_OUTPUT_BLOB_BYTES {
            return Err(anyhow!(
                "tool output is {} bytes; the store limit is {} bytes",
                content.len(),
                MAX_TOOL_OUTPUT_BLOB_BYTES
            ));
        }
        let sha256 = format!("{:x}", Sha256::digest(content.as_bytes()));
        self.write_tool_output_blob(&sha256, content.as_bytes())?;
        let created_at = now_rfc3339();
        let created = OffsetDateTime::parse(&created_at, &Rfc3339)
            .context("parsing tool-output creation timestamp")?;
        let expires = created + time::Duration::seconds(retention_secs.min(31_536_000) as i64);
        let expires_at = expires
            .format(&Rfc3339)
            .context("formatting tool-output expiry")?;
        let id = format!("out_{}", Uuid::now_v7());
        let lines = tool_output_line_count(content);
        let entry = vp_protocol::types::ToolOutputEntry {
            id: id.clone(),
            session_id: session_id.to_string(),
            tool_call_id: tool_call_id.map(str::to_string),
            tool_name: tool_name.to_string(),
            sha256,
            bytes: content.len() as u64,
            lines,
            preview_bytes: preview.len() as u64,
            created_at: created_at.clone(),
            expires_at: Some(expires_at.clone()),
        };
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO tool_outputs
             (id, project_id, session_id, tool_call_id, tool_name, sha256, bytes, lines,
              created_at, expires_at, preview)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                id,
                project_id,
                session_id,
                entry.tool_call_id,
                entry.tool_name,
                entry.sha256,
                entry.bytes as i64,
                entry.lines as i64,
                created_at,
                expires_at,
                preview
            ],
        )?;
        Ok(StoredToolOutput {
            entry,
            preview: preview.to_string(),
        })
    }

    /// Force one handle's expiry. This is a maintenance/test seam: normal
    /// runtime code always derives expiry from the policy at spill time.
    pub fn set_tool_output_expiry(&self, id: &str, expires_at: &str) -> Result<bool> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE tool_outputs SET expires_at = ?1 WHERE id = ?2",
            params![expires_at, id],
        )?;
        Ok(changed > 0)
    }

    pub fn get_tool_output(&self, id: &str) -> Result<Option<StoredToolOutput>> {
        if id.len() > 128 || id.is_empty() {
            return Ok(None);
        }
        let conn = self.lock()?;
        conn.query_row(
            &format!("SELECT {TOOL_OUTPUT_COLUMNS} FROM tool_outputs WHERE id = ?1"),
            params![id],
            map_tool_output,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Read and verify the full body for one handle. The caller must already
    /// have validated session ownership; this method never treats the handle
    /// as a path and re-checks the content hash before returning it.
    pub fn read_tool_output_text(&self, output: &StoredToolOutput) -> Result<String> {
        let path = self.tool_output_blob_path(&output.entry.sha256)?;
        let metadata = std::fs::symlink_metadata(&path)
            .with_context(|| format!("inspecting tool-output blob {}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(anyhow!(
                "tool-output blob {} is not a regular file",
                path.display()
            ));
        }
        if metadata.len() != output.entry.bytes {
            return Err(anyhow!(
                "tool-output blob {} size does not match metadata",
                path.display()
            ));
        }
        let bytes = std::fs::read(&path)
            .with_context(|| format!("reading tool-output blob {}", path.display()))?;
        let actual = format!("{:x}", Sha256::digest(&bytes));
        if actual != output.entry.sha256 {
            return Err(anyhow!(
                "tool-output blob {} content does not match its hash",
                path.display()
            ));
        }
        String::from_utf8(bytes).context("tool-output blob is not valid UTF-8")
    }

    #[allow(clippy::too_many_arguments)]
    pub fn list_tool_outputs(
        &self,
        project_id: Option<&str>,
        session_id: Option<&str>,
        tool_name: Option<&str>,
        before_created_at: Option<&str>,
        include_expired: bool,
        limit: usize,
    ) -> Result<Vec<StoredToolOutput>> {
        let now = now_rfc3339();
        let conn = self.lock()?;
        let limit = limit.clamp(1, 10_000) as i64;
        let mut stmt = conn.prepare(&format!(
            "SELECT {TOOL_OUTPUT_COLUMNS} FROM tool_outputs
             WHERE (?1 IS NULL OR project_id = ?1)
               AND (?2 IS NULL OR session_id = ?2)
               AND (?3 IS NULL OR tool_name = ?3)
               AND (?4 IS NULL OR created_at < ?4)
               AND (?5 = 1 OR expires_at IS NULL OR expires_at > ?6)
             ORDER BY created_at DESC, id DESC LIMIT ?7"
        ))?;
        let rows = stmt
            .query_map(
                params![
                    project_id,
                    session_id,
                    tool_name,
                    before_created_at,
                    if include_expired { 1 } else { 0 },
                    now,
                    limit
                ],
                map_tool_output,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn tool_output_total(
        &self,
        project_id: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<u64> {
        let conn = self.lock()?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM tool_outputs
             WHERE (?1 IS NULL OR project_id = ?1)
               AND (?2 IS NULL OR session_id = ?2)",
            params![project_id, session_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count.max(0) as u64)
    }

    pub fn tool_output_stats(
        &self,
        project_id: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<ToolOutputStats> {
        let now = now_rfc3339();
        let conn = self.lock()?;
        let mut stats = ToolOutputStats::default();
        conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(t.bytes), 0), COALESCE(SUM(length(t.preview)), 0),
                    COALESCE(SUM(CASE WHEN t.expires_at IS NOT NULL AND t.expires_at <= ?3
                                      THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN s.status IN ('queued', 'running', 'paused', 'waiting_input')
                                      THEN 1 ELSE 0 END), 0),
                    MIN(t.created_at), MAX(t.created_at)
             FROM tool_outputs t JOIN sessions s ON s.id = t.session_id
             WHERE (?1 IS NULL OR t.project_id = ?1)
               AND (?2 IS NULL OR t.session_id = ?2)",
            params![project_id, session_id, now],
            |row| {
                stats.entries = row.get::<_, i64>(0)?.max(0) as u64;
                stats.total_bytes = row.get::<_, i64>(1)?.max(0) as u64;
                stats.preview_bytes = row.get::<_, i64>(2)?.max(0) as u64;
                stats.expired_entries = row.get::<_, i64>(3)?.max(0) as u64;
                stats.active_entries = row.get::<_, i64>(4)?.max(0) as u64;
                stats.oldest_created_at = row.get(5)?;
                stats.newest_created_at = row.get(6)?;
                Ok(())
            },
        )?;
        Ok(stats)
    }

    pub fn tool_output_tool_stats(
        &self,
        project_id: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<Vec<ToolOutputToolStat>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT tool_name, COUNT(*), COALESCE(SUM(bytes), 0),
                    COALESCE(SUM(length(preview)), 0)
             FROM tool_outputs
             WHERE (?1 IS NULL OR project_id = ?1)
               AND (?2 IS NULL OR session_id = ?2)
             GROUP BY tool_name
             ORDER BY tool_name ASC",
        )?;
        let rows = stmt
            .query_map(params![project_id, session_id], |row| {
                Ok(ToolOutputToolStat {
                    tool_name: row.get(0)?,
                    entries: row.get::<_, i64>(1)?.max(0) as u64,
                    bytes: row.get::<_, i64>(2)?.max(0) as u64,
                    preview_bytes: row.get::<_, i64>(3)?.max(0) as u64,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn prune_tool_outputs(
        &self,
        request: &ToolOutputPruneRequest,
    ) -> Result<vp_protocol::types::ToolOutputPruneResult> {
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        struct Candidate {
            id: String,
            sha256: String,
            bytes: u64,
            created_at: String,
            expires_at: Option<String>,
            active: bool,
        }
        let mut stmt = tx.prepare(
            "SELECT t.id, t.sha256, t.bytes, t.created_at, t.expires_at,
                    CASE WHEN s.status IN ('queued', 'running', 'paused', 'waiting_input')
                         THEN 1 ELSE 0 END
             FROM tool_outputs t JOIN sessions s ON s.id = t.session_id
             WHERE (?1 IS NULL OR t.project_id = ?1)
               AND (?2 IS NULL OR t.session_id = ?2)
             ORDER BY t.created_at ASC, t.id ASC",
        )?;
        let rows = stmt
            .query_map(params![request.project_id, request.session_id], |row| {
                Ok(Candidate {
                    id: row.get(0)?,
                    sha256: row.get(1)?,
                    bytes: row.get::<_, i64>(2)?.max(0) as u64,
                    created_at: row.get(3)?,
                    expires_at: row.get(4)?,
                    active: row.get::<_, i64>(5)? != 0,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        let cutoff = request
            .older_than_secs
            .map(|seconds| {
                let now = OffsetDateTime::parse(&now, &Rfc3339)?;
                let cutoff = now - time::Duration::seconds(seconds.min(31_536_000) as i64);
                cutoff
                    .format(&Rfc3339)
                    .context("formatting tool-output cutoff")
            })
            .transpose()?;
        let mut candidates = std::collections::BTreeSet::new();
        let mut overflow_candidates = std::collections::BTreeSet::new();
        let mut expired_removed = 0u64;
        let protected_active = rows.iter().filter(|row| row.active).count() as u64;
        for row in &rows {
            let expired = row
                .expires_at
                .as_deref()
                .is_some_and(|expires| expires <= now.as_str());
            if expired {
                candidates.insert(row.id.clone());
                expired_removed += 1;
                continue;
            }
            if row.active {
                continue;
            }
            if cutoff
                .as_deref()
                .is_some_and(|cutoff| row.created_at.as_str() <= cutoff)
            {
                candidates.insert(row.id.clone());
            }
        }
        if let Some(max_entries) = request.max_entries {
            let remaining = rows
                .iter()
                .filter(|row| !candidates.contains(&row.id))
                .collect::<Vec<_>>();
            if remaining.len() as u64 > max_entries {
                let mut keep = remaining
                    .iter()
                    .map(|row| (row.created_at.clone(), row.id.clone()))
                    .collect::<Vec<_>>();
                // Newest-first keeps the cap useful; creation time and id are
                // deterministic tie-breakers.
                keep.sort_by(|left, right| right.cmp(left));
                let keep = keep
                    .into_iter()
                    .take(max_entries as usize)
                    .map(|(_, id)| id)
                    .collect::<std::collections::BTreeSet<_>>();
                for row in remaining {
                    if !keep.contains(&row.id) && !row.active {
                        candidates.insert(row.id.clone());
                        overflow_candidates.insert(row.id.clone());
                    }
                }
            }
        }
        let removed_rows = rows
            .iter()
            .filter(|row| candidates.contains(&row.id))
            .collect::<Vec<_>>();
        let overflow_removed = overflow_candidates
            .iter()
            .filter(|id| candidates.contains(*id))
            .count() as u64;
        let removed_bytes = removed_rows
            .iter()
            .fold(0u64, |total, row| total.saturating_add(row.bytes));
        if !request.dry_run {
            for row in &removed_rows {
                tx.execute("DELETE FROM tool_outputs WHERE id = ?1", params![row.id])?;
            }
        }
        let result = vp_protocol::types::ToolOutputPruneResult {
            project_id: request.project_id.clone(),
            session_id: request.session_id.clone(),
            candidates: removed_rows.len() as u64,
            removed: if request.dry_run {
                0
            } else {
                removed_rows.len() as u64
            },
            removed_bytes: if request.dry_run { 0 } else { removed_bytes },
            expired_removed,
            overflow_removed,
            protected_active,
            dry_run: request.dry_run,
        };
        tx.commit()?;
        if !request.dry_run {
            let mut checked = std::collections::BTreeSet::new();
            for row in &removed_rows {
                if !checked.insert(row.sha256.clone()) {
                    continue;
                }
                let references: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM tool_outputs WHERE sha256 = ?1",
                    params![row.sha256],
                    |row| row.get(0),
                )?;
                if references == 0
                    && let Ok(path) = self.tool_output_blob_path(&row.sha256)
                    && let Ok(metadata) = std::fs::symlink_metadata(&path)
                    && metadata.is_file()
                    && !metadata.file_type().is_symlink()
                {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
        Ok(result)
    }

    pub fn get_tool_output_policy(&self, project_id: &str) -> Result<ToolOutputPolicy> {
        let conn = self.lock()?;
        let stored: Option<String> = conn
            .query_row(
                "SELECT policy FROM project_tool_output_policies WHERE project_id = ?1",
                params![project_id],
                |row| row.get(0),
            )
            .optional()?;
        match stored {
            Some(policy) => {
                serde_json::from_str(&policy).context("parsing stored tool-output policy")
            }
            None => Ok(ToolOutputPolicy::default()),
        }
    }

    pub fn set_tool_output_policy(
        &self,
        project_id: &str,
        policy: &ToolOutputPolicy,
    ) -> Result<()> {
        let text = serde_json::to_string(policy).context("serializing tool-output policy")?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_tool_output_policies (project_id, policy, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(project_id) DO UPDATE SET
                policy = excluded.policy,
                updated_at = excluded.updated_at",
            params![project_id, text, now_rfc3339()],
        )?;
        Ok(())
    }

    /// Verify the metadata/blob boundary. Metadata is never deleted here;
    /// optional cleanup removes only unreferenced, hash-shaped blob files.
    pub fn audit_tool_outputs(
        &self,
        project_id: Option<&str>,
        verify_hashes: bool,
        clean_orphans: bool,
    ) -> Result<ToolOutputAuditResult> {
        let checked_at = now_rfc3339();
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT sha256, bytes FROM tool_outputs
             WHERE (?1 IS NULL OR project_id = ?1)
             ORDER BY sha256 ASC",
        )?;
        let rows = stmt
            .query_map(params![project_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?.max(0) as u64,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        let mut all_hashes = std::collections::BTreeSet::new();
        {
            let mut stmt = conn.prepare("SELECT DISTINCT sha256 FROM tool_outputs")?;
            let hashes = stmt.query_map([], |row| row.get::<_, String>(0))?;
            for hash in hashes {
                all_hashes.insert(hash?);
            }
        }
        let mut audit = ToolOutputAuditResult {
            project_id: project_id.map(str::to_string),
            rows: rows.len() as u64,
            checked_at,
            ..ToolOutputAuditResult::default()
        };
        let dir = self.tool_output_blob_dir();
        let mut referenced = std::collections::BTreeSet::new();
        for (sha256, bytes) in &rows {
            referenced.insert(sha256.clone());
            let Ok(path) = self.tool_output_blob_path(sha256) else {
                audit.missing_blobs += 1;
                continue;
            };
            match std::fs::symlink_metadata(&path) {
                Ok(metadata) => {
                    if metadata.file_type().is_symlink() || !metadata.is_file() {
                        audit.unsafe_blob_files += 1;
                        continue;
                    }
                    if metadata.len() != *bytes {
                        audit.hash_mismatches += 1;
                        continue;
                    }
                    if verify_hashes {
                        let data = std::fs::read(&path).with_context(|| {
                            format!("reading tool-output blob {}", path.display())
                        })?;
                        let actual = format!("{:x}", Sha256::digest(&data));
                        if actual != *sha256 {
                            audit.hash_mismatches += 1;
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    audit.missing_blobs += 1;
                }
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("inspecting tool-output blob {}", path.display())
                    });
                }
            }
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => Some(entries),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("reading tool-output directory {}", dir.display()));
            }
        };
        if let Some(entries) = entries {
            for entry in entries {
                let entry = entry?;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                let Some(hash) = name.strip_suffix(".txt") else {
                    continue;
                };
                if hash.len() != 64
                    || !hash
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                {
                    audit.unsafe_blob_files += 1;
                    continue;
                }
                let file_type = entry.file_type()?;
                if file_type.is_symlink() || !file_type.is_file() {
                    audit.unsafe_blob_files += 1;
                    continue;
                }
                let metadata = entry.metadata()?;
                audit.blob_files += 1;
                if !all_hashes.contains(hash) {
                    audit.orphan_blobs += 1;
                    audit.orphan_bytes = audit.orphan_bytes.saturating_add(metadata.len());
                    // A concurrent spill writes the blob just before inserting
                    // its metadata row. Do not delete a fresh unreferenced file:
                    // it may be in that narrow window. Old crash leftovers are
                    // safe to remove.
                    let old_enough = metadata
                        .modified()
                        .ok()
                        .and_then(|modified| modified.elapsed().ok())
                        .is_some_and(|age| age >= std::time::Duration::from_secs(300));
                    if clean_orphans && old_enough {
                        let path = entry.path();
                        std::fs::remove_file(&path).with_context(|| {
                            format!("removing orphan tool-output blob {}", path.display())
                        })?;
                        audit.cleaned_orphans += 1;
                        audit.cleaned_bytes = audit.cleaned_bytes.saturating_add(metadata.len());
                    }
                }
            }
        }
        Ok(audit)
    }
}

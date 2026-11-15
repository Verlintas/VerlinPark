//! Session merge queue and remote merge visibility.

use super::*;

impl Store {
    /// Attach a supplemental evidence digest to a queue row. This deliberately
    /// does not touch `verifier_approved`, `status`, or `commit`.
    pub fn set_merge_queue_remote_test_with_event(
        &self,
        entry_id: &str,
        evidence_id: &str,
        digest: &str,
        status: &str,
    ) -> Result<Option<(MergeQueueEntry, Event)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE merge_queue
             SET remote_test_evidence_id = ?1, remote_test_digest = ?2,
                 remote_test_status = ?3, updated_at = ?4
             WHERE id = ?5 AND source = 'remote'",
            params![evidence_id, digest, status, now_rfc3339(), entry_id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let entry = tx
            .query_row(
                "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash,
                        base_commit, pre_merge_commit, status, attempt, detail, source,
                        remote_host, remote_branch, bundle_sha256, imported_ref,
                        verifier_approved, verifier_summary, environment_caveat,
                        remote_test_evidence_id, remote_test_digest, remote_test_status,
                        created_at, updated_at
                 FROM merge_queue WHERE id = ?1",
                params![entry_id],
                map_merge_queue_entry,
            )
            .optional()?;
        let Some(entry) = entry else {
            return Ok(None);
        };
        let event = insert_event_tx(
            &tx,
            &entry.session_id,
            "merge_queue.updated",
            serde_json::json!({
                "entry": entry,
                "reason": "remote_test_evidence_attached",
                "supplemental": true,
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((entry, event)))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn enqueue_merge(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        task_id: Option<&str>,
        branch: &str,
        commit: &str,
        base_commit: &str,
        detail: Option<&str>,
    ) -> Result<MergeQueueEntry> {
        self.enqueue_merge_with_source(
            session_id,
            agent_id,
            agent_name,
            task_id,
            branch,
            commit,
            base_commit,
            detail,
            &MergeQueueSource::local(),
        )
    }

    /// Enqueue an entry with explicit provenance. Remote integration uses this
    /// to persist the host, branch, bundle hash, and temporary imported ref on
    /// the same row that the deterministic Integrator later merges.
    #[allow(clippy::too_many_arguments)]
    pub fn enqueue_merge_with_source(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        task_id: Option<&str>,
        branch: &str,
        commit: &str,
        base_commit: &str,
        detail: Option<&str>,
        source: &MergeQueueSource,
    ) -> Result<MergeQueueEntry> {
        let id = Uuid::now_v7().to_string();
        let now = now_rfc3339();
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO merge_queue
             (id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, pre_merge_commit, status, attempt, detail, source, remote_host, remote_branch, bundle_sha256, imported_ref, verifier_approved, verifier_summary, environment_caveat, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, 'queued', 1, ?9, ?10, ?11, ?12, ?13, ?14, NULL, NULL, NULL, ?15, ?15)",
            params![
                id,
                session_id,
                agent_id,
                agent_name,
                task_id,
                branch,
                commit,
                base_commit,
                detail,
                source.source,
                source.remote_host,
                source.remote_branch,
                source.bundle_sha256,
                source.imported_ref,
                now,
            ],
        )?;
        conn.query_row(
            "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, pre_merge_commit, status, attempt, detail, source, remote_host, remote_branch, bundle_sha256, imported_ref, verifier_approved, verifier_summary, environment_caveat, remote_test_evidence_id, remote_test_digest, remote_test_status, created_at, updated_at
             FROM merge_queue WHERE id = ?1",
            params![id],
            map_merge_queue_entry,
        )
        .map_err(Into::into)
    }

    pub fn update_merge_queue_entry(
        &self,
        id: &str,
        status: &str,
        detail: Option<&str>,
    ) -> Result<Option<MergeQueueEntry>> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE merge_queue
             SET status = ?1, detail = COALESCE(?2, detail), updated_at = ?3
             WHERE id = ?4",
            params![status, detail, now_rfc3339(), id],
        )?;
        conn.query_row(
            "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, pre_merge_commit, status, attempt, detail, source, remote_host, remote_branch, bundle_sha256, imported_ref, verifier_approved, verifier_summary, environment_caveat, remote_test_evidence_id, remote_test_digest, remote_test_status, created_at, updated_at
             FROM merge_queue WHERE id = ?1",
            params![id],
            map_merge_queue_entry,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Attach the Verifier verdict and environment caveat to a remote entry.
    /// Emits a full-entry update so replay/repair reconstructs the verdict
    /// without querying agent events.
    pub fn set_merge_queue_verification_with_event(
        &self,
        id: &str,
        approved: Option<bool>,
        summary: Option<&str>,
        environment_caveat: Option<&str>,
    ) -> Result<Option<(MergeQueueEntry, Event)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE merge_queue
             SET verifier_approved = ?1, verifier_summary = ?2,
                 environment_caveat = ?3, updated_at = ?4
             WHERE id = ?5",
            params![
                approved.map(i64::from),
                summary,
                environment_caveat,
                now_rfc3339(),
                id
            ],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let entry = tx
            .query_row(
                "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, pre_merge_commit, status, attempt, detail, source, remote_host, remote_branch, bundle_sha256, imported_ref, verifier_approved, verifier_summary, environment_caveat, remote_test_evidence_id, remote_test_digest, remote_test_status, created_at, updated_at
                 FROM merge_queue WHERE id = ?1",
                params![id],
                map_merge_queue_entry,
            )
            .optional()?;
        let Some(entry) = entry else {
            return Ok(None);
        };
        let event = insert_event_tx(
            &tx,
            &entry.session_id,
            "merge_queue.updated",
            serde_json::json!({"entry": entry, "reason": "verification_recorded"}),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((entry, event)))
    }

    /// Mark a queue entry as merging and persist the exact pre-merge HEAD.
    pub fn begin_merge_queue_entry(
        &self,
        id: &str,
        pre_merge_commit: &str,
    ) -> Result<Option<MergeQueueEntry>> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE merge_queue
             SET status = 'merging', pre_merge_commit = ?1, updated_at = ?2
             WHERE id = ?3",
            params![pre_merge_commit, now_rfc3339(), id],
        )?;
        conn.query_row(
            "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, pre_merge_commit, status, attempt, detail, source, remote_host, remote_branch, bundle_sha256, imported_ref, verifier_approved, verifier_summary, environment_caveat, remote_test_evidence_id, remote_test_digest, remote_test_status, created_at, updated_at
             FROM merge_queue WHERE id = ?1",
            params![id],
            map_merge_queue_entry,
        )
        .optional()
        .map_err(Into::into)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn enqueue_merge_with_event(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        task_id: Option<&str>,
        branch: &str,
        commit: &str,
        base_commit: &str,
        detail: Option<&str>,
    ) -> Result<(MergeQueueEntry, Event)> {
        self.enqueue_merge_with_event_source(
            session_id,
            agent_id,
            agent_name,
            task_id,
            branch,
            commit,
            base_commit,
            detail,
            &MergeQueueSource::local(),
        )
    }

    /// Event-backed enqueue with explicit provenance. The complete entry,
    /// including additive source fields, is embedded in `merge_queue.enqueued`
    /// so replay/repair reconstructs it without a second event.
    #[allow(clippy::too_many_arguments)]
    pub fn enqueue_merge_with_event_source(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        task_id: Option<&str>,
        branch: &str,
        commit: &str,
        base_commit: &str,
        detail: Option<&str>,
        source: &MergeQueueSource,
    ) -> Result<(MergeQueueEntry, Event)> {
        let id = Uuid::now_v7().to_string();
        let now = now_rfc3339();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO merge_queue
             (id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, pre_merge_commit, status, attempt, detail, source, remote_host, remote_branch, bundle_sha256, imported_ref, verifier_approved, verifier_summary, environment_caveat, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, 'queued', 1, ?9, ?10, ?11, ?12, ?13, ?14, NULL, NULL, NULL, ?15, ?15)",
            params![
                id,
                session_id,
                agent_id,
                agent_name,
                task_id,
                branch,
                commit,
                base_commit,
                detail,
                source.source,
                source.remote_host,
                source.remote_branch,
                source.bundle_sha256,
                source.imported_ref,
                now,
            ],
        )?;
        let entry = tx.query_row(
            "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, pre_merge_commit, status, attempt, detail, source, remote_host, remote_branch, bundle_sha256, imported_ref, verifier_approved, verifier_summary, environment_caveat, remote_test_evidence_id, remote_test_digest, remote_test_status, created_at, updated_at
             FROM merge_queue WHERE id = ?1",
            params![id],
            map_merge_queue_entry,
        )?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "merge_queue.enqueued",
            serde_json::json!({"entry": entry}),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((entry, event))
    }

    pub fn update_merge_queue_entry_with_event(
        &self,
        id: &str,
        status: &str,
        detail: Option<&str>,
    ) -> Result<Option<(MergeQueueEntry, Event)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE merge_queue
             SET status = ?1, detail = COALESCE(?2, detail), updated_at = ?3
             WHERE id = ?4",
            params![status, detail, now_rfc3339(), id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let entry = tx
            .query_row(
                "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, pre_merge_commit, status, attempt, detail, source, remote_host, remote_branch, bundle_sha256, imported_ref, verifier_approved, verifier_summary, environment_caveat, remote_test_evidence_id, remote_test_digest, remote_test_status, created_at, updated_at
                 FROM merge_queue WHERE id = ?1",
                params![id],
                map_merge_queue_entry,
            )
            .optional()?;
        let Some(entry) = entry else {
            return Ok(None);
        };
        let event = insert_event_tx(
            &tx,
            &entry.session_id,
            "merge_queue.updated",
            serde_json::json!({"entry": entry}),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((entry, event)))
    }

    pub fn retry_merge_queue_entry_with_event(
        &self,
        id: &str,
        commit: &str,
        base_commit: &str,
        detail: Option<&str>,
    ) -> Result<Option<(MergeQueueEntry, Event)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE merge_queue
             SET status = 'queued', attempt = attempt + 1, commit_hash = ?1,
                 base_commit = ?2, detail = ?3, updated_at = ?4
             WHERE id = ?5 AND status IN ('conflict', 'failed', 'interrupted')",
            params![commit, base_commit, detail, now_rfc3339(), id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let entry = tx
            .query_row(
                "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, pre_merge_commit, status, attempt, detail, source, remote_host, remote_branch, bundle_sha256, imported_ref, verifier_approved, verifier_summary, environment_caveat, remote_test_evidence_id, remote_test_digest, remote_test_status, created_at, updated_at
                 FROM merge_queue WHERE id = ?1",
                params![id],
                map_merge_queue_entry,
            )
            .optional()?;
        let Some(entry) = entry else {
            return Ok(None);
        };
        let event = insert_event_tx(
            &tx,
            &entry.session_id,
            "merge_queue.updated",
            serde_json::json!({"entry": entry}),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((entry, event)))
    }

    /// Interrupted integration entries that still have a recorded pre-merge
    /// HEAD. The engine uses this to restore a clean session worktree after a
    /// crash before the merge status was finalized.
    pub fn interrupted_merge_recoveries(&self) -> Result<Vec<MergeQueueEntry>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash,
                    base_commit, pre_merge_commit, status, attempt, detail, source, remote_host, remote_branch, bundle_sha256, imported_ref, verifier_approved, verifier_summary, environment_caveat, remote_test_evidence_id, remote_test_digest, remote_test_status, created_at, updated_at
             FROM merge_queue
             WHERE status = 'interrupted' AND pre_merge_commit IS NOT NULL
             ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt.query_map([], map_merge_queue_entry)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn list_merge_queue(&self, session_id: &str) -> Result<Vec<MergeQueueEntry>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, pre_merge_commit, status, attempt, detail, source, remote_host, remote_branch, bundle_sha256, imported_ref, verifier_approved, verifier_summary, environment_caveat, remote_test_evidence_id, remote_test_digest, remote_test_status, created_at, updated_at
             FROM merge_queue WHERE session_id = ?1 ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![session_id], map_merge_queue_entry)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Remote-sourced queue rows for a project, newest first. Local rows are
    /// excluded even if a caller later filters by session.
    pub fn list_remote_merge_queue(
        &self,
        project_id: &str,
        session_id: Option<&str>,
    ) -> Result<Vec<MergeQueueEntry>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT m.id, m.session_id, m.agent_id, m.agent_name, m.task_id, m.branch,
                    m.commit_hash, m.base_commit, m.pre_merge_commit, m.status, m.attempt,
                    m.detail, m.source, m.remote_host, m.remote_branch, m.bundle_sha256,
                    m.imported_ref, m.verifier_approved, m.verifier_summary,
                    m.environment_caveat, m.remote_test_evidence_id,
                    m.remote_test_digest, m.remote_test_status,
                    m.created_at, m.updated_at
             FROM merge_queue m
             JOIN sessions s ON s.id = m.session_id
             WHERE s.project_id = ?1
               AND m.source = 'remote'
               AND (?2 IS NULL OR m.session_id = ?2)
             ORDER BY m.created_at DESC, m.id DESC
             LIMIT 500",
        )?;
        let rows = stmt.query_map(params![project_id, session_id], map_merge_queue_entry)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_merge_queue_entry(&self, id: &str) -> Result<Option<MergeQueueEntry>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, pre_merge_commit, status, attempt, detail, source, remote_host, remote_branch, bundle_sha256, imported_ref, verifier_approved, verifier_summary, environment_caveat, remote_test_evidence_id, remote_test_digest, remote_test_status, created_at, updated_at
             FROM merge_queue WHERE id = ?1",
            params![id],
            map_merge_queue_entry,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn retry_merge_queue_entry(
        &self,
        id: &str,
        commit: &str,
        base_commit: &str,
        detail: Option<&str>,
    ) -> Result<Option<MergeQueueEntry>> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE merge_queue
             SET status = 'queued', attempt = attempt + 1, commit_hash = ?1,
                 base_commit = ?2, detail = ?3, updated_at = ?4
             WHERE id = ?5 AND status IN ('conflict', 'failed', 'interrupted')",
            params![commit, base_commit, detail, now_rfc3339(), id],
        )?;
        drop(conn);
        self.get_merge_queue_entry(id)
    }

    pub fn interrupt_merge_queue(&self) -> Result<Vec<MergeQueueEntry>> {
        let conn = self.lock()?;
        let ids = {
            let mut stmt = conn.prepare(
                "SELECT id FROM merge_queue WHERE status IN ('queued', 'merging', 'checking')",
            )?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        conn.execute(
            "UPDATE merge_queue
             SET status = 'interrupted',
                 detail = 'daemon restarted during integration',
                 updated_at = ?1
             WHERE status IN ('queued', 'merging', 'checking')",
            params![now_rfc3339()],
        )?;
        let mut entries = Vec::with_capacity(ids.len());
        for id in ids {
            entries.push(conn.query_row(
                "SELECT id, session_id, agent_id, agent_name, task_id, branch, commit_hash, base_commit, pre_merge_commit, status, attempt, detail, source, remote_host, remote_branch, bundle_sha256, imported_ref, verifier_approved, verifier_summary, environment_caveat, remote_test_evidence_id, remote_test_digest, remote_test_status, created_at, updated_at
                 FROM merge_queue WHERE id = ?1",
                params![id],
                map_merge_queue_entry,
            )?);
        }
        Ok(entries)
    }
}

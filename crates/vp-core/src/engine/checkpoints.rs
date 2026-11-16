use super::*;

impl Engine {
    pub async fn checkpoint_session(
        &self,
        session_id: &str,
        label: Option<String>,
    ) -> Result<SessionCheckpoint> {
        self.checkpoint_session_inner(session_id, label, false)
            .await
    }

    /// Checkpoint at a hard-preemption boundary. The run loop has already
    /// stopped provider/tool work and is the only writer, so the normal
    /// running/paused quiescence gate is intentionally bypassed. This is not a
    /// general-purpose escape hatch and is not exposed over RPC.
    pub(crate) async fn checkpoint_session_at_preemption(
        &self,
        session_id: &str,
        label: Option<String>,
    ) -> Result<SessionCheckpoint> {
        self.checkpoint_session_inner(session_id, label, true).await
    }

    async fn checkpoint_session_inner(
        &self,
        session_id: &str,
        label: Option<String>,
        preemption_safe: bool,
    ) -> Result<SessionCheckpoint> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        if !preemption_safe {
            ensure_session_quiescent(&session, "create a checkpoint")?;
        }
        let worktree = PathBuf::from(
            session
                .worktree
                .clone()
                .ok_or_else(|| anyhow::anyhow!("session has no worktree"))?,
        );
        if !worktree.is_dir() {
            anyhow::bail!("session worktree is missing: {}", worktree.display());
        }
        let checkpoint_id = Uuid::now_v7().to_string();
        let label = label
            .map(|label| {
                label
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(120)
                    .collect::<String>()
            })
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| format!("checkpoint {}", &checkpoint_id[..8]));
        let git_lock = self.git_lock.clone();
        let status_path = worktree.clone();
        let branch_path = worktree.clone();
        let snapshot_path = worktree.clone();
        let data_dir = self.store.data_dir().to_path_buf();
        let session_id_for_snapshot = session.id.clone();
        let checkpoint_id_for_snapshot = checkpoint_id.clone();
        let label_for_snapshot = label.clone();
        let expected_branch = session.branch.clone();
        let (commit, status) = tokio::task::spawn_blocking(move || {
            let _guard = git_lock
                .lock()
                .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
            let status = worktree::status_porcelain(&status_path)?;
            let branch = worktree::current_branch(&branch_path)?;
            if let Some(expected) = expected_branch.as_deref()
                && branch != expected
            {
                anyhow::bail!(
                    "session worktree is on branch {branch}; expected {expected}. Check it out before creating a checkpoint"
                );
            }
            let commit = worktree::snapshot_worktree(
                &snapshot_path,
                &data_dir,
                &session_id_for_snapshot,
                &checkpoint_id_for_snapshot,
                &label_for_snapshot,
            )?;
            Ok::<_, anyhow::Error>((commit, status))
        })
        .await??;
        let dirty = !status.is_empty();
        let (messages, start, summary) = self.load_active_snapshot(session_id).await?;
        let start_message_id = messages.get(start).map(|message| message.id.clone());
        let through_message_id = if start < messages.len() {
            messages.last().map(|message| message.id.clone())
        } else {
            None
        };
        let message_count = messages.len().saturating_sub(start);
        let payload = serde_json::json!({
            "checkpoint_id": checkpoint_id,
            "label": label,
            "commit": commit,
            "branch": session.branch,
            "worktree": session.worktree,
            "start_message_id": start_message_id,
            "through_message_id": through_message_id,
            "message_count": message_count,
            "summary": summary,
            "dirty": dirty,
        });
        let event = self
            .append_event(session_id, "session.checkpoint", payload)
            .await?;
        checkpoint_from_event(&event)
            .ok_or_else(|| anyhow::anyhow!("could not decode checkpoint event"))
    }

    pub async fn list_checkpoints(&self, session_id: &str) -> Result<SessionCheckpointListResult> {
        Ok(SessionCheckpointListResult {
            checkpoints: self.active_checkpoints(session_id).await?,
        })
    }

    pub(crate) async fn active_checkpoints(
        &self,
        session_id: &str,
    ) -> Result<Vec<SessionCheckpoint>> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let (created, deleted) = tokio::task::spawn_blocking(move || {
            Ok::<_, anyhow::Error>((
                store.list_events_by_kind(&id, "session.checkpoint", 1000)?,
                store.list_events_by_kind(&id, "session.checkpoint.deleted", 1000)?,
            ))
        })
        .await??;
        let deleted_ids = deleted
            .iter()
            .filter_map(|event| {
                event
                    .payload
                    .get("checkpoint_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .collect::<std::collections::HashSet<_>>();
        Ok(created
            .iter()
            .filter_map(checkpoint_from_event)
            .filter(|checkpoint| !deleted_ids.contains(&checkpoint.id))
            .collect())
    }

    async fn ensure_checkpoint_mutable(&self, session_id: &str) -> Result<()> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        if session.status == "imported" {
            anyhow::bail!("imported sessions are read-only archives");
        }
        Ok(())
    }

    pub async fn checkpoint_stats(&self, session_id: &str) -> Result<SessionCheckpointStatsResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let (session, project) = tokio::task::spawn_blocking(move || {
            let session = store
                .get_session(&id)?
                .ok_or_else(|| anyhow::anyhow!("session not found: {id}"))?;
            let project = store
                .get_project(&session.project_id)?
                .ok_or_else(|| anyhow::anyhow!("project not found: {}", session.project_id))?;
            Ok::<_, anyhow::Error>((session, project))
        })
        .await??;
        let checkpoints = self.active_checkpoints(session_id).await?;
        let dirty = checkpoints
            .iter()
            .filter(|checkpoint| checkpoint.dirty)
            .count();
        let project_root = PathBuf::from(&project.root);
        let git_lock = self.git_lock.clone();
        let id = session.id.clone();
        let checkpoints_for_git = checkpoints.clone();
        let (commit_bytes, snapshot_bytes, unique_blobs, refs) =
            tokio::task::spawn_blocking(move || -> Result<_> {
                let _guard = git_lock
                    .lock()
                    .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
                let mut commit_bytes = 0u64;
                let mut snapshot_bytes = 0u64;
                let mut seen = std::collections::HashSet::new();
                for checkpoint in &checkpoints_for_git {
                    if let Ok(size) =
                        worktree::git_output(&project_root, &["cat-file", "-s", &checkpoint.commit])
                    {
                        commit_bytes =
                            commit_bytes.saturating_add(size.trim().parse().unwrap_or(0));
                    }
                    let parent = format!("{}^", checkpoint.commit);
                    if let Ok(parent) = worktree::git_output(&project_root, &["rev-parse", &parent])
                    {
                        let range = format!("{parent}..{}", checkpoint.commit);
                        if let Ok(paths) =
                            worktree::git_output(&project_root, &["diff", "--name-only", &range])
                        {
                            for path in paths.lines().filter(|path| !path.trim().is_empty()) {
                                let spec = format!("{}:{}", checkpoint.commit, path);
                                if let Ok(blob) =
                                    worktree::git_output(&project_root, &["rev-parse", &spec])
                                    && seen.insert(blob.clone())
                                    && let Ok(size) = worktree::git_output(
                                        &project_root,
                                        &["cat-file", "-s", &blob],
                                    )
                                {
                                    snapshot_bytes = snapshot_bytes
                                        .saturating_add(size.trim().parse().unwrap_or(0));
                                }
                            }
                        }
                    }
                }
                let prefix = format!("refs/verlinpark/checkpoints/{id}/");
                let refs = worktree::git_output(
                    &project_root,
                    &["for-each-ref", "--format=%(refname)", &prefix],
                )
                .map(|output| {
                    output
                        .lines()
                        .filter(|line| !line.trim().is_empty())
                        .count()
                })
                .unwrap_or(0);
                Ok((commit_bytes, snapshot_bytes, seen.len(), refs))
            })
            .await??;
        Ok(SessionCheckpointStatsResult {
            session_id: session.id,
            checkpoints: checkpoints.len(),
            dirty,
            refs,
            commit_bytes,
            snapshot_bytes,
            unique_blobs,
        })
    }

    pub async fn restore_checkpoint(
        &self,
        session_id: &str,
        checkpoint_id: &str,
    ) -> Result<SessionCheckpointRestoreResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let (session, project) = tokio::task::spawn_blocking(move || {
            let session = store
                .get_session(&id)?
                .ok_or_else(|| anyhow::anyhow!("session not found: {id}"))?;
            let project = store
                .get_project(&session.project_id)?
                .ok_or_else(|| anyhow::anyhow!("project not found: {}", session.project_id))?;
            Ok::<_, anyhow::Error>((session, project))
        })
        .await??;
        ensure_session_quiescent(&session, "restore a checkpoint")?;
        let checkpoint = self
            .active_checkpoints(session_id)
            .await?
            .into_iter()
            .find(|checkpoint| checkpoint.id == checkpoint_id)
            .ok_or_else(|| anyhow::anyhow!("checkpoint not found: {checkpoint_id}"))?;
        let worktree = PathBuf::from(
            session
                .worktree
                .clone()
                .ok_or_else(|| anyhow::anyhow!("session has no worktree"))?,
        );
        if !worktree.is_dir() {
            anyhow::bail!("session worktree is missing: {}", worktree.display());
        }
        let expected_branch = session.branch.clone();
        let project_root = PathBuf::from(&project.root);
        let git_lock = self.git_lock.clone();
        let restore_path = worktree.clone();
        let status_path = worktree.clone();
        let branch_path = worktree.clone();
        let restore_commit = checkpoint.commit.clone();
        let (previous_commit, restored_commit) = tokio::task::spawn_blocking(move || {
            let _guard = git_lock
                .lock()
                .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
            let status = worktree::status_porcelain(&status_path)?;
            let branch = worktree::current_branch(&branch_path)?;
            if let Some(expected) = expected_branch.as_deref()
                && branch != expected
            {
                anyhow::bail!(
                    "session worktree is on branch {branch}; expected {expected}"
                );
            }
            if !status.is_empty() {
                anyhow::bail!(
                    "session worktree has uncommitted changes; commit or discard them before restoring a checkpoint"
                );
            }
            if !worktree::commit_exists(&project_root, &restore_commit) {
                anyhow::bail!("checkpoint commit is missing: {restore_commit}");
            }
            let previous = worktree::head_commit(&restore_path)?;
            worktree::reset_hard(&restore_path, &restore_commit)?;
            Ok::<_, anyhow::Error>((previous, restore_commit))
        })
        .await??;
        self.append_event(
            session_id,
            "session.checkpoint.restored",
            serde_json::json!({
                "checkpoint_id": checkpoint.id,
                "previous_commit": previous_commit,
                "restored_commit": restored_commit,
            }),
        )
        .await?;
        Ok(SessionCheckpointRestoreResult {
            session_id: session.id,
            checkpoint_id: checkpoint.id,
            previous_commit,
            restored_commit,
        })
    }

    pub async fn diff_checkpoint(
        &self,
        session_id: &str,
        checkpoint_id: &str,
    ) -> Result<SessionCheckpointDiffResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let (session, project) = tokio::task::spawn_blocking(move || {
            let session = store
                .get_session(&id)?
                .ok_or_else(|| anyhow::anyhow!("session not found: {id}"))?;
            let project = store
                .get_project(&session.project_id)?
                .ok_or_else(|| anyhow::anyhow!("project not found: {}", session.project_id))?;
            Ok::<_, anyhow::Error>((session, project))
        })
        .await??;
        ensure_session_quiescent(&session, "inspect a checkpoint")?;
        let checkpoint = self
            .active_checkpoints(session_id)
            .await?
            .into_iter()
            .find(|checkpoint| checkpoint.id == checkpoint_id)
            .ok_or_else(|| anyhow::anyhow!("checkpoint not found: {checkpoint_id}"))?;
        let worktree = PathBuf::from(
            session
                .worktree
                .clone()
                .ok_or_else(|| anyhow::anyhow!("session has no worktree"))?,
        );
        if !worktree.is_dir() {
            anyhow::bail!("session worktree is missing: {}", worktree.display());
        }
        let expected_branch = session.branch.clone();
        let project_root = PathBuf::from(&project.root);
        let git_lock = self.git_lock.clone();
        let diff_path = worktree.clone();
        let checkpoint_commit = checkpoint.commit.clone();
        let (current_commit, current_branch, clean, numstat, patch) =
            tokio::task::spawn_blocking(move || {
                let _guard = git_lock
                    .lock()
                    .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
                let branch = worktree::current_branch(&diff_path)?;
                if let Some(expected) = expected_branch.as_deref()
                    && branch != expected
                {
                    anyhow::bail!("session worktree is on branch {branch}; expected {expected}");
                }
                let clean = worktree::status_porcelain(&diff_path)?.is_empty();
                if !worktree::commit_exists(&project_root, &checkpoint_commit) {
                    anyhow::bail!("checkpoint commit is missing: {checkpoint_commit}");
                }
                let current = worktree::head_commit(&diff_path)?;
                let range = format!("{current}..{checkpoint_commit}");
                let numstat = worktree::git_output(
                    &project_root,
                    &["diff", "--numstat", "--no-color", "--no-ext-diff", &range],
                )?;
                let patch = worktree::git_output(
                    &project_root,
                    &["diff", "--no-color", "--no-ext-diff", &range],
                )?;
                Ok::<_, anyhow::Error>((current, branch, clean, numstat, patch))
            })
            .await??;
        let truncated = patch.chars().count() > 200_000;
        Ok(SessionCheckpointDiffResult {
            session_id: session.id,
            checkpoint_id: checkpoint.id,
            current_commit,
            checkpoint_commit: checkpoint.commit,
            current_branch,
            clean,
            files: parse_numstat(&numstat),
            patch: clip_transcript(&patch, 200_000),
            truncated,
        })
    }

    pub async fn delete_checkpoint(
        &self,
        session_id: &str,
        checkpoint_id: &str,
    ) -> Result<SessionCheckpointDeleteResult> {
        self.ensure_checkpoint_mutable(session_id).await?;
        let checkpoint = self
            .active_checkpoints(session_id)
            .await?
            .into_iter()
            .find(|checkpoint| checkpoint.id == checkpoint_id);
        let Some(checkpoint) = checkpoint else {
            return Ok(SessionCheckpointDeleteResult {
                session_id: session_id.to_string(),
                checkpoint_id: checkpoint_id.to_string(),
                removed: false,
            });
        };
        self.delete_checkpoint_record(session_id, &checkpoint, "manual")
            .await?;
        Ok(SessionCheckpointDeleteResult {
            session_id: session_id.to_string(),
            checkpoint_id: checkpoint_id.to_string(),
            removed: true,
        })
    }

    async fn delete_checkpoint_record(
        &self,
        session_id: &str,
        checkpoint: &SessionCheckpoint,
        reason: &str,
    ) -> Result<()> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let project_root = tokio::task::spawn_blocking(move || -> Result<PathBuf> {
            let session = store
                .get_session(&id)?
                .ok_or_else(|| anyhow::anyhow!("session not found: {id}"))?;
            let project = store
                .get_project(&session.project_id)?
                .ok_or_else(|| anyhow::anyhow!("project not found: {}", session.project_id))?;
            Ok(PathBuf::from(project.root))
        })
        .await??;
        self.append_event(
            session_id,
            "session.checkpoint.deleted",
            serde_json::json!({
                "checkpoint_id": checkpoint.id,
                "label": checkpoint.label,
                "commit": checkpoint.commit,
                "reason": reason,
            }),
        )
        .await?;
        if let Err(error) =
            worktree::delete_checkpoint_ref(&project_root, session_id, &checkpoint.id)
        {
            tracing::warn!(
                %error,
                session_id,
                checkpoint_id = %checkpoint.id,
                "could not remove checkpoint snapshot ref"
            );
        }
        Ok(())
    }

    pub async fn prune_checkpoints(
        &self,
        session_id: &str,
        older_than_days: Option<u32>,
        keep_latest: Option<u32>,
        dry_run: bool,
    ) -> Result<SessionCheckpointPruneResult> {
        if older_than_days.is_none() && keep_latest.is_none() {
            anyhow::bail!("checkpoint prune requires older_than_days or keep_latest");
        }
        self.ensure_checkpoint_mutable(session_id).await?;
        let checkpoints = self.active_checkpoints(session_id).await?;
        let keep_latest = keep_latest.unwrap_or(0) as usize;
        let cutoff = older_than_days
            .map(|days| time::OffsetDateTime::now_utc() - time::Duration::days(i64::from(days)));
        let mut candidates = Vec::new();
        for (index, checkpoint) in checkpoints.iter().enumerate() {
            let beyond_keep = index < checkpoints.len().saturating_sub(keep_latest);
            let old = cutoff.is_none_or(|cutoff| {
                time::OffsetDateTime::parse(
                    &checkpoint.created_at,
                    &time::format_description::well_known::Rfc3339,
                )
                .map(|created_at| created_at < cutoff)
                .unwrap_or(false)
            });
            if beyond_keep && old {
                candidates.push(checkpoint.clone());
            }
        }
        if dry_run {
            return Ok(SessionCheckpointPruneResult {
                session_id: session_id.to_string(),
                candidates: candidates.len(),
                removed: 0,
                dry_run: true,
                checkpoints: candidates,
            });
        }
        let mut removed = 0usize;
        for checkpoint in &candidates {
            self.delete_checkpoint_record(session_id, checkpoint, "prune")
                .await?;
            removed += 1;
        }
        Ok(SessionCheckpointPruneResult {
            session_id: session_id.to_string(),
            candidates: candidates.len(),
            removed,
            dry_run: false,
            checkpoints: candidates,
        })
    }

    pub async fn get_checkpoint_policy(
        &self,
        project_id: &str,
    ) -> Result<CheckpointRetentionPolicy> {
        let store = self.store.clone();
        let id = project_id.to_string();
        tokio::task::spawn_blocking(move || {
            store
                .get_project(&id)?
                .ok_or_else(|| anyhow::anyhow!("project not found: {id}"))?;
            store.get_checkpoint_policy(&id)
        })
        .await?
    }

    pub async fn set_checkpoint_policy(
        &self,
        project_id: &str,
        policy: &CheckpointRetentionPolicy,
    ) -> Result<CheckpointPolicyResult> {
        if policy.older_than_secs > 365 * 24 * 60 * 60 {
            anyhow::bail!("checkpoint retention must be at most 365 days");
        }
        if policy.keep_latest > 100 {
            anyhow::bail!("checkpoint keep_latest must be at most 100");
        }
        let store = self.store.clone();
        let id = project_id.to_string();
        let policy = policy.clone();
        let saved = tokio::task::spawn_blocking(move || {
            store
                .get_project(&id)?
                .ok_or_else(|| anyhow::anyhow!("project not found: {id}"))?;
            store.set_checkpoint_policy(&id, &policy)?;
            store.get_checkpoint_policy(&id)
        })
        .await??;
        Ok(CheckpointPolicyResult {
            project_id: project_id.to_string(),
            policy: saved,
        })
    }

    pub async fn get_context_checkpoint_policy(
        &self,
        project_id: &str,
    ) -> Result<vp_protocol::types::ContextCheckpointPolicy> {
        let store = self.store.clone();
        let id = project_id.to_string();
        tokio::task::spawn_blocking(move || {
            store
                .get_project(&id)?
                .ok_or_else(|| anyhow::anyhow!("project not found: {id}"))?;
            store.get_context_checkpoint_policy(&id)
        })
        .await?
    }

    pub async fn set_context_checkpoint_policy(
        &self,
        project_id: &str,
        policy: &vp_protocol::types::ContextCheckpointPolicy,
    ) -> Result<vp_protocol::types::ContextCheckpointPolicyResult> {
        crate::context::validate_context_checkpoint_policy(policy)?;
        let store = self.store.clone();
        let id = project_id.to_string();
        let policy = policy.clone();
        let saved = tokio::task::spawn_blocking(move || {
            store
                .get_project(&id)?
                .ok_or_else(|| anyhow::anyhow!("project not found: {id}"))?;
            store.set_context_checkpoint_policy(&id, &policy)?;
            store.get_context_checkpoint_policy(&id)
        })
        .await??;
        Ok(vp_protocol::types::ContextCheckpointPolicyResult {
            project_id: project_id.to_string(),
            policy: saved,
        })
    }

    /// Insert a context checkpoint row and its full-payload event atomically.
    /// Used by automatic compaction, the manual checkpoint command, and tests.
    pub(crate) async fn record_context_checkpoint(
        &self,
        checkpoint: vp_protocol::types::ContextCheckpoint,
    ) -> Result<Event> {
        let checkpoint = crate::context::bound_context_checkpoint(checkpoint)?;
        let store = self.store.clone();
        let session_id = checkpoint.session_id.clone();
        let event = tokio::task::spawn_blocking(move || {
            store.record_context_checkpoint_with_event(&session_id, &checkpoint)
        })
        .await??;
        let _ = self.events.send(event.clone());
        Ok(event)
    }

    /// Append a successful compaction and its automatic checkpoint atomically.
    pub(crate) async fn append_context_compaction_with_checkpoint(
        &self,
        checkpoint: vp_protocol::types::ContextCheckpoint,
        compaction_payload: serde_json::Value,
    ) -> Result<(Event, Event)> {
        let checkpoint = crate::context::bound_context_checkpoint(checkpoint)?;
        let store = self.store.clone();
        let session_id = checkpoint.session_id.clone();
        let (compaction, checkpoint_event) = tokio::task::spawn_blocking(move || {
            store.append_context_compaction_with_checkpoint(
                &session_id,
                compaction_payload,
                &checkpoint,
            )
        })
        .await??;
        let _ = self.events.send(compaction.clone());
        let _ = self.events.send(checkpoint_event.clone());
        Ok((compaction, checkpoint_event))
    }

    /// Append the reconstructed compaction boundary and the restore audit
    /// event. This does not touch messages, checkpoints, or the worktree.
    pub(crate) async fn append_context_restore_events(
        &self,
        session_id: &str,
        compaction_payload: serde_json::Value,
        restored_payload: serde_json::Value,
    ) -> Result<(Event, Event)> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let (compaction, restored) = tokio::task::spawn_blocking(move || {
            store.append_context_restore_events(&session_id, compaction_payload, restored_payload)
        })
        .await??;
        let _ = self.events.send(compaction.clone());
        let _ = self.events.send(restored.clone());
        Ok((compaction, restored))
    }

    pub async fn list_context_checkpoints(
        &self,
        session_id: &str,
        limit: Option<u32>,
    ) -> Result<vp_protocol::types::ContextCheckpointListResult> {
        let limit = limit
            .unwrap_or(50)
            .clamp(1, crate::context::MAX_CONTEXT_CHECKPOINT_LIST_LIMIT);
        let store = self.store.clone();
        let id = session_id.to_string();
        let (session, checkpoints) = tokio::task::spawn_blocking(move || {
            let session = store
                .get_session(&id)?
                .ok_or_else(|| anyhow::anyhow!("session not found: {id}"))?;
            let checkpoints = store.list_context_checkpoints(&id, limit)?;
            Ok::<_, anyhow::Error>((session, checkpoints))
        })
        .await??;
        let policy = self
            .get_context_checkpoint_policy(&session.project_id)
            .await?;
        let latest_id = checkpoints.last().map(|checkpoint| checkpoint.id.clone());
        Ok(vp_protocol::types::ContextCheckpointListResult {
            session_id: session.id,
            checkpoints,
            latest_id,
            policy,
            limit,
        })
    }

    pub async fn delete_context_checkpoint(
        &self,
        session_id: &str,
        checkpoint_id: &str,
    ) -> Result<vp_protocol::types::ContextCheckpointDeleteResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        if session.status == "imported" {
            anyhow::bail!("imported sessions are read-only archives");
        }
        let active = is_active_session_status(&session.status);
        let latest = {
            let store = self.store.clone();
            let id = session_id.to_string();
            tokio::task::spawn_blocking(move || store.latest_context_checkpoint(&id)).await??
        };
        if active && latest.as_ref().is_some_and(|row| row.id == checkpoint_id) {
            anyhow::bail!(
                "cannot delete the latest context checkpoint while the session is active"
            );
        }
        let store = self.store.clone();
        let id = session_id.to_string();
        let checkpoint_id_owned = checkpoint_id.to_string();
        let event = tokio::task::spawn_blocking(move || {
            store.delete_context_checkpoint_with_event(&id, &checkpoint_id_owned, "manual")
        })
        .await??;
        if let Some(event) = event {
            let _ = self.events.send(event);
            return Ok(vp_protocol::types::ContextCheckpointDeleteResult {
                session_id: session_id.to_string(),
                checkpoint_id: checkpoint_id.to_string(),
                removed: true,
            });
        }
        Ok(vp_protocol::types::ContextCheckpointDeleteResult {
            session_id: session_id.to_string(),
            checkpoint_id: checkpoint_id.to_string(),
            removed: false,
        })
    }

    /// Oldest-first retention. The latest checkpoint of an active run is
    /// always protected, even when `keep_latest` is zero.
    pub async fn prune_context_checkpoints(
        &self,
        session_id: &str,
        older_than_days: Option<u32>,
        keep_latest: Option<u32>,
        dry_run: bool,
    ) -> Result<vp_protocol::types::ContextCheckpointPruneResult> {
        if older_than_days.is_some_and(|days| days > 365) {
            anyhow::bail!("context checkpoint older_than_days must be at most 365");
        }
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        if session.status == "imported" {
            anyhow::bail!("imported sessions are read-only archives");
        }
        let policy = self
            .get_context_checkpoint_policy(&session.project_id)
            .await?;
        let keep_latest = keep_latest
            .unwrap_or(policy.max_checkpoints)
            .min(crate::context::MAX_CONTEXT_CHECKPOINT_POLICY_ITEMS)
            as usize;
        let age_days = match older_than_days {
            Some(0) => None,
            Some(days) => Some(days),
            None if policy.max_age_days == 0 => None,
            None => Some(policy.max_age_days),
        };
        let cutoff = age_days
            .map(|days| time::OffsetDateTime::now_utc() - time::Duration::days(i64::from(days)));
        let store = self.store.clone();
        let id = session_id.to_string();
        let checkpoints =
            tokio::task::spawn_blocking(move || store.list_context_checkpoints(&id, 10_000))
                .await??;
        let active = is_active_session_status(&session.status);
        let mut candidates = Vec::new();
        for (index, checkpoint) in checkpoints.iter().enumerate() {
            let protected_by_count = index >= checkpoints.len().saturating_sub(keep_latest);
            let protected_active = active && index + 1 == checkpoints.len();
            let old = cutoff.is_none_or(|cutoff| {
                time::OffsetDateTime::parse(
                    &checkpoint.created_at,
                    &time::format_description::well_known::Rfc3339,
                )
                .map(|created_at| created_at < cutoff)
                .unwrap_or(false)
            });
            if !protected_by_count && !protected_active && old {
                candidates.push(checkpoint.clone());
            }
        }
        if dry_run {
            return Ok(vp_protocol::types::ContextCheckpointPruneResult {
                session_id: session_id.to_string(),
                candidates: candidates.len(),
                removed: 0,
                dry_run: true,
                checkpoints: candidates,
            });
        }
        let mut removed = 0usize;
        for checkpoint in &candidates {
            let store = self.store.clone();
            let id = session_id.to_string();
            let checkpoint_id = checkpoint.id.clone();
            let event = tokio::task::spawn_blocking(move || {
                store.delete_context_checkpoint_with_event(&id, &checkpoint_id, "prune")
            })
            .await??;
            if let Some(event) = event {
                let _ = self.events.send(event);
                removed += 1;
            }
        }
        Ok(vp_protocol::types::ContextCheckpointPruneResult {
            session_id: session_id.to_string(),
            candidates: candidates.len(),
            removed,
            dry_run: false,
            checkpoints: candidates,
        })
    }

    /// Run the opt-in context checkpoint retention policy for every project.
    pub async fn sweep_context_checkpoint_retention(&self) -> Result<usize> {
        let store = self.store.clone();
        let policies =
            tokio::task::spawn_blocking(move || store.list_enabled_context_checkpoint_policies())
                .await??;
        let mut removed = 0usize;
        for (project_id, policy) in policies {
            let store = self.store.clone();
            let sessions =
                tokio::task::spawn_blocking(move || store.list_sessions(Some(&project_id)))
                    .await??;
            for session in sessions {
                if session.status == "imported" {
                    continue;
                }
                let result = self
                    .prune_context_checkpoints(
                        &session.id,
                        (policy.max_age_days > 0).then_some(policy.max_age_days),
                        Some(policy.max_checkpoints),
                        false,
                    )
                    .await?;
                removed = removed.saturating_add(result.removed);
            }
        }
        Ok(removed)
    }

    /// Run the opt-in checkpoint retention policy for every project. Deletes
    /// only checkpoints older than the policy threshold and beyond its
    /// keep-latest count. Existing fork branches are unaffected.
    pub async fn sweep_checkpoint_retention(&self) -> Result<usize> {
        let store = self.store.clone();
        let policies =
            tokio::task::spawn_blocking(move || store.list_enabled_checkpoint_policies()).await??;
        let mut removed = 0usize;
        for (project_id, policy) in policies {
            let store = self.store.clone();
            let sessions =
                tokio::task::spawn_blocking(move || store.list_sessions(Some(&project_id)))
                    .await??;
            let days = policy.older_than_secs.div_ceil(86_400) as u32;
            for session in sessions {
                if session.status == "imported" {
                    continue;
                }
                removed += self
                    .prune_checkpoints(&session.id, Some(days), Some(policy.keep_latest), false)
                    .await?
                    .removed;
            }
        }
        Ok(removed)
    }

    /// Create a new session/worktree from a checkpoint. Without an explicit
    /// checkpoint, one is created from the source's current worktree state.
    pub async fn fork_session(
        &self,
        source_session_id: &str,
        checkpoint_id: Option<&str>,
        goal: Option<String>,
    ) -> Result<SessionForkResult> {
        let store = self.store.clone();
        let source_id = source_session_id.to_string();
        let source = tokio::task::spawn_blocking(move || store.get_session(&source_id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {source_session_id}"))?;
        ensure_session_quiescent(&source, "fork a session")?;
        let checkpoint = if let Some(checkpoint_id) = checkpoint_id {
            self.list_checkpoints(source_session_id)
                .await?
                .checkpoints
                .into_iter()
                .find(|checkpoint| checkpoint.id == checkpoint_id)
                .ok_or_else(|| anyhow::anyhow!("checkpoint not found: {checkpoint_id}"))?
        } else {
            self.checkpoint_session(source_session_id, None).await?
        };
        let store = self.store.clone();
        let project_id = source.project_id.clone();
        let project = tokio::task::spawn_blocking(move || store.get_project(&project_id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("project not found: {}", source.project_id))?;
        let goal = goal
            .map(|goal| goal.trim().to_string())
            .filter(|goal| !goal.is_empty())
            .unwrap_or_else(|| format!("{} (fork)", source.goal));
        let session_id = Uuid::now_v7().to_string();
        let root = PathBuf::from(&project.root);
        let data_dir = self.store.data_dir().to_path_buf();
        let project_id_for_worktree = project.id.clone();
        let session_id_for_worktree = session_id.clone();
        let commit = checkpoint.commit.clone();
        let git_lock = self.git_lock.clone();
        let worktree = tokio::task::spawn_blocking(move || {
            let _guard = git_lock
                .lock()
                .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
            worktree::create_from_commit(
                &root,
                &data_dir,
                &project_id_for_worktree,
                &session_id_for_worktree,
                &commit,
            )
        })
        .await??;
        let worktree_path = worktree.path.to_string_lossy().to_string();
        let branch = worktree.branch.clone();
        let base_commit = worktree.base_commit.clone();
        let store = self.store.clone();
        let new_session = {
            let session_id = session_id.clone();
            let project_id = project.id.clone();
            let goal = goal.clone();
            let worktree_path = worktree_path.clone();
            let branch = branch.clone();
            let base_commit = base_commit.clone();
            let token_budget = source.token_budget;
            let cost_budget = source.cost_budget_usd;
            let time_budget = source.time_budget_secs;
            let priority = source.priority;
            tokio::task::spawn_blocking(move || {
                store.create_session_with_worktree(
                    &session_id,
                    &project_id,
                    &goal,
                    &worktree_path,
                    &branch,
                    token_budget,
                    cost_budget,
                    time_budget,
                    source.unlimited,
                    &base_commit,
                    priority,
                )
            })
            .await?
        };
        let new_session = match new_session {
            Ok(session) => session,
            Err(error) => {
                worktree::remove(&PathBuf::from(&project.root), &worktree.path, &branch);
                return Err(error);
            }
        };

        let created = serde_json::json!({
            "goal": new_session.goal,
            "project_id": new_session.project_id,
            "worktree": new_session.worktree,
            "branch": new_session.branch,
            "priority": new_session.priority,
            "token_budget": new_session.token_budget,
            "cost_budget_usd": new_session.cost_budget_usd,
            "time_budget_secs": new_session.time_budget_secs,
            "unlimited": new_session.unlimited,
            "budget_mode": if new_session.unlimited { "unlimited" } else { "limited" },
            "base_commit": new_session.base_commit,
            "prompt_layout": new_session.prompt_layout,
            "created_at": new_session.created_at,
            "forked_from_session_id": source.id,
            "forked_from_checkpoint_id": checkpoint.id,
            "forked_from_commit": checkpoint.commit,
        });
        self.append_event(&new_session.id, "session.created", created)
            .await?;
        let source_leader = self
            .list_agents(source_session_id)
            .await?
            .into_iter()
            .find(|agent| agent.role == "leader");
        let model = source_leader
            .map(|agent| agent.model)
            .unwrap_or_else(|| "unconfigured".to_string());
        if let Err(error) = self.create_leader_agent(&new_session.id, &model).await {
            tracing::warn!(%error, session_id = %new_session.id, "could not seed forked leader agent");
        }
        self.append_event(
            &new_session.id,
            "session.forked",
            serde_json::json!({
                "source_session_id": source.id,
                "source_goal": source.goal,
                "checkpoint_id": checkpoint.id,
                "commit": checkpoint.commit,
                "message_count": checkpoint.message_count,
                "has_summary": checkpoint.summary.is_some(),
            }),
        )
        .await?;
        self.copy_checkpoint_messages(source_session_id, &new_session.id, &checkpoint)
            .await?;
        if let Err(error) = self
            .append_event(
                source_session_id,
                "session.forked",
                serde_json::json!({
                    "fork_session_id": new_session.id,
                    "fork_goal": new_session.goal,
                    "checkpoint_id": checkpoint.id,
                    "commit": checkpoint.commit,
                }),
            )
            .await
        {
            tracing::warn!(%error, session_id = %source.id, "could not record source fork event");
        }
        Ok(SessionForkResult {
            source_session_id: source.id,
            checkpoint_id: checkpoint.id,
            session: new_session,
        })
    }

    pub async fn session_fork_tree(&self, session_id: &str) -> Result<SessionForkTreeResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        let store = self.store.clone();
        let id = session_id.to_string();
        let created = tokio::task::spawn_blocking(move || {
            store.list_events_by_kind(&id, "session.created", 1)
        })
        .await??;
        let created_payload = created.first().map(|event| event.payload.clone());
        let checkpoint_id = created_payload
            .as_ref()
            .and_then(|payload| payload.get("forked_from_checkpoint_id"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let commit = created_payload
            .as_ref()
            .and_then(|payload| payload.get("forked_from_commit"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let total = Arc::new(AtomicUsize::new(1));
        let truncated = Arc::new(AtomicBool::new(false));
        let children = self
            .build_fork_children(session_id, 0, total.clone(), truncated.clone())
            .await?;
        let total = total.load(Ordering::Relaxed);
        let truncated = truncated.load(Ordering::Relaxed);
        Ok(SessionForkTreeResult {
            session_id: session.id.clone(),
            root: SessionForkTreeNode {
                session,
                checkpoint_id,
                commit,
                children,
            },
            total,
            truncated,
        })
    }

    #[async_recursion]
    async fn build_fork_children(
        &self,
        session_id: &str,
        depth: u32,
        total: Arc<AtomicUsize>,
        truncated: Arc<AtomicBool>,
    ) -> Result<Vec<SessionForkTreeNode>> {
        if depth >= 32 {
            truncated.store(true, Ordering::Relaxed);
            return Ok(Vec::new());
        }
        let store = self.store.clone();
        let id = session_id.to_string();
        let events = tokio::task::spawn_blocking(move || {
            store.list_events_by_kind(&id, "session.forked", 1000)
        })
        .await??;
        let mut nodes = Vec::new();
        for event in events {
            if total.load(Ordering::Relaxed) >= 200 {
                truncated.store(true, Ordering::Relaxed);
                break;
            }
            let Some(entry) = self.lineage_entry_from_fork_event(&event).await else {
                continue;
            };
            total.fetch_add(1, Ordering::Relaxed);
            let children = self
                .build_fork_children(
                    &entry.session.id,
                    depth + 1,
                    total.clone(),
                    truncated.clone(),
                )
                .await?;
            nodes.push(SessionForkTreeNode {
                session: entry.session,
                checkpoint_id: entry.checkpoint_id,
                commit: entry.commit,
                children,
            });
        }
        nodes.sort_by(|left, right| left.session.created_at.cmp(&right.session.created_at));
        Ok(nodes)
    }

    pub async fn session_lineage(&self, session_id: &str) -> Result<SessionLineageResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        let store = self.store.clone();
        let id = session_id.to_string();
        let created = tokio::task::spawn_blocking(move || {
            store.list_events_by_kind(&id, "session.created", 1)
        })
        .await??;
        let created_payload = created.first().map(|event| event.payload.clone());
        let parent_id = created_payload
            .as_ref()
            .and_then(|payload| payload.get("forked_from_session_id"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let parent_checkpoint = created_payload
            .as_ref()
            .and_then(|payload| payload.get("forked_from_checkpoint_id"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let parent_commit = created_payload
            .as_ref()
            .and_then(|payload| payload.get("forked_from_commit"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let mut parent = None;
        let mut siblings = Vec::new();
        if let Some(parent_id) = &parent_id {
            let store = self.store.clone();
            let parent_for_load = parent_id.clone();
            if let Some(parent_session) =
                tokio::task::spawn_blocking(move || store.get_session(&parent_for_load)).await??
            {
                parent = Some(SessionLineageEntry {
                    session: parent_session,
                    checkpoint_id: parent_checkpoint,
                    commit: parent_commit,
                });
            }
            let store = self.store.clone();
            let parent_for_events = parent_id.clone();
            let events = tokio::task::spawn_blocking(move || {
                store.list_events_by_kind(&parent_for_events, "session.forked", 1000)
            })
            .await??;
            for event in events {
                let Some(entry) = self.lineage_entry_from_fork_event(&event).await else {
                    continue;
                };
                if entry.session.id != session_id {
                    siblings.push(entry);
                }
            }
        }
        let store = self.store.clone();
        let id = session_id.to_string();
        let events = tokio::task::spawn_blocking(move || {
            store.list_events_by_kind(&id, "session.forked", 1000)
        })
        .await??;
        let mut children = Vec::new();
        for event in events {
            if let Some(entry) = self.lineage_entry_from_fork_event(&event).await {
                children.push(entry);
            }
        }
        siblings.sort_by(|left, right| left.session.created_at.cmp(&right.session.created_at));
        children.sort_by(|left, right| left.session.created_at.cmp(&right.session.created_at));
        Ok(SessionLineageResult {
            session_id: session.id,
            parent,
            siblings,
            children,
        })
    }

    async fn lineage_entry_from_fork_event(&self, event: &Event) -> Option<SessionLineageEntry> {
        let fork_id = event
            .payload
            .get("fork_session_id")
            .and_then(serde_json::Value::as_str)?
            .to_string();
        let checkpoint_id = event
            .payload
            .get("checkpoint_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let mut commit = event
            .payload
            .get("commit")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let store = self.store.clone();
        let session_id = fork_id.clone();
        let session =
            match tokio::task::spawn_blocking(move || store.get_session(&session_id)).await {
                Ok(Ok(Some(session))) => session,
                _ => return None,
            };
        if commit.is_none() {
            let store = self.store.clone();
            let session_id = fork_id.clone();
            if let Ok(Ok(events)) = tokio::task::spawn_blocking(move || {
                store.list_events_by_kind(&session_id, "session.created", 1)
            })
            .await
                && let Some(created) = events.first()
            {
                commit = created
                    .payload
                    .get("forked_from_commit")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
            }
        }
        Some(SessionLineageEntry {
            session,
            checkpoint_id,
            commit,
        })
    }

    pub async fn compare_sessions(
        &self,
        session_id: &str,
        other_session_id: &str,
    ) -> Result<SessionCompareResult> {
        if session_id == other_session_id {
            anyhow::bail!("choose two different sessions to compare");
        }
        let store = self.store.clone();
        let left_id = session_id.to_string();
        let right_id = other_session_id.to_string();
        let (left, right) = tokio::task::spawn_blocking(move || {
            let left = store
                .get_session(&left_id)?
                .ok_or_else(|| anyhow::anyhow!("session not found: {left_id}"))?;
            let right = store
                .get_session(&right_id)?
                .ok_or_else(|| anyhow::anyhow!("session not found: {right_id}"))?;
            Ok::<_, anyhow::Error>((left, right))
        })
        .await??;
        if left.project_id != right.project_id {
            anyhow::bail!("sessions must belong to the same project");
        }
        let left_branch = left
            .branch
            .clone()
            .ok_or_else(|| anyhow::anyhow!("source session has no branch"))?;
        let right_branch = right
            .branch
            .clone()
            .ok_or_else(|| anyhow::anyhow!("other session has no branch"))?;
        let store = self.store.clone();
        let project_id = left.project_id.clone();
        let project = tokio::task::spawn_blocking(move || store.get_project(&project_id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("project not found: {}", left.project_id))?;
        let root = PathBuf::from(&project.root);
        let git_lock = self.git_lock.clone();
        let left_branch_for_git = left_branch.clone();
        let right_branch_for_git = right_branch.clone();
        let root_for_git = root.clone();
        let (left_commit, right_commit, numstat, patch) = tokio::task::spawn_blocking(move || {
            let _guard = git_lock
                .lock()
                .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
            let left_commit =
                worktree::git_output(&root_for_git, &["rev-parse", &left_branch_for_git])?;
            let right_commit =
                worktree::git_output(&root_for_git, &["rev-parse", &right_branch_for_git])?;
            let range = format!("{left_commit}..{right_commit}");
            let numstat = worktree::git_output(
                &root_for_git,
                &["diff", "--numstat", "--no-color", "--no-ext-diff", &range],
            )?;
            let patch = worktree::git_output(
                &root_for_git,
                &["diff", "--no-color", "--no-ext-diff", &range],
            )?;
            Ok::<_, anyhow::Error>((left_commit, right_commit, numstat, patch))
        })
        .await??;
        let truncated = patch.chars().count() > 200_000;
        Ok(SessionCompareResult {
            session_id: session_id.to_string(),
            other_session_id: other_session_id.to_string(),
            left_commit,
            right_commit,
            files: parse_numstat(&numstat),
            patch: clip_transcript(&patch, 200_000),
            truncated,
        })
    }

    async fn load_active_snapshot(
        &self,
        session_id: &str,
    ) -> Result<(Vec<StoredMessage>, usize, Option<String>)> {
        let messages = self.list_messages(session_id).await?;
        let mut start = 0usize;
        let mut summary = None;
        if let Some(event) = self
            .latest_event_of_kind(session_id, "context.compacted")
            .await?
        {
            let mode = event
                .payload
                .get("mode")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("summary");
            let through = event
                .payload
                .get("through_message_id")
                .and_then(serde_json::Value::as_str);
            let text = event
                .payload
                .get("summary")
                .and_then(serde_json::Value::as_str)
                .filter(|text| !text.trim().is_empty());
            if let Some(through) = through
                && let Some(index) = messages.iter().position(|message| message.id == through)
            {
                start = index.saturating_add(1);
                if mode == "summary" {
                    summary = text.map(str::to_string);
                }
            }
        }
        Ok((messages, start, summary))
    }

    async fn copy_checkpoint_messages(
        &self,
        source_session_id: &str,
        target_session_id: &str,
        checkpoint: &SessionCheckpoint,
    ) -> Result<usize> {
        let messages = self.list_messages(source_session_id).await?;
        let mut tool_names = std::collections::HashMap::new();
        if let Some(summary) = &checkpoint.summary {
            self.append_message(
                target_session_id,
                "user",
                summary,
                serde_json::json!([]),
                None,
            )
            .await?;
        }
        let (start, end) = match (
            checkpoint.start_message_id.as_deref(),
            checkpoint.through_message_id.as_deref(),
        ) {
            (Some(start_id), Some(end_id)) => {
                let start = messages
                    .iter()
                    .position(|message| message.id == start_id)
                    .ok_or_else(|| anyhow::anyhow!("checkpoint start message is missing"))?;
                let end = messages
                    .iter()
                    .position(|message| message.id == end_id)
                    .ok_or_else(|| anyhow::anyhow!("checkpoint through message is missing"))?;
                if end < start {
                    anyhow::bail!("checkpoint message range is invalid");
                }
                (start, end)
            }
            (None, None) => return Ok(0),
            _ => anyhow::bail!("checkpoint message range is incomplete"),
        };

        let mut copied = 0usize;
        for message in &messages[start..=end] {
            let tool_calls = message.tool_calls.clone();
            self.append_message(
                target_session_id,
                &message.role,
                &message.content,
                tool_calls.clone(),
                message.tool_call_id.clone(),
            )
            .await?;
            if message.role == "assistant" {
                if !message.content.trim().is_empty() {
                    self.append_event(
                        target_session_id,
                        "agent.message",
                        serde_json::json!({
                            "text": message.content,
                            "agent_role": "leader",
                            "agent_name": "Leader",
                            "forked": true,
                        }),
                    )
                    .await?;
                }
                if let Some(calls) = tool_calls.as_array() {
                    for call in calls {
                        let Some(id) = call.get("id").and_then(serde_json::Value::as_str) else {
                            continue;
                        };
                        let name = call
                            .get("name")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("tool");
                        let arguments = call
                            .get("arguments")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("");
                        tool_names.insert(id.to_string(), name.to_string());
                        let input = serde_json::from_str::<serde_json::Value>(arguments)
                            .unwrap_or(serde_json::Value::Null);
                        self.append_event(
                            target_session_id,
                            "tool.started",
                            serde_json::json!({
                                "tool_call_id": id,
                                "name": name,
                                "input": input,
                                "agent_role": "leader",
                                "agent_name": "Leader",
                                "forked": true,
                            }),
                        )
                        .await?;
                    }
                }
            } else if message.role == "tool"
                && let Some(tool_call_id) = &message.tool_call_id
            {
                let name = tool_names
                    .get(tool_call_id)
                    .cloned()
                    .unwrap_or_else(|| "tool".to_string());
                self.append_event(
                    target_session_id,
                    "tool.finished",
                    serde_json::json!({
                        "tool_call_id": tool_call_id,
                        "name": name,
                        "output": message.content,
                        "is_error": false,
                        "agent_role": "leader",
                        "agent_name": "Leader",
                        "forked": true,
                    }),
                )
                .await?;
            }
            copied += 1;
        }
        Ok(copied)
    }
}

pub(crate) fn checkpoint_from_event(event: &Event) -> Option<SessionCheckpoint> {
    let payload = event.payload.as_object()?;
    Some(SessionCheckpoint {
        id: payload.get("checkpoint_id")?.as_str()?.to_string(),
        session_id: event.session_id.clone(),
        label: payload
            .get("label")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("checkpoint")
            .to_string(),
        commit: payload.get("commit")?.as_str()?.to_string(),
        branch: payload
            .get("branch")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        start_message_id: payload
            .get("start_message_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        through_message_id: payload
            .get("through_message_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        message_count: payload
            .get("message_count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as usize,
        summary: payload
            .get("summary")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        dirty: payload
            .get("dirty")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        created_at: payload
            .get("created_at")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(&event.created_at)
            .to_string(),
    })
}

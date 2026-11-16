use super::*;

impl Engine {
    #[allow(clippy::too_many_arguments)]
    pub async fn get_worktree_policy(&self, project_id: &str) -> Result<WorktreeRetentionPolicy> {
        let store = self.store.clone();
        let id = project_id.to_string();
        tokio::task::spawn_blocking(move || {
            store
                .get_project(&id)?
                .ok_or_else(|| anyhow::anyhow!("project not found: {id}"))?;
            store.get_worktree_policy(&id)
        })
        .await?
    }

    pub async fn set_worktree_policy(
        &self,
        project_id: &str,
        policy: &WorktreeRetentionPolicy,
    ) -> Result<WorktreePolicyResult> {
        if policy.older_than_secs > 365 * 24 * 60 * 60 {
            anyhow::bail!("worktree retention must be at most 365 days");
        }
        let store = self.store.clone();
        let id = project_id.to_string();
        let policy = policy.clone();
        let saved = tokio::task::spawn_blocking(move || {
            store
                .get_project(&id)?
                .ok_or_else(|| anyhow::anyhow!("project not found: {id}"))?;
            store.set_worktree_policy(&id, &policy)?;
            store.get_worktree_policy(&id)
        })
        .await??;
        Ok(WorktreePolicyResult {
            project_id: project_id.to_string(),
            policy: saved,
        })
    }

    /// Run the opt-in retention policy for every project with it enabled.
    /// Returns the number of worktrees removed.
    pub async fn sweep_worktree_retention(&self) -> Result<usize> {
        let store = self.store.clone();
        let policies =
            tokio::task::spawn_blocking(move || store.list_enabled_worktree_policies()).await??;
        let mut removed = 0;
        for (project_id, policy) in policies {
            match self
                .cleanup_worktrees(
                    &project_id,
                    policy.older_than_secs,
                    false,
                    false,
                    policy.delete_branches,
                    policy.keep_failed,
                )
                .await
            {
                Ok(result) => removed += result.removed,
                Err(error) => {
                    tracing::warn!(
                        project_id = %project_id,
                        error = %error,
                        "automatic worktree retention failed"
                    );
                }
            }
        }
        Ok(removed)
    }

    pub async fn cleanup_worktrees(
        &self,
        project_id: &str,
        older_than_secs: u64,
        dry_run: bool,
        force: bool,
        delete_branches: bool,
        keep_failed: bool,
    ) -> Result<WorktreeCleanupResult> {
        let store = self.store.clone();
        let project_store = store.clone();
        let project_id_owned = project_id.to_string();
        let project =
            tokio::task::spawn_blocking(move || project_store.get_project(&project_id_owned))
                .await??
                .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        let project_root = PathBuf::from(&project.root);
        let list_store = store.clone();
        let list_project_id = project_id.to_string();
        let sessions =
            tokio::task::spawn_blocking(move || list_store.list_sessions(Some(&list_project_id)))
                .await??;
        let now = time::OffsetDateTime::now_utc();
        let sandbox_root = store.data_dir().join("sandbox");
        let mut entries = Vec::new();
        let mut removed = 0usize;
        let mut skipped = 0usize;
        let mut scanned = 0usize;
        for session in sessions {
            let terminal = matches!(
                session.status.as_str(),
                "idle" | "failed" | "cancelled" | "landed"
            );
            if !terminal {
                continue;
            }
            let Some(worktree) = session.worktree.clone() else {
                continue;
            };
            let Some(branch) = session.branch.clone() else {
                continue;
            };
            scanned += 1;
            if keep_failed && matches!(session.status.as_str(), "failed" | "cancelled") {
                skipped += 1;
                entries.push(WorktreeCleanupEntry {
                    session_id: session.id.clone(),
                    status: session.status.clone(),
                    worktree,
                    branch,
                    removed: false,
                    reason: "kept failed/cancelled worktree".to_string(),
                });
                continue;
            }
            let age = time::OffsetDateTime::parse(
                &session.created_at,
                &time::format_description::well_known::Rfc3339,
            )
            .map(|created| (now - created).whole_seconds().max(0) as u64)
            .unwrap_or(0);
            if age < older_than_secs {
                skipped += 1;
                entries.push(WorktreeCleanupEntry {
                    session_id: session.id.clone(),
                    status: session.status.clone(),
                    worktree,
                    branch,
                    removed: false,
                    reason: "younger than retention threshold".to_string(),
                });
                continue;
            }
            let path = PathBuf::from(&worktree);
            if !path.exists() {
                self.stop_session_browsers(&session.id).await;
                self.stop_session_computers(&session.id).await;
                let sandbox = sandbox_root.join(&session.id);
                let _ =
                    tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&sandbox)).await?;
                skipped += 1;
                entries.push(WorktreeCleanupEntry {
                    session_id: session.id.clone(),
                    status: session.status.clone(),
                    worktree,
                    branch,
                    removed: false,
                    reason: "worktree path is already missing".to_string(),
                });
                continue;
            }
            let dirty = tokio::task::spawn_blocking({
                let path = path.clone();
                move || worktree::has_uncommitted_changes(&path)
            })
            .await??;
            if dirty && !force {
                skipped += 1;
                entries.push(WorktreeCleanupEntry {
                    session_id: session.id.clone(),
                    status: session.status.clone(),
                    worktree,
                    branch,
                    removed: false,
                    reason: "uncommitted changes (use force to remove)".to_string(),
                });
                continue;
            }
            if dry_run {
                skipped += 1;
                entries.push(WorktreeCleanupEntry {
                    session_id: session.id.clone(),
                    status: session.status.clone(),
                    worktree,
                    branch,
                    removed: false,
                    reason: "would remove".to_string(),
                });
                continue;
            }
            self.stop_session_processes(&session.id, "worktree cleanup")
                .await;
            crate::tooling::lsp::stop_lsp_servers(&path);
            self.stop_session_browsers(&session.id).await;
            self.stop_session_computers(&session.id).await;
            match tokio::task::spawn_blocking({
                let root = project_root.clone();
                let path = path.clone();
                move || worktree::remove_worktree(&root, &path)
            })
            .await?
            {
                Ok(()) => {
                    let mut reason = "removed worktree".to_string();
                    let sandbox = sandbox_root.join(&session.id);
                    if let Err(error) =
                        tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&sandbox))
                            .await?
                        && error.kind() != std::io::ErrorKind::NotFound
                    {
                        reason = format!("{reason}; scratch cleanup failed: {error}");
                    }
                    if delete_branches
                        && let Err(error) = tokio::task::spawn_blocking({
                            let root = project_root.clone();
                            let branch = branch.clone();
                            move || worktree::delete_branch(&root, &branch)
                        })
                        .await?
                    {
                        reason = format!("removed worktree; branch delete failed: {error}");
                    }
                    removed += 1;
                    entries.push(WorktreeCleanupEntry {
                        session_id: session.id.clone(),
                        status: session.status.clone(),
                        worktree,
                        branch,
                        removed: true,
                        reason,
                    });
                }
                Err(error) => {
                    skipped += 1;
                    entries.push(WorktreeCleanupEntry {
                        session_id: session.id.clone(),
                        status: session.status.clone(),
                        worktree,
                        branch,
                        removed: false,
                        reason: format!("remove failed: {error}"),
                    });
                }
            }
        }
        Ok(WorktreeCleanupResult {
            project_id: project.id,
            scanned,
            removed,
            skipped,
            entries,
        })
    }

    // ---------------------------------------------------------------------
    // Task contracts and intent authorization
    // ---------------------------------------------------------------------
}

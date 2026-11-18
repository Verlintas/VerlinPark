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
            let session_age = time::OffsetDateTime::parse(
                &session.created_at,
                &time::format_description::well_known::Rfc3339,
            )
            .map(|created| (now - created).whole_seconds().max(0) as u64)
            .unwrap_or(0);
            let retains = !(keep_failed
                && matches!(session.status.as_str(), "failed" | "cancelled"))
                && session_age >= older_than_secs;
            if retains {
                let (task_scanned, task_removed, task_skipped, mut task_entries) = self
                    .sweep_task_worktrees(
                        &project_root,
                        &session,
                        older_than_secs,
                        dry_run,
                        force,
                        delete_branches,
                        now,
                    )
                    .await?;
                scanned += task_scanned;
                removed += task_removed;
                skipped += task_skipped;
                entries.append(&mut task_entries);
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
            let age = session_age;
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

    /// Task/Worker checkouts created by the legacy task-worktree flow. They
    /// live under `<data>/worktrees/<project>/<session>/<task>` and are not
    /// siblings of the session worktree, so the session cleanup above never
    /// touched them. This sweep is bounded by the same retention policy and
    /// refuses to remove a worktree whose agent is active, whose branch has a
    /// commit outside the session head, or that has uncommitted changes
    /// (unless `force` is explicit).
    #[allow(clippy::too_many_arguments)]
    async fn sweep_task_worktrees(
        &self,
        project_root: &Path,
        session: &Session,
        older_than_secs: u64,
        dry_run: bool,
        force: bool,
        delete_branches: bool,
        now: time::OffsetDateTime,
    ) -> Result<(usize, usize, usize, Vec<WorktreeCleanupEntry>)> {
        let agents = self.list_agents(&session.id).await?;
        let store = self.store.clone();
        let session_id = session.id.clone();
        let queue =
            tokio::task::spawn_blocking(move || store.list_merge_queue(&session_id)).await??;
        let merged_branches = queue
            .iter()
            .filter(|entry| entry.status == "merged")
            .map(|entry| entry.branch.clone())
            .collect::<std::collections::HashSet<_>>();
        let task_root = self
            .store
            .data_dir()
            .join("worktrees")
            .join(&session.project_id)
            .join(&session.id);
        let session_worktree = session.worktree.clone();
        let session_head = match session_worktree.clone().map(PathBuf::from) {
            Some(path) => tokio::task::spawn_blocking(move || worktree::head_commit(&path))
                .await
                .ok()
                .and_then(Result::ok),
            None => None,
        };
        let mut scanned = 0usize;
        let mut removed = 0usize;
        let mut skipped = 0usize;
        let mut entries = Vec::new();
        let mut stopped_processes = false;
        fn task_entry(
            session: &Session,
            agent: &AgentRecord,
            worktree: &str,
            reason: String,
        ) -> WorktreeCleanupEntry {
            WorktreeCleanupEntry {
                session_id: session.id.clone(),
                status: session.status.clone(),
                worktree: worktree.to_string(),
                branch: agent.branch.clone().unwrap_or_default(),
                removed: false,
                reason,
            }
        }
        for agent in agents {
            if agent.workspace_id.is_some() {
                // S4 component workspace checkouts have their own lifecycle.
                continue;
            }
            let Some(worktree) = agent.worktree.clone() else {
                continue;
            };
            if session_worktree.as_deref() == Some(worktree.as_str()) {
                continue;
            }
            let path = PathBuf::from(&worktree);
            if !path.starts_with(&task_root) || !path.exists() {
                continue;
            }
            scanned += 1;
            if matches!(agent.status.as_str(), "running" | "waiting_input") {
                skipped += 1;
                entries.push(task_entry(
                    session,
                    &agent,
                    &worktree,
                    "task agent is still active".to_string(),
                ));
                continue;
            }
            let age = time::OffsetDateTime::parse(
                &agent.created_at,
                &time::format_description::well_known::Rfc3339,
            )
            .map(|created| (now - created).whole_seconds().max(0) as u64)
            .unwrap_or(0);
            if age < older_than_secs {
                skipped += 1;
                entries.push(task_entry(
                    session,
                    &agent,
                    &worktree,
                    "task worktree is younger than retention threshold".to_string(),
                ));
                continue;
            }
            let branch = agent.branch.clone().unwrap_or_default();
            let merged = !branch.is_empty()
                && (merged_branches.contains(&branch)
                    || session_head.as_deref().is_some_and(|head| {
                        worktree::is_ancestor(&path, &branch, head).unwrap_or(false)
                    }));
            if !merged {
                skipped += 1;
                entries.push(task_entry(
                    session,
                    &agent,
                    &worktree,
                    "task commit is not merged into the session branch".to_string(),
                ));
                continue;
            }
            let dirty = tokio::task::spawn_blocking({
                let path = path.clone();
                move || worktree::has_uncommitted_changes(&path)
            })
            .await??;
            if dirty && !force {
                skipped += 1;
                entries.push(task_entry(
                    session,
                    &agent,
                    &worktree,
                    "task worktree has uncommitted changes (use force to remove)".to_string(),
                ));
                continue;
            }
            if dry_run {
                skipped += 1;
                entries.push(task_entry(
                    session,
                    &agent,
                    &worktree,
                    "would remove task worktree".to_string(),
                ));
                continue;
            }
            if !stopped_processes {
                self.stop_session_processes(&session.id, "task worktree cleanup")
                    .await;
                stopped_processes = true;
            }
            match tokio::task::spawn_blocking({
                let root = project_root.to_path_buf();
                let path = path.clone();
                move || worktree::remove_worktree(&root, &path)
            })
            .await?
            {
                Ok(()) => {
                    let mut reason = "removed task worktree".to_string();
                    if delete_branches
                        && !branch.is_empty()
                        && let Err(error) = tokio::task::spawn_blocking({
                            let root = project_root.to_path_buf();
                            let branch = branch.clone();
                            move || worktree::delete_branch(&root, &branch)
                        })
                        .await?
                    {
                        reason = format!("removed task worktree; branch delete failed: {error}");
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
                    entries.push(task_entry(
                        session,
                        &agent,
                        &worktree,
                        format!("remove failed: {error}"),
                    ));
                }
            }
        }
        Ok((scanned, removed, skipped, entries))
    }

    // ---------------------------------------------------------------------
    // Task contracts and intent authorization
    // ---------------------------------------------------------------------
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn init_repo(path: &Path) {
        std::fs::write(path.join("README.md"), "hello\n").unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "test@verlinpark.local"],
            vec!["config", "user.name", "VerlinPark Test"],
            vec!["add", "."],
            vec!["commit", "-q", "-m", "initial"],
        ] {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(path)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
    }

    async fn register_task_agent(
        engine: &Engine,
        session: &Session,
        name: &str,
        info: &crate::worktree::WorktreeInfo,
    ) {
        let agent = engine
            .create_agent(&session.id, "worker", name, None, None, "test")
            .await
            .unwrap();
        engine
            .set_agent_worktree(&agent.id, &info.path.to_string_lossy(), &info.branch)
            .await
            .unwrap();
        engine.update_agent_status(&agent.id, "idle").await.unwrap();
    }

    #[tokio::test]
    async fn retention_removes_merged_task_worktrees_and_keeps_unmerged_or_dirty_work() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("task-cleanup".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session(project.id.clone(), "task cleanup".into(), None)
            .await
            .unwrap();
        let session_worktree = PathBuf::from(session.worktree.clone().unwrap());
        let session_branch = session.branch.clone().unwrap();

        let merged = engine
            .create_task_worktree(
                &project.root,
                &project.id,
                &session.id,
                "T1",
                &session_branch,
            )
            .await
            .unwrap();
        std::fs::write(merged.path.join("merged.txt"), "merged\n").unwrap();
        worktree::commit_all(&merged.path, "task one").unwrap();
        worktree::merge_branch(&session_worktree, &merged.branch, "merge T1").unwrap();
        register_task_agent(&engine, &session, "Worker T1", &merged).await;

        let unmerged = engine
            .create_task_worktree(
                &project.root,
                &project.id,
                &session.id,
                "T2",
                &session_branch,
            )
            .await
            .unwrap();
        std::fs::write(unmerged.path.join("unmerged.txt"), "unmerged\n").unwrap();
        worktree::commit_all(&unmerged.path, "task two").unwrap();
        register_task_agent(&engine, &session, "Worker T2", &unmerged).await;

        let dirty = engine
            .create_task_worktree(
                &project.root,
                &project.id,
                &session.id,
                "T3",
                &session_branch,
            )
            .await
            .unwrap();
        std::fs::write(dirty.path.join("dirty.txt"), "dirty\n").unwrap();
        worktree::commit_all(&dirty.path, "task three").unwrap();
        worktree::merge_branch(&session_worktree, &dirty.branch, "merge T3").unwrap();
        std::fs::write(dirty.path.join("dirty.txt"), "dirty work\n").unwrap();
        register_task_agent(&engine, &session, "Worker T3", &dirty).await;

        engine
            .update_session_status(&session.id, "idle")
            .await
            .unwrap();

        let result = engine
            .cleanup_worktrees(&project.id, 0, false, false, true, false)
            .await
            .unwrap();
        assert!(
            !merged.path.exists(),
            "a merged, clean task worktree must be removed: {:?}",
            result.entries
        );
        assert!(
            unmerged.path.exists(),
            "an unmerged task worktree must be preserved: {:?}",
            result.entries
        );
        assert!(
            dirty.path.exists(),
            "a dirty task worktree must be preserved: {:?}",
            result.entries
        );
        assert!(!session_worktree.exists());
        assert!(
            !worktree::branch_exists(repo.path(), &merged.branch).unwrap(),
            "delete_branches also removes merged task branches"
        );
        assert!(
            worktree::branch_exists(repo.path(), &unmerged.branch).unwrap(),
            "an unmerged task branch is preserved"
        );
        assert!(
            result.entries.iter().any(|entry| {
                entry.worktree == merged.path.to_string_lossy().as_ref()
                    && entry.removed
                    && entry.reason.contains("task worktree")
            }),
            "task worktree removals must be visible in the cleanup result: {:?}",
            result.entries
        );
    }

    #[tokio::test]
    async fn task_worktree_retention_is_skipped_for_active_or_failed_sessions() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("task-cleanup-guard".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session(project.id.clone(), "task cleanup guard".into(), None)
            .await
            .unwrap();
        let session_worktree = PathBuf::from(session.worktree.clone().unwrap());
        let session_branch = session.branch.clone().unwrap();
        let task = engine
            .create_task_worktree(
                &project.root,
                &project.id,
                &session.id,
                "T1",
                &session_branch,
            )
            .await
            .unwrap();
        std::fs::write(task.path.join("done.txt"), "done\n").unwrap();
        worktree::commit_all(&task.path, "task").unwrap();
        worktree::merge_branch(&session_worktree, &task.branch, "merge T1").unwrap();
        register_task_agent(&engine, &session, "Worker T1", &task).await;

        engine
            .update_session_status(&session.id, "failed")
            .await
            .unwrap();
        let kept = engine
            .cleanup_worktrees(&project.id, 0, false, false, false, true)
            .await
            .unwrap();
        assert_eq!(kept.removed, 0);
        assert!(
            task.path.exists(),
            "keep_failed protects task worktrees too"
        );

        engine
            .update_session_status(&session.id, "running")
            .await
            .unwrap();
        let active = engine
            .cleanup_worktrees(&project.id, 0, false, true, true, false)
            .await
            .unwrap();
        assert!(
            task.path.exists(),
            "an active session's task worktrees are never swept: {:?}",
            active.entries
        );
    }
}

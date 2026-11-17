use super::*;

impl AgentService {
    /// Serialize a checkpoint with prompt/plan/compaction operations. The
    /// checkpoint itself is provider-free but reads Git state and messages, so
    /// it must not race a run.
    pub async fn checkpoint_now(
        &self,
        session_id: &str,
        label: Option<String>,
    ) -> Result<SessionCheckpoint> {
        self.begin_operation(session_id)?;
        let result = self.engine.checkpoint_session(session_id, label).await;
        self.end_operation(session_id);
        result
    }

    /// Serialize a worktree checkpoint restore with prompts/plans/compaction.
    pub async fn restore_checkpoint_now(
        &self,
        session_id: &str,
        checkpoint_id: &str,
    ) -> Result<SessionCheckpointRestoreResult> {
        self.begin_operation(session_id)?;
        let result = self
            .engine
            .restore_checkpoint(session_id, checkpoint_id)
            .await;
        self.end_operation(session_id);
        result
    }

    /// Serialize a checkpoint diff preview with prompts/plans/compaction.
    pub async fn diff_checkpoint_now(
        &self,
        session_id: &str,
        checkpoint_id: &str,
    ) -> Result<SessionCheckpointDiffResult> {
        self.begin_operation(session_id)?;
        let result = self.engine.diff_checkpoint(session_id, checkpoint_id).await;
        self.end_operation(session_id);
        result
    }

    /// Serialize a fork with prompt/plan/compaction operations on the source
    /// session. The new session is created only after the source is quiescent.
    pub async fn fork_now(
        &self,
        source_session_id: &str,
        checkpoint_id: Option<&str>,
        goal: Option<String>,
    ) -> Result<SessionForkResult> {
        self.begin_operation(source_session_id)?;
        let result = self
            .engine
            .fork_session(source_session_id, checkpoint_id, goal)
            .await;
        self.end_operation(source_session_id);
        result
    }

    /// Merge a direct fork's committed branch into its source session. The
    /// source worktree must be clean; conflicts are aborted and reported.
    pub async fn merge_fork_now(
        &self,
        source_session_id: &str,
        fork_session_id: &str,
    ) -> Result<SessionMergeForkResult> {
        self.begin_operations(&[source_session_id, fork_session_id])?;
        let result = async {
            let integration_command = self.config.read().ok().and_then(|config| {
                config
                    .as_ref()
                    .and_then(|config| config.integration_command.clone())
            });
            let (source, project) =
                load_session_project(self.engine.store().clone(), source_session_id).await?;
            let (fork, fork_project) =
                load_session_project(self.engine.store().clone(), fork_session_id).await?;
            if source.project_id != fork.project_id || project.id != fork_project.id {
                anyhow::bail!("sessions must belong to the same project");
            }
            crate::ensure_session_quiescent(&source, "merge a fork")?;
            crate::ensure_session_quiescent(&fork, "merge a fork")?;
            let lineage = self.engine.session_lineage(fork_session_id).await?;
            if lineage
                .parent
                .as_ref()
                .map(|parent| parent.session.id.as_str())
                != Some(source_session_id)
            {
                anyhow::bail!("fork is not a direct child of the source session");
            }
            self.merge_fork_inner(&source, &project, &fork, integration_command)
                .await
        }
        .await;
        self.end_operations(&[source_session_id, fork_session_id]);
        result
    }

    pub(in crate::agent) async fn merge_fork_inner(
        &self,
        source: &Session,
        project: &Project,
        fork: &Session,
        integration_command: Option<String>,
    ) -> Result<SessionMergeForkResult> {
        let source_worktree = PathBuf::from(
            source
                .worktree
                .clone()
                .ok_or_else(|| anyhow!("source session has no worktree"))?,
        );
        let source_branch = source
            .branch
            .clone()
            .ok_or_else(|| anyhow!("source session has no branch"))?;
        let fork_branch = fork
            .branch
            .clone()
            .ok_or_else(|| anyhow!("fork session has no branch"))?;
        if !source_worktree.is_dir() {
            anyhow::bail!("source session worktree is missing");
        }
        let git_lock = self.engine.git_lock.clone();
        let status_path = source_worktree.clone();
        let branch_path = source_worktree.clone();
        let head_path = source_worktree.clone();
        let expected_branch = source_branch.clone();
        let (status, branch, pre_merge_commit) = tokio::task::spawn_blocking(move || {
            let _guard = git_lock.lock().map_err(|_| anyhow!("git mutex poisoned"))?;
            Ok::<_, anyhow::Error>((
                worktree::status_porcelain(&status_path)?,
                worktree::current_branch(&branch_path)?,
                worktree::head_commit(&head_path)?,
            ))
        })
        .await??;
        if branch != expected_branch {
            anyhow::bail!("source worktree is on branch {branch}; expected {expected_branch}");
        }
        if !status.is_empty() {
            anyhow::bail!(
                "source worktree has uncommitted changes; commit or discard them before merging a fork"
            );
        }
        if let Some(fork_worktree) = &fork.worktree {
            let fork_path = PathBuf::from(fork_worktree);
            if fork_path.is_dir() {
                let git_lock = self.engine.git_lock.clone();
                let status = tokio::task::spawn_blocking(move || {
                    let _guard = git_lock.lock().map_err(|_| anyhow!("git mutex poisoned"))?;
                    worktree::status_porcelain(&fork_path)
                })
                .await??;
                if !status.is_empty() {
                    anyhow::bail!(
                        "fork worktree has uncommitted changes; commit them before merging"
                    );
                }
            }
        }
        let root = PathBuf::from(&project.root);
        let fork_branch_for_git = fork_branch.clone();
        let root_for_git = root.clone();
        let fork_commit = tokio::task::spawn_blocking(move || {
            worktree::git_output(&root_for_git, &["rev-parse", &fork_branch_for_git])
        })
        .await??;
        let message = format!("merge fork {}", fork.id);
        let merge_worktree = source_worktree.clone();
        let merge_lock = self.engine.git_lock.clone();
        let merge_outcome = tokio::task::spawn_blocking(move || {
            let _guard = merge_lock
                .lock()
                .map_err(|_| anyhow!("git mutex poisoned"))?;
            worktree::merge_fork_branch(&merge_worktree, &fork_branch, &message)
        })
        .await??;
        if !merge_outcome.merged {
            let detail = truncate(&format!("merge conflict: {}", merge_outcome.detail));
            self.record_fork_merge(
                source,
                fork,
                "conflict",
                &pre_merge_commit,
                &fork_commit,
                None,
                &merge_outcome.conflicts,
                &detail,
            )
            .await?;
            return Ok(SessionMergeForkResult {
                source_session_id: source.id.clone(),
                fork_session_id: fork.id.clone(),
                status: "conflict".to_string(),
                pre_merge_commit,
                fork_commit,
                merge_commit: None,
                conflicts: merge_outcome.conflicts,
                detail,
            });
        }

        let merge_worktree = source_worktree.clone();
        let check_base = pre_merge_commit.clone();
        let check_lock = self.engine.git_lock.clone();
        let check = tokio::task::spawn_blocking(move || {
            let _guard = check_lock
                .lock()
                .map_err(|_| anyhow!("git mutex poisoned"))?;
            worktree::check_merged_diff(&merge_worktree, &check_base)
        })
        .await?;
        let integration = match check {
            Ok(()) => {
                if let Some(command) = integration_command {
                    run_integration_command(
                        &self.engine,
                        project,
                        source,
                        source_worktree.clone(),
                        command.clone(),
                    )
                    .await
                } else {
                    Ok("diff check passed".to_string())
                }
            }
            Err(error) => Err(error),
        };
        let integration = match integration {
            Ok(detail) => detail,
            Err(error) => {
                let reset_worktree = source_worktree.clone();
                let reset_base = pre_merge_commit.clone();
                let reset_lock = self.engine.git_lock.clone();
                let reset = tokio::task::spawn_blocking(move || {
                    let _guard = reset_lock
                        .lock()
                        .map_err(|_| anyhow!("git mutex poisoned"))?;
                    worktree::reset_hard(&reset_worktree, &reset_base)
                })
                .await?;
                let detail = truncate(&match reset {
                    Ok(()) => format!("integration check failed: {error}"),
                    Err(reset_error) => {
                        format!("integration check failed: {error}; rollback failed: {reset_error}")
                    }
                });
                self.record_fork_merge(
                    source,
                    fork,
                    "failed",
                    &pre_merge_commit,
                    &fork_commit,
                    None,
                    &[],
                    &detail,
                )
                .await?;
                return Ok(SessionMergeForkResult {
                    source_session_id: source.id.clone(),
                    fork_session_id: fork.id.clone(),
                    status: "failed".to_string(),
                    pre_merge_commit,
                    fork_commit,
                    merge_commit: None,
                    conflicts: Vec::new(),
                    detail,
                });
            }
        };
        let head_path = source_worktree.clone();
        let merge_commit =
            tokio::task::spawn_blocking(move || worktree::head_commit(&head_path)).await??;
        let detail = truncate(&integration);
        self.record_fork_merge(
            source,
            fork,
            "merged",
            &pre_merge_commit,
            &fork_commit,
            Some(&merge_commit),
            &[],
            &detail,
        )
        .await?;
        Ok(SessionMergeForkResult {
            source_session_id: source.id.clone(),
            fork_session_id: fork.id.clone(),
            status: "merged".to_string(),
            pre_merge_commit,
            fork_commit,
            merge_commit: Some(merge_commit),
            conflicts: Vec::new(),
            detail,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::agent) async fn record_fork_merge(
        &self,
        source: &Session,
        fork: &Session,
        status: &str,
        pre_merge_commit: &str,
        fork_commit: &str,
        merge_commit: Option<&str>,
        conflicts: &[String],
        detail: &str,
    ) -> Result<()> {
        self.engine
            .append_event(
                &source.id,
                "session.fork.merge",
                serde_json::json!({
                    "fork_session_id": fork.id,
                    "status": status,
                    "pre_merge_commit": pre_merge_commit,
                    "fork_commit": fork_commit,
                    "merge_commit": merge_commit,
                    "conflicts": conflicts,
                    "detail": detail,
                }),
            )
            .await?;
        let _ = self
            .engine
            .append_event(
                &fork.id,
                "session.fork.merge",
                serde_json::json!({
                    "source_session_id": source.id,
                    "status": status,
                    "merge_commit": merge_commit,
                }),
            )
            .await;
        Ok(())
    }
}

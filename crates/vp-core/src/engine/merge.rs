use super::*;

impl Engine {
    #[allow(clippy::too_many_arguments)]
    pub async fn enqueue_merge(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        task_id: Option<String>,
        branch: &str,
        commit: &str,
        base_commit: &str,
        detail: Option<String>,
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
            vp_protocol::types::MergeQueueSource::local(),
        )
        .await
    }

    async fn reject_imported_session(&self, session_id: &str) -> Result<()> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        if session.status == "imported" {
            anyhow::bail!(
                "imported sessions are read-only archives; create a new session to continue"
            );
        }
        Ok(())
    }

    pub async fn discard_session(&self, session_id: &str) -> Result<()> {
        self.reject_imported_session(session_id).await?;
        let session = self.ensure_session_worktree(session_id).await?;
        ensure_session_quiescent(&session, "discard the session worktree")?;
        let worktree = session
            .worktree
            .clone()
            .ok_or_else(|| anyhow::anyhow!("session has no worktree"))?;
        self.stop_session_processes(&session.id, "session discard")
            .await;
        let worktree = PathBuf::from(worktree);
        let git_lock = self.git_lock.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = git_lock
                .lock()
                .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
            git_output(&worktree, &["reset", "--hard", "HEAD"])?;
            git_output(&worktree, &["clean", "-fd"])?;
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        self.append_event(&session.id, "session.discarded", serde_json::json!({}))
            .await?;
        Ok(())
    }

    pub async fn commit_session(&self, session_id: &str, message: &str) -> Result<SessionCommit> {
        self.reject_imported_session(session_id).await?;
        let message = message.trim().to_string();
        if message.is_empty() {
            return Err(anyhow::anyhow!("commit message is required"));
        }
        let session = self.ensure_session_worktree(session_id).await?;
        ensure_session_quiescent(&session, "commit the session worktree")?;
        let worktree = session
            .worktree
            .clone()
            .ok_or_else(|| anyhow::anyhow!("session has no worktree"))?;
        let session_id = session.id.clone();
        let worktree = PathBuf::from(worktree);
        let message_for_git = message.clone();
        let git_lock = self.git_lock.clone();
        let (commit, summary) = tokio::task::spawn_blocking(move || {
            let _guard = git_lock
                .lock()
                .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
            git_output(&worktree, &["add", "-A"])?;
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(&worktree)
                .args(["commit", "-m", &message_for_git])
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .context("running git commit")?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if stderr.contains("nothing to commit") || stdout.contains("nothing to commit") {
                    anyhow::bail!("no changes to commit");
                }
                anyhow::bail!("git commit failed: {stderr}");
            }
            let commit = git_output(&worktree, &["rev-parse", "--short", "HEAD"])?;
            let summary = git_output(
                &worktree,
                &["show", "--stat", "--oneline", "--no-color", "-1"],
            )?;
            Ok::<_, anyhow::Error>((commit, summary))
        })
        .await??;

        let commit_target = crate::effect::EffectTarget::new(
            crate::effect::kinds::GIT_COMMIT,
            session
                .branch
                .clone()
                .unwrap_or_else(|| "session".to_string()),
        );
        let commit_request = crate::effect::canonical_request_hash(
            "session.commit",
            serde_json::json!({
                "branch": session.branch.clone(),
                "commit": commit.clone(),
                "message_hash": crate::sha256_hex(message.as_bytes()),
            }),
        )?;
        let mut commit_effect = crate::effect::runtime_receipt(
            &session_id,
            None,
            "session.commit",
            commit_target,
            commit_request,
            Some("workspace".to_string()),
        );
        commit_effect.state = EffectState::Done;
        commit_effect.external_id = Some(commit.clone());
        commit_effect.evidence = vp_protocol::types::EffectEvidence {
            probe: crate::effect::probes::GIT_REV_PARSE.to_string(),
            outcome: "observed".to_string(),
            commit: Some(commit.clone()),
            branch: session.branch.clone(),
            ..Default::default()
        };
        self.record_settled_effect(commit_effect).await?;
        self.append_event(
            &session_id,
            "session.committed",
            serde_json::json!({
                "commit": commit,
                "message": message,
            }),
        )
        .await?;

        Ok(SessionCommit {
            session_id,
            commit,
            message,
            summary,
        })
    }

    pub async fn land_session(&self, session_id: &str) -> Result<SessionLandResult> {
        self.reject_imported_session(session_id).await?;
        let session = self.ensure_session_worktree(session_id).await?;
        ensure_session_quiescent(&session, "land the session")?;
        if session.status == "landed" {
            anyhow::bail!("session is already landed");
        }
        self.assert_effects_settled(&session.id, None).await?;
        let session_branch = session
            .branch
            .clone()
            .ok_or_else(|| anyhow::anyhow!("session has no branch"))?;
        let session_worktree = PathBuf::from(
            session
                .worktree
                .clone()
                .ok_or_else(|| anyhow::anyhow!("session has no worktree"))?,
        );
        let head_worktree = session_worktree.clone();
        let session_head =
            tokio::task::spawn_blocking(move || worktree::head_commit(&head_worktree)).await??;
        let status = tokio::task::spawn_blocking(move || {
            git_output(&session_worktree, &["status", "--porcelain"])
        })
        .await??;
        if !status.is_empty() {
            anyhow::bail!("session worktree has uncommitted changes; commit or discard them first");
        }
        let store = self.store.clone();
        let project_id = session.project_id.clone();
        let project = tokio::task::spawn_blocking(move || store.get_project(&project_id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("project not found"))?;
        let target_branch = project.default_branch.clone();
        let project_root = PathBuf::from(&project.root);
        let branch_for_merge = session_branch.clone();
        let target_for_merge = target_branch.clone();
        let effect_target = crate::effect::EffectTarget::new(
            crate::effect::kinds::GIT_MERGE,
            format!("land:{target_branch}:{session_branch}"),
        );
        let effect_request = crate::effect::canonical_request_hash(
            "session.land",
            serde_json::json!({
                "branch": session_branch.clone(),
                "target_branch": target_branch.clone(),
                "head": session_head.clone(),
            }),
        )?;
        let existing = self
            .lookup_effect(
                &session.id,
                None,
                "session.land",
                &effect_target.canonical(),
                &effect_request,
            )
            .await?;
        if let Some(receipt) = existing.as_ref()
            && receipt.state == EffectState::Done
        {
            let commit = receipt
                .external_id
                .clone()
                .or_else(|| receipt.evidence.commit.clone())
                .unwrap_or_else(|| session_head.clone());
            return Ok(SessionLandResult {
                session_id: session.id,
                branch: session_branch,
                target_branch,
                commit,
                detail:
                    "already landed; the stored effect receipt was replayed without merging again"
                        .to_string(),
            });
        }
        let intent = crate::effect::runtime_receipt(
            &session.id,
            None,
            "session.land",
            effect_target,
            effect_request,
            Some("workspace".to_string()),
        );
        let (intent, inserted) = self.begin_effect(intent).await?;
        if !inserted && intent.state != EffectState::Intent {
            return Err(anyhow::Error::new(crate::effect::EffectBlocked {
                session_id: session.id.clone(),
                task_id: None,
                receipt_ids: vec![intent.id.clone()],
                message: format!(
                    "EFFECT_UNKNOWN: landing is blocked by effect receipt {} ({})",
                    intent.id,
                    intent.state.as_str()
                ),
            }));
        }
        let git_lock = self.git_lock.clone();
        let merge = tokio::task::spawn_blocking(move || -> Result<(String, String)> {
            let _guard = git_lock
                .lock()
                .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
            let status = git_output(&project_root, &["status", "--porcelain"])?;
            if !status.is_empty() {
                anyhow::bail!(
                    "project worktree has uncommitted changes; commit or stash them first"
                );
            }
            let current = git_output(&project_root, &["branch", "--show-current"])?;
            if current != target_for_merge {
                anyhow::bail!("project is on branch '{current}', expected '{target_for_merge}'");
            }
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(&project_root)
                .args([
                    "merge",
                    "--no-ff",
                    &branch_for_merge,
                    "-m",
                    &format!("land {branch_for_merge}"),
                ])
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .context("running git merge")?;
            if !output.status.success() {
                let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
                let _ = git_output(&project_root, &["merge", "--abort"]);
                anyhow::bail!("landing merge failed: {detail}");
            }
            Ok((
                git_output(&project_root, &["rev-parse", "--short", "HEAD"])?,
                worktree::head_commit(&project_root)?,
            ))
        })
        .await?;
        let (commit, full_commit) = match merge {
            Ok(value) => value,
            Err(error) => {
                let mut failed = intent.clone();
                failed.state = EffectState::Failed;
                failed.evidence = vp_protocol::types::EffectEvidence {
                    probe: "git_merge_head".to_string(),
                    outcome: "mismatch".to_string(),
                    reason: Some("landing_merge_failed".to_string()),
                    branch: Some(session_branch.clone()),
                    ..Default::default()
                };
                let _ = self.settle_effect(failed).await;
                return Err(error);
            }
        };
        let mut settled = intent;
        settled.state = EffectState::Done;
        settled.external_id = Some(commit.clone());
        settled.response_hash = Some(crate::sha256_hex(full_commit.as_bytes()));
        settled.evidence = vp_protocol::types::EffectEvidence {
            probe: "git_merge_head".to_string(),
            outcome: "observed".to_string(),
            commit: Some(full_commit),
            branch: Some(session_branch.clone()),
            ..Default::default()
        };
        self.settle_effect(settled).await?;

        self.append_event(
            &session.id,
            "session.landed",
            serde_json::json!({
                "branch": session_branch,
                "target_branch": target_branch,
                "commit": commit,
            }),
        )
        .await?;
        self.update_session_status(&session.id, "landed").await?;
        Ok(SessionLandResult {
            session_id: session.id,
            branch: session_branch,
            target_branch,
            commit,
            detail: "landed into the project default branch".to_string(),
        })
    }

    pub async fn open_pull_request(&self, session_id: &str) -> Result<PullRequestResult> {
        self.open_pull_request_with_provider(session_id, None).await
    }

    /// Open a pull request with an optional explicit provider. Explicit input
    /// wins over `VERLINPARK_PR_PROVIDER`, which wins over origin detection.
    pub async fn open_pull_request_with_provider(
        &self,
        session_id: &str,
        provider: Option<&str>,
    ) -> Result<PullRequestResult> {
        self.reject_imported_session(session_id).await?;
        let origin = self.pull_request_origin(session_id).await?;
        let environment = std::env::var("VERLINPARK_PR_PROVIDER").ok();
        let target = crate::forge::resolve_target(provider, environment.as_deref(), &origin)?;
        let bitbucket = if target.provider == PrCli::BitbucketCloud {
            Some(crate::forge::BitbucketCloudProvider::from_env(
                crate::forge::BitbucketCloudConfig::default(),
            )?)
        } else {
            None
        };
        self.open_pull_request_with_target(session_id, target, bitbucket, None)
            .await
    }

    async fn pull_request_origin(&self, session_id: &str) -> Result<String> {
        let session = self.ensure_session_worktree(session_id).await?;
        let store = self.store.clone();
        let project_id = session.project_id;
        let project = tokio::task::spawn_blocking(move || store.get_project(&project_id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("project not found"))?;
        let root = PathBuf::from(&project.root);
        tokio::task::spawn_blocking(move || {
            git_output(&root, &["remote", "get-url", "origin"])
                .map(|origin| origin.trim().to_string())
                .context("a git origin remote is required to open a PR")
        })
        .await?
    }

    /// Testable CLI entry point. The fake CLI path is explicit while the
    /// provider preflight/push/create sequence remains the production one.
    #[cfg(test)]
    pub(crate) async fn open_pull_request_with_cli(
        &self,
        session_id: &str,
        cli_bin: &str,
        kind: PrCli,
    ) -> Result<PullRequestResult> {
        let target = crate::forge::ForgeTarget {
            provider: kind,
            origin: None,
            selected_by: crate::forge::ForgeSelectionSource::Explicit,
        };
        self.open_pull_request_with_target(session_id, target, None, Some(cli_bin.to_string()))
            .await
    }

    pub(crate) async fn open_pull_request_with_target(
        &self,
        session_id: &str,
        target: crate::forge::ForgeTarget,
        bitbucket: Option<crate::forge::BitbucketCloudProvider>,
        cli_binary: Option<String>,
    ) -> Result<PullRequestResult> {
        let session = self.ensure_session_worktree(session_id).await?;
        if session.status == "running" {
            anyhow::bail!("cannot open a PR while the session is running");
        }
        let branch = session
            .branch
            .clone()
            .ok_or_else(|| anyhow::anyhow!("session has no branch"))?;
        let session_worktree = PathBuf::from(
            session
                .worktree
                .clone()
                .ok_or_else(|| anyhow::anyhow!("session has no worktree"))?,
        );
        let session_worktree_for_status = session_worktree.clone();
        let status = tokio::task::spawn_blocking(move || {
            git_output(&session_worktree_for_status, &["status", "--porcelain"])
        })
        .await??;
        if !status.is_empty() {
            anyhow::bail!("session worktree has uncommitted changes; commit or discard them first");
        }

        let store = self.store.clone();
        let project_id = session.project_id.clone();
        let project = tokio::task::spawn_blocking(move || store.get_project(&project_id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("project not found"))?;
        let project_root = PathBuf::from(&project.root);
        let base_branch = project.default_branch.clone();
        let title = session
            .goal
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("VerlinPark change")
            .chars()
            .take(120)
            .collect::<String>();
        let description = format!(
            "Opened from VerlinPark session {}.\n\nGoal: {}",
            session.id, session.goal
        );
        let request = crate::forge::ForgeChangeRequest::new(
            title,
            description,
            branch.clone(),
            base_branch.clone(),
        );

        // Local safety checks happen before any forge network preflight so a
        // branch with no commits, or a dirty worktree, never causes a push or a
        // provider call.
        let project_root_for_check = project_root.clone();
        let branch_for_check = branch.clone();
        let base_for_check = base_branch.clone();
        let git_lock = self.git_lock.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let _guard = git_lock
                .lock()
                .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
            git_output(&project_root_for_check, &["remote", "get-url", "origin"])
                .context("a git origin remote is required to open a PR")?;
            let ahead = git_output(
                &project_root_for_check,
                &[
                    "rev-list",
                    "--count",
                    &format!("{base_for_check}..{branch_for_check}"),
                ],
            )?;
            if ahead.trim() == "0" {
                anyhow::bail!("session branch has no commits ahead of {base_for_check}");
            }
            Ok(())
        })
        .await??;

        let provider: Box<dyn crate::forge::ForgeProvider> = match target.provider {
            PrCli::BitbucketCloud => Box::new(bitbucket.ok_or_else(|| {
                anyhow::anyhow!("the Bitbucket Cloud client was not initialized")
            })?),
            kind => {
                let binary = match cli_binary {
                    Some(binary) => binary,
                    None => kind.resolve_cli().ok_or_else(|| {
                        anyhow::anyhow!("no CLI configured for the {kind} provider")
                    })?,
                };
                // Gitea/Forgejo run from the session worktree where the branch
                // is checked out; GitHub/GitLab keep the project-root cwd.
                let create_cwd = if kind == PrCli::Gitea {
                    session_worktree.clone()
                } else {
                    project_root.clone()
                };
                Box::new(crate::forge::CliForgeProvider::new(
                    kind,
                    binary,
                    project_root.clone(),
                    create_cwd,
                ))
            }
        };

        // Preflight (credential/repository check) happens before the push where
        // practical, so missing Bitbucket credentials or an inaccessible repo
        // never leaves a pushed branch behind.
        let preflight = provider.preflight(&target).await?;
        let repository = preflight.repository.clone().or_else(|| target.repository());
        let provider_id = target.provider.id().to_string();
        let repository_key = repository
            .clone()
            .unwrap_or_else(|| format!("origin:{branch}"));
        let head_worktree = project_root.clone();
        let head_branch = branch.clone();
        let head_commit = tokio::task::spawn_blocking(move || {
            git_output(&head_worktree, &["rev-parse", &head_branch])
        })
        .await??;

        // --- git push effect -------------------------------------------------
        let push_target = crate::effect::EffectTarget::new(
            crate::effect::kinds::GIT_PUSH,
            format!("{provider_id}:{repository_key}:{branch}"),
        );
        let push_request = crate::effect::canonical_request_hash(
            "session.pr.push",
            serde_json::json!({
                "provider": provider_id.clone(),
                "repository": repository_key.clone(),
                "branch": branch.clone(),
                "commit": head_commit.clone(),
            }),
        )?;
        let existing_push = self
            .lookup_effect(
                &session.id,
                None,
                "session.pr.push",
                &push_target.canonical(),
                &push_request,
            )
            .await?;
        let push_done = matches!(
            existing_push.as_ref().map(|receipt| receipt.state),
            Some(EffectState::Done)
        );
        if let Some(receipt) = existing_push.as_ref()
            && matches!(receipt.state, EffectState::Intent | EffectState::Unknown)
        {
            return Err(anyhow::Error::new(crate::effect::EffectBlocked {
                session_id: session.id.clone(),
                task_id: None,
                receipt_ids: vec![receipt.id.clone()],
                message: format!(
                    "EFFECT_UNKNOWN: a previous push of {branch} is {}; reconcile it before pushing again",
                    receipt.state.as_str()
                ),
            }));
        }
        let mut push_intent = None;
        if !push_done {
            let intent = crate::effect::runtime_receipt(
                &session.id,
                None,
                "session.pr.push",
                push_target,
                push_request,
                Some(provider_id.clone()),
            );
            let (intent, inserted) = self.begin_effect(intent).await?;
            if !inserted && intent.state != EffectState::Intent {
                return Err(anyhow::Error::new(crate::effect::EffectBlocked {
                    session_id: session.id.clone(),
                    task_id: None,
                    receipt_ids: vec![intent.id.clone()],
                    message: format!(
                        "EFFECT_UNKNOWN: a previous push of {branch} is {}; reconcile it before pushing again",
                        intent.state.as_str()
                    ),
                }));
            }
            push_intent = Some(intent);
        }

        let project_root_for_push = project_root.clone();
        let branch_for_push = branch.clone();
        let base_for_push_check = base_branch.clone();
        let git_lock = self.git_lock.clone();
        let push_result = tokio::task::spawn_blocking(move || -> Result<()> {
            let _guard = git_lock
                .lock()
                .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
            git_output(&project_root_for_push, &["remote", "get-url", "origin"])
                .context("a git origin remote is required to open a PR")?;
            // Re-check after preflight (which may wait on the network) so a
            // concurrently reset branch never gets pushed as an empty update.
            let ahead = git_output(
                &project_root_for_push,
                &[
                    "rev-list",
                    "--count",
                    &format!("{base_for_push_check}..{branch_for_push}"),
                ],
            )?;
            if ahead.trim() == "0" {
                anyhow::bail!("session branch has no commits ahead of {base_for_push_check}");
            }
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(&project_root_for_push)
                .args(["push", "-u", "origin", &branch_for_push])
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .context("running git push")?;
            if !output.status.success() {
                anyhow::bail!(
                    "git push failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
            Ok(())
        })
        .await?;
        if let Some(intent) = push_intent {
            let mut settled = intent;
            match &push_result {
                Ok(()) => {
                    settled.state = EffectState::Done;
                    settled.external_id = Some(head_commit.clone());
                    settled.evidence = vp_protocol::types::EffectEvidence {
                        probe: crate::effect::probes::GIT_LS_REMOTE.to_string(),
                        outcome: "declared".to_string(),
                        branch: Some(branch.clone()),
                        commit: Some(head_commit.clone()),
                        note: Some(
                            "the push command completed; reconcile if the remote head is disputed"
                                .to_string(),
                        ),
                        ..Default::default()
                    };
                }
                Err(error) => {
                    let unknown = push_error_is_ambiguous(error);
                    settled.state = if unknown {
                        EffectState::Unknown
                    } else {
                        EffectState::Failed
                    };
                    settled.evidence = vp_protocol::types::EffectEvidence {
                        probe: crate::effect::probes::GIT_LS_REMOTE.to_string(),
                        outcome: if unknown { "unverifiable" } else { "mismatch" }.to_string(),
                        reason: Some(if unknown {
                            crate::effect::reasons::TRANSPORT_UNKNOWN.to_string()
                        } else {
                            "git_push_failed".to_string()
                        }),
                        branch: Some(branch.clone()),
                        ..Default::default()
                    };
                }
            }
            self.settle_effect(settled).await?;
        }
        push_result?;

        self.append_event(
            &session.id,
            if push_done {
                "session.push_replayed"
            } else {
                "session.pushed"
            },
            serde_json::json!({
                "branch": branch.clone(),
                "base_branch": base_branch.clone(),
                "provider": provider_id.clone(),
                "provider_selected_by": selection_source_id(target.selected_by),
                "repository": repository.clone(),
                "effect_receipt_id": existing_push.as_ref().map(|receipt| receipt.id.clone()),
                "effect_replayed": push_done,
            }),
        )
        .await?;

        // --- forge PR effect -------------------------------------------------
        let pr_target = crate::effect::EffectTarget::new(
            crate::effect::kinds::FORGE_PR,
            format!("{provider_id}/{repository_key}:{branch}->{base_branch}"),
        );
        let pr_request = crate::effect::canonical_request_hash(
            "session.pr.create",
            serde_json::json!({
                "provider": provider_id.clone(),
                "repository": repository_key.clone(),
                "branch": branch.clone(),
                "base_branch": base_branch.clone(),
                "commit": head_commit.clone(),
            }),
        )?;
        let existing_pr = self
            .lookup_effect(
                &session.id,
                None,
                "session.pr.create",
                &pr_target.canonical(),
                &pr_request,
            )
            .await?;
        if let Some(receipt) = existing_pr.as_ref() {
            match receipt.state {
                EffectState::Done => {
                    let url = receipt
                        .external_id
                        .clone()
                        .filter(|value| !value.is_empty());
                    return Ok(PullRequestResult {
                        session_id: session.id,
                        branch,
                        base_branch,
                        pushed: true,
                        url: url.clone(),
                        detail: format!(
                            "the stored effect receipt for this branch was replayed instead of opening a duplicate {}",
                            receipt.evidence.change.as_deref().unwrap_or("change")
                        ),
                        provider: Some(provider_id),
                        repository: receipt.evidence.repository.clone().or(repository),
                        change: receipt.evidence.change.clone(),
                    });
                }
                EffectState::Intent | EffectState::Unknown => {
                    return Err(anyhow::Error::new(crate::effect::EffectBlocked {
                        session_id: session.id.clone(),
                        task_id: None,
                        receipt_ids: vec![receipt.id.clone()],
                        message: format!(
                            "EFFECT_UNKNOWN: a previous PR/MR creation for {branch} is {}; reconcile it before trying again",
                            receipt.state.as_str()
                        ),
                    }));
                }
                EffectState::Failed => {}
            }
        }
        let pr_intent = crate::effect::runtime_receipt(
            &session.id,
            None,
            "session.pr.create",
            pr_target,
            pr_request,
            Some(provider_id.clone()),
        );
        let (pr_intent, inserted) = self.begin_effect(pr_intent).await?;
        if !inserted && pr_intent.state != EffectState::Intent {
            return Err(anyhow::Error::new(crate::effect::EffectBlocked {
                session_id: session.id.clone(),
                task_id: None,
                receipt_ids: vec![pr_intent.id.clone()],
                message: format!(
                    "EFFECT_UNKNOWN: a previous PR/MR creation for {branch} is {}; reconcile it before trying again",
                    pr_intent.state.as_str()
                ),
            }));
        }
        let change = match provider.create(&target, &request).await {
            Ok(change) => {
                let mut settled = pr_intent;
                settled.state = EffectState::Done;
                settled.external_id = Some(change.url.clone());
                settled.evidence = vp_protocol::types::EffectEvidence {
                    probe: crate::effect::probes::FORGE_RESPONSE.to_string(),
                    outcome: "observed".to_string(),
                    branch: Some(branch.clone()),
                    commit: Some(head_commit.clone()),
                    external_id: Some(change.url.clone()),
                    change: Some(change.change_name.to_string()),
                    repository: change.repository.clone().or_else(|| repository.clone()),
                    caveat: Some(
                        "the forge response proves the local client observed a change; it is not a provider exactly-once guarantee"
                            .to_string(),
                    ),
                    ..Default::default()
                };
                self.settle_effect(settled).await?;
                change
            }
            Err(error) => {
                let unknown = forge_error_is_ambiguous(&error);
                let mut settled = pr_intent;
                settled.state = if unknown {
                    EffectState::Unknown
                } else {
                    EffectState::Failed
                };
                settled.evidence = vp_protocol::types::EffectEvidence {
                    probe: crate::effect::probes::FORGE_RESPONSE.to_string(),
                    outcome: if unknown { "unverifiable" } else { "mismatch" }.to_string(),
                    reason: Some(if unknown {
                        crate::effect::reasons::TRANSPORT_UNKNOWN.to_string()
                    } else {
                        "forge_create_failed".to_string()
                    }),
                    branch: Some(branch.clone()),
                    commit: Some(head_commit.clone()),
                    repository: repository.clone(),
                    ..Default::default()
                };
                self.settle_effect(settled).await?;
                // The branch is already on the remote at this point. Make the
                // failure detail honest without claiming the PR exists.
                return Err(anyhow::Error::new(
                    error.prefix_message("branch pushed, but "),
                ));
            }
        };

        let mut event = serde_json::json!({
            "branch": branch,
            "base_branch": base_branch,
            "url": change.url,
            "provider": change.provider.id(),
            "repository": change.repository,
            "change": change.change_name,
        });
        if let Some(id) = change.id {
            event["change_id"] = serde_json::json!(id);
        }
        self.append_event(&session.id, "session.pr_opened", event)
            .await?;
        Ok(PullRequestResult {
            session_id: session.id,
            branch,
            base_branch,
            pushed: true,
            url: Some(change.url.clone()),
            detail: format!(
                "branch pushed and {} opened at {} via {}",
                change.change_name,
                change.url,
                change.provider.display_name()
            ),
            provider: Some(change.provider.id().to_string()),
            repository: change.repository,
            change: Some(change.change_name.to_string()),
        })
    }

    /// Read-only forge inventory for CLI/Settings. It does not spawn a CLI or
    /// make a provider request, and it never returns credential values.
    pub async fn forge_status(&self, project_id: Option<String>) -> Result<ForgeStatusResult> {
        let provider_override = std::env::var("VERLINPARK_PR_PROVIDER")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .map(|value| crate::forge::sanitize_status_text(&value, 64));
        let parsed_override = provider_override.as_deref().map(PrCli::parse).transpose();
        let override_error = parsed_override
            .as_ref()
            .err()
            .map(|error| error.message.clone());
        let override_kind = parsed_override.ok().flatten();

        let mut detected_provider = PrCli::GitHub;
        let mut origin_status = None;
        if let Some(project_id) = project_id {
            let store = self.store.clone();
            let project = tokio::task::spawn_blocking(move || store.get_project(&project_id))
                .await??
                .ok_or_else(|| anyhow::anyhow!("project not found"))?;
            let root = PathBuf::from(&project.root);
            let origin = tokio::task::spawn_blocking(move || {
                git_output(&root, &["remote", "get-url", "origin"]).ok()
            })
            .await?
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
            if let Some(origin) = origin {
                let inspection = crate::forge::inspect_origin(&origin);
                detected_provider = inspection
                    .provider
                    .unwrap_or_else(|| detect_pr_cli(&origin));
                origin_status = Some(ForgeOriginStatus {
                    host: inspection.host,
                    provider: inspection
                        .provider
                        .map(|provider| provider.id().to_string()),
                    repository: inspection.repository,
                    supported: inspection.supported,
                    server_detected: inspection.server_detected,
                    detail: inspection.detail,
                });
            }
        }

        let effective = override_kind.unwrap_or(detected_provider);
        let providers = crate::forge::provider_snapshots()
            .into_iter()
            .map(|snapshot| ForgeProviderStatus {
                provider: snapshot.provider.id().to_string(),
                change_name: snapshot.provider.change_name().to_string(),
                transport: snapshot.provider.transport().to_string(),
                command: snapshot.command,
                command_available: snapshot.command_available,
                auth_configured: snapshot.auth_configured,
                credential_source: snapshot.credential_source,
                supported: snapshot.supported,
                detail: snapshot.detail,
            })
            .collect::<Vec<_>>();
        let detail = match (&provider_override, override_error) {
            (Some(raw), Some(error)) => format!(
                "VERLINPARK_PR_PROVIDER='{raw}' is invalid ({error}); pull request creation fails with FORGE_UNSUPPORTED_PROVIDER until the override is corrected"
            ),
            (Some(_), None) => format!(
                "VERLINPARK_PR_PROVIDER selects {effective}; credentials are read from the daemon environment only"
            ),
            (None, _) => format!(
                "no provider override; {effective} is selected from the origin; credentials are read from the daemon environment only"
            ),
        };
        Ok(ForgeStatusResult {
            provider_override,
            default_provider: effective.id().to_string(),
            origin: origin_status,
            providers,
            detail,
        })
    }

    /// Run the provider's read-only preflight for a project origin without
    /// pushing a branch or creating a pull request. This is the explicit
    /// `forge.preflight` operator surface; it is never called by a scheduler.
    pub async fn forge_preflight(
        &self,
        project_id: &str,
        provider: Option<&str>,
    ) -> Result<ForgePreflightResult> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        let project = tokio::task::spawn_blocking(move || store.get_project(&project_id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("project not found"))?;
        let root = PathBuf::from(&project.root);
        let root_for_origin = root.clone();
        let origin = tokio::task::spawn_blocking(move || {
            git_output(&root_for_origin, &["remote", "get-url", "origin"])
                .map(|origin| origin.trim().to_string())
                .context("a git origin remote is required to preflight a forge provider")
        })
        .await??;
        let environment = std::env::var("VERLINPARK_PR_PROVIDER").ok();
        let target = crate::forge::resolve_target(provider, environment.as_deref(), &origin)?;
        let provider_impl: Box<dyn crate::forge::ForgeProvider> = match target.provider {
            PrCli::BitbucketCloud => Box::new(crate::forge::BitbucketCloudProvider::from_env(
                crate::forge::BitbucketCloudConfig::default(),
            )?),
            kind => {
                let binary = kind
                    .resolve_cli()
                    .ok_or_else(|| anyhow::anyhow!("no CLI configured for the {kind} provider"))?;
                Box::new(crate::forge::CliForgeProvider::new(
                    kind,
                    binary,
                    root.clone(),
                    root,
                ))
            }
        };
        self.run_forge_preflight(&target, provider_impl.as_ref())
            .await
    }

    pub(crate) async fn run_forge_preflight(
        &self,
        target: &crate::forge::ForgeTarget,
        provider: &dyn crate::forge::ForgeProvider,
    ) -> Result<ForgePreflightResult> {
        let preflight = provider.preflight(target).await?;
        Ok(ForgePreflightResult {
            provider: preflight.provider.id().to_string(),
            change_name: preflight.provider.change_name().to_string(),
            repository: preflight.repository.or_else(|| target.repository()),
            auth: preflight.auth,
            detail: preflight.detail,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn enqueue_merge_with_source(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        task_id: Option<String>,
        branch: &str,
        commit: &str,
        base_commit: &str,
        detail: Option<String>,
        source: vp_protocol::types::MergeQueueSource,
    ) -> Result<MergeQueueEntry> {
        // Fail closed before a queue row exists. Integration re-checks the
        // same gate under the integration lease in case an effect becomes
        // unresolved while the entry waits.
        self.assert_effects_settled(session_id, task_id.as_deref())
            .await?;
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let agent_id = agent_id.to_string();
        let agent_name = agent_name.to_string();
        let branch = branch.to_string();
        let commit = commit.to_string();
        let base_commit = base_commit.to_string();
        let (entry, event) = tokio::task::spawn_blocking(move || {
            store.enqueue_merge_with_event_source(
                &session_id,
                &agent_id,
                &agent_name,
                task_id.as_deref(),
                &branch,
                &commit,
                &base_commit,
                detail.as_deref(),
                &source,
            )
        })
        .await??;
        let _ = self.events.send(event);
        Ok(entry)
    }

    pub async fn set_merge_queue_verification(
        &self,
        id: &str,
        approved: Option<bool>,
        summary: Option<String>,
        environment_caveat: Option<String>,
    ) -> Result<Option<MergeQueueEntry>> {
        let store = self.store.clone();
        let id = id.to_string();
        let updated = tokio::task::spawn_blocking(move || {
            store.set_merge_queue_verification_with_event(
                &id,
                approved,
                summary.as_deref(),
                environment_caveat.as_deref(),
            )
        })
        .await??;
        let entry = updated.map(|(entry, event)| {
            let _ = self.events.send(event);
            entry
        });
        Ok(entry)
    }

    pub async fn begin_merge_queue_entry(
        &self,
        id: &str,
        pre_merge_commit: &str,
    ) -> Result<Option<MergeQueueEntry>> {
        let store = self.store.clone();
        let id = id.to_string();
        let pre_merge_commit = pre_merge_commit.to_string();
        tokio::task::spawn_blocking(move || store.begin_merge_queue_entry(&id, &pre_merge_commit))
            .await?
    }

    pub async fn update_merge_queue_entry(
        &self,
        id: &str,
        status: &str,
        detail: Option<String>,
    ) -> Result<Option<MergeQueueEntry>> {
        let store = self.store.clone();
        let id = id.to_string();
        let status = status.to_string();
        let updated = tokio::task::spawn_blocking(move || {
            store.update_merge_queue_entry_with_event(&id, &status, detail.as_deref())
        })
        .await??;
        let entry = updated.map(|(entry, event)| {
            let _ = self.events.send(event);
            entry
        });
        Ok(entry)
    }

    pub async fn list_merge_queue(&self, session_id: &str) -> Result<Vec<MergeQueueEntry>> {
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.list_merge_queue(&id)).await?
    }

    pub async fn get_merge_queue_entry(&self, id: &str) -> Result<Option<MergeQueueEntry>> {
        let store = self.store.clone();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || store.get_merge_queue_entry(&id)).await?
    }

    pub async fn retry_merge_queue_entry(
        &self,
        id: &str,
        commit: &str,
        base_commit: &str,
        detail: Option<String>,
    ) -> Result<Option<MergeQueueEntry>> {
        let store = self.store.clone();
        let id = id.to_string();
        let commit = commit.to_string();
        let base_commit = base_commit.to_string();
        let updated = tokio::task::spawn_blocking(move || {
            store.retry_merge_queue_entry_with_event(&id, &commit, &base_commit, detail.as_deref())
        })
        .await??;
        let entry = updated.map(|(entry, event)| {
            let _ = self.events.send(event);
            entry
        });
        Ok(entry)
    }
}

fn detect_pr_cli(origin: &str) -> PrCli {
    crate::forge::detect_provider(origin)
}

fn selection_source_id(source: crate::forge::ForgeSelectionSource) -> &'static str {
    match source {
        crate::forge::ForgeSelectionSource::Explicit => "explicit",
        crate::forge::ForgeSelectionSource::Environment => "environment",
        crate::forge::ForgeSelectionSource::Detected => "detected",
    }
}

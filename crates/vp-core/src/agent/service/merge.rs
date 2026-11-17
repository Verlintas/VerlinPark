use super::*;

impl AgentService {
    /// Import one remote branch through a bounded Git bundle, verify the
    /// imported commit locally, and enqueue it through the existing
    /// deterministic Integrator.
    ///
    /// This is operator-initiated and policy-gated. It never duplicates a
    /// local Worker's authority: the remote commit is verified in the same
    /// isolated clone, merged with `--no-ff`, and conflict/retry uses the same
    /// persisted queue. The remote host is trusted infrastructure; the bundle
    /// and the imported ref are the only transport surface.
    pub async fn integrate_remote(
        &self,
        params: RemoteIntegrationParams,
    ) -> Result<RemoteIntegrationResult> {
        let session_id = params.session_id.clone();
        self.begin_operation(&session_id)?;
        let result = self.integrate_remote_inner(params, None).await;
        self.end_operation(&session_id);
        result
    }

    /// Integrate a bundle that is already on the local disk. The bytes are
    /// still checked against `bundle.sha256`, the bundle is still verified, and
    /// the imported head must still match `bundle.head`. This exists for
    /// offline/operator workflows and deterministic contract tests; the daemon
    /// RPC always downloads through the helper first.
    pub async fn integrate_remote_bundle_file(
        &self,
        params: RemoteIntegrationParams,
        bundle_path: PathBuf,
        bundle: RemoteBundleSummary,
    ) -> Result<RemoteIntegrationResult> {
        let session_id = params.session_id.clone();
        self.begin_operation(&session_id)?;
        let result = self
            .integrate_remote_inner(params, Some((bundle_path, bundle)))
            .await;
        self.end_operation(&session_id);
        result
    }

    pub(in crate::agent) async fn integrate_remote_inner(
        &self,
        params: RemoteIntegrationParams,
        downloaded: Option<(PathBuf, RemoteBundleSummary)>,
    ) -> Result<RemoteIntegrationResult> {
        let branch = remote_integration::validate_remote_branch(&params.branch)
            .map_err(anyhow::Error::new)?;
        let import_id = Uuid::now_v7().to_string();
        remote_integration::validate_import_id(&import_id).map_err(anyhow::Error::new)?;
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned());
        let (session, project) =
            load_session_project(self.engine.store().clone(), &params.session_id).await?;
        if session.project_id != params.project_id {
            anyhow::bail!(
                "session {} belongs to project {}, not {}",
                session.id,
                session.project_id,
                params.project_id
            );
        }
        if session.status == "landed" {
            anyhow::bail!("session is already landed");
        }
        crate::ensure_session_quiescent(&session, "integrate a remote branch")?;
        let host_config = self.engine.get_execution_host(&params.project_id).await?;
        let policy = self
            .engine
            .get_remote_integration_policy(&params.project_id)
            .await?;
        remote_integration::authorize(&policy, &host_config, params.confirm)
            .map_err(anyhow::Error::new)?;
        if let Some(requested) = params.host.as_deref() {
            let requested = requested.trim();
            let matches = requested.eq_ignore_ascii_case(&host_config.host)
                || requested.eq_ignore_ascii_case(&host_config.label())
                || host_config.user.as_deref().is_some_and(|user| {
                    requested.eq_ignore_ascii_case(&format!("{user}@{}", host_config.host))
                });
            if !matches {
                return Err(RemoteIntegrationFailure::new(
                    ToolErrorCode::REMOTE_INTEGRATION_HOST_NOT_ALLOWED,
                    format!(
                        "requested host {requested} does not match the configured host {}",
                        host_config.host
                    ),
                )
                .into());
            }
        }
        if policy.require_verifier && config.is_none() {
            return Err(RemoteIntegrationFailure::new(
                ToolErrorCode::REMOTE_INTEGRATION_VERIFIER_REQUIRED,
                "remote integration requires a configured Verifier provider",
            )
            .into());
        }
        let session_worktree = session
            .worktree
            .clone()
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("session has no worktree"))?;
        let session_branch = session
            .branch
            .clone()
            .ok_or_else(|| anyhow!("session has no branch"))?;
        let head_worktree = session_worktree.clone();
        let session_head =
            tokio::task::spawn_blocking(move || worktree::head_commit(&head_worktree)).await??;
        let event_spec =
            remote_integrator_spec(&session, &session_worktree, &session_branch, &session_head);
        emit(
            &self.engine,
            &session.id,
            &event_spec,
            "remote.integration.requested",
            json!({
                "project_id": params.project_id,
                "host": host_config.host,
                "remote_branch": branch,
                "expected_commit": params.expected_commit,
                "task_id": params.task_id,
                "max_bundle_bytes": policy.max_bundle_bytes,
                "require_verifier": policy.require_verifier,
            }),
        )
        .await?;
        emit(
            &self.engine,
            &session.id,
            &event_spec,
            "remote.integration.approved",
            json!({
                "host": host_config.host,
                "remote_branch": branch,
                "risk": RiskLevel::HostState.as_str(),
                "reason": "explicit operator confirmation for remote bundle import",
                "approval_mode": self
                    .config
                    .read()
                    .ok()
                    .and_then(|config| config.as_ref().map(|config| config.approval_mode))
                    .unwrap_or_default(),
            }),
        )
        .await?;

        let keep_import_refs = params.keep_import_refs.unwrap_or(policy.keep_import_refs);
        let (bundle_path, mut bundle) = if let Some(downloaded) = downloaded {
            downloaded
        } else {
            let workspace = self.engine.remote_workspace(&params.project_id).await?;
            if !workspace.supports_git_bundle().await? {
                return Err(RemoteIntegrationFailure::new(
                    ToolErrorCode::REMOTE_PROTOCOL_ERROR,
                    "the remote helper does not advertise the git.bundle capability",
                )
                .with_hint("upgrade vp-remote-agent on the remote host")
                .into());
            }
            let chunk_bytes = DEFAULT_BUNDLE_CHUNK_BYTES
                .min(policy.max_bundle_bytes.max(1024))
                .max(1024);
            let download = download_remote_bundle(
                &workspace,
                &session.id,
                &branch,
                policy.max_bundle_bytes,
                chunk_bytes,
                params.expected_commit.as_deref(),
                params.base_commit.as_deref(),
                &import_id,
                self.engine.store().data_dir(),
            )
            .await;
            match download {
                Ok(download) => download,
                Err(error) => {
                    let detail = truncate(&error.to_string());
                    emit(
                        &self.engine,
                        &session.id,
                        &event_spec,
                        "remote.integration.failed",
                        json!({
                            "stage": "bundle_download",
                            "code": error.code(),
                            "detail": detail,
                            "host": host_config.host,
                            "remote_branch": branch,
                        }),
                    )
                    .await?;
                    return Err(error.into());
                }
            }
        };
        bundle.host = host_config.host.clone();
        if bundle.bytes > policy.max_bundle_bytes {
            return Err(RemoteIntegrationFailure::new(
                ToolErrorCode::REMOTE_BUNDLE_TOO_LARGE,
                format!(
                    "bundle reports {} bytes; the configured cap is {}",
                    bundle.bytes, policy.max_bundle_bytes
                ),
            )
            .into());
        }
        if let Some(expected) = params.expected_commit.as_deref()
            && !expected.trim().is_empty()
            && !expected.trim().eq_ignore_ascii_case(&bundle.head)
        {
            return Err(RemoteIntegrationFailure::new(
                ToolErrorCode::REMOTE_BUNDLE_HEAD_MISMATCH,
                format!(
                    "bundle head {} does not match the expected commit {}",
                    bundle.head,
                    expected.trim()
                ),
            )
            .into());
        }
        match tokio::fs::metadata(&bundle_path).await {
            Ok(metadata) if metadata.len() == bundle.bytes => {}
            Ok(metadata) => {
                return Err(RemoteIntegrationFailure::new(
                    ToolErrorCode::REMOTE_BUNDLE_TAMPERED,
                    format!(
                        "local bundle is {} bytes; the helper reported {}",
                        metadata.len(),
                        bundle.bytes
                    ),
                )
                .into());
            }
            Err(error) => {
                return Err(RemoteIntegrationFailure::new(
                    ToolErrorCode::REMOTE_BUNDLE_IMPORT_FAILED,
                    format!(
                        "cannot inspect local bundle {}: {error}",
                        bundle_path.display()
                    ),
                )
                .into());
            }
        }
        let import_root = PathBuf::from(&project.root);
        let import_bundle_path = bundle_path.clone();
        let expected_head = bundle.head.clone();
        let expected_sha = bundle.sha256.clone();
        let session_id_for_import = session.id.clone();
        let import_id_for_ref = import_id.clone();
        let keep_for_import = keep_import_refs;
        let imported = tokio::task::spawn_blocking(move || {
            remote_integration::import_bundle(
                &import_root,
                &import_bundle_path,
                &expected_head,
                Some(&expected_sha),
                &session_id_for_import,
                &import_id_for_ref,
                keep_for_import,
            )
        })
        .await?;
        let _ = tokio::fs::remove_file(&bundle_path).await;
        let imported = match imported {
            Ok(imported) => imported,
            Err(error) => {
                let detail = truncate(&error.to_string());
                emit(
                    &self.engine,
                    &session.id,
                    &event_spec,
                    "remote.integration.failed",
                    json!({
                        "stage": "bundle_import",
                        "code": error.code(),
                        "detail": detail,
                        "bundle_sha256": bundle.sha256,
                    }),
                )
                .await?;
                return Err(error.into());
            }
        };
        emit(
            &self.engine,
            &session.id,
            &event_spec,
            "remote.bundle.imported",
            json!({
                "host": bundle.host,
                "remote_branch": bundle.branch,
                "head": bundle.head,
                "bundle_sha256": bundle.sha256,
                "bytes": bundle.bytes,
                "imported_ref": imported.imported_ref,
            }),
        )
        .await?;

        let data_dir = self.engine.store().data_dir().to_path_buf();
        let project_root = PathBuf::from(&project.root);
        let project_id = project.id.clone();
        let session_id = session.id.clone();
        let import_id_for_worktree = import_id.clone();
        let imported_ref = imported.imported_ref.clone();
        let worktree_result = tokio::task::spawn_blocking({
            let project_root = project_root.clone();
            move || {
                remote_integration::create_remote_worktree(
                    &project_root,
                    &data_dir,
                    &project_id,
                    &session_id,
                    &import_id_for_worktree,
                    &imported_ref,
                )
            }
        })
        .await?;
        let remote_worktree = match worktree_result {
            Ok(worktree) => worktree,
            Err(error) => {
                if !keep_import_refs {
                    let _ = tokio::task::spawn_blocking({
                        let project_root = project_root.clone();
                        let imported_ref = imported.imported_ref.clone();
                        move || {
                            remote_integration::prune_import_refs(
                                &project_root,
                                &[imported_ref],
                                false,
                            )
                        }
                    })
                    .await?;
                }
                return Err(error.into());
            }
        };
        let agent_name = truncate_to(&format!("Remote {}", bundle.branch), 60);
        let agent = self
            .engine
            .create_agent(
                &session.id,
                AgentRole::Worker.as_str(),
                &agent_name,
                Some("Remote integration".to_string()),
                None,
                &config
                    .as_ref()
                    .map(|config| model_for(config, AgentRole::Worker))
                    .unwrap_or_else(|| "remote".to_string()),
            )
            .await?;
        self.engine
            .set_agent_worktree(
                &agent.id,
                &remote_worktree.path.to_string_lossy(),
                &remote_worktree.branch,
            )
            .await?;
        self.engine
            .set_agent_profile(&agent.id, AgentProfile::Coding.as_str())
            .await?;
        self.engine
            .set_agent_task(&agent.id, params.task_id.clone(), None)
            .await?;

        let profile = AgentProfile::Coding;
        let manager_spec = AgentSpec {
            id: "remote-integrator".to_string(),
            role: AgentRole::Manager,
            name: "Remote Integrator".to_string(),
            depth: 1,
            parent_name: None,
            title: Some("Integration".to_string()),
            task_id: None,
            department: None,
            identity_id: None,
            identity_generation: None,
            profile: AgentProfile::Generic,
            allow_delegation: false,
            task_kind: None,
            auto_approve: true,
            worktree: PathBuf::from(&session.worktree.clone().unwrap_or_default()),
            branch: session_branch.clone(),
            session_worktree: PathBuf::from(&session.worktree.clone().unwrap_or_default()),
            session_branch: session_branch.clone(),
            write_scope: Vec::new(),
            base_commit: session_head.clone(),

            workspace_id: None,
            session_workspace_id: None,
            parent_workspace_id: None,
        };
        let worker_spec = AgentSpec {
            id: agent.id.clone(),
            role: AgentRole::Worker,
            name: agent_name.clone(),
            depth: 2,
            parent_name: Some("Remote Integrator".to_string()),
            title: agent.title.clone(),
            task_id: params.task_id.clone(),
            department: None,
            identity_id: None,
            identity_generation: None,
            profile,
            allow_delegation: false,
            task_kind: None,
            auto_approve: true,
            worktree: remote_worktree.path.clone(),
            branch: remote_worktree.branch.clone(),
            session_worktree: PathBuf::from(&session.worktree.clone().unwrap_or_default()),
            session_branch: session_branch.clone(),
            write_scope: Vec::new(),
            base_commit: imported.commit.clone(),

            workspace_id: None,
            session_workspace_id: None,
            parent_workspace_id: None,
        };
        let provenance = VerifierProvenance {
            source: Some(MERGE_SOURCE_REMOTE.to_string()),
            remote_host: Some(bundle.host.clone()),
            remote_branch: Some(bundle.branch.clone()),
            bundle_sha256: Some(bundle.sha256.clone()),
            environment_caveat: Some(remote_integration::environment_caveat().to_string()),
        };
        let plan = self.engine.get_team_plan(&session.id).await.ok().flatten();
        let task = params.task_id.as_deref().and_then(|task_id| {
            plan.as_ref()
                .and_then(|plan| plan.tasks.iter().find(|task| task.id == task_id))
        });
        let objective = params
            .objective
            .clone()
            .or_else(|| task.map(|task| task.title.clone()))
            .unwrap_or_else(|| {
                format!(
                    "Integrate remote branch {} from {}",
                    bundle.branch, bundle.host
                )
            });
        let acceptance = task.map(|task| task.acceptance.join("; "));
        let mut verification = RemoteIntegrationVerification {
            required: policy.require_verifier,
            performed: false,
            approved: None,
            summary: None,
            environment_caveat: remote_integration::environment_caveat().to_string(),
        };
        if policy.require_verifier {
            let config = config
                .as_ref()
                .ok_or_else(|| anyhow!("agent provider is not configured"))?;
            let control = Arc::new(RunControl::new_operation());
            let budget = SessionBudget::new(
                session.token_budget,
                session.tokens_in + session.tokens_out,
                AgentCaps {
                    managers: config.max_managers,
                    workers: config.max_workers,
                    labour: config.max_labour,
                },
            )
            .with_cost(session.cost_budget_usd, session.cost_spent_usd)
            .with_time(session.time_budget_secs)
            .with_unlimited_flag(self.engine.unlimited_flag(&session.id, session.unlimited));
            let outcome = verify_worker(
                &self.engine,
                config,
                &session,
                &project,
                &control,
                &budget,
                &manager_spec,
                &worker_spec,
                &imported.commit,
                &objective,
                &format!(
                    "Imported {} bytes from {}:{} with bundle SHA-256 {}.",
                    bundle.bytes, bundle.host, bundle.branch, bundle.sha256
                ),
                acceptance.as_deref(),
                true,
                &provenance,
            )
            .await?;
            verification.performed = true;
            match outcome {
                VerificationOutcome::Approved(verdict) => {
                    verification.approved = Some(true);
                    verification.summary = Some(verdict.summary);
                    self.engine.update_agent_status(&agent.id, "idle").await?;
                }
                VerificationOutcome::Rejected(verdict) => {
                    verification.approved = Some(false);
                    verification.summary = Some(verdict.summary.clone());
                    self.engine.update_agent_status(&agent.id, "failed").await?;
                    self.cleanup_remote_import(
                        &project_root,
                        &remote_worktree.path,
                        &remote_worktree.branch,
                        std::slice::from_ref(&imported.imported_ref),
                        keep_import_refs,
                    )
                    .await;
                    emit(
                        &self.engine,
                        &session.id,
                        &event_spec,
                        "remote.integration.rejected",
                        json!({
                            "host": bundle.host,
                            "remote_branch": bundle.branch,
                            "bundle_sha256": bundle.sha256,
                            "summary": verdict.summary,
                            "environment_caveat": verification.environment_caveat,
                        }),
                    )
                    .await?;
                    return Err(RemoteIntegrationFailure::new(
                        ToolErrorCode::REMOTE_INTEGRATION_VERIFIER_REJECTED,
                        format!(
                            "the Verifier rejected the imported commit: {}",
                            verdict.summary
                        ),
                    )
                    .into());
                }
                VerificationOutcome::Cancelled => {
                    self.cleanup_remote_import(
                        &project_root,
                        &remote_worktree.path,
                        &remote_worktree.branch,
                        std::slice::from_ref(&imported.imported_ref),
                        keep_import_refs,
                    )
                    .await;
                    return Err(RemoteIntegrationFailure::new(
                        ToolErrorCode::REMOTE_INTEGRATION_FAILED,
                        "remote verification was cancelled before enqueueing",
                    )
                    .into());
                }
                VerificationOutcome::BudgetExceeded(stop) => {
                    self.cleanup_remote_import(
                        &project_root,
                        &remote_worktree.path,
                        &remote_worktree.branch,
                        std::slice::from_ref(&imported.imported_ref),
                        keep_import_refs,
                    )
                    .await;
                    return Err(RemoteIntegrationFailure::new(
                        ToolErrorCode::REMOTE_INTEGRATION_FAILED,
                        format!("remote verification stopped: {}", stop.summary()),
                    )
                    .into());
                }
                VerificationOutcome::Error(error) => {
                    self.cleanup_remote_import(
                        &project_root,
                        &remote_worktree.path,
                        &remote_worktree.branch,
                        std::slice::from_ref(&imported.imported_ref),
                        keep_import_refs,
                    )
                    .await;
                    return Err(RemoteIntegrationFailure::new(
                        ToolErrorCode::REMOTE_INTEGRATION_FAILED,
                        format!("remote verification failed: {error}"),
                    )
                    .into());
                }
            }
        } else {
            emit(
                &self.engine,
                &session.id,
                &worker_spec,
                "remote.integration.verification_skipped",
                json!({
                    "host": bundle.host,
                    "remote_branch": bundle.branch,
                    "bundle_sha256": bundle.sha256,
                    "reason": "RemoteIntegrationPolicy.require_verifier is false",
                    "environment_caveat": remote_integration::environment_caveat(),
                }),
            )
            .await?;
        }
        let base_worktree = PathBuf::from(&session.worktree.clone().unwrap_or_default());
        let base_commit =
            tokio::task::spawn_blocking(move || worktree::head_commit(&base_worktree)).await??;
        let verification_detail = verification
            .summary
            .as_deref()
            .map(|summary| format!("verifier: {summary}"))
            .unwrap_or_else(|| "verifier skipped by policy".to_string());
        let detail = truncate(&format!(
            "remote {}:{} @ {}; bundle sha256 {}; {verification_detail}; {}",
            bundle.host,
            bundle.branch,
            imported.commit,
            bundle.sha256,
            verification.environment_caveat
        ));
        let source = MergeQueueSource::remote(
            bundle.host.clone(),
            bundle.branch.clone(),
            bundle.sha256.clone(),
            imported.imported_ref.clone(),
        );
        let remote_target = crate::effect::EffectTarget::new(
            crate::effect::kinds::REMOTE_INTEGRATION,
            format!("{}:{}", bundle.host, bundle.branch),
        );
        let remote_request = crate::effect::canonical_request_hash(
            "remote.integrate",
            json!({
                "host": bundle.host,
                "branch": bundle.branch,
                "commit": imported.commit,
            }),
        );
        let remote_request = match remote_request {
            Ok(value) => value,
            Err(error) => return Err(error),
        };
        let existing = self
            .engine
            .lookup_effect(
                &session.id,
                params.task_id.as_deref(),
                "remote.integrate",
                &remote_target.canonical(),
                &remote_request,
            )
            .await?;
        if let Some(receipt) = existing.as_ref() {
            match receipt.state {
                EffectState::Done => {
                    return Err(anyhow!(
                        "EFFECT_REPLAYED: remote {}:{} @ {} was already integrated; inspect the stored receipt {} and the existing queue entry instead of integrating again",
                        bundle.host,
                        bundle.branch,
                        imported.commit,
                        receipt.id
                    ));
                }
                EffectState::Intent | EffectState::Unknown => {
                    return Err(anyhow::Error::new(crate::effect::EffectBlocked {
                        session_id: session.id.clone(),
                        task_id: params.task_id.clone(),
                        receipt_ids: vec![receipt.id.clone()],
                        message: format!(
                            "EFFECT_UNKNOWN: a previous remote integration of {}:{} is {}; reconcile it before retrying",
                            bundle.host,
                            bundle.branch,
                            receipt.state.as_str()
                        ),
                    }));
                }
                EffectState::Failed => {}
            }
        }
        let entry = self
            .engine
            .enqueue_merge_with_source(
                &session.id,
                &agent.id,
                &agent_name,
                params.task_id.clone(),
                &remote_worktree.branch,
                &imported.commit,
                &base_commit,
                Some(detail),
                source,
            )
            .await?;
        let entry = self
            .engine
            .set_merge_queue_verification(
                &entry.id,
                verification.approved,
                verification.summary.clone(),
                Some(verification.environment_caveat.clone()),
            )
            .await?
            .unwrap_or(entry);
        emit(
            &self.engine,
            &session.id,
            &worker_spec,
            "remote.integration.enqueued",
            json!({
                "entry_id": entry.id,
                "host": bundle.host,
                "remote_branch": bundle.branch,
                "bundle_sha256": bundle.sha256,
                "imported_ref": imported.imported_ref,
                "commit": imported.commit,
                "attempt": entry.attempt,
            }),
        )
        .await?;
        // The remote effect is the verified bundle import/enqueue: the remote
        // commit is now a durable queue identity. The local deterministic
        // merge is tracked by the merge queue and the `git_merge` receipt, so
        // this receipt settles done before integration instead of blocking
        // its own queue entry.
        let mut accepted = crate::effect::runtime_receipt(
            &session.id,
            params.task_id.as_deref(),
            "remote.integrate",
            remote_target,
            remote_request,
            Some(bundle.host.clone()),
        );
        accepted.state = EffectState::Done;
        accepted.external_id = Some(imported.commit.clone());
        accepted.response_hash = Some(crate::sha256_hex(imported.commit.as_bytes()));
        accepted.evidence = vp_protocol::types::EffectEvidence {
            probe: "remote_bundle_import".to_string(),
            outcome: "observed".to_string(),
            commit: Some(imported.commit.clone()),
            branch: Some(bundle.branch.clone()),
            repository: Some(bundle.host.clone()),
            note: Some(format!(
                "accepted as merge queue entry {}; local integration is tracked separately",
                entry.id
            )),
            caveat: Some(remote_integration::environment_caveat().to_string()),
            ..Default::default()
        };
        self.engine.record_settled_effect(accepted).await?;
        let integration_spec = AgentSpec {
            auto_approve: false,
            ..worker_spec
        };
        let integration = process_merge_entry(
            &self.engine,
            &session,
            &integration_spec,
            &entry,
            config
                .as_ref()
                .and_then(|config| config.integration_command.as_deref()),
        )
        .await;
        let entry = self
            .engine
            .get_merge_queue_entry(&entry.id)
            .await?
            .ok_or_else(|| anyhow!("merge queue entry disappeared after remote integration"))?;
        match integration {
            Ok(_) => {
                let (pruned, cleanup_error) = match tokio::task::spawn_blocking({
                    let project_root = project_root.clone();
                    let imported_ref = imported.imported_ref.clone();
                    move || {
                        remote_integration::prune_import_refs(
                            &project_root,
                            &[imported_ref],
                            keep_import_refs,
                        )
                    }
                })
                .await?
                {
                    Ok(pruned) => (pruned, None),
                    Err(error) => {
                        let detail = error.to_string();
                        emit(
                            &self.engine,
                            &session.id,
                            &integration_spec,
                            "remote.integration.cleanup_failed",
                            json!({"entry_id": entry.id, "detail": detail}),
                        )
                        .await?;
                        (0, Some(detail))
                    }
                };
                emit(
                    &self.engine,
                    &session.id,
                    &integration_spec,
                    "remote.integration.finished",
                    json!({
                        "entry_id": entry.id,
                        "status": "merged",
                        "host": bundle.host,
                        "remote_branch": bundle.branch,
                        "bundle_sha256": bundle.sha256,
                        "imported_ref": entry.imported_ref,
                        "refs_pruned": pruned,
                        "environment_caveat": remote_integration::environment_caveat(),
                    }),
                )
                .await?;
                Ok(RemoteIntegrationResult {
                    project_id: params.project_id,
                    session_id: session.id,
                    entry,
                    bundle,
                    verification,
                    imported_ref: if keep_import_refs || cleanup_error.is_some() {
                        Some(imported.imported_ref.clone())
                    } else {
                        None
                    },
                    refs_pruned: pruned > 0,
                    cleanup_error,
                    environment_caveat: remote_integration::environment_caveat().to_string(),
                })
            }
            Err(error) => {
                let detail = format!("{error:#}");
                emit(
                    &self.engine,
                    &session.id,
                    &integration_spec,
                    "remote.integration.failed",
                    json!({
                        "entry_id": entry.id,
                        "status": entry.status,
                        "host": bundle.host,
                        "remote_branch": bundle.branch,
                        "bundle_sha256": bundle.sha256,
                        "imported_ref": entry.imported_ref,
                        "detail": truncate(&detail),
                        "environment_caveat": remote_integration::environment_caveat(),
                        "retry_hint": "retry the merge queue entry; the imported ref is retained while the entry is retryable",
                    }),
                )
                .await?;
                Err(RemoteIntegrationFailure::new(
                    ToolErrorCode::REMOTE_INTEGRATION_FAILED,
                    format!(
                        "remote bundle was verified and enqueued as {}, but integration did not merge: {detail}",
                        entry.id
                    ),
                )
                .with_evidence(format!(
                    "entry={}; status={}; imported_ref={}",
                    entry.id,
                    entry.status,
                    entry.imported_ref.as_deref().unwrap_or("")
                ))
                .into())
            }
        }
    }

    /// Prune the imported ref after a successful retry/resolution unless the
    /// project policy retains refs. A failure is returned as a warning string;
    /// it never flips an already-merged entry back to failed.
    pub(in crate::agent) async fn prune_remote_refs_after_merge(
        &self,
        project: &Project,
        entry: &MergeQueueEntry,
    ) -> Option<String> {
        if entry.source != MERGE_SOURCE_REMOTE {
            return None;
        }
        let reference = entry.imported_ref.clone()?;
        let policy = self.engine.get_remote_integration_policy(&project.id).await;
        let keep = match policy {
            Ok(policy) => policy.keep_import_refs,
            Err(error) => {
                return Some(format!("could not read remote integration policy: {error}"));
            }
        };
        if keep {
            return None;
        }
        let root = PathBuf::from(&project.root);
        match tokio::task::spawn_blocking(move || {
            remote_integration::prune_import_refs(&root, &[reference], false)
        })
        .await
        {
            Ok(Ok(_)) => None,
            Ok(Err(error)) => Some(error.to_string()),
            Err(error) => Some(format!("ref cleanup task failed: {error}")),
        }
    }

    pub(in crate::agent) async fn cleanup_remote_import(
        &self,
        project_root: &Path,
        worktree_path: &Path,
        branch: &str,
        references: &[String],
        keep: bool,
    ) {
        let root = project_root.to_path_buf();
        let worktree_path = worktree_path.to_path_buf();
        let branch = branch.to_string();
        let references = references.to_vec();
        let _ = tokio::task::spawn_blocking(move || {
            let _ = remote_integration::remove_remote_worktree(&root, &worktree_path, &branch);
            let _ = remote_integration::prune_import_refs(&root, &references, keep);
        })
        .await;
    }

    pub async fn retry_merge(&self, entry_id: &str) -> Result<MergeQueueEntry> {
        let entry = self
            .engine
            .get_merge_queue_entry(entry_id)
            .await?
            .ok_or_else(|| anyhow!("merge queue entry not found: {entry_id}"))?;
        let (session, _) =
            load_session_project(self.engine.store().clone(), &entry.session_id).await?;
        crate::ensure_session_quiescent(&session, "retry a merge")?;
        if session.status == "landed" {
            anyhow::bail!("session is already landed");
        }
        self.begin_operation(&entry.session_id)?;
        let result = self.retry_merge_inner(entry_id).await;
        self.end_operation(&entry.session_id);
        result
    }

    pub(in crate::agent) async fn retry_merge_inner(
        &self,
        entry_id: &str,
    ) -> Result<MergeQueueEntry> {
        let entry = self
            .engine
            .get_merge_queue_entry(entry_id)
            .await?
            .ok_or_else(|| anyhow!("merge queue entry not found: {entry_id}"))?;
        if !matches!(entry.status.as_str(), "conflict" | "failed" | "interrupted") {
            return Err(anyhow!(
                "merge queue entry is {}, only conflict/failed/interrupted can be retried",
                entry.status
            ));
        }
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned())
            .ok_or_else(|| anyhow!("agent provider is not configured"))?;
        let (session, project) =
            load_session_project(self.engine.store().clone(), &entry.session_id).await?;
        let agents = self.engine.list_agents(&entry.session_id).await?;
        let agent = agents
            .iter()
            .find(|agent| agent.id == entry.agent_id)
            .ok_or_else(|| anyhow!("worker agent not found: {}", entry.agent_id))?;
        let agent_worktree = agent
            .worktree
            .clone()
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("worker has no worktree: {}", entry.agent_id))?;
        let session_worktree = session
            .worktree
            .clone()
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("session has no worktree"))?;
        let session_branch = session
            .branch
            .clone()
            .ok_or_else(|| anyhow!("session has no branch"))?;

        let rebase_worktree = agent_worktree.clone();
        let rebase_onto = session_branch.clone();
        let rebase = tokio::task::spawn_blocking(move || {
            worktree::rebase_onto(&rebase_worktree, &rebase_onto)
        })
        .await?;
        if let Err(error) = rebase {
            let detail = truncate(&format!("rebase failed: {error:#}"));
            self.engine
                .update_merge_queue_entry(&entry.id, "conflict", Some(detail.clone()))
                .await?;
            return Err(anyhow!(detail));
        }

        let commit_worktree = agent_worktree.clone();
        let commit =
            tokio::task::spawn_blocking(move || worktree::head_commit(&commit_worktree)).await??;
        let base_worktree = session_worktree.clone();
        let base_commit =
            tokio::task::spawn_blocking(move || worktree::head_commit(&base_worktree)).await??;
        if entry.source == MERGE_SOURCE_REMOTE
            && let Some(reference) = entry.imported_ref.clone()
        {
            let project_root = PathBuf::from(&project.root);
            let commit_for_ref = commit.clone();
            tokio::task::spawn_blocking(move || {
                crate::remote_integration::update_import_ref(
                    &project_root,
                    &reference,
                    &commit_for_ref,
                )
            })
            .await??;
        }

        let profile = agent
            .profile
            .as_deref()
            .and_then(AgentProfile::parse)
            .unwrap_or(AgentProfile::Coding);
        let plan = self.engine.get_team_plan(&session.id).await.ok().flatten();
        let task = entry.task_id.as_deref().and_then(|task_id| {
            plan.as_ref()
                .and_then(|plan| plan.tasks.iter().find(|task| task.id == task_id))
        });
        let objective = task
            .map(|task| task.title.clone())
            .unwrap_or_else(|| format!("Retry integration for queue entry {}", entry.id));
        let acceptance = task.map(|task| task.acceptance.join("; "));
        let control = Arc::new(RunControl::new_operation());
        let budget = SessionBudget::new(
            session.token_budget,
            session.tokens_in + session.tokens_out,
            AgentCaps {
                managers: config.max_managers,
                workers: config.max_workers,
                labour: config.max_labour,
            },
        )
        .with_cost(session.cost_budget_usd, session.cost_spent_usd)
        .with_time(session.time_budget_secs)
        .with_unlimited_flag(self.engine.unlimited_flag(&session.id, session.unlimited))
        .with_scaling_slots(self.engine.scaling_slots(
            &session.id,
            &session.project_id,
            config.max_workers,
            config.max_labour,
        ));
        let manager_spec = AgentSpec {
            id: agent
                .parent_id
                .clone()
                .unwrap_or_else(|| "integrator".to_string()),
            role: AgentRole::Manager,
            name: "Integrator".to_string(),
            depth: 1,
            parent_name: None,
            title: Some("Integration".to_string()),
            task_id: None,
            department: None,
            identity_id: None,
            identity_generation: None,
            profile: AgentProfile::Generic,
            allow_delegation: false,
            task_kind: None,
            auto_approve: true,
            worktree: session_worktree.clone(),
            branch: session_branch.clone(),
            session_worktree: session_worktree.clone(),
            session_branch: session_branch.clone(),
            write_scope: Vec::new(),
            base_commit: base_commit.clone(),

            workspace_id: None,
            session_workspace_id: None,
            parent_workspace_id: None,
        };
        let worker_spec = AgentSpec {
            id: agent.id.clone(),
            role: AgentRole::Worker,
            name: agent.name.clone(),
            depth: 2,
            parent_name: Some("Integrator".to_string()),
            title: agent.title.clone(),
            task_id: entry.task_id.clone(),
            department: None,
            identity_id: None,
            identity_generation: None,
            profile,
            allow_delegation: false,
            task_kind: None,
            auto_approve: true,
            worktree: agent_worktree.clone(),
            branch: entry.branch.clone(),
            session_worktree: session_worktree.clone(),
            session_branch: session_branch.clone(),
            write_scope: Vec::new(),
            base_commit: base_commit.clone(),

            workspace_id: None,
            session_workspace_id: None,
            parent_workspace_id: None,
        };
        let verification = verify_worker(
            &self.engine,
            &config,
            &session,
            &project,
            &control,
            &budget,
            &manager_spec,
            &worker_spec,
            &commit,
            &objective,
            "Rebased onto the current session branch after a previous integration attempt.",
            acceptance.as_deref(),
            true,
            &verifier_provenance_for_entry(&entry),
        )
        .await?;
        let verifier_summary = match verification {
            VerificationOutcome::Approved(verdict) => verdict.summary,
            VerificationOutcome::Rejected(verdict) => {
                let detail = truncate(&format!("retry rejected by Verifier: {}", verdict.summary));
                self.engine
                    .update_merge_queue_entry(&entry.id, "failed", Some(detail.clone()))
                    .await?;
                return Err(anyhow!(detail));
            }
            VerificationOutcome::Cancelled => {
                let detail = "retry verification cancelled".to_string();
                self.engine
                    .update_merge_queue_entry(&entry.id, "failed", Some(detail.clone()))
                    .await?;
                return Err(anyhow!(detail));
            }
            VerificationOutcome::BudgetExceeded(stop) => {
                let detail = truncate(&format!("retry verification stopped: {}", stop.summary()));
                self.engine
                    .update_merge_queue_entry(&entry.id, "failed", Some(detail.clone()))
                    .await?;
                return Err(anyhow!(detail));
            }
            VerificationOutcome::Error(error) => {
                let detail = truncate(&format!("retry verification failed: {error}"));
                self.engine
                    .update_merge_queue_entry(&entry.id, "failed", Some(detail.clone()))
                    .await?;
                return Err(anyhow!(detail));
            }
        };
        let Some(entry) = self
            .engine
            .retry_merge_queue_entry(
                &entry.id,
                &commit,
                &base_commit,
                Some(format!(
                    "rebased onto the current session branch; verified: {verifier_summary}"
                )),
            )
            .await?
        else {
            return Err(anyhow!("merge queue entry can no longer be retried"));
        };

        let spec = AgentSpec {
            id: agent.id.clone(),
            role: AgentRole::Worker,
            name: agent.name.clone(),
            depth: 2,
            parent_name: None,
            title: agent.title.clone(),
            task_id: entry.task_id.clone(),
            department: None,
            identity_id: None,
            identity_generation: None,
            profile,
            allow_delegation: true,
            task_kind: None,
            auto_approve: false,
            worktree: agent_worktree,
            branch: entry.branch.clone(),
            session_worktree,
            session_branch,
            write_scope: Vec::new(),
            base_commit: entry.base_commit.clone(),

            workspace_id: None,
            session_workspace_id: None,
            parent_workspace_id: None,
        };
        process_merge_entry(
            &self.engine,
            &session,
            &spec,
            &entry,
            config.integration_command.as_deref(),
        )
        .await?;
        let merged = self
            .engine
            .get_merge_queue_entry(&entry.id)
            .await?
            .ok_or_else(|| anyhow!("merge queue entry disappeared after retry"))?;
        if merged.status == "merged"
            && let Some(warning) = self.prune_remote_refs_after_merge(&project, &merged).await
        {
            emit(
                &self.engine,
                &session.id,
                &spec,
                "remote.integration.cleanup_failed",
                json!({"entry_id": merged.id, "detail": warning}),
            )
            .await?;
        }
        Ok(merged)
    }

    /// Resolve a merge conflict with an internal coding agent, re-verify the
    /// rebased branch, then integrate it. This is an explicit user action: it
    /// skips routine prompts for the resolver and verifier because the user
    /// already chose auto-resolution, but host-level operations are still
    /// denied and the shell sandbox still applies.
    pub async fn resolve_merge(&self, entry_id: &str) -> Result<MergeQueueEntry> {
        self.resolve_merge_with_limit(entry_id, None).await
    }

    pub(in crate::agent) async fn resolve_merge_with_limit(
        &self,
        entry_id: &str,
        max_resolve_tokens: Option<u64>,
    ) -> Result<MergeQueueEntry> {
        let entry = self
            .engine
            .get_merge_queue_entry(entry_id)
            .await?
            .ok_or_else(|| anyhow!("merge queue entry not found: {entry_id}"))?;
        if entry.status != "conflict" {
            return Err(anyhow!(
                "merge queue entry is {}, only conflict entries can be auto-resolved",
                entry.status
            ));
        }
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned())
            .ok_or_else(|| anyhow!("agent provider is not configured"))?;
        let (session, project) =
            load_session_project(self.engine.store().clone(), &entry.session_id).await?;
        if session.status == "imported" {
            anyhow::bail!("imported sessions are read-only archives");
        }
        let control = Arc::new(RunControl::new());
        {
            let mut runs = self
                .runs
                .lock()
                .map_err(|_| anyhow!("agent run mutex poisoned"))?;
            if runs.contains_key(&session.id) {
                return Err(anyhow!(
                    "session is running; stop or wait before auto-resolving a merge"
                ));
            }
            runs.insert(session.id.clone(), control.clone());
        }
        self.engine
            .update_session_status(&session.id, "queued")
            .await?;
        let permit = tokio::select! {
            permit = self
                .global_slots
                .acquire(&session.id, &session.project_id, session.priority, &control) => permit,
            _ = wait_for_cancel(&control) => None,
        };
        let Some(_permit) = permit else {
            if let Ok(mut runs) = self.runs.lock() {
                runs.remove(&session.id);
            }
            let _ = self.engine.update_session_status(&session.id, "idle").await;
            return Err(anyhow!(
                "auto-resolution cancelled while waiting for a global slot"
            ));
        };
        if control.is_cancelled() {
            if let Ok(mut runs) = self.runs.lock() {
                runs.remove(&session.id);
            }
            let _ = self.engine.update_session_status(&session.id, "idle").await;
            return Err(anyhow!("auto-resolution cancelled"));
        }
        self.engine
            .update_session_status(&session.id, "running")
            .await?;
        let result = self
            .resolve_merge_inner(
                &config,
                &session,
                &project,
                &entry,
                &control,
                max_resolve_tokens,
            )
            .await;
        if let Ok(mut runs) = self.runs.lock() {
            runs.remove(&session.id);
        }
        let _ = self.engine.update_session_status(&session.id, "idle").await;
        result
    }

    pub(in crate::agent) async fn resolve_merge_inner(
        &self,
        config: &AgentConfig,
        session: &Session,
        project: &Project,
        entry: &MergeQueueEntry,
        control: &RunControl,
        max_resolve_tokens: Option<u64>,
    ) -> Result<MergeQueueEntry> {
        let agents = self.engine.list_agents(&session.id).await?;
        let worker = agents
            .iter()
            .find(|agent| agent.id == entry.agent_id)
            .ok_or_else(|| anyhow!("worker agent not found: {}", entry.agent_id))?;
        let worker_worktree = worker
            .worktree
            .clone()
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("worker has no worktree: {}", entry.agent_id))?;
        let session_worktree = session
            .worktree
            .clone()
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("session has no worktree"))?;
        let session_branch = session
            .branch
            .clone()
            .ok_or_else(|| anyhow!("session has no branch"))?;

        let clean_worktree = session_worktree.clone();
        let clean = tokio::task::spawn_blocking(move || {
            worktree::git_output(&clean_worktree, &["status", "--porcelain"])
        })
        .await??;
        if !clean.trim().is_empty() {
            anyhow::bail!("session worktree has uncommitted changes; commit or discard them first");
        }
        let clean_worker = worker_worktree.clone();
        let clean = tokio::task::spawn_blocking(move || {
            worktree::git_output(&clean_worker, &["status", "--porcelain"])
        })
        .await??;
        if !clean.trim().is_empty() {
            anyhow::bail!("worker worktree has uncommitted changes; resolve them first");
        }
        let base_worktree = session_worktree.clone();
        let base_commit =
            tokio::task::spawn_blocking(move || worktree::head_commit(&base_worktree)).await??;

        let rebase_worktree = worker_worktree.clone();
        let rebase_onto = session_branch.clone();
        let rebase = tokio::task::spawn_blocking(move || {
            worktree::rebase_onto_keep_conflicts(&rebase_worktree, &rebase_onto)
        })
        .await?;
        if let Err(error) = rebase {
            let paths_worktree = worker_worktree.clone();
            let paths =
                tokio::task::spawn_blocking(move || worktree::conflicted_paths(&paths_worktree))
                    .await??;
            let in_progress_worktree = worker_worktree.clone();
            let in_progress = tokio::task::spawn_blocking(move || {
                worktree::rebase_in_progress(&in_progress_worktree)
            })
            .await?;
            if paths.is_empty() || !in_progress {
                let detail = truncate(&format!("rebase failed: {error:#}"));
                self.engine
                    .update_merge_queue_entry(&entry.id, "conflict", Some(detail.clone()))
                    .await?;
                return Err(anyhow!(detail));
            }
            let detail = truncate(&format!(
                "rebase conflict in {} path(s): {}",
                paths.len(),
                paths.join(", ")
            ));
            self.engine
                .update_merge_queue_entry(&entry.id, "conflict", Some(detail.clone()))
                .await?;
        }

        let paths_worktree = worker_worktree.clone();
        let conflict_paths =
            tokio::task::spawn_blocking(move || worktree::conflicted_paths(&paths_worktree))
                .await??;
        let resolver = self
            .engine
            .create_agent(
                &session.id,
                AgentRole::Worker.as_str(),
                "Conflict Resolver",
                Some("Integration".to_string()),
                worker.parent_id.clone(),
                &model_for(config, AgentRole::Worker),
            )
            .await?;
        self.engine
            .set_agent_task(
                &resolver.id,
                worker.task_id.clone(),
                worker.department.clone(),
            )
            .await?;
        self.engine
            .set_agent_worktree(
                &resolver.id,
                &worker_worktree.to_string_lossy(),
                &entry.branch,
            )
            .await?;
        self.engine
            .set_agent_profile(&resolver.id, AgentProfile::Coding.as_str())
            .await?;
        let resolver_spec = AgentSpec {
            id: resolver.id.clone(),
            role: AgentRole::Worker,
            name: resolver.name.clone(),
            depth: 2,
            parent_name: worker.parent_id.clone(),
            title: Some("Integration".to_string()),
            task_id: entry.task_id.clone(),
            department: None,
            identity_id: None,
            identity_generation: None,
            profile: AgentProfile::Coding,
            allow_delegation: false,
            task_kind: None,
            auto_approve: true,
            worktree: worker_worktree.clone(),
            branch: entry.branch.clone(),
            session_worktree: session_worktree.clone(),
            session_branch: session_branch.clone(),
            write_scope: Vec::new(),
            base_commit: base_commit.clone(),

            workspace_id: None,
            session_workspace_id: None,
            parent_workspace_id: None,
        };
        emit(
            &self.engine,
            &session.id,
            &resolver_spec,
            "agent.spawned",
            serde_json::json!({
                "parent_id": resolver.parent_id,
                "parent_name": worker.parent_id,
                "objective": "Resolve a merge conflict",
                "conflict_paths": conflict_paths,
            }),
        )
        .await?;
        emit(
            &self.engine,
            &session.id,
            &resolver_spec,
            "integrator.conflict_resolution_started",
            serde_json::json!({
                "entry_id": entry.id,
                "branch": entry.branch,
                "base_commit": base_commit,
                "conflict_paths": conflict_paths,
            }),
        )
        .await?;

        let spent = session.tokens_in + session.tokens_out;
        let limit = match (session.token_budget, max_resolve_tokens) {
            (Some(session_limit), Some(cap)) => Some(session_limit.min(spent.saturating_add(cap))),
            (None, Some(cap)) => Some(spent.saturating_add(cap)),
            (Some(session_limit), None) => Some(session_limit),
            (None, None) => None,
        };
        let budget = SessionBudget::new(
            limit,
            spent,
            AgentCaps {
                managers: config.max_managers,
                workers: config.max_workers,
                labour: config.max_labour,
            },
        )
        .with_cost(session.cost_budget_usd, session.cost_spent_usd)
        .with_time(session.time_budget_secs)
        .with_unlimited_flag(self.engine.unlimited_flag(&session.id, session.unlimited))
        .with_scaling_slots(self.engine.scaling_slots(
            &session.id,
            &session.project_id,
            config.max_workers,
            config.max_labour,
        ));
        let prompt = format!(
            "You are resolving a Git rebase conflict for integration queue entry {}.              The worktree is {} on branch {}. The rebase target is session branch {} at commit {}.              Conflicted paths: {}.              Resolve the conflicts in place, preserving the intent of both sides. Do not abort the rebase.              Do not commit manually; leave the resolved files in the worktree. Run relevant tests if possible.              The runtime will stage resolved files, continue the rebase, verify the result, and merge it.              Reply with a concise summary of the resolution.",
            entry.id,
            worker_worktree.to_string_lossy(),
            entry.branch,
            session_branch,
            base_commit,
            conflict_paths.join(", ")
        );
        let mut conversation = vec![ChatMessage::user(prompt.clone())];
        let result = run_loop(
            &self.engine,
            config,
            session,
            project,
            control,
            &budget,
            &resolver_spec,
            &prompt,
            &mut conversation,
            None,
            false,
            None,
        )
        .await;
        let outcome = match result {
            Ok(LoopOutcome::Final(summary)) => summary,
            Ok(LoopOutcome::Deferred(wait)) => {
                let wt = worker_worktree.clone();
                tokio::task::spawn_blocking(move || worktree::abort_rebase(&wt)).await?;
                self.engine
                    .update_agent_status(&resolver.id, "failed")
                    .await?;
                self.engine
                    .update_merge_queue_entry(
                        &entry.id,
                        "conflict",
                        Some("auto-resolution cannot wait on an external gate".into()),
                    )
                    .await?;
                return Err(anyhow!(
                    "auto-resolution cannot defer to gate {}",
                    wait.gate_id
                ));
            }
            Ok(LoopOutcome::Cancelled) => {
                let wt = worker_worktree.clone();
                tokio::task::spawn_blocking(move || worktree::abort_rebase(&wt)).await?;
                self.engine
                    .update_agent_status(&resolver.id, "cancelled")
                    .await?;
                self.engine
                    .update_merge_queue_entry(
                        &entry.id,
                        "conflict",
                        Some("auto-resolution cancelled".into()),
                    )
                    .await?;
                return Err(anyhow!("auto-resolution cancelled"));
            }
            Ok(LoopOutcome::BudgetExceeded(stop)) => {
                let wt = worker_worktree.clone();
                tokio::task::spawn_blocking(move || worktree::abort_rebase(&wt)).await?;
                self.engine
                    .update_agent_status(&resolver.id, "failed")
                    .await?;
                self.engine
                    .update_merge_queue_entry(&entry.id, "conflict", Some(stop.summary()))
                    .await?;
                return Err(anyhow!("{} during auto-resolution", stop.summary()));
            }
            Ok(LoopOutcome::Verdict(_)) => {
                let wt = worker_worktree.clone();
                tokio::task::spawn_blocking(move || worktree::abort_rebase(&wt)).await?;
                self.engine
                    .update_agent_status(&resolver.id, "failed")
                    .await?;
                return Err(anyhow!("resolver returned a verdict unexpectedly"));
            }
            Err(error) => {
                let wt = worker_worktree.clone();
                tokio::task::spawn_blocking(move || worktree::abort_rebase(&wt)).await?;
                self.engine
                    .update_agent_status(&resolver.id, "failed")
                    .await?;
                let detail = truncate(&format!("auto-resolution failed: {error:#}"));
                self.engine
                    .update_merge_queue_entry(&entry.id, "conflict", Some(detail.clone()))
                    .await?;
                return Err(anyhow!(detail));
            }
        };

        let paths_worktree = worker_worktree.clone();
        let remaining = tokio::task::spawn_blocking(move || {
            if worktree::rebase_in_progress(&paths_worktree) {
                worktree::stage_all(&paths_worktree)?;
            }
            worktree::conflicted_paths(&paths_worktree)
        })
        .await??;
        if !remaining.is_empty() {
            let wt = worker_worktree.clone();
            tokio::task::spawn_blocking(move || worktree::abort_rebase(&wt)).await?;
            self.engine
                .update_agent_status(&resolver.id, "failed")
                .await?;
            let detail = truncate(&format!(
                "auto-resolution left {} conflicted path(s): {}",
                remaining.len(),
                remaining.join(", ")
            ));
            self.engine
                .update_merge_queue_entry(&entry.id, "conflict", Some(detail.clone()))
                .await?;
            return Err(anyhow!(detail));
        }

        let marker_worktree = worker_worktree.clone();
        let markers =
            tokio::task::spawn_blocking(move || worktree::conflict_markers(&marker_worktree))
                .await??;
        if !markers.is_empty() {
            let wt = worker_worktree.clone();
            tokio::task::spawn_blocking(move || worktree::abort_rebase(&wt)).await?;
            self.engine
                .update_agent_status(&resolver.id, "failed")
                .await?;
            let detail = truncate(&format!(
                "auto-resolution left conflict markers in: {}",
                markers.join(", ")
            ));
            self.engine
                .update_merge_queue_entry(&entry.id, "conflict", Some(detail.clone()))
                .await?;
            return Err(anyhow!(detail));
        }

        let continue_worktree = worker_worktree.clone();
        let continued = tokio::task::spawn_blocking(move || -> Result<()> {
            if worktree::rebase_in_progress(&continue_worktree) {
                worktree::continue_rebase(&continue_worktree)?;
            }
            Ok(())
        })
        .await?;
        if let Err(error) = continued {
            let wt = worker_worktree.clone();
            tokio::task::spawn_blocking(move || worktree::abort_rebase(&wt)).await?;
            self.engine
                .update_agent_status(&resolver.id, "failed")
                .await?;
            let detail = truncate(&format!("could not complete rebase: {error:#}"));
            self.engine
                .update_merge_queue_entry(&entry.id, "conflict", Some(detail.clone()))
                .await?;
            return Err(anyhow!(detail));
        }

        let commit_worktree = worker_worktree.clone();
        let commit =
            tokio::task::spawn_blocking(move || worktree::head_commit(&commit_worktree)).await??;
        if entry.source == MERGE_SOURCE_REMOTE
            && let Some(reference) = entry.imported_ref.clone()
        {
            let project_root = PathBuf::from(&project.root);
            let commit_for_ref = commit.clone();
            tokio::task::spawn_blocking(move || {
                crate::remote_integration::update_import_ref(
                    &project_root,
                    &reference,
                    &commit_for_ref,
                )
            })
            .await??;
        }
        self.engine
            .update_agent_status(&resolver.id, "idle")
            .await?;
        emit(
            &self.engine,
            &session.id,
            &resolver_spec,
            "agent.finished",
            serde_json::json!({
                "status": "idle",
                "summary": truncate(&outcome),
                "resolved": true,
                "commit": commit,
                "worktree": worker_worktree,
                "branch": entry.branch,
            }),
        )
        .await?;

        let manager_spec = AgentSpec {
            id: worker
                .parent_id
                .clone()
                .unwrap_or_else(|| "integrator".to_string()),
            role: AgentRole::Manager,
            name: "Integrator".to_string(),
            depth: 1,
            parent_name: None,
            title: Some("Integration".to_string()),
            task_id: None,
            department: None,
            identity_id: None,
            identity_generation: None,
            profile: AgentProfile::Generic,
            allow_delegation: false,
            task_kind: None,
            auto_approve: true,
            worktree: session_worktree.clone(),
            branch: session_branch.clone(),
            session_worktree: session_worktree.clone(),
            session_branch: session_branch.clone(),
            write_scope: Vec::new(),
            base_commit: base_commit.clone(),

            workspace_id: None,
            session_workspace_id: None,
            parent_workspace_id: None,
        };
        let worker_spec = AgentSpec {
            id: worker.id.clone(),
            role: AgentRole::Worker,
            name: worker.name.clone(),
            depth: 2,
            parent_name: Some("Integrator".to_string()),
            title: worker.title.clone(),
            task_id: entry.task_id.clone(),
            department: None,
            identity_id: None,
            identity_generation: None,
            profile: AgentProfile::Coding,
            allow_delegation: false,
            task_kind: None,
            auto_approve: true,
            worktree: worker_worktree.clone(),
            branch: entry.branch.clone(),
            session_worktree: session_worktree.clone(),
            session_branch: session_branch.clone(),
            write_scope: Vec::new(),
            base_commit: base_commit.clone(),

            workspace_id: None,
            session_workspace_id: None,
            parent_workspace_id: None,
        };
        let verification = verify_worker(
            &self.engine,
            config,
            session,
            project,
            control,
            &budget,
            &manager_spec,
            &worker_spec,
            &commit,
            &format!("Resolve integration conflict for queue entry {}", entry.id),
            &outcome,
            Some("No conflict markers; both sides' intent is preserved; relevant tests pass"),
            true,
            &verifier_provenance_for_entry(entry),
        )
        .await?;
        let verdict = match verification {
            VerificationOutcome::Approved(verdict) => verdict,
            VerificationOutcome::Rejected(verdict) => {
                let detail = truncate(&format!(
                    "auto-resolution rejected by Verifier: {}",
                    verdict.summary
                ));
                self.engine
                    .update_merge_queue_entry(&entry.id, "conflict", Some(detail.clone()))
                    .await?;
                return Err(anyhow!(detail));
            }
            VerificationOutcome::Cancelled => {
                self.engine
                    .update_merge_queue_entry(
                        &entry.id,
                        "conflict",
                        Some("auto-resolution verification cancelled".into()),
                    )
                    .await?;
                return Err(anyhow!("auto-resolution verification cancelled"));
            }
            VerificationOutcome::BudgetExceeded(stop) => {
                self.engine
                    .update_merge_queue_entry(
                        &entry.id,
                        "conflict",
                        Some(format!("{} during verification", stop.summary())),
                    )
                    .await?;
                return Err(anyhow!("{} during verification", stop.summary()));
            }
            VerificationOutcome::Error(error) => {
                let detail = truncate(&format!("auto-resolution verification failed: {error}"));
                self.engine
                    .update_merge_queue_entry(&entry.id, "conflict", Some(detail.clone()))
                    .await?;
                return Err(anyhow!(detail));
            }
        };

        let Some(entry) = self
            .engine
            .retry_merge_queue_entry(
                &entry.id,
                &commit,
                &base_commit,
                Some(format!(
                    "auto-resolved rebase conflict; verifier: {}",
                    truncate(&verdict.summary)
                )),
            )
            .await?
        else {
            return Err(anyhow!("merge queue entry can no longer be resolved"));
        };
        emit(
            &self.engine,
            &session.id,
            &resolver_spec,
            "integrator.conflict_resolution_finished",
            serde_json::json!({
                "entry_id": entry.id,
                "commit": commit,
                "verdict": truncate(&verdict.summary),
            }),
        )
        .await?;
        let integration_spec = AgentSpec {
            auto_approve: true,
            ..resolver_spec
        };
        let conflict_detail = format!(
            "{} path(s): {}; resolver: {}",
            conflict_paths.len(),
            conflict_paths.join(", "),
            outcome.trim()
        );
        let integration = process_merge_entry(
            &self.engine,
            session,
            &integration_spec,
            &entry,
            config.integration_command.as_deref(),
        )
        .await;
        let integration_detail = match &integration {
            Ok(detail) => detail.clone(),
            Err(error) => format!("integration failed: {error:#}"),
        };
        record_integration_resolution_memory(
            &self.engine,
            &session.id,
            &entry.id,
            entry.task_id.as_deref(),
            &commit,
            &verdict.summary,
            &conflict_detail,
            &integration_detail,
        )
        .await;
        integration?;
        let merged = self
            .engine
            .get_merge_queue_entry(&entry.id)
            .await?
            .ok_or_else(|| anyhow!("merge queue entry disappeared after auto-resolution"))?;
        if merged.status == "merged"
            && let Some(warning) = self.prune_remote_refs_after_merge(project, &merged).await
        {
            emit(
                &self.engine,
                &session.id,
                &integration_spec,
                "remote.integration.cleanup_failed",
                json!({"entry_id": merged.id, "detail": warning}),
            )
            .await?;
        }
        Ok(merged)
    }
}

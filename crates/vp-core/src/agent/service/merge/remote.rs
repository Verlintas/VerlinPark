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
}

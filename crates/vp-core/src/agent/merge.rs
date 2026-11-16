use super::*;

pub(super) fn verifier_provenance_for_entry(entry: &MergeQueueEntry) -> VerifierProvenance {
    if entry.source == MERGE_SOURCE_REMOTE {
        VerifierProvenance {
            source: Some(MERGE_SOURCE_REMOTE.to_string()),
            remote_host: entry.remote_host.clone(),
            remote_branch: entry.remote_branch.clone(),
            bundle_sha256: entry.bundle_sha256.clone(),
            environment_caveat: Some(crate::remote_integration::environment_caveat().to_string()),
        }
    } else {
        VerifierProvenance::default()
    }
}

pub(super) fn remote_integrator_spec(
    session: &Session,
    worktree: &Path,
    branch: &str,
    base_commit: &str,
) -> AgentSpec {
    AgentSpec {
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
        worktree: worktree.to_path_buf(),
        branch: branch.to_string(),
        session_worktree: session
            .worktree
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| worktree.to_path_buf()),
        session_branch: session.branch.clone().unwrap_or_else(|| branch.to_string()),
        write_scope: Vec::new(),
        base_commit: base_commit.to_string(),
        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
    }
}

pub(super) async fn delete_helper_bundle(
    workspace: &crate::remote::RemoteWorkspace,
    bundle_id: &str,
    operation_id: &str,
) {
    let _ = workspace
        .git_bundle_delete(vp_remote::GitBundleDeleteParams {
            bundle_id: bundle_id.to_string(),
            operation_id: Some(operation_id.to_string()),
        })
        .await;
}

/// Download a bundle in bounded chunks, verifying every chunk hash and the
/// total hash. The helper bundle is deleted on both success and failure; a
/// partial local file is never imported.
#[allow(clippy::too_many_arguments)]
pub(super) async fn download_remote_bundle(
    workspace: &crate::remote::RemoteWorkspace,
    session_id: &str,
    branch: &str,
    max_bytes: u64,
    chunk_bytes: u64,
    expected_commit: Option<&str>,
    base_commit: Option<&str>,
    import_id: &str,
    data_dir: &Path,
) -> Result<(PathBuf, RemoteBundleSummary), RemoteIntegrationFailure> {
    let operation_base = format!("remote-bundle:{session_id}:{import_id}");
    let created = workspace
        .git_bundle_create(vp_remote::GitBundleCreateParams {
            branch: branch.to_string(),
            base: base_commit.map(str::to_string),
            max_bytes,
            chunk_bytes,
            operation_id: Some(format!("{operation_base}:create")),
        })
        .await
        .map_err(|error| {
            RemoteIntegrationFailure::new(
                ToolErrorCode::REMOTE_GIT_FAILED,
                format!("remote bundle export failed: {error}"),
            )
        })?;
    if created.bytes == 0 || created.bytes > max_bytes {
        delete_helper_bundle(
            workspace,
            &created.bundle_id,
            &format!("{operation_base}:delete"),
        )
        .await;
        return Err(RemoteIntegrationFailure::new(
            ToolErrorCode::REMOTE_BUNDLE_TOO_LARGE,
            format!(
                "remote bundle reported {} bytes; the configured cap is {max_bytes}",
                created.bytes
            ),
        ));
    }
    if let Some(expected) = expected_commit
        && !expected.trim().is_empty()
        && !expected.trim().eq_ignore_ascii_case(&created.head)
    {
        delete_helper_bundle(
            workspace,
            &created.bundle_id,
            &format!("{operation_base}:delete"),
        )
        .await;
        return Err(RemoteIntegrationFailure::new(
            ToolErrorCode::REMOTE_BUNDLE_HEAD_MISMATCH,
            format!(
                "remote branch head {} does not match the expected commit {}",
                created.head,
                expected.trim()
            ),
        ));
    }
    let directory = data_dir.join("tmp").join("remote-bundles");
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(|error| {
            RemoteIntegrationFailure::new(
                ToolErrorCode::REMOTE_BUNDLE_IMPORT_FAILED,
                format!("cannot create local bundle directory: {error}"),
            )
        })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ =
            tokio::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).await;
    }
    let path = directory.join(format!("{import_id}.bundle"));
    let _ = tokio::fs::remove_file(&path).await;
    let result = async {
        let mut file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .await
            .map_err(|error| {
                RemoteIntegrationFailure::new(
                    ToolErrorCode::REMOTE_BUNDLE_IMPORT_FAILED,
                    format!("cannot create local bundle file: {error}"),
                )
            })?;
        let mut offset = 0u64;
        let mut chunks = 0u32;
        let mut hasher = Sha256::new();
        loop {
            let chunk = workspace
                .git_bundle_read(vp_remote::GitBundleReadParams {
                    bundle_id: created.bundle_id.clone(),
                    offset,
                    limit: chunk_bytes,
                })
                .await
                .map_err(|error| {
                    RemoteIntegrationFailure::new(
                        ToolErrorCode::REMOTE_GIT_FAILED,
                        format!("remote bundle chunk {chunks} failed: {error}"),
                    )
                })?;
            if chunk.bundle_id != created.bundle_id
                || chunk.offset != offset
                || chunk.total_bytes != created.bytes
                || !chunk.total_sha256.eq_ignore_ascii_case(&created.sha256)
            {
                return Err(RemoteIntegrationFailure::new(
                    ToolErrorCode::REMOTE_BUNDLE_TAMPERED,
                    "bundle chunk metadata changed between requests",
                ));
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(chunk.base64.as_bytes())
                .map_err(|error| {
                    RemoteIntegrationFailure::new(
                        ToolErrorCode::REMOTE_BUNDLE_TAMPERED,
                        format!("bundle chunk is not valid base64: {error}"),
                    )
                })?;
            if bytes.len() as u64 != chunk.bytes_read || bytes.len() as u64 > chunk_bytes {
                return Err(RemoteIntegrationFailure::new(
                    ToolErrorCode::REMOTE_BUNDLE_TAMPERED,
                    "bundle chunk length does not match the requested bound",
                ));
            }
            let mut chunk_hasher = Sha256::new();
            chunk_hasher.update(&bytes);
            if format!("{:x}", chunk_hasher.finalize()) != chunk.chunk_sha256 {
                return Err(RemoteIntegrationFailure::new(
                    ToolErrorCode::REMOTE_BUNDLE_TAMPERED,
                    "bundle chunk SHA-256 does not match the helper report",
                ));
            }
            if offset.saturating_add(bytes.len() as u64) > max_bytes {
                return Err(RemoteIntegrationFailure::new(
                    ToolErrorCode::REMOTE_BUNDLE_TOO_LARGE,
                    format!("downloaded bundle exceeds the {max_bytes}-byte cap"),
                ));
            }
            file.write_all(&bytes).await.map_err(|error| {
                RemoteIntegrationFailure::new(
                    ToolErrorCode::REMOTE_BUNDLE_IMPORT_FAILED,
                    format!("cannot write local bundle: {error}"),
                )
            })?;
            hasher.update(&bytes);
            offset = offset.saturating_add(bytes.len() as u64);
            chunks = chunks.saturating_add(1);
            if chunk.done {
                break;
            }
            if bytes.is_empty() {
                return Err(RemoteIntegrationFailure::new(
                    ToolErrorCode::REMOTE_BUNDLE_TAMPERED,
                    "helper returned an empty chunk before the bundle was complete",
                ));
            }
        }
        file.sync_all().await.map_err(|error| {
            RemoteIntegrationFailure::new(
                ToolErrorCode::REMOTE_BUNDLE_IMPORT_FAILED,
                format!("cannot sync local bundle: {error}"),
            )
        })?;
        let total_sha = format!("{:x}", hasher.finalize());
        if offset != created.bytes || !total_sha.eq_ignore_ascii_case(&created.sha256) {
            return Err(RemoteIntegrationFailure::new(
                ToolErrorCode::REMOTE_BUNDLE_TAMPERED,
                format!(
                    "downloaded bundle has {} bytes and SHA-256 {}, expected {} bytes and {}",
                    offset, total_sha, created.bytes, created.sha256
                ),
            ));
        }
        Ok(RemoteBundleSummary {
            host: String::new(),
            branch: created.branch.clone(),
            head: created.head.clone(),
            sha256: created.sha256.clone(),
            bytes: created.bytes,
            chunks,
            chunk_bytes,
            imported_at: vp_protocol::now_rfc3339(),
        })
    }
    .await;
    delete_helper_bundle(
        workspace,
        &created.bundle_id,
        &format!("{operation_base}:delete"),
    )
    .await;
    match result {
        Ok(summary) => Ok((path, summary)),
        Err(error) => {
            let _ = tokio::fs::remove_file(&path).await;
            Err(error)
        }
    }
}

/// Refresh a workspace-bound run's base commit after its in-process workspace
/// lock is acquired. Without this, two serialized plan Workers on one workspace
/// would each treat the other's commit as their own change and enqueue it again.
pub(super) async fn refresh_workspace_base(spec: &mut AgentSpec) {
    if spec.workspace_id.is_none() {
        return;
    }
    let path = spec.worktree.clone();
    if let Ok(Ok(head)) = tokio::task::spawn_blocking(move || worktree::head_commit(&path)).await {
        spec.base_commit = head;
    }
}

/// Bounded, evidence-only changed paths for a Worker result. Paths are not
/// file content and never include a diff. They are persisted so failure
/// attribution can connect a commit to the files it touched without reading
/// tool output or the repository at diagnosis time.
pub(super) fn bounded_worker_changed_paths(spec: &AgentSpec, commit: &str) -> Vec<String> {
    let base = if spec.base_commit.trim().is_empty() {
        format!("{commit}^")
    } else {
        spec.base_commit.clone()
    };
    crate::worktree::changed_paths(&spec.worktree, &base)
        .unwrap_or_default()
        .into_iter()
        .take(64)
        .map(|path| path.chars().take(240).collect())
        .collect()
}

pub(super) async fn commit_worker(spec: &AgentSpec, name: &str) -> Result<Option<String>> {
    let task_worktree = spec.worktree.clone();
    let message = format!("task: {name}");
    let write_scope = spec.write_scope.clone();
    let base_commit = spec.base_commit.clone();
    tokio::task::spawn_blocking(move || -> Result<Option<String>> {
        if !write_scope.is_empty() {
            let paths = worktree::changed_paths(&task_worktree, &base_commit)?;
            let outside = paths
                .iter()
                .filter(|path| !path_in_scope(path, &write_scope))
                .cloned()
                .collect::<Vec<_>>();
            if !outside.is_empty() {
                return Err(anyhow!("write scope violation: {}", outside.join(", ")));
            }
        }
        let committed = worktree::commit_all(&task_worktree, &message)?;
        if committed.is_some() {
            return Ok(committed);
        }
        // A resumed task may already have a committed branch even though its
        // worktree is clean. Re-enter verification/integration with that commit
        // instead of treating the task as a no-op.
        let head = worktree::head_commit(&task_worktree)?;
        if head == base_commit {
            Ok(None)
        } else {
            Ok(Some(head))
        }
    })
    .await?
}

/// Record the local Worker commit as `done` evidence. The commit is already
/// visible in the task branch; `rev-parse` supplies the postcondition and a
/// duplicate logical commit (same task/branch/commit) returns the stored row.
pub(super) async fn record_worker_commit_effect(
    engine: &Engine,
    session: &Session,
    spec: &AgentSpec,
    commit: &str,
) {
    let target = crate::effect::EffectTarget::new(
        crate::effect::kinds::GIT_COMMIT,
        format!(
            "{}:{}",
            spec.branch,
            spec.task_id.as_deref().unwrap_or("session")
        ),
    );
    let request = match crate::effect::canonical_request_hash(
        "worker.commit",
        json!({
            "branch": spec.branch,
            "base": spec.base_commit,
            "commit": commit,
            "task_id": spec.task_id,
        }),
    ) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(%error, "failed to build the worker commit effect identity");
            return;
        }
    };
    let mut receipt = crate::effect::runtime_receipt(
        &session.id,
        spec.task_id.as_deref(),
        "worker.commit",
        target,
        request,
        Some("workspace".to_string()),
    );
    receipt.state = EffectState::Done;
    receipt.external_id = Some(commit.to_string());
    receipt.response_hash = Some(crate::sha256_hex(commit.as_bytes()));
    receipt.evidence = vp_protocol::types::EffectEvidence {
        probe: crate::effect::probes::GIT_REV_PARSE.to_string(),
        outcome: "observed".to_string(),
        commit: Some(commit.to_string()),
        branch: Some(spec.branch.clone()),
        ..Default::default()
    };
    if let Err(error) = engine.record_settled_effect(receipt).await {
        tracing::warn!(%error, "failed to record the worker commit effect receipt");
    }
}

/// Record the deterministic Integrator's merge as `done` after the final
/// lease check. The session HEAD is the postcondition; the entry commit is
/// retained as the source identity.
pub(super) async fn record_integration_merge_effect(
    engine: &Engine,
    session: &Session,
    spec: &AgentSpec,
    entry: &MergeQueueEntry,
) {
    let worktree_path = spec.session_worktree.clone();
    let head = tokio::task::spawn_blocking(move || worktree::head_commit(&worktree_path)).await;
    let head = match head {
        Ok(Ok(head)) => head,
        Ok(Err(error)) => {
            tracing::warn!(%error, "integration merge head probe failed");
            return;
        }
        Err(error) => {
            tracing::warn!(%error, "integration merge head probe task failed");
            return;
        }
    };
    let target = crate::effect::EffectTarget::new(
        crate::effect::kinds::GIT_MERGE,
        format!("{}:{}", spec.branch, entry.id),
    );
    let request = match crate::effect::canonical_request_hash(
        "integrator.merge",
        json!({
            "entry_id": entry.id,
            "branch": spec.branch,
            "commit": entry.commit,
            "merge_head": head,
            "attempt": entry.attempt,
        }),
    ) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(%error, "failed to build the integration merge effect identity");
            return;
        }
    };
    let mut receipt = crate::effect::runtime_receipt(
        &session.id,
        entry.task_id.as_deref(),
        "integrator.merge",
        target,
        request,
        Some("workspace".to_string()),
    );
    receipt.state = EffectState::Done;
    receipt.external_id = Some(head.clone());
    receipt.response_hash = Some(crate::sha256_hex(head.as_bytes()));
    receipt.evidence = vp_protocol::types::EffectEvidence {
        probe: crate::effect::probes::GIT_MERGE_HEAD.to_string(),
        outcome: "observed".to_string(),
        commit: Some(head),
        branch: Some(spec.branch.clone()),
        note: Some(format!("merged queue entry {}", entry.id)),
        ..Default::default()
    };
    if let Err(error) = engine.record_settled_effect(receipt).await {
        tracing::warn!(%error, "failed to record the integration merge effect receipt");
    }
}

pub(super) async fn run_integration_command(
    engine: &Engine,
    project: &Project,
    session: &Session,
    worktree: PathBuf,
    command: String,
) -> Result<String> {
    let policy = engine.get_shell_policy(&project.id).await?;
    let cache_policy = engine.get_test_cache_policy(&project.id).await?;
    let mut cache_key = None;
    let mut inputs_hash = None;
    let mut impact_generation = None;
    let mut cache_access = None;
    if cache_policy.enabled && cache_policy.include_integration {
        let profile = crate::tooling::project::detect_profile(&worktree);
        let profile_hash = crate::test_cache::profile_hash(&profile);
        let fingerprint = crate::test_cache::compute_input_fingerprint(&worktree, &[], &[]);
        if fingerprint.complete {
            let generation = crate::test_cache::full_impact_generation(&profile_hash);
            let material = crate::test_cache::TestCacheKeyMaterial::resolved(
                "check",
                &command,
                &profile_hash,
                &fingerprint.hash,
                &generation,
            );
            let key = crate::test_cache::compute_cache_key(&material);
            let access =
                crate::test_cache::TestCacheAccess::new(engine.store().clone(), project.id.clone());
            let lookup_access = access.clone();
            let lookup_key = key.clone();
            let lookup_session = session.id.clone();
            let hit = tokio::task::spawn_blocking(move || {
                lookup_access.lookup(&lookup_key, &lookup_session, &vp_protocol::now_rfc3339())
            })
            .await??;
            if let Some(hit) = hit {
                let text = hit
                    .result
                    .get("raw_tail")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if hit.entry.status == "passed" {
                    return Ok(format!(
                        "{text}\n[integration cache hit {}: no command ran; inputs {}]",
                        hit.entry.id, hit.entry.inputs_hash
                    ));
                }
                return Err(anyhow!(
                    "integration command cached failure (exit {}):\n{text}",
                    hit.entry.exit_code
                ));
            }
            cache_key = Some(key);
            inputs_hash = Some(fingerprint.hash);
            impact_generation = Some(generation);
            cache_access = Some(access);
        } else {
            tracing::warn!(
                session_id = %session.id,
                "integration cache policy is enabled but input hashing was incomplete; running fresh"
            );
        }
    }
    let mut secret_roots = shell_secret_roots(engine.store().data_dir());
    secret_roots.extend(policy.read_deny.iter().map(PathBuf::from));
    let context = crate::tools::ToolContext {
        root: worktree.clone(),
        project_root: PathBuf::from(&project.root),
        sandbox_root: engine
            .store()
            .data_dir()
            .join("sandbox")
            .join(&session.id)
            .join("integration"),
        secret_roots,
        network_allowed: policy.network != "deny",
        home: env::var_os("HOME").map(PathBuf::from).unwrap_or_default(),
        restrict_home: policy.restrict_home,
        read_jail: policy.read_jail,
        git_branch: session.branch.clone(),
        remote: None,
        session_id: None,
        process_supervisor: None,
        test_cache: None,
    };
    let (exit, text) =
        crate::tools::run_sandboxed_login_shell(&context, &command, Duration::from_secs(300))
            .await?;
    let text = truncate(&text);
    if let (Some(access), Some(key), Some(hash), Some(generation)) =
        (cache_access, cache_key, inputs_hash, impact_generation)
    {
        let retention = if exit == 0 {
            u64::from(cache_policy.retention_days).saturating_mul(86_400)
        } else {
            cache_policy.failed_retention_secs
        };
        let spill = crate::test_cache::spill_cache_output(
            &access,
            &session.id,
            &text,
            cache_policy.max_output_bytes,
            retention,
        );
        let spec = crate::test_cache::TestResultSpec {
            session_id: session.id.clone(),
            kind: "check".to_string(),
            command: command.clone(),
            command_source: "integration_policy".to_string(),
            cache_key: key,
            inputs_hash: hash,
            impact_generation: generation,
            status: if exit == 0 { "passed" } else { "failed" }.to_string(),
            exit_code: exit,
            duration_ms: 0,
            output_ref: spill.output_ref,
            output_bytes: spill.bytes,
            covered_paths: Vec::new(),
            warnings: Vec::new(),
            warning_count: 0,
            failure_count: u64::from(exit != 0),
            result_json: serde_json::json!({
                "ok": exit == 0,
                "raw_tail": text,
                "exit_code": exit,
                "source": "integration_policy",
            }),
        };
        let persist_access = access;
        let persist_policy = cache_policy.clone();
        if let Err(error) =
            tokio::task::spawn_blocking(move || persist_access.persist(spec, &persist_policy))
                .await?
        {
            tracing::warn!(
                session_id = %session.id,
                %error,
                "failed to persist integration command cache metadata"
            );
        }
    }
    if exit == 0 {
        Ok(text)
    } else {
        Err(anyhow!("integration command exited with {exit}:\n{text}"))
    }
}

pub(super) async fn integrate_worker(
    engine: &Engine,
    session: &Session,
    spec: &AgentSpec,
    commit: &str,
    verifier_summary: &str,
    integration_command: Option<&str>,
) -> Result<String> {
    let worktree = spec.session_worktree.clone();
    let base_commit =
        tokio::task::spawn_blocking(move || worktree::head_commit(&worktree)).await??;
    let entry = engine
        .enqueue_merge(
            &session.id,
            &spec.id,
            &spec.name,
            spec.task_id.clone(),
            &spec.branch,
            commit,
            &base_commit,
            Some(verifier_summary.to_string()),
        )
        .await?;
    process_merge_entry(engine, session, spec, &entry, integration_command).await
}

/// Serialize integration across processes with a runtime lease, then run the
/// deterministic merge/check path.
pub(super) async fn process_merge_entry(
    engine: &Engine,
    session: &Session,
    spec: &AgentSpec,
    entry: &MergeQueueEntry,
    integration_command: Option<&str>,
) -> Result<String> {
    let lease = engine
        .acquire_runtime_lease(
            "integration",
            &session.id,
            std::time::Duration::from_secs(900),
        )
        .await?;
    let result =
        process_merge_entry_fenced(engine, session, spec, entry, integration_command, &lease).await;
    lease.release().await;
    result
}

/// Merge and integration-check an existing queue entry under an already-held
/// integration lease. The entry must already have a valid `base_commit` (the
/// session HEAD before this merge). The fence is checked immediately before
/// the queue row starts `merging` and again inside the Git critical section
/// immediately before `git merge`; a stale integrator never mutates the
/// session branch.
pub(super) async fn process_merge_entry_fenced(
    engine: &Engine,
    session: &Session,
    spec: &AgentSpec,
    entry: &MergeQueueEntry,
    integration_command: Option<&str>,
    lease: &RuntimeLeaseGuard,
) -> Result<String> {
    let (_, project) = load_session_project(engine.store().clone(), &session.id).await?;
    // Effects may have become unresolved while the entry waited in the queue.
    // Re-check under the integration lease; never merge incomplete work.
    if let Err(error) = engine
        .assert_effects_settled(&session.id, entry.task_id.as_deref())
        .await
    {
        let detail = truncate(&format!("{error:#}"));
        engine
            .update_merge_queue_entry(&entry.id, "failed", Some(detail.clone()))
            .await?;
        emit(
            engine,
            &session.id,
            spec,
            "integrator.finished",
            serde_json::json!({
                "entry_id": entry.id,
                "status": "failed",
                "detail": detail,
                "reason": "effect_unsettled",
            }),
        )
        .await?;
        return Err(error);
    }
    let preflight_worktree = spec.session_worktree.clone();
    let git_lock = engine.git_mutex();
    let preflight = {
        let git_lock = git_lock.clone();
        let preflight_worktree = preflight_worktree.clone();
        tokio::task::spawn_blocking(move || -> Result<String> {
            let _guard = git_lock.lock().map_err(|_| anyhow!("git mutex poisoned"))?;
            if !worktree::is_clean(&preflight_worktree)? {
                return Err(anyhow!(
                    "session worktree has uncommitted tracked changes; refusing to integrate"
                ));
            }
            worktree::head_commit(&preflight_worktree)
        })
        .await?
    };
    let preflight = match preflight {
        Ok(commit) => commit,
        Err(error) => {
            let detail = truncate(&format!("merge preflight failed: {error}"));
            engine
                .update_merge_queue_entry(&entry.id, "failed", Some(detail.clone()))
                .await?;
            emit(
                engine,
                &session.id,
                spec,
                "integrator.finished",
                serde_json::json!({
                    "entry_id": entry.id,
                    "status": "failed",
                    "detail": detail,
                }),
            )
            .await?;
            return Err(anyhow!(detail));
        }
    };
    if let Err(error) = lease.assert_held().await {
        let detail = truncate(&format!(
            "integration lease lost before merge: {error}; session branch was not changed"
        ));
        engine
            .update_merge_queue_entry(&entry.id, "failed", Some(detail.clone()))
            .await?;
        emit(
            engine,
            &session.id,
            spec,
            "integrator.finished",
            serde_json::json!({
                "entry_id": entry.id,
                "status": "failed",
                "detail": detail,
                "reason": "lease_lost_before_merge",
            }),
        )
        .await?;
        return Err(anyhow!(detail));
    }
    engine
        .begin_merge_queue_entry(&entry.id, &preflight)
        .await?;
    emit(
        engine,
        &session.id,
        spec,
        "integrator.started",
        serde_json::json!({
            "entry_id": entry.id,
            "branch": entry.branch,
            "base_commit": entry.base_commit,
            "pre_merge_commit": preflight,
            "attempt": entry.attempt,
            "has_command": integration_command.is_some(),
        }),
    )
    .await?;

    let merge_worktree = spec.session_worktree.clone();
    let merge_commit = entry.commit.clone();
    let expected_base = preflight.clone();
    let message = format!("merge {}", spec.name);
    let fence_store = engine.store().clone();
    let fence_key = lease.lease_key.clone();
    let fence_owner = lease.owner_id.clone();
    let fence_token = lease.token().clone();
    let merge_result = tokio::task::spawn_blocking(move || -> Result<String> {
        let _guard = git_lock.lock().map_err(|_| anyhow!("git mutex poisoned"))?;
        // Effect-boundary fence: this is the last read before the session
        // branch is mutated. A stale generation aborts without running merge.
        fence_store
            .assert_runtime_lease(&fence_key, &fence_owner, &fence_token)
            .map_err(|error| anyhow!("integration lease lost before merge: {error}"))?;
        let actual_base = worktree::head_commit(&merge_worktree)?;
        if actual_base != expected_base {
            tracing::warn!(
                expected = %expected_base,
                actual = %actual_base,
                "merge queue base moved; integrating against current session HEAD"
            );
        }
        worktree::merge_commit(&merge_worktree, &merge_commit, &message)?;
        Ok(actual_base)
    })
    .await?;
    let actual_base = match merge_result {
        Ok(base) => base,
        Err(error) => {
            let message = format!("{error:#}");
            let conflict = message.to_lowercase().contains("conflict")
                || message.contains("Automatic merge failed");
            let status = if conflict { "conflict" } else { "failed" };
            let detail = truncate(&if conflict {
                format!("merge conflict: {message}")
            } else {
                format!("merge failed: {message}")
            });
            engine
                .update_merge_queue_entry(&entry.id, status, Some(detail.clone()))
                .await?;
            emit(
                engine,
                &session.id,
                spec,
                "integrator.finished",
                serde_json::json!({
                    "entry_id": entry.id,
                    "status": status,
                    "detail": detail,
                }),
            )
            .await?;
            return Err(anyhow!(detail));
        }
    };

    // The row could expire during the merge itself. Roll the branch back to
    // the pre-merge HEAD before touching the queue state so a newer owner
    // never observes a half-finished integration from a stale generation.
    if let Err(error) = lease.assert_held().await {
        let reset_worktree = spec.session_worktree.clone();
        let reset_base = actual_base.clone();
        let reset =
            tokio::task::spawn_blocking(move || worktree::reset_hard(&reset_worktree, &reset_base))
                .await?;
        let detail = truncate(&match reset {
            Ok(()) => {
                format!("integration lease lost after merge: {error}; rolled back to {actual_base}")
            }
            Err(reset_error) => format!(
                "integration lease lost after merge: {error}; rollback failed: {reset_error}"
            ),
        });
        engine
            .update_merge_queue_entry(&entry.id, "failed", Some(detail.clone()))
            .await?;
        emit(
            engine,
            &session.id,
            spec,
            "integrator.finished",
            serde_json::json!({
                "entry_id": entry.id,
                "status": "failed",
                "detail": detail,
                "reason": "lease_lost_after_merge",
            }),
        )
        .await?;
        return Err(anyhow!(detail));
    }
    engine
        .update_merge_queue_entry(&entry.id, "checking", None)
        .await?;
    let check_worktree = spec.session_worktree.clone();
    let check_base = actual_base.clone();
    let check_result = tokio::task::spawn_blocking(move || {
        worktree::check_merged_diff(&check_worktree, &check_base)
    })
    .await?;
    let check_result = match check_result {
        Ok(()) => {
            if let Some(command) = integration_command {
                run_integration_command(
                    engine,
                    &project,
                    session,
                    spec.session_worktree.clone(),
                    command.to_string(),
                )
                .await
            } else {
                Ok("diff check passed".to_string())
            }
        }
        Err(error) => Err(error),
    };

    let integration_detail = match check_result {
        Ok(detail) => detail,
        Err(error) => {
            let mut detail = format!("integration check failed: {error}");
            // A new owner may have merged on top while the check command was
            // running. Only the generation that owns the key may reset its own
            // merge; otherwise leave the worktree for operator inspection.
            let owned = match lease.assert_held().await {
                Ok(_) => true,
                Err(lease_error) => {
                    detail = format!(
                        "{detail}; rollback skipped because the integration lease was lost: {lease_error}"
                    );
                    false
                }
            };
            if owned {
                let reset_worktree = spec.session_worktree.clone();
                let reset_base = actual_base.clone();
                let reset = tokio::task::spawn_blocking(move || {
                    worktree::reset_hard(&reset_worktree, &reset_base)
                })
                .await?;
                detail = match reset {
                    Ok(()) => detail,
                    Err(reset_error) => format!("{detail}; rollback failed: {reset_error}"),
                };
            }
            let detail = truncate(&detail);
            engine
                .update_merge_queue_entry(&entry.id, "failed", Some(detail.clone()))
                .await?;
            emit(
                engine,
                &session.id,
                spec,
                "integrator.finished",
                serde_json::json!({
                    "entry_id": entry.id,
                    "status": "failed",
                    "detail": detail,
                }),
            )
            .await?;
            return Err(anyhow!(detail));
        }
    };
    let integration_detail = truncate(&integration_detail);
    // Final effect-boundary check: the integration command may have run for
    // minutes. If ownership moved, discard this generation's merge instead of
    // publishing a merged entry that a newer owner cannot reason about.
    if let Err(error) = lease.assert_held().await {
        let reset_worktree = spec.session_worktree.clone();
        let reset_base = actual_base.clone();
        let reset =
            tokio::task::spawn_blocking(move || worktree::reset_hard(&reset_worktree, &reset_base))
                .await?;
        let detail = truncate(&match reset {
            Ok(()) => format!(
                "integration lease lost before finalize: {error}; rolled back to {actual_base}"
            ),
            Err(reset_error) => format!(
                "integration lease lost before finalize: {error}; rollback failed: {reset_error}"
            ),
        });
        engine
            .update_merge_queue_entry(&entry.id, "failed", Some(detail.clone()))
            .await?;
        emit(
            engine,
            &session.id,
            spec,
            "integrator.finished",
            serde_json::json!({
                "entry_id": entry.id,
                "status": "failed",
                "detail": detail,
                "reason": "lease_lost_before_finalize",
            }),
        )
        .await?;
        return Err(anyhow!(detail));
    }
    record_integration_merge_effect(engine, session, spec, entry).await;
    engine
        .update_merge_queue_entry(&entry.id, "merged", Some(integration_detail.clone()))
        .await?;
    emit(
        engine,
        &session.id,
        spec,
        "integrator.finished",
        serde_json::json!({
            "entry_id": entry.id,
            "status": "merged",
            "detail": integration_detail,
            "attempt": entry.attempt,
        }),
    )
    .await?;
    Ok(format!("merged; {integration_detail}"))
}

pub(super) enum VerificationOutcome {
    Approved(VerifierVerdict),
    Rejected(VerifierVerdict),
    Cancelled,
    BudgetExceeded(BudgetStop),
    Error(String),
}

/// Provenance attached to a Verifier run. Local Worker verification uses the
/// default empty value; remote integration records the exact host, branch,
/// bundle hash, and the environment caveat so the event stream never implies
/// that the remote host was reproduced.
#[derive(Debug, Clone, Default)]
pub(crate) struct VerifierProvenance {
    pub source: Option<String>,
    pub remote_host: Option<String>,
    pub remote_branch: Option<String>,
    pub bundle_sha256: Option<String>,
    pub environment_caveat: Option<String>,
}

pub(super) fn verification_payload(
    mut payload: serde_json::Value,
    provenance: &VerifierProvenance,
) -> serde_json::Value {
    let Some(object) = payload.as_object_mut() else {
        return payload;
    };
    if let Some(source) = provenance.source.as_deref() {
        object.insert("source".into(), serde_json::json!(source));
    }
    if let Some(host) = provenance.remote_host.as_deref() {
        object.insert("remote_host".into(), serde_json::json!(host));
    }
    if let Some(branch) = provenance.remote_branch.as_deref() {
        object.insert("remote_branch".into(), serde_json::json!(branch));
    }
    if let Some(bundle) = provenance.bundle_sha256.as_deref() {
        object.insert("bundle_sha256".into(), serde_json::json!(bundle));
    }
    if let Some(caveat) = provenance.environment_caveat.as_deref() {
        object.insert("environment_caveat".into(), serde_json::json!(caveat));
    }
    payload
}

pub(super) struct VerificationCheckout {
    pub(super) path: PathBuf,
}

impl Drop for VerificationCheckout {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.path) {
            tracing::warn!(%error, path = %self.path.display(), "failed to remove verification checkout");
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn verify_worker(
    engine: &Engine,
    config: &AgentConfig,
    session: &Session,
    project: &Project,
    control: &RunControl,
    budget: &SessionBudget,
    manager_spec: &AgentSpec,
    worker_spec: &AgentSpec,
    commit: &str,
    objective: &str,
    worker_summary: &str,
    acceptance: Option<&str>,
    auto_approve: bool,
    provenance: &VerifierProvenance,
) -> Result<VerificationOutcome> {
    let Some((ordinal, _verifier_slot)) = budget.reserve_slot(AgentRole::Verifier) else {
        return Ok(VerificationOutcome::Error(
            "the Verifier cap for this session has been reached".to_string(),
        ));
    };
    let verifier_name = format!("Verifier {ordinal}");
    let checkout_path = match worktree::create_verification_checkout(
        Path::new(&project.root),
        engine.store().data_dir(),
        &session.id,
        commit,
    ) {
        Ok(path) => path,
        Err(error) => {
            return Ok(VerificationOutcome::Error(format!(
                "could not create isolated verification checkout: {error}"
            )));
        }
    };
    let _checkout = VerificationCheckout {
        path: checkout_path.clone(),
    };
    let verifier_branch = format!("verify/{}", truncate_to(commit, 12));
    let verifier = engine
        .create_agent(
            &session.id,
            AgentRole::Verifier.as_str(),
            &verifier_name,
            Some("Verification".to_string()),
            Some(manager_spec.id.clone()),
            &model_for(config, AgentRole::Verifier),
        )
        .await?;
    engine
        .set_agent_worktree(
            &verifier.id,
            &checkout_path.to_string_lossy(),
            &verifier_branch,
        )
        .await?;
    engine
        .set_agent_profile(&verifier.id, AgentProfile::Verification.as_str())
        .await?;
    engine
        .set_agent_task(
            &verifier.id,
            worker_spec.task_id.clone(),
            worker_spec.department.clone(),
        )
        .await?;
    let verifier_spec = AgentSpec {
        id: verifier.id.clone(),
        role: AgentRole::Verifier,
        name: verifier_name.clone(),
        depth: manager_spec.depth.saturating_add(1),
        parent_name: Some(manager_spec.name.clone()),
        title: Some("Verification".to_string()),
        task_id: worker_spec.task_id.clone(),
        department: worker_spec.department.clone(),
        identity_id: None,
        identity_generation: None,
        profile: AgentProfile::Verification,
        allow_delegation: false,
        task_kind: None,
        auto_approve,
        worktree: checkout_path.clone(),
        branch: verifier_branch,
        session_worktree: manager_spec.session_worktree.clone(),
        session_branch: manager_spec.session_branch.clone(),
        write_scope: Vec::new(),
        base_commit: worker_spec.base_commit.clone(),

        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
    };
    let focus = match worker_spec.profile {
        AgentProfile::Research => {
            "Check the report, sources, and citations; use fetch_url if a source needs verification."
        }
        AgentProfile::Coding => {
            "Inspect the diff, run relevant tests with shell, and check the acceptance criteria."
        }
        AgentProfile::Verification => "Inspect the evidence and run checks; do not edit files.",
        AgentProfile::Generic => "Inspect the output and evidence; do not edit files.",
    };
    let acceptance_note = acceptance
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!(" Acceptance criteria: {value}."))
        .unwrap_or_default();
    let provenance_note = match (
        provenance.remote_host.as_deref(),
        provenance.remote_branch.as_deref(),
    ) {
        (Some(host), Some(branch)) => format!(
            " This is an imported remote commit from {host}:{branch}; local verification does not reproduce the remote host environment."
        ),
        _ => String::new(),
    };
    let prompt = format!(
        "Verify Worker {} for this task. Objective: {objective}.{acceptance_note} \
         Worker summary: {worker_summary}. The isolated verification worktree is {} on branch {}; base commit {}.{provenance_note} \
         {focus} Do not edit files. Then call submit_verdict with approved and summary.",
        worker_spec.name,
        verifier_spec.worktree.to_string_lossy(),
        verifier_spec.branch,
        verifier_spec.base_commit
    );
    emit(
        engine,
        &session.id,
        &verifier_spec,
        "agent.spawned",
        verification_payload(
            serde_json::json!({
                "parent_id": manager_spec.id,
                "parent_name": manager_spec.name,
                "objective": truncate(&prompt),
                "verified_worker": worker_spec.name,
            }),
            provenance,
        ),
    )
    .await?;
    let mut conversation = vec![ChatMessage::user(prompt.clone())];
    let result = run_loop(
        engine,
        config,
        session,
        project,
        control,
        budget,
        &verifier_spec,
        &prompt,
        &mut conversation,
        None,
        false,
        None,
    )
    .await;
    match result {
        Ok(LoopOutcome::Verdict(verdict)) => {
            let verify_root = verifier_spec.worktree.clone();
            let expected_commit = commit.to_string();
            let head_root = verify_root.clone();
            let head =
                tokio::task::spawn_blocking(move || worktree::head_commit(&head_root)).await??;
            if head != expected_commit {
                let reset_root = verify_root.clone();
                let reset_commit = expected_commit.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    worktree::reset_hard(&reset_root, &reset_commit)
                })
                .await?;
                let summary = format!(
                    "Verifier changed HEAD from {} to {}",
                    truncate_to(&expected_commit, 12),
                    truncate_to(&head, 12)
                );
                engine.update_agent_status(&verifier.id, "failed").await?;
                emit(
                    engine,
                    &session.id,
                    &verifier_spec,
                    "agent.finished",
                    verification_payload(
                        serde_json::json!({
                            "status": "failed",
                            "approved": false,
                            "summary": summary,
                            "verified_worker": worker_spec.name,
                            "worktree": verifier_spec.worktree,
                            "branch": verifier_spec.branch,
                        }),
                        provenance,
                    ),
                )
                .await?;
                return Ok(VerificationOutcome::Rejected(VerifierVerdict {
                    approved: false,
                    summary,
                }));
            }
            let changed =
                tokio::task::spawn_blocking(move || worktree::tracked_changes(&verify_root))
                    .await??;
            if !changed.is_empty() {
                let reset_root = verifier_spec.worktree.clone();
                if let Err(error) = tokio::task::spawn_blocking(move || {
                    worktree::reset_tracked_changes(&reset_root)
                })
                .await?
                {
                    tracing::warn!(%error, "failed to reset verifier worktree changes");
                }
                let summary = format!(
                    "Verifier modified tracked files: {}",
                    truncate_to(&changed.join(", "), 300)
                );
                engine.update_agent_status(&verifier.id, "failed").await?;
                emit(
                    engine,
                    &session.id,
                    &verifier_spec,
                    "agent.finished",
                    verification_payload(
                        serde_json::json!({
                            "status": "failed",
                            "approved": false,
                            "summary": summary,
                            "modified_paths": changed,
                            "verified_worker": worker_spec.name,
                            "worktree": verifier_spec.worktree,
                            "branch": verifier_spec.branch,
                        }),
                        provenance,
                    ),
                )
                .await?;
                return Ok(VerificationOutcome::Rejected(VerifierVerdict {
                    approved: false,
                    summary,
                }));
            }
            engine.update_agent_status(&verifier.id, "idle").await?;
            emit(
                engine,
                &session.id,
                &verifier_spec,
                "agent.finished",
                verification_payload(
                    serde_json::json!({
                        "status": "idle",
                        "approved": verdict.approved,
                        "summary": truncate(&verdict.summary),
                        "verified_worker": worker_spec.name,
                        "worktree": verifier_spec.worktree,
                        "branch": verifier_spec.branch,
                    }),
                    provenance,
                ),
            )
            .await?;
            Ok(if verdict.approved {
                VerificationOutcome::Approved(verdict)
            } else {
                VerificationOutcome::Rejected(verdict)
            })
        }
        Ok(LoopOutcome::Final(text)) => {
            let summary = if text.trim().is_empty() {
                "Verifier finished without submitting a verdict.".to_string()
            } else {
                format!("Verifier finished without submitting a verdict: {text}")
            };
            engine.update_agent_status(&verifier.id, "failed").await?;
            emit(
                engine,
                &session.id,
                &verifier_spec,
                "agent.finished",
                verification_payload(
                    serde_json::json!({
                        "status": "failed",
                        "approved": false,
                        "summary": truncate(&summary),
                        "verified_worker": worker_spec.name,
                    }),
                    provenance,
                ),
            )
            .await?;
            Ok(VerificationOutcome::Error(summary))
        }
        Ok(LoopOutcome::Cancelled) => {
            engine
                .update_agent_status(&verifier.id, "cancelled")
                .await?;
            emit(
                engine,
                &session.id,
                &verifier_spec,
                "agent.finished",
                verification_payload(
                    serde_json::json!({
                        "status": "cancelled",
                        "verified_worker": worker_spec.name,
                    }),
                    provenance,
                ),
            )
            .await?;
            Ok(VerificationOutcome::Cancelled)
        }
        Ok(LoopOutcome::Deferred(wait)) => {
            engine.update_agent_status(&verifier.id, "failed").await?;
            let summary = format!(
                "Verifier cannot wait on external gate {}; a verdict is required",
                wait.gate_id
            );
            emit(
                engine,
                &session.id,
                &verifier_spec,
                "agent.finished",
                verification_payload(
                    serde_json::json!({
                        "status": "failed",
                        "approved": false,
                        "summary": truncate(&summary),
                        "verified_worker": worker_spec.name,
                    }),
                    provenance,
                ),
            )
            .await?;
            Ok(VerificationOutcome::Error(summary))
        }
        Ok(LoopOutcome::BudgetExceeded(stop)) => {
            engine.update_agent_status(&verifier.id, "failed").await?;
            emit(
                engine,
                &session.id,
                &verifier_spec,
                "agent.finished",
                verification_payload(
                    serde_json::json!({
                        "status": "failed",
                        "summary": stop.summary(),
                        "verified_worker": worker_spec.name,
                    }),
                    provenance,
                ),
            )
            .await?;
            Ok(VerificationOutcome::BudgetExceeded(stop))
        }
        Err(error) => {
            engine.update_agent_status(&verifier.id, "failed").await?;
            emit(
                engine,
                &session.id,
                &verifier_spec,
                "agent.finished",
                verification_payload(
                    serde_json::json!({
                        "status": "failed",
                        "approved": false,
                        "summary": truncate(error.to_string().as_str()),
                        "verified_worker": worker_spec.name,
                    }),
                    provenance,
                ),
            )
            .await?;
            Ok(VerificationOutcome::Error(error.to_string()))
        }
    }
}

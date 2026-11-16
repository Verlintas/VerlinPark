use super::*;

impl AgentService {
    pub fn from_env(engine: Engine) -> Self {
        let base_url = env::var("VERLINPARK_BASE_URL")
            .ok()
            .or_else(|| env::var("OPENAI_BASE_URL").ok());
        let api_key = env::var("VERLINPARK_API_KEY")
            .ok()
            .or_else(|| env::var("OPENAI_API_KEY").ok());
        let model = env::var("VERLINPARK_MODEL")
            .ok()
            .or_else(|| env::var("OPENAI_MODEL").ok());

        if base_url.is_none() && api_key.is_none() {
            return Self::unconfigured(engine);
        }

        let base_url = base_url.unwrap_or_else(|| "https://api.openai.com/v1".to_string());
        let model = model.unwrap_or_else(|| "gpt-4o-mini".to_string());
        let provider = Arc::new(OpenAiCompatibleProvider::new(
            "openai-compatible",
            base_url.clone(),
            api_key,
        ));
        let scope_store = engine.store().clone();
        Self::configured(
            engine,
            AgentConfig {
                provider,
                provider_name: "openai-compatible".to_string(),
                model,
                role_models: RoleModels::default(),
                base_url: Some(base_url),
                max_turns: env::var("VERLINPARK_MAX_TURNS")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(20),
                max_tokens: env::var("VERLINPARK_MAX_TOKENS")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .or(Some(4096)),
                max_managers: env::var("VERLINPARK_MAX_MANAGERS")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(5),
                max_workers: env::var("VERLINPARK_MAX_WORKERS")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(10),
                max_labour: env::var("VERLINPARK_MAX_LABOUR")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(40),
                auto_expand_workers: auto_expand_workers(),
                integration_command: env::var("VERLINPARK_INTEGRATION_COMMAND")
                    .ok()
                    .filter(|command| !command.trim().is_empty()),
                tools: ToolRegistry::default_tools(),
                max_active_sessions: global_session_cap(),
                scope_locks: Arc::new(ScopeLocks::with_store(scope_store)),
                capacity: Arc::new(CapacityController::new(
                    global_worker_cap(),
                    global_labour_cap(),
                )),
                provider_idle_timeout_secs: env::var("VERLINPARK_PROVIDER_IDLE_SECS")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(300)
                    .clamp(10, 3600),
                question_timeout_secs: env::var("VERLINPARK_QUESTION_TIMEOUT_SECS")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(1800)
                    .clamp(60, 86400),
                approval_mode: env::var("VERLINPARK_APPROVAL_MODE")
                    .ok()
                    .and_then(|value| ApprovalMode::parse(&value))
                    .unwrap_or_default(),
                approval_timeout_secs: env::var("VERLINPARK_APPROVAL_TIMEOUT_SECS")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(600)
                    .clamp(10, 86400),
                compact_after_tokens: compact_after_tokens(),
                compact_keep_messages: compact_keep_messages(),
                compact_keep_tokens: compact_keep_tokens(),
                compact_prepare_ratio: compact_prepare_ratio(),
                context_window: context_window(),
            },
        )
    }

    pub fn unconfigured(engine: Engine) -> Self {
        Self {
            engine,
            config: Arc::new(RwLock::new(None)),
            runs: Arc::new(Mutex::new(HashMap::new())),
            auto_resolver_started: Arc::new(AtomicBool::new(false)),
            task_scheduler_started: Arc::new(AtomicBool::new(false)),
            scaling_started: Arc::new(AtomicBool::new(false)),
            global_slots: Arc::new(AdmissionGate::new(global_session_cap())),
            capacity: Arc::new(CapacityController::new(
                global_worker_cap(),
                global_labour_cap(),
            )),
        }
    }

    pub fn configured(engine: Engine, config: AgentConfig) -> Self {
        let global_cap = config.max_active_sessions.max(1);
        let capacity = config.capacity.clone();
        Self {
            engine,
            config: Arc::new(RwLock::new(Some(config))),
            runs: Arc::new(Mutex::new(HashMap::new())),
            auto_resolver_started: Arc::new(AtomicBool::new(false)),
            task_scheduler_started: Arc::new(AtomicBool::new(false)),
            scaling_started: Arc::new(AtomicBool::new(false)),
            global_slots: Arc::new(AdmissionGate::new(global_cap)),
            capacity,
        }
    }

    pub fn configure(&self, params: ConfigureAgentParams) -> Result<AgentInfo> {
        let model = params.model.trim().to_string();
        if model.is_empty() {
            return Err(anyhow!("model is required"));
        }
        let base_url = params
            .base_url
            .map(|url| url.trim().trim_end_matches('/').to_string())
            .filter(|url| !url.is_empty())
            .unwrap_or_else(|| match params.provider.as_str() {
                "anthropic" => "https://api.anthropic.com/v1".to_string(),
                _ => "https://api.openai.com/v1".to_string(),
            });
        let provider: Arc<dyn Provider> = match params.provider.as_str() {
            "openai-compatible" => Arc::new(OpenAiCompatibleProvider::new(
                "openai-compatible",
                base_url.clone(),
                params.api_key,
            )),
            "anthropic" => Arc::new(AnthropicProvider::with_base_url(
                "anthropic",
                base_url.clone(),
                params.api_key,
            )),
            other => return Err(anyhow!("unsupported provider: {other}")),
        };
        let scope_locks = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().map(|config| config.scope_locks.clone()))
            .unwrap_or_else(|| Arc::new(ScopeLocks::with_store(self.engine.store().clone())));
        let config = AgentConfig {
            provider,
            provider_name: params.provider,
            model,
            role_models: params.role_models.unwrap_or_default(),
            base_url: Some(base_url),
            max_turns: params.max_turns.unwrap_or(20).clamp(1, 200),
            max_tokens: params.max_tokens.or(Some(4096)),
            max_managers: params.max_managers.unwrap_or(5).clamp(1, 5) as usize,
            max_workers: params.max_workers.unwrap_or(10).clamp(1, 10) as usize,
            max_labour: params.max_labour.unwrap_or(40).clamp(1, 40) as usize,
            auto_expand_workers: params.auto_expand_workers.unwrap_or(false),
            integration_command: env::var("VERLINPARK_INTEGRATION_COMMAND")
                .ok()
                .filter(|command| !command.trim().is_empty()),
            tools: ToolRegistry::default_tools(),
            max_active_sessions: global_session_cap(),
            scope_locks,
            capacity: self.capacity.clone(),
            provider_idle_timeout_secs: env::var("VERLINPARK_PROVIDER_IDLE_SECS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(300)
                .clamp(10, 3600),
            question_timeout_secs: env::var("VERLINPARK_QUESTION_TIMEOUT_SECS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(1800)
                .clamp(60, 86400),
            approval_mode: params
                .approval_mode
                .or_else(|| {
                    env::var("VERLINPARK_APPROVAL_MODE")
                        .ok()
                        .and_then(|value| ApprovalMode::parse(&value))
                })
                .unwrap_or_default(),
            approval_timeout_secs: env::var("VERLINPARK_APPROVAL_TIMEOUT_SECS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(600)
                .clamp(10, 86400),
            compact_after_tokens: compact_after_tokens(),
            compact_keep_messages: compact_keep_messages(),
            compact_keep_tokens: compact_keep_tokens(),
            compact_prepare_ratio: compact_prepare_ratio(),
            context_window: params
                .context_window
                .map(|value| value.min(10_000_000))
                .unwrap_or_else(context_window),
        };
        *self
            .config
            .write()
            .map_err(|_| anyhow!("agent config lock poisoned"))? = Some(config);
        Ok(self.info())
    }

    pub fn info(&self) -> AgentInfo {
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned());
        match config {
            Some(config) => AgentInfo {
                configured: true,
                provider: Some(config.provider_name.clone()),
                model: Some(config.model.clone()),
                base_url: config.base_url.clone(),
                role_models: config.role_models.clone(),
                capacity: self.capacity.snapshot(),
                context_window: Some(config.context_window),
                approval_mode: config.approval_mode,
                tool_bundles: tool_bundle_catalog_rows(),
            },
            None => AgentInfo {
                configured: false,
                provider: None,
                model: None,
                base_url: None,
                role_models: RoleModels::default(),
                capacity: self.capacity.snapshot(),
                context_window: None,
                approval_mode: ApprovalMode::default(),
                tool_bundles: tool_bundle_catalog_rows(),
            },
        }
    }

    /// Static tool-bundle catalog for Settings and `vp tool-bundles`. It reads
    /// no session state and never calls a provider.
    pub fn tool_bundle_catalog(&self) -> ToolBundleCatalogResult {
        ToolBundleCatalogResult {
            bundles: tool_bundle_catalog_rows(),
            warnings: tool_bundle_warnings(),
        }
    }

    /// Evaluate the real bundle matrix against the configured tool registry.
    /// This is a read-only planning view: no session, worktree, provider call,
    /// or event is created. The row set is bounded to the standard roles and
    /// at most eight requested task kinds.
    pub fn tool_bundle_matrix(&self, task_kinds: &[String]) -> Result<ToolBundleMatrixResult> {
        let config = self.configured_agent()?;
        let mut kinds = if task_kinds.is_empty() {
            vec![
                String::new(),
                "implementation".to_string(),
                "verification".to_string(),
                "research".to_string(),
                "trivial".to_string(),
                "unknown-task-kind".to_string(),
            ]
        } else {
            task_kinds
                .iter()
                .map(|kind| kind.trim().to_string())
                .filter(|kind| kind.len() <= 80)
                .take(8)
                .collect::<Vec<_>>()
        };
        if kinds.is_empty() {
            kinds.push(String::new());
        }
        let combinations = [
            (AgentRole::Leader, AgentProfile::Generic),
            (AgentRole::Leader, AgentProfile::Coding),
            (AgentRole::Manager, AgentProfile::Coding),
            (AgentRole::Worker, AgentProfile::Coding),
            (AgentRole::Worker, AgentProfile::Research),
            (AgentRole::Worker, AgentProfile::Verification),
            (AgentRole::Worker, AgentProfile::Generic),
            (AgentRole::Labour, AgentProfile::Coding),
            (AgentRole::Verifier, AgentProfile::Verification),
            (AgentRole::Scout, AgentProfile::Research),
        ];
        let no_mcp: Vec<McpToolBinding> = Vec::new();
        let mut rows = Vec::with_capacity(combinations.len() * kinds.len());
        for (role, profile) in combinations {
            for kind in &kinds {
                let task_kind = (!kind.is_empty()).then(|| kind.clone());
                let spec = preview_spec(role, profile, task_kind);
                let frozen = frozen_tool_bundle(&config, &spec, &no_mcp);
                let builtin_count = frozen.builtin_count();
                let mcp_count = frozen.mcp_count();
                let total_count = frozen.tool_count();
                let schema_bytes = frozen.schema_bytes();
                let estimated_tool_tokens = frozen.estimated_tokens();
                let requested_count = if frozen.selection.kind.is_partitioned() {
                    frozen.requested.len()
                } else {
                    builtin_count
                };
                let omitted_count = frozen.omitted.len();
                let within_target = frozen.within_target();
                let warning_count = frozen.warnings.len();
                rows.push(ToolBundleMatrixRow {
                    role: role.as_str().to_string(),
                    profile: profile.as_str().to_string(),
                    task_kind: frozen.selection.task_kind.clone(),
                    bundle: frozen.selection.kind.as_str().to_string(),
                    reason: frozen.selection.reason.to_string(),
                    fallback: frozen.selection.fallback,
                    bundle_hash: frozen.bundle_hash,
                    tools_hash: frozen.tools_hash,
                    builtin_count,
                    mcp_count,
                    total_count,
                    schema_bytes,
                    estimated_tool_tokens,
                    requested_count,
                    omitted_count,
                    target_limit: frozen.target_limit,
                    within_target,
                    warning_count,
                });
            }
        }
        Ok(ToolBundleMatrixResult {
            task_kinds: kinds,
            rows,
            warnings: tool_bundle_warnings(),
        })
    }

    pub async fn probe(&self) -> Result<AgentProbeResult> {
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned())
            .ok_or_else(|| anyhow!("agent provider is not configured"))?;
        let started = std::time::Instant::now();
        // Reasoning models can spend hundreds of tokens before emitting their
        // first visible content token. Keep the probe small but large enough to
        // distinguish a healthy reasoning endpoint from a dead one.
        let probe_tokens = config.max_tokens.unwrap_or(1024).clamp(1024, 2048);
        let request = ChatRequest {
            model: config.model.clone(),
            messages: vec![ChatMessage::user("Reply with the single word: OK")],
            system: Some("You are a connection probe. Reply concisely.".to_string()),
            system_stable_prefix: None,
            system_sections: Vec::new(),
            tail_sections: Vec::new(),
            cache: None,
            max_tokens: Some(probe_tokens),
            temperature: Some(0.0),
            tools: Vec::new(),
        };
        let mut stream = timeout(
            Duration::from_secs(60),
            config.provider.stream_chat(request),
        )
        .await
        .map_err(|_| anyhow!("provider probe timed out"))?
        .context("starting provider probe")?;
        let mut text = String::new();
        let mut usage = (0u64, 0u64);
        let mut chunks = 0u32;
        let mut first_token_ms = 0u64;
        let mut done = false;
        while let Some(item) = timeout(Duration::from_secs(60), stream.next())
            .await
            .map_err(|_| anyhow!("provider probe stream timed out"))?
        {
            match item? {
                StreamEvent::TextDelta(delta) => {
                    if !delta.is_empty() {
                        if chunks == 0 {
                            first_token_ms =
                                started.elapsed().as_millis().min(u64::MAX as u128) as u64;
                        }
                        chunks = chunks.saturating_add(1);
                        text.push_str(&delta);
                    }
                }
                StreamEvent::Usage {
                    input_tokens,
                    output_tokens,
                    ..
                } => {
                    usage.0 = usage.0.saturating_add(input_tokens as u64);
                    usage.1 = usage.1.saturating_add(output_tokens as u64);
                }
                StreamEvent::Done => done = true,
                StreamEvent::ToolCall(_) => {}
            }
        }
        if text.trim().is_empty() {
            anyhow::bail!("provider probe returned no text");
        }
        Ok(AgentProbeResult {
            provider: config.provider_name,
            model: config.model,
            latency_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
            first_token_ms,
            chunks,
            done,
            input_tokens: usage.0,
            output_tokens: usage.1,
            text: truncate(&text),
        })
    }

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

    pub(super) async fn integrate_remote_inner(
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
    pub(super) async fn prune_remote_refs_after_merge(
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

    pub(super) async fn cleanup_remote_import(
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

    pub(super) async fn retry_merge_inner(&self, entry_id: &str) -> Result<MergeQueueEntry> {
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

    pub(super) async fn resolve_merge_with_limit(
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

    pub(super) async fn resolve_merge_inner(
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

    /// Start the daemon-side watcher that auto-resolves conflicts when a
    /// project opts in. Safe to call more than once.
    pub fn start_auto_resolver(&self) {
        if self.auto_resolver_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let service = self.clone();
        tokio::spawn(async move {
            let mut events = service.engine.subscribe();
            service.scan_auto_resolve().await;
            loop {
                match events.recv().await {
                    Ok(event)
                        if event.kind == "session.updated" && event.payload["status"] == "idle" =>
                    {
                        service.auto_resolve_session(&event.session_id).await;
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    /// Deterministic project-program controller. This reconciles completed,
    /// failed, retrying, and dependency-blocked milestones even when no
    /// provider is configured, then launches at most one ready milestone per
    /// program slot through the ordinary session/plan path.
    pub async fn reconcile_programs(&self, project_id: Option<&str>) -> Result<ProgramTickResult> {
        let controller = self.engine.programs();
        let result = controller.reconcile(project_id).await?;
        let mut started = 0usize;
        if self.info().configured {
            let candidates = controller.ready_candidates_in_project(project_id, None, 10)?;
            for candidate in candidates {
                if !candidate.program.policy.auto_dispatch {
                    continue;
                }
                match self.start_program_milestone(&candidate.milestone.id).await {
                    Ok(_) => started += 1,
                    Err(error) => {
                        tracing::warn!(
                            %error,
                            program_id = %candidate.program.id,
                            milestone_id = %candidate.milestone.id,
                            "program controller could not start a ready milestone"
                        );
                    }
                }
            }
        }
        Ok(ProgramTickResult {
            programs: result.programs,
            milestones_advanced: result.milestones_advanced,
            sessions_started: started,
            completed: result.completed,
            failed: result.failed,
            ready: Vec::new(),
        })
    }

    /// Cooperatively cancel a whole program. Running sessions receive the
    /// normal cancellation signal; the ledger rows become cancelled and the
    /// program becomes failed with an explicit operator reason. This never
    /// marks partial work as completed.
    pub async fn cancel_program(&self, program_id: &str, reason: &str) -> Result<Program> {
        let reason = reason.trim();
        if reason.is_empty() || reason.chars().count() > 1_000 {
            anyhow::bail!("program cancellation reason must be 1-1000 characters");
        }
        let store = self.engine.store().clone();
        let program_id_owned = program_id.to_string();
        let Some(program) = tokio::task::spawn_blocking({
            let store = store.clone();
            let program_id = program_id_owned.clone();
            move || store.get_program(&program_id)
        })
        .await??
        else {
            anyhow::bail!("program {program_id} was not found");
        };
        if program.status.is_terminal() {
            anyhow::bail!("program {program_id} is already terminal");
        }
        if program.archived {
            anyhow::bail!("program {program_id} is an imported archive");
        }
        let milestones = tokio::task::spawn_blocking({
            let store = store.clone();
            let program_id = program_id_owned.clone();
            move || store.list_program_milestones(&program_id)
        })
        .await??;
        let ledger_reason = format!("program cancelled: {reason}");
        for milestone in milestones {
            if milestone.status.is_terminal() {
                continue;
            }
            if let Some(session_id) = milestone.session_id.as_deref() {
                let _ = self.cancel(session_id);
            }
            let store = store.clone();
            let milestone_id = milestone.id.clone();
            let ledger_reason = ledger_reason.clone();
            let _ = tokio::task::spawn_blocking(move || {
                store.cancel_program_milestone(&milestone_id, &ledger_reason, false)
            })
            .await?;
        }
        let store = self.engine.store().clone();
        let program_id_owned = program_id.to_string();
        let program_reason = format!("cancelled by operator: {reason}");
        let program = tokio::task::spawn_blocking(move || {
            store.fail_program(&program_id_owned, &program_reason)
        })
        .await??
        .map(|(program, _event)| program)
        .ok_or_else(|| anyhow!("program {program_id} could not be cancelled"))?;
        self.engine.record_project_manifest().await?;
        Ok(program)
    }

    /// Start one milestone through the normal session path. The milestone is
    /// claimed before the session is created, so a concurrent tick cannot
    /// create a duplicate worktree or plan. All gates remain in force.
    pub async fn start_program_milestone(&self, milestone_id: &str) -> Result<ProgramMilestone> {
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned())
            .ok_or_else(|| anyhow!("agent provider is not configured"))?;
        let controller = self.engine.programs();
        let Some(milestone) = controller.claim_start(milestone_id)? else {
            anyhow::bail!("milestone {milestone_id} is not runnable");
        };
        let store = self.engine.store().clone();
        let program_id = milestone.program_id.clone();
        let Some(program) = tokio::task::spawn_blocking({
            let store = store.clone();
            let program_id = program_id.clone();
            move || store.get_program(&program_id)
        })
        .await??
        else {
            anyhow::bail!("program {program_id} disappeared after the milestone was claimed");
        };
        let (token_budget, cost_budget_usd) =
            match controller.budget_for_start(&program, &milestone) {
                Ok(budget) => (budget.token_budget, budget.cost_budget_usd),
                Err(error) => {
                    controller.fail_start(&program, &milestone, &format!("{error:#}"))?;
                    return Err(error);
                }
            };
        let plan = crate::program::milestone_plan_with_budget(
            &program,
            &milestone,
            token_budget,
            cost_budget_usd,
        );
        if let Err(error) = validate_team_plan(&plan, config.max_managers) {
            controller.fail_start(
                &program,
                &milestone,
                &format!("program milestone plan validation failed: {error:#}"),
            )?;
            return Err(error);
        }
        let goal = crate::program::milestone_session_goal(&program, &milestone);
        let priority = milestone.priority;
        let session = match self
            .engine
            .create_session_with_options(
                program.project_id.clone(),
                goal,
                token_budget,
                cost_budget_usd,
                None,
                priority,
            )
            .await
        {
            Ok(session) => session,
            Err(error) => {
                controller.fail_start(
                    &program,
                    &milestone,
                    &format!("could not create a program session: {error:#}"),
                )?;
                return Err(error);
            }
        };
        let setup = async {
            validate_team_plan(&plan, config.max_managers)?;
            self.engine.set_team_plan(&session.id, &plan).await?;
            controller.attach_session(&milestone.id, &session.id, 1)?;
            self.engine
                .append_event(
                    &session.id,
                    "program.milestone.dispatched",
                    serde_json::json!({
                        "program_id": program.id,
                        "milestone_id": milestone.id,
                        "milestone_key": milestone.key,
                        "attempt": milestone.attempts,
                    }),
                )
                .await?;
            Ok::<(), anyhow::Error>(())
        }
        .await;
        if let Err(error) = setup {
            let _ = self
                .engine
                .update_session_status(&session.id, "failed")
                .await;
            let _ = self
                .engine
                .append_event(
                    &session.id,
                    "program.milestone.dispatch_failed",
                    serde_json::json!({
                        "program_id": program.id,
                        "milestone_id": milestone.id,
                        "error": format!("{error:#}"),
                    }),
                )
                .await;
            controller.fail_start(
                &program,
                &milestone,
                &format!("could not prepare the milestone plan: {error:#}"),
            )?;
            return Err(error);
        }
        if let Err(error) = self.resume_plan(&session.id).await {
            let _ = self
                .engine
                .update_session_status(&session.id, "failed")
                .await;
            let _ = self
                .engine
                .append_event(
                    &session.id,
                    "program.milestone.dispatch_failed",
                    serde_json::json!({
                        "program_id": program.id,
                        "milestone_id": milestone.id,
                        "error": format!("{error:#}"),
                    }),
                )
                .await;
            controller.fail_start(
                &program,
                &milestone,
                &format!("could not start the milestone run: {error:#}"),
            )?;
            return Err(error);
        }
        let store = self.engine.store().clone();
        let milestone_id = milestone.id.clone();
        Ok(
            tokio::task::spawn_blocking(move || store.get_program_milestone(&milestone_id))
                .await??
                .unwrap_or(milestone),
        )
    }

    /// Dispatch a standalone intake item immediately. Manual dispatch is a
    /// deliberate user action, so it bypasses the project's schedule toggle
    /// and parallel cap but still atomically claims the row and uses the same
    /// one-task plan, Verifier, and Integrator path as every other Worker.
    pub async fn dispatch_intake_task(&self, id: &str) -> Result<IntakeDispatchResult> {
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned())
            .ok_or_else(|| anyhow!("agent provider is not configured"))?;
        let Some(mut task) = self.engine.get_intake_task(id.to_string()).await? else {
            anyhow::bail!("intake task not found: {id}");
        };
        if !task.deps.is_empty() {
            let peers = self
                .engine
                .list_intake_tasks(Some(task.project_id.clone()), None, None)
                .await?
                .tasks;
            for dep in &task.deps {
                match peers.iter().find(|row| &row.id == dep) {
                    Some(row) if matches!(row.status.as_str(), "completed" | "skipped") => {}
                    Some(row) => anyhow::bail!(
                        "intake task {id} is waiting for dependency {} ({})",
                        row.title,
                        row.status
                    ),
                    None => anyhow::bail!("intake task {id} has unknown dependency {dep}"),
                }
            }
        }
        if !matches!(
            task.status.as_str(),
            "queued" | "failed" | "interrupted" | "cancelled"
        ) {
            anyhow::bail!("intake task {id} is not runnable (status: {})", task.status);
        }
        if task.attempts >= task.max_attempts
            && matches!(task.status.as_str(), "failed" | "interrupted" | "cancelled")
        {
            task = self
                .engine
                .retry_intake_task(task.id.clone())
                .await?
                .ok_or_else(|| anyhow!("intake task {id} could not be reset for a manual run"))?;
        }
        let Some(claimed) = self.engine.claim_intake_task(task.id.clone()).await? else {
            anyhow::bail!("intake task {id} was claimed by another dispatcher");
        };
        let plan = self.engine.intake_plan(&claimed);
        if let Err(error) = validate_team_plan(&plan, config.max_managers) {
            let next = self.intake_retry_at(&claimed).await;
            let _ = self
                .engine
                .settle_intake_task(
                    claimed.id.clone(),
                    "failed".to_string(),
                    None,
                    Some(format!("plan validation failed: {error:#}")),
                    next,
                )
                .await;
            return Err(error);
        }
        let goal = format!(
            "{} · {}",
            claimed.title,
            claimed
                .instructions
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("")
                .trim()
                .chars()
                .take(180)
                .collect::<String>()
        );
        let session = match self
            .engine
            .create_session_with_options(
                claimed.project_id.clone(),
                goal,
                claimed.token_budget,
                claimed.cost_budget_usd,
                claimed.time_budget_secs,
                claimed.priority,
            )
            .await
        {
            Ok(session) => session,
            Err(error) => {
                let next = self.intake_retry_at(&claimed).await;
                self.engine
                    .settle_intake_task(
                        claimed.id.clone(),
                        "failed".to_string(),
                        None,
                        Some(format!("could not create session: {error:#}")),
                        next,
                    )
                    .await?;
                return Err(error);
            }
        };
        let setup = async {
            validate_team_plan(&plan, config.max_managers)?;
            self.engine.set_team_plan(&session.id, &plan).await?;
            self.engine
                .start_intake_task(claimed.id.clone(), session.id.clone(), "T1".to_string())
                .await?;
            self.engine
                .append_event(
                    &session.id,
                    "intake.dispatched",
                    serde_json::json!({
                        "intake_task_id": claimed.id,
                        "title": claimed.title,
                        "profile": claimed.profile,
                        "priority": claimed.priority,
                        "attempt": claimed.attempts,
                    }),
                )
                .await?;
            Ok::<(), anyhow::Error>(())
        }
        .await;
        if let Err(error) = setup {
            let next = self.intake_retry_at(&claimed).await;
            let _ = self
                .engine
                .update_session_status(&session.id, "failed")
                .await;
            let _ = self
                .engine
                .append_event(
                    &session.id,
                    "intake.dispatch_failed",
                    serde_json::json!({
                        "intake_task_id": claimed.id,
                        "error": format!("{error:#}"),
                    }),
                )
                .await;
            let _ = self
                .engine
                .settle_intake_task(
                    claimed.id.clone(),
                    "failed".to_string(),
                    None,
                    Some(format!("could not prepare the task plan: {error:#}")),
                    next,
                )
                .await;
            return Err(error);
        }
        if let Err(error) = self.resume_plan(&session.id).await {
            let next = self.intake_retry_at(&claimed).await;
            let _ = self
                .engine
                .update_session_status(&session.id, "failed")
                .await;
            let _ = self
                .engine
                .append_event(
                    &session.id,
                    "intake.dispatch_failed",
                    serde_json::json!({
                        "intake_task_id": claimed.id,
                        "error": format!("{error:#}"),
                    }),
                )
                .await;
            let updated = self
                .engine
                .settle_intake_task(
                    claimed.id.clone(),
                    "failed".to_string(),
                    None,
                    Some(format!("could not start the task run: {error:#}")),
                    next,
                )
                .await?;
            return Ok(IntakeDispatchResult {
                started: false,
                task: updated.unwrap_or(claimed),
                session_id: Some(session.id),
            });
        }
        let updated = self.engine.get_intake_task(claimed.id.clone()).await?;
        Ok(IntakeDispatchResult {
            started: true,
            task: updated.unwrap_or(claimed),
            session_id: Some(session.id),
        })
    }

    /// Settle intake rows whose session reached a terminal plan status. This
    /// runs on every scheduler tick, so a daemon crash or a provider failure
    /// never leaves an intake item stuck in `running`.
    pub async fn reconcile_intake_tasks(&self) -> Result<u32> {
        let dependency_blocked = self.reconcile_intake_dependencies().await?;
        let tasks = self
            .engine
            .list_intake_tasks(None, Some("running".to_string()), Some(1_000))
            .await?
            .tasks;
        let mut settled = dependency_blocked;
        for task in tasks {
            let Some(session_id) = task.session_id.clone() else {
                let next = self.intake_retry_at(&task).await;
                let _ = self
                    .engine
                    .settle_intake_task(
                        task.id.clone(),
                        "failed".to_string(),
                        None,
                        Some("session id was not recorded".to_string()),
                        next,
                    )
                    .await;
                settled += 1;
                continue;
            };
            let (session, _) =
                match load_session_project(self.engine.store().clone(), &session_id).await {
                    Ok(value) => value,
                    Err(_) => {
                        let next = self.intake_retry_at(&task).await;
                        let _ = self
                            .engine
                            .settle_intake_task(
                                task.id.clone(),
                                "failed".to_string(),
                                None,
                                Some("the linked session no longer exists".to_string()),
                                next,
                            )
                            .await;
                        settled += 1;
                        continue;
                    }
                };
            let queue = self
                .engine
                .task_queue(None, Some(session.id.clone()))
                .await?;
            let row = queue
                .tasks
                .iter()
                .find(|row| row.task_id == task.task_id.clone().unwrap_or_default());
            let summary = latest_plan_results(&self.engine, &session.id)
                .await
                .ok()
                .and_then(|results| {
                    task.task_id
                        .as_ref()
                        .and_then(|task_id| results.get(task_id).cloned())
                });
            let terminal: Option<(&str, Option<String>, Option<String>)> =
                match row.map(|row| row.status.as_str()) {
                    Some("completed") | Some("skipped") => Some((
                        "completed",
                        Some(summary.unwrap_or_else(|| "completed".into())),
                        None,
                    )),
                    Some("failed") | Some("blocked") | Some("interrupted") => {
                        let detail = row.and_then(|row| row.last_error.clone());
                        Some((
                            "failed",
                            detail
                                .clone()
                                .or_else(|| summary.clone())
                                .or(Some("the worker did not complete".into())),
                            Some(detail.unwrap_or_else(|| "the worker did not complete".into())),
                        ))
                    }
                    Some("cancelled") => Some(("cancelled", summary, None)),
                    Some("pending") | Some("running") => match session.status.as_str() {
                        "idle" | "draft" | "landed" | "imported" => Some((
                            "failed",
                            Some("the session ended before the task settled".into()),
                            Some("the session ended before the task settled".into()),
                        )),
                        _ => None,
                    },
                    _ => match session.status.as_str() {
                        "failed" => Some((
                            "failed",
                            Some("the session failed before the task settled".into()),
                            Some("the session failed before the task settled".into()),
                        )),
                        "cancelled" => Some(("cancelled", summary, None)),
                        "idle" | "draft" | "landed" | "imported" => Some((
                            "failed",
                            Some("the plan task was not found".into()),
                            Some("the plan task was not found".into()),
                        )),
                        _ => None,
                    },
                };
            let Some((status, result_summary, last_error)) = terminal else {
                continue;
            };
            let next = if status == "failed" {
                self.intake_retry_at(&task).await
            } else {
                None
            };
            let blocked_next = next.clone();
            let _ = self
                .engine
                .settle_intake_task(
                    task.id.clone(),
                    status.to_string(),
                    result_summary,
                    last_error,
                    next,
                )
                .await?;
            if status == "cancelled" || (status == "failed" && blocked_next.is_none()) {
                self.block_intake_dependents(
                    &task.project_id,
                    &task.id,
                    &format!("dependency \"{}\" is {status}", task.title),
                )
                .await;
            }
            let kind = match status {
                "completed" => "intake.completed",
                "cancelled" => "intake.cancelled",
                _ => "intake.failed",
            };
            let _ = self
                .engine
                .append_event(
                    &session.id,
                    kind,
                    serde_json::json!({
                        "intake_task_id": task.id,
                        "task_id": task.task_id,
                        "status": status,
                        "attempt": task.attempts,
                    }),
                )
                .await;
            settled += 1;
        }
        Ok(settled)
    }

    /// Read-only scheduler observability for the dashboard and CLI. Counts
    /// are derived from the persisted projections; this never changes state.
    pub async fn scheduler_status(
        &self,
        project_id: Option<String>,
    ) -> Result<SchedulerStatusResult> {
        let sessions = self.engine.list_sessions(project_id.clone()).await?;
        let tasks = self
            .engine
            .task_queue(project_id.clone(), None)
            .await?
            .tasks;
        let intake = self
            .engine
            .list_intake_tasks(project_id.clone(), None, Some(2_000))
            .await?
            .tasks;
        let all_projects = self.engine.list_projects().await?;
        let projects = all_projects
            .into_iter()
            .filter(|project| project_id.as_deref().is_none_or(|id| id == project.id))
            .collect::<Vec<_>>();

        let session_is_runnable =
            |session: &Session| matches!(session.status.as_str(), "idle" | "failed" | "cancelled");
        let task_is_retryable =
            |status: &str| matches!(status, "pending" | "interrupted" | "failed" | "cancelled");
        let task_is_running =
            |status: &str| matches!(status, "running" | "claimed" | "pausing" | "paused");
        let mut pending_sessions = std::collections::HashSet::new();
        for task in &tasks {
            if !task_is_retryable(&task.status) {
                continue;
            }
            if sessions
                .iter()
                .find(|session| session.id == task.session_id)
                .is_some_and(session_is_runnable)
            {
                pending_sessions.insert(task.session_id.clone());
            }
        }
        let active_sessions = sessions
            .iter()
            .filter(|session| {
                matches!(
                    session.status.as_str(),
                    "running" | "paused" | "waiting_input"
                )
            })
            .count();
        let queued_sessions = sessions
            .iter()
            .filter(|session| session.status == "queued")
            .count();
        let paused_sessions = sessions
            .iter()
            .filter(|session| session.status == "paused")
            .count();
        let pending_plan_tasks = tasks
            .iter()
            .filter(|task| task_is_retryable(&task.status))
            .count();
        let running_plan_tasks = tasks
            .iter()
            .filter(|task| task_is_running(&task.status))
            .count();
        let queued_intake = intake.iter().filter(|task| task.status == "queued").count();
        let active_intake = intake
            .iter()
            .filter(|task| task.status == "running" || task.status == "dispatching")
            .count();
        let failed_intake = intake
            .iter()
            .filter(|task| task.status == "failed" || task.status == "interrupted")
            .count();

        let mut project_statuses = Vec::new();
        for project in projects {
            let policy = self
                .engine
                .get_task_scheduler_policy(&project.id)
                .await?
                .policy;
            let project_tasks = tasks
                .iter()
                .filter(|task| task.project_id == project.id)
                .collect::<Vec<_>>();
            let project_intake = intake
                .iter()
                .filter(|task| task.project_id == project.id)
                .collect::<Vec<_>>();
            let mut pending = std::collections::HashSet::new();
            for task in &project_tasks {
                if task_is_retryable(&task.status)
                    && sessions
                        .iter()
                        .find(|session| session.id == task.session_id)
                        .is_some_and(session_is_runnable)
                {
                    pending.insert(task.session_id.clone());
                }
            }
            project_statuses.push(SchedulerProjectStatus {
                project_id: project.id.clone(),
                project_name: project.name.clone(),
                enabled: policy.enabled,
                cooldown_secs: policy.cooldown_secs,
                dispatch_tasks: policy.dispatch_tasks,
                include_intake: policy.include_intake,
                max_intake_parallel: policy.max_intake_parallel,
                pending_sessions: pending.len(),
                pending_plan_tasks: project_tasks
                    .iter()
                    .filter(|task| task_is_retryable(&task.status))
                    .count(),
                running_plan_tasks: project_tasks
                    .iter()
                    .filter(|task| task_is_running(&task.status))
                    .count(),
                queued_intake: project_intake
                    .iter()
                    .filter(|task| task.status == "queued")
                    .count(),
                active_intake: project_intake
                    .iter()
                    .filter(|task| task.status == "running" || task.status == "dispatching")
                    .count(),
                failed_intake: project_intake
                    .iter()
                    .filter(|task| task.status == "failed" || task.status == "interrupted")
                    .count(),
            });
        }

        Ok(SchedulerStatusResult {
            active_sessions,
            queued_sessions,
            paused_sessions,
            pending_plan_tasks,
            running_plan_tasks,
            queued_intake,
            active_intake,
            failed_intake,
            global_session_cap: global_session_cap(),
            global_worker_cap: global_worker_cap(),
            global_labour_cap: global_labour_cap(),
            projects: project_statuses,
        })
    }

    /// Block queued dependents whose dependency can no longer make progress.
    /// This also repairs rows after a daemon restart, where recovery marked a
    /// running intake task failed without touching its dependents.
    pub(super) async fn reconcile_intake_dependencies(&self) -> Result<u32> {
        let tasks = self.engine.list_intake_tasks(None, None, None).await?.tasks;
        let by_id = tasks
            .iter()
            .map(|task| (task.id.clone(), task))
            .collect::<HashMap<_, _>>();
        let mut policies = HashMap::new();
        let mut blocked = 0;
        for task in tasks.iter().filter(|task| task.status == "queued") {
            if task.deps.is_empty() {
                continue;
            }
            if !policies.contains_key(&task.project_id) {
                let policy = self
                    .engine
                    .orchestration_policy(&task.project_id)
                    .await
                    .unwrap_or_default();
                policies.insert(task.project_id.clone(), policy);
            }
            let policy = policies.get(&task.project_id).cloned().unwrap_or_default();
            let mut waiting = false;
            let mut failure = None;
            for dep in &task.deps {
                let Some(dependency) = by_id.get(dep) else {
                    failure = Some(format!("dependency {dep} no longer exists"));
                    break;
                };
                match dependency.status.as_str() {
                    "completed" | "skipped" => {}
                    "failed" | "interrupted"
                        if dependency.next_attempt_at.is_some()
                            || (policy.auto_retry_failed
                                && dependency.attempts < dependency.max_attempts) =>
                    {
                        // A retry is scheduled or still available; keep
                        // waiting, but continue scanning for a permanent
                        // failure in a sibling dependency.
                        waiting = true;
                    }
                    "failed" | "interrupted" | "cancelled" | "blocked" => {
                        failure = Some(format!(
                            "dependency \"{}\" is {}",
                            dependency.title, dependency.status
                        ));
                        break;
                    }
                    _ => {
                        // Dispatching/running/queued dependency: wait.
                        waiting = true;
                    }
                }
            }
            if waiting && failure.is_none() {
                continue;
            }
            let Some(reason) = failure else {
                continue;
            };
            self.engine
                .settle_intake_task(
                    task.id.clone(),
                    "blocked".to_string(),
                    None,
                    Some(format!("blocked: {reason}")),
                    None,
                )
                .await?;
            blocked += 1;
        }
        Ok(blocked)
    }

    pub async fn cancel_intake_task(&self, id: &str) -> Result<IntakeTask> {
        let Some(task) = self.engine.get_intake_task(id.to_string()).await? else {
            anyhow::bail!("intake task not found: {id}");
        };
        match task.status.as_str() {
            "running" | "dispatching" => {
                if let Some(session_id) = task.session_id.as_deref() {
                    let _ = self.cancel(session_id);
                    let _ = self
                        .engine
                        .append_event(
                            session_id,
                            "intake.cancelled",
                            serde_json::json!({
                                "intake_task_id": task.id,
                                "reason": "cancelled by user",
                            }),
                        )
                        .await;
                }
                let cancelled = self
                    .engine
                    .settle_intake_task(
                        task.id.clone(),
                        "cancelled".to_string(),
                        None,
                        Some("cancelled by user".to_string()),
                        None,
                    )
                    .await?
                    .ok_or_else(|| anyhow!("intake task disappeared while cancelling"))?;
                self.block_intake_dependents(
                    &cancelled.project_id,
                    &cancelled.id,
                    &format!("dependency \"{}\" was cancelled", cancelled.title),
                )
                .await;
                Ok(cancelled)
            }
            "queued" | "failed" | "interrupted" | "blocked" => {
                let cancelled = self
                    .engine
                    .cancel_intake_task(id.to_string())
                    .await?
                    .ok_or_else(|| {
                        anyhow!("intake task {id} cannot be cancelled from {}", task.status)
                    })?;
                self.block_intake_dependents(
                    &cancelled.project_id,
                    &cancelled.id,
                    &format!("dependency \"{}\" was cancelled", cancelled.title),
                )
                .await;
                Ok(cancelled)
            }
            "completed" | "cancelled" => Ok(task),
            other => anyhow::bail!("intake task {id} cannot be cancelled from {other}"),
        }
    }

    pub async fn retry_intake_task(&self, id: &str) -> Result<IntakeTask> {
        self.engine
            .retry_intake_task(id.to_string())
            .await?
            .ok_or_else(|| {
                anyhow!("intake task {id} is not failed, interrupted, cancelled, or blocked")
            })
    }

    pub async fn delete_intake_task(&self, id: &str) -> Result<()> {
        let dependents = self
            .engine
            .list_intake_tasks(None, None, None)
            .await?
            .tasks
            .into_iter()
            .filter(|task| task.deps.iter().any(|dep| dep == id))
            .collect::<Vec<_>>();
        if let Some(dependent) = dependents
            .iter()
            .find(|task| !matches!(task.status.as_str(), "completed" | "cancelled"))
        {
            anyhow::bail!(
                "intake task {id} is still a dependency of \"{}\"",
                dependent.title
            );
        }
        if !self.engine.delete_intake_task(id.to_string()).await? {
            anyhow::bail!("intake task {id} is active or does not exist");
        }
        Ok(())
    }

    pub(super) async fn block_intake_dependents(
        &self,
        project_id: &str,
        dependency_id: &str,
        reason: &str,
    ) {
        let tasks = match self
            .engine
            .list_intake_tasks(Some(project_id.to_string()), None, None)
            .await
        {
            Ok(tasks) => tasks.tasks,
            Err(error) => {
                tracing::warn!(%error, project_id, "could not block intake dependents");
                return;
            }
        };
        for task in tasks {
            if task.status != "queued" || !task.deps.iter().any(|dep| dep == dependency_id) {
                continue;
            }
            let error = format!("blocked: {reason}");
            if let Err(error) = self
                .engine
                .settle_intake_task(
                    task.id.clone(),
                    "blocked".to_string(),
                    None,
                    Some(error.clone()),
                    None,
                )
                .await
            {
                tracing::warn!(%error, task_id = %task.id, "could not block dependent intake task");
            }
        }
    }

    pub(super) async fn intake_retry_at(&self, task: &IntakeTask) -> Option<String> {
        if task.attempts >= task.max_attempts {
            return None;
        }
        let policy = self
            .engine
            .orchestration_policy(&task.project_id)
            .await
            .ok()?;
        if !policy.auto_retry_failed {
            return None;
        }
        let delay = crate::orchestration::retry_delay(&policy, task.attempts.max(1));
        (time::OffsetDateTime::now_utc() + delay)
            .format(&time::format_description::well_known::Rfc3339)
            .ok()
    }

    /// Evaluate deferred continuations and start at most one normal session
    /// run per session in this pass. The scheduler loop calls this on every
    /// tick; it shares the same `AgentService` and `start` path as operator
    /// prompts, so approvals, sandboxing, capacity, and budgets are unchanged.
    pub async fn reconcile_session_continuations(&self) -> Result<usize> {
        let store = self.engine.store().clone();
        let open =
            tokio::task::spawn_blocking(move || store.list_open_session_continuations()).await??;
        let now = time::OffsetDateTime::now_utc();
        let mut started = 0usize;
        let mut attempted_sessions = HashSet::new();
        let mut task_cache: HashMap<String, Vec<ScheduledTask>> = HashMap::new();

        for continuation in open {
            if attempted_sessions.contains(&continuation.session_id) {
                continue;
            }
            let store = self.engine.store().clone();
            let session_id = continuation.session_id.clone();
            let session =
                tokio::task::spawn_blocking(move || store.get_session(&session_id)).await??;
            let Some(session) = session else {
                self.engine
                    .fail_session_continuation(&continuation.id, "session not found")
                    .await?;
                continue;
            };
            match session.status.as_str() {
                "imported" => {
                    self.engine
                        .cancel_session_continuation_with_reason(
                            &continuation.id,
                            "session is an imported read-only archive",
                        )
                        .await?;
                    continue;
                }
                "landed" => {
                    self.engine
                        .cancel_session_continuation_with_reason(
                            &continuation.id,
                            "session is landed and final",
                        )
                        .await?;
                    continue;
                }
                _ => {}
            }
            let mut continuation = continuation;
            let gate_record =
                crate::gate::gate_for_continuation(&self.engine, &continuation).await?;
            if continuation.watch_gate_id.is_some() && gate_record.is_none() {
                self.engine
                    .fail_session_continuation(
                        &continuation.id,
                        "the watched external gate no longer exists",
                    )
                    .await?;
                continue;
            }
            let gate_terminal = gate_record
                .as_ref()
                .is_some_and(|record| record.gate.is_terminal());
            if continuation.status == "pending" {
                let tasks = if let Some(tasks) = task_cache.get(&continuation.session_id) {
                    tasks.clone()
                } else {
                    let tasks = self
                        .engine
                        .task_queue(None, Some(continuation.session_id.clone()))
                        .await?
                        .tasks;
                    task_cache.insert(continuation.session_id.clone(), tasks.clone());
                    tasks
                };
                let Some(wake) = crate::continuation::evaluate_wake_with_gate(
                    &continuation.watch_task_ids,
                    &continuation.watch_mode,
                    continuation.deadline_at.as_deref(),
                    &tasks,
                    gate_terminal,
                    now,
                ) else {
                    continue;
                };
                let Some(ready) = self
                    .engine
                    .mark_session_continuation_ready(&continuation.id, &wake.reason, &wake.outcomes)
                    .await?
                else {
                    continue;
                };
                continuation = ready;
            }
            if !matches!(continuation.status.as_str(), "ready" | "delivering") {
                continue;
            }
            if let Some(next_attempt_at) = continuation.next_attempt_at.as_deref()
                && time::OffsetDateTime::parse(
                    next_attempt_at,
                    &time::format_description::well_known::Rfc3339,
                )
                .is_ok_and(|next| next > now)
            {
                continue;
            }

            match session.status.as_str() {
                "queued" | "running" | "paused" | "waiting_input" => continue,
                "idle" | "failed" | "cancelled" => {}
                _ => continue,
            }
            // Stall recovery owns the next plan resume when it is already
            // cancelling or waiting for quiescence. Let it complete first so a
            // ready continuation cannot race the recovered task.
            let recovery_active = self
                .engine
                .orchestration_recoveries(&continuation.session_id)
                .await?
                .recoveries
                .iter()
                .any(|recovery| matches!(recovery.status.as_str(), "requested" | "waiting_cancel"));
            if recovery_active {
                continue;
            }

            attempted_sessions.insert(continuation.session_id.clone());
            let Some(claim) = self
                .engine
                .claim_session_continuation_for_delivery(
                    &continuation.id,
                    continuation_claim_secs(),
                )
                .await?
            else {
                // Another reconciliation pass owns a live claim. It is now
                // `delivering`; this pass must not start a duplicate run.
                continue;
            };
            let continuation = claim.continuation.clone();
            let gate_summary = gate_record.as_ref().map(crate::gate::prompt_summary);
            let reason = continuation.wake_reason.as_deref().unwrap_or("watch");
            let prompt = crate::continuation::render_prompt(&crate::continuation::PromptContext {
                id: &continuation.id,
                series_id: &continuation.series_id,
                run_count: continuation.run_count,
                recurrence_every_secs: continuation.recurrence_every_secs,
                recurrence_max_runs: continuation.recurrence_max_runs,
                body: &continuation.body,
                reason,
                outcomes: &continuation.outcomes,
                gate: gate_summary.as_ref(),
            });
            // The operation receipt binds this continuation to the normal
            // start path. If the daemon crashed after starting but before the
            // claim was acknowledged, a reclaimed owner sees `unknown` and
            // fails closed instead of starting a second run.
            let operation_id = format!("continuation-delivery:{}", continuation.id);
            match self
                .start_with_operation(
                    &continuation.session_id,
                    &prompt,
                    Some(operation_id.as_str()),
                )
                .await
            {
                Ok(()) => match self.engine.ack_session_continuation_delivered(&claim).await {
                    Ok(Some(_)) => {
                        started = started.saturating_add(1);
                    }
                    Ok(None) => {
                        tracing::warn!(
                            continuation_id = %continuation.id,
                            "continuation delivered but its acknowledgement was not applied"
                        );
                    }
                    Err(crate::SessionContinuationAckError::Lost { .. }) => {
                        tracing::warn!(
                            continuation_id = %continuation.id,
                            "continuation delivery claim was reclaimed before acknowledgement"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(
                            %error,
                            continuation_id = %continuation.id,
                            "continuation delivery acknowledgement failed"
                        );
                    }
                },
                Err(error) => {
                    let error = format!("start failed: {error}");
                    let attempts = continuation.attempts.saturating_add(1);
                    if attempts >= continuation.max_attempts.max(1) {
                        match self
                            .engine
                            .ack_session_continuation_failed(&claim, &error)
                            .await
                        {
                            Ok(_) => {}
                            Err(crate::SessionContinuationAckError::Lost { .. }) => {
                                tracing::warn!(
                                    continuation_id = %continuation.id,
                                    "stale continuation owner could not record failure"
                                );
                            }
                            Err(error) => {
                                tracing::warn!(
                                    %error,
                                    continuation_id = %continuation.id,
                                    "continuation failure acknowledgement failed"
                                );
                            }
                        }
                    } else {
                        let next = now
                            + time::Duration::seconds(continuation_retry_backoff_secs(attempts));
                        let next_attempt_at = next
                            .format(&time::format_description::well_known::Rfc3339)
                            .map_err(|error| anyhow!("formatting retry time: {error}"))?;
                        match self
                            .engine
                            .ack_session_continuation_retry(&claim, &error, &next_attempt_at)
                            .await
                        {
                            Ok(_) => {}
                            Err(crate::SessionContinuationAckError::Lost { .. }) => {
                                tracing::warn!(
                                    continuation_id = %continuation.id,
                                    "stale continuation owner could not schedule a retry"
                                );
                            }
                            Err(error) => {
                                tracing::warn!(
                                    %error,
                                    continuation_id = %continuation.id,
                                    "continuation retry acknowledgement failed"
                                );
                            }
                        }
                    }
                }
            }
        }
        Ok(started)
    }

    pub(super) fn scaling_base_caps(&self) -> (usize, usize) {
        self.config
            .read()
            .ok()
            .and_then(|config| {
                config
                    .as_ref()
                    .map(|config| (config.max_workers, config.max_labour))
            })
            .unwrap_or((crate::scaling::HARD_WORKERS, crate::scaling::HARD_LABOUR))
    }

    pub(super) fn capacity_snapshot(&self) -> CapacityInfo {
        self.capacity.snapshot()
    }

    /// Evaluate one project/session without persisting or applying anything.
    pub async fn scaling_preview(
        &self,
        session_id: &str,
    ) -> Result<vp_protocol::scaling::ScalingPreviewResult> {
        let (workers, labour) = self.scaling_base_caps();
        let capacity = self.capacity_snapshot();
        crate::scaling::preview(&self.engine, session_id, &capacity, workers, labour).await
    }

    /// Read-only scaling status. It reports durable targets and live agent
    /// counts; it never evaluates or applies a decision.
    pub async fn scaling_status(
        &self,
        project_id: Option<&str>,
        session_id: Option<&str>,
        limit: Option<u32>,
    ) -> Result<vp_protocol::scaling::ScalingStatusResult> {
        let (workers, labour) = self.scaling_base_caps();
        let engine = self.engine.clone();
        let capacity = self.capacity_snapshot();
        let project_id = project_id.map(ToOwned::to_owned);
        let session_id = session_id.map(ToOwned::to_owned);
        tokio::task::spawn_blocking(move || {
            crate::scaling::status(
                &engine,
                project_id.as_deref(),
                session_id.as_deref(),
                limit,
                &capacity,
                workers,
                labour,
            )
        })
        .await?
    }

    /// One deterministic controller pass over every enabled project. It only
    /// moves Worker/Labour slot ceilings; all admission, approval, write-scope,
    /// Verifier, and merge gates remain in the existing run paths.
    pub async fn scaling_tick(&self) -> Result<vp_protocol::scaling::ScalingTickResult> {
        let (workers, labour) = self.scaling_base_caps();
        let capacity = self.capacity_snapshot();
        crate::scaling::run_tick(&self.engine, &capacity, workers, labour, None, None, true).await
    }

    pub fn start_scaling_controller(&self) {
        if self.scaling_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let seconds = env::var("VERLINPARK_SCALING_TICK_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(30);
        if seconds == 0 {
            return;
        }
        let seconds = seconds.clamp(5, 3_600);
        let service = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(seconds));
            ticker.tick().await;
            loop {
                ticker.tick().await;
                match service.scaling_tick().await {
                    Ok(result) if result.evaluated > 0 => {
                        tracing::debug!(
                            projects = result.projects,
                            sessions = result.sessions,
                            evaluated = result.evaluated,
                            applied = result.applied,
                            expanded = result.expanded,
                            scaled_down = result.scaled_down,
                            "deterministic scaling tick"
                        );
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(%error, "deterministic scaling tick failed");
                    }
                }
            }
        });
    }

    /// Opt-in global task-DAG resume scheduler. Every tick it picks one
    /// idle/failed/cancelled session with retryable tasks, ordered by effective
    /// priority and session round-robin, then resumes its stored plan. This is
    /// intentionally not default-on: resuming a plan spends the session budget.
    pub fn start_task_scheduler(&self) {
        if self.task_scheduler_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let interval_secs = env::var("VERLINPARK_TASK_SCHEDULER_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(5)
            .clamp(1, 3600);
        let service = self.clone();
        tokio::spawn(async move {
            if let Err(error) = service.reconcile_intake_tasks().await {
                tracing::warn!(%error, "intake reconciler failed at startup");
            }
            if let Err(error) = service.reconcile_session_continuations().await {
                tracing::warn!(%error, "continuation reconciler failed at startup");
            }
            if let Err(error) = service.reconcile_programs(None).await {
                tracing::warn!(%error, "program controller failed at startup");
            }
            let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
            ticker.tick().await;
            let mut last_session: Option<String> = None;
            let mut last_project: Option<String> = None;
            let mut cooldowns: HashMap<String, Instant> = HashMap::new();
            let mut intake_cooldowns: HashMap<String, Instant> = HashMap::new();
            loop {
                ticker.tick().await;
                if let Err(error) = service.reconcile_session_continuations().await {
                    tracing::warn!(%error, "global task scheduler could not reconcile continuations");
                }
                if let Err(error) = service.reconcile_intake_tasks().await {
                    tracing::warn!(%error, "global task scheduler could not reconcile intake tasks");
                }
                if let Err(error) = service.reconcile_programs(None).await {
                    tracing::warn!(%error, "global task scheduler could not reconcile programs");
                }
                let now = Instant::now();
                cooldowns.retain(|_, until| *until > now);
                intake_cooldowns.retain(|_, until| *until > now);

                let excluded = cooldowns.keys().cloned().collect::<Vec<_>>();
                let candidate = match service
                    .engine
                    .scheduler()
                    .next_pending_session(last_session.as_deref(), &excluded)
                    .await
                {
                    Ok(candidate) => candidate,
                    Err(error) => {
                        tracing::warn!(%error, "global task scheduler could not read the queue");
                        None
                    }
                };
                if let Some(candidate) = candidate {
                    cooldowns.insert(
                        candidate.session_id.clone(),
                        now + Duration::from_secs(candidate.cooldown_secs),
                    );
                    let dispatch_tasks = service
                        .engine
                        .get_task_scheduler_policy(&candidate.project_id)
                        .await
                        .map(|policy| policy.policy.dispatch_tasks)
                        .unwrap_or(false);
                    tracing::info!(
                        session_id = %candidate.session_id,
                        project_id = %candidate.project_id,
                        pending = candidate.pending,
                        session_priority = candidate.priority,
                        task_priority = candidate.task_priority,
                        effective_priority = candidate.effective_priority,
                        cooldown_secs = candidate.cooldown_secs,
                        dispatch_tasks,
                        "global task scheduler dispatching work"
                    );
                    let result = if dispatch_tasks {
                        service.dispatch_next_task(&candidate.session_id).await
                    } else {
                        service.resume_plan(&candidate.session_id).await
                    };
                    match result {
                        Ok(true) => {
                            last_session = Some(candidate.session_id);
                        }
                        Ok(false) => {}
                        Err(error) => {
                            tracing::warn!(
                                session_id = %candidate.session_id,
                                dispatch_tasks,
                                %error,
                                "global task scheduler could not dispatch work"
                            );
                        }
                    }
                }

                if !service.info().configured {
                    continue;
                }
                let excluded_intake = intake_cooldowns.keys().cloned().collect::<Vec<_>>();
                let intake = match service
                    .engine
                    .scheduler()
                    .next_pending_intake(last_project.as_deref(), &excluded_intake)
                    .await
                {
                    Ok(intake) => intake,
                    Err(error) => {
                        tracing::warn!(%error, "global task scheduler could not read intake tasks");
                        None
                    }
                };
                let Some(intake) = intake else {
                    continue;
                };
                let policy = service
                    .engine
                    .get_task_scheduler_policy(&intake.task.project_id)
                    .await
                    .ok()
                    .map(|result| result.policy)
                    .unwrap_or_default();
                intake_cooldowns.insert(
                    intake.task.id.clone(),
                    now + Duration::from_secs(policy.cooldown_secs),
                );
                tracing::info!(
                    intake_task_id = %intake.task.id,
                    project_id = %intake.task.project_id,
                    effective_priority = intake.effective_priority,
                    attempt = intake.task.attempts,
                    "global task scheduler dispatching standalone intake work"
                );
                match service.dispatch_intake_task(&intake.task.id).await {
                    Ok(result) if result.started => {
                        last_project = result.task.project_id.clone().into();
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(
                            intake_task_id = %intake.task.id,
                            %error,
                            "global task scheduler could not dispatch intake work"
                        );
                    }
                }
            }
        });
    }

    pub async fn auto_resolve_project(&self, project_id: &str) {
        let sessions = match self
            .engine
            .list_sessions(Some(project_id.to_string()))
            .await
        {
            Ok(sessions) => sessions,
            Err(error) => {
                tracing::warn!(%error, "auto-resolve could not list project sessions");
                return;
            }
        };
        for session in sessions {
            self.auto_resolve_session(&session.id).await;
        }
    }

    pub async fn auto_resolve_all(&self) {
        self.scan_auto_resolve().await;
    }

    pub(super) async fn scan_auto_resolve(&self) {
        let sessions = match self.engine.list_sessions(None).await {
            Ok(sessions) => sessions,
            Err(error) => {
                tracing::warn!(%error, "auto-resolve could not list sessions");
                return;
            }
        };
        for session in sessions {
            self.auto_resolve_session(&session.id).await;
        }
    }

    pub(super) async fn auto_resolve_session(&self, session_id: &str) {
        let Ok((session, _)) = load_session_project(self.engine.store().clone(), session_id).await
        else {
            return;
        };
        if session.status != "idle" {
            return;
        }
        let policy = match self
            .engine
            .get_integration_policy(&session.project_id)
            .await
        {
            Ok(policy) => policy,
            Err(error) => {
                tracing::warn!(%error, session_id, "auto-resolve could not load policy");
                return;
            }
        };
        if !policy.auto_resolve || policy.max_auto_attempts == 0 {
            return;
        }
        if !self.info().configured {
            return;
        }
        if self
            .runs
            .lock()
            .map(|runs| runs.contains_key(session_id))
            .unwrap_or(true)
        {
            return;
        }
        let queue = match self.engine.list_merge_queue(session_id).await {
            Ok(queue) => queue,
            Err(error) => {
                tracing::warn!(%error, session_id, "auto-resolve could not list queue");
                return;
            }
        };
        for entry in queue.into_iter().filter(|entry| entry.status == "conflict") {
            let attempted = self
                .engine
                .auto_resolve_attempts(&entry.id)
                .await
                .unwrap_or(policy.max_auto_attempts);
            if attempted >= policy.max_auto_attempts {
                continue;
            }
            let attempted = match self.engine.bump_auto_resolve_attempts(&entry.id).await {
                Ok(attempted) => attempted,
                Err(error) => {
                    tracing::warn!(%error, entry_id = %entry.id, "auto-resolve could not record attempt");
                    continue;
                }
            };
            if attempted > policy.max_auto_attempts {
                continue;
            }
            let _ = self
                .engine
                .append_event(
                    session_id,
                    "merge_queue.auto_resolve_started",
                    serde_json::json!({
                        "entry_id": entry.id,
                        "attempt": attempted,
                        "max_auto_attempts": policy.max_auto_attempts,
                        "max_resolve_tokens": policy.max_resolve_tokens,
                    }),
                )
                .await;
            match self
                .resolve_merge_with_limit(&entry.id, policy.max_resolve_tokens)
                .await
            {
                Ok(resolved) => {
                    let _ = self
                        .engine
                        .append_event(
                            session_id,
                            "merge_queue.auto_resolve_finished",
                            serde_json::json!({
                                "entry_id": entry.id,
                                "status": resolved.status,
                                "detail": resolved.detail,
                            }),
                        )
                        .await;
                }
                Err(error) => {
                    let _ = self
                        .engine
                        .append_event(
                            session_id,
                            "merge_queue.auto_resolve_finished",
                            serde_json::json!({
                                "entry_id": entry.id,
                                "status": "failed",
                                "detail": error.to_string(),
                            }),
                        )
                        .await;
                }
            }
        }
    }
}

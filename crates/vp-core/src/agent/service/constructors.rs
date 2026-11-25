use super::*;

/// Invalid approval modes fail closed to `strict` instead of silently
/// downgrading to `smart`. An unset variable keeps the normal default.
fn resolve_approval_mode(value: Option<&str>) -> ApprovalMode {
    match value {
        None => ApprovalMode::default(),
        Some(value) => match ApprovalMode::parse(value) {
            Some(mode) => mode,
            None => {
                tracing::warn!(
                    value = %value,
                    "invalid VERLINPARK_APPROVAL_MODE; falling back to strict (fail closed)"
                );
                ApprovalMode::Strict
            }
        },
    }
}

fn approval_mode_from_env() -> ApprovalMode {
    match env::var("VERLINPARK_APPROVAL_MODE") {
        Ok(value) => resolve_approval_mode(Some(&value)),
        Err(env::VarError::NotPresent) => resolve_approval_mode(None),
        Err(env::VarError::NotUnicode(_)) => {
            tracing::warn!(
                "non-Unicode VERLINPARK_APPROVAL_MODE; falling back to strict (fail closed)"
            );
            ApprovalMode::Strict
        }
    }
}

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
                approval_mode: approval_mode_from_env(),
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
            approval_mode: params.approval_mode.unwrap_or_else(approval_mode_from_env),
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

    /// Change the risk-based approval policy without touching the configured
    /// provider. The GUI must be able to switch modes even when the provider
    /// was configured from environment variables and has no Keychain key.
    pub fn set_approval_mode(&self, approval_mode: ApprovalMode) -> Result<AgentInfo> {
        self.set_runtime_config(Some(approval_mode), None, None, None)
    }

    /// Update runtime limits without rebuilding the provider. This keeps an
    /// env-only or Keychain-backed key intact while the operator changes the
    /// context window, turn limit, or approval mode.
    pub fn set_runtime_config(
        &self,
        approval_mode: Option<ApprovalMode>,
        context_window: Option<u64>,
        max_turns: Option<u32>,
        max_tokens: Option<u32>,
    ) -> Result<AgentInfo> {
        if approval_mode.is_none()
            && context_window.is_none()
            && max_turns.is_none()
            && max_tokens.is_none()
        {
            return Err(anyhow!("no runtime setting was provided"));
        }
        {
            let mut config = self
                .config
                .write()
                .map_err(|_| anyhow!("agent config lock poisoned"))?;
            let Some(config) = config.as_mut() else {
                return Err(anyhow!(
                    "agent is not configured; configure a provider before updating runtime settings"
                ));
            };
            if let Some(approval_mode) = approval_mode {
                config.approval_mode = approval_mode;
            }
            if let Some(context_window) = context_window {
                config.context_window = context_window.min(10_000_000);
            }
            if let Some(max_turns) = max_turns {
                config.max_turns = max_turns.clamp(1, 200);
            }
            if let Some(max_tokens) = max_tokens {
                config.max_tokens = Some(max_tokens.clamp(256, 65_536));
            }
        }
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
}

#[cfg(test)]
mod approval_mode_tests {
    use super::*;

    #[test]
    fn invalid_approval_mode_falls_back_to_strict() {
        assert_eq!(resolve_approval_mode(None), ApprovalMode::Smart);
        assert_eq!(resolve_approval_mode(Some("bogus")), ApprovalMode::Strict);
        assert_eq!(
            resolve_approval_mode(Some("unattended")),
            ApprovalMode::Unattended
        );
    }
}

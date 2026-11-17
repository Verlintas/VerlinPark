use super::*;

/// Explicit or opt-in warm of the stable prompt prefix.
///
/// This makes exactly one provider request using only `core` + `project` +
/// tools. It never includes memory, the objective, or conversation history.
/// The call is recorded in the normal usage ledger and emits both
/// `context.cache` (`warm: true`) and `context.cache_warm`, so its cost is
/// visible and cannot be mistaken for a free background operation.
#[allow(clippy::too_many_arguments)]
pub(in crate::agent) async fn warm_prompt_cache(
    engine: &Engine,
    config: &AgentConfig,
    session: &Session,
    spec: &AgentSpec,
    budget: &SessionBudget,
    sections: &crate::prompt_cache::PromptSections,
    tools: &[ToolSchema],
    source: &str,
) -> Result<SessionCacheWarmResult> {
    let model = model_for(config, spec.role);
    let cache_policy = engine
        .get_cache_policy(&session.project_id)
        .await
        .unwrap_or_default();
    let cache_config = crate::prompt_cache::prompt_cache_config(&cache_policy, spec.role.as_str());
    let plan = crate::prompt_cache::request_cache_plan(
        &config.provider_name,
        sections,
        tools.len(),
        1,
        &cache_policy,
        spec.role.as_str(),
    );
    let hashes = sections.hashes(&model, tools);
    let prompt_epoch = crate::cache_epoch::CacheEpoch::from_hashes(&model, &hashes);
    let epoch_reuse = engine
        .cache_epoch_reuse(&session.project_id, &prompt_epoch.hash, &session.id)
        .await?;
    if !cache_config.enabled {
        return Ok(SessionCacheWarmResult {
            session_id: session.id.clone(),
            warmed: false,
            provider: config.provider_name.clone(),
            model,
            role: spec.role.as_str().to_string(),
            source: source.to_string(),
            cache_breakpoints: 0,
            cache_ttl: plan.ttl.as_str().to_string(),
            automatic: false,
            core_hash: Some(hashes.core_hash),
            project_hash: Some(hashes.project_hash),
            tools_hash: Some(hashes.tools_hash),
            identity_hash: (!hashes.identity_hash.is_empty()).then(|| hashes.identity_hash.clone()),
            identity_id: hashes.identity_id.clone(),
            identity_generation: hashes.identity_generation,
            cache_epoch: Some(prompt_epoch.hash),
            cache_epoch_unchanged: true,
            cache_epoch_reused_from_sessions: epoch_reuse.prior_sessions,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            estimated: false,
            cost_usd: 0.0,
            latency_ms: 0,
            first_token_ms: 0,
            message: "project cache policy is disabled; no provider call was made".to_string(),
        });
    }
    let system = sections.render();
    let request = ChatRequest {
        model: model.clone(),
        messages: vec![ChatMessage::user(
            "Cache warm request. Reply with the single word OK.",
        )],
        system: Some(system),
        system_stable_prefix: Some(sections.stable_prefix()),
        system_sections: sections.provider_sections(),
        tail_sections: sections.tail_sections(),
        cache: Some(cache_config),
        max_tokens: Some(32),
        temperature: Some(0.0),
        tools: tools.to_vec(),
    };
    let input_estimate = estimate_request_tokens(&request);
    let reserve = !budget.is_unlimited() && budget.limit.is_some();
    let reservation = if reserve {
        budget.reserve_tokens(input_estimate.saturating_add(32))
    } else {
        None
    };
    if reserve && reservation.is_none() {
        anyhow::bail!("session budget has no room for an explicit cache warm");
    }
    let started = Instant::now();
    let mut stream = timeout(
        Duration::from_secs(config.provider_idle_timeout_secs),
        config.provider.stream_chat(request.clone()),
    )
    .await
    .map_err(|_| anyhow!("cache warm provider request timed out"))?
    .context("starting cache warm provider stream")?;
    let mut text = String::new();
    let mut usage = TokenUsage::default();
    let mut first_token_ms = 0u64;
    loop {
        let item = timeout(
            Duration::from_secs(config.provider_idle_timeout_secs),
            stream.next(),
        )
        .await
        .map_err(|_| anyhow!("cache warm provider stream was idle"))?;
        let Some(item) = item else {
            break;
        };
        match item? {
            StreamEvent::TextDelta(delta) => {
                if first_token_ms == 0 && !delta.is_empty() {
                    first_token_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
                }
                text.push_str(&delta);
                if text.len() >= 4_096 {
                    break;
                }
            }
            StreamEvent::Usage {
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
            } => usage.add(
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
            ),
            StreamEvent::ToolCall(_) | StreamEvent::Done => {}
        }
    }
    let has_output = !text.trim().is_empty();
    if !has_output && usage.is_empty() {
        anyhow::bail!("cache warm provider returned no text or usage");
    }
    let estimated = usage.input == 0 || usage.output == 0;
    let input_tokens = if usage.input > 0 {
        usage.input
    } else {
        estimate_request_tokens(&request)
    };
    let output_tokens = if usage.output > 0 {
        usage.output
    } else if has_output {
        estimate_text_tokens(&text)
    } else {
        0
    };
    let prices = role_prices(config, spec.role);
    let cost = usage_cost(
        input_tokens,
        output_tokens,
        usage.cache_read,
        usage.cache_write,
        prices,
    );
    let latency_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
    engine
        .record_agent_usage_with_cache(
            &session.id,
            "cache-warm",
            "Cache warm",
            "cache_warm",
            spec.task_id.clone(),
            spec.department.clone(),
            &model,
            input_tokens,
            output_tokens,
            usage.cache_read,
            usage.cache_write,
            estimated,
            cost,
        )
        .await?;
    if let Some(reservation) = reservation {
        let _ = reservation.settle(input_tokens.saturating_add(output_tokens));
    }
    budget.add_cost(cost);
    let savings = crate::prompt_cache::round_usd(crate::prompt_cache::estimated_cache_savings_usd(
        input_tokens,
        output_tokens,
        usage.cache_read,
        usage.cache_write,
        prices.input,
        prices.output,
        prices.cache_read,
        prices.cache_write,
    ));
    let hit_ratio =
        vp_protocol::types::cache_hit_ratio(input_tokens, usage.cache_read, usage.cache_write);
    // Warm calls are real provider calls, so they carry the same canonical
    // fields. They are excluded from conversation continuity aggregation
    // because they intentionally send only the stable prefix.
    let mut warm_tracker = crate::conversation::MessagePrefixTracker::default();
    let warm_observation = crate::conversation::CanonicalCallObservation::new(
        &request,
        config.provider.cache_capabilities(),
        &hashes.tools_hash,
        &mut warm_tracker,
    );
    let warm_canonical = crate::conversation::canonical_payload(&warm_observation);
    let mut warm_payload = serde_json::json!({
        "input_tokens": input_tokens,
        "cache_read_tokens": usage.cache_read,
        "cache_write_tokens": usage.cache_write,
        "output_tokens": output_tokens,
        "cache_hit_ratio": hit_ratio,
        "cache_read_ratio": if input_tokens == 0 {
            0.0
        } else {
            (usage.cache_read as f64 / input_tokens as f64).clamp(0.0, 1.0)
        },
        "cache_write_ratio": if input_tokens == 0 {
            0.0
        } else {
            (usage.cache_write as f64 / input_tokens as f64).clamp(0.0, 1.0)
        },
        "cache_breakpoints": plan.breakpoints,
        "cache_ttl": plan.ttl.as_str(),
        "cache_ttl_explicit": plan.ttl_explicit,
        "cache_automatic": plan.automatic,
        "first_token_ms": first_token_ms,
        "core_hash": hashes.core_hash,
        "project_hash": hashes.project_hash,
        "tools_hash": hashes.tools_hash,
        "volatile_hash": hashes.volatile_hash,
        "prompt_prefix_hash": hashes.prefix_hash,
        "cache_generation": hashes.prefix_hash,
        "estimated_savings_usd": savings,
        "miss_reason": serde_json::Value::Null,
        "estimated": estimated,
        "warm": true,
        "warm_source": source,
        "model": model,
        "cache_epoch_source": "l4",
        "cache_epoch_unchanged": true,
        "cache_epoch_reused_from_sessions": epoch_reuse.prior_sessions,
        "cross_session_cache_read_tokens": 0,
        "cross_session_first_call": false,
        "cache_miss_tokens": crate::prompt_cache::cache_miss_tokens(input_tokens, usage.cache_read),
        "theoretical_max_hit_ratio": 0.0,
        "message_prefix_hash": crate::prompt_cache::message_prefix_hash(&request.messages),
        "previous_message_prefix_hash": serde_json::Value::Null,
        "message_prefix_count": request.messages.len(),
        "previous_message_prefix_count": 0,
        "append_only": serde_json::Value::Null,
        "previous_input_tokens": 0,
    });
    if let (Some(target), Some(fields)) = (warm_payload.as_object_mut(), warm_canonical.as_object())
    {
        for (key, value) in fields {
            target.insert(key.clone(), value.clone());
        }
    }
    if let Some(object) = warm_payload.as_object_mut() {
        for (key, value) in prompt_epoch.payload_fields() {
            object.insert(key, value);
        }
    }
    if let Err(error) = emit(engine, &session.id, spec, "context.cache", warm_payload).await {
        tracing::warn!(%error, "could not persist cache warm telemetry");
    }
    let result = SessionCacheWarmResult {
        session_id: session.id.clone(),
        warmed: true,
        provider: config.provider_name.clone(),
        model,
        role: spec.role.as_str().to_string(),
        source: source.to_string(),
        cache_breakpoints: plan.breakpoints,
        cache_ttl: plan.ttl.as_str().to_string(),
        automatic: plan.automatic,
        core_hash: Some(hashes.core_hash),
        project_hash: Some(hashes.project_hash),
        tools_hash: Some(hashes.tools_hash),
        identity_hash: (!hashes.identity_hash.is_empty()).then(|| hashes.identity_hash.clone()),
        identity_id: hashes.identity_id.clone(),
        identity_generation: hashes.identity_generation,
        cache_epoch: Some(prompt_epoch.hash.clone()),
        cache_epoch_unchanged: true,
        cache_epoch_reused_from_sessions: epoch_reuse.prior_sessions,
        input_tokens,
        output_tokens,
        cache_read_tokens: usage.cache_read,
        cache_write_tokens: usage.cache_write,
        estimated,
        cost_usd: crate::prompt_cache::round_usd(cost),
        latency_ms,
        first_token_ms,
        message: truncate_to(text.trim(), 240),
    };
    if let Err(error) = emit(
        engine,
        &session.id,
        spec,
        "context.cache_warm",
        serde_json::to_value(&result)?,
    )
    .await
    {
        tracing::warn!(%error, "could not persist cache warm event");
    }
    Ok(result)
}

/// Preview profile when no agent row exists yet. The bundle matrix still
/// constrains the role, so this only affects which optional surfaces appear.
pub(in crate::agent) fn default_profile_for_preview(role: AgentRole) -> AgentProfile {
    match role {
        AgentRole::Leader | AgentRole::Manager | AgentRole::Worker | AgentRole::Labour => {
            AgentProfile::Generic
        }
        AgentRole::Verifier => AgentProfile::Verification,
        AgentRole::Scout => AgentProfile::Research,
    }
}

/// Resolve a plan task kind for a stored agent so a read-only cache preview
/// reports the same bundle the next run will use. Missing plans and taskless
/// agents return `None`, which preserves the legacy full fallback.
pub(in crate::agent) async fn plan_task_kind_for_agent(
    engine: &Engine,
    session_id: &str,
    agent: &Option<AgentRecord>,
) -> Option<String> {
    let task_id = agent.as_ref()?.task_id.as_ref()?;
    let plan = engine.get_team_plan(session_id).await.ok().flatten()?;
    plan.tasks
        .into_iter()
        .find(|task| task.id == *task_id)
        .map(|task| task.kind)
}

/// Build the stable synthetic spec used by cache preview/warm. It uses the
/// recorded agent row when one exists, otherwise the role's default name and a
/// generic profile. No worktree is created or modified.
pub(in crate::agent) fn stable_agent_spec(
    session: &Session,
    agent: Option<AgentRecord>,
    role: AgentRole,
    task_kind: Option<String>,
) -> AgentSpec {
    let profile = agent
        .as_ref()
        .and_then(|agent| agent.profile.as_deref())
        .and_then(AgentProfile::parse)
        .unwrap_or(AgentProfile::Generic);
    let worktree = PathBuf::from(session.worktree.clone().unwrap_or_default());
    let branch = session.branch.clone().unwrap_or_default();
    AgentSpec {
        id: agent
            .as_ref()
            .map(|agent| agent.id.clone())
            .unwrap_or_else(|| role.as_str().to_string()),
        role,
        name: agent
            .as_ref()
            .map(|agent| agent.name.clone())
            .unwrap_or_else(|| role.default_name().to_string()),
        depth: 0,
        parent_name: None,
        title: agent.as_ref().and_then(|agent| agent.title.clone()),
        task_id: None,
        department: None,
        identity_id: None,
        identity_generation: None,
        profile,
        task_kind,
        allow_delegation: role.child_role().is_some(),
        auto_approve: false,
        worktree: worktree.clone(),
        branch: branch.clone(),
        session_worktree: worktree,
        session_branch: branch,
        write_scope: Vec::new(),
        base_commit: session.base_commit.clone().unwrap_or_default(),
        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
    }
}

/// `true` when a persisted cache event describes this agent. Older events may
/// predate the `agent_id` field; a matching role is the best available seed,
/// and a mismatch is never used to fabricate continuity.
pub(in crate::agent) fn cache_event_belongs_to(payload: &Value, spec: &AgentSpec) -> bool {
    if let Some(agent_id) = payload.get("agent_id").and_then(Value::as_str) {
        return agent_id == spec.id;
    }
    payload
        .get("agent_role")
        .and_then(Value::as_str)
        .is_some_and(|role| role == spec.role.as_str())
}

/// Project the static bundle catalog into the protocol shape shared by
/// `agent.info`, `tool_bundle.catalog`, and `session.tool_bundle`.
pub(in crate::agent) fn tool_bundle_catalog_rows() -> Vec<ToolBundleCatalogRow> {
    crate::tool_bundle::catalog_entries()
        .into_iter()
        .map(|entry| ToolBundleCatalogRow {
            bundle: entry.kind.as_str().to_string(),
            title: entry.title.to_string(),
            summary: entry.summary.to_string(),
            target_limit: entry.target_limit,
            requested_count: entry.requested_count,
            representative_role: entry.representative_role.to_string(),
            representative_profile: entry.representative_profile.to_string(),
            allows_dynamic_mcp: entry.allows_dynamic_mcp,
        })
        .collect()
}

pub(in crate::agent) fn tool_bundle_warnings() -> Vec<ToolBundleWarningInfo> {
    crate::tool_bundle::audit_matrix()
        .into_iter()
        .map(|warning| ToolBundleWarningInfo {
            code: warning.code.to_string(),
            message: warning.message,
        })
        .collect()
}

/// Synthetic spec for the read-only bundle matrix. It uses no real worktree
/// and is never executed; only `frozen_tool_bundle` consumes it.
pub(in crate::agent) fn preview_spec(
    role: AgentRole,
    profile: AgentProfile,
    task_kind: Option<String>,
) -> AgentSpec {
    AgentSpec {
        id: format!("preview-{}", role.as_str()),
        role,
        name: role.default_name().to_string(),
        depth: 0,
        parent_name: None,
        title: None,
        task_id: None,
        department: None,
        identity_id: None,
        identity_generation: None,
        profile,
        task_kind,
        allow_delegation: role.child_role().is_some(),
        auto_approve: false,
        worktree: PathBuf::new(),
        branch: String::new(),
        session_worktree: PathBuf::new(),
        session_branch: String::new(),
        write_scope: Vec::new(),
        base_commit: String::new(),
        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
    }
}

/// Deterministic tool-bundle selection for one run. The role decides the
/// surface family; a known plan task kind partitions Worker/Labour work. The
/// result is computed once and frozen for the whole run.
pub(in crate::agent) fn tool_bundle_selection(
    spec: &AgentSpec,
) -> crate::tool_bundle::ToolBundleSelection {
    crate::tool_bundle::select_bundle(
        spec.role.as_str(),
        spec.profile.as_str(),
        spec.task_kind.as_deref(),
    )
}

/// Build the exact tool prefix the provider will see: role policy first, then
/// the named bundle filter, then dynamic MCP schemas sorted and appended once.
/// This is the only production path that constructs provider tool schemas.
pub(in crate::agent) fn frozen_tool_bundle(
    config: &AgentConfig,
    spec: &AgentSpec,
    mcp: &[McpToolBinding],
) -> crate::tool_bundle::FrozenToolBundle {
    let available = tool_schemas(config, spec.role, spec.profile, spec.allow_delegation);
    let mcp_schemas = mcp
        .iter()
        .map(|binding| binding.schema.clone())
        .collect::<Vec<_>>();
    crate::tool_bundle::freeze(tool_bundle_selection(spec), available, mcp_schemas)
}

/// Preview an explicitly named bundle without changing the role-derived
/// selection. Used by `tool_bundle.diff`; never used to start a run.
pub(in crate::agent) fn frozen_tool_bundle_for_kind(
    config: &AgentConfig,
    spec: &AgentSpec,
    kind: crate::tool_bundle::ToolBundleKind,
) -> crate::tool_bundle::FrozenToolBundle {
    let available = tool_schemas(config, spec.role, spec.profile, spec.allow_delegation);
    let selection = crate::tool_bundle::ToolBundleSelection::new(
        kind,
        spec.role.as_str(),
        spec.profile.as_str(),
        spec.task_kind.clone(),
        "explicit_preview",
        kind == crate::tool_bundle::ToolBundleKind::Full,
    );
    crate::tool_bundle::freeze(selection, available, Vec::new())
}

/// Legacy fingerprint retained for tests that assert the historical
/// tool-bundle hash helper. Production prompt hashing lives in
/// `crate::prompt_cache` so source/skill/volatile hashes are first-class.
#[cfg(test)]
pub(in crate::agent) const PROMPT_PREFIX_VERSION: u32 = 1;

#[cfg(test)]
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(in crate::agent) struct PromptPrefixFingerprint {
    pub(in crate::agent) version: u32,
    pub(in crate::agent) model: String,
    pub(in crate::agent) tool_bundle_hash: String,
    pub(in crate::agent) stable_system_hash: String,
    pub(in crate::agent) prefix_hash: String,
}

#[cfg(test)]
pub(in crate::agent) fn prompt_prefix_fingerprint(
    model: &str,
    tools: &[ToolSchema],
    stable_system: &str,
) -> PromptPrefixFingerprint {
    let mut sorted = tools.to_vec();
    sorted.sort_by(|left, right| left.name.cmp(&right.name));
    let tool_bundle = serde_json::to_string(&sorted).unwrap_or_default();
    let tool_bundle_hash = crate::sha256_hex(tool_bundle.as_bytes());
    let stable_system_hash = crate::sha256_hex(stable_system.as_bytes());
    let prefix_hash = crate::sha256_hex(
        format!("{PROMPT_PREFIX_VERSION}\n{model}\n{tool_bundle_hash}\n{stable_system_hash}")
            .as_bytes(),
    );
    PromptPrefixFingerprint {
        version: PROMPT_PREFIX_VERSION,
        model: model.to_string(),
        tool_bundle_hash,
        stable_system_hash,
        prefix_hash,
    }
}

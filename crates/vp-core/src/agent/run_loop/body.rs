use super::*;

/// Stable provider-turn failure: the model returned neither text nor a tool
/// call. This is a typed error rather than a successful empty answer so the run
/// journal and session status record a failure.
#[derive(Debug)]
pub(in crate::agent) struct EmptyProviderTurnError;

impl std::fmt::Display for EmptyProviderTurnError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "PROVIDER_EMPTY_TURN: the provider returned an assistant turn with no text and no tool calls"
        )
    }
}

impl std::error::Error for EmptyProviderTurnError {}

#[allow(clippy::too_many_arguments)]
pub(in crate::agent) async fn run_loop_body(
    engine: &Engine,
    config: &AgentConfig,
    session: &Session,
    project: &Project,
    control: &RunControl,
    budget: &SessionBudget,
    spec: &AgentSpec,
    objective: &str,
    conversation: &mut Vec<ChatMessage>,
    conversation_ids: Option<&mut Vec<Option<String>>>,
    top_level: bool,
    journal: &crate::run_journal::RunJournal,
    resumed: bool,
) -> Result<LoopOutcome> {
    let mut empty_ids = Vec::new();
    let conversation_ids: &mut Vec<Option<String>> = match conversation_ids {
        Some(ids) => ids,
        None => &mut empty_ids,
    };
    let mut citations = CitationRegistry::default();
    let mut active_skill: Option<Skill> = None;
    let mut deferred_wait: Option<DeferredWait> = None;
    let mut last_lease_heartbeat = Instant::now();
    const RUN_LEASE_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(300);
    let task_priority = if let Some(task_id) = &spec.task_id {
        engine
            .get_team_plan(&session.id)
            .await
            .ok()
            .flatten()
            .and_then(|plan| {
                plan.tasks
                    .iter()
                    .find(|task| &task.id == task_id)
                    .map(|task| task.priority)
            })
            .unwrap_or(0)
    } else {
        0
    };
    let capacity_priority = task_capacity_priority(session.priority, task_priority);
    let _capacity_permit = if matches!(
        spec.role,
        AgentRole::Worker | AgentRole::Labour | AgentRole::Scout
    ) && !spec.auto_approve
    {
        let worker_lane = matches!(spec.role, AgentRole::Worker | AgentRole::Scout);
        match config.capacity.try_acquire(spec.role, &session.id) {
            Some(permit) => Some(permit),
            None => {
                let snapshot = config.capacity.snapshot();
                emit(
                    engine,
                    &session.id,
                    spec,
                    "agent.capacity_wait",
                    serde_json::json!({
                        "role": spec.role.as_str(),
                        "limit": if worker_lane {
                            snapshot.worker_limit
                        } else {
                            snapshot.labour_limit
                        },
                        "available": if worker_lane {
                            snapshot.worker_available
                        } else {
                            snapshot.labour_available
                        },
                    }),
                )
                .await?;
                engine.update_agent_status(&spec.id, "queued").await?;
                control
                    .preempt
                    .set_phase(crate::preemption::Phase::Queue, None);
                let permit = match config
                    .capacity
                    .acquire(spec.role, &session.id, capacity_priority, control)
                    .await
                {
                    Some(permit) => permit,
                    None => return Ok(LoopOutcome::Cancelled),
                };
                engine.update_agent_status(&spec.id, "running").await?;
                emit(
                    engine,
                    &session.id,
                    spec,
                    "agent.capacity_ready",
                    serde_json::json!({
                        "role": spec.role.as_str(),
                        "waited": true,
                    }),
                )
                .await?;
                Some(permit)
            }
        }
    } else {
        None
    };
    let _scope_guard = if matches!(spec.role, AgentRole::Worker | AgentRole::Labour) {
        control
            .preempt
            .set_phase(crate::preemption::Phase::Queue, None);
        match config
            .scope_locks
            .acquire(
                &project.id,
                &session.id,
                &spec.id,
                &spec.write_scope,
                control,
            )
            .await?
        {
            Some(guard) => Some(guard),
            None => return Ok(LoopOutcome::Cancelled),
        }
    } else {
        None
    };
    let shell_policy = engine.get_shell_policy(&project.id).await?;
    let effect_policy = engine.get_effect_policy(&project.id).await?;
    let mut secret_roots = shell_secret_roots(engine.store().data_dir());
    secret_roots.extend(shell_policy.read_deny.iter().map(PathBuf::from));
    let tool_context = ToolContext {
        root: spec.worktree.clone(),
        project_root: PathBuf::from(&project.root),
        sandbox_root: engine
            .store()
            .data_dir()
            .join("sandbox")
            .join(&session.id)
            .join(&spec.id),
        secret_roots,
        network_allowed: shell_policy.network != "deny",
        home: std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default(),
        restrict_home: shell_policy.restrict_home,
        read_jail: shell_policy.read_jail,
        git_branch: Some(spec.branch.clone()),
        remote: engine.remote_access(&project.id).await?.map(Arc::new),
        session_id: Some(session.id.clone()),
        process_supervisor: Some(Arc::clone(engine.process_supervisor())),
        test_cache: Some(Arc::new(crate::test_cache::TestCacheAccess::new(
            engine.store().clone(),
            project.id.clone(),
        ))),
    };
    let run_contract = derive_contract_for_run(engine, project, session, spec).await?;
    let run_contract_policy = engine
        .get_contract_policy(&project.id)
        .await
        .unwrap_or_default();
    let mut run_contract_usage = match &run_contract {
        Some(contract) => engine
            .contract_usage(&contract.contract_id)
            .await
            .unwrap_or_default(),
        None => crate::contract::MutationUsage::default(),
    };
    let mut identity_memory_telemetry = None;
    // Resolve the session tool surface once and use the same frozen bundle for
    // prompt sections, hashes, context estimates, provider requests, cache warm,
    // tool_search promotion, and Code Mode bindings.
    let PromptAssembly {
        surface,
        frozen_bundle,
        sections,
        dynamic_mcp: dynamic_mcp_tools,
    } = build_prompt_and_bundle(
        engine,
        config,
        project,
        session,
        spec,
        run_contract.as_ref(),
        objective,
        true,
        InstructionBuildMode::Materialize,
        Some(&mut identity_memory_telemetry),
        Some(&session.id),
    )
    .await?;
    // Tail memory is appended to history once, at the position where it was
    // first seen, and is never reinserted ahead of newer messages. Without this
    // anchor a freshly rebuilt memory brief would move every turn and break the
    // provider prefix after the previous prompt. The anchor is a stable system
    // message; Anthropic projects it to a `[memory]` user message because its
    // `system` field is top-level.
    let memory_anchor = ensure_memory_anchor(
        engine,
        session,
        spec,
        &sections,
        conversation,
        conversation_ids,
        top_level,
    )
    .await?;
    let system = sections.render();
    // Retained for older adapters and the Anthropic legacy fallback. It never
    // contains volatile run context or memory.
    let stable_prefix = sections.stable_prefix();
    // The tool bundle is frozen for the run by `build_prompt_and_bundle`.
    // Skill activation is enforced at execution time, not by filtering schemas
    // mid-run, and dynamic MCP schemas are sorted and appended exactly once.
    // This is the prefix that every provider call in the run receives unchanged.
    let mut tools = frozen_bundle.tools.clone();
    let system_tokens = estimate_context_tokens(&system);
    let mut tool_tokens = serde_json::to_string(&tools)
        .map(|schema| estimate_context_tokens(&schema))
        .unwrap_or(0);
    let compaction_policy = context_compaction_policy(config, spec.role);
    let (context_threshold, context_hard_limit) =
        (compaction_policy.threshold, compaction_policy.hard_limit);
    if context_hard_limit != u64::MAX
        && system_tokens.saturating_add(tool_tokens) > context_hard_limit
    {
        return Err(anyhow!(
            "system prompt and tool schemas (~{} tokens) exceed the model context hard limit ({}); increase VERLINPARK_CONTEXT_WINDOW or role context_window, or reduce enabled tools/MCP servers",
            system_tokens.saturating_add(tool_tokens),
            context_hard_limit
        ));
    }
    let model_name = model_for(config, spec.role);
    let mut prompt_hashes = sections.hashes(&model_name, &tools);
    let mut prefix_hash = prompt_hashes.prefix_hash.clone();
    let contract_section = prompt_hashes
        .head_sections
        .iter()
        .find(|section| section.key == "session/contract");
    let contract_hash = contract_section.map(|section| section.hash.clone());
    let contract_chars = prompt_hashes.section_chars.get("session/contract").copied();
    let contract_policy_enabled = engine
        .get_contract_policy(&project.id)
        .await
        .map(|policy| policy.enabled)
        .unwrap_or(false);
    let cache_policy = engine
        .get_cache_policy(&project.id)
        .await
        .unwrap_or_default();
    let mut cache_plan = crate::prompt_cache::request_cache_plan(
        &config.provider_name,
        &sections,
        tools.len(),
        conversation.len(),
        &cache_policy,
        spec.role.as_str(),
    );
    let mut cache_config =
        crate::prompt_cache::prompt_cache_config(&cache_policy, spec.role.as_str());
    let provider_capabilities = config.provider.cache_capabilities();
    // L4 finalizes the shareable epoch. It commits to the model, canonical
    // tool hash, and ordered core/project section hashes. Session context, run
    // task, prompt, memory, and tail bytes are deliberately excluded.
    let mut prompt_epoch = crate::cache_epoch::CacheEpoch::from_hashes(&model_name, &prompt_hashes);
    let previous_prefix_event = engine
        .recent_events_of_kind(&session.id, "context.prompt_prefix", 1)
        .await?
        .into_iter()
        .last();
    let previous_epoch = previous_prefix_event
        .as_ref()
        .and_then(|event| crate::cache_epoch::CacheEpoch::from_event_payload(&event.payload));
    let epoch_change_reason = crate::cache_epoch::epoch_change_reason(
        previous_epoch.as_ref().map(|epoch| &epoch.identity),
        &prompt_epoch.identity,
    );
    let mut epoch_reuse = engine
        .cache_epoch_reuse(&project.id, &prompt_epoch.hash, &session.id)
        .await?;
    let identity_section_count = prompt_hashes.identity_sections.len();
    let identity_memory_present = prompt_hashes.section_chars.contains_key("agent/memory");
    let identity_memory_payload = identity_memory_telemetry
        .as_ref()
        .and_then(crate::identity_memory::IdentityMemoryBrief::telemetry);
    let identity_hash_value =
        (!prompt_hashes.identity_hash.is_empty()).then(|| prompt_hashes.identity_hash.clone());
    let mut prefix_payload = serde_json::json!({
        "version": prompt_hashes.prompt_layout,
        "prompt_layout": prompt_hashes.prompt_layout,
        "prompt_layout_name": crate::instruction::prompt_layout_name(prompt_hashes.prompt_layout),
        "instruction_state_id": crate::instruction::instruction_state_id(
            &session.id,
            &spec.id,
            spec.role == AgentRole::Leader,
        ),
        "provider_tail_system": provider_capabilities.tail_system.accepts_system(),
        "provider_wire": provider_capabilities.wire.as_str(),
        "tail_system_capability": provider_capabilities.tail_system.as_str(),
        "canonical_version": vp_providers::canonical::CANONICAL_VERSION,
        "memory_anchor": memory_anchor,
        "cache_break_events": crate::conversation::CACHE_BREAK_EVENT,
        "model": model_name,
        "tool_bundle": frozen_bundle.selection.kind.as_str(),
        "tool_bundle_mode": surface.mode.as_str(),
        "tool_surface": surface.payload(),
        "tool_bundle_hash": frozen_bundle.bundle_hash,
        "tool_bundle_reason": frozen_bundle.selection.reason,
        "tool_bundle_fallback": frozen_bundle.selection.fallback,
        "tool_bundle_tool_count": frozen_bundle.builtin_count(),
        "tool_bundle_mcp_count": frozen_bundle.mcp_count(),
        "tool_bundle_total_tool_count": frozen_bundle.tool_count(),
        "tool_bundle_target_limit": frozen_bundle.target_limit,
        "tool_bundle_within_target": frozen_bundle.within_target(),
        "tool_bundle_omitted": &frozen_bundle.omitted,
        "tool_bundle_warnings": frozen_bundle.warnings.iter().map(|warning| warning.payload()).collect::<Vec<_>>(),
        "tool_bundle_frozen": true,
        "stable_system_hash": prompt_hashes.stable_system_hash,
        "stable_prefix_hash": prompt_hashes.stable_prefix_hash,
        "core_hash": prompt_hashes.core_hash,
        "project_hash": prompt_hashes.project_hash,
        "tools_hash": prompt_hashes.tools_hash,
        "volatile_hash": prompt_hashes.volatile_hash,
        "prefix_hash": prefix_hash,
        "cache_generation": prefix_hash,
        // S3 identity metadata. It is hash/size only; no identity prompt text
        // or memory brief ever enters telemetry.
        "identity_hash": identity_hash_value,
        "identity_id": prompt_hashes.identity_id,
        "identity_generation": prompt_hashes.identity_generation,
        "identity_sections": prompt_hashes.identity_sections,
        "identity_section_count": identity_section_count,
        "identity_memory_present": identity_memory_present,
        // S5 memory telemetry: counts/hashes/chars only. Content never enters
        // the event stream.
        "identity_memory": identity_memory_payload,
        // The finalized L4 epoch replaces the L1 placeholder. A compaction
        // boundary later in the run records a conversation-cache reset; it
        // does not rewrite this shareable instruction identity.
        "cache_epoch_source": "l4",
        "cache_epoch_placeholder": false,
        "cache_epoch_previous": previous_epoch.as_ref().map(|epoch| epoch.hash.clone()),
        "cache_epoch_changed": previous_epoch.as_ref().map(|epoch| epoch.hash.as_str()) != Some(prompt_epoch.hash.as_str()),
        "cache_epoch_change_reason": epoch_change_reason.as_str(),
        "cache_epoch_reused_from_sessions": epoch_reuse.prior_sessions,
        "cache_epoch_session_seen_before": !epoch_reuse.first_session_call,
        "cache_epoch_session_calls": epoch_reuse.session_calls,
        "head_sections": prompt_hashes.head_sections,
        "tail_sections": prompt_hashes.tail_sections,
        "section_chars": prompt_hashes.section_chars,
        "contract_hash": contract_hash,
        "contract_chars": contract_chars,
        "contract_capsule": contract_hash.is_some(),
        "contract_policy_enabled": contract_policy_enabled,
        "tool_count": tools.len(),
        "core_chars": sections.core.chars().count(),
        "project_chars": sections.project.chars().count(),
        "volatile_chars": sections.volatile.chars().count(),
        "stable_prefix_chars": stable_prefix.chars().count(),
        "cache_policy": cache_policy,
        "cache_plan": cache_plan.payload(),
        // Heuristic estimate of the system-prompt and tool-schema preamble.
        // This is the same estimate already used for context-limit checks,
        // not provider-reported usage.
        "preamble_tokens_estimate": system_tokens.saturating_add(tool_tokens),
        "system_tokens_estimate": system_tokens,
        "tool_tokens_estimate": tool_tokens,
    });
    if let Some(object) = prefix_payload.as_object_mut() {
        for (key, value) in prompt_epoch.payload_fields() {
            object.insert(key, value);
        }
    }
    let base_prefix_payload = prefix_payload.clone();
    emit(
        engine,
        &session.id,
        spec,
        "context.prompt_prefix",
        prefix_payload,
    )
    .await?;
    // Durable per-run manifest. It is separate from `context.prompt_prefix`
    // so cache/prefix readers can stay small while an operator can inspect the
    // exact frozen tool list without a provider call.
    emit(
        engine,
        &session.id,
        spec,
        "context.tool_bundle",
        tool_bundle_manifest_with_surface(&frozen_bundle, surface),
    )
    .await?;
    // Seed miss attribution from the latest persisted ordinary agent call.
    // Warm calls are real provider calls but describe the stable sections only,
    // so comparing an agent call to a warm would mislabel a role/tool change as
    // churn. A resumed run can still attribute a tool/project change to the
    // previous agent generation.
    let recent_cache_events = engine
        .recent_events_of_kind(&session.id, "context.cache", 20)
        .await?;
    let latest_agent_cache_event = recent_cache_events.iter().rev().find(|event| {
        event
            .payload
            .get("warm")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
            && cache_event_belongs_to(&event.payload, spec)
    });
    let mut cache_tracker = crate::prompt_cache::CacheCallTracker::from_event(
        latest_agent_cache_event.map(|event| &event.payload),
    );
    let mut message_tracker = crate::conversation::MessagePrefixTracker::from_event(
        latest_agent_cache_event.map(|event| &event.payload),
    );
    let run_capabilities = config.provider.cache_capabilities();
    if let Some(payload) = crate::conversation::capability_change_payload(
        run_capabilities,
        latest_agent_cache_event.map(|event| &event.payload),
        &config.provider_name,
    ) && let Err(error) = emit(
        engine,
        &session.id,
        spec,
        "context.cache_capability",
        payload,
    )
    .await
    {
        tracing::warn!(%error, "could not persist provider cache capability telemetry");
    }
    // A compaction that happened after the last provider call (manual compact,
    // or an automatic compaction at the end of a previous run) makes the next
    // observation a reset, not a generic edit. Event sequence numbers are
    // per-session and monotonic, so this comparison is exact.
    if let Some(compaction) = engine
        .latest_event_of_kind(&session.id, "context.compacted")
        .await?
        && latest_agent_cache_event.is_none_or(|cache| compaction.seq > cache.seq)
    {
        message_tracker.mark_compaction();
    }
    if resumed && cache_policy.warm_on_resume && !budget.exhausted() {
        // Explicit policy opt-in only. The default is false, so a normal run
        // still makes exactly one provider call per turn.
        let warm_sections = build_prompt_sections_with_surface(
            engine,
            project,
            session,
            spec,
            None,
            objective,
            false,
            InstructionBuildMode::Materialize,
            None,
            surface,
            &frozen_bundle,
        )
        .await?;
        match warm_prompt_cache(
            engine,
            config,
            session,
            spec,
            budget,
            &warm_sections,
            &tools,
            surface,
            "resume",
        )
        .await
        {
            Ok(result) => {
                tracing::info!(
                    session_id = %session.id,
                    input_tokens = result.input_tokens,
                    cache_read_tokens = result.cache_read_tokens,
                    cost_usd = result.cost_usd,
                    "warmed the stable prompt prefix before resuming"
                );
            }
            Err(error) => {
                tracing::warn!(%error, "cache warm on resume failed; continuing the run");
                if let Err(emit_error) = emit(
                    engine,
                    &session.id,
                    spec,
                    "context.cache_warm",
                    serde_json::json!({
                        "session_id": session.id,
                        "warmed": false,
                        "source": "resume",
                        "provider": config.provider_name,
                        "model": model_name,
                        "message": truncate(&format!("{error:#}")),
                    }),
                )
                .await
                {
                    tracing::warn!(%emit_error, "could not persist failed cache warm event");
                }
            }
        }
    }

    let mut calibration = if top_level {
        load_context_calibration(engine, &session.id, &model_name).await
    } else {
        ContextCalibration::default()
    };
    let mut team_plan = engine.get_team_plan(&session.id).await?;
    let project_budget = engine.get_project_budget(&project.id).await?;
    let plan_budget = PlanBudgetScope::from_plan(
        team_plan.as_ref(),
        spec,
        Some(&project_budget.budget),
        &project.id,
    );
    // Static critical-path context for this run. It is intentionally computed
    // once at run start; a plan revision or completion is picked up by the next
    // run or scheduler tick. A disabled policy returns `None` without touching
    // task admission or provider budgeting.
    let governor_context = engine
        .governor_run_context(&session.id, spec.task_id.as_deref())
        .await?;
    let mut warned_project = false;
    // Bounded in-run prepared-summary cache. It is discarded at run end and
    // never starts a second agent run.
    let mut compaction_runtime = CompactionRuntime::default();
    // Number of prior provider observations for this session/epoch. The first
    // call of a new session can attribute provider cache reads to another
    // session; later calls would double-count the conversation prefix this
    // session itself created.
    let mut epoch_observed_calls = epoch_reuse.session_calls;
    let mut pending_promotions: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();
    let mut tool_bundle_revision = 0u32;
    let mut active_bundle_hash = frozen_bundle.bundle_hash.clone();
    // Only the top-level Leader owns the session auto-budget/turn/no-progress
    // state. Parallel Workers/Labour/Verifier runs never mutate it.
    let mut auto_budget = if top_level && spec.role == AgentRole::Leader {
        AutoBudgetRuntime::load(engine, session).await?
    } else {
        None
    };
    let max_turns = auto_budget
        .as_ref()
        .map(AutoBudgetRuntime::per_run_turn_limit)
        .unwrap_or(config.max_turns);

    for turn in 0..max_turns {
        // A long plan can outlive the assignment/workspace lease taken when
        // the run attached. Renew both at a bounded cadence. Lease loss is an
        // event-backed warning, not a silent switch to the legacy path.
        if let Some(identity_id) = &spec.identity_id
            && last_lease_heartbeat.elapsed() >= RUN_LEASE_HEARTBEAT_INTERVAL
        {
            match engine.heartbeat_run_leases(&session.id, identity_id).await {
                Ok(report) => {
                    if !report.warnings.is_empty() {
                        emit(
                            engine,
                            &session.id,
                            spec,
                            "agent.lease_heartbeat",
                            serde_json::json!({
                                "identity_id": identity_id,
                                "assignment_renewed": report.assignment_renewed,
                                "workspaces_renewed": report.workspaces_renewed,
                                "warnings": report.warnings,
                            }),
                        )
                        .await?;
                    }
                }
                Err(error) => {
                    emit(
                        engine,
                        &session.id,
                        spec,
                        "agent.lease_heartbeat_failed",
                        serde_json::json!({
                            "identity_id": identity_id,
                            "error": format!("{error:#}").chars().take(1_000).collect::<String>(),
                        }),
                    )
                    .await?;
                }
            }
            last_lease_heartbeat = Instant::now();
        }
        // Provider-facing bytes retained for tool results in this assistant
        // turn. It resets each turn and feeds the per-turn aggregate budget.
        let mut turn_tool_bytes = 0u64;
        if !wait_for_task_resume(engine, &session.id, spec, control).await? {
            return Ok(LoopOutcome::Cancelled);
        }
        if control.is_cancelled() {
            return Ok(LoopOutcome::Cancelled);
        }
        // Auto budget is evaluated before every top-level Leader provider call.
        // It may extend a low-runway budget, or pause at the hard cap and wait
        // for an explicit operator resume/top-up. Fixed/unlimited sessions and
        // scoped child runs skip this entirely.
        if let Some(runtime) = auto_budget.as_mut()
            && !runtime
                .before_turn(engine, session, spec, control, budget)
                .await?
        {
            return Ok(LoopOutcome::Cancelled);
        }
        if budget.exhausted() {
            return Ok(LoopOutcome::BudgetExceeded(BudgetStop::session(budget)));
        }
        if auto_budget.is_none()
            && let Some(reason) = budget.warning()
        {
            emit(
                engine,
                &session.id,
                spec,
                "agent.budget_warning",
                serde_json::json!({
                    "reason": reason,
                    "limit": budget.token_limit(),
                    "spent": budget.spent(),
                    "cost_limit_usd": budget.cost_limit(),
                    "cost_spent_usd": budget.cost_spent(),
                    "time_budget_secs": budget.time_limit().map(|limit| limit.as_secs()),
                    "elapsed_secs": budget.elapsed_secs(),
                }),
            )
            .await?;
        }
        // C8 schema promotion is applied only at a turn boundary. The current
        // provider request still used the frozen bundle that produced the
        // tool_search result; the next request sees the new schema set.
        if !pending_promotions.is_empty() {
            let candidates = std::mem::take(&mut pending_promotions);
            let available = tool_schemas_with_capabilities(
                config,
                spec.role,
                spec.profile,
                spec.allow_delegation,
                &surface.capabilities,
            );
            let mut added = Vec::new();
            for name in candidates {
                if !surface.capabilities.allows_tool(&name) {
                    continue;
                }
                if tools.iter().any(|tool| tool.name == name) {
                    continue;
                }
                if let Some(schema) = available.iter().find(|tool| tool.name == name) {
                    added.push(schema.clone());
                }
            }
            if !added.is_empty() {
                let old_tools_hash = prompt_hashes.tools_hash.clone();
                let old_epoch = prompt_epoch.hash.clone();
                let old_tool_tokens = tool_tokens;
                let prices = role_prices(config, spec.role);
                let added_names = added
                    .iter()
                    .map(|tool| tool.name.clone())
                    .collect::<Vec<_>>();
                let mut candidate_tools = tools.clone();
                candidate_tools.extend(added);
                candidate_tools.sort_by(|left, right| left.name.cmp(&right.name));
                candidate_tools.dedup_by(|left, right| left.name == right.name);
                let candidate_tool_tokens = serde_json::to_string(&candidate_tools)
                    .map(|schema| estimate_context_tokens(&schema))
                    .unwrap_or(0);
                if context_hard_limit != u64::MAX
                    && system_tokens.saturating_add(candidate_tool_tokens) > context_hard_limit
                {
                    if let Err(error) = emit(
                        engine,
                        &session.id,
                        spec,
                        "bundle.revision",
                        serde_json::json!({
                            "bundle": frozen_bundle.selection.kind.as_str(),
                            "tool_bundle_mode": surface.mode.as_str(),
                            "tool_surface": surface.payload(),
                            "revision": tool_bundle_revision,
                            "applied": false,
                            "reason": "context_limit",
                            "added_tools": added_names,
                            "tool_count": tools.len(),
                            "tool_tokens": candidate_tool_tokens,
                            "hard_limit": context_hard_limit,
                            "estimated": true,
                        }),
                    )
                    .await
                    {
                        tracing::warn!(%error, "could not persist rejected bundle revision");
                    }
                } else {
                    tools = candidate_tools;
                    tool_tokens = candidate_tool_tokens;
                    prompt_hashes = sections.hashes(&model_name, &tools);
                    prefix_hash = prompt_hashes.prefix_hash.clone();
                    cache_plan = crate::prompt_cache::request_cache_plan(
                        &config.provider_name,
                        &sections,
                        tools.len(),
                        conversation.len(),
                        &cache_policy,
                        spec.role.as_str(),
                    );
                    cache_config =
                        crate::prompt_cache::prompt_cache_config(&cache_policy, spec.role.as_str());
                    prompt_epoch =
                        crate::cache_epoch::CacheEpoch::from_hashes(&model_name, &prompt_hashes);
                    epoch_reuse = engine
                        .cache_epoch_reuse(&project.id, &prompt_epoch.hash, &session.id)
                        .await?;
                    epoch_observed_calls = epoch_reuse.session_calls;
                    tool_bundle_revision = tool_bundle_revision.saturating_add(1);
                    active_bundle_hash = crate::tool_bundle::canonical_tools_hash(&tools);
                    let schema_tokens_added = tool_tokens.saturating_sub(old_tool_tokens);
                    let lost_cache_read_usd = crate::prompt_cache::round_usd(
                        (schema_tokens_added as f64 / 1_000_000.0) * prices.cache_read,
                    );
                    if let Err(error) = emit(
                        engine,
                        &session.id,
                        spec,
                        "bundle.revision",
                        serde_json::json!({
                            "bundle": frozen_bundle.selection.kind.as_str(),
                            "tool_bundle_mode": surface.mode.as_str(),
                            "tool_surface": surface.payload(),
                            "revision": tool_bundle_revision,
                            "applied": true,
                            "previous_tools_hash": old_tools_hash,
                            "tools_hash": prompt_hashes.tools_hash,
                            "previous_cache_epoch": old_epoch,
                            "cache_epoch": prompt_epoch.hash,
                            "cache_epoch_changed": true,
                            "cache_epoch_change_reason": "tools_changed",
                            "added_tools": added_names,
                            "removed_tools": [],
                            "tool_count": tools.len(),
                            "target_limit": frozen_bundle.target_limit,
                            "over_target": frozen_bundle.target_limit.is_some_and(|limit| tools.len() > limit),
                            "previous_tool_tokens": old_tool_tokens,
                            "tool_tokens": tool_tokens,
                            "schema_tokens_added": schema_tokens_added,
                            "estimated_cache_invalidation_tokens": tool_tokens.max(old_tool_tokens),
                            "cache_read_price_per_million": prices.cache_read,
                            "estimated_lost_cache_read_usd": lost_cache_read_usd,
                            "estimated": true,
                        }),
                    )
                    .await
                    {
                        tracing::warn!(%error, "could not persist bundle revision");
                    }
                    let mut promoted_prefix = base_prefix_payload.clone();
                    if let Some(object) = promoted_prefix.as_object_mut() {
                        object.insert("prefix_hash".into(), serde_json::json!(prefix_hash));
                        object.insert("cache_generation".into(), serde_json::json!(prefix_hash));
                        object.insert(
                            "tools_hash".into(),
                            serde_json::json!(prompt_hashes.tools_hash),
                        );
                        object.insert(
                            "tool_bundle_hash".into(),
                            serde_json::json!(active_bundle_hash),
                        );
                        object.insert(
                            "bundle_revision".into(),
                            serde_json::json!(tool_bundle_revision),
                        );
                        object.insert("tool_bundle_frozen".into(), serde_json::json!(false));
                        object.insert("tool_count".into(), serde_json::json!(tools.len()));
                        object.insert(
                            "tool_bundle_tool_count".into(),
                            serde_json::json!(tools.len()),
                        );
                        object.insert(
                            "tool_bundle_total_tool_count".into(),
                            serde_json::json!(tools.len()),
                        );
                        object.insert(
                            "cache_epoch_change_reason".into(),
                            serde_json::json!("tools_changed"),
                        );
                        object.insert("cache_epoch_changed".into(), serde_json::json!(true));
                        object.insert("cache_epoch_previous".into(), serde_json::json!(old_epoch));
                        object.insert("cache_epoch_source".into(), serde_json::json!("l4"));
                        object.insert("cache_epoch_placeholder".into(), serde_json::json!(false));
                        object.insert(
                            "cache_epoch_reused_from_sessions".into(),
                            serde_json::json!(epoch_reuse.prior_sessions),
                        );
                        object.insert(
                            "cache_epoch_session_seen_before".into(),
                            serde_json::json!(!epoch_reuse.first_session_call),
                        );
                        object.insert(
                            "cache_epoch_session_calls".into(),
                            serde_json::json!(epoch_reuse.session_calls),
                        );
                        object.insert(
                            "stable_prefix_hash".into(),
                            serde_json::json!(prompt_hashes.stable_prefix_hash),
                        );
                        object.insert(
                            "stable_system_hash".into(),
                            serde_json::json!(prompt_hashes.stable_system_hash),
                        );
                        object.insert(
                            "core_hash".into(),
                            serde_json::json!(prompt_hashes.core_hash),
                        );
                        object.insert(
                            "project_hash".into(),
                            serde_json::json!(prompt_hashes.project_hash),
                        );
                        object.insert(
                            "volatile_hash".into(),
                            serde_json::json!(prompt_hashes.volatile_hash),
                        );
                        object.insert(
                            "head_sections".into(),
                            serde_json::json!(prompt_hashes.head_sections),
                        );
                        object.insert(
                            "tail_sections".into(),
                            serde_json::json!(prompt_hashes.tail_sections),
                        );
                        object.insert(
                            "section_chars".into(),
                            serde_json::json!(prompt_hashes.section_chars),
                        );
                        object.insert("cache_plan".into(), cache_plan.payload());
                        object.insert(
                            "system_tokens_estimate".into(),
                            serde_json::json!(system_tokens),
                        );
                        object.insert(
                            "tool_tokens_estimate".into(),
                            serde_json::json!(tool_tokens),
                        );
                        object.insert(
                            "preamble_tokens_estimate".into(),
                            serde_json::json!(system_tokens.saturating_add(tool_tokens)),
                        );
                    }
                    for (key, value) in prompt_epoch.payload_fields() {
                        if let Some(object) = promoted_prefix.as_object_mut() {
                            object.insert(key, value);
                        }
                    }
                    if let Err(error) = emit(
                        engine,
                        &session.id,
                        spec,
                        "context.prompt_prefix",
                        promoted_prefix,
                    )
                    .await
                    {
                        tracing::warn!(%error, "could not persist promoted prompt prefix");
                    }
                }
            }
        }
        {
            let compaction_before = crate::run_journal::journal_cursors(engine, &session.id)
                .await?
                .0;
            let compaction_changed = compact_context_if_needed(
                engine,
                config,
                session,
                spec,
                budget,
                control,
                conversation,
                &mut *conversation_ids,
                context_threshold,
                context_hard_limit,
                system_tokens.saturating_add(tool_tokens),
                calibration.factor,
                false,
                &prefix_hash,
                config.compact_prepare_ratio,
                &mut compaction_runtime,
                context_strategy(spec.role),
            )
            .await?;
            if compaction_changed {
                // The stable system/tool prefix is unchanged, but the cached
                // conversation prefix is not. The next call is attributed to
                // the reset rather than to a provider TTL guess.
                cache_tracker.mark_compaction();
                message_tracker.mark_compaction();
            }
            let compaction_after = crate::run_journal::journal_cursors(engine, &session.id)
                .await?
                .0;
            if compaction_after != compaction_before {
                journal
                    .record_compaction(
                        engine,
                        turn + 1,
                        compaction_before,
                        compaction_after,
                        conversation.len(),
                    )
                    .await?;
            }
            if control.is_cancelled() {
                return Ok(LoopOutcome::Cancelled);
            }
            if budget.exhausted() {
                return Ok(LoopOutcome::BudgetExceeded(BudgetStop::session(budget)));
            }
            let context_window = context_window_for(config, spec.role);
            if context_window > 0 {
                let estimated_context = calibrate_context_tokens(
                    estimate_messages_tokens(conversation)
                        .saturating_add(system_tokens)
                        .saturating_add(tool_tokens),
                    calibration.factor,
                );
                emit(
                    engine,
                    &session.id,
                    spec,
                    "context.usage",
                    serde_json::json!({
                        "estimated_tokens": estimated_context,
                        "context_window": context_window,
                        "threshold_tokens": context_threshold,
                        "hard_limit_tokens": context_hard_limit,
                        "threshold_source": compaction_policy.source(),
                        "threshold_ratio_basis_points": compaction_policy.ratio_basis_points,
                        "output_reserve_tokens": compaction_policy.output_reserve,
                        "messages": conversation.len(),
                        "calibration": calibration.factor,
                    }),
                )
                .await?;
            }
        }
        if let Err(error) = engine
            .touch_task_progress(&session.id, spec.task_id.as_deref(), turn + 1, None)
            .await
        {
            tracing::warn!(%error, "failed to record task turn progress");
        }
        let delivered = engine
            .deliver_pending_steering(&session.id, &spec.id, spec.task_id.as_deref(), top_level)
            .await?;
        for steering in delivered {
            conversation.push(ChatMessage::user(steering.content));
            if let Some(message) = steering.message {
                conversation_ids.push(Some(message.id));
            }
        }
        let mut request = ChatRequest {
            model: model_name.clone(),
            messages: conversation.clone(),
            system: Some(system.clone()),
            system_stable_prefix: Some(stable_prefix.clone()),
            system_sections: sections.provider_sections(),
            tail_sections: sections.tail_sections(),
            cache: Some(cache_config),
            max_tokens: config.max_tokens,
            temperature: Some(0.2),
            // Keep the full role bundle stable across the run. `active_skill`
            // is enforced when a tool is dispatched, not by changing schemas.
            tools: tools.clone(),
        };
        // A safe turn boundary: messages, budget, worktree HEAD, and plan
        // revision are all durable before the provider request intent.
        journal
            .checkpoint(engine, turn + 1, budget.run_snapshot())
            .await?;
        const MIN_OUTPUT_TOKENS: u64 = 64;
        let configured_output = config.max_tokens.unwrap_or(4096).max(1) as u64;
        let input_estimate = estimate_request_tokens(&request);
        let prices = role_prices(config, spec.role);
        let (input_price, output_price) = (prices.input, prices.output);
        let scope_spent = if plan_budget.active() && !budget.is_unlimited() {
            Some(
                engine
                    .usage_spent(
                        &session.id,
                        spec.task_id.as_deref(),
                        spec.department.as_deref(),
                    )
                    .await?,
            )
        } else {
            None
        };
        let task_spent = scope_spent
            .map(|(tokens, cost, _, _)| (tokens, cost))
            .unwrap_or((0, 0.0));
        let department_spent = scope_spent
            .map(|(_, _, tokens, cost)| (tokens, cost))
            .unwrap_or((0, 0.0));
        let project_spent = if plan_budget.project_active() && !budget.is_unlimited() {
            engine.project_usage(&project.id).await?
        } else {
            (0, 0.0)
        };
        if !budget.is_unlimited()
            && !warned_project
            && let Some(warning) = plan_budget.project_warning(project_spent)
        {
            emit(
                engine,
                &session.id,
                spec,
                "agent.budget_warning",
                warning.payload(),
            )
            .await?;
            warned_project = true;
        }
        let mut desired_output = if !budget.is_unlimited() && budget.token_limit().is_some() {
            let remaining = budget.available_tokens().unwrap_or(0);
            if remaining <= input_estimate.saturating_add(MIN_OUTPUT_TOKENS) {
                return Ok(LoopOutcome::BudgetExceeded(BudgetStop::session_tokens(
                    budget,
                )));
            }
            configured_output.min(remaining - input_estimate)
        } else {
            configured_output
        };
        // Governor gate for this provider call. It carries `critical` and the
        // task id with the normal scope requests. A non-critical request is
        // clamped to the free headroom, or deferred with a typed reason, but
        // never above the session/project/department hard budget.
        let governor_budget = if budget.token_limit().is_some() {
            (budget.available_tokens(), budget.token_limit())
        } else if let Some(limit) = plan_budget.project_tokens {
            (Some(limit.saturating_sub(project_spent.0)), Some(limit))
        } else {
            (None, None)
        };
        let governor_gate = governor_context
            .as_ref()
            .and_then(|context| context.gate(governor_budget.0, governor_budget.1));
        if !plan_budget.active()
            && !budget.is_unlimited()
            && let Some(gate) = governor_gate.as_ref()
        {
            match gate.available_output(input_estimate, MIN_OUTPUT_TOKENS) {
                Ok(Some(cap)) => desired_output = desired_output.min(cap),
                Ok(None) => {}
                Err(reason) => {
                    emit_governor_deferral(GovernorDeferralEvent {
                        engine,
                        session,
                        spec,
                        context: governor_context.as_ref(),
                        gate: Some(gate),
                        reason,
                        requested_tokens: input_estimate.saturating_add(desired_output),
                        remaining_tokens: gate.remaining_tokens,
                        limit_tokens: governor_budget.1,
                        spent_tokens: budget.spent(),
                        reserved_tokens: engine.budget_registry().reserved_for_session(&session.id),
                    })
                    .await?;
                    let stop = BudgetStop::governor(
                        reason,
                        spec.task_id.as_deref(),
                        budget,
                        gate.protected_tokens,
                    );
                    emit_scope_budget_stop(engine, &session.id, spec, &stop).await?;
                    return Ok(LoopOutcome::BudgetExceeded(stop));
                }
            }
        }
        let scope_lease = if plan_budget.active() && !budget.is_unlimited() {
            let requests = plan_budget.requests(
                &session.id,
                task_spent,
                department_spent,
                project_spent,
                input_price,
                output_price,
            );
            match engine.budget_registry().grant_governed(
                &requests,
                governor_gate.as_ref(),
                input_estimate,
                desired_output,
                MIN_OUTPUT_TOKENS,
            ) {
                Ok(lease) => Some(lease),
                Err(rejection) => {
                    if matches!(
                        rejection.reason,
                        BudgetReason::Reserved | BudgetReason::Throttled
                    ) {
                        emit_governor_deferral(GovernorDeferralEvent {
                            engine,
                            session,
                            spec,
                            context: governor_context.as_ref(),
                            gate: governor_gate.as_ref(),
                            reason: rejection.reason,
                            requested_tokens: input_estimate.saturating_add(desired_output),
                            remaining_tokens: governor_budget.0,
                            limit_tokens: governor_budget.1,
                            spent_tokens: budget.spent(),
                            reserved_tokens: rejection.reserved_tokens,
                        })
                        .await?;
                    }
                    let stop =
                        BudgetStop::from_governor_rejection(rejection, spec.task_id.as_deref());
                    emit_scope_budget_stop(engine, &session.id, spec, &stop).await?;
                    return Ok(LoopOutcome::BudgetExceeded(stop));
                }
            }
        } else {
            None
        };
        let output_budget = scope_lease
            .as_ref()
            .map_or(desired_output, |lease| lease.granted_output_tokens());
        let mut token_reservation = None;
        if !budget.is_unlimited() && budget.token_limit().is_some() {
            let Some(reservation) =
                budget.reserve_tokens(input_estimate.saturating_add(output_budget))
            else {
                return Ok(LoopOutcome::BudgetExceeded(BudgetStop::session_tokens(
                    budget,
                )));
            };
            token_reservation = Some(reservation);
        }
        let mut cost_reservation = match budget.reserve_cost(input_estimate, output_budget, prices)
        {
            Ok(reservation) => reservation,
            Err(()) => {
                return Ok(LoopOutcome::BudgetExceeded(BudgetStop::session_cost(
                    budget,
                )));
            }
        };
        if output_budget < configured_output {
            request.max_tokens = Some(output_budget.min(u32::MAX as u64) as u32);
        }
        let _scope_lease = scope_lease;
        // Canonical continuity is computed from the exact request that will be
        // sent, before the provider call. A structural break is recorded now;
        // a provider-side miss is only knowable after usage arrives. Neither
        // path ever rewrites history.
        let provider_capabilities = config.provider.cache_capabilities();
        let observation = crate::conversation::CanonicalCallObservation::new(
            &request,
            provider_capabilities,
            &prompt_hashes.tools_hash,
            &mut message_tracker,
        );
        if let Some(reason) = observation.report.reason {
            let payload = crate::conversation::cache_break_payload(
                &observation,
                reason,
                &config.provider_name,
                &request.model,
            );
            if let Err(error) = emit(
                engine,
                &session.id,
                spec,
                crate::conversation::CACHE_BREAK_EVENT,
                payload,
            )
            .await
            {
                tracing::warn!(%error, "could not persist cache break telemetry");
            }
        }
        let model_step = journal.model_intent(engine, turn + 1, &request).await?;
        let stream_turn = consume_stream(
            engine,
            &session.id,
            spec,
            control,
            config.provider.as_ref(),
            &request,
            Duration::from_secs(config.provider_idle_timeout_secs),
        )
        .await?;
        if top_level && stream_turn.usage.input > 0 {
            let estimated_request = estimate_messages_tokens(&request.messages)
                .saturating_add(system_tokens)
                .saturating_add(tool_tokens);
            let ratio = stream_turn.usage.input as f64 / estimated_request.max(1) as f64;
            calibration.observe(estimated_request, stream_turn.usage.input);
            // Measurement is diagnostic; a failed append must not discard the
            // provider call's accounting or the turn output.
            if let Err(error) = emit(
                engine,
                &session.id,
                spec,
                "context.measured",
                serde_json::json!({
                    "estimated_tokens": estimated_request,
                    "actual_input_tokens": stream_turn.usage.input,
                    "actual_output_tokens": stream_turn.usage.output,
                    "cache_read_tokens": stream_turn.usage.cache_read,
                    "cache_write_tokens": stream_turn.usage.cache_write,
                    "cache_hit_ratio": vp_protocol::types::cache_hit_ratio(
                        stream_turn.usage.input,
                        stream_turn.usage.cache_read,
                        stream_turn.usage.cache_write,
                    ),
                    "ratio": ratio,
                    "calibration": calibration.factor,
                    "samples": calibration.samples,
                    "model": request.model.clone(),
                    "messages": request.messages.len(),
                    "prompt_prefix_hash": prefix_hash,
                }),
            )
            .await
            {
                tracing::warn!(%error, "could not persist context measurement");
            }
        }
        let has_output = !stream_turn.text.is_empty() || !stream_turn.tool_calls.is_empty();
        let reported = stream_turn.usage;
        let input_tokens = if reported.input > 0 {
            reported.input
        } else if has_output || reported.output > 0 {
            estimate_request_tokens(&request)
        } else {
            0
        };
        let output_tokens = if reported.output > 0 {
            reported.output
        } else if has_output {
            estimate_output_tokens(&stream_turn.text, &stream_turn.tool_calls)
        } else {
            0
        };
        let estimated = reported.input == 0 || reported.output == 0;
        let (cache_read_tokens, cache_write_tokens) = if reported.input > 0 {
            (reported.cache_read, reported.cache_write)
        } else {
            (0, 0)
        };
        let record_usage = !reported.is_empty() || has_output;
        let mut budget_exceeded = false;
        if record_usage {
            let cost = usage_cost(
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
                prices,
            );
            engine
                .record_agent_usage_with_cache(
                    &session.id,
                    &spec.id,
                    &spec.name,
                    spec.role.as_str(),
                    spec.task_id.clone(),
                    spec.department.clone(),
                    &request.model,
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    cache_write_tokens,
                    estimated,
                    cost,
                )
                .await?;
            // Operational cache telemetry after settlement. No prompt text or
            // provider secrets; failures must not discard the accounting.
            let fingerprint =
                prompt_hashes.fingerprint_with_epoch_and_model(&prompt_epoch.hash, &model_name);
            let first_session_call = epoch_observed_calls == 0;
            cache_tracker.set_epoch_context(first_session_call, epoch_reuse.prior_sessions);
            let cache_telemetry =
                cache_tracker.observe_call(crate::prompt_cache::CacheCallObservation {
                    fingerprint: &fingerprint,
                    messages: &request.messages,
                    input_tokens,
                    cache_read_tokens,
                    cache_write_tokens,
                    cache_expected: cache_plan.enabled,
                    cache_epoch: Some(&prompt_epoch.hash),
                    cache_epoch_source: Some("l4"),
                });
            let miss_reason = cache_telemetry.miss_reason;
            let savings =
                crate::prompt_cache::round_usd(crate::prompt_cache::estimated_cache_savings_usd(
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    cache_write_tokens,
                    prices.input,
                    prices.output,
                    prices.cache_read,
                    prices.cache_write,
                ));
            if input_tokens > 0 {
                let provider_cache_miss = matches!(
                    miss_reason,
                    Some(crate::prompt_cache::CacheMissReason::ProviderCacheMiss)
                );
                let mut payload = serde_json::json!({
                    "input_tokens": input_tokens,
                    "cache_read_tokens": cache_read_tokens,
                    "cache_write_tokens": cache_write_tokens,
                    "output_tokens": output_tokens,
                    "cache_hit_ratio": vp_protocol::types::cache_hit_ratio(
                        input_tokens,
                        cache_read_tokens,
                        cache_write_tokens,
                    ),
                    "cache_read_ratio": if input_tokens == 0 {
                        0.0
                    } else {
                        (cache_read_tokens as f64 / input_tokens as f64).clamp(0.0, 1.0)
                    },
                    "cache_write_ratio": if input_tokens == 0 {
                        0.0
                    } else {
                        (cache_write_tokens as f64 / input_tokens as f64).clamp(0.0, 1.0)
                    },
                    "cache_breakpoints": cache_plan.breakpoints,
                    "cache_ttl": cache_plan.ttl.as_str(),
                    "cache_ttl_explicit": cache_plan.ttl_explicit,
                    "cache_automatic": cache_plan.automatic,
                    "first_token_ms": stream_turn.first_token_ms,
                    "core_hash": prompt_hashes.core_hash,
                    "project_hash": prompt_hashes.project_hash,
                    "tools_hash": prompt_hashes.tools_hash,
                    "tool_bundle": frozen_bundle.selection.kind.as_str(),
                    "tool_bundle_hash": active_bundle_hash,
                    "bundle_revision": tool_bundle_revision,
                    "tool_bundle_tool_count": tools.len(),
                    "tool_bundle_mcp_count": frozen_bundle.mcp_count(),
                    "volatile_hash": prompt_hashes.volatile_hash,
                    "prompt_prefix_hash": prefix_hash,
                    "cache_generation": prefix_hash,
                    "cache_epoch_source": "l4",
                    "cross_session_first_call": first_session_call,
                    "estimated_savings_usd": savings,
                    "miss_reason": miss_reason.map(|reason| reason.as_str()),
                    "estimated": estimated,
                    "warm": false,
                    "model": request.model,
                });
                let canonical_fields = crate::conversation::canonical_payload(&observation);
                if let (Some(target), Some(fields)) =
                    (payload.as_object_mut(), canonical_fields.as_object())
                {
                    for (key, value) in fields {
                        target.insert(key.clone(), value.clone());
                    }
                }
                if let Some(object) = payload.as_object_mut() {
                    object.insert(
                        "cache_miss_tokens".into(),
                        serde_json::json!(cache_telemetry.cache_miss_tokens),
                    );
                    object.insert(
                        "cache_hit_ratio_v2".into(),
                        serde_json::json!(cache_telemetry.cache_hit_ratio),
                    );
                    object.insert(
                        "theoretical_max_hit_ratio".into(),
                        serde_json::json!(cache_telemetry.theoretical_max_hit_ratio),
                    );
                    object.insert(
                        "message_prefix_hash".into(),
                        serde_json::json!(cache_telemetry.message_prefix_hash),
                    );
                    object.insert(
                        "previous_message_prefix_hash".into(),
                        serde_json::json!(cache_telemetry.previous_message_prefix_hash),
                    );
                    object.insert(
                        "message_prefix_count".into(),
                        serde_json::json!(cache_telemetry.message_prefix_count),
                    );
                    object.insert(
                        "previous_message_prefix_count".into(),
                        serde_json::json!(cache_telemetry.previous_message_prefix_count),
                    );
                    object.insert(
                        "append_only".into(),
                        serde_json::json!(cache_telemetry.append_only),
                    );
                    object.insert(
                        "previous_input_tokens".into(),
                        serde_json::json!(cache_telemetry.previous_input_tokens),
                    );
                }
                if let Some(object) = payload.as_object_mut() {
                    for (key, value) in prompt_epoch.payload_fields() {
                        object.insert(key, value);
                    }
                }
                agent_fields(&mut payload, spec);
                if provider_cache_miss && let Some(object) = payload.as_object_mut() {
                    object.insert(
                        "cache_break_reason".into(),
                        serde_json::json!("provider_cache_miss"),
                    );
                }
                let call = crate::PromptPrefixCall {
                    project_id: project.id.clone(),
                    cache_epoch: prompt_epoch.hash.clone(),
                    session_id: session.id.clone(),
                    model: model_name.clone(),
                    tool_bundle: frozen_bundle.selection.kind.as_str().to_string(),
                    tool_bundle_hash: frozen_bundle.bundle_hash.clone(),
                    core_hash: prompt_epoch.identity.core_hash.clone(),
                    project_hash: prompt_epoch.identity.project_hash.clone(),
                    core_sections: prompt_epoch.identity.core_sections.clone(),
                    project_sections: prompt_epoch.identity.project_sections.clone(),
                    input_tokens,
                    cache_read_tokens,
                };
                if let Err(error) = engine
                    .append_cache_call_event(&session.id, "context.cache", payload, &call)
                    .await
                {
                    tracing::warn!(%error, "could not persist context cache telemetry");
                } else {
                    epoch_observed_calls = epoch_observed_calls.saturating_add(1);
                }
                if provider_cache_miss {
                    let miss_payload = crate::conversation::provider_miss_payload(
                        &observation,
                        &config.provider_name,
                        &request.model,
                        input_tokens,
                        cache_read_tokens,
                    );
                    if let Err(error) = emit(
                        engine,
                        &session.id,
                        spec,
                        crate::conversation::CACHE_BREAK_EVENT,
                        miss_payload,
                    )
                    .await
                    {
                        tracing::warn!(%error, "could not persist provider cache miss telemetry");
                    }
                }
            }
            let actual_usage = input_tokens.saturating_add(output_tokens);
            let cost_exceeded = if let Some(reservation) = cost_reservation.take() {
                reservation.settle(cost)
            } else {
                budget.add_cost(cost)
            };
            budget_exceeded = token_reservation
                .take()
                .is_some_and(|reservation| reservation.settle(actual_usage))
                || cost_exceeded;
        }
        if let Some(preempted) = stream_turn.preempted {
            let output_bytes = stream_turn.text.len() as u64;
            control
                .preempt
                .annotate_usage(input_tokens, output_tokens, output_bytes, estimated);
            let snapshot = control.preempt.snapshot().unwrap_or(preempted);
            Box::pin(emit_provider_preempted(
                engine,
                session,
                project,
                spec,
                &snapshot,
                &stream_turn.text,
                &stream_turn.tool_calls,
            ))
            .await?;
            journal
                .model_failed(
                    engine,
                    &model_step,
                    &format!(
                        "provider stream preempted during {} ({})",
                        snapshot.phase.as_str(),
                        snapshot.reason.as_str()
                    ),
                )
                .await?;
            return Ok(LoopOutcome::Cancelled);
        }
        if stream_turn.cancelled {
            journal
                .model_failed(
                    engine,
                    &model_step,
                    "the run was cancelled during the provider stream",
                )
                .await?;
            return Ok(LoopOutcome::Cancelled);
        }
        if let Some(error) = stream_turn.error {
            // Keep partial streamed output visible instead of letting the
            // failure clear the live transcript.
            if !stream_turn.text.is_empty() {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "agent.message",
                    serde_json::json!({
                        "text": stream_turn.text,
                        "tool_calls": stream_turn.tool_calls,
                        "partial": true,
                    }),
                )
                .await?;
            }
            journal
                .model_failed(engine, &model_step, &format!("{error:#}"))
                .await?;
            return Err(error);
        }
        if budget_exceeded {
            journal
                .model_failed(
                    engine,
                    &model_step,
                    "the provider response arrived but the session budget was exhausted before it could be persisted",
                )
                .await?;
            return Ok(LoopOutcome::BudgetExceeded(BudgetStop::session(budget)));
        }
        if !budget.is_unlimited() && plan_budget.active() {
            let (task_tokens, task_cost, department_tokens, department_cost) = engine
                .usage_spent(
                    &session.id,
                    spec.task_id.as_deref(),
                    spec.department.as_deref(),
                )
                .await?;
            let project_usage = if plan_budget.project_active() {
                engine.project_usage(&project.id).await?
            } else {
                (0, 0.0)
            };
            if let Some(stop) = plan_budget.after(
                (task_tokens, task_cost),
                (department_tokens, department_cost),
                project_usage,
            ) {
                emit_scope_budget_stop(engine, &session.id, spec, &stop).await?;
                return Ok(LoopOutcome::BudgetExceeded(stop));
            }
        }
        let text = stream_turn.text;
        let tool_calls = stream_turn.tool_calls;

        if text.is_empty() && tool_calls.is_empty() {
            // An assistant turn with no text and no tool calls is not a valid
            // completion. Failing closed here prevents a provider glitch from
            // being persisted as a successful empty Final answer.
            let error = anyhow::Error::new(EmptyProviderTurnError);
            journal
                .model_failed(engine, &model_step, &format!("{error:#}"))
                .await?;
            return Err(error);
        }

        let assistant_tool_calls = if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls.clone())
        };
        let stored_id = if top_level {
            let stored = engine
                .append_message(
                    &session.id,
                    "assistant",
                    &text,
                    serde_json::to_value(assistant_tool_calls.as_deref().unwrap_or(&[]))?,
                    None,
                )
                .await?;
            conversation_ids.push(Some(stored.id.clone()));
            Some(stored.id)
        } else {
            None
        };
        let citation_records = citations.list();
        emit(
            engine,
            &session.id,
            spec,
            "agent.message",
            serde_json::json!({
                "text": text,
                "tool_calls": tool_calls,
                "message_id": stored_id,
                "citations": citation_records,
            }),
        )
        .await?;
        conversation.push(ChatMessage {
            role: "assistant".to_string(),
            content: text.clone(),
            tool_calls: assistant_tool_calls,
            tool_call_id: None,
        });
        journal
            .model_completed(
                engine,
                &model_step,
                stored_id.as_deref(),
                &text,
                &tool_calls,
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
                estimated,
            )
            .await?;

        if !wait_for_task_resume(engine, &session.id, spec, control).await? {
            return Ok(LoopOutcome::Cancelled);
        }
        if tool_calls.is_empty() {
            emit(
                engine,
                &session.id,
                spec,
                "agent.turn_finished",
                serde_json::json!({"turn": turn + 1}),
            )
            .await?;
            if let Some(runtime) = auto_budget.as_mut() {
                let current_turn = runtime.current_turn();
                if !runtime
                    .finish_turn(
                        engine,
                        session,
                        spec,
                        control,
                        budget,
                        current_turn,
                        false,
                    )
                    .await?
                {
                    return Ok(LoopOutcome::Cancelled);
                }
            }
            return Ok(LoopOutcome::Final(text));
        }

        let safe_parallel_batch = tool_calls.len() > 1
            && tool_calls.iter().all(|call| {
                is_parallel_safe_tool(&call.name)
                    && spec.role.allows_tool(spec.profile, &call.name)
                    && active_skill
                        .as_ref()
                        .is_none_or(|skill| skill_allows_tool(skill, &call.name))
                    && (config.tools.get(&call.name).is_some() || call.name == "read_tool_output")
            });
        let gui_policy = engine
            .gui_policy_for_session(&session.id)
            .await
            .unwrap_or_default();
        if safe_parallel_batch {
            let mut prepared = Vec::new();
            for (index, call) in tool_calls.iter().enumerate() {
                let call_id = if call.id.is_empty() {
                    format!("call_{}_{}", turn, index)
                } else {
                    call.id.clone()
                };
                let Ok(input) = parse_tool_arguments(&call.arguments) else {
                    // Invalid provider arguments are a typed tool failure on
                    // the sequential path. Do not assess or execute a call
                    // with guessed `{}` input.
                    prepared.clear();
                    break;
                };
                let assessment = crate::permissions::assess_tool_call_with_gui(
                    &call.name,
                    &input,
                    &spec.worktree,
                    &gui_policy,
                );
                // A contract denial/escalation on a read-only parallel call
                // falls back to the sequential path, which records the intent
                // decision and takes the normal approval route.
                if let Some(contract) = &run_contract {
                    let contract_spec = crate::contract::ContractCallSpec {
                        tool: &call.name,
                        contract_tool: None,
                        input: &input,
                        risk: assessment.risk,
                        worktree: &spec.worktree,
                        usage: run_contract_usage,
                        mcp_trust: None,
                        patch_text: None,
                    };
                    if !crate::contract::authorize(&contract_spec, contract, &run_contract_policy)
                        .is_allow()
                    {
                        prepared.clear();
                        break;
                    }
                }
                // Preview the persisted broker (templates, grants, audit) with
                // a non-consuming rule evaluation. The fast path may only run
                // when every candidate is a plain Allow; anything else falls
                // back to the sequential dispatcher, which performs the one
                // real evaluation and records the operator-visible decision.
                let preview = match preview_tool_permission(
                    engine,
                    session,
                    &assessment,
                    &call.name,
                    &input,
                    config.approval_mode,
                    spec.auto_approve,
                ) {
                    Ok(preview) => preview,
                    Err(error) => {
                        // Let the sequential broker surface the error with its
                        // normal Deny path instead of adding a new failure mode.
                        tracing::warn!(%error, tool = %call.name, "approval preview failed");
                        prepared.clear();
                        break;
                    }
                };
                if !matches!(preview.decision, ApprovalDecision::Allow { .. }) {
                    prepared.clear();
                    break;
                }
                let tool = config.tools.get(&call.name);
                if tool.is_none() && call.name != "read_tool_output" {
                    prepared.clear();
                    break;
                }
                prepared.push((
                    index,
                    call_id,
                    call.name.clone(),
                    input,
                    tool,
                    assessment.risk,
                ));
            }
            if !prepared.is_empty() {
                // Every candidate passed the non-consuming preview. Record the
                // real contract decision and the persisted broker decision
                // exactly once now. A concurrent policy change that makes the
                // real decision non-Allow fails closed before any tool runs;
                // falling back here would consume a template use twice.
                for (_, _, name, input, _, _) in &prepared {
                    if let Some(contract) = &run_contract {
                        let contract_spec = crate::contract::ContractCallSpec {
                            tool: name,
                            contract_tool: None,
                            input,
                            risk: crate::permissions::assess_tool_call_with_gui(
                                name,
                                input,
                                &spec.worktree,
                                &gui_policy,
                            )
                            .risk,
                            worktree: &spec.worktree,
                            usage: run_contract_usage,
                            mcp_trust: None,
                            patch_text: None,
                        };
                        let decision = crate::contract::authorize(
                            &contract_spec,
                            contract,
                            &run_contract_policy,
                        );
                        engine
                            .record_intent_decision(contract, &decision, &contract_spec)
                            .await?;
                    }
                    let assessment = crate::permissions::assess_tool_call_with_gui(
                        name,
                        input,
                        &spec.worktree,
                        &gui_policy,
                    );
                    let evaluation = engine.evaluate_tool_permission(
                        &session.id,
                        &session.project_id,
                        &assessment,
                        name,
                        input,
                        config.approval_mode,
                        spec.auto_approve,
                    )?;
                    if !matches!(
                        evaluation.decision,
                        crate::permissions::ApprovalDecision::Allow { .. }
                    ) {
                        return Err(anyhow!(
                            "approval policy changed while preparing a parallel read batch for {name}; retry the turn"
                        ));
                    }
                }
                let mut journal_pairs = std::collections::HashMap::new();
                for (_, call_id, name, input, _, _) in &prepared {
                    if let Err(error) = engine
                        .touch_task_progress(
                            &session.id,
                            spec.task_id.as_deref(),
                            turn + 1,
                            Some(name),
                        )
                        .await
                    {
                        tracing::warn!(%error, "failed to record task tool progress");
                    }
                    let (step, tool, _) = journal
                        .tool_intent(engine, turn + 1, call_id, name, input, None)
                        .await?;
                    journal_pairs.insert(call_id.to_string(), (step, tool));
                    emit(
                        engine,
                        &session.id,
                        spec,
                        "tool.started",
                        serde_json::json!({
                            "tool_call_id": call_id,
                            "name": name,
                            "input": input,
                        }),
                    )
                    .await?;
                }
                let mut cancel_rx = control.subscribe();
                // Parallel tools are a short read-only batch. A per-tool
                // deadline is enforced on sequential tools; the batch observes
                // the run token so one slow read cannot leave siblings running
                // after an operator/wall-clock preemption.
                let batch_token = crate::preemption::Token::tool_child(&control.preempt, None);
                batch_token.set_phase(crate::preemption::Phase::Tool, None);
                // Each tool gets its own deadline token in addition to the
                // run/batch token. A tool deadline must abort only that tool;
                // the run token still aborts the whole batch on operator or
                // wall-clock preemption.
                let tool_deadline = crate::preemption::tool_deadline(&control.preemption_policy());
                let mut pending = prepared.into_iter();
                let mut set = tokio::task::JoinSet::new();
                for _ in 0..4 {
                    let Some((index, call_id, name, input, tool, risk)) = pending.next() else {
                        break;
                    };
                    let context = tool_context.clone();
                    let engine = engine.clone();
                    let session = session.clone();
                    let tool_token =
                        crate::preemption::Token::tool_child(&batch_token, tool_deadline);
                    tool_token.set_phase(crate::preemption::Phase::Tool, Some(&name));
                    set.spawn(async move {
                        let started = Instant::now();
                        let (mut output, deadline) = execute_parallel_tool(
                            tool,
                            engine,
                            session,
                            context,
                            tool_token,
                            input,
                            name.clone(),
                        )
                        .await;
                        output.normalize_failure(&name);
                        let duration_ms =
                            started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
                        (index, call_id, name, output, duration_ms, risk, deadline)
                    });
                }
                let mut results = Vec::new();
                loop {
                    if set.is_empty() && pending.len() == 0 {
                        break;
                    }
                    tokio::select! {
                        _ = cancel_rx.changed() => {
                            set.abort_all();
                            return Ok(LoopOutcome::Cancelled);
                        }
                        _ = batch_token.cancelled() => {
                            set.abort_all();
                            if let Some(snapshot) = batch_token.snapshot() {
                                let mut snapshot = snapshot;
                                snapshot.detail = Some("parallel_batch_aborted".to_string());
                                Box::pin(emit_tool_preempted(
                                    engine, session, project, spec, &snapshot,
                                ))
                                .await?;
                            }
                            return Ok(LoopOutcome::Cancelled);
                        }
                        joined = set.join_next() => {
                            match joined {
                                Some(Ok(item)) => results.push(item),
                                Some(Err(error)) => {
                                    return Err(anyhow!("parallel tool task failed: {error}"));
                                }
                                None => break,
                            }
                            if let Some((index, call_id, name, input, tool, risk)) = pending.next() {
                                let context = tool_context.clone();
                                let engine = engine.clone();
                                let session = session.clone();
                                let tool_token = crate::preemption::Token::tool_child(
                                    &batch_token,
                                    tool_deadline,
                                );
                                tool_token
                                    .set_phase(crate::preemption::Phase::Tool, Some(&name));
                                set.spawn(async move {
                                    let started = Instant::now();
                                    let (mut output, deadline) = execute_parallel_tool(
                                        tool,
                                        engine,
                                        session,
                                        context,
                                        tool_token,
                                        input,
                                        name.clone(),
                                    )
                                    .await;
                                    output.normalize_failure(&name);
                                    let duration_ms =
                                        started.elapsed().as_millis().min(u128::from(u64::MAX))
                                            as u64;
                                    (index, call_id, name, output, duration_ms, risk, deadline)
                                });
                            }
                        }
                    }
                }
                results.sort_by_key(|(index, _, _, _, _, _, _)| *index);
                // A per-tool deadline is recorded as that tool's event only.
                // It never changes session status and never aborts siblings.
                for (_, _, _, _, _, _, deadline) in &results {
                    if let Some(snapshot) = deadline
                        && snapshot.reason == crate::preemption::Reason::ToolDeadline
                    {
                        Box::pin(emit_tool_preempted(
                            engine, session, project, spec, snapshot,
                        ))
                        .await?;
                    }
                }
                let mut parallel_metrics = std::collections::HashMap::new();
                let pending = results
                    .into_iter()
                    .map(|(_, call_id, name, output, duration_ms, risk, _)| {
                        parallel_metrics.insert(call_id.clone(), (duration_ms, risk));
                        crate::tool_output::PendingToolOutput {
                            tool_call_id: call_id,
                            tool_name: name,
                            output,
                        }
                    })
                    .collect::<Vec<_>>();
                let (mut shaped, _) = shape_tool_output_batch(
                    engine,
                    session,
                    spec,
                    project,
                    turn_tool_bytes,
                    pending,
                )
                .await?;
                for item in &mut shaped {
                    let call_id = item.tool_call_id.clone();
                    let name = item.tool_name.clone();
                    let output = &mut item.output;
                    output.annotate_shape_metadata();
                    if let Some((journal_step, journal_tool)) = journal_pairs.remove(&call_id) {
                        journal
                            .tool_completed(engine, &journal_step, &journal_tool, output)
                            .await?;
                    }
                    let (duration_ms, risk) = parallel_metrics
                        .get(&call_id)
                        .copied()
                        .unwrap_or((0, RiskLevel::ReadOnly));
                    emit(
                        engine,
                        &session.id,
                        spec,
                        "tool.finished",
                        tool_finished_payload(
                            &call_id,
                            &name,
                            output,
                            duration_ms,
                            Some(risk),
                            config.approval_mode,
                            None,
                        ),
                    )
                    .await?;
                    if top_level {
                        let stored = engine
                            .append_message(
                                &session.id,
                                "tool",
                                &output.content,
                                serde_json::json!([]),
                                Some(call_id.clone()),
                            )
                            .await?;
                        conversation_ids.push(Some(stored.id));
                    }
                    conversation.push(ChatMessage::tool(call_id, output.content.clone()));
                }
                emit(
                    engine,
                    &session.id,
                    spec,
                    "agent.turn_finished",
                    serde_json::json!({"turn": turn + 1, "parallel": true}),
                )
                .await?;
                if let Some(runtime) = auto_budget.as_mut() {
                    let current_turn = runtime.current_turn();
                    if !runtime
                        .finish_turn(
                            engine,
                            session,
                            spec,
                            control,
                            budget,
                            current_turn,
                            true,
                        )
                        .await?
                    {
                        return Ok(LoopOutcome::Cancelled);
                    }
                }
                continue;
            }
        }

        if let Some(outcome) = super::tool_calls::execute_tool_calls(
            engine,
            config,
            session,
            project,
            control,
            budget,
            spec,
            journal,
            top_level,
            turn,
            tool_calls,
            conversation,
            conversation_ids,
            &mut citations,
            &mut active_skill,
            &mut deferred_wait,
            &mut pending_promotions,
            &mut team_plan,
            &mut turn_tool_bytes,
            &run_contract,
            &run_contract_policy,
            &mut run_contract_usage,
            &dynamic_mcp_tools,
            &gui_policy,
            &effect_policy,
            &tool_context,
            &plan_budget,
            &frozen_bundle,
            surface,
            &tools,
        )
        .await?
        {
            return Ok(outcome);
        }
        if let Some(runtime) = auto_budget.as_mut() {
            let current_turn = runtime.current_turn();
            if !runtime
                .finish_turn(
                    engine,
                    session,
                    spec,
                    control,
                    budget,
                    current_turn,
                    true,
                )
                .await?
            {
                return Ok(LoopOutcome::Cancelled);
            }
        }
    }

    Err(anyhow!("maximum turns reached"))
}

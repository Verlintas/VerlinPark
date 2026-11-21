use super::*;

pub(in crate::agent) const COMPACTION_SYSTEM_PROMPT: &str = "You are compacting an AI coding agent conversation. \
Summarize the transcript so another agent can continue without the full history. Preserve: \
the user's goal and constraints, decisions and their reasons, files and paths changed or inspected, \
important tool results and test outcomes, open tasks and blockers, and exact identifiers or commands \
that would be costly to rediscover. Be concise and factual. Do not invent work, results, or files. \
Return only the summary text.";

pub(in crate::agent) fn compaction_transcript(messages: &[ChatMessage], max_tokens: u64) -> String {
    const PER_MESSAGE: usize = 4_000;
    let mut rendered = Vec::new();
    let mut used = 0u64;
    let mut omitted = false;
    // Keep the most recent (most relevant) summarized messages when the
    // transcript would not fit the summarizer's own context window.
    for message in messages.iter().rev() {
        let mut part = String::new();
        part.push_str(&message.role);
        part.push_str(": ");
        part.push_str(&truncate_to(&message.content, PER_MESSAGE));
        part.push('\n');
        if let Some(tool_calls) = &message.tool_calls {
            part.push_str("tool_calls: ");
            part.push_str(&serde_json::to_string(tool_calls).unwrap_or_default());
            part.push('\n');
        }
        if let Some(tool_call_id) = &message.tool_call_id {
            part.push_str("tool_call_id: ");
            part.push_str(tool_call_id);
            part.push('\n');
        }
        let tokens = estimate_context_tokens(&part);
        if used.saturating_add(tokens) > max_tokens && !rendered.is_empty() {
            omitted = true;
            break;
        }
        used = used.saturating_add(tokens);
        rendered.push(part);
    }
    rendered.reverse();
    let mut transcript = String::new();
    if omitted {
        transcript.push_str("… older messages omitted at the compaction limit …\n");
    }
    transcript.extend(rendered);
    transcript
}

pub(in crate::agent) async fn collect_compaction_completion(
    provider: &dyn Provider,
    request: &ChatRequest,
    idle_timeout: Duration,
    control: &RunControl,
) -> Result<(String, TokenUsage)> {
    let mut stream = provider
        .stream_chat(request.clone())
        .await
        .context("starting context compaction stream")?;
    let mut cancel_rx = control.subscribe();
    let mut text = String::new();
    let mut usage = TokenUsage::default();
    loop {
        if control.is_cancelled() {
            anyhow::bail!("context compaction cancelled");
        }
        let item = tokio::select! {
            changed = cancel_rx.changed() => {
                if changed.is_err() || *cancel_rx.borrow() {
                    anyhow::bail!("context compaction cancelled");
                }
                continue;
            }
            item = tokio::time::timeout(idle_timeout, stream.next()) => match item {
                Ok(item) => item,
                Err(_) => anyhow::bail!(
                    "context compaction stream was idle for {}s",
                    idle_timeout.as_secs()
                ),
            },
        };
        let Some(item) = item else {
            break;
        };
        match item? {
            StreamEvent::TextDelta(delta) => {
                text.push_str(&delta);
                if text.len() >= 32_768 {
                    break;
                }
            }
            StreamEvent::Usage {
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
            } => {
                usage.add(
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    cache_write_tokens,
                );
            }
            StreamEvent::ToolCall(_) | StreamEvent::Done => {}
        }
    }
    Ok((text, usage))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::agent) struct PreparedSummaryKey {
    pub(in crate::agent) provider: String,
    pub(in crate::agent) model: String,
    pub(in crate::agent) prefix_hash: String,
    pub(in crate::agent) plan_revision: u32,
    pub(in crate::agent) cut: usize,
    pub(in crate::agent) cursor_hash: String,
}

impl PreparedSummaryKey {
    pub(in crate::agent) fn stable_id(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}",
            self.provider,
            self.model,
            self.prefix_hash,
            self.plan_revision,
            self.cut,
            self.cursor_hash
        )
    }
}

#[derive(Clone)]
pub(in crate::agent) struct PreparedSummary {
    pub(in crate::agent) key: PreparedSummaryKey,
    pub(in crate::agent) summary: String,
    pub(in crate::agent) input_tokens: u64,
    pub(in crate::agent) output_tokens: u64,
    pub(in crate::agent) estimated_usage: bool,
}

#[derive(Default)]
pub(in crate::agent) struct CompactionRuntime {
    pub(in crate::agent) prepared: Option<PreparedSummary>,
    pub(in crate::agent) failed_prepare_keys: HashSet<String>,
}

impl CompactionRuntime {
    /// Remove and return the prepared summary only when every key component
    /// still matches the live run and the exact message prefix is unchanged.
    pub(in crate::agent) fn take_reusable(
        &mut self,
        config: &AgentConfig,
        spec: &AgentSpec,
        prefix_hash: &str,
        plan_revision: u32,
        conversation: &[ChatMessage],
    ) -> (Option<PreparedSummary>, bool) {
        let was_present = self.prepared.is_some();
        match self.prepared.take() {
            Some(prepared)
                if prepared.key.provider == config.provider_name
                    && prepared.key.model == model_for(config, spec.role)
                    && prepared.key.prefix_hash == prefix_hash
                    && prepared.key.plan_revision == plan_revision
                    && prepared.key.cut <= conversation.len()
                    && message_cursor_hash(conversation, prepared.key.cut)
                        == prepared.key.cursor_hash =>
            {
                (Some(prepared), true)
            }
            Some(_) => (None, was_present),
            None => (None, false),
        }
    }
}

pub(in crate::agent) fn prepared_summary_key(
    config: &AgentConfig,
    spec: &AgentSpec,
    prefix_hash: &str,
    plan_revision: u32,
    conversation: &[ChatMessage],
    cut: usize,
) -> PreparedSummaryKey {
    PreparedSummaryKey {
        provider: config.provider_name.clone(),
        model: model_for(config, spec.role),
        prefix_hash: prefix_hash.to_string(),
        plan_revision,
        cut,
        cursor_hash: message_cursor_hash(conversation, cut),
    }
}

pub(in crate::agent) struct SummaryCall {
    pub(in crate::agent) summary: String,
    pub(in crate::agent) input_tokens: u64,
    pub(in crate::agent) output_tokens: u64,
    pub(in crate::agent) estimated: bool,
}

/// Run exactly one summarizer call under the session budget. `Ok(None)` means
/// a token reservation could not be granted; the caller falls back to a rolling
/// trim. This never starts a second agent run.
#[allow(clippy::too_many_arguments)]
pub(in crate::agent) async fn run_compaction_summary(
    engine: &Engine,
    config: &AgentConfig,
    session: &Session,
    spec: &AgentSpec,
    budget: &SessionBudget,
    control: &RunControl,
    transcript: String,
) -> Result<Option<SummaryCall>> {
    if transcript.trim().is_empty() {
        return Ok(None);
    }
    let request = ChatRequest {
        model: model_for(config, spec.role),
        messages: vec![ChatMessage::user(transcript)],
        system: Some(COMPACTION_SYSTEM_PROMPT.to_string()),
        system_stable_prefix: Some(COMPACTION_SYSTEM_PROMPT.to_string()),
        system_sections: Vec::new(),
        tail_sections: Vec::new(),
        cache: None,
        max_tokens: Some(1024),
        temperature: Some(0.2),
        tools: Vec::new(),
    };
    let reservation = budget
        .token_limit()
        .is_some()
        .then(|| budget.reserve_tokens(estimate_request_tokens(&request).saturating_add(1024)))
        .flatten();
    if !(budget.is_unlimited() || budget.token_limit().is_none() || reservation.is_some()) {
        return Ok(None);
    }
    let outcome = collect_compaction_completion(
        config.provider.as_ref(),
        &request,
        Duration::from_secs(config.provider_idle_timeout_secs),
        control,
    )
    .await;
    let (summary, usage) = match outcome {
        Ok(value) => value,
        Err(error) => {
            drop(reservation);
            return Err(error);
        }
    };
    let summary = summary.trim();
    if summary.is_empty() {
        drop(reservation);
        return Ok(None);
    }
    let (input_tokens, output_tokens, cache_read, cache_write, estimated_usage) =
        if !usage.is_empty() {
            (
                usage.input,
                usage.output,
                usage.cache_read,
                usage.cache_write,
                false,
            )
        } else {
            (
                estimate_request_tokens(&request),
                estimate_text_tokens(summary),
                0,
                0,
                true,
            )
        };
    let cost = usage_cost(
        input_tokens,
        output_tokens,
        cache_read,
        cache_write,
        role_prices(config, spec.role),
    );
    engine
        .record_agent_usage_with_cache(
            &session.id,
            &spec.id,
            &spec.name,
            "compaction",
            spec.task_id.clone(),
            spec.department.clone(),
            &request.model,
            input_tokens,
            output_tokens,
            cache_read,
            cache_write,
            estimated_usage,
            cost,
        )
        .await?;
    if let Some(reservation) = reservation {
        let _ = reservation.settle(input_tokens.saturating_add(output_tokens));
    }
    budget.add_cost(cost);
    Ok(Some(SummaryCall {
        summary: summary.to_string(),
        input_tokens,
        output_tokens,
        estimated: estimated_usage,
    }))
}

/// Keep the active context under the model window. The primary strategy is a
/// persistent summary plus a retained tail. If summarization is unavailable,
/// fails, or the summarized context is still above the hard limit, older
/// messages are dropped at safe user/assistant boundaries as a rolling-window
/// fallback. Original messages remain in SQLite for search and audit.
///
/// A prepared summary is a bounded in-run cache, not a background agent run.
/// It is prepared at `prepare_ratio * compact_threshold`, keyed by provider,
/// model, stable prefix hash, plan revision, and the exact message-prefix
/// cursor. It is reused only if every key component still matches; the swap
/// itself is synchronous.
#[allow(clippy::too_many_arguments)]
pub(in crate::agent) async fn compact_context_if_needed(
    engine: &Engine,
    config: &AgentConfig,
    session: &Session,
    spec: &AgentSpec,
    budget: &SessionBudget,
    control: &RunControl,
    conversation: &mut Vec<ChatMessage>,
    conversation_ids: &mut Vec<Option<String>>,
    compact_threshold: u64,
    hard_limit: u64,
    base_tokens: u64,
    calibration: f64,
    force: bool,
    prefix_hash: &str,
    prepare_ratio: f64,
    runtime: &mut CompactionRuntime,
    strategy: ContextStrategy,
) -> Result<bool> {
    let started = Instant::now();
    if budget.exhausted() {
        return Ok(false);
    }
    if !force && compact_threshold == 0 && hard_limit == u64::MAX {
        return Ok(false);
    }
    let keep_messages = config.compact_keep_messages.max(2);
    let keep_tokens = config.compact_keep_tokens;
    if conversation.len() <= keep_messages {
        return Ok(false);
    }
    // Thresholds and the hard limit describe the whole active request, so the
    // comparison must include the system prompt and tool schemas too.
    let estimated_before = calibrate_context_tokens(
        estimate_messages_tokens(conversation).saturating_add(base_tokens),
        calibration,
    );
    let triggered = match strategy {
        ContextStrategy::Summarize => {
            estimated_before >= compact_threshold || estimated_before > hard_limit
        }
        ContextStrategy::Trim => estimated_before > hard_limit,
    };
    let prepare_threshold = if compact_threshold > 0 {
        (compact_threshold as f64 * prepare_ratio.clamp(0.3, 0.95)).round() as u64
    } else {
        0
    };
    if !force && !triggered {
        if strategy == ContextStrategy::Summarize
            && prepare_threshold > 0
            && estimated_before >= prepare_threshold
            && let Some(cut) =
                safe_context_cut_with_tokens(conversation, keep_messages, keep_tokens, 0)
        {
            let state = load_compaction_structured_state(engine, session, spec).await;
            let key = prepared_summary_key(
                config,
                spec,
                prefix_hash,
                state.plan_revision,
                conversation,
                cut,
            );
            let already_valid = runtime
                .prepared
                .as_ref()
                .is_some_and(|prepared| prepared.key == key);
            if !already_valid && !runtime.failed_prepare_keys.contains(&key.stable_id()) {
                let transcript = compaction_transcript(
                    &conversation[..cut],
                    compaction_transcript_budget(config, spec, calibration),
                );
                match run_compaction_summary(
                    engine, config, session, spec, budget, control, transcript,
                )
                .await
                {
                    Ok(Some(call)) => {
                        runtime.prepared = Some(PreparedSummary {
                            key: key.clone(),
                            summary: call.summary,
                            input_tokens: call.input_tokens,
                            output_tokens: call.output_tokens,
                            estimated_usage: call.estimated,
                        });
                        emit(
                            engine,
                            &session.id,
                            spec,
                            "context.compaction_prepared",
                            serde_json::json!({
                                "cut_messages": cut,
                                "plan_revision": key.plan_revision,
                                "model": key.model,
                                "prefix_hash": key.prefix_hash,
                                "cursor_hash": key.cursor_hash,
                                "compaction_threshold": compact_threshold,
                                "prepare_threshold": prepare_threshold,
                                "input_tokens": call.input_tokens,
                                "output_tokens": call.output_tokens,
                                "estimated": call.estimated,
                            }),
                        )
                        .await?;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        runtime.failed_prepare_keys.insert(key.stable_id());
                        emit(
                            engine,
                            &session.id,
                            spec,
                            "context.compaction_prepare_failed",
                            serde_json::json!({
                                "cut_messages": cut,
                                "error": truncate(&format!("{error:#}")),
                            }),
                        )
                        .await?;
                    }
                }
            }
        }
        return Ok(false);
    }

    // Only a real compaction needs the project cache policy and the
    // pre-compaction generation. Keeping the lookup here avoids a database
    // query on every below-threshold turn.
    let cache_policy = engine
        .get_cache_policy(&session.project_id)
        .await
        .unwrap_or_default();
    let cache_generation_before =
        crate::prompt_cache::conversation_generation(prefix_hash, conversation);
    // The stable epoch is recorded, not recomputed: a mid-run compaction must
    // not silently re-derive an identity from changed in-memory state. A
    // manual compaction on a session with no prefix event records `None`
    // rather than inventing an epoch.
    let cache_epoch_before = engine
        .recent_events_of_kind(&session.id, "context.prompt_prefix", 1)
        .await?
        .into_iter()
        .last()
        .and_then(|event| {
            event
                .payload
                .get("cache_epoch")
                .and_then(serde_json::Value::as_str)
                .filter(|value| crate::cache_epoch::is_cache_epoch(value))
                .map(str::to_string)
        });
    let cache_epoch_after = cache_epoch_before.clone();
    let cache_epoch_recorded = cache_epoch_before.is_some();
    let conversation_tokens_before_cache = estimate_messages_tokens(conversation);
    let state = load_compaction_structured_state(engine, session, spec).await;
    let mut estimated = estimated_before;
    let mut summarized_messages = 0usize;
    let mut dropped_messages = 0usize;
    let mut has_model_summary = conversation
        .first()
        .is_some_and(|message| message.content.starts_with("Earlier conversation summary"));
    // Preserve a summary already loaded at the head of the conversation.
    let mut summary_text = if conversation_ids.first().is_some_and(Option::is_none) {
        conversation.first().map(|message| message.content.clone())
    } else {
        None
    };
    let mut through_id: Option<String> = None;
    let mut usage_tokens = (0u64, 0u64);
    let mut usage_estimated = true;
    let mut background_reuse = false;
    let mut fallback_to_sync = false;

    if strategy == ContextStrategy::Summarize
        && (force || (compact_threshold > 0 && estimated >= compact_threshold))
    {
        // A prepared summary covers a fixed prefix. Newer messages appended
        // after it stay in the retained tail, so reuse does not require the
        // current ideal cut to equal the prepared cut.
        let (prepared, had_prepared) =
            runtime.take_reusable(config, spec, prefix_hash, state.plan_revision, conversation);
        let cut = prepared
            .as_ref()
            .map(|prepared| prepared.key.cut)
            .or_else(|| safe_context_cut_with_tokens(conversation, keep_messages, keep_tokens, 0));
        fallback_to_sync = had_prepared && prepared.is_none();
        if let Some(cut) = cut
            && let Some(through) = if conversation_ids.is_empty() {
                Some(None)
            } else {
                conversation_ids
                    .get(cut.saturating_sub(1))
                    .cloned()
                    .flatten()
                    .map(Some)
            }
        {
            let state_block = render_structured_state(&state);
            let mut model_summary: Option<String> = None;
            let mut call_usage = None;
            if let Some(prepared) = prepared {
                model_summary = Some(prepared.summary);
                call_usage = Some((
                    prepared.input_tokens,
                    prepared.output_tokens,
                    prepared.estimated_usage,
                ));
                background_reuse = true;
            } else {
                let transcript = compaction_transcript(
                    &conversation[..cut],
                    compaction_transcript_budget(config, spec, calibration),
                );
                match run_compaction_summary(
                    engine, config, session, spec, budget, control, transcript,
                )
                .await
                {
                    Ok(Some(call)) => {
                        model_summary = Some(call.summary);
                        call_usage = Some((call.input_tokens, call.output_tokens, call.estimated));
                    }
                    Ok(None) => {}
                    Err(error) => {
                        emit(
                            engine,
                            &session.id,
                            spec,
                            "context.compact_failed",
                            serde_json::json!({
                                "error": truncate(&format!("{error:#}")),
                            }),
                        )
                        .await?;
                    }
                }
            }
            if let Some(model_summary) = model_summary {
                let prefixed = format!(
                    "Earlier conversation summary (auto-compacted from {cut} messages):\n{state_block}\n\n{model_summary}"
                );
                conversation.drain(..cut);
                conversation_ids.drain(..cut);
                conversation.insert(0, ChatMessage::user(prefixed.clone()));
                conversation_ids.insert(0, None);
                summarized_messages = cut;
                summary_text = Some(prefixed);
                through_id = through;
                has_model_summary = true;
                if let Some((input_tokens, output_tokens, estimated_usage)) = call_usage {
                    usage_tokens = (input_tokens, output_tokens);
                    usage_estimated = estimated_usage;
                }
                estimated = calibrate_context_tokens(
                    estimate_messages_tokens(conversation).saturating_add(base_tokens),
                    calibration,
                );
            }
        }
    }

    // Rolling-window fallback. The structured state block is authoritative and
    // is preserved even when summarization failed; a trim-only compaction
    // still carries it at index 0.
    let trim_target = if force && !has_model_summary {
        (hard_limit / 2).max(1_024).min(estimated.saturating_sub(1))
    } else {
        hard_limit
    };
    let state_block = render_structured_state(&state);
    // A pathological tiny model window cannot fit the authoritative block and
    // a usable tail. Preserve the block when it fits; otherwise fall back to a
    // pure rolling trim rather than making the request larger than the window.
    let state_can_fit = hard_limit == u64::MAX
        || estimate_context_tokens(&state_block).saturating_add(base_tokens)
            <= hard_limit.saturating_div(2);
    if estimated > trim_target && state_can_fit {
        if let Some(existing) = summary_text.as_mut() {
            *existing = refresh_structured_state(existing, &state_block);
            if let Some(first) = conversation.first_mut() {
                first.content = existing.clone();
            }
        } else {
            conversation.insert(0, ChatMessage::user(state_block.clone()));
            conversation_ids.insert(0, None);
            summary_text = Some(state_block);
        }
        estimated = calibrate_context_tokens(
            estimate_messages_tokens(conversation).saturating_add(base_tokens),
            calibration,
        );
    }
    let floor = usize::from(summary_text.is_some());
    let mut retention = keep_messages;
    loop {
        if estimated <= trim_target {
            break;
        }
        let cut = match safe_context_cut_with_tokens(conversation, retention, keep_tokens, floor) {
            Some(cut) => cut,
            None if retention > 2 => {
                retention = 2;
                continue;
            }
            None => break,
        };
        if cut <= floor {
            break;
        }
        let through = conversation_ids.get(cut - 1).cloned().flatten();
        if !conversation_ids.is_empty() && through.is_none() {
            if retention > 2 {
                retention = 2;
                continue;
            }
            break;
        }
        let removed = cut - floor;
        conversation.drain(floor..cut);
        conversation_ids.drain(floor..cut);
        dropped_messages = dropped_messages.saturating_add(removed);
        if through.is_some() {
            through_id = through;
        }
        estimated = calibrate_context_tokens(
            estimate_messages_tokens(conversation).saturating_add(base_tokens),
            calibration,
        );
    }

    if summarized_messages == 0 && dropped_messages == 0 {
        return Ok(false);
    }
    let mode = if has_model_summary { "summary" } else { "trim" };
    let kept_messages = conversation.len();
    let latency_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
    let cache_generation_after =
        crate::prompt_cache::conversation_generation(prefix_hash, conversation);
    let cache_invalidation_tokens = if cache_policy.compact_aware {
        conversation_tokens_before_cache.saturating_sub(estimate_messages_tokens(conversation))
    } else {
        0
    };
    let prices = role_prices(config, spec.role);
    let cache_invalidation_estimated_usd = if cache_policy.compact_aware {
        crate::prompt_cache::round_usd(
            cache_invalidation_tokens as f64 * (prices.input - prices.cache_read).max(0.0)
                / 1_000_000.0,
        )
    } else {
        0.0
    };
    // L4 always accounts for the boundary, even when the legacy
    // `compact_aware` switch is off. The estimate is zero when prices are not
    // configured, and it is explicitly an estimate, not an invoice.
    let invalidated_tokens_all =
        conversation_tokens_before_cache.saturating_sub(estimate_messages_tokens(conversation));
    let estimated_lost_cache_read_usd = crate::prompt_cache::round_usd(
        invalidated_tokens_all as f64 * (prices.input - prices.cache_read).max(0.0) / 1_000_000.0,
    );
    let compaction_count = engine
        .count_events_of_kind(&session.id, "context.compacted")
        .await?
        .saturating_add(1);
    let checkpoint_id = uuid::Uuid::now_v7().to_string();
    let checkpoint_plan_revision = state.plan_revision;
    let checkpoint_structured_state = state.clone();
    // The contract capsule is a head section, so compaction cannot drop it.
    // Record its identity on the boundary anyway: a changed contract after a
    // compaction is a priced prefix change, not silent churn.
    let compact_contract = engine
        .latest_task_contract(&session.id, spec.task_id.as_deref())
        .await
        .ok()
        .flatten()
        .filter(|contract| contract.role == spec.role.as_str());
    let compact_contract_hash = compact_contract
        .as_ref()
        .map(|contract| contract.hash.clone());
    let compact_contract_chars = compact_contract
        .as_ref()
        .map(|contract| contract.capsule().chars().count() as u64);
    let compact_contract_enabled = engine
        .get_contract_policy(&session.project_id)
        .await
        .map(|policy| policy.enabled)
        .unwrap_or(false);
    let checkpoint_summary = summary_text.clone();
    let checkpoint_through_id = through_id.clone();
    let checkpoint_mode = if mode == "summary" && checkpoint_summary.is_none() {
        "trim"
    } else {
        mode
    };
    let mut compaction_payload = serde_json::json!({
        "mode": mode,
        "through_message_id": through_id,
        "summary": summary_text,
        "summarized_messages": summarized_messages,
        "dropped_messages": dropped_messages,
        "kept_messages": kept_messages,
        "estimated_tokens_before": estimated_before,
        "estimated_tokens_after": estimated,
        "threshold_tokens": compact_threshold,
        "hard_limit_tokens": hard_limit,
        "calibration": calibration,
        "input_tokens": usage_tokens.0,
        "output_tokens": usage_tokens.1,
        "estimated": usage_estimated,
        "latency_ms": latency_ms,
        "background_reuse": background_reuse,
        "fallback_to_sync": fallback_to_sync,
        "compaction_count": compaction_count,
        "structured_state": state,
        "background_prepared": runtime.prepared.is_some(),
        "cache_generation_before": cache_generation_before,
        "cache_generation_after": cache_generation_after,
        "cache_reset_reason": if cache_policy.compact_aware {
            Some("compaction")
        } else {
            None
        },
        "cache_invalidation_tokens": cache_invalidation_tokens,
        "cache_invalidation_estimated_usd": cache_invalidation_estimated_usd,
        "cache_compact_aware": cache_policy.compact_aware,
        "cache_epoch_before": cache_epoch_before.clone(),
        "cache_epoch_after": cache_epoch_after.clone(),
        "cache_epoch_changed": false,
        "cache_epoch_source": if cache_epoch_recorded { "recorded" } else { "unavailable" },
        "estimated_lost_cache_read_usd": estimated_lost_cache_read_usd,
        "contract_hash": compact_contract_hash,
        "contract_chars": compact_contract_chars,
        "contract_capsule": compact_contract_hash.is_some(),
        "contract_policy_enabled": compact_contract_enabled,
    });
    agent_fields(&mut compaction_payload, spec);
    let checkpoint = ContextCheckpoint {
        id: checkpoint_id,
        session_id: session.id.clone(),
        project_id: session.project_id.clone(),
        label: format!("auto {checkpoint_mode} #{compaction_count}"),
        mode: checkpoint_mode.to_string(),
        summary: checkpoint_summary,
        through_message_id: checkpoint_through_id,
        message_cursor: 0,
        plan_revision: checkpoint_plan_revision,
        provider: config.provider_name.clone(),
        model: model_for(config, spec.role),
        prompt_prefix_hash: prefix_hash.to_string(),
        worktree_head: context_worktree_head(session.worktree.as_deref()),
        tokens_before: estimated_before,
        tokens_after: estimated,
        structured_state: Some(checkpoint_structured_state),
        source_compaction_seq: None,
        automatic: true,
        created_at: vp_protocol::now_rfc3339(),
    };
    // The compaction boundary and its durable checkpoint are one transaction:
    // replay must never observe a compaction without its checkpoint reference.
    engine
        .append_context_compaction_with_checkpoint(checkpoint, compaction_payload)
        .await?;
    // Operational metrics event. It deliberately omits the summary text; the
    // compatibility event above remains the audit record.
    emit(
        engine,
        &session.id,
        spec,
        "context.compaction",
        serde_json::json!({
            "mode": mode,
            "latency_ms": latency_ms,
            "estimated_tokens_before": estimated_before,
            "estimated_tokens_after": estimated,
            "summarized_messages": summarized_messages,
            "dropped_messages": dropped_messages,
            "kept_messages": kept_messages,
            "threshold_tokens": compact_threshold,
            "hard_limit_tokens": hard_limit,
            "input_tokens": usage_tokens.0,
            "output_tokens": usage_tokens.1,
            "estimated": usage_estimated,
            "background_reuse": background_reuse,
            "fallback_to_sync": fallback_to_sync,
            "compaction_count": compaction_count,
            "cache_generation_before": cache_generation_before,
            "cache_generation_after": cache_generation_after,
            "cache_reset_reason": if cache_policy.compact_aware {
                Some("compaction")
            } else {
                None
            },
            "cache_invalidation_tokens": cache_invalidation_tokens,
            "cache_invalidation_estimated_usd": cache_invalidation_estimated_usd,
            "cache_compact_aware": cache_policy.compact_aware,
            "cache_epoch_before": cache_epoch_before.clone(),
            "cache_epoch_after": cache_epoch_after.clone(),
            "cache_epoch_changed": false,
            "cache_epoch_source": if cache_epoch_recorded { "recorded" } else { "unavailable" },
            "estimated_lost_cache_read_usd": estimated_lost_cache_read_usd,
            "invalidated_tokens": invalidated_tokens_all,
        }),
    )
    .await?;
    // Bounded L4 accounting event. It contains counts, epochs, and USD
    // estimates only; the summary text stays on `context.compacted`.
    emit(
        engine,
        &session.id,
        spec,
        "context.compaction.cache_reset",
        serde_json::json!({
            "mode": mode,
            "compaction_count": compaction_count,
            "cache_epoch_before": cache_epoch_before,
            "cache_epoch_after": cache_epoch_after,
            "cache_epoch_changed": false,
            "cache_epoch_source": if cache_epoch_recorded { "recorded" } else { "unavailable" },
            "cache_generation_before": cache_generation_before,
            "cache_generation_after": cache_generation_after,
            "reset_reason": "compaction",
            "conversation_tokens_before": conversation_tokens_before_cache,
            "conversation_tokens_after": estimate_messages_tokens(conversation),
            "invalidated_tokens": invalidated_tokens_all,
            "estimated_lost_cache_read_tokens": invalidated_tokens_all,
            "estimated_lost_cache_read_usd": estimated_lost_cache_read_usd,
            "accounting_enabled": cache_policy.compact_aware,
            "compact_aware": cache_policy.compact_aware,
            "threshold_tokens": compact_threshold,
            "hard_limit_tokens": hard_limit,
            "background_reuse": background_reuse,
        }),
    )
    .await?;
    Ok(true)
}

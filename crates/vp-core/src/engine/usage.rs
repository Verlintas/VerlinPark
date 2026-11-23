use super::*;

impl Engine {
    /// Bounded cross-session cache-epoch observation for `session.cache`.
    /// This is a read-only scan of recent `context.cache` events in other
    /// sessions; it never calls a provider and never claims a guaranteed hit.
    pub async fn cache_epoch_cross_session(
        &self,
        epoch: &str,
        exclude_session: &str,
    ) -> Result<crate::prompt_cache::CacheEpochReuse> {
        const CROSS_SESSION_SCAN_LIMIT: u32 = 5_000;
        let store = self.store.clone();
        let epoch = epoch.to_string();
        let exclude = exclude_session.to_string();
        let (sessions, hit_tokens, calls) = tokio::task::spawn_blocking(move || {
            store.cache_epoch_cross_session(&epoch, &exclude, CROSS_SESSION_SCAN_LIMIT)
        })
        .await??;
        Ok(crate::prompt_cache::CacheEpochReuse {
            sessions,
            hit_tokens,
            calls,
        })
    }

    pub async fn record_usage(&self, session_id: &str, input: u64, output: u64) -> Result<()> {
        self.record_usage_with_source(session_id, input, output, false)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn record_agent_usage(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        role: &str,
        task_id: Option<String>,
        department: Option<String>,
        model: &str,
        input: u64,
        output: u64,
        estimated: bool,
        cost_usd: f64,
    ) -> Result<UsageEntry> {
        self.record_agent_usage_with_cache(
            session_id, agent_id, agent_name, role, task_id, department, model, input, output, 0,
            0, estimated, cost_usd,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn record_agent_usage_with_cache(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        role: &str,
        task_id: Option<String>,
        department: Option<String>,
        model: &str,
        input: u64,
        output: u64,
        cache_read: u64,
        cache_write: u64,
        estimated: bool,
        cost_usd: f64,
    ) -> Result<UsageEntry> {
        let cost_usd = if cost_usd.is_finite() && cost_usd > 0.0 {
            cost_usd
        } else {
            0.0
        };
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let agent_id = agent_id.to_string();
        let agent_name = agent_name.to_string();
        let role = role.to_string();
        let model = model.to_string();
        let (entry, events) = tokio::task::spawn_blocking(move || {
            store.add_usage_entry_with_events(
                &session_id,
                &agent_id,
                &agent_name,
                &role,
                task_id.as_deref(),
                department.as_deref(),
                &model,
                input,
                output,
                cache_read,
                cache_write,
                estimated,
                cost_usd,
            )
        })
        .await??;
        for event in events {
            let _ = self.events.send(event);
        }
        Ok(entry)
    }

    pub async fn usage_ledger(&self, session_id: &str) -> Result<UsageLedgerResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let entries = tokio::task::spawn_blocking(move || store.list_usage_entries(&id)).await??;
        let mut totals = UsageTotals {
            calls: entries.len() as u64,
            ..UsageTotals::default()
        };
        let mut by_role = HashMap::new();
        let mut by_model = HashMap::new();
        let mut by_agent = HashMap::new();
        let mut by_task = HashMap::new();
        let mut by_department = HashMap::new();
        for entry in &entries {
            totals.input_tokens = totals.input_tokens.saturating_add(entry.input_tokens);
            totals.output_tokens = totals.output_tokens.saturating_add(entry.output_tokens);
            totals.cache_read_tokens = totals
                .cache_read_tokens
                .saturating_add(entry.cache_read_tokens);
            totals.cache_write_tokens = totals
                .cache_write_tokens
                .saturating_add(entry.cache_write_tokens);
            totals.cost_usd += entry.cost_usd;
            add_usage_bucket(&mut by_role, &entry.role, entry);
            add_usage_bucket(&mut by_model, &entry.model, entry);
            add_usage_bucket(&mut by_agent, &entry.agent_name, entry);
            add_usage_bucket(
                &mut by_task,
                entry.task_id.as_deref().unwrap_or("(unassigned)"),
                entry,
            );
            add_usage_bucket(
                &mut by_department,
                entry.department.as_deref().unwrap_or("(unassigned)"),
                entry,
            );
        }
        totals.by_role = sorted_usage_buckets(by_role);
        totals.by_model = sorted_usage_buckets(by_model);
        totals.by_agent = sorted_usage_buckets(by_agent);
        totals.by_task = sorted_usage_buckets(by_task);
        totals.by_department = sorted_usage_buckets(by_department);
        totals.cache_hit_ratio = vp_protocol::types::cache_hit_ratio(
            totals.input_tokens,
            totals.cache_read_tokens,
            totals.cache_write_tokens,
        );
        Ok(UsageLedgerResult {
            session_id: session_id.to_string(),
            entries,
            totals,
        })
    }

    pub async fn usage_csv(&self, session_id: &str) -> Result<UsageCsvResult> {
        let ledger = self.usage_ledger(session_id).await?;
        let mut csv = String::from(
            "created_at,agent_name,role,task_id,department,model,input_tokens,cache_read_tokens,cache_write_tokens,output_tokens,total_tokens,estimated,cost_usd,cache_hit_ratio\n",
        );
        for entry in ledger.entries {
            let ratio = vp_protocol::types::cache_hit_ratio(
                entry.input_tokens,
                entry.cache_read_tokens,
                entry.cache_write_tokens,
            );
            let fields = [
                entry.created_at,
                entry.agent_name,
                entry.role,
                entry.task_id.unwrap_or_default(),
                entry.department.unwrap_or_default(),
                entry.model,
                entry.input_tokens.to_string(),
                entry.cache_read_tokens.to_string(),
                entry.cache_write_tokens.to_string(),
                entry.output_tokens.to_string(),
                (entry.input_tokens + entry.output_tokens).to_string(),
                entry.estimated.to_string(),
                format!("{:.6}", entry.cost_usd),
                format!("{ratio:.6}"),
            ];
            csv.push_str(
                &fields
                    .iter()
                    .map(|field| csv_field(field))
                    .collect::<Vec<_>>()
                    .join(","),
            );
            csv.push('\n');
        }
        Ok(UsageCsvResult {
            session_id: session_id.to_string(),
            csv,
        })
    }

    /// Bounded cache telemetry derived from the usage ledger and the latest
    /// context prompt-prefix/measurement/cache events. Never returns prompt
    /// text or provider secrets.
    /// Append a cache call event and its epoch observation atomically. The
    /// store computes cross-session fields inside the same transaction so the
    /// event payload and projection cannot disagree after a commit.
    pub async fn append_cache_call_event(
        &self,
        session_id: &str,
        kind: &str,
        payload: serde_json::Value,
        call: &PromptPrefixCall,
    ) -> Result<Event> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let kind = kind.to_string();
        let call = call.clone();
        let (event, _record) = tokio::task::spawn_blocking(move || {
            store.append_cache_call_event(&id, &kind, payload, &call)
        })
        .await??;
        let _ = self.events.send(event.clone());
        Ok(event)
    }

    /// Projection view of one epoch for one session. A read, not a claim.
    pub async fn cache_epoch_reuse(
        &self,
        project_id: &str,
        cache_epoch: &str,
        session_id: &str,
    ) -> Result<crate::cache_epoch::CacheEpochReuse> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        let cache_epoch = cache_epoch.to_string();
        let session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || {
            let aggregate = store.prompt_prefix_aggregate(&project_id, &cache_epoch)?;
            let observation =
                store.prompt_prefix_observation(&project_id, &cache_epoch, &session_id)?;
            Ok(crate::cache_epoch::reuse_from_projection(
                aggregate.as_ref(),
                observation.as_ref(),
            ))
        })
        .await?
    }

    pub async fn session_cache_status(&self, session_id: &str) -> Result<SessionCacheStatus> {
        const CACHE_CALL_SCAN_LIMIT: u32 = 2_000;
        const RECENT_CALLS: usize = 20;
        const RECENT_BREAKS: usize = 20;
        let ledger = self.usage_ledger(session_id).await?;
        let prefix_event = self
            .latest_event_of_kind(session_id, "context.prompt_prefix")
            .await?;
        let measured_event = self
            .latest_event_of_kind(session_id, "context.measured")
            .await?;
        let cache_events = self
            .recent_events_of_kind(session_id, "context.cache", CACHE_CALL_SCAN_LIMIT)
            .await?;
        let break_events = self
            .recent_events_of_kind(session_id, "context.cache_break", 500)
            .await?;
        let compaction_events = self
            .recent_events_of_kind(session_id, "context.compaction", CACHE_CALL_SCAN_LIMIT)
            .await?;
        let cache_reset_events = self
            .recent_events_of_kind(
                session_id,
                "context.compaction.cache_reset",
                CACHE_CALL_SCAN_LIMIT,
            )
            .await?;
        // L4 reset events are authoritative when present. Older sessions only
        // have `context.compaction`, whose reset fields are policy-gated; using
        // both would double-count the same boundary.
        let lifecycle_events = if cache_reset_events.is_empty() {
            compaction_events
        } else {
            cache_reset_events
        };
        let aggregate =
            crate::prompt_cache::aggregate_cache_events(&cache_events, &lifecycle_events);
        let continuity = crate::conversation::aggregate_continuity(&cache_events, &break_events);

        let prompt_prefix = prefix_event
            .as_ref()
            .and_then(|event| event.payload.get("prefix_hash"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                cache_events
                    .last()
                    .and_then(|event| event.payload.get("prompt_prefix_hash"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .or_else(|| {
                measured_event
                    .as_ref()
                    .and_then(|event| event.payload.get("prompt_prefix_hash"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            });
        let hash = |key: &str| {
            prefix_event
                .as_ref()
                .and_then(|event| event.payload.get(key))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        };
        let preamble_tokens_estimate = prefix_event.as_ref().and_then(|event| {
            event
                .payload
                .get("preamble_tokens_estimate")
                .and_then(serde_json::Value::as_u64)
                .or_else(|| {
                    event
                        .payload
                        .get("stable_prefix_chars")
                        .and_then(serde_json::Value::as_u64)
                        .map(|chars| chars / 4)
                })
        });
        let estimated_savings_usd = crate::prompt_cache::round_usd(
            cache_events
                .iter()
                .filter_map(|event| {
                    event
                        .payload
                        .get("estimated_savings_usd")
                        .and_then(serde_json::Value::as_f64)
                })
                .sum::<f64>(),
        );
        let last_recorded = cache_events.last();
        // Warm calls are real provider calls and count in totals, but "last
        // miss" should describe the latest ordinary agent call so a warm does
        // not hide the most recent production miss.
        let last_cache = cache_events
            .iter()
            .rev()
            .find(|event| {
                event
                    .payload
                    .get("warm")
                    .and_then(serde_json::Value::as_bool)
                    != Some(true)
            })
            .or(last_recorded);
        let last_call_at = ledger
            .entries
            .last()
            .map(|entry| entry.created_at.clone())
            .or_else(|| last_recorded.map(|event| event.created_at.clone()))
            .or_else(|| measured_event.map(|event| event.created_at));
        let cache_calls = cache_events
            .iter()
            .filter(|event| {
                event
                    .payload
                    .get("warm")
                    .and_then(serde_json::Value::as_bool)
                    != Some(true)
            })
            .count() as u64;
        let recent_calls = cache_events
            .iter()
            .rev()
            .take(RECENT_CALLS)
            .map(|event| crate::prompt_cache::cache_call_record(&event.payload, &event.created_at))
            .collect::<Vec<_>>();
        let recent_breaks = break_events
            .iter()
            .rev()
            .take(RECENT_BREAKS)
            .map(|event| crate::prompt_cache::cache_break_record(&event.payload, &event.created_at))
            .collect::<Vec<_>>();
        let session_store = self.store.clone();
        let lookup_id = session_id.to_string();
        let project_id = tokio::task::spawn_blocking(move || session_store.get_session(&lookup_id))
            .await??
            .map(|session| session.project_id);
        let cache_epoch = prefix_event
            .as_ref()
            .and_then(|event| crate::cache_epoch::finalized_epoch_hash(&event.payload))
            .map(str::to_string)
            .or_else(|| {
                last_cache
                    .and_then(|event| crate::cache_epoch::finalized_epoch_hash(&event.payload))
                    .map(str::to_string)
            });
        let (epoch_aggregate, epoch_observation) =
            match (project_id.as_deref(), cache_epoch.as_deref()) {
                (Some(project_id), Some(epoch)) => {
                    let store = self.store.clone();
                    let project = project_id.to_string();
                    let epoch = epoch.to_string();
                    let session = session_id.to_string();
                    tokio::task::spawn_blocking(move || {
                        let aggregate = store.prompt_prefix_aggregate(&project, &epoch)?;
                        let observation =
                            store.prompt_prefix_observation(&project, &epoch, &session)?;
                        Ok::<_, anyhow::Error>((aggregate, observation))
                    })
                    .await??
                }
                _ => (None, None),
            };
        let policy = match project_id {
            Some(project_id) => self.get_cache_policy(&project_id).await.ok(),
            None => None,
        };
        let latest_cache_epoch = aggregate.latest_cache_epoch.clone().or_else(|| {
            prefix_event
                .as_ref()
                .and_then(|event| event.payload.get("cache_epoch"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        });
        let cross_session = match latest_cache_epoch.as_deref() {
            Some(epoch) => self
                .cache_epoch_cross_session(epoch, session_id)
                .await
                .unwrap_or_default(),
            None => crate::prompt_cache::CacheEpochReuse::default(),
        };
        let role_costs = ledger
            .totals
            .by_role
            .iter()
            .map(|bucket| (bucket.key.clone(), bucket.cost_usd))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut per_role = aggregate.per_role.clone();
        for bucket in &mut per_role {
            if let Some(cost) = role_costs.get(&bucket.role) {
                bucket.cost_usd = crate::prompt_cache::round_usd(*cost);
            }
        }
        let cache_miss_tokens = crate::prompt_cache::cache_miss_tokens(
            ledger.totals.input_tokens,
            ledger.totals.cache_read_tokens,
        );

        let epoch_reuse = crate::cache_epoch::reuse_from_projection(
            epoch_aggregate.as_ref(),
            epoch_observation.as_ref(),
        );
        let epoch_session_count = epoch_aggregate
            .as_ref()
            .map(|row| row.session_count)
            .unwrap_or(0);
        let identity_sections = prefix_event
            .as_ref()
            .and_then(|event| event.payload.get("identity_sections"))
            .and_then(|value| {
                serde_json::from_value::<Vec<vp_protocol::types::PromptSectionRef>>(value.clone())
                    .ok()
            })
            .unwrap_or_default();

        Ok(SessionCacheStatus {
            session_id: session_id.to_string(),
            cache_epoch: cache_epoch.clone(),
            cache_epoch_source: cache_epoch.as_ref().and_then(|_| {
                prefix_event
                    .as_ref()
                    .and_then(|event| event.payload.get("cache_epoch_source"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            }),
            cache_epoch_first_seen: epoch_observation.as_ref().map(|row| row.first_seen.clone()),
            cache_epoch_last_seen: epoch_observation.as_ref().map(|row| row.last_seen.clone()),
            cache_epoch_project_first_seen: epoch_aggregate
                .as_ref()
                .map(|row| row.first_seen.clone()),
            cache_epoch_project_last_seen: epoch_aggregate
                .as_ref()
                .map(|row| row.last_seen.clone()),
            cache_epoch_core_sections: epoch_aggregate
                .as_ref()
                .map(|row| row.core_sections.clone())
                .unwrap_or_default(),
            cache_epoch_project_sections: epoch_aggregate
                .as_ref()
                .map(|row| row.project_sections.clone())
                .unwrap_or_default(),
            cache_epoch_identity_hash: hash("identity_hash").filter(|value| !value.is_empty()),
            cache_epoch_identity_id: hash("identity_id").filter(|value| !value.is_empty()),
            cache_epoch_identity_generation: prefix_event.as_ref().and_then(|event| {
                event
                    .payload
                    .get("identity_generation")
                    .and_then(serde_json::Value::as_u64)
                    .filter(|generation| *generation > 0)
            }),
            cache_epoch_identity_sections: identity_sections,
            cache_epoch_session_count: epoch_session_count,
            cache_epoch_call_count: epoch_aggregate
                .as_ref()
                .map(|row| row.call_count)
                .unwrap_or(0),
            cache_epoch_observed_input_tokens: epoch_aggregate
                .as_ref()
                .map(|row| row.observed_input_tokens)
                .unwrap_or(0),
            cache_epoch_observed_cache_read_tokens: epoch_aggregate
                .as_ref()
                .map(|row| row.observed_cache_read_tokens)
                .unwrap_or(0),
            cache_epochs_seen: aggregate.cache_epoch_counts.len() as u64,
            prefix_reused_from_sessions: last_cache
                .and_then(|event| event.payload.get("cache_epoch_reused_from_sessions"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(epoch_reuse.prior_sessions),
            cross_session_hit_tokens: aggregate.cross_session_hit_tokens,
            cross_session_first_calls: aggregate.cross_session_first_calls,
            provider_cache_misses: aggregate.provider_cache_misses,
            prefix_changes: aggregate.prefix_changes,
            compaction_cache_resets: aggregate.compaction_resets,
            compaction_lost_cache_usd: aggregate.compaction_lost_cache_usd,
            input_tokens: ledger.totals.input_tokens,
            cache_read_tokens: ledger.totals.cache_read_tokens,
            cache_write_tokens: ledger.totals.cache_write_tokens,
            output_tokens: ledger.totals.output_tokens,
            cache_hit_ratio: ledger.totals.cache_hit_ratio,
            cache_read_ratio: if ledger.totals.input_tokens == 0 {
                0.0
            } else {
                (ledger.totals.cache_read_tokens as f64 / ledger.totals.input_tokens as f64)
                    .clamp(0.0, 1.0)
            },
            cache_write_ratio: if ledger.totals.input_tokens == 0 {
                0.0
            } else {
                (ledger.totals.cache_write_tokens as f64 / ledger.totals.input_tokens as f64)
                    .clamp(0.0, 1.0)
            },
            estimated_savings_usd,
            cache_calls,
            prompt_prefix,
            cache_generation: last_cache
                .and_then(|event| event.payload.get("cache_generation"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .or_else(|| hash("cache_generation")),
            core_hash: hash("core_hash"),
            project_hash: hash("project_hash"),
            tools_hash: hash("tools_hash"),
            tool_bundle: hash("tool_bundle"),
            tool_bundle_hash: hash("tool_bundle_hash"),
            tool_bundle_tool_count: prefix_event.as_ref().and_then(|event| {
                event
                    .payload
                    .get("tool_bundle_tool_count")
                    .or_else(|| event.payload.get("tool_count"))
                    .and_then(serde_json::Value::as_u64)
            }),
            tool_bundle_mcp_count: prefix_event.as_ref().and_then(|event| {
                event
                    .payload
                    .get("tool_bundle_mcp_count")
                    .and_then(serde_json::Value::as_u64)
            }),
            volatile_hash: hash("volatile_hash"),
            preamble_tokens_estimate,
            last_call_at,
            last_breakpoints: last_cache
                .and_then(|event| event.payload.get("cache_breakpoints"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as u32,
            last_cache_ttl: last_cache
                .and_then(|event| event.payload.get("cache_ttl"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .or_else(|| policy.as_ref().map(|policy| policy.ttl.clone())),
            last_miss_reason: last_cache
                .and_then(|event| event.payload.get("miss_reason"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            last_first_token_ms: last_cache
                .and_then(|event| event.payload.get("first_token_ms"))
                .and_then(serde_json::Value::as_u64),
            last_message_prefix_hash: continuity.last_message_prefix_hash.or_else(|| {
                last_cache
                    .and_then(|event| event.payload.get("message_prefix_hash"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            }),
            last_previous_message_prefix_hash: continuity.last_previous_message_prefix_hash,
            non_append_only_calls: continuity.non_append_only_calls,
            last_message_count: continuity.last_message_count,
            last_tail_memory_mode: continuity.last_tail_memory_mode,
            provider_tail_system: continuity.provider_tail_system,
            last_tail_system_capability: continuity.last_tail_system_capability,
            cache_break_counts: continuity.break_reason_counts,
            recent_breaks,
            last_cache_break_at: continuity.last_break_at,
            last_message_sequence_issue_count: continuity.last_message_sequence_issue_count,
            last_message_sequence_issues: continuity.last_message_sequence_issues,
            message_sequence_issue_counts: continuity.message_sequence_issue_counts,
            recent_calls,
            miss_reason_counts: aggregate.miss_reason_counts,
            ttl_counts: aggregate.ttl_counts,
            breakpoint_counts: aggregate.breakpoint_counts,
            savings_by_role: aggregate.savings_by_role,
            first_token_p50_ms: aggregate.first_token_p50_ms,
            first_token_p95_ms: aggregate.first_token_p95_ms,
            compaction_resets: aggregate.compaction_resets,
            compaction_invalidation_tokens: aggregate.compaction_invalidation_tokens,
            compaction_invalidation_estimated_usd: aggregate.compaction_invalidation_estimated_usd,
            policy,
            cache_miss_tokens,
            theoretical_max_hit_ratio: aggregate.latest_theoretical_max_hit_ratio,
            append_only_calls: aggregate.append_only_calls,
            append_only_breaks: aggregate.append_only_breaks,
            last_append_only: aggregate.last_append_only,
            hit_ratio_p50: aggregate.hit_ratio_p50,
            hit_ratio_p95: aggregate.hit_ratio_p95,
            theoretical_max_p50: aggregate.theoretical_max_p50,
            theoretical_max_p95: aggregate.theoretical_max_p95,
            cost_usd: ledger.totals.cost_usd,
            cache_epoch_timeline: aggregate.cache_epoch_timeline.clone(),
            per_role,
            top_miss_reasons: aggregate.top_miss_reasons.clone(),
            cross_session_reuse_sessions: cross_session.sessions,
            cross_session_calls_observed: cross_session.calls,
        })
    }

    /// Bounded newest-first per-call cache table for `session.cache.calls`.
    ///
    /// Filters are exact and case-insensitive for role/reason. Warm calls are
    /// excluded by default. The scan is bounded to the newest 2,000 cache
    /// events; `truncated` reports only whether the requested limit cut the
    /// matching rows, not whether the underlying scan hit its bound.
    pub async fn session_cache_calls(
        &self,
        params: &vp_protocol::types::SessionCacheCallsParams,
    ) -> Result<vp_protocol::types::SessionCacheCallsResult> {
        const SCAN_LIMIT: u32 = 2_000;
        let limit = params.limit.unwrap_or(20).clamp(1, 100);
        let role_filter = params
            .role
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_ascii_lowercase);
        let reason_filter = params
            .miss_reason
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_ascii_lowercase);
        let events = self
            .recent_events_of_kind(&params.session_id, "context.cache", SCAN_LIMIT)
            .await?;
        let mut matching = Vec::new();
        for event in events.iter().rev() {
            let warm = event
                .payload
                .get("warm")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            if warm && !params.include_warm {
                continue;
            }
            let role = event
                .payload
                .get("agent_role")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            if let Some(filter) = role_filter.as_deref()
                && role != filter
            {
                continue;
            }
            let reason = event
                .payload
                .get("miss_reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            if let Some(filter) = reason_filter.as_deref()
                && reason != filter
            {
                continue;
            }
            matching.push(crate::prompt_cache::cache_call_record(
                &event.payload,
                &event.created_at,
            ));
        }
        let observed = matching.len() as u64;
        matching.truncate(limit as usize);
        let returned = matching.len() as u64;
        Ok(vp_protocol::types::SessionCacheCallsResult {
            session_id: params.session_id.clone(),
            calls: matching,
            observed,
            returned,
            truncated: observed > returned,
            limit,
            role: params.role.clone(),
            miss_reason: params.miss_reason.clone(),
            include_warm: params.include_warm,
        })
    }

    /// Read-only project-wide cache overview over a bounded scan of recent
    /// `context.cache` events. No provider call, no prompt text, and no
    /// cross-host claim.
    pub async fn project_cache_overview(
        &self,
        params: &vp_protocol::types::ProjectCacheParams,
    ) -> Result<vp_protocol::types::ProjectCacheOverview> {
        let limit = params.limit.unwrap_or(5_000).clamp(1, 50_000);
        let store = self.store.clone();
        let project_id = params.project_id.clone();
        let (events, truncated) =
            tokio::task::spawn_blocking(move || store.project_cache_events(&project_id, limit))
                .await??;
        let sessions = events
            .iter()
            .map(|event| event.session_id.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len() as u64;
        let aggregate = crate::prompt_cache::aggregate_cache_events(&events, &[]);
        Ok(vp_protocol::types::ProjectCacheOverview {
            project_id: params.project_id.clone(),
            sessions_observed: sessions,
            calls: aggregate.calls,
            input_tokens: aggregate.input_tokens,
            cache_read_tokens: aggregate.cache_read_tokens,
            cache_miss_tokens: aggregate.cache_miss_tokens,
            cache_write_tokens: aggregate.cache_write_tokens,
            hit_ratio: aggregate.hit_ratio,
            hit_ratio_p50: aggregate.hit_ratio_p50,
            hit_ratio_p95: aggregate.hit_ratio_p95,
            append_only_calls: aggregate.append_only_calls,
            append_only_breaks: aggregate.append_only_breaks,
            cache_epochs: aggregate.cache_epoch_timeline.len() as u64,
            top_miss_reasons: aggregate.top_miss_reasons,
            cache_epoch_timeline: aggregate.cache_epoch_timeline,
            scan_limit: limit,
            truncated,
        })
    }

    /// Bounded read-only structural break history for one session. It scans
    /// `context.cache_break` and the ordinary `context.cache` window to report
    /// append-only counts, never message text, and never calls a provider.
    pub async fn session_cache_breaks(
        &self,
        session_id: &str,
        limit: Option<u32>,
    ) -> Result<vp_protocol::types::SessionCacheBreaksResult> {
        const SCAN_LIMIT: u32 = 2_000;
        let limit = limit.unwrap_or(50).clamp(1, 200) as usize;
        let break_events = self
            .recent_events_of_kind(session_id, "context.cache_break", SCAN_LIMIT)
            .await?;
        let cache_events = self
            .recent_events_of_kind(session_id, "context.cache", SCAN_LIMIT)
            .await?;
        let continuity = crate::conversation::aggregate_continuity(&cache_events, &break_events);
        let total_breaks = break_events.len() as u64;
        let breaks = break_events
            .iter()
            .rev()
            .take(limit)
            .map(|event| crate::prompt_cache::cache_break_record(&event.payload, &event.created_at))
            .collect::<Vec<_>>();
        Ok(vp_protocol::types::SessionCacheBreaksResult {
            session_id: session_id.to_string(),
            append_only_calls: continuity.append_only_calls,
            non_append_only_calls: continuity.non_append_only_calls,
            total_breaks,
            truncated: total_breaks > breaks.len() as u64,
            breaks,
        })
    }

    /// Read-only cache-epoch inspection. No provider call is made and nothing
    /// is mutated. The epoch timeline is derived from bounded event windows.
    pub async fn session_cache_epoch(
        &self,
        session_id: &str,
        limit: Option<u32>,
    ) -> Result<vp_protocol::types::SessionCacheEpochResult> {
        let limit = limit
            .unwrap_or(50)
            .clamp(1, crate::cache_epoch::MAX_CACHE_EPOCH_TIMELINE as u32);
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        let prefix_events = self
            .recent_events_of_kind(
                session_id,
                "context.prompt_prefix",
                crate::cache_epoch::MAX_CACHE_EPOCH_TIMELINE as u32,
            )
            .await?;
        let reset_events = self
            .recent_events_of_kind(
                session_id,
                "context.compaction.cache_reset",
                crate::cache_epoch::MAX_CACHE_EPOCH_TIMELINE as u32,
            )
            .await?;
        let cache_events = self
            .recent_events_of_kind(session_id, "context.cache", 2_000)
            .await?;
        let (timeline, timeline_truncated) =
            crate::cache_epoch::epoch_timeline(&prefix_events, &reset_events, limit as usize);
        let current_epoch = prefix_events
            .iter()
            .rev()
            .find_map(|event| crate::cache_epoch::CacheEpoch::from_event_payload(&event.payload))
            .map(|epoch| epoch.hash)
            .or_else(|| {
                cache_events.iter().rev().find_map(|event| {
                    crate::cache_epoch::finalized_epoch_hash(&event.payload).map(str::to_string)
                })
            });
        let store = self.store.clone();
        let project_id = session.project_id.clone();
        let reuse_project = session.project_id.clone();
        let sid = session_id.to_string();
        let epoch_limit = limit;
        let (aggregates, observations, total_epochs) = tokio::task::spawn_blocking(move || {
            let aggregates = store.list_prompt_prefix_aggregates(&project_id, 1_000)?;
            let observations = store.list_prompt_prefix_observations(&project_id, &sid)?;
            let total = store.count_prompt_prefix_aggregates(&project_id)?;
            Ok::<_, anyhow::Error>((aggregates, observations, total))
        })
        .await??;
        let epochs =
            crate::cache_epoch::summarize_epochs(&aggregates, &observations, epoch_limit as usize);
        let mut current = epochs
            .iter()
            .find(|summary| Some(&summary.cache_epoch) == current_epoch.as_ref())
            .cloned();
        if current.is_none()
            && let Some(epoch) = current_epoch.as_deref()
            && let Some(aggregate) = aggregates
                .iter()
                .find(|aggregate| aggregate.cache_epoch == epoch)
        {
            current = crate::cache_epoch::summarize_epochs(
                std::slice::from_ref(aggregate),
                &observations,
                1,
            )
            .into_iter()
            .next();
        }
        let reused_sessions = if let Some(epoch) = current_epoch.clone() {
            let store = self.store.clone();
            let sid = session_id.to_string();
            tokio::task::spawn_blocking(move || {
                store.list_prompt_prefix_reuse_sessions(
                    &reuse_project,
                    &epoch,
                    Some(&sid),
                    limit.min(50),
                )
            })
            .await??
        } else {
            Vec::new()
        };
        let stats = crate::prompt_cache::aggregate_cache_events(&cache_events, &reset_events);
        let reuse = crate::cache_epoch::reuse_from_projection(
            current_epoch.as_deref().and_then(|epoch| {
                aggregates
                    .iter()
                    .find(|aggregate| aggregate.cache_epoch == epoch)
            }),
            current_epoch.as_deref().and_then(|epoch| {
                observations
                    .iter()
                    .find(|observation| observation.cache_epoch == epoch)
            }),
        );
        Ok(vp_protocol::types::SessionCacheEpochResult {
            session_id: session_id.to_string(),
            project_id: session.project_id,
            prompt_layout: session.prompt_layout,
            current_epoch: current,
            epochs,
            timeline,
            reused_sessions,
            total_epochs,
            timeline_truncated,
            prefix_reused_from_sessions: reuse.prior_sessions,
            cross_session_hit_tokens: stats.cross_session_hit_tokens,
            cross_session_first_calls: stats.cross_session_first_calls,
            provider_cache_misses: stats.provider_cache_misses,
            compaction_resets: stats.compaction_resets,
            compaction_lost_cache_usd: stats.compaction_lost_cache_usd,
            generated_at: vp_protocol::now_rfc3339(),
        })
    }

    /// Read-only project-wide cache-epoch report. It summarizes the same
    /// event-derived projection as `session.cache.epoch` without requiring a
    /// session and without a provider call.
    pub async fn project_cache_epochs(
        &self,
        project_id: &str,
        limit: Option<u32>,
    ) -> Result<ProjectCacheEpochResult> {
        let limit = limit
            .unwrap_or(50)
            .clamp(1, crate::cache_epoch::MAX_CACHE_EPOCH_AGGREGATES as u32);
        let store = self.store.clone();
        let project = project_id.to_string();
        let (aggregates, total_epochs, bundles, totals) = tokio::task::spawn_blocking(move || {
            let aggregates = store.list_prompt_prefix_aggregates(&project, 1_000)?;
            let total = store.count_prompt_prefix_aggregates(&project)?;
            let bundles = store.prompt_prefix_bundle_buckets(&project)?;
            let totals = store.prompt_prefix_totals(&project)?;
            Ok::<_, anyhow::Error>((aggregates, total, bundles, totals))
        })
        .await??;
        let epochs = crate::cache_epoch::summarize_epochs(&aggregates, &[], limit as usize);
        let PromptPrefixTotals {
            total_sessions,
            total_calls,
            observed_input_tokens,
            observed_cache_read_tokens,
        } = totals;
        Ok(ProjectCacheEpochResult {
            project_id: project_id.to_string(),
            epochs,
            bundles,
            total_epochs,
            total_sessions,
            total_calls,
            observed_input_tokens,
            observed_cache_read_tokens,
            cache_hit_ratio: if observed_input_tokens == 0 {
                0.0
            } else {
                (observed_cache_read_tokens as f64 / observed_input_tokens as f64).clamp(0.0, 1.0)
            },
            generated_at: vp_protocol::now_rfc3339(),
        })
    }

    /// Prune old cache-epoch telemetry for one project. This is an explicit
    /// maintenance operation; nothing in the scheduler calls it. The newest
    /// epochs and epochs with active sessions are retained.
    pub async fn prune_project_cache_epochs(
        &self,
        project_id: &str,
        older_than_days: Option<u32>,
        keep_epochs: Option<u32>,
        dry_run: bool,
    ) -> Result<ProjectCacheEpochPruneResult> {
        let days = older_than_days.unwrap_or(90).clamp(1, 3_650);
        let keep = keep_epochs.unwrap_or(50).clamp(1, 1_000);
        let cutoff = (time::OffsetDateTime::now_utc() - time::Duration::days(i64::from(days)))
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|error| anyhow::anyhow!("formatting cache-epoch cutoff: {error}"))?;
        let store = self.store.clone();
        let project = project_id.to_string();
        let cutoff_for_store = cutoff.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            store.prune_prompt_prefixes(&project, &cutoff_for_store, keep, dry_run)
        })
        .await??;
        Ok(ProjectCacheEpochPruneResult {
            project_id: project_id.to_string(),
            dry_run,
            cutoff,
            removed_epochs: outcome.removed_epochs,
            removed_observations: outcome.removed_observations,
            kept_epochs: outcome.kept_epochs,
            remaining_epochs: outcome.remaining_epochs,
            protected_epochs: outcome.protected_epochs,
        })
    }

    /// Bounded inspection of the ordered instruction state for one session.
    ///
    /// Returns exact keys, hashes, ordinals, tail flags, and character counts.
    /// It never returns section text; the only exact-byte path is test-only in
    /// `vp_core::instruction`. No provider call, worktree, or generation is
    /// triggered by this read.
    pub async fn session_instructions(
        &self,
        session_id: &str,
        agent_id: Option<&str>,
    ) -> Result<SessionInstructionsReport> {
        let session = self
            .store
            .get_session(session_id)?
            .ok_or_else(|| anyhow!("session not found: {session_id}"))?;
        let states = self.store.list_instruction_states_for_session(session_id)?;
        let agents = self.list_agents(session_id).await?;
        let latest = {
            let prefixes = self
                .recent_events_of_kind(session_id, "context.prompt_prefix", 1)
                .await?;
            crate::instruction::latest_prompt_prefix(&prefixes)
        };
        let latest_prefix_hash = latest.prefix_hash.clone();
        let latest_cache_epoch = latest.cache_epoch.clone();
        let latest_stable_prefix_hash = latest.stable_prefix_hash.clone();
        let rebuilt_events = self
            .recent_events_of_kind(session_id, "context.instruction.rebuilt", 200)
            .await?;
        let mut by_state: std::collections::BTreeMap<
            String,
            Vec<vp_protocol::types::SessionInstructionState>,
        > = std::collections::BTreeMap::new();
        for state in states {
            by_state
                .entry(state.session_id.clone())
                .or_default()
                .push(state);
        }
        let mut reports = Vec::with_capacity(by_state.len());
        for (state_id, rows) in by_state {
            let scoped_agent = crate::instruction::instruction_state_agent(&state_id, session_id);
            if let Some(filter) = agent_id {
                let matches_leader = filter == "leader" && state_id == session_id;
                if scoped_agent.as_deref() != Some(filter) && !matches_leader {
                    continue;
                }
            }
            let role = scoped_agent
                .as_deref()
                .and_then(|agent_id| agents.iter().find(|agent| agent.id == agent_id))
                .map(|agent| agent.role.clone())
                .or_else(|| (state_id == session_id).then(|| "leader".to_string()));
            let mut sections = Vec::with_capacity(rows.len());
            let mut head_texts = Vec::new();
            let mut tail_texts = Vec::new();
            for row in &rows {
                let blob = self.store.get_instruction_blob(&row.hash)?;
                let available = blob
                    .as_ref()
                    .is_some_and(|blob| sha256_hex(blob.text.as_bytes()) == row.hash);
                let chars = blob
                    .as_ref()
                    .filter(|_| available)
                    .map(|blob| blob.text.chars().count() as u64)
                    .unwrap_or(0);
                if let Some(blob) = blob.filter(|_| available) {
                    if row.tail {
                        tail_texts.push(blob.text);
                    } else {
                        head_texts.push(blob.text);
                    }
                }
                sections.push(SessionInstructionSection {
                    key: row.key.clone(),
                    hash: row.hash.clone(),
                    ordinal: row.ordinal,
                    tail: row.tail,
                    chars,
                    available,
                });
            }
            let head_hash = if head_texts.is_empty() {
                String::new()
            } else {
                sha256_hex(head_texts.join("\n\n").as_bytes())
            };
            let tail_hash = if tail_texts.is_empty() {
                String::new()
            } else {
                sha256_hex(tail_texts.join("\n\n").as_bytes())
            };
            reports.push(SessionInstructionStateReport {
                state_id,
                role,
                agent_id: scoped_agent,
                prompt_layout: crate::instruction::PROMPT_LAYOUT_V3,
                head_hash,
                tail_hash,
                identity_hash: rows
                    .iter()
                    .find(|row| row.key == crate::identity_prompt::IDENTITY_SECTION_KEY)
                    .map(|row| row.hash.clone()),
                sections,
            });
        }
        Ok(SessionInstructionsReport {
            session_id: session_id.to_string(),
            prompt_layout: crate::instruction::normalize_prompt_layout(session.prompt_layout),
            latest_prefix_hash,
            latest_cache_epoch,
            latest_stable_prefix_hash,
            latest_identity_id: latest.identity_id,
            latest_identity_generation: latest.identity_generation,
            latest_identity_hash: latest.identity_hash,
            latest_identity_memory: latest.identity_memory,
            states: reports,
            last_rebuilt: rebuilt_events.len() as u64,
        })
    }

    /// Persist a session-scoped prompt layout override. A rollback to v2 keeps
    /// the existing instruction state untouched so switching back to v3 reuses
    /// the same bytes; no migration is destructive.
    pub async fn set_session_prompt_layout(
        &self,
        session_id: &str,
        prompt_layout: u32,
    ) -> Result<vp_protocol::types::SetSessionPromptLayoutResult> {
        if !matches!(prompt_layout, 2 | 3) {
            anyhow::bail!("prompt_layout must be 2 or 3");
        }
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking({
            let store = store.clone();
            let id = id.clone();
            move || store.get_session(&id)
        })
        .await??
        .ok_or_else(|| anyhow!("session not found: {session_id}"))?;
        if session.status == "imported" {
            anyhow::bail!("imported sessions are read-only archives");
        }
        let event = tokio::task::spawn_blocking(move || {
            store.set_session_prompt_layout_with_event(&id, prompt_layout)
        })
        .await??;
        let _ = self.events.send(event);
        Ok(vp_protocol::types::SetSessionPromptLayoutResult {
            session_id: session_id.to_string(),
            prompt_layout,
        })
    }

    /// Set the project default for newly created sessions. Existing sessions
    /// keep their persisted layout.
    pub async fn set_project_prompt_layout(
        &self,
        project_id: &str,
        prompt_layout: u32,
    ) -> Result<vp_protocol::types::SetProjectPromptLayoutResult> {
        if !matches!(prompt_layout, 2 | 3) {
            anyhow::bail!("prompt_layout must be 2 or 3");
        }
        let mut policy = self.get_cache_policy(project_id).await?;
        policy.prompt_layout = prompt_layout;
        let policy = policy
            .normalized()
            .map_err(|message| anyhow!("invalid cache policy: {message}"))?;
        self.set_cache_policy(project_id, policy).await?;
        Ok(vp_protocol::types::SetProjectPromptLayoutResult {
            project_id: project_id.to_string(),
            prompt_layout,
        })
    }

    /// Deterministic cache audit over persisted events. No provider call and
    /// no TTL inference; findings are derived only from recorded telemetry.
    pub async fn session_cache_audit(
        &self,
        session_id: &str,
    ) -> Result<vp_protocol::types::SessionCacheAuditResult> {
        const LIMIT: u32 = 2_000;
        let cache_events = self
            .recent_events_of_kind(session_id, "context.cache", LIMIT)
            .await?;
        let prefix_events = self
            .recent_events_of_kind(session_id, "context.prompt_prefix", 50)
            .await?;
        let compaction_events = self
            .recent_events_of_kind(session_id, "context.compaction", LIMIT)
            .await?;
        let break_events = self
            .recent_events_of_kind(session_id, "context.cache_break", LIMIT)
            .await?;
        let cache_reset_events = self
            .recent_events_of_kind(session_id, "context.compaction.cache_reset", LIMIT)
            .await?;
        // Prefer the L4 reset events when present; older sessions only have
        // `context.compaction`, whose reset fields are policy-gated.
        let lifecycle_events = if cache_reset_events.is_empty() {
            compaction_events
        } else {
            cache_reset_events
        };
        let session_store = self.store.clone();
        let lookup_id = session_id.to_string();
        let project_id = tokio::task::spawn_blocking(move || session_store.get_session(&lookup_id))
            .await??
            .map(|session| session.project_id);
        let policy = match project_id {
            Some(project_id) => self.get_cache_policy(&project_id).await.ok(),
            None => None,
        };
        let provider = prefix_events
            .iter()
            .rev()
            .find_map(|event| {
                event
                    .payload
                    .get("cache_plan")
                    .and_then(|plan| plan.get("provider"))
                    .and_then(serde_json::Value::as_str)
            })
            .map(str::to_string);
        Ok(crate::cache_audit::audit_cache_events(
            session_id,
            provider.as_deref(),
            policy.as_ref(),
            &cache_events,
            &prefix_events,
            &lifecycle_events,
            &break_events,
        ))
    }

    /// Aggregate-only session usage for read-only resource snapshots. It does
    /// not materialize per-call ledger rows.
    pub async fn usage_totals(&self, session_id: &str) -> Result<UsageTotals> {
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.usage_totals(&id)).await?
    }

    pub async fn usage_spent(
        &self,
        session_id: &str,
        task_id: Option<&str>,
        department: Option<&str>,
    ) -> Result<(u64, f64, u64, f64)> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let task_id = task_id.map(str::to_string);
        let department = department.map(str::to_string);
        tokio::task::spawn_blocking(move || {
            store.usage_spent(&session_id, task_id.as_deref(), department.as_deref())
        })
        .await?
    }

    pub async fn record_cost(&self, session_id: &str, cost_usd: f64) -> Result<()> {
        if !cost_usd.is_finite() || cost_usd <= 0.0 {
            return Ok(());
        }
        let store = self.store.clone();
        let id = session_id.to_string();
        let event =
            tokio::task::spawn_blocking(move || store.add_session_cost_with_event(&id, cost_usd))
                .await??;
        let _ = self.events.send(event);
        Ok(())
    }

    pub async fn record_estimated_usage(
        &self,
        session_id: &str,
        input: u64,
        output: u64,
    ) -> Result<()> {
        self.record_usage_with_source(session_id, input, output, true)
            .await
    }

    async fn record_usage_with_source(
        &self,
        session_id: &str,
        input: u64,
        output: u64,
        estimated: bool,
    ) -> Result<()> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let event = tokio::task::spawn_blocking(move || {
            store.add_session_usage_with_event(&id, input, output, estimated)
        })
        .await??;
        let _ = self.events.send(event);
        Ok(())
    }
}

fn add_usage_bucket(buckets: &mut HashMap<String, UsageBucket>, key: &str, entry: &UsageEntry) {
    let bucket = buckets
        .entry(key.to_string())
        .or_insert_with(|| UsageBucket {
            key: key.to_string(),
            ..UsageBucket::default()
        });
    bucket.calls = bucket.calls.saturating_add(1);
    bucket.input_tokens = bucket.input_tokens.saturating_add(entry.input_tokens);
    bucket.output_tokens = bucket.output_tokens.saturating_add(entry.output_tokens);
    bucket.cache_read_tokens = bucket
        .cache_read_tokens
        .saturating_add(entry.cache_read_tokens);
    bucket.cache_write_tokens = bucket
        .cache_write_tokens
        .saturating_add(entry.cache_write_tokens);
    bucket.cache_hit_ratio = vp_protocol::types::cache_hit_ratio(
        bucket.input_tokens,
        bucket.cache_read_tokens,
        bucket.cache_write_tokens,
    );
    bucket.cost_usd += entry.cost_usd;
}

fn sorted_usage_buckets(buckets: HashMap<String, UsageBucket>) -> Vec<UsageBucket> {
    let mut values: Vec<_> = buckets.into_values().collect();
    values.sort_by(|left, right| {
        right
            .cost_usd
            .total_cmp(&left.cost_usd)
            .then_with(|| left.key.cmp(&right.key))
    });
    values
}

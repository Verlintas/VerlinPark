use super::*;

impl AgentService {
    /// Force a summary/trim of an idle session's active conversation. This is
    /// the explicit counterpart to automatic compaction: it never starts a
    /// prompt, and it refuses to run while the session is active so the
    /// in-memory conversation of a live run cannot diverge from the event log.
    pub async fn compact_now(&self, session_id: &str) -> Result<SessionCompactResult> {
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned())
            .ok_or_else(|| anyhow!("agent provider is not configured"))?;
        let (session, project) =
            load_session_project(self.engine.store().clone(), session_id).await?;
        if session.status == "imported" {
            anyhow::bail!("imported sessions are read-only archives");
        }
        if matches!(
            session.status.as_str(),
            "queued" | "running" | "paused" | "waiting_input"
        ) {
            anyhow::bail!(
                "session must not be queued, running, paused, or waiting for input while compacting context"
            );
        }
        // Reuse the run registry so a prompt cannot start while the
        // compaction call is in flight. The manual control is cancellable but
        // not pausable.
        let control = self.begin_manual_operation(session_id)?;
        let result = async {
            let prompt = build_session_prompt_context(
                &self.engine,
                &config,
                &session,
                &project,
                InstructionBuildMode::Materialize,
            )
            .await?;
            let spec = prompt.spec.clone();
            let model_name = prompt.model.clone();
            let calibration = load_context_calibration(&self.engine, session_id, &model_name).await;
            let system_tokens = prompt.system_tokens;
            let tool_tokens = prompt.tool_tokens;
            let compaction_policy = context_compaction_policy(&config, spec.role);
            let (threshold, hard_limit) =
                (compaction_policy.threshold, compaction_policy.hard_limit);
            let loaded = load_conversation(&self.engine, session_id).await?;
            let mut conversation = loaded.messages;
            let mut conversation_ids = loaded.ids;
            let before = self
                .engine
                .latest_event_of_kind(session_id, "context.compacted")
                .await?;
            let failed_before = self
                .engine
                .latest_event_of_kind(session_id, "context.compact_failed")
                .await?;
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
            if budget.exhausted() {
                anyhow::bail!("session budget is exhausted; compaction needs provider tokens");
            }
            let prefix_hash = prompt.prefix_hash.clone();
            let mut compaction_runtime = CompactionRuntime::default();
            let changed = compact_context_if_needed(
                &self.engine,
                &config,
                &session,
                &spec,
                &budget,
                &control,
                &mut conversation,
                &mut conversation_ids,
                threshold,
                hard_limit,
                system_tokens.saturating_add(tool_tokens),
                calibration.factor,
                true,
                &prefix_hash,
                config.compact_prepare_ratio,
                &mut compaction_runtime,
                context_strategy(spec.role),
            )
            .await?;
            if changed {
                let context_window = context_window_for(&config, spec.role);
                if context_window > 0 {
                    emit(
                        &self.engine,
                        session_id,
                        &spec,
                        "context.usage",
                        serde_json::json!({
                            "estimated_tokens": calibrate_context_tokens(
                                estimate_messages_tokens(&conversation)
                                    .saturating_add(system_tokens)
                                    .saturating_add(tool_tokens),
                                calibration.factor,
                            ),
                            "context_window": context_window,
                            "threshold_tokens": threshold,
                            "hard_limit_tokens": hard_limit,
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
            if !changed
                && let Some(event) = self
                    .engine
                    .latest_event_of_kind(session_id, "context.compact_failed")
                    .await?
                && failed_before
                    .as_ref()
                    .is_none_or(|before| event.seq > before.seq)
            {
                let message = event
                    .payload
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("context compaction failed");
                anyhow::bail!("context compaction failed: {message}");
            }
            let compaction = if changed {
                self.engine
                    .latest_event_of_kind(session_id, "context.compacted")
                    .await?
                    .filter(|event| before.as_ref().is_none_or(|before| event.seq > before.seq))
                    .and_then(|event| {
                        serde_json::from_value::<ContextCompactionSummary>(event.payload.clone())
                            .ok()
                            .map(|mut summary| {
                                summary.created_at = event.created_at;
                                summary
                            })
                    })
            } else {
                None
            };
            Ok(SessionCompactResult {
                session_id: session.id.clone(),
                compacted: changed,
                compaction,
            })
        }
        .await;
        self.end_operation(session_id);
        result
    }
}

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
            let agents = self.engine.list_agents(session_id).await?;
            let leader = agents.into_iter().find(|agent| agent.role == "leader");
            let profile = leader
                .as_ref()
                .and_then(|agent| agent.profile.as_deref())
                .and_then(AgentProfile::parse)
                .unwrap_or(AgentProfile::Generic);
            let worktree = PathBuf::from(session.worktree.clone().unwrap_or_default());
            let branch = session.branch.clone().unwrap_or_default();
            let spec = AgentSpec {
                id: leader
                    .as_ref()
                    .map(|agent| agent.id.clone())
                    .unwrap_or_else(|| "leader".to_string()),
                role: AgentRole::Leader,
                name: leader
                    .as_ref()
                    .map(|agent| agent.name.clone())
                    .unwrap_or_else(|| "Leader".to_string()),
                depth: 0,
                parent_name: None,
                title: leader.as_ref().and_then(|agent| agent.title.clone()),
                task_id: None,
                department: None,
                identity_id: None,
                identity_generation: None,
                profile,
                allow_delegation: true,
                task_kind: None,
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
            };
            let system =
                role_system_prompt(spec.role, &project, &session, &spec, &session.goal).render();
            let dynamic_mcp =
                collect_dynamic_mcp_tools(&self.engine, spec.role, &session.project_id, None).await;
            let frozen_bundle = frozen_tool_bundle(&config, &spec, &dynamic_mcp);
            let tools = frozen_bundle.tools.clone();
            let model_name = model_for(&config, spec.role);
            let calibration = load_context_calibration(&self.engine, session_id, &model_name).await;
            let system_tokens = estimate_context_tokens(&system);
            let tool_tokens = serde_json::to_string(&tools)
                .map(|schema| estimate_context_tokens(&schema))
                .unwrap_or(0);
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
            let prefix_hash = crate::sha256_hex(
                format!(
                    "{system}\n{}",
                    serde_json::to_string(&tools).unwrap_or_default()
                )
                .as_bytes(),
            );
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

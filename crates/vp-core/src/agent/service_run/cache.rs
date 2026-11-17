use super::*;

impl AgentService {
    /// Read-only preview of the per-role prompt cache plan and component
    /// hashes. It never calls a provider, creates a worktree, or starts a run.
    /// Operators use it to verify that a source/skill edit moves only the
    /// project component and that a policy TTL/breakpoint plan is what they
    /// expect before spending tokens on a warm.
    pub async fn cache_plan_now(
        &self,
        session_id: &str,
        roles: &[String],
    ) -> Result<SessionCachePlanResult> {
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
        let requested = if roles.is_empty() {
            vec!["leader".to_string()]
        } else {
            roles.to_vec()
        };
        let agents = self.engine.list_agents(session_id).await?;
        let policy = self.engine.get_cache_policy(&project.id).await?;
        let mut plans = Vec::with_capacity(requested.len());
        for name in requested {
            let role = AgentRole::parse(name.trim().to_ascii_lowercase().as_str())
                .ok_or_else(|| anyhow!("unknown cache-plan role: {name}"))?;
            let agent = agents
                .iter()
                .find(|agent| agent.role == role.as_str())
                .cloned();
            let task_kind = plan_task_kind_for_agent(&self.engine, &session.id, &agent).await;
            let spec = stable_agent_spec(&session, agent, role, task_kind);
            let sections = build_prompt_sections(
                &self.engine,
                &project,
                &session,
                &spec,
                None,
                &session.goal,
                false,
                InstructionBuildMode::Preview,
                None,
            )
            .await?;
            let dynamic_mcp =
                collect_dynamic_mcp_tools(&self.engine, spec.role, &session.project_id, None).await;
            let frozen_bundle = frozen_tool_bundle(&config, &spec, &dynamic_mcp);
            let tools = frozen_bundle.tools.clone();
            let system = sections.render();
            let hashes = sections.hashes(&model_for(&config, spec.role), &tools);
            let plan = crate::prompt_cache::request_cache_plan(
                &config.provider_name,
                &sections,
                tools.len(),
                1,
                &policy,
                spec.role.as_str(),
            );
            plans.push(SessionCachePlanRole {
                role: spec.role.as_str().to_string(),
                provider: config.provider_name.clone(),
                model: model_for(&config, spec.role),
                cache_breakpoints: plan.breakpoints,
                cache_ttl: plan.ttl.as_str().to_string(),
                ttl_explicit: plan.ttl_explicit,
                automatic: plan.automatic,
                tail_message: plan.tail_message,
                tool_breakpoint: plan.tool_breakpoint,
                core_breakpoint: plan.core_breakpoint,
                project_breakpoint: plan.project_breakpoint,
                core_hash: hashes.core_hash,
                project_hash: hashes.project_hash,
                tools_hash: hashes.tools_hash,
                volatile_hash: hashes.volatile_hash,
                prefix_hash: hashes.prefix_hash,
                identity_hash: hashes.identity_hash,
                identity_id: hashes.identity_id,
                identity_generation: hashes.identity_generation,
                prompt_layout: hashes.prompt_layout,
                stable_prefix_hash: hashes.stable_prefix_hash,
                head_sections: hashes.head_sections,
                tail_sections: hashes.tail_sections,
                section_chars: hashes.section_chars,
                core_chars: sections.core.chars().count() as u64,
                project_chars: sections.project.chars().count() as u64,
                stable_prefix_chars: sections.stable_prefix().chars().count() as u64,
                tool_count: tools.len(),
                tool_bundle: frozen_bundle.selection.kind.as_str().to_string(),
                tool_bundle_hash: frozen_bundle.bundle_hash.clone(),
                tool_bundle_tool_count: frozen_bundle.builtin_count(),
                tool_bundle_mcp_count: frozen_bundle.mcp_count(),
                tool_bundle_target_limit: frozen_bundle.target_limit,
                tool_bundle_within_target: frozen_bundle.within_target(),
                tool_bundle_fallback: frozen_bundle.selection.fallback,
                system_tokens_estimate: estimate_context_tokens(&system),
                tool_tokens_estimate: serde_json::to_string(&tools)
                    .map(|schema| estimate_context_tokens(&schema))
                    .unwrap_or(0),
            });
        }
        Ok(SessionCachePlanResult {
            session_id: session.id,
            provider: config.provider_name,
            policy: Some(policy),
            roles: plans,
        })
    }

    /// Explicitly warm the stable core/project prompt prefix and tool bundle.
    /// This is the only operator-triggered cache warm path. It never runs
    /// automatically unless the project policy sets `warm_on_resume`; it
    /// records its own usage and cost in the normal ledger.
    pub async fn warm_cache_now(
        &self,
        session_id: &str,
        role: Option<&str>,
    ) -> Result<SessionCacheWarmResult> {
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned())
            .ok_or_else(|| anyhow!("agent provider is not configured"))?;
        let role = match role {
            Some(role) => AgentRole::parse(role.trim().to_ascii_lowercase().as_str())
                .ok_or_else(|| anyhow!("unknown cache-warm role: {role}"))?,
            None => AgentRole::Leader,
        };
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
                "session must be idle, failed, or cancelled before an explicit cache warm"
            );
        }
        let _control = self.begin_manual_operation(session_id)?;
        let result = async {
            let agents = self.engine.list_agents(session_id).await?;
            let agent = agents.into_iter().find(|agent| agent.role == role.as_str());
            let task_kind = plan_task_kind_for_agent(&self.engine, &session.id, &agent).await;
            let spec = stable_agent_spec(&session, agent, role, task_kind);
            // Warm only the stable sections. Memory, objective, and
            // conversation history are deliberately excluded.
            let sections = build_prompt_sections(
                &self.engine,
                &project,
                &session,
                &spec,
                None,
                &session.goal,
                false,
                InstructionBuildMode::Materialize,
                None,
            )
            .await?;
            let dynamic_mcp =
                collect_dynamic_mcp_tools(&self.engine, spec.role, &session.project_id, None).await;
            let frozen_bundle = frozen_tool_bundle(&config, &spec, &dynamic_mcp);
            let tools = frozen_bundle.tools.clone();
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
                anyhow::bail!("session budget is exhausted; cache warm needs provider tokens");
            }
            warm_prompt_cache(
                &self.engine,
                &config,
                &session,
                &spec,
                &budget,
                &sections,
                &tools,
                "manual",
            )
            .await
        }
        .await;
        self.end_operation(session_id);
        result
    }
}

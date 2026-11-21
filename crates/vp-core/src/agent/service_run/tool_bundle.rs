use super::*;

impl AgentService {
    /// Read-only tool-bundle inspection for one session. It reports the frozen
    /// bundle the next run would use for each role, the exact built-in and MCP
    /// counts, the canonical hashes, and the latest observed prefix event. It
    /// never calls a provider, creates a worktree, or starts a run.
    pub async fn tool_bundle_now(
        &self,
        session_id: &str,
        roles: &[String],
        task_kind: Option<&str>,
        include_tools: bool,
    ) -> Result<ToolBundleResult> {
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned())
            .ok_or_else(|| anyhow!("agent provider is not configured"))?;
        let (session, _project) =
            load_session_project(self.engine.store().clone(), session_id).await?;
        let agents = self.engine.list_agents(session_id).await?;
        let mut requested = if roles.is_empty() {
            let mut names = agents
                .iter()
                .map(|agent| agent.role.clone())
                .collect::<Vec<_>>();
            names.push(AgentRole::Leader.as_str().to_string());
            names
        } else {
            roles.to_vec()
        };
        let mut seen = HashSet::new();
        requested.retain(|name| {
            let normalized = name.trim().to_ascii_lowercase();
            !normalized.is_empty() && seen.insert(normalized)
        });
        if requested.is_empty() {
            requested.push(AgentRole::Leader.as_str().to_string());
        }
        let plan = self.engine.get_team_plan(&session.id).await.ok().flatten();
        let recent_prefixes = self
            .engine
            .recent_events_of_kind(session_id, "context.prompt_prefix", 400)
            .await?;
        let explicit_task_kind = task_kind
            .map(str::trim)
            .filter(|kind| !kind.is_empty())
            .map(str::to_string);
        let mut results = Vec::with_capacity(requested.len());
        for name in requested {
            let role = AgentRole::parse(name.trim().to_ascii_lowercase().as_str())
                .ok_or_else(|| anyhow!("unknown tool-bundle role: {name}"))?;
            let agent = agents
                .iter()
                .find(|agent| agent.role == role.as_str())
                .cloned();
            let profile = agent
                .as_ref()
                .and_then(|agent| agent.profile.as_deref())
                .and_then(AgentProfile::parse)
                .unwrap_or_else(|| default_profile_for_preview(role));
            let inferred_kind = agent.as_ref().and_then(|agent| {
                let task_id = agent.task_id.as_ref()?;
                plan.as_ref()?
                    .tasks
                    .iter()
                    .find(|task| task.id == *task_id)
                    .map(|task| task.kind.clone())
            });
            let task_kind = explicit_task_kind.clone().or(inferred_kind);
            let spec = stable_agent_spec(&session, agent, role, task_kind);
            let surface = resolve_tool_surface(&self.engine, &session, &spec).await;
            let dynamic_mcp =
                collect_dynamic_mcp_tools(&self.engine, spec.role, &session.project_id, None).await;
            let frozen = frozen_tool_bundle_with_surface(&config, &spec, &dynamic_mcp, surface);
            let tools = if include_tools {
                frozen
                    .tools
                    .iter()
                    .map(|tool| {
                        let spec = crate::tool_bundle::catalog_entry(&tool.name);
                        ToolBundleToolInfo {
                            name: tool.name.clone(),
                            category: frozen.category_for(&tool.name).to_string(),
                            workspace_write: spec.is_some_and(|spec| spec.workspace_write),
                            delegation: spec.is_some_and(|spec| spec.delegation),
                            schema_hash: crate::tool_bundle::tool_schema_hash(tool),
                            description_chars: tool.description.chars().count() as u64,
                            parameter_fields: tool
                                .parameters
                                .get("properties")
                                .and_then(serde_json::Value::as_object)
                                .map_or(0, serde_json::Map::len)
                                as u64,
                        }
                    })
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            let last_run = recent_prefixes
                .iter()
                .rev()
                .find(|event| {
                    event
                        .payload
                        .get("agent_role")
                        .and_then(serde_json::Value::as_str)
                        == Some(role.as_str())
                        && event.payload.get("tool_bundle").is_some()
                })
                .map(|event| ToolBundleLastRun {
                    agent_id: event
                        .payload
                        .get("agent_id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    agent_name: event
                        .payload
                        .get("agent_name")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    role: event
                        .payload
                        .get("agent_role")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or(role.as_str())
                        .to_string(),
                    bundle: event
                        .payload
                        .get("tool_bundle")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown")
                        .to_string(),
                    bundle_hash: event
                        .payload
                        .get("tool_bundle_hash")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    tools_hash: event
                        .payload
                        .get("tools_hash")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    tool_count: event
                        .payload
                        .get("tool_count")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0),
                    mcp_tool_count: event
                        .payload
                        .get("tool_bundle_mcp_count")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0),
                    created_at: event.created_at.clone(),
                });
            results.push(ToolBundleRoleResult {
                role: role.as_str().to_string(),
                profile: profile.as_str().to_string(),
                task_kind: frozen.selection.task_kind.clone(),
                bundle: frozen.selection.kind.as_str().to_string(),
                title: crate::tool_bundle::definition(frozen.selection.kind)
                    .title
                    .to_string(),
                reason: frozen.selection.reason.to_string(),
                fallback: frozen.selection.fallback,
                bundle_hash: frozen.bundle_hash.clone(),
                tools_hash: frozen.tools_hash.clone(),
                requested: frozen.requested.clone(),
                omitted: frozen.omitted.clone(),
                warnings: frozen
                    .warnings
                    .iter()
                    .map(|warning| ToolBundleWarningInfo {
                        code: warning.code.to_string(),
                        message: warning.message.clone(),
                    })
                    .collect(),
                builtin_count: frozen.builtin_count(),
                mcp_count: frozen.mcp_count(),
                total_count: frozen.tool_count(),
                schema_bytes: frozen.schema_bytes(),
                estimated_tool_tokens: frozen.estimated_tokens(),
                target_limit: frozen.target_limit,
                within_target: frozen.within_target(),
                categories: frozen
                    .category_counts()
                    .into_iter()
                    .map(|count| ToolBundleCategoryCount {
                        category: count.category.to_string(),
                        tools: count.tools,
                        schema_bytes: count.schema_bytes,
                    })
                    .collect(),
                tools,
                last_run,
            });
        }
        let catalog = crate::tool_bundle::catalog_entries()
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
            .collect();
        Ok(ToolBundleResult {
            session_id: session.id,
            roles: results,
            catalog,
        })
    }

    /// Bounded history of persisted tool-bundle observations for a session.
    /// It reads `context.prompt_prefix` events only; it never reconstructs a
    /// historical schema or claims that the current registry matches the run.
    pub async fn tool_bundle_history(
        &self,
        session_id: &str,
        role: Option<&str>,
        limit: Option<u64>,
    ) -> Result<ToolBundleHistoryResult> {
        let (session, _project) =
            load_session_project(self.engine.store().clone(), session_id).await?;
        let role = match role {
            Some(role) => Some(
                AgentRole::parse(role.trim().to_ascii_lowercase().as_str())
                    .ok_or_else(|| anyhow!("unknown tool-bundle role: {role}"))?,
            ),
            None => None,
        };
        let limit = limit.unwrap_or(50).clamp(1, 200) as usize;
        let events = self
            .engine
            .recent_events_of_kind(session_id, "context.prompt_prefix", 2_000)
            .await?;
        let entries = events
            .iter()
            .rev()
            .filter(|event| event.payload.get("tool_bundle").is_some())
            .filter(|event| {
                role.is_none_or(|role| {
                    event
                        .payload
                        .get("agent_role")
                        .and_then(serde_json::Value::as_str)
                        == Some(role.as_str())
                })
            })
            .take(limit)
            .map(|event| ToolBundleHistoryEntry {
                seq: event.seq,
                role: event
                    .payload
                    .get("agent_role")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                agent_id: event
                    .payload
                    .get("agent_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                agent_name: event
                    .payload
                    .get("agent_name")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                bundle: event
                    .payload
                    .get("tool_bundle")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                bundle_hash: event
                    .payload
                    .get("tool_bundle_hash")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                tools_hash: event
                    .payload
                    .get("tools_hash")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                tool_count: event
                    .payload
                    .get("tool_count")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                mcp_tool_count: event
                    .payload
                    .get("tool_bundle_mcp_count")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                task_kind: event
                    .payload
                    .get("tool_bundle_task_kind")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                fallback: event
                    .payload
                    .get("tool_bundle_fallback")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                reason: event
                    .payload
                    .get("tool_bundle_reason")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                created_at: event.created_at.clone(),
            })
            .collect();
        Ok(ToolBundleHistoryResult {
            session_id: session.id,
            entries,
        })
    }

    /// Deterministic preview diff between two named bundles for one role and
    /// profile. Empty bundles yield an empty list; unknown names fail closed.
    pub fn tool_bundle_diff(
        &self,
        from_bundle: &str,
        to_bundle: &str,
        role: Option<&str>,
        profile: Option<&str>,
        task_kind: Option<&str>,
    ) -> Result<ToolBundleDiffResult> {
        let config = self.configured_agent()?;
        let from_kind = crate::tool_bundle::ToolBundleKind::parse(from_bundle)
            .ok_or_else(|| anyhow!("unknown tool bundle: {from_bundle}"))?;
        let to_kind = crate::tool_bundle::ToolBundleKind::parse(to_bundle)
            .ok_or_else(|| anyhow!("unknown tool bundle: {to_bundle}"))?;
        let role = AgentRole::parse(role.unwrap_or(AgentRole::Leader.as_str()))
            .ok_or_else(|| anyhow!("unknown tool-bundle role"))?;
        let profile = match profile {
            Some(profile) => AgentProfile::parse(profile)
                .ok_or_else(|| anyhow!("unknown tool-bundle profile: {profile}"))?,
            None => default_profile_for_preview(role),
        };
        let task_kind = task_kind
            .map(str::trim)
            .filter(|kind| !kind.is_empty())
            .map(str::to_string);
        let spec = preview_spec(role, profile, task_kind.clone());
        let from = frozen_tool_bundle_for_kind(&config, &spec, from_kind);
        let to = frozen_tool_bundle_for_kind(&config, &spec, to_kind);
        let from_names = from
            .tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let to_names = to
            .tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let added = to_names
            .difference(&from_names)
            .cloned()
            .collect::<Vec<_>>();
        let removed = from_names
            .difference(&to_names)
            .cloned()
            .collect::<Vec<_>>();
        let shared = from_names
            .intersection(&to_names)
            .cloned()
            .collect::<Vec<_>>();
        let mut warnings = from
            .warnings
            .iter()
            .chain(to.warnings.iter())
            .map(|warning| ToolBundleWarningInfo {
                code: warning.code.to_string(),
                message: warning.message.clone(),
            })
            .collect::<Vec<_>>();
        warnings.sort_by(|left, right| {
            left.code
                .cmp(&right.code)
                .then_with(|| left.message.cmp(&right.message))
        });
        warnings.dedup();
        Ok(ToolBundleDiffResult {
            from_bundle: from_kind.as_str().to_string(),
            to_bundle: to_kind.as_str().to_string(),
            role: role.as_str().to_string(),
            profile: profile.as_str().to_string(),
            task_kind: spec.task_kind.clone(),
            from_hash: from.tools_hash.clone(),
            to_hash: to.tools_hash.clone(),
            from_count: from.tool_count(),
            to_count: to.tool_count(),
            added,
            removed,
            shared,
            changed: from.tools_hash != to.tools_hash,
            warnings,
        })
    }
}

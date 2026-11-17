use super::*;

impl AgentService {
    pub(in crate::agent) fn configured_agent(&self) -> Result<AgentConfig> {
        self.config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned())
            .ok_or_else(|| anyhow!("agent provider is not configured"))
    }

    pub(in crate::agent) async fn context_preamble(
        &self,
        config: &AgentConfig,
        session: &Session,
        project: &Project,
    ) -> Result<(AgentSpec, u64, String)> {
        let agents = self.engine.list_agents(&session.id).await?;
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
        let system = role_system_prompt(spec.role, project, session, &spec, &session.goal).render();
        let dynamic_mcp =
            collect_dynamic_mcp_tools(&self.engine, spec.role, &session.project_id, None).await;
        let frozen_bundle = frozen_tool_bundle(config, &spec, &dynamic_mcp);
        let tools = frozen_bundle.tools.clone();
        let prefix_hash = crate::sha256_hex(
            format!(
                "{system}\n{}",
                crate::tool_bundle::canonical_tools_json(&tools)
            )
            .as_bytes(),
        );
        let tokens = estimate_context_tokens(&system).saturating_add(
            serde_json::to_string(&tools)
                .map(|schema| estimate_context_tokens(&schema))
                .unwrap_or(0),
        );
        Ok((spec, tokens, prefix_hash))
    }

    /// Capture the current active-context boundary without running a prompt or
    /// creating a new compaction. When the session already has a compaction
    /// boundary, the manual checkpoint records exactly that boundary's summary,
    /// cursor, structured state, and token estimates.
    pub async fn context_checkpoint_now(
        &self,
        session_id: &str,
        label: Option<String>,
    ) -> Result<ContextCheckpoint> {
        let config = self.configured_agent()?;
        let (session, project) =
            load_session_project(self.engine.store().clone(), session_id).await?;
        if session.status == "imported" {
            anyhow::bail!("imported sessions are read-only archives");
        }
        if crate::is_active_session_status(&session.status) {
            anyhow::bail!(
                "session must be idle, failed, or cancelled before creating a context checkpoint"
            );
        }
        self.begin_operation(session_id)?;
        let result = async {
            let (spec, base_tokens, prefix_hash) =
                self.context_preamble(&config, &session, &project).await?;
            let store = self.engine.store().clone();
            let revision_session = session.id.clone();
            let current_revision =
                tokio::task::spawn_blocking(move || store.plan_revision_count(&revision_session))
                    .await??;
            let latest = self
                .engine
                .latest_event_of_kind(session_id, "context.compacted")
                .await?;
            let (
                mode,
                summary,
                through_message_id,
                structured_state,
                plan_revision,
                tokens_before,
                tokens_after,
            ) = if let Some(event) = &latest {
                let mode = event
                    .payload
                    .get("mode")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("summary")
                    .to_string();
                let summary = event
                    .payload
                    .get("summary")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                let through = event
                    .payload
                    .get("through_message_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                let structured_state = event
                    .payload
                    .get("structured_state")
                    .cloned()
                    .and_then(|value| serde_json::from_value::<ContextStructuredState>(value).ok());
                let plan_revision = structured_state
                    .as_ref()
                    .map(|state| state.plan_revision)
                    .unwrap_or(current_revision);
                let tokens_before = event
                    .payload
                    .get("estimated_tokens_before")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                let tokens_after = event
                    .payload
                    .get("estimated_tokens_after")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                (
                    mode,
                    summary,
                    through,
                    structured_state,
                    plan_revision,
                    tokens_before,
                    tokens_after,
                )
            } else {
                let state = load_compaction_structured_state(&self.engine, &session, &spec).await;
                let loaded = load_conversation(&self.engine, session_id).await?;
                let model_name = model_for(&config, spec.role);
                let calibration =
                    load_context_calibration(&self.engine, session_id, &model_name).await;
                let estimated = calibrate_context_tokens(
                    estimate_messages_tokens(&loaded.messages).saturating_add(base_tokens),
                    calibration.factor,
                );
                (
                    "manual".to_string(),
                    None,
                    None,
                    Some(state),
                    current_revision,
                    estimated,
                    estimated,
                )
            };
            let checkpoint = ContextCheckpoint {
                id: uuid::Uuid::now_v7().to_string(),
                session_id: session.id.clone(),
                project_id: project.id.clone(),
                label: label.unwrap_or_else(|| "manual context checkpoint".to_string()),
                mode,
                summary,
                through_message_id,
                message_cursor: 0,
                plan_revision,
                provider: config.provider_name.clone(),
                model: model_for(&config, spec.role),
                prompt_prefix_hash: prefix_hash,
                worktree_head: context_worktree_head(session.worktree.as_deref()),
                tokens_before,
                tokens_after,
                structured_state,
                source_compaction_seq: latest.as_ref().map(|event| event.seq),
                automatic: false,
                created_at: vp_protocol::now_rfc3339(),
            };
            let event = self
                .engine
                .record_context_checkpoint(checkpoint.clone())
                .await?;
            Ok::<_, anyhow::Error>(
                crate::context::context_checkpoint_from_event(&event).unwrap_or(checkpoint),
            )
        }
        .await;
        self.end_operation(session_id);
        result
    }

    /// Deterministic restore preflight shared by the detail RPC and the
    /// restore command. Errors are hard safety refusals; warnings are
    /// token-threshold advisories that `force` may bypass.
    pub(in crate::agent) async fn context_restore_preflight(
        &self,
        config: Option<&AgentConfig>,
        session: &Session,
        project: &Project,
        checkpoint: &ContextCheckpoint,
    ) -> Result<ContextRestorePreflight> {
        let mut preflight = ContextRestorePreflight {
            checkpoint_plan_revision: checkpoint.plan_revision,
            worktree_head: checkpoint.worktree_head.clone(),
            ..ContextRestorePreflight::default()
        };
        if session.status == "imported" {
            preflight
                .errors
                .push("imported sessions are read-only archives".to_string());
        } else if !matches!(session.status.as_str(), "idle" | "failed" | "cancelled") {
            preflight.errors.push(format!(
                "session must be idle, failed, or cancelled before restoring context; current status is {}",
                session.status
            ));
        }
        let store = self.engine.store().clone();
        let imported_session = session.id.clone();
        let imported = tokio::task::spawn_blocking(move || {
            store.session_has_imported_event(&imported_session)
        })
        .await??;
        if imported {
            preflight.errors.push(
                "imported or rehydrated sessions cannot restore a context checkpoint".to_string(),
            );
        }
        let store = self.engine.store().clone();
        let revision_session = session.id.clone();
        let current_revision =
            tokio::task::spawn_blocking(move || store.plan_revision_count(&revision_session))
                .await??;
        preflight.plan_revision = current_revision;
        if checkpoint.plan_revision != current_revision {
            preflight.errors.push(format!(
                "plan revision changed: checkpoint={}, current={current_revision}",
                checkpoint.plan_revision
            ));
        }
        if let Some(config) = config {
            preflight.provider = config.provider_name.clone();
            preflight.model = model_for(config, AgentRole::Leader);
            if checkpoint.provider != config.provider_name {
                preflight.errors.push(format!(
                    "provider changed: checkpoint={}, current={}",
                    checkpoint.provider, config.provider_name
                ));
            }
            if checkpoint.model != preflight.model {
                preflight.errors.push(format!(
                    "model changed: checkpoint={}, current={}",
                    checkpoint.model, preflight.model
                ));
            }
        } else {
            preflight
                .errors
                .push("agent provider is not configured".to_string());
        }

        let worktree_head = checkpoint.worktree_head.clone();
        if worktree_head.is_empty() {
            preflight
                .errors
                .push("checkpoint does not record a worktree head".to_string());
        }
        let worktree = session.worktree.clone().map(PathBuf::from);
        let project_root = PathBuf::from(&project.root);
        match worktree {
            Some(worktree) if worktree.is_dir() => {
                let status = worktree::status_porcelain(&worktree);
                let current = worktree::head_commit(&worktree);
                match (status, current) {
                    (Ok(status), Ok(current)) => {
                        preflight.current_worktree_head = Some(current.clone());
                        if !status.is_empty() {
                            preflight.errors.push(
                                "session worktree has uncommitted changes; commit or discard them before restoring context"
                                    .to_string(),
                            );
                        }
                        if !worktree_head.is_empty() {
                            if !worktree::commit_exists(&project_root, &worktree_head) {
                                preflight.errors.push(format!(
                                    "checkpoint worktree head is missing: {worktree_head}"
                                ));
                            } else if worktree_head == current {
                                preflight.worktree_descendant = true;
                            } else {
                                let ancestor = worktree::git_output(
                                    &project_root,
                                    &["merge-base", "--is-ancestor", &worktree_head, &current],
                                )
                                .is_ok();
                                preflight.worktree_descendant = ancestor;
                                if !ancestor {
                                    preflight.errors.push(format!(
                                        "current worktree head {current} is not a descendant of checkpoint head {worktree_head}"
                                    ));
                                }
                            }
                        }
                    }
                    (Err(error), _) => preflight
                        .errors
                        .push(format!("could not inspect session worktree: {error:#}")),
                    (_, Err(error)) => preflight
                        .errors
                        .push(format!("could not read session worktree head: {error:#}")),
                }
            }
            Some(worktree) => preflight.errors.push(format!(
                "session worktree is missing: {}",
                worktree.display()
            )),
            None => preflight.errors.push("session has no worktree".to_string()),
        }

        let budget = SessionBudget::new(
            session.token_budget,
            session.tokens_in + session.tokens_out,
            AgentCaps {
                managers: 1,
                workers: 1,
                labour: 1,
            },
        )
        .with_cost(session.cost_budget_usd, session.cost_spent_usd)
        .with_time(session.time_budget_secs)
        .with_unlimited_flag(self.engine.unlimited_flag(&session.id, session.unlimited));
        if budget.exhausted() {
            preflight.budget_exhausted = true;
            preflight
                .errors
                .push("session budget is exhausted".to_string());
        }

        let messages = self.engine.list_messages(&session.id).await?;
        let (restored, through_found) = restored_active_messages(&messages, checkpoint);
        if checkpoint.through_message_id.is_some() && !through_found {
            preflight.errors.push(format!(
                "checkpoint through message is missing: {}",
                checkpoint.through_message_id.as_deref().unwrap_or_default()
            ));
        }
        preflight.active_messages = restored.len();
        if let Some(config) = config {
            match self.context_preamble(config, session, project).await {
                Ok((spec, base_tokens, _)) => {
                    let model_name = model_for(config, spec.role);
                    let calibration =
                        load_context_calibration(&self.engine, &session.id, &model_name).await;
                    let (threshold, hard_limit) = context_limits(config, spec.role);
                    let estimated = calibrate_context_tokens(
                        estimate_messages_tokens(&restored).saturating_add(base_tokens),
                        calibration.factor,
                    );
                    preflight.estimated_tokens = estimated;
                    preflight.threshold_tokens = threshold;
                    preflight.hard_limit_tokens = hard_limit;
                    if estimated > hard_limit {
                        preflight.errors.push(format!(
                            "restored context estimate {estimated} exceeds the hard limit {hard_limit}"
                        ));
                    } else if threshold > 0 && estimated >= threshold {
                        preflight.warnings.push(format!(
                            "restored context estimate {estimated} is at or above the compaction threshold {threshold}; pass force=true to accept it"
                        ));
                    }
                }
                Err(error) => preflight
                    .errors
                    .push(format!("could not estimate restored context: {error:#}")),
            }
        }
        preflight.restorable = preflight.errors.is_empty();
        Ok(preflight)
    }

    pub async fn context_checkpoint_detail(
        &self,
        session_id: &str,
        checkpoint_id: &str,
    ) -> Result<ContextCheckpointDetailResult> {
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned());
        let (session, project) =
            load_session_project(self.engine.store().clone(), session_id).await?;
        let checkpoint = self
            .engine
            .list_context_checkpoints(session_id, Some(200))
            .await?
            .checkpoints
            .into_iter()
            .find(|checkpoint| checkpoint.id == checkpoint_id)
            .ok_or_else(|| crate::context::empty_context_checkpoint_error(checkpoint_id))?;
        let preflight = self
            .context_restore_preflight(config.as_ref(), &session, &project, &checkpoint)
            .await?;
        let events = self.engine.all_events(session_id).await?;
        let is_active = crate::context::active_context_checkpoint_id(&events)
            .is_some_and(|id| id == checkpoint.id);
        let summary_preview =
            crate::context::context_checkpoint_preview(checkpoint.summary.as_deref(), 160);
        Ok(ContextCheckpointDetailResult {
            checkpoint,
            preflight,
            is_active,
            summary_preview,
        })
    }

    /// Context-only restore. It appends a reconstructed compaction boundary
    /// plus a restore audit event; it never resets the worktree, creates a
    /// task/run/merge, or rewrites stored messages.
    pub async fn restore_context_checkpoint_now(
        &self,
        session_id: &str,
        checkpoint_id: &str,
        force: bool,
    ) -> Result<ContextRestoreResult> {
        let config = self.configured_agent()?;
        let (session, project) =
            load_session_project(self.engine.store().clone(), session_id).await?;
        self.begin_operation(session_id)?;
        let result = async {
            let checkpoint = self
                .engine
                .list_context_checkpoints(session_id, Some(200))
                .await?
                .checkpoints
                .into_iter()
                .find(|checkpoint| checkpoint.id == checkpoint_id)
                .ok_or_else(|| crate::context::empty_context_checkpoint_error(checkpoint_id))?;
            let preflight = self
                .context_restore_preflight(Some(&config), &session, &project, &checkpoint)
                .await?;
            if !preflight.errors.is_empty() {
                anyhow::bail!("context restore refused: {}", preflight.errors.join("; "));
            }
            let warning = if preflight.warnings.is_empty() {
                None
            } else {
                let warning = preflight.warnings.join("; ");
                if !force {
                    anyhow::bail!("context restore warning: {warning}");
                }
                Some(warning)
            };
            let previous = self
                .engine
                .latest_event_of_kind(session_id, "context.compacted")
                .await?;
            let previous_seq = previous.as_ref().map(|event| event.seq);
            let compaction_count = self
                .engine
                .count_events_of_kind(session_id, "context.compacted")
                .await?
                .saturating_add(1);
            let model_name = model_for(&config, AgentRole::Leader);
            let calibration = load_context_calibration(&self.engine, session_id, &model_name).await;
            let restore_mode = crate::context::context_checkpoint_restore_mode(&checkpoint);
            let checkpoint_summary = checkpoint.summary.clone();
            let checkpoint_through = checkpoint.through_message_id.clone();
            let checkpoint_structured_state = checkpoint.structured_state.clone();
            let checkpoint_id_value = checkpoint.id.clone();
            let (usage_spec, _, _) = self.context_preamble(&config, &session, &project).await?;
            let compaction_payload = serde_json::json!({
                "mode": restore_mode,
                "through_message_id": checkpoint_through.clone(),
                "summary": checkpoint_summary.clone(),
                "summarized_messages": 0,
                "dropped_messages": 0,
                "kept_messages": preflight.active_messages,
                "estimated_tokens_before": checkpoint.tokens_before,
                "estimated_tokens_after": preflight.estimated_tokens,
                "threshold_tokens": preflight.threshold_tokens,
                "hard_limit_tokens": preflight.hard_limit_tokens,
                "calibration": calibration.factor,
                "input_tokens": 0,
                "output_tokens": 0,
                "estimated": true,
                "latency_ms": 0,
                "background_reuse": false,
                "fallback_to_sync": false,
                "compaction_count": compaction_count,
                "structured_state": checkpoint_structured_state,
                "background_prepared": false,
                "restored": true,
                "restored_from_checkpoint": checkpoint_id_value.clone(),
                "context_checkpoint_id": checkpoint_id_value.clone(),
            });
            let restored_payload = serde_json::json!({
                "checkpoint_id": checkpoint_id_value,
                "mode": restore_mode,
                "through_message_id": checkpoint_through,
                "checkpoint": checkpoint.clone(),
                "estimated_tokens": preflight.estimated_tokens,
                "hard_limit_tokens": preflight.hard_limit_tokens,
                "active_messages": preflight.active_messages,
                "previous_compaction_seq": previous_seq,
                "forced": force,
                "warning": warning,
            });
            let (compaction_event, restored_event) = self
                .engine
                .append_context_restore_events(session_id, compaction_payload, restored_payload)
                .await?;
            let context_window = context_window_for(&config, AgentRole::Leader);
            let threshold = preflight.threshold_tokens;
            let hard_limit = preflight.hard_limit_tokens;
            if context_window > 0 {
                emit(
                    &self.engine,
                    session_id,
                    &usage_spec,
                    "context.usage",
                    serde_json::json!({
                        "estimated_tokens": preflight.estimated_tokens,
                        "context_window": context_window,
                        "threshold_tokens": threshold,
                        "hard_limit_tokens": hard_limit,
                        "messages": preflight.active_messages,
                        "calibration": calibration.factor,
                    }),
                )
                .await?;
            }
            Ok(ContextRestoreResult {
                session_id: session.id.clone(),
                checkpoint_id: checkpoint.id,
                mode: restore_mode.to_string(),
                through_message_id: checkpoint.through_message_id,
                active_messages: preflight.active_messages,
                estimated_tokens: preflight.estimated_tokens,
                hard_limit_tokens: preflight.hard_limit_tokens,
                warning,
                compaction_event_seq: compaction_event.seq,
                restored_event_seq: restored_event.seq,
            })
        }
        .await;
        self.end_operation(session_id);
        result
    }
}

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_tool_calls(
    engine: &Engine,
    config: &AgentConfig,
    session: &Session,
    project: &Project,
    control: &RunControl,
    budget: &SessionBudget,
    spec: &AgentSpec,
    journal: &crate::run_journal::RunJournal,
    top_level: bool,
    turn: u32,
    tool_calls: Vec<ToolCall>,
    conversation: &mut Vec<ChatMessage>,
    conversation_ids: &mut Vec<Option<String>>,
    citations: &mut CitationRegistry,
    active_skill: &mut Option<Skill>,
    deferred_wait: &mut Option<DeferredWait>,
    pending_promotions: &mut std::collections::BTreeSet<String>,
    team_plan: &mut Option<TeamPlan>,
    turn_tool_bytes: &mut u64,
    run_contract: &Option<crate::contract::TaskContract>,
    run_contract_policy: &vp_protocol::types::ContractPolicy,
    run_contract_usage: &mut crate::contract::MutationUsage,
    dynamic_mcp_tools: &[McpToolBinding],
    gui_policy: &vp_protocol::types::GuiPolicy,
    effect_policy: &vp_protocol::types::EffectPolicy,
    tool_context: &ToolContext,
    plan_budget: &PlanBudgetScope,
    frozen_bundle: &crate::tool_bundle::FrozenToolBundle,
    surface: ToolSurface,
    tools: &[ToolSchema],
) -> Result<Option<LoopOutcome>> {
    // Capture every serial call id before the iterator consumes the list.
    // If one call defers the run, the remaining siblings must still get a
    // persisted tool result: provider tool-call history requires one
    // result per call, and a resume would otherwise send an invalid turn.
    let serial_call_ids = tool_calls
        .iter()
        .enumerate()
        .map(|(idx, call)| {
            let id = if call.id.is_empty() {
                format!("call_{}_{}", turn, idx)
            } else {
                call.id.clone()
            };
            (id, call.name.clone())
        })
        .collect::<Vec<_>>();
    for (index, call) in tool_calls.into_iter().enumerate() {
        if !wait_for_task_resume(engine, &session.id, spec, control).await? {
            return Ok(Some(LoopOutcome::Cancelled));
        }
        if control.is_cancelled() || control.is_preempted() {
            return Ok(Some(LoopOutcome::Cancelled));
        }
        if budget.exhausted() {
            return Ok(Some(LoopOutcome::BudgetExceeded(BudgetStop::session(
                budget,
            ))));
        }
        let mut imposed_preempt: Option<crate::preemption::Snapshot> = None;
        if let Err(error) = engine
            .touch_task_progress(
                &session.id,
                spec.task_id.as_deref(),
                turn + 1,
                Some(&call.name),
            )
            .await
        {
            tracing::warn!(%error, "failed to record task tool progress");
        }
        let call_id = if call.id.is_empty() {
            format!("call_{}_{}", turn, index)
        } else {
            call.id.clone()
        };
        let input: serde_json::Value = match parse_tool_arguments(&call.arguments) {
            Ok(input) => input,
            Err(reason) => {
                // Never execute a tool with guessed input. The provider still
                // receives a tool result so its call/result history stays
                // valid, and the failure is typed and auditable.
                let output = ToolOutput::error_coded(
                    call.name.as_str(),
                    crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
                    reason,
                )
                .with_failure_evidence("invalid_tool_arguments");
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.finished",
                    tool_finished_payload(
                        &call_id,
                        &call.name,
                        &output,
                        0,
                        None,
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
                conversation.push(ChatMessage::tool(call_id, output.content));
                continue;
            }
        };
        // The probe needs the original arguments after dispatch consumes
        // `input`; cloning one bounded JSON value is cheaper than threading
        // it through every tool branch.
        let effect_input = input.clone();
        // Duplicate-write gate runs before approval and before any journal
        // intent: an already-completed write is returned from the journal,
        // not executed again. Read-only tools re-run normally.
        let duplicate = journal
            .check_duplicate_write(engine, &call.name, &input)
            .await?;
        if duplicate.is_some() {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                    "journal_replay": true,
                }),
            )
            .await?;
        }
        let is_delegate = call.name == "delegate" && spec.allow_delegation;
        let is_acp_delegate = call.name == "delegate_acp"
            && spec.allow_delegation
            && matches!(spec.role, AgentRole::Leader | AgentRole::Manager);
        let is_delegate_many = call.name == "delegate_many"
            && spec.role == AgentRole::Manager
            && spec.allow_delegation;
        let is_verdict = call.name == "submit_verdict" && spec.role == AgentRole::Verifier;
        let is_team_plan = call.name == "submit_team_plan" && spec.role == AgentRole::Leader;
        let is_plan_fanout = call.name == "plan_fanout" && spec.role == AgentRole::Leader;
        let is_run_plan = call.name == "run_team_plan" && spec.role == AgentRole::Leader;
        let is_deliberate = call.name == "deliberate_team_plan" && spec.role == AgentRole::Leader;
        let is_todo = call.name == "todo_write" && spec.role == AgentRole::Leader;
        let is_mailbox_tool = matches!(
            call.name.as_str(),
            "send_message"
                | "read_mailbox"
                | "publish_artifact"
                | "list_artifacts"
                | "read_artifact"
        );
        let is_memory_tool = matches!(call.name.as_str(), "remember" | "recall");
        let is_read_tool_output = call.name == "read_tool_output";
        let is_source_tool = matches!(
            call.name.as_str(),
            "search_sources" | "read_source" | "list_sources"
        );
        let is_skill_tool = call.name == "use_skill";
        let is_question_tool = call.name == "ask_user" && spec.role != AgentRole::Verifier;
        let is_wait_for_gate = call.name == "wait_for_gate"
            && matches!(
                spec.role,
                AgentRole::Leader | AgentRole::Manager | AgentRole::Worker
            )
            && !(spec.role == AgentRole::Manager && !spec.allow_delegation);
        let is_mcp_tool = call.name == "mcp";
        let is_coordination_tool = call.name == "coordinate";
        let is_team_status = call.name == "team_status";
        let is_budget_status = call.name == "budget_status";
        let is_task_detail = call.name == "task_detail";
        let is_task_control = call.name == "task_control"
            && matches!(spec.role, AgentRole::Leader | AgentRole::Manager);
        let is_tool_search = call.name == "tool_search";
        let is_tool_help = call.name == "tool_help";
        let is_progress_report = call.name == "report_progress"
            && matches!(spec.role, AgentRole::Leader | AgentRole::Manager);
        let is_run_script = call.name == "run_script"
            && matches!(
                spec.role,
                AgentRole::Leader | AgentRole::Worker | AgentRole::Labour
            );
        let is_artifact_file = call.name == "publish_artifact_file";
        let is_archive_tool = matches!(call.name.as_str(), "archive_list" | "archive_extract");
        let is_media_tool = matches!(call.name.as_str(), "media_info" | "media_transcode");
        let is_scout = call.name == "scout"
            && spec.allow_delegation
            && matches!(spec.role, AgentRole::Leader | AgentRole::Manager);
        let is_browser = call.name == "browser" && spec.role.allows_tool(spec.profile, "browser");
        let is_computer =
            call.name == "computer" && spec.role.allows_tool(spec.profile, "computer");
        let dynamic_mcp_tool = dynamic_mcp_tools
            .iter()
            .find(|binding| binding.exposed_name == call.name);
        let registry_tool = config
            .tools
            .get(&call.name)
            .filter(|_| spec.role.allows_tool(spec.profile, &call.name));
        let scout_blocked =
            spec.role == AgentRole::Scout && !spec.role.allows_tool(spec.profile, &call.name);
        // A registry tool outside the role policy is rejected by the
        // existing role check with its historical message. Do not let the
        // contract replace that denial.
        let role_policy_unavailable = config.tools.get(&call.name).is_some()
            && !spec.role.allows_tool(spec.profile, &call.name);
        let mcp_trust = if let Some(binding) = dynamic_mcp_tool {
            Some(binding.pin_trust)
        } else if is_mcp_tool && input.get("action").and_then(Value::as_str) == Some("call") {
            Some(resolve_mcp_pin_trust(engine, &session.project_id, &input).await)
        } else {
            None
        };
        let contract_tool = dynamic_mcp_tool
            .map(|_| "mcp")
            .or(is_mcp_tool.then_some("mcp"));
        let authorization = authorize_tool_call(
            engine,
            config,
            session,
            spec,
            control,
            journal,
            &call_id,
            &call.name,
            &input,
            turn + 1,
            run_contract.as_ref(),
            run_contract_policy,
            run_contract_usage,
            active_skill.as_ref(),
            gui_policy,
            contract_tool,
            dynamic_mcp_tool.is_some(),
            mcp_trust,
            role_policy_unavailable,
            scout_blocked,
            false,
            false,
            None,
            None,
        )
        .await?;
        if authorization.cancelled {
            return Ok(Some(LoopOutcome::Cancelled));
        }
        let approval_risk = authorization.risk;
        let approval_wait_ms = authorization.approval_wait_ms;
        let denied_reason = authorization.denied_reason;
        let skill_denied = authorization.skill_denied;
        // Effect receipts are written in the same transaction as the run
        // journal intent. A completed logical effect replays its stored
        // evidence without executing; an unresolved one blocks execution
        // until an operator reconciles it. A denied/skill-blocked call
        // never creates an effect intent because it never runs.
        let effect_gate = prepare_tool_effect_gate(
            engine,
            session,
            spec,
            journal,
            turn + 1,
            &call_id,
            &call.name,
            &input,
            approval_risk,
            denied_reason.is_some(),
            skill_denied.is_some(),
            dynamic_mcp_tool.is_some(),
            effect_policy,
            tool_context,
            duplicate.is_some(),
        )
        .await?;
        let planned_effect = effect_gate.planned_effect;
        let effect_override = effect_gate.effect_override;
        let mut effect_begin = effect_gate.effect_begin;
        let effect_duplicate = effect_gate.effect_duplicate;
        let journal_pair = effect_gate.journal_pair;
        let effect_classification_failed = effect_gate.classification_failed;
        if effect_override.is_some() {
            // No dispatch branch runs for a replayed or blocked effect, so
            // emit the same start marker the executing branches emit.
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                    "effect_blocked": !effect_classification_failed
                        && effect_override
                            .as_ref()
                            .is_some_and(|output| output.is_error),
                    "effect_classification_failed": effect_classification_failed,
                }),
            )
            .await?;
        }
        control
            .preempt
            .set_phase(crate::preemption::Phase::Tool, Some(&call.name));
        let tool_deadline = crate::preemption::tool_deadline(&control.preemption_policy());
        let tool_token = crate::preemption::Token::tool_child(&control.preempt, tool_deadline);
        tool_token.set_phase(crate::preemption::Phase::Tool, Some(&call.name));
        let tool_started = Instant::now();
        let output = if let Some(mut output) = effect_override {
            if let Some(receipt) = &effect_begin {
                annotate_effect_output(&mut output, receipt);
            }
            output
        } else if let Some(mut output) = duplicate {
            if let Some(receipt) = &effect_duplicate {
                annotate_effect_output(&mut output, receipt);
            }
            output
        } else if let Some(reason) = denied_reason {
            ToolOutput::error_coded(
                call.name.as_str(),
                crate::tooling::errors::ToolErrorCode::PERMISSION_DENIED,
                reason,
            )
            .with_failure_evidence(format!("tool={}", call.name))
        } else if let Some(skill) = skill_denied {
            ToolOutput::error_coded(
                call.name.as_str(),
                crate::tooling::errors::ToolErrorCode::SKILL_TOOL_DENIED,
                format!(
                    "tool '{}' is not allowed by active skill '{}'",
                    call.name, skill.name
                ),
            )
            .with_failure_evidence(format!("skill={}", skill.slug))
        } else if is_progress_report {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            match run_progress_report(engine, session, spec, &input).await {
                Ok(output) => output,
                Err(error) => ToolOutput::error(format!("report_progress failed: {error}")),
            }
        } else if is_run_script {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            match run_script_tool(
                engine,
                config,
                session,
                spec,
                control,
                journal,
                &input,
                tools,
                surface,
                &call_id,
                turn + 1,
                run_contract.as_ref(),
                run_contract_policy,
                run_contract_usage,
                active_skill.as_ref(),
                gui_policy,
                effect_policy,
                tool_context,
                &tool_token,
            )
            .await?
            {
                ScriptToolOutcome::Output(output) => output,
                ScriptToolOutcome::Cancelled => return Ok(Some(LoopOutcome::Cancelled)),
            }
        } else if is_deliberate {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            match engine.get_team_plan(&session.id).await? {
                None => ToolOutput::error("no TeamPlan submitted; call submit_team_plan first"),
                Some(plan) => {
                    let rounds = input
                        .get("rounds")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(2)
                        .clamp(1, 3) as u32;
                    match deliberate_team_plan(
                        engine, config, session, project, control, budget, spec, &plan, rounds,
                    )
                    .await?
                    {
                        RoundtableResult::Done(report) => ToolOutput::ok(report),
                        RoundtableResult::Cancelled => {
                            return Ok(Some(LoopOutcome::Cancelled));
                        }
                        RoundtableResult::BudgetExceeded(stop) => {
                            if stop.is_abortive() {
                                return Ok(Some(LoopOutcome::BudgetExceeded(stop)));
                            }
                            ToolOutput::error(stop.summary())
                        }
                    }
                }
            }
        } else if is_run_plan {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            match engine.get_team_plan(&session.id).await? {
                None => ToolOutput::error("no TeamPlan submitted; call submit_team_plan first"),
                Some(plan) => match execute_team_plan(
                    engine, config, session, project, control, budget, spec, &plan, None,
                )
                .await?
                {
                    PlanRunResult::Done(report) => ToolOutput::ok(report),
                    PlanRunResult::Cancelled => return Ok(Some(LoopOutcome::Cancelled)),
                    PlanRunResult::Deferred(wait) => {
                        return Ok(Some(LoopOutcome::Deferred(wait)));
                    }
                    PlanRunResult::BudgetExceeded(stop) => {
                        return Ok(Some(LoopOutcome::BudgetExceeded(stop)));
                    }
                },
            }
        } else if is_team_plan {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            let parsed = serde_json::from_value::<TeamPlan>(input.clone())
                .map_err(|error| anyhow!("invalid team plan: {error}"))
                .and_then(|plan| {
                    validate_team_plan(&plan, config.max_managers)?;
                    Ok(plan)
                });
            match parsed {
                Ok(plan) => {
                    let summary = format!(
                        "Team plan recorded: {} departments, {} tasks. Delegate to Managers using the planned titles.",
                        plan.departments.len(),
                        plan.tasks.len()
                    );
                    match engine.submit_team_plan(&session.id, &plan).await {
                        Ok(()) => {
                            *team_plan = Some(plan);
                            ToolOutput::ok(summary)
                        }
                        Err(error) if error.to_string().starts_with("PLAN_REVISION_REQUIRED") => {
                            // A second submission must never replace the
                            // baseline plan: that would start a new plan_seq
                            // and reset completed tasks/attempts. The model
                            // gets a typed refusal; the run continues.
                            ToolOutput::error_coded(
                                "submit_team_plan",
                                crate::tooling::errors::ToolErrorCode::CONFLICT,
                                error.to_string(),
                            )
                            .with_failure_evidence("PLAN_REVISION_REQUIRED")
                            .with_failure_hint(
                                "The session already has a TeamPlan. Do not resubmit it; use plan_fanout for a bounded expansion or ask the operator for an explicit plan revision.",
                            )
                        }
                        Err(error) => return Err(error),
                    }
                }
                Err(error) => ToolOutput::error(error.to_string()),
            }
        } else if is_plan_fanout {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            let params = serde_json::from_value::<vp_protocol::types::PlanFanoutParams>(
                input.clone(),
            )
            .map(|mut params| {
                // The HostState approval broker already gated this call;
                // the tool itself cannot pre-approve a plan rewrite.
                params.session_id = session.id.clone();
                params.approved = true;
                params.author = Some(spec.role.as_str().to_string());
                params
            });
            match params {
                Ok(params) => match engine.fanout_plan(params).await {
                    Ok(result) => ToolOutput::ok_structured(
                        format!(
                            "Plan fan-out applied at revision {}: {} child task(s), join {}.",
                            result.revision,
                            result.child_task_ids.len(),
                            result.join_task_id
                        ),
                        serde_json::to_value(&result).unwrap_or_else(|_| serde_json::json!({})),
                    ),
                    Err(error) => {
                        let code = error
                            .downcast_ref::<crate::fanout::FanoutError>()
                            .map(|typed| typed.code)
                            .unwrap_or(crate::fanout::codes::FANOUT_INVALID);
                        let tool_code = if code == crate::fanout::codes::FANOUT_CONFLICT {
                            crate::tooling::errors::ToolErrorCode::CONFLICT
                        } else {
                            crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT
                        };
                        ToolOutput::error_coded(
                            call.name.as_str(),
                            tool_code,
                            format!("plan fanout failed: {error}"),
                        )
                    }
                },
                Err(error) => ToolOutput::error_coded(
                    call.name.as_str(),
                    crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
                    format!("invalid plan fanout arguments: {error}"),
                ),
            }
        } else if is_coordination_tool {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            match run_coordination_tool(engine, session, spec, input.clone()).await {
                Ok(output) => output,
                Err(error) => ToolOutput::error(format!("coordination error: {error}")),
            }
        } else if is_team_status {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            match run_team_status(engine, session, input.clone()).await {
                Ok(output) => output,
                Err(error) => ToolOutput::error(format!("team status error: {error}")),
            }
        } else if is_budget_status {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            match run_budget_status(engine, session, project, spec, budget, plan_budget).await {
                Ok(output) => output,
                Err(error) => ToolOutput::error(format!("budget status error: {error}")),
            }
        } else if is_task_detail {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            match run_task_detail(engine, session, spec, input.clone()).await {
                Ok(output) => output,
                Err(error) => ToolOutput::error(format!("task detail error: {error}")),
            }
        } else if is_task_control {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            match run_task_control(engine, session, spec, input.clone()).await {
                Ok(output) => output,
                Err(error) => ToolOutput::from_tool_error(&call.name, &error),
            }
        } else if is_tool_help {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            run_tool_help(spec, &input)
        } else if is_tool_search {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            let output = match run_tool_search(
                config,
                engine,
                spec,
                &session.project_id,
                frozen_bundle,
                surface,
                input.clone(),
            )
            .await
            {
                Ok(output) => output,
                Err(error) => ToolOutput::error(format!("tool search error: {error}")),
            };
            if let Some(candidates) = output
                .structured
                .as_ref()
                .and_then(|value| value.get("promotion"))
                .filter(|promotion| {
                    promotion
                        .get("requested")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                })
                .and_then(|promotion| promotion.get("candidates"))
                .and_then(Value::as_array)
            {
                for candidate in candidates.iter().filter_map(Value::as_str) {
                    pending_promotions.insert(candidate.to_string());
                }
            }
            output
        } else if is_artifact_file {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            match run_publish_artifact_file(engine, session, spec, input.clone()).await {
                Ok(output) => output,
                Err(error) => ToolOutput::error(format!("artifact publish error: {error}")),
            }
        } else if is_archive_tool {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            run_archive_tool(engine, session, &call.name, input.clone()).await
        } else if is_media_tool {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            run_media_tool(engine, session, spec, &call.name, input.clone()).await
        } else if is_browser {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            match tool_token
                .scoped(engine.browser_action(
                    &session.id,
                    &spec.worktree,
                    &spec.id,
                    &spec.name,
                    input.clone(),
                ))
                .await
            {
                Ok(output) => output,
                Err(error) => ToolOutput::from_tool_error("browser", &error),
            }
        } else if is_computer {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            match tool_token
                .scoped(engine.computer_action(
                    &session.id,
                    &spec.worktree,
                    &spec.id,
                    &spec.name,
                    spec.task_id.as_deref(),
                    input.clone(),
                ))
                .await
            {
                Ok(output) => output,
                Err(error) => ToolOutput::from_tool_error("computer", &error),
            }
        } else if is_mailbox_tool {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            match run_mailbox_tool(engine, session, spec, &call.name, input.clone()).await {
                Ok(output) => output,
                Err(error) => ToolOutput::from_tool_error(&call.name, &error),
            }
        } else if let Some(binding) = dynamic_mcp_tool {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            let context = crate::mcp::tasks::McpTaskContext::from_session(
                &session.project_id,
                &session.id,
                &spec.id,
                &spec.name,
                spec.task_id.as_deref(),
                effect_begin.as_ref().map(|receipt| receipt.id.as_str()),
            );
            match tool_token
                .scoped(engine.mcp_call_with_context(
                    &binding.server_id,
                    &binding.tool_name,
                    input.clone(),
                    context,
                ))
                .await
            {
                Ok(result) => {
                    let mut text = format_mcp_tool_result(&result.result);
                    if let Some(task) = &result.task {
                        text.push_str(&format!(
                                "\nAsync MCP task {} is {}. Poll explicitly with mcp action=task_result task_id={} (poll interval {} ms).",
                                task.id, task.status, task.id, task.poll_interval_ms
                            ));
                    }
                    ToolOutput::ok(text)
                }
                Err(error) => tool_token
                    .snapshot()
                    .map(|snapshot| crate::preemption::tool_output("mcp", &snapshot))
                    .unwrap_or_else(|| ToolOutput::from_tool_error("mcp", &error)),
            }
        } else if is_mcp_tool {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            match tool_token
                .scoped(run_mcp_tool(
                    engine,
                    session,
                    spec,
                    input.clone(),
                    effect_begin.as_ref().map(|receipt| receipt.id.as_str()),
                ))
                .await
            {
                Ok(output) => output,
                Err(error) => tool_token
                    .snapshot()
                    .map(|snapshot| crate::preemption::tool_output("mcp", &snapshot))
                    .unwrap_or_else(|| ToolOutput::from_tool_error("mcp", &error)),
            }
        } else if is_wait_for_gate {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            let gate_id = input
                .get("gate_id")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|gate_id| !gate_id.is_empty());
            if let Some(gate_id) = gate_id {
                let note = input.get("note").and_then(serde_json::Value::as_str);
                match engine.create_gate_wait(&session.id, gate_id, note).await {
                    Ok((gate, continuation)) => {
                        let summary = format!(
                            "Waiting for external gate {} ({}, {}).",
                            gate.label,
                            gate.kind.as_str(),
                            gate.state.as_str()
                        );
                        *deferred_wait = Some(DeferredWait {
                            gate_id: gate.id.clone(),
                            continuation_id: continuation.id.clone(),
                            summary: summary.clone(),
                        });
                        ToolOutput::ok_structured(
                            format!(
                                "{summary} The run will end after this tool result; no Worker slot is held."
                            ),
                            serde_json::json!({
                                "waiting": true,
                                "gate_id": gate.id,
                                "kind": gate.kind.as_str(),
                                "label": gate.label,
                                "state": gate.state.as_str(),
                                "continuation_id": continuation.id,
                                "status": continuation.status,
                            }),
                        )
                    }
                    Err(error) => gate_tool_error("wait_for_gate", &error),
                }
            } else {
                ToolOutput::error_coded(
                    "wait_for_gate",
                    crate::tooling::errors::ToolErrorCode::GATE_INVALID,
                    "gate_id is required and must name an existing open gate",
                )
            }
        } else if is_question_tool {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            let question = input
                .get("question")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("question is required"))?;
            let options = input
                .get("options")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>();
            let (item, receiver) = engine
                .request_question(&session.id, &spec.id, &spec.name, question, options)
                .await?;
            let question_step = journal
                .question_intent(engine, turn + 1, &item.id, &item.question, &item.options)
                .await?;
            engine
                .update_session_status(&session.id, "waiting_input")
                .await?;
            engine
                .update_agent_status(&spec.id, "waiting_input")
                .await?;
            emit(
                engine,
                &session.id,
                spec,
                "agent.waiting_input",
                serde_json::json!({
                    "question_id": item.id,
                    "question": item.question,
                    "options": item.options,
                }),
            )
            .await?;
            match wait_for_question(receiver, control, config.question_timeout_secs).await {
                QuestionOutcome::Answered(answer) => {
                    journal
                        .question_completed(engine, &question_step, "answered")
                        .await?;
                    resume_session_after_question(engine, &session.id).await?;
                    engine.update_agent_status(&spec.id, "running").await?;
                    ToolOutput::ok(format!("User answered: {answer}"))
                }
                QuestionOutcome::Cancelled => {
                    let _ = engine.close_question(&item.id, "cancelled").await;
                    journal
                        .question_completed(engine, &question_step, "cancelled")
                        .await?;
                    engine.update_agent_status(&spec.id, "cancelled").await?;
                    return Ok(Some(LoopOutcome::Cancelled));
                }
                QuestionOutcome::TimedOut => {
                    let _ = engine.close_question(&item.id, "expired").await;
                    journal
                        .question_completed(engine, &question_step, "timed_out")
                        .await?;
                    resume_session_after_question(engine, &session.id).await?;
                    engine.update_agent_status(&spec.id, "running").await?;
                    ToolOutput::error("User did not answer before the question timed out.")
                }
            }
        } else if is_skill_tool {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            let output =
                match run_skill_tool(engine, project, spec, input.clone(), active_skill).await {
                    Ok(output) => output,
                    Err(error) => ToolOutput::error(format!("skill error: {error}")),
                };
            if !output.is_error
                && let Some(skill) = &active_skill
            {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "skill.activated",
                    serde_json::json!({
                        "name": skill.name,
                        "slug": skill.slug,
                        "version": skill.version,
                        "role": spec.role.as_str(),
                        "profile": spec.profile.as_str(),
                        "tool_allow": skill.tool_allow,
                        "tool_deny": skill.tool_deny,
                        "agent_id": spec.id,
                        "agent_name": spec.name,
                        "task_id": spec.task_id,
                    }),
                )
                .await?;
            }
            output
        } else if is_source_tool {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            match run_source_tool(engine, project, &call.name, input.clone(), citations).await {
                Ok(output) => output,
                Err(error) => ToolOutput::from_tool_error(&call.name, &error),
            }
        } else if is_read_tool_output {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            run_read_tool_output(engine, session, input.clone()).await
        } else if is_memory_tool {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            match run_memory_tool(engine, session, project, spec, &call.name, input.clone()).await {
                Ok(output) => output,
                Err(error) => ToolOutput::from_tool_error(&call.name, &error),
            }
        } else if is_verdict {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            match parse_verdict(&input) {
                Ok(verdict) => {
                    let verdict_output = ToolOutput::ok("verdict recorded");
                    if let Some((journal_step, journal_tool)) = &journal_pair {
                        journal
                            .tool_completed(engine, journal_step, journal_tool, &verdict_output)
                            .await?;
                    }
                    emit(
                        engine,
                        &session.id,
                        spec,
                        "tool.finished",
                        tool_finished_payload(
                            &call_id,
                            &call.name,
                            &verdict_output,
                            tool_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                            Some(approval_risk),
                            config.approval_mode,
                            approval_wait_ms,
                        ),
                    )
                    .await?;
                    return Ok(Some(LoopOutcome::Verdict(verdict)));
                }
                Err(error) => ToolOutput::error_coded(
                    "submit_verdict",
                    crate::tooling::errors::ToolErrorCode::VERDICT_INVALID,
                    error.to_string(),
                ),
            }
        } else if is_delegate_many {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            match run_parallel_workers(
                engine, config, session, project, control, budget, spec, &input,
            )
            .await?
            {
                ParallelResult::Output(output) => output,
                ParallelResult::Cancelled => return Ok(Some(LoopOutcome::Cancelled)),
                ParallelResult::BudgetExceeded(stop) => {
                    return Ok(Some(LoopOutcome::BudgetExceeded(stop)));
                }
                ParallelResult::Deferred(wait) => {
                    return Ok(Some(LoopOutcome::Deferred(wait)));
                }
            }
        } else if is_scout {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            match run_scout(
                engine, config, session, project, control, budget, spec, &input,
            )
            .await?
            {
                ScoutRunResult::Output(output) => output,
                ScoutRunResult::Cancelled => return Ok(Some(LoopOutcome::Cancelled)),
                ScoutRunResult::BudgetExceeded(stop) => {
                    if stop.is_abortive() {
                        return Ok(Some(LoopOutcome::BudgetExceeded(stop)));
                    }
                    ToolOutput::error(stop.summary())
                }
            }
        } else if is_delegate {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            match parse_delegate(&input, spec.role, spec.profile, team_plan.as_ref()) {
                Ok((role, name, child_objective, title, child_profile)) => {
                    let reservation = if role == AgentRole::Manager {
                        budget.reserve(role).map(|ordinal| (ordinal, None))
                    } else {
                        budget
                            .reserve_slot(role)
                            .map(|(ordinal, slot)| (ordinal, Some(slot)))
                    };
                    match reservation {
                        None => ToolOutput::error(format!(
                            "the {} cap for this session has been reached",
                            role.as_str()
                        )),
                        Some((ordinal, _slot)) => {
                            let child_name = name.clone().unwrap_or_else(|| {
                                title.clone().unwrap_or_else(|| {
                                    format!("{} {}", role.default_name(), ordinal)
                                })
                            });
                            let child = engine
                                .create_agent(
                                    &session.id,
                                    role.as_str(),
                                    &child_name,
                                    title.clone(),
                                    Some(spec.id.clone()),
                                    &model_for(config, role),
                                )
                                .await?;
                            engine
                                .set_agent_profile(&child.id, child_profile.as_str())
                                .await?;
                            let write_scope = parse_write_scope(&input)?;
                            let workspace_binding = if role == AgentRole::Worker {
                                engine
                                    .attach_worker_workspace_for_run(
                                        &session.id,
                                        title.as_deref(),
                                        spec.department.as_deref(),
                                    )
                                    .await?
                            } else {
                                None
                            };
                            // Bind the child run to its own project identity.
                            // A Worker uses the workspace it just attached;
                            // a Manager/Worker without a workspace resolves by
                            // role/title/department. Labour inherits the
                            // Worker cell that owns its work.
                            let (child_identity_id, child_identity_generation) = match role {
                                AgentRole::Worker => match workspace_binding.as_ref() {
                                    Some(binding) => {
                                        let identity_id = binding.workspace.identity_id.clone();
                                        let generation = engine
                                            .get_agent_identity(&identity_id)
                                            .await
                                            .ok()
                                            .map(|result| result.identity.generation);
                                        (Some(identity_id), generation)
                                    }
                                    None => engine
                                        .prompt_identity_for_run(
                                            &session.id,
                                            AgentRole::Worker.as_str(),
                                            title.as_deref(),
                                            spec.department.as_deref(),
                                        )
                                        .await?
                                        .map(|identity| (identity.id, identity.generation))
                                        .map(|(id, generation)| (Some(id), Some(generation)))
                                        .unwrap_or((None, None)),
                                },
                                AgentRole::Manager => engine
                                    .prompt_identity_for_run(
                                        &session.id,
                                        AgentRole::Manager.as_str(),
                                        title.as_deref(),
                                        spec.department.as_deref(),
                                    )
                                    .await?
                                    .map(|identity| (identity.id, identity.generation))
                                    .map(|(id, generation)| (Some(id), Some(generation)))
                                    .unwrap_or((None, None)),
                                AgentRole::Labour => {
                                    (spec.identity_id.clone(), spec.identity_generation)
                                }
                                _ => (None, None),
                            };
                            let labour_child_worktree =
                                role == AgentRole::Labour && spec.session_workspace_id.is_some();
                            let (
                                child_worktree,
                                child_branch,
                                child_base,
                                child_scope,
                                child_workspace_id,
                                child_session_workspace_id,
                                child_parent_workspace_id,
                            ) = match role {
                                AgentRole::Worker => match workspace_binding {
                                    Some(binding) => {
                                        let head_path =
                                            PathBuf::from(&binding.session.worktree_path);
                                        let head = tokio::task::spawn_blocking(move || {
                                            worktree::head_commit(&head_path)
                                        })
                                        .await??;
                                        (
                                            PathBuf::from(&binding.session.worktree_path),
                                            binding.session.branch.clone(),
                                            head,
                                            write_scope,
                                            Some(binding.workspace.id.clone()),
                                            Some(binding.session.id.clone()),
                                            None,
                                        )
                                    }
                                    None => {
                                        let info = engine
                                            .create_task_worktree(
                                                &project.root,
                                                &project.id,
                                                &session.id,
                                                &child.id,
                                                &spec.session_branch,
                                            )
                                            .await?;
                                        (
                                            info.path,
                                            info.branch,
                                            info.base_commit,
                                            write_scope,
                                            None,
                                            None,
                                            None,
                                        )
                                    }
                                },
                                AgentRole::Manager => (
                                    spec.session_worktree.clone(),
                                    spec.session_branch.clone(),
                                    spec.base_commit.clone(),
                                    Vec::new(),
                                    None,
                                    None,
                                    None,
                                ),
                                AgentRole::Labour if labour_child_worktree => {
                                    let info = engine
                                        .create_task_worktree(
                                            &project.root,
                                            &project.id,
                                            &session.id,
                                            &child.id,
                                            &spec.branch,
                                        )
                                        .await?;
                                    (
                                        info.path,
                                        info.branch,
                                        info.base_commit,
                                        spec.write_scope.clone(),
                                        None,
                                        None,
                                        spec.session_workspace_id.clone(),
                                    )
                                }
                                AgentRole::Labour => (
                                    spec.worktree.clone(),
                                    spec.branch.clone(),
                                    spec.base_commit.clone(),
                                    spec.write_scope.clone(),
                                    None,
                                    None,
                                    None,
                                ),
                                AgentRole::Leader | AgentRole::Scout => (
                                    spec.session_worktree.clone(),
                                    spec.session_branch.clone(),
                                    spec.base_commit.clone(),
                                    Vec::new(),
                                    None,
                                    None,
                                    None,
                                ),
                                AgentRole::Verifier => (
                                    spec.worktree.clone(),
                                    spec.branch.clone(),
                                    spec.base_commit.clone(),
                                    Vec::new(),
                                    None,
                                    None,
                                    None,
                                ),
                            };
                            engine
                                .set_agent_worktree(
                                    &child.id,
                                    &child_worktree.to_string_lossy(),
                                    &child_branch,
                                )
                                .await?;
                            engine
                                .set_agent_workspace_binding(
                                    &child.id,
                                    child_workspace_id.as_deref(),
                                    child_parent_workspace_id.as_deref(),
                                )
                                .await?;
                            engine
                                .set_agent_task(
                                    &child.id,
                                    (role == AgentRole::Labour)
                                        .then(|| spec.task_id.clone())
                                        .flatten(),
                                    spec.department.clone(),
                                )
                                .await?;
                            let mut child_spec = AgentSpec {
                                id: child.id.clone(),
                                role,
                                name: child_name.clone(),
                                depth: spec.depth.saturating_add(1),
                                parent_name: Some(spec.name.clone()),
                                title: title.clone(),
                                task_id: if role == AgentRole::Labour {
                                    spec.task_id.clone()
                                } else {
                                    None
                                },
                                department: spec.department.clone(),
                                identity_id: child_identity_id,
                                identity_generation: child_identity_generation,
                                profile: child_profile,
                                task_kind: spec.task_kind.clone(),
                                allow_delegation: role.child_role().is_some(),
                                auto_approve: false,
                                worktree: child_worktree,
                                branch: child_branch,
                                session_worktree: spec.session_worktree.clone(),
                                session_branch: spec.session_branch.clone(),
                                write_scope: child_scope,
                                base_commit: child_base,
                                workspace_id: child_workspace_id,
                                session_workspace_id: child_session_workspace_id,
                                parent_workspace_id: child_parent_workspace_id,
                            };
                            emit(
                                engine,
                                &session.id,
                                &child_spec,
                                "agent.spawned",
                                serde_json::json!({
                                    "parent_id": spec.id,
                                    "parent_name": spec.name,
                                    "objective": truncate(child_objective.as_str()),
                                }),
                            )
                            .await?;
                            let mut child_conversation =
                                vec![ChatMessage::user(child_objective.clone())];
                            let workspace_lock = child_spec
                                .workspace_id
                                .as_ref()
                                .map(|workspace_id| engine.workspace_run_lock(workspace_id));
                            let child_result = match workspace_lock {
                                Some(lock) => {
                                    let _guard = lock.lock().await;
                                    refresh_workspace_base(&mut child_spec).await;
                                    run_loop(
                                        engine,
                                        config,
                                        session,
                                        project,
                                        control,
                                        budget,
                                        &child_spec,
                                        &child_objective,
                                        &mut child_conversation,
                                        None,
                                        false,
                                        None,
                                    )
                                    .await
                                }
                                None => {
                                    run_loop(
                                        engine,
                                        config,
                                        session,
                                        project,
                                        control,
                                        budget,
                                        &child_spec,
                                        &child_objective,
                                        &mut child_conversation,
                                        None,
                                        false,
                                        None,
                                    )
                                    .await
                                }
                            };
                            match child_result {
                                Ok(LoopOutcome::Verdict(_)) => ToolOutput::error(
                                    "a child agent returned a verdict unexpectedly",
                                ),
                                Ok(LoopOutcome::Final(summary)) => {
                                    let summary = if summary.trim().is_empty() {
                                        "The agent completed without a summary.".to_string()
                                    } else {
                                        summary
                                    };
                                    if role == AgentRole::Worker {
                                        match finish_worker(
                                            engine,
                                            config,
                                            session,
                                            project,
                                            control,
                                            budget,
                                            spec,
                                            &child_spec,
                                            &child_objective,
                                            &summary,
                                            None,
                                        )
                                        .await?
                                        {
                                            WorkerFinish::Done { report, success } => {
                                                let text = format!(
                                                    "Worker {} finished:\n{}",
                                                    child_name, report
                                                );
                                                if success {
                                                    ToolOutput::ok(text)
                                                } else {
                                                    ToolOutput::error(text)
                                                }
                                            }
                                            WorkerFinish::Cancelled => {
                                                return Ok(Some(LoopOutcome::Cancelled));
                                            }
                                            WorkerFinish::BudgetExceeded(stop) => {
                                                if stop.is_abortive() {
                                                    return Ok(Some(LoopOutcome::BudgetExceeded(
                                                        stop,
                                                    )));
                                                }
                                                ToolOutput::error(stop.summary())
                                            }
                                        }
                                    } else if role == AgentRole::Labour
                                        && child_spec.parent_workspace_id.is_some()
                                    {
                                        match finish_labour(
                                            engine,
                                            session,
                                            spec,
                                            &child_spec,
                                            &summary,
                                        )
                                        .await?
                                        {
                                            WorkerFinish::Done { report, success } => {
                                                let text = format!(
                                                    "Labour {} finished:\n{}",
                                                    child_name, report
                                                );
                                                if success {
                                                    ToolOutput::ok(text)
                                                } else {
                                                    ToolOutput::error(text)
                                                }
                                            }
                                            WorkerFinish::Cancelled => {
                                                return Ok(Some(LoopOutcome::Cancelled));
                                            }
                                            WorkerFinish::BudgetExceeded(stop) => {
                                                if stop.is_abortive() {
                                                    return Ok(Some(LoopOutcome::BudgetExceeded(
                                                        stop,
                                                    )));
                                                }
                                                ToolOutput::error(stop.summary())
                                            }
                                        }
                                    } else {
                                        engine.update_agent_status(&child.id, "idle").await?;
                                        emit(
                                            engine,
                                            &session.id,
                                            &child_spec,
                                            "agent.finished",
                                            serde_json::json!({
                                                "status": "idle",
                                                "summary": truncate(&summary),
                                                "worktree": child_spec.worktree,
                                                "branch": child_spec.branch,
                                            }),
                                        )
                                        .await?;
                                        ToolOutput::ok(format!(
                                            "{} {} finished:\n{}",
                                            role.default_name(),
                                            child_name,
                                            truncate(&summary)
                                        ))
                                    }
                                }
                                Ok(LoopOutcome::Deferred(wait)) => {
                                    engine.update_agent_status(&child.id, "idle").await?;
                                    emit(
                                        engine,
                                        &session.id,
                                        &child_spec,
                                        "agent.finished",
                                        serde_json::json!({
                                            "status": "waiting_gate",
                                            "gate_id": wait.gate_id,
                                            "continuation_id": wait.continuation_id,
                                        }),
                                    )
                                    .await?;
                                    match role {
                                        AgentRole::Manager => {
                                            // A Manager wait suspends the
                                            // top-level run so no Manager or
                                            // Worker slot stays occupied.
                                            return Ok(Some(LoopOutcome::Deferred(wait)));
                                        }
                                        AgentRole::Worker => ToolOutput::ok_structured(
                                            format!(
                                                "Worker {} is waiting for external gate {}. Its run ended without a commit; the continuation will resume the session when the gate settles.",
                                                child_name, wait.gate_id
                                            ),
                                            serde_json::json!({
                                                "waiting": true,
                                                "gate_id": wait.gate_id,
                                                "continuation_id": wait.continuation_id,
                                                "worker_id": child.id,
                                            }),
                                        ),
                                        _ => ToolOutput::error(format!(
                                            "{} cannot wait on an external gate",
                                            role.default_name()
                                        )),
                                    }
                                }
                                Ok(LoopOutcome::Cancelled) => {
                                    engine.update_agent_status(&child.id, "cancelled").await?;
                                    emit(
                                        engine,
                                        &session.id,
                                        &child_spec,
                                        "agent.finished",
                                        serde_json::json!({"status": "cancelled"}),
                                    )
                                    .await?;
                                    return Ok(Some(LoopOutcome::Cancelled));
                                }
                                Ok(LoopOutcome::BudgetExceeded(stop)) => {
                                    engine.update_agent_status(&child.id, "failed").await?;
                                    emit(
                                        engine,
                                        &session.id,
                                        &child_spec,
                                        "agent.finished",
                                        serde_json::json!({
                                            "status": "failed",
                                            "summary": stop.summary(),
                                        }),
                                    )
                                    .await?;
                                    return Ok(Some(LoopOutcome::BudgetExceeded(stop)));
                                }
                                Err(error) => {
                                    engine.update_agent_status(&child.id, "failed").await?;
                                    emit(
                                        engine,
                                        &session.id,
                                        &child_spec,
                                        "agent.finished",
                                        serde_json::json!({
                                            "status": "failed",
                                            "summary": truncate(error.to_string().as_str()),
                                        }),
                                    )
                                    .await?;
                                    ToolOutput::error(format!(
                                        "{} {} failed: {error}",
                                        role.default_name(),
                                        child_name
                                    ))
                                }
                            }
                        }
                    }
                }
                Err(error) => ToolOutput::error(error.to_string()),
            }
        } else if is_acp_delegate {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            match delegate_acp(
                engine, config, session, project, control, budget, spec, &input,
            )
            .await?
            {
                AcpDelegation::Output(output) => output,
                AcpDelegation::Cancelled => return Ok(Some(LoopOutcome::Cancelled)),
                AcpDelegation::BudgetExceeded(stop) => {
                    return Ok(Some(LoopOutcome::BudgetExceeded(stop)));
                }
            }
        } else if is_todo {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            match parse_todos(&input) {
                Ok(items) => match engine.set_todos(&session.id, items).await {
                    Ok(()) => ToolOutput::ok("Updated the session plan.".to_string()),
                    Err(error) => ToolOutput::error(format!("todo_write failed: {error}")),
                },
                Err(error) => ToolOutput::error(format!("todo_write failed: {error}")),
            }
        } else if let Some(tool) = registry_tool {
            emit(
                engine,
                &session.id,
                spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "input": input,
                }),
            )
            .await?;
            let mut cancel_rx = control.subscribe();
            if control.is_cancelled() {
                return Ok(Some(LoopOutcome::Cancelled));
            }
            let observes_token = crate::preemption::tool_observes_token(&call.name);
            let result = if observes_token {
                let name = call.name.clone();
                let context = tool_context.clone();
                tool_token
                    .scoped(Box::pin(async move { tool.execute(&context, input).await }))
                    .await
                    .map_err(|error| (name, error))
            } else {
                let context = tool_context.clone();
                tokio::select! {
                    _ = cancel_rx.changed() => {
                        return Ok(Some(LoopOutcome::Cancelled));
                    }
                    _ = tool_token.cancelled() => {
                        let snapshot = tool_token.snapshot().unwrap_or_else(|| {
                            crate::preemption::fallback_snapshot(
                                crate::preemption::Phase::Tool,
                                Some(&call.name),
                            )
                        });
                        imposed_preempt = Some(snapshot.clone());
                        Ok(crate::preemption::tool_output(&call.name, &snapshot))
                    }
                    result = tool_token.scoped(Box::pin(async move {
                        tool.execute(&context, input).await
                    })) => result.map_err(|error| (call.name.clone(), error)),
                }
            };
            match result {
                Ok(output) => output,
                Err((_name, error)) => ToolOutput::error(format!("tool error: {error}")),
            }
        } else {
            ToolOutput::error_coded(
                call.name.as_str(),
                crate::tooling::errors::ToolErrorCode::TOOL_NOT_AVAILABLE,
                format!(
                    "tool '{}' is not available to {}",
                    call.name,
                    spec.role.as_str()
                ),
            )
        };
        let mut output = output;
        output.normalize_failure(&call.name);
        let output_preempt = imposed_preempt
            .take()
            .or_else(|| crate::preemption::output_snapshot(&output));
        let terminal_preempt = output_preempt
            .as_ref()
            .filter(|snapshot| {
                snapshot.reason != crate::preemption::Reason::ToolDeadline
                    || !tool_token.is_own_preempted()
            })
            .cloned();
        if let Some(snapshot) = &output_preempt {
            if snapshot.side_effects_unknown {
                control.preempt.mark_side_effects_unknown();
            }
            Box::pin(emit_tool_preempted(
                engine, session, project, spec, snapshot,
            ))
            .await?;
        }
        let tool_duration_ms = tool_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;

        let pending = vec![crate::tool_output::PendingToolOutput {
            tool_call_id: call_id.clone(),
            tool_name: call.name.clone(),
            output,
        }];
        let (mut shaped, next_turn_bytes) =
            shape_tool_output_batch(engine, session, spec, project, *turn_tool_bytes, pending)
                .await?;
        *turn_tool_bytes = next_turn_bytes;
        let mut output = shaped
            .pop()
            .ok_or_else(|| anyhow!("tool-output shaping returned no result"))?
            .output;
        output.annotate_shape_metadata();

        finish_tool_call(
            engine,
            session,
            spec,
            journal,
            &call_id,
            &call.name,
            &effect_input,
            &mut output,
            tool_duration_ms,
            Some(approval_risk),
            config.approval_mode,
            approval_wait_ms,
            planned_effect.as_ref(),
            effect_begin.take(),
            journal_pair.as_ref(),
            None,
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
        conversation.push(ChatMessage::tool(call_id, output.content));
        if let Some(wait) = deferred_wait.take() {
            // A gate wait is a clean handoff: do not execute any remaining
            // sibling tool calls or make another provider request. Every
            // skipped sibling still records a tool result so the resumed
            // conversation has one result per assistant tool call.
            for (remaining_id, remaining_name) in serial_call_ids.iter().skip(index + 1) {
                let skipped = ToolOutput::error_coded(
                    remaining_name,
                    crate::tooling::errors::ToolErrorCode::TOOL_FAILED,
                    "not executed: the run deferred to wait for an external gate; re-issue this call after the gate resolves",
                );
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.finished",
                    tool_finished_payload(
                        remaining_id,
                        remaining_name,
                        &skipped,
                        0,
                        None,
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
                            &skipped.content,
                            serde_json::json!([]),
                            Some(remaining_id.clone()),
                        )
                        .await?;
                    conversation_ids.push(Some(stored.id));
                }
                conversation.push(ChatMessage::tool(remaining_id.clone(), skipped.content));
            }
            return Ok(Some(LoopOutcome::Deferred(wait)));
        }
        if terminal_preempt.is_some() {
            return Ok(Some(LoopOutcome::Cancelled));
        }
    }
    emit(
        engine,
        &session.id,
        spec,
        "agent.turn_finished",
        serde_json::json!({"turn": turn + 1}),
    )
    .await?;
    Ok(None)
}

/// Return the session to `running` after an `ask_user` answer/timeout without
/// overwriting a concurrent pause or cancellation. The store applies the
/// transition only from `waiting_input`; the event is broadcast only when the
/// row actually changed.
async fn resume_session_after_question(engine: &Engine, session_id: &str) -> Result<()> {
    let store = engine.store.clone();
    let session_id = session_id.to_string();
    let event = tokio::task::spawn_blocking(move || {
        store.resume_session_after_question_with_event(&session_id)
    })
    .await??;
    if let Some(event) = event {
        let _ = engine.events.send(event);
    }
    Ok(())
}

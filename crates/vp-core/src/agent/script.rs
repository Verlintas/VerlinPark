use super::*;

// `ToolOutput` is intentionally rich (text, structured data, evidence, and
// failure metadata), so boxing every success would add allocation churn on the
// common path for an enum with only one non-empty variant.
#[allow(clippy::large_enum_variant)]
pub(super) enum ScriptToolOutcome {
    Output(ToolOutput),
    Cancelled,
}

pub(super) struct ScriptInnerOutcome {
    pub(super) output: ToolOutput,
    pub(super) risk: RiskLevel,
    pub(super) approval_wait_ms: Option<u64>,
    pub(super) duration_ms: u64,
    pub(super) cancelled: bool,
}

/// Run one Code Mode script and route every inner call back through the
/// dispatcher gates. The outer `run_script` call is already journaled and
/// approval-classified by the caller; this function owns only the inner calls.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_script_tool(
    engine: &Engine,
    config: &AgentConfig,
    session: &Session,
    spec: &AgentSpec,
    control: &RunControl,
    journal: &crate::run_journal::RunJournal,
    input: &Value,
    advertised: &[vp_providers::ToolSchema],
    surface: ToolSurface,
    call_id: &str,
    turn: u32,
    run_contract: Option<&crate::contract::TaskContract>,
    run_contract_policy: &vp_protocol::types::ContractPolicy,
    run_contract_usage: &mut crate::contract::MutationUsage,
    active_skill: Option<&Skill>,
    gui_policy: &vp_protocol::types::GuiPolicy,
    effect_policy: &vp_protocol::types::EffectPolicy,
    tool_context: &ToolContext,
    tool_token: &crate::preemption::Token,
) -> Result<ScriptToolOutcome> {
    if !matches!(
        spec.role,
        AgentRole::Leader | AgentRole::Worker | AgentRole::Labour
    ) {
        return Ok(ScriptToolOutcome::Output(ToolOutput::error_coded(
            "run_script",
            crate::tooling::errors::ToolErrorCode::PERMISSION_DENIED,
            "this role cannot run Code Mode scripts",
        )));
    }
    let Some(source) = input.get("script").and_then(Value::as_str) else {
        return Ok(ScriptToolOutcome::Output(ToolOutput::error_coded(
            "run_script",
            crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
            "run_script requires a string `script`",
        )));
    };
    let allow_write = input
        .get("allow_write")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if allow_write
        && !matches!(
            spec.role,
            AgentRole::Leader | AgentRole::Worker | AgentRole::Labour
        )
    {
        return Ok(ScriptToolOutcome::Output(ToolOutput::error_coded(
            "run_script",
            crate::tooling::errors::ToolErrorCode::PERMISSION_DENIED,
            "this role cannot enable script write bindings",
        )));
    }
    // Code Mode binds only the capability-gated tools from the same frozen
    // surface as the provider request. A gated-off MCP/remote/media/GUI schema
    // can never become an inner binding through a permissive legacy list.
    let gated_advertised = advertised
        .iter()
        .filter(|tool| surface.capabilities.allows_tool(&tool.name))
        .cloned()
        .collect::<Vec<_>>();
    let bindings = crate::script::bindings(&gated_advertised, allow_write);
    if bindings.is_empty() {
        return Ok(ScriptToolOutcome::Output(ToolOutput::error_coded(
            "run_script",
            crate::tooling::errors::ToolErrorCode::SCRIPT_DISABLED,
            "no script-safe tools are available in this frozen bundle",
        )));
    }
    let limits = crate::script::ScriptLimits::from_input(input);
    let script_input = input.get("input").cloned().unwrap_or(Value::Null);
    let mut runner = match crate::script::spawn(source, script_input, bindings, limits.clone()) {
        Ok(runner) => runner,
        Err(error) => {
            return Ok(ScriptToolOutcome::Output(ToolOutput::error_coded(
                "run_script",
                crate::tooling::errors::ToolErrorCode::SCRIPT_INVALID,
                format!("{error:#}"),
            )));
        }
    };
    let mut records: Vec<Value> = Vec::new();
    let mut cancel_rx = control.cancelled.subscribe();
    loop {
        if control.is_cancelled() {
            return Ok(ScriptToolOutcome::Cancelled);
        }
        let event = tokio::select! {
            _ = cancel_rx.changed() => return Ok(ScriptToolOutcome::Cancelled),
            _ = tool_token.cancelled() => return Ok(ScriptToolOutcome::Cancelled),
            event = runner.next_event() => event,
        };
        let Some(event) = event else {
            return Ok(ScriptToolOutcome::Output(ToolOutput::error_coded(
                "run_script",
                crate::tooling::errors::ToolErrorCode::SCRIPT_FAILED,
                "script runner ended without a result",
            )));
        };
        match event {
            crate::script::ScriptEvent::Finished(result) => {
                let result = match result {
                    Ok(result) => result,
                    Err(error) => {
                        let message = format!("{error:#}");
                        let code = crate::script::classify_error(&message);
                        return Ok(ScriptToolOutcome::Output(
                            ToolOutput::error_coded("run_script", code, message)
                                .with_failure_evidence(format!(
                                    "script_calls={}; bindings={}",
                                    records.len(),
                                    records
                                        .iter()
                                        .filter_map(|record| record
                                            .get("tool")
                                            .and_then(Value::as_str))
                                        .take(8)
                                        .collect::<Vec<_>>()
                                        .join(",")
                                )),
                        ));
                    }
                };
                let description = input
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|description| !description.is_empty());
                let mut text = result.summary();
                if let Some(description) = description {
                    text = format!("{description}\n{text}");
                }
                if !result.stdout.is_empty() {
                    text.push_str("\nstdout:\n");
                    text.push_str(&result.stdout);
                }
                if let Some(value) = &result.value {
                    let encoded = serde_json::to_string(value).unwrap_or_else(|_| "null".into());
                    text.push_str("\nreturned: ");
                    text.push_str(&crate::tools::truncate(encoded));
                }
                let structured = json!({
                    "stdout": result.stdout,
                    "value": result.value,
                    "calls": records,
                    "elapsed_ms": result.elapsed_ms,
                    "host_calls": result.host_calls,
                    "write_calls": result.write_calls,
                    "stdout_truncated": result.stdout_truncated,
                    "allow_write": allow_write,
                    "max_calls": limits.max_calls,
                    "max_write_calls": limits.max_write_calls,
                    "timeout_ms": limits.wall_ms,
                    "tool_bundle": crate::tool_bundle::select_bundle_with_mode(
                        spec.role.as_str(),
                        spec.profile.as_str(),
                        spec.task_kind.as_deref(),
                        surface.mode,
                    )
                    .kind
                    .as_str(),
                    "tool_bundle_mode": surface.mode.as_str(),
                    "tool_surface": surface.payload(),
                });
                return Ok(ScriptToolOutcome::Output(ToolOutput::ok_structured(
                    crate::tools::truncate(text),
                    structured,
                )));
            }
            crate::script::ScriptEvent::Call(request) => {
                let outcome = execute_script_inner_call(
                    engine,
                    config,
                    session,
                    spec,
                    control,
                    journal,
                    call_id,
                    turn,
                    run_contract,
                    run_contract_policy,
                    run_contract_usage,
                    active_skill,
                    gui_policy,
                    effect_policy,
                    tool_context,
                    tool_token,
                    surface,
                    &request,
                )
                .await?;
                if outcome.cancelled {
                    let _ = request.reply.send(crate::script::ScriptReply {
                        output: ToolOutput::error_coded(
                            &request.tool,
                            crate::tooling::errors::ToolErrorCode::SCRIPT_CANCELLED,
                            "script stopped because the run was cancelled",
                        ),
                        aborted: true,
                    });
                    return Ok(ScriptToolOutcome::Cancelled);
                }
                records.push(json!({
                    "seq": request.seq,
                    "tool": request.tool,
                    "read_only": request.read_only,
                    "is_error": outcome.output.is_error,
                    "error_code": outcome
                        .output
                        .failure
                        .as_ref()
                        .map(|failure| failure.code.as_str()),
                    "duration_ms": outcome.duration_ms,
                    "approval_risk": outcome.risk.as_str(),
                    "approval_wait_ms": outcome.approval_wait_ms,
                }));
                let _ = request.reply.send(crate::script::ScriptReply {
                    output: outcome.output,
                    aborted: false,
                });
            }
        }
    }
}

/// Execute one inner script call through the same policy stack as a normal tool
/// call. The script bridge is not an authorization shortcut.
#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_script_inner_call(
    engine: &Engine,
    config: &AgentConfig,
    session: &Session,
    spec: &AgentSpec,
    control: &RunControl,
    journal: &crate::run_journal::RunJournal,
    parent_call_id: &str,
    turn: u32,
    run_contract: Option<&crate::contract::TaskContract>,
    run_contract_policy: &vp_protocol::types::ContractPolicy,
    run_contract_usage: &mut crate::contract::MutationUsage,
    active_skill: Option<&Skill>,
    gui_policy: &vp_protocol::types::GuiPolicy,
    effect_policy: &vp_protocol::types::EffectPolicy,
    tool_context: &ToolContext,
    tool_token: &crate::preemption::Token,
    surface: ToolSurface,
    request: &crate::script::ScriptCallRequest,
) -> Result<ScriptInnerOutcome> {
    let tool_name = request.tool.as_str();
    let call_id = format!("{parent_call_id}:inner:{}", request.seq);
    let started = Instant::now();
    let input = request.arguments.clone();
    let read_only = request.read_only;
    let mut approval_wait_ms: Option<u64> = None;
    let mut risk = RiskLevel::ReadOnly;

    if !surface.capabilities.allows_tool(tool_name) {
        return Ok(ScriptInnerOutcome {
            output: ToolOutput::error_coded(
                tool_name,
                crate::tooling::errors::ToolErrorCode::TOOL_NOT_AVAILABLE,
                format!("tool '{tool_name}' is not available in the current capability surface"),
            )
            .with_failure_evidence(format!(
                "tool={tool_name}; gated_off={:?}",
                surface.capabilities.gated_off()
            )),
            risk,
            approval_wait_ms,
            duration_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            cancelled: false,
        });
    }

    let registry_tool = config
        .tools
        .get(tool_name)
        .filter(|_| spec.role.allows_tool(spec.profile, tool_name))
        .clone();
    if registry_tool.is_none() {
        return Ok(ScriptInnerOutcome {
            output: ToolOutput::error_coded(
                tool_name,
                crate::tooling::errors::ToolErrorCode::TOOL_NOT_AVAILABLE,
                format!(
                    "tool '{tool_name}' is not available to {}",
                    spec.role.as_str()
                ),
            ),
            risk,
            approval_wait_ms,
            duration_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            cancelled: false,
        });
    }
    let allowed = if read_only {
        crate::script::READ_ONLY_TOOLS.contains(&tool_name)
    } else {
        crate::script::WRITE_TOOLS.contains(&tool_name)
    };
    if !allowed {
        return Ok(ScriptInnerOutcome {
            output: ToolOutput::error_coded(
                tool_name,
                crate::tooling::errors::ToolErrorCode::PERMISSION_DENIED,
                format!("script binding '{tool_name}' is not permitted in this mode"),
            ),
            risk,
            approval_wait_ms,
            duration_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            cancelled: false,
        });
    }
    let mut duplicate: Option<ToolOutput> = journal
        .check_duplicate_write(engine, tool_name, &input)
        .await?;
    let authorization = authorize_tool_call(
        engine,
        config,
        session,
        spec,
        control,
        journal,
        &call_id,
        tool_name,
        &input,
        turn,
        run_contract,
        run_contract_policy,
        run_contract_usage,
        active_skill,
        gui_policy,
        None,
        false,
        None,
        false,
        false,
        read_only,
        !read_only,
        Some(json!({
            "script": true,
            "parent_tool_call_id": parent_call_id,
            "script_call_id": call_id,
            "seq": request.seq,
        })),
        Some(("script_parent_tool_call_id", json!(parent_call_id))),
    )
    .await?;
    if authorization.cancelled {
        return Ok(ScriptInnerOutcome {
            output: ToolOutput::error_coded(
                tool_name,
                crate::tooling::errors::ToolErrorCode::SCRIPT_CANCELLED,
                "script stopped while waiting for approval",
            ),
            risk: authorization.risk,
            approval_wait_ms: authorization.approval_wait_ms,
            duration_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            cancelled: true,
        });
    }
    risk = authorization.risk;
    approval_wait_ms = authorization.approval_wait_ms;
    let denied_reason = authorization.denied_reason;
    let skill_denied = authorization.skill_denied;
    let effect_gate = prepare_tool_effect_gate(
        engine,
        session,
        spec,
        journal,
        turn,
        &call_id,
        tool_name,
        &input,
        risk,
        denied_reason.is_some(),
        skill_denied.is_some(),
        false,
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
    let mut output = if let Some(mut output) = effect_override {
        if let Some(receipt) = &effect_begin {
            annotate_effect_output(&mut output, receipt);
        }
        output
    } else if let Some(mut output) = duplicate.take() {
        if let Some(receipt) = &effect_duplicate {
            annotate_effect_output(&mut output, receipt);
        }
        output
    } else if let Some(reason) = denied_reason {
        ToolOutput::error_coded(
            tool_name,
            crate::tooling::errors::ToolErrorCode::PERMISSION_DENIED,
            reason,
        )
        .with_failure_evidence(format!("tool={tool_name}"))
    } else if let Some(skill) = skill_denied {
        ToolOutput::error_coded(
            tool_name,
            crate::tooling::errors::ToolErrorCode::SKILL_TOOL_DENIED,
            format!(
                "tool '{}' is not allowed by active skill '{}'",
                tool_name, skill.name
            ),
        )
        .with_failure_evidence(format!("skill={}", skill.slug))
    } else {
        emit(
            engine,
            &session.id,
            spec,
            "tool.started",
            json!({
                "tool_call_id": call_id,
                "name": tool_name,
                "input": input,
                "script": true,
                "script_parent_tool_call_id": parent_call_id,
                "script_seq": request.seq,
            }),
        )
        .await?;
        control
            .preempt
            .set_phase(crate::preemption::Phase::Tool, Some(tool_name));
        let registry_tool = registry_tool.expect("checked above");
        let context = tool_context.clone();
        let name = tool_name.to_string();
        let execution = tokio::select! {
            _ = tool_token.cancelled() => None,
            result = registry_tool.execute(&context, input.clone()) => Some(result),
        };
        match execution {
            None => {
                let snapshot = tool_token.snapshot().unwrap_or_else(|| {
                    crate::preemption::fallback_snapshot(
                        crate::preemption::Phase::Tool,
                        Some(tool_name),
                    )
                });
                crate::preemption::tool_output(tool_name, &snapshot)
            }
            Some(Ok(output)) => output,
            Some(Err(error)) => ToolOutput::from_tool_error(&name, &error),
        }
    };
    output.normalize_failure(tool_name);
    let duration_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    finish_tool_call(
        engine,
        session,
        spec,
        journal,
        &call_id,
        tool_name,
        &input,
        &mut output,
        duration_ms,
        Some(risk),
        config.approval_mode,
        approval_wait_ms,
        planned_effect.as_ref(),
        effect_begin.take(),
        journal_pair.as_ref(),
        Some((
            "script",
            json!({
                "parent_tool_call_id": parent_call_id,
                "seq": request.seq,
                "read_only": read_only,
            }),
        )),
    )
    .await?;
    Ok(ScriptInnerOutcome {
        output,
        risk,
        approval_wait_ms,
        duration_ms,
        cancelled: false,
    })
}

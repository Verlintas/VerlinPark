use super::*;

pub(super) fn agent_fields(payload: &mut serde_json::Value, spec: &AgentSpec) {
    if let Some(object) = payload.as_object_mut() {
        object.insert("agent_id".into(), serde_json::json!(spec.id));
        object.insert("agent_role".into(), serde_json::json!(spec.role.as_str()));
        object.insert("agent_name".into(), serde_json::json!(spec.name));
        object.insert("agent_depth".into(), serde_json::json!(spec.depth));
        object.insert(
            "agent_profile".into(),
            serde_json::json!(spec.profile.as_str()),
        );
        if let Some(title) = &spec.title {
            object.insert("agent_title".into(), serde_json::json!(title));
        }
        if let Some(task_id) = &spec.task_id {
            object.insert("task_id".into(), serde_json::json!(task_id));
        }
        if let Some(department) = &spec.department {
            object.insert("department".into(), serde_json::json!(department));
        }
        if let Some(identity_id) = &spec.identity_id {
            object.insert("identity_id".into(), serde_json::json!(identity_id));
        }
        if let Some(generation) = spec.identity_generation {
            object.insert("identity_generation".into(), serde_json::json!(generation));
        }
    }
}

pub(super) async fn emit(
    engine: &Engine,
    session_id: &str,
    spec: &AgentSpec,
    kind: &str,
    mut payload: serde_json::Value,
) -> Result<()> {
    agent_fields(&mut payload, spec);
    engine.append_event(session_id, kind, payload).await?;
    Ok(())
}

/// Parse one provider tool-call argument string. An empty/whitespace argument
/// string is the unambiguous no-argument form; any other non-object JSON is
/// rejected instead of being silently replaced with `{}`. The caller turns the
/// error into a typed `INVALID_ARGUMENT` tool result and never executes the
/// tool with guessed input.
pub(super) fn parse_tool_arguments(
    arguments: &str,
) -> std::result::Result<serde_json::Value, String> {
    if arguments.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }
    let value: serde_json::Value = serde_json::from_str(arguments)
        .map_err(|error| format!("tool arguments are not valid JSON: {error}"))?;
    if !value.is_object() {
        return Err("tool arguments must be a JSON object".to_string());
    }
    Ok(value)
}

/// Cold diagnostics for a preempted provider stream. Kept in a separate async
/// function and called through `Box::pin` so the recursive run-loop future
/// does not retain this formatting/emission state at every delegation depth.
pub(super) async fn emit_provider_preempted(
    engine: &Engine,
    session: &Session,
    project: &Project,
    spec: &AgentSpec,
    snapshot: &crate::preemption::Snapshot,
    text: &str,
    tool_calls: &[ToolCall],
) -> Result<()> {
    if !text.is_empty() {
        emit(
            engine,
            &session.id,
            spec,
            "agent.message",
            serde_json::json!({
                "text": text,
                "tool_calls": tool_calls,
                "partial": true,
                "preempted": true,
                "preempt_reason": snapshot.reason.as_str(),
            }),
        )
        .await?;
    }
    let attribution = crate::preemption::PreemptAttribution {
        session_id: &session.id,
        project_id: &project.id,
        agent_id: &spec.id,
        agent_name: &spec.name,
        agent_role: spec.role.as_str(),
        task_id: spec.task_id.as_deref(),
        department: spec.department.as_deref(),
    };
    emit(
        engine,
        &session.id,
        spec,
        "provider.preempted",
        serde_json::to_value(crate::preemption::record(
            &attribution,
            snapshot,
            false,
            None,
            None,
            None,
        ))?,
    )
    .await
}

/// Cold diagnostics for a preempted tool. See `emit_provider_preempted`.
pub(super) async fn emit_tool_preempted(
    engine: &Engine,
    session: &Session,
    project: &Project,
    spec: &AgentSpec,
    snapshot: &crate::preemption::Snapshot,
) -> Result<()> {
    let attribution = crate::preemption::PreemptAttribution {
        session_id: &session.id,
        project_id: &project.id,
        agent_id: &spec.id,
        agent_name: &spec.name,
        agent_role: spec.role.as_str(),
        task_id: spec.task_id.as_deref(),
        department: spec.department.as_deref(),
    };
    emit(
        engine,
        &session.id,
        spec,
        "tool.preempted",
        serde_json::to_value(crate::preemption::record(
            &attribution,
            snapshot,
            false,
            None,
            None,
            None,
        ))?,
    )
    .await
}

/// Additive per-tool observability. The existing output/structured fields stay
/// unchanged; these fields describe the call without duplicating its content.
pub(super) fn tool_finished_payload(
    tool_call_id: &str,
    name: &str,
    output: &ToolOutput,
    duration_ms: u64,
    approval_risk: Option<RiskLevel>,
    approval_mode: ApprovalMode,
    approval_wait_ms: Option<u64>,
) -> serde_json::Value {
    let failure = output.failure.as_ref();
    json!({
        "tool_call_id": tool_call_id,
        "name": name,
        "output": crate::tools::truncate(output.content.clone()),
        "is_error": output.is_error,
        "structured": crate::tools::bounded_structured(output.structured.clone()),
        "output_ref": output.output_ref,
        "truncated": output.truncated,
        "total_bytes": output.total_bytes,
        "preview_bytes": output.preview_bytes,
        "lines": output.lines,
        "duration_ms": duration_ms.min(86_400_000),
        "output_bytes": output.total_bytes,
        "error_code": failure.map(|failure| failure.code.as_str()),
        "retryable": failure.is_some_and(|failure| failure.retryable),
        "error_hint": failure.map(|failure| {
            failure.hint.chars().take(crate::tooling::errors::MAX_FAILURE_HINT_CHARS).collect::<String>()
        }),
        "approval_risk": approval_risk.map(RiskLevel::as_str),
        "approval_mode": approval_mode.as_str(),
        "approval_wait_ms": approval_wait_ms.map(|wait| wait.min(86_400_000)),
    })
}

pub(super) struct ToolAuthorization {
    pub(super) risk: RiskLevel,
    pub(super) denied_reason: Option<String>,
    pub(super) skill_denied: Option<Skill>,
    pub(super) approval_wait_ms: Option<u64>,
    pub(super) cancelled: bool,
}

/// Non-mutating preview of the persisted approval broker. It runs the same
/// `approvals::evaluate_approval_rules` precedence matrix with a template hook
/// that never consumes a use or writes an audit row. The parallel read-only
/// fast path uses it only to decide whether it may take the fast path at all:
/// a non-`Allow` preview falls back to the sequential dispatcher, which then
/// performs the single real evaluation (template use consumption plus
/// `approval.template.matched` audit). This keeps `deny always wins` for a
/// batched call without consuming a use-capped deny/escalate template twice.
#[allow(clippy::too_many_arguments)]
pub(super) fn preview_tool_permission(
    engine: &Engine,
    session: &Session,
    assessment: &crate::permissions::PermissionAssessment,
    tool_name: &str,
    input: &serde_json::Value,
    mode: ApprovalMode,
    auto_approve: bool,
) -> Result<crate::approvals::ApprovalEvaluation> {
    let templates = engine
        .store()
        .list_approval_templates(&session.project_id)?;
    let grants = engine.store().list_approval_grants(&session.project_id)?;
    let now = time::OffsetDateTime::now_utc();
    let mut preview_consume = |template: &vp_protocol::types::ApprovalTemplate| {
        Some(
            template
                .max_uses
                .map(|max| max.saturating_sub(template.used_count).saturating_sub(1)),
        )
    };
    Ok(crate::approvals::evaluate_approval_rules(
        &templates,
        &grants,
        assessment,
        tool_name,
        input,
        mode,
        auto_approve,
        now,
        &mut preview_consume,
    ))
}

/// Run the pre-execution policy stack for one tool call: risk assessment,
/// optional script-binding fail-closed checks, task contract authorization,
/// the approval broker, mutation accounting, and active-skill denial.
///
/// The normal dispatcher and the Code Mode inner dispatcher both call this so
/// a contract, approval, or skill change cannot be applied to only one path.
#[allow(clippy::too_many_arguments)]
pub(super) async fn authorize_tool_call(
    engine: &Engine,
    config: &AgentConfig,
    session: &Session,
    spec: &AgentSpec,
    control: &RunControl,
    journal: &crate::run_journal::RunJournal,
    call_id: &str,
    tool_name: &str,
    input: &serde_json::Value,
    turn: u32,
    contract: Option<&crate::contract::TaskContract>,
    contract_policy: &vp_protocol::types::ContractPolicy,
    contract_usage: &mut crate::contract::MutationUsage,
    active_skill: Option<&Skill>,
    gui_policy: &vp_protocol::types::GuiPolicy,
    contract_tool: Option<&str>,
    dynamic_mcp: bool,
    mcp_trust: Option<crate::contract::McpPinTrust>,
    role_policy_unavailable: bool,
    scout_blocked: bool,
    read_only_binding: bool,
    write_binding_risk_floor: bool,
    approval_context: Option<serde_json::Value>,
    audit_extra: Option<(&str, serde_json::Value)>,
) -> Result<ToolAuthorization> {
    let mut assessment =
        crate::permissions::assess_tool_call_with_gui(tool_name, input, &spec.worktree, gui_policy);
    if dynamic_mcp {
        assessment = crate::permissions::PermissionAssessment::new(
            RiskLevel::HostState,
            "dynamic MCP tool execution can change host or remote state",
        );
    }
    if read_only_binding && assessment.risk >= RiskLevel::WorkspaceWrite {
        return Ok(ToolAuthorization {
            risk: assessment.risk,
            denied_reason: Some(format!(
                "read-only script binding '{tool_name}' assessed as {}; fail closed",
                assessment.risk.as_str()
            )),
            skill_denied: None,
            approval_wait_ms: None,
            cancelled: false,
        });
    }
    if write_binding_risk_floor && assessment.risk < RiskLevel::WorkspaceWrite {
        assessment = crate::permissions::PermissionAssessment::new(
            RiskLevel::HostState,
            "script write binding did not classify as a workspace write",
        );
    }
    // Argument-level intent authorization runs before the approval broker. A
    // contract can only narrow: Deny never prompts, Escalate raises the
    // assessment to at least HostState and then takes the normal approval path.
    let mut contract_pending_mutation: Option<crate::contract::MutationDelta> = None;
    let mut contract_denied: Option<(RiskLevel, String)> = None;
    if !role_policy_unavailable && let Some(contract) = contract {
        let contract_spec = crate::contract::ContractCallSpec {
            tool: tool_name,
            contract_tool,
            input,
            risk: assessment.risk,
            worktree: &spec.worktree,
            usage: *contract_usage,
            mcp_trust,
            patch_text: input
                .get("patch")
                .and_then(serde_json::Value::as_str)
                .or_else(|| input.get("diff").and_then(serde_json::Value::as_str)),
        };
        let contract_decision =
            crate::contract::authorize(&contract_spec, contract, contract_policy);
        engine
            .record_intent_decision(contract, &contract_decision, &contract_spec)
            .await?;
        match contract_decision.outcome {
            crate::contract::ContractOutcome::Deny => {
                contract_denied = Some((
                    contract_decision.effective_risk(),
                    format!(
                        "intent: {} ({} · {})",
                        contract_decision.reason, contract_decision.code, contract_decision.rule
                    ),
                ));
            }
            crate::contract::ContractOutcome::Escalate => {
                let blocked = assessment.blocked;
                assessment = crate::permissions::PermissionAssessment {
                    risk: assessment.risk.max(contract_decision.effective_risk()),
                    reason: format!(
                        "intent: {} ({} · {})",
                        contract_decision.reason, contract_decision.code, contract_decision.rule
                    ),
                    blocked,
                };
            }
            crate::contract::ContractOutcome::Allow => {
                let delta = crate::contract::mutation_delta(&contract_spec);
                if !delta.is_zero() {
                    contract_pending_mutation = Some(delta);
                }
            }
        }
    }
    let decision = if let Some((risk, reason)) = &contract_denied {
        crate::permissions::ApprovalDecision::Deny {
            risk: *risk,
            reason: reason.clone(),
        }
    } else if scout_blocked {
        crate::permissions::ApprovalDecision::Allow {
            risk: RiskLevel::ReadOnly,
            reason: "tool is outside the Scout read-only policy".to_string(),
        }
    } else {
        match engine.evaluate_tool_permission(
            &session.id,
            &session.project_id,
            &assessment,
            tool_name,
            input,
            config.approval_mode,
            spec.auto_approve,
        ) {
            Ok(evaluation) => evaluation.decision,
            Err(error) => crate::permissions::ApprovalDecision::Deny {
                risk: assessment.risk,
                reason: format!("approval evaluation failed: {error}"),
            },
        }
    };
    let mut approval_wait_ms: Option<u64> = None;
    let mut denied_reason: Option<String> =
        scout_blocked.then(|| format!("tool '{tool_name}' is not available to Scout"));
    match decision {
        crate::permissions::ApprovalDecision::Allow { risk, reason } => {
            if risk >= RiskLevel::External {
                emit(engine, &session.id, spec, "permission.allowed", {
                    let mut payload = serde_json::json!({
                        "tool": tool_name,
                        "tool_call_id": call_id,
                        "risk": risk.as_str(),
                        "reason": reason,
                    });
                    if let (Some((key, value)), Some(object)) =
                        (audit_extra.clone(), payload.as_object_mut())
                    {
                        object.insert(key.to_string(), value);
                    }
                    payload
                })
                .await?;
            }
        }
        crate::permissions::ApprovalDecision::Ask { risk, reason } => {
            control
                .preempt
                .set_phase(crate::preemption::Phase::Queue, None);
            let mut context = approval_context
                .clone()
                .unwrap_or_else(|| serde_json::json!({}));
            agent_fields(&mut context, spec);
            let (approval, receiver) = Box::pin(engine.request_approval_with_risk(
                &session.id,
                call_id,
                tool_name,
                input.clone(),
                Some(context),
                Some(risk.as_str()),
                Some(&reason),
            ))
            .await?;
            let approval_step = match journal
                .approval_intent(
                    engine,
                    turn,
                    &approval.id,
                    tool_name,
                    call_id,
                    risk.as_str(),
                    &reason,
                )
                .await
            {
                Ok(step) => step,
                Err(error) => {
                    let _ = engine.resolve_approval(&approval.id, false).await;
                    return Err(error);
                }
            };
            let wait_started = Instant::now();
            let outcome = match wait_for_approval(
                receiver,
                control,
                Duration::from_secs(config.approval_timeout_secs),
            )
            .await
            {
                ApprovalOutcome::Approved => "approved",
                ApprovalOutcome::Denied => {
                    denied_reason = Some("The user denied this tool call.".to_string());
                    "denied"
                }
                ApprovalOutcome::TimedOut => {
                    denied_reason = Some("The approval request timed out.".to_string());
                    let _ = engine.resolve_approval(&approval.id, false).await;
                    "timed_out"
                }
                ApprovalOutcome::Cancelled => {
                    let _ = engine.resolve_approval(&approval.id, false).await;
                    journal
                        .approval_completed(engine, &approval_step, "cancelled")
                        .await?;
                    return Ok(ToolAuthorization {
                        risk: assessment.risk,
                        denied_reason,
                        skill_denied: None,
                        approval_wait_ms,
                        cancelled: true,
                    });
                }
            };
            journal
                .approval_completed(engine, &approval_step, outcome)
                .await?;
            control
                .preempt
                .set_phase(crate::preemption::Phase::Tool, Some(tool_name));
            approval_wait_ms =
                Some(wait_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64);
        }
        crate::permissions::ApprovalDecision::Deny { risk, reason } => {
            emit(engine, &session.id, spec, "permission.denied", {
                let mut payload = serde_json::json!({
                    "tool": tool_name,
                    "tool_call_id": call_id,
                    "risk": risk.as_str(),
                    "reason": reason,
                });
                if let (Some((key, value)), Some(object)) =
                    (audit_extra.clone(), payload.as_object_mut())
                {
                    object.insert(key.to_string(), value);
                }
                payload
            })
            .await?;
            denied_reason = Some(format!("Blocked by the local permission policy: {reason}"));
        }
    }
    if denied_reason.is_none()
        && !scout_blocked
        && active_skill.is_none_or(|skill| skill_allows_tool(skill, tool_name))
        && let (Some(contract), Some(delta)) = (contract, contract_pending_mutation)
    {
        *contract_usage = engine.record_intent_mutation(contract, delta).await?;
    }
    let skill_denied = active_skill
        .filter(|skill| !skill_allows_tool(skill, tool_name))
        .cloned();
    Ok(ToolAuthorization {
        risk: assessment.risk,
        denied_reason,
        skill_denied,
        approval_wait_ms,
        cancelled: false,
    })
}

pub(super) struct ToolEffectGate {
    pub(super) planned_effect: Option<crate::effect::ToolEffectPlan>,
    pub(super) effect_override: Option<ToolOutput>,
    pub(super) effect_begin: Option<EffectReceipt>,
    pub(super) effect_duplicate: Option<EffectReceipt>,
    pub(super) journal_pair: Option<(vp_protocol::types::RunStep, vp_protocol::types::RunToolCall)>,
    pub(super) classification_failed: bool,
}

/// Plan, replay, or block one effect and write the journal intent. This is the
/// effect half of the pre-execution gate and is shared by both dispatchers.
#[allow(clippy::too_many_arguments)]
pub(super) async fn prepare_tool_effect_gate(
    engine: &Engine,
    session: &Session,
    spec: &AgentSpec,
    journal: &crate::run_journal::RunJournal,
    turn: u32,
    call_id: &str,
    tool_name: &str,
    input: &serde_json::Value,
    risk: RiskLevel,
    denied: bool,
    skill_denied: bool,
    dynamic_mcp: bool,
    effect_policy: &vp_protocol::types::EffectPolicy,
    tool_context: &ToolContext,
    duplicate: bool,
) -> Result<ToolEffectGate> {
    let mut classification_error: Option<ToolOutput> = None;
    let planned_effect = if !denied && !skill_denied {
        match crate::effect::classify_tool_effect_for_host(
            tool_name,
            input,
            risk,
            dynamic_mcp,
            effect_policy,
            tool_context
                .remote
                .as_ref()
                .map(|remote| remote.label())
                .as_deref(),
        ) {
            Ok(plan) => plan,
            Err(error) => {
                classification_error = Some(ToolOutput::error_coded(
                    tool_name,
                    crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
                    format!("cannot plan the effect for this call: {error}"),
                ));
                None
            }
        }
    } else {
        None
    };
    let effect_context = if planned_effect.is_some() {
        let worktree = spec.worktree.clone();
        tokio::task::spawn_blocking(move || crate::worktree::head_commit(&worktree))
            .await?
            .unwrap_or_default()
    } else {
        String::new()
    };
    let classification_failed = classification_error.is_some();
    let mut effect_override = classification_error;
    let mut effect_begin: Option<EffectReceipt> = None;
    let mut effect_duplicate: Option<EffectReceipt> = None;
    let mut journal_pair: Option<(vp_protocol::types::RunStep, vp_protocol::types::RunToolCall)> =
        None;
    if effect_override.is_some() {
        // The call never executes; the tool error is the result.
    } else if duplicate {
        // A journaled write replay gets its own intent in this attempt and is
        // completed with the replayed result. If this attempt fails after the
        // replay, the successor completed row (and the never-consumed source
        // row) lets a later resume replay again instead of re-executing.
        let (step, tool, _) = journal
            .tool_replay_intent(engine, turn, call_id, tool_name, input)
            .await?;
        journal_pair = Some((step, tool));
        if let Some(plan) = &planned_effect
            && let crate::effect::EffectPrepare::Replay(receipt)
            | crate::effect::EffectPrepare::Blocked(receipt)
            | crate::effect::EffectPrepare::Proceed(receipt) = engine
                .prepare_tool_effect(
                    &session.id,
                    spec.task_id.as_deref(),
                    &spec.id,
                    &effect_context,
                    plan,
                )
                .await?
        {
            effect_duplicate = Some(receipt);
        }
    } else if let Some(plan) = &planned_effect {
        match engine
            .prepare_tool_effect(
                &session.id,
                spec.task_id.as_deref(),
                &spec.id,
                &effect_context,
                plan,
            )
            .await?
        {
            crate::effect::EffectPrepare::Replay(receipt) => {
                effect_override = Some(effect_replay_output(tool_name, &receipt));
            }
            crate::effect::EffectPrepare::Blocked(receipt) => {
                effect_override = Some(effect_blocked_output(tool_name, &receipt));
            }
            crate::effect::EffectPrepare::Proceed(receipt) => {
                let (step, tool, begin) = journal
                    .tool_intent(engine, turn, call_id, tool_name, input, Some(&receipt))
                    .await?;
                match begin {
                    Some(begin) if begin.inserted => {
                        effect_begin = Some(begin.receipt);
                        journal_pair = Some((step, tool));
                    }
                    Some(begin) => {
                        effect_override = Some(if begin.receipt.state == EffectState::Done {
                            effect_replay_output(tool_name, &begin.receipt)
                        } else {
                            effect_blocked_output(tool_name, &begin.receipt)
                        });
                        journal_pair = Some((step, tool));
                    }
                    None => journal_pair = Some((step, tool)),
                }
            }
        }
    } else {
        let (step, tool, _) = journal
            .tool_intent(engine, turn, call_id, tool_name, input, None)
            .await?;
        journal_pair = Some((step, tool));
    }
    Ok(ToolEffectGate {
        planned_effect,
        effect_override,
        effect_begin,
        effect_duplicate,
        journal_pair,
        classification_failed,
    })
}

/// Finish one gated tool call: settle its effect receipt, attach the bounded
/// effect summary, build the `tool.finished` payload, and commit the journal
/// completion and event together. Both the normal dispatcher and the Code Mode
/// inner dispatcher call this so the two paths cannot drift in settlement or
/// event shape.
#[allow(clippy::too_many_arguments)]
pub(super) async fn finish_tool_call(
    engine: &Engine,
    session: &Session,
    spec: &AgentSpec,
    journal: &crate::run_journal::RunJournal,
    call_id: &str,
    tool_name: &str,
    effect_input: &serde_json::Value,
    output: &mut ToolOutput,
    duration_ms: u64,
    approval_risk: Option<RiskLevel>,
    approval_mode: ApprovalMode,
    approval_wait_ms: Option<u64>,
    planned_effect: Option<&crate::effect::ToolEffectPlan>,
    effect_begin: Option<EffectReceipt>,
    journal_pair: Option<&(vp_protocol::types::RunStep, vp_protocol::types::RunToolCall)>,
    extra: Option<(&str, serde_json::Value)>,
) -> Result<Option<EffectReceipt>> {
    let effect_settled = if let (Some(intent), Some(plan)) = (effect_begin, planned_effect) {
        let probe =
            crate::effect::probe_tool_effect(plan, effect_input, output, &spec.worktree).await;
        let settled = crate::effect::settled_receipt(intent, probe);
        annotate_effect_output(output, &settled);
        Some(settled)
    } else {
        None
    };
    let mut payload = tool_finished_payload(
        call_id,
        tool_name,
        output,
        duration_ms,
        approval_risk,
        approval_mode,
        approval_wait_ms,
    );
    agent_fields(&mut payload, spec);
    if let (Some((key, value)), Some(object)) = (extra, payload.as_object_mut()) {
        object.insert(key.to_string(), value);
    }
    if let Some((step, tool)) = journal_pair {
        journal
            .tool_completed_with_effect(
                engine,
                step,
                tool,
                output,
                effect_settled.as_ref(),
                Some(("tool.finished", payload)),
            )
            .await?;
    } else {
        engine
            .append_event(&session.id, "tool.finished", payload)
            .await?;
    }
    Ok(effect_settled)
}

/// Bounded, secret-free effect metadata attached to a tool result. This is
/// how a completed write returns its receipt even when the run-journal
/// fingerprint gate, not the dispatcher, produced the output.
pub(super) fn annotate_effect_output(output: &mut ToolOutput, receipt: &EffectReceipt) {
    let summary = json!({
        "id": receipt.id,
        "state": receipt.state.as_str(),
        "attempt": receipt.attempt,
        "target": receipt.target.canonical(),
        "probe": receipt.evidence.probe,
        "reason": receipt.evidence.reason,
        "sha256": receipt.evidence.sha256,
        "commit": receipt.evidence.commit,
        "external_id": receipt.external_id,
        "caveat": receipt.evidence.caveat,
        "replayed": receipt.evidence.replayed,
    });
    match output.structured.as_mut() {
        Some(Value::Object(object)) => {
            object.insert("effect".to_string(), summary);
        }
        Some(value) => {
            *value = json!({"value": value, "effect": summary});
        }
        None => {
            output.structured = Some(json!({"effect": summary}));
        }
    }
}

pub(super) fn gate_tool_error(tool: &str, error: &anyhow::Error) -> ToolOutput {
    use crate::tooling::errors::ToolErrorCode;
    let code = match crate::gate::gate_error_code(error) {
        Some(crate::gate::codes::GATE_NOT_FOUND) => ToolErrorCode::GATE_NOT_FOUND,
        Some(crate::gate::codes::GATE_TOKEN_INVALID) => ToolErrorCode::GATE_TOKEN_INVALID,
        Some(crate::gate::codes::GATE_EXPIRED) => ToolErrorCode::GATE_EXPIRED,
        Some(crate::gate::codes::GATE_CANCELLED) => ToolErrorCode::GATE_CANCELLED,
        Some(crate::gate::codes::GATE_CONFLICT) => ToolErrorCode::GATE_CONFLICT,
        Some(crate::gate::codes::GATE_ALREADY_WATCHED) => ToolErrorCode::GATE_ALREADY_WATCHED,
        Some(crate::gate::codes::GATE_ARCHIVED) => ToolErrorCode::GATE_ARCHIVED,
        Some(crate::gate::codes::GATE_LOOPBACK_DISABLED) => ToolErrorCode::GATE_LOOPBACK_DISABLED,
        _ => ToolErrorCode::GATE_INVALID,
    };
    ToolOutput::error_coded(tool, code, format!("{error:#}"))
}

pub(super) fn effect_replay_output(_tool: &str, receipt: &EffectReceipt) -> ToolOutput {
    let mut evidence = receipt.evidence.clone();
    evidence.replayed = true;
    let anchor = evidence
        .sha256
        .as_deref()
        .or(receipt.external_id.as_deref())
        .or(evidence.commit.as_deref())
        .unwrap_or("no additional evidence");
    let mut output = ToolOutput::ok_structured(
        format!(
            "effect already completed: {} {} (attempt {}); the original action was not executed again. evidence: {anchor}",
            receipt.target.canonical(),
            receipt.state.as_str(),
            receipt.attempt
        ),
        json!({
            "effect": {
                "id": receipt.id,
                "state": receipt.state.as_str(),
                "attempt": receipt.attempt,
                "target": receipt.target.canonical(),
                "probe": evidence.probe,
                "sha256": evidence.sha256,
                "commit": evidence.commit,
                "external_id": receipt.external_id,
                "caveat": evidence.caveat,
                "replayed": true,
            },
            "replayed": true,
        }),
    );
    output.failure = None;
    output
}

pub(super) fn effect_blocked_output(tool: &str, receipt: &EffectReceipt) -> ToolOutput {
    ToolOutput::error_coded(
        tool,
        crate::tooling::errors::ToolErrorCode::EFFECT_UNKNOWN,
        format!(
            "effect receipt {} for {} is {}; it must be reconciled with effect.reconcile before this action can run again",
            receipt.id,
            receipt.target.canonical(),
            receipt.state.as_str()
        ),
    )
    .with_failure_evidence(format!(
        "effect_id={}; state={}; target={}",
        receipt.id,
        receipt.state.as_str(),
        receipt.target.canonical()
    ))
}

pub(super) async fn collect_dynamic_mcp_tools(
    engine: &Engine,
    role: AgentRole,
    project_id: &str,
    session_id: Option<&str>,
) -> Vec<McpToolBinding> {
    if !matches!(
        role,
        AgentRole::Leader | AgentRole::Worker | AgentRole::Labour
    ) {
        return Vec::new();
    }
    let servers = match engine.list_mcp_servers().await {
        Ok(result) => result.servers,
        Err(error) => {
            tracing::warn!(%error, "MCP dynamic tool discovery could not list servers");
            return Vec::new();
        }
    };
    let mut bindings = Vec::new();
    let mut used = HashSet::new();
    for server in servers.into_iter().filter(|server| server.enabled) {
        let tools = match engine.mcp_tools(&server.id).await {
            Ok(result) => result.tools,
            Err(error) => {
                tracing::warn!(%error, server = %server.name, "MCP dynamic tool discovery failed");
                continue;
            }
        };
        let mut pin_sync_failed = false;
        let pins = match session_id {
            Some(session_id) => match engine
                .sync_mcp_pins(session_id, project_id, &server, &tools)
                .await
            {
                Ok(pins) => pins,
                Err(error) => {
                    tracing::warn!(%error, server = %server.name, "MCP pin sync failed");
                    pin_sync_failed = true;
                    // Use the last approved pins rather than an empty list.
                    // An empty list would make every discovered tool look
                    // like first-use TOFU and re-trust drifted tools.
                    engine
                        .project_mcp_pins(project_id, Some(&server.id))
                        .await
                        .map(|result| result.pins)
                        .unwrap_or_default()
                }
            },
            None => engine
                .project_mcp_pins(project_id, Some(&server.id))
                .await
                .map(|result| result.pins)
                .unwrap_or_default(),
        };
        let pin_by_tool = pins
            .into_iter()
            .map(|pin| (pin.tool_name.clone(), pin))
            .collect::<HashMap<_, _>>();
        // TOFU is only valid for a session-less preview. A real run whose pin
        // sync failed must not silently trust every discovered tool.
        let tofu_pending = pin_by_tool.is_empty() && session_id.is_none() && !pin_sync_failed;
        for tool in tools {
            // A project with no pins for this server is a TOFU-preview path.
            // The real run syncs pins before advertising, so this fallback is
            // deterministic with the first sync result.
            let pin = pin_by_tool.get(&tool.name);
            let trust = match pin {
                Some(pin) if crate::mcp_pins::pin_is_callable(pin) => {
                    crate::contract::McpPinTrust::Trusted
                }
                Some(pin) => match pin.status.as_str() {
                    "drift" => crate::contract::McpPinTrust::Drift,
                    "removed" => crate::contract::McpPinTrust::Removed,
                    _ => crate::contract::McpPinTrust::Unverified,
                },
                None if tofu_pending => crate::contract::McpPinTrust::Trusted,
                None => crate::contract::McpPinTrust::Unverified,
            };
            // Unapproved tools are intentionally not advertised or executable.
            if trust != crate::contract::McpPinTrust::Trusted {
                continue;
            }
            let exposed_name = dynamic_mcp_tool_name(&server.name, &tool.name);
            if !used.insert(exposed_name.clone()) {
                continue;
            }
            let mut parameters = if tool.input_schema.is_object() {
                tool.input_schema.clone()
            } else {
                serde_json::json!({})
            };
            if parameters.get("type").is_none()
                && let Some(object) = parameters.as_object_mut()
            {
                object.insert("type".to_string(), serde_json::json!("object"));
            }
            bindings.push(McpToolBinding {
                exposed_name: exposed_name.clone(),
                server_id: server.id.clone(),
                tool_name: tool.name.clone(),
                schema: ToolSchema {
                    name: exposed_name,
                    description: truncate_to(
                        &format!("[MCP {}] {}", server.name, tool.description),
                        400,
                    ),
                    parameters,
                },
                pin_trust: trust,
            });
        }
    }
    // Server and tool order is not stable across MCP implementations. Sort by
    // exposed name before applying the cap so the advertised prefix is
    // deterministic for the run and across sessions.
    sort_and_cap_dynamic_mcp_tools(bindings)
}

pub(super) fn sort_and_cap_dynamic_mcp_tools(
    mut bindings: Vec<McpToolBinding>,
) -> Vec<McpToolBinding> {
    bindings.sort_by(|left, right| {
        left.exposed_name
            .cmp(&right.exposed_name)
            .then_with(|| left.server_id.cmp(&right.server_id))
            .then_with(|| left.tool_name.cmp(&right.tool_name))
    });
    bindings.truncate(MAX_DYNAMIC_MCP_TOOLS);
    bindings
}

pub(super) fn dynamic_mcp_tool_name(server: &str, tool: &str) -> String {
    fn sanitize(value: &str) -> String {
        let sanitized = value
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() {
                    character.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect::<String>();
        sanitized.trim_matches('_').to_string()
    }
    let mut name = format!("mcp_{}_{}", sanitize(server), sanitize(tool));
    if name.len() > 64 {
        name.truncate(64);
    }
    if name.is_empty() || name == "mcp__" {
        "mcp_tool".to_string()
    } else {
        name
    }
}

pub(super) fn format_mcp_tool_result(result: &serde_json::Value) -> String {
    let mut text = String::new();
    if let Some(content) = result.get("content").and_then(serde_json::Value::as_array) {
        for item in content {
            if let Some(value) = item.get("text").and_then(serde_json::Value::as_str) {
                text.push_str(value);
                text.push('\n');
            } else {
                text.push_str(&serde_json::to_string(item).unwrap_or_else(|_| item.to_string()));
                text.push('\n');
            }
        }
    }
    if text.trim().is_empty() {
        text = serde_json::to_string_pretty(result).unwrap_or_else(|_| result.to_string());
    }
    truncate_to(&text, 16_000)
}

/// Shape one assistant turn's tool results before journal completion, event
/// persistence, or conversation insertion. The full body is spilled first;
/// only the bounded preview enters the transcript and event stream.
#[allow(clippy::too_many_arguments)]
pub(super) async fn shape_tool_output_batch(
    engine: &Engine,
    session: &Session,
    spec: &AgentSpec,
    project: &Project,
    prior_turn_bytes: u64,
    mut pending: Vec<crate::tool_output::PendingToolOutput>,
) -> Result<(Vec<crate::tool_output::PendingToolOutput>, u64)> {
    if pending.is_empty() {
        return Ok((pending, prior_turn_bytes));
    }
    let policy = engine
        .get_tool_output_policy(&project.id)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%error, project_id = %project.id, "failed to load tool-output policy; using defaults");
            vp_protocol::types::ToolOutputPolicy::default()
        });
    let max_total_bytes_per_turn = policy.max_total_bytes_per_turn;
    let store = crate::tool_output::ToolOutputStore::new(engine.store().clone());
    let project_id = project.id.clone();
    let session_id = session.id.clone();
    let (pending, report) = tokio::task::spawn_blocking(move || {
        let report = crate::tool_output::shape_tool_outputs(
            Some(&store),
            &policy,
            &session_id,
            &project_id,
            prior_turn_bytes,
            &mut pending,
        )?;
        Ok::<_, anyhow::Error>((pending, report))
    })
    .await
    .context("tool-output shaping task failed")??;
    for notice in report.notices {
        let kind = match notice.kind {
            crate::tool_output::ToolOutputNoticeKind::Spilled => "tool.output_spilled",
            crate::tool_output::ToolOutputNoticeKind::Truncated => "tool.output_truncated",
        };
        emit(
            engine,
            &session.id,
            spec,
            kind,
            serde_json::json!({
                "tool_call_id": notice.tool_call_id,
                "name": notice.tool_name,
                "handle": notice.handle,
                "sha256": notice.sha256,
                "bytes": notice.total_bytes,
                "returned_bytes": notice.returned_bytes,
                "lines": notice.total_lines,
                "returned_lines": notice.returned_lines,
                "preview_bytes": notice.preview_bytes,
                "reason": notice.reason,
                "error": notice.error,
            }),
        )
        .await?;
    }
    let next_turn_bytes = prior_turn_bytes
        .saturating_add(report.provider_bytes)
        .min(max_total_bytes_per_turn);
    Ok((pending, next_turn_bytes))
}

/// Registry tools that only read repository state and can safely run in a
/// Execute one tool inside a read-only parallel batch with its own deadline
/// token. The token observes both the batch/run parent and the per-tool
/// deadline; only this future is dropped when the deadline fires. Sibling
/// tools keep running, and the run token still aborts the whole batch.
pub(super) async fn execute_parallel_tool(
    tool: Option<Arc<dyn crate::tools::AgentTool>>,
    engine: Engine,
    session: Session,
    context: ToolContext,
    tool_token: crate::preemption::Token,
    input: Value,
    name: String,
) -> (ToolOutput, Option<crate::preemption::Snapshot>) {
    let scoped_token = tool_token.clone();
    let work = async move {
        match tool {
            Some(tool) => {
                scoped_token
                    .scoped(Box::pin(async move { tool.execute(&context, input).await }))
                    .await
            }
            None => Ok(run_read_tool_output(&engine, &session, input).await),
        }
    };
    run_tool_with_token_deadline(&tool_token, &name, work).await
}

/// Race one tool future against its own deadline token. This is the unit under
/// test for parallel-batch deadline isolation: the future is dropped when the
/// token fires, so a read-only tool cannot leave work running after its result
/// has been reported.
pub(super) async fn run_tool_with_token_deadline<F>(
    tool_token: &crate::preemption::Token,
    name: &str,
    work: F,
) -> (ToolOutput, Option<crate::preemption::Snapshot>)
where
    F: std::future::Future<Output = anyhow::Result<ToolOutput>>,
{
    tokio::pin!(work);
    tokio::select! {
        result = &mut work => match result {
            Ok(output) => (output, None),
            Err(error) => (ToolOutput::from_tool_error(name, &error), None),
        },
        _ = tool_token.cancelled() => {
            let snapshot = tool_token.snapshot().unwrap_or_else(|| {
                crate::preemption::fallback_snapshot(
                    crate::preemption::Phase::Tool,
                    Some(name),
                )
            });
            let output = crate::preemption::tool_output(name, &snapshot);
            (output, Some(snapshot))
        }
    }
}

/// bounded parallel batch. `shell` and `fetch_url` are deliberately excluded:
/// shell commands can mutate state and network calls should not fan out.
pub(super) fn is_parallel_safe_tool(name: &str) -> bool {
    matches!(
        name,
        "read_file"
            | "list_dir"
            | "grep"
            | "find_files"
            | "file_info"
            | "read_files"
            | "project_profile"
            | "git"
            | "repo_map"
            | "find_symbol"
            | "find_references"
            | "dependency_graph"
            | "build_context"
            | "affected_tests"
            | "read_tool_output"
            | "remote_read"
            | "remote_list"
            | "remote_git"
    )
}

pub(super) enum ApprovalOutcome {
    Approved,
    Denied,
    TimedOut,
    Cancelled,
}

pub(super) enum QuestionOutcome {
    Answered(String),
    Cancelled,
    TimedOut,
}

pub(super) async fn wait_for_question(
    receiver: oneshot::Receiver<String>,
    control: &RunControl,
    timeout_secs: u64,
) -> QuestionOutcome {
    if control.is_cancelled() || control.is_preempted() {
        return QuestionOutcome::Cancelled;
    }
    let mut cancel_rx = control.subscribe();
    tokio::select! {
        _ = cancel_rx.changed() => QuestionOutcome::Cancelled,
        _ = control.preempt.cancelled() => QuestionOutcome::Cancelled,
        result = timeout(Duration::from_secs(timeout_secs), receiver) => match result {
            Ok(Ok(answer)) => QuestionOutcome::Answered(answer),
            Ok(Err(_)) => QuestionOutcome::TimedOut,
            Err(_) => QuestionOutcome::TimedOut,
        },
    }
}

pub(super) async fn wait_for_approval(
    receiver: oneshot::Receiver<bool>,
    control: &RunControl,
    timeout_duration: Duration,
) -> ApprovalOutcome {
    wait_for_approval_with(
        control.subscribe(),
        Some(control.preempt.clone()),
        receiver,
        timeout_duration,
    )
    .await
}

pub(super) async fn wait_for_approval_with(
    mut cancel_rx: watch::Receiver<bool>,
    preempt: Option<crate::preemption::Token>,
    receiver: oneshot::Receiver<bool>,
    timeout_duration: Duration,
) -> ApprovalOutcome {
    if *cancel_rx.borrow() {
        return ApprovalOutcome::Cancelled;
    }
    let preempted = async {
        match preempt {
            Some(token) => token.cancelled().await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(preempted);
    tokio::select! {
        _ = cancel_rx.changed() => ApprovalOutcome::Cancelled,
        _ = &mut preempted => ApprovalOutcome::Cancelled,
        result = timeout(timeout_duration, receiver) => match result {
            Ok(Ok(true)) => ApprovalOutcome::Approved,
            Ok(Ok(false)) | Ok(Err(_)) => ApprovalOutcome::Denied,
            Err(_) => ApprovalOutcome::TimedOut,
        },
    }
}

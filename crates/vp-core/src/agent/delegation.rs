use super::*;

pub(super) enum AcpDelegation {
    Output(ToolOutput),
    Cancelled,
    BudgetExceeded(BudgetStop),
}

pub(super) struct AcpWorkerHost {
    pub(super) engine: Engine,
    pub(super) session: Session,
    pub(super) spec: AgentSpec,
    pub(super) acp_name: String,
    pub(super) auto_approve: bool,
    pub(super) approval_mode: ApprovalMode,
    pub(super) approval_timeout_secs: u64,
    pub(super) cancel: watch::Receiver<bool>,
    pub(super) text: Mutex<String>,
    pub(super) started_tools: Mutex<HashSet<String>>,
    pub(super) finished_tools: Mutex<HashSet<String>>,
}

impl AcpWorkerHost {
    pub(super) fn final_summary(&self) -> String {
        let text = self
            .text
            .lock()
            .map(|text| text.trim().to_string())
            .unwrap_or_default();
        if text.is_empty() {
            "external agent completed without a summary".to_string()
        } else {
            truncate(&text)
        }
    }

    pub(super) async fn finish_tool(
        &self,
        id: &str,
        status: &str,
        content: Option<serde_json::Value>,
        raw_output: Option<serde_json::Value>,
        title: Option<&str>,
    ) {
        let first = self
            .started_tools
            .lock()
            .map(|mut started| started.insert(id.to_string()))
            .unwrap_or(false);
        let tool_call_id = format!("acp:{}:{id}", self.spec.id);
        if first {
            let _ = emit(
                &self.engine,
                &self.session.id,
                &self.spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": tool_call_id,
                    "name": "acp.tool",
                    "input": {
                        "title": title.unwrap_or("External agent tool"),
                    },
                }),
            )
            .await;
        }
        if status != "completed" && status != "failed" {
            return;
        }
        let first_finish = self
            .finished_tools
            .lock()
            .map(|mut finished| finished.insert(id.to_string()))
            .unwrap_or(false);
        if !first_finish {
            return;
        }
        let output = raw_output
            .or(content)
            .map(|value| value.to_string())
            .unwrap_or_default();
        let _ = emit(
            &self.engine,
            &self.session.id,
            &self.spec,
            "tool.finished",
            serde_json::json!({
                "tool_call_id": tool_call_id,
                "name": "acp.tool",
                "output": truncate(&output),
                "is_error": status == "failed",
                "duration_ms": 0,
                "output_bytes": output.len() as u64,
                "truncated": output.len() > crate::tools::MAX_TOOL_OUTPUT,
                "error_code": (status == "failed").then_some("ACP_TOOL_FAILED"),
                "retryable": false,
                "approval_risk": null,
                "approval_mode": self.approval_mode.as_str(),
                "approval_wait_ms": null,
            }),
        )
        .await;
    }
}

#[async_trait]
impl acp::AcpHost for AcpWorkerHost {
    async fn update(&self, update: acp::AcpUpdate) {
        match update {
            acp::AcpUpdate::Message(text) => {
                if let Ok(mut full) = self.text.lock()
                    && full.len() < 64 * 1024
                {
                    full.push_str(&text);
                }
                let _ = emit(
                    &self.engine,
                    &self.session.id,
                    &self.spec,
                    "agent.message_delta",
                    serde_json::json!({"text": text, "acp_agent": self.acp_name}),
                )
                .await;
            }
            acp::AcpUpdate::Thought(text) => {
                let _ = emit(
                    &self.engine,
                    &self.session.id,
                    &self.spec,
                    "agent.thought_delta",
                    serde_json::json!({"text": text, "acp_agent": self.acp_name}),
                )
                .await;
            }
            acp::AcpUpdate::ToolCall {
                id,
                title,
                kind,
                status,
                raw_input,
                content,
                raw_output,
            } => {
                let first = self
                    .started_tools
                    .lock()
                    .map(|mut started| started.insert(id.clone()))
                    .unwrap_or(false);
                if first {
                    let _ = emit(
                        &self.engine,
                        &self.session.id,
                        &self.spec,
                        "tool.started",
                        serde_json::json!({
                            "tool_call_id": format!("acp:{}:{id}", self.spec.id),
                            "name": format!("acp.{kind}"),
                            "input": {
                                "title": title,
                                "input": raw_input,
                            },
                        }),
                    )
                    .await;
                }
                if status == "completed" || status == "failed" {
                    self.finish_tool(&id, &status, content, raw_output, Some(&title))
                        .await;
                }
            }
            acp::AcpUpdate::ToolCallUpdate {
                id,
                title,
                status,
                content,
                raw_output,
            } => {
                let status = status.unwrap_or_else(|| "in_progress".to_string());
                self.finish_tool(&id, &status, content, raw_output, title.as_deref())
                    .await;
            }
        }
    }

    async fn permission(&self, request: acp::AcpPermissionRequest) -> acp::AcpPermissionDecision {
        let assessment = assess_acp_permission(&request, &self.spec.worktree);
        let tool = format!("acp.{}", self.acp_name);
        let raw_input = request
            .raw_input
            .as_ref()
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let decision = match self.engine.evaluate_tool_permission(
            &self.session.id,
            &self.session.project_id,
            &assessment,
            &tool,
            &raw_input,
            self.approval_mode,
            self.auto_approve,
        ) {
            Ok(evaluation) => evaluation.decision,
            Err(error) => {
                let _ = emit(
                    &self.engine,
                    &self.session.id,
                    &self.spec,
                    "permission.denied",
                    serde_json::json!({
                        "tool": tool,
                        "risk": assessment.risk.as_str(),
                        "reason": format!("approval evaluation failed: {error}"),
                    }),
                )
                .await;
                return acp::AcpPermissionDecision::Deny;
            }
        };
        let tool_call_id = format!("acp:{}:{}", self.spec.id, request.tool_call_id);
        let (risk, reason) = match decision {
            ApprovalDecision::Allow { risk, reason } => {
                if risk >= RiskLevel::External {
                    let _ = emit(
                        &self.engine,
                        &self.session.id,
                        &self.spec,
                        "permission.allowed",
                        serde_json::json!({
                            "tool": tool,
                            "tool_call_id": tool_call_id,
                            "risk": risk.as_str(),
                            "reason": reason,
                        }),
                    )
                    .await;
                }
                return acp::AcpPermissionDecision::Approve;
            }
            ApprovalDecision::Deny { risk, reason } => {
                let _ = emit(
                    &self.engine,
                    &self.session.id,
                    &self.spec,
                    "permission.denied",
                    serde_json::json!({
                        "tool": tool,
                        "tool_call_id": tool_call_id,
                        "risk": risk.as_str(),
                        "reason": reason,
                    }),
                )
                .await;
                return acp::AcpPermissionDecision::Deny;
            }
            ApprovalDecision::Ask { risk, reason } => (risk, reason),
        };
        let options = request
            .options
            .iter()
            .map(|option| {
                serde_json::json!({
                    "id": option.id,
                    "name": option.name,
                    "kind": option.kind,
                })
            })
            .collect::<Vec<_>>();
        let input = serde_json::json!({
            "title": request.title,
            "kind": request.kind,
            "input": request.raw_input,
            "options": options,
            "agent": self.acp_name,
        });
        let mut context = serde_json::json!({});
        agent_fields(&mut context, &self.spec);
        context["acp_agent"] = serde_json::json!(self.acp_name);
        let approval = Box::pin(self.engine.request_approval_with_risk(
            &self.session.id,
            &tool_call_id,
            &format!("acp.{}", self.acp_name),
            input,
            Some(context),
            Some(risk.as_str()),
            Some(&reason),
        ))
        .await;
        let Ok((approval, receiver)) = approval else {
            return acp::AcpPermissionDecision::Deny;
        };
        match wait_for_approval_with(
            self.cancel.clone(),
            None,
            receiver,
            Duration::from_secs(self.approval_timeout_secs),
        )
        .await
        {
            ApprovalOutcome::Approved => acp::AcpPermissionDecision::Approve,
            ApprovalOutcome::Denied => acp::AcpPermissionDecision::Deny,
            ApprovalOutcome::TimedOut => {
                let _ = self.engine.resolve_approval(&approval.id, false).await;
                acp::AcpPermissionDecision::Deny
            }
            ApprovalOutcome::Cancelled => {
                let _ = self.engine.resolve_approval(&approval.id, false).await;
                acp::AcpPermissionDecision::Cancelled
            }
        }
    }
}

pub(super) struct AcpDelegateRequest {
    pub(super) agent: String,
    pub(super) name: Option<String>,
    pub(super) objective: String,
    pub(super) instructions: String,
    pub(super) timeout: Option<u64>,
    pub(super) resume_session_id: Option<String>,
}

pub(super) fn parse_acp_delegate(input: &serde_json::Value) -> Result<AcpDelegateRequest> {
    let agent = input
        .get("agent")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("agent is required"))?
        .to_string();
    let objective = input
        .get("objective")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("objective is required"))?
        .to_string();
    let name = input
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let instructions = input
        .get("instructions")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let timeout = input
        .get("timeout_secs")
        .and_then(serde_json::Value::as_u64)
        .filter(|seconds| *seconds >= 30);
    let resume_session_id = input
        .get("session_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    if resume_session_id
        .as_deref()
        .is_some_and(|value| value.chars().count() > 256 || value.contains(['\r', '\n', '\0']))
    {
        return Err(anyhow!("session_id must be at most 256 characters"));
    }
    Ok(AcpDelegateRequest {
        agent,
        name,
        objective,
        instructions,
        timeout,
        resume_session_id,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn delegate_acp(
    engine: &Engine,
    config: &AgentConfig,
    session: &Session,
    project: &Project,
    control: &RunControl,
    budget: &SessionBudget,
    parent_spec: &AgentSpec,
    input: &serde_json::Value,
) -> Result<AcpDelegation> {
    let request = match parse_acp_delegate(input) {
        Ok(parsed) => parsed,
        Err(error) => return Ok(AcpDelegation::Output(ToolOutput::error(error.to_string()))),
    };
    let AcpDelegateRequest {
        agent: agent_ref,
        name: requested_name,
        objective,
        instructions,
        timeout,
        resume_session_id,
    } = request;
    let Some(acp_agent) = engine.resolve_acp_agent(&agent_ref).await? else {
        return Ok(AcpDelegation::Output(ToolOutput::error(format!(
            "ACP agent not found: {agent_ref}"
        ))));
    };
    if !acp_agent.enabled {
        return Ok(AcpDelegation::Output(ToolOutput::error(format!(
            "ACP agent {} is disabled",
            acp_agent.name
        ))));
    }
    if acp_agent.transport.is_remote() && !acp_agent.allow_remote {
        return Ok(AcpDelegation::Output(ToolOutput::error(format!(
            "ACP agent {} is a remote agent but allow_remote is disabled",
            acp_agent.name
        ))));
    }
    if acp_agent.transport.is_remote() {
        let assessment =
            assess_remote_acp_delegation(&acp_agent.name, acp_agent.transport.as_str());
        let risk = assessment.risk;
        let reason = assessment.reason.clone();
        let decision = decide(config.approval_mode, &assessment, false);
        let denied = match decision {
            ApprovalDecision::Allow { .. } => None,
            ApprovalDecision::Deny { reason, .. } => Some(reason),
            ApprovalDecision::Ask { .. } => {
                let tool_call_id = format!("delegate-acp:{}", acp_agent.id);
                let approval = engine
                    .request_approval_with_risk(
                        &session.id,
                        &tool_call_id,
                        "delegate_acp",
                        serde_json::json!({
                            "agent": acp_agent.name,
                            "transport": acp_agent.transport.as_str(),
                            "endpoint": acp_agent.endpoint.clone(),
                            "objective": truncate(&objective),
                        }),
                        Some(serde_json::json!({"agent_id": parent_spec.id, "agent": parent_spec.name})),
                        Some(risk.as_str()),
                        Some(&reason),
                    )
                    .await;
                let Ok((approval, receiver)) = approval else {
                    return Ok(AcpDelegation::Output(ToolOutput::error(
                        "could not request approval for the remote ACP delegation",
                    )));
                };
                match wait_for_approval_with(
                    control.subscribe(),
                    Some(control.preempt.clone()),
                    receiver,
                    Duration::from_secs(config.approval_timeout_secs),
                )
                .await
                {
                    ApprovalOutcome::Approved => None,
                    ApprovalOutcome::Denied => {
                        Some("operator denied the remote delegation".to_string())
                    }
                    ApprovalOutcome::TimedOut => {
                        let _ = engine.resolve_approval(&approval.id, false).await;
                        Some("remote delegation approval timed out".to_string())
                    }
                    ApprovalOutcome::Cancelled => {
                        let _ = engine.resolve_approval(&approval.id, false).await;
                        return Ok(AcpDelegation::Cancelled);
                    }
                }
            }
        };
        if let Some(reason) = denied {
            emit(
                engine,
                &session.id,
                parent_spec,
                "permission.denied",
                serde_json::json!({
                    "tool": "delegate_acp",
                    "agent": acp_agent.name,
                    "risk": risk.as_str(),
                    "reason": truncate(&reason),
                }),
            )
            .await?;
            return Ok(AcpDelegation::Output(ToolOutput::error(format!(
                "remote ACP delegation was denied: {reason}"
            ))));
        }
    }
    let Some((ordinal, _slot)) = budget.reserve_slot(AgentRole::Worker) else {
        return Ok(AcpDelegation::Output(ToolOutput::error(
            "the Worker cap for this session has been reached",
        )));
    };
    let write_scope = match parse_write_scope(input) {
        Ok(scope) => scope,
        Err(error) => return Ok(AcpDelegation::Output(ToolOutput::error(error.to_string()))),
    };
    let child_name = requested_name.unwrap_or_else(|| format!("{} {}", acp_agent.name, ordinal));
    let child = engine
        .create_agent(
            &session.id,
            "worker",
            &child_name,
            Some(format!("ACP · {}", acp_agent.name)),
            Some(parent_spec.id.clone()),
            &format!("acp:{}", acp_agent.name),
        )
        .await?;
    engine
        .set_agent_profile(&child.id, AgentProfile::Coding.as_str())
        .await?;
    let info = engine
        .create_task_worktree(
            &project.root,
            &project.id,
            &session.id,
            &child.id,
            &parent_spec.session_branch,
        )
        .await?;
    engine
        .set_agent_worktree(&child.id, &info.path.to_string_lossy(), &info.branch)
        .await?;
    let child_spec = AgentSpec {
        id: child.id.clone(),
        role: AgentRole::Worker,
        name: child_name.clone(),
        depth: parent_spec.depth.saturating_add(1),
        parent_name: Some(parent_spec.name.clone()),
        title: Some(format!("ACP · {}", acp_agent.name)),
        task_id: None,
        department: parent_spec.department.clone(),
        identity_id: None,
        identity_generation: None,
        profile: AgentProfile::Coding,
        allow_delegation: false,
        task_kind: None,
        auto_approve: acp_agent.auto_approve,
        worktree: info.path.clone(),
        branch: info.branch.clone(),
        session_worktree: parent_spec.session_worktree.clone(),
        session_branch: parent_spec.session_branch.clone(),
        write_scope: write_scope.clone(),
        base_commit: info.base_commit.clone(),

        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
    };
    emit(
        engine,
        &session.id,
        &child_spec,
        "agent.spawned",
        serde_json::json!({
            "parent_id": parent_spec.id,
            "parent_name": parent_spec.name,
            "objective": truncate(&objective),
            "acp_agent": acp_agent.name,
            "acp_transport": acp_agent.transport.as_str(),
            "acp_endpoint": acp_agent.endpoint,
            "acp_sandboxed": !acp_agent.transport.is_remote(),
            "acp_command": acp_agent.command,
            "backend": "acp",
        }),
    )
    .await?;

    let capacity_priority = task_capacity_priority(session.priority, 0);
    let _capacity_permit = match config.capacity.try_acquire(AgentRole::Worker, &session.id) {
        Some(permit) => Some(permit),
        None => {
            let snapshot = config.capacity.snapshot();
            emit(
                engine,
                &session.id,
                &child_spec,
                "agent.capacity_wait",
                serde_json::json!({
                    "role": "worker",
                    "limit": snapshot.worker_limit,
                    "available": snapshot.worker_available,
                    "backend": "acp",
                }),
            )
            .await?;
            engine.update_agent_status(&child.id, "queued").await?;
            let permit = match config
                .capacity
                .acquire(AgentRole::Worker, &session.id, capacity_priority, control)
                .await
            {
                Some(permit) => permit,
                None => {
                    engine.update_agent_status(&child.id, "cancelled").await?;
                    return Ok(AcpDelegation::Cancelled);
                }
            };
            emit(
                engine,
                &session.id,
                &child_spec,
                "agent.capacity_ready",
                serde_json::json!({"role": "worker", "waited": true, "backend": "acp"}),
            )
            .await?;
            Some(permit)
        }
    };
    let _scope_guard = match config
        .scope_locks
        .acquire(&project.id, &session.id, &child.id, &write_scope, control)
        .await?
    {
        Some(guard) => guard,
        None => {
            engine.update_agent_status(&child.id, "cancelled").await?;
            emit(
                engine,
                &session.id,
                &child_spec,
                "agent.finished",
                serde_json::json!({"status": "cancelled", "backend": "acp"}),
            )
            .await?;
            return Ok(AcpDelegation::Cancelled);
        }
    };
    engine.update_agent_status(&child.id, "running").await?;

    let shell_policy = engine.get_shell_policy(&project.id).await?;
    let mut secret_roots = shell_secret_roots(engine.store().data_dir());
    secret_roots.extend(shell_policy.read_deny.iter().map(PathBuf::from));
    let mcp_servers = engine
        .list_mcp_servers()
        .await?
        .servers
        .into_iter()
        .filter(|server| server.enabled)
        .map(|server| acp::mcp_server(&server))
        .collect::<Result<Vec<_>>>()?;
    let scope_note = if write_scope.is_empty() {
        "No explicit write scope was declared.".to_string()
    } else {
        format!("Write scope: {}.", write_scope.join(", "))
    };
    let instructions = if instructions.is_empty() {
        String::new()
    } else {
        format!(
            "
Additional instructions:
{instructions}
"
        )
    };
    let prompt = if acp_agent.transport.is_remote() {
        format!(
            "You are a remote external ACP agent delegated by {} inside VerlinPark.
             Objective: {objective}
{instructions}         This delegation uses the {} transport. You are not running on the
         VerlinPark host and you do not have access to the local worktree at {}.
         Do not claim to have edited that path. Complete the objective in your
         own environment, run relevant checks there, and finish with a concise
         summary of what you did and which checks ran.",
            parent_spec.name,
            acp_agent.transport.as_str(),
            child_spec.worktree.to_string_lossy(),
        )
    } else {
        format!(
            "You are an external ACP agent delegated by {} inside VerlinPark.
             Objective: {objective}
{instructions}         Work only in the current Git worktree at {} on branch {} (base commit {}).
         {scope_note}
         Make the requested changes directly, run relevant checks, and do not ask the user questions.          Finish with a concise summary of what changed and which checks ran.",
            parent_spec.name,
            child_spec.worktree.to_string_lossy(),
            child_spec.branch,
            child_spec.base_commit,
        )
    };
    let launch = acp::AcpLaunch {
        name: acp_agent.name.clone(),
        command: acp_agent.command.clone(),
        args: acp_agent.args.clone(),
        env: acp_agent.env.clone(),
        cwd: child_spec.worktree.clone(),
        project_root: PathBuf::from(&project.root),
        sandbox_root: engine
            .store()
            .data_dir()
            .join("sandbox")
            .join(&session.id)
            .join(&child.id),
        secret_roots,
        network_allowed: shell_policy.network != "deny",
        home: std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default(),
        restrict_home: shell_policy.restrict_home,
        read_jail: shell_policy.read_jail,
        git_branch: Some(child_spec.branch.clone()),
        mcp_servers,
        timeout: Duration::from_secs(
            timeout
                .unwrap_or(acp_agent.timeout_secs)
                .min(acp_agent.timeout_secs),
        ),
        transport: acp_agent.transport,
        endpoint: acp_agent.endpoint.clone(),
        auth_header_env: acp_agent.auth_header_env.clone(),
        headers: acp_agent.headers.clone(),
        connect_timeout: Duration::from_secs(acp_agent.connect_timeout_secs),
        allow_remote: acp_agent.allow_remote,
        resume_session_id,
        diagnostics: None,
    };
    let host = Arc::new(AcpWorkerHost {
        engine: engine.clone(),
        session: session.clone(),
        spec: child_spec.clone(),
        acp_name: acp_agent.name.clone(),
        auto_approve: acp_agent.auto_approve,
        approval_mode: config.approval_mode,
        approval_timeout_secs: config.approval_timeout_secs,
        cancel: control.subscribe(),
        text: Mutex::new(String::new()),
        started_tools: Mutex::new(HashSet::new()),
        finished_tools: Mutex::new(HashSet::new()),
    });
    let result = acp::run_prompt(launch, prompt, host.clone(), control.subscribe()).await;
    if control.is_cancelled() {
        engine.update_agent_status(&child.id, "cancelled").await?;
        emit(
            engine,
            &session.id,
            &child_spec,
            "agent.finished",
            serde_json::json!({
                "status": "cancelled",
                "backend": "acp",
                "acp_agent": acp_agent.name,
            }),
        )
        .await?;
        return Ok(AcpDelegation::Cancelled);
    }
    let outcome = match result {
        Ok(outcome) => outcome,
        Err(error) => {
            let summary = format!("external ACP agent failed: {error:#}");
            engine.update_agent_status(&child.id, "failed").await?;
            emit(
                engine,
                &session.id,
                &child_spec,
                "agent.finished",
                serde_json::json!({
                    "status": "failed",
                    "summary": truncate(&summary),
                    "backend": "acp",
                    "acp_agent": acp_agent.name,
                }),
            )
            .await?;
            return Ok(AcpDelegation::Output(ToolOutput::error(summary)));
        }
    };
    if outcome.cancelled {
        engine.update_agent_status(&child.id, "cancelled").await?;
        emit(
            engine,
            &session.id,
            &child_spec,
            "agent.finished",
            serde_json::json!({
                "status": "cancelled",
                "backend": "acp",
                "acp_agent": acp_agent.name,
            }),
        )
        .await?;
        return Ok(AcpDelegation::Cancelled);
    }
    if outcome.stop_reason != "end_turn" {
        let summary = format!(
            "external ACP agent stopped with {} without completing the task",
            outcome.stop_reason
        );
        engine.update_agent_status(&child.id, "failed").await?;
        emit(
            engine,
            &session.id,
            &child_spec,
            "agent.finished",
            serde_json::json!({
                "status": "failed",
                "summary": summary,
                "backend": "acp",
                "acp_agent": outcome.agent_name,
                "stop_reason": outcome.stop_reason,
            }),
        )
        .await?;
        return Ok(AcpDelegation::Output(ToolOutput::error(summary)));
    }
    let summary = host.final_summary();
    emit(
        engine,
        &session.id,
        &child_spec,
        "agent.message",
        serde_json::json!({
            "text": summary,
            "backend": "acp",
            "acp_agent": outcome.agent_name,
            "acp_version": outcome.agent_version,
            "acp_transport": outcome.transport.as_str(),
            "acp_session_id": outcome.session_id,
            "acp_resumed": outcome.resumed,
            "protocol_version": outcome.protocol_version,
        }),
    )
    .await?;
    match finish_worker(
        engine,
        config,
        session,
        project,
        control,
        budget,
        parent_spec,
        &child_spec,
        &objective,
        &summary,
        None,
    )
    .await?
    {
        WorkerFinish::Done { report, success } => {
            let report = format!(
                "External worker {} finished:
{}",
                child_name, report
            );
            if success {
                Ok(AcpDelegation::Output(ToolOutput::ok(report)))
            } else {
                Ok(AcpDelegation::Output(ToolOutput::error(report)))
            }
        }
        WorkerFinish::Cancelled => Ok(AcpDelegation::Cancelled),
        WorkerFinish::BudgetExceeded(stop) => {
            if stop.is_abortive() {
                Ok(AcpDelegation::BudgetExceeded(stop))
            } else {
                Ok(AcpDelegation::Output(ToolOutput::error(stop.summary())))
            }
        }
    }
}

pub(super) fn worker_report_reports_failure(summary: &str) -> bool {
    let lower = summary.to_ascii_lowercase();
    [
        "blocked",
        "permission denied",
        "no write tool",
        "cannot proceed",
        "could not complete",
        "unable to complete",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

pub(super) enum WorkerFinish {
    Done { report: String, success: bool },
    Cancelled,
    BudgetExceeded(BudgetStop),
}

#[allow(clippy::type_complexity)]
pub(super) fn parse_delegate(
    input: &serde_json::Value,
    parent_role: AgentRole,
    parent_profile: AgentProfile,
    team_plan: Option<&TeamPlan>,
) -> Result<(
    AgentRole,
    Option<String>,
    String,
    Option<String>,
    AgentProfile,
)> {
    let role_name = input
        .get("role")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("role is required"))?;
    let role = AgentRole::parse(role_name).ok_or_else(|| anyhow!("unknown role: {role_name}"))?;
    let expected = parent_role
        .child_role()
        .ok_or_else(|| anyhow!("{} cannot delegate", parent_role.as_str()))?;
    if role != expected {
        return Err(anyhow!(
            "{} can only delegate to {}",
            parent_role.as_str(),
            expected.as_str()
        ));
    }
    let objective = input
        .get("objective")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|objective| !objective.is_empty() && objective.len() <= 4000)
        .ok_or_else(|| anyhow!("objective is required (max 4000 chars)"))?
        .to_string();
    let name = input
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty() && name.len() <= 80)
        .map(ToOwned::to_owned);
    let instructions = input
        .get("instructions")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|instructions| !instructions.is_empty() && instructions.len() <= 4000);
    let objective = match instructions {
        Some(instructions) => format!("{objective}\n\nInstructions:\n{instructions}"),
        None => objective,
    };
    let title = if role == AgentRole::Manager {
        let title = input
            .get("title")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|title| !title.is_empty() && title.len() <= 60)
            .map(ToOwned::to_owned);
        if title.is_none() {
            return Err(anyhow!(
                "Managers need a title such as Discovery, Implementation, or Quality"
            ));
        }
        title
    } else {
        None
    };
    let requested_profile = match input.get("profile") {
        None => None,
        Some(value) => {
            let value = value
                .as_str()
                .ok_or_else(|| anyhow!("profile must be a string"))?;
            Some(AgentProfile::parse(value).ok_or_else(|| anyhow!("unknown profile: {value}"))?)
        }
    };
    let profile = match role {
        AgentRole::Manager => {
            if let Some(plan) = team_plan {
                let title = title.as_deref().unwrap_or_default();
                let department = plan
                    .departments
                    .iter()
                    .find(|department| department.manager_title.eq_ignore_ascii_case(title))
                    .ok_or_else(|| {
                        anyhow!("Manager title '{title}' is not in the submitted team plan")
                    })?;
                AgentProfile::parse(&department.profile)
                    .ok_or_else(|| anyhow!("unknown plan profile: {}", department.profile))?
            } else {
                requested_profile.unwrap_or(AgentProfile::Coding)
            }
        }
        AgentRole::Worker => requested_profile.unwrap_or(parent_profile),
        AgentRole::Labour | AgentRole::Verifier | AgentRole::Leader | AgentRole::Scout => {
            parent_profile
        }
    };
    Ok((role, name, objective, title, profile))
}

pub(super) fn parse_write_scope(input: &serde_json::Value) -> Result<Vec<String>> {
    let Some(items) = input.get("write_scope") else {
        return Ok(Vec::new());
    };
    let items = items
        .as_array()
        .ok_or_else(|| anyhow!("write_scope must be an array"))?;
    if items.len() > 20 {
        return Err(anyhow!("at most 20 write scopes are allowed"));
    }
    let mut scopes = Vec::with_capacity(items.len());
    for item in items {
        let scope = item
            .as_str()
            .ok_or_else(|| anyhow!("write_scope entries must be strings"))?;
        let scope = normalize_scope(scope);
        if scope.is_empty()
            || scope.contains("..")
            || scope.starts_with('/')
            || scope.starts_with('~')
        {
            return Err(anyhow!("invalid write scope: {scope}"));
        }
        scopes.push(scope);
    }
    Ok(scopes)
}

pub(super) struct ParallelTask {
    pub(super) name: Option<String>,
    pub(super) objective: String,
    pub(super) scope: Vec<String>,
    pub(super) profile: AgentProfile,
}

pub(super) enum ParallelResult {
    Output(ToolOutput),
    Cancelled,
    BudgetExceeded(BudgetStop),
    Deferred(DeferredWait),
}

pub(super) fn parse_parallel_tasks(
    input: &serde_json::Value,
    parent_profile: AgentProfile,
) -> Result<Vec<ParallelTask>> {
    let tasks = input
        .get("tasks")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow!("tasks is required"))?;
    if tasks.is_empty() || tasks.len() > 10 {
        return Err(anyhow!("delegate_many accepts 1 to 10 tasks"));
    }
    let mut parsed = Vec::with_capacity(tasks.len());
    for task in tasks {
        let objective = task
            .get("objective")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|objective| !objective.is_empty() && objective.len() <= 4000)
            .ok_or_else(|| anyhow!("each task needs an objective (max 4000 chars)"))?;
        let instructions = task
            .get("instructions")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|instructions| !instructions.is_empty() && instructions.len() <= 4000);
        let objective = match instructions {
            Some(instructions) => format!("{objective}\n\nInstructions:\n{instructions}"),
            None => objective.to_string(),
        };
        let name = task
            .get("name")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty() && name.len() <= 80)
            .map(ToOwned::to_owned);
        let scope = parse_write_scope(task)?;
        let profile = match task.get("profile") {
            None => parent_profile,
            Some(value) => {
                let value = value
                    .as_str()
                    .ok_or_else(|| anyhow!("profile must be a string"))?;
                AgentProfile::parse(value).ok_or_else(|| anyhow!("unknown profile: {value}"))?
            }
        };
        if tasks.len() > 1 && scope.is_empty() {
            return Err(anyhow!(
                "parallel Workers require a non-empty write_scope for every task"
            ));
        }
        parsed.push(ParallelTask {
            name,
            objective,
            scope,
            profile,
        });
    }
    for (index, left) in parsed.iter().enumerate() {
        for right in parsed.iter().skip(index + 1) {
            if scopes_overlap(&left.scope, &right.scope) {
                return Err(anyhow!(
                    "write scopes overlap: {} and {}",
                    left.scope.join(","),
                    right.scope.join(",")
                ));
            }
        }
    }
    Ok(parsed)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_parallel_workers(
    engine: &Engine,
    config: &AgentConfig,
    session: &Session,
    project: &Project,
    control: &RunControl,
    budget: &SessionBudget,
    spec: &AgentSpec,
    input: &serde_json::Value,
) -> Result<ParallelResult> {
    let tasks = parse_parallel_tasks(input, spec.profile)?;
    let mut prepared = Vec::new();
    let mut worker_slots = Vec::new();
    for task in tasks {
        if control.is_cancelled() {
            return Ok(ParallelResult::Cancelled);
        }
        let Some((ordinal, slot)) = budget.reserve_slot(AgentRole::Worker) else {
            return Ok(ParallelResult::Output(ToolOutput::error(
                "the Worker cap for this session has been reached",
            )));
        };
        worker_slots.push(slot);
        let child_name = task
            .name
            .clone()
            .unwrap_or_else(|| format!("Worker {}", ordinal));
        let child = engine
            .create_agent(
                &session.id,
                AgentRole::Worker.as_str(),
                &child_name,
                None,
                Some(spec.id.clone()),
                &model_for(config, AgentRole::Worker),
            )
            .await?;
        let workspace_binding = engine
            .attach_worker_workspace_for_run(&session.id, None, spec.department.as_deref())
            .await?;
        let (child_identity_id, child_identity_generation) = match workspace_binding.as_ref() {
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
                    None,
                    spec.department.as_deref(),
                )
                .await?
                .map(|identity| (identity.id, identity.generation))
                .map(|(id, generation)| (Some(id), Some(generation)))
                .unwrap_or((None, None)),
        };
        let (info, workspace_id, session_workspace_id) = match workspace_binding {
            Some(binding) => {
                let head_path = PathBuf::from(&binding.session.worktree_path);
                let head = tokio::task::spawn_blocking(move || worktree::head_commit(&head_path))
                    .await??;
                (
                    crate::worktree::WorktreeInfo {
                        path: PathBuf::from(&binding.session.worktree_path),
                        branch: binding.session.branch.clone(),
                        base_commit: head,
                    },
                    Some(binding.workspace.id),
                    Some(binding.session.id),
                )
            }
            None => (
                engine
                    .create_task_worktree(
                        &project.root,
                        &project.id,
                        &session.id,
                        &child.id,
                        &spec.session_branch,
                    )
                    .await?,
                None,
                None,
            ),
        };
        engine
            .set_agent_worktree(&child.id, &info.path.to_string_lossy(), &info.branch)
            .await?;
        engine
            .set_agent_workspace_binding(&child.id, workspace_id.as_deref(), None)
            .await?;
        let child_spec = AgentSpec {
            id: child.id.clone(),
            role: AgentRole::Worker,
            name: child_name.clone(),
            depth: spec.depth.saturating_add(1),
            parent_name: Some(spec.name.clone()),
            title: None,
            task_id: None,
            department: spec.department.clone(),
            identity_id: child_identity_id,
            identity_generation: child_identity_generation,
            profile: task.profile,
            allow_delegation: true,
            task_kind: None,
            auto_approve: false,
            worktree: info.path,
            branch: info.branch,
            session_worktree: spec.session_worktree.clone(),
            session_branch: spec.session_branch.clone(),
            write_scope: task.scope.clone(),
            base_commit: info.base_commit,
            workspace_id,
            session_workspace_id,
            parent_workspace_id: None,
        };
        emit(
            engine,
            &session.id,
            &child_spec,
            "agent.spawned",
            serde_json::json!({
                "parent_id": spec.id,
                "parent_name": spec.name,
                "objective": truncate(&task.objective),
                "write_scope": child_spec.write_scope,
            }),
        )
        .await?;
        let conversation = vec![ChatMessage::user(task.objective.clone())];
        prepared.push((child_spec, task.objective, conversation));
    }

    let futures = prepared
        .iter_mut()
        .map(|(child_spec, objective, conversation)| {
            let workspace_lock = child_spec
                .workspace_id
                .as_ref()
                .map(|workspace_id| engine.workspace_run_lock(workspace_id));
            async move {
                let _guard = match workspace_lock {
                    Some(lock) => Some(lock.lock_owned().await),
                    None => None,
                };
                refresh_workspace_base(child_spec).await;
                run_loop(
                    engine,
                    config,
                    session,
                    project,
                    control,
                    budget,
                    child_spec,
                    objective,
                    conversation,
                    None,
                    false,
                    None,
                )
                .await
            }
        });
    let results = futures_util::future::join_all(futures).await;

    let mut cancelled = false;
    let mut budget_exceeded = false;
    let mut handled = vec![false; prepared.len()];
    for (index, result) in results.iter().enumerate() {
        handled[index] = true;
        match result {
            Ok(LoopOutcome::Cancelled) => {
                cancelled = true;
                let (child_spec, _, _) = &prepared[index];
                engine
                    .update_agent_status(&child_spec.id, "cancelled")
                    .await?;
                emit(
                    engine,
                    &session.id,
                    child_spec,
                    "agent.finished",
                    serde_json::json!({"status": "cancelled"}),
                )
                .await?;
            }
            Ok(LoopOutcome::Verdict(_)) => {}
            Ok(LoopOutcome::BudgetExceeded(stop)) => {
                if stop.is_abortive() {
                    budget_exceeded = true;
                }
                let (child_spec, _, _) = &prepared[index];
                engine.update_agent_status(&child_spec.id, "failed").await?;
                emit(
                    engine,
                    &session.id,
                    child_spec,
                    "agent.finished",
                    serde_json::json!({
                        "status": "failed",
                        "summary": stop.summary(),
                    }),
                )
                .await?;
            }
            _ => {}
        }
    }
    if cancelled || budget_exceeded {
        let status = if cancelled { "cancelled" } else { "failed" };
        for (index, (child_spec, _, _)) in prepared.iter().enumerate() {
            if handled[index] {
                continue;
            }
            engine.update_agent_status(&child_spec.id, status).await?;
            emit(
                engine,
                &session.id,
                child_spec,
                "agent.finished",
                serde_json::json!({
                    "status": status,
                    "summary": "batch aborted before integration",
                }),
            )
            .await?;
        }
        return if cancelled {
            Ok(ParallelResult::Cancelled)
        } else {
            Ok(ParallelResult::BudgetExceeded(BudgetStop::session(budget)))
        };
    }

    let mut reports = Vec::new();
    let mut all_success = true;
    let mut deferred_wait: Option<DeferredWait> = None;
    for (index, result) in results.into_iter().enumerate() {
        let (child_spec, objective, _) = &prepared[index];
        match result {
            Ok(LoopOutcome::Final(summary)) => {
                let summary = if summary.trim().is_empty() {
                    "The agent completed without a summary.".to_string()
                } else {
                    summary
                };
                match finish_worker(
                    engine, config, session, project, control, budget, spec, child_spec, objective,
                    &summary, None,
                )
                .await?
                {
                    WorkerFinish::Done { report, success } => {
                        all_success &= success;
                        reports.push(format!("{}: {}", child_spec.name, report));
                    }
                    WorkerFinish::Cancelled => {
                        for (_, (remaining, _, _)) in prepared.iter().enumerate().skip(index + 1) {
                            engine
                                .update_agent_status(&remaining.id, "cancelled")
                                .await?;
                            emit(
                                engine,
                                &session.id,
                                remaining,
                                "agent.finished",
                                serde_json::json!({
                                    "status": "cancelled",
                                    "summary": "batch aborted during verification",
                                }),
                            )
                            .await?;
                        }
                        return Ok(ParallelResult::Cancelled);
                    }
                    WorkerFinish::BudgetExceeded(stop) => {
                        for (_, (remaining, _, _)) in prepared.iter().enumerate().skip(index + 1) {
                            engine.update_agent_status(&remaining.id, "failed").await?;
                            emit(
                                engine,
                                &session.id,
                                remaining,
                                "agent.finished",
                                serde_json::json!({
                                    "status": "failed",
                                    "summary": "batch aborted during verification",
                                }),
                            )
                            .await?;
                        }
                        return Ok(ParallelResult::BudgetExceeded(stop));
                    }
                }
            }
            Ok(LoopOutcome::Verdict(_)) | Ok(LoopOutcome::Cancelled) => {
                all_success = false;
            }
            Ok(LoopOutcome::Deferred(wait)) => {
                all_success = false;
                engine.update_agent_status(&child_spec.id, "idle").await?;
                emit(
                    engine,
                    &session.id,
                    child_spec,
                    "agent.finished",
                    serde_json::json!({
                        "status": "waiting_gate",
                        "gate_id": wait.gate_id,
                        "continuation_id": wait.continuation_id,
                    }),
                )
                .await?;
                reports.push(format!(
                    "{}: waiting for external gate {}",
                    child_spec.name, wait.gate_id
                ));
                if deferred_wait.is_none() {
                    deferred_wait = Some(wait);
                }
            }
            Ok(LoopOutcome::BudgetExceeded(stop)) => {
                if !stop.is_abortive() {
                    all_success = false;
                    reports.push(format!("{}: {}", child_spec.name, stop.summary()));
                }
            }
            Err(error) => {
                all_success = false;
                engine.update_agent_status(&child_spec.id, "failed").await?;
                emit(
                    engine,
                    &session.id,
                    child_spec,
                    "agent.finished",
                    serde_json::json!({
                        "status": "failed",
                        "summary": truncate(error.to_string().as_str()),
                    }),
                )
                .await?;
                reports.push(format!("{}: failed: {error}", child_spec.name));
            }
        }
    }
    let text = reports.join("\n\n");
    if let Some(wait) = deferred_wait {
        return Ok(ParallelResult::Deferred(wait));
    }
    Ok(ParallelResult::Output(if all_success {
        ToolOutput::ok(text)
    } else {
        ToolOutput::error(text)
    }))
}

use super::*;

pub(super) struct ScoutRequest {
    pub(super) objective: String,
    pub(super) files: Vec<String>,
    pub(super) max_steps: u32,
}

pub(super) enum ScoutRunResult {
    Output(ToolOutput),
    Cancelled,
    BudgetExceeded(BudgetStop),
}

pub(super) fn parse_scout(input: &serde_json::Value) -> Result<ScoutRequest> {
    let objective = input
        .get("objective")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|objective| {
            let chars = objective.chars().count();
            (1..=4000).contains(&chars)
        })
        .ok_or_else(|| anyhow!("objective is required (1 to 4000 chars)"))?
        .to_string();
    let mut files = Vec::new();
    if let Some(values) = input.get("files") {
        let values = values
            .as_array()
            .ok_or_else(|| anyhow!("files must be an array"))?;
        if values.len() > 20 {
            return Err(anyhow!("at most 20 files may be suggested"));
        }
        for value in values {
            let value = value
                .as_str()
                .ok_or_else(|| anyhow!("files entries must be strings"))?
                .trim();
            if value.is_empty() {
                continue;
            }
            if value.chars().count() > 400 {
                return Err(anyhow!("file hints are limited to 400 chars"));
            }
            if !files.iter().any(|existing| existing == value) {
                files.push(value.to_string());
            }
        }
    }
    let max_steps = input
        .get("max_steps")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(6)
        .clamp(1, 12) as u32;
    Ok(ScoutRequest {
        objective,
        files,
        max_steps,
    })
}

pub(super) fn scout_brief(parent: &AgentSpec, request: &ScoutRequest, steps: u32) -> String {
    let files = if request.files.is_empty() {
        "No specific files were provided; locate the relevant files with find_files, grep, or repo_map.".to_string()
    } else {
        request
            .files
            .iter()
            .map(|file| format!("- {file}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "Read-only Scout task from {parent}.\n\
         Objective: {objective}\n\
         Suggested starting points:\n{files}\n\n\
         Rules:\n\
         - Do not modify files. Do not run shell commands, write, edit, apply patches, delegate, commit, or merge.\n\
         - Use only the read-only tools advertised to you. If a tool is not advertised, it is not available.\n\
         - Every code claim must cite an exact path:line reference. Prefer the smallest useful excerpt over a file dump.\n\
         - For indexed project sources, cite the source title and chunk number; do not return a bare [cN] key the parent cannot resolve.\n\
         - If the evidence is missing, ambiguous, or cannot be obtained with read-only tools, say so explicitly.\n\
         - Return a concise brief with findings first and open questions last.\n\
         You have at most {steps} turns. When you have enough evidence, reply with the final brief and no tool calls.",
        parent = parent.name,
        objective = request.objective,
        steps = steps,
    )
}

pub(super) async fn scout_usage(
    engine: &Engine,
    session_id: &str,
    agent_id: &str,
) -> serde_json::Value {
    let ledger = match engine.usage_ledger(session_id).await {
        Ok(ledger) => ledger,
        Err(error) => {
            tracing::warn!(%error, %agent_id, "could not read Scout usage");
            return serde_json::json!({
                "calls": 0,
                "input_tokens": 0,
                "output_tokens": 0,
                "cache_read_tokens": 0,
                "cache_write_tokens": 0,
                "total_tokens": 0,
                "cost_usd": 0.0,
            });
        }
    };
    let entries = ledger
        .entries
        .iter()
        .filter(|entry| entry.agent_id == agent_id);
    let mut calls = 0u64;
    let mut input_tokens = 0u64;
    let mut output_tokens = 0u64;
    let mut cache_read_tokens = 0u64;
    let mut cache_write_tokens = 0u64;
    let mut cost_usd = 0.0f64;
    for entry in entries {
        calls = calls.saturating_add(1);
        input_tokens = input_tokens.saturating_add(entry.input_tokens);
        output_tokens = output_tokens.saturating_add(entry.output_tokens);
        cache_read_tokens = cache_read_tokens.saturating_add(entry.cache_read_tokens);
        cache_write_tokens = cache_write_tokens.saturating_add(entry.cache_write_tokens);
        cost_usd += entry.cost_usd;
    }
    serde_json::json!({
        "calls": calls,
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "cache_read_tokens": cache_read_tokens,
        "cache_write_tokens": cache_write_tokens,
        "total_tokens": input_tokens.saturating_add(output_tokens),
        "cost_usd": cost_usd,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_scout(
    engine: &Engine,
    config: &AgentConfig,
    session: &Session,
    project: &Project,
    control: &RunControl,
    budget: &SessionBudget,
    parent: &AgentSpec,
    input: &serde_json::Value,
) -> Result<ScoutRunResult> {
    let request = match parse_scout(input) {
        Ok(request) => request,
        Err(error) => return Ok(ScoutRunResult::Output(ToolOutput::error(error.to_string()))),
    };
    let Some((ordinal, _slot)) = budget.reserve_slot(AgentRole::Scout) else {
        return Ok(ScoutRunResult::Output(ToolOutput::error(format!(
            "the Scout cap for this session has been reached (max {SCOUT_CAP})"
        ))));
    };
    if control.is_cancelled() {
        return Ok(ScoutRunResult::Cancelled);
    }
    let worktree = parent.session_worktree.clone();
    let before = match tokio::task::spawn_blocking({
        let worktree = worktree.clone();
        move || worktree::status_snapshot(&worktree)
    })
    .await?
    {
        Ok(before) => before,
        Err(error) => {
            return Ok(ScoutRunResult::Output(ToolOutput::error(format!(
                "could not inspect the session worktree before the Scout run: {error}"
            ))));
        }
    };
    let existing_scouts = engine
        .list_agents(&session.id)
        .await?
        .iter()
        .filter(|agent| agent.role == AgentRole::Scout.as_str())
        .count();
    let scout_name = format!("Scout {}", existing_scouts.saturating_add(ordinal));
    let child = engine
        .create_agent(
            &session.id,
            AgentRole::Scout.as_str(),
            &scout_name,
            Some("Scout".to_string()),
            Some(parent.id.clone()),
            &model_for(config, AgentRole::Scout),
        )
        .await?;
    engine
        .set_agent_profile(&child.id, AgentProfile::Research.as_str())
        .await?;
    engine
        .set_agent_worktree(
            &child.id,
            &worktree.to_string_lossy(),
            &parent.session_branch,
        )
        .await?;
    engine
        .set_agent_task(&child.id, parent.task_id.clone(), parent.department.clone())
        .await?;
    engine.update_agent_status(&child.id, "running").await?;
    let child_spec = AgentSpec {
        id: child.id.clone(),
        role: AgentRole::Scout,
        name: scout_name.clone(),
        depth: parent.depth.saturating_add(1),
        parent_name: Some(parent.name.clone()),
        title: Some("Scout".to_string()),
        task_id: parent.task_id.clone(),
        department: parent.department.clone(),
        identity_id: None,
        identity_generation: None,
        profile: AgentProfile::Research,
        allow_delegation: false,
        task_kind: None,
        auto_approve: false,
        worktree: worktree.clone(),
        branch: parent.session_branch.clone(),
        session_worktree: worktree.clone(),
        session_branch: parent.session_branch.clone(),
        write_scope: Vec::new(),
        base_commit: parent.base_commit.clone(),

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
            "parent_id": parent.id,
            "parent_name": parent.name,
            "objective": truncate_to(&request.objective, 1_000),
            "read_only": true,
        }),
    )
    .await?;
    emit(
        engine,
        &session.id,
        &child_spec,
        "agent.started",
        serde_json::json!({
            "parent_id": parent.id,
            "parent_name": parent.name,
            "objective": truncate_to(&request.objective, 1_000),
            "files": request.files,
            "max_steps": request.max_steps,
            "model": model_for(config, AgentRole::Scout),
            "worktree": child_spec.worktree,
            "branch": child_spec.branch,
            "read_only": true,
        }),
    )
    .await?;
    // Never raise the configured global turn limit; `max_steps` can only
    // narrow the Scout's run.
    let scout_turns = config.max_turns.min(request.max_steps);
    let brief = scout_brief(parent, &request, scout_turns);
    let mut conversation = vec![ChatMessage::user(brief.clone())];
    let mut scout_config = config.clone();
    scout_config.max_turns = scout_turns;
    let result = run_loop(
        engine,
        &scout_config,
        session,
        project,
        control,
        budget,
        &child_spec,
        &brief,
        &mut conversation,
        None,
        false,
        None,
    )
    .await;
    let usage = scout_usage(engine, &session.id, &child.id).await;
    let completed = |status: &str, summary: &str, extra: serde_json::Value| {
        let mut payload = serde_json::json!({
            "agent_id": child.id,
            "parent_id": parent.id,
            "status": status,
            "summary": truncate_to(summary, SCOUT_SUMMARY_MAX_CHARS),
            "usage": usage,
            "cancelled": status == "cancelled",
            "read_only": true,
        });
        if let (Some(object), Some(extra)) = (payload.as_object_mut(), extra.as_object()) {
            for (key, value) in extra {
                object.insert(key.clone(), value.clone());
            }
        }
        payload
    };
    match result {
        Ok(LoopOutcome::Verdict(_)) => {
            let summary = "Scout returned an unexpected verdict.".to_string();
            engine.update_agent_status(&child.id, "failed").await?;
            emit(
                engine,
                &session.id,
                &child_spec,
                "agent.finished",
                serde_json::json!({"status": "failed", "summary": summary}),
            )
            .await?;
            emit(
                engine,
                &session.id,
                &child_spec,
                "scout.completed",
                completed("failed", &summary, serde_json::json!({})),
            )
            .await?;
            Ok(ScoutRunResult::Output(ToolOutput::error(summary)))
        }
        Ok(LoopOutcome::Final(summary)) => {
            let summary = if summary.trim().is_empty() {
                "The Scout completed without a brief.".to_string()
            } else {
                summary
            };
            let after_result = tokio::task::spawn_blocking({
                let worktree = worktree.clone();
                move || worktree::status_snapshot(&worktree)
            })
            .await;
            let after = match after_result {
                Ok(Ok(after)) => after,
                Ok(Err(error)) => {
                    let summary = format!(
                        "could not inspect the session worktree after the Scout run: {error}"
                    );
                    engine.update_agent_status(&child.id, "failed").await?;
                    emit(
                        engine,
                        &session.id,
                        &child_spec,
                        "agent.finished",
                        serde_json::json!({"status": "failed", "summary": summary}),
                    )
                    .await?;
                    emit(
                        engine,
                        &session.id,
                        &child_spec,
                        "scout.completed",
                        completed(
                            "failed",
                            &summary,
                            serde_json::json!({"guard_failed": true}),
                        ),
                    )
                    .await?;
                    return Ok(ScoutRunResult::Output(ToolOutput::error_structured(
                        summary.clone(),
                        serde_json::json!({
                            "agent_id": child.id,
                            "agent_name": scout_name,
                            "summary": summary,
                            "usage": usage,
                            "cancelled": false,
                            "guard_failed": true,
                        }),
                    )));
                }
                Err(error) => {
                    let summary = format!(
                        "could not inspect the session worktree after the Scout run: {error}"
                    );
                    engine.update_agent_status(&child.id, "failed").await?;
                    emit(
                        engine,
                        &session.id,
                        &child_spec,
                        "agent.finished",
                        serde_json::json!({"status": "failed", "summary": summary}),
                    )
                    .await?;
                    emit(
                        engine,
                        &session.id,
                        &child_spec,
                        "scout.completed",
                        completed(
                            "failed",
                            &summary,
                            serde_json::json!({"guard_failed": true}),
                        ),
                    )
                    .await?;
                    return Ok(ScoutRunResult::Output(ToolOutput::error_structured(
                        summary.clone(),
                        serde_json::json!({
                            "agent_id": child.id,
                            "agent_name": scout_name,
                            "summary": summary,
                            "usage": usage,
                            "cancelled": false,
                            "guard_failed": true,
                        }),
                    )));
                }
            };
            let changed = after
                .into_iter()
                .filter(|line| !before.contains(line))
                .collect::<Vec<_>>();
            if !changed.is_empty() {
                let detail = format!(
                    "Scout modified the session worktree: {}",
                    truncate_to(&changed.join("; "), 500)
                );
                engine.update_agent_status(&child.id, "failed").await?;
                emit(
                    engine,
                    &session.id,
                    &child_spec,
                    "agent.finished",
                    serde_json::json!({
                        "status": "failed",
                        "summary": detail,
                        "modified_paths": changed,
                        "guard_failed": true,
                    }),
                )
                .await?;
                emit(
                    engine,
                    &session.id,
                    &child_spec,
                    "scout.completed",
                    completed(
                        "rejected",
                        &summary,
                        serde_json::json!({
                            "guard_failed": true,
                            "modified_paths": changed,
                        }),
                    ),
                )
                .await?;
                return Ok(ScoutRunResult::Output(ToolOutput::error_structured(
                    detail,
                    serde_json::json!({
                        "agent_id": child.id,
                        "agent_name": scout_name,
                        "summary": truncate_to(&summary, SCOUT_SUMMARY_MAX_CHARS),
                        "usage": usage,
                        "cancelled": false,
                        "guard_failed": true,
                        "modified_paths": changed,
                    }),
                )));
            }
            let bounded = truncate_to(&summary, SCOUT_SUMMARY_MAX_CHARS);
            engine.update_agent_status(&child.id, "idle").await?;
            emit(
                engine,
                &session.id,
                &child_spec,
                "agent.finished",
                serde_json::json!({
                    "status": "idle",
                    "summary": bounded,
                    "worktree": child_spec.worktree,
                    "branch": child_spec.branch,
                }),
            )
            .await?;
            emit(
                engine,
                &session.id,
                &child_spec,
                "scout.completed",
                completed("completed", &bounded, serde_json::json!({})),
            )
            .await?;
            let content = format!(
                "{} ({}) completed.\nObjective: {}\nUsage: {} input + {} output tokens ({} calls).\n\n{}",
                scout_name,
                child.id,
                truncate_to(&request.objective, 400),
                usage["input_tokens"].as_u64().unwrap_or(0),
                usage["output_tokens"].as_u64().unwrap_or(0),
                usage["calls"].as_u64().unwrap_or(0),
                bounded
            );
            Ok(ScoutRunResult::Output(ToolOutput::ok_structured(
                content,
                serde_json::json!({
                    "agent_id": child.id,
                    "agent_name": scout_name,
                    "summary": bounded,
                    "usage": usage,
                    "cancelled": false,
                    "read_only": true,
                }),
            )))
        }
        Ok(LoopOutcome::Deferred(wait)) => {
            let summary = format!(
                "Scout cannot wait on external gate {}; it must return a bounded brief",
                wait.gate_id
            );
            engine.update_agent_status(&child.id, "failed").await?;
            emit(
                engine,
                &session.id,
                &child_spec,
                "agent.finished",
                serde_json::json!({"status": "failed", "summary": summary}),
            )
            .await?;
            emit(
                engine,
                &session.id,
                &child_spec,
                "scout.completed",
                completed("failed", &summary, serde_json::json!({})),
            )
            .await?;
            Ok(ScoutRunResult::Output(ToolOutput::error(summary)))
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
            emit(
                engine,
                &session.id,
                &child_spec,
                "scout.completed",
                completed("cancelled", "", serde_json::json!({})),
            )
            .await?;
            Ok(ScoutRunResult::Cancelled)
        }
        Ok(LoopOutcome::BudgetExceeded(stop)) => {
            let summary = stop.summary();
            engine.update_agent_status(&child.id, "failed").await?;
            emit(
                engine,
                &session.id,
                &child_spec,
                "agent.finished",
                serde_json::json!({"status": "failed", "summary": summary}),
            )
            .await?;
            emit(
                engine,
                &session.id,
                &child_spec,
                "scout.completed",
                completed("budget_exceeded", &summary, serde_json::json!({})),
            )
            .await?;
            Ok(ScoutRunResult::BudgetExceeded(stop))
        }
        Err(error) => {
            let summary = truncate_to(&error.to_string(), SCOUT_SUMMARY_MAX_CHARS);
            engine.update_agent_status(&child.id, "failed").await?;
            emit(
                engine,
                &session.id,
                &child_spec,
                "agent.finished",
                serde_json::json!({"status": "failed", "summary": summary}),
            )
            .await?;
            emit(
                engine,
                &session.id,
                &child_spec,
                "scout.completed",
                completed("failed", &summary, serde_json::json!({})),
            )
            .await?;
            Ok(ScoutRunResult::Output(ToolOutput::error_structured(
                format!("{scout_name} failed: {summary}"),
                serde_json::json!({
                    "agent_id": child.id,
                    "agent_name": scout_name,
                    "summary": summary,
                    "usage": usage,
                    "cancelled": false,
                }),
            )))
        }
    }
}

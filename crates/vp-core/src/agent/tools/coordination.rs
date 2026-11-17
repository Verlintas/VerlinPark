use super::*;

pub(in crate::agent) async fn run_coordination_tool(
    engine: &Engine,
    session: &Session,
    spec: &AgentSpec,
    input: serde_json::Value,
) -> Result<ToolOutput> {
    if spec.role == AgentRole::Verifier {
        return Ok(ToolOutput::error(
            "the Verifier records evidence through submit_verdict, not coordination notes",
        ));
    }
    let action = input
        .get("action")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("action is required"))?;
    if !matches!(
        action,
        "note" | "blocker" | "unblock" | "handoff" | "escalate" | "decision" | "list"
    ) {
        return Ok(ToolOutput::error(format!(
            "unsupported coordination action: {action}"
        )));
    }
    if action == "list" {
        let limit = input
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(20)
            .clamp(1, 50) as usize;
        let notes = recent_coordination_notes(engine, &session.id, limit).await?;
        if notes.is_empty() {
            return Ok(ToolOutput::ok("No coordination notes yet."));
        }
        return Ok(ToolOutput::ok(
            notes
                .iter()
                .map(format_coordination_note)
                .collect::<Vec<_>>()
                .join("\n"),
        ));
    }
    if matches!(action, "unblock" | "decision")
        && !matches!(spec.role, AgentRole::Leader | AgentRole::Manager)
    {
        return Ok(ToolOutput::error(format!(
            "only a Leader or Manager may record a coordination {action}"
        )));
    }
    let message = input
        .get("message")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .ok_or_else(|| anyhow!("message is required"))?;
    if message.chars().count() > 2_000 {
        return Ok(ToolOutput::error(
            "coordination message is limited to 2000 chars",
        ));
    }
    let task_id = input
        .get("task_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|task_id| !task_id.is_empty())
        .map(str::to_string);
    if let Some(task_id) = &task_id {
        worktree::validate_task_id(task_id)?;
    }
    let target_agent_id = input
        .get("target_agent_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|target| !target.is_empty())
        .map(str::to_string);
    if let Some(target) = &target_agent_id {
        let agents = engine.list_agents(&session.id).await?;
        if !agents.iter().any(|agent| &agent.id == target) {
            return Ok(ToolOutput::error(format!(
                "target agent is not in this session: {target}"
            )));
        }
    }
    let severity = input
        .get("severity")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(if matches!(action, "blocker" | "escalate") {
            "warning"
        } else {
            "info"
        });
    if !matches!(severity, "info" | "warning" | "critical") {
        return Ok(ToolOutput::error(
            "severity must be info, warning, or critical",
        ));
    }
    engine
        .record_coordination_note(
            &session.id,
            action,
            severity,
            task_id.as_deref(),
            target_agent_id.as_deref(),
            None,
            &spec.id,
            &spec.name,
            Some(spec.role.as_str()),
            message,
        )
        .await?;
    let scope = task_id
        .as_deref()
        .map(|task_id| format!(" for {task_id}"))
        .unwrap_or_default();
    Ok(ToolOutput::ok(format!(
        "recorded {} coordination{scope}: {}",
        action,
        truncate_to(message, 240)
    )))
}

/// Recent persisted coordination notes in ascending order.
pub(in crate::agent) async fn recent_coordination_notes(
    engine: &Engine,
    session_id: &str,
    limit: usize,
) -> Result<Vec<vp_protocol::types::OrchestrationNote>> {
    engine
        .coordination_notes(session_id, limit.clamp(1, 2_000) as u32)
        .await
}

pub(in crate::agent) fn format_coordination_note(
    note: &vp_protocol::types::OrchestrationNote,
) -> String {
    let task = note
        .task_id
        .as_deref()
        .map(|task| format!(" · {task}"))
        .unwrap_or_default();
    let status = if note.status == "resolved" {
        " (resolved)"
    } else {
        ""
    };
    format!(
        "[{}] {}{} · {}{} · {}",
        note.severity,
        note.action,
        status,
        note.agent_name,
        task,
        truncate_to(&note.message, 300)
    )
}

pub(in crate::agent) async fn run_team_status(
    engine: &Engine,
    session: &Session,
    input: serde_json::Value,
) -> Result<ToolOutput> {
    let note_limit = input
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(8)
        .clamp(1, 50) as usize;
    let plan = engine.get_team_plan(&session.id).await?;
    let agents = engine.list_agents(&session.id).await?;
    let queue = engine.task_queue(None, Some(session.id.clone())).await?;
    let merge = engine.list_merge_queue(&session.id).await?;
    let questions = engine
        .list_questions(Some(session.id.clone()))
        .await?
        .questions;
    let recent = engine.recent_events(&session.id, 1_000).await?;

    let plan_tasks = plan
        .as_ref()
        .map(|plan| plan.tasks.as_slice())
        .unwrap_or(&[]);
    let scheduled = queue
        .tasks
        .iter()
        .map(|task| (task.task_id.clone(), task))
        .collect::<HashMap<_, _>>();
    let mut statuses = plan_tasks
        .iter()
        .map(|task| (task.id.clone(), "pending".to_string()))
        .collect::<HashMap<_, _>>();
    for event in &recent {
        if let Some(task_id) = event
            .payload
            .get("task_id")
            .and_then(serde_json::Value::as_str)
            && event.kind.starts_with("plan_task.")
            && let Some(status) = event
                .payload
                .get("task_status")
                .and_then(serde_json::Value::as_str)
        {
            statuses.insert(task_id.to_string(), status.to_string());
        }
    }
    for task in &queue.tasks {
        statuses.insert(task.task_id.clone(), task.status.clone());
    }
    let completed = plan_tasks
        .iter()
        .filter(|task| {
            matches!(
                statuses.get(&task.id).map(String::as_str),
                Some("completed" | "skipped")
            )
        })
        .count();
    let active = plan_tasks
        .iter()
        .filter(|task| {
            matches!(
                statuses.get(&task.id).map(String::as_str),
                Some("running" | "queued" | "paused" | "pausing" | "cancelling")
            )
        })
        .count();
    let ready = plan_tasks
        .iter()
        .filter(|task| {
            matches!(
                statuses.get(&task.id).map(String::as_str),
                Some("pending" | "interrupted" | "failed" | "cancelled")
            ) && task.deps.iter().all(|dep| {
                matches!(
                    statuses.get(dep).map(String::as_str),
                    Some("completed" | "skipped")
                )
            })
        })
        .collect::<Vec<_>>();
    let failed_tasks = plan_tasks
        .iter()
        .filter(|task| {
            matches!(
                statuses.get(&task.id).map(String::as_str),
                Some("failed" | "blocked")
            )
        })
        .collect::<Vec<_>>();
    let role_count = |role: &str| agents.iter().filter(|agent| agent.role == role).count();
    let running = agents
        .iter()
        .filter(|agent| agent.status == "running")
        .count();
    let conflicts = merge
        .iter()
        .filter(|entry| entry.status == "conflict")
        .count();
    let pending_questions = questions
        .iter()
        .filter(|question| question.status == "pending")
        .count();
    let notes = engine
        .coordination_notes(&session.id, note_limit as u32)
        .await?;

    let mut lines = Vec::new();
    lines.push(format!(
        "Team status · session {} · {}",
        session.status, session.goal
    ));
    lines.push(format!(
        "Agents: {} leader · {} managers · {} workers ({} running) · {} verifiers · {} labour · {} scouts",
        role_count("leader"),
        role_count("manager"),
        role_count("worker"),
        running,
        role_count("verifier"),
        role_count("labour"),
        role_count("scout")
    ));
    if plan.is_some() {
        lines.push(format!(
            "Tasks: {completed}/{} complete · {active} active · {} failed/blocked · {} ready",
            plan_tasks.len(),
            failed_tasks.len(),
            ready.len()
        ));
        if !ready.is_empty() {
            lines.push(format!(
                "Ready: {}",
                ready
                    .iter()
                    .take(8)
                    .map(|task| format!("{} ({})", task.id, task.department))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !failed_tasks.is_empty() {
            lines.push("Blocked/failed tasks:".to_string());
            for task in failed_tasks.iter().take(8) {
                let detail = scheduled
                    .get(&task.id)
                    .and_then(|task| task.last_error.as_deref())
                    .unwrap_or("no detail");
                lines.push(format!(
                    "- {} [{}] {}",
                    task.id,
                    statuses
                        .get(&task.id)
                        .map(String::as_str)
                        .unwrap_or("unknown"),
                    truncate_to(detail, 180)
                ));
            }
        }
    } else {
        lines.push("No TeamPlan submitted.".to_string());
    }
    if !merge.is_empty() {
        lines.push(format!(
            "Merge queue: {} entries · {} conflicts",
            merge.len(),
            conflicts
        ));
    }
    if pending_questions > 0 {
        lines.push(format!("User questions waiting: {pending_questions}"));
    }
    if !notes.is_empty() {
        lines.push("Recent coordination:".to_string());
        for note in notes.iter().rev() {
            lines.push(format!("- {}", format_coordination_note(note)));
        }
    }
    Ok(ToolOutput::ok(lines.join("\n")))
}

pub(in crate::agent) async fn run_budget_status(
    engine: &Engine,
    session: &Session,
    project: &Project,
    spec: &AgentSpec,
    budget: &SessionBudget,
    plan_budget: &PlanBudgetScope,
) -> Result<ToolOutput> {
    let unlimited = budget.is_unlimited();
    let spent_tokens = budget.spent();
    let spent_cost = budget.cost_spent();
    let (task_tokens, task_cost, department_tokens, department_cost) = engine
        .usage_spent(
            &session.id,
            spec.task_id.as_deref(),
            spec.department.as_deref(),
        )
        .await?;
    let project_budget = engine.get_project_budget(&project.id).await?;
    let session_value = json!({
        "limit_tokens": budget.limit,
        "spent_tokens": spent_tokens,
        "remaining_tokens": budget.limit.map(|limit| limit.saturating_sub(spent_tokens)),
        "limit_cost_usd": budget.cost_limit,
        "spent_cost_usd": spent_cost,
        "reserved_cost_usd": budget.reserved_cost(),
        "remaining_cost_usd": budget
            .cost_limit
            .map(|limit| (limit - spent_cost - budget.reserved_cost()).max(0.0)),
        "time_limit_secs": budget.time_limit.map(|limit| limit.as_secs()),
        "elapsed_secs": budget.elapsed_secs(),
        "remaining_time_secs": budget
            .time_limit
            .map(|limit| limit.saturating_sub(Duration::from_secs(budget.elapsed_secs())).as_secs()),
        "unlimited": unlimited,
    });
    let task_value = spec.task_id.as_ref().map(|task_id| {
        json!({
            "task_id": task_id,
            "limit_tokens": plan_budget.task_tokens,
            "spent_tokens": task_tokens,
            "remaining_tokens": plan_budget.task_tokens.map(|limit| limit.saturating_sub(task_tokens)),
            "limit_cost_usd": plan_budget.task_cost,
            "spent_cost_usd": task_cost,
            "remaining_cost_usd": plan_budget.task_cost.map(|limit| (limit - task_cost).max(0.0)),
        })
    });
    let department_value = spec.department.as_ref().map(|department| {
        json!({
            "department": department,
            "limit_tokens": plan_budget.department_tokens,
            "spent_tokens": department_tokens,
            "remaining_tokens": plan_budget.department_tokens.map(|limit| limit.saturating_sub(department_tokens)),
            "limit_cost_usd": plan_budget.department_cost,
            "spent_cost_usd": department_cost,
            "remaining_cost_usd": plan_budget.department_cost.map(|limit| (limit - department_cost).max(0.0)),
        })
    });
    let project_value = json!({
        "project_id": project.id,
        "limit_tokens": project_budget.budget.token_budget,
        "spent_tokens": project_budget.spent_tokens,
        "remaining_tokens": project_budget.budget.token_budget.map(|limit| limit.saturating_sub(project_budget.spent_tokens)),
        "limit_cost_usd": project_budget.budget.cost_budget_usd,
        "spent_cost_usd": project_budget.spent_cost_usd,
        "remaining_cost_usd": project_budget.budget.cost_budget_usd.map(|limit| (limit - project_budget.spent_cost_usd).max(0.0)),
    });
    let text = format!(
        "session tokens {}/{} · USD ${:.4}/{}{} · elapsed {}s{}\ntask tokens {}{} · department tokens {}{} · project tokens {}/{}",
        spent_tokens,
        budget
            .limit
            .map(|limit| limit.to_string())
            .unwrap_or_else(|| "unlimited".to_string()),
        spent_cost,
        budget
            .cost_limit
            .map(|limit| format!("{limit:.4}"))
            .unwrap_or_else(|| "unlimited".to_string()),
        if unlimited { " · unlimited mode" } else { "" },
        budget.elapsed_secs(),
        budget
            .time_limit
            .map(|limit| format!("/{}", limit.as_secs()))
            .unwrap_or_default(),
        spec.task_id.as_deref().unwrap_or("none"),
        plan_budget
            .task_tokens
            .map(|limit| format!("/{limit}"))
            .unwrap_or_default(),
        spec.department.as_deref().unwrap_or("none"),
        plan_budget
            .department_tokens
            .map(|limit| format!("/{limit}"))
            .unwrap_or_default(),
        project_budget.spent_tokens,
        project_budget
            .budget
            .token_budget
            .map(|limit| limit.to_string())
            .unwrap_or_else(|| "unlimited".to_string()),
    );
    Ok(ToolOutput::ok_structured(
        truncate(&text),
        json!({
            "session": session_value,
            "task": task_value,
            "department": department_value,
            "project": project_value,
            "unlimited": unlimited,
        }),
    ))
}

pub(in crate::agent) async fn run_task_detail(
    engine: &Engine,
    session: &Session,
    spec: &AgentSpec,
    input: Value,
) -> Result<ToolOutput> {
    let task_id = input
        .get("task_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| spec.task_id.clone())
        .ok_or_else(|| anyhow!("task_id is required outside a plan task"))?;
    let plan = engine
        .get_team_plan(&session.id)
        .await?
        .ok_or_else(|| anyhow!("session has no stored TeamPlan"))?;
    let plan_task = plan
        .tasks
        .iter()
        .find(|task| task.id == task_id)
        .ok_or_else(|| anyhow!("task {task_id} is not in the stored TeamPlan"))?;
    let queue = engine.task_queue(None, Some(session.id.clone())).await?;
    let scheduled = queue.tasks.iter().find(|task| task.task_id == task_id);
    let progress = queue.progress.iter().find(|task| task.task_id == task_id);
    let threshold_secs = crate::task_stall_threshold_secs();
    let observed_at = progress
        .and_then(|progress| progress.last_progress_at.as_deref())
        .or_else(|| scheduled.and_then(|task| task.started_at.as_deref()))
        .or_else(|| scheduled.map(|task| task.enqueued_at.as_str()));
    let stalled = threshold_secs > 0
        && observed_at
            .and_then(|value| {
                time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
                    .ok()
            })
            .is_some_and(|observed| {
                time::OffsetDateTime::now_utc() - observed
                    >= time::Duration::seconds(threshold_secs.min(i64::MAX as u64) as i64)
            });
    let (task_tokens, task_cost, _, _) = engine
        .usage_spent(&session.id, Some(&task_id), Some(&plan_task.department))
        .await?;
    let changed_paths = crate::tooling::project::git_changed_paths(&spec.worktree)
        .into_iter()
        .take(100)
        .collect::<Vec<_>>();
    let structured = json!({
        "task_id": task_id,
        "department": plan_task.department,
        "title": plan_task.title,
        "kind": plan_task.kind,
        "priority": plan_task.priority,
        "profile": plan_task.profile,
        "deps": plan_task.deps,
        "write_scope": plan_task.write_scope,
        "acceptance": plan_task.acceptance,
        "token_budget": plan_task.token_budget,
        "cost_budget_usd": plan_task.cost_budget_usd,
        "spent_tokens": task_tokens,
        "spent_cost_usd": task_cost,
        "status": scheduled.map(|task| task.status.clone()),
        "attempts": scheduled.map(|task| task.attempts),
        "last_error": scheduled.and_then(|task| task.last_error.clone()),
        "next_attempt_at": scheduled.and_then(|task| task.next_attempt_at.clone()),
        "started_at": scheduled.and_then(|task| task.started_at.clone()),
        "finished_at": scheduled.and_then(|task| task.finished_at.clone()),
        "last_progress_at": progress.and_then(|progress| progress.last_progress_at.clone()),
        "stall_count": progress.map(|progress| progress.stall_count).unwrap_or(0),
        "last_turn": progress.and_then(|progress| progress.last_turn),
        "last_tool": progress.and_then(|progress| progress.last_tool.clone()),
        "stall_threshold_secs": threshold_secs,
        "stalled": stalled,
        "changed_paths": changed_paths,
        "current_agent_task": spec.task_id.as_deref() == Some(task_id.as_str()),
    });
    let text = format!(
        "{} · {} · {} · {} attempt(s) · tokens {}{} · changed {} file(s)",
        plan_task.title,
        plan_task.department,
        scheduled
            .map(|task| task.status.as_str())
            .unwrap_or("not queued"),
        scheduled.map(|task| task.attempts).unwrap_or(0),
        task_tokens,
        plan_task
            .token_budget
            .map(|limit| format!("/{limit}"))
            .unwrap_or_default(),
        changed_paths.len()
    );
    Ok(ToolOutput::ok_structured(truncate(&text), structured))
}

pub(in crate::agent) async fn run_task_control(
    engine: &Engine,
    session: &Session,
    spec: &AgentSpec,
    input: Value,
) -> Result<ToolOutput> {
    if !matches!(spec.role, AgentRole::Leader | AgentRole::Manager) {
        return Ok(ToolOutput::error_structured(
            "only Leader or Manager may control plan tasks",
            json!({"code": "PERMISSION_DENIED"}),
        ));
    }
    let action = input
        .get("action")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("action is required"))?;
    let task_id = input
        .get("task_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("task_id is required"))?;
    let plan = engine
        .get_team_plan(&session.id)
        .await?
        .ok_or_else(|| anyhow!("session has no stored TeamPlan"))?;
    if !plan.tasks.iter().any(|task| task.id == task_id) {
        return Ok(ToolOutput::error_structured(
            format!("task {task_id} is not in the stored TeamPlan"),
            json!({"code": "TASK_NOT_FOUND", "task_id": task_id}),
        ));
    }
    let result = match action {
        "retry" => engine.retry_plan_task(&session.id, task_id).await,
        "skip" => engine.skip_plan_task(&session.id, task_id).await,
        "cancel" => engine.request_plan_task_cancel(&session.id, task_id).await,
        "pause" => engine.pause_plan_task(&session.id, task_id).await,
        "resume" => engine.resume_plan_task(&session.id, task_id).await,
        other => {
            return Ok(ToolOutput::error_structured(
                format!("unsupported task action: {other}"),
                json!({"code": "INVALID_ARGUMENT", "action": other}),
            ));
        }
    };
    match result {
        Ok(task) => {
            let structured = serde_json::to_value(&task).unwrap_or_else(|_| json!({}));
            Ok(ToolOutput::ok_structured(
                format!("{action} applied to {task_id} (status {})", task.status),
                json!({
                    "action": action,
                    "task": structured,
                }),
            ))
        }
        Err(error) => Ok(ToolOutput::error_structured(
            format!("{action} failed for {task_id}: {error}"),
            json!({"code": "TASK_CONTROL_FAILED", "action": action, "task_id": task_id}),
        )),
    }
}

#[allow(clippy::large_enum_variant)]
pub(in crate::agent) async fn run_progress_report(
    engine: &Engine,
    session: &Session,
    spec: &AgentSpec,
    input: &Value,
) -> Result<ToolOutput> {
    let round = crate::progress_ledger::ProgressRound {
        round_id: input
            .get("round_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        agent_id: Some(spec.id.clone()),
        agent_name: Some(spec.name.clone()),
        task_id: spec.task_id.clone(),
        satisfied: string_list(input, "satisfied"),
        in_loop: string_list(input, "in_loop"),
        progressing: string_list(input, "progressing"),
        blockers: string_list(input, "blockers"),
        next_task: input
            .get("next_task")
            .and_then(Value::as_str)
            .map(str::to_string),
        instruction: input
            .get("instruction")
            .and_then(Value::as_str)
            .map(str::to_string),
        evidence: input
            .get("evidence")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        let kind = item.get("kind").and_then(Value::as_str)?;
                        let kind = match kind {
                            "task" => crate::progress_ledger::ProgressEvidenceKind::Task,
                            "test" => crate::progress_ledger::ProgressEvidenceKind::Test,
                            "commit" => crate::progress_ledger::ProgressEvidenceKind::Commit,
                            "artifact" => crate::progress_ledger::ProgressEvidenceKind::Artifact,
                            "event" => crate::progress_ledger::ProgressEvidenceKind::Event,
                            "note" => crate::progress_ledger::ProgressEvidenceKind::Note,
                            _ => return None,
                        };
                        Some(crate::progress_ledger::ProgressEvidence {
                            kind,
                            reference: item
                                .get("reference")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            note: item.get("note").and_then(Value::as_str).map(str::to_string),
                        })
                    })
                    .take(8)
                    .collect()
            })
            .unwrap_or_default(),
        created_at: None,
    };
    let result = engine.record_progress_round(&session.id, round).await?;
    let text = format!(
        "Progress {}: {} (in-loop streak {}, stalled rounds {}, satisfied {}, evidence {}){}",
        result.assessment.status.as_str(),
        result.assessment.reason,
        result.assessment.in_loop_streak,
        result.assessment.stalled_rounds,
        result.assessment.satisfied_count,
        result.assessment.evidence_count,
        result
            .assessment
            .next_task
            .as_deref()
            .map(|task| format!("; suggested next task: {task}"))
            .unwrap_or_default()
    );
    Ok(ToolOutput::ok_structured(
        crate::tools::truncate(text),
        serde_json::json!(result),
    ))
}

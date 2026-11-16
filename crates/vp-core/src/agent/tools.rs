use super::*;

pub(super) async fn run_coordination_tool(
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
pub(super) async fn recent_coordination_notes(
    engine: &Engine,
    session_id: &str,
    limit: usize,
) -> Result<Vec<vp_protocol::types::OrchestrationNote>> {
    engine
        .coordination_notes(session_id, limit.clamp(1, 2_000) as u32)
        .await
}

pub(super) fn format_coordination_note(note: &vp_protocol::types::OrchestrationNote) -> String {
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

pub(super) async fn run_team_status(
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

pub(super) async fn run_budget_status(
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

pub(super) async fn run_task_detail(
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

pub(super) async fn run_task_control(
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

/// Read-only guidance. It never executes a tool, calls a provider, or changes
/// the advertised schema. The catalog is process-wide and deterministic.
pub(super) fn run_tool_help(spec: &AgentSpec, input: &Value) -> ToolOutput {
    let slug = input
        .get("recipe")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|slug| !slug.is_empty());
    let requested_code = input
        .get("error_code")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|code| !code.is_empty());
    let include_error_codes = input
        .get("include_error_codes")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let max_chars = input
        .get("max_chars")
        .and_then(Value::as_u64)
        .unwrap_or(crate::tooling::recipes::MAX_HELP_RENDER_CHARS as u64)
        .clamp(1_000, crate::tooling::recipes::MAX_HELP_RENDER_CHARS as u64)
        as usize;
    let role = spec.role.as_str();
    let profile = spec.profile.as_str();
    let (mut rendered, recipes) = if let Some(raw_code) = requested_code {
        match crate::tooling::errors::ToolErrorCode::from_code(raw_code) {
            Some(code) => (crate::tooling::errors::render_code_detail(code), Vec::new()),
            None => {
                return ToolOutput::error_coded(
                    "tool_help",
                    crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
                    format!(
                        "unknown error code '{raw_code}'; call tool_help with include_error_codes=true to list stable codes"
                    ),
                )
                .with_failure_evidence(format!("error_code={raw_code}"));
            }
        }
    } else if include_error_codes && slug.is_none() {
        // A pure code lookup should not be crowded out by the full recipe
        // catalog. Request a recipe explicitly to combine both.
        (String::new(), Vec::new())
    } else {
        crate::tooling::recipes::render_help(role, profile, slug)
    };
    if include_error_codes {
        if !rendered.is_empty() {
            rendered.push_str("\n\n");
        }
        rendered.push_str(&crate::tooling::errors::render_catalog(max_chars));
    }
    if rendered.chars().count() > max_chars {
        rendered = rendered
            .chars()
            .take(max_chars.saturating_sub(1))
            .collect::<String>();
        rendered.push('…');
    }
    let error_code = requested_code
        .and_then(crate::tooling::errors::ToolErrorCode::from_code)
        .map(|code| {
            json!({
                "code": code.as_str(),
                "category": code.category(),
                "retryable": code.default_retryable(),
                "hint": code.default_hint(),
            })
        });
    let structured = json!({
        "version": 1,
        "catalog_hash": crate::tooling::recipes::catalog_hash(),
        "role": role,
        "profile": profile,
        "requested_recipe": slug,
        "requested_error_code": requested_code,
        "error_code": error_code,
        "error_codes_included": include_error_codes,
        "count": recipes.len(),
        "recipes": recipes
            .iter()
            .map(|recipe| json!({
                "slug": recipe.slug,
                "title": recipe.title,
                "intent": recipe.intent,
                "tools": recipe.tools,
                "steps": recipe.steps,
                "example": recipe.example,
                "caveats": recipe.caveats,
            }))
            .collect::<Vec<_>>(),
        "executed_tool": false,
        "note": "Recipes and error codes are static guidance. This tool did not execute or route any tool.",
    });
    if recipes.is_empty() && requested_code.is_none() && !include_error_codes {
        return ToolOutput::error_coded(
            "tool_help",
            crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
            rendered,
        )
        .with_structured(structured);
    }
    ToolOutput::ok_structured(rendered, structured)
}

pub(super) async fn run_tool_search(
    config: &AgentConfig,
    engine: &Engine,
    spec: &AgentSpec,
    project_id: &str,
    frozen_bundle: &crate::tool_bundle::FrozenToolBundle,
    input: Value,
) -> Result<ToolOutput> {
    use crate::tooling::search::{
        MissingPrerequisite, SearchDocument, completeness_estimate, prerequisite_closure,
        rank_documents,
    };

    let query = input
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let limit = input
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .clamp(1, 50) as usize;
    let include_schema = input
        .get("include_schema")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let promote = input
        .get("promote")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let requested_task_kind = input
        .get("task_kind")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| spec.task_kind.clone());
    let advertised = frozen_bundle
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<std::collections::HashSet<_>>();

    struct SearchEntry {
        key: String,
        name: String,
        description: String,
        task_kind: String,
        category: String,
        kind: &'static str,
        callable: bool,
        in_bundle: bool,
        pinned: bool,
        server_id: Option<String>,
        server_name: Option<String>,
        tool_name: Option<String>,
        trust: Option<String>,
        schema: Option<Value>,
    }

    let builtin_schemas = tool_schemas(config, spec.role, spec.profile, spec.allow_delegation)
        .into_iter()
        .filter(|tool| tool.name != "tool_search")
        .collect::<Vec<_>>();
    let available_builtins = builtin_schemas
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let mut entries = builtin_schemas
        .iter()
        .map(|tool| SearchEntry {
            key: format!("builtin:{}", tool.name),
            name: tool.name.clone(),
            description: tool.description.chars().take(4_000).collect(),
            task_kind: spec.task_kind.clone().unwrap_or_default(),
            category: crate::tool_bundle::catalog_entry(&tool.name)
                .map(|entry| entry.category.as_str().to_string())
                .unwrap_or_else(|| "unbundled".to_string()),
            kind: "builtin",
            callable: true,
            in_bundle: advertised.contains(tool.name.as_str()),
            pinned: true,
            server_id: None,
            server_name: None,
            tool_name: None,
            trust: None,
            schema: Some(tool.parameters.clone()),
        })
        .collect::<Vec<_>>();

    let mcp_callable = matches!(
        spec.role,
        AgentRole::Leader | AgentRole::Worker | AgentRole::Labour
    );
    let pin_map = engine
        .project_mcp_pins(project_id, None)
        .await
        .map(|result| {
            result
                .pins
                .into_iter()
                .map(|pin| ((pin.server_id.clone(), pin.tool_name.clone()), pin))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let servers = engine.list_mcp_servers().await?.servers;
    for server in servers.into_iter().filter(|server| server.enabled).take(20) {
        let Ok(tools) = engine.mcp_tools(&server.id).await else {
            continue;
        };
        let server_has_pins = pin_map.keys().any(|(server_id, _)| *server_id == server.id);
        for tool in tools.tools.into_iter().take(100) {
            let exposed = dynamic_mcp_tool_name(&server.name, &tool.name);
            let pin = pin_map.get(&(server.id.clone(), tool.name.clone()));
            let trust = match pin {
                Some(pin) if crate::mcp_pins::pin_is_callable(pin) => pin.trust(),
                Some(pin) if pin.status == "pinned" => "drift",
                Some(pin) => pin.trust(),
                None if !server_has_pins => "approved",
                None => "unverified",
            };
            let pinned = pin.is_none_or(crate::mcp_pins::pin_is_callable);
            entries.push(SearchEntry {
                key: format!("mcp:{}:{}", server.id, tool.name),
                name: exposed.clone(),
                description: tool.description.chars().take(4_000).collect(),
                task_kind: String::new(),
                category: "mcp".to_string(),
                kind: "mcp",
                callable: mcp_callable && pinned,
                in_bundle: advertised.contains(exposed.as_str()),
                pinned,
                server_id: Some(server.id.clone()),
                server_name: Some(server.name.clone()),
                tool_name: Some(tool.name.clone()),
                trust: Some(trust.to_string()),
                schema: Some(tool.input_schema.clone()),
            });
        }
    }

    let documents = entries
        .iter()
        .map(|entry| SearchDocument {
            key: &entry.key,
            name: &entry.name,
            description: &entry.description,
            task_kind: &entry.task_kind,
            category: &entry.category,
        })
        .collect::<Vec<_>>();
    let ranked = rank_documents(&documents, &query, requested_task_kind.as_deref(), 100);
    let ranked_truncated = ranked.len() > limit;
    let selected_keys = ranked
        .iter()
        .take(limit)
        .map(|hit| hit.key.clone())
        .collect::<Vec<_>>();
    let omitted_hits = ranked
        .iter()
        .skip(limit)
        .filter_map(|hit| entries.iter().find(|entry| entry.key == hit.key))
        .map(|entry| entry.name.clone())
        .collect::<Vec<_>>();
    let selected_builtin_names = selected_keys
        .iter()
        .filter_map(|key| {
            entries
                .iter()
                .find(|entry| &entry.key == key && entry.kind == "builtin")
                .map(|entry| entry.name.clone())
        })
        .collect::<Vec<_>>();
    let closure = prerequisite_closure(
        &selected_builtin_names,
        &available_builtins,
        crate::tooling::search::MAX_PREREQUISITE_ADDITIONS,
    );
    let mut selected = selected_keys
        .iter()
        .filter_map(|key| entries.iter().position(|entry| &entry.key == key))
        .collect::<Vec<_>>();
    let mut prerequisite_names = std::collections::BTreeSet::new();
    for addition in &closure.additions {
        if let Some(index) = entries
            .iter()
            .position(|entry| entry.name == addition.name && entry.kind == "builtin")
        {
            prerequisite_names.insert(entries[index].name.clone());
            if !selected.contains(&index) {
                selected.push(index);
            }
        }
    }
    // MCP calls require the generic `mcp` bridge. Include it in the returned
    // set when available, or report it missing; the MCP tool itself is not
    // promoted as a builtin schema.
    let selected_mcp = selected.iter().any(|index| entries[*index].kind == "mcp");
    if selected_mcp
        && let Some(index) = entries
            .iter()
            .position(|entry| entry.name == "mcp" && entry.kind == "builtin")
    {
        prerequisite_names.insert(entries[index].name.clone());
        if !selected.contains(&index) {
            selected.push(index);
        }
    }
    let promoted_candidates = if promote {
        selected
            .iter()
            .filter_map(|index| {
                let entry = &entries[*index];
                (entry.kind == "builtin" && entry.callable && !entry.in_bundle)
                    .then(|| entry.name.clone())
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let mut missing = closure.missing;
    if selected_mcp && !available_builtins.contains("mcp") {
        missing.push(MissingPrerequisite {
            name: "mcp".to_string(),
            reason: "not_available_for_role".to_string(),
            required_by: selected
                .iter()
                .filter(|index| entries[**index].kind == "mcp")
                .map(|index| entries[*index].name.clone())
                .take(8)
                .collect(),
        });
    }
    for index in selected
        .iter()
        .filter(|index| entries[**index].kind == "mcp")
    {
        let entry = &entries[*index];
        if !entry.callable {
            missing.push(MissingPrerequisite {
                name: entry.name.clone(),
                reason: "mcp_pin_untrusted".to_string(),
                required_by: vec![entry.name.clone()],
            });
        }
    }
    for name in omitted_hits {
        if missing.iter().any(|entry| entry.name == name) {
            continue;
        }
        missing.push(MissingPrerequisite {
            name,
            reason: "result_limit".to_string(),
            required_by: vec!["query".to_string()],
        });
    }
    if ranked_truncated && ranked.len() >= 100 {
        missing.push(MissingPrerequisite {
            name: "search_result_cap".to_string(),
            reason: "result_cap".to_string(),
            required_by: vec!["query".to_string()],
        });
    }
    missing.sort_by(|left, right| left.name.cmp(&right.name));
    missing.dedup_by(|left, right| left.name == right.name);
    missing.truncate(crate::tooling::search::MAX_MISSING_PREREQUISITES);

    let hit_scores = ranked
        .iter()
        .map(|hit| (hit.key.clone(), hit.score))
        .collect::<HashMap<_, _>>();
    let mut results = Vec::new();
    for index in selected {
        let entry = &entries[index];
        let prerequisite = prerequisite_names.contains(&entry.name);
        let mut value = json!({
            "name": entry.name,
            "kind": entry.kind,
            "description": entry.description.chars().take(400).collect::<String>(),
            "callable": entry.callable,
            "in_bundle": entry.in_bundle,
            "bundle": frozen_bundle.selection.kind.as_str(),
            "category": entry.category,
            "score": hit_scores.get(&entry.key).copied().unwrap_or(0.0),
            "prerequisite": prerequisite,
        });
        if let Some(server_id) = &entry.server_id {
            value["server_id"] = json!(server_id);
            value["server_name"] = json!(entry.server_name);
            value["tool_name"] = json!(entry.tool_name);
            value["trust"] = json!(entry.trust);
            value["pinned"] = json!(entry.pinned);
        }
        if prerequisite
            && let Some(addition) = closure
                .additions
                .iter()
                .find(|addition| addition.name == entry.name)
        {
            value["required_by"] = json!(addition.required_by);
        }
        if include_schema && let Some(schema) = &entry.schema {
            value["parameters_preview"] = json!(bounded_schema(schema));
        }
        results.push(value);
    }
    results.sort_by(|left, right| {
        right["score"]
            .as_f64()
            .partial_cmp(&left["score"].as_f64())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                left["prerequisite"]
                    .as_bool()
                    .cmp(&right["prerequisite"].as_bool())
            })
            .then_with(|| left["kind"].as_str().cmp(&right["kind"].as_str()))
            .then_with(|| left["name"].as_str().cmp(&right["name"].as_str()))
    });
    let final_names = results
        .iter()
        .filter_map(|result| result["name"].as_str())
        .collect::<Vec<_>>();
    let selected_count = final_names.len();
    let estimate = if selected_count == 0 && !query.is_empty() {
        0.0
    } else {
        completeness_estimate(selected_count, missing.len())
    };
    let rendered = results
        .iter()
        .map(|result| {
            format!(
                "{} · {}{}{}{} · {}",
                result["name"].as_str().unwrap_or("?"),
                result["kind"].as_str().unwrap_or("tool"),
                if result["kind"] == "mcp" {
                    result["server_id"]
                        .as_str()
                        .map(|server| format!(" ({server})"))
                        .unwrap_or_default()
                } else {
                    String::new()
                },
                if result["prerequisite"].as_bool().unwrap_or(false) {
                    " [prerequisite]"
                } else {
                    ""
                },
                if result["in_bundle"].as_bool().unwrap_or(false) {
                    ""
                } else {
                    " [outside frozen bundle; callable if role policy allows]"
                },
                result["description"].as_str().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mcp_hint = if results.iter().any(|result| result["kind"] == "mcp") {
        if mcp_callable {
            "\nCall an MCP result with mcp action=call server=<server_id> tool=<tool_name> arguments={...}."
        } else {
            "\nThis role can inspect MCP tools but cannot call them."
        }
    } else {
        ""
    };
    let promotion = if promote {
        json!({
            "requested": true,
            "candidates": promoted_candidates,
            "note": "Promoted schemas are applied only at the next turn boundary; the current request keeps its frozen tool set.",
        })
    } else {
        json!({"requested": false})
    };
    Ok(ToolOutput::ok_structured(
        truncate(&format!("{rendered}{mcp_hint}")),
        json!({
            "query": query,
            "task_kind": requested_task_kind,
            "tools": results,
            "set": final_names,
            "count": selected_count,
            "missing": missing,
            "completeness_estimate": estimate,
            "truncated": ranked_truncated,
            "prerequisite_additions": closure.additions,
            "mcp_callable": mcp_callable,
            "bundle": frozen_bundle.selection.kind.as_str(),
            "bundle_hash": frozen_bundle.bundle_hash,
            "advertised_count": advertised.len(),
            "outside_bundle": results.iter().filter(|result| !result["in_bundle"].as_bool().unwrap_or(false)).count(),
            "promotion": promotion,
        }),
    ))
}

pub(super) fn bounded_schema(value: &Value) -> String {
    serde_json::to_string(value)
        .unwrap_or_default()
        .chars()
        .take(800)
        .collect()
}

pub(super) async fn run_publish_artifact_file(
    engine: &Engine,
    session: &Session,
    spec: &AgentSpec,
    input: Value,
) -> Result<ToolOutput> {
    let relative = input
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("path is required"))?;
    let absolute = crate::tools::safe_existing_path(&spec.worktree, relative)?;
    if !absolute.is_file() {
        return Ok(ToolOutput::error_structured(
            "artifact source must be a file",
            json!({"code": "INVALID_ARGUMENT", "path": relative}),
        ));
    }
    let name = input
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            absolute
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| "artifact".to_string());
    let kind = input.get("kind").and_then(Value::as_str).unwrap_or("file");
    let mime_type = input
        .get("mime_type")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| crate::tooling::files::mime_for_path(&absolute).to_string());
    let artifact = engine
        .publish_artifact_file(
            &session.id,
            &spec.id,
            &spec.name,
            spec.task_id.clone(),
            &name,
            kind,
            &mime_type,
            &absolute,
            &spec.worktree,
        )
        .await?;
    let structured = serde_json::to_value(&artifact).unwrap_or_else(|_| json!({}));
    Ok(ToolOutput::ok_structured(
        format!(
            "published {} ({}) as {} · {} bytes",
            name, mime_type, artifact.id, artifact.size_bytes
        ),
        structured,
    ))
}

/// Convert a typed archive/media engine failure into the shared tool failure
/// contract. Unknown errors go through the ordinary anyhow classifier.
pub(super) fn archive_media_failure(tool: &str, error: &anyhow::Error) -> ToolOutput {
    if let Some(failure) = error.downcast_ref::<crate::archive_media::ArchiveMediaError>() {
        let mut failure = failure.failure.clone();
        if failure.tool.is_none() {
            failure = failure.with_tool(tool);
        }
        return ToolOutput::error_failure(failure);
    }
    ToolOutput::from_tool_error(tool, error)
}

pub(super) async fn run_archive_tool(engine: &Engine, name: &str, input: Value) -> ToolOutput {
    let artifact_id = match input
        .get("artifact_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) => value.to_string(),
        None => {
            return ToolOutput::error_coded(
                name,
                crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
                "artifact_id is required",
            );
        }
    };
    match name {
        "archive_list" => {
            let max_entries = input
                .get("max_entries")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok());
            let params = vp_protocol::types::ArtifactArchiveListParams {
                artifact_id,
                max_entries,
            };
            match engine.artifact_archive_list(params).await {
                Ok(result) => {
                    let mut text = format!(
                        "{} · {} · {} entr{} · {} bytes uncompressed{}",
                        result.name,
                        result.format,
                        result.entry_count,
                        if result.entry_count == 1 { "y" } else { "ies" },
                        result.total_uncompressed_bytes,
                        if result.truncated {
                            " · listing truncated at the entry cap"
                        } else {
                            ""
                        }
                    );
                    for entry in result.entries.iter().take(60) {
                        text.push('\n');
                        text.push_str(&format!(
                            "{}  {}  {} bytes{}",
                            if entry.safe { "safe" } else { "UNSAFE" },
                            entry.path,
                            entry.size_bytes,
                            entry
                                .unsafe_reason
                                .as_ref()
                                .map(|reason| format!(" · {reason}"))
                                .unwrap_or_default()
                        ));
                    }
                    if result.entries.len() > 60 {
                        text.push_str(
                            "\n… more entries omitted; use the RPC for the full bounded list",
                        );
                    }
                    ToolOutput::ok_structured(
                        text,
                        serde_json::to_value(&result).unwrap_or_else(|_| json!({})),
                    )
                }
                Err(error) => archive_media_failure(name, &error),
            }
        }
        "archive_extract" => {
            let entries = input
                .get("entries")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let all = input.get("all").and_then(Value::as_bool).unwrap_or(false);
            let params = vp_protocol::types::ArtifactArchiveExtractParams {
                artifact_id,
                entries,
                all,
                max_entries: input
                    .get("max_entries")
                    .and_then(Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok()),
                include_previews: Some(
                    input
                        .get("include_previews")
                        .and_then(Value::as_bool)
                        .unwrap_or(true),
                ),
            };
            match engine.artifact_archive_extract(params).await {
                Ok(result) => {
                    let mut text = format!(
                        "extracted {} entr{} ({} bytes) from {} · {} published artifact(s); daemon scratch removed",
                        result.entries.len(),
                        if result.entries.len() == 1 {
                            "y"
                        } else {
                            "ies"
                        },
                        result.total_bytes,
                        result.format,
                        result.published_artifacts.len()
                    );
                    for entry in &result.entries {
                        text.push('\n');
                        text.push_str(&format!(
                            "{}  {} bytes{}",
                            entry.path,
                            entry.size_bytes,
                            entry
                                .published_artifact_id
                                .as_ref()
                                .map(|id| format!(" · artifact {id}"))
                                .or_else(|| {
                                    entry
                                        .skipped_reason
                                        .as_ref()
                                        .map(|reason| format!(" · skipped: {reason}"))
                                })
                                .unwrap_or_default()
                        ));
                    }
                    ToolOutput::ok_structured(
                        text,
                        serde_json::to_value(&result).unwrap_or_else(|_| json!({})),
                    )
                }
                Err(error) => archive_media_failure(name, &error),
            }
        }
        other => ToolOutput::error_coded(
            other,
            crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
            "unknown archive action",
        ),
    }
}

pub(super) async fn run_media_tool(
    engine: &Engine,
    spec: &AgentSpec,
    name: &str,
    input: Value,
) -> ToolOutput {
    match name {
        "media_info" => {
            if let Some(artifact_id) = input
                .get("artifact_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                let params = vp_protocol::types::MediaInfoParams {
                    artifact_id: artifact_id.to_string(),
                };
                return match engine.media_info(params).await {
                    Ok(result) => {
                        let summary = media_info_summary(&result.name, &result.info, 0);
                        ToolOutput::ok_structured(
                            summary,
                            serde_json::to_value(&result).unwrap_or_else(|_| json!({})),
                        )
                    }
                    Err(error) => archive_media_failure(name, &error),
                };
            }
            let Some(relative) = input.get("path").and_then(Value::as_str) else {
                return ToolOutput::error_coded(
                    "media_info",
                    crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
                    "media_info requires artifact_id or a worktree-relative path",
                );
            };
            let path = match crate::tools::safe_existing_path(&spec.worktree, relative) {
                Ok(path) => path,
                Err(error) => {
                    return ToolOutput::error_failure(error.with_tool("media_info"));
                }
            };
            let tools = crate::tooling::media::MediaTools::discover();
            let path_for_task = path.clone();
            let info = match tokio::task::spawn_blocking(move || {
                crate::tooling::media::probe_media(
                    &path_for_task,
                    &tools,
                    std::time::Duration::from_secs(
                        crate::tooling::media::DEFAULT_FFPROBE_TIMEOUT_SECS,
                    ),
                )
            })
            .await
            {
                Ok(Ok(info)) => info,
                Ok(Err(error)) => {
                    let typed =
                        anyhow::Error::new(crate::archive_media::ArchiveMediaError::from(error));
                    return archive_media_failure(name, &typed);
                }
                Err(error) => {
                    return ToolOutput::error_coded(
                        "media_info",
                        crate::tooling::errors::ToolErrorCode::INTERNAL_ERROR,
                        format!("media probe task failed: {error}"),
                    );
                }
            };
            let size_bytes = std::fs::metadata(&path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            let summary = media_info_summary(relative, &info, size_bytes);
            ToolOutput::ok_structured(
                summary,
                json!({
                    "path": relative,
                    "size_bytes": size_bytes,
                    "info": info,
                }),
            )
        }
        "media_transcode" => {
            let Some(artifact_id) = input
                .get("artifact_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                return ToolOutput::error_coded(
                    "media_transcode",
                    crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
                    "artifact_id is required",
                );
            };
            let Some(format) = input.get("format").and_then(Value::as_str) else {
                return ToolOutput::error_coded(
                    "media_transcode",
                    crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
                    "format is required",
                );
            };
            let params = vp_protocol::types::MediaTranscodeParams {
                artifact_id: artifact_id.to_string(),
                format: format.to_string(),
                max_duration_secs: input.get("max_duration_secs").and_then(Value::as_u64),
                max_bytes: input.get("max_bytes").and_then(Value::as_u64),
                // The approval broker has already approved this specific call;
                // the RPC surface still requires an explicit operator marker.
                approved: true,
            };
            match engine.media_transcode(params).await {
                Ok(result) => ToolOutput::ok_structured(
                    format!(
                        "transcoded to {} · artifact {} · {} bytes · {}s cap{}",
                        result.format,
                        result.artifact.id,
                        result.size_bytes,
                        result.max_duration_secs,
                        if result.warnings.is_empty() {
                            String::new()
                        } else {
                            format!(" · {}", result.warnings.join("; "))
                        }
                    ),
                    serde_json::to_value(&result).unwrap_or_else(|_| json!({})),
                ),
                Err(error) => archive_media_failure(name, &error),
            }
        }
        other => ToolOutput::error_coded(
            other,
            crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
            "unknown media action",
        ),
    }
}

pub(super) fn media_info_summary(
    label: &str,
    info: &vp_protocol::types::MediaInfo,
    size_bytes: u64,
) -> String {
    let mut text = format!(
        "{} · {} · source {}",
        label,
        info.container.as_deref().unwrap_or("unknown container"),
        info.source
    );
    if let Some(duration) = info.duration_secs {
        text.push_str(&format!(" · {duration:.2}s"));
    }
    if let (Some(width), Some(height)) = (info.width, info.height) {
        text.push_str(&format!(" · {width}x{height}"));
    }
    if size_bytes > 0 {
        text.push_str(&format!(" · {size_bytes} bytes"));
    }
    for stream in info.streams.iter().take(8) {
        text.push_str(&format!(
            "\nstream {}: {} {}{}{}",
            stream.index,
            stream.codec_type,
            stream.codec_name,
            stream
                .width
                .zip(stream.height)
                .map(|(width, height)| format!(" {width}x{height}"))
                .unwrap_or_default(),
            stream
                .duration_secs
                .map(|duration| format!(" {duration:.2}s"))
                .unwrap_or_default()
        ));
    }
    for warning in &info.warnings {
        text.push_str(&format!("\nwarning: {warning}"));
    }
    text
}

/// Resolve the pin trust for a generic `mcp action=call` request. A missing
/// server/tool is `Unknown` so the contract escalates rather than allowing.
pub(super) async fn resolve_mcp_pin_trust(
    engine: &Engine,
    project_id: &str,
    input: &Value,
) -> crate::contract::McpPinTrust {
    let Some(selector) = input.get("server").and_then(Value::as_str) else {
        return crate::contract::McpPinTrust::Unknown;
    };
    let Some(tool) = input.get("tool").and_then(Value::as_str) else {
        return crate::contract::McpPinTrust::Unknown;
    };
    let server = match engine.find_mcp_server(selector).await {
        Ok(Some(server)) => server,
        Ok(None) | Err(_) => return crate::contract::McpPinTrust::Unknown,
    };
    engine
        .mcp_pin_trust_for_tool(project_id, &server.id, tool)
        .await
        .unwrap_or(crate::contract::McpPinTrust::Unknown)
}

#[allow(clippy::large_enum_variant)]
pub(super) async fn run_progress_report(
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

#[allow(clippy::result_large_err)] // the typed failure carries the stable contract
pub(super) async fn run_mcp_tool(
    engine: &Engine,
    session: &Session,
    spec: &AgentSpec,
    input: serde_json::Value,
    effect_receipt_id: Option<&str>,
) -> Result<ToolOutput> {
    let role = spec.role;
    let project_id = session.project_id.as_str();
    let action = input
        .get("action")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("action is required"))?;
    if action == "call"
        && !matches!(
            role,
            AgentRole::Leader | AgentRole::Worker | AgentRole::Labour
        )
    {
        return Ok(ToolOutput::error_coded(
            "mcp",
            crate::tooling::errors::ToolErrorCode::PERMISSION_DENIED,
            "this role may inspect MCP servers but cannot execute MCP tools",
        ));
    }
    let servers = engine.list_mcp_servers().await?.servers;
    let server = input
        .get("server")
        .and_then(serde_json::Value::as_str)
        .map(|selector| {
            servers
                .iter()
                .find(|server| server.id == selector || server.name == selector)
                .cloned()
                .ok_or_else(|| {
                    crate::tooling::errors::ToolFailure::new(
                        crate::tooling::errors::ToolErrorCode::MCP_SERVER_NOT_FOUND,
                        format!("MCP server not found: {selector}"),
                    )
                    .with_tool("mcp")
                    .with_evidence(format!("server={selector}"))
                })
        })
        .transpose()?;
    match action {
        "list" => {
            if let Some(server) = server {
                let tools = engine.mcp_tools(&server.id).await?.tools;
                let mut text = format!(
                    "MCP server {} ({}):
",
                    server.name, server.id
                );
                for tool in tools {
                    text.push_str(&format!(
                        "- {}: {}
",
                        tool.name,
                        truncate_to(&tool.description, 300)
                    ));
                }
                if text.ends_with(":\n") {
                    text.push_str("(no tools)");
                }
                Ok(ToolOutput::ok(text))
            } else {
                let mut text = String::from(
                    "MCP servers:
",
                );
                for server in servers {
                    text.push_str(&format!(
                        "- {} ({}) [{}] {}
",
                        server.name,
                        server.id,
                        if server.enabled {
                            "enabled"
                        } else {
                            "disabled"
                        },
                        server.command
                    ));
                }
                Ok(ToolOutput::ok(text))
            }
        }
        "call" => {
            let server = server.ok_or_else(|| anyhow!("server is required for action=call"))?;
            if !server.enabled {
                return Ok(ToolOutput::error_coded(
                    "mcp",
                    crate::tooling::errors::ToolErrorCode::MCP_DISABLED,
                    format!(
                        "MCP server {} is disabled; enable it before calling its tools",
                        server.name
                    ),
                ));
            }
            let tool = input
                .get("tool")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("tool is required for action=call"))?;
            // Pin trust is enforced here, independent of ContractPolicy mode.
            // The generic `mcp` tool must not become a bypass for a drifted,
            // unverified, or removed tool.
            let trust = engine
                .mcp_pin_trust_for_tool(project_id, &server.id, tool)
                .await
                .unwrap_or(crate::contract::McpPinTrust::Unknown);
            if trust != crate::contract::McpPinTrust::Trusted {
                return Ok(ToolOutput::error_coded(
                    "mcp",
                    crate::tooling::errors::ToolErrorCode::PERMISSION_DENIED,
                    format!(
                        "MCP tool {}/{} is not approved for execution (trust: {}); approve its current pin first",
                        server.name,
                        tool,
                        trust.label()
                    ),
                ));
            }
            let arguments = input
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            let context = crate::mcp::tasks::McpTaskContext::from_session(
                &session.project_id,
                &session.id,
                &spec.id,
                &spec.name,
                spec.task_id.as_deref(),
                effect_receipt_id,
            );
            let result = engine
                .mcp_call_with_context(&server.id, tool, arguments, context)
                .await?;
            let mut text = format_mcp_tool_result(&result.result);
            if let Some(task) = &result.task {
                text.push_str(&format!(
                    "\nAsync MCP task {} is {}. Inspect explicitly with mcp action=task_get task_id={} or task_result.",
                    task.id, task.status, task.id
                ));
            }
            Ok(ToolOutput::ok(truncate_to(&text, 16_000)))
        }
        "task_list" => {
            let tasks = engine
                .mcp_task_list(vp_protocol::types::McpTaskListParams {
                    project_id: Some(session.project_id.clone()),
                    session_id: Some(session.id.clone()),
                    server_id: server.map(|server| server.id),
                    status: input
                        .get("status")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    limit: input
                        .get("limit")
                        .and_then(serde_json::Value::as_u64)
                        .map(|limit| limit.clamp(1, 500) as u32),
                })
                .await?;
            let mut text = String::new();
            for task in &tasks.tasks {
                text.push_str(&format!(
                    "- {} {}/{} [{}] poll={}ms expires={}\n",
                    task.id,
                    task.server_name,
                    task.tool_name,
                    task.status,
                    task.poll_interval_ms,
                    task.expires_at.as_deref().unwrap_or("none")
                ));
            }
            if text.is_empty() {
                text.push_str("(no MCP async tasks)");
            }
            if tasks.truncated {
                text.push_str("\n(list truncated)");
            }
            Ok(ToolOutput::ok_structured(
                text,
                serde_json::json!({"tasks": tasks.tasks, "truncated": tasks.truncated}),
            ))
        }
        "task_get" | "task_result" => {
            let task_id = input
                .get("task_id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("task_id is required for action={action}"))?;
            if action == "task_get" {
                let task = engine.mcp_task_get(task_id, true).await?;
                Ok(ToolOutput::ok_structured(
                    format!(
                        "MCP task {} {}/{} [{}] poll={}ms",
                        task.id,
                        task.server_name,
                        task.tool_name,
                        task.status,
                        task.poll_interval_ms
                    ),
                    serde_json::json!(task),
                ))
            } else {
                let result = engine
                    .mcp_task_result(vp_protocol::types::McpTaskResultParams {
                        task_id: task_id.to_string(),
                        refresh: input.get("refresh").and_then(serde_json::Value::as_bool),
                    })
                    .await?;
                let mut text = format!(
                    "MCP task {} {}/{} [{}]",
                    result.task.id,
                    result.task.server_name,
                    result.task.tool_name,
                    result.task.status
                );
                if result.pending {
                    text.push_str(&format!(
                        "\nStill pending; poll explicitly after {} ms.",
                        result.task.poll_interval_ms
                    ));
                }
                if let Some(value) = &result.result {
                    text.push('\n');
                    text.push_str(&format_mcp_tool_result(value));
                }
                Ok(ToolOutput::ok_structured(
                    truncate_to(&text, 16_000),
                    serde_json::json!(result),
                ))
            }
        }
        "task_cancel" => {
            if !matches!(
                role,
                AgentRole::Leader | AgentRole::Manager | AgentRole::Worker | AgentRole::Labour
            ) {
                return Ok(ToolOutput::error_coded(
                    "mcp",
                    crate::tooling::errors::ToolErrorCode::PERMISSION_DENIED,
                    "this role may inspect MCP tasks but cannot cancel them",
                ));
            }
            let task_id = input
                .get("task_id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("task_id is required for action=task_cancel"))?;
            let result = engine
                .mcp_task_cancel(
                    task_id,
                    input.get("reason").and_then(serde_json::Value::as_str),
                )
                .await?;
            Ok(ToolOutput::ok_structured(
                format!(
                    "MCP task {} [{}]: {}",
                    result.task.id, result.task.status, result.message
                ),
                serde_json::json!(result),
            ))
        }
        "resources" => {
            let server =
                server.ok_or_else(|| anyhow!("server is required for action=resources"))?;
            let result = engine.mcp_resources(&server.id).await?;
            let mut text = format!("MCP resources on {}:\n", server.name);
            for resource in &result.resources {
                text.push_str(&format!("- {} <{}>", resource.name, resource.uri));
                if let Some(mime_type) = &resource.mime_type {
                    text.push_str(&format!(" [{mime_type}]"));
                }
                if !resource.description.is_empty() {
                    text.push_str(&format!(": {}", truncate_to(&resource.description, 200)));
                }
                text.push('\n');
            }
            for template in &result.templates {
                text.push_str(&format!(
                    "- template {} <{}>: {}\n",
                    template.name,
                    template.uri_template,
                    truncate_to(&template.description, 200)
                ));
            }
            if result.resources.is_empty() && result.templates.is_empty() {
                text.push_str("(no resources)");
            }
            Ok(ToolOutput::ok(text))
        }
        "read_resource" => {
            let server =
                server.ok_or_else(|| anyhow!("server is required for action=read_resource"))?;
            let uri = input
                .get("uri")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("uri is required for action=read_resource"))?;
            let result = engine.mcp_read_resource(&server.id, uri).await?;
            let mut text = String::new();
            for content in result.contents {
                if let Some(body) = content.text {
                    text.push_str(&body);
                    text.push('\n');
                } else if let Some(blob) = content.blob {
                    text.push_str(&format!(
                        "[binary resource {}: {} base64 chars{}]\n",
                        content.uri,
                        blob.len(),
                        content
                            .mime_type
                            .map(|mime| format!(", {mime}"))
                            .unwrap_or_default()
                    ));
                }
            }
            if text.is_empty() {
                text.push_str("(empty resource)");
            }
            Ok(ToolOutput::ok(truncate_to(&text, 16_000)))
        }
        "prompts" => {
            let server = server.ok_or_else(|| anyhow!("server is required for action=prompts"))?;
            let result = engine.mcp_prompts(&server.id).await?;
            let mut text = format!("MCP prompts on {}:\n", server.name);
            for prompt in result.prompts {
                text.push_str(&format!(
                    "- {}: {}",
                    prompt.name,
                    truncate_to(&prompt.description, 200)
                ));
                if !prompt.arguments.is_empty() {
                    let arguments = prompt
                        .arguments
                        .iter()
                        .map(|argument| {
                            if argument.required {
                                format!("{}=?", argument.name)
                            } else {
                                format!("{}?", argument.name)
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    text.push_str(&format!(" ({arguments})"));
                }
                text.push('\n');
            }
            if text.ends_with(":\n") {
                text.push_str("(no prompts)");
            }
            Ok(ToolOutput::ok(text))
        }
        "get_prompt" => {
            let server =
                server.ok_or_else(|| anyhow!("server is required for action=get_prompt"))?;
            let prompt = input
                .get("prompt")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("prompt is required for action=get_prompt"))?;
            let arguments = input
                .get("prompt_arguments")
                .and_then(serde_json::Value::as_object)
                .map(|arguments| {
                    arguments
                        .iter()
                        .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
                        .collect()
                })
                .unwrap_or_default();
            let result = engine.mcp_prompt(&server.id, prompt, arguments).await?;
            let mut text = String::new();
            if !result.description.is_empty() {
                text.push_str(&result.description);
                text.push_str("\n\n");
            }
            for message in result.messages {
                text.push_str(&format!("[{}] ", message.role));
                text.push_str(&truncate_to(
                    &serde_json::to_string(&message.content)?,
                    4_000,
                ));
                text.push('\n');
            }
            Ok(ToolOutput::ok(truncate_to(&text, 16_000)))
        }
        other => Ok(ToolOutput::error_coded(
            "mcp",
            crate::tooling::errors::ToolErrorCode::INVALID_ACTION,
            format!("unknown MCP action: {other}"),
        )
        .with_failure_action(
            "use action list, call, resources, read_resource, prompts, or get_prompt",
        )),
    }
}

pub(super) fn skill_allows_tool(skill: &Skill, tool: &str) -> bool {
    if tool == "use_skill" {
        return true;
    }
    if skill
        .tool_deny
        .iter()
        .any(|denied| denied == "*" || denied == tool)
    {
        return false;
    }
    skill.tool_allow.is_empty()
        || skill
            .tool_allow
            .iter()
            .any(|allowed| allowed == "*" || allowed == tool)
}

pub(super) async fn run_skill_tool(
    engine: &Engine,
    project: &Project,
    spec: &AgentSpec,
    input: Value,
    active: &mut Option<Skill>,
) -> Result<ToolOutput> {
    let name = input
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("name is required"))?;
    let skills = engine
        .skills_for(&project.id, spec.role.as_str(), spec.profile.as_str())
        .await?;
    let Some(skill) = skills.iter().find(|skill| {
        skill.name.eq_ignore_ascii_case(name) || skill.slug.eq_ignore_ascii_case(name)
    }) else {
        let available = skills
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Ok(ToolOutput::error_coded(
            "use_skill",
            crate::tooling::errors::ToolErrorCode::SKILL_FAILED,
            if available.is_empty() {
                "No enabled skills apply to this role/profile.".to_string()
            } else {
                format!("Unknown skill '{name}'. Available skills: {available}")
            },
        ));
    };
    *active = Some(skill.clone());
    let allow = if skill.tool_allow.is_empty() {
        "unrestricted by this skill".to_string()
    } else {
        skill.tool_allow.join(", ")
    };
    let deny = if skill.tool_deny.is_empty() {
        "none".to_string()
    } else {
        skill.tool_deny.join(", ")
    };
    Ok(ToolOutput::ok(truncate_to(
        &format!(
            "Skill '{}' v{} activated.\nAllowed tools: {allow}\nDenied tools: {deny}\n\n{}\n\nFollow these instructions for the rest of this run. Use_skill always remains available to switch skills.",
            skill.name, skill.version, skill.instructions
        ),
        22_000,
    )))
}

pub(super) async fn run_source_tool(
    engine: &Engine,
    project: &Project,
    name: &str,
    input: Value,
    citations: &mut CitationRegistry,
) -> Result<ToolOutput> {
    match name {
        "search_sources" => {
            let query = input
                .get("query")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("query is required"))?;
            let source_ids = input
                .get("source_ids")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .take(20)
                        .map(ToOwned::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let collections = input
                .get("collections")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .take(20)
                        .map(ToOwned::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let labels = input
                .get("labels")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .take(20)
                        .map(ToOwned::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let limit = input
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(6)
                .clamp(1, 20) as u32;
            let mode = input
                .get("mode")
                .and_then(Value::as_str)
                .map(str::parse::<vp_protocol::types::SourceSearchMode>)
                .transpose()
                .map_err(|error| anyhow!("{error}"))?;
            let result = engine
                .search_sources_with_mode(
                    project.id.clone(),
                    query.to_string(),
                    source_ids,
                    collections,
                    labels,
                    Some(limit),
                    mode,
                )
                .await?;
            if result.hits.is_empty() {
                let reason = result
                    .semantic_unavailable
                    .as_deref()
                    .map(|reason| format!(" Semantic retrieval was unavailable ({reason})."))
                    .unwrap_or_default();
                return Ok(ToolOutput::ok(format!(
                    "No matching source chunks. Try a shorter query or list_sources.{reason}"
                )));
            }
            let mut output = String::new();
            for hit in &result.hits {
                let key = citations.add_hit(hit, "search");
                let heading = hit.heading.as_deref().unwrap_or("untitled section");
                output.push_str(&format!(
                    "[{key}] {} — {} (chunk {}, score {:.2})\n{}\n\n",
                    hit.source_title,
                    heading,
                    hit.chunk_no,
                    hit.score,
                    truncate_to(&hit.content, 1_400),
                ));
            }
            if let Some(reason) = &result.semantic_unavailable {
                output.push_str(&format!(
                    "Semantic retrieval was unavailable ({reason}); results are lexical.\n"
                ));
            } else if result.mode != "lexical" {
                output.push_str(&format!(
                    "Retrieval mode: {} (each chunk is labeled with its origin).\n",
                    result.mode
                ));
            }
            if result.degraded {
                output.push_str(
                    "Search used the lexical fallback because the FTS index had no direct match.\n",
                );
            }
            Ok(ToolOutput::ok(truncate_to(&output, 24_000)))
        }
        "read_source" => {
            let source_id = input
                .get("source_id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("source_id is required"))?;
            let chunk_no = input
                .get("chunk_no")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .clamp(1, 500) as u32;
            let max_chars = input
                .get("max_chars")
                .and_then(Value::as_u64)
                .unwrap_or(6_000)
                .clamp(300, 20_000) as usize;
            let result = engine
                .get_source(source_id.to_string(), Some(chunk_no - 1), Some(1))
                .await?;
            let Some(chunk) = result.chunks.first() else {
                return Ok(ToolOutput::error(format!(
                    "source {source_id} has no chunk {chunk_no}"
                )));
            };
            let hit = SourceSearchHit {
                source_id: result.source.id.clone(),
                source_title: result.source.title.clone(),
                collection: result.source.collection.clone(),
                labels: result.source.labels.clone(),
                chunk_id: chunk.id.clone(),
                chunk_no: chunk.chunk_no,
                heading: chunk.heading.clone(),
                content: chunk.content.clone(),
                score: 1.0,
                source: "lexical".to_string(),
                confidence: 1.0,
                origins: vec!["read".to_string()],
                lexical_score: Some(1.0),
                semantic_score: None,
                source_updated_at: Some(result.source.updated_at.clone()),
                ..SourceSearchHit::default()
            };
            let key = citations.add_hit(&hit, "read");
            let heading = chunk.heading.as_deref().unwrap_or("untitled section");
            Ok(ToolOutput::ok(format!(
                "[{key}] {} — {} (chunk {} of {})\n{}",
                result.source.title,
                heading,
                chunk.chunk_no,
                result.total_chunks,
                truncate_to(&chunk.content, max_chars),
            )))
        }
        "list_sources" => {
            let limit = input
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(20)
                .clamp(1, 50) as u32;
            let result = engine
                .list_sources(project.id.clone(), None, None, Some(limit))
                .await?;
            if result.sources.is_empty() {
                return Ok(ToolOutput::ok("No project sources are indexed."));
            }
            let mut output = String::new();
            for source in result.sources {
                output.push_str(&format!(
                    "- {} · {} · {} chunks · status={}\n  id={}\n",
                    source.title, source.kind, source.chunk_count, source.status, source.id
                ));
            }
            Ok(ToolOutput::ok(truncate_to(&output, 12_000)))
        }
        other => Ok(ToolOutput::error_coded(
            "sources",
            crate::tooling::errors::ToolErrorCode::INVALID_ACTION,
            format!("unknown source tool: {other}"),
        )),
    }
}

pub(super) async fn run_read_tool_output(
    engine: &Engine,
    session: &Session,
    input: serde_json::Value,
) -> ToolOutput {
    let params = match crate::tool_output::ReadToolOutputParams::from_input(&input) {
        Ok(params) => params,
        Err(error) => {
            return ToolOutput::error_coded(
                "read_tool_output",
                crate::tooling::errors::ToolErrorCode::TOOL_OUTPUT_INVALID,
                format!("read_tool_output: {error}"),
            );
        }
    };
    let store = crate::tool_output::ToolOutputStore::new(engine.store().clone());
    let session_id = session.id.clone();
    let result = tokio::task::spawn_blocking(move || {
        crate::tool_output::page_tool_output(&store, &session_id, &params)
    })
    .await;
    match result {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            let message = error.to_string();
            let lower = message.to_ascii_lowercase();
            let code = if lower.contains("expired") {
                crate::tooling::errors::ToolErrorCode::TOOL_OUTPUT_EXPIRED
            } else if lower.contains("not found") || lower.contains("unknown") {
                crate::tooling::errors::ToolErrorCode::TOOL_OUTPUT_NOT_FOUND
            } else {
                crate::tooling::errors::ToolErrorCode::TOOL_OUTPUT_INVALID
            };
            ToolOutput::error_coded(
                "read_tool_output",
                code,
                format!("read_tool_output: {message}"),
            )
        }
        Err(error) => ToolOutput::error_coded(
            "read_tool_output",
            crate::tooling::errors::ToolErrorCode::INTERNAL_ERROR,
            format!("read_tool_output task failed: {error}"),
        ),
    }
}

pub(super) async fn run_memory_tool(
    engine: &Engine,
    session: &Session,
    project: &Project,
    spec: &AgentSpec,
    name: &str,
    input: serde_json::Value,
) -> Result<ToolOutput> {
    match name {
        "remember" => {
            if matches!(spec.role, AgentRole::Verifier | AgentRole::Scout) {
                return Ok(ToolOutput::error(format!(
                    "{} cannot write project memory",
                    spec.role.default_name()
                )));
            }
            let kind = input
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("kind is required"))?;
            let title = input
                .get("title")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("title is required"))?;
            let body = input
                .get("body")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("body is required"))?;
            let tags = input
                .get("tags")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>();
            let scope = match tool_memory_scope(&input) {
                Ok(scope) => scope,
                Err(output) => return Ok(output),
            };
            let Some(scope) = scope else {
                // Omitted scope is the legacy project-memory path, unchanged.
                let memory = engine
                    .create_memory_as(&session.id, &spec.id, &spec.name, kind, title, body, tags)
                    .await?;
                return Ok(ToolOutput::ok(format!(
                    "remembered [{}] {} ({})",
                    memory.kind, memory.title, memory.id
                )));
            };
            let identity = match active_memory_identity(engine, session, spec).await {
                Ok(identity) => identity,
                Err(error) => return Ok(memory_tool_failure("remember", &error)),
            };
            let (identity_id, department) = match scope {
                "agent" => {
                    let Some(identity) = identity else {
                        return Ok(ToolOutput::error_coded(
                            "remember",
                            crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
                            "error[MEMORY_IDENTITY_REQUIRED] agent scope requires an assigned project identity; omit scope for legacy project memory",
                        ));
                    };
                    (Some(identity.id), None)
                }
                "department" => {
                    let Some(identity) = identity else {
                        return Ok(ToolOutput::error_coded(
                            "remember",
                            crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
                            "error[MEMORY_IDENTITY_REQUIRED] department scope requires an assigned project identity",
                        ));
                    };
                    let Some(department) = identity.department.clone() else {
                        return Ok(ToolOutput::error_coded(
                            "remember",
                            crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
                            "error[MEMORY_DEPARTMENT_REQUIRED] department scope requires an identity with a department",
                        ));
                    };
                    (None, Some(department))
                }
                "project" => (None, None),
                other => {
                    return Ok(ToolOutput::error_coded(
                        "remember",
                        crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
                        format!(
                            "error[MEMORY_SCOPE_INVALID] scope must be agent, department, or project: {other}"
                        ),
                    ));
                }
            };
            let content = format!("{}: {}", title.trim(), body.trim());
            let mut evidence = match tool_memory_evidence(&input, session, spec) {
                Ok(evidence) => evidence,
                Err(output) => return Ok(output),
            };
            if evidence.len() > vp_protocol::types::AGENT_MEMORY_MAX_EVIDENCE {
                evidence.truncate(vp_protocol::types::AGENT_MEMORY_MAX_EVIDENCE);
            }
            let expires_at = input
                .get("expires_at")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let pinned = input
                .get("pinned")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let memory = engine
                .remember_agent_memory(vp_protocol::types::CreateAgentMemoryParams {
                    project_id: project.id.clone(),
                    identity_id,
                    department,
                    scope: scope.to_string(),
                    content,
                    evidence,
                    source: Some("agent".to_string()),
                    expires_at,
                    pinned,
                    ..vp_protocol::types::CreateAgentMemoryParams::default()
                })
                .await;
            match memory {
                Ok(result) => Ok(ToolOutput::ok(format!(
                    "remembered [{}] {} ({})",
                    result.memory.scope,
                    truncate_to(&result.memory.content, 120),
                    result.memory.id
                ))),
                Err(error) => Ok(memory_tool_failure("remember", &error)),
            }
        }
        "recall" => {
            let query = input
                .get("query")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let kind = input
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let limit = input
                .get("limit")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(10)
                .clamp(1, 20) as u32;
            let scope = match tool_memory_scope(&input) {
                Ok(scope) => scope,
                Err(output) => return Ok(output),
            };
            let Some(scope) = scope else {
                let memories = engine
                    .list_memories(&project.id, query, kind, limit)
                    .await?
                    .memories;
                if memories.is_empty() {
                    return Ok(ToolOutput::ok("No matching project memories."));
                }
                let mut text = String::new();
                for memory in memories {
                    text.push_str(&format!(
                        "[{}] {} · {} · {}\n{}\n",
                        memory.kind,
                        memory.title,
                        memory.agent_name,
                        truncate_to(&memory.body, 1200),
                        memory.id
                    ));
                }
                return Ok(ToolOutput::ok(text));
            };
            let identity = match active_memory_identity(engine, session, spec).await {
                Ok(identity) => identity,
                Err(error) => return Ok(memory_tool_failure("recall", &error)),
            };
            let viewer = match scope {
                "agent" | "department" if identity.is_none() => {
                    return Ok(ToolOutput::error_coded(
                        "recall",
                        crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
                        format!(
                            "error[MEMORY_IDENTITY_REQUIRED] {scope} scope requires an assigned project identity"
                        ),
                    ));
                }
                "agent" | "department" | "project" => {
                    identity.as_ref().map(|identity| identity.id.clone())
                }
                other => {
                    return Ok(ToolOutput::error_coded(
                        "recall",
                        crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
                        format!(
                            "error[MEMORY_SCOPE_INVALID] scope must be agent, department, or project: {other}"
                        ),
                    ));
                }
            };
            let result = engine
                .list_agent_memories(vp_protocol::types::AgentMemoryListParams {
                    project_id: project.id.clone(),
                    identity_id: viewer,
                    scope: Some(scope.to_string()),
                    query,
                    limit: Some(limit),
                    ..vp_protocol::types::AgentMemoryListParams::default()
                })
                .await;
            let result = match result {
                Ok(result) => result,
                Err(error) => return Ok(memory_tool_failure("recall", &error)),
            };
            if result.memories.is_empty() {
                return Ok(ToolOutput::ok("No matching identity memories."));
            }
            let mut text = String::new();
            for memory in result.memories {
                let status = if memory.expired {
                    "expired"
                } else {
                    memory.status.as_str()
                };
                let provenance = crate::identity_memory::memory_provenance_label(&memory);
                text.push_str(&format!(
                    "[{}·{}·{}] {}{}\n{}\n",
                    memory.scope,
                    status,
                    memory.source,
                    truncate_to(&memory.content, 1200),
                    if provenance.is_empty() {
                        String::new()
                    } else {
                        format!(" ({provenance})")
                    },
                    memory.id
                ));
            }
            Ok(ToolOutput::ok(text))
        }
        other => Ok(ToolOutput::error_coded(
            "memory",
            crate::tooling::errors::ToolErrorCode::INVALID_ACTION,
            format!("unknown memory tool: {other}"),
        )),
    }
}

/// Parse the optional S5 scope. An absent/blank value means the legacy
/// project-memory path; an unknown non-empty value fails closed instead of
/// silently switching stores.
#[allow(clippy::result_large_err)] // carries the bounded Q3 tool failure
pub(super) fn tool_memory_scope(
    input: &serde_json::Value,
) -> std::result::Result<Option<&str>, ToolOutput> {
    let Some(scope) = input.get("scope").and_then(serde_json::Value::as_str) else {
        return Ok(None);
    };
    let scope = scope.trim();
    if scope.is_empty() {
        return Ok(None);
    }
    if vp_protocol::types::agent_memory_scope(scope).is_none() {
        return Err(ToolOutput::error_coded(
            "memory",
            crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
            format!(
                "error[MEMORY_SCOPE_INVALID] scope must be agent, department, or project: {scope}"
            ),
        ));
    }
    Ok(Some(scope))
}

pub(super) async fn active_memory_identity(
    engine: &Engine,
    session: &Session,
    spec: &AgentSpec,
) -> Result<Option<vp_protocol::types::AgentIdentity>> {
    engine
        .prompt_identity_for_run(
            &session.id,
            spec.role.as_str(),
            spec.title.as_deref(),
            spec.department.as_deref(),
        )
        .await
}

#[allow(clippy::result_large_err)] // carries the bounded Q3 tool failure
pub(super) fn tool_memory_evidence(
    input: &serde_json::Value,
    session: &Session,
    spec: &AgentSpec,
) -> std::result::Result<Vec<vp_protocol::types::MemoryEvidence>, ToolOutput> {
    let mut evidence = Vec::new();
    if let Some(items) = input.get("evidence").and_then(serde_json::Value::as_array) {
        for item in items
            .iter()
            .take(vp_protocol::types::AGENT_MEMORY_MAX_EVIDENCE)
        {
            let kind = item
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let reference = item
                .get("reference")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            if kind.is_empty() || reference.is_empty() {
                return Err(ToolOutput::error_coded(
                    "remember",
                    crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
                    "error[MEMORY_EVIDENCE_INVALID] evidence items require kind and reference",
                ));
            }
            evidence.push(vp_protocol::types::MemoryEvidence {
                kind: kind.to_string(),
                reference: reference.to_string(),
                note: item
                    .get("note")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
            });
        }
    }
    // Automatic session/task provenance, deduplicated and still bounded.
    for (kind, reference) in [
        ("session", Some(session.id.as_str())),
        ("task", spec.task_id.as_deref()),
    ] {
        if let Some(reference) = reference
            && !evidence
                .iter()
                .any(|entry| entry.kind == kind && entry.reference == reference)
        {
            evidence.push(vp_protocol::types::MemoryEvidence {
                kind: kind.to_string(),
                reference: reference.to_string(),
                note: None,
            });
        }
    }
    if evidence.len() > vp_protocol::types::AGENT_MEMORY_MAX_EVIDENCE {
        evidence.truncate(vp_protocol::types::AGENT_MEMORY_MAX_EVIDENCE);
    }
    Ok(evidence)
}

pub(super) fn memory_tool_failure(tool: &str, error: &anyhow::Error) -> ToolOutput {
    if let Some(typed) = error.downcast_ref::<vp_protocol::types::AgentMemoryError>() {
        let code = if matches!(
            typed.code,
            vp_protocol::types::agent_memory_codes::MEMORY_CONFLICT
                | vp_protocol::types::agent_memory_codes::MEMORY_IMPORTED_READONLY
                | vp_protocol::types::agent_memory_codes::MEMORY_IDENTITY_RETIRED
        ) {
            crate::tooling::errors::ToolErrorCode::CONFLICT
        } else {
            crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT
        };
        return ToolOutput::error_coded(
            tool,
            code,
            format!("error[{}] {}", typed.code, typed.message),
        );
    }
    ToolOutput::error_coded(
        tool,
        crate::tooling::errors::ToolErrorCode::TOOL_FAILED,
        format!("{tool}: {error:#}"),
    )
}

pub(super) async fn run_mailbox_tool(
    engine: &Engine,
    session: &Session,
    spec: &AgentSpec,
    name: &str,
    input: serde_json::Value,
) -> Result<ToolOutput> {
    match name {
        "send_message" => {
            let to = input
                .get("to")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("to is required"))?;
            let kind = input
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("kind is required"))?;
            let body = input
                .get("body")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("body is required"))?;
            let payload = input
                .get("payload")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            let agents = engine.list_agents(&session.id).await?;
            let target = agents
                .iter()
                .find(|agent| {
                    agent.id == to || agent.name == to || agent.name.eq_ignore_ascii_case(to)
                })
                .ok_or_else(|| {
                    anyhow!(
                        "agent not found in this session: {to}. Address another agent by id/name; answer the human directly instead of sending to 'user'."
                    )
                })?;
            let message = engine
                .send_agent_message(
                    &session.id,
                    &spec.id,
                    &spec.name,
                    &target.id,
                    &target.name,
                    kind,
                    body,
                    payload,
                )
                .await?;
            Ok(ToolOutput::ok(format!(
                "message {} sent to {} ({})",
                message.id, message.to_name, message.kind
            )))
        }
        "read_mailbox" => {
            let unread_only = input
                .get("unread_only")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true);
            let messages = engine
                .list_agent_messages(&session.id, Some(spec.id.clone()), unread_only)
                .await?;
            let mut text = String::new();
            for message in &messages {
                if message.status == "unread" {
                    let _ = engine.mark_agent_message_read(&message.id).await;
                }
                text.push_str(&format!(
                    "[{}] {} · {} · {}
{}
",
                    message.id, message.from_name, message.kind, message.created_at, message.body
                ));
            }
            if text.is_empty() {
                text.push_str("No messages.");
            }
            Ok(ToolOutput::ok(text))
        }
        "publish_artifact" => {
            let name = input
                .get("name")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("name is required"))?;
            let content = input
                .get("content")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("content is required"))?;
            let kind = input
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("note");
            let mime_type = input
                .get("mime_type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("text/plain");
            let requested_key = input
                .get("request_key")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            if requested_key.is_some()
                && input
                    .get("artifact_id")
                    .and_then(serde_json::Value::as_str)
                    .is_some()
            {
                return Ok(ToolOutput::error(
                    "publish_artifact cannot update an artifact and use a request_key at the same time",
                ));
            }
            let artifact = if let Some(artifact_id) =
                input.get("artifact_id").and_then(serde_json::Value::as_str)
            {
                let expected = input
                    .get("expected_version")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok());
                engine
                    .publish_artifact_version(artifact_id, expected, name, kind, mime_type, content)
                    .await?
            } else if let Some(request_key) = requested_key {
                engine
                    .publish_artifact_keyed(
                        &session.id,
                        &spec.id,
                        &spec.name,
                        spec.task_id.clone(),
                        name,
                        kind,
                        mime_type,
                        content,
                        request_key,
                    )
                    .await?
            } else {
                engine
                    .publish_artifact(
                        &session.id,
                        &spec.id,
                        &spec.name,
                        spec.task_id.clone(),
                        name,
                        kind,
                        mime_type,
                        content,
                    )
                    .await?
            };
            Ok(ToolOutput::ok(format!(
                "artifact {} version {} published: {}",
                artifact.id, artifact.version, artifact.name
            )))
        }
        "list_artifacts" => {
            let artifacts = engine.list_artifacts(&session.id).await?;
            if artifacts.is_empty() {
                return Ok(ToolOutput::ok("No artifacts."));
            }
            let text = artifacts
                .iter()
                .map(|artifact| {
                    format!(
                        "{}  {}  {}  {}  {}  {} bytes  {}",
                        artifact.id,
                        artifact.name,
                        artifact.kind,
                        artifact.storage,
                        artifact.compression,
                        artifact.size_bytes,
                        artifact.agent_name
                    )
                })
                .collect::<Vec<_>>()
                .join(
                    "
",
                );
            Ok(ToolOutput::ok(text))
        }
        "read_artifact" => {
            let id = input
                .get("artifact_id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("artifact_id is required"))?;
            let artifact = engine
                .get_artifact(id)
                .await?
                .ok_or_else(|| anyhow!("artifact not found: {id}"))?;
            if artifact.storage == "file" {
                let materialized = engine
                    .materialize_artifact(id)
                    .await?
                    .ok_or_else(|| anyhow!("artifact {id} has no materialized file"))?;
                Ok(ToolOutput::ok(format!(
                    "{} [{}] · {} · {} · {} bytes · {}",
                    artifact.name,
                    artifact.mime_type,
                    artifact.compression,
                    artifact.storage,
                    artifact.size_bytes,
                    materialized.path
                )))
            } else {
                Ok(ToolOutput::ok(format!(
                    "{} [{}]
{}",
                    artifact.name, artifact.mime_type, artifact.content
                )))
            }
        }
        other => Err(anyhow!("unknown mailbox tool: {other}")),
    }
}

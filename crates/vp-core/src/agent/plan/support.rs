use super::*;

pub(in crate::agent) async fn latest_plan_results(
    engine: &Engine,
    session_id: &str,
) -> Result<HashMap<String, String>> {
    let events = engine.recent_events(session_id, 1_000).await?;
    let mut results = HashMap::new();
    for event in events {
        if event.kind != "plan_task.finished" {
            continue;
        }
        let Some(task_id) = event
            .payload
            .get("task_id")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let status = event
            .payload
            .get("task_status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        let detail = event
            .payload
            .get("summary")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                event
                    .payload
                    .get("reason")
                    .and_then(serde_json::Value::as_str)
            })
            .unwrap_or("");
        results.insert(
            task_id.to_string(),
            if detail.is_empty() {
                status.to_string()
            } else {
                format!("{status} · {}", truncate_to(detail, 500))
            },
        );
    }
    Ok(results)
}

/// Apply one guarded task transition. `None` means the store refused the
/// write because the row is already completed/terminal; callers must not emit
/// a `plan_task.finished` event in that case.
pub(in crate::agent) async fn record_scheduled_task_finish(
    engine: &Engine,
    session_id: &str,
    plan_seq: u64,
    task_id: &str,
    status: &str,
    detail: Option<&str>,
) -> Result<Option<ScheduledTask>> {
    let task = engine
        .scheduler()
        .finish(session_id, plan_seq, task_id, status, detail)
        .await?;
    if status == "completed"
        && let Some(summary) = detail.map(str::trim).filter(|summary| !summary.is_empty())
    {
        record_task_memory(engine, session_id, task_id, summary).await;
    }
    if task.is_some()
        && let Err(error) = engine.advance_orchestration(session_id).await
    {
        tracing::warn!(%error, task_id, "orchestration controller pass failed");
    }
    Ok(task)
}

pub(in crate::agent) fn task_profile(
    task: &PlanTask,
    department_profile: AgentProfile,
) -> AgentProfile {
    if let Some(profile) = &task.profile
        && let Some(profile) = AgentProfile::parse(profile)
    {
        return profile;
    }
    match task.kind.as_str() {
        "research" => AgentProfile::Research,
        "review" | "verification" => AgentProfile::Verification,
        "coding" => AgentProfile::Coding,
        _ => department_profile,
    }
}

pub(in crate::agent) async fn ensure_plan_manager(
    engine: &Engine,
    config: &AgentConfig,
    session: &Session,
    budget: &SessionBudget,
    leader_spec: &AgentSpec,
    department: &PlanDepartment,
) -> Result<AgentSpec> {
    let profile = AgentProfile::parse(&department.profile)
        .ok_or_else(|| anyhow!("unknown profile: {}", department.profile))?;
    let existing = engine
        .list_agents(&session.id)
        .await?
        .into_iter()
        .find(|agent| {
            agent.role == "manager"
                && agent
                    .title
                    .as_deref()
                    .is_some_and(|title| title.eq_ignore_ascii_case(&department.manager_title))
        });
    let agent = match existing {
        Some(agent) => {
            engine
                .set_agent_profile(&agent.id, profile.as_str())
                .await?;
            engine
                .set_agent_task(&agent.id, None, Some(department.name.clone()))
                .await?;
            engine.update_agent_status(&agent.id, "idle").await?;
            agent
        }
        None => {
            if budget.reserve(AgentRole::Manager).is_none() {
                return Err(anyhow!("the Manager cap for this session has been reached"));
            }
            let agent = engine
                .create_agent(
                    &session.id,
                    AgentRole::Manager.as_str(),
                    &department.manager_title,
                    Some(department.manager_title.clone()),
                    Some(leader_spec.id.clone()),
                    &model_for(config, AgentRole::Manager),
                )
                .await?;
            engine
                .set_agent_profile(&agent.id, profile.as_str())
                .await?;
            engine
                .set_agent_task(&agent.id, None, Some(department.name.clone()))
                .await?;
            engine.update_agent_status(&agent.id, "idle").await?;
            agent
        }
    };
    let session_worktree = session
        .worktree
        .clone()
        .map(PathBuf::from)
        .unwrap_or_default();
    let session_branch = session.branch.clone().unwrap_or_default();
    let spec = AgentSpec {
        id: agent.id.clone(),
        role: AgentRole::Manager,
        name: agent.name.clone(),
        depth: 1,
        parent_name: Some(leader_spec.name.clone()),
        title: Some(department.manager_title.clone()),
        task_id: None,
        department: Some(department.name.clone()),
        identity_id: None,
        identity_generation: None,
        profile,
        allow_delegation: false,
        task_kind: None,
        auto_approve: false,
        worktree: session_worktree.clone(),
        branch: session_branch.clone(),
        session_worktree,
        session_branch,
        write_scope: Vec::new(),
        base_commit: session.base_commit.clone().unwrap_or_default(),

        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
    };
    engine
        .set_agent_worktree(&agent.id, &spec.worktree.to_string_lossy(), &spec.branch)
        .await?;
    emit(
        engine,
        &session.id,
        &spec,
        "agent.spawned",
        serde_json::json!({
            "parent_id": leader_spec.id,
            "parent_name": leader_spec.name,
            "objective": department.objective,
            "department": department.name,
            "lazy": true,
        }),
    )
    .await?;
    Ok(spec)
}

pub(super) fn normalized_task_scope(task: &PlanTask) -> Vec<String> {
    task.write_scope
        .iter()
        .map(|scope| normalize_scope(scope))
        .collect()
}

/// Select a ready batch that can safely run concurrently. The input order is
/// the scheduler's effective-priority order (session + task + aging); this
/// function only removes scope conflicts. Tasks with no scope are treated as
/// unrestricted and run alone. A task that overlaps an earlier task in the
/// batch stays pending for the next batch instead of being merged into a
/// conflicting write set.
pub(in crate::agent) fn select_plan_batch(ready: Vec<PlanTask>, limit: usize) -> Vec<PlanTask> {
    let mut batch: Vec<PlanTask> = Vec::new();
    for task in ready {
        if batch.len() >= limit {
            break;
        }
        let scope = normalized_task_scope(&task);
        if !batch.is_empty() {
            if scope.is_empty() {
                continue;
            }
            let overlaps = batch.iter().any(|existing| {
                let existing = normalized_task_scope(existing);
                existing.is_empty() || scopes_overlap(&scope, &existing)
            });
            if overlaps {
                continue;
            }
        }
        batch.push(task);
    }
    batch
}

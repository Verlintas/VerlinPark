use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) async fn finish_worker(
    engine: &Engine,
    config: &AgentConfig,
    session: &Session,
    project: &Project,
    control: &RunControl,
    budget: &SessionBudget,
    manager_spec: &AgentSpec,
    worker_spec: &AgentSpec,
    objective: &str,
    summary: &str,
    acceptance: Option<&str>,
) -> Result<WorkerFinish> {
    let commit = match commit_worker(worker_spec, &worker_spec.name).await {
        Ok(commit) => commit,
        Err(error) => {
            let summary = format!("integration failed: {error}");
            engine
                .update_agent_status(&worker_spec.id, "failed")
                .await?;
            emit(
                engine,
                &session.id,
                worker_spec,
                "agent.finished",
                serde_json::json!({
                    "status": "failed",
                    "summary": truncate(&summary),
                    "merged": false,
                    "worktree": worker_spec.worktree,
                    "branch": worker_spec.branch,
                }),
            )
            .await?;
            return Ok(WorkerFinish::Done {
                report: summary,
                success: false,
            });
        }
    };
    let Some(commit) = commit else {
        engine.update_agent_status(&worker_spec.id, "idle").await?;
        emit(
            engine,
            &session.id,
            worker_spec,
            "agent.finished",
            serde_json::json!({
                "status": "idle",
                "summary": truncate(summary),
                "merged": false,
                "verified": false,
                "worktree": worker_spec.worktree,
                "branch": worker_spec.branch,
            }),
        )
        .await?;
        return Ok(WorkerFinish::Done {
            report: format!("no changes to merge\n{}", truncate(summary)),
            success: !worker_report_reports_failure(summary),
        });
    };

    record_worker_commit_effect(engine, session, worker_spec, &commit).await;
    let changed_paths = bounded_worker_changed_paths(worker_spec, &commit);
    let verification = verify_worker(
        engine,
        config,
        session,
        project,
        control,
        budget,
        manager_spec,
        worker_spec,
        &commit,
        objective,
        summary,
        acceptance,
        false,
        &VerifierProvenance::default(),
    )
    .await?;
    match verification {
        VerificationOutcome::Approved(verdict) => {
            match integrate_worker(
                engine,
                session,
                worker_spec,
                &commit,
                &verdict.summary,
                config.integration_command.as_deref(),
            )
            .await
            {
                Ok(detail) => {
                    record_verification_memory(
                        engine,
                        &session.id,
                        worker_spec.task_id.as_deref(),
                        &worker_spec.name,
                        &commit,
                        &verdict,
                        Some(&detail),
                    )
                    .await;
                    engine.update_agent_status(&worker_spec.id, "idle").await?;
                    emit(
                        engine,
                        &session.id,
                        worker_spec,
                        "agent.finished",
                        serde_json::json!({
                            "status": "idle",
                            "summary": truncate(summary),
                            "commit": commit,
                            "changed_paths": changed_paths,
                            "merged": true,
                            "verified": true,
                            "approved": true,
                            "integrator": true,
                            "integration": truncate(&detail),
                            "verdict": truncate(&verdict.summary),
                            "worktree": worker_spec.worktree,
                            "branch": worker_spec.branch,
                        }),
                    )
                    .await?;
                    Ok(WorkerFinish::Done {
                        report: format!(
                            "merged: true\nverified: true\nintegrator: {}\n{}\nverdict: {}",
                            truncate(&detail),
                            truncate(summary),
                            truncate(&verdict.summary)
                        ),
                        success: true,
                    })
                }
                Err(error) => {
                    engine
                        .update_agent_status(&worker_spec.id, "failed")
                        .await?;
                    emit(
                        engine,
                        &session.id,
                        worker_spec,
                        "agent.finished",
                        serde_json::json!({
                            "status": "failed",
                            "summary": truncate(error.to_string().as_str()),
                            "commit": commit,
                            "changed_paths": changed_paths,
                            "merged": false,
                            "verified": true,
                            "approved": true,
                            "integrator": true,
                            "verdict": truncate(&verdict.summary),
                            "worktree": worker_spec.worktree,
                            "branch": worker_spec.branch,
                        }),
                    )
                    .await?;
                    let memory_integration = format!("integration failed: {error:#}");
                    record_verification_memory(
                        engine,
                        &session.id,
                        worker_spec.task_id.as_deref(),
                        &worker_spec.name,
                        &commit,
                        &verdict,
                        Some(&memory_integration),
                    )
                    .await;
                    Ok(WorkerFinish::Done {
                        report: format!(
                            "merge/integration failed: {error}\nverdict: {}",
                            truncate(&verdict.summary)
                        ),
                        success: false,
                    })
                }
            }
        }
        VerificationOutcome::Rejected(verdict) => {
            engine
                .update_agent_status(&worker_spec.id, "changes_requested")
                .await?;
            emit(
                engine,
                &session.id,
                worker_spec,
                "agent.finished",
                serde_json::json!({
                    "status": "changes_requested",
                    "summary": truncate(summary),
                    "commit": commit,
                    "changed_paths": changed_paths,
                    "merged": false,
                    "verified": true,
                    "approved": false,
                    "verdict": truncate(&verdict.summary),
                    "worktree": worker_spec.worktree,
                    "branch": worker_spec.branch,
                }),
            )
            .await?;
            record_verification_memory(
                engine,
                &session.id,
                worker_spec.task_id.as_deref(),
                &worker_spec.name,
                &commit,
                &verdict,
                None,
            )
            .await;
            Ok(WorkerFinish::Done {
                report: format!(
                    "merged: false\nverification rejected:\n{}",
                    truncate(&verdict.summary)
                ),
                success: false,
            })
        }
        VerificationOutcome::Error(error) => {
            engine
                .update_agent_status(&worker_spec.id, "failed")
                .await?;
            emit(
                engine,
                &session.id,
                worker_spec,
                "agent.finished",
                serde_json::json!({
                    "status": "failed",
                    "summary": truncate(&error),
                    "commit": commit,
                    "changed_paths": changed_paths,
                    "merged": false,
                    "verified": false,
                    "worktree": worker_spec.worktree,
                    "branch": worker_spec.branch,
                }),
            )
            .await?;
            Ok(WorkerFinish::Done {
                report: format!("verification error: {error}"),
                success: false,
            })
        }
        VerificationOutcome::Cancelled => {
            engine
                .update_agent_status(&worker_spec.id, "cancelled")
                .await?;
            emit(
                engine,
                &session.id,
                worker_spec,
                "agent.finished",
                serde_json::json!({
                    "status": "cancelled",
                    "commit": commit,
                    "merged": false,
                }),
            )
            .await?;
            Ok(WorkerFinish::Cancelled)
        }
        VerificationOutcome::BudgetExceeded(stop) => {
            engine
                .update_agent_status(&worker_spec.id, "failed")
                .await?;
            emit(
                engine,
                &session.id,
                worker_spec,
                "agent.finished",
                serde_json::json!({
                    "status": "failed",
                    "summary": format!("{} during verification", stop.summary()),
                    "commit": commit,
                    "merged": false,
                }),
            )
            .await?;
            if stop.is_abortive() {
                Ok(WorkerFinish::BudgetExceeded(stop))
            } else {
                Ok(WorkerFinish::Done {
                    report: format!("verification stopped: {}", stop.summary()),
                    success: false,
                })
            }
        }
    }
}

/// Finish one workspace-bound Labour run. The Labour commit is merged into the
/// parent Worker session workspace with scope/diff checks and an exact
/// rollback. This is internal Worker integration, not a Verifier/Integrator
/// bypass: the resulting Worker commit still enters the normal Verifier ->
/// merge queue -> deterministic Integrator path.
pub(super) async fn finish_labour(
    engine: &Engine,
    session: &Session,
    parent_spec: &AgentSpec,
    labour_spec: &AgentSpec,
    summary: &str,
) -> Result<WorkerFinish> {
    let commit = match commit_worker(labour_spec, &labour_spec.name).await {
        Ok(commit) => commit,
        Err(error) => {
            let detail = format!("Labour commit failed: {error}");
            engine
                .update_agent_status(&labour_spec.id, "failed")
                .await?;
            emit(
                engine,
                &session.id,
                labour_spec,
                "agent.finished",
                serde_json::json!({
                    "status": "failed",
                    "summary": truncate(&detail),
                    "merged": false,
                    "worktree": labour_spec.worktree,
                    "branch": labour_spec.branch,
                    "parent_workspace_id": labour_spec.parent_workspace_id,
                }),
            )
            .await?;
            return Ok(WorkerFinish::Done {
                report: detail,
                success: false,
            });
        }
    };
    let Some(commit) = commit else {
        engine.update_agent_status(&labour_spec.id, "idle").await?;
        emit(
            engine,
            &session.id,
            labour_spec,
            "agent.finished",
            serde_json::json!({
                "status": "idle",
                "summary": truncate(summary),
                "merged": false,
                "worktree": labour_spec.worktree,
                "branch": labour_spec.branch,
                "parent_workspace_id": labour_spec.parent_workspace_id,
            }),
        )
        .await?;
        return Ok(WorkerFinish::Done {
            report: format!("no changes to merge\n{}", truncate(summary)),
            success: !worker_report_reports_failure(summary),
        });
    };
    record_worker_commit_effect(engine, session, labour_spec, &commit).await;
    let workspace_id = parent_spec
        .workspace_id
        .as_deref()
        .ok_or_else(|| anyhow!("Labour workspace binding has no parent project workspace"))?;
    let outcome = engine
        .merge_labour_commit(
            workspace_id,
            &session.id,
            &labour_spec.id,
            labour_spec.task_id.as_deref(),
            &commit,
            &labour_spec.write_scope,
        )
        .await;
    match outcome {
        Ok(outcome) if outcome.merged => {
            engine.update_agent_status(&labour_spec.id, "idle").await?;
            emit(
                engine,
                &session.id,
                labour_spec,
                "agent.finished",
                serde_json::json!({
                    "status": "idle",
                    "summary": truncate(summary),
                    "commit": commit,
                    "merged": true,
                    "merged_into": "worker_workspace",
                    "pre_merge": outcome.pre_merge,
                    "post_merge": outcome.post_merge,
                    "paths": outcome.paths,
                    "worktree": labour_spec.worktree,
                    "branch": labour_spec.branch,
                    "parent_workspace_id": labour_spec.parent_workspace_id,
                }),
            )
            .await?;
            Ok(WorkerFinish::Done {
                report: format!(
                    "merged into Worker workspace: true\ncommit: {}\nworker commit: {}\npaths: {}\n{}",
                    commit,
                    outcome.post_merge,
                    truncate(&outcome.paths.join(", ")),
                    truncate(summary)
                ),
                success: true,
            })
        }
        Ok(outcome) => {
            let detail = truncate(&outcome.detail);
            engine
                .update_agent_status(&labour_spec.id, "failed")
                .await?;
            emit(
                engine,
                &session.id,
                labour_spec,
                "agent.finished",
                serde_json::json!({
                    "status": "failed",
                    "summary": truncate(&detail),
                    "commit": commit,
                    "merged": false,
                    "merged_into": "worker_workspace",
                    "pre_merge": outcome.pre_merge,
                    "post_merge": outcome.post_merge,
                    "conflicts": outcome.conflicts,
                    "paths": outcome.paths,
                    "worktree": labour_spec.worktree,
                    "branch": labour_spec.branch,
                    "parent_workspace_id": labour_spec.parent_workspace_id,
                }),
            )
            .await?;
            Ok(WorkerFinish::Done {
                report: format!(
                    "internal Worker integration failed; Worker HEAD is unchanged at {}\n{}\n{}",
                    outcome.pre_merge,
                    detail,
                    truncate(summary)
                ),
                success: false,
            })
        }
        Err(error) => {
            let detail = truncate(&format!("{error:#}"));
            engine
                .update_agent_status(&labour_spec.id, "failed")
                .await?;
            emit(
                engine,
                &session.id,
                labour_spec,
                "agent.finished",
                serde_json::json!({
                    "status": "failed",
                    "summary": detail,
                    "commit": commit,
                    "merged": false,
                    "merged_into": "worker_workspace",
                    "worktree": labour_spec.worktree,
                    "branch": labour_spec.branch,
                    "parent_workspace_id": labour_spec.parent_workspace_id,
                }),
            )
            .await?;
            Ok(WorkerFinish::Done {
                report: format!("internal Worker integration failed: {detail}"),
                success: false,
            })
        }
    }
}

pub(crate) fn validate_team_plan(plan: &TeamPlan, max_managers: usize) -> Result<()> {
    if plan.goal.trim().is_empty() {
        return Err(anyhow!("team plan goal must not be empty"));
    }
    if plan.departments.is_empty() || plan.departments.len() > max_managers {
        return Err(anyhow!("team plan needs 1 to {max_managers} departments"));
    }
    let mut names = std::collections::HashSet::new();
    let mut titles = std::collections::HashSet::new();
    for department in &plan.departments {
        let name = department.name.trim();
        let title = department.manager_title.trim();
        if name.is_empty() || name.len() > 40 {
            return Err(anyhow!("department name must be 1-40 chars"));
        }
        if title.is_empty() || title.len() > 60 {
            return Err(anyhow!("manager title must be 1-60 chars"));
        }
        if !names.insert(name.to_lowercase()) {
            return Err(anyhow!("duplicate department name: {name}"));
        }
        if !titles.insert(title.to_lowercase()) {
            return Err(anyhow!("duplicate manager title: {title}"));
        }
        if AgentProfile::parse(&department.profile).is_none() {
            return Err(anyhow!(
                "unknown profile '{}' for department '{}'",
                department.profile,
                department.name
            ));
        }
        if department.objective.trim().is_empty() {
            return Err(anyhow!(
                "department '{}' needs an objective",
                department.name
            ));
        }
        if department.token_budget == Some(0) {
            return Err(anyhow!(
                "department '{}' token budget must be greater than zero",
                department.name
            ));
        }
        if department
            .cost_budget_usd
            .is_some_and(|budget| !budget.is_finite() || budget <= 0.0)
        {
            return Err(anyhow!(
                "department '{}' cost budget must be a positive finite number",
                department.name
            ));
        }
    }
    if plan.tasks.len() > 100 {
        return Err(anyhow!("team plan supports at most 100 tasks"));
    }
    let mut ids = std::collections::HashSet::new();
    for task in &plan.tasks {
        let id = task.id.trim();
        worktree::validate_task_id(id)?;
        if !ids.insert(id.to_string()) {
            return Err(anyhow!("duplicate task id: {id}"));
        }
        let has_parent = task.parent_task_id.is_some();
        let has_key = task.fanout_key.is_some();
        if has_parent != has_key {
            return Err(anyhow!(
                "task '{id}' must set parent_task_id and fanout_key together"
            ));
        }
        if let Some(parent) = task.parent_task_id.as_deref()
            && (parent.trim().is_empty() || parent == id)
        {
            return Err(anyhow!("task '{id}' has an invalid fan-out parent"));
        }
        if let Some(key) = task.fanout_key.as_deref()
            && (key.is_empty()
                || key.chars().count() > 64
                || !key
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.')))
        {
            return Err(anyhow!("task '{id}' has an invalid fan-out key"));
        }
        if let Some(parent) = task.fanout_join_for.as_deref() {
            if parent.trim().is_empty() || parent == id {
                return Err(anyhow!("task '{id}' has an invalid fan-out join parent"));
            }
            if has_parent || has_key {
                return Err(anyhow!(
                    "task '{id}' cannot be both a fan-out join and a fan-out child"
                ));
            }
            if !task.write_scope.is_empty() {
                return Err(anyhow!(
                    "fan-out join task '{id}' must not declare a write scope"
                ));
            }
        }
        let Some(department) = plan
            .departments
            .iter()
            .find(|department| department.name == task.department)
        else {
            return Err(anyhow!(
                "task '{}' references unknown department '{}'",
                id,
                task.department
            ));
        };
        let needs_write = !task.write_scope.is_empty()
            || matches!(
                task.kind.to_ascii_lowercase().as_str(),
                "coding" | "code" | "implement" | "implementation" | "edit" | "fix"
            );
        let resolved_profile = task
            .profile
            .as_deref()
            .unwrap_or(department.profile.as_str());
        if needs_write && matches!(resolved_profile, "generic" | "verification") {
            return Err(anyhow!(
                "task '{}' needs a writable profile: set task.profile=\"coding\" or use a coding department",
                id
            ));
        }
        if task.title.trim().is_empty() {
            return Err(anyhow!("task '{id}' needs a title"));
        }
        if task.kind.trim().is_empty() {
            return Err(anyhow!("task '{id}' needs a kind"));
        }
        if !(-100..=100).contains(&task.priority) {
            return Err(anyhow!("task '{id}' priority must be between -100 and 100"));
        }
        if let Some(profile) = &task.profile
            && AgentProfile::parse(profile).is_none()
        {
            return Err(anyhow!("task '{id}' has unknown profile: {profile}"));
        }
        if task.acceptance.is_empty() {
            return Err(anyhow!("task '{id}' needs at least one acceptance item"));
        }
        for dep in &task.deps {
            if dep == id {
                return Err(anyhow!("task '{id}' depends on itself"));
            }
            if !ids.contains(dep) && !plan.tasks.iter().any(|candidate| candidate.id == *dep) {
                return Err(anyhow!("task '{id}' has unknown dependency '{dep}'"));
            }
        }
        let mut scopes = Vec::new();
        for scope in &task.write_scope {
            let normalized = normalize_scope(scope);
            if normalized.is_empty()
                || normalized.contains("..")
                || normalized.starts_with('/')
                || normalized.starts_with('~')
            {
                return Err(anyhow!("task '{id}' has invalid write scope: {scope}"));
            }
            scopes.push(normalized);
        }
        if task.token_budget == Some(0) {
            return Err(anyhow!(
                "task '{id}' token budget must be greater than zero"
            ));
        }
        if task
            .cost_budget_usd
            .is_some_and(|budget| !budget.is_finite() || budget <= 0.0)
        {
            return Err(anyhow!(
                "task '{id}' cost budget must be a positive finite number"
            ));
        }
        if let Some(department) = plan
            .departments
            .iter()
            .find(|department| department.name == task.department)
        {
            if let (Some(task_limit), Some(department_limit)) =
                (task.token_budget, department.token_budget)
                && task_limit > department_limit
            {
                return Err(anyhow!(
                    "task '{id}' token budget exceeds its department budget"
                ));
            }
            if let (Some(task_limit), Some(department_limit)) =
                (task.cost_budget_usd, department.cost_budget_usd)
                && task_limit > department_limit + f64::EPSILON
            {
                return Err(anyhow!(
                    "task '{id}' cost budget exceeds its department budget"
                ));
            }
        }
    }
    // Fan-out children and their shared join form one explicit group. The join
    // must depend on exactly the children and no child may have a second key.
    let mut fanout_keys: std::collections::HashMap<(&str, &str), &str> =
        std::collections::HashMap::new();
    for task in &plan.tasks {
        let (Some(parent), Some(key)) =
            (task.parent_task_id.as_deref(), task.fanout_key.as_deref())
        else {
            continue;
        };
        if let Some(existing) = fanout_keys.insert((parent, key), task.id.as_str()) {
            return Err(anyhow!(
                "fan-out parent '{parent}' repeats key '{key}' in tasks '{existing}' and '{}'",
                task.id
            ));
        }
        if !plan
            .tasks
            .iter()
            .any(|candidate| candidate.fanout_join_for.as_deref() == Some(parent))
        {
            return Err(anyhow!(
                "fan-out child '{}' has no shared join task for parent '{parent}'",
                task.id
            ));
        }
    }
    for join in plan
        .tasks
        .iter()
        .filter(|task| task.fanout_join_for.is_some())
    {
        let parent = join.fanout_join_for.as_deref().unwrap_or_default();
        let children = plan
            .tasks
            .iter()
            .filter(|task| task.parent_task_id.as_deref() == Some(parent))
            .collect::<Vec<_>>();
        if children.is_empty() {
            return Err(anyhow!(
                "fan-out join task '{}' has no children for parent '{parent}'",
                join.id
            ));
        }
        let child_ids = children
            .iter()
            .map(|child| child.id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let dep_ids = join
            .deps
            .iter()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        if child_ids != dep_ids {
            return Err(anyhow!(
                "fan-out join task '{}' must depend on exactly its {} children",
                join.id,
                children.len()
            ));
        }
    }
    for department in &plan.departments {
        let task_tokens: u64 = plan
            .tasks
            .iter()
            .filter(|task| task.department == department.name)
            .filter_map(|task| task.token_budget)
            .fold(0, u64::saturating_add);
        if let Some(limit) = department.token_budget
            && task_tokens > limit
        {
            return Err(anyhow!(
                "department '{}' task token budgets exceed the department cap",
                department.name
            ));
        }
        let task_cost: f64 = plan
            .tasks
            .iter()
            .filter(|task| task.department == department.name)
            .filter_map(|task| task.cost_budget_usd)
            .sum();
        if let Some(limit) = department.cost_budget_usd
            && task_cost > limit + f64::EPSILON
        {
            return Err(anyhow!(
                "department '{}' task cost budgets exceed the department cap",
                department.name
            ));
        }
    }
    // Cycle detection over the task DAG.
    fn visit(
        id: &str,
        tasks: &[vp_protocol::types::PlanTask],
        state: &mut std::collections::HashMap<String, u8>,
    ) -> Result<()> {
        match state.get(id).copied().unwrap_or(0) {
            1 => return Err(anyhow!("team plan task graph contains a cycle at '{id}'")),
            2 => return Ok(()),
            _ => {}
        }
        state.insert(id.to_string(), 1);
        if let Some(task) = tasks.iter().find(|task| task.id == id) {
            for dep in &task.deps {
                visit(dep, tasks, state)?;
            }
        }
        state.insert(id.to_string(), 2);
        Ok(())
    }
    let mut state = std::collections::HashMap::new();
    for task in &plan.tasks {
        visit(&task.id, &plan.tasks, &mut state)?;
    }
    Ok(())
}

pub(super) enum RoundtableResult {
    Done(String),
    Cancelled,
    BudgetExceeded(BudgetStop),
}

pub(super) fn plan_budget_text(tokens: Option<u64>, cost: Option<f64>) -> String {
    match (tokens, cost) {
        (None, None) => String::new(),
        (Some(tokens), None) => format!(" budget={tokens} tokens"),
        (None, Some(cost)) => format!(" budget=${cost:.4}"),
        (Some(tokens), Some(cost)) => format!(" budget={tokens} tokens/${cost:.4}"),
    }
}

pub(super) fn plan_prompt(plan: &TeamPlan) -> String {
    let departments = plan
        .departments
        .iter()
        .map(|department| {
            let budget = plan_budget_text(department.token_budget, department.cost_budget_usd);
            format!(
                "- {}: manager '{}' ({}) — {}{}",
                department.name,
                department.manager_title,
                department.profile,
                department.objective,
                budget
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let tasks = plan
        .tasks
        .iter()
        .map(|task| {
            format!(
                "- {} [{}] {} priority={} deps={} scope={}{}",
                task.id,
                task.department,
                task.title,
                task.priority,
                task.deps.join(","),
                task.write_scope.join(","),
                plan_budget_text(task.token_budget, task.cost_budget_usd)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Goal: {}\nDepartments:\n{departments}\nTasks:\n{tasks}",
        plan.goal
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn deliberate_team_plan(
    engine: &Engine,
    config: &AgentConfig,
    session: &Session,
    project: &Project,
    control: &RunControl,
    budget: &SessionBudget,
    leader_spec: &AgentSpec,
    plan: &TeamPlan,
    rounds: u32,
) -> Result<RoundtableResult> {
    let mut managers = Vec::new();
    for department in &plan.departments {
        managers.push(
            ensure_plan_manager(engine, config, session, budget, leader_spec, department).await?,
        );
    }
    let overview = plan_prompt(plan);
    let mut positions: Vec<(String, String)> = Vec::new();
    let mut report = Vec::new();

    for round in 1..=rounds {
        if control.is_cancelled() {
            return Ok(RoundtableResult::Cancelled);
        }
        if budget.exhausted() {
            return Ok(RoundtableResult::BudgetExceeded(BudgetStop::session(
                budget,
            )));
        }
        let peer_context = positions
            .iter()
            .map(|(name, position)| format!("{name}: {position}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut prepared = Vec::new();
        for manager in &managers {
            let title = manager.title.as_deref().unwrap_or(manager.name.as_str());
            let prompt = if round == 1 {
                format!(
                    "Manager roundtable, round 1.\n{overview}\n\
                     You are the {title} Manager ({profile}). Analyze your department scope, \
                     dependencies, risks, and required handoffs with the other Managers. \
                     Do not delegate, edit, or run shell. Return a concise position.",
                    profile = manager.profile.as_str()
                )
            } else {
                format!(
                    "Manager roundtable, round {round}.\n{overview}\n\
                     Positions from the previous round:\n{peer_context}\n\
                     You are the {title} Manager ({profile}). Respond to peers, resolve \
                     conflicts, agree interfaces and handoffs, and update your position. \
                     Do not delegate, edit, or run shell. Return a concise decision.",
                    profile = manager.profile.as_str()
                )
            };
            let conversation = vec![ChatMessage::user(prompt.clone())];
            prepared.push((manager.clone(), prompt, conversation));
        }

        let futures = prepared.iter_mut().map(|(manager, prompt, conversation)| {
            run_loop(
                engine,
                config,
                session,
                project,
                control,
                budget,
                manager,
                prompt,
                conversation,
                None,
                false,
                None,
            )
        });
        let results = futures_util::future::join_all(futures).await;
        let mut round_positions = Vec::new();
        for (index, result) in results.into_iter().enumerate() {
            let (manager, _, _) = &prepared[index];
            match result {
                Ok(LoopOutcome::Final(position)) => {
                    let position = if position.trim().is_empty() {
                        "No position returned.".to_string()
                    } else {
                        position
                    };
                    emit(
                        engine,
                        &session.id,
                        manager,
                        "manager.roundtable",
                        serde_json::json!({
                            "round": round,
                            "position": truncate(&position),
                            "title": manager.title,
                            "profile": manager.profile.as_str(),
                        }),
                    )
                    .await?;
                    round_positions.push((
                        manager
                            .title
                            .clone()
                            .unwrap_or_else(|| manager.name.clone()),
                        position,
                    ));
                }
                Ok(LoopOutcome::Deferred(wait)) => {
                    emit(
                        engine,
                        &session.id,
                        manager,
                        "manager.roundtable.failed",
                        serde_json::json!({
                            "round": round,
                            "error": format!(
                                "a roundtable Manager cannot defer to external gate {}",
                                wait.gate_id
                            ),
                        }),
                    )
                    .await?;
                    return Err(anyhow!(
                        "a roundtable Manager cannot defer to external gate {}",
                        wait.gate_id
                    ));
                }
                Ok(LoopOutcome::Cancelled) => return Ok(RoundtableResult::Cancelled),
                Ok(LoopOutcome::BudgetExceeded(stop)) => {
                    return Ok(RoundtableResult::BudgetExceeded(stop));
                }
                Ok(LoopOutcome::Verdict(_)) => {}
                Err(error) => {
                    emit(
                        engine,
                        &session.id,
                        manager,
                        "manager.roundtable.failed",
                        serde_json::json!({
                            "round": round,
                            "error": truncate(error.to_string().as_str()),
                        }),
                    )
                    .await?;
                    return Err(error);
                }
            }
        }
        positions = round_positions;
        report.push(format!(
            "Round {round}\n{}",
            positions
                .iter()
                .map(|(title, position)| format!("{title}: {}", truncate(position)))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    Ok(RoundtableResult::Done(report.join("\n\n")))
}

pub(super) enum PlanRunResult {
    Done(String),
    Cancelled,
    BudgetExceeded(BudgetStop),
    Deferred(DeferredWait),
}

pub(super) enum PlanAbort {
    Cancelled,
    Budget(BudgetStop),
    Deferred(DeferredWait),
}

impl PlanAbort {
    pub(super) fn task_status(&self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::Budget(_) => "blocked",
            Self::Deferred(_) => "blocked",
        }
    }

    pub(super) fn reason(&self) -> String {
        match self {
            Self::Cancelled => "session cancelled".to_string(),
            Self::Budget(stop) => stop.summary(),
            Self::Deferred(wait) => {
                format!("waiting for external gate {}", wait.gate_id)
            }
        }
    }
}

pub(super) async fn emit_plan_task_finished(
    engine: &Engine,
    session_id: &str,
    spec: &AgentSpec,
    task_id: &str,
    status: &str,
    reason: Option<&str>,
) -> Result<()> {
    let mut payload = serde_json::json!({
        "task_id": task_id,
        "task_status": status,
    });
    if let Some(reason) = reason {
        payload["reason"] = serde_json::json!(reason);
    }
    emit(engine, session_id, spec, "plan_task.finished", payload).await
}

pub(super) async fn latest_plan_results(
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

pub(super) async fn record_scheduled_task_finish(
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

pub(super) async fn record_task_memory(
    engine: &Engine,
    session_id: &str,
    task_id: &str,
    summary: &str,
) {
    let Ok((_session, project)) = load_session_project(engine.store().clone(), session_id).await
    else {
        return;
    };
    let title = format!("Task {task_id} result");
    let mut evidence = vec![
        vp_protocol::types::MemoryEvidence {
            kind: "task".to_string(),
            reference: task_id.to_string(),
            note: None,
        },
        vp_protocol::types::MemoryEvidence {
            kind: "session".to_string(),
            reference: session_id.to_string(),
            note: None,
        },
    ];
    let store = engine.store().clone();
    let lookup_session = session_id.to_string();
    let lookup_task = task_id.to_string();
    let merged = tokio::task::spawn_blocking(move || {
        store.latest_merged_entry_for_task(&lookup_session, &lookup_task)
    })
    .await
    .ok()
    .and_then(std::result::Result::ok)
    .flatten();
    if let Some(entry) = merged {
        evidence.push(vp_protocol::types::MemoryEvidence {
            kind: "commit".to_string(),
            reference: entry.commit.clone(),
            note: None,
        });
        if !entry.base_commit.trim().is_empty()
            && let Ok(paths) = crate::git_output(
                std::path::Path::new(&project.root),
                &[
                    "diff",
                    "--name-only",
                    &format!("{}..{}", entry.base_commit, entry.commit),
                ],
            )
        {
            for path in paths
                .lines()
                .map(str::trim)
                .filter(|path| !path.is_empty())
                .take(20)
            {
                evidence.push(vp_protocol::types::MemoryEvidence {
                    kind: "path".to_string(),
                    reference: path.to_string(),
                    note: None,
                });
            }
        }
    }
    let Ok(policy) = engine.get_memory_policy(&project.id).await else {
        return;
    };
    let verify_after = crate::memory_verify_after(policy.policy.task_result_verify_days);
    if let Err(error) = engine
        .create_memory_with_metadata(
            session_id,
            "orchestrator",
            "Orchestrator",
            "task_result",
            &title,
            &truncate_to(summary, 8_000),
            vec![task_id.to_string(), "task_result".to_string()],
            "task_result",
            evidence,
            false,
            verify_after,
        )
        .await
    {
        tracing::warn!(%error, task_id, "failed to persist task result memory");
    }
}

/// Persist a capture memory with the project's default verification TTL.
/// Capture is best-effort: callers keep their normal gate result even when the
/// memory write fails.
#[allow(clippy::too_many_arguments)]
pub(super) async fn record_capture_memory(
    engine: &Engine,
    session_id: &str,
    agent_id: &str,
    agent_name: &str,
    kind: &str,
    title: &str,
    body: &str,
    tags: Vec<String>,
    source: &str,
    evidence: Vec<vp_protocol::types::MemoryEvidence>,
) {
    let Ok((_session, project)) = load_session_project(engine.store().clone(), session_id).await
    else {
        tracing::warn!(%session_id, kind, "failed to load project for memory capture");
        return;
    };
    let verify_after = match engine.get_memory_policy(&project.id).await {
        Ok(policy) => crate::memory_verify_after(policy.policy.default_verify_days.or(Some(365))),
        Err(error) => {
            tracing::warn!(%error, %session_id, kind, "memory policy lookup failed; using fallback TTL");
            crate::memory_verify_after(Some(365))
        }
    };
    if let Err(error) = engine
        .create_memory_with_metadata(
            session_id,
            agent_id,
            agent_name,
            kind,
            title,
            body,
            tags,
            source,
            evidence,
            false,
            verify_after,
        )
        .await
    {
        tracing::warn!(%error, %session_id, kind, "failed to persist capture memory");
    }
}

/// Capture a Verifier verdict as durable project knowledge. This is
/// best-effort and never changes the Worker/Verifier/merge result.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn record_verification_memory(
    engine: &Engine,
    session_id: &str,
    task_id: Option<&str>,
    worker_name: &str,
    commit: &str,
    verdict: &VerifierVerdict,
    integration: Option<&str>,
) {
    let task_id = task_id.map(str::trim).filter(|value| !value.is_empty());
    let title = match task_id {
        Some(task_id) => format!("Task {task_id} verification"),
        None => format!(
            "Worker {} verification {}",
            worker_name.trim(),
            commit.trim().chars().take(8).collect::<String>()
        ),
    };
    let mut body = format!(
        "approved: {}\nverdict: {}\n",
        verdict.approved,
        truncate_to(verdict.summary.trim(), 2_000)
    );
    if let Some(integration) = integration {
        body.push_str(&format!(
            "integration: {}\n",
            truncate_to(integration.trim(), 4_000)
        ));
    }
    body.push_str(&format!("commit: {}", commit.trim()));
    let mut tags = Vec::new();
    if let Some(task_id) = task_id {
        tags.push(task_id.to_string());
    }
    tags.push("verification".to_string());
    let mut evidence = Vec::new();
    if let Some(task_id) = task_id {
        evidence.push(vp_protocol::types::MemoryEvidence {
            kind: "task".to_string(),
            reference: task_id.to_string(),
            note: None,
        });
    }
    evidence.push(vp_protocol::types::MemoryEvidence {
        kind: "session".to_string(),
        reference: session_id.to_string(),
        note: None,
    });
    evidence.push(vp_protocol::types::MemoryEvidence {
        kind: "commit".to_string(),
        reference: commit.trim().to_string(),
        note: None,
    });
    record_capture_memory(
        engine,
        session_id,
        "verifier",
        "Verifier",
        "verification",
        &truncate_to(&title, 199),
        &body,
        tags,
        "verifier",
        evidence,
    )
    .await;
}

/// Capture a successfully re-verified and requeued conflict resolution.
/// Callers only invoke this after the conflict was resolved and requeued; the
/// outcome detail stays honest when the later integration attempt fails.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn record_integration_resolution_memory(
    engine: &Engine,
    session_id: &str,
    entry_id: &str,
    task_id: Option<&str>,
    commit: &str,
    verifier_summary: &str,
    conflict_detail: &str,
    outcome_detail: &str,
) {
    let task_id = task_id.map(str::trim).filter(|value| !value.is_empty());
    let title = format!("Integration {entry_id} resolution");
    let body = format!(
        "resolved: true\nverifier: {}\ncommit: {}\nconflict: {}\noutcome: {}",
        truncate_to(verifier_summary.trim(), 2_000),
        commit.trim(),
        truncate_to(conflict_detail.trim(), 2_000),
        truncate_to(outcome_detail.trim(), 2_000)
    );
    let mut tags = Vec::new();
    if let Some(task_id) = task_id {
        tags.push(task_id.to_string());
    }
    tags.push("integration".to_string());
    let mut evidence = Vec::new();
    if let Some(task_id) = task_id {
        evidence.push(vp_protocol::types::MemoryEvidence {
            kind: "task".to_string(),
            reference: task_id.to_string(),
            note: None,
        });
    }
    evidence.push(vp_protocol::types::MemoryEvidence {
        kind: "session".to_string(),
        reference: session_id.to_string(),
        note: None,
    });
    evidence.push(vp_protocol::types::MemoryEvidence {
        kind: "commit".to_string(),
        reference: commit.trim().to_string(),
        note: None,
    });
    record_capture_memory(
        engine,
        session_id,
        "orchestrator",
        "Orchestrator",
        "integration",
        &truncate_to(&title, 199),
        &body,
        tags,
        "orchestrator",
        evidence,
    )
    .await;
}

pub(super) fn task_profile(task: &PlanTask, department_profile: AgentProfile) -> AgentProfile {
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

pub(super) async fn ensure_plan_manager(
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
pub(super) fn select_plan_batch(ready: Vec<PlanTask>, limit: usize) -> Vec<PlanTask> {
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

#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_team_plan(
    engine: &Engine,
    config: &AgentConfig,
    session: &Session,
    project: &Project,
    control: &RunControl,
    budget: &SessionBudget,
    leader_spec: &AgentSpec,
    plan: &TeamPlan,
    max_tasks: Option<usize>,
) -> Result<PlanRunResult> {
    let limited = max_tasks.is_some();
    let limit = max_tasks.unwrap_or(usize::MAX).max(1);
    let mut executed = 0usize;
    let mut deferred_by_governor = false;
    let original_worker_cap = budget.worker_cap();
    let _worker_cap_guard = config.auto_expand_workers.then(|| WorkerCapGuard {
        budget,
        original: original_worker_cap,
    });
    let mut manager_specs: std::collections::HashMap<String, AgentSpec> =
        std::collections::HashMap::new();

    let (plan_seq, scheduled) = engine.scheduler().sync_plan(session, plan).await?;
    let mut statuses = scheduled
        .iter()
        .map(|task| (task.task_id.clone(), task.status.clone()))
        .collect::<std::collections::HashMap<_, _>>();
    for task in &plan.tasks {
        statuses
            .entry(task.id.clone())
            .or_insert_with(|| "pending".to_string());
    }
    let completed = statuses
        .iter()
        .filter(|(_, status)| status.as_str() == "completed")
        .map(|(task_id, _)| task_id.clone())
        .collect::<std::collections::HashSet<_>>();
    let skipped = statuses
        .iter()
        .filter(|(_, status)| status.as_str() == "skipped")
        .map(|(task_id, _)| task_id.clone())
        .collect::<std::collections::HashSet<_>>();
    let mut remaining = plan
        .tasks
        .iter()
        .filter(|task| !completed.contains(&task.id) && !skipped.contains(&task.id))
        .cloned()
        .collect::<Vec<_>>();
    let mut reports = plan
        .tasks
        .iter()
        .filter_map(|task| {
            if completed.contains(&task.id) {
                Some(format!(
                    "{} [completed]: already completed; skipped",
                    task.id
                ))
            } else if skipped.contains(&task.id) {
                Some(format!("{} [skipped]: previously skipped", task.id))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    while !remaining.is_empty() {
        if executed >= limit {
            break;
        }
        if control.is_cancelled() {
            if limited {
                return Ok(PlanRunResult::Cancelled);
            }
            for task in &remaining {
                statuses.insert(task.id.clone(), "cancelled".to_string());
                emit(
                    engine,
                    &session.id,
                    leader_spec,
                    "plan_task.finished",
                    serde_json::json!({
                        "task_id": task.id,
                        "task_status": "cancelled",
                    }),
                )
                .await?;
                record_scheduled_task_finish(
                    engine,
                    &session.id,
                    plan_seq,
                    &task.id,
                    "cancelled",
                    None,
                )
                .await?;
            }
            return Ok(PlanRunResult::Cancelled);
        }
        if budget.exhausted() {
            if limited {
                return Ok(PlanRunResult::BudgetExceeded(BudgetStop::session(budget)));
            }
            for task in &remaining {
                statuses.insert(task.id.clone(), "blocked".to_string());
                emit(
                    engine,
                    &session.id,
                    leader_spec,
                    "plan_task.finished",
                    serde_json::json!({
                        "task_id": task.id,
                        "task_status": "blocked",
                        "reason": "token budget exhausted",
                    }),
                )
                .await?;
                record_scheduled_task_finish(
                    engine,
                    &session.id,
                    plan_seq,
                    &task.id,
                    "blocked",
                    Some("token budget exhausted"),
                )
                .await?;
            }
            return Ok(PlanRunResult::BudgetExceeded(BudgetStop::session(budget)));
        }
        let governor_budget = GovernorBudgetView::new(
            budget.limit,
            budget.spent(),
            engine.budget_registry().reserved_for_session(&session.id),
        );
        let selection = engine
            .scheduler()
            .ready_tasks_governed(
                session,
                plan_seq,
                plan,
                plan.tasks.len(),
                Some(governor_budget),
            )
            .await?;
        if let Some(admission) = selection.governor.as_ref() {
            engine
                .record_budget_governor_admission(&session.id, admission)
                .await?;
        }
        let ready = selection.ready;
        if ready.is_empty() {
            if !selection.deferred.is_empty() {
                // Governor deferrals are not dependency failures: leave the
                // tasks queued/retryable and end this batch without blocking
                // them. The next scheduler tick or explicit resume recomputes
                // the reserve from the then-current budget.
                reports.push(format!(
                    "Budget governor deferred {} ready task(s): {}",
                    selection.deferred.len(),
                    selection
                        .deferred
                        .iter()
                        .take(5)
                        .map(|deferral| format!("{} ({})", deferral.task_id, deferral.reason))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
                deferred_by_governor = true;
                break;
            }
            for task in &remaining {
                statuses.insert(task.id.clone(), "blocked".to_string());
                let unmet = task
                    .deps
                    .iter()
                    .filter(|dep| {
                        !matches!(
                            statuses.get(*dep).map(String::as_str),
                            Some("completed" | "skipped")
                        )
                    })
                    .map(|dep| {
                        format!(
                            "{} ({})",
                            dep,
                            statuses.get(dep).map(String::as_str).unwrap_or("pending")
                        )
                    })
                    .collect::<Vec<_>>();
                let reason = if unmet.is_empty() {
                    "dependency not completed".to_string()
                } else {
                    format!("waiting on {}", unmet.join(", "))
                };
                emit(
                    engine,
                    &session.id,
                    leader_spec,
                    "plan_task.finished",
                    serde_json::json!({
                        "task_id": task.id,
                        "task_status": "blocked",
                        "reason": reason,
                    }),
                )
                .await?;
                record_scheduled_task_finish(
                    engine,
                    &session.id,
                    plan_seq,
                    &task.id,
                    "blocked",
                    Some(&reason),
                )
                .await?;
            }
            break;
        }
        if !limited
            && config.auto_expand_workers
            && !budget.scaling_managed()
            && ready.len() > budget.worker_cap()
        {
            let target = ready.len().min(HARD_WORKER_CAP);
            if budget.can_expand_workers() {
                budget.expand_workers(target);
                emit(
                    engine,
                    &session.id,
                    leader_spec,
                    "plan.auto_expand",
                    serde_json::json!({
                        "ready_tasks": ready.len(),
                        "worker_cap": budget.worker_cap(),
                    }),
                )
                .await?;
            }
        }
        let capacity = budget
            .available(AgentRole::Worker)
            .min(limit.saturating_sub(executed));
        if capacity == 0 {
            if limited {
                break;
            }
            for task in &remaining {
                statuses.insert(task.id.clone(), "blocked".to_string());
                emit(
                    engine,
                    &session.id,
                    leader_spec,
                    "plan_task.finished",
                    serde_json::json!({
                        "task_id": task.id,
                        "task_status": "blocked",
                        "reason": "Worker cap is fully occupied",
                    }),
                )
                .await?;
                record_scheduled_task_finish(
                    engine,
                    &session.id,
                    plan_seq,
                    &task.id,
                    "blocked",
                    Some("Worker cap is fully occupied"),
                )
                .await?;
            }
            break;
        }
        let batch = select_plan_batch(ready, capacity);
        let task_ids = batch.iter().map(|task| task.id.clone()).collect::<Vec<_>>();
        let (claimed, claim_events) = engine
            .scheduler()
            .claim(&session.id, plan_seq, &task_ids)
            .await?;
        for event in claim_events {
            engine.broadcast_event(event);
        }
        if claimed.is_empty() {
            break;
        }
        let batch_ids = claimed
            .iter()
            .map(|task| task.task_id.clone())
            .collect::<std::collections::HashSet<_>>();
        let batch = batch
            .into_iter()
            .filter(|task| batch_ids.contains(&task.id))
            .collect::<Vec<_>>();

        let mut prepared = Vec::new();
        let mut worker_slots = Vec::new();
        let plan_results = latest_plan_results(engine, &session.id).await?;
        for task in &batch {
            let manager_spec = if let Some(spec) = manager_specs.get(&task.department) {
                spec.clone()
            } else {
                let department = plan
                    .departments
                    .iter()
                    .find(|department| department.name == task.department)
                    .ok_or_else(|| anyhow!("missing department for task {}", task.department))?;
                let spec =
                    ensure_plan_manager(engine, config, session, budget, leader_spec, department)
                        .await?;
                manager_specs.insert(task.department.clone(), spec.clone());
                spec
            };
            let profile = task_profile(task, manager_spec.profile);
            let acceptance = task.acceptance.join("; ");
            let write_scope = task
                .write_scope
                .iter()
                .map(|scope| normalize_scope(scope))
                .collect::<Vec<_>>();
            let worker_name = format!("{} · {}", task.department, truncate_to(&task.title, 48));
            let Some((_ordinal, slot)) = budget.reserve_slot(AgentRole::Worker) else {
                return Err(anyhow!("the Worker cap for this session has been reached"));
            };
            worker_slots.push(slot);
            let worker = engine
                .create_agent(
                    &session.id,
                    AgentRole::Worker.as_str(),
                    &worker_name,
                    None,
                    Some(manager_spec.id.clone()),
                    &model_for(config, AgentRole::Worker),
                )
                .await?;
            engine
                .set_agent_profile(&worker.id, profile.as_str())
                .await?;
            engine
                .set_agent_task(
                    &worker.id,
                    Some(task.id.clone()),
                    Some(task.department.clone()),
                )
                .await?;
            let workspace_binding = engine
                .attach_worker_workspace_for_run(&session.id, None, Some(task.department.as_str()))
                .await?;
            let (worker_identity_id, worker_identity_generation) = match workspace_binding.as_ref()
            {
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
                        Some(task.department.as_str()),
                    )
                    .await?
                    .map(|identity| (identity.id, identity.generation))
                    .map(|(id, generation)| (Some(id), Some(generation)))
                    .unwrap_or((None, None)),
            };
            let (info, workspace_id, session_workspace_id) = match workspace_binding {
                Some(binding) => {
                    let head_path = PathBuf::from(&binding.session.worktree_path);
                    let head =
                        tokio::task::spawn_blocking(move || worktree::head_commit(&head_path))
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
                            &task.id,
                            &manager_spec.session_branch,
                        )
                        .await?,
                    None,
                    None,
                ),
            };
            engine
                .set_agent_worktree(&worker.id, &info.path.to_string_lossy(), &info.branch)
                .await?;
            engine
                .set_agent_workspace_binding(&worker.id, workspace_id.as_deref(), None)
                .await?;
            let worker_spec = AgentSpec {
                id: worker.id.clone(),
                role: AgentRole::Worker,
                name: worker_name,
                depth: manager_spec.depth.saturating_add(1),
                parent_name: Some(manager_spec.name.clone()),
                title: None,
                task_id: Some(task.id.clone()),
                department: Some(task.department.clone()),
                identity_id: worker_identity_id,
                identity_generation: worker_identity_generation,
                profile,
                allow_delegation: true,
                // The plan task kind is the deterministic bundle selector for
                // this Worker. It is frozen for the run, including retries.
                task_kind: Some(task.kind.clone()),
                auto_approve: false,
                worktree: info.path,
                branch: info.branch,
                session_worktree: manager_spec.session_worktree.clone(),
                session_branch: manager_spec.session_branch.clone(),
                write_scope,
                base_commit: info.base_commit,
                workspace_id,
                session_workspace_id,
                parent_workspace_id: None,
            };
            emit(
                engine,
                &session.id,
                &worker_spec,
                "agent.spawned",
                serde_json::json!({
                    "parent_id": manager_spec.id,
                    "parent_name": manager_spec.name,
                    "objective": task.title,
                    "plan_task_id": task.id,
                    "department": task.department,
                }),
            )
            .await?;
            statuses.insert(task.id.clone(), "running".to_string());
            let dependency_brief = task_dependency_brief(task, &statuses, &plan_results);
            let coordination = coordination_brief_for_agent(
                engine,
                &session.id,
                &worker_spec.id,
                Some(&task.id),
                6,
            )
            .await?;
            let mailbox = mailbox_brief(engine, &session.id, &worker_spec.id, 4).await?;
            let memory_brief =
                project_memory_brief(engine, &session.id, Some(&task.id), Some(&task.department))
                    .await
                    .unwrap_or_default();
            let memory_note = if memory_brief.is_empty() {
                String::new()
            } else {
                format!("\nRelevant long-term memory:\n{memory_brief}")
            };
            let coordination_note = if coordination.is_empty() {
                String::new()
            } else {
                format!("\nRecent coordination for this task:\n{coordination}")
            };
            let mailbox_note = if mailbox.is_empty() {
                String::new()
            } else {
                format!("\nUnread mailbox:\n{mailbox}")
            };
            let objective = format!(
                "Execute plan task {}: {}. Department: {} ({}). Acceptance: {}. \
                 Upstream handoffs:\n{}\n{}{}{}\
                 Work only in your task worktree and return a concise summary.",
                task.id,
                task.title,
                task.department,
                manager_spec.title.as_deref().unwrap_or(&manager_spec.name),
                acceptance,
                dependency_brief,
                memory_note,
                coordination_note,
                mailbox_note
            );
            prepared.push((
                task.clone(),
                worker_spec,
                manager_spec,
                objective.clone(),
                acceptance,
                vec![ChatMessage::user(objective)],
            ));
        }

        let futures = prepared
            .iter_mut()
            .map(|(_, worker_spec, _, objective, _, conversation)| {
                let workspace_lock = worker_spec
                    .workspace_id
                    .as_ref()
                    .map(|workspace_id| engine.workspace_run_lock(workspace_id));
                async move {
                    let _guard = match workspace_lock {
                        Some(lock) => Some(lock.lock_owned().await),
                        None => None,
                    };
                    refresh_workspace_base(worker_spec).await;
                    run_loop(
                        engine,
                        config,
                        session,
                        project,
                        control,
                        budget,
                        worker_spec,
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

        let mut abort: Option<PlanAbort> = None;
        for (index, result) in results.into_iter().enumerate() {
            let (task, worker_spec, manager_spec, objective, acceptance, _) = &prepared[index];
            if let Some(abort) = &abort {
                let status = abort.task_status();
                let reason = abort.reason();
                engine
                    .update_agent_status(
                        &worker_spec.id,
                        if status == "cancelled" {
                            "cancelled"
                        } else {
                            "failed"
                        },
                    )
                    .await?;
                statuses.insert(task.id.clone(), status.to_string());
                emit_plan_task_finished(
                    engine,
                    &session.id,
                    worker_spec,
                    &task.id,
                    status,
                    Some(&reason),
                )
                .await?;
                record_scheduled_task_finish(
                    engine,
                    &session.id,
                    plan_seq,
                    &task.id,
                    status,
                    Some(&reason),
                )
                .await?;
                reports.push(format!("{} [{}]: {}", task.id, status, reason));
                continue;
            }
            match result {
                Ok(LoopOutcome::Final(summary)) => {
                    let summary = if summary.trim().is_empty() {
                        "The worker completed without a summary.".to_string()
                    } else {
                        summary
                    };
                    if !control.is_cancelled()
                        && engine
                            .plan_task_cancel_requested(&session.id, &task.id)
                            .await?
                    {
                        engine
                            .update_agent_status(&worker_spec.id, "cancelled")
                            .await?;
                        statuses.insert(task.id.clone(), "cancelled".to_string());
                        emit(
                            engine,
                            &session.id,
                            worker_spec,
                            "plan_task.finished",
                            serde_json::json!({
                                "task_id": task.id,
                                "task_status": "cancelled",
                                "reason": "cancel requested by user",
                            }),
                        )
                        .await?;
                        record_scheduled_task_finish(
                            engine,
                            &session.id,
                            plan_seq,
                            &task.id,
                            "cancelled",
                            Some("cancel requested by user"),
                        )
                        .await?;
                        reports.push(format!("{} [cancelled]: cancel requested", task.id));
                        continue;
                    }
                    match finish_worker(
                        engine,
                        config,
                        session,
                        project,
                        control,
                        budget,
                        manager_spec,
                        worker_spec,
                        objective,
                        &summary,
                        Some(acceptance),
                    )
                    .await?
                    {
                        WorkerFinish::Done { report, success } => {
                            let status = if success { "completed" } else { "failed" };
                            statuses.insert(task.id.clone(), status.to_string());
                            emit(
                                engine,
                                &session.id,
                                worker_spec,
                                "plan_task.finished",
                                serde_json::json!({
                                    "task_id": task.id,
                                    "task_status": status,
                                    "summary": truncate(&report),
                                }),
                            )
                            .await?;
                            record_scheduled_task_finish(
                                engine,
                                &session.id,
                                plan_seq,
                                &task.id,
                                status,
                                Some(report.as_str()),
                            )
                            .await?;
                            reports.push(format!(
                                "{} [{}]: {}",
                                task.id,
                                status,
                                truncate(&report)
                            ));
                        }
                        WorkerFinish::Cancelled => {
                            statuses.insert(task.id.clone(), "cancelled".to_string());
                            emit(
                                engine,
                                &session.id,
                                worker_spec,
                                "plan_task.finished",
                                serde_json::json!({
                                    "task_id": task.id,
                                    "task_status": "cancelled",
                                }),
                            )
                            .await?;
                            record_scheduled_task_finish(
                                engine,
                                &session.id,
                                plan_seq,
                                &task.id,
                                "cancelled",
                                None,
                            )
                            .await?;
                            abort = Some(PlanAbort::Cancelled);
                        }
                        WorkerFinish::BudgetExceeded(stop) => {
                            statuses.insert(task.id.clone(), "blocked".to_string());
                            emit(
                                engine,
                                &session.id,
                                worker_spec,
                                "plan_task.finished",
                                serde_json::json!({
                                    "task_id": task.id,
                                    "task_status": "blocked",
                                    "reason": stop.summary(),
                                }),
                            )
                            .await?;
                            record_scheduled_task_finish(
                                engine,
                                &session.id,
                                plan_seq,
                                &task.id,
                                "blocked",
                                Some(&stop.summary()),
                            )
                            .await?;
                            abort = Some(PlanAbort::Budget(stop));
                        }
                    }
                }
                Ok(LoopOutcome::Deferred(wait)) => {
                    engine.update_agent_status(&worker_spec.id, "idle").await?;
                    statuses.insert(task.id.clone(), "blocked".to_string());
                    emit(
                        engine,
                        &session.id,
                        worker_spec,
                        "plan_task.finished",
                        serde_json::json!({
                            "task_id": task.id,
                            "task_status": "blocked",
                            "gate_id": wait.gate_id,
                            "continuation_id": wait.continuation_id,
                            "reason": "waiting for external gate",
                        }),
                    )
                    .await?;
                    record_scheduled_task_finish(
                        engine,
                        &session.id,
                        plan_seq,
                        &task.id,
                        "blocked",
                        Some("waiting for external gate"),
                    )
                    .await?;
                    reports.push(format!(
                        "{} [blocked]: waiting for external gate {}",
                        task.id, wait.gate_id
                    ));
                    abort = Some(PlanAbort::Deferred(wait));
                }
                Ok(LoopOutcome::Cancelled) => {
                    let task_cancel = !control.is_cancelled()
                        && engine
                            .plan_task_cancel_requested(&session.id, &task.id)
                            .await?;
                    engine
                        .update_agent_status(&worker_spec.id, "cancelled")
                        .await?;
                    statuses.insert(task.id.clone(), "cancelled".to_string());
                    emit(
                        engine,
                        &session.id,
                        worker_spec,
                        "plan_task.finished",
                        serde_json::json!({
                            "task_id": task.id,
                            "task_status": "cancelled",
                            "reason": if task_cancel { "cancel requested by user" } else { "session cancelled" },
                        }),
                    )
                    .await?;
                    record_scheduled_task_finish(
                        engine,
                        &session.id,
                        plan_seq,
                        &task.id,
                        "cancelled",
                        task_cancel.then_some("cancel requested by user"),
                    )
                    .await?;
                    if task_cancel {
                        reports.push(format!("{} [cancelled]: cancel requested", task.id));
                    } else {
                        abort = Some(PlanAbort::Cancelled);
                    }
                }
                Ok(LoopOutcome::BudgetExceeded(stop)) => {
                    engine
                        .update_agent_status(&worker_spec.id, "failed")
                        .await?;
                    statuses.insert(task.id.clone(), "blocked".to_string());
                    emit(
                        engine,
                        &session.id,
                        worker_spec,
                        "plan_task.finished",
                        serde_json::json!({
                            "task_id": task.id,
                            "task_status": "blocked",
                            "reason": stop.summary(),
                        }),
                    )
                    .await?;
                    record_scheduled_task_finish(
                        engine,
                        &session.id,
                        plan_seq,
                        &task.id,
                        "blocked",
                        Some(&stop.summary()),
                    )
                    .await?;
                    if stop.is_abortive() {
                        abort = Some(PlanAbort::Budget(stop));
                    }
                }
                Ok(LoopOutcome::Verdict(_)) => {}
                Err(error) => {
                    engine
                        .update_agent_status(&worker_spec.id, "failed")
                        .await?;
                    statuses.insert(task.id.clone(), "failed".to_string());
                    emit(
                        engine,
                        &session.id,
                        worker_spec,
                        "plan_task.finished",
                        serde_json::json!({
                            "task_id": task.id,
                            "task_status": "failed",
                            "summary": truncate(error.to_string().as_str()),
                        }),
                    )
                    .await?;
                    record_scheduled_task_finish(
                        engine,
                        &session.id,
                        plan_seq,
                        &task.id,
                        "failed",
                        Some(&error.to_string()),
                    )
                    .await?;
                    reports.push(format!("{} [failed]: {error}", task.id));
                }
            }
        }

        if let Some(abort) = abort {
            if limited {
                return Ok(match abort {
                    PlanAbort::Cancelled => PlanRunResult::Cancelled,
                    PlanAbort::Budget(stop) => PlanRunResult::BudgetExceeded(stop),
                    PlanAbort::Deferred(wait) => PlanRunResult::Deferred(wait),
                });
            }
            let status = abort.task_status();
            let reason = abort.reason();
            for task in remaining
                .iter()
                .filter(|task| !batch_ids.contains(&task.id))
            {
                statuses.insert(task.id.clone(), status.to_string());
                emit_plan_task_finished(
                    engine,
                    &session.id,
                    leader_spec,
                    &task.id,
                    status,
                    Some(&reason),
                )
                .await?;
                record_scheduled_task_finish(
                    engine,
                    &session.id,
                    plan_seq,
                    &task.id,
                    status,
                    Some(&reason),
                )
                .await?;
            }
            return Ok(match abort {
                PlanAbort::Cancelled => PlanRunResult::Cancelled,
                PlanAbort::Budget(stop) => PlanRunResult::BudgetExceeded(stop),
                PlanAbort::Deferred(wait) => PlanRunResult::Deferred(wait),
            });
        }

        executed = executed.saturating_add(batch.len());
        remaining.retain(|task| !batch_ids.contains(&task.id));
    }

    if limited {
        if executed == 0 {
            if deferred_by_governor {
                return Ok(PlanRunResult::Done(
                    reports
                        .last()
                        .cloned()
                        .unwrap_or_else(|| "No ready task was dispatched.".to_string()),
                ));
            }
            return Ok(PlanRunResult::Done(
                "No ready task was dispatched.".to_string(),
            ));
        }
        let summary = reports
            .last()
            .cloned()
            .unwrap_or_else(|| "Task dispatch finished.".to_string());
        return Ok(PlanRunResult::Done(summary));
    }

    let completed = statuses
        .values()
        .filter(|status| status.as_str() == "completed")
        .count();
    let failed = statuses
        .values()
        .filter(|status| {
            matches!(
                status.as_str(),
                "failed" | "blocked" | "cancelled" | "skipped"
            )
        })
        .count();
    reports.push(format!(
        "Plan complete: {completed} completed, {failed} not completed."
    ));
    Ok(PlanRunResult::Done(reports.join("\n")))
}

pub(super) fn parse_verdict(input: &serde_json::Value) -> Result<VerifierVerdict> {
    let approved = input
        .get("approved")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| anyhow!("approved is required"))?;
    let summary = input
        .get("summary")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|summary| !summary.is_empty() && summary.len() <= 2000)
        .ok_or_else(|| anyhow!("summary is required (max 2000 chars)"))?
        .to_string();
    Ok(VerifierVerdict { approved, summary })
}

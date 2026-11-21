use super::*;

pub(in crate::agent) async fn run_plan_resume(
    engine: Engine,
    config: AgentConfig,
    session_id: &str,
    control: Arc<RunControl>,
    max_tasks: Option<usize>,
) -> Result<()> {
    let store = engine.store().clone();
    let (_, project) = load_session_project(store, session_id).await?;
    let session = engine.ensure_session_worktree(session_id).await?;
    let plan = engine
        .get_team_plan(session_id)
        .await?
        .ok_or_else(|| anyhow!("session has no TeamPlan"))?;
    let worktree = PathBuf::from(
        session
            .worktree
            .clone()
            .ok_or_else(|| anyhow!("session has no worktree"))?,
    );
    let session_branch = session.branch.clone().unwrap_or_default();
    let leader = engine
        .list_agents(session_id)
        .await?
        .into_iter()
        .find(|agent| agent.role == "leader");
    let leader_id = leader.as_ref().map(|agent| agent.id.clone());
    let mut spec = AgentSpec {
        id: leader_id.clone().unwrap_or_else(|| "leader".to_string()),
        role: AgentRole::Leader,
        name: leader
            .map(|agent| agent.name)
            .unwrap_or_else(|| "Leader".to_string()),
        depth: 0,
        parent_name: None,
        title: None,
        task_id: None,
        department: None,
        identity_id: None,
        identity_generation: None,
        profile: AgentProfile::Generic,
        allow_delegation: true,
        task_kind: None,
        auto_approve: false,
        worktree: worktree.clone(),
        branch: session_branch.clone(),
        session_worktree: worktree.clone(),
        session_branch,
        write_scope: Vec::new(),
        base_commit: session.base_commit.clone().unwrap_or_default(),

        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
    };
    let (identity_id, identity_generation) =
        match resolve_run_identity(&engine, &session, &spec).await {
            Ok(identity) => identity,
            Err(error) => {
                let _ = control
                    .finalize_session(&engine, session_id, "failed")
                    .await;
                return Err(error);
            }
        };
    spec.identity_id = identity_id;
    spec.identity_generation = identity_generation;
    if let Some(identity_id) = &spec.identity_id
        && let Err(error) = engine
            .set_agent_identity_binding(&spec.id, Some(identity_id))
            .await
    {
        tracing::warn!(%error, agent_id = %spec.id, "failed to persist run identity binding");
    }
    let budget_status = engine.session_budget(&session.id).await?;
    let auto_runtime = AutoBudgetRuntime::from_status(&session, &budget_status);
    let mut budget = SessionBudget::new(
        session.token_budget,
        session.tokens_in + session.tokens_out,
        AgentCaps {
            managers: config.max_managers,
            workers: config.max_workers,
            labour: config.max_labour,
        },
    )
    .with_cost(session.cost_budget_usd, session.cost_spent_usd)
    .with_time(session.time_budget_secs)
    .with_unlimited_flag(engine.unlimited_flag(&session.id, session.unlimited))
    .with_scaling_slots(engine.scaling_slots(
        &session.id,
        &session.project_id,
        config.max_workers,
        config.max_labour,
    ));
    if auto_runtime.is_some() {
        let elapsed_secs = budget_status
            .state
            .as_ref()
            .map(|state| state.elapsed_secs)
            .unwrap_or(0);
        budget = budget.with_cumulative_elapsed(elapsed_secs);
        budget.apply_status(&budget_status);
        budget.set_remaining_time(budget_status.remaining_secs);
    }
    control.arm_preemption_deadline(
        budget.wall_clock_deadline(),
        engine.get_preemption_policy(&session.project_id).await?,
    );
    if budget.exhausted() {
        emit(
            &engine,
            session_id,
            &spec,
            "agent.budget_exceeded",
            serde_json::json!({
                "reason": budget.exceeded_reason().unwrap_or("tokens"),
                "limit": session.token_budget,
                "spent": budget.spent(),
                "cost_limit_usd": session.cost_budget_usd,
                "cost_spent_usd": budget.cost_spent(),
                "time_budget_secs": session.time_budget_secs,
                "elapsed_secs": budget.elapsed_secs(),
            }),
        )
        .await?;
        control
            .finalize_session(&engine, session_id, "failed")
            .await?;
        return Ok(());
    }
    if let Err(error) =
        crate::run_journal::supersede_unresolved_runs(&engine, session_id, "plan-resume").await
    {
        control
            .finalize_session(&engine, session_id, "failed")
            .await?;
        return Err(error);
    }
    if let Some(agent_id) = &leader_id {
        engine.update_agent_status(agent_id, "running").await?;
    }
    let event_kind = if max_tasks.is_some() {
        "agent.task_dispatched"
    } else {
        "agent.plan_resumed"
    };
    emit(
        &engine,
        session_id,
        &spec,
        event_kind,
        serde_json::json!({
            "goal": plan.goal,
            "model": model_for(&config, spec.role),
            "provider": config.provider_name,
            "worktree": session.worktree,
            "branch": session.branch,
            "task_limit": max_tasks,
        }),
    )
    .await?;

    let result = execute_team_plan(
        &engine, &config, &session, &project, &control, &budget, &spec, &plan, max_tasks,
    )
    .await;
    match result? {
        PlanRunResult::Done(summary) => {
            if let Some(agent_id) = &leader_id {
                engine.update_agent_status(agent_id, "idle").await?;
            }
            emit(
                &engine,
                session_id,
                &spec,
                "agent.message",
                serde_json::json!({"text": summary}),
            )
            .await?;
            control
                .finalize_session(&engine, session_id, "idle")
                .await?;
        }
        PlanRunResult::Cancelled => {
            if let Some(agent_id) = &leader_id {
                engine.update_agent_status(agent_id, "cancelled").await?;
            }
            if let Some(snapshot) = control.preempt_report() {
                emit(
                    &engine,
                    session_id,
                    &spec,
                    "agent.cancelled",
                    serde_json::json!({
                        "resumed_plan": true,
                        "preempted": true,
                        "reason": snapshot.reason.as_str(),
                        "phase": snapshot.phase.as_str(),
                    }),
                )
                .await?;
                record_terminal_preemption(
                    &engine,
                    session_id,
                    &project.id,
                    &spec,
                    &control,
                    &snapshot,
                )
                .await?;
            } else {
                emit(
                    &engine,
                    session_id,
                    &spec,
                    "agent.cancelled",
                    serde_json::json!({"resumed_plan": true}),
                )
                .await?;
                control
                    .finalize_session(&engine, session_id, "cancelled")
                    .await?;
            }
        }
        PlanRunResult::Deferred(wait) => {
            if let Some(agent_id) = &leader_id {
                engine.update_agent_status(agent_id, "idle").await?;
            }
            emit(
                &engine,
                session_id,
                &spec,
                "agent.deferred",
                serde_json::json!({
                    "gate_id": wait.gate_id,
                    "continuation_id": wait.continuation_id,
                    "summary": truncate(&wait.summary),
                    "reason": "external_gate",
                    "resumed_plan": true,
                }),
            )
            .await?;
            control
                .finalize_session(&engine, session_id, "idle")
                .await?;
        }
        PlanRunResult::BudgetExceeded(stop) => {
            if let Some(agent_id) = &leader_id {
                engine.update_agent_status(agent_id, "failed").await?;
            }
            let mut payload = stop.payload();
            if let Some(object) = payload.as_object_mut() {
                object.insert(
                    "time_budget_secs".into(),
                    serde_json::json!(session.time_budget_secs),
                );
            }
            emit(&engine, session_id, &spec, "agent.budget_exceeded", payload).await?;
            control
                .finalize_session(&engine, session_id, "failed")
                .await?;
        }
    }
    Ok(())
}

use super::*;

pub(in crate::agent) async fn run_agent(
    engine: Engine,
    config: AgentConfig,
    session_id: &str,
    prompt: &str,
    control: Arc<RunControl>,
    resume_journal: Option<crate::run_journal::RunJournal>,
) -> Result<()> {
    let store = engine.store().clone();
    let (_, project) = load_session_project(store.clone(), session_id).await?;
    let session = engine.ensure_session_worktree(session_id).await?;
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
    let budget = SessionBudget::new(
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
    control.arm_preemption_deadline(
        budget.wall_clock_deadline(),
        engine.get_preemption_policy(&session.project_id).await?,
    );
    let load_from_events = resume_journal.is_some();
    let journal = match resume_journal {
        Some(journal) => journal,
        None => {
            crate::run_journal::RunJournal::begin(
                &engine,
                &session,
                &project.id,
                &spec.id,
                &spec.name,
                spec.role.as_str(),
                spec.task_id.as_deref(),
                spec.department.as_deref(),
                prompt,
                &config.provider_name,
                &model_for(&config, spec.role),
                &worktree,
                budget.run_snapshot(),
                true,
            )
            .await?
        }
    };
    if budget.exhausted() {
        if let Err(error) = journal
            .finish_failed(
                &engine,
                "budget_exceeded",
                budget
                    .exceeded_reason()
                    .unwrap_or("session budget exhausted"),
            )
            .await
        {
            tracing::warn!(%error, "failed to finish a budget-exhausted run journal");
        }
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
        engine.update_session_status(session_id, "failed").await?;
        return Ok(());
    }
    if !load_from_events
        && let Err(error) =
            crate::run_journal::supersede_unresolved_runs(&engine, session_id, journal.id()).await
    {
        let _ = journal
            .finish_failed(
                &engine,
                "error",
                &format!("could not supersede older unresolved runs: {error:#}"),
            )
            .await;
        return Err(error);
    }
    if let Some(agent_id) = &leader_id
        && let Err(error) = engine.update_agent_status(agent_id, "running").await
    {
        let _ = journal
            .finish_failed(
                &engine,
                "error",
                &format!("could not mark the agent running: {error:#}"),
            )
            .await;
        return Err(error);
    }

    let (identity_id, identity_generation) =
        match resolve_run_identity(&engine, &session, &spec).await {
            Ok(identity) => identity,
            Err(error) => {
                let _ = journal
                    .finish_failed(
                        &engine,
                        "error",
                        &format!("could not resolve run identity: {error:#}"),
                    )
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
    if let Err(error) = emit(
        &engine,
        session_id,
        &spec,
        "agent.started",
        serde_json::json!({
            "goal": session.goal,
            "prompt": prompt,
            "model": model_for(&config, spec.role),
            "provider": config.provider_name,
            "worktree": session.worktree,
            "branch": session.branch,
        }),
    )
    .await
    {
        let _ = journal
            .finish_failed(
                &engine,
                "error",
                &format!("failed to record agent.started: {error:#}"),
            )
            .await;
        return Err(error);
    }

    let loaded = if load_from_events {
        let (mut messages, ids) = match engine.conversation_from_events(session_id).await {
            Ok(loaded) => loaded,
            Err(error) => {
                if let Err(finish_error) = journal
                    .finish_failed(
                        &engine,
                        "journal_unreadable",
                        &format!("could not reconstruct the resumed conversation: {error:#}"),
                    )
                    .await
                {
                    tracing::warn!(%finish_error, "failed to finish an unreadable run journal");
                }
                return Err(error);
            }
        };
        if let Err(error) = journal
            .repair_resumed_conversation(&engine, &mut messages)
            .await
        {
            if let Err(finish_error) = journal
                .finish_failed(
                    &engine,
                    "journal_unreadable",
                    &format!("could not repair the resumed conversation: {error:#}"),
                )
                .await
            {
                tracing::warn!(%finish_error, "failed to finish an unreadable run journal");
            }
            return Err(error);
        }
        LoadedConversation { messages, ids }
    } else {
        match load_conversation(&engine, session_id).await {
            Ok(loaded) => loaded,
            Err(error) => {
                let _ = journal
                    .finish_failed(
                        &engine,
                        "error",
                        &format!("could not load the persisted conversation: {error:#}"),
                    )
                    .await;
                return Err(error);
            }
        }
    };
    let mut conversation = loaded.messages;
    let mut conversation_ids = loaded.ids;
    let result = run_loop(
        &engine,
        &config,
        &session,
        &project,
        &control,
        &budget,
        &spec,
        prompt,
        &mut conversation,
        Some(&mut conversation_ids),
        true,
        Some(journal),
    )
    .await;
    if result.is_err()
        && let Some(agent_id) = &leader_id
    {
        let _ = engine.update_agent_status(agent_id, "failed").await;
    }
    match result? {
        LoopOutcome::Final(_) => {
            if let Some(agent_id) = &leader_id {
                engine.update_agent_status(agent_id, "idle").await?;
            }
            control
                .finalize_session(&engine, session_id, "idle")
                .await?;
        }
        LoopOutcome::Deferred(wait) => {
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
                }),
            )
            .await?;
            // The scheduler delivers the continuation through the normal
            // prompt path when the gate settles. The session is idle meanwhile.
            control
                .finalize_session(&engine, session_id, "idle")
                .await?;
        }
        LoopOutcome::Cancelled => {
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
                engine
                    .append_event(session_id, "agent.cancelled", serde_json::json!({}))
                    .await?;
                control
                    .finalize_session(&engine, session_id, "cancelled")
                    .await?;
            }
        }
        LoopOutcome::BudgetExceeded(stop) => {
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
        LoopOutcome::Verdict(_) => {
            emit(
                &engine,
                session_id,
                &spec,
                "agent.failed",
                serde_json::json!({"error": "a verdict was submitted outside a verification run"}),
            )
            .await?;
            control
                .finalize_session(&engine, session_id, "failed")
                .await?;
        }
    }
    Ok(())
}

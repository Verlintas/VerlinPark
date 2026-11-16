use super::*;

pub(super) async fn run_plan_resume(
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

/// Record the terminal preemption after the session is quiescent. The caller
/// must finalize the session status first: checkpoint creation refuses a
/// running/paused session. A checkpoint failure is logged and never changes the
/// cancelled status, never merges incomplete work, and never hides the event.
pub(super) async fn record_terminal_preemption(
    engine: &Engine,
    session_id: &str,
    project_id: &str,
    spec: &AgentSpec,
    control: &RunControl,
    snapshot: &crate::preemption::Snapshot,
) -> Result<()> {
    let policy = control.preemption_policy();
    let mut checkpoint_id = None;
    if policy.checkpoint_on_preempt {
        let label = format!(
            "preempted: {} during {}",
            snapshot.reason.as_str(),
            snapshot.phase.as_str()
        );
        match timeout(
            Duration::from_secs(60),
            engine.checkpoint_session_at_preemption(session_id, Some(label)),
        )
        .await
        {
            Ok(Ok(checkpoint)) => checkpoint_id = Some(checkpoint.id),
            Ok(Err(error)) => {
                tracing::warn!(%error, session_id, "preemption checkpoint was not created");
            }
            Err(_) => {
                tracing::warn!(
                    session_id,
                    "preemption checkpoint exceeded its 60s bound and was skipped"
                );
            }
        }
    }
    let attribution = crate::preemption::PreemptAttribution {
        session_id,
        project_id,
        agent_id: &spec.id,
        agent_name: &spec.name,
        agent_role: spec.role.as_str(),
        task_id: spec.task_id.as_deref(),
        department: spec.department.as_deref(),
    };
    let mut task_checkpoint_commit = None;
    if let Some(task_id) = spec.task_id.as_deref()
        && matches!(spec.role, AgentRole::Worker | AgentRole::Labour)
    {
        let label = format!(
            "preempted: {} during {}",
            snapshot.reason.as_str(),
            snapshot.phase.as_str()
        );
        match timeout(
            Duration::from_secs(30),
            engine.snapshot_preempted_task_worktree(session_id, task_id, &spec.worktree, &label),
        )
        .await
        {
            Ok(Ok(commit)) => task_checkpoint_commit = Some(commit),
            Ok(Err(error)) => {
                tracing::warn!(
                    %error,
                    session_id,
                    task_id,
                    "preempted task worktree snapshot was not created"
                );
            }
            Err(_) => {
                tracing::warn!(
                    session_id,
                    task_id,
                    "preempted task worktree snapshot exceeded its 30s bound and was skipped"
                );
            }
        }
    }
    engine
        .append_event(
            session_id,
            "session.preempted",
            serde_json::to_value(crate::preemption::record(
                &attribution,
                snapshot,
                true,
                checkpoint_id,
                task_checkpoint_commit,
                None,
            ))?,
        )
        .await?;
    control
        .finalize_session(engine, session_id, "cancelled")
        .await?;
    Ok(())
}

/// Record a preemption that happened before the run loop started (queued for
/// admission or a global slot). No worktree changes exist yet, so this records
/// the terminal event without a checkpoint.
pub(super) async fn record_queued_preemption(
    engine: &Engine,
    session_id: &str,
    project_id: &str,
    control: &RunControl,
    snapshot: &crate::preemption::Snapshot,
) -> Result<()> {
    let attribution = crate::preemption::PreemptAttribution {
        session_id,
        project_id,
        agent_id: "leader",
        agent_name: "Leader",
        agent_role: "leader",
        task_id: None,
        department: None,
    };
    engine
        .append_event(
            session_id,
            "session.preempted",
            serde_json::to_value(crate::preemption::record(
                &attribution,
                snapshot,
                true,
                None,
                None,
                Some("run preempted while waiting for admission".to_string()),
            ))?,
        )
        .await?;
    control
        .finalize_session(engine, session_id, "cancelled")
        .await?;
    Ok(())
}

/// Resolve the project identity for a top-level run once, before the first
/// event is emitted. The resolved id/generation are observational provenance;
/// they do not grant authority or change any runtime gate.
pub(super) async fn resolve_run_identity(
    engine: &Engine,
    session: &Session,
    spec: &AgentSpec,
) -> Result<(Option<String>, Option<u64>)> {
    Ok(
        match engine
            .prompt_identity_for_run(
                &session.id,
                spec.role.as_str(),
                spec.title.as_deref(),
                spec.department.as_deref(),
            )
            .await?
        {
            Some(identity) => (Some(identity.id), Some(identity.generation)),
            None => (spec.identity_id.clone(), spec.identity_generation),
        },
    )
}

pub(super) async fn run_agent(
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

pub(super) async fn emit_scope_budget_stop(
    engine: &Engine,
    session_id: &str,
    spec: &AgentSpec,
    stop: &BudgetStop,
) -> Result<()> {
    emit(
        engine,
        session_id,
        spec,
        "agent.budget_exceeded",
        stop.payload(),
    )
    .await
}

pub(super) struct GovernorDeferralEvent<'a> {
    pub(super) engine: &'a Engine,
    pub(super) session: &'a Session,
    pub(super) spec: &'a AgentSpec,
    pub(super) context: Option<&'a GovernorRunContext>,
    pub(super) gate: Option<&'a GovernorGate>,
    pub(super) reason: BudgetReason,
    pub(super) requested_tokens: u64,
    pub(super) remaining_tokens: Option<u64>,
    pub(super) limit_tokens: Option<u64>,
    pub(super) spent_tokens: u64,
    pub(super) reserved_tokens: u64,
}

/// Persist the provider-call deferral that the budget governor rejected. This
/// is the bridge between the in-flight reservation registry and the event-backed
/// decision history; the task itself remains retryable/queued.
pub(super) async fn emit_governor_deferral(input: GovernorDeferralEvent<'_>) -> Result<()> {
    let reason = match input.reason {
        BudgetReason::Throttled => GovernorReason::Throttle,
        _ => GovernorReason::Reserve,
    };
    let now = vp_protocol::now_rfc3339();
    let decision_id = format!("gov-{}", uuid::Uuid::now_v7());
    let path = input
        .context
        .map(|context| context.path_task_ids.as_slice())
        .unwrap_or(&[]);
    let protected = input.gate.map(|gate| gate.protected_tokens).unwrap_or(0);
    let detail = format!(
        "non-critical request for {} tokens deferred by {}; remaining {} · protected {}",
        input.requested_tokens,
        reason.as_str(),
        input
            .remaining_tokens
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unbounded".to_string()),
        protected
    );
    let payload = denial_payload(GovernorDenial {
        session_id: &input.session.id,
        decision_id: &decision_id,
        plan_revision: input
            .context
            .map(|context| context.plan_revision)
            .unwrap_or(0),
        mode: input
            .context
            .map(|context| context.policy.mode)
            .unwrap_or_default(),
        task_id: input.spec.task_id.as_deref().unwrap_or("unknown"),
        task_critical: input
            .context
            .map(|context| context.critical)
            .unwrap_or(false),
        reason,
        requested_tokens: input.requested_tokens,
        limit_tokens: input.limit_tokens,
        spent_tokens: input.spent_tokens,
        reserved_tokens: input.reserved_tokens,
        remaining_tokens: input.remaining_tokens,
        protected_tokens: protected,
        path,
        path_weight_tokens: input
            .context
            .map(|context| context.path_weight_tokens)
            .unwrap_or(0),
        inputs_hash: input
            .context
            .map(|context| context.inputs_hash.as_str())
            .unwrap_or(""),
        detail,
        created_at: &now,
    });
    emit(
        input.engine,
        &input.session.id,
        input.spec,
        DECISION_EVENT_KIND,
        payload,
    )
    .await
}

#[async_recursion]
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_loop(
    engine: &Engine,
    config: &AgentConfig,
    session: &Session,
    project: &Project,
    control: &RunControl,
    budget: &SessionBudget,
    spec: &AgentSpec,
    objective: &str,
    conversation: &mut Vec<ChatMessage>,
    conversation_ids: Option<&mut Vec<Option<String>>>,
    top_level: bool,
    resume_journal: Option<crate::run_journal::RunJournal>,
) -> Result<LoopOutcome> {
    let resumed = resume_journal.is_some();
    let journal = match resume_journal {
        Some(journal) => journal,
        None => {
            crate::run_journal::RunJournal::begin(
                engine,
                session,
                &project.id,
                &spec.id,
                &spec.name,
                spec.role.as_str(),
                spec.task_id.as_deref(),
                spec.department.as_deref(),
                objective,
                &config.provider_name,
                &model_for(config, spec.role),
                &spec.worktree,
                budget.run_snapshot(),
                top_level,
            )
            .await?
        }
    };
    let result = run_loop_body(
        engine,
        config,
        session,
        project,
        control,
        budget,
        spec,
        objective,
        conversation,
        conversation_ids,
        top_level,
        &journal,
        resumed,
    )
    .await;
    // A run boundary is final for steering: anything still pending was not
    // consumed at a turn boundary and must not leak into a later run that
    // reuses this agent row.
    if let Err(error) = engine
        .cancel_pending_steering_for_agent(
            &session.id,
            &spec.id,
            "agent run ended before the steering message was delivered",
        )
        .await
    {
        tracing::warn!(%error, agent_id = %spec.id, "failed to cancel pending steering at run end");
    }
    let finish = match &result {
        Ok(LoopOutcome::Final(_) | LoopOutcome::Verdict(_)) => {
            journal.finish_completed(engine).await
        }
        Ok(LoopOutcome::Deferred(wait)) => {
            journal
                .finish_deferred(engine, &format!("waiting for gate {}", wait.gate_id))
                .await
        }
        Ok(LoopOutcome::Cancelled) => {
            if control.is_preempted() {
                journal
                    .finish_interrupted(
                        engine,
                        "preempted",
                        "the run was hard-preempted before it completed",
                    )
                    .await
            } else {
                journal
                    .finish_interrupted(
                        engine,
                        "cancelled",
                        "the run was cancelled before it completed",
                    )
                    .await
            }
        }
        Ok(LoopOutcome::BudgetExceeded(stop)) => {
            journal
                .finish_failed(engine, "budget_exceeded", &stop.summary())
                .await
        }
        Err(error) => {
            let message = format!("{error:#}");
            let kind = if message.contains("maximum turns reached") {
                "max_turns"
            } else {
                "error"
            };
            journal.finish_failed(engine, kind, &message).await
        }
    };
    if let Err(error) = finish {
        tracing::warn!(%error, run_id = journal.id(), "failed to finish durable run journal");
    }
    result
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_loop_body(
    engine: &Engine,
    config: &AgentConfig,
    session: &Session,
    project: &Project,
    control: &RunControl,
    budget: &SessionBudget,
    spec: &AgentSpec,
    objective: &str,
    conversation: &mut Vec<ChatMessage>,
    conversation_ids: Option<&mut Vec<Option<String>>>,
    top_level: bool,
    journal: &crate::run_journal::RunJournal,
    resumed: bool,
) -> Result<LoopOutcome> {
    let mut empty_ids = Vec::new();
    let conversation_ids: &mut Vec<Option<String>> = match conversation_ids {
        Some(ids) => ids,
        None => &mut empty_ids,
    };
    let mut citations = CitationRegistry::default();
    let mut active_skill: Option<Skill> = None;
    let mut deferred_wait: Option<DeferredWait> = None;
    let mut last_lease_heartbeat = Instant::now();
    const RUN_LEASE_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(300);
    let task_priority = if let Some(task_id) = &spec.task_id {
        engine
            .get_team_plan(&session.id)
            .await
            .ok()
            .flatten()
            .and_then(|plan| {
                plan.tasks
                    .iter()
                    .find(|task| &task.id == task_id)
                    .map(|task| task.priority)
            })
            .unwrap_or(0)
    } else {
        0
    };
    let capacity_priority = task_capacity_priority(session.priority, task_priority);
    let _capacity_permit = if matches!(
        spec.role,
        AgentRole::Worker | AgentRole::Labour | AgentRole::Scout
    ) && !spec.auto_approve
    {
        let worker_lane = matches!(spec.role, AgentRole::Worker | AgentRole::Scout);
        match config.capacity.try_acquire(spec.role, &session.id) {
            Some(permit) => Some(permit),
            None => {
                let snapshot = config.capacity.snapshot();
                emit(
                    engine,
                    &session.id,
                    spec,
                    "agent.capacity_wait",
                    serde_json::json!({
                        "role": spec.role.as_str(),
                        "limit": if worker_lane {
                            snapshot.worker_limit
                        } else {
                            snapshot.labour_limit
                        },
                        "available": if worker_lane {
                            snapshot.worker_available
                        } else {
                            snapshot.labour_available
                        },
                    }),
                )
                .await?;
                engine.update_agent_status(&spec.id, "queued").await?;
                control
                    .preempt
                    .set_phase(crate::preemption::Phase::Queue, None);
                let permit = match config
                    .capacity
                    .acquire(spec.role, &session.id, capacity_priority, control)
                    .await
                {
                    Some(permit) => permit,
                    None => return Ok(LoopOutcome::Cancelled),
                };
                engine.update_agent_status(&spec.id, "running").await?;
                emit(
                    engine,
                    &session.id,
                    spec,
                    "agent.capacity_ready",
                    serde_json::json!({
                        "role": spec.role.as_str(),
                        "waited": true,
                    }),
                )
                .await?;
                Some(permit)
            }
        }
    } else {
        None
    };
    let _scope_guard = if matches!(spec.role, AgentRole::Worker | AgentRole::Labour) {
        control
            .preempt
            .set_phase(crate::preemption::Phase::Queue, None);
        match config
            .scope_locks
            .acquire(
                &project.id,
                &session.id,
                &spec.id,
                &spec.write_scope,
                control,
            )
            .await?
        {
            Some(guard) => Some(guard),
            None => return Ok(LoopOutcome::Cancelled),
        }
    } else {
        None
    };
    let shell_policy = engine.get_shell_policy(&project.id).await?;
    let effect_policy = engine.get_effect_policy(&project.id).await?;
    let mut secret_roots = shell_secret_roots(engine.store().data_dir());
    secret_roots.extend(shell_policy.read_deny.iter().map(PathBuf::from));
    let tool_context = ToolContext {
        root: spec.worktree.clone(),
        project_root: PathBuf::from(&project.root),
        sandbox_root: engine
            .store()
            .data_dir()
            .join("sandbox")
            .join(&session.id)
            .join(&spec.id),
        secret_roots,
        network_allowed: shell_policy.network != "deny",
        home: std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default(),
        restrict_home: shell_policy.restrict_home,
        read_jail: shell_policy.read_jail,
        git_branch: Some(spec.branch.clone()),
        remote: engine.remote_access(&project.id).await?.map(Arc::new),
        session_id: Some(session.id.clone()),
        process_supervisor: Some(Arc::clone(engine.process_supervisor())),
        test_cache: Some(Arc::new(crate::test_cache::TestCacheAccess::new(
            engine.store().clone(),
            project.id.clone(),
        ))),
    };
    let run_contract = derive_contract_for_run(engine, project, session, spec).await?;
    let run_contract_policy = engine
        .get_contract_policy(&project.id)
        .await
        .unwrap_or_default();
    let mut run_contract_usage = match &run_contract {
        Some(contract) => engine
            .contract_usage(&contract.contract_id)
            .await
            .unwrap_or_default(),
        None => crate::contract::MutationUsage::default(),
    };
    let mut identity_memory_telemetry = None;
    let sections = build_prompt_sections(
        engine,
        project,
        session,
        spec,
        run_contract.as_ref(),
        objective,
        true,
        InstructionBuildMode::Materialize,
        Some(&mut identity_memory_telemetry),
    )
    .await?;
    // Tail memory is appended to history once, at the position where it was
    // first seen, and is never reinserted ahead of newer messages. Without this
    // anchor a freshly rebuilt memory brief would move every turn and break the
    // provider prefix after the previous prompt. The anchor is a stable system
    // message; Anthropic projects it to a `[memory]` user message because its
    // `system` field is top-level.
    let memory_anchor = ensure_memory_anchor(
        engine,
        session,
        spec,
        &sections,
        conversation,
        conversation_ids,
        top_level,
    )
    .await?;
    let system = sections.render();
    // Retained for older adapters and the Anthropic legacy fallback. It never
    // contains volatile run context or memory.
    let stable_prefix = sections.stable_prefix();
    // The tool bundle is frozen for the run. Skill activation is enforced at
    // execution time, not by filtering schemas mid-run, and dynamic MCP schemas
    // are sorted and appended exactly once. This is the prefix that every
    // provider call in the run receives unchanged.
    let dynamic_mcp_tools =
        collect_dynamic_mcp_tools(engine, spec.role, &project.id, Some(&session.id)).await;
    let frozen_bundle = frozen_tool_bundle(config, spec, &dynamic_mcp_tools);
    let mut tools = frozen_bundle.tools.clone();
    let system_tokens = estimate_context_tokens(&system);
    let mut tool_tokens = serde_json::to_string(&tools)
        .map(|schema| estimate_context_tokens(&schema))
        .unwrap_or(0);
    let compaction_policy = context_compaction_policy(config, spec.role);
    let (context_threshold, context_hard_limit) =
        (compaction_policy.threshold, compaction_policy.hard_limit);
    if context_hard_limit != u64::MAX
        && system_tokens.saturating_add(tool_tokens) > context_hard_limit
    {
        return Err(anyhow!(
            "system prompt and tool schemas (~{} tokens) exceed the model context hard limit ({}); increase VERLINPARK_CONTEXT_WINDOW or role context_window, or reduce enabled tools/MCP servers",
            system_tokens.saturating_add(tool_tokens),
            context_hard_limit
        ));
    }
    let model_name = model_for(config, spec.role);
    let mut prompt_hashes = sections.hashes(&model_name, &tools);
    let mut prefix_hash = prompt_hashes.prefix_hash.clone();
    let contract_section = prompt_hashes
        .head_sections
        .iter()
        .find(|section| section.key == "session/contract");
    let contract_hash = contract_section.map(|section| section.hash.clone());
    let contract_chars = prompt_hashes.section_chars.get("session/contract").copied();
    let contract_policy_enabled = engine
        .get_contract_policy(&project.id)
        .await
        .map(|policy| policy.enabled)
        .unwrap_or(false);
    let cache_policy = engine
        .get_cache_policy(&project.id)
        .await
        .unwrap_or_default();
    let mut cache_plan = crate::prompt_cache::request_cache_plan(
        &config.provider_name,
        &sections,
        tools.len(),
        conversation.len(),
        &cache_policy,
        spec.role.as_str(),
    );
    let mut cache_config =
        crate::prompt_cache::prompt_cache_config(&cache_policy, spec.role.as_str());
    let provider_capabilities = config.provider.cache_capabilities();
    // L4 finalizes the shareable epoch. It commits to the model, canonical
    // tool hash, and ordered core/project section hashes. Session context, run
    // task, prompt, memory, and tail bytes are deliberately excluded.
    let mut prompt_epoch = crate::cache_epoch::CacheEpoch::from_hashes(&model_name, &prompt_hashes);
    let previous_prefix_event = engine
        .recent_events_of_kind(&session.id, "context.prompt_prefix", 1)
        .await?
        .into_iter()
        .last();
    let previous_epoch = previous_prefix_event
        .as_ref()
        .and_then(|event| crate::cache_epoch::CacheEpoch::from_event_payload(&event.payload));
    let epoch_change_reason = crate::cache_epoch::epoch_change_reason(
        previous_epoch.as_ref().map(|epoch| &epoch.identity),
        &prompt_epoch.identity,
    );
    let mut epoch_reuse = engine
        .cache_epoch_reuse(&project.id, &prompt_epoch.hash, &session.id)
        .await?;
    let identity_section_count = prompt_hashes.identity_sections.len();
    let identity_memory_present = prompt_hashes.section_chars.contains_key("agent/memory");
    let identity_memory_payload = identity_memory_telemetry
        .as_ref()
        .and_then(crate::identity_memory::IdentityMemoryBrief::telemetry);
    let identity_hash_value =
        (!prompt_hashes.identity_hash.is_empty()).then(|| prompt_hashes.identity_hash.clone());
    let mut prefix_payload = serde_json::json!({
        "version": prompt_hashes.prompt_layout,
        "prompt_layout": prompt_hashes.prompt_layout,
        "prompt_layout_name": crate::instruction::prompt_layout_name(prompt_hashes.prompt_layout),
        "instruction_state_id": crate::instruction::instruction_state_id(
            &session.id,
            &spec.id,
            spec.role == AgentRole::Leader,
        ),
        "provider_tail_system": provider_capabilities.tail_system.accepts_system(),
        "provider_wire": provider_capabilities.wire.as_str(),
        "tail_system_capability": provider_capabilities.tail_system.as_str(),
        "canonical_version": vp_providers::canonical::CANONICAL_VERSION,
        "memory_anchor": memory_anchor,
        "cache_break_events": crate::conversation::CACHE_BREAK_EVENT,
        "model": model_name,
        "tool_bundle": frozen_bundle.selection.kind.as_str(),
        "tool_bundle_hash": frozen_bundle.bundle_hash,
        "tool_bundle_reason": frozen_bundle.selection.reason,
        "tool_bundle_fallback": frozen_bundle.selection.fallback,
        "tool_bundle_tool_count": frozen_bundle.builtin_count(),
        "tool_bundle_mcp_count": frozen_bundle.mcp_count(),
        "tool_bundle_total_tool_count": frozen_bundle.tool_count(),
        "tool_bundle_target_limit": frozen_bundle.target_limit,
        "tool_bundle_within_target": frozen_bundle.within_target(),
        "tool_bundle_omitted": &frozen_bundle.omitted,
        "tool_bundle_warnings": frozen_bundle.warnings.iter().map(|warning| warning.payload()).collect::<Vec<_>>(),
        "tool_bundle_frozen": true,
        "stable_system_hash": prompt_hashes.stable_system_hash,
        "stable_prefix_hash": prompt_hashes.stable_prefix_hash,
        "core_hash": prompt_hashes.core_hash,
        "project_hash": prompt_hashes.project_hash,
        "tools_hash": prompt_hashes.tools_hash,
        "volatile_hash": prompt_hashes.volatile_hash,
        "prefix_hash": prefix_hash,
        "cache_generation": prefix_hash,
        // S3 identity metadata. It is hash/size only; no identity prompt text
        // or memory brief ever enters telemetry.
        "identity_hash": identity_hash_value,
        "identity_id": prompt_hashes.identity_id,
        "identity_generation": prompt_hashes.identity_generation,
        "identity_sections": prompt_hashes.identity_sections,
        "identity_section_count": identity_section_count,
        "identity_memory_present": identity_memory_present,
        // S5 memory telemetry: counts/hashes/chars only. Content never enters
        // the event stream.
        "identity_memory": identity_memory_payload,
        // The finalized L4 epoch replaces the L1 placeholder. A compaction
        // boundary later in the run records a conversation-cache reset; it
        // does not rewrite this shareable instruction identity.
        "cache_epoch_source": "l4",
        "cache_epoch_placeholder": false,
        "cache_epoch_previous": previous_epoch.as_ref().map(|epoch| epoch.hash.clone()),
        "cache_epoch_changed": previous_epoch.as_ref().map(|epoch| epoch.hash.as_str()) != Some(prompt_epoch.hash.as_str()),
        "cache_epoch_change_reason": epoch_change_reason.as_str(),
        "cache_epoch_reused_from_sessions": epoch_reuse.prior_sessions,
        "cache_epoch_session_seen_before": !epoch_reuse.first_session_call,
        "cache_epoch_session_calls": epoch_reuse.session_calls,
        "head_sections": prompt_hashes.head_sections,
        "tail_sections": prompt_hashes.tail_sections,
        "section_chars": prompt_hashes.section_chars,
        "contract_hash": contract_hash,
        "contract_chars": contract_chars,
        "contract_capsule": contract_hash.is_some(),
        "contract_policy_enabled": contract_policy_enabled,
        "tool_count": tools.len(),
        "core_chars": sections.core.chars().count(),
        "project_chars": sections.project.chars().count(),
        "volatile_chars": sections.volatile.chars().count(),
        "stable_prefix_chars": stable_prefix.chars().count(),
        "cache_policy": cache_policy,
        "cache_plan": cache_plan.payload(),
        // Heuristic estimate of the system-prompt and tool-schema preamble.
        // This is the same estimate already used for context-limit checks,
        // not provider-reported usage.
        "preamble_tokens_estimate": system_tokens.saturating_add(tool_tokens),
        "system_tokens_estimate": system_tokens,
        "tool_tokens_estimate": tool_tokens,
    });
    if let Some(object) = prefix_payload.as_object_mut() {
        for (key, value) in prompt_epoch.payload_fields() {
            object.insert(key, value);
        }
    }
    let base_prefix_payload = prefix_payload.clone();
    emit(
        engine,
        &session.id,
        spec,
        "context.prompt_prefix",
        prefix_payload,
    )
    .await?;
    // Durable per-run manifest. It is separate from `context.prompt_prefix`
    // so cache/prefix readers can stay small while an operator can inspect the
    // exact frozen tool list without a provider call.
    emit(
        engine,
        &session.id,
        spec,
        "context.tool_bundle",
        frozen_bundle.manifest_payload(),
    )
    .await?;
    // Seed miss attribution from the latest persisted ordinary agent call.
    // Warm calls are real provider calls but describe the stable sections only,
    // so comparing an agent call to a warm would mislabel a role/tool change as
    // churn. A resumed run can still attribute a tool/project change to the
    // previous agent generation.
    let recent_cache_events = engine
        .recent_events_of_kind(&session.id, "context.cache", 20)
        .await?;
    let latest_agent_cache_event = recent_cache_events.iter().rev().find(|event| {
        event
            .payload
            .get("warm")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
            && cache_event_belongs_to(&event.payload, spec)
    });
    let mut cache_tracker = crate::prompt_cache::CacheCallTracker::from_event(
        latest_agent_cache_event.map(|event| &event.payload),
    );
    let mut message_tracker = crate::conversation::MessagePrefixTracker::from_event(
        latest_agent_cache_event.map(|event| &event.payload),
    );
    let run_capabilities = config.provider.cache_capabilities();
    if let Some(payload) = crate::conversation::capability_change_payload(
        run_capabilities,
        latest_agent_cache_event.map(|event| &event.payload),
        &config.provider_name,
    ) && let Err(error) = emit(
        engine,
        &session.id,
        spec,
        "context.cache_capability",
        payload,
    )
    .await
    {
        tracing::warn!(%error, "could not persist provider cache capability telemetry");
    }
    // A compaction that happened after the last provider call (manual compact,
    // or an automatic compaction at the end of a previous run) makes the next
    // observation a reset, not a generic edit. Event sequence numbers are
    // per-session and monotonic, so this comparison is exact.
    if let Some(compaction) = engine
        .latest_event_of_kind(&session.id, "context.compacted")
        .await?
        && latest_agent_cache_event.is_none_or(|cache| compaction.seq > cache.seq)
    {
        message_tracker.mark_compaction();
    }
    if resumed && cache_policy.warm_on_resume && !budget.exhausted() {
        // Explicit policy opt-in only. The default is false, so a normal run
        // still makes exactly one provider call per turn.
        let warm_sections = build_prompt_sections(
            engine,
            project,
            session,
            spec,
            None,
            objective,
            false,
            InstructionBuildMode::Materialize,
            None,
        )
        .await?;
        match warm_prompt_cache(
            engine,
            config,
            session,
            spec,
            budget,
            &warm_sections,
            &tools,
            "resume",
        )
        .await
        {
            Ok(result) => {
                tracing::info!(
                    session_id = %session.id,
                    input_tokens = result.input_tokens,
                    cache_read_tokens = result.cache_read_tokens,
                    cost_usd = result.cost_usd,
                    "warmed the stable prompt prefix before resuming"
                );
            }
            Err(error) => {
                tracing::warn!(%error, "cache warm on resume failed; continuing the run");
                if let Err(emit_error) = emit(
                    engine,
                    &session.id,
                    spec,
                    "context.cache_warm",
                    serde_json::json!({
                        "session_id": session.id,
                        "warmed": false,
                        "source": "resume",
                        "provider": config.provider_name,
                        "model": model_name,
                        "message": truncate(&format!("{error:#}")),
                    }),
                )
                .await
                {
                    tracing::warn!(%emit_error, "could not persist failed cache warm event");
                }
            }
        }
    }

    let mut calibration = if top_level {
        load_context_calibration(engine, &session.id, &model_name).await
    } else {
        ContextCalibration::default()
    };
    let mut team_plan = engine.get_team_plan(&session.id).await?;
    let project_budget = engine.get_project_budget(&project.id).await?;
    let plan_budget = PlanBudgetScope::from_plan(
        team_plan.as_ref(),
        spec,
        Some(&project_budget.budget),
        &project.id,
    );
    // Static critical-path context for this run. It is intentionally computed
    // once at run start; a plan revision or completion is picked up by the next
    // run or scheduler tick. A disabled policy returns `None` without touching
    // task admission or provider budgeting.
    let governor_context = engine
        .governor_run_context(&session.id, spec.task_id.as_deref())
        .await?;
    let mut warned_project = false;
    // Bounded in-run prepared-summary cache. It is discarded at run end and
    // never starts a second agent run.
    let mut compaction_runtime = CompactionRuntime::default();
    // Number of prior provider observations for this session/epoch. The first
    // call of a new session can attribute provider cache reads to another
    // session; later calls would double-count the conversation prefix this
    // session itself created.
    let mut epoch_observed_calls = epoch_reuse.session_calls;
    let mut pending_promotions: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();
    let mut tool_bundle_revision = 0u32;
    let mut active_bundle_hash = frozen_bundle.bundle_hash.clone();

    for turn in 0..config.max_turns {
        // A long plan can outlive the assignment/workspace lease taken when
        // the run attached. Renew both at a bounded cadence. Lease loss is an
        // event-backed warning, not a silent switch to the legacy path.
        if let Some(identity_id) = &spec.identity_id
            && last_lease_heartbeat.elapsed() >= RUN_LEASE_HEARTBEAT_INTERVAL
        {
            match engine.heartbeat_run_leases(&session.id, identity_id).await {
                Ok(report) => {
                    if !report.warnings.is_empty() {
                        emit(
                            engine,
                            &session.id,
                            spec,
                            "agent.lease_heartbeat",
                            serde_json::json!({
                                "identity_id": identity_id,
                                "assignment_renewed": report.assignment_renewed,
                                "workspaces_renewed": report.workspaces_renewed,
                                "warnings": report.warnings,
                            }),
                        )
                        .await?;
                    }
                }
                Err(error) => {
                    emit(
                        engine,
                        &session.id,
                        spec,
                        "agent.lease_heartbeat_failed",
                        serde_json::json!({
                            "identity_id": identity_id,
                            "error": format!("{error:#}").chars().take(1_000).collect::<String>(),
                        }),
                    )
                    .await?;
                }
            }
            last_lease_heartbeat = Instant::now();
        }
        // Provider-facing bytes retained for tool results in this assistant
        // turn. It resets each turn and feeds the per-turn aggregate budget.
        let mut turn_tool_bytes = 0u64;
        if !wait_for_task_resume(engine, &session.id, spec, control).await? {
            return Ok(LoopOutcome::Cancelled);
        }
        if control.is_cancelled() {
            return Ok(LoopOutcome::Cancelled);
        }
        if budget.exhausted() {
            return Ok(LoopOutcome::BudgetExceeded(BudgetStop::session(budget)));
        }
        if let Some(reason) = budget.warning() {
            emit(
                engine,
                &session.id,
                spec,
                "agent.budget_warning",
                serde_json::json!({
                    "reason": reason,
                    "limit": budget.limit,
                    "spent": budget.spent(),
                    "cost_limit_usd": budget.cost_limit,
                    "cost_spent_usd": budget.cost_spent(),
                    "time_budget_secs": budget.time_limit.map(|limit| limit.as_secs()),
                    "elapsed_secs": budget.elapsed_secs(),
                }),
            )
            .await?;
        }
        // C8 schema promotion is applied only at a turn boundary. The current
        // provider request still used the frozen bundle that produced the
        // tool_search result; the next request sees the new schema set.
        if !pending_promotions.is_empty() {
            let candidates = std::mem::take(&mut pending_promotions);
            let available = tool_schemas(config, spec.role, spec.profile, spec.allow_delegation);
            let mut added = Vec::new();
            for name in candidates {
                if tools.iter().any(|tool| tool.name == name) {
                    continue;
                }
                if let Some(schema) = available.iter().find(|tool| tool.name == name) {
                    added.push(schema.clone());
                }
            }
            if !added.is_empty() {
                let old_tools_hash = prompt_hashes.tools_hash.clone();
                let old_epoch = prompt_epoch.hash.clone();
                let old_tool_tokens = tool_tokens;
                let prices = role_prices(config, spec.role);
                let added_names = added
                    .iter()
                    .map(|tool| tool.name.clone())
                    .collect::<Vec<_>>();
                let mut candidate_tools = tools.clone();
                candidate_tools.extend(added);
                candidate_tools.sort_by(|left, right| left.name.cmp(&right.name));
                candidate_tools.dedup_by(|left, right| left.name == right.name);
                let candidate_tool_tokens = serde_json::to_string(&candidate_tools)
                    .map(|schema| estimate_context_tokens(&schema))
                    .unwrap_or(0);
                if context_hard_limit != u64::MAX
                    && system_tokens.saturating_add(candidate_tool_tokens) > context_hard_limit
                {
                    if let Err(error) = emit(
                        engine,
                        &session.id,
                        spec,
                        "bundle.revision",
                        serde_json::json!({
                            "bundle": frozen_bundle.selection.kind.as_str(),
                            "revision": tool_bundle_revision,
                            "applied": false,
                            "reason": "context_limit",
                            "added_tools": added_names,
                            "tool_count": tools.len(),
                            "tool_tokens": candidate_tool_tokens,
                            "hard_limit": context_hard_limit,
                            "estimated": true,
                        }),
                    )
                    .await
                    {
                        tracing::warn!(%error, "could not persist rejected bundle revision");
                    }
                } else {
                    tools = candidate_tools;
                    tool_tokens = candidate_tool_tokens;
                    prompt_hashes = sections.hashes(&model_name, &tools);
                    prefix_hash = prompt_hashes.prefix_hash.clone();
                    cache_plan = crate::prompt_cache::request_cache_plan(
                        &config.provider_name,
                        &sections,
                        tools.len(),
                        conversation.len(),
                        &cache_policy,
                        spec.role.as_str(),
                    );
                    cache_config =
                        crate::prompt_cache::prompt_cache_config(&cache_policy, spec.role.as_str());
                    prompt_epoch =
                        crate::cache_epoch::CacheEpoch::from_hashes(&model_name, &prompt_hashes);
                    epoch_reuse = engine
                        .cache_epoch_reuse(&project.id, &prompt_epoch.hash, &session.id)
                        .await?;
                    epoch_observed_calls = epoch_reuse.session_calls;
                    tool_bundle_revision = tool_bundle_revision.saturating_add(1);
                    active_bundle_hash = crate::tool_bundle::canonical_tools_hash(&tools);
                    let schema_tokens_added = tool_tokens.saturating_sub(old_tool_tokens);
                    let lost_cache_read_usd = crate::prompt_cache::round_usd(
                        (schema_tokens_added as f64 / 1_000_000.0) * prices.cache_read,
                    );
                    if let Err(error) = emit(
                        engine,
                        &session.id,
                        spec,
                        "bundle.revision",
                        serde_json::json!({
                            "bundle": frozen_bundle.selection.kind.as_str(),
                            "revision": tool_bundle_revision,
                            "applied": true,
                            "previous_tools_hash": old_tools_hash,
                            "tools_hash": prompt_hashes.tools_hash,
                            "previous_cache_epoch": old_epoch,
                            "cache_epoch": prompt_epoch.hash,
                            "cache_epoch_changed": true,
                            "cache_epoch_change_reason": "tools_changed",
                            "added_tools": added_names,
                            "removed_tools": [],
                            "tool_count": tools.len(),
                            "target_limit": frozen_bundle.target_limit,
                            "over_target": frozen_bundle.target_limit.is_some_and(|limit| tools.len() > limit),
                            "previous_tool_tokens": old_tool_tokens,
                            "tool_tokens": tool_tokens,
                            "schema_tokens_added": schema_tokens_added,
                            "estimated_cache_invalidation_tokens": tool_tokens.max(old_tool_tokens),
                            "cache_read_price_per_million": prices.cache_read,
                            "estimated_lost_cache_read_usd": lost_cache_read_usd,
                            "estimated": true,
                        }),
                    )
                    .await
                    {
                        tracing::warn!(%error, "could not persist bundle revision");
                    }
                    let mut promoted_prefix = base_prefix_payload.clone();
                    if let Some(object) = promoted_prefix.as_object_mut() {
                        object.insert("prefix_hash".into(), serde_json::json!(prefix_hash));
                        object.insert("cache_generation".into(), serde_json::json!(prefix_hash));
                        object.insert(
                            "tools_hash".into(),
                            serde_json::json!(prompt_hashes.tools_hash),
                        );
                        object.insert(
                            "tool_bundle_hash".into(),
                            serde_json::json!(active_bundle_hash),
                        );
                        object.insert(
                            "bundle_revision".into(),
                            serde_json::json!(tool_bundle_revision),
                        );
                        object.insert("tool_bundle_frozen".into(), serde_json::json!(false));
                        object.insert("tool_count".into(), serde_json::json!(tools.len()));
                        object.insert(
                            "tool_bundle_tool_count".into(),
                            serde_json::json!(tools.len()),
                        );
                        object.insert(
                            "tool_bundle_total_tool_count".into(),
                            serde_json::json!(tools.len()),
                        );
                        object.insert(
                            "cache_epoch_change_reason".into(),
                            serde_json::json!("tools_changed"),
                        );
                        object.insert("cache_epoch_changed".into(), serde_json::json!(true));
                        object.insert("cache_epoch_previous".into(), serde_json::json!(old_epoch));
                        object.insert("cache_epoch_source".into(), serde_json::json!("l4"));
                        object.insert("cache_epoch_placeholder".into(), serde_json::json!(false));
                        object.insert(
                            "cache_epoch_reused_from_sessions".into(),
                            serde_json::json!(epoch_reuse.prior_sessions),
                        );
                        object.insert(
                            "cache_epoch_session_seen_before".into(),
                            serde_json::json!(!epoch_reuse.first_session_call),
                        );
                        object.insert(
                            "cache_epoch_session_calls".into(),
                            serde_json::json!(epoch_reuse.session_calls),
                        );
                        object.insert(
                            "stable_prefix_hash".into(),
                            serde_json::json!(prompt_hashes.stable_prefix_hash),
                        );
                        object.insert(
                            "stable_system_hash".into(),
                            serde_json::json!(prompt_hashes.stable_system_hash),
                        );
                        object.insert(
                            "core_hash".into(),
                            serde_json::json!(prompt_hashes.core_hash),
                        );
                        object.insert(
                            "project_hash".into(),
                            serde_json::json!(prompt_hashes.project_hash),
                        );
                        object.insert(
                            "volatile_hash".into(),
                            serde_json::json!(prompt_hashes.volatile_hash),
                        );
                        object.insert(
                            "head_sections".into(),
                            serde_json::json!(prompt_hashes.head_sections),
                        );
                        object.insert(
                            "tail_sections".into(),
                            serde_json::json!(prompt_hashes.tail_sections),
                        );
                        object.insert(
                            "section_chars".into(),
                            serde_json::json!(prompt_hashes.section_chars),
                        );
                        object.insert("cache_plan".into(), cache_plan.payload());
                        object.insert(
                            "system_tokens_estimate".into(),
                            serde_json::json!(system_tokens),
                        );
                        object.insert(
                            "tool_tokens_estimate".into(),
                            serde_json::json!(tool_tokens),
                        );
                        object.insert(
                            "preamble_tokens_estimate".into(),
                            serde_json::json!(system_tokens.saturating_add(tool_tokens)),
                        );
                    }
                    for (key, value) in prompt_epoch.payload_fields() {
                        if let Some(object) = promoted_prefix.as_object_mut() {
                            object.insert(key, value);
                        }
                    }
                    if let Err(error) = emit(
                        engine,
                        &session.id,
                        spec,
                        "context.prompt_prefix",
                        promoted_prefix,
                    )
                    .await
                    {
                        tracing::warn!(%error, "could not persist promoted prompt prefix");
                    }
                }
            }
        }
        {
            let compaction_before = crate::run_journal::journal_cursors(engine, &session.id)
                .await?
                .0;
            let compaction_changed = compact_context_if_needed(
                engine,
                config,
                session,
                spec,
                budget,
                control,
                conversation,
                &mut *conversation_ids,
                context_threshold,
                context_hard_limit,
                system_tokens.saturating_add(tool_tokens),
                calibration.factor,
                false,
                &prefix_hash,
                config.compact_prepare_ratio,
                &mut compaction_runtime,
                context_strategy(spec.role),
            )
            .await?;
            if compaction_changed {
                // The stable system/tool prefix is unchanged, but the cached
                // conversation prefix is not. The next call is attributed to
                // the reset rather than to a provider TTL guess.
                cache_tracker.mark_compaction();
                message_tracker.mark_compaction();
            }
            let compaction_after = crate::run_journal::journal_cursors(engine, &session.id)
                .await?
                .0;
            if compaction_after != compaction_before {
                journal
                    .record_compaction(
                        engine,
                        turn + 1,
                        compaction_before,
                        compaction_after,
                        conversation.len(),
                    )
                    .await?;
            }
            if control.is_cancelled() {
                return Ok(LoopOutcome::Cancelled);
            }
            if budget.exhausted() {
                return Ok(LoopOutcome::BudgetExceeded(BudgetStop::session(budget)));
            }
            let context_window = context_window_for(config, spec.role);
            if context_window > 0 {
                let estimated_context = calibrate_context_tokens(
                    estimate_messages_tokens(conversation)
                        .saturating_add(system_tokens)
                        .saturating_add(tool_tokens),
                    calibration.factor,
                );
                emit(
                    engine,
                    &session.id,
                    spec,
                    "context.usage",
                    serde_json::json!({
                        "estimated_tokens": estimated_context,
                        "context_window": context_window,
                        "threshold_tokens": context_threshold,
                        "hard_limit_tokens": context_hard_limit,
                        "threshold_source": compaction_policy.source(),
                        "threshold_ratio_basis_points": compaction_policy.ratio_basis_points,
                        "output_reserve_tokens": compaction_policy.output_reserve,
                        "messages": conversation.len(),
                        "calibration": calibration.factor,
                    }),
                )
                .await?;
            }
        }
        if let Err(error) = engine
            .touch_task_progress(&session.id, spec.task_id.as_deref(), turn + 1, None)
            .await
        {
            tracing::warn!(%error, "failed to record task turn progress");
        }
        let delivered = engine
            .deliver_pending_steering(&session.id, &spec.id, spec.task_id.as_deref(), top_level)
            .await?;
        for steering in delivered {
            conversation.push(ChatMessage::user(steering.content));
            if let Some(message) = steering.message {
                conversation_ids.push(Some(message.id));
            }
        }
        let mut request = ChatRequest {
            model: model_name.clone(),
            messages: conversation.clone(),
            system: Some(system.clone()),
            system_stable_prefix: Some(stable_prefix.clone()),
            system_sections: sections.provider_sections(),
            tail_sections: sections.tail_sections(),
            cache: Some(cache_config),
            max_tokens: config.max_tokens,
            temperature: Some(0.2),
            // Keep the full role bundle stable across the run. `active_skill`
            // is enforced when a tool is dispatched, not by changing schemas.
            tools: tools.clone(),
        };
        // A safe turn boundary: messages, budget, worktree HEAD, and plan
        // revision are all durable before the provider request intent.
        journal
            .checkpoint(engine, turn + 1, budget.run_snapshot())
            .await?;
        const MIN_OUTPUT_TOKENS: u64 = 64;
        let configured_output = config.max_tokens.unwrap_or(4096).max(1) as u64;
        let input_estimate = estimate_request_tokens(&request);
        let prices = role_prices(config, spec.role);
        let (input_price, output_price) = (prices.input, prices.output);
        let scope_spent = if plan_budget.active() && !budget.is_unlimited() {
            Some(
                engine
                    .usage_spent(
                        &session.id,
                        spec.task_id.as_deref(),
                        spec.department.as_deref(),
                    )
                    .await?,
            )
        } else {
            None
        };
        let task_spent = scope_spent
            .map(|(tokens, cost, _, _)| (tokens, cost))
            .unwrap_or((0, 0.0));
        let department_spent = scope_spent
            .map(|(_, _, tokens, cost)| (tokens, cost))
            .unwrap_or((0, 0.0));
        let project_spent = if plan_budget.project_active() && !budget.is_unlimited() {
            engine.project_usage(&project.id).await?
        } else {
            (0, 0.0)
        };
        if !budget.is_unlimited()
            && !warned_project
            && let Some(warning) = plan_budget.project_warning(project_spent)
        {
            emit(
                engine,
                &session.id,
                spec,
                "agent.budget_warning",
                warning.payload(),
            )
            .await?;
            warned_project = true;
        }
        let mut desired_output = if !budget.is_unlimited() && budget.limit.is_some() {
            let remaining = budget.available_tokens().unwrap_or(0);
            if remaining <= input_estimate.saturating_add(MIN_OUTPUT_TOKENS) {
                return Ok(LoopOutcome::BudgetExceeded(BudgetStop::session_tokens(
                    budget,
                )));
            }
            configured_output.min(remaining - input_estimate)
        } else {
            configured_output
        };
        // Governor gate for this provider call. It carries `critical` and the
        // task id with the normal scope requests. A non-critical request is
        // clamped to the free headroom, or deferred with a typed reason, but
        // never above the session/project/department hard budget.
        let governor_budget = if budget.limit.is_some() {
            (budget.available_tokens(), budget.limit)
        } else if let Some(limit) = plan_budget.project_tokens {
            (Some(limit.saturating_sub(project_spent.0)), Some(limit))
        } else {
            (None, None)
        };
        let governor_gate = governor_context
            .as_ref()
            .and_then(|context| context.gate(governor_budget.0, governor_budget.1));
        if !plan_budget.active()
            && !budget.is_unlimited()
            && let Some(gate) = governor_gate.as_ref()
        {
            match gate.available_output(input_estimate, MIN_OUTPUT_TOKENS) {
                Ok(Some(cap)) => desired_output = desired_output.min(cap),
                Ok(None) => {}
                Err(reason) => {
                    emit_governor_deferral(GovernorDeferralEvent {
                        engine,
                        session,
                        spec,
                        context: governor_context.as_ref(),
                        gate: Some(gate),
                        reason,
                        requested_tokens: input_estimate.saturating_add(desired_output),
                        remaining_tokens: gate.remaining_tokens,
                        limit_tokens: governor_budget.1,
                        spent_tokens: budget.spent(),
                        reserved_tokens: engine.budget_registry().reserved_for_session(&session.id),
                    })
                    .await?;
                    let stop = BudgetStop::governor(
                        reason,
                        spec.task_id.as_deref(),
                        budget,
                        gate.protected_tokens,
                    );
                    emit_scope_budget_stop(engine, &session.id, spec, &stop).await?;
                    return Ok(LoopOutcome::BudgetExceeded(stop));
                }
            }
        }
        let scope_lease = if plan_budget.active() && !budget.is_unlimited() {
            let requests = plan_budget.requests(
                &session.id,
                task_spent,
                department_spent,
                project_spent,
                input_price,
                output_price,
            );
            match engine.budget_registry().grant_governed(
                &requests,
                governor_gate.as_ref(),
                input_estimate,
                desired_output,
                MIN_OUTPUT_TOKENS,
            ) {
                Ok(lease) => Some(lease),
                Err(rejection) => {
                    if matches!(
                        rejection.reason,
                        BudgetReason::Reserved | BudgetReason::Throttled
                    ) {
                        emit_governor_deferral(GovernorDeferralEvent {
                            engine,
                            session,
                            spec,
                            context: governor_context.as_ref(),
                            gate: governor_gate.as_ref(),
                            reason: rejection.reason,
                            requested_tokens: input_estimate.saturating_add(desired_output),
                            remaining_tokens: governor_budget.0,
                            limit_tokens: governor_budget.1,
                            spent_tokens: budget.spent(),
                            reserved_tokens: rejection.reserved_tokens,
                        })
                        .await?;
                    }
                    let stop =
                        BudgetStop::from_governor_rejection(rejection, spec.task_id.as_deref());
                    emit_scope_budget_stop(engine, &session.id, spec, &stop).await?;
                    return Ok(LoopOutcome::BudgetExceeded(stop));
                }
            }
        } else {
            None
        };
        let output_budget = scope_lease
            .as_ref()
            .map_or(desired_output, |lease| lease.granted_output_tokens());
        let mut token_reservation = None;
        if !budget.is_unlimited() && budget.limit.is_some() {
            let Some(reservation) =
                budget.reserve_tokens(input_estimate.saturating_add(output_budget))
            else {
                return Ok(LoopOutcome::BudgetExceeded(BudgetStop::session_tokens(
                    budget,
                )));
            };
            token_reservation = Some(reservation);
        }
        let mut cost_reservation = match budget.reserve_cost(input_estimate, output_budget, prices)
        {
            Ok(reservation) => reservation,
            Err(()) => {
                return Ok(LoopOutcome::BudgetExceeded(BudgetStop::session_cost(
                    budget,
                )));
            }
        };
        if output_budget < configured_output {
            request.max_tokens = Some(output_budget.min(u32::MAX as u64) as u32);
        }
        let _scope_lease = scope_lease;
        // Canonical continuity is computed from the exact request that will be
        // sent, before the provider call. A structural break is recorded now;
        // a provider-side miss is only knowable after usage arrives. Neither
        // path ever rewrites history.
        let provider_capabilities = config.provider.cache_capabilities();
        let observation = crate::conversation::CanonicalCallObservation::new(
            &request,
            provider_capabilities,
            &prompt_hashes.tools_hash,
            &mut message_tracker,
        );
        if let Some(reason) = observation.report.reason {
            let payload = crate::conversation::cache_break_payload(
                &observation,
                reason,
                &config.provider_name,
                &request.model,
            );
            if let Err(error) = emit(
                engine,
                &session.id,
                spec,
                crate::conversation::CACHE_BREAK_EVENT,
                payload,
            )
            .await
            {
                tracing::warn!(%error, "could not persist cache break telemetry");
            }
        }
        let model_step = journal.model_intent(engine, turn + 1, &request).await?;
        let stream_turn = consume_stream(
            engine,
            &session.id,
            spec,
            control,
            config.provider.as_ref(),
            &request,
            Duration::from_secs(config.provider_idle_timeout_secs),
        )
        .await?;
        if top_level && stream_turn.usage.input > 0 {
            let estimated_request = estimate_messages_tokens(&request.messages)
                .saturating_add(system_tokens)
                .saturating_add(tool_tokens);
            let ratio = stream_turn.usage.input as f64 / estimated_request.max(1) as f64;
            calibration.observe(estimated_request, stream_turn.usage.input);
            // Measurement is diagnostic; a failed append must not discard the
            // provider call's accounting or the turn output.
            if let Err(error) = emit(
                engine,
                &session.id,
                spec,
                "context.measured",
                serde_json::json!({
                    "estimated_tokens": estimated_request,
                    "actual_input_tokens": stream_turn.usage.input,
                    "actual_output_tokens": stream_turn.usage.output,
                    "cache_read_tokens": stream_turn.usage.cache_read,
                    "cache_write_tokens": stream_turn.usage.cache_write,
                    "cache_hit_ratio": vp_protocol::types::cache_hit_ratio(
                        stream_turn.usage.input,
                        stream_turn.usage.cache_read,
                        stream_turn.usage.cache_write,
                    ),
                    "ratio": ratio,
                    "calibration": calibration.factor,
                    "samples": calibration.samples,
                    "model": request.model.clone(),
                    "messages": request.messages.len(),
                    "prompt_prefix_hash": prefix_hash,
                }),
            )
            .await
            {
                tracing::warn!(%error, "could not persist context measurement");
            }
        }
        let has_output = !stream_turn.text.is_empty() || !stream_turn.tool_calls.is_empty();
        let reported = stream_turn.usage;
        let input_tokens = if reported.input > 0 {
            reported.input
        } else if has_output || reported.output > 0 {
            estimate_request_tokens(&request)
        } else {
            0
        };
        let output_tokens = if reported.output > 0 {
            reported.output
        } else if has_output {
            estimate_output_tokens(&stream_turn.text, &stream_turn.tool_calls)
        } else {
            0
        };
        let estimated = reported.input == 0 || reported.output == 0;
        let (cache_read_tokens, cache_write_tokens) = if reported.input > 0 {
            (reported.cache_read, reported.cache_write)
        } else {
            (0, 0)
        };
        let record_usage = !reported.is_empty() || has_output;
        let mut budget_exceeded = false;
        if record_usage {
            let cost = usage_cost(
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
                prices,
            );
            engine
                .record_agent_usage_with_cache(
                    &session.id,
                    &spec.id,
                    &spec.name,
                    spec.role.as_str(),
                    spec.task_id.clone(),
                    spec.department.clone(),
                    &request.model,
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    cache_write_tokens,
                    estimated,
                    cost,
                )
                .await?;
            // Operational cache telemetry after settlement. No prompt text or
            // provider secrets; failures must not discard the accounting.
            let fingerprint =
                prompt_hashes.fingerprint_with_epoch_and_model(&prompt_epoch.hash, &model_name);
            let first_session_call = epoch_observed_calls == 0;
            cache_tracker.set_epoch_context(first_session_call, epoch_reuse.prior_sessions);
            let cache_telemetry =
                cache_tracker.observe_call(crate::prompt_cache::CacheCallObservation {
                    fingerprint: &fingerprint,
                    messages: &request.messages,
                    input_tokens,
                    cache_read_tokens,
                    cache_write_tokens,
                    cache_expected: cache_plan.enabled,
                    cache_epoch: Some(&prompt_epoch.hash),
                    cache_epoch_source: Some("l4"),
                });
            let miss_reason = cache_telemetry.miss_reason;
            let savings =
                crate::prompt_cache::round_usd(crate::prompt_cache::estimated_cache_savings_usd(
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    cache_write_tokens,
                    prices.input,
                    prices.output,
                    prices.cache_read,
                    prices.cache_write,
                ));
            if input_tokens > 0 {
                let provider_cache_miss = matches!(
                    miss_reason,
                    Some(crate::prompt_cache::CacheMissReason::ProviderCacheMiss)
                );
                let mut payload = serde_json::json!({
                    "input_tokens": input_tokens,
                    "cache_read_tokens": cache_read_tokens,
                    "cache_write_tokens": cache_write_tokens,
                    "output_tokens": output_tokens,
                    "cache_hit_ratio": vp_protocol::types::cache_hit_ratio(
                        input_tokens,
                        cache_read_tokens,
                        cache_write_tokens,
                    ),
                    "cache_read_ratio": if input_tokens == 0 {
                        0.0
                    } else {
                        (cache_read_tokens as f64 / input_tokens as f64).clamp(0.0, 1.0)
                    },
                    "cache_write_ratio": if input_tokens == 0 {
                        0.0
                    } else {
                        (cache_write_tokens as f64 / input_tokens as f64).clamp(0.0, 1.0)
                    },
                    "cache_breakpoints": cache_plan.breakpoints,
                    "cache_ttl": cache_plan.ttl.as_str(),
                    "cache_ttl_explicit": cache_plan.ttl_explicit,
                    "cache_automatic": cache_plan.automatic,
                    "first_token_ms": stream_turn.first_token_ms,
                    "core_hash": prompt_hashes.core_hash,
                    "project_hash": prompt_hashes.project_hash,
                    "tools_hash": prompt_hashes.tools_hash,
                    "tool_bundle": frozen_bundle.selection.kind.as_str(),
                    "tool_bundle_hash": active_bundle_hash,
                    "bundle_revision": tool_bundle_revision,
                    "tool_bundle_tool_count": tools.len(),
                    "tool_bundle_mcp_count": frozen_bundle.mcp_count(),
                    "volatile_hash": prompt_hashes.volatile_hash,
                    "prompt_prefix_hash": prefix_hash,
                    "cache_generation": prefix_hash,
                    "cache_epoch_source": "l4",
                    "cross_session_first_call": first_session_call,
                    "estimated_savings_usd": savings,
                    "miss_reason": miss_reason.map(|reason| reason.as_str()),
                    "estimated": estimated,
                    "warm": false,
                    "model": request.model,
                });
                let canonical_fields = crate::conversation::canonical_payload(&observation);
                if let (Some(target), Some(fields)) =
                    (payload.as_object_mut(), canonical_fields.as_object())
                {
                    for (key, value) in fields {
                        target.insert(key.clone(), value.clone());
                    }
                }
                if let Some(object) = payload.as_object_mut() {
                    object.insert(
                        "cache_miss_tokens".into(),
                        serde_json::json!(cache_telemetry.cache_miss_tokens),
                    );
                    object.insert(
                        "cache_hit_ratio_v2".into(),
                        serde_json::json!(cache_telemetry.cache_hit_ratio),
                    );
                    object.insert(
                        "theoretical_max_hit_ratio".into(),
                        serde_json::json!(cache_telemetry.theoretical_max_hit_ratio),
                    );
                    object.insert(
                        "message_prefix_hash".into(),
                        serde_json::json!(cache_telemetry.message_prefix_hash),
                    );
                    object.insert(
                        "previous_message_prefix_hash".into(),
                        serde_json::json!(cache_telemetry.previous_message_prefix_hash),
                    );
                    object.insert(
                        "message_prefix_count".into(),
                        serde_json::json!(cache_telemetry.message_prefix_count),
                    );
                    object.insert(
                        "previous_message_prefix_count".into(),
                        serde_json::json!(cache_telemetry.previous_message_prefix_count),
                    );
                    object.insert(
                        "append_only".into(),
                        serde_json::json!(cache_telemetry.append_only),
                    );
                    object.insert(
                        "previous_input_tokens".into(),
                        serde_json::json!(cache_telemetry.previous_input_tokens),
                    );
                }
                if let Some(object) = payload.as_object_mut() {
                    for (key, value) in prompt_epoch.payload_fields() {
                        object.insert(key, value);
                    }
                }
                agent_fields(&mut payload, spec);
                if provider_cache_miss && let Some(object) = payload.as_object_mut() {
                    object.insert(
                        "cache_break_reason".into(),
                        serde_json::json!("provider_cache_miss"),
                    );
                }
                let call = crate::PromptPrefixCall {
                    project_id: project.id.clone(),
                    cache_epoch: prompt_epoch.hash.clone(),
                    session_id: session.id.clone(),
                    model: model_name.clone(),
                    tool_bundle: frozen_bundle.selection.kind.as_str().to_string(),
                    tool_bundle_hash: frozen_bundle.bundle_hash.clone(),
                    core_hash: prompt_epoch.identity.core_hash.clone(),
                    project_hash: prompt_epoch.identity.project_hash.clone(),
                    core_sections: prompt_epoch.identity.core_sections.clone(),
                    project_sections: prompt_epoch.identity.project_sections.clone(),
                    input_tokens,
                    cache_read_tokens,
                };
                if let Err(error) = engine
                    .append_cache_call_event(&session.id, "context.cache", payload, &call)
                    .await
                {
                    tracing::warn!(%error, "could not persist context cache telemetry");
                } else {
                    epoch_observed_calls = epoch_observed_calls.saturating_add(1);
                }
                if provider_cache_miss {
                    let miss_payload = crate::conversation::provider_miss_payload(
                        &observation,
                        &config.provider_name,
                        &request.model,
                        input_tokens,
                        cache_read_tokens,
                    );
                    if let Err(error) = emit(
                        engine,
                        &session.id,
                        spec,
                        crate::conversation::CACHE_BREAK_EVENT,
                        miss_payload,
                    )
                    .await
                    {
                        tracing::warn!(%error, "could not persist provider cache miss telemetry");
                    }
                }
            }
            let actual_usage = input_tokens.saturating_add(output_tokens);
            let cost_exceeded = if let Some(reservation) = cost_reservation.take() {
                reservation.settle(cost)
            } else {
                budget.add_cost(cost)
            };
            budget_exceeded = token_reservation
                .take()
                .is_some_and(|reservation| reservation.settle(actual_usage))
                || cost_exceeded;
        }
        if let Some(preempted) = stream_turn.preempted {
            let output_bytes = stream_turn.text.len() as u64;
            control
                .preempt
                .annotate_usage(input_tokens, output_tokens, output_bytes, estimated);
            let snapshot = control.preempt.snapshot().unwrap_or(preempted);
            Box::pin(emit_provider_preempted(
                engine,
                session,
                project,
                spec,
                &snapshot,
                &stream_turn.text,
                &stream_turn.tool_calls,
            ))
            .await?;
            journal
                .model_failed(
                    engine,
                    &model_step,
                    &format!(
                        "provider stream preempted during {} ({})",
                        snapshot.phase.as_str(),
                        snapshot.reason.as_str()
                    ),
                )
                .await?;
            return Ok(LoopOutcome::Cancelled);
        }
        if stream_turn.cancelled {
            journal
                .model_failed(
                    engine,
                    &model_step,
                    "the run was cancelled during the provider stream",
                )
                .await?;
            return Ok(LoopOutcome::Cancelled);
        }
        if let Some(error) = stream_turn.error {
            // Keep partial streamed output visible instead of letting the
            // failure clear the live transcript.
            if !stream_turn.text.is_empty() {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "agent.message",
                    serde_json::json!({
                        "text": stream_turn.text,
                        "tool_calls": stream_turn.tool_calls,
                        "partial": true,
                    }),
                )
                .await?;
            }
            journal
                .model_failed(engine, &model_step, &format!("{error:#}"))
                .await?;
            return Err(error);
        }
        if budget_exceeded {
            journal
                .model_failed(
                    engine,
                    &model_step,
                    "the provider response arrived but the session budget was exhausted before it could be persisted",
                )
                .await?;
            return Ok(LoopOutcome::BudgetExceeded(BudgetStop::session(budget)));
        }
        if !budget.is_unlimited() && plan_budget.active() {
            let (task_tokens, task_cost, department_tokens, department_cost) = engine
                .usage_spent(
                    &session.id,
                    spec.task_id.as_deref(),
                    spec.department.as_deref(),
                )
                .await?;
            let project_usage = if plan_budget.project_active() {
                engine.project_usage(&project.id).await?
            } else {
                (0, 0.0)
            };
            if let Some(stop) = plan_budget.after(
                (task_tokens, task_cost),
                (department_tokens, department_cost),
                project_usage,
            ) {
                emit_scope_budget_stop(engine, &session.id, spec, &stop).await?;
                return Ok(LoopOutcome::BudgetExceeded(stop));
            }
        }
        let text = stream_turn.text;
        let tool_calls = stream_turn.tool_calls;

        if text.is_empty() && tool_calls.is_empty() {
            journal
                .model_completed(
                    engine,
                    &model_step,
                    None,
                    "",
                    &[],
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    cache_write_tokens,
                    estimated,
                )
                .await?;
            emit(
                engine,
                &session.id,
                spec,
                "agent.turn_finished",
                serde_json::json!({"turn": turn + 1, "empty": true}),
            )
            .await?;
            return Ok(LoopOutcome::Final(String::new()));
        }

        let assistant_tool_calls = if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls.clone())
        };
        let stored_id = if top_level {
            let stored = engine
                .append_message(
                    &session.id,
                    "assistant",
                    &text,
                    serde_json::to_value(assistant_tool_calls.as_deref().unwrap_or(&[]))?,
                    None,
                )
                .await?;
            conversation_ids.push(Some(stored.id.clone()));
            Some(stored.id)
        } else {
            None
        };
        let citation_records = citations.list();
        emit(
            engine,
            &session.id,
            spec,
            "agent.message",
            serde_json::json!({
                "text": text,
                "tool_calls": tool_calls,
                "message_id": stored_id,
                "citations": citation_records,
            }),
        )
        .await?;
        conversation.push(ChatMessage {
            role: "assistant".to_string(),
            content: text.clone(),
            tool_calls: assistant_tool_calls,
            tool_call_id: None,
        });
        journal
            .model_completed(
                engine,
                &model_step,
                stored_id.as_deref(),
                &text,
                &tool_calls,
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
                estimated,
            )
            .await?;

        if !wait_for_task_resume(engine, &session.id, spec, control).await? {
            return Ok(LoopOutcome::Cancelled);
        }
        if tool_calls.is_empty() {
            emit(
                engine,
                &session.id,
                spec,
                "agent.turn_finished",
                serde_json::json!({"turn": turn + 1}),
            )
            .await?;
            return Ok(LoopOutcome::Final(text));
        }

        let safe_parallel_batch = tool_calls.len() > 1
            && tool_calls.iter().all(|call| {
                is_parallel_safe_tool(&call.name)
                    && spec.role.allows_tool(spec.profile, &call.name)
                    && active_skill
                        .as_ref()
                        .is_none_or(|skill| skill_allows_tool(skill, &call.name))
                    && (config.tools.get(&call.name).is_some() || call.name == "read_tool_output")
            });
        let gui_policy = engine
            .gui_policy_for_session(&session.id)
            .await
            .unwrap_or_default();
        if safe_parallel_batch {
            let mut prepared = Vec::new();
            for (index, call) in tool_calls.iter().enumerate() {
                let call_id = if call.id.is_empty() {
                    format!("call_{}_{}", turn, index)
                } else {
                    call.id.clone()
                };
                let input: serde_json::Value =
                    serde_json::from_str(&call.arguments).unwrap_or_else(|_| serde_json::json!({}));
                let assessment = crate::permissions::assess_tool_call_with_gui(
                    &call.name,
                    &input,
                    &spec.worktree,
                    &gui_policy,
                );
                // A contract denial/escalation on a read-only parallel call
                // falls back to the sequential path, which records the intent
                // decision and takes the normal approval route.
                if let Some(contract) = &run_contract {
                    let contract_spec = crate::contract::ContractCallSpec {
                        tool: &call.name,
                        contract_tool: None,
                        input: &input,
                        risk: assessment.risk,
                        worktree: &spec.worktree,
                        usage: run_contract_usage,
                        mcp_trust: None,
                        patch_text: None,
                    };
                    if !crate::contract::authorize(&contract_spec, contract, &run_contract_policy)
                        .is_allow()
                    {
                        prepared.clear();
                        break;
                    }
                }
                if !matches!(
                    decide(config.approval_mode, &assessment, spec.auto_approve),
                    ApprovalDecision::Allow { .. }
                ) {
                    prepared.clear();
                    break;
                }
                let tool = config.tools.get(&call.name);
                if tool.is_none() && call.name != "read_tool_output" {
                    prepared.clear();
                    break;
                }
                prepared.push((
                    index,
                    call_id,
                    call.name.clone(),
                    input,
                    tool,
                    assessment.risk,
                ));
            }
            if !prepared.is_empty() {
                let mut journal_pairs = std::collections::HashMap::new();
                for (_, call_id, name, input, _, _) in &prepared {
                    if let Err(error) = engine
                        .touch_task_progress(
                            &session.id,
                            spec.task_id.as_deref(),
                            turn + 1,
                            Some(name),
                        )
                        .await
                    {
                        tracing::warn!(%error, "failed to record task tool progress");
                    }
                    let (step, tool, _) = journal
                        .tool_intent(engine, turn + 1, call_id, name, input, None)
                        .await?;
                    journal_pairs.insert(call_id.to_string(), (step, tool));
                    emit(
                        engine,
                        &session.id,
                        spec,
                        "tool.started",
                        serde_json::json!({
                            "tool_call_id": call_id,
                            "name": name,
                            "input": input,
                        }),
                    )
                    .await?;
                }
                let mut cancel_rx = control.subscribe();
                // Parallel tools are a short read-only batch. A per-tool
                // deadline is enforced on sequential tools; the batch observes
                // the run token so one slow read cannot leave siblings running
                // after an operator/wall-clock preemption.
                let batch_token = crate::preemption::Token::tool_child(&control.preempt, None);
                batch_token.set_phase(crate::preemption::Phase::Tool, None);
                // Each tool gets its own deadline token in addition to the
                // run/batch token. A tool deadline must abort only that tool;
                // the run token still aborts the whole batch on operator or
                // wall-clock preemption.
                let tool_deadline = crate::preemption::tool_deadline(&control.preemption_policy());
                let mut pending = prepared.into_iter();
                let mut set = tokio::task::JoinSet::new();
                for _ in 0..4 {
                    let Some((index, call_id, name, input, tool, risk)) = pending.next() else {
                        break;
                    };
                    let context = tool_context.clone();
                    let engine = engine.clone();
                    let session = session.clone();
                    let tool_token =
                        crate::preemption::Token::tool_child(&batch_token, tool_deadline);
                    tool_token.set_phase(crate::preemption::Phase::Tool, Some(&name));
                    set.spawn(async move {
                        let started = Instant::now();
                        let (mut output, deadline) = execute_parallel_tool(
                            tool,
                            engine,
                            session,
                            context,
                            tool_token,
                            input,
                            name.clone(),
                        )
                        .await;
                        output.normalize_failure(&name);
                        let duration_ms =
                            started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
                        (index, call_id, name, output, duration_ms, risk, deadline)
                    });
                }
                let mut results = Vec::new();
                loop {
                    if set.is_empty() && pending.len() == 0 {
                        break;
                    }
                    tokio::select! {
                        _ = cancel_rx.changed() => {
                            set.abort_all();
                            return Ok(LoopOutcome::Cancelled);
                        }
                        _ = batch_token.cancelled() => {
                            set.abort_all();
                            if let Some(snapshot) = batch_token.snapshot() {
                                let mut snapshot = snapshot;
                                snapshot.detail = Some("parallel_batch_aborted".to_string());
                                Box::pin(emit_tool_preempted(
                                    engine, session, project, spec, &snapshot,
                                ))
                                .await?;
                            }
                            return Ok(LoopOutcome::Cancelled);
                        }
                        joined = set.join_next() => {
                            match joined {
                                Some(Ok(item)) => results.push(item),
                                Some(Err(error)) => {
                                    return Err(anyhow!("parallel tool task failed: {error}"));
                                }
                                None => break,
                            }
                            if let Some((index, call_id, name, input, tool, risk)) = pending.next() {
                                let context = tool_context.clone();
                                let engine = engine.clone();
                                let session = session.clone();
                                let tool_token = crate::preemption::Token::tool_child(
                                    &batch_token,
                                    tool_deadline,
                                );
                                tool_token
                                    .set_phase(crate::preemption::Phase::Tool, Some(&name));
                                set.spawn(async move {
                                    let started = Instant::now();
                                    let (mut output, deadline) = execute_parallel_tool(
                                        tool,
                                        engine,
                                        session,
                                        context,
                                        tool_token,
                                        input,
                                        name.clone(),
                                    )
                                    .await;
                                    output.normalize_failure(&name);
                                    let duration_ms =
                                        started.elapsed().as_millis().min(u128::from(u64::MAX))
                                            as u64;
                                    (index, call_id, name, output, duration_ms, risk, deadline)
                                });
                            }
                        }
                    }
                }
                results.sort_by_key(|(index, _, _, _, _, _, _)| *index);
                // A per-tool deadline is recorded as that tool's event only.
                // It never changes session status and never aborts siblings.
                for (_, _, _, _, _, _, deadline) in &results {
                    if let Some(snapshot) = deadline
                        && snapshot.reason == crate::preemption::Reason::ToolDeadline
                    {
                        Box::pin(emit_tool_preempted(
                            engine, session, project, spec, snapshot,
                        ))
                        .await?;
                    }
                }
                let mut parallel_metrics = std::collections::HashMap::new();
                let pending = results
                    .into_iter()
                    .map(|(_, call_id, name, output, duration_ms, risk, _)| {
                        parallel_metrics.insert(call_id.clone(), (duration_ms, risk));
                        crate::tool_output::PendingToolOutput {
                            tool_call_id: call_id,
                            tool_name: name,
                            output,
                        }
                    })
                    .collect::<Vec<_>>();
                let (mut shaped, _) = shape_tool_output_batch(
                    engine,
                    session,
                    spec,
                    project,
                    turn_tool_bytes,
                    pending,
                )
                .await?;
                for item in &mut shaped {
                    let call_id = item.tool_call_id.clone();
                    let name = item.tool_name.clone();
                    let output = &mut item.output;
                    output.annotate_shape_metadata();
                    if let Some((journal_step, journal_tool)) = journal_pairs.remove(&call_id) {
                        journal
                            .tool_completed(engine, &journal_step, &journal_tool, output)
                            .await?;
                    }
                    let (duration_ms, risk) = parallel_metrics
                        .get(&call_id)
                        .copied()
                        .unwrap_or((0, RiskLevel::ReadOnly));
                    emit(
                        engine,
                        &session.id,
                        spec,
                        "tool.finished",
                        tool_finished_payload(
                            &call_id,
                            &name,
                            output,
                            duration_ms,
                            Some(risk),
                            config.approval_mode,
                            None,
                        ),
                    )
                    .await?;
                    if top_level {
                        let stored = engine
                            .append_message(
                                &session.id,
                                "tool",
                                &output.content,
                                serde_json::json!([]),
                                Some(call_id.clone()),
                            )
                            .await?;
                        conversation_ids.push(Some(stored.id));
                    }
                    conversation.push(ChatMessage::tool(call_id, output.content.clone()));
                }
                emit(
                    engine,
                    &session.id,
                    spec,
                    "agent.turn_finished",
                    serde_json::json!({"turn": turn + 1, "parallel": true}),
                )
                .await?;
                continue;
            }
        }

        // Capture every serial call id before the iterator consumes the list.
        // If one call defers the run, the remaining siblings must still get a
        // persisted tool result: provider tool-call history requires one
        // result per call, and a resume would otherwise send an invalid turn.
        let serial_call_ids = tool_calls
            .iter()
            .enumerate()
            .map(|(idx, call)| {
                let id = if call.id.is_empty() {
                    format!("call_{}_{}", turn, idx)
                } else {
                    call.id.clone()
                };
                (id, call.name.clone())
            })
            .collect::<Vec<_>>();
        for (index, call) in tool_calls.into_iter().enumerate() {
            if !wait_for_task_resume(engine, &session.id, spec, control).await? {
                return Ok(LoopOutcome::Cancelled);
            }
            if control.is_cancelled() || control.is_preempted() {
                return Ok(LoopOutcome::Cancelled);
            }
            if budget.exhausted() {
                return Ok(LoopOutcome::BudgetExceeded(BudgetStop::session(budget)));
            }
            let mut imposed_preempt: Option<crate::preemption::Snapshot> = None;
            if let Err(error) = engine
                .touch_task_progress(
                    &session.id,
                    spec.task_id.as_deref(),
                    turn + 1,
                    Some(&call.name),
                )
                .await
            {
                tracing::warn!(%error, "failed to record task tool progress");
            }
            let call_id = if call.id.is_empty() {
                format!("call_{}_{}", turn, index)
            } else {
                call.id.clone()
            };
            let input: serde_json::Value =
                serde_json::from_str(&call.arguments).unwrap_or_else(|_| serde_json::json!({}));
            // The probe needs the original arguments after dispatch consumes
            // `input`; cloning one bounded JSON value is cheaper than threading
            // it through every tool branch.
            let effect_input = input.clone();
            // Duplicate-write gate runs before approval and before any journal
            // intent: an already-completed write is returned from the journal,
            // not executed again. Read-only tools re-run normally.
            let duplicate = journal
                .check_duplicate_write(engine, &call.name, &input)
                .await?;
            if duplicate.is_some() {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                        "journal_replay": true,
                    }),
                )
                .await?;
            }
            let is_delegate = call.name == "delegate" && spec.allow_delegation;
            let is_acp_delegate = call.name == "delegate_acp"
                && spec.allow_delegation
                && matches!(spec.role, AgentRole::Leader | AgentRole::Manager);
            let is_delegate_many = call.name == "delegate_many"
                && spec.role == AgentRole::Manager
                && spec.allow_delegation;
            let is_verdict = call.name == "submit_verdict" && spec.role == AgentRole::Verifier;
            let is_team_plan = call.name == "submit_team_plan" && spec.role == AgentRole::Leader;
            let is_plan_fanout = call.name == "plan_fanout" && spec.role == AgentRole::Leader;
            let is_run_plan = call.name == "run_team_plan" && spec.role == AgentRole::Leader;
            let is_deliberate =
                call.name == "deliberate_team_plan" && spec.role == AgentRole::Leader;
            let is_todo = call.name == "todo_write" && spec.role == AgentRole::Leader;
            let is_mailbox_tool = matches!(
                call.name.as_str(),
                "send_message"
                    | "read_mailbox"
                    | "publish_artifact"
                    | "list_artifacts"
                    | "read_artifact"
            );
            let is_memory_tool = matches!(call.name.as_str(), "remember" | "recall");
            let is_read_tool_output = call.name == "read_tool_output";
            let is_source_tool = matches!(
                call.name.as_str(),
                "search_sources" | "read_source" | "list_sources"
            );
            let is_skill_tool = call.name == "use_skill";
            let is_question_tool = call.name == "ask_user" && spec.role != AgentRole::Verifier;
            let is_wait_for_gate = call.name == "wait_for_gate"
                && matches!(
                    spec.role,
                    AgentRole::Leader | AgentRole::Manager | AgentRole::Worker
                )
                && !(spec.role == AgentRole::Manager && !spec.allow_delegation);
            let is_mcp_tool = call.name == "mcp";
            let is_coordination_tool = call.name == "coordinate";
            let is_team_status = call.name == "team_status";
            let is_budget_status = call.name == "budget_status";
            let is_task_detail = call.name == "task_detail";
            let is_task_control = call.name == "task_control"
                && matches!(spec.role, AgentRole::Leader | AgentRole::Manager);
            let is_tool_search = call.name == "tool_search";
            let is_tool_help = call.name == "tool_help";
            let is_progress_report = call.name == "report_progress"
                && matches!(spec.role, AgentRole::Leader | AgentRole::Manager);
            let is_run_script = call.name == "run_script"
                && matches!(
                    spec.role,
                    AgentRole::Leader | AgentRole::Worker | AgentRole::Labour
                );
            let is_artifact_file = call.name == "publish_artifact_file";
            let is_archive_tool = matches!(call.name.as_str(), "archive_list" | "archive_extract");
            let is_media_tool = matches!(call.name.as_str(), "media_info" | "media_transcode");
            let is_scout = call.name == "scout"
                && spec.allow_delegation
                && matches!(spec.role, AgentRole::Leader | AgentRole::Manager);
            let is_browser =
                call.name == "browser" && spec.role.allows_tool(spec.profile, "browser");
            let is_computer =
                call.name == "computer" && spec.role.allows_tool(spec.profile, "computer");
            let dynamic_mcp_tool = dynamic_mcp_tools
                .iter()
                .find(|binding| binding.exposed_name == call.name);
            let registry_tool = config
                .tools
                .get(&call.name)
                .filter(|_| spec.role.allows_tool(spec.profile, &call.name));
            let scout_blocked =
                spec.role == AgentRole::Scout && !spec.role.allows_tool(spec.profile, &call.name);
            // A registry tool outside the role policy is rejected by the
            // existing role check with its historical message. Do not let the
            // contract replace that denial.
            let role_policy_unavailable = config.tools.get(&call.name).is_some()
                && !spec.role.allows_tool(spec.profile, &call.name);
            let mcp_trust = if let Some(binding) = dynamic_mcp_tool {
                Some(binding.pin_trust)
            } else if is_mcp_tool && input.get("action").and_then(Value::as_str) == Some("call") {
                Some(resolve_mcp_pin_trust(engine, &session.project_id, &input).await)
            } else {
                None
            };
            let contract_tool = dynamic_mcp_tool
                .map(|_| "mcp")
                .or(is_mcp_tool.then_some("mcp"));
            let authorization = authorize_tool_call(
                engine,
                config,
                session,
                spec,
                control,
                journal,
                &call_id,
                &call.name,
                &input,
                turn + 1,
                run_contract.as_ref(),
                &run_contract_policy,
                &mut run_contract_usage,
                active_skill.as_ref(),
                &gui_policy,
                contract_tool,
                dynamic_mcp_tool.is_some(),
                mcp_trust,
                role_policy_unavailable,
                scout_blocked,
                false,
                false,
                None,
                None,
            )
            .await?;
            if authorization.cancelled {
                return Ok(LoopOutcome::Cancelled);
            }
            let approval_risk = authorization.risk;
            let approval_wait_ms = authorization.approval_wait_ms;
            let denied_reason = authorization.denied_reason;
            let skill_denied = authorization.skill_denied;
            // Effect receipts are written in the same transaction as the run
            // journal intent. A completed logical effect replays its stored
            // evidence without executing; an unresolved one blocks execution
            // until an operator reconciles it. A denied/skill-blocked call
            // never creates an effect intent because it never runs.
            let effect_gate = prepare_tool_effect_gate(
                engine,
                session,
                spec,
                journal,
                turn + 1,
                &call_id,
                &call.name,
                &input,
                approval_risk,
                denied_reason.is_some(),
                skill_denied.is_some(),
                dynamic_mcp_tool.is_some(),
                &effect_policy,
                &tool_context,
                duplicate.is_some(),
            )
            .await?;
            let planned_effect = effect_gate.planned_effect;
            let effect_override = effect_gate.effect_override;
            let mut effect_begin = effect_gate.effect_begin;
            let effect_duplicate = effect_gate.effect_duplicate;
            let journal_pair = effect_gate.journal_pair;
            let effect_classification_failed = effect_gate.classification_failed;
            if effect_override.is_some() {
                // No dispatch branch runs for a replayed or blocked effect, so
                // emit the same start marker the executing branches emit.
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                        "effect_blocked": !effect_classification_failed
                            && effect_override
                                .as_ref()
                                .is_some_and(|output| output.is_error),
                        "effect_classification_failed": effect_classification_failed,
                    }),
                )
                .await?;
            }
            control
                .preempt
                .set_phase(crate::preemption::Phase::Tool, Some(&call.name));
            let tool_deadline = crate::preemption::tool_deadline(&control.preemption_policy());
            let tool_token = crate::preemption::Token::tool_child(&control.preempt, tool_deadline);
            tool_token.set_phase(crate::preemption::Phase::Tool, Some(&call.name));
            let tool_started = Instant::now();
            let output = if let Some(mut output) = effect_override {
                if let Some(receipt) = &effect_begin {
                    annotate_effect_output(&mut output, receipt);
                }
                output
            } else if let Some(mut output) = duplicate {
                if let Some(receipt) = &effect_duplicate {
                    annotate_effect_output(&mut output, receipt);
                }
                output
            } else if let Some(reason) = denied_reason {
                ToolOutput::error_coded(
                    call.name.as_str(),
                    crate::tooling::errors::ToolErrorCode::PERMISSION_DENIED,
                    reason,
                )
                .with_failure_evidence(format!("tool={}", call.name))
            } else if let Some(skill) = skill_denied {
                ToolOutput::error_coded(
                    call.name.as_str(),
                    crate::tooling::errors::ToolErrorCode::SKILL_TOOL_DENIED,
                    format!(
                        "tool '{}' is not allowed by active skill '{}'",
                        call.name, skill.name
                    ),
                )
                .with_failure_evidence(format!("skill={}", skill.slug))
            } else if is_progress_report {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match run_progress_report(engine, session, spec, &input).await {
                    Ok(output) => output,
                    Err(error) => ToolOutput::error(format!("report_progress failed: {error}")),
                }
            } else if is_run_script {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match run_script_tool(
                    engine,
                    config,
                    session,
                    spec,
                    control,
                    journal,
                    &input,
                    &tools,
                    &call_id,
                    turn + 1,
                    run_contract.as_ref(),
                    &run_contract_policy,
                    &mut run_contract_usage,
                    active_skill.as_ref(),
                    &gui_policy,
                    &effect_policy,
                    &tool_context,
                    &tool_token,
                )
                .await?
                {
                    ScriptToolOutcome::Output(output) => output,
                    ScriptToolOutcome::Cancelled => return Ok(LoopOutcome::Cancelled),
                }
            } else if is_deliberate {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match engine.get_team_plan(&session.id).await? {
                    None => ToolOutput::error("no TeamPlan submitted; call submit_team_plan first"),
                    Some(plan) => {
                        let rounds = input
                            .get("rounds")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(2)
                            .clamp(1, 3) as u32;
                        match deliberate_team_plan(
                            engine, config, session, project, control, budget, spec, &plan, rounds,
                        )
                        .await?
                        {
                            RoundtableResult::Done(report) => ToolOutput::ok(report),
                            RoundtableResult::Cancelled => {
                                return Ok(LoopOutcome::Cancelled);
                            }
                            RoundtableResult::BudgetExceeded(stop) => {
                                if stop.is_abortive() {
                                    return Ok(LoopOutcome::BudgetExceeded(stop));
                                }
                                ToolOutput::error(stop.summary())
                            }
                        }
                    }
                }
            } else if is_run_plan {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match engine.get_team_plan(&session.id).await? {
                    None => ToolOutput::error("no TeamPlan submitted; call submit_team_plan first"),
                    Some(plan) => match execute_team_plan(
                        engine, config, session, project, control, budget, spec, &plan, None,
                    )
                    .await?
                    {
                        PlanRunResult::Done(report) => ToolOutput::ok(report),
                        PlanRunResult::Cancelled => return Ok(LoopOutcome::Cancelled),
                        PlanRunResult::Deferred(wait) => {
                            return Ok(LoopOutcome::Deferred(wait));
                        }
                        PlanRunResult::BudgetExceeded(stop) => {
                            return Ok(LoopOutcome::BudgetExceeded(stop));
                        }
                    },
                }
            } else if is_team_plan {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                let parsed = serde_json::from_value::<TeamPlan>(input.clone())
                    .map_err(|error| anyhow!("invalid team plan: {error}"))
                    .and_then(|plan| {
                        validate_team_plan(&plan, config.max_managers)?;
                        Ok(plan)
                    });
                match parsed {
                    Ok(plan) => {
                        let summary = format!(
                            "Team plan recorded: {} departments, {} tasks. Delegate to Managers using the planned titles.",
                            plan.departments.len(),
                            plan.tasks.len()
                        );
                        engine.set_team_plan(&session.id, &plan).await?;
                        team_plan = Some(plan);
                        ToolOutput::ok(summary)
                    }
                    Err(error) => ToolOutput::error(error.to_string()),
                }
            } else if is_plan_fanout {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                let params =
                    serde_json::from_value::<vp_protocol::types::PlanFanoutParams>(input.clone())
                        .map(|mut params| {
                            // The HostState approval broker already gated this call;
                            // the tool itself cannot pre-approve a plan rewrite.
                            params.session_id = session.id.clone();
                            params.approved = true;
                            params.author = Some(spec.role.as_str().to_string());
                            params
                        });
                match params {
                    Ok(params) => match engine.fanout_plan(params).await {
                        Ok(result) => ToolOutput::ok_structured(
                            format!(
                                "Plan fan-out applied at revision {}: {} child task(s), join {}.",
                                result.revision,
                                result.child_task_ids.len(),
                                result.join_task_id
                            ),
                            serde_json::to_value(&result).unwrap_or_else(|_| serde_json::json!({})),
                        ),
                        Err(error) => {
                            let code = error
                                .downcast_ref::<crate::fanout::FanoutError>()
                                .map(|typed| typed.code)
                                .unwrap_or(crate::fanout::codes::FANOUT_INVALID);
                            let tool_code = if code == crate::fanout::codes::FANOUT_CONFLICT {
                                crate::tooling::errors::ToolErrorCode::CONFLICT
                            } else {
                                crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT
                            };
                            ToolOutput::error_coded(
                                call.name.as_str(),
                                tool_code,
                                format!("plan fanout failed: {error}"),
                            )
                        }
                    },
                    Err(error) => ToolOutput::error_coded(
                        call.name.as_str(),
                        crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
                        format!("invalid plan fanout arguments: {error}"),
                    ),
                }
            } else if is_coordination_tool {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match run_coordination_tool(engine, session, spec, input.clone()).await {
                    Ok(output) => output,
                    Err(error) => ToolOutput::error(format!("coordination error: {error}")),
                }
            } else if is_team_status {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match run_team_status(engine, session, input.clone()).await {
                    Ok(output) => output,
                    Err(error) => ToolOutput::error(format!("team status error: {error}")),
                }
            } else if is_budget_status {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match run_budget_status(engine, session, project, spec, budget, &plan_budget).await
                {
                    Ok(output) => output,
                    Err(error) => ToolOutput::error(format!("budget status error: {error}")),
                }
            } else if is_task_detail {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match run_task_detail(engine, session, spec, input.clone()).await {
                    Ok(output) => output,
                    Err(error) => ToolOutput::error(format!("task detail error: {error}")),
                }
            } else if is_task_control {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match run_task_control(engine, session, spec, input.clone()).await {
                    Ok(output) => output,
                    Err(error) => ToolOutput::from_tool_error(&call.name, &error),
                }
            } else if is_tool_help {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                run_tool_help(spec, &input)
            } else if is_tool_search {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                let output = match run_tool_search(
                    config,
                    engine,
                    spec,
                    &session.project_id,
                    &frozen_bundle,
                    input.clone(),
                )
                .await
                {
                    Ok(output) => output,
                    Err(error) => ToolOutput::error(format!("tool search error: {error}")),
                };
                if let Some(candidates) = output
                    .structured
                    .as_ref()
                    .and_then(|value| value.get("promotion"))
                    .filter(|promotion| {
                        promotion
                            .get("requested")
                            .and_then(Value::as_bool)
                            .unwrap_or(false)
                    })
                    .and_then(|promotion| promotion.get("candidates"))
                    .and_then(Value::as_array)
                {
                    for candidate in candidates.iter().filter_map(Value::as_str) {
                        pending_promotions.insert(candidate.to_string());
                    }
                }
                output
            } else if is_artifact_file {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match run_publish_artifact_file(engine, session, spec, input.clone()).await {
                    Ok(output) => output,
                    Err(error) => ToolOutput::error(format!("artifact publish error: {error}")),
                }
            } else if is_archive_tool {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                run_archive_tool(engine, &call.name, input.clone()).await
            } else if is_media_tool {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                run_media_tool(engine, spec, &call.name, input.clone()).await
            } else if is_browser {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match tool_token
                    .scoped(engine.browser_action(
                        &session.id,
                        &spec.worktree,
                        &spec.id,
                        &spec.name,
                        input.clone(),
                    ))
                    .await
                {
                    Ok(output) => output,
                    Err(error) => ToolOutput::from_tool_error("browser", &error),
                }
            } else if is_computer {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match tool_token
                    .scoped(engine.computer_action(
                        &session.id,
                        &spec.worktree,
                        &spec.id,
                        &spec.name,
                        spec.task_id.as_deref(),
                        input.clone(),
                    ))
                    .await
                {
                    Ok(output) => output,
                    Err(error) => ToolOutput::from_tool_error("computer", &error),
                }
            } else if is_mailbox_tool {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match run_mailbox_tool(engine, session, spec, &call.name, input.clone()).await {
                    Ok(output) => output,
                    Err(error) => ToolOutput::from_tool_error(&call.name, &error),
                }
            } else if let Some(binding) = dynamic_mcp_tool {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                let context = crate::mcp::tasks::McpTaskContext::from_session(
                    &session.project_id,
                    &session.id,
                    &spec.id,
                    &spec.name,
                    spec.task_id.as_deref(),
                    effect_begin.as_ref().map(|receipt| receipt.id.as_str()),
                );
                match tool_token
                    .scoped(engine.mcp_call_with_context(
                        &binding.server_id,
                        &binding.tool_name,
                        input.clone(),
                        context,
                    ))
                    .await
                {
                    Ok(result) => {
                        let mut text = format_mcp_tool_result(&result.result);
                        if let Some(task) = &result.task {
                            text.push_str(&format!(
                                "\nAsync MCP task {} is {}. Poll explicitly with mcp action=task_result task_id={} (poll interval {} ms).",
                                task.id, task.status, task.id, task.poll_interval_ms
                            ));
                        }
                        ToolOutput::ok(text)
                    }
                    Err(error) => tool_token
                        .snapshot()
                        .map(|snapshot| crate::preemption::tool_output("mcp", &snapshot))
                        .unwrap_or_else(|| ToolOutput::from_tool_error("mcp", &error)),
                }
            } else if is_mcp_tool {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match tool_token
                    .scoped(run_mcp_tool(
                        engine,
                        session,
                        spec,
                        input.clone(),
                        effect_begin.as_ref().map(|receipt| receipt.id.as_str()),
                    ))
                    .await
                {
                    Ok(output) => output,
                    Err(error) => tool_token
                        .snapshot()
                        .map(|snapshot| crate::preemption::tool_output("mcp", &snapshot))
                        .unwrap_or_else(|| ToolOutput::from_tool_error("mcp", &error)),
                }
            } else if is_wait_for_gate {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                let gate_id = input
                    .get("gate_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|gate_id| !gate_id.is_empty());
                if let Some(gate_id) = gate_id {
                    let note = input.get("note").and_then(serde_json::Value::as_str);
                    match engine.create_gate_wait(&session.id, gate_id, note).await {
                        Ok((gate, continuation)) => {
                            let summary = format!(
                                "Waiting for external gate {} ({}, {}).",
                                gate.label,
                                gate.kind.as_str(),
                                gate.state.as_str()
                            );
                            deferred_wait = Some(DeferredWait {
                                gate_id: gate.id.clone(),
                                continuation_id: continuation.id.clone(),
                                summary: summary.clone(),
                            });
                            ToolOutput::ok_structured(
                                format!(
                                    "{summary} The run will end after this tool result; no Worker slot is held."
                                ),
                                serde_json::json!({
                                    "waiting": true,
                                    "gate_id": gate.id,
                                    "kind": gate.kind.as_str(),
                                    "label": gate.label,
                                    "state": gate.state.as_str(),
                                    "continuation_id": continuation.id,
                                    "status": continuation.status,
                                }),
                            )
                        }
                        Err(error) => gate_tool_error("wait_for_gate", &error),
                    }
                } else {
                    ToolOutput::error_coded(
                        "wait_for_gate",
                        crate::tooling::errors::ToolErrorCode::GATE_INVALID,
                        "gate_id is required and must name an existing open gate",
                    )
                }
            } else if is_question_tool {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                let question = input
                    .get("question")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| anyhow!("question is required"))?;
                let options = input
                    .get("options")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                let (item, receiver) = engine
                    .request_question(&session.id, &spec.id, &spec.name, question, options)
                    .await?;
                let question_step = journal
                    .question_intent(engine, turn + 1, &item.id, &item.question, &item.options)
                    .await?;
                engine
                    .update_session_status(&session.id, "waiting_input")
                    .await?;
                engine
                    .update_agent_status(&spec.id, "waiting_input")
                    .await?;
                emit(
                    engine,
                    &session.id,
                    spec,
                    "agent.waiting_input",
                    serde_json::json!({
                        "question_id": item.id,
                        "question": item.question,
                        "options": item.options,
                    }),
                )
                .await?;
                match wait_for_question(receiver, control, config.question_timeout_secs).await {
                    QuestionOutcome::Answered(answer) => {
                        journal
                            .question_completed(engine, &question_step, "answered")
                            .await?;
                        engine.update_session_status(&session.id, "running").await?;
                        engine.update_agent_status(&spec.id, "running").await?;
                        ToolOutput::ok(format!("User answered: {answer}"))
                    }
                    QuestionOutcome::Cancelled => {
                        let _ = engine.close_question(&item.id, "cancelled").await;
                        journal
                            .question_completed(engine, &question_step, "cancelled")
                            .await?;
                        engine.update_agent_status(&spec.id, "cancelled").await?;
                        return Ok(LoopOutcome::Cancelled);
                    }
                    QuestionOutcome::TimedOut => {
                        let _ = engine.close_question(&item.id, "expired").await;
                        journal
                            .question_completed(engine, &question_step, "timed_out")
                            .await?;
                        engine.update_session_status(&session.id, "running").await?;
                        engine.update_agent_status(&spec.id, "running").await?;
                        ToolOutput::error("User did not answer before the question timed out.")
                    }
                }
            } else if is_skill_tool {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                let output =
                    match run_skill_tool(engine, project, spec, input.clone(), &mut active_skill)
                        .await
                    {
                        Ok(output) => output,
                        Err(error) => ToolOutput::error(format!("skill error: {error}")),
                    };
                if !output.is_error
                    && let Some(skill) = &active_skill
                {
                    emit(
                        engine,
                        &session.id,
                        spec,
                        "skill.activated",
                        serde_json::json!({
                            "name": skill.name,
                            "slug": skill.slug,
                            "version": skill.version,
                            "role": spec.role.as_str(),
                            "profile": spec.profile.as_str(),
                            "tool_allow": skill.tool_allow,
                            "tool_deny": skill.tool_deny,
                            "agent_id": spec.id,
                            "agent_name": spec.name,
                            "task_id": spec.task_id,
                        }),
                    )
                    .await?;
                }
                output
            } else if is_source_tool {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match run_source_tool(engine, project, &call.name, input.clone(), &mut citations)
                    .await
                {
                    Ok(output) => output,
                    Err(error) => ToolOutput::from_tool_error(&call.name, &error),
                }
            } else if is_read_tool_output {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                run_read_tool_output(engine, session, input.clone()).await
            } else if is_memory_tool {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match run_memory_tool(engine, session, project, spec, &call.name, input.clone())
                    .await
                {
                    Ok(output) => output,
                    Err(error) => ToolOutput::from_tool_error(&call.name, &error),
                }
            } else if is_verdict {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match parse_verdict(&input) {
                    Ok(verdict) => {
                        let verdict_output = ToolOutput::ok("verdict recorded");
                        if let Some((journal_step, journal_tool)) = &journal_pair {
                            journal
                                .tool_completed(engine, journal_step, journal_tool, &verdict_output)
                                .await?;
                        }
                        emit(
                            engine,
                            &session.id,
                            spec,
                            "tool.finished",
                            tool_finished_payload(
                                &call_id,
                                &call.name,
                                &verdict_output,
                                tool_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                                Some(approval_risk),
                                config.approval_mode,
                                approval_wait_ms,
                            ),
                        )
                        .await?;
                        return Ok(LoopOutcome::Verdict(verdict));
                    }
                    Err(error) => ToolOutput::error_coded(
                        "submit_verdict",
                        crate::tooling::errors::ToolErrorCode::VERDICT_INVALID,
                        error.to_string(),
                    ),
                }
            } else if is_delegate_many {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match run_parallel_workers(
                    engine, config, session, project, control, budget, spec, &input,
                )
                .await?
                {
                    ParallelResult::Output(output) => output,
                    ParallelResult::Cancelled => return Ok(LoopOutcome::Cancelled),
                    ParallelResult::BudgetExceeded(stop) => {
                        return Ok(LoopOutcome::BudgetExceeded(stop));
                    }
                    ParallelResult::Deferred(wait) => {
                        return Ok(LoopOutcome::Deferred(wait));
                    }
                }
            } else if is_scout {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match run_scout(
                    engine, config, session, project, control, budget, spec, &input,
                )
                .await?
                {
                    ScoutRunResult::Output(output) => output,
                    ScoutRunResult::Cancelled => return Ok(LoopOutcome::Cancelled),
                    ScoutRunResult::BudgetExceeded(stop) => {
                        if stop.is_abortive() {
                            return Ok(LoopOutcome::BudgetExceeded(stop));
                        }
                        ToolOutput::error(stop.summary())
                    }
                }
            } else if is_delegate {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match parse_delegate(&input, spec.role, spec.profile, team_plan.as_ref()) {
                    Ok((role, name, child_objective, title, child_profile)) => {
                        let reservation = if role == AgentRole::Manager {
                            budget.reserve(role).map(|ordinal| (ordinal, None))
                        } else {
                            budget
                                .reserve_slot(role)
                                .map(|(ordinal, slot)| (ordinal, Some(slot)))
                        };
                        match reservation {
                            None => ToolOutput::error(format!(
                                "the {} cap for this session has been reached",
                                role.as_str()
                            )),
                            Some((ordinal, _slot)) => {
                                let child_name = name.clone().unwrap_or_else(|| {
                                    title.clone().unwrap_or_else(|| {
                                        format!("{} {}", role.default_name(), ordinal)
                                    })
                                });
                                let child = engine
                                    .create_agent(
                                        &session.id,
                                        role.as_str(),
                                        &child_name,
                                        title.clone(),
                                        Some(spec.id.clone()),
                                        &model_for(config, role),
                                    )
                                    .await?;
                                engine
                                    .set_agent_profile(&child.id, child_profile.as_str())
                                    .await?;
                                let write_scope = parse_write_scope(&input)?;
                                let workspace_binding = if role == AgentRole::Worker {
                                    engine
                                        .attach_worker_workspace_for_run(
                                            &session.id,
                                            title.as_deref(),
                                            spec.department.as_deref(),
                                        )
                                        .await?
                                } else {
                                    None
                                };
                                // Bind the child run to its own project identity.
                                // A Worker uses the workspace it just attached;
                                // a Manager/Worker without a workspace resolves by
                                // role/title/department. Labour inherits the
                                // Worker cell that owns its work.
                                let (child_identity_id, child_identity_generation) = match role {
                                    AgentRole::Worker => match workspace_binding.as_ref() {
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
                                                title.as_deref(),
                                                spec.department.as_deref(),
                                            )
                                            .await?
                                            .map(|identity| (identity.id, identity.generation))
                                            .map(|(id, generation)| (Some(id), Some(generation)))
                                            .unwrap_or((None, None)),
                                    },
                                    AgentRole::Manager => engine
                                        .prompt_identity_for_run(
                                            &session.id,
                                            AgentRole::Manager.as_str(),
                                            title.as_deref(),
                                            spec.department.as_deref(),
                                        )
                                        .await?
                                        .map(|identity| (identity.id, identity.generation))
                                        .map(|(id, generation)| (Some(id), Some(generation)))
                                        .unwrap_or((None, None)),
                                    AgentRole::Labour => {
                                        (spec.identity_id.clone(), spec.identity_generation)
                                    }
                                    _ => (None, None),
                                };
                                let labour_child_worktree = role == AgentRole::Labour
                                    && spec.session_workspace_id.is_some();
                                let (
                                    child_worktree,
                                    child_branch,
                                    child_base,
                                    child_scope,
                                    child_workspace_id,
                                    child_session_workspace_id,
                                    child_parent_workspace_id,
                                ) = match role {
                                    AgentRole::Worker => match workspace_binding {
                                        Some(binding) => {
                                            let head_path =
                                                PathBuf::from(&binding.session.worktree_path);
                                            let head = tokio::task::spawn_blocking(move || {
                                                worktree::head_commit(&head_path)
                                            })
                                            .await??;
                                            (
                                                PathBuf::from(&binding.session.worktree_path),
                                                binding.session.branch.clone(),
                                                head,
                                                write_scope,
                                                Some(binding.workspace.id.clone()),
                                                Some(binding.session.id.clone()),
                                                None,
                                            )
                                        }
                                        None => {
                                            let info = engine
                                                .create_task_worktree(
                                                    &project.root,
                                                    &project.id,
                                                    &session.id,
                                                    &child.id,
                                                    &spec.session_branch,
                                                )
                                                .await?;
                                            (
                                                info.path,
                                                info.branch,
                                                info.base_commit,
                                                write_scope,
                                                None,
                                                None,
                                                None,
                                            )
                                        }
                                    },
                                    AgentRole::Manager => (
                                        spec.session_worktree.clone(),
                                        spec.session_branch.clone(),
                                        spec.base_commit.clone(),
                                        Vec::new(),
                                        None,
                                        None,
                                        None,
                                    ),
                                    AgentRole::Labour if labour_child_worktree => {
                                        let info = engine
                                            .create_task_worktree(
                                                &project.root,
                                                &project.id,
                                                &session.id,
                                                &child.id,
                                                &spec.branch,
                                            )
                                            .await?;
                                        (
                                            info.path,
                                            info.branch,
                                            info.base_commit,
                                            spec.write_scope.clone(),
                                            None,
                                            None,
                                            spec.session_workspace_id.clone(),
                                        )
                                    }
                                    AgentRole::Labour => (
                                        spec.worktree.clone(),
                                        spec.branch.clone(),
                                        spec.base_commit.clone(),
                                        spec.write_scope.clone(),
                                        None,
                                        None,
                                        None,
                                    ),
                                    AgentRole::Leader | AgentRole::Scout => (
                                        spec.session_worktree.clone(),
                                        spec.session_branch.clone(),
                                        spec.base_commit.clone(),
                                        Vec::new(),
                                        None,
                                        None,
                                        None,
                                    ),
                                    AgentRole::Verifier => (
                                        spec.worktree.clone(),
                                        spec.branch.clone(),
                                        spec.base_commit.clone(),
                                        Vec::new(),
                                        None,
                                        None,
                                        None,
                                    ),
                                };
                                engine
                                    .set_agent_worktree(
                                        &child.id,
                                        &child_worktree.to_string_lossy(),
                                        &child_branch,
                                    )
                                    .await?;
                                engine
                                    .set_agent_workspace_binding(
                                        &child.id,
                                        child_workspace_id.as_deref(),
                                        child_parent_workspace_id.as_deref(),
                                    )
                                    .await?;
                                engine
                                    .set_agent_task(
                                        &child.id,
                                        (role == AgentRole::Labour)
                                            .then(|| spec.task_id.clone())
                                            .flatten(),
                                        spec.department.clone(),
                                    )
                                    .await?;
                                let mut child_spec = AgentSpec {
                                    id: child.id.clone(),
                                    role,
                                    name: child_name.clone(),
                                    depth: spec.depth.saturating_add(1),
                                    parent_name: Some(spec.name.clone()),
                                    title: title.clone(),
                                    task_id: if role == AgentRole::Labour {
                                        spec.task_id.clone()
                                    } else {
                                        None
                                    },
                                    department: spec.department.clone(),
                                    identity_id: child_identity_id,
                                    identity_generation: child_identity_generation,
                                    profile: child_profile,
                                    task_kind: spec.task_kind.clone(),
                                    allow_delegation: role.child_role().is_some(),
                                    auto_approve: false,
                                    worktree: child_worktree,
                                    branch: child_branch,
                                    session_worktree: spec.session_worktree.clone(),
                                    session_branch: spec.session_branch.clone(),
                                    write_scope: child_scope,
                                    base_commit: child_base,
                                    workspace_id: child_workspace_id,
                                    session_workspace_id: child_session_workspace_id,
                                    parent_workspace_id: child_parent_workspace_id,
                                };
                                emit(
                                    engine,
                                    &session.id,
                                    &child_spec,
                                    "agent.spawned",
                                    serde_json::json!({
                                        "parent_id": spec.id,
                                        "parent_name": spec.name,
                                        "objective": truncate(child_objective.as_str()),
                                    }),
                                )
                                .await?;
                                let mut child_conversation =
                                    vec![ChatMessage::user(child_objective.clone())];
                                let workspace_lock = child_spec
                                    .workspace_id
                                    .as_ref()
                                    .map(|workspace_id| engine.workspace_run_lock(workspace_id));
                                let child_result = match workspace_lock {
                                    Some(lock) => {
                                        let _guard = lock.lock().await;
                                        refresh_workspace_base(&mut child_spec).await;
                                        run_loop(
                                            engine,
                                            config,
                                            session,
                                            project,
                                            control,
                                            budget,
                                            &child_spec,
                                            &child_objective,
                                            &mut child_conversation,
                                            None,
                                            false,
                                            None,
                                        )
                                        .await
                                    }
                                    None => {
                                        run_loop(
                                            engine,
                                            config,
                                            session,
                                            project,
                                            control,
                                            budget,
                                            &child_spec,
                                            &child_objective,
                                            &mut child_conversation,
                                            None,
                                            false,
                                            None,
                                        )
                                        .await
                                    }
                                };
                                match child_result {
                                    Ok(LoopOutcome::Verdict(_)) => ToolOutput::error(
                                        "a child agent returned a verdict unexpectedly",
                                    ),
                                    Ok(LoopOutcome::Final(summary)) => {
                                        let summary = if summary.trim().is_empty() {
                                            "The agent completed without a summary.".to_string()
                                        } else {
                                            summary
                                        };
                                        if role == AgentRole::Worker {
                                            match finish_worker(
                                                engine,
                                                config,
                                                session,
                                                project,
                                                control,
                                                budget,
                                                spec,
                                                &child_spec,
                                                &child_objective,
                                                &summary,
                                                None,
                                            )
                                            .await?
                                            {
                                                WorkerFinish::Done { report, success } => {
                                                    let text = format!(
                                                        "Worker {} finished:\n{}",
                                                        child_name, report
                                                    );
                                                    if success {
                                                        ToolOutput::ok(text)
                                                    } else {
                                                        ToolOutput::error(text)
                                                    }
                                                }
                                                WorkerFinish::Cancelled => {
                                                    return Ok(LoopOutcome::Cancelled);
                                                }
                                                WorkerFinish::BudgetExceeded(stop) => {
                                                    if stop.is_abortive() {
                                                        return Ok(LoopOutcome::BudgetExceeded(
                                                            stop,
                                                        ));
                                                    }
                                                    ToolOutput::error(stop.summary())
                                                }
                                            }
                                        } else if role == AgentRole::Labour
                                            && child_spec.parent_workspace_id.is_some()
                                        {
                                            match finish_labour(
                                                engine,
                                                session,
                                                spec,
                                                &child_spec,
                                                &summary,
                                            )
                                            .await?
                                            {
                                                WorkerFinish::Done { report, success } => {
                                                    let text = format!(
                                                        "Labour {} finished:\n{}",
                                                        child_name, report
                                                    );
                                                    if success {
                                                        ToolOutput::ok(text)
                                                    } else {
                                                        ToolOutput::error(text)
                                                    }
                                                }
                                                WorkerFinish::Cancelled => {
                                                    return Ok(LoopOutcome::Cancelled);
                                                }
                                                WorkerFinish::BudgetExceeded(stop) => {
                                                    if stop.is_abortive() {
                                                        return Ok(LoopOutcome::BudgetExceeded(
                                                            stop,
                                                        ));
                                                    }
                                                    ToolOutput::error(stop.summary())
                                                }
                                            }
                                        } else {
                                            engine.update_agent_status(&child.id, "idle").await?;
                                            emit(
                                                engine,
                                                &session.id,
                                                &child_spec,
                                                "agent.finished",
                                                serde_json::json!({
                                                    "status": "idle",
                                                    "summary": truncate(&summary),
                                                    "worktree": child_spec.worktree,
                                                    "branch": child_spec.branch,
                                                }),
                                            )
                                            .await?;
                                            ToolOutput::ok(format!(
                                                "{} {} finished:\n{}",
                                                role.default_name(),
                                                child_name,
                                                truncate(&summary)
                                            ))
                                        }
                                    }
                                    Ok(LoopOutcome::Deferred(wait)) => {
                                        engine.update_agent_status(&child.id, "idle").await?;
                                        emit(
                                            engine,
                                            &session.id,
                                            &child_spec,
                                            "agent.finished",
                                            serde_json::json!({
                                                "status": "waiting_gate",
                                                "gate_id": wait.gate_id,
                                                "continuation_id": wait.continuation_id,
                                            }),
                                        )
                                        .await?;
                                        match role {
                                            AgentRole::Manager => {
                                                // A Manager wait suspends the
                                                // top-level run so no Manager or
                                                // Worker slot stays occupied.
                                                return Ok(LoopOutcome::Deferred(wait));
                                            }
                                            AgentRole::Worker => ToolOutput::ok_structured(
                                                format!(
                                                    "Worker {} is waiting for external gate {}. Its run ended without a commit; the continuation will resume the session when the gate settles.",
                                                    child_name, wait.gate_id
                                                ),
                                                serde_json::json!({
                                                    "waiting": true,
                                                    "gate_id": wait.gate_id,
                                                    "continuation_id": wait.continuation_id,
                                                    "worker_id": child.id,
                                                }),
                                            ),
                                            _ => ToolOutput::error(format!(
                                                "{} cannot wait on an external gate",
                                                role.default_name()
                                            )),
                                        }
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
                                        return Ok(LoopOutcome::Cancelled);
                                    }
                                    Ok(LoopOutcome::BudgetExceeded(stop)) => {
                                        engine.update_agent_status(&child.id, "failed").await?;
                                        emit(
                                            engine,
                                            &session.id,
                                            &child_spec,
                                            "agent.finished",
                                            serde_json::json!({
                                                "status": "failed",
                                                "summary": stop.summary(),
                                            }),
                                        )
                                        .await?;
                                        return Ok(LoopOutcome::BudgetExceeded(stop));
                                    }
                                    Err(error) => {
                                        engine.update_agent_status(&child.id, "failed").await?;
                                        emit(
                                            engine,
                                            &session.id,
                                            &child_spec,
                                            "agent.finished",
                                            serde_json::json!({
                                                "status": "failed",
                                                "summary": truncate(error.to_string().as_str()),
                                            }),
                                        )
                                        .await?;
                                        ToolOutput::error(format!(
                                            "{} {} failed: {error}",
                                            role.default_name(),
                                            child_name
                                        ))
                                    }
                                }
                            }
                        }
                    }
                    Err(error) => ToolOutput::error(error.to_string()),
                }
            } else if is_acp_delegate {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match delegate_acp(
                    engine, config, session, project, control, budget, spec, &input,
                )
                .await?
                {
                    AcpDelegation::Output(output) => output,
                    AcpDelegation::Cancelled => return Ok(LoopOutcome::Cancelled),
                    AcpDelegation::BudgetExceeded(stop) => {
                        return Ok(LoopOutcome::BudgetExceeded(stop));
                    }
                }
            } else if is_todo {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match parse_todos(&input) {
                    Ok(items) => match engine.set_todos(&session.id, items).await {
                        Ok(()) => ToolOutput::ok("Updated the session plan.".to_string()),
                        Err(error) => ToolOutput::error(format!("todo_write failed: {error}")),
                    },
                    Err(error) => ToolOutput::error(format!("todo_write failed: {error}")),
                }
            } else if let Some(tool) = registry_tool {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                let mut cancel_rx = control.subscribe();
                if control.is_cancelled() {
                    return Ok(LoopOutcome::Cancelled);
                }
                let observes_token = crate::preemption::tool_observes_token(&call.name);
                let result = if observes_token {
                    let name = call.name.clone();
                    let context = tool_context.clone();
                    tool_token
                        .scoped(Box::pin(async move { tool.execute(&context, input).await }))
                        .await
                        .map_err(|error| (name, error))
                } else {
                    let context = tool_context.clone();
                    tokio::select! {
                        _ = cancel_rx.changed() => {
                            return Ok(LoopOutcome::Cancelled);
                        }
                        _ = tool_token.cancelled() => {
                            let snapshot = tool_token.snapshot().unwrap_or_else(|| {
                                crate::preemption::fallback_snapshot(
                                    crate::preemption::Phase::Tool,
                                    Some(&call.name),
                                )
                            });
                            imposed_preempt = Some(snapshot.clone());
                            Ok(crate::preemption::tool_output(&call.name, &snapshot))
                        }
                        result = tool_token.scoped(Box::pin(async move {
                            tool.execute(&context, input).await
                        })) => result.map_err(|error| (call.name.clone(), error)),
                    }
                };
                match result {
                    Ok(output) => output,
                    Err((_name, error)) => ToolOutput::error(format!("tool error: {error}")),
                }
            } else {
                ToolOutput::error_coded(
                    call.name.as_str(),
                    crate::tooling::errors::ToolErrorCode::TOOL_NOT_AVAILABLE,
                    format!(
                        "tool '{}' is not available to {}",
                        call.name,
                        spec.role.as_str()
                    ),
                )
            };
            let mut output = output;
            output.normalize_failure(&call.name);
            let output_preempt = imposed_preempt
                .take()
                .or_else(|| crate::preemption::output_snapshot(&output));
            let terminal_preempt = output_preempt
                .as_ref()
                .filter(|snapshot| {
                    snapshot.reason != crate::preemption::Reason::ToolDeadline
                        || !tool_token.is_own_preempted()
                })
                .cloned();
            if let Some(snapshot) = &output_preempt {
                if snapshot.side_effects_unknown {
                    control.preempt.mark_side_effects_unknown();
                }
                Box::pin(emit_tool_preempted(
                    engine, session, project, spec, snapshot,
                ))
                .await?;
            }
            let tool_duration_ms =
                tool_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;

            let pending = vec![crate::tool_output::PendingToolOutput {
                tool_call_id: call_id.clone(),
                tool_name: call.name.clone(),
                output,
            }];
            let (mut shaped, next_turn_bytes) =
                shape_tool_output_batch(engine, session, spec, project, turn_tool_bytes, pending)
                    .await?;
            turn_tool_bytes = next_turn_bytes;
            let mut output = shaped
                .pop()
                .ok_or_else(|| anyhow!("tool-output shaping returned no result"))?
                .output;
            output.annotate_shape_metadata();

            finish_tool_call(
                engine,
                session,
                spec,
                journal,
                &call_id,
                &call.name,
                &effect_input,
                &mut output,
                tool_duration_ms,
                Some(approval_risk),
                config.approval_mode,
                approval_wait_ms,
                planned_effect.as_ref(),
                effect_begin.take(),
                journal_pair.as_ref(),
                None,
            )
            .await?;
            if top_level {
                let stored = engine
                    .append_message(
                        &session.id,
                        "tool",
                        &output.content,
                        serde_json::json!([]),
                        Some(call_id.clone()),
                    )
                    .await?;
                conversation_ids.push(Some(stored.id));
            }
            conversation.push(ChatMessage::tool(call_id, output.content));
            if let Some(wait) = deferred_wait.take() {
                // A gate wait is a clean handoff: do not execute any remaining
                // sibling tool calls or make another provider request. Every
                // skipped sibling still records a tool result so the resumed
                // conversation has one result per assistant tool call.
                for (remaining_id, remaining_name) in serial_call_ids.iter().skip(index + 1) {
                    let skipped = ToolOutput::error_coded(
                        remaining_name,
                        crate::tooling::errors::ToolErrorCode::TOOL_FAILED,
                        "not executed: the run deferred to wait for an external gate; re-issue this call after the gate resolves",
                    );
                    emit(
                        engine,
                        &session.id,
                        spec,
                        "tool.finished",
                        tool_finished_payload(
                            remaining_id,
                            remaining_name,
                            &skipped,
                            0,
                            None,
                            config.approval_mode,
                            None,
                        ),
                    )
                    .await?;
                    if top_level {
                        let stored = engine
                            .append_message(
                                &session.id,
                                "tool",
                                &skipped.content,
                                serde_json::json!([]),
                                Some(remaining_id.clone()),
                            )
                            .await?;
                        conversation_ids.push(Some(stored.id));
                    }
                    conversation.push(ChatMessage::tool(remaining_id.clone(), skipped.content));
                }
                return Ok(LoopOutcome::Deferred(wait));
            }
            if terminal_preempt.is_some() {
                return Ok(LoopOutcome::Cancelled);
            }
        }
        emit(
            engine,
            &session.id,
            spec,
            "agent.turn_finished",
            serde_json::json!({"turn": turn + 1}),
        )
        .await?;
    }

    Err(anyhow!("maximum turns reached"))
}

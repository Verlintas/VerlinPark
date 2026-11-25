use super::*;

/// Statuses that already represent a real, settled outcome. A batch abort
/// must not rewrite them: a completed/merged sibling stays completed, and a
/// cancelled/failed/deferred sibling keeps the state its own result produced.
fn preserved_terminal_status(status: &str) -> bool {
    matches!(status, "completed" | "skipped" | "cancelled" | "failed")
}

fn task_dependency_satisfied(status: Option<&str>) -> bool {
    crate::orchestration::dependency_satisfied(status)
}

async fn latest_merged_entry_id(
    engine: &Engine,
    session_id: &str,
    task_id: &str,
) -> Result<Option<String>> {
    let store = engine.store().clone();
    let session_id = session_id.to_string();
    let task_id = task_id.to_string();
    Ok(tokio::task::spawn_blocking(move || {
        store.latest_merged_entry_for_task(&session_id, &task_id)
    })
    .await??
    .map(|entry| entry.id))
}

/// Apply the guarded projection transition first, then emit the matching
/// `plan_task.finished` event only when the store actually applied it. A
/// completed/merged row is authoritative: a late abort/cancel must not emit a
/// contradictory event that replay would project over the real result.
#[allow(clippy::too_many_arguments)]
async fn finish_task_and_emit(
    engine: &Engine,
    session_id: &str,
    plan_seq: u64,
    spec: &AgentSpec,
    task_id: &str,
    status: &str,
    detail: Option<&str>,
    payload: serde_json::Value,
    statuses: &mut std::collections::HashMap<String, String>,
) -> Result<Option<vp_protocol::types::ScheduledTask>> {
    let Some(task) =
        record_scheduled_task_finish(engine, session_id, plan_seq, task_id, status, detail).await?
    else {
        if let Some(current) = engine.plan_task_status(session_id, task_id).await? {
            statuses.insert(task_id.to_string(), current);
        }
        return Ok(None);
    };
    statuses.insert(task_id.to_string(), task.status.clone());
    emit(engine, session_id, spec, "plan_task.finished", payload).await?;
    Ok(Some(task))
}

/// Settle a task that produced no actual result in this batch. The current
/// projection is read first so an abort can never overwrite work another
/// path already completed or cancelled.
#[allow(clippy::too_many_arguments)]
async fn settle_aborted_task(
    engine: &Engine,
    session_id: &str,
    plan_seq: u64,
    spec: &AgentSpec,
    task_id: &str,
    status: &str,
    reason: &str,
    statuses: &mut std::collections::HashMap<String, String>,
    reports: &mut Vec<String>,
) -> Result<()> {
    if let Some(current) = engine.plan_task_status(session_id, task_id).await?
        && preserved_terminal_status(&current)
    {
        statuses.insert(task_id.to_string(), current.clone());
        reports.push(format!(
            "{task_id} [{current}]: already settled; abort status {status} not applied"
        ));
        return Ok(());
    }
    let payload = serde_json::json!({
        "task_id": task_id,
        "task_status": status,
        "reason": reason,
    });
    if finish_task_and_emit(
        engine,
        session_id,
        plan_seq,
        spec,
        task_id,
        status,
        Some(reason),
        payload,
        statuses,
    )
    .await?
    .is_some()
    {
        reports.push(format!("{task_id} [{status}]: {reason}"));
    } else {
        let current = statuses
            .get(task_id)
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        reports.push(format!(
            "{task_id} [{current}]: guarded finish refused {status}; no event emitted"
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(in crate::agent) async fn execute_team_plan(
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
                settle_aborted_task(
                    engine,
                    &session.id,
                    plan_seq,
                    leader_spec,
                    &task.id,
                    "cancelled",
                    "session cancelled",
                    &mut statuses,
                    &mut reports,
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
                settle_aborted_task(
                    engine,
                    &session.id,
                    plan_seq,
                    leader_spec,
                    &task.id,
                    "blocked",
                    "token budget exhausted",
                    &mut statuses,
                    &mut reports,
                )
                .await?;
            }
            return Ok(PlanRunResult::BudgetExceeded(BudgetStop::session(budget)));
        }
        // R2a resource gate: do not start new work when attention/risk or the
        // completion reserve is already exhausted. This is a dispatch guard,
        // not a merge/verification gate; existing work continues.
        match engine
            .resource_governor_decision(&session.id, "dispatch")
            .await
        {
            Ok(decision) if decision.should_block() => {
                engine.record_resource_decision(&decision).await?;
                reports.push(format!(
                    "resource governor deferred dispatch: {} ({})",
                    decision.reason,
                    decision.bottleneck.as_deref().unwrap_or("no bottleneck")
                ));
                deferred_by_governor = true;
                break;
            }
            Ok(_) => {}
            Err(error) => {
                // The guard is advisory infrastructure; a read failure must not
                // silently block an otherwise valid run. The error is logged
                // and the existing budget/approval gates remain authoritative.
                tracing::warn!(%error, session_id = %session.id, "resource governor dispatch check failed");
            }
        }
        let governor_budget = GovernorBudgetView::new(
            budget.token_limit(),
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
                let unmet = task
                    .deps
                    .iter()
                    .filter(|dep| {
                        !task_dependency_satisfied(statuses.get(*dep).map(String::as_str))
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
                settle_aborted_task(
                    engine,
                    &session.id,
                    plan_seq,
                    leader_spec,
                    &task.id,
                    "blocked",
                    &reason,
                    &mut statuses,
                    &mut reports,
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
            // Capacity is transient; leave the remaining tasks pending. A
            // terminal block here would strand them because the scheduler only
            // dispatches pending/interrupted rows.
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
        let claimed_ids = claimed
            .iter()
            .map(|task| task.task_id.clone())
            .collect::<Vec<_>>();
        let setup_snapshot = match batch_setup_snapshot(engine, session).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                // The snapshot is taken before any agent/worktree exists, so
                // only the claims need settling.
                let detail = format!("plan batch setup failed: {error:#}");
                for task_id in &claimed_ids {
                    if let Err(finish_error) = record_scheduled_task_finish(
                        engine,
                        &session.id,
                        plan_seq,
                        task_id,
                        "failed",
                        Some(&detail),
                    )
                    .await
                    {
                        tracing::warn!(%finish_error, task_id, "batch setup rollback could not settle claimed task");
                    }
                }
                return Err(error);
            }
        };
        let setup_result: Result<()> = Box::pin(async {
            let plan_results = latest_plan_results(engine, &session.id).await?;
            for task in &batch {
                let manager_spec = if let Some(spec) = manager_specs.get(&task.department) {
                    spec.clone()
                } else {
                    let department = plan
                        .departments
                        .iter()
                        .find(|department| department.name == task.department)
                        .ok_or_else(|| {
                            anyhow!("missing department for task {}", task.department)
                        })?;
                    let spec = ensure_plan_manager(
                        engine,
                        config,
                        session,
                        budget,
                        leader_spec,
                        department,
                    )
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
                    .attach_worker_workspace_for_run(
                        &session.id,
                        None,
                        Some(task.department.as_str()),
                    )
                    .await?;
                let (worker_identity_id, worker_identity_generation) =
                    match workspace_binding.as_ref() {
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
                let memory_brief = project_memory_brief(
                    engine,
                    &session.id,
                    Some(&task.id),
                    Some(&task.department),
                )
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
            Ok(())
        })
        .await;
        if let Err(error) = setup_result {
            let detail = format!("plan batch setup failed: {error:#}");
            rollback_batch_setup(
                engine,
                &session.id,
                Some(plan_seq),
                &claimed_ids,
                &setup_snapshot,
                &detail,
            )
            .await;
            return Err(error);
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
            match result {
                Ok(LoopOutcome::Final(summary)) => {
                    let summary = if summary.trim().is_empty() {
                        "The worker completed without a summary.".to_string()
                    } else {
                        summary
                    };
                    let session_cancelled = control.is_cancelled();
                    let task_cancelled = engine
                        .plan_task_cancel_requested(&session.id, &task.id)
                        .await?;
                    if session_cancelled || task_cancelled {
                        let reason = if session_cancelled {
                            "session cancelled"
                        } else {
                            "cancel requested by user"
                        };
                        let payload = serde_json::json!({
                            "task_id": task.id,
                            "task_status": "cancelled",
                            "reason": reason,
                        });
                        let applied = finish_task_and_emit(
                            engine,
                            &session.id,
                            plan_seq,
                            worker_spec,
                            &task.id,
                            "cancelled",
                            Some(reason),
                            payload,
                            &mut statuses,
                        )
                        .await?;
                        if applied.is_some() {
                            engine
                                .update_agent_status(&worker_spec.id, "cancelled")
                                .await?;
                            reports.push(format!("{} [cancelled]: {reason}", task.id));
                        } else {
                            reports.push(format!(
                                "{} [{}]: guarded cancel refused; task event not emitted",
                                task.id,
                                statuses
                                    .get(&task.id)
                                    .map(String::as_str)
                                    .unwrap_or("unknown")
                            ));
                        }
                        if session_cancelled {
                            abort = Some(PlanAbort::Cancelled);
                        }
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
                            let cancelled_after_finish = control.is_cancelled()
                                || engine
                                    .plan_task_cancel_requested(&session.id, &task.id)
                                    .await?;
                            let merged_before_cancel = if cancelled_after_finish {
                                latest_merged_entry_id(engine, &session.id, &task.id).await?
                            } else {
                                None
                            };
                            let (status, detail, reason) = if cancelled_after_finish {
                                if merged_before_cancel.is_some() {
                                    // The commit already landed; report the
                                    // actual merged result instead of
                                    // rewriting history to cancelled.
                                    (
                                        "completed",
                                        report.clone(),
                                        Some("commit merged before the cancel was observed"),
                                    )
                                } else {
                                    (
                                        "cancelled",
                                        "cancel requested before the commit merged; integration did not complete"
                                            .to_string(),
                                        Some("cancel requested by user"),
                                    )
                                }
                            } else if success {
                                ("completed", report.clone(), None)
                            } else if integration_blocked_by_effect_unknown(&report) {
                                (
                                    "blocked",
                                    report.clone(),
                                    Some(
                                        "operator must reconcile the unresolved effect receipt(s) before retrying integration",
                                    ),
                                )
                            } else {
                                ("failed", report.clone(), None)
                            };
                            let mut payload = serde_json::json!({
                                "task_id": task.id,
                                "task_status": status,
                                "summary": truncate(&detail),
                            });
                            if integration_blocked_by_effect_unknown(&report) {
                                payload["effect_blocked"] = serde_json::json!(true);
                            }
                            if cancelled_after_finish {
                                payload["cancel_observed"] = serde_json::json!(true);
                            }
                            if merged_before_cancel.is_some() {
                                payload["merged"] = serde_json::json!(true);
                            }
                            if let Some(reason) = reason {
                                payload["reason"] = serde_json::json!(reason);
                            }
                            let applied = finish_task_and_emit(
                                engine,
                                &session.id,
                                plan_seq,
                                worker_spec,
                                &task.id,
                                status,
                                Some(detail.as_str()),
                                payload,
                                &mut statuses,
                            )
                            .await?;
                            if applied.is_none() {
                                if control.is_cancelled() {
                                    abort = Some(PlanAbort::Cancelled);
                                }
                                reports.push(format!(
                                    "{} [{}]: guarded finish refused {status}; no event emitted",
                                    task.id,
                                    statuses
                                        .get(&task.id)
                                        .map(String::as_str)
                                        .unwrap_or("unknown")
                                ));
                                continue;
                            }
                            if status == "cancelled" && control.is_cancelled() {
                                abort = Some(PlanAbort::Cancelled);
                            }
                            reports.push(format!(
                                "{} [{}]: {}",
                                task.id,
                                status,
                                truncate(&detail)
                            ));
                        }
                        WorkerFinish::Cancelled => {
                            let session_cancelled = control.is_cancelled();
                            let task_cancelled = engine
                                .plan_task_cancel_requested(&session.id, &task.id)
                                .await?;
                            let reason = if session_cancelled {
                                "session cancelled"
                            } else if task_cancelled {
                                "cancel requested by user"
                            } else {
                                "verification cancelled"
                            };
                            let payload = serde_json::json!({
                                "task_id": task.id,
                                "task_status": "cancelled",
                                "reason": reason,
                            });
                            let applied = finish_task_and_emit(
                                engine,
                                &session.id,
                                plan_seq,
                                worker_spec,
                                &task.id,
                                "cancelled",
                                Some(reason),
                                payload,
                                &mut statuses,
                            )
                            .await?;
                            if applied.is_some() {
                                reports.push(format!("{} [cancelled]: {reason}", task.id));
                            } else {
                                reports.push(format!(
                                    "{} [{}]: guarded cancel refused; task event not emitted",
                                    task.id,
                                    statuses
                                        .get(&task.id)
                                        .map(String::as_str)
                                        .unwrap_or("unknown")
                                ));
                            }
                            if session_cancelled {
                                abort = Some(PlanAbort::Cancelled);
                            }
                        }
                        WorkerFinish::BudgetExceeded(stop) => {
                            let payload = serde_json::json!({
                                "task_id": task.id,
                                "task_status": "blocked",
                                "reason": stop.summary(),
                            });
                            let applied = finish_task_and_emit(
                                engine,
                                &session.id,
                                plan_seq,
                                worker_spec,
                                &task.id,
                                "blocked",
                                Some(&stop.summary()),
                                payload,
                                &mut statuses,
                            )
                            .await?;
                            if applied.is_some() {
                                reports.push(format!("{} [blocked]: {}", task.id, stop.summary()));
                                abort = Some(PlanAbort::Budget(stop));
                            } else {
                                reports.push(format!(
                                    "{} [{}]: guarded budget stop refused; task event not emitted",
                                    task.id,
                                    statuses
                                        .get(&task.id)
                                        .map(String::as_str)
                                        .unwrap_or("unknown")
                                ));
                            }
                        }
                    }
                }
                Ok(LoopOutcome::Deferred(wait)) => {
                    engine.update_agent_status(&worker_spec.id, "idle").await?;
                    let payload = serde_json::json!({
                        "task_id": task.id,
                        "task_status": "blocked",
                        "gate_id": wait.gate_id,
                        "continuation_id": wait.continuation_id,
                        "reason": "waiting for external gate",
                    });
                    let applied = finish_task_and_emit(
                        engine,
                        &session.id,
                        plan_seq,
                        worker_spec,
                        &task.id,
                        "blocked",
                        Some("waiting for external gate"),
                        payload,
                        &mut statuses,
                    )
                    .await?;
                    if applied.is_some() {
                        reports.push(format!(
                            "{} [blocked]: waiting for external gate {}",
                            task.id, wait.gate_id
                        ));
                        abort = Some(PlanAbort::Deferred(wait));
                    } else {
                        reports.push(format!(
                            "{} [{}]: guarded gate wait refused; task event not emitted",
                            task.id,
                            statuses
                                .get(&task.id)
                                .map(String::as_str)
                                .unwrap_or("unknown")
                        ));
                    }
                }
                Ok(LoopOutcome::Cancelled) => {
                    let task_cancel = !control.is_cancelled()
                        && engine
                            .plan_task_cancel_requested(&session.id, &task.id)
                            .await?;
                    let reason = if task_cancel {
                        "cancel requested by user"
                    } else {
                        "session cancelled"
                    };
                    let payload = serde_json::json!({
                        "task_id": task.id,
                        "task_status": "cancelled",
                        "reason": reason,
                    });
                    let applied = finish_task_and_emit(
                        engine,
                        &session.id,
                        plan_seq,
                        worker_spec,
                        &task.id,
                        "cancelled",
                        task_cancel.then_some(reason),
                        payload,
                        &mut statuses,
                    )
                    .await?;
                    if applied.is_some() {
                        engine
                            .update_agent_status(&worker_spec.id, "cancelled")
                            .await?;
                        reports.push(format!("{} [cancelled]: {reason}", task.id));
                    } else {
                        reports.push(format!(
                            "{} [{}]: guarded cancel refused; task event not emitted",
                            task.id,
                            statuses
                                .get(&task.id)
                                .map(String::as_str)
                                .unwrap_or("unknown")
                        ));
                    }
                    if !task_cancel {
                        abort = Some(PlanAbort::Cancelled);
                    }
                }
                Ok(LoopOutcome::BudgetExceeded(stop)) => {
                    let payload = serde_json::json!({
                        "task_id": task.id,
                        "task_status": "blocked",
                        "reason": stop.summary(),
                    });
                    let applied = finish_task_and_emit(
                        engine,
                        &session.id,
                        plan_seq,
                        worker_spec,
                        &task.id,
                        "blocked",
                        Some(&stop.summary()),
                        payload,
                        &mut statuses,
                    )
                    .await?;
                    if applied.is_some() {
                        engine
                            .update_agent_status(&worker_spec.id, "failed")
                            .await?;
                        reports.push(format!("{} [blocked]: {}", task.id, stop.summary()));
                        if stop.is_abortive() {
                            abort = Some(PlanAbort::Budget(stop));
                        }
                    } else {
                        reports.push(format!(
                            "{} [{}]: guarded budget stop refused; task event not emitted",
                            task.id,
                            statuses
                                .get(&task.id)
                                .map(String::as_str)
                                .unwrap_or("unknown")
                        ));
                    }
                }
                Ok(LoopOutcome::Verdict(verdict)) => {
                    // Role policy reserves `submit_verdict` for Verifiers. If a
                    // Worker ever reaches this outcome, settle the task and its
                    // agent as failed rather than leaving both projections
                    // `running` with no terminal event.
                    let summary = format!(
                        "Worker returned an unsupported Verifier verdict: {}",
                        truncate(&verdict.summary)
                    );
                    let payload = serde_json::json!({
                        "task_id": task.id,
                        "task_status": "failed",
                        "summary": truncate(&summary),
                        "unsupported_outcome": "verdict",
                    });
                    let applied = finish_task_and_emit(
                        engine,
                        &session.id,
                        plan_seq,
                        worker_spec,
                        &task.id,
                        "failed",
                        Some(&summary),
                        payload,
                        &mut statuses,
                    )
                    .await?;
                    if applied.is_some() {
                        engine
                            .update_agent_status(&worker_spec.id, "failed")
                            .await?;
                        reports.push(format!("{} [failed]: {summary}", task.id));
                    } else {
                        reports.push(format!(
                            "{} [{}]: guarded verdict finish refused; task event not emitted",
                            task.id,
                            statuses
                                .get(&task.id)
                                .map(String::as_str)
                                .unwrap_or("unknown")
                        ));
                    }
                }
                Err(error) => {
                    let payload = serde_json::json!({
                        "task_id": task.id,
                        "task_status": "failed",
                        "summary": truncate(error.to_string().as_str()),
                    });
                    let applied = finish_task_and_emit(
                        engine,
                        &session.id,
                        plan_seq,
                        worker_spec,
                        &task.id,
                        "failed",
                        Some(&error.to_string()),
                        payload,
                        &mut statuses,
                    )
                    .await?;
                    if applied.is_some() {
                        engine
                            .update_agent_status(&worker_spec.id, "failed")
                            .await?;
                        reports.push(format!("{} [failed]: {error}", task.id));
                    } else {
                        reports.push(format!(
                            "{} [{}]: guarded failure refused; task event not emitted",
                            task.id,
                            statuses
                                .get(&task.id)
                                .map(String::as_str)
                                .unwrap_or("unknown")
                        ));
                    }
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
            // Every batch future has a real result and was processed above.
            // Only tasks with no result at all are synthesized here: the
            // non-batch remainder plus any batch task whose result did not
            // produce a terminal state. A completed/merged sibling is never
            // rewritten by the abort.
            for task in remaining
                .iter()
                .filter(|task| !batch_ids.contains(&task.id))
            {
                settle_aborted_task(
                    engine,
                    &session.id,
                    plan_seq,
                    leader_spec,
                    &task.id,
                    status,
                    &reason,
                    &mut statuses,
                    &mut reports,
                )
                .await?;
            }
            for task in &batch {
                if statuses.get(&task.id).map(String::as_str) != Some("running") {
                    continue;
                }
                settle_aborted_task(
                    engine,
                    &session.id,
                    plan_seq,
                    leader_spec,
                    &task.id,
                    status,
                    &reason,
                    &mut statuses,
                    &mut reports,
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

/// An unresolved effect is a security boundary, not a normal implementation
/// failure. Retrying it as a fresh task burns budget and cannot merge; the
/// operator must reconcile the receipt first.
fn integration_blocked_by_effect_unknown(report: &str) -> bool {
    report.starts_with("merge/integration failed:") && report.contains("EFFECT_UNKNOWN")
}

#[cfg(test)]
mod tests {
    use super::integration_blocked_by_effect_unknown;

    #[test]
    fn effect_unknown_integration_is_distinguished_from_a_normal_failure() {
        assert!(integration_blocked_by_effect_unknown(
            "merge/integration failed: EFFECT_UNKNOWN: 0 intent and 1 unknown effect receipt(s) block integration\nverdict: APPROVED"
        ));
        assert!(!integration_blocked_by_effect_unknown(
            "merge/integration failed: session worktree is dirty\nverdict: APPROVED"
        ));
        assert!(!integration_blocked_by_effect_unknown(
            "verification error: provider timeout"
        ));
    }
}

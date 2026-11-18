use super::*;

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
pub(in crate::agent) async fn deliberate_team_plan(
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

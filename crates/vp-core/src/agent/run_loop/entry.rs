use super::*;

#[async_recursion]
#[allow(clippy::too_many_arguments)]
pub(in crate::agent) async fn run_loop(
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

//! Session lifecycle, context, and transcript commands.

use super::*;

pub(super) async fn dispatch(
    command: Command,
    discovery: Discovery,
    _json: bool,
) -> Result<Option<Value>> {
    let value =
        match command {
            Command::Sessions { project } => {
                rpc(&discovery, "session.list", json!({"project_id": project})).await?
            }
            Command::SessionNew {
                project,
                goal,
                priority,
                token_budget,
                cost_budget_usd,
                time_budget_secs,
                unlimited,
            } => {
                rpc(
                    &discovery,
                    "session.create",
                    json!({
                        "project_id": project,
                        "goal": goal,
                        "priority": priority,
                        "token_budget": token_budget,
                        "cost_budget_usd": cost_budget_usd,
                        "time_budget_secs": time_budget_secs,
                        "unlimited": unlimited,
                    }),
                )
                .await?
            }
            Command::SessionUnlimited { session, enabled } => {
                rpc(
                    &discovery,
                    "session.budget.set",
                    json!({"session_id": session, "unlimited": enabled.unwrap_or(true)}),
                )
                .await?
            }
            Command::SessionPriority { session, priority } => {
                rpc(
                    &discovery,
                    "session.priority.set",
                    json!({"session_id": session, "priority": priority}),
                )
                .await?
            }
            Command::Search {
                query,
                project,
                limit,
            } => {
                rpc(
                    &discovery,
                    "session.search",
                    json!({"query": query, "project_id": project, "limit": limit}),
                )
                .await?
            }
            Command::Events {
                session,
                since,
                limit,
            } => {
                rpc(
                    &discovery,
                    "event.list",
                    json!({"session_id": session, "since_seq": since, "limit": limit}),
                )
                .await?
            }
            Command::Prompt {
                session,
                content,
                operation_id,
            } => rpc(
                &discovery,
                "session.prompt",
                json!({"session_id": session, "content": content, "operation_id": operation_id}),
            )
            .await?,
            Command::Cancel { session } => {
                rpc(&discovery, "session.cancel", json!({"session_id": session})).await?
            }
            Command::Pause { session } => {
                rpc(&discovery, "session.pause", json!({"session_id": session})).await?
            }
            Command::Resume { session } => {
                rpc(&discovery, "session.resume", json!({"session_id": session})).await?
            }
            Command::ResumePlan { session } => {
                rpc(
                    &discovery,
                    "session.plan_resume",
                    json!({"session_id": session}),
                )
                .await?
            }
            Command::Compatibility { session, run } => {
                let mut params = json!({"session_id": session});
                if let Some(run) = run {
                    params["run_id"] = json!(run);
                }
                rpc(&discovery, "session.compatibility", params).await?
            }
            Command::Diagnose {
                session,
                task,
                limit,
                graph,
                record,
                history,
            } => {
                if history {
                    rpc(
                        &discovery,
                        "session.attributions",
                        json!({"session_id": session, "limit": limit}),
                    )
                    .await?
                } else {
                    let method = if record {
                        "session.diagnosis.record"
                    } else {
                        "session.diagnosis"
                    };
                    rpc(
                        &discovery,
                        method,
                        json!({
                            "session_id": session,
                            "target_task_id": task,
                            "limit": limit,
                            "include_graph": graph,
                        }),
                    )
                    .await?
                }
            }
            Command::Context { session } => {
                rpc(
                    &discovery,
                    "session.context",
                    json!({"session_id": session}),
                )
                .await?
            }
            Command::Compact { session } => {
                rpc(
                    &discovery,
                    "session.compact",
                    json!({"session_id": session}),
                )
                .await?
            }
            Command::Todos { session } => {
                rpc(&discovery, "session.todos", json!({"session_id": session})).await?
            }
            Command::CitationAudit { session } => {
                rpc(
                    &discovery,
                    "session.citation_audit",
                    json!({"session_id": session}),
                )
                .await?
            }
            Command::Citations { session, message } => {
                rpc(
                    &discovery,
                    "session.citations",
                    json!({"session_id": session, "message_id": message}),
                )
                .await?
            }
            Command::Transcript { session } => {
                rpc(
                    &discovery,
                    "session.transcript",
                    json!({"session_id": session}),
                )
                .await?
            }
            Command::Usage { session, csv } => {
                let method = if csv {
                    "session.usage_csv"
                } else {
                    "session.usage"
                };
                rpc(&discovery, method, json!({"session_id": session})).await?
            }
            _ => unreachable!("command routed to the wrong dispatch family"),
        };
    Ok(Some(value))
}

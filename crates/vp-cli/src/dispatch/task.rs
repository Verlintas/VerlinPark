//! Task, run, intake, program, and steering commands.

use super::*;

pub(super) async fn dispatch(
    command: Command,
    discovery: Discovery,
    _json: bool,
) -> Result<Option<Value>> {
    let value = match command {
        Command::Scheduler { project } => {
            rpc(
                &discovery,
                "scheduler.status",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::Programs {
            project,
            status,
            limit,
        } => {
            rpc(
                &discovery,
                "program.list",
                json!({"project_id": project, "status": status, "limit": limit}),
            )
            .await?
        }
        Command::ProgramStatus { id, events } => {
            rpc(
                &discovery,
                "program.status",
                json!({"program_id": id, "event_limit": events}),
            )
            .await?
        }
        Command::ProgramCreate {
            project,
            goal,
            activate,
            auto_dispatch,
            token_budget,
            cost_budget_usd,
            max_attempts,
            retry_backoff_secs,
            max_parallel,
            dependency_failure,
        } => {
            let dependency_failure = parse_dependency_failure(&dependency_failure)?;
            rpc(
                &discovery,
                "program.create",
                json!({
                    "project_id": project,
                    "goal": goal,
                    "activate": activate,
                    "token_budget": token_budget,
                    "cost_budget_usd": cost_budget_usd,
                    "policy": {
                        "auto_dispatch": auto_dispatch || activate,
                        "max_attempts": max_attempts,
                        "retry_backoff_secs": retry_backoff_secs,
                        "dependency_failure": dependency_failure,
                        "max_parallel_milestones": max_parallel,
                    },
                }),
            )
            .await?
        }
        Command::ProgramUpdate {
            id,
            goal,
            auto_dispatch,
            token_budget,
            cost_budget_usd,
            max_attempts,
            retry_backoff_secs,
            max_parallel,
            dependency_failure,
        } => {
            let dependency_failure = parse_dependency_failure(&dependency_failure)?;
            rpc(
                &discovery,
                "program.update",
                json!({
                    "program_id": id,
                    "goal": goal,
                    "token_budget": token_budget,
                    "cost_budget_usd": cost_budget_usd,
                    "policy": {
                        "auto_dispatch": auto_dispatch,
                        "max_attempts": max_attempts,
                        "retry_backoff_secs": retry_backoff_secs,
                        "dependency_failure": dependency_failure,
                        "max_parallel_milestones": max_parallel,
                    },
                }),
            )
            .await?
        }
        Command::ProgramPause { id, reason } => {
            rpc(
                &discovery,
                "program.pause",
                json!({"program_id": id, "reason": reason}),
            )
            .await?
        }
        Command::ProgramResume { id } => {
            rpc(&discovery, "program.resume", json!({"program_id": id})).await?
        }
        Command::ProgramCancel { id, reason } => {
            rpc(
                &discovery,
                "program.cancel",
                json!({"program_id": id, "reason": reason}),
            )
            .await?
        }
        Command::ProgramDelete { id } => {
            rpc(&discovery, "program.delete", json!({"program_id": id})).await?
        }
        Command::ProgramMilestoneAdd {
            program,
            title,
            objective,
            key,
            acceptance,
            depends_on,
            priority,
            max_attempts,
            profile,
            scope,
            token_budget,
            cost_budget_usd,
        } => {
            let objective = if objective == "-" {
                let mut buffer = String::new();
                std::io::stdin()
                    .read_to_string(&mut buffer)
                    .context("reading milestone objective from stdin")?;
                buffer
            } else {
                objective
            };
            rpc(
                &discovery,
                "program.milestone.create",
                json!({
                    "program_id": program,
                    "key": key,
                    "title": title,
                    "objective": objective,
                    "acceptance": acceptance,
                    "deps": depends_on,
                    "priority": priority,
                    "max_attempts": max_attempts,
                    "profile": profile,
                    "write_scope": scope,
                    "token_budget": token_budget,
                    "cost_budget_usd": cost_budget_usd,
                }),
            )
            .await?
        }
        Command::ProgramMilestoneRetry { id } => {
            rpc(
                &discovery,
                "program.milestone.retry",
                json!({"milestone_id": id}),
            )
            .await?
        }
        Command::ProgramMilestoneSkip { id, reason } => {
            rpc(
                &discovery,
                "program.milestone.skip",
                json!({"milestone_id": id, "reason": reason}),
            )
            .await?
        }
        Command::ProgramRun { milestone } => {
            rpc(
                &discovery,
                "program.milestone.start",
                json!({"milestone_id": milestone}),
            )
            .await?
        }
        Command::ProgramTick { project } => {
            rpc(&discovery, "program.tick", json!({"project_id": project})).await?
        }
        Command::ProgramReplay { id, repair } => {
            rpc(
                &discovery,
                "program.replay",
                json!({"program_id": id, "repair": repair}),
            )
            .await?
        }
        Command::Tasks { project, session } => {
            rpc(
                &discovery,
                "task.queue",
                json!({"project_id": project, "session_id": session}),
            )
            .await?
        }
        Command::Runs { session, limit } => {
            rpc(
                &discovery,
                "session.runs",
                json!({"session_id": session, "limit": limit}),
            )
            .await?
        }
        Command::RunDetail { run } => {
            rpc(&discovery, "session.run.detail", json!({"run_id": run})).await?
        }
        Command::RunResume {
            session,
            run,
            operation_id,
        } => {
            rpc(
                &discovery,
                "session.run.resume",
                json!({
                    "session_id": session,
                    "run_id": run,
                    "operation_id": operation_id,
                }),
            )
            .await?
        }
        Command::RunCancel { session, run } => {
            rpc(
                &discovery,
                "session.run.cancel",
                json!({"session_id": session, "run_id": run}),
            )
            .await?
        }
        Command::RunPolicy { project } => {
            rpc(
                &discovery,
                "session.run.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::RunPolicySet {
            project,
            auto_resume,
            max_attempts,
            max_age_hours,
        } => {
            let current = rpc(
                &discovery,
                "session.run.policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy = current.get("policy").cloned().unwrap_or_else(|| json!({}));
            if let Some(object) = policy.as_object_mut() {
                if let Some(auto_resume) = auto_resume {
                    object.insert("auto_resume".into(), json!(auto_resume));
                }
                if let Some(max_attempts) = max_attempts {
                    object.insert("max_attempts".into(), json!(max_attempts));
                }
                if let Some(max_age_hours) = max_age_hours {
                    object.insert("max_age_hours".into(), json!(max_age_hours));
                }
            }
            rpc(
                &discovery,
                "session.run.policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::Intake {
            project,
            status,
            limit,
        } => {
            rpc(
                &discovery,
                "intake.list",
                json!({"project_id": project, "status": status, "limit": limit}),
            )
            .await?
        }
        Command::IntakeAdd {
            project,
            title,
            instructions,
            profile,
            kind,
            priority,
            scope,
            acceptance,
            depends_on,
            token_budget,
            cost_budget_usd,
            time_budget_secs,
            max_attempts,
            labels,
        } => {
            let instructions = if instructions == "-" {
                let mut buffer = String::new();
                std::io::stdin()
                    .read_to_string(&mut buffer)
                    .context("reading intake instructions from stdin")?;
                buffer
            } else {
                instructions
            };
            rpc(
                &discovery,
                "intake.create",
                json!({
                    "project_id": project,
                    "title": title,
                    "instructions": instructions,
                    "profile": profile,
                    "kind": kind,
                    "priority": priority,
                    "write_scope": scope,
                    "acceptance": acceptance,
                    "deps": depends_on,
                    "token_budget": token_budget,
                    "cost_budget_usd": cost_budget_usd,
                    "time_budget_secs": time_budget_secs,
                    "max_attempts": max_attempts,
                    "labels": labels,
                    "origin": "cli",
                }),
            )
            .await?
        }
        Command::IntakeEdit {
            id,
            title,
            instructions,
            profile,
            kind,
            priority,
            scope,
            acceptance,
            depends_on,
            token_budget,
            cost_budget_usd,
            time_budget_secs,
            max_attempts,
            labels,
        } => {
            let instructions = if instructions == "-" {
                let mut buffer = String::new();
                std::io::stdin()
                    .read_to_string(&mut buffer)
                    .context("reading intake instructions from stdin")?;
                buffer
            } else {
                instructions
            };
            rpc(
                &discovery,
                "intake.update",
                json!({
                    "id": id,
                    "title": title,
                    "instructions": instructions,
                    "profile": profile,
                    "kind": kind,
                    "priority": priority,
                    "write_scope": scope,
                    "acceptance": acceptance,
                    "deps": depends_on,
                    "token_budget": token_budget,
                    "cost_budget_usd": cost_budget_usd,
                    "time_budget_secs": time_budget_secs,
                    "max_attempts": max_attempts,
                    "labels": labels,
                }),
            )
            .await?
        }
        Command::IntakeRun { id } => rpc(&discovery, "intake.dispatch", json!({"id": id})).await?,
        Command::IntakeCancel { id } => rpc(&discovery, "intake.cancel", json!({"id": id})).await?,
        Command::IntakeRetry { id } => rpc(&discovery, "intake.retry", json!({"id": id})).await?,
        Command::IntakeDelete { id } => rpc(&discovery, "intake.delete", json!({"id": id})).await?,
        Command::TaskDispatch { session } => {
            rpc(&discovery, "task.dispatch", json!({"session_id": session})).await?
        }
        Command::TaskRetry { session, task } => {
            rpc(
                &discovery,
                "task.retry",
                json!({"session_id": session, "task_id": task}),
            )
            .await?
        }
        Command::TaskSkip { session, task } => {
            rpc(
                &discovery,
                "task.skip",
                json!({"session_id": session, "task_id": task}),
            )
            .await?
        }
        Command::TaskCancel { session, task } => {
            rpc(
                &discovery,
                "task.cancel",
                json!({"session_id": session, "task_id": task}),
            )
            .await?
        }
        Command::TaskPause { session, task } => {
            rpc(
                &discovery,
                "task.pause",
                json!({"session_id": session, "task_id": task}),
            )
            .await?
        }
        Command::TaskResume { session, task } => {
            rpc(
                &discovery,
                "task.resume",
                json!({"session_id": session, "task_id": task}),
            )
            .await?
        }
        Command::Steer {
            session,
            text,
            agent,
            task,
            operation_id,
        } => {
            rpc(
                &discovery,
                "session.steer",
                json!({
                    "session_id": session,
                    "body": text,
                    "agent_id": agent,
                    "task_id": task,
                    "operation_id": operation_id,
                }),
            )
            .await?
        }
        Command::Steering {
            session,
            status,
            limit,
        } => {
            rpc(
                &discovery,
                "session.steering",
                json!({"session_id": session, "status": status, "limit": limit}),
            )
            .await?
        }
        Command::SteerCancel { id } => {
            rpc(&discovery, "session.steering.cancel", json!({"id": id})).await?
        }
        Command::Defer {
            session,
            prompt,
            watch,
            gate,
            watch_mode,
            after_secs,
            at,
            every_secs,
            max_runs,
            operation_id,
        } => {
            rpc(
                &discovery,
                "session.defer",
                json!({
                    "session_id": session,
                    "body": prompt,
                    "watch_task_ids": watch,
                    "watch_gate_id": gate,
                    "watch_mode": watch_mode,
                    "after_secs": after_secs,
                    "deadline_at": at,
                    "every_secs": every_secs,
                    "max_runs": max_runs,
                    "operation_id": operation_id,
                }),
            )
            .await?
        }
        Command::Continuations {
            session,
            status,
            limit,
        } => {
            rpc(
                &discovery,
                "session.continuations",
                json!({"session_id": session, "status": status, "limit": limit}),
            )
            .await?
        }
        Command::ContinuationCancel { id } => {
            rpc(&discovery, "session.continuation.cancel", json!({"id": id})).await?
        }
        Command::ContinuationStatus { id } => {
            rpc(&discovery, "session.continuation.status", json!({"id": id})).await?
        }
        Command::ContinuationReclaim { id, reason } => {
            rpc(
                &discovery,
                "session.continuation.reclaim",
                json!({"id": id, "reason": reason}),
            )
            .await?
        }
        Command::TaskStalls { session } => {
            rpc(&discovery, "session.stalls", json!({"session_id": session})).await?
        }
        Command::ToolActivity {
            session,
            limit,
            csv: _,
        } => {
            rpc(
                &discovery,
                "session.tool_activity",
                json!({"session_id": session, "limit": limit}),
            )
            .await?
        }
        Command::Recoveries { session } => {
            rpc(
                &discovery,
                "orchestration.recoveries",
                json!({"session_id": session}),
            )
            .await?
        }
        _ => unreachable!("command routed to the wrong dispatch family"),
    };
    Ok(Some(value))
}

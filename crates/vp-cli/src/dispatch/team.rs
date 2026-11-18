//! Team plan, orchestration, lease, and progress commands.

use super::*;

pub(super) async fn dispatch(
    command: Command,
    discovery: Discovery,
    _json: bool,
) -> Result<Option<Value>> {
    let value = match command {
        Command::OrchestrationPolicy { project } => {
            rpc(
                &discovery,
                "orchestration.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::OrchestrationPolicySet {
            project,
            auto_retry,
            max_attempts,
            retry_backoff_secs,
            retry_priority_boost,
            dependency_failure,
            escalate_after_retries,
            auto_resolve_blockers,
            stall_recovery,
            stall_min_observations,
            stall_max_attempts,
            stall_grace_secs,
        } => {
            let current = rpc(
                &discovery,
                "orchestration.policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy = current.get("policy").cloned().unwrap_or_else(|| json!({}));
            if let Some(value) = auto_retry {
                policy["auto_retry_failed"] = json!(value);
            }
            if let Some(value) = max_attempts {
                policy["max_attempts"] = json!(value);
            }
            if let Some(value) = retry_backoff_secs {
                policy["retry_backoff_secs"] = json!(value);
            }
            if let Some(value) = retry_priority_boost {
                policy["retry_priority_boost"] = json!(value);
            }
            if let Some(value) = dependency_failure {
                policy["dependency_failure"] = json!(value);
            }
            if let Some(value) = escalate_after_retries {
                policy["escalate_after_retries"] = json!(value);
            }
            if let Some(value) = auto_resolve_blockers {
                policy["auto_resolve_blockers"] = json!(value);
            }
            if let Some(value) = stall_recovery {
                policy["stall_recovery"] = json!(value);
            }
            if let Some(value) = stall_min_observations {
                policy["stall_min_observations"] = json!(value);
            }
            if let Some(value) = stall_max_attempts {
                policy["stall_max_attempts"] = json!(value);
            }
            if let Some(value) = stall_grace_secs {
                policy["stall_grace_secs"] = json!(value);
            }
            rpc(
                &discovery,
                "orchestration.policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::Orchestration { session } => {
            rpc(
                &discovery,
                "orchestration.snapshot",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::PlanRevisions { session } => {
            rpc(
                &discovery,
                "team_plan.revisions",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::OrchestrationOverview { project, limit } => {
            let mut params = json!({"limit": limit});
            if let Some(project) = project {
                params["project_id"] = json!(project);
            }
            rpc(&discovery, "orchestration.overview", params).await?
        }
        Command::Orchestrate { session, dry_run } => {
            rpc(
                &discovery,
                if dry_run {
                    "orchestration.preview"
                } else {
                    "orchestration.advance"
                },
                json!({"session_id": session}),
            )
            .await?
        }
        Command::Coordination {
            session,
            action,
            task,
            agent,
            note,
            severity,
            message,
        } => {
            rpc(
                &discovery,
                "coordination.note",
                json!({
                    "session_id": session,
                    "action": action,
                    "task_id": task,
                    "target_agent_id": agent,
                    "target_note_id": note,
                    "severity": severity,
                    "message": message,
                }),
            )
            .await?
        }
        Command::Unblock {
            session,
            task,
            note,
            message,
        } => {
            rpc(
                &discovery,
                "coordination.note",
                json!({
                    "session_id": session,
                    "action": "unblock",
                    "task_id": task,
                    "target_note_id": note,
                    "severity": "info",
                    "message": message,
                }),
            )
            .await?
        }
        Command::PlanRevision { session, revision } => {
            rpc(
                &discovery,
                "team_plan.revision",
                json!({"session_id": session, "revision": revision}),
            )
            .await?
        }
        Command::PlanRestore {
            session,
            revision,
            reason,
            expected_revision,
        } => {
            rpc(
                &discovery,
                "team_plan.restore",
                json!({
                    "session_id": session,
                    "revision": revision,
                    "reason": reason,
                    "expected_revision": expected_revision,
                }),
            )
            .await?
        }
        Command::TaskEdit {
            session,
            file,
            reason,
            expected_revision,
        } => {
            let raw = read_json_input(file.as_deref())?;
            let task: Value = serde_json::from_str(&raw).context("parsing task JSON")?;
            rpc(
                &discovery,
                "team_plan.task_edit",
                json!({
                    "session_id": session,
                    "task": task,
                    "reason": reason,
                    "expected_revision": expected_revision,
                }),
            )
            .await?
        }
        Command::PlanFanout {
            session,
            template,
            items,
            file,
            artifact,
            join_task,
            join_title,
            join_acceptance,
            reason,
            expected_revision,
            yes,
        } => {
            let items: Vec<Value> = match (items, file) {
                (Some(raw), None) => serde_json::from_str(&raw).context("parsing fan-out items")?,
                (None, Some(path)) => {
                    let raw = read_json_input(Some(&path))?;
                    serde_json::from_str(&raw).context("parsing fan-out items")?
                }
                (None, None) => Vec::new(),
                (Some(_), Some(_)) => bail!("pass either --items or --file, not both"),
            };
            rpc(
                &discovery,
                "plan.fanout",
                json!({
                    "session_id": session,
                    "template_task_id": template,
                    "items": items,
                    "artifact_id": artifact,
                    "join_task_id": join_task,
                    "join_title": join_title,
                    "join_acceptance": join_acceptance,
                    "reason": reason,
                    "expected_revision": expected_revision,
                    "approved": yes,
                    "author": "operator",
                }),
            )
            .await?
        }
        Command::TeamPlanRevise {
            session,
            reason,
            file,
            expected_revision,
        } => {
            let raw = read_json_input(file.as_deref())?;
            let plan: Value = serde_json::from_str(&raw).context("parsing plan JSON")?;
            rpc(
                &discovery,
                "team_plan.revise",
                json!({
                    "session_id": session,
                    "plan": plan,
                    "reason": reason,
                    "expected_revision": expected_revision,
                }),
            )
            .await?
        }
        Command::Leases => rpc(&discovery, "runtime.leases", json!({})).await?,
        Command::LeasesSweep => rpc(&discovery, "runtime.leases.sweep", json!({})).await?,
        Command::LeaseHistory { key, limit } => {
            rpc(
                &discovery,
                "runtime.leases.history",
                json!({"lease_key": key, "limit": limit}),
            )
            .await?
        }
        Command::PlanMigrate {
            session,
            reason,
            expected_revision,
            yes,
            approved_by,
        } => {
            rpc(
                &discovery,
                "plan.migrate",
                json!({
                    "session_id": session,
                    "reason": reason,
                    "expected_revision": expected_revision,
                    "approved": yes,
                    "approved_by": approved_by,
                }),
            )
            .await?
        }
        Command::OrchestrateReplay { session, limit } => {
            rpc(
                &discovery,
                "orchestration.replay",
                json!({"session_id": session, "limit": limit}),
            )
            .await?
        }
        Command::Progress {
            session,
            plan_revision,
            limit,
        } => {
            rpc(
                &discovery,
                "session.progress",
                json!({
                    "session_id": session,
                    "plan_revision": plan_revision,
                    "limit": limit,
                }),
            )
            .await?
        }
        Command::ProgressReport { session, payload } => {
            let round: serde_json::Value =
                serde_json::from_str(&payload).context("parsing --payload as JSON")?;
            rpc(
                &discovery,
                "session.progress.report",
                json!({"session_id": session, "round": round}),
            )
            .await?
        }
        Command::TeamPlan { session } => {
            rpc(&discovery, "team_plan.get", json!({"session_id": session})).await?
        }
        _ => unreachable!("command routed to the wrong dispatch family"),
    };
    Ok(Some(value))
}

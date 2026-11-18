//! Approval, approval-template, and operator-question commands.

use super::*;

pub(super) async fn dispatch(
    command: Command,
    discovery: Discovery,
    _json: bool,
) -> Result<Option<Value>> {
    let value = match command {
        Command::Approvals { session, project } => {
            let project = match project {
                Some(project) => Some(resolve_project(&discovery, Some(&project)).await?),
                None => None,
            };
            rpc(
                &discovery,
                "approval.queue",
                json!({"session_id": session, "project_id": project}),
            )
            .await?
        }
        Command::ApprovalBatch { approve, deny } => {
            if approve.is_empty() && deny.is_empty() {
                bail!("approval-batch needs at least one --approve or --deny id");
            }
            if let Some(id) = approve.iter().find(|id| deny.contains(id)) {
                bail!("approval id '{id}' appears in both --approve and --deny");
            }
            let mut decisions = Vec::new();
            decisions.extend(
                approve
                    .into_iter()
                    .map(|id| json!({"approval_id": id, "approved": true})),
            );
            decisions.extend(
                deny.into_iter()
                    .map(|id| json!({"approval_id": id, "approved": false})),
            );
            rpc(
                &discovery,
                "approval.batch.resolve",
                json!({"decisions": decisions}),
            )
            .await?
        }
        Command::ApprovalTemplate { project } => {
            let project = resolve_project(&discovery, project.as_deref()).await?;
            rpc(
                &discovery,
                "approval.templates",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ApprovalTemplateSet {
            project,
            id,
            name,
            tool_pattern,
            risk,
            path_glob,
            action,
            expires_at,
            max_uses,
            disabled,
        } => {
            let project = resolve_project(&discovery, project.as_deref()).await?;
            if let Some(id) = id {
                let current = rpc(
                    &discovery,
                    "approval.templates",
                    json!({"project_id": project}),
                )
                .await?;
                let existing = current
                    .get("templates")
                    .and_then(Value::as_array)
                    .and_then(|templates| {
                        templates.iter().find(|template| {
                            template.get("id").and_then(Value::as_str) == Some(&id)
                        })
                    })
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("approval template not found: {id}"))?;
                let name = name.unwrap_or_else(|| {
                    existing
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("template")
                        .to_string()
                });
                let tool_pattern = tool_pattern.unwrap_or_else(|| {
                    existing
                        .get("tool_pattern")
                        .and_then(Value::as_str)
                        .unwrap_or("write_file")
                        .to_string()
                });
                let action = action.unwrap_or_else(|| {
                    existing
                        .get("action")
                        .and_then(Value::as_str)
                        .unwrap_or("allow")
                        .to_string()
                });
                let risk = if risk.is_empty() {
                    existing
                        .get("risk_levels")
                        .and_then(Value::as_array)
                        .map(|levels| {
                            levels
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                } else {
                    risk
                };
                let path_glob = if path_glob.is_empty() {
                    existing
                        .get("path_globs")
                        .and_then(Value::as_array)
                        .map(|globs| {
                            globs
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                } else {
                    path_glob
                };
                rpc(
                    &discovery,
                    "approval.template.update",
                    json!({
                        "id": id,
                        "name": name,
                        "enabled": !disabled,
                        "tool_pattern": tool_pattern,
                        "risk_levels": risk,
                        "path_globs": path_glob,
                        "action": action,
                        "expires_at": expires_at.or_else(|| {
                            existing.get("expires_at").cloned().and_then(|value| value.as_str().map(str::to_string))
                        }),
                        "max_uses": max_uses.or_else(|| {
                            existing.get("max_uses").and_then(Value::as_u64).map(|value| value as u32)
                        }),
                    }),
                )
                .await?
            } else {
                let name = name.ok_or_else(|| anyhow::anyhow!("--name is required"))?;
                let tool_pattern =
                    tool_pattern.ok_or_else(|| anyhow::anyhow!("--tool-pattern is required"))?;
                let action = action.ok_or_else(|| anyhow::anyhow!("--action is required"))?;
                rpc(
                    &discovery,
                    "approval.template.create",
                    json!({
                        "project_id": project,
                        "name": name,
                        "tool_pattern": tool_pattern,
                        "risk_levels": risk,
                        "path_globs": path_glob,
                        "action": action,
                        "expires_at": expires_at,
                        "max_uses": max_uses,
                    }),
                )
                .await?
            }
        }
        Command::ApprovalTemplateDelete { id } => {
            rpc(&discovery, "approval.template.delete", json!({"id": id})).await?
        }
        Command::ApprovalPolicy { project } => {
            rpc(
                &discovery,
                "approval.policy",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ApprovalPolicySet {
            project,
            enabled,
            max_pending,
            coalesce_secs,
            stale_secs,
            priority_boost_risk,
            notify_coalesce_secs,
        } => {
            let current = rpc(
                &discovery,
                "approval.policy",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy = current.get("policy").cloned().unwrap_or_else(|| json!({}));
            if let Some(enabled) = enabled {
                policy["enabled"] = json!(enabled);
            }
            if let Some(max_pending) = max_pending {
                policy["max_pending_per_session"] = json!(max_pending);
            }
            if let Some(coalesce_secs) = coalesce_secs {
                policy["coalesce_window_secs"] = json!(coalesce_secs);
            }
            if let Some(stale_secs) = stale_secs {
                policy["stale_after_secs"] = json!(stale_secs);
            }
            if let Some(priority_boost_risk) = priority_boost_risk {
                policy["priority_boost_risk"] = json!(priority_boost_risk);
            }
            if let Some(notify_coalesce_secs) = notify_coalesce_secs {
                policy["notify_coalesce_secs"] = json!(notify_coalesce_secs);
            }
            rpc(
                &discovery,
                "approval.policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::ApprovalAudit { project, limit } => {
            let project = resolve_project(&discovery, Some(&project)).await?;
            rpc(
                &discovery,
                "approval.audit",
                json!({"project_id": project, "limit": limit}),
            )
            .await?
        }
        Command::Questions { session } => {
            rpc(&discovery, "question.list", json!({"session_id": session})).await?
        }
        Command::Answer { question, answer } => {
            rpc(
                &discovery,
                "question.answer",
                json!({"question_id": question, "answer": answer}),
            )
            .await?
        }
        Command::Approve { approval } => {
            rpc(
                &discovery,
                "approval.resolve",
                json!({"approval_id": approval, "approved": true}),
            )
            .await?
        }
        Command::Deny { approval } => {
            rpc(
                &discovery,
                "approval.resolve",
                json!({"approval_id": approval, "approved": false}),
            )
            .await?
        }
        _ => unreachable!("command routed to the wrong dispatch family"),
    };
    Ok(Some(value))
}

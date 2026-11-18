//! Prompt-cache, instruction, and tool-bundle commands.

use super::*;

pub(super) async fn dispatch(
    command: Command,
    discovery: Discovery,
    _json: bool,
) -> Result<Option<Value>> {
    let value = match command {
        Command::Cache { session } => {
            rpc(&discovery, "session.cache", json!({"session_id": session})).await?
        }
        Command::CacheEpoch { session, limit } => {
            rpc(
                &discovery,
                "session.cache.epoch",
                json!({"session_id": session, "limit": limit}),
            )
            .await?
        }
        Command::CacheEpochs { project, limit } => {
            rpc(
                &discovery,
                "project.cache_epochs",
                json!({"project_id": project, "limit": limit}),
            )
            .await?
        }
        Command::CacheEpochsPrune {
            project,
            older_than_days,
            keep_epochs,
            dry_run,
        } => {
            rpc(
                &discovery,
                "project.cache_epochs.prune",
                json!({
                    "project_id": project,
                    "older_than_days": older_than_days,
                    "keep_epochs": keep_epochs,
                    "dry_run": dry_run,
                }),
            )
            .await?
        }
        Command::CacheCalls {
            session,
            limit,
            role,
            miss_reason,
            include_warm,
        } => {
            rpc(
                &discovery,
                "session.cache.calls",
                json!({
                    "session_id": session,
                    "limit": limit,
                    "role": role,
                    "miss_reason": miss_reason,
                    "include_warm": include_warm,
                }),
            )
            .await?
        }
        Command::Instructions { session, agent } => {
            rpc(
                &discovery,
                "session.instructions",
                json!({"session_id": session, "agent_id": agent}),
            )
            .await?
        }
        Command::PromptLayout { session, layout } => {
            if !matches!(layout, 2 | 3) {
                bail!("prompt layout must be 2 or 3, got {layout}");
            }
            rpc(
                &discovery,
                "session.prompt_layout.set",
                json!({"session_id": session, "prompt_layout": layout}),
            )
            .await?
        }
        Command::CacheAudit { session } => {
            rpc(
                &discovery,
                "session.cache.audit",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::CacheBreaks { session, limit } => {
            rpc(
                &discovery,
                "session.cache.breaks",
                json!({"session_id": session, "limit": limit}),
            )
            .await?
        }
        Command::CachePlan { session, role } => {
            rpc(
                &discovery,
                "session.cache.plan",
                json!({"session_id": session, "roles": role}),
            )
            .await?
        }
        Command::CacheWarm { session, role } => {
            rpc(
                &discovery,
                "session.cache.warm",
                json!({"session_id": session, "role": role}),
            )
            .await?
        }
        Command::ToolBundles {
            matrix,
            task_kind,
            diff,
            role,
            profile,
        } => {
            if let Some(diff) = diff {
                let Some((from_bundle, to_bundle)) = diff.split_once(',') else {
                    bail!("--diff expects FROM,TO");
                };
                rpc(
                    &discovery,
                    "tool_bundle.diff",
                    json!({
                        "from_bundle": from_bundle.trim(),
                        "to_bundle": to_bundle.trim(),
                        "role": role,
                        "profile": profile,
                        "task_kind": task_kind.first(),
                    }),
                )
                .await?
            } else {
                let method = if matrix {
                    "tool_bundle.matrix"
                } else {
                    "tool_bundle.catalog"
                };
                rpc(
                    &discovery,
                    method,
                    json!({"task_kinds": if matrix { task_kind } else { Vec::new() } }),
                )
                .await?
            }
        }
        Command::ToolBundle {
            session,
            role,
            task_kind,
            tools,
            history,
        } => {
            if history {
                rpc(
                    &discovery,
                    "session.tool_bundle.history",
                    json!({"session_id": session, "role": role.first()}),
                )
                .await?
            } else {
                rpc(
                    &discovery,
                    "session.tool_bundle",
                    json!({
                        "session_id": session,
                        "roles": role,
                        "task_kind": task_kind,
                        "include_tools": tools,
                    }),
                )
                .await?
            }
        }
        Command::CachePolicy { project } => {
            rpc(
                &discovery,
                "project.cache_policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::CachePolicySet {
            project,
            disabled,
            ttl,
            no_automatic,
            warm_on_resume,
            no_compact_aware,
            role_ttl,
            prompt_layout,
        } => {
            // Read-modify-write preserves role overrides that the operator did
            // not mention instead of silently clearing them.
            let existing = rpc(
                &discovery,
                "project.cache_policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut roles = existing["policy"]["role_ttl"]
                .as_object()
                .cloned()
                .unwrap_or_default();
            for entry in role_ttl {
                let Some((role, value)) = entry.split_once('=') else {
                    bail!("--role-ttl must be ROLE=TTL, got '{entry}'");
                };
                if role.trim().is_empty() || value.trim().is_empty() {
                    bail!("--role-ttl must be ROLE=TTL, got '{entry}'");
                }
                roles.insert(
                    role.trim().to_string(),
                    Value::String(value.trim().to_string()),
                );
            }
            let prompt_layout = prompt_layout.unwrap_or_else(|| {
                existing["policy"]["prompt_layout"]
                    .as_u64()
                    .map(|value| value as u32)
                    .unwrap_or(3)
            });
            if !matches!(prompt_layout, 2 | 3) {
                bail!("--prompt-layout must be 2 or 3, got {prompt_layout}");
            }
            rpc(
                &discovery,
                "project.cache_policy.set",
                json!({
                    "project_id": project,
                    "policy": {
                        "enabled": !disabled,
                        "ttl": ttl,
                        "automatic_caching": !no_automatic,
                        "warm_on_resume": warm_on_resume,
                        "compact_aware": !no_compact_aware,
                        "role_ttl": roles,
                        "prompt_layout": prompt_layout,
                    }
                }),
            )
            .await?
        }
        _ => unreachable!("command routed to the wrong dispatch family"),
    };
    Ok(Some(value))
}

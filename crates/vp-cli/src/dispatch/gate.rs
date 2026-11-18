//! External gates, effect receipts, and task contracts.

use super::*;

pub(super) async fn dispatch(
    command: Command,
    discovery: Discovery,
    _json: bool,
) -> Result<Option<Value>> {
    let value = match command {
        Command::Contract { session, task } => {
            rpc(
                &discovery,
                "session.contract",
                json!({"session_id": session, "task_id": task}),
            )
            .await?
        }
        Command::ContractPolicy { project } => {
            rpc(
                &discovery,
                "project.contract_policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ContractPolicySet {
            project,
            enabled,
            mode,
            enforce_shell,
            forbidden_paths,
            egress_allowlist,
            max_writes,
            max_bytes,
            max_commits,
        } => {
            let current = rpc(
                &discovery,
                "project.contract_policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy = current.get("policy").cloned().unwrap_or_else(|| json!({}));
            if let Some(enabled) = enabled {
                policy["enabled"] = json!(enabled);
            }
            if let Some(mode) = mode {
                if vp_protocol::types::ContractMode::parse(&mode).is_none() {
                    bail!("contract mode must be `advisory` or `enforce`, got {mode}");
                }
                policy["mode"] = json!(mode.to_ascii_lowercase());
            }
            if let Some(enforce_shell) = enforce_shell {
                policy["enforce_shell"] = json!(enforce_shell);
            }
            if !forbidden_paths.is_empty() {
                policy["forbidden_paths"] = json!(forbidden_paths);
            }
            if !egress_allowlist.is_empty() {
                policy["egress_allowlist"] = json!(egress_allowlist);
            }
            if let Some(max_writes) = max_writes {
                policy["max_writes"] = json!(max_writes);
            }
            if let Some(max_bytes) = max_bytes {
                policy["max_bytes"] = json!(max_bytes);
            }
            if let Some(max_commits) = max_commits {
                policy["max_commits"] = json!(max_commits);
            }
            rpc(
                &discovery,
                "project.contract_policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::Effects {
            session,
            task,
            limit,
            unresolved,
        } => {
            rpc(
                &discovery,
                "session.effects",
                json!({
                    "session_id": session,
                    "task_id": task,
                    "limit": limit,
                    "include_settled": !unresolved,
                }),
            )
            .await?
        }
        Command::EffectReconcile {
            effect,
            session,
            done,
            failed,
            sha256,
            commit,
            external_id,
            http_status,
            note,
            operator,
        } => {
            if done == failed {
                bail!("pass exactly one of --done or --failed");
            }
            let decision = if done { "done" } else { "failed" };
            rpc(
                &discovery,
                "effect.reconcile",
                json!({
                    "session_id": session,
                    "effect_id": effect,
                    "decision": decision,
                    "evidence": {
                        "sha256": sha256,
                        "commit": commit,
                        "external_id": external_id,
                        "http_status": http_status,
                        "note": note,
                    },
                    "operator": operator,
                }),
            )
            .await?
        }
        Command::EffectPolicy { project } => {
            rpc(
                &discovery,
                "project.effect_policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::EffectPolicySet {
            project,
            enabled,
            unknown_blocks_merge,
            stale_intent_grace_secs,
            shell_mode,
            track_mutating_http,
            track_read_http,
            track_mcp_mutations,
            track_workspace_writes,
        } => {
            let current = rpc(
                &discovery,
                "project.effect_policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy = current.get("policy").cloned().unwrap_or_else(|| json!({}));
            if let Some(enabled) = enabled {
                policy["enabled"] = json!(enabled);
            }
            if let Some(value) = unknown_blocks_merge {
                policy["unknown_blocks_merge"] = json!(value);
            }
            if let Some(value) = stale_intent_grace_secs {
                policy["stale_intent_grace_secs"] = json!(value);
            }
            if let Some(mode) = shell_mode {
                if vp_protocol::types::EffectShellMode::parse(&mode).is_none() {
                    bail!("shell mode must be `external_only` or `all`, got {mode}");
                }
                policy["shell_mode"] = json!(mode.to_ascii_lowercase());
            }
            if let Some(value) = track_mutating_http {
                policy["track_mutating_http"] = json!(value);
            }
            if let Some(value) = track_read_http {
                policy["track_read_http"] = json!(value);
            }
            if let Some(value) = track_mcp_mutations {
                policy["track_mcp_mutations"] = json!(value);
            }
            if let Some(value) = track_workspace_writes {
                policy["track_workspace_writes"] = json!(value);
            }
            rpc(
                &discovery,
                "project.effect_policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::EffectSweep { limit } => {
            rpc(&discovery, "effect.sweep", json!({"limit": limit})).await?
        }
        Command::Gate { command } => match command {
            GateCommand::Open {
                session,
                task,
                kind,
                label,
                payload,
                after_secs,
                at,
            } => {
                let payload = match payload {
                    Some(payload) => Some(serde_json::from_str::<Value>(&payload)?),
                    None => None,
                };
                rpc(
                    &discovery,
                    "gate.open",
                    json!({
                        "session_id": session,
                        "task_id": task,
                        "kind": kind,
                        "label": label,
                        "payload": payload,
                        "after_secs": after_secs,
                        "expires_at": at,
                    }),
                )
                .await?
            }
            GateCommand::Resolve {
                gate,
                token,
                token_stdin,
                token_file,
                outcome,
                note,
                operator,
            } => {
                let token = read_gate_token(token, token_stdin, token_file.as_deref())?;
                rpc(
                    &discovery,
                    "gate.resolve",
                    json!({
                        "gate_id": gate,
                        "token": token,
                        "outcome": outcome,
                        "note": note,
                        "resolved_by": operator,
                    }),
                )
                .await?
            }
            GateCommand::Cancel {
                gate,
                reason,
                operator,
            } => {
                rpc(
                    &discovery,
                    "gate.cancel",
                    json!({
                        "gate_id": gate,
                        "reason": reason,
                        "cancelled_by": operator,
                    }),
                )
                .await?
            }
            GateCommand::List {
                session,
                state,
                limit,
            } => {
                rpc(
                    &discovery,
                    "gate.list",
                    json!({"session_id": session, "state": state, "limit": limit}),
                )
                .await?
            }
            GateCommand::Status { gate } => {
                rpc(&discovery, "gate.status", json!({"gate_id": gate})).await?
            }
            GateCommand::Sweep { limit } => {
                rpc(&discovery, "gate.sweep", json!({"limit": limit})).await?
            }
            GateCommand::Policy { project } => {
                rpc(
                    &discovery,
                    "project.gate_policy.get",
                    json!({"project_id": project}),
                )
                .await?
            }
            GateCommand::PolicySet { project, loopback } => {
                rpc(
                    &discovery,
                    "project.gate_policy.set",
                    json!({"project_id": project, "policy": {"loopback_resolve": loopback}}),
                )
                .await?
            }
        },
        _ => unreachable!("command routed to the wrong dispatch family"),
    };
    Ok(Some(value))
}

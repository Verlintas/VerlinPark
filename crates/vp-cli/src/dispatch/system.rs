//! Daemon, telemetry, and A2A commands.

use super::*;

pub(super) async fn dispatch(
    command: Command,
    discovery: Discovery,
    json: bool,
) -> Result<Option<Value>> {
    let value = match command {
        Command::Ping => {
            let result = rpc(&discovery, "ping", json!({})).await?;
            if json {
                print_json(&result)?;
            } else {
                println!(
                    "pong · v{} · protocol {} · up {}s",
                    result["version"].as_str().unwrap_or("?"),
                    result["protocol"].as_u64().unwrap_or(0),
                    result["uptime_secs"].as_u64().unwrap_or(0)
                );
            }
            return Ok(None);
        }
        Command::OtelStatus { project, session } => {
            rpc(
                &discovery,
                "telemetry.otel.status",
                json!({"project_id": project, "session_id": session}),
            )
            .await?
        }
        Command::OtelPolicy { project } => {
            rpc(
                &discovery,
                "project.otel_policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::OtelPolicySet {
            project,
            enable,
            disable,
            endpoint,
            service_name,
            batch_limit,
            timeout_secs,
        } => {
            if enable && disable {
                bail!("--enable and --disable are mutually exclusive");
            }
            let current = rpc(
                &discovery,
                "project.otel_policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy = current.get("policy").cloned().unwrap_or_else(|| json!({}));
            if let Some(object) = policy.as_object_mut() {
                if enable || disable {
                    object.insert("enabled".into(), json!(enable));
                }
                if let Some(endpoint) = endpoint {
                    object.insert("endpoint".into(), json!(endpoint));
                }
                if let Some(service_name) = service_name {
                    object.insert("service_name".into(), json!(service_name));
                }
                if let Some(batch_limit) = batch_limit {
                    object.insert("batch_limit".into(), json!(batch_limit));
                }
                if let Some(timeout_secs) = timeout_secs {
                    object.insert("timeout_secs".into(), json!(timeout_secs));
                }
            }
            rpc(
                &discovery,
                "project.otel_policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::OtelExport {
            session,
            limit,
            offset,
        } => {
            rpc(
                &discovery,
                "telemetry.otel.export",
                json!({"session_id": session, "limit": limit, "offset": offset}),
            )
            .await?
        }
        Command::OtelTest { project } => {
            rpc(
                &discovery,
                "telemetry.otel.test",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::Metrics { prometheus } => {
            let result = rpc(&discovery, "daemon.metrics", json!({})).await?;
            if prometheus {
                let text = result
                    .get("prometheus")
                    .and_then(Value::as_str)
                    .context("daemon.metrics returned no Prometheus text")?;
                print!("{text}");
                if !text.ends_with('\n') {
                    println!();
                }
            } else {
                print_json(result.get("metrics").unwrap_or(&result))?;
            }
            return Ok(None);
        }
        Command::Info => rpc(&discovery, "server.info", json!({})).await?,
        Command::Doctor => rpc(&discovery, "server.doctor", json!({})).await?,
        Command::ProviderTest { tools } => {
            let connection = rpc(&discovery, "agent.probe", json!({})).await?;
            if tools {
                let capability = rpc(&discovery, "agent.probe.tools", json!({})).await?;
                json!({"connection": connection, "tools": capability})
            } else {
                connection
            }
        }
        Command::ApprovalModeSet { mode } => {
            let mode = mode.trim().to_ascii_lowercase();
            if !matches!(mode.as_str(), "smart" | "strict" | "unattended") {
                bail!("mode must be one of: smart, strict, unattended");
            }
            rpc(
                &discovery,
                "agent.approval_mode.set",
                json!({"approval_mode": mode}),
            )
            .await?
        }
        Command::RuntimeConfigSet {
            approval_mode,
            context_window,
            max_turns,
            max_tokens,
        } => {
            if approval_mode.is_none()
                && context_window.is_none()
                && max_turns.is_none()
                && max_tokens.is_none()
            {
                bail!(
                    "provide at least one of --approval-mode, --context-window, --max-turns, or --max-tokens"
                );
            }
            let approval_mode = approval_mode.map(|mode| mode.trim().to_ascii_lowercase());
            if approval_mode
                .as_deref()
                .is_some_and(|mode| !matches!(mode, "smart" | "strict" | "unattended"))
            {
                bail!("--approval-mode must be one of: smart, strict, unattended");
            }
            rpc(
                &discovery,
                "agent.runtime_config.set",
                json!({
                    "approval_mode": approval_mode,
                    "context_window": context_window,
                    "max_turns": max_turns,
                    "max_tokens": max_tokens,
                }),
            )
            .await?
        }
        Command::A2aStatus => rpc(&discovery, "a2a.status", json!({})).await?,
        Command::A2aCard => rpc(&discovery, "a2a.card", json!({})).await?,
        Command::A2aPolicySet {
            enable,
            disable,
            allow_inbound,
            port,
            max_tasks,
            task_ttl_secs,
        } => {
            if enable && disable {
                bail!("--enable and --disable are mutually exclusive");
            }
            let mut policy = vp_protocol::a2a::A2aPolicy {
                enabled: enable && !disable,
                allow_inbound,
                ..Default::default()
            };
            if let Some(port) = port {
                policy.port = port;
            }
            if let Some(max_tasks) = max_tasks {
                policy.max_tasks = max_tasks;
            }
            if let Some(task_ttl_secs) = task_ttl_secs {
                policy.task_ttl_secs = task_ttl_secs;
            }
            rpc(&discovery, "a2a.policy.set", json!({"policy": policy})).await?
        }
        Command::A2aTasks { limit, state } => {
            rpc(
                &discovery,
                "a2a.tasks",
                json!({"limit": limit, "state": state}),
            )
            .await?
        }
        Command::A2aStart {
            approved,
            port,
            max_tasks,
            task_ttl_secs,
            allow_inbound,
            enable,
        } => {
            let mut policy = vp_protocol::a2a::A2aPolicy {
                enabled: enable,
                allow_inbound,
                ..Default::default()
            };
            if let Some(port) = port {
                policy.port = port;
            }
            if let Some(max_tasks) = max_tasks {
                policy.max_tasks = max_tasks;
            }
            if let Some(task_ttl_secs) = task_ttl_secs {
                policy.task_ttl_secs = task_ttl_secs;
            }
            rpc(
                &discovery,
                "a2a.start",
                json!({"approved": approved, "policy": policy}),
            )
            .await?
        }
        Command::A2aStop { reason } => {
            rpc(&discovery, "a2a.stop", json!({"reason": reason})).await?
        }
        _ => unreachable!("command routed to the wrong dispatch family"),
    };
    Ok(Some(value))
}

//! Agent Client Protocol (ACP) commands.

use super::*;

pub(super) async fn dispatch(
    command: Command,
    discovery: Discovery,
    _json: bool,
) -> Result<Option<Value>> {
    let value = match command {
        Command::AcpAgents => rpc(&discovery, "acp.agent.list", json!({})).await?,
        Command::AcpAdd {
            name,
            command,
            transport,
            endpoint,
            auth_header_env,
            headers,
            allow_remote,
            connect_timeout,
            args,
            env,
            auto_approve,
            timeout,
        } => {
            let env = parse_env_pairs(&env)?;
            let headers = parse_pairs(&headers, "--header")?;
            rpc(
                &discovery,
                "acp.agent.add",
                json!({
                    "name": name,
                    "command": command.unwrap_or_default(),
                    "args": args,
                    "env": env,
                    "enabled": true,
                    "auto_approve": auto_approve,
                    "timeout_secs": timeout,
                    "transport": transport.as_str(),
                    "endpoint": endpoint,
                    "auth_header_env": auth_header_env,
                    "headers": headers,
                    "connect_timeout_secs": connect_timeout,
                    "allow_remote": allow_remote
                }),
            )
            .await?
        }
        Command::AcpUpdate {
            agent,
            name,
            command,
            transport,
            endpoint,
            clear_endpoint,
            auth_header_env,
            clear_auth_header_env,
            headers,
            clear_headers,
            allow_remote,
            no_allow_remote,
            connect_timeout,
            args,
            env,
            auto_approve,
            no_auto_approve,
            enabled,
            disabled,
            timeout,
        } => {
            if clear_endpoint && endpoint.is_some() {
                bail!("use either --endpoint or --clear-endpoint, not both");
            }
            if clear_auth_header_env && auth_header_env.is_some() {
                bail!("use either --auth-header-env or --clear-auth-header-env, not both");
            }
            if clear_headers && !headers.is_empty() {
                bail!("use either --header or --clear-headers, not both");
            }
            if allow_remote && no_allow_remote {
                bail!("use either --allow-remote or --no-allow-remote, not both");
            }
            if auto_approve && no_auto_approve {
                bail!("use either --auto-approve or --no-auto-approve, not both");
            }
            if enabled && disabled {
                bail!("use either --enabled or --disabled, not both");
            }
            let current = rpc(&discovery, "acp.agent.list", json!({})).await?;
            let agent_json = current
                .get("agents")
                .and_then(Value::as_array)
                .and_then(|agents| {
                    agents.iter().find(|candidate| {
                        candidate.get("id").and_then(Value::as_str) == Some(agent.as_str())
                            || candidate.get("name").and_then(Value::as_str) == Some(agent.as_str())
                    })
                })
                .cloned()
                .with_context(|| format!("ACP agent not found: {agent}"))?;
            let existing_transport = agent_json
                .get("transport")
                .and_then(Value::as_str)
                .unwrap_or("stdio");
            let transport = transport
                .map(AcpTransportArg::as_str)
                .unwrap_or(existing_transport)
                .to_string();
            let existing_args = agent_json.get("args").cloned().unwrap_or_else(|| json!([]));
            let existing_env = agent_json.get("env").cloned().unwrap_or_else(|| json!({}));
            let existing_headers = agent_json
                .get("headers")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let env = if env.is_empty() {
                existing_env
            } else {
                json!(parse_env_pairs(&env)?)
            };
            let headers = if !headers.is_empty() {
                json!(parse_pairs(&headers, "--header")?)
            } else if clear_headers {
                json!({})
            } else {
                existing_headers
            };
            let endpoint = if clear_endpoint {
                Value::Null
            } else if let Some(endpoint) = endpoint {
                json!(endpoint)
            } else {
                agent_json.get("endpoint").cloned().unwrap_or(Value::Null)
            };
            let auth_header_env = if clear_auth_header_env {
                Value::Null
            } else if let Some(name) = auth_header_env {
                json!(name)
            } else {
                agent_json
                    .get("auth_header_env")
                    .cloned()
                    .unwrap_or(Value::Null)
            };
            let allow_remote = if allow_remote {
                true
            } else if no_allow_remote {
                false
            } else {
                agent_json
                    .get("allow_remote")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            };
            let auto_approve = if auto_approve {
                true
            } else if no_auto_approve {
                false
            } else {
                agent_json
                    .get("auto_approve")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            };
            let enabled = if enabled {
                true
            } else if disabled {
                false
            } else {
                agent_json
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(true)
            };
            rpc(
                &discovery,
                "acp.agent.update",
                json!({
                    "id": agent_json.get("id").cloned().unwrap_or(Value::Null),
                    "name": name.unwrap_or_else(|| {
                        agent_json
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    }),
                    "command": command.unwrap_or_else(|| {
                        agent_json
                            .get("command")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    }),
                    "args": if args.is_empty() { existing_args } else { json!(args) },
                    "env": env,
                    "enabled": enabled,
                    "auto_approve": auto_approve,
                    "timeout_secs": timeout.unwrap_or_else(|| {
                        agent_json
                            .get("timeout_secs")
                            .and_then(Value::as_u64)
                            .unwrap_or(1800)
                    }),
                    "transport": transport,
                    "endpoint": endpoint,
                    "auth_header_env": auth_header_env,
                    "headers": headers,
                    "connect_timeout_secs": connect_timeout.unwrap_or_else(|| {
                        agent_json
                            .get("connect_timeout_secs")
                            .and_then(Value::as_u64)
                            .unwrap_or(30)
                    }),
                    "allow_remote": allow_remote
                }),
            )
            .await?
        }
        Command::AcpRemove { agent_id } => {
            rpc(
                &discovery,
                "acp.agent.remove",
                json!({"agent_id": agent_id}),
            )
            .await?
        }
        Command::AcpProbe {
            agent,
            project,
            no_session,
        } => {
            rpc(
                &discovery,
                "acp.agent.probe",
                json!({
                    "agent_id": agent,
                    "project_id": project,
                    "create_session": !no_session
                }),
            )
            .await?
        }
        _ => unreachable!("command routed to the wrong dispatch family"),
    };
    Ok(Some(value))
}

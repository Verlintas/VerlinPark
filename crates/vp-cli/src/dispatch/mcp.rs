//! Model Context Protocol server, tool, resource, and pin commands.

use super::*;

pub(super) async fn dispatch(
    command: Command,
    discovery: Discovery,
    _json: bool,
) -> Result<Option<Value>> {
    let value = match command {
        Command::McpServers => rpc(&discovery, "mcp.server.list", json!({})).await?,
        Command::McpAdd {
            name,
            command,
            transport,
            args,
            env,
            url,
            headers,
        } => {
            let env = parse_env_pairs(&env)?;
            let headers = parse_env_pairs(&headers)?;
            rpc(
                &discovery,
                "mcp.server.add",
                json!({
                    "name": name,
                    "transport": transport.as_str(),
                    "command": command.unwrap_or_default(),
                    "args": args,
                    "env": env,
                    "url": url,
                    "headers": headers,
                }),
            )
            .await?
        }
        Command::McpUpdate {
            server_id,
            name,
            transport,
            command,
            args,
            env,
            url,
            headers,
            enabled,
        } => {
            let env = parse_env_pairs(&env)?;
            let headers = parse_env_pairs(&headers)?;
            let enabled = match enabled {
                Some(enabled) => enabled,
                None => mcp_server_enabled(&discovery, &server_id).await?,
            };
            rpc(
                &discovery,
                "mcp.server.update",
                json!({
                    "id": server_id,
                    "name": name,
                    "transport": transport.as_str(),
                    "command": command.unwrap_or_default(),
                    "args": args,
                    "env": env,
                    "url": url,
                    "headers": headers,
                    "enabled": enabled,
                }),
            )
            .await?
        }
        Command::McpProbe { server_id } => {
            rpc(
                &discovery,
                "mcp.server.probe",
                json!({"server_id": server_id}),
            )
            .await?
        }
        Command::McpRemove { server_id } => {
            rpc(
                &discovery,
                "mcp.server.remove",
                json!({"server_id": server_id}),
            )
            .await?
        }
        Command::McpTools { server } => {
            rpc(&discovery, "mcp.tools", json!({"server_id": server})).await?
        }
        Command::McpCall {
            server,
            tool,
            arguments,
            session,
            agent_id,
            agent_name,
        } => {
            let arguments: serde_json::Value =
                serde_json::from_str(&arguments).context("parsing --arguments as JSON")?;
            rpc(
                &discovery,
                "mcp.call",
                json!({
                    "server_id": server,
                    "tool": tool,
                    "arguments": arguments,
                    "session_id": session,
                    "agent_id": agent_id,
                    "agent_name": agent_name,
                }),
            )
            .await?
        }
        Command::McpTasks {
            project,
            session,
            server,
            status,
            limit,
        } => {
            rpc(
                &discovery,
                "mcp.task.list",
                json!({
                    "project_id": project,
                    "session_id": session,
                    "server_id": server,
                    "status": status,
                    "limit": limit,
                }),
            )
            .await?
        }
        Command::McpTask { task_id } => {
            rpc(&discovery, "mcp.task.get", json!({"task_id": task_id})).await?
        }
        Command::McpTaskResult {
            task_id,
            no_refresh,
        } => {
            rpc(
                &discovery,
                "mcp.task.result",
                json!({"task_id": task_id, "refresh": !no_refresh}),
            )
            .await?
        }
        Command::McpTaskCancel { task_id, reason } => {
            rpc(
                &discovery,
                "mcp.task.cancel",
                json!({"task_id": task_id, "reason": reason}),
            )
            .await?
        }
        Command::McpResources { server } => {
            rpc(&discovery, "mcp.resources", json!({"server_id": server})).await?
        }
        Command::McpReadResource { server, uri } => {
            rpc(
                &discovery,
                "mcp.resource.read",
                json!({"server_id": server, "uri": uri}),
            )
            .await?
        }
        Command::McpPrompts { server } => {
            rpc(&discovery, "mcp.prompts", json!({"server_id": server})).await?
        }
        Command::McpPrompt { server, name, args } => {
            let arguments = parse_env_pairs(&args)?;
            rpc(
                &discovery,
                "mcp.prompt.get",
                json!({"server_id": server, "name": name, "arguments": arguments}),
            )
            .await?
        }
        Command::McpPins { project, server } => {
            rpc(
                &discovery,
                "project.mcp_pins",
                json!({"project_id": project, "server_id": server}),
            )
            .await?
        }
        Command::McpPinApprove {
            project,
            server,
            tool,
            schema_hash,
            description_hash,
            approved_by,
        } => {
            rpc(
                &discovery,
                "mcp.pin.approve",
                json!({
                    "project_id": project,
                    "server_id": server,
                    "tool_name": tool,
                    "expected_schema_hash": schema_hash,
                    "expected_description_hash": description_hash,
                    "approved_by": approved_by,
                }),
            )
            .await?
        }
        Command::McpPinReject {
            project,
            server,
            tool,
            reason,
        } => {
            rpc(
                &discovery,
                "mcp.pin.reject",
                json!({
                    "project_id": project,
                    "server_id": server,
                    "tool_name": tool,
                    "reason": reason,
                }),
            )
            .await?
        }
        _ => unreachable!("command routed to the wrong dispatch family"),
    };
    Ok(Some(value))
}

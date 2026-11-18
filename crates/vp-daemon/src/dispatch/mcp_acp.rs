use super::*;

pub(super) async fn handle(
    state: &AppState,
    id: Option<serde_json::Value>,
    request: RpcRequest,
) -> RpcResponse {
    match request.method.as_str() {
        "mcp.pin.approve" => match parse_params::<McpPinApproveParams>(request.params) {
            Ok(params) => match state.engine.approve_mcp_pin(&params, None).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.pin.reject" => match parse_params::<McpPinRejectParams>(request.params) {
            Ok(params) => match state.engine.reject_mcp_pin(&params, None).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.server.list" => match state.engine.list_mcp_servers().await {
            Ok(result) => RpcResponse::success(id, result),
            Err(error) => internal_error(id, error),
        },
        "mcp.server.add" => match parse_params::<AddMcpServerParams>(request.params) {
            Ok(params) => match state.engine.add_mcp_server(params.config).await {
                Ok(server) => RpcResponse::success(id, json!({"server": server})),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.server.update" => match parse_params::<UpdateMcpServerParams>(request.params) {
            Ok(params) => match state
                .engine
                .update_mcp_server(&params.id, params.config)
                .await
            {
                Ok(Some(server)) => RpcResponse::success(id, json!({"server": server})),
                Ok(None) => RpcResponse::error(
                    id,
                    RpcError::new(codes::INVALID_PARAMS, "MCP server not found"),
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.server.probe" => match parse_params::<McpProbeParams>(request.params) {
            Ok(params) => match state.engine.probe_mcp_server(&params.server_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.server.remove" => match parse_params::<McpServerIdParams>(request.params) {
            Ok(params) => match state.engine.remove_mcp_server(&params.server_id).await {
                Ok(removed) => RpcResponse::success(id, json!({"removed": removed})),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "acp.agent.list" => match state.engine.list_acp_agents().await {
            Ok(result) => RpcResponse::success(id, result),
            Err(error) => internal_error(id, error),
        },
        "acp.agent.add" => match parse_params::<AddAcpAgentParams>(request.params) {
            Ok(params) => match state
                .engine
                .add_acp_agent_config(AcpAgentConfig {
                    name: params.name,
                    command: params.command,
                    args: params.args,
                    env: params.env,
                    enabled: params.enabled,
                    auto_approve: params.auto_approve,
                    timeout_secs: params.timeout_secs,
                    transport: params.transport,
                    endpoint: params.endpoint,
                    auth_header_env: params.auth_header_env,
                    headers: params.headers,
                    connect_timeout_secs: params.connect_timeout_secs,
                    allow_remote: params.allow_remote,
                })
                .await
            {
                Ok(agent) => RpcResponse::success(id, json!({"agent": agent})),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "acp.agent.update" => match parse_params::<UpdateAcpAgentParams>(request.params) {
            Ok(params) => match state
                .engine
                .update_acp_agent_config(
                    &params.id,
                    AcpAgentConfig {
                        name: params.name,
                        command: params.command,
                        args: params.args,
                        env: params.env,
                        enabled: params.enabled,
                        auto_approve: params.auto_approve,
                        timeout_secs: params.timeout_secs,
                        transport: params.transport,
                        endpoint: params.endpoint,
                        auth_header_env: params.auth_header_env,
                        headers: params.headers,
                        connect_timeout_secs: params.connect_timeout_secs,
                        allow_remote: params.allow_remote,
                    },
                )
                .await
            {
                Ok(Some(agent)) => RpcResponse::success(id, json!({"agent": agent})),
                Ok(None) => RpcResponse::error(
                    id,
                    RpcError::new(codes::INVALID_PARAMS, "ACP agent not found".to_string()),
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "acp.agent.remove" => match parse_params::<AcpAgentIdParams>(request.params) {
            Ok(params) => match state.engine.remove_acp_agent(&params.agent_id).await {
                Ok(removed) => RpcResponse::success(id, json!({"removed": removed})),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "acp.agent.probe" => match parse_params::<AcpProbeParams>(request.params) {
            Ok(params) => match state
                .engine
                .probe_acp_agent_options(&params.agent_id, params.project_id, params.create_session)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.tools" => match parse_params::<McpServerIdParams>(request.params) {
            Ok(params) => match state.engine.mcp_tools(&params.server_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.call" => match parse_params::<McpCallParams>(request.params) {
            Ok(params) => match state.engine.mcp_call_for_params(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.task.list" => {
            match parse_params::<vp_protocol::types::McpTaskListParams>(request.params) {
                Ok(params) => match state.engine.mcp_task_list(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "mcp.task.get" => match parse_params::<vp_protocol::types::McpTaskParams>(request.params) {
            Ok(params) => match state.engine.mcp_task_get(&params.task_id, true).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.task.result" => {
            match parse_params::<vp_protocol::types::McpTaskResultParams>(request.params) {
                Ok(params) => match state.engine.mcp_task_result(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "mcp.task.cancel" => {
            match parse_params::<vp_protocol::types::McpTaskCancelParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .mcp_task_cancel(&params.task_id, params.reason.as_deref())
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "mcp.resources" => match parse_params::<McpServerIdParams>(request.params) {
            Ok(params) => match state.engine.mcp_resources(&params.server_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.resource.read" => match parse_params::<McpReadResourceParams>(request.params) {
            Ok(params) => match state
                .engine
                .mcp_read_resource(&params.server_id, &params.uri)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.prompts" => match parse_params::<McpServerIdParams>(request.params) {
            Ok(params) => match state.engine.mcp_prompts(&params.server_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mcp.prompt.get" => match parse_params::<McpGetPromptParams>(request.params) {
            Ok(params) => match state
                .engine
                .mcp_prompt(&params.server_id, &params.name, params.arguments)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        _ => RpcResponse::error(
            id,
            RpcError::new(codes::METHOD_NOT_FOUND, "method not found"),
        ),
    }
}

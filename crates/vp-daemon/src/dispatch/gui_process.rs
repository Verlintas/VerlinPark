use super::*;

pub(super) async fn handle(
    state: &AppState,
    id: Option<serde_json::Value>,
    request: RpcRequest,
) -> RpcResponse {
    match request.method.as_str() {
        "computer.leases" => match state.engine.list_computer_leases().await {
            Ok(leases) => RpcResponse::success(
                id,
                ComputerLeaseListResult {
                    runtime_id: state.engine.runtime_id().to_string(),
                    leases: leases
                        .into_iter()
                        .map(|lease| ComputerLeaseInfo {
                            lease_id: lease.lease_id,
                            surface: lease.surface,
                            owner_id: lease.owner_id,
                            session_id: lease.session_id,
                            agent_id: lease.agent_id,
                            agent_name: lease.agent_name,
                            acquired_at: lease.acquired_at,
                            heartbeat_at: lease.heartbeat_at,
                            expires_at: lease.expires_at,
                            generation: lease.generation,
                        })
                        .collect(),
                },
            ),
            Err(error) => internal_error(id, error),
        },
        "computer.leases.sweep" => match state.engine.sweep_computer_leases().await {
            Ok(removed) => RpcResponse::success(id, ComputerLeaseSweepResult { removed }),
            Err(error) => internal_error(id, error),
        },
        "computer.leases.release" => {
            match parse_params::<ComputerLeaseReleaseParams>(request.params) {
                Ok(params) => match state.engine.release_computer_lease(&params.lease_id).await {
                    Ok(released) => {
                        RpcResponse::success(id, ComputerLeaseReleaseResult { released })
                    }
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "terminal.open" => match parse_params::<TerminalOpenParams>(request.params) {
            Ok(params) => match state
                .engine
                .terminal_open(
                    &params.session_id,
                    params.cols.unwrap_or(80),
                    params.rows.unwrap_or(24),
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "terminal.write" => match parse_params::<TerminalWriteParams>(request.params) {
            Ok(params) => match state
                .engine
                .terminal_write(&params.terminal_id, &params.data)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "terminal.read" => match parse_params::<TerminalReadParams>(request.params) {
            Ok(params) => match state
                .engine
                .terminal_read(&params.terminal_id, params.after_seq)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "terminal.resize" => match parse_params::<TerminalResizeParams>(request.params) {
            Ok(params) => match state
                .engine
                .terminal_resize(&params.terminal_id, params.cols, params.rows)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "terminal.close" => match parse_params::<TerminalCloseParams>(request.params) {
            Ok(params) => match state.engine.terminal_close(&params.terminal_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "console.open" => match parse_params::<ConsoleOpenParams>(request.params) {
            Ok(params) => match state.engine.console_open(&params.session_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "console.write" => match parse_params::<ConsoleWriteParams>(request.params) {
            Ok(params) => match state
                .engine
                .console_write(&params.console_id, &params.data)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "console.read" => match parse_params::<ConsoleReadParams>(request.params) {
            Ok(params) => match state
                .engine
                .console_read(&params.console_id, params.after_seq)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "console.signal" => match parse_params::<ConsoleSignalParams>(request.params) {
            Ok(params) => match state
                .engine
                .console_signal(&params.console_id, &params.signal)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "console.close" => match parse_params::<ConsoleCloseParams>(request.params) {
            Ok(params) => match state.engine.console_close(&params.console_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "process.policy.get" => match parse_params::<ProcessPolicyParams>(request.params) {
            Ok(params) => match state.engine.get_process_policy(&params.project_id).await {
                Ok(policy) => RpcResponse::success(
                    id,
                    ProcessPolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "process.policy.set" => match parse_params::<SetProcessPolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_process_policy(&params.project_id, params.policy)
                .await
            {
                Ok(policy) => RpcResponse::success(
                    id,
                    ProcessPolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "computer.status" => match parse_params::<ComputerStatusParams>(request.params) {
            Ok(params) => RpcResponse::success(
                id,
                state
                    .engine
                    .computer_status_for(params.session_id.as_deref(), params.refresh)
                    .await,
            ),
            Err(response) => response_with_id(id, response),
        },
        "gui.status" => match parse_params::<vp_protocol::types::GuiStatusParams>(request.params) {
            Ok(params) => match state.engine.gui_status(params.project_id.as_deref()).await {
                Ok(status) => RpcResponse::success(id, status),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "gui.timeline.audit" => match parse_params::<GuiTimelineAuditParams>(request.params) {
            Ok(params) => match state
                .engine
                .gui_timeline_audit(&params.project_id, params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "gui.timeline.retention" => {
            match parse_params::<GuiTimelineRetentionParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .gui_timeline_retention(&params.project_id, params.older_than_days)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "gui.timeline.prune" => match parse_params::<GuiTimelinePruneParams>(request.params) {
            Ok(params) if !params.dry_run.unwrap_or(false) && !params.confirm => {
                RpcResponse::error(
                    id,
                    vp_protocol::rpc::RpcError::new(
                        vp_protocol::rpc::codes::INVALID_PARAMS,
                        "gui.timeline.prune requires confirm=true for a non-dry-run prune",
                    ),
                )
            }
            Ok(params) => match state
                .engine
                .prune_gui_timeline(
                    &params.project_id,
                    params.older_than_days,
                    params.dry_run.unwrap_or(false),
                    params.limit,
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "gui.timeline" => match parse_params::<GuiTimelineParams>(request.params) {
            Ok(params) => {
                let limit = params.limit.unwrap_or(100).clamp(1, 500);
                match state
                    .engine
                    .gui_timeline(params.session_id.as_deref(), limit)
                    .await
                {
                    Ok(actions) => RpcResponse::success(id, GuiTimelineResult { actions, limit }),
                    Err(error) => internal_error(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "gui.policy.get" => match parse_params::<GuiPolicyParams>(request.params) {
            Ok(params) => match state.engine.get_gui_policy(&params.project_id).await {
                Ok(policy) => RpcResponse::success(
                    id,
                    GuiPolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "gui.policy.set" => match parse_params::<SetGuiPolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_gui_policy(&params.project_id, params.policy)
                .await
            {
                Ok(policy) => RpcResponse::success(
                    id,
                    GuiPolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
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

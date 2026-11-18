use super::*;

pub(super) async fn handle(
    state: &AppState,
    id: Option<serde_json::Value>,
    request: RpcRequest,
) -> RpcResponse {
    match request.method.as_str() {
        "git.log" => match parse_params::<GitLogParams>(request.params) {
            Ok(params) => match state
                .engine
                .git_log(&params.session_id, params.limit.unwrap_or(50))
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "git.show" => match parse_params::<GitShowParams>(request.params) {
            Ok(params) => match state
                .engine
                .git_show(&params.session_id, &params.commit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "update.download" => match parse_params::<UpdateDownloadParams>(request.params) {
            Ok(params) => match vp_core::update::download_update(params.repo.as_deref()).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "update.check" => match parse_params::<UpdateCheckParams>(request.params) {
            Ok(params) => match vp_core::update::check_for_update(params.repo.as_deref()).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "backup.status" => {
            match parse_params::<vp_protocol::types::BackupStatusParams>(request.params) {
                Ok(_) => match backup_status(&state.data_dir) {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "merge_queue.list" => match parse_params::<MergeQueueListParams>(request.params) {
            Ok(params) => match state.engine.list_merge_queue(&params.session_id).await {
                Ok(entries) => RpcResponse::success(id, MergeQueueListResult { entries }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "merge_queue.retry" => match parse_params::<RetryMergeParams>(request.params) {
            Ok(params) => match state.agent.retry_merge(&params.entry_id).await {
                Ok(entry) => RpcResponse::success(id, RetryMergeResult { entry }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "merge_queue.resolve" => match parse_params::<ResolveMergeParams>(request.params) {
            Ok(params) => match state.agent.resolve_merge(&params.entry_id).await {
                Ok(entry) => RpcResponse::success(id, ResolveMergeResult { entry }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "forge.status" => match parse_params::<ForgeStatusParams>(request.params) {
            Ok(params) => match state.engine.forge_status(params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "forge.preflight" => match parse_params::<ForgePreflightParams>(request.params) {
            Ok(params) => match state
                .engine
                .forge_preflight(&params.project_id, params.provider.as_deref())
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => forge_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        _ => RpcResponse::error(
            id,
            RpcError::new(codes::METHOD_NOT_FOUND, "method not found"),
        ),
    }
}

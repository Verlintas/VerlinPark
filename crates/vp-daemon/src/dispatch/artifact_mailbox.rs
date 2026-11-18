use super::*;

pub(super) async fn handle(
    state: &AppState,
    id: Option<serde_json::Value>,
    request: RpcRequest,
) -> RpcResponse {
    match request.method.as_str() {
        "artifact.diff" => match parse_params::<ArtifactDiffParams>(request.params) {
            Ok(params) => match state.engine.artifact_diff(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "artifact.versions" => match parse_params::<ArtifactVersionsParams>(request.params) {
            Ok(params) => match state.engine.artifact_versions(&params.artifact_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "artifact.restore" => match parse_params::<ArtifactRestoreParams>(request.params) {
            Ok(params) => match state
                .engine
                .restore_artifact_version(&params.artifact_id, params.version)
                .await
            {
                Ok(artifact) => {
                    let provenance = artifact.provenance.clone();
                    RpcResponse::success(
                        id,
                        ArtifactResult {
                            artifact,
                            provenance,
                        },
                    )
                }
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mailbox.list" => match parse_params::<MailboxListParams>(request.params) {
            Ok(params) => match state
                .engine
                .list_agent_messages(&params.session_id, params.agent_id, params.unread_only)
                .await
            {
                Ok(messages) => RpcResponse::success(id, MailboxListResult { messages }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mailbox.read" => match parse_params::<MarkMessageReadParams>(request.params) {
            Ok(params) => match state
                .engine
                .mark_agent_message_read(&params.message_id)
                .await
            {
                Ok(Some(message)) => RpcResponse::success(id, AgentMessageResult { message }),
                Ok(None) => RpcResponse::error(
                    id,
                    RpcError::new(codes::INVALID_PARAMS, "message not found".to_string()),
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "artifact.list" => match parse_params::<ArtifactListParams>(request.params) {
            Ok(params) => match state.engine.list_artifacts(&params.session_id).await {
                Ok(artifacts) => RpcResponse::success(id, ArtifactListResult { artifacts }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mailbox.policy.get" => match parse_params::<MailboxPolicyParams>(request.params) {
            Ok(params) => match state.engine.get_mailbox_policy(&params.project_id).await {
                Ok(policy) => RpcResponse::success(
                    id,
                    MailboxPolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mailbox.policy.set" => match parse_params::<SetMailboxPolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_mailbox_policy(&params.project_id, params.policy)
                .await
            {
                Ok(policy) => RpcResponse::success(
                    id,
                    MailboxPolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "mailbox.gc" => match parse_params::<MailboxGcParams>(request.params) {
            Ok(params) => {
                let policy = match state.engine.get_mailbox_policy(&params.project_id).await {
                    Ok(policy) => policy,
                    Err(error) => return internal_error(id, error),
                };
                match state.engine.gc_mailbox(&params.project_id, &policy).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "artifact.backfill" => match parse_params::<ArtifactBackfillParams>(request.params) {
            Ok(params) => {
                match state
                    .engine
                    .backfill_artifact_events(&params.session_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "artifact.materialize" => match parse_params::<ArtifactMaterializeParams>(request.params) {
            Ok(params) => match state.engine.materialize_artifact(&params.artifact_id).await {
                Ok(Some(materialized)) => RpcResponse::success(id, materialized),
                Ok(None) => RpcResponse::error(
                    id,
                    RpcError::new(codes::INVALID_PARAMS, "artifact is not a file artifact"),
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "tool_output.policy.get" => match parse_params::<ToolOutputPolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .get_tool_output_policy(&params.project_id)
                .await
            {
                Ok(policy) => RpcResponse::success(
                    id,
                    ToolOutputPolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "tool_output.policy.set" => {
            match parse_params::<SetToolOutputPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_tool_output_policy(&params.project_id, params.policy)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "tool_output.status" => match parse_params::<ToolOutputStatusParams>(request.params) {
            Ok(params) => match state.engine.tool_output_status(params).await {
                Ok(status) => RpcResponse::success(id, status),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "tool_output.list" => match parse_params::<ToolOutputListParams>(request.params) {
            Ok(params) => match state.engine.list_tool_outputs(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "tool_output.prune" => match parse_params::<ToolOutputPruneParams>(request.params) {
            Ok(params) => match state.engine.prune_tool_outputs(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "tool_output.audit" => match parse_params::<ToolOutputAuditParams>(request.params) {
            Ok(params) => match state.engine.audit_tool_outputs(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "artifact.policy.get" => match parse_params::<ArtifactPolicyParams>(request.params) {
            Ok(params) => match state.engine.get_artifact_policy(&params.project_id).await {
                Ok(policy) => RpcResponse::success(
                    id,
                    ArtifactPolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "artifact.policy.set" => match parse_params::<SetArtifactPolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_artifact_policy(&params.project_id, params.policy)
                .await
            {
                Ok(policy) => RpcResponse::success(
                    id,
                    ArtifactPolicyResult {
                        project_id: params.project_id,
                        policy,
                    },
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "artifact.gc" => match parse_params::<ArtifactGcParams>(request.params) {
            Ok(params) => {
                let policy = match state.engine.get_artifact_policy(&params.project_id).await {
                    Ok(policy) => policy,
                    Err(error) => return internal_error(id, error),
                };
                match state.engine.gc_artifacts(&params.project_id, &policy).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "artifact.get" => match parse_params::<ArtifactGetParams>(request.params) {
            Ok(params) => match state.engine.get_artifact(&params.artifact_id).await {
                Ok(Some(artifact)) => {
                    let provenance = artifact.provenance.clone();
                    RpcResponse::success(
                        id,
                        ArtifactResult {
                            artifact,
                            provenance,
                        },
                    )
                }
                Ok(None) => RpcResponse::error(
                    id,
                    RpcError::new(codes::INVALID_PARAMS, "artifact not found".to_string()),
                ),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "artifact.archive.list" => {
            match parse_params::<ArtifactArchiveListParams>(request.params) {
                Ok(params) => match state.engine.artifact_archive_list(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => archive_media_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "artifact.archive.extract" => {
            match parse_params::<ArtifactArchiveExtractParams>(request.params) {
                Ok(params) => match state.engine.artifact_archive_extract(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => archive_media_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "media.info" => match parse_params::<MediaInfoParams>(request.params) {
            Ok(params) => match state.engine.media_info(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => archive_media_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "media.transcode" => match parse_params::<MediaTranscodeParams>(request.params) {
            Ok(params) => match state.engine.media_transcode(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => archive_media_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        _ => RpcResponse::error(
            id,
            RpcError::new(codes::METHOD_NOT_FOUND, "method not found"),
        ),
    }
}

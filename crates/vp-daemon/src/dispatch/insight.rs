//! Resource, Run Contract, delivery, rescue, allocation, and trade-off routes.
//!
//! These read-mostly insight surfaces share the deterministic governor inputs.
//! They are split from the main session router to keep every dispatch module
//! under the repository line cap; auth, params, and response shapes are
//! unchanged.

use super::*;

/// Partial-delivery failures carry a stable `DELIVERY_*` code in `data.code`
/// and map to `INVALID_PARAMS`; unexpected store/internal failures keep the
/// ordinary `INTERNAL_ERROR`.
fn partial_delivery_error(id: Option<serde_json::Value>, error: anyhow::Error) -> RpcResponse {
    if let Some(typed) = error.downcast_ref::<vp_core::PartialDeliveryError>() {
        return RpcResponse::error(
            id,
            RpcError::with_data(
                codes::INVALID_PARAMS,
                typed.message.clone(),
                json!({"code": typed.code}),
            ),
        );
    }
    internal_error(id, error)
}

/// Rescue failures carry a stable `RESCUE_*` code in `data.code` and map to
/// `INVALID_PARAMS`; unexpected store/internal failures keep `INTERNAL_ERROR`.
fn rescue_error(id: Option<serde_json::Value>, error: anyhow::Error) -> RpcResponse {
    if let Some(typed) = error.downcast_ref::<vp_core::RescueError>() {
        return RpcResponse::error(
            id,
            RpcError::with_data(
                codes::INVALID_PARAMS,
                typed.message.clone(),
                json!({"code": typed.code}),
            ),
        );
    }
    internal_error(id, error)
}

pub(super) async fn handle(
    state: &AppState,
    id: Option<serde_json::Value>,
    request: RpcRequest,
) -> RpcResponse {
    match request.method.as_str() {
        "session.budget.governor" => match parse_params::<GovernorStatusParams>(request.params) {
            Ok(params) => match state
                .engine
                .budget_governor_status(&params.session_id, params.limit.unwrap_or(20))
                .await
            {
                Ok(status) => RpcResponse::success(id, status),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "session.contract" => match parse_params::<SessionContractParams>(request.params) {
            Ok(params) => {
                let task = params.task_id.as_deref();
                match state
                    .engine
                    .session_contract_status(&params.session_id, task)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                }
            }
            Err(response) => response_with_id(id, response),
        },
        "session.resources" => {
            match parse_params::<vp_protocol::types::SessionResourcesParams>(request.params) {
                Ok(params) => match state.engine.session_resources(&params.session_id).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.readiness" => {
            match parse_params::<vp_protocol::types::SessionReadinessParams>(request.params) {
                Ok(params) => match state.engine.session_readiness(&params.session_id).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.delivery" => {
            match parse_params::<vp_protocol::types::SessionDeliveryParams>(request.params) {
                Ok(params) => match state.engine.session_delivery(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.delivery.partial" => {
            match parse_params::<vp_protocol::types::PartialDeliveryParams>(request.params) {
                Ok(params) => match state.engine.record_partial_delivery(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => partial_delivery_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.allocation" => {
            match parse_params::<vp_protocol::types::SessionAllocationParams>(request.params) {
                Ok(params) => match state.engine.session_allocation(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.tradeoffs" => {
            match parse_params::<vp_protocol::types::SessionTradeoffsParams>(request.params) {
                Ok(params) => match state.engine.session_tradeoffs(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.envelope.persist" => {
            match parse_params::<vp_protocol::types::PersistEnvelopeParams>(request.params) {
                Ok(params) if !params.confirm => RpcResponse::error(
                    id,
                    RpcError::with_data(
                        codes::INVALID_PARAMS,
                        "persisting a runtime envelope requires explicit confirmation",
                        json!({"code": "ENVELOPE_CONFIRM_REQUIRED"}),
                    ),
                ),
                Ok(params) => match state
                    .agent
                    .persist_recorded_envelope(
                        &params.session_id,
                        &params.task_id,
                        params.expected_revision,
                    )
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.rescue.status" => {
            match parse_params::<vp_protocol::types::RescueStatusParams>(request.params) {
                Ok(params) => match state.engine.rescue_status(&params.session_id).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.rescue" => {
            match parse_params::<vp_protocol::types::RescueParams>(request.params) {
                Ok(params) => match state.engine.grant_rescue(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => rescue_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.run_contract.get" => {
            match parse_params::<vp_protocol::types::SessionRunContractParams>(request.params) {
                Ok(params) => match state.engine.run_contract_view(&params.session_id).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.run_contract.set" => {
            match parse_params::<vp_protocol::types::SetRunContractParams>(request.params) {
                Ok(params) => match state.engine.set_run_contract(params).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "session.resources.top_up" => {
            match parse_params::<vp_protocol::types::TopUpSessionResourcesParams>(request.params) {
                Ok(params) => {
                    let session_id = params.session_id.clone();
                    let resume = params.resume;
                    match state.engine.top_up_session_budget(params).await {
                        Ok((budget, capped)) => {
                            let resumed = if resume {
                                match state.agent.resume(&session_id).await {
                                    Ok(resumed) => resumed,
                                    Err(error) => return internal_error(id, error),
                                }
                            } else {
                                false
                            };
                            match state.engine.session_resources(&session_id).await {
                                Ok(resources) => RpcResponse::success(
                                    id,
                                    vp_protocol::types::TopUpSessionResourcesResult {
                                        session_id,
                                        budget,
                                        resumed,
                                        capped,
                                        resources,
                                    },
                                ),
                                Err(error) => internal_error(id, error),
                            }
                        }
                        Err(error) => internal_error(id, error),
                    }
                }
                Err(response) => response_with_id(id, response),
            }
        }
        _ => RpcResponse::error(
            id,
            RpcError::new(codes::METHOD_NOT_FOUND, "method not found"),
        ),
    }
}

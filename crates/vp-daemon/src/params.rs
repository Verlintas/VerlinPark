use serde_json::Value;
use vp_protocol::rpc::{RpcError, RpcResponse, codes};

pub(crate) fn parse_params<T: serde::de::DeserializeOwned>(
    params: Value,
) -> std::result::Result<T, RpcError> {
    serde_json::from_value(params)
        .map_err(|error| RpcError::new(codes::INVALID_PARAMS, error.to_string()))
}

pub(crate) fn response_with_id(id: Option<Value>, error: RpcError) -> RpcResponse {
    RpcResponse::error(id, error)
}

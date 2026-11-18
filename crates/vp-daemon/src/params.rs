use serde_json::{Value, json};
use vp_protocol::rpc::{RpcError, RpcResponse, codes};

/// Decode JSON-RPC params. JSON-RPC 2.0 makes `params` optional, so an omitted
/// (or explicit `null`) value is normalized to `{}` before typed decoding. A
/// malformed object, array, or scalar is still rejected with `INVALID_PARAMS`.
pub(crate) fn parse_params<T: serde::de::DeserializeOwned>(
    params: Value,
) -> std::result::Result<T, RpcError> {
    let params = if params.is_null() { json!({}) } else { params };
    serde_json::from_value(params)
        .map_err(|error| RpcError::new(codes::INVALID_PARAMS, error.to_string()))
}

pub(crate) fn response_with_id(id: Option<Value>, error: RpcError) -> RpcResponse {
    RpcResponse::error(id, error)
}

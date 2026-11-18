use serde_json::{Value, json};
use vp_protocol::rpc::{RpcError, RpcResponse, codes};

pub(crate) fn internal_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    RpcResponse::error(id, RpcError::new(codes::INTERNAL_ERROR, error.to_string()))
}

/// Map the stable identity error taxonomy to a bounded RPC failure. Message
/// text never changes the classification; only the typed code does.
pub(crate) fn identity_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    let typed = error.downcast_ref::<vp_protocol::types::AgentIdentityError>();
    let Some(typed) = typed else {
        return RpcResponse::error(
            id,
            RpcError::new(codes::INTERNAL_ERROR, format!("{error:#}")),
        );
    };
    let rpc_code = match typed.code {
        vp_protocol::types::agent_identity_codes::IDENTITY_NOT_FOUND
        | vp_protocol::types::agent_identity_codes::HIRE_NOT_FOUND
        | vp_protocol::types::agent_identity_codes::RETIREMENT_NOT_FOUND
        | vp_protocol::types::agent_identity_codes::ORG_PROJECT_NOT_FOUND
        | vp_protocol::types::team_template_codes::TEMPLATE_NOT_FOUND
        | vp_protocol::types::team_template_codes::TEMPLATE_PROJECT_NOT_FOUND => codes::NOT_FOUND,
        vp_protocol::types::agent_identity_codes::IDENTITY_BUSY
        | vp_protocol::types::agent_identity_codes::IDENTITY_RETIRED
        | vp_protocol::types::agent_identity_codes::IDENTITY_CONFLICT
        | vp_protocol::types::agent_identity_codes::HIRE_CONFLICT
        | vp_protocol::types::agent_identity_codes::HIRE_ALREADY_DECIDED
        | vp_protocol::types::agent_identity_codes::PROMOTION_CONFLICT
        | vp_protocol::types::agent_identity_codes::TRANSFER_CONFLICT
        | vp_protocol::types::agent_identity_codes::RETIREMENT_CONFLICT
        | vp_protocol::types::agent_identity_codes::RETIREMENT_COOLDOWN
        | vp_protocol::types::agent_identity_codes::RETIREMENT_NOT_QUIESCENT
        | vp_protocol::types::agent_identity_codes::RETIREMENT_NOT_ELIGIBLE
        | vp_protocol::types::team_template_codes::TEMPLATE_CONFLICT
        | vp_protocol::types::team_template_codes::TEMPLATE_APPLY_CONFLICT => codes::CONFLICT,
        _ => codes::INVALID_PARAMS,
    };
    RpcResponse::error(
        id,
        RpcError::with_data(rpc_code, typed.message.clone(), json!({"code": typed.code})),
    )
}

/// Map the stable agent-memory taxonomy to a bounded RPC failure. Message text
/// never changes the classification; only the typed code does.
pub(crate) fn fanout_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    if let Some(typed) = error.downcast_ref::<vp_core::fanout::FanoutError>() {
        let rpc_code = match typed.code {
            vp_core::fanout::codes::FANOUT_NOT_FOUND | vp_core::fanout::codes::FANOUT_NO_PLAN => {
                codes::NOT_FOUND
            }
            vp_core::fanout::codes::FANOUT_APPROVAL_REQUIRED
            | vp_core::fanout::codes::FANOUT_INVALID
            | vp_core::fanout::codes::FANOUT_CAP
            | vp_core::fanout::codes::FANOUT_SCOPE_OVERLAP
            | vp_core::fanout::codes::FANOUT_RECURSIVE
            | vp_core::fanout::codes::FANOUT_PROTECTED
            | vp_core::fanout::codes::FANOUT_ARTIFACT_INVALID => codes::INVALID_PARAMS,
            _ => codes::CONFLICT,
        };
        return RpcResponse::error(
            id,
            RpcError::with_data(rpc_code, typed.message.clone(), json!({"code": typed.code})),
        );
    }
    internal_error(id, error)
}

pub(crate) fn gate_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    if let Some(typed) = error.downcast_ref::<vp_core::gate::GateError>() {
        let rpc_code = match typed.code {
            vp_core::gate::codes::GATE_NOT_FOUND => codes::NOT_FOUND,
            vp_core::gate::codes::GATE_CONFLICT
            | vp_core::gate::codes::GATE_TOKEN_INVALID
            | vp_core::gate::codes::GATE_EXPIRED
            | vp_core::gate::codes::GATE_CANCELLED
            | vp_core::gate::codes::GATE_ALREADY_WATCHED
            | vp_core::gate::codes::GATE_ARCHIVED => codes::CONFLICT,
            _ => codes::INVALID_PARAMS,
        };
        return RpcResponse::error(
            id,
            RpcError::with_data(rpc_code, typed.message.clone(), json!({"code": typed.code})),
        );
    }
    internal_error(id, error)
}

pub(crate) fn effect_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    if let Some(blocked) = error.downcast_ref::<vp_core::effect::EffectBlocked>() {
        return RpcResponse::error(
            id,
            RpcError::with_data(
                codes::CONFLICT,
                blocked.message.clone(),
                json!({
                    "code": "EFFECT_UNKNOWN",
                    "session_id": blocked.session_id,
                    "task_id": blocked.task_id,
                    "receipt_ids": blocked.receipt_ids,
                }),
            ),
        );
    }
    internal_error(id, error)
}

pub(crate) fn memory_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    if let Some(typed) = error.downcast_ref::<vp_protocol::types::AgentMemoryError>() {
        let rpc_code = match typed.code {
            vp_protocol::types::agent_memory_codes::MEMORY_NOT_FOUND => codes::NOT_FOUND,
            vp_protocol::types::agent_memory_codes::MEMORY_CONFLICT
            | vp_protocol::types::agent_memory_codes::MEMORY_IMPORTED_READONLY
            | vp_protocol::types::agent_memory_codes::MEMORY_IDENTITY_RETIRED => codes::CONFLICT,
            _ => codes::INVALID_PARAMS,
        };
        return RpcResponse::error(
            id,
            RpcError::with_data(rpc_code, typed.message.clone(), json!({"code": typed.code})),
        );
    }
    RpcResponse::error(
        id,
        RpcError::new(codes::INTERNAL_ERROR, format!("{error:#}")),
    )
}

/// Map the stable workspace taxonomy to a bounded RPC failure. The typed code is
/// carried in `data.code`; message text is never used for classification.
pub(crate) fn workspace_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    if let Some(typed) = error.downcast_ref::<vp_protocol::types::AgentWorkspaceError>() {
        let rpc_code = match typed.code {
            vp_protocol::types::agent_workspace_codes::WORKSPACE_NOT_FOUND => codes::NOT_FOUND,
            vp_protocol::types::agent_workspace_codes::WORKSPACE_BUSY
            | vp_protocol::types::agent_workspace_codes::WORKSPACE_CONFLICT
            | vp_protocol::types::agent_workspace_codes::WORKSPACE_NOT_ACTIVE
            | vp_protocol::types::agent_workspace_codes::WORKSPACE_NOT_QUIESCENT
            | vp_protocol::types::agent_workspace_codes::WORKSPACE_NOT_ACCEPTED => codes::CONFLICT,
            _ => codes::INVALID_PARAMS,
        };
        return RpcResponse::error(
            id,
            RpcError::with_data(rpc_code, typed.message.clone(), json!({"code": typed.code})),
        );
    }
    if let Some(typed) = error.downcast_ref::<vp_protocol::types::AgentIdentityError>() {
        let rpc_code = match typed.code {
            vp_protocol::types::agent_identity_codes::IDENTITY_NOT_FOUND => codes::NOT_FOUND,
            _ => codes::INVALID_PARAMS,
        };
        return RpcResponse::error(
            id,
            RpcError::with_data(rpc_code, typed.message.clone(), json!({"code": typed.code})),
        );
    }
    RpcResponse::error(
        id,
        RpcError::new(codes::INTERNAL_ERROR, format!("{error:#}")),
    )
}

/// Map the stable assignment/inbox taxonomy to a bounded RPC failure. It also
/// accepts the S1 identity taxonomy so a missing identity or read-only import
/// keeps its existing wire code.
pub(crate) fn coordination_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    if let Some(typed) = error.downcast_ref::<vp_protocol::types::AgentCoordinationError>() {
        let rpc_code = match typed.code {
            vp_protocol::types::agent_coordination_codes::ASSIGNMENT_NOT_FOUND
            | vp_protocol::types::agent_coordination_codes::INBOX_NOT_FOUND => codes::NOT_FOUND,
            _ => codes::INVALID_PARAMS,
        };
        return RpcResponse::error(
            id,
            RpcError::with_data(rpc_code, typed.message.clone(), json!({"code": typed.code})),
        );
    }
    if let Some(typed) = error.downcast_ref::<vp_protocol::types::AgentIdentityError>() {
        let rpc_code = match typed.code {
            vp_protocol::types::agent_identity_codes::IDENTITY_NOT_FOUND => codes::NOT_FOUND,
            _ => codes::INVALID_PARAMS,
        };
        return RpcResponse::error(
            id,
            RpcError::with_data(rpc_code, typed.message.clone(), json!({"code": typed.code})),
        );
    }
    RpcResponse::error(
        id,
        RpcError::new(codes::INTERNAL_ERROR, format!("{error:#}")),
    )
}

/// Runtime pin/migration failures carry a stable Q3 code, retryability, hint,
/// and the bounded compatibility report. The message already includes
/// `error[CODE]`.
pub(crate) fn runtime_pin_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    if let Some(pin) = error.downcast_ref::<vp_core::runtime_pin::RuntimePinError>() {
        return RpcResponse::error(
            id,
            RpcError::with_data(codes::INTERNAL_ERROR, pin.to_string(), pin.data()),
        );
    }
    internal_error(id, error)
}

/// Diagnosis failures use the Q3 `{code,message,retryable,hint}` shape. They
/// never include prompt text, tool output, command text, or secrets.
pub(crate) fn diagnosis_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    if let Some(diagnosis) = error.downcast_ref::<vp_core::diagnosis::DiagnosisError>() {
        let rpc_code = match diagnosis.code {
            vp_core::diagnosis::codes::DIAGNOSIS_SESSION_NOT_FOUND => codes::NOT_FOUND,
            _ => codes::INVALID_PARAMS,
        };
        return RpcResponse::error(
            id,
            RpcError::with_data(rpc_code, diagnosis.to_string(), diagnosis.data()),
        );
    }
    internal_error(id, error)
}

/// OTel export failures carry a stable `OTEL_*` code and bounded data. They
/// never include a collector credential or session content.
pub(crate) fn otel_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    if let Some(otel) = error.downcast_ref::<vp_core::otel::OtelError>() {
        return RpcResponse::error(
            id,
            RpcError::with_data(codes::INTERNAL_ERROR, otel.to_string(), otel.data()),
        );
    }
    internal_error(id, error)
}

/// Forge failures carry the Q3 `FORGE_*` code, retryability, hint, and safe
/// evidence in RPC error data. The message already starts with `error[CODE]`.
pub(crate) fn forge_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    if let Some(forge) = error.downcast_ref::<vp_core::forge::ForgeError>() {
        return RpcResponse::error(
            id,
            RpcError::with_data(codes::INTERNAL_ERROR, forge.to_string(), forge.data()),
        );
    }
    internal_error(id, error)
}

/// Retrieval policy validation uses the Q3 error catalog. The numeric fields
/// are clamped by the engine, so a failure here means a manifest or hand-edited
/// value is outside the supported range.
pub(crate) fn retrieval_policy_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    let code = vp_core::tooling::errors::ToolErrorCode::SOURCE_RETRIEVAL_POLICY_INVALID;
    RpcResponse::error(
        id,
        RpcError::with_data(
            codes::INVALID_PARAMS,
            format!("error[{}] {}", code.as_str(), error),
            serde_json::json!({
                "code": code.as_str(),
                "retryable": code.default_retryable(),
                "hint": code.default_hint(),
            }),
        ),
    )
}

/// A derived-index rebuild failure is retryable but never silently swallowed.
pub(crate) fn source_index_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    let code = vp_core::tooling::errors::ToolErrorCode::SOURCE_INDEX_FAILED;
    RpcResponse::error(
        id,
        RpcError::with_data(
            codes::INTERNAL_ERROR,
            format!("error[{}] {}", code.as_str(), error),
            serde_json::json!({
                "code": code.as_str(),
                "retryable": code.default_retryable(),
                "hint": code.default_hint(),
            }),
        ),
    )
}

/// Approval rule failures are typed with the Q3 catalog. Invalid template or
/// policy input is a client error; everything else stays an internal error.
pub(crate) fn approval_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    if let Some(rule) = error.downcast_ref::<vp_core::approvals::ApprovalRuleError>() {
        return RpcResponse::error(
            id,
            RpcError::with_data(codes::INVALID_PARAMS, rule.to_string(), rule.data()),
        );
    }
    RpcResponse::error(id, RpcError::new(codes::INVALID_PARAMS, error.to_string()))
}

/// Remote integration failures carry the Q3 tool-error shape in `data` so the
/// CLI and Swift can show a stable code, retryability, and hint. Other errors
/// remain ordinary internal errors and never invent a code.
pub(crate) fn remote_bootstrap_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    match vp_core::remote_bootstrap::failure_data(&error) {
        Some(data) => RpcResponse::error(
            id,
            RpcError::with_data(codes::INTERNAL_ERROR, error.to_string(), data),
        ),
        None => internal_error(id, error),
    }
}

pub(crate) fn remote_test_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    match vp_core::remote_test::failure_data(&error) {
        Some(data) => RpcResponse::error(
            id,
            RpcError::with_data(codes::INTERNAL_ERROR, error.to_string(), data),
        ),
        None => internal_error(id, error),
    }
}

pub(crate) fn remote_integration_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    match vp_core::remote_integration::failure_data(&error) {
        Some(data) => RpcResponse::error(
            id,
            RpcError::with_data(codes::INTERNAL_ERROR, error.to_string(), data),
        ),
        None => internal_error(id, error),
    }
}

/// Archive/media failures are typed with the Q3 catalog. Request-shaped
/// failures use INVALID_PARAMS; environment failures stay INTERNAL_ERROR. The
/// `data` object carries the same bounded code/retryable/hint contract used by
/// agent tools, so the Swift client can show the typed reason without parsing
/// the human message.
pub(crate) fn archive_media_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    match vp_core::archive_media::failure_data(&error) {
        Some(data) => {
            let code = data
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("TOOL_FAILED");
            let rpc_code = match code {
                "MEDIA_TOOL_UNAVAILABLE" | "INTERNAL_ERROR" | "ARCHIVE_CORRUPT" => {
                    codes::INTERNAL_ERROR
                }
                _ => codes::INVALID_PARAMS,
            };
            RpcResponse::error(id, RpcError::with_data(rpc_code, error.to_string(), data))
        }
        None => internal_error(id, error),
    }
}

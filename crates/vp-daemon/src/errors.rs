use serde_json::{Value, json};
use vp_protocol::rpc::{RpcError, RpcResponse, codes};
use vp_protocol::types::Session;

pub(crate) fn internal_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    RpcResponse::error(id, RpcError::new(codes::INTERNAL_ERROR, error.to_string()))
}

fn bounded_message(message: String) -> String {
    const MAX_ERROR_CHARS: usize = 2_000;
    if message.chars().count() <= MAX_ERROR_CHARS {
        return message;
    }
    let mut bounded = message.chars().take(MAX_ERROR_CHARS).collect::<String>();
    bounded.push('…');
    bounded
}

fn typed_client_error(
    id: Option<Value>,
    rpc_code: i32,
    code: &str,
    message: String,
    retryable: bool,
    hint: &str,
) -> RpcResponse {
    RpcResponse::error(
        id,
        RpcError::with_data(
            rpc_code,
            bounded_message(message),
            json!({
                "code": code,
                "retryable": retryable,
                "hint": hint,
            }),
        ),
    )
}

/// Session operation receipts (`session.prompt`, `session.steer`,
/// `session.defer`, `session.run.resume`) distinguish four outcomes that a
/// retry policy must be able to tell apart.
///
/// `vp-core::operation` currently returns plain `anyhow` messages for them and
/// does not expose the marker type, so this mapper recognizes the four exact,
/// stable sentences. Nothing else is classified here: an unrelated store or IO
/// failure still maps to `INTERNAL_ERROR`.
pub(crate) fn operation_receipt_error(
    id: Option<Value>,
    error: &anyhow::Error,
) -> Option<RpcResponse> {
    let message = format!("{error:#}");
    let lowered = message.to_ascii_lowercase();
    if lowered.contains("is already in progress for session") {
        return Some(typed_client_error(
            id,
            codes::CONFLICT,
            "OPERATION_IN_PROGRESS",
            message,
            true,
            "the same operation_id is still running; wait for it to finish instead of retrying",
        ));
    }
    if lowered.contains("was already used for session") {
        return Some(typed_client_error(
            id,
            codes::CONFLICT,
            "OPERATION_CONFLICT",
            message,
            false,
            "the operation_id is already bound to a different method or payload; use a new id",
        ));
    }
    if lowered.contains("has an unknown outcome after a daemon restart") {
        return Some(typed_client_error(
            id,
            codes::CONFLICT,
            "OPERATION_UNKNOWN",
            message,
            false,
            "the previous attempt may have run; inspect the session transcript before retrying",
        ));
    }
    if lowered.contains("operation_id must be 1-")
        || lowered.contains("operation_id must not contain control characters")
    {
        return Some(typed_client_error(
            id,
            codes::INVALID_PARAMS,
            "INVALID_OPERATION_ID",
            message,
            false,
            "operation_id must be 1-200 non-blank characters without control characters",
        ));
    }
    None
}

/// Engine setters still report client-shaped validation failures as plain
/// messages. This maps only the bounded, stable sentences that the engine uses
/// for input that the caller can fix; everything else stays an internal error.
pub(crate) fn validation_error(id: Option<Value>, error: &anyhow::Error) -> Option<RpcResponse> {
    let message = format!("{error:#}");
    let lowered = message.to_ascii_lowercase();
    let (rpc_code, code, retryable, hint) = if lowered.contains("project not found") {
        (
            codes::NOT_FOUND,
            "PROJECT_NOT_FOUND",
            false,
            "create or select an existing project before retrying",
        )
    } else if lowered.contains("session priority must be between -100 and 100") {
        (
            codes::INVALID_PARAMS,
            "SESSION_PRIORITY_INVALID",
            false,
            "priority must be between -100 and 100",
        )
    } else if lowered.contains("an unlimited session cannot also declare") {
        (
            codes::INVALID_PARAMS,
            "SESSION_UNLIMITED_CONFLICT",
            false,
            "an unlimited session cannot also declare token, cost, or time budgets",
        )
    } else if lowered.contains("project token budget must be greater than zero") {
        (
            codes::INVALID_PARAMS,
            "PROJECT_TOKEN_BUDGET_INVALID",
            false,
            "token_budget must be greater than zero; omit it to clear the project budget",
        )
    } else if lowered.contains("project cost budget must be a positive finite number") {
        (
            codes::INVALID_PARAMS,
            "PROJECT_COST_BUDGET_INVALID",
            false,
            "cost_budget_usd must be a positive finite number",
        )
    } else if lowered.contains("token budget must be greater than zero") {
        (
            codes::INVALID_PARAMS,
            "SESSION_TOKEN_BUDGET_INVALID",
            false,
            "token_budget must be greater than zero; omit it for the project default",
        )
    } else if lowered.contains("cost budget must be a positive finite number") {
        (
            codes::INVALID_PARAMS,
            "SESSION_COST_BUDGET_INVALID",
            false,
            "cost_budget_usd must be a positive finite number",
        )
    } else if lowered.contains("time budget must be greater than zero") {
        (
            codes::INVALID_PARAMS,
            "SESSION_TIME_BUDGET_INVALID",
            false,
            "time_budget_secs must be greater than zero",
        )
    } else if lowered.contains("invalid critical-path policy") {
        (
            codes::INVALID_PARAMS,
            "POLICY_INVALID",
            false,
            "fix the policy ranges reported in the message and retry",
        )
    } else if lowered.starts_with("gui ") || lowered.contains(": gui ") {
        (
            codes::INVALID_PARAMS,
            "GUI_POLICY_INVALID",
            false,
            "fix the GUI policy ranges reported in the message and retry",
        )
    } else {
        return None;
    };
    Some(typed_client_error(
        id, rpc_code, code, message, retryable, hint,
    ))
}

/// Auto-budget validation uses a stable typed taxonomy. The engine and store
/// wrap `BudgetValidationError` in `anyhow::Error`, so the code is preserved by
/// downcasting instead of being flattened into `INTERNAL_ERROR`. A few store
/// paths format the typed error into a new message; those bounded prefixes keep
/// their stable code rather than inventing one from arbitrary prose.
pub(crate) fn budget_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    if let Some(typed) = error.downcast_ref::<vp_protocol::types::BudgetValidationError>() {
        let (retryable, hint) = match typed.code {
            vp_protocol::types::auto_budget_codes::BUDGET_BELOW_SPENT => (
                false,
                "raise the budget or hard cap above the amount already spent",
            ),
            vp_protocol::types::auto_budget_codes::AUTO_BUDGET_CAP_REQUIRED => (
                false,
                "choose --mode fixed or provide at least one hard token, cost, or wall-clock cap",
            ),
            vp_protocol::types::auto_budget_codes::AUTO_BUDGET_POLICY_REQUIRED => (
                false,
                "enable the project auto-budget policy or pass an explicit session policy",
            ),
            vp_protocol::types::auto_budget_codes::INVALID_BUDGET_MODE => {
                (false, "use fixed, auto, or unlimited")
            }
            vp_protocol::types::auto_budget_codes::BUDGET_MODE_CONFLICT => (
                false,
                "do not combine unlimited with fixed/auto, or set conflicting mode fields",
            ),
            vp_protocol::types::auto_budget_codes::BUDGET_POLICY_NOT_APPLICABLE => (
                false,
                "a session auto-budget policy only applies when the mode is auto",
            ),
            _ => (
                false,
                "fix the budget fields reported in the message and retry",
            ),
        };
        return typed_client_error(
            id,
            codes::INVALID_PARAMS,
            typed.code,
            typed.message.clone(),
            retryable,
            hint,
        );
    }

    let message = format!("{error:#}");
    let lowered = message.to_ascii_lowercase();
    if lowered.contains("imported sessions are read-only archives") {
        return typed_client_error(
            id,
            codes::CONFLICT,
            "SESSION_IMPORTED_READONLY",
            message,
            false,
            "imported sessions are read-only archives; create a new session or work from the source session",
        );
    }
    if lowered.contains("session not found") {
        return typed_client_error(
            id,
            codes::NOT_FOUND,
            "SESSION_NOT_FOUND",
            message,
            false,
            "select an existing session before retrying",
        );
    }
    if lowered.contains("invalid auto-budget policy:") {
        return typed_client_error(
            id,
            codes::INVALID_PARAMS,
            vp_protocol::types::auto_budget_codes::INVALID_AUTO_BUDGET_POLICY,
            message,
            false,
            "fix the policy ranges reported in the message and retry",
        );
    }
    if lowered.contains("invalid hard budget cap:") {
        return typed_client_error(
            id,
            codes::INVALID_PARAMS,
            vp_protocol::types::auto_budget_codes::INVALID_BUDGET_CAP,
            message,
            false,
            "hard caps must be positive finite values",
        );
    }
    if lowered.contains("session has no auto-budget row") {
        return typed_client_error(
            id,
            codes::NOT_FOUND,
            "SESSION_AUTO_BUDGET_NOT_FOUND",
            message,
            false,
            "switch the session to auto mode before changing auto-only state",
        );
    }
    // `project not found`, priority, and the legacy unlimited conflict keep
    // their existing typed mappings.
    validation_or_internal(id, error)
}

/// Idempotent-operation errors first, then bounded validation failures, then
/// the ordinary internal error. Used by RPC arms that accept an
/// `operation_id`.
pub(crate) fn operation_error(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    if let Some(response) = operation_receipt_error(id.clone(), &error) {
        return response;
    }
    if let Some(response) = validation_error(id.clone(), &error) {
        return response;
    }
    internal_error(id, error)
}

/// A typed `INVALID_PARAMS` for a bounded engine validation failure; every
/// other error keeps the ordinary `INTERNAL_ERROR` with no invented code.
pub(crate) fn validation_or_internal(id: Option<Value>, error: anyhow::Error) -> RpcResponse {
    match validation_error(id.clone(), &error) {
        Some(response) => response,
        None => internal_error(id, error),
    }
}

/// `session.create` is deliberately not transactional: the session row exists
/// before the Leader agent is created and the first run starts. Returning a
/// bare success would hide a failed start, so the RPC fails with a typed
/// `SESSION_START_FAILED` payload that names the session that was created.
pub(crate) fn session_start_error(
    id: Option<Value>,
    session: &Session,
    phase: &str,
    error: anyhow::Error,
) -> RpcResponse {
    let message = bounded_message(format!(
        "session {} was created but {phase} failed: {error:#}",
        session.id
    ));
    RpcResponse::error(
        id,
        RpcError::with_data(
            codes::INTERNAL_ERROR,
            message,
            json!({
                "code": "SESSION_START_FAILED",
                "phase": phase,
                "session_id": session.id,
                "project_id": session.project_id,
                "status": session.status,
                "session_created": true,
                "retryable": true,
                "hint": "the session exists; do not create a duplicate — fix the provider/agent configuration, then retry session.prompt",
            }),
        ),
    )
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
    operation_error(id, error)
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

//! Typed errors and their JSON-RPC projection.
//!
//! Error messages are deliberately terse. In particular, no error path ever
//! includes a bearer token, and host handler failures are reported with a
//! fixed public message rather than the host's internal error text.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::jsonrpc::{JsonRpcError, JsonRpcId, JsonRpcResponse};
use crate::limits::MAX_ID_BYTES;

pub type A2aResult<T> = Result<T, A2aError>;

/// Stable, machine-readable error identifiers. They are carried both in the
/// JSON-RPC `data.a2aCode` field and in local typed errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum A2aErrorCode {
    ParseError,
    InvalidRequest,
    MethodNotFound,
    InvalidParams,
    InternalError,
    TaskNotFound,
    TaskNotCancelable,
    PushNotificationNotSupported,
    UnsupportedOperation,
    ContentTypeNotSupported,
    InvalidAgentResponse,
    Unauthorized,
    PolicyDisabled,
    LoopbackRequired,
    BoundsExceeded,
    StoreFull,
    IdempotencyConflict,
    InvalidTransition,
    TaskBusy,
    TaskTerminal,
    HandlerFailed,
    InvalidHandlerUpdate,
    Transport,
    Protocol,
    AgentCardInvalid,
    ConfigInvalid,
}

impl A2aErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ParseError => "PARSE_ERROR",
            Self::InvalidRequest => "INVALID_REQUEST",
            Self::MethodNotFound => "METHOD_NOT_FOUND",
            Self::InvalidParams => "INVALID_PARAMS",
            Self::InternalError => "INTERNAL_ERROR",
            Self::TaskNotFound => "TASK_NOT_FOUND",
            Self::TaskNotCancelable => "TASK_NOT_CANCELABLE",
            Self::PushNotificationNotSupported => "PUSH_NOTIFICATION_NOT_SUPPORTED",
            Self::UnsupportedOperation => "UNSUPPORTED_OPERATION",
            Self::ContentTypeNotSupported => "CONTENT_TYPE_NOT_SUPPORTED",
            Self::InvalidAgentResponse => "INVALID_AGENT_RESPONSE",
            Self::Unauthorized => "UNAUTHORIZED",
            Self::PolicyDisabled => "POLICY_DISABLED",
            Self::LoopbackRequired => "LOOPBACK_REQUIRED",
            Self::BoundsExceeded => "BOUNDS_EXCEEDED",
            Self::StoreFull => "STORE_FULL",
            Self::IdempotencyConflict => "IDEMPOTENCY_CONFLICT",
            Self::InvalidTransition => "INVALID_TRANSITION",
            Self::TaskBusy => "TASK_BUSY",
            Self::TaskTerminal => "TASK_TERMINAL",
            Self::HandlerFailed => "HANDLER_FAILED",
            Self::InvalidHandlerUpdate => "INVALID_HANDLER_UPDATE",
            Self::Transport => "TRANSPORT_ERROR",
            Self::Protocol => "PROTOCOL_ERROR",
            Self::AgentCardInvalid => "AGENT_CARD_INVALID",
            Self::ConfigInvalid => "CONFIG_INVALID",
        }
    }

    /// JSON-RPC 2.0 code. A2A-specific codes use the reserved -32001.. range.
    pub const fn json_rpc_code(self) -> i64 {
        match self {
            Self::ParseError => -32700,
            Self::InvalidRequest => -32600,
            Self::MethodNotFound => -32601,
            Self::InvalidParams | Self::BoundsExceeded => -32602,
            Self::InternalError | Self::Protocol => -32603,
            Self::TaskNotFound => -32001,
            Self::TaskNotCancelable => -32002,
            Self::PushNotificationNotSupported => -32003,
            Self::UnsupportedOperation => -32004,
            Self::ContentTypeNotSupported => -32005,
            Self::InvalidAgentResponse => -32006,
            Self::Unauthorized => -32010,
            Self::PolicyDisabled => -32011,
            Self::LoopbackRequired => -32012,
            Self::StoreFull => -32013,
            Self::IdempotencyConflict => -32014,
            Self::InvalidTransition => -32015,
            Self::TaskBusy => -32016,
            Self::TaskTerminal => -32017,
            Self::HandlerFailed => -32018,
            Self::InvalidHandlerUpdate => -32019,
            Self::Transport => -32020,
            Self::AgentCardInvalid => -32021,
            Self::ConfigInvalid => -32022,
        }
    }

    /// HTTP status used when a failure is detected before a JSON-RPC result
    /// envelope can be produced.
    pub const fn http_status(self) -> u16 {
        match self {
            Self::Unauthorized => 401,
            Self::LoopbackRequired | Self::PolicyDisabled => 403,
            Self::BoundsExceeded => 413,
            Self::StoreFull => 429,
            Self::MethodNotFound | Self::TaskNotFound => 404,
            Self::ContentTypeNotSupported => 415,
            Self::TaskNotCancelable
            | Self::TaskBusy
            | Self::TaskTerminal
            | Self::InvalidTransition
            | Self::IdempotencyConflict => 409,
            _ => 400,
        }
    }
}

impl fmt::Display for A2aErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for A2aErrorCode {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let code = match value {
            "PARSE_ERROR" => Self::ParseError,
            "INVALID_REQUEST" => Self::InvalidRequest,
            "METHOD_NOT_FOUND" => Self::MethodNotFound,
            "INVALID_PARAMS" => Self::InvalidParams,
            "INTERNAL_ERROR" => Self::InternalError,
            "TASK_NOT_FOUND" => Self::TaskNotFound,
            "TASK_NOT_CANCELABLE" => Self::TaskNotCancelable,
            "PUSH_NOTIFICATION_NOT_SUPPORTED" => Self::PushNotificationNotSupported,
            "UNSUPPORTED_OPERATION" => Self::UnsupportedOperation,
            "CONTENT_TYPE_NOT_SUPPORTED" => Self::ContentTypeNotSupported,
            "INVALID_AGENT_RESPONSE" => Self::InvalidAgentResponse,
            "UNAUTHORIZED" => Self::Unauthorized,
            "POLICY_DISABLED" => Self::PolicyDisabled,
            "LOOPBACK_REQUIRED" => Self::LoopbackRequired,
            "BOUNDS_EXCEEDED" => Self::BoundsExceeded,
            "STORE_FULL" => Self::StoreFull,
            "IDEMPOTENCY_CONFLICT" => Self::IdempotencyConflict,
            "INVALID_TRANSITION" => Self::InvalidTransition,
            "TASK_BUSY" => Self::TaskBusy,
            "TASK_TERMINAL" => Self::TaskTerminal,
            "HANDLER_FAILED" => Self::HandlerFailed,
            "INVALID_HANDLER_UPDATE" => Self::InvalidHandlerUpdate,
            "TRANSPORT_ERROR" => Self::Transport,
            "PROTOCOL_ERROR" => Self::Protocol,
            "AGENT_CARD_INVALID" => Self::AgentCardInvalid,
            "CONFIG_INVALID" => Self::ConfigInvalid,
            _ => return Err(()),
        };
        Ok(code)
    }
}

/// All facade failures are represented here. The variants are deliberately
/// data-minimal so a token or host secret cannot accidentally become part of
/// an error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum A2aError {
    #[error("the A2A facade is disabled")]
    PolicyDisabled,
    #[error("only loopback addresses are allowed: {address}")]
    LoopbackRequired { address: String },
    #[error("invalid configuration: {reason}")]
    ConfigInvalid { reason: String },
    #[error("invalid agent card: {reason}")]
    AgentCardInvalid { reason: String },
    #[error("parse error: {reason}")]
    ParseError { reason: String },
    #[error("invalid request: {reason}")]
    InvalidRequest { reason: String },
    #[error("invalid parameters: {reason}")]
    InvalidParams { reason: String },
    #[error("{field} exceeds its bound (max {max}, actual {actual})")]
    BoundsExceeded {
        field: &'static str,
        max: usize,
        actual: usize,
    },
    #[error("unauthorized")]
    Unauthorized,
    #[error("method not found: {method}")]
    MethodNotFound { method: String },
    #[error("unsupported operation: {operation}")]
    UnsupportedOperation { operation: String },
    #[error("push notifications are not supported")]
    PushNotificationNotSupported,
    #[error("unsupported content type")]
    ContentTypeNotSupported,
    #[error("invalid agent response: {reason}")]
    InvalidAgentResponse { reason: String },
    #[error("task not found: {task_id}")]
    TaskNotFound { task_id: String },
    #[error("task cannot be canceled: {task_id}")]
    TaskNotCancelable { task_id: String },
    #[error("task is busy: {task_id}")]
    TaskBusy { task_id: String },
    #[error("task is terminal: {task_id}")]
    TaskTerminal { task_id: String },
    #[error("invalid task transition: {from} -> {to}")]
    InvalidTransition { from: String, to: String },
    #[error("idempotency conflict for message {message_id}")]
    IdempotencyConflict { message_id: String },
    #[error("the task store is full")]
    StoreFull,
    #[error("the host handler failed")]
    HandlerFailed,
    #[error("the host handler returned an update that cannot settle a run")]
    InvalidHandlerUpdate,
    #[error("transport error: {reason}")]
    Transport { reason: String },
    #[error("protocol error: {reason}")]
    Protocol { reason: String },
    #[error("internal A2A error")]
    Internal,
}

impl A2aError {
    pub const fn code(&self) -> A2aErrorCode {
        match self {
            Self::PolicyDisabled => A2aErrorCode::PolicyDisabled,
            Self::LoopbackRequired { .. } => A2aErrorCode::LoopbackRequired,
            Self::ConfigInvalid { .. } => A2aErrorCode::ConfigInvalid,
            Self::AgentCardInvalid { .. } => A2aErrorCode::AgentCardInvalid,
            Self::ParseError { .. } => A2aErrorCode::ParseError,
            Self::InvalidRequest { .. } => A2aErrorCode::InvalidRequest,
            Self::InvalidParams { .. } => A2aErrorCode::InvalidParams,
            Self::BoundsExceeded { .. } => A2aErrorCode::BoundsExceeded,
            Self::Unauthorized => A2aErrorCode::Unauthorized,
            Self::MethodNotFound { .. } => A2aErrorCode::MethodNotFound,
            Self::UnsupportedOperation { .. } => A2aErrorCode::UnsupportedOperation,
            Self::PushNotificationNotSupported => A2aErrorCode::PushNotificationNotSupported,
            Self::ContentTypeNotSupported => A2aErrorCode::ContentTypeNotSupported,
            Self::InvalidAgentResponse { .. } => A2aErrorCode::InvalidAgentResponse,
            Self::TaskNotFound { .. } => A2aErrorCode::TaskNotFound,
            Self::TaskNotCancelable { .. } => A2aErrorCode::TaskNotCancelable,
            Self::TaskBusy { .. } => A2aErrorCode::TaskBusy,
            Self::TaskTerminal { .. } => A2aErrorCode::TaskTerminal,
            Self::InvalidTransition { .. } => A2aErrorCode::InvalidTransition,
            Self::IdempotencyConflict { .. } => A2aErrorCode::IdempotencyConflict,
            Self::StoreFull => A2aErrorCode::StoreFull,
            Self::HandlerFailed => A2aErrorCode::HandlerFailed,
            Self::InvalidHandlerUpdate => A2aErrorCode::InvalidHandlerUpdate,
            Self::Transport { .. } => A2aErrorCode::Transport,
            Self::Protocol { .. } => A2aErrorCode::Protocol,
            Self::Internal => A2aErrorCode::InternalError,
        }
    }

    pub const fn json_rpc_code(&self) -> i64 {
        self.code().json_rpc_code()
    }

    pub const fn http_status(&self) -> u16 {
        self.code().http_status()
    }

    /// A bounded public message. IDs and reasons are validated and capped
    /// before they reach this method.
    pub fn public_message(&self) -> String {
        let message = self.to_string();
        if message.chars().count() > 512 {
            message.chars().take(512).collect()
        } else {
            message
        }
    }

    /// Encode as a JSON-RPC 2.0 error object. The A2A code is always present
    /// in `data.a2aCode`; useful bounds/IDs are included when applicable.
    pub fn to_json_rpc(&self, id: JsonRpcId) -> JsonRpcResponse {
        let mut data = Map::new();
        data.insert("a2aCode".into(), Value::String(self.code().as_str().into()));
        match self {
            Self::BoundsExceeded { field, max, actual } => {
                data.insert("field".into(), Value::String((*field).into()));
                data.insert("max".into(), json!(max));
                data.insert("actual".into(), json!(actual));
            }
            Self::TaskNotFound { task_id }
            | Self::TaskNotCancelable { task_id }
            | Self::TaskBusy { task_id }
            | Self::TaskTerminal { task_id } => {
                data.insert("taskId".into(), Value::String(task_id.clone()));
            }
            Self::IdempotencyConflict { message_id } => {
                data.insert("messageId".into(), Value::String(message_id.clone()));
            }
            Self::MethodNotFound { method } => {
                data.insert("method".into(), Value::String(method.clone()));
            }
            _ => {}
        }
        JsonRpcResponse::error(
            id,
            JsonRpcError {
                code: self.json_rpc_code(),
                message: self.public_message(),
                data: Some(Value::Object(data)),
            },
        )
    }

    /// Reconstruct a typed error from a remote JSON-RPC error. Remote text is
    /// bounded here; callers that know a secret should redact it afterwards.
    pub fn from_json_rpc(error: &JsonRpcError) -> Self {
        let explicit = error
            .data
            .as_ref()
            .and_then(|data| data.get("a2aCode"))
            .and_then(Value::as_str)
            .and_then(|value| A2aErrorCode::from_str(value).ok());
        let code = explicit.or(match error.code {
            -32700 => Some(A2aErrorCode::ParseError),
            -32600 => Some(A2aErrorCode::InvalidRequest),
            -32601 => Some(A2aErrorCode::MethodNotFound),
            -32602 => Some(A2aErrorCode::InvalidParams),
            -32603 => Some(A2aErrorCode::InternalError),
            -32001 => Some(A2aErrorCode::TaskNotFound),
            -32002 => Some(A2aErrorCode::TaskNotCancelable),
            -32003 => Some(A2aErrorCode::PushNotificationNotSupported),
            -32004 => Some(A2aErrorCode::UnsupportedOperation),
            -32005 => Some(A2aErrorCode::ContentTypeNotSupported),
            -32006 => Some(A2aErrorCode::InvalidAgentResponse),
            -32010 => Some(A2aErrorCode::Unauthorized),
            _ => None,
        });
        let task_id = error
            .data
            .as_ref()
            .and_then(|data| data.get("taskId"))
            .and_then(Value::as_str)
            .map(bounded_id)
            .unwrap_or_default();
        let message_id = error
            .data
            .as_ref()
            .and_then(|data| data.get("messageId"))
            .and_then(Value::as_str)
            .map(bounded_id)
            .unwrap_or_default();
        let reason = if error.message.chars().count() > 512 {
            error.message.chars().take(512).collect()
        } else {
            error.message.clone()
        };
        match code {
            Some(A2aErrorCode::ParseError) => Self::ParseError { reason },
            Some(A2aErrorCode::InvalidRequest) => Self::InvalidRequest { reason },
            Some(A2aErrorCode::MethodNotFound) => Self::MethodNotFound {
                method: error
                    .data
                    .as_ref()
                    .and_then(|data| data.get("method"))
                    .and_then(Value::as_str)
                    .map(bounded_id)
                    .unwrap_or_default(),
            },
            Some(A2aErrorCode::InvalidParams) => Self::InvalidParams { reason },
            Some(A2aErrorCode::InternalError) => Self::Protocol { reason },
            Some(A2aErrorCode::TaskNotFound) => Self::TaskNotFound { task_id },
            Some(A2aErrorCode::TaskNotCancelable) => Self::TaskNotCancelable { task_id },
            Some(A2aErrorCode::PushNotificationNotSupported) => Self::PushNotificationNotSupported,
            Some(A2aErrorCode::UnsupportedOperation) => {
                Self::UnsupportedOperation { operation: reason }
            }
            Some(A2aErrorCode::ContentTypeNotSupported) => Self::ContentTypeNotSupported,
            Some(A2aErrorCode::InvalidAgentResponse) => Self::InvalidAgentResponse { reason },
            Some(A2aErrorCode::Unauthorized) => Self::Unauthorized,
            Some(A2aErrorCode::PolicyDisabled) => Self::PolicyDisabled,
            Some(A2aErrorCode::LoopbackRequired) => Self::LoopbackRequired { address: reason },
            Some(A2aErrorCode::BoundsExceeded) => {
                let field = error
                    .data
                    .as_ref()
                    .and_then(|data| data.get("field"))
                    .and_then(Value::as_str)
                    .map(bounded_id)
                    .unwrap_or_else(|| "remote".into());
                let max = error
                    .data
                    .as_ref()
                    .and_then(|data| data.get("max"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize;
                let actual = error
                    .data
                    .as_ref()
                    .and_then(|data| data.get("actual"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize;
                // The field name is a &'static str in the local type. An
                // unknown remote field is reported as a generic local bound
                // instead of retaining attacker-controlled text.
                let local_field = match field.as_str() {
                    "max_body_bytes" | "body" => "body",
                    "max_task_history" | "task.history" => "task.history",
                    "max_message_parts" | "message.parts" => "message.parts",
                    "max_part_text_bytes" | "message.part.text" => "message.part.text",
                    "max_data_bytes" | "message.part.data" => "message.part.data",
                    "max_artifact_bytes" | "artifact" => "artifact",
                    "max_skills" | "skills" => "skills",
                    "max_stored_tasks" | "tasks" => "tasks",
                    "max_file_part_bytes" | "message.part.file" => "message.part.file",
                    _ => "remote",
                };
                Self::BoundsExceeded {
                    field: local_field,
                    max,
                    actual,
                }
            }
            Some(A2aErrorCode::StoreFull) => Self::StoreFull,
            Some(A2aErrorCode::IdempotencyConflict) => Self::IdempotencyConflict { message_id },
            Some(A2aErrorCode::InvalidTransition) => Self::InvalidTransition {
                from: "remote".into(),
                to: reason,
            },
            Some(A2aErrorCode::TaskBusy) => Self::TaskBusy { task_id },
            Some(A2aErrorCode::TaskTerminal) => Self::TaskTerminal { task_id },
            Some(A2aErrorCode::HandlerFailed) => Self::HandlerFailed,
            Some(A2aErrorCode::InvalidHandlerUpdate) => Self::InvalidHandlerUpdate,
            Some(A2aErrorCode::Transport) => Self::Transport { reason },
            Some(A2aErrorCode::AgentCardInvalid) => Self::AgentCardInvalid { reason },
            Some(A2aErrorCode::ConfigInvalid) => Self::ConfigInvalid { reason },
            Some(A2aErrorCode::Protocol) | None => Self::Protocol { reason },
        }
    }
}

fn bounded_id(value: &str) -> String {
    if value.chars().count() > MAX_ID_BYTES {
        value.chars().take(MAX_ID_BYTES).collect()
    } else {
        value.to_string()
    }
}

impl From<serde_json::Error> for A2aError {
    fn from(error: serde_json::Error) -> Self {
        Self::Protocol {
            reason: format!(
                "invalid JSON at line {} column {}",
                error.line(),
                error.column()
            ),
        }
    }
}

impl From<std::io::Error> for A2aError {
    fn from(error: std::io::Error) -> Self {
        Self::Transport {
            reason: error.kind().to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_stable_and_distinct() {
        let codes = [
            A2aErrorCode::ParseError,
            A2aErrorCode::InvalidRequest,
            A2aErrorCode::MethodNotFound,
            A2aErrorCode::InvalidParams,
            A2aErrorCode::InternalError,
            A2aErrorCode::TaskNotFound,
            A2aErrorCode::TaskNotCancelable,
            A2aErrorCode::BoundsExceeded,
            A2aErrorCode::Unauthorized,
        ];
        let mut strings = std::collections::BTreeSet::new();
        for code in codes {
            assert!(strings.insert(code.as_str()));
            assert_eq!(A2aErrorCode::from_str(code.as_str()), Ok(code));
        }
    }

    #[test]
    fn jsonrpc_error_contains_typed_data() {
        let error = A2aError::BoundsExceeded {
            field: "message.parts",
            max: 32,
            actual: 33,
        };
        let response = error.to_json_rpc(JsonRpcId::Number(7));
        assert_eq!(response.id, JsonRpcId::Number(7));
        let jsonrpc = response.error.expect("error response");
        assert_eq!(jsonrpc.code, -32602);
        assert_eq!(
            jsonrpc
                .data
                .as_ref()
                .and_then(|data| data.get("a2aCode"))
                .and_then(Value::as_str),
            Some("BOUNDS_EXCEEDED")
        );
    }

    #[test]
    fn unauthorized_never_contains_token_material() {
        let error = A2aError::Unauthorized;
        let encoded =
            serde_json::to_string(&error.to_json_rpc(JsonRpcId::Null)).expect("serializable error");
        assert!(!encoded.to_ascii_lowercase().contains("bearer"));
    }
}

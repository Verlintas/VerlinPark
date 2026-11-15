//! JSON-RPC 2.0 envelope and method parameter types.

use std::fmt;

use serde::de::{self, Deserializer};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::wire::Message;

pub const JSONRPC_VERSION: &str = "2.0";
pub const METHOD_MESSAGE_SEND: &str = "message/send";
pub const METHOD_MESSAGE_STREAM: &str = "message/stream";
pub const METHOD_TASKS_GET: &str = "tasks/get";
pub const METHOD_TASKS_CANCEL: &str = "tasks/cancel";
pub const METHOD_TASKS_RESUBSCRIBE: &str = "tasks/resubscribe";

/// The Agent Card is served by HTTP GET, not by JSON-RPC.
pub const AGENT_CARD_PATH: &str = "/.well-known/agent-card.json";
pub const RPC_PATH: &str = "/a2a";
pub const TASK_EVENTS_PATH_PREFIX: &str = "/a2a/tasks/";

/// JSON-RPC request identifier. `null` is accepted only on responses.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum JsonRpcId {
    Null,
    Number(i64),
    String(String),
}

impl JsonRpcId {
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    pub fn bounded_len(&self) -> usize {
        match self {
            Self::Null | Self::Number(_) => 0,
            Self::String(value) => value.len(),
        }
    }
}

impl fmt::Display for JsonRpcId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => f.write_str("null"),
            Self::Number(value) => value.fmt(f),
            Self::String(value) => value.fmt(f),
        }
    }
}

impl Serialize for JsonRpcId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Null => serializer.serialize_none(),
            Self::Number(value) => serializer.serialize_i64(*value),
            Self::String(value) => serializer.serialize_str(value),
        }
    }
}

impl<'de> Deserialize<'de> for JsonRpcId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        match value {
            Value::Null => Ok(Self::Null),
            Value::Number(number) => number
                .as_i64()
                .map(Self::Number)
                .ok_or_else(|| de::Error::custom("JSON-RPC id must be an integer or string")),
            Value::String(value) => Ok(Self::String(value)),
            _ => Err(de::Error::custom(
                "JSON-RPC id must be null, an integer, or a string",
            )),
        }
    }
}

/// JSON-RPC 2.0 request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: JsonRpcId,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcRequest {
    pub fn new(id: JsonRpcId, method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.into(),
            id,
            method: method.into(),
            params: Some(params),
        }
    }
}

/// JSON-RPC 2.0 success or error response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: JsonRpcId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    pub fn success(id: JsonRpcId, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: JsonRpcId, error: JsonRpcError) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.into(),
            id,
            result: None,
            error: Some(error),
        }
    }

    pub fn is_success(&self) -> bool {
        self.error.is_none() && self.result.is_some()
    }
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Parameters for `message/send` and `message/stream`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageParams {
    pub message: Message,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configuration: Option<MessageSendConfiguration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
}

impl SendMessageParams {
    pub fn new(message: Message) -> Self {
        Self {
            message,
            configuration: None,
            metadata: None,
        }
    }
}

/// Optional `message/send` configuration.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageSendConfiguration {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accepted_output_modes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_length: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocking: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push_notification_config: Option<Value>,
}

impl MessageSendConfiguration {
    pub fn blocking(&self) -> bool {
        self.blocking.unwrap_or(true)
    }
}

/// Parameters for `tasks/get`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskQueryParams {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_length: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
}

impl TaskQueryParams {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            history_length: None,
            metadata: None,
        }
    }
}

/// Parameters for `tasks/cancel`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskIdParams {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
}

impl TaskIdParams {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            metadata: None,
        }
    }
}

/// Parameters for `tasks/resubscribe`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskResubscribeParams {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
}

impl TaskResubscribeParams {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            last_event_id: None,
            metadata: None,
        }
    }
}

/// Query for the GET task-events route. The SSE `Last-Event-ID` header takes
/// precedence when both are present.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskEventsQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event_id: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_all_jsonrpc_shapes() {
        for id in [
            JsonRpcId::Null,
            JsonRpcId::Number(-7),
            JsonRpcId::Number(0),
            JsonRpcId::String("request-1".into()),
        ] {
            let encoded = serde_json::to_string(&id).expect("serialize id");
            let decoded: JsonRpcId = serde_json::from_str(&encoded).expect("deserialize id");
            assert_eq!(decoded, id);
        }
    }

    #[test]
    fn rejects_fractional_and_structured_ids() {
        assert!(serde_json::from_str::<JsonRpcId>("1.5").is_err());
        assert!(serde_json::from_str::<JsonRpcId>("{}").is_err());
        assert!(serde_json::from_str::<JsonRpcId>("false").is_err());
    }

    #[test]
    fn response_has_exactly_one_payload() {
        let response = JsonRpcResponse::success(JsonRpcId::Number(1), Value::Null);
        let encoded = serde_json::to_value(&response).expect("serialize response");
        assert!(encoded.get("result").is_some());
        assert!(encoded.get("error").is_none());
    }
}

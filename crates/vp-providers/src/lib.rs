//! Provider adapters.
//!
//! v0.1 ships two adapters:
//! - any OpenAI-compatible `/chat/completions` endpoint (cloud or local)
//! - Anthropic's `/v1/messages` streaming endpoint
//!
//! Tool calling and multimodal content are deliberately deferred to M1.

pub mod anthropic;
pub mod openai;

use std::pin::Pin;

use async_trait::async_trait;
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use anthropic::AnthropicProvider;
pub use openai::OpenAiCompatibleProvider;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".to_string(),
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn assistant_tool_calls(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: content.into(),
            tool_calls: Some(tool_calls),
            tool_call_id: None,
        }
    }

    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".to_string(),
            content: content.into(),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub system: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub tools: Vec<ToolSchema>,
}

impl ChatRequest {
    pub fn new(model: impl Into<String>, messages: Vec<ChatMessage>) -> Self {
        Self {
            model: model.into(),
            messages,
            system: None,
            max_tokens: None,
            temperature: None,
            tools: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    TextDelta(String),
    ToolCall(ToolCall),
    Usage {
        input_tokens: u32,
        output_tokens: u32,
    },
    Done,
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("missing API key")]
    MissingApiKey,
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("provider returned HTTP {status}: {body}")]
    Api { status: u16, body: String },
    #[error("provider stream error: {0}")]
    Stream(String),
    #[error("provider response parse error: {0}")]
    Parse(String),
    #[error("provider does not support this request: {0}")]
    Unsupported(String),
}

pub type EventStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>>;

const PROVIDER_ATTEMPTS: u32 = 3;
const RETRY_BASE_DELAY: std::time::Duration = std::time::Duration::from_millis(200);
const RETRY_MAX_DELAY: std::time::Duration = std::time::Duration::from_secs(4);

pub(crate) fn provider_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

fn retryable_status(status: u16) -> bool {
    matches!(status, 408 | 409 | 425 | 429 | 500 | 502 | 503 | 504)
}

fn retry_after(response: &reqwest::Response) -> Option<std::time::Duration> {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(std::time::Duration::from_secs)
}

fn backoff(attempt: u32) -> std::time::Duration {
    RETRY_BASE_DELAY
        .saturating_mul(1 << attempt.saturating_sub(1).min(4))
        .min(RETRY_MAX_DELAY)
}

/// Send a provider request, retrying transient connection failures and
/// rate-limit/server responses. The request body is rebuilt for every attempt.
pub(crate) async fn send_with_retry<F>(mut build: F) -> Result<reqwest::Response, ProviderError>
where
    F: FnMut() -> reqwest::RequestBuilder,
{
    let mut attempt = 0;
    loop {
        attempt += 1;
        match build().send().await {
            Ok(response) if response.status().is_success() => return Ok(response),
            Ok(response) => {
                let status = response.status().as_u16();
                if !retryable_status(status) || attempt >= PROVIDER_ATTEMPTS {
                    let body = response.text().await.unwrap_or_default();
                    return Err(ProviderError::Api { status, body });
                }
                let delay = retry_after(&response)
                    .unwrap_or_else(|| backoff(attempt))
                    .min(RETRY_MAX_DELAY);
                drop(response);
                tokio::time::sleep(delay).await;
            }
            Err(error) => {
                if attempt >= PROVIDER_ATTEMPTS || !(error.is_timeout() || error.is_connect()) {
                    return Err(ProviderError::Http(error));
                }
                tokio::time::sleep(backoff(attempt)).await;
            }
        }
    }
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &str;

    async fn stream_chat(&self, request: ChatRequest) -> Result<EventStream, ProviderError>;
}

pub(crate) fn openai_usage(value: &Value) -> Option<StreamEvent> {
    let input = value
        .pointer("/usage/prompt_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let output = value
        .pointer("/usage/completion_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    if input == 0 && output == 0 {
        None
    } else {
        Some(StreamEvent::Usage {
            input_tokens: input,
            output_tokens: output,
        })
    }
}

pub(crate) fn provider_error_from_stream(error: impl std::fmt::Display) -> ProviderError {
    ProviderError::Stream(error.to_string())
}

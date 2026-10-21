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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".to_string(),
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: content.into(),
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
}

impl ChatRequest {
    pub fn new(model: impl Into<String>, messages: Vec<ChatMessage>) -> Self {
        Self {
            model: model.into(),
            messages,
            system: None,
            max_tokens: None,
            temperature: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    TextDelta(String),
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
}

pub type EventStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>>;

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

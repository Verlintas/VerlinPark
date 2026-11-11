//! Provider adapters.
//!
//! v0.1 ships two adapters:
//! - any OpenAI-compatible `/chat/completions` endpoint (cloud or local)
//! - Anthropic's `/v1/messages` streaming endpoint
//!
//! Tool calling and multimodal content are deliberately deferred to M1.

pub mod anthropic;
pub mod cache;
pub mod openai;
pub mod preempt;

use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use anthropic::AnthropicProvider;
pub use cache::{
    CacheBreakpointPlan, CacheTtl, PromptCacheConfig, SystemSection,
    looks_like_cache_ttl_rejection, plan_cache_breakpoints,
};
pub use openai::OpenAiCompatibleProvider;
pub use preempt::{
    PreemptPhase, PreemptReason, PreemptSnapshot, PreemptToken, ToolGrace,
    current as current_preempt, scoped as scoped_preempt,
};

static TOOL_CALL_COUNTER: AtomicU64 = AtomicU64::new(1);

pub(crate) fn fallback_tool_call_id(index: usize) -> String {
    format!(
        "call_{index}_{}",
        TOOL_CALL_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

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
    /// Optional marker for providers with explicit prompt caching: when this is
    /// an exact prefix of `system`, the adapter may place a cache breakpoint
    /// after it. It never adds or removes prompt content.
    pub system_stable_prefix: Option<String>,
    /// Deterministic prompt sections in stable-first order. Adapters that
    /// support cache breakpoints use these exact boundaries; providers without
    /// explicit caching still receive the same combined `system` text.
    pub system_sections: Vec<SystemSection>,
    /// Per-request cache policy. `None` preserves the legacy adapter behavior.
    pub cache: Option<PromptCacheConfig>,
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
            system_stable_prefix: None,
            system_sections: Vec::new(),
            cache: None,
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
        /// Total provider input tokens. This is deliberately the provider's
        /// total: uncached input + cache reads + cache writes.
        input_tokens: u32,
        output_tokens: u32,
        /// Subset of `input_tokens` served from the provider prompt cache.
        cache_read_tokens: u32,
        /// Subset of `input_tokens` written to the provider prompt cache.
        cache_write_tokens: u32,
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
    /// The run or tool deadline fired while the provider request was in
    /// flight. The adapter aborts the read between chunks and never resumes or
    /// replays the partial turn.
    #[error("provider preempted during {phase} ({reason}) after {elapsed_ms}ms")]
    Preempted {
        reason: String,
        phase: String,
        elapsed_ms: u64,
        side_effects_unknown: bool,
    },
}

impl ProviderError {
    pub fn preempted(token: &PreemptToken) -> Self {
        let (reason, phase, elapsed_ms, side_effects_unknown) = preempt::error_snapshot(token);
        Self::Preempted {
            reason,
            phase,
            elapsed_ms,
            side_effects_unknown,
        }
    }

    pub fn is_preempted(&self) -> bool {
        matches!(self, Self::Preempted { .. })
    }
}

pub type EventStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>>;

const PROVIDER_ATTEMPTS: u32 = 3;
const RETRY_BASE_DELAY: std::time::Duration = std::time::Duration::from_millis(200);
const RETRY_MAX_DELAY: std::time::Duration = std::time::Duration::from_secs(4);

pub(crate) fn provider_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .read_timeout(std::time::Duration::from_secs(300))
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
/// A retry delay or in-flight attempt observes the optional preemption token;
/// retries never ignore a hard preemption.
pub(crate) async fn send_with_retry_preemptible<F>(
    mut build: F,
    preempt: Option<&PreemptToken>,
) -> Result<reqwest::Response, ProviderError>
where
    F: FnMut() -> reqwest::RequestBuilder,
{
    let mut attempt = 0;
    loop {
        if preempt.is_some_and(PreemptToken::is_preempted) {
            return Err(preempt
                .map(ProviderError::preempted)
                .unwrap_or_else(|| ProviderError::Stream("request preempted".into())));
        }
        attempt += 1;
        let sent = match preempt {
            Some(token) => tokio::select! {
                _ = token.cancelled() => return Err(ProviderError::preempted(token)),
                response = build().send() => response,
            },
            None => build().send().await,
        };
        match sent {
            Ok(response) if response.status().is_success() => return Ok(response),
            Ok(response) => {
                let status = response.status().as_u16();
                if !retryable_status(status) || attempt >= PROVIDER_ATTEMPTS {
                    let body = redact_provider_body(&response.text().await.unwrap_or_default());
                    return Err(ProviderError::Api { status, body });
                }
                let delay = retry_after(&response)
                    .unwrap_or_else(|| backoff(attempt))
                    .min(RETRY_MAX_DELAY);
                drop(response);
                if !sleep_preemptible(delay, preempt).await {
                    return Err(preempt
                        .map(ProviderError::preempted)
                        .unwrap_or_else(|| ProviderError::Stream("retry aborted".into())));
                }
            }
            Err(error) => {
                if attempt >= PROVIDER_ATTEMPTS || !(error.is_timeout() || error.is_connect()) {
                    return Err(ProviderError::Http(error));
                }
                if !sleep_preemptible(backoff(attempt), preempt).await {
                    return Err(preempt
                        .map(ProviderError::preempted)
                        .unwrap_or_else(|| ProviderError::Stream("retry aborted".into())));
                }
            }
        }
    }
}

/// Sleep for a retry delay unless the token fires first. Returns false when
/// preempted.
async fn sleep_preemptible(delay: std::time::Duration, preempt: Option<&PreemptToken>) -> bool {
    match preempt {
        Some(token) => {
            tokio::select! {
                _ = token.cancelled() => false,
                _ = tokio::time::sleep(delay) => !token.is_preempted(),
            }
        }
        None => {
            tokio::time::sleep(delay).await;
            true
        }
    }
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &str;

    async fn stream_chat(&self, request: ChatRequest) -> Result<EventStream, ProviderError>;

    /// Start a stream that observes a hard preemption token. The default
    /// implementation ignores the token so test and third-party providers keep
    /// their existing behavior; the runtime consumer still watches the token
    /// between chunks. First-party adapters override this and abort the
    /// HTTP/SSE read between chunks.
    async fn stream_chat_preemptible(
        &self,
        request: ChatRequest,
        token: Option<PreemptToken>,
    ) -> Result<EventStream, ProviderError> {
        let _ = token;
        self.stream_chat(request).await
    }
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
    // OpenAI reports `prompt_tokens_details.cached_tokens`; several compatible
    // servers also expose DeepSeek-style cache-hit fields. Cache writes are not
    // standard OpenAI metadata, but accept the vendor fields when present.
    let cache_read = value
        .pointer("/usage/prompt_tokens_details/cached_tokens")
        .and_then(Value::as_u64)
        .or_else(|| {
            value
                .pointer("/usage/prompt_cache_hit_tokens")
                .and_then(Value::as_u64)
        })
        .unwrap_or(0);
    let cache_write = value
        .pointer("/usage/prompt_tokens_details/cache_write_tokens")
        .and_then(Value::as_u64)
        .or_else(|| {
            value
                .pointer("/usage/cache_write_tokens")
                .and_then(Value::as_u64)
        })
        .or_else(|| {
            value
                .pointer("/usage/prompt_cache_write_tokens")
                .and_then(Value::as_u64)
        })
        .unwrap_or(0);
    // Cache tokens are parts of the total input, never additive. Clamp
    // inconsistent provider reports so cost accounting cannot double count.
    let input_u64 = u64::from(input);
    let cache_read = cache_read.min(input_u64);
    let cache_write = cache_write.min(input_u64.saturating_sub(cache_read));
    if input == 0 && output == 0 {
        None
    } else {
        Some(StreamEvent::Usage {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: cache_read as u32,
            cache_write_tokens: cache_write as u32,
        })
    }
}

/// Provider error bodies can echo request headers or keys. Keep a short,
/// redacted diagnostic instead of persisting the full response.
pub(crate) fn redact_provider_body(body: &str) -> String {
    let mut redacted = body.to_string();
    for marker in [
        "Bearer ",
        "bearer ",
        "sk-",
        "x-api-key",
        "api_key",
        "api-key",
    ] {
        let mut search = 0;
        while let Some(relative) = redacted[search..].find(marker) {
            let index = search + relative;
            let value_start = index + marker.len();
            let rest = &redacted[value_start..];
            let value_len = rest
                .find(|ch: char| ch.is_whitespace() || matches!(ch, '"' | '\'' | ',' | '}'))
                .unwrap_or(rest.len());
            if value_len == 0 {
                search = value_start;
                continue;
            }
            let value_end = value_start + value_len;
            redacted.replace_range(value_start..value_end, "[redacted]");
            search = value_start + "[redacted]".len();
        }
    }
    const MAX: usize = 8_000;
    if redacted.len() > MAX {
        let mut end = MAX;
        while end > 0 && !redacted.is_char_boundary(end) {
            end -= 1;
        }
        redacted.truncate(end);
        redacted.push('…');
    }
    redacted
}

pub(crate) fn provider_error_from_stream(error: impl std::fmt::Display) -> ProviderError {
    ProviderError::Stream(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_error_bodies_are_redacted_and_bounded() {
        let body = format!(
            "{{\"error\":\"bad key sk-secret123\",\"header\":\"Bearer abc.def\",\"x\":\"{}\"}}",
            "x".repeat(10_000)
        );
        let redacted = redact_provider_body(&body);
        assert!(!redacted.contains("sk-secret123"));
        assert!(!redacted.contains("abc.def"));
        assert!(redacted.contains("[redacted]"));
        assert!(redacted.len() <= 8_010);
    }
}

//! Explicit, operator-configured embedding client for hybrid project memory.
//!
//! This module deliberately contains only transport and validation:
//!
//! - no model selection
//! - no fallback provider
//! - no automatic enablement
//! - no retrieval ranking (that stays in [`crate::memory`])
//! - no persistence of request text, vectors, or credentials
//!
//! The endpoint is an OpenAI-compatible `/embeddings` URL. The API key is read
//! from the environment variable named by the policy at call time and is never
//! stored, returned, or logged. A disabled or incomplete policy never reaches
//! this module from daemon code paths.
//!
//! Requests send an array of exact text strings and an explicit `dimensions`
//! value. Responses are accepted as JSON number arrays or OpenAI base64
//! (little-endian `f32`) strings. Individual malformed entries are reported
//! per item so a partial batch can still index the valid entries.

use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use base64::Engine as _;
use serde_json::{Value, json};
use vp_protocol::types::MemoryEmbeddingPolicy;

use crate::memory::{
    MAX_EMBEDDING_DIMENSIONS, MIN_EMBEDDING_DIMENSIONS, decode_embedding_vector,
    encode_embedding_vector,
};

/// Retry budget for the daemon reconciler. HTTP-level retries are separate and
/// bounded inside one call; this catches persistent endpoint failure with
/// exponential backoff.
pub const MAX_EMBEDDING_INDEX_ATTEMPTS: u32 = 5;
/// HTTP attempts per embedding call (initial + two retries).
pub const MAX_EMBEDDING_HTTP_ATTEMPTS: u32 = 3;
/// A response larger than this is rejected without parsing. 8 MiB is enough
/// for 64 vectors at 8,192 dimensions plus JSON overhead.
pub const MAX_EMBEDDING_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
/// Inputs above this count are rejected even if the policy is hand-edited.
pub const MAX_EMBEDDING_BATCH: u32 = 64;

const DEFAULT_RETRY_BASE: Duration = Duration::from_millis(250);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(4);
const INDEX_BACKOFF_BASE_SECS: u64 = 5;
const INDEX_BACKOFF_MAX_SECS: u64 = 3_600;

/// Fully validated runtime form of a project embedding policy. It is built
/// only from `MemoryEmbeddingPolicy`, so the endpoint, model, and bounds are
/// explicit and auditable.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddingRuntimePolicy {
    pub endpoint: String,
    pub model: String,
    pub dimensions: u32,
    pub timeout_secs: u64,
    pub batch_size: u32,
    pub max_candidates: u32,
    pub min_similarity: f64,
    pub api_key_env: Option<String>,
}

impl EmbeddingRuntimePolicy {
    pub fn from_policy(policy: &MemoryEmbeddingPolicy) -> Result<Self> {
        validate_embedding_policy(policy)?;
        if !policy.is_configured() {
            anyhow::bail!("embedding policy needs both endpoint and model");
        }
        let endpoint = normalize_embedding_endpoint(
            policy
                .endpoint
                .as_deref()
                .ok_or_else(|| anyhow!("embedding endpoint is missing"))?,
        )?;
        let model = policy
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .ok_or_else(|| anyhow!("embedding model is missing"))?
            .to_string();
        Ok(Self {
            endpoint,
            model,
            dimensions: policy.dimensions,
            timeout_secs: policy.timeout_secs,
            batch_size: policy.batch_size.clamp(1, MAX_EMBEDDING_BATCH),
            max_candidates: policy.max_candidates.clamp(1, 256),
            min_similarity: f64::from(policy.min_similarity).clamp(0.0, 1.0),
            api_key_env: policy
                .api_key_env
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_string),
        })
    }
}

/// Validate every additive policy field. Invalid or half-configured policies
/// are rejected by `memory.policy.set`; this function is the single source of
/// truth for those checks.
pub fn validate_embedding_policy(policy: &MemoryEmbeddingPolicy) -> Result<()> {
    if !(MIN_EMBEDDING_DIMENSIONS..=MAX_EMBEDDING_DIMENSIONS).contains(&policy.dimensions) {
        anyhow::bail!(
            "embedding dimensions must be between {MIN_EMBEDDING_DIMENSIONS} and {MAX_EMBEDDING_DIMENSIONS}"
        );
    }
    if !(1..=MAX_EMBEDDING_BATCH).contains(&policy.batch_size) {
        anyhow::bail!("embedding batch_size must be between 1 and {MAX_EMBEDDING_BATCH}");
    }
    if !(1..=300).contains(&policy.timeout_secs) {
        anyhow::bail!("embedding timeout_secs must be between 1 and 300");
    }
    if !(1..=256).contains(&policy.max_candidates) {
        anyhow::bail!("embedding max_candidates must be between 1 and 256");
    }
    if !policy.min_similarity.is_finite() || !(0.0..=1.0).contains(&policy.min_similarity) {
        anyhow::bail!("embedding min_similarity must be finite and between 0 and 1");
    }

    let endpoint = policy
        .endpoint
        .as_deref()
        .map(str::trim)
        .filter(|endpoint| !endpoint.is_empty());
    let model = policy
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty());
    if endpoint.is_none() || model.is_none() {
        anyhow::bail!(
            "embedding policy requires both endpoint and model; omit the object for lexical-only memory"
        );
    }
    if let Some(endpoint) = endpoint {
        normalize_embedding_endpoint(endpoint)?;
    }
    if let Some(model) = model {
        if model.chars().count() > 200 {
            anyhow::bail!("embedding model must be 1-200 characters");
        }
        if model.chars().any(char::is_control) || model.chars().any(char::is_whitespace) {
            anyhow::bail!("embedding model must not contain whitespace or control characters");
        }
    }
    if let Some(env_name) = policy
        .api_key_env
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        if env_name.chars().count() > 128 {
            anyhow::bail!("embedding api_key_env must be at most 128 characters");
        }
        let mut chars = env_name.chars();
        let valid_first = chars
            .next()
            .is_some_and(|value| value.is_ascii_alphabetic() || value == '_');
        let valid_rest = chars.all(|value| value.is_ascii_alphanumeric() || value == '_');
        if !valid_first || !valid_rest {
            anyhow::bail!(
                "embedding api_key_env must be an environment variable name (letters, digits, underscore; no value)"
            );
        }
    } else if policy
        .api_key_env
        .as_deref()
        .is_some_and(|name| !name.trim().is_empty())
    {
        anyhow::bail!("embedding api_key_env must not be blank");
    }
    Ok(())
}

/// Normalize a base or full embeddings URL. Query strings, fragments, and
/// userinfo are rejected so credentials cannot be smuggled into a stored or
/// health-reported policy. A URL whose path does not end in `/embeddings`
/// gets that suffix exactly once.
pub fn normalize_embedding_endpoint(raw: &str) -> Result<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        anyhow::bail!("embedding endpoint must not be empty");
    }
    if raw.chars().count() > 2_048 {
        anyhow::bail!("embedding endpoint is too long");
    }
    let url = reqwest::Url::parse(raw).context("embedding endpoint is not a valid URL")?;
    if !matches!(url.scheme(), "http" | "https") {
        anyhow::bail!("embedding endpoint must use http or https");
    }
    if url.host_str().is_none() {
        anyhow::bail!("embedding endpoint must include a host");
    }
    if !url.username().is_empty() || url.password().is_some() {
        anyhow::bail!("embedding endpoint must not include userinfo");
    }
    if url.query().is_some() || url.fragment().is_some() {
        anyhow::bail!("embedding endpoint must not include a query string or fragment");
    }
    let trimmed_path = url.path().trim_end_matches('/').to_string();
    if trimmed_path.is_empty() {
        let mut normalized = url.clone();
        normalized.set_path("/embeddings");
        return Ok(normalized.to_string().trim_end_matches('/').to_string());
    }
    if trimmed_path.ends_with("/embeddings") {
        let mut normalized = url;
        normalized.set_path(&trimmed_path);
        return Ok(normalized.to_string().trim_end_matches('/').to_string());
    }
    let mut normalized = url;
    normalized.set_path(&format!("{trimmed_path}/embeddings"));
    Ok(normalized.to_string())
}

/// One embedding call's usage. If the endpoint omits `usage`, `input_tokens`
/// is a documented `bytes / 4` estimate and `provider_reported` is false.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EmbeddingCallUsage {
    pub calls: u64,
    pub input_tokens: u64,
    pub estimated_tokens: u64,
    pub bytes: u64,
    pub provider_reported: bool,
}

impl EmbeddingCallUsage {
    pub fn estimated_for_texts(texts: &[String]) -> Self {
        let bytes = texts.iter().map(|text| text.len() as u64).sum::<u64>();
        let estimated = bytes.div_ceil(4).max(1);
        Self {
            calls: 1,
            input_tokens: estimated,
            estimated_tokens: estimated,
            bytes,
            provider_reported: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddingItem {
    pub vector: Option<Vec<f32>>,
    /// Sanitized per-item error. A batch can contain both successes and
    /// failures; the reconciler persists each outcome independently.
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddingBatch {
    pub items: Vec<EmbeddingItem>,
    pub usage: EmbeddingCallUsage,
}

#[derive(Debug, thiserror::Error)]
pub enum EmbeddingError {
    #[error("embedding policy is invalid: {0}")]
    InvalidPolicy(String),
    #[error("embedding API key environment variable {0} is not set")]
    MissingApiKey(String),
    #[error("embedding request timed out")]
    Timeout,
    #[error("embedding endpoint returned HTTP {status}: {body}")]
    Status { status: u16, body: String },
    #[error("embedding request failed: {0}")]
    Http(String),
    #[error("embedding response is invalid: {0}")]
    Parse(String),
}

impl EmbeddingError {
    pub fn retryable_http(&self) -> bool {
        match self {
            Self::Timeout => true,
            Self::Http(_) => true,
            Self::Status { status, .. } => retryable_status(*status),
            Self::InvalidPolicy(_) | Self::MissingApiKey(_) | Self::Parse(_) => false,
        }
    }
}

/// Sanitize a diagnostic before it is persisted in `last_error`, emitted in an
/// event, or returned by `memory.embeddings.test`. The request text and API key
/// are never passed here; this additionally redacts common key shapes and
/// bearer tokens that a misbehaving endpoint might echo.
pub fn sanitize_embedding_error(value: &str) -> String {
    let mut output = value.trim().replace(['\n', '\r', '\t'], " ");
    for marker in ["Bearer ", "bearer ", "api_key=", "api-key=", "x-api-key="] {
        let mut search = 0;
        while let Some(relative) = output[search..].find(marker) {
            let index = search + relative;
            let start = index + marker.len();
            let end = output[start..]
                .find(|character: char| {
                    character.is_whitespace() || matches!(character, '"' | '\'' | ',' | '}')
                })
                .map(|offset| start + offset)
                .unwrap_or(output.len());
            if end <= start {
                break;
            }
            output.replace_range(start..end, "[redacted]");
            search = start + "[redacted]".len();
        }
    }
    for marker in ["sk-", "sk_"] {
        let mut search = 0;
        while let Some(relative) = output[search..].find(marker) {
            let index = search + relative;
            let end = output[index..]
                .find(|character: char| {
                    character.is_whitespace() || matches!(character, '"' | '\'' | ',' | '}' | ']')
                })
                .map(|offset| index + offset)
                .unwrap_or(output.len());
            output.replace_range(index..end, "[redacted]");
            search = index + "[redacted]".len();
        }
    }
    if output.chars().count() > 2_000 {
        output = output.chars().take(1_999).collect::<String>();
        output.push('…');
    }
    output
}

fn retryable_status(status: u16) -> bool {
    matches!(status, 408 | 409 | 425 | 429 | 500 | 502 | 503 | 504)
}

fn retry_after(response: &reqwest::Response) -> Option<Duration> {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

fn backoff(attempt: u32, base: Duration) -> Duration {
    base.saturating_mul(1 << attempt.saturating_sub(1).min(4))
        .min(MAX_RETRY_DELAY)
}

/// Daemon-level retry delay for a failed indexing attempt. This is separate
/// from HTTP retries inside one call.
pub fn index_retry_delay(attempts: u32) -> Duration {
    let exponent = attempts.saturating_sub(1).min(10);
    Duration::from_secs(
        INDEX_BACKOFF_BASE_SECS
            .saturating_mul(1u64 << exponent)
            .min(INDEX_BACKOFF_MAX_SECS),
    )
}

/// Explicit embedding client. Construction reads the environment variable
/// named by the policy, failing before any HTTP request if it is absent.
pub struct EmbeddingClient {
    client: reqwest::Client,
    endpoint: String,
    model: String,
    dimensions: u32,
    api_key: Option<String>,
    retry_base: Duration,
}

impl std::fmt::Debug for EmbeddingClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EmbeddingClient")
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .field("dimensions", &self.dimensions)
            .field("api_key", &self.api_key.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

impl EmbeddingClient {
    pub fn from_policy(policy: &MemoryEmbeddingPolicy) -> Result<Self, EmbeddingError> {
        let runtime = EmbeddingRuntimePolicy::from_policy(policy)
            .map_err(|error| EmbeddingError::InvalidPolicy(error.to_string()))?;
        let api_key = match runtime.api_key_env.as_deref() {
            Some(name) => {
                let value = std::env::var(name)
                    .map_err(|_| EmbeddingError::MissingApiKey(name.to_string()))?;
                let value = value.trim().to_string();
                if value.is_empty() {
                    return Err(EmbeddingError::MissingApiKey(name.to_string()));
                }
                Some(value)
            }
            None => None,
        };
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(runtime.timeout_secs.min(15)))
            .timeout(Duration::from_secs(runtime.timeout_secs))
            // A redirect can move an authenticated request to a host the
            // operator did not configure. Fail closed instead of forwarding a
            // bearer token across an unexpected origin.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| EmbeddingError::Http(sanitize_embedding_error(&error.to_string())))?;
        Ok(Self {
            client,
            endpoint: runtime.endpoint,
            model: runtime.model,
            dimensions: runtime.dimensions,
            api_key,
            retry_base: DEFAULT_RETRY_BASE,
        })
    }

    /// Override the retry base for tests and deterministic local checks.
    pub fn with_retry_base(mut self, base: Duration) -> Self {
        self.retry_base = base;
        self
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn dimensions(&self) -> u32 {
        self.dimensions
    }

    /// Embed one bounded batch. Per-item response problems produce item errors;
    /// transport/HTTP/parse failures that make the whole response unusable
    /// return `Err`. The caller records one usage row per successful HTTP call,
    /// including partial batches.
    pub async fn embed(&self, texts: &[String]) -> Result<EmbeddingBatch, EmbeddingError> {
        if texts.is_empty() {
            return Err(EmbeddingError::InvalidPolicy(
                "embedding batch must not be empty".to_string(),
            ));
        }
        if texts.len() > MAX_EMBEDDING_BATCH as usize {
            return Err(EmbeddingError::InvalidPolicy(format!(
                "embedding batch is limited to {MAX_EMBEDDING_BATCH} inputs"
            )));
        }
        let body = json!({
            "model": self.model,
            "input": texts,
            "dimensions": self.dimensions,
            "encoding_format": "float",
        });
        let response = self.send_with_retry(&body).await?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|error| EmbeddingError::Http(sanitize_embedding_error(&error.to_string())))?;
        if bytes.len() > MAX_EMBEDDING_RESPONSE_BYTES {
            return Err(EmbeddingError::Parse(format!(
                "embedding response exceeds {MAX_EMBEDDING_RESPONSE_BYTES} bytes"
            )));
        }
        if !status.is_success() {
            let body = sanitize_embedding_error(&String::from_utf8_lossy(&bytes));
            return Err(EmbeddingError::Status {
                status: status.as_u16(),
                body,
            });
        }
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| EmbeddingError::Parse(format!("invalid JSON: {error}")))?;
        parse_embedding_response(value, texts, self.dimensions)
    }

    async fn send_with_retry(&self, body: &Value) -> Result<reqwest::Response, EmbeddingError> {
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let mut builder = self.client.post(&self.endpoint).json(body);
            if let Some(api_key) = &self.api_key {
                builder = builder.bearer_auth(api_key);
            }
            match builder.send().await {
                Ok(response) if response.status().is_success() => return Ok(response),
                Ok(response) => {
                    let status = response.status();
                    if !retryable_status(status.as_u16()) || attempt >= MAX_EMBEDDING_HTTP_ATTEMPTS
                    {
                        let body = response.text().await.unwrap_or_default();
                        return Err(EmbeddingError::Status {
                            status: status.as_u16(),
                            body: sanitize_embedding_error(&body),
                        });
                    }
                    let delay = retry_after(&response)
                        .unwrap_or_else(|| backoff(attempt, self.retry_base))
                        .min(MAX_RETRY_DELAY);
                    drop(response);
                    tokio::time::sleep(delay).await;
                }
                Err(error) => {
                    let timeout = error.is_timeout();
                    if attempt >= MAX_EMBEDDING_HTTP_ATTEMPTS || !(timeout || error.is_connect()) {
                        return Err(if timeout {
                            EmbeddingError::Timeout
                        } else {
                            EmbeddingError::Http(sanitize_embedding_error(&error.to_string()))
                        });
                    }
                    tokio::time::sleep(backoff(attempt, self.retry_base)).await;
                }
            }
        }
    }
}

fn parse_embedding_response(
    value: Value,
    texts: &[String],
    dimensions: u32,
) -> Result<EmbeddingBatch, EmbeddingError> {
    let Some(data) = value.get("data").and_then(Value::as_array) else {
        return Err(EmbeddingError::Parse(
            "embedding response is missing a data array".to_string(),
        ));
    };
    if data.len() > texts.len().saturating_mul(4).max(16) {
        return Err(EmbeddingError::Parse(
            "embedding response contains too many entries".to_string(),
        ));
    }
    let mut items = (0..texts.len())
        .map(|_| EmbeddingItem {
            vector: None,
            error: None,
        })
        .collect::<Vec<_>>();
    let mut assigned = vec![false; texts.len()];
    let mut positional = data.iter();
    let mut explicit_seen = 0usize;
    // First honour explicit indexes so a well-formed server can return entries
    // out of order.
    for entry in &mut positional {
        let Some(index) = entry.get("index").and_then(Value::as_u64) else {
            continue;
        };
        explicit_seen += 1;
        let index = usize::try_from(index).unwrap_or(usize::MAX);
        if index >= texts.len() {
            return Err(EmbeddingError::Parse(
                "embedding index is out of range".to_string(),
            ));
        }
        if assigned[index] {
            items[index] = EmbeddingItem {
                vector: None,
                error: Some("endpoint returned a duplicate embedding index".to_string()),
            };
            continue;
        }
        assigned[index] = true;
        items[index] = parse_embedding_item(entry, dimensions);
    }
    // Then assign entries without an index to the first unassigned slots, in
    // response order. This preserves partial-batch behavior.
    if explicit_seen == 0 {
        for (index, entry) in data.iter().enumerate().take(texts.len()) {
            assigned[index] = true;
            items[index] = parse_embedding_item(entry, dimensions);
        }
    } else {
        let mut cursor = 0usize;
        for entry in data.iter().filter(|entry| entry.get("index").is_none()) {
            while cursor < assigned.len() && assigned[cursor] {
                cursor += 1;
            }
            if cursor >= assigned.len() {
                break;
            }
            assigned[cursor] = true;
            items[cursor] = parse_embedding_item(entry, dimensions);
            cursor += 1;
        }
    }
    for (index, item) in items.iter_mut().enumerate() {
        if item.vector.is_none() && item.error.is_none() {
            item.error = Some(format!(
                "endpoint omitted an embedding for batch input {index}"
            ));
        }
    }
    let usage = parse_usage(&value, texts);
    Ok(EmbeddingBatch { items, usage })
}

fn parse_embedding_item(entry: &Value, dimensions: u32) -> EmbeddingItem {
    let Some(embedding) = entry.get("embedding") else {
        return EmbeddingItem {
            vector: None,
            error: Some("embedding entry is missing its vector".to_string()),
        };
    };
    let values = if let Some(array) = embedding.as_array() {
        let mut values = Vec::with_capacity(array.len());
        for value in array {
            let Some(number) = value.as_f64() else {
                return EmbeddingItem {
                    vector: None,
                    error: Some("embedding vector contains a non-numeric value".to_string()),
                };
            };
            let number = number as f32;
            if !number.is_finite() {
                return EmbeddingItem {
                    vector: None,
                    error: Some("embedding vector contains a non-finite value".to_string()),
                };
            }
            values.push(number);
        }
        values
    } else if let Some(encoded) = embedding.as_str() {
        match decode_base64_vector(encoded, dimensions) {
            Ok(values) => values,
            Err(error) => {
                return EmbeddingItem {
                    vector: None,
                    error: Some(error),
                };
            }
        }
    } else {
        return EmbeddingItem {
            vector: None,
            error: Some("embedding vector is neither an array nor a base64 string".to_string()),
        };
    };
    if values.len() != dimensions as usize {
        return EmbeddingItem {
            vector: None,
            error: Some(format!(
                "embedding has {} dimensions, expected {dimensions}",
                values.len()
            )),
        };
    }
    if values.iter().all(|value| *value == 0.0) {
        return EmbeddingItem {
            vector: None,
            error: Some("embedding has zero norm".to_string()),
        };
    }
    // Round-trip through the versioned encoder so every stored vector has the
    // exact bytes the decoder and tests expect.
    match encode_embedding_vector(&values) {
        Ok(_) => EmbeddingItem {
            vector: Some(values),
            error: None,
        },
        Err(error) => EmbeddingItem {
            vector: None,
            error: Some(sanitize_embedding_error(&error.to_string())),
        },
    }
}

fn decode_base64_vector(encoded: &str, dimensions: u32) -> Result<Vec<f32>, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .map_err(|_| "embedding base64 payload is invalid".to_string())?;
    let expected = dimensions as usize * 4;
    if bytes.len() != expected {
        return Err(format!(
            "base64 embedding has {} bytes, expected {expected}",
            bytes.len()
        ));
    }
    let mut values = Vec::with_capacity(dimensions as usize);
    for chunk in bytes.as_chunks::<4>().0 {
        let value = f32::from_le_bytes(*chunk);
        if !value.is_finite() {
            return Err("base64 embedding contains a non-finite value".to_string());
        }
        values.push(value);
    }
    Ok(values)
}

fn parse_usage(value: &Value, texts: &[String]) -> EmbeddingCallUsage {
    let reported = value
        .get("usage")
        .and_then(|usage| {
            usage
                .get("prompt_tokens")
                .or_else(|| usage.get("total_tokens"))
                .and_then(Value::as_u64)
        })
        .filter(|tokens| *tokens > 0);
    let bytes = texts.iter().map(|text| text.len() as u64).sum::<u64>();
    match reported {
        Some(tokens) => EmbeddingCallUsage {
            calls: 1,
            input_tokens: tokens,
            estimated_tokens: 0,
            bytes,
            provider_reported: true,
        },
        None => {
            let estimated = bytes.div_ceil(4).max(1);
            EmbeddingCallUsage {
                calls: 1,
                input_tokens: estimated,
                estimated_tokens: estimated,
                bytes,
                provider_reported: false,
            }
        }
    }
}

/// Validate and encode decoded vectors for storage. This is public so the
/// engine and store tests use exactly the same encoding path as the client.
pub fn encode_vectors_for_storage(vectors: &[Vec<f32>]) -> Result<Vec<Vec<u8>>> {
    vectors
        .iter()
        .map(|vector| encode_embedding_vector(vector))
        .collect()
}

/// Decode a stored vector. Thin wrapper kept here so callers do not import
/// both modules for the common path.
pub fn decode_stored_vector(bytes: &[u8], dimensions: u32) -> Result<Vec<f32>> {
    decode_embedding_vector(bytes, dimensions)
}

/// Check a status code without exposing reqwest in callers that only need the
/// retry classification.
pub fn embedding_status_retryable(status: u16) -> bool {
    retryable_status(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    fn policy(endpoint: String) -> MemoryEmbeddingPolicy {
        MemoryEmbeddingPolicy {
            enabled: true,
            endpoint: Some(endpoint),
            model: Some("test-embedding".to_string()),
            dimensions: 8,
            batch_size: 4,
            timeout_secs: 2,
            max_candidates: 16,
            min_similarity: 0.2,
            api_key_env: None,
        }
    }

    fn vector(value: f32) -> Vec<f32> {
        (0..8).map(|index| value + index as f32 * 0.01).collect()
    }

    fn response(vectors: Vec<Vec<f32>>, usage: Option<u64>) -> Value {
        let mut data = Vec::new();
        for (index, vector) in vectors.into_iter().enumerate() {
            data.push(json!({
                "object": "embedding",
                "index": index,
                "embedding": vector,
            }));
        }
        let mut value = json!({"object": "list", "data": data, "model": "test-embedding"});
        if let Some(tokens) = usage {
            value["usage"] = json!({"prompt_tokens": tokens, "total_tokens": tokens});
        }
        value
    }

    fn client(server: &MockServer) -> EmbeddingClient {
        EmbeddingClient::from_policy(&policy(server.uri()))
            .unwrap()
            .with_retry_base(Duration::from_millis(1))
    }

    #[tokio::test]
    async fn embeds_a_batch_with_provider_usage() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(response(vec![vector(0.5), vector(-0.5)], Some(17))),
            )
            .expect(1)
            .mount(&server)
            .await;
        let batch = client(&server)
            .embed(&["alpha".to_string(), "beta".to_string()])
            .await
            .unwrap();
        assert_eq!(batch.items.len(), 2);
        assert_eq!(batch.items[0].vector.as_ref().unwrap().len(), 8);
        assert!(batch.items.iter().all(|item| item.error.is_none()));
        assert_eq!(batch.usage.input_tokens, 17);
        assert_eq!(batch.usage.estimated_tokens, 0);
        assert!(batch.usage.provider_reported);
    }

    #[tokio::test]
    async fn sends_model_dimensions_and_bearer_token() {
        let server = MockServer::start().await;
        let env_name = "VERLINPARK_TEST_EMBED_KEY_SENDS";
        // SAFETY: unique test-local variable; no other test reads this name.
        unsafe { std::env::set_var(env_name, "secret-value") };
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .and(header("authorization", "Bearer secret-value"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(response(vec![vector(0.1)], None)),
            )
            .expect(1)
            .mount(&server)
            .await;
        let mut policy = policy(server.uri());
        policy.api_key_env = Some(env_name.to_string());
        let client = EmbeddingClient::from_policy(&policy).unwrap();
        client.embed(&["hello".to_string()]).await.unwrap();
        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["model"], "test-embedding");
        assert_eq!(body["dimensions"], 8);
        assert_eq!(body["input"][0], "hello");
        unsafe { std::env::remove_var(env_name) };
    }

    #[tokio::test]
    async fn missing_api_key_fails_before_http() {
        let server = MockServer::start().await;
        let mut policy = policy(server.uri());
        policy.api_key_env = Some("VERLINPARK_TEST_EMBED_KEY_ABSENT_9381".to_string());
        let error = EmbeddingClient::from_policy(&policy).unwrap_err();
        assert!(matches!(error, EmbeddingError::MissingApiKey(_)));
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn dimension_mismatch_is_a_per_item_error_and_partial_batch_survives() {
        let server = MockServer::start().await;
        let mut bad = vector(0.4);
        bad.push(0.9);
        let mut data = json!({
            "data": [
                {"index": 0, "embedding": vector(0.2)},
                {"index": 1, "embedding": bad},
            ]
        });
        data["usage"] = json!({"prompt_tokens": 3});
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(data))
            .mount(&server)
            .await;
        let batch = client(&server)
            .embed(&["one".to_string(), "two".to_string()])
            .await
            .unwrap();
        assert!(batch.items[0].vector.is_some());
        assert!(
            batch.items[1]
                .error
                .as_deref()
                .unwrap()
                .contains("expected 8")
        );
    }

    #[tokio::test]
    async fn missing_response_entry_is_a_per_item_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{"index": 0, "embedding": vector(0.3)}]
            })))
            .mount(&server)
            .await;
        let batch = client(&server)
            .embed(&["one".to_string(), "two".to_string()])
            .await
            .unwrap();
        assert!(batch.items[0].vector.is_some());
        assert!(batch.items[1].error.as_deref().unwrap().contains("omitted"));
    }

    #[tokio::test]
    async fn malformed_vectors_are_rejected_individually() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [
                    {"index": 0, "embedding": "not-base64!!!"},
                    {"index": 1, "embedding": [0, 0, 0, 0, 0, 0, 0, 0]},
                    {"index": 2, "embedding": [1, "x", 0, 0, 0, 0, 0, 0]},
                ]
            })))
            .mount(&server)
            .await;
        let batch = client(&server)
            .embed(&["a".to_string(), "b".to_string(), "c".to_string()])
            .await
            .unwrap();
        assert!(batch.items.iter().all(|item| item.vector.is_none()));
        assert!(batch.items[0].error.as_deref().unwrap().contains("base64"));
        assert!(
            batch.items[1]
                .error
                .as_deref()
                .unwrap()
                .contains("zero norm")
        );
        assert!(
            batch.items[2]
                .error
                .as_deref()
                .unwrap()
                .contains("non-numeric")
        );
    }

    #[tokio::test]
    async fn timeout_is_reported_without_partial_data() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(response(vec![vector(0.1)], None))
                    .set_delay(Duration::from_millis(1_500)),
            )
            .mount(&server)
            .await;
        let mut policy = policy(server.uri());
        policy.timeout_secs = 1;
        let client = EmbeddingClient::from_policy(&policy)
            .unwrap()
            .with_retry_base(Duration::from_millis(1));
        let error = client.embed(&["slow".to_string()]).await.unwrap_err();
        assert!(matches!(error, EmbeddingError::Timeout));
    }

    #[tokio::test]
    async fn retry_after_and_server_errors_retry_then_succeed() {
        let server = MockServer::start().await;
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_clone = attempts.clone();
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(move |_request: &Request| {
                let attempt = attempts_clone.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    ResponseTemplate::new(429)
                        .insert_header("retry-after", "0")
                        .set_body_string("rate limited")
                } else if attempt == 1 {
                    ResponseTemplate::new(503).set_body_string("busy")
                } else {
                    ResponseTemplate::new(200).set_body_json(response(vec![vector(0.6)], None))
                }
            })
            .mount(&server)
            .await;
        let batch = client(&server).embed(&["retry".to_string()]).await.unwrap();
        assert!(batch.items[0].vector.is_some());
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn persistent_server_errors_exhaust_retries() {
        let server = MockServer::start().await;
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_clone = attempts.clone();
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(move |_request: &Request| {
                attempts_clone.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(500).set_body_string("nope")
            })
            .mount(&server)
            .await;
        let error = client(&server)
            .embed(&["fail".to_string()])
            .await
            .unwrap_err();
        assert!(matches!(error, EmbeddingError::Status { status: 500, .. }));
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn auth_errors_are_not_retried() {
        let server = MockServer::start().await;
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_clone = attempts.clone();
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(move |_request: &Request| {
                attempts_clone.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(401).set_body_string("bad key")
            })
            .mount(&server)
            .await;
        let error = client(&server)
            .embed(&["auth".to_string()])
            .await
            .unwrap_err();
        assert!(matches!(error, EmbeddingError::Status { status: 401, .. }));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn usage_absent_records_an_honest_estimate() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(response(vec![vector(0.1)], None)),
            )
            .mount(&server)
            .await;
        let batch = client(&server)
            .embed(&["12345678".to_string()])
            .await
            .unwrap();
        assert_eq!(batch.usage.calls, 1);
        assert_eq!(batch.usage.bytes, 8);
        assert_eq!(batch.usage.estimated_tokens, 2);
        assert_eq!(batch.usage.input_tokens, 2);
        assert!(!batch.usage.provider_reported);
    }

    #[tokio::test]
    async fn base64_vectors_are_supported() {
        let server = MockServer::start().await;
        let values = vector(0.25);
        let bytes = values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect::<Vec<_>>();
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{"index": 0, "embedding": encoded}],
                "usage": {"prompt_tokens": 6}
            })))
            .mount(&server)
            .await;
        let batch = client(&server)
            .embed(&["base64".to_string()])
            .await
            .unwrap();
        let decoded = batch.items[0].vector.as_ref().unwrap();
        for (left, right) in decoded.iter().zip(&values) {
            assert_eq!(left, right);
        }
    }

    #[tokio::test]
    async fn missing_data_is_a_parse_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"object": "list"})))
            .mount(&server)
            .await;
        let error = client(&server).embed(&["x".to_string()]).await.unwrap_err();
        assert!(matches!(error, EmbeddingError::Parse(_)));
    }

    #[tokio::test]
    async fn out_of_range_index_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{"index": 8, "embedding": vector(0.1)}]
            })))
            .mount(&server)
            .await;
        let error = client(&server).embed(&["x".to_string()]).await.unwrap_err();
        assert!(matches!(error, EmbeddingError::Parse(_)));
    }

    #[test]
    fn validates_bounds_and_incomplete_configuration() {
        let mut policy = MemoryEmbeddingPolicy::default();
        assert!(
            validate_embedding_policy(&policy).is_err(),
            "an embedding object without endpoint/model is incomplete"
        );
        policy.endpoint = Some("https://example.com/v1".to_string());
        assert!(validate_embedding_policy(&policy).is_err());
        policy.model = Some("embed".to_string());
        assert!(validate_embedding_policy(&policy).is_ok());
        policy.enabled = true;
        assert!(validate_embedding_policy(&policy).is_ok());

        policy.dimensions = 1;
        assert!(validate_embedding_policy(&policy).is_err());
        policy.dimensions = 8;
        policy.batch_size = 0;
        assert!(validate_embedding_policy(&policy).is_err());
        policy.batch_size = 16;
        policy.timeout_secs = 0;
        assert!(validate_embedding_policy(&policy).is_err());
        policy.timeout_secs = 30;
        policy.max_candidates = 0;
        assert!(validate_embedding_policy(&policy).is_err());
        policy.max_candidates = 64;
        policy.min_similarity = f32::NAN;
        assert!(validate_embedding_policy(&policy).is_err());
    }

    #[test]
    fn endpoint_normalization_is_explicit_and_secret_free() {
        assert_eq!(
            normalize_embedding_endpoint("https://api.example.com/v1").unwrap(),
            "https://api.example.com/v1/embeddings"
        );
        assert_eq!(
            normalize_embedding_endpoint("https://api.example.com/v1/embeddings/").unwrap(),
            "https://api.example.com/v1/embeddings"
        );
        assert_eq!(
            normalize_embedding_endpoint("http://127.0.0.1:8080").unwrap(),
            "http://127.0.0.1:8080/embeddings"
        );
        assert!(normalize_embedding_endpoint("ftp://example.com").is_err());
        assert!(normalize_embedding_endpoint("https://user:pass@example.com").is_err());
        assert!(normalize_embedding_endpoint("https://example.com/?key=secret").is_err());
        assert!(normalize_embedding_endpoint("https://example.com/#frag").is_err());
        assert_eq!(
            normalize_embedding_endpoint("  https://example.com/v1/  ").unwrap(),
            "https://example.com/v1/embeddings"
        );
    }

    #[test]
    fn api_key_env_is_a_name_not_a_secret() {
        let mut policy = MemoryEmbeddingPolicy {
            endpoint: Some("https://example.com/v1".to_string()),
            model: Some("embed".to_string()),
            ..MemoryEmbeddingPolicy::default()
        };
        policy.api_key_env = Some("OPENAI_API_KEY".to_string());
        assert!(validate_embedding_policy(&policy).is_ok());
        policy.api_key_env = Some("sk-live-secret".to_string());
        assert!(validate_embedding_policy(&policy).is_err());
        policy.api_key_env = Some("1STARTS_WITH_DIGIT".to_string());
        assert!(validate_embedding_policy(&policy).is_err());
    }

    #[test]
    fn error_sanitizer_redacts_key_shapes() {
        let sanitized = sanitize_embedding_error(
            "failed: Authorization Bearer sk-abc123 and api_key=supersecret\nretry",
        );
        assert!(!sanitized.contains("sk-abc123"));
        assert!(!sanitized.contains("supersecret"));
        assert!(sanitized.contains("[redacted]"));
        assert!(!sanitized.contains('\n'));
    }

    #[test]
    fn index_backoff_is_bounded() {
        assert_eq!(index_retry_delay(1), Duration::from_secs(5));
        assert_eq!(index_retry_delay(2), Duration::from_secs(10));
        assert_eq!(index_retry_delay(3), Duration::from_secs(20));
        assert_eq!(index_retry_delay(20), Duration::from_secs(3_600));
    }

    #[test]
    fn status_retry_classification_is_stable() {
        for status in [408, 409, 425, 429, 500, 502, 503, 504] {
            assert!(embedding_status_retryable(status));
        }
        for status in [200, 400, 401, 403, 404, 422] {
            assert!(!embedding_status_retryable(status));
        }
    }

    #[test]
    fn zero_vectors_are_rejected_by_storage_encoding_too() {
        let vector = vec![0.0f32; 8];
        assert!(encode_embedding_vector(&vector).is_ok());
        let encoded = encode_embedding_vector(&vector).unwrap();
        assert_eq!(decode_embedding_vector(&encoded, 8).unwrap(), vector);
        let bad = vec![f32::INFINITY; 8];
        assert!(encode_embedding_vector(&bad).is_err());
    }

    #[tokio::test]
    async fn explicit_indexes_may_arrive_out_of_order() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [
                    {"index": 1, "embedding": vector(0.2)},
                    {"index": 0, "embedding": vector(0.9)},
                ]
            })))
            .mount(&server)
            .await;
        let batch = client(&server)
            .embed(&["first".to_string(), "second".to_string()])
            .await
            .unwrap();
        assert!((batch.items[0].vector.as_ref().unwrap()[0] - 0.9).abs() < 1e-6);
        assert!((batch.items[1].vector.as_ref().unwrap()[0] - 0.2).abs() < 1e-6);
    }

    #[tokio::test]
    async fn duplicate_index_fails_the_slot_but_keeps_other_items() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [
                    {"index": 0, "embedding": vector(0.1)},
                    {"index": 0, "embedding": vector(0.2)},
                    {"index": 1, "embedding": vector(0.3)},
                ]
            })))
            .mount(&server)
            .await;
        let batch = client(&server)
            .embed(&["one".to_string(), "two".to_string()])
            .await
            .unwrap();
        assert!(batch.items[0].vector.is_none());
        assert!(
            batch.items[0]
                .error
                .as_deref()
                .unwrap()
                .contains("duplicate")
        );
        assert!(batch.items[1].vector.is_some());
    }

    #[tokio::test]
    async fn mixed_explicit_and_positional_entries_fill_remaining_slots() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [
                    {"index": 0, "embedding": vector(0.1)},
                    {"embedding": vector(0.8)},
                ]
            })))
            .mount(&server)
            .await;
        let batch = client(&server)
            .embed(&["one".to_string(), "two".to_string()])
            .await
            .unwrap();
        assert!(batch.items[0].vector.is_some());
        assert!(batch.items[1].vector.is_some());
        assert!((batch.items[1].vector.as_ref().unwrap()[0] - 0.8).abs() < 1e-6);
    }

    #[tokio::test]
    async fn total_tokens_is_accepted_when_prompt_tokens_is_absent() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{"index": 0, "embedding": vector(0.4)}],
                "usage": {"total_tokens": 21}
            })))
            .mount(&server)
            .await;
        let batch = client(&server).embed(&["usage".to_string()]).await.unwrap();
        assert_eq!(batch.usage.input_tokens, 21);
        assert_eq!(batch.usage.estimated_tokens, 0);
        assert!(batch.usage.provider_reported);
    }

    #[tokio::test]
    async fn missing_embedding_field_is_a_per_item_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{"index": 0}]
            })))
            .mount(&server)
            .await;
        let batch = client(&server)
            .embed(&["missing".to_string()])
            .await
            .unwrap();
        assert_eq!(batch.items.len(), 1);
        assert!(
            batch.items[0]
                .error
                .as_deref()
                .unwrap()
                .contains("missing its vector")
        );
    }

    #[tokio::test]
    async fn base64_with_wrong_byte_length_is_rejected() {
        let server = MockServer::start().await;
        let encoded = base64::engine::general_purpose::STANDARD.encode([0u8; 8]);
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{"index": 0, "embedding": encoded}]
            })))
            .mount(&server)
            .await;
        let batch = client(&server).embed(&["short".to_string()]).await.unwrap();
        assert!(
            batch.items[0]
                .error
                .as_deref()
                .unwrap()
                .contains("expected 32")
        );
    }

    #[tokio::test]
    async fn empty_batch_and_oversized_batch_are_rejected_before_http() {
        let server = MockServer::start().await;
        let client = client(&server);
        let empty = client.embed(&[]).await.unwrap_err();
        assert!(matches!(empty, EmbeddingError::InvalidPolicy(_)));
        let oversized = vec!["x".to_string(); MAX_EMBEDDING_BATCH as usize + 1];
        let error = client.embed(&oversized).await.unwrap_err();
        assert!(matches!(error, EmbeddingError::InvalidPolicy(_)));
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[test]
    fn runtime_policy_trims_blank_key_names_and_clamps_bounds() {
        let policy = MemoryEmbeddingPolicy {
            endpoint: Some("https://example.com/v1".to_string()),
            model: Some("embed".to_string()),
            batch_size: 4,
            max_candidates: 12,
            min_similarity: 0.4,
            api_key_env: Some("   ".to_string()),
            ..MemoryEmbeddingPolicy::default()
        };
        let runtime = EmbeddingRuntimePolicy::from_policy(&policy).unwrap();
        assert!(runtime.api_key_env.is_none());
        assert_eq!(runtime.batch_size, 4);
        assert_eq!(runtime.max_candidates, 12);
        assert!((runtime.min_similarity - 0.4).abs() < 1e-6);
    }

    #[test]
    fn sanitizer_caps_error_length() {
        let long = format!("prefix {}", "x".repeat(5_000));
        let sanitized = sanitize_embedding_error(&long);
        assert!(sanitized.chars().count() <= 2_000);
        assert!(sanitized.ends_with('…'));
    }

    #[test]
    fn estimation_rounds_up_and_never_reports_zero_tokens() {
        let usage = EmbeddingCallUsage::estimated_for_texts(&[]);
        assert_eq!(usage.input_tokens, 1);
        assert_eq!(usage.bytes, 0);
        let usage = EmbeddingCallUsage::estimated_for_texts(&["abcde".to_string()]);
        assert_eq!(usage.bytes, 5);
        assert_eq!(usage.estimated_tokens, 2);
        assert!(!usage.provider_reported);
    }

    #[test]
    fn non_success_outside_retry_set_has_no_retry() {
        let error = EmbeddingError::Status {
            status: 422,
            body: "bad dimensions".to_string(),
        };
        assert!(!error.retryable_http());
        let error = EmbeddingError::Status {
            status: 503,
            body: "busy".to_string(),
        };
        assert!(error.retryable_http());
        assert!(!EmbeddingError::Parse("bad".to_string()).retryable_http());
    }

    #[tokio::test]
    async fn extra_unindexed_entries_are_ignored_without_index() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [
                    {"embedding": vector(0.1)},
                    {"embedding": vector(0.2)},
                    {"embedding": vector(0.3)},
                ]
            })))
            .mount(&server)
            .await;
        let batch = client(&server)
            .embed(&["one".to_string(), "two".to_string()])
            .await
            .unwrap();
        assert_eq!(batch.items.len(), 2);
        assert!(batch.items.iter().all(|item| item.vector.is_some()));
    }

    #[tokio::test]
    async fn zero_usage_token_count_falls_back_to_an_estimate() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{"index": 0, "embedding": vector(0.1)}],
                "usage": {"prompt_tokens": 0, "total_tokens": 0}
            })))
            .mount(&server)
            .await;
        let batch = client(&server)
            .embed(&["12345678".to_string()])
            .await
            .unwrap();
        assert!(!batch.usage.provider_reported);
        assert_eq!(batch.usage.estimated_tokens, 2);
        assert_eq!(batch.usage.input_tokens, 2);
    }

    #[tokio::test]
    async fn base64_payload_ignores_surrounding_whitespace() {
        let server = MockServer::start().await;
        let encoded = {
            let values = vector(0.5);
            let bytes = values
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect::<Vec<_>>();
            format!(
                "  {}  ",
                base64::engine::general_purpose::STANDARD.encode(bytes)
            )
        };
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{"index": 0, "embedding": encoded}]
            })))
            .mount(&server)
            .await;
        let batch = client(&server)
            .embed(&["base64".to_string()])
            .await
            .unwrap();
        assert!(batch.items[0].vector.is_some());
    }

    #[test]
    fn endpoint_keeps_existing_path_prefixes() {
        assert_eq!(
            normalize_embedding_endpoint("https://example.com").unwrap(),
            "https://example.com/embeddings"
        );
        assert_eq!(
            normalize_embedding_endpoint("https://example.com/openai/v1/").unwrap(),
            "https://example.com/openai/v1/embeddings"
        );
        assert_eq!(
            normalize_embedding_endpoint("https://example.com/openai/v1/embeddings/").unwrap(),
            "https://example.com/openai/v1/embeddings"
        );
    }

    #[test]
    fn policy_rejects_whitespace_model_and_overlong_endpoint() {
        let mut policy = MemoryEmbeddingPolicy {
            endpoint: Some("https://example.com/v1".to_string()),
            model: Some("embed model".to_string()),
            ..MemoryEmbeddingPolicy::default()
        };
        let error = validate_embedding_policy(&policy).unwrap_err();
        assert!(error.to_string().contains("whitespace"));
        policy.model = Some("embed-model".to_string());
        assert!(validate_embedding_policy(&policy).is_ok());
        policy.endpoint = Some(format!("https://example.com/{}", "x".repeat(3_000)));
        let error = validate_embedding_policy(&policy).unwrap_err();
        assert!(error.to_string().contains("too long"));
    }

    #[test]
    fn status_errors_display_without_html_or_newlines() {
        let error = EmbeddingError::Status {
            status: 503,
            body: sanitize_embedding_error("line one\nline two\t<b>busy</b>"),
        };
        let message = error.to_string();
        assert!(message.contains("HTTP 503"));
        assert!(!message.contains('\n'));
        assert!(!message.contains('\t'));
        assert!(message.contains("line one line two"));
    }

    #[tokio::test]
    async fn authenticated_redirects_are_not_followed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(
                ResponseTemplate::new(307)
                    .insert_header("location", format!("{}/other", server.uri())),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/other"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(response(vec![vector(0.1)], None)),
            )
            .mount(&server)
            .await;
        let error = client(&server)
            .embed(&["redirect".to_string()])
            .await
            .unwrap_err();
        assert!(matches!(error, EmbeddingError::Status { status: 307, .. }));
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1, "redirect target must not be requested");
        assert_eq!(requests[0].url.path(), "/embeddings");
    }
}

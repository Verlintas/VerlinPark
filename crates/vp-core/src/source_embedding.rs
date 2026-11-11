//! Explicit, operator-configured semantic lane for project knowledge sources.
//!
//! This module owns the deterministic parts of source retrieval:
//!
//! - policy normalization/validation (endpoint and key are environment *names*)
//! - exact chunk/query text composition and content hashes
//! - availability/fallback reason codes
//! - exact-cosine candidate ranking and deterministic reciprocal-rank fusion
//!
//! It deliberately does **not** select a model, route between providers, or
//! enable itself. The daemon calls it only for a project whose
//! [`SourceEmbeddingPolicy`] is explicitly enabled and configured. Vectors are
//! derived state: they are never returned by a tool, RPC, event, log, or UI.
//!
//! Source embeddings are separate from project memory embeddings. They have a
//! different text contract (title + heading + chunk body), a different job
//! queue/generation model, a different usage table, and their own retention.
//! Sharing the low-level OpenAI-compatible HTTP client is intentional; sharing
//! accounting or generation state is not.

use std::collections::HashMap;

use anyhow::Result;
use vp_protocol::types::{SourceChunk, SourceEmbeddingPolicy, SourceSearchHit, SourceSearchMode};

use crate::memory::{
    EMBEDDING_VECTOR_HEADER_BYTES, MAX_EMBEDDING_DIMENSIONS, MIN_EMBEDDING_DIMENSIONS,
    cosine_similarity, decode_embedding_vector, encode_embedding_vector,
};
use crate::memory_embedding::{
    EmbeddingBatch, EmbeddingClient, EmbeddingError, sanitize_embedding_error,
};

/// The derived source index never embeds more than 500 chunks of one source.
/// The lexical index still covers every chunk, so this cap only narrows the
/// optional semantic lane.
pub const MAX_SOURCE_EMBEDDING_CHUNKS: u32 = 500;
/// Hard request bound even for a hand-edited policy/database.
pub const MAX_SOURCE_EMBEDDING_BATCH: u32 = 64;
pub const MAX_SOURCE_EMBEDDING_RETENTION_DAYS: u32 = 3_650;
pub const MAX_SOURCE_EMBEDDING_TIMEOUT_SECS: u64 = 300;

/// Exact text sent for one chunk is capped so a pathological source cannot
/// create an unbounded request. The cap is UTF-8 safe and deterministic.
pub const SOURCE_EMBEDDING_TEXT_MAX_CHARS: usize = 8_000;
pub const SOURCE_EMBEDDING_BODY_MAX_CHARS: usize = 7_200;
pub const SOURCE_EMBEDDING_QUERY_MAX_CHARS: usize = 4_000;
/// Title/heading are part of the embedded text but independently capped.
pub const SOURCE_EMBEDDING_TITLE_MAX_CHARS: usize = 400;
pub const SOURCE_EMBEDDING_HEADING_MAX_CHARS: usize = 800;

/// Exact scan bound. This is a bounded candidate window, not an ANN index;
/// source retrieval intentionally makes no approximate-nearest-neighbour
/// claim this round. Increasing this value increases query CPU linearly.
pub const MAX_SOURCE_SEMANTIC_CANDIDATES: u32 = 512;
/// Candidate window used for lexical recall during hybrid fusion. The final
/// result limit remains caller-controlled.
pub const MAX_SOURCE_LEXICAL_CANDIDATES: u32 = 50;
/// Cosine similarities below this are treated as no semantic evidence.
pub const MIN_SOURCE_SIMILARITY: f64 = 0.05;
/// RRF constant shared with memory retrieval so ranking math is auditable in
/// one place conceptually. Two first places normalize to 1.0.
pub const SOURCE_RRF_K: f64 = 10.0;

/// Retry budget for the source reconciler. HTTP retries inside one call are
/// separately bounded by the shared embedding client.
pub const MAX_SOURCE_EMBEDDING_ATTEMPTS: u32 = 5;
/// A running job older than this may be reclaimed by a later reconciler tick.
pub const SOURCE_EMBEDDING_JOB_STALE_SECS: u64 = 900;
/// Jobs processed per project per daemon tick. Each job is additionally bounded
/// by `batch_size` chunks, so one tick cannot fan out to a whole corpus.
pub const MAX_SOURCE_EMBEDDING_JOBS_PER_TICK: u32 = 2;

/// Fully validated runtime form of a source embedding policy. The endpoint is
/// resolved from the environment at call time, never persisted in this struct.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceEmbeddingRuntimePolicy {
    pub endpoint: String,
    pub model: String,
    pub dimensions: u32,
    pub timeout_secs: u64,
    pub batch_size: u32,
    pub max_chunks_per_source: u32,
    pub retention_days: u32,
    pub api_key_env: Option<String>,
}

/// A stable, sanitized reason why the semantic lane cannot contribute. The
/// `reason` is safe to put in an RPC/CLI/UI payload: it contains a code and at
/// most an environment variable *name*, never a value, response body, or
/// endpoint URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSemanticUnavailable {
    pub reason: String,
}

impl SourceSemanticUnavailable {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: sanitize_reason(&reason.into()),
        }
    }
}

impl std::fmt::Display for SourceSemanticUnavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.reason)
    }
}

impl std::error::Error for SourceSemanticUnavailable {}

/// Explicit embedding client for source chunks. Construction reads the
/// endpoint and optional key environment variables and fails before any HTTP
/// request when either is absent.
pub struct SourceEmbeddingClient {
    inner: EmbeddingClient,
    model: String,
    dimensions: u32,
}

impl std::fmt::Debug for SourceEmbeddingClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SourceEmbeddingClient")
            .field("model", &self.model)
            .field("dimensions", &self.dimensions)
            .field("inner", &self.inner)
            .finish()
    }
}

impl SourceEmbeddingClient {
    pub fn from_policy(
        policy: &SourceEmbeddingPolicy,
    ) -> std::result::Result<Self, SourceSemanticUnavailable> {
        let runtime = SourceEmbeddingRuntimePolicy::from_policy(policy)?;
        let memory_policy = vp_protocol::types::MemoryEmbeddingPolicy {
            enabled: true,
            endpoint: Some(runtime.endpoint.clone()),
            model: Some(runtime.model.clone()),
            dimensions: runtime.dimensions,
            batch_size: runtime.batch_size,
            timeout_secs: runtime.timeout_secs,
            max_candidates: 64,
            min_similarity: 0.0,
            api_key_env: runtime.api_key_env.clone(),
            ..vp_protocol::types::MemoryEmbeddingPolicy::default()
        };
        let inner = EmbeddingClient::from_policy(&memory_policy).map_err(|error| {
            let reason = match &error {
                EmbeddingError::MissingApiKey(name) => {
                    format!("api_key_env_missing:{name}")
                }
                EmbeddingError::InvalidPolicy(_) => "invalid_policy".to_string(),
                _ => source_embedding_error_code(&error),
            };
            SourceSemanticUnavailable::new(reason)
        })?;
        Ok(Self {
            inner,
            model: runtime.model,
            dimensions: runtime.dimensions,
        })
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn dimensions(&self) -> u32 {
        self.dimensions
    }

    pub async fn embed(
        &self,
        texts: &[String],
    ) -> std::result::Result<EmbeddingBatch, EmbeddingError> {
        self.inner.embed(texts).await
    }

    pub async fn embed_one(&self, text: String) -> std::result::Result<Vec<f32>, String> {
        let batch = self
            .embed(std::slice::from_ref(&text))
            .await
            .map_err(|error| source_embedding_error_code(&error))?;
        let Some(item) = batch.items.into_iter().next() else {
            return Err("invalid_response".to_string());
        };
        if let Some(error) = item.error {
            return Err(source_embedding_item_error_code(&error));
        }
        item.vector.ok_or_else(|| "invalid_response".to_string())
    }
}

impl SourceEmbeddingRuntimePolicy {
    pub fn from_policy(
        policy: &SourceEmbeddingPolicy,
    ) -> std::result::Result<Self, SourceSemanticUnavailable> {
        if !policy.enabled {
            return Err(SourceSemanticUnavailable::new("disabled"));
        }
        if !policy.is_configured() {
            return Err(SourceSemanticUnavailable::new("not_configured"));
        }
        validate_source_embedding_policy(policy)
            .map_err(|error| SourceSemanticUnavailable::new(format!("invalid_policy: {error}")))?;
        let endpoint_env = policy
            .endpoint_env_name()
            .ok_or_else(|| SourceSemanticUnavailable::new("not_configured"))?;
        let endpoint = std::env::var(endpoint_env).map_err(|_| {
            SourceSemanticUnavailable::new(format!("endpoint_env_missing:{endpoint_env}"))
        })?;
        let endpoint = endpoint.trim().to_string();
        if endpoint.is_empty() {
            return Err(SourceSemanticUnavailable::new(format!(
                "endpoint_env_missing:{endpoint_env}"
            )));
        }
        let endpoint = crate::memory_embedding::normalize_embedding_endpoint(&endpoint)
            .map_err(|_| SourceSemanticUnavailable::new("invalid_endpoint"))?;
        let model = policy
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .ok_or_else(|| SourceSemanticUnavailable::new("not_configured"))?
            .to_string();
        Ok(Self {
            endpoint,
            model,
            dimensions: policy.dimensions,
            timeout_secs: policy
                .timeout_secs
                .clamp(1, MAX_SOURCE_EMBEDDING_TIMEOUT_SECS),
            batch_size: policy.batch_size.clamp(1, MAX_SOURCE_EMBEDDING_BATCH),
            max_chunks_per_source: policy
                .max_chunks_per_source
                .clamp(1, MAX_SOURCE_EMBEDDING_CHUNKS),
            retention_days: policy
                .retention_days
                .min(MAX_SOURCE_EMBEDDING_RETENTION_DAYS),
            api_key_env: policy.api_key_env_name().map(str::to_string),
        })
    }
}

/// Trim and normalize additive policy fields. Blank names become `None`; URL
/// values are never accepted because `endpoint_env` is a variable name.
pub fn normalize_source_embedding_policy(policy: &mut SourceEmbeddingPolicy) {
    policy.endpoint_env = policy
        .endpoint_env
        .take()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    policy.model = policy
        .model
        .take()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    policy.api_key_env = policy
        .api_key_env
        .take()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
}

/// Validate every additive policy field. This is the single validation path
/// used by `source.embedding.policy.set`, the manifest rebuild, and runtime
/// construction.
pub fn validate_source_embedding_policy(policy: &SourceEmbeddingPolicy) -> Result<()> {
    if !(MIN_EMBEDDING_DIMENSIONS..=MAX_EMBEDDING_DIMENSIONS).contains(&policy.dimensions) {
        anyhow::bail!(
            "source embedding dimensions must be between {MIN_EMBEDDING_DIMENSIONS} and {MAX_EMBEDDING_DIMENSIONS}"
        );
    }
    if !(1..=MAX_SOURCE_EMBEDDING_BATCH).contains(&policy.batch_size) {
        anyhow::bail!(
            "source embedding batch_size must be between 1 and {MAX_SOURCE_EMBEDDING_BATCH}"
        );
    }
    if !(1..=MAX_SOURCE_EMBEDDING_CHUNKS).contains(&policy.max_chunks_per_source) {
        anyhow::bail!(
            "source embedding max_chunks_per_source must be between 1 and {MAX_SOURCE_EMBEDDING_CHUNKS}"
        );
    }
    if policy.retention_days > MAX_SOURCE_EMBEDDING_RETENTION_DAYS {
        anyhow::bail!(
            "source embedding retention_days must be between 0 and {MAX_SOURCE_EMBEDDING_RETENTION_DAYS}"
        );
    }
    if !(1..=MAX_SOURCE_EMBEDDING_TIMEOUT_SECS).contains(&policy.timeout_secs) {
        anyhow::bail!(
            "source embedding timeout_secs must be between 1 and {MAX_SOURCE_EMBEDDING_TIMEOUT_SECS}"
        );
    }
    if let Some(name) = policy
        .endpoint_env
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        validate_environment_name(name, "source embedding endpoint_env")?;
    }
    if let Some(name) = policy
        .api_key_env
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        validate_environment_name(name, "source embedding api_key_env")?;
    }
    let model = policy
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty());
    if let Some(model) = model {
        if model.chars().count() > 200 {
            anyhow::bail!("source embedding model must be 1-200 characters");
        }
        if model.chars().any(char::is_control) || model.chars().any(char::is_whitespace) {
            anyhow::bail!(
                "source embedding model must not contain whitespace or control characters"
            );
        }
    }
    if policy.enabled && !policy.is_configured() {
        anyhow::bail!(
            "enabled source embedding policy requires endpoint_env and model; disable it or omit those fields"
        );
    }
    Ok(())
}

fn validate_environment_name(name: &str, field: &str) -> Result<()> {
    if name.chars().count() > 128 {
        anyhow::bail!("{field} must be at most 128 characters");
    }
    let mut chars = name.chars();
    let valid_first = chars
        .next()
        .is_some_and(|value| value.is_ascii_alphabetic() || value == '_');
    let valid_rest = chars.all(|value| value.is_ascii_alphanumeric() || value == '_');
    if !valid_first || !valid_rest {
        anyhow::bail!("{field} must be an environment variable name (no value)");
    }
    Ok(())
}

/// Stable code for a whole-request embedding failure. Response bodies are not
/// copied into retrieval telemetry.
pub fn source_embedding_error_code(error: &EmbeddingError) -> String {
    match error {
        EmbeddingError::InvalidPolicy(_) => "invalid_policy".to_string(),
        EmbeddingError::MissingApiKey(name) => format!("api_key_env_missing:{name}"),
        EmbeddingError::Timeout => "timeout".to_string(),
        EmbeddingError::Status { status, .. } => format!("http_{status}"),
        EmbeddingError::Http(_) => "connection_failed".to_string(),
        EmbeddingError::Parse(_) => "invalid_response".to_string(),
    }
}

/// Per-item response errors are classified conservatively. Dimension and
/// encoding mismatches will never heal by retrying the same request, so they
/// dead-letter immediately; transient endpoint issues are retried.
pub fn source_embedding_item_error_code(error: &str) -> String {
    let error = sanitize_embedding_error(error);
    let lower = error.to_ascii_lowercase();
    if lower.contains("dimension")
        || lower.contains("zero norm")
        || lower.contains("non-finite")
        || lower.contains("non-numeric")
        || lower.contains("base64")
        || lower.contains("neither an array")
    {
        format!("invalid_embedding: {}", truncate_chars(&error, 200))
    } else {
        format!("item_failed: {}", truncate_chars(&error, 200))
    }
}

pub fn source_embedding_item_error_retryable(error: &str) -> bool {
    !source_embedding_item_error_code(error).starts_with("invalid_embedding:")
}

/// Stable availability reason for status and search preflight. `indexed` is a
/// current-generation vector count; `stale` is older-generation/status-stale
/// rows. An empty corpus is "no_indexed_vectors", not an error.
pub fn source_semantic_unavailable(
    policy: Option<&SourceEmbeddingPolicy>,
    indexed: u32,
    stale: u32,
) -> Option<String> {
    let Some(policy) = policy else {
        return Some("disabled".to_string());
    };
    if !policy.enabled {
        return Some("disabled".to_string());
    }
    if !policy.is_configured() {
        return Some("not_configured".to_string());
    }
    if let Some(name) = policy.endpoint_env_name() {
        let value = std::env::var(name).ok();
        let present = value
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
        if !present {
            return Some(format!("endpoint_env_missing:{name}"));
        }
        if value.as_deref().is_some_and(|value| {
            crate::memory_embedding::normalize_embedding_endpoint(value).is_err()
        }) {
            return Some("invalid_endpoint".to_string());
        }
    }
    if let Some(name) = policy.api_key_env_name() {
        let present = std::env::var(name)
            .ok()
            .is_some_and(|value| !value.trim().is_empty());
        if !present {
            return Some(format!("api_key_env_missing:{name}"));
        }
    }
    if indexed == 0 {
        if stale > 0 {
            return Some("stale_vectors".to_string());
        }
        return Some("no_indexed_vectors".to_string());
    }
    None
}

/// Exact text embedded for one source chunk. Composition is:
///
/// ```text
/// source: <title>
/// heading: <heading path, or empty>
///
/// <chunk body, capped at 7,200 chars with a trailing ellipsis>
/// ```
///
/// The final string is capped at 8,000 chars. Its SHA-256 is stored beside the
/// vector; retrieval recomputes the hash from the current chunk and never
/// scores a vector whose text has changed.
pub fn source_chunk_embedding_text(source_title: &str, chunk: &SourceChunk) -> String {
    let title = truncate_chars(source_title.trim(), SOURCE_EMBEDDING_TITLE_MAX_CHARS);
    let heading = truncate_chars(
        chunk.heading.as_deref().unwrap_or("").trim(),
        SOURCE_EMBEDDING_HEADING_MAX_CHARS,
    );
    let body = truncate_chars(chunk.content.trim(), SOURCE_EMBEDDING_BODY_MAX_CHARS);
    let text = format!("source: {title}\nheading: {heading}\n\n{body}");
    truncate_chars(&text, SOURCE_EMBEDDING_TEXT_MAX_CHARS)
}

/// Query text is capped separately and is never persisted or logged.
pub fn source_embedding_query_text(query: &str) -> String {
    truncate_chars(query.trim(), SOURCE_EMBEDDING_QUERY_MAX_CHARS)
}

pub fn source_chunk_embedding_hash(text: &str) -> String {
    crate::sha256_hex(text.as_bytes())
}

/// One candidate entering semantic/hybrid fusion. Lexical and semantic scores
/// are optional because a candidate can appear in only one lane.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceRankInput {
    pub chunk_id: String,
    pub lexical: Option<f64>,
    pub semantic: Option<f64>,
}

/// One fused candidate. `score` is the effective ranking value; both raw lane
/// scores are retained for audit/debugging but no vector content is present.
#[derive(Debug, Clone, PartialEq)]
pub struct FusedSourceCandidate {
    pub chunk_id: String,
    pub score: f64,
    pub confidence: f64,
    pub source: String,
    pub origins: Vec<String>,
    pub lexical_score: Option<f64>,
    pub semantic_score: Option<f64>,
    pub lexical_rank: Option<usize>,
    pub semantic_rank: Option<usize>,
}

/// Deterministic reciprocal-rank fusion for source retrieval.
///
/// `score = (1/(K+lex_rank) + 1/(K+sem_rank)) * (K+1)/2` so two first places
/// equal 1.0 and a single first place equals 0.5. Ties sort by the better
/// lane rank, then by chunk id ascending. No provider call happens here.
pub fn fuse_source_candidates(inputs: Vec<SourceRankInput>) -> Vec<FusedSourceCandidate> {
    let mut merged: HashMap<String, (Option<f64>, Option<f64>)> = HashMap::new();
    for input in inputs {
        let entry = merged.entry(input.chunk_id).or_insert((None, None));
        if let Some(lexical) = input
            .lexical
            .filter(|score| score.is_finite() && *score > 0.0)
        {
            entry.0 = Some(entry.0.map_or(lexical, |current| current.max(lexical)));
        }
        if let Some(semantic) = input
            .semantic
            .filter(|score| score.is_finite() && *score >= MIN_SOURCE_SIMILARITY)
        {
            entry.1 = Some(entry.1.map_or(semantic, |current| current.max(semantic)));
        }
    }
    let lexical_ranks = lane_ranks(
        merged
            .iter()
            .filter_map(|(chunk_id, (lexical, _))| lexical.map(|score| (chunk_id.clone(), score)))
            .collect(),
    );
    let semantic_ranks = lane_ranks(
        merged
            .iter()
            .filter_map(|(chunk_id, (_, semantic))| semantic.map(|score| (chunk_id.clone(), score)))
            .collect(),
    );
    let mut candidates = Vec::with_capacity(merged.len());
    for (chunk_id, (lexical, semantic)) in merged {
        let lexical_rank = lexical_ranks.get(&chunk_id).copied();
        let semantic_rank = semantic_ranks.get(&chunk_id).copied();
        let mut raw_rrf = 0.0;
        if let Some(rank) = lexical_rank {
            raw_rrf += 1.0 / (SOURCE_RRF_K + rank as f64);
        }
        if let Some(rank) = semantic_rank {
            raw_rrf += 1.0 / (SOURCE_RRF_K + rank as f64);
        }
        let score = (raw_rrf * (SOURCE_RRF_K + 1.0) / 2.0).clamp(0.0, 1.0);
        let mut origins = Vec::new();
        if lexical_rank.is_some() {
            origins.push("lexical".to_string());
        }
        if semantic_rank.is_some() {
            origins.push("semantic".to_string());
        }
        let source = match (lexical_rank.is_some(), semantic_rank.is_some()) {
            (true, true) => "hybrid",
            (false, true) => "semantic",
            _ => "lexical",
        };
        candidates.push(FusedSourceCandidate {
            chunk_id,
            score,
            confidence: score,
            source: source.to_string(),
            origins,
            lexical_score: lexical,
            semantic_score: semantic,
            lexical_rank,
            semantic_rank,
        });
    }
    candidates.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| best_rank(left).cmp(&best_rank(right)))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    candidates
}

/// Semantic-only ranking by exact cosine. Ties are deterministic by chunk id.
pub fn rank_semantic_candidates(mut inputs: Vec<SourceRankInput>) -> Vec<FusedSourceCandidate> {
    inputs.retain(|input| {
        input
            .semantic
            .is_some_and(|score| score.is_finite() && score >= MIN_SOURCE_SIMILARITY)
    });
    let mut candidates = inputs
        .into_iter()
        .map(|input| {
            let semantic = input.semantic.unwrap_or(0.0).clamp(0.0, 1.0);
            FusedSourceCandidate {
                chunk_id: input.chunk_id,
                score: semantic,
                confidence: semantic,
                source: "semantic".to_string(),
                origins: vec!["semantic".to_string()],
                lexical_score: None,
                semantic_score: Some(semantic),
                lexical_rank: None,
                semantic_rank: Some(1),
            }
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    for (index, candidate) in candidates.iter_mut().enumerate() {
        candidate.semantic_rank = Some(index + 1);
    }
    candidates
}

fn best_rank(candidate: &FusedSourceCandidate) -> usize {
    candidate
        .lexical_rank
        .into_iter()
        .chain(candidate.semantic_rank)
        .min()
        .unwrap_or(usize::MAX)
}

fn lane_ranks(mut scored: Vec<(String, f64)>) -> HashMap<String, usize> {
    scored.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    scored
        .into_iter()
        .enumerate()
        .map(|(index, (chunk_id, _))| (chunk_id, index + 1))
        .collect()
}

/// Attach a deterministic lexical-only label/confidence to an unchanged hit.
/// The store's score/order is preserved byte-for-byte; confidence is only a
/// bounded presentation value (`score / max(score)` in the returned window).
pub fn label_lexical_hits(mut hits: Vec<SourceSearchHit>) -> Vec<SourceSearchHit> {
    let max_score = hits
        .iter()
        .map(|hit| hit.score)
        .filter(|score| score.is_finite() && *score > 0.0)
        .fold(0.0f64, f64::max);
    for hit in &mut hits {
        hit.source = "lexical".to_string();
        hit.origins = vec!["lexical".to_string()];
        hit.lexical_score = Some(hit.score);
        hit.semantic_score = None;
        hit.confidence = if max_score > 0.0 {
            (hit.score / max_score).clamp(0.0, 1.0)
        } else {
            0.0
        };
    }
    hits
}

/// Build one returned hit from a stored chunk plus its exact fused candidate.
/// Citation keys are attached later by the agent registry, only for hits in
/// this returned set.
pub fn fused_source_hit(
    chunk: SourceSearchHit,
    candidate: &FusedSourceCandidate,
) -> SourceSearchHit {
    SourceSearchHit {
        source: candidate.source.clone(),
        confidence: candidate.confidence,
        origins: candidate.origins.clone(),
        lexical_score: candidate.lexical_score,
        semantic_score: candidate.semantic_score,
        score: candidate.score,
        ..chunk
    }
}

/// Decode and score one stored candidate against a query vector. Rejects
/// structural corruption, dimension mismatch, zero norm, and hash drift.
pub fn semantic_similarity(
    vector: &[u8],
    dimensions: u32,
    query_vector: &[f32],
    content_hash_matches: bool,
) -> Option<f64> {
    if !content_hash_matches {
        return None;
    }
    let decoded = decode_embedding_vector(vector, dimensions).ok()?;
    if decoded.len() != query_vector.len() {
        return None;
    }
    let similarity = cosine_similarity(query_vector, &decoded)?;
    (similarity >= MIN_SOURCE_SIMILARITY).then_some(similarity)
}

/// Encode one validated source vector. Dimensions and finiteness are checked
/// by the shared versioned encoder before any bytes reach SQLite.
pub fn encode_source_vector(vector: &[f32]) -> Result<Vec<u8>> {
    if vector.len() < MIN_EMBEDDING_DIMENSIONS as usize
        || vector.len() > MAX_EMBEDDING_DIMENSIONS as usize
    {
        anyhow::bail!("source embedding vector dimensions are outside the supported range");
    }
    encode_embedding_vector(vector)
}

/// Loud, bounded diagnostic for logs only. It never includes request text,
/// endpoint values, or credentials.
pub fn sanitized_source_embedding_error(error: &str) -> String {
    truncate_chars(&sanitize_embedding_error(error), 2_000)
}

/// Versioned source vector header size. Exposed so the store's structural SQL
/// and tests use the same constant rather than a duplicated literal.
pub const SOURCE_VECTOR_HEADER_BYTES: usize = EMBEDDING_VECTOR_HEADER_BYTES;

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut truncated = value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    truncated.push('…');
    truncated
}

fn sanitize_reason(value: &str) -> String {
    let mut sanitized = value.trim().replace(['\n', '\r', '\t'], " ");
    if sanitized.chars().count() > 300 {
        sanitized = sanitized.chars().take(299).collect::<String>();
        sanitized.push('…');
    }
    sanitized
}

/// Bounded lane selection. A caller asking for semantic/hybrid gets the
/// documented lexical fallback when the lane is unavailable; the effective
/// mode is always reported by the engine.
pub fn effective_mode(
    requested: Option<SourceSearchMode>,
    policy: Option<&SourceEmbeddingPolicy>,
) -> (SourceSearchMode, bool) {
    match requested {
        Some(SourceSearchMode::Lexical) => (SourceSearchMode::Lexical, false),
        Some(SourceSearchMode::Semantic) => (SourceSearchMode::Semantic, true),
        Some(SourceSearchMode::Hybrid) => (SourceSearchMode::Hybrid, true),
        None => {
            if policy.is_some_and(SourceEmbeddingPolicy::is_active) {
                (SourceSearchMode::Hybrid, true)
            } else {
                (SourceSearchMode::Lexical, false)
            }
        }
    }
}

/// Candidate limit helper. The final caller limit stays at most 50; semantic
/// and hybrid scans use a bounded wider window so fusion has signal.
pub fn candidate_limit(limit: u32, semantic: bool) -> (u32, u32) {
    let final_limit = limit.clamp(1, 50);
    if semantic {
        (
            final_limit,
            MAX_SOURCE_SEMANTIC_CANDIDATES.min((final_limit.saturating_mul(16)).max(64)),
        )
    } else {
        (final_limit, final_limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use vp_protocol::types::MemoryEmbeddingUsage;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn policy(endpoint_env: &str) -> SourceEmbeddingPolicy {
        SourceEmbeddingPolicy {
            enabled: true,
            endpoint_env: Some(endpoint_env.to_string()),
            model: Some("source-embed".to_string()),
            dimensions: 8,
            batch_size: 4,
            max_chunks_per_source: 20,
            retention_days: 7,
            timeout_secs: 2,
            api_key_env: None,
        }
    }

    fn chunk(content: &str, heading: Option<&str>) -> SourceChunk {
        SourceChunk {
            id: "src-1:0001:abcdef".to_string(),
            source_id: "src-1".to_string(),
            chunk_no: 1,
            heading: heading.map(str::to_string),
            content: content.to_string(),
            start_offset: 0,
            end_offset: content.len(),
            token_count: 4,
        }
    }

    fn set_env(name: &str, value: &str) {
        // SAFETY: test-local unique variable names; no other test reads these.
        unsafe { std::env::set_var(name, value) };
    }

    fn remove_env(name: &str) {
        // SAFETY: test-local unique variable names.
        unsafe { std::env::remove_var(name) };
    }

    fn vector(seed: f32) -> Vec<f32> {
        (0..8).map(|index| seed + index as f32 * 0.01).collect()
    }

    #[test]
    fn policy_defaults_are_disabled_and_have_no_hidden_routing() {
        let policy = SourceEmbeddingPolicy::default();
        assert!(!policy.enabled);
        assert!(!policy.is_active());
        assert!(validate_source_embedding_policy(&policy).is_ok());
        assert_eq!(
            SourceEmbeddingRuntimePolicy::from_policy(&policy)
                .expect_err("disabled policy is unavailable")
                .reason,
            "disabled"
        );
    }

    #[test]
    fn policy_validation_rejects_unbounded_or_secret_like_values() {
        let mut policy = SourceEmbeddingPolicy {
            enabled: false,
            endpoint_env: None,
            model: None,
            ..SourceEmbeddingPolicy::default()
        };
        assert!(validate_source_embedding_policy(&policy).is_ok());
        policy.enabled = true;
        assert!(
            validate_source_embedding_policy(&policy).is_err(),
            "enabled requires endpoint_env and model"
        );
        policy.endpoint_env = Some("https://api.example.com/v1".to_string());
        assert!(
            validate_source_embedding_policy(&policy).is_err(),
            "the endpoint field is an env name, not a URL"
        );
        policy.endpoint_env = Some("SOURCE_EMBED_ENDPOINT".to_string());
        policy.model = Some("embed-small".to_string());
        assert!(validate_source_embedding_policy(&policy).is_ok());
        policy.dimensions = 1;
        assert!(validate_source_embedding_policy(&policy).is_err());
        policy.dimensions = 8;
        policy.batch_size = 0;
        assert!(validate_source_embedding_policy(&policy).is_err());
        policy.batch_size = 65;
        assert!(validate_source_embedding_policy(&policy).is_err());
        policy.batch_size = 8;
        policy.max_chunks_per_source = 0;
        assert!(validate_source_embedding_policy(&policy).is_err());
        policy.max_chunks_per_source = 501;
        assert!(validate_source_embedding_policy(&policy).is_err());
        policy.max_chunks_per_source = 200;
        policy.retention_days = 3_651;
        assert!(validate_source_embedding_policy(&policy).is_err());
        policy.retention_days = 30;
        policy.timeout_secs = 0;
        assert!(validate_source_embedding_policy(&policy).is_err());
        policy.timeout_secs = 30;
        policy.api_key_env = Some("sk-live-secret".to_string());
        assert!(validate_source_embedding_policy(&policy).is_err());
        policy.api_key_env = Some("SOURCE_EMBED_KEY".to_string());
        assert!(validate_source_embedding_policy(&policy).is_ok());
        policy.model = Some("bad model name".to_string());
        assert!(validate_source_embedding_policy(&policy).is_err());
    }

    #[test]
    fn normalization_trims_blank_names_and_never_stores_a_url() {
        let mut policy = SourceEmbeddingPolicy {
            enabled: true,
            endpoint_env: Some("  SOURCE_ENDPOINT  ".to_string()),
            model: Some("  embed-small  ".to_string()),
            api_key_env: Some("   ".to_string()),
            ..SourceEmbeddingPolicy::default()
        };
        normalize_source_embedding_policy(&mut policy);
        assert_eq!(policy.endpoint_env.as_deref(), Some("SOURCE_ENDPOINT"));
        assert_eq!(policy.model.as_deref(), Some("embed-small"));
        assert!(policy.api_key_env.is_none());
        assert!(validate_source_embedding_policy(&policy).is_ok());
    }

    #[test]
    fn chunk_text_is_bounded_stable_and_heading_aware() {
        let body = "word ".repeat(3_000);
        let source = chunk(&body, Some("Runtime / Leases"));
        let text = source_chunk_embedding_text("Integration notes", &source);
        assert!(text.contains("source: Integration notes"));
        assert!(text.contains("heading: Runtime / Leases"));
        assert!(text.chars().count() <= SOURCE_EMBEDDING_TEXT_MAX_CHARS);
        assert_eq!(
            source_chunk_embedding_hash(&text),
            source_chunk_embedding_hash(&text)
        );
        let changed = source_chunk_embedding_text("Other title", &source);
        assert_ne!(
            source_chunk_embedding_hash(&text),
            source_chunk_embedding_hash(&changed)
        );
        let no_heading = source_chunk_embedding_text("Title", &chunk("body", None));
        assert!(no_heading.contains("heading: \n"));
    }

    #[test]
    fn query_text_is_bounded_and_trimmed() {
        let query = format!("  {}  ", "x".repeat(SOURCE_EMBEDDING_QUERY_MAX_CHARS + 20));
        let normalized = source_embedding_query_text(&query);
        assert_eq!(normalized.chars().count(), SOURCE_EMBEDDING_QUERY_MAX_CHARS);
        assert!(!normalized.starts_with(' '));
        assert!(!normalized.ends_with(' '));
    }

    #[test]
    fn rrf_math_is_exact_and_deterministic() {
        let candidates = fuse_source_candidates(vec![
            SourceRankInput {
                chunk_id: "a".into(),
                lexical: Some(3.0),
                semantic: Some(0.9),
            },
            SourceRankInput {
                chunk_id: "b".into(),
                lexical: Some(2.0),
                semantic: Some(0.8),
            },
            SourceRankInput {
                chunk_id: "c".into(),
                lexical: Some(1.0),
                semantic: None,
            },
        ]);
        assert_eq!(candidates[0].chunk_id, "a");
        assert_eq!(candidates[0].score, 1.0);
        assert_eq!(candidates[0].source, "hybrid");
        assert_eq!(candidates[0].origins, vec!["lexical", "semantic"]);
        assert_eq!(candidates[1].chunk_id, "b");
        assert!((candidates[1].score - 0.916_666_666_666_666_6).abs() < 1e-12);
        assert_eq!(candidates[2].chunk_id, "c");
        assert!((candidates[2].score - 0.423_076_923_076_923_1).abs() < 1e-12);
        assert_eq!(candidates[2].source, "lexical");
        assert_eq!(candidates[2].origins, vec!["lexical"]);
        // Deterministic tie: same scores and ranks resolve by chunk id.
        let tied = fuse_source_candidates(vec![
            SourceRankInput {
                chunk_id: "z".into(),
                lexical: Some(1.0),
                semantic: None,
            },
            SourceRankInput {
                chunk_id: "a".into(),
                lexical: Some(1.0),
                semantic: None,
            },
        ]);
        assert_eq!(tied[0].chunk_id, "a");
        assert_eq!(tied[1].chunk_id, "z");
    }

    #[test]
    fn fusion_deduplicates_lanes_and_ignores_below_threshold_cosine() {
        let candidates = fuse_source_candidates(vec![
            SourceRankInput {
                chunk_id: "a".into(),
                lexical: Some(1.0),
                semantic: None,
            },
            SourceRankInput {
                chunk_id: "a".into(),
                lexical: Some(4.0),
                semantic: Some(MIN_SOURCE_SIMILARITY / 2.0),
            },
        ]);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].lexical_score, Some(4.0));
        assert_eq!(candidates[0].source, "lexical");
        assert_eq!(
            candidates[0].origins,
            vec!["lexical"],
            "below-threshold cosine is not a semantic origin"
        );
    }

    #[test]
    fn fusion_handles_empty_and_single_lane_windows() {
        assert!(fuse_source_candidates(Vec::new()).is_empty());
        let lexical = fuse_source_candidates(vec![SourceRankInput {
            chunk_id: "only".into(),
            lexical: Some(1.0),
            semantic: None,
        }]);
        assert_eq!(lexical.len(), 1);
        assert_eq!(lexical[0].source, "lexical");
        assert_eq!(lexical[0].score, 0.5);
        let semantic = fuse_source_candidates(vec![SourceRankInput {
            chunk_id: "only".into(),
            lexical: None,
            semantic: Some(0.42),
        }]);
        assert_eq!(semantic[0].source, "semantic");
        assert_eq!(semantic[0].origins, vec!["semantic"]);
        assert_eq!(semantic[0].semantic_score, Some(0.42));
        assert_eq!(semantic[0].score, 0.5);
        assert!(rank_semantic_candidates(Vec::new()).is_empty());
    }

    #[test]
    fn source_text_truncates_title_heading_and_body_independently() {
        let title = "t".repeat(SOURCE_EMBEDDING_TITLE_MAX_CHARS + 40);
        let heading = "h".repeat(SOURCE_EMBEDDING_HEADING_MAX_CHARS + 40);
        let body = "b".repeat(SOURCE_EMBEDDING_BODY_MAX_CHARS + 40);
        let text = source_chunk_embedding_text(&title, &chunk(&body, Some(&heading)));
        assert!(text.chars().count() <= SOURCE_EMBEDDING_TEXT_MAX_CHARS);
        assert!(text.starts_with(&format!("source: {}", "t".repeat(100))));
        assert!(text.contains(&format!("heading: {}", "h".repeat(100))));
        assert!(text.contains('…'), "truncation is visible, not silent");
        let body_only = source_chunk_embedding_text("Title", &chunk(&body, None));
        assert!(body_only.chars().count() <= SOURCE_EMBEDDING_TEXT_MAX_CHARS);
    }

    #[test]
    fn semantic_similarity_enforces_the_documented_threshold() {
        let query = vec![1.0f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let close = encode_source_vector(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]).unwrap();
        let orthogonal = encode_source_vector(&[0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]).unwrap();
        assert_eq!(semantic_similarity(&close, 8, &query, true), Some(1.0));
        assert!(
            semantic_similarity(&orthogonal, 8, &query, true).is_none(),
            "cosine 0 is below MIN_SOURCE_SIMILARITY"
        );
        assert!(semantic_similarity(&close, 8, &query, false).is_none());
    }

    #[test]
    fn semantic_ranking_is_exact_and_tie_broken_by_id() {
        let ranked = rank_semantic_candidates(vec![
            SourceRankInput {
                chunk_id: "b".into(),
                lexical: None,
                semantic: Some(0.7),
            },
            SourceRankInput {
                chunk_id: "a".into(),
                lexical: None,
                semantic: Some(0.7),
            },
            SourceRankInput {
                chunk_id: "c".into(),
                lexical: None,
                semantic: Some(0.01),
            },
        ]);
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].chunk_id, "a");
        assert_eq!(ranked[1].chunk_id, "b");
        assert_eq!(ranked[0].semantic_rank, Some(1));
        assert_eq!(ranked[1].semantic_rank, Some(2));
        assert!(
            ranked
                .iter()
                .all(|candidate| candidate.origins == ["semantic"])
        );
    }

    #[test]
    fn lexical_labels_do_not_change_score_or_order() {
        let hits = vec![
            SourceSearchHit {
                chunk_id: "a".into(),
                content: "a".into(),
                score: 4.0,
                ..SourceSearchHit::default()
            },
            SourceSearchHit {
                chunk_id: "b".into(),
                content: "b".into(),
                score: 2.0,
                ..SourceSearchHit::default()
            },
        ];
        let labeled = label_lexical_hits(hits);
        assert_eq!(labeled[0].score, 4.0);
        assert_eq!(labeled[1].score, 2.0);
        assert_eq!(labeled[0].confidence, 1.0);
        assert_eq!(labeled[1].confidence, 0.5);
        assert_eq!(labeled[0].source, "lexical");
        assert_eq!(labeled[0].lexical_score, Some(4.0));
        assert!(labeled[0].semantic_score.is_none());
    }

    #[test]
    fn availability_reasons_are_stable_and_never_include_values() {
        let disabled = SourceEmbeddingPolicy::default();
        assert_eq!(
            source_semantic_unavailable(Some(&disabled), 0, 0).as_deref(),
            Some("disabled")
        );
        let mut configured = SourceEmbeddingPolicy {
            enabled: true,
            endpoint_env: Some("VERLINPARK_TEST_SOURCE_EMBED_ABSENT_1".to_string()),
            model: Some("embed".to_string()),
            ..SourceEmbeddingPolicy::default()
        };
        assert_eq!(
            source_semantic_unavailable(Some(&configured), 0, 0).as_deref(),
            Some("endpoint_env_missing:VERLINPARK_TEST_SOURCE_EMBED_ABSENT_1")
        );
        set_env(
            "VERLINPARK_TEST_SOURCE_EMBED_ENDPOINT_2",
            "http://127.0.0.1:9/v1",
        );
        configured.endpoint_env = Some("VERLINPARK_TEST_SOURCE_EMBED_ENDPOINT_2".to_string());
        assert_eq!(
            source_semantic_unavailable(Some(&configured), 0, 0).as_deref(),
            Some("no_indexed_vectors")
        );
        assert_eq!(
            source_semantic_unavailable(Some(&configured), 0, 4).as_deref(),
            Some("stale_vectors")
        );
        assert!(source_semantic_unavailable(Some(&configured), 3, 4).is_none());
        let mut invalid_endpoint = configured.clone();
        set_env(
            "VERLINPARK_TEST_SOURCE_EMBED_ENDPOINT_INVALID_4",
            "ftp://example.test",
        );
        invalid_endpoint.endpoint_env =
            Some("VERLINPARK_TEST_SOURCE_EMBED_ENDPOINT_INVALID_4".to_string());
        assert_eq!(
            source_semantic_unavailable(Some(&invalid_endpoint), 3, 0).as_deref(),
            Some("invalid_endpoint")
        );
        remove_env("VERLINPARK_TEST_SOURCE_EMBED_ENDPOINT_INVALID_4");
        configured.api_key_env = Some("VERLINPARK_TEST_SOURCE_EMBED_KEY_ABSENT_3".to_string());
        assert_eq!(
            source_semantic_unavailable(Some(&configured), 3, 0).as_deref(),
            Some("api_key_env_missing:VERLINPARK_TEST_SOURCE_EMBED_KEY_ABSENT_3")
        );
        remove_env("VERLINPARK_TEST_SOURCE_EMBED_ENDPOINT_2");
        assert_eq!(
            source_semantic_unavailable(None, 0, 0).as_deref(),
            Some("disabled")
        );
    }

    #[test]
    fn effective_mode_defaults_to_hybrid_only_when_enabled() {
        let disabled = SourceEmbeddingPolicy::default();
        assert_eq!(
            effective_mode(None, Some(&disabled)),
            (SourceSearchMode::Lexical, false)
        );
        let enabled = SourceEmbeddingPolicy {
            enabled: true,
            endpoint_env: Some("SOURCE_ENDPOINT".to_string()),
            model: Some("embed".to_string()),
            ..SourceEmbeddingPolicy::default()
        };
        assert_eq!(
            effective_mode(None, Some(&enabled)),
            (SourceSearchMode::Hybrid, true)
        );
        assert_eq!(
            effective_mode(Some(SourceSearchMode::Lexical), Some(&enabled)),
            (SourceSearchMode::Lexical, false)
        );
        assert_eq!(
            effective_mode(Some(SourceSearchMode::Semantic), Some(&disabled)),
            (SourceSearchMode::Semantic, true)
        );
        let (final_limit, candidates) = candidate_limit(8, true);
        assert_eq!(final_limit, 8);
        assert_eq!(candidates, 128, "bounded wider window");
        assert_eq!(candidate_limit(500, false), (50, 50));
        assert_eq!(candidate_limit(50, true).1, 512);
    }

    #[test]
    fn encoding_and_similarity_reject_corrupt_or_stale_vectors() {
        let query = vector(0.2);
        let encoded = encode_source_vector(&vector(0.2)).unwrap();
        assert!(
            semantic_similarity(&encoded, 8, &query, true)
                .is_some_and(|score| (score - 1.0).abs() < 1e-6)
        );
        assert!(semantic_similarity(&encoded, 8, &query, false).is_none());
        assert!(semantic_similarity(&encoded, 16, &query, true).is_none());
        let mut corrupt = encoded.clone();
        corrupt[0] = 0;
        assert!(semantic_similarity(&corrupt, 8, &query, true).is_none());
        assert!(encode_source_vector(&[0.0; 4]).is_err());
        assert!(encode_source_vector(&[f32::NAN; 8]).is_err());
        assert_eq!(SOURCE_VECTOR_HEADER_BYTES, 9);
    }

    fn success_response(vectors: Vec<Vec<f32>>, usage: Option<u64>) -> serde_json::Value {
        let data = vectors
            .into_iter()
            .enumerate()
            .map(|(index, vector)| {
                serde_json::json!({
                    "object": "embedding",
                    "index": index,
                    "embedding": vector,
                })
            })
            .collect::<Vec<_>>();
        let mut value =
            serde_json::json!({"object": "list", "data": data, "model": "source-embed"});
        if let Some(tokens) = usage {
            value["usage"] = serde_json::json!({"prompt_tokens": tokens, "total_tokens": tokens});
        }
        value
    }

    #[tokio::test]
    async fn client_reads_endpoint_and_key_from_named_environment_only() {
        let server = MockServer::start().await;
        let env_name = "VERLINPARK_TEST_SOURCE_EMBED_ENDPOINT_SUCCESS";
        let key_name = "VERLINPARK_TEST_SOURCE_EMBED_KEY_SUCCESS";
        set_env(env_name, &server.uri());
        set_env(key_name, "secret-source-key");
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .and(header("authorization", "Bearer secret-source-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(success_response(vec![vector(0.3)], Some(11))),
            )
            .expect(1)
            .mount(&server)
            .await;
        let mut policy = policy(env_name);
        policy.api_key_env = Some(key_name.to_string());
        let client = SourceEmbeddingClient::from_policy(&policy).unwrap();
        let batch = client.embed(&["chunk".to_string()]).await.unwrap();
        assert!(batch.items[0].vector.is_some());
        assert_eq!(batch.usage.input_tokens, 11);
        let requests = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["model"], "source-embed");
        assert_eq!(body["dimensions"], 8);
        assert_eq!(body["input"][0], "chunk");
        remove_env(env_name);
        remove_env(key_name);
    }

    #[tokio::test]
    async fn client_reports_dimension_mismatch_per_item() {
        let server = MockServer::start().await;
        let env_name = "VERLINPARK_TEST_SOURCE_EMBED_ENDPOINT_DIMS";
        set_env(env_name, &server.uri());
        let mut bad = vector(0.2);
        bad.push(0.9);
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    {"index": 0, "embedding": vector(0.2)},
                    {"index": 1, "embedding": bad},
                ]
            })))
            .mount(&server)
            .await;
        let client = SourceEmbeddingClient::from_policy(&policy(env_name)).unwrap();
        let batch = client
            .embed(&["one".to_string(), "two".to_string()])
            .await
            .unwrap();
        assert!(batch.items[0].vector.is_some());
        let error = batch.items[1].error.as_deref().unwrap();
        assert!(error.contains("expected 8"));
        assert!(!source_embedding_item_error_retryable(error));
        assert!(source_embedding_item_error_code(error).starts_with("invalid_embedding:"));
        remove_env(env_name);
    }

    #[tokio::test]
    async fn client_timeout_maps_to_a_stable_reason() {
        let server = MockServer::start().await;
        let env_name = "VERLINPARK_TEST_SOURCE_EMBED_ENDPOINT_TIMEOUT";
        set_env(env_name, &server.uri());
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(success_response(vec![vector(0.2)], None))
                    .set_delay(Duration::from_millis(1_500)),
            )
            .mount(&server)
            .await;
        let mut policy = policy(env_name);
        policy.timeout_secs = 1;
        let client = SourceEmbeddingClient::from_policy(&policy).unwrap();
        let error = client.embed(&["slow".to_string()]).await.unwrap_err();
        assert_eq!(source_embedding_error_code(&error), "timeout");
        assert_eq!(client.model(), "source-embed");
        assert_eq!(client.dimensions(), 8);
        remove_env(env_name);
    }

    #[test]
    fn usage_type_stays_separate_from_memory_usage() {
        // Compile-time shape guard: source usage has its own type even though
        // the field names intentionally match for UI consistency.
        let source = vp_protocol::types::SourceEmbeddingUsage::default();
        let memory = MemoryEmbeddingUsage::default();
        assert_eq!(source.calls, memory.calls);
        let text = format!("{source:?}");
        assert!(text.contains("calls"));
    }
}

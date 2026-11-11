//! Deterministic project-memory ranking, vector encoding, and brief selection.
//!
//! Memory is a *claim with provenance*, not a chat summary. Lexical ranking is
//! pure Rust over FTS/LIKE relevance, structural provenance, and observed
//! usage. The optional semantic lane performs one explicitly configured
//! provider call outside this module and feeds cosine similarities back in;
//! this module never performs network I/O and never selects a model.
//!
//! Lexical weights (unchanged when the semantic lane is disabled):
//! - lexical relevance (bm25 or LIKE) ................ 0.60
//! - exact task evidence/tag match ................... 0.60
//! - path evidence/tag prefix overlap ................ 0.15 each, cap 0.45
//! - department tag/evidence match ................... 0.20
//! - kind weight (verification 0.24, integration 0.22) 0.02–0.30
//! - source weight (verifier 0.18) .................. 0.00–0.20
//! - recency (30-day half-life) ...................... 0.00–0.25
//! - observed usage (`ln(1+use_count)`) .............. 0.03 each, cap 0.15
//! - pinned ......................................... 0.50
//! - stale multiplier ............................... ×0.25 (only when allowed)
//!
//! Hybrid relevance keeps the 0.60 lexical ceiling but never lowers a row's
//! lexical relevance:
//!
//! ```text
//! hybrid = max(lexical,
//!              0.45*lexical + 0.35*cosine + 0.20*rrf_normalized)
//! score  = hybrid * 0.60 + structural/recency/usage/pinned weights
//! ```
//!
//! `rrf_normalized` is deterministic reciprocal-rank fusion with
//! `K = 10`, normalized so two first-place ranks equal 1.0. Ties are
//! deterministic: score desc, `created_at` desc, `id` asc.
//!
//! Vectors are encoded as `VPEM` magic, version byte, little-endian `u32`
//! dimensions, then `dimensions` little-endian `f32` values. Zero-norm,
//! dimension-mismatched, non-finite, and truncated vectors are rejected.

use std::collections::{HashMap, HashSet};

use time::OffsetDateTime;
use vp_protocol::types::{MemoryItem, MemoryMatch};
use vp_store::store::{MemoryCandidate, MemorySemanticCandidate};

const LEXICAL_WEIGHT: f64 = 0.60;
const TASK_MATCH_BOOST: f64 = 0.60;
const PATH_MATCH_BOOST: f64 = 0.15;
const PATH_MATCH_CAP: f64 = 0.45;
const DEPARTMENT_MATCH_BOOST: f64 = 0.20;
const RECENCY_WEIGHT: f64 = 0.25;
const USAGE_WEIGHT: f64 = 0.03;
const USAGE_CAP: f64 = 0.15;
const PINNED_BOOST: f64 = 0.50;
const STALE_PENALTY: f64 = 0.25;
const RECENCY_HALF_LIFE_DAYS: f64 = 30.0;
const MAX_BODY_CHARS_PER_ITEM: usize = 420;
const MIN_BODY_CHARS_PER_ITEM: usize = 40;

/// Hybrid blend weights. These are fractions of the existing 0.60 relevance
/// budget, not additional points.
const HYBRID_LEXICAL_SHARE: f64 = 0.45;
const HYBRID_SEMANTIC_SHARE: f64 = 0.35;
const HYBRID_RRF_SHARE: f64 = 0.20;
const RRF_K: f64 = 10.0;

/// Versioned vector encoding shared by the store/status tests. Keep this
/// magic stable: changing it requires re-indexing (the decoder fails closed).
pub const EMBEDDING_VECTOR_MAGIC: [u8; 4] = *b"VPEM";
pub const EMBEDDING_VECTOR_VERSION: u8 = 1;
pub const EMBEDDING_VECTOR_HEADER_BYTES: usize = 9;
pub const MIN_EMBEDDING_DIMENSIONS: u32 = 8;
pub const MAX_EMBEDDING_DIMENSIONS: u32 = 8192;
/// Exact text sent to the embedding endpoint is capped so a pathological body
/// cannot create an unbounded request. The cap is deterministic and UTF-8
/// safe; the same function produces the hash stored in SQLite.
pub const EMBEDDING_TEXT_MAX_CHARS: usize = 16_000;
pub const EMBEDDING_BODY_MAX_CHARS: usize = 12_000;
pub const EMBEDDING_QUERY_MAX_CHARS: usize = 8_000;

/// Structural query context. `text` drives lexical retrieval; the other fields
/// boost memories whose provenance matches the current task.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryQuery {
    pub text: String,
    pub task_id: Option<String>,
    pub department: Option<String>,
    pub paths: Vec<String>,
}

impl MemoryQuery {
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
            && self.task_id.is_none()
            && self.department.is_none()
            && self.paths.is_empty()
    }
}

/// Why the optional semantic lane did or did not contribute to a ranking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticRanking {
    /// No embedding policy, or the policy is disabled/incomplete.
    Disabled,
    /// Current embeddings were decoded successfully and can contribute.
    Active,
    /// Semantic recall was attempted but failed. Ranking is lexical and every
    /// returned row carries the sanitized reason.
    Unavailable(String),
}

impl SemanticRanking {
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Active)
    }

    pub fn unavailable_reason(&self) -> Option<&str> {
        match self {
            Self::Unavailable(reason) => Some(reason),
            Self::Disabled | Self::Active => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BriefSelection {
    pub text: String,
    pub memories: Vec<MemoryMatch>,
}

/// Exact deterministic text embedded for a memory. Composition is:
///
/// ```text
/// title: <title>
/// kind: <kind>
/// tags: <comma-separated tags in stored order>
///
/// <body, capped at 12,000 chars with a trailing ellipsis>
/// ```
///
/// The final string is capped at 16,000 chars. The content hash is SHA-256 of
/// this exact string, so any change to title, body, kind, or tags invalidates
/// coverage.
pub fn embedding_text(memory: &MemoryItem) -> String {
    let title = truncate_chars(memory.title.trim(), 2_000);
    let kind = truncate_chars(memory.kind.trim(), 80);
    let tags = truncate_chars(&memory.tags.join(", "), 4_000);
    let body = truncate_chars(memory.body.trim(), EMBEDDING_BODY_MAX_CHARS);
    let text = format!("title: {title}\nkind: {kind}\ntags: {tags}\n\n{body}");
    truncate_chars(&text, EMBEDDING_TEXT_MAX_CHARS)
}

/// Query text is capped separately; it is never stored or logged.
pub fn embedding_query_text(query: &str) -> String {
    truncate_chars(query.trim(), EMBEDDING_QUERY_MAX_CHARS)
}

/// SHA-256 of the exact embedded text. The store persists this derived hash,
/// never the text.
pub fn embedding_content_hash(text: &str) -> String {
    crate::sha256_hex(text.as_bytes())
}

/// Encode a finite vector with a versioned header. Dimensions are validated
/// against the policy range before any bytes are allocated.
pub fn encode_embedding_vector(values: &[f32]) -> anyhow::Result<Vec<u8>> {
    let dimensions = u32::try_from(values.len())
        .map_err(|_| anyhow::anyhow!("embedding dimensions exceed u32"))?;
    if !(MIN_EMBEDDING_DIMENSIONS..=MAX_EMBEDDING_DIMENSIONS).contains(&dimensions) {
        anyhow::bail!(
            "embedding dimensions must be between {MIN_EMBEDDING_DIMENSIONS} and {MAX_EMBEDDING_DIMENSIONS}"
        );
    }
    if values.iter().any(|value| !value.is_finite()) {
        anyhow::bail!("embedding contains a non-finite value");
    }
    let mut bytes = Vec::with_capacity(EMBEDDING_VECTOR_HEADER_BYTES + values.len() * 4);
    bytes.extend_from_slice(&EMBEDDING_VECTOR_MAGIC);
    bytes.push(EMBEDDING_VECTOR_VERSION);
    bytes.extend_from_slice(&dimensions.to_le_bytes());
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    Ok(bytes)
}

/// Decode a stored vector and validate magic, version, dimension, exact
/// length, and finiteness. `expected_dimensions` comes from the policy; a
/// mismatch is rejected before cosine math.
pub fn decode_embedding_vector(bytes: &[u8], expected_dimensions: u32) -> anyhow::Result<Vec<f32>> {
    if bytes.len() < EMBEDDING_VECTOR_HEADER_BYTES {
        anyhow::bail!("embedding vector is shorter than its header");
    }
    if bytes[..4] != EMBEDDING_VECTOR_MAGIC {
        anyhow::bail!("embedding vector has an unknown magic");
    }
    if bytes[4] != EMBEDDING_VECTOR_VERSION {
        anyhow::bail!("unsupported embedding vector version {}", bytes[4]);
    }
    let dimensions = u32::from_le_bytes([bytes[5], bytes[6], bytes[7], bytes[8]]);
    if dimensions != expected_dimensions {
        anyhow::bail!(
            "embedding vector dimensions {dimensions} do not match policy {expected_dimensions}"
        );
    }
    if !(MIN_EMBEDDING_DIMENSIONS..=MAX_EMBEDDING_DIMENSIONS).contains(&dimensions) {
        anyhow::bail!("embedding vector dimensions are outside the supported range");
    }
    let expected_len = EMBEDDING_VECTOR_HEADER_BYTES
        .checked_add(dimensions as usize * 4)
        .ok_or_else(|| anyhow::anyhow!("embedding vector length overflow"))?;
    if bytes.len() != expected_len {
        anyhow::bail!(
            "embedding vector has {} bytes, expected {expected_len}",
            bytes.len()
        );
    }
    let mut values = Vec::with_capacity(dimensions as usize);
    for chunk in bytes[EMBEDDING_VECTOR_HEADER_BYTES..].as_chunks::<4>().0 {
        let value = f32::from_le_bytes(*chunk);
        if !value.is_finite() {
            anyhow::bail!("embedding vector contains a non-finite value");
        }
        values.push(value);
    }
    Ok(values)
}

/// Cosine similarity clamped to `0..=1` (negative similarity is not useful for
/// retrieval and is treated as zero). Returns `None` for dimension mismatch,
/// empty/zero-norm vectors, or non-finite arithmetic.
pub fn cosine_similarity(left: &[f32], right: &[f32]) -> Option<f64> {
    if left.is_empty() || left.len() != right.len() {
        return None;
    }
    let mut dot = 0.0f64;
    let mut left_norm = 0.0f64;
    let mut right_norm = 0.0f64;
    for (&left, &right) in left.iter().zip(right) {
        if !left.is_finite() || !right.is_finite() {
            return None;
        }
        let left = f64::from(left);
        let right = f64::from(right);
        dot += left * right;
        left_norm += left * left;
        right_norm += right * right;
    }
    if left_norm <= 0.0 || right_norm <= 0.0 {
        return None;
    }
    let similarity = dot / (left_norm.sqrt() * right_norm.sqrt());
    if !similarity.is_finite() {
        return None;
    }
    Some(similarity.clamp(0.0, 1.0))
}

/// Score stored semantic candidates against a query vector.
///
/// Every candidate is re-hashed from its current memory row before use, so a
/// tag/body update that has not been re-indexed cannot contribute an embedding
/// from stale text. Invalid encodings, dimension mismatches, zero-norm vectors,
/// and similarities below `min_similarity` are dropped rather than surfaced.
/// The caller bounds `candidates`; no unbounded scan happens here.
pub fn semantic_scores(
    candidates: Vec<MemorySemanticCandidate>,
    query_vector: &[f32],
    dimensions: u32,
    min_similarity: f64,
) -> Vec<(MemoryItem, f64)> {
    let min_similarity = min_similarity.clamp(0.0, 1.0);
    let mut scored = Vec::new();
    for candidate in candidates {
        if candidate.dimensions != dimensions || candidate.model.is_empty() {
            continue;
        }
        let current_text = embedding_text(&candidate.memory);
        if embedding_content_hash(&current_text) != candidate.content_hash {
            continue;
        }
        let Ok(vector) = decode_embedding_vector(&candidate.vector, dimensions) else {
            continue;
        };
        let Some(similarity) = cosine_similarity(query_vector, &vector) else {
            continue;
        };
        if similarity < min_similarity {
            continue;
        }
        scored.push((candidate.memory, similarity));
    }
    scored
}

/// Rank candidates with an injected `now` so tests are stable.
pub fn rank_candidates(
    candidates: Vec<MemoryCandidate>,
    query: &MemoryQuery,
    now: OffsetDateTime,
    include_stale: bool,
) -> Vec<MemoryMatch> {
    rank_candidates_with_semantics(
        candidates,
        query,
        now,
        include_stale,
        SemanticRanking::Disabled,
    )
}

/// Hybrid-aware ranking. This is a pure function: no provider call, no global
/// state, and no model selection.
pub fn rank_candidates_with_semantics(
    candidates: Vec<MemoryCandidate>,
    query: &MemoryQuery,
    now: OffsetDateTime,
    include_stale: bool,
    semantic: SemanticRanking,
) -> Vec<MemoryMatch> {
    let task_id = query
        .task_id
        .as_deref()
        .map(str::trim)
        .filter(|task_id| !task_id.is_empty());
    let department = query
        .department
        .as_deref()
        .map(str::trim)
        .filter(|department| !department.is_empty());
    let paths = query
        .paths
        .iter()
        .map(|path| normalize_path(path))
        .filter(|path| !path.is_empty())
        .collect::<Vec<_>>();
    let (lexical_ranks, semantic_ranks) = if semantic.is_active() {
        (lexical_ranks(&candidates), semantic_ranks(&candidates))
    } else {
        (HashMap::new(), HashMap::new())
    };
    let semantic_active = semantic.is_active();
    let unavailable = semantic.unavailable_reason().map(str::to_string);

    let mut matches = Vec::new();
    for candidate in candidates {
        let memory = candidate.memory;
        match memory.status.as_str() {
            "superseded" | "archived" => continue,
            "stale" if !include_stale => continue,
            _ => {}
        }
        let mut reasons = Vec::new();
        let mut seen_reasons = HashSet::new();
        let lexical = candidate.lexical.clamp(0.0, 1.0);
        let semantic_score = if semantic_active {
            candidate
                .semantic
                .map(|value| value.clamp(0.0, 1.0))
                .filter(|value| *value > 0.0)
        } else {
            None
        };
        let relevance = if semantic_active {
            let lexical_rank = lexical_ranks.get(&memory.id).copied();
            let semantic_rank = semantic_ranks.get(&memory.id).copied();
            hybrid_relevance(lexical, semantic_score, lexical_rank, semantic_rank)
        } else {
            lexical
        };
        let mut score = relevance * LEXICAL_WEIGHT;
        if lexical > 0.0 {
            push_reason(
                &mut reasons,
                &mut seen_reasons,
                if candidate.fts { "fts" } else { "like" },
            );
        }
        if semantic_score.is_some() {
            push_reason(&mut reasons, &mut seen_reasons, "semantic");
        }
        if lexical > 0.0 && semantic_score.is_some() {
            push_reason(&mut reasons, &mut seen_reasons, "hybrid");
        }
        if unavailable.is_some() {
            push_reason(&mut reasons, &mut seen_reasons, "semantic_unavailable");
        }
        let source = if lexical > 0.0 && semantic_score.is_some() {
            "hybrid"
        } else if semantic_score.is_some() {
            "semantic"
        } else if lexical > 0.0 {
            "lexical"
        } else {
            "structural"
        }
        .to_string();
        let lexical_score = Some(lexical);
        let semantic_unavailable = unavailable.clone();

        if let Some(task_id) = task_id
            && memory_matches_task(&memory, task_id)
        {
            score += TASK_MATCH_BOOST;
            push_reason(&mut reasons, &mut seen_reasons, &format!("task:{task_id}"));
        }

        if !paths.is_empty() {
            let mut matched = Vec::new();
            for query_path in &paths {
                if memory_paths(&memory)
                    .iter()
                    .any(|path| path_prefix_overlap(path, query_path))
                {
                    matched.push(query_path.clone());
                }
            }
            if !matched.is_empty() {
                let boost = (PATH_MATCH_BOOST * matched.len() as f64).min(PATH_MATCH_CAP);
                score += boost;
                for path in matched {
                    push_reason(&mut reasons, &mut seen_reasons, &format!("path:{path}"));
                }
            }
        }

        if let Some(department) = department
            && memory_matches_department(&memory, department)
        {
            score += DEPARTMENT_MATCH_BOOST;
            push_reason(
                &mut reasons,
                &mut seen_reasons,
                &format!("department:{department}"),
            );
        }

        let kind_weight = kind_weight(&memory.kind);
        if kind_weight > 0.0 {
            score += kind_weight;
            push_reason(
                &mut reasons,
                &mut seen_reasons,
                &format!("kind:{}", memory.kind),
            );
        }
        let source_weight = source_weight(&memory.source);
        if source_weight > 0.0 {
            score += source_weight;
            push_reason(
                &mut reasons,
                &mut seen_reasons,
                &format!("source:{}", memory.source),
            );
        }

        let recency = recency_factor(&memory.created_at, now);
        if recency > 0.0 {
            score += recency * RECENCY_WEIGHT;
            if recency >= 0.5 {
                push_reason(&mut reasons, &mut seen_reasons, "recent");
            }
        }

        if memory.use_count > 0 {
            let usage = (f64::from(memory.use_count).ln_1p() * USAGE_WEIGHT).min(USAGE_CAP);
            score += usage;
            push_reason(
                &mut reasons,
                &mut seen_reasons,
                &format!("used:{}", memory.use_count),
            );
        }

        if memory.pinned {
            score += PINNED_BOOST;
            push_reason(&mut reasons, &mut seen_reasons, "pinned");
        }

        if memory.status == "stale" {
            score *= STALE_PENALTY;
            push_reason(&mut reasons, &mut seen_reasons, "stale");
        }

        matches.push(MemoryMatch {
            memory,
            score,
            reasons,
            lexical_score,
            semantic_score,
            source: Some(source),
            semantic_unavailable,
        });
    }

    matches.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| right.memory.created_at.cmp(&left.memory.created_at))
            .then_with(|| left.memory.id.cmp(&right.memory.id))
    });
    matches
}

fn lexical_ranks(candidates: &[MemoryCandidate]) -> HashMap<String, usize> {
    let mut order = candidates
        .iter()
        .filter(|candidate| candidate.lexical > 0.0)
        .map(|candidate| (candidate.memory.id.clone(), candidate.lexical))
        .collect::<Vec<_>>();
    order.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    order
        .into_iter()
        .enumerate()
        .map(|(index, (id, _))| (id, index + 1))
        .collect()
}

fn semantic_ranks(candidates: &[MemoryCandidate]) -> HashMap<String, usize> {
    let mut order = candidates
        .iter()
        .filter_map(|candidate| {
            candidate
                .semantic
                .map(|score| (candidate.memory.id.clone(), score))
        })
        .filter(|(_, score)| *score > 0.0)
        .collect::<Vec<_>>();
    order.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    order
        .into_iter()
        .enumerate()
        .map(|(index, (id, _))| (id, index + 1))
        .collect()
}

/// Deterministic weighted blend plus reciprocal-rank fusion. `max(lexical, ..)`
/// guarantees that a missing or weak semantic signal can never reduce the
/// lexical contribution.
fn hybrid_relevance(
    lexical: f64,
    semantic: Option<f64>,
    lexical_rank: Option<usize>,
    semantic_rank: Option<usize>,
) -> f64 {
    let semantic = semantic.unwrap_or(0.0);
    let mut raw_rrf = 0.0;
    if let Some(rank) = lexical_rank {
        raw_rrf += 1.0 / (RRF_K + rank as f64);
    }
    if let Some(rank) = semantic_rank {
        raw_rrf += 1.0 / (RRF_K + rank as f64);
    }
    // Maximum raw score is two first places: 2 / (K + 1).
    let rrf = raw_rrf * (RRF_K + 1.0) / 2.0;
    let blend = HYBRID_LEXICAL_SHARE * lexical
        + HYBRID_SEMANTIC_SHARE * semantic
        + HYBRID_RRF_SHARE * rrf.clamp(0.0, 1.0);
    lexical.max(blend).clamp(0.0, 1.0)
}

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

/// Build a compact brief under a hard character budget. Items are considered in
/// rank order; a low-ranked item is dropped rather than exceeding the budget.
pub fn select_brief(matches: &[MemoryMatch], max_chars: u32) -> BriefSelection {
    let budget = max_chars.max(1) as usize;
    let mut text = String::new();
    let mut selected = Vec::new();
    for matched in matches {
        let prefix = format!(
            "[{}·{}] {}{} · ",
            matched.memory.kind,
            matched.memory.source,
            matched.memory.title,
            provenance_suffix(&matched.memory)
        );
        let prefix_chars = prefix.chars().count();
        let used = text.chars().count();
        if used > 0 && used + 1 >= budget {
            break;
        }
        let separator = usize::from(used > 0);
        let remaining = budget.saturating_sub(used + separator + prefix_chars);
        if remaining < MIN_BODY_CHARS_PER_ITEM {
            break;
        }
        let body_budget = remaining.min(MAX_BODY_CHARS_PER_ITEM);
        let body = collapse_whitespace(&matched.memory.body);
        let body_chars = body.chars().count();
        let body_part = if body_chars > body_budget {
            let mut clipped = body.chars().take(body_budget - 1).collect::<String>();
            clipped.push('…');
            clipped
        } else {
            body
        };
        if used > 0 {
            text.push('\n');
        }
        text.push_str(&prefix);
        text.push_str(&body_part);
        selected.push(matched.clone());
    }
    BriefSelection {
        text,
        memories: selected,
    }
}

/// Human-readable provenance for a memory without exposing raw evidence JSON.
pub fn provenance_label(memory: &MemoryItem) -> String {
    let mut parts = Vec::new();
    if let Some(task) = memory
        .evidence
        .iter()
        .find(|evidence| evidence.kind == "task" && !evidence.reference.trim().is_empty())
    {
        parts.push(format!("task {}", task.reference.trim()));
    }
    if let Some(commit) = memory
        .evidence
        .iter()
        .find(|evidence| evidence.kind == "commit" && !evidence.reference.trim().is_empty())
    {
        parts.push(format!("commit {}", short_commit(&commit.reference)));
    }
    parts.join(", ")
}

fn provenance_suffix(memory: &MemoryItem) -> String {
    let label = provenance_label(memory);
    if label.is_empty() {
        String::new()
    } else {
        format!(" ({label})")
    }
}

fn push_reason(reasons: &mut Vec<String>, seen: &mut HashSet<String>, reason: &str) {
    if seen.insert(reason.to_string()) {
        reasons.push(reason.to_string());
    }
}

fn normalize_path(value: &str) -> String {
    value
        .trim()
        .replace('\\', "/")
        .trim_start_matches("./")
        .trim_end_matches('/')
        .to_string()
}

fn path_prefix_overlap(left: &str, right: &str) -> bool {
    if left.is_empty() || right.is_empty() {
        return false;
    }
    left == right
        || left
            .strip_prefix(right)
            .is_some_and(|rest| rest.starts_with('/'))
        || right
            .strip_prefix(left)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn memory_paths(memory: &MemoryItem) -> Vec<String> {
    let mut paths = memory
        .evidence
        .iter()
        .filter(|evidence| evidence.kind == "path")
        .map(|evidence| normalize_path(&evidence.reference))
        .filter(|path| !path.is_empty())
        .collect::<Vec<_>>();
    for tag in &memory.tags {
        let tag = tag.strip_prefix("path:").unwrap_or(tag);
        if tag.contains('/') {
            let path = normalize_path(tag);
            if !path.is_empty() {
                paths.push(path);
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

fn memory_matches_task(memory: &MemoryItem, task_id: &str) -> bool {
    memory
        .evidence
        .iter()
        .any(|evidence| evidence.kind == "task" && evidence.reference.trim() == task_id)
        || memory.tags.iter().any(|tag| tag.trim() == task_id)
}

fn memory_matches_department(memory: &MemoryItem, department: &str) -> bool {
    let department = department.to_lowercase();
    memory
        .tags
        .iter()
        .any(|tag| tag.eq_ignore_ascii_case(&department))
        || memory.evidence.iter().any(|evidence| {
            evidence.kind == "department"
                || evidence.reference.eq_ignore_ascii_case(&department)
                || evidence
                    .note
                    .as_deref()
                    .is_some_and(|note| note.eq_ignore_ascii_case(&department))
        })
}

fn kind_weight(kind: &str) -> f64 {
    match kind {
        "decision" => 0.30,
        "convention" => 0.28,
        "handoff" => 0.26,
        "verification" => 0.24,
        "integration" => 0.22,
        "fact" => 0.20,
        "task_result" => 0.15,
        "scratch" => 0.02,
        "" => 0.0,
        _ => 0.10,
    }
}

fn source_weight(source: &str) -> f64 {
    match source {
        "operator" => 0.20,
        "verifier" => 0.18,
        "task_result" => 0.15,
        "orchestrator" => 0.12,
        "agent" => 0.05,
        "import" => 0.0,
        "" => 0.0,
        _ => 0.02,
    }
}

fn recency_factor(created_at: &str, now: OffsetDateTime) -> f64 {
    let Some(created_at) =
        OffsetDateTime::parse(created_at, &time::format_description::well_known::Rfc3339).ok()
    else {
        return 0.0;
    };
    let age_secs = (now - created_at).whole_seconds().max(0) as f64;
    let age_days = age_secs / 86_400.0;
    (0.5_f64)
        .powf(age_days / RECENCY_HALF_LIFE_DAYS)
        .clamp(0.0, 1.0)
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn short_commit(reference: &str) -> String {
    reference.trim().chars().take(7).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vp_protocol::types::MemoryEvidence;

    fn memory(id: &str, kind: &str, title: &str, created_at: &str) -> MemoryItem {
        MemoryItem {
            id: id.to_string(),
            session_id: "ses".to_string(),
            project_id: "prj".to_string(),
            agent_id: "agent".to_string(),
            agent_name: "Agent".to_string(),
            kind: kind.to_string(),
            title: title.to_string(),
            body: "body text".to_string(),
            tags: Vec::new(),
            created_at: created_at.to_string(),
            status: "active".to_string(),
            source: "agent".to_string(),
            evidence: Vec::new(),
            supersedes: Vec::new(),
            superseded_by: None,
            pinned: false,
            use_count: 0,
            last_used_at: None,
            last_verified_at: None,
            verify_after: None,
            stale_reason: None,
        }
    }

    fn candidate(memory: MemoryItem, lexical: f64) -> MemoryCandidate {
        MemoryCandidate {
            memory,
            lexical,
            fts: true,
            semantic: None,
        }
    }

    fn evidence(kind: &str, reference: &str) -> MemoryEvidence {
        MemoryEvidence {
            kind: kind.to_string(),
            reference: reference.to_string(),
            note: None,
        }
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::parse(
            "2026-11-11T00:00:00Z",
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap()
    }

    #[test]
    fn ranks_deterministically_with_task_and_path_boosts() {
        let mut task_memory = memory("m-task", "handoff", "Task handoff", "2026-11-01T00:00:00Z");
        task_memory.evidence = vec![
            MemoryEvidence {
                kind: "task".to_string(),
                reference: "T3".to_string(),
                note: None,
            },
            MemoryEvidence {
                kind: "path".to_string(),
                reference: "src/api".to_string(),
                note: None,
            },
        ];
        let plain = memory("m-plain", "fact", "Plain fact", "2026-11-10T00:00:00Z");
        let query = MemoryQuery {
            text: "handoff".to_string(),
            task_id: Some("T3".to_string()),
            department: None,
            paths: vec!["src/api/handlers".to_string()],
        };
        let ranked = rank_candidates(
            vec![
                candidate(plain.clone(), 0.4),
                candidate(task_memory.clone(), 0.3),
            ],
            &query,
            now(),
            false,
        );
        assert_eq!(ranked[0].memory.id, "m-task");
        assert!(ranked[0].reasons.iter().any(|reason| reason == "task:T3"));
        assert!(
            ranked[0]
                .reasons
                .iter()
                .any(|reason| reason == "path:src/api/handlers")
        );

        let again = rank_candidates(
            vec![candidate(plain, 0.4), candidate(task_memory, 0.3)],
            &query,
            now(),
            false,
        );
        assert_eq!(ranked, again);
    }

    #[test]
    fn pinned_and_stale_penalty_are_visible_in_reasons() {
        let mut pinned = memory("m-pinned", "fact", "Pinned", "2026-10-01T00:00:00Z");
        pinned.pinned = true;
        let mut stale = memory("m-stale", "fact", "Stale", "2026-11-10T00:00:00Z");
        stale.status = "stale".to_string();
        let query = MemoryQuery {
            text: "pinned".to_string(),
            ..MemoryQuery::default()
        };
        let ranked = rank_candidates(
            vec![
                candidate(stale.clone(), 0.5),
                candidate(pinned.clone(), 0.1),
            ],
            &query,
            now(),
            true,
        );
        assert_eq!(ranked[0].memory.id, "m-pinned");
        assert!(ranked[0].reasons.iter().any(|reason| reason == "pinned"));
        let stale_match = ranked
            .iter()
            .find(|matched| matched.memory.id == "m-stale")
            .unwrap();
        assert!(stale_match.reasons.iter().any(|reason| reason == "stale"));
        let excluded = rank_candidates(vec![candidate(stale, 0.5)], &query, now(), false);
        assert!(excluded.is_empty());
    }

    #[test]
    fn brief_respects_char_budget_and_formats_provenance() {
        let mut task = memory(
            "m1",
            "decision",
            "Keep merges deterministic",
            "2026-11-10T00:00:00Z",
        );
        task.body = "The Integrator must not depend on an LLM. ".repeat(80);
        task.evidence = vec![
            MemoryEvidence {
                kind: "task".to_string(),
                reference: "T3".to_string(),
                note: None,
            },
            MemoryEvidence {
                kind: "commit".to_string(),
                reference: "abcdef1234567890".to_string(),
                note: None,
            },
        ];
        let matches = vec![MemoryMatch {
            memory: task,
            score: 1.0,
            reasons: vec!["fts".to_string()],
            lexical_score: Some(0.5),
            semantic_score: None,
            source: Some("lexical".to_string()),
            semantic_unavailable: None,
        }];
        let selection = select_brief(&matches, 300);
        assert!(selection.text.chars().count() <= 300);
        assert!(selection.text.starts_with("[decision·agent]"));
        assert!(selection.text.contains("(task T3, commit abcdef1)"));
        assert!(selection.text.ends_with('…'));
        assert_eq!(selection.memories.len(), 1);

        let tiny = select_brief(&matches, 10);
        assert!(tiny.text.is_empty());
        assert!(tiny.memories.is_empty());
    }

    #[test]
    fn recency_half_life_and_usage_are_monotonic() {
        let fresh = memory("fresh", "fact", "Fresh", "2026-11-10T00:00:00Z");
        let old = memory("old", "fact", "Old", "2026-08-01T00:00:00Z");
        let query = MemoryQuery::default();
        let ranked = rank_candidates(
            vec![candidate(old, 0.0), candidate(fresh.clone(), 0.0)],
            &query,
            now(),
            false,
        );
        assert_eq!(ranked[0].memory.id, "fresh");
        assert!(ranked[0].reasons.iter().any(|reason| reason == "recent"));

        let mut used = fresh;
        used.id = "used".to_string();
        used.use_count = 9;
        let ranked = rank_candidates(vec![candidate(used, 0.0)], &query, now(), false);
        assert!(ranked[0].reasons.iter().any(|reason| reason == "used:9"));
        assert!(ranked[0].score > 0.0);
    }

    #[test]
    fn exact_task_evidence_beats_a_recent_generic_fact() {
        let mut task = memory("m-task", "fact", "Task fact", "2026-01-01T00:00:00Z");
        task.evidence = vec![evidence("task", "T3")];
        let generic = memory("m-generic", "fact", "Generic fact", "2026-11-10T00:00:00Z");
        let query = MemoryQuery {
            text: "fact".to_string(),
            task_id: Some("T3".to_string()),
            ..MemoryQuery::default()
        };
        // Equal lexical relevance: the exact task evidence must beat the much
        // fresher generic fact.
        let ranked = rank_candidates(
            vec![
                candidate(generic, LEXICAL_WEIGHT),
                candidate(task, LEXICAL_WEIGHT),
            ],
            &query,
            now(),
            false,
        );
        assert_eq!(ranked[0].memory.id, "m-task");
        assert!(ranked[0].reasons.iter().any(|reason| reason == "task:T3"));
        assert!(ranked[1].reasons.iter().any(|reason| reason == "recent"));
    }

    #[test]
    fn path_prefix_overlap_boosts_a_task_write_scope_match() {
        let mut scoped = memory("m-scoped", "fact", "Scoped", "2026-11-10T00:00:00Z");
        scoped.evidence = vec![evidence("path", "src/api")];
        let other = memory("m-other", "fact", "Other", "2026-11-10T00:00:00Z");
        let query = MemoryQuery {
            paths: vec!["src/api/handlers".to_string()],
            ..MemoryQuery::default()
        };
        let ranked = rank_candidates(
            vec![candidate(other, 0.2), candidate(scoped, 0.2)],
            &query,
            now(),
            false,
        );
        assert_eq!(ranked[0].memory.id, "m-scoped");
        assert!(
            ranked[0]
                .reasons
                .iter()
                .any(|reason| reason == "path:src/api/handlers")
        );
        assert!(ranked[1].score < ranked[0].score);
    }

    #[test]
    fn pinned_beats_an_otherwise_equal_unpinned_memory() {
        let mut pinned = memory("m-pinned", "fact", "Pinned", "2026-01-01T00:00:00Z");
        pinned.pinned = true;
        let unpinned = memory("m-unpinned", "fact", "Unpinned", "2026-11-10T00:00:00Z");
        let query = MemoryQuery::default();
        let ranked = rank_candidates(
            vec![candidate(unpinned, 0.3), candidate(pinned, 0.3)],
            &query,
            now(),
            false,
        );
        assert_eq!(ranked[0].memory.id, "m-pinned");
        assert!(ranked[0].reasons.iter().any(|reason| reason == "pinned"));
        assert!(!ranked[1].reasons.iter().any(|reason| reason == "pinned"));
    }

    #[test]
    fn stale_memories_are_excluded_by_default_and_penalized_when_included() {
        let fresh = memory("m-fresh", "fact", "Fresh", "2026-11-10T00:00:00Z");
        let mut stale = fresh.clone();
        stale.id = "m-stale".to_string();
        stale.status = "stale".to_string();
        let query = MemoryQuery::default();

        let excluded = rank_candidates(vec![candidate(stale.clone(), 0.5)], &query, now(), false);
        assert!(excluded.is_empty());

        let ranked = rank_candidates(
            vec![candidate(fresh, 0.5), candidate(stale, 0.5)],
            &query,
            now(),
            true,
        );
        assert_eq!(ranked[0].memory.id, "m-fresh");
        assert_eq!(ranked[1].memory.id, "m-stale");
        assert!(ranked[1].reasons.iter().any(|reason| reason == "stale"));
        let expected = ranked[0].score * STALE_PENALTY;
        assert!((ranked[1].score - expected).abs() < 1e-12);
    }

    #[test]
    fn usage_boost_is_capped() {
        let mut low = memory("m-low", "fact", "Low", "2026-11-10T00:00:00Z");
        low.use_count = 10_000;
        let mut high = memory("m-high", "fact", "High", "2026-11-10T00:00:00Z");
        high.use_count = 10_000_000;
        let ranked = rank_candidates(
            vec![candidate(low, 0.4), candidate(high, 0.4)],
            &MemoryQuery::default(),
            now(),
            false,
        );
        assert!(ranked.iter().all(|matched| {
            matched
                .reasons
                .iter()
                .any(|reason| reason.starts_with("used:"))
        }));
        // Both usage contributions saturate at USAGE_CAP, so the scores tie
        // despite the wildly different counts.
        assert!((ranked[0].score - ranked[1].score).abs() < 1e-12);
        assert_eq!(ranked[0].memory.id, "m-high");
        assert_eq!(ranked[1].memory.id, "m-low");
    }

    #[test]
    fn deterministic_tie_break_uses_score_created_at_and_id() {
        let mut pinned = memory("m-z", "fact", "Pinned", "2026-01-01T00:00:00Z");
        pinned.pinned = true;
        let a = memory("m-a", "fact", "A", "2026-11-10T00:00:00Z");
        let b = memory("m-b", "fact", "B", "2026-11-10T00:00:00Z");
        let c = memory("m-c", "fact", "C", "2026-09-01T00:00:00Z");
        let query = MemoryQuery::default();
        let ranked = rank_candidates(
            vec![
                candidate(c.clone(), 0.2),
                candidate(b.clone(), 0.2),
                candidate(a.clone(), 0.2),
                candidate(pinned, 0.2),
            ],
            &query,
            now(),
            false,
        );
        let ids = ranked
            .iter()
            .map(|matched| matched.memory.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["m-z", "m-a", "m-b", "m-c"]);

        let mut again_matches = vec![
            candidate(c, 0.2),
            candidate(b, 0.2),
            candidate(a, 0.2),
            candidate(memory("m-z", "fact", "Pinned", "2026-01-01T00:00:00Z"), 0.2),
        ];
        for matched in &mut again_matches {
            if matched.memory.id == "m-z" {
                matched.memory.pinned = true;
            }
        }
        let again = rank_candidates(again_matches, &query, now(), false);
        assert_eq!(ranked, again);
    }

    #[test]
    fn brief_drops_an_item_instead_of_truncating_its_prefix() {
        let mut first = memory("m-first", "fact", "First", "2026-11-10T00:00:00Z");
        first.body = "A".repeat(80);
        let second = memory(
            "m-second",
            "fact",
            "Second title that should be dropped",
            "2026-11-10T00:00:00Z",
        );
        let matches = vec![
            MemoryMatch {
                memory: first,
                score: 2.0,
                reasons: Vec::new(),
                lexical_score: Some(0.5),
                semantic_score: None,
                source: Some("lexical".to_string()),
                semantic_unavailable: None,
            },
            MemoryMatch {
                memory: second,
                score: 1.0,
                reasons: Vec::new(),
                lexical_score: Some(0.5),
                semantic_score: None,
                source: Some("lexical".to_string()),
                semantic_unavailable: None,
            },
        ];
        let selection = select_brief(&matches, 130);
        assert_eq!(selection.memories.len(), 1);
        assert!(selection.text.contains("[fact·agent] First"));
        assert!(!selection.text.contains("Second"));
        assert!(selection.text.chars().count() <= 130);
    }

    fn semantic_memory(id: &str, title: &str, body: &str) -> MemoryItem {
        let mut memory = memory(id, "fact", title, "2026-11-10T00:00:00Z");
        memory.body = body.to_string();
        memory
    }

    fn semantic_candidate(memory: MemoryItem, values: &[f32]) -> MemorySemanticCandidate {
        let text = embedding_text(&memory);
        MemorySemanticCandidate {
            memory,
            model: "test-embedding".to_string(),
            dimensions: values.len() as u32,
            content_hash: embedding_content_hash(&text),
            vector: encode_embedding_vector(values).unwrap(),
        }
    }

    fn eight(value: f32) -> Vec<f32> {
        (0..8).map(|index| value + index as f32 * 0.01).collect()
    }

    #[test]
    fn embedding_text_is_deterministic_and_composition_is_documented() {
        let mut memory = semantic_memory("m-text", "  Title  ", "Body\nwith   spaces");
        memory.kind = "decision".to_string();
        memory.tags = vec!["alpha".to_string(), "path:src/lib.rs".to_string()];
        let text = embedding_text(&memory);
        assert_eq!(
            text,
            "title: Title\nkind: decision\ntags: alpha, path:src/lib.rs\n\nBody\nwith   spaces"
        );
        assert_eq!(text, embedding_text(&memory));
        assert_eq!(
            embedding_content_hash(&text),
            embedding_content_hash(&embedding_text(&memory))
        );
        let mut changed = memory.clone();
        changed.tags.push("beta".to_string());
        assert_ne!(
            embedding_content_hash(&embedding_text(&changed)),
            embedding_content_hash(&text)
        );
    }

    #[test]
    fn embedding_text_caps_long_bodies_without_splitting_utf8() {
        let mut memory = semantic_memory("m-long", "Long", "");
        memory.body = "Ä".repeat(50_000);
        let text = embedding_text(&memory);
        assert!(text.chars().count() <= EMBEDDING_TEXT_MAX_CHARS);
        assert!(text.ends_with('…'));
        assert!(text.is_char_boundary(text.len() - '…'.len_utf8()));
        // The exact same capped text is hashed on every run.
        assert_eq!(
            embedding_content_hash(&text),
            embedding_content_hash(&embedding_text(&memory))
        );
    }

    #[test]
    fn query_text_is_capped_and_trimmed() {
        let query = format!("  {}  ", "x".repeat(9_000));
        let capped = embedding_query_text(&query);
        assert!(capped.chars().count() <= EMBEDDING_QUERY_MAX_CHARS);
        assert!(capped.starts_with('x'));
        assert!(!capped.ends_with(' '));
    }

    #[test]
    fn vector_encoding_round_trips_and_rejects_corruption() {
        let values = vec![0.25f32, -0.5, 1.0, 0.0, 3.5, -2.0, 0.125, 9.0];
        let encoded = encode_embedding_vector(&values).unwrap();
        assert_eq!(&encoded[..4], b"VPEM");
        assert_eq!(encoded[4], EMBEDDING_VECTOR_VERSION);
        assert_eq!(
            u32::from_le_bytes([encoded[5], encoded[6], encoded[7], encoded[8]]),
            8
        );
        assert_eq!(decode_embedding_vector(&encoded, 8).unwrap(), values);

        let mut wrong_magic = encoded.clone();
        wrong_magic[0] = b'X';
        assert!(decode_embedding_vector(&wrong_magic, 8).is_err());
        let mut wrong_version = encoded.clone();
        wrong_version[4] = 99;
        assert!(decode_embedding_vector(&wrong_version, 8).is_err());
        assert!(decode_embedding_vector(&encoded, 9).is_err());
        assert!(decode_embedding_vector(&encoded[..encoded.len() - 1], 8).is_err());
        assert!(decode_embedding_vector(&[], 8).is_err());
        assert!(encode_embedding_vector(&[1.0; 7]).is_err());
        assert!(encode_embedding_vector(&[f32::NAN; 8]).is_err());
    }

    #[test]
    fn vector_decode_rejects_non_finite_payload_bytes() {
        let mut encoded = encode_embedding_vector(&eight(0.2)).unwrap();
        let offset = EMBEDDING_VECTOR_HEADER_BYTES;
        encoded[offset..offset + 4].copy_from_slice(&f32::NAN.to_le_bytes());
        assert!(decode_embedding_vector(&encoded, 8).is_err());
    }

    #[test]
    fn cosine_is_clamped_and_rejects_zero_norm_or_mismatch() {
        assert_eq!(cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]), Some(1.0));
        assert_eq!(cosine_similarity(&[1.0, 0.0], &[-1.0, 0.0]), Some(0.0));
        assert!(cosine_similarity(&[0.0, 0.0], &[1.0, 0.0]).is_none());
        assert!(cosine_similarity(&[1.0, 0.0], &[1.0]).is_none());
        assert!(cosine_similarity(&[f32::NAN, 0.0], &[1.0, 0.0]).is_none());
        assert!(cosine_similarity(&[], &[]).is_none());
    }

    #[test]
    fn semantic_scores_skip_stale_hash_wrong_dimension_and_low_similarity() {
        let query = eight(1.0);
        let current = semantic_memory("m-current", "Current", "current body");
        let mut stale_hash = semantic_candidate(
            semantic_memory("m-stale-hash", "Stale hash", "old body"),
            &query,
        );
        stale_hash.content_hash = "deadbeef".to_string();
        let wrong_dimensions =
            semantic_candidate(semantic_memory("m-dims", "Dims", "body"), &query);
        let mut wrong_dimensions = wrong_dimensions;
        wrong_dimensions.dimensions = 16;
        let zero = semantic_candidate(semantic_memory("m-zero", "Zero", "body"), &[0.0; 8]);
        let below_threshold =
            semantic_candidate(semantic_memory("m-low", "Low", "body"), &eight(-1.0));
        let scored = semantic_scores(
            vec![
                semantic_candidate(current.clone(), &query),
                stale_hash,
                wrong_dimensions,
                zero,
                below_threshold,
            ],
            &query,
            8,
            0.5,
        );
        assert_eq!(scored.len(), 1);
        assert_eq!(scored[0].0.id, current.id);
        assert!((scored[0].1 - 1.0).abs() < 1e-6);
    }

    #[test]
    fn hybrid_ranking_lets_a_paraphrase_beat_an_unrelated_lexical_match() {
        let lexical = semantic_memory("m-lexical", "Deploy notes", "unrelated deploy notes");
        let paraphrase = semantic_memory(
            "m-paraphrase",
            "Release checklist",
            "how we roll out a build to production",
        );
        let candidates = vec![
            MemoryCandidate {
                memory: lexical,
                lexical: 0.12,
                fts: true,
                semantic: Some(0.05),
            },
            MemoryCandidate {
                memory: paraphrase,
                lexical: 0.0,
                fts: false,
                semantic: Some(0.93),
            },
        ];
        let ranked = rank_candidates_with_semantics(
            candidates,
            &MemoryQuery {
                text: "production release".to_string(),
                ..MemoryQuery::default()
            },
            now(),
            false,
            SemanticRanking::Active,
        );
        assert_eq!(ranked[0].memory.id, "m-paraphrase");
        assert_eq!(ranked[0].source.as_deref(), Some("semantic"));
        assert!(ranked[0].reasons.iter().any(|reason| reason == "semantic"));
        assert!(ranked[0].score > ranked[1].score);
        assert_eq!(ranked[1].source.as_deref(), Some("hybrid"));
    }

    #[test]
    fn hybrid_ranking_preserves_task_and_path_boosts() {
        let mut task = semantic_memory("m-task", "Task handoff", "body");
        task.evidence = vec![evidence("task", "T7"), evidence("path", "src/store")];
        let mut similar = semantic_memory("m-similar", "Similar", "body");
        similar.evidence = Vec::new();
        let candidates = vec![
            MemoryCandidate {
                memory: similar,
                lexical: 0.1,
                fts: true,
                semantic: Some(0.99),
            },
            MemoryCandidate {
                memory: task,
                lexical: 0.2,
                fts: true,
                semantic: Some(0.4),
            },
        ];
        let ranked = rank_candidates_with_semantics(
            candidates,
            &MemoryQuery {
                text: "task".to_string(),
                task_id: Some("T7".to_string()),
                department: None,
                paths: vec!["src/store/memory".to_string()],
            },
            now(),
            false,
            SemanticRanking::Active,
        );
        assert_eq!(ranked[0].memory.id, "m-task");
        assert!(ranked[0].reasons.iter().any(|reason| reason == "task:T7"));
        assert!(
            ranked[0]
                .reasons
                .iter()
                .any(|reason| reason == "path:src/store/memory")
        );
    }

    #[test]
    fn lexical_only_ranking_is_unchanged_and_had_no_semantic_fields() {
        let a = semantic_memory("m-a", "Alpha", "alpha body");
        let b = semantic_memory("m-b", "Beta", "beta body");
        let lexical = rank_candidates(
            vec![candidate(a.clone(), 0.4), candidate(b.clone(), 0.2)],
            &MemoryQuery::default(),
            now(),
            false,
        );
        let disabled = rank_candidates_with_semantics(
            vec![candidate(a, 0.4), candidate(b, 0.2)],
            &MemoryQuery::default(),
            now(),
            false,
            SemanticRanking::Disabled,
        );
        assert_eq!(lexical, disabled);
        assert!(
            disabled
                .iter()
                .all(|matched| matched.semantic_score.is_none())
        );
        assert!(
            disabled
                .iter()
                .all(|matched| matched.source.as_deref() == Some("lexical"))
        );
        assert!(
            disabled
                .iter()
                .all(|matched| matched.semantic_unavailable.is_none())
        );
    }

    #[test]
    fn unavailable_semantics_mark_every_match_and_fall_back_to_lexical() {
        let a = semantic_memory("m-a", "Alpha", "alpha body");
        let semantic_disabled = rank_candidates_with_semantics(
            vec![candidate(a, 0.4)],
            &MemoryQuery::default(),
            now(),
            false,
            SemanticRanking::Unavailable("embedding request timed out".to_string()),
        );
        assert_eq!(semantic_disabled.len(), 1);
        assert_eq!(
            semantic_disabled[0].semantic_unavailable.as_deref(),
            Some("embedding request timed out")
        );
        assert!(
            semantic_disabled[0]
                .reasons
                .iter()
                .any(|reason| reason == "semantic_unavailable")
        );
        assert!(semantic_disabled[0].semantic_score.is_none());
        let lexical_baseline = rank_candidates(
            vec![candidate(semantic_disabled[0].memory.clone(), 0.4)],
            &MemoryQuery::default(),
            now(),
            false,
        );
        assert_eq!(semantic_disabled[0].score, lexical_baseline[0].score);
    }

    #[test]
    fn hybrid_ties_break_on_created_at_then_id() {
        let mut first = semantic_memory("m-a", "A", "a");
        first.created_at = "2026-11-10T00:00:00Z".to_string();
        let mut second = semantic_memory("m-b", "B", "b");
        second.created_at = "2026-11-10T00:00:00Z".to_string();
        let candidates = vec![
            MemoryCandidate {
                memory: first.clone(),
                lexical: 0.3,
                fts: true,
                semantic: Some(0.8),
            },
            MemoryCandidate {
                memory: second.clone(),
                lexical: 0.3,
                fts: true,
                semantic: Some(0.8),
            },
        ];
        let ranked = rank_candidates_with_semantics(
            candidates.clone(),
            &MemoryQuery::default(),
            now(),
            false,
            SemanticRanking::Active,
        );
        assert_eq!(ranked[0].memory.id, "m-a");
        assert_eq!(ranked[1].memory.id, "m-b");
        let again = rank_candidates_with_semantics(
            candidates.into_iter().rev().collect(),
            &MemoryQuery::default(),
            now(),
            false,
            SemanticRanking::Active,
        );
        assert_eq!(ranked, again);
    }

    #[test]
    fn semantic_ranking_respects_status_pruning() {
        let mut archived = semantic_memory("m-archived", "Archived", "body");
        archived.status = "archived".to_string();
        let mut stale = semantic_memory("m-stale", "Stale", "body");
        stale.status = "stale".to_string();
        let candidates = vec![
            MemoryCandidate {
                memory: archived,
                lexical: 0.9,
                fts: true,
                semantic: Some(0.99),
            },
            MemoryCandidate {
                memory: stale,
                lexical: 0.5,
                fts: true,
                semantic: Some(0.99),
            },
        ];
        let ranked = rank_candidates_with_semantics(
            candidates,
            &MemoryQuery::default(),
            now(),
            false,
            SemanticRanking::Active,
        );
        assert!(ranked.is_empty());
    }

    #[test]
    fn semantic_scores_reject_corrupt_encodings_but_keep_valid_rows() {
        let query = eight(1.0);
        let valid = semantic_candidate(semantic_memory("m-valid", "Valid", "body"), &query);
        let mut wrong_magic = valid.clone();
        wrong_magic.memory.id = "m-magic".to_string();
        wrong_magic.vector[0] = b'X';
        let mut truncated = valid.clone();
        truncated.memory.id = "m-short".to_string();
        truncated.vector.truncate(12);
        let mut non_finite = valid.clone();
        non_finite.memory.id = "m-nan".to_string();
        let offset = EMBEDDING_VECTOR_HEADER_BYTES;
        non_finite.vector[offset..offset + 4].copy_from_slice(&f32::NAN.to_le_bytes());
        let scored = semantic_scores(
            vec![wrong_magic, truncated, non_finite, valid],
            &query,
            8,
            0.1,
        );
        assert_eq!(scored.len(), 1);
        assert_eq!(scored[0].0.id, "m-valid");
    }

    #[test]
    fn semantic_ranking_includes_stale_only_when_allowed() {
        let mut stale = semantic_memory("m-stale", "Stale", "body");
        stale.status = "stale".to_string();
        let candidates = vec![MemoryCandidate {
            memory: stale,
            lexical: 0.2,
            fts: true,
            semantic: Some(0.9),
        }];
        let excluded = rank_candidates_with_semantics(
            candidates.clone(),
            &MemoryQuery::default(),
            now(),
            false,
            SemanticRanking::Active,
        );
        assert!(excluded.is_empty());
        let included = rank_candidates_with_semantics(
            candidates,
            &MemoryQuery::default(),
            now(),
            true,
            SemanticRanking::Active,
        );
        assert_eq!(included.len(), 1);
        assert!(included[0].reasons.iter().any(|reason| reason == "stale"));
        assert!(included[0].semantic_score.is_some());
    }

    #[test]
    fn tag_order_is_part_of_the_embedded_content_identity() {
        let mut memory = semantic_memory("m-tags", "Tags", "body");
        memory.tags = vec!["alpha".to_string(), "beta".to_string()];
        let first = embedding_text(&memory);
        memory.tags.reverse();
        let second = embedding_text(&memory);
        assert_ne!(first, second);
        assert_ne!(
            embedding_content_hash(&first),
            embedding_content_hash(&second)
        );
    }

    #[test]
    fn hybrid_source_labels_distinguish_lexical_semantic_and_hybrid() {
        let lexical = semantic_memory("m-lexical", "Lexical", "body");
        let semantic_only = semantic_memory("m-semantic", "Semantic", "body");
        let both = semantic_memory("m-both", "Both", "body");
        let ranked = rank_candidates_with_semantics(
            vec![
                MemoryCandidate {
                    memory: lexical,
                    lexical: 0.4,
                    fts: true,
                    semantic: None,
                },
                MemoryCandidate {
                    memory: semantic_only,
                    lexical: 0.0,
                    fts: false,
                    semantic: Some(0.8),
                },
                MemoryCandidate {
                    memory: both,
                    lexical: 0.3,
                    fts: true,
                    semantic: Some(0.7),
                },
            ],
            &MemoryQuery::default(),
            now(),
            false,
            SemanticRanking::Active,
        );
        let by_id = ranked
            .iter()
            .map(|matched| (matched.memory.id.as_str(), matched.source.as_deref()))
            .collect::<HashMap<_, _>>();
        assert_eq!(by_id["m-lexical"], Some("lexical"));
        assert_eq!(by_id["m-semantic"], Some("semantic"));
        assert_eq!(by_id["m-both"], Some("hybrid"));
    }

    #[test]
    fn embedding_hash_matches_a_fixed_composition_vector() {
        let mut memory = semantic_memory("m-fixed", "Title", "Body");
        memory.kind = "fact".to_string();
        memory.tags = vec!["alpha".to_string(), "beta".to_string()];
        assert_eq!(
            embedding_content_hash(&embedding_text(&memory)),
            "25c3b0ba2322e9f4d9411401c59ff60464aa53b632767f88526d578d0fc3ec70"
        );
    }

    #[test]
    fn memory_query_emptiness_covers_all_structural_fields() {
        assert!(MemoryQuery::default().is_empty());
        assert!(
            !MemoryQuery {
                text: "x".to_string(),
                ..MemoryQuery::default()
            }
            .is_empty()
        );
        assert!(
            !MemoryQuery {
                task_id: Some("T1".to_string()),
                ..MemoryQuery::default()
            }
            .is_empty()
        );
        assert!(
            !MemoryQuery {
                department: Some("engineering".to_string()),
                ..MemoryQuery::default()
            }
            .is_empty()
        );
        assert!(
            !MemoryQuery {
                paths: vec!["src".to_string()],
                ..MemoryQuery::default()
            }
            .is_empty()
        );
    }

    #[test]
    fn cosine_is_scale_invariant_and_zero_for_orthogonal_vectors() {
        let left = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        let scaled = left.map(|value| value * 10.0);
        let similarity = cosine_similarity(&left, &scaled).unwrap();
        assert!((similarity - 1.0).abs() < 1e-6);
        let mut orthogonal = [0.0f32; 8];
        orthogonal[7] = 1.0;
        let mut first = [0.0f32; 8];
        first[0] = 1.0;
        assert_eq!(cosine_similarity(&first, &orthogonal), Some(0.0));
    }

    #[test]
    fn semantic_scores_accept_exact_threshold_and_reject_just_below() {
        let query = vec![1.0f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let mut candidate_memory = semantic_memory("m-threshold", "Threshold", "body");
        candidate_memory.created_at = "2026-11-10T00:00:00Z".to_string();
        // cosine = 0.5 for this vector against the query.
        let values = vec![0.5f32, 0.866_025_4, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let candidate = MemorySemanticCandidate {
            memory: candidate_memory,
            model: "test".to_string(),
            dimensions: 8,
            content_hash: embedding_content_hash(&embedding_text(&semantic_memory(
                "m-threshold",
                "Threshold",
                "body",
            ))),
            vector: encode_embedding_vector(&values).unwrap(),
        };
        let accepted = semantic_scores(vec![candidate.clone()], &query, 8, 0.49);
        assert_eq!(accepted.len(), 1);
        assert!((accepted[0].1 - 0.5).abs() < 1e-5);
        let rejected = semantic_scores(vec![candidate], &query, 8, 0.51);
        assert!(rejected.is_empty());
    }

    #[test]
    fn hybrid_never_lowers_the_lexical_component() {
        let memory = semantic_memory("m-low-semantic", "Low semantic", "body");
        let ranked = rank_candidates_with_semantics(
            vec![MemoryCandidate {
                memory,
                lexical: 0.9,
                fts: true,
                semantic: Some(0.01),
            }],
            &MemoryQuery::default(),
            now(),
            false,
            SemanticRanking::Active,
        );
        let lexical_only = rank_candidates(
            vec![candidate(
                semantic_memory("m-low-semantic", "Low semantic", "body"),
                0.9,
            )],
            &MemoryQuery::default(),
            now(),
            false,
        );
        assert_eq!(ranked[0].score, lexical_only[0].score);
    }

    #[test]
    fn brief_selection_keeps_hybrid_scores_and_reasons() {
        let memory = semantic_memory("m-brief", "Brief", "body");
        let matches = rank_candidates_with_semantics(
            vec![MemoryCandidate {
                memory,
                lexical: 0.1,
                fts: true,
                semantic: Some(0.9),
            }],
            &MemoryQuery::default(),
            now(),
            false,
            SemanticRanking::Active,
        );
        let selection = select_brief(&matches, 400);
        assert_eq!(selection.memories.len(), 1);
        assert_eq!(selection.memories[0].semantic_score, Some(0.9));
        assert_eq!(selection.memories[0].source.as_deref(), Some("hybrid"));
    }
}

//! Deterministic source retrieval v2: policy bounds, rerank, and lexical query
//! expansion.
//!
//! This module is the pure/synchronous half of source retrieval. The async
//! engine embeds at most one query batch, then calls these functions inside
//! `spawn_blocking`. Nothing here performs a model call, network request, or
//! hidden model selection.
//!
//! Invariants:
//!
//! - Retrieval only reorders or labels chunks that the store already returned.
//!   It never creates a chunk, citation key, or evidence entry.
//! - Rerank components are bounded to `0..=1` and blended with fixed weights.
//!   Ties are resolved by explicit, stable keys and finally chunk id.
//! - Expansion terms come from the deterministic tokenizer over the first-pass
//!   hits. The plan is bounded by `max_expanded_terms` (1..=16), and the second
//!   pass is bounded by the same lexical window as the first.
//! - No vector, centroid, embedded text, or query text crosses this module's
//!   public result types.
//! - ANN selection is delegated to [`crate::source_ann`]; any unavailable,
//!   stale, corrupt, or too-small index falls back to the exact scan with an
//!   explicit reason.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use time::OffsetDateTime;
use vp_protocol::types::{
    SourceQueryExpansionMode, SourceRerankMode, SourceRetrievalPolicy, SourceSearchHit,
    SourceSearchMode, SourceSearchResult,
};
use vp_store::Store;
use vp_store::store::{SourceSearchQuery, SourceSemanticCandidate};

use crate::source_ann::{SourceAnnProbe, probe_source_ann};
use crate::source_embedding::{
    MAX_SOURCE_LEXICAL_CANDIDATES, SourceRankInput, effective_mode, fuse_source_candidates,
    label_lexical_hits, rank_semantic_candidates,
};
use crate::sources::{source_query, source_tokens};

pub const MIN_SOURCE_ANN_MIN_VECTORS: u32 = 1;
pub const MAX_SOURCE_ANN_MIN_VECTORS: u32 = 100_000;
pub const MIN_SOURCE_ANN_NPROBE: u32 = 1;
pub const MAX_SOURCE_ANN_NPROBE: u32 = 32;
pub const MIN_SOURCE_EXPANDED_TERMS: u32 = 1;
pub const MAX_SOURCE_EXPANDED_TERMS: u32 = 16;
/// Fixed rerank half-life for the freshness component. A source updated 30
/// days ago contributes 0.5; the value is clamped and never negative.
pub const SOURCE_FRESHNESS_HALF_LIFE_DAYS: f64 = 30.0;
/// Fixed rerank weights. They sum to 1.0 and are never learned, configured by
/// a model, or randomized. Missing components are renormalized away.
pub const RERANK_WEIGHT_COSINE: f64 = 0.55;
pub const RERANK_WEIGHT_LEXICAL: f64 = 0.25;
pub const RERANK_WEIGHT_TITLE: f64 = 0.12;
pub const RERANK_WEIGHT_FRESHNESS: f64 = 0.08;
/// Expansion reads at most this many top hits to build its term plan.
pub const MAX_EXPANSION_SEED_HITS: usize = 3;
/// Per-hit token windows keep expansion CPU bounded even for a pathological
/// source that somehow exceeds the chunking caps.
pub const MAX_EXPANSION_TITLE_TOKENS: usize = 16;
pub const MAX_EXPANSION_HEADING_TOKENS: usize = 24;
pub const MAX_EXPANSION_CONTENT_TOKENS: usize = 96;
/// The second lexical pass may return at most this many candidates. The final
/// caller limit is applied after dedupe and rerank.
pub const MAX_EXPANSION_CANDIDATES: u32 = 50;
/// Rerank rescoring window. The first-pass lexical fetch is widened only when
/// rerank or expansion is enabled, and it is always capped here.
pub const MAX_RERANK_WINDOW: u32 = 50;

/// Stable stopword set for expansion. It is deliberately small and static: a
/// project cannot configure it, and it never grows from model output.
const EXPANSION_STOPWORDS: &[&str] = &[
    "a",
    "an",
    "and",
    "are",
    "as",
    "at",
    "be",
    "been",
    "but",
    "by",
    "can",
    "could",
    "did",
    "do",
    "does",
    "for",
    "from",
    "had",
    "has",
    "have",
    "he",
    "her",
    "here",
    "him",
    "his",
    "how",
    "i",
    "if",
    "in",
    "into",
    "is",
    "it",
    "its",
    "just",
    "may",
    "me",
    "might",
    "more",
    "most",
    "must",
    "my",
    "no",
    "not",
    "of",
    "on",
    "or",
    "our",
    "out",
    "over",
    "said",
    "same",
    "she",
    "should",
    "so",
    "some",
    "such",
    "than",
    "that",
    "the",
    "their",
    "them",
    "then",
    "there",
    "these",
    "they",
    "this",
    "those",
    "through",
    "to",
    "too",
    "under",
    "up",
    "us",
    "use",
    "used",
    "using",
    "was",
    "we",
    "were",
    "what",
    "when",
    "where",
    "which",
    "while",
    "who",
    "why",
    "will",
    "with",
    "would",
    "you",
    "your",
    "source",
    "sources",
    "note",
    "notes",
    "document",
    "documents",
    "doc",
    "docs",
    "file",
    "files",
    "page",
    "pages",
    "section",
    "sections",
    "chapter",
    "chapters",
    "index",
    "readme",
    "untitled",
    "title",
    "heading",
];

/// Trim and clamp additive retrieval policy fields. This is the single
/// normalization path used by `source.retrieval.policy.set`, the manifest
/// rebuild, and status. Clamping is intentional: operators cannot persist an
/// unbounded probe count or expansion budget.
pub fn normalize_source_retrieval_policy(policy: &mut SourceRetrievalPolicy) {
    policy.ann_min_vectors = policy
        .ann_min_vectors
        .clamp(MIN_SOURCE_ANN_MIN_VECTORS, MAX_SOURCE_ANN_MIN_VECTORS);
    policy.ann_nprobe = policy
        .ann_nprobe
        .clamp(MIN_SOURCE_ANN_NPROBE, MAX_SOURCE_ANN_NPROBE);
    policy.max_expanded_terms = policy
        .max_expanded_terms
        .clamp(MIN_SOURCE_EXPANDED_TERMS, MAX_SOURCE_EXPANDED_TERMS);
}

/// Validate an already-normalized retrieval policy. Numeric ranges are checked
/// again so a hand-edited manifest cannot silently acquire an unbounded value.
pub fn validate_source_retrieval_policy(policy: &SourceRetrievalPolicy) -> Result<()> {
    if !(MIN_SOURCE_ANN_MIN_VECTORS..=MAX_SOURCE_ANN_MIN_VECTORS).contains(&policy.ann_min_vectors)
    {
        anyhow::bail!(
            "source retrieval ann_min_vectors must be between {MIN_SOURCE_ANN_MIN_VECTORS} and {MAX_SOURCE_ANN_MIN_VECTORS}"
        );
    }
    if !(MIN_SOURCE_ANN_NPROBE..=MAX_SOURCE_ANN_NPROBE).contains(&policy.ann_nprobe) {
        anyhow::bail!(
            "source retrieval ann_nprobe must be between {MIN_SOURCE_ANN_NPROBE} and {MAX_SOURCE_ANN_NPROBE}"
        );
    }
    if !(MIN_SOURCE_EXPANDED_TERMS..=MAX_SOURCE_EXPANDED_TERMS).contains(&policy.max_expanded_terms)
    {
        anyhow::bail!(
            "source retrieval max_expanded_terms must be between {MIN_SOURCE_EXPANDED_TERMS} and {MAX_SOURCE_EXPANDED_TERMS}"
        );
    }
    Ok(())
}

/// One synchronous retrieval request. The caller has already parsed and
/// normalized filters and effective mode.
#[derive(Debug, Clone)]
pub struct SourceRetrievalRequest {
    pub project_id: String,
    pub query: SourceSearchQuery,
    pub source_ids: Vec<String>,
    pub limit: u32,
    pub policy: SourceRetrievalPolicy,
}

/// Query-vector context produced by the async engine after at most one
/// embedding batch. `expanded_vector` is present only when expansion is on and
/// produced at least one term.
#[derive(Debug, Clone)]
pub struct SourceSemanticContext {
    pub model: String,
    pub dimensions: u32,
    pub max_chunks: u32,
    pub query_vector: Vec<f32>,
    pub expanded_vector: Option<Vec<f32>>,
    pub expansion_terms: Vec<String>,
}

/// Result of a lexical-only retrieval call. `semantic_unavailable` is carried
/// through unchanged for the documented fallback contract.
pub fn search_lexical(
    store: &Store,
    request: &SourceRetrievalRequest,
    semantic_unavailable: Option<String>,
) -> Result<SourceSearchResult> {
    let window = lexical_window(request.limit, &request.policy);
    let (first, degraded) = store.search_source_chunks(
        &request.project_id,
        &request.source_ids,
        &request.query,
        Some(window),
    )?;
    let mut hits = label_lexical_hits(first);
    let mut expansion_terms = Vec::new();
    let mut expanded_ids = HashSet::new();
    if request.policy.query_expansion == SourceQueryExpansionMode::Lexical {
        expansion_terms = extract_expansion_terms(
            &request.query.terms,
            &hits,
            request.policy.max_expanded_terms,
        );
        if !expansion_terms.is_empty()
            && let Some(expanded_query) = expanded_source_query(&request.query, &expansion_terms)
        {
            let (second, _) = store.search_source_chunks(
                &request.project_id,
                &request.source_ids,
                &expanded_query,
                Some(
                    MAX_EXPANSION_CANDIDATES
                        .min(window.saturating_mul(2))
                        .max(window),
                ),
            )?;
            merge_expanded_hits(&mut hits, &mut expanded_ids, second, &expansion_terms);
        }
    }
    if request.policy.rerank == SourceRerankMode::Deterministic {
        rerank_hits(
            &mut hits,
            &request.query.terms,
            OffsetDateTime::now_utc(),
            &expanded_ids,
            &expansion_terms,
        );
        hits.truncate(request.limit.clamp(1, 50) as usize);
    } else {
        mark_rerank_off(&mut hits);
        hits.truncate(request.limit.clamp(1, 50) as usize);
    }
    Ok(SourceSearchResult {
        hits,
        degraded,
        mode: SourceSearchMode::Lexical.as_str().to_string(),
        requested_mode: SourceSearchMode::Lexical.as_str().to_string(),
        semantic_unavailable,
        candidates_scanned: 0,
        index_path: "exact".to_string(),
        index_generation: None,
        index_fallback_reason: None,
        probed_clusters: 0,
        ann_candidates: 0,
        rerank: request.policy.rerank.as_str().to_string(),
        expansion: request.policy.query_expansion.as_str().to_string(),
        expansion_terms,
    })
}

/// Semantic or hybrid retrieval with a pre-embedded original query (and
/// optional expanded query). The exact scan remains the fallback for every ANN
/// condition.
pub fn search_semantic(
    store: &Store,
    request: &SourceRetrievalRequest,
    context: &SourceSemanticContext,
    mode: SourceSearchMode,
) -> Result<SourceSearchResult> {
    let (_, semantic_limit) = crate::source_embedding::candidate_limit(request.limit, true);
    let (mut index, mut candidates);
    if request.policy.ann_enabled {
        let probe = probe_source_ann(
            store,
            &request.project_id,
            &request.policy,
            &context.model,
            context.dimensions,
            context.max_chunks,
            &request.source_ids,
            &request.query.collections,
            &request.query.labels,
            &context.query_vector,
            semantic_limit,
        );
        match probe {
            SourceAnnProbe::Ready(ready) => {
                index = SourceIndexPath::ann(
                    ready.generation,
                    ready.probed_clusters,
                    ready.candidates.len(),
                );
                candidates = ready.candidates;
                if let Some(expanded_vector) = context.expanded_vector.as_deref() {
                    let expanded = probe_source_ann(
                        store,
                        &request.project_id,
                        &request.policy,
                        &context.model,
                        context.dimensions,
                        context.max_chunks,
                        &request.source_ids,
                        &request.query.collections,
                        &request.query.labels,
                        expanded_vector,
                        semantic_limit,
                    );
                    if let SourceAnnProbe::Ready(ready) = expanded {
                        index.probed_clusters =
                            index.probed_clusters.saturating_add(ready.probed_clusters);
                        merge_ann_candidates(&mut candidates, ready.candidates);
                    }
                }
            }
            SourceAnnProbe::Fallback(fallback) => {
                index = SourceIndexPath::exact(&fallback.reason);
                candidates = exact_candidates(store, request, context, semantic_limit)?;
            }
        }
    } else {
        index = SourceIndexPath::exact("ann_disabled");
        candidates = exact_candidates(store, request, context, semantic_limit)?;
    }
    index.candidates = candidates.len();

    let mut hit_by_chunk: HashMap<String, SourceSearchHit> = HashMap::new();
    let mut semantic_inputs = Vec::new();
    let mut expanded_ids = HashSet::new();
    let mut stale_skips = 0usize;
    for candidate in &candidates {
        let text = crate::source_embedding::source_chunk_embedding_text(
            &candidate.source_title,
            &vp_protocol::types::SourceChunk {
                id: candidate.chunk_id.clone(),
                source_id: candidate.source_id.clone(),
                chunk_no: candidate.chunk_no,
                heading: candidate.heading.clone(),
                content: candidate.content.clone(),
                start_offset: 0,
                end_offset: 0,
                token_count: 0,
            },
        );
        let hash_matches =
            crate::source_embedding::source_chunk_embedding_hash(&text) == candidate.content_hash;
        if !hash_matches {
            stale_skips += 1;
            continue;
        }
        let decoded = crate::source_embedding::semantic_similarity(
            &candidate.vector,
            candidate.dimensions,
            &context.query_vector,
            true,
        );
        let expanded = context.expanded_vector.as_deref().and_then(|vector| {
            crate::source_embedding::semantic_similarity(
                &candidate.vector,
                candidate.dimensions,
                vector,
                true,
            )
        });
        let Some(similarity) = decoded else {
            // Even when the original query has no evidence, an expanded query
            // may still support this chunk. It is a real stored chunk, so it is
            // safe to surface and label as expanded.
            if let Some(expanded_score) = expanded {
                hit_by_chunk.insert(
                    candidate.chunk_id.clone(),
                    semantic_candidate_hit(candidate),
                );
                expanded_ids.insert(candidate.chunk_id.clone());
                semantic_inputs.push(SourceRankInput {
                    chunk_id: candidate.chunk_id.clone(),
                    lexical: None,
                    semantic: Some(expanded_score),
                });
            }
            continue;
        };
        if let Some(expanded_score) = expanded
            && expanded_score > similarity + 1e-9
        {
            expanded_ids.insert(candidate.chunk_id.clone());
        }
        hit_by_chunk.insert(
            candidate.chunk_id.clone(),
            semantic_candidate_hit(candidate),
        );
        semantic_inputs.push(SourceRankInput {
            chunk_id: candidate.chunk_id.clone(),
            lexical: None,
            semantic: Some(similarity),
        });
    }
    if semantic_inputs.is_empty() && stale_skips > 0 {
        let mut lexical = search_lexical(store, request, Some("stale_vectors".to_string()))?;
        lexical.requested_mode = mode.as_str().to_string();
        lexical.index_path = "exact".to_string();
        lexical.index_generation = index.generation;
        lexical.index_fallback_reason = Some("stale_vectors".to_string());
        lexical.candidates_scanned = candidates.len();
        return Ok(lexical);
    }

    if mode == SourceSearchMode::Semantic {
        let ranked = rank_semantic_candidates(semantic_inputs);
        let mut hits = ranked
            .into_iter()
            .map(|candidate| {
                let hit = hit_by_chunk
                    .get(&candidate.chunk_id)
                    .cloned()
                    .unwrap_or_default();
                crate::source_embedding::fused_source_hit(hit, &candidate)
            })
            .collect::<Vec<_>>();
        if request.policy.rerank == SourceRerankMode::Deterministic {
            rerank_hits(
                &mut hits,
                &request.query.terms,
                OffsetDateTime::now_utc(),
                &expanded_ids,
                &context.expansion_terms,
            );
        } else {
            mark_rerank_off(&mut hits);
        }
        hits.truncate(request.limit.clamp(1, 50) as usize);
        return Ok(SourceSearchResult {
            hits,
            degraded: false,
            mode: SourceSearchMode::Semantic.as_str().to_string(),
            requested_mode: SourceSearchMode::Semantic.as_str().to_string(),
            semantic_unavailable: None,
            candidates_scanned: candidates.len(),
            index_path: index.path,
            index_generation: index.generation,
            index_fallback_reason: index.fallback_reason,
            probed_clusters: index.probed_clusters,
            ann_candidates: index.candidates,
            rerank: request.policy.rerank.as_str().to_string(),
            expansion: request.policy.query_expansion.as_str().to_string(),
            expansion_terms: context.expansion_terms.clone(),
        });
    }

    // Hybrid: seed lexical first, then fuse. The first lexical pass is widened
    // to the documented candidate window so a lower-ranked lexical hit can
    // still win after semantic fusion.
    let (first, mut degraded) = store.search_source_chunks(
        &request.project_id,
        &request.source_ids,
        &request.query,
        Some(MAX_SOURCE_LEXICAL_CANDIDATES),
    )?;
    let mut lexical_hits = label_lexical_hits(first);
    let mut expansion_terms = context.expansion_terms.clone();
    if request.policy.query_expansion == SourceQueryExpansionMode::Lexical {
        if expansion_terms.is_empty() {
            expansion_terms = extract_expansion_terms(
                &request.query.terms,
                &lexical_hits,
                request.policy.max_expanded_terms,
            );
        }
        if !expansion_terms.is_empty()
            && let Some(expanded_query) = expanded_source_query(&request.query, &expansion_terms)
            && let Ok((second, second_degraded)) = store.search_source_chunks(
                &request.project_id,
                &request.source_ids,
                &expanded_query,
                Some(MAX_EXPANSION_CANDIDATES),
            )
        {
            if second_degraded {
                degraded = true;
            }
            merge_expanded_hits(
                &mut lexical_hits,
                &mut expanded_ids,
                second,
                &expansion_terms,
            );
        }
    }
    let mut inputs = semantic_inputs;
    for hit in &lexical_hits {
        hit_by_chunk
            .entry(hit.chunk_id.clone())
            .or_insert_with(|| hit.clone());
        inputs.push(SourceRankInput {
            chunk_id: hit.chunk_id.clone(),
            lexical: Some(hit.score),
            semantic: None,
        });
    }
    let fused = fuse_source_candidates(inputs);
    let mut hits = fused
        .into_iter()
        .filter_map(|candidate| {
            hit_by_chunk
                .get(&candidate.chunk_id)
                .cloned()
                .map(|hit| crate::source_embedding::fused_source_hit(hit, &candidate))
        })
        .collect::<Vec<_>>();
    if request.policy.rerank == SourceRerankMode::Deterministic {
        rerank_hits(
            &mut hits,
            &request.query.terms,
            OffsetDateTime::now_utc(),
            &expanded_ids,
            &expansion_terms,
        );
    } else {
        mark_rerank_off(&mut hits);
    }
    hits.truncate(request.limit.clamp(1, 50) as usize);
    Ok(SourceSearchResult {
        hits,
        degraded,
        mode: SourceSearchMode::Hybrid.as_str().to_string(),
        requested_mode: SourceSearchMode::Hybrid.as_str().to_string(),
        semantic_unavailable: None,
        candidates_scanned: candidates.len(),
        index_path: index.path,
        index_generation: index.generation,
        index_fallback_reason: index.fallback_reason,
        probed_clusters: index.probed_clusters,
        ann_candidates: index.candidates,
        rerank: request.policy.rerank.as_str().to_string(),
        expansion: request.policy.query_expansion.as_str().to_string(),
        expansion_terms,
    })
}

/// Compute the expansion plan before embedding the query. The engine uses this
/// to embed the original and expanded query in one bounded batch.
pub fn plan_expansion(
    store: &Store,
    request: &SourceRetrievalRequest,
    mode: SourceSearchMode,
) -> Result<Vec<String>> {
    if request.policy.query_expansion != SourceQueryExpansionMode::Lexical {
        return Ok(Vec::new());
    }
    let window = if mode == SourceSearchMode::Hybrid {
        MAX_SOURCE_LEXICAL_CANDIDATES
    } else {
        MAX_EXPANSION_SEED_HITS as u32
    };
    let (hits, _) = store.search_source_chunks(
        &request.project_id,
        &request.source_ids,
        &request.query,
        Some(window),
    )?;
    Ok(extract_expansion_terms(
        &request.query.terms,
        &hits,
        request.policy.max_expanded_terms,
    ))
}

/// Build the deterministic expanded query. Original terms stay first, then up
/// to `MAX_QUERY_TERMS` expansion terms. The second pass is recall-oriented:
/// both FTS expressions use OR so a new term can surface a stored chunk that
/// the original AND query could not.
pub fn expanded_source_query(
    query: &SourceSearchQuery,
    terms: &[String],
) -> Option<SourceSearchQuery> {
    if terms.is_empty() {
        return None;
    }
    let mut combined = query.terms.clone();
    let mut seen = combined.iter().cloned().collect::<HashSet<_>>();
    for term in terms {
        if combined.len() >= crate::sources::MAX_QUERY_TERMS {
            break;
        }
        if seen.insert(term.clone()) {
            combined.push(term.clone());
        }
    }
    let raw = combined.join(" ");
    let parsed = source_query(&raw)?;
    // The store tries `all` then `any`; using the OR expression for both means
    // the expansion pass never degrades to an AND that the new term cannot
    // satisfy. Relevance is restored by rerank and the caller limit.
    let any = parsed.any.clone();
    Some(SourceSearchQuery {
        all: any.clone(),
        any,
        terms: parsed.terms,
        raw: parsed.raw,
        prefer_like: query.prefer_like || parsed.prefer_like,
        collections: query.collections.clone(),
        labels: query.labels.clone(),
    })
}

/// Extract a bounded deterministic expansion plan from the first-pass hits.
/// Title tokens are considered before heading and body tokens, and the plan
/// never includes a query term, a stopword, a bare number, or a duplicate.
pub fn extract_expansion_terms(
    query_terms: &[String],
    hits: &[SourceSearchHit],
    max_terms: u32,
) -> Vec<String> {
    let max_terms = max_terms.clamp(MIN_SOURCE_EXPANDED_TERMS, MAX_SOURCE_EXPANDED_TERMS) as usize;
    let mut seen = query_terms.iter().cloned().collect::<HashSet<_>>();
    let mut terms = Vec::new();
    let seeds = hits
        .iter()
        .take(MAX_EXPANSION_SEED_HITS)
        .collect::<Vec<_>>();
    for hit in &seeds {
        push_expansion_tokens(
            &mut terms,
            &mut seen,
            &hit.source_title,
            MAX_EXPANSION_TITLE_TOKENS,
            max_terms,
        );
    }
    for hit in &seeds {
        if let Some(heading) = hit.heading.as_deref() {
            push_expansion_tokens(
                &mut terms,
                &mut seen,
                heading,
                MAX_EXPANSION_HEADING_TOKENS,
                max_terms,
            );
        }
    }
    for hit in &seeds {
        push_expansion_tokens(
            &mut terms,
            &mut seen,
            &hit.content,
            MAX_EXPANSION_CONTENT_TOKENS,
            max_terms,
        );
    }
    terms.truncate(max_terms);
    terms
}

fn push_expansion_tokens(
    terms: &mut Vec<String>,
    seen: &mut HashSet<String>,
    text: &str,
    token_cap: usize,
    max_terms: usize,
) {
    if terms.len() >= max_terms {
        return;
    }
    for token in source_tokens(text).into_iter().take(token_cap) {
        if terms.len() >= max_terms {
            break;
        }
        if !is_expansion_candidate(&token) {
            continue;
        }
        if seen.insert(token.clone()) {
            terms.push(token);
        }
    }
}

fn is_expansion_candidate(token: &str) -> bool {
    let chars = token.chars().count();
    if chars < 2 && !token.chars().any(crate::sources::source_is_cjk) {
        return false;
    }
    if chars > 80 {
        return false;
    }
    if token.chars().all(|character| character.is_ascii_digit()) {
        return false;
    }
    !EXPANSION_STOPWORDS.contains(&token)
}

/// Merge a second-pass lexical window into the first-pass hits. Existing hits
/// keep their original scores and labels; only newly surfaced chunks are
/// marked `expanded: true` and carry the expansion terms that occur in them.
pub fn merge_expanded_hits(
    hits: &mut Vec<SourceSearchHit>,
    expanded_ids: &mut HashSet<String>,
    second: Vec<SourceSearchHit>,
    expansion_terms: &[String],
) {
    let mut present = hits
        .iter()
        .map(|hit| hit.chunk_id.clone())
        .collect::<HashSet<_>>();
    for mut hit in label_lexical_hits(second) {
        if !present.insert(hit.chunk_id.clone()) {
            continue;
        }
        hit.expanded = true;
        hit.expansion_terms = present_expansion_terms(&hit, expansion_terms);
        expanded_ids.insert(hit.chunk_id.clone());
        hits.push(hit);
    }
}

/// Deterministic rerank. Fixed component weights, a fixed freshness half-life,
/// and explicit tie-breaks; there is no model call and no random tie-break.
pub fn rerank_hits(
    hits: &mut [SourceSearchHit],
    query_terms: &[String],
    now: OffsetDateTime,
    expanded_ids: &HashSet<String>,
    expansion_terms: &[String],
) {
    let query_set = query_terms
        .iter()
        .map(|term| term.to_lowercase())
        .collect::<HashSet<_>>();
    for hit in hits.iter_mut() {
        let content_tokens = source_tokens(&hit.content)
            .into_iter()
            .collect::<HashSet<_>>();
        let title_tokens = source_tokens(&format!(
            "{} {}",
            hit.source_title,
            hit.heading.as_deref().unwrap_or("")
        ))
        .into_iter()
        .collect::<HashSet<_>>();
        let lexical_overlap = coverage(&query_set, &content_tokens);
        let title_match = coverage(&query_set, &title_tokens);
        let freshness = freshness_score(hit.source_updated_at.as_deref(), now);
        let cosine = hit
            .semantic_score
            .filter(|score| score.is_finite())
            .map(|score| score.clamp(0.0, 1.0));
        let mut weighted = 0.0;
        let mut weight = 0.0;
        if let Some(cosine) = cosine {
            weighted += cosine * RERANK_WEIGHT_COSINE;
            weight += RERANK_WEIGHT_COSINE;
        }
        if let Some(overlap) = lexical_overlap {
            weighted += overlap * RERANK_WEIGHT_LEXICAL;
            weight += RERANK_WEIGHT_LEXICAL;
        }
        if let Some(title) = title_match {
            weighted += title * RERANK_WEIGHT_TITLE;
            weight += RERANK_WEIGHT_TITLE;
        }
        if let Some(freshness) = freshness {
            weighted += freshness * RERANK_WEIGHT_FRESHNESS;
            weight += RERANK_WEIGHT_FRESHNESS;
        }
        let score = if weight > 0.0 {
            (weighted / weight).clamp(0.0, 1.0)
        } else {
            0.0
        };
        hit.rerank = SourceRerankMode::Deterministic.as_str().to_string();
        hit.rerank_score = Some(score);
        hit.lexical_overlap = lexical_overlap;
        hit.title_match = title_match;
        hit.freshness = freshness;
        if expanded_ids.contains(&hit.chunk_id) {
            hit.expanded = true;
            if hit.expansion_terms.is_empty() {
                hit.expansion_terms = present_expansion_terms(hit, expansion_terms);
            }
        }
    }
    hits.sort_by(compare_reranked);
    for hit in hits.iter_mut() {
        let score = hit.rerank_score.unwrap_or(0.0);
        hit.score = score;
        hit.confidence = score;
    }
}

fn coverage(query: &HashSet<String>, tokens: &HashSet<String>) -> Option<f64> {
    if query.is_empty() {
        return None;
    }
    let matched = query.iter().filter(|term| tokens.contains(*term)).count();
    Some(matched as f64 / query.len() as f64)
}

/// Freshness decays with a fixed 30-day half-life:
/// `1 / (1 + age_days / 30)`, clamped to `0..=1`. Unknown or unparsable
/// timestamps yield no component instead of a fabricated value.
pub fn freshness_score(updated_at: Option<&str>, now: OffsetDateTime) -> Option<f64> {
    let updated_at = updated_at?;
    let parsed =
        OffsetDateTime::parse(updated_at, &time::format_description::well_known::Rfc3339).ok()?;
    let seconds = (now - parsed).whole_seconds().max(0) as f64;
    let days = seconds / 86_400.0;
    Some((1.0 / (1.0 + days / SOURCE_FRESHNESS_HALF_LIFE_DAYS)).clamp(0.0, 1.0))
}

fn compare_reranked(left: &SourceSearchHit, right: &SourceSearchHit) -> std::cmp::Ordering {
    opt_desc(left.rerank_score, right.rerank_score)
        .then_with(|| opt_desc(left.semantic_score, right.semantic_score))
        .then_with(|| opt_desc(left.lexical_score, right.lexical_score))
        .then_with(|| opt_desc(left.title_match, right.title_match))
        .then_with(|| opt_desc(left.freshness, right.freshness))
        .then_with(|| left.chunk_id.cmp(&right.chunk_id))
}

fn opt_desc(left: Option<f64>, right: Option<f64>) -> std::cmp::Ordering {
    let left = left.filter(|value| value.is_finite());
    let right = right.filter(|value| value.is_finite());
    match (left, right) {
        (Some(left), Some(right)) => right.total_cmp(&left),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

fn mark_rerank_off(hits: &mut [SourceSearchHit]) {
    for hit in hits.iter_mut() {
        hit.rerank = SourceRerankMode::Off.as_str().to_string();
        hit.rerank_score = None;
        hit.lexical_overlap = None;
        hit.title_match = None;
        hit.freshness = None;
    }
}

/// Terms from the plan that actually occur in this hit, preserving plan order.
pub fn present_expansion_terms(hit: &SourceSearchHit, expansion_terms: &[String]) -> Vec<String> {
    if expansion_terms.is_empty() {
        return Vec::new();
    }
    let tokens = source_tokens(&format!(
        "{} {} {}",
        hit.source_title,
        hit.heading.as_deref().unwrap_or(""),
        hit.content
    ))
    .into_iter()
    .collect::<HashSet<_>>();
    expansion_terms
        .iter()
        .filter(|term| tokens.contains(*term))
        .cloned()
        .collect()
}

fn lexical_window(limit: u32, policy: &SourceRetrievalPolicy) -> u32 {
    let limit = limit.clamp(1, 50);
    if policy.rerank == SourceRerankMode::Deterministic
        || policy.query_expansion == SourceQueryExpansionMode::Lexical
    {
        limit.saturating_mul(4).clamp(limit, MAX_RERANK_WINDOW)
    } else {
        limit
    }
}

fn exact_candidates(
    store: &Store,
    request: &SourceRetrievalRequest,
    context: &SourceSemanticContext,
    semantic_limit: u32,
) -> Result<Vec<SourceSemanticCandidate>> {
    let rows = store.source_semantic_candidates(
        &request.project_id,
        &context.model,
        context.dimensions,
        context.max_chunks,
        &request.source_ids,
        &request.query.collections,
        &request.query.labels,
        semantic_limit,
    )?;
    Ok(rows)
}

fn merge_ann_candidates(
    target: &mut Vec<SourceSemanticCandidate>,
    extra: Vec<SourceSemanticCandidate>,
) {
    let mut present = target
        .iter()
        .map(|candidate| candidate.chunk_id.clone())
        .collect::<HashSet<_>>();
    for candidate in extra {
        if present.insert(candidate.chunk_id.clone()) {
            target.push(candidate);
        }
    }
}

fn semantic_candidate_hit(candidate: &SourceSemanticCandidate) -> SourceSearchHit {
    SourceSearchHit {
        source_id: candidate.source_id.clone(),
        source_title: candidate.source_title.clone(),
        collection: candidate.collection.clone(),
        labels: candidate.labels.clone(),
        chunk_id: candidate.chunk_id.clone(),
        chunk_no: candidate.chunk_no,
        heading: candidate.heading.clone(),
        content: candidate.content.clone(),
        score: 0.0,
        source: "semantic".to_string(),
        confidence: 0.0,
        origins: vec!["semantic".to_string()],
        lexical_score: None,
        semantic_score: None,
        source_updated_at: candidate.source_updated_at.clone(),
        ..SourceSearchHit::default()
    }
}

/// Stable index-path telemetry shared by every search result.
#[derive(Debug, Clone)]
struct SourceIndexPath {
    path: String,
    generation: Option<u64>,
    fallback_reason: Option<String>,
    probed_clusters: u32,
    candidates: usize,
}

impl SourceIndexPath {
    fn exact(reason: &str) -> Self {
        Self {
            path: "exact".to_string(),
            generation: None,
            fallback_reason: Some(reason.to_string()),
            probed_clusters: 0,
            candidates: 0,
        }
    }

    fn ann(generation: u64, probed_clusters: u32, candidates: usize) -> Self {
        Self {
            path: "ann".to_string(),
            generation: Some(generation),
            fallback_reason: None,
            probed_clusters,
            candidates,
        }
    }
}

/// Convenience used by status and tests: the effective mode helper from the
/// embedding module, re-exported here so retrieval callers have one import.
pub fn source_effective_mode(
    requested: Option<SourceSearchMode>,
    policy: Option<&vp_protocol::types::SourceEmbeddingPolicy>,
) -> (SourceSearchMode, bool) {
    effective_mode(requested, policy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn hit(
        id: &str,
        title: &str,
        content: &str,
        score: f64,
        updated: Option<&str>,
    ) -> SourceSearchHit {
        SourceSearchHit {
            source_id: "src".into(),
            source_title: title.into(),
            chunk_id: id.into(),
            chunk_no: 1,
            content: content.into(),
            score,
            source: "lexical".into(),
            confidence: score,
            origins: vec!["lexical".into()],
            lexical_score: Some(score),
            semantic_score: None,
            source_updated_at: updated.map(str::to_string),
            ..SourceSearchHit::default()
        }
    }

    #[test]
    fn policy_normalization_clamps_every_numeric_bound() {
        let mut policy = SourceRetrievalPolicy {
            ann_enabled: true,
            ann_min_vectors: 0,
            ann_nprobe: 99,
            rerank: SourceRerankMode::Deterministic,
            query_expansion: SourceQueryExpansionMode::Lexical,
            max_expanded_terms: 0,
        };
        normalize_source_retrieval_policy(&mut policy);
        assert_eq!(policy.ann_min_vectors, MIN_SOURCE_ANN_MIN_VECTORS);
        assert_eq!(policy.ann_nprobe, MAX_SOURCE_ANN_NPROBE);
        assert_eq!(policy.max_expanded_terms, MIN_SOURCE_EXPANDED_TERMS);
        assert!(validate_source_retrieval_policy(&policy).is_ok());

        policy.ann_min_vectors = MAX_SOURCE_ANN_MIN_VECTORS + 1;
        assert!(validate_source_retrieval_policy(&policy).is_err());
        policy.ann_min_vectors = MAX_SOURCE_ANN_MIN_VECTORS;
        policy.ann_nprobe = 0;
        assert!(validate_source_retrieval_policy(&policy).is_err());
        policy.ann_nprobe = MAX_SOURCE_ANN_NPROBE;
        policy.max_expanded_terms = MAX_SOURCE_EXPANDED_TERMS + 1;
        assert!(validate_source_retrieval_policy(&policy).is_err());
        policy.max_expanded_terms = MAX_SOURCE_EXPANDED_TERMS;
        assert!(validate_source_retrieval_policy(&policy).is_ok());
    }

    #[test]
    fn rerank_weights_and_components_are_fixed_and_bounded() {
        let now = datetime!(2026-11-12 00:00:00 UTC);
        let mut hits = vec![
            hit(
                "b",
                "Other title",
                "unrelated words only",
                1.0,
                Some("2026-11-12T00:00:00Z"),
            ),
            hit(
                "a",
                "Deterministic leases",
                "deterministic leases and evidence",
                1.0,
                Some("2026-11-12T00:00:00Z"),
            ),
        ];
        hits[0].semantic_score = Some(0.2);
        hits[1].semantic_score = Some(0.2);
        rerank_hits(
            &mut hits,
            &["deterministic".into(), "leases".into()],
            now,
            &HashSet::new(),
            &[],
        );
        assert_eq!(hits.len(), 2);
        let top = &hits[0];
        assert_eq!(top.chunk_id, "a");
        assert_eq!(top.rerank, "deterministic");
        let score = top.rerank_score.unwrap();
        assert!((0.0..=1.0).contains(&score));
        assert_eq!(top.lexical_overlap, Some(1.0));
        assert_eq!(top.title_match, Some(1.0));
        assert_eq!(top.freshness, Some(1.0));
        assert_eq!(top.score, score);
        assert_eq!(top.confidence, score);
        let expected = (0.2 * RERANK_WEIGHT_COSINE
            + 1.0 * RERANK_WEIGHT_LEXICAL
            + 1.0 * RERANK_WEIGHT_TITLE
            + 1.0 * RERANK_WEIGHT_FRESHNESS)
            / (RERANK_WEIGHT_COSINE
                + RERANK_WEIGHT_LEXICAL
                + RERANK_WEIGHT_TITLE
                + RERANK_WEIGHT_FRESHNESS);
        assert!((score - expected).abs() < 1e-12);
        assert_eq!(hits[1].lexical_overlap, Some(0.0));
    }

    #[test]
    fn rerank_ties_are_resolved_by_chunk_id_and_never_random() {
        let now = datetime!(2026-11-12 00:00:00 UTC);
        let make = |id: &str| hit(id, "Title", "body", 1.0, Some("2026-11-12T00:00:00Z"));
        let mut first = vec![make("z"), make("a"), make("m")];
        let mut second = vec![make("m"), make("z"), make("a")];
        let terms = vec!["body".into()];
        rerank_hits(&mut first, &terms, now, &HashSet::new(), &[]);
        rerank_hits(&mut second, &terms, now, &HashSet::new(), &[]);
        let ids = |hits: &[SourceSearchHit]| {
            hits.iter()
                .map(|hit| hit.chunk_id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&first), vec!["a", "m", "z"]);
        assert_eq!(ids(&first), ids(&second));
    }

    #[test]
    fn rerank_off_preserves_order_and_labels_off() {
        let mut hits = vec![
            hit("b", "Title", "body", 0.5, None),
            hit("a", "Title", "body", 0.9, None),
        ];
        rerank_hits(
            &mut hits,
            &["body".into()],
            datetime!(2026-11-12 00:00:00 UTC),
            &HashSet::new(),
            &[],
        );
        let before = hits
            .iter()
            .map(|hit| hit.chunk_id.clone())
            .collect::<Vec<_>>();
        mark_rerank_off(&mut hits);
        assert_eq!(before, vec!["a", "b"]);
        assert!(hits.iter().all(|hit| hit.rerank == "off"));
        assert!(hits.iter().all(|hit| hit.rerank_score.is_none()));
        assert!(hits.iter().all(|hit| hit.lexical_overlap.is_none()));
    }

    #[test]
    fn freshness_uses_a_fixed_half_life_and_rejects_unparsable_values() {
        let now = datetime!(2026-11-12 00:00:00 UTC);
        assert_eq!(freshness_score(None, now), None);
        assert_eq!(freshness_score(Some("not-a-date"), now), None);
        let fresh = freshness_score(Some("2026-11-12T00:00:00Z"), now).unwrap();
        assert!((fresh - 1.0).abs() < 1e-12);
        let month = freshness_score(Some("2026-10-13T00:00:00Z"), now).unwrap();
        assert!((month - 0.5).abs() < 1e-3);
        let future = freshness_score(Some("2030-01-01T00:00:00Z"), now).unwrap();
        assert!((future - 1.0).abs() < 1e-12, "future timestamps clamp to 1");
    }

    #[test]
    fn expansion_plan_is_bounded_deterministic_and_stopword_aware() {
        let hits = vec![
            hit(
                "a",
                "Sandbox runtime",
                "The sandbox runtime uses leases. Ignore the and of this sentence.",
                1.0,
                None,
            ),
            hit("b", "Second note", "unrelated filler", 0.5, None),
        ];
        let terms = extract_expansion_terms(&["leases".into()], &hits, 4);
        assert!(terms.len() <= 4);
        assert_eq!(terms.first().map(String::as_str), Some("sandbox"));
        assert!(terms.contains(&"runtime".to_string()));
        assert!(!terms.contains(&"the".to_string()));
        assert!(!terms.contains(&"leases".to_string()));
        let again = extract_expansion_terms(&["leases".into()], &hits, 4);
        assert_eq!(terms, again, "input order is stable");
        let capped = extract_expansion_terms(&["leases".into()], &hits, 1);
        assert_eq!(capped.len(), 1);
        assert_eq!(capped, vec!["sandbox".to_string()]);
        let zero = extract_expansion_terms(&["leases".into()], &hits, 0);
        assert_eq!(zero.len(), 1, "the low bound clamps to one");
        let high = extract_expansion_terms(&["leases".into()], &hits, 999);
        assert!(high.len() <= MAX_SOURCE_EXPANDED_TERMS as usize);
    }

    #[test]
    fn expansion_labels_only_terms_present_in_the_hit() {
        let hit = hit("a", "Sandbox runtime", "deterministic leases", 1.0, None);
        let labels =
            present_expansion_terms(&hit, &["sandbox".into(), "absent".into(), "leases".into()]);
        assert_eq!(labels, vec!["sandbox".to_string(), "leases".to_string()]);
        assert!(present_expansion_terms(&hit, &[]).is_empty());
    }

    #[test]
    fn expanded_query_keeps_original_terms_first_and_uses_recall_or() {
        let parsed = source_query("deterministic leases").unwrap();
        let query = SourceSearchQuery {
            all: parsed.all,
            any: parsed.any,
            terms: parsed.terms,
            raw: parsed.raw,
            prefer_like: parsed.prefer_like,
            collections: Vec::new(),
            labels: Vec::new(),
        };
        let expanded = expanded_source_query(
            &query,
            &["sandbox".into(), "leases".into(), "runtime".into()],
        )
        .unwrap();
        assert_eq!(expanded.terms[0], "deterministic");
        assert_eq!(expanded.terms[1], "leases");
        assert!(expanded.terms.contains(&"sandbox".to_string()));
        assert_eq!(expanded.all, expanded.any);
        assert!(expanded.all.contains(" OR "));
        assert!(!expanded.all.contains(" AND "));
        assert!(expanded_source_query(&query, &[]).is_none());
    }

    #[test]
    fn merge_marks_only_new_second_pass_hits_as_expanded() {
        let mut hits = vec![hit("a", "Sandbox runtime", "leases", 1.0, None)];
        let mut expanded_ids = HashSet::new();
        let second = vec![
            hit("a", "Sandbox runtime", "leases", 0.4, None),
            hit("b", "Runtime notes", "sandbox evidence", 0.3, None),
        ];
        merge_expanded_hits(&mut hits, &mut expanded_ids, second, &["sandbox".into()]);
        assert_eq!(hits.len(), 2);
        assert!(!hits[0].expanded);
        assert_eq!(hits[0].score, 1.0, "first-pass score is unchanged");
        assert!(hits[1].expanded);
        assert_eq!(hits[1].expansion_terms, vec!["sandbox".to_string()]);
        assert_eq!(hits[1].source, "lexical");
        assert!(expanded_ids.contains("b"));
        assert!(!expanded_ids.contains("a"));
    }

    #[test]
    fn rerank_labels_expanded_ids_without_creating_hits() {
        let mut hits = vec![hit("a", "Runtime", "sandbox evidence", 0.2, None)];
        let mut ids = HashSet::new();
        ids.insert("a".to_string());
        rerank_hits(
            &mut hits,
            &["runtime".into()],
            datetime!(2026-11-12 00:00:00 UTC),
            &ids,
            &[],
        );
        assert!(hits[0].expanded);
        assert_eq!(hits[0].chunk_id, "a");
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn rerank_and_expansion_serialize_no_vector_or_embedding_fields() {
        let mut hits = vec![hit("a", "Runtime", "sandbox evidence", 0.2, None)];
        let mut expanded = HashSet::new();
        expanded.insert("a".to_string());
        rerank_hits(
            &mut hits,
            &["runtime".into()],
            datetime!(2026-11-12 00:00:00 UTC),
            &expanded,
            &["sandbox".into()],
        );
        let result = SourceSearchResult {
            hits,
            rerank: "deterministic".into(),
            expansion: "lexical".into(),
            expansion_terms: vec!["sandbox".into()],
            ..SourceSearchResult::default()
        };
        let value = serde_json::to_value(&result).unwrap();
        let encoded = value.to_string();
        assert!(!encoded.contains("\"vector\""));
        assert!(!encoded.contains("\"embedding\""));
        assert!(!encoded.contains("\"centroid\""));
        assert_eq!(value["rerank"], "deterministic");
        assert_eq!(value["hits"][0]["expanded"], true);
        assert!(value["hits"][0].get("rerank_score").is_some());
    }

    #[test]
    fn lexical_window_is_bounded_and_only_widens_when_needed() {
        let mut policy = SourceRetrievalPolicy {
            rerank: SourceRerankMode::Off,
            ..SourceRetrievalPolicy::default()
        };
        assert_eq!(lexical_window(8, &policy), 8);
        policy.rerank = SourceRerankMode::Deterministic;
        assert_eq!(lexical_window(8, &policy), 32);
        assert_eq!(lexical_window(50, &policy), 50);
        assert_eq!(lexical_window(100, &policy), 50);
        policy.rerank = SourceRerankMode::Off;
        policy.query_expansion = SourceQueryExpansionMode::Lexical;
        assert_eq!(lexical_window(8, &policy), 32);
    }

    #[test]
    fn no_hits_means_no_expansion_terms_or_second_pass_labels() {
        let terms = extract_expansion_terms(&["leases".into()], &[], 8);
        assert!(terms.is_empty());
        let mut hits: Vec<SourceSearchHit> = Vec::new();
        let mut ids = HashSet::new();
        merge_expanded_hits(&mut hits, &mut ids, Vec::new(), &terms);
        assert!(hits.is_empty());
        assert!(ids.is_empty());
    }
}

# ADR-0182: Source retrieval v2 — ANN, deterministic rerank, and lexical expansion

- Status: Accepted
- Date: 2026-11-12
- Depends on: ADR-0174 (semantic project-source index), ADR-0166 (IVF memory index
  patterns), ADR-0170 (tool error contract)
- Supersedes only the retrieval non-goals of ADR-0174; its storage, generation,
  job, accounting, privacy, and citation contracts remain in force.

## Context

ADR-0174 delivered an opt-in source embedding lane with exact-cosine retrieval
and deterministic reciprocal-rank fusion. It deliberately deferred three
quality features:

- approximate nearest-neighbor candidate selection (the exact scan is linear in
  current vectors);
- reranking by signals other than lane rank;
- query expansion when the first query under-specifies the corpus.

The exact scan remains correct, but it has two practical costs. First, every
semantic query decodes and scores up to 512 vectors even when only a small
neighborhood is relevant. Second, lexical-only projects cannot recover a chunk
when the query uses one vocabulary and the chunk uses another, and hybrid
ranking ignores title agreement and recency entirely.

This round adds those features without changing the deterministic contract:

- no model is selected or called by retrieval;
- no vector, centroid, or embedded text is exposed;
- no source write depends on retrieval state;
- no citation is invented;
- every fallback is explicit and stable.

## Decision

### 1. `SourceRetrievalPolicy` is additive and off by default

A project-scoped policy is stored in `project_source_retrieval_policies`,
included in the project manifest, and restored by `db-rebuild`. The derived
index itself is not copied by `db-rebuild`; it rebuilds from restored vectors or
the next maintenance tick:

```text
ann_enabled          false
ann_min_vectors      256       (1..=100_000)
ann_nprobe           8         (1..=32)
rerank               deterministic  (off | deterministic)
query_expansion      off       (off | lexical)
max_expanded_terms   8         (1..=16)
```

Every field is `serde(default)` so older manifests, RPC clients, CLI binaries,
and Swift payloads decode unchanged. Numeric fields are clamped on write and
validated again on manifest restore. A malformed stored policy decodes as the
documented defaults; it can never enable ANN accidentally.

Disabling ANN does not delete the derived index. It stops use immediately and
keeps the generation so a later enable can reuse a validated index. Explicit
`source.embedding.reindex` remains the cleanup path for vectors, and it clears
the derived ANN state in the same transaction.

### 2. Source IVF index

The index is derived operational state, not an event or replay projection. It
can be deleted and rebuilt from `source_embeddings` at any time.

**Scope.** One row per project in `source_ann_index`, scoped by model,
dimensions, index generation, `nlist`, and centroid count. One row per chunk in
`source_ann_assignments`, keyed globally by chunk id and scoped by project,
model, dimensions, index generation, and cluster.

**Training.** The training set is a bounded, stable prefix of current-generation
vectors ordered by chunk id:

- at most 4,096 vectors and 8 MiB of vector bytes;
- only vectors whose model, dimensions, source snapshot generation, structural
  header, and chunk cap match the active embedding policy;
- `nlist = ceil(sqrt(live_vector_count))`, capped at 256, and then capped again
  by the sample size.

k-means is deterministic spherical k-means:

- input is sorted by chunk id before initialization;
- k-means++ initialization uses a splitmix64 stream seeded from a SHA-256 of
  `project | model | dimensions | nlist | sorted chunk ids`;
- the generation is intentionally excluded from the seed, so rebuilding
  unchanged data produces byte-identical centroids;
- iterations are bounded at 16; empty clusters retain their previous centroid;
- ties break by the lower centroid index; final assignments break by chunk id.

Centroids use a versioned little-endian blob:

```text
bytes 0..4   magic "VPSA"
byte  4      version (1)
bytes 5..9   dimensions, little-endian u32
bytes 9..13  centroid count, little-endian u32
bytes 13..   count * dimensions little-endian f32 values
```

Header, dimension, count, exact byte length, finiteness, and non-zero norm are
validated before use.

**Rebuild.** A rebuild reads the current generation, trains, writes a new
monotonically increasing index generation with an expected-generation fence,
deletes all prior assignments in the same transaction, and then assigns up to
20,000 current vectors. If another rebuild won the fence, the result is
`generation_conflict` and nothing changes. Repeated rebuilds over unchanged
vectors are content-idempotent even though the generation advances.

**Incremental maintenance.** The daemon tick that already reconciles source
embeddings also runs bounded ANN maintenance only for projects with
`ann_enabled = true`:

- below `ann_min_vectors`, derived index state is cleared and no work is done;
- missing, undecodable, or metadata-mismatched state rebuilds;
- growth by at least 64 vectors or 50% refreshes centroids;
- otherwise, up to 1,024 unassigned current vectors are assigned in batches of
  256, each batch fenced by the current index generation.

Concurrent embedding writes are never blocked. A vector written after the
training sample is picked up by a later assignment pass.

**Probe.** A semantic query ranks centroids by exact cosine, probes `nprobe`
clusters, and reads a bounded per-cluster window with `ROW_NUMBER()`. The
candidate window is capped at 1,024 vectors and filtered by the same source ids,
collections, labels, current snapshot generation, model, dimensions, structural
header, and `max_chunks_per_source` used by the exact scan. Every returned
candidate is then scored with the same exact cosine function. ANN only changes
which candidates are considered; it never changes how they are scored.

**Fallback.** The exact scan remains authoritative. The engine reports
`index_path: "exact"` and a stable `index_fallback_reason` for:

```text
ann_disabled
embedding_disabled
not_configured
below_min_vectors
not_built
model_mismatch
dimensions_mismatch
index_corrupt: <sanitized detail>
no_assignments
index_read_error: <sanitized detail>
query_invalid
generation_conflict
```

A fallback is not an error and never makes search fail. A query that has no
current vectors still reports the ADR-0174 semantic reasons
(`no_indexed_vectors`, `stale_vectors`, environment names, request codes) before
ANN is considered. Search never builds or repairs the index; it only reports
status.

### 3. Deterministic rerank

Rerank is on by default (`deterministic`) and performs no model call. Each hit
gets bounded components in `0..=1`:

- `semantic_score`: raw cosine, when the hit came from the semantic lane;
- `lexical_overlap`: fraction of query terms present in the chunk body;
- `title_match`: fraction of query terms present in the source title plus
  heading path;
- `freshness`: `1 / (1 + age_days / 30)` from the source `updated_at`, absent
  when the timestamp is missing or unparsable;
- `rerank_score`: the weighted blend below.

Fixed weights:

```text
cosine     0.55
lexical    0.25
title      0.12
freshness  0.08
```

Missing components are removed and the remaining weights are renormalized, so
a lexical-only project is not penalized for having no cosine. Comparisons use
`f64::total_cmp`, then cosine, lexical score, title match, freshness, and
finally chunk id ascending. No time-dependent tie-break exists beyond the
freshness component; within one search the same inputs produce the same order.

`score` and `confidence` become the blended value; raw lane scores stay in
`lexical_score`/`semantic_score` for audit. `rerank` is `off` or
`deterministic` on every hit and on the result. With `off`, ordering from the
lane/fusion is preserved byte-for-byte and component fields are cleared.

Rerank never adds a chunk. It can only reorder and relabel chunks already
returned by lexical search, exact cosine, ANN, or fusion. The citation registry
still assigns `[cN]` only to hits in the final returned set.

### 4. Lexical query expansion

Expansion is off by default. When `query_expansion = lexical`:

1. Run the bounded lexical first pass.
2. Take at most the top three hits and extract at most `max_expanded_terms`
   terms from their titles first, then heading paths, then body tokens.
3. Exclude query terms, duplicates, a fixed English/code stopword set, bare
   numbers, one-character ASCII tokens, and tokens longer than 80 chars.
4. Build a second query: original terms first, then expansion terms, capped at
   the existing 32-term query limit. Both FTS expressions use OR so the new
   term can surface a chunk the original AND query could not.
5. Run one bounded second lexical pass (at most 50 candidates). For hybrid,
   include it in the same fusion. For semantic/hybrid, also embed the expanded
   query in the same bounded embedding batch as the original query; no second
   network round trip is made.
6. Deduplicate by chunk id. First-pass hits keep their score and are not marked
   expanded. Newly surfaced hits get `expanded: true` and the expansion terms
   that actually occur in that hit, in plan order.

The result reports `expansion: off|lexical`, the bounded global
`expansion_terms`, and the same fields per hit. Expansion cannot create a chunk,
a citation, or a model-generated query; it only re-runs deterministic lexical
search with tokens extracted from stored text.

The semantic query embedding batch grows from one input to at most two when
expansion produces at least one term. This is the only added network cost and
only occurs on explicit opt-in.

### 5. Surfaces

RPC:

- `source.retrieval.policy.get`
- `source.retrieval.policy.set`
- `source.embedding.index_status`
- `source.embedding.index_rebuild`

`source.embedding.index_status` reports policy, embedding enabled/configured,
model, dimensions, ready, effective path, generation, vector/assigned/invalid
counts, centroid count, `nlist`, `nprobe`, minimum vectors, coverage, centroid
byte size, fallback reason, last rebuild time, and last sanitized error. It never
returns a centroid, vector, content hash, or embedded text.

`source.embedding.index_rebuild` is explicit and idempotent. It performs no
embedding request. Failures use the Q3 catalog code `SOURCE_INDEX_FAILED`;
policy validation uses `SOURCE_RETRIEVAL_POLICY_INVALID`. Rebuild failures are
recorded as a sanitized `last_error` on an existing row.

CLI:

- `vp source-retrieval-policy <project>`
- `vp source-retrieval-policy-set <project> --enable-ann|--disable-ann
  --ann-min-vectors <n> --ann-nprobe <n> --rerank <off|deterministic>
  --query-expansion <off|lexical> --max-expanded-terms <n> | --clear`
- `vp source-index <project>`
- `vp source-index-rebuild <project>`

Swift Sources adds a bounded Retrieval v2 card (path, vectors, centroids,
nprobe, generation/coverage, fallback, last error), a policy sheet, and an
explicit rebuild action. Every new field is decoded with a default so older
daemons remain usable. The client never reads or writes index bytes.

Search results and hits add serde-defaulted fields: `index_path`,
`index_generation`, `index_fallback_reason`, `probed_clusters`,
`ann_candidates`, `rerank`, `expansion`, `expansion_terms`, `rerank_score`,
`lexical_overlap`, `title_match`, `freshness`, `expanded`,
`source_updated_at`.

### 6. Privacy, limits, and failure behavior

- Centroids and vectors remain daemon-internal derived state. No RPC, CLI, tool,
  event, log, bundle, or UI payload contains them.
- ANN training, probing, and assignment are bounded by the constants in
  `source_ann.rs` and the existing source chunk cap.
- `max_expanded_terms` bounds extraction; the existing query limit bounds the
  second query; the lexical search bound bounds the second pass.
- Rerank tokenizes at most the returned candidate window (50 hits).
- Index maintenance is best-effort and never changes embedding job outcomes or
  source writes. A maintenance error is logged and retried by a later tick.
- Rebuild never serves stale centroids: a new generation deletes prior
  assignments in the same transaction, and an assignment batch whose generation
  no longer matches is refused.
- Search is read-only with respect to derived index state. A corrupt/stale index
  degrades to exact cosine with an explicit reason, not an error or a partial
  silent result.
- No universal recall guarantee is claimed. The recall fixture measures
  recall@k against exact cosine on synthetic separated clusters; real recall
  depends on the embedding distribution, `nlist`, and `nprobe`.

## Non-goals

- No automatic enabling, automatic model selection, provider routing, or
  hidden rerank/expansion model.
- No distributed vector index, cross-project source search, or cross-host
  training. The index is local SQLite derived state.
- No learned rerank weights, LTR training, or embedding-fusion experiments.
- No LLM query expansion, synonym dictionaries, spelling correction, or
  translation.
- No vector or centroid export, backup, bundle inclusion, or replay projection.
  Rebuild is the repair path.
- No change to the lexical tokenizer contract, source write path, approval
  classification, or session budget accounting.
- No claim that ANN improves recall; it is a bounded candidate selector and the
  exact scan remains the fallback and the correctness reference.

## Consequences

Positive:

- Semantic search can avoid scoring the full bounded corpus when an operator
  opts in, while exact cosine remains the reference and fallback.
- Rerank improves title/recency-sensitive ordering deterministically and is
  auditable per hit.
- Lexical expansion recovers stored chunks without a model or a second provider
  round trip, and labels exactly which stored chunks were re-surfaced.
- Every failure mode is explicit, testable, and backward compatible.

Negative:

- ANN adds derived tables and a maintenance path; invalid or stale state must
  fall back, so search latency can vary with index health.
- k-means centroids are a training snapshot; growth is handled incrementally
  until the doubling/64-vector rebuild threshold.
- Rerank changes lexical ordering by default. Raw lexical scores are retained,
  and `rerank = off` restores the prior order, but clients must not assume the
  old lexical order when the default is active.
- Expansion can add one embedding input per semantic search when enabled, and
  the second lexical pass costs a bounded extra query.

## Verification

Rust tests cover policy defaults/normalization/validation, centroid
encode/decode corruption, deterministic k-means and seed stability, deterministic
recall against an exact fixture, nprobe/candidate bounds, generation fencing,
incremental assignment windows, assignment audit/repair, stale/corrupt/too-small
fallbacks, rerank component weights and tie determinism, expansion bounds and
labels, no-vector serialization, engine status/rebuild surfaces, manifest and
`db-rebuild` restore, and ANN/hybrid search end to end. Protocol, CLI, and Swift
tests decode legacy payloads and the new fields. Existing source, memory, agent
citation, and retrieval tests remain green.

# ADR-0174: Semantic project-source index

- Status: Accepted
- Date: 2026-11-11
- Depends on: ADR-0051 (project knowledge sources), ADR-0166 (IVF memory index
  patterns), ADR-0170 (tool error contract)
- Does not depend on: F1 process supervision, F2, F4 remote integration, F5
  budget governor

## Context

Project sources are immutable text snapshots split into heading-aware chunks and
searched with SQLite FTS5 plus a bounded literal fallback. That lexical path is
deterministic, citation-safe, and works without any model or network access. It
cannot answer a query that uses different wording from the source, because FTS5
has no notion of semantic similarity.

Project memory already has an opt-in embedding lane, but its contract does not
fit sources:

- a memory is a small claim with provenance; a source is a large document with
  many chunks and a durable snapshot;
- memory embedding text is title/kind/tags/body; source embedding text must be
  title/heading/chunk body, and a source refresh replaces every chunk;
- memory indexing tracks one job per memory; a source can exceed a bounded
  chunk cap and needs incremental, resumable progress;
- source refresh must invalidate all prior chunk vectors without making the
  source write depend on the embedding endpoint;
- source retrieval returns citation evidence `[cN]`; a semantic lane must never
  invent a chunk or attach a citation that was not returned.

Sharing the memory embedding HTTP client is desirable (one audited
OpenAI-compatible transport), but sharing accounting, generations, jobs, or
ranking weights would conflate two products.

## Decision

### 1. Opt-in policy, name-only secrets

`SourceEmbeddingPolicy` is additive and off by default:

```text
enabled                 false
endpoint_env            None       (environment variable NAME)
model                   None       (no hidden model selection)
dimensions              1536
batch_size              8
max_chunks_per_source   200
retention_days          30
timeout_secs            30
api_key_env             None       (environment variable NAME)
```

The persisted policy contains no URL value and no secret. `endpoint_env` names a
daemon environment variable whose value is read at call time, so changing the
endpoint does not rewrite project state and a backup never contains a URL or
credential. Enabling requires `endpoint_env` and `model`; validation bounds
dimensions (8–8192), batch size (1–64), chunk cap (1–500), retention
(0–3650 days), and timeout (1–300s). Normalization trims names and drops blanks.
A malformed stored policy is treated as disabled, never as an enabled default.

Setting a policy performs no request and queues no work. The operator must call
`source.embedding.reindex` to index existing sources. New or refreshed sources
are queued automatically only while the policy is active.

### 2. Derived state: chunks, generations, jobs, usage

SQLite gains four operational tables:

- `project_source_embedding_policies` — the JSON policy per project.
- `source_embedding_state` — current snapshot generation per source.
- `source_embeddings` — one vector per bounded chunk, with chunk id, chunk
  number, generation, model, dimensions, content hash, status (`current` /
  `stale`), and bytes.
- `source_embedding_jobs` — one job per source with generation, status
  (`pending`, `running`, `done`, `failed`, `dead`), attempts, `chunks_total`,
  `chunks_done`, `next_attempt_at`, and a sanitized last error.
- `source_embedding_usage` — separate call/token/byte accounting.

A source snapshot generation is monotonic. `insert_source` (initial add, import,
refresh, restore) advances the generation in the same transaction, marks every
prior current vector `stale`, and queues one bounded job when the project policy
is active. A title edit also advances the generation because the title is part
of the exact embedded text. Vector writes carry a generation check: a response
from an old generation that arrives after a refresh is ignored instead of
resurrecting superseded content.

Chunk text is exactly:

```text
source: <title, capped 400 chars>
heading: <heading path, capped 800 chars>

<chunk body, capped 7,200 chars>
```

The final string is capped at 8,000 chars and SHA-256 hashed. Retrieval
recomputes the hash from the current chunk; a mismatched vector is skipped even
if its generation still matches. `max_chunks_per_source` caps embedding work at
the first N chunks while every chunk remains lexically searchable.

Jobs are bounded twice: at most two jobs per project per tick and at most
`batch_size` chunks per job per tick. Progress is persisted, so a large source
resumes without re-embedding completed chunks. A successful partial batch
returns the job to `pending` without consuming a retry. Transport failures
increment attempts with exponential backoff; five attempts or a non-retryable
status dead-letters the job. Per-item dimension/encoding mismatches are
non-retryable and dead-letter immediately. A running job older than 15 minutes
is reclaimable, so a daemon crash does not strand a source. Configuration errors
(missing environment variables) are logged and do not burn retry budget.

Embedding failure never fails a source write, a source read, a lexical search,
or a session. The reconciler is a daemon tick
(`VERLINPARK_SOURCE_EMBEDDING_SECS`, default 45; `0` disables). Usage is
project infrastructure accounting and is not charged to a session token or USD
budget.

`retention_days` prunes stale vectors, orphan vectors, and dead jobs older than
the cutoff. `0` keeps them until an explicit reindex/clear. Usage history is
accounting, not derived content, and is retained.

### 3. Retrieval: lexical, semantic, hybrid

`search_sources` and `source.search` accept `mode: lexical|semantic|hybrid`.
Absent means the backward-compatible dynamic default: hybrid when the policy is
active, otherwise lexical. An explicit semantic/hybrid request that cannot run
falls back to the lexical path and returns a stable `semantic_unavailable`
reason. Fallback never returns an error just because the lane is disabled.

The lexical lane is unchanged: same FTS5 all-terms then any-terms queries, same
bounded LIKE fallback and scoring, and the same returned order. In lexical mode
the only additions are `source: "lexical"`, `origins: ["lexical"]`, raw
`lexical_score`, and a presentation-only `confidence` equal to
`score / max(returned score)`. Ranking is byte-for-byte compatible.

The semantic lane performs exactly one bounded query-embedding request and then
an exact cosine scan over at most 512 current-generation vectors. This is not an
ANN index and the implementation makes no approximate-nearest-neighbour claim.
Candidates are filtered by source ids/collections/labels, indexed source status,
current generation, model, dimensions, vector header, and length. Each vector is
decoded and its content hash rechecked before cosine. Scores below `0.05` are
dropped.

Hybrid fusion is deterministic reciprocal-rank fusion with `K = 10`:

```text
raw = 1/(K + lexical_rank) + 1/(K + semantic_rank)   (only present lanes)
score = raw * (K + 1) / 2                            (clamped 0..=1)
```

Two first places equal 1.0; a single first place equals 0.5. Ties sort by the
better lane rank, then chunk id ascending. The result carries the effective
`mode`, requested mode, bounded `candidates_scanned`, per-hit `source`
(`lexical`, `semantic`, or `hybrid`), `origins`, both raw lane scores, and
`semantic_unavailable` on the result and fallback hits. No vector, embedded
text, endpoint URL value, or credential is serialized.

Status and search use stable reason codes:

```text
disabled
not_configured
endpoint_env_missing:<NAME>
api_key_env_missing:<NAME>
invalid_endpoint
no_indexed_vectors
stale_vectors
request_failed:<timeout|http_<status>|connection_failed|invalid_response|...>
empty_query
```

`endpoint_env_present` and `api_key_env_present` expose only booleans. The
status surface reports indexable/indexed/stale/invalid counts, job counts,
coverage, usage, recent jobs, structural integrity, and the current fallback
reason.

### 4. Citations remain exact

The citation registry only assigns `[cN]` when a hit is actually returned by
`search_sources` or `read_source`. Semantic and hybrid hits use the same
registry and the same chunk ids as lexical hits. Fusion never creates a chunk;
the hit map is built only from stored lexical hits and stored semantic
candidates. `read_source` continues to cite the exact requested chunk. Thus a
semantic result cannot introduce a citation for a chunk that was not returned,
and renumbering remains deterministic by first appearance.

### 5. Operations and rebuild

Operator setup is explicit:

1. Export the endpoint URL in the daemon environment and, if needed, the key:

   ```text
   SOURCE_EMBED_ENDPOINT=https://provider.example/v1
   SOURCE_EMBED_KEY=...
   ```

2. Persist the policy using only the environment *names*:

   ```text
   vp source-embedding-policy-set <project> \
     --enable \
     --endpoint-env SOURCE_EMBED_ENDPOINT \
     --model <embedding-model> \
     --dimensions <n> \
     --batch-size 8 \
     --max-chunks-per-source 200 \
     --retention-days 30 \
     --api-key-env SOURCE_EMBED_KEY
   ```

3. Queue existing sources once:

   ```text
   vp source-embedding-reindex <project>
   ```

4. Inspect progress and fallback state:

   ```text
   vp source-embedding <project>
   vp source-search <project> "query" --mode hybrid
   ```

The daemon tick advances jobs automatically after step 3. New sources,
refreshes, and title edits queue their own generations. Disabling the policy
stops new work immediately; `source-embedding-reindex` is the cleanup path that
removes derived vectors and jobs.

RPC:

- `source.embedding.status`
- `source.embedding.reindex`
- `source.embedding.policy.get`
- `source.embedding.policy.set`

CLI:

- `vp source-embedding <project>`
- `vp source-embedding-reindex <project>`
- `vp source-embedding-policy <project>`
- `vp source-embedding-policy-set <project> ...`
- `vp source-search <project> <query> --mode lexical|semantic|hybrid`

Swift Sources gains a bounded status card (indexed/indexable, pending, failed,
stale, stable unavailable reason, last error), a reindex action, and a search
mode picker. The client decodes old payloads with defaults.

The project manifest records `source_embedding_policy`; `db-rebuild` validates
and restores it before restoring sources. Restoring sources advances derived
generations and re-queues jobs when the lane is active. Vectors, jobs, and usage
are operational state and are intentionally not copied by session bundles or
treated as replay/audit projections. `session.replay` leaves them alone, and
`source.embedding.status` exposes structural integrity (valid/invalid/stale/
orphan/generation-mismatched) so an operator can detect drift and repair it by
`source.embedding.reindex`. A future derived-vector replay must be described as
rebuild, not audit.

### 6. Failure matrix

| Condition | Indexing behavior | Search behavior |
| --- | --- | --- |
| Policy absent or disabled | no job is queued | lexical; `disabled` |
| Enabled without endpoint/model | set is rejected; stored policy stays disabled | lexical; `not_configured` |
| Endpoint env missing | reconciler logs a configuration error; attempts do not advance | lexical; `endpoint_env_missing:<NAME>` |
| Key env missing | same as endpoint env | lexical; `api_key_env_missing:<NAME>` |
| Endpoint 5xx/429/timeout | retry with backoff, then dead-letter | lexical; `request_failed:<code>` |
| Dimension/encoding mismatch | immediate dead-letter for that batch | lexical before any vector; `no_indexed_vectors` |
| Source refreshed, new generation | old vectors stale; job queued | lexical; `stale_vectors` until reconciled |
| No current vectors | job pending or disabled | lexical; `no_indexed_vectors` |
| Current vectors, no query match | no failure | semantic/hybrid empty or lexical partial hit set |
| Daemon restarts mid-job | running job reclaimed after 900s and resumes from chunks | previous ready vectors remain usable |

The table intentionally distinguishes a configuration failure from a content
failure. Configuration failures must not consume a source's retry budget,
because the operator can fix them without changing the source snapshot.

## Privacy and security

- Vectors and embedded text never leave the daemon through RPC, CLI, tools,
  events, logs, bundles, or UI. Only counts, scores, chunk excerpts that the
  lexical path already returns, hashes, and stable reason codes are exposed.
- Endpoint and key are environment variable names. Values are read in-process,
  are not persisted, and are never returned. Error bodies are sanitized and
  bounded before storage; request text is not stored by the embedding client.
- The client disables redirects, so a bearer token cannot be forwarded to an
  unconfigured origin.
- Source writes and lexical reads do not depend on the embedding endpoint. A
  malicious or unavailable endpoint cannot block source ingestion.
- The endpoint is operator-configured infrastructure. This ADR does not sandbox
  it, does not add hidden model routing, and does not enable embeddings by
  default.

## Cost and limits

- One query embedding call per semantic/hybrid search; zero for lexical.
- At most 64 texts per request, 64 chunks per job tick, 2 jobs per project per
  tick, 500 chunks per source, and 512 vectors scored per exact scan.
- `max_chunks_per_source` bounds both storage and indexing cost. Chunks above
  the cap remain lexical-only.
- Usage is recorded per call with provider-reported tokens when present and a
  documented `bytes / 4` estimate otherwise. Estimated tokens are labeled.
- Retention prunes stale derived rows; it does not prune usage accounting.

## Non-goals

- No ANN/IVF source index. Exact cosine is the only semantic path this round.
- No automatic enabling, automatic reindex on policy set, model selection,
  provider fallback, reranking, multi-vector retrieval, or query expansion.
- No cross-project source search.
- No source-embedding vectors in session bundles, events, or audit projections.
- No claim that semantic retrieval is fact-checking or citation validation.
- No change to the lexical ranking or to the existing agent tool budgets and
  approval classification.

## Consequences

Positive:

- Source retrieval can match paraphrases without weakening the deterministic
  lexical lane or citation guarantees.
- Failed or disabled embeddings degrade to exact prior behavior with an
  explicit reason.
- Source refresh invalidation is generation-scoped and race-safe.
- Accounting and retention are separate from memory embeddings, so operators can
  reason about source cost independently.

Negative:

- Each source can consume up to 500 chunk vectors; exact scans are linear in
  current vector count and are bounded by a candidate cap.
- Semantic availability depends on daemon environment configuration and on the
  reconciler having caught up; fresh sources can be temporarily lexical-only.
- A title edit re-embeds the source even when the body is unchanged.
- There is no per-session budget hook for source embeddings; this is deliberate
  project infrastructure accounting.

## Verification

Tests cover policy bounds/defaults and name-only secrets, fake
OpenAI-compatible success/dimension-mismatch/timeout behavior, chunk/query
bounds, exact RRF math and tie ordering, generation invalidation and stale
response rejection, retry/dead-letter transitions, disabled/missing-env/stale
fallback, lexical ranking compatibility, citation integrity, bounded results,
and protocol/CLI/Swift decoding. Existing source and memory tests remain green;
no test asserts vectors in tool or RPC output.

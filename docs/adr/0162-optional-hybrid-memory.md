# ADR-0162: Optional hybrid project memory

- **Status:** Accepted
- **Date:** 2026-11-11
- **Owners:** Runtime, Store, Protocol, CLI, macOS
- **Related:** ADR-0074 (project memory), ADR-0078 (retention and FTS),
  ADR-0124 (role-aware context and memory), ADR-0142 (evidence-backed memory),
  ADR-0144 (memory conflicts and health), ADR-0151 (idempotent session
  operations), `docs/CONTEXT_OWNERSHIP_MEMORY_BENCH_PLAN.md` C3

## Context

Project memory is deterministic and lexical. `memory.search` and `memory.brief`
rank FTS5 `bm25` or LIKE candidates with explicit task, path, department, kind,
source, recency, usage, pinned, and stale weights. That behavior is reliable,
cheap, and auditable, but it cannot recall a memory that uses different words
for the same decision. A user who writes "rollout checklist" should be able to
find a memory titled "Release handoff" when the bodies are semantically close.

The tempting implementation is a hidden embedding call: silently send every
memory body to whichever provider is configured for chat, store vectors, and
blend them into every query. That would violate three project rules at once:

1. no hidden model routing;
2. no unbounded background work or silent spend;
3. no safety/protocol behavior that depends on an opaque provider response.

The memory body is also sensitive project content. Sending it to an endpoint is
a privacy decision the operator must make explicitly, per project. A malformed
policy must not accidentally turn into a default cloud call.

This ADR records a deliberately conservative optional lane:

- explicit per-project policy with an endpoint, model, dimensions, and bounds;
- derived, rebuildable embeddings that are never replay/bundle content;
- memory writes never fail because an embedding call fails;
- lexical recall is byte-for-byte unchanged when the lane is disabled;
- semantic failure is visible and falls back to lexical;
- no raw vectors leave the daemon;
- embedding usage is accounted separately from session/task budgets.

## Decision

### 1. Explicit project policy

`MemoryPolicy` gains one additive field:

```rust
#[serde(default)]
pub embedding: Option<MemoryEmbeddingPolicy>,
```

`MemoryEmbeddingPolicy` contains:

| Field | Default | Meaning |
|---|---:|---|
| `enabled` | `false` | Opt-in authorization for automatic calls and hybrid recall |
| `endpoint` | `null` | OpenAI-compatible base URL or full `/embeddings` URL |
| `model` | `null` | Exact model identifier; never inferred |
| `dimensions` | `1536` | Expected vector width, bounded 8–8192 |
| `batch_size` | `16` | Inputs per indexing call, bounded 1–64 |
| `timeout_secs` | `30` | Per-request timeout, bounded 1–300 |
| `max_candidates` | `64` | Maximum semantic candidates per recall, bounded 1–256 |
| `min_similarity` | `0.20` | Minimum cosine similarity in 0–1 |
| `api_key_env` | `null` | Environment variable **name**, never the key |

Validation happens in `memory.policy.set` before persistence:

- any embedding object requires both endpoint and model, even when disabled;
  lexical-only is represented by omitting the object entirely;
- the endpoint must be `http` or `https`, have a host, and contain no userinfo,
  query string, or fragment;
- the model must be 1–200 characters with no whitespace or control characters;
- `api_key_env` must be a valid environment-variable name (letters, digits,
  underscore; no value);
- every numeric bound above is enforced.

A malformed row found in a hand-edited database decodes to `None`, so it cannot
silently enable network access. The daemon logs a warning without the row
contents.

`endpoint` accepts `https://host/v1` or `https://host/v1/embeddings`. The
runtime appends `/embeddings` exactly once when absent. Query strings are
rejected because they are a common way to smuggle a key into a stored policy
that is returned by `memory.policy.get` and copied into the project manifest.

### 2. Disabled means zero network calls

When the policy object is absent, disabled, or not configured:

- `memory.search` and `memory.brief` execute the existing lexical path and
  ranking;
- no environment variable is read for a key;
- no semantic candidate rows are fetched;
- no HTTP client is constructed;
- `memory.embeddings.rebuild` still clears derived vectors/jobs (privacy
  cleanup) but queues nothing;
- the reconciler skips the project.

An explicit `memory.embeddings.test` is the only exception: a configured but
disabled policy may be probed because the operator asked for exactly one
request. This lets a user verify connectivity before turning on automatic
indexing. It returns no vector.

The disabled path is covered by tests that assert zero HTTP requests and by
existing lexical ranking tests, which remain unchanged.

### 3. Derived tables

Two new derived tables hold index state:

```sql
CREATE TABLE memory_embeddings (
    memory_id    TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
    model        TEXT NOT NULL,
    dimensions   INTEGER NOT NULL,
    content_hash TEXT NOT NULL,
    vector       BLOB NOT NULL,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);

CREATE TABLE memory_embedding_jobs (
    memory_id       TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
    model           TEXT NOT NULL,
    status          TEXT NOT NULL, -- pending | indexed | failed | skipped
    attempts        INTEGER NOT NULL DEFAULT 0,
    last_error      TEXT,
    next_attempt_at TEXT,
    updated_at      TEXT NOT NULL
);
```

A separate `memory_embedding_usage` table records calls, reported or estimated
tokens, exact text bytes, model, and whether the endpoint reported usage. It is
project-scoped and intentionally not a `usage_ledger` row.

These are **derived operational state**, not replay projections:

- `session.replay` neither requires nor repairs them;
- bundles do not include vectors or jobs;
- `session.import` preserves memories and queues a fresh index;
- `vp db-rebuild` can recreate the tables but not the vectors;
- `memory.embeddings.rebuild` clears and re-queues them;
- deleting a memory or pruning it removes the rows by cascade plus explicit
  cleanup in the delete transaction.

Storing vectors in SQLite is sufficient for the bounded project-memory scale.
There is no separate vector database, ANN index, or cross-project store.

### 4. Content identity

The exact embedded text is deterministic:

```text
title: <title>
kind: <kind>
tags: <comma-separated tags in stored order>

<body, capped at 12,000 characters with a trailing ellipsis>
```

The final string is capped at 16,000 characters, UTF-8 safe. Its SHA-256 is
stored in `memory_embeddings.content_hash`. There is no whitespace
normalization beyond trimming the title, kind, tags, and body independently; the
hash is of the exact string that was sent.

A memory create, tag update, import, or reactivation from
archived/superseded enqueues a job in the same transaction as the memory write.
Metadata-only updates (use count, verification timestamp, status stale) do not
enqueue. The job enqueue is local SQLite work; no network call occurs inside the
memory transaction, so an unavailable endpoint can never fail a write.

Before any indexing request, the reconciler recomputes the current text and
hash. If a current-model/current-dimension row already has the same hash, the
job transitions to `skipped` with reason `content hash unchanged` and no HTTP
request is made.

Changing the model or dimensions makes old coverage ineligible immediately:
status counts only rows matching the configured model and dimensions, recall
selects only those rows, and the reconciler re-indexes eligible memories.
Changing `min_similarity` affects recall only.

### 5. Jobs, retries, and bounds

The daemon reconciler is separate from the session runtime. It runs on
`VERLINPARK_MEMORY_EMBEDDING_SECS` (default 30, `0` disables) and processes one
bounded batch per enabled project per tick:

- candidate scan limit: `min(batch_size * 2, 128)`;
- request batch limit: policy `batch_size`, at most 64;
- vector width: at most 8,192;
- semantic recall candidates: policy `max_candidates`, at most 256;
- response body cap: 8 MiB;
- HTTP attempts: 3 (initial plus two retries) for timeout/connect and
  `408/409/425/429/500/502/503/504`, respecting `Retry-After` up to 4 seconds;
- indexing attempts: 5, then `failed` until explicit rebuild.

The attempt counter increments before the HTTP call. A crash during a call
therefore leaves a retryable job rather than a lost one. Backoff is exponential:
5s, 10s, 20s, 40s, ... capped at one hour. A failed job is visible with its
sanitized last error and next attempt time. A skipped job records a reason.

Each call records usage exactly once, including partial batches. If the
endpoint does not report token usage, `input_tokens` is a documented
`bytes / 4` estimate and `estimated_tokens` marks the estimated portion. A
status response says how many calls were provider-reported.

Events are appended for `memory.embedding.indexed`, `memory.embedding.failed`,
and `memory.embedding.skipped`. Payloads contain memory id, project id, model,
dimensions, content hash, attempts, and a sanitized error/reason. They never
contain text, vectors, endpoints, or keys. Replay ignores these event kinds;
they are audit, not projection input.

### 6. Hybrid recall and ranking

`memory.search` first obtains the existing lexical candidates and structural
task candidates. If the lane is enabled and configured:

1. Build the query text with the same 8,000-character cap.
2. Read the API key from `api_key_env`, failing before HTTP if absent.
3. Send one bounded query embedding request and record its usage.
4. Fetch at most `max_candidates` active/stale rows with current
   model/dimension embeddings.
5. Recompute each row's text hash and skip any row whose embedding is stale.
6. Decode with the versioned encoding, reject dimension mismatch/non-finite/
   zero-norm vectors, and compute cosine similarity.
7. Drop similarities below `min_similarity`.

Cosine is clamped to `0..=1`; negative similarity is treated as no signal.
Zero-norm and dimension-mismatched vectors are rejected rather than normalized
or padded.

Retrieval status rules are unchanged: archived/superseded rows never surface,
stale rows require `include_stale`, and memory status/conflict/prune behavior is
not altered by the semantic lane.

Ranking is a pure function with four documented stages:

1. Lexical relevance remains `bm25`/LIKE in `0..=1`.
2. Semantic similarity is cosine in `0..=1`.
3. Deterministic reciprocal-rank fusion uses `K = 10` over lexical and
   semantic ranks. Ties in each rank use memory id ascending. The normalized
   RRF contribution rewards agreement between the two lanes.
4. Hybrid relevance is:

   ```text
   hybrid = max(lexical,
                0.45*lexical + 0.35*semantic + 0.20*rrf_normalized)
   score  = hybrid * 0.60
            + task/path/department/kind/source/recency/usage/pinned weights
   ```

   The `max(lexical, ...)` term guarantees hybrid recall never lowers the
   lexical contribution of a row. A semantic-only row can surface because its
   blend is nonzero and its structural weights still apply.

The existing task (0.60), path (0.15 each, cap 0.45), department (0.20), kind
(0.02–0.30), source (0.00–0.20), recency (0.00–0.25), usage (0.03 each, cap
0.15), pinned (0.50), and stale (×0.25) weights are unchanged. Final ties break
by score desc, `created_at` desc, memory id asc, exactly as before.

`MemoryMatch` gains four additive, optional fields:

- `lexical_score`
- `semantic_score`
- `source` (`lexical`, `semantic`, `hybrid`, or `structural`)
- `semantic_unavailable`

`MemorySearchResult` gains an optional `semantic` summary with enabled/model,
lexical and semantic candidate counts, and a fallback reason. Lexical-only
responses remain protocol-compatible.

When semantic recall is requested but fails (missing key, timeout, HTTP error,
parse error, malformed query vector, empty query), ranking stays lexical.
Every returned match carries `semantic_unavailable` with the sanitized reason and
a `semantic_unavailable` reason string. The failure never fails the search and
never silently probes a different endpoint/model.

### 7. Vector encoding

Stored vectors use a versioned little-endian format:

| Bytes | Meaning |
|---:|---|
| 4 | ASCII magic `VPEM` |
| 1 | version `1` |
| 4 | dimensions as little-endian `u32` |
| `4 * dimensions` | IEEE-754 `f32` values, little-endian |

The decoder checks header length, magic, version, exact byte length, policy
dimension equality, supported dimension range, and finite values. The encoder
rejects non-finite values and out-of-range dimensions. Base64 response vectors
are decoded as little-endian `f32` and then pass the same checks.

Raw vectors are never returned by a tool, RPC, event, status, CLI, or UI.
`memory.embeddings.status` exposes counts, model, coverage, recent job metadata,
usage, and sanitized errors only.

### 8. Cost and privacy accounting

Embedding calls are project infrastructure, not task spend. They are recorded in
`memory_embedding_usage` with model, calls, input tokens (reported or estimated),
estimated-token portion, bytes, and provider-reported flag.

They are **not** charged to:

- a session token/USD budget;
- a task/department/project budget;
- a provider-call usage ledger bucket.

This is deliberate. A background reconciliation can run while no session is
active, and a failed recall must not consume a Worker's budget. If an operator
wants embedding costs to count against a project budget, that requires a future
explicit policy field and ADR; it is not inferred here. Historical usage rows
survive an index rebuild because they are accounting, not derived content.

Privacy rule: enabling the policy authorizes the daemon to send memory titles,
bodies, and tags to the configured endpoint. The endpoint is under the
operator's control; it may be local or remote. The key is read from the daemon
environment by name and never persisted. Status/CLI/UI display the model and
sanitized errors, not the key or full request text. Error bodies are redacted for
common bearer/key shapes and capped at 2,000 characters.

Memory content still follows the existing authorization boundary: agents see
only project memories their tools expose, and semantic retrieval does not grant
new read access. The endpoint is a trusted user-configured destination, not a
sandbox. A malicious endpoint can return bad vectors; the parser rejects
malformed/dimension-mismatched/non-finite data, but semantic quality is not a
security boundary.

### 9. Protocol and surfaces

New RPCs, all additive:

- `memory.embeddings.status` → coverage/model/last error/usage/recent jobs
- `memory.embeddings.rebuild` → cleared rows and enqueued memories
- `memory.embeddings.test` → explicit one-request probe, no vector

`memory.policy.get/set` carry the additive `embedding` object. CLI adds
`vp memory-embeddings`, `vp memory-embeddings-rebuild`,
`vp memory-embeddings-test`, and embedding flags on `vp memory-policy-set`.
Manage Dash adds policy fields, probe/rebuild actions, and a health line in the
memory card. None of these surfaces display vectors or secrets.

### 10. Failure and recovery matrix

| Condition | Behavior |
|---|---|
| Policy disabled/absent | Zero calls; lexical ranking unchanged |
| Enabled but incomplete policy | Rejected at set time; hand-edited row ignored |
| Missing API key env | Recall falls back with reason; reconciler logs config error without burning job attempts |
| Endpoint timeout/429/5xx | Three HTTP attempts, then retryable job failure with backoff |
| Endpoint 400/401/403/404/422 | Three attempts only if the client class is retryable; non-retryable 4xx marks the job terminal until rebuild |
| Partial batch | Valid items indexed; invalid/missing items fail independently |
| Dimension mismatch | Per-item failure; no vector stored |
| Malformed/non-finite/zero-norm vector | Per-item failure; no vector stored |
| Content changed | Pending job; reconciler recomputes hash and re-indexes |
| Pending re-index | `indexed` drops for that row until the new vector commits |
| Corrupt vector blob | `invalid` increments; row is re-queued even if its job says indexed |
| Model/dimension changed | Old coverage ineligible; status resets; recall ignores old rows |
| Memory deleted/pruned | Embedding and job rows removed |
| Memory archived/superseded | Embedding/job rows removed; reactivation queues a new job |
| Daemon crash during call | Attempt counter already persisted; job retried |
| Rebuild while enabled | Derived rows cleared; active/stale memories queued; usage retained |
| Rebuild while disabled/absent | Derived rows cleared; nothing queued; usage retained |
| Semantic query failure | Lexical fallback with `semantic_unavailable`; search succeeds |

### 11. Operational runbook

Enable and verify:

1. Save the policy with `endpoint`, `model`, `dimensions`, and optional
   `api_key_env`. Save replaces the whole policy object; resend lifecycle fields.
2. Call `memory.embeddings.test`. It performs exactly one request; `ok=true`
   means the endpoint returned a vector with the configured dimensions.
3. Enable the policy. The reconciler (default 30s tick) indexes active/stale
   memories in bounded batches.
4. Watch `memory.embeddings.status` until `coverage` approaches 1.0. Pending
   rows mean work is queued; failed rows carry a sanitized last error.

Interpret counters:

| Counter | Meaning |
|---|---|
| `indexable` | Active/stale memories eligible for semantic retrieval |
| `indexed` | Valid current-model/dimension rows with no pending/failed job |
| `invalid` | Structurally corrupt vectors; re-queued automatically |
| `pending` | Jobs awaiting the reconciler or retry time |
| `failed` | Last attempt failed; may still retry unless terminal |
| `skipped` | Intentionally skipped (hash unchanged/disabled work) |
| `coverage` | `indexed / indexable`; zero with no memories |
| `usage.calls` | Successful embedding HTTP batches (index + query) |
| `usage.estimated_tokens` | Portion of input tokens estimated from bytes |

Recovery:

- **Endpoint changed**: update the policy. Current-model coverage resets and the
  reconciler reindexes. Old vectors are ignored immediately.
- **API key rotated**: update the daemon environment and restart; the key is
  never stored in the policy or database. A terminal auth failure can be reset
  with `memory.embeddings.rebuild`.
- **Corrupt vectors**: status reports `invalid`; the reconciler re-queues them.
  A manual rebuild is the deterministic repair.
- **Disable and remove derived data**: disable the policy, then run
  `memory.embeddings.rebuild`. It clears vectors/jobs without network access and
  queues nothing; usage history remains for accounting.
- **Suspect privacy exposure**: disabling stops automatic calls immediately.
  Clearing removes derived vectors. Provider-side copies are outside the
  daemon's control and must be handled under the operator's endpoint contract.
- **Budget concern**: usage is project infrastructure accounting and does not
  appear in session/task budgets. Set provider-side limits or leave the lane
  off if that accounting is insufficient.

### 12. Migration and compatibility

- Existing SQLite databases add `embedding_json` to
  `project_memory_policies` and create three derived tables with
  `CREATE TABLE IF NOT EXISTS`.
- Existing policies decode with `embedding = None`; behavior is unchanged.
- Existing memories have no jobs/vectors. When a policy is enabled, missing
  embeddings are selected even without an explicit job, so the first
  reconciliation backfills the project.
- Replay/repair never fabricates vectors. Importing a bundle requires a fresh
  index pass.
- Old clients that send a whole `MemoryPolicy` without `embedding` clear the
  optional lane because policy set is a replacement, matching existing policy
  semantics. In-repo CLI/Swift clients always resend the field. This is a known
  compatibility limitation, not a hidden fallback.
- `memory.search` accepts `lexical_only` for a per-request bypass even when the
  project lane is enabled. It is protocol-additive and never changes project
  policy.

### 13. Threat model and validation

The endpoint is trusted user configuration. A malicious endpoint can return
incorrect but structurally valid vectors; retrieval quality is not a security
boundary. The daemon defends against:

- unbounded response size (8 MiB cap);
- unbounded batch/dimension/candidate counts;
- non-finite, zero-norm, wrong-dimension, truncated, or unknown-version vectors;
- query strings or userinfo in stored endpoint URLs;
- key material in `api_key_env` (name validation);
- key/body/log leakage through sanitized error redaction and bounded status;
- authenticated endpoint redirects (redirects are disabled rather than
  forwarding a bearer token to an unconfigured origin);
- stale text hashes contributing old semantics;
- model/dimension changes serving old coverage;
- semantic recall bypassing status/stale filtering;
- writes failing because the endpoint is unavailable.

It does not defend against a compromised daemon process, a provider that
retains request content, a malicious local user with database access, or
network interception of an explicitly configured plain-HTTP remote endpoint.
Those are deployment/trust decisions.

## Consequences

Positive:

- Paraphrase recall is available without weakening deterministic lexical
  behavior.
- Every automatic call requires an explicit per-project enable and endpoint.
- Memory writes and agent runs cannot be failed by an embedding outage.
- Derived state can be inspected, rebuilt, and deleted without touching memory
  truth.
- Embedding cost is visible and isolated from session/task budgets.
- Ranking remains pure, deterministic, bounded, and testable.

Negative:

- Enabling the lane sends memory bodies to an external (or local) endpoint;
  operators must treat that as a data-egress decision.
- Semantic recall adds one query embedding call and up to 256 vector decodes per
  search/brief when enabled.
- There is no ANN index; each recall scans a bounded candidate set.
- The reconciler is single-daemon and tick-based, not a distributed indexing
  service. A multi-daemon deployment could duplicate calls; usage is
  at-least-once per completed call.
- Embedding cost is not automatically included in project budgets.
- A changed `min_similarity` affects recall immediately but does not re-index.
- Semantic quality depends entirely on the user's model; there is no
  benchmark-based automatic model choice.

## Explicit non-goals

- No hidden provider/model routing or automatic fallback provider.
- No automatic enablement, sampling, or "try embeddings if lexical is empty".
- No cross-project vector store.
- No ANN/HNSW/IVF index or vector database.
- No raw vector or embedding-text exposure through tools/RPC/UI.
- No retry of a partially streamed provider response (embeddings are
  non-streaming).
- No change to memory conflict detection, staleness, retention, or access
  control.
- No claim that embeddings are replayable bundle content or a backup of memory
  truth.

## Tests

- Fake OpenAI-compatible `/embeddings` server: success, explicit dimensions,
  provider usage, absent usage estimate, base64 vectors, dimension mismatch,
  malformed/non-finite/zero vectors, partial batches, missing entries,
  out-of-range indexes, timeout, 429→503→success retry, exhausted 500 retries,
  and non-retried 401.
- Disabled policy: zero HTTP requests; lexical ranking unchanged; memory write
  succeeds when the endpoint is down.
- Content hash re-index, model change coverage reset, rebuild/status, delete and
  prune cleanup.
- Ranking determinism, task/scope boost, semantic paraphrase recall,
  `min_similarity`, zero vectors, long-body caps, stale hash rejection, and
  lexical fallback with `semantic_unavailable`.
- Additive protocol decode, `lexical_only` bypass, model-scoped status counters,
  terminal auth failure handling, corrupt-vector repair, deleted-memory
  orphan prevention, manifest policy capture, and disabled-mode cleanup.
- Protocol decode defaults, CLI status formatting, and Swift model/status
  decoding. Existing lexical memory tests remain green.

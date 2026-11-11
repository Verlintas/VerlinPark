# ADR-0166: Opt-in IVF ANN index for hybrid project memory

- **Status:** Accepted
- **Date:** 2026-11-11
- **Owners:** Runtime, Store, Protocol, CLI, macOS
- **Related:** ADR-0162 (optional hybrid memory), ADR-0074 (project memory),
  ADR-0078 (retention and FTS), ADR-0142 (evidence-backed memory),
  ADR-0163 (cost per solved task), `docs/CONTEXT_CHECKPOINT_ACP_ANN_BACKUP_PLAN.md`
  D3

## Context

ADR-0162 added an optional hybrid semantic lane. Recall fetches at most
`max_candidates` (default 64, maximum 256) current vectors and computes exact
cosine. That bound is deliberate: it caps SQLite reads, allocations, and query
latency, and it keeps the lexical/structural ranking deterministic.

The bound is also a recall ceiling. On a project with thousands of indexed
memories, the newest 64 rows are unlikely to contain the best semantic match for
an arbitrary query. A larger exact window raises cost linearly for every
search. Operators with large projects asked for an opt-in approximate index with
bounded rebuild cost, while keeping the exact lane as the default and as the
fallback when approximate state is unhealthy.

The design constraints are stricter than "add a vector database":

1. No hidden model or provider routing. The embedding policy remains the only
   source of model selection.
2. No unbounded background work. Rebuilds and assignments must have hard vector,
   byte, and iteration limits.
3. No new safety boundary. Approximate retrieval must not bypass memory status,
   stale filtering, project isolation, approvals, budgets, or the path jail.
4. No raw vectors, centroids, or embedded text through RPC, CLI, UI, events, or
   logs. Existing status is aggregate-only; the ANN index must preserve that.
5. Derived index state is rebuildable operational state, not truth and not a
   replay projection. Losing it must degrade gracefully to exact scan.
6. Determinism matters more than best-in-class recall. The same stored vectors
   and policy must produce the same centroids, assignments, and ranking ties on
   every run and platform.
7. No claim of universal recall. The acceptance fixture measures recall@k
   against exact scoring; real recall depends on the embedding distribution,
   `nlist`, and `nprobe`.

This ADR records the IVF implementation. It updates the "no ANN index" non-goal
in ADR-0162 to "no ANN index unless explicitly opted in per project".

## Decision

### 1. Additive policy

`MemoryEmbeddingPolicy` gains five `serde(default)` fields:

| Field | Default | Bounds | Meaning |
|---|---:|---:|---|
| `index` | `scan` | `scan`, `ivf` | Retrieval index mode |
| `ivf_nlist` | `64` | 4–1024 | Target k-means centroid count |
| `ivf_nprobe` | `8` | 1..=`nlist` | Centroids probed per recall |
| `ivf_min_vectors` | `256` | 1–10,000,000 | Below this, use exact scan |
| `ivf_rebuild_vectors` | `4096` | `min_vectors`–10,000,000 | Growth threshold for a rebuild |

Validation is part of `validate_embedding_policy`, the single policy validation
path:

- `nlist` is bounded;
- `nprobe` cannot be zero or exceed `nlist`;
- `min_vectors` and `rebuild_vectors` are bounded;
- `rebuild_vectors >= min_vectors`.

A disabled policy performs zero index work even if `index = ivf`. An absent
embedding object has no index mode, no index reads, and no index writes. Scan is
the default; existing policies decode and behave exactly as before.

The mode is visible through `memory.policy.get/set`, CLI
`--embedding-index`, and the Swift policy card. There is no hidden switch and no
automatic promotion from scan to IVF.

### 2. Derived tables

Two tables hold derived state:

```sql
CREATE TABLE memory_embedding_index (
    project_id           TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    model                TEXT NOT NULL,
    dimensions           INTEGER NOT NULL,
    generation           INTEGER NOT NULL,
    nlist                INTEGER NOT NULL,
    cluster_count        INTEGER NOT NULL,
    vector_count         INTEGER NOT NULL,
    centroids            BLOB NOT NULL,
    updated_at           TEXT NOT NULL,
    last_error           TEXT,
    fallback_count       INTEGER NOT NULL DEFAULT 0,
    last_fallback_at     TEXT,
    last_fallback_reason TEXT
);

CREATE TABLE memory_embedding_assignments (
    memory_id  TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
    generation INTEGER NOT NULL,
    cluster    INTEGER NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX idx_memory_embedding_assignments_generation
    ON memory_embedding_assignments(generation, cluster);
```

`generation` is a monotonically increasing integer per project. A new build
writes a complete centroid set and deletes assignments from older generations in
one transaction. Recall joins assignments on the current generation, so it can
never mix generations.

A generation-0 row with an empty centroid blob is used only for diagnostics
(`last_error`, fallback counter). It cannot serve recall: validation rejects an
empty or structurally invalid blob and falls back to the exact scan.

These tables are **operational derived state**:

- `session.replay` neither requires nor repairs them;
- bundles and event mirrors do not contain centroids, assignments, or vectors;
- `vp db-rebuild` recreates the schema but not the contents;
- `memory.embeddings.rebuild` and `memory.embeddings.index.rebuild` can clear
  them;
- deleting/pruning a memory removes its assignment by cascade plus explicit
  cleanup;
- archiving/superseding removes the assignment explicitly;
- disabling the lane stops all work but does not silently delete existing
  derived state; the explicit rebuild/clear path is the privacy cleanup.

### 3. Deterministic k-means

A build samples at most 8,192 valid vectors and at most 64 MiB of vector bytes,
whichever is smaller. The sample query orders rows by a 16-hex-character prefix
of the existing SHA-256 content hash and then memory id. That is stable for an
unchanged database and avoids "first N memory ids" bias.

`build_kmeans`:

1. Sorts the sample by memory id. Input order cannot change the result.
2. Rejects duplicate ids, wrong dimensions, non-finite values, and zero-norm
   vectors.
3. Computes a 64-bit seed from SHA-256 of project id, model, dimensions, `nlist`,
   and the sorted memory ids. Generation is intentionally excluded so an
   explicit rebuild of unchanged data produces identical centroids.
4. Initializes centroids with deterministic k-means++ using a splitmix64 stream.
   The first point is `next_u64() % n`; subsequent points use distance-squared
   weights. If no distance mass remains, the lowest not-yet-chosen index is
   used.
5. Runs at most 24 spherical k-means iterations. Centroids are L2-normalized
   means; cosines are used for assignment because that matches recall scoring.
6. An empty cluster retains its previous centroid. A zero mean never becomes a
   centroid.
7. Ties in nearest-centroid selection and probe ordering break by the lower
   centroid index. Assignment output is keyed by memory id.

The cluster count is `min(nlist, sampled vectors)`. A project with fewer than
`ivf_min_vectors` valid vectors never builds; a project with at least the
minimum but fewer than `nlist` vectors uses one centroid per vector.

The centroid blob is a versioned little-endian format:

| Bytes | Meaning |
|---:|---|
| 4 | ASCII magic `VPIV` |
| 1 | version `1` |
| 4 | dimensions as little-endian `u32` |
| 4 | centroid count as little-endian `u32` |
| `4 * dimensions * count` | IEEE-754 `f32` values, little-endian |

The decoder checks header length, magic, version, policy dimension equality,
supported dimension/count ranges, exact byte length, every value's finiteness,
and every centroid's non-zero L2 norm. Status uses the same header/length checks
without decoding values.

### 4. Build, assignment, and generation lifecycle

A build is transactional with respect to the index tables:

- the new centroid blob and metadata are upserted;
- the returned generation is `max(requested, stored + 1)`, so two racing builds
  cannot reuse a generation;
- assignments from every other generation for the project are deleted in the
  same transaction.

After a build, a bounded assignment pass decodes unassigned vectors and writes
their nearest cluster. Maintenance assigns at most 2,048 vectors per daemon
tick; an explicit rebuild assigns at most 8,192. A vector written while a build
is running is never lost:

- if it was in the sample, it is covered by the new generation;
- if it was written after the sample, it has no assignment for the new
  generation and is picked up by a later assignment pass;
- `record_memory_embedding_success` deletes the memory's old assignment in the
  same transaction as the vector upsert, so a changed vector cannot keep a stale
  cluster.

Maintenance rebuilds when:

- no index exists;
- the stored blob cannot be decoded;
- model, dimensions, or `nlist` differs from policy;
- live vector count crosses `ivf_rebuild_vectors` for the first time; or
- live vector count has grown by another `ivf_rebuild_vectors` since the stored
  `vector_count`.

Otherwise it only assigns missing/stale vectors. If live valid vectors fall
below `ivf_min_vectors`, real centroid state is deleted and recall uses exact
scan; a generation-0 diagnostic row may remain so the fallback reason and counter
are observable. The exception is a row whose model/dimensions/nlist no longer
match: it is already unusable, so maintenance keeps it until current-model
vectors exist, preserving the monotonic generation and fallback diagnostics
across a migration. Assignment writes are fenced by generation: if a newer
build committed first, the old batch writes nothing.

The daemon embedding reconciler runs one bounded maintenance pass before and
after each embedding batch; the pre-client pass lets a corrupt/stale index
repair itself while the endpoint or API key is unavailable. Delete and prune also run one bounded maintenance pass. An explicit
`memory.embeddings.index.rebuild` always creates a new generation when the policy
is eligible, and otherwise returns an honest `rebuilt = false` plus reason.

### 5. Recall semantics

When `index = ivf`, the policy is enabled/configured, and a query vector exists:

1. Load the stored index row and live counts. Counts include only active/stale
   memories with the current model/dimensions and a structurally valid vector
   header.
2. Validate scan mode, enabled/configured state, model, dimensions, `nlist`,
   minimum vector count, centroid header/length/count, and assignment presence.
   Any failure returns a fallback reason; it does not fail search.
3. Decode centroids and rank them by exact cosine against the query. Probe
   `nprobe` nearest centroids, tie-breaking by lower id.
4. Fetch at most
   `min(max_candidates * nprobe, 2048)` assigned vectors from those clusters,
   with the same status/stale filtering as the exact lane.
5. Recompute each memory's content hash, decode each vector with the `VPEM`
   checks, compute exact cosine, drop values below `min_similarity`, sort by
   similarity desc then memory id asc, and keep at most `max_candidates`.

Those results enter the existing deterministic RRF/lexical fusion unchanged.
The final score still applies task (0.60), path (0.15 each, cap 0.45),
department (0.20), kind/source, recency, usage, pinned, and stale weights. IVF
changes which semantic candidates enter fusion; it does not change the fusion
formula or return vectors.

`MemorySemanticSummary` gains additive fields:

- `index_source`: `scan` or `ivf` (absent on older daemons);
- `index_generation`;
- `index_fallback`;
- `probed_clusters`;
- `candidate_vectors`.

`fallback` continues to describe semantic-lane failure. `index_fallback`
describes only the ANN probe. This distinction matters: an index can be corrupt
while the exact semantic scan still succeeds. In that case semantic ranking stays
active, `index_source = scan`, and `index_fallback` carries the reason.

Explicit fallback reasons include:

| Reason | Meaning |
|---|---|
| `scan_mode` | Policy explicitly uses exact scan |
| `embedding_disabled` | IVF policy exists but the lane is disabled |
| `not_configured` | Endpoint/model incomplete |
| `below_min_vectors` | Live valid vector count below policy minimum |
| `not_built` | No usable generation yet |
| `model_mismatch` | Stored model differs from policy |
| `dimensions_mismatch` | Stored dimensions differ |
| `nlist_mismatch` | Stored centroid count policy differs |
| `index_corrupt: ...` | Magic/version/length/dimension/count/value failure |
| `no_assignments` | Generation has no assigned vectors yet |
| `query_invalid` | Query vector wrong dimension/finite/zero norm |
| `index_read_error: ...` | Store error; search still scans exactly |

A fallback increments a diagnostic counter in the index row when one exists, or
creates a generation-0 diagnostic row. This is the only write on the recall read
path, and only for an actual IVF fallback. Scan mode never touches it.

### 6. Status and operator surfaces

`memory.embeddings.status` gains an optional `index` object:
mode, ready, generation, model, dimensions, live vector count, assigned count,
cluster count, `nlist`/`nprobe`, coverage, minimum/rebuild thresholds, blob bytes,
last rebuild, last build error, fallback count/timestamp/reason, and current
fallback reason.

New additive RPCs:

- `memory.embeddings.index.status` returns the same index object for one project;
- `memory.embeddings.index.rebuild` returns `rebuilt`, reason, generation,
  vector count, assigned count, cluster count, and `nlist`.

CLI adds `vp memory-embeddings-index` and
`vp memory-embeddings-index-rebuild`, plus `--embedding-index`, `--ivf-nlist`,
`--ivf-nprobe`, `--ivf-min-vectors`, and `--ivf-rebuild-vectors` on
`vp memory-policy-set`. Manage Dash's semantic card shows index mode, readiness,
generation, coverage, assigned/vector counts, cluster count, fallback count, and
a separate "Rebuild ANN index" action gated on an enabled, configured IVF
policy.

None of these surfaces display centroids, vectors, content hashes, or embedded
text. `bytes` is the centroid blob size, not content.

### 7. Cost and bounds

Hard limits:

- sample: 8,192 vectors and 64 MiB;
- centroids: 1,024;
- k-means iterations: 24;
- probe candidates: 2,048 before exact scoring;
- returned semantic candidates: policy `max_candidates`, at most 256;
- maintenance assignments: 2,048 per tick;
- explicit rebuild assignments: 8,192;
- stored centroid blob: 64 MiB.

A build reads at most the sampled vectors and one centroid blob. Recall reads
one index row, one count aggregate, and one bounded candidate query. No rebuild
path calls the embedding endpoint; it only reads already stored vectors. Index
work is therefore free of provider spend, while embedding calls remain in the
existing project usage ledger.

Rebuild and assignment run in `spawn_blocking` SQLite work. Memory writes are
short transactions and are never held open across network calls; a concurrent
write either joins the current generation or is picked up later. There is no
global index lock across projects.

### 8. Privacy and exposure

The index is derived from vectors that already exist in `memory_embeddings`.
It does not add a new data source, new provider call, or new read permission.
Centroid blobs and assignments are never returned through:

- tool results;
- JSON-RPC responses;
- events or event mirrors;
- logs or tracing fields;
- CLI output;
- Swift models or UI.

Status exposes aggregate counts and a sanitized error string. Build errors are
sanitized with the embedding error redactor (bearer/key shapes, newlines) and
capped at 2,000 characters. A corrupt blob error includes only structural
information (magic/version/dimension/length), never vector values.

Deleting/pruning memories and explicit embedding rebuild/clear remove
assignments and centroids. Backups/bundles are unchanged: they do not include
derived ANN state.

### 9. What this is not

- Not HNSW, product quantization, GPU, or a separate vector database.
- Not a distributed index, cross-host coordination, or multi-writer consensus.
  Generation fencing is same-host SQLite transaction semantics.
- Not an exact-search replacement. Scan remains the default and the fallback;
  IVF is opt-in.
- Not a universal recall guarantee. The fixture measures recall@k for
  `nlist=64`, `nprobe=8` on synthetic clusters; production recall is
  data-dependent.
- Not a semantic-quality claim. Embedding quality is entirely the configured
  model's responsibility.
- Not replayable or backed up. It is rebuildable operational state.
- Not a new safety gate. It cannot bypass memory status, project scope,
  approvals, sandbox, budgets, or verification.
- Not incremental k-means with centroid drift guarantees. Incremental work is
  nearest-centroid assignment; centroid quality is refreshed by bounded
  rebuilds, not by online updates.
- Not a change to lexical/structural ranking, RRF, or the existing hybrid
  formula.

### 10. Alternatives considered

**Keep exact scan only.** This is the status quo and remains the default. It is
simple, deterministic, and exact over the candidate window, but its recall
ceiling is policy `max_candidates`, not the project size. Raising the window
costs every search and makes latency depend on total vectors. Rejected as the
only large-project option, retained as default/fallback.

**HNSW or another graph index.** HNSW typically gives better recall/latency at
high `nlist`, but needs incremental graph maintenance, deletion handling, and
more complex persistence. It is harder to make bit-for-bit deterministic across
platforms and harder to audit with SQL aggregates. Rejected for this round;
revisit if measured IVF recall is insufficient on real project data.

**Product quantization / compression.** Reduces bytes but adds training and
approximation error on top of IVF. The current bound is 64 MiB per centroid
blob and a bounded sample, which is acceptable for project memory. Deferred.

**External vector database (pgvector, Qdrant, LanceDB).** Adds a service,
credential, backup, and consistency surface outside the daemon's SQLite
transaction and privacy boundary. Rejected. The daemon remains local-only and
SQLite remains the single operational store.

**Random projection / LSH.** Cheap and incremental, but harder to reason about
cluster coverage and to report cluster-level status. IVF's centroid assignment
maps naturally to the existing vector-derived tables and operator rebuild
concept.

**Online centroid updates.** Tempting for write-heavy projects, but makes
generation semantics and determinism harder: a recall could observe centroids
mid-update. Bounded batch assignment plus threshold rebuilds is simpler and
matches the existing daemon tick model. Rejected for v1.

**Storing centroids in a flat file or artifact.** External files complicate
cascade cleanup, backup exclusion, and project isolation. SQLite BLOB columns
inside the existing store preserve transactional generation changes. Rejected.

### 11. Failure and recovery matrix

| Condition | Behavior |
|---|---|
| Embedding policy absent | No index read/write; lexical-only |
| Policy disabled (scan or ivf) | No index work; recall lexical-only; existing derived state preserved |
| Policy enabled, `index = scan` | No index work; exact semantic scan |
| Valid index, coverage < 100% | Probes assigned vectors; status shows coverage; maintenance assigns remaining |
| Fewer than `ivf_min_vectors` | Exact scan; reason `below_min_vectors` |
| No index yet | Exact scan; reason `not_built` |
| Stored model differs | Exact scan; reason `model_mismatch`; maintenance rebuilds after re-embedding |
| Stored dimensions differ | Exact scan; reason `dimensions_mismatch` |
| Stored `nlist` differs | Exact scan; reason `nlist_mismatch`; maintenance rebuilds |
| Centroid header/version/length corrupt | Exact scan; reason `index_corrupt`; explicit or maintenance rebuild repairs |
| Centroid value non-finite/zero norm | Full decode fails; exact scan; reason `index_corrupt` |
| No assignments in generation | Exact scan; reason `no_assignments`; maintenance assigns |
| Out-of-range/missing/stale assignment | Not counted as assigned; audit deletes; maintenance refills |
| Query vector wrong dimension/zero/non-finite | Exact scan; reason `query_invalid` |
| Store read error during probe | Exact scan; reason `index_read_error`; search succeeds |
| Concurrent build wins generation | Old assignment batch writes nothing; recall uses new generation |
| Concurrent memory write during build | Vector either in sample or assigned by a later pass; never lost |
| Memory deleted/pruned | Assignment cascade plus explicit cleanup; maintenance updates coverage |
| Memory archived/superseded | Assignment removed; not eligible for recall |
| Explicit rebuild while below minimum | Derived state cleared; `rebuilt = false`, reason `below_min_vectors` |
| Explicit rebuild in scan mode | Derived state cleared; `rebuilt = false`, reason `index_mode_scan` |
| Explicit rebuild while disabled | No-op; reason `embedding_disabled`; existing state preserved |

### 12. Operational runbook

Enable:

1. Save an embedding policy with endpoint, model, and dimensions.
2. Probe with `memory.embeddings.test`; confirm a vector with the expected
   dimensions is returned.
3. Enable the lane and wait for `memory.embeddings.status` coverage.
4. Select `index = ivf`, set `ivf_nlist`, `ivf_nprobe`, `ivf_min_vectors`, and
   `ivf_rebuild_vectors` in the same policy.
5. The reconciler builds after the minimum is reached. Check
   `memory.embeddings.index.status` for `ready`, generation, assigned count, and
   cluster count.

Tuning guidance (not a quality guarantee):

- `ivf_nlist` near `sqrt(vectors)` is a conventional starting point; the default
  64 suits a few thousand vectors. More centroids can improve separation but
  increase sample build work and assignments per probe.
- `ivf_nprobe` trades recall for candidate work. The default 8 is the fixture
  baseline. Raising it increases the candidate window up to the 2,048 cap.
- `ivf_min_vectors` avoids a degenerate index on small projects. Keep scan for
  projects that will never reach it.
- `ivf_rebuild_vectors` controls centroid freshness. Lower values rebuild more
  often; the threshold is a growth trigger, not a per-tick rebuild.

Interpret counters:

| Field | Meaning |
|---|---|
| `vector_count` | Live active/stale valid vectors for the current model/dimensions |
| `assigned_vectors` | Vectors with an in-range assignment in the current generation |
| `invalid_assignments` | Stale/out-of-range/orphan assignments; maintenance repairs |
| `cluster_count` | Actual centroids stored (may be less than `nlist`) |
| `coverage` | `assigned / vector_count` |
| `bytes` | Centroid blob size |
| `generation` | Monotonic derived index generation |
| `fallback_count` | Recall probes that could not use the index |
| `last_error` | Last sanitized build/maintenance error |

Recovery:

- **Stale/corrupt index**: run `memory.embeddings.index.rebuild`. It does not
  call the embedding endpoint and does not modify vectors or memories.
- **Embedding model/dimensions changed**: re-index embeddings first; maintenance
  rebuilds the index automatically once current vectors exist. An explicit
  rebuild after coverage reaches 1.0 is optional.
- **Suspicious recall**: compare `index_source`/`index_fallback` in the search
  summary, then run `memory-embeddings-index` for the persisted fallback count
  and reason. Set `index = scan` to disable approximate retrieval immediately.
- **Privacy cleanup**: use `memory.embeddings.rebuild`, which clears vectors,
  jobs, centroids, and assignments without network access.
- **Tuning experiment**: change `nlist` or `nprobe`. `nlist` changes invalidate
  the index and rebuild on the next maintenance tick; `nprobe` takes effect
  immediately.
- **Threshold storm**: if rebuilds are too frequent, raise
  `ivf_rebuild_vectors`; if centroid quality lags, lower it or rebuild
  explicitly.

### 13. Migration and compatibility

- Existing databases create the two new tables with `CREATE TABLE IF NOT
  EXISTS`; no data migration is required.
- Existing policies decode with `index = scan`, `ivf_nlist = 64`,
  `ivf_nprobe = 8`, `ivf_min_vectors = 256`, and `ivf_rebuild_vectors = 4096`.
  Behavior is byte-for-byte the previous exact lane.
- Older daemons ignore the new policy fields, but a policy set is a replacement:
  an older client resending a policy without the fields resets to scan defaults.
  In-repo CLI/Swift clients always resend the fields.
- Older status consumers tolerate the additive `index` object; newer clients
  treat its absence as scan/not-configured.
- Replay, bundles, imports, and backups never contain index state. An imported
  session may have memories and vectors; the index builds on the next tick.
- `vp db-rebuild` recreates empty index tables; vectors are already excluded
  from the rebuild source, so a regular rebuild is required.
- Deleting the project cascades the index row and assigned memory rows.

### 14. Threat model

The IVF index does not add a network or trust boundary, but it introduces new
derived state and operator inputs. Defenses:

- policy bounds prevent unbounded `nlist`, `nprobe`, sample, and assignment work;
- centroid blobs are length/range/magic-checked before allocation and decoded
  values are finite/non-zero checked;
- assignment rows are fenced by generation and cluster range;
- recall never trusts assignment rows without joining the current memory,
  embedding, model, dimensions, status, and vector header;
- the candidate window and byte caps bound worst-case decode/scoring;
- status and errors exclude vectors, centroids, hashes, and text;
- build errors are sanitized with the embedding redactor and capped;
- index work happens outside memory write transactions and never calls the
  endpoint;
- the index cannot grant access to a memory an agent could not already read,
  and it cannot bypass status, project scope, approval, or verification gates.

It does not defend against:

- a malicious local user with write access to the SQLite file (they can corrupt
  any derived or source table);
- a compromised daemon process;
- a provider returning structurally valid but semantically wrong vectors;
- side-channel inference from aggregate status counts (counts are already
  exposed by embedding status);
- denial of service by repeatedly causing rebuilds (operator policy and bounded
  sample limit the impact).

### 15. Measured recall versus claims

The test fixture uses eight synthetic clusters of 32 vectors each in 8
dimensions, `nlist = 64`, `nprobe = 8`, and `k = 10`. It computes brute-force
cosine top-k over all fixture vectors and compares the probed top-k. The
acceptance threshold is `recall@10 >= 0.90`; the implementation also asserts that
`nprobe = 8` does not trail `nprobe = 1` on the same fixture.

This is a regression guard for the candidate path, not evidence about any real
embedding model. It does not measure:

- provider embedding quality;
- recall on anisotropic or duplicate-heavy data;
- recall after many incremental writes without a rebuild;
- cross-project data;
- latency distributions.

Those require a project-specific benchmark and are explicitly outside this
ADR's claim.

### 16. Operator examples

Enable and inspect from the CLI:

```bash
vp memory-policy-set <project> --embedding-enable \
  --embedding-endpoint https://host/v1 --embedding-model embed-small \
  --embedding-dimensions 1536 --embedding-index ivf \
  --ivf-nlist 64 --ivf-nprobe 8 --ivf-min-vectors 256 \
  --ivf-rebuild-vectors 4096
vp memory-embeddings <project>
vp memory-embeddings-index <project>
vp memory-embeddings-index-rebuild <project>
```

The equivalent RPCs are:

```json
{"method":"memory.embeddings.index.status","params":{"project_id":"prj"}}
{"method":"memory.embeddings.index.rebuild","params":{"project_id":"prj"}}
```

A rebuild response is intentionally small:

```json
{
  "project_id": "prj",
  "rebuilt": true,
  "reason": null,
  "generation": 4,
  "vector_count": 512,
  "assigned_vectors": 512,
  "cluster_count": 64,
  "nlist": 64
}
```

A search summary distinguishes lane and index health:

```json
{
  "enabled": true,
  "index_source": "scan",
  "index_generation": 4,
  "index_fallback": "index_corrupt: centroid blob has an unknown magic",
  "probed_clusters": 0,
  "candidate_vectors": 0
}
```

None of these responses contains a vector, centroid, content hash, or memory
text.

### 17. Implementation map

- `crates/vp-core/src/memory_index.rs`: centroid encoding/validation,
  deterministic k-means, assignment/probe/status/audit logic, recall fixture.
- `crates/vp-core/src/memory_embedding.rs`: policy bounds and index constants.
- `crates/vp-core/src/lib.rs`: recall integration, status/index RPC engine
  methods, reconciler maintenance, delete/prune maintenance.
- `crates/vp-store/src/store.rs`: tables and transactional index/assignment
  methods, audit queries, cleanup.
- `crates/vp-protocol/src/types.rs`: additive policy/status/RPC/summary types.
- `crates/vp-daemon/src/main.rs`: `memory.embeddings.index.status` and
  `memory.embeddings.index.rebuild` dispatch.
- `crates/vp-cli/src/main.rs`: index commands and policy flags.
- `apps/macos/VerlinPark/Core/Models.swift`,
  `CoreClient.swift`, `Features/AppModel.swift`,
  `Features/DashboardPolicyCards.swift`: decode, status display, rebuild action,
  and action gating.
- `crates/vp-core/tests/memory_ivf.rs`, `crates/vp-store/src/store/tests.rs`,
  `crates/vp-protocol/src/types.rs` tests, `apps/macos/VerlinParkTests`: the
  acceptance matrix.

## Consequences

Positive:

- Large projects can opt into bounded approximate candidate generation while
  keeping exact cosine scoring for probed vectors.
- Exact scan remains the default, the fallback, and the comparison lane.
- Deterministic builds make status, tests, and operator rebuilds reproducible.
- Generation fencing and transactional writes prevent mixed-generation recall
  and lost vectors.
- No new provider calls, secrets, network surface, or vector exposure.
- Corrupt/stale/too-small indexes degrade to lexical/exact recall with an
  explicit reason instead of failing search.

Negative:

- Two more derived tables and a maintenance path to operate.
- Recall is approximate when enabled; quality depends on `nlist`/`nprobe` and
  data distribution.
- Rebuilds read a bounded sample and can take measurable CPU on large projects.
- Assignment coverage can lag behind a burst of writes until the next tick.
- The index is local to one daemon and one SQLite database.
- Operators must reason about two rebuilds: embedding rebuild (clears vectors)
  versus index rebuild (recomputes centroids from vectors).

## Tests

- Centroid encoding: round-trip, magic/version/dimension/count/length/zero-norm/
  non-finite rejection, corrupt-header handling, status header-only validation.
- k-means: repeated-build equality, shuffled-input equality, duplicate-vector
  determinism, stable tie-break, duplicate id/dimension/zero/finite rejection,
  seed stability.
- Recall fixture: synthetic 8-cluster vectors, `nlist=64`, `nprobe=8`, recall@10
  measured against brute-force exact top-k with a documented `>= 0.90`
  threshold; no exactness claim.
- Store: generation monotonicity, assignment fencing, deterministic bounded
  sample, byte cap, unassigned tracking after writes/generation changes,
  cluster/generation/lifecycle-scoped candidates, error preservation, fallback
  counter, delete/prune/archive/clear cleanup, project isolation, empty-blob
  rejection.
- End-to-end: 300-vector build and IVF recall; incremental assignment without a
  rebuild; below-minimum exact fallback; scan-mode clearing; corrupt-index
  fallback and repair; model-change invalidation and rebuild; delete/prune
  coverage updates.
- Protocol/CLI/Swift: additive defaults, status decode, RPC result round-trip,
  policy bounds, CLI parse, Swift action gating, and legacy status without an
  index object.
- Existing lexical and hybrid memory tests remain green.

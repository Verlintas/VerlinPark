# Context, Ownership, Memory, and Bench Plan — long-task round 2

> **Status:** implemented (2026-11-11); this file remains the design record.
> **Created:** 2026-11-11
> **Goal:** make long-running sessions cheaper and safer under context pressure,
> make ownership durable across processes, add an explicit optional semantic
> memory lane, and measure cost per solved task instead of only tokens.
> **Budget:** one large round, target +30,000 net lines of production code and
> tests. No filler; every module must operate, fail closed, and be auditable.
>
> **Result:** C1–C4 landed as four commits on `main` (`b0adba1`, `26256ed`,
> `93a991a`, `bef841a`) plus this plan commit (`beaefae`). `git diff --stat
> a5f57c8..HEAD` shows **38,436 insertions / 2,153 deletions** before the
> documentation close-out. Rust is **1,331 passing tests with 6 ignored**;
> Swift is **195 app tests plus 116 computer-helper tests**. `just lint`,
> `xcodebuild test`, and `just bundle` pass. The implementation and remaining
> limits are recorded in ADR-0159–0163 and `PROGRESS.md`.

---

## 0. Why this round

Round 1 added crash-resume, remote workspaces, programs, and native GUI control.
The next structural limits are:

1. A single tool result can dominate the context window; truncation is currently
   lossy and not page-readable.
2. Compaction has a message-count window but no token budget, no structured task
   state, and always pays summarization latency at the threshold.
3. Runtime leases and continuation delivery are durable but single-daemon
   ownership is still the default assumption; stale writers are not fenced.
4. Project memory is lexical only, with no explicit optional semantic lane.
5. There is no repeatable cost-per-solved-task harness, so efficiency changes
   are judged by anecdotes.

This round addresses all five with independent modules. Every module preserves
the invariants in `AGENTS.md`: deterministic Rust owns scheduling, permissions,
and merge; session state is event-backed before projections; no hidden model
routing; no safety gate is weakened.

---

## C1 — Context economy: tool-output paging and compaction budget

### Problem

`ToolOutput` is truncated in memory at 40k characters with no durable copy and
no cursor. The full audit event is capped at 32 KiB structured / 40k text. A
large build log or grep result is either lost or pollutes the context for the
rest of the run. Compaction keeps a fixed number of messages, not a token
budget, and re-summarizes synchronously at the threshold.

### Design

**Tool output store**

- New project-scoped `ToolOutputPolicy` with safe defaults:
  `enabled`, `spill_threshold_bytes` (default 16 KiB), `preview_bytes`
  (default 4 KiB), `max_total_bytes_per_turn` (default 64 KiB),
  `retention_secs` (default 7 days), `max_entries` (default 2,000),
  per-tool overrides for `read_file`, `read_files`, `grep`, `shell`,
  `run_project_task`, `git`, `remote_exec`, `code_diagnostics`, `browser`.
- Store full output in a content-addressed directory under the data directory
  (`tool-outputs/<sha256>.txt`), with SQLite metadata:
  `tool_outputs(id, session_id, tool_call_id, tool_name, sha256, bytes, lines,
  created_at, expires_at, preview)`.
- Never store secrets differently from the existing event policy: tool output
  is already agent-visible; the store is daemon-local, 0600 files, canonical
  path-owned, and never exposed as a workspace file. Do not add a path input.
- `ToolOutput` gains optional `output_ref`, `truncated`, `total_bytes`,
  `preview_bytes`, `lines`; provider-facing `content` stays bounded and ends
  with an explicit marker:
  `[output truncated: showing N of M bytes/lines; use read_tool_output handle=...]`.
- New read-only agent tool `read_tool_output`:
  `{ handle, offset?, limit?, max_bytes?, from_end?, pattern? }`;
  offset is zero-based lines; pattern is a literal substring; hard caps on
  returned bytes/lines; handles are validated as daemon-owned records (never
  user paths).
- Per-turn aggregate shaping: when several tool results in one assistant turn
  exceed the turn budget, spill the largest first and keep bounded previews.
  Truncation is deterministic and never splits UTF-8.
- Events: `tool.output_spilled`, `tool.output_truncated` with sizes/hashes and
  no content; `ToolOutput` structured metadata carries `output_ref`.
- GC: daemon sweep + manual `tool_output.prune` / CLI, project policy,
  oldest-first, never delete a row referenced by an active run unless expired.

**Compaction**

- Add `compact_keep_tokens` (default 15,000) alongside
  `compact_keep_messages`.
- Summary payload includes a structured state block derived from the stored
  plan: goal, active task id/title/acceptance, department, write scope, plan
  revision, and recent completed task ids. The model summary is free text; the
  structured block is deterministic and authoritative for recovery.
- Optional background summary preparation: at a configurable pre-threshold
  (default 60% of the compaction threshold), prepare a summary in memory keyed
  by provider/model, stable prefix hash, plan revision, and message cursor.
  Reuse it only if the key still matches; discard on model/instruction/plan
  change, cancellation, or failure. The swap is still synchronous and bounded;
  background work never starts a second run or bypasses the budget.
- Metrics: compaction count, latency, tokens before/after, background reuse,
  fallback-to-sync. `session.context` exposes the latest values.
- Compaction remains lossy and is never durable task state.

### Tests

- Spill/truncate at exact byte/line boundaries, UTF-8, empty output.
- Provider-facing content has a continuation hint and bounded size.
- `read_tool_output` offset/limit/from_end/literal pattern; invalid/foreign
  handle; max bytes; no path escape.
- Per-turn aggregate spilling; largest-first deterministic order.
- Retention/GC, active-run protection, bundle import (outputs are operational
  state and are not replayable bundle content), `db-rebuild` behavior.
- Compaction keep-token math, structured plan state, background key reuse and
  invalidation, model switch, plan revision change, failure fallback, metrics.
- Existing compaction, tool, replay, and budget tests stay green.

### Files

New `vp-core/src/tool_output.rs`; `tools.rs`, `agent.rs` tool execution and
compaction code; `store.rs` tables/methods; protocol types; daemon RPC/sweep;
CLI `tool-output-*`; Swift Efficiency/Context surface; ADR-0159 and ADR-0160.

---

## C2 — Cross-process ownership and continuation fencing

### Problem

`runtime_leases` has an owner id and expiry, but no per-acquisition fence.
`session_continuations` can be claimed by a daemon and delivered; a stale daemon
that wakes after expiry can still write a late result. SQLite is a single local
writer, but two daemon processes can share one data directory.

### Design

- Add a persisted daemon instance identity (`store_meta`) with a random UUID,
  pid, start time; use it in lease owners.
- Extend `runtime_leases` with `owner_instance`, `lease_token`,
  `fencing_token` (monotonic per lease key), `renewed_at`, `released_at`.
  Acquire/renew/release/deliver are conditional on the token. A stale owner gets
  a typed `LeaseLost` error and must stop its critical section.
- Add `session_continuations.lease_owner`, `lease_token`, `lease_expires_at`,
  `delivery_attempt`. Claim is an atomic compare-and-set; `mark_delivered`,
  `fail`, and `retry` require the token. Expired claims become claimable; the
  old owner cannot acknowledge.
- Merge/integration ownership: use a runtime lease keyed to the session merge
  queue; the Integrator validates the token before committing. A stale
  integrator must abort without changing the session branch.
- Startup recovery: leave unexpired foreign claims alone; reclaim expired ones;
  never reset a partially merged worktree.
- Token values are never logged or exposed through RPC; list only owner,
  generation, expiry, and liveness.

### Tests

- Two Store/Engine instances over one data dir: exactly one acquires; stale
  renew/release/acknowledge fails; expiry reacquisition increments the token.
- Two continuation reconcilers cannot deliver the same child; stale delivery
  cannot write a terminal row.
- Merge/integration stale owner aborts before commit.
- Crash/restart recovery, expired foreign claims, no duplicate side effects.
- Existing single-daemon lease and continuation tests stay green.

### Files

`store.rs` schema/methods; `lib.rs` lease guard; `agent.rs` continuation
delivery and integration guard; daemon RPC/CLI visibility; tests; ADR-0161.

---

## C3 — Optional hybrid project memory

### Problem

Project memory is deterministic lexical retrieval only. Long projects benefit
from semantic recall for paraphrases, but any embedding lane must be explicit,
user-configured, fall back safely, and never silently route models.

### Design

- Project-scoped `MemoryEmbeddingPolicy`: `enabled` (default false),
  `endpoint` (OpenAI-compatible `/embeddings`), `model`, `dimensions`
  (default 1536, validated), `batch_size`, `timeout_secs`, `max_candidates`,
  `min_similarity`, `api_key_env` (name only; never store the key).
- Derived tables: `memory_embeddings(memory_id, model, dimensions,
  content_hash, vector BLOB, created_at, updated_at)` and
  `memory_embedding_jobs(memory_id, model, status, attempts, last_error,
  next_attempt_at)`.
- Writes never fail because embedding is unavailable: memory create/update
  commits normally and enqueues an index job. A daemon reconciler indexes with
  bounded batches/retries/backoff. Stale content hashes are re-indexed.
- `recall` runs lexical FTS and, when enabled and healthy, vector similarity
  over a bounded candidate set; combine with deterministic reciprocal-rank
  fusion plus existing task/scope/recency/status weights. Semantic failure
  falls back to lexical and reports `semantic_unavailable`.
- Tool/RPC: `recall` output gains optional scores/source; `memory.embeddings
  .status/rebuild/test`; CLI and Manage Dash health show coverage/model/last
  error, not vectors or secrets.
- Embedding usage is recorded separately (model, tokens, calls) and never
  charged to an unrelated session budget without an explicit policy; document
  the accounting rule. No automatic embedding calls when disabled.
- Events: `memory.embedding.indexed/failed/skipped`; derived embeddings are
  rebuildable and not replay projections.

### Tests

- Fake OpenAI-compatible embedding server: dimension mismatch, timeout,
  retry/backoff, partial batch, stale hash, model change invalidates coverage.
- Ranking determinism, task/scope boost, lexical fallback, disabled mode makes
  zero network calls.
- Memory write succeeds when embedding fails; rebuild/status; GC with memory
  delete; no secrets in logs/status.
- Existing lexical memory tests stay green.

### Files

`vp-core/src/memory.rs`; store tables/methods; protocol; daemon RPC/reconciler;
CLI; Swift memory health; tests; ADR-0162.

---

## C4 — Cost-per-solved-task benchmark harness

### Problem

Efficiency claims currently rest on token/cost totals. A harness must measure
solved tasks, verifier passes, merge success, and human interventions under
repeatable scenarios.

### Design

- New `crates/vp-bench` library + `vp-bench` binary. It creates disposable
  fixture repositories, runs scripted/deterministic scenarios, and never uses
  real provider credentials by default.
- Scenario definitions: baseline single session, plan Worker, read-only Scout,
  tool-output paging on/off, compaction on/off, worktree parallelism 1 vs N.
  Each fixture has deterministic acceptance (tests or patch checks).
- Metrics per run: solved, acceptance passed, verifier verdict, merge success,
  wall-clock, input/output/cache tokens, cost (explicit price input),
  tool-output bytes/spills, compactions, retries, human interventions.
- Reports: JSON (machine), Markdown (human), CSV (rows); include git commit,
  host metadata, model/pricing metadata, and a caveat that one host is not a
  universal ranking.
- `vp bench run/list/report` (optional CLI; keep the harness usable as a test).
- Run harness in CI with deterministic fake provider; real-provider mode is
  env-gated and ignored by default.
- No fake success: a scenario with failed acceptance or merge is `unsolved`,
  even if tokens were spent.

### Tests

- Scenario validation, fixture setup/cleanup, deterministic replay.
- Cost math with cache reads/writes; solved/unsolved classification.
- Report JSON/Markdown/CSV shape and rounding.
- A/B comparison refuses mismatched fixture/acceptance.
- Existing workspace tests stay green.

### Files

New `crates/vp-bench`; workspace `Cargo.toml`; CLI optional; fixtures/tests;
ADR-0163.

---

## 4.1 Implementation record (2026-11-11)

| Workstream | Landed code | Operator surface | Limits that remain |
|---|---|---|---|
| C1 context | `vp-core/src/tool_output.rs`, `tool-outputs/<sha256>.txt` blobs + `tool_outputs` metadata, per-turn shaping, `read_tool_output`, keep-token compaction, structured plan state, prepared summaries | `tool_output.policy.get/set`, `tool_output.status/list/prune/audit`, CLI `vp tool-output-*`; `read_tool_output`; `session.context` compaction metrics | Output bodies are operational, not replay/bundle state; no encryption or compression; prepared summaries are per-run memory only; compaction stays lossy |
| C2 ownership | Daemon instance identity, fenced `runtime_leases` + `runtime_lease_fences`, continuation claim/ack fields, integration effect-boundary assertion | `runtime.leases`, `runtime.leases.history`, `session.continuation.status/reclaim`, `vp leases` / `lease-history` / `continuation-*` | Same-host SQLite coordination only; TTL is the liveness bound; external effects are not exactly-once; write leases are not process-scoped yet |
| C3 memory | `memory_embedding.rs`, `memory_embeddings` / `memory_embedding_jobs` / `memory_embedding_usage`, hybrid recall + RRF, fake-server tests | `memory.embeddings.status/rebuild/test`, policy fields, CLI flags, Manage Dash health | SQLite vector scan, no ANN/distributed index; bodies go to the configured endpoint; embedding cost is separate from session budgets; provider token counts may be estimated |
| C4 bench | New `vp-bench` crate, deterministic fixtures/scenarios, scripted provider, metrics/reports/compare | `vp bench list/run/report/compare`, standalone `vp-bench` | Fake usage is simulated from request size; paging metrics depend on C1 (now present); single-host/sequential; no universal ranking |

C1–C4 preserve the existing approval, Seatbelt, path-jail, write-scope,
Verifier, merge, capacity, and budget gates. Derived tool outputs, embeddings,
and benchmark reports are explicitly operational state, not session replay
projections.

## 5. Parallelization and integration order

| Workstream | Primary files | Shared files |
|---|---|---|
| C1 context | new `tool_output.rs`, `tools.rs`, `agent.rs` (tool/compaction), protocol, store | daemon, CLI, Swift |
| C2 ownership | store, `lib.rs`, continuation/integration in agent.rs | protocol, daemon, CLI |
| C3 memory | `memory.rs`, store, protocol, daemon, CLI, Swift | — |
| C4 bench | new `vp-bench` crate, workspace manifest, scripts | CLI optional |

Integration order: **C4 → C3 → C2 → C1**. C1 merges last because it owns the
largest `agent.rs` regions. Resolve additive protocol/daemon/schema conflicts in
that order; never rewrite another module's tables or events.

## 6. Acceptance

- +30,000 net lines of production code/tests/docs, measured by `git diff --stat`
  against `a5f57c8`.
- No filler modules, no dead code, no untested happy-path-only code.
- `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace`,
  `just lint`, `just test`, `just bundle` pass.
- Every new state change is event-backed where it is session state; derived
  indexes/output stores are explicitly operational and rebuildable/GC-able.
- Approvals, Seatbelt, path jail, write scope, Verifier, merge, capacity, and
  budget gates are unchanged or stricter.
- Shared docs and ADR index updated; this file marked complete.

## 7. Non-goals

- No cloud vector database, remote memory service, or mandatory embeddings.
- No hidden model routing; embeddings are explicit per-project configuration.
- No lossless compaction or compaction-as-durable-state.
- No claim that one benchmark run ranks all models/harnesses.
- No cross-host consensus; leases coordinate processes that share one SQLite
  data directory, not arbitrary network partitions.
- No new agent write path; tool-output handles are daemon-owned read-only
  references, not workspace paths.

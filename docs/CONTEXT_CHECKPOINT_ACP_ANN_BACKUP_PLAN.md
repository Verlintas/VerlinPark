# Context Checkpoints, ACP Remote Transport, ANN Memory, and Encrypted Backups

> **Status:** implemented (2026-11-11); this file remains the design record.
> **Created:** 2026-11-11
> **Goal:** make compaction reversible at the active-context boundary, add
> standard remote ACP transports, make hybrid memory scale beyond a SQLite scan,
> and add encrypted incremental backups.
> **Budget:** one large round, target +30,000 net lines of production code and
> tests. No filler; every module must operate, fail closed, and be auditable.
>
> **Result:** D1–D4 landed as four commits on `main` (`081fecd`, `1bbc78f`,
> `7f8ff32`, `a7d54fd`) plus this plan commit (`b972436`). `git diff --stat
> b972436..HEAD` shows **35,046 insertions / 1,661 deletions** before the
> documentation close-out. Rust is **1,654 passing tests with 6 ignored**;
> Swift is **215 app tests plus 116 computer-helper tests**. `just lint`,
> `xcodebuild test`, and `just bundle` pass. The implementation and remaining
> limits are recorded in ADR-0164–0167 and `PROGRESS.md`.

---

## 0. Why this round

Round 2 added paging, compaction budgets, same-host fencing, hybrid memory, and
a benchmark harness. The remaining high-value gaps are:

1. Compaction is still one-way at the active-context boundary: original messages
   remain in SQLite, but there is no checkpoint/restore surface for the exact
   active summary + cursor.
2. ACP is stdio-only; remote agents cannot be reached over the standard
   Streamable HTTP / WebSocket transport.
3. Hybrid memory uses a bounded SQLite vector scan; there is no opt-in ANN index
   for larger projects.
4. Backups are checksummed ZIPs, but not encrypted or incremental.

This round adds four independent modules. Invariants from `AGENTS.md` remain:
deterministic Rust owns scheduling/permissions/merge; session state is
event-backed before projections; no hidden model routing; no gate is weakened.

---

## D1 — Durable context checkpoints and restore

### Problem

`context.compacted` events retain the original `message.created` rows, but the
active context is reconstructed from only the latest compaction event. There is
no durable, inspectable checkpoint of (summary, through-message cursor, plan
revision, model/prefix) that an operator can restore after a bad compaction or a
long pause. Worktree checkpoints exist, but they do not restore conversation
context.

### Design

- New `context_checkpoints` table and `ContextCheckpoint` protocol type:
  `id, session_id, project_id, label, mode, summary, through_message_id,
  message_cursor, plan_revision, provider, model, prompt_prefix_hash,
  worktree_head, tokens_before, tokens_after, structured_state, created_at`.
- Auto-create after every successful compaction; manual
  `session.context.checkpoint` captures the current active boundary. Bounded
  summary/state sizes; no prompt text beyond the summary already present in
  `context.compacted`.
- Restore is context-only and does not start a run:
  - append a new `context.compacted` event reconstructed from the checkpoint
    (summary, through_message_id, mode, structured state), then
    `context.restored`;
  - `load_conversation`/`conversation_from_events` therefore rewind to the
    checkpoint boundary; original messages remain in SQLite for search/audit;
  - preconditions: session idle/failed/cancelled, not imported, plan revision
    unchanged, provider/model unchanged, worktree head equal or clean
    descendant, budget not exhausted, restored active-context estimate within
    the hard limit;
  - no worktree reset and no new agent run.
- RPC: `session.context.checkpoints`, `session.context.checkpoint.detail`,
  `session.context.checkpoint`, `session.context.restore`,
  `session.context.checkpoint.delete`, `session.context.checkpoint.prune`.
  Events: `context.checkpointed`, `context.restored`,
  `context.checkpoint.deleted`.
- Checkpoint rows are event-backed projections: replay/repair can rebuild them
  from their full-payload events; bundle export/import carries them. Retention
  policy (project-scoped) can prune automatically; never delete the latest
  checkpoint while the session is active.
- Stage/Manage Dash shows recent context checkpoints, token deltas, summary
  preview, and a Restore action gated by preflight; no silent restore.
- Tests: auto checkpoint after compaction; manual checkpoint; restore rewinds
  active context; originals/search unchanged; plan/model/provider/worktree/
  budget mismatch refuses; hard-limit refusal; delete/prune; replay/repair/
  bundle/db-rebuild; CLI/Swift decode/action gates.

### Files

`vp-core/src/context.rs` (new) or agent context module; `agent.rs` compaction
and conversation loaders; store schema/methods; protocol; daemon RPC; CLI;
Swift context/Stage/Manage Dash; ADR-0164.

---

## D2 — ACP Streamable HTTP and WebSocket transport

### Problem

ACP agents are launched only as local stdio child processes. Remote or
containerized ACP agents require a standard HTTP transport. The ACP RFD defines
Streamable HTTP (POST + long-lived SSE GET streams) and WebSocket on one
endpoint, with `Acp-Connection-Id` and `Acp-Session-Id` identity.

### Design

- Extend `AcpAgent` additively:
  `transport: stdio | streamable_http | websocket` (default stdio), `endpoint`,
  `auth_header_env`, `headers`, `connect_timeout_secs`, `allow_remote`
  (explicit opt-in). stdio continues to require `command`; remote requires an
  `http(s)`/`ws(s)` endpoint and no command.
- Implement an ACP transport adapter over `agent_client_protocol`'s
  `ConnectTo<Agent>`/`Channel` abstraction:
  - HTTP: POST JSON-RPC messages; `initialize` returns 200 + connection id;
    other requests return 202 and responses arrive on the connection/session
    SSE streams; open one connection-scoped stream and one per session; route
    permission requests/responses and notifications; DELETE on close.
  - WebSocket: one text-frame channel on the same endpoint; initialize first.
  - Cookie jar and explicit headers; auth header value read from an environment
    variable name, never stored/logged.
  - Bounded frames/event sizes, connection/session timeouts, cancellation,
    reconnect policy (`session/load` when advertised), no in-flight replay
    (document v1 limits).
- Remote ACP agents are trusted external services, not sandboxed. They may run
  tools outside the local worktree; `delegate_acp` must require explicit
  `allow_remote` and approval policy must classify remote delegation as
  external/host-state where appropriate. No claim of local Seatbelt coverage.
- RPC/Swift/CLI settings expose transport/endpoint/auth header name, probe and
  validation; stdio behavior unchanged. Secrets stay in env/Keychain references,
  not SQLite.
- Tests: in-process axum/wiremock ACP HTTP server (initialize, session/new,
  session/prompt, notifications, permission request/response, 202+SSE routing,
  session-scoped vs connection-scoped, DELETE, timeouts, malformed frames,
  auth failure); WebSocket handshake/messages; cookie/header forwarding; no
  process spawn for remote; stdio regression; Swift decode/probe.
- ADR-0165.

### Files

`vp-core/src/acp.rs` + new transport module; protocol; daemon RPC; CLI; Swift
ACP settings; tests; ADR.

---

## D3 — Opt-in IVF ANN index for hybrid memory

### Problem

Hybrid memory currently scores a bounded set of lexical candidates and exact
cosine scans. Larger projects need an opt-in approximate index with bounded
rebuild cost and a safe exact-scan fallback.

### Design

- Extend `MemoryEmbeddingPolicy` additively:
  `index: scan | ivf` (default scan), `ivf_nlist` (default 64, bounded 4–1024),
  `ivf_nprobe` (default 8, bounded 1–nlist), `ivf_min_vectors` (default 256),
  `ivf_rebuild_vectors` (default 4096).
- Derived tables `memory_embedding_index` (project/model/generation, dimensions,
  centroids BLOB, vector_count, updated_at) and
  `memory_embedding_assignments` (memory_id, generation, cluster).
- Deterministic k-means (seeded, bounded iterations, stable tie-break) builds
  centroids from a bounded sample; new/stale vectors are assigned to the nearest
  centroid transactionally. Rebuild when generation/model/dimensions change or
  vector count crosses the rebuild threshold. Corrupt/stale index falls back to
  exact scan with an explicit reason.
- `recall` probes `nprobe` nearest centroids and scores those vectors exactly;
  lexical candidates remain in the RRF fusion. Bound candidates/vectors/bytes;
  never return vectors. Status exposes index mode, generation, vector count,
  cluster count, coverage, last rebuild/error.
- RPC `memory.embeddings.index.status/rebuild`; CLI/Swift health controls.
  Embedding disabled means zero index work.
- Tests: deterministic synthetic recall vs exact top-k (recall@k threshold),
  k-means determinism, incremental assignment, model/dimension change invalidation,
  stale/corrupt index fallback, min-vector scan fallback, bounded nprobe/nlist,
  delete/prune cleanup, status/rebuild, existing hybrid tests green.
- ADR-0166.

### Files

`vp-core/src/memory_embedding.rs`; memory/store; protocol; daemon RPC; CLI;
Swift memory health; tests; ADR.

---

## D4 — Encrypted and incremental backups

### Problem

Backups are checksummed ZIPs. They are not encrypted, and every backup rewrites
all data. Long-lived projects need authenticated encryption and incremental
snapshots while preserving the existing v1 restore path.

### Design

- Keep v1 ZIP read support. Add v2 **backup repository** mode:
  `vp backup --repository <dir>` writes a snapshot manifest plus
  content-addressed blobs; `--incremental` reuses blobs present in a parent
  snapshot; `--passphrase-env NAME` (or prompt) enables encryption. Never accept
  a passphrase in argv/logs.
- Crypto: Argon2id KDF (salt + parameters persisted), ChaCha20-Poly1305 AEAD per
  blob with random nonce, authenticated manifest metadata, SHA-256 of plaintext
  for dedup/verification. Wrong passphrase, tampered manifest/blob, or truncated
  chain fails closed. Use `zeroize` for key material.
- Repository layout: `manifest.json`/snapshots, encrypted blobs keyed by hash,
  parent chain, created/pruned timestamps, tool/app version, encrypted flag,
  KDF params, nonce table. Restore validates the chain and every blob before
  atomically replacing the data dir; restore requires the daemon stopped.
- `vp backup-prune --repository --keep N` removes unreferenced snapshots/blobs
  only after validating references; dry-run.
- Existing `vp backup --output zip` and `vp restore --input zip` remain
  compatible. Encrypted ZIP is not attempted; encrypted mode uses the repository
  format.
- Tests: v1 round-trip unchanged; encrypted repo round-trip; wrong passphrase;
  tampered blob/manifest; truncated/missing parent; incremental dedup byte
  counts; prune/dry-run; restore atomicity; no secret in manifest/logs; KDF
  parameter validation; CLI.
- ADR-0167.

### Files

`vp-core/src/backup.rs` and new crypto/repository module; workspace deps
(`argon2`, `chacha20poly1305`, `zeroize`); CLI; docs; tests; ADR.

---

## 4.1 Implementation record (2026-11-11)

| Workstream | Landed code | Operator surface | Limits that remain |
|---|---|---|---|
| D1 context checkpoints | `vp-core/src/context.rs`, context checkpoint table/events, auto/manual checkpoints, restore-by-rewriting the compaction boundary, retention sweep | `session.context.checkpoint*`, `session.context.restore`, `session.context.policy*`, CLI `context-checkpoint*`, Stage/Manage Dash card | Context-only restore, not run resume; summaries remain advisory/lossy; no cross-host consensus |
| D2 ACP remote | ACP transport config, Streamable HTTP (POST + connection/session SSE), WebSocket, cookie/auth headers, bounded frames, `session/load` resume, remote approval guard | `acp.agent.add/update/probe` transport fields, CLI/Swift ACP settings | Remote agents are trusted external services, not sandboxed; no automatic reconnect/in-flight replay; test server is HTTP/1.1 |
| D3 ANN memory | `memory_index.rs`, `memory_embedding_index`/assignments, deterministic IVF k-means, generation/incremental assignment, status/rebuild | `memory.embeddings.index.status/rebuild`, CLI flags/commands, Manage Dash memory health | Scan remains default/fallback; no HNSW/GPU/quantization/distributed index; recall is fixture-measured, not universal |
| D4 encrypted backup | `backup/crypto.rs`, `backup/repository/*`, Argon2id + ChaCha20-Poly1305, content-addressed blobs, parent chain, prune, atomic restore | `vp backup --repository`, `backup-list`, `backup-prune`, encrypted restore flags | No key escrow/rotation/anti-rollback, no distributed lock, no live-daemon point-in-time snapshot; v1 ZIP remains unencrypted |

All four modules preserve existing gates. Derived indexes, tool outputs,
backup blobs, and context-checkpoint summaries are explicitly operational or
event-backed as documented; none silently weakens approval, sandbox, path,
write-scope, Verifier, merge, capacity, or budget enforcement.

## 5. Parallelization and integration order

| Workstream | Primary files | Shared files |
|---|---|---|
| D1 context checkpoints | new context module, `agent.rs`, protocol, store | daemon, CLI, Swift |
| D2 ACP remote | `acp.rs` + new transport, protocol | daemon, CLI, Swift |
| D3 ANN | `memory_embedding.rs`, store, protocol | daemon, CLI, Swift |
| D4 backup | `backup.rs` + new crypto/repository, workspace deps | CLI |

Integration order: **D4 → D3 → D2 → D1**. D1 merges last because it owns
`agent.rs` context/compaction paths. Resolve additive conflicts in that order;
never rewrite another module's tables/events.

## 6. Acceptance

- +30,000 net lines of production code/tests/docs, measured from `6f85ab7`.
- No filler, dead code, or untested happy-path-only code.
- `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace`,
  `just lint`, `just test`, `just bundle` pass.
- Every session state change is event-backed before projection; derived
  indexes/backup blobs are explicitly operational/rebuildable.
- Approvals, Seatbelt, path jail, write scope, Verifier, merge, capacity, and
  budget gates unchanged or stricter.
- Shared docs/ADR index updated; this file marked complete.

## 7. Non-goals

- No cross-host lease consensus or distributed scheduler.
- No lossless compaction or model-generated durable truth.
- No mandatory embeddings/ANN; scan remains the default.
- No claim that remote ACP agents are sandboxed or credential-isolated.
- No encrypted ZIP v1; encryption uses the repository format.
- No cloud backup service, key escrow, or automatic key rotation.
- No provider-call exactly-once or mid-stream resume.

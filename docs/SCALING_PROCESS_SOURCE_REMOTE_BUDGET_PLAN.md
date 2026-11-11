# Scaling, Process Supervision, Semantic Sources, Remote Integration, and Budget Governance Plan

> **Status:** active implementation plan.
> **Created:** 2026-11-11
> **Base:** `c6540c3` (quality round Q1–Q4 complete).
> **Goal:** close the next five runtime gaps: deterministic org scaling,
> durable long-running process supervision, semantic project-source retrieval,
> remote worktree integration, and critical-path-aware budget governance.
> **Budget:** one large round, target +30,000 net lines of production code and
> tests. No filler; every module must operate, fail closed, and be auditable.

---

## 0. Issue checklist

### Scaling

- [ ] Team size is configured per session/project, but there is no deterministic
      controller that expands Workers/Labour from observed ready-queue pressure.
- [ ] Worker auto-expansion is opt-in and only raises the Worker count; there is
      no Labour expansion and no scale-down.
- [ ] No expansion cooldown, max-step, minimum-idle, or scale-down safety rules;
      no event-backed decision trail or preview.
- [ ] Manager expansion would require a plan revision; treating it as automatic
      would silently rewrite department ownership.

### Process supervision

- [ ] `process` is a per-run child; it does not survive daemon restart, has no
      durable handle, and its output is not continuously captured.
- [ ] Long builds/servers cannot be inspected after a provider turn or crash.
- [ ] There is no PID-identity verification, so reattaching after restart would
      be unsafe by construction.
- [ ] No bounded output rotation/retention for supervised processes.

### Semantic sources

- [ ] Project sources are lexical FTS5 + LIKE only; `search_sources` cannot
      answer paraphrase queries.
- [ ] Hybrid memory has embeddings/IVF, but sources do not reuse that path.
- [ ] No source-embedding jobs, generation invalidation, retry, health, or
      separate cost accounting.
- [ ] No explicit lexical/semantic/hybrid result labels or fallback state.

### Remote integration

- [ ] Remote worktrees can be created and inspected, but their commits cannot
      enter the local Verifier → merge queue path.
- [ ] There is no bounded, integrity-checked way to import a remote commit.
- [ ] Remote branches cannot be verified/merged without trusting an
      out-of-band push; conflict/retry is undefined.

### Budget governance

- [ ] Budgets are per session/task/department with reservations, but allocation
      is unaware of the plan's critical path.
- [ ] Non-critical tasks can consume budget that the blocking path needs.
- [ ] No deterministic throttle/defer policy, decision events, or preview.
- [ ] No starvation or revision-change guarantees.

### Deferred backlog (unchanged)

- [ ] F6 approval/attention optimizer.
- [ ] Distributed GUI lease/queue, cross-host scheduler/fencing.
- [ ] Test-result cache/impact map.
- [ ] Bitbucket/other forge adapters.

This round implements F1–F5 below. Deferred items are not claimed as shipped.

---

## F1 — Deterministic org scaling

### Design

- Add a persisted project `ScalingPolicy`:
  - `enabled` (default false),
  - `min_workers` / `max_workers`, `min_labour` / `max_labour`,
  - `expand_ready_threshold` (ready tasks per idle Worker),
  - `expand_critical_delay_secs`,
  - `scale_down_idle_secs`,
  - `cooldown_secs`,
  - `max_step`,
  - `reserve_ratio` interaction with F5 (read-only; F5 owns budget math).
- Add a pure `ScalingController` in `vp-core/src/scaling.rs`:
  - input: plan revision, task states, active slots, capacity snapshot, budget
    headroom, policy, last decision time;
  - output: a deterministic `ScalingDecision { action, target_workers,
    target_labour, reasons[], inputs_hash }` or `Hold`;
  - expansion: only when there are ready tasks, no idle slots, cooldown elapsed,
    budget headroom above policy, and global capacity available; step bounded by
    `max_step`;
  - scale-down: only idle Labour first, then Workers; never removes an agent
    with a running task; never below plan demand or policy minimums; requires
    `scale_down_idle_secs`;
  - ties broken by fixed ordering (department id, task priority, task id).
- Manager count remains plan-owned. The controller may emit
  `manager_recommended` evidence but never creates/reassigns Managers; a plan
  revision is still explicit.
- Persist `scaling_decisions` with event-backed `scaling.decision`; replay,
  bundle import, and `db-rebuild` rebuild it.
- RPC `scaling.preview`, `scaling.status`, `scaling.policy.get/set`; CLI
  `vp scaling`, `vp scaling-preview`, `vp scaling-policy*`; Manage Dash policy
  editor and Stage/collab badge.
- Daemon tick calls the controller only when policy is enabled; decisions are
  applied through existing delegation/capacity paths, never by bypassing them.
- Tests: expansion thresholds/cooldown/max-step, global capacity, budget
  headroom, scale-down safety/idle, deterministic ties/hash, plan revision
  change reset, event replay/bundle, disabled policy no-op, CLI/Swift decode.

### Files

`vp-core/src/scaling.rs`, plan/task store projections, protocol, daemon tick,
CLI, Swift Manage Dash/Stage, ADR-0172.

---

## F2 — Durable process supervisor

### Design

- Add a daemon-owned supervisor in `vp-core/src/process_supervisor.rs`:
  - `supervised_processes` row: id, session, worktree, argv, cwd, env allowlist
    hash, state (`starting|running|exited|stopped|lost|unknown`), pid, pgid,
    nonce, started_at, heartbeat_at, exit_code, signal, output path/size,
    owner instance id, retention deadline.
  - Spawn under the existing Seatbelt write sandbox and env allowlist; the child
    is placed in its own process group and detached from the daemon's stdio.
  - Output is streamed to a daemon-owned append-only file with bounded rotation
    (max bytes/lines), not into the event log.
  - Reattach on daemon restart only when PID, process start time, expected
    command hash, **and** a per-spawn nonce read from the process environment
    all match; otherwise mark `lost` and require operator action. Never adopt a
    PID on partial evidence.
  - Explicit stop sends SIGTERM to the process group, waits, then SIGKILL under a
    bounded deadline; idle process groups are swept only per policy.
- Upgrade the agent `process` tool:
  - actions `start`, `status`, `logs`, `wait`, `stop`, `list`;
  - durable `process_id`; `start` returns immediately; `wait` has a bounded
    deadline; `logs` pages by line/byte with a hard cap.
- Event-backed lifecycle (`process.started`, `process.exited`, `process.lost`,
  `process.stopped`), replay/rebuild, daemon sweep, metrics.
- RPC/CLI/Swift: `session.processes`, `vp processes`, Stage/Preview process card
  with state/log tail; no auto-restart of commands (restart is explicit).
- Tests: restart reattach/lost/unknown, PID reuse rejection, nonce mismatch,
  sandbox/profile args, output rotation, stop escalation, idle sweep, replay,
  CLI/Swift decode. Use a small shell script child, not a real long build.

### Files

`vp-core/src/process_supervisor.rs`, `tooling/process.rs`, store, protocol,
daemon startup/sweep, CLI, Swift, ADR-0173.

---

## F3 — Semantic project-source index

### Design

- Reuse the memory embedding client pattern with a separate source policy:
  - `SourceEmbeddingPolicy { enabled, endpoint_env, model, dimensions,
    batch_size, max_chunks_per_source, retention_days }`; key only via env name.
- `source_embeddings` and `source_embedding_jobs` tables:
  - one vector per bounded chunk, generation-scoped by source snapshot;
  - job states `pending|running|done|failed|dead`, bounded retries/backoff;
  - source refresh enqueues a new generation and marks older vectors stale.
- Retrieval:
  - `search_sources` gains `mode: lexical|semantic|hybrid` (default `hybrid`
    when enabled, otherwise `lexical`);
  - hybrid fuses lexical FTS and cosine candidates with deterministic RRF;
  - exact cosine scan is the default; no ANN claim for sources this round;
  - results carry `source: lexical|semantic|hybrid`, per-candidate origin,
    confidence, and `semantic_unavailable` fallback reason;
  - citation keys remain exact `[cN]` and are only emitted for returned chunks.
- Reconciler: daemon tick indexes pending jobs with a bounded batch and records
  usage/cost separately from memory embeddings; embedding failure never fails a
  source write or lexical search.
- RPC `source.embedding.status`, `source.embedding.reindex`,
  `source.embedding.policy.get/set`; CLI `vp source-embedding*`; Swift Sources
  status card.
- Tests: fake OpenAI-compatible embedding server, chunking bounds, RRF math,
  generation invalidation, retry/dead-letter, disabled/unavailable fallback,
  lexical compatibility, citation integrity, protocol/Swift decode.

### Files

`vp-core/src/source_embedding.rs`, `sources.rs`, store, protocol, daemon tick,
CLI, Swift Sources, ADR-0174.

---

## F4 — Remote worktree integration

### Design

- Add an explicit, policy-gated remote integration path:
  - remote helper action `git bundle --stdout` with a canonical-root path jail,
    bounded chunked frames, SHA-256, and a hard byte cap
    (`RemoteIntegrationPolicy.max_bundle_bytes`, default 256 MiB);
  - local import into a temporary bare ref namespace (`refs/verlinpark/remote/…`)
    without touching the session branch; reject malformed bundles, wrong head,
    or SHA mismatch;
  - the imported commit must match the recorded remote branch head; otherwise
    fail closed.
- Verifier:
  - the imported commit is verified in a local detached clone exactly like a
    local Worker commit;
  - record `source: remote`, host, branch, bundle SHA-256, verifier verdict, and
    environment caveat (local verification does not reproduce the remote host);
  - approved commits enter the existing persisted merge queue and deterministic
    Integrator with `--no-ff`.
- Policy: `RemoteIntegrationPolicy { enabled (default false), max_bundle_bytes,
  require_verifier (default true), allowed_hosts, keep_import_refs }`; never
  enabled by default; approval-classified; no automatic cross-host scheduling.
- Merge queue entry gains additive `source`, `remote_host`, `remote_branch`,
  `bundle_sha256`, `imported_ref`; conflict/retry operates on the imported ref;
  cleanup deletes temporary refs unless `keep_import_refs`.
- RPC `remote.integrate`, `remote.integration.status`; CLI `vp remote-integrate`;
  Manage Dash remote queue badge.
- Tests: fake helper frames/bundle round-trip, cap, tamper/malformed bundle,
  head mismatch, verifier gate, conflict abort/retry, policy disabled, cleanup,
  existing local merge tests green.

### Files

`vp-core/src/remote.rs`, merge queue/Integrator path in `agent.rs`/`lib.rs`,
`vp-remote`, store, protocol, daemon, CLI, Swift, ADR-0175.

---

## F5 — Budget and critical-path governor

### Design

- Add a pure `vp-core/src/critical_path.rs`:
  - deterministic longest-remaining-path computation over the plan DAG using
    task weight = declared token budget, else priority * fixed unit;
  - includes dependency edges, active/queued/completed states, and revision
    identity; cycles/unknown tasks fail closed.
- Add `BudgetGovernor`:
  - project `CriticalPathPolicy { enabled (default false), reserve_ratio
    (0.0–1.0, default 0.25), throttle_at_ratio, critical_boost, min_task_budget }`;
  - reservations for non-critical tasks are denied/deferred when granting them
    would breach the reserved critical-path pool; critical tasks are admitted
    first under pressure;
  - never exceeds the session/project/department hard budget; never changes
    model/provider routing; no silent borrowing after exhaustion;
  - decisions are event-backed (`budget.governor.decision`), previewable
    (`governor.preview`), and explainable (`inputs_hash`, path, reserve math).
- Integrate with the existing in-flight reservation registry and task scheduler:
  - reservation requests carry `critical: bool` and `task_id`;
  - denial returns a typed `BUDGET_RESERVED` reason and the task remains queued;
  - on budget release, deferred tasks are reconsidered in deterministic order.
- RPC `session.budget.governor`, `governor.preview`, `critical_path.policy.*`;
  CLI `vp governor`, `vp critical-path`, `vp governor-policy*`; Swift Efficiency
  card + Stage budget popover.
- Tests: DAG path/tie/cycle, reserve math boundaries, throttle/defer, critical
  admission, budget exhaustion, plan revision reset, preview determinism,
  no-starvation ordering, protocol/Swift decode.

### Files

`vp-core/src/critical_path.rs`, `budget.rs`, reservation registry, store,
protocol, daemon, CLI, Swift Efficiency/Stage, ADR-0176.

---

## 5. Integration and acceptance

Integration order: **F3 → F2 → F1 → F5 → F4**. F4 lands last because it touches
the merge queue/Integrator; F5 lands after F1 so both read the same plan state.

- +30,000 net lines of production code/tests/docs from `c6540c3`.
- No filler, no dead code, no untested happy path.
- `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace`,
  `just lint`, `xcodebuild test`, `just bundle`.
- Approval, Seatbelt, path jail, write scope, Verifier, merge, capacity, and
  budget gates unchanged or stricter.
- Scaling never creates Managers or rewrites a plan; it only scales
  Worker/Labour slots within runtime hard caps.
- Process supervision never adopts a PID without full identity evidence; it is
  not a host-reboot supervisor and does not auto-restart commands.
- Source embeddings are opt-in, vectors are never exposed, and lexical search
  remains available when embeddings are disabled/unavailable.
- Remote integration is opt-in, integrity-checked, Verifier-gated, and does not
  claim the remote environment was reproduced locally.
- The budget governor is deterministic, opt-in, and cannot exceed hard budgets
  or alter provider/model routing.

## 6. Non-goals

- No automatic Manager creation/reassignment; no plan rewrite.
- No cross-host scheduler, distributed lease/fencing, or remote daemon.
- No host-reboot process persistence or automatic command restart.
- No ANN index for sources in this round; exact cosine only.
- No remote test execution as a substitute for local Verifier evidence.
- No test-result cache/impact map, approval optimizer, or Bitbucket adapter.

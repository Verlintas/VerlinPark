# VerlinPark Long-Task Efficiency Playbook

> **Status:** durable research-to-implementation plan.
> **Created:** 2026-11-11
> **Source research:** OpenCode + Orca ADE local measurements, official docs,
> and independent harness studies. This file is the archive of the findings so
> the plan survives context compaction.
> **Rule:** every item below must map to VerlinPark invariants: deterministic
> Rust owns scheduling/permissions/merge; the SwiftUI app is a client; no hidden
> model routing; every session-scoped state change is event-backed.

---

## 0. Baseline measured on this machine

OpenCode 2.1.4 + Orca 1.4.238, 2026-07-29 → 2026-11-11, single macOS host,
DeepSeek V4 Flash family. Aggregates only; no prompt/message content read.

| Metric | Value |
|---|---:|
| Sessions | 151 |
| Root sessions | 25 |
| Child / subagent sessions | 126（83.4%） |
| Messages | 49,905 |
| Tool calls | 54,484 |
| Tool error rate | 0.80% |
| Cache-read tokens | 17.35B |
| Non-cached input tokens | 111.3M |
| Output tokens | 17.98M |
| **Cache-read share** | **99.36%** |
| Estimated input cost without caching | ~$2,619 |
| Actual input cost (list-price reconstruction) | ~$68.75 |
| Root cost share | ~92.5% |
| Child cost share | ~7.5% |
| Root avg messages | 1,769（p50 992） |
| Child avg messages | 44（p50 24） |
| Root avg tool calls | 1,847 |
| Child avg tool calls | 63 |
| Roots that delegated | 8 / 25 |
| Max children under one root | 42 |
| Compactions | 58, in 12 roots; 0 children |
| Multi-tool messages | 12.7%（max 14） |
| Orca agent starts | 1,223 |
| Orca worktrees | 24 |
| Orca orchestration DB | 0 tasks / 0 dispatches / 0 messages |

### Interpretation

1. **Cache reuse is the dominant cost lever.** Median call ≈ 326k cached tokens
   + 239 uncached input + 186 output.
2. **A few very long roots dominate.** 25 roots carry ~92% of cost; 126 children
   are a different workload class, not miniature roots.
3. **Subagents are a context firewall.** Children are read-heavy, short, and
   cheap; the root remains the long-lived implementer.
4. **Parallelism is session/worktree-level, not tool-batch-level.** 87.3% of
   tool-bearing assistant messages carry one tool call.
5. **Compaction is a root-only operating cost.** It enables multi-week roots
   but is lossy and occasionally overflows.
6. **Human attention dominates wall-clock.** `question` waits far exceed edit
   time; approval/attention UX matters more than micro-optimizing tools.
7. **Orca orchestration is not the local driver.** The orchestration DB and
   structured-chat journal are empty; worktrees, PTYs, status hooks, and the CLI
   are the active layer.

### Public-evidence caveats

- Liu & Han (arXiv 2610.04433): harness can change cost/task by up to 3×;
  OpenCode preamble ≈7k tokens vs Claude Code ≈16.6k; OpenCode trailed by up to
  9pp on some models.
- Harness Leaderboard: OpenCode $3.35 per solved task vs Pi $0.29. **Cost per
  task ≠ cost per solved task.**
- Orca publishes no efficiency benchmark. Treat its “10x/100x” language as
  marketing, not measurement.
- Local sample is one host, one model family, ~$80 total spend. Do not
  generalize it into a benchmark.

---

## 1. Design rules

1. **Stable prefix first.** Cache-friendly prompt order: charter → security /
   output contract → project conventions → skills catalog (stable) → volatile
   run context last. Never change tool schemas mid-run.
2. **Context firewall.** Exploration runs in a separate fresh-context agent with
   read-only tools; only a bounded summary returns to the parent.
3. **Worktree parallelism with disjoint ownership.** Parallel implementation
   lines own disjoint files/directories; integration is serial.
4. **Event/operation receipts.** A retried operator or client request must not
   create a duplicate run, prompt, continuation, or merge.
5. **Measure the right denominator.** Track cache hit, preamble tokens, tool
   output bytes, compaction count, queue wait, and cost per solved task.
6. **Do not weaken gates.** No automatic permission bypass, no hidden routing,
   no compaction-as-memory, no automatic conflict resolution without policy.

---

## 2. Workstreams

### E1 — Cache and preamble telemetry (P0)

**Goal:** make the dominant cost lever visible and actionable. No warming by
default; no provider-specific hidden behavior.

Steps:

- [ ] Add `UsageTotals.cache_hit_ratio` (computed:
      `cache_read / (input + cache_read + cache_write)`) and expose it through
      `session.usage`, `project.usage`, and `daemon.metrics`.
- [ ] Add a namespace-scoped `ContextCacheStatus` RPC:
      `session.cache` → input, cache read/write, hit ratio, last call, prompt
      prefix hash, estimated preamble tokens.
- [x] Emit `context.cache` after provider usage settlement (operational:
      cache read/write, hit ratio, prefix hash; no prompt text).
- [ ] Add Prometheus metrics:
      `verlinpark_cache_read_tokens_total`,
      `verlinpark_cache_write_tokens_total`,
      `verlinpark_cache_hit_ratio`,
      `verlinpark_preamble_tokens_estimate`.
- [ ] CLI: `vp cache <session>`.
- [ ] Swift: Usage & Budget shows cache hit ratio and cached vs uncached input.
- [ ] Tests: hit-ratio math (zero input, cache > input clamp), replay/usage
      settlement, protocol defaults, CLI parse.

**Non-goals:** automatic warming, provider TTL inference, hidden cache routing.

**Acceptance:** a long session shows a stable hit ratio; changing the tool
schema or system prefix produces a visible drop; no extra provider call.

---

### E2 — Read-only scout agent (P0)

**Goal:** implement the context firewall inside VerlinPark: cheap, fresh-context,
read-only exploration whose summary returns to the parent.

Steps:

- [ ] Add an auxiliary `Scout` structural role (or a dedicated read-only agent
      path) with:
      - no write/edit/apply_patch/shell/process tools
      - read/grep/glob/file_info/repo_map/find_symbol/find_references/
        dependency_graph/affected_tests/code_diagnostics/git-read only
      - optional source retrieval (`list_sources`/`search_sources`/`read_source`)
      - no delegation, no merge-queue entry, no worktree
- [ ] Add a `scout` tool for Leader/Manager:
      `{ objective, files?, max_steps? }` → bounded findings + evidence refs.
- [x] Enforce:
      - per-session Scout concurrency cap (default 3)
      - token/step cap
      - cancellation on session cancel
      - session budget reservation
      - read-only tool policy + post-run tracked/untracked guard
- [ ] Record `agent.started`/`agent.finished` with role `scout`, and a
      `scout.completed` summary event; no `merge_queue` row.
- [ ] Brief: `AgentSpec.profile = research`-style instructions, but Scout must
      return `file:line` references and explicitly say when evidence is missing.
- [ ] Tests: tool catalog excludes writes, scout cannot commit/merge, budget and
      cancel, summary event, no worktree creation, replay/agent roster.

**Non-goals:** network research by default, nested scouts, autonomous coding.

**Acceptance:** a Leader can ask “map the auth flow” without polluting the root
with raw file dumps; the Scout result is bounded and auditable.

---

### E3 — Idempotent session operations (P0)

**Goal:** mirror Orca’s operation receipts so retries after a lost reply or a
daemon crash cannot double-start a run or duplicate a prompt.

Steps:

- [ ] Add `operation_id: Option<String>` to:
      - `session.prompt`
      - `session.defer`
      - `session.steer`
      - (optional) `session.create`
- [ ] New SQLite table `session_operations`:
      `(session_id, operation_id)` PK, `method`, `fingerprint`, `status`
      (`pending/completed/failed/unknown`), `result_json`, `error`,
      `created_at`, `updated_at`.
- [x] Engine helper:
      - same ID + same fingerprint + completed → replay stored result
      - same ID + same fingerprint + pending → return “in progress”
      - same ID + different fingerprint → conflict
      - failed → allow a safe retry
      - unknown (daemon restarted mid-operation) → refuse with an inspection
        message; do not duplicate
- [ ] Mark `pending` → `unknown` on daemon startup.
- [ ] CLI `--operation-id` on `vp prompt` / `vp defer` / `vp steer`.
- [ ] Tests: replay, conflict, in-progress, restart-unknown, failed retry,
      duplicate continuation/steer prevention.
- [ ] ADR.

**Non-goals:** distributed idempotency across daemons; exactly-once model calls
(provider streams are not transactionally replayable).

**Acceptance:** a client can safely retry a prompt with the same operation ID;
the second call returns the first result or a clear conflict/in-progress error.

---

### E4 — Efficiency dashboard (P1)

**Goal:** surface the measurements where operators already work.

Steps:

- [ ] Manage Dash → Usage & Budget: `EfficiencyCard` with cache hit ratio,
      cached/uncached input, output, cost, context usage, compaction count.
- [ ] Stage context popover: cache hit ratio + last prefix hash.
- [ ] Session table sortable by cost / cache hit / tokens.
- [ ] Show per-task usage in Task Detail where the ledger has task ids.
- [ ] CSV export includes cache hit ratio.
- [ ] Tests: decode, formatting, empty data, legacy payloads.

**Non-goals:** predictive cost, cross-user comparison, model benchmarking.

---

### E5 — Tool output shaping and paging (P1)

**Goal:** bound what the model sees while retaining the full result for audit.

Steps:

- [x] Define per-tool caps (lines/bytes) for shell/read/grep; keep the full
      result in a content-addressed tool-output store (or artifact-like file).
- [x] Tool result includes `output_ref`, `truncated`, `total_bytes`.
- [x] Add `read_tool_output` with offset/limit and a hard cap.
- [x] Emit `tool.output_truncated`/`tool.output_spilled` with sizes, no content.
- [x] Tests: cap boundaries, paging, ref retention/GC, no path escape.

**Non-goals:** replacing shell output with an LLM summary; compressing away
diagnostics silently.

---

### E6 — Background compaction and keep-token budget (P1)

**Goal:** reduce compaction latency and preserve recent detail.

Steps:

- [x] Add `keep_tokens` (default 15k) alongside keep-messages.
- [x] Add optional prepared summary generation before the threshold; hold the
      summary in memory, swap at threshold; discard on model/instruction/plan
      change.
- [x] Preserve current task/plan/acceptance as structured state in the summary.
- [x] Metrics: compactions, compaction latency, tokens before/after, reuse,
      fallback.
- [x] Tests: threshold math, prepared summary reuse, model switch invalidation,
      fallback to synchronous compaction.

**Non-goals:** infinite context, lossless compaction, relying on summaries for
durable state.

---

### E7 — Cross-process continuation and lease (P2)

**Goal:** make continuations and runtime ownership survive daemon replacement in
multi-daemon/remote setups.

Steps:

- [x] Durable lease per continuation series; fencing token.
- [x] Claim/ack protocol with expiry and recovery.
- [x] Reconcile orphaned series after daemon restart.
- [x] Tests: two daemons cannot deliver the same child; stale owner rejected.
      Scope: processes sharing one SQLite data directory, not cross-host
      consensus.

**Non-goals:** distributed scheduler for all sessions; cross-host clock trust.

---

### E8 — Cost-per-solved-task benchmark harness (P2)

**Goal:** measure what matters, not just tokens.

Steps:

- [x] Fixture tasks with deterministic acceptance (tests/patches).
- [x] Run N repetitions per configuration; record wall-clock, tokens, cost,
      verifier pass, merge success, human interventions.
- [x] A/B cache stability, scout vs root, 1 vs N worktrees, orchestrator vs
      subagents.
- [x] Publish a local JSON/Markdown/CSV report; do not claim universal rankings
      from one run.

**Non-goals:** public leaderboard, model quality claims.

---

## 3. Parallelization map

| Workstream | Files | Depends on |
|---|---|---|
| E1 cache telemetry | usage/protocol/daemon/CLI/Swift | — |
| E2 scout | agent/tools/store/protocol | — |
| E3 idempotency | protocol/store/core/daemon/CLI | — |
| E4 dashboard | Swift only | E1 fields (optional decode) |
| E5 tool output | tooling/agent/store/protocol | — |
| E6 compaction | agent/context/protocol | — |
| E7 cross-process | store/continuation/daemon | E3 receipts |
| E8 benchmark | tests/harness/docs | E1–E6 |

Safe parallel set for one round: **E1 + E2 + E3 + E4** (E4 decodes E1 fields as
optional). E5/E6 are next-round candidates because both touch `agent.rs` tool
execution/compaction and should not race E2.

---

## 4. Measurement and acceptance

Every workstream must add tests and update:

- `docs/PROGRESS.md` (what exists, test counts)
- `docs/AUDIT.md` (limits)
- `AGENTS.md` / `README.md` when the user-facing surface changes
- one ADR for architectural decisions

Efficiency acceptance for the round:

- cache hit ratio visible in `session.usage`, `daemon.metrics`, and the app;
- a scout call produces no write/merge and a bounded summary;
- a repeated prompt with the same operation ID does not start a second run;
- all existing gates (approval, sandbox, write scope, Verifier, merge, capacity,
  budget) remain unchanged;
- `just lint`, `just test`, `just bundle` pass.

---

## 5. Non-goals (explicit)

- No default permission bypass or “worktree is the sandbox” security model.
- No hidden model routing; Scout/Worker models are user-configured.
- No LLM-written memory or truth judgments.
- No compaction-as-memory; durable state stays in events/task results/memory.
- No claim that cache share equals cost savings; cache reads are still billed.
- No universal “OpenCode/Orca is faster” claim from a single local sample.

---

## 6. Current round status

- [x] Playbook archived.
- [x] E1 cache telemetry — `UsageTotals`/`UsageBucket`/project/daemon
  `cache_hit_ratio`, `session.cache` / `vp cache`, `context.cache`,
  Prometheus cache-hit/preamble metrics. No warming/TTL inference.
- [x] E2 scout agent — read-only `scout` tool, no shell/write/merge/worktree,
  per-run cap 3, shared budget, post-run guard, `scout.completed`.
- [x] E3 idempotent session operations — `operation_id` on prompt/defer/steer,
  `session_operations` receipts, replay/in-progress/conflict/unknown.
- [x] E4 efficiency dashboard — Manage Dash Efficiency card, Stage cache-hit
  pill, Collab Scout node.
- [x] E5 tool output paging — `tool_outputs` store, per-turn shaping,
  `read_tool_output`, status/list/prune/audit, GC.
- [x] E6 background compaction / keep-token budget — `compact_keep_tokens`,
  structured plan state, prepared summaries, metrics/fallback.
- [x] E7 cross-process continuation/lease — fenced runtime leases,
  continuation claim/ack, integration effect boundary. Same-host SQLite scope.
- [x] E8 cost-per-solved-task benchmark harness — `vp-bench` scenarios,
  deterministic acceptance, JSON/Markdown/CSV, compare rules.

Round 1 commits: `32b449b` (E3), `97fdeb2` (E1), `3fc2bed` (E4),
`71544fb` (E2). Round 2 commits: `b0adba1` (C4), `26256ed` (C2),
`93a991a` (C3), `bef841a` (C1). Test baseline after round 2: **1,330 Rust /
311 Swift**. Limits are recorded in ADR-0159–0163 and `PROGRESS.md`.

Update the checkboxes as work lands, and keep this file as the durable index for
the next round.

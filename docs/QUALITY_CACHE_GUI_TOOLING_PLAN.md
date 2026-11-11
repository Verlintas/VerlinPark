# Cache, GUI Session, Tool UX, and LSP Precision Plan

> **Status:** active implementation plan.
> **Created:** 2026-11-11
> **Goal:** fix the concrete issues found in the cache audit and heavy-tool
> review: conversation cache breakpoints, GUI surface ownership, uniform tool
> failures/recipes, and LSP-backed code intelligence.
> **Budget:** one large round, target +30,000 net lines of production code and
> tests. No filler; every module must operate, fail closed, and be auditable.

---

## 0. Issue checklist

### Cache / prompt prefix

- [ ] Anthropic caches tools + stable system only; messages have no cache
      breakpoint, so long conversations are largely uncached.
- [ ] Core role prompt, source brief, and skill catalog share one cache block;
      a source/skill change invalidates too much.
- [ ] OpenAI adapter ignores `system_stable_prefix`; stable and volatile run
      context are concatenated into one system message.
- [ ] No explicit 5m/1h TTL policy; long human waits exceed the default 5m cache
      lifetime.
- [ ] No per-call cache savings, miss attribution, breakpoint plan, or
      compaction cache-reset metrics.
- [ ] No explicit cache warm path after resume or before a long session.

### Native/browser GUI

- [ ] `computer` and `browser` use separate lease surfaces and can drive the
      same physical GUI concurrently.
- [ ] A warm helper keeps a global lease for up to 600 idle seconds; other
      sessions get immediate `COMPUTER_BUSY` with no queue/holder/retry hint.
- [ ] Snapshot refs are snapshot-scoped with no generation/stale-ref contract.
- [ ] Wait is polling; no AX/window event path.
- [ ] No action timeline/target audit for operators.
- [ ] No sensitive-app/security-dialog denylist or explicit screenshot policy.
- [ ] Status spawns a temporary helper and does not cache/preflight TCC state.
- [ ] Multi-display/screen target selection is limited.

### Tool UX

- [ ] Error results are mostly strings; no uniform
      `code/retryable/hint/evidence` contract.
- [ ] 40+ schemas with overlapping read/search/edit tools lack stable recipes.
- [ ] No per-tool latency/error/output-byte timeline in the operator UI.
- [ ] `find_references`/`find_symbol`/`code_diagnostics` are lexical or
      build-based even when a language server is available.
- [ ] No test-result cache/impact map (left for a later round).

### Deferred backlog

- [ ] F1 deterministic org scaling.
- [ ] F2 durable shell/process supervisor.
- [ ] F3 semantic project-source index.
- [ ] F4 remote worktree integration.
- [ ] F5 budget/critical-path governor.
- [ ] F6 approval/attention optimizer.

This round implements Q1–Q4 below. The deferred backlog stays documented; it is
not silently claimed as shipped.

---

## Q1 — Cache-first prompt architecture

### Design

- Introduce a prompt section model:
  - `core`: role charter, security/output contract, stable project conventions.
  - `project`: source brief and skill catalog (semi-stable).
  - `volatile`: run context, memory brief, objective.
  Keep a combined `system` string for compatibility; each section has a
  deterministic hash.
- Anthropic:
  - add top-level automatic caching where supported, so the breakpoint can move
    to the last cacheable message block;
  - keep explicit breakpoints for tools + core + project sections within the
    4-breakpoint limit;
  - put `cache_control` on message content blocks when automatic caching is not
    available/supported;
  - support `5m` default and opt-in `1h` TTL; never silently assume 1h support.
- OpenAI-compatible:
  - send stable system sections as the first system message(s), volatile run
    context as a later system message; keep tool definitions sorted and stable;
  - rely on automatic prefix caching; do not claim explicit breakpoints.
- Add `CachePolicy` (user-configured per role/project): enabled, ttl
  (`5m`/`1h`), warm_on_resume (default false), compact_aware (default true).
- Compaction:
  - record the cache generation before/after, the reset reason, and an
    estimated invalidation cost; do not delay compaction past context safety.
  - prepared summaries remain per-run.
- Metrics/events:
  - per call: prefix component hashes, breakpoint count, TTL, cache
    read/write/input/output, first-token latency;
  - derived: cache_read_ratio, cache_write_ratio, estimated_savings_usd,
    miss_reason (`prefix_changed`, `tools_changed`, `project_changed`,
    `ttl_or_provider_miss`, `compaction_reset`, `first_call`);
  - `session.cache.calls` bounded list; Stage/Efficiency surfaces.
- Optional explicit `session.cache.warm` / `vp cache warm` using the configured
  stable sections and tools; records its own usage/cost; no hidden call.
- Tests: Anthropic body assertions for breakpoints/TTL, OpenAI system ordering,
  component hash stability, miss-reason derivation, warm/cost accounting,
  protocol/Swift decode, no added provider call by default.

### Files

`vp-providers` adapters, `vp-core` prompt/agent/cache metrics, protocol, daemon,
CLI, Swift Efficiency/Stage, ADR-0168.

---

## Q2 — GUI session layer

### Design

- Unify `browser` and `computer` under one logical GUI surface. New acquisitions
  use a shared `gui` lease; legacy `browser`/`computer` rows remain readable and
  are migrated/expired safely.
- Separate helper warm lifetime from GUI lease lifetime:
  - after an action, retain the lease for a bounded `gui_grace_secs` (default
    30–60s) so multi-step interaction is fast;
  - release the lease on idle grace expiry while keeping helpers warm;
  - reacquire on the next action;
  - close/session-end releases immediately.
- Bounded wait queue instead of immediate `COMPUTER_BUSY`:
  - configurable wait timeout (default 15s), FIFO/priority-safe;
  - return holder session/agent, `retry_after_ms`, and a typed busy code when
    timeout expires; no hidden infinite wait.
- Snapshot generation:
  - every snapshot returns `snapshot_generation`; refs are bound to it;
  - stale refs return a typed `STALE_REF` error plus a fresh snapshot hint;
  - never auto-retry input actions on stale refs.
- Wait/observe:
  - add AX/window change observation where practical, with bounded timeout and
    polling fallback; document the limits.
- Safety:
  - configurable sensitive app/window policy (System Settings, SecurityAgent,
    password managers) requiring approval or deny;
  - screenshot/snapshot risk follows project policy; no silent privacy bypass.
- Operator observability:
  - bounded GUI action timeline: action, target app/window, generation,
    latency, result/error code, lease generation; no screen content logged.
  - cache helper/TCC status for a short interval; status should not spawn a
    helper on every dashboard refresh.
- Tests: unified lease contention/queue, idle release/reacquire, browser vs
  computer serialization, stale refs, sensitive target policy, timeline/status,
  helper crash respawn, existing browser/computer tests green.

### Files

`vp-core/src/computer.rs`, `browser.rs`, GUI lease/session module, protocol,
daemon RPC/sweep, CLI, Swift Manage Dash/Stage, ADR-0169.

---

## Q3 — Uniform tool errors and recipes

### Design

- Add a structured failure contract used by core tools:
  `{ code, message, retryable, hint, evidence?, tool, action? }`.
  Preserve `content` for the provider and `structured` for events/UI.
- Convert core tool failures (file/edit/patch/git/shell/process/LSP/browser/
  computer/remote/tool_output/MCP) to stable codes and actionable hints.
  Unknown errors remain `TOOL_FAILED` and are not silently retried.
- Add a bounded, stable `ToolRecipe` catalog to the system prompt/`tool_help`:
  read many files, symbol navigation, safe edit, test/check, GUI verification,
  remote operation. Recipes are static and cache-friendly; no dynamic schema
  changes per turn.
- Add per-tool metrics: latency, success/error code, output bytes/truncation,
  retries, approval wait. Emit `tool.finished` additive fields; expose a bounded
  timeline in Manage Dash/Stage.
- Tests: error code/hint/retryability, provider content remains compatible,
  recipe rendering is stable/cacheable, metrics/timeline bounds, Swift decode.

### Files

`ToolOutput`/core tool modules, agent dispatch/events, protocol, daemon, Swift,
ADR-0170.

---

## Q4 — LSP-backed code intelligence

### Design

- Make LSP a first-class optional backend for:
  - `find_symbol` (workspace symbols),
  - `find_references` (exact references/definition),
  - `code_diagnostics` (language-server diagnostics),
  - symbol navigation in `repo_map`/`build_context` where safe.
- Persistent per-worktree/language servers already exist; reuse lifecycle,
  sandbox, idle reaping, and bounded frames.
- Every result declares `source: "lsp" | "lexical" | "build"` and confidence;
  automatic fallback is explicit. Never pretend lexical references are exact.
- `lsp_query` remains the low-level escape hatch; add definition/hover/symbols
  if not already covered.
- No auto-install; missing server returns an actionable unavailable status.
- Tests: fake language server over stdio, definition/reference/diagnostic
  mapping, range handling, timeouts, crash/restart, lexical fallback, no server,
  existing code-index tests green.

### Files

`vp-core/src/tooling/lsp.rs`, `tooling/code.rs`, agent dispatch, protocol, CLI,
Swift labels, ADR-0171.

---

## 5. Integration and acceptance

Integration order: **Q4 → Q3 → Q2 → Q1**. Q1 merges last because it changes
prompt/provider request construction.

- +30,000 net lines of production code/tests/docs from `a901230`.
- No filler, no dead code, no untested happy path.
- `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace`,
  `just lint`, `just test`, `just bundle`.
- Approval, Seatbelt, path jail, write scope, Verifier, merge, capacity, and
  budget gates unchanged or stricter.
- Cache changes must be measured; no claim that cache reads are free.
- GUI changes must not weaken the trust model: the helper remains unsandboxed
  and TCC-bound; the lease is still single-daemon.
- LSP remains optional; lexical fallback is explicit.

## 6. Non-goals

- No distributed GUI lease/queue, remote GUI service, or cross-host control.
- No automatic cache warming by default; no provider TTL inference.
- No new agent write path; no hidden model routing.
- No HNSW/GPU/code-index replacement.
- No test-result cache in this round.
- No Bitbucket/remote merge work in this round.

# 0217 — Tool search v2 and delta browser snapshots

- Status: Accepted
- Date: 2026-11-14
- Scope: C8 set-complete tool discovery, turn-boundary schema promotion, incremental browser snapshots

## Context

`tool_search` originally scored names and descriptions with a small substring
heuristic, returned only the top N without saying what was omitted, and could
not promote a discovered schema into the frozen run bundle. Browser snapshots
always returned the whole tree and text, even when only one control changed,
and actions had no daemon-side snapshot generation guard.

C8 requires deterministic set-complete retrieval, explicit schema promotion
with cache pricing, and incremental snapshots that preserve the existing GUI
lease and `STALE_REF` semantics.

## Decision

### Tool search v2

- `crates/vp-core/src/tooling/search.rs` implements BM25 (`k1=1.2`,
  `b=0.75`) over name (field boost 3x), category and task kind (2x), and
  description (1x). Tokens are lowercase alphanumeric with a bounded singular
  stem. Exact name and name-substring matches receive deterministic boosts.
- `tool_search` accepts `query`, `task_kind`, `limit`, `include_schema`, and
  `promote`. It searches the already role-filtered built-in catalog plus
  enabled MCP tools, and never bypasses role policy or MCP pins.
- A deterministic prerequisite map (for example `edit_file`→`read_file`,
  `search_sources`→`list_sources`, delegation→`team_status`) adds closure
  entries to the returned set. Prerequisites are discovery hints, not
  authorization; the runtime's contracts, approvals, sandbox, and write scope
  remain authoritative. Missing prerequisites are explicit
  `{name, reason, required_by}` entries. MCP results also require the generic
  `mcp` bridge when callable.
- The response includes `set`, detailed `tools`, `missing`,
  `completeness_estimate`, `truncated`, `prerequisite_additions`, and
  `outside_bundle`. Omitted ranked matches are reported as `result_limit`;
  a result cap is reported rather than hidden.
- `BM25`, completeness, prerequisites, and empty-query ordering are pure and
  unit-tested; no model call is involved.

### Turn-boundary schema promotion

- `promote: true` asks for returned out-of-bundle built-in tools to be added to
  the next provider request. The current request keeps the frozen schema set,
  so tool search cannot change the bytes of the call that produced it.
- At the next turn boundary the runtime resolves each candidate against the
  role-filtered schema catalog, deduplicates/sorts the tool list, and either
  applies it or rejects it if the new tool preamble would exceed the context
  hard limit. The decision is event-backed as `bundle.revision` with
  `applied`, previous/new tool hashes, previous/new cache epoch, added tools,
  target/over-target state, previous/new schema tokens, schema token delta, and
  an estimated cache invalidation cost from the configured cache-read price.
- An applied promotion emits a new `context.prompt_prefix` event with
  `bundle_revision`, `tool_bundle_frozen=false`, the new `tools_hash`, cache
  epoch, per-call cache plan, and existing identity/memory metadata. The next
  ordinary `context.cache` event carries the new tool hash and revision, so
  `session.cache` and `session.instructions` read the post-promotion state.
  The cache miss reason can therefore be attributed to `tools_changed`.
- Promotion is built-in only; MCP schemas stay project-local and pin-gated.
  No automatic promotion exists: the agent must call `tool_search` with
  `promote=true`. Over-target bundles are allowed only as an explicit
  promotion and are marked `over_target=true`; context hard limits still fail
  closed. The price is an estimate based on configured prices, not a provider
  cache guarantee.

### Delta browser snapshots

- `BrowserManager` keeps a per-session/worktree snapshot state with a
  monotonic daemon-side `generation`, the last tree, and the last visible text.
  The first snapshot is full. Later snapshots default to delta and return
  changed/new/removed refs plus a bounded line-level text delta instead of a
  full tree. `delta=false` requests a full snapshot again.
- Snapshot output includes `generation` and `base_generation`. Input actions
  (`click`, `type`, `press`, `select`) may carry `snapshot_generation`; a
  mismatched or zero generation returns `STALE_REF` before the GUI lease is
  acquired, with the current generation and no automatic retry. Omitting the
  field preserves the legacy helper-ref path for existing clients.
- Closing, sweeping, crashing, or replacing a helper invalidates the stored
  tree but keeps the monotonic generation, so old refs cannot become valid
  again. AI refs remain helper-generated; delta computation is daemon-side and
  does not change the helper protocol or the GUI lease.
- The delta contains only DOM-derived bounded refs/text, never cookies,
  credentials, or typed secrets (password values remain `***`). Screenshot and
  console behavior are unchanged.

## Security and gate preservation

Tool search still searches only the role-filtered catalog; promotion cannot
add a tool outside role policy, bypass MCP pin trust, or skip a contract,
approval, effect receipt, sandbox, write scope, Verifier, capacity, or merge
gate. Promotion only changes the advertised schema set and cache identity.
Browser delta output is derived from the same sandboxed helper snapshot; stale
refs fail closed and no action is auto-retried.

## Limits

- BM25 is lexical, not semantic. Prerequisite edges are a hand-maintained,
  bounded discovery map, not a planner.
- Completeness is a deterministic estimate over returned and explicitly
  missing entries; a query synonym outside the corpus may still be missed.
- Promotion is built-in only, one-turn delayed by design, and can exceed the
  bundle target (explicitly marked); it is still subject to the context limit
  and role policy. Cache cost is estimated, not a provider hit guarantee.
- Delta snapshots compare daemon-cached DOM trees, not browser-native layout
  diffs; the helper protocol is unchanged. Generation state is per daemon and
  is not shared across hosts.
- No visual regression judgment, screenshot diffing, or semantic page
  understanding is added.

## Verification

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`: 3,920 passed, 0 failed
- New tests cover BM25 ordering/task-kind boosting, prerequisite closure and
  missing entries, empty-query determinism, set/completeness output, real
  turn-boundary schema promotion with `bundle.revision` and cache pricing,
  delta ref/text correctness, and runtime `STALE_REF` rejection.

## Related

- ADR-0193 (task contracts)
- ADR-0196 (runtime pinning and replay)
- ADR-0216 (bounded dynamic fan-out)
- `docs/CONTRACTED_EXECUTION_PLAN.md` C8

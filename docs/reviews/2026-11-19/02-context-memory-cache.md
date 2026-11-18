# Context, Prompt Cache, Memory, and Sources Review — `vp-core`

- Date: 2026-11-19
- Baseline: `d239dca` ("Fix live provider compatibility and budget preflight")
- Scope: `crates/vp-core/src/instruction.rs`, `prompt_cache.rs`, `conversation.rs`, `context.rs`, `cache_audit.rs`, `cache_epoch.rs`, `memory.rs`, `memory_index.rs`, `memory_embedding.rs`, `identity_memory.rs`, `identity_prompt.rs`, `source_ann.rs`, `source_embedding.rs`, `source_retrieval.rs`, `sources.rs`, `engine/sources/`, `engine/memory/`, `agent/context/`, and the memory/source tools in `agent/tools/`; the corresponding `vp-store` read/write paths and `vp-providers::canonical` serialization where they are the other half of a contract.
- Method: read-only source review. Read `AGENTS.md`, the relevant `docs/ARCHITECTURE.md` sections (§7.0.2 memory, §7.0.4 efficiency, §7.5 context assembly), `docs/AUDIT.md`, the cache/memory ADR summaries in `AGENTS.md`, and the relevant tests. Ran one targeted baseline check: `cargo test -p vp-core --lib instruction::tests` (24 passed, 0 failed). No live provider or network calls were made; no source, test, or config was changed. The only file created is this report.

## 1. Executive summary

The cache-first machinery is real, not cosmetic: prompt sections have fixed ordinals, are content-addressed, are hash-verified before use, and are split into pinned, generation-scoped, and regenerated classes. The provider request is assembled from the stable head plus a bounded tail, the canonical message chain is recorded, and the epoch/observation tables are event-backed rather than inferred from the UI. Compaction has a late window-derived threshold, a token-aware safe cut, a summary-plus-trim fallback, durable checkpoints, and explicit cache-reset accounting. Memory and source reads are project-scoped in SQL; identity memory adds agent/department/project visibility; source retrieval checks the stored content hash before scoring; and the ANN paths all have explicit exact-scan fallbacks and generation fences.

The defects are concentrated in the places where two independently-written "proofs" or "estimates" meet:

1. The canonical conversation-continuity tracker is seeded from a hash produced by a **different hash scheme**. On the first call of a resumed run that reuses the same agent id (the Leader path and daemon-restart recovery), the canonical tracker reports a spurious structural break (`unknown`), while the same `context.cache` event's `append_only` field says `true`.
2. Manual compaction and manual context checkpoints build a **different prompt** from the real run: the legacy `core + run_context` string, missing identity/source/skill/contract sections, and including the volatile objective. They also store a synthetic `prompt_prefix_hash` that cannot match any `context.prompt_prefix` event.
3. Cross-session cache attribution counts a **same-session explicit warm** as reuse from another session whenever any other session has observed the epoch.
4. The identity-memory prompt brief takes the 128 most recently updated rows across all scopes and only then ranks by scope priority/pinned/verified. A busy agent can therefore starve department/project memory (including an old pinned memory) out of its own brief.
5. The `agent/memory` tail is sent on every request but is not counted by the context meter, compaction base tokens, or the session budget preflight.
6. Session goals and prompts are admitted without a size bound; a goal above the 64 KiB `session/context` section cap creates a session whose every run fails closed.

The remaining findings are smaller lifecycle and resource issues: a full active-memory scan on every memory create under the global store lock; import dedupe limited to the first 1,000 sources; and a reactivated legacy memory retaining its `superseded_by` edge. No cross-project data leak was found in the reviewed paths, and no live-provider cache claim is inferred from the fake-provider tests.

Bottom line: the instruction/epoch/scope design is sound, but cache/continuity telemetry and manual context management are not yet trustworthy enough to drive operator decisions. Fix findings 1–3 before treating `cache_break`, `prefix_hash`, or cross-session reuse numbers as evidence.

## 2. Findings table

| ID | Severity | Confidence | Location | Issue |
|----|----------|------------|----------|-------|
| C1 | Medium | Confirmed | `crates/vp-core/src/agent/run_loop/body.rs:1316-1344`; `conversation.rs:53-55`; `crates/vp-providers/src/canonical.rs:525-577,683-700` | `context.cache.message_prefix_hash` is overwritten with the prompt-cache hash, but the canonical tracker reads it on resume; the first call of a resumed run with a stable agent id is reported as a structural break (`unknown`) even when the history is append-only |
| C2 | Medium | Confirmed | `agent/service_run/context.rs:59-76,197`; `agent/service_run/compaction.rs:74-75,121-144`; `agent/context/prompts.rs:616-623` | Manual compaction/checkpoints use the legacy combined prompt and a synthetic `prompt_prefix_hash`; they omit identity/source/skill/contract bytes, include the volatile objective, and under/over-estimate the real request |
| C3 | Medium | Confirmed | `crates/vp-store/src/store/cache.rs:308-319`; `crates/vp-core/src/cache_epoch.rs:494-508`; `agent/context/bundles.rs:241` | A same-session explicit warm is attributed as cross-session cache reuse on the first ordinary call whenever another session has observed the epoch |
| C4 | Medium | Confirmed | `crates/vp-store/src/store/identity_memory/query.rs:249-287`; `crates/vp-core/src/identity_memory.rs:218-227,301-348`; `engine/agents.rs:647-660` | Identity-memory brief candidates are the 128 newest rows across scopes; scope priority and pinned/verified ranking are applied after that cut, so department/project or old pinned memory can never enter the brief |
| C5 | Low‑Medium | Confirmed | `agent/run_loop/body.rs:197-224,875-899`; `agent/support.rs:117-147`; `prompt_cache.rs:298-304` | The `agent/memory` tail (up to 6 KiB) is sent every turn but omitted from `system_tokens`, `context.usage`, compaction base tokens, and the budget preflight |
| C6 | Medium | Confirmed | `crates/vp-store/src/store/sessions.rs:100-103`; `agent/context/prompts.rs:192-210`; `instruction.rs:197-204,506-516`; `store.rs:6275-6293` | Session goal/prompt are unbounded; a goal above the 64 KiB `session/context` cap bricks every run, and the goal is expanded without a term cap into the memory FTS query |
| C7 | Low | Confirmed | `engine/sources/import.rs:101-112`; `crates/vp-store/src/store/sources/ingest.rs:126` | `import_sources` builds its dedupe set from the newest 1,000 sources; projects with more can re-import existing files |
| C8 | Low‑Medium | Confirmed | `crates/vp-store/src/store/memory/records.rs:527-556` | Reactivating a `superseded` legacy memory leaves `superseded_by` set and can create two active rows for the same normalized kind/title |
| C9 | Low | Confirmed | `instruction.rs:656-734`; `prompts.rs:620-622`; `crates/vp-store/src/store/cache.rs:44-71` | `materialize` calls `put_instruction_blob` for every section on every run even when the blob is unchanged, and runs synchronously in async context under the global store mutex |

Severity is impact if the code path is reached; confidence is that the path is reachable as described. None of these findings requires a live provider; C1 and C3 can be reproduced with the existing fake-provider harness.

## 3. Detailed findings

### C1 — Resumed runs start with a spurious canonical cache break

**Severity:** Medium · **Confidence:** Confirmed

**Evidence.**

On every ordinary provider call, `run_loop_body` builds a payload and then merges two telemetry projections into it:

1. `crates/vp-core/src/agent/run_loop/body.rs:1316-1323` merges `crate::conversation::canonical_payload(&observation)`. That payload contains the canonical `message_prefix_hash` from `vp_providers::canonical` (`crates/vp-providers/src/canonical.rs:498-519`), which is a chain hash over provider-projected history messages, and `message_hashes`/`message_hashes_complete`.
2. `body.rs:1337-1344` then **overwrites** `message_prefix_hash` and `previous_message_prefix_hash` with `cache_telemetry.*`. Those values are produced by `prompt_cache::message_prefix_hash` (`crates/vp-core/src/prompt_cache.rs:818-825`), which concatenates role/content/tool calls/tool-call id with `0x1f` separators and SHA-256s the result. That is a different algorithm from the canonical per-message hash plus chain hash.

The two schemes are not interchangeable. On the next run, `run_loop_body` seeds `MessagePrefixTracker` from the latest persisted ordinary `context.cache` event (`body.rs:389-405`). `MessagePrefixTracker::from_event` calls `MessagePrefixSnapshot::from_event_payload` (`crates/vp-core/src/conversation.rs:53-55`), which reads the overwritten `message_prefix_hash` as `history_hash` (`crates/vp-providers/src/canonical.rs:525-577`). The persisted `message_hashes` are shortened to 16 characters and capped at 512 (`canonical.rs:495-519`). The event's `message_hashes_complete` flag is computed before that shortening and can say `true`, but `MessagePrefixSnapshot::from_event_payload` re-checks `hash.len() == 64` (`canonical.rs:550-555`) and sets the decoded snapshot's `message_hashes_complete` to false. `is_prefix` therefore falls back to:

```rust
chain_hash_at(&current.message_hashes, previous.message_count) == previous.history_hash
```

at `canonical.rs:683-700`. `current.message_hashes` is canonical; `previous.history_hash` is the prompt-cache hash. The comparison cannot match. `first_difference` then compares the stored 16-character prefixes to the current full hashes (`canonical.rs:706-734`); because those prefixes do match and `hashes_complete` is false, it returns no index. `reordered` also returns false. `ContinuityReport::detect` therefore yields `append_only: false` with `reason: Unknown` (`canonical.rs:661-679`).

`body.rs:1122-1140` emits a `context.cache_break` event for that reason before the provider call. The ordinary `context.cache` event for the same call contains `cache_break_reason: "unknown"` from the canonical payload, but its `append_only` field was then overwritten by the prompt-cache tracker and is `true`. The two fields in the same event contradict each other.

The warm path merges the two projections in the opposite order (`agent/context/bundles.rs:211-263`), so warm `context.cache` events carry the canonical hash while ordinary events carry the prompt-cache hash. This is additional evidence that the event schema does not have one stable meaning for `message_prefix_hash`.

**Impact.**

- Every resumed run that seeds from the previous call (the Leader path, daemon-restart recovery, and any second `session.prompt` on an idle session with a stable agent id) emits a `context.cache_break` with reason `unknown` on its first call, even when the conversation is an exact prefix of the previous one.
- `session.cache` break counts, `vp cache` output, and `cache_audit` findings are polluted with a structural break that did not happen. A real later break (`message_edited`, `reordered`, or `compaction_reset`) is no longer distinguishable from this noise because the first call already consumed the comparison slot.
- The same event says `append_only: true` and `cache_break_reason: "unknown"`, so a UI or operator cannot trust either field.
- The documented "append-only proof" in `AGENTS.md` (canonical serialization plus `message_prefix_hash`/`previous_message_prefix_hash`) is not actually being taken across runs.

**Reproduction / check.**

Use the existing fake-provider `service` fixture:

1. Create a session and Leader; run `service.start` with prompt 1; wait idle.
2. Run `service.start` with prompt 2; wait idle (no compaction, no source/skill edit).
3. Read `recent_events_of_kind(session, "context.cache", 20)` and `recent_events_of_kind(session, "context.cache_break", 20)`.

Expected: the first call of run 2 has no structural break (canonical append-only proof succeeds); a post-call `provider_cache_miss` is acceptable if the fake provider reports no reads, but `unknown`/`message_edited`/`message_reordered` is not. Its `context.cache.append_only` should be `true` with `cache_break_reason` absent/null. Actual: a `context.cache_break` with `reason = "unknown"` exists for run 2's first call, while `append_only` is `true`.

The existing test `memory_is_appended_once_and_keeps_history_append_only_across_runs` (`crates/vp-core/tests/append_only_conversation.rs:280-442`) runs twice but only asserts the prompt-cache `append_only` field and the prompt-cache previous/current hash pair; it does not inspect `context.cache_break` for run 2. The daemon-restart test at `:770-840` has the same gap.

**Suggested fix.**

Keep one key for one meaning. Add explicit canonical keys (for example `canonical_message_prefix_hash`, `canonical_previous_message_prefix_hash`, `canonical_message_prefix_count`) and have `MessagePrefixSnapshot::from_event_payload` read those; keep the existing prompt-cache values under `raw_message_prefix_hash`/`raw_previous_message_prefix_hash` for `CacheCallTracker::from_event` and `aggregate_continuity`. Do not overwrite `canonical_payload`'s fields from the prompt-cache telemetry. For backward compatibility, a decoder may fall back to `message_prefix_hash` only when the canonical key is absent, but it must not treat a prompt-cache hash as a canonical chain hash.

**Test to add.**

`resumed_run_first_call_has_no_canonical_break`: two separate runs with unchanged project/prompt sections; assert (a) no `context.cache_break` with a structural reason (`unknown`, `message_edited`, `message_reordered`, `system_changed`) for run 2's first call (a `provider_cache_miss` is acceptable with a fake provider that reports no reads), (b) `context.cache.append_only == true`, and (c) `cache_break_reason` is null for a structural break. Also assert `context.cache.message_prefix_hash` equals `context.cache_break.message_prefix_hash` for the same call when a structural break does occur.

---

### C2 — Manual compaction and checkpoints estimate a different prompt than the run

**Severity:** Medium · **Confidence:** Confirmed

**Evidence.**

The real run builds a v3 prompt through `agent/context/prompts.rs:build_prompt_sections` (`:616-623`). That function resolves the project identity (`prompt_identity_for_run`), the source brief (`engine.source_brief`), the skill catalog (`engine.skill_catalog`), the session contract capsule, the session context, and the task objective, then materializes the ordered instruction store. The authoritative hashes are computed by `prompt_cache::PromptSectionHashes::compute` (`prompt_cache.rs:400-475`), including `prefix_hash_v3` (`prompt_cache.rs:589-600`), and the authoritative preamble is `sections.render()` plus the provider tail.

The manual paths do not use that builder:

- `crates/vp-core/src/agent/service_run/context.rs:59` uses `role_system_prompt(spec.role, project, session, &spec, &session.goal).render()` for `context_preamble`.
- `crates/vp-core/src/agent/service_run/compaction.rs:74-75` uses the same legacy renderer for `compact_now`.
- `context.rs:64-70` and `compaction.rs:121-127` compute a synthetic prefix hash as `sha256(format!("{system}\n{tools_json}"))`.

`role_system_prompt` returns `core` (charter + recipes) plus `run_context` (which includes the objective). It does **not** include the `agent/identity` section (up to 8 KiB), `project/sources` (up to eight indexed titles plus collection), `project/skills` (up to 4,000 chars), `session/context` (the actual goal/worktree/branch/write scope), or `session/contract` (the task contract capsule). Conversely, it includes the volatile objective that a v3 Leader does not receive in the head. The `spec` built by `context_preamble` also sets `identity_id: None`, so identity resolution never occurs.

`context_checkpoint_now` stores that synthetic hash in the checkpoint's `prompt_prefix_hash` (`context.rs:197`), and `compact_now` passes it into `compact_context_if_needed` as both the cache generation input and the checkpoint's `prompt_prefix_hash` (`compaction.rs:121-143`, then `agent/context/compaction.rs:776`). `context_restore_preflight` does not compare the checkpoint hash to any `context.prompt_prefix` event, so the mismatch is not detected; it only checks plan revision, provider/model, worktree cleanliness/descendant, and an estimate built from the same undercounting preamble (`context.rs:374-402`).

**Impact.**

- `compact_now` decides whether the session is over the threshold using a prompt that is not the prompt that will be sent. With an assigned identity, sources, skills, and a contract, the real base is larger and the manual compaction can leave the context above the real hard limit; with a large objective in `run_context`, it can overestimate and compact earlier than necessary. The next automatic compaction may repair the first case, but the boundary and its `estimated_tokens_*` are wrong.
- `context.checkpointed.prompt_prefix_hash`, `context.compacted.cache_generation_before/after`, and `context.compaction.cache_reset` record a value that no `context.prompt_prefix` event ever produced. A UI or audit that compares them cannot reconcile the checkpoint with the actual provider prefix.
- `context_restore_preflight` rejects plan/provider/model/worktree changes but not a changed prompt prefix (source/skill/identity/contract). A checkpoint taken before a source or skill edit can be restored because the estimate is also built from the legacy prompt and may still look below the threshold.
- `session.cache.plan` and `cache_plan_now` use `InstructionBuildMode::Preview` with `contract=None` and a stable spec (`agent/service_run/cache.rs:42-67`), so their reported `prefix_hash`/`head_sections` omit the contract and run/task sections too. This is the same root cause and affects the "plan the exact cache breakpoints" surface.

**Reproduction / check.**

Create a Leader session with an assigned identity, one indexed source, one skill, and a contract policy enabled. Run one turn so `context.prompt_prefix` is recorded. Then call `service.compact_now` (or `context_checkpoint_now`) without changing the plan/provider. Compare:

- `context.checkpointed.prompt_prefix_hash` vs the latest `context.prompt_prefix.prefix_hash`;
- `context.checkpointed.tokens_before` vs `system_tokens + tail + conversation estimate`;
- `session.cache.plan`'s `prefix_hash` vs the real prompt-prefix `prefix_hash`.

All three currently differ even when nothing changed. A second variant with a 5 KiB source brief and a 4 KiB skill catalog demonstrates the threshold error: the legacy estimate is below the hard limit while the real request is above it.

**Suggested fix.**

Factor a single `build_run_preamble(engine, project, session, spec, mode)` used by the run loop, warm, cache plan, manual compaction, and checkpoint/restore preflight. It should resolve identity/source/skill/contract consistently and return:
`PromptSections`, the real `PromptSectionHashes`, the resolved tail tokens, and the actual tool bundle. For a read-only caller, use `Materialize` with the existing instruction store (idempotent) or a read-only materialization that reads the persisted state/blobs; do not use `role_system_prompt` as a second prompt definition. Store `hashes.prefix_hash` (or the current persisted state's hash) on checkpoints and use the resolved preamble token count for `tokens_before/after`. Add a restore check for `prompt_prefix_hash` once the value is meaningful.

**Test to add.**

`manual_compaction_uses_the_real_prompt_identity`: with identity + source + skill + contract, assert `context.checkpointed.prompt_prefix_hash == latest context.prompt_prefix.prefix_hash`, `tokens_before == estimate(system + tail + messages)`, and `session.cache.plan.prefix_hash == context.prompt_prefix.prefix_hash`. Add a near-threshold case that asserts `compact_now` triggers at the same estimate the run loop would use.

---

### C3 — Same-session warm is counted as cross-session reuse

**Severity:** Medium · **Confidence:** Confirmed

**Evidence.**

`crates/vp-store/src/store/cache.rs:308-319` computes `cross_session_cache_read_tokens` as:

```rust
if record.first_session_call && record.prior_sessions > 0 {
    call.cache_read_tokens.min(call.input_tokens)
} else { 0 }
```

`first_session_call` is true when the session has no `prompt_prefix_observations` row for the epoch (`store.rs:4817-4877`); `prior_sessions` counts observations for the epoch from **other** sessions. Warm calls never create an observation: `warm_prompt_cache` emits only a `context.cache` event with `"warm": true` (`agent/context/bundles.rs:241`) and no `context.prompt_prefix`; `observations_from_events` skips warm events (`crates/vp-core/src/cache_epoch.rs:537-544`), and `aggregate_cache_events` treats them as a separate `warm_calls` count (`prompt_cache.rs:1547-1557`).

If session B explicitly warms the stable prefix and then makes its first ordinary call, the provider can serve that same prefix from B's warm. But if session A had previously observed the same epoch, `prior_sessions > 0` and B's first ordinary call records all of its `cache_read_tokens` as cross-session. The attribution code has no concept of a same-session warm.

The helper's own doc says "Only the first provider call of a session can be attributed **wholly** to another session" (`cache_epoch.rs:454-459`). That is already too strong: a provider prefix is shared only up to the last common section (the epoch excludes session/context, contract, and run/task), and a same-session warm is a direct counterexample.

**Impact.**

- `session.cache.cross_session_hit_tokens`, `session_cache_epoch`/`project_cache_epochs` reuse summaries, `vp cache` output, and project cache reports overstate cross-session reuse and often understate same-session warm effectiveness.
- Operators cannot use these numbers to decide whether a warm is worth its cost, and the "cross-session reuse" claim in the cache-first notes is not supported by the implementation for this case.
- The error compounds with C1: the first ordinary call after a warm can simultaneously report a spurious break and full cross-session reuse.

**Reproduction / check.**

Using a fake provider that reports a non-zero `cache_read_tokens`:

1. Session A: one run with the same Leader role, model, tool bundle, core/project sections; wait idle. This creates an epoch observation.
2. Session B: `service.warm_cache_now` (or the CLI equivalent) with the same role; then `service.start` for the first time; wait idle.
3. Read B's first `context.cache` event. Current behavior: `cross_session_first_call = true`, `cross_session_cache_read_tokens > 0`. Expected: those tokens were warmed by B itself; either the field is zero or it is labeled `same_session_warm`.

The existing `cache_epoch_lifecycle` test (`:303-365`) covers two sessions with no warm; `cache_first_agents` covers warm display but not the first ordinary call after a warm.

**Suggested fix.**

Before assigning cross-session tokens, look for a warm `context.cache` event for this session and the same epoch whose `prompt_prefix_hash`/`cache_epoch` matches and that is later than the last ordinary call. If present, set `cross_session_cache_read_tokens = 0` and add a bounded `same_session_warm` marker, or cap the cross-session value to the shared core/project prefix tokens rather than the full read. Update `CrossSessionAttribution` and the `cache_epoch_reused_from_sessions` docs accordingly; do not call the value a proof of another session.

**Test to add.**

`first_call_after_same_session_warm_is_not_cross_session`: create an epoch in session A, warm it in session B, then run B; assert `cross_session_cache_read_tokens == 0` (or a new same-session field) while the warm's own usage is still visible as a warm call. Add a second case where the warm's prefix does not match the run (different role/tool bundle) and cross-session stays zero for a different reason.

---

### C4 — Identity-memory brief can starve department/project memory

**Severity:** Medium · **Confidence:** Confirmed

**Evidence.**

`engine.identity_memory_brief` asks the store for candidates with `Some(128)` (`crates/vp-core/src/engine/agents.rs:647-660`). `list_agent_memories_for_brief` applies the viewer-scope SQL filter, then:

```sql
ORDER BY updated_at DESC, content_hash ASC, id ASC
LIMIT ?5
```

with `limit = 128` (`crates/vp-store/src/store/identity_memory/query.rs:249-287`). The Rust ranking then filters `brief_eligible` and sorts by `memory_score` (scope weight, pinned, verified, recency) and truncates to `MAX_BRIEF_CANDIDATES = 128` (`crates/vp-core/src/identity_memory.rs:218-227,190-214`). The final renderer selects by that rank and a 5,600-byte/24-item budget (`identity_memory.rs:301-348`).

There is no per-scope partition or pinned-first pre-selection in SQL. A single Worker or Leader identity with 128 or more recent agent-scope memories fills the entire candidate window; every older department and project memory is dropped before ranking. The same is true for an old pinned memory: pinning affects the score only after the recency cut.

**Impact.**

- The documented brief ranking ("agent 3/department 2/project 1, pinned, verified, 30-day recency") is not what the store materializes. In a long-lived project with a busy agent, the prompt can contain only that agent's private memory and no shared project conventions/decisions, even though shared memory is the more useful signal.
- Pinned project memory is not a retention guarantee for the prompt. An operator who pins a critical convention still will not see it once 128 newer agent-scope rows exist.
- This is a retrieval-quality and operator-trust defect, not a cross-scope authorization failure: the SQL viewer clause correctly excludes other agents' and other departments' memory. It is still a practical "loss of derived state" for the brief.

**Reproduction / check.**

Enable the identity policy, assign a Worker identity, and create 129 agent-scope memories for that identity (`remember` with `scope: agent`) with increasing `updated_at`, then one older project-scope memory with `pinned: true`. Build a run prompt for that identity. The brief contains only agent-scope rows; the pinned project memory is absent. A direct call to `list_agent_memories_for_brief` with limit 128 reproduces the same cut.

**Suggested fix.**

Give each visible scope a bounded candidate quota before the global limit, for example a SQL `ROW_NUMBER() OVER (PARTITION BY scope ORDER BY pinned DESC, updated_at DESC, ...)` with `WHERE rn <= 64` per scope, then take the union and rank; or run three bounded queries (agent, department, project) and merge. Always include pinned rows up to a cap, independent of recency. Keep the total candidate and byte bounds unchanged.

**Test to add.**

`brief_keeps_shared_and_pinned_memory_under_agent_pressure`: 129 newer agent-scope memories + one older pinned project memory; assert `total_candidates` includes both scopes, the project memory is selected, and the brief stays under the byte/item caps. Add a department-memory variant.

---

### C5 — `agent/memory` tail is not counted by context or budget estimates

**Severity:** Low‑Medium · **Confidence:** Confirmed

**Evidence.**

For a v3 prompt, `sections.render()` returns only the head sections (`prompt_cache.rs:197-200`); `sections.tail_sections()` are sent separately in `ChatRequest.tail_sections` (`agent/run_loop/body.rs:918-931`). `body.rs:197-224` computes:

```rust
let system = sections.render();
let system_tokens = estimate_context_tokens(&system);
let tool_tokens = ...;
...
if system_tokens + tool_tokens > context_hard_limit { ... }
```

The hard-limit check, `compaction_policy` base tokens (`body.rs:828-846`), the emitted `context.usage.estimated_tokens` (`body.rs:875-899`), and `estimate_request_tokens` (`agent/support.rs:117-147`) all use only `request.system`/`system_tokens` plus messages. `estimate_request_tokens` iterates `request.system` and `request.messages` and `request.tools`, but never `request.tail_sections`.

`agent/memory` is a real tail section (identity memory up to `IDENTITY_MEMORY_MAX_BYTES = 6 * 1024`, `identity_prompt.rs:40`) and is not persisted as a conversation message; `resolved_tail_messages` appends it on every request unless an identical message exists in history (`vp-providers/src/canonical.rs:1193-1231`). It is therefore omitted from the estimate but included in the provider input. `tail/memory` is normally embedded by `ensure_memory_anchor` (`agent/context/prompts.rs:276-337`) and counted as the anchor message, so `agent/memory` is the reproducible uncounted case.

**Impact.**

- A session can pass the hard-limit check and then send a request that is up to ~1.5k tokens larger than the configured hard limit (6 KiB of tail at the heuristic rate), with output reserve already deducted. The risk is concentrated near a threshold/window boundary and in small-window models.
- The Stage context meter and `context.usage` underreport the active request; `session.budget` preflight under-reserves by the same amount. Provider usage still settles the budget after the call, so the ledger is honest, but the pre-flight safety property is not.
- The context estimator's stated scope ("system prompt and tool schemas" plus messages) is not the actual request when the identity memory is present.

**Reproduction / check.**

Assign an identity with a full 6 KiB `agent/memory` brief, build a conversation whose estimated head+messages is just below `hard_limit`, and inspect the fake provider's captured request input tokens vs `context.usage.estimated_tokens`. The request contains the tail; the estimate does not. A direct unit call to `estimate_request_tokens` with a `ChatRequest { tail_sections: vec![...], .. }` returns the same value as with an empty tail.

**Suggested fix.**

Resolve tail tokens the same way the canonical serializer does: compute `resolved_tail_messages` from the request and capabilities and add their token estimate to `estimate_request_tokens`, `system_tokens`/`base_tokens`, and `context.usage`. A conservative fallback is to add `estimate_context_tokens(&sections.tail_text())`; it may double-count `tail/memory` when the anchor is embedded, but it keeps the hard-limit check on the safe side. Do not count tail memory twice in `conversation` estimates if it is also an anchor message.

**Test to add.**

`identity_memory_tail_counts_toward_the_context_limit`: fake provider with a full identity-memory brief; assert `context.usage.estimated_tokens` and `estimate_request_tokens` include the tail bytes and that a near-window request is rejected or compacted before the provider call.

---

### C6 — Unbounded session goal can brick a session; memory FTS query is unbounded

**Severity:** Medium · **Confidence:** Confirmed

**Evidence.**

`Store::insert_session` validates only `goal.trim().is_empty()` (`crates/vp-store/src/store/sessions.rs:100-103`). There is no length/byte cap in the store, the daemon `session.create` dispatch, or the CLI. The goal is placed into the v3 `session/context` section at `agent/context/prompts.rs:204`, whose per-section limit is `MAX_INSTRUCTION_SECTION_BYTES = 64 * 1024` (`instruction.rs:50,197-204`). `validate_candidates` returns `OversizeSection`, `materialize` fails, `build_prompt_sections` emits `context.instruction.failed` and returns an error (`agent/context/prompts.rs:624-643`). Because there is no goal-edit surface in the reviewed RPC/CLI, that session cannot be repaired; every run and `session.cache.plan` fails with a resource-limit error.

The goal also becomes part of the memory brief query: `memory_brief_with_department` joins `session.goal`, task title, department, write scope, and acceptance into `MemorySearchParams.query` (`crates/vp-core/src/engine/memory/records.rs:495-548`), and `fts_query` turns every whitespace-separated token into a quoted prefix term with no term-count or length cap (`crates/vp-store/src/store.rs:6275-6293`). A large goal produces a very large MATCH expression (or a very large LIKE fallback pattern), and the query runs before the instruction store has a chance to fail closed. The same unbounded text is stored in the `agent.queued` event payload by `service_run/start.rs:126-134` (the prompt is duplicated in a message and an event), so a huge `session.prompt` grows the event log as well.

**Impact.**

- A 64 KiB+ goal creates a permanently unusable session (`RESOURCE_LIMIT` on every run). Even a goal just under the section cap can make the prompt large enough to leave no room for conversation/output.
- A large goal/acceptance list can make the memory-brief FTS/LIKE query expensive or error-prone, consuming the blocking SQLite lock before the run fails.
- A large prompt is duplicated into the event log, increasing replay/export/transcript cost; the event log has no prompt-size guard in this path.

**Reproduction / check.**

`session.create` with a 70 KiB goal; `service.start` with a short prompt. Expect a `context.instruction.failed` event and a run error mentioning `session/context` and the 65,536-byte limit. A second check constructs a 70 KiB whitespace-rich goal and calls `engine.memory_brief`, then inspects the generated FTS query/LIKE pattern length (or times the call on a project with many memories).

**Suggested fix.**

Reject goals and prompts above a documented bound at admission (for example 16 KiB or 64 KiB – 1 for the goal, and a similar explicit prompt bound) with a typed `INVALID_ARGUMENT`, before creating a session or appending an event. Cap the memory-brief query to a bounded number of meaningful terms (for example 32) and a bounded total query length; use the goal's first N tokens or a summary extract rather than every token. Keep the full goal in the session record if the UI needs it, but do not expand it into every prompt/query surface.

**Test to add.**

`oversized_goal_is_rejected_at_creation` and `oversized_prompt_is_rejected_before_event_append`: assert no session/event is created on rejection. Add `memory_brief_query_is_term_bounded` with a goal containing thousands of tokens.

---

### C7 — `import_sources` dedupes only the newest 1,000 sources

**Severity:** Low · **Confidence:** Confirmed

**Evidence.**

`Engine::import_sources` builds `existing_uris` from `store.list_sources(&project_id, None, None, Some(1_000))` (`crates/vp-core/src/engine/sources/import.rs:101-112`). `Store::list_sources` clamps the limit to `1..=1_000` (`crates/vp-store/src/store/sources/ingest.rs:126`). A project with more than 1,000 sources has older URIs outside the set. `collect_import_files` can then return those files (up to `max_files`, default 100, capped at 500), and the `existing_uris.contains` check misses, so `ingest_source` creates a second source row with a new id, new chunks, and a new embedding generation.

**Impact.**

- Duplicate file sources and chunks appear in the project; retrieval returns near-duplicate hits, embeddings are paid twice, and the source list/health counts inflate. The duplicate also changes the `project/sources` brief and therefore the prompt cache epoch.
- The project can accumulate duplicates on every large import, and `import_sources` reports them as successfully imported rather than skipped.

**Reproduction / check.**

Create 1,001 indexed file sources (or a fixture project with >1,000 `sources` rows), then call `import_sources` with the directory containing one of the older files. Current behavior: a second source with the same URI is created. Expected: skipped.

**Suggested fix.**

Query existing URIs directly with a bounded, paged scan (for example `SELECT uri FROM sources WHERE project_id = ? AND uri IN (...)` for the candidate file list), or add a partial unique index on `(project_id, uri)` for `kind IN ('file','url')` and let the insert fail/skip. The candidate file list is already bounded to 500, so checking those exact URIs is cheap. Reconcile the `existing_uris` set from the scan rather than a fixed newest-1,000 window.

**Test to add.**

`import_skips_existing_files_beyond_the_first_thousand_sources`: seed 1,001 sources with URIs including the file to import; run `import_sources`; assert one source with that URI and no new chunks/embeddings.

---

### C8 — Reactivating a superseded legacy memory leaves `superseded_by` and can break the one-active-row key invariant

**Severity:** Low‑Medium · **Confidence:** Confirmed

**Evidence.**

`update_memory_with_event` accepts `status` values `active`, `stale`, `superseded`, and `archived` (`crates/vp-core/src/engine/memory/records.rs:601-605`), then in the store sets `memory.status = status`, clears/sets `stale_reason`, and updates the projection (`crates/vp-store/src/store/memory/records.rs:527-556`). It never clears `memory.superseded_by`. `MemoryItem` exposes `superseded_by` (`crates/vp-protocol/src/types.rs:5157-5160`), and `memory_verify` explicitly says "restore it with memory.update before verifying" (`engine/memory/records.rs:642-647`), so reactivation is an intended operator action.

The result is a row with `status = "active"` and `superseded_by = Some(old_replacement)`. `memory_health` counts it active and does not inspect the edge; the UI shows both an active status and a replacement; `active_memory_for_key_tx` (`crates/vp-store/src/store.rs:6086-6111`) can then choose it for a same-key supersede transaction while the old edge still points elsewhere.

More importantly, this breaks the documented lifecycle invariant that a project plus normalized kind/title has only one active row (`docs/ARCHITECTURE.md` §7.0.2). If the operator reactivates the old row while its replacement is still active, both rows are `active` with the same normalized key. `active_memory_for_key_tx` returns only the newest one, so the older active row stops being considered for future dedupe/supersede and remains visible to `memory.list`/`recall` indefinitely. A later `remember` with that key can supersede one of them and leave the other active.

**Impact.**

- Operator-facing lifecycle state is internally contradictory; a conflict/merge tool or a future "is this the current memory?" check may follow a stale edge.
- The one-active-row-per-key invariant is broken; recall can return two competing active claims for the same key, and future dedupe/supersede decisions are taken against only one of them.
- The history is still available in events, so this is a metadata/lifecycle integrity issue rather than lost content.

**Reproduction / check.**

Create two memories with the same kind/title, allow auto-supersede so the first becomes `superseded` with `superseded_by = second.id`; call `memory.update` on the first with `status = active`; read it back. Current: `status = active`, `superseded_by = Some(second.id)`. Expected: `superseded_by = None` (or the update is rejected until an explicit clear).

**Suggested fix.**

When `status` is set to `active` (and probably `stale`), clear `superseded_by` in the same projection/event; record the clear in `changed_fields`. If the caller wants to preserve the old replacement for audit, keep it in the `memory.superseded`/`memory.updated` event history, not on the live row. Add a check in `memory_verify` that a reactivated row has no live replacement edge.

**Test to add.**

`reactivating_a_superseded_memory_clears_the_replacement_edge`: supersede, reactivate, assert `superseded_by.is_none()`, the row is active, and the old replacement is still readable/audit-visible.

---

### C9 — Instruction materialization writes and blocks on the async runtime every run

**Severity:** Low · **Confidence:** Confirmed

**Evidence.**

`instruction::materialize` iterates every candidate and, for each one, unconditionally calls `store.put_instruction_blob(&source_text)` (`crates/vp-core/src/instruction.rs:725-734`) before reading the blob back. `put_instruction_blob` takes the global store mutex and executes `INSERT ... ON CONFLICT DO UPDATE ... WHERE text != excluded.text` (`crates/vp-store/src/store/cache.rs:44-71`). Even when the text is unchanged (the normal case), this is a write statement and a mutex acquisition; the `WHERE` only prevents the row update, not the transaction/lock. `materialize` is called synchronously from the async `build_prompt_sections` (`agent/context/prompts.rs:620-622`) without `spawn_blocking`, and it also calls `list_session_instructions` and per-section `get_instruction_blob`/`put_instruction_blob` on the same global connection.

This is not a correctness failure; the content-addressed store dedupes the row and the hash check protects the bytes. It is a hot-path reliability/latency issue: every run (and every cache warm/manual operation) holds the SQLite mutex for a sequence of small writes while a Tokio worker is blocked, which can stall other sessions' event/projection writes.

**Impact.**

- Under concurrent sessions, instruction materialization can add avoidable write-lock contention to the store, the same lock used by event appends and projections. The effect is small for one session but scales with run count and section count.
- The blocking call prevents other tasks on that Tokio worker from making progress while the SQLite operations complete.

**Reproduction / check.**

Instrument a test store to count `put_instruction_blob` executions; call `materialize` twice with identical candidates. Current: 2 × sections writes. Expected: zero writes on the second call. A timing test with a deliberately contended store can show the async worker stall, but a counter is sufficient for the regression.

**Suggested fix.**

Before writing, read the blob and verify it; if it exists and matches, skip the `put`. For candidate sections that are regenerated and unchanged, compare the desired hash to the existing row hash and skip the blob write. Move `materialize` to `tokio::task::spawn_blocking` (it only needs `&Store` and owned candidates) or make the store APIs async/batched. Batch the state replacement (already one transaction) and avoid per-section round trips where possible.

**Test to add.**

`second_materialization_performs_no_blob_writes`: add a test-only write counter to the store; assert the first materialization writes only the missing blobs and the second writes zero. Add an async smoke test that `build_prompt_sections` does not run SQLite writes on the current-thread runtime (or at least that the materialization is wrapped in `spawn_blocking`).

---

### Other observations (not ranked)

- **`source_brief` uses recency as catalog order.** `source_brief` lists the first eight indexed sources from `list_sources`, which orders by `updated_at DESC, id DESC` (`engine/sources/retrieval.rs:356-378`; `crates/vp-store/src/store/sources/ingest.rs:128-134`). A collection/label-only update or a reindex with unchanged title/kind can reorder or replace the top-eight list, moving the `project/sources` head section and the project cache epoch. That is avoidable cache churn (the source set did not change), even though the architecture permits source edits to move the project component. A deterministic catalog order (for example title/kind/id) would avoid it.
- **ANN recall is bounded by a non-distance window.** `source_ann_candidates` partitions by cluster and takes the first `per_cluster_limit` rows ordered by `source_id, chunk_no`, not by distance to the query (`crates/vp-store/src/store/sources/ann.rs:464-496`). A cluster larger than the per-cluster window can omit the query's nearest chunk before the in-memory cosine ranking. This is consistent with the documented approximate ANN/fallback contract, but the "deterministic rerank" label should not be read as exact recall; the exact scan remains the correctness fallback.

## 4. Test gaps

The existing suite is broad but does not assert the contracts that these findings break:

1. **Canonical continuity across runs.** `append_only_conversation.rs` runs twice and restarts a daemon, but only asserts `context.cache.append_only` and the prompt-cache previous/current hash pair. It never asserts that run 2's first `context.cache_break` is absent or that the canonical hash in `context.cache` equals the hash in `context.cache_break` for the same call.
2. **Manual prompt identity.** No test compares `context.checkpointed.prompt_prefix_hash` or `session.cache.plan.prefix_hash` with the real `context.prompt_prefix.prefix_hash` in a session with identity/source/skill/contract sections.
3. **Same-session warm attribution.** `cache_epoch_lifecycle` covers two sessions with no warm; `cache_first_agents` covers warm display but not the first ordinary call after a warm.
4. **Memory candidate scope diversity.** No test creates more than 128 agent-scope memories with a shared project/department memory, so the recency cut is untested.
5. **Tail accounting.** No test creates a full `agent/memory` tail and asserts it is included in `context.usage`/`estimate_request_tokens`.
6. **Oversized goal/prompt.** No creation-time or start-time size test; the `session/context` 64 KiB failure is only reachable after the session already exists.
7. **Large-project import dedupe.** No test with >1,000 sources.
8. **Reactivation lifecycle.** No test for `superseded -> active` clearing `superseded_by`.
9. **Materialization write count.** No test asserts idempotent materialization performs no writes or runs off the async worker.

The fake-provider tests in `cache_first_agents`/`cache_epoch_lifecycle` validate arithmetic and wire shape, not real provider cache behavior. The documentation already says the 99.84% target is a stretch goal and not a provider guarantee; the tests should keep that distinction and avoid asserting a "hit" that is only a mock's fixed `cache_read_tokens`.

## 5. Areas clean

- **Instruction ordering and hash stability.** Fixed ordinals, unique keys, parse-time rejection of unknown/duplicate keys, ordinal/tail validation on replay, pinned vs generation-scoped vs regenerated handling, post-write blob verification, and fail-closed oversize checks are consistent. The targeted `instruction::tests` suite (24 tests) passes.
- **Prompt bytes and cache telemetry separation.** Section refs/hashes, `section_chars`, `head_sections`/`tail_sections`, and epoch payloads expose no prompt text. Warm, compaction, and cache events are bounded and hash-only where required.
- **Compaction arithmetic.** `CompactionPolicy::derive` computes `hard_limit = window - output_reserve - framing` and `threshold = min(90% window, hard_limit)` with a non-zero floor; `safe_context_cut_with_tokens` respects both message and token tails, and the trim loop re-estimates after each cut. The summary/trim fallback and the compaction/checkpoint transaction are coherent.
- **Cache epoch identity.** `CacheEpochIdentity` is length-delimited, sorts section refs, verifies its hash against the event, and intentionally excludes session/task/memory bytes. The projection/rebuild and `finalized_epoch_hash` placeholder filtering are consistent with the documented "no fabricated epoch" rule.
- **Memory scope isolation.** `memory_visible_to`, the SQL viewer clause, `list_visible_agent_memories`, `visible_agent_memory_count`, and `ensure_memory_visible` agree for agent/department/project scope; cross-project viewers fail closed; imported rows are filtered from the brief and refused for content edits; evidence kind/reference/note bounds and content-hash checks are enforced on create/update/replay.
- **Source retrieval.** Project filtering is present in lexical, exact semantic, and ANN candidate SQL; source labels/collections are filters, not authority; the exact semantic path re-hashes the chunk text before scoring and skips stale vectors; ANN probe failures always carry a bounded fallback reason and fall back to the exact scan.
- **Embedding clients.** Endpoint validation rejects userinfo/query/fragment, redirects are disabled, keys come from environment variable names, error bodies are sanitized, responses are byte/count bounded, and vectors are never returned by status/health surfaces.
- **Derived index fencing.** Source/memory ANN write paths check the generation and return a conflict rather than mixing generations; invalid assignments are repaired only in maintenance, not in recall.

A store-level `session_instruction_state` `LIKE` wildcard scoping issue and a project-scoped event truncation issue are already reported in `05-store-sqlite.md`; they are not repeated here.

## 6. Assumptions and limits

- This was a code and test review at `d239dca`. I ran only `cargo test -p vp-core --lib instruction::tests`; I did not run the full workspace, the Swift app, or any live provider call. The baseline's reported full-suite result is taken from `AGENTS.md`, not re-verified.
- The fake-provider cache tests are not evidence of vendor cache behavior. C1/C3 reproductions can use the fake provider because they are about the runtime's own continuity/attribution fields, not the provider's actual cache.
- "Deterministic" in the source/memory ranking code means no model call and no random tie-break; freshness and recency still depend on the wall clock. I did not treat time-dependent ranking as a defect.
- The epoch is intentionally a core/project/tool/model identity, not a full-prefix identity. I treated the exclusion of session/context, contract, and run/task as design, but it means cross-session reuse numbers are an upper-bound/shared-prefix attribution, not a proof of a whole-prefix hit.
- Server-side goal/prompt limits may exist outside the reviewed files (UI/daemon policy), but no store/dispatch/CLI limit was found in the reviewed path; C6 is reported on the reachable core/RPC contract.
- I did not test a live restart or a real macOS sandbox; the findings are behavioral and reproducible with the fake-provider/service harness.

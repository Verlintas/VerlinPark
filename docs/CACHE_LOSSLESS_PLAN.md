# Cache-Lossless Plan: targeting 99.84% DeepSeek cache hits in real development

> **Status:** L1–L5 implemented and merged; live provider validation pending an
> environment key.
> **Created:** 2026-11-12
> **Base:** `6dc4443` (Worker return-prompt fix).
> **Merged:** `bc64d6f` (L1–L5, 3,363 Rust tests; Swift `xcodebuild test` passed).
> **Goal:** make long development sessions reuse an exact, append-only prompt
> prefix so DeepSeek reports ~99.84% cache-hit tokens in real use, without
> hiding token costs, mutating prior messages, or weakening any gate.
> **Budget:** one large round, target +36,000 net lines of production code and
> tests. No filler.

### Implementation results (2026-11-12)

| Scenario | Hit ratio | Notes |
|---|---:|---|
| Simulator, 200k context / 50 turns | 99.7081% | first 20k uncached context amortized over 50 turns |
| Simulator, 200k context / 100 turns | 99.8099% | longer session, same prefix shape |
| Simulator, 420k context / 100 turns | **99.9077%** | stretch target met; no filler, prefix-size guard |
| Live DeepSeek | not run | no `DEEPSEEK_API_KEY` / `VERLINPARK_LIVE_API_KEY` in this workspace |

The simulator uses 64-token blocks and deterministic synthetic tokens; it is a
regression tool, not provider-reported evidence. It rejects stable prefixes
above 400k to prevent a ratio manufactured solely by prefix size. `99.84%` is
still a stretch target that depends on context length, turn count, and the
provider's first-call behavior; cache reads remain billed.

**Delivered:** instruction store/layout v3 (L1), append-only canonical
serialization (L2), frozen tool bundles (L3), finalized epochs + late
compaction + cross-session reuse (L4), per-call telemetry + `vp cache-bench` +
GUI cache surfaces (L5). ADRs 0188–0192.

---

## 0. Why this plan exists

We measured two systems against the same DeepSeek API:

| System | Scope | Uncached input | Cache read | Hit ratio |
|---|---|---:|---:|---:|
| OpenCode2 2.2.7 | 114 sessions, aggregate | 79,748,915 | 8,331,745,024 | **99.052%** |
| OpenCode2 2.2.7 | best large session (`ses_f1755d…`) | 8,360,679 | 1,684,646,912 | **99.506%** |
| VerlinPark live team session | 20 calls | 32,342 miss (of 311,254) | 278,912 | **89.61%** |
| VerlinPark live team + follow-up | 23 calls | 35,876 miss (of 370,340) | 334,464 | **90.31%** |
| VerlinPark live simple session | 3 calls | 16,959 miss (of 49,983) | 33,024 | **66.07%** |

The user reports **99.84%** in actual OrcaADE + OpenCode2 + deepseek-flash
development, measured on `platform.deepseek.com`. The number is consistent with
the OpenCode2 database shape: very long, append-only sessions where the stable
prefix dominates the per-turn delta. This plan closes the gap between our
89–90% and that target.

### 0.1 OpenCode2 evidence (read-only inspection)

OpenCode2 stores per-message token usage in `opencode.db`:

```sql
-- session totals
SELECT id, title, tokens_input, tokens_cache_read, tokens_output,
       100.0*tokens_cache_read/(tokens_input+tokens_cache_read) AS hit_pct
FROM session WHERE tokens_cache_read > 100000
ORDER BY hit_pct DESC;

-- per-call shape (cache read grows, uncached input stays small)
SELECT json_extract(data,'$.role'),
       json_extract(data,'$.tokens.input'),
       json_extract(data,'$.tokens.cache.read'),
       json_extract(data,'$.tokens.output')
FROM message WHERE session_id='ses_f1755d…' ORDER BY time_created LIMIT 20;
```

Observed per-call shape (first 12 calls of the 99.506% session):

```text
call 1:  input 7087   cache 1664
call 2:  input 639    cache 8960
call 3:  input 921    cache 9856
call 4:  input 4130   cache 11008
call 5:  input 1198   cache 15488
call 6:  input 3098   cache 16896
call 7:  input 2513   cache 20608
call 8:  input 16683  cache 24832
call 9:  input 3994   cache 41728
call 10: input 290    cache 52608
call 11: input 1819   cache 55936
call 12: input 2316   cache 55936
```

The first call already has 1,664 cache-read tokens, proving **cross-session
prefix reuse of the stable instruction/tool block**. Later calls are almost
entirely previous prompt + a small new suffix.

OpenCode2's `instruction_state` for that session:

```json
{
  "core/codemode": "<sha256>",
  "core/skill-guidance": "<sha256>",
  "core/date": "<sha256>",
  "core/environment": "<sha256>"
}
```

The values are content hashes into `instruction_blob`; `initial_values` and
`current_values` are identical for the whole session. The stable `codemode` and
`skill-guidance` blobs (5,145 and 2,892 chars) are shared across sessions; only
`date` (17 chars) and `environment` (~377 chars) are session-specific and small.

### 0.2 Our cache killers (measured / code-verified)

1. **The current prompt is embedded in the system prefix.**
   `start_inner` passes the new prompt as `objective`; `role_system_prompt`
   writes `Objective: {objective}` into `Run context`, which becomes the
   `volatile` system section sent **before** the conversation. A follow-up
   prompt therefore changes a system message ahead of the entire history.
2. **Memory brief is rebuilt per run into the same volatile section.**
   A memory update changes the system prefix and can invalidate the history.
3. **The stable prefix is rebuilt from generated strings every run.**
   There is no content-addressed instruction store guaranteeing byte identity
   across sessions/calls. Any formatting or source/skill drift changes bytes.
4. **The first call of a fresh session gets 0 cache read.**
   Our live team session started at 0; OpenCode2 starts at 1,664 because its
   stable instruction block was used by earlier sessions.
5. **The default tool bundle is huge and role-wide.**
   Leader: 72 tools, ~12–13k tokens; Worker: 64 tools, ~11.8k tokens. The
   bundle is stable, but it is re-sent for every trivial task and dominates
   the uncached input when the conversation is short.
6. **Compaction is thresholded at 75% of the window.**
   With a 1M-context DeepSeek model, compacting at 75% is unnecessary and
   destroys the cache tail. The default should scale with the model window and
   remain lossless until safety requires otherwise.
7. **Telemetry does not prove append-only continuity.**
   We record `prefix_hash` but not per-message prefix continuity. We cannot
   distinguish "provider evicted the cache" from "we changed a prefix byte".
8. **Cross-session prefix reuse is not measured.**
   `session.cache` is session-scoped; we do not report whether a new session
   reused the project/role/tool prefix.

---

## 1. Metric definition (must match platform.deepseek.com)

Use provider-reported usage, never estimates, for the target:

- `input_tokens` = `usage.prompt_tokens` (total prompt tokens).
- `cache_read_tokens` = `usage.prompt_cache_hit_tokens` (DeepSeek) or
  `usage.prompt_tokens_details.cached_tokens` (OpenAI-compatible).
- `cache_miss_tokens` = `input_tokens - cache_read_tokens` (clamped ≥0).
- `cache_hit_ratio = cache_read_tokens / input_tokens`.
- `target = 99.84%` for real development sessions with a stable prefix and
  ≥200k-token context; `≥99.0%` aggregate over a working day; `≥95%` for
  short sessions (no gaming by padding a giant prefix).

OpenCode2's `tokens_input` column is uncached input, so its ratio is
`cache_read / (tokens_input + cache_read)`. Our `input_tokens` is total, so our
ratio is `cache_read / input_tokens`. Dashboards must label the denominator.

**Non-goal:** do not manipulate the metric by inserting filler tokens into the
prefix. The benchmark must use realistic development messages/tool outputs and
report both hit ratio and total cost.

---

## 2. Lossless invariants

These are hard invariants; tests must enforce them.

1. **Append-only conversation.** For every provider call after the first, the
   canonical message list must have the previous call's message list as an exact
   prefix. Prior messages are never edited, reordered, or re-serialized
   differently.
2. **Stable bytes.** The stable instruction prefix (core + project + tools) for
   a given `(model, project, role, profile, tool bundle, instruction hashes)` is
   byte-identical across sessions and calls. It is loaded from content-addressed
   blobs, not regenerated ad hoc.
3. **No current-prompt data in the system prefix.** The current user prompt is
   only a user message. The system/session block contains the session goal and
   agent identity, which are stable for the session.
4. **Memory is tail-only.** A memory brief or memory update must not change the
   stable prefix or any earlier message. It is appended at the tail (user
   message or tail system message) and is allowed to be a small cache miss.
5. **One cache epoch per stable change.** A cache epoch changes only when the
   model, tool bundle, project/source/skill sections, or compaction boundary
   changes. Session/prompt/memory changes do not change the epoch.
6. **Compaction is explicit and rare.** Compaction creates a new epoch, records
   the reset reason and the lost cache estimate, and never runs merely because
   a 1M-context session crossed 75%.
7. **No hidden model routing.** Model, prices, and cache policy remain
   user-configured.
8. **No gate changes.** Approval, Seatbelt, path jail, write scope, Verifier,
   merge, capacity, and budget gates are unchanged.

---

## 3. Architecture v3

### 3.1 Instruction store (content-addressed)

New tables:

```sql
instruction_blobs (
  hash TEXT PRIMARY KEY,        -- sha256(text)
  text TEXT NOT NULL,
  created_at TEXT NOT NULL
);

session_instruction_state (
  session_id TEXT NOT NULL,
  key TEXT NOT NULL,            -- e.g. core/charter, core/recipes
  hash TEXT NOT NULL,           -- FK to instruction_blobs
  ordinal INTEGER NOT NULL,     -- deterministic order
  tail INTEGER NOT NULL,        -- 0 = head system section, 1 = tail section
  updated_at TEXT NOT NULL,
  PRIMARY KEY (session_id, key)
);
```

Keys and order:

| ordinal | key | volatility | content |
|---:|---|---|---|
| 10 | `core/charter` | stable | role charter, security/output contract, conventions |
| 20 | `core/recipes` | stable | static tool recipes |
| 30 | `project/sources` | semi-stable | indexed source brief |
| 40 | `project/skills` | semi-stable | skill catalog |
| 50 | `session/context` | session-stable | agent name/title/parent, project root, worktree, branch, profile, scope, session goal |
| 60 | `run/task` | run-stable | task objective for Worker/Labour/Verifier/Scout; absent for Leader |
| 90 | `tail/memory` | per-run | memory brief; sent after history or as the first tail message |

`session/context` must use the **session goal**, not the current prompt.
`run/task` is the task objective for task-scoped agents (stable for the run).
For a Leader follow-up, neither the prompt nor memory is in the head prefix.

The engine materializes sections by hash, verifies `sha256(text) == hash`, and
records the ordered `(key, hash, tail)` list in `context.prompt_prefix`.
Unknown/missing hashes fail closed (rebuild from the deterministic generator,
store the new blob, and emit a diagnostic).

### 3.2 Provider layout

- **OpenAI-compatible / DeepSeek**
  - head sections in ordinal order as system messages, then the conversation,
    then `tail/memory` as the last system message (or a user message prefixed
    `[memory]` if the provider rejects mid-history system messages), then the
    newest user message. Test both; choose the provider-safe ordering that keeps
    the prior history prefix exact.
  - No `cache_control` (DeepSeek ignores it); rely on automatic prefix caching.
  - Do not send `reasoning_content` back; only assistant content/tool calls.
  - `stream_options.include_usage = true` (already).
- **Anthropic**
  - explicit breakpoints on stable sections/tools; tail memory is after the
    last breakpoint; no change to the 4-breakpoint cap.
- **Explicit cache warm**
  - `session.cache.warm` sends the head sections + tools with a 1-token tail;
    records usage/cost; never automatic except opt-in `warm_on_resume`.

### 3.3 Append-only continuity

- Canonical message serialization: sorted object keys where the provider does
  not care, stable tool-call ordering, stable tool-result ordering, no
  timestamps or generated IDs in content.
- Before each call, compute `message_prefix_hash` (hash of the canonical
  message list) and `previous_message_prefix_hash`. Continuity holds iff the
  previous canonical list is a prefix of the new one.
- On a break, record `cache_break` with a bounded reason:
  `system_changed`, `tool_bundle_changed`, `message_edited`, `message_reordered`,
  `compaction_reset`, `provider_cache_miss`, `first_call`, `unknown`.
- Never silently "repair" a break by rewriting history.

### 3.4 Tool bundle partition and freeze

- Bundle definitions per role/profile/task kind:
  - `leader` (orchestration, plan, read/edit/shell/git, memory, artifacts,
    questions, team, code intel; browser/computer only for coding profiles);
  - `coding` (file/edit/patch/search/shell/process/git/code/diagnostics/test
    cache/LSP/artifact/memory/coordinate; delegation only for Worker);
  - `research` (read/search/web/fetch/source/memory; no writes);
  - `verification` (read/search/git/shell/LSP/diagnostics/verdict; no writes);
  - `minimal` (read/write/shell/read_tool_output; for trivial tasks).
- The bundle is chosen once per session/run from the task kind and role; it is
  frozen for the run. Dynamic MCP tools are sorted and appended once.
- Canonical schema JSON: sorted by tool name, stable field order, no
  per-session data. `tools_hash` is the prefix identity.
- Target size: Leader ≤ 48 tools in the default path (down from 72); coding
  Worker ≤ 40 (down from 64); `minimal` ≤ 12. The exact set is test-gated.

### 3.5 Cache epochs and compaction

- `cache_epoch = sha256(model, tools_hash, core hashes, project hashes)`
  (volatile excluded).
- `context.prompt_prefix` records `cache_epoch`, `head_section_hashes`,
  `tail_section_hashes`, `message_prefix_hash`, `previous_message_prefix_hash`,
  and `append_only`.
- Context limits:
  - Derive the default window from the configured model window (DeepSeek
    `deepseek-flash` = 1,048,576). Default compaction threshold becomes
    `min(0.90 * window, window - reserve)`; `VERLINPARK_COMPACT_AFTER_TOKENS`
    remains an explicit override.
  - Do not compact solely because the session is old; compact only when the
    next request would exceed the threshold.
  - Compaction records `cache_epoch_before/after`, `reset_reason=compaction`,
    `estimated_lost_cache_read_usd`, and keeps the stable instruction prefix.
  - For DeepSeek, prefer no compaction until 90% of 1M; the 1M window is the
    lossless budget.
- Cross-session reuse:
  - `prompt_prefixes` table stores `cache_epoch`, first/last seen, model,
    tool bundle, core/project hashes, and observed hit/miss totals.
  - `session.cache` reports `prefix_reused_from_sessions` and
    `cross_session_hit_tokens`.
  - New sessions with the same epoch reuse the same bytes and should hit on
    their first call if the provider cache is warm.

### 3.6 Telemetry and miss attribution

Per call:

```json
{
  "cache_epoch": "…",
  "head_section_hashes": {"core/charter":"…","core/recipes":"…","project/sources":"…"},
  "tail_section_hashes": {"tail/memory":"…"},
  "message_prefix_hash": "…",
  "previous_message_prefix_hash": "…",
  "append_only": true,
  "input_tokens": 19739,
  "cache_read_tokens": 19584,
  "cache_miss_tokens": 155,
  "cache_hit_ratio": 0.9922,
  "theoretical_max_hit_ratio": 0.995,
  "miss_reason": "tail_delta | provider_cache_miss | prefix_changed | first_call | compaction_reset",
  "provider": "deepseek",
  "model": "deepseek-flash"
}
```

Session aggregation: hit ratio, total miss, cost, per-role, p50/p95 hit ratio,
cache epoch timeline, top miss reasons, cross-session reuse count, and
"lost cache USD" from compaction.

---

## 4. Workstreams

### L1 — Instruction store and prompt layout v3

**Files:** `vp-core/src/prompt_cache.rs` (extend), new
`vp-core/src/instruction.rs`, `vp-core/src/agent.rs` prompt construction,
`vp-store/src/store.rs` + tests, protocol section types, daemon/CLI surfaces,
ADR-0188.

**Deliverables:**
- `instruction_blobs` / `session_instruction_state` tables, migration, replay,
  `db-rebuild`.
- Content-addressed materialization with hash verification; deterministic
  fallback generation that stores the blob.
- Session-stable `session/context` (session goal, not prompt); `run/task` for
  task agents; `tail/memory` for memory.
- `build_prompt_sections` becomes a thin assembler over the instruction store.
- `context.prompt_prefix` records ordered keys/hashes and `prompt_layout=3`.
- API `session.instructions` / CLI `vp instructions` to inspect the ordered
  sections and hashes without printing full prompt text.
- Tests: byte identity across two calls/sessions, prompt change only in user
  message, memory change only in tail, hash verification, replay.

### L2 — Append-only conversation, canonical serialization, provider parity

**Files:** `vp-providers/src/openai.rs`, `anthropic.rs`, `vp-core/src/agent.rs`
message assembly, `prompt_cache.rs`, tests, ADR-0189.

**Deliverables:**
- Canonical message list serializer with stable field ordering and no volatile
  IDs/timestamps in content.
- `message_prefix_hash` / `previous_message_prefix_hash` / `append_only` on
  every call; `cache_break` events with bounded reasons.
- Provider tests asserting the HTTP body's prior messages are byte-identical
  across turns (wiremock).
- DeepSeek: reasoning content excluded from replayed messages; usage fields
  parsed; no `cache_control`.
- If a mid-history `tail/memory` system message is rejected, fall back to a
  user message with a stable prefix and record the provider capability result.
- Tests: append-only, reorder/edit detection, compaction reset, provider parity.

### L3 — Tool bundle partition and freeze

**Files:** `vp-core/src/agent.rs` tool schema construction, `tools.rs`,
`tooling/mod.rs`, protocol/CLI, tests, ADR-0190.

**Deliverables:**
- `tool_bundle_for(role, profile, task_kind)` with named bundles: leader,
  coding, research, verification, minimal.
- Bundle selection from plan task `kind`; frozen for the run; dynamic MCP tools
  sorted/frozen.
- Canonical schema JSON; `tools_hash` stable across sessions for the same
  bundle.
- Default bundle size reductions; explicit tests for the exact sets and role
  permissions.
- `minimal` bundle for trivial tasks (read/write/shell/read_tool_output) so a
  one-file task does not pay 12k tokens of unused tool schemas.
- Backward-compatible fallback to the full bundle when no task kind is known.

### L4 — Cache epochs, compaction policy, cross-session reuse

**Files:** `vp-core/src/agent.rs` compaction, `prompt_cache.rs`,
`cache_audit.rs`, store, protocol/CLI, tests, ADR-0191.

**Deliverables:**
- `cache_epoch` identity and `prompt_prefixes` table; cross-session reuse
  telemetry.
- Default compaction threshold derived from the model window (90% of 1M for
  DeepSeek); explicit override retained; compaction records epoch/reset/lost
  cache.
- Explicit warm path and `warm_on_resume` remain opt-in.
- Miss attribution distinguishes provider eviction from our prefix changes.
- Tests: epoch stability, compaction reset/lost estimate, cross-session reuse,
  threshold math, warm cost, no hidden provider call.

### L5 — Telemetry, live benchmark, and GUI cache panel

**Files:** `prompt_cache.rs`, `cache_audit.rs`, protocol, daemon, CLI, Swift
Efficiency/Stage, `vp-bench` or a new cache benchmark, docs, ADR-0192.

**Deliverables:**
- Per-call cache table fields described in §3.6; session aggregation;
  `session.cache.calls` bounded list; theoretical max; hit ratio.
- `vp cache-bench` / a deterministic fake provider that simulates prefix cache
  and a live DeepSeek mode (explicit env opt-in) that runs a controlled
  multi-turn development session and reports provider hit tokens.
- OpenCode2 baseline extraction doc/script with the SQL queries in §0.1; no
  user content, only aggregate metrics.
- GUI: Efficiency cache card shows hit ratio, miss tokens, epoch, append-only,
  top miss reasons, cross-session reuse; Stage cache pill; a per-call table.
- Tests: metric math, theoretical max, miss reasons, benchmark determinism,
  live gating, protocol/Swift decode.

### L6 — Optional: cache-loss audit and repair guidance

If time allows: a `cache-audit` finding that flags `system_changed` on a live
session, shows the first differing section key, and suggests the stable
alternative. No auto-repair.

---

## 5. Benchmark and acceptance

### 5.1 Unit / integration

- Byte identity: two calls with the same stable inputs produce identical
  canonical system/tool bytes; a different prompt changes only the user
  message.
- Append-only: prior messages are a prefix; any edit/reorder records a break.
- Instructions: hash verification; missing blob fallback; replay.
- Tool bundles: exact sets, stable hash, minimal bundle for trivial tasks.
- Epoch: stable across prompt/memory changes; changes on model/tool/project
  changes; compaction resets once.
- Metrics: ratio math, theoretical max, miss reasons, no division by zero.

### 5.2 Deterministic cache simulator

A fake provider that computes cache hits from the longest exact prefix of the
previous request (block size 64) and reports DeepSeek-style usage. Run a
50-turn synthetic development session with realistic deltas; assert hit ratio
≥99.5% for a ≥200k prefix and no `prefix_changed` breaks.

### 5.3 Live DeepSeek benchmark (explicit opt-in)

- Use `deepseek-flash`, `https://api.deepseek.com`, 1M window.
- Scenario: a stable instruction block + 60 turns of small user messages and
  tool outputs; no compaction; measure provider `prompt_cache_hit_tokens`.
- Gate: ≥99.8% for the long-context scenario, ≥99.0% aggregate across the
  existing live sessions. Record the exact prompts/tool outputs in a temp dir,
  never in the repo.
- The benchmark must print both hit ratio and total cost; a huge prefix that
  inflates the ratio is rejected by the scenario size check.

### 5.4 Acceptance

- OpenCode2 baseline documented (aggregate 99.052%, best 99.506%).
- Our live team session re-run after L1–L4 reaches ≥99.0% and the long-context
  benchmark reaches ≥99.8%; 99.84% is the stretch target, reported honestly if
  the provider/cache state differs.
- `cargo fmt/clippy/test`, `just lint`, `xcodebuild test`, `just bundle` pass.
- No gate weakening; no hidden model routing; no automatic warm by default.

---

## 6. Migration / rollout / rollback

1. Add tables and additive fields; old sessions keep the legacy layout.
2. Enable v3 instruction layout for new sessions; keep a project flag
   `prompt_layout=2|3` for rollback.
3. Backfill instruction blobs lazily on the first run; missing/corrupt blobs
   rebuild deterministically and emit `context.instruction.rebuilt`.
4. Cache telemetry remains additive; old events decode with defaults.
5. If a provider rejects tail system messages, fall back to a user-memory
   message and record `provider_tail_system=false`; no data loss.
6. Rollback: flip the project flag to v2; existing sessions continue with
   their stored layout; no destructive migration.

---

## 7. Risks

- **Model behavior changes** when the volatile objective moves out of the system
  prompt. Mitigation: keep the session goal in `session/context` and the task
  objective in `run/task`; add prompt tests and a live smoke.
- **Provider rejects mid-history system messages.** Mitigation: capability
  probe/fallback to a user message.
- **Long context cost** on the first miss. Mitigation: stable cross-session
  prefix, optional warm, and cost telemetry; do not hide the first-call cost.
- **Compaction still resets cache.** Mitigation: 90% threshold for 1M models,
  explicit reset accounting; do not pretend compaction is cache-free.
- **Tool bundle reduction removes a needed tool.** Mitigation: task-kind
  mapping, `tool_search`/`tool_help`, fallback to the full bundle, and tests.
- **Cross-session cache is provider-managed.** We can prove reuse only when the
  provider reports hit tokens; we must not claim guaranteed hits.

---

## 8. Non-goals

- No fake usage or metric gaming.
- No hidden model routing or provider-specific secret behavior.
- No change to approval/Seatbelt/path/Verifier/merge/capacity/budget gates.
- No automatic cache warm by default.
- No provider TTL inference.
- No cross-host cache sharing.
- No change to the DeepSeek model selection without user configuration.

---

## 9. Implementation order

**L1 → L3 → L2 → L4 → L5** (L6 optional).

L1 establishes the stable instruction bytes; L3 freezes/reduces the tool prefix;
L2 proves append-only continuity; L4 owns epochs/compaction/reuse; L5 measures
and surfaces it.

## 10. Appendix: source references

- `crates/vp-core/src/agent.rs`: `role_system_prompt` (~23090), `build_prompt_sections` (~23235), request assembly (~14599), compaction (~13730).
- `crates/vp-providers/src/openai.rs`: system section ordering (~90), message conversion (~287), usage parsing (`vp-providers/src/lib.rs` ~330).
- `crates/vp-core/src/prompt_cache.rs`: `PromptSections`, `provider_sections`, hashes.
- `crates/vp-core/src/cache_audit.rs`: current audit findings.
- OpenCode2 database (read-only): `~/.local/share/opencode/opencode.db`, `session`, `message`, `instruction_state`, `instruction_blob`.
- Live VerlinPark test data: `vp-live-deepseek` temp data dir (not committed).

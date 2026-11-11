# ADR-0160: Compaction token budget, structured plan state, and prepared summaries

- **Status:** Accepted
- **Date:** 2026-11-11
- **Owners:** Runtime, Protocol, macOS
- **Related:** ADR-0153 (cache telemetry), ADR-0155 (durable run journal),
  ADR-0159 (tool-output store), AGENTS.md invariants 1, 2, 3, 7, and 8

## Context

The context manager already compacted long conversations. It estimated the
active request, compared the estimate with a model-window-derived threshold,
summarized the prefix with a provider call, and retained a fixed number of
recent messages. If summarization failed, it trimmed older messages at safe
user/assistant boundaries and continued the run.

That was a good first step, but it had three structural gaps.

1. **Message count is not a token budget.** Eight messages can be eight short
   notes or eight 40 KB tool results. A fixed tail could still be most of the
   model window, or could be far smaller than necessary.
2. **The summary had no authoritative task state.** The model was asked to
   preserve goals, decisions, and open tasks, but a free-text summary can omit
   the exact active task, its acceptance criteria, its write scope, or the plan
   revision. Recovery then depended on the model having noticed the important
   row. The stored TeamPlan is deterministic and already the scheduler's source
   of truth; it should be represented without another inference step.
3. **Every threshold crossing paid full summarization latency.** A summary
   prepared slightly before the threshold could make the swap faster, but only
   if it is bounded, correctly invalidated, budgeted exactly like a normal
   summary, and never allowed to become a second agent run.

C1 is also coupled to tool-output paging (ADR-0159). Paging reduces the size of
individual tool results, but a long run still accumulates many messages. The
compaction budget must be correct even when previews are small, handles are
unresolved, or a tool result is exactly at a shaping boundary.

The design has to preserve the existing invariants:

- deterministic Rust owns threshold and retention decisions;
- no hidden model routing;
- compaction is lossy and is never durable task state;
- the TeamPlan remains authoritative even if the model summary fails;
- a manual compaction still works while the session is idle;
- the daemon never creates a second agent run or bypasses the session budget
  just to prepare a summary.

## Decision

### 1. Two windows: message count and token budget

`AgentConfig` gains:

- `compact_keep_messages` (existing, default 8, clamped 2–100);
- `compact_keep_tokens` (new, default 15,000, bounded 0–2,000,000).

The environment overrides are `VERLINPARK_COMPACT_KEEP_MESSAGES` and
`VERLINPARK_COMPACT_KEEP_TOKENS`. `0` disables the token constraint and leaves
message-count selection in charge; the default is intentionally finite so a
tail of short messages does not accidentally retain the whole conversation.

The cut is a message index, never a byte offset. `safe_context_cut_with_tokens`
works as follows:

1. `len <= floor + 2` means there is no useful cut; return `None`.
2. `count_cut = max(floor + 1, len - keep_messages)`.
3. If the token budget is zero, `token_cut = floor + 1`. Otherwise walk backward
   from the newest message, summing `estimate_message_tokens`; stop before
   adding a message that would exceed `keep_tokens`; `token_cut` is the first
   retained index.
4. `proposed = max(count_cut, token_cut)`. The larger index retains fewer
   messages and therefore satisfies both constraints. A single oversized
   message is still retained rather than cut in half; the loop always leaves at
   least one message after the floor.
5. Move forward from `proposed` to the first user or assistant message. Tool
   messages can be retained only as part of a valid user/assistant tail; the
   cut is never inside a tool-call message or between a tool result and its
   assistant caller.

`estimate_message_tokens` includes the message content and the serialized
tool-call payload. It is the same heuristic used by the existing active-context
estimate, not a tokenizer claim. Provider-measured calibration continues to
scale the whole estimate upward-only.

Properties:

- deterministic for a fixed conversation;
- never splits a UTF-8 scalar because it only chooses message indices;
- never splits a message in half;
- respects both the count and token constraints when possible;
- the retained tail is at most the configured message count and (unless one
  message alone exceeds the budget) at most the configured token budget.

The token budget applies to the retained tail, not to the summary block. The
summary block is separately included in the active estimate and is checked
against the hard limit.

### 2. Threshold math is unchanged, but prepared summaries add a pre-threshold

`context_limits` still derives:

```text
hard_limit = model_window - output_reserve - safety
threshold  = min(compact_after_tokens, hard_limit)       if configured
           = hard_limit * 3 / 4                          otherwise
```

The hard limit is the whole request (system prompt, tool schemas, messages, and
output reserve). `compact_prepare_ratio` (default 0.6, env
`VERLINPARK_COMPACT_PREPARE_RATIO`, clamped 0.3–0.95) defines:

```text
prepare_threshold = round(threshold * prepare_ratio)
```

Prepare only when:

- the strategy is `Summarize`;
- the request is not already at the compaction threshold;
- `prepare_threshold > 0`;
- the estimate is at or above `prepare_threshold`;
- a safe token-aware cut exists;
- no valid prepared summary already exists;
- the exact prepare key has not already failed in this run.

Preparation is a normal provider summary call with the same system prompt,
temperature, max output, idle timeout, cancellation handling, and budget
reservation as a synchronous summary. It is not a second agent run, not a
background thread with its own budget, and not hidden routing. The call is made
from the run loop at the next provider boundary, so the run is already at a
safe turn boundary and the conversation cannot mutate concurrently.

If the reservation cannot be granted, preparation is skipped. If the provider
fails, the failure key is recorded for the run and a
`context.compaction_prepare_failed` event is emitted; the run continues. The
failure does not emit `context.compact_failed`, because no compaction happened
and the existing manual-compaction error path would misinterpret it.

The cost of a prepared summary is real and is recorded in the usage ledger under
role `compaction` immediately. If the summary is never reused (for example, the
run ends first), the cost is still charged. This is honest: the call happened.
It also means preparation must not be repeated for every message append.

### 3. Prepared-summary key and cursor

A prepared summary is keyed by:

```text
provider
model
stable prefix hash
plan revision
cut index
message-prefix cursor hash
```

`stable prefix hash` is the existing `context.prompt_prefix` fingerprint of the
model, stable system prompt, and frozen tool bundle. A model switch, instruction
change, or tool-schema change invalidates the snapshot.

`plan revision` is `plan_revision_count(session)` (the number of submitted or
revised TeamPlan events). A plan edit invalidates the snapshot even if the
message prefix is byte-identical, because the structured state block would be
stale.

`message-prefix cursor hash` is a SHA-256 over the roles, content lengths,
contents, tool calls, and tool-call ids of every message before the cut. This is
stronger than a message count: a steering insertion, a message edit, or a
reconstructed conversation with the same length but different text invalidates
the entry.

The entry stores the cut index, the model summary text, and the provider usage.
It does not store a frozen structured-state block: at the swap the runtime
re-renders the block from the current plan and scheduler rows (the plan revision
is already part of the key), so a completed task that settled between prepare
and threshold is reflected without invalidating the paid-for model summary. The
summary text is free text; the structured state is authoritative.

### 4. Reuse semantics

A prepared summary covers a fixed prefix. New messages appended after the cut
do not invalidate it; they remain in the retained tail. At the threshold:

1. Recompute the ideal cut from the current conversation.
2. If a prepared entry exists and every key component still matches, use the
   prepared cut instead of the ideal cut. The summary covers the prefix it was
   prepared for; newer messages are retained.
3. If the key does not match, discard the entry and fall back to a synchronous
   summary. `fallback_to_sync` is true in the event.
4. If no prepared entry exists, take the synchronous path.
5. If the synchronous provider call fails, keep the old behavior: emit
   `context.compact_failed`, then perform the rolling-window trim fallback so
   the run can continue.

Reusing a prepared cut can retain more messages than the token-aware ideal
would. This is deliberate and safe: those messages are newer than the prepared
prefix and were never summarized away. If the resulting active context is still
above the hard limit, the normal rolling trim runs afterward, using the same
message-boundary rules. A prepared summary never causes a message to be dropped
without either being in the summary or remaining in the tail.

A valid prepared entry is not "re-prepared" on every turn. If the existing key
is still valid, the preparation branch is skipped. After a reuse the entry is
gone; the next pre-threshold window can prepare another one.

The cache is per `run_loop_body` invocation, not persisted. It is dropped on:

- run end (normal, failed, cancelled, or budget stop);
- model change;
- provider change;
- stable-prefix change;
- plan revision change;
- prefix cursor change;
- cancellation while the summary call is in flight;
- prepare failure (the failed key is remembered only for this run).

It does not survive daemon restart, session resume, or a fork. A fork copies
the conversation summary, not the prepared cache.

### 5. Worked preparation and reuse timeline

Assume a 128k window, a 4k output reserve, a 1,024-byte safety reserve, and no
explicit `compact_after_tokens`. The hard limit is about 123k and the default
threshold is about 92k. The prepare threshold at ratio 0.6 is about 55k.

```text
turn 1  estimate 40k   no prepare
turn 2  estimate 58k   prepare summary for prefix P (one provider call)
turn 3  estimate 70k   prepared key still valid; no second prepare
turn 4  estimate 95k   threshold crossed; reuse P; drain P; keep newer tail
turn 5  estimate 60k   no prepare (below prepare threshold)
turn 6  estimate 80k   prepare a new prefix Q
...
```

At turn 4, the ideal cut for 95k would usually be later than the prepared cut.
The runtime intentionally drains only to P. Messages appended in turns 3 and 4
remain in the tail. This is why a prepared summary cannot silently discard
newer work: the newer messages were never part of P.

If the operator edits the TeamPlan between turn 2 and turn 4, the plan revision
in the key changes. The prepared entry is discarded, `fallback_to_sync` is true,
and the synchronous summary uses the new state block. The old prepared summary
is never applied to a new plan revision.

If the model is switched between turn 2 and turn 4, the key model changes and
the same fallback occurs. If a new tool schema is advertised, the stable prefix
hash changes. If a steering message is inserted at the head of the
conversation, the prefix cursor hash changes. All four invalidations are
covered by tests.

### 6. Selection math examples

Message-count-only selection (`keep_tokens = 0`) is the legacy behavior:

```text
len = 20, keep_messages = 8, floor = 0
count_cut = 12
token_cut = 1 (disabled)
proposed = 12
cut = first user/assistant at or after 12
```

Token-aware selection can cut later:

```text
len = 20, keep_messages = 8, keep_tokens = 500
messages 12..19 each cost 40 tokens
count_cut = 12
token_cut = 12 (tail is exactly 320 tokens)
proposed = 12
```

Token-aware selection can also cut earlier when the tail is expensive:

```text
len = 20, keep_messages = 8, keep_tokens = 100
messages 18 and 19 cost 60 tokens each (120 > 100)
token_cut = 18
count_cut = 12
proposed = 18
```

The cut is moved forward to a user/assistant boundary. The retained tail is
never split at a byte offset and never starts inside a tool-result exchange.
When a single message costs more than `keep_tokens`, it remains because there is
no smaller safe message to retain; the hard-limit trim and the provider's own
window are the final backstops.

### 7. Structured state block

The structured state is derived from the stored TeamPlan and scheduler rows:

```text
[structured task state - authoritative]
Goal: <plan.goal or session.goal>
Plan revision: <count>
Active task: <id> - <title>
Department: <department>
Acceptance:
- ...
Write scope:
- ...
Recently completed tasks: <ids>
Generated at: <RFC3339>
[/structured task state]
```

Rules:

- `goal` prefers the plan goal; a session without a plan uses the session goal.
- active task is the explicit run task, else the first scheduled task in
  running/claimed/paused/cancelling/pausing state, else the first plan task not
  completed/failed/blocked/cancelled/skipped.
- acceptance and write scope come from the plan task, in plan order.
- recent completed tasks come from scheduler rows with status `completed`,
  newest `finished_at` first, tie-broken by task id, bounded to 8.
- all embedded newlines are flattened to spaces so the block's line structure is
  deterministic.
- the block is rendered by Rust, not by the model.

When a summary is produced, the block is prepended to the model summary inside
the existing `Earlier conversation summary` message. When a summary already
exists at the index-zero position, `refresh_structured_state` replaces the old
block in place, preserving the surrounding free text. This means a plan edit
made while the conversation is already compacted is reflected at the next
compaction even if the model summary is not re-run.

If summarization fails and the rolling trim runs, the state block is inserted as
the index-zero user message (or refreshed in the existing summary message)
before messages are dropped. The block is therefore preserved even on the
fallback path. A tiny synthetic hard limit that cannot fit the block is the one
exception: inserting a block larger than half the window would make the request
worse, so the runtime skips the block and performs a pure trim. Real model
windows are far above the block size; the condition is documented and covered
by the calibration/tiny-window tests.

The block is not a second source of task state in SQLite. It is a rendering of
the plan and scheduler projection. The plan remains authoritative; a future
`team_plan.task_edit` is reflected by the next render or compaction.

### 8. Manual compaction

`session.compact` / `vp compact` keeps its existing contract:

- only an idle/failed/cancelled/draft session may compact;
- the manual operation holds the run registry so a prompt cannot start in the
  middle;
- it is cancellable but not pausable;
- it passes `force = true`, so it runs even below the automatic threshold;
- it uses a fresh `CompactionRuntime`, so it never reuses a prepared summary
  from another run;
- it records the same `context.compacted` and `context.compaction` events and
  the same usage ledger entries as an automatic compaction;
- it releases the session in a `defer`-equivalent path even if the provider
  call fails.

The only behavioral addition is that a manual compaction on a session with a
stored plan now includes the structured state block. A manual compaction that
cannot produce a summary still trims to half the hard limit when possible, as
before, and now also carries the state block when it fits.

Manual compaction is lossy and is not a checkpoint. Operators who need to
restore conversations should use `session.checkpoint` and `session.fork`; those
snapshot the active conversation and worktree but intentionally do not copy the
tool-output store or the prepared-summary cache.

### 9. Metrics and compatibility

Every successful compaction still emits `context.compacted` with the existing
fields:

- `mode` (`summary` or `trim`);
- `through_message_id`;
- `summary`;
- `summarized_messages`, `dropped_messages`, `kept_messages`;
- `estimated_tokens_before`, `estimated_tokens_after`;
- `threshold_tokens`, `hard_limit_tokens`;
- `calibration`, `input_tokens`, `output_tokens`, `estimated`.

It also carries additive fields:

- `latency_ms`: wall-clock duration from the compaction entry to the event,
  including preparation reuse or a synchronous call;
- `background_reuse`: true when the prepared snapshot was consumed;
- `fallback_to_sync`: true when a prepared entry existed but was invalid, so the
  synchronous path ran;
- `compaction_count`: cumulative successful compaction events for the session,
  including this one;
- `structured_state`: the authoritative block as JSON;
- `background_prepared`: whether a new prepared entry remains after this
  compaction.

A second event, `context.compaction`, carries the operational metrics without
the summary text. It exists so a metrics/telemetry consumer can read latency and
token deltas without duplicating conversation content. `context.compacted`
remains the audit/compatibility event.

`ContextCompactionSummary` gains serde-default fields for the same metrics.
`SessionContextStatus` exposes the latest values as top-level fields:
`compaction_count`, `last_compaction_latency_ms`,
`compaction_background_reuse`, and `compaction_fallback_to_sync`. Older clients
decode the summary without these fields; older daemons leave the status fields
at zero/false. The Swift efficiency card and Stage context popover read these
fields additively and do not infer them from event text.

`context.compaction_prepared` and `context.compaction_prepare_failed` are
operational events. They are not replay projections, are not used to rebuild
state, and are not required for recovery.

### 10. Lossiness and replay boundaries

Compaction is explicitly lossy:

- original messages remain in SQLite for search, audit, and export;
- the active conversation loses messages that are represented only by the
  summary and structured block;
- the summary is model-generated free text and may omit details;
- the structured block is deterministic but bounded to the plan fields listed
  above, not the full plan body;
- a prepared summary is an optimization, not a durable artifact;
- `context.compacted` is an event, but the summary text is not a replay
  projection that repair will reconstruct from a source of truth.

`session.replay` continues to validate session, usage, messages, plan, todos,
queue, approvals, memory, questions, coordination, and run journal projections.
It does not attempt to prove that a summary equals a re-summarization of stored
messages. Re-running the model is not deterministic and would be a hidden
provider call.

A fork copies the compaction summary and active tail as conversation messages
(the existing behavior), not the prepared cache or the tool-output handles. A
checkpoint includes the conversation snapshot and worktree commit, not a
compaction history.

### 11. Failure modes

| Failure | Behavior |
|---|---|
| Preparation provider error | `context.compaction_prepare_failed`; failed key remembered for the run; continue |
| Preparation budget reservation denied | Skip preparation silently; normal threshold path later |
| Cancellation during preparation | Summary call observes cancellation; no entry is stored |
| Model change after preparation | Key mismatch; discard; `fallback_to_sync`; synchronous summary |
| Stable prefix / tool bundle change | Same as model change |
| Plan revision change | Same; the structured block would be stale |
| Message prefix mutation | Cursor hash mismatch; discard; synchronous summary |
| New messages after prepared cut | Key stays valid; reuse at prepared cut; newer messages remain in tail |
| Prepared entry stale and sync fails | Existing `context.compact_failed`; rolling trim with structured state |
| Synchronous provider returns empty text | Treated as no summary; rolling trim; state block preserved |
| Summary reservation denied | Skip summary; rolling trim; state block preserved when it fits |
| State block larger than half a tiny hard limit | Skip state insertion; pure trim; no false claim of preservation |
| Manual compaction on active session | Refused before provider call |
| Manual compaction provider failure | Error surfaced, session released, no half-state |
| Budget exhausted | Compaction returns without a provider call; run's normal budget gate handles it |
| Daemon crash mid-summary | Durable run journal marks the run; no prepared cache survives; resume uses events |

### 11.1 Operator runbook

**Context still grows after compaction.** Inspect `session.context`: the
`usage.estimated_tokens`, `threshold_tokens`, `hard_limit_tokens`, and
`last_compaction` fields explain which limit applies. A large `kept_messages`
tail, a large structured plan block, or a very large system/tool preamble can
keep the estimate high. Lower `compact_keep_messages` or
`VERLINPARK_COMPACT_KEEP_TOKENS`, reduce the tool bundle, or raise the model
window. Do not assume compaction failed just because the estimate is above the
threshold; it may be under the hard limit and therefore not require another
drop.

**Compaction is slow.** `session.context.last_compaction_latency_ms` and
`context.compaction` show the last latency. A high value with
`background_reuse=false` means the synchronous summarizer ran. If preparation
frequently falls back, check for a changing plan revision, model switch, tool
bundle change, or unstable prefix. A pre-threshold prepare is one bounded call;
it does not run in parallel with the agent.

**A summary lost a detail.** The structured block is authoritative for the
fields it lists. Recover older messages from SQLite/search/transcript; compaction
does not delete the audit history. Use `session.checkpoint` before a risky
long run if exact conversational rollback is required. A checkpoint is not a
compaction transcript, but it preserves the active conversation at that point.

**Manual compaction fails.** The session must be idle/failed/cancelled/draft,
the budget must permit a provider call, and the operation must not be paused.
Read the surfaced error; a compact failure event remains in the audit stream.
The run is not left active and the session is released.

**A prepared summary appears in the ledger but was not used.** That is honest:
the call happened and was charged. A later plan/model/prefix change invalidated
it, or the run ended first. Prepared summaries are an optimization, not a
guarantee.

### 11.2 Alternatives considered

**Keep messages-only compaction with a larger window.** Rejected: message count
does not bound tokens, and a single paged tool preview can be much larger than a
short note. The token window is the missing constraint.

**Cut at a byte offset to hit the token budget exactly.** Rejected: it can split
a message, a tool-call exchange, or a UTF-8 scalar, and it cannot be represented
as a message cursor. The cut is always a message index.

**Ask the model to emit the structured task state.** Rejected: it is
non-deterministic, may omit the active task, and would fail exactly when
summarization fails. The plan and scheduler are deterministic Rust state.

**Persist prepared summaries in SQLite.** Rejected for this round: it would need
invalidation across daemon restarts, provider/model changes, plan revisions, and
message edits, plus a second retention/GC surface. A per-run cache gives the
latency win without claiming durability.

**Run preparation on a background Tokio task.** Rejected: it would race
conversation mutation, complicate cancellation and budget reservations, and
could look like a second agent run. Preparation runs synchronously at a turn
boundary and only the reuse is deferred.

**Prepare at every turn below the threshold.** Rejected: it would multiply
provider calls and cost. One valid prepared key suppresses further preparation
until the key changes or the entry is consumed.

**Use the prepared cut as the only cut.** Rejected: it can leave a larger tail
than the token budget intends. The runtime uses the prepared cut to preserve
the paid-for summary, then still applies the normal hard-limit trim if needed.
This is a deliberate efficiency trade-off, not a correctness exemption.

**Make compaction lossless by never dropping a message.** Impossible under a
hard model window. Checkpoints and original SQLite messages provide recovery,
but they are not a substitute for a bounded active context.

### 12. Non-goals

- No lossless compaction.
- No persistent summary cache across daemon restarts or forks.
- No second agent run, subagent, or hidden model call for preparation.
- No automatic model routing. The summary uses the role's configured model.
- No embedding-based retrieval over old messages.
- No claim that the structured block contains the entire TeamPlan. It contains
  the listed authoritative fields; the full plan stays in SQLite and is loaded
  by the scheduler.
- No compaction-as-checkpoint and no checkpoint-as-compaction.

## Consequences

Positive:

- The retained tail is bounded by both message count and token cost.
- Recovery has a deterministic task-state anchor even when the model summary is
  weak or the summarizer fails.
- The threshold crossing can reuse a prepared summary, reducing latency without
  creating a second run or bypassing the budget.
- Compaction latency, reuse, fallback, and token deltas are observable without
  putting summary text into metrics.
- Existing clients and old summaries remain readable.

Negative:

- Preparation can spend tokens that are never reused. This is accounted for
  honestly and is bounded to one attempt per key per run.
- More context fields and events increase the protocol surface; all new fields
  are additive and optional.
- The state block is a second rendering of plan data that must be kept in sync
  with the plan revision key. The key test suite covers model/plan/prefix/cursor
  invalidation.
- A prepared cut can retain a larger tail than the ideal token-aware cut. The
  hard-limit trim still runs, so correctness is preserved at a small efficiency
  cost.

## Verification

- `cargo test -p vp-core compaction_` covers the existing threshold, manual
  compaction, failure fallback, calibration, and system/tool token accounting
  paths.
- `keep_tokens_trims_to_a_deterministic_message_boundary` proves the token
  window chooses a message boundary and retains at most the budget.
- `compaction_fallback_preserves_the_authoritative_plan_state` proves the plan
  goal, active task, acceptance/write scope, and plan revision survive a failed
  summarizer and rolling trim.
- `prepared_summary_is_reused_once_at_the_threshold` proves preparation happens
  below the threshold, reuse avoids a second summarizer call, and the event
  reports `background_reuse`.
- `prepared_summary_is_invalidated_by_model_and_plan_changes` proves model
  invalidation and `fallback_to_sync`.
- `prepared_summary_cache_invalidates_on_every_key_component` proves prefix,
  plan revision, and cursor mutation invalidation.
- `structured_state_is_deterministic_and_refreshable` proves deterministic
  rendering and in-place state refresh without duplicating the block.
- `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D
  warnings`, and `cargo fmt --all -- --check` remain the merge gate.

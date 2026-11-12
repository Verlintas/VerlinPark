# ADR-0189: Append-only conversation and canonical provider serialization

- **Status:** Accepted
- **Date:** 2026-11-12
- **Depends on:** ADR-0173 (cache-first prompt sections), ADR-0188
  (content-addressed instruction store and prompt layout v3), ADR-0190
  (tool bundle partition and canonical schemas)
- **Scope:** provider request assembly, conversation continuity accounting,
  cache-break telemetry, and the OpenAI-compatible/Anthropic parity contract

## Context

The instruction store made the *head* prefix byte-stable and the tool bundle
gave it a deterministic identity. That is only half of a reusable provider
prefix. The conversation that follows the head must also be append-only: a
follow-up turn may add messages, but it may not edit, reorder, or re-encode a
message that an earlier request already sent.

Before this ADR the runtime could not prove that property:

1. Messages were assembled separately in `openai.rs` and `anthropic.rs`; there
   was no single canonical representation that both adapters and telemetry
   agreed on.
2. Cache telemetry recorded the stable head/tool prefix hash but not the
   conversation's identity. A low cache-hit ratio could not distinguish
   "the provider evicted the cache" from "we changed a byte".
3. The memory brief was rebuilt every run and placed after the conversation.
   When the next turn appended an assistant/tool message, the previous tail
   memory moved out of its old position. Every turn therefore broke the exact
   prefix after the previous prompt, even though the string content was
   semantically unchanged.
4. There was no bounded record of *why* a list stopped being append-only, so
   debugging an edit or a reorder required diffing raw prompts by hand.

The L2 workstream of the cache-lossless plan requires a canonical serializer,
message-prefix hashes, bounded break reasons, and provider parity. It must not
change model routing, approval/Seatbelt/path/Verifier/merge/capacity/budget
gates, or introduce an automatic cache warm.

## Decision

### 1. One canonical message list

`vp-providers::canonical` owns a provider-aware projection of the exact
outbound message list. Both adapters and cache accounting call the same
functions, so the bytes that are hashed are the bytes that are sent, with one
deliberate exception: provider cache-control metadata is transport-only and is
excluded from the canonical identity.

The canonical view has three ordered parts:

| part | contents | wire placement |
|---|---|---|
| head | ordered `system_sections` (or the legacy single system string) | separate system messages for OpenAI-compatible; top-level `system` for Anthropic |
| history | the conversation, provider-projected | `messages` array |
| tail | non-embedded `tail_sections` (`tail/memory`) | trailing system/user message |

The history projection is provider-specific but semantic:

- OpenAI-compatible: `system`/`user`/`assistant` strings, `tool_calls` with
  `id`/`type`/`function`, and `tool` messages with `tool_call_id`.
- Anthropic: tool results become user messages with a `tool_result` block;
  assistant tool calls become `tool_use` blocks; a persisted system-role
  memory anchor becomes a user message prefixed `[memory]` because Anthropic's
  `system` field is a top-level request parameter.

`canonical_json` is a deterministic JSON writer. Object keys are sorted at
every depth, array order is preserved, strings use JSON escaping, and no
insignificant whitespace is emitted. Tool-call arrays and tool-result messages
keep the order produced by the provider/conversation; the runtime never sorts
or groups them to "improve" a cache hit. A bounded
`audit_message_sequence` pass reports orphan results, unanswered calls,
duplicate/empty ids, and out-of-order results. Diagnostics never mutate the
request.

Transport and provider-only fields are stripped before hashing:
`cache_control`, `reasoning_content`, and `reasoning_details`. Anthropic moves
its breakpoint on the current last message each turn; that is metadata, not
prompt content, and must not make the canonical list look edited. DeepSeek
reasoning tokens are never replayed; only assistant content and tool calls are.

### 2. Hash chain and append-only proof

Each canonical history message hashes to a full SHA-256 hex string. The list
hash is a domain-separated chain:

```text
message_hash[i] = sha256(canonical_json(message[i]))
history_hash    = sha256("verlinpark.canonical.messages.v1\n"
                         + count + "\n" + join(message_hash, "\n"))
```

The chain commits to order and to every message. It also allows an O(1) prefix
check from persisted state: a later call recomputes the chain at the previous
message count and compares it with the previous chain value. Per-message hashes
are persisted only as a bounded diagnostic list (512 entries, 16 hex prefix
characters); the chain hash remains full-length and covers every message.

`head_prefix_hash`, `tail_prefix_hash`, `tools_hash`, and a provider wire tag
are combined into `request_prefix_hash`:

```text
request_prefix_hash = sha256("verlinpark.canonical.request.v1\n"
                             + wire + "\n" + head + "\n" + history + "\n"
                             + tail + "\n" + tools_hash)
```

The stable tools hash uses the same canonical schema contract as ADR-0190:
tools sorted by name, deduplicated by first occurrence, parameters recursively
key-sorted. A test asserts the provider helper and
`vp_core::tool_bundle::canonical_tools_hash` agree.

`append_only` is true exactly when the previous canonical history list is an
exact prefix of the current list. The full request hash also covers head and
tail, so a changed head/tool bundle is a separate structural break, not an
append.

### 3. Break detection and bounded telemetry

Continuity is evaluated before every provider call from the exact request that
will be sent. A `context.cache_break` event is emitted for every non-append-only
observation, including the first call of an agent. The bounded reason set is:

| reason | meaning |
|---|---|
| `first_call` | no previous canonical observation exists |
| `system_changed` | the head sections or provider wire changed |
| `tool_bundle_changed` | the canonical tools hash changed |
| `message_edited` | a message hash differs and no reorder pattern was found |
| `message_reordered` | a hash moved to a different index |
| `compaction_reset` | a compaction boundary changed the active list |
| `provider_cache_miss` | the list was append-only but the provider reported zero cache reads |
| `unknown` | a break is proven by the chain but the first index is outside the persisted window |

Priority is deliberate: wire/tools/head changes take precedence over message
classification; a pending compaction marker takes precedence over
edited/reordered classification because it explains the rewrite; a provider
miss is only emitted post-call when the list was append-only. A compaction
marker that does not change the list is cleared without a false reset.

The event carries only safe metadata:

```json
{
  "reason": "message_reordered",
  "first_differing_index": 3,
  "first_differing_role": "assistant",
  "first_differing_tool_call_id": "call_01...",
  "previous_message_count": 9,
  "message_count": 10,
  "previous_message_prefix_hash": "…",
  "message_prefix_hash": "…",
  "provider_wire": "openai-compatible",
  "tail_memory_mode": "embedded",
  "provider_tail_system": true,
  "tail_system_capability": "supported"
}
```

It never contains message text. `context.cache` carries the same continuity
fields on every ordinary provider call, so a session can be audited without a
separate break lookup:

- `canonical_version`, `provider_wire`
- `previous_message_prefix_hash`, `message_prefix_hash`
- `append_only`, `previous_message_count`, `message_count`
- `message_hashes` (bounded), `message_hashes_complete`
- `head_prefix_hash`, `tail_prefix_hash`, `request_prefix_hash`
- `canonical_message_bytes`
- `tail_memory_mode`, `provider_tail_system`, `tail_system_capability`
- `cache_break_reason` (nullable on an append-only call)
- `message_sequence_issue_count`, `message_sequence_issues`

Warm calls carry canonical fields too (`warm: true`) but are excluded from
append-only aggregation: they intentionally send only the stable head and are
not part of the conversation chain.

### 4. Memory is an append-only anchor, not a movable suffix

A per-request tail memory message cannot coexist with exact-prefix continuity.
If turn N sends `[history, memory]` and turn N+1 sends
`[history, assistant, tool, memory]`, the previous list is not a prefix: the
memory moved. Re-inserting memory ahead of the conversation would be worse—a
memory edit would invalidate the entire history.

The runtime therefore materializes the L1 `tail/memory` section as a real
conversation message once per distinct brief:

- The anchor is a `system` message with the exact tail-section text.
- It is appended after the prompt that triggered it, never inserted ahead of
  history.
- An identical brief reuses the latest anchor and adds no bytes.
- A changed brief appends a newer anchor; older anchors remain as honest
  history. They can only disappear through compaction.
- OpenAI-compatible providers receive the anchor as a system message when the
  capability is `supported`; Anthropic and unknown providers receive a user
  message prefixed `[memory]`, deterministically.
- If the tail text is already present in history, the adapter does not append a
  second suffix (`tail_memory_mode=embedded`).

This preserves the L1 invariant "memory is tail-only" in the meaningful sense:
memory never changes the head or an earlier message, and every memory update is
an append at the tail. It also makes cross-run continuity possible: the anchor
is persisted, so a follow-up run reconstructs the same history bytes.

The request still carries the L1 `tail_sections` list for telemetry and for
direct adapter callers that have not materialized an anchor. The canonical
layer detects an exact embedded value and does not append it again
(`tail_memory_mode=embedded`), so the compatibility field cannot duplicate the
memory on the wire.

`context.memory.anchor` records `anchor_hash`, `anchor_chars`, `appended`,
`message_position`, and `prompt_layout`; it never records the memory text.

### 5. Provider parity and capability result

OpenAI-compatible/DeepSeek:

- canonical body; prior messages byte-identical across turns;
- no `cache_control` marker anywhere in the body;
- `reasoning_content` is not streamed as assistant text and is never replayed;
- usage parsing is unchanged (`prompt_tokens`, cached tokens, DeepSeek
  `prompt_cache_hit_tokens`, cache-write vendor fields when present);
- `stream_options.include_usage` is still requested, with the existing
  one-shot fallback for servers that reject it.

Anthropic:

- the same canonical message list; explicit breakpoints remain on tools and
  stable system sections and stay within the four-breakpoint cap;
- tail memory is after the last breakpoint and is a stable `[memory]` user
  message when not embedded;
- when an explicit tail breakpoint mode is active (`automatic=false`), every
  non-empty string content is serialized as a single text block array before
  the marker is placed. The array shape is stable across turns; only the
  marker metadata moves. Without this normalization an already-sent message
  would switch between a plain string and a marked array when it stopped being
  last, which is a real serialization change;
- the canonical identity strips `cache_control`, so a moved breakpoint is not a
  message edit.

Capability fallback is explicit. `TailSystemCapability` is `supported`,
`unsupported`, or `unknown`. `unknown` uses the user form because both server
shapes accept it. Anthropic is `unsupported` by construction. An
OpenAI-compatible server that rejects a mid-history system message with a
narrow 400/404/422 pattern is retried exactly once with the user form; the
adapter records the downgrade and `Provider::cache_capabilities` reports
`unsupported` for later calls. The runtime emits `context.cache_capability`
when it observes the change. The retry never rewrites prior content; only the
role of the memory anchor changes, and that is recorded as a real break.

### 6. Cross-run seeding and no "repair"

The in-run tracker is seeded from the latest ordinary `context.cache` event for
the same agent (falling back to role for legacy events). If the latest
`context.compacted` event is newer than that cache event, the tracker starts
with a compaction marker so a manual or previous-run compaction is attributed
to `compaction_reset` rather than a generic edit.

There is no repair path. The runtime never rewrites, reorders, or drops history
to turn a break into a hit. The only intentional history rewrite is compaction,
which is explicit, event-backed, and already records its own invalidation
estimate. A provider miss on an append-only list is recorded as
`provider_cache_miss`, not blamed on a local edit.

## Consequences

- Cache telemetry can now distinguish local byte churn from provider eviction.
  `session.cache` exposes `last_message_prefix_hash`,
  `last_previous_message_prefix_hash`, `last_append_only`,
  `append_only_calls`, `non_append_only_calls`, `recent_breaks`,
  `cache_break_counts`, tail-memory mode, and sequence-issue counts.
  `session.cache.breaks` / `vp cache-breaks` returns the bounded newest-first
  break history without message text.
- `cache_audit` adds three findings: `non_append_only_history` (warning, with
  the first differing index/role and tool id), `provider_evicted_append_only_prefix`
  (informational; the chain was stable), and
  `legacy_events_without_continuity` (informational; old events cannot prove
  continuity).
- The OpenAI adapter's body is now built by one function shared with canonical
  accounting, so a future field addition cannot silently diverge from the
  hashed bytes.
- A persisted memory anchor appears as a `system` message in the session
  transcript. This is intentional: it is real context that was sent to the
  model, and keeping it visible avoids a hidden prompt mutation.
- Protocol changes are additive. Every new `SessionCacheCall`,
  `SessionCacheStatus`, and `SessionCacheBreak` field has a serde default, so
  legacy events and clients decode unchanged.

## Limits and non-goals

- The canonical identity is only as good as the provider projection. A
  provider that normalizes whitespace, re-wraps content, or injects its own
  system preamble can still miss a prefix that is byte-identical on our side.
  The cache chain proves *our* continuity, not a provider hit.
- Anthropic cache-control markers are excluded deliberately. A provider that
  treats marker movement as content would invalidate the prefix; the canonical
  telemetry would still report an append. That is a provider-specific risk we
  cannot prove from the wire.
- `provider_cache_miss` is only emitted when the provider reports zero cache
  reads. Partial reads are not attributed; the reason set stays bounded.
- Compaction summarizer calls are one-shot auxiliary requests. They are
  recorded in the usage ledger but are not part of the conversation chain and
  therefore do not emit `message_prefix_hash`/`append_only` fields. The
  compaction boundary itself is recorded by the existing compaction event and
  the next ordinary call is classified as `compaction_reset`.
- Per-message hashes beyond 512 entries are not persisted. The chain still
  proves a break, but the first differing index may be `unknown` on a very long
  conversation.
- Old memory anchors accumulate until compaction. This is the price of exact
  continuity; the runtime does not silently delete context to shrink a prompt.
- The OpenAI capability retry is best-effort and narrow. It does not attempt to
  discover an arbitrary server's accepted role grammar, and it never retries a
  partial stream.
- No automatic cache warm, no hidden model routing, no provider TTL inference,
  no cross-host cache sharing, and no gate changes.
- This does not implement L4 cache epochs, compaction policy, or L5 benchmark
  and GUI surfaces. It records the continuity evidence those modules consume.

## Verification

- Wiremock tests assert the raw JSON `messages` array prefix across appended
  turns, including tool calls/results, Unicode content, embedded memory, and a
  ten-turn conversation.
- A fallback test rejects a mid-history system message once and proves the
  retry uses the stable user form without changing prior messages.
- DeepSeek reasoning deltas are excluded from text and from replayed history;
  usage parsing is unchanged.
- Unit tests cover append/unchanged/edit/reorder/tool-call reorder/truncation/
  compaction classification, legacy payload defaults, bounded hash lists,
  chain-prefix proof with truncated telemetry, sequence diagnostics, and
  canonical-tool hash parity with the L3 contract.
- Integration tests run the real agent loop and assert append-only continuity,
  memory anchor reuse/append across runs, compaction attribution, canonical
  telemetry fields, and content-free protocol projections.

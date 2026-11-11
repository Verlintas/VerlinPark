# ADR-0154: Efficiency dashboard in the macOS client

- **Status:** Accepted
- **Date:** 2026-11-11
- **Owners:** macOS client, Runtime, Observability
- **Related:** ADR-0138, ADR-0146, E4 in `docs/EFFICIENCY_PLAYBOOK.md`

## Context

The usage ledger already records input, output, cache-read, and cache-write
tokens, and the context snapshot already records estimated active context. The
macOS client showed those numbers on separate surfaces (Manage Dash → Usage &
Budget and the Studio cost preview), but not the one ratio that explains most of
a long session's input cost: how much provider input was served from cache.

The efficiency workstream (E1) will add `UsageTotals.cache_hit_ratio`,
`ContextUsage.preamble_tokens_estimate`, and a `session.cache` RPC with prompt
prefix and preamble telemetry. E4 is the client surface and ships independently.
It must therefore work against both an older daemon that only reports token
counters and a newer daemon that reports the ratio directly.

This is a read-only dashboard. It does not warm caches, change tool schemas,
infer provider TTLs, or route models. It only makes the daemon's measurements
visible where operators already work.

## Decision

### 1. Manage Dash → Usage & Budget

`EfficiencyCard` appears before `ProjectUsageCard` for the selected project or
session. It shows:

| Value | Source | Fallback |
|---|---|---|
| Cache hit ratio | `session.cache.cache_hit_ratio`, then `session.usage.totals.cache_hit_ratio` | Derived from cache-read, cache-write, and uncached-input tokens |
| Cached input | `session.cache.cache_read_tokens` | `session.usage.totals.cache_read_tokens` |
| Uncached input | `session.cache.resolvedUncachedInputTokens` | `input_tokens - cache_read - cache_write`, clamped to zero |
| Output | `session.usage.totals.output_tokens` | Shown as `—` when no ledger is available |
| Total cost | `session.usage.totals.cost_usd` | `project.usage.totals.cost_usd` when no session is selected |
| Cost per session | Total cost / session count | Session scope uses one session; project scope divides by `project.usage.sessions` |
| Cost per task | Total cost / ledger task-bucket count | Hidden when `by_task` is empty; the ledger does not claim completion |
| Context usage | `session.context.usage` | Hidden until a snapshot exists |
| Compaction count | `context.compacted` events | At least one when `last_compaction` exists but the event window does not |
| Preamble estimate | `session.cache`, then `ContextUsage.preamble_tokens_estimate` | Hidden |
| Prompt prefix | `session.cache`, then the latest `context.prompt_prefix` event | Hidden |

The ratio is rendered large. It uses the theme warning color below 50% and the
success color otherwise. Token, cost, and context values use the existing
monospaced labels, card surface, and hover/appearance motion system. There are
no new ad-hoc colors or fixture rows.

The card always states: **"Cache read is billed, not free; this is a hit ratio,
not a discount."** A derived ratio is labeled as derived; a daemon-reported
ratio is labeled as reported.

### 2. Stage context popover

The context popover adds a compact cache-hit pill and, when available, the last
prompt prefix hash. Both disappear rather than showing invented values. The
same "billed, not free" note appears under the pill.

### 3. Optional-field compatibility

`UsageTotals` gains `cacheHitRatio: Double?` (`cache_hit_ratio`) and
`ContextUsage` gains `preambleTokensEstimate: Int?`
(`preamble_tokens_estimate`). Absent fields decode as `nil`.

`SessionCacheStatus` is a new optional model for the future `session.cache`
RPC. Every field is optional, aliases `prefix_hash`/`cache_generation` and
`last_call` are accepted, and numeric fields accept integral doubles. The
client calls `session.cache` only from `loadEfficiency`; an error clears the
cache status and falls back to `session.usage`. It never fails the dashboard or
requires a field that an older daemon does not send.

### 4. Derived ratio math

The daemon field is authoritative when present. Otherwise:

```text
uncached = max(0, input_tokens - cache_read - cache_write)
ratio    = cache_read / (uncached + cache_read + cache_write)
```

`input_tokens` is the provider's total input under ADR-0138 and already includes
cache reads/writes, so the client normalizes it before applying the E4 formula.
A zero denominator yields no ratio. Ratios are clamped to `0...1`; a daemon
value outside that range is treated as a reporting defect, not as a discount
larger than 100%.

### 5. Refresh and state

`AppModel` adds `efficiencyBusy`, `efficiencyStatus`, and
`sessionCacheStatus`. `loadEfficiency()` reads `project.usage`, `session.usage`,
`session.context`, and optionally `session.cache`. It runs on Usage section
appear, on selected-session change, and on debounced `usage.recorded` /
`agent.usage` / `agent.cost` / `context.measured` / `context.compacted` /
`context.cache` / `context.prompt_prefix` events. Existing
`selectionGeneration` guards discard stale responses.

## Why cache share is not a discount

Prompt caching changes the **price per cached input token**, not the number of
tokens processed. Cache reads are still billed at the configured cache-read
price, cache writes can cost more than uncached input, and output is never
cached. A 99% hit ratio means most input was reused; it does not mean the bill
fell by 99%. The card shows the observed ratio and the actual ledger cost side
by side so an operator cannot mistake one for the other.

The client does not multiply tokens by prices, estimate provider discounts, or
infer cache hits from latency. USD values come only from the daemon ledger,
which uses user-configured per-role prices.

## No automatic warming

The client never issues a provider call to warm a cache. It does not request
cache TTL changes, background summaries, or prefix refreshes. Making a cache
visible is not permission to spend money on it. Any future warming feature
needs its own explicit opt-in, budget gate, and ADR.

## No provider cost inference

`session.cache` and the fallback ledger describe tokens actually reported by the
provider adapter. If a provider reports no cache fields, the UI shows no ratio
or a zero-token state; it does not estimate a hit rate from timing, model
metadata, or a previous model.

## Consequences

- Positive: the dominant long-session cost lever is visible next to cost and
  context without leaving Manage Dash.
- Positive: older daemons keep working; the card degrades to usage-derived
  ratios and hides unavailable preamble/prefix values.
- Positive: the card unlocks automatically when E1 lands; no second client
  migration is needed.
- Negative: before E1, deriving the ratio requires normalized token math and
  the UI labels it as derived.
- Negative: compaction count is a lower bound when the event window has rolled
  or events are not loaded; `last_compaction` still guarantees at least one.
- Negative: cost per task uses ledger task buckets, not a solved-task verdict.
  A task with failed or partial calls can still appear in the denominator.
- Follow-up: per-task usage belongs in Task Detail when the ledger exposes task
  ids there; CSV cache-ratio columns and session-table sorting are separate E4
  slices.

## Validation

Swift tests cover `UsageTotals` with and without `cache_hit_ratio`,
`SessionCacheStatus` empty and populated payloads, alias decoding, derived
ratio math including zero input and cache-greater-than-input clamping,
formatting, compaction count, and snapshot assembly with and without
`session.cache`. The existing macOS test suite must remain green.

## Deliberately not included

- Automatic cache warming or TTL management.
- Provider-specific discount or savings estimates.
- Cross-user or cross-model benchmarking.
- Mock, sample, or placeholder efficiency rows.
- Rust telemetry changes; E1 owns the daemon fields and RPC.

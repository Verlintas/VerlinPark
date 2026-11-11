# ADR-0153: Cache and preamble telemetry

- **Status:** Accepted
- **Date:** 2026-11-11
- **Scope:** usage ratio derivation, `session.cache`, context cache events,
  Prometheus cache metrics, and the `vp cache` CLI
- **Related:** ADR-0138 (cache usage and stable prompt prefixes), ADR-0146
  (daemon metrics), E1 in `docs/EFFICIENCY_PLAYBOOK.md`

## Context

ADR-0138 records provider cache-read/cache-write tokens, prices the disjoint
uncached/cache-read/cache-write portions, and preserves a stable prompt prefix.
The local efficiency research measured a 99.36% cache-read share, making cache
reuse the dominant cost lever, but operators still had to reconstruct the ratio
from raw ledger rows. There was no bounded cache status RPC, no post-settlement
cache event, and no daemon-level cache ratio or preamble estimate.

This ADR makes that lever observable. It deliberately adds no cache warming,
provider TTL inference, or hidden cache routing.

## Decision

### 1. Cache token semantics

`UsageEntry.input_tokens`, `UsageTotals.input_tokens`, and
`ProjectUsageSession.input_tokens` remain the provider's **total** input as
defined by ADR-0138: uncached input + cache reads + cache writes. Cache reads
and writes are disjoint subsets of that total and are never added to session
token totals a second time.

For the hit ratio, the three ledger portions are treated as disjoint buckets.
In the formula, `input` is the uncached remainder and cache reads/writes are the
cache subsets of the provider total:

```text
uncached = input_tokens - cache_read_tokens - cache_write_tokens
cache_hit_ratio =
    cache_read_tokens / (uncached + cache_read_tokens + cache_write_tokens)
```

For a consistent provider report this simplifies to
`cache_read_tokens / input_tokens`. The result is clamped to `0.0..=1.0`; a
zero denominator yields `0.0`. An inconsistent report where the cache buckets
exceed input is clamped to `1.0` rather than exceeding the metric bound.

### 2. Derived, not persisted

`cache_hit_ratio` is computed while building the usage ledger, project usage
report, and daemon metrics snapshot. It is not a SQLite column, event payload
field, or bundle field. Replay, repair, database rebuild, import/export, and
older bundles therefore recompute the same value from the ledger entries.

New response fields use `#[serde(default)]`:

- `UsageTotals.cache_hit_ratio`
- `UsageBucket.cache_hit_ratio`
- `ProjectUsageSession.cache_hit_ratio`
- `DaemonMetrics.cache_hit_ratio`
- `DaemonMetrics.preamble_tokens_estimate: Option<u64>`

`session.usage`, `project.usage`, and usage CSV expose the ratio. CSV adds a
`cache_hit_ratio` column per ledger entry. The ratio is observational; it is not
a billing reconstruction and does not claim that a cache read is free.

### 3. `session.cache`

`session.cache { session_id }` returns the bounded `SessionCacheStatus`:

```text
session_id, input_tokens, cache_read_tokens, cache_write_tokens,
output_tokens, cache_hit_ratio, prompt_prefix, preamble_tokens_estimate,
last_call_at
```

Token values and the aggregate ratio come from the usage ledger.
`prompt_prefix` is the latest `context.prompt_prefix.prefix_hash`, falling back
to `context.measured.prompt_prefix_hash`. It is a hash, never prompt text.
`last_call_at` is the newest ledger entry time (with the latest measurement as
a fallback). A session with no calls returns zeros and `None` fields; missing
sessions behave the same way rather than inventing data.

`preamble_tokens_estimate` is a **heuristic**, not provider-reported usage. New
`context.prompt_prefix` events store the existing system-plus-tool-schema
estimate used for context-limit checks. Older events without that field fall
back to `stable_prefix_chars / 4`. The estimate includes the stable system
prefix, run context, and tool schemas; it excludes conversation messages.

### 4. Context events

Each provider call that settles usage emits `context.cache` with input, output,
cache-read/write tokens, the derived ratio, the prompt-prefix hash, the
estimated flag, and the model. It contains no prompt text and never causes an
extra provider call. A failed telemetry append is logged and does not discard
the usage settlement.

`context.measured` also carries `cache_hit_ratio`. `context.prompt_prefix`
gains `preamble_tokens_estimate`, `system_tokens_estimate`, and
`tool_tokens_estimate`. Existing event consumers ignore the additive fields.

### 5. Metrics

Prometheus adds:

- `verlinpark_cache_hit_ratio` (gauge, no labels), derived from the aggregate
  usage-ledger input/cache buckets and clamped to `0..1`;
- `verlinpark_preamble_tokens_estimate` (gauge, no labels), emitted only when a
  prompt-prefix event with an estimate exists. The whole HELP/TYPE/sample block
  is omitted when unavailable.

The existing `verlinpark_cache_tokens_total{direction="read"|"write"}` series
remain unchanged. `daemon.metrics` JSON gains `cache_hit_ratio` and an optional
`preamble_tokens_estimate`.

The preamble metric is daemon-wide: it reports the most recently observed
agent-run estimate, not an average and not a per-session value. No session,
task, model, path, or agent label is added. Detailed values remain available
through `session.cache` and `session.usage`.

### 6. CLI

`vp cache <session>` prints a human-readable cache summary; the global
`--json` flag prints the raw `SessionCacheStatus`. `vp usage` and
`vp usage --csv` show the same derived ratio.

## Non-goals

- No automatic cache warming, prefetch, or prompt replay.
- No provider TTL inference; absent provider cache fields stay zero.
- No hidden cache routing or model selection.
- No extra provider call solely for telemetry.
- No claim that cache-read share equals cost savings; cache reads are still
  billed at the user-configured cache price.
- No exact preamble tokenizer. The estimate is a stable heuristic and may
  differ from provider tokenization.

## Consequences

- A long-running session shows a stable ratio; a tool-schema or stable-system
  change produces a visible drop through the prefix hash and a subsequent cache
  write.
- The daemon can expose a low-cardinality cache signal to Prometheus without
  exporting prompts or identity.
- Historical sessions without prompt-prefix events report no preamble estimate
  and a zero ratio until ledger data exists.
- The daemon-wide preamble metric can lag behind the newest session briefly
  because it is read from the most recent event at scrape time.
- Because the ratio is derived at read time, replay/repair and database rebuild
  cannot drift from the ledger, and no migration is required.

## Validation

Tests cover ratio math (zero denominator, normal case, cache-greater-than-input
clamp), legacy payload defaults, usage-ledger settlement and replay, project
usage totals, empty and populated `session.cache`, legacy prompt-prefix
character fallback, `context.cache` emission after provider settlement, CSV
output, `vp cache` parsing, and bounded/omitted Prometheus lines.

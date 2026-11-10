# ADR-0138: Cache usage accounting and stable prompt prefixes

- **Status:** Accepted
- **Date:** 2026-11-11
- **Scope:** provider usage normalization, usage ledger/pricing, role prompt layout,
  tool-bundle stability, and prompt-cache telemetry

## Context

Long VerlinPark sessions spend most provider input tokens re-reading the same
role charter, tool schemas, and project conventions. Prompt caching only helps
when the request prefix is byte-stable and the runtime can observe cache reads
and writes. The cache research notes (`docs/AGENT_ORCHESTRATION_AND_CACHE_RESEARCH.md`)
list two P0/P1 gaps:

1. provider adapters discarded cache usage, so the ledger could not price or
   audit cache behavior;
2. volatile run context (goal, worktree, branch, task objective) was emitted
   before stable role text, and skill activation filtered the advertised tool
   schema mid-run, invalidating the cached prefix.

## Decision

### 1. Usage semantics

`StreamEvent::Usage` gains `cache_read_tokens` and `cache_write_tokens`.
`input_tokens` remains the provider's **total** input: uncached input + cache
reads + cache writes. Cache fields are disjoint subsets of that total and are
used only for pricing and hit-rate telemetry. They are never added to session
token totals a second time.

- **OpenAI-compatible:** `input_tokens = usage.prompt_tokens`; cache reads come
  from `usage.prompt_tokens_details.cached_tokens` (falling back to
  `usage.prompt_cache_hit_tokens`); cache writes from
  `usage.prompt_tokens_details.cache_write_tokens`, `usage.cache_write_tokens`,
  or `usage.prompt_cache_write_tokens` when a compatible server exposes one.
- **Anthropic:** `message_start.usage.input_tokens` is the uncached part;
  `cache_read_input_tokens` and `cache_creation_input_tokens` are added to it to
  produce the total public `input_tokens`.
- Unknown or absent fields stay zero.
- Inconsistent reports are clamped so `cache_read + cache_write <= input_tokens`
  and no token can be billed twice.

`UsageEntry`, `UsageBucket`, `UsageTotals`, `ProjectUsageSession`, the
`usage_ledger` table, the `agent.usage` event, CSV export, replay/repair,
import/export, and `db-rebuild` carry the same fields. New fields use serde
defaults; older bundles and events still decode, and existing SQLite databases
gain the columns through the non-destructive `ensure_column` migration.

### 2. Pricing

`RoleModelConfig` gains optional `cache_read_usd_per_million` and
`cache_write_usd_per_million`. Cost is computed as:

```text
uncached_input * input_price
+ cache_read   * cache_read_price
+ cache_write  * cache_write_price
+ output       * output_price
```

If a cache price is unset it falls back to the full input price. Existing
configurations therefore keep their previous (conservative) cost and automatic
cache discounts are never invented. Budget reservations still reserve the whole
input estimate at the full input price because the actual cache mix is unknown
before the call; settlement uses the provider-reported mix.

### 3. Stable prompt layout

`role_system_prompt` is split into two deterministic blocks:

1. **Stable prefix:** role charter, security/output contract, and stable project
   conventions. This block never contains timestamps, session ids, worktree
   paths, branch names, task objectives, or agent names.
2. **Run context:** agent name, department title, parent, project root, session
   goal, worktree, branch, profile, write scope, and objective. It is stable for
   one agent run and therefore comes last, after the shared prefix.

Project source titles and the project skill catalog are appended to the stable
prefix. Goal-dependent project memory is appended after run context. Tool
definitions are serialized before system blocks by the providers, so the stable
role/project prefix can be reused across sessions and across Workers in the same
team even though their worktrees differ.

The advertised tool bundle is frozen for the run:

- skill activation no longer filters tool schemas; `use_skill` changes only the
  execution-time policy, and a disallowed call returns a precise
  `tool '<name>' is not allowed by active skill '<name>'` error;
- dynamic MCP tools are sorted by exposed name before the 32-tool cap and are
  collected once per run;
- role and MCP schema order is therefore stable across turns and skill
  activation.

### 4. Provider cache breakpoints

`ChatRequest` gains an optional `system_stable_prefix` marker. The Anthropic
adapter uses it to split `system` into a cached stable text block plus an
uncached run-context block, and marks the last tool definition with
`cache_control: {type: "ephemeral"}`. That produces the documented
`tools -> system` prefix order with two breakpoints, within Anthropic's limit.
The marker never adds or removes prompt content. OpenAI-compatible providers
ignore it and use their automatic prefix cache.

If the marker is absent or is not an exact prefix, the adapter sends the system
prompt unchanged (as a single text block) so older callers and non-caching
proxies still work.

### 5. Prompt-prefix telemetry

Each agent run emits one `context.prompt_prefix` event with:

- `version` (1),
- `model`,
- `tool_bundle_hash` (SHA-256 of the sorted serialized tool bundle),
- `stable_system_hash` (SHA-256 of the stable system prefix),
- `prefix_hash` (SHA-256 of version + model + both hashes), also exposed as
  `cache_generation`,
- `tool_count` and `stable_prefix_chars`.

The hash input contains only model id, tool schemas, and the stable system text.
It contains no secrets, API keys, worktree paths, or session identifiers. Hits
and misses can be compared against this generation to distinguish cache churn
from model/tool changes. Compaction still replaces the message head (a new
conversation cache generation) but leaves the stable prefix unchanged.

## Consequences

- `session.usage`, `usage_csv`, the CLI usage view, the Preview Cost surface,
  and the project usage report show cache reads/writes and a derived read hit
  rate (`cache_read_tokens / input_tokens`).
- Cache pricing is user-configured, consistent with ADR-0004 (no hidden model
  routing).
- Anthropic writes can cost more than uncached input; observe write volume
  before enabling aggressive caching.
- Explicit `cache_control` is not sent to OpenAI-compatible endpoints. Gemini
  explicit cache objects remain out of scope.
- A provider that reports no cache fields produces zero cache usage; the
  runtime does not estimate a hit rate from timing or heuristics.

## Validation

Tests cover OpenAI SSE `cached_tokens` / cache-write fields, Anthropic
`cache_read_input_tokens` / `cache_creation_input_tokens`, cache pricing and
fallback, ledger totals/CSV, event replay/import, provider cache usage reaching
session totals, stable-prefix ordering, unchanged tool bundles after skill
activation, deterministic MCP ordering, and Swift decoding of the new optional
usage fields.

# ADR-0168: Cache-first prompt architecture

- **Status:** Accepted
- **Date:** 2026-11-11
- **Owners:** Runtime, providers, protocol, Swift dashboard
- **Related:** ADR-0138 (cache usage accounting), ADR-0149 (context
  compaction), ADR-0160 (compaction budgets and prepared summaries), ADR-0164
  (context checkpoints)

## Context

VerlinPark already records provider cache-read and cache-write tokens and
derives a cache hit ratio. That accounting is honest, but prompt construction
was not designed for reuse:

1. The role charter, project conventions, indexed-source brief, and skill
   catalog were concatenated into one "stable" string. A source or skill edit
   moved the entire cached prefix, including the role charter that never
   changed.
2. The retrieved memory brief and run context were appended after that string,
   but there was no component boundary to prove they could not move the
   reusable prefix.
3. The Anthropic adapter marked the last tool and the stable system prefix, but
   it had no moving breakpoint on the conversation. Long conversations were
   therefore largely uncached after the first turn.
4. The OpenAI-compatible adapter ignored the stable-prefix marker, merged all
   system text into one message, and did not sort tool definitions. A registry
   iteration-order change could move the provider's automatic prefix cache.
5. There was no explicit TTL policy. Operators could not opt into Anthropic's
   1-hour pool, and the runtime could not distinguish a cache miss caused by a
   tool change from a TTL/provider miss.
6. Compaction rewrote the active conversation without recording which cache
   generation was invalidated or estimating the lost cache-read savings.
7. There was no explicit warm path. An operator could not populate the stable
   prefix before a long session or after a human pause, and there was no
   accounting if such a call existed.

Cache reads are billed. A cache-first design is not a claim that caching is
free; it is a claim that the reusable prefix must be deterministic, observable,
and impossible to invalidate accidentally.

## Decision

### 1. Three deterministic prompt sections

Every agent system prompt is assembled from three sections, in this order:

| Section | Contents | Stability |
| --- | --- | --- |
| `core` | Role charter, security/output contract, stable project conventions | Shared by every agent with the same role/profile/project |
| `project` | Indexed-source brief and skill catalog | Semi-stable; changes with project knowledge |
| `volatile` | Run context, objective, and retrieved memory brief | Per agent/session/run |

The runtime keeps the combined `system` string for backward compatibility and
also carries the exact section boundaries to provider adapters. The combined
string is exactly `core`, then `project`, then `volatile`, joined by one blank
line. Empty sections are omitted. Volatile content is never placed before
stable content.

A retrieved memory brief is always part of `volatile`. No memory retrieval,
task assignment, worktree path, or objective text can change `core` or
`project`.

Source and skill changes are confined to `project` where possible. They move
`project_hash` and the combined prefix hash, but not `core_hash` or
`tools_hash`.

### 2. Component hashes

`context.prompt_prefix` and every per-call cache event record SHA-256 component
hashes:

- `core_hash`
- `project_hash`
- `tools_hash` (sorted tool schemas)
- `volatile_hash`
- `stable_system_hash` (`core + project`)
- `prefix_hash` over `(prompt-section version, model, tools_hash, core_hash,
  project_hash)`

`prefix_hash` deliberately excludes volatile content. It is the stable cache
generation shared across sessions with the same role, profile, model, project,
and tool bundle. `context.prompt_prefix.version` is `2` for this layout; older
events remain readable and are not rewritten.

Hashes are persisted instead of prompt text. Cache telemetry contains no
prompt, memory, tool result, or provider secret.

### 3. Anthropic-compatible caching

The adapter supports two safe combinations, chosen by the project policy:

**Automatic moving breakpoint (default).**

1. Last tool block: explicit `cache_control` breakpoint.
2. `core` system block: explicit breakpoint.
3. `project` system block: explicit breakpoint.
4. Request-level top-level `cache_control`: Anthropic applies the moving
   breakpoint to the last cacheable block and advances it as the conversation
   grows.

This uses exactly four of Anthropic's documented four breakpoint slots. The
volatile system block gets no explicit breakpoint. If one of the optional
sections is absent, the plan uses fewer slots; it never exceeds four.

**Explicit tail breakpoint (`automatic_caching = false`).**

When the operator disables request-level automatic caching (for example for a
gateway that accepts block-level markers but not the top-level field), the
adapter replaces slot 4 with a breakpoint on the last cacheable content block
of the last message. Tool, `core`, and `project` breakpoints remain. A string
message content is converted to a one-block array with the marker; an existing
array receives the marker on its last block. Message text is never changed.

The adapter never emits both a top-level automatic marker and a tail marker;
that would double-count the moving slot and can create TTL conflicts. The
selected plan is recorded before the request as `cache_plan` and again on the
cache event as `cache_breakpoints`, `cache_ttl`, and `cache_automatic`.

**TTL.**

- `5m` is the default and omits the `ttl` field.
- `1h` sends `"ttl":"1h"` on every explicit marker and on the top-level
  automatic marker when present.
- The runtime never assumes a provider or model supports 1h. If a provider
  rejects the extended TTL with a cache/TTL-related HTTP 400/404/422, the
  adapter returns an actionable `ProviderError::Unsupported` that names the
  policy command and explicitly says no silent downgrade was attempted. It
  does not retry with 5m. An unrelated HTTP error is reported as the normal
  provider error.
- No beta header is added implicitly. If an endpoint requires one, its
  rejection is surfaced through the same actionable error.

The compatibility fallback (no section boundaries or no cache policy) retains
the historical last-tool plus stable-prefix breakpoint shape. It does not
silently enable automatic caching.

### 4. OpenAI-compatible caching

OpenAI-compatible endpoints use provider-managed automatic prefix caching. The
adapter therefore:

- sends `core`, `project`, and `volatile` as separate system messages in that
  order, with stable sections first and volatile content last;
- falls back to one legacy system message when no non-empty sections are
  supplied;
- sorts tool definitions by name before serialization;
- does not emit `cache_control`, `ttl`, or any Anthropic-specific marker;
- preserves streaming, `stream_options.include_usage`, and usage parsing.

The runtime reports `cache_breakpoints = 0` and `cache_ttl_explicit = false` for
these requests. `automatic = true` means the provider owns prefix caching, not
that VerlinPark sent a breakpoint.

### 5. Project cache policy

A new project-scoped `CachePolicy` is persisted in SQLite
(`project_cache_policies`), recorded in `projects.jsonl`, and restored by
`db-rebuild`. It is validated on write; unknown TTLs or roles are rejected.

```json
{
  "enabled": true,
  "ttl": "5m",
  "automatic_caching": true,
  "warm_on_resume": false,
  "compact_aware": true,
  "role_ttl": {"worker": "5m", "leader": "1h"}
}
```

- `enabled` is the master switch. Disabled means no markers and no warm call.
- `ttl` is `5m` or `1h`; `role_ttl` can override one role.
- `automatic_caching` selects the Anthropic moving-breakpoint mode.
- `warm_on_resume` is false by default and is the only automatic warm path.
- `compact_aware` records compaction generations and invalidation estimates;
  it never delays compaction.

The policy is exposed through `project.cache_policy.get` /
`project.cache_policy.set`, `vp cache-policy` / `vp cache-policy-set`, and the
`session.cache` status payload. There is no hidden provider/model routing and
no per-request policy inference.

### 6. Compaction invalidation

Compaction rewrites the active conversation prefix. The stable system/tool
prefix is unchanged, but cached message content before the breakpoint is no
longer reusable. Every compaction event records:

- `cache_generation_before` and `cache_generation_after`: SHA-256 over the
  stable prefix plus the exact message cursor;
- `cache_reset_reason`: `compaction`;
- `cache_invalidation_tokens`: estimated removed conversation tokens;
- `cache_invalidation_estimated_usd`: estimated lost cache-read savings using
  the role's configured input and cache-read prices;
- `cache_compact_aware`: whether the policy requested these estimates.

The next provider call in the same run is attributed to `compaction_reset`
when its component hashes are otherwise unchanged. Compaction safety and
timing are unchanged: the cache metrics are descriptive, not a gate.

### 7. Warm path

`session.cache.warm` / `vp cache warm <session>` (also accepted as
`vp cache-warm <session>`) makes one explicit provider request using only the
stable `core` and `project` sections plus the frozen tool bundle. It excludes
memory, objective, and conversation history. It is rejected for active
sessions, records usage under role `cache_warm`, emits `context.cache.warm`
with `warm: true` and `context.cache_warm`, and returns token/cost/latency
fields. A disabled policy makes it a no-op with no provider call.

When `warm_on_resume` is true, a resumed journaled run performs one warm before
its first turn and attributes the source as `resume`. Failure is logged and
does not abort the run. The default is false, so a normal run has no extra
provider call.

### 8. Metrics, savings, and miss attribution

Per call the runtime records:

- input, output, cache read, and cache write tokens;
- `cache_hit_ratio`, `cache_read_ratio`, `cache_write_ratio`;
- breakpoint count, TTL, TTL-explicit flag, automatic/tail mode;
- first-token latency;
- component hashes and the combined prefix hash;
- `estimated_savings_usd`;
- a derived miss reason.

`estimated_savings_usd` is:

```
uncached_cost(input, output) - billed_cost(uncached_input, cache_read,
                                          cache_write, output)
```

using the configured role prices. Cache reads are priced at the cache-read
price (or full input when unset, conservative). Cache writes are priced at the
cache-write price. The result can be negative; it is not clamped, because
writing a cache that is never read costs more than not caching.

Miss reasons are derived only from observed facts:

| Reason | Meaning |
| --- | --- |
| `first_call` | No prior cache observation for the session |
| `tools_changed` | Sorted tool bundle hash moved |
| `project_changed` | Source/skill project hash moved |
| `prefix_changed` | Core or combined stable prefix moved |
| `compaction_reset` | Conversation generation was rewritten |
| `ttl_or_provider_miss` | Same component hashes, zero cache reads |
| `unknown` | Reserved for future use |

A reason is recorded only when the provider reports zero cache reads. A hit
records no miss reason. The runtime never invents a provider TTL fact; when the
hashes are unchanged it says `ttl_or_provider_miss`, not "expired".

`session.cache` additively exposes totals, ratios, estimated savings, cache
generation/component hashes, last breakpoint/TTL/miss reason, the policy, and a
bounded newest-first `recent_calls` list. The Stage header and Efficiency card
show the ratio, savings, TTL, breakpoint count, and last miss reason. The CLI
prints the same fields.

### 9. Operator preview and audit

Two read-only RPCs make the model inspectable without a provider call:

- `session.cache.plan` / `vp cache-plan <session> --role <role>` builds the
  stable `core` + `project` sections and tool bundle for each requested role,
  then returns the exact cache plan, component hashes, section sizes, and token
  estimates. It creates no worktree and starts no run. It is the supported way
  to verify that a source/skill edit moved only `project_hash` and that a role
  TTL override is applied.
- `session.cache.audit` / `vp cache-audit <session>` scans a bounded event
  window and reports deterministic cost/latency patterns: more than four
  breakpoints, cache writes with no observed reads, a low hit ratio, project or
  tool hash churn, legacy events without component hashes, 1h TTL without
  reads, a trailing warm, compaction resets, a disabled policy that still
  emitted breakpoints, and a plan/observed breakpoint mismatch. It never calls
  a provider and never infers a provider TTL fact.

Audit findings have `info`, `warning`, or `critical` severity. `clean` means
there are no warning or critical findings; informational observations such as
an expected compaction reset do not fail the audit. The CLI prints the
findings and their evidence; JSON output preserves the full evidence map.

### 10. Cost caveats and provider support limits

- Cache reads are billed at the cache-read price; the hit ratio is not a
  discount.
- Cache writes can be more expensive than uncached input, especially with the
  1-hour pool. The estimated savings math reports negative values honestly.
- Anthropic supports at most four cache breakpoints per request. The plan uses
  four at most. Automatic caching consumes one of those slots.
- A top-level automatic breakpoint and a tail message breakpoint are mutually
  exclusive.
- The 1-hour pool is not universally available. Rejection is surfaced, not
  silently downgraded.
- OpenAI-compatible caching is provider-managed. VerlinPark orders stable
  content first and sorts tools, but cannot guarantee a provider's cache hit,
  minimum prefix length, eviction policy, or TTL.
- Prompt content must remain byte-stable for a cache to hit. Dynamic MCP tools
  are sorted and frozen per run; skill activation does not change the tool
  bundle. User, source, or skill edits intentionally invalidate the relevant
  section.
- Warm calls and compaction summarizer calls consume real tokens and are
  recorded in the normal usage ledger. Warm is never a hidden background call.

## Consequences

- A source or skill change no longer invalidates the role charter; a memory
  brief change cannot invalidate the shared prefix.
- Anthropic long conversations can reuse a moving breakpoint without exceeding
  the four-slot limit. OpenAI-compatible requests preserve a stable prefix and
  a deterministic tool order.
- Operators can see why a call missed, how many breakpoints were requested,
  which TTL was used, and whether the estimated savings are positive.
- The runtime adds no provider call by default. Explicit warm and opt-in
  resume warm are visible and priced.
- Compaction can lower cache-read hits. That is expected and now measured
  instead of being invisible.
- Protocol changes are additive and `serde(default)`-compatible; old session
  events, bundles, and clients still decode.
- The implementation does not claim provider TTL inference, guaranteed cache
  hits, free cache reads, automatic 1h support, or cross-host cache sharing.

## Alternatives considered

1. **One stable system string.** Simpler, but source/skill edits invalidate the
   role charter and there is no way to attribute a miss to project content.
2. **Only explicit Anthropic breakpoints.** Safe, but long conversations lose
   the moving breakpoint and remain largely uncached.
3. **Only automatic Anthropic caching.** Simplest request, but tools and the
   stable sections then share the moving slot and a provider that lacks
   top-level automatic caching gets no stable breakpoints at all.
4. **Automatic 1h TTL with silent 5m fallback.** Rejected: it hides cost and
   support differences and makes cache behavior depend on an unrecorded
   downgrade.
5. **Automatic warm on every run.** Rejected: it adds a provider call and cost
   without operator consent; `warm_on_resume` is explicit and off by default.
6. **Merge all OpenAI system sections into one message.** Rejected for this
   round: separate stable-first messages preserve the exact boundary and let a
   compatible provider cache the stable prefix. The legacy single-message shape
   remains for callers that do not provide sections.

## Tests

The round includes:

- Anthropic wiremock body assertions for automatic and explicit breakpoints,
  the four-breakpoint cap, 5m/1h markers, disabled caching, and actionable 1h
  rejection;
- OpenAI body-ordering, sorted-tool, no-Anthropic-marker, usage, and legacy
  fallback assertions;
- component-hash stability under memory changes and source/skill edits;
- miss-reason derivation for every documented reason;
- savings math including negative and zero cases;
- compaction generation/reset/invalidation persistence and policy gating;
- explicit warm usage/cost/event accounting, role overrides, disabled-policy
  no-op, and opt-in `warm_on_resume` with exactly one stable-section call;
- plan preview without a provider call and audit findings for every documented
  pattern;
- protocol legacy decode and new-field round trips;
- Swift payload decode and Efficiency snapshot propagation, including recent
  calls, TTL, breakpoints, savings, and miss reason;
- existing workspace tests for no extra provider call by default.

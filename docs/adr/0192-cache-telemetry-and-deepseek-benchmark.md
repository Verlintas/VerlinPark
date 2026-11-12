# ADR-0192: Cache telemetry and the DeepSeek prefix-cache benchmark

- **Status:** Accepted
- **Date:** 2026-11-12
- **Supersedes:** none
- **Related:** ADR-0138 (usage accounting), ADR-0168 (cache-first prompt),
  ADR-0188 (instruction store), ADR-0190 (tool bundles), plan L5 in
  `docs/CACHE_LOSSLESS_PLAN.md`

## Context

The lossless-cache plan targets the provider-reported hit ratio that
platform.deepseek.com shows: cache-read tokens divided by total prompt tokens.
The runtime already records cache reads, cache writes, component hashes, a
derived miss reason, and compaction invalidation. It does not record the
per-call uncached remainder, the append-only headroom implied by the previous
prompt, message-list continuity, a cache-epoch timeline, per-role accounting,
or cross-session reuse. The dashboard therefore cannot answer three questions:

1. Did the provider fail to serve a prefix we had already sent?
2. Did our own message list change, or is the miss the expected new tail?
3. How close is a session to the append-only maximum, and what did compaction
   cost?

The existing tests also cannot run a long-prefix scenario without spending real
provider tokens, and the GUI lacks a per-call cache panel.

## Decision

L5 adds observational telemetry and a benchmark. It does not change prompt
assembly, provider serialization, tool bundles, cache epochs, compaction
policy, approvals, budgets, sandboxes, Verifier gates, or merge behavior. All
new protocol fields are additive and `serde(default)`/optional.

### 1. Metric definitions

For one provider call:

```text
input_tokens          = provider total prompt tokens (includes cache buckets)
cache_read_tokens     = provider prompt-cache hit tokens
cache_write_tokens    = provider cache-write tokens (subset of input)
cache_miss_tokens     = max(0, input_tokens - cache_read_tokens)
cache_hit_ratio       = clamp(cache_read_tokens / input_tokens, 0, 1)
theoretical_max_ratio = clamp(min(previous_input_tokens, input_tokens)
                              / input_tokens, 0, 1)
```

- The denominator is **total input**, not uncached input. This matches
  DeepSeek's `prompt_cache_hit_tokens / prompt_tokens` shape. OpenCode2 stores
  uncached input, so its denominator is different; comparisons must relabel.
- `cache_miss_tokens` intentionally subtracts only reads. Cache writes are
  already part of the uncached portion for billing; subtracting them again
  would double-count and would not match the provider's hit/miss split.
- A zero denominator returns `0.0`, never NaN or infinity. Ratios read from
  persisted or provider telemetry are finite-checked and clamped.
- `theoretical_max_hit_ratio` is an in-session upper bound: the previous prompt
  is the longest prefix the provider could have cached locally. It is not a
  TTL, block-size, eviction-policy, or hit guarantee. A first call after a
  provider warm has previous input zero, so the session-local theoretical max
  is zero even when the provider reports cross-session reads.
- Cross-session reuse is reported separately as distinct other sessions and
  observed hit tokens for the same cache epoch. The scan is bounded (5,000
  recent `context.cache` events). It proves observed reuse, not future hits.

### 2. Append-only continuity

The agent loop computes a canonical hash over the message list before sending
the request. The persisted call records:

- `message_prefix_hash`, `message_prefix_count`;
- `previous_message_prefix_hash`, `previous_message_prefix_count`;
- `append_only: bool | null`;
- `previous_input_tokens`.

To prove that the previous list is a prefix, the runtime recomputes the hash
over the first `previous_message_prefix_count` messages of the current list and
compares it with the persisted previous hash. This proves prefix equality
without storing prompt text or the full message list. A first observation has
no previous list and is recorded as `null`, not as a break. A message edit,
reorder, or re-serialization produces `append_only=false`; it is never repaired
by rewriting history.

The first observation after a resumed run can use the persisted previous
event, so continuity survives daemon restarts. The tracker is seeded from the
latest ordinary `context.cache` event; warm calls are excluded from the seed
because they describe the stable prefix only.

### 3. Miss reasons

Bounded reasons:

| reason | meaning |
|---|---|
| `first_call` | no local previous prompt and zero cache reads |
| `tail_delta` | append-only call; uncached tokens fit the new suffix plus 64-token block tolerance |
| `prefix_changed` | core/prefix bytes changed, or the message list was edited/reordered |
| `tools_changed` | frozen tool bundle hash changed |
| `project_changed` | project/source/skill section hash changed |
| `compaction_reset` | the next observation after a successful compaction boundary |
| `provider_cache_miss` | append-only prefix unchanged, but the provider served less than the previous prompt beyond the new tail |
| `unknown` | legacy/inconsistent telemetry |

`ttl_or_provider_miss` remains parseable for legacy events but new calls use
the split reasons. A reason is `null` when the call fully hit or the provider
policy does not expect prompt caching. `tail_delta` is not a cache failure: it
is the expected cost of a new turn.

### 4. Session aggregation

`session.cache` adds:

- total input/read/miss/write, hit ratio, cost, p50/p95 hit ratio and
  theoretical maximum (nearest rank, no interpolation);
- per-role calls, input/read/miss, hit ratio, theoretical average, and ledger
  cost joined by role;
- top miss reasons (count descending, key tie-break, max five);
- a bounded cache-epoch timeline (newest last, max twenty), including calls,
  first/last timestamp, tokens, ratio, and append-only breaks;
- append-only call/break counts and the latest status;
- cross-session reuse sessions, hit tokens, and inspected-call count;
- compaction-invalidated tokens and USD, also exposed as
  `compaction_lost_cache_usd`.

Token totals include warm calls because warm calls are real provider input;
`calls` and `warm_calls` remain separate so ordinary-call counts are not hidden.
Percentiles include calls with provider input; missing/legacy telemetry is
skipped rather than invented. Buckets are deterministically sorted.

The separate read-only `session.cache.calls` RPC returns a newest-first bounded
table (limit 1–100, default 20) with exact role/miss-reason filters and an
opt-in warm-call flag. It reports `observed`, `returned`, and `truncated`
against the filter, scans at most 2,000 recent cache events, and never returns
prompt text. `vp cache-calls` renders the same table. The Efficiency card uses
`session.cache.calls` when present and falls back to the bounded list on
`session.cache` for older daemons.

`project.cache` / `vp project-cache` adds a read-only project-wide aggregate
over a bounded scan of the project's `context.cache` events (default 5,000,
max 50,000). It reports sessions, calls, token totals, hit ratio, p50/p95,
append-only calls/breaks, epoch count, top miss reasons, and a truncated flag.
It intentionally includes every session in the project, but returns no session
ids or prompt text. The scan is best-effort and is not a complete historical
index.

### 5. Deterministic benchmark

`vp-bench/src/cache_bench` contains a pure simulator:

- 64-token block prefix cache keyed by `(epoch, chain hash)`; a block is
  readable only while every prior block matches;
- a 50-turn synthetic development scenario with a stable instruction/repository
  prefix, initial file/tool context, and realistic small append-only deltas;
- a one-time cross-session prime of the stable prefix, excluded from measured
  calls and reported as `warmup_included=false`;
- scenario size checks: at least 200,000 total prefix tokens, at least 20
  turns, mean turn delta ≤1,024 tokens, initial context ≤half the stable
  prefix, and stable prefix ≤400,000 tokens (twice the floor). The check
  rejects a short chat or a filler-inflated prefix instead of gaming the ratio;
- assertions in tests: hit ratio ≥99.5%, zero `prefix_changed` breaks, every
  turn after the first append-only, and first-call miss bounded by the initial
  context plus one block;
- two negative controls: `--cold-start` skips the previous-session prime, and
  `--prefix-break` mutates one early conversation token mid-session. Neither
  can claim the long-prefix target; they exist to prove that the simulator and
  miss attribution detect a real cold start and a real break;
- an explicit `CacheBenchAcceptance` verdict (`stretch`, `pass`,
  `below_floor`, `unrealistic`, `cold_start_control`,
  `prefix_break_control`, or a provider-estimated variant) that records the
  measurement source and the 99.84/99.5/99.0/95 targets. The verdict is
  computed, never copied from the ratio alone;
- a `validate()` check on loaded reports: schema, turn sequence, token
  identities, ratio math, aggregate consistency, first-call continuity, and
  acceptance agreement. A tampered or hand-edited report fails closed;
- JSON, Markdown, and per-call CSV output. CSV uses the same math and contains
  no prompt text;
- `vp cache-bench matrix` runs the warm baseline plus both controls with the
  same shape and prints a comparison table; `--output` writes three JSON/
  Markdown pairs;
- `vp cache-bench verify` gates a saved report by floor, provider-reported
  usage, and control status. It exits 0 only when the report validates and all
  requested checks pass; a simulated report cannot satisfy
  `--require-provider-reported`, and a control cannot pass without
  `--allow-control`.

The report contains per-call input/read/miss/hit/theoretical/append-only/miss
reason/cost rows, aggregate p50/p95, cost breakdown (uncached input, cache read,
cache write, output, total, no-cache comparison, savings), the scenario check,
and caveats that the simulator tokenizer is synthetic. Reports are
byte-deterministic for the simulator and refuse mismatched comparisons by
default (schema, scenario, mode, provider, model, block size, turns, prefix
shape, price profile).

`vp cache-bench` runs the simulator with no daemon and no network. `vp
cache-bench compare` refuses apples-to-oranges reports unless
`--allow-mismatch` is passed, in which case it labels the refusals.
`scripts/cache-benchmark/opencode2_baseline.sh` extracts the aggregate
OpenCode2 baseline with read-only SQL and never selects user content.

### 6. Live mode

Live mode is explicit and never default:

- requires `--live --allow-live`;
- requires `VERLINPARK_LIVE_API_KEY` or `DEEPSEEK_API_KEY`;
- base URL: flag, `VERLINPARK_CACHE_BENCH_BASE_URL`, or
  `DEEPSEEK_BASE_URL`, defaulting to `https://api.deepseek.com` only in live
  mode;
- model: flag, `VERLINPARK_CACHE_BENCH_MODEL`, or `DEEPSEEK_MODEL`; there is no
  hidden model default;
- an optional `VERLINPARK_CACHE_BENCH_LIVE=0` disables live mode even with a
  key;
- an optional `--max-live-cost-usd` stop-loss checks the accumulated estimate
  plus the previous call's size before each provider call. It is a guardrail,
  not a provider-side budget or a guarantee against the next call's variance.

The live run is a controlled multi-turn conversation with a stable instruction
block, bounded output, and no tools. It records provider-reported usage,
first-token latency, and a cost estimate from explicit or clearly labeled
illustrative prices. Missing usage is marked `estimated_usage=true`; the key
never appears in a report, log, or `Debug` output. The default live smoke has a
small prefix and does not claim the long-context target; a below-target result
is reported as a miss.

### 7. GUI

- `EfficiencyCard` shows hit ratio, read/miss tokens, theoretical maximum,
  append-only counts/breaks, cache epoch and source, p50/p95, top miss reasons,
  per-role accounting, cross-session reuse, compaction lost-cache USD, cost,
  and recent calls with miss/theoretical/append-only markers.
- The Stage context popover uses a `CacheTelemetryPanel` with the same values
  and an explicit "observed, never guaranteed" caveat.
- The Stage header, composer footer, and dashboard header resolve the model
  from daemon `agent.info`: `role_models.leader.model` when configured,
  otherwise `agent.info.model`. They never show the local `providerModel`
  UserDefaults value for a live run. Settings continues to edit the local form;
  saving the provider refreshes `agent.info`.
- All state lives in `AppModel`/`Core` models. Views only format values.

### 8. Honesty and limits

- Cache reads are billed. The dashboard labels the ratio as a share, not a
  discount.
- No provider TTL, block size, future hit, or eviction behavior is inferred.
  The block size is a simulator model and a tolerance for `tail_delta`.
- `theoretical_max_hit_ratio` is session-local; cross-session reuse is a
  separate bounded observation.
- Provider usage can be missing or estimated. Reports and telemetry mark the
  estimate; nothing is backfilled from prompt text.
- The simulator is synthetic. Only live mode measures a real provider, and live
  results depend on the provider's cache state at that moment.
- The cross-session scan is bounded and best-effort; it is not a complete
  historical index and does not count as a distributed cache guarantee.
- Compaction lost-cache USD is an estimate from configured prices and token
  counts, not an invoice line.
- Legacy events without the new fields decode with defaults and are reported as
  missing continuity telemetry, not as a break.
- The audit reports `append_only_prefix_break`, `cache_epoch_churn`,
  `theoretical_cache_headroom_gap`, long-context target misses,
  `missing_append_only_telemetry`, `stable_section_changed` (first key/hash
  diff between the latest prompt-prefix events), and a placeholder-epoch
  notice. It never auto-repairs history and still does not claim a TTL fact.

## Non-goals

- No prompt/provider/tool/epoch/compaction behavior change in L5.
- No automatic cache warm; warm remains explicit or opt-in by policy.
- No hidden model routing, provider fallback, or model selection.
- No fabricated usage, filler-prefix ratio inflation, or retroactive metric
  repair.
- No cross-host cache sharing, distributed epoch registry, or provider TTL
  emulation.
- No claim that a high benchmark ratio proves a future hit or a lower bill.
- No gate changes: approval, Seatbelt, path jail, write scope, Verifier,
  Integrator, capacity, budget, and sandbox behavior are untouched.

## Consequences

- Dashboards gain an auditable explanation of cache misses and a reproducible
  benchmark without spending tokens.
- Telemetry grows by bounded per-call fields and a bounded cross-session scan.
  The call list remains capped at 20; the epoch timeline at 20; the scan at
  5,000 events.
- `append_only` can be `null` for the first call or legacy data. Consumers must
  treat null as unknown, not as a break.
- L4 may replace the epoch placeholder with the finalized epoch; the protocol
  field and display stay valid because L5 records provenance.
- Live mode adds an explicit operator path with real cost; it is not part of CI
  and must remain skipped without credentials.

## Operational verification

Ordered checks for an operator or CI:

1. `cargo test -p vp-bench cache_bench` — deterministic simulator, the 99.5%
   long-prefix assertion, both negative controls, report validation, CSV,
   live-gate, injected-provider, and cost-guard tests. No network.
2. `cargo test -p vp-core prompt_cache cache_audit` — metric math, theoretical
   max, clamping, append-only proof, miss attribution, epoch bounds, audit
   findings, and legacy payloads.
3. `vp cache-bench --output /tmp/cache-bench` — human Markdown plus JSON/MD
   artifacts. Check `acceptance.verdict`, not just the ratio.
4. `vp cache-bench matrix --output /tmp/cache-matrix` — warm baseline, cold
   start, and prefix break. The controls must miss the floor and must report
   `cold_start_control`/`prefix_break_control`.
5. `vp cache-bench verify /tmp/cache-bench/cache-bench-simulated.json` — CI
   gate. Add `--require-provider-reported` only for a live report.
6. `vp cache <session>` and `vp cache-calls <session> --limit 20` — inspect the
   live session aggregate and per-call table. `vp cache-audit <session>` shows
   breaks, epoch churn, headroom gaps, and the first changed section key.
7. `vp project-cache <project>` — aggregate across the project's sessions over
   a bounded scan.
8. `scripts/cache-benchmark/opencode2_baseline.sh` — independent OpenCode2
   baseline. It is aggregate-only and read-only.

A live run is complete only when the provider actually reports usage. A live
smoke with estimated usage is useful for connectivity but cannot satisfy
`--require-provider-reported` and must not be quoted as a provider hit ratio.

## Appendix: protocol examples

Per-call row (no prompt text):

```json
{
  "at": "2026-11-12T10:00:00Z",
  "agent_role": "leader",
  "model": "deepseek-chat",
  "input_tokens": 200000,
  "cache_read_tokens": 199000,
  "cache_miss_tokens": 1000,
  "cache_hit_ratio": 0.995,
  "theoretical_max_hit_ratio": 0.997,
  "message_prefix_hash": "sha256…",
  "previous_message_prefix_hash": "sha256…",
  "message_prefix_count": 42,
  "previous_message_prefix_count": 40,
  "append_only": true,
  "cache_epoch": "sha256…",
  "cache_epoch_source": "prompt_prefix_v3",
  "previous_input_tokens": 198000,
  "miss_reason": "tail_delta"
}
```

`session.cache` aggregation:

```json
{
  "cache_hit_ratio": 0.9971,
  "cache_miss_tokens": 29816,
  "theoretical_max_hit_ratio": 0.9992,
  "hit_ratio_p50": 0.9990,
  "hit_ratio_p95": 0.9992,
  "append_only_calls": 49,
  "append_only_breaks": 0,
  "cache_epoch": "sha256…",
  "cache_epoch_timeline": [{"epoch": "sha256…", "calls": 50, "hit_ratio": 0.9971, "append_only_breaks": 0}],
  "per_role": [{"role": "leader", "calls": 50, "cache_miss_tokens": 29816, "hit_ratio": 0.9971}],
  "top_miss_reasons": [{"key": "tail_delta", "calls": 49}],
  "cross_session_reuse_sessions": 2,
  "compaction_lost_cache_usd": 0.0
}
```

`session.cache.calls` request:

```json
{
  "session_id": "ses-…",
  "limit": 20,
  "role": "leader",
  "miss_reason": "provider_cache_miss",
  "include_warm": false
}
```

The result echoes `observed`, `returned`, `truncated`, and the filters. It is
bounded to 100 rows and scans 2,000 recent cache events.

`project.cache` request:

```json
{"project_id": "prj-…", "limit": 5000}
```

The result is aggregate-only: sessions observed, calls, token totals, ratio,
p50/p95, append-only calls/breaks, epoch count, top reasons, and `truncated`.
The maximum scan is 50,000 events and no session ids or prompt text are
returned.

Benchmark report (abridged):

```json
{
  "schema": "verlinpark.cache-bench/v1",
  "scenario": "long_prefix_50_turn_development",
  "mode": "simulated",
  "cross_session_warm": true,
  "warmup_included": false,
  "deterministic": true,
  "aggregate": {
    "calls": 50,
    "input_tokens": 10214200,
    "cache_read_tokens": 10184384,
    "cache_miss_tokens": 29816,
    "cache_hit_ratio": 0.99708,
    "prefix_breaks": 0
  },
  "acceptance": {
    "measurement": "simulator",
    "provider_reported": false,
    "floor_met": true,
    "stretch_met": false,
    "verdict": "pass"
  }
}
```

A live report with provider usage sets `measurement` to `provider_reported`
and `provider_reported` to true. A missing-usage live report sets
`measurement` to `provider_estimated` and labels the affected calls.

## Appendix: why the denominator differs from OpenCode2

OpenCode2's `tokens_input` column stores uncached input. Its displayed ratio is:

```text
cache_read / (uncached_input + cache_read)
```

VerlinPark normalizes provider usage so `input_tokens` is the total prompt
(uncached + cache read + cache write). Its ratio is:

```text
cache_read / input_tokens
```

Both ratios have the same mathematical meaning when the cache buckets are
consistent, but a dashboard must label the denominator. The baseline script
uses the OpenCode2 formula; `session.cache` and `cache-bench` use the total
input formula. Never compare the raw percentages without checking which
denominator produced them.

## Alternatives considered

- **Estimate cache hits from prompt length.** Rejected: only provider-reported
  usage can match the vendor dashboard, and estimates hide eviction.
- **Store full previous message lists for prefix proof.** Rejected: privacy and
  size. A hash over the first previous-count messages is sufficient to prove
  exact prefix equality.
- **Treat every partial hit as `tail_delta`.** Rejected: a provider eviction
  would be hidden. The 64-token tolerance plus the append-only proof separates
  the expected new suffix from a provider gap.
- **Make the benchmark default to live DeepSeek.** Rejected: accidental spend,
  credential exposure risk, and nondeterministic CI.
- **Pad a huge prefix to reach 99.8%.** Rejected by the scenario size check and
  the explicit no-gaming rule.
- **Use the local model setting in the Stage header.** Rejected: it can be
  stale after a daemon restart or role override; the daemon is authoritative.

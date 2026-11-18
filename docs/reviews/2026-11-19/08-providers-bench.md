# Provider & Benchmark Review — `vp-providers` and `vp-bench`

- Date: 2026-11-19
- Baseline: `d239dca` ("Fix live provider compatibility and budget preflight")
- Scope: `crates/vp-providers/src/` (OpenAI-compatible and Anthropic adapters, canonical request projection, cache plan, preemption) and `crates/vp-bench/src/` (cache benchmark, simulator, live runner, agent benchmark runner, metrics, comparisons), plus their integration tests.
- Method: read-only source review. No live provider or network call was made and no API key was used. Offline checks run:
  - `cargo test -p vp-providers` — 136 library tests + 27 `append_only` tests passed; 3 live tests ignored.
  - `cargo test -p vp-bench --lib` — 120 tests passed (about 37 s; the cache simulator dominates).
  - `cargo test -p vp-bench --test harness` — 11 passed, 1 real-provider test ignored.
  - Offline cache-bench CLI reproductions (report generation and `verify` through `/dev/stdin`); no repository files were written by the repros.
- Baseline docs read: `AGENTS.md`, `docs/ARCHITECTURE.md`, `docs/AUDIT.md`, ADR-0168, ADR-0189, ADR-0192, ADR-0229.

## 1. Executive summary

The provider adapters are structurally sound: request serialization is deterministic, the OpenAI empty-`tool_calls` fix is correctly applied, retries are bounded and preemption-aware, error bodies are redacted, and the append-only canonical projection is unusually well tested. The cache simulator's block/epoch math and the cache bucketing/cost arithmetic also hold up under reading and tests.

The remaining risk is concentrated in two areas:

1. **Silent provider completion states.** Neither adapter surfaces `finish_reason` / `stop_reason`. A max-token truncation, content filter, or refusal is indistinguishable from a clean turn. Combined with the runtime's empty-turn path and its silent `{}` tool-argument fallback, this can turn truncated output into a successful run, or into a tool call with wrong arguments.
2. **Benchmark provenance and gate integrity.** A negative control can report `stretch` when its break was never injected; `--require-target` ignores scenario shape and acceptance; `verify` trusts stored acceptance fields (a tampered simulated report passes `--require-provider-reported`); live mode fabricates an `"OK"` assistant turn when the provider returns no text; and provider identity is too coarse for honest A/B comparison.

No P0 security issue, secret leak, hidden model route, or approval/merge bypass was found in this scope. The highest-impact defects are correctness and benchmark-integrity issues, not workspace escape.

## 2. Findings table

| ID | Severity | Confidence | Location | Issue |
|----|----------|------------|----------|-------|
| PR-1 | High | Confirmed | `crates/vp-providers/src/openai.rs:219-260`; `anthropic.rs:462-496`; `crates/vp-core/src/agent/run_loop/body.rs:1189-1205`, `1524-1548`, `1642` | `finish_reason` / `stop_reason` are ignored; truncation/refusal/empty reasoning-only turns are reported as success, and invalid tool JSON becomes `{}` |
| PR-2 | High | Confirmed | `crates/vp-bench/src/cache_bench/live.rs:329-334` | Live cache benchmark fabricates an `"OK"` assistant turn when the provider returns no text, then sends it as real conversation |
| PR-3 | High | Confirmed | `crates/vp-bench/src/cache_bench/simulator.rs:373-403`; `cache_bench/mod.rs:656-678` | `--prefix-break` with `--initial-context-tokens 0` injects no break but is classified as a passing `stretch` run |
| PR-4 | High | Confirmed | `crates/vp-bench/src/cache_bench/mod.rs:514-516`, `633-636`, `1112-1119`, `1222-1236` | `--require-target` and `verify` use only ratio / stored acceptance; shape, control, and `provider_reported` are not recomputed and can be tampered |
| PR-5 | High impact, Medium confidence | Confirmed code; protocol requirement not live-verified | `crates/vp-providers/src/anthropic.rs:49-68`, `282-300`, `312-320`; `crates/vp-providers/src/cache.rs:41-45`; ADR-0168:120-132 | 1h cache TTL is sent without the Anthropic extended-cache beta header; the legacy system path hardcodes a 5m marker when a stable prefix is supplied |
| PR-6 | Medium | Confirmed | `crates/vp-providers/src/anthropic.rs:441-493`, `510-514`; `crates/vp-core/src/agent/provider.rs:307-319`; `openai.rs:258-259` | Anthropic usage parsed at `message_start` is discarded if the stream fails before `message_delta`; multiple usage events are added, so cumulative reports can double-count |
| PR-7 | High impact, Medium confidence | Confirmed serialization; provider requirement not live-verified | `crates/vp-providers/src/canonical.rs:1114-1131`; `crates/vp-core/src/agent/run_loop/body.rs:1888-1902`, `tool_calls.rs:1856-1868` | Parallel tool results are emitted as one user message per result, not one user message containing all `tool_result` blocks |
| PR-8 | Medium | Confirmed | `crates/vp-bench/src/cache_bench/live.rs:269-278`, `412-423`; `crates/vp-providers/src/lib.rs:361-410`; `cache_bench/live.rs:95-97`, `356-361`; `cache_bench/mod.rs:785-814` | A live call with a partial usage object is marked provider-reported; live provider is hardcoded `deepseek` and base URL is not in comparison identity |
| PR-9 | Medium | Confirmed | `crates/vp-bench/src/metrics.rs:21-61`; `scenario.rs:233`; `report.rs:87-133`, `349`; `cache_bench/mod.rs:1026-1045` | Agent benchmark pricing has no provenance; real-provider runs silently use the benchmark default price profile; partial cache-bench overrides label all four prices `Caller` |
| PR-10 | Medium | Confirmed | `crates/vp-bench/src/runner.rs:616-649` | Merge wait returns after 2 s without a new queue entry; a slow Integrator turns a solved run into `merge_expected_missing` |

Additional confirmed lower-severity findings are listed in §4.

## 3. Detailed findings

### PR-1 — Provider completion reasons are discarded; truncation and empty turns look successful

**Severity:** High · **Confidence:** Confirmed

**Evidence.**

- The OpenAI loop reads `delta.content`, `delta.tool_calls`, and `usage`, but never reads `choices[0].finish_reason` (`crates/vp-providers/src/openai.rs:219-260`). `[DONE]` is treated as the only completion signal (`openai.rs:200-203`).
- The Anthropic loop reads `message_delta.usage` but not `message_delta.delta.stop_reason` (`crates/vp-providers/src/anthropic.rs:462-493`). `message_stop` is treated as clean completion (`anthropic.rs:494-497`).
- The runtime treats an empty text/tool-call turn as a successful final outcome: `crates/vp-core/src/agent/run_loop/body.rs:1189` computes `has_output`; `body.rs:1524-1548` records `model_completed` and returns `LoopOutcome::Final("")` when both are empty.
- Tool arguments are parsed with a silent fallback: `serde_json::from_str(&call.arguments).unwrap_or_else(|_| serde_json::json!({}))` at `body.rs:1642` (and the same pattern in `tool_calls.rs:81`). A truncated `arguments` string is therefore executed as `{}`, not rejected.

**Impact.**

- `finish_reason: "length"` (OpenAI) or `stop_reason: "max_tokens"` (Anthropic) with partial text is persisted as a normal assistant answer. The session continues, and the next turn builds on truncated context.
- A truncated tool call can be executed with empty/default arguments. That is especially dangerous for tools with all-optional arguments or for `write_file`/`edit_file` path/content defaults; at minimum the model's intended action is silently replaced by a different one.
- A reasoning model that emits only `reasoning_content` (intentionally dropped by `openai.rs:219-225`; tested at `vp-providers/tests/append_only.rs:407-459`) and then `[DONE]` produces no text and no tool call. The runtime reports `Final("")` as success with no answer. A content-filter/refusal response behaves the same way.
- Cost/usage can still be recorded for a turn that produced no usable answer, so the benchmark and usage ledger can show spend with no output.

**Reproduction / check.** Add a wiremock SSE body with `delta.content: "partial"`, `finish_reason: "length"`, then `[DONE]`. The adapter yields `TextDelta("partial")`, then `Done`; it does not yield an error or completion-reason event. For Anthropic, send `message_delta` with `delta.stop_reason: "max_tokens"` and `usage.output_tokens: 1`, then `message_stop`; the adapter yields `Usage`, then `Done`. To see the empty-turn result, use a scripted provider that emits only `Done`; `body.rs:1524-1548` returns `Final("")`.

**Suggested fix.** Add a provider-neutral `StreamEvent::Finish { reason }` (or a typed `ProviderError::Truncated` for `length`/`max_tokens`), propagate it through `StreamTurn`, and make `run_loop_body` fail or explicitly mark the message `truncated` instead of calling it complete. At the adapter boundary, reject a `tool_call` whose `arguments` is not valid JSON rather than accepting `{}`; at minimum log a typed `TOOL_ARGUMENTS_INVALID` and fail the turn. For empty turns, return an error or a bounded automatic retry only when there was no provider output and no usage, never `Final("")`.

**Tests to add.** `openai_length_finish_reason_is_not_success`, `anthropic_max_tokens_is_not_success`, `reasoning_only_turn_is_not_success`, and `truncated_tool_arguments_do_not_execute_with_empty_object`.

---

### PR-2 — Live cache benchmark fabricates an `"OK"` assistant turn

**Severity:** High · **Confidence:** Confirmed

**Evidence.** `run_live_benchmark_with_provider` builds the next request from `conversation`, then appends:

```rust
let reply = if usage.text.trim().is_empty() {
    "OK".to_string()
} else {
    usage.text
};
conversation.push(ChatMessage::assistant(reply));
```

`crates/vp-bench/src/cache_bench/live.rs:329-334`.

The provider stream may return no text because it returned only a tool call, only reasoning content, a truncated/failed turn, or an empty completion. The delta parser also explicitly ignores `StreamEvent::ToolCall` (`live.rs:407-411`), so an unexpected tool call is discarded rather than failing the benchmark.

**Impact.**

- The benchmark injects an assistant message that the provider never generated into the next request. The provider then sees a fabricated prior turn, and the measured cache prefix includes bytes that are not evidence of the provider's behavior.
- The report's append-only check is computed before the fabricated assistant is appended (`live.rs:279-290`, `335-337`), so it still reports `append_only=true`; the report cannot distinguish this corruption from a clean run.
- An unexpected tool call is silently hidden, which is exactly the kind of protocol event a live compatibility benchmark should fail on.
- Any cost/cache ratio from subsequent turns is measured on a conversation that has diverged from the real model transcript.

**Reproduction / check.** Inject a provider that returns `Ok(StreamEvent::Done)` (or only `Usage`, or only `ToolCall`) for the first turn. With two turns, inspect the second request's `messages`; it contains an `assistant` message with content `"OK"` that the provider never emitted. No existing test covers this; the injected test provider always returns `"OK"` deliberately (`live.rs:537-546`).

**Suggested fix.** Do not fabricate content. Append only `usage.text`; if it is empty, either fail the live run with a typed error (`LIVE_EMPTY_TURN`) or mark the turn invalid and stop before another provider call. If a `ToolCall` arrives, fail the run (or record it as an explicit unsupported event) because the live smoke advertises no tools.

**Tests to add.** `live_empty_text_does_not_inject_ok`, `live_unexpected_tool_call_fails_the_run`, and a two-turn assertion that the second request's messages are exactly the first request plus the returned text.

---

### PR-3 — Prefix-break control can claim `stretch` when no break was injected

**Severity:** High · **Confidence:** Confirmed

**Evidence.**

- The simulator injects the break only when `initial_tokens` is non-empty: `if turn == break_turn && !scenario.initial_tokens.is_empty()` (`crates/vp-bench/src/cache_bench/simulator.rs:394-403`).
- Acceptance classifies a prefix-break control solely from the observed `aggregate.prefix_breaks > 0` (`crates/vp-bench/src/cache_bench/mod.rs:656-666`). It never checks `scenario.kind` or the scenario name. `prefix_breaks` counts only calls after the first that are not append-only (`simulator.rs:590-594`).
- Therefore a `PrefixBreakControl` run with `--initial-context-tokens 0` has zero breaks and falls through to the normal simulated verdict (`mod.rs:679-690`).

**Reproduction (observed offline).**

```text
vp-bench cache-bench run \
  --prefix-break \
  --initial-context-tokens 0 \
  --stable-prefix-tokens 200000 \
  --turns 20 --json
```

The generated report was:

```text
scenario          = prefix_break_control_50_turn
prefix_breaks     = 0
acceptance.verdict = stretch
scenario_size.passed = true
aggregate hit ratio = 0.999028568...
```

This directly contradicts the documented rule that negative controls "cannot claim the long-prefix target" (ADR-0192 §5 and §8), and `vp cache-bench matrix` only avoids it because its default `initial_context_tokens` is 20k.

**Impact.** An operator can produce a report named `prefix_break_control_*` that claims a stretch pass, and `verify` will accept it as a non-control because the verdict is `stretch`. The result is a false positive for the negative control that is supposed to prove break detection.

**Suggested fix.** Classify `PrefixBreakControl` by `scenario.kind`/`scenario.starts_with("prefix_break")`, not only by observed breaks; require `initial_tokens` non-empty (or a non-zero break index) when `--prefix-break` is selected; fail the run if `prefix_break_injected == false`. Add a `control` flag independent of the ratio.

**Tests to add.** `prefix_break_requires_an_injected_break`: zero initial context must either be rejected at argument validation or produce `verdict=prefix_break_control` with an explicit error, never `stretch`. Also test that `verify --require-target` rejects the control.

---

### PR-4 — Target and verify gates trust stored acceptance and ignore scenario shape

**Severity:** High · **Confidence:** Confirmed

**Evidence.**

- `CacheBenchReport::live_target_met` is ratio-only: `self.aggregate.cache_hit_ratio >= LONG_PREFIX_HIT_TARGET` (`crates/vp-bench/src/cache_bench/mod.rs:514-516`), and `--require-target` uses only that (`mod.rs:1112-1119`). It does not require `acceptance.scenario_shape_valid`, a non-control verdict, or provider-reported usage.
- `validate()` checks many token identities but only compares `acceptance.hit_ratio` with the aggregate (`mod.rs:521-636`, especially `633-635`). It does not call `compute_acceptance()`, does not verify `acceptance.measurement`, `verdict`, `provider_reported`, or `scenario_shape_valid`, and does not verify `prefix_breaks`/p50/p95/theoretical values.
- `verify` trusts the loaded fields: `control = report.acceptance.verdict.contains("control")`, `provider_ok = !require || report.acceptance.provider_reported`, and `passed = floor_met && provider_ok && control_ok` (`mod.rs:1231-1236`).

**Reproduction (observed offline).**

1. `--require-target` on an unrealistic but high-ratio run:

```text
vp-bench cache-bench run \
  --turns 1000 --stable-prefix-tokens 1000 --initial-context-tokens 1000 \
  --allow-unrealistic-scenario --require-target --json
```

Observed: `scenario_size.passed=false`, `acceptance.verdict="unrealistic"`, aggregate ratio `0.997711...`, process exit code `0`. The run cannot support a long-prefix claim, but `--require-target` reports success.

2. `verify` on a tampered simulated report (piped through `/dev/stdin`; no file written):

```text
vp-bench cache-bench run --turns 50 --json |
  python3 -c '...set acceptance.provider_reported=true,
                  acceptance.measurement="provider_reported",
                  acceptance.verdict="pass"...' |
  vp-bench cache-bench verify /dev/stdin \
    --require-provider-reported --json
```

Observed: `hit_ratio=0.99708...`, default floor `0.995`, `provider_ok=true`, `provider_reported=true`, `passed=true`, exit code `0`, even though `mode=simulated` and every call was generated by the simulator. `validate()` accepted the report because the acceptance ratio still matched the aggregate.

**Impact.** The CI gate can be satisfied by a report whose acceptance metadata was edited, and `--require-target` can exit successfully for a control, an `unrealistic` scenario, or a provider-estimated live report. This is a benchmark-integrity defect, not just a presentation issue; it contradicts ADR-0192 §5's statement that a tampered or hand-edited report fails closed.

**Suggested fix.** On load, recompute `compute_acceptance()` from `mode`, calls, scenario_size, and prefix breaks and reject any mismatch. Make `--require-target` require `scenario_shape_valid && !control && !estimated && verdict in {pass, stretch}` (or define a separate `--allow-shape-override`). Make `verify` recompute the control/provider status and require `acceptance.measurement` consistency. Add `pricing.validate()` and the missing aggregate consistency checks while there.

**Tests to add.** `verify_rejects_tampered_provider_reported`, `require_target_rejects_unrealistic`, `require_target_rejects_control`, `verify_rejects_acceptance_verdict_mismatch`, and `verify_rejects_missing_scenario_shape`.

---

### PR-5 — 1h cache TTL: missing beta header and a legacy 5m marker

**Severity:** High impact · **Confidence:** Medium (code is confirmed; the current Anthropic API beta requirement was not live-verified here)

**Evidence.**

- `CacheTtl::OneHour.cache_control()` emits `{"type":"ephemeral","ttl":"1h"}` (`crates/vp-providers/src/cache.rs:41-45`).
- The Anthropic request sets only `x-api-key`, `anthropic-version`, and `content-type` (`crates/vp-providers/src/anthropic.rs:312-320`). No `anthropic-beta: extended-cache-ttl-2025-04-11` (or equivalent) header is sent.
- ADR-0168 explicitly says "No beta header is added implicitly" and relies on an actionable error (`docs/adr/0168-cache-first-prompt-architecture.md:120-132`). ADR-0229 does not add live 1h validation.
- Separately, the legacy system path calls `anthropic_system()`, whose marker is hardcoded to 5m: `json!({"type": "ephemeral"})` at `anthropic.rs:60-62`, while the caller already has a 1h plan at `anthropic.rs:282-286`. `anthropic_system_sections()` correctly uses `plan.ttl` (`anthropic.rs:144-152`), so section-aware requests and legacy requests behave differently. This only applies when a stable prefix is supplied; without one, the legacy path emits no system marker at all.
- The existing test `explicit_one_hour_ttl_is_sent_on_every_marker` uses `cache_request()` with `system_sections` populated, so it never exercises the legacy path.

**Impact.** On an Anthropic endpoint that beta-gates 1h cache, every 1h request fails with an HTTP error; the adapter returns an actionable `Unsupported`, but the advertised 1h option is not actually usable. On an endpoint that accepts 1h without the header, a request using the legacy system path still sends a 5m marker, which is a silent TTL downgrade and can mix 5m and 1h markers in one request. That contradicts the ADR's "1h is sent on every explicit marker" contract and can produce different cache billing/retention than the operator selected.

**Reproduction / check.** Capture the Anthropic request with a 1h policy, a stable prefix, and no `system_sections`: the `system` block and tool block use different TTLs. A wiremock assertion on `x-api-key`/`anthropic-version` also confirms there is no beta header. Against the official API, the 1h request returns a 4xx and is converted to the actionable error at `anthropic.rs:326-335`.

**Suggested fix.** If the target API still requires the beta header, send it only when `CacheTtl::OneHour` is explicitly configured (never implicitly for 5m), or remove the 1h option until it can be validated. Thread `plan.ttl` into `anthropic_system()` (and any legacy marker helper) so every explicit marker uses the selected TTL. Add a live-or-wiremock test that asserts the header and TTL on both section-aware and legacy paths.

**Tests to add.** `one_hour_sends_the_required_beta_header`, `one_hour_legacy_system_marker_uses_1h`, and `rejected_one_hour_is_actionable_and_not_retried` (the last already exists for the body path).

---

### PR-6 — Anthropic usage is lost on early stream failure and multiple usage events are additive

**Severity:** Medium · **Confidence:** Confirmed

**Evidence.**

- Anthropic `message_start` parses `input_tokens`, `cache_read_input_tokens`, and `cache_creation_input_tokens` into local variables (`anthropic.rs:441-461`) but emits no `StreamEvent::Usage`.
- Usage is emitted only inside `message_delta` (`anthropic.rs:462-493`). If the stream errors or closes after `message_start` but before `message_delta`, the adapter emits no usage and then the end-of-stream error at `anthropic.rs:510-514`. The runtime computes an estimated input count from the request (`body.rs:1191-1210`) and records it when there is partial output, even though the provider already reported the input count.
- The same empty usage state is used by the retry gate: `body.rs:326-350` retries a stream error if no output and no usage. An Anthropic stream that has sent `message_start` and then stalls can therefore be retried, duplicating the already-billed input request.
- Usage events are additive in `crates/vp-core/src/agent/provider.rs:307-319` (`turn.usage.add(...)`). The OpenAI adapter yields usage for every chunk containing a usage object (`openai.rs:258-259`), and the Anthropic adapter yields one Usage per `message_delta` (`anthropic.rs:487-492`). A provider that sends cumulative usage more than once is double-counted. The new usage contract does not distinguish "this chunk's delta" from "cumulative final".

**Impact.** Token/cost accounting can undercount a failed Anthropic call (estimated instead of provider-reported) or overcount a provider that repeats cumulative usage. A retry after `message_start` can pay for the same prompt twice without recording the first attempt. Cache-hit ratios use the same totals, so the cache benchmark can be wrong in the optimistic direction.

**Suggested fix.** Separate "usage delta" from "final usage" in the stream contract (or make `Usage` explicitly final/cumulative), emit a final usage snapshot at stream end/error if `message_start` data was seen, and have the runtime use the last cumulative snapshot rather than `add` for providers that report cumulative usage. At minimum, expose `StreamEvent::Usage` as cumulative and document it; then change core to `set`/`max` instead of `add`, or have adapters emit deltas explicitly. Do not let an Anthropic stream that already reported input usage retry as if it had no usage.

**Tests to add.** `message_start_without_message_delta_preserves_usage`, `anthropic_usage_is_not_added_twice`, `openai_repeated_usage_is_not_double_counted`, and `anthropic_stream_retry_after_message_start_is_suppressed`.

---

### PR-7 — Parallel tool results are serialized as multiple Anthropic user messages

**Severity:** High impact · **Confidence:** Medium (serialization is confirmed; the exact current Anthropic validation rule was not live-verified)

**Evidence.**

- `anthropic_history_message` maps each `role == "tool"` message to its own `role: "user"` message with a single `tool_result` block (`crates/vp-providers/src/canonical.rs:1114-1131`).
- The runtime pushes one `ChatMessage::tool(...)` per completed call (`crates/vp-core/src/agent/run_loop/body.rs:1888-1902`; sequentially in `tool_calls.rs:1856-1868`). For a parallel batch, the canonical list therefore becomes `assistant(tool_use, tool_use)` followed by `user(tool_result)`, `user(tool_result)`.
- Anthropic's tool-use contract requires the `tool_result` blocks for one assistant turn to be immediately associated with that turn. Sending one user message per result is the shape Anthropic-compatible APIs commonly reject ("tool_result blocks must be sent in a single user message" / adjacency validation); the repository has no test that asserts the grouped shape. The existing `valid_parallel_tool_calls_have_no_sequence_issues` test (`canonical.rs:2150-2174`) only checks the diagnostic sequence audit, not the Anthropic wire projection.

**Impact.** Any Anthropic session that uses parallel tool calls (a documented capability; manual `delegate_many`, parallel read tools, and Code Mode can produce multiple calls) may be rejected by the provider, or may be accepted only because the gateway coalesces consecutive user messages. If rejected, the run fails after the tools have already executed, and the assistant/tool data must be retried. If coalesced, the canonical hash and the provider wire differ, weakening cache telemetry.

**Suggested fix.** In `anthropic_wire_messages`/`project_history`, group consecutive `tool` messages that follow the same assistant message into one `user` message with an array of `tool_result` blocks, preserving order. Keep the one-result-per-message projection for providers that require it (none currently) behind a capability flag. Add a wire-shape test for two parallel calls and one for parallel results interleaved with a text assistant turn.

**Tests to add.** `anthropic_parallel_tool_results_are_one_user_message`, `anthropic_parallel_tool_results_preserve_order`, and a test that `anthropic_wire_messages` matches the actual HTTP body for a multi-tool turn.

---

### PR-8 — Live usage presence and provider identity are mislabeled

**Severity:** Medium · **Confidence:** Confirmed

**Evidence.**

- `openai_usage` treats absent `prompt_tokens` / `completion_tokens` as `0` rather than absent (`crates/vp-providers/src/lib.rs:361-410`). If a provider returns `{"usage":{"prompt_tokens":123}}` (no completion tokens), the adapter emits `Usage { input:123, output:0 }`; `LiveTurnUsage.reported` is set to true on any Usage event (`cache_bench/live.rs:412-423`), and the report marks the call `estimated_usage=false` (`live.rs:326`). `compute_acceptance` then sees all calls provider-reported (`cache_bench/mod.rs:643-651`).
- The live runner always constructs `OpenAiCompatibleProvider::new("deepseek", ...)` (`live.rs:95-97`) and always writes `provider: "deepseek"` / `scenario: "live_deepseek_controlled_session"` (`live.rs:356-361`), regardless of `--base-url` or model.
- `compare_cache_bench_reports` checks `provider` and `model` but not `live_gate.base_url` (`cache_bench/mod.rs:785-814`). Two live runs against different endpoints with the same model string are therefore "compatible".
- The agent benchmark's `ProviderMetadata` has no endpoint field (`crates/vp-bench/src/report.rs:87-121`), and `ComparisonKey` does not contain provider id/endpoint; its provider identity is only `provider_mode` + `model` plus fixture/acceptance/pricing (`report.rs:124-133`).

**Impact.** A live report can claim `provider_reported=true` when only part of the usage was reported, which is exactly the condition `--require-provider-reported` is supposed to reject. A report against a local mock or a different gateway is labeled `deepseek` and can be compared with an official DeepSeek run as if they were the same provider. The cache comparison then presents an apples-to-oranges delta as compatible.

**Suggested fix.** Track presence per usage field (or distinguish `reported` from `partial`) and make `provider_reported` require both input and output (and any configured cache fields) to be present. Derive the provider id from settings or at least use a neutral `openai-compatible`/`custom` label with the base URL host recorded; include base URL/provider id in the comparison key. For the agent bench, add `base_url`/provider id to `ProviderMetadata` and `ComparisonKey`.

**Tests to add.** `live_partial_usage_is_not_provider_reported`, `live_custom_base_url_is_labeled_custom`, and `compare_refuses_different_live_base_urls`.

---

### PR-9 — Pricing provenance is missing in the agent benchmark and partial in cache-bench

**Severity:** Medium · **Confidence:** Confirmed

**Evidence.**

- `Pricing` has four numeric fields and no source enum (`crates/vp-bench/src/metrics.rs:21-61`). `benchmark_default()` is 3/15/0.3/3.75 and is documented as illustrative (`metrics.rs:55-61`), but `ScenarioConfig::defaults()` uses it unconditionally (`crates/vp-bench/src/scenario.rs:230-233`).
- `vp-bench run --provider real` uses the scenario pricing unless the operator passes overrides (`crates/vp-bench/src/cli.rs:116-127`, `387-400`); there is no requirement to pass explicit prices for a real provider. `RunMetrics.cost_usd` is recomputed from those numbers (`report.rs:308-316`), and `RunRecord.pricing` stores only the values (`report.rs:349`).
- The report caveat says "Caller-supplied prices may not match provider billing" (`REPORT_CAVEAT`), which is misleading when no caller supplied them. `BenchReport` validates only non-negative prices (`report.rs:568`), not provenance.
- Cache-bench has a `PricingSource` enum, but `CacheBenchRunArgs::pricing()` treats *any* one explicit flag as making all four values `Caller`; missing fields silently fall back to 0.28/0.42/0.028/0.28 while the report prints `Caller` (`cache_bench/mod.rs:1026-1045`).

**Impact.** A real-provider run with no price flags can report a cost that is off by an order of magnitude, and the report's caveat does not tell the reader that the values are the benchmark's defaults rather than measured or caller-supplied prices. A cache-bench report with one price flag claims all four are caller-supplied, so a reader cannot tell which numbers were actually chosen. A/B comparisons use the pricing fingerprint, so two equally wrong default profiles look "compatible" and the cost delta is presented as meaningful.

**Suggested fix.** Add a pricing source (`caller` / `illustrative_default` / `environment`) to `Pricing` or to `Report`, require explicit prices for `--provider real` (or require an explicit `--allow-default-prices` acknowledgement), and make cache-bench either reject partial overrides or record per-field provenance. Include the source in the comparison key and in the Markdown output.

**Tests to add.** `real_provider_without_explicit_prices_is_refused_or_labeled`, `partial_cache_bench_prices_are_not_labeled_caller`, and `compare_refuses_different_price_sources`.

---

### PR-10 — Merge wait can declare a slow but successful run unsolved

**Severity:** Medium · **Confidence:** Confirmed

**Evidence.** `wait_for_merges` polls the merge queue, but returns as soon as `entries.len() < expected` and no queue length change has been observed for 2 seconds (`crates/vp-bench/src/runner.rs:628-647`). The session terminal wait can complete before the asynchronous merge/Integrator work has enqueued anything. The default `merge_timeout_secs` is 10 s for scenarios (`scenario.rs:212-214`, `244`), but the 2 s no-change window is much shorter and is not configurable.

**Impact.** A real-provider run (or a run with a slow integration command / slow disk) can finish the session in `idle`, then take more than 2 s to create the merge entry. The benchmark returns an empty/short queue, `classify` records `merge_expected_missing`, and the run is counted unsolved even though the integration later succeeds. This directly distorts pass@k, cost-per-solved, and merge metrics, and it is more likely on the real-provider path the benchmark is meant to measure.

**Suggested fix.** Wait until the expected entries exist and are terminal, or until the merge queue is known quiescent for the session (no pending/in-flight rows) plus a bounded idle window; do not use a 2 s no-new-row heuristic when the expected count is unmet. Make the idle window a scenario option and test with an artificially delayed integration.

**Tests to add.** `slow_merge_after_idle_session_is_not_unsolved`: inject a merge delay greater than 2 s and assert the run is solved and the merge queue is recorded.

---

## 4. Additional confirmed lower-severity findings

| ID | Severity | Location | Issue |
|----|----------|----------|-------|
| PR-11 | Low‑Medium | `crates/vp-bench/src/cache_bench/live.rs:228-241`, `495-498`; `crates/vp-providers/src/lib.rs:228-234` | `LIVE_CALL_TIMEOUT` is dead code; live calls use the provider client's 300 s read timeout and have no explicit 120 s ceiling. The first-call cost guard projects only `previous_input.max(1)+256` tokens, ignoring the ~5k stable prefix, so `--max-live-cost-usd` can be exceeded before the first report. |
| PR-12 | Low‑Medium | `crates/vp-bench/src/provider.rs:710-739` | `MeteringProvider` does not override `stream_chat_preemptible`; the default forwards to `stream_chat`, so the wrapped provider never sees the preemption token. Core still drops the stream between chunks, but the adapter's own request/backoff abort is bypassed in every benchmarked run. |
| PR-13 | Low | `crates/vp-bench/src/cache_bench/mod.rs:521-636`, `848-868` | `validate()` does not check `prefix_breaks`, p50/p95, theoretical max, pricing validity/source, acceptance verdict/measurement, or `cross_session_warm`; `load_cache_bench_report` has no size bound. A legacy report with `acceptance` absent is rejected by `validate()` despite `serde(default)` (the test at `cache_bench/tests.rs:321-334` asserts this failure). |
| PR-14 | Low | `crates/vp-providers/src/lib.rs:262-314`; `crates/vp-core/src/agent/provider.rs:326-350`; `crates/vp-bench/src/runner.rs:266-270` | Pre-stream `timeout`/`connect` retries and core stream retries can duplicate a provider request; the benchmark records only successful usage-ledger calls for real providers but all attempts for the fake provider, so `provider_calls` has different semantics by mode. Failed attempts that were billed are invisible. |
| PR-15 | Low | `crates/vp-providers/src/openai.rs:211-218`, `264-278`; `anthropic.rs:498-514` | An OpenAI in-stream `error` object yields an error and then also yields `ToolCall`/`Done`; an Anthropic `error` event yields `Api{status:200}` and then a second `Stream` error. Core stops on the first error, so the user impact is small, but the stream contract permits contradictory terminal events. An SSE `event: error` whose data is ignored is silently treated as normal if `[DONE]` follows. |
| PR-16 | Low | `crates/vp-providers/src/anthropic.rs:266-290`; `canonical.rs:1007-1033`, `1236-1259` | Anthropic sends no `system` when `request.system` is `None` even if `system_sections` is non-empty, while the canonical projection hashes those sections. A caller that supplies sections without the duplicated combined string gets a wire/hash mismatch. |
| PR-17 | Low | `crates/vp-providers/src/anthropic.rs:70-111`; `canonical.rs:988-1005` | Anthropic tool schemas are sent in caller order; `canonical_tools_hash` sorts and deduplicates. If the caller's order changes, the wire prefix moves while the telemetry `tools_hash` can stay unchanged. OpenAI sorts at `openai.rs:305-325`; Anthropic does not. |
| PR-18 | Low | `crates/vp-bench/src/cache_bench/mod.rs:966-1006`; `simulator.rs:222-237` | `--stable-prefix-tokens` and `--turns` are allocated before the scenario size check, so absurd CLI values can allocate huge vectors before rejection. The `--stable-prefix-tokens` help says "default 160k" while the code default is 180k, and `--allow-unrealistic-scenario` help says "Reject" when the flag allows. |
| PR-19 | Low | `crates/vp-bench/src/cache_bench/mod.rs:392-395` | The Markdown renderer prints `Simulator: prefix-cache-sim/v1` for live reports too; only the `deterministic` flag distinguishes them. |
| PR-20 | Low | `crates/vp-providers/src/openai.rs:299-303`; `crates/vp-providers/src/anthropic.rs:260-265` | The OpenAI adapter sends `max_tokens` when configured, which newer OpenAI reasoning models reject in favor of `max_completion_tokens`; the Anthropic adapter defaults to 4096. There is no capability/fallback for this, and no live test covers an o-series or reasoning model. |
| PR-21 | Low‑Medium | `crates/vp-providers/src/lib.rs:236-238`; `crates/vp-core/src/agent/provider.rs:47-64` | `retryable_status` omits Anthropic's 529 `overloaded_error`; a 529 is treated as a terminal `Api` error even though the provider documents it as transient. The retry list is otherwise duplicated in two places and can drift. |

## 5. Test-coverage gaps

**Provider serialization / streaming**

- No test for OpenAI `finish_reason` `length`, `content_filter`, or `stop`; no test for Anthropic `stop_reason` `max_tokens`, `refusal`, or `stop_sequence`.
- No test for a reasoning-only stream (`reasoning_content` only) or an empty `Done`; no test for `reasoning_content` after a tool call.
- No test for cumulative/repeated usage events or partial usage objects (`prompt_tokens` without `completion_tokens`).
- No test for Anthropic `message_start` followed by an error/EOF before `message_delta`.
- No test for the Anthropic 1h beta header or the legacy 1h system marker.
- No test for parallel `tool_result` grouping; the existing test only checks the diagnostic sequence audit.
- No test for non-empty `content_block_start.input` on an Anthropic `tool_use` block.
- No test for malformed SSE JSON, unknown event types, an SSE `event: error`, a 204/HTML success response, HTTP 3xx, `Retry-After` as an HTTP date, or a 422 `stream_options` rejection.
- No test for retry-attempt limits, duplicate-request counting, or timeout-after-send duplicate risk; no test for Anthropic 529 (`overloaded_error`) or other provider-specific transient statuses.
- No test for `MeteringProvider` forwarding preemption.

**Benchmark integrity / cache**

- No test for `--prefix-break` with zero initial context (the finding above).
- No test for `--require-target` rejecting `unrealistic`, control, or provider-estimated reports.
- No test that `verify` recomputes acceptance or rejects tampered `provider_reported`/`verdict`/`measurement`.
- No test that a live empty text or unexpected `ToolCall` fails instead of injecting `"OK"`.
- No test for live partial usage or a custom base URL/provider identity.
- No test for partial pricing overrides or pricing provenance.
- No test for a merge queue that becomes visible more than 2 s after session idle.
- No test for huge `--turns`/`--stable-prefix-tokens` bounds.
- No test for `LIVE_CALL_TIMEOUT` behavior or first-call cost-guard sizing.
- No test that `load_cache_bench_report` rejects oversized JSON, invalid prices, or inconsistent acceptance metadata.

**Timing assumptions**

- `crates/vp-providers/src/openai.rs:925-969` and `anthropic.rs:1164-1209` assert preemption completes in under 2 s after a 5 s server sleep; a heavily loaded CI can exceed the 2 s assertion even though preemption is correct.
- `crates/vp-providers/src/openai.rs:971-1006` asserts a 40 ms trigger aborts a 2 s retry-after in under 1 s; scheduler delay can make this flaky.
- `crates/vp-bench/tests/harness.rs:190-215` asserts `peak_workers >= 2`; on a constrained single-vCPU runner, four fast fake streams can be scheduled serially. The test passed here, but it is a scheduling-dependent assertion rather than a deterministic invariant.
- `runner.rs:643-647` is a production timing assumption, not just a test: 2 s of no queue change is treated as quiescence.

## 6. Clean areas

- **Deterministic cache simulator math.** 64-token block chaining, epoch scoping, prefix-only reuse, partial-block exclusion, nearest-rank p50/p95, and the warm/cold/prefix-break controls are internally consistent; the 120 `vp-bench` library tests passed.
- **Cache/cost arithmetic.** `input` as total prompt, read/write as disjoint subsets, clamping of inconsistent provider metadata, zero-denominator `None`, and the `read / input` denominator are consistent across `vp-core`, `vp-providers`, and `vp-bench`.
- **Append-only canonical projection.** Head/tail separation, message-list chain hashes, tools hashing, tail fallback, and byte-identical prior messages are well covered by `crates/vp-providers/tests/append_only.rs`.
- **Secret handling.** Provider API keys are never serialized; error bodies are redacted and bounded to 8 KB; `LiveCacheBenchSettings` has a redacting `Debug`; live mode requires explicit acknowledgement and never falls back to simulated success.
- **Retry/preemption boundaries.** Pre-stream 429/5xx/connection retries are bounded, `Retry-After` and exponential backoff are respected (capped), preemption aborts waits and stream reads, and a partial turn is not replayed.
- **No hidden routing.** Both adapters accept the configured model and base URL; benchmark modes require explicit opt-in; the fake provider is deterministic and bounded.
- **Report token identity checks.** `CacheBenchReport::validate` catches inconsistent call math, aggregate totals, miss/read relationships, first-call append-only, and cost totals; `BenchReport` rejects mixed pricing within one report.

## 7. Assumptions and limits

- This review did **not** make a live OpenAI, Anthropic, or DeepSeek call and did not use an API key. The 1h beta-header requirement and Anthropic parallel `tool_result` grouping are based on the documented protocol and code shape, not a live 4xx observation; they are labeled Medium confidence.
- "Confirmed" means the code path was read and, where practical, reproduced offline (the cache-bench target/verify reproductions). "Probable" is not used for any table row; the Medium-confidence rows are confirmed code behavior with an unverified provider acceptance rule.
- The review is against baseline `d239dca`; no repository source, test, or configuration file was modified. The only repository file added is this review; the offline reproductions printed reports to stdout or used `/dev/stdin`.
- The offline test runs passed on the review host. `vp-providers` live tests remain intentionally ignored; the real-provider harness test remains ignored.
- The simulator is synthetic and is not a vendor tokenizer; the review did not attempt to validate any vendor's current cache TTL, block size, or billing.

## 8. Recommended order

1. **PR-1** (silent truncation/empty success) and **PR-2** (live `"OK"` fabrication): both can silently corrupt the conversation/benchmark record.
2. **PR-3/PR-4** (control and gate integrity): these make the benchmark's own pass/fail claims unreliable.
3. **PR-5/PR-7** (Anthropic 1h and parallel tool results): protocol compatibility for the second provider.
4. **PR-6/PR-8** (usage accounting/provenance): cost and cache telemetry correctness.
5. **PR-9/PR-10** and the lower-severity items: benchmark honesty and measurement stability.

No commit was made.

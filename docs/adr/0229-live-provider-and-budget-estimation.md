# 0229 — Live provider compatibility and budget estimation

- Status: Accepted
- Date: 2026-11-18
- Scope: provider wire compatibility, session budget preflight, live validation

## Context

The first real DeepSeek run exposed two issues that deterministic provider
stubs did not catch:

1. `cache-bench --live` sent `"tool_calls": []` on ordinary assistant messages.
   DeepSeek rejects an empty `tool_calls` array even though some
   OpenAI-compatible servers accept it.
2. A normal first prompt with 49 advertised tools estimated ~21k tokens by the
   budget path while context management estimated ~10.5k. A 20k session budget
   failed before the provider call even though the real request was ~11.6k
   input tokens.

Both are compatibility/reliability bugs, not model-routing changes.

## Decision

### Empty tool calls

- `ChatMessage` gains a plain `assistant(content)` constructor.
- The live cache benchmark uses it instead of `assistant_tool_calls(reply, [])`.
- The OpenAI-compatible serializer omits `tool_calls` when the vector is empty.
  Existing real tool-call messages are unchanged.
- A canonical unit test asserts that an empty vector and a plain assistant
  message produce the same wire object, and that a non-empty call is retained.

### Budget preflight estimate

`estimate_request_tokens` no longer uses raw bytes / 2. It now:

- uses the same language-aware `estimate_context_tokens` as context management
  (ASCII ~4 chars/token, non-ASCII ~1.5 tokens/char);
- counts system, message content, tool names/arguments, tool names,
  descriptions, and serialized parameter schemas once;
- adds 25% headroom plus 64 framing tokens.

This remains a conservative preflight estimate, but it no longer over-reserves
large tool bundles by roughly 2x. Actual provider usage still settles the
reservation; a real overrun is still detected after the call.

### Budget semantics

A session token budget is a budget for the **whole session**, not one turn.
Each provider call re-sends the accumulated context, so a 20k or 60k budget can
legitimately be exhausted after one or two turns even when the task succeeded.
The runtime must fail honestly rather than fake completion. Operators should
size budgets for expected turns × context, or use `unlimited` mode when the
task is intentionally open-ended.

## Live validation

Environment: `DEEPSEEK_API_KEY` supplied only as an environment variable;
base URL `https://api.deepseek.com`; model `deepseek-flash`. No key is stored in
the repository, events, logs, or test output.

- Provider stream: 54 chunks, 65 input / 122 output tokens, `DONE`, 1.44 s.
- `vp provider-test`: 585 ms, 48 input / 12 output, OK.
- Small live cache-bench (4 turns, ~4k stable prefix): provider-reported input
  4,746–4,818, cache reads 4,608, 95.64–97.09% hit ratio, theoretical max
  ~99.5%, labels the small-prefix scenario as not a long-prefix acceptance run.
- Full agent E2E (temporary Git repo and temp data dir, `unattended` approvals):
  Leader used `write_file`, wrote `LIVE_SMOKE.md` with exact content
  `live provider smoke ok`, and finished idle with a 120k token budget.
  Provider-reported totals: 3 calls, 35,614 input, 188 output, 34,688 cache-read
  tokens, **97.3999% cache hit ratio**, last miss reason `tail_delta`.
  A second run through `scripts/live-smoke-deepseek.sh` reported 3 calls,
  35,985 input, 405 output, 34,944 cache-read, **97.1071%**; the observed range
  across both runs is 97.1–97.4%. The 20k/60k attempts failed honestly on the
  next-turn reservation after the file had been written; this is expected
  whole-session budget behavior.

## Invariants

- No hidden model or provider routing.
- No key serialization, event payload, log line, or UI display.
- No silent fallback from a failed live run to a simulated success.
- Provider-reported usage remains authoritative; estimates are labeled.
- The 99.84% cache target is not claimed from the small-prefix smoke.

## Verification

- `cargo test -p vp-providers --test live openai_compatible_local_live_streams_text -- --ignored`
- `vp cache-bench run --live --allow-live` with explicit small scenario and
  cost stop
- Full agent E2E against DeepSeek
- `scripts/live-smoke-deepseek.sh` (explicit opt-in, temp data/repo, exits nonzero on any mismatch)
- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`

## Limits

- The estimator is still a heuristic, not a vendor tokenizer.
- The live cache smoke used a small prefix and cannot validate the 200k long
  prefix acceptance target.
- A live full agent run depends on network, provider availability, and budget;
  it is not part of the default CI suite.

# ADR-0094: Provider stream retry and partial settlement

- **Status:** Accepted
- **Date:** 2026-11-07

## Context

Provider startup already retried connection failures and transient HTTP statuses
up to three times (`send_with_retry`). A stream that was established and then
failed, stalled, or was cancelled was handled differently:

- a mid-stream error returned `Err` before the usage ledger was written, so the
  token reservation was released and provider-charged tokens were not counted;
- partial text had already been emitted as `agent.message_delta`, but the
  failure cleared the live transcript and no message recorded what was received;
- a stream that failed before its first event was not retried at all;
- there was no reusable live E2E path for real OpenAI/Anthropic endpoints.

## Decision

1. **Retry only before observable output.** The agent loop retries a stream up
   to three attempts when no text, tool call, or provider usage has been
   received and the failure is transient (`ProviderError::Stream`, transient
   HTTP, retryable API status, or an idle timeout). It never reissues a stream
   after text or tool output because replaying a prompt can duplicate content,
   tool calls, or billing. Startup failures from `stream_chat` are not retried
   again here; the provider's `send_with_retry` already owns that phase.
2. **Settle partial turns honestly.** When a stream fails or is cancelled after
   partial output, the loop records provider-reported usage if available,
   otherwise a persisted estimate derived from the partial text/tool calls, and
   settles the in-flight token reservation. Cost is added to the session budget.
3. **Preserve partial output.** A failed partial turn emits `agent.message`
   with `partial: true`, so the UI keeps what the model produced before the
   failure. The turn still returns an error (or cancellation) and does not run
   tool calls from the incomplete turn.
4. **Do not retry deterministic errors.** Missing API keys, 4xx auth errors,
   parse errors, and unsupported requests fail immediately.
5. **Live E2E is explicit.** `vp-providers/tests/live.rs` contains ignored tests
   for real OpenAI, Anthropic, and local OpenAI-compatible endpoints. They only
   run with `--ignored` and explicit environment credentials, so CI and normal
   test runs stay deterministic. `agent.probe` now also reports stream chunks,
   first-token latency, and whether `Done` was observed.

## Consequences

- A provider that stalls before its first token can now take up to three idle
  timeouts (plus small backoff) before the run fails.
- Once any output exists, a dropped stream fails the turn instead of risking
  duplicate output; the user still sees the partial response and the budget is
  charged for it.
- Provider usage reported as zero with no output is not charged; a provider
  that charged for a request but sent no usage and no output cannot be detected.
- The live tests are not part of CI and require the user's own credentials and
  network; they are a reproducibility tool, not a guarantee of provider uptime.

## Alternatives considered

- **Replay the whole prompt after partial output:** risks duplicated text, tool
  calls, and billing, and would desynchronize the visible transcript.
- **Buffer all output until the stream completes:** hides streaming and makes
  long turns feel stalled.
- **Record estimated usage for every failed connection:** overcharges sessions
  for requests that produced no observable output.
- **Run live tests in CI with repository secrets:** unavailable for this
  project and non-deterministic; explicit local tests are safer.

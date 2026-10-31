# ADR-0061: Transient provider retry

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

A single connection reset, rate limit, or provider 5xx aborted the agent run.
Users had to retry manually even when the failure was clearly transient.

## Decision

1. **Bounded retry.** Provider requests are attempted at most three times.
2. **Retryable failures.** Retry only HTTP `408`, `409`, `425`, `429`, `500`,
   `502`, `503`, `504`, and connection/timeout errors before the stream starts.
3. **Backoff.** Honor `Retry-After` seconds when present; otherwise use
   exponential backoff starting at 200 ms, capped at 4 s.
4. **Rebuild the request.** The request body is rebuilt for each attempt so the
   adapter never replays a consumed body.
5. **Fail fast otherwise.** Auth, validation, parse, and other 4xx errors are
   returned immediately.
6. **Connect timeout.** Both adapters use a 15 s connection timeout.
7. **No mid-stream replay.** Once SSE bytes have been consumed, an error is
   surfaced to the caller; partial output is never silently replayed.

## Consequences

- Transient provider failures recover without user intervention.
- A failed integration/probe can take a few seconds longer before reporting.
- Mid-stream disconnects remain unrecoverable within the request; the session
  keeps its events and can be retried explicitly.
- Retry does not change budgets or the usage ledger; only reported usage is
  recorded.

## Alternatives considered

- **Unlimited retry:** can hang a run and multiply provider load.
- **Retry every status:** repeats permanent auth/validation failures.
- **Retry after partial stream:** duplicates text/tool calls and corrupts
  conversation state.

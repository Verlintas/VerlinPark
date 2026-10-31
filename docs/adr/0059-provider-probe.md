# ADR-0059: Live provider probe

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

`doctor` can report whether a provider is configured, but not whether the
endpoint, key, model, network, or streaming path actually work. Users had to
start a session to discover a bad key or base URL.

## Decision

1. **Tiny live request.** `agent.probe` / `vp provider-test` sends one
   non-tool chat request (`Reply with the single word: OK`), max 16 output
   tokens, no tools, temperature 0.
2. **Bounded.** Start and stream are each limited to 30 seconds; empty text is
   an error.
3. **Report.** Returns provider, model, latency, input/output tokens, and the
   response text. It does not persist a session, message, or usage row.
4. **Settings.** The provider card has a **Test connection** button and shows
   the result or error.
5. **Not a benchmark.** The probe is a connectivity check, not a model quality
   or tool-calling test.

## Consequences

- Bad endpoints/keys/models fail before a session is created.
- The probe consumes a small amount of provider quota but does not affect
  session budgets or the usage ledger.
- It does not test tool calls, streaming cancellation, or long-context limits;
  those remain E2E gaps.
- A provider that returns only tool calls fails the probe by design.

## Alternatives considered

- **Start a real session:** expensive and pollutes state.
- **Only check configuration:** misses bad keys/network.
- **Full tool-calling smoke test:** better coverage but much slower and more
  provider-specific.

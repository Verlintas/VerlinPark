# ADR-0071: Provider stream idle timeout

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

Provider HTTP retry covered failures before the stream started. Once an SSE
stream was established, a stalled endpoint could block a Worker forever, holding
a session run, Worker slot, and global capacity permit.

## Decision

1. **Idle timeout per stream event.** `run_loop` waits at most
   `VERLINPARK_PROVIDER_IDLE_SECS` for each `stream.next()` item.
2. **Default and bounds.** Default 300 seconds; configured values are clamped to
   10–3600 seconds. The agent probe keeps its own 30-second timeout.
3. **Fail the run.** A timeout returns an error from the run loop. The normal
   wrapper emits `agent.failed` and finalizes the session as failed.
4. **No mid-stream retry.** A timed-out stream is not replayed because partial
   text/tool calls may already have been applied.
5. **Cancellation still wins.** The existing cancel subscription remains a
   select branch, so a cooperative cancel does not wait for the timeout.

## Consequences

- A dead provider or network black hole cannot pin a session indefinitely.
- Reasoning models that pause longer than the configured idle interval must
  raise the limit; the default is a trade-off, not a model-specific value.
- The timeout applies between events, not to total request duration; long but
  active streams continue.
- Tool execution time is not covered by this timeout; shell has its own limit.

## Alternatives considered

- **Total request timeout:** kills legitimately long streams.
- **Retry after partial stream:** duplicates output and tool calls.
- **No timeout:** leaves runs and capacity permits stuck.

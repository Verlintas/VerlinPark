# ADR-0053: Session-fair Worker/Labour capacity queue

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

ADR-0043 used Tokio semaphores for daemon-wide Worker/Labour capacity. Tokio's
semaphore is FIFO and has no notion of sessions, so a session that queued a large
batch first could occupy every Worker slot while other sessions waited. That is
unfair even though the global ceiling is respected.

The full global Task-DAG scheduler and persistent leases are still not
implemented. This ADR improves the admission layer without claiming those.

## Decision

1. **Custom fair queue.** Replace the Worker/Labour semaphores with
   `FairCapacity`, a mutex-protected active count plus a waiter queue keyed by
   session ID.
2. **Round-robin grant.** When a slot frees, the queue chooses the oldest waiter
   whose session differs from the session that just received a slot; if none
   exists, it takes the oldest waiter. A single session cannot monopolize the
   queue while another session is waiting.
3. **Cancellation-safe.** A waiter that is cancelled is removed from the queue.
   If the grant raced with cancellation, the received permit is released.
4. **Non-bypass.** `try_acquire` succeeds only when no waiter is queued, so an
   arriving run cannot jump ahead of already-waiting sessions.
5. **Observable.** `agent.info` still reports limit/available. Existing
   `agent.capacity_wait` / `agent.capacity_ready` events remain.
6. **Boundary.** This is an in-process fair admission queue. It is not a
   persisted global Task-DAG scheduler, not priority scheduling, and not a
   cross-process lease manager.

## Consequences

- Multiple sessions share Worker/Labour slots more fairly under load.
- Grant order is deterministic for a given waiter arrival order.
- The queue holds no state across daemon restarts; in-flight calls die with the
  process.
- The fairness rule is session-level, not project-level or task-priority-level.
- The queue adds one mutex and one oneshot per waiting run.

## Alternatives considered

- **Keep Tokio semaphores:** simple but unfair to sessions.
- **One semaphore per session:** cannot enforce a shared global ceiling.
- **Persistent scheduler:** the correct long-term shape, but needs a durable
  queue, task identity, recovery, and priority policy; out of scope here.

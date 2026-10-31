# ADR-0079: Priority-aware global admission and capacity

- **Status:** Accepted
- **Date:** 2026-11-01

## Context

The daemon-wide session semaphore and Worker/Labour capacity controller were
FIFO. Users could not say that one session was more urgent, and a single project
with many queued runs could delay unrelated work.

## Decision

1. **Session priority.** Sessions carry an integer priority from -100 to 100
   (default 0). It is set at creation via `session.create` / `vp session-new
   --priority` and the New Session sheet. It is immutable for now.
2. **Priority admission.** The global session gate is a deterministic in-process
   queue with waiters keyed by session, project, and priority. It admits the
   highest-priority waiter first. Among equal priorities it prefers a different
   session, then a different project, from the last admitted run, then FIFO.
3. **Priority capacity.** Worker/Labour capacity waiters carry the same session
   priority. Slack is granted by priority, with the existing round-robin tie
   break across sessions.
4. **Cancellation.** A queued run that is cancelled is removed from the waiter
   queue; if a permit was granted concurrently, the permit is released.
5. **No aging yet.** Lower-priority waiters can starve under sustained high-
   priority load. There is no aging or deadline promotion.
6. **Not a Task-DAG scheduler.** This orders session runs and capacity slots, not
   individual DAG tasks across sessions. Task batches still dispatch per session;
   there is still no persistent global task queue or persistent global write
   lease.

## Consequences

- Urgent sessions can start and acquire workers before background sessions.
- Equal-priority work still alternates across sessions and projects.
- A burst of high-priority sessions can delay low-priority work indefinitely.
- The gate remains in-process; queued runs survive daemon restart only through
  the existing session recovery path, not through a persisted priority queue.

## Alternatives considered

- **Global Task-DAG scheduler first:** the correct long-term design, but much
  larger. Priority admission is a compatible first step.
- **Project-level priority only:** simpler, but cannot express one urgent session
  inside a normally low-priority project.
- **Strict FIFO with quotas:** fair, but does not let the user express urgency.

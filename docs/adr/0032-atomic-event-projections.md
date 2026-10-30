# ADR-0032: Atomic event + projection writes for critical state

- **Status:** Accepted
- **Date:** 2026-10-30

## Context

The runtime appended an event after updating SQLite for several critical state
transitions, and some status changes had no event at all. A crash between the
projection write and the event write could leave the UI, audit log, and recovery
logic disagreeing. The full event-sourcing rewrite is larger than the current
milestone, but the critical paths can be made transactional now.

## Decision

1. **Critical transitions share one SQLite transaction.** For:
   - session status changes
   - session token usage
   - agent status changes
   - merge queue enqueue / status update / retry

   the projection update and the event insert happen in the same transaction.
2. **The event is the returned unit.** Store methods return the inserted `Event`
   (or `(entry, event)` for queue operations); the Engine broadcasts it after
   commit. There is no separate append call that can be forgotten.
3. **The JSONL mirror remains best-effort.** It is written after commit and a
   failure is logged; SQLite remains authoritative for recovery.
4. **This is not full event sourcing.** Session creation metadata and some
   auxiliary rows still have separate paths. The covered critical projections can
   be checked/repaired from events, but a from-scratch database rebuild is still
   future work.
5. **Test the audit surface.** A regression test reconstructs critical
   projections from the event stream: latest session status, token usage sum,
   latest agent status, and latest merge queue status.

## Replay check (2026-10-30)

`session.replay` / `vp replay` compares the event stream with the current
projections for session status, usage, team plan, todos, messages, agent roster,
and merge queue status/IDs. `--repair` / `session.replay {repair:true}` repairs
the covered projections in one SQLite transaction: status/usage, plan, todos,
messages (when every projected message has an event), agent roster snapshots
(`agent.snapshot`), and queue entries. `--rebuild` forces the same reconstruction
even when the projections look consistent. `session.export` writes a portable
bundle of events and projections. It does **not** implement bundle import or
rebuild session/project creation metadata from zero. A deliberate out-of-band
projection write in the replay test proves that drift is detected and that repair
restores the covered fields.

## Consequences

- A crash cannot leave one of the critical status projections updated without a
  corresponding event.
- `agent.updated` is now an actual event; the app reloads the roster on it.
- Replay/rebuild is still not automatic; the event stream is sufficient to
  reconstruct the covered projections in tests.
- Transaction scope is small and does not block provider/network work.

## Alternatives considered

- **Append event first, then projection:** still two writes; a crash between them
  leaves the opposite inconsistency.
- **Full event-sourced rewrite now:** correct long-term, but too broad for this
  slice and would disrupt every store caller.
- **Outbox queue / background projector:** useful at larger scale, unnecessary
  for local SQLite with a single daemon.

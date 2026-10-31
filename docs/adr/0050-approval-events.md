# ADR-0050: Atomic approval events and replay

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

Approval rows were inserted and then an `approval.requested` event was appended
by the engine; resolution updated the row and appended `approval.resolved`
separately. A crash between the two writes could leave an approval in SQLite
with no matching event, or an event with no projection. Replay also did not
check approvals at all.

## Decision

1. **One transaction.** `create_approval_with_event` inserts the pending row and
   the `approval.requested` event together. `resolve_approval_with_event`
   updates the row and inserts `approval.resolved` together.
2. **Payload.** `approval.requested` contains the full `Approval` plus optional
   agent attribution fields. `approval.resolved` contains `approval_id`,
   `approved`, and `resolved_at`.
3. **Replay.** `session.replay` reconstructs approvals from requested/resolved
   events, including the resolved timestamp. Legacy resolved events without
   `resolved_at` fall back to the event timestamp.
4. **Repair.** `SessionRepair` can replace the approvals projection from the
   event stream; `--repair`/`--rebuild` use it.
5. **Report.** `SessionReplayReport.approvals_matches` joins the consistency
   set. An orphan row or missing row is reported as drift.
6. **No approval creation outside the engine.** Store-level
   `create_approval` remains for store tests and explicit migration paths; the
   engine uses the event-atomic variant.

## Consequences

- Approval projection and event log cannot diverge on a normal write.
- A direct store insert (test or manual repair) is detected by replay.
- Approval resolution is idempotent: a second resolve returns `None` and emits
  no event.
- The event payload includes agent attribution without changing the `Approval`
  JSON shape consumers already decode.

## Alternatives considered

- **Keep two writes and repair later:** leaves a crash window.
- **Put approvals in the session row:** loses per-approval queryability.
- **Ignore approvals in replay:** leaves the projection gap we just closed.

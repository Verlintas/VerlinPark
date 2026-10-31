# ADR-0052: Legacy artifact event backfill

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

Artifact rows created before ADR-0048 have `artifact.published` events with only
ID, name, kind, and agent fields. Replay can compare those legacy events
leniently, but `--repair` deliberately refuses to reconstruct them because the
event does not contain MIME, content, SHA-256, or storage. That leaves old
sessions permanently unable to recover a deleted artifact row.

The artifact row still contains the full data. A one-time backfill can append a
new full `artifact.published` event without changing the row, making future
replay and repair possible.

## Decision

1. **Explicit migration.** `artifact.backfill` / `vp backfill-artifacts
   <session>` scans the session's artifact rows and appends a full
   `artifact.published` event for each artifact that lacks a full publish event.
2. **Idempotent.** An artifact that already has a full event is skipped;
   repeated runs report `backfilled = 0`.
3. **Additive only.** No rows, files, or existing events are modified. The new
   event is appended after the legacy event, and the expected-artifact map uses
   the latest publish event for that ID.
4. **Content source.** The current artifact row supplies content for inline
   artifacts and SHA-256/path for file artifacts. This trusts the row as the
   migration source; it is not a reconstruction from the event log.
5. **Repair after backfill.** Once the full event exists, normal
   `session.replay --repair` can restore a missing row.
6. **Not automatic.** Backfill is not run on daemon startup. Operators choose
   when to grow the event log, and can inspect `backfilled` before relying on
   repair.

## Consequences

- Legacy sessions become replay/repair capable after one explicit command.
- The event log grows by up to the artifact content size; text artifacts are
  capped at 100 KB.
- If the current row is already corrupted or missing content, the backfill
  preserves that corruption rather than inventing data.
- The JSONL mirror and database can temporarily diverge if the command is run
  after manual event tampering; `log-status` exposes that separately.

## Alternatives considered

- **Automatic backfill on startup:** unpredictable event-log growth.
- **Reconstruct from legacy events:** impossible for text/binary content.
- **Leave legacy sessions unrepairable:** the status quo.

# ADR-0048: Artifact replay and repair

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

Artifact rows were projections with only partial publish events. `artifact.pruned`
made deletions auditable, but `session.replay` could not verify artifact state
and `--repair` could not restore a deleted row or its content. That left the
largest remaining exception to the "every state change is an event" invariant.

Text artifacts are small (≤100 KB) and can safely live in the event. Binary
artifacts are content-addressed blobs (ADR-0042); the blob file is the content
store and the event carries its SHA-256 and path.

## Decision

1. **Full publish metadata.** New `artifact.published` events include artifact
   ID, agent, task, name, kind, MIME type, storage, size, SHA-256, path, and the
   inline content for text artifacts.
2. **Event-derived projection.** `session.replay` applies
   `artifact.published` and removes IDs named by `artifact.pruned`.
3. **Full vs legacy comparison.** If a publish event carries MIME, SHA, and
   content keys, replay compares all semantic fields except `path` and
   `created_at`. Legacy events without those keys compare only the fields they
   actually contain (ID, agent, name, kind, storage, size).
4. **Repair only when safe.** `--repair` replaces artifact rows only when every
   expected artifact has full publish metadata. Partial/legacy events are never
   used to overwrite content, because doing so would blank the content column.
5. **Binary blobs.** Replay ignores the absolute path (it is machine-local and
   can change on import). Repair restores the event's path; if the blob is
   missing, it logs an `artifact replay warning` instead of pretending the
   content exists.
6. **Projection field.** `SessionReplayReport.artifacts_matches` joins the
   consistency set. A direct row deletion is reported as drift and repaired when
   full metadata is available.

## Consequences

- New sessions get artifact-level replay/repair for both text and binary
  metadata.
- Legacy artifact events remain readable but cannot be used to rebuild missing
  content; they are warned about and left untouched.
- Replay does not copy blob bytes; it relies on the content-addressed blob
  directory or a bundle import to materialize them.
- `path` is deliberately excluded from equality and `created_at` is event-time,
  so imported artifacts do not produce false drift.

## Alternatives considered

- **Put binary bytes in events:** bloats the event log and duplicates the blob
  store.
- **Rebuild from legacy partial events:** would erase text content.
- **Compare absolute paths:** breaks after bundle import and machine moves.
- **Ignore artifacts in replay:** leaves the known projection gap.

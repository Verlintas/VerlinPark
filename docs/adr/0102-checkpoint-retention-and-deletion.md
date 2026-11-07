# ADR-0102: Checkpoint retention and deletion

- **Status:** Accepted
- **Date:** 2026-11-08

## Context

ADR-0101 introduced checkpoints with snapshot refs under
`refs/verlinpark/checkpoints/...`. Dirty snapshots keep commits reachable, so
refs accumulate even after the user no longer needs the checkpoint. The event
log is append-only, so deleting an event row is not an option. Forks already
created from a checkpoint have their own branch and must remain usable.

## Decision

1. **Tombstone, do not mutate history.** `session.checkpoint.delete` appends
   `session.checkpoint.deleted` with the checkpoint ID, label, commit, and a
   reason (`manual` or `prune`). `session.checkpoints` filters tombstoned IDs.
   Deleting an already-deleted checkpoint is idempotent (`removed: false`).
2. **Release the ref.** Deletion removes
   `refs/verlinpark/checkpoints/<session>/<checkpoint>` when it exists. Clean
   checkpoints point at the session branch commit and have no ref, so deletion
   is a no-op at the Git layer. Existing fork branches keep their commits.
3. **Prune policy.** `session.checkpoint.prune` accepts `older_than_days`,
   `keep_latest`, and `dry_run`. At least one of the first two is required.
   The latest N checkpoints are always protected; when an age is provided the
   candidate must also be older than the cutoff. Candidates are returned even
   for a dry run. Each deletion emits its own tombstone event.
4. **Interfaces.** CLI `vp checkpoint-delete` / `vp checkpoint-prune` and a
   Manage Dash "Session checkpoints" card list, delete, and prune checkpoints.
   The card explains that existing forks are unaffected.
5. **Automatic sweep not included.** This is manual lifecycle control. A
   project-level checkpoint retention policy and daemon sweep can be added
   later with the same tombstone/ref semantics.

## Consequences

- Checkpoint refs no longer accumulate forever when users prune them.
- The event log retains the original checkpoint summary and the deletion
  tombstone for audit; only the active checkpoint list changes.
- A deleted dirty checkpoint commit can become unreachable and be pruned by
  Git GC unless a fork branch or another ref still points at it.
- Pruning is not a merge-back mechanism; forks stay independent.
- UI/CLI deletion is explicit; no automatic retention runs yet.

## Alternatives considered

- **Delete event rows:** violates the append-only event model and breaks
  replay/mirror reconstruction.
- **Delete only the Git ref but keep listing:** users still see unusable
  checkpoints and fork attempts fail later.
- **Delete fork branches with the checkpoint:** loses independent work and is
  unsafe; forks own their branches.

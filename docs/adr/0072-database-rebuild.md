# ADR-0072: Rebuild the database from manifests and event mirrors

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

Session event mirrors already survived a lost or corrupted SQLite database, but
project metadata, policies, and budgets lived only in SQLite. Imported bundles
were a recovery path, but users had to export every session in advance. There
was no “start from zero” database path.

## Decision

1. **Project manifest.** The daemon appends a full project/policy/budget
   snapshot to `projects.jsonl` on startup and after project or policy
   mutations. The latest line per project is authoritative for a rebuild.
2. **Expanded session.created.** New sessions record budgets, base commit, and
   creation time in `session.created`; older events fall back to defaults.
3. **Explicit command.** `vp db-rebuild [--dry-run]` reads the manifest and every
   `sessions/*/events.jsonl`, then:
   - backs up `verlinpark.db` and its sidecars,
   - creates a fresh database,
   - restores projects and policies,
   - reconstructs session rows from `session.created`,
   - restores event tables and rebuilds covered projections through the existing
     replay/repair path.
4. **Nothing else is rewritten.** Worktrees, branches, artifact blobs, and
   mailbox/artifact files remain on disk. Reattach with the existing
   `ensure_worktree`/`rehydrate` paths.
5. **Safe by default.** `--dry-run` validates and reports without moving the
   database. A daemon discovery file blocks the command unless `--force` is
   passed.
6. **Skip what cannot be reconstructed.** Imported archives without a
   `session.created` event, malformed mirrors, and sessions whose project is not
   in the manifest are reported as skipped rather than guessed.

## Consequences

- Projects and sessions can be recovered after losing the database, provided
  the daemon has written a project manifest at least once.
- Old sessions created before the expanded payload lose budgets/base commit in
  the rebuilt row; their events and projections still restore.
- Imported archives are not rebuilt from mirrors; re-import their bundles.
- The backup file is retained; there is no automatic cleanup policy yet.
- Manifest snapshots are append-only and not compacted; the file is small
  relative to event logs but grows with policy changes.

## Alternatives considered

- **SQLite backup only:** good for corruption, useless if the DB is deleted or
  schema-incompatible.
- **Require session bundles:** complete but requires prior per-session exports.
- **Recreate projects from repository paths:** cannot recover IDs, names,
  policies, or budgets reliably.

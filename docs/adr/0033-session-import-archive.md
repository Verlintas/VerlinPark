# ADR-0033: Import session bundles as read-only archives

- **Status:** Accepted
- **Date:** 2026-10-30

## Context

`session.export` can now produce a portable bundle containing the project/session
metadata, event stream, and projections. The next question is how to import it
without pretending that external Git worktrees and historical paths were
restored.

A full working rehydration needs path remapping, branch/base reconstruction, and
safe worktree creation. Silently creating a new worktree from the current HEAD
would make an old session look runnable while its history no longer matches.

## Decision

1. **Import preserves the bundle IDs.** The project and session IDs from the
   bundle are kept so the event stream remains internally consistent.
2. **The project root must exist locally.** Import fails if
   `bundle.project.root` is not a directory. A missing project row is created
   with the bundle's ID/name/root/default branch.
3. **Imported sessions are read-only.** The session is inserted with status
   `imported`, no worktree, and a `session.updated` event for that status. The
   event stream, agents, messages, plan, todos, and queue are imported.
4. **No worktree is created.** The bundle's worktree path is not assumed to exist.
   `session.diff` returns an “imported archive” result, and commit/discard/land/PR
   and agent runs are rejected for imported sessions.
5. **All events are preserved verbatim.** Imported events keep their original
   `seq`; import appends `session.updated` and `session.imported` events after the
   imported stream.
6. **No automatic rehydration.** A future explicit rehydrate flow can map paths,
   recreate worktrees at `base_commit`, and clear the archive flag. Import must
   not silently do that.

## Consequences

- Users can move/archive complete session history between data directories where
  the repository root still exists.
- An imported session is inspectable/replayable but not silently mutated or
  resumed.
- Working import still requires a separate rehydrate step; this is deliberate.
- Projects with the same ID but a different local root are rejected.

## Alternatives considered

- **Create a worktree at current HEAD:** misleading; historical commits/diffs may
  not exist.
- **Remap all worktree paths automatically:** needs explicit source/target
  mapping and branch validation.
- **Drop events and import only projections:** loses the audit trail.
- **Reject import entirely:** leaves export without a restore path.

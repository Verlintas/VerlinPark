# ADR-0056: Explicit terminal worktree cleanup

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

Every session and task creates Git worktrees. Finished sessions keep them forever,
which leaks disk space and leaves many stale branches. Deleting them silently is
dangerous because unlanded commits may still be the only copy of the work.

## Decision

1. **Explicit command.** `project.cleanup_worktrees` /
   `vp cleanup-worktrees <project>` removes only worktrees for terminal sessions
   (`idle`, `failed`, `cancelled`, `landed`). Running/queued/paused sessions are
   never considered.
2. **Age threshold.** `--older-than-days` defaults to 7 days and uses session
   `created_at`. `0` means all terminal sessions.
3. **Dirty protection.** A worktree with any `git status --porcelain` output
   (tracked or untracked) is skipped unless `--force`.
4. **Dry run.** `--dry-run` reports `would remove` without touching the
   filesystem.
5. **Branches kept by default.** Cleanup removes the worktree but keeps the
   session branch. `--delete-branches` is required to delete branches, and a
   branch-delete failure is reported in the entry reason.
6. **Per-entry report.** The result contains scanned/removed/skipped counts and
   per-session status, path, branch, removed flag, and reason.
7. **No automatic cleanup.** The daemon never deletes worktrees on its own.

## Consequences

- Users can reclaim disk space without losing branches by default.
- Dirty worktrees require an explicit `--force`.
- Session rows and events remain; only worktree directories are removed.
- This does not clean scratch directories, artifact materialized cache, or
  unreferenced branches; those remain separate maintenance tasks.

## Alternatives considered

- **Automatic age-based deletion:** risks destroying unlanded work.
- **Delete branches with the worktree:** irreversible by default.
- **Never clean up:** unbounded disk growth.

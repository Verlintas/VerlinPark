# ADR-0104: Fork lineage and branch comparison

- **Status:** Accepted
- **Date:** 2026-11-08

## Context

Forks are independent sessions with their own branches, but users could not see
which sessions came from which checkpoint or compare what a fork actually
changed. `session.diff` compares a session worktree with its own base commit; it
does not compare two session branches. Lineage is already event-backed through
`session.created` fork metadata and source `session.forked` events.

## Decision

1. **Lineage query.** `session.lineage` returns:
   - the direct parent (if the session was forked), including checkpoint and
     commit;
   - sibling forks of the same parent;
   - direct child forks created from this session.
   It reads `session.created.forked_from_*` on the child and
   `session.forked.fork_session_id` on the source. New source fork events carry
   the checkpoint commit; older events fall back to the child's
   `session.created` metadata.
2. **Branch comparison.** `session.compare` takes two sessions in the same
   project, resolves their session branch tips in the project repository, and
   returns `git diff --numstat` file stats plus a unified patch. The patch is
   capped at 200,000 characters and marks `truncated`. No worktree is modified.
3. **Committed state only.** Comparison uses branch commits, not uncommitted
   worktree state. Uncommitted checkpoint snapshots are compared after a fork
   commits or when the fork branch includes the checkpoint commit.
4. **Interfaces.** RPC `session.lineage` / `session.compare`, CLI `vp lineage` /
   `vp compare`, and Manage Dash "Fork lineage" with Open and Compare actions.
   The Compare sheet shows commit IDs, file stats, and the unified patch.
5. **Not merge-back.** This is inspection only. There is no automatic merge of
   a fork into its source; users can open the fork and use the normal
   landing/PR flow on its branch.

## Consequences

- Users can navigate the fork graph without guessing session IDs.
- Comparison is deterministic and read-only; it works even if one worktree is
  missing as long as both branches still exist.
- Deleted branches or sessions outside the same project return explicit errors.
- Large patches are truncated in the UI/RPC; the CLI can still use git directly
  for the full diff.
- Uncommitted changes are not visible in comparison; this is separate from
  checkpoint snapshot semantics.

## Alternatives considered

- **Reuse `session.diff`:** only compares against a session's base, not another
  branch.
- **Compare worktrees directly:** misses branches when worktrees are cleaned up
  and can include transient build outputs.
- **Merge-back in the same slice:** requires conflict/verifier/queue semantics;
  kept separate from read-only inspection.

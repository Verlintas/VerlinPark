# ADR-0105: Explicit fork merge-back

- **Status:** Accepted
- **Date:** 2026-11-08

## Context

Forks and lineage were read-only: users could compare branches but had to
manually merge a fork with Git. A controlled merge-back must preserve the
deterministic Integrator's safety properties: clean source state, no silent
conflicts, rollback on failed checks, and explicit operation status.

## Decision

1. **API.** `session.fork.merge` takes a source session and a direct child
   fork. Only direct children are accepted; both sessions must belong to the
   same project.
2. **Preconditions.** Both sessions must be quiescent (not running/queued/
   paused/waiting_input/imported). The source worktree must exist, be on its
   expected branch, and have a clean tracked/untracked non-ignored status. A
   present fork worktree must also be clean; only committed fork branch changes
   are merged. Missing fork worktrees are allowed if the branch still exists.
3. **Merge.** The source worktree runs `git merge --no-ff <fork-branch>`. On
   conflict the merge is aborted, conflict paths are read from the unmerged
   index, and the result is `status: "conflict"` with the source branch
   unchanged.
4. **Checks.** After a successful merge, `git diff --check` runs against the
   pre-merge commit, followed by the configured
   `VERLINPARK_INTEGRATION_COMMAND` if present. A failed check resets the
   source worktree to the pre-merge commit and returns `status: "failed"`.
5. **Events.** `session.fork.merge` is recorded in the source session with
   pre-merge commit, fork commit, merge commit, status, conflicts, and detail;
   a best-effort event is also recorded in the fork. The fork branch is not
   deleted, so the fork remains usable.
6. **Operation locks.** Source and fork are registered in the run registry for
   the merge duration, blocking prompts, plan runs, and manual compaction.
7. **Interfaces.** RPC `session.fork.merge`, CLI
   `vp fork-merge <source> <fork>`, and Manage Dash Fork lineage child entries
   with a Merge confirmation. The source diff refreshes after a merge.

## Consequences

- Users can bring a fork's committed work back without leaving the app, with
  deterministic conflict/rollback behavior.
- Uncommitted fork changes are rejected; users must commit them into the fork
  branch first.
- A source with local uncommitted work is rejected rather than auto-stashed.
- The fork branch is preserved; merge-back is not a fork deletion operation.
- There is no automatic conflict resolution for fork merges yet; conflicts
  return paths and leave the source unchanged.

## Alternatives considered

- **Reuse the merge queue:** requires synthetic Worker/task/Verifier rows and
  would blur the ownership of a user-created fork.
- **Merge dirty worktrees:** silently excludes or risks uncommitted changes;
  rejected.
- **Force-merge with `-X ours/theirs`:** hides conflicts; rejected.

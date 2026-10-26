# ADR-0020: Worker task worktrees and deterministic integration

- **Status:** Accepted
- **Date:** 2026-10-26

## Context

The four-tier delegation slice runs every role in the session worktree and
serializes all writes. That is safe, but it does not provide the per-task
isolation the product promises, and a Worker's changes are indistinguishable from
Leader or Labour changes until the session ends.

We need real task isolation and a way for the parent agent to observe a Worker's
result, without pretending the parallel scheduler or verifier gate exists.

## Decision

1. **Workers get a task worktree.** When a Manager delegates to a Worker, the
   runtime creates `<data>/worktrees/<project>/<session>/<agent-id>` on branch
   `verlinpark/<session-id>--<agent-id>`, based on the session branch. Labour
   spawned by that Worker inherits the same task worktree. Leader and Manager use
   the session worktree; Managers are read-only.
2. **Worker completion is integrated serially.** After the Worker returns and any
   Labour children have finished, the runtime stages all changes in the task
   worktree and creates an internal task commit if the worktree is dirty. It then
   merges the task branch into the session branch with `--no-ff`. The task branch
   and worktree are retained for audit.
3. **Conflicts fail the Worker, not the session.** On merge failure the runtime
   aborts the merge, marks the Worker failed, and returns the error to the
   Manager. The task branch is left intact for manual resolution. The session
   remains usable.
4. **The session stores its base commit.** `session.diff` now compares the base
   commit to the session worktree, so committed and merged task work remains
   reviewable. Uncommitted state is still reported separately by `git status`.
5. **Commit/discard remain about uncommitted changes.** The explicit Commit and
   Discard actions only apply when `status` is non-empty. Task integration commits
   are internal checkpoints; final landing to the project branch is still a
   manual operation.
6. **Execution stays serial.** This slice does not run Workers in parallel and
   does not use write-scope locks. Parallelism requires the task DAG, a scheduler,
   and a merge queue.

## Consequences

- Worker changes are isolated while they are being made and become visible to the
  parent only after a deterministic integration step.
- A Worker is not independently verified before its branch is merged into the
  session branch. The session branch is not `main`; the user still reviews and
  lands manually. The Verifier/Integrator gate and merge queue are required before
  this can be called a verified merge.
- The session branch accumulates merge commits, so `session.diff` is based on a
  stored base commit rather than `HEAD`.
- Conflicting Worker branches remain on disk. Cleanup and conflict UI are future
  work.
- Task worktrees and branches increase disk usage until session cleanup lands.
- Managers still see the session worktree only after integration; they do not
  inspect the Worker's worktree during the run.

## Alternatives considered

- **Keep the shared session worktree:** no isolation and no place for parallel
  Workers.
- **Leave task branches unmerged:** the parent could not inspect the result in its
  own worktree and every consumer would need a multi-worktree view.
- **Auto-merge before the Worker runs:** impossible; changes do not exist yet.
- **Block integration on a Verifier immediately:** the Verifier role and gate are
  not implemented; a deterministic merge checkpoint is the smallest useful step.

# ADR-0021: Parallel Workers and write-scope validation

- **Status:** Accepted
- **Date:** 2026-10-28

> **Implementation note (2026-10-30):** The parallel Worker cap is 10
> (ADR-0026). Pairwise scope validation is enforced for manual `delegate_many`
> and for TeamPlan batch selection; tasks with empty or overlapping scopes run
> alone. See [AUDIT P0-1](../AUDIT.md).

## Context

Workers now run in isolated task worktrees and integrate serially. The Manager
still delegates one Worker at a time, so independent tasks cannot overlap, and
nothing prevents two Workers from touching the same paths.

We need real parallel Workers without claiming a full DAG scheduler or a
verifier-gated merge queue.

## Decision

1. **`delegate_many` for Managers.** A Manager can submit 1–5 Worker tasks in one
   call. Tasks run concurrently in their own task worktrees. Labour children
   inherit their Worker's worktree and scope.
2. **Parallel tasks must declare write scopes.** Every task in a multi-task call
   needs a non-empty `write_scope`. Scopes are normalized prefixes; pairwise
   overlapping scopes are rejected before any Worker starts. A single task may
   omit the scope.
3. **Scopes are checked before integration.** After a Worker finishes, the runtime
   lists tracked and untracked changes relative to the task base commit. If any
   path is outside the declared scope, the Worker is marked failed, its branch is
   retained, and nothing is merged.
4. **Shared control plane.** Parallel Workers share the session token budget,
   cancellation, provider configuration, and approval queue. Approvals still
   carry the requesting agent's id/name/role.
5. **Deterministic serial integration.** After all Workers settle, successful
   results are integrated in task order with the existing `--no-ff` merge. A
   conflict fails that Worker, not the batch. If any Worker is cancelled or
   exhausts the token budget, the batch aborts without integrating any branch.
6. **No verifier gate yet.** Merge into the session branch is still an
   integration checkpoint, not a verified landing. The session branch is not
   `main`, and final landing remains manual.

## Consequences

- Independent Workers can actually run in parallel while their writes stay in
  separate worktrees.
- Overlapping write scopes are rejected up front; out-of-scope writes are
  detected before merge. Shell commands are covered because the check inspects
  the final changed paths, not only tool parameters.
- Scope declarations are still advisory at edit time; a Worker can write outside
  its scope in the worktree, but the change will not merge.
- Token budget is shared and can be exceeded by concurrent Workers between
  checks; the hard stop happens on the next provider/turn boundary.
- Integration remains serial and happens before any independent Verifier or
  merge queue. Conflicts are surfaced as failed Workers with retained branches.
- The Manager receives a combined summary, not per-task diffs; the session diff
  and event log remain the review surface.

## Alternatives considered

- **Parallelize without scopes:** risk of silent overlapping changes and merge
  conflicts. Rejected.
- **Tool-level path enforcement only:** shell and generated files can bypass path
  checks; final changed-path validation is stronger.
- **Merge as each Worker finishes:** nondeterministic integration order and more
  merge races. Batch settlement then ordered integration is simpler.
- **Add a Verifier in this slice:** valuable, but would combine two large changes
  and obscure which one failed. Deferred.

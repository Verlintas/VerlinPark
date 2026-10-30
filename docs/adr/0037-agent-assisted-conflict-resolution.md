# ADR-0037: Agent-assisted conflict resolution for the merge queue

- **Status:** Accepted
- **Date:** 2026-10-30

## Context

The deterministic Integrator merges verified Worker branches with `--no-ff` and
rolls back on failed checks. When the session branch has advanced, the Worker
branch can conflict. Today the only recovery is `merge_queue.retry`, which
rebases the Worker branch and reports the conflict again; a human must edit the
conflict, commit, and retry.

Conflict resolution is semantic work, not a deterministic operation. Git merge
strategies such as `ours`/`theirs` can silently discard one side. Running an
LLM automatically on every conflict would also spend tokens and bypass explicit
user control.

## Decision

1. **Explicit auto-resolve action.** Add `merge_queue.resolve` / `vp resolve` /
   Queue → “Auto-resolve” for entries in `conflict` state. It is not triggered
   silently by the Integrator.
2. **Resolve on the Worker branch.** The resolver starts a rebase of the Worker
   branch onto the current session branch and leaves conflicts in the Worker
   worktree. It does not rewrite the session branch.
3. **Scoped resolver agent.** A Worker-profile agent named “Conflict Resolver”
   receives the conflicting paths, branch/base commits, and instructions to
   preserve both sides, run relevant checks, and leave resolved files staged.
   The agent cannot delegate and is auto-approved for this explicit run; the
   shell sandbox, credential read-denials, env allowlist, and process-group
   cleanup still apply.
4. **Runtime completes the rebase.** After the agent returns, the runtime stages
   resolved files and continues the rebase. It rejects leftover unmerged paths
   or conflict markers and aborts the rebase on failure, restoring the original
   Worker head.
5. **Re-verify the new commit.** The rebased commit is verified by a fresh
   independent Verifier before merge. The Verifier is auto-approved for this
   explicit path, but still has to inspect the diff/checks and submit a verdict.
6. **Reuse the deterministic Integrator.** On approval, the queue entry gets the
   new commit/base commit and `attempt + 1`, then goes through the normal
   `--no-ff` merge, `git diff --check`, optional integration command, and
   rollback path. On rejection/failure the queue returns to `conflict` and the
   session branch is not changed.
7. **Session safety.** Resolution refuses to start if the session is already
   running, the session worktree is dirty, or the Worker worktree is dirty. It
   reserves a session run slot, uses the session token budget, and participates
   in cancel/pause. The session returns to `idle` after the action.
8. **One attempt per invocation.** Auto-resolve does not loop forever. If the
   session branch advances again during verification, the entry returns to
   `conflict` and can be resolved again.

## Consequences

- Users get an agent-assisted path out of merge conflicts without editing files
  by hand, while keeping the deterministic Integrator and Verifier gates.
- Auto-resolve consumes tokens and requires a configured provider.
- Auto-approval is limited to the explicit resolver and verifier runs; it is not
  a global policy and is not a silent background action. The shell sandbox still
  confines writes.
- Only rebase conflicts are handled. Binary conflicts, semantic conflicts that
  the agent cannot resolve, and conflicts outside the Worker worktree still
  require human intervention.
- Conflict resolution runs serially per session and can take a long time; the
  RPC call waits for completion.
- The queue detail may be overwritten by the subsequent Integrator result; the
  `integrator.conflict_resolution_*` events retain the resolution history.

## Alternatives considered

- **Silent auto-resolution on every conflict:** spends tokens without explicit
  consent and makes failures harder to audit.
- **`git merge -X ours/theirs`:** fast but silently drops one side’s intent.
- **Manual-only resolution:** current behavior; does not use the team’s agents.
- **Resolve directly on the session branch:** risks leaving the shared branch in
  a conflicted state and complicates rollback.

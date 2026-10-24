# ADR-0016: One Git worktree per session

- **Status:** Accepted
- **Date:** 2026-10-24

## Context

ADR-0004 requires every task to run in its own worktree, and the repository
invariant says an agent must not have a write path outside its worktree. The
first Leader loop wrote directly in the project root, which was acceptable only
as a temporary single-agent scaffold and would become unsafe as soon as a second
run or a background command overlapped.

The runtime also needs a stable place to put worktrees without polluting the
user's repository or `.gitignore`.

## Decision

1. **Create the worktree when the session is created.** A session is not created
   unless a linked worktree can be created from the repository's `HEAD`.
2. **Layout.** `<data>/worktrees/<project-id>/<session-id>/leader`, branch
   `verlinpark/<session-id>`. The full session id is used because UUIDv7
   prefixes encode the creation timestamp and collide for nearby sessions.
3. **The worktree is the agent's root.** `ToolContext` points at the worktree.
   Read and write tools are jailed there. The project root is read-only from the
   agent's perspective.
4. **Legacy sessions are upgraded lazily.** If a persisted session has no
   worktree, the next run creates/reattaches it and emits `session.worktree`.
5. **Creation is serialized.** One process-wide git mutex covers worktree
   creation/pruning because worktrees share refs with the main repository.
6. **Retention.** Worktrees are kept after the run for review. Cleanup/merge is a
   later milestone; deleting a session will eventually remove its worktree.
7. **Non-Git folders cannot start sessions.** The error is explicit rather than
   silently falling back to writing the project root.

## Consequences

- The user's checkout stays clean; every agent edit lands on a visible branch.
- The UI shows the session branch and worktree state can be inspected on disk.
- Until the merge queue lands, landing a session is a manual Git operation.
- Session worktrees consume disk and are not garbage-collected yet.
- The branch is per session, not per task. Managers/Workers/Labour will each get
  their own worktree under the session directory when the scheduler lands.
- Repositories without an initial commit cannot start a session. A future
  onboarding flow can offer to create the first commit explicitly.

## Alternatives considered

- **Copy the directory:** loses Git metadata and diverges from the main repo.
- **Write in the project root until M2:** violates the write-isolation
  invariant and makes concurrent runs unsafe.
- **Put worktrees inside the repository:** pollutes search, indexing, and
  commits unless every user adds an ignore rule.

# ADR-0031: Recover interrupted merge queue entries on daemon restart

- **Status:** Accepted
- **Date:** 2026-10-30

## Context

`recover_interrupted_sessions` marked running sessions failed, interrupted
running agents, and denied pending approvals. It did not touch the merge queue.
After a daemon crash, an entry could remain `queued`, `merging`, or `checking`
forever, and the UI would show a merge that could never finish.

The queue is persisted, so recovery can make the uncertainty explicit instead of
guessing whether a merge landed.

## Decision

1. **In-flight queue entries become `interrupted`.** On startup, every entry in
   `queued`, `merging`, or `checking` is updated with status `interrupted`,
   `detail = "daemon restarted during integration"`, and a fresh timestamp.
2. **Recovery is observable.** Each changed entry emits `merge_queue.updated`
   with the entry payload, alongside the existing session/agent/approval recovery
   events.
3. **Interrupted is retryable.** `merge_queue.retry` accepts `interrupted` in
   addition to `conflict` and `failed`; the human-triggered rebase+retry path
   remains the recovery mechanism.
4. **Do not guess merge state.** Recovery does not reset or merge the session
   branch. If a `git merge` was in progress, the worktree may still need manual
   inspection/abort; automatic cleanup is a follow-up.
5. **No auto-retry on startup.** Restart recovery must not spend tokens, push, or
   merge without the user asking.

## Consequences

- A stale queue entry is visible and actionable after a restart instead of
  looking alive forever.
- Entry history is preserved; `attempt` and branch/commit remain.
- Users may need to inspect a session worktree if the daemon died mid-merge.
- The queue is still a projection, not a fully replayable event-sourced store;
  recovery is explicit state repair, not replay.

## Alternatives considered

- **Auto-retry every interrupted entry:** could merge after an unknown partial
  state and spend tokens without consent.
- **Mark queued/merging entries `merged`:** false success.
- **Delete stale entries:** loses audit history and branch references.
- **Full event replay on startup:** the right long-term direction, but a larger
  storage refactor than this recovery slice.

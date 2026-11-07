# ADR-0111: Restore a session worktree to a checkpoint

- **Status:** Accepted
- **Date:** 2026-11-08

## Context

`session.checkpoint` records a snapshot commit and active-conversation metadata,
and `session.fork` can branch a new session from that commit. There was no
supported way to return an existing session's branch and worktree to a
checkpoint. Users could only do this manually with `git reset --hard`, which
bypasses the daemon's operation locks, session state checks, and event log.

## Decision

1. **Explicit RPC.** Add `session.checkpoint.restore` with
   `session_id` + `checkpoint_id`, exposed as `vp checkpoint-restore` and as a
   Restore action in Manage Dash → Checkpoints.
2. **Quiescent, clean source.** The session must not be queued, running,
   paused, or waiting for input; imported sessions remain read-only. The
   worktree must exist, be on the session's expected branch, and have an empty
   `git status --porcelain`. This prevents restoring over uncommitted user work
   or an in-flight agent turn.
3. **Deterministic reset.** Under the engine Git lock, verify the checkpoint
   commit still exists, capture the current HEAD, and `reset --hard` the session
   worktree to the checkpoint commit. The checkpoint commit is already kept
   reachable by its `refs/verlinpark/checkpoints/*` ref.
4. **Append-only evidence.** Append `session.checkpoint.restored` with the
   checkpoint ID, previous commit, and restored commit. The event does not
   rewrite conversation, plans, approvals, task queues, or checkpoints.
5. **Serialize with mutations.** `Agent::restore_checkpoint_now` takes the same
   operation lock as prompt, plan, compaction, and fork operations.
6. **Worktree-only restore.** Restore does not roll back the conversation or
   session metadata. A fork remains the way to resume from a checkpoint's
   conversation snapshot. Existing fork branches are unaffected.
7. **No automatic safety checkpoint.** The UI confirmation names the target
   commit and states that later commits remain recoverable through Git reflog;
   the daemon does not create an implicit checkpoint because that would make a
   destructive-looking action unexpectedly allocate storage.
8. **Read-only preview.** `session.checkpoint.diff` returns tracked-file
   numstat, a 200k-character-capped patch, branch/commit metadata, and a
   `clean` flag comparing the current HEAD to the checkpoint. It requires a
   quiescent session but permits a dirty worktree so users can inspect before
   committing or discarding; working-tree-only changes are intentionally not
   included. Manage Dash uses it from a Preview action before Restore.

## Consequences

- Users can return a session's branch/worktree to a deterministic snapshot
  without manual Git commands.
- Dirty or active sessions fail closed; users must commit/discard or stop first.
- Restore is exact for tracked content because the worktree is required to be
  clean; ignored files and Git refs outside the session branch are untouched.
- The previous HEAD is recorded in the event for audit and recovery.
- Users can inspect the tracked diff before restoring; a dirty worktree is
  reported rather than hidden.
- Conversation restore, plan restore, and remote-branch rewrite remain out of
  scope.

## Alternatives considered

- **Automatically checkpoint before restore:** convenient, but silently grows
  checkpoint storage and can itself fail on a broken worktree.
- **`git clean -fd` after reset:** unnecessary once a clean worktree is
  required, and dangerous for ignored/user files.
- **Reset only the index or worktree:** leaves the branch tip inconsistent with
  the restored state.
- **Reuse `session.fork`:** creates a new session instead of repairing the
  current one.

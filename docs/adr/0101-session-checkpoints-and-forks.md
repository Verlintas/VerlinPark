# ADR-0101: Session checkpoints and forks

- **Status:** Accepted
- **Date:** 2026-11-08

## Context

Compaction changes the active conversation window but not the session branch,
worktree, plan, or history. Users still had no way to explore an alternative
from a known session state without manually creating a branch and copying
messages. Checkpoint/fork was the remaining large gap in the session model.

A fork must be deterministic and must not mutate the source. It therefore needs
both a committed tree and a bounded conversation snapshot.

## Decision

1. **Checkpoint event.** `session.checkpoint` requires a quiescent session
   (draft/idle/failed/cancelled/landed; not running/queued/paused/waiting_input/
   imported) on the expected session branch. The worktree may be dirty:
   a temporary index (`GIT_INDEX_FILE`) stages the tracked working tree plus
   untracked non-ignored files, `write-tree`/`commit-tree` creates a snapshot
   commit with `HEAD` as parent, and `refs/verlinpark/checkpoints/<session>/<id>`
   keeps it reachable. The real index/HEAD/worktree are not modified. Ignored
   files are excluded. The checkpoint captures:
   - the snapshot commit and branch;
   - the active-conversation start/through message IDs (after the latest
     `context.compacted`, if any);
   - the prefixed summary text when the source is in summary mode;
   - the active raw message count, label, and a `dirty` flag.
   It is persisted as `session.checkpoint`; `session.checkpoints` lists them.
2. **Fork event.** `session.fork` creates a new session and worktree from a
   checkpoint commit on a new `verlinpark/<id>` branch. Without an explicit
   checkpoint it creates a fresh checkpoint first. It copies source budgets,
   time/cost limits, and priority; usage and spend start at zero.
3. **Conversation snapshot.** A fork copies only the active conversation at the
   checkpoint (summary plus retained tail), not the full raw history. Copied
   messages get new message IDs and preserve tool-call IDs. The source remains
   unchanged and retains the full audit history. A summary is copied as a
   leading `user` message so all provider adapters keep it; Anthropic strips
   `system` role messages from the request.
4. **Display reconstruction.** Copied assistant text emits `agent.message`;
   copied tool calls emit `tool.started`/`tool.finished` so the fork's chat shows
   the prior active conversation. These are display events; the agent context
   comes from the copied `message.created` rows.
5. **Lineage.** The new session's `session.created` includes
   `forked_from_session_id`, `forked_from_checkpoint_id`, and
   `forked_from_commit`; the new session gets `session.forked`, and the source
   gets a best-effort `session.forked` event pointing at the fork. The new
   session seeds a Leader agent with the source Leader's model.
6. **Not copied.** TeamPlan, todos, merge queue, approvals, mailbox, artifacts,
   usage ledger, and original session events are not copied. A fork starts with
   the snapshotted conversation and committed tree only.
7. **Interfaces.** RPC/CLI (`vp checkpoint`, `vp checkpoints`, `vp fork`) and
   the Stage session menu "Fork Session…" expose the flow. The fork sheet can
   choose the current worktree snapshot or a listed checkpoint and edit the new
   goal. Dirty snapshots are labeled in the picker.

## Consequences

- Users can explore alternatives from a committed, reproducible tree without
  disturbing the source session.
- Dirty worktrees are supported: the checkpoint commit includes tracked and
  untracked non-ignored changes without touching the source index. Ignored
  build outputs are not included, and uncommitted secrets not covered by
  `.gitignore` will be snapshotted if the user checkpoints them.
- Forks are not full history clones: pre-compaction messages are summarized,
  and plan/integration state is intentionally fresh. The source remains the
  audit record.
- Worktree retention still applies to forks; the new branch is independent and
  can be cleaned up under the same project policy.
- Checkpoints are event-backed and survive export/rebuild. A missing source
  message or commit makes the fork fail explicitly.
- This is not compaction checkpoint/resume and does not reset a session in
  place. There is no merge-back of a fork into its source.

## Alternatives considered

- **Fork raw history:** preserves everything but duplicates large histories and
  ignores the source's active compaction window. Rejected for the first
  checkpoint model.
- **Require a clean worktree:** simpler, but forces users to commit half-done
  exploration before forking. Rejected; the temporary-index snapshot gives a
  deterministic commit without mutating the source.
- **Reuse the source branch:** unsafe; two sessions would mutate the same
  branch. Each fork gets its own branch/worktree.
- **Copy plan and integration state:** task worktrees, statuses, and merge
  queues are tied to the source session; copying them would create ambiguous
  ownership. A fork starts a fresh plan.

# ADR-0099: Manual context compaction for idle sessions

- **Status:** Accepted
- **Date:** 2026-11-08

## Context

ADR-0097/0098 compact automatically before a top-level provider call. That
covers long running conversations, but it does not let a user reclaim context
before starting a new task, and it gives no way to inspect or force a summary
from the client. Running a compaction against a live session would be unsafe:
the run loop holds an in-memory conversation and would not notice an event
written by another path.

## Decision

1. **Explicit API.** Add `session.compact` / `vp compact <session>` and a Stage
   context popover action. The response returns the resulting
   `context.compacted` summary (`mode`, counts, before/after tokens) or
   `compacted: false` when the conversation is already too short.
2. **Same pipeline.** Manual compaction calls the existing summary/trim
   implementation with `force: true`; it uses the same model, context window,
   output reserve, usage ledger entry (`compaction`), `context.compacted`
   event, and `context.compact_failed` failure path as automatic compaction.
   Original messages remain in SQLite.
3. **Idle only.** Manual compaction is allowed for draft, idle, failed,
   cancelled, and landed sessions. It is rejected for imported archives,
   queued/running/paused sessions, and sessions waiting on a user question.
4. **Mutual exclusion without pretending to be a run.** The service registers
   a manual `RunControl` in the existing run registry while the summary request
   is in flight, so a prompt cannot start concurrently. The control is marked
   `manual`, which makes `session.pause` return `false` instead of leaving the
   session stuck in `paused` after the compaction ends. Cancellation still
   cancels the summary stream.
5. **Failure visibility.** If the summarizer fails and no trim fallback is
   possible, the RPC returns an error and the `context.compact_failed` event
   remains for replay/diagnostics. A successful summary/trim is immediately
   reflected by `session.context` and the Stage context badge/popover.

## Consequences

- Users can compact a session before a new task without sending a prompt.
- Manual compaction consumes provider tokens and is recorded against the
  session budget; an exhausted budget is reported instead of silently doing
  nothing.
- The popover shows active context, window, summary threshold, hard limit,
  active message count, last compaction, and the manual action.
- This is not checkpoint/fork: it changes the active conversation window, not
  the session branch, worktree, plan, or message history.
- Pausing during a manual compaction is not supported; cancel is.

## Alternatives considered

- **Run compaction inside the next prompt only:** simpler, but cannot be
  requested explicitly and surprises the user with an extra provider call.
- **Mutate a live run's in-memory conversation:** rejected; it would diverge
  from the event log and can lose tool-call ordering.
- **Shell out to a separate compaction process:** duplicates runtime logic and
  would need its own provider/approval/budget path.

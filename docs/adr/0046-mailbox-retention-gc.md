# ADR-0046: Mailbox retention GC

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

ADR-0041 introduced the session-scoped typed mailbox. Messages are pull-based and
persist indefinitely. Long-running sessions with busy Workers can accumulate
thousands of messages, while there was no way to bound storage or apply a
project-level retention policy.

Mailbox deletion is more sensitive than artifact deletion because an unread
message may contain an instruction that has not been consumed.

## Decision

1. **Project policy.** `MailboxPolicy` has optional `max_age_secs` and
   `max_count`, plus `delete_unread` (default false). Zero limits are rejected.
2. **Manual and optional auto GC.** `mailbox.gc` / `vp mailbox-gc` applies the
   policy manually. `auto_gc` (default false) runs the same policy after a
   message is sent or marked read. The trigger is deterministic and uses the
   same unread protection; there is no background timer.
3. **Oldest first.** Candidates are ordered by `created_at`, then ID. Both the
   age rule and the count rule collect the oldest first; duplicates are removed.
4. **Unread protection.** Unless `delete_unread` is true, an unread message is
   skipped even when it is a candidate. GC can therefore leave the mailbox above
   the configured count when unread messages dominate. It reports only actual
   deletions.
5. **APIs and UI.** RPC `mailbox.policy.get/set` and `mailbox.gc`; CLI
   `mailbox-policy`, `mailbox-policy-set`, `mailbox-gc`; Manage Dash has a
   Mailbox retention card with a Delete unread toggle and GC now.
6. **Audit events.** Each deletion emits `mailbox.pruned` with the message ID,
   sender/receiver, and kind in the same transaction as the row delete. The
   count is also returned to the caller. `session.replay` reconstructs the
   mailbox from `mailbox.message_sent`, `mailbox.message_read`, and
   `mailbox.pruned`; `--repair` rewrites the mailbox projection.

## Consequences

- Users can bound mailbox storage deliberately without silently losing unread
  instructions.
- `max_count` is a target, not a guarantee, when unread messages are protected.
- Auto GC runs only at message send/read boundaries; a project with no further
  mailbox activity keeps its last state until a manual GC or the next event.
- Mailbox rows are deleted with the session; exported bundles still contain the
  messages present at export time.

## Alternatives considered

- **Delete unread by default:** risks losing instructions; unacceptable for a
  pull-based mailbox.
- **Auto GC on every send:** could delete messages before an idle agent reads
  them; surprising and hard to audit.
- **Delete by sender or role:** adds policy complexity without solving the
  storage bound as directly.

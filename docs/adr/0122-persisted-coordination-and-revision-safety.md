# ADR 0122: Persisted coordination/decision projections and revision safety

- Status: Accepted
- Date: 2026-11-09
- Area: State / orchestration

## Context

Coordination notes and orchestration decisions were appended as events and
then re-derived from the last event window for `team_status`, Collab, and the
CLI. That worked for small sessions but had three problems:

1. A long session could push old blockers and decisions out of the 5,000-event
   window, so a live blocker could disappear from the UI.
2. Every snapshot rebuilt note resolution by scanning events. The result was
   correct but not queryable, and did not survive a bounded event refresh.
3. Plan revision history exposed metadata only. Users could not inspect the
   stored plan behind a revision, restore it, or edit one task through the
   same safety checks as a full plan revision.

## Decision

1. **Coordination and decisions are SQLite projections.**
   `coordination_notes` and `orchestration_decisions` store the latest
   projection with a sequence number. Writes create the event and update the
   projection in one transaction. `list_coordination_notes*` and
   `list_orchestration_decisions` read the projection, not the event window.
2. **Unblock is a resolution event.** An `unblock` coordination note resolves a
   target note or every open blocker for a task, using the unblock event's
   `created_at` as `resolved_at`. It does not create a new note row. This makes
   replay deterministic.
3. **Targeted delivery is durable.** A coordination note with
   `target_agent_id` also enters the agent mailbox as kind `coordination`.
   Workers receive:
   - notes directly addressed to them;
   - notes on their task;
   - shared handoff/decision/escalation notes;
   and an unread mailbox brief at the start of a plan task. The mailbox copy
   remains readable after the note window moves on.
4. **Replay covers projections.** `SessionReplayReport` now compares notes and
   decisions, and `repair` / `rebuild` / bundle import reconstruct them from
   events. A tampered projection is detected and repaired.
5. **Revision safety is shared.** Full plan revision, one-task edit, and
   revision restore all call the same `ensure_revision_safe` check. Active,
   queued, paused, verifying, merging, completed, or skipped tasks cannot be
   removed or rewritten. Pending/failed/blocked/cancelled work can be revised.
6. **Revision inspection is first-class.** `team_plan.revision` returns the
   stored plan for a revision. `team_plan.restore` creates a new revision from a
   previous plan. `team_plan.task_edit` replaces one task in the current plan
   and records a revision with a reason and optimistic expected revision.
7. **Optimistic concurrency.** All revision mutations accept
   `expected_revision`; a stale client is rejected before any state changes.

## Consequences

- Blockers and controller decisions no longer disappear from long sessions.
- Replay/repair now checks the same projections that Collab displays.
- Targeted notes survive until read, while the task brief stays focused.
- Users can inspect and restore plan history, and edit a task without sending
  the whole plan JSON.
- Projections add two tables and repair paths; imports of archives without
  these events simply produce empty projections.
- Event + projection are atomic in the new write path. Older sessions may need
  `vp replay --repair` once to backfill.

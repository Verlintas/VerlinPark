# ADR-0086: Mutable session priority

- **Status:** Accepted
- **Date:** 2026-11-04

## Context

Session priority was fixed at creation. Users could not promote a queued session
or lower a background session without abandoning it and creating a new one.

## Decision

1. **Event-backed updates.** `session.priority.set` validates `-100..100`, updates
   the session projection and appends `session.priority` in one transaction, then
   publishes the event.
2. **Replay/repair.** `SessionRepair` carries an optional priority derived from the
   latest `session.priority` event. `rebuild_session` and `db-rebuild` therefore
   restore the latest priority instead of the creation value.
3. **Queued sessions.** The global admission queue reads the in-memory session at
   enqueue time; a changed priority affects subsequent admission decisions, not a
   waiter already queued with its old priority.
4. **Surfaces.** CLI `vp session-priority`, RPC `session.priority.set`, and the
   session header priority menu (Low/Normal/High/Urgent).
5. **Reserved tokens.** Changing priority does not move budget reservations or
   interrupt a running provider call.

## Consequences

- Users can reprioritize an idle, failed, or queued session without recreating it.
- Priority changes are visible in the event stream and survive rebuild.
- Already-queued waiters keep their existing effective priority until re-enqueued.
- There is still no deadline, preemption, or task-level global DAG.

## Alternatives considered

- **Recreate the session:** loses worktree, plan, history, and budgets.
- **Projection-only update:** cheap, but `db-rebuild` would silently reset the
  priority to the creation value.
- **Preempt running sessions:** unsafe; provider/tool work is not checkpointed.

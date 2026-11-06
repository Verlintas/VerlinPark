# ADR-0088: Persistent task-DAG queue and global write leases

- **Status:** Accepted
- **Date:** 2026-11-06

## Context

Plan execution kept task status in a per-run `HashMap`, and cross-session
Worker/Labour write scopes were guarded by an in-memory `ScopeLocks` registry.
A daemon restart lost the task queue and every lease, plan resume had to infer
completion from raw events, and there was no global view of ready work or
active write scopes. ADR-0060 added task priority but explicitly limited it to
one ready batch inside one session.

## Decision

1. **Persistent task rows.** The daemon stores one `plan_tasks` row per
   `(session_id, plan_seq, task_id)`. Rows carry department, title, priority,
   status, dependencies, write scope, attempts, enqueue/start/finish times, and
   the last error. The latest `team_plan.submitted` event identifies the active
   plan revision.
2. **Event-derived rebuild.** `sync_plan_tasks` rebuilds a session's queue from
   the active plan plus `plan_task.started` / `plan_task.finished` events. It is
   idempotent and runs on submission and again before a plan resume, so stale
   rows are repaired even if the daemon died between an event and a projection
   update. A started task with no finish becomes `interrupted`.
3. **Task-level ordering.** Ready tasks must have all dependencies
   `completed`. Retryable statuses are `pending`, `interrupted`, `failed`,
   `blocked`, and `cancelled`. Effective priority is
   `clamp(session_priority + task_priority, -100, 100)` plus one point per
   `VERLINPARK_PRIORITY_AGING_SECS` of queue age, capped at +100. Ties use
   enqueue time and task id. Scope-non-overlapping tasks are selected into a
   batch after ordering; empty scopes remain exclusive.
4. **SQLite-backed write leases.** `write_leases` is the authoritative
   cross-session registry for Worker/Labour write scopes. A lease stores the
   project, session, agent, normalized scopes, and acquisition time. Different
   sessions may not overlap; the same session may share its worktree. An empty
   or wildcard scope conflicts with every other session. `ScopeLocks` keeps only
   the in-process notification handle; the production configuration reuses one
   lock registry across provider reconfiguration.
5. **Recovery.** Daemon startup marks running/claimed task rows `interrupted`,
   appends `plan_task.interrupted` events, and deletes all write leases, because
   no agent from the previous process is still running.
6. **Opt-in global resume scheduler.** With `VERLINPARK_TASK_SCHEDULER=1`, a
   background loop selects one idle/failed/cancelled session with retryable
   tasks using effective priority, aging, and a session round-robin tie-break,
   then calls the existing `resume_plan`. A cooldown
   (`VERLINPARK_TASK_SCHEDULER_COOLDOWN_SECS`, default 60) prevents hot retry
   loops. It is off by default because resuming a plan spends the session budget.
7. **Observability.** `task.queue` returns rows plus active leases; `vp tasks`
   and the Manage Dash scheduler card expose them. `Sync_plan_tasks` remains the
   repair point for the operational projection.

## Consequences

- Task state, attempts, priority, and write-scope ownership survive daemon
  restarts and are visible without scraping events.
- Two sessions cannot simultaneously hold overlapping Worker leases, even after
  a provider reconfiguration; stale leases are cleared on recovery.
- A low-priority task eventually overtakes a high-priority sibling; across
  sessions the existing admission and Worker capacity controllers remain the
  execution gates.
- Execution is still session-driven. The opt-in scheduler resumes stored
  plans; it does not run a single task from an inactive session without a plan
  run, and it executes ready tasks in batches rather than a fully autonomous
  per-task dispatcher.
- Task rows are an operational projection, not yet part of `session.replay`
  repair, bundle export, or `db-rebuild`. They are regenerated from the plan
  and plan events on sync. Write leases are ephemeral coordination state, not
  event-sourced audit history; recovery clears them.
- The scheduler and lease registry remain single-daemon (the daemon already
  holds an instance lock). There is no cross-process priority queue.

## Alternatives considered

- **Keep in-memory queues:** simplest, but loses priority/attempt history and
  cannot show a global view after a crash.
- **Store only completed task ids:** enough for resume, but not for fair
  ordering, attempts, errors, or leases.
- **Default-on global dispatcher:** would spend budgets without the user asking;
  opt-in keeps cost behavior explicit.
- **Cross-process SQLite leases:** unnecessary while the daemon owns the single
  instance lock and all shell execution.

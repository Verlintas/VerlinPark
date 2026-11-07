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
5. **Recovery and terminal cleanup.** Daemon startup marks running/claimed task
   rows `interrupted`, appends `plan_task.interrupted` events, and deletes all
   write leases, because no agent from the previous process is still running.
   A session that reaches a terminal state during normal operation also clears
   its own leftover running task rows and appends `plan_task.interrupted`.
6. **Opt-in resume scheduler.** A background loop selects one
   idle/failed/cancelled session with retryable tasks using effective priority,
   aging, and a session round-robin tie-break, then calls the existing
   `resume_plan`. Superseded in ADR-0091 by a persisted per-project
   `TaskSchedulerPolicy`; the original env flag remains as a fallback default.
7. **Observability.** `task.queue` returns rows plus active leases; `vp tasks`
   and the Manage Dash scheduler card expose them. `Sync_plan_tasks` remains the
   repair point for the operational projection.
8. **Manual skip/retry.** A non-active session can retry a
   failed/blocked/interrupted/cancelled/skipped task back to `pending`
   (`plan_task.retried`) or skip a queued task (`plan_task.skipped`). A skipped
   task is terminal for scheduling and is not picked up by the global scheduler;
   its dependants remain blocked. Both transitions are atomic with events and
   replay/repair/bundle aware.
9. **Running task cancel/pause/resume.** `task.cancel` moves a running/paused
   task to `cancelling` (`plan_task.cancelling`); `task.pause` moves a running
   task to `pausing` (`plan_task.pausing`), and the Worker transitions it to
   `paused` at its next boundary; `task.resume` returns it to `running`
   (`plan_task.resumed`). The Worker checks states at turn/tool boundaries; task
   cancellation marks only that task cancelled and continues siblings. Control is
   cooperative: an in-flight provider stream/tool is not preempted mid-call.

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
- Tasks can be skipped, retried, cancelled, paused, and resumed without
  restarting a session; all running-task controls are boundary-based.
- Task rows are part of `session.replay` drift checks and
  `--repair`/`--rebuild`, session bundle export/import, and `db-rebuild`.
  They are regenerated from the plan and plan events. Write leases are ephemeral
  coordination state, not event-sourced audit history; recovery clears them.
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

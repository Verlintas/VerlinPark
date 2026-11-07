# ADR-0095: Per-task dispatch mode

- **Status:** Accepted
- **Date:** 2026-11-07

## Context

The project-level auto-resume policy wakes an idle/failed/cancelled session and
resumes its entire stored TeamPlan. The run is admitted once and then executes
all remaining dependency batches in that session. This is simple, but it gives
an entire plan priority as a unit: a session with many ready tasks can occupy
its Worker slots and budget for a long time before another session gets a turn,
and there is no explicit way to advance just one task.

The task rows, dependency graph, priority/aging ordering, write scopes, global
admission, and Worker capacity already exist. The missing piece is a scheduling
mode that claims and runs one ready task, then releases the session.

## Decision

1. **Opt-in per project.** `TaskSchedulerPolicy` gains `dispatch_tasks`
   (default false). It is persisted in `project_task_scheduler_policies`,
   included in `projects.jsonl`, exposed through
   `project.task_scheduler.get/set`, `vp task-scheduler-set --dispatch-tasks`,
   and the Manage Dash task-scheduler card. The existing behavior remains the
   default.
2. **One ready task per tick.** When the policy is enabled in this mode, the
   global scheduler calls `dispatch_next_task(session_id)` instead of
   `resume_plan`. That path uses the same admission gate, session status
   transitions, session budget, plan sync, and dependency ordering, but limits
   `execute_team_plan` to one claimed task; after it settles the session returns
   to idle. The scheduler can pick the next task on a later tick.
3. **No plan-wide abort propagation.** In limited mode, cancellation or budget
   exhaustion settles the current task and returns; it does not mark every
   remaining task cancelled/blocked. Tasks that cannot run because their
   dependencies failed are still marked blocked by the normal no-ready-task
   path. Auto-expansion is disabled in limited mode because one task does not
   justify expanding the Worker cap.
4. **Manual dispatch.** `task.dispatch` / `vp task-dispatch` / Manage Dash
   "Dispatch one task" starts the same one-task path without waiting for the
   scheduler tick.
5. **Claims remain row-status based.** The task claim is the atomic
   `UPDATE ... WHERE status IN (retryable)` plus the in-process session run map.
   This is sufficient for a single daemon. Durable per-session ownership and
   lease renewal for multiple daemon processes are not added here.

## Consequences

- With dispatch mode enabled, one session runs at most one plan task at a time;
  the project cooldown still applies between task dispatches (use a small value
  such as 5s for continuous progress);
  this improves cross-session fairness but reduces intra-plan parallelism
  compared with full-plan resume. Users choose the mode per project.
- A failed task does not by itself mark the session failed; the task row records
  failure, dependents become blocked, and the session returns to idle.
- A session that still has blocked tasks remains selectable by the scheduler
  (as before) and may be retried after its cooldown.
- Two daemon processes sharing a database would still race on session status
  even though the task-row claim is atomic. A cross-process session lease is
  future work.
- The full-plan resume path is unchanged when `dispatch_tasks` is false.

## Alternatives considered

- **Always per-task dispatch when auto-resume is enabled:** changes existing
  behavior and reduces throughput for plans that intentionally batch work.
- **A separate dispatcher daemon:** unnecessary for the current single-daemon
  architecture and would need durable leases first.
- **Filter the plan to one task before calling the executor:** `sync_plan`
  treats missing tasks as pending and would corrupt the queue projection.
  A limit parameter on the existing executor keeps the plan identity intact.

# ADR-0091: Project-level automatic task resume policy

- **Status:** Accepted
- **Date:** 2026-11-07

## Context

The global resume scheduler was env-only (`VERLINPARK_TASK_SCHEDULER=1`), with a
daemon-wide interval and cooldown. That made it hard to enable for one project,
impossible to configure from the app, and easy to forget after a restart. The
task queue itself is already project-scoped.

## Decision

1. **Persistent policy.** Every project has a `TaskSchedulerPolicy` with
   `enabled` (default `false`) and `cooldown_secs` (default 60, valid 1–86400).
   It is stored in `project_task_scheduler_policies`, included in the project
   manifest, and restored by `vp db-rebuild`.
2. **Explicit per-project opt-in.** The scheduler loop is always running (a
   cheap query each tick), but only sessions whose project policy is enabled are
   candidates. A project with no policy row stays disabled by default.
3. **Legacy global default.** `VERLINPARK_TASK_SCHEDULER=1` remains a default for
   projects without an explicit policy row. An explicit project policy wins, so
   a project can be disabled even when the global default is on.
4. **Per-project cooldown.** The selected session carries its project's cooldown;
   the daemon uses that cooldown after a resume attempt, while the tick interval
   remains the existing `VERLINPARK_TASK_SCHEDULER_SECS`.
5. **Surfaces.** `project.task_scheduler.get` / `.set`, CLI
   `vp task-scheduler` / `vp task-scheduler-set`, and a Manage Dash toggle with
   a cooldown field.
6. **Recovery.** The policy is event-independent project configuration, but it
   is persisted and included in the manifest; changing it does not affect the
   task queue or active runs.

## Consequences

- Automatic resume is now an explicit, visible project setting rather than a
  hidden daemon environment variable.
- Disabled projects are never auto-resumed; enabled projects can still be
  stopped by cancelling their session or disabling the policy.
- The scheduler still resumes a stored plan, not an orphan task, and still spends
  the session budget. The policy does not bypass session admission, Worker
  capacity, task priorities, or budgets.
- The global env flag remains for compatibility but is no longer required.

## Alternatives considered

- **Keep env-only:** simple, but not usable from the app and poor multi-project
  ergonomics.
- **Global daemon toggle only:** cannot express "only this project".
- **Default enabled:** risks spending budget without the user asking; rejected.

# ADR-0127: Standalone task inbox and default-on autonomous dispatch

- **Status:** Accepted
- **Date:** 2026-11-10
- **Owners:** Runtime, Scheduling, macOS UI
- **Supersedes:** none
- **Related:** ADR-0088, ADR-0091, ADR-0095, ADR-0121, ADR-0123

## Context

The scheduler could only resume sessions that already had a stored `TeamPlan`.
There was no durable place for a user to say “do this one thing later” without
first starting a Leader conversation and asking it to invent a whole plan. The
audit recorded this as the missing default-on dispatcher and orphan-task path.

The runtime already had the pieces that make a safe implementation possible:

- a durable task-DAG projection with atomic claim and lifecycle events;
- deterministic `TeamPlan` validation;
- Worker task worktrees, Verifier gates, and the merge queue;
- global write leases and daemon-wide Worker/Labour capacity;
- priority, aging, retry gates, and session admission;
- project policies persisted in SQLite and the project manifest.

What was missing was a project-scoped intake object, a one-task plan bridge, a
default scheduling policy, and honest reconciliation when a run disappears.

## Decision

### 1. The task inbox is a project-scoped object, not a fake session

`intake_tasks` stores the title, instructions, profile, kind, priority, write
scope, acceptance criteria, optional budgets, dependencies, labels, origin,
attempts, max attempts, retry time, linked session, linked plan task, last
error, and result summary.

Intake is not attached to a session until dispatch. This keeps it useful before
a worktree exists, lets the user queue work while the machine is idle, and
avoids creating empty sessions that do nothing.

### 2. Dispatch synthesizes a validated one-task TeamPlan

When an item is dispatched, the runtime:

1. claims the row atomically (`queued`/`failed`/`interrupted` → `dispatching`,
   attempts + 1);
2. builds a deterministic one-department, one-task `TeamPlan` with task id `T1`;
3. validates it with the same `validate_team_plan` path used by Leader plans;
4. creates a normal session and worktree;
5. stores the plan and starts the normal plan-resume path;
6. records the linked session and plan task (`running`);
7. writes audit events into that session:
   `intake.dispatched`, `intake.completed`, `intake.failed`, or
   `intake.cancelled`.

The item therefore uses the real Manager/Worker/Verifier/Integrator pipeline.
There is no second execution engine and no hidden model or profile routing.

### 3. The dispatcher is default-on, bounded, and opt-out-able

`TaskSchedulerPolicy` is now default-on for new projects:

- `enabled = true` — resume stored plans and dispatch intake;
- `dispatch_tasks = false` — resume a whole plan rather than one task per tick;
- `include_intake = true` — include standalone tasks;
- `max_intake_parallel = 2` — at most two dispatching/running intake sessions
  per project (range 1–10);
- `cooldown_secs = 60` — minimum spacing between scheduler selections.

An explicit stored policy row overrides the defaults. `VERLINPARK_TASK_SCHEDULER=0`
is the global operator opt-out for projects without an explicit row. Manual
“Run now” bypasses the toggle and the parallel cap, but still respects
dependencies, claims, budgets, capacity, write leases, and verification.

The scheduling order is deterministic: task priority + age, plus the
orchestration policy's retry boost for retried items, then oldest enqueue time,
then id; equal priorities prefer a different project from the previous pick.

### 4. Dependencies are intake-to-intake gates

An intake item can depend on up to 20 other intake items in the same project.
The scheduler only selects it when every dependency is `completed` or
`skipped`. Manual dispatch refuses the same way. Dependencies are validated for
missing ids, self-reference, and cycles on edit.

When a dependency is cancelled, or fails with no retry scheduled, queued
dependents move to `blocked` with an explicit last error instead of waiting
forever. Retrying the blocked item re-queues it; deleting a dependency is
rejected while a non-terminal dependent still references it.

This gives a project-level DAG without requiring a full TeamPlan. The dependency
edges are scheduling preconditions, not Worker tool permissions; each task still
gets its own worktree and merge queue entry.

### 5. Failed dispatches are reconciled, not left running

`reconcile_intake_tasks` runs on every scheduler tick and at startup. It maps
the linked plan-task state to the intake state:

- `completed`/`skipped` → `completed`;
- `failed`/`blocked`/`interrupted` → `failed`;
- `cancelled` → `cancelled`;
- a terminal session with no settled task → `failed`;
- an active or paused session → remain `running`.

On failure, the orchestration policy's exponential backoff and retry flag decide
whether to record `next_attempt_at`. The item never auto-retries beyond its own
`max_attempts`. Manual retry resets attempts to zero. Daemon restart recovery
marks `dispatching`/`running` intake rows as failed with an explicit error, so a
crash cannot silently strand work.

### 6. Persistence and observability

Intake rows are in SQLite and in the project manifest snapshot. `vp db-rebuild`
restores them and downgrades stale `dispatching`/`running` rows to failed with a
rebuild explanation. RPC/CLI/UI expose list/create/update/dispatch/cancel/retry/
delete. `scheduler.status` (`vp scheduler`) reports global caps, session
occupancy, plan-task occupancy, intake occupancy, and each project's effective
policy without mutating anything.

The Task Inbox card polls quietly while visible, supports editing and dependency
selection, links to the session, and keeps the raw scheduler controls in the
same Team & Tasks section.

## Consequences

- A user can queue one focused task without inventing an org chart. The runtime
  still executes it with full isolation and verification.
- Default-on dispatch means new projects begin behaving like a small autonomous
  delivery queue. Budgets and explicit plan/task rows remain the spending
  boundary; users can pause intake or disable the scheduler.
- Each intake item creates a session/worktree. Retries create a new session
  rather than mutating a failed one, preserving the failed run for inspection.
- Dependency cycles are rejected on edit. A permanently failed or cancelled
  dependency blocks its queued dependents; the scheduler does not guess a repair,
  so the user retries/replaces the dependency or retries the blocked item.
- Intake events live in the linked session. Before dispatch, the item is
  project-scoped SQLite state and is not part of the session event stream.

## Alternatives considered

- **Create a session immediately for every queued item.** Rejected: it burns
  worktrees and leaves empty sessions when the queue is paused.
- **Let the Leader invent a plan for each orphan item.** Rejected: that adds a
  model call, hidden routing, and non-deterministic validation to what should be
  a deterministic queue operation.
- **Add intake to the existing `plan_tasks` table.** Rejected: `plan_tasks` is
  bound to a session and a plan revision; intake exists before either.
- **Make dispatch default-off until server-mode leases exist.** Rejected for the
  local single-daemon product: the data-dir lock plus atomic claims already
  prevent duplicate local dispatch, and the UI exposes an immediate opt-out.

# ADR 0121: Deterministic orchestration controller and retry policy

- Status: Accepted
- Date: 2026-11-09
- Area: Orchestration / runtime

## Context

The runtime already has a persisted task DAG, a Verifier gate, a merge queue,
and a budget-aware Worker/Labour capacity controller. It still lacked a
persistent answer to a basic team question: after a task fails, what happens
next?

Before this ADR, a failed task ended that plan batch and the session could be
resumed manually. There was no durable retry gate, no dependency-failure
policy, no typed controller decision history, and no way to preview an
orchestration action without mutating task rows. A user had to reason from the
event log and the task queue by hand.

## Decision

Add a deterministic orchestration controller. It never calls a model.

1. **Policy is project-scoped.** `OrchestrationPolicy` stores:
   `auto_retry_failed`, `max_attempts`, `retry_backoff_secs`,
   `retry_priority_boost`, `dependency_failure`
   (`block|cancel|retry`), `escalate_after_retries`, and
   `auto_resolve_blockers`. It is persisted in
   `project_orchestration_policies`, included in `projects.jsonl`, and
   restored by `db-rebuild`.
2. **Retries are durable.** `plan_tasks.next_attempt_at` gates claiming. A
   retry writes `plan_task.retry_scheduled` and returns the row to `pending`;
   the scheduler cannot claim it before the gate. The daemon tick and task
   completion path both run the controller, so a due retry survives a daemon
   restart.
3. **Backoff is exponential and capped.** Attempt 1 uses the configured base,
   attempt 2 doubles, and the policy cap is 24h. The delay is computed from
   persisted attempt count, not wall-clock heuristics.
4. **Budgets suppress retries.** If the session token or USD budget is already
   exhausted, the controller does not schedule another automatic attempt. It
   escalates once instead. This prevents a retry loop from spending budget the
   user already committed.
5. **Dependency policy is explicit.** When a failed task exhausts its retry
   budget:
   - `block` moves remaining dependents to `blocked` with a dependency reason;
   - `cancel` moves them to `cancelled`;
   - `retry` leaves them pending for an external retry or plan revision.
   Blocked tasks are released automatically when their dependencies complete.
6. **Escalations and resolutions are coordination notes.** Exhaustion writes
   a `critical` escalation for the task. A completed task can resolve open
   coordination blockers for that task when `auto_resolve_blockers` is on.
7. **Actions are event-backed.** Each applied decision writes
   `orchestration.decision` and a derived row. The controller action set is
   retry, block, unblock, cancel, escalate, and resolve blockers.
8. **Preview is pure.** `orchestration.preview` returns the same decisions
   without applying them. The UI shows the list and requires an explicit
   Apply action.
9. **No model routing.** The controller never chooses a model, profile, or
   task owner. It only acts on persisted scheduling state and explicit policy.

## Execution points

- After a plan task finishes, so retries and dependency handling happen
  immediately.
- On a daemon tick (`VERLINPARK_ORCHESTRATION_TICK_SECS`, default 20s), which
  discovers sessions with failed/blocked rows and sweeps expired runtime
  leases.
- Explicitly through `orchestration.advance` / `vp orchestrate` and the Collab
  Ops panel.

## Consequences

- A failed task now has a durable, inspectable next step.
- Retry policy is project configuration, not hidden runtime behavior.
- A blocked dependent can recover after an upstream retry without a new plan.
- The controller can be previewed and audited; it cannot silently rewrite a
  plan.
- Automatic retry is bounded by policy and session budget, but it still uses
  provider budget when the gate opens. There is no global retry budget beyond
  the per-task attempt count.
- The dependency policy applies to direct dependents. Transitive effects are
  resolved by later controller passes as statuses settle.

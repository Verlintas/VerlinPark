# ADR-0065: Plan batch abort settlement

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

A plan batch runs its ready tasks concurrently and then processes their results
one by one. If an early result aborted the plan (cancel or an abortive budget
stop), the result loop returned immediately. Siblings that had already started
never received a `plan_task.finished` event, so the Plan board could show them
as running forever.

## Decision

1. **Settle the whole batch.** Once a result requests an abort, later results
   in the same batch are not passed to `finish_worker` (so no commit or merge
   happens after the abort), but each task is explicitly settled.
2. **Status mapping.** Cancellation marks the task `cancelled`; an abortive
   budget stop marks it `blocked`. The agent is marked `cancelled` or `failed`
   respectively.
3. **Event + reason.** Each settled task emits `plan_task.finished` with the
   status and a reason. The current task keeps its existing event/report path.
4. **Non-batch work.** Tasks in later dependency batches are marked with the
   same terminal status, as before.
5. **Return once.** The plan returns `Cancelled` or `BudgetExceeded` only after
   every task in the current batch has been settled.

## Consequences

- The Plan board and `plan_task.*` events no longer leave siblings running.
- Work produced by a sibling after the abort point is not committed or merged;
  its task worktree remains available for inspection.
- An agent that actually finished may be reported as cancelled because its
  result was intentionally not integrated. This is explicit rather than
  silently discarding it.
- Plans still do not resume in a later run; a new run starts the plan again.

## Alternatives considered

- **Return immediately:** leaves hanging projections and was the bug.
- **Process every result and merge:** cancellation would not stop later merges.
- **Abort the futures:** they are already complete when the batch returns; this
  does not solve result settlement.

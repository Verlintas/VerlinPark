# ADR-0060: Plan task priority

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

Ready tasks in a dependency batch were selected in plan order. There was no way
to express that a critical-path task should run before an equally ready
low-value task without reordering the plan by hand.

## Decision

1. **Optional priority.** `PlanTask.priority` is an integer in `-100..100`,
   default 0. Values outside the range are rejected by plan validation.
2. **Deterministic ordering.** `select_plan_batch` sorts ready tasks by priority
   descending, then task ID ascending. Higher priority runs first; ties are
   stable and deterministic.
3. **Scope/capacity still win.** Priority does not bypass dependency order,
   write-scope serialization, Worker capacity, or budgets. A high-priority task
   that overlaps another selected task stays in the next batch.
4. **Visibility.** The plan prompt includes `priority`, and the Preview plan row
   shows non-zero priorities.
5. **No preemption.** A running lower-priority task is not paused or killed when
   a higher-priority task becomes ready.

## Consequences

- Leaders can encode critical-path intent without reordering the entire task
  list.
- Priority is per ready batch, not a global scheduler across sessions.
- A high-priority task can still be blocked by dependencies or scope conflicts.
- Negative priorities are allowed for background work.

## Alternatives considered

- **Only reorder the plan:** fragile and not explicit.
- **Global priority scheduler:** requires a persistent cross-session queue.
- **Preempt running Workers:** expensive and unsafe for worktrees.

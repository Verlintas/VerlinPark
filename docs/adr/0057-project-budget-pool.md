# ADR-0057: Project-wide shared budget pool

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

Budgets were session-scoped; a TeamPlan could narrow a department or task, but
there was no shared pool across sessions in one project. Running several
sessions could therefore multiply spend even when each session respected its
own budget.

## Decision

1. **Project budget.** `ProjectBudget` has optional `token_budget` and
   `cost_budget_usd`. Empty means unlimited. Non-positive values are rejected.
2. **Shared spend.** `project_usage` sums `tokens_in + tokens_out` and
   `cost_spent_usd` over all sessions in the project. This is the observed
   spend used by the reservation registry.
3. **Shared pool key.** The existing in-flight `BudgetRegistry` keys the project
   pool as `project:<id>`, so all sessions reserve against one pool atomically.
4. **Runtime enforcement.** A run loads the project limits at start, queries
   project spend before each provider call, and includes the project scope in
   the same atomic task/department grant. A post-call ledger check remains.
5. **Abortive scope.** A project budget stop is abortive like a session stop:
   it terminates the current run instead of merely blocking one task.
6. **APIs and UI.** RPC `project.budget.get/set`; CLI
   `project-budget` / `project-budget-set`; Manage Dash card with limits,
   current spend, Save/Reset.
7. **Not persisted reservations.** The registry is process-local; observed
   spend is durable in sessions and the usage ledger. A daemon restart drops
   in-flight reservations along with the dead calls.

## Consequences

- Multiple sessions cannot silently exceed a project-wide budget.
- The project budget is a root above the session budget; both must allow a
  call.
- Limits are loaded at run start, so changing them does not affect an already
  running session until its next run.
- Cost enforcement requires per-role USD prices; with no prices, cost stays
  zero and only token limits are meaningful.

## Alternatives considered

- **Sum only at session start:** permits large mid-run overshoot.
- **Separate per-project semaphore:** does not enforce token/cost amounts.
- **No project pool:** the original cross-session gap.

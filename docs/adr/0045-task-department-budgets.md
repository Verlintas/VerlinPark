# ADR-0045: Task and department budgets

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

ADR-0044 added per-call usage attribution. The session budget is still the only
enforced limit, so one expensive task or department can consume the whole
session before other work runs. TeamPlan already describes departments and a
task DAG, which is the natural place to declare narrower budgets.

## Decision

1. **Budget tree.** `PlanDepartment` and `PlanTask` gain optional
   `token_budget` and `cost_budget_usd`. Session budget remains the root; the
   department and task limits narrow it. A task may set a budget without a
   department budget, and vice versa.
2. **Validation.** Limits must be positive. A task budget may not exceed its
   department budget when both are present, and the sum of explicit task
   budgets in a department may not exceed the department budget. Plans that
   violate these rules are rejected before persistence.
3. **Attribution.** `AgentSpec` carries an optional department. Plan Workers
   receive `task.department`; the Verifier and Labour inherit the Worker's task
   and department. The usage ledger stores department alongside task, so
   department spend aggregates all calls for that department.
4. **Pre-flight check.** Before each provider request the runtime sums the
   ledger for the task and department, rejects a call that cannot fit
   `input estimate + 64 output tokens`, and lowers `max_tokens` to the tightest
   remaining task/department/session allowance. Session token reservations are
   unchanged.
5. **Post-call check.** After usage is committed, the runtime re-reads the
   ledger. A call that reaches or crosses a task/department token or cost limit
   stops the run.
6. **Scope-aware failure.** Budget stops carry a scope (`session`, `task`,
   `department`). A task/department stop blocks or fails that task and allows
   the plan to continue. A session stop still aborts the session.
7. **One-call overshoot.** Cost cannot be known before a provider call, and
   concurrent tasks in one department share a ledger but not a reservation.
   A department can overshoot by up to one call per concurrently running task.
   This is deliberate; per-scope reservations would need a shared atomic budget
   registry and are not implemented.
8. **Warning threshold.** The session budget emits one `agent.budget_warning`
   at 80% of tokens, cost, or time. Task/department warning thresholds are not
   emitted separately; their blocked/failed events carry the scope.
9. **No silent retries.** A blocked task is not automatically retried with a
   larger budget. The Leader can submit a new plan or the user can raise the
   session budget and retry.

## Consequences

- A plan can express a real budget tree and isolate expensive tasks.
- Verifier and Labour calls count against the task they are reviewing or
  assisting, which matches user expectations.
- Manual delegation tasks without a plan task ID are only covered by the
  session budget.
- The ledger query adds a small SQLite read per turn when task/department
  budgets are active; agents with no scope budget skip it.
- Task/department budget failures are visible as `agent.budget_exceeded` with
  `scope`, `id`, `limit`, and `spent`, and as blocked `plan_task.finished`
  events.

## Alternatives considered

- **Enforce only at task completion:** allows unbounded overspend.
- **Give every task its own LLM session budget:** cannot aggregate a
  department across parallel tasks.
- **Abort the whole plan on any task budget stop:** wastes unaffected work and
  makes one bad task take down the session.
- **Shared atomic department reservations:** correct but requires a new
  daemon-wide budget registry, persistence, and recovery semantics.

# ADR-0040: Per-role models and USD/time session budgets

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

All roles used the same model, and the only enforced budget was a session-wide
token count. A Leader can need a stronger model than a Labour assistant, while a
Verifier may use a cheaper model with stronger checks. Token counts also hide
real spend when model prices differ, and a run can consume an unbounded amount of
wall-clock time even under a token budget.

The runtime must not add hidden routing. Users choose role models and prices, and
the runtime applies them deterministically.

## Decision

1. **Role model policy.** `RoleModels` stores an optional model and optional
   input/output USD price per 1M tokens for Leader, Manager, Worker, Verifier, and
   Labour. A blank model falls back to the base model. `agent.configure` accepts
   the policy; `agent.info` returns it.
2. **Model selection.** Every provider request uses the model for the requesting
   role. Agent records store the selected model. Child agents (Manager, Worker,
   Labour, Verifier, Conflict Resolver) use their own role model.
3. **Cost accounting.** When a role has input/output prices, each provider turn's
   reported usage (or the persisted estimate when usage is absent) produces a USD
   cost. Cost is added atomically to `sessions.cost_spent_usd` with an
   `agent.cost` event (`amount_usd`, `total_usd`).
4. **Session cost budget.** `sessions.cost_budget_usd` is optional. The runtime
   checks `cost_spent_usd >= cost_budget_usd` after each turn and stops with a
   `BudgetExceeded` result whose event reason is `cost`. Missing/zero prices mean
   no cost is recorded, so a USD budget requires configured prices.
5. **Session wall-clock budget.** `sessions.time_budget_secs` is optional. A run
   starts a wall-clock timer and checks it at turn and tool boundaries. Exceeding
   it stops the run with reason `time`. The budget applies per run, not across
   daemon restarts; recovery still fails in-flight runs.
6. **Token budget remains.** The session token budget is independent and still
   uses pre-flight reservations and usage settlement. Cost and time are additional
   limits; none of them replaces another.
7. **UI/CLI.**
   - Providers settings: per-role model override and input/output price fields;
     persisted in the app and sent on configure.
   - New Session: optional USD and wall-clock fields.
   - Session header: tokens, USD, and time budget labels.
   - CLI `session-new --cost-budget-usd --time-budget-secs`.
8. **Validation.** Cost budgets must be positive finite values; time budgets must
   be positive. Prices are user-entered and are not fetched automatically.

## Consequences

- A session can use different models per tier without hidden routing.
- USD spend and limits become visible and enforceable when prices are configured.
- A wall-clock budget bounds runaway runs even when token usage is understated.
- Cost computation is only as good as the user-entered prices; estimated usage
  can under- or over-state actual billing.
- Cost is session-wide, not per task/department; there is no per-role token
  budget, USD rollback, or automatic price catalog yet.
- Time budgets are cooperative boundary checks, not hard preemption of an
  in-flight provider stream or tool.
- Cost/time values are exported in session bundles; policy/model configuration is
  app/daemon configuration and is not part of a bundle.
- Paused time counts against the per-run wall-clock budget.

## Alternatives considered

- **One global price per session:** inaccurate when roles use different models.
- **Automatic provider price lookup:** vendor/model catalogs are unstable and
  would add hidden configuration; user-entered prices are explicit.
- **Per-task/department budgets now:** useful, but requires a larger budget tree;
  the session limit is the first enforceable layer.
- **Hard timeout with process kill:** should be added for tools, but the current
  provider stream/tool boundary check is the safe incremental step.

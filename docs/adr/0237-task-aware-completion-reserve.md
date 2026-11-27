# 0237 — Task-aware completion reserve

- Status: Accepted
- Date: 2026-11-27
- Scope: Fixed/Auto session completion reserve, pending critical-path p90,
  active-task finish behavior
- Depends on: ADR-0018 (session budget), ADR-0231 (Auto Budget), ADR-0234
  (Run Contract), ADR-0236 (operator-owned turn budget), R3 allocation preview

## Context

The completion reserve was a fixed fraction of the session budget (default
20%). A real two-task plan paused a running Labour task at 163k/200k tokens
because the proportional reserve was 40k, even though the tasks were already
in progress. That pause interrupted verification and produced an unclear
state; a blind 20% threshold is not a model of the remaining work.

R3 already computes per-task p50/p90 estimates from session actuals, task
envelopes, project history, and department envelopes, plus the remaining
critical path. The runtime did not use that information for the reserve.

## Decision

The completion reserve remains a **future-work protection**, not a mid-edit
interrupt:

1. Direct (no TeamPlan) sessions keep the proportional reserve as a finalization
   guard.
2. For a session with a TeamPlan:
   - if no non-running, non-terminal task remains, the reserve override is
     zero so the current task can reach a clean commit;
   - if pending critical-path tasks have p90 token estimates, the reserve is
     their sum; a lower p90 allows progress that the proportional rule would
     have blocked, and a higher p90 protects a genuinely expensive remainder;
   - if any pending critical-path task lacks a p90 estimate, fall back to the
     proportional reserve;
   - cost p90 is used only when no task is currently running (the allocation
     task row exposes token p90; per-task cost p90 is not available). Time
     remains proportional.
3. The allocation preview is computed only when the proportional rule would
   already pause. The turn path performs a cheap TeamPlan/task-queue probe
   first and returns the zero future-work reserve without running the full
   allocation preview.
4. Hard caps, attention, context, approvals, cancellation, preemption,
   Verifier, and Integrator remain unchanged. Active work is still stopped by
   a hard cap or no-progress policy, not by the completion reserve.
5. Decision details expose `completion_reserve_source`,
   `completion_reserve_p90_tokens`, and `completion_reserve_p90_cost_usd` so
   the pause is explainable.

## Consequences

- A running task is not paused merely because the session crossed an arbitrary
  reserve percentage; the next **unstarted** work is what the reserve protects.
- If p90 is unavailable, behavior is the previous proportional fallback.
- p90 is a historical estimate, not a vendor token guarantee; sparse history
  may under- or over-estimate. The hard cap remains authoritative.
- A plan with only active tasks can consume up to the hard cap before a hard
  stop; the operator still has top-up/cancel controls.
- Auto sessions continue to leave completion-reserve management to the Auto
  controller (`reserve_ratio = 0` on the governor path).
- No automatic spend, top-up, model routing, approval, or merge is added.

## Alternatives rejected

- **Raise or remove the percentage.** Still blind to the remaining plan.
- **Reserve p90 of all remaining tasks including running ones.** An active task
  would be paused again, and its historical total is not its remaining cost.
- **Block plan execution on a precomputed p90.** Sparse history is common and
  would prevent useful work instead of adapting at the boundary.

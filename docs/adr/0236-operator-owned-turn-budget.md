# 0236 — Operator-owned turn budget

- Status: Accepted
- Date: 2026-11-27
- Scope: per-run provider-turn ceiling, fixed/Auto sessions, finalize window
- Depends on: ADR-0018 (session budget and cooperative cancel), ADR-0133
  (explicit unlimited mode), ADR-0231 (Auto Budget), ADR-0234 (Run Contract)

## Context

A real, isolated two-task TeamPlan reached its second task's Verifier and merge
queue, but the Leader exhausted a hidden 20 provider-turn ceiling while handling
one operator reconciliation. The same workflow would have hit 60 turns on a
larger plan; simply changing the constant to 60 would only move an arbitrary
limit.

VerlinPark already has stronger, user-visible bounds:

- session token, USD, and wall-clock budgets with pre-flight reservations;
- Auto Budget extension/pause and the completion reserve;
- no-progress pause policy and plan-task settlement;
- approvals, contracts, cancellation, and preemption;
- Verifier/Integrator gates and operator questions.

A per-run turn ceiling remains useful as a debugging/safety override, but it
must not silently truncate normal work.

## Decision

`AgentConfig.max_turns` is `Option<u32>` and defaults to `None` (no implicit
turn ceiling). The run loop still uses a finite iteration shape, but no
finalize-window nudge, finalize reserve, or max-turn failure is produced unless
an explicit ceiling exists.

Explicit ceilings are operator policy:

- `VERLINPARK_MAX_TURNS` / `runtime-config-set --max-turns` accept `1..=200`;
  `0` or an invalid value disables the ceiling;
- Auto Budget's `max_turns_per_run` remains an explicit project policy and
  still drives its configured per-run limit;
- Scout keeps its own bounded step request when no global ceiling is set.

The token/cost/time budgets, Auto completion reserve, no-progress policy,
approval broker, cancellation, preemption, Verifier, and Integrator remain
unchanged. Unlimited mode still removes economic gates, so an operator who
wants a turn ceiling in that mode must set one explicitly.

## Consequences

- Normal multi-task plans do not fail at a hidden 20- or 60-turn line; they are
  bounded by the budget and progress controls the operator can see.
- A pathological provider/tool loop without any budget (notably explicit
  unlimited mode) has no implicit turn stop; the operator must set
  `VERLINPARK_MAX_TURNS`, `max_turns_per_run`, or cancel the session.
- Existing explicit turn-limit tests continue to exercise the finalize window
  and `maximum turns reached` path.
- Per-task/turn derivation from remaining budget is possible future work; this
  ADR deliberately does not introduce another heuristic constant.

## Alternatives rejected

- **Raise the default to 60.** Still an arbitrary hidden number; larger plans
  would hit it later.
- **Remove the loop bound entirely without a budget check.** The loop already
  checks budgets and progress; this ADR keeps the loop shape but makes the
  ceiling operator-owned.
- **Require a turn cap for every session.** Adds friction and duplicates the
  existing budget/no-progress controls.

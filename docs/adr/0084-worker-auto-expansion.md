# ADR-0084: Budget-gated worker auto-expansion

- **Status:** Accepted
- **Date:** 2026-11-04

## Context

A TeamPlan with many independent ready tasks could only use the configured
per-session Worker cap. Users had to raise the cap globally before starting the
plan, even when the plan was small.

## Decision

1. **Opt-in.** `auto_expand_workers` is a configured boolean, off by default. It
   is exposed in Settings → Team capacity, `agent.configure`, and
   `VERLINPARK_AUTO_EXPAND_WORKERS`.
2. **Plan-only.** Expansion happens inside `execute_team_plan` only. The
   session's original Worker cap is restored by a drop guard when plan execution
   ends, so manual delegation keeps the configured cap.
3. **Budget gate.** Expansion is allowed only when at least 20% of the session
   token budget remains (or no token budget is configured). Per-task,
   per-department, project, and global capacity gates still apply.
4. **Hard cap.** Expansion targets the number of ready tasks, bounded by the
   runtime hard Worker cap of 10.
5. **Observability.** A `plan.auto_expand` event records ready task count and the
   expanded cap.
6. **No Managers/Labour expansion.** Managers and Labour remain at the configured
   caps; there is no scale-down policy and no global persistent scheduler.

## Consequences

- Large independent plans can use more Workers without a permanent cap change.
- A low token budget prevents expansion, even if global capacity is free.
- Behaviour is deterministic: target = `min(ready_tasks, 10)`, never above the
  existing cap unless it is larger.
- Expansion lasts only for the plan run; subsequent manual delegation returns to
  the base cap.

## Alternatives considered

- **Raise the global Worker cap:** affects every session and is hard to undo.
- **Always expand:** violates the budget/hard-cap invariant and surprises users.
- **Auto-expand Managers/Labour:** needs a different demand signal and scale-down
  policy; deferred.

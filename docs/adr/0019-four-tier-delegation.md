# ADR-0019: Recursive four-tier delegation in the session worktree

- **Status:** Accepted
- **Date:** 2026-10-25

## Context

The single-Leader runtime, approvals, worktree, diff, budget, cancellation, and
persisted plan are working. The product's defining feature is still a preview:
Leader / Manager / Worker / Labour cards with no real agents behind them.

Building the final scheduler, per-task worktree pool, capability-routed models,
verifier gates, and merge queue at once would be too large to validate safely.
We need a first real org execution path that preserves the existing invariants
and can be extended without rewriting the runtime.

## Decision

1. **Strict role chain.** The only allowed delegation edges are
   Leader → Manager, Manager → Worker, and Worker → Labour. Labour cannot
   delegate. Managers are read-only; Workers and Labour have the coding tools.
2. **Synchronous recursion.** A parent calls `delegate`, the runtime runs the
   child to completion, and returns the child's summary as the tool result.
   Execution is sequential; no two agents write at the same time.
3. **One worktree for the run.** Every role works in the session worktree
   (ADR-0016). This keeps writes serialized and immediately reviewable. Per-task
   worktrees and a merge queue remain future work.
4. **Persisted roster, ephemeral child conversations.** Leader, Manager, Worker,
   and Labour records are stored in `agents` and rendered in Collab. Child
   conversations are not persisted as message rows; their deltas, tool calls,
   summaries, and failures are persisted in the event log. The Leader conversation
   remains resumable.
5. **Caps are enforced per run.** The app sends the org preset caps with
   `agent.configure`: 1–5 Managers, 1–10 Workers, 1–40 Labour. The runtime rejects
   delegation beyond the caps. Token budget and turn limits are shared.
6. **Shared control plane.** Approvals remain session-scoped and now carry the
   requesting agent's id/name/role. Cancel and token-budget exhaustion propagate
   through the recursive call stack. All roles use the configured model until
   per-role model routing exists.
7. **Leader plan stays visible.** The Leader's `todo_write` list remains the
   session plan; Collab shows it alongside the real agent roster.

## Consequences

- The four tiers can actually execute a session end-to-end, with a real audit
  trail for every child agent.
- Execution is serial and can be slow. This is not the parallel scheduler from
  the architecture; concurrency, write-scope locks, and per-task worktrees are
  still missing.
- Managers only review and delegate; there is no independent Verifier or
  Integrator, and no merge queue. A Worker summary is not a verification gate.
- A daemon restart loses child conversations (the session is marked failed).
  Completed child summaries remain in the event log.
- Caps reset per top-level run, not per session lifetime.
- One model is used for all roles, so role-specific model presets are not yet
  honored.
- Working in one worktree avoids merge conflicts but does not deliver the
  isolation guarantees required for parallel Workers.

## Alternatives considered

- **Build the full DAG scheduler first:** rejected as too large to validate in
  one step; this recursive slice produces real evidence and reusable records.
- **Give every role its own worktree immediately:** rejected because merging
  child branches without a verifier/merge queue would create more risk than value.
- **Run child agents as separate processes:** deferred; in-process recursion is
  enough for the first org slice and keeps cancellation/budget shared.

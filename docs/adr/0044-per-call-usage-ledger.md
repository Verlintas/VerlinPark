# ADR-0044: Per-call usage ledger

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

Session token and USD projections are aggregate counters. The event stream has
`agent.usage` and `agent.cost` events, but they did not identify the calling
agent, role, task, or model. That makes it impossible to answer which Worker or
department spent a session budget, and it blocks per-task/department budget
trees.

## Decision

1. **One row per provider call.** `usage_ledger` stores session, agent ID/name,
   role, optional task ID, model, input/output tokens, `estimated`, USD cost, and
   timestamp.
2. **Atomic projection plus events.** The store updates `sessions.tokens_in`,
   `tokens_out`, and `cost_spent_usd`, inserts the ledger row, and emits the
   `agent.usage` and (when cost is positive) `agent.cost` events in one
   transaction. Event mirror writes happen after commit.
3. **Backward-compatible events.** `agent.usage` keeps `input_tokens`,
   `output_tokens`, and `estimated`, and adds optional context fields. Existing
   replay usage checks still work because they only read the original fields.
4. **Runtime attribution.** The run loop passes the active `AgentSpec` and the
   exact request model into the ledger. `estimated = true` preserves local
   endpoints that omit provider usage.
5. **Read API.** `session.usage` / `vp usage <session>` returns entries plus
   deterministic totals grouped by role, model, agent, and task. Buckets sort by
   cost descending, then key.
6. **Preview.** The Cost tab in Preview shows session totals, grouped buckets,
   and the newest calls first, including the estimated marker.
7. **No budget enforcement yet.** The ledger is accounting only. Per-task and
   per-department budget limits are not enforced by this slice.

## Consequences

- Budget spend is attributable without parsing free-form messages.
- The extra row write shares the existing event transaction, so it does not add
  a separate failure point after usage is committed.
- Old sessions have no historical ledger rows; only new provider calls appear.
- Ledger rows are session-scoped and deleted with the session. They are not yet
  included in `session.replay` or export/import bundles.
- The ledger is not content-addressed or compacted; high-frequency sessions will
  grow it linearly with provider calls.

## Alternatives considered

- **Derive from events only:** no index, slow for long sessions, and old events
  lack attribution.
- **Keep only aggregates:** cannot answer per-task/department questions.
- **Separate usage and cost transactions:** can leave cost and tokens
  inconsistent when the second write fails.

# ADR-0068: Project usage report

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

The project budget and usage ledger exposed spend for one session at a time.
With multiple sessions sharing a project pool, users could see the total but
not which sessions consumed it.

## Decision

1. **Project-scoped report.** `project.usage` returns every session in the
   project with goal, status, provider-call count, input/output tokens, and USD
   cost, plus project totals.
2. **Source of totals.** Tokens and cost come from the session projections
   (`tokens_in`, `tokens_out`, `cost_spent_usd`) used by the project budget.
   Call count comes from `usage_ledger` rows for that session.
3. **All sessions included.** Idle, running, failed, cancelled, and imported
   archive sessions appear; sessions with no calls show zeroes.
4. **Entry points.** `vp project-usage <project>` and Manage Dash → Usage by
   session, with a manual refresh. The full list is not truncated in the API;
   the dashboard shows the first eight.
5. **Read-only.** The report does not mutate projections or emit events.
6. **No date buckets yet.** The initial report is per session; daily/model
   breakdowns remain in the per-session usage ledger.

## Consequences

- Users can attribute project spend to sessions and spot expensive runs.
- Totals stay consistent with the shared project budget because both use the
  session projections.
- Imported archives with projected usage are included.
- A session with estimated usage still reports the persisted estimate; the
  report does not distinguish estimated from reported rows.
- Large projects return a large session array; the UI truncates the display.

## Alternatives considered

- **Aggregate only:** already existed and did not answer “which session?”.
- **Read every session ledger:** accurate per-call grouping but much heavier;
  the per-session Cost tab already covers that.
- **Date-bucketed report first:** useful but needs timezone/range decisions.

# ADR-0066: Usage CSV export

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

The per-call usage ledger is visible in Preview → Cost and as grouped human
output in `vp usage`, but finance/ops workflows need a spreadsheet-friendly
export without parsing screen output or JSON.

## Decision

1. **Server-side CSV.** `session.usage_csv` builds CSV from the same ledger used
   by `session.usage`; the app and CLI do not re-implement aggregation.
2. **Columns.** `created_at, agent_name, role, task_id, department, model,
   input_tokens, output_tokens, total_tokens, estimated, cost_usd`.
3. **Escaping.** Fields containing comma, quote, CR, or LF are quoted and inner
   quotes are doubled, per RFC 4180 conventions.
4. **No totals row.** Totals remain in the JSON/human ledger and in the app
   summary metrics; the CSV is one row per provider call.
5. **Entry points.** `vp usage <session> --csv` prints stdout; Preview → Cost has
   **Export CSV…** with a save panel.
6. **Read-only.** Export does not mutate the ledger or emit events.

## Consequences

- Spreadsheet users can account for per-agent/task/department/model spend.
- Empty task/department values remain empty cells.
- The CSV uses six decimal places for USD and does not include a currency
  column; the project is USD-only today.
- Very large sessions produce a correspondingly large CSV; there is no
  streaming export yet.

## Alternatives considered

- **CLI-only formatting:** duplicated in the app and easy to drift.
- **JSON export:** precise but not directly spreadsheet-friendly.
- **Include totals:** mixes record and summary rows, which breaks pivoting.

# Auto budget and adaptive tooling plan

- Status: implemented (verification/test execution limited by host process-launch stall)
- Date: 2026-11-21
- Scope: deterministic auto budget, forecast, hard caps, no-progress pause,
  conditional tool advertisement, program/milestone reservations, and the
  operator surfaces needed to use them.

## Goal

Make the common long-task path require **one user decision** (a hard budget cap)
instead of manual token/cost/time/turn/tool configuration. The runtime may
extend budgets, trim tools, compact context, and pause work, but only
deterministically, within an explicit hard cap, and with an event trail.

Auto mode never changes the model/provider, approvals, sandbox, write scope,
Verifier, or merge policy.

## Core types

### `BudgetMode`

`fixed | auto | unlimited`.

- `fixed`: existing behavior; effective token/cost/time budgets are user-set.
- `auto`: effective budgets start at the policy's initial values and may be
  extended by the runtime up to the policy's hard caps.
- `unlimited`: explicit bypass of economic gates; never selected implicitly.

`Session.unlimited` remains for wire compatibility. A session with
`unlimited=true` is `unlimited` regardless of the stored mode.

### `AutoBudgetPolicy`

Project-scoped policy with session-level overrides at creation:

- `enabled: bool`
- `initial_token_budget: Option<u64>`
- `initial_cost_budget_usd: Option<f64>`
- `initial_wall_clock_secs: Option<u64>`
- `hard_token_cap: Option<u64>`
- `hard_cost_cap_usd: Option<f64>`
- `hard_wall_clock_secs: Option<u64>`
- `extend_ratio: f64` (default 0.25)
- `max_multiplier: f64` (default 4.0)
- `forecast_calls: u32` (default 8)
- `min_runway_turns: u32` (default 4)
- `warn_at_ratio: f64` (default 0.20)
- `pause_at_ratio: f64` (default 0.05)
- `max_turns_per_run: u32` (default 60, hard max 200)
- `max_total_turns: Option<u32>`
- `no_progress_turns: u32` (default 6)
- `no_progress_action: pause | escalate` (default pause)
- `auto_tool_bundle: bool` (default true)

At least one hard cap is required when `auto` is selected. If a project policy
has no hard cap, `auto` creation fails with a typed validation error and the
caller must choose `fixed` or provide a cap. No silent fallback to unlimited.

### `BudgetForecast`

Derived, event/ledger-backed, never model-generated:

- `sample_calls`
- `avg_input_tokens_per_call`
- `avg_output_tokens_per_call`
- `avg_cost_usd_per_call`
- `remaining_tokens`, `remaining_cost_usd`, `remaining_secs`
- `runway_turns`, `runway_secs`, `runway_cost_usd`
- `critical_path_turns` (if a stored plan exists)
- `source`: `provider_ledger | mixed | estimate | unavailable`
- `generated_at`

## Storage

New additive tables (rebuilt from manifest/events where applicable):

- `project_auto_budget_policies(project_id PK, policy_json, updated_at)`
- `session_auto_budgets(session_id PK, mode, policy_json, state_json, caps_json,
  created_at, updated_at)`

`session_auto_budgets.state_json` carries bounded operational state:

- `extensions`, `last_extension_at`
- `total_turns`, `turns_without_progress`, `last_progress_seq`
- `last_forecast`
- `paused_reason`, `paused_at`

Effective budgets remain in the existing session
`token_budget`/`cost_budget_usd`/`time_budget_secs` columns. Hard caps live in
the auto table. `unlimited` remains the compatibility flag.

Project policy updates are persisted in SQLite and the project manifest;
session auto state is event-backed for lifecycle changes and operational for
forecast snapshots.

## Auto budget algorithm

Before a provider call, in this order:

1. Resolve effective budgets and hard caps.
2. Compute `BudgetForecast` from the usage ledger, current context, and plan.
3. Emit a warning at `warn_at_ratio` once per threshold crossing.
4. If remaining is below the minimum runway:
   - if under the hard caps, extend the effective budget by the larger of
     `extend_ratio * initial` and the amount needed for `min_runway_turns`,
     clamped to the hard cap; persist and emit `session.budget.auto_extended`;
   - otherwise stop with `AUTO_BUDGET_EXHAUSTED` and pause the session at the
     next safe boundary. Do not retry the same provider call.
5. The existing token/cost/time preflight and reservations run after this step;
   auto extension can only widen up to the stored hard cap, never bypass
   reservation/settlement or approval.

Time is cumulative for auto sessions: total elapsed is persisted and the
per-run preemption deadline is `min(remaining_auto_wall_clock,
max_turns_per_run guard)`, not a fresh full window.

## No-progress guard

At each turn boundary:

- Count progress evidence events since `last_progress_seq`:
  `commit`, `test_cache.stored`, `plan_task.finished`, `merge_queue.merged`,
  `artifact.published`, `workspace.land`, `session.checkpoint`.
- Read-only calls, repeated reads, and model prose are not progress.
- If `turns_without_progress >= no_progress_turns`, emit
  `session.auto_paused` with reason `no_progress`, evidence counters and the
  current forecast, then pause at the next safe boundary.
- `escalate` creates a coordination/attention record instead of pausing.
- Operator resume clears `turns_without_progress` only with an explicit
  decision; it is not reset by another read-only turn.

## Adaptive tooling

- Add explicit capability flags to the tool-schema builder:
  `mcp_configured`, `remote_host_configured`, `archive_media_available`,
  `browser_helper_available`, `computer_helper_available`,
  `web_search_configured`, `artifacts_enabled`.
- Only advertise conditional tools when the capability is present.
- Leader defaults to an orchestrator bundle (read-only, plan/delegate/review,
  artifacts/memory/sources, no shell/write/computer/browser/remote_write)
  unless an explicit solo mode is selected.
- Unknown task kind falls back to the role's minimal/default bundle, not `full`.
- Enforce a final advertised-schema budget per role and fail tests when the
  budget is exceeded.
- `tool_search` remains the deterministic escape hatch; promotion is explicit
  and turn-boundary scoped.

## Program and milestone reservations

Use `BudgetRegistry` pools for program/milestone dispatch:

- Reserve the milestone's remaining budget before starting its session.
- Concurrency across milestones shares a program pool; two milestones cannot
  each receive the full remaining program allowance.
- Release on session start failure; settle from the usage ledger on completion.

## RPC / CLI / UI

RPC:

- `session.budget.get` -> mode, effective/hard budgets, forecast, state.
- `session.budget.set` -> set mode/unlimited/effective budgets/hard caps with
  event-backed validation; cannot set below already spent.
- `project.auto_budget.get/set`.
- `session.budget.forecast` (read-only preview).

CLI:

- `vp session-budget <session>`
- `vp session-budget-set <session> --mode auto|fixed|unlimited --max-usd ...`
- `vp session-budget-forecast <session>`
- `vp project-auto-budget <project>` / `-set`

Swift:

- New session sheet: Budget preset `Auto | Fixed | Unlimited`; Auto shows one
  hard-cap field (USD or tokens), not five budget fields.
- Settings: Automation card for auto budget defaults and no-progress pause.
- Stage: budget/runway card with auto-extension/pause reasons.

## Tests

- Store round-trip/migration/manifest/replay for both tables.
- Forecast math with synthetic usage; no provider calls.
- Auto extension stays under hard cap; hard-cap stop pauses, never unlimited.
- Cumulative auto wall-clock across resumes; per-run turn cap.
- Concurrent task/department calls share reservations.
- No-progress pause after K turns with only reads; progress resets it.
- Program parallel milestones cannot overspend.
- Conditional tools absent when capability absent; Leader orchestrator bundle.
- Schema byte/token budget regression.
- Live/offline long-task soak: 50+ turns, compaction, interruption/resume,
  parallel workers, assert no overspend, no duplicate write, no lost task.

## Non-goals

- Hidden model/provider routing.
- Automatic approval/sandbox/Verifier/merge widening.
- Cross-host budget fencing or distributed leases.
- LLM-generated budget decisions.

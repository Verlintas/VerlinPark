# 0231 — Auto budget and adaptive tooling

- Status: Accepted
- Date: 2026-11-21
- Scope: deterministic session budgets, forecast, hard caps, cumulative wall clock, no-progress pause/escalate, program/milestone reservations, conditional tool capabilities, Leader orchestrator bundle, operator surfaces
- Depends on: ADR-0003 (manual model/budget configuration), ADR-0018 (session budget and cooperative cancel), ADR-0047 (in-flight budget reservations), ADR-0133 (explicit unlimited mode), ADR-0157 (programs and milestones), ADR-0190 (tool bundle partition), ADR-0229 (budget preflight estimation)
- Plan: [AUTO_BUDGET_PLAN.md](../AUTO_BUDGET_PLAN.md)

## Context

Long-running sessions were economically manual. An operator had to choose token,
cost, and wall-clock limits up front, and the runtime could only warn or stop at
those fixed limits. There was no deterministic forecast, no bounded top-up, and
no hard stop when a session kept producing model prose and read-only calls
without durable progress. The only escape hatch was the explicit `unlimited`
mode, which removes economic gates instead of making a bounded decision.

Three additional problems made this worse at scale:

- Parallel program milestones could each be granted the same remaining program
  allowance because settled usage is visible only after provider calls.
- A Leader received the full role-wide tool surface even when MCP, remote hosts,
  archive/media helpers, GUI helpers, web search, LSP, or file artifacts were
  not configured. The standard bundle still requests 49 schemas for a generic
  Leader and 51 for a coding Leader. The schema prefix is a per-call cost and
  part of the cache prefix.
- Tool partitions already existed, but there was no bounded opt-in read-only
  Leader orchestration surface and no capability-aware schema builder.

The result must not become hidden model routing, automatic approval widening, or
automatic conversion to `unlimited`. Auto budget is a bounded runtime decision:
one operator-selected hard cap, deterministic extension and pause behavior, and
an event trail. All existing approval, sandbox, contract, write-scope, Verifier,
Integrator, merge, capacity, and model/provider rules remain authoritative.

## Decision

### 1. Three explicit budget modes

`BudgetMode` is `fixed | auto | unlimited`.

- `fixed` is the existing behavior and the default. Effective token/cost/time
  limits are operator-set.
- `auto` starts at the policy's initial values and may be extended by the
  runtime only inside the policy's hard cap.
- `unlimited` is the explicit ADR-0133 bypass and is never selected implicitly.

`Session.unlimited` remains the wire-compatibility authority. A session with
`unlimited=true` reports `mode=unlimited` even when a stored auto mode is still
present, so toggling unlimited off restores the prior stored mode. Auto sessions
never set the unlimited flag themselves.

Auto creation fails closed with a typed error when no cap or no policy is
available:

- `AUTO_BUDGET_POLICY_REQUIRED` when neither an explicit session policy nor an
  enabled project policy exists;
- `AUTO_BUDGET_CAP_REQUIRED` when the resolved policy has no token, cost, or
  wall-clock hard cap;
- `BUDGET_MODE_CONFLICT` when unlimited and auto/fixed are combined, or when an
  unlimited session also declares numeric budgets;
- `BUDGET_POLICY_NOT_APPLICABLE` when a session auto policy is supplied with a
  non-auto mode.

The store persists:

- `project_auto_budget_policies(project_id, policy_json, updated_at)`;
- `project_auto_budget_events(id, project_id, seq, kind, payload, created_at)`
  for the project-scoped audit event;
- `session_auto_budgets(session_id, mode, policy_json, state_json, caps_json,
  created_at, updated_at)`.

Effective budgets stay on the existing `sessions` columns. Hard caps and the
resolved policy copy live in the auto row, so a later project-policy edit cannot
silently widen an existing session. Session creation commits the session row,
`session.created` event, and auto row together; bundle export/import, replay,
repair, and `db-rebuild` carry the policy, caps, and bounded progress state.
The remaining pause-metadata replay gap is listed below. Forecast-only state
snapshots are operational and may be rewritten without a new event; extension,
pause, resume, and progress-state changes are event-backed. The project policy
update writes SQLite plus the project manifest and a project-scoped
`project.auto_budget.updated` audit row.

### 2. `AutoBudgetPolicy`

The project-scoped default (with an optional session-level override at creation)
contains:

| Field | Default | Meaning |
|---|---:|---|
| `enabled` | `false` | Project default only; an explicit session policy does not require it |
| `initial_token_budget` | none | Starting effective token budget |
| `initial_cost_budget_usd` | none | Starting effective cost budget |
| `initial_wall_clock_secs` | none | Starting cumulative wall-clock budget |
| `hard_token_cap` | none | Absolute token ceiling |
| `hard_cost_cap_usd` | none | Absolute USD ceiling |
| `hard_wall_clock_secs` | none | Absolute cumulative wall-clock ceiling |
| `extend_ratio` | `0.25` | Extension step as a fraction of the initial value |
| `max_multiplier` | `4.0` | Growth ceiling relative to the initial value |
| `forecast_calls` | `8` | Bounded usage-ledger sample window |
| `min_runway_turns` | `4` | Minimum desired runway before extension |
| `warn_at_ratio` | `0.20` | Remaining-budget warning threshold |
| `pause_at_ratio` | `0.05` | Remaining-budget extension/pause threshold |
| `max_turns_per_run` | `60` | Per-run provider-turn cap (hard maximum 200) |
| `max_total_turns` | none | Optional cumulative turn cap |
| `no_progress_turns` | `6` | Consecutive no-evidence turns before action |
| `no_progress_action` | `pause` | `pause` or `escalate` |
| `auto_tool_bundle` | `true` | Stored policy preference for adaptive tooling |

At least one hard cap is required for auto. `max_total_turns`, when present, must
be at least `max_turns_per_run`. Numeric validation rejects zero/negative and
non-finite limits. At creation the effective value for each dimension is the
operator request, then the policy initial value, then the hard cap, clamped to
the cap. The initial value is not a second cap: it is the first effective limit.

`max_multiplier` is a tighter growth ceiling than the hard cap. The extension
ceiling is `min(hard_cap, initial * max_multiplier)` for each dimension; the hard
cap remains the absolute maximum and can be raised only by an explicit operator
budget update.

### 3. Deterministic forecast

`BudgetForecast` is derived from the usage ledger and session state. It is never
model-generated. It records:

- the bounded sample size and average input/output tokens and cost per call;
- remaining tokens, cost, and seconds;
- estimated runway turns (the minimum of token and cost runway), remaining time,
  and remaining cost;
- `critical_path_turns` as a reserved field;
- a source label: `provider_ledger`, `mixed`, `estimate`, or `unavailable`;
- a generated-at timestamp.

The forecast reads at most `forecast_calls` recent usage entries (clamped to
1–1,000). The current implementation sets `critical_path_turns` to `None`; plan
DAG integration is not part of this round. `runway_secs` is the remaining
wall-clock allowance, not a provider-token estimate. A missing sample is labeled
`unavailable` and never invented.

### 4. Turn-boundary auto algorithm

For each top-level Leader provider turn, before the provider call:

1. Re-read `session.budget.get`-equivalent status from the store and apply the
   effective limits to the live `SessionBudget` without resetting spend or
   reservations.
2. Re-arm the preemption deadline from the cumulative wall clock.
3. If the session is no longer auto, or is unlimited, disable the live auto
   controller for the rest of the run and continue with normal fixed/unlimited
   behavior.
4. Emit at most one `session.budget.forecast_warning` per low-runway threshold
   crossing at `warn_at_ratio`.
5. If `max_total_turns` is reached, pause with reason `max_total_turns`.
6. If runway is below `min_runway_turns` or a remaining ratio is at or below
   `pause_at_ratio`, request an atomic extension.
7. If extension is impossible at the current ceiling, pause with reason
   `budget_cap` and code `AUTO_BUDGET_CAP_REACHED`; never retry the same provider
   call and never switch to unlimited.
8. Only after these checks increment the cumulative turn counter, persist the
   state, and make the provider call.

An extension request carries additional token/cost/time amounts. The store uses
the larger of the policy step (`extend_ratio * initial`) and the requested runway
amount, clamps to the dimension ceiling, never moves below already-spent value,
and returns a typed no-op at the cap. A targeted request extends only the
dimensions it names, so a time-only extension cannot silently raise an unrelated
token or cost allowance; a fully default request extends every capped dimension
by one policy step. Concurrent extension calls serialize on one SQLite
`BEGIN IMMEDIATE` transaction, so the stored effective budget cannot jump past
the cap. A successful extension writes `session.budget.auto_extended`; a no-op
at the cap writes no event.

Before the token reservation, the top-level Leader also checks the exact
estimated request size against the remaining effective tokens. A fresh session
with no forecast samples can have a request that is larger than its initial
allowance; the runtime extends first, or pauses at the hard cap, instead of
letting the first provider call fail as a generic budget error.

Auto wall clock is cumulative across runs. The live clock is seeded from the
persisted `elapsed_secs`; pausing stops the active clock, so operator decision
time does not consume budget; resume shifts the clock forward by exactly the
paused interval. Every turn re-arms the remaining deadline, so an operator
top-up or auto extension takes effect at the next boundary. `max_turns_per_run`
bounds one run (default 60, hard maximum 200); `max_total_turns` bounds the
session.

### 5. Auto pause versus failure

An auto stop is an ordinary cooperative session pause, not a failed run. The
runtime sets session status to `paused`, emits `session.auto_paused` with the
bounded budget/forecast/reason payload, and waits. The auto decision is made
before the next provider call, and the wait loop starts no provider or tool
call. Reasons are `budget_cap`, `no_progress`, and `max_total_turns`.
Cancellation while paused returns `Cancelled`.

An operator resume continues the same run. `session.auto_resumed` records the
previous reason and whether the reset was operator-initiated. If the session is
resumed at the cap without a top-up, the next boundary pauses again; that is
intended. Operator top-up is an explicit `session.budget.set` (or CLI/Swift
equivalent), applied at the next turn without restarting the run. Auto never
turns itself into unlimited.

### 6. No-progress guard

The no-progress tracker is a pure, bounded, evidence-only state machine in
`vp-core::auto_progress`. It never reads model prose, never calls a provider or
tool, and never mutates a plan, budget, approval, worktree, or session status.
The runtime consults it after completed turns; the session controller performs
any pause or escalation.

Progress evidence is limited to durable events, including commits and merges,
test cache/runner results, completed plan tasks, artifact publication, workspace
landing, session checkpoints, durable file writes, and successful effect
settlement for those kinds. Read-only calls, repeated reads, model text,
bookkeeping, telemetry, cache/budget events, and retries do not reset the
counter. Failed tests, Verifier rejections, and integration failures are still
new evidence and reset the counter, but carry `success=false`; they never count
as task success.

Evidence identity is `sha256(kind, success, target)` where the target contains
only identifiers, paths, short hashes, and test digests. No file content, command
text, model text, URL, tool output, or credential is copied. Retention defaults
to 12 recent evidence rows and 128 recent hashes, with hard ceilings of 64 and
512. The turn scan is paged at 500 events and capped at 5,000 events; a truncated
scan with no progress evidence never creates a false pause and emits
`session.auto_state_warning` instead.

When `turns_without_progress >= no_progress_turns`:

- `pause` pauses at the next safe boundary with `session.auto_paused` reason
  `no_progress` and bounded evidence;
- `escalate` records one coordination note at the threshold and does not pause.

Only an explicit operator resume calls `reset_on_operator_resume`; it clears the
counter, keeps the evidence history, and excuses the currently open turn. A
later read-only turn counts again. The tracker state is stored in the opaque
`progress` field of `SessionAutoBudgetState`; the serialized state is capped at
64 KiB and fails closed when oversized.

### 7. Program and milestone reservations

Program dispatch uses a process-wide `ProgramBudgetLedger` keyed by data
directory plus program id. Before a milestone session is created, the controller
reserves the milestone's granted program allowance. Concurrent milestones see
sibling reservations under one lock, so two milestones cannot each receive the
full remaining program allowance. The grant also respects the milestone's own
remaining budget and divides the remaining program allowance across milestones
that can run concurrently. Release happens on failed start, terminal/cancelled
milestones, program cancellation, and the reconcile sweep; settled usage stays
authoritative in the program usage ledger.

The ledger is a scheduling guard for concurrent starts. It does not change the
session, plan-task, department, or project budget gates, and it never widens a
budget. Program sessions are created through the normal fixed-budget session
path with the granted token/cost values; they are not auto-budget sessions.

### 8. Conditional tool capabilities and bundles

`ToolCapabilities` adds eight advertisement-only flags:

`mcp_configured`, `remote_host_configured`, `archive_media_available`,
`browser_helper_available`, `computer_helper_available`,
`web_search_configured`, `artifacts_enabled`, `lsp_available`.

A static gate table maps conditional tools to required capabilities (MCP,
`remote_*`, archive/media, browser/computer, web search, LSP, and file-backed
artifact publishing). `tool_schemas_with_capabilities` applies the gates last,
after role policy and special-tool branches, so a gated tool cannot reappear
through another source. `read_pdf`, `read_image`, and `read_document` remain
unconditional because their pure-Rust parsers still return useful labeled
metadata when optional helpers are absent. Capability state is not
authorization: a present flag still goes through role policy, approvals,
contracts, write scope, sandbox, and effect receipts, while a hidden tool remains
discoverable through `tool_search`.

Two compatibility defaults are deliberate:

- `ToolCapabilities::default()` is permissive (all true), so the legacy
  `tool_schemas` path keeps identical schema bytes;
- a runtime that probes the environment starts from `ToolCapabilities::none()`
  and sets only confirmed flags.

The runtime now resolves one `ToolSurface` per run from the persisted budget
status. Fixed/unlimited sessions and `auto_tool_bundle=false` keep the standard
permissive surface. Auto sessions probe MCP, SSH execution host, media tools,
web search, and artifact policy; capability-probe errors stay permissive for
that dimension. The same surface is used by prompt sections, prompt hashes,
provider schemas, cache plan/warm, manual compaction/checkpoint, `tool_search`,
and Code Mode inner bindings, so a gated schema cannot reappear through a
different path.

`ToolBundleMode` adds `LeaderOrchestrator`, an opt-in read-only Leader/Manager
partition (target 44 built-ins; 43 requested for a generic Leader, 44 for a
coding Manager). It keeps inspection, planning, delegation, review, task
control, memory/sources, mailbox, and inline artifacts, and drops shell/process,
workspace writes, `run_script`, GUI, remote write/exec, and the MCP bridge. It
retains delegation because a child still runs under its own task worktree,
Verifier, and merge gates.

Auto Leader selection is deliberately conditional: the orchestrator surface is
used only when a stored TeamPlan has at least one task. A new/simple Auto session
with no plan keeps the standard writable surface, so the first coding task is
not blocked by a read-only orchestration bundle. A plan-probe failure also keeps
the standard surface. `auto_tool_bundle=false` is the explicit solo/manual
opt-out.

Worker/Labour task selection remains deterministic: missing or unknown task
kinds fall back to a role-aware default with
`reason=task_kind_unknown_role_default` and `fallback=true`, not to the `full`
catalog. A Worker keeps the coding surface (or its explicit research /
verification profile surface) so it can still inspect, design, review, and
delegate Labour; Labour and other roles use the profile default (`coding`,
`research`, `verification`, or `minimal`). Unknown roles still use `full`. A
bundle is frozen at run start; an explicit `tool_search` promotion is the only
turn-boundary revision, and it emits a priced `bundle.revision`/new prompt
prefix rather than silently changing the schema set.

### 9. Operator surfaces

RPC:

- `session.create` accepts `budget_mode` and `auto_budget`.
- `session.budget.get` returns mode, effective/hard limits, spend, forecast,
  policy, and bounded state.
- `session.budget.set` updates mode, unlimited flag, effective limits, hard caps,
  policy, and clear flags; it cannot set a limit below spent value.
- `session.budget.forecast` returns the read-only forecast.
- `project.auto_budget.get` / `project.auto_budget.set` read and update the
  project default.
- No standalone `session.budget.extend` RPC or CLI command exists; auto
  extension is runtime-internal, and operator top-up uses `session.budget.set`.

CLI:

- `vp session-new --budget-mode auto|fixed|unlimited`, plus `--auto`,
  `--max-token`, `--max-usd`, and `--max-hours`;
- `vp session-budget <session>`;
- `vp session-budget-set <session> --mode ... --hard-token-cap ...`;
- `vp session-budget-forecast <session>`;
- `vp project-auto-budget <project>`;
- `vp project-auto-budget-set <project> ...` (including `--no-progress-turns`,
  `--no-progress-action`, and `--auto-tool-bundle`).

Swift:

- New Session sheet: `Auto (recommended)`, `Fixed`, and `Unlimited` presets.
  Auto exposes one hard-cap field (USD or tokens) plus optional wall clock, not
  five independent budget fields.
- Settings → Automation: `AutoBudgetSettingsCard` for project defaults,
  extension ratio, caps, no-progress threshold/action.
- Stage: `StageBudgetCard` for mode, caps, spend, forecast/runway, extension
  count, and pause/no-progress reason; `SessionBudgetSheet` for edits. Values
  come from `session.budget.get`; the card does not infer a budget.
- The notification/event path refreshes on `session.budget.updated`,
  `session.budget.auto_extended`, `session.auto_paused`, `session.auto_resumed`,
  and `session.auto_state`.

Typed validation codes are wire values: `INVALID_BUDGET_MODE`,
`INVALID_BUDGET`, `INVALID_BUDGET_CAP`, `BUDGET_BELOW_SPENT`,
`AUTO_BUDGET_CAP_REQUIRED`, `AUTO_BUDGET_POLICY_REQUIRED`,
`AUTO_BUDGET_CAP_REACHED`, `INVALID_AUTO_BUDGET_POLICY`,
`INVALID_AUTO_BUDGET_STATE`, `BUDGET_MODE_CONFLICT`, and
`BUDGET_POLICY_NOT_APPLICABLE`.

## Invariants and security

- Auto budget never bypasses approvals, the shell/write sandbox, TaskContract
  intent authorization, write scope, Verifier/Integrator/merge gates, global
  capacity, preemption, or model/provider selection.
- Auto can widen only the effective session economic limits, and only up to the
  stored hard cap (and the tighter `max_multiplier` ceiling). It cannot select
  unlimited, erase spend, reset reservations, or hide a budget stop. Project,
  plan-task, and department budgets remain independent gates; extending the
  session limit does not create project budget headroom.
- Every automatic action is event-backed: `session.budget.updated`,
  `session.budget.auto_extended`, `session.budget.forecast_warning`,
  `session.auto_paused`, `session.auto_resumed`, and `session.auto_state` where
  operational state changes. Project policy updates use
  `project.auto_budget.updated` in the project audit table and manifest.
- No-progress evidence contains identifiers, paths, short hashes, and digests
  only. It excludes model text, file content, command text, URLs, tool output,
  and credentials.
- Operator resume and top-up are explicit. A pause is not success, a resume is
  not verification, and a forecast is not a guarantee.
- Program reservations are a scheduling guard only; they never grant approval,
  capacity, or budget beyond the existing gates.
- Capability flags and bundle membership are advertisement-only. They never
  widen role policy and never become an authorization boundary.

## Consequences

- A normal long-task session can be created with one hard cap instead of five
  manual limits. Fixed and unlimited remain available and unchanged.
- Auto sessions pause earlier than a hard failure would, preserving the run and
  the worktree for an operator decision. Resuming at the cap without a top-up
  will pause again by design.
- Deterministic extension makes spend bounded and auditable, but it can still
  consume the full hard cap; it is not a cost-saving claim.
- The cumulative wall clock excludes paused intervals, so an operator decision
  does not consume the time budget; a daemon crash can lose the elapsed segment
  since the last turn-boundary persist.
- Program dispatch no longer grants the same remaining allowance to parallel
  milestones, but the reservation ledger is process-local and not a distributed
  lock.
- Auto sessions now get capability-gated schemas and, once a TeamPlan exists,
  the smaller Leader orchestrator surface. Fixed/unlimited sessions keep the
  standard permissive surface; this is intentional compatibility, not a missed
  wiring.
- New tables and events increase manifest/bundle size and require replay,
  repair, import, and rebuild coverage. The auto row is additive and legacy
  fixed sessions keep their old budgets.

## Remaining explicit gaps

- Auto-budget state is SQLite-backed, but all decisions run in one daemon and
  there is no cross-process or cross-host budget fencing. Program reservations
  are process-local: a daemon restart starts that ledger empty and releases stale
  entries only through reconcile.
- Auto extension is evaluated at the top-level Leader provider boundary. The
  deterministic plan-resume path seeds the cumulative auto clock but does not
  call `AutoBudgetRuntime::before_turn`/`finish_turn`; child Worker/Labour
  provider calls can consume the effective budget between Leader turns.
- Program and intake dispatch create sessions through the normal fixed-budget
  path and do not expose a budget-mode selector. Auto is not the default, and
  scheduler ticks only resume or dispatch existing sessions.
- `critical_path_turns` is always `None`; forecast is ledger/sample based and is
  not a vendor tokenizer or a plan-DAG completion estimate.
- `auto_tool_bundle=false` is the explicit solo/manual opt-out; there is no
  finer-grained per-section toggle. Switching capability flags or bundle mode
  changes the advertised bytes and `tools_hash`, so the prompt/cache prefix
  changes; mode-aware warm/cache-plan paths use the same surface.
- Browser/computer availability is not PATH-probed (the packaged helpers live in
  the app bundle), so those schemas stay advertised unless an explicit disable
  environment flag is set. This is conservative rather than a measured
  reduction.
- `session.auto_paused` / `session.auto_resumed` are audit events, but
  `budget_projection_from_events` does not yet fold every operational pause field
  back into the auto row; a forced rebuild can restore budgets/caps while losing
  a pause reason or timestamp.
- There is no standalone operator "extend by" RPC/CLI; top-up is a full
  `session.budget.set` update.
- No live 50+ turn long-task soak with real provider latency, compaction,
  interruption/resume, and parallel Workers is recorded in this worktree. The
  deterministic runtime tests use a fake provider.
- The no-progress detector is conservative evidence bookkeeping, not semantic
  correctness: negative evidence resets the counter, deduplication is bounded,
  and a truncated scan intentionally declines to pause.

## Verification

Final verification after the host reboot (2026-11-21):

- `cargo fmt --all -- --check` — passed.
- `cargo check --workspace --all-targets` — passed.
- `cargo clippy --workspace --all-targets -- -D warnings` — passed.
- `cargo test --workspace` — **4,513 passed, 0 failed, 6 ignored** (the ignored
  cases are the existing live-provider tests).
- `swiftformat --lint apps/macos/VerlinPark apps/macos/VerlinParkTests` —
  passed.
- `xcodebuild build-for-testing` and `test-without-building` on macOS —
  **455 app tests + 132 computer-helper tests passed, 0 failed**.

Targeted deterministic suites that ran during the round also passed:
store auto-budget 11, core auto-budget runtime 20, protocol auto-budget 6,
program 49, tool-bundle 81, no-progress 21, and the full core integration set
(including identity prompts and the adversarial corpus). No live provider call
was made for this round. The deterministic 50-turn soak uses a fake provider;
the limitation about a real-latency long-task soak below remains accurate.

Commands:

```bash
cargo test -p vp-protocol
cargo test -p vp-store auto_budget
cargo test -p vp-core auto_budget
cargo test -p vp-core auto_progress
cargo test -p vp-core tool_bundle
cargo test -p vp-core program
cargo test -p vp-cli
cargo test -p vp-daemon
```

Full workspace and GUI:

```bash
cargo test --workspace
cd apps/macos && xcodebuild -project VerlinPark.xcodeproj -scheme VerlinPark \
  -configuration Debug -destination 'platform=macOS' \
  -derivedDataPath build CODE_SIGNING_ALLOWED=NO test
```

Relevant coverage lives in `crates/vp-core/src/agent/auto_budget_runtime_tests.rs`,
`crates/vp-store/src/store/auto_budget.rs`, `crates/vp-core/src/auto_progress.rs`,
`crates/vp-core/src/tool_bundle.rs`, `crates/vp-core/src/program.rs`,
`crates/vp-core/src/engine/auto_budget.rs`, the protocol type tests, and
`apps/macos/VerlinParkTests/AutoBudgetTests.swift`. A live soak must be run
separately with explicit provider credentials in the environment; keys must
never enter the repository, events, logs, or UI.

## Alternatives considered

- Keep budgets fully manual: rejected because it requires five economic
  decisions for every long task and still has no no-progress or forecast
  behavior.
- Auto-switch to `unlimited` at the cap: rejected because it makes spend
  unpredictable and defeats the hard cap.
- Raise the budget whenever a provider or Worker reports exhaustion: rejected
  because it hides overruns and can double-bill.
- Fail the session at the cap: rejected because it destroys a resumable run and
  conflates an operator decision with a runtime failure.
- Use an LLM to forecast or decide extensions: rejected because it is
  non-deterministic, costs tokens, and adds a hidden decision path.
- Detect no-progress from model prose or a second summarizer call: rejected
  because prose is not evidence and a summarizer adds cost and latency.
- Use SQLite for program reservations: deferred because the existing program
  controller is same-host and process-local; a durable cross-host reservation
  needs a separate fencing design.
- Always advertise the full tool catalog and rely on `tool_search`: rejected
  because schema bytes are paid on every provider call and the stable prefix is
  part of the cache identity.

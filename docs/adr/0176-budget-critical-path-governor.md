# ADR-0176: Budget and critical-path governor

- **Status:** Accepted
- **Date:** 2026-11-11
- **Deciders:** VerlinPark runtime maintainers
- **Related:** ADR-0122 (plan task budgets), ADR-0141 (session time/token budgets),
  ADR-0157 (programs), ADR-0170 (tool error contract),
  `docs/SCALING_PROCESS_SOURCE_REMOTE_BUDGET_PLAN.md` F5

## Context

Plan tasks, departments, projects, and sessions already have hard token/cost
budgets, an in-flight reservation registry, and a deterministic task scheduler.
Those gates stop overspending, but they are blind to the plan's dependency
shape. A cheap side task can consume the last budget that a long blocking chain
needs, and the scheduler has no deterministic way to say "not now" without
failing the session or pretending the plan is complete.

The governor addresses only that allocation problem. It is not a second budget
ledger, not a scheduler replacement, and not a model router.

## Decision

### 1. Pure critical-path math

`vp-core/src/critical_path.rs` computes the heaviest remaining chain in the
stored `TeamPlan` DAG:

- **Task weight** is the declared `token_budget` when it is positive and at
  most `MAX_TASK_WEIGHT_TOKENS` (100,000,000). Otherwise weight is
  `max(priority, 1) * 1,000` tokens, with priority clamped to `-100..=100`.
- **Statuses:** `completed` and `skipped` are removed from the remaining graph
  and satisfy their dependents. `cancelled` is also removed because an operator
  explicitly abandoned that branch. Failed, blocked, interrupted, paused,
  queued, running, and pending tasks remain because they can still block or be
  retried.
- **Path:** longest dependency-respecting chain to a sink, summing task weights.
  The result is the task-id sequence, cumulative weight, remaining session
  budget, plan revision, completeness, node/edge counts, and an `inputs_hash`.
- **Tie-breaking:** heavier path first; equal weights use the lexicographically
  smaller task-id sequence. The DP is processed in topological order, so the
  same plan/status/revision/budget inputs always produce the same path.
- **Fail closed:** duplicate task ids, unknown dependencies that are not
  completed/skipped, cycles, more than 100 nodes, or more than 1,000 edges
  return a typed `BUDGET_PATH_*` error. The governor never guesses through a
  malformed DAG.
- **Revision identity:** the plan revision and the status of every task are in
  the hash. A plan revision, completion, cancellation, or budget change moves
  `inputs_hash`, so a stale decision can be recognized without wall-clock time.

The path is a chain, not a set of all blocking tasks. Protecting the heaviest
chain is a deterministic heuristic; it does not claim to be an optimal
multi-path schedule.

### 2. Project policy

A persisted `CriticalPathPolicy` is stored per project in SQLite
(`project_critical_path_policies`), copied into `projects.jsonl`, and restored by
`db-rebuild`:

```json
{
  "enabled": false,
  "reserve_ratio": 0.25,
  "throttle_at_ratio": 0.35,
  "critical_boost": 25,
  "min_task_budget": 1000
}
```

- `enabled` defaults to false. A missing row or an older manifest decodes as
  disabled, so upgrading cannot change task admission.
- `reserve_ratio` is clamped to `0.0..=1.0`; `0.0` disables the ratio buffer.
- `throttle_at_ratio` is clamped to `0.0..=1.0`; `0.0` disables the throttle
  zone and `1.0` throttles whenever unfinished critical work remains.
- `critical_boost` is a scheduling-order priority boost (`0..=100`). It never
  edits the plan, department ownership, or provider/model choice.
- `min_task_budget` is a hard admission floor (`0..=10,000,000` tokens).
- Invalid RPC values are rejected; stored values are normalized.

### 3. Reserve and admission math

For one decision with `remaining = limit - spent - in_flight`:

```text
ratio_reserve   = ceil(remaining * reserve_ratio)
protected       = min(remaining, path_weight + ratio_reserve)
free_headroom   = remaining - protected
throttled       = path_weight > 0
                  && remaining <= floor(limit * throttle_at_ratio)
```

- A completed path (`path_weight == 0`) protects nothing. The ratio buffer is
  released instead of permanently starving the tail of the plan.
- An unbounded session/project has no reserve math and admits with
  `no_budget`; the governor cannot reserve from an unknown limit.
- Under pressure (throttled or `remaining <= protected`), critical-path tasks
  sort first. Otherwise ordering is `effective_priority + critical_boost` for
  critical tasks, then enqueue time, then task id.
- **Critical task:** admitted even if its declared weight exceeds the remaining
  budget, because the hard session/project/department gate still applies. It
  consumes the protected pool.
- **Non-critical task:** admitted only when its weight fits `free_headroom`;
  otherwise deferred with `BUDGET_RESERVED`. In the throttle zone it is
  deferred with `BUDGET_THROTTLED`.
- **Floor:** when `remaining < min_task_budget`, every newly considered task is
  deferred with `BUDGET_EXHAUSTED`.
- The governor never adds budget, borrows across scopes, changes a model or
  provider, or writes usage. It is an admission filter in front of the existing
  reservation registry.

### 4. In-flight provider reservations

The agent pre-flight computes a static `GovernorRunContext` at run start:
task id, critical flag, path ids, path weight, policy, plan revision, and input
hash. Before each provider call it derives a `GovernorGate` from the current
session/project remaining budget.

`BudgetRegistry::grant_governed` evaluates the gate before hard scope checks:

- critical requests are not capped by the reserve;
- non-critical output is clamped to `remaining - protected - input`;
- if that leaves less than `MIN_OUTPUT_TOKENS` (64), the call is rejected with
  `BudgetReason::Reserved` or `BudgetReason::Throttled`;
- hard task/department/project/session limits still run afterward and can only
  reject further.

A provider-call deferral becomes a task-scoped `BudgetStop` (non-abortive), so
the Worker task is marked blocked/deferred while the session continues. Blocked
tasks remain retryable by the existing scheduler/retry paths.

The gate is static for one run. A plan revision or task completion is observed
by the next run or scheduler tick, not mid-stream.

### 5. Scheduler integration and release

`TaskScheduler::ready_tasks_governed` evaluates the same pure function with the
scheduler's existing priority/aging/retry ordering. It returns the admitted
tasks and the deferred tasks separately.

- Disabled policy returns exactly the previous ready list, with `governor=None`.
- Enabled policy admits critical tasks first under pressure and keeps deferred
  tasks queued. The plan runner records the admission decision and ends the
  batch without marking deferred tasks blocked or cancelled.
- Deferrals are reconsidered on the next tick from the then-current budget and
  status; there is no persisted deferral table to drift. A completed critical
  path releases the ratio buffer deterministically.
- No separate starvation timer is introduced: existing priority aging still
  applies to admitted work, while reserve deferrals are bounded by the path
  completing, budget growth, or an operator policy change.

### 6. Events, preview, and explainability

Every applied admission writes one `budget.governor.decision` event; every
provider-call deferral writes one event with the task id and requested tokens.
The payload carries action, reason, Q3 code, path, path weight, limit/spent/
reserved/remaining/protected tokens, admitted/deferred lists, `inputs_hash`, and
a bounded detail string.

Reads are bounded to the newest 100 decisions. `governor.preview` evaluates the
same pure function with an optional remaining-token override and writes no
event. `session.budget.governor` returns live policy, path, reserve math,
admitted/deferred decisions, and bounded history. `critical_path.policy.get/set`
own the project policy.

RPC surface:

- `critical_path.policy.get` / `.set`
- `session.budget.governor`
- `governor.preview`

CLI surface:

- `vp governor <session>`
- `vp critical-path <session> [--remaining-tokens N]`
- `vp governor-preview <session>`
- `vp governor-policy <project>`
- `vp governor-policy-set <project> ...`

SwiftUI surface: the Efficiency card shows the path, protected/remaining
tokens, deferred tasks, and the last decision; the Stage context popover shows
a compact governor pill. Deferral codes use the Q3 catalog:
`BUDGET_RESERVED`, `BUDGET_THROTTLED`, `BUDGET_EXHAUSTED`, and the
`BUDGET_PATH_*` path errors.

### 7. Limits

- This is a same-daemon, same-project admission controller. It is not a
  cross-host or cross-daemon economic scheduler, and it does not coordinate
  distributed reservations.
- Reserve math uses the session token budget when present, otherwise the
  project token budget. It does not model provider price changes, cache pricing,
  or future retry cost.
- Path weight uses declared task budgets, not observed per-task spend. A running
  critical task is still counted at full weight until the next status
  evaluation.
- In-flight reservations are visible through the daemon-wide registry; the
  scheduler fallback without a registry view undercounts them.
- The path is one chain. Multi-path/fan-out protection and weighted fair
  sharing across departments are out of scope.
- Preview ordering is deterministic but does not include the scheduler's live
  `next_attempt_at` gating; it is an explanation surface, not a claim to replay
  the exact future tick.
- Provider-call exactly-once, mid-stream resume, and automatic budget increases
  remain unimplemented.

## Test coverage

The implementation ships with deterministic unit and integration coverage:

- **DAG math:** linear chain, branching diamond, declared-vs-priority weights,
  equal-weight tie-break by full id sequence, completed/skipped/cancelled
  removal, failed/interrupted retention, empty plans, maximum-depth chains,
  wide DAG stability, duplicate ids, unknown dependencies, self-dependency,
  cycles, node/edge caps, declaration-order-independent hashing, revision and
  status hash movement, and weight caps.
- **Reserve math:** `0.0`, `0.25`, `0.5`, and `1.0` ratios; NaN and out-of-range
  normalization; protected/headroom caps at remaining; throttle boundary
  inclusive at the computed floor; `0.0` throttle no-op; `1.0` pressure;
  `min_task_budget` zero and positive floors; no-budget and completed-path
  states.
- **Admission:** critical-first under pressure, critical boost ordering,
  deterministic deferred priority order, non-critical reserve rejection,
  throttle rejection, exhaustion, declared-budget admission, path completion
  release, repeated-evaluation identity, preview override determinism, policy
  hash movement, and plan-revision reset.
- **Registry:** non-critical output clamping, no-minimum rejection, throttle
  rejection, critical bypass, disabled-gate no-op, and per-session reservation
  accounting.
- **Events:** admission/deferral payload shape and bounds, malformed-event
  skipping, bounded history, and full protocol round-trips.
- **Runtime:** engine status/preview/run-context determinism, preview writes no
  event, disabled policy writes no event, typed path errors in status, policy
  validation/persistence/manifest rebuild, and task-scoped non-abortive
  governor budget stops.
- **Surfaces:** CLI flag parsing, human rendering of path/reserve/deferrals/
  errors, Swift policy/status/failure decode, governor metrics, and the
  Efficiency snapshot carrying the governor.

The existing budget, reservation, scheduler, plan, merge, and approval tests
remain unchanged and green; the governor defaults to disabled, so those paths
retain their previous behavior.

## Consequences

- Operators can opt into protecting a blocking chain without changing any hard
  budget, approval, sandbox, Verifier, or merge gate.
- Every deferral is explainable and replayable from the event log.
- Disabled defaults preserve current behavior exactly.
- A malformed plan fails the governor closed instead of silently producing an
  arbitrary path.
- Critical-path protection can delay low-priority side work for a long time.
  This is intentional under pressure; operators can lower `reserve_ratio`,
  disable throttling, or disable the policy.

## Non-goals

- No hidden model/provider routing or automatic model downgrade.
- No budget bypass, silent borrowing, or automatic limit increase.
- No automatic plan rewrite, Manager creation/reassignment, or task deletion.
- No cross-host/distributed economic controller.
- No claim that the governor makes a plan globally optimal.

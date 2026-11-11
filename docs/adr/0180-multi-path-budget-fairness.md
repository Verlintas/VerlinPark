# ADR-0180: Multi-path budget fairness

- **Status:** Accepted
- **Date:** 2026-11-12
- **Deciders:** VerlinPark runtime maintainers
- **Related:** ADR-0176 (budget and critical-path governor), ADR-0122
  (plan task budgets), ADR-0141 (session time/token budgets), ADR-0170
  (tool error contract), `docs/LONG_RUN_OPERATIONS_HARDENING_PLAN.md` H4

## Context

ADR-0176 added a deterministic critical-path governor. It protects the single
heaviest remaining chain in the stored `TeamPlan` DAG, which is a safe and
explainable heuristic but not a fair one. A plan with several independent
blocking chains - for example a large implementation branch, a verification
branch, and a documentation branch - can spend the whole protected reserve on
the heaviest chain while the smaller chains never receive provider budget.
Under pressure those paths are eligible for later ticks, but a continuously
heavy path can keep winning the ratio buffer indefinitely.

The failure mode is not overspending. Hard task/department/project/session
budgets, the in-flight reservation registry, and the provider pre-flight clamp
already prevent that. The failure mode is starvation and explainability: an
operator cannot see which parallel chains are funded, which are not, and why.

H4 adds a multi-path allocator without changing the F5 single-path behavior.
The governor remains an admission filter in front of the existing budget
registry. It does not route models, add budget, write usage, or bypass any hard
gate.

## Decision

### 1. Additive project policy

`CriticalPathPolicy` gains five serde-default fields. A missing row, an older
manifest, or an older RPC payload decodes to the F5 behavior.

```json
{
  "enabled": false,
  "reserve_ratio": 0.25,
  "throttle_at_ratio": 0.35,
  "critical_boost": 25,
  "min_task_budget": 1000,
  "mode": "single",
  "max_paths": 4,
  "department_min_ratio": 0.0,
  "observed_weight_ratio": 0.0,
  "starvation_after_secs": 1800
}
```

- `mode` is `single` (default) or `multi`. Unknown values are rejected by the
  enum decoder; the CLI only accepts explicit `single`/`multi` aliases.
- `max_paths` is clamped to `2..=16` (`MIN_MAX_PATHS..=MAX_MAX_PATHS`). The
  lower bound keeps a "multi" table meaningful; the upper bound keeps the
  table, event payload, and UI bounded.
- `department_min_ratio` is a per-department minimum share of the protected
  reserve, clamped to `0.0..=1.0`. `0.0` disables floors.
- `observed_weight_ratio` blends observed usage-ledger spend into path weights,
  clamped to `0.0..=1.0`. `0.0` uses declared task budgets only.
- `starvation_after_secs` grants deterministic aging credit to long-queued
  tasks after this many seconds, capped at seven days. `0` disables aging.

Invalid RPC values are rejected before persistence. Stored and returned values
are normalized. The policy remains a JSON row in
`project_critical_path_policies`, copied into `projects.jsonl`, and restored by
`db-rebuild`; no schema migration is required because the row is already JSON.

Single mode does not use any of the new fields. Its path, reserve, ordering,
event shape additions, and error behavior stay identical to F5. The new fields
are present in the serialized policy and are additive for old clients.

### 2. Bounded path enumeration

`vp-core/src/multi_path.rs` enumerates maximal remaining root-to-leaf paths in
the partially completed DAG:

- **Remaining graph.** `completed` and `skipped` tasks are removed and satisfy
  their dependents; `cancelled` tasks are removed because an operator abandoned
  them; failed/blocked/interrupted/queued/running/pending tasks remain.
- **Roots and leaves.** A root has no remaining dependency; a leaf has no
  remaining dependent. Every root-to-leaf visit is one maximal path.
- **Determinism.** Roots and dependents are sorted by task id before traversal.
  No hash-map iteration order reaches the table.
- **Validation.** The F5 single-path computation runs first. Duplicate task
  ids, unknown dependencies that are not satisfied, self dependencies, cycles,
  more than 100 tasks, or more than 1,000 remaining edges fail closed with the
  existing `BUDGET_PATH_*` Q3 codes. The multi-path pass never walks a graph
  that F5 rejected.
- **Enumeration bound.** At most `MAX_ENUMERATED_PATHS = 4,096` paths are
  collected. Hitting the bound sets `truncated = true` and stops the DFS.
  Truncation is reported through the reserve/status payload; the allocator
  never claims to have enumerated an exponential DAG completely.
- **Selection bound.** Paths are sorted by weight descending, then by
  lexicographic task-id sequence, and the top `max_paths` are selected. Path
  ids are ranks in this table: `#0` is the heaviest.

A task shared by several paths (a common prefix) is critical if any selected
path contains it. Its deficit is the largest deficit among those paths, so a
shared task inherits its least-funded branch.

### 3. Path weights

For each selected path:

```text
declared = min(MAX_PATH_WEIGHT_TOKENS, sum(task_weight_tokens(task)))
observed = min(MAX_PATH_WEIGHT_TOKENS, sum(ledger_tokens(task)))
observed = 0                       if observed_weight_ratio == 0
observed = min(observed, 2 * declared)
blended  = round(declared * (1 - r) + observed * r)
weight   = clamp(blended, 1, MAX_PATH_WEIGHT_TOKENS)
```

- `task_weight_tokens` is the F5 weight: declared `token_budget` when positive
  and within `MAX_PATH_WEIGHT_TOKENS`, otherwise `max(priority, 1) * 1,000`.
- Observed tokens are input+output tokens per task from the usage ledger. They
  are evidence, not a second ledger: the governor never writes usage and never
  treats an estimate as fact.
- A path with no observed spend keeps its full declared weight. An unstarted
  path is not punished with a near-zero weight.
- Observed spend is clamped to twice the declared path weight. Without that
  bound one runaway path could dominate every subsequent decision.
- Blending uses basis points so the result is an integer and identical across
  platforms. Default `r = 0.0` reproduces declared-only weighting.

### 4. Reserve pool and water-filling

Let `remaining = limit - spent - in_flight` and `W = sum(selected weights)`.

```text
ratio_reserve = ceil(remaining * reserve_ratio)
protected     = 0                                  if the plan is complete
protected     = min(remaining, W + ratio_reserve)  otherwise
```

This is the same pool shape as F5: the selected path demand plus the ratio
buffer, bounded by what is actually left. Hard budgets remain authoritative.

The pool is allocated in two stages.

**Stage 1 - department floors.** For each department `d` touched by a selected
path:

```text
dept_weight[d] = sum(weight of paths touching d)      (each path once)
floor[d]       = floor(pool * department_min_ratio
                       * dept_weight[d] / W)
```

If the sum of department floors exceeds the pool, all floors are scaled down
proportionally. Each department floor is then split across its paths by path
weight; a path floor is the sum of its department shares, capped at the path's
own demand. Integer division under-allocates rather than over-allocates, so
floors can never exceed the pool. Capped or fractional tokens flow back into
stage 2 instead of disappearing.

**Stage 2 - weighted water-filling.** Each path starts at its floor. The
remaining pool is distributed in rounds proportional to path weight:

- a path whose proportional share exceeds its unfunded demand is capped at
  that demand;
- the leftover is redistributed in the next round;
- when no path is capped, the integer rounding leftover is assigned one token
  at a time by largest remainder, with path id as the tie-break.

The allocator is bounded by the path count (at most 16 rounds of
redistribution). The result sums to `min(pool, sum(selected weights))` for
floors within demand. Every path reports `weight_tokens`, `floor_tokens`,
`allocation_tokens`, and `deficit_tokens = weight - allocation`.

The same pass emits a bounded department table: department name, total path
weight touching it (each path counted once), department floor, and the path
ranks that touch it. Rows are sorted by weight then name and capped at
`MAX_DEPARTMENT_ROWS = 32`. The table explains the floor stage; it is not a
second allocation ledger, and shared prefixes are not double-counted.

### 5. Admission, critical membership, and aging

Admission keeps every F5 gate:

- `remaining < min_task_budget` defers with `BUDGET_EXHAUSTED`;
- a critical task is admitted even if its declared weight exceeds the remaining
  budget, because the hard session/project/department registry still applies;
- the throttle zone defers non-critical work with `BUDGET_THROTTLED`;
- a non-critical task larger than the free headroom defers with
  `BUDGET_RESERVED`;
- the provider pre-flight gate clamps non-critical output to
  `remaining - protected - input` and never blocks a critical request.

Multi mode changes membership and ordering only:

- **Critical membership** is the union of selected path task ids, not just the
  single F5 chain.
- **Pressure order.** Under pressure, critical tasks sort first. Within that
  group, larger path deficit sorts first, then the boosted effective priority,
  then enqueue time, then task id. Non-path tasks have deficit zero.
- **Non-pressure order.** The same deficit/priority/age order applies so a
  persistently underfunded path is visible and reconsidered, but no hard gate
  is relaxed.
- **Starvation aging.** In multi mode a task queued for at least
  `starvation_after_secs` receives `floor(age / starvation_after_secs) * 5`
  priority points, capped at 100. Aging uses the daemon's evaluation timestamp
  and the scheduler's enqueued-at value; the pure evaluator never reads the
  clock. `0` disables aging. Single mode always uses zero credit, preserving
  F5 ordering exactly.

The deferred list remains in decision order, and `deferred_order` exposes the
same ordering explicitly for preview/UI. Deferrals are still not persisted as a
table: the next tick recomputes from the then-current budget, statuses, plan
revision, and ledger.

### 6. Preview, events, and explainability

`governor.preview` and `session.budget.governor` evaluate the same pure
function. Preview uses optional remaining-token and mode overrides and writes
no event. The mode override (`single` or `multi`) evaluates the stored policy
under the requested mode for comparison; it never persists the policy, and an
unknown mode is an invalid-parameter error. The status payload adds:

- `reserve.mode`, `reserve.path_count`, `reserve.allocated_tokens`,
  `reserve.floor_tokens`, `reserve.enumerated_paths`,
  `reserve.paths_truncated`;
- `paths[]` with path id, task ids, departments, blended/declared/observed
  weight, floor, allocation, and deficit (at most 16 rows);
- `departments[]` with department weight, floor, and touching path ids (at
  most 32 rows);
- `deferred_order`;
- per-decision `path_id`, `path_deficit_tokens`, and `starvation_credit`.

`budget.governor.decision` admission events carry the same `mode`, bounded
`paths[]`, `departments[]`, `allocated_tokens`, and `deferred_order`. Provider-call denial events
carry `mode` and an empty path table because the admission event owns the
table; their shape stays stable for replay clients. History remains bounded to
the newest 100 decisions. `inputs_hash` includes the F5 inputs hash, every
multi-path policy field in basis points, the selected path table, and the
observed-spend map (sorted by task id). A plan revision, status change, policy
change, observed-spend change, or allocation change moves the hash.

### 7. Surfaces

RPCs remain backward compatible:

- `critical_path.policy.get` / `.set`
- `session.budget.governor`
- `governor.preview`

CLI:

- `vp governor <session>` renders the mode, fairness line, bounded path table,
  and deferred order;
- `vp critical-path <session> [--remaining-tokens N] [--mode multi]` and
  `vp governor-preview <session> [--mode multi]` reuse the same status payload;
  `--mode` is a preview-only override and never edits the project policy;
- `vp governor-policy <project>` prints mode/fairness fields;
- `vp governor-policy-set <project> --mode multi --max-paths 6
  --department-min-ratio 0.2 --observed-weight-ratio 0.5
  --starvation-after-secs 600` merges only the provided fields into the stored
  policy.

SwiftUI:

- the Efficiency card shows a `MULTI-PATH` badge, path count, allocated/floors,
  starvation/observed settings, up to four protected-path rows with a funding
  bar and deficit color, and up to three department floors;
- the Stage governor popover shows the mode, path count, allocation, the top two
  paths with deficits, and a compact floor line;
- all values are daemon-provided. The client never recomputes allocations and
  falls back to the deferral list if an older daemon omits `deferred_order`.

### 8. Failure behavior

- Malformed DAGs fail closed before enumeration with the existing
  `BUDGET_PATH_*` codes. The status `error` field carries the typed code,
  message, retryability, and hint.
- Deferrals use `BUDGET_RESERVED`, `BUDGET_THROTTLED`, and `BUDGET_EXHAUSTED`
  from the Q3 catalog. No new code is needed; the allocation table is
  explanatory data, not an error.
- Policy RPC validation rejects unknown modes, out-of-range paths/ratios, and
  oversized starvation windows before persistence. Stored values are clamped.
- A disabled policy still computes and exposes the path table for inspection,
  but admits every candidate with the F5 `disabled` reason and writes no event.
- A completed plan releases the pool and admits remaining candidates with
  `path_complete`, exactly like F5.

### 9. Invariants and audit checklist

The implementation is expected to preserve these invariants. They are covered
by unit and integration tests, and an operator can check them from the status
payload without reading the source.

1. **No budget bypass.** `protected <= remaining`; every path allocation is
   `<=` its blended weight; `allocated <= protected`; floor sum is `<=`
   `allocated`. Hard session, project, department, and task limits still run
   after the governor and can only reject further.
2. **No model routing.** The allocator only reads the plan, statuses, policy,
   and usage-ledger tokens. It never reads a provider name, model id, price, or
   secret, and never writes a model/provider choice.
3. **Determinism.** For the same session/project/plan revision/statuses/policy/
   remaining budget/observed map, path order, department order, allocations,
   decisions, deferred order, and `inputs_hash` are identical across calls and
   platforms.
4. **Fail closed.** A malformed DAG never reaches enumeration. Truncated
   enumeration is reported, not hidden. Unknown policy modes and out-of-range
   values are rejected before persistence.
5. **Single-mode compatibility.** `mode = single` uses the F5 computation and
   ordering with no starvation credit and no path water-filling. The only new
   data is additive status/event fields; deferral codes and reserve math are
   unchanged.
6. **Bounded everything.** Paths per decision `<= 16`, enumerated paths
   `<= 4,096`, department rows `<= 32`, decision history `<= 100`, deferred
   ordering and event lists `<= 100`. No payload grows with plan size without a
   cap.
7. **No hidden persistence.** Preview writes no event. Policy writes go through
   the existing SQLite/manifest path. The allocation itself is recomputed, not
   stored as a second source of truth.
8. **Operator control.** Disabling the policy, switching to single mode,
   lowering `max_paths`, lowering `department_min_ratio`, or setting
   `observed_weight_ratio = 0` each restore a simpler, more predictable
   behavior without deleting plan or usage data.

## Tests

The implementation ships deterministic coverage at every layer:

- **Allocator:** two/three independent chains, tight-budget proportional fill,
  water-fill capping/redistribution/rounding/property bounds, department
  floors, floor scaling, shared-department counting, bounded department rows,
  observed-spend blending/clamping, max-path bounds, enumeration
  cap/truncation, path-id tie-break, revision reset, empty and completed plans,
  unbounded budgets, starvation credit boundaries, membership union/deficit
  lookup, oversize and edge-bound rejection.
- **Governor:** multi table and reserve math, third-path deferral with typed
  code, deficit metadata on decisions, single-mode compatibility row, observed
  reorder/hash movement, starvation reorder, revision reset, completed-plan
  release, unbounded table, event payload paths/deferred order, mode-switch
  hash movement.
- **Engine:** status/preview determinism with paths and departments, preview
  mode override without persisting the policy, observed ledger spend from
  SQLite, run-context union membership and path allocation, governed scheduler
  admission with deferral, persisted admission event, fairness policy
  validation/persistence/manifest/reopen/db-rebuild, revision reset after
  completion, and disabled multi policy visibility.
- **Store:** multi-policy field round-trip and normalization, usage-ledger
  `input+output` aggregation by task with null-task exclusion.
- **Protocol:** policy defaults/decode/normalize/validate, mode aliases and
  unknown rejection, path table partial decode, reserve/status round-trip,
  decision event with paths/deferred order.
- **CLI:** new policy flags and preview mode flags parse/validate, fairness
  policy rendering, path/department table and deferred-order rendering,
  single-mode no-op rendering.
- **Swift:** policy fairness decode/round-trip, multi status path and
  department decode, metrics formatting, deferred-order fallback, multi
  decision decode, empty single-mode table.
- Existing F5 governor, budget registry, scheduler, plan, and merge tests stay
  green. Defaults are `enabled = false`, `mode = single`, so an upgrade does
  not change admission.

## Limits

- This is a same-daemon, same-project admission controller. It is not a
  cross-host scheduler, distributed reservation service, or fair-share
  controller across projects.
- Path enumeration is bounded at 4,096. On a pathological diamond DAG the
  table is a deterministic prefix, not a proof of the globally heaviest K
  paths. Truncation is reported.
- Weights use declared task budgets and clamped observed spend, not provider
  price changes, cache pricing, forecast retry cost, or wall-clock demand.
  Observed spend is input+output tokens only; cost blending is not implemented.
- Department floors are proportional to path weight and split by integer
  arithmetic. They are a fairness heuristic, not a formal max-min optimum.
- Starvation aging is a priority credit, not a guarantee of future admission.
  Hard budgets, throttle, exhaustion, and capacity can still defer a task.
- In-flight reservations are visible through the daemon-wide registry when the
  caller supplies them; the scheduler fallback without a registry view
  undercounts reservations, as in F5.
- The path table is not a persisted projection. It is recomputed on each status,
  preview, scheduler tick, or run start. Deferrals are reconsidered from live
  state rather than a deferral table.
- Provider-call exactly-once, mid-stream resume, automatic budget increases,
  and distributed economic scheduling remain unimplemented.

## Non-goals

- No hidden model/provider routing or automatic model downgrade.
- No budget bypass, silent borrowing, negative reserve, or automatic limit
  increase.
- No automatic plan rewrite, Manager creation/reassignment, or task deletion.
- No cross-project or cross-host fair-share allocation.
- No claim that water-filling produces a globally optimal schedule.
- No replacement of the Verifier, capacity, approval, sandbox, or merge gates.

## Consequences

- Operators can opt into protecting several parallel chains without losing the
  F5 single-chain behavior they already tuned.
- Every funded and unfunded path is visible in status, preview, events, CLI,
  and SwiftUI, with a stable input hash for replay comparison.
- Department floors and starvation aging bound the "one heavy path wins
  forever" failure mode without promising instantaneous fairness.
- The default remains disabled/single, so existing plans, tests, and budgets
  are unchanged until an operator explicitly selects multi mode.

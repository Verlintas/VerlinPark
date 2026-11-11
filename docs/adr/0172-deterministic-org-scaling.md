# ADR-0172: Deterministic organization scaling

- **Status:** Accepted
- **Date:** 2026-11-11
- **Supersedes:** none
- **Related:** ADR-0011 (four-level org), ADR-0021 (parallel Workers and write
  scopes), ADR-0024 (TeamPlan profiles), ADR-0032 (atomic event projections),
  ADR-0134 (Verifier gate), ADR-0153 (cache and preamble telemetry),
  ADR-0154 (efficiency dashboard), ADR-0161 (cross-process lease fencing).

## Context

VerlinPark has a four-tier organization (Leader → Manager → Worker → Labour).
Team caps are configured per daemon/session, and the opt-in
`VERLINPARK_AUTO_EXPAND_WORKERS` path can raise the Worker ceiling once a plan
is already running. That path is not a controller:

- it only raises Worker slots, never Labour;
- it has no cooldown, demand bound, budget guard, or decision trail;
- it never scales down;
- it has no preview or status surface;
- a plan resume can start with a ceiling that is unrelated to observed demand.

At the same time, the plan itself is the authoritative source of department
ownership. Automatically creating a Manager or rewriting a TeamPlan would
silently change who owns which task, invalidate a revision, and bypass the
explicit plan lifecycle. Manager growth must remain an explicit plan revision.

The runtime already has the pieces needed for a safe controller:

- a persisted task-DAG projection (`plan_tasks`) with priority, dependencies,
  status, attempts, and write scopes;
- a daemon-wide Worker/Labour capacity controller;
- an atomic event+projection store and a replay/repair/rebuild path;
- project-scoped policies stored in SQLite and the project manifest;
- a deterministic task scheduler and orchestration controller;
- a persisted usage ledger and project/session budgets.

## Decision

Add an opt-in, project-scoped `ScalingPolicy` and a pure deterministic
controller in `vp-core/src/scaling.rs`. The controller evaluates durable plan
state and observed slot pressure, emits a bounded decision, and applies only
Worker/Labour target changes through the existing slot reservation path. It
never creates agents, Managers, sessions, or plan tasks, and it never rewrites
a plan.

### Policy

`ScalingPolicy` is persisted in `project_scaling_policies`, included in the
project manifest, and restored by `db-rebuild`. All fields are additive and
serde-defaulted.

| Field | Default | Meaning |
| --- | --- | --- |
| `enabled` | `false` | Explicit opt-in. Existing projects never start scaling after an upgrade. |
| `min_workers` | `1` | Worker scale-down floor. |
| `max_workers` | `4` | Worker ceiling, clamped to the runtime hard cap of 10. |
| `min_labour` | `1` | Labour scale-down floor. |
| `max_labour` | `8` | Labour ceiling, clamped to the runtime hard cap of 40. |
| `expand_ready_threshold` | `2` | Minimum ready tasks before expansion. One fresh task does not churn the team. |
| `expand_critical_delay_secs` | `120` | Hysteresis before pre-expanding an idle/failed plan. A running plan with saturated slots may expand immediately. |
| `scale_down_idle_secs` | `600` | Continuous idle window before scale-down. |
| `cooldown_secs` | `120` | Minimum time between applied decisions for one plan revision. |
| `max_step` | `2` | Maximum slots moved by one decision, clamped to 10. |
| `budget_headroom_ratio` | `None` | Optional expansion guard. Unknown headroom fails closed when configured. |

Normalization clamps every field into the runtime ranges. `set` validates
first, so an explicit RPC/CLI mistake returns `SCALING_POLICY_INVALID` rather
than silently changing behavior. A plan revision change resets the cooldown
and idle-carry: new work should not inherit an old plan's quiet period. When a
policy is enabled it is authoritative for the bounded Worker/Labour starting
point, but it is still clamped to the runtime hard caps and never touches
Manager, Verifier, or Scout caps.

### Pure controller

`evaluate(policy, observation) -> ScalingDecision` has no I/O. The observation
contains:

- session, project, session status, and plan revision;
- task states, priorities, dependencies, scopes, and enqueue/retry times;
- current Worker/Labour targets and live reservation counts;
- daemon-wide Worker/Labour availability;
- an optional budget headroom ratio;
- the last applied action time/revision and idle-since markers;
- the evaluation time and runtime hard caps.

The controller computes a deterministic demand:

1. A task is ready when its status is retryable (`pending`, `interrupted`,
   `failed`, `blocked`, `cancelled`), all dependencies are completed/skipped,
   and its retry time has elapsed.
2. Ready tasks are ordered by priority descending, department ascending, then
   task id ascending.
3. Running tasks already occupy their write scopes.
4. Ready tasks are added greedily only when their scopes do not overlap a
   selected task. A task with no scope is a barrier, matching the scheduler's
   “unrestricted runs alone” rule.
5. Demand is the number of selected tasks, bounded by the hard Worker cap.

This makes write-scope conflicts reduce demand instead of inflating it, and
makes ties reproducible without a model call.

### Expansion

Expansion happens only when all of the following hold:

- the policy is enabled and a plan revision with tasks exists;
- demand is above the current role target;
- for a running session, live reservations saturate the current Worker target
  and, for a Labour expansion, the current Labour target (no idle slots in the
  role being expanded);
- for a non-running session, the oldest ready task has waited at least
  `expand_critical_delay_secs`, so a freshly queued plan is not pre-expanded
  before the scheduler has a chance to start it;
- the ready-task count meets `expand_ready_threshold`;
- the cooldown has elapsed for the same plan revision;
- the optional budget headroom check passes (unknown fails closed);
- daemon-wide Worker/Labour capacity has at least one free slot;
- the step is bounded by `max_step`, remaining demand, and observed global
  availability.

Worker and Labour expansion may happen in one bounded decision
(`expand_workers_and_labour`). Labour is capped by the resolved Worker target
and by `max_labour`. A Worker expansion that is blocked by global Worker
capacity does not expand Labour in isolation.

### Scale-down

Scale-down is evaluated only when no expansion occurred. It requires:

- the current target is above the role minimum;
- the live reservation count is below the target;
- the target has stayed above the observed count for at least
  `scale_down_idle_secs`;
- the target is at or above the live reservation count, plan demand, and the
  policy minimum.

Labour is reduced before Workers. When both are eligible, the decision reduces
Labour and records `scale_down_workers_pending_next_tick`; the next tick may
reduce Workers. A scale-down never removes or interrupts an agent with a
running task: the shared slot state clamps the applied target to the active
reservation count, and reservations and target changes share one mutex.

### Managers remain plan-owned

The controller compares runnable tasks with the scalable Worker ceiling. When
the ceiling cannot absorb the queue, it records `manager_recommended` plus
evidence that an additional department/Manager would require an explicit plan
revision. It never creates a Manager, reassigns a department, or edits the
plan. Manager count therefore remains a property of the stored TeamPlan.

### Application path

The existing per-run `SessionBudget` now owns a shared `ScalingSlots` object
with target ceilings and live Worker/Labour reservation counters. `reserve` and
`release` use that object when present; Manager/Verifier/Scout counters remain
unchanged. Legacy `auto_expand_workers` raises the shared target only while the
controller does not own the run; a managed target is not restored by the
legacy guard. A decision applies targets with the same mutex used by
reservations, so a concurrent reserve either happens under the old target or
after the new target, never between a check and an increment.

The daemon tick (`VERLINPARK_SCALING_TICK_SECS`, default 30, `0` disables)
evaluates every session with a stored plan in every opted-in project. It
persists each decision and applies non-hold decisions to the shared slot state.
It does not start sessions or tasks; the existing task scheduler and plan
resume path remain responsible for execution. All admission, approval,
write-scope, Seatbelt, Verifier, integration, merge queue, and budget gates are
unchanged.

A policy-disabled project is skipped entirely. A session without a plan is
skipped. Imported read-only archives are skipped by the tick, and preview
returns a hold with `imported_read_only`. Preview and status never persist or
apply. Setting `enabled=false`
releases managed targets for that project's next runs; a live run keeps its
held `Arc` and is never interrupted.

### Events and replay

Each applied or state-changing evaluation is persisted as `scaling_decisions`
and a `scaling.decision` event in one transaction; an identical repeated Hold is
not re-persisted every tick. The unique `(session_id, decision_key)` constraint
makes a repeated tick idempotent. `decision_key` is derived from the plan
revision, a policy hash, and the tick bucket; a policy change therefore creates
a new decision instead of being suppressed by an old tick.

The decision payload includes action, current/target Worker and Labour slots,
active counts, ready/running task counts, ordered reasons, `inputs_hash`,
manager evidence, apply state, and idle-since markers. `inputs_hash` is a
SHA-256 over a canonical, sorted map of the policy, plan/task state, slot
counts, global capacity, budget headroom, previous decision markers, and
evaluation time.

`session.replay` compares the projection with decisions reconstructed from
events. `session.replay {repair:true}`, `session.rebuild`, bundle import, and
`db-rebuild` replace the projection from the event stream. The project policy
is part of the project manifest and survives `db-rebuild`.

### RPC, CLI, and UI

- RPC: `scaling.policy.get`, `scaling.policy.set`, `scaling.preview`,
  `scaling.status`.
- CLI: `vp scaling`, `vp scaling-preview`, `vp scaling-policy`,
  `vp scaling-policy-set`.
- Swift: Manage Dash → Team scaling card with policy editing, status metrics,
  a selected-session preview, reasons, and Manager evidence; Stage shows a
  compact team/slot badge.

## Consequences

### Positive

- Team size responds to observed plan pressure without hiding a model decision
  or rewriting the plan.
- Expansion and scale-down are explainable, deterministic, and auditable.
- Operators get a preview before enabling, and a durable event trail after.
- Existing capacity, write-scope, approval, Verifier, and merge behavior is
  reused rather than duplicated.
- Managers cannot be created implicitly, so department ownership stays
  explicit.

### Costs and limitations

- Slot targets and live counters are daemon-process memory. Decisions and
  policy are durable, but a restart recreates slot state from the policy and
  the latest durable observation; this is not a distributed fencing mechanism.
- The controller evaluates on a daemon tick, not at every task transition. A
  burst can be served by the configured target before the next tick.
- Plan demand is a deterministic scope-aware estimate, not a guarantee that the
  scheduler will run every selected task concurrently (the Verifier, capacity,
  integration queue, and budgets still gate execution).
- Budget headroom is based on the project or session ledger and does not
  include in-flight provider reservations; the controller never bypasses the
  budget gates, so an over-optimistic expansion still cannot exceed them.
- Global capacity is observed as free slots at evaluation time. It is not a
  reservation, and a competing session may take the slot first.
- No automatic Manager creation, no plan rewrite, no cross-host scheduler, no
  distributed slot leases, and no model-based sizing.

## Non-goals

- Creating or reassigning Managers.
- Editing a TeamPlan or changing department ownership.
- Starting sessions, tasks, or Workers directly.
- Replacing the deterministic task scheduler or orchestration controller.
- Weakening approvals, Seatbelt, path jails, write scopes, Verifier, merge
  queue, capacity, or budget gates.
- Cross-host scaling, distributed slot fencing, or a global fairness controller.
- Hidden model routing or provider selection.

## Observability

The decision trail is intentionally small and structured:

- `action` is one of `hold`, `expand_workers`, `expand_labour`,
  `expand_workers_and_labour`, `scale_down_labour`, `scale_down_workers`;
- `current_*` and `target_*` describe the slot ceiling before/after;
- `active_*` describes the live reservations observed at evaluation time;
- `ready_tasks`/`running_tasks` describe plan pressure;
- `reasons` is an ordered, bounded list of stable tokens (for example
  `cooldown_active:10/30s`, `global_worker_capacity_blocked`,
  `expand_workers:2->3`, `budget_headroom_below_threshold:0.100<0.250`,
  `scale_down_labour:6->4`, `applied_target_clamped:...`);
- `inputs_hash` is the canonical SHA-256 input identity;
- `manager_evidence` is present only when the scalable ceiling cannot absorb
  the queue and a plan revision would be required.

`scaling.status` reports the project policy, current targets, active counts,
ready/running/completed task counts, and the latest decision for each session.
`scaling.preview` reports the same decision shape with `applied=false` and
never writes. The CLI formats both surfaces without hiding the reason list.
The Swift card shows the policy, a selected-session preview, the latest action
per session, and a Stage badge with the current Worker/Labour ceiling and ready
count.

The tick logs a debug summary with project/session/evaluated/applied counts.
Per-session failures are bounded to the first 20 messages in the tick result
and are also logged; a failed evaluation never aborts the rest of the tick.

## Testing

The module is covered at four layers:

1. Pure controller tests use fixed `OffsetDateTime` observations and cover
   disabled no-op, expansion thresholds, combined expansion, cooldown and
   revision reset, max-step bounds, global Worker/Labour capacity, budget
   headroom boundaries (including unknown), plan-demand and write-scope
   bounds, dependency/retry readiness, deterministic priority/department/task
   ties, hashing stability, Manager evidence, Labour-before-Worker scale-down,
   active/demand/minimum floors, idle-window waiting, and hard-cap overflow.
2. Slot tests cover reserve/release/original-order behavior, managed versus
   legacy target restore, apply-clamping to active reservations, and
   coordinator reuse per session.
3. Store tests cover policy defaulting/round-tripping, decision event
   projection, uniqueness/idempotency, ordering, and replace/rebuild.
4. Engine/service tests cover policy RPC surface semantics, manifest
   persistence, replay/repair/import/rebuild, preview non-persistence, tick
   apply/idempotency, imported/no-plan skipping, project filtering, and the
   invariant that a tick never creates Managers or rewrites a plan.

Protocol unit tests cover additive policy decoding, normalization/validation,
action wire values, legacy decision payloads, and tick/result serialization.
Swift tests cover policy defaults/encoding, decision/status decoding, missing
collections, and the empty-decision hold default. CLI unit tests cover preview,
status, and policy human output.

## Rejected alternatives

- **Automatic Manager creation.** Rejected because it silently changes
  department ownership and invalidates the explicit plan revision contract.
- **Controller writing a new TeamPlan.** Rejected for the same reason; a plan
  edit must go through the existing validated revision path.
- **Model-based sizing.** Rejected because sizing must be deterministic,
  previewable, and auditable without another provider call.
- **Changing `AgentConfig` per decision.** Rejected because it would race live
  runs and lose the exact active-reservation count. The shared slot object is
  the narrower, testable boundary.
- **Per-tick targets without an event.** Rejected because scaling would then
  be invisible to replay, bundle import, and `db-rebuild`.

# ADR-0196: Runtime version pinning and controller replay

- **Status:** Accepted
- **Date:** 2026-11-12
- **Supersedes:** none
- **Related:** ADR-0070 (resumable TeamPlan), ADR-0072 (`db-rebuild`),
  ADR-0143/0144 (run journal), ADR-0188/0189/0190 (prompt layout, append-only
  conversation, tool bundles), ADR-0193 (TaskContract), ADR-0194 (effect
  receipts), ADR-0195 (external gates)

## Context

VerlinPark plans and durable runs can outlive a daemon release, a project
policy edit, or a tool-bundle change. Before C4, a resume compared provider,
model, budget, plan revision, and worktree head, but it did not record the
deterministic *identity* of the runtime that produced the plan. A resumed plan
could therefore run under a different contract policy, prompt layout, or tool
partition without a durable warning. There was also no way to ask whether a
recorded deterministic scheduler/orchestration decision still matched current
code.

The two failures are different:

1. **Version skew.** A plan accepted under one policy/contract/tool-bundle may
   be invalid under another. Resuming it silently is unsafe; refusing every
   upgrade is unusable.
2. **Controller drift.** Events say what the controller decided, but not
   whether today's pure controller would produce the same decision from the
   same recorded inputs. Replaying model output would be meaningless; replaying
   deterministic decisions is useful diagnosis.

C4 addresses both without widening any approval, sandbox, path-jail, write
scope, Verifier, Integrator, capacity, or budget gate.

## Decision

### 1. Canonical runtime snapshot

Every submitted/revised TeamPlan and every durable run records a
`RuntimeSnapshot`:

```json
{
  "schema_version": 1,
  "runtime_version": "0.1.0-dev",
  "runtime_build": "vp-core/0.1.0-dev",
  "policy_hash": "sha256(canonical policy bundle)",
  "tool_bundle_hash": "sha256(bundle membership identity)",
  "prompt_layout_version": 3,
  "plan_revision": 4,
  "contract_hash": "sha256(canonical plan contract material)",
  "created_at": "2026-11-12T00:00:00Z"
}
```

Fields are additive and serde-defaulted. `runtime_version` is the workspace
crate version. `runtime_build` uses `VERLINPARK_BUILD` when the packaging build
sets it, otherwise `CARGO_PKG_NAME/CARGO_PKG_VERSION`; it is a label, not a
signature. A missing snapshot is legacy state, not an incompatible plan.

`contract_hash` commits to the trusted Leader-authored plan (goal, departments,
tasks, write scopes, acceptance, budgets), the project `ContractPolicy`, and the
project `ShellPolicy.network` value. It contains no prompt, provider key, tool
argument, command text, file content, or MCP secret.

`tool_bundle_hash` is the tool-bundle **membership** identity:
`tool_bundle::bundle_contract_hash(selection)` hashes the bundle version,
kind/role/profile/task-kind/fallback, and selected built-in names. It does not
read MCP schemas, so pinning a plan does not perform provider or MCP I/O. The
exact frozen schema bytes remain in `context.prompt_prefix.tools_hash`; this
snapshot is a resume identity, not a schema cache.

Storage:

- `sessions.plan_runtime_snapshot` — JSON for the current stored plan.
- `session_runs.runtime_snapshot_json` — JSON for one run, carried in
  `session.run.started` / `session.run.resumed` events.
- `policy_snapshots(project_id, hash, json, created_at)` — immutable,
  content-addressed project policy bundles. The primary key is
  `(project_id, hash)`; identical policies deduplicate, and a policy edit
  creates a new row rather than rewriting an old one.

The snapshot JSON is embedded in the plan/run event payload. The policy bundle
is emitted once per `(project, hash)` as `policy.snapshot` with `{project_id,
hash, json, created_at}`. It is also exported in `SessionBundle` and the
project manifest, restored by `db-rebuild`, checked by `session.replay`, and
repaired from events. No secret is part of a policy bundle: it captures shell,
contract, effect, gate, cache, orchestration, scaling, scheduler, session-run,
critical-path, and attention policies only. Team caps, provider keys, headers,
and MCP credentials remain runtime configuration.

A policy edit that affects a stored plan produces a new current hash but does
**not** rewrite the stored snapshot. The difference is visible until an
explicit migration.

### 2. Compatibility states

`CompatibilityReport { status, target, session_id, run_id, plan_revision,
snapshot_age_secs, snapshot_created_at, stored, current, changed[], warnings[],
migration_required, checked_at }` is bounded: at most 32 changed fields and 32
warnings, values truncated to 160 chars.

| State | Meaning | Resume behavior |
|---|---|---|
| `compatible` | Stored and current runtime/policy/bundle/layout/revision/contract match | Proceed |
| `drift` | Runtime version/build, policy hash, tool-bundle membership, or prompt layout changed; plan revision and contract are still valid | Proceed and append `runtime.compatibility_drift` |
| `incompatible` | Plan is missing, plan revision changed, or contract hash changed at the same revision | Fail closed with `RUNTIME_INCOMPATIBLE`; require `plan.migrate` |

A missing snapshot is `drift` with `snapshot_missing` and proceeds, preserving
pre-C4 resume behavior. It is never treated as a match.

Comparison rules:

- `runtime_version`, `runtime_build`, `policy_hash`, `tool_bundle_hash`, and
  `prompt_layout_version` differences are drift.
- `plan_revision` differences are incompatible. A plan revision means the
  task DAG or task inputs may have changed; resuming messages written against
  the old revision would be unsafe.
- `contract_hash` differences at the same revision are incompatible (for
  example, the stored plan was mutated outside the event path or the contract
  policy changed the derived contract).
- A missing plan for a plan target, or a run whose plan revision no longer
  exists, is incompatible.

For `session.run.resume`, the run's own `plan_revision` is compared with the
current plan. For `resume_plan`, the session's stored plan snapshot is used.
Drift is recorded once per resume attempt. No model or tool routing changes:
the current provider/model must still match the run preflight, and ordinary
dispatch policy still applies.

### 3. Explicit plan migration

`plan.migrate` (`vp plan-migrate <session> --yes`) is an operator,
approval-classified action with `approval_risk = "host_state"`. Without
`approved = true` it returns `PLAN_MIGRATION_APPROVAL_REQUIRED` and mutates
nothing. It refuses active sessions (`queued`, `running`, `paused`,
`waiting_input`) and failed/imported archives.

On an approved request it:

1. checks the optional `expected_revision` (conflict fails);
2. calls the trusted plan validator (`validate_team_plan`) with the plan's own
   department count, adds a dependency-cycle check, and derives one contract
   per task under the current `ContractPolicy`;
3. computes the current snapshot and policy bundle;
4. updates only `sessions.plan_runtime_snapshot` and appends `plan.migrated`
   atomically;
5. reports unresolved effect receipts and open gates as warnings without
   reconciling, cancelling, or changing them.

On any failure the original plan and snapshot are untouched and no
`plan.migrated` event is appended. Migration never rewrites task ids,
dependencies, write scopes, budgets, contracts, effect receipts, or gates, and
there is no automatic migration on resume.

### 4. Controller replay

`orchestration.replay` / `vp orchestrate-replay <session> [--limit N]` is
read-only. It takes the newest `1..=2000` decision-bearing events (default 200)
from `orchestration.decision`, `scaling.decision`, `plan_task.started`,
`agent.plan_resumed`, and `agent.task_dispatched`.

Reconstruction:

- `team_plan.*` events reconstruct the plan in effect at each decision.
- `plan_task.*` events reconstruct status, attempts, starts/finishes, errors,
  and `next_attempt_at` with the same transition table as
  `Store::sync_plan_tasks`.
- `orchestration.decision` is replayed through the current pure
  `plan_actions` controller with reconstructed scheduled rows, coordination
  notes, event prefix, and budget-exhaustion state.
- `scaling.decision` carries `replay_inputs` (the exact `ScalingObservation`
  and policy); the current pure `evaluate` runs on those inputs. Older decisions
  without recorded inputs are `skipped`, never guessed.
- `plan_task.started` is checked against the deterministic ready set at that
  event time. Governor-enabled dispatches are skipped because historical
  governor inputs are not recorded.
- resume events are checked for remaining non-terminal work.

Each entry is `matched`, `mismatched`, or `skipped`, with bounded changed
fields. The report carries `controller_equivalent_only = true`, counts,
truncation, the window, the current policy hash, and warnings. Replay never
calls a provider, executes a tool, appends an event, mutates a projection,
retries, cancels, merges, or rewrites a plan. A mismatch is diagnostic only and
is not Verifier or merge evidence.

### 5. Surfaces

| Surface | Contract |
|---|---|
| RPC | `session.compatibility {session_id, run_id?}`, `plan.migrate {session_id, reason?, expected_revision?, approved, approved_by?}`, `orchestration.replay {session_id, limit?}` |
| CLI | `vp compatibility <session> [--run <id>]`, `vp plan-migrate <session> --yes [--reason ...]`, `vp orchestrate-replay <session> [--limit N]` |
| Swift | `CompatibilityReportView`, `PlanMigrateResultView`, `ControllerReplayReportView`; a Stage runtime-pin badge and Efficiency inspector section; no business logic in views |

All new protocol fields are serde-defaulted. Error responses use the Q3
`{code, message, retryable, hint, compatibility?}` shape; the `RuntimePinError`
is downcast by the daemon.

## Invariants preserved

- Rust owns state; SQLite/events remain authoritative.
- Snapshots and policy rows are event-backed projections; replay/repair and
  `db-rebuild` rebuild them from events/manifest, and bundles carry the
  referenced policy rows.
- No hidden model routing, no budget/capacity widening, no approval weakening.
- `plan.migrate` is explicit and operator-approved; no automatic migration.
- Replay is observation-only; no auto-retry, auto-merge, auto-cancel, or DAG
  rewrite.
- No secrets, prompts, tool arguments, command text, file content, or provider
  output are stored in snapshots, policy rows, events, CLI, or UI.

## Non-goals

- No provider or remote exactly-once claim.
- No distributed/cross-host snapshot or policy consensus; SQLite remains
  single-daemon.
- No signature or attestation of the runtime build. `runtime_build` is a
  best-effort label.
- No model-output replay, prompt replay, or "what would the model have done"
  simulation.
- No automatic plan rewrite, contract regeneration during resume, or silent
  snapshot backfill on a live resume.
- No historical governor-input capture or replay.
- No C5+ reliability metrics, failure attribution, fan-out, tool-search v2,
  MCP tasks/elicitation, Code Mode, A2A, or progress ledger.

## Consequences

- Plans and runs created before C4 report `drift`/`snapshot_missing` until an
  explicit `plan.migrate`; this is visible and does not block resume.
- A policy change that alters derived contracts makes a stored plan
  incompatible and requires migration. That is intentional: the contract the
  runtime enforces must match the plan it resumes.
- Tool-bundle membership changes are drift, not incompatibility. Schema bytes
  can still change without a membership change; the existing prompt telemetry
  and role policy remain the authority for actual schemas.
- Controller replay can produce many mismatches after a controller change.
  They are bounded and diagnostic; operators must not treat them as merge
  evidence.
- `policy_snapshots` grows with distinct policy bundles. Rows are deduplicated
  per project and contain canonical policy JSON only.

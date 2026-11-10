# ADR-0140: Policy-gated automatic stall recovery

- **Status:** Accepted
- **Date:** 2026-11-11
- **Owners:** Runtime, Agent Orchestration, CLI
- **Related:** ADR-0004, ADR-0080, ADR-0087, ADR-0123, ADR-0134, ADR-0139

## Context

ADR-0139 introduced `plan_task_progress` and `orchestration.stall` as
observability-only telemetry. That remains the safe default: a long legitimate
turn, a slow tool, or a provider-stalled stream must not cause the runtime to
cancel or retry work by itself. Operators can already pause, cancel, retry,
resume, and steer a live task.

Some long runs nevertheless stop making observable progress in a way that
operators cannot notice for hours. This ADR adds a conservative, project-level
opt-in for two bounded actions after repeated stall observations:

1. record an escalation without touching the task; or
2. request cooperative cancellation, wait for a clean quiescent point, and
   retry the same plan task once through the existing retry/resume path.

`observe` is the default and preserves ADR-0139 behavior exactly. This is
same-task/department recovery, not same-agent recovery: VerlinPark creates a
fresh Worker for the retried task. The ADR does not claim immutable agent
identity, preserved conversation, or worktree state beyond the normal task
worktree lifecycle.

## Decision

### 1. Policy

`OrchestrationPolicy` gains four fields. Existing project payloads decode with
`serde(default)` and keep the observation-only behavior.

| Field | Default | Bounds | Meaning |
|---|---|---|---|
| `stall_recovery` | `observe` | `observe`, `escalate`, `retry` | What a repeated stall may trigger |
| `stall_min_observations` | `2` | `1..=10` | Consecutive stall observations required |
| `stall_max_attempts` | `1` | `0..=5` | Automatic recovery retries per task/plan revision; `0` disables retries |
| `stall_grace_secs` | `300` | `0..=86400` | Time allowed for cancellation and session quiescence |

`stall_min_observations` applies to both `escalate` and `retry`. A single
observation never triggers an action. `observe` performs no recovery mutation
at all; the existing `orchestration.stall` event and progress refresh remain the
only effects. `set_orchestration_policy` validates all bounds. No migration
auto-enables recovery for existing projects.

### 2. Persisted recovery projection

`orchestration_recoveries` is SQLite state keyed by
`(session_id, plan_seq, task_id)`:

| Column | Meaning |
|---|---|
| `project_id`, `department` | task provenance at request time |
| `action` | `escalate` or `retry` |
| `status` | `requested`, `waiting_cancel`, `retried`, `escalated`, `failed`, or `unnecessary` |
| `attempts` | automatic retry requests issued for this task revision |
| `last_stall_count` | stall observation that produced the latest transition |
| `requested_at`, `updated_at` | request and transition timestamps |
| `last_error` | reason for escalation/failure or the latest transition |
| `next_action_at` | grace deadline while cancellation/quiescence is pending |

Every transition appends one event in the same transaction as the projection
update:

- `orchestration.recovery_requested`
- `orchestration.recovery_waiting_cancel`
- `orchestration.recovery_retried`
- `orchestration.recovery_escalated`
- `orchestration.recovery_failed`
- `orchestration.recovery_unnecessary`

Each event payload is the full `OrchestrationRecovery` row, so replay keeps the
latest row per key. `session.replay` compares the recovery projection,
`session.replay {repair:true}` restores it, `vp db-rebuild` reconstructs it
through the same repair path, and bundles carry `recoveries` with
`serde(default)`. Bundle import also replays any recovery events the archive
contains. Rows from an older plan revision remain in history but are filtered
out of live snapshot/RPC reads by joining the current `plan_tasks` projection.

### 3. Observe and escalate

`observe` is a no-op beyond the existing stall event. `escalate` creates or
updates one recovery row to `escalated` and records a critical
`coordination.note` with action `escalate`. It does not cancel, pause, retry,
change budgets, or change write scopes. At most one escalation is recorded per
task/stall window; a later `stall_count` window can escalate the same task
again. Terminal `failed` and `unnecessary` rows do not escalate again merely
because the stall sweep repeats.

### 4. Retry

A retry is considered only when all of the following hold:

- `stall_recovery` is `retry`;
- `stall_count >= stall_min_observations`;
- the plan task is still `running` or `claimed`;
- no recovery request is already in flight;
- this stall window has not already produced a request; and
- `attempts < stall_max_attempts`.

The runtime first persists `orchestration.recovery_requested` with status
`requested`, then calls the existing `request_plan_task_cancel_with_event` path.
It does not mark the task failed, change its write scope, or bypass a budget.
On success the row moves to `waiting_cancel`; the original `next_action_at`
deadline is retained. If the task already completed or was skipped, the row is
marked `unnecessary`. If the task left `running` before cancellation was
delivered, the row still waits for session quiescence.

A dedicated recovery reconciler, separate from the task scheduler loop, runs on
`VERLINPARK_STALL_RECOVERY_TICK_SECS` (default 30, `0` disables the tick). It
examines only `requested`/`waiting_cancel` rows and waits for:

- task status `cancelled`, `failed`, or `interrupted`; and
- session status `idle`, `failed`, `cancelled`, or `draft` (pre-run).

Only then does it reuse `retry_plan_task` (which preserves existing attempt and
backoff semantics) and call `resume_plan` once. A completed or skipped task is
never retried; it becomes `unnecessary`. A session whose token or USD budget is
exhausted is never auto-resumed: the row becomes `failed` and an escalation
note is recorded. `stall_max_attempts` exhaustion, a failed retry, or expiry of
`stall_grace_secs` also produce `orchestration.recovery_failed` plus an
escalation note instead of looping. The `unlimited` session mode still bypasses
economic gates, while capacity, approvals, sandbox, write-scope, Verifier,
merge, context, turn, and wall-clock gates remain unchanged.

A crash can land after the task retry event and before the recovery row is
finalized. On the next tick, a pending task in a quiescent session is treated
as the already-applied retry: the recovery row is finalized once and the plan
resume is attempted once. The reconciler never issues a second automatic retry
for the same attempt.

### 5. Surfaces and diagnostics

- `orchestration.snapshot` includes current-revision `recoveries` and a
  per-task recovery row.
- `orchestration.overview` reports total/active/failed recovery counts, the
  latest recovery detail, and aggregate totals.
- `orchestration.recoveries {session_id}` returns the read-only projection;
  `vp recoveries <session>` prints it.
- `orchestration.policy.get` / `set` and `vp orchestration-policy` expose the
  four policy fields. The Swift settings surface may follow without changing
  the backend contract.

### 6. Safety boundaries

Recovery never creates a new agent tool, routes models, widens a budget or
write scope, skips an approval, bypasses the Seatbelt/path jail, preempts a
Verifier, or forces a merge. Cancellation is cooperative at worker
turn/tool boundaries, and the retry enters the normal Worker → Verifier →
Integrator pipeline with a fresh Worker. A stalled provider stream that never
reaches a boundary cannot be interrupted by this feature; the task waits until
the grace deadline fails the recovery and escalates.

## Consequences

- Operators can opt into bounded same-task recovery for genuinely silent stalls
  without making observation destructive by default.
- Every recovery decision is durable, inspectable through CLI/RPC, replayable,
  repairable, and included in bundles.
- Retry is deliberately narrow: one plan task at a time, no hidden fan-out, no
  same-agent identity claim, and no budget or capacity bypass. A recovery that
  cannot reach a clean quiescent point fails and escalates rather than looping.
- Progress and recovery are different kinds of state: `plan_task_progress`
  remains operational telemetry, while recovery intents and outcomes are
  event-backed projections.

# ADR-0139: Operator steering and the task progress/stall ledger

- **Status:** Accepted
- **Date:** 2026-11-11
- **Owners:** Runtime, Agent Orchestration, CLI
- **Related:** ADR-0004, ADR-0080, ADR-0087, ADR-0123, ADR-0134

## Context

Multi-hour and multi-day runs already have cooperative session pause/resume,
plan-task pause/cancel/retry, runtime leases, a persistent task DAG, and
deterministic orchestration policy. Those controls answer "stop", "resume", and
"retry", but they do not let an operator add a new instruction to a live run,
and they do not expose whether an apparently running task has stopped making
observable progress. Long sessions need both: a bounded steering channel into
the active conversation, and an event-backed progress ledger that distinguishes
a legitimate long turn from a silent stall.

This round deliberately keeps both features observable and cooperative. It adds
no hidden prompt injection, no agent-to-agent steering tool, and no automatic
cancel/retry from the stall signal.

## Decision

### 1. Steering is operator-only, persisted, and addressed to one run

`steering_messages` is session-scoped SQLite state:

| Column | Meaning |
|---|---|
| `id` | UUID v7; stable event/projection identity |
| `session_id` | owning session |
| `agent_id` | one target agent/run, never a broadcast |
| `task_id` | optional plan-task scope |
| `body` | operator text, 1-4000 chars |
| `status` | `pending`, `delivered`, or `cancelled` |
| `created_at`, `delivered_at` | creation and cooperative delivery time |

`session.steer` creates a row and emits `steering.created`. Without `--agent`
it targets the session Leader only when that Leader is `running` or
`waiting_input`; otherwise it returns an explicit error. An explicit target must
belong to the session and be in one of those states. When `--task` is supplied,
the target must currently own that plan task. There is no implicit fan-out to
Workers and no agent-facing steering tool in this round.

The agent loop drains pending rows for its own `AgentSpec` immediately before
the next provider request, after compaction and the cancellation/budget gates.
Draining and the row transition are atomic with the audit events:

- `steering.delivered` plus an optional `message.created` user message for
  top-level runs;
- one visible user turn labeled `[Operator steering #<id>] <body>` in the active
  conversation (never a system-prompt mutation).

Rows are ordered by `created_at, id`. A row scoped to task `T` is withheld from a
run working another task (or no task). The daemon never injects a steering row
into a later run: when `run_loop` returns for any reason, it cancels everything
still pending for that agent and emits `steering.cancelled`. Daemon recovery
cancels pending rows after a crash for the same reason. `session.steering`
lists rows; `session.steering.cancel` cancels only a still-pending row.

The projection is rebuilt from the latest `steering.*` event per id.
`session.replay` compares it, `session.replay {repair:true}` restores it,
`vp db-rebuild` reconstructs it through the same repair path, and bundles carry
`steering` with `serde(default)` so older archives decode as an empty list.

### 2. Progress is operational telemetry, not a replayed projection

A companion table, `plan_task_progress`, carries
`last_progress_at`, `stall_count`, `last_turn`, `last_tool`, and `updated_at`
keyed by `(session_id, plan_seq, task_id)`. It is joined into `task.queue`,
`task_detail`, and orchestration snapshots rather than widening the
`plan_tasks` lifecycle row.

Task-scoped runs update it at two bounded boundaries only:

- the start of each model turn (after pause/cancel/budget gates);
- the start of each tool call (parallel batches record each starting tool).

It never writes per token, per stream chunk, or per tool output. A late tool
result cannot resurrect progress for a task that is no longer `running` or
`claimed`.

This table is intentionally **not** repaired by `session.replay`. It is live
runtime observation that cannot be reconstructed exactly from session events
without replaying every wall-clock boundary, and stale-or-missing observations
must never make replay claim that scheduling state drifted. Repair rewrites the
plan-task projection and leaves progress observations alone; rows for an older
plan revision are deleted on the next `sync_plan_tasks` and join-filtered in the
meantime. This is the explicit operational-table exception to projection replay.

### 3. Stall observations are threshold-windowed and non-destructive

The daemon sweep is controlled by `VERLINPARK_TASK_STALL_SECS` (default 1800,
`0` disables). It examines only plan tasks whose status is `running` or
`claimed`. The observation time is `COALESCE(last_progress_at, started_at,
enqueued_at)`; when it is older than the threshold, the sweep:

1. increments `stall_count`;
2. sets `last_progress_at` to the observation instant;
3. emits one `orchestration.stall` event with `age_secs`, `task_id`, `plan_seq`,
   `department`, `title`, `status`, `attempts`, `stall_count`, `last_turn`,
   `last_tool`, and `observed_at`.

Refreshing the timestamp makes the signal one event per threshold window, so a
long legitimate turn or tool is not spammed. A real turn/tool observation also
resets the window. Terminal (`completed`, `failed`, `cancelled`, `skipped`) and
non-running tasks never produce an event. The sweep performs no cancellation,
retry, pause, budget change, or write-scope change.

The signal is exposed through:

- `session.stalls` / `vp task-stalls <session>`: progress rows and recent stall
  events plus the configured threshold;
- `task_detail`: a task's progress fields and current `stalled` flag;
- `orchestration.snapshot` / `orchestration.overview`: per-task progress fields,
  `stalled` flags, and `stalled_tasks` metrics; the overview totals aggregate
  them.

### 4. Safety boundaries are unchanged

Steering is operator RPC/CLI only. It does not create a tool, does not bypass
approval, Seatbelt, path-jail, write-scope, Verifier, merge, capacity, or budget
gates, and does not write outside the agent worktree. The injected text is a
normal user turn in the active conversation and is persisted/audited explicitly.
The stall sweep is observability-only and cannot mutate task status or scheduling
policy.

## Consequences

- Operators can correct a long-running Leader or Worker without canceling and
  rebuilding the run; delivery happens at the next safe model boundary.
- Undelivered steering is auditable and terminal: it is cancelled, not carried
  into a future run.
- Stalls are visible for hours-long work without repeated log spam or automatic
  destructive action.
- Progress timestamps are approximate runtime telemetry, not replay-grade
  scheduling state. A daemon restart can lose the latest in-memory observation
  window; the next claim/turn re-establishes it.
- Steering targets an agent row, not a formal immutable run id. Terminal
  cleanup and the one-active-run-per-session invariant are what prevent leakage.
- Delivery is cooperative: a provider call already in flight cannot be
  interrupted to read new steering, and a run that ends before the next boundary
  cancels the message.

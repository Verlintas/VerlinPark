# ADR-0147: Recurring session continuations

- **Status:** Accepted
- **Date:** 2026-11-11
- **Owners:** Runtime, Scheduler, CLI
- **Related:** ADR-0121, ADR-0127, ADR-0139, ADR-0141

## Context

ADR-0141 added a single deferred operator prompt: a persisted row wakes once
when watched plan tasks settle or a deadline arrives, then delivers through the
normal `AgentService::start` path. Multi-day work also needs the same bounded
follow-up to repeat, for example "summarize progress every six hours" or "check
the dependency every morning", without a cron daemon, a model-authored
scheduler, or a long-lived run.

The plan DAG remains the ordering authority. A recurring continuation only
produces normal session runs; it does not add plan tasks, satisfy dependencies,
retry tasks, or bypass approvals, budgets, capacity, write scopes, the
Verifier, or the merge queue.

## Decision

### 1. Additive protocol fields

`SessionContinuation` gains, all with `#[serde(default)]` so old events,
bundles, and rows decode:

| Field | Meaning |
|---|---|
| `series_id` | Stable id shared by every row in the series. Empty on legacy rows; empty means the row's own id and a one-shot series. New first rows store the row id. |
| `parent_continuation_id` | Previous delivered run that scheduled this row; absent on the first run. |
| `recurrence_every_secs` | Interval between delivered runs. Absent means one-shot. |
| `recurrence_max_runs` | Maximum delivered runs in the series, including the first. Defaults to 1, so legacy rows behave as one-shot. |
| `run_count` | Delivered runs already completed before this row; delivery increments it. A pending child inherits the parent's post-delivery count. |

`SessionDeferParams` gains optional `every_secs` and `max_runs`. Existing
JSON-RPC calls, CLI scripts, and stored bundles keep working; absent fields
mean one-shot.

### 2. Validation

- `every_secs` is 60..=2,592,000 seconds (30 days).
- With `every_secs`, `max_runs` defaults to 10 and must be 2..=100.
- Without `every_secs`, `max_runs` may be omitted or exactly 1; any other value
  is rejected instead of silently becoming a series.
- The first wake still requires at least one watched task or an
  `after_secs`/`deadline_at` deadline. `every_secs` does not create a wake
  condition by itself.
- `series_id` is generated on the first row (the row id) and inherited by every
  child. The number of children is bounded by `max_runs` (hard cap 100).
- The operator-facing recurrence label is bounded: `series` is truncated to 80
  characters in the prompt note, and the whole note keeps the existing
  2,000-character cap. The body keeps its 1-4,000-character bound.

### 3. First wake vs child wake

The first row is created exactly like a one-shot continuation: it watches plan
tasks and/or a deadline. When it wakes, it delivers normally.

Only a successful delivery can create a child. The child is a new row with:

- the same session, project, body, series id, recurrence fields, and
  `max_attempts`;
- `parent_continuation_id` set to the delivered row;
- `watch_task_ids = []` and `watch_mode = "all"`;
- `deadline_at = delivery_time + every_secs`;
- status `pending`, `run_count = delivered.run_count`.

Because the child has no watched tasks, already-terminal tasks from the first
run cannot wake it. Its first possible wake is its own deadline, at least 60
seconds after delivery. The interval is measured from the successful delivery
timestamp, not from when the previous run finishes; a long run can make the next
deadline already due when the session becomes idle, and it then wakes on the
next scheduler tick.

### 4. Atomic delivery and run counts

`mark_session_continuation_delivered_with_event` commits one SQLite
transaction that:

1. changes the claimed `delivering` row to `delivered`, sets `delivered_at`,
   and increments `run_count`;
2. inserts `session.continuation.delivered` with the full delivered row;
3. when `run_count < recurrence_max_runs` and `recurrence_every_secs` is set,
   inserts the child row and `session.continuation.scheduled` with the full
   child row.

The event stream is written before projection visibility because the event
inserts and row mutations share the transaction. A delivered row with
`run_count == recurrence_max_runs` schedules no child. Repeating the delivery
transition on an already delivered row returns no result, so it cannot create a
second child or a second event.

### 5. Crash and failure behavior

The existing delivery claim is committed separately as `delivering`. If the
delivery-success transaction fails after `start` succeeded, the whole
transaction rolls back: the claimed row remains `delivering`, no delivered
event and no child exist, and daemon recovery fails the row closed with
`daemon restarted while delivering; not retried automatically`. This is an
honest crash window: the run may have happened, but the continuation is not
redelivered and no recurring child is fabricated. The operator inspects the
session and explicitly re-defers if needed.

A start failure, retry with backoff, or terminal start failure never creates a
child because only the delivered transition does. Exhausting the attempt cap
fails that row and therefore ends the series.

Landed and imported sessions cancel any open series row with the existing
reasons; a missing session fails the row. Neither path creates a child. Normal
foreign-key cascades delete continuation rows with their session, so the
missing-session branch is defensive.

Cancellation is per row. Cancelling a pending child cancels only that child; an
already delivered parent and any separately pending row are untouched. There is
no "cancel series" action in this round; cancel each open row or let the series
reach `max_runs`.

### 6. Replay, repair, bundles, and rebuild

`session.continuation.scheduled` carries the complete child row, and replay
folds it with the existing continuation lifecycle events. The new fields are
part of every emitted continuation payload. `session.replay`,
`session.replay {repair}`, session bundles, bundle import, and
`vp db-rebuild` therefore reconstruct parent and child rows without special
ordering or extra bookkeeping. Old events decode with empty series, no parent,
no interval, `max_runs = 1`, and `run_count = 0`.

### 7. Operator surface

- RPC: `session.defer` accepts `every_secs` and `max_runs`; the existing
  `session.continuations` and `session.continuation.cancel` responses include
  the new fields.
- CLI:
  `vp defer <session> "prompt" ... --every-secs N --max-runs K`,
  and `vp continuations <session>` renders
  `run X/Y`, `series <id>`, and `every Ns`. `--json` keeps the full
  machine-readable result.

There is still no agent-facing tool. Continuations are operator state, not
model-authored scheduling.

## Consequences

- An operator can express a bounded recurring follow-up without cron, a
  daemon-wide recurrence table, or an open run.
- Wake and interval decisions are deterministic Rust. No model call decides
  whether a run is due or how many runs remain.
- Every run still goes through `AgentService::start` only when the session is
  idle, failed, or cancelled. There is no start-while-active bypass.
- The series is bounded by `max_runs <= 100`; children cannot multiply without
  bound, and a failed or cancelled row ends its branch.
- The interval is not a wall-clock schedule: delays from provider latency,
  capacity admission, approvals, or a long previous run push the next run
  later. There is no catch-up, jitter, calendar rule, or missed-interval replay.
- The `delivering` crash window remains fail-closed. A recurrence series can
  stop after a delivery whose success transaction did not commit; the operator
  must re-defer.
- This round does not add a cross-process lease/fencing model, cron syntax,
  automatic child creation from agent output, series-wide cancellation,
  backoff-until-success scheduling, or LLM-decided recurrence. It remains a
  single-daemon, operator-only primitive.

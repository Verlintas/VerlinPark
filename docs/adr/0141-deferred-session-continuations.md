# ADR-0141: Deferred session continuations

- **Status:** Accepted
- **Date:** 2026-11-11
- **Owners:** Runtime, Scheduler, CLI
- **Related:** ADR-0004, ADR-0080, ADR-0087, ADR-0121, ADR-0127, ADR-0139

## Context

Operators can already pause/resume a session, retry individual plan tasks, and
inject bounded steering into a live run. Those controls assume the run is
already active. Multi-hour and multi-day work also needs the opposite shape: an
operator wants to add a prompt *later*, after a set of plan tasks settles or a
deadline arrives, without holding a daemon run open and without polling
manually.

The plan DAG remains the scheduler's ordering authority. A deferred
continuation is a session-level operator primitive: it does not add a plan task,
does not satisfy dependencies, and does not bypass any existing gate. It is not
an Agent Swarm and it does not create agent-to-agent messaging.

## Decision

### 1. Persisted, event-backed rows

`session_continuations` is session-scoped SQLite state:

| Column | Meaning |
|---|---|
| `id` | UUID v7; stable event/projection identity |
| `session_id`, `project_id` | owning session/project |
| `body` | operator prompt, 1-4000 chars |
| `watch_task_ids` | JSON list of plan task ids from the stored TeamPlan |
| `watch_mode` | `all` or `any` |
| `deadline_at` | optional RFC3339 fallback wake |
| `status` | `pending`, `ready`, `delivering`, `delivered`, `cancelled`, `failed` |
| `attempts`, `max_attempts` | bounded start-failure accounting |
| `wake_reason` | `watch` or `deadline` once ready |
| `outcomes` | frozen task id/title/status snapshot captured at wake |
| `last_error`, `next_attempt_at` | retry backoff and terminal reason |
| `created_at`, `ready_at`, `delivered_at`, `updated_at` | lifecycle timestamps |

The events are `session.continuation.created`, `.ready`, `.delivering`,
`.retrying`, `.delivered`, `.cancelled`, and `.failed`. Every transition is
committed with its event before projection visibility; the projection is
rebuilt from the latest event per id by replay/repair, project bundles, and
`vp db-rebuild`.

### 2. Creation validation

An operator must supply at least one watch task or a deadline. Watch ids must be
unique, safe task ids present in the currently stored TeamPlan, and must not
already be in a terminal state; the error tells the operator to read the
existing outcome instead. A relative deadline (`after_secs`) or an absolute
RFC3339 deadline must be in the future and at most 30 days away. The two forms
are mutually exclusive. Imported and landed sessions reject new deferrals.

### 3. Wake semantics

- `all`: every watched task must be terminal (`completed`, `failed`,
  `cancelled`, `skipped`, or `interrupted`).
- `any`: the first terminal watched task wakes the continuation.
- A terminal task does not have to be successful.
- A deadline wakes the continuation even if watched tasks are still running.
- If watch and deadline are both true in one evaluation pass, watch wins because
  it has a concrete outcome summary.
- A task that disappeared with a plan revision is reported as `missing` and
  treated as settled so a stale watch id cannot hang forever.
- The ready event freezes a compact `(task id, title, status)` outcome list so
  later plan edits cannot rewrite the context that justified the wake.

The daemon scheduler tick evaluates pending rows, marks the due row `ready`, and
then attempts delivery in the same pass. Delivery requires the session to be
`idle`, `failed`, or `cancelled`; `running`, `queued`, `paused`, and
`waiting_input` keep the row ready and retry on a later tick. At most one start
attempt is made per session per tick.

Before calling `start`, the pass atomically claims the row as `delivering` and
emits `session.continuation.delivering`. Only the pass whose claim commits may
start the run; a racing pass sees no claimable row and skips. A crash after the
claim but before the delivery result is resolved cannot be rolled forward
safely: daemon recovery marks every surviving `delivering` row `failed` with
`daemon restarted while delivering; not retried automatically`. This is
deliberately fail-closed; the operator inspects the session and re-defers if
needed rather than risking a duplicate run.

### 4. Delivery through the normal start path

Ready continuations call `AgentService::start(session_id, prompt)`, the same
entry point used by an operator prompt. Approvals, Seatbelt, path jail, write
scopes, Verifier, merge queue, capacity admission, and session/project budgets
are therefore unchanged. The prompt is an ordinary user message, not a system
prompt mutation:

```text
[Deferred continuation #<id>]
Deferred reason: watch|deadline
Watched tasks:
- <task-id> "<title>": <status>
Operator prompt:
<body>
```

The context note is capped at 2,000 characters and titles at 120 characters.
`delivered` is recorded only after `start` returns success, and only from the
`delivering` claim owned by that pass. A start error is persisted with an
attempt increment and a deterministic backoff; after the bounded cap (default
3, maximum 3) the row becomes `failed` and is not retried. A delivered row is
never started again.

### 5. Terminal session states

A ready continuation for a landed session is cancelled with a clear reason. An
imported (read-only archive) session is cancelled the same way. A missing
session fails the row, though normal foreign-key cascades remove rows with
deleted sessions. Cancellation is only allowed while pending or ready.

### 6. Operator surface

- RPC: `session.defer`, `session.continuations`, `session.continuation.cancel`.
- CLI: `vp defer <session> "prompt" [--watch <task-id> ...]
  [--watch-mode all|any] [--after-secs N | --at RFC3339]`,
  `vp continuations <session>`, `vp continuation-cancel <id>`.

There is no agent-facing tool in this round. Continuations are operator state,
not model-authored scheduling.

## Consequences

- An operator can schedule a follow-up prompt without keeping a run open or
  polling task state by hand.
- Wake decisions are deterministic Rust; no model call decides whether a
  continuation is due.
- The stored plan remains the only task dependency graph. Continuations wake a
  session; they do not reorder, retry, or fabricate plan tasks.
- Delivery inherits every existing safety and economic gate because it uses the
  normal run start path.
- A provider that is unconfigured or unhealthy consumes the bounded start-attempt
  budget and eventually fails the continuation; the operator can inspect
  `last_error` and re-defer after fixing configuration.
- Delivery is fail-closed across a daemon crash. A row caught in `delivering`
  becomes failed on recovery instead of being redelivered; the operator must
  inspect the session and explicitly re-defer.
- Continuation wake/inspection is cooperative: an in-flight run is never
  interrupted, and a ready row waits for the session to become quiescent.
- This round does not add cross-process continuation ownership, automatic
  creation from agent output, recurring continuations, or a distributed
  scheduler. The `delivering` claim is a single-daemon SQLite transition; future
  multi-daemon work would need a fencing/ownership design.

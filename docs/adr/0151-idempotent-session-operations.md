# ADR-0151: Idempotent session operations

- **Status:** Accepted
- **Date:** 2026-11-11
- **Owners:** Runtime, Store, CLI
- **Related:** ADR-0041, ADR-0139, ADR-0141

## Context

A client can lose the JSON-RPC reply after the daemon accepted a request, or the
daemon can crash between accepting a request and persisting its outcome. Without
an operation receipt, retrying `session.prompt` can start a second Leader run,
retrying `session.defer` can create a duplicate continuation, and retrying
`session.steer` can create a duplicate steering row. The same risk applies to
operator scripts and to any future client that retries automatically.

VerlinPark already has deterministic idempotency in several adjacent paths:
session leases serialize runs, steering and continuation rows are persisted, and
task/merge transitions are guarded. Those guards prevent concurrent duplicates
inside one process, but they do not remember that a specific *request* was
already accepted. A client that retries a successful prompt after a lost reply
needs that request-level memory.

## Decision

Add a session-scoped operation receipt table and optional `operation_id` fields
to the three operator request methods:

- `session.prompt` (`PromptParams.operation_id`)
- `session.defer` (`SessionDeferParams.operation_id`)
- `session.steer` (`SessionSteerParams.operation_id`)

The fields are additive and use `#[serde(default)]`, so old clients that omit
them and old payloads decode unchanged. The CLI exposes them as
`vp prompt ... --operation-id ID`, `vp defer ... --operation-id ID`, and
`vp steer ... --operation-id ID`.

`session.create` is intentionally **not** covered. Its natural key includes the
new session id, which does not exist before creation. A project-scoped receipt
would need a different conflict model (same operation id could legitimately
create two sessions in different projects) and a project-level table. That is
out of scope for this ADR.

### Receipt table

New SQLite table, created with the normal `CREATE TABLE IF NOT EXISTS` schema
bootstrap:

| Column | Meaning |
|---|---|
| `session_id` | Owning session; part of the primary key |
| `operation_id` | Caller key, trimmed, 1–200 characters, no control characters; part of the primary key |
| `method` | `session.prompt`, `session.defer`, or `session.steer` |
| `fingerprint` | SHA-256 of method plus canonical payload |
| `status` | `pending`, `completed`, `failed`, or `unknown` |
| `result_json` | Serialized result for `completed`; null otherwise |
| `error` | Failure/unknown detail |
| `created_at`, `updated_at` | RFC 3339 timestamps |

The primary key is `(session_id, operation_id)`. The same key in another session
is a separate receipt. The table is operational state, comparable to runtime
leases and plan-task progress: it prevents duplicate side effects and is
recovered at startup, but it is not a session projection and is not rebuilt by
`session.replay` or `vp db-rebuild`. Every actual session mutation still uses
the existing event-backed prompt, continuation, and steering paths.

### Fingerprint

The fingerprint is:

```text
SHA-256(method || "\n" || canonical_json(payload without operation_id))
```

Object keys are sorted recursively before serialization, so wire-map order and
struct field order cannot change the fingerprint. The payload is the method
arguments as received, including the session id. For `defer`, relative deadline
inputs (`after_secs`) are fingerprinted as supplied, not as resolved wall-clock
time; a retry with the same operation id and the same relative input replays the
original continuation rather than creating a new deadline.

Claim/compare/insert runs in one immediate SQLite transaction. Only one caller
can move a missing or failed row to `pending`.

### Status and claim semantics

| Existing receipt | Same method + fingerprint | Different method or fingerprint |
|---|---|---|
| none | insert `pending`, execute | insert `pending`, execute |
| `completed` | replay `result_json`; do not execute | conflict error |
| `pending` | in-progress error; do not execute | conflict error |
| `failed` | reset to `pending`, execute | conflict error |
| `unknown` | fail closed with an inspection error | conflict error |

A successful prompt stores `{"accepted":true}`; the RPC still returns
`PromptResult { accepted: true }` on replay. Defer and steer store the serialized
`SessionDeferResult` / `SessionSteerResult` and deserialize it on replay, so the
caller receives the original continuation or steering row id. Replay returns
before session/provider validation, run admission, row creation, or event
append.

Errors are deliberately explicit:

- pending: `operation ID is already in progress ... not starting a duplicate`
- conflict: `operation id ... already used ... with a different method or payload`
- unknown: `operation ... unknown outcome after a daemon restart; inspect ...`

A failed row is safe to retry only because the covered methods claim before they
perform their side effect and fail the receipt on every error path. If a future
method performs partial side effects before returning an error, it must not use
the blanket failed-retry rule without a compensating check.

## Crash window

The daemon commits `pending` before starting the run or creating the
continuation/steering row, and commits `completed` only after the side effect
returns. A crash can therefore leave `pending` in these windows:

1. after `pending` commit, before the side effect starts;
2. after the side effect lands, before `completed` commit.

At startup, `recover_interrupted_sessions` calls
`mark_pending_session_operations_unknown()` before queued sessions are
re-enqueued. `pending` becomes `unknown`; a client retry with that id is refused.
The daemon does **not** silently replay an unknown operation, because the side
effect may already exist. An operator must inspect the session transcript,
continuations, or steering list before deciding whether to issue a new request
with a new operation id.

The existing startup paths still run: sessions left `running`/`paused`/
`waiting_input` are marked failed, and persisted `queued` sessions are
re-enqueued from their stored prompt. A prompt receipt can therefore be
`unknown` while the daemon resumes the queued run that the original request
created. That is not a duplicate: the retry is refused and the original queued
work continues.

## What this is not

- **Not exactly-once provider execution.** Provider requests, streams, tool
  calls, external ACP work, and filesystem side effects are not transactional.
  If the daemon crashes after a provider call starts, the receipt can be
  `unknown` and the provider may have produced output. The receipt prevents a
  duplicate *accepted operation*, not a duplicate *model call* in every crash
  interleaving.
- **Not distributed idempotency.** Receipts live in the local SQLite database.
  The single-daemon data-directory lock remains the deployment model; there is
  no cross-daemon fencing token or shared receipt service.
- **Not a new agent tool.** Agents cannot claim arbitrary operation ids. Only the
  three operator/RPC entry points use the helper.
- **Not automatic recovery of unknown outcomes.** Unknown is fail-closed by
  design. There is no reconciliation RPC or auto-resolver for operator prompts,
  continuations, or steering.
- **Not covered for `session.create`.** See above.
- **Not covered for intake/task/scheduler/merge retries.** Those paths keep
  their existing deterministic transition guards; this ADR only adds request
  receipts to prompt, defer, and steer.
- **Not GC'd.** Receipts accumulate in SQLite. A future retention policy can
  prune old completed rows by age, but no pruning is added here.

No approval, sandbox, write-scope, Verifier, merge, capacity, or budget gate is
changed. A replay bypasses only the duplicate side effect; a new operation still
passes every existing gate.

## Consequences

- Clients can safely retry a prompt, defer, or steer with the same operation id
  and payload after a lost reply. The second call returns the first result
  (prompt accepted, same continuation, or same steering row) or a clear
  conflict/in-progress/unknown error.
- The engine helper is `vp-core::operation`: canonical fingerprint, receipt
  claim, replay decode, and completed/failed transitions. `AgentService::start`
  delegates to `start_with_operation(..., None)`, preserving existing callers.
- Tests cover protocol defaults, store replay/conflict/in-progress/failed/
  unknown transitions, prompt replay without a second provider call, recovery
  marking pending as unknown, failed prompt retry with the same id, duplicate
  continuation/steering prevention, and CLI flag parsing.

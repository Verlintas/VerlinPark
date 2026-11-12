# ADR-0195: External gates and durable waits

- **Status:** Accepted
- **Date:** 2026-11-12
- **Depends on:** ADR-0193 (task contracts and argument-level intent authorization), ADR-0194 (effect receipts and postcondition probes)
- **Plan:** `docs/CONTRACTED_EXECUTION_PLAN.md` C3

## Context

VerlinPark already had two ways for a run to wait: `ask_user` blocks one
agent on a question, and a deferred continuation waits for plan-task status or
a deadline. Neither models a real external wait. A long plan may need to pause
for CI, PR review, a deploy, or a human decision after the run that started the
work has ended. Polling from a shell or holding a Worker slot open for hours is
both wasteful and outside the runtime's authority model.

The C3 design must answer four questions without weakening any existing gate:

1. Who may declare that an external wait exists?
2. How is a resolver authenticated, and why can it not be replayed?
3. How does a session stop without holding a Worker/Labour/Manager slot?
4. What happens when the daemon restarts, the gate expires, or a duplicate
   resolution arrives?

## Decision

### 1. Gate model

`external_gates` stores one row per gate:

```
id, session_id, task_id, kind, label, token_hash, state, outcome,
payload_json, resolution_note, created_at, expires_at, resolved_at, resolved_by
```

`kind` is one of `ci`, `pr_review`, `deploy`, `human`, or `custom`. The daemon
has no provider-specific logic and never interprets the external result. A gate
is an opaque, authenticated wait handle, not an approval, verifier verdict,
merge signal, or budget decision.

`state` is `open`, `resolved`, `expired`, or `cancelled`. `outcome` is
`resolved`, `rejected`, or `failed` and is present only for a resolved gate.
Expiry and cancellation are terminal states with no outcome.

The public `ExternalGate` projection deliberately omits both the token and the
token hash. `GateRecord` is the server-side event/projection shape and carries
the SHA-256 verifier so replay, repair, bundle import, and `db-rebuild` can
reconstruct the gate without ever storing the raw token.

`payload` is bounded secret-free caller context such as a PR number or CI
workflow label. It is never used for authorization and must not contain a
credential or the gate token.

### 2. One-shot scoped token

`gate.open` generates 32 bytes from the operating system CSPRNG, encodes them
URL-safe without padding, and prefixes them with `vpg_`. The token is returned
exactly once, in the `gate.open` response. SQLite stores only
`SHA-256(token)`.

- The token is never written to an event, log, bundle, prompt, tool output, or
  Swift model.
- `GateToken` redacts itself in `Debug`.
- `gate.resolve` verifies the presented token against the stored verifier with
  a constant-time comparison before considering the gate state.
- The daemon Bearer token is not a substitute for the gate token and cannot
  bypass this check.
- An operator who loses the token cancels the gate and opens a new one. There
  is no token recovery endpoint.

Resolution is one-shot:

- the first valid resolution moves `open -> resolved` and writes the outcome,
  note, resolver label, and timestamp;
- replaying the same token with the same outcome is idempotent and returns
  `changed=false`;
- replaying the same token with a different outcome is `GATE_CONFLICT`;
- a wrong token is `GATE_TOKEN_INVALID`;
- an expired gate is `GATE_EXPIRED`; a cancelled gate is `GATE_CANCELLED`.

Expiry and cancellation never carry a token. `gate.cancel` is the explicit
operator escape hatch; it is terminal and wakes the wait with the `cancelled`
state so the agent can report the cancellation instead of hanging.

### 3. Durable wait without a slot

`session.defer` accepts an additive `watch_gate_id`. The continuation row
stores that gate id. At most one continuation may be `pending`, `ready`, or
`delivering` for a gate; a partial unique index enforces it and a typed
`GATE_ALREADY_WATCHED` error explains the conflict.

Operator path:

```
vp gate open <session> --kind ci --label "unit tests"
vp defer <session> "continue after CI" --gate <gate-id>
```

Agent path:

- Leader, Manager, and Worker runs can call `wait_for_gate` with an existing
  gate id and an optional bounded note.
- The tool validates that the gate belongs to the session, is open, and has no
  other active watch. It does not create a gate and never receives a token.
- The tool result is a structured `waiting=true` result. The run loop then
  returns `LoopOutcome::Deferred` before another provider call or sibling tool
  call.
- `run_loop` records the journal as `deferred`; the top-level run marks the
  session idle through the normal status path.
- A Manager/Worker child that defers marks its agent idle and releases its
  slot. `delegate_many` skips verification/merge for the deferred child and
  propagates the wait to the Manager/Leader. A plan-task Worker marks its task
  `blocked` with an explicit reason and aborts the plan batch into the same
  deferred handoff.
- No Worker, Labour, Manager, capacity permit, scope lease, or runtime lease is
  held while waiting.

On resolution, expiry, or cancellation, the store transitions the gate and the
watching continuation to `ready` in one transaction, emits
`session.continuation.ready` and `gate.continuation_ready`, and the normal
continuation scheduler starts one run through the normal prompt path. The
prompt contains a bounded `[gate]` summary with gate id, kind, state, outcome,
and note. It contains no token or verifier. The scheduler still requires the
session to be idle, so an active run is never interrupted.

A daemon restart preserves the gate and the continuation because both are
SQLite/event-backed. The continuation scheduler's existing claim/fencing and
operation-receipt path is unchanged. Expiry is swept by the daemon
(`VERLINPARK_GATE_SWEEP_SECS`, default 30, first tick immediate); a gate whose
deadline passed while the daemon was down is expired on restart and its wait is
woken.

### 4. Authority and boundaries

`gate.resolve` only moves a gate and wakes a continuation. It cannot:

- approve or resolve an approval request;
- bypass the approval broker, Seatbelt, path jail, write scope, or HOME jail;
- submit a Verifier verdict or affect verification;
- enter or bypass the merge queue or deterministic Integrator;
- change a budget, capacity limit, plan, task status, or model routing.

A gate is local-only by default. The daemon continues to bind only
`127.0.0.1`. The optional loopback resolve route (`POST /gate/resolve`) exists
only when the gate's project sets `loopback_resolve=true`; even then it requires
the same scoped token. It is a same-host CI convenience, not a public webhook.
No project policy can make the daemon bind a public interface.

Gate opening and resolution are rejected for imported archives and landed
sessions. Gate events are replayed by `session.replay`, `repair`, `rebuild`,
bundle import, and `vp db-rebuild`; malformed gate payloads are reported as
drift rather than silently dropping the verifier.

### 5. Surfaces

RPC:

- `gate.open` (`GateOpenParams` -> `GateOpenResult`; the token is present only
  in this result);
- `gate.resolve` (`GateResolveParams` -> `GateResolveResult`);
- `gate.cancel` (`GateCancelParams` -> `GateCancelResult`);
- `gate.list` (`GateListParams` -> `GateListResult` with counts);
- `gate.status` (`GateStatusParams` -> `GateStatusResult`);
- `gate.sweep` (`GateSweepParams` -> `GateSweepResult`; manual bounded expiry);
- `session.defer` with `watch_gate_id`;
- `project.gate_policy.get` / `project.gate_policy.set`.

CLI:

- `vp gate open`, `vp gate resolve`, `vp gate cancel`, `vp gate list`,
  `vp gate status`, `vp gate sweep`, `vp gate policy`, `vp gate policy-set`;
- `vp defer ... --gate <id>`.

Swift:

- `ExternalGateView`, `SessionGateCounts`, and `SessionGatesReport` decode
  additively and never contain a token or verifier;
- `CoreClient.sessionGates` and `AppModel.sessionGates` load `gate.list`;
- a read-only `GateBadge` in Stage and `GateListSection` in Efficiency show
  open/resolved/expired/cancelled state. The GUI does not resolve gates; the
  one-time token remains a CLI/RPC contract.

All new protocol fields are `serde(default)` so older events, bundles, and
clients keep decoding.

## Consequences

Positive:

- A session can wait for CI, review, deploy, or a human without polling and
  without pinning a Worker/Labour/Manager slot or capacity permit.
- A leaked event/bundle/database does not expose a usable gate token.
- Duplicate and wrong-token resolutions fail closed; expiry and cancellation
  are terminal.
- The wait survives daemon restarts and follows the existing fenced
  continuation delivery path.
- The existing approval, sandbox, Verifier, merge, capacity, and budget gates
  are untouched.

Costs and limits:

- The raw token is unrecoverable. Loss requires cancel-and-reopen.
- A gate can be resolved by anyone who has both the gate id and token; the
  token is therefore an operator secret.
- The optional loopback endpoint is same-host only and disabled by default.
  There is no public webhook listener and no provider callback registration.
- Expiry is swept on a daemon interval, so an expired gate may be observed up
  to one sweep interval late. The continuation then wakes through the normal
  scheduler, which may itself wait for the session to become idle.
- A Worker that defers leaves its task worktree uncommitted; the resumed
  Leader must re-delegate or continue the task. A plan task that defers is
  marked `blocked`, not completed.
- Gate resolution is local evidence. It does not prove the external system's
  state beyond the authenticated resolver's claim, and it is not provider
  exactly-once.
- `wait_for_gate` is not available to Verifier, Labour, or Scout, and the
  read-only Manager roundtable cannot defer.

## Non-goals

- No public webhook/HTTP listener and no provider-specific CI/review/deploy
  integration.
- No token recovery, token rotation, or multi-use token.
- No gate-to-approval, gate-to-verdict, gate-to-merge, or gate-to-budget
  shortcut.
- No distributed gate consensus across hosts; this is same-host SQLite.
- No automatic plan rewrite, model routing, or Worker reassignment on gate
  settlement.
- No GUI gate resolution and no token or verifier in Swift models.

## Verification

- Store tests: one-shot/idempotent resolution, wrong token, expiry, cancel,
  replay/rebuild, policy persistence, single active watch.
- Engine tests: token never stored/emitted, restart persistence, operator
  defer, loopback policy, approval isolation, replay/repair/bundle export.
- Agent tests: `wait_for_gate` ends the run with no second provider call,
  invalid gate returns typed `GATE_NOT_FOUND`, Worker slot is released, and
  resolution resumes through the continuation path.
- Protocol/CLI tests: additive decode, command parsing, token printed once,
  verifier never rendered.
- Swift tests: gate list/view decode without token/verifier and safe defaults.
- Existing approval/Seatbelt/Verifier/Integrator suites remain green.

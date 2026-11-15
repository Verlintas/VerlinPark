# 0221 — Loopback-only A2A facade

- Status: Accepted
- Date: 2026-11-16
- Scope: C11 bounded Agent2Agent interoperability

## Context

A2A describes agent cards, task lifecycle, streaming updates, and
`input-required`/`auth-required` states. Exposing a public listener or granting
inbound tasks local tool authority would bypass VerlinPark's contract,
approval, sandbox, and Verifier gates. C11 is therefore deliberately a
**loopback-only facade**, disabled by default.

## Decision

### Wire/runtime library

A new `vp-a2a` crate owns the bounded wire model and loopback transport:

- Agent Card (`agent-card.json`), JSON-RPC 2.0 methods `message/send`,
  `message/stream`, `tasks/get`, `tasks/cancel`, `tasks/resubscribe`.
- Task states `submitted|working|input_required|auth_required|completed|
  failed|canceled|rejected`; message parts text/data/file; bounded artifacts.
- Bounds: 1 MiB body, 128 history messages, 32 parts, 64 KiB text/data,
  256 KiB artifact, 32 skills, 1,024 tasks, 24 h TTL, bounded SSE frames.
- In-memory task projection with lazy TTL sweep; no durable replay, push
  notifications, or cross-host transport in this version.
- One bearer token, compared in constant time over SHA-256 digests, redacted
  from `Debug`, never serialized, and available exactly once through
  `A2aStartHandle::take_token`.
- Non-loopback bind addresses/URLs are rejected before binding; the served
  card URL is always the actual loopback address.

### Daemon lifecycle

- `A2aRuntime` in `vp-core/src/a2a.rs` holds the wire policy, the running
  handle, a bounded in-memory task summary list, and the last error.
- `a2a.status`, `a2a.tasks`, `a2a.start`, `a2a.stop` RPCs and
  `vp a2a-status/a2a-tasks/a2a-start/a2a-stop` are explicit. Starting requires
  `approved=true` and an enabled policy; the token is returned once by the
  start response/CLI, never by status. `allow_inbound=true` is reserved for a
  future capability contract and currently fails start with
  `A2A_INBOUND_DENIED` rather than silently accepting work it cannot execute.
- The runtime policy's `max_tasks` and `task_ttl_secs` are passed into the
  wire `A2aLimits`, so the server store and bounded status list enforce the
  same cap. A normal stop does not populate `last_error`.
- The facade is **card-only** in this version: inbound tasks are recorded as
  `rejected` with a bounded reason stating that no capability contract is
  configured. No tool, file, shell, network, provider, or model authority is
  granted. A later capability-attenuation round may replace the handler, but
  only through a project contract and external gate.
- The task list is bounded operational memory, lost on daemon restart or stop;
  it is not a session projection and cannot be used as Verifier/merge evidence.
  It is separately capped by `max_tasks` and cleared on stop; the wire task
  store enforces its own TTL, but the summary list is not independently
  TTL-swept.

### Surfaces

- Protocol DTOs in `vp-protocol/src/a2a.rs`: policy, status, task summary,
  start/stop/list params, and stable `A2A_*` error codes. Status never contains
  a token.
- SwiftUI `A2AStatusCard` shows running/disabled, loopback URL/card URL, task
  capacity, token presence (never value), inbound warning, and last error.
- CLI prints the one-time token for the operator; the GUI does not render it.

## Security properties and non-claims

- Loopback-only is a network binding property, not a sandbox for the host
  process or the handler.
- Inbound card-only rejection is not a promise that A2A tasks can execute;
  it is an explicit fail-closed placeholder.
- The facade does not approve local approvals, bypass gates, submit Verifier
  verdicts, merge, or change budgets/capacity.
- No public A2A endpoint, push notifications, remote registry, identity
  federation, or cross-host task transport is implemented.
- A bearer token is bearer authority for this one facade; losing it requires a
  stop/start with a new token.

## Verification

- `cargo test -p vp-a2a` — model/policy/store/server/client/SSE/auth/bounds.
- `cargo test -p vp-core --test a2a_facade` — disabled default, approval,
  non-loopback rejection, card, card-only task rejection, task list, stop.
- Full `cargo test --workspace`: 4,227 passed / 0 failed / 6 ignored; fmt and clippy passed.
- Swift models/views are typechecked against the real theme files; full
  `xcodebuild test` passed (403 app + 132 helper tests).

## Limits

- Card-only inbound is intentionally incomplete C11: no execution bridge, no
  `input-required` resume, no external-gate mapping yet.
- Task store is in-memory, same-daemon, and single-process.
- No task persistence/replay, push notification, multi-token rotation, CRL,
  public listener, or cross-host scheduler.
- The token is returned exactly once; the GUI only displays presence.
- Handler execution is host-supplied; the crate itself is not an OS sandbox.

## Related

- ADR-0195 external gates
- ADR-0193 task contracts
- ADR-0219 Code Mode (capability-scoped execution)
- `docs/CONTRACTED_EXECUTION_PLAN.md` C11

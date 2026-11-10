# ADR-0143: Long-task operator console in the macOS app

- **Status:** Accepted
- **Date:** 2026-11-11
- **Owners:** macOS client, Runtime, Agent Orchestration
- **Related:** ADR-0034, ADR-0039, ADR-0139, ADR-0140, ADR-0141

## Context

The daemon already ships the long-task controls: operator steering, deferred
session continuations, stall observations, and policy-gated stall recovery.
The SwiftUI client had no surface for any of them, so an operator had to leave
the app for the CLI to inspect or influence a run that had been active for
hours or days. The missing surface is not a new scheduling feature. It is a
client for existing, event-backed RPCs.

Three constraints shape the console:

- Steering and continuations are operator-only primitives, not agent-to-agent
  tools and not hidden prompt injection.
- Stall recovery is deterministic and policy-gated; an `observe` policy writes
  no recovery rows.
- Every action must call the daemon and surface the daemon's error. A local
  optimistic success or fixture data would misrepresent state.

## Decision

The Manage Dash **Team & Tasks** section gains four session-scoped cards,
backed by `AppModel` loaders and actions:

| Surface | RPCs | Behavior |
|---|---|---|
| `SteeringCard` | `session.steering`, `session.steering.cancel`, `session.steer` | Lists pending, delivered, and cancelled steering rows; cancels a still-pending row after confirmation; opens the steer sheet. |
| `ContinuationCard` | `session.continuations`, `session.continuation.cancel`, `session.defer` | Lists continuations with watch mode/ids, deadline, wake reason, attempts, outcomes, and last error; cancels pending/ready/delivering rows after confirmation; opens the defer sheet. |
| `RecoveryCard` | `orchestration.recoveries` | Read-only recovery intents and outcomes. There is no cancel RPC because the daemon owns the bounded recovery state machine. |
| `TaskStallCard` | `session.stalls` | Read-only task progress and stall observations, including the configured threshold and a note that observations do not cancel or retry tasks by themselves. |

`SteerSheet` exposes a multiline instruction, an optional running/waiting agent
target, and an optional plan-task scope. `DeferSheet` exposes a prompt, a
multi-select of stored plan tasks, an `all`/`any` wake mode, and either a
relative or absolute deadline. Both validate against the same limits as the
daemon and show the RPC error inline.

The Stage header adds a `Steer` button for `running` or `paused` sessions,
`Defer Continuation…` in the session menu, and a compact indicator for
pending/ready continuations and active recoveries.

### Steering is cooperative and addressed to one run

Steering remains persisted operator text drained immediately before the next
provider request. The sheet states that delivery cannot interrupt an in-flight
model call or tool execution. The UI never pretends to stop the current stream,
and it does not broadcast to every agent.

### Continuations are operator-created and fail closed

Creating a continuation is not starting a run. The card and sheet describe the
daemon's actual delivery rule: the row becomes ready when the watch condition or
deadline fires, but the daemon starts it only when the session is idle, failed,
or cancelled, and only through the normal start path. It adds no plan task and
does not bypass dependencies, approvals, budgets, verification, or write scopes.
A restart during delivery leaves the row failed closed rather than guessing.

### Recovery stays read-only and policy-gated

The app renders recovery state and last error but offers no recovery action. A
recovery row exists only when the project orchestration policy opts into
`escalate` or `retry`; `observe` records stalls without creating recoveries.
Operators change that policy through the existing project policy surface, not
through a per-task shortcut in this console.

### Refresh is event-driven

Selecting a session loads all four lists. The central event router schedules a
debounced refresh on `steering.*`, `session.continuation.*`,
`orchestration.recovery_*`, `orchestration.stall`, and `plan_task.*`. Manual
refresh buttons and post-action reloads use the same RPC loaders. Stale results
are discarded with the existing `selectionGeneration` guard.

## Consequences

- Positive: operators can inspect and influence a long run without leaving the
  app; all four surfaces share the daemon's real state and error semantics.
- Positive: the UI does not imply powers the backend does not have (recovery
  cancel, steering interruption, forced continuation start).
- Negative: the console is session-scoped, so cross-session troubleshooting
  still uses Mission Control and the CLI.
- Negative: four additional reads occur per session selection and per debounced
  event burst; they are bounded to 50 rows per list and remain cheap relative
  to a provider call.
- Follow-up: if the daemon gains a truthful recovery-cancel RPC or recurring
  continuations, add them as explicit surfaces rather than overloading these.

## Deliberately not included

- No recovery cancel or retry action in the app.
- No recurring/cron-style continuations.
- No live steering stream or typing indicator for steering delivery.
- No agent-facing steering or continuation tools.
- No optimistic fake rows, preview data, or local state transitions that the
  daemon has not confirmed.

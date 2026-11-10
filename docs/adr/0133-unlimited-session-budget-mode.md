# ADR-0133: Explicit unlimited session budget mode

- **Status:** Accepted
- **Date:** 2026-11-10
- **Owners:** Runtime, Budgets, macOS UI, CLI
- **Related:** ADR-0003, ADR-0018, ADR-0040, ADR-0121, ADR-0132

## Context

Budgets are a safety and cost-control feature, not a statement that every run
must be economically bounded. Large engineering work can legitimately exceed a
default 200k-token session budget, a plan task budget, or a project pool. Before
this ADR the only way to approximate "unlimited" was to create a session with no
token/cost/time values and hope no project or plan budget applied. That was not
an explicit mode, was not discoverable in the UI, and could not be selected for
an already-running session.

Unlimited must remain an intentional choice. Silently extending a limit when a
provider returns `budget exhausted` would defeat the purpose of budgets and make
spend unpredictable.

## Decision

### 1. A first-class session flag

`Session.unlimited` is a persisted boolean, default `false` for legacy sessions.
It is set at creation through `unlimited: true`, or toggled later through
`session.budget.set` (`SetSessionUnlimitedParams`).

Creation with `unlimited: true` rejects explicit token/cost/time values instead
of silently ignoring them. Toggling an existing session to unlimited retains its
stored numeric budgets, so disabling unlimited later restores the previous
limits exactly.

### 2. Unlimited bypasses economic gates, not runtime safety

When the flag is on, the session bypasses:

- session token, cost, and wall-clock limits;
- TeamPlan task and department token/cost limits;
- the project shared token/cost pool for this session;
- pre-flight budget reservations and budget-stop events.

It does **not** bypass:

- daemon-wide session admission or Worker/Labour capacity;
- approvals, the write sandbox, or Verifier gates;
- context-window limits and compaction;
- `max_turns` or provider request timeouts;
- cancellation, pause/resume, or verification.

The UI and CLI describe this distinction explicitly. Unlimited is not a
security boundary and is not a hidden model route.

### 3. Runtime toggles affect live runs at the next boundary

The engine keeps a shared `Arc<AtomicBool>` per session. Every `SessionBudget`
created for a run holds the same flag, so enabling or disabling unlimited while
a session is running is visible at the next turn/tool boundary without a restart.
The persisted flag remains authoritative after daemon restart; the map entry is
cleared when the run finalizes.

### 4. Event-backed and replayable

The creation event includes `unlimited` and `budget_mode`. A toggle writes
`session.budget_mode_changed` with both fields in one event+projection
transaction and appends the JSONL mirror. `session.replay` compares the projection
against the latest event, `session.repair` restores it, and `db-rebuild` reads it
from `session.created` plus later mode-change events. Session bundles export and
import the flag.

### 5. Product surface

- New Session sheet: an explicit `Unlimited budget` switch; budget fields are
  disabled and a warning explains what is and is not bypassed.
- Settings → Providers → Session defaults: a persisted default for new sessions,
  off by default.
- Stage session menu: enable/disable unlimited for the selected session.
- Stage usage label: `unlimited budget` instead of a finite token denominator.
- CLI: `vp session-new --unlimited` and `vp session-unlimited <session>
  [--enabled false]`.
- API: `session.create { unlimited: true }` and
  `session.budget.set { unlimited: bool }`.

## Consequences

- A user can deliberately run a large project without economic stops while
  retaining approvals, verification, capacity, and context safeguards.
- Spend is still recorded in the usage ledger; unlimited means "do not stop",
  not "do not measure". The Cost tab and CSV export remain accurate.
- A project shared budget can be intentionally bypassed by an unlimited session.
  The warning and audit event make this visible; operators who require a hard
  project-wide ceiling should not enable unlimited for that session.
- A very long run can consume substantial provider tokens/cost. There is no
  artificial hidden cap in the unlimited branch.
- The toggle is process-local in its immediate effect but persisted for restart
  and replay; it is not a distributed fencing mechanism.

## Alternatives considered

- **Use `token_budget: 0` as unlimited.** Rejected: zero is invalid and
  ambiguous, and it cannot express "restore my previous limits".
- **Auto-extend on exhaustion.** Rejected: hidden spend and no explicit user
  intent.
- **Global environment variable only.** Rejected: too broad and not inspectable
  per session.
- **Unlimited bypasses approvals/capacity/verification too.** Rejected: those are
  safety and correctness gates, not economic gates.
- **A separate "cost ceiling" that unlimited still honors.** Deferred: it would
  make the mode partially limited and harder to explain; the project can still
  use an external spend monitor.

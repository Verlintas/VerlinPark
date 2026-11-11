# ADR-0157: Project programs and milestones

- **Status:** Accepted
- **Date:** 2026-11-11
- **Owners:** Runtime, Store, Protocol, macOS
- **Related:** ADR-0041, ADR-0139, ADR-0140, ADR-0143, ADR-0151

## Context

Sessions and TeamPlans are bounded by one run. A session worktree, a stored plan,
and a merge queue entry are enough to finish a task, but they do not describe a
multi-day objective whose work must cross several sessions. Operators currently
encode that objective in prose, create sessions by hand, and remember which
pieces are blocked, retrying, or already verified.

The missing layer is a project-scoped ledger with milestones, dependencies,
bounded retries, and budgets. It must not become a second scheduler with
different safety rules, and it must not let a model decide — invisibly — which
workstream runs next. Existing admission, capacity, write leases, approvals,
sandboxing, the Verifier, the Integrator, and the merge queue must remain the
only execution path.

## Decision

Add a project program ledger, a deterministic `ProgramController`, and an
additive protocol/UI surface. A program is a plan for plans, not a runtime.

### Program and milestone model

`programs` is project-scoped:

| Column | Meaning |
|---|---|
| `id` | UUIDv7 primary key |
| `project_id` | Owning project, foreign key with cascade delete |
| `goal` | Operator objective, 1–2000 characters |
| `status` | `draft`, `active`, `paused`, `completed`, `failed` |
| `policy` | Serialized `ProgramPolicy` |
| `token_budget` | Optional aggregate program token cap |
| `cost_budget_usd` | Optional aggregate program USD cap |
| `reason` | Pause/failure/block explanation |
| `archived` | Imported bundle ledgers are read-only archives |
| `created_at`, `updated_at`, `completed_at` | RFC 3339 timestamps |

`program_milestones` holds the ledger:

| Column | Meaning |
|---|---|
| `id` | UUIDv7 primary key |
| `program_id` | Foreign key with cascade delete |
| `key` | Stable human ordinal or dotted key (`1`, `1.2`), unique per program |
| `title`, `objective` | Human work definition |
| `acceptance` | JSON string array, 1–32 non-empty criteria |
| `deps` | JSON string array of milestone ids in the same program |
| `priority` | `-100..=100`, used only for deterministic ordering |
| `status` | `pending`, `ready`, `running`, `verifying`, `completed`, `failed`, `blocked`, `cancelled` |
| `session_id` | The one normal session that is executing this attempt |
| `plan_revision` | Revision recorded when the one-task plan was attached |
| `attempts`, `max_attempts` | Automatic start count and per-milestone cap (1–5) |
| `next_attempt_at` | Backoff gate for a retryable failure |
| `result`, `last_error` | Outcome/error evidence |
| `profile`, `write_scope` | Deterministic execution hints for the one-task plan |
| `token_budget`, `cost_budget_usd` | Optional per-milestone caps |
| `created_at`, `updated_at`, `completed_at` | RFC 3339 timestamps |

`ProgramPolicy` is declarative:

- `auto_dispatch` — default `false`; dispatch is an explicit opt-in.
- `max_attempts` — default 2, range 1–5.
- `retry_backoff_secs` — default 60, range 0–86400, exponential per attempt.
- `dependency_failure` — `block` (default), `cancel`, or `retry`.
- `token_budget` / `cost_budget_usd` — optional defaults inherited by a
  milestone that declares no budget of its own.
- `max_parallel_milestones` — default 2, range 1–10.

Every new protocol field and manifest/bundle collection uses `serde(default)` or
an explicit decoder fallback. Older clients that omit the program fields decode
unchanged, and older bundles import as an empty ledger.

### Strategy → Execution → Verification

The program layer has exactly three deterministic phases:

1. **Strategy** is the persisted program goal, milestone definitions,
   dependencies, priorities, policies, and budgets. It is authored through RPC
   or the Manage Dash card. There is no LLM program planner and no hidden model
   call in the controller.
2. **Execution** is a normal `session` with a normal one-task `TeamPlan`. The
   controller chooses a ready milestone by priority, aging, dependency
   completion, attempt budget, and the per-program parallelism cap. It claims
   the milestone atomically, then creates the session through
   `create_session_with_options`, submits the plan through `set_team_plan`, and
   starts it through the existing `resume_plan` path. Admission, global session
   capacity, Worker/Labour caps, budgets, approvals, Seatbelt, write leases,
   Verifier, Integrator, and merge queue are unchanged.
3. **Verification** is the existing Worker → Verifier → Integrator path. A
   milestone is marked `verifying` only after the linked session is idle, its
   plan task is completed, and any merge queue entry for that task is
   integrated. `completed` additionally requires non-empty acceptance criteria
   and records a result summary. A failed attempt follows bounded backoff; an
   exhausted attempt or milestone budget becomes `blocked` for operator action.

The controller never reads a model response to choose a milestone, and the
program layer never writes to a session worktree directly.

### Atomic claim and no duplicate sessions

Starting a milestone is two-phase because creating a git worktree cannot be part
of a SQLite transaction:

1. `claim_program_milestone_start` atomically verifies the program is active and
   unarchived, the milestone is runnable, dependencies are complete, the
   attempt budget is available, and the retry backoff is due. It then sets
   `running`, increments `attempts`, clears the previous session, and appends
   `program.milestone.started` with the full milestone row.
2. The launcher creates the session and one-task plan, then attaches
   `session_id` and `plan_revision` in a second event-backed update.

A second tick or manual RPC cannot claim the same row. A crash after step 1 and
before step 2 leaves a `running` row with no session; after a grace period the
controller records a failed attempt and applies the normal retry/block rules.

### Readiness, aging, and dependency rules

Ready candidates are selected deterministically:

- program must be `active`, not archived, `auto_dispatch` must be true, and the
  program budget must not be exhausted;
- active slots are at most `max_parallel_milestones`;
- milestone status is `pending`, `ready`, or retry-due `failed`;
- attempts are below `max_attempts`;
- every dependency is `completed`;
- effective priority is the milestone priority plus one aging point per
  `VERLINPARK_PROGRAM_AGING_SECS` (default 60), capped at +100, plus a +10
  retry boost. Ties use creation time then milestone id.

The controller persists `ready` when dependencies are met. A paused program does
not mark or start new work, but in-flight sessions continue. A `blocked`,
`cancelled`, or otherwise permanently failed dependency applies the program
policy:

- `block`: dependents become `blocked` with a precise reason;
- `cancel`: dependents become `cancelled`;
- `retry`: dependents remain `pending`; they do not spin and become ready only
  after an operator retries the dependency or an automatic retry succeeds.

### Completion, failure, and budgets

A program is completed only when every milestone is terminal (`completed`,
`blocked`, or `cancelled`), at least one milestone is `completed`, and no
milestone is `blocked`. A program with zero completed milestones or any blocked
milestone becomes `failed`; it never reports success because a sibling was
cancelled. `failed` milestone rows are transient retry states, not terminal
success.

Budget rules:

- A program's aggregate spend is the sum of every linked session across every
  attempt; the append-only `program_milestone_sessions` dispatch ledger is never
  cleared by a retry and is rebuilt from program events on import/`db-rebuild`.
  Program caps gate new dispatch.
- A milestone session is created with the smaller of its **remaining**
  milestone budget (the declared or inherited cap minus prior attempts' spend)
  and the remaining program budget. A retry receives only the remainder, so a
  milestone cap is cumulative rather than per attempt. If the program has an
  aggregate cap but the milestone declares none, the remaining program budget
  becomes the session cap.
- Existing session and plan-task budgets still win. The program controller
  cannot widen them.
- When a program budget is exhausted, pending/ready/retryable milestones are
  blocked. The program fails once no running/verifying milestone remains. When
  a milestone budget is exhausted across its attempts, that milestone is
  blocked and is not retried, even if automatic attempts remain.

### Pause, resume, cancel, delete

- `program.pause` is cooperative and event-backed: it stops new dispatch and
  leaves running/verifying work alone. `program.resume` reactivates a draft or
  paused program. Terminal programs cannot resume.
- `program.cancel` is an operator stop: it sends the normal cooperative
  cancellation signal to linked running sessions, marks every non-terminal
  milestone `cancelled` with `program.milestone.cancelled`, and marks the
  program `failed` with an explicit reason. Partial work is never reported as
  completed.
- `program.milestone.skip` marks one milestone `cancelled` and emits
  `program.milestone.skipped`; it is not a success.
- `program.delete` is limited to draft or terminal programs with no
  running/verifying milestones. It appends a `program.deleted` tombstone before
  deleting the projection rows, so replay does not resurrect the program.
- `program.milestone.retry` is an explicit operator action that resets automatic
  attempts to zero. It does not bypass dependency, budget, admission, or
  verification gates.

### Events, replay, repair, bundles, and rebuilds

Program events are separate from the session event stream because a program
outlives every session. `program_events` is append-only, keyed by
`(program_id, seq)`, and stores full serialized program/milestone rows in the
payload. The vocabulary is:

`program.created`, `program.updated`, `program.paused`, `program.resumed`,
`program.completed`, `program.failed`, `program.deleted`,
`program.milestone.created`, `program.milestone.updated`,
`program.milestone.started`, `program.milestone.verified`,
`program.milestone.completed`, `program.milestone.failed`,
`program.milestone.blocked`, `program.milestone.retried`,
`program.milestone.skipped`, `program.milestone.cancelled`.

Every projection change and its event commit in one SQLite transaction; the
event is inserted before the row update. Replay folds the ordered full-row
events, honors the delete tombstone, compares the expected program and milestone
rows with the projection, and optionally replaces the projection
(`program.replay`, `vp program-replay`). `program.status` returns a bounded
recent event tail.

The project manifest (`projects.jsonl`) carries programs, milestones, and the
program event log. `vp db-rebuild` restores all three before restoring sessions,
so a database rebuild preserves the ledger and its replay/audit history.

A session bundle carries every program linked to that session, all milestones
for those programs, and their events. Importing a bundle is explicitly an
archive operation: program rows are marked `archived`, active/draft programs are
paused, `program.resume` refuses archives, and the controller skips them. Import
never overwrites a live program with the same id.

### RPC and UI

Additive RPC methods:

- `program.list`, `program.create`, `program.update`, `program.pause`,
  `program.resume`, `program.cancel`, `program.delete`, `program.status`;
- `program.milestone.list`, `program.milestone.create`,
  `program.milestone.update`, `program.milestone.retry`,
  `program.milestone.skip`, `program.milestone.start`;
- `program.tick` and `program.replay` for diagnostics.

CLI: `vp programs`, `vp program-status`, `vp program-create`,
`vp program-update`, `vp program-pause`, `vp program-resume`,
`vp program-cancel`, `vp program-delete`, `vp program-milestone-add`, `vp program-milestone-retry`,
`vp program-milestone-skip`, `vp program-run`, `vp program-tick`, and
`vp program-replay`.

Manage Dash adds a Programs card with goal, status, budgets, milestone counts,
pause/resume/delete, a detail sheet with the milestone ledger, dependency
display, attempt counts, errors/results, start/retry/skip actions, a milestone
composer, and a recent-event tail. The Swift models decode additive fields and
treat missing policy/counts/usage as safe defaults.

### Validation

The engine and store validate:

- program goal length; token budget > 0; USD budget finite and > 0;
- policy attempt/backoff/parallel bounds and non-negative budgets;
- milestone title, objective, acceptance count/length, write scope, profile
  (`coding`, `research`, `verification`, `generic`), priority, attempt cap;
- milestone key syntax and per-program uniqueness;
- dependency existence, no duplicates, no self-dependency, and no cycles.

Invalid input fails closed with a precise error and creates no event or row.

## Consequences

Positive:

- Multi-session work has a persisted, auditable ledger instead of operator
  memory or prose.
- Scheduling remains deterministic Rust; no hidden routing or prompt-only state.
- Sessions, budgets, capacity, approvals, sandboxing, Verifier, Integrator, and
  merge queue remain the only execution and safety path.
- Replay, repair, manifest rebuilds, and bundle archives carry the ledger.

Costs and limits:

- A program is not a distributed scheduler: one daemon owns its SQLite ledger.
- The controller is same-daemon cooperative. It does not fence a second daemon
  or preempt a running provider call.
- Aggregate program spend is derived from session counters; it is not a
  pre-flight reservation across every possible future milestone.
- A crash after a milestone claim may consume one attempt before the abandoned
  dispatch is observed.
- Automatic dependency `retry` can leave dependents pending indefinitely if an
  operator never retries a permanently blocked dependency. That is intentional:
  the controller does not spin or invent a success.

## What is not claimed

- No LLM program planner or model-authored milestone selection.
- No cross-project programs. Programs are project-scoped.
- No distributed or cross-process program scheduler, fencing token, or
  leadership election.
- No bypass of session/task budgets, admission, capacity, write leases,
  approvals, sandboxing, Verifier, or merge gates.
- No new agent tool. Programs are an operator/runtime surface, not a hidden
  capability for agents.
- No autonomous code changes outside a normal session worktree.

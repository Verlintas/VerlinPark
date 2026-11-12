# ADR-0210: Agent hiring, retirement, and performance

- **Status:** Accepted
- **Date:** 2026-11-13
- **Phase:** S6 of the project-scoped agent identity program
- **Depends on:** ADR-0205 (identity schema/lifecycle), ADR-0206 (assignments and
  inbox), ADR-0207 (identity prompts/role contracts), ADR-0208 (Worker
  workspaces), ADR-0209 (identity memory), ADR-0198 (failure attribution)
- **Does not implement:** S7 org-chart UI, S8 team templates, automatic hiring,
  firing, promotion, transfer, or LLM performance judgment.

## Context

S1 introduced durable project-scoped Leader/Manager/Worker identities with a
bounded lifecycle and project caps. S2 added session assignments and identity
leases. S4 added component workspaces and explicit archive/retire states. S5
added evidence-backed identity memory. C6 added a deterministic, advisory
failure-attribution DAG.

What was still missing was the employment layer: an explicit way to add a
Manager/Worker, move one between departments, record a promotion or retirement
decision, and inspect what an identity has actually done across sessions. The
runtime already produced the raw evidence (plan task outcomes, usage ledger,
merge-queue Verifier verdicts, failure attributions), but no bounded read model
turned it into accountability.

S6 adds that layer without turning identities into processes or giving any
controller authority to fire an employee. It also closes two bypasses in the S1
surface: `agent.identity.create` is now the Leader bootstrap path, and the
generic `agent.identity.update` no longer changes role/department/component/
parent. Those changes go through the approval-classified S6 operations. The
legacy `agent.identity.retire` RPC remains as a compatibility alias, but it now
follows the same retirement-approval gate and fails closed by default.

## Decision

### 1. Hire is an explicit request, never a side effect

`agent.hire` accepts a project, role (`manager|worker`), department, component,
title, profile, model, parent identity, optional bounded policy metadata, an
explicit `approved` flag, and an operator label. The Leader is never hired
through this RPC; it is bootstrapped through the S1 Leader creation path.

The project `AgentIdentityPolicy` is authoritative:

- project caps (`max_managers`, `max_workers`) always apply, even when identity
  assignments are disabled or the policy `enabled` flag is false;
- a Manager parent must be the project's Leader;
- a Worker parent must be a Manager;
- a Leader cannot have a parent;
- one live Leader per project is a database invariant;
- levels and profiles are validated with the S1 rules.

If `hire_requires_approval` is false, or the caller supplies `approved: true`,
the identity is created in the same transaction as an approved hire request.
Otherwise the operation persists a pending request and emits
`agent.hire.requested`; no identity, session, assignment, workspace, inbox, or
memory row is created. `agent.hire.approve` revalidates caps and hierarchy at
decision time because the project may have changed since the request, then
creates the identity using the identity id reserved by the request.
`agent.hire.reject` is terminal. A rejected request cannot be approved later.
An approved request cannot be re-decided.

The direct approved path emits both `agent.hire.requested` and
`agent.hire.approved` in one transaction so the audit stream does not depend on
whether the operator approved immediately or later. Every decision carries
`approval_risk: "host_state"` and an operator label. These are audit metadata,
not credentials.

Hiring is not exposed to a model tool. There is no `hire` tool in any frozen
bundle. A Leader or Manager cannot call an RPC through the agent loop; only an
operator/client can.

### 2. Promotion and transfer are org changes, not edits

`agent.promote` supports exactly two transitions:

- Worker → Manager;
- Manager → Leader, and only when no live Leader exists.

`agent.transfer` changes department, component, title, or parent without
changing role. A role change through transfer is rejected.

Both operations:

- require explicit `approved: true`; there is no automatic promotion or
  transfer and no model-facing tool;
- validate the resulting hierarchy, not just the supplied field;
- enforce Manager/Worker caps and the one-live-Leader invariant;
- increment `generation` exactly once;
- append the normal `agent.identity.updated` snapshot event;
- append one `agent.promote.approved` or `agent.transfer.approved` employment
  event with the previous generation/role and approval metadata.

Active work is a conflict, not a silent side effect. If the identity has an
active assignment, active session workspace binding, or (for a component
change) an active component workspace, the operation returns `IDENTITY_BUSY` or
`TRANSFER_CONFLICT`. An explicit `force` releases S2 assignments through the
normal `agent.assignment.released` event and archives S4 workspaces through the
normal `agent.workspace.archived` event. Branch names, commits, and all
coordination/workspace evidence are preserved. `force` is cooperative: it does
not interrupt an in-flight provider stream or pretend a running session was
stopped.

### 3. Retirement is terminal, evidence-preserving, and approval-gated

`AgentRetirementPolicy` is a project setting with a **disabled** default and
bounded thresholds:

| Field | Default | Bound |
|---|---:|---|
| `enabled` | false | — |
| `lookback_days` | 30 | 1–365 |
| `min_completed_tasks` | 3 | 1–1,000 |
| `max_consecutive_failures` | 3 | 0–100 (0 disables) |
| `max_verifier_rejections` | 2 | 0–100 |
| `max_unsubstantiated_handoffs` | 2 | 0–100 |
| `max_false_consensus` | 2 | 0–100 |
| `cooldown_secs` | 86,400 | 60–2,592,000 |
| `require_approval` | true | — |

`agent.retirement.propose` and `agent.retirement.candidates` compute
deterministic candidates from the performance ledger plus C6 attribution
evidence. An identity is eligible only when it has at least
`min_completed_tasks` completed tasks and crosses at least one enabled failing
threshold. Candidates carry the observed count, the threshold, a bounded
evidence list, and a deterministic severity key (reason count, then excess
evidence). There is no model score, no hidden ranking, and no global leaderboard.

Proposing appends `agent.retirement.proposed` and creates a pending proposal. It
does not change the identity status, release work, or notify a scheduler.
`agent.retirement.approve` and `agent.retirement.reject` record an explicit
decision. Approval does not execute retirement; a separate `agent.retire` call
is still required.

`agent.retire` is terminal. It requires `approved: true` when either
`AgentIdentityPolicy.retirement_requires_approval` or
`AgentRetirementPolicy.require_approval` is true. An approved proposal also
satisfies the requirement; a pending or rejected proposal does not. If both
policies disable approval, retirement still requires an explicit operator call,
but the `approved` flag is not required.

Retirement:

- refuses with `RETIREMENT_NOT_QUIESCENT` when active assignments, session
  workspace bindings, or component workspaces remain, unless `force` is set;
- with `force`, releases assignments and archives workspaces exactly as
  promotion/transfer do, preserving every branch and event;
- records an optional successor that is same-project, non-retired, and either
  the same role or the immediate subordinate role (Worker for a Manager, Manager
  for a Leader); successor appointment/promotion remains a separate explicit
  operation;
- marks the identity `retired`, sets `retired_at`/`successor_id`, and appends
  `agent.identity.retired` plus `agent.retirement.executed`;
- marks an attached proposal `executed` and keeps its evidence.

A retired identity cannot be assigned, receive inbox work, own a workspace, be
promoted/transferred, or be edited as an employee. There is no rehire under the
same id. Memory, performance, proposal, employment, workspace, and lifecycle
evidence are never deleted by retirement.

### 4. The performance ledger is derived, bounded, and descriptive

`agent_performance` is a deterministic read model over existing evidence:

- final plan task outcomes per task (`completed`, `failed`, `cancelled`,
  `blocked`, `skipped`), with retries counted separately from
  `plan_task.retry_scheduled`;
- trailing consecutive failures;
- Verifier approvals/rejections from merge-queue `verifier_approved` values
  (a `None` value is not a verdict and is not counted);
- failure-attribution reason counts and evidence pointers;
- token/cache/cost totals and estimated-call counts from the usage ledger;
- assignment/session counts and first/last activity timestamps.

Attribution is deterministic and conservative. The ledger prefers an explicit
`identity_id` in the event, then the run record's `identity_id`, then a unique
run/agent mapping, then a unique session assignment matching role and (when
present) department/title. Ambiguous evidence is not attributed. The ledger
never reads prompt text, goals, tool arguments, tool output, file content, or
secrets. It does not make scheduling, merge, assignment, budget, or model
routing decisions.

Reads (`agent.performance`, `agent.performance.summary`,
`agent.retirement.candidates`) compute the ledger without writing it.
`agent.performance.replay` compares the stored projection with the current
evidence and can explicitly repair it. `db-rebuild` recomputes it after session
events are restored. Bundle import keeps performance rows as advisory evidence
and never lets an imported row overwrite a local row.

### 5. Storage and replay

Additive SQLite tables:

- `project_agent_retirement_policies` — bounded project policy;
- `agent_hire_requests` — reserved identity id, request fields, status, decision
  metadata, origin;
- `agent_retirement_proposals` — bounded reasons/evidence/performance snapshot,
  status, decision metadata, successor, execution timestamp, origin;
- `agent_employment_events` — append-only project stream, portable by event id,
  with a per-project sequence;
- `agent_performance` — derived read-only projection keyed by
  `(project_id, identity_id)`.

Identity lifecycle events remain in `agent_identity_events`. Assignment release
and workspace archive events remain in their S2/S4 streams. The employment
stream is the audit projection for hire/promote/transfer/retirement decisions;
it does not replace any of the underlying lifecycle streams.

Bundle export/import carries pending/decided hire requests, retirement
proposals, performance rows, and employment events. Imported requests and
proposals are marked `origin: "imported"` and can never be approved, rejected,
or executed locally. Imported employment events are byte-compared by id and
fail closed on a sequence collision. `db-rebuild` rebuilds proposals/requests
from the employment stream and recomputes performance from session evidence.

### 6. Surfaces

RPC:

- `agent.hire`, `agent.hire.approve`, `agent.hire.reject`,
  `agent.hire.list`;
- `agent.promote`, `agent.transfer`;
- `agent.retire`;
- `agent.retirement.propose`, `.approve`, `.reject`, `.proposals`,
  `.candidates`;
- `agent.performance`, `.summary`, `.replay`;
- `agent.employment.replay`;
- `project.retirement_policy.get/set`.

CLI:

- `vp agent-hire`, `vp agent-hires`, `vp agent-hire-approve`,
  `vp agent-hire-reject`;
- `vp agent-promote`, `vp agent-transfer`;
- `vp agent-retire --approved --force --proposal`;
- `vp agent-performance [--identity]`;
- `vp agent-retirement [--candidates|--proposals|--propose|--approve|--reject]`;
- `vp retirement-policy`, `vp retirement-policy-set`.

Swift decodes the read-only performance summary, candidates, proposals, hire
result, and retire result. Manage Dash adds read-only performance and retirement
cards. S7 owns hire/retire forms, the org chart, and mutation controls.

All new protocol fields use `serde(default)`. New error codes are stable wire
values under `agent_identity_codes`.

## Invariants preserved

- Identity never bypasses role/profile policy, approvals, Seatbelt, path jail,
  write scope, Verifier, merge queue, capacity, or budget.
- Hire/promote/transfer/retire are operator RPCs, not model tools and not
  scheduler actions.
- No automatic firing, promotion, transfer, rehire, or Manager creation.
- Retirement never deletes evidence or branches.
- Identity/memory/performance state is project-scoped and never shared across
  projects.
- A project without a retirement policy keeps the disabled default; no
  candidate is produced implicitly.
- C6 diagnosis remains advisory evidence, not a verdict.

## Limits and non-goals

- The ledger is a deterministic attribution heuristic, not an exhaustive audit.
  Ambiguous evidence is skipped, so per-identity totals can be lower than the
  session total.
- Raw shell/test output is not parsed into performance; only recognized task,
  usage, merge-queue, and C6 evidence is used.
- Retirement proposals do not transfer memory or component-branch ownership.
  The successor pointer is recorded; a future explicit transfer must move
  memory/branch ownership. This is deliberately not silent.
- `force` releases coordination/workspace state but does not kill a provider
  stream or guarantee the remote side stopped.
- The employment stream is same-database SQLite state, not distributed
  consensus or cross-host employment control.
- There is no public webhook, external HR integration, compensation model,
  interview workflow, or identity federation.
- No LLM performance judgment, hidden ranking, automatic remediation, or
  auto-rehire.
- No S7 org-chart GUI, team templates, cross-project identity sharing, or
  automatic Manager creation.
- Performance evidence is retained indefinitely in this phase; there is no
  separate performance-retention sweep.

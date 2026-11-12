# 0205 — Project-scoped agent identities (S1)

- **Status:** Accepted
- **Date:** 2026-11-12
- **Base:** `950649a` (after ADR-0197)
- **Scope:** S1 of `docs/PROJECT_SCOPED_AGENT_IDENTITIES_PLAN.md`
- **Related:** ADR-0172 (role/profile policy), ADR-0188 (prompt layout),
  ADR-0193 (task contracts), ADR-0196 (runtime pinning)

## Context

The runtime is run-scoped. Every Leader, Manager, Worker, Labour, Verifier, and
Scout execution gets an `agents` row tied to one session with
`ON DELETE CASCADE`. A long-lived project therefore has no durable roster,
generation, retirement, or project-level accountability boundary. The stored
`TeamPlan` is the only continuity across sessions.

The project-identity plan separates the durable employee record from execution.
A project owns Leader, Manager, and Worker identities. Labour remains task-scoped
and is never hired into the roster. Verifier and Scout remain session/task gates.
S1 is the schema, lifecycle, policy, and migration foundation only. It does not
assign identities to sessions, create inboxes or workspaces, inject identity
prompts, move memory to identities, implement hire/retire approval workflow, or
add an org-chart UI.

## Decision

### 1. Identity and run are separate records

`agent_identities` is the project-scoped employee record. `agents` remains the
existing session-scoped run/roster projection and gains a nullable
`identity_id`. S1 never writes that column: run creation is unchanged, and no
synthetic identity is invented for an existing or new run.

A compatibility read-only view, `agent_runs`, projects the existing `agents`
columns plus `started_at`/`ended_at`. It is not a second write path. A later
phase may replace the view with a real run table, but that is outside S1.

The identity record is bounded metadata:

```text
id, project_id, role, department, component, title, profile, model,
parent_identity_id, status, generation, created_at, updated_at, retired_at,
successor_id, policy_json, origin
```

`origin` is `local` or `imported`. It is storage provenance, not lifecycle
state. Imported rows are read-only until an explicit rehome phase; S1 has no
rehome RPC, so imported rows remain immutable.

`policy_json` is a bounded JSON object (8 KiB) for future identity-scoped
metadata. It is never prompt text, a model-output store, or a credential. It
cannot widen approvals, sandbox, model routing, budget, or role policy.

### 2. Project scope and roles

- `role` is `leader | manager | worker`. Labour, Verifier, and Scout are
  rejected by the RPC/store surface.
- A parent identity must exist in the same project. A Manager's parent, when
  present, must be a Leader; a Worker's parent must be a Manager. A Leader may
  not have a parent. Retired parents are rejected.
- A successor must exist in the same project, must not be the identity itself,
  and must not be retired. S1 records only a pointer; S6 owns transfer,
  promotion, and workspace/memory handoff semantics.
- A project has at most one live (non-retired) Leader. A partial unique index
  enforces this even under concurrent creation. Retirement releases the slot.
- Managers and Workers are capped by `AgentIdentityPolicy`. Caps apply to
  non-retired identities; retirement frees capacity.

### 3. Lifecycle and generation

The only S1 transitions are:

```text
active <-> paused
active | paused -> retired   (terminal)
```

`idle` is reserved for the session-assignment phase and is never produced in
S1. A retired identity cannot be resumed. It may receive a successor pointer,
and nothing else may change. Repeated pause or retirement calls are idempotent
no-ops and append no event.

`generation` starts at 1 and increments exactly once per request that changes
any of `role`, `department`, `component`, `title`, `profile`, or `model`. The
identity id is stable. A non-generation change (policy metadata, successor,
parent) updates `updated_at` but not `generation`. `created_at` is immutable.

Update events contain the full resulting identity snapshot, the changed field
names, and `previous_generation`/`generation`. No event contains free-form prose
or prompts. Event payloads are bounded metadata.

### 4. Event-backed projection and replay

Identity events are project-scoped and live in `agent_identity_events`
(`identity_id`, `seq`, `kind`, payload, timestamp). They are not session events
because an identity outlives the sessions that may later use it.

Kinds:

- `agent.identity.created`
- `agent.identity.updated`
- `agent.identity.paused`
- `agent.identity.resumed`
- `agent.identity.retired`

Each event carries `{"identity": <snapshot>, ...}`. The projection table is
rebuilt by replay. `agent.identities.replay` is read-only by default; with
`repair=true` it replaces the project projection from events and appends no
event. A malformed or cross-project event fails closed.

`agent_identity_events` is authoritative; `agent_identities` is derived. The
project manifest carries the policy, the projection snapshot, and the events so
`vp db-rebuild` can reconstruct both. Rebuild inserts events first and derives
the projection; it never invents a row or a lifecycle event.

### 5. Policy

`AgentIdentityPolicy` is a project setting:

```text
enabled = false
max_managers = 4
max_workers = 10
hire_requires_approval = true
retirement_requires_approval = true
```

The defaults preserve existing behavior. `enabled` only gates future identity
use; S1 has no session behavior to gate, so an operator can pre-provision a
roster while the policy is disabled. Caps always apply to explicit creation.
Validation rejects more than the runtime hard caps (5 Managers, 10 Workers),
not silently clamps them. The policy is stored in SQLite, written to
`projects.jsonl`, and restored by `db-rebuild`. `hire_requires_approval` and
`retirement_requires_approval` are stored/validated only; S6 owns the workflow.

### 6. Surfaces

RPC:

- `agent.identities.list`, `agent.identities.get`
- `agent.identity.create`, `.update`, `.pause`, `.resume`, `.retire`
- `agent.identities.replay`
- `project.agent_identity_policy.get`, `.set`

CLI:

- `vp agents <project>`, `vp agent-create`, `vp agent-update`,
  `vp agent-pause`, `vp agent-resume`, `vp agent-retire`,
  `vp agent-policy`, `vp agent-policy-set`

`vp agents <session>` keeps its legacy run-roster behavior by falling back when
the argument is not a project id; `--session` forces it. There is no hire,
assignment, inbox, or workspace command in S1.

Swift adds only read-only `AgentIdentity`/roster models, one decode path, an
`AppModel` list load, and a minimal Manage Dash roster card. Hire/retire and the
org chart remain S7.

All new protocol fields are `serde(default)`, so old payloads decode and new
fields can be ignored. Errors use stable typed codes (`IDENTITY_NOT_FOUND`,
`IDENTITY_INVALID`, `IDENTITY_ROLE_INVALID`, `IDENTITY_PROJECT_MISMATCH`,
`IDENTITY_CONFLICT`, `IDENTITY_RETIRED`, `IDENTITY_CAP_EXCEEDED`,
`IDENTITY_IMPORTED_READONLY`, `IDENTITY_POLICY_INVALID`) rather than message
text classification.

### 7. Bundle and migration

Session bundles carry the project identity snapshot and its event stream. Import
preserves project ids and never silently remaps an identity into another
project. Newly inserted archive identities get `origin=imported` and reject
lifecycle mutations. If an identity id already exists in another project, the
import fails closed. Event conflicts fail closed rather than merging payloads.

Existing `agents` rows are unchanged and replay unchanged. No identities are
created for old sessions. Disabling the policy leaves existing rows and runs
readable. Rollback to run-scoped mode is simply "do not assign identities";
S1 has no assignment to undo.

## Authority and cache implications

An identity is a durable accountability record, not a credential. It does not
change role/profile tool policy, approvals, Seatbelt, path jail, write scope,
Verifier, merge queue, capacity, budget, or model routing. S1 adds no prompt
section, so there is no cache-epoch change yet. S3 will add the stable
`agent/identity` head section and include identity generation in the cache
epoch; promotion/transfer/model changes will then move the epoch. S5 will keep
memory changes in the tail so they do not move the head prefix.

The identity `model` field is metadata only. It is never used for hidden
routing; user-configured role models and budgets remain authoritative.

## Tests

Covered in S1:

- create/get/list/update/pause/resume/retire lifecycle, generation increments,
  idempotent no-ops, and terminal retirement;
- role validation (`labour`, `verifier`, `scout` rejected);
- parent/successor project isolation and hierarchy validation;
- one live Leader and project cap enforcement;
- policy defaults, validation, isolation, and disabled-policy compatibility;
- event ordering, projection replay, explicit repair, manifest rebuild, and
  bundle export/import read-only origin;
- old `agents` rows keep `identity_id = NULL`, session replay is unchanged, and
  no assignment/inbox/workspace/memory tables or behavior exist;
- protocol decode (legacy `AgentRecord`, partial identity, double-option update
  params), CLI parsing/human output, and Swift read-only decoding.

## Limits and non-goals

- S1 does not assign identities to sessions, create leases, inboxes,
  workspaces, or memories, inject identity prompts, or change the Worker into a
  tech lead. S2–S8 remain separate phases.
- No automatic hiring, firing, promotion, transfer, or org-chart rewrite.
- No cross-project identity sharing or automatic rehome.
- No identity-scoped budgets or performance ledger in S1; the `policy_json`
  field is bounded metadata only.
- No distributed identity lease. The one-live-Leader index is same-database
  SQLite enforcement, not cross-host consensus.
- Imported identities are read-only because there is no rehome command; this is
  deliberate, not a hidden assignment path.
- An identity is not a security principal. The role/profile execution gate and
  all existing approval/sandbox/verification gates remain in force.

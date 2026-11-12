# 0206 — Session assignments, identity leases, and the project agent inbox (S2)

- **Status:** Accepted
- **Date:** 2026-11-13
- **Base:** `e2e064e` (after ADR-0205)
- **Scope:** S2 of `docs/PROJECT_SCOPED_AGENT_IDENTITIES_PLAN.md`
- **Related:** ADR-0205 (project-scoped identities), ADR-0055 (session
  operations), ADR-0195 (durable external gates), ADR-0196 (runtime pinning)

## Context

S1 introduced durable project-scoped Leader/Manager/Worker identities, but a
session could not use one. This phase adds the durable assignment record, a
same-host identity lease, and a project-scoped agent inbox. It deliberately
does not add workspaces, identity prompts, identity memory, hire/retire
approval, org-chart UI, or team templates. Those remain S3–S8.

The runtime is still run-scoped. A session's `agents` rows remain execution
records. An assignment is coordination metadata: it does not create an agent,
worktree, provider call, capacity slot, or hidden model route.

## Decision

### 1. One assignment table with a one-active-session invariant

`session_agent_assignments` stores one row per `(session_id, identity_id)`:

```text
session_id, identity_id, role, status (active | released | queued),
lease_token, lease_expires_at, assigned_at, released_at, heartbeat_at
PRIMARY KEY (session_id, identity_id)
```

A session may hold several identities (Leader, Managers, Workers). In v1 an
identity may be active in **one** session at a time. This is enforced twice:

1. a partial unique index on `identity_id WHERE status = 'active'`; and
2. an explicit transaction check that returns a typed conflict before any row
   is written.

The partial index is the database backstop; the explicit check is the stable
wire error. `queued` is reserved for a later portfolio phase and is never
produced by S2. There is no cross-host consensus: this is one SQLite database,
same host, same daemon family.

Assignment validation:

- the session and identity must exist;
- the identity must belong to the session's project;
- the identity role must be `leader`, `manager`, or `worker`;
- an optional requested `role` must equal the identity role;
- the project `AgentIdentityPolicy.enabled` must be true;
- retired identities are rejected; paused identities are rejected until
  resumed; imported identities are rejected until an explicit rehome;
- imported/landed sessions are final and reject new assignments or renewals;
  an existing row can still be released for cleanup;
- a live assignment in another session is a typed conflict.

Repeating `session.assign` for the same session/identity with a live lease is
an idempotent no-op (`created=false`) and writes no duplicate event. Once the
lease has expired, the stale row is released with `agent.assignment.expired`
before a new assignment is created. A live identity is never silently stolen.

The assignment role is a copy of identity role for query efficiency and
display. It is validated on every insertion and replay.

### 2. Identity lease

The identity lease prevents two sessions from driving the same durable
identity concurrently. It is a bounded coordination lease:

- default 600 seconds;
- explicit range 60–86,400 seconds (`normalize_assignment_lease_secs`);
- `lease_token` is a random internal fencing value; it is **not** returned by
  the RPC, CLI, events, bundles, logs, or the UI;
- `session.assignment.renew` extends a live lease and rotates the token;
- renewal of an expired lease fails closed with `ASSIGNMENT_LEASE_EXPIRED` and
  records the expiry;
- `session.release` releases only the named owning session's row and clears
  the token and expiry; repeated release is an idempotent no-op;
- the daemon startup path and a 30-second sweep call
  `expire_session_agent_assignments`, which releases due rows and emits one
  bounded `agent.assignment.expired` event per row.

Expiry is not a reassignment. A due lease becomes `released`; a later explicit
assignment can then claim the identity. The daemon never invents an
assignment, never picks a new session, and never terminates a live session.

The lease is same-database SQLite coordination. It is not a distributed lock,
not a cross-host fence, and not a security credential. Assignments do not
replace the existing runtime leases, write leases, GUI lease, or continuation
delivery claims.

### 3. Project-scoped inbox

`agent_inbox` stores pull-based messages for durable project identities:

```text
id, project_id, identity_id, session_id, from_identity_id,
kind (task | message | question | handoff | decision | review),
payload_json, status (unread | read | done | expired),
created_at, read_at, expires_at
```

Rules:

- recipient and sender must be non-retired `leader|manager|worker` identities
  in the same project;
- an absent sender is the stable `operator` label; operator rows have
  `from_identity_id = NULL` and `from_label = "operator"`;
- Labour, Verifier, and Scout never receive project inbox rows; they are not
  project identities, and task-scoped delivery stays outside this table;
- when `session_id` is present, it must belong to the same project, and if the
  recipient has an active assignment it must be that session;
- payloads must be JSON objects, at most 16 KiB, and must not use obvious
  credential-like keys at any depth. This is a shape guard, not a content
  classifier; callers still must not send secrets;
- `expires_in_secs` is optional and bounded to 60 seconds–30 days;
- `agent.inbox.list` filters by project/identity/session/status, sorts unread
  first then newest, and clamps to 500 rows;
- `agent.inbox.read` is idempotent from unread/read; `agent.inbox.ack` moves
  unread/read to done and is idempotent for done. A due TTL is recorded as
  `expired` and the read/ack call fails closed;
- the startup path and the 30-second sweep expire due unread/read rows with
  one `agent.inbox.expired` event each.

A row is durable project state. It survives daemon restart and session
archive. It is not a workflow engine, a task queue, or an execution trigger.

### 4. Optional wake continuation

Both commands accept `wake_session`:

- `session.assign { wake_session: true }`
- `agent.inbox.send { wake_session: true }`

If the target session is `idle`, `failed`, or `cancelled`, the daemon creates
a normal deferred continuation through the existing continuation path, with a
bounded `[inbox]` summary. If the session is running, queued, paused, imported,
landed, or missing, no continuation is created and the inbox row stays unread.
The command returns a bounded `wake` object (`requested`, `created`,
`continuation_id`, `reason`).

A wake:

- does not call a provider;
- does not create an agent run, worktree, or Labour slot;
- does not reserve capacity;
- does not bypass approvals, Seatbelt, path jail, write scope, Verifier,
  merge queue, budgets, or model routing;
- is cooperative: the scheduler delivers the continuation like any operator
  defer when the session is eligible;
- is idempotent per assignment period or inbox message id via a stable
  `session_operations` receipt (`coordination-wake:*`). Repeating the same
  assignment call replays the same continuation id instead of creating a
  second one.

For `agent.inbox.send` without `session_id`, the wake target is the recipient's
active assignment session if one exists. A message with no resolvable session
is still durably stored; only the wake is skipped.

### 5. Events, replay, and scope

Assignment and inbox events are **project-scoped** and live in
`agent_coordination_events`:

```text
id TEXT PRIMARY KEY, project_id, seq, kind, payload_json, created_at
UNIQUE(project_id, seq)
```

They are not session events because the one-active-session invariant is
cross-session and an inbox row belongs to an identity that outlives a sender
session. `seq` is a per-project replay order; `id` is a portable UUID so an
archive import can be idempotent. Kinds:

- `agent.assignment.created`, `.renewed`, `.released`, `.expired`
- `agent.inbox.message`, `.read`, `.done`, `.expired`

Each event carries the full non-secret row snapshot. No event carries a lease
token, raw provider key, prompt, file content, or tool output.

`agent.coordination.replay` folds the stream into both projections and compares
them with the stored rows. It is read-only by default; `repair=true` replaces
the project projections from events and appends no event. Repairs preserve a
live lease token where the `(session, identity)` row still exists and generate
a fresh internal token for newly derived live rows. Time-relative
`lease_active` is ignored in comparison.

`record_project_manifest` writes the policy-adjacent assignment rows, inbox
rows, and event stream to `projects.jsonl`; `vp db-rebuild` inserts events first,
derives both projections, and never invents a row or lifecycle event.
Session bundles carry the same project-scoped snapshots and stream. Import is
idempotent by event id and by assignment/inbox key; a conflicting event or row
fails closed rather than merging. Archived active assignments are evidence,
not a live local claim: the startup expiry sweep releases them when their
stored expiry has passed.

### 6. Surfaces and errors

RPC:

- `session.assignments`
- `session.assign`
- `session.assignment.renew`
- `session.release`
- `agent.inbox.list`
- `agent.inbox.send`
- `agent.inbox.read`
- `agent.inbox.ack`
- `agent.coordination.replay`

CLI:

- `vp assignments <session> [--identity|--project|--status]`
- `vp assign <session> <identity> [--role] [--lease-secs] [--wake]`
- `vp assignment-renew <session> <identity> [--lease-secs]`
- `vp release <session> <identity> [--operator] [--reason]`
- `vp inbox <identity> [--project|--session|--status|--unread]`
- `vp inbox-send <identity> [--project|--session|--from|--kind|--body|--payload-json|--expires-in|--wake]`
- `vp inbox-read <message> [--project|--identity]`
- `vp inbox-ack <message> [--project|--identity]`
- `vp coordination-replay <project> [--repair]`

All assignment/inbox protocol fields use `serde(default)`. Errors use stable
typed codes (`ASSIGNMENT_*`, `INBOX_*`) and are never classified by message
text. Missing rows map to JSON-RPC `NOT_FOUND`; validation/conflict/exhaustion
map to `INVALID_PARAMS` with a bounded `data.code`.

Swift adds read-only `AgentAssignment`, `AgentInboxMessage`, counts, and wake
models, one CoreClient load path, and a Manage Dash coordination card. There is
no assign/release/send/ack action in the app; those remain explicit
RPC/CLI/operator actions.

## Authority and cache implications

An assignment or inbox row is coordination metadata, not a security principal.
It does not change role/profile tool policy, approvals, Seatbelt, path jail,
write scope, verifier behavior, merge queue, capacity, budgets, or model
routing. S2 adds no prompt section, so it does not move the prompt prefix or
cache epoch. S3 owns identity prompt sections, S4 owns component/session
workspaces, S5 owns identity memory, and S6 owns hire/retire/promotion.

`lease_token` is internal fencing state, not a credential. A caller cannot use
an assignment to gain a tool, shell, network, MCP, GUI, or provider capability
it did not already have.

## Tests

Covered in S2:

- one active session per identity, partial-index backstop, idempotent same
  assignment, conflicting assignment, cross-project session/identity, role
  mismatch, paused/retired/imported identity rejection, policy-disabled
  rejection;
- explicit release, repeated release, release through the wrong owning session,
  expired lease reclaim, renewal of an expired lease, persisted startup
  reconciliation after reopening the data directory;
- inbox send/list/read/ack idempotency, counts, TTL expiry on read and sweep,
  cross-project recipient/sender rejection, retired recipient rejection, and
  payload size/credential-key rejection;
- Labour/Verifier/Scout cannot be project identities, and therefore cannot
  receive a project inbox row;
- wake continuation only for idle/failed/cancelled sessions, same-assignment
  duplicate wake idempotency, no agent row or capacity slot, and a busy session
  leaves the inbox row unread;
- project replay/repair, manifest rebuild, session bundle export/import,
  protocol decode, CLI parsing/human output, and Swift decode;
- existing identity/session/continuation/gate/effect suites remain green.

## Limits and non-goals

- No distributed lease, cross-host fencing, or cross-host scheduling. The
  invariant is SQLite on one host.
- No automatic assignment, hiring, firing, promotion, transfer, or Manager
  creation. Assignment is explicit.
- No Worker/Labour workspace, component branch, two-level integration, or
  identity prompt in S2.
- No identity memory, performance ledger, or retirement approval workflow.
- No project inbox for Labour, Verifier, or Scout; no task-scoped inbox change.
- No public agent-to-agent federation, remote identity sharing, or
  cross-project identity transfer.
- A wake is a deferred continuation, not a guaranteed run; an operator cancel,
  budget/capacity gate, terminal session state, or scheduler failure can leave
  the message unread.
- Imported coordination rows are archive evidence. There is no rehome command;
  imported active leases expire by their stored timestamp and do not grant a
  local session any authority.
- The JSON payload guard rejects obvious credential key names but cannot prove
  that arbitrary text contains no secret. No secret should be sent at all.

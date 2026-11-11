# ADR-0177: Approval and attention optimizer

- **Status:** Accepted
- **Date:** 2026-11-12
- **Deciders:** VerlinPark runtime maintainers
- **Related:** ADR-0137 (global write leases), ADR-0140 (risk-based approval
  policy), ADR-0151 (question inbox), ADR-0170 (tool error contract),
  `docs/LONG_RUN_OPERATIONS_HARDENING_PLAN.md` H1

## Context

Long sessions produce repeated, nearly identical approval prompts. Before this
ADR the only durable waiver was an exact `ApprovalGrant` (tool plus path prefix),
and every pending request was an independent row. Operators had no batch
decision, no deterministic queue order, no stale policy, and no notification
coalescing. The result is attention fatigue: several agents can block on the
same write while the operator resolves each row separately, and an old low-value
prompt can sit above a destructive one merely because it arrived first.

The approval broker is a security boundary. Optimizing attention must never
turn a missing operator decision into an approval. This ADR therefore separates
two concerns:

1. **Rules** that may allow, deny, or escalate a *known, scoped, low/medium-risk*
   operation.
2. **Attention** that reduces duplicate rows and notification noise without
   pre-approving anything.

## Decision

### 1. Bounded project approval templates

A persisted `ApprovalTemplate` is stored in SQLite
(`approval_templates`), copied into `projects.jsonl`, and restored by
`db-rebuild`:

```json
{
  "id": "uuid",
  "project_id": "project",
  "name": "src writes",
  "enabled": true,
  "tool_pattern": "write_*",
  "risk_levels": ["workspace_write"],
  "path_globs": ["src/**"],
  "action": "allow",
  "expires_at": null,
  "max_uses": 25,
  "used_count": 3,
  "created_by": "operator",
  "created_at": "2026-11-12T00:00:00Z",
  "updated_at": null
}
```

- `tool_pattern` is an exact tool name, an anchored prefix glob (`write_*`), or
  a bounded `*`/`?` glob. `**`, more than four wildcards, fewer than two literal
  characters, control characters, and unsupported characters are rejected.
  A bare `*` cannot match every tool.
- `risk_levels` defaults to `external` and `workspace_write`. `allow` templates
  reject `host_state` and `destructive` at creation *and* at evaluation, even if
  a hand-edited row names them. `deny` and `escalate` templates may name any
  known risk.
- `path_globs` are relative to the session worktree. Absolute paths, `~`,
  drive-prefixed paths, `..`, empty components, and control characters are
  rejected. `*` does not cross `/`; `**` does. Empty globs mean the whole tool,
  not the whole filesystem.
- `action` is `allow`, `deny`, or `escalate`. `escalate` only forces a human; it
  never approves.
- `expires_at` must be in the future and at most 366 days away. `max_uses` is
  1–100,000. `used_count` is preserved across updates, so lowering the cap
  cannot silently reset consumption.
- A project supports at most 100 templates. Reads are bounded at 100 even if a
  hand-edited database contains more.
- One-click templates created from an approval must be path-scoped. Command-only
  approvals (`shell`, `process`, `run_project_task`, `local_http`,
  `code_diagnostics`, `task_control`), remote tools, `mcp`, and `computer` are
  refused by `approval.template.from_approval`; an operator can still create a
  manual tool-wide template deliberately.

Template creation and mutation are audited in `approval_audit` as
`approval.template.created`, `approval.template.updated`, and
`approval.template.deleted`. Every evaluation that actually matches a template
records `approval.template.matched` with the template id, scope, action,
remaining uses, risk, tool, and session. Audit rows are project configuration,
not session state; they are copied into the manifest and restored by
`db-rebuild`.

### 2. Deterministic precedence

Evaluation is a pure function in `vp-core/src/approvals.rs`. It receives the
project templates, exact grants, the risk assessment, approval mode, and an
atomic use-consumption callback:

```text
explicit deny (blocked assessment / sensitive GUI deny policy)
  > template deny
  > exact ApprovalGrant
  > template allow / escalate
  > risk policy (smart, strict, unattended)
```

- **Explicit deny wins.** A blocked assessment never reaches a human, a
  template, or a grant.
- **Deny always wins.** A matching deny template beats an exact grant and a
  matching allow template. Multiple matching templates are ordered by
  specificity (exact tool before prefix before bounded glob, then more specific
  path globs, then id); the first live template that can atomically consume a
  use decides.
- **High risk is never auto-approved.** `Destructive` and `HostState` cannot be
  covered by an `allow` template. Remote tools (`remote_*`) are refused by allow
  templates. Sensitive GUI targets are classified `Destructive` or blocked by
  project policy, so they are refused as well. `mcp` and `computer` calls at
  `HostState` or above are refused.
- **Unknown risk fails closed.** Unknown template risk names do not match; an
  unknown template action is skipped. A malformed or hand-edited template is
  inert rather than treated as an exact tool.
- **Exact grants remain local.** A grant is considered only for risk at or
  below `External` and never for `remote_*`. Grants still match only path-like
  input prefixes.
- **Internal auto-approval does not widen rules.** An explicitly authorized
  internal run still honors deny templates and the risk policy, but allow and
  escalate templates do not turn its `Ask` into `Allow`.
- **Risk policy is the fallback.** `smart`, `strict`, and `unattended` retain
  their existing semantics. `auto_approve` still maps `Ask` to `Deny`.

Use consumption is atomic in SQLite:

```sql
UPDATE approval_templates
SET used_count = used_count + 1, updated_at = ?
WHERE id = ?
  AND enabled = 1
  AND (expires_at IS NULL OR expires_at > ?)
  AND (max_uses IS NULL OR used_count < max_uses)
```

A zero-row update means inert. The evaluator then tries the next candidate; it
never assumes success. Expired, disabled, zero-cap, or exhausted templates are
inert and are reported as such by the template list (`enabled`, `remaining
uses`) and by the absence of a match audit.

### 3. Deduplication, not auto-approval

An approval request carries a deterministic identity:

```text
SHA-256(session_id | agent identity | tool | canonical input scope hash | risk)
```

The canonical input scope is a recursively key-sorted JSON serialization hashed
with SHA-256. Command text and file contents are never copied into the hash
output, event field, or queue. Inputs are still stored verbatim on the approval
row for review.

When an identical request is already `pending` in the same session within
`coalesce_window_secs`, the store:

1. increments `waiter_count` on the existing row,
2. emits `approval.coalesced` with the new tool-call id and waiter count,
3. returns the same approval id and a fresh waiter receiver.

The engine keeps a bounded waiter list per approval id. One operator decision
resolves every current waiter. A coalesced request after resolution is a new
approval row; dedup never becomes a grant and never auto-approves. A request
that arrives while a resolution is being committed is checked once against the
stored status and receives the terminal decision immediately instead of
waiting for the next timeout.

Dedup is per session and per agent identity. An approval from Worker A does not
cover Worker B, a different tool, a different risk, or a different canonical
input.

### 4. Attention policy

`AttentionPolicy` is project-scoped, stored in
`project_attention_policies`, copied into the manifest, and restored by
`db-rebuild`:

```json
{
  "enabled": true,
  "max_pending_per_session": 64,
  "coalesce_window_secs": 120,
  "stale_after_secs": 86400,
  "priority_boost_risk": "host_state",
  "notify_coalesce_secs": 60
}
```

- Missing policy rows decode as the bounded default above, so attention
  optimization is on for every project. The values are clamped on read and
  write: 1–500 pending, 0–3,600s coalescing, 60s–7d stale, 0–3,600s
  notification window, and a known risk name.
- Pending approvals are ordered deterministically:
  1. risk descending (`destructive` > `host_state` > `external` >
     `workspace_write` > `read_only` > unknown),
  2. blocked-task criticality descending (the plan task's priority plus a
     running/ready/blocked bonus; 0 when no task is attached),
  3. age ascending,
  4. id ascending.
- When a new row would exceed `max_pending_per_session`, the oldest
  lowest-risk pending rows are expired first. Expiry denies their waiters; it
  never approves them.
- Stale rows are marked `expired` with an `approval.expired` event. The daemon
  sweep runs every `VERLINPARK_APPROVAL_SWEEP_SECS` (default 30, 0 disables).
  A risk at or above `priority_boost_risk` receives a four-times longer stale
  window, but it still expires; the boost never approves anything. Expiry is
  computed from the approval's stored `expires_at` when present and from the
  policy otherwise.

### 5. Notification coalescing

Each new approval stores a notification decision and a stable
`session|tool|risk` key. If a notifiable approval with the same key was created
within `notify_coalesce_secs`, the new row carries `notify=false` and emits no
inbox item. Every approval row remains in the queue and remains individually
reviewable; only the notification is suppressed. A risk at or above
`priority_boost_risk` bypasses coalescing so high-risk prompts are never hidden
by a low-risk burst. `notify_coalesce_secs=0` disables notification coalescing
without disabling deduplication.

### 6. Batch resolution

`approval.batch.resolve` accepts an explicit `decisions` list of
`{approval_id, approved}` pairs. There is no "approve all pending" shorthand and
no wildcard id. The engine rejects empty batches, more than 100 decisions, and
duplicate ids. The store applies each item independently and reports:

```json
{
  "results": [
    {"approval_id": "a1", "resolved": true, "status": "approved", "waiter_count": 2},
    {"approval_id": "a2", "resolved": false, "status": "approved",
     "code": "APPROVAL_NOT_PENDING", "message": "..."}
  ],
  "resolved_count": 1,
  "failed_count": 1
}
```

Missing rows report `APPROVAL_NOT_FOUND`; already terminal rows report
`APPROVAL_NOT_PENDING` with the current status. A partial failure never rolls
back successful decisions and never guesses an id.

### 7. RPC, CLI, and Swift surfaces

- RPC: `approval.queue`, `approval.batch.resolve`, `approval.templates`,
  `approval.template.create`, `approval.template.update`,
  `approval.template.delete`, `approval.template.from_approval`,
  `approval.policy`, `approval.policy.set`, `approval.audit`.
- Existing `approval.list` and `approval.resolve` remain wire-compatible.
- CLI: `vp approvals`, `vp approval-batch --approve ID --deny ID`,
  `vp approval-template`, `vp approval-template-set`,
  `vp approval-template-delete`, `vp approval-policy`,
  `vp approval-policy-set`, `vp approval-audit`.
- Swift Manage Dash: multi-select per row with per-item approve/deny, select
  all/clear, a "Template" action that creates a bounded template from one
  reviewed approval, and a project attention-policy/template editor. The
  approval inbox is backed by `approval.queue`, so it receives the daemon's
  deterministic order. `approval.created`/`approval.requested` are handled
  idempotently, `approval.coalesced` refreshes waiter counts, and
  `approval.expired` removes the row. Notification items respect the event's
  `notify` flag; older events without the flag remain notifiable.

### 8. Events, replay, repair, and bundles

Session events:

- `approval.created` (new canonical event; payload is the full `Approval`)
- `approval.requested` (legacy event retained with the same payload for older
  clients and replay snapshots)
- `approval.coalesced`
- `approval.resolved`
- `approval.expired`

Project audit events:

- `approval.template.matched`
- `approval.template.created`
- `approval.template.updated`
- `approval.template.deleted`
- `approval.policy.updated`

`session.replay` reconstructs approval status, waiter count, agent identity,
scope/dedup hashes, priority, expiry, notification fields, and template id from
the session events. `session.replay {repair:true}` rewrites all approval
columns, including the H1 fields. Session bundles carry the full `Approval`
rows (older bundles decode with defaults), and import inserts every column.
Project audit rows are copied into `projects.jsonl` and restored by
`db-rebuild`; they are not session-scoped archive content.

### 9. Error contract and limits

Approval rule errors use the Q3 `{code, message, retryable, hint}` shape without
growing the static tool catalog:

| Code | Meaning |
| --- | --- |
| `APPROVAL_NOT_FOUND` | The id no longer exists. |
| `APPROVAL_NOT_PENDING` | The row already has a terminal status. |
| `APPROVAL_TEMPLATE_INVALID` | Invalid pattern, glob, risk, name, expiry, or cap. |
| `APPROVAL_TEMPLATE_EXHAUSTED` | The template is inert because its use cap is reached. |
| `APPROVAL_TEMPLATE_EXPIRED` | The template is inert because its expiry passed. |
| `APPROVAL_TEMPLATE_FORBIDDEN_RISK` | An allow template named a high risk. |
| `APPROVAL_POLICY_INVALID` | Attention values could not be normalized. |

Hard bounds: 100 templates/project, 16 path globs/template, 120 tool-pattern
characters, 300 path-glob characters, 4 tool wildcards, 100 batch decisions,
5,000 candidates per stale sweep, 100,000 audit rows per manifest snapshot.
Approval rows and notification keys are bounded by the input/path extraction
caps; full tool inputs remain the existing bounded stored payload.

## Consequences

- Repeated identical low/medium-risk prompts become one row and one decision
  while preserving a waiter count.
- Operators can clear a backlog with one explicit batch action, but every id is
  still named and every failure is reported.
- The approval queue orders destructive/host-state and blocking-task work first
  and stops stale low-value rows from lingering forever.
- Notification noise drops without deleting any approval row.
- Security gates are unchanged or stricter: templates are narrower than a
  permission system, deny wins, high/unknown risk fails closed, and stale rows
  deny rather than approve.
- Evaluation performs a bounded SQLite read (templates and grants) and, on a
  match, one conditional write. It runs synchronously on the tool path; this is
  intentional to keep the recursive agent future small. It is a local write and
  is covered by the existing daemon admission, budget, sandbox, Verifier, merge,
  and write-lease gates.

## Non-goals

- No automatic approval of `Destructive`, `HostState`, `remote_*`, MCP tool
  execution, or sensitive GUI targets.
- No cross-host or distributed approval queue; waiters and leases remain
  same-daemon memory plus SQLite.
- No command-text template matching. Templates scope tools and paths only;
  shell commands still pass through risk classification.
- No implicit approval of an unknown tool, unknown risk, or missing path.
- No silent template edits: full replacement is audited and preserves
  consumption.
- No blanket batch approval, no "remember this decision" that bypasses explicit
  templates, and no model-generated approval rules.
- No claim that notification coalescing is durable across clients; it is a
  daemon-side decision carried on the event. The approval queue remains the
  source of truth.

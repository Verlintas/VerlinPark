# ADR-0212: Portable team templates and team charter

- **Status:** Accepted
- **Date:** 2026-11-13
- **Phase:** S8, final module of the project-scoped agent identity program
- **Depends on:** ADR-0205 (identity schema/lifecycle), ADR-0206 (assignments
  and inbox), ADR-0207 (identity prompts and role contracts), ADR-0208 (Worker
  workspaces and two-level integration), ADR-0209 (identity memory), ADR-0210
  (hiring, retirement, and performance), ADR-0211 (org chart and identity
  console), ADR-0193 (task contracts), ADR-0198 (failure attribution)
- **Does not implement:** cross-project identity sharing, automatic hiring,
  identity rehoming, memory/inbox/worktree migration, a template marketplace,
  an LLM recruiter, C7+, or any remaining S-phase.

## Context

S1 through S7 made the project org durable and observable: Leader, Managers,
Workers, charters, assignments, workspaces, memory, inbox, performance, and an
org-chart console. A successful project still had no portable artifact for the
part of that org that should travel: **who does what, under which rules, with
which role references**.

Copying an identity database would leak context and violate project isolation.
Copying only a prose charter would lose the validated structure. S8 therefore
defines a bounded, content-addressed **team charter**:

- `team.json` is the machine-readable template;
- `TEAM.md` is a deterministic human rendering of the same value;
- import is a preview plus an explicit, approval-classified apply;
- apply creates at most pending S6 hire requests, never identities.

## Decision

### 1. Canonical schema

`TeamTemplate` is versioned by `schema_version` (currently `1`). Unknown fields
are rejected with `serde(deny_unknown_fields)` so a caller cannot smuggle local
context into a future-compatible template.

```text
TeamTemplate {
  schema_version, template_id, name, version, description, content_hash,
  leader: TeamRoleSlot,
  departments: [{ name, objective, manager: TeamRoleSlot,
                  workers: [TeamRoleSlot] }],
  labour_defaults: { profile, model_role?, bundle, max_concurrency },
  rules: { delegation, review, acceptance, escalation, retirement,
           handoff, communication },
  budgets: { defaults: TeamBudget, caps: TeamBudget },
  model_bindings: { role-or-slot -> explicit model reference },
  tool_policy: { role -> { bundle?, allow[], deny[] } },
  skills: [{ name, version_hash }],
  conventions: { coding, commit, test, review_checklist[] },
  provenance: { source_project? }
}

TeamRoleSlot {
  title, role, department?, component?, profile, model_role?,
  charter, decision_rights[], review_duties[], default_budget
}
```

`TeamBudget` contains optional `token_budget` and `cost_budget_usd`.
`RoleSlot.model_role` is a label into `model_bindings`; neither field is a
route. `tool_policy` references named static bundles and static tool names, not
MCP schemas. `skills` references name plus the source skill's SHA-256.

The template deliberately has **no** identity id, project id, session id,
message, memory, inbox, worktree, branch, commit, tool output, performance
evidence, MCP pin/schema, provider key, absolute path, repository URL, or
secret field. Free-text fields are checked for path, URL, credential, UUID,
and known session/agent/worktree marker patterns. Export runs the same check,
so a project whose own name or title contains such a value fails closed rather
than producing a non-portable artifact.

### 2. Canonical hash and id

The canonical hash is SHA-256 over compact JSON with `content_hash` empty.
Field order is the Rust declaration order and all maps are `BTreeMap`s.
`template_id` is derived deterministically from the body when an export
chooses it (`tt_<first 16 hex>`), then folded into the content hash. The same
project state exports byte-identical `team.json` and the same hash; whitespace
in a pretty-printed file cannot change identity. Import recomputes the hash and
fails closed on mismatch; it never silently repairs a supplied template.

### 3. Export

`team.template.export` / `vp team-export <project> --name ... --out <dir>`
derives:

- the live Leader as a charter overlay (no identity id);
- departments from live Managers and Workers, with deterministic ordering;
- canonical role charters, decision rights, and review duties;
- project budget as **suggestions** in `budgets.defaults` and `budgets.caps`;
- explicit model references from identity metadata, deduplicated by role and
  given deterministic binding keys;
- static role/profile bundle references from the tool-bundle matrix;
- enabled project skills by name and content hash;
- canonical coding/commit/test/review conventions;
- provenance as the human project name only.

Export registers a library record and emits `team.template.exported` with
ids/name/version/hash/counts only. It creates no link, hire request, identity,
session, assignment, workspace, memory, inbox row, or policy change. The CLI
writes `team.json` and `TEAM.md`; it is not a backup.

### 4. Preview and validation

`team.template.preview` / `vp team-import --dry-run` is read-only. It parses
and validates:

- schema version, content hash, size, bounds, and unknown fields;
- role names, profile names, department/worker uniqueness;
- labour concurrency (`1–40`) and static bundle names;
- model binding shape and explicit references against daemon-configured role
  models and model presets (never a client-supplied list);
- tool bundle and allow/deny names against the static tool catalog;
- budgets: finite/non-negative, hard bounds, defaults not above caps;
- portable-text leakage checks.

Target-specific gaps are reported by preview and block apply:

- a model binding not configured in the target;
- a tool/bundle name not present in the static catalog (shape errors are
  rejected even earlier);
- a template budget or cap that would widen the target project budget;
- identity conflicts, a missing live Leader, or a blocked parent slot.

Preview shows the proposed org chart, new/renamed/existing/blocked slots,
conflicts, budget changes, model/tool gaps, policy conflicts, and warnings.
It writes no template record, link, hire request, identity, lease, session, or
project policy.

### 5. Apply and hiring

`team.template.import` with `apply: true` requires `approved: true`; there is no
auto-apply policy. Apply:

1. stores (or verifies) the exact content-addressed template row;
2. creates one `project_team_templates` link;
3. creates **pending** `agent_hire_requests` and `agent.hire.requested`
   employment events for missing Manager/Worker slots only;
4. emits `team.template.applied` with ids/name/version/hash/counts.

It never creates an identity, approves a hire, assigns a session, creates a
workspace, sends inbox mail, writes memory, or changes a policy. A pending
request still goes through S6 caps, hierarchy, approval, and identity-policy
validation when approved.

Existing identities are never overwritten:

- a live Leader is an overlay only; the charter never renames, pauses, retires,
  or re-parents it;
- an exact matching live identity is `existing`;
- a same department/component owner with a different title is a conflict;
- a conflict can be resolved by an explicit `skip`, `merge` (bind the slot to a
  live identity without writing it), or `rename` (change the proposed title).
  `retire` is not an apply action; the operator retires through S6 and
  re-applies. An unresolved conflict fails apply closed.

Apply is idempotent for the same content hash. Re-applying an already-linked
template creates no duplicate hire requests; after a pending Manager is
approved, a later re-apply can create the formerly blocked Worker request.
Applying is always scoped to the target project and never rehomes identities
from the source project.

### 6. Policy precedence and provenance

The target project always wins:

- the template cannot widen token/USD budgets, approvals, sandbox/Seatbelt,
  path jail, MCP trust/pins, Verifier/Integrator gates, capacity, or identity
  caps;
- model bindings remain explicit references; there is no hidden routing;
- tool-policy references cannot authorize a tool, bypass role/profile policy,
  or approve an MCP server;
- acceptance commands and review conventions are suggestions until the target
  project confirms them;
- applying a template stores no project policy and changes no policy row.

New pending hire requests carry a bounded `policy.team_template` object with
template id, version, content hash, and slot. The template record and apply
link keep the same provenance for audit. New identities therefore inherit only
fresh project memory/inbox/performance and the normal S6 policy, not source
context.

### 7. Storage, replay, manifest, and bundle

SQLite adds:

```sql
team_templates(
  id, name, version, schema_version, content_hash, json, created_at,
  source_project_id
);
project_team_templates(
  project_id, template_id, applied_at, applied_by,
  PRIMARY KEY (project_id, template_id)
);
team_template_events(
  id, project_id, seq, kind, template_id, name, version, content_hash,
  payload, created_at, UNIQUE (project_id, seq)
);
```

`source_project_id` is local storage metadata, not part of the portable body.
The audit payload allowlist is template id/name/version/hash, slot count,
pending-hire count, source project label, and applied-by. `team.template.replay`
checks contiguous sequence, bounded payloads, template/link consistency, and
can repair a missing link from an existing applied event only. It never
fabricates a template body or identity.

The project manifest and `db-rebuild` carry template records, apply links, and
events. A session bundle carries the same relevant records; archive import
treats them as evidence and never creates hires or identities. Imported
template records without a link are read-only until explicit apply.

### 8. Protocol, CLI, and UI

JSON-RPC v1 adds `serde(default)` fields and methods:

- `team.template.export`
- `team.template.preview`
- `team.template.import`
- `team.template.list`
- `team.template.get`
- `team.template.diff`
- `team.template.replay`

CLI adds `vp team-export`, `vp team-import`, `vp team-templates`,
`vp team-diff`, `vp team-template`, and `vp team-replay`. `--dry-run` previews,
`--apply` requires `--approved`, and `--resolution-json` stages explicit
skip/merge/rename decisions.

Manage Dash adds a Team Templates library card with the applied/read-only
state, a diff sheet, a preview sheet with proposed org/conflicts/budget/model/
tool/policy sections, skip/merge conflict actions, and a separate approval
toggle before apply. Every action calls a real RPC; the UI never synthesizes a
success or a template body. All new protocol fields are additive and
`serde(default)`.

## Consequences and limits

- A template can carry only what the schema models. It is not a backup and
  cannot move memory, inbox, worktrees, branches, performance evidence, MCP
  pins, credentials, or source context.
- Model validation is against the daemon's configured role models and model
  presets at import time. A model that is configured later must be re-applied.
- Tool allow/deny validation is against the static tool catalog. MCP tools are
  not portable and remain project-local trust decisions.
- Apply creates pending hire requests, not identities. Operators still approve
  each S6 hire; a project with no live Leader cannot import a Manager/Worker
  org until the Leader is bootstrapped explicitly.
- `merge` and `skip` are explicit conflict resolutions. `rename` changes the
  proposed slot title; it does not rename an existing identity. Retirement is
  handled by the S6 RPC and a later re-apply.
- The local library is same-host SQLite. There is no cross-host template
  registry, signature, marketplace, or remote attestation. The content hash is
  integrity evidence, not an author signature.
- Bundle/manifest replay restores template metadata and links; it does not
  rerun a hire, create an identity, or reproduce the source project's
  environment.
- The GUI is model/state tested and manually smoked on macOS; there is no
  automated UI test suite.

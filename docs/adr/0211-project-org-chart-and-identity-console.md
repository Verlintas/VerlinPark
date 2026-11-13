# ADR-0211: Project org chart and identity console

- **Status:** Accepted
- **Date:** 2026-11-13
- **Phase:** S7 of the project-scoped agent identity program
- **Depends on:** ADR-0205 (identity schema/lifecycle), ADR-0206 (assignments and
  inbox), ADR-0207 (identity prompts/role contracts), ADR-0208 (Worker
  workspaces), ADR-0209 (identity memory), ADR-0210 (hiring, retirement, and
  performance), ADR-0197 (OTel GenAI projection), ADR-0198 (failure
  attribution)
- **Does not implement:** S8 portable team templates, cross-project identity
  sharing, automatic hiring/firing/promotion/transfer, an LLM recruiter or
  performance judge, remote identity federation, or native GUI sandboxing.

## Context

S1 through S6 made project identities durable, assignable, prompted,
workspace-bound, memory-bearing, and employment-accountable. The evidence was
available through many separate RPCs, but an operator still had to reconstruct
the organization from identity rows, assignment rows, workspace rows, inbox
metadata, memory rows, a derived performance ledger, and pending employment
decisions.

The missing surface was not another store. It was a bounded read projection and
a console:

- who is the Leader, which Managers own which departments, and which Workers
  report to them;
- who is active, idle, paused, retired, or imported;
- who is assigned to which session and workspace right now;
- what each identity has actually done, with evidence;
- which hire requests and retirement proposals are waiting for an operator;
- how to hire, promote, transfer, and retire without inventing an untracked
  client-side write path.

S7 adds that projection and console. It deliberately keeps every existing gate:
identity policy, project caps, hierarchy validation, approval classification,
write scope, Seatbelt, Verifier, merge queue, capacity, and budgets.

## Decision

### 1. The org chart is a read projection, not a second org store

`agent.org.snapshot` is a project-scoped, bounded projection over existing S1–S6
rows:

- identities and lifecycle status;
- active session assignments with lease metadata (never the lease token);
- Worker component workspaces with branch/status/lease metadata (never an
  absolute worktree path);
- unread inbox counts and active agent-memory counts;
- the compact performance summary;
- advisory retirement candidate state and pending proposals;
- pending hire requests;
- project identity and retirement policies.

It does not create, mutate, repair, assign, hire, retire, or call a model.
`db-rebuild` and bundle import are unchanged because the source of truth is the
existing identity/coordination/workspace/memory/employment projections.

The tree is assembled deterministically: exactly one live Leader is the root,
Managers attach to the Leader, Workers attach to their Manager, and any row with
no valid parent remains visible in `unassigned` with a warning. A hidden or
retired parent never silently deletes a child from the chart.

`agent.identity.detail` is the bounded single-identity view. It returns the
identity, the same org node, recent assignments, inbox metadata, visible memory
rows, the derived performance ledger, retirement candidates and proposals, hire
requests, workspaces, parent/successor/direct reports, and recent employment
events.

All new protocol fields use `serde(default)`; older clients decode a reduced
snapshot and newer clients fail open only on additive, bounded fields.

### 2. Bounds and privacy are explicit

| Surface | Default | Hard cap |
|---|---:|---:|
| Org identity nodes | 200 | 200 |
| Department lanes | — | 64 |
| Active assignments in the snapshot | — | 200 |
| Workspace summaries | — | 200 |
| Detail inbox rows | 20 | 100 |
| Detail memory rows | 20 | 100 |
| Detail assignments | 50 | 200 |
| Detail proposals | 20 | 50 |
| Detail hire requests | 20 | 50 |
| Detail employment events | 20 | 50 |

Truncation is reported (`truncated` plus a warning); it is never hidden. The org
surface returns text-free telemetry: ids, roles, titles, departments,
components, statuses, generations, branch names, counts, durations, timestamps,
and USD totals.

Inbox content is metadata by default. `include_inbox_content` opts into a short
known-field preview (`summary`, `title`, `body`, `message`, `question`, or
`decision`) capped at 200 characters. Raw payload objects, tool arguments, tool
output, prompts, file content, absolute worktree paths, lease tokens, and
secrets never enter the org or detail payload.

### 3. Mutations stay S6 RPCs with S6 approvals

The console does not add mutation RPCs. It calls the existing explicit
operations:

- `agent.hire`, `agent.hire.approve`, `agent.hire.reject`;
- `agent.promote`, `agent.transfer`;
- `agent.retire`;
- `agent.retirement.propose`, `.approve`, `.reject`.

The UI never synthesizes `approved: true`. Hire defaults to a pending request
when policy requires approval. Promotion and transfer are inherently
approval-classified and always submit an explicit operator decision, but the
daemon still revalidates caps, hierarchy, live status, and active work.
Retirement defaults to requiring an explicit approval toggle (or an approved
proposal) and is labeled terminal/irreversible; a pending proposal does not
satisfy the gate.

`force` is presented as "release active work", not as "stop the agent". It
releases assignments and archives workspaces through their normal events. It
does not interrupt an in-flight provider stream and never deletes evidence.

Imported identities, imported hire requests, and imported proposals are
read-only. The console disables their actions and the daemon still fails closed
if a client attempts them. Retired identities remain visible as evidence when
`include_retired` is set; a dedicated action is unavailable because retirement
is terminal.

### 4. Manage Dash owns the operator surface

Manage Dash adds a real "Project org" section:

- a Leader card, department/Manager columns, and component Worker cards;
- status, generation, active-session, workspace, inbox/memory, performance, and
  advisory-retirement badges;
- department lanes, a cross-session active-assignment roster, workspace status,
  pending hire requests, and pending retirement proposals;
- empty, disabled-policy, no-Leader, imported, and truncated states.

Selecting an identity opens a seven-tab detail sheet: Overview, Assignments,
Inbox, Memory, Performance, Workspace, and Lifecycle. Every list is bounded and
read-only except the explicit lifecycle actions.

Hire, Promote, Transfer, and Retire are dedicated sheets. They show the policy
state and the exact approval semantics before submitting. All UI state lives in
`AppModel`; views stay declarative. Errors preserve the daemon's typed Q3 code
and hint instead of flattening every failure to a generic string.

The console uses the existing `VPTheme`/`VPMotion` system, honors Reduce Motion
through the shared motion helpers, and provides accessibility labels for cards,
status dots, counts, and action groups.

There is no background polling loop. The section loads on appear, on explicit
refresh, and after an operator mutation that already happened in the same
client. Existing dashboard/session event refresh patterns remain authoritative.

### 5. CLI and JSON

`vp agent-org <project> [--include-retired] [--limit n]` prints the bounded
chart and counts. `vp agent-detail <identity> [--project <id>]
[--include-inbox] [--inbox-limit n] [--memory-limit n] [--assignment-limit n]`
prints one bounded detail. Both support the global `--json` flag and use the
same RPCs as the app.

The existing `vp agent-hire`, `vp agent-hires`, `vp agent-hire-approve`,
`vp agent-hire-reject`, `vp agent-promote`, `vp agent-transfer`,
`vp agent-retire`, `vp agent-performance`, `vp agent-retirement`,
`vp agent-memory`, `vp inbox`, and `vp workspaces` commands remain the
authoritative operator/automation paths. S7 does not add a second write surface.

### 6. Observability stays metadata-only

The OTel GenAI projection now copies `identity_id` and `identity_generation`
onto agent, tool, and usage spans when the source event already carries them.
It still never exports prompts, goals, tool arguments, tool results, file
content, raw errors, or secrets. Event projections and Manage Dash expose only
bounded org/identity counts; no new store or event stream is introduced.

## Invariants preserved

- The daemon remains the owner of state; the SwiftUI app is a read client plus
  explicit RPC caller.
- Identity never bypasses role/profile policy, approvals, Seatbelt, path jail,
  write scope, Verifier, merge queue, capacity, or budget.
- The org/detail projections never mutate employment, assignment, workspace,
  memory, performance, or plan state.
- No automatic hiring, firing, promotion, transfer, rehire, or Manager creation.
- No hidden model routing; the identity model field remains metadata.
- No secrets, prompt text, tool content, absolute worktree paths, or lease
  tokens enter the console payloads or OTel spans.
- A project without an identity policy shows the disabled state rather than
  implying the feature is active.
- Bounds are explicit and truncation is reported.

## Limits and non-goals

- The projection is assembled on demand from existing SQLite rows. It is not a
  materialized org-chart store and does not add an org event stream.
- The performance and retirement sections are the deterministic S6 ledger and
  C6 attribution evidence. They are descriptive, not a ranking, verdict, gate,
  or LLM judgment.
- The org chart displays at most 200 identity nodes and 64 department lanes. A
  pathological imported archive remains visible through CLI JSON but is
  truncated in the console.
- The console does not create the Leader. Leader bootstrap remains
  `vp agent-create --role leader` (or the equivalent RPC).
- The console does not attach, land, archive, or retire workspaces. Those remain
  explicit S4 operator/CLI actions.
- The console does not create sessions or assignments. Assignment lease
  renewal/release remains an explicit S2 action.
- A forced employment change is cooperative. It does not kill a provider
  stream, guarantee a remote side stopped, or reconstruct remote state.
- Imported identities remain read-only evidence; rehome is a future explicit
  phase.
- Successor recording does not transfer memory or component-branch ownership;
  that remains a separate explicit operation.
- The console is same-host SQLite state, not distributed employment control,
  cross-host identity federation, or an external HR integration.
- S8 team templates, automatic org generation, a graphical org editor,
  compensation/interview workflows, and cross-project identity sharing are not
  implemented.
- The SwiftUI surfaces are compiled and unit-tested for model/state behavior;
  GUI interaction is manual-smoke tested on macOS, not covered by automated UI
  tests.

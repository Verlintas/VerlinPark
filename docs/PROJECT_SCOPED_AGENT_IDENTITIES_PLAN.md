# Project-Scoped Agent Identities Plan: stable班组 + 临时 Labour

> **Status:** active planning; not implemented.
> **Created:** 2026-11-12
> **Base:** `7f805e7` (Contracted Execution plan stored).
> **Relationship:** companion program to
> [CONTRACTED_EXECUTION_PLAN.md](CONTRACTED_EXECUTION_PLAN.md). Contract/effects
> (C1–C5) is the substrate; this plan makes Leader/Manager/Worker identities
> project-scoped and durable, while Labour remains task-scoped.
> **Goal:** a long-lived project gets a stable org chart — Leader, Managers,
> Workers — with durable memory, inbox, workspaces, accountability, hiring,
> and retirement — without turning agents into unkillable processes or
> weakening approvals, Seatbelt, path jail, write scope, Verifier, merge
> queue, capacity, or budgets.

---

## 0. Why project-scoped

The current runtime is **run-scoped**: every Leader/Manager/Worker/Labour run
gets an `agents` row that is session-scoped and `ON DELETE CASCADE`; the
worktree is task-scoped; the mailbox is session-scoped; there is no durable
identity, generation, retirement, project roster, or cross-session memory.
The stored TeamPlan is the only continuity.

That is fine for one-off tasks, but wrong for a long-lived codebase:

- a Manager should own a department across many sessions, not be re-invented
  each time;
- a Worker should own a component, its architecture, interfaces, and review
  standards across tasks and sessions;
- a Worker should decompose low-level work and direct Labour, while Labour is
  the main implementation workforce;
- hiring, promotion, transfer, and retirement should be explicit project
  events, not accidental side effects of a new run;
- project memory and decisions should belong to the project org, not to a
  single session that may be archived.

Labour stays task-scoped by design: it is the "AI hired by the base employee"
to do one implementation task. It does not need a project identity.

This plan is deliberately split into phases. No phase requires a big-bang
rewrite of sessions, worktrees, leases, or the event log.

---

## 1. Target model

| Tier | Identity scope | Execution | Owns | Primary job |
|---|---|---|---|---|
| Leader | project | per run | project goal, portfolio, budget, final plan approval | direction, prioritization, cross-session decisions |
| Manager | project, department | per run | department architecture, interface contracts, Worker review | decompose department goals, coordinate Workers, hire/retire proposals |
| Worker | project, component/sub-team | per run | component workspace/branch, Labour tasks | low-level design, decomposition, code review, integration |
| Labour | task | per task | child task worktree | bulk implementation, tests, commit |
| Verifier | session/task | per verification | no identity | independent gate |
| Scout | session/task | per run | no identity | read-only exploration |

**Stable identity ≠ persistent process.** What persists is the employee record:
role, department, component, profile, model generation, memory, inbox,
performance evidence, assignments, and lifecycle. Execution remains a
short-lived run under the existing capacity, budget, approval, and sandbox
gates.

Project scope means the identity belongs to `project_id`, not `session_id`.
A session **assigns** identities from the project roster. An identity can be
assigned to one active session at a time in the first version; later versions
may support disjoint multi-session portfolios.

---

## 2. Architecture

### 2.1 Identity vs run

```sql
agent_identities(
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  role TEXT NOT NULL,                 -- leader | manager | worker
  department TEXT,
  component TEXT,
  title TEXT,
  profile TEXT,
  model TEXT,
  parent_identity_id TEXT,
  status TEXT NOT NULL,               -- active | idle | paused | retired
  generation INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL,
  retired_at TEXT,
  successor_id TEXT,
  policy_json TEXT NOT NULL DEFAULT '{}'
);

agent_runs(
  id TEXT PRIMARY KEY,
  identity_id TEXT NOT NULL REFERENCES agent_identities(id),
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  task_id TEXT,
  role TEXT NOT NULL,
  status TEXT NOT NULL,
  worktree TEXT,
  branch TEXT,
  started_at TEXT NOT NULL,
  ended_at TEXT
);
```

The existing `agents` table remains the run/roster projection during
migration. New code writes `agent_runs` and keeps `agents` as a compatibility
view/backfill. A project policy `AgentIdentityPolicy` (default off) enables
project identities for new sessions.

### 2.2 Project roster and session assignment

```sql
project_agent_policy(
  project_id TEXT PRIMARY KEY,
  enabled INTEGER NOT NULL DEFAULT 0,
  max_managers INTEGER NOT NULL DEFAULT 4,
  max_workers INTEGER NOT NULL DEFAULT 10,
  hire_requires_approval INTEGER NOT NULL DEFAULT 1,
  retirement_requires_approval INTEGER NOT NULL DEFAULT 1
);

session_agent_assignments(
  session_id TEXT NOT NULL,
  identity_id TEXT NOT NULL,
  role TEXT NOT NULL,
  status TEXT NOT NULL,               -- active | released | queued
  lease_token TEXT,
  assigned_at TEXT NOT NULL,
  released_at TEXT,
  PRIMARY KEY (session_id, identity_id)
);
```

- A project has one Leader identity by default. A session assigns the Leader;
  only one active Leader assignment per identity in the first version.
- Managers and Workers are hired into departments/components and assigned to
  sessions by the Leader/Manager or operator.
- A Worker assignment carries an identity lease so two sessions cannot drive
  the same Worker workspace concurrently.
- Labour is not in the project roster; it is created per task under a Worker
  assignment and a session.

### 2.3 Worker workspace and two-level integration

Recommended first model:

```text
project repo
  └── component branch (Worker identity, long-lived)
        └── session worker worktree (checked out for one session)
              └── Labour task worktree (per task)
```

- The Worker identity owns a project-level **component branch** (for example
  `verlinpark/components/auth`).
- A session assignment creates a **session worker worktree** from that branch.
- Labour gets a child task worktree from the session worker worktree.
- Labour commits → Worker reviews → deterministic merge into the session
  worker worktree (diff check, scoped paths).
- The session worker worktree → Verifier → deterministic Integrator → session
  branch, exactly as today.
- When the session ends, the session worker worktree's commits are merged back
  into the component branch through the same verification path (or an explicit
  operator `workspace.land`).
- The identity lease prevents two sessions from writing the same component
  workspace concurrently. Different Workers on disjoint components may run in
  parallel under the existing write-lease registry.

Alternative (rejected for v1): one long-lived worktree shared directly by
multiple sessions. It creates branch/conflict coupling and complicates crash
recovery. Revisit only if component-level parallelism demands it.

### 2.4 Agent inbox

```sql
agent_inbox(
  id TEXT PRIMARY KEY,
  identity_id TEXT NOT NULL REFERENCES agent_identities(id),
  session_id TEXT,
  from_identity_id TEXT,
  kind TEXT NOT NULL,                 -- task | message | question | handoff | decision | review
  payload_json TEXT NOT NULL,
  status TEXT NOT NULL,               -- unread | read | done | expired
  created_at TEXT NOT NULL,
  read_at TEXT,
  expires_at TEXT
);
```

- Leader/Manager/Worker receive project-scoped messages, task assignments,
  questions, handoffs, and review requests.
- Labour uses a task-scoped inbox only.
- Delivery remains pull-based; a new assignment can also wake a session via the
  existing continuation path.
- Cross-session notes/handoffs are durable project state, not transient
  session mailbox rows.

### 2.5 Identity memory

```sql
agent_memories(
  id TEXT PRIMARY KEY,
  identity_id TEXT REFERENCES agent_identities(id),
  project_id TEXT NOT NULL,
  department TEXT,
  scope TEXT NOT NULL,                -- agent | department | project
  content TEXT NOT NULL,
  evidence_json TEXT NOT NULL,
  status TEXT NOT NULL,               -- active | superseded | stale
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  expires_at TEXT
);
```

- Agent memory: component map, design decisions, interfaces, style, open
  questions, review standards.
- Department/project memory: shared contracts, conventions, decisions.
- Only a **bounded memory brief** enters the prompt; full content is retrieved
  with `recall`/`search_sources`.
- Evidence-backed and event-backed; no unverified model prose becomes
  governance.
- Memory updates are explicit `remember`/`memory.update` events with
  provenance.

### 2.6 Prompt and cache

New instruction sections:

| Ordinal | Key | Scope | Volatility |
|---:|---|---|---|
| 15 | `agent/identity` | project identity charter, role, department, component, stable preferences | stable for identity generation |
| 65 | `agent/memory` | bounded identity/department memory brief | per-run/tail |
| 70 | `agent/inbox` | optional unread assignment/review summary | per-run/tail |

- `session/context` remains session goal/worktree/branch.
- `run/task` remains task objective for task-scoped agents.
- `agent/identity` is project-scoped and can be reused across sessions, which
  improves cache hits; a promotion/transfer/model-generation change creates a
  new cache epoch.
- Labour has no `agent/identity`; it uses `session/context` + `run/task` + the
  frozen coding bundle.

### 2.7 Hiring, retirement, promotion

- `agent.hire` (Leader/Manager proposal, operator approval): role, department,
  component, profile, model, budget, parent identity.
- `agent.retire` (Leader/Manager proposal or operator): evidence-backed,
  operator approval by default; ends assignments, archives workspace, retains
  all events; optional `successor_id`.
- `agent.promote`, `agent.transfer` later; model/profile changes increment
  `generation` and require approval if configured.
- No automatic Manager creation or automatic firing. A deterministic
  `AgentRetirementPolicy` may propose retirement after repeated Verifier
  rejections, no-progress retries, stalls, or budget burn without progress;
  the operator still decides by default.
- Labour is never "hired" into the project roster; it is spawned and ends.

### 2.8 Budgets and accountability

- Identity-scoped token/USD ledger; project budget remains authoritative.
- Manager/Worker/Leader budgets can be set at hire time and narrowed by plan.
- Verifier verdicts, failure attribution, retries, cost, and task results feed
  the identity performance record; no LLM-only judgment.
- Retired/ paused identities cannot be assigned new tasks; no auto-rehire.

---

## 3. Phases

### S1 — Identity schema, lifecycle, and policy

- Additive `agent_identities`, `agent_runs`; keep `agents` compatibility.
- `AgentIdentityPolicy` project setting; default off.
- Events: `agent.identity.created`, `.updated`, `.paused`, `.resumed`,
  `.retired`; projections and replay.
- Migration: existing sessions stay ephemeral; new/project-enabled sessions
  use identities.
- `vp agents` / `agent.identities` read APIs; no GUI yet.

**Acceptance:** old sessions replay unchanged; new identity rows are
project-scoped and event-backed; disabling the policy falls back cleanly.

### S2 — Session assignment, identity lease, inbox

- `session_agent_assignments`, `agent_inbox`.
- Leader/Manager/Worker assignment from the project roster; one active session
  per identity in v1.
- Identity lease prevents concurrent same-identity sessions; different
  components can still run in parallel via write scopes.
- Pull-based inbox; assignment can wake a session through the continuation
  path.

**Acceptance:** a Worker cannot be assigned to two active sessions; an inbox
message survives daemon restart; Labour has only task-scoped inbox.

### S3 — Role contracts, prompts, and cache

- `agent/identity` stable section; `agent/memory` tail section.
- Worker becomes tech lead: low-level design, decomposition, interface/
  acceptance, review, integration; no bulk implementation.
- Labour becomes implementation worker: code/tests/commit; no delegation.
- Manager becomes department architect; Leader becomes project/portfolio
  owner.
- Tool bundles and role policy updated; Worker gets `delegate_labour`, Labour
  does not.
- Cache epoch includes identity generation; promotion/transfer changes it.

**Acceptance:** a Worker task can decompose into Labour subtasks and review
their commits; Labour cannot delegate; prompt identity section is
byte-identical across runs of the same generation.

### S4 — Worker workspace and two-level integration

- Project component branch per Worker identity.
- Session worker worktree from component branch; Labour child worktree.
- Labour → Worker deterministic merge; Worker → session Verifier/Integrator.
- Identity lease; conflict/rollback; workspace archive/land.
- `vp workspace` / GUI component view.

**Acceptance:** Labour commits can be reviewed and merged into the Worker
workspace; Verifier/Integrator still gate the session branch; two sessions
cannot mutate the same component workspace concurrently.

### S5 — Identity memory

- `agent_memories`; bounded brief; evidence/provenance; TTL/stale/supersede.
- `remember`/`recall` scoped to identity/department/project; retrieval via
  existing memory/source tools.
- Memory update events; cache invalidation only in the tail.

**Acceptance:** identity memory survives sessions; a bounded brief is injected;
no unbounded prompt growth; stale memory is visible and not silently trusted.

### S6 — Hiring, retirement, performance

- `agent.hire`, `agent.retire`, `agent.promote`, `agent.transfer`; approval
  classified; project caps.
- `AgentRetirementPolicy` evidence-based proposals; operator approval.
- Performance ledger: task results, Verifier verdicts, failure attribution,
  cost, retries.
- Successor transfer of workspace/memory where safe.

**Acceptance:** a Worker can be retired without deleting evidence; retired
identity cannot be assigned; no automatic firing; caps enforced.

### S7 — GUI, CLI, and observability

- Project org chart (Leader/Managers/Workers), identity detail (role,
  component, generation, assignments, inbox, memory, performance, retirement).
- Cross-session roster; hire/retire sheets; inbox; workspace status.
- CLI: `vp agents`, `vp agent-hire`, `vp agent-retire`, `vp agent-inbox`,
  `vp agent-memory`.
- OTel/event projections include identity id without secrets.

**Acceptance:** operator can see who owns what, who is idle/retired, and what
each identity has done; all actions call real RPCs and surface errors.

---

## 4. Dependencies and ordering

- **S1** depends on C1 (TaskContract) and C4 (version pinning).
- **S2** depends on S1.
- **S3** depends on S1/S2 and the cache-lossless prompt sections.
- **S4** depends on S2/S3; it is the largest worktree change.
- **S5** depends on S1/S2.
- **S6** depends on S1 + C2 (effect receipts) + C6 (failure attribution).
- **S7** after S1–S6.
- **C10 Code Mode** should wait until S3/S4 if stable identities are enabled;
  otherwise it can proceed with the current run-scoped model.

Recommended program order:

```text
C1 → C2 → C4 (Contracted Execution Phase 1 substrate)
        └── S1 → S2 → S3 → S4 → S5 → S6 → S7
C3/C5 can proceed in parallel.
C6/C7/C8/C9 after C2/C4 and S1/S3 where relevant.
C10 after S3/S4 (if identities enabled) or after C1/C2 (run-scoped mode).
```

---

## 5. Migration and rollback

- Additive tables/events; no destructive migration.
- `AgentIdentityPolicy.enabled = false` by default. Existing projects and
  sessions keep current behavior.
- Existing `agents` rows backfill to `agent_runs` only for display; no
  synthetic identities are invented.
- New project-enabled sessions create identities at Leader/Manager/Worker
  hire/assignment.
- Disable policy: stop new assignments, identities remain readable/retired;
  active sessions continue under their existing assignment until they end.
- Bundle export/import and `db-rebuild` include identities, assignments,
  inbox, memories, workspaces; imported identities are project-scoped and
  read-only until explicitly rehomed.
- Rollback to run-scoped mode leaves evidence and assignments intact.

---

## 6. Acceptance

- No gate weakening: identity never bypasses role/profile policy, approvals,
  Seatbelt, path jail, write scope, Verifier, merge queue, capacity, or budget.
- Project isolation: an identity cannot be assigned outside its project.
- Identity/run separation: a run crash does not destroy identity, memory,
  inbox, or performance evidence.
- Labour remains ephemeral and cannot delegate.
- Worker can decompose, review, and integrate Labour work.
- Two-level integration preserves Verifier and deterministic Integrator.
- Identity lease prevents concurrent same-workspace writes.
- Retirement is evidence-based, operator-approved by default, and never
  deletes evidence.
- Cache: stable identity section is byte-identical across runs of the same
  generation; memory changes only move the tail.
- Full Rust/Swift suites, replay/repair/db-rebuild, and migration fixtures.

---

## 7. Non-goals

- No unkillable long-running agent process.
- No cross-project identity sharing in v1.
- No automatic hiring, firing, promotion, or department rewrite.
- No LLM-only performance judgment or hidden model routing.
- No infinite identity memory; no identity memory as a security credential.
- No public A2A identity federation in v1.
- No replacement of Verifier/Integrator or deterministic scheduler.
- No automatic Manager creation; hires are explicit and approved.

---

## 8. Risks

- **Big migration.** Mitigation: additive tables, policy off by default,
  compatibility `agents` projection, dual-write only in S1.
- **Long-lived component branches.** Mitigation: session worker worktree,
  frequent Labour→Worker integration, deterministic diff/rollback, identity
  lease.
- **Identity concurrency.** One active session per identity in v1; later
  disjoint portfolio with explicit leases.
- **Stale memory.** Evidence-backed memory, TTL/stale, bounded brief, no
  silent trust.
- **Model/role drift.** `generation` on model/profile/role changes; cache epoch
  and approval.
- **Operator overhead.** Hire/retire approvals, roster UI, defaults; keep
  Labour automatic.
- **Anthropomorphism.** Identity is a durable record and accountability
  boundary, not a claim of consciousness or personhood.

---

## 9. Open decisions

1. **Leader concurrency:** one active session per project Leader in v1, or a
   project Leader supervising multiple sessions? Recommended: one active
   session in v1; portfolio view later.
2. **Worker workspace:** one long-lived component worktree per identity vs
   per-session worker worktree from a component branch. Recommended:
   per-session worktree from a project component branch; identity lease.
3. **Manager workspace:** docs-only or no workspace. Recommended: no code
   workspace; memory/inbox only.
4. **Model changes:** new generation with approval vs new identity. Recommended:
   generation + approval, preserve memory/performance.
5. **Cross-project detachment:** later; v1 project-only.

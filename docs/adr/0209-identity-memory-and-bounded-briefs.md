# 0209 — Identity memory and bounded briefs (S5)

- **Status:** Accepted
- **Date:** 2026-11-13
- **Base:** `1bc6b94` (after ADR-0208)
- **Scope:** S5 of `docs/PROJECT_SCOPED_AGENT_IDENTITIES_PLAN.md`
- **Related:** ADR-0205 (project identities), ADR-0206 (assignments and inbox),
  ADR-0207 (identity prompts and Worker/Labour contracts), ADR-0208 (Worker
  workspaces), ADR-0188 (instruction store and prompt layout), ADR-0189
  (append-only conversation), ADR-0191 (cache epochs and compaction), ADR-0193
  (task contracts), ADR-0196 (runtime pinning)

## Context

S1–S4 made Leader, Manager, and Worker durable project records with lifecycle,
session assignments, role contracts, stable prompt sections, and Worker
component workspaces. Those records can outlive the session that created them,
but the only durable “knowledge” attached to an identity was the existing
project-memory projection: a session-scoped row with a free-form kind, title,
body, tags, and optional evidence. It was visible to any agent in the project,
had no identity/department scope, no explicit supersede edge, no bounded TTL,
and no dedicated append-only stream. The S3 `agent/memory` tail reused that
project memory as a temporary compatibility source.

That is not sufficient for a long-lived project org:

- a Worker cannot own component design notes that survive sessions without
  becoming project-wide claims;
- a department cannot share interface contracts that other departments must not
  see;
- stale model prose can be re-injected indefinitely because there is no
  verification, expiry, or supersede lifecycle;
- there is no project-local answer to “who recorded this, on what evidence, and
  what replaced it?”;
- an unbounded memory store could grow the prompt without limit or move the
  stable cache head.

S5 adds a separate, bounded, evidence-backed memory store for project
identities, departments, and the project. It deliberately does **not** replace
the existing session/project memory projection, and it does not create identity
hiring, retirement, performance, an org chart, or team templates (S6–S8).

## Decision

### 1. A separate project-scoped memory table

Two additive tables are created:

```sql
agent_memories(
  id            TEXT PRIMARY KEY,
  project_id    TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  identity_id   TEXT REFERENCES agent_identities(id) ON DELETE CASCADE,
  department    TEXT,
  component     TEXT,
  scope         TEXT NOT NULL,          -- agent | department | project
  content       TEXT NOT NULL,
  content_hash  TEXT NOT NULL,          -- SHA-256 of normalized content
  evidence_json TEXT NOT NULL DEFAULT '[]',
  source        TEXT NOT NULL,
  status        TEXT NOT NULL,          -- active | superseded | stale
  pinned        INTEGER NOT NULL DEFAULT 0,
  created_at    TEXT NOT NULL,
  updated_at    TEXT NOT NULL,
  expires_at    TEXT,
  verified_at   TEXT,
  superseded_by TEXT,
  stale_reason  TEXT,
  origin        TEXT NOT NULL DEFAULT 'local'
);

agent_memory_events(
  id         TEXT PRIMARY KEY,          -- portable archive event id
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  seq        INTEGER NOT NULL,          -- per-project replay order
  kind       TEXT NOT NULL,
  payload    TEXT NOT NULL,
  created_at TEXT NOT NULL,
  UNIQUE (project_id, seq)
);
```

The session-scoped `memories`, `memory_usage`, `memory_conflicts`, FTS indexes,
and `remember`/`recall` legacy path are unchanged. S5 does not duplicate their
tables or tools. A project can use both stores: legacy project memory remains
the default when a tool omits `scope`, while S5 memory is the explicit identity
org store.

`pinned`, `verified_at`, and `stale_reason` are additive metadata needed by the
deterministic brief ranking and stale/forget visibility. `content_hash` is
always recomputed from the exact stored content; it is not caller authority.

### 2. Scope model and isolation

The three scopes have hard, transactional rules:

| Scope | Owner key | Visibility | Required fields |
|---|---|---|---|
| `agent` | `identity_id` | exactly that identity | identity in the same project, local, not retired |
| `department` | `department` | identities in that department | department label; no identity id |
| `project` | project | every identity in the project | no identity id and no department |

For an `agent` memory, the store loads the identity row and derives the
department/component from it. A caller cannot attach a different department to
make a cross-department claim, and cannot point at an identity from another
project. A `department` memory must not carry an identity id; a `project` memory
must not carry an identity id or department. Violations are typed
`MEMORY_SCOPE_INVALID`, `MEMORY_IDENTITY_REQUIRED`, `MEMORY_DEPARTMENT_REQUIRED`,
or `MEMORY_PROJECT_MISMATCH`; they are never silently coerced.

Reads enforce the same rules. `agent.memory.list`/`get` accept an optional
`identity_id` viewer. When present:

- `agent` rows are returned only for the viewer identity;
- `department` rows only for the viewer's department (case-insensitive label
  comparison);
- `project` rows for everyone in the project.

An imported viewer or a viewer from another project fails closed. Omitting the
viewer is an explicit **operator** project-wide read on the local daemon; it is
not reachable through the agent `recall` tool, which always resolves the active
run identity. A task-scoped Labour/Verifier/Scout run has no project identity
and therefore cannot use `agent`/`department` memory scope at all.

Retirement does not delete memory. The row remains readable evidence, enters no
new prompt brief for a retired identity (retired identities are not selected
for runs), and content/evidence/expiry edits fail with
`MEMORY_IDENTITY_RETIRED`. Lifecycle transitions (supersede, expire, forget) stay
available so an operator can reconcile it. Imported identities and imported
memory are read-only until an explicit future rehome.

### 3. Evidence, provenance, and bounded content

`content` is trimmed, UTF-8 safe, at most 2,000 characters, and rejects control
characters except newline/tab/CR. `content_hash` is SHA-256 of that exact string.
An update that changes content recomputes the hash and clears `verified_at`.

`source` is one of `operator`, `agent`, `task_result`, `verifier`,
`integration`, or `imported`. `imported` is archive provenance and is rejected
by the local create path. `task_result`, `verifier`, and `integration` memory
requires at least one evidence pointer. Evidence is bounded to eight entries,
each with a kind in `task|session|commit|artifact|event`, a non-empty
reference of at most 200 characters, and an optional note of at most 200
characters. Evidence never stores prompt text, tool output, file content, or
secrets.

The OTel/cache/event surfaces never receive memory content. The
`context.prompt_prefix` event records only `IdentityMemoryTelemetry`: source
kind, selected memory count, brief characters, brief hash, per-scope counts,
and the selected content hashes. `session.instructions` copies the same
telemetry. It is counts/hashes only.

### 4. Lifecycle

Transitions are explicit and event-backed:

```text
active ── update ───────────────────────────────> active
active ── supersede (existing/new replacement) ─> superseded
active ── expire (TTL/sweep) ───────────────────> stale (reason=expired)
active ── forget (soft) ────────────────────────> stale (reason=forgotten)
stale/superseded ── prune (forget/sweep) ───────> row removed
```

- `agent.memory.supersede` atomically either points at an existing active row
  in the exact same project/scope or creates a replacement from bounded content,
  then sets the old row to `superseded` with `superseded_by`. It cannot
  supersede itself or cross scopes. Replacement inherits the old evidence when
  the caller provides none.
- Soft forget keeps the row and evidence as `stale`/`forgotten`. Prune requires
  a row that is already stale or superseded; an active row must be forgotten
  first. Imported rows are never pruned.
- `agent.memory.verify` records `verified_at` and may extend the TTL. Verification
  is evidence that an operator/agent rechecked a claim; it is not truth and not
  governance authority.
- `expires_at` must be in the future and at most 3,650 days away. Expired rows
  are excluded from briefs and default lists; the read model exposes an
  `expired` flag. `agent.memory.sweep` transitions due active rows to stale and
  optionally prunes stale/superseded rows older than 1–3,650 days. The sweep is
  bounded (default 100, hard cap 1,000) and never touches imported rows.
- Updates accept an optional `expected_content_hash` optimistic guard. A
  mismatch fails with `MEMORY_CONFLICT` and writes nothing.

Events are `agent.memory.created`, `.updated`, `.superseded`, `.expired`, and
`.deleted`. `deleted` carries `mode=soft|prune`; prune events are terminal for
replay. Every mutation commits its projection row and event in one transaction.
The project-scoped stream is replayable, repairable, exportable in a session
bundle, and used by `db-rebuild`.

### 5. Bounded prompt brief

The S3 `agent/memory` tail now primarily renders a deterministic brief from
`agent_memories`, scoped to the active identity, then its department, then the
project. The legacy S3 project-memory brief remains only as a compatibility
fallback when the S5 store has no eligible rows; its telemetry source is
`legacy_project_memory`.

Eligibility:

- status `active`;
- `origin=local`;
- not expired;
- non-empty content;
- visible under the viewer scope.

Ranking is pure Rust with an injected `now`:

```text
score = scope weight (agent 3.0, department 2.0, project 1.0)
      + pinned 0.50
      + verified 0.25
      + recency(updated_at, 30-day half-life) × 0.25
```

Ties are deterministic: score desc, pinned desc, scope rank desc, `updated_at`
desc, `content_hash` asc, `id` asc. There is no model call, no summarizer, no
provider I/O, and no hidden routing.

The brief is byte-bounded to `AGENT_MEMORY_BRIEF_MAX_BYTES` (5,600 bytes), under
the existing 6 KiB `agent/memory` cap. Items are considered in rank order; if a
whole item does not fit, selection stops. An item is never split. An empty brief
omits the tail section entirely. Content is whitespace-collapsed and rendered
with a scope/source label and a bounded provenance suffix (task and short
commit, or a short session). The header states that memory is not conversation,
instructions, or authorization.

The `agent/identity` head section is unchanged. A memory-only change moves the
tail hash/volatile hash and never the core/project/identity/stable-prefix hash or
the finalized cache epoch. An explicit cache warm omits the memory tail and
reports no memory telemetry.

### 6. Tools, RPC, CLI, and client

`remember` and `recall` gain an optional `scope`:

- omitted/blank → the existing legacy project-memory path, unchanged;
- `agent` → the active run identity's agent memory;
- `department` → the active run identity's department memory;
- `project` → project-wide org memory.

There is no silent scope switch. `agent`/`department` without an assigned
identity fails with `MEMORY_IDENTITY_REQUIRED`; `department` without an
identity department fails with `MEMORY_DEPARTMENT_REQUIRED`; an unknown scope
fails closed. Explicit `project` writes the S5 project org store, not the legacy
project-memory projection. Tool output is bounded and never returns raw evidence
JSON. `remember` automatically adds bounded session/task evidence and dedupes it.

New RPCs:

```text
agent.memory.list      agent.memory.get       agent.memory.remember
agent.memory.update    agent.memory.supersede agent.memory.forget
agent.memory.verify    agent.memory.sweep     agent.memory.replay
```

New CLI commands:

```text
vp agent-memory <project> [--identity ...] [--scope ...] [--status ...]
vp agent-memory-remember <project> --scope ... --content ...
vp agent-memory-supersede <memory> [--replacement ... | --content ...]
vp agent-memory-forget <memory> [--prune]
vp agent-memory-sweep <project> [--older-than-days ...] [--prune]
```

Errors use stable wire codes in `agent_memory_codes` and are mapped to RPC
`NOT_FOUND`/`CONFLICT`/`INVALID_PARAMS` by typed downcast, never by message text.

The SwiftUI client has a read-only identity-memory card in Manage Dash: scope,
department/component, status/expiry, bounded content, evidence labels, and an
imported marker. There is no mutation UI in S5. All state lives in `AppModel`
and `Core.Models`; the view only renders.

### 7. Archive, replay, and rebuild

Session bundles gain `agent_memories` and `agent_memory_events` (both
`serde(default)` for old bundles). Imported rows are forced to
`origin=imported`; event snapshots are deterministically rehomed to the same
origin, so a repeated import compares byte-for-byte and a local event with the
same id still conflicts. Imported memory is readable evidence, excluded from
briefs, and rejects mutation with `MEMORY_IMPORTED_READONLY`.

`agent.memory.replay` folds the project stream and compares the full row except
`origin` (archive provenance) and `expired` (read-time derivation). Repair
replaces the projection while preserving imported origin markers for ids that
were already imported. The project manifest carries both rows and events;
`db-rebuild` inserts the events, validates the manifest projection against the
event-derived projection, and restores the snapshot rows so provenance markers
survive.

## Tests

The S5 suite covers:

- scope isolation for agent/department/project, viewer enforcement, cross-project
  rejection, and typed scope-shape errors;
- evidence validation, source/evidence requirements, content normalization and
  hash, bounds, TTL validation, optimistic update conflicts, and verification
  invalidation on content change;
- atomic supersede (existing and new replacement), soft forget/prune, retired
  identity read-only behavior, imported read-only behavior, and bounded sweep;
- deterministic brief ranking, scope visibility, byte cap, no item splitting,
  empty omission, telemetry counts/hashes without content, and a cache test
  proving a memory-only change moves the tail but not the head/epoch;
- `remember`/`recall` default backcompat, explicit scope behavior, and typed
  tool errors;
- replay/repair, bundle import/idempotence/conflict rejection, manifest
  `db-rebuild`, and protocol/CLI/Swift decode.

## Limits and non-goals

- Memory is **untrusted reference material with provenance**, not truth,
  instructions, governance, a credential, or an authorization source. It never
  bypasses role/profile policy, approvals, Seatbelt, path jail, write scope,
  Verifier, merge queue, capacity, or budget.
- The brief is best-effort and ranked, not a complete recall or a semantic
  search. Full retrieval remains `recall`/source tools; there is no embedding or
  ANN lane for identity memory in S5.
- Department matching is a case-insensitive label, not a hierarchy. There is no
  inheritance, cross-department sharing, or project-wide Leader bypass.
- Same-host SQLite project isolation only; no cross-project or cross-host memory
  federation.
- Imported memory is read-only; there is no S5 rehome operation.
- No automatic memory generation from transcripts, task results, or Verifier
  prose. Provenance sources are recorded only on explicit writes.
- No automatic daemon sweep is enabled by this ADR; `agent.memory.sweep` is an
  explicit operator/daemon action. There is no retention policy table yet.
- No mutation UI, org chart, hiring/retirement, performance ledger, or team
  templates (S6–S8). No cross-host/distributed memory.
- The legacy `memories` table, FTS, semantic lane, conflict detection, and tools
  are intentionally unchanged. This ADR does not claim they are the same store.

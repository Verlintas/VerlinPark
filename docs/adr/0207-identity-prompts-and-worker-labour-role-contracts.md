# 0207 — Identity prompts, role contracts, and cache epochs (S3)

- **Status:** Accepted
- **Date:** 2026-11-13
- **Base:** `292cc78` (after ADR-0206)
- **Scope:** S3 of `docs/PROJECT_SCOPED_AGENT_IDENTITIES_PLAN.md`
- **Related:** ADR-0188 (instruction store), ADR-0190 (tool bundles), ADR-0191
  (cache epochs), ADR-0205 (project identities), ADR-0206 (assignments and
  inbox)

## Context

S1 added durable project-scoped Leader/Manager/Worker identities. S2 bound an
identity to one session through a lease and added the project inbox. Neither
phase changed the prompt: every run still received the same run-scoped role
charter, session context, and memory tail as before. A Worker and a Labour run
with the same profile looked nearly identical to the model, and a promotion or
transfer could not move a stable prompt component.

S3 closes that gap without adding workspaces (S4), an identity-memory table
(S5), hiring/retirement (S6), an org chart (S7), or team templates (S8). It
adds two instruction sections, rewrites the four role contracts so the
Leader → Manager → Worker → Labour division of labour is explicit, and makes a
promotion/transfer/model/profile/component/title change a new cache epoch.

The prompt is still a deterministic function of trusted metadata. No model
routing, budget, approval, sandbox, write-scope, Verifier, Integrator, or
capacity rule changes in this phase.

## Decision

### 1. Two new instruction sections

| Ordinal | Key | Position | Volatility |
| ---: | --- | --- | --- |
| 15 | `agent/identity` | head | stable for one identity generation |
| 65 | `agent/memory` | tail | per-run bounded identity/department brief |

`agent/identity` sits between `core/charter` (10) and `core/recipes` (20). It is
a head section so rolling conversation compaction cannot drop it and so the
ordered v3 prefix hash commits to it. `agent/memory` sits between `run/task`
(60) and `tail/memory` (90); it is sent after the conversation, changes only the
volatile/tail hash, and never moves the reusable head prefix.

The existing ordinal spacing is preserved. `session/contract` remains 55 and
`tail/memory` remains 90. Adding ordinals 15 and 65 changes the ordered section
vocabulary only for runs that actually emit those sections; a legacy session
with no assignment emits exactly the pre-S3 keys and bytes.

### 2. Identity section content

The section is rendered by `vp_core::identity_prompt::render_identity_section`
from trusted `AgentIdentity` metadata and the deterministic role contract:

```text
Agent identity (stable for this identity generation; not a session goal,
conversation, or authorization):
Identity: <id>
Generation: <n>
Role: <leader|manager|worker>
Role label: <short human label>
Title: <title or "not set">
Department: <department or "not set">
Component: <component or "not set">
Profile: <profile or "not set">
Model: <identity model metadata or "not set">
Charter: <short deterministic charter>
Decision rights: <deterministic decision rights>
Review duties: <deterministic review duties>
Implementation stance: <deterministic stance>
Delegation: <deterministic delegation rule>
This identity record describes scope and duties only. The runtime role/profile
policy, task contract, approvals, sandbox, path jail, write scope, Verifier,
and deterministic Integrator remain authoritative.
```

Properties:

- **Stable.** The renderer contains no timestamp, session id, goal, worktree,
  branch, task objective, memory, inbox row, tool argument, provider usage, or
  secret. For a fixed identity id/generation/metadata the bytes are identical
  across runs and sessions.
- **Bounded.** The rendered section is capped at 8 KiB
  (`IDENTITY_SECTION_MAX_BYTES`). Individual metadata fields are sanitized to a
  single line and character-bounded before rendering, so a newline in a title
  cannot fabricate a section boundary.
- **Generation-scoped.** The instruction store reuses verified existing bytes
  while the freshly rendered candidate has the same hash. A promotion,
  transfer, model/profile/component/title change, or any other metadata change
  increments `generation` in S1, produces a new candidate, and moves the head
  on purpose. Missing or corrupt blobs rebuild from the deterministic renderer
  and emit the existing bounded `context.instruction.rebuilt` diagnostic.
- **Not authority.** The runtime role/profile policy, `TaskContract`,
  approvals, Seatbelt, path jail, write scope, Verifier, and Integrator remain
  the enforcement points. The section describes the identity; it does not grant
  anything.

The identity `Model` line is metadata only. It does not select a provider
model: `RoleModelConfig` remains the only model source. This is deliberately
called out in the section and in tests.

### 3. Which runs get an identity section

Only Leader, Manager, and Worker can own a project identity. The resolver
(`Engine::prompt_identity_for_run`) returns `None` when:

- the role is Labour, Verifier, or Scout;
- the session has no active assignment for that role;
- the assignment disappeared, was released, or was reclaimed;
- the identity is paused, retired, or imported;
- the identity and session belong to different projects;
- the assignment role and identity role do not match.

Leader/Manager/Worker therefore get the section only when the session has an
active assignment of the matching role. Labour is task-scoped and remains
ephemeral; Verifier and Scout are gates/exploration helpers. None of them gets
an identity section even if a caller passes an identity-shaped role name.

Multiple identities of one role may be assigned to a session. S3 resolves a run
deterministically:

1. exact role;
2. exact title match (Managers normally; Worker titles are optional);
3. exact department match;
4. oldest `assigned_at`, then identity id, for a stable tie-break.

The run-to-identity binding becomes explicit with the Worker workspace and
component branch in S4. S3 does not claim that a Worker run is permanently
bound to one identity beyond this deterministic selection.

The resolver uses the persisted assignment status, not the wall-clock lease
expiry. S2 reclaims an expired lease before another session can take the
identity; consulting `lease_active` here would make a live session's stable
prompt section disappear after ten minutes without a renewal. The lease still
prevents concurrent assignment.

Layout v2 keeps the historical combined `core + project + volatile` system
prompt byte-for-byte and never emits `agent/identity` or `agent/memory`.
Assigning an identity does not change an explicit v2 rollback session.

### 4. Identity/department memory tail

S3 does not add the dedicated `agent_memories` table; that is S5. It reuses the
existing evidence-backed project memory store:

- the query token is the identity component, then department, then the first
  title word, then the role;
- the project memory search is lexical-only for this brief (no new embedding
  call), bounded to 16 candidates and then to 5.5k characters;
- the rendered tail is capped at 6 KiB;
- an empty result omits the section entirely rather than emitting an empty
  heading;
- it is labeled identity/department memory and explicitly marked as not
  conversation and not authorization;
- it does not mark memory as used; the ordinary `tail/memory` run brief keeps
  the existing usage accounting.

A memory change moves only `volatile_hash` and the tail bytes. It never moves
`prefix_hash`, `stable_prefix_hash`, `core_hash`, `project_hash`, or the cache
epoch.

### 5. Role contracts and delegation

The role charters and coding-profile notes now express the target operating
model:

- **Leader — project/portfolio owner.** Direction, plan, priorities, budget,
  cross-department decisions, final approval. Small inspections or an
  explicitly requested one-file fix remain acceptable; bulk implementation
  belongs to Labour.
- **Manager — department architect.** Department architecture, cross-Worker
  interface contracts, Worker plan/design review, coordination. Read-only: no
  file edits, shell, or bulk code. Delegates to Workers, optionally in parallel
  with non-overlapping write scopes.
- **Worker — technical lead.** Low-level design, decomposition into Labour
  tasks, interface/acceptance definition, code review, and integration of
  Labour changes. Design docs, interface notes, and minimal scaffolding are
  acceptable; bulk implementation must be delegated to Labour.
- **Labour — implementation worker.** Executes one task in its worktree, writes
  code/tests, runs the relevant checks, commits, and reports evidence. It
  cannot delegate and has no project identity.
- **Verifier — independent gate.** Read-only review and verdict; no delegation.
- **Scout — read-only exploration.** No writes, shell, delegation, identity, or
  merge.

The `delegate` schema is narrowed to the one child role that the delegation
chain permits: Leader → Manager, Manager → Worker, Worker → Labour. The enum is
an affordance, not the enforcement point: `parse_delegate` still rejects a
wrong or missing child role, and `spec.allow_delegation` still comes from
`AgentRole::child_role()`. Labour, Verifier, and Scout have no child role, no
`delegate` schema, and no `delegate` entry in `role_tool_surface`. Tool bundle
membership remains a cache-prefix partition; role/profile policy remains
authoritative at dispatch.

The Manager bundle now keeps its authority surface (`delegate`,
`delegate_many`, `delegate_acp`, `scout`, `task_control`) while still pruning
workspace writes, shell, process, browser, and computer. This closes a
pre-existing mismatch where the Manager prompt told the model to delegate but
the frozen Leader bundle removed every delegation schema for the Manager role.

No automatic delegation is added. The Worker must choose to delegate; a task
that fits inside the Worker's own scope can still be done directly within the
stated "no bulk implementation" contract.

### 6. Cache identity

`PromptSections` now carries an optional `PromptIdentityMarker`
(`identity_id`, `generation`, `agent/identity` hash). The marker is `None` for
legacy sessions, v2 layout, and Labour/Verifier/Scout.

`PromptSectionHashes` adds:

- `identity_hash`: hash of the `agent/identity` head section, or empty;
- `identity_id` and `identity_generation`;
- `identity_sections`: the ordered identity head refs, kept separate from
  core/project refs so attribution can name an identity change instead of
  calling it a project edit.

The v3 ordered prefix hash already commits to every head section, including
`agent/identity`. The cache epoch additionally commits to the identity
component:

```text
cache_epoch = sha256(
  scheme, version, model, tools,
  core, project,
  core_sections, project_sections,
  identity_hash, identity_generation, identity_id, identity_sections  // only when present
)
```

`CacheEpochIdentity` gains `identity_hash`, `identity_generation`,
`identity_id`, and `identity_sections` with serde defaults. The identity
components are appended to the canonical material **only when
`identity_hash` is non-empty**. This preserves every pre-S3 epoch hash exactly;
old event payloads decode with empty defaults and still validate against their
recorded hash.

`CacheEpochChangeReason::Identity` is reported when the previous and current
identity components differ. `CacheMissReason::IdentityChanged` is used for
per-call attribution when the stable identity hash changes. A memory-only change
does not produce either reason.

The epoch and prompt-prefix payloads carry hashes, ids, generations, ordinals,
and character counts only. They never carry identity prompt text, memory text,
goals, worktrees, or tool arguments.

### 7. Read surfaces

`session.instructions` remains the ordered, text-free instruction view. It adds:

- report-level `latest_identity_id`, `latest_identity_generation`, and
  `latest_identity_hash` from the latest `context.prompt_prefix`;
- per-state `identity_hash` when the state contains `agent/identity`.

`session.cache.plan` role rows add `identity_hash`, `identity_id`, and
`identity_generation`. `session.cache` adds
`cache_epoch_identity_hash`, `cache_epoch_identity_id`,
`cache_epoch_identity_generation`, and `cache_epoch_identity_sections`.
`session.cache.warm` adds the same identity metadata to its result.
`context.prompt_prefix` adds the same identity fields plus `identity_sections`,
`identity_section_count`, and `identity_memory_present`. `vp instructions`
prints a short identity badge. Swift decodes the additive optional fields and
shows a compact identity badge in the instruction-section inspector. No new RPC
or command is required.

## Tests

Unit tests cover:

- key vocabulary, ordinals 15/65, head/tail classification, generation-scoped
  reuse, missing/corrupt identity blob rebuild, warm preserving the identity
  head, identity/memory replay and ordinal drift rejection, and oversize
  identity/memory sections failing closed;
- deterministic renderer bytes, metadata sanitization, no session/worktree/
  secret material, generation changes, unsupported roles, empty ids, bounded
  memory clipping, assignment selection, paused/project-mismatched identities,
  and all three role contracts;
- identity hashing separate from core/project, provider `identity` section kind,
  identity generation vs memory tail effects, bounded section metadata, and
  `IdentityChanged` miss attribution;
- cache-epoch identity hashing, identity change attribution, memory stability,
  removal of an identity, and legacy identity-free epoch payloads keeping their
  historical hash;
- Worker→Labour delegation schema and policy, Labour/Verifier/Scout having no
  delegation, and the four role prompt contracts.

Integration tests (`crates/vp-core/tests/identity_prompts.rs`) cover:

- a legacy session with no assignment keeps the pre-S3 prompt and reports no
  identity;
- an assigned Leader gets a byte-identical identity section across two runs,
  and telemetry/bundle metadata do not expose its text through ordinary cache
  surfaces;
- a title/generation update moves the identity section, prefix hash, and cache
  epoch with `cache_epoch_change_reason = identity`, while core/project hashes
  and the charter stay unchanged;
- identity memory appears only in the tail, moves `volatile_hash`, leaves the
  head prefix unchanged, and is labeled as memory;
- cache plans resolve identity for Leader/Manager/Worker and omit it for
  Labour/Verifier/Scout;
- multiple Worker identities resolve by title then age deterministically;
- identity instruction state replays/repairs after projection loss and the
  verified blob remains exportable;
- cross-project assignment is rejected and legacy sessions remain unchanged.

Protocol tests cover additive decode with legacy defaults. CLI tests cover the
identity badge and text-free output. Swift tests cover the additive identity
fields and the instruction-section badge.

## Limits and non-goals

- S4 workspace/component branches are not implemented. Run-to-identity binding
  is deterministic for S3 but becomes explicit only in S4.
- S5 identity memory is not implemented. `agent/memory` is a bounded reuse of
  existing project memory, not a durable per-identity memory store.
- S6 hiring/retirement/performance, S7 org chart/CLI roster work, and S8 team
  templates are not implemented.
- No automatic delegation, hidden model routing, automatic Manager creation,
  or role reassignment. `delegate` remains a model choice gated by the role
  policy.
- The identity section is a prompt description, not a credential. It cannot
  approve tool calls, widen write scope, bypass the Verifier/Integrator, or
  change budgets/capacity.
- A generator/prompt-wording change is a deliberate product change. If it
  changes the rendered identity bytes for an existing generation, the head
  moves and the epoch changes; the runtime cannot distinguish that from an
  identity metadata edit using only the instruction-store row. The generation
  counter remains the semantic signal for identity changes.
- Layout v2 ignores identities by design. There is no identity section in the
  legacy combined prompt.
- ACP agents are external protocol participants; S3's native identity section
  applies to native agent runs. `delegate_acp` still records a Worker run under
  the existing gates.
- Memory retrieval is lexical and best-effort. An empty result omits the tail;
  it does not claim that the identity has no relevant memory.
- The resolver does not consult lease wall-clock expiry. Assignment status is
  authoritative; S2 owns reclaiming an expired lease.
- Imported identities are excluded from prompt identity even if an imported
  assignment row exists. Imported coordination rows remain archive evidence.

## Acceptance matrix

| Requirement | Where it is enforced | Test evidence |
| --- | --- | --- |
| Leader/Manager/Worker get an identity section only from an active assignment | `Engine::prompt_identity_for_run`, `identity_prompt::prefer_identity_assignment`, `build_prompt_sections` | assigned Leader/Manager/Worker prompt tests; legacy session test |
| Labour/Verifier/Scout never get one | role allowlist plus `build_prompt_sections` role path | cache-plan role test; direct build tests; delegated-run test |
| Legacy sessions keep the pre-S3 prompt | `prompt_identity_for_run` returns `None`; no candidate is pushed | legacy no-assignment test; v2 rollback test |
| Same generation is byte-identical across runs/sessions | deterministic renderer, content-addressed store, generation-scoped reuse | two-run test; cross-session epoch test; instruction unit tests |
| Generation/metadata change moves section and epoch | generation-scoped materialization, ordered v3 prefix, epoch identity fields | generation-change integration test; cache epoch unit tests |
| Memory change moves only the tail | `agent/memory` is tail-only; volatile/tail hash covers it; epoch excludes it | memory-only integration test; cache epoch memory test |
| Missing/corrupt identity blob rebuilds with a diagnostic | generation-scoped verification path + `context.instruction.rebuilt` | missing-blob integration test; instruction unit tests |
| Identity rendering is bounded and sanitized | 8 KiB cap, per-field single-line sanitizer, UTF-8 clipping | renderer bound/control/Unicode tests |
| Worker delegates implementation to Labour | role contract, `delegate` schema enum, `parse_delegate` chain | Worker→Labour unit test; delegated nested-run test; Worker bundle test |
| Labour cannot delegate | no child role, no `delegate` schema, dispatch/parse rejection | leaf-role schema/surface test; parse rejection test |
| Manager/Leader do not claim bulk implementation | rewritten role charters/profile notes | role prompt tests; Manager bundle authority test |
| Role policy remains authoritative | dispatch checks `allows_tool`, `allow_delegation`, task contract, sandbox, approvals | frozen bundle/role matrix tests; existing dispatch suite |
| Cache metadata contains no prompt text | bounded epoch/prefix payloads, `session.instructions` metadata only | telemetry leak tests; report/CLI decode tests |
| Compaction preserves the identity head | instruction state is separate from conversation; compaction never rewrites it | compaction integration test; warm test |
| Replay/repair/db-rebuild preserve identity rows | existing instruction-state replay + rebuild paths | replay/repair integration test; database rebuild test |
| Additive protocol decode | serde defaults on every new field | protocol decode tests; Swift decode tests |

## Operational notes

- `agent/identity` is intentionally placed before `core/recipes`: an explicit
  Anthropic core breakpoint still lands after the identity bytes, so the
  identity is part of the cached core prefix without consuming a fourth
  breakpoint.
- The identity section is cached as a normal content-addressed blob. Exporting
  a session bundle carries the verified blob; ordinary cache/telemetry surfaces
  never return its text.
- A cache preview or warm is read-only with respect to identity assignment. It
  resolves the same identity the next run would use and never creates an
  assignment, lease, or run.
- If the optional identity memory would push the existing run-memory tail over
  the aggregate 32 KiB tail cap, the identity memory is omitted and the head
  identity section still materializes. Identity memory must never make a run
  fail that would otherwise succeed.
- Removing an assignment, pausing/retiring an identity, or changing its
  generation is visible as a new prompt prefix. Operators should expect the
  next call to miss the previous prefix; this is an intentional identity
  change, not a provider eviction.

## Why this layout is cache-lossless

The provider cache reuses an exact byte prefix. S3 keeps the same discipline as
the cache-lossless round (ADR-0188/0191):

1. **No moving text in the head.** The identity head contains only metadata
   that changes when the identity itself changes. `updated_at`, assignment
   timestamps, lease expiry, session goal, worktree, branch, and inbox state
   are deliberately excluded.
2. **The current prompt never enters the head.** Identity is rendered once and
   stored by hash. A follow-up prompt is a user message after the existing
   conversation, not a new head section.
3. **Memory is tail-only.** `agent/memory` is sent after the conversation and
   is covered by `volatile_hash`, not `prefix_hash` or the cache epoch.
4. **Ordered identity is explicit.** The v3 prefix hash includes the key,
   ordinal, and hash of every head section. A section reorder or a key rename
   cannot silently reuse a provider prefix that represents different bytes.
5. **The epoch names the identity.** Two sessions with the same identity
   generation, model, tools, core, and project sections share an epoch even
   when their goals and memory differ. A promotion moves the epoch because the
   identity component is in the canonical epoch material.
6. **Legacy epochs remain valid.** The identity component is appended only when
   present. Old `context.prompt_prefix`/`context.cache` payloads decode with
   empty defaults and their recorded epoch hash still validates.
7. **Warm is explicit and honest.** An explicit warm sends the identity head
   and stable sections but no memory, and reports the identity metadata on the
   warm result. It is billed as a normal provider call.

## Failure modes and operator expectations

- **Identity changed.** The next prompt has a new `agent/identity` hash and a
  new cache epoch. A provider miss is expected; the per-call reason is
  `identity_changed` when the stable identity hash moved.
- **Assignment removed.** The next run has no identity section, so the head
  and epoch move back to the legacy shape. This is intentional and visible in
  `session.instructions` and `context.prompt_prefix`.
- **Identity paused/retired.** The resolver stops selecting it for new prompts.
  Existing assignment rows are not deleted; S2 owns release/reclaim.
- **Blob corruption or deletion.** The instruction store verifies every hash,
  rebuilds the identity section from the deterministic renderer, stores the
  blob again, and emits `context.instruction.rebuilt` with
  `key=agent/identity` and a bounded reason. The run is not given unverified
  bytes.
- **Optional memory overflow.** If identity memory plus run memory would exceed
  the aggregate tail cap, identity memory is omitted; the identity head still
  materializes and the run proceeds.
- **Legacy v2 rollback.** No identity or identity-memory section is emitted.
  Switching back to v3 reuses the materialized v3 state, including the identity
  section, because rollback is non-destructive.
- **Imported/archived session.** Imported identities and assignments are
  archive evidence. The prompt resolver returns `None`; a read-only archive
  never gains a local identity prompt.

## Migration and compatibility

- No schema migration is required for S3: instruction keys are new rows in the
  existing content-addressed store; identity rows/assignments come from
  S1/S2.
- Existing sessions have no assignment and therefore no identity rows. They
  replay unchanged.
- Existing epoch payloads are not rewritten. New fields are additive and
  serde-defaulted.
- New protocol fields are optional/empty by default. Old daemons decoding a
  new payload ignore them; new daemons decoding an old payload default them.
- The resolver is deterministic but intentionally does not persist a
  run-to-identity binding. S4 will make the binding explicit for Worker
  workspaces; until then, operators should not assign two Workers with
  identical title/department/age expectations to one session and expect an
  ambiguous run to choose a specific one beyond the documented tie-break.

## Non-goals restated

S3 does not add a long-lived agent process, cross-project identity sharing,
automatic hiring/firing/promotion, identity memory tables, Worker component
branches, org-chart UI, team templates, distributed leasing, or provider
exactly-once behavior. Identity remains an accountability and prompt-scope
record, not a security credential or a claim of personhood.

## Example metadata shapes

`session.instructions` remains text-free. A state with an assigned Worker
identity reports the key, hash, ordinal, tail flag, size, availability, and the
state-level identity hash:

```json
{
  "state_id": "ses-1#worker-1",
  "role": "worker",
  "prompt_layout": 3,
  "head_hash": "…",
  "tail_hash": "…",
  "identity_hash": "8f2c…",
  "sections": [
    {"key": "agent/identity", "hash": "8f2c…", "ordinal": 15, "tail": false, "chars": 1420, "available": true},
    {"key": "agent/memory", "hash": "4a11…", "ordinal": 65, "tail": true, "chars": 380, "available": true},
    {"key": "tail/memory", "hash": "91dd…", "ordinal": 90, "tail": true, "chars": 512, "available": true}
  ]
}
```

A `context.prompt_prefix` event carries the identity component that the cache
epoch commits to. No title, charter, memory body, goal, worktree, or tool
argument is present:

```json
{
  "identity_id": "agt-worker-1",
  "identity_generation": 4,
  "identity_hash": "8f2c…",
  "identity_sections": [
    {"key": "agent/identity", "hash": "8f2c…", "ordinal": 15, "tail": false}
  ],
  "identity_section_count": 1,
  "identity_memory_present": true,
  "cache_epoch_identity_hash": "8f2c…",
  "cache_epoch_identity_generation": 4,
  "cache_epoch_identity_id": "agt-worker-1",
  "cache_epoch_identity_sections": [
    {"key": "agent/identity", "hash": "8f2c…", "ordinal": 15, "tail": false}
  ],
  "cache_epoch_change_reason": "identity"
}
```

A legacy or Labour prompt prefix has no identity component:

```json
{
  "identity_id": null,
  "identity_generation": null,
  "identity_hash": null,
  "identity_sections": [],
  "identity_section_count": 0,
  "identity_memory_present": false
}
```

`session.cache.plan` mirrors the role metadata before a run, and
`session.cache.warm` mirrors it for an explicit warm. Both are read-only with
respect to assignment and never create an agent row, worktree, or provider run
except for the warm's own billed provider call.

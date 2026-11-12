# ADR-0188: Content-addressed instruction store and prompt layout v3

- **Status:** Accepted
- **Date:** 2026-11-12
- **Depends on:** ADR-0170 (tool error contract), ADR-0173 (cache-first
  prompt sections), ADR-0187 (same-host durable runtime state patterns)
- **Supersedes:** the three-section `core/project/volatile` prompt layout for
  new sessions; layout v2 remains selectable for rollback

## Context

The cache-first prompt work split the system prompt into deterministic
`core`, `project`, and `volatile` sections. That was necessary but not
sufficient. Three problems still moved bytes ahead of the conversation:

1. `start_inner` passed the newest user prompt as `objective`, and
   `role_system_prompt` wrote `Objective: {objective}` into the volatile
   system section. A follow-up prompt therefore changed a system message that
   preceded the entire history.
2. The memory brief was rebuilt into the same volatile section on every run.
   A memory update invalidated the provider prefix.
3. The stable prefix was regenerated from source strings on every call. There
   was no content-addressed identity guaranteeing byte identity across calls
   or sessions, so a formatting/source/skill drift could silently move the
   prefix even when the inputs were semantically unchanged.

The instruction store must make the head prefix **materialized bytes**, not a
fresh rendering. The current prompt must be only a user message; for
task-scoped agents the task objective may be a run-stable head section. Memory
must be tail-only. No approval, sandbox, path, write-scope, Verifier, merge,
capacity, or budget gate changes.

## Decision

### 1. Blob and state tables

Two SQLite tables own the store:

```sql
instruction_blobs (
  hash       TEXT PRIMARY KEY,   -- sha256(text), 64 lowercase hex chars
  text       TEXT NOT NULL,
  created_at TEXT NOT NULL
);

session_instruction_state (
  session_id TEXT NOT NULL,      -- Leader session id, or <session>#<agent>
  key        TEXT NOT NULL,      -- deterministic instruction key
  hash       TEXT NOT NULL,      -- references instruction_blobs.hash
  ordinal    INTEGER NOT NULL,
  tail       INTEGER NOT NULL,   -- 0 = head system section, 1 = tail section
  updated_at TEXT NOT NULL,
  PRIMARY KEY (session_id, key)
);
```

`text` is prompt material, never cache telemetry. `put_instruction_blob`
deduplicates by `sha256`; when a row with the same hash exists but its text
differs, the body is repaired from the verified text because the hash is
computed from that text by the store. Sections are bounded both in core
(per-key and aggregate) and at the store boundary
(`MAX_INSTRUCTION_BLOB_BYTES = 256 KiB`). Oversized input fails closed; it is
never truncated.

The state table has no foreign key to `sessions` on purpose. Task-scoped
agents use a scoped state id `<session_id>#<agent_id>` so role charters and
task objectives stay isolated while the key vocabulary and `(session_id, key)`
primary key remain exactly as specified. The Leader uses the plain session id.
Deletion/repair matches the exact id plus the scoped prefix, parameterized.

### 2. Deterministic key order

| ordinal | key | volatility | content |
|---:|---|---|---|
| 10 | `core/charter` | stable | role charter, security/output contract, project conventions |
| 20 | `core/recipes` | stable | static tool recipes for role/profile |
| 30 | `project/sources` | semi-stable | indexed source brief |
| 40 | `project/skills` | semi-stable | skill catalog |
| 50 | `session/context` | session-stable | agent identity, project root, worktree, branch, profile, scope, session goal |
| 60 | `run/task` | run-stable | task objective for Worker/Labour/Verifier/Scout only |
| 90 | `tail/memory` | per-run tail | retrieved memory brief |

Ordinals are spaced by ten for future insertion. A duplicate key, unknown key,
or drifted ordinal in a replay event fails closed. Empty optional sections
(`core/recipes`, project sections, `run/task`, `tail/memory`) are omitted from
the ordered state; `session/context` is always present.

`session/context` is derived from `Session.goal`, never from the current
prompt. A Leader follow-up has no `run/task`; the newest prompt is only the
final user message. A task-scoped agent gets a `run/task` section because its
objective is stable for that scoped run and is not a general project
convention.

### 3. Materialization and hash verification

`vp_core::instruction::materialize`:

1. validates keys, uniqueness, per-section size, aggregate head/tail size;
2. loads the current ordered state for the scope;
3. for **pinned** keys (`core/charter`, `core/recipes`, `session/context`,
   `run/task`) reuses the stored blob while `sha256(text) == hash`; a later
   generator change does not rewrite an existing session;
4. for **regenerated** keys (project sections and tail memory) uses the fresh
   candidate, stores the new blob when the hash changed, and records the state
   change;
5. if a referenced blob is missing or corrupt, rebuilds from the current
   deterministic generator, stores the new blob, and returns a bounded
   `InstructionRebuild` diagnostic with `missing_blob`, `hash_mismatch`, or
   `missing_state`;
6. verifies the final text hash again and fails closed rather than ever
   passing unverified text to a provider;
7. writes the ordered state atomically only when it changed.

`session.cache.plan` uses `instruction::preview`, which performs the same
validation without touching blobs, state, events, worktrees, or providers.

### 4. Events and telemetry

On a state change the agent emits `context.instruction` with `state_id`,
`layout`, and the complete ordered `{key, hash, ordinal, tail}` list (no
text). Each rebuild emits a bounded `context.instruction.rebuilt` diagnostic.
A materialization failure emits `context.instruction.failed` with the shared
tool-failure object from ADR-0170 (`RESOURCE_LIMIT`, `HASH_MISMATCH`,
`INVALID_ARGUMENT`, or `INTERNAL_ERROR`) and then aborts the run before any
provider call.

`context.prompt_prefix` v3 adds:

- `prompt_layout` (and `version` now carries the layout value);
- `head_sections` / `tail_sections` ordered `PromptSectionRef` rows;
- `stable_prefix_hash` (exact ordered head text);
- `volatile_hash` (session/run head sections plus tail memory);
- `prefix_hash` (layout + model + tools + ordered key/hash identity);
- `cache_epoch` / `cache_epoch_source: "l1_placeholder"` /
  `cache_epoch_placeholder: true` — L4 owns the finalized epoch;
- `section_chars` (key -> Unicode scalar count);
- `provider_tail_system`, `instruction_state_id`.

No event contains full prompt text. Existing `core_hash`, `project_hash`,
`tools_hash`, and `stable_system_hash` remain for older clients; in v3 the
`core`/`project` compatibility strings are joined views of the ordered head.

### 5. Provider layout

- **OpenAI-compatible / DeepSeek**: head sections are system messages in
  ordinal order, then the exact conversation, then `tail/memory` as a trailing
  system message. No `cache_control` is added. Tail memory is a small suffix
  cache miss, never a change to an earlier message.
- **Anthropic**: head sections remain the top-level `system` blocks with the
  existing breakpoint plan (last core and last project section carry the
  explicit kinds so the four-breakpoint cap still holds). Because Anthropic's
  system field is not mid-history, `tail/memory` becomes one stable user
  message prefixed `[memory]` after the conversation. The request records
  `provider_tail_system=false`.
- Explicit `session.cache.warm` sends head sections plus the 1-token warm user
  message only. It preserves verified tail state but does not send memory.

This ADR does not add provider-native mid-history system capability probes and
does not make an automatic provider call. It records a deterministic provider
contract; L2 owns canonical message serialization and append-only continuity
telemetry.

### 6. Layout selection, legacy path, and rollback

`CachePolicy.prompt_layout` is the project default for newly created sessions
(`2` or `3`, default `3`; invalid values fail validation). `Session` persists
its own `prompt_layout`; existing sessions are migrated to `2` so an upgrade
never changes their prompt bytes. New sessions inherit the project default.

`session.prompt_layout.set` / `vp prompt-layout <session> 2|3` overrides one
session with an event (`session.prompt_layout`). `replay` repairs the session
projection from `session.created` / `session.prompt_layout`. Switching back to
v3 preserves the existing instruction state and blobs; rollback is not
destructive. `project.prompt_layout.set` (also exposed through
`vp cache-policy-set --prompt-layout`) changes only the default for future
sessions.

Layout v2 continues to build the exact historical combined system prompt and
emits no instruction state. Layout v3's `build_prompt_sections` is a thin
assembler over the instruction store.

### 7. Inspection surfaces

- RPC `session.instructions` (`{session_id, agent_id?}`) returns bounded
  ordered metadata: state id, role, agent id, section key/hash/ordinal/tail,
  character count, and whether the referenced blob still verifies. It never
  returns text and never triggers generation.
- CLI `vp instructions <session> [--agent <id>]` renders the ordered table.
- Stage cache details show the head/tail section list, short hashes, layout,
  and a missing-blob marker. No prompt text is displayed.
- The only exact-byte inspection path is `#[cfg(test)]` in
  `vp_core::instruction`, used to assert byte identity.

### 8. Replay, rebuild, and bundles

`context.instruction` events reconstruct `session_instruction_state` during
`session.replay`, `repair`, `rebuild`, and `db-rebuild` (which restores event
mirrors then repairs projections). Blob bodies are not event-sourced: the
event carries hashes, and a missing body is rebuilt deterministically on the
next materialization with a `context.instruction.rebuilt` diagnostic. This is
an explicit repair path, not silent text substitution.

`SessionBundle` gains additive `instruction_blobs` and `instructions` fields
with `serde(default)`. Export includes verified blobs for referenced hashes.
Import is read-only: it rejects a blob whose `sha256(text)` does not match and
rejects a state row that references a missing blob, then preserves both for
inspection. Import never regenerates text or runs an agent.

## Consequences

- A follow-up prompt changes only the user message (plus, at most, a
  task-scoped `run/task` on a new run); the head bytes and `prefix_hash` stay
  identical.
- A memory update changes only `tail/memory` and `volatile_hash`; the head
  prefix and `prefix_hash` do not move.
- Two calls with the same stable inputs and scope produce byte-identical head
  sections even if the generator's project section is re-evaluated, because
  pinned sections are loaded from verified blobs and unchanged project hashes
  reuse the same blob.
- A source/skill edit legitimately moves the project component and therefore
  the head prefix (a real cache-epoch change; L4 finalizes attribution).
- Cost is one extra blob lookup per section per materialization and bounded
  SQLite writes only when the ordered state changes. No provider call is
  added.
- First-call cross-session provider hits are still provider-managed and are
  not claimed as guaranteed; L4/L5 own cross-session reuse telemetry and the
  live benchmark.

## Limits and non-goals

- Blob text is not encrypted at rest; it lives in the same local SQLite
  database as conversation messages.
- A deterministic rebuild after a code upgrade can produce different text for
  a pinned section whose blob was lost. The diagnostic records the old and new
  hashes; the runtime does not claim the original bytes are recoverable.
- No cross-host instruction sharing, distributed cache, automatic provider
  probe, hidden model routing, or automatic cache warm is introduced.
- L2 owns append-only message canonicalization, L3 owns tool bundle partition
  and freeze, L4 owns cache epochs/compaction/cross-session reuse, and L5 owns
  the full telemetry/GUI benchmark. This ADR only establishes stable
  instruction bytes, ordered metadata, tail memory, and the legacy/rollback
  path.
- Approval, Seatbelt, path-jail, write-scope, Verifier, merge, capacity, and
  budget gates are unchanged.

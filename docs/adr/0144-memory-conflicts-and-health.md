# ADR-0144: Deterministic memory conflicts and project memory health

- **Status:** Accepted
- **Date:** 2026-11-11

## Context

ADR-0142 made project memory a claim with provenance: lifecycle status,
evidence references, supersede-on-same-key, TTL verification, and deterministic
ranked recall. A project can still accumulate two *active* memories that cite
the same path or the same task but say different things. Nothing surfaced that
disagreement, and operators had no single view of memory evidence coverage,
verification debt, or conflict backlog.

Detecting semantic contradiction with an LLM or embeddings would add hidden
model routing, cost, and an opaque judgment. VerlinPark needs a narrower,
explainable review signal: deterministic scope overlap plus a body difference.

## Decision

1. **High-signal scope rules only.** A potential conflict exists when two
   active memories share an exact evidence reference and their normalized
   bodies differ.
   - `decision`, `convention`, and `fact` memories are grouped by exact `path`
     evidence.
   - `verification` and `task_result` memories are grouped by exact `task`
     evidence.
   - Stale, superseded, and archived rows are ignored. Detection is capped at
     5,000 active rows, 50 pairs per scope group, and 500 total pairs, with
     deterministic ordering by scope and memory id.
2. **Deterministic identity.** A conflict id is the first 32 hex characters of
   SHA-256 over project, scope kind/value, sorted memory ids, and both
   normalized body hashes. A resolved conflict with unchanged content stays
   resolved. Changed content produces a new id and therefore a new review row.
3. **Potential, not semantic, contradiction.** The human-readable reason says
   only that both active memories cite the same path/task and have different
   bodies. It never claims that one is false. Same-scope only; different-scope
   contradictions are out of scope. URL evidence is not fetched or checked.
4. **Detection never mutates memory.** `memory.conflict.scan` inserts new rows
   as `open` and emits one event-backed `memory.conflict.detected` per new row
   in the same transaction. Duplicate scans do not recreate rows, including
   resolved ones. The daemon scans hourly by default
   (`VERLINPARK_MEMORY_CONFLICT_SECS`, `0` disables) and never auto-resolves.
5. **Resolution is explicit and atomic.** The operator chooses `keep_a`,
   `keep_b`, `keep_both`, or `archive_both`.
   - `keep_a` / `keep_b` mark only the loser `superseded` with
     `superseded_by = winner.id` and emit `memory.superseded`.
   - `archive_both` marks both `archived` and emits `memory.updated` for each.
   - `keep_both` changes neither memory.
   The conflict row becomes `resolved` with `resolved_at`, `resolution`, and an
   optional note, and `memory.conflict.resolved` carries the full row. Memory
   mutations, the conflict update, and all events commit together.
   Resolving twice, cross-project ids, invalid actions, and vanished memories
   are rejected.
6. **Health is a deterministic projection.** `memory.health` reports total,
   status counts, pinned rows, active evidence coverage, verified rows,
   verification due (active, non-pinned rows whose `verify_after` is in the
   past), open conflicts, and a by-kind breakdown. It reads local rows only.
7. **Replay, repair, and bundles cover conflicts.** Replay/repair fold
   `memory.conflict.detected` and `memory.conflict.resolved` last-write-wins by
   conflict id. A `memory.deleted` event removes conflicts that reference the
   deleted memory from replay, matching the projection tombstone. `SessionBundle`
   carries `memory_conflicts` with `serde(default)`; old bundles decode as an
   empty list, and import/export validate the session and project. Deleting a
   memory deletes conflicts that reference it without a separate conflict event,
   because `memory.deleted` already removes the memory from replay.
8. **Operator surfaces only.** The daemon exposes `memory.conflicts`,
   `memory.conflict.scan`, `memory.conflict.resolve`, and `memory.health`.
   The CLI exposes `vp memory-conflicts`, `vp memory-conflict-scan`,
   `vp memory-conflict-resolve`, and `vp memory-health`. Manage Dash adds a
   Memory Health card next to the existing memory card. No agent-facing tool is
   added.

## Consequences

- Operators get a small, explainable review queue instead of a vague warning
  that memory “may disagree.” Every row names the scope, both claims, both
  bodies, and a deterministic reason.
- Detection is cheap and local. It does not call a provider, build an
  embedding index, or judge truth.
- Resolved decisions survive future scans until the underlying content
  changes, at which point a fresh conflict is appropriate.
- Conflicts are project-scoped review state. They stay out of recall and briefs;
  resolving one is the only way a lifecycle change happens.

## What is NOT claimed

- **No semantic contradiction detection.** Two different-scope claims can
  contradict without being detected. Same-scope, different-body is the whole
  rule.
- **No LLM or embedding judgment.** No hidden model routing, confidence score,
  or semantic similarity threshold is involved.
- **No cross-project conflicts.** Detection and health are project-scoped.
- **No URL or remote evidence verification.** Conflict detection compares
  bodies and local evidence references; it does not fetch or validate URLs.
- **No auto-resolution.** Even destructive-looking conflicts stay open until
  an operator chooses an action.
- **No safety change.** Conflicts do not bypass approval, sandbox, Verifier,
  merge, capacity, or budget gates.

## Alternatives considered

- **Embedding similarity:** could find paraphrased contradictions, but adds an
  opaque index, provider coupling, and false positives that are hard to
  explain.
- **LLM contradiction review:** broader semantic coverage, but hidden routing,
  cost, and an unauditable truth signal. ADR-0142 deliberately keeps memory
  deterministic.
- **Auto-supersede on same scope:** silently chooses a winner without operator
  intent and would turn detection into mutation.
- **Cross-project conflict graph:** useful eventually, but project memory
  recall, policy, and evidence are currently project-scoped.

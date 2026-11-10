# ADR-0142: Evidence-backed project memory v2

- **Status:** Accepted
- **Date:** 2026-11-11

## Context

ADR-0074 introduced project memory as an explicit, inspectable record, and
ADR-0078 added event-backed deletion, FTS5 recall, and opt-in retention. A
memory was still a free-floating note: kind, title, body, tags, author, and
timestamp. Recall ranked FTS hits mostly by recency, every `task_result` create
was skipped when the title existed, and stale knowledge could remain "active"
forever with no provenance or lifecycle.

VerlinPark's durable truth is events, task results, merged commits, and
Verifier verdicts — not chat transcripts. A memory that cannot point at
evidence is a claim, not a fact. It must be treated that way.

## Decision

1. **Memory is a claim with provenance.** `MemoryItem` gains `status`,
   `source`, `evidence`, `supersedes`, `superseded_by`, `pinned`, `use_count`,
   `last_used_at`, `last_verified_at`, `verify_after`, and `stale_reason`.
   Evidence entries are typed references: `commit`, `path`, `task`, `session`,
   `event`, or `url`. All fields are additive with `serde(default)`, so old
   events, bundles, and clients keep decoding.
2. **Deterministic retrieval, not embeddings.** A new `vp-core::memory` module
   ranks candidates with lexical relevance (FTS5 bm25 or LIKE fallback),
   structural boosts (exact task evidence, path-prefix overlap, department),
   kind/source weights, a 30-day recency half-life, observed usage, and a pinned
   boost. Stale candidates are multiplied by 0.25 and are only eligible when
   the caller opts in. Ties break by score, `created_at` desc, then `id` asc.
   No provider call, embedding, or hidden model routing is involved. Every
   match carries human-readable `reasons`.
3. **TTL plus explicit verification, not LLM truth judgments.**
   `memory.verify` checks commit ancestry with `git merge-base --is-ancestor`,
   path existence under the project root, and cheap task/session/event
   existence. URL and unsupported evidence are reported as `unchecked`, never
   silently treated as fact-checked. Success writes `memory.verified` and a new
   `verify_after`; failure writes `memory.stale` with a reason. `memory.sweep`
   marks expired active, non-pinned rows stale and never deletes or restores.
   The daemon sweep is opt-in per project (`MemoryPolicy.sweep_stale`) and
   controlled by `VERLINPARK_MEMORY_STALE_SECS` (default 3600, 0 disables).
4. **Observed usage instead of self-rating.** A memory is used when a brief
   selects it. `memory.used` increments `use_count` at most once per
   `(memory_id, session_id)` via the `memory_usage` table, so replay and
   repeated briefs are deterministic. Agents do not rate their own memory.
5. **Supersede and dedupe are deterministic and atomic.** A normalized
   (project, kind, title) key may have at most one active row. An exact
   same-key create with the same body, evidence, and source is idempotent.
   Different content inserts a new active row and marks the previous one
   `superseded`, emitting `memory.superseded` for the old full row and
   `memory.created` for the new row in one transaction. `dedupe=false` is the
   explicit escape hatch, and `MemoryPolicy.auto_supersede` (default true) can
   disable replacement.
6. **Task results carry evidence.** When a plan task completes, the
   orchestrator records a `task_result` memory with task, session, latest merged
   commit, and up to 20 changed paths from `git diff --name-only base..commit`.
   A retried task that completes again supersedes the previous result instead
   of being skipped; identical content stays idempotent. The policy's
   `task_result_verify_days` (default 90) sets the TTL.
7. **Bounded, inspectable briefs.** `memory.brief` builds its query from the
   session goal plus plan-task title, acceptance, department, and write scope,
   selects a deterministic character-budgeted brief
   (`MemoryPolicy.brief_max_chars`, default 4000), falls back to recent active
   memories when there are no lexical hits, and emits observed usage. Its
   compact format is `[kind·source] title (task T3, commit abc1234) · body`.
8. **Every lifecycle change is event-backed.** `memory.created`,
   `memory.updated`, `memory.verified`, `memory.stale`, `memory.superseded`,
   `memory.used`, and `memory.deleted` all carry the full updated
   `MemoryItem`. Replay/repair fold them last-write-wins, bundles round-trip
   the fields, and `db-rebuild` reconstructs them through the repair path.
9. **No new agent tool.** `remember`/`recall` keep working with source `agent`.
   Operator surfaces are RPC/CLI only: `memory.search`, `memory.brief`,
   `memory.update`, `memory.verify`, `memory.sweep`, `memory.stats` and the
   corresponding `vp memory-*` commands.

## Consequences

- Recall can explain itself: every selected item reports lexical and
  structural reasons, provenance is visible, and pins are user intent rather
  than model confidence.
- Stale knowledge is visible and reversible (`memory.verify` or
  `memory.update`), not silently trusted. Retention (`enabled`) and staleness
  (`sweep_stale`) are separate opt-ins.
- Task-result memories supersede on retry, so the latest completed attempt is
  the active claim while the prior attempt stays in event history.
- Memory rows grow wider and update/use events reindex their FTS row. Ranking
  is cheap and local; no embedding index or provider cost is added.
- A project-scoped create key means the same title in two projects remains
  distinct, and intentionally duplicated active rows require `dedupe=false`.

## What is NOT claimed

- **No semantic recall.** Ranking is lexical, structural, and behavioural;
  there are no embeddings or vector search.
- **No automatic fact-checking.** Verification checks local evidence
  references (commit ancestry, paths, cheap ids). It does not prove a claim is
  true, and URL evidence is left unchecked.
- **No cross-project memory.** Recall remains project-scoped.
- **No self-rating loop.** Usage is observed selection, not an agent quality
  score.
- **No deletion by sweep.** Sweeps only mark stale; deletion stays explicit
  through `memory.delete` / `memory.prune`.
- **No chat transcript mining.** Memories are explicit records with
  provenance; transcripts are not scanned into memory.

## Alternatives considered

- **Embedding store:** better fuzzy recall, but adds provider/model coupling,
  cost, and an opaque index that conflicts with deterministic Rust ranking.
- **LLM verification:** could judge semantics, but adds hidden routing,
  cost, and an unauditable "truth" signal. Explicit evidence checks are
  narrower but honest.
- **Agent self-rating:** easy engagement signal, but rewards confident
  writing rather than verified knowledge.
- **Append-only duplicates:** preserves every note but leaves conflicting
  active claims; supersede keeps one active row while retaining history.

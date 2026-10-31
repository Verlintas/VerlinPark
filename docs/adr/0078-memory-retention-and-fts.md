# ADR-0078: Memory retention and full-text recall

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

Project memory was append-only and searched with `LIKE`. It grew forever and
could not be corrected or pruned, and substring scans do not scale.

## Decision

1. **Event-backed deletion.** `memory.delete` emits one `memory.deleted` event per
   removed item with the full `MemoryItem` payload. Event+projection deletion is
   transactional and the JSONL mirror is appended after commit.
2. **FTS5 external content.** `memories_fts` indexes `title`, `body`, and `tags`
   with insert/delete/update triggers against `memories`. Existing databases are
   backfilled once through `INSERT INTO memories_fts(memories_fts)
   VALUES('rebuild')`, tracked in `store_meta` so restarts do not rebuild every
   time.
3. **FTS first, LIKE fallback.** Recall queries are tokenized into quoted FTS
   terms and ranked by `bm25` plus recency. If FTS returns nothing or fails, the
   existing `LIKE` scan runs so CJK text and substring queries still work.
4. **Manual retention.** `memory.prune` supports `older_than_days` and
   `keep_latest` (delete when either matches), requires at least one policy, and
   offers `dry_run`. CLI exposes `vp memory-prune`; Manage Dash exposes per-item
   delete and a confirmed "older than 30 days" prune.
5. **Replay/repair ordering.** Replay applies `memory.created` and
   `memory.deleted` in event order. Repair rebuilds the projection the same way,
   so a deleted memory cannot come back after `session.replay --repair` or
   `db-rebuild`.
6. **Opt-in scheduled retention.** `project_memory_policies` stores `enabled`,
   `max_items`, and `max_age_days`. The daemon sweeps enabled projects every
   `VERLINPARK_MEMORY_SWEEP_SECS` (default 3600; `0` disables) through the same
   event-backed prune path. Policies are off by default, require at least one
   limit to enable, and bound limits at 100,000 items / 36,500 days. CLI exposes
   `vp memory-policy` / `vp memory-policy-set`; Manage Dash edits the same policy.
7. **No semantic recall.** Embeddings and vector search are not implemented.

## Consequences

- Memory can be corrected and kept bounded without losing event history, either
  manually or through an opt-in hourly sweep.
- FTS indexes add write overhead and a rebuild on first open of an old database.
- CJK search relies on the LIKE fallback; unicode61 does not segment every CJK
  phrase.
- A deleted memory still exists in the event log and exported bundles, which is
  intentional audit history.
- `store_meta` is now the migration marker table for future schema changes.

## Alternatives considered

- **Hard delete without events:** simpler, but replay/repair would resurrect the
  row and the audit trail would lie.
- **Automatic pruning on create:** risks deleting knowledge without user intent.
- **Default-on background retention:** could delete memory on first launch after
  an upgrade; the policy is opt-in and explicit.
- **Embedding search:** better semantics, but needs an embedding provider, cost
  controls, and a vector index that do not exist yet.

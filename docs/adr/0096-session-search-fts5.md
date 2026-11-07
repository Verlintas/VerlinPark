# ADR-0096: FTS5 index for session search

- **Status:** Accepted
- **Date:** 2026-11-08

## Context

`session.search` scanned `messages.content` and `events.payload` with `LIKE
'%query%'`. This preserved substring matching (including Chinese substrings) but
had no index, scanned every message and event in every session, and could not
use token ranking. Project memory already uses an FTS5 external-content index
with a LIKE fallback, so the store already requires FTS5.

## Decision

1. **External-content FTS5 indexes.** Add `messages_fts` over `messages.content`
   and `tool_calls`, and `events_fts` over `events.type` and `events.payload`.
   Triggers keep them in sync on insert/update/delete; both tables use
   `unicode61`.
2. **One-time rebuild for existing databases.** `store_meta.search_fts` marks
   migration. On the first open of an older database, both indexes are rebuilt
   from existing rows with the FTS5 `rebuild` command.
3. **FTS first, LIKE fallback.** ASCII queries use the FTS index with quoted,
   prefix-matched terms (`"term"*`), project filtering, and newest-first order.
   If FTS returns no hits, the existing LIKE scan runs, preserving substring
   matches such as `param` inside `hyperparameterization`. Non-ASCII queries
   (including CJK) go directly to LIKE because `unicode61` groups CJK runs into
   a single token and would miss common substring searches.
4. **Snippets remain plain text.** The existing snippet window is kept; it now
   anchors on the first whitespace-separated term found in the text. No HTML
   markers or BM25 ordering are exposed; results stay newest-first for UI
   compatibility and deterministic tests.
5. **No semantic search.** Embeddings, stemming, and cross-session relevance
   ranking are out of scope.

## Consequences

- Search scales with the FTS index instead of scanning every row for normal
  word/prefix queries, while substring and CJK behavior remain available.
- The events index includes raw JSON payloads, so JSON keys and values are
  searchable; event hits still expose the original payload snippet.
- If FTS returns any hits, a substring-only match in a different row can be
  omitted. This is the deliberate recall/precision trade-off; LIKE still runs
  when FTS is empty.
- The database grows with two FTS indexes and their triggers. Index rebuild on
  the first open of a large existing database can take noticeable time once.
- FTS5 is already required by project memory, so this adds no new extension
  requirement.

## Alternatives considered

- **Keep LIKE only:** simple but unindexed and slow on long sessions.
- **Union FTS and LIKE always:** preserves maximal recall but doubles the scan
  cost and complicates deduplication/order.
- **BM25 relevance order:** would change UI ordering and existing tests; can be
  added later as a separate sort mode.
- **Semantic/embedding search:** out of scope and adds model/provider coupling.

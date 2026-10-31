# ADR-0062: Session search

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

Sessions, agent messages, tool output, and event payloads accumulated locally,
but users could only inspect one session timeline at a time. Finding an earlier
decision or tool result required scrolling.

## Decision

1. **One query surface.** `session.search` / `vp search` searches stored message
   content and raw event payloads across all sessions, optionally scoped to one
   project.
2. **Deterministic results.** Newest first, capped by a caller limit (default
   50, max 100), with a `truncated` flag when more rows exist.
3. **Literal matching.** User `%`, `_`, and `\` are escaped; the query is a
   literal substring, not a SQL pattern.
4. **Snippets.** Return a short context window around the first match, with
   control characters flattened. Event hits include the event kind and seq;
   message hits include the role.
5. **App entry point.** `Cmd-F` in the project stage opens a search sheet with
   an optional “This project” scope. Selecting a hit switches project/session.
6. **No FTS yet.** v0.1 uses `LIKE` scans over the existing tables. There is no
   FTS index, ranking, stemming, or cross-field relevance score.

## Consequences

- Search is useful at current local data volumes without a schema migration.
- Large event payloads and non-ASCII case folding may produce a less precise
  snippet; matching is case-insensitive for ASCII only.
- The API can later move to FTS5 without changing the UI contract.
- Search results are read-only and do not emit events.

## Alternatives considered

- **SQLite FTS5:** better ranking and speed, but adds projection/index rebuild
  and repair obligations before the data model is stable.
- **Client-side scan:** forces the app to download every message/event.
- **Regex/full query language:** surprising and harder to escape safely.

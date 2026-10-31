# ADR-0074: Explicit project memory

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

Agents could publish artifacts and exchange mailbox messages, but decisions and
facts did not survive the session in a form later agents could recall. Users had
to repeat architecture constraints and project conventions in every prompt.

## Decision

1. **Explicit, inspectable memory.** A memory is a typed record with kind,
   title, body, tags, creating session/agent, and timestamp. It is not a hidden
   model memory or an embedding store.
2. **Session-event-backed.** `memory.created` events live in the creating
   session; the projection is rebuilt/repaired with that session. Recall queries
   all sessions in a project, so memory is shared without a separate global event
   log.
3. **Tools.** `remember` creates a record; `recall` searches by query/kind.
   `remember` is available to Leader/Manager/Worker/Labour, not Verifier;
   `recall` is read-only and available to every role.
4. **RPC and UI.** `memory.list` / `memory.create`, `vp memory` / `vp remember`,
   and Manage Dash → Project memory (search, list, add from the selected
   session).
5. **Replay and bundles.** `session.replay` compares memory projections,
   `--repair` restores them, and bundles export/import their memories.
6. **Bounded content.** Kind ≤40, title ≤200, body ≤20,000, tags ≤10 of ≤40
   characters. Recall returns at most 20 records and clips long bodies.
7. **No destructive memory API yet.** Memories are append-only; there is no
   forget/delete or retention policy.

## Consequences

- Agents can persist architecture decisions, facts, conventions, and sources
  across sessions and recall them explicitly.
- Memory is project-wide for recall but tied to a creating session for replay;
  deleting/importing a session affects its memories.
- Duplicate or conflicting notes are possible; no merge/dedup policy exists.
- Search is a LIKE scan over title/body/tags, not semantic search or FTS.
- Imported archives without memory events simply have no memories.

## Alternatives considered

- **Mailbox only:** pull-based and addressed to one agent; not durable knowledge.
- **Artifacts only:** good for large outputs, awkward for short decisions.
- **Embedding store:** useful later, but opaque and adds model/provider coupling.
- **Global project event stream:** cleaner for cross-session deletion, but a much
  larger projection change than this slice.

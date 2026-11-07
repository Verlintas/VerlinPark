# ADR-0106: Recursive fork tree

- **Status:** Accepted
- **Date:** 2026-11-08

## Context

ADR-0104 exposed only the direct parent, siblings, and children of a session.
Fork-of-fork chains were not visible, so users could not inspect a branch family
without opening sessions one by one. The lineage data is already fully
event-backed, so the tree can be derived without a new projection.

## Decision

1. **API.** `session.fork.tree` returns a nested `SessionForkTreeNode` with the
   session, checkpoint/commit metadata, and children. `total` counts all nodes
   including the root; `truncated` is set when a safety cap is hit.
2. **Bounds.** Depth is capped at 32 and total nodes at 200. The API marks
   `truncated` instead of silently returning a partial-looking tree.
3. **Sources.** Children come from `session.forked` events and are resolved to
   sessions through the same lineage entry helper as `session.lineage`; missing
   sessions are skipped. Nodes are sorted by session creation time.
4. **Interfaces.** CLI `vp fork-tree`, Manage Dash "Tree" sheet with recursive
   indentation and Open actions. The existing direct-lineage card remains for
   quick parent/sibling actions.
5. **Read-only.** The tree never mutates sessions, branches, worktrees, or
   checkpoints. It is not a merge/planning graph; it only shows checkpoint
   descendants.

## Consequences

- Users can inspect multi-level fork families without repeated navigation.
- Depth/total caps keep the RPC and UI bounded.
- Missing/deleted sessions are omitted from the tree; the source event remains
  in the event log.
- This is a derived view, not a persisted projection; `session.replay` is not
  affected.

## Alternatives considered

- **Persist a fork tree table:** redundant with events and introduces drift.
- **Unbounded recursion:** risks pathological payloads if many forks exist.
- **Only direct children:** the existing lineage card already covers that.

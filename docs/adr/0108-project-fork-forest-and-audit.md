# ADR-0108: Project fork forest, integrity audit, and checkpoint stats

- **Status:** Accepted
- **Date:** 2026-11-08

## Context

Per-session fork trees and lineage are useful for one branch family, but users
need a project-wide view and a way to detect broken lineage/ref state after
crashes, manual Git edits, or retention sweeps. Event-derived reads are cheap
but can silently omit children when a source `session.forked` event is missing.

## Decision

1. **Project forest.** `project.fork.forest` builds a forest from
   `session.created.forked_from_session_id` parent metadata. Sessions whose
   parent is missing become roots instead of disappearing. Depth is capped at
   32 and total nodes at 500; `truncated` is explicit. The result includes the
   active checkpoint count across project sessions.
2. **Integrity audit.** `project.fork.audit` reports:
   - missing parent sessions;
   - missing source `session.forked` links;
   - source fork events whose child session is absent;
   - missing checkpoint commits;
   - dirty checkpoint events missing their snapshot ref;
   - tombstoned checkpoints that still have a snapshot ref.
   Repairable issues are marked explicitly.
3. **Repair.** `project.fork.repair` backfills missing source fork links as new
   `session.forked` events (never rewriting history), recreates missing
   checkpoint refs for existing commits, and removes stale refs for tombstoned
   checkpoints. Unrepairable issues are counted and detailed.
4. **Checkpoint stats.** `session.checkpoint.stats` reports active/dirty
   checkpoint counts, snapshot refs, commit-object bytes, unique changed-blob
   bytes, and unique blob count. The byte values are approximate content sizes
   from Git object metadata, not packed on-disk usage.
5. **Interfaces.** CLI `vp fork-forest` / `vp fork-audit` / `vp fork-repair` /
   `vp checkpoint-stats`, and a Manage Dash project forest sheet with audit and
   repair controls.

## Consequences

- Missing fork links no longer make sessions invisible in the project view.
- Audit/repair is deterministic and event-preserving; repairs append evidence.
- Repair only touches checkpoint refs and missing fork-link events; it never
  deletes sessions, commits, or forks.
- Byte stats are approximate and can differ from `du`/pack size.
- The forest is built from metadata, while per-session `session.fork.tree`
  remains event-derived; a missing event is visible as an audit issue.

## Alternatives considered

- **Persist a forest projection:** duplicates event truth and creates drift.
- **Silently skip malformed sessions:** loses visibility; rejected.
- **Rewrite/delete events to repair:** violates append-only event sourcing.
- **Report exact packed size:** requires repository-wide pack accounting and is
  unstable across GC; approximate object bytes are more useful per checkpoint.

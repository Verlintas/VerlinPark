# ADR-0051: Event mirror verification and restore

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

SQLite is the authoritative store, but every committed event is also appended to
`<data>/sessions/<session>/events.jsonl` on a best-effort basis. There was no way
to tell whether the mirror was complete, and no recovery path if the `events`
table was damaged while the JSONL file remained intact.

A full from-scratch database rebuild is still not implemented because project
metadata and worktrees are not event-sourced. Restoring a session's event table
from its mirror is a smaller, well-defined recovery step.

## Decision

1. **Exact comparison.** `session.log_status` / `vp log-status` compares the
   mirror and database by sequence, type, payload, and timestamp. The result
   reports both counts and a human-readable detail.
2. **Restore contract.** `session.restore_log` / `vp restore-log` requires:
   - a readable mirror
   - at least one event
   - every event belongs to the requested session
   - sequence numbers start at 1 and are contiguous
3. **Transactional replacement.** The `events` table for that session is
   replaced in one transaction, then `session.rebuilt` appends a fresh event and
   rebuilds all covered projections (status, usage, plan, todos, messages,
   agents, queue, usage ledger, mailbox, artifacts, approvals).
4. **Not a full DB rebuild.** Project rows, session worktree directories, and
   content-addressed blobs must already exist. The command repairs the event
   table and session projections; it does not recreate projects or checkouts.
5. **Failure is explicit.** A missing/empty/non-contiguous mirror returns an
   error instead of silently restoring a partial history.
6. **Mirror remains best-effort.** A normal event commit does not fail if the
   mirror append fails. `log-status` exposes that drift; `restore-log` uses the
   mirror only when explicitly requested.

## Consequences

- A damaged `events` table can be recovered from the JSONL mirror for an
  existing session.
- Operators can detect mirror drift before relying on it.
- Restoring appends a `session.rebuilt` event, so the mirror and database should
  match after a successful restore.
- This does not close the full from-scratch database rebuild gap, and the docs
  must keep saying so.

## Alternatives considered

- **Rebuild the entire database from mirrors:** blocked on project metadata and
  worktree state not being event-sourced.
- **Trust the mirror implicitly on startup:** could restore a partial history.
- **Do nothing:** leaves a known recovery hole.

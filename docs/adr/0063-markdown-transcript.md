# ADR-0063: Markdown transcript export

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

Session bundles are recovery artifacts: they preserve events, projections, and
hashes, but they are not convenient to read or share. Users needed a portable
human-readable record of a session.

## Decision

1. **Explicit export.** `session.transcript` / `vp transcript` builds Markdown
   from the stored session, project, team plan, and messages.
2. **Contents.** Header metadata (status, branch, worktree, tokens, cost), the
   validated plan by department, and the conversation in timestamp order.
   Assistant tool calls are listed with clipped arguments.
3. **Bounded cells.** Each message is clipped at 8,000 characters and each tool
   argument at 1,000, with an explicit truncation marker. The export does not
   include full event payloads.
4. **App save panel.** The session menu has **Export Transcript…**, which opens
   an `NSSavePanel` and writes UTF-8 Markdown.
5. **No mutation.** Export is read-only and does not emit events.
6. **Not a bundle.** Transcripts are not importable and do not include worktree
   contents, artifacts, approvals, or the raw event log; use `session.export`
   for recovery.

## Consequences

- Users can attach a readable session record to a review or issue.
- Very large tool outputs are intentionally shortened; the bundle remains the
  lossless source.
- Secret redaction relies on the stored conversation already excluding secrets;
  the export adds no extra redaction pass.
- Event-level detail (queue transitions, capacity waits) is not in the
  transcript.

## Alternatives considered

- **Only bundle export:** lossless but unreadable and large.
- **HTML/PDF:** more presentation work and a larger dependency surface.
- **Export all events:** noisy and dominated by internal bookkeeping.

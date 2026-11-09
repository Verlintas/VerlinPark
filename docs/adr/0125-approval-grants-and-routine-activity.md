# ADR 0125: Path-scoped approval grants and quiet routine tool activity

- Status: Accepted
- Date: 2026-11-09
- Area: Permissions / UI

## Context

Smart approval already allowed ordinary workspace writes and only asked for
host-level or destructive operations, but shell commands that used
`$(...)` substitutions were conservatively classified as host state. Common
read-only checks such as `$(cat file)`, `$(git rev-parse HEAD)`, or
`$(wc -c < file)` therefore produced approval prompts during verification.

The conversation transcript also rendered every file read, write, grep, todo,
fetch, and mailbox operation as a tool row. These are useful audit events but
low-signal conversation content.

Approvals had only Allow/Deny. A user who trusted a path had to approve every
subsequent operation individually.

## Decision

1. **Classify substitutions instead of blanket escalation.** `$()` and
   backtick substitutions are parsed and their inner commands run through the
   same deterministic shell assessment. Safe read-only substitutions remain at
   workspace risk; a dangerous inner command still escalates to host state.
2. **Approval grants are project-scoped SQLite records.**
   `approval_grants` stores tool, path prefix, include-children, creator, and
   timestamp.
3. **Grant matching is explicit.** A tool call matches when:
   - the tool equals the grant tool (or the grant is `*`), and
   - an explicit path field (`path`, `file`, `file_path`, `target`,
     `destination`, `workdir`) equals the prefix, or starts with
     `prefix/` when `include_children` is set.
   Shell commands and opaque JSON without a path are not covered by a path
   grant; they continue through normal risk assessment.
4. **Always allow from the approval card.** A pending approval with an
   inspectable path offers “This file” and “This folder and subfolders”. The
   grant is created first, then the approval is resolved as approved so the
   waiting agent can continue.
5. **Grants are manageable.** Settings → General lists grants and allows
   removal. RPCs: `approval.grants`, `approval.grant.create`,
   `approval.grant.delete`.
6. **Routine tool activity is not conversation.** `read_file`, `list_dir`,
   `grep`, `write_file`, `edit_file`, `todo_write`, `fetch_url`, `remember`,
   `recall`, `coordinate`, `team_status`, mailbox/artifact operations, and
   successful routine use no longer create transcript rows. Errors still
   appear. All events remain in the event stream for audit, replay, and Ops.

## Consequences

- Routine verification commands stop asking for approval when they are
  composed of safe workspace/read-only substitutions.
- Users can grant a file or folder once; the grant survives restarts.
- A grant is intentionally path-prefix based, not a shell command allowlist.
  It does not broaden the write sandbox.
- The conversation is quieter; operators who need full detail use events,
  Preview Trace, or the audit ledger.

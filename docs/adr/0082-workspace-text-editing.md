# ADR-0082: Workspace text editing

- **Status:** Accepted
- **Date:** 2026-11-01

## Context

The read-only file browser exposed the session worktree but forced users back to
the Console to make a small edit. A blind editor with no path or conflict
protection would create a second write path and could race agents.

## Decision

1. **User-initiated writes only.** `workspace.write` is reachable through the
   desktop UI and CLI, never through an agent tool. Agent writes still go through
   the worktree tools and approvals.
2. **Same path jail.** Writes use relative paths, reject `..`/absolute paths and
   `.git`, canonicalize existing files, canonicalize the parent for new files,
   and reject symlink escapes.
3. **Conflicts are explicit.** `workspace.file` returns a SHA-256 of the loaded
   bytes and `editable`. `workspace.write` accepts `expected_sha256`; a mismatch
   fails with “file changed on disk; reload before saving” instead of silently
   overwriting.
4. **Atomic replacement.** Content is written to a temporary file in the same
   directory, synced, permissions are copied from the existing file, and the
   temporary file is renamed over the target.
5. **Bounded edits.** Text files are capped at 512 KiB, matching the browser.
   Binary, truncated, or oversized files are not editable.
6. **Audit trail.** A successful save appends `workspace.file_saved` with path,
   size, and SHA-256 to the session event stream.
7. **UI guardrails.** The Files tab tracks a local draft, shows a modified badge
   and save status, requires confirmation before discarding a dirty draft, and
   refreshes the session diff after saving.

## Consequences

- Small fixes no longer require opening a Console.
- Concurrent agent edits are detected on save rather than merged; the user must
  reload and reapply.
- Save events are audit events, not a content projection; file contents live in
  the Git worktree.
- There is no multi-file save, autosave, formatting, or language server support.

## Alternatives considered

- **Direct Swift file writes:** bypasses the daemon path jail and audit trail.
- **Last-write-wins:** simpler, but silently destroys agent or external changes.
- **Editable binary/hex:** larger UI and risk surface; deferred.

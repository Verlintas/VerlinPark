# ADR-0081: Read-only workspace file browser

- **Status:** Accepted
- **Date:** 2026-11-01

## Context

The Preview showed diffs, plans, queue, mailbox, cost, trace, and console, but the
user could not inspect the files in a session worktree without a shell command.
An editor or arbitrary filesystem API would add a second write path.

## Decision

1. **Session worktree only.** `workspace.tree` and `workspace.file` resolve paths
   inside the selected session's worktree; imported archives and sessions without
   a worktree are rejected.
2. **Read-only API.** There is no write/delete/rename RPC and no editor. File
   changes still go through agents or the sandboxed Console.
3. **Path jail.** Paths must be relative, must not contain `..`, are
   canonicalized, must remain under the canonical worktree root, and must not
   target `.git`.
4. **Bounded listing.** The tree walk follows `.gitignore`, skips `.git`, caps at
   5,000 entries and depth 20, and marks truncation. Directories sort before
   files.
5. **Bounded reads.** Text reads cap at 512 KiB and mark truncation. Binary files
   return a 1 KiB hex/ASCII preview with `binary: true` instead of raw bytes.
6. **Surfaces.** Preview has a Files tab with filter, file list, metadata, and a
   read-only monospaced viewer. CLI exposes `vp files` and `vp file`.
7. **No syntax highlighting or editing.** Those need a real editor model and a
   write/approval path.

## Consequences

- Users can inspect the exact files an agent is changing without using the
  Console.
- The file list respects ignore rules, so ignored build output may not appear.
- Binary and large files are previews, not downloads.
- The daemon still owns all reads; the Swift app never touches the worktree
  directly.

## Alternatives considered

- **Editor with save:** would create a second write path outside agent approvals
  and the shell sandbox.
- **Project-root browser:** would expose files outside the session worktree and
  weaken worktree isolation.
- **Raw binary response:** large and unsafe for the UI; hex preview is bounded.

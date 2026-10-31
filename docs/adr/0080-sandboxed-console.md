# ADR-0080: Sandboxed line-oriented console

- **Status:** Accepted
- **Date:** 2026-11-01

## Context

VerlinPark had agent shell execution but no way for the user to run a manual
command in a session worktree without asking an agent. A local `Process` in the
Swift app would bypass the daemon sandbox and state ownership.

## Decision

1. **Daemon-owned console.** `console.open` creates a `/bin/sh -s` process in the
   session worktree under the same `sandboxed_command` builder used by the agent
   shell tool. Writes are confined to the worktree, Git metadata, and console
   scratch; the project `ShellPolicy` still applies.
2. **Line-oriented transport.** Input is written to stdin; stdout/stderr are
   streamed back as sequenced chunks over `console.read`. This is not a PTY:
   there is no terminal size, raw mode, or cursor control.
3. **One console per session.** Opening a console replaces an existing console
   for that session. Closing kills the process group.
4. **Bounded buffers.** Output keeps the last 256 KiB and reports dropped chunks;
   a single write is capped at 64 KiB; reads return at most 200 chunks.
5. **Signals and lifecycle.** `console.signal` supports `int`, `term`, and `kill`.
   Process group cleanup runs on close, replacement, daemon shutdown, and `Drop`.
6. **Surfaces.** Preview has a Console tab with open/clear/interrupt/close and a
   command field; `vp console <session> "<command>"` runs one command and appends
   `exit` for CI and scripted use.
7. **Transient state.** Console output and status are not event-sourced, persisted
   in bundles, or replayed. The worktree and shell policy remain the security
   boundary.

## Consequences

- Users can run manual build/test/git commands in the same worktree as the agent.
- Full-screen programs (`vim`, `top`, interactive prompts) are not supported; a
  PTY and terminal emulator are future work.
- A console survives across agent runs until closed or the daemon exits, but not
  across daemon restarts.
- Long-running processes can keep the console open; closing kills the whole
  process group.

## Alternatives considered

- **Swift `Process`:** simple, but bypasses the daemon sandbox and violates the
  single-owner state model.
- **PTY + terminal emulator:** better UX, but requires a PTY transport, resize
  handling, ANSI rendering, and a substantially larger test matrix.
- **One-shot command runner only:** avoids lifecycle management, but loses `cd`,
  environment changes, and an interactive working session.

# ADR-0087: PTY terminal with SwiftTerm

- **Status:** Accepted
- **Date:** 2026-11-04

## Context

The Console is line-oriented and cannot run full-screen programs. A real ADE also
needs an interactive terminal with a prompt, cursor control, resize, and TUI
support.

## Decision

1. **Daemon-owned PTY.** `portable-pty` opens a PTY and runs
   `/usr/bin/sandbox-exec` with the same profile/env/cwd produced by
   `sandboxed_command`, then `/bin/sh -l`. The shell process is owned by the
   daemon, not the Swift app.
2. **Raw bytes over RPC.** `terminal.write` and `terminal.read` carry base64
   output chunks; `terminal.resize` updates `PtySize`; `terminal.close` kills the
   process group. The 512 KiB output ring reports dropped chunks.
3. **SwiftTerm renderer.** The macOS app uses SwiftTerm 1.10.1 through SwiftPM.
   The pinned version avoids the newer build-plugin/Metal-toolchain requirement
   while retaining the TerminalViewDelegate API used here.
4. **Separate surface.** Preview has a Terminal tab distinct from the line
   Console. The terminal keeps scrollback and input in the Swift view; the daemon
   remains the only process owner.
5. **CLI one-shot.** `vp terminal <session> "<command>"` opens a PTY, writes the
   command plus `exit`, streams decoded output, and closes. It is intended for
   automation and smoke tests, not a full interactive CLI terminal.
6. **Security boundary unchanged.** The PTY shell runs under the same Seatbelt
   write sandbox, ShellPolicy, environment allowlist, and process-group cleanup as
   the agent shell. Arbitrary reads and network under `allow` remain unconfined.

## Consequences

- Users can run `vim`, `top`, shells, and prompts in the session worktree.
- The terminal is not persisted across daemon restarts and its output is not
  event-sourced.
- The pinned SwiftTerm version is a maintenance item; newer versions may require
  a Metal toolchain or build-plugin approval.
- Terminal input is base64 to preserve control bytes; the UI handles encoding and
  rendering.

## Alternatives considered

- **Line Console only:** simpler, but rejects full-screen tools.
- **Local Process in Swift:** bypasses the daemon sandbox and state ownership.
- **Custom terminal emulator:** too much ANSI/cursor/input handling to maintain.

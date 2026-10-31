# ADR-0058: Local doctor diagnostics

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

Operational failures (missing worktrees, event mirror drift, corrupt SQLite,
unconfigured provider, missing sandbox) were only discoverable by reading logs or
running several commands. A single read-only diagnostic surface makes recovery
and support substantially easier without changing state.

## Decision

1. **Read-only report.** `server.doctor` / `vp doctor` returns a list of named
   checks with `ok` / `warn` / `error`, a detail string, and a top-level `ok`
   (true unless any check is `error`).
2. **Engine checks.** SQLite `PRAGMA integrity_check`, data-directory mode,
   project root existence, session counts, active/queued/paused count, missing
   worktrees, event mirror consistency, and `sandbox-exec` availability.
3. **Daemon check.** Provider configured/not configured is appended by the
   daemon using `agent.info`.
4. **No mutation.** Doctor never repairs, deletes, or reconfigures anything.
   Warnings point to existing CLI recovery commands (`restore-log`, `cleanup-worktrees`,
   `rehydrate`, provider settings).
5. **Human output.** The CLI prints one aligned line per check; JSON remains
   available through the RPC.

## Consequences

- One command surfaces the most common local failure modes.
- Mirror consistency is checked per session and can be slower on large stores;
  it remains a manual diagnostic.
- Doctor does not validate live provider credentials or run an LLM call.
- It does not replace `session.replay`; replay remains the projection-level
  repair tool.

## Alternatives considered

- **Add checks to `server.info`:** mixes stable server metadata with mutable
  diagnostics.
- **Auto-repair on startup:** surprising and destructive.
- **External script:** duplicates Rust store logic and drifts.

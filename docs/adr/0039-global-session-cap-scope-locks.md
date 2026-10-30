# ADR-0039: Global session admission and cross-session write-scope locks

- **Status:** Accepted
- **Date:** 2026-10-30

## Context

Caps were per session. A daemon with several sessions could run each at the full
Worker cap, so total concurrency and provider spend scaled with the number of
sessions. The merge queue is session-local, and Worker worktrees are isolated,
but two sessions can still work on overlapping project paths and only discover
the conflict during integration or landing. There was no daemon-wide admission
control or cross-session write lease.

A full global Task-DAG scheduler with priorities and fair queue positions is a
larger design. The immediate need is to bound concurrent session runs and make
overlapping Worker scopes wait instead of racing.

## Decision

1. **Daemon-wide session admission.** `AgentService` owns a semaphore. The
   capacity comes from `VERLINPARK_GLOBAL_SESSION_CAP` (default 4, clamped
   `1..=32`) at service construction. `session.prompt` inserts the run control,
   sets the session to `queued`, appends `agent.queued`, and waits for a permit
   before `run_agent` starts. The session becomes `running` when the permit is
   acquired.
2. **Queue semantics.** Waiters are served by Tokio's semaphore. Cancellation
   while queued finalizes the session as `cancelled`; pausing while queued keeps
   the run paused after admission. A queued session that survives a daemon crash
   is recovered as `failed` by the existing recovery path (`queued`, `running`,
   and `paused` are in-flight states).
3. **Per-session caps remain.** The global cap limits session runs, not Workers
   or Labour. The existing per-session Manager/Worker/Labour caps still apply
   inside an admitted run.
4. **Cross-session scope locks.** A daemon-wide `ScopeLocks` registry holds
   active Worker/Labour write scopes keyed by project and agent. Different
   sessions with overlapping scopes wait for each other; the same session is
   allowed to share (its own scheduler already serializes tasks). An empty scope
   is a wildcard. Locks are released when the Worker/Labour `run_loop` ends and
   are in-memory only (a daemon restart fails in-flight runs).
5. **Cancellation and pause.** Scope-lock waits select on the run's cancel flag.
   A cancelled Worker releases its wait and returns `Cancelled`. Pause does not
   release a held lock; resume continues the same run.
6. **Auto-resolution and verification.** Auto-resolve acquires a global session
   permit before starting. Resolver/Verifier runs are explicitly auto-approved
   internal runs and do not acquire Worker scope locks; their writes are already
   confined to the Worker worktree by the shell sandbox.
7. **Observability.** Queued sessions emit `agent.queued` and are shown as
   `QUEUED` in the app. The existing `agent.started`/`agent.finished` events
   bracket the admitted run. Scope-lock waits are not yet a separate event.

## Consequences

- Total concurrent session runs are bounded even when many sessions are created.
- Overlapping Worker scopes across sessions no longer run concurrently; they wait
  instead of producing avoidable integration conflicts.
- The global cap is process-local and configured by environment, not the app UI;
  it is not a fair multi-tenant scheduler or a persistent queue.
- A queued session consumes a run-control slot but not a session semaphore permit;
  cancellation still works through the run-control map.
- The global cap does not limit Leader/Manager/Verifier work or tool subprocesses
  directly; provider usage is still bounded by session token budgets.
- Scope locks only cover Worker/Labour runs. Leader/Manager shell writes and
  project-level Git operations remain governed by approvals, worktrees, and the
  existing git mutex/landing checks.
- There is no priority, deadline, fairness policy, global task DAG, or persisted
  queue position yet.

## Alternatives considered

- **Global cap only:** bounds concurrency but still allows overlapping scopes.
- **Scope locks only:** prevents overlap but leaves unbounded session runs.
- **Reject when full:** simpler but worse UX than queueing; cancellation covers
  the waiting case.
- **Persist locks/queue:** useful for daemon restart but unnecessary while
  in-flight runs are failed on restart; can be added with checkpointing.
- **Full global DAG scheduler:** the right long-term design, but a larger change
  than this slice.

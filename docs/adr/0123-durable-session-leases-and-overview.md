# ADR 0123: Durable session leases and cross-session overview

- Status: Accepted
- Date: 2026-11-09
- Area: Runtime / operations

## Context

The daemon serializes runs with an in-process `runs` map and an exclusive
data-directory instance lock. That is sufficient for one daemon, but it leaves
two gaps:

1. There is no durable record of which process owns a session run. A second
   process pointed at the same SQLite database (for example during development,
   recovery, or a future server mode) has nothing to check.
2. Operators need a cross-session view of plan health. Looking at each session
   one by one hides which team needs attention.

## Decision

1. **Runtime leases are SQLite-backed.** `runtime_leases` stores
   `lease_key`, `owner_id`, `kind`, `session_id`, `acquired_at`,
   `heartbeat_at`, and `expires_at`. Acquisition deletes expired rows, refuses
   an unexpired lease held by another owner, and refreshes the same owner.
2. **Session runs acquire a lease.** Both `AgentService::start` (Leader prompt)
   and `start_plan_run` (task scheduler / resume) acquire
   `session:{session_id}` with a 180-second TTL. A background heartbeat renews
   every TTL/3 (at least 5s). Release writes `session.lease.released`; dropping
   the guard aborts the heartbeat and leaves the TTL to expire after a crash.
3. **Expired leases are swept.** The orchestration tick calls
   `clear_expired_runtime_leases`. `runtime.leases` / `runtime.leases.sweep`
   expose state and manual cleanup to the CLI and Manage Dash.
4. **Integration work is leased too.** `process_merge_entry` acquires an
   `integration:{session}` runtime lease around the deterministic merge/check
   path, so a session run and an explicit retry/resolver cannot integrate the
   same worktree concurrently. The same heartbeat/TTL rules apply.
5. **Merge recovery is crash-safe.** Before `git merge`, the queue entry
   records `pre_merge_commit` (the exact session HEAD). On daemon restart,
   interrupted entries with a recorded pre-merge HEAD are reset only when the
   session worktree is clean; dirty worktrees are left untouched and receive a
   `merge_queue.recovery_skipped` event, while successful resets emit
   `merge_queue.recovered`. The entry remains retryable either way.
6. **The existing instance lock remains.** Leases do not replace per-data-dir
   exclusivity; they make ownership explicit if the daemon topology changes.
   They are not a distributed lock service and do not provide fencing tokens.
7. **Overview is a read projection.** `orchestration.overview` scans recent
   TeamPlan sessions, derives the same snapshot used by Collab, and ranks by
   attention, running status, then priority. It returns totals plus top
   blockers and the last controller decision per session.
8. **Overview never mutates.** Mission Control is a monitor; task actions and
   plan revisions stay in Collab Ops, the task queue, or the CLI.

## Consequences

- A session run has a durable owner and heartbeat, and crashes are recoverable
  after the TTL.
- The CLI and Manage Dash can inspect leases and stale ownership.
- Cross-session attention is visible without opening each session.
- SQLite writes add one lease row per active run, not per agent; the global
  Worker/Labour capacity controller remains in-process.
- This is not a complete distributed scheduler. A future multi-daemon mode
  still needs a shared admission/fairness design, fencing, and merge-queue
  ownership beyond the current per-session lease.

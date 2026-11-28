# 0245 — Dead-process runtime lease recovery

- Status: Accepted
- Date: 2026-11-28
- Scope: same-host daemon restart recovery of unexpired session leases
- Depends on: ADR-0161 (cross-process lease fencing), ADR-0187 (runtime leases),
  ADR-0232 (run journal/interrupted runs)

## Context

A live 10-task scale run was killed with `SIGKILL` mid-plan. On restart,
durable runs were marked `interrupted`, but the session stayed `running` and
its plan tasks stayed `running` for up to the 900s lease TTL. `resume_plan`
rejected the session ("must be idle, failed, cancelled, or draft").

The cause was deliberate recovery logic: `recover_state` preserves a session
whose `session:<id>` runtime lease is unexpired and owned by another instance,
so it does not fail a session that another daemon may still own. A crashed
same-host process leaves exactly such a row. The daemon identity is
`instance_id:pid`; the old PID is dead, but recovery never checked it.

## Decision

Before `Store::recover_state()`, the Engine calls
`Store::release_dead_process_runtime_leases()`:

- Only leases whose `owner_instance` is `instance_id:pid` with a canonical UUID
  equal to this data directory's persisted daemon identity are considered.
- The PID is checked with `kill(pid, 0)` (existence/permission only, no
  signal). A dead PID (`ESRCH`) releases the lease by setting
  `released_at/expires_at`.
- A live PID, a non-UUID/foreign owner label, and any other kind of lease are
  left untouched. This preserves the existing foreign-live-lease safety test.
- After that release, normal session/agent/approval recovery can mark the
  interrupted session failed and transition running plan tasks to interrupted.

This is same-host coordination only. It is not cross-host lease consensus and
does not reclaim a lease owned by a live process, even if that process is
misbehaving. PID reuse can delay recovery until the TTL; the lease remains the
authority.

## Consequences

- A same-host `kill -9` no longer leaves a plan session stuck in `running`
  until the 15-minute lease TTL; the operator can `resume-plan` after restart.
- The recovery is bounded to the persisted daemon identity, so a foreign or
  malformed owner label is preserved.
- A shared data directory with another live daemon (same identity, live PID)
  remains protected.
- Recovery still does not replay in-flight provider calls or prove external
  effects; run-journal/effect rules still apply.

## Live validation (2026-11-28)

A 10-task live plan was killed with `SIGKILL` three times. The first restart
reproduced the stuck `running` state and refused `resume-plan`; after this
change, restarts marked the session failed, moved running plan tasks to
interrupted, and `resume-plan` completed the remaining tasks. The session
finished with 10/10 merged tasks and 0 duplicate merge entries.

## Tests

- `tests::restart_recovery_releases_a_dead_same_host_lease` (dead child PID,
  same identity, session becomes failed and the lease is released).
- `tests::restart_recovery_does_not_fail_a_session_with_a_live_foreign_lease`
  (unchanged preservation behavior).
- Live scale run: `SIGKILL` mid-plan, restart released the dead lease, session
  became failed, 7 interrupted tasks were resumed.

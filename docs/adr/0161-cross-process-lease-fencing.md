# ADR-0161: Cross-process lease fencing and continuation delivery ownership

- **Status:** Accepted
- **Date:** 2026-11-11
- **Supersedes:** none
- **Related:** ADR-0151 (idempotent session operations), ADR-0155 (durable run
  journal), C2 in `docs/CONTEXT_OWNERSHIP_MEMORY_BENCH_PLAN.md`, E7 in
  `docs/EFFICIENCY_PLAYBOOK.md`

## Context

VerlinPark's daemon is local-only, but the data directory is an ordinary
filesystem directory. Nothing prevents two daemon processes from opening the
same `verlinpark.db` on one host:

- an operator starts `verlinpadd` twice by mistake;
- a supervisor restarts the daemon before the previous process exits;
- a test harness or development tool opens the same store;
- a future packaging mode runs a foreground daemon next to a login item.

SQLite WAL serializes individual writers, so the projections do not corrupt.
That is not enough for ownership. Before this ADR:

- `runtime_leases` stored an owner string and expiry. A crashed process's row
  expired and could be replaced, but the old process had no way to discover
  that it no longer owned the key.
- A live process could call renew with the same owner string after another
  process had taken over, because renewal matched only `owner_id`. Two
  processes using the same configured owner could both renew.
- `session_continuations` used a status-only claim (`ready -> delivering`).
  Once a process claimed a row, any process could acknowledge it, and an
  expired claim could not be distinguished from a live one.
- Startup recovery failed every `delivering` continuation closed, which was
  safe on one daemon but wrong when another process was still alive.
- The Integrator acquired `integration:{session}` but never re-checked the row
  at the Git effect boundary. A process that lost the lease could still run
  `git merge` and mutate the session branch.

The goal is not distributed consensus. The goal is to make same-host,
shared-data-directory ownership explicit and fail closed, while preserving the
existing single-daemon behavior and protocol.

## Decision

### 1. Daemon identity is persisted per data directory

`store_meta` holds a `daemon_instance` JSON value:

```json
{
  "instance_id": "6e8f...",
  "pid": 7331,
  "started_at": "2026-11-11T09:14:03Z"
}
```

- `instance_id` is a random UUID created once by the first Store opener.
  Concurrent first openers use `INSERT OR IGNORE` followed by a read, so every
  process sharing the directory observes the same id.
- `pid` and `started_at` are diagnostic. They are **not** a liveness probe.
  PIDs are reused; `started_at` is not compared across hosts.
- The identity is not a secret and is exposed in `runtime.leases`.

A process-scoped owner id is composed as:

```
owner_instance = <instance_id>:<pid>
owner_id       = <instance_id>:<pid>:<random nonce>
```

`Engine` generates a fresh nonce at construction. Two Engines in one process
share `owner_instance` but have different `owner_id`s. Two processes on one
host differ in both. `owner_id` is what a lease row records; `owner_instance`
lets startup recovery identify rows belonging to this process rather than a
foreign one.

**Limits.** The persisted id identifies the installation/data directory, not a
single process incarnation. A process restart reuses `instance_id` and may
reuse a PID. That is why every lease also carries a random token and a
monotonic generation; identity strings are for diagnosis and cleanup, never for
fencing.

### 2. Runtime leases carry an acquisition token and monotonic generation

`runtime_leases` adds:

| column | meaning |
|---|---|
| `owner_instance` | process-scoped `<instance_id>:<pid>` |
| `lease_token` | unpredictable 244-bit secret for this acquisition |
| `fencing_token` | monotonically increasing generation per lease key |
| `renewed_at` | last successful fenced renewal |
| `released_at` | tombstone timestamp; live queries hide the row |

`runtime_lease_fences(lease_key, fencing_token, updated_at)` is the
authoritative generation counter. It survives deletion of the live row, so
cleanup cannot make generations reusable.

Acquisition is one `BEGIN IMMEDIATE` transaction:

1. Read the row.
2. If it is live and owned by a different `owner_id`, return
   `RuntimeLeaseError::Busy` with the public holder projection.
3. Otherwise compute `generation = max(row.generation, fence.generation) + 1`,
   write the new fence, generate a token, and upsert the row with
   `released_at = NULL`.
4. Return the public row plus the token to the caller only.

A same-owner acquisition while live is treated as a reacquisition: it advances
the generation and replaces the token. This preserves the old single-daemon
"the same owner may refresh" behavior while making an older guard stale. A
caller that wants to renew must use `renew_runtime_lease`, not acquisition.

Renewal updates only when all of the following hold:

```
lease_key = ? AND owner_id = ? AND lease_token = ?
AND released_at IS NULL
AND julianday(expires_at) > julianday(now)
```

Zero rows means the lease is lost. The Store classifies the loss as missing,
expired, released, owner mismatch, token mismatch, or invalid.

Release is conditional on `lease_key + owner_id + lease_token` and
`released_at IS NULL`. It is allowed after expiry when the token still matches:
cleaning up an expired row that nobody reacquired is harmless. It is never
allowed to delete a newer generation. Release writes a tombstone instead of
deleting immediately; `clear_expired_runtime_leases` may delete tombstones
later, but the fence row remains.

`assert_runtime_lease(key, owner, token)` is the read fence. It requires the
row to exist, be unreleased, unexpired, and match both owner and token. It
returns the public row and is safe to call from a blocking Git closure.

### 3. Guard lifecycle and cancellation

`RuntimeLeaseGuard` carries:

- the lease key, kind, owner, owner instance, generation;
- the token;
- an `AtomicBool` lost flag;
- a heartbeat task that renews at `TTL/3` (minimum 5 seconds).

A failed renewal sets the lost flag and stops the heartbeat. `assert_held()`
checks the fast flag and then queries the database; it also sets the flag on a
failed assertion. `assert_not_lost()` is the non-async fast check for code that
cannot await. The Integrator uses `assert_held()` at the effect boundary.

Normal callers `await release()`. `Drop` performs a best-effort synchronous
fenced release (and aborts the heartbeat). A process abort or `SIGKILL` cannot
run `Drop`; the row then expires after its TTL. This is the crash path.

Session leases use the same token and generation. The `session.lease.acquired`
event payload records owner, instance, generation, and TTL but never the token.
The release path records a `lease_lost` boolean if the token had already been
superseded; it never overwrites the newer row.

### 4. Continuation delivery is a fenced claim, not a status convention

`session_continuations` adds:

| column | meaning |
|---|---|
| `lease_owner` | full owner id that claimed delivery |
| `lease_token` | unpredictable claim secret |
| `lease_expires_at` | claim TTL |
| `delivery_attempt` | number of claims, including reclaims |

The public `SessionContinuation` projection exposes owner, expiry, and attempt
count. It never exposes the token.

Claim is one `BEGIN IMMEDIATE` transaction with this predicate:

```sql
(status = 'ready'
 AND (lease_expires_at IS NULL OR lease_expires_at <= now))
OR
(status = 'delivering'
 AND lease_expires_at IS NOT NULL
 AND lease_expires_at <= now)
```

The update sets `status = 'delivering'`, installs a fresh token and expiry,
increments `delivery_attempt`, and writes the
`session.continuation.delivering` event in the same transaction. A racing
process sees a live claim and returns `None`. The winner is the only process
that may acknowledge.

Acknowledgement variants — delivered, retry, failed — require:

```
id = ? AND status = 'delivering'
AND lease_owner = ? AND lease_token = ?
AND lease_expires_at IS NOT NULL
AND lease_expires_at > now
```

Zero rows produces a typed `SessionContinuationAckError::Lost`; no projection
is changed and no event is appended. Delivered increments `run_count` once,
clears the claim, emits `session.continuation.delivered`, and inserts the next
recurrence child in the same transaction. Expired claims cannot write that
child.

A live foreign claim is left alone by startup recovery. An expired claim is
reclaimed to `ready` with a `session.continuation.reclaimed` event, preserving
`delivery_attempt`, so the scheduler can try again. Legacy `delivering` rows
with no lease expiry (from before this ADR) still fail closed, because their
ownership cannot be reconstructed.

### 5. Duplicate delivery is prevented by the operation receipt

A claim protects ownership, not the external start effect. The sequence is:

1. claim row (transactional);
2. call the normal session start path;
3. acknowledge delivered or retry/fail (transactional).

A crash can happen between 1 and 3. Reclaiming the row after expiry could start
a second run if the first start already happened. The reconciler therefore
passes a stable operation id:

```
continuation-delivery:<continuation-id>
```

`start_with_operation` binds that id to `session.prompt` and the rendered
prompt payload through the existing `session_operations` ledger:

- If the receipt completed, the new claimant gets `Replay` and acknowledges
  delivery without starting a second run.
- If the receipt is still `pending` in another live process, the new claimant
  gets `InProgress` and fails this attempt.
- If the previous daemon left it `pending`, startup recovery marks it
  `unknown`; the new claimant fails closed instead of replaying an uncertain
  side effect.

This makes duplicate start prevention stronger than the claim alone. It is
still not exactly-once for external effects: a provider call or a Git command
inside the started run has its own semantics. The receipt only prevents a
second *continuation delivery start*.

### 6. Integration uses the lease at the effect boundary

`process_merge_entry` acquires `integration:{session}` and passes the guard to
`process_merge_entry_fenced`. The function checks the fence:

1. after the clean-worktree preflight, before `begin_merge_queue_entry`;
2. inside the Git mutex immediately before `git merge`;
3. immediately after `git merge`, before marking the entry `checking`.

The second check is the important one: it is the last read before the session
branch can change. A stale generation returns an error and records the queue
entry as `failed` with `lease_lost_before_merge`; the branch is untouched.

The third check handles expiry *during* the merge. If the fence is lost after
the merge commit exists but before checks run, the Integrator resets the
worktree to the recorded pre-merge HEAD, records `lease_lost_after_merge`, and
aborts. The reset is conditional on the Git mutex and the known commit; it does
not reset a partially merged worktree left by a different process.

No Verifier, diff-check, integration-command, rollback, or merge-queue rule is
weakened. The fence is additive and fails closed.

### 7. Startup recovery is process-scoped

`recover_state` previously deleted all runtime leases and failed all
`delivering` continuations. That is wrong with a second live daemon. The new
rules are:

- Delete only runtime leases whose `owner_instance` is this process, plus
  pre-fencing rows with an empty `owner_instance`.
- Do not fail a running session or its agents when a live foreign
  `session:{id}` lease exists.
- Do not mark a merge entry interrupted when a live foreign
  `integration:{session}` lease exists.
- Do not interrupt plan tasks for a session with a live foreign session lease.
- Do not deny pending approvals, expire pending questions, or cancel pending
  steering for a session with a live foreign session lease.
- Do not fail a running intake task whose linked session has a live foreign
  session lease.
- Leave live foreign continuation claims untouched.
- Reclaim expired continuation claims to `ready`; fail only legacy
  `delivering` rows with no lease expiry.
- Audit every own-process lease removed by startup cleanup as
  `startup_cleanup`; the token-free history survives the row deletion.

This is not a distributed scheduler. A process that is alive but partitioned
from SQLite (for example, blocked in `SIGSTOP`) still loses its row at TTL and
a competitor may proceed. The token fence is what makes the resumed process
stop before its next effect.

### 8. Visibility and protocol

`runtime.leases` / `vp leases` expose:

- daemon instance id, creator pid, and start time;
- lease key, kind, session id;
- owner id and process instance;
- monotonic generation;
- acquired, heartbeat, renewed, expiry, and release timestamps;
- derived `live`, `stale`, or `released` state.

The token is not in the public `RuntimeLease` projection, RPC DTO, CLI output,
event payload, bundle, or debug output. `RuntimeLeaseToken` and
`ContinuationClaimToken` have redacted `Debug` implementations.

`runtime.leases.history` / `vp lease-history` expose a token-free audit trail of
acquisition, reacquisition, release, expiry sweep, and startup cleanup
transitions. The audit table (`runtime_lease_audit`) is operational state, not a
replay projection. It is pruned from the existing daemon lease-sweep loop using
`VERLINPARK_LEASE_AUDIT_DAYS` (default 30, clamped 1–365). The live lease and
fence tables are never affected by audit pruning.

`session.continuation.status` / `vp continuation-status` expose the public
continuation plus token-free claim metadata (status, owner, expiry, attempt
count, live/stale). `session.continuation.reclaim` / `vp continuation-reclaim`
perform the same expired-claim reclaim as startup recovery and the scheduler;
neither can reclaim a live claim.

All new protocol fields are additive with `serde(default)`. The JSON-RPC major
version remains 1. `vp leases` renders the new columns but keeps the existing
JSON-RPC method names.

## Alternatives considered

### Delete expired rows on every read

Rejected. A read-only status request should not mutate ownership, and deleting
a tombstone would lose the audit view. The fence row is separate so cleanup is
safe.

### Reuse `owner_id` as the fence

Rejected. Owner strings are configured or derived and can collide. Two
processes with the same owner string could renew each other's row. A random
token per acquisition is required.

### Use the persisted instance id as the fence

Rejected. It is shared by every process on the data directory and survives
restarts, so it cannot distinguish generations.

### Delete the lease row on release

Rejected for the live row. A tombstone makes the release observable and keeps
the "released" state distinct from "missing". The generation itself is kept in
`runtime_lease_fences`, so eventual tombstone cleanup is safe.

### Fail all `delivering` continuations on startup

Rejected for fenced rows. It is safe on one daemon but kills another live
process's delivery. Legacy unfenced rows still use this path because there is
no token that could make a later acknowledgement safe.

### Retry the continuation start without an operation receipt

Rejected. A crash between start and acknowledgement would start a second run.
The receipt ledger already exists and is designed for this case.

### Use a Git file lock or `flock` for integration

Rejected as the primary mechanism. Locks are released on process death but do
not provide a generation for an already-running stale process. A filesystem
lock can complement the database fence later; it does not replace it.

## Consequences

### Positive

- Two processes on one host cannot both own a runtime lease or a continuation
  delivery claim.
- A stale Integrator aborts before `git merge`; a post-merge loss rolls back.
- Startup recovery no longer destroys another live process's work.
- Lease state is auditable without exposing tokens.
- Existing single-daemon tests and protocol clients remain compatible.

### Negative / accepted limitations

- **Not distributed consensus.** SQLite on a local/shared filesystem is the
  authority. Cross-host clock trust, network partitions, and Byzantine
  processes are out of scope.
- **TTL is the liveness bound.** A paused process may lose ownership while it
  is still alive. It must check the fence before its next effect. Code that
  performs an external effect without calling `assert_held`/`assert_not_lost`
  is not protected.
- **External effects are not exactly-once.** A provider request, shell
  command, or Git operation that already started cannot be rolled back by the
  lease. The continuation receipt prevents duplicate *delivery starts*, not
  duplicate provider calls inside a run.
- **The post-merge rollback is best effort.** It requires the Git mutex, a
  clean worktree, and the expected pre-merge commit. If another process has
  already moved the branch, the Integrator fails closed and leaves the state
  for operator inspection.
- **Write-scope leases are not yet process-scoped.** `write_leases` retains its
  existing single-daemon cleanup semantics. Two daemons sharing a data dir
  should not schedule overlapping Worker write scopes until that registry is
  migrated; the global write-lease registry remains the intended coordination
  point.
- **Daemon identity is not rotated on restart.** It identifies the data
  directory. Per-process identity is `instance_id:pid` plus the random owner
  nonce, and fencing is token/generation based.
- **Clock skew on one host.** Timestamps use UTC RFC3339 and SQLite
  `julianday` comparisons. Large manual clock changes can expire or extend
  leases; this is the same trust model as the rest of the daemon.
- **Crash windows.** A crash after `git merge` but before the post-merge fence
  may leave a merged branch and an interrupted queue row. Startup recovery
  resets a clean worktree to the recorded pre-merge commit, but cannot reset a
  dirty or partially merged worktree. A crash after a provider request starts
  but before usage/events are persisted remains an unknown outcome.

## Test coverage

- Two Store instances over one temp data directory: concurrent acquisition has
  exactly one winner; a live lease returns typed `Busy`.
- Stale renew, release, and assert fail after reacquisition; generations
  advance on expiry, release, and tombstone cleanup.
- Lease tokens are redacted from public projections and debug output.
- Two continuation claimants: exactly one wins; a live foreign claim is not
  reclaimable; an expired claim is reclaimable once.
- Stale continuation owners cannot write delivered/retry/failed state.
- Recovery leaves live foreign claims, sessions, approvals, questions,
  steering, plan tasks, merge entries, and intake tasks alone; it reclaims
  expired claims and fails only legacy unfenced rows.
- Lease audit records acquisition, reacquisition, release, expiry sweep, and
  startup cleanup without a token; retention pruning leaves live leases and
  fencing generations intact.
- `session.continuation.status`/`reclaim` expose and reclaim only token-free
  claim metadata.
- A stale integration generation aborts before `git merge` without changing
  the session branch or the worker file.
- Existing single-daemon session lease, continuation, merge, replay, and
  bundle tests remain green.

## References

- `crates/vp-store/src/store/lease.rs`
- `crates/vp-store/src/store/continuation_lease.rs`
- `crates/vp-core/src/lib.rs` (`SessionLease`, `RuntimeLeaseGuard`)
- `crates/vp-core/src/agent.rs` (`reconcile_session_continuations`,
  `process_merge_entry_fenced`)
- ADR-0151: idempotent session operations
- ADR-0155: durable run journal

## Appendix A: runtime lease state transitions

| Current row | Operation | Result |
|---|---|---|
| absent | acquire | new row, generation 1, new token |
| live, same owner | acquire | reacquisition, generation + 1, new token |
| live, other owner | acquire | `Busy`, row unchanged |
| expired | acquire | takeover, generation + 1, new token |
| released tombstone | acquire | reacquisition, generation + 1, new token |
| live, correct token | renew | heartbeat/renewed/expiry updated |
| expired, any token | renew | `Lost(Expired)` |
| released, any token | renew | `Lost(Released)` |
| missing | renew/assert/release | `Lost(Missing)` |
| wrong owner | renew/assert/release | `Lost(OwnerMismatch)` |
| wrong token | renew/assert/release | `Lost(TokenMismatch)` |
| expired, correct token | assert | `Lost(Expired)` |
| expired, correct token | release | tombstone; harmless cleanup |
| released, correct token | release | `Lost(Released)` |
| any, release | delete later sweep | fence generation retained |

Generation is reserved by `runtime_lease_fences`. Deleting a row (expiry sweep,
release tombstone cleanup) never resets the next generation.

## Appendix B: continuation delivery state transitions

| Current row | Operation | Result |
|---|---|---|
| pending | deadline/watch wake | ready |
| ready | claim | delivering, attempt + 1, token installed |
| delivering, live claim | claim | `None`, row unchanged |
| delivering, expired claim | claim | delivering, attempt + 1, new token |
| delivering, live, correct token | delivered | delivered, run_count + 1, child scheduled |
| delivering, live, correct token | retry | ready, attempts + 1, claim cleared |
| delivering, live, correct token | failed | failed, attempts + 1, claim cleared |
| delivering, expired | delivered/retry/failed | typed `Lost(Expired)`, row unchanged |
| delivering, wrong owner/token | any ack | typed `Lost(OwnerMismatch/TokenMismatch)` |
| delivering, live foreign | recovery | untouched |
| delivering, expired | recovery/reclaim | ready, claim cleared, `reclaimed` event |
| delivering, no lease expiry | recovery | failed closed (legacy) |

`delivery_attempt` counts claims. `attempts` counts failed starts. They are
deliberately separate: a claim can be reclaimed without a failed start, and a
start can be retried without a new generation.

## Appendix C: crash-window matrix

| Crash point | Durable state | Recovery behavior |
|---|---|---|
| after claim, before start | delivering with unexpired claim | left alone until expiry, then reclaimable |
| after start, before ack | delivering + completed operation receipt | reclaim replays receipt, no second start |
| after start, before receipt completion | delivering + pending receipt | startup marks receipt `unknown`; reclaim fails closed |
| after delivered commit | delivered + child inserted atomically | no duplicate child |
| after retry commit | ready + attempt count | next tick claims a fresh generation |
| after release commit | released tombstone + audit | next acquire advances generation |
| after acquire commit, before guard construction | live row with no heartbeat | expires after TTL; audited as expired |
| after `git merge`, before post-merge fence | branch merged, queue `merging` | recovery resets clean worktree to pre-merge commit |
| after post-merge fence, before checks | branch merged, lease live | next owner cannot start until expiry; rollback is owned by the live generation |
| after checks, during integration command | branch merged, command running | final fence checks ownership before `merged` |
| after final `merged` commit | terminal queue row | no recovery action |

## Appendix D: operator runbook

1. `vp leases` shows current owners, process instances, generations, expiry, and
   derived state. A `stale` row is expired but not yet swept.
2. `vp lease-history --key integration:<session>` shows how the key moved. No
   entry contains a token.
3. `vp leases-sweep` removes expired and released rows. Fencing generations
   survive.
4. `vp continuation-status <id>` shows the public continuation and token-free
   claim metadata. `live: true` means a foreign process may still own it.
5. `vp continuation-reclaim <id>` reclaims only an expired claim. It emits
   `session.continuation.reclaimed`.
6. `vp retry`/`vp resolve` re-enter the deterministic Integrator; they acquire
   the integration lease themselves and still require Verifier approval.
7. Do not delete lease or continuation rows by hand while a daemon is live.
   The database is the authority; manual edits can invalidate a running guard
   without cancelling its work.

## Appendix E: what is intentionally not provided

- A distributed lock service or consensus protocol.
- Cross-host lease acquisition or clock trust.
- Exactly-once provider calls or shell commands.
- Automatic merge conflict resolution without the existing explicit policy.
- Automatic recovery of a dirty/partially merged worktree.
- Process-scoped cleanup for `write_leases`; see the limitations above.

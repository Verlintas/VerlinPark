# Orchestration and State Reliability Review — 2026-11-19

- Repository: `/Users/apple/orca/workspaces/VerlinPark/m3-round`
- Baseline: `d239dca` (`Fix live provider compatibility and budget preflight`)
- Reviewer: subagent (senior Rust reliability review)
- Mode: read-only; no source, test, doc, or config changed. The only file created is this report.
- Target: `vp-core` plans, tasks, scheduler, scaling, programs, intake, identities,
  workspaces, leases, continuations, gates, effects, and contracts.

## Executive summary

The plan/task event+projection path is generally carefully written: task
claim/retry/block/cancel transitions are transactional, `sync_plan_tasks` is
idempotent, effects and gates fail closed at merge boundaries, and the
store-level lease fencing for runtime/integration leases is stronger than most
of this codebase. The review nevertheless found ten material state-machine and
concurrency defects, led by four that can lose or misattribute work:

1. A plan batch that aborts (cancel, gate defer, abortive budget) discards the
   already-produced results of every sibling task after the aborting task,
   overwriting a `completed`/merged task with `cancelled`/`blocked`.
2. `submit_team_plan` / `set_team_plan` can reset the entire task projection:
   a second submission starts a new `plan_seq`, so completed/attempt state is
   lost and work can be re-executed.
3. A task-level cancel that arrives after the Verifier finishes but before the
   Integrator does can still merge the commit; the subsequent unconditional
   finish also overwrites the cancel state.
4. `skipped` is treated as a satisfied dependency by the controller but not by
   the scheduler, producing a permanent unblock/block loop downstream of a
   skipped task.

The remaining findings are revision-history/optimistic-concurrency errors,
gate expiry enforcement, cross-process lease table handling, task-worktree
retention, and program budget reservation. All are reachable from the current
public surfaces; none require a hidden model call or network access.

Bottom line: this round should be considered `needs-fix-before-trust` for
multi-task plans, long-running plans (large event logs), and shared data-dir
deployments. Single-task plans and the normal claim→Verifier→Integrator path
remain structurally sound.

## Findings

| # | Severity | Confidence | Area | Location | Summary |
|---|---|---|---|---|---|
| F1 | High | High | Batch abort settling | `crates/vp-core/src/agent/plan/execute.rs:534-573`; `crates/vp-store/src/store/plans/transitions.rs:133-150` | Abort drops sibling results and overwrites completed/merged tasks with `cancelled`/`blocked` |
| F2 | High | High | Plan revision history | `crates/vp-core/src/engine/plans/revisions.rs:4-7,52-53`; `crates/vp-store/src/store/events.rs:118-125` | Revision numbering and history use a 2,000-event window, not the real revision count |
| F3 | High | High | Plan submission | `crates/vp-core/src/agent/run_loop/tool_calls.rs:433-462`; `crates/vp-core/src/engine/plans/revisions.rs:209-236`; `crates/vp-core/src/scheduler.rs:113-128` | Re-submitting a plan resets tasks/attempts and bypasses revision safety |
| F4 | High | High | Skip / dependency readiness | `crates/vp-core/src/scheduler.rs:186-195`; `crates/vp-core/src/orchestration/controller.rs:120-148,259-284`; `crates/vp-core/src/agent/plan/execute.rs:174-220` | `skipped` satisfies dependencies in the controller but not in the scheduler → unblock/block livelock |
| F5 | Medium-High | High | Cancel / integration | `crates/vp-core/src/agent/plan/execute.rs:581-584,614-652`; `crates/vp-core/src/agent/plan/worker.rs:69-97`; `crates/vp-store/src/store/plans/transitions.rs:144-150` | Cancel during verification/integration can be ignored; the commit can merge and the status becomes `completed` |
| F6 | Medium | High | Optimistic revision concurrency | `crates/vp-core/src/engine/plans/revisions.rs:52-96`; `crates/vp-core/src/fanout.rs:790-905`; `crates/vp-store/src/store/plans/snapshots.rs:71-105` | `expected_revision` is check-then-write; concurrent writers both succeed with the same revision number |
| F7 | Medium | High | Gate state machine | `crates/vp-store/src/store/gate.rs:157-192`; `crates/vp-core/src/gate.rs:285-350,525-542` | `resolve` accepts an expired-but-unswept gate; archived sessions' expired gates never transition |
| F8 | Medium | High | Cross-process write/GUI leases | `crates/vp-store/src/store/backup/recover.rs:317,346-360`; `crates/vp-core/src/agent.rs:507-557,561-572` | Startup wipes all write/GUI leases; store-backed waiters only wake for in-process drops |
| F9 | Low-Medium | High | Worktree retention | `crates/vp-core/src/engine/worktrees.rs:102-116,199-225`; `crates/vp-core/src/worktree.rs:346-351` | Task worktrees/branches are never cleaned; the audit note P2-8 overstates coverage |
| F10 | Medium | High | Program budgets | `crates/vp-core/src/program.rs:1181-1230`; `crates/vp-core/src/agent/service/programs.rs:12-30,127-140` | Parallel milestones each receive the full remaining program budget (documented, but still an enforcement gap) |

Severity is impact if the code path is reached; confidence is that the code path
is reachable as described (not a claim that it occurs on every run).

## F1 — Batch abort discards already-produced sibling results

**Evidence**

`execute_team_plan` waits for every Worker future with `join_all`
(`execute.rs:534`). It then iterates the results. As soon as one result sets
`abort`, every later index is processed by this branch without looking at its
result (`execute.rs:539-573`):

```rust
if let Some(abort) = &abort {
    let status = abort.task_status();
    ...
    update_agent_status(... "cancelled"/"failed")
    statuses.insert(task.id.clone(), status)
    emit_plan_task_finished(..., status, ...)
    record_scheduled_task_finish(..., status, ...)
    continue;
}
match result { ... }
```

`record_scheduled_task_finish` calls `finish_plan_task`, whose SQL update has no
current-status predicate (`transitions.rs:144-150`):

```sql
UPDATE plan_tasks
SET status = ?1, ... 
WHERE session_id = ?4 AND plan_seq = ?5 AND task_id = ?6
```

`PlanAbort` is set by `WorkerFinish::Cancelled`, `WorkerFinish::BudgetExceeded`
with an abortive scope, and `LoopOutcome::Deferred` (gate wait). The deferred
case is task-local, so a gate wait in T1 is enough to trigger this for T2.

**Impact**

- A sibling that already completed its Worker, passed the Verifier, and merged
  its commit is recorded as `cancelled` or `blocked`. The merge queue and the
  task row disagree.
- Its summary/result is discarded, and its agent status is flipped from
  `idle` to `cancelled`/`failed` after a successful merge.
- A later resume treats the task as retryable (blocked/cancelled are in
  `claim` statuses), so it can re-run, spend budget, and create new commits or
  duplicate provider work under an already-merged task.
- A program milestone whose task is one of these rows fails
  `reconcile_active_milestone` even though the merged commit exists.

**Repro**

With the fake-provider harness, submit a two-task plan with non-overlapping
scopes. Make T1 call `wait_for_gate`/return `LoopOutcome::Deferred`; make T2
write a file and finish normally. After the batch, inspect
`task_queue`: T1 is `blocked` (expected), T2 is `blocked` (bug) while
`merge_queue` has a merged T2 entry. The same repro works for a session-level
cancel where T2 finishes before the cancel propagates.

**Suggested fix**

Process every result. For each result, record the state it actually produced.
Only synthesize terminal state for items that genuinely have no result (there
are none with `join_all`, so the branch can be deleted) and settle the
non-batch `remaining` tasks separately. At minimum, skip the synthesized state
when the task's current row is already terminal, and make `finish_plan_task`
conditional on a non-terminal status (for example
`WHERE status NOT IN ('completed','skipped','cancelled')`).

**Missing test**

`settles_sibling_tasks_when_a_batch_is_cancelled` covers the all-cancelled
case only. Add a two-task test where one task defers/cancels and the other
finishes and merges; assert the finished task stays `completed`, its merge stays
merged, and only the non-finished tasks are settled by the abort.

## F2 — Plan revision numbering/history use a truncated event window

**Evidence**

`recent_events` clamps its limit to 2,000 rows
(`crates/vp-store/src/store/events.rs:118-125`). The revision path passes
5,000, so it only ever sees the newest 2,000 events:

- `plan_revisions()` uses `recent_events(..., 5_000)` (`revisions.rs:4-7`).
- `revise_team_plan()` computes `current_revision` from
  `recent_events(..., 5_000)` (`revisions.rs:52-53`).
- `orchestration::plan_revision` counts plan lifecycle events in that window
  (`orchestration/plan.rs:118-129`).
- `advance_orchestration` builds `escalated_attempts` from
  `recent_events(..., 5_000)` (`engine/plans/orchestration.rs:474`;
  `orchestration/controller.rs:290-315`).
- `plan_revision_detail()` correctly uses `all_events`, so detail and list
  disagree.

`current_plan_revision()` in `engine/plans.rs:44-48` uses the store's full
`plan_revision_count` (`runs.rs:762-770`), while `set_team_plan` uses that
count. The two paths therefore number revisions differently.

**Impact**

- In a long session (more than 2,000 events since the plan submission),
  `revise_team_plan` can compute the wrong `current_revision`. With
  `expected_revision` set it returns a false conflict; without it, it can
  assign the same revision number twice (for example a second revision 1).
- `plan_revisions` hides revisions whose events fall outside the window, so the
  UI/CLI history is incomplete and `revision` numbers shown to users can restart
  at 1 for the window.
- `plan_revision_detail(1)` then returns the first matching stored plan, which
  may be the original submission rather than the new revision.
- `escalated_attempts` loses old escalation decisions once they age out of the
  window. A failed task that remains failed while the session continues will be
  escalated again each time the previous decision leaves the window. That adds
  duplicate escalation notes/decisions without bound in an active long session.

**Repro**

Create a session with a plan, append >2,100 unrelated events with
`append_event`, then call `team_plan.revision` (or `revise_team_plan`) with
`expected_revision=Some(1)`. The call fails with `expected 1, current 0`.
Call it with no expected revision and inspect `plan.revisions`: two entries can
carry revision 1 (or a number lower than the store count).

**Suggested fix**

- Use a store query for the lifecycle events (`type IN
  ('team_plan.submitted','team_plan.revised')`) or use
  `plan_revision_count` as the authoritative source for numbering.
- Make `plan_revisions` read all revision events (or a bounded page with an
  explicit `truncated` flag and a sequence cursor).
- Replace `escalated_attempts`' event-window scan with a projection (the
  existing `orchestration_decisions` table) or a store query filtered by
  `(task_id, attempt, action='escalate')`.

**Missing test**

A test that appends >2,000 events between revisions and asserts monotonically
increasing revision numbers, complete `plan_revisions` output, and no repeated
escalation decision for the same `(task, attempt)`.

## F3 — Re-submitting a plan resets the task projection

**Evidence**

The Leader-only tool branch accepts `submit_team_plan` unconditionally and calls
`engine.set_team_plan` (`agent/run_loop/tool_calls.rs:433-462`). There is no
"plan already exists" guard and no `ensure_revision_safe` call.

`set_team_plan` writes a **new** `team_plan.submitted` event every time
(`engine/plans/revisions.rs:209-236`), and `scheduler.sync_plan` derives
`plan_seq` from `MAX(seq)` of that event kind (`scheduler.rs:113-128`), only
`team_plan.submitted`. `sync_plan_tasks` then:

- replays only events with `seq > plan_seq` (`tasks.rs:59-77`),
- deletes **all** `plan_tasks` rows for the session and re-inserts
  (`tasks.rs:184-193`),
- sets `attempts` from the replayed `plan_task.started` count, which is zero
  after a new submission (`tasks.rs:194-232`).

So a second `submit_team_plan` after `run_team_plan` has completed loses every
completed status and attempt count and marks all tasks pending again. If the
Leader then calls `run_team_plan`, `execute_team_plan` sees no completed tasks
and re-runs them.

**Impact**

- Completed work is re-executed (provider cost, duplicate merge attempts, test
  runs) and may produce conflicting edits on a reused task worktree.
- Attempt budgets reset, so a previously exhausted task can be retried an
  unbounded number of times by model re-submission.
- The plan-revision safety check (`ensure_revision_safe`, ADR-0122 item 5) is
  bypassed entirely: a resubmission can replace active/completed task
  definitions. This is not an optimistic-concurrency edge; it is the normal
  tool surface.

**Repro**

Submit a plan, run it to completion, then call `submit_team_plan` again with
the same plan JSON and inspect `task.queue`: every row is `pending`, `attempts`
is 0. A subsequent `run_team_plan` re-spawns Workers for completed tasks. This
requires a provider to run end-to-end, but the projection reset can be observed
without one by calling the store/engine `set_team_plan` twice.

**Suggested fix**

- Reject `set_team_plan` when a plan already exists unless the caller explicitly
  supplies a revision (for example `expected_revision` and a reason), and route
  that path through `revise_team_plan` / `ensure_revision_safe`.
- Make `sync_plan` use the latest plan lifecycle event
  (`submitted` or `revised`) and a stable plan-revision identity rather than a
  new "submitted" sequence.
- Keep the old task-state replay for unchanged task ids on a real revision.

**Missing test**

Submit a plan, complete T1, resubmit the same plan, and assert T1 remains
completed with the same attempt count. A second test should assert that
`submit_team_plan` cannot rewrite a running/completed task's scope.

## F4 — `skipped` dependency semantics diverge, causing an unblock/block loop

**Evidence**

Three places disagree on whether a `skipped` dependency is satisfied:

- `scheduler.ready_tasks_governed` builds `completed` only from
  `status == "completed"` (`scheduler.rs:186-195`), so a downstream task whose
  dependency is `skipped` is never ready.
- `orchestration::ready_tasks` and `orchestration::blocked_by` treat
  `"completed" | "skipped"` as satisfied (`orchestration/plan.rs:267-310`).
- The controller unblocks a blocked task when all dependencies are in that
  combined set (`orchestration/controller.rs:120-124,135-148,259-284`).
- `execute_team_plan` computes the unmet list with the combined set
  (`execute.rs:174-220`), so for a blocked downstream task it reports the
  misleading reason `"dependency not completed"` even though the dependency is
  skipped.

The store's own comment says skipped dependencies are intended to remain
blocked (`vp-store/src/store/plans/control.rs:253-255`: "downstream dependencies
remain blocked rather than silently running"), which makes the controller's
unblock behavior the immediate bug even if the scheduler semantics are kept.

**Impact**

For a plan `T1 -> T2` where T1 is skipped:

1. The plan runner has no ready task and marks T2 `blocked`.
2. The controller (20s tick) sees T2 blocked and its dependency skipped,
   unblocks it to `pending`.
3. The global scheduler (default-on, ~60s cooldown) sees a pending task and
   resumes the plan.
4. The scheduler readiness filter rejects T2, and the runner blocks it again.

The cycle repeats for the lifetime of the session. Each pass emits
`plan_task.finished` (`blocked`), `plan_task.unblocked`, and
`orchestration.decision` events and runs the full plan-resume path (session
lease, `sync_plan`, coordination reads), although the skipped dependency means
no Worker can be dispatched. T2 is never executed. `task.skip` is a normal
operator action, so this is reachable without a buggy model.

**Repro**

Plan `T1` (no deps) and `T2` (deps `[T1]`). `task.skip T1`. Run the
orchestration controller and the task scheduler ticks repeatedly and inspect
events/rows. `T2` oscillates `blocked -> pending -> blocked`.

**Suggested fix**

Define one dependency predicate, for example:

```rust
fn dependency_satisfied(status: &str) -> bool {
    matches!(status, "completed" | "skipped")
}
```

and use it in the scheduler, controller, plan runner, and stale-blocked
cleanup. If skipping intentionally blocks downstream work (as the store comment
says), remove the controller's `skipped`-as-satisfied behavior and make the
downstream task terminal-blocked with an explicit "dependency skipped" reason
instead of repeatedly unblocking it. Either decision must be applied
consistently.

**Missing test**

`T1` skipped, `T2` dependent; run the controller/scheduler ticks twice and
assert the event count grows by one block (or the task is terminal) rather than
oscillating.

## F5 — Task cancel can be lost during verification/integration and merge the commit

**Evidence**

`execute_team_plan` checks `plan_task_cancel_requested` only before
`finish_worker` (`execute.rs:581-584`). `finish_worker` then:

- commits the Worker worktree (`worker.rs:17-43`),
- runs the Verifier (`worker.rs:69-85`),
- on approval, enqueues and merges the commit (`worker.rs:88-97`).

The Verifier is a separate run with the same task id, so a cancel that arrives
before the Verifier's next turn is observed by `wait_for_task_resume`
(`agent.rs:954-999`) and returns `VerificationOutcome::Cancelled`. But the
Verifier usually submits its verdict in one turn: a cancel that arrives after
the Verifier's final turn begins (or after it returns) is not observed. The
same applies during `integrate_worker` and a long
`VERLINPARK_INTEGRATION_COMMAND`. The plan loop then records a successful
finish:

```rust
let status = if success { "completed" } else { "failed" };
...
record_scheduled_task_finish(... status ...)
```

`finish_plan_task` has no status guard, so it overwrites `cancelling` with
`completed` (`transitions.rs:144-150`). There is no re-check of
`plan_task_cancel_requested` after `finish_worker` and no pre-merge task-status
check in `process_merge_entry_fenced`.

**Impact**

- An operator cancellation can be silently lost; the session branch contains a
  merge that the operator asked not to land.
- The task row, agent status, and merge queue report completion after the
  cancel response. This is not just "cooperative cancel during a provider
  stream"; the lengthy part can be the deterministic integration command.
- A later pause/cancel/retry sees a terminal `completed` row and cannot undo
  the merge.

**Repro**

Use a two-task plan (or a single task) and a `VERLINPARK_INTEGRATION_COMMAND`
that sleeps for several seconds. Start the task; wait until the Verifier has
returned approve (or simply sleep until the integration command starts); call
`task.cancel` for the running task. The task ends `completed`, `agent.finished`
says `merged: true`, and the task row no longer mentions the cancel. The same
window exists for `task.pause` only at Verifier turn boundaries; the integration
phase is not interruptible at all.

**Suggested fix**

- Re-check `plan_task_cancel_requested` (and `cancelling`) immediately before
  `enqueue_merge`, immediately before `git merge`, and before recording the
  final status. On cancel, leave the commit unmerged and record
  `cancelled`/`failed` with the commit hash.
- Pass the task-level cancel signal into the Verifier/Integrator or assert the
  task status at the integration effect boundary.
- Make `finish_plan_task` conditional on a non-terminal status, so a late
  finish cannot overwrite `cancelled`, `skipped`, or `pausing`.

**Missing test**

A test with a slow integration command that cancels the task while the
integration is running and asserts no merge completes and the status remains
`cancelled` (or `failed` with `merged: false`).

## F6 — `expected_revision` is check-then-write, not atomic

**Evidence**

`revise_team_plan` reads `current_revision`, compares `expected_revision`,
computes `revision = current + 1`, then writes the event
(`revisions.rs:52-96`). `fanout_plan` does the same (`fanout.rs:790-825,
855-905`). The store's `set_team_plan_revision_with_runtime_snapshot_and_event`
only checks that the session exists; it does not compare the revision or the
prior plan (`snapshots.rs:71-105`).

**Impact**

Two concurrent `team_plan.task_edit`/`team_plan.revision`/`plan.fanout`
requests, both with `expected_revision=N`, can both pass the check and both
write revision `N+1`. The last writer wins the `sessions.team_plan` row while
the event log contains two revisions with the same number. Consequences:

- The diff fields and `runtime_snapshot` of the losing writer are computed
  against a plan that was never current; `plan.revision` and `plan.migrate`
  can pin the wrong snapshot.
- The "stale client is rejected" guarantee in ADR-0122 item 7 is not enforced.
- Concurrent `sync_plan` calls can delete/re-insert the task projection twice,
  exposing the transient state to the other run.

**Repro**

Two concurrent tasks (Tokio tasks) call `engine.revise_team_plan` with the same
`expected_revision=N` but different plans. Both return success; inspect
`team_plan.revision` for two entries with revision `N+1`. The existing
`plan_revision_records_a_diff_and_rejects_protected_tasks` test is sequential
and does not cover this.

**Suggested fix**

Move the revision check into the same `BEGIN IMMEDIATE` store transaction that
writes the event: read `MAX(revision)` for the session, compare with
`expected_revision`, and insert or fail. Add a projection/unique index for
`(session_id, revision)`. A per-session plan mutex or session lease held across
the revision write is also acceptable for the same-host case.

**Missing test**

A concurrency test with two writers and the same expected revision; exactly one
must succeed and the event log must contain one `N+1`.

## F7 — Gate resolution ignores `expires_at`; archived gates never expire

**Evidence**

`Store::resolve_gate` verifies the token and matches on `GateState` only
(`store/gate.rs:157-192`). It does not compare `record.gate.expires_at` with
`now`, so an expired gate is still resolvable until the sweep transitions it.
The daemon sweep is the only thing that marks expiry
(`vp-core/src/gate.rs:525-566`), and the store query only selects expired rows
(`store/gate.rs:130-142`). The core layer also checks `is_open()` but not
`expires_at` (`gate.rs:285-350`).

`sweep_expired_gates` skips sessions whose status is `imported` or `landed`
(`gate.rs:540-542`). Those gates remain `open` forever. Because the query is
`ORDER BY expires_at ASC LIMIT`, a batch of archived expired gates can also
consume the sweep window and delay expiry of live gates.

**Impact**

- A late `gate.resolve` (after the deadline but before the next 30s sweep)
  succeeds and wakes a continuation with `resolved`, contradicting the C3
  contract that `expired` gates fail closed.
- An open gate on a landed session is stuck: it cannot resolve (landed is
  rejected by `ensure_gate_session_live`), cannot expire (the sweep skips it),
  and cannot be cancelled by the normal "resolve instead" guidance. `gate.list`
  shows an open row forever.
- Archived gates accumulate in `gate_counts.open` and can starve the bounded
  sweep.

**Repro**

1. Open a gate with `after_secs=1`. Without running the gate sweep, wait two
   seconds and call `gate.resolve` with the token. It succeeds; expected
   `GATE_EXPIRED`.
2. Land a session that has an open gate. Run `gate.sweep` repeatedly. The gate
   remains `open` and `changed=0`.

**Suggested fix**

- In `resolve_gate`, compare `expires_at` inside the `BEGIN IMMEDIATE`
  transaction and transition to `expired` (waking the continuation with the
  expired state) or return `GATE_EXPIRED` before changing the state.
- In the sweep, either allow expiring gates for landed sessions (expiry is a
  terminal state, not a new action) or exclude archived sessions' gates from
  the sweep query and mark them cancelled on session land. Imported archives
  should be excluded from the sweep query so they do not consume the limit.

**Missing test**

A resolve-after-deadline-before-sweep test, and a sweep test with one archived
expired gate plus one live expired gate that asserts the live gate still
expires.

## F8 — Write/GUI leases are not owner-scoped and cross-process waiters can hang

**Evidence**

`recover_state` intentionally preserves live foreign `runtime_leases` by
owner instance (`recover.rs:346-350`) and documents that another daemon sharing
the data directory keeps its live ownership (`recover.rs:318-320`). However, it
unconditionally deletes **all** write leases and **all** computer leases:

```rust
tx.execute("DELETE FROM write_leases", [])?;   // recover.rs:317
...
tx.execute("DELETE FROM computer_leases", [])?; // recover.rs:360
```

Neither table has an `owner_instance` or a fencing token; `write_leases` also
has no expiry, while `computer_leases` has `expires_at`/`generation` but no
owner instance (`schema.rs:2309-2315`, `2365-2376`). A second engine starting
on the same data directory therefore clears the first engine's write scopes
(allowing overlapping cross-session writes) and its GUI lease (allowing two
surfaces to drive the same host GUI).

Separately, `ScopeLocks::acquire` with the store backend waits only on an
in-process `tokio::sync::Notify` (`agent.rs:507-557`). If the conflicting lease
is held by another Engine/process, the holder's `Drop` notifies only its own
`ScopeLocks`; the waiter has no polling timer and no DB wakeup, so it can wait
forever (until session cancellation). `persistent_scope_locks_publish_store_leases`
uses the same `ScopeLocks` instance for both parties, so it does not catch this.

**Impact**

- In a shared-data-dir deployment, a daemon restart can silently revoke another
  process's write-lease and GUI-lease protection.
- A Worker blocked behind a foreign write lease can hang indefinitely, holding
  a Worker slot and budget; the session never settles and no orchestration
  decision can free it.

**Repro**

Using two `Engine`s over the same `Store`:

1. Engine A acquires a write lease for `src/api`.
2. Engine B calls `recover_state` (or opens a new daemon instance).
3. Engine B can acquire an overlapping `src/api` lease; the recovery test for
   runtime leases still passes because that table is scoped.
4. Alternatively, Engine B calls `ScopeLocks::acquire` for the same scope and
   Engine A drops its guard; B's future does not complete within the timeout.

**Suggested fix**

- Add `owner_instance`, `fencing_token`, and `expires_at` to `write_leases` and
  `computer_leases`; clear only own or expired rows on startup.
- Give `ScopeLocks` a bounded retry timer with jitter (and optionally a DB
  `changed_at` poll) so a release by another process is observed; never rely on
  a process-local `Notify` for a store-backed conflict.
- If two live daemons on one data directory are explicitly unsupported, remove
  the opposing comments from the store docs and make recovery fail closed
  instead of wiping other owners' rows.

**Missing test**

Extend `recovery_clears_own_runtime_leases_but_preserves_live_foreign_rows` to
write and computer leases, and add a two-`ScopeLocks` waiter test that releases
the foreign guard and expects the waiter to acquire.

## F9 — Task worktrees and branches are not covered by retention/cleanup

**Evidence**

`cleanup_worktrees` iterates sessions only (`engine/worktrees.rs:102-116`) and
removes `session.worktree` plus the session branch (`worktrees.rs:199-225`).
Task worktrees are created under
`<data>/worktrees/<project>/<session>/<task>` with branch
`verlinpark/<session>--<task>` (`worktree.rs:346-351`). Removing the session
worktree does not remove those siblings, and `delete_branch` is called with the
session branch only. No other production path (daemon sweep, agent cleanup,
retention policy) iterates task worktrees; `project.cleanup_worktrees` is the
only entry point and it has no task entries in its result.

This contradicts `docs/AUDIT.md:954`, which says task worktrees and branches are
now covered by `project.cleanup_worktrees` / the retention policy.

**Impact**

- Disk and Git ref growth is linear in the number of tasks (and in session
  history), independent of the retention policy.
- Operators see "removed worktree" for the session while the task checkouts and
  branches remain, so cleanup does not reclaim the storage they expect.
- `keep_failed` only protects the session worktree; task worktrees of failed
  sessions are also retained regardless of the flag.

**Repro**

Run a plan task to create a task worktree. Finish the session. Call
`project.cleanup_worktrees` with `delete_branches=true` and inspect the result:
it lists only the session; `.git/worktrees` and
`<data>/worktrees/<project>/<session>/<task>` and the
`verlinpark/<session>--<task>` branch still exist.

**Suggested fix**

For each terminal session, enumerate its agent/task worktrees (from `agents` or
by scanning the session directory) and remove those whose task rows are
terminal, applying the same dirty/`keep_failed`/retention guards. Delete the
task branches when `delete_branches` is set. Alternatively, correct the audit
note and add a separate explicit task-worktree cleanup command; but the current
claim is not true.

**Missing test**

A cleanup test that creates a session plus a task worktree and asserts both the
session and the task path/branch are handled according to the policy.

## F10 — Program budgets are not reserved across parallel milestones

**Evidence**

`effective_budget` computes the program allowance from
`program_usage.token_spent` / `cost_spent_usd`, which are settled usage, not
reservations (`program.rs:1181-1230`). `start_program_milestone` calls
`budget_for_start` before creating the session
(`agent/service/programs.rs:105-200`, call at `127-134`). `reconcile_programs` can start up to 10
ready candidates per tick, and `ready_candidates_in_project` may return up to
`max_parallel_milestones` (up to 10) per program
(`program.rs:655-767`). Two milestones starting before either has spent tokens
each receive the entire remaining program budget as their session budget.

**Impact**

- With a program budget of 10k and two parallel milestones, each session can be
  granted ~10k, so the program can spend ~20k before the next tick observes the
  overrun. The cumulative program cap is not a hard boundary under parallelism.
- The same applies to cost budgets. This is a budget-enforcement gap, not just
  an observability issue; it can overspend real provider money.

This is already listed as a limitation in `docs/AUDIT.md` ("program budget 不是
全局 preflight reservation"), so it is a confirmed known gap rather than a
newly discovered state-machine bug, but it belongs in this review because the
scope explicitly includes program budgets and retries.

**Repro**

Create an active program with a 10k token budget, two ready independent
milestones, `max_parallel_milestones=2`, and each milestone budget >10k. Start
both before any usage is recorded; inspect the two sessions' token budgets:
both can be ~10k. Let both spend; `program_usage` exceeds 10k.

**Suggested fix**

Reserve at claim/start time, like the session/plan/task budget registry: grant
each milestone `min(remaining_program, fair_share)` and update the program
reservation atomically. On completion/cancel, release the unused portion. At
minimum, divide the remaining program budget by the number of active/starting
milestones and clamp each session budget accordingly.

**Missing test**

A deterministic two-milestone program test with a small budget that asserts the
sum of granted session budgets never exceeds the program budget.

## Additional observations (not in the top ten)

- **Intake `dispatching` recovery**: `reconcile_intake_tasks` scans only
  `status='running'` (`agent/service/intake.rs:204-210`), while
  `count_active_intake_tasks` counts `dispatching` toward
  `max_intake_parallel` (`vp-store/src/store/intake.rs:280-288`). A dispatch
  future that is aborted/panics after `claim_intake_task` but before
  `start_intake_task` leaves a row that no tick reconciles; it consumes a
  parallel slot and cannot be edited/deleted (only explicitly cancelled). A
  daemon restart repairs it via `recover_state`; an in-process failure does not.
  Low severity, but easy to close by including `dispatching` in the
  reconcile query with an age/lease bound.

- **Lost session lease is never observed**: `SessionLease` exposes `is_lost`
  and sets it on heartbeat failure (`engine/leases.rs:20-23,445-448`), but no
  run-loop or tool-boundary caller checks it; `run_agent` does not receive the
  lease (`agent/service_run/start.rs:522-...`). Another process can acquire the
  session after the TTL while this process keeps executing. This is a
  conditional cross-process gap (the integration lease is asserted at the merge
  boundary); the session lease should be asserted at turn/tool boundaries or
  the lost flag removed to avoid implying enforcement.

- **`finish_plan_task` has no transition predicate**: this is the shared root of
  F1 and F5. Even outside those scenarios, any late/duplicate finish from an
  older run can overwrite a terminal row. A guarded update
  (`status NOT IN ('completed','skipped','cancelled')`) plus an expected-status
  parameter would make the state machine explicit.

- **Stall count is not reset on claim**: `claim_plan_tasks_with_events` resets
  `last_progress_at`, `last_turn`, and `last_tool` on the progress row but not
  `stall_count` (`vp-store/src/store/plans/tasks.rs:86-98`). A retry or plan
  revision with the same task id inherits the previous stall count, so a new
  attempt can reach `stall_min_observations` sooner than intended. The recovery
  row's `last_stall_count + min` guard mitigates repeated recovery, but the
  observation count itself is stale.

- **Mutation budgets are bypassable via `shell`**: `mutation_delta` counts
  commit-like shell commands only when the normalized string literally contains
  `git commit` (`contract.rs:1568-1571`), and sets `writes=0`/`bytes=0` for
  shell (`contract.rs:1553-1563`). A Worker can perform writes or
  `git -C ... commit` without consuming the contract's write/byte/commit caps.
  This is partially documented as bounded shell analysis, but the optional
  mutation caps are presented as enforcement.

## Test gaps

The following are directly relevant to the findings above and are not covered
by the existing tests (all targeted tests below pass, so these are gaps rather
than known failures):

1. Batch abort with one deferring/cancelled task and one successfully merged
   sibling (F1).
2. `skipped` dependency behavior through a controller tick plus scheduler tick
   (F4).
3. Re-submission of the same plan and preservation of completed/attempt state
   (F3).
4. Two concurrent revisions with the same `expected_revision` (F6).
5. A >2,000-event session revision-numbering/history test (F2).
6. Resolve-after-expiry before the gate sweep, and archived-session gate sweep
   (F7).
7. Two-Engine/two-process write-lease and GUI-lease recovery/wake tests (F8).
8. Cleanup of task worktrees/branches (F9).
9. Parallel program milestones with a shared program budget (F10).
10. Cancel during `VERLINPARK_INTEGRATION_COMMAND` (F5).
11. Intake `dispatching` recovery on a tick without a daemon restart.

Useful existing tests that already cover adjacent behavior:

- `cargo test -p vp-store --lib gate::tests::` — 10 passed.
- `cargo test -p vp-core --lib scheduler::tests::` — 9 passed.
- `cargo test -p vp-core --lib agent::tests::settles_sibling_tasks_when_a_batch_is_cancelled`
  — 1 passed (covers the all-cancelled case only).
- `cargo test -p vp-core --lib plan_revision_records_a_diff_and_rejects_protected_tasks`
  — 1 passed (sequential revision only).
- `cargo test -p vp-store --lib recovery_clears_own_runtime_leases_but_preserves_live_foreign_rows`
  — 1 passed (runtime leases only, not write/GUI leases).

No live provider calls and no full-workspace `cargo test` were run; the targeted
suites are enough to confirm the baseline is green and that the findings are
not already exposed by the existing assertions.

## Clean areas (no defect found in this pass)

- Task claim/retry/block/cancel store transitions are atomic with their
  `plan_task.*` events and are guarded by current status and attempt gates;
  concurrent claims are serialized by the SQL update.
- `sync_plan_tasks` is idempotent for an unchanged `plan_seq`; it correctly
  reconstructs attempts from `plan_task.started` and terminal status from
  finished/interrupted/cancelled events.
- Effect receipts: unknown/intent block merge/land, tokens and receipts are
  bounded, and the integration path re-checks effects under the integration
  lease. The sweep is live-session-aware and only replaces a synthetic
  `stale_intent_sweep` unknown with a real completion.
- Gates: token hashing, one-time compare-and-set, conflict/idempotency handling,
  and continuation wake are transactional; the defect is limited to the
  missing `expires_at` check and archived-session state.
- Contracts and MCP pins: contract hashes cover the trusted inputs; MCP trust
  is checked before execution and independent of contract mode; forbidden paths
  and egress denials fail closed.
- Workspace lifecycle: component/session branch validation, session base
  history checks, owner-fenced release, Labour merge rollback, and
  `workspace.land` acceptance checks are consistent with the S4 description.
- Identity assignment/workspace resolution: `prompt_identity_for_run` requires
  an active session assignment with a live lease and enforces project/role
  matching; no assignment bypass was found in the run paths reviewed.
- Program milestone claim/attach/reconcile grace handles the crash between
  claim and session attach; dependency policy, retry backoff, and manual retry
  work as described.
- Scheduler admission/aging/round-robin is deterministic and its unit coverage
  is reasonable; the main gap found is the skipped-dependency predicate, not
  the ordering itself.
- Integration lease fencing (`process_merge_entry_fenced`) correctly asserts
  the token before merge, after merge, and rolls back on loss.

## Assumptions and scope limits

- The review is static plus targeted unit tests. No live provider, daemon, or
  GUI session was used. Findings F1, F3, F4, F5, F6, F7, F9, and F10 are
  provable from the code; F2's effect requires a session with more than 2,000
  events; F8 requires two engines/processes sharing one data directory (the
  store explicitly supports this, while the normal daemon instance lock makes
  it a non-default deployment).
- Cancellation is cooperative by design; an in-flight provider stream or tool
  is allowed to finish. F5 is specifically about the window after the Verifier
  has finished and while the deterministic Integrator/command runs, which is
  within the runtime's control rather than an uninterruptible provider stream.
- Skipping semantics are ambiguous in the documentation: the store comment says
  downstream work remains blocked, while the controller treats `skipped` as
  satisfied. F4 is the inconsistency itself; the fix must choose one policy.
- The review did not attempt to prove the absence of all races. It focused on
  the requested plans/tasks/scheduler/scaling/program/identity/workspace/lease/
  continuation/gate/effect/contract paths and on state transitions that can
  lose work or report contradictory state.
- File/line references are against `d239dca`. No commit was created and no
  repository file other than this report was written.

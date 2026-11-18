# Replay, Remote, Backup, A2A, and Progress Reliability Review — 2026-11-19

- Repository: `/Users/apple/orca/workspaces/VerlinPark/m3-round`
- Baseline: `d239dca` (`Fix live provider compatibility and budget preflight`)
- Reviewer: subagent (senior Rust reliability review)
- Mode: read-only; no source, test, doc, or config changed. The only file created is this report.
- Scope: `vp-core` replay/repair/journal/import/export, remote workspaces and
  integration, backup/restore/rekey/anchor, `vp-remote`, `vp-remote-agent`,
  `vp-a2a`, `a2a.rs`, `progress_ledger.rs`, `engine/collaboration.rs`, and the
  remote tool modules.

## Executive summary

The replay and journal machinery is carefully bounded in most places: event
mirrors are checked for contiguity before restore, run-journal preflight checks
provider/model/budget/plan/worktree before a provider call, duplicate-write
claims are released on failure and on restart, remote bundle chunks and the
imported commit are independently hashed and head-checked, backup blobs and
manifests use authenticated envelopes with strict allowlists, and the A2A
facade is genuinely loopback-only and card-only. The backup crypto and remote
path-jail code in particular read as intentionally defensive rather than
incidental.

The review nevertheless found **ten material replay/durability defects**, four
of which can silently corrupt or reinterpret durable state:

1. **Plan revisions break replay and repair.** A `team_plan.revised` event is
   compared against the stored plan as if its wrapper were the plan, and repair
   writes that wrapper into `sessions.team_plan`. After one plan revision,
   `session.replay` reports drift; after `--repair` or `db-rebuild`,
   `get_team_plan` returns `None` and plan editing/resume is broken.
2. **Task replay ignores revised plans.** `expected_tasks` always rebuilds from
   the last `team_plan.submitted` event. A revision-added task is treated as
   drift by replay and is deleted by repair.
3. **Gate deferral never finishes its run journal.** `finish_deferred` passes
   status `deferred` to a store that only accepts `completed|failed|interrupted`.
   The error is logged as a warning; the run row remains `running` and becomes
   resumable after a daemon restart, contrary to the durable-gate design.
4. **Merge-queue replay is both over-inclusive and content-blind.** It folds
   any event payload containing `entry.id`/`entry.status` (including
   `test_cache.stored`), producing permanent false drift that repair cannot
   remove, while comparing only id/status so a changed commit or Verifier
   verdict is not detected.
5. **Progress drift is excluded from `consistent`.** `session.replay` reports
   `consistent=true` while listing a progress mismatch, and
   `session.replay --repair` returns without repairing it.

The remaining findings are repair data loss (`next_attempt_at`), incomplete
export/import of event-backed projections, a false “full” backup-verification
label above 100k blobs, unbounded remote Git output buffering, a run-journal
replay comparison that ignores the C4 runtime snapshot, and
lower-probability duplicate/idempotency and secret-handling gaps. The
2,000-event plan-revision window is also confirmed; it overlaps
`03-orchestration-state.md` F2 and is listed as an overlap rather than a new
primary finding.

## Findings

Severity is impact if the path is reached; confidence is that the described
path exists at this baseline.

| # | Severity | Confidence | Area | Location | Summary |
|---|---|---|---|---|---|
| F1 | High | High | Plan revision replay/repair | `crates/vp-core/src/engine/runs/replay.rs:689-710`; `crates/vp-core/src/engine/runs/repair.rs:31-47` | Revised-plan payload is compared/stored as the plan; repair corrupts `sessions.team_plan` and breaks plan reads |
| F2 | High | High | Task DAG replay | `crates/vp-core/src/engine/runs/support.rs:113-134`; `crates/vp-core/src/scheduler.rs:113-128` | Task projection is rebuilt only from `team_plan.submitted`; revision-added tasks are dropped by repair |
| F3 | High | High | Gate deferral journal | `crates/vp-core/src/run_journal.rs:588-598`; `crates/vp-store/src/store/runs.rs:426-432` | `deferred` is not an accepted run status; a gate wait leaves a stale `running` run that can be auto-resumed after restart |
| F4 | Medium-High | High | Merge-queue replay | `crates/vp-core/src/engine/runs/replay.rs:781-800` | Replay folds unrelated `entry` payloads (test cache) and compares only id/status, causing false drift and missing commit/verifier drift |
| F5 | Medium | High | Replay consistency gate | `crates/vp-core/src/engine/runs/replay.rs:581-588,890-924`; `crates/vp-core/src/engine/runs/repair.rs:457-460` | `progress_matches` is excluded from `consistent`; progress-only drift is never repaired |
| F6 | Medium | High | Repair task-row loss | `crates/vp-store/src/store/backup/repair.rs:279-309` | Repair omits `next_attempt_at`; retry backoff is erased and not detectable by replay |
| F7 | Medium | High | Archive completeness | `crates/vp-core/src/engine/runs/export.rs:232-286`; `crates/vp-core/src/engine/runs/import.rs:41-102`; `crates/vp-protocol/src/types.rs:11282-11465` | Export/import omits contracts, contract usage, progress, and remote-test evidence projections |
| F8 | Medium | High | Backup verification | `crates/vp-core/src/backup/repository/mod.rs:892-947`; `crates/vp-core/src/backup/repository/verify.rs:202-277` | Full verification silently stops at 100k unique blobs but still reports `blob_verification="full"`/`valid=true` and may advance the anchor |
| F9 | Medium-Low | High | Run-journal replay | `crates/vp-core/src/run_journal.rs:1414-1449` | `runs_match` omits `runtime_snapshot` (and `agent_name`/`objective`), so a lost C4 run pin is not detected as projection drift |
| F10 | Medium-Low | High | Remote Git bounds | `crates/vp-remote/src/git.rs:63-105`; `crates/vp-remote/src/protocol.rs:39` | Git subprocess stdout/stderr are fully buffered with `wait_with_output`; the 8 MiB cap is applied only after allocation |

## F1 — Revised-plan replay compares/stores the wrong shape and corrupts the stored plan

**Evidence**

`team_plan.revised` payloads are built as a wrapper, not as a bare plan
(`engine/plans/revisions.rs:68-82` / `fanout.rs:884-913`):

```json
{ "plan": { "goal": ..., "departments": [...], "tasks": [...] },
  "revision": 2, "reason": "...", "author": "...", ... }
```

`sessions.team_plan` stores the bare plan
(`store/plans/snapshots.rs:71-105`; `Engine::get_team_plan` parses this string
directly at `engine/plans/revisions.rs:239-244`).

Replay selects the latest `team_plan.submitted|revised` event, removes only
`runtime_snapshot`, and compares that payload to the stored plan
(`replay.rs:689-710`):

```rust
.find(|event| matches!(event.kind.as_str(),
    "team_plan.submitted" | "team_plan.revised"))
.map(|event| {
    let mut payload = event.payload.clone();
    if let Some(object) = payload.as_object_mut() {
        object.remove("runtime_snapshot");
    }
    payload
});
let projected_plan = data.plan.as_deref()
    .and_then(|plan| serde_json::from_str::<serde_json::Value>(plan).ok());
let plan_matches = replay_plan == projected_plan;
```

For `submitted` this works because the payload is the plan plus an optional
snapshot. For `revised` it always differs: the stored value is the plan object,
while the replayed value is `{plan, revision, reason, ...}`.

`build_session_repair` has the same assumption (`repair.rs:31-47`): it
serializes the revised event payload (minus the snapshot) and writes it to
`sessions.team_plan`. After repair, `get_team_plan` deserializes
`TeamPlan` directly from that wrapper and fails, returning `None`.

**Impact**

- `session.replay` reports `plan_matches=false` and
  `task_queue_matches=false` on every session that has ever used a plan
  revision (task edit, plan restore, fan-out, or any `team_plan.revised`
  producer).
- `session.replay --repair` and `vp db-rebuild` (through
  `restore_session_log` → `rebuild_session` → `build_session_repair`) appear to
  succeed but store a wrapper in `sessions.team_plan`.
- After that, `vp plan`, `plan_revision_detail`-driven restore, task editing,
  and plan resume all fail or see no plan. The session’s authoritative plan is
  effectively lost from the public API while the event stream still contains
  it.
- `session.export` reads `data.plan` through `serde_json::from_str::<TeamPlan>`
  (`export.rs:76-79`), so exporting a repaired session also loses
  `bundle.team_plan`; importing that bundle starts with no plan projection.

**Reproduction**

1. Create a session and submit a plan.
2. Revise the plan (add a task or change a title) so a `team_plan.revised`
   event exists.
3. Run `vp replay <session>`: observe the plan/task mismatches.
4. Run `vp replay <session> --repair` (or `vp db-rebuild`).
5. Run `vp plan <session>` / attempt a plan resume: no plan is returned; the
   task queue no longer matches the revised event.

**Fix**

- Compare the **plan value**, not the event envelope. Reuse
  `crate::orchestration::plan::plan_from_event` (which handles both the
  submitted bare shape and the revised `plan` field) in both `replay.rs` and
  `build_session_repair`.
- In repair, serialize `plan_from_event(event)` into `sessions.team_plan`, and
  keep the event payload as-is for audit.
- Add a regression that submits, revises, replays, repairs, and then asserts
  `get_team_plan` still returns the revised plan and the event stream is
  unchanged.

**Test to add**

`revised_plan_replays_and_survives_repair`: submit P1, revise to P2, run
`replay_session`, `repair_session`, and `rebuild_session`; assert
`plan_matches`, `get_team_plan() == Some(P2)`, and a subsequent
`edit_plan_task`/`restore_team_plan_revision` still works.

## F2 — `expected_tasks` ignores revised plans and repair deletes revision tasks

**Evidence**

`expected_tasks` finds only the latest **submitted** plan and uses that
submitted event’s sequence as `plan_seq`
(`crates/vp-core/src/engine/runs/support.rs:113-134`):

```rust
let plan_event = data.events.iter().rev()
    .find(|event| event.kind == "team_plan.submitted");
...
let plan = serde_json::from_value::<TeamPlan>(event.payload.clone()).ok()?;
(event.seq, plan, event.created_at.clone())
```

The live scheduler, however, rebuilds task rows from the latest submitted
**sequence** but the **current revised plan** (`scheduler.rs:113-128`):

```rust
let plan_seq = store.latest_event_seq(&session_id, "team_plan.submitted")?;
let tasks = store.sync_plan_tasks(&session_id, &project_id, plan_seq, &plan)?;
```

`sync_plan_tasks` deletes and reinserts all rows using the supplied plan
(`store/plans/tasks.rs:184-233`). A revision that adds `T3` therefore produces
rows with the same `plan_seq` as the original submission but includes `T3`.
`expected_tasks` returns only the original P1 tasks, so:
replay reports `task_queue_matches=false` even when the projection is correct,
and repair deletes/reinserts P1’s task set, dropping `T3` and any status
recorded for it.

**Impact**

- Task queue drift is reported forever after any plan revision/fan-out/task
  edit.
- `session.replay --repair`, `vp db-rebuild`, and any path using
  `build_session_repair` silently remove revision-added tasks and can revert
  the status of surviving tasks to the state inferred from the original plan.
- Because the revised plan itself is also broken by F1, the resulting state is
  internally inconsistent: `sessions.team_plan` may contain the new plan while
  the task rows contain the old one.
- Plan Workers/Labour dispatch can see missing tasks or re-run work that the
  plan revision added.

**Reproduction**

1. Submit P1 with `T1,T2`.
2. Revise to P2 adding `T3`.
3. `vp replay` → task queue mismatch; `vp replay --repair` → `vp tasks` has no
   `T3`.
4. `vp db-rebuild` (or `session.restore_log`) repeats the same loss.

**Fix**

- Select the latest `team_plan.submitted|revised` event and use
  `plan_from_event` to obtain the plan, while retaining the **submitted**
  event’s sequence for the `plan_seq`/task-event window (or explicitly migrate
  `plan_seq` and all task events together if the model changes).
- Compare the remaining durable fields (`next_attempt_at`) as well; see F6.
- Add a test that revisions add, rename, and remove tasks and assert
  `task_queue_matches` and repair idempotence.

**Test to add**

`plan_revision_added_task_survives_replay_and_repair`: add `T3` in a revision,
complete it, then replay/repair; assert `T3` still exists with its completed
status and attempts.

## F3 — `finish_deferred` uses an invalid run status and leaves a resumable `running` run

**Evidence**

`RunJournal::finish_deferred` explicitly documents the gate handoff as a clean
terminal state (`crates/vp-core/src/run_journal.rs:588-598`):

```rust
self.finish(engine, "deferred", Some(failure)).await
```

`finish` calls `store.finish_session_run(&run_id, &status, ...)`
(`run_journal.rs:600-621`). `finish_session_run` validates the status against
only three values (`crates/vp-store/src/store/runs.rs:426-432`):

```rust
validate_run_status(status, &["completed", "failed", "interrupted"])?;
```

`deferred` therefore always returns an error. `run_loop` logs
`failed to finish durable run journal` and returns
`LoopOutcome::Deferred`; the session is finalized idle and the gate
continuation is created, but no terminal `session.run.*` event is appended and
the `session_runs` row remains `running`.

**Impact**

- `session.runs` shows a phantom running run after every gate wait;
  `session.run.detail` and the Stage runtime badge cannot represent the
  deferred handoff.
- `session.run.cancel` finds a `running` row but no live run control in the
  daemon and reports `cancelled=false`; the stale row remains.
- On daemon restart, `interrupt_running_session_runs` marks the row
  `interrupted` with failure kind `interrupted` (not `deferred`), so
  `session.run.resume` and opt-in auto-resume can resume a run that the gate
  design says must not be replayed mid-wait. With multiple stale runs in one
  session, auto-resume may choose the old deferred attempt instead of the
  newer run.
- This defeats ADR-0195’s “no run is held / resume mid-wait is refused”
  behavior even though the gate continuation itself works.

**Reproduction**

1. Open a gate and run a Leader/Manager/Worker that calls `wait_for_gate`.
2. Inspect `vp runs <session>` / `session.runs`: the run is `running` while the
   session is idle and a continuation is pending.
3. Restart the daemon.
4. `vp runs` now shows `interrupted`; `vp run-resume` can start it (policy
   permitting), contrary to the deferred-wait design.

**Fix**

- Add a first-class `deferred` terminal status and `session.run.deferred`
  event (or map deferred to an accepted terminal status with a persisted
  failure kind), update `projections_from_events`/`runs_match`, and make
  `preflight`/auto-resume refuse it.
- Alternatively, keep `interrupted` but persist `failure.kind =
  "deferred"` and make resume/auto-resume reject that kind; do not leave the
  status `running`.
- Add a gate test that inspects `session.runs` immediately after the deferral
  and after a simulated restart.

**Test to add**

`gate_deferral_finishes_the_run_as_deferred_and_blocks_resume`: run the
`wait_for_gate` tool, assert one run with status `deferred`, restart/recover,
assert `session.run.resume` and auto-resume both refuse it.

## F4 — Merge-queue replay is kind-blind (test cache false drift) and content-blind (missed commit/verifier changes)

**Evidence**

`replay_report` builds the replay queue from every event with an `entry` object
(`crates/vp-core/src/engine/runs/replay.rs:781-800`):

```rust
let replay_queue = data.events.iter()
    .filter_map(|event| {
        let entry = event.payload.get("entry")?;
        Some((entry.get("id")?.as_str()?.to_string(),
              entry.get("status")?.as_str()?.to_string()))
    })
    .fold(HashMap::new(), ...);
```

There is no `merge_queue.*` kind filter. `test_cache.stored` has exactly this
payload shape (`crates/vp-store/src/store/test_cache.rs:283-292`): an `entry`
with `id` and `status` (`passed|failed|error`). Any session that used
`run_project_task` and stored a result therefore adds a test-result id to
`replay_queue`. `projected_queue` only contains merge entries, so
`queue_matches=false` even though the merge queue is correct. Because the
events are the authority and repair rebuilds the queue from the same events,
`session.replay --repair` **cannot** clear this mismatch; the session remains
permanently “inconsistent”.

Conversely, the comparison retains only `id` and `status`. The event payload
contains the full `MergeQueueEntry` (`store/merge.rs:283-310,319-360`), but a
projection whose `commit`, `base_commit`, `verifier_approved`,
`verifier_summary`, remote provenance, or `remote_test_*` fields were changed
while keeping the same status compares equal. `session.replay` therefore does
not detect exactly the merge-queue fields that determine what the Integrator
merges and whether verification was recorded.

**Impact**

- Permanent false replay drift on normal sessions with test-cache entries;
  `consistent` can never be true and `vc replay --repair` keeps reporting a
  mismatch.
- Missed corruption of a queue row’s commit/base/verifier metadata; a
  later retry or UI inspection can act on a commit that the event stream never
  approved.
- The same false drift can make imported archives (which often carry
  test-cache events) look corrupt (see F7).

**Reproduction**

1. Run a project task through `run_project_task` so a `test_cache.stored`
   event exists.
2. `vp replay <session>` → merge-queue mismatch.
3. `vp replay --repair` → still mismatch (or at least no way to converge),
   because replay re-reads the test-cache event on every attempt.
4. Separate check: alter only `merge_queue.commit_hash` or
   `verifier_approved` in SQLite while keeping `status`; replay still reports
   `queue_matches=true`.

**Fix**

- Filter to `merge_queue.enqueued` and `merge_queue.updated` and deserialize
  the full `MergeQueueEntry` from `event.payload["entry"]`, as
  `build_session_repair` already does (`repair.rs:131-158`).
- Compare the full durable entry (at minimum commit/base/status/attempt/
  verifier fields/provenance; `created_at`/`updated_at` can be compared too
  because the event carries them).
- Optionally, treat `test_cache.stored` entries as a separate projection and
  never fold arbitrary `entry` payloads.

**Test to add**

`test_cache_events_do_not_create_merge_queue_drift` and
`merge_queue_replay_detects_commit_and_verifier_drift`.

## F5 — Progress drift is excluded from `consistent`, so `--repair` never fixes it

**Evidence**

`replay_report` computes `progress_matches` and pushes a `progress` mismatch
(`crates/vp-core/src/engine/runs/replay.rs:581-588`), but the `consistent`
conjunction at `replay.rs:890-924` never includes `progress_matches`.
`repair_session` then uses `report.consistent` as its gate
(`crates/vp-core/src/engine/runs/repair.rs:457-460`):

```rust
let report = Self::replay_report(&data, false)?;
if report.consistent {
    return Ok(report);
}
```

A session whose only drift is missing/corrupt `progress_rounds` returns
`consistent=true` with a non-empty `mismatches` vector; repair returns
immediately. The repair code that could rebuild progress exists in
`build_session_repair` (`repair.rs:300`) and
`repair_session_projections` (`store/backup/repair.rs:405-431`), but it is
unreachable for a progress-only drift.

**Impact**

- Operators and UI see “consistent” while the report still lists a progress
  mismatch.
- The advisory progress ledger projection can remain stale/corrupt forever;
  `session.progress` reads the stale projection, not the events.
- A replay/repair regression test for progress would fail if it asserted
  `consistent` rather than only checking `mismatches`.

**Reproduction**

1. Record a `progress.round` event or import an archive that carries one.
2. Delete/alter the corresponding `progress_rounds` row (or simulate a crash
   before its projection write; the event is already committed).
3. `vp replay` → `consistent=true`, mismatch text includes `progress`.
4. `vp replay --repair` → no progress repair event/round restoration.

**Fix**

Add `&& progress_matches` to the `consistent` conjunction and keep the
existing `expected_progress` repair path. Add an explicit test for
progress-only drift.

**Test to add**

`progress_only_drift_is_inconsistent_and_repair_rebuilds_it`.

## F6 — Repair erases `next_attempt_at` and hides it from drift detection

**Evidence**

`ScheduledTask` carries `next_attempt_at` (`crates/vp-store/src/store.rs:553`),
and the scheduler uses it to gate retries (`crates/vp-core/src/scheduler.rs:190-208`).
`repair_session_projections` inserts task rows without that column
(`crates/vp-store/src/store/backup/repair.rs:279-309`):

```sql
INSERT INTO plan_tasks
 (session_id, plan_seq, task_id, project_id, department, title, priority,
  status, deps, write_scope, attempts, last_error, enqueued_at, started_at,
  finished_at, updated_at)
VALUES (...)
```

The column is left at its default `NULL`. `same_expected_task`
(`crates/vp-core/src/engine/runs/support.rs:289-302`) also does not compare
`next_attempt_at`, so replay will not report the loss even after repair.

**Impact**

- A retry scheduled with exponential backoff becomes immediately eligible
  after any repair triggered by unrelated drift (or by F1/F2), causing early
  retries instead of respecting the controller’s delay.
- `task.queue`/orchestration snapshots lose the scheduled time, and
  `orchestration.decision` may re-attempt a failing task early.
- The loss is silent because replay compares a subset of task fields.

**Reproduction**

1. Cause a task to enter `retry_scheduled` with a future `next_attempt_at`.
2. Introduce an unrelated projection drift (e.g., a stray approval row), then
   `vp replay --repair`.
3. `vp tasks` shows the task `pending` with `next_attempt_at=null` and
   `task_queue_matches` still true on a subsequent replay.

**Fix**

Include `next_attempt_at` in the repair INSERT and parameters; compare it in
`same_expected_task` (or document it as intentionally operational only if the
runtime stops using it for retry gating). Add a repair round-trip test.

**Test to add**

`scheduled_retry_backoff_survives_session_repair`.

## F7 — Session export/import omits event-backed contract, progress, and remote-test projections

**Evidence**

`SessionBundle` has no `contracts`, `contract_usage`, `progress`, or
`remote_test_evidence` fields (`crates/vp-protocol/src/types.rs:11282-11465`).
`export_session` gathers the replay data but its return literal
(`crates/vp-core/src/engine/runs/export.rs:232-286`) includes
`effects`, `test_results`, `recoveries`, etc., and not those four projections.
`import_session` rebuilds only notes/decisions/scaling/recoveries, run journal,
context checkpoints, test results, and effects from events
(`crates/vp-core/src/engine/runs/import.rs:41-102`). Contracts and progress
are not rebuilt, and `store.import_session_bundle` does not insert
`remote_test_evidence` either.

Because the bundle **does** carry the original events, `load_replay_data` after
import sees `contract.derived`, `intent.mutation`, `progress.round`, and
`remote_test.completed` events but empty projections. `session.replay` then
reports `task contracts`, `contract usage`, `progress`, and
`remote test evidence` mismatches depending on which events are present. This
is not a harmless legacy case: any real plan run emits contract events, and
any `run_project_task` may emit test-cache events that interact with F4.

**Impact**

- `vp export`/`vp import` is not a replay-consistent round trip for modern
  sessions. Imported archives can appear corrupt immediately.
- Contract usage and progress are lost from the public `session.contract`/
  `session.progress` views until an explicit `session.replay --repair`
  (which mutates the imported archive despite its read-only label).
- Remote test evidence rows are not exported at all, so a reviewer cannot
  inspect them after archive import; only the raw event remains.
- A subsequent `vp export` of the imported archive omits them again, so the
  loss is cumulative.

**Reproduction**

1. Run a session through a plan/task that derives contracts and records usage;
   optionally record a progress round and remote-test evidence.
2. `vp export` then `vp import` into a fresh data directory.
3. `vp replay <session>` reports contract/usage (and progress/remote-test)
   mismatches; `vp contract`/`vp progress` are empty.

**Fix**

Add the missing event-backed projections to `SessionBundle` with
`#[serde(default)]` fields, export them, and rebuild them from events in
`import_session_bundle`/`import_session` like test results and effects. For
`remote_test_evidence`, export the metadata rows (not output bodies) and
reconstruct from `remote_test.completed` when present. Add an export/import
round-trip test that asserts `consistent`.

**Test to add**

`planned_session_export_import_replays_consistently_with_contracts_progress_and_remote_evidence`.

## F8 — `backup-verify` labels a capped verification “full” and can advance the anchor

**Evidence**

Both list and verify iterate referenced blobs with
`if verify_blobs && verified_cache.len() < BLOB_VERIFY_CACHE_LIMIT`
(`crates/vp-core/src/backup/repository/mod.rs:892`;
`crates/vp-core/src/backup/repository/verify.rs:202`). The constant is 100,000
(`mod.rs:114`). After the 100,000th unique blob the loop silently stops
calling `verify_blob_file`; `blobs_verified` stays below `blobs_present`, but
the report still sets:

- `blob_verification: "full"` (`mod.rs:968`, `verify.rs:350-354`);
- `valid = ... blobs_missing == 0 && blobs_corrupt == 0 ...` without checking
  `blobs_verified == blobs_present` (`mod.rs:947`, `verify.rs:271-277`);
- in `verify_repository`, if `valid` and generation > 0, the anchor is updated
  (`verify.rs:281-300`).

The doc comment on `list_repository_snapshots` promises that `verify_blobs`
“decrypts and hashes every unique referenced blob” (`mod.rs:783-787`). The cap
is a cache cap, not an intended verification horizon.

**Impact**

- A repository with >100,000 unique blobs can have a corrupt blob beyond the
  cap; `backup-verify` reports `valid=true` and `blob_verification="full"`,
  and advances the local anti-rollback anchor.
- A later restore fails on the corrupt blob, but the anchor may already have
  moved forward, making a rollback to the last good generation require
  `--allow-rollback` and reducing the value of the anti-rollback check.
- `backup-list --verify` makes the same false “full” claim.

**Reproduction**

1. Create a repository with 100,001 distinct small files/blobs.
2. Corrupt the blob that sorts after the first 100,000 referenced hashes.
3. `vp backup-verify` → `blobs_present=100001`, `blobs_verified=100000`,
   `blob_verification="full"`, `valid=true`; the anchor generation is
   recorded.
4. Restore the snapshot → fails on the corrupt blob.

**Fix**

Do not cap full verification; if a hard work cap is required, report
`blob_verification="partial"`, add a bounded issue/warning,
set `valid=false`, and do not update the anchor when verification is
incomplete. Add a test with a lowered cap (or a test-only cap) asserting
partial status and no anchor update.

**Test to add**

`full_verification_cap_is_reported_partial_and_does_not_advance_anchor`.

## F9 — `runs_match` ignores the C4 runtime snapshot and other durable run fields

**Evidence**

`projections_from_events` reconstructs `SessionRun` from
`session.run.started/resumed/completed/failed/interrupted` events, including
`runtime_snapshot` (`crates/vp-core/src/run_journal.rs:1350-1412`).
`runs_match` compares a subset (`run_journal.rs:1414-1449`) and omits
`runtime_snapshot`, `agent_name`, and `objective`. Its own doc comment says only
`last_event_seq` is operational and excluded (`run_journal.rs:1414-1416`).

**Impact**

- A run row whose `runtime_snapshot_json` was lost, truncated, or replaced is
  reported as consistent; `session.replay --repair` does not run for that drift
  alone.
- C4 compatibility treats a missing run snapshot as `snapshot_missing` drift
  and proceeds rather than failing closed (`runtime_pin` contract), so losing
  the row can weaken the pin rather than surface as projection corruption.
- `agent_name`/`objective` are display/audit fields; their omission is lower
  impact but the same contract inconsistency.

**Reproduction**

1. Create a completed or interrupted run and confirm its event payload
   includes `runtime_snapshot`.
2. Set `session_runs.runtime_snapshot_json = NULL` (or replace it) without
   touching the event stream.
3. `vp replay` → `runs_match=true`, `consistent=true`.
4. `vp compatibility`/`vp run-resume` sees `snapshot_missing` drift and may
   proceed where the original pin would have been incompatible.

**Fix**

Compare `runtime_snapshot` (and `agent_name`/`objective`) in `runs_match`, or
explicitly document and test them as non-replay fields if the C4 pin is
intended to tolerate loss. Given C4, comparing the snapshot is the safer
choice. Add a drift/repair test.

**Test to add**

`replay_detects_a_lost_run_runtime_snapshot_and_repair_restores_it`.

## F10 — Remote Git commands buffer unbounded stdout/stderr before the 8 MiB truncation

**Evidence**

`run_git` uses `child.wait_with_output()` inside the timeout
(`crates/vp-remote/src/git.rs:63-99`). `wait_with_output` collects the entire
stdout and stderr into memory. Only afterward does the helper truncate stdout
to `MAX_GIT_OUTPUT_BYTES` (8 MiB, `protocol.rs:39`) and convert stderr to a
String (`git.rs:100-105`). `git diff`, `git status`, and `git log` all route
through this helper; `git diff` has no request-side byte limit at all.

**Impact**

- A remote repository with a large diff, many untracked files, or huge status
  output can make the helper allocate hundreds of MB or more and OOM before
  the protocol cap is applied.
- This contradicts ADR-0156’s bounded-operation claim; `exec` and bundle
  export already stream with per-stream caps, but structured Git does not.
- The daemon may see a transport close/timeout rather than a typed
  `OutputLimit` error, and the remote host may be left under memory pressure.
- The remote host is trusted infrastructure, so this is a reliability/DoS
  issue rather than a containment escape.

**Reproduction**

1. On the configured remote root, create a repository with a diff larger than
   the helper’s available memory (or a worktree with a very large untracked
   tree).
2. Call `remote_git diff` / `remote_git status` through the tool/RPC.
3. Observe helper RSS grow with the full output before truncation, or the
   helper/SSH session die from memory pressure.

**Fix**

Stream stdout and stderr with independent caps (reuse the `exec`/bundle
reader pattern), kill the process group when either cap is exceeded, and
return a typed `OutputLimit` result with a truncated prefix. Do not use
`wait_with_output` for commands whose output is model-controlled.

**Test to add**

A helper test that produces >`MAX_GIT_OUTPUT_BYTES` of `git diff` output
(fixture repo) and asserts a bounded result/typed error and process-group
cleanup. A memory-limit test is ideal but at minimum assert the response and
stored buffer are capped.

## Cross-report overlaps and additional confirmed low-severity gaps

These are real but either lower impact or already covered by another review in
this batch; they are listed so the merge pass does not lose them.

- **Plan revision numbering uses a 2,000-event window** (`crates/vp-core/src/engine/plans/revisions.rs:4-7,52-53`; `crates/vp-store/src/store/events.rs:118-125`; `crates/vp-core/src/fanout.rs:794-795`). `recent_events(5_000)` is silently clamped to 2,000, so `plan_revisions`, `revise_team_plan`, and fan-out can undercount revisions, assign duplicate revision numbers, return false `expected_revision` conflicts, and hide old revisions. This is also `03-orchestration-state.md` F2; it amplifies F1/F2 above because the replay/repair breakage starts from a valid-looking revision.
- **Remote integration can enqueue twice after a crash** (`crates/vp-core/src/agent/service/merge/remote.rs:640-690,722-746`). The effect receipt is consulted before enqueue but recorded as `done` only after `enqueue_merge_with_source`. A crash after enqueue and before `record_settled_effect` leaves a queue row with no receipt; retrying `remote.integrate` downloads/re-verifies and creates a second row for the same host/branch/commit. There is no queue-level dedupe. Begin the effect intent before the import/enqueue and settle it in the same transaction with the queue row, or check the queue by provenance/commit under the session operation receipt.
- **Secret-bearing structs derive `Debug`** (`crates/vp-remote/src/server.rs:41` `ServerConfig.auth_token`;
  `crates/vp-remote/src/protocol.rs:347` `HelloParams.auth_token`;
  `crates/vp-protocol/src/a2a.rs:173` `A2aStartResult.token`). No current call site
  logs them, and `A2aSecretToken`/`Passphrase` are redacted, but a future
  `?config`/`?result` or panic payload would print a bearer token. Implement
  redacting `Debug` like `GateToken`/`Passphrase`. This is adjacent to
  `06-daemon-protocol.md`’s discovery-token observation.
- **`db-rebuild` silently drops unreadable project source snapshots**
  (`crates/vp-core/src/rebuild.rs:493-500`;
  `crates/vp-store/src/store/sources/snapshots.rs:86-91`). `restore_sources_from_disk`
  is best-effort and `list_source_snapshots` skips malformed JSON with a warning;
  the rebuild report still says success and does not add a `skipped` entry. A
  corrupt source snapshot can therefore disappear from the rebuilt DB without
  an operator-visible failure.
- **Bundle import accepts event `seq` outside `1..=i64::MAX` and does not bound
  event payload size** (`crates/vp-store/src/store/backup/import.rs:1215-1233`;
  `crates/vp-store/src/store.rs:1706-1708`). This is also
  `05-store-sqlite.md` finding 7; it can make a session unreplayable/overflow
  and is not addressed by this report’s replay fixes.
- **Repair instruction-state deletion uses `LIKE '<session>#%'`**
  (`crates/vp-store/src/store/backup/repair.rs:444-448`), which can match a
  case-variant or wildcard-bearing session id. This is `05-store-sqlite.md`
  finding 4; it is a replay/repair correctness and isolation issue worth fixing
  with exact prefix logic.
- **A non-Git error after `begin_merge_queue_entry` can leave a queue row in
  `merging`/`checking`** (`crates/vp-core/src/agent/merge.rs:762-779,874-876,985-1001`;
  only startup repair resets it). This is `01-core-runtime.md` R4; it is not
  re-detailed here.

## Test gaps

The existing suite deliberately covers a lot of replay/repair behavior, but
the following paths have no assertion that would have caught the confirmed
defects above:

1. **Plan revision round trip:** no test submits, revises, replays, repairs,
   `db-rebuild`s, and then verifies `get_team_plan` and revision-added task
   rows. `plan_revision_records_a_diff_and_rejects_protected_tasks`,
   `revision_detail_task_edit_and_restore_are_optimistic`, and the gate tests
   stop before replay/repair.
2. **Gate-deferred journal state:** the two `wait_for_gate` tests assert
   session idle, continuation creation, event emission, and no second provider
   call, but never inspect `session.runs`/`session.run.detail` or simulate a
   restart.
3. **Test-cache and merge-queue replay:** no test creates `test_cache.stored`
   and then calls `replay_session`; no test mutates `merge_queue.commit_hash`
   or `verifier_approved` while keeping status and asserts drift.
4. **Progress-only drift:** no test deletes/corrupts a `progress_rounds` row
   while leaving the event and asserts `consistent=false` plus repair.
5. **Repair field preservation:** no test asserts `next_attempt_at` (and
   `last_error`) survive `repair_session`; `same_expected_task` also does not
   compare them.
6. **Archive completeness:** no export/import test covers `contract.derived`,
   `intent.mutation`, `progress.round`, or `remote_test.completed` events and
   asserts replay consistency; the existing import test has a plan but no
   contract-emitting run.
7. **Large repository verification:** no test lowers/forces the 100k blob
   verification cap, corrupts beyond it, and asserts partial status/anchor
   behavior.
8. **Remote Git bounds:** existing Git tests assert `truncated` after the
   fact; none assert memory/stream bounds or a typed `OutputLimit` for a
   huge `git diff`/`git status`.
9. **Run pin replay:** no test clears `session_runs.runtime_snapshot_json` and
   asserts `runs_match=false`/repair restores it.
10. **Remote integration crash window:** no test kills the service between
    `enqueue_merge_with_source` and `record_settled_effect` and retries.
11. **Long-session revisions:** no test appends >2,000 non-plan events between
    revisions and asserts monotonic revision ids/history (overlaps 03 F2).

## Clean areas / things that held up under reading

- **Backup crypto and envelope handling** (`crates/vp-core/src/backup/crypto.rs`,
  `repository/blob.rs`, `repository/format.rs`): Argon2 bounds are validated
  before allocation, keys are domain-separated and zeroized, blob/manifest
  AAD binds repository/snapshot/chunk identity, final-chunk and trailing-data
  checks are present, and restore verifies plaintext hash/size/stored bytes
  before the swap. The 31 repository backup tests pass.
- **Backup path/chain validation** (`classify_archive_path`, manifest
  allowlist, sorted entries, linear parent chain, tombstone prefix, symlinked
  metadata/lock/blob/directory rejection, private scratch/modes): no traversal
  or downgrade path was found in the reviewed code. V1 ZIP restore validates
  the whole manifest/checksum set before touching the target and rechecks
  daemon absence before the swap.
- **Remote path jail and bundle import** (`crates/vp-remote/src/jail.rs`,
  `bundle.rs`, `crates/vp-core/src/remote_integration.rs`): relative-path and
  `..` rejection, canonical-root containment, symlink rejection for writes,
  atomic temp+rename, per-chunk and total SHA-256 checks, bundle head matching,
  private ref namespace, and exact fetched-commit revalidation are all present.
  The trust caveat (remote host/user is trusted infrastructure) is documented.
- **Run-journal duplicate-write gate**: I specifically checked the concern that
  a replayed write could be re-executed on a later attempt. `claim_completed_tool_result`
  consumes one row, but `finish_session_run` releases claims where
  `consumed_by_run_id = failing_run` (`crates/vp-store/src/store/runs.rs:463-473`)
  and `interrupt_running_session_runs` releases all consumed claims on restart
  (`runs.rs:687-699`). A failed/interrupted successor attempt therefore returns
  the claim to the lineage; a completed successor is terminal and cannot be
  resumed. I did not find a reachable path where a completed replay claim is
  re-executed without a new, explicitly authorized write call.
- **A2A facade** (`vp-a2a`): loopback-only literal IPv4/IPv6 validation on both
  listen and URL paths, one bearer token hashed and compared in constant time,
  duplicate `Authorization` headers rejected, one-time token handoff, card echo
  check, body/frame/message/artifact/SSE bounds, and card-only rejection with no
  local tool/file/shell/network/model authority. The A2A tests pass.
- **Progress ledger purity**: `progress_ledger.rs` is pure, enforces monotonic
  round ids and bounded lists/evidence, and always sets `advisory_only=true`;
  no scheduler, merge, budget, or task-controller consumer was found.
- **Event mirror restore**: `restore_session_log` requires contiguous sequence
  numbers from 1 and rejects cross-session events before replacing the table;
  `log_status` compares the full event tuple. Mirror append is best-effort
  (warning on failure), but that is documented and detectable.

## Assumptions

- Single daemon per data directory; the `run/daemon.lock`/discovery checks and
  the data-dir instance lock are the deployment boundary. Same-user
  filesystem races against repository/anchor/mirror paths are outside the
  claimed threat model (the ADRs state the host/user is trusted).
- Remote hosts/SSH users are trusted infrastructure; the helper is not an OS
  sandbox. This review treats remote memory exhaustion and the helper binary
  as trusted-host reliability issues, not containment escapes.
- Backup/restore is offline; live backup explicitly does not promise cross-file
  consistency, and restore refuses a daemon via discovery/held lock.
- A2A is the card-only facade described by ADR-0221; no inbound execution
  bridge exists at this baseline.
- No live provider or remote network calls were made. Tests run for this review
  were local fakes/fixtures only; the findings are from code paths and existing
  test coverage, not a live repro.

# vp-store / SQLite reliability review

- **Date:** 2026-11-19
- **Baseline:** `d239dca`
- **Scope:** `crates/vp-store/src/` — `store.rs`, `store/schema.rs`, every
  `store/*/` and `store/*.rs` module, `store/tests.rs`, `paths.rs`.
- **Method:** read-only static review against `AGENTS.md`,
  `docs/ARCHITECTURE.md`, `docs/AUDIT.md`, and the store/event/backup ADRs
  (0032, 0066, 0072, 0150, 0151, 0155, 0159, 0161, 0164, 0167, 0213–0215,
  0222, 0226). `cargo check -p vp-store` passed. `cargo test -p vp-store --lib`
  passed: **390 tests, 0 failed**. No source, test, config, or doc file was
  modified; this report is the only file created.

## Executive summary

The store layer is substantially stronger than a typical SQLite projection:
session events and their projections are committed together in most lifecycle
paths, the bundle importer validates nearly every nested collection, lease
tokens are redacted and fenced, and the 390 store tests include real
event+projection atomicity and concurrency checks.

The remaining risks cluster in four areas:

1. **Durable rebuild inputs are silently lossy instead of fail-closed.**
   `projects.jsonl` snapshots cap project-scoped event streams at the *oldest*
   100,000 rows, and `db-rebuild` treats those events as authoritative while
   replacing the projection. Several event mappers also turn malformed JSON
   payloads into `null` instead of failing. A large or partially corrupted
   project can therefore rebuild an *older* identity/coordination/workspace
   state without any warning.
2. **A few critical transitions rely on caller sequencing rather than a
   conditional update.** Session creation is two transactions, merge-queue and
   plan-task terminal states can be overwritten by a late writer, and the
   effect `unknown` override is keyed on a substring of free-form evidence.
3. **Identifier assumptions are inconsistent.** `validate_storage_id` allows
   `%`, `_`, and uppercase, but `session_id LIKE '<id>#%'` is used for
   destructive instruction-state replacement. SQLite `LIKE` is
   case-insensitive and treats `%`/`_` as wildcards, so a crafted or
   case-variant imported session id can delete another session's task-scoped
   instruction rows.
4. **Multi-process claims are internally inconsistent.** The lease module
   documents that two daemons may share one data directory and `recover_state`
   guards most tables by owner instance, but it deletes *all* write and GUI
   leases; conversely most read-modify-write transactions are `DEFERRED` while
   only the lease paths use `BEGIN IMMEDIATE`. The daemon's exclusive
   `daemon.lock` currently prevents this in production, which is why these are
   ranked below the data-durability findings.

No P0/critical defect was found in the reviewed code. Findings below are
ordered by expected risk; items 1–4 are worth fixing before calling the store
"production hardened".

## Findings

| # | Severity | Confidence | Finding | Key references |
|---|----------|------------|---------|----------------|
| 1 | High | High | Manifest/rebuild truncates project-scoped event streams to the oldest 100k rows and treats them as authoritative; partial manifest lines are silently skipped | `store/projects.rs:131-138,158-162,219-227`; `store/identity.rs:485-500`; `store/coordination/replay.rs:8-25`; `store/workspaces/replay.rs:8-25`; `store/identity_memory/replay.rs:16-30`; `store/employment/retirement.rs:650-664`; `crates/vp-core/src/engine/rebuild.rs:101-132`; `store.rs:1546-1571` |
| 2 | High | High | Session creation is not atomic with `session.created`; a crash/failure leaves an unrecoverable session row and orphan worktree | `store/sessions.rs:110-152`; `crates/vp-core/src/engine/sessions.rs:150-212`; `crates/vp-core/src/engine/rebuild.rs:40-41` |
| 3 | Medium | High | Corrupt orchestration policy silently decodes to the active default (`auto_retry_failed`, `auto_resolve_blockers`, `escalate_after_retries`) instead of failing closed | `store/scheduler.rs:108-120`; `crates/vp-protocol/src/types.rs:16063-16078`; contrast `store/projects.rs:332-351` |
| 4 | Medium | High | `session_id LIKE '<id>#%'` (case-insensitive, `%`/`_` wildcards) can delete/list another session's task-scoped instruction state | `store/backup/repair.rs:444-448`; `store/cache.rs:150-162`; `store.rs:1659-1667` |
| 5 | Medium | Medium | Merge-queue and plan-task terminal states are overwritten unconditionally; no store-level state machine or attempt guard | `store/merge.rs:211-231,319-358`; `store/plans/transitions.rs:133-162` |
| 6 | Medium | High | Startup recovery clears *all* write leases and *all* computer leases, including other live instances' rows, despite instance-scoped guards elsewhere | `store/backup/recover.rs:317,360`; contrast `recover.rs:36-49`; dead duplicate `store/plans/interrupt.rs:29` |
| 7 | Low–Medium | High | Bundle import accepts event `seq` values outside `1..=i64::MAX` and does not bound event payload size; wrapping can make a session unreplayable | `store.rs:1706-1708`; `store/backup/import.rs:1215-1233`; `store.rs:3923-3929` |
| 8 | Low | High | A genuine `unknown` effect can be settled if its free-form evidence contains the literal `stale_intent_sweep`, bypassing explicit reconciliation | `store/effects.rs:165-184,431-442` |
| 9 | Low | Medium | 196 `DEFERRED` read-modify-write transactions vs 59 `BEGIN IMMEDIATE`; a second process can cause `SQLITE_BUSY(_SNAPSHOT)` with no retry | `store/schema.rs:18-21`; e.g. `store/effects.rs:17,158`; `store/approvals.rs:547` |
| 10 | Low | High | Memory LIKE fallback does not escape `%`/`_`; recall over-matches (functional, not injection) | `store/memory/records.rs:173,178-183,337,344-346` |

Store paths are relative to `crates/vp-store/src/`; other crates are shown in
full (`crates/vp-core/...`, `crates/vp-protocol/...`, `crates/vp-daemon/...`).

## Detailed findings

### 1. Rebuild sources silently truncate and hide corruption — High / High

**Evidence**

- `record_project_manifest` builds one snapshot per project and passes bounded
  listers to it:
  - `store/projects.rs:131-138` → `list_agent_identity_events_for_project(..., Some(100_000))`
  - `store/projects.rs:158-162` → `list_employment_events(..., Some(100_000))`
  - `store/projects.rs:209-213` → intake/program events with explicit caps
- Those listers return the **oldest** rows:
  - `store/identity.rs:485-500`: `ORDER BY created_at ASC, identity_id ASC, seq ASC LIMIT ?2`
  - `store/employment/retirement.rs:650-664`: `ORDER BY seq ASC LIMIT ?2`
  - `store/coordination/replay.rs:8-25`, `store/workspaces/replay.rs:8-25`,
    `store/identity_memory/replay.rs:16-30`: `ORDER BY seq ASC LIMIT <=100_000`
- `db-rebuild` treats non-empty event streams as authoritative and
  **replaces** the projection with the replay of those events:
  - `crates/vp-core/src/engine/rebuild.rs:101-132`: if `agent_identity_events` is non-empty it
    replays and calls `replace_agent_identity_projection`. It never compares
    the snapshot rows against the replay or checks a count/watermark.
- The same project-scoped tables have no JSONL mirror (only session `events`
  have `sessions/<id>/events.jsonl`), so once the manifest truncates, there is
  no second durable copy.
- `projects.jsonl` is appended with one `write_all` of the whole buffer and
  `flush()` only (`store/projects.rs:219-227`); a crash can leave a partial
  line. `read_project_manifest` silently skips malformed lines and keeps the
  previous snapshot (`store.rs:1546-1571`), so rebuild uses stale state without
  reporting it.
- Several replay mappers convert malformed payloads to null instead of
  erroring, which hides corruption in the same path:
  `map_program_event` (`store.rs:428-438`), `map_coordination_event`
  (`store/coordination.rs:454`), `map_workspace_event`
  (`store/workspaces.rs:764`). `map_agent_identity_event`
  (`store.rs:2593-2606`) correctly fails; the inconsistency is the problem.

**Impact.** A project with more than 100k identity/employment/coordination/
workspace/memory events silently loses its *newest* lifecycle events from the
manifest. On the fresh/replacement database that `db-rebuild` is for, those
events are permanently gone, and `replace_agent_identity_projection` /
`replace_coordination_projections` rebuild from the truncated list, so the
result can resurrect a retired identity, restore an old
generation/title/parent, drop a newly created identity/workspace, or lose
coordination history while `vp db-rebuild` reports success. Because the
snapshot also carries the current projection, the result is a mixed state that
neither the manifest nor the events fully describe. There is no JSONL mirror
for these project-scoped streams to recover from.

**Repro (conceptual).** Create 100,001 `agent_identity_events` for one project
(e.g. repeated S6 updates), call `record_project_manifest`, then
`vp db-rebuild`; the manifest contains the oldest 100,000 events, and
`replace_agent_identity_projection` rebuilds from them, dropping the state
created by the newest event. A test can make the same point with a smaller
injectable limit if the constant is parameterized.

**Fix.**
- Do not cap manifest/rebuild reads; replay needs the full stream from the
  first event, so newest-first alone is not sufficient. If the cap must stay
  for memory bounds, refuse with a typed error or keep the snapshot projection
  and label the archive `truncated`.
- Record `event_count`, `last_seq`, and a `truncated` flag in
  `ProjectManifestSnapshot`; `rebuild_database` must refuse (or clearly warn
  and keep the snapshot projection) when the stored count exceeds the rows it
  can replay.
- Write the manifest line atomically (temp + `sync_all` + rename) and keep at
  least one previous snapshot; reject/skip with an explicit report instead of a
  silent warn.
- Make all project-event mappers fail on malformed JSON, or centralize the
  decode so correlation is impossible to miss.

**Test.** A store/rebuild test that seeds >cap events (with a lowered test cap)
and asserts either a typed truncation error or a complete rebuild; a partial
last-line manifest fixture asserting the rebuild refuses rather than silently
using the previous snapshot.

### 2. Session creation is not atomic with `session.created` — High / High

**Evidence**

- `store/sessions.rs:110-152` (`insert_session`) inserts the `sessions` row and
  commits; no event is written by the store.
- `crates/vp-core/src/engine/sessions.rs:150-212` creates the worktree, calls
  `create_session_with_worktree`, then makes a **separate**
  `append_event(&session.id, "session.created", ...)` call. If that call fails
  the `?` propagates and the session row plus worktree remain.
- `crates/vp-core/src/engine/rebuild.rs:40-41` skips any session directory whose mirror has no
  `session.created` event, and `db-rebuild` only reconstructs sessions from
  mirrors. The AGENTS invariant also documents that imported archives without
  `session.created` are skipped.

**Impact.** A crash (or DB error) between the insert and the event leaves a
session that is visible to `list_sessions`, may be queued/run, but cannot be
rebuilt from the event log. The new worktree is orphaned because the cleanup
branch only runs when the *insert* fails, not when the event append fails.
This is the one session-lifecycle path that is not event+projection atomic.

**Repro.** Add a fault-injection seam after `create_session_with_worktree`
returns and before `append_event` (or kill the process there); reopen the data
dir and run `vp db-rebuild --dry-run`: the session is not in
`RebuildReport.sessions`, and the worktree remains registered in Git.

**Fix.** Add a store primitive
`create_session_with_worktree_and_event(...)` that inserts the row, writes the
`session.created` event, and commits in one transaction. On the engine side,
wrap the worktree creation so an event failure removes the worktree (the
existing `worktree::remove` cleanup path).

**Test.** A store test asserting the row and event appear together, plus an
engine test that injects an append failure and asserts both the session row and
worktree are gone (or the session is at least marked `failed` with an event).

### 3. Corrupt orchestration policy fails open — Medium / High

**Evidence**

- `store/scheduler.rs:108-120`:
  ```rust
  Ok(stored
      .and_then(|raw| serde_json::from_str::<OrchestrationPolicy>(&raw).ok())
      .unwrap_or_default())
  ```
- `OrchestrationPolicy::default()` is active, not inert
  (`crates/vp-protocol/src/types.rs:16063-16078`):
  `auto_retry_failed: true`, `auto_resolve_blockers: true`,
  `escalate_after_retries: true`, `max_attempts: 2`.
- Other project policies fail closed on decode:
  `get_contract_policy` (`store/projects.rs:332-351`) and
  `get_effect_policy` (`store/projects.rs:370-389`) return an error.
- The policy is used by the scheduler/orchestration controller
  (`crates/vp-core/src/engine/scheduler.rs:176,468`) and by runtime pinning
  (`crates/vp-core/src/engine/runtime_pin.rs:213`).

**Impact.** A truncated/manually edited row, or a downgrade from a version that
wrote a newer enum/field value, silently *enables* automatic retries and
blocker resolution. The operator sees a successful policy read and may believe
the project still has the stricter behavior they configured. This is exactly
the "JSON decode fallbacks that hide corruption" class, in the one policy
where the default is not safe.

**Repro.** `UPDATE project_orchestration_policies SET policy='{'` (or set
`stall_recovery` to an unknown value), then call `orchestration.preview` /
the scheduler tick; no error is returned and default retry decisions are made.

**Fix.** Mirror `get_contract_policy`: return `anyhow!("parsing stored
orchestration policy")` on decode failure, normalize on read, and only treat a
*missing* row as `Default`. Consider the same for scaling/critical-path if
their defaults ever become active; today those defaults are disabled, so they
are fail-safe.

**Test.** Store test with a corrupt policy row asserting an error (not a
default), and a regression test that an unknown enum value is rejected while a
missing row still returns the documented default.

### 4. `LIKE` prefix matching crosses session boundaries — Medium / High

**Evidence**

- `store/backup/repair.rs:444-448` deletes instruction state with:
  ```sql
  DELETE FROM session_instruction_state
  WHERE session_id = ?1 OR session_id LIKE ?2   -- ?2 = "{session_id}#%"
  ```
- `store/cache.rs:150-162` reads with the same pattern.
- `store.rs:1659-1667` (`validate_storage_id`) rejects only empty,
  overlong, padded, `.`/`..`, `/`, `\`, and NUL. It **allows** `%`, `_`, and
  uppercase. Imported bundles only pass through this check
  (`store.rs:1700-1708`).
- SQLite `LIKE` is, by default, ASCII case-insensitive and treats `%`/`_` as
  wildcards. Verified with the system SQLite:
  `LIKE 'ABC#%'` matches `abc#task`, and `LIKE 'a%#%'` matches both
  `abc#task` and `aZZZ#task`.

**Impact.** An imported session whose id is the uppercase form of a local
session (or contains `%`/`_`) can, during `session.replay {repair:true}`, delete
the local session's task-scoped instruction state rows. The same pattern makes
`list_instruction_states_for_session` return another session's keys/hashes
(not text). This is a cross-session data-isolation violation and a silent
cache-state reset for the victim session. Triggering requires an operator to
import a crafted/case-variant bundle and then repair it, so likelihood is lower
than the impact.

**Repro.** Create session `abc` with an instruction row `abc#task-1`; import a
bundle with session id `ABC` (or `ab%`); run `session.replay --repair` on the
imported session. The `abc#task-1` row is deleted, and repair succeeds.

**Fix.** Do not use `LIKE` for a prefix: use
`substr(session_id, 1, length(?1) + 1) = ?1 || '#'` (SQLite `substr` is
byte/char based and case-sensitive), or escape `%`/`_` and add
`ESCAPE '\' COLLATE BINARY`. Tighten `validate_storage_id` to a safe alphabet
for imported ids (UUID/`[A-Za-z0-9_.:-]`) and reject spaces/uppercase-only
case collisions where a case-sensitive comparison is required.

**Test.** Two sessions whose ids differ only by case plus a `%` variant;
repair one and assert the other's `session_instruction_state` rows are intact.

### 5. Merge-queue and plan-task terminal states are not guarded — Medium / Medium

**Evidence**

- `store/merge.rs:211-231` (`begin_merge_queue_entry`):
  `UPDATE merge_queue SET status='merging' ... WHERE id = ?3` — no source-state
  predicate.
- `store/merge.rs:319-358` (`update_merge_queue_entry_with_event`):
  `UPDATE merge_queue SET status=?1 ... WHERE id = ?4` — no predicate.
- `store/plans/transitions.rs:133-162` (`finish_plan_task`):
  `UPDATE plan_tasks SET status=?1 ... WHERE session_id=?4 AND plan_seq=?5 AND task_id=?6`
  — no status/attempt predicate.
- By contrast, `retry_merge_queue_entry_with_event`
  (`store/merge.rs:360-378`) correctly requires
  `status IN ('conflict','failed','interrupted')`, and
  `skip_plan_task_with_event` checks the current status.

**Impact.** The Integrator is serialized by the runtime lease and the queue is
processed one entry at a time, so today this is mostly latent. But a duplicate
integration/finalize call (e.g. a retried RPC or a second resolver tick) can
move a `merged` entry back to `merging`/`failed`, or a late worker completion
can move a `completed` task back to `failed`. That invalidates the
`merged`/`completed` evidence used by `session.land`, merge retry, and
`session.replay`, and can allow a second merge attempt for the same entry.
`update_merge_queue_entry_with_event` is also the helper used by the engine's
integration path (`crates/vp-core/src/engine/merge.rs:1096`), so the store is the
last line of defense.

**Repro.** Enqueue an entry, mark it `merged`, then call
`begin_merge_queue_entry` (or `update_merge_queue_entry_with_event` with
`failed`) and observe the state regress with a new event. No error is returned.

**Fix.** Make transitions conditional on the allowed source states (and, for
plan tasks, on the current attempt) and return `None`/a typed conflict when no
row changes. Add a single `allowed_merge_transition` helper and use it from
both the event and non-event variants. Deprecate the non-event
`update_merge_queue_entry`/`enqueue_merge` variants or document them as
test-only.

**Test.** `merged -> merging` and `merged -> failed` are rejected; `completed
-> failed` on the same attempt is rejected (or a new attempt is required).

### 6. Recovery clears other instances' leases — Medium / High

**Evidence**

- `store/backup/recover.rs:317`: `tx.execute("DELETE FROM write_leases", [])?`
- `store/backup/recover.rs:360`: `tx.execute("DELETE FROM computer_leases", [])?`
  (and it then emits `computer.lease.expired` events for every deleted row,
  including another instance's live lease).
- The same function carefully guards sessions, agents, approvals, questions,
  merge queue, plan tasks, steering, and continuations with
  `NOT EXISTS (... runtime_leases l WHERE l.owner_instance <> ?1 ...)`
  (`recover.rs:36-49` and following). It also comments at 317-322 that only
  this process's runtime leases are cleared.
- `store/lease.rs:7-13` explicitly documents that "two daemons can share one
  data directory", and `recover_state` is written to respect that.
- `store/plans/interrupt.rs:6-32` contains the same global
  `DELETE FROM write_leases` (currently unused dead code, but a trap for future
  callers).

**Impact.** When a second `Store`/daemon instance runs `recover_state` while
another instance is alive, it clears the live instance's cross-session write
scopes, allowing overlapping writes from two sessions; it also deletes its GUI
lease and writes a false `computer.lease.expired` event into the other
instance's session log. The live instance's next `renew` fails; if it ignores
that, two helpers can drive the desktop concurrently. Today the daemon's
exclusive `daemon.lock` (`crates/vp-daemon/src/server.rs:57-73`) prevents two daemons
from starting normally, so this is **latent**, but the store's stated contract
and its own guards say otherwise.

**Repro.** Open two `Store` instances on one temp dir. A: create a running
session + `try_acquire_write_lease` and `acquire_computer_lease("gui", ...)`.
B: `recover_state()`. A's `write_leases` and `computer_leases` are empty and
A's session log contains a `computer.lease.expired` event for a lease that was
still live.

**Fix.** Delete write leases only for `agent_id`s belonging to sessions this
recovery is interrupting (or `session_id` in the recovered set), and delete
computer leases only for recovered sessions. For a shared-dir design, use the
same `NOT EXISTS (live other-instance runtime lease)` guard. Remove or fix the
global delete in `plans/interrupt.rs`.

**Test.** Extend the existing cross-instance continuation-claim test
(`store/tests.rs` around the second `Store::open` case) to assert another
instance's write lease and GUI lease survive recovery. This test is currently
missing.

### 7. Bundle import accepts out-of-range event sequences — Low–Medium / High

**Evidence**

- `store.rs:1700-1708` (`validate_session_bundle`) only checks that each
  event's `session_id` matches; it does not check `seq > 0`, uniqueness, or
  `seq <= i64::MAX`.
- `store/backup/import.rs:1215-1233` inserts `event.seq as i64`. A `seq` of
  `u64::MAX` becomes `-1`.
- `next_event_seq_tx` (`store.rs:3923-3929`) computes `MAX(seq)+1`; event mappers
  read `i64` and `as u64`. A wrapped row (`u64::MAX` stored as `-1`) reads back
  as a giant `u64`, and if `MAX(seq)` is `i64::MIN` the next sequence can also
  be negative; `log_status`/`restore_log` cannot round-trip that consistently.
- There is no payload-size cap for imported events; the 32 KiB cap applies to
  structured tool results (`MAX_TOOL_RESULT`/`store` constants), not to
  `bundle.events[i].payload`. A crafted archive can persist a multi-hundred-MB
  payload per event.

**Impact.** Operator-supplied archives are semi-trusted (import is an explicit
action), but a malformed/synthetic bundle can create a session that appears
imported yet can never be replayed/restored correctly, and can inflate the
database beyond intended bounds. This is a validation gap, not a live attack
path.

**Repro.** Bundle with `events: [{"session_id": "...", "seq": 18446744073709551615,
"type": "x", "payload": null, "created_at": "..."}]`; import succeeds and the
row's SQLite `seq` is `-1`. A subsequent event gets `seq = 0`.

**Fix.** Validate `seq >= 1 && seq <= i64::MAX` and reject duplicate seqs
(and, if the format requires it, non-contiguous seqs). Add a bounded per-event
payload size and a total bundle size. Mirror the same check for the
project-scoped event import paths (`agent_coordination_events`,
`agent_workspace_events`, `agent_memory_events`, `team_template_events`), which
currently only reject `seq == 0`.

**Test.** Bundle fixtures with `seq = 0`, `i64::MAX+1`, a duplicate seq, and an
oversized payload; each must return a typed error and leave the DB unchanged.

### 8. Effect `unknown` escape hatch is a substring match on evidence — Low / High

**Evidence**

- `store/effects.rs:165-184`:
  ```sql
  WHERE id = ?1
    AND (state = 'intent'
         OR (state = 'unknown'
             AND instr(evidence_json, 'stale_intent_sweep') > 0))
  ```
- The sweep writes the marker into `evidence.probe` and a free-form
  `evidence.reason`/`note` (`store/effects.rs:431-442`). Other probes populate
  `note` with remote paths and method names
  (`crates/vp-core/src/effect.rs:1189,1200,1241`), and error text can flow into
  `reason`/`note`.
- A genuine `unknown` (probe failed / transport ambiguous) is supposed to be
  terminal until `effect.reconcile`; this substring is the only thing
  distinguishing it from the sweep's synthetic unknown.

**Impact.** If a genuine unknown receipt's evidence happens to contain that
literal string (e.g. a remote path, branch, or probe message influenced by the
environment), a late `settle_effect` can overwrite it to `done`/`failed`
without the explicit reconciliation that is supposed to gate merge/landing.
The probability is low, but the gate is intentionally strong elsewhere and
should not be textual.

**Repro.** Insert a receipt in `unknown` with evidence
`{"probe":"git_push","note":"stale_intent_sweep"}`; call `settle_effect(done)`
and observe the row change with no `effect.reconcile`.

**Fix.** Add an explicit column/flag (e.g. `swept_at` or
`settle_source='stale_intent_sweep'`) and predicate on it, or use
`json_extract(evidence_json, '$.probe') = 'stale_intent_sweep'` (still better,
though a dedicated column is preferable). Same for any other sentinel checks.

**Test.** A receipt with the marker only in `note` cannot be settled; only a
row produced by `sweep_stale_effect_intents` can be replaced by a live
completion.

### 9. Most read-modify-write transactions are DEFERRED — Low / Medium

**Evidence**

- `store/schema.rs:18-21` sets WAL, FKs, `synchronous=NORMAL`, and a 10s
  `busy_timeout`.
- 196 call sites use `conn.transaction()` (DEFERRED); 59 use
  `TransactionBehavior::Immediate` (mostly leases/continuations). Examples:
  `store/effects.rs:17,158`, `store/approvals.rs:547`,
  `store/plans/transitions.rs:68`.
- There is no `SQLITE_BUSY`/`DatabaseBusy` retry/handling anywhere in
  `vp-store` or `vp-core` (grep returns nothing).
- `store/lease.rs` explicitly claims two daemons can share one data directory;
  `db-rebuild` takes the daemon lock, and backup opens read-only, but the lease
  wording and `recover_state` guards invite concurrent `Store`s.

**Impact.** In SQLite WAL, a DEFERRED transaction that reads and then writes
can fail with `SQLITE_BUSY`/`BUSY_SNAPSHOT` if another connection commits in
between; the busy timeout does not always retry that upgrade. A second
daemon/embedder or a future concurrent CLI path would surface sporadic
"database is locked" RPC failures. Within the current single-daemon
architecture this is mostly unreachable because of `daemon.lock`, hence Low;
the inconsistency is still worth removing before the store's multi-process
claims are relied on.

**Repro.** Two `Store` instances, barrier between the read and write of one
deferred path (e.g. `begin_effect`'s `latest_effect_tx` then insert), commit
from the second instance in between; the first fails with a busy error.

**Fix.** Route every read-modify-write through an `immediate_transaction()`
helper (or `TransactionBehavior::Immediate`), keep DEFERRED only for read-only
transactions, and add a bounded retry with jitter for `SQLITE_BUSY`. Align the
lease module's two-daemon claim with the daemon lock (or document the store as
single-process and remove the cross-instance guards).

**Test.** A two-connection contention test for one representative path
(`begin_effect`, `claim_plan_tasks_with_events`, or `create_approval_deduplicated`)
that asserts a typed retryable error rather than a generic failure.

### 10. Memory LIKE fallback does not escape wildcards — Low / High

**Evidence**

- `store/memory/records.rs:173,178-183` uses
  `let pattern = query.map(|q| format!("%{q}%"));` and
  `m.title LIKE ?2 OR m.body LIKE ?2 OR m.tags LIKE ?2` without `ESCAPE` for
  the non-FTS fallback.
- `store/memory/records.rs:337,344-346` (`memory_candidates_like`) has the same
  pattern; the `(?2 = '%%' OR ...)` special case treats an empty query as
  "match all".
- The task-candidate path already does it correctly:
  `escape_like(task_id)` + `LIKE ?3 ESCAPE '\'` (`records.rs:391-400`).

**Impact.** A query containing `%` or `_` (e.g. "50% coverage", "foo_bar")
matches unrelated rows, changing recall and memory-conflict/short-brief
eligibility. It is parameterized, so this is correctness/UX, not SQL
injection.

**Fix.** Use `escape_like` and `ESCAPE '\'` in both fallbacks, and replace the
`'%%'` sentinel with an explicit "no lexical filter" bind flag.

**Test.** Store test: memories with bodies "50% done" and "5000 done"; query
"50%" must return only the first.

## Additional observations (not ranked)

- **Artifact blob path is derived from an unvalidated caller hash.**
  `store/artifacts.rs:108-121` joins `format!("{sha256}.zst")` without a
  64-hex check, unlike `tool_output_blob_path` (`store/tool_outputs.rs:13-22`).
  All current callers compute the hash (`crates/vp-core/src/engine/artifacts.rs:283,359`), so this
  is defense-in-depth, not a live traversal.
- **The database file is not chmod 0600.** `Store::open` creates
  `verlinpark.db` inside a 0700 data dir but never calls
  `paths::ensure_private_file` on it (`store/schema.rs:16-22`), while snapshots
  (`store.rs:1637`) and discovery/lock files are 0600. The 0700 parent blocks
  other users today; symmetric permissions would be safer.
- **Legacy non-event mutators remain public.** `update_session_status`
  (`store/sessions.rs:445-452`), `set_team_plan` (`store/plans/snapshots.rs:4-11`),
  `enqueue_merge`/`update_merge_queue_entry` (`store/merge.rs:61-83,135-156`),
  `interrupt_running_sessions` (`store/backup/queue.rs:50-62`), and
  `interrupt_plan_tasks` (`store/plans/interrupt.rs:6-32`) update projections
  without an event. Current engine callers use the `_with_event` variants, but
  the non-event ones are easy to pick up by mistake and bypass the replay
  invariant.
- **Unbounded reads.** `list_all_events` (`store/events.rs:821-838`),
  `list_sessions` (`store/sessions.rs:408-431`), `list_agents`
  (`store/agents.rs:354-360`), and `list_usage_entries`
  (`store/usage.rs:315-345`) are unbounded. They are internal APIs, but a very
  long session or project can make an export/repair call allocate the entire
  log. Prefer the windowed helpers where the caller only needs a prefix.
- **`record_settled_effect` returns an existing `failed`/`intent` row
  unchanged** (`store/effects.rs:113-116`). Current callers use unique
  request hashes per commit/merge/attempt, so a successful retry after a
  failed attempt is recorded as a new receipt; if a future caller reuses a
  hash, the successful retry would be reported as the old failed state. It
  would be safer to create a new attempt for `failed` (as `begin_effect`
  does) and to reject an `intent` row with an explicit "settle, don't record"
  error.
- **`get_gate_policy` does not normalize on read** (`store/projects.rs:408-421`),
  unlike contract/effect/OTel. If `GatePolicy` gains clamping, a hand-edited
  row could bypass it. Low.
- **`ensure_column` / migrations.** `open()` applies DDL and `ALTER TABLE` adds
  outside one transaction, with no schema version and no pre-migration backup
  (`store/schema.rs:14-2931`, `store.rs:6243-6254`). The statements are
  individually idempotent, but a future migration that creates a UNIQUE index
  on legacy data can make every subsequent `open` fail with no recovery path.
  A `store_meta` schema version plus the documented "back up the old DB" step
  would close this.
- **Manifest cost.** `record_project_manifest` rebuilds the performance ledger
  and serializes up to 100k rows for every project on every call
  (`store/projects.rs:111-122,131-213`), while holding the manifest lock. On a
  large project this is a multi-second blocking operation for a routine policy
  save.
- **`list_plan_tasks` has no `(project_id)` index** (`store/plans/tasks.rs:239-255`);
  the only indexes are `(status, priority, enqueued_at)` and
  `(session_id, plan_seq)` (`store/schema.rs:2266-2269`). A project-wide
  `task.queue` scan is linear. Small today, but the task DAG is unbounded per
  project.
- **`tool_outputs` pagination** uses `created_at < ?` with no `(created_at, id)`
  tie-break (`store/tool_outputs.rs:224-244`); two rows with the same timestamp
  can be skipped by the cursor. Timestamps normally have sub-second precision,
  so this is a latent edge.

## Test gaps

The 390-test suite is strong on happy-path event+projection atomicity, lease
fencing, concurrent claim winners, and bundle round-trips. The gaps correspond
to the findings above:

1. No test that `recover_state` preserves another instance's `write_leases` or
   `computer_leases`; the cross-instance continuation claim is tested, which
   makes the omission more visible.
2. No test for a failure between session-row insertion and `session.created`
   (or for `db-rebuild` skipping the resulting session).
3. No test for corrupt orchestration-policy JSON; the contract/effect policy
   error paths are tested, so this asymmetry is untested.
4. No test for instruction-state isolation with `%`, `_`, or case-variant
   session ids.
5. No test that `merged`/`completed` are terminal in the store; only the
   retry-from-conflict path is covered.
6. No bundle fixture with `seq = 0`, negative/wrapped `seq`, duplicate `seq`,
   or an oversized event payload.
7. No test for effect settlement of a genuine `unknown` versus a
   sweep-produced unknown.
8. No two-connection contention test for a DEFERRED read-modify-write path
   (the existing `concurrent_*` tests either use one Store or the
   `BEGIN IMMEDIATE` lease paths).
9. No manifest truncation test (>cap events) and no partial-last-line
   `projects.jsonl` fixture for `db-rebuild`.
10. No test for memory LIKE wildcard behavior (`%`/`_`).
11. No test for the public non-event mutators being unusable or for the
    artifact/`sha256` path guard.

## Clean areas (verified)

- **Bundle validation is unusually thorough.** `validate_session_bundle`
  checks session/project ownership for messages, agents, artifacts, usage,
  test results, instructions, memories, conflicts, questions, tasks, gates,
  recoveries, runs, checkpoints, processes, identities, assignments, inbox,
  workspace bindings, templates, and programs; it also checks duplicate
  project-scoped event ids/seqs and recomputes template hashes
  (`store.rs:1700-2560`). The cross-session injection hypothesis I tested
  (messages/agents with a different `session_id`) is already blocked.
- **Event+projection atomicity** for approvals, plan-task claims, effects,
  gates, merge-queue `_with_event` updates, identity, memory, and workspaces:
  the event is inserted in the same transaction as the projection update, and
  the mirror append happens after commit (best-effort, logged).
- **Lease fencing and token hygiene** (`store/lease.rs`): persisted monotonic
  `runtime_lease_fences`, per-acquisition random tokens, conditional
  renew/release/assert, redacted `Debug`, tokens never exposed through the
  public `RuntimeLease` projection, startup audit rows, and real concurrent
  winner tests.
- **Continuation delivery fencing** (`store/continuation_lease.rs`): one-shot
  claim with `BEGIN IMMEDIATE`, token/owner/expiry predicate on ack/retry/fail,
  expired-claim reclaim, and a concurrent single-winner test.
- **Gate handling** (`store/gate.rs`): only a SHA-256 verifier is stored, a
  constant-time compare is used, resolution is one-shot and expiry-bound,
  duplicate same-outcome is idempotent, and different outcomes conflict.
- **Path/secret safety for tool outputs** (`store/tool_outputs.rs`): 64-hex
  hash addressing, symlink rejection, atomic 0600 temp+rename, size/hash
  re-verification on read, and no user path parameter.
- **Project/session scoping** in reads: memory, sources, search, metrics,
  workspace, and cache queries consistently bind `project_id`/`session_id`;
  dynamic SQL uses fixed call-site literals only; all user values are bound
  parameters (no SQL injection found).
- **Schema fundamentals:** WAL, `foreign_keys=ON` (verified that
  `PRAGMA foreign_keys='ON'` actually enables cascades), `synchronous=NORMAL`
  as documented, FTS5 external-content tables with insert/delete/update
  triggers and a one-time rebuild marker, and broad FK `ON DELETE CASCADE`
  coverage on session/project children.
- **Backup snapshotting** uses `VACUUM INTO` from a read-only connection and
  sets 0600 on the destination (`store.rs:1606-1640`).

## Assumptions and limits

- The normal deployment is one daemon per data directory (enforced by
  `daemon.lock`). Findings 6 and 9 depend on the store's explicit
  "two daemons may share one data directory" contract and on
  `recover_state`'s own instance guards; if that contract is withdrawn, those
  findings reduce to test/embedder hardening.
- Imported bundles and the local database are operator-controlled, not a
  remote attack surface. Findings 4 and 7 assume an operator can be induced to
  import a crafted archive; they are data-integrity/isolation issues, not
  remote code execution.
- `now_rfc3339` consistently emits UTC RFC3339 with optional sub-seconds; all
  in-tree timestamp comparisons use either parsed `OffsetDateTime` or
  `julianday()`, so I did not treat string ordering as a defect.
- I did not run live provider, network, or GUI tests. Store tests were run
  against the baseline and all 390 passed; `cargo check -p vp-store` was
  clean.
- Line numbers refer to baseline `d239dca`; later refactors may shift them.

## Suggested fix order

1. Findings 1 and 2 (durability/atomicity; both are small, high-value
   changes).
2. Findings 3 and 4 (fail-closed policy + identifier/`LIKE` isolation).
3. Findings 5 and 6 (state-machine predicates + instance-scoped recovery).
4. Findings 7–10 and the additional observations (validation hardening,
   intent settlement, transaction behavior, recall correctness).

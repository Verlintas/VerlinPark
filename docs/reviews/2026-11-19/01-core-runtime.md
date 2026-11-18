# Core Runtime Review — `vp-core` agent runtime and execution paths

- Date: 2026-11-19
- Baseline: `d239dca` ("Fix live provider compatibility and budget preflight")
- Scope: `crates/vp-core/src/agent.rs`, `agent/run_loop/`, `agent/service/`, `agent/service_run/`, `agent/dispatch.rs`, `agent/delegation.rs`, `agent/merge.rs`, `agent/plan/`, `agent/script.rs`, `agent/scout.rs`, `agent/provider.rs`, `agent/schemas.rs`, `agent/context/`, `agent/tools/`
- Method: read-only source review; `AGENTS.md`, `docs/ARCHITECTURE.md` (target invariants), `docs/AUDIT.md`, the `docs/DECISIONS.md` ADR index, and the C1–C12 / S1–S8 / hardening-round summaries were read first; targeted `cargo test -p vp-core` runs only (no live provider/network calls, no edits).

## 1. Executive summary

The runtime is materially stronger than a prototype: the gate stack (role policy → contract → approval broker → effect receipt → run journal → tool execution → settlement) is genuinely shared by the sequential dispatcher and Code Mode, the Verifier/Integrator path is structurally enforced, and the pause/preempt/cancel state machine is careful about lock ordering and terminal statuses. Budget reservation and settlement, admission/capacity fairness, lease fencing, and remote bundle hash verification all hold up under reading.

The remaining defects are concentrated in the *fast paths* and in *recovery boundaries*:

1. The parallel read-only tool batch is the one dispatch path that does **not** go through `Engine::evaluate_tool_permission`. It uses the stateless `permissions::decide()` helper, so project deny templates/approval rules are skipped and `intent.decision` is never persisted for allowed calls. This is the highest-severity finding: it breaks the documented "deny always wins" invariant for read-only tools.
2. The durable-journal duplicate-write gate **consumes** the completed row at check time but never writes a successor completed row for the replay. A resumed run that fails after replaying a write will re-execute that write on the next resume, which is exactly the duplicate side effect the journal exists to prevent (for `edit_file`/`apply_patch`/`file_manage`, not just idempotent `write_file`).
3. Batch preparation (plan dispatch and `delegate_many`) claims tasks and creates agents/worktrees/workspace leases incrementally with no compensating cleanup on a mid-loop error; a single failure leaks state until the next plan sync or lease expiry.
4. Several error paths degrade a durable state machine into a warning (`journal.finish_*`, merge-queue status writes, session status writes), and two terminal-outcome matches are not exhaustive. These are low-probability but cheap to harden.

No panic, unbounded loop, or approval/verifier/merge bypass was found in the non-parallel dispatch path. Tests overclaim in a few places only mildly (helper-level isolation tests presented as dispatcher guarantees); the real gap is missing adversarial tests for recovery boundaries, not dishonest assertions.

## 2. Findings table

| ID | Severity | Confidence | Location | Issue |
|----|----------|------------|----------|-------|
| R1 | High | Confirmed | `crates/vp-core/src/agent/run_loop/body.rs:1643-1691` | Parallel read-only tool batch bypasses the persisted approval broker (deny templates, grants, `approval.template.matched` audit) and `intent.decision`; `deny` can become `allow` |
| R2 | High | Confirmed | `agent/run_loop/tool_calls.rs:89-106`; `agent/dispatch.rs:524-539`, `640-655`; `crates/vp-store/src/store/runs.rs:951-1003` | Journal duplicate-write replay consumes the source row at check time and records no successor row; a later resume re-executes an already-completed write |
| R3 | Medium | Confirmed | `agent/plan/execute.rs:278-502`; `agent/delegation.rs:1110-1234` | Claim + partial batch preparation with no rollback: setup error leaves claimed tasks `running`, agent rows without `agent.finished`, unused worktrees, and held workspace leases |
| R4 | Medium | Probable | `agent/merge.rs:762-779`, `874-876`, `985-1001` | A non-git error after `begin_merge_queue_entry` leaves the queue row in `merging`/`checking` (or a merged branch with a non-merged row); only a restart repairs it |
| R5 | Low | Confirmed | `agent/run_loop/tool_calls.rs:961-986`; `agent/service_run/preemption.rs:111-143` | Answering or timing out `ask_user` rewrites a concurrently paused session status to `running` while the run control stays paused |
| R6 | Low | Confirmed | `agent/delegation.rs:1271-1309`, `1397-1399`; `agent/plan/execute.rs:809` | `LoopOutcome::Verdict` from a Worker is silently ignored; agent/task projections can be left `running` with no terminal event if role enforcement ever regresses |
| R7 | Low‑Medium | Probable | `agent/plan/worker.rs:44-65`, `340-361`; `agent/delegation.rs:864-876` | A no-change Worker marks a plan task `completed` (unblocking dependents) using a six-substring summary heuristic, with no Verifier or acceptance evidence |
| R8 | Low | Confirmed | `agent/run_loop/body.rs:433-491` | `warm_on_resume` makes a real provider call before any journal model intent; a crash after the warm re-pays for it on the next resume (opt-in only) |
| R9 | Low | Confirmed | `agent/run_loop/entry.rs:114-116`; `agent/service_run/start.rs:607` | Journal-finish and session-status errors are downgraded to warnings; a run row can stay `running` and a session status can stay stale |

## 3. Detailed findings

### R1 — Parallel read-only batches bypass the approval broker (deny becomes allow)

**Severity:** High · **Confidence:** Confirmed

**Evidence.** The parallel fast path in `run_loop_body` pre-screens each call with three stateless checks:

- `crates/vp-core/src/agent/run_loop/body.rs:1643` — `crate::permissions::assess_tool_call_with_gui(...)`
- `body.rs:1652-1669` — `crate::contract::authorize(...)` (and it falls back to the sequential path only on a non-Allow outcome)
- `body.rs:1670-1676` — `matches!(decide(config.approval_mode, &assessment, spec.auto_approve), Allow)`

It never calls `Engine::evaluate_tool_permission` (`crates/vp-core/src/engine/approvals.rs:67-128`), which is where project approval templates, grants, and the `approval.template.matched` audit live; it also never calls `Engine::record_intent_decision` (only `authorize_tool_call` does, at `agent/dispatch.rs:255-257`). The fast path is taken when an assistant turn has 2+ calls and every call is in `is_parallel_safe_tool` (`agent/dispatch.rs:1068-1090`), passes the role/skill checks, and resolves to a registry tool or `read_tool_output` (`body.rs:1620-1628`). The list includes `read_file`, `read_files`, `git`, `remote_read`, `remote_list`, `remote_git`, and `read_tool_output`.

`permissions::decide` (`crates/vp-core/src/permissions.rs:104-160`) is deliberately stateless: in Smart mode it allows every risk below `HostState|Destructive`. A project deny template can be created for read-only risk: `validate_risk_levels` only forbids high risk for `Allow` templates (`crates/vp-core/src/approvals.rs:918-945`, the check is guarded by `action == ApprovalTemplateAction::Allow`), and `template_risk_matches` accepts `read_only` (`approvals.rs:505-516`). `extract_input_paths` reads the `path` key (`approvals.rs:680-713`), so a path-scoped deny matches `read_file`.

**Impact.** With a deny template such as `tool_pattern = "read_file"`, `risk_levels = ["read_only"]`, `path_globs = ["secrets/**"]`:

- one `read_file` in a turn is denied by the broker;
- two `read_file` calls in the same assistant turn (both parallel-safe) are assessed `ReadOnly`, pass `decide`, and execute.

The documented invariant in `AGENTS.md` ("deny always wins") and in `docs/ARCHITECTURE.md` §1 item 6 ("every tool call, approval, command, and diff has an event record") is violated, and the C1 contract requires `intent.decision` events for authorization decisions. The exposure is read-only workspace content (the shell sandbox still allows reads for shell-capable profiles), but for read-only roles (Research Worker, Manager, Scout) the template is the only path-level guard, and the audit trail loses both the template match and the intent decision. This also silently bypasses a future read-only deny rule added for incident response.

**Reproduction / check.** In a test harness, create a project deny template for `read_file` over `secrets/**`; run a fake provider that emits two `read_file` tool calls in one assistant message; assert the tools execute (current behavior) vs. denied (expected). A single-call turn already returns `PERMISSION_DENIED`.

**Suggested fix.** Route each parallel candidate through `Engine::evaluate_tool_permission` and `record_intent_decision` exactly like the sequential path (fall back to sequential on `Ask`/`Deny` and on any contract escalation), or delete the fast path's private decision and reuse `authorize_tool_call` with a read-only preflight flag. Do not keep two policy stacks.

**Test to add.** `parallel_read_batch_honors_a_path_scoped_deny_template`: fake provider with two `read_file` calls; assert both `tool.finished` events are `PERMISSION_DENIED`, that `approval.template.matched` (action `deny`) is persisted, and that no file content reaches the assistant message. Also assert one `intent.decision` per call.

---

### R2 — Journal duplicate-write replay consumes evidence and can re-execute a completed write

**Severity:** High · **Confidence:** Confirmed

**Evidence.**

1. `agent/run_loop/tool_calls.rs:89-91` calls `journal.check_duplicate_write(...)` **before** approval, contract accounting, the effect gate, and any execution.
2. `check_duplicate_write` (`crates/vp-core/src/run_journal.rs:708-756`) calls `store.claim_completed_tool_result(...)`, which atomically sets `consumed_by_run_id = <new run>` (`crates/vp-store/src/store/runs.rs:951-1003`). This is consume-on-check.
3. On the duplicate branch, `prepare_tool_effect_gate` (`agent/dispatch.rs:524-539`) only probes the effect receipt and sets `effect_duplicate`; it never calls `journal.tool_intent`, so `journal_pair` stays `None`.
4. `finish_tool_call` therefore takes the `else` branch (`agent/dispatch.rs:640-655`) and only appends a `tool.finished` event. No new `run_tool_calls` row is created for the replayed call.

**Impact.** Scenario: run A completes an `edit_file`; run A later fails (provider error, budget, max turns) and is resumed as run B. Run B replays the write from the journal (consuming A's row), then fails again before finishing. Run C is resumed: `check_duplicate_write` finds no unconsumed completed row with that fingerprint, so it executes the write again.

- `write_file` with identical content is mostly idempotent, but it re-applies an `expected_sha256` check that may now fail.
- `edit_file` is not idempotent: `old_string` is gone, so the replay fails ("old string not found"), or, if the pattern still occurs elsewhere, it applies a second logical edit.
- `apply_patch` fails or double-applies; `file_manage` move/delete fails.

This defeats the stated purpose of the durable journal ("hard-blocks duplicate completed writes … consume-once"). A secondary effect: a duplicate call still runs `authorize_tool_call`, so strict mode may prompt the operator to approve a call that will not execute; if that approval is denied, the operator loses the journaled evidence output (the row is already consumed) and the next resume re-executes the write.

**Reproduction / check.** Use the existing fake-provider harness:

1. Turn 1: `edit_file` to replace a known block; turn 2: a tool error that fails the run → run A failed.
2. Resume A → run B replays `edit_file`; make the next provider call fail before the tool result turn completes (or cancel/fail after the replay).
3. Resume B → run C. Assert the file is untouched and the second resume does not execute the tool (current behavior: it executes).

**Suggested fix.** Move the consume to a successful settlement: create the completed `run_tool_calls` row for the replay first (with a `journal_replay` marker), and only then mark the source row consumed — ideally in one transaction with the `tool.finished` event (`tool_completed_with_effect`). Alternatively, `claim_completed_tool_result` should accept a "pending replay" state that a crash rolls back.

**Test to add.** `resume_twice_does_not_reexecute_a_completed_edit`: two consecutive resumes after a post-replay failure; assert the file content is unchanged, exactly one `tool.started` has `journal_replay: true` per attempt, and no real `edit_file` execution occurs after the first run.

---

### R3 — Batch preparation leaks claims, agents, worktrees, and leases on a setup error

**Severity:** Medium · **Confidence:** Confirmed

**Evidence.** `execute_team_plan` claims tasks atomically before creating any agent (`agent/plan/execute.rs:278-284`). It then iterates the batch and uses `?` for `ensure_plan_manager` (`:309-313`), `budget.reserve_slot` (`:323-325`), `create_agent` (`:327-336`), workspace attach (`:347-349`), `head_commit` (`:373-378`), and `create_task_worktree` (`:389-401`). Any error returns from the whole function; there is no rollback, no `plan_task.finished`, and no `agent.finished` for the already-created children. `run_parallel_workers` has the same structure (`agent/delegation.rs:1110-1234`).

**Impact.** On a setup failure (branch/worktree collision, manager cap, DB error, missing worktree):

- claimed tasks stay `running` in the projection until the next `sync_plan` (which reconstructs them as `interrupted` because `plan_task.started` has no finish event), so the current plan run reports an error that looks like a task failure;
- created agent rows have no terminal event and no `agent.finished`, so the team tree and usage attribution show "ghost" workers;
- `attach_worker_workspace_for_run` may have acquired a component lease; it is only released by expiry/heartbeat loss;
- task worktrees created for earlier tasks stay on disk (workspace retention does not sweep task worktrees).

The effect is recoverable but noisy and can hold a component busy long enough to force other sessions onto the legacy task-worktree fallback.

**Reproduction / check.** Plan with two tasks; force `create_task_worktree` to fail for the second (e.g., pre-create the expected branch/path, or make the worktree parent read-only in the test). Assert `plan_tasks.status = 'running'` for the first task, an agent row with status `idle` and no `agent.finished`, and a lease still held after the run fails.

**Suggested fix.** Split preparation into a fallible stage that records what it created and rolls it back on error (mark claimed tasks `failed`/`interrupted`, release slot guards/workspace bindings, remove newly created worktrees), or claim each task immediately before its worker is fully prepared and only once preparation succeeded. A drop guard around the preparation list is sufficient for slots and workspaces; task claims need an explicit `finish` in the error path.

**Test to add.** `partial_batch_preparation_failure_settles_claims_and_releases_workspaces`: inject a failure on the second task; assert both claimed tasks are terminal (`failed`/`interrupted` with an event), every created agent has an `agent.finished`, and the workspace lease is released.

---

### R4 — Merge-queue rows can stay `merging`/`checking` after a non-git failure

**Severity:** Medium · **Confidence:** Probable

**Evidence.** `process_merge_entry_fenced` calls `begin_merge_queue_entry` (`agent/merge.rs:762-764`), then `emit(integrator.started)` (`:765-779`). Both use `?`. After the merge it calls `update_merge_queue_entry(&entry.id, "checking", None)` (`:874-876`, `?`), runs the integration command, and finally writes `merged` (`:985-987`). Every `?` between those points returns to the caller with the row in `merging` (or, after a successful `git merge`, in `merging` while the branch already advanced). The only automated repair is daemon-startup `interrupt_merge_queue`, which rewrites `queued|merging|checking` to `interrupted` and `interrupted_merge_recoveries` resets clean worktrees (`crates/vp-store/src/store/merge.rs:406-417`, `488-520`). There is no in-process sweep.

**Impact.** Within a live daemon, a transient SQLite/event error (or a poisoned git mutex) can leave an entry non-terminal while the session branch has already merged the commit. `merge_queue.retry` refuses non-`conflict|failed|interrupted` rows, the UI shows a permanent "merging", and later queue processing may merge other entries on top of a commit the ledger does not know about. The branch state and the ledger diverge until restart.

**Reproduction / check.** Inject an `append_event` failure (store handle closed / test-only hook) immediately after `begin_merge_queue_entry`, or make the `checking` update fail after simulating a successful merge; assert the row status.

**Suggested fix.** Wrap the post-begin section in an error path that always transitions the row (`failed` with a bounded detail), and, if the git merge already happened, either completes the row or resets the branch to `actual_base` under the lease before failing. Treat `emit` failures during integration as non-fatal (log) rather than state-changing failures.

**Test to add.** `integration_event_failure_does_not_leave_the_queue_row_merging`: fault-inject the `integrator.started` append; assert the entry is `failed`/`interrupted` and the session branch is unchanged.

---

### R5 — `ask_user` answer/timeout overwrites a paused session status

**Severity:** Low · **Confidence:** Confirmed

**Evidence.** While a question is pending, the session status is `waiting_input` (`agent/run_loop/tool_calls.rs:943-948`). On answer the code writes `running` unconditionally (`tool_calls.rs:966`), and on timeout it does the same (`:983`). `AgentService::pause` sets `paused` on the same session and only flips the run control (`agent/service_run/preemption.rs:111-143`); `allowed_session_transition` allows `paused -> running` (`crates/vp-store/src/store.rs:2538-2548`). The run loop still observes `control.paused` at the next `wait_for_task_resume` boundary (`agent.rs:928-952`), so the control and the projection disagree.

**Impact.** A user can pause a session while a question is pending, answer the question, and the UI/scheduler now see `running` although the run is blocked and paused. `resume_plan` refuses a `running` session (`agent/service_run/plan_run.rs:30-35`), continuations skip `running` (`agent/service/continuations.rs:111-115`), and `session.pause` returns early without rewriting the status (`if control.is_paused() { return Ok(true) }`), so the mismatch persists until an explicit `session.resume`. No data corruption; it is an operator-facing state bug.

**Reproduction / check.** Fake provider calls `ask_user`; call `session.pause` while pending; answer; read `session.status`. Expected `paused`, current `running`.

**Suggested fix.** Restore the status through a helper that preserves `paused` (e.g., `if control.is_paused() { paused } else { running }`) in both the answered and timeout branches, or make `wait_for_question` observe pause and return without changing the session status.

**Test to add.** `answering_a_question_does_not_clear_a_concurrent_pause`.

---

### R6 — Non-exhaustive terminal outcomes can leave projections `running`

**Severity:** Low · **Confidence:** Confirmed (unreachable today because role policy blocks `submit_verdict` for Workers)

**Evidence.** `run_parallel_workers` handles `Ok(LoopOutcome::Verdict(_))` with an empty arm in the abort scan (`agent/delegation.rs:1289`) and again with no status update or `agent.finished` (`delegation.rs:1397-1399`). `execute_team_plan` does the same (`agent/plan/execute.rs:809`) after inserting `statuses[task] = "running"` and claiming the task; the task is then removed from `remaining`, so no `plan_task.finished` or `record_scheduled_task_finish` ever happens.

**Impact.** Today a Worker cannot reach the `submit_verdict` branch (`tool_calls.rs:114` requires `AgentRole::Verifier`), so this is defense-in-depth. If the role check regresses, or a future role gains verdict capability, a plan task and its agent stay `running` forever and the batch's dependents are blocked with no terminal evidence.

**Suggested fix.** Make the outcome matches exhaustive with a catch-all that records `failed`/`interrupted` and emits `agent.finished`/`plan_task.finished`; a compiler-enforced exhaustive match is preferable to `_ => {}`.

**Test to add.** Unit-test the outcome handlers with a synthetic `LoopOutcome::Verdict` (or a helper seam) and assert a terminal status plus one finish event.

---

### R7 — No-change Worker completion is a heuristic and bypasses verification

**Severity:** Low‑Medium · **Confidence:** Probable

**Evidence.** When `commit_worker` returns `None`, `finish_worker` marks the Worker `idle` and returns `success: !worker_report_reports_failure(summary)` (`agent/plan/worker.rs:44-65`). The heuristic is six lowercase substrings (`agent/delegation.rs:864-876`: "blocked", "permission denied", "no write tool", "cannot proceed", "could not complete", "unable to complete"). The Verifier never runs; acceptance criteria are not checked.

**Impact.** A plan task can be marked `completed` and unblock dependents when the Worker produced no commit and no verified evidence, as long as its summary avoids those phrases (e.g., "I implemented the feature but nothing needed changing"). This is a semantic judgment, not a mechanically checkable claim, and the task memory (`record_task_memory` for `completed`) then records a result with no commit.

**Suggested fix.** Require an explicit machine-readable no-op result (`{"result":"no_changes","reason":...}`) and route it through a Verifier or a deterministic acceptance check, or mark it `completed_no_change` and let the scheduler decide whether dependents may start. At minimum, record `verified:false, merged:false` in the task memory and the plan event (the event does, but the projection still says `completed`).

**Test to add.** `no_change_worker_completion_requires_an_explicit_no_op_or_verdict`: Worker returns `Final("Implemented the feature.")` with a clean worktree; assert the current `completed` behavior and the absence of verifier evidence, then pin the new contract.

---

### R8 — `warm_on_resume` provider call is outside the run journal

**Severity:** Low · **Confidence:** Confirmed

**Evidence.** `run_loop_body` calls `warm_prompt_cache` before the turn loop (`agent/run_loop/body.rs:433-491`) when the project `CachePolicy.warm_on_resume` is enabled. The run journal's first model intent is created inside the loop (`body.rs:1141`). The warm call records usage/cost (`agent/context/bundles.rs:185` and the warm tests), but it is not a journal step.

**Impact.** A crash or preemption after a paid warm and before the first checkpoint repeats the warm call on the next resume. `warm_on_resume` is opt-in and off by default, so the cost is bounded and explicit, but the "exactly one provider call per turn" and journal-resume invariants do not cover it.

**Suggested fix.** Record an explicit `cache_warm` journal step (or skip the warm when the previous attempt for the same epoch already warmed) so resume can decide to reuse or skip.

**Test to add.** `resume_after_a_successful_warm_does_not_warm_again_for_the_same_epoch`.

---

### R9 — Journal/status write failures are downgraded to warnings

**Severity:** Low · **Confidence:** Confirmed

**Evidence.** `run_loop` logs `journal.finish_*` errors instead of propagating them (`agent/run_loop/entry.rs:114-116`). `spawn_run` ignores the initial `update_session_status` result (`agent/service_run/start.rs:607`), and failure paths ignore the `failed` update (`start.rs:142`). A failed finish leaves the `session_runs` row `running`; startup recovery only repairs it on the next daemon restart (`interrupt_running_runs`).

**Impact.** Within the same daemon process, a persistent DB error can leave a run row `running` while no control exists. `cancel_session_run` finds it and reports "cooperative cancellation requested", but there is nothing to cancel; resume preflight may refuse; operators see a stuck run until restart. No data loss.

**Suggested fix.** Keep the warning, but add a bounded daemon sweep that marks `running` journal rows with no live control/lease as `interrupted` (same pattern as stale effect sweeps), and treat the initial status write as a hard error before spawning the run.

**Test to add.** `a_run_journal_without_a_live_control_is_repaired_by_the_sweep`.

## 4. Test / reliability gaps

- **Recovery-boundary tests are the largest gap.** The suite covers happy-path resume (`resumes_a_crashed_run_and_hard_blocks_a_completed_write`), but not *two* consecutive resumes after a post-replay failure (R2), partial batch setup failure (R3), or integration status-write failure (R4). These are the states where a crash is most likely to occur.
- **Policy-rule coverage for the parallel fast path is absent.** Existing approval-template tests exercise the broker; none drive a multi-call parallel batch through a deny template, a strict-mode write assessment, or a project grant (R1).
- **Parallel-batch tests test helpers, not the dispatcher.** `parallel_tool_deadline_aborts_only_the_slow_tool_and_drops_its_work` and `parallel_tool_run_preemption_still_aborts_every_tool` exercise `run_tool_with_token_deadline` with fake tools; they do not prove the dispatcher's approval/contract/journal behavior in the batch path. That is acceptable as unit coverage, but the names/placement suggest a stronger guarantee than is tested.
- **No adversarial tests** for: duplicate tool-call ids in one assistant turn, empty tool-call ids across a resume, an `ask_user` answer racing a pause (R5), a no-change Worker with a misleading summary (R7), or a plan task whose outcome is `Verdict` (R6).
- **No test asserts that a denied/skill-blocked sequential call emits paired `started`/`finished`.** The denial branch emits only `tool.finished`; if any consumer assumes pairing, the stream is inconsistent. (Low, not listed as a separate finding.)
- **No test asserts that every terminal run path leaves exactly one journal terminal state**; R9 is observable only via a fault-injection hook that does not exist yet.
- **No fault-injection harness** for SQLite/event append failures inside the integrator; R4 was found by reading, not by a failing test.

## 5. Areas reviewed with no issue found

- **Admission and capacity fairness** (`agent.rs:283-926`): the waiter/cancel race in `FairCapacity::acquire` and `AdmissionGate::acquire` is safe under the state mutex (the cancel branch observes the waiter still queued, or the grant observes the receiver alive); priority aging, session/project round-robin, and permit release on drop are correct.
- **Budget reservation and settlement** (`agent.rs:1677-2101`): token/cost reservations are pre-flight, settled with provider usage (or estimates), and released on drop; `unlimited` is checked at settlement; governor deferrals are typed and do not block tasks permanently.
- **Pause/resume/finalize ordering** (`agent.rs:702-745`, `service_run/preemption.rs:111-171`): the `finalized` mutex prevents a terminal status from being overwritten by a concurrent pause/resume; startup `recover_state` marks `paused`/`waiting_input` (and orphaned `running`) sessions `failed` when no other live owner lease exists (`vp-store/src/store/backup/recover.rs:6-65`).
- **Preemption and tool deadlines** (`preemption.rs`, `provider.rs:149-360`, `dispatch.rs:1039-1090`): retries happen only before any output/usage, partial output is preserved and never replayed, tool deadlines are child tokens that do not preempt the run, and provider grace drains usage without accepting output.
- **Verifier/Integrator gate** (`agent/merge.rs:614-1002`, `plan/worker.rs:86-298`): the local path only enqueues after an approving Verifier; the merge is by exact commit under an integration lease with fence checks before/after the merge, `git diff --check`, optional integration command, and exact rollback on check failure; startup repair handles interrupted merges. Remote integration can explicitly opt out of the Verifier via `RemoteIntegrationPolicy.require_verifier=false`, which is a documented policy decision.
- **Code Mode** (`agent/script.rs`): inner calls re-enter the full gate stack (contract, approval, effect, journal, skill), writes are separately authorized, cancellation is observed, and the runner has bounded heap/stack/CPU/call/write limits.
- **Scout** (`agent/scout.rs`): role policy excludes write/shell/delegation, capacity and local cap apply, and the post-run `status --porcelain --untracked-files=all` guard detects tracked and untracked session-worktree changes.
- **Remote bundle integration** (`agent/merge.rs:71-276`): chunk hashes, total hash, size cap, head match, helper deletion on success/failure, and a clean local temp file are all enforced.
- **Run/resume admission serialization** (`service_run/start.rs`, `plan_run.rs`, `operations.rs`): the in-memory `runs` map plus the SQLite session lease prevents two runs in the daemon; operation receipts make prompt/resume idempotent; continuation delivery claims are one-winner with fail-closed recovery.
- **Tool schema/bundle consistency** (`agent/context/bundles.rs`, `agent/dispatch.rs:760-890`): dynamic MCP tools are pinned, sorted, capped, and frozen; unpinned tools are neither advertised nor executable; MCP role checks are enforced in `run_mcp_tool`.

## 6. Explicit non-findings / assumptions

- **R1 is only exploitable with an operator-created deny template that includes `read_only` risk.** Default templates target `external`/`workspace_write`, so default installs are unaffected. It is reported as High because it violates a stated invariant and an incident-response control, not because default behavior is unsafe.
- **Cooperative cancellation does not interrupt a verified merge.** `finish_worker` may complete verification and enqueue/merge after an operator cancels; the merged work is committed and verifier-approved, and cancellation is documented as turn/tool-boundary cooperative. Not treated as a defect.
- **`auto_approve` internal runs skip the global Worker capacity gate** (`body.rs:46-50`), but all such specs are `Manager` role (remote integrator, resolver, retry) where the Worker/Labour lane does not apply; ACP Workers acquire capacity explicitly (`delegation.rs:570`). Not a bypass.
- **`run_script` is not an OS sandbox** and `shell` analysis is syntactic; both are documented limits. No claim of full confinement is inferred.
- **Landed/imported terminal transitions** are enforced in `allowed_session_transition`; the review did not re-audit every recovery writer.
- **`warm_on_resume` and `prepared summaries` are opt-in, bounded, and off by default**; R8 is a cost-duplication edge, not a correctness break.
- **No live provider calls were made.** `cargo test -p vp-core --lib` was run only for three targeted tests (`pauses_and_resumes_between_turns`, `cancels_a_slow_provider_stream`, `tool_deadline_stops_only_the_tool_and_the_run_continues`), all passing; the full workspace suite was not re-run.

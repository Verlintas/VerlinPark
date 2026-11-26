# VerlinPark V1 readiness ledger

- Status: Active
- Date: 2026-11-27
- Owner: Verlintas
- Scope: single-project coding V1
- Plan: [V1_FINAL_PLAN.md](V1_FINAL_PLAN.md)

This ledger is the current defect and evidence index for V1. Every new finding
from real use, review, or a failed run enters here before it is fixed or
explicitly classified as a non-goal. A closed row needs a regression test or a
reproducible check; live evidence is labelled with the exact run, not with a
claim that a test did not make.

The carried must-fix list in [V1_FINAL_PLAN.md](V1_FINAL_PLAN.md) §6 remains
authoritative for items discovered before the live round. Those rows are not
silently re-marked here without a new audit; they are tracked as `OPEN (carried)`
until their own evidence closes them.

## Live round evidence (2026-11-23)

Real provider: DeepSeek `deepseek-flash` at `https://api.deepseek.com` via an
environment-only key; no key was written to the repository, events, logs, UI,
or this document. Data directory was a temporary test directory, not the
owner's live daemon data.

| Run | Goal | Budget | Input | Output | Cache read | Hit ratio | Result |
|---|---|---|---:|---:|---:|---:|---|
| A | 3 independent tasks (calc, CLI, README) | 2,000,000 | 1,926,836 | 70,848 | 1,753,088 | 90.98% | Failed honestly at the budget boundary before verification; no merged work |
| B | 1 task (multiply/divide + tests) | 1,500,000 | 531,916 | 11,393 | 471,552 | 88.65% | Completed: Worker → Labour → Verifier → merge queue → Integrator, merge `c7244f7`, 4 tests passed |
| C | 1 task (power/modulo + tests) | 1,500,000 | 1,456,885 | 39,496 | not recorded | — | Worker commit `2d47d49` verified; integration blocked by an unprobed raw-shell receipt; budget exhausted during Leader recovery |

Provider prices were not configured, so `cost_usd` remained `0.0`; no USD
savings claim is made. Cache-read tokens are a subset of input and are still
billed. Run A recorded 3 append-only breaks; run B recorded 0.

GUI evidence:

- The Collab node inspector opened for an ephemeral Manager and rendered the
  Overview, Context, and Messages views; the Conversation and Activity tabs
  were present but their content was not individually captured in this round.
- The Messages tab showed the truthful `AGENT_NOT_ACTIVE` state for an idle
  ephemeral Manager, with the composer disabled and no optimistic reply.
- A real running Labour received operator steering (`pending` → `delivered`)
  and the Labour committed the work.
- The team graph showed Leader, Manager, Worker, Verifier, and Labour activity
  while a real run progressed.
- The app reconnect test launched the app with stale local defaults
  (`http://127.0.0.1:11434/v1`, `qwen2.5-coder`) while the daemon was
  configured with DeepSeek; after reconnect `provider-test` still returned
  `deepseek-flash`.

Verification gaps:

- The GUI composer was not used to send to a live Worker during this round; the
  running-Agent steering path was exercised through the same
  `agent.message.send` route from the CLI. A manual GUI running-Worker send is
  still required by acceptance scenario 8.
- An idle **identity** inbox send was not reproduced in the GUI in this round;
  the existing engine/Swift tests cover it, but the live GUI evidence is for
  the ephemeral `AGENT_NOT_ACTIVE` path.

## New findings

| ID | Severity | Finding | Status | Evidence / test |
|---|---|---|---|---|
| V1-D01 | S1 | Steering trusted an Agent row's `running` status and queued a row even after the durable provider run had completed (`session.steer` as well as `agent.message.send`). The message stayed `pending` forever because no future provider boundary could consume it. | **CLOSED** | Shared `steer_session_inner` now uses `Store::has_live_agent_run`; the direct-message route additionally routes a completed-run identity Agent to inbox and an ephemeral Agent to typed `AGENT_NOT_ACTIVE`. Engine tests cover live-run steering, completed-run ephemeral/identity, and the daemon RPC test covers the typed error and no extra pending row. |
| V1-D02 | S0 | On reconnect, the macOS client could overwrite a daemon configured outside the app with stale local provider defaults (including a keyless localhost endpoint). | **CLOSED** | `ProviderSyncRules`; four Swift regression tests; live DeepSeek daemon check after launching with localhost defaults; ADR-0233 |
| V1-D03 | S1 | A raw-shell git commit produced an `EFFECT_UNKNOWN` receipt that blocked integration. The agent has no reconciliation tool, so the Leader retried with a new manual run and burned the remaining budget; the operator had to reconcile through the CLI. This is fail-closed C2 behavior, but the recovery path is expensive and not guided. | **MITIGATED** | P0 now marks the task `blocked` with an operator reconciliation reason instead of a normal `failed` retry (`integration_blocked_by_effect_unknown` + test). The operator reconcile path is still explicit; R2 pause/attention is still open (V1-D05). |
| V1-D04 | S2 | A plan Worker row can remain `running` after its provider run completes while the task is committed, verified, and merged. Messaging is now safe, but status surfaces still show the Worker as running. | **OPEN** | Run A: Worker run completed at 04:34:45, row still `running` at 04:40; ADR-0232 hardening |
| V1-D05 | S2 | Session budget exhaustion during post-failure recovery has no automatic pause/escalation for `EFFECT_UNKNOWN` or repeated integration blocks. Run C failed at the hard budget with no merged result. | **OPEN** | Run C totals above; the task error names the reconciliation requirement, but the Leader's recovery still consumed the allowance |
| V1-D06 | S2 | The GUI direct-message path to a **running Worker** and the idle **identity** inbox path were not both captured live in the same round. | **OPEN (verification)** | CLI route and Swift/engine tests pass; acceptance scenario 8 still needs the manual GUI pass |
| V1-D07 | S2 | A plan Worker's first-class run can be committing/verifying while the agent row is shown as `running`; the UI has no distinct "verifying/merging" state. | **OPEN** | Overlaps V1-D04; the team graph showed a Worker "RUNNING" with no provider activity |
| V1-D08 | S1 | `read_files` rendered a 12-character sha256 prefix while `edit_file`/`write_file` required the full hash; the resulting `HASH_MISMATCH` looked like a concurrent edit and caused repeated re-read loops. | **CLOSED** | `read_files` now prints the full hash; write/edit errors include `actual_sha256`, a truncation-specific message, and an actionable hint; tool tests cover both paths |
| V1-D09 | S1 | A bare Python `unittest` project without pyproject/pytest metadata was reported as `PROJECT_UNSUPPORTED`, pushing Agent runs into shell guessing. | **CLOSED** | `project_profile` detects `tests/test_*.py` and proposes `python3 -m unittest discover -s tests -v`; regression test covers the bare fixture |
| V1-D10 | S1 | Contract analysis treated scratch verification writes and absolute read-only git commands as host-state/scope violations, creating repeated approvals and `.scratch-proof` artifacts in the worktree. | **CLOSED (P0)** | Shell exports `$VERLINPARK_WORKTREE`/`$VERLINPARK_SCRATCH`; prompts/recipes say cwd is the worktree; contract allows the stable scratch/TMPDIR prefixes; absolute read-only git (`status/diff/log/show/rev-parse/...`) is ReadOnly; `clean`/`reset --hard` remains Destructive. Tests cover all paths. |
| V1-D11 | S1 | Fixed token budgets charged cache-read input at full weight, ignored missing prices, starved verification, and failed solvable runs; the dashboard could not show the difference between cheap-but-stuck and expensive-near-completion. | **PARTIAL (R2a)** | R1 exposes raw vs weighted tokens, money with `pricing_source`, time, attention, risk, context, capacity, quality, and progress plus readiness/forecast. R2a adds operator top-up (clamped to caps) and pre-dispatch completion-reserve deferral. Raw token caps remain authoritative; cost-weighted settlement/automatic reallocation is R2b/R3. |
| V1-D12 | S2 | Operator attention had no budget or governor: repeated host-state approvals could consume the run without triggering a policy decision. | **PARTIAL (R2b)** | Pre-dispatch deferral and a true turn-boundary pause (`session.resource.paused`) now stop new work at the attention cap; a bare resume re-pauses until the cap/policy changes. The pause carries a bounded attention batch (risk groups + at most 8 approval metadata rows, no raw input) and the Stage banner opens Inbox or Policies. The existing approval queue, coalescing, and batch resolve remain the only decision path; no automatic grant/template/profile. Explicit partial delivery is event-only and available; rescue reserve, opt-in R3b runtime task-envelope reallocation, and explicit operator-confirmed persistence of one recorded overlay, and opt-in pre-run auto-persistence are implemented; automatic reallocation with reservation semantics remains open. |

## Run Contract R1/R2a/R2b (2026-11-24)

Implemented the observe/operator slice from ADR-0234:

- `session.resources` returns a bounded multi-resource snapshot (money, raw and
  cache-weighted tokens, time, attention, risk, context, capacity, quality,
  progress), forecast, bottleneck, warnings, and nested readiness.
- `session.readiness` returns the conservative `none/dirty/committed/tests_passed/
  verified/merged/partial/blocked` level, shippable/mergeable flags, latest
  commit/verifier/merge, bounded changed paths, remaining work, and blockers.
- `session.run_contract.get` returns the derived profile, hard/soft targets,
  completion reserve, attention cap, acceptance, stop policy, and warnings.
- `vp resources`, `vp readiness`, and `vp run-contract` use the same RPCs; the
  macOS Stage has a resource strip/card backed only by `session.resources`.
- R2a adds `session.run_contract.set` (event-backed profile/reserve/attention/
  stop-policy override), `session.resources.top_up` (fixed/auto budgets clamped
  to existing caps; optional resume), CLI `run-contract-set` / `top-up`, and a
  Stage profile menu. A pre-dispatch governor defers new plan work when
  readiness is blocked, the attention cap is reached, or the completion
  reserve is insufficient, and records `session.resource.decision`.
- R2a itself does not pause an active run, rebalance/rescue, approve, reconcile,
  merge, or route. It does not remove caps or select unlimited. Missing prices
  are labeled `unconfigured`; readiness does not infer tests from raw shell
  output.
- R2b core adds a true provider-turn boundary pause (`session.resource.paused`)
  for Leader/Manager/Worker/Labour/Scout when the governor blocks; the same run
  waits for operator resume/top-up and re-pauses if the resource cap is
  unchanged. Verifier and trusted internal repair runs are exempt so
  already-produced work can still be verified/integrated. `readiness_blocked`
  remains a new-work dispatch gate, not a turn pause, to avoid deadlocking
  approvals/effects that the active run must finish. The turn path is lean: it
  does not run readiness/`git status`, task queue, merge queue, or the agent
  roster, and dispatch/turn share one action ladder.
- Auto Budget request-fit now runs for every role: tokens for all roles, plus
  cost/time for child runs using the recent per-call forecast and a minimum
  time runway. Only the top-level Leader advances cumulative turns/no-progress;
  child runtimes never overwrite Leader-owned progress state and pause instead
  of crossing an unextendable hard cap.
- macOS adds a Top Up sheet, capped/resumed truth, readiness caveats, bounded
  report copy, a project RunPolicy editor, and an event-derived resource-pause
  banner with Resume/Top Up/Approvals/Policy actions. The banner also shows the
  bounded attention batch from the pause details. It never removes a cap,
  infers a pause from a bottleneck, or claims a resume/merge the daemon did not
  report.
- The turn pause carries a bounded `attention_batch` with pending/interrupts/
  cap, risk groups, and up to 8 approval metadata rows (tool/risk/task/
  priority/waiters/reason). Raw tool input, command text, and file contents are
  excluded; approvals still resolve only through the existing queue/batch.
- Explicit partial delivery is an event-only `session.partial_delivery` record
  with bounded summary, remaining work, readiness level, commit, uncommitted
  paths, and blockers. `session.delivery` / `session.delivery.partial`, CLI
  `delivery` / `partial-delivery`, and the Stage record row/sheet surface it.
  `--yes` / `confirm=true` is required; running/queued/imported/empty/
  already-complete sessions are refused with typed `DELIVERY_*` codes. It never
  commits, merges, approves, completes, resumes, changes budgets/capacity, or
  bypasses the Verifier/Integrator; export/import preserves the record and a
  newer record wins. It is advisory evidence and can become stale as newer work
  lands.
- Rescue reserve + allocation preview: `session.rescue.status` returns
  daemon-validated eligibility, reserve headroom, near-completion/looping
  candidates, and bounded recommendations. `session.rescue` records a
  30-minute event-only grant for fixed-budget, clean near-completion work with
  zero no-progress turns; running/queued/imported/dirty/blocked/looping/auto/
  unlimited/no-headroom sessions are refused with typed `RESCUE_*` codes. The
  next non-pausing turn boundary releases only the completion-reserve ratio and
  appends one `session.resource.rescue.used` receipt; hard caps, attention,
  context, approvals, Verifier, and Integrator remain authoritative. CLI and
  the Stage confirmation sheet surface it. Reallocation recommendations are
  advisory only; R3a adds per-task envelopes and R3b adds opt-in runtime
  reallocation, while automatic persisted plan-budget mutation remains open.
- Readiness snapshot cache: `session_resources` reuses readiness for 2 seconds,
  keyed by the newest session event sequence, bounded to 64 sessions. Any new
  event invalidates it. `session.readiness`, partial-delivery/rescue actions,
  and all state-changing paths remain uncached; the lean turn governor never
  reads this cache.
- Project-level RunPolicy adds persisted defaults (profile/reserve/attention/
  stop), manifest recording and `db-rebuild` restore, RPC
  `project.run_policy.get/set`, and CLI `project-run-policy` /
  `project-run-policy-set`. Session overrides win field-by-field; source is
  `session_override` / `project_policy` / `derived`. It cannot widen any safety
  or capacity gate. Stop-policy fields are bounded-validated, and the CLI human
  renderer no longer mistakes a RunPolicy for an effect policy.
- GUI smoke found and fixed a first-selection loading bug: `loadSessions`
  assigned the first session id directly instead of using the normal selection
  path, so budget/resources/plan/diff/checkpoint/lineage/mailbox state never
  loaded for the auto-selected session and the Stage resource strip stayed
  empty. The resource failure state is now visible with an explicit Retry
  instead of silently hiding the strip.
- The live GUI smoke (temp daemon, no provider) keeps the resident launch,
  shows the resource card when the project/session is opened, and shows the
  Manage Dash Project RunPolicy card/editor with the daemon-reported policy;
  the pause banner itself was not triggered by a live pause in this smoke and
  remains covered by Swift event tests plus core pause tests. A second smoke
  injected real user/agent transcript events and verified the default display
  (user plain, agent Markdown) plus the Settings → General → Transcript switch
  to user Markdown / agent plain text applying immediately.

Tests: R2b targeted: 27 auto-budget runtime tests, 368 `agent::tests`, 14
resource tests, 2 project-policy/rebuild tests, 2 protocol stop-policy tests,
434 store tests, 54 daemon tests, 194 CLI tests, 35 targeted Swift
`ResourceGovernorTests`, 6 Swift `MarkdownTranscriptTests`, 3 partial-delivery
protocol tests, 6 delivery-engine tests, 3 rescue protocol tests, 6
rescue-engine tests, and 651 full Swift tests (519 app + 132 helper); P0 tests
remain. Final full workspace after the last R2b slice is recorded below; the
readiness-cache run is **4,598 passed, 0 failed, 6 ignored**; clippy exit 0.
R2b project policy was 4,559; R2b core before it 4,554; R2a 4,551. A live
temp-daemon CLI smoke exercised `run-contract-set`, `resources`, `top-up`, and
`readiness` end to end; a temp-daemon GUI smoke exercised first-selection
resource loading and the Project RunPolicy card/editor.

## Run Contract R3a/R3b/R4a (2026-11-25)

R3/R4 now have read-only allocation and trade-off previews plus two explicit
project opt-ins: one-shot completion-reserve rescue and runtime envelope
reallocation. No preview mutates stored state; R3b uses a runtime-only plan
copy and never rewrites the stored TeamPlan, budget, model route, approval,
effect, Verifier, or merge row. A separate explicit operator RPC can persist
one validated recorded overlay into a normal plan revision while the session
is quiescent.

- `session.allocation` / `vp allocation` reconciles the session envelope,
  stored TeamPlan DAG/critical path, task queue, bounded project usage history
  (newest 1,000 rows), readiness, approvals, effects, failure diagnosis, and
  stall observations. Per-task estimate precedence is session actuals, plan
  task envelope, project department actuals (nearest-rank p50/p90), department
  envelope, then an explicit unknown prior. An unknown estimate is `None`, not
  zero; a path with no estimate is `insufficient_estimate`, a partly estimated
  path is `partial_estimate` (with `estimated_tasks`/`total_tasks`), and an
  unestimated task is not feasible. Zero-token/zero-cost history falls through
  to the department envelope. Token, cost, and time reserve pressure is exposed
  as `pressure_dimensions`; per-task feasibility checks token/cost estimates
  against the task budget and session remaining (cost-only estimates are not
  treated as unestimated). Critical-path failure reports
  `critical_path_available=false` plus a bounded reason instead of silently
  presenting the all-task fallback as the path. Each task reports stored and
  runtime effective budgets plus `envelope_reallocated` when an R3b event
  applies.
- `session.tradeoffs` / `vp tradeoffs` returns the five documented profile
  postures, the current profile, the **active contract reserve and its source**
  (profile rows are labeled posture, not an implicit reserve change), the
  existing session turn/cost forecast (repeated, not a per-profile guarantee),
  a bounded quality/reviewability review, and a heuristic calibrated
  confidence score. Test evidence comes from the test cache with a pass/fail
  flag; a failing test lowers reviewability/confidence, and a queued or
  conflicting merge entry is not labeled merged and does not add merge
  confidence. A different profile is badged recommended only for deterministic
  evidence (quality for risk/blockers/blocked effects, cost under token/cost/
  time completion-reserve pressure); the current profile is never double-badged
  by the client and no profile is switched.
- `ProjectRunPolicy.auto_rescue` is off by default. When enabled, at most one
  automatic completion-reserve grant is appended per session, only when the
  turn pause is exactly `completion_reserve_*`, the session is fixed-budget and
  near-completion eligible (latest commit, zero no-progress turns, token/cost
  headroom, no prior grant). Manual and automatic grants and consumption share
  one in-process lock; eligibility reads run outside it. A manual grant consumes
  the automatic allowance; concurrent boundaries cannot append two grants or
  consume one grant twice. An auto-rescue store/readiness error is logged and
  the normal resource pause is kept, while a failure to persist the one-shot
  consumption receipt fails closed and stops the turn. The grant releases only
  the completion reserve for its 30-minute TTL.
- The dispatch/turn governors and wall-clock preemption ignore retained
  token/cost/time caps when `Session.unlimited` is on, while attention,
  readiness, risk, context, approvals, and verification still gate work. This
  keeps the plan path consistent with the prompt path.
- The completion reserve is measured against the effective allowance, not a
  separate hard cap; Auto sessions leave that reserve to the Auto controller,
  matching the turn-boundary exemption. Resolved/denied approvals no longer
  count as operator attention or risk (only pending approvals and unresolved
  effects do).
- A gate or task-scope budget deferral settles only its task/batch; unrelated
  siblings stay pending for the scheduler. A session-scope budget stop and an
  explicit cancellation still settle the remaining plan.
- `sync_plan_tasks` resets a task's projection when a plan revision added that
  id after it had been removed, so a re-added task starts pending with zero
  attempts instead of inheriting an old failed/cancelled row.
- A landed program session can still complete an already-completed,
  integration-clean milestone. A cancelled intake item can be manually
  dispatched again without a separate retry command.
- `ProjectRunPolicy.auto_reallocate_envelopes` is off by default. When enabled,
  a Worker/Labour run gets a runtime-only effective task budget from unused
  token/cost capacity of already-completed tasks in the same department.
  Sources are consumed independently per token/cost dimension; only capacity
  that fits under the department cap is applied and recorded. A prior target
  event replays idempotently, a completed target can become a source with its
  effective budget, and a per-target plan fingerprint over the target task and its
  department budgets invalidates an old override instead of widening the new
  plan; unrelated task edits do not. `auto_reallocate_cross_department`
  (default off) additionally lets completed tasks in other departments supply
  capacity, but the target department cap still bounds what is applied. The stored
  TeamPlan and every department, session, and project cap are unchanged;
  Verifier/Scout see the recorded read-only overlay but are not bound by the
  implementation task envelope (they keep department/session/project caps and
  the completion reserve); only non-auto-approve Worker/Labour runs may append,
  and a per-session same-daemon lock serializes event read/append. The
  allocation preview reports stored/effective budgets and uses effective values
  for estimates and critical-path weight.
- `ProjectRunPolicy.auto_persist_envelopes` is off by default. Before the next
  plan run, still-valid recorded overlays are written into one normal plan
  revision. It records budgets the runtime already allowed, skips
  protected/terminal tasks, never changes task status or creates capacity, is
  idempotent because the revision makes old events stale, and a failure logs
  without blocking the run. The project policy RPC/CLI and macOS editor expose
  the toggle.
- Program milestone completion accepts the latest merge attempt's
  deterministic Integrator `merged` status (legacy `integrated` rows remain
  accepted) and ignores older failed attempts, so a successful retry does not
  look like failed integration and burn the remaining program allowance.
  An explicitly skipped milestone task blocks instead of completing.
  Auto-resolution only considers the latest plan revision and skips a conflict
  whose task was removed or whose latest row is
  `skipped`/`cancelled`/`completed`; an explicit operator resolve can still
  act on it.
- `session.envelope.persist` / `vp envelope-persist --task <id> --yes` writes
  the latest still-valid recorded runtime overlay for one task into a normal
  plan revision. It requires a quiescent session and re-folds the event through
  the same fingerprint/old-budget validator that the runtime uses; a stale
  overlay fails closed. It supports an optional expected revision, does not
  create new capacity, and no scheduler/controller/model path calls it.
  Engine and daemon RPC tests cover persistence, stale-overlay rejection, and
  the required confirmation; it was not exercised live because the live plan
  produced no reallocation event before its deadline.
- macOS: Project RunPolicy toggles for completion-reserve rescue and runtime
  envelope reallocation, with the fixed-budget/no-bypass and
  stored-plan/caps-unchanged caveats, and Manage Dash → Usage & Budget Allocation
  insight and Trade-offs & quality cards. The cards load only when visible or
  explicitly refreshed, reject responses for another session, and fail closed to
  a visible error.
- CLI: `project-run-policy-set --auto-rescue/--no-auto-rescue`,
  `--auto-reallocate-envelopes/--no-auto-reallocate-envelopes`, `allocation`,
  and `tradeoffs`. The project RunPolicy human renderer includes both automatic
  states with short caveats.
- A temp-daemon CLI smoke exercised `project-run-policy-set --auto-rescue`,
  `project-run-policy`, `allocation`, and `tradeoffs` against a real daemon and
  temporary repository. A temp-daemon GUI smoke exercised the two cards and the
  policy toggle: the trade-off card showed `active reserve 20% · project
  policy · profile rows are documented posture`, profile rows showed `posture
  25%` rather than an implied contract change, the allocation card showed
  `0/0 task(s) estimated`, and CURRENT was not duplicated by RECOMMENDED. The
  policy editor also showed `AUTO ENVELOPES ON`, saved both opt-ins, and the
  daemon reported `auto_rescue=true` and
  `auto_reallocate_envelopes=true`. No provider was configured, so no live
  turn-boundary auto rescue or runtime envelope reallocation was triggered
  (unit/daemon tests cover both paths).
- Tests: targeted allocation, trade-off, auto-rescue, envelope-persist,
  protocol, CLI, and daemon tests; full workspace
  **4,679 passed / 0 failed / 6 ignored**;
  Swift **659 passed (527 app + 132 helper)**; fmt, clippy,
  SwiftFormat, and `xcodebuild test` pass.
- Live DeepSeek evidence (2026-11-25): the explicit smoke passed with
  `deepseek-flash` at `https://api.deepseek.com` using an environment-only key;
  the key was never written to the repository, events, logs, or UI (the
  temporary 0600 env file was deleted). Provider stream passed. The small live
  cache-bench (4 turns, ~4k stable prefix) reported 19,290 input / 18,560
  cache-read / 730 miss / 96.2157% hit, cost $0.000858; it is not the 200k
  long-prefix acceptance run. A full agent E2E reached idle with 3 calls,
  36,126 input / 337 output / 24,192 cache-read / 66.97% hit. A second live
  session (4 calls, 49,907 input / 1,119 output / 75.92% hit) exercised
  `allocation` and `tradeoffs`: allocation returned no remaining tasks because
  the Leader completed the simple goal without storing a TeamPlan; tradeoffs
  returned 5 profiles, active reserve 20% from `project_policy`, no recommended
  profile, dirty/watch quality, low confidence (0.29, 4 samples), and honest
  warnings. Per-task effective/reallocation was not exercised by that live
  session; unit/daemon tests cover it. A two-task live plan attempt with
  30k/30k task budgets exposed a real hot retry loop: T1 wrote the file but
  exhausted its task budget at 24,877 spent before the next 12k request, then
  the orchestration stale-block cleanup unblocked it 43 times because its
  dependencies were complete; the controller now only releases dependency/legacy
  blocks, and the scheduler/claim path only dispatches pending/interrupted
  rows; failed work waits for the controller retry transition, while
  budget/gate blocks stay blocked. Abort/no-ready batches now preserve an
  existing `blocked` reason instead of rewriting it into a releasable
  dependency block, while an explicit session cancellation (including a
  mid-batch abort) still settles it; a pending retry whose `next_attempt_at`
  has not arrived stays pending instead of being settled blocked. A terminal
  gate resolve/cancel/expiry explicitly unblocks its recorded plan task only
  after the current row and latest block event still match; the dispatch
  resource and critical-path guards run before queue/lease/plan-resume events
  with the effective Auto budget and deduplicated decision/admission events,
  so a terminal block does not append a full resume cycle on every scheduler
  cooldown. Readiness and program evidence use the latest merge attempt per
  task, so an older failed attempt does not permanently block a successful
  retry. Auto pause now writes `session.auto_paused` before the paused status
  projection, so a dropped or cancelled run cannot leave a paused session
  without its event. Program and intake dispatches that the governor defers
  now report not-started, cancel the empty session, and retry instead of
  leaving a Running milestone or a fake started intake. A second
  attempt with 80k/80k budgets completed T1 and started the Verifier, but the
  session hit its 900s wall-clock budget before T2; 24 calls, 317,308 input /
  7,367 output / 87.66% cache hit, T1 completed, T2 blocked. No
  `session.envelope.reallocated` event was emitted before the deadline; the
  runtime reallocation path remains unit/daemon-tested rather than live-proven.
- Limits: automatic reallocation with reservation semantics,
  cross-session/project pooling, reservation redistribution, and the daily
  attention schedule remain open. Explicit one-task and opt-in pre-run
  auto-persistence record existing overlays, but R3b remains an event-derived
  runtime overlay by default, not an automatic reallocation controller; it
  scans the newest 1,000 reallocation events, so source single-use memory can
  be forgotten after very heavy plan revision (department/session/project caps
  still bound actual spend). A target/department budget revision invalidates an
  old overlay; a Worker retry may then need a top-up or plan edit, while
  Verifier/Scout are not task-cap bound. Reallocation events are schema v2 with
  source fingerprints; pre-v2/legacy events without a matching fingerprint are
  ignored (their sources remain consumed). R3b binds sources before the first
  governor/approval gate; a run denied at admission leaves the event for its
  retry. Estimates are sparse historical heuristics, not vendor token counts; task feasibility is a plan-envelope
  forecast, not a next-call reservation; test-cache evidence may predate the
  latest commit;
  confidence is not a calibrated probability; profile forecasts are not
  per-profile; profile reserves are documented posture, not automatic contract
  changes; rescue is same-daemon and envelope serialization is per-session
  same-daemon; project RunPolicy saves remain a whole-policy read-modify-write
  without a compare-and-set precondition, so concurrent editors can lose the
  other field.

## V1 Phase 1 closure batch (2026-11-26)

The following carried items have closure evidence; the backup cap drill
remains partial, and this is not a claim that Phase 1 is complete.

- **Approval mode fail-open**: an invalid `VERLINPARK_APPROVAL_MODE` now resolves
  to `strict` (fail closed) instead of silently becoming `smart`; unset keeps
  the normal default. Test: `agent::service::constructors::approval_mode_tests`.
- **Remote ACP cleartext/credentials**: remote `http`/`ws` ACP endpoints are
  rejected; cleartext is allowed only for loopback (`localhost`/loopback IP),
  redirects are disabled, and credential-like header values must be an exact
  `${ENV_VAR}` reference (an optional Bearer/Basic prefix is allowed) or use
  `auth_header_env`. Tests: `endpoint_schemes_match_the_profile`,
  `literal_credential_headers_are_rejected`.
- **Schema downgrade guard**: `Store::open` records `PRAGMA user_version` and
  refuses to open a database written by a newer schema. Test:
  `refuses_a_newer_database_schema_version` and `schema_guard::tests`.
- **Imported archive repair**: `repair_session`/`rebuild_session`/event-log
  restore now reject `imported` sessions through the shared quiescence guard.
  Test: import/repair assertion in the bundle round-trip test.
- **Backup verification cap (PARTIAL)**: blob verification reports `partial`
  and fails `valid` when the 100k blob cap leaves referenced blobs unverified;
  the anti-rollback anchor is not advanced by a partial verification. The list
  report uses the same classification. Only the mode mapping has a unit test;
  the 100k cap/valid/list/anchor drill still needs a fixture or configurable
  limit.
- **Backup scope truthfulness**: CLI human output and Settings now state that
  backup covers VerlinPark data/optional artifacts, not Git worktrees or
  source, and recommend landing/pushing plus encrypted repository backups.
- **db-rebuild malformed sources**: `read_project_manifest` already fails closed
  on malformed/truncated manifest lines and version-capped streams. Missing or
  empty session mirrors and mirrors without a valid `session.created` now reach
  the `skipped` report; malformed JSON lines still abort the rebuild rather than
  being silently ignored. Regression coverage: the rebuild round-trip test adds
  an orphan session directory.

Tests: **4,686 Rust passed / 0 failed / 6 ignored**; `vp-store` 436; Swift
659; clippy/fmt/SwiftFormat/xcodebuild pass. Remaining carried groups are
listed below; Phase 0/2 live and scale evidence is still required.

## Carried findings from the V1 plan

The following groups are carried from [V1_FINAL_PLAN.md](V1_FINAL_PLAN.md) §6.
They stay `OPEN (carried)` until a dedicated audit records evidence. This
ledger does not claim they are fixed.

- **Data safety/security**: tool-secret redaction; safe shell/fetch defaults;
  native computer/Code Mode experimental gating.
- **Core runtime**: Auto Budget across TeamPlan Workers/Labour (R2b adds
  token/cost/time request-fit, but first-call pricing, parallel reservation
  accuracy, and live multi-Worker evidence remain open); in-run plan revision;
  source rebuild regression coverage.
- **First-run/local usability**: provider configured without a key; text-only
  probe; wizard without a project; non-Git project add; Git identity preflight;
  New Session Auto cap/provider gate; project removal; wizard/daemon startup
  race; real-machine Keychain/TCC verification.
- **Scale/precision**: 100+ task plans, 8–10 Workers, long DAGs, parallel write
  scopes, merge pressure, conflict handling; long sessions, compaction,
  checkpoint/resume, daemon restart, provider stream failure; performance
  budgets; state-transition fault injection; diagnosability from events.
- **Public release** (after personal V1 stability): version alignment, release
  preflight, signing/verification, clean-Mac install, provider matrix, notices,
  dependency audit, claim reconciliation.

## Verification commands

Rust targeted final (after the last steering guard and P0/R1/R2a/R2b):

```bash
cargo test -p vp-core --lib steering
# 8 passed, 0 failed (includes the 7 operator-route tests and the inspector test)
cargo test -p vp-daemon --bin verlinpadd tests::agent_operator_rpc_tests
# 3 passed, 0 failed (operator route, read-only snapshots, contract override/top-up RPCs)
cargo test -p vp-core --lib engine::resources::tests
# 14 passed, 0 failed (snapshot/readiness/attention/governor/override/top-up/project defaults/lean turn/readiness-dispatch-only/attention batch/cache TTL)
cargo test -p vp-core --lib project_run_policy
# 2 passed, 0 failed (project defaults/override precedence + manifest rebuild)
cargo test -p vp-protocol run_stop_policy
cargo test -p vp-protocol project_and_session_run_policies
# 2 passed, 0 failed (bounded stop policy and project/session rejection)
cargo test -p vp-protocol partial_delivery
# 3 passed, 0 failed (params/record/result round-trip and additive defaults)
cargo test -p vp-core --lib engine::delivery::tests
# 6 passed, 0 failed (confirm/active/imported/empty/complete/bounds/export-import)
cargo test -p vp-protocol rescue
# 3 passed, 0 failed (grant/status params and stable error codes)
cargo test -p vp-core --lib engine::rescue::tests
# 6 passed, 0 failed (eligibility, one-shot reserve release, expiry, bounds)
cargo test -p vp-store --lib
# 436 passed, 0 failed (incl. project RunPolicy round-trip/validation and schema guard)
cargo test -p vp-core --lib agent::auto_budget_runtime_tests
# 27 passed, 0 failed (token/cost/time child request-fit, turn pause/resume, no-progress, wall clock)
cargo test -p vp-core --lib agent::tests
# 368 passed, 0 failed (agent/daemon-agent integration filter)
cargo test -p vp-core --lib 'tools::tests'
# 28 passed, 0 failed (incl. full-hash, scratch env, shell confinement)
cargo test -p vp-core --lib 'permissions::tests::read_only_git'
# 2 passed, 0 failed (absolute read-only git vs clean destructive)
cargo test -p vp-core --lib 'tooling::project::tests'
# 16 passed, 0 failed (incl. bare Python unittest detection)
cargo test -p vp-core --lib 'tooling::files::tests'
# 8 passed, 0 failed (incl. full-hash display)
cargo test -p vp-core --lib 'contract::tests::shell_scratch'
# 1 passed, 0 failed
cargo test -p vp-core --lib 'agent::plan::execute::tests'
# 1 passed, 0 failed (EFFECT_UNKNOWN blocked classification)
cargo test -p vp-daemon
# 54 unit + 1 governance passed
cargo test -p vp-daemon partial_delivery
# 1 passed, 0 failed (RPC record/read/typed confirmation error)
cargo test -p vp-daemon rescue
# 1 passed, 0 failed (RPC preview/grant/typed confirmation error)
cargo test -p vp-cli
# 194 unit + 10 integration + 1 governance passed
cargo test -p vp-cli --bin vp delivery
# 3 passed, 0 failed (CLI parse/--yes/human render)
cargo test -p vp-cli --bin vp rescue
# 3 passed, 0 failed (CLI parse/--yes/human render)
```

Final full workspace after the R2b readiness-cache slice (2026-11-25, healthy
boot):

```bash
cargo test --workspace
# 4,598 passed, 0 failed, 6 ignored
cargo clippy --workspace --all-targets -- -D warnings
# exit 0
```

R2b project policy was 4,559, R2b core before it 4,554, R2a 4,551, R1 4,546.
Full Swift after the slice: 651 (519 app + 132 helper), including the Markdown
transcript renderer, display preferences, attention batch prompt, partial
delivery sheet, rescue reserve surface, and readiness cache path.

The earlier full run before P0/R1 also passed every crate except the daemon
operator test, which was fixed and is covered above. The host XProtect/
`syspolicyd` spin is intermittent and documented separately in
[HOST_SYSPOLICYD_INVESTIGATION.md](HOST_SYSPOLICYD_INVESTIGATION.md); it is a
macOS 27 host defect, not a code failure. The final green run above was taken
on a healthy boot after the recommended conditions.

Swift:

```bash
cd apps/macos && xcodegen generate
xcodebuild -project VerlinPark.xcodeproj -scheme VerlinPark \
  -destination 'platform=macOS,arch=arm64' \
  -only-testing:VerlinParkTests/ResourceGovernorTests test
# 11 passed, 0 failed (top-up decoding/validation, readiness caveats/report)
xcodebuild -project VerlinPark.xcodeproj -scheme VerlinPark \
  -destination 'platform=macOS,arch=arm64' \
  -only-testing:VerlinParkTests/RuntimeReliabilityTests test
# 31 passed, 0 failed (earlier round)
```

Live provider smoke (key from the daemon environment/Keychain only):

```bash
vp --data-dir "$TEMP_DATA" provider-test
```

The live runs are recorded above. They are not a substitute for the full
Rust/Swift suites or for the remaining acceptance scenarios.

### 2026-11-26 runtime efficiency P0

The first live spatial run exposed four runtime costs that are now fixed or
bounded locally:

- **Budget refresh for every role.** `SessionBudget` was built from a run-start
  session snapshot; `before_turn`/`ensure_request_fits` only applied persisted
  top-ups while the Auto runtime was active. Fixed/unlimited Worker/Labour runs
  could therefore keep an old limit and report `session tokens budget exhausted`
  after an operator top-up or `unlimited`. `run_loop_body` now refreshes the
  persisted effective limits at every turn boundary for all roles, and
  `run_agent` applies the starting status to every role. Test:
  `fixed_budget_refreshes_from_operator_top_up_and_unlimited`.
- **Completion reserve cap.** The proportional reserve (default 20%) could park
  a 4M-token session at 3.2M for 800k tokens. Token/time/cost reserves are now
  capped at 200k tokens / 300s / $0.75, so the reserve protects the final
  commit/verify/merge calls without blocking a healthy task. Test:
  `large_fixed_budget_completion_reserve_is_capped`.
- **Delta coalescing.** Provider text deltas were persisted one event per token,
  making the SQLite event log fall minutes behind operator polling. The first
  non-empty delta is flushed immediately for stream-start observability; later
  deltas are coalesced at 8 KiB or 120 ms, and the final `agent.message` remains
  complete. Test: `coalesces_stream_deltas_into_bounded_events`.
- **Output-limit recovery.** A provider `finish_reason=length` used to fail the
  whole task and replay its context. It is now a bounded same-run recovery:
  the runtime emits `agent.output_truncated`, persists a split instruction, and
  continues up to two times. Test:
  `recovers_once_from_a_provider_output_limit_without_failing_the_task`.
- **Bounded writes.** `write_file` rejects a single content payload above 48 KiB
  with a typed `INVALID_ARGUMENT` split hint and supports `append:true` for
  chunked files. Test:
  `write_file_caps_one_call_and_append_builds_chunks`.
- **Finalize window.** Leader/Worker/Labour runs receive one persisted
  `agent.finalize_nudge` message during the last 3–6 turns, asking them to stop
  exploring, run the bounded check, and commit. This is advisory; it does not
  auto-commit. Test: `injects_a_finalize_nudge_before_the_turn_limit`.

- **Soft context budget.** `RoleModelConfig.context_budget` (optional) now
  moves the compaction threshold to 90% of that soft target while the hard
  request limit remains the model window minus output/framing reserves. This
  lets a 1M-token model keep its capability but have Worker/Labour sessions
  compact around 59k instead of growing a 70k active context across hundreds of
  calls. Test:
  `soft_context_budget_moves_threshold_not_the_hard_window`. The field is
  optional and default-off, so operators must set it per role.
- **Plan scope cap.** `validate_team_plan` now rejects more than 8 acceptance
  items per task (and empty/over-300-character items); the schema advertises
  the same bound. This prevents a single task from carrying the 22-item scope
  that made the live T1 run so expensive. Test:
  `rejects_plans_with_more_than_eight_acceptance_items`.
- **Optional-lock noise.** Worktree and `git` tool commands now set
  `GIT_OPTIONAL_LOCKS=0`; the shell sandbox already disabled `gc.auto`. This
  removes the non-fatal `packed-refs.lock: Operation not permitted` warning on
  a read-only project root.

A small live smoke after the slice used a fresh temp daemon and the `PDfuji`
project: one task `smoke-file`, a Worker `write_file` of
`site/xeno-smoke/hello.txt`, a Verifier approval, and an Integrator merge
(`cc3cbed` worker commit, `dcdcbc1` merge). The task completed; session cost was
about 287k tokens, and the session reached idle after a top-up. This confirms
the new runtime path still completes a real four-tier task; it does not
exercise the 64k context target or a mid-run budget top-up by itself. The
`packed-refs.lock` stderr warning still appeared even with
`GIT_OPTIONAL_LOCKS=0`/`gc.auto=0`; it remains cosmetic here but needs a
focused Git-ref test.

Local verification after this slice: `cargo test --workspace` 4,702 passed /
0 failed / 6 ignored; Swift `xcodebuild test` 659 passed / 0 failed;
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
and `swiftformat --lint` pass. The reserve caps are constants for this slice
rather than project-configurable; the finalize nudge is not a hard finalize;
event deltas still persist in SQLite but are bounded; and per-task tool-call
budgets, the deterministic browser probe, and default per-role context budgets
remain open.

### 2026-11-26 live DeepSeek PDfuji retry: harness fixes, partial E2E

A real `deepseek-flash` run against the `PDfuji` project exposed four harness
defects and one still-open budget/governor problem. The fixes in this round are
locally test-covered but the live acceptance path is not complete.

- The daemon was configured to a 1,000,000-token context window, 16,384 output
  tokens, 40 turns, and unattended approvals. A new
  `agent.runtime_config.set` RPC (and `vp runtime-config-set`) changes these
  without rebuilding the provider, so an env-only or Keychain-backed key is not
  lost. The GUI approval-mode action also uses a dedicated RPC now instead of
  re-running provider configuration.
- `submit_team_plan` validation errors are now typed `INVALID_ARGUMENT` with a
  field-specific hint, and the schema explains that writing tasks need
  `profile="coding"` while verification tasks are read-only with an empty
  `write_scope`. The live Leader submitted a valid one-department plan on the
  first attempt of the retry.
- A repeated-tool-failure guard stops a run after three consecutive
  deterministic plan/verdict failures (six for general tools) and records
  `agent.tool_failure_loop`; a regression test covers three invalid
  `submit_team_plan` calls.
- Resume now normalizes incomplete provider tool history: missing results for
  an interrupted assistant `tool_calls` turn are synthesized in memory and
  orphan tool messages are dropped. This unblocked a session that had been
  poisoned by a cancellation and then rejected by the provider with HTTP 400.
- Live plan execution started, the Worker wrote the four `site/xeno/` files,
  passed a bounded local check, and committed exactly those four files
  (`2e30b38`). The Verifier then started but failed at the session budget
  boundary; no merge queue entry or session-branch integration was produced.
- The run still burned far more input tokens than a small static site should
  need, and the finalize loop repeatedly hit completion-reserve pauses. Setting
  the session budget or unlimited mode during an active plan run did not
  reliably take effect for that run; the site was preserved and visually
  reviewed, but the live Verifier/Integrator path remains open. Do not claim a
  successful four-tier merge from this run.

Local verification for the harness fixes: `cargo test --workspace` 4,694
passed / 0 failed / 6 ignored; Swift `xcodebuild test` 659 passed / 0 failed;
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
and `swiftformat --lint` passed. The built site was copied to the target
project's `site/xeno/` for visual review and is not part of this repository's
commit.

### 2026-11-26 live PDfuji spatial-instrument run (completed E2E)

A second real `deepseek-flash` run on the `PDfuji` test project completed the
four-tier path with a deliberately non-human spatial output.

- Session `01a21441-936c-728a-8f90-232468cfffa1`, temporary data dir
  `vp-live-xeno2`. Runtime was 1,000,000 context tokens, 32,768 output tokens,
  60 turns, unattended approvals, and session-unlimited after the completion
  reserve paused the run.
- Plan: one Manager department `xeno-spatial` and one coding task `T1`
  (`write_scope: site/xeno/`). No separate verification plan task.
- The Manager/Worker produced the chamber, core, spine, console, CSS, and
  seven strata. A Labour agent (`xeno-instrument-js`) wrote the 30 KB
  behavioural script and browser-tested language switching, keyboard digits,
  and linear mode. The final Worker hit max turns before committing the last
  three files, so the task was revised to a finalize-only objective; the next
  Worker committed them without rewriting the design.
- Commits: `40efe88` (script.js), `bdce113` (index.html, style.css,
  README.md), merge `c5ae960` into the session branch. The Verifier returned
  `APPROVED` with `merged: true`; the deterministic Integrator recorded
  `merged; diff check passed`; task `T1` is `completed`.
- A visual review found two responsive issues: the fixed mobile console could
  cover the last line of a stratum at 360px, and the English facet values
  collapsed into narrow stacks on desktop. A second plan revision added task
  `T2` (dependency `T1`, same write scope); it changed only `style.css`
  (commit `dc14a34`, merge `42eba28`) and the Verifier again returned
  `APPROVED` with `merged: true`. `T2` is `completed`. The final desktop
  (1400×900) and narrow (460/360px) reviews show the single-viewport model
  preserved, the bilingual toggle persisting, the linear fallback working, and
  the dossier links returning HTTP 200 when served beside the existing tree.
- An automated headless-Chrome CDP smoke test at 320×568, 360×800, and
  1400×900 passed: no console errors, 7 strata, 137/137 `data-en`/`data-zh`
  nodes, mobile lens `overflow-y: auto` with the bottom scroll position reached
  (348/448px of scroll room at 320/360), EN/中文 persisted under `xeno.lang`
  across reload, linear mode exposed all 7 strata, desktop facet values stayed
  on one line (138–207px wide, 18px high), and `scrollWidth == viewport` at
  320 and 1400. Linear mode is intentionally session-scoped rather than
  persisted; the README only promises language persistence.
- Output: single-viewport irregular lens, orbital core/canvas field, numbered
  glyph spine, floating console; seven strata (Signal, Profile, Mechanism,
  Containment, Lattice, Log, Provenance); EN/中文 copy with 137/137
  `data-en`/`data-zh` pairs; spatial and linear modes; native `<details>` log;
  no external fonts, images, CDNs, trackers, or network requests. The five
  host-relative dossier links returned HTTP 200 when served beside the
  existing `site/dossier.html` tree.
- Live cost was very high: about 15.1M input / 266.7K output tokens across
  T1 and T2. Most of it was exploratory browser testing, repeated budget
  pauses, and the finalize retry. This run is evidence that the path can
  complete, not an efficiency claim.
- The target project `/Users/apple/orca/projects/TEST` was not modified in
  this round; the successful output lives in the VerlinPark session worktree
  and session branch, not in the target repository.
- The initial Verifier flagged narrow desktop facet wrapping and the mobile
  console overlap; `T2` fixed both. Remaining non-blocking notes: the skip
  link, noscript note, and four aria labels are English-only; dossier
  fact-to-source tracing was not re-checkable from the task worktree. These
  are open polish items, not completion claims.

## Release hardening (2026-11-27)

### Closed in this batch

- **Project add accepted non-Git/empty directories.** `Engine::preflight_project`
  now requires a canonical Git repository root, at least one commit, and
  configured `user.name`/`user.email`; the daemon `project.add` RPC runs it
  before inserting a row. Tests: four `vp-core` preflight tests plus
  `project_add_rejects_a_non_git_directory_before_insert`.
- **Remote provider without a key appeared configured.** `AgentService::from_env`
  now trims/filters empty keys and returns `unconfigured` for a non-loopback
  endpoint with no API key; loopback endpoints remain usable without one.
  Tests: `identifies_loopback_endpoints` and
  `remote_provider_requires_an_api_key`.
- **Text connectivity did not prove tool calling.** `agent.probe.tools` /
  `vp provider-test --tools` sends one explicit `probe_echo` tool request and
  reports whether the model emitted the expected call. It performs no local
  tool execution. Tests: `tool_probe_requires_the_expected_tool_call` and
  `tool_probe_reports_text_only_providers`. This is a live capability check, not
  evidence that a specific real endpoint was probed in this workspace.
- **Doctor lacked Git and shell-trust readiness checks.** `server.doctor` now
  reports `git projects` and `shell trust` rows; the latter warns while a
  project still uses the permissive default (`network=allow`, no read jail).
- **First-run setup could claim connected without a project or a probe.** The
  wizard now requires an added project, an explicit shell trust decision
  (hardened recommended), and a successful live `agent.probe` before Continue.
  It also runs the one-call `agent.probe.tools` check and labels tool calling
  as verified or unverified; unsupported tool calling is never silently called
  "connected". Session creation is also blocked until a provider is configured.
- **Wizard startup raced the daemon.** First-run now waits a bounded time for
  the local daemon, shows connection state, and provides an explicit Retry that
  cancels automatic backoff; setup controls stay disabled until connected.
- **The first-run shell decision was implicit.** The wizard offers hardened
  shell access (network deny plus HOME/volume read jail) by default and standard
  access as an explicit opt-in; the policy is applied as soon as the project is
  added, and again on finish if the toggle changed. `vp project-add
  --hardened-shell` applies the same policy from the CLI.
- **No scripted backup/restore drill.** `scripts/backup-restore-drill.sh` seeds
  a real daemon data directory, adds a Git project with `--hardened-shell`,
  checks the persisted shell policy, then proves v1 ZIP and signed/encrypted
  repository (v3) restore round trips and project-row equality. Run with
  `just backup-drill`; `just release-preflight` includes it.
- **Tool/approval secrets could be persisted.** A deterministic,
  persistence-boundary redactor now sanitizes `tool.started`/`tool.finished`
  payloads (including the journal path), approval event/row input, URL query
  credentials, cookie headers, bearer/JWT/provider token shapes, and
  assignment-shaped secrets. Execution arguments, effect probes, contract
  checks, and journal fingerprints still use the real input. Tests:
  `crates/vp-core/tests/secret_redaction.rs` plus the redaction unit tests.
- **No packaged DMG verification.** `scripts/package-release.sh` produced a
  universal (x86_64/arm64) `0.1.0-dev` DMG with the bundled daemon, CLI,
  remote helper, and CUSTOS binaries, ad-hoc signed. `scripts/verify-dmg.sh` /
  `just verify-dmg` verified its checksum, image, app/helper layout, and code
  signature without installing it. This is not Developer ID signing or
  notarization.
- **A bad project entry could not be removed.** `project.remove` /
  `vp project-remove --yes` removes only an empty project (no sessions,
  identities, workspaces, or intake tasks), refuses without explicit
  confirmation, never deletes repository files, and clears the manifest when
  the last project is removed. The Rail project menu exposes the same action
  with a confirmation alert. Tests:
  `removing_an_empty_project_clears_the_manifest`,
  `removing_a_project_with_sessions_fails_closed`, and
  `project_remove_requires_explicit_confirmation`.
- **No bounded scale/fault harness.** `vp bench scale` validates and completes a
  100-task/10-Worker plan through the real TeamPlan validator and task
  scheduler projection, and `vp bench fault` covers two real fake-provider
  runtime faults (pre-output stream failure, truncation, cancellation) plus two
  explicitly labeled harness models (duplicate-call replay and batch abort).
  This is CI-scale structural evidence, not the Phase 2 live 10-Worker/soak
  proof.

Still open in the same area: redaction covers tool/approval event and row
payloads only; legacy rows are not rewritten, and provider-facing conversation
messages, artifacts/sources, file content, and journal/effect inputs are not
filtered, so a secret echoed into those surfaces can still persist. Matching is
pattern/key based, not entropy detection. Native computer and Code Mode are not
yet a project-level explicit opt-in (computer remains helper/TCC-gated and Code
Mode remains capability-limited, but both are still advertised by default);
Keychain/TCC first-run needs a real-machine pass; the provider tool probe has
not yet been run against a live endpoint in this workspace; automatic project
creation from non-GUI clients does not force a shell trust decision; and the
scale/fault harness does not replace live multi-Worker, merge-conflict, daemon
restart, or 72-hour soak evidence.

### Evidence run (2026-11-27)

| Check | Result |
|---|---|
| `cargo test -p vp-core --lib` | 2,723 passed / 0 failed / 2 ignored |
| `cargo test -p vp-core --test secret_redaction` | 3 passed |
| `cargo test -p vp-bench` | 164 passed / 1 ignored |
| `cargo test -p vp-cli` | unit + integration suites passed |
| `cargo test -p vp-daemon` | 57 passed plus module governance |
| `cargo test -p vp-protocol` | 306 passed |
| `just backup-drill` | v1 ZIP plus signed/encrypted repository restore passed |
| `vp bench scale` | PASS, 100/100 tasks, 10 batches, peak 10 |
| `vp bench fault` | PASS, 5/5 cases, 3 real fake-provider runtime cases |
| `cargo clippy ... -D warnings` | passed |
| `swiftformat --lint` | passed |
| `xcodebuild build` | passed |
| `scripts/package-release.sh` | DMG created (universal, ad-hoc signed) |
| `just verify-dmg` | checksum, image, helpers, layout, signature passed |

A full `cargo test --workspace` run reported 4,802 passed / 0 failed / 6
ignored before the final empty-project-removal tests were added; those three
tests and all touched-crate suites passed individually afterward. The run is
not claimed as a clean final full-workspace number until it is repeated without
concurrent cargo builds.

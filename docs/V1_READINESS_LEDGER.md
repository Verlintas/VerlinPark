# VerlinPark V1 readiness ledger

- Status: Active
- Date: 2026-11-25
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
| V1-D12 | S2 | Operator attention had no budget or governor: repeated host-state approvals could consume the run without triggering a policy decision. | **PARTIAL (R2b)** | Pre-dispatch deferral and a true turn-boundary pause (`session.resource.paused`) now stop new work at the attention cap; a bare resume re-pauses until the cap/policy changes. The pause carries a bounded attention batch (risk groups + at most 8 approval metadata rows, no raw input) and the Stage banner opens Inbox or Policies. The existing approval queue, coalescing, and batch resolve remain the only decision path; no automatic grant/template/profile. Explicit partial delivery is event-only and available; rescue reserve and opt-in R3b runtime task-envelope reallocation are implemented; persisted plan-revision reallocation remains open. |

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
  reallocation, while persisted plan-budget mutation remains open.
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
effect, Verifier, or merge row.

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
- `ProjectRunPolicy.auto_reallocate_envelopes` is off by default. When enabled,
  a Worker/Labour run gets a runtime-only effective task budget from unused
  token/cost capacity of already-completed tasks in the same department.
  Sources are consumed independently per token/cost dimension; only capacity
  that fits under the department cap is applied and recorded. A prior target
  event replays idempotently, a completed target can become a source with its
  effective budget, and a per-target plan fingerprint over the target task and its
  department budgets invalidates an old override instead of widening the new
  plan; unrelated task edits do not. The stored
  TeamPlan and every department, session, and project cap are unchanged;
  Verifier/Scout see the recorded read-only overlay but are not bound by the
  implementation task envelope (they keep department/session/project caps and
  the completion reserve); only non-auto-approve Worker/Labour runs may append,
  and a per-session same-daemon lock serializes event read/append. The
  allocation preview reports stored/effective budgets and uses effective values
  for estimates and critical-path weight.
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
- Tests: targeted allocation, trade-off, auto-rescue, protocol, CLI, and
  daemon tests; full workspace **4,652 passed / 0 failed / 6 ignored**;
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
  dependencies were complete; the controller now only releases dependency
  blocks (or legacy blocks with no reason), never budget/gate blocks. A second
  attempt with 80k/80k budgets completed T1 and started the Verifier, but the
  session hit its 900s wall-clock budget before T2; 24 calls, 317,308 input /
  7,367 output / 87.66% cache hit, T1 completed, T2 blocked. No
  `session.envelope.reallocated` event was emitted before the deadline; the
  runtime reallocation path remains unit/daemon-tested rather than live-proven.
- Limits: persisted plan-revision reallocation, cross-department/session
  pooling, reservation redistribution, and the daily attention schedule remain
  open. R3b is an event-derived runtime overlay, not a TeamPlan rewrite; it
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

## Carried findings from the V1 plan

The following groups are carried from [V1_FINAL_PLAN.md](V1_FINAL_PLAN.md) §6.
They stay `OPEN (carried)` until a dedicated audit records evidence. This
ledger does not claim they are fixed.

- **Data safety/security**: tool-secret redaction; safe shell/fetch defaults;
  backup-verify cap/anchor; backup scope warning; remote ACP cleartext/headers;
  invalid approval mode fail-open; native computer/Code Mode experimental
  gating; schema downgrade guard.
- **Core runtime**: Auto Budget across TeamPlan Workers/Labour (R2b adds
  token/cost/time request-fit, but first-call pricing, parallel reservation
  accuracy, and live multi-Worker evidence remain open); in-run plan revision;
  db-rebuild silent skips; imported archive repair semantics; backup-verify/
  source rebuild regression tests.
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
# 434 passed, 0 failed (incl. project RunPolicy round-trip/validation)
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

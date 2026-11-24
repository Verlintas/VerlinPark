# Run Contract and Resource Governor plan

- Status: Accepted; P0/R1/R2a/R2b implemented plus the R3 allocation preview and opt-in runtime per-task envelope reallocation and the R4 trade-off/quality preview slice; persisted plan-revision reallocation and the daily attention schedule remain open (see R3/R4 below)
- Date: 2026-11-24
- Owner: Verlintas
- Scope: V1 single-project coding
- ADR: [0234](adr/0234-run-contract-and-anytime-execution.md)
- Related: [0231 auto budget](adr/0231-auto-budget-and-adaptive-tooling.md), [0232 operator channel](adr/0232-collab-agent-inspector-and-direct-channel.md), [0233 provider reconnect](adr/0233-non-destructive-provider-reconnect.md), [V1 readiness ledger](V1_READINESS_LEDGER.md)

## 0. Why this plan exists

The live 2026-11-23 round showed four separate problems that a single token
budget cannot solve:

1. **Tool friction**: truncated read hashes vs full write hashes, missing
   Python `unittest` detection, unnecessary `cd` wrappers, scratch writes
   classified outside the task contract, and read-only git commands classified
   as host-state.
2. **Attention friction**: repeated approvals consumed operator attention and
   wall-clock time even when the work was cheap.
3. **Wrong resource model**: a 2M raw-token fixed budget failed a run whose
   input was 91% cache reads and whose actual cost was small; verification and
   merge were starved before new tasks were stopped.
4. **Bad stop/recovery path**: an `EFFECT_UNKNOWN` integration block was correct,
   but the Leader started new recovery work and exhausted the budget instead of
   preserving the verified commit and asking for reconciliation.

This plan defines the full governance model and the staged implementation. The
governor is deterministic, event-backed, and subordinate to every existing
safety/correctness gate.

## 1. Definitions

### 1.1 Run Contract

A bounded agreement for one session:

- **Goal and deliverables**: what the session must produce.
- **Acceptance evidence**: tests, checks, Verifier verdict, diff review.
- **Quality policy**: tests, docs, reviewability, temporary-artifact policy.
- **Risk ceiling**: allowed risk classes and approval requirements.
- **Autonomy**: what may proceed without interruption.
- **Resource envelope**: money, time, context, capacity, attention.
- **Completion reserve**: resources that only verification/merge/landing may
  spend.
- **Stop/escalation policy**: evidence threshold, marginal-value threshold,
  no-progress window, deadline, attention cap.
- **Profile**: `quality`, `balanced`, `fast`, `cheap`, `unattended`.

The contract is derived from the session goal, TeamPlan, project policies,
approval mode, and auto-budget policy. Operator overrides are bounded and
event-backed. The contract cannot widen approvals, sandbox, write scope,
capacity, Verifier, Integrator, or model routing.

### 1.2 Resource vector

Not a single scalar. Each dimension has used/remaining, a soft target, an
optional hard cap, and a p50/p90 forecast:

| Dimension | Unit | Source |
|---|---|---|
| Money | USD | usage ledger + configured/derived prices |
| Raw tokens | tokens | usage ledger |
| Weighted tokens | token-equivalents | cache-aware fallback |
| Time | seconds | run journal + queue/approval waits |
| Attention | interrupts | approvals, questions, manual actions |
| Risk | risk units | approval risk classes, effect unknowns, denied actions |
| Context | ratio / tokens | context usage, compaction |
| Capacity | slots | session/worker/labour/GUI occupancy |
| Quality | bounded score + evidence | tests, Verifier, diff/reviewability |
| Progress | evidence events / no-progress turns | run journal, commit/test/plan/verifier |

### 1.3 Readiness (anytime delivery)

A bounded projection of how shippable the session is:

- `none`: no changes or commit.
- `dirty`: uncommitted changes.
- `committed`: clean task/session commit.
- `tests_passed`: latest post-commit test evidence passed.
- `verified`: Verifier approved the exact commit.
- `merged`: deterministic Integrator merged it.
- `partial`: explicitly labeled partial delivery with remaining work.
- `blocked`: a hard risk/authority/effect/approval blocker.

`shippable` means at least `committed` + passing test evidence (or an explicitly
labeled partial with a known caveat). `mergeable` additionally requires the
Verifier and no unresolved effect/merge blocker.

### 1.4 Governor decision

One of: `continue`, `throttle`, `reallocate`, `rescue`, `pause_for_operator`,
`stop_task`, `replan`, `finish`, `partial_deliver`. Every state-changing
decision is an event with a reason, forecast, confidence, resource snapshot,
and available options. Read-only snapshots do not append events.

## 2. Principles

1. **Safety and correctness are hard constraints.** No resource trade can
   bypass approvals, sandbox, contracts, write scope, Verifier, Integrator,
   capacity, or effect reconciliation.
2. **Trustworthy completion is the objective.** Minimize operator regret, not
   just spend.
3. **Operator attention is a first-class resource.** Every interrupt has a cost.
4. **Cost is one dimension.** It is not the default primary axis.
5. **Anytime delivery.** Keep a shippable state; a stop must preserve work.
6. **Evidence-driven stopping.** Stop when evidence is sufficient or marginal
   value is exhausted, not only when a counter hits zero.
7. **Deterministic controller.** No LLM governor, no hidden model routing.
8. **Bounded and auditable.** All new views are capped, redacted, event/ledger
   backed, and replayable where they affect decisions.
9. **Same-host operational state.** Not distributed scheduling or accounting.
10. **Additive compatibility.** Protocol v1, existing budgets/approvals/merge
    semantics remain valid.

## 3. Resource math

### 3.1 Tokens and cost

Provider usage distinguishes:

- `input_tokens` (total input, including cache read/write subsets),
- `cache_read_tokens`,
- `cache_write_tokens`,
- `output_tokens`.

Define:

```text
uncached_input = max(0, input_tokens - cache_read_tokens - cache_write_tokens)
raw_tokens = input_tokens + output_tokens
weighted_tokens = uncached_input
                + 0.10 * cache_read_tokens
                + 1.25 * cache_write_tokens
                + output_weight * output_tokens
```

`output_weight` defaults to the configured output price divided by the input
price; if prices are missing, use 1.0 and label the source as heuristic.

```text
cost_usd = uncached_input * input_price
         + cache_read_tokens * cache_read_price
         + cache_write_tokens * cache_write_price
         + output_tokens * output_price
```

Prices come from per-role model configuration. If a price is missing:

- do not silently report `$0.00` as if it were measured;
- show `pricing_source = unconfigured` and use weighted tokens as the primary
  economic dimension;
- never claim savings or a hard USD cap is enforced without configured prices.

### 3.2 Attention units

Each interrupt has a base weight by risk:

| Interrupt | Weight |
|---|---:|
| approval `workspace_write` | 1 |
| approval `external` | 2 |
| approval `host_state` | 3 |
| approval `destructive` | 5 |
| approval `mcp` / sensitive GUI | 3 |
| typed question / manual decision | 2 |
| denied operation requiring rework | 2 |

Read-only operations, workspace writes inside the task contract, and writes to
the per-agent runtime scratch are not interrupts.

### 3.3 Risk units

Risk units are derived from approval/effect events:

- each host-state approval: 3;
- destructive: 5;
- external/remote/GUI: 2;
- `EFFECT_UNKNOWN`: 5;
- unresolved gate/approval: 2;
- denied destructive action: 1 (the denial protected the run).

Risk is a hard ceiling input for the contract, not a currency that can be
purchased.

### 3.4 Progress and quality

Progress evidence is a new commit, passing test result, Verifier verdict, plan
revision, or a new artifact/evidence row. Repeated reads, repeated shells, and
unchanged errors do not count.

No-progress turns increment when a turn produces no progress evidence. The
existing auto-budget state field is reused.

Quality/reviewability is a bounded heuristic over:

- tests present/passed for changed behavior;
- Verifier verdict;
- changed files/lines and scope creep;
- temporary/debug artifacts (for example `.scratch-proof`);
- new dependencies;
- unresolved findings/citations.

It is reported with confidence and never replaces the Verifier.

## 4. Profiles

Profiles document a recommended posture for soft targets, reserve, attention,
and quality. The active run contract still takes its reserve, attention, and
stop values from the session override or project RunPolicy; selecting a profile
does not silently change them. Profiles never relax hard gates.

| Profile | Primary goal | Documented reserve posture | Attention | Time | Cost | Quality |
|---|---|---:|---|---|---|---|
| `quality` | trustworthy completion | 25% | high tolerance | generous | secondary | tests + Verifier + reviewability |
| `balanced` | default | 20% | moderate | moderate | moderate | tests + Verifier |
| `fast` | deadline | 15% | low tolerance | first | secondary | do not skip Verifier; skip optional checks |
| `cheap` | cost | 15% | low | secondary | first | tests + Verifier; fewer retries |
| `unattended` | no interruptions | 20% | zero (deny risky) | bounded | bounded | tests + Verifier; stop on uncertainty |

Defaults:

- new projects: `balanced`;
- owner's explicit key: may select `quality`;
- existing sessions keep their current fixed/auto behavior until they opt in;
- `unattended` denies host-state/destructive/remote/MCP/sensitive GUI instead
  of asking.

## 5. Run Contract lifecycle

1. **Derive** at session creation and plan revision from:
   - goal and TeamPlan acceptance;
   - project approval/shell/effect/test-cache/auto-budget policies;
   - session budgets and profile;
   - configured role models/prices (metadata only, no secrets).
2. **Validate** bounds:
   - reserve ≤ remaining envelope;
   - attention cap 0–1000;
   - no widening of approval mode/risk classes;
   - no write-scope/contract expansion;
   - no capacity/model-routing change;
   - profile-known values only.
3. **Persist** the effective contract and overrides. The first implementation
   derives it on demand; Phase R2 persists overrides in a session-scoped table
   with an event `session.run_contract.updated`.
4. **Re-derive** on plan revision, budget change, profile change, or operator
   override. Log why the effective contract changed.
5. **Never let an Agent set** the contract, budget, approval mode, risk ceiling,
   or profile. These are operator/project surfaces only.

## 6. Governor algorithm

### 6.1 Pre-dispatch feasibility

Before dispatching a plan batch:

1. Compute remaining envelope and reserve.
2. Estimate the batch and remaining DAG at p50/p90 from historical actuals.
3. If p90 completion + reserve > remaining, do not start the batch:
   - recommend reducing concurrency/scope, changing profile, or topping up;
   - if the operator is present, pause with options;
   - never start work that cannot be verified/finished.
4. If feasible, allocate per-task envelopes and a shared completion reserve.

### 6.2 Turn/task boundary observation

At safe boundaries:

1. Update actual resource use.
2. Recompute progress/no-progress and readiness.
3. Identify the bottleneck:
   - money/quota, time, attention, risk, context, capacity, quality, progress,
     or feasibility.
4. Evaluate hard constraints first (risk/authority, acceptance evidence).
5. Evaluate stop conditions (section 7).
6. Choose a decision (continue/throttle/reallocate/rescue/pause/stop/replan).
7. Emit a decision event only if the action changes.

### 6.3 Actions

- **Continue**: no bottleneck or all within soft targets.
- **Throttle**: lower output limits/tool scope or disable optional exploration;
  never lower the required Verifier/merge checks.
- **Reallocate**: reduce exploration/low-value loop envelopes; protect
  near-completion work and the completion reserve.
- **Rescue**: release reserve for a task with a clean commit/Verifier/merge
  pending; never for a looping task.
- **Pause**: stop new provider calls at a safe boundary; keep worktree/commit;
  show forecast and options (`top_up`, `reduce_scope`, `change_profile`,
  `reconcile_effect`, `change_role_model`, `stop`).
- **Stop task**: repeated identical call/error, no progress, risk/authority
  violation, or task envelope exhausted; preserve evidence.
- **Replan**: infeasible plan, repeated decomposition failure, or diagnosis
  showing a plan/spec problem; route through the existing plan revision path.
- **Finish/partial**: acceptance evidence sufficient, or deliver the best
  partial state with remaining work and caveats.

### 6.4 Forecast

Use historical actuals by `(project, task kind, role, model)` with sample size.
Fallback order: session actuals → project actuals → global prior → plan budget.
Report p50/p90, sample count, and confidence. Never present a point estimate as
certainty.

## 7. Stop policy

Stop or pause when any of:

1. acceptance evidence is sufficient and confidence ≥ contract threshold;
2. marginal expected value < marginal resource cost;
3. hard risk/authority/deadline cap reached;
4. no progress for the configured window;
5. attention budget exhausted;
6. plan infeasible / requires replan;
7. operator pause/cancel;
8. hard economic cap reached (last resort; preserve work).

A hard cap must not delete/abandon a clean commit or verified branch. The
session may be `paused`/`partial` rather than `failed` when work is recoverable.

## 8. Anytime delivery

### 8.1 Readiness levels

Compute from bounded events/projections, newest first:

- latest task/session commit and worktree cleanliness;
- latest post-commit test result (`run_project_task`, test cache evidence);
- latest Verifier verdict for that commit;
- latest merge queue/Integrator result;
- unresolved effects/approvals/gates.

### 8.2 Stop behavior

On pause/stop/budget exhaustion:

1. Do not start new tasks/providers.
2. Let the active tool call finish or cancel at a safe boundary.
3. Commit eligible changes through the normal Worker/Labour path; never fake a
   commit.
4. Keep the Verifier/Integrator available for already-produced commits if the
   reserve permits.
5. Report readiness, remaining work, blockers, and options.
6. If no clean commit exists, label `partial` and list uncommitted paths; do
   not claim completion.

### 8.3 Merge safety

Anytime delivery does not bypass the Verifier, effect reconciliation, or the
deterministic Integrator. A partial delivery is a candidate, not a merge.

## 9. Attention and trust

- Count every interrupt; exclude read-only/scratch/contract-allowed operations.
- Batch/coalesce approvals where the existing attention policy permits.
- If a task exceeds its attention cap or repeats the same approval class, pause
  with a policy decision (grant, template, adjust profile, stop).
- Risk-based autonomy: `smart`/`strict`/`unattended` remain authoritative.
- Trust calibration is a recommendation only: per-role/model/task history may
  suggest more verification/autonomy, but hard gates remain.
- No model is switched silently; role models are user-configured.

## 10. Quality and reviewability

- Include tests, Verifier evidence, diff scope, changed files/lines, new
  dependencies, temporary artifacts, and unresolved findings.
- Report a bounded score + evidence, not a universal quality truth.
- The governor may spend more for a clean/tested result; it may not waive the
  Verifier/Integrator to save resources.
- Temporary/debug artifacts are a reviewability penalty and should be cleaned
  or explicitly labeled.

## 11. Metareasoning and re-planning

Diagnosis taxonomy when a task stalls/exceeds forecast:

- unclear spec/acceptance;
- bad decomposition/plan;
- missing project profile/tool;
- tool/contract/sandbox mismatch;
- model capability/tool discipline;
- approval/attention friction;
- environment/provider failure;
- genuine implementation difficulty.

Interventions: clarify, split/replan, add Scout, change configured role model,
fix project profile/tool policy, adjust approval policy, pause for operator,
stop. Do not default to "spend more".

Portfolio options before a high-uncertainty run:

- direct implementation;
- Scout-first discovery;
- decompose into smaller tasks;
- stronger Verifier/model for high-risk work;
- narrower scope.

The operator configures allowed profiles/models; no hidden routing.

## 12. Learning

- Persist actual resource vectors per task/role/model in the usage ledger and a
  bounded resource history projection.
- Compute p50/p90 and no-progress/error priors per project/task kind/role/model.
- Use sample size/confidence; fall back safely.
- Post-run calibration: predicted vs actual, cause of stop, operator
  interventions, readiness achieved.
- No online policy that changes safety/approval/merge behavior.

## 13. Surfaces

### 13.1 RPC (additive protocol v1)

Phase R1 (read-only):

- `session.resources { session_id }` → `ResourceSnapshot`.
- `session.readiness { session_id }` → `ReadinessSnapshot`.
- `session.run_contract.get { session_id }` → derived `RunContract`.

Phase R2a (operator mutations implemented):

- `session.run_contract.set { session_id, profile, completion_reserve_ratio?,
  attention_cap?, stop_policy?, updated_by? }` → updated `RunContractView`.
- `session.resources.top_up { session_id, add_tokens?, add_cost_usd?,
  add_time_secs?, resume, reason? }` → `TopUpSessionResourcesResult`.
  Fixed sessions set a clamped effective budget; Auto sessions use the atomic
  auto-extension path. Caps are never removed and unlimited is never selected.
- A pre-dispatch governor decision (`continue`, `warn`, `defer`) is appended as
  `session.resource.decision` only when it changes; it blocks new plan work but
  does not pause/rebalance an active run or bypass verification.

Phase R2b (implemented core):

- `project.run_policy.get/set` (persisted SQLite + manifest/rebuild).
- Lean true turn-boundary pause/wait (no readiness/git status on the run path),
  Auto Budget token request-fit for every role plus cost/time fit for children.
- macOS top-up/readiness, project policy editor, event-derived pause banner,
  a bounded attention batch prompt with Inbox/Policy actions, an explicit
  partial-delivery record sheet, and a rescue-reserve confirmation surface.
- Readiness snapshot cache for the resource card: 2-second TTL, keyed by the
  newest session event sequence, bounded to 64 sessions, and bypassed by
  `session.readiness`/state-changing operations. The lean turn path remains
  uncached and does not call readiness.
- No R2b items remain. Automatic task-envelope mutation is deliberately in R3,
  where it belongs with per-task envelope allocation; R2b provides the rescue
  action and advisory allocation preview.

### 13.2 CLI

Implemented:

- `vp resources <session> [--json]`
- `vp readiness <session> [--json]`
- `vp run-contract <session> [--json]`
- `vp run-contract-set <session> --profile ... [--reserve ...] [--attention-cap ...] [--no-progress-turns ...]`
- `vp top-up <session> [--tokens N] [--cost N] [--time N] [--resume]`
- `vp project-run-policy <project>`
- `vp project-run-policy-set <project> [--profile ...] [--reserve ...] [--attention-cap ...] [--no-progress-turns ...]`

### 13.3 macOS

- Stage/Manage Dash card: readiness, bottleneck, money/time/attention/risk,
  forecast, reserve, options.
- No fake precision; show sample size/pricing source.
- The card is read-only except the profile menu in R2a; top-up/pause actions
  remain CLI/Stage budget surfaces until R2b.

## 14. Persistence, events, replay

- R1 snapshots are derived, bounded, and not events.
- R2a contract overrides are **event-only**: the newest
  `session.run_contract.updated` payload is the source, so replay/import carry
  it without a new projection table. An imported session rejects `set`.
- Governor decisions: `session.resource.decision` event with reason/forecast/
  states; bounded payload; only non-`continue` actions are appended.
- Readiness level changes: `session.readiness.changed` only on level changes,
  not every refresh.
- Resource history: bounded rows/aggregates; no prompt/tool content/secrets.
- Imported sessions are read-only; no governor actions, no contract mutation.

## 15. Security and invariants

- No agent tool can set budgets/contracts/profiles/approval modes/risk ceilings.
- The governor cannot approve, resolve gates, submit verdicts, merge, change
  capacity, or route models.
- No secrets/prompts/raw tool output in snapshots.
- Same-host SQLite/event state only; not distributed scheduling/accounting.
- Existing contract/sandbox/write scope/Verifier/Integrator remain mandatory.
- `unlimited` still bypasses economic gates only; risk/attention/quality gates
  may still pause/deny.

## 16. Phases and acceptance

### P0 — Tool friction (implemented)

Deliver:

1. `read_files` text shows exact/full hash (or a stable edit token); write/edit
   errors include current full hash and an actionable hint.
2. Python `unittest` project detection for bare `tests/test_*.py`.
3. Shell env/hint: `VERLINPARK_WORKTREE`, `VERLINPARK_SCRATCH`; recipe says cwd
   is the worktree and not to `cd` to the git root.
4. Contract allows the per-agent runtime scratch and classifies read-only git
   commands as read-only.
5. `EFFECT_UNKNOWN` integration failure marks the task blocked/operator-action
   instead of auto-retrying new work; reconcile path remains explicit.
6. Duplicate/no-progress guard: repeated identical failed calls/errors are
   surfaced/bounded; no-progress turns feed the auto-budget state.

Acceptance: unit/integration tests for each; a focused fake/live scenario
shows no repeated hash/scratch/read-only approval loop; existing budgets,
approvals, Verifier, and merge tests unchanged.

### R1 — Observe (implemented)

Deliver:

- `ResourceSnapshot` + `ReadinessSnapshot` + `RunContract` derivation.
- `session.resources`, `session.readiness`, `session.run_contract.get`.
- `vp resources`, `vp readiness`, `vp run-contract`.
- Swift models/card (read-only).
- Tests for math, bounds, redaction, empty/partial states, replay/no events.

Acceptance: a real/fake session shows money/weighted tokens/time/attention/
risk/context/capacity/quality/progress/readiness; no behavior change; no
secrets; no unbounded data.

### R2a — Govern (implemented)

- Event-backed profile/contract override (`session.run_contract.updated`).
- Completion reserve and attention/risk pre-dispatch deferral.
- `session.resources.top_up` for fixed/auto budgets, clamped to caps.
- CLI `run-contract-set` / `top-up`; macOS profile menu.
- `session.resource.decision` for defer/warn decisions; no hidden gate bypass.

Acceptance: override validation/event replay, governor attention/reserve tests,
fixed top-up clamping, daemon RPC tests, Swift profile tests.

### R2b — Govern (implemented)

Implemented:

- True turn-boundary pause/wait (`session.resource.paused`) for Leader,
  Manager, Worker, Labour, Scout; Verifier and trusted internal repair runs are
  exempt so already-produced work can still be verified/integrated.
- Lean turn-boundary decision: attention/risk, money/token/time caps, context,
  and completion reserve only. The turn path no longer runs
  `session_readiness` (`git status`), task queue, merge queue, or the agent
  roster; dispatch and turn share the same action ladder.
- Auto Budget request-fit for every role (tokens) plus cost/time fit for child
  runs; child runs never own cumulative turns/no-progress state and pause when
  a dimension cannot be extended or is at its ceiling.
- macOS Top Up sheet, capped/resumed truth, readiness caveats, bounded report
  copy, project RunPolicy editor, and a resource-pause banner derived only
  from real `session.resource.paused` / resume events. First selection now
  loads the full session state (budget/resources/events/plan/diff/...), and a
  resource RPC failure shows an explicit unavailable/Retry card instead of
  silently hiding the strip.
- Project-level Run Contract policy: persisted SQLite policy, manifest/rebuild,
  `project.run_policy.get/set`, CLI `project-run-policy`/`project-run-policy-set`,
  project defaults with session override precedence.
- Attention batch prompt: a turn-boundary pause carries a bounded
  `attention_batch` (pending count, weighted interrupts/cap, risk groups, and up
  to 8 approval metadata rows with tool/risk/task/priority/waiters/reason). Raw
  tool input, command text, and file contents are excluded. The Stage pause
  banner shows the batch and opens the existing approval Inbox or Policies
  surface; the existing approval queue, dedup/coalescing, and batch-resolve path
  remain the only decision path. No grant, template, approval, or profile is
  applied automatically.
- Explicit partial delivery: `session.partial_delivery` is an event-only
  operator record with bounded summary, remaining work, readiness level,
  commit, uncommitted paths, and blockers; it is exposed through
  `session.delivery` / `session.delivery.partial`, CLI `delivery` /
  `partial-delivery`, and a Stage record row/sheet. `--yes` / `confirm=true` is
  required; running/queued/imported/empty/already-complete sessions are refused
  with typed `DELIVERY_*` codes. The record never commits, merges, approves,
  completes, resumes, changes budgets/capacity, or bypasses the Verifier/
  Integrator; it survives export/import and a newer record wins.
- Rescue reserve + allocation preview: `session.rescue.status` reports
  daemon-validated eligibility, reserve headroom, near-completion and looping
  candidates, and bounded recommendations. `session.rescue` records a
  short-lived (30 minute) event-only grant for a fixed-budget, clean
  near-completion session with `no_progress_turns == 0`; running/queued/
  imported/dirty/blocked/looping/auto/unlimited/no-headroom sessions are refused
  with typed `RESCUE_*` codes. At the next non-pausing turn boundary the
  governor releases only the completion-reserve ratio (hard caps, attention,
  context, approvals, Verifier, and Integrator remain authoritative) and appends
  one `session.resource.rescue.used` receipt. CLI `rescue-status`/`rescue` and
  the Stage confirmation sheet surface it. The allocation preview's
  reallocation recommendations are advisory only; no task envelope is mutated
  automatically.
- Readiness-blocked remains a dispatch gate, not a turn pause (avoids
  deadlocking approval/effect flows in an active run).

Remaining:

- None in R2b. R3a shows per-task envelopes and R3b adds opt-in event-backed
  runtime reallocation from completed tasks; persisted plan-revision
  reallocation remains open (see below).

### R3 — Allocate (preview + runtime reallocation implemented; persisted reallocation open)

Implemented (R3a):

- `session.allocation` / `vp allocation` is a read-only preview over the
  session envelope, stored TeamPlan DAG and critical path, task queue, bounded
  project usage history (latest 1,000 ledger rows), readiness, approvals,
  effects, failure diagnosis, and stall observations.
- Per-task estimates use deterministic precedence: session actuals for the
  task, the plan task envelope, project department actuals (nearest-rank
  p50/p90 across tasks), the department envelope, then an explicit unknown
  prior. Unknown is `None`, not zero.
- Feasibility focuses on the remaining critical path when available and
  otherwise on all remaining tasks. It reports p50/p90 tokens/cost, the
  completion reserve, headroom, `estimated_tasks`/`total_tasks`, and
  `pressure_dimensions` (`tokens`, `money`, `time`). A partly estimated path
  fails closed as `partial_estimate`, not as a green result; a path with no
  estimate is `insufficient_estimate`; critical-path computation failure
  reports `critical_path_available=false` plus a bounded reason instead of
  silently pretending the all-task fallback is the critical path. Other reasons
  include `remaining_below_completion_reserve`,
  `p90_exceeds_reserve_headroom`, and `cost_unconfigured`. Per-task feasibility
  checks token and cost estimates against the task budget and session remaining,
  so a cost-only task is not reported as unestimated and an undersized task
  budget is visible. Task rows also report the runtime effective budget and a
  reallocation flag when an R3b event applies; the stored budget remains visible.
- Recommendations, metareasoning (diagnosis target, no-progress turns,
  stalled/failed/retrying tasks), and attention/trust calibration are bounded
  and advisory. The preview appends no event and changes no task envelope,
  budget, model, route, approval, effect, Verifier, or merge state.
- `ProjectRunPolicy.auto_rescue` (default off) is the automatic reserve action:
  at a turn boundary that pauses exactly on `completion_reserve_*`, an eligible
  fixed-budget, near-completion session with a latest commit, no no-progress
  turns, headroom, and no prior rescue grant may receive one automatic grant.
  Manual/automatic grants and consumption share one in-process lock; a manual
  grant consumes the one-shot allowance. The grant releases only the completion
  reserve for its normal TTL and never raises a hard cap.
- R3b `ProjectRunPolicy.auto_reallocate_envelopes` (default off) lets a
  Worker/Labour run use a runtime-only effective task budget built from unused
  token/cost capacity of already-completed tasks in the same department.
  Sources are consumed independently per token/cost dimension; only the
  capacity that actually fits under the department cap is taken and recorded;
  a target's prior event replays idempotently; a completed target can become a
  source using its effective budget. A per-target plan fingerprint over the target task and its
  department budgets invalidates old overrides on a target/department budget
  revision instead of widening the new plan; unrelated task edits do not. The stored TeamPlan and department/session/project caps are
  never changed, and the runtime plan is used only for task admission/budget
  scope. Every task-scoped role sees the recorded read-only overlay, but
  Verifier/Scout are intentionally **not** bound by the implementation task
  envelope: they keep department/session/project caps and the completion
  reserve so a Worker that spent its task cap cannot make the task impossible
  to verify. Only normal Worker/Labour runs (not auto-approve maintenance) may
  append a new event. The allocation preview reports stored and effective
  budgets and uses the effective values for estimates/critical-path weight.
- CLI `--auto-rescue` / `--no-auto-rescue`,
  `--auto-reallocate-envelopes` / `--no-auto-reallocate-envelopes`, the macOS
  policy toggles, and the Manage Dash allocation/trade-off cards use only these
  daemon surfaces.

Remaining:

- Persisted plan-revision reallocation. R3b is an event-derived runtime overlay,
  not a TeamPlan rewrite; cross-department/session reallocation, reservation
  redistribution, and a revisioned plan budget remain open. Its 1,000-event
  source-single-use window can forget very old sources after heavy plan
  revision, though hard caps still bound actual spend. Serialization is
  per-session and same-daemon, not cross-host. If a target/department revision
  invalidates an overlay and observed spend exceeds the revised stored cap, a
  Worker retry needs a top-up or plan edit; Verifier/Scout are not task-cap
  bound.
- Learning from actuals beyond deterministic project/department nearest-rank
  p50/p90; no model training or hidden routing.
- LLM replanning; replan recommendations remain textual/advisory.

### R4 — Deliver (preview implemented; scheduling open)

Implemented (R4a):

- `session.tradeoffs` / `vp tradeoffs` returns the five documented profile
  postures (quality/balanced/fast/cheap/unattended), the current profile, the
  active contract reserve and its source, the session's existing turn/cost
  forecast repeated without per-profile guarantees, a bounded
  quality/reviewability review, and an explicitly heuristic calibrated
  confidence score. Profile reserve values are labeled posture; they do not
  silently change the active contract reserve. Confidence gives test credit
  only for a recorded pass, penalizes a failing test, and gives merge credit
  only for a `merged` queue entry.
- Quality review reports readiness, latest commit/test/Verifier/merge evidence,
  completed/total tasks, merged entries, changed paths, blockers, and
  `ok|watch|risk` reviewability. Test evidence comes from the test cache and is
  shown with a pass/fail flag; a queued/conflicting merge entry is not labeled
  merged and does not add merge confidence. It never replaces the Verifier.
- A different profile is marked recommended only from deterministic evidence:
  `quality` for risk/blockers/blocked effects, `cost` under completion-reserve
  pressure. The current profile is not double-badged, and the view never
  switches it.
- Manage Dash → Usage & Budget has read-only Allocation insight and Trade-offs &
  quality cards; both reject a response for a different session and neither is
  loaded by the selection sweep.

Remaining:

- Daily attention schedule / quiet-window batching beyond the existing
  attention batch prompt; no cron or operator-calendar integration.
- Anytime UI beyond the advisory cards; the existing pause/partial-delivery
  flow remains the execution surface.
- Learned confidence calibration; the current score is a documented heuristic.

## 17. Test matrix

| Area | Tests |
|---|---|
| Resource math | cache weighting, missing prices, output weight, clamping, p50/p90 |
| Allocation (R3a/R3b) | estimate precedence, partial/insufficient estimates, critical-path focus/availability, token/cost/time pressure, task-budget and cost-only feasibility, no-event preview, serialized one-shot auto rescue, runtime envelope reallocation (single-use sources, idempotent replay, department caps, plan-revision invalidation) |
| Trade-offs (R4a) | five profiles, active reserve vs posture, test-cache pass/fail, queued/merged distinction, confidence heuristic, deterministic recommended profile, no profile switch |
| Contract | derive/validate/profile bounds/reserve/risk/attention/no widening |
| Governor | pre-dispatch infeasibility, continue/throttle/reallocate/rescue/pause/stop/replan, no hidden routing |
| Readiness | none/dirty/committed/tests/verified/merged/partial/blocked, stale evidence, unresolved effect |
| Attention | read-only/scratch excluded, risk weights, cap/pause, batching |
| Risk | host-state/destructive/effect unknown counted, never traded away |
| Anytime | budget stop preserves commit, partial labels, no fake completion, merge safety |
| Persistence | contract/decision replay/repair/bundle/db-rebuild, imported read-only |
| RPC/CLI/Swift | decode/encode, empty/partial, truthful labels, no secrets |
| Regression | fixed/auto/unlimited budgets, approvals, Verifier, Integrator, worktrees |

## 18. Migration and compatibility

- Protocol additions are optional fields/RPCs; older clients ignore them.
- Existing sessions: no contract override; derived profile `balanced` unless an
  explicit auto policy/budget says otherwise.
- Existing fixed/auto/unlimited budgets keep their current hard semantics in
  R1/R2a. R2a top-up never removes a cap or selects unlimited.
- R2a adds no new SQLite table: contract overrides and resource decisions are
  event-only and bounded; bundle/import preserves them as bounded evidence;
  imported sessions cannot mutate.
- No secrets in the new events.

## 19. Non-goals

- Distributed/cross-host scheduling or accounting.
- An LLM budget manager or hidden model routing.
- Automatic approval/Verifier/merge bypass.
- Universal cost prediction or guaranteed completion.
- Exact token counting across providers; cache semantics remain provider-owned.
- A universal quality score or training a model on user code.
- Replacing the existing auto-budget subsystem; this builds on it.

## 20. Immediate implementation slice

Implemented **P0 + R1 + R2a + R2b core + R3a/R3b/R4a**:

- tool friction fixes with tests;
- read-only resource/readiness/contract RPCs and CLI;
- Swift resource card/models, profile menu, Top Up sheet, readiness caveats and
  bounded report copy;
- event-backed profile/contract override and bounded top-up;
- pre-dispatch deferral on attention/risk/completion reserve;
- lean turn-boundary resource pause/resume and Auto Budget token/cost/time
  request-fit (Leader owns turns/no-progress; children only extend);
- project-level run policy plus macOS project-policy editor and event-derived
  pause banner;
- bounded attention batch prompt with approval Inbox and policy actions;
- explicit partial-delivery record with RPC/CLI/Stage UI;
- rescue reserve grant + read-only allocation preview with RPC/CLI/Stage UI;
- readiness snapshot cache for explicit resource views;
- R3a read-only allocation preview with bounded per-task envelopes,
  critical-path feasibility, project/department p50/p90, diagnosis and
  metareasoning, attention/trust calibration, RPC/CLI, and macOS card;
- opt-in one-shot automatic completion-reserve rescue
  (`ProjectRunPolicy.auto_rescue`, CLI/GUI, serialized turn-boundary path);
- R3b opt-in runtime per-task envelope reallocation
  (`ProjectRunPolicy.auto_reallocate_envelopes`, CLI/GUI, event-backed
  single-use completed-task capacity, department/session/project caps
  unchanged, plan-revision invalidation);
- R4a read-only trade-off frontier, quality/reviewability review, calibrated
  confidence, deterministic advisory profile recommendation, RPC/CLI, and
  macOS card;
- ledger/docs updated with the multi-resource model.

Next: persisted plan-revision reallocation with reservation semantics, then the
R4 daily attention schedule.

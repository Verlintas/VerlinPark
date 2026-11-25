# 0234 — Run contract and anytime execution

- Status: Accepted
- Date: 2026-11-24
- Scope: multi-resource run governance, completion-aware stopping, operator attention, risk/quality policy, anytime delivery, profiles, resource observability, learning
- Depends on: ADR-0003 (manual model/budget), ADR-0018 (session budget/cancel), ADR-0035/0036 (shell sandbox/policy), ADR-0047 (budget reservations), ADR-0071 (provider idle timeout), ADR-0193 (task contract/intent), ADR-0194 (effect receipts), ADR-0195 (external gates), ADR-0197 (reliability/OTel), ADR-0231 (auto budget/adaptive tooling), ADR-0232 (operator channel), ADR-0233 (provider reconnect)
- Plan: [RUN_CONTRACT_AND_RESOURCE_GOVERNOR_PLAN.md](../RUN_CONTRACT_AND_RESOURCE_GOVERNOR_PLAN.md)

## Context

The 2026-11-23 live DeepSeek/GUI round exposed a mismatch between how
VerlinPark measures a run and what makes a run succeed:

- A 3-task TeamPlan exhausted its 2M-token fixed budget before verification,
  even though 91% of its input was cache reads and the actual provider cost was
  far below what the raw token count implied.
- A 1-task run completed Worker → Labour → Verifier → merge queue → Integrator,
  but only after repeated tool friction: truncated read hashes vs full expected
  hashes, a missing Python `unittest` profile, unnecessary `cd` wrappers, and
  contract approvals for scratch verification inside the worktree.
- A second 1-task run was blocked by an `EFFECT_UNKNOWN` receipt from a raw
  shell commit. The block was correct fail-closed behavior, but the recovery
  path let the Leader start new work and exhaust the remaining budget instead
  of preserving the verified commit and asking the operator to reconcile.
- The most expensive resource in the failed runs was not money: it was operator
  attention (repeated host-state approvals), wall-clock time, and the absence
  of new evidence. A scalar token budget could not see any of those.

The existing auto-budget machinery already has fixed/auto/unlimited modes,
hard caps, forecasts, extension, max turns, no-progress pause/escalate, and
pre-flight reservations. It is a useful foundation, but it is still primarily a
token/cost counter. It does not model a run as a contract with deliverables,
acceptance evidence, risk ceiling, attention budget, quality target, and a
completion reserve. It also cannot distinguish "cheap but stuck" from
"expensive but about to finish".

V1 therefore needs a governance layer above the budget counter. It must remain
deterministic, event-backed, auditable, and subordinate to the existing safety
and correctness gates.

## Decision

### 1. A run is governed by a Run Contract

Every session has a derived Run Contract with bounded, validated fields:

- goal and deliverables;
- acceptance evidence (tests, checks, Verifier verdict, diff review);
- quality policy (tests, docs, reviewability, no temporary artifacts);
- risk ceiling (host-state, destructive, external writes, GUI, remote);
- autonomy level (what may proceed without operator interruption);
- resource envelope (money, time, context, capacity, attention);
- stop/escalation policy (evidence threshold, marginal-value threshold,
  no-progress limit, deadline, attention cap);
- profile (`quality`, `balanced`, `fast`, `cheap`, `unattended`).

The contract is derived from the session goal, TeamPlan, project policies,
approval mode, and auto-budget policy. An operator may override a bounded set of
fields; every override is event-backed. The contract never widens approvals,
sandbox, write scope, Verifier/Integrator, capacity, or model routing.

### 2. Budget is one constraint, not the objective

The governor tracks a resource vector, not a single scalar:

| Dimension | Examples |
|---|---|
| Money | provider cost, cache-read/write cost, configured prices |
| Time | wall clock, provider latency, queue/approval wait |
| Attention | operator approvals, questions, manual interventions |
| Risk | host-state/destructive/external/GUI/remote operations, effect unknowns |
| Context | active context, compaction pressure, prompt/tool schema size |
| Capacity | Worker/Labour slots, session admission, GUI lease |
| Quality | tests, Verifier evidence, diff/reviewability, unresolved findings |
| Progress | new commit, test result, plan revision, Verifier verdict, evidence |

Hard constraints are safety, permissions, data correctness, acceptance
evidence, and the Verifier/Integrator gates. Economic and time limits are soft
targets unless the operator explicitly marks one as a hard cap. Decisions are
lexicographic: safety/correctness, then trustworthy completion, then attention
and time, then money, then stylistic preferences. The system may present a
Pareto frontier of cost/time/attention/confidence instead of collapsing them
into one number.

Cost is not the primary axis. For a personal key the main constraints may be
time, attention, risk, and quality; for a public deployment cost and rate limits
matter more. Profiles make that choice explicit.

### 3. The governor is a deterministic controller

The controller is deterministic Rust. It is not an LLM, does not route models
silently, and cannot approve anything. At task dispatch and safe turn
boundaries it observes actuals, forecasts completion, identifies the bottleneck,
and chooses one of:

- continue;
- throttle (lower output/tool scope);
- reallocate (take from a low-value loop, protect near-completion work);
- rescue (release the completion reserve for a verified/mergeable task);
- pause and ask the operator with concrete options;
- stop a task/loop;
- replan (via the existing plan-revision path);
- finish/partial-deliver.

Every state-changing decision is an event with a reason, forecast, confidence,
resource snapshot, and the options that were available. Read-only snapshots do
not append events.

### 4. Anytime delivery

The runtime must maintain a shippable state whenever possible:

1. compiles / tests pass;
2. a clean commit exists on the task branch;
3. the Verifier can evaluate it;
4. the Integrator can merge it;
5. the operator can review the diff and evidence.

If a run stops, it stops at the best available delivery level and reports the
remaining work. Budget exhaustion or a soft stop must not discard a verified
commit or leave an unrecoverable dirty worktree. A partial delivery is labeled
as partial; it never claims completion.

### 5. Stopping is evidence-driven

A run stops when one of these is true:

- acceptance evidence is sufficient and confidence is above the contract
  threshold;
- the marginal expected value of more work is below its marginal resource cost;
- a hard risk/authority/deadline cap is reached;
- no new evidence appears for the configured no-progress window;
- the operator attention budget is exhausted;
- the plan is infeasible and a replan is required;
- the operator pauses/cancels.

A hard budget cap is only one of these conditions. Hitting it stops new
provider calls; it does not invalidate work already produced.

### 6. Attention and trust are first-class resources

Operator attention is scarce. The governor counts interruptions by type and
risk, batches/coalesces where policy permits, and pauses for a policy decision
when a task repeatedly asks for the same class of approval. Safe read-only
operations, workspace writes inside scope, and the per-agent runtime scratch
must not become interrupts.

Risk-based autonomy is explicit and bounded. A role/model/task history may
inform how much verification or autonomy is recommended, but it never bypasses
the approval broker, task contract, sandbox, Verifier, or Integrator. Any model
change is a user-configured role model or an explicit recommendation, never
hidden routing.

### 7. Quality and reviewability are objectives

The contract includes a reviewability target: diff scope, changed files, new
dependencies, temporary/debug artifacts, test evidence, and explanation quality.
A cheaper run that produces an unreviewable diff is not cheaper. The governor
may spend more to produce a clean, tested, reviewable result. It cannot waive
the Verifier or merge checks to save resources.

### 8. Metareasoning before spending

When a task stalls or exceeds its forecast, the governor classifies the likely
cause before adding resources: unclear specification, bad decomposition,
missing project profile/tool, model/tool mismatch, approval/sandbox friction,
environment failure, or a genuine implementation problem. It then chooses a
targeted intervention (clarify, split, replan, use a configured role model,
fix tooling, pause for operator, stop) rather than "spend more tokens".

### 9. Observability surfaces

Phase 1 adds read-only, bounded views:

- `session.resources`: per-dimension actuals, forecasts, bottleneck, reserve,
  attention counts, risk counts, progress, and profile;
- `session.readiness`: the current shippable level, latest commit/test/Verifier/
  merge evidence, and remaining work;
- `vp resources` / `vp readiness`;
- a macOS resource/readiness card backed only by these RPCs.

These views are additive in protocol v1. They do not expose prompts, raw tool
output, secrets, or unbounded event payloads.

### 10. Allocation, runtime envelope reallocation, and trade-off surfaces (R3a/R3b/R4a)

The R3/R4 slice is read-only/advisory except for two explicit project opt-ins:

- `session.allocation` / `vp allocation` reconciles the session envelope, the
  stored TeamPlan DAG and critical path, the task queue, bounded project usage
  history, readiness, approvals/effects, failure diagnosis, and stall
  observations into bounded per-task p50/p90 estimates, a remaining-path
  feasibility result, advisory recommendations, metareasoning, and
  attention/trust calibration. Estimate precedence is session actuals, plan
  task envelope, project department actuals (nearest-rank p50/p90), department
  envelope, then an explicit unknown prior. Unknown estimates are not zero: a
  path with no estimate reports `insufficient_estimate`, a partly estimated path
  reports `partial_estimate`, and an unestimated task is not reported as
  feasible. Critical-path failure is reported instead of silently labeling the
  all-task fallback as the path; token/cost/time reserve pressure is exposed as
  bounded dimensions. When an R3b event applies, each task also reports its
  stored budget, runtime effective budget, and a reallocation flag so the
  preview cannot imply the stored plan is the runtime cap.
- `session.tradeoffs` / `vp tradeoffs` compares the five documented profile
  postures, reports the active contract reserve and its source separately from
  the documented per-profile posture, reports a bounded quality/reviewability
  review and an explicitly heuristic confidence score, and marks a different
  profile recommended only from deterministic evidence (quality for
  risk/blockers/blocked effects, cost under token/cost/time completion-reserve
  pressure). Test evidence comes from the test cache with a pass/fail flag;
  queued or conflicting merge entries are not labeled merged and do not add
  merge confidence. It never switches a profile.
- `ProjectRunPolicy.auto_rescue` (default off) permits at most one automatic
  completion-reserve grant per session at a turn boundary when the pause is
  exactly `completion_reserve_*`, the session is fixed-budget and eligible
  (near-completion readiness, latest commit, no no-progress turns, token/cost
  headroom, and no prior grant). Manual/automatic grants and consumption share
  one in-process lock; a manual grant consumes the automatic allowance, and a
  grant receipt is persisted at most once. The grant releases only the
  completion reserve for its normal TTL and never raises a hard cap or bypasses
  approvals, contracts, the sandbox, the Verifier, or the Integrator. A failure
  to persist the one-shot consumption receipt fails closed and stops the turn
  rather than leaving the reserve released.
- `ProjectRunPolicy.auto_reallocate_envelopes` (default off) lets a
  Worker/Labour run use a runtime-only effective task budget built from unused
  token/cost capacity of already-completed tasks in the same department.
  Sources are consumed independently per token/cost dimension; only capacity
  that actually fits under the department cap is taken and recorded. A target's
  prior event replays idempotently, a completed target can become a source
  using its effective budget, and a per-target plan fingerprint over the target task and its
  department budgets invalidates an old override instead of widening the new
  plan; unrelated task edits do not. The
  stored TeamPlan and every department, session, and project cap remain
  unchanged; the runtime plan is used only for task admission/budget scope.
  Every task-scoped role sees the recorded read-only overlay, but Verifier/Scout
  are intentionally not bound by the implementation task envelope; they keep
  department/session/project caps and the completion reserve, so a Worker that
  spent its task cap cannot make verification impossible. Only non-auto-approve
  Worker/Labour runs may append. A per-session same-daemon lock serializes event
  read/append. The allocation preview reports stored and effective budgets and
  uses effective values for estimates and critical-path weight.
- `session.envelope.persist` / `vp envelope-persist` is an explicit
  operator action that persists the latest recorded runtime envelope for one
  task into a normal plan revision. It requires a quiescent session, explicit
  `confirm`/`--yes`, and optionally an expected revision. It re-folds the
  recorded events through the same plan-fingerprint/old-budget validator as the
  runtime path, so a stale overlay from a prior target/department budget
  revision fails closed; it never creates capacity and is not called by the
  scheduler, controller, or a model. Automatic persisted reallocation remains
  open.
- The dispatch path evaluates both governor tiers before queue/lease events,
  deduplicates identical decision/admission records, and treats
  `Session.unlimited` as bypassing only retained economic caps (attention,
  readiness, risk, context, approvals, and verification remain). A terminal
  gate releases its plan task only when the current row is still blocked and
  the latest block event names that gate.
- Both preview RPCs append no events and mutate nothing. The macOS cards load
  only when visible or explicitly refreshed and reject a response for a
  different session.

Limits: automatic persisted plan-revision reallocation and the daily attention
schedule remain open; explicit one-task persistence does not make R3b an
automatic TeamPlan rewrite, and it remains a one-department overlay rather than
a cross-department/session budget pool. It scans the newest
1,000 reallocation events, so source single-use memory can be forgotten after
very heavy plan revision (hard department/session/project caps still bound
actual spend). Envelope serialization is per-session and same-daemon, not
cross-host. Task feasibility is a plan-envelope forecast, not a next-call
reservation. A target/department revision can invalidate an overlay; a Worker
retry then needs a top-up or plan edit if observed spend exceeds the revised
stored cap, while Verifier/Scout are not task-cap bound. Reallocation events
use schema v2 with source fingerprints; pre-v2 events are ignored and their
sources remain consumed. R3b binds sources before the first governor/approval
gate; a run denied at admission leaves the event for its retry. Historical estimates are sparse and are not vendor token counts;
confidence is a heuristic, not a calibrated probability; profile forecasts
repeat the existing session forecast and are not per-profile guarantees;
rescue serialization is same-daemon only.

## Consequences

Positive:

- fixed token caps stop being the primary cause of failed solvable runs;
- cache-heavy long-context work is no longer charged as if every token were
  full-price input;
- operator attention and risk become visible and governable;
- every stop has a reason, a forecast, and a recovery path;
- partial work is shippable and reviewable;
- the system can learn per-repo/task/role/model resource profiles.

Negative and limits:

- multi-resource governance is more complex than one scalar and needs careful
  UI/tooling to avoid decision paralysis;
- historical p50/p90 estimates are initially sparse and must be labeled;
- attention/risk/quality metrics are bounded heuristics, not objective truth;
- the governor cannot make a weak model strong; tool/prompt/model quality still
  matters;
- resource decisions are same-host operational state, not distributed
  scheduling or cost accounting;
- no guarantee that a run can finish within a contract; the contract can only
  stop earlier and explain why.

## Required tests

- resource math: cache-read/cache-write/output weighting, raw vs weighted
  tokens, effective cost with configured and missing prices;
- contract validation: profile bounds, reserve ≤ budget, attention cap, risk
  ceiling, no widening of approvals/write scope/capacity;
- stop policy: evidence-sufficient, marginal value, risk cap, deadline,
  no-progress, attention cap, feasibility/replan;
- anytime states: no commit → commit → tests → Verifier → merge, partial
  delivery, dirty worktree, budget stop preserving a verified commit;
- attention: read-only/scratch not counted, host-state counted, repeated
  approval triggers a policy pause, batching;
- governor: continue/throttle/reallocate/rescue/pause/stop/replan decisions,
  replay/repair, no hidden model routing, no approval bypass;
- RPC/CLI/Swift models and truthfulness of labels/forecasts;
- full regression: existing fixed/auto/unlimited budgets, approvals, Verifier,
  Integrator, and worktree semantics remain unchanged.

## Alternatives considered

- **Keep one token budget and tune it.** Rejected: it still charges cache reads
  at full weight, cannot see attention/risk/time, and cannot distinguish cheap
  loops from near-completion work.
- **Use USD cost only.** Rejected by the owner: cost is one dimension; personal
  use may be attention/time/quality constrained, and unconfigured prices make
  USD meaningless.
- **Let an LLM manage the budget.** Rejected: hidden routing and non-determinism;
  it would also consume the resource it is managing.
- **Remove all limits for the owner's key.** Rejected: safety/attention/risk
  constraints still matter, and an unbounded loop can burn time and trust even
  when money is free.
- **Auto-approve safe operations globally.** Rejected: the approval broker and
  contracts remain authoritative; only specific read-only/scratch classes may
  be automated with tests.
- **Spawn a new agent/run to recover every failure.** Rejected: it multiplies
  cost and obscures the original blocker; use reconcile/replan/partial delivery
  first.

## Phases

1. **P0 tool friction**: full read/edit hash handling, Python unittest profile,
   shell cwd/scratch contract, read-only git classification, effect-unknown
   blocked/reconcile path, duplicate/no-progress guard.
2. **R1 observe**: multi-resource snapshot, readiness, forecasts, profiles in
   read-only form; no behavior change.
3. **R2 govern**: R2a implements event-backed profile/contract overrides,
   bounded top-up, and pre-dispatch attention/risk/completion-reserve deferral;
   R2b core adds a lean turn-boundary pause path (no readiness/`git status` on
   the run path), Auto Budget token request-fit for every role plus cost/time
   fit for children, project RunPolicy with manifest/rebuild, and macOS
   top-up/readiness, project-policy editor, and an event-derived pause banner
   that shows a bounded attention batch with Inbox/Policy actions; first
   selection loads the full session resource state and resource failures are
   visible with an explicit retry. Explicit partial delivery is event-only
   (`session.partial_delivery`) with RPC/CLI/Stage surfaces; it never merges,
   approves, completes, or resumes. A short-lived event-only rescue grant can
   release only the completion reserve for clean near-completion fixed-budget
   work, and an allocation preview recommends (but does not enact)
   reallocation. A bounded 2-second readiness snapshot cache serves explicit
   resource views, keyed by the newest event sequence; the turn governor never
   reads it. Automatic task-envelope reallocation is R3.
4. **R3 allocate**: R3a implements the read-only task/plan-aware allocation
   preview, critical-path feasibility, project/department p50/p90,
   diagnosis/metareasoning, and the opt-in one-shot automatic
   completion-reserve rescue. R3b adds opt-in, event-backed, runtime-only
   per-task envelope reallocation from completed same-department tasks, plus an
   explicit operator-confirmed persistence of one validated overlay into a plan
   revision. Automatic persisted reallocation, cross-department/session pooling,
   reservation-aware redistribution, and learned estimates remain open.
5. **R4 deliver**: R4a implements the trade-off frontier, quality/reviewability
   review, calibrated-confidence heuristic, deterministic advisory profile
   recommendation, and macOS cards. The daily attention schedule and a broader
   anytime UI remain open.

The detailed scope, formulas, surfaces, migration, and acceptance criteria are
in [RUN_CONTRACT_AND_RESOURCE_GOVERNOR_PLAN.md](../RUN_CONTRACT_AND_RESOURCE_GOVERNOR_PLAN.md).

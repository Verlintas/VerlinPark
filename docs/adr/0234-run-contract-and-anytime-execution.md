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
   R2b core adds true turn-boundary pause/wait, Auto Budget request-fit for
   Worker/Labour, and macOS top-up/readiness. Attention batching, project run
   policy, explicit partial-delivery action, and rescue/reallocation remain.
4. **R3 allocate**: task/plan-aware allocation, rescue/reallocation, learning
   from actuals, metareasoning/replan.
5. **R4 deliver**: anytime UI, trade-off frontier, calibrated trust, daily
   attention scheduling.

The detailed scope, formulas, surfaces, migration, and acceptance criteria are
in [RUN_CONTRACT_AND_RESOURCE_GOVERNOR_PLAN.md](../RUN_CONTRACT_AND_RESOURCE_GOVERNOR_PLAN.md).

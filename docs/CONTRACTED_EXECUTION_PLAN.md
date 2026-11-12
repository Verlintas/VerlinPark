# Contracted Execution Plan: authority, effects, durability, and tool execution v2

> **Status:** active implementation plan.
> **Created:** 2026-11-12
> **Base:** `448f1b5` (cache-lossless L1–L5 merged).
> **Goal:** make long-horizon multi-agent runs authority-safe, effect-safe,
> durable across upgrades and external waits, and legible under failure —
> without weakening approvals, Seatbelt, path jail, write scope, Verifier,
> merge queue, capacity, or budgets.
> **Shape:** Phase 1 is the next implementation round. Phase 2 and Phase 3 are
> planned but gated on Phase 1 acceptance. No phase is a framework port.

---

## 0. Why this plan exists

Three independent research passes (orchestration frameworks, tool calling and
execution, frontier papers/news) converged on the same conclusion:

> The frontier is not "more agents" or more graph diagrams. It is **durable
> semantics** — what survives crashes, retries, upgrades, and external side
> effects — plus **authority that is checked per call, not per tool name**.

VerlinPark already has stronger isolation than most frameworks: per-task
worktrees, deterministic Verifier/Integrator, event-sourced SQLite, runtime
leases, approval templates, and cache-lossless prompts. The real gaps are
narrow and concrete:

1. **Argument-level intent.** `smart` approvals auto-allow routine workspace
   edits. A tool can be on the allowlist, the path can be inside the worktree,
   and the *arguments* can still be wrong: a write outside the task's declared
   scope, a URL outside the task's egress policy, a shell command that mutates
   something the task never declared, a `git push`/PR/remote write that repeats
   on retry.
2. **External effect receipts.** The run journal blocks duplicate completed
   *workspace writes* by fingerprint, but `git push`, forge PR/MR creation,
   remote writes, deploys, and webhooks have no settled/unknown record. A
   retry or resume can double-push and invalidate the claim that the merged
   commit is the verified commit.
3. **Durability across real waits and upgrades.** Long plans outlive daemon
   releases and wait on CI, PR review, deploy, or a human. VerlinPark has
   deferred continuations, questions, and approval waits, but no generic
   external gate, no plan/runtime version pinning, and no deterministic
   controller replay.
4. **Failure causality and reliability measurement.** Events can say *what*
   failed, not *where the error entered*. `vp-bench` measures solved/cost, not
   pass^k, Lucky-Pass behavior, or coordination overhead.
5. **Tool execution efficiency.** `tool_search` returns matches but does not
   promote schemas; browser snapshots are full trees; MCP async tasks and
   elicitation are unsupported; Code Mode is absent.

Measured evidence behind these gaps:

| Finding | Evidence |
|---|---|
| Compaction erases soft governance | [Governance Decay](https://arxiv.org/abs/2606.22528) (2026-06): violation 0% → 30% pooled (up to 59%); 38% when the summary drops the constraint, 0% when it survives; Constraint Pinning restores 0% for ~47 tokens |
| Passing ≠ correct process | [AgentLens](https://arxiv.org/abs/2607.06624) (2026-07): 10.7% of passing SWE-bench trajectories are "Lucky Passes"; pass^k differs sharply from pass@k |
| Independent MAS can amplify errors | [Nature MI 2026](https://www.nature.com/articles/s42256-026-01268-y.pdf): 17.2× vs 4.4× centralized; 200–300% coordination overhead is the efficiency sweet spot, >400% is over-coordination |
| Failure is a graph property | MAST (NeurIPS 2025) 14 failure modes; [From Spark to Fire](https://arxiv.org/abs/2603.04474) (2026-03) cascade/topology analysis, ≥89% final-infection prevention with a message-layer genealogy guard |
| Code Mode cuts context and round trips | [Anthropic advanced tool use](https://www.anthropic.com/research/advanced-tool-use) (2025-11): 85% prompt-token cut, 37% PTC token reduction, parameter accuracy 72%→90% with examples; [code execution with MCP](https://www.anthropic.com/engineering/code-execution-with-mcp): 150k→2k tokens (98.7%) |
| Tool catalog discovery is incomplete | Set-complete retrieval matters: incomplete tool sets complete <10% of AppWorld queries at k=16; complete sets succeed 21/21 vs 2/39 incomplete ([arXiv 2610.41905](https://arxiv.org/abs/2610.41905), 2026-10) |
| MCP schemas are a supply-chain surface | Tool poisoning/rug pull/cross-server shadowing ([Invariant Labs](https://invariantlabs.ai/blog/mcp-security-notification-tool-poisoning-attacks), 2025-04); poisoning persists over multi-round runs ([arXiv 2610.25372](https://arxiv.org/abs/2610.25372), 2026-10) |
| Durable execution is the harness layer | Inngest `step.run`/`waitForEvent`; Temporal signals/queries/updates; Restate awakeables; LangGraph interrupts; DBOS step checkpoints; Trigger.dev run version locking |
| Plan/runtime version skew is a real bug | Trigger.dev run version locking; Temporal worker versioning; OpenAI Agents SDK explicitly warns approval is not exactly-once and unknown outcomes must not be replayed |

---

## 1. Invariants (hard)

1. **Rust daemon owns state.** Contracts, receipts, gates, snapshots, and
   decisions are SQLite/event-backed. Swift remains a client.
2. **Event-first.** Append the decision/effect event before updating
   projections. New projections must be replayable and rebuildable.
3. **No hidden model routing.** Models, budgets, and policies remain
   user-configured.
4. **No gate weakening.** Approval, Seatbelt, path jail, write scope, Verifier,
   Integrator, capacity, and budget gates are unchanged. New checks may only
   narrow, never widen.
5. **Intent is narrowing-only.** A contract is derived from trusted
   Leader-authored plan text and project policy, never from tool output,
   retrieved content, or model prose. Ambiguity escalates.
6. **Deny wins.** A contract can never auto-approve Destructive, HostState,
   remote, MCP, sensitive GUI, or unknown-risk actions.
7. **Receipts are local evidence, not provider exactly-once.** A `done`
   receipt means the local runtime observed the postcondition; it does not
   guarantee a remote service deduplicated the call. `unknown` blocks
   auto-retry and blocks merge.
8. **External gates are local-only by default.** No public webhook listener.
   CLI/GUI/same-host CI resolution only; an optional loopback endpoint is
   explicit opt-in.
9. **Code Mode is capability-scoped.** `run_script` gets only typed tool
   bindings for the frozen bundle. No shell, HTTP, filesystem, process handle,
   native binding, or ambient network. Every inner call re-enters the normal
   dispatcher.
10. **Schema promotion is event-backed.** Tool search promotion creates a
    bundle revision event and a priced `tools_hash`/cache-epoch change. No
    silent mid-run schema mutation.
11. **Advisory stays advisory.** Failure attribution, progress ledgers, and
    controller replay never auto-merge, auto-cancel, or auto-rewrite a plan.
12. **No free Magentic replan.** The deterministic scheduler remains
    authoritative; an LLM may only provide evidence, never pick the next task
    or rewrite the DAG.

---

## 2. Core model

### 2.1 TaskContract: one object, two uses

```rust
pub struct TaskContract {
    pub contract_id: String,
    pub session_id: String,
    pub task_id: String,
    pub plan_revision: u64,
    pub role: String,
    pub allowed_tools: Vec<String>,
    pub allowed_paths: Vec<String>,      // worktree-relative
    pub forbidden_paths: Vec<String>,
    pub egress: EgressPolicy,            // deny | allowlist
    pub mutation_budget: MutationBudget, // writes, bytes, commits
    pub acceptance: Vec<AcceptanceSpec>,
    pub source_hash: String,             // trusted plan/policy inputs
    pub expires_at: Option<String>,
    pub hash: String,                    // sha256(canonical contract)
}
```

Two uses:

1. **Runtime authorization.** Before dispatch, a deterministic
   `authorize(call, contract) -> Allow | Escalate(code) | Deny(code)` checks
   tool membership, argument paths/URLs/argv, mutation budget, egress, and
   acceptance. It runs in microseconds and persists `intent.decision`.
2. **Prompt capsule.** The same contract is rendered as a small
   `tail/contract` section after compaction, so the model still knows its
   scope and acceptance. The capsule is generated from the typed contract,
   never from the summarizer, and its hash is recorded on
   `context.compacted`.

This is the central architectural bet: **the contract the runtime enforces is
the same contract the model sees.**

### 2.2 EffectReceipt

```sql
effect_receipts(
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  task_id TEXT,
  attempt INTEGER NOT NULL,
  tool TEXT NOT NULL,
  target TEXT NOT NULL,
  request_hash TEXT NOT NULL,
  state TEXT NOT NULL,             -- intent | done | failed | unknown
  provider_key TEXT,
  external_id TEXT,
  response_hash TEXT,
  created_at TEXT NOT NULL,
  resolved_at TEXT,
  UNIQUE(session_id, task_id, tool, target, request_hash)
)
```

- `begin_effect()` inserts `intent` in the same transaction as `tool.started`.
- `finish_effect()` writes `done` + evidence and `tool.finished` atomically.
- A crash between the two becomes `unknown`, not "retry blindly".
- Replay returns a stored `done` receipt; `unknown` requires
  `effect.reconcile` and blocks auto-retry/merge.

### 2.3 ExternalGate

```sql
external_gates(
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  task_id TEXT,
  kind TEXT NOT NULL,              -- ci | pr_review | deploy | human | custom
  token_hash TEXT NOT NULL,
  state TEXT NOT NULL,             -- open | resolved | expired | cancelled
  expires_at TEXT,
  resolution_json TEXT,
  resolved_by TEXT,
  created_at TEXT NOT NULL,
  resolved_at TEXT
)
```

`gate.open` returns a scoped one-shot token; `gate.resolve` is authenticated,
expiry-bound, event-backed. The agent waits through the existing
continuation/defer path, not by polling or holding a Worker slot.

### 2.4 VersionSnapshots

Persist `runtime_version`, `policy_hash`, `tool_bundle_hash`,
`prompt_layout_version`, and `plan_revision` on `team_plans`/`session_runs`.
Store immutable `policy_snapshots(hash, json, created_at)`. Resume uses the
snapshot; drift is a warning or an approval-classified `plan.migrate`.
`orchestration.replay` feeds recorded deterministic decisions through current
code and diffs outcomes. Model output is never replayed as a decision.

---

## 3. Phases

### Phase 1 — Contract & Effects (next round)

**C1. TaskContract + intent authorization + MCP schema drift**

Deliverables:

- `vp-core::contract` with canonical `TaskContract`, derivation from trusted
  plan task + project policy, and `authorize(call, contract)`.
- Argument-level checks for `path`, `paths[]`, `cwd`, `url`, `argv`, `command`,
  `query`/`project_id`/`server_id` where applicable; unknown/ambiguous →
  `Escalate`.
- New approval reason `IntentViolation` / `IntentAmbiguous`; never auto-approve
  Destructive/HostState/remote/MCP/sensitive GUI/unknown.
- `mcp_tool_pins(project_id, server_id, server_name, tool_name, schema_hash,
  description_hash, approved_at, approved_by, status)`; new/changed tools
  require approval; changed tools are quarantined; `mcp.tool.drift` event with
  a bounded diff.
- `tail/contract` capsule rendered after compaction; `context.compacted`
  records `contract_hash`, `contract_chars`; audit
  `contract_capsule_missing`.
- RPC/CLI: `session.contract`, `project.mcp_pins`,
  `mcp.pin.approve`, `vp contract`, `vp mcp-pins`.

Tests:

- In-scope tool + out-of-scope path escalates.
- URL/egress violation escalates; `network=deny` refuses.
- Shell argv/command mutation outside contract escalates or returns unknown.
- MCP description/schema drift requires approval; unchanged tool does not
  prompt; removed tool is informational.
- Ambiguous contract escalates, never allows.
- Compaction keeps the capsule and its hash; summarizer cannot inject a
  constraint.
- Existing approval/Seatbelt/path-jail/Verifier tests stay green.

**C2. EffectReceipts + postconditions**

Deliverables:

- `effect_receipts` table, `begin_effect`/`finish_effect`, `effect.reconcile`.
- Postcondition probes per tool family:
  - workspace write: file hash / absence;
  - git commit: `rev-parse` + branch head;
  - `run_project_task`: result/exit code + output hash;
  - remote write: remote stat/hash;
  - HTTP/local_http: status + body hash;
  - shell: default `unknown` unless an explicit postcondition probe is declared.
- Replay reads receipts first; `done` returns stored evidence; `unknown`
  blocks auto-retry and merge.
- Cover `git push`, forge PR/MR creation, remote write, deploy, and webhook via
  explicit effect targets.

Tests:

- Crash between intent and finish → `unknown`; retry does not double-push.
- `done` receipt replays without re-execution.
- `unknown` blocks merge and requires reconcile.
- Provider-dependent caveat is visible; no claim of remote exactly-once.

**C3. Durable external gates**

Deliverables:

- `external_gates` table, `gate.open`/`gate.resolve`/`gate.cancel`/`gate.list`.
- Agent tool `wait_for_gate` or `session.defer` watch gate; resume through the
  normal prompt/continuation path.
- Expiry, one-shot token, wrong-token rejection, replay idempotency.
- Local CLI/GUI/same-host CI resolution; no public webhook bind. Optional
  loopback listener behind an explicit project policy.

Tests:

- One-shot resolution; replay is idempotent; expiry fails closed.
- Wrong token rejected; no secret in event/log.
- Gate wait survives daemon restart; no Worker slot held.
- No public listener by default.

**C4. Version pinning + controller replay**

Deliverables:

- `policy_snapshots` and version fields on plan/run rows.
- Resume drift warning; `plan.migrate` revalidates DAG, write scopes, budgets,
  and contract hashes.
- `orchestration.replay` diffs deterministic scheduler/task decisions; never
  replays model output; never bypasses budgets.

Tests:

- Version drift warning and migration approval.
- Replay diff detects a changed decision; no model call.
- Migration preserves task IDs/scopes or fails closed.

**C5. Reliability + observability (acceptance)**

Deliverables:

- `vp-bench` metrics: pass^k, variance, flake rate, Lucky-Pass signals
  (commit without passing test, retry with no new evidence, edit→revert,
  verify-before-implement), coordination overhead, cost per solved task.
- Observation-only; never a merge gate.
- Optional OTel GenAI export as an event-log projection: conversation id,
  agent/tool spans, cache read/write; local OTLP endpoint; no secrets.

Tests:

- Deterministic metric math; no provider call.
- Lucky-Pass fixtures; pass^k fixture.
- OTel span decode; no secret attributes.

**Phase 1 acceptance:** contract/intent and receipts are event-backed and
replayable; no gate is weakened; existing Rust/Swift suites stay green; a
long-session fixture demonstrates contract survival across compaction and
receipt-based retry safety.

---

### Phase 2 — Diagnosis & Scale

**C6. Failure attribution / error-cascade ledger**

- Build a provenance DAG from existing events: task → input artifact hashes →
  tool calls → commits → Verifier verdict → merge/integration.
- On failure, report the earliest unsupported claim, responsible agent/step,
  and evidence pointers; advisory only.
- Deterministic checks: handoff contract (Worker result cites the upstream
  artifact hash it used); false consensus (Worker and Verifier both claim a
  test passed while integration fails); stale evidence.
- `session.diagnosis` / `vp diagnose`; no auto-retry/cancel/merge.

**C7. Bounded dynamic fan-out**

- `plan.fanout`: a template task plus a JSON item list from an artifact/tool
  result expands to N tasks (hard cap 20) in a new plan revision.
- Each child has `parent_task_id`, `fanout_key`, a provably disjoint write
  scope, and a shared join task.
- Dedupe by key; no recursive fan-out; cancel siblings on batch abort; keep
  per-task budgets; explicit approval; never automatic plan rewrite.

**C8. Tool search v2 + delta browser snapshots**

- Set-complete retrieval: BM25 over name/description/task kind + prerequisite
  closure; return `{set, completeness_estimate, missing[]}`.
- Schema promotion at turn boundaries as an event-backed `bundle.revision`;
  changing `tools_hash`/cache epoch is priced, not silent.
- Delta browser snapshots: changed/new/removed refs + text delta instead of a
  full tree; bind to snapshot generation; `STALE_REF` unchanged.

**C9. MCP tasks + elicitation**

- MCP async `tools/call` task handle: `working/input_required/completed`,
  `tasks/get|result|cancel|list`, ttl/pollInterval; no hidden polling; map to
  effect receipts/gates.
- Elicitation: form mode routes to the question inbox; URL mode denied by
  default and never auto-opens; audit.
- Dynamic server add/remove remains operator-approved.

---

### Phase 3 — Tool Execution v2

**C10. Code Mode `run_script`**

- `rquickjs` + `oxc`, 64 MB/CPU budget, typed bindings generated from the
  frozen bundle.
- No shell/HTTP/filesystem/process/native bindings; the only capability is the
  tool RPC bridge.
- Every inner call re-enters the normal dispatcher with contract, approvals,
  budget, and effect receipts; stdout via `ToolOutputPolicy`.
- Read-only spike first; write tools require per-call approvals and a
  pre-declared call budget; unknown tools rejected at bind time.
- `tool.finished` events for inner calls; no secret/network leakage.

**C11. A2A loopback facade (optional)**

- Agent Card, task lifecycle, `input-required`/`auth-required`, SSE; loopback
  only; no public push endpoint; maps to external gates.
- No inbound A2A until a concrete partner and capability attenuation exist.

**C12. Advisory progress ledger / Magentic evidence**

- Manager emits a bounded `progress_ledger` per round (satisfied, in_loop,
  progressing, next_task, instruction, evidence).
- Deterministic controller uses it only as evidence; max stalls/resets; no
  automatic replan or DAG rewrite.

---

## 4. Implementation order

**Phase 1:** C1 → C2 → C3 → C4 → C5.

- C1 establishes the contract and schema pins.
- C2 makes effects auditable.
- C3 adds external waits.
- C4 makes plans survive releases.
- C5 measures the result.

**Phase 2:** C6 → C7 → C8 → C9.

- C6 uses C2's receipts for diagnosis.
- C7 uses C1's contracts for disjoint scopes.
- C8 improves tool selection without silent schema churn.
- C9 extends MCP without widening the trusted surface.

**Phase 3:** C10 → C11 → C12.

- C10 reuses C1/C2 for safe inner calls.
- C11 exposes only loopback interoperability.
- C12 stays advisory.

---

## 5. Acceptance

- `cargo fmt/clippy/test`, `just lint`, `xcodebuild test`.
- New event projections replay/repair/db-rebuild.
- No gate weakening: contract/intent never auto-approves Destructive,
  HostState, remote, MCP, sensitive GUI, or unknown.
- Effect receipts: `unknown` blocks retry/merge; `done` replays without
  re-execution.
- External gates: one-shot, expiry, no public listener by default.
- Version pinning: drift is visible and migration is approval-classified.
- Reliability suite: pass^k/Lucky-Pass/overhead math is deterministic and
  observation-only.
- Code Mode: sandbox cannot reach network/filesystem/shell; inner calls honor
  approvals/budgets; read-only spike measured before write expansion.

---

## 6. Non-goals

- No full LangGraph/CrewAI/AutoGen framework port.
- No LLM routing of scheduler decisions or automatic DAG rewrite.
- No public A2A/push endpoint without a concrete partner.
- No provider exactly-once claim; receipts are local evidence.
- No hidden model routing.
- No gate weakening.
- No silent mid-run tool-schema mutation.
- No Code Mode with shell/HTTP/fs/native bindings.
- No auto-merge from attribution, replay, progress ledger, or reliability
  metrics.

---

## 7. Risks

- **Contract injection.** A contract derived from poisoned plan text can be
  too broad. Mitigation: only trusted Leader plan/policy inputs, narrowing-only,
  ambiguity escalates, operator review.
- **Approval fatigue.** A script or a broad shell command can create many
  prompts. Mitigation: pre-declared call budgets, read-only spike first,
  batch/approval templates, explicit unknown.
- **False `done`.** A postcondition probe weaker than the effect can claim
  success incorrectly. Mitigation: per-tool probes, default `unknown`, no
  auto-retry on unknown.
- **Cache churn.** Contract capsule and schema promotion change the prompt.
  Mitigation: tail-only capsule, turn-boundary schema promotion, priced
  `tools_hash` change.
- **Version replay theater.** Replaying deterministic decisions does not prove
  model behavior. Mitigation: label "controller-equivalent only"; never use as
  Verifier or merge evidence.
- **Fan-out explosion.** Bounded cap, disjoint scopes, join task, explicit
  approval, no recursion.
- **Code Mode blast radius.** No ambient capabilities, per-call authorization,
  effect receipts, read-only first.

---

## 8. Companion program: project-scoped agent identities

Stable project-scoped Leader/Manager/Worker identities are a separate, larger
program documented in
[PROJECT_SCOPED_AGENT_IDENTITIES_PLAN.md](PROJECT_SCOPED_AGENT_IDENTITIES_PLAN.md).
It shares C1/C2/C4 as its substrate and must not be bundled into Phase 1.
Labour remains task-scoped. When identities are enabled, Code Mode (C10)
should wait until the identity/role contracts and Worker workspace phases
(S3/S4) are in place.

---

## 9. Implementation progress

| Task | Status | Evidence |
|---|---|---|
| C1 TaskContract + intent + MCP pins | **implemented** | `2647e67`, ADR-0193, 3,418 Rust / 483 Swift tests |
| C2 EffectReceipts | **implemented** | `d78d253`, ADR-0194, 3,458 Rust / 487 Swift tests |
| C3 Durable external gates | **implemented** | `835cb17`, ADR-0195, 3,506 Rust / 491 Swift tests |
| C4 Version pinning + controller replay | **implemented** | `f66e485`, ADR-0196, 3,531 Rust / 495 Swift tests |
| C5 Reliability + OTel | **implemented** | `782cb19`, ADR-0197, 3,571 Rust / 500 Swift tests |
| **Phase 1 complete** | **C1–C5** | contract/intent/MCP pins, effects, gates, runtime pinning, reliability |
| S1 Project-scoped identities | **implemented** | `9bf3df9`, ADR-0205, 3,607 Rust / 503 Swift tests |
| S2 Session assignment + identity lease + inbox | **implemented** | `fa25081`, ADR-0206, 3,634 Rust / 506 Swift tests |
| S3 Role contracts + prompts/cache | **implemented** | `5cedc62`, ADR-0207, 3,719 Rust / 506 Swift tests |
| S4 Worker workspace + two-level integration | planned | next |
| C6 Failure attribution | planned | |
| C7 Bounded fan-out | planned | |
| C8 Tool search v2 + delta snapshots | planned | |
| C9 MCP tasks/elicitation | planned | |
| C10 Code Mode | planned | |
| C11 A2A loopback | planned | |
| C12 Advisory progress ledger | planned | |

## 10. References

- Anthropic, [Introducing advanced tool use](https://www.anthropic.com/research/advanced-tool-use), 2025-11-24.
- Anthropic, [Code execution with MCP](https://www.anthropic.com/engineering/code-execution-with-mcp), 2025-11-04.
- Cloudflare, [Code Mode](https://blog.cloudflare.com/code-mode/), 2025-09-26.
- Inngest, [Durable execution: the key to harnessing AI agents](https://www.inngest.com/blog/durable-execution-key-to-harnessing-ai-agents), 2026-02-19.
- Temporal, signals/queries/updates and worker versioning; Restate awakeables; DBOS step checkpoints; Trigger.dev run version locking.
- LangGraph 1.2.x interrupts/checkpoints/`Send`; Microsoft Agent Framework Magentic and superstep checkpoints; OpenAI Agents SDK approval semantics; Google ADK `LongRunningFunctionTool`/resumability.
- Chen, [Governance Decay](https://arxiv.org/abs/2606.22528), 2026-06.
- MAST, [Why Do Multi-Agent LLM Systems Fail?](https://arxiv.org/abs/2503.13657), 2025; [From Spark to Fire](https://arxiv.org/abs/2603.04474), 2026-03.
- [AgentLens](https://arxiv.org/abs/2607.06624), 2026-07; [Nature MI multi-agent reliability](https://www.nature.com/articles/s42256-026-01268-y.pdf), 2026-07.
- [MCP tool poisoning](https://invariantlabs.ai/blog/mcp-security-notification-tool-poisoning-attacks), 2025-04; [MCP tasks](https://modelcontextprotocol.io/specification/2025-11-25/basic/utilities/tasks) and elicitation.
- [A2A 1.0](https://github.com/a2aproject/A2A), 2026-03; AGNTCY TBAC task authorization, 2026-08.

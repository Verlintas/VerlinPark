# VerlinPark V1 最终计划

- Status: Final plan; implementation in progress. Collab Agent
  Inspector/Direct Channel implemented (2026-11-22) and live-provider exercised
  (2026-11-23). Run Contract/resource observability R1, the P0 tool-friction
  fixes, R2a profile/top-up/pre-dispatch, and R2b core lean turn-boundary pause +
  token/cost/time child Auto request-fit + attention batch prompt + explicit
  partial delivery + rescue reserve/allocation preview + readiness snapshot
  cache + macOS top-up/pause/project policy complete R2b (2026-11-25); R3 per-task envelope, explicit and opt-in pre-run envelope
  persistence, and R4a trade-off preview are implemented; Phase 0 target
  baseline, remaining Phase 1 carried items, Phase 2 scale/fault/soak evidence,
  and the acceptance scenarios remain. The 2026-11-27 hardening batch adds
  Git/provider/setup preflights, an explicit first-run shell trust decision,
  a provider tool-calling probe, a scripted backup/restore drill, bounded
  scale/fault harnesses, and deterministic tool-payload redaction. Open findings
  and live evidence are tracked in
  [V1_READINESS_LEDGER.md](V1_READINESS_LEDGER.md).
- Date: 2026-11-22 (live validation 2026-11-23; Phase 1 closure batch 2026-11-26;
  release hardening 2026-11-27)
- Owner: Verlintas
- Scope: **single-project coding ADE**
- Primary use: the owner's own development of a large, complex project
- Public open-source release: secondary, but V1 is the stable fork point for the
  future private Advanced branch

## 0. Mandate

VerlinPark V1 is the first complete, usable version of the coding agent
workspace. It must be precise enough, reliable enough, and complete enough that
the owner can use it for real development, not only for demos.

V1 is not a public-Alpha checklist exercise. Public packaging, stranger
onboarding, and multi-provider polish are secondary. However, V1 is also the
source baseline from which the owner will later fork the private
VerlinPark Advanced branch, so V1 must remain clean, modular, testable,
documented, and reproducible.

The owner will use V1 directly for testing and development. Real provider keys,
the macOS GUI, and real-machine runs are part of V1 acceptance evidence.

## 1. Scope correction

V1 is **single-project coding only**.

The following are explicitly **not** V1:

- Workspace/Initiative multi-project orchestration
- Environment/Target runtime for hardware or lab equipment
- VerlinAgenticTransLink (VATL)
- Lab, Research, Scientist, Experimenter, or other non-coding agent
  architectures
- A generic Workflow/TaskTarget/Evidence platform
- Multi-repo atomic integration
- Cross-host distributed scheduling, leases, merge ownership, or fencing
- Advanced-specific extension APIs or private crates

Hardware connectivity, VATL, Lab, Research, and other domains will be added
later as independent modules or partitions, in the same spirit as the external
Aviiya module. V1 must not add placeholder abstractions for them, and V1 code
must not be made more complex in anticipation of them.

Single-project means:

- One session/plan belongs to one project/repository.
- A task gets one worktree/branch inside that repository.
- The project remains the unit of worktree, merge queue, Verifier, Integrator,
  budget, ownership, and recovery.
- Multiple projects may exist in the app, but V1 does not coordinate one
  session across multiple projects. A cross-project effort is split into
  separate sessions/projects by the operator.

The single-repository worktree/merge model is retained because it is the most
reliable part of the current architecture.

## 2. What V1 must deliver

V1 must complete the coding loop:

1. Open one local Git repository.
2. Create a session with a goal, a coding team, a model, and a budget.
3. The Leader produces a validated TeamPlan.
4. Managers/Workers/Labour execute tasks in isolated worktrees.
5. The plan can be revised or expanded during discovery, within bounded rules.
6. A read-only Verifier independently validates the work.
7. Approved commits enter the merge queue and the deterministic Integrator.
8. Conflicts/failures can be inspected, retried, or explicitly resolved.
9. The operator reviews the diff and lands/commits or opens a PR/MR.
10. The full plan, conversation, tool calls, approvals, costs, diffs, merges,
    and recovery events are auditable and replayable.
11. A daemon restart or provider failure does not silently lose the session,
    duplicate writes, or merge incomplete work.
12. The macOS app can drive the workflow on the owner's real machine.
13. The operator can inspect any Agent from the Collab team graph and open a
    direct operator channel to that Agent, not only to the Leader.

V1 is complete when the owner can use this loop on a real project without
requiring the operator to hand-edit database rows, manually repair Git state,
or guess what failed.

## 3. Dynamic coding teams

V1 keeps the four structural tiers and their safety boundaries, but the team is
not a fixed four-person script:

- Leader, Manager, Worker, Labour, Verifier, and Scout retain their structural
  roles and authority limits.
- Profiles/task kinds determine tools, prompts, model overrides, and worker
  specialization (coding, research, verification, generic).
- Manager/Worker/Labour counts are configurable within runtime caps.
- Plan tasks can be added through the task inbox, fan-out, or bounded
  in-run plan revision.
- Scout can explore before a plan is committed.
- Parallel Workers must declare non-overlapping write scopes.
- Managers remain read-only department owners; Workers own task worktrees;
  Labour is the implementation worker.
- Verifier remains independent and cannot be bypassed by a dynamic team shape.
- The operator can inspect any live team member from the Collab graph and open
  a direct operator channel to that member, not only to the Leader.

"Dynamic" means task decomposition, team composition, and replanning are
flexible. It does **not** mean free-swarm autonomy, hidden model routing, or
relaxed approval/sandbox/Verifier/merge gates.

V1 does not add Lab/Research roles. Future domains will be separate modules and
will not rewrite the coding core.

### 3.1 Collab Agent Inspector and Direct Channel

V1 adds operator transparency at the Agent level. The current Collab graph
already supports node selection and a task/activity inspector, and Stage
already supports `session.steer` for running/waiting Agents. Those pieces are
insufficient because the operator can only see task metadata and can only
message an Agent through a separate Stage sheet.

V1 must make the Collab node the primary Agent inspection and communication
surface:

- Clicking an Agent node selects it and opens a bounded inspector.
- The inspector has tabs:
  - **Overview**: role, title, department, task, status, model, profile,
    parent Agent, worktree/branch, budget, cost.
  - **Context**: current objective, task acceptance/write scope, context usage,
    compaction/checkpoint summary, memory-brief summary, tool bundle, cache
    prefix hashes, recent provider/context telemetry.
  - **Conversation**: the Agent's recent visible messages, tool calls/results,
    test evidence, citations, and failures. This is a bounded display model,
    not a raw hidden prompt.
  - **Activity**: tools, approvals, commits, merge queue, coordination,
    failures, and recovery events.
  - **Messages**: the operator-to-Agent channel, delivery status, and unread
    state.
- The node shows badges for running/waiting/tool-active, pending steering,
  unread operator message, blocked/failed, and completion.
- The operator can send a message without leaving Collab.

Delivery semantics must be truthful:

1. **Running/waiting Agent**: use the existing `session.steer` path. The message
   is queued and delivered at the target run's next provider boundary; UI shows
   `pending`, `delivered`, or `cancelled`. It cannot interrupt an in-flight
   provider stream or tool call.
2. **Idle Agent with a project identity**: use `agent.inbox.send`, optionally
   with an explicit wake request. UI shows `queued`, `unread`, or `read`; it
   never claims instant delivery.
3. **Idle Agent without a project identity** (Labour/Verifier/Scout and similar
   ephemeral runs): reject the message with a typed `AGENT_NOT_ACTIVE` and tell
   the operator to start/retry the task. Do not store a note that can never be
   delivered, and do not spawn a new run solely to simulate a chat.
4. Operator messages are audit events. They must not approve approvals, bypass
   sandbox/contracts/write scope, change budgets, or enter the merge queue.
5. The target Agent must belong to the current session/project; cross-session
   or cross-project targeting fails closed.

Backend requirements:

- A bounded read model/RPC, implemented as `agent.inspect`, that aggregates
  event-derived metadata, context telemetry, recent visible conversation/tool
  evidence, budget, and unread state by `session_id` and `agent_id`. The
  `agent.context` name is not a separate RPC; context is a section of the
  snapshot.
- An index/projection for agent-scoped event reads; the current event table is
  ordered by session/seq and JSON `agent_id` is not indexed.
- A message route, implemented as `agent.message.send`, that dispatches to
  steering or agent inbox according to the target's state, rejects idle
  ephemeral Agents with typed `AGENT_NOT_ACTIVE`, and reports honest delivery
  status.
- Redaction and bounds for the Context/Conversation tabs. Raw hidden system
  prompts, secrets, credential material, and unbounded tool output must not be
  returned. Show hashes/metadata or bounded previews where necessary.

Acceptance:

- Click every live Agent and see its own Overview/Context/Conversation/Activity/
  Messages without reading the whole session transcript.
- Message a running Worker; see `pending` then `delivered` at the next boundary.
- Message an idle identity Agent; see queued/unread and an explicit wake result.
- Message an ephemeral idle Agent; see a typed `AGENT_NOT_ACTIVE` failure, not a
  fake reply or an undeliverable note.
- Invalid/cross-session targets fail closed.
- Context view is bounded, redacted, and does not leak secrets.
- A direct message cannot approve an approval or bypass a gate.

Estimated V1 cost: **1.5–2.5 weeks** (backend read model/index 2–4 days, UI 3–5
days, delivery routing 2–3 days, tests 2–3 days).

## 4. Non-goals for V1

These are scope exclusions, not forgotten bugs:

- Multi-project Workspace/Initiative orchestration
- Hardware targets, serial/JTAG/network device adapters, lab instruments
- VATL
- Lab/Research/Scientist/Experimenter agents
- Generic Environment/Target/Workflow/Evidence platform
- Cross-host or distributed runtime
- Sparkle auto-update
- A2A inbound execution
- Provider-call exactly-once or mid-stream resume
- Full event sourcing of all operational state
- Native GUI OS sandboxing
- Universal Intel support if V1 is declared Apple Silicon-only
- Advanced branch implementation or Advanced-specific extension points
- Free-swarm or Agent-to-Agent chat rooms
- Spawning a new Agent run solely to simulate a conversation with an idle
  Agent
- Returning raw hidden system prompts, secrets, or unbounded tool output in the
  Agent Inspector

Any new finding that is a correctness, data-safety, or security bug is not a
non-goal. It enters the defect ledger and is fixed at the appropriate gate.

## 5. Zero-deferral defect policy

Every finding from the 2026-11-19 review, the three 2026-11-22 readiness audits,
AUDIT.md, and future dogfood enters `docs/V1_READINESS_LEDGER.md`.

Severity and gates:

| Level | Meaning | Must close before |
|---|---|---|
| S0 | Data loss, secret exposure, cross-session authority, incorrect merge, physical-safety issue | Any real use |
| S1 | Core coding workflow error, silent failure, unrecoverable state | V1 |
| S2 | Scale/precision/performance/diagnosability issue triggered by the target project | V1 |
| PUBLIC | Distribution, stranger onboarding, multi-provider polish, licenses, release engineering | Public binary release |

Rules:

- Every fix needs a regression test or a reproducible command.
- No item is closed as "later" without an explicit non-goal decision.
- A target milestone may differ, but the item remains visible until closed.
- No feature work starts while an S0/S1 item that affects that feature is open.
- No fake success: a test or live run that did not execute is reported as
  compile-only or unverified.
- The ledger is reviewed at every phase boundary.

## 6. Known must-fix items carried into V1

These are already discovered; V1 does not start with a clean slate.

### 6.1 Data safety and security

- Tool secrets currently can enter `tool.started`/`tool.finished`, approval
  rows, event mirrors, search, bundles, and backups. Add deterministic
  redaction or content-by-reference for secret-like headers, URL query tokens,
  MCP arguments, and file content.
- The default shell/fetch posture is permissive (`network=allow`,
  `restrict_home=false`, `read_jail=false`). Provide safe defaults or an
  explicit first-run trust decision for the owner's repository.
- `backup-verify` can report `"full"` after the 100k-blob verification cap and
  can advance the anti-rollback anchor. It must report partial/sampled
  verification or refuse to advance the anchor.
- Backups exclude worktrees/Git/sources and v1 ZIP is unencrypted/unsigned.
  The CLI/UI must state that backup is not a full repository/disaster-recovery
  backup; recommend landing/pushing and the encrypted repository.
- Remote ACP accepts cleartext endpoints and literal credential-like headers.
  V1 disables remote ACP or fixes both.
- Invalid `VERLINPARK_APPROVAL_MODE` silently becomes `smart`; it must fail
  closed.
- Native computer and Code Mode are not OS-sandboxed. They are disabled or
  explicit experimental opt-ins for V1.
- Add a database schema-version/downgrade guard.

### 6.2 Core runtime

- Auto Budget previously extended only at top-level Leader turns, so TeamPlan
  Worker/Labour spend could exhaust the budget between Leader turns. R2b adds
  token request-fit for every role and cost/time request-fit for child runs at
  provider boundaries, but the cost estimate is a recent per-call average, an
  unpriced first call can still overshoot, and this is not exactly-once
  accounting. Treat the residual gap as open until a live multi-Worker run
  records evidence.
- In-run plan revision is incomplete; the operator RPC/CLI exists, but the
  Leader cannot perform a bounded revision during discovery.
- `db-rebuild` silently skips malformed source snapshots; report them in
  `skipped` or fail closed.
- Imported archives are called read-only but `session.replay --repair` can
  mutate projections; guard or correct the claim.
- `backup-verify` and source rebuild findings must have regression tests.

### 6.3 First-run and local usability

- A remote provider can be saved without an API key and still appear
  `configured: true`. The wizard must not display "Connected" unless the
  provider is actually configured and the required probe passes.
- The probe is text-only. A model that cannot use tool calls must be labeled
  unverified or rejected for coding sessions.
- The wizard can finish without a project. Require a project.
- Project add accepts non-Git/empty directories. Require a non-empty Git
  repository or provide an explicit initial-commit path.
- Git CLI/CLT and `user.name`/`user.email` are not preflighted; the first
  Worker commit or landing merge can fail. Add a preflight and an actionable
  fix path.
- New Session defaults to Auto with an empty hard cap and does not require a
  configured provider. Explain the cap and gate session creation.
- Add project removal/recovery for a bad project entry.
- Fix the wizard/daemon startup race and add connection/retry feedback.
- Verify Keychain behavior with the ad-hoc build and TCC-protected project
  folders on the owner's real machine.

### 6.4 Scale and precision

- Prove 100+ task plans, 8–10 Workers, long DAGs, parallel write scopes, merge
  queue pressure, and conflict handling on a real repository.
- Prove 50+ turn sessions, compaction, checkpoint/resume, daemon restart, and
  provider stream failure.
- Keep context, cost, and capacity within hard budgets.
- Add performance budgets for startup, plan validation, scheduling, repo map,
  search, merge, and memory.
- Add failure injection at state transitions: daemon kill, partial write, disk
  full, network drop, provider truncation, duplicate tool call, lost task.
- Make failures diagnosable from the event log without reading source code.

### 6.5 Public release (after V1 personal stability)

- Align Cargo/app/tag/cask versions; a normal release tag must find the built
  artifact.
- Run fmt/clippy/tests before a tag can publish.
- Sign and verify every shipped binary, including `bin/vp`; make the
  architecture coherent (arm64-only declared or universal Rust binaries).
- Clean-Mac install + first-run smoke; Gatekeeper instructions.
- Complete provider matrix or label unsupported presets experimental.
- Update THIRD_PARTY_NOTICES and stage license files.
- Pin actions, add dependency/license audit, and enforce `--locked`.
- Reconcile README/STATUS/PLAN claims about signing, notarization, backup, and
  what is actually implemented.

## 7. V1 acceptance scenarios

The owner must run these on the real target project before V1 is accepted.

1. **Small bug fix**
   - One repository, one Worker, one Verifier, merge, land.
   - Check exact diff, tests, cost, trace.

2. **Cross-module feature**
   - Leader plan with multiple Managers/Workers, non-overlapping scopes.
   - Verifier rejects once; Worker fixes; merge queue integrates; operator lands.

3. **Large plan**
   - 50–100 tasks, 8–10 Workers, dependency batches, one conflict, one retry.
   - No lost tasks, duplicate writes, or silent batch abort.

4. **Long session**
   - 50+ turns, compaction, Auto Budget, pause/resume, provider stream failure.
   - No overspend; context remains usable; recovery is explicit.

5. **Crash/restart**
   - Kill the daemon mid-run, restart, resume or fail closed.
   - No duplicate completed write; no merge of incomplete work.

6. **GUI real-machine**
   - Provider setup, project add, new session, approval, pause/resume, diff,
     commit/discard, land, restart, backup/restore.
   - Record the exact run and any failure.

7. **Backup/restore**
   - Back up a real data directory, restore into a copy, verify sessions,
     events, approvals, and artifacts; document what is not covered.

8. **Collab Agent Inspector and Direct Channel**
   - Click Leader, Manager, Worker, Labour, and Verifier nodes.
   - Inspect each Agent's own context/conversation/activity, not only task
     metadata or the session-wide transcript.
   - Message a running Worker and observe pending → delivered at the next
     provider boundary.
   - Message an idle identity Agent and an ephemeral idle Agent; verify honest
     queued/pending states and no fake instant reply.
   - Verify redaction/bounds, unread badges, cross-session rejection, and that a
     message cannot approve an approval or bypass a gate.

### 7.1 Live validation update (2026-11-23)

Real DeepSeek `deepseek-flash` runs were executed in a temporary data
directory. A one-task run completed Worker → Labour → Verifier → merge queue →
deterministic Integrator, merged `c7244f7`, and passed the integration test
command. A three-task run failed honestly at its 2M token budget before
verification. A second one-task run hit `EFFECT_UNKNOWN` from an unprobed raw
shell commit, was separately verified, and failed at its 1.5M budget during
recovery after the operator reconciled the receipt.

The live round found and closed two V1 bugs: steering could be queued to an
Agent whose durable run had already ended (stale `running` row), and an app
reconnect could overwrite a daemon provider configured outside the client with
stale local defaults. Both have regression tests; the provider fix has a live
daemon check. Remaining findings, including the effect-reconciliation recovery
cost and the missing live GUI running-Worker send, are listed in
[V1_READINESS_LEDGER.md](V1_READINESS_LEDGER.md).

## 8. Phase plan

### Phase 0 — Target project and baseline (3–5 days)

No business-code changes.

- Confirm the target project is a single Git repository for V1.
- Record languages, build/test commands, test duration, branch/review policy,
  CI, secrets, and expected task sizes.
- Define the 8 acceptance scenarios above.
- Run the current VerlinPark on 3 representative tasks; record successes,
  failures, latency, tokens, and cost.
- Merge every known finding into `V1_READINESS_LEDGER.md`.
- Freeze V1 scope and non-goals.
- Decide the version/tag policy (`v1.0.0` only if protocol/DB compatibility is
  committed; otherwise `v0.1.0` stable).

Exit: target profile, baseline report, ordered ledger, acceptance scripts,
frozen scope.

### Phase 1 — Correctness, data safety, and local truthfulness (3–4 weeks)

Close all S0/S1 items:

- secret persistence/redaction
- safe shell/fetch posture or explicit trust decision
- backup-verify cap/anchor and backup-scope warning
- Auto Budget vs TeamPlan
- approval-mode fail-closed
- remote ACP/Code Mode/computer gating
- source rebuild reporting
- imported archive repair semantics
- schema/downgrade guard
- provider/project/Git/Auto-cap/New-Session preflights
- plan revision foundation

Exit: S0/S1 zero; fault-injection and regression tests pass; backup/restore
drill passes; a clean local first-run cannot report false success.

2026-11-27 status: secret redaction, shell trust decision, backup scope/verify,
approval fail-closed, remote ACP/header handling, source rebuild reporting,
imported repair semantics, schema guard, Git/provider/Auto-cap/New-Session
preflights, empty-project recovery, and a scripted backup/restore drill are
implemented. A clean `just release-preflight full` reported 4,806 Rust passed /
0 failed / 6 ignored, 544 app + 132 helper Swift tests passed, the backup drill
and scale/fault harness passed, a universal ad-hoc DMG was built and verified,
and release binaries built. Live DeepSeek validation on 2026-11-27 confirmed
the provider text stream, `tool_calling=true` for `probe_echo`, a full
Leader→`write_file`→idle session (5 calls, 93.85% cache hit), and a small
cache-bench; the key was environment-only. A real-machine GUI smoke also
verified the first-run probe/tool calling, hardened trust, an exact
`GUI_LIVE.md` write, and host-state shell approval cards where approval was
recorded but the sandbox still blocked an outside-worktree write, and a retry
denial did not run the command. A live two-task TeamPlan reached the full T2
Worker→Verifier→merge-queue→Integrator path and merged T2; T1 was verified but
blocked by scratch-probe effect receipts, and a retry exposed generated Python
caches as a write-scope violation. In-worktree Python cache cleanup and commit
staging were fixed and unit-tested. Mid-plan pause/resume verification loss,
scratch-variable approval loops, and the low default Leader turn cap remain
open. Native computer/Code Mode remain open S1 items, the live run is not a
10-Worker/soak proof, and Keychain/TCC first-run still needs a real-machine
pass. In-run plan revision remains open.

### Phase 2 — Scale, coding precision, and Agent Inspector (4–6 weeks)

On the real target repository:

- 100-task plans and 10 Workers
- parallel write scopes, merge queue, conflicts, retries
- long context, compaction, checkpoint/resume
- test impact/cache, integration command, flaky tests
- performance budgets and memory
- 72-hour soak
- diagnosis from events
- Collab Agent Inspector: per-Agent context, conversation, activity, and
  message tabs
- direct operator channel: active steering, idle identity inbox, typed
  ephemeral not-active, truthful delivery states
- agent-scoped event read model/index; bounded and redacted context views

Exit: acceptance scenarios 1–8 pass; no unknown S0/S1; performance within
budget; failures are diagnosable; no secret leakage in the inspector.

### Phase 3 — Personal dogfood (2–3 weeks)

- The owner uses V1 daily on the real project.
- Real provider keys and GUI real-machine runs.
- Every new issue enters the ledger and is fixed before continuing.
- Record run logs, cost, recovery, and UI friction.
- Use the Collab Inspector/Direct Channel daily; fix any context, delivery, or
  redaction problem before continuing.
- No feature additions except those required by the real project.

Exit: owner is willing to trust V1 for continuous work; acceptance scenarios
pass with recorded evidence.

### Phase 4 — V1 stabilization and fork point (2–3 weeks)

- Fix dogfood findings.
- Freeze protocol, database schema, event names, and budget semantics.
- Write/refresh architecture, migration, recovery, and known-limitations docs.
- Run full Rust/Swift/live/fault-injection/soak suites.
- Run `just release-preflight` on a clean tree. It checks version alignment,
  runs fmt/clippy/full Rust tests, SwiftFormat/xcodebuild, the backup/restore
  drill, `vp bench scale`/`fault`, and builds release binaries. It does not
  sign or notarize; `scripts/package-release.sh` handles that when a Developer
  ID identity and notary profile are supplied, and `just verify-dmg` checks the
  resulting checksum, image, app/helper layout, and signature without
  installing it.
- Tag the V1 source release and create a source archive.
- Optionally run the public binary release gate; public packaging is not
  allowed to delay the owner's V1 if the source tag is stable.

Exit: V1 stable tag, test report, source archive, migration/backup notes, and a
clean working tree.

**Total: 12–18 focused weeks for one engineer, quality-first**, including the
Collab Agent Inspector/Direct Channel (1.5–2.5 weeks). With parallel subagents,
the critical path can be compressed to roughly 9–14 weeks, but no phase may
skip its exit gate.

## 9. Quality contract

- No `unwrap()` in daemon library code unless clearly infallible.
- Typed errors for user-visible failures; no error-code folding.
- Every behavior change has a regression test or a reproducible scenario.
- Core scheduler/merge/budget/security modules have a coverage baseline and
  must not regress.
- Add property/model tests for plan scheduler, merge queue, budget reservation,
  and lease state machines.
- Add failure injection at every state transition that can lose work.
- Keep module-governance budgets; do not create new giant files.
- Performance budgets are set in Phase 0 and measured in Phase 2/3.
- Live provider, GUI, and backup tests are recorded evidence, not claims.
- No fake success from empty provider turns, missing test output, or skipped
  checks.
- The Collab Agent Inspector returns bounded, redacted data; raw hidden system
  prompts, secrets, credential material, and unbounded tool output never enter
  the read model.
- Direct-channel delivery states are truthful: `pending`, `delivered`,
  `queued`, `unread`, `read`, `cancelled`, or typed failure. Never show an
  instant reply that did not occur.
- Operator messages cannot approve approvals, bypass contracts/sandbox/write
  scope, change budgets, or enter the merge queue.
- Agent-scoped timeline reads must be indexed/bounded and must not scan the
  whole session event log on every UI refresh.
- Architectural changes require ADRs; behavior changes update PROGRESS.

## 10. Public release and Advanced

V1 is the stable source baseline. The public GitHub base continues after V1;
public binary release can follow once the PUBLIC ledger is closed.

The future Advanced branch:

- is a deep customization fork of the stable public V1 tree;
- is used only by the owner; private GPL use is acceptable while it is not
  distributed;
- may add Lab/Research agent architectures, VATL, private prompts/skills,
  private memory, and device adapters as separate modules/partitions;
- does not require V1 to implement placeholder extension APIs now;
- must not be developed in this V1 plan.

If Advanced is later shared with friends, the GPL distribution implications must
be reviewed before sharing binaries.

## 11. Risks

| Risk | Mitigation |
|---|---|
| Scope creep into Workspace/hardware/Lab | V1 scope is single-project coding; all other domains are separate modules |
| Public release distracts from personal use | V1 is the source-stable gate; public packaging is a separate gate |
| Large repository performance unknown | Phase 0 baseline; Phase 2 budgets and real-project soak |
| Provider behavior differs from wiremock | Live API-key matrix in Phase 3; one primary provider required, others labeled |
| Data loss from backup assumptions | Backup-scope warning; encrypted repository; restore drill; land/push before backup |
| Auto Budget fails during TeamPlan | Fix plan Worker extension or use Fixed budgets with warning |
| Dynamic team weakens safety | Structural roles, contracts, approvals, Verifier, merge gates remain unchanged |
| Agent Inspector leaks secrets or raw hidden prompts | Bounded/redacted read model; hashes/metadata; explicit tests |
| Direct channel implies fake instant delivery | Truthful pending/queued/unread states; no idle-run spawning |
| Agent-scoped timeline is too slow | Index/projection and bounded pagination |
| Advanced fork diverges painfully | Keep V1 modular, documented, tagged; do not build Advanced now |

## 12. Assumptions and open decisions

Assumptions:

- V1 is one repository per session; cross-project work is split into multiple
  sessions.
- Hardware, VATL, Lab, and Research are out of scope until after V1.
- The owner will dogfood V1 on the real project before accepting it.
- Public binary release may lag the V1 source tag.
- The target provider(s) for V1 will be confirmed in Phase 0; one primary
  provider must be live-validated.

Decisions to confirm before Phase 0:

1. V1 tag name: `v1.0.0` (protocol/DB stability commitment) or `v0.1.0` stable.
2. Primary coding provider and one fallback, if any.
3. The real target repository and its acceptance tasks.
4. Whether the public binary release ships with V1 or follows it.

## 13. Definition of done

V1 is done only when all are true:

- [ ] S0/S1 ledger is empty; S2 items affecting the target project are empty.
- [ ] All acceptance scenarios pass with recorded evidence.
- [ ] Real provider, GUI, and backup/restore runs are recorded.
- [ ] Collab Agent Inspector and Direct Channel acceptance passes for Leader,
      Manager, Worker, Labour, and Verifier, including redaction and truthful
      delivery states.
- [ ] 72-hour soak and crash/restart tests pass.
- [ ] No known duplicate-write, lost-task, cross-session, or merge-boundary defect.
- [ ] Full Rust and Swift suites, clippy, fmt, and swiftformat pass.
- [ ] Architecture, protocol, schema, migration, recovery, and known-limitations
      docs are current.
- [ ] V1 source tag and source archive exist.
- [ ] The owner has used V1 on the real project continuously and accepts it.
- [ ] No Advanced/VATL/Lab code has entered the V1 base.

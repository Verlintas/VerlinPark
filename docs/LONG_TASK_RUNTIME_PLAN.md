# Long-Task Runtime Plan — durable, remote, programmatic, native

> **Status:** implemented (2026-11-11); this file remains the design record.
> **Created:** 2026-11-11
> **Goal:** make VerlinPark survive process death, remote workspaces, and
> multi-milestone programs, and add native macOS computer control.
> **Budget:** one large round, target +30,000 net lines of production code and
> tests. No filler; each module must be complete enough to operate, fail closed,
> and be replayed/audited.
>
> **Result:** R1–R4 landed as seven commits on `main` (`5208270`, `191ec56`,
> `6298fc0`, `315ecf7`, `0d75274`, `7e4a7ba`, `a504e10`), with `git diff --stat
> c615f72..HEAD --` showing **42,176 insertions / 1,715 deletions** before the
> documentation close-out. Rust coverage is **932 passing tests** (636 core,
> 98 store, 73 remote, 32 remote-agent stdio, 31 protocol, 18 provider, 14
> jail-contract, 12 wire-contract, 6 remote-agent unit, 4 CLI, 4 daemon, 4
> remote-host) with 5 ignored; Swift is **170 app tests plus 116
> computer-helper tests**. All gates remain intact. The implementation and its
> limitations are described in ADR-0155–0158 and `PROGRESS.md`; the modules are
> not stubs, but the explicit limits below still apply.

---

## 0. Why this round

The efficiency round made the harness cheap and observable. The next reliability
gaps are structural:

1. A crash between a tool call and its projection can lose or duplicate work.
2. The runtime is local-only; super-heavy projects often live on remote hosts.
3. A session plan is not a multi-session program; there is no milestone ledger.
4. Native macOS GUI control is still absent.

This round addresses all four with independent, additive modules. Each module
must preserve the invariants in `AGENTS.md`: deterministic Rust owns
scheduling/permissions/merge; every session state change is event-backed; no
hidden model routing; no safety gate is weakened.

---

## R1 — Durable run journal and crash resume

### Problem

`session_operations` prevents duplicate prompt admission, but it does not record
what happened inside a run. A daemon crash after a side-effecting tool call can
leave the session `interrupted` with no precise knowledge of what completed.
Blind replay can duplicate writes; blind reset loses work.

### Design

- New tables:
  - `session_runs`: run id, session/project, agent/task/department, objective,
    model, status (`running/completed/failed/interrupted/resumed`), attempt,
    `resumed_from_run_id`, started/updated/ended, budget snapshot, worktree head,
    last event seq.
  - `run_steps`: run id, seq, turn, kind (`model_request`, `tool_call`,
    `compaction`, `approval`, `question`), status (`intent`, `completed`,
    `failed`, `unknown`), input hash, output hash, payload JSON.
  - `run_tool_calls`: run id, step id, provider tool call id, tool name, input
    fingerprint, status, result JSON, error.
  - `run_checkpoints`: run id, seq, message cursor, event cursor, budget,
    worktree head, plan revision.
- Agent loop records intent before a provider/tool call and outcome after it.
  Checkpoints are written at safe turn boundaries.
- On daemon startup, `running` runs become `interrupted`. A project policy can
  opt into bounded auto-resume; manual `session.run.resume` always exists.
- Resume:
  - only idle/failed/cancelled session, clean/compatible worktree head, same
    plan revision, unexhausted budget, configured provider/model;
  - creates a new linked run attempt, reconstructs messages from events, injects
    a bounded journal note naming completed and unknown tool calls;
  - completed read-only tools may be re-run; completed write tools are
    hard-blocked by tool name + input fingerprint and return the journaled
    result;
  - interrupted tools are reported as `unknown outcome`; the model must verify.
- Events: `session.run.started/checkpointed/step_intent/step_completed/
  tool_intent/tool_completed/interrupted/resumed/failed/completed`.
- Replay/repair: run status and attempts are event-backed; tool outcomes are
  operational audit state and are never silently replayed.
- Policy: `SessionRunPolicy { auto_resume: bool (default false), max_attempts:
  1..3, max_age_hours: 1..168 }` on the project.

### Tests

- Crash after model response before checkpoint; crash after tool result before
  checkpoint; crash before tool start.
- Resume skips completed provider calls; hard-blocks duplicate write tools;
  re-runs safe read tools; reports unknown tools.
- Budget/plan/worktree/provider mismatch refuses resume.
- Attempt cap, policy auto-resume, manual resume.
- Replay/repair/bundle/db-rebuild; no duplicate side effects in tests.
- Existing operation receipts and Scout tests remain green.

### Files

New `vp-core/src/run_journal.rs`, store tables/methods, protocol types, daemon
RPC/startup, agent loop hooks, tests, ADR-0155.

---

## R2 — Remote workspace over SSH / stdio

### Problem

The daemon and all tools are local. Remote projects require either a full remote
daemon or manual SSH. Agents need structured remote file, shell, git, and
worktree operations without installing provider credentials on the remote host.

### Design

- New crate `vp-remote`:
  - newline-delimited JSON-RPC 2.0 over a child process's stdio;
  - transports: `StdioTransport` (local helper) and `SshTransport`
    (`ssh <host> -- <helper> remote-agent`);
  - handshake with protocol version, capabilities, auth token, root;
  - operations: `hello`, `stat`, `list_dir`, `read_file` (offset/limit/base64),
    `write_file` (expected sha, atomic temp+rename), `exec` (cwd/env allowlist/
    timeout/output cap/process-group cancel), `git_status`, `git_diff`,
    `git_log`, `git_worktree_add/list/remove`;
  - path jail: canonical root, reject `..`, absolute escape, symlink escape;
  - operation ledger with `operation_id` idempotency and append-only audit;
  - bounded frames, version mismatch refusal, explicit error codes.
- Helper binary `vp-remote-agent` (new crate or `verlinpadd remote-agent`) is
  dependency-light, needs only POSIX + git; no provider keys.
- Core:
  - `ExecutionHost` project config (`local` or `ssh`, host/user/port/root/
    helper path/connect timeout).
  - `RemoteWorkspace` manager keyed by project/host, idle reconnect, capability
    probe, health.
  - New agent tools: `remote_read`, `remote_write`, `remote_list`, `remote_exec`,
    `remote_git`, `remote_worktree`; explicit remote tools, not a hidden global
    routing switch.
  - Permissions: remote write/exec are host-state/external; approvals and
    project ShellPolicy still apply; no remote shell bypass.
  - Worktree create/list/remove on the remote root with the same branch/base
    semantics as local worktrees.
- RPC: `host.list/add/update/remove/probe`, `remote.status`.
- Tests: fake helper over stdio, path traversal/symlink, atomic write + sha
  conflict, exec timeout/cancel/output cap, git worktree create/list/remove,
  operation idempotency, version/auth refusal, SSH argv construction, approval
  classification.

### Files

New `crates/vp-remote`, new helper crate, `vp-core/src/tooling/remote.rs`,
protocol, daemon, store host table, tests, ADR-0156. CLI optional.

---

## R3 — Project programs and milestones

### Problem

Sessions and TeamPlans are bounded by one run. Multi-day work needs a
project-scoped program ledger: milestones, dependencies, budgets, retries, and
an auditable Strategy → Execution → Verification loop.

### Design

- New project-scoped tables:
  - `programs`: id, project, goal, status (`draft/active/paused/completed/
    failed`), policy, token/USD budgets, created/updated.
  - `program_milestones`: id, program, ordinal/dotted key, title, objective,
    acceptance, deps, priority, status (`pending/ready/running/verifying/
    completed/failed/blocked/cancelled`), session_id, plan_revision, attempts,
    max_attempts, next_attempt_at, result, created/updated.
- Deterministic controller:
  - ready milestones by priority/aging/deps;
  - create a normal session + one-task TeamPlan (or reuse an explicitly attached
    session), through the existing scheduler/admission path;
  - milestone completion requires session idle + plan task completed + Verifier
    approval; failure follows bounded retry/backoff then block/escalate;
  - dependency failure blocks/cancels dependents by policy;
  - pause/resume/cancel are cooperative and event-backed.
- Budgets: program and milestone token/USD budgets; existing session/task
  budgets still win; no bypass.
- Events: `program.created/updated/paused/resumed/completed/failed`,
  `program.milestone.created/started/verified/completed/failed/blocked/
  retried/skipped`.
- RPC: `program.list/create/update/pause/resume/delete`,
  `program.milestone.create/update/retry/skip`,
  `program.status`.
- Replay/repair/bundle/db-rebuild; program rows are project-scoped and enter the
  project manifest.
- Swift: Manage Dash Program card with milestone list/status/actions; optional
  but preferred for usability.
- Tests: lifecycle, deps, retry/backoff, budget exhaustion, pause/resume,
  scheduler tick, no duplicate session, replay/repair/bundle/manifest,
  authorization/validation bounds.

### Files

New `vp-core/src/program.rs`, store tables/methods, protocol, scheduler tick,
daemon RPC, Swift card/models, tests, ADR-0157.

---

## R4 — Native macOS computer control

### Problem

The browser helper covers web UI only. Agents cannot operate native apps,
windows, dialogs, or system UI.

### Design

- New packaged helper `VerlinParkComputerHelper` (Swift/AppKit + Accessibility +
  CGEvent + ScreenCaptureKit), JSON-lines protocol.
- Actions:
  - `permissions` (Accessibility + Screen Recording status);
  - `apps`, `windows`, `focus`;
  - `snapshot` (bounded AX tree with stable element refs, roles, labels, values);
  - `screenshot` (window/screen PNG to artifact, bounded);
  - `click` (element ref or normalized point), `type`, `key`, `scroll`, `drag`;
  - `wait` (element/condition with timeout).
- Rust `computer.rs` manager reuses `ComputerLease`; helper launched by daemon,
  not by the agent; bounded response sizes; command timeout; process cleanup.
- Agent tool `computer` for Leader and coding Worker/Labour, serialized by
  `ComputerLease`; every action is risk-classified and approval-gated according
  to the existing `ApprovalMode` (host-state/external).
- Safety:
  - permission checks are explicit and fail closed;
  - no shell execution through the helper;
  - coordinates/element refs are validated;
  - screenshots are artifacts, not inline model data unless explicitly requested;
  - document that native GUI is not sandboxed and requires a logged-in session.
- Tests: protocol parsing, AX tree bounding, action validation, lease contention,
  permission failure, screenshot metadata, approval classification; manual smoke
  on macOS is documented, not CI.

### Files

New helper target, `vp-core/src/computer.rs`, protocol, daemon, agent tool
dispatch, Swift packaging, tests, ADR-0158.

---

## 4. Parallelization and integration order

| Workstream | Primary files | Shared files |
|---|---|---|
| R1 durable runs | `vp-core/src/run_journal.rs`, store | protocol, daemon, agent |
| R2 remote | new `vp-remote` + helper | protocol, daemon, core tooling |
| R3 programs | `vp-core/src/program.rs`, store | protocol, daemon, scheduler, Swift |
| R4 computer | helper + `vp-core/src/computer.rs` | protocol, daemon, agent tool dispatch |

Integration order: **R1 → R2 → R3 → R4**. Resolve additive protocol/daemon
conflicts in that order; never rewrite another module's tables or events.

## 4.1 Implementation record (2026-11-11)

| Workstream | Landed code | Operator surface | Limits that remain |
|---|---|---|---|
| R1 durable runs | `vp-core/src/run_journal.rs`, `session_runs` / `run_steps` / `run_tool_calls` / `run_checkpoints`, project `SessionRunPolicy` | `session.runs`, `session.run.detail/resume/cancel`, `session.run.policy.*`, `vp runs` / `run-*` | Top-level Leader runs only for ad-hoc resume; no provider-call exactly-once or mid-stream resume; task-scoped runs use plan retry; imported archives not resumable |
| R2 remote | `vp-remote`, `vp-remote-agent`, `ExecutionHost`, six explicit tools | `host.*`, `remote.status`, `remote.worktree_list`, `remote.ledger`, `vp host-*` / `remote-*` | Helper must be pre-installed; SSH/user trust is the boundary; no remote daemon/port, cross-host scheduler, or automatic merge of remote worktrees; remote exec is not Seatbelt-confined |
| R3 programs | `vp-core/src/program.rs`, `programs` / `program_milestones` / `program_milestone_sessions`, deterministic controller | `program.*`, `program.tick`, `vp program-*`, Manage Dash Programs card | No LLM program planner, no cross-project program, no distributed fencing; program budget is not a global pre-flight reservation, but milestone budgets are cumulative across retries |
| R4 computer | Packaged `VerlinParkComputerHelper`, `vp-core/src/computer.rs`, `computer` tool, ComputerLease | `computer.status`, agent `computer`, helper README/manual smoke | Not sandboxed; manual Accessibility/Screen Recording TCC grants; AX trees/refs vary by app; screenshots are artifacts; no menu-bar walker, AppleScript, clipboard, URL opener, or shell path |

The four modules are additive and preserve the existing gates. The
`execution_host`, program ledger, and run-journal projections are covered by
replay/repair/bundle/`db-rebuild` to the extent explicitly described in the
ADRs; operational tool outcomes and `plan_task_progress` remain operational
state, not replayable state.

## 5. Acceptance

- +30,000 net lines of production code/tests/docs, measured by `git diff --stat`
  against the starting commit.
- No filler modules, no dead code, no untested happy-path-only code.
- `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace`,
  `just lint`, `just test`, `just bundle` pass.
- Every new state change is event-backed; every new table has replay/repair or
  an explicit operational-state exemption.
- Approvals, Seatbelt, path jail, write scope, Verifier, merge, capacity, and
  budget gates are unchanged or stricter.
- Shared docs and ADR index updated; this file marked complete.

## 6. Non-goals

- No provider/model changes in this round.
- No hidden routing or automatic permission bypass.
- No full remote daemon or cross-host distributed scheduler; remote workspaces
  are explicit project hosts with structured operations.
- No claim that native GUI control is sandboxed.

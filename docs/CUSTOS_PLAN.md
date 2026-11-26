# ◇ CUSTOS — deterministic integrity and repair supervisor

Status: **discussion draft, no implementation yet**  
Date: 2026-11-26  
Repository: `/Users/apple/orca/workspaces/VerlinPark/m3-round`  
Branch: `Verlintas/m3-round`  
Latest commit at write time: `2a9c0c9` (`Record runtime hardening smoke evidence`)

> **Round 1 owner direction (2026-11-26):** Custos may have a GUI, but the
> user must have **no operational authority** in it. The GUI is a sealed,
> read-only observatory with deliberately limited information. Custos is a
> resident guardian with real protective power — closer to the Red Queen than
> to an operator console. This direction supersedes the earlier
> "operator-gated" UI assumptions in this draft; see section 13.

This file is both the current-context handoff and the first Custos design
proposal. It is intentionally outside the agent tool surface.

## 0. Handoff: current state and important locations

### Repository and branch

- Repo: `/Users/apple/orca/workspaces/VerlinPark/m3-round`
- Branch: `Verlintas/m3-round`, pushed to `origin/Verlintas/m3-round`
- No merge to `main`
- Working tree clean at write time
- Rust workspace tests: 4,702 passed / 0 failed / 6 ignored
- Swift tests: 659 passed / 0 failed
- `cargo fmt --check`, full clippy, `swiftformat --lint` passed

Recent commits:

- `566aa5d` — plan-validation errors, repeated-tool-failure guard, incomplete
  tool-history normalization, runtime config RPC, approval-mode GUI path
- `9bb8793` — completed spatial-instrument live run evidence
- `2d09551` — spatial responsive follow-up evidence
- `ed48463` — headless-browser smoke evidence
- `579ad1b` — 320px browser smoke evidence
- `5948f61` — budget refresh, completion-reserve cap, delta coalescing,
  output-limit recovery, bounded write/append, finalize nudge
- `6728fc2` — soft per-role context budget, acceptance cap, Swift field
- `88dba11` — `GIT_OPTIONAL_LOCKS=0` for worktree/git commands
- `2a9c0c9` — runtime-hardening smoke evidence

### Test project and live artifacts

- Test project: `/Users/apple/orca/projects/TEST` (`PDfuji详解网站`)
- The target repository was **not modified** in the latest rounds. It still has
  its pre-existing untracked site/report/research content.
- Final spatial-instrument output (agent-produced) lives in the temp session
  worktree, not in the target repository:
  - data dir:
    `/private/var/folders/fj/gwg4chqx7nvfvygvwmcyxh240000gn/T/opencode/vp-live-xeno2`
  - project: `01a21441-8b3e-74de-92db-3ef4bc1dff48`
  - session: `01a21441-936c-728a-8f90-232468cfffa1`
  - T1 merge: `c5ae960`; T2 merge: `42eba28`
  - preview copy:
    `/private/var/folders/fj/gwg4chqx7nvfvygvwmcyxh240000gn/T/opencode/xeno2-preview`
  - preview server: `http://127.0.0.1:8912/site/xeno/` (still running)
- Runtime-hardening smoke:
  - data dir:
    `/private/var/folders/fj/gwg4chqx7nvfvygvwmcyxh240000gn/T/opencode/vp-live-smoke`
  - session: `01a216e8-dc94-7194-8c83-ddeeea206f9b`
  - Worker commit: `cc3cbed`; merge: `dcdcbc1`
  - one-task plan `smoke-file`, Verifier approved, Integrator merged
  - about 287k tokens
- Older failed/partial live run:
  - data dir:
    `/private/var/folders/fj/gwg4chqx7nvfvygvwmcyxh240000gn/T/opencode/vp-live-pdfuji`
  - session: `01a21409-0791-7464-b56a-b6afbf7bd8b2`
  - T1 worker commit: `2e30b38`; Verifier/Integrator did not complete
- The API key is **not** in the repo, events, logs, or UI. For live tests it is
  read from macOS Keychain service `com.verlintas.VerlinPark`, account
  `provider.apiKey`, and injected only as a process environment variable.
  Never print it or write it to a file.

### Important documents

- `AGENTS.md` — repo invariants and current milestone/limits
- `docs/V1_FINAL_PLAN.md` — V1 scope and acceptance scenarios
- `docs/V1_READINESS_LEDGER.md` — current evidence, open gaps, live results
- `docs/ARCHITECTURE.md` — runtime/protocol invariants
- `docs/DECISIONS.md` — ADRs
- `docs/PROGRESS.md` — implemented features
- `docs/AUDIT.md` — known enforcement gaps/priority
- `docs/PLAN.md` — product direction
- `docs/RUN_CONTRACT_AND_RESOURCE_GOVERNOR_PLAN.md` — budget/governor design
- `docs/TOOLING_ROADMAP.md` — tool gaps
- `docs/CUSTOS_PLAN.md` — this file

### Important code paths

- `crates/vp-core/src/agent.rs` — `SessionBudget`, context policy, resource
  pause, auto budget runtime
- `crates/vp-core/src/agent/run_loop/body.rs` — turn loop, budget refresh,
  truncation recovery, finalize nudge, tool execution
- `crates/vp-core/src/agent/run_loop/agent.rs` — per-run budget construction
- `crates/vp-core/src/agent/provider.rs` — stream consumption, delta
  coalescing, preemption
- `crates/vp-core/src/engine/resources.rs` — resource governor, completion
  reserve, readiness
- `crates/vp-core/src/engine/` — plans, runs, leases, merge/forge, allocation,
  rescue, sessions, effects, cache/usage, workspaces
- `crates/vp-core/src/agent/service_run/` — start/resume/plan/stall/context
- `crates/vp-core/src/tools.rs` — file/shell tools, write cap/append, shell
  sandbox environment
- `crates/vp-core/src/worktree.rs` — worktree/Git operations
- `crates/vp-core/src/tooling/git.rs` — read-only Git tool
- `crates/vp-providers/src/lib.rs` — provider traits/errors
- `crates/vp-daemon/src/` — RPC/HTTP dispatch and sweepers
- `crates/vp-cli/src/` — CLI command model/dispatch/render
- `apps/macos/VerlinPark/{App,Core,Features}` — SwiftUI client

### Known issues / open gaps (honest list)

1. The full spatial-instrument live run spent about **15.1M input / 266.7K
   output tokens**. Main drivers: ~70k active context × hundreds of calls,
   exploratory Labour browser tests, finalize retry, and repeated completion
   pauses. This is not an efficiency success.
2. The child-budget refresh, completion-reserve cap, delta coalescing,
   truncation recovery, write cap/append, finalize nudge, and acceptance cap
   are locally tested and smoke-tested, but **not** revalidated on a full
   spatial-site run with the new 64k Worker context target.
3. `RoleModelConfig.context_budget` is optional and has **no GUI/CLI setting**
   and no recommended default. Operators must set it explicitly.
4. GUI reconnect can still overwrite a daemon 1M context with a stale local
   128k provider setting. Runtime context is not persisted across daemon
   restarts in SQLite.
5. `packed-refs.lock: Operation not permitted` still appeared in the latest
   live smoke despite `GIT_OPTIONAL_LOCKS=0`/`gc.auto=0`. It was cosmetic on
   that run but remains an unexplained Git-ref path.
6. Event deltas are now coalesced, but a full live run has not re-measured
   event freshness under load.
7. Finalize nudge is advisory; it does not auto-commit. A Worker can still hit
   max turns without a commit.
8. Write cap is 48 KiB per call with `append:true`; agents must be taught to
   use it.
9. Spatial site polish: linear mode is session-scoped, not persisted;
   skip-link/noscript/4 aria labels are English-only; no favicon.
10. The target test repo is untouched by design. The final spatial site is in
    a temp preview/session worktree, not the target repo.
11. No new full live site run after the runtime-hardening commits; the smoke is
    only one tiny file.
12. The preview HTTP server on 8912 and Safari may still be open; test daemons
    and watchers are stopped.

## 1. What Custos is

Custos is a **separate deterministic supervisor** for VerlinPark:

- it protects the integrity and health of the runtime/state/worktrees/queues;
- it detects, classifies, and repairs execution-state failures;
- it can freeze, quarantine, restart, restore, or escalate according to a
  pinned policy;
- it is **not an LLM**, never calls a model for a decision, and treats every
  model/agent output as untrusted input;
- it is **not** a replacement for the Verifier, Integrator, Orchestrator,
  Leader, approval broker, sandbox, or budget governor;
- it does not merge code, approve approvals, expand write scopes, raise caps,
  hire identities, or modify VerlinPark's source/policy.

Names/glyph: `CUSTOS` / `◇CUSTOS`.

The Mirtazapine Shield analogy is useful but Custos is broader: a shield
against corruption/unhealthy state plus a steward for bounded recovery. It
should be designed as a **control-plane supervisor**, not another agent.

## 2. Non-negotiable principles

1. **Deterministic decisions.** Rules/state machines, not model judgment.
2. **No LLM dependency.** No provider key, no network model call, no prompt.
3. **Deny by default.** Explicit capabilities and allowlisted actions.
4. **Least privilege.** Separate process/identity, no provider credentials,
   no arbitrary shell, no source writes.
5. **Evidence before action.** Every action has a precondition, a dry-run, a
   postcondition, and a rollback/stop path.
6. **No gate bypass.** Never approve, never merge, never bypass Verifier,
   contract, sandbox, effect, capacity, or budget.
7. **No self-modification.** Custos does not edit its own policy/code or
   VerlinPark source. Policy updates are operator-authored, signed/hashed,
   reviewed, and deployed out-of-band.
8. **Fail closed on unknown.** Unknown failure → observe/escalate, never guess.
9. **Audit everything.** Append-only, hash-chained, bounded, replayable.
10. **No infinite repair.** Attempt/fingerprint/cost/cooldown caps; escalate.

## 3. Authority ladder

Proposed capabilities; the actual ceiling is an open discussion item.

- **L0 Observe** — read health, status, metrics, logs, DB snapshots; no writes.
- **L1 Advise** — produce a bounded repair preview/recommendation; no action.
- **L2 Safe repair** — deterministic, reversible state repairs:
  - reclaim stale leases/claims;
  - reattach interrupted runs/worktrees;
  - replay/repair covered projections;
  - reset stale Git optional locks? (only if proven safe);
  - retry a transient provider/queue operation through the normal daemon path;
  - clean daemon-owned stale temp/scratch files.
- **L3 Containment** — freeze/quarantine:
  - pause/cancel a stuck session through the normal cooperative RPC;
  - quarantine a worktree/branch from merge;
  - block dispatch/admission for a project;
  - stop a runaway process under the existing supervisor;
  - mark a DB/snapshot unhealthy and refuse further writes.
- **L4 Recovery** — high-impact, operator-gated:
  - restart the daemon;
  - restore a verified backup/snapshot;
  - roll back to a checkpoint/anchor;
  - rekey/revoke? (probably separate security operator flow);
  - break-glass kill of all sessions.
- **L5 Never automatic** — source/policy edit, approval/security bypass,
  merge, budget increase, identity changes, secret access, remote trust
  changes.

Every L3/L4 action needs an explicit policy pin, a bounded time window, an
operator-visible reason, and a post-action verification. Some may require an
operator confirmation/break-glass token even if Custos is allowed to act.

## 4. Health model

Custos should probe, with bounded costs:

- daemon process/alive/heartbeat/start time/build/version;
- discovery/instance lock;
- SQLite openability, integrity check (bounded/rate-limited), WAL growth,
  file size, free disk, inode pressure;
- schema/user_version and migration state;
- event stream freshness (last event age, delta backlog, mirror lag);
- runtime leases/owners/fencing/heartbeats;
- in-flight runs/steps/tool intents and interrupted states;
- task queue/claims/retries/next_attempt_at;
- merge queue/Integrator/remote integration;
- effect receipts (`intent`/`unknown`), external gates;
- worktrees/branches/refs/checkpoints/forks/retention;
- backup repository verification, keyring/anchor/generation;
- provider circuit/error rates/latency (metadata only, no prompts);
- budget anomalies (spend rate, reserves, repeated pauses);
- GUI lease/helper health;
- Git optional/ref locks and disk-related failures;
- clock/time skew.

Health states: `healthy | watch | degraded | critical | quarantined`.

Each probe needs a threshold, a cooldown, a max frequency, and a "not
configured/not applicable" state. No aggressive `PRAGMA integrity_check` on
every tick; schedule/sample and cap.

## 5. Repair ladder

A proposed deterministic matrix (draft; classes/thresholds to be agreed):

| Failure | L0/L1 signal | L2/L3 action | Approval |
|---|---|---|---|
| provider transient/stream | error code, retry count | retry through existing provider path | auto if policy |
| output truncation | `finish_reason=length` | same-run split recovery (already in runtime) | auto |
| stale lease | expired heartbeat | reclaim via existing lease API | auto |
| interrupted run | daemon restart | reattach/resume by existing policy | auto/assisted |
| projection drift | replay mismatch | replay/repair covered projections | auto if replayable |
| queue claim stale | next_attempt_at/age | release/retry claim | auto |
| worktree missing | path/ref check | rehydrate/reattach or quarantine | assisted |
| merge conflict | queue status | existing opt-in resolver | policy/operator |
| effect unknown | receipt unknown | freeze merge, notify/reconcile | operator |
| repeated budget pause | completion reserve/usage | preview scope/budget; no auto top-up | operator |
| DB corruption | integrity check | freeze writes, restore verified backup | operator |
| low disk | free space threshold | stop new dispatch, prune bounded temp/artifacts | assisted |
| daemon crash loop | restart count/window | backoff/quarantine, operator | operator |
| Git lock failure | stderr/lock path | retry without optional locks; quarantine refs | assisted |
| approval/security error | risk class | never auto-repair | never |

Custos must not "repair" by making a failing test pass, changing policy, or
weakening a gate. It repairs the execution environment and state transitions.

## 6. Architecture options

### A. Separate sidecar process (recommended for discussion)

- `custosd` runs as a separate LaunchAgent/LaunchDaemon or user process.
- Talks to VerlinPark via a **dedicated local admin socket** (Unix domain
  socket, 0600, separate credential; no TCP by default).
- VerlinPark exposes a bounded `custos.*` admin RPC surface; normal `vp` RPC
  and agent tools do not expose it.
- Custos has no provider key, no model client, no arbitrary shell.
- It can read daemon health/state through RPC and, where necessary, a
  read-only SQLite snapshot if the daemon is down.
- If the daemon is down, Custos can inspect discovery/DB and invoke a
  preconfigured restart/restore command with fixed argv.
- Its own supervisor is independent (launchd KeepAlive); it cannot repair
  itself by executing generated code.

### B. In-daemon module

Simpler, but a stuck/compromised daemon cannot heal itself; not preferred for
the high-power role.

### C. Hybrid

Small always-on watchdog + separate repair worker. More moving parts; only if
launchd/process reliability proves insufficient.

Open: should Custos be a user LaunchAgent (same user as daemon) or a
root/LaunchDaemon with a dedicated service user? V1 likely user-level, no sudo.

## 7. Policy, authority, and break-glass

- Policy is a signed/hashed file or SQLite row, pinned by hash; Custos logs
  the hash it used.
- `CustosPolicy` fields (draft): enabled, allowed actions/classes, max
  actions per hour/day, cooldowns, max duration, max disk/DB growth, restart
  limits, backup generation requirements, escalation channels, dry-run mode,
  operator approval requirements, retention.
- High-risk actions use a **break-glass grant**: short-lived signed token,
  one action, one target, reason, expiry; logged.
- A kill switch must stop all automatic actions immediately.
- Custos never has a generic "run this command" capability.
- The operator can always downgrade to observe-only.

## 8. Audit and evidence

- Append-only local `custos_events`/JSONL with hash chain.
- Each action: action id, policy hash, precondition snapshot, evidence
  pointers, dry-run, result, postcondition, rollback/stop, duration, operator
  grant id if any.
- No prompts, tool bodies, secrets, model keys, or screen content.
- Bounded retention; export/verify tool.
- Custos actions should be replayable as a state machine; a replay must not
  re-execute side effects.

## 9. Interfaces

Proposed:

- `custos.status` — health, state, policy, version, last probes
- `custos.health` — detailed dimensions/evidence
- `custos.preview` — action plan/cost/risk, no side effects
- `custos.act` — execute a bounded action with grant/approval
- `custos.history` — bounded audit
- `custos.policy.get/set` — operator-only, signed/pinned
- `custos.pause/resume` — global kill switch
- CLI `vp custos ...` and/or separate `custos` binary
- GUI: a read-only Custos card plus preview/act for L3/L4
- Agent tool bundle: **none**. Agents cannot call Custos.

## 10. Testing and rollout

1. Build a deterministic fault-injection harness:
   - provider error/truncation
   - stale lease/claim
   - interrupted run
   - projection drift
   - merge conflict/effect unknown
   - DB busy/corrupt copy
   - low disk
   - event lag/backlog
   - daemon crash loop
   - Git lock/ref failure
   - backup verification/anchor rollback
2. Shadow mode: Custos observes and emits recommendations; no actions.
3. Assisted mode: operator sees preview and applies.
4. Auto mode only for L2 safe classes with tight caps.
5. L3/L4 always operator-gated initially.
6. No live auto-repair until shadow-mode recommendations match operator
   decisions on a recorded corpus.

## 11. Open questions for the next discussions

1. Process boundary: sidecar vs in-daemon; user LaunchAgent vs LaunchDaemon.
2. Authority ceiling: may Custos restart the daemon? restore a backup?
   quarantine a project? kill all sessions? Which of these are L3 vs L4?
3. Break-glass UX: local GUI confirmation, signed file, OS keychain, or a
   separate admin token? How to prevent a compromised agent from obtaining it?
4. Is Custos allowed to call existing daemon RPCs only, or read SQLite
   directly when the daemon is down? What is the read-only snapshot protocol?
5. Policy authorship: static signed file, SQLite + signature, or per-project
   policy? How to rotate/verify?
6. Health thresholds and probe budgets: what is "degraded" vs "critical"?
7. Repair actions that are *not* allowed even at L4.
8. Audit storage: separate SQLite, JSONL, or both; retention and export.
9. How to prevent oscillation/false positives and repair-vs-repair loops.
10. How Custos relates to existing `OrchestrationPolicy`,
    `IntegrationPolicy`, `EffectPolicy`, `RunContract`, and backup/anchor.
11. Does Custos ever repair *itself*? Recommendation: no; launchd restarts
    it, and a human redeploys a verified build.
12. Naming/branding: `CUSTOS`, `◇CUSTOS`, binary name, glyph, UI language.

## 12. Recommended first slice (for discussion)

Do not start with L4. Start with:

- a read-only `custosd` sidecar over a local admin socket;
- `CustosPolicy` in observe-only mode;
- probes for daemon/DB/WAL/leases/runs/queues/events/backups/Git;
- `custos.status`/`health`/`history`;
- shadow-mode recommendations;
- deterministic fault-injection tests;
- no mutations, no provider key, no agent tool, no auto-restart.

Then add L2 safe repairs one class at a time with tests and operator preview.
Only after shadow mode is boring and accurate should Custos get L3/L4 powers.

**Thesis:** VerlinPark should have a powerful Custos, but its power should be
over *state and process health*, not over *truth, policy, security, or code*.
It should be the system that keeps the game fair and running, not a player
that can rewrite the rules.

## 13. Round 1 direction: a sealed resident guardian

The owner's vision is not an admin console. It is closer to the Red Queen in
Resident Evil or the Relic in Cyberpunk 2077: a resident, autonomous guardian
that belongs to the system, watches it, and can act to preserve it. The human
can see that Custos exists and see a deliberately limited status surface, but
cannot command it.

### Design consequences

- **GUI is read-only.** No start/stop/repair/restore/kill/approve buttons and
  no command palette. The human cannot configure Custos or ask it to act.
- **Limited information.** The GUI shows high-level state, integrity rings,
  current protection mode, bounded incident summaries, and action categories.
  It does not expose raw prompts, tool bodies, file contents, paths, SQL,
  provider payloads, stack traces, secrets, or the full audit.
- **Custos is a resident process, not a VerlinPark feature.** It has its own
  binary, launchd supervision, local sealed control channel, and lifecycle.
- **The user is an observer, not an operator.** The only human "override"
  is OS-level custody (quit/disable/uninstall/delete data), outside the product
  GUI. If Custos is wrong, the owner can still stop it at the OS level; the
  product does not provide a polite in-app veto.
- **Custos has real authority.** It may freeze/quarantine/stop/restart/restore
  according to its immutable charter and policies. It does not ask the GUI for
  permission for L2/L3 actions.
- **Custos is deterministic and LLM-distrusting.** No model calls, no API key,
  no natural-language planning, no dynamic code generation. Its messages are
  templated from state.
- **Agents cannot see or call Custos.** It is absent from the agent tool
  surface, model prompts, and task contracts.
- **Immutable charter.** The rules that grant Custos authority are shipped with
  the signed binary/release, not editable from the GUI or by an agent. Policy
  changes require a new signed release and developer review.
- **Custos cannot modify VerlinPark source or policy.** It protects the game;
  it does not rewrite the rules.
- **Custos does not replace truth gates.** It never approves approvals,
  verifies work, merges branches, changes budgets, or edits identities. It can
  stop an unsafe process; it cannot certify correctness.

### What the GUI could show

- a single ◇CUSTOS sigil and a sealed state (`healthy / watch / degraded /
  critical / quarantined`);
- 5–7 integrity dimensions with coarse states (runtime, database, leases,
  queues, worktrees, backups, event freshness);
- a short, bounded "Custos log" of decisions (time, category, action class,
  result) without raw internals;
- a "protection active" indicator and a count of contained incidents;
- a static, deterministic persona line (no chat, no prompt box, no controls).

### Hard questions introduced by this direction

1. If the user cannot operate Custos, who can update its charter? Proposed:
   only a signed release; no runtime UI/agent policy editing.
2. If Custos wrongly quarantines the system, is OS-level kill/uninstall the
   only recourse? Proposed: yes for V1; document it honestly.
3. Does Custos have veto power over user/agent commands that threaten
   integrity? Proposed: yes for a narrow, enumerated set (corruption, unsafe
   restore, runaway process, merge-gate poisoning); it reports the veto.
4. Can the GUI export an incident bundle? If yes, that is an operation. Maybe
   only copy a bounded incident id, not a file/command.
5. Does the user see enough to trust Custos? The tension is deliberate:
   limited information is part of the fiction, but hiding a destructive action
   is unacceptable. Proposed: show categories and consequences, never secrets
   or raw internals.
6. Is the GUI a separate app/window or a sealed panel inside VerlinPark?
   Proposed: a separate ◇CUSTOS window or a read-only panel that cannot issue
   daemon commands.
7. How does Custos prove it is alive/healthy? A heartbeat/status surface and
   launchd supervision; no user ping button.
8. Does Custos get a voice/persona? Proposed: deterministic templated lines
   only; no generative chat, no "personality model".
9. How is Custos prevented from becoming a second root authority? Separate
   process, no provider key, no source writes, immutable charter, signed
   binary, OS-level custody, and a small action allowlist.
10. What is the minimum human-visible incident information that is ethically
    required? This needs a separate "sealed but honest" policy.

## 14. Round 2 direction: sealed authority, notices, and diagnostic export

### 14.1 Charter is fixed

The charter is **not runtime configuration**. It ships with the signed Custos
binary/release, is hash-pinned, and is not editable from the GUI, project
policy, agent tools, SQLite, or environment variables. If the user can edit it,
an agent team could eventually reach the same control surface and the guardian
would stop being a guardian. Changing the charter requires a new signed release
by the developer/owner, outside Custos and outside any agent.

### 14.2 Authority relationship

- Custos authority is **higher than and independent of the user** for
  integrity/health protection.
- The user has **awareness, not jurisdiction**: read-only status, limited
  incident information, and an explicit diagnostic-export action.
- Agents cannot call, query, configure, or impersonate Custos.
- The user can still stop Custos at the OS level (quit/kill/uninstall/delete
  data). That is the accepted last-resort custody, not an in-product control.
- A normal VerlinPark user action or agent action that would damage integrity
  may be vetoed/frozen by Custos, with a bounded notice.

### 14.3 Non-destructive pressure vs stuck work

Custos distinguishes:

1. **Resource pressure / possible false stall** — a process or test is consuming
   excessive CPU/memory/I/O or has exceeded a soft expected duration, but is
   still alive and making some progress.
   - Custos sends a one-way notice to the **responsible agent** (the agent that
     started the process/test, resolved from the process supervisor's
     session/task/agent ownership; daemon-internal work maps to system).
   - The notice says: who Custos is, which process/test is over budget, the
     observed impact, and asks it to stop or adjust.
   - A grace window follows. No kill on the first warning.
2. **Stuck / unresponsive** — no heartbeat/progress beyond the hard threshold,
   process group unresponsive to the normal stop path, or a hard resource cap
   is breached.
   - Custos terminates the process group through the existing supervised
     process API (TERM, bounded grace, KILL).
   - Custos then sends a short post-action notice to the responsible agent.
   - The notice is templated; it contains process/test identity, action, result,
     and consequence, not raw output or secrets.

The notice channel should be a **sealed `custos.notice` system message**, not a
normal agent-to-agent coordination note. It is one-way; agents have no tool to
reply or query Custos. It is delivered at the next safe turn boundary for a
running agent, or to the project inbox for an idle/identity agent. Agents may
act by using their normal process/tool controls; they cannot negotiate with
Custos.

### 14.4 User-initiated diagnostic export

The GUI has one deliberate user action: **generate a diagnostic bundle for a
developer**. It is consent, not control, and it is never automatic/uploaded.

- A secondary sheet/window, not the main Custos status surface.
- One-click generation, local save only; no network, no auto-send.
- Bundle contains detailed device/runtime evidence: OS/hardware/app/daemon
  versions, health snapshot, recent Custos actions, bounded logs, DB/WAL/schema
  state, queue/lease/run summaries, worktree/ref health, crash traces, selected
  incident timeline, redacted config.
- It excludes provider keys, prompts, message bodies, file contents, screen
  content, cookies, and credentials. Paths/payloads are redacted or reduced to
  basenames by default.
- A clear privacy notice and a standard/forensic choice; forensic may include
  more path/state detail after an explicit warning, but still no secrets or
  user content.
- Signed manifest + SHA-256 + report id; developer can verify provenance.
- The main Custos surface still has no repair/restart/restore/approve/kill
  controls.

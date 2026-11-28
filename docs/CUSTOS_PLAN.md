# ◇ CUSTOS — deterministic integrity and repair supervisor

Status: **observe-only foundation implemented** (ADR-0235); **v2 direction
accepted** (ADR-0247): CUSTOS Observer by default and CUSTOS Advanced as a
one-time, local-only root guardian with L2–L4. The current code is still
observe-only; Advanced is not implemented, the ring has not been
installed/verified on a real machine, and release signing/L2–L4 remain open.
Process sampling is read-only shadow evidence and never signals; sealed one-way
notices are delivered at turn boundaries and route to the owning agent/task when
`supervised_process_owners` is present (Leader fallback otherwise); the hash vault
supports optional local Ed25519, pinned-key release-style verification, and
opt-in plist/binary auto-repair; binary repair requests a launchd restart and
is off by default.
Date: 2026-11-26  
Repository: `/Users/apple/orca/workspaces/VerlinPark/m3-round`  
Branch: `Verlintas/m3-round`

> **Round 1 owner direction (2026-11-26):** Custos may have a GUI, but the
> user must have **no operational authority** in it. The GUI is a sealed,
> read-only observatory with deliberately limited information. Custos is a
> resident guardian with real protective power — closer to the Red Queen than
> to an operator console. This direction supersedes the earlier
> "operator-gated" UI assumptions in this draft; see section 13.
>
> **Round 4 owner direction (2026-11-26):** Custos is a role-separated ring of
> processes (core/warden/anchor, plus a fourth liveness sentinel) with a
> watchdog ring and signed-vault self-heal, not a single daemon. Daemon restart
> and verified-backup restore are authorized automatic L4 actions under 2-of-3
> quorum. Process thresholds are capacity-aware and use multi-signal
> `proc_pid_rusage`-based probes. The GUI is an independent window inside the
> VerlinPark app, not a separate `.app`. The owner declined a privileged
> `custos-guard`; same-user residual is accepted. No stack samples or full
> paths in diagnostics. See §16 and §17.
>
> **Round 5 owner direction (2026-11-29):** split the product into **CUSTOS
> Observer** (default, no admin, read-only) and **CUSTOS Advanced** (one admin
> installation, all capabilities at once). Advanced is a local-only Tier C
> guardian: root monitor/executor, `_custos_core/_warden/_anchor/_sentinel`
> independent users and keys, 2-of-3 signed intents, L2–L4, root-owned
> audit/vault, and a local anti-rollback generation anchor. It does not use
> Apple notarization/Developer ID, a local LLM, generative AI, or a network
> dependency. Intelligence is deterministic (multi-signal, statistical,
> causal/counterfactual, expected loss, incident learning); explanations are
> fixed templates. Unattended/large/high-value work is gated on Advanced or an
> explicit acknowledgement of Observer limits. See ADR-0247.

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

## 3. Authority ladder (updated)

The authoritative version is `docs/CUSTOS_CHARTER_V0.md`. Summary:

- **L0 Observe** — read health, status, metrics, logs, DB snapshots; no writes.
- **L1 Notify** — sealed, one-way `custos.notice`; no state mutation.
- **L2 Safe repair** — deterministic, reversible or evidence-preserving state
  repairs: stale leases/claims, interrupted runs, covered projection replay,
  transient queue retry, daemon-owned scratch cleanup, policy-allowed
  supervised process restart.
- **L3 Containment** — pause/cancel a stuck session, stop a known process
  group, quarantine a worktree/branch/merge entry, block project dispatch,
  release a stale GUI lease, mark a project/session read-only.
- **L4 Recovery** — daemon restart, verified backup restore, safe mode,
  terminate all known sessions; only under Article 7 preconditions and 2-of-3
  role quorum.
- **L5 Prohibited forever** — source/policy edit, approval/Verifier/merge
  bypass, budget/cap/model-routing change, identity change, secret access,
  remote-trust change, foreign-process kill, LLM call, agent-facing surface.

There is no operator approval or break-glass token in the product. L3/L4
actions are charter-driven, quorum-gated, and audited; the user sees a bounded
result, not a control. The four-process ring (core/warden/anchor/sentinel) is
described in §16 and the charter.

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

| Failure | Signal | Action | Authority |
|---|---|---|---|
| provider transient/stream | error code, retry count | retry through existing provider path | L2 |
| output truncation | `finish_reason=length` | same-run split recovery (already in runtime) | L2 |
| stale lease | expired heartbeat | reclaim via existing lease API | L2 |
| interrupted run | daemon restart | reattach/resume by existing policy | L2 |
| projection drift | replay mismatch | replay/repair covered projections | L2 |
| queue claim stale | next_attempt_at/age | release/retry claim | L2 |
| worktree missing | path/ref check | rehydrate/reattach or quarantine | L3 |
| merge conflict | queue status | existing opt-in resolver; Custos may quarantine | L3 |
| effect unknown | receipt unknown | freeze merge; never settle | L3 |
| repeated budget pause | completion reserve/usage | notify; no auto top-up | L1 |
| DB corruption | integrity check | freeze writes, verified restore, safe mode | L4 |
| low disk | free space threshold | block dispatch + bounded scratch prune | L3 + L2 |
| daemon crash loop | restart count/window | bounded restart, then safe mode | L4 |
| Git lock failure | stderr/lock path | retry without optional locks; quarantine refs | L2/L3 |
| approval/security error | risk class | never auto-repair | never |

Custos must not "repair" by making a failing test pass, changing policy, or
weakening a gate. It repairs the execution environment and state transitions.

## 6. Architecture options (superseded by §16)

Selected: a **role-separated user-level process ring** (core/warden/anchor),
not an in-daemon module. The original sidecar sketch:

### A. Separate sidecar process (selected direction, expanded to a ring)

- `custosd` runs as separate LaunchAgents/user processes.
- Talks to VerlinPark via a **dedicated local admin socket** (Unix domain
  socket, 0600, separate credential; no TCP by default).
- VerlinPark exposes a bounded read-only `custos.*` admin surface; normal `vp`
  RPC and agent tools do not expose it.
- Custos has no provider key, no model client, no arbitrary shell.
- It can read daemon health/state through RPC and, where necessary, a
  read-only SQLite snapshot if the daemon is down.
- If the daemon is down, the warden can invoke a preconfigured restart/restore
  path with fixed argv after quorum.
- Its supervisor is independent (launchd KeepAlive). Self-heal restores only
  the signed baseline; it never executes generated code.

### B. In-daemon module

Simpler, but a stuck/compromised daemon cannot heal itself; not preferred for
the high-power role.

### C. Hybrid

Small always-on watchdog + separate repair worker. More moving parts; only if
launchd/process reliability proves insufficient.

Open: should Custos be a user LaunchAgent (same user as daemon) or a
root/LaunchDaemon with a dedicated service user? V1 likely user-level, no sudo.

## 7. Policy and authority (superseded by §13–15 and the charter)

The earlier "operator policy / break-glass grant" proposal is superseded. The
current direction is:

- the charter is a fixed, signed release artifact pinned by hash, not runtime
  policy;
- there is no GUI/RPC/agent `policy.set`, no admin mode, and no in-product
  break-glass grant;
- the human can only observe and export diagnostics; the final override is
  OS-level quit/kill/uninstall/delete-data outside the product;
- Custos never has a generic "run this command" capability;
- Custos may always be downgraded by shipping a new signed release, not by a
  runtime switch.

Action classes, caps, cooldowns, and prohibited actions are defined in
`docs/CUSTOS_CHARTER_V0.md`, which is the authoritative draft.

## 8. Audit and evidence

- Append-only local `custos_events`/JSONL with hash chain.
- Each action: action id, charter hash, precondition snapshot, evidence
  pointers, dry-run, result, postcondition, rollback/stop, duration, notice id
  (no operator grant; there is no in-product operator authority).
- No prompts, tool bodies, secrets, model keys, or screen content.
- Bounded retention; export/verify tool.
- Custos actions should be replayable as a state machine; a replay must not
  re-execute side effects.

## 9. Interfaces (updated for sealed authority)

- `custos.status` — health, state, build/charter hash, last probes
- `custos.health` — bounded dimensions/evidence codes
- `custos.history` — bounded, redacted audit
- `custos.diagnostics.generate` — the single user-consented local export action
  (standard + forensic), never upload
- `custos.preview` — internal/developer-only dry-run for testing, not a user
  command surface
- no `custos.act`, `custos.policy.set`, `custos.pause/resume` in the product
  RPC/CLI/GUI; actions are charter-driven and internal
- admin RPC is exposed only on a 0600 Unix domain socket with peer-uid checks
  and a sealed token; normal `vp` RPC/discovery/agent/MCP do not see it
- CLI: a separate read-only `custos`/`◇CUSTOS` binary or a developer
  `custos-status` command; no repair verbs
- GUI: sealed read-only main window plus the secondary diagnostics sheet
- Agent tool bundle: **none**. Agents cannot call, query, or configure Custos.

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
2. Shadow mode: Custos observes and emits sealed recommendations to the audit
   log only; no actions, no user apply.
3. L2 auto mode only for safe classes with tight caps, after fault-injection
   fixtures and shadow evidence agree.
4. L3 containment after shadow evidence; L4 recovery only under the narrow
   charter preconditions.
5. There is no "assisted operator apply" mode; the user has no jurisdiction.
   Developer review happens through signed releases and recorded evidence.
6. No live auto-repair until deterministic fault-injection and replay
   comparisons pass on a recorded corpus.

## 11. Open questions (remaining)

Most earlier questions are answered by §13–15 and the charter. Remaining
decisions for the owner:

Answered in this round: fourth sentinel (yes), no privileged `custos-guard`
(accepted residual), daemon restart (yes, automatic), verified restore (yes,
automatic), GUI (independent window inside the app), adaptive capacity
calibration (yes), no stack samples/full paths in diagnostics. Remaining
decisions are kept in `docs/CUSTOS_CHARTER_V0.md` Appendix D:

1. Are the capacity formulas and score weights acceptable after calibration?
2. Vault/app-bundle baseline vs a signed read-only disk image?
3. Idle-agent notices: project inbox or drop with audit?
4. Quarantine UX: what the user sees.
5. Relation to existing `OrchestrationPolicy`, `IntegrationPolicy`,
   `EffectPolicy`, `RunContract`, backup/anchor.
6. Audit storage/retention; menu-bar presence; naming/language.

## 12. Recommended first slice (for discussion)

Do not start with L4 in production. Start with:

- a read-only core over a local admin socket, with a single-role temporary
  shell (the full four-role ring lands before any mutation);
- the charter-pinned observe mode;
- probes for daemon/DB/WAL/leases/runs/queues/events/backups/Git/known
  processes;
- `custos.status`/`health`/`history`;
- shadow-mode recommendations;
- deterministic fault-injection tests;
- no mutations, no provider key, no agent tool, no auto-restart in this first
  slice.

Then add L2 safe repairs one class at a time with tests and shadow evidence.
Only after shadow mode is boring and accurate should Custos get L3/L4 powers;
the user still has no apply/approve control.

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
4. Can the GUI export an incident bundle? Yes: the secondary diagnostics sheet
   generates local standard + forensic bundles; no upload, no command surface.
5. Does the user see enough to trust Custos? The tension is deliberate:
   limited information is part of the fiction, but hiding a destructive action
   is unacceptable. Proposed: show categories and consequences, never secrets
   or raw internals.
6. GUI packaging: answered as an independent window inside the VerlinPark app,
   like Settings, not a separate `.app` and not an embedded panel.
7. How does Custos prove it is alive/healthy? A heartbeat/status surface and
   launchd supervision; no user ping button.
8. Does Custos get a voice/persona? Proposed: deterministic templated lines
   only; no generative chat, no "personality model".
9. How is Custos prevented from becoming a second root authority? Role-separated
   process ring, quorum for L4, no provider key, no source writes, immutable
   charter, signed binary, OS-level custody, and a small action allowlist.
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
  more state/log detail after an explicit warning, but still no full paths,
  process argv values, stack samples, secrets, or user content.
- Signed manifest + SHA-256 + report id; developer can verify provenance.
- The main Custos surface still has no repair/restart/restore/approve/kill
  controls.

## 15. Round 3 direction: V1 process scope, advisory expectations, dual bundles, self-protection

### 15.1 V1 never manages arbitrary system processes

Custos V1 may inspect and act only on processes the daemon already knows and
owns:

- `supervised_processes` rows (start time + nonce + command hash verified);
- `run_project_task` / test-cache command process groups;
- shell-tool process groups started by an agent run;
- daemon-launched browser/computer/LSP/MCP/ACP stdio children;
- daemon-owned helper and sweeper processes.

It must not scan and kill arbitrary high-CPU system processes. A system-wide
guardian is an **Advanced** concept, explicitly not planned for V1. This keeps
Custos from becoming a second, unaccountable `kill -9` authority on the user's
machine.

### 15.2 Agent-declared expected durations are advisory only

Agents frequently mis-declare timeouts (for example, a one-minute test with a
fifteen-minute timeout). Therefore:

- `expected_secs` / declared timeout is one weak signal, never a kill trigger
  by itself;
- Custos weights observed progress (output growth, CPU-time delta, child
  completion, heartbeat) more heavily than the declaration;
- a declared duration can only *raise* the warning threshold within a bounded
  clamp, not lower protection;
- hard caps are Custos-owned constants from the signed charter, not
  agent-supplied values.

### 15.3 Notices stay one-way

`custos.notice` is sealed, templated, one-way, and cannot be replied to. Agents
have no tool, RPC, mailbox address, or model-visible channel to query, argue
with, or configure Custos. A notice is a fact, not a conversation.

### 15.4 Diagnostic export defaults to two bundles

The secondary diagnostics surface generates **both** a `standard` bundle and a
`forensic` bundle in one action, locally, with no upload. The forensic bundle
adds detail (basename-level process lineage, bounded raw logs, SQLite/WAL/schema
statistics, queue and lease identifiers, capacity/calibration profile) but
still excludes full paths, process argv values, stack samples, provider keys,
prompts, message bodies, file contents, screen content, cookies, and
credentials. The user chooses which bundle to share.

### 15.5 GUI is minimal, hardcore, and visibly separate

The Custos window must not look like a VerlinPark page. It is a sealed
instrument: obsidian black, thin ice lines, a central `◇` sigil, coarse
integrity rings, monospace uppercase labels, restrained glow/pulse
effects, no cards/sidebar/tabs, no repair/restart/restore/approve/kill
controls. Reduce Motion is honored. It is an independent window inside the
VerlinPark app (like the Settings window), not a separate `.app` and not a
panel embedded in the main Stage. The only user action is opening the secondary
diagnostics sheet and generating local bundles.

### 15.6 Self-protection is a design requirement

Agents must not be able to stop, pause, suspend, reconfigure, or impersonate
Custos through any product surface. The only accepted stop is machine
shutdown. V1 therefore needs:

- boot autostart;
- launchd `KeepAlive` supervision;
- a role-separated four-process ring (core/warden/anchor/sentinel) so one
  killed process does not remove all authority;
- a watchdog/heartbeat ring so a dead/stopped role is restored quickly;
- a signed vault/app-bundle baseline with quarantine + overwrite self-heal for
  Custos-owned files;
- no agent tool/RPC/mailbox/model surface that addresses Custos;
- daemon process controls that refuse foreign PIDs and never target Custos;
- signed, hash-pinned charter/binary; no runtime policy editing.

The honest limit: a same-user process with arbitrary shell/`launchctl` can
still signal or boot out a user LaunchAgent. V1 cannot mathematically prevent
that without a separate OS user or privileged LaunchDaemon, which is
Advanced-only. V1 must therefore be **kill-resistant and self-restoring**, and
record the residual same-user attack as a known limit rather than claiming
absolute invulnerability. See `docs/CUSTOS_CHARTER_V0.md` for the concrete
launchd/watchdog design.

### 15.7 Design artifacts produced from this round

- `docs/CUSTOS_CHARTER_V0.md` — draft immutable charter, authority ladder,
  action catalog, pressure/stuck matrix, notice templates, diagnostic bundle
  manifest, GUI information tiers, and launchd/watchdog self-protection.

These artifacts are **design drafts for review**, not implemented behavior.

## 16. Round 4 direction: multi-process ring, self-heal, and conservative probes

### 16.1 Multi-process Custos, not a single daemon

Custos is a role-separated ring of four user LaunchAgents:

- `com.verlintas.custos-core` — probes, scoring, decisions, action intents;
- `com.verlintas.custos-warden` — executes L3 stops/quarantine/restart and L4
  recovery after quorum;
- `com.verlintas.custos-anchor` — self-heal, hash/plist/charter/audit-chain
  verification, vault restore, peer restart, backup verification;
- `com.verlintas.custos-sentinel` — liveness-only watchdog; heartbeats and
  launchd kickstart/bootstrap of the other three, no product authority.

Each has a separate process group, heartbeat, launchd label, and subset of
authority. Killing one does not give an attacker the others' authority; the
remaining roles detect the stale heartbeat and restart it. L4 actions require
2-of-3 signed intents from core/warden/anchor; sentinel does not vote. L3 is
core+warden with anchor audit; a hard-cap stop can be executed by the warden
alone. A monotonic epoch lease prevents split-brain.

### 16.2 Self-heal / overwrite authority

Custos keeps a signed baseline in two places: the app bundle's read-only
`Contents/Resources/Custos/` and a 0500/0400 vault under Application Support.
On hash/signature/plist/charter/audit-chain mismatch it:

1. quarantines the tampered file (0400, hash recorded);
2. restores from the vault, verifying hash/signature;
3. falls back to the app bundle;
4. restarts the affected role via launchd;
5. enters `DEGRADED_SELF` and refuses L3/L4 if both baselines are bad.

The overwrite is limited to Custos-owned files. It never overwrites VerlinPark
source, project policy, user documents, worktrees, or database rows. Audit-chain
tamper is recorded as an explicit gap; missing entries are not fabricated.

### 16.3 Daemon restart and verified restore are authorized

- `REC-01` daemon restart is automatic with 2-of-3 quorum and the Article 7
  preconditions; crash loops stop after the cap and enter safe mode.
- `REC-02` verified backup restore is automatic with 2-of-3 quorum: DB
  unopenable/corrupt, backup verifies, anchor generation acceptable, fresh
  directory on the same volume, corrupt original preserved, atomic rename swap,
  post-restore integrity check. One attempt per failure fingerprint; a second
  failure enters safe mode.

### 16.4 Capacity-aware, calibration-based process probe

The old fixed CPU-percent thresholds were both too easy to hit on a small
machine and too high on a large one. The new design is deterministic but
machine-aware and workload-aware:

- **Capacity profile.** Read `hw.memsize`, logical/physical/P-E cores, disk
  free, swap, memory pressure, thermal state, low-power mode, load average, and
  daemon latency. Derive `rss_hard = clamp(0.50×mem, 3, 24 GiB)`,
  `children_hard = clamp(16×cores, 64, 256)`,
  `scratch_hard = clamp(0.05×disk_free, 2, 24 GiB)`,
  `fds_hard = clamp(1024×cores, 4096, 32768)`, and normalize CPU to
  `cpu_share = cpu_time/wall/cores`.
- **Workload calibration.** For each `project + tool/profile + command hash +
  label`, keep the last 20 completed runs (duration/CPU/RSS/children/scratch).
  After 5 samples, `expected_effective = p95 × 2` with median/MAD outlier
  rejection, then apply a thermal/low-power capacity multiplier. Fewer than 5
  samples fall back to the declared time; the first 5 runs are learning mode
  (notices only, except hard caps or 15-minute no-progress + ignored TERM).
  Agent declarations cannot override observed history.
- **Score.** A 0–100 deterministic score combines normalized resource,
  progress, and control families. Notice at 60, final at 80, stop at 90 with
  two independent signal families. Any progress in the last 60s blocks a kill
  except an effective hard cap. Hard caps are the capacity-derived values, not
  fixed 12 GiB/128/8 GiB/8192 constants; they tighten under memory/disk
  pressure and use hysteresis.
- **No stack sampling.** The forensic bundle and the decision path do not use
  `sample`, thread stacks, memory dumps, full paths, or argv values.

### 16.5 GUI is an independent window, not a separate app

The `◇CUSTOS` surface is a separate window scene inside the VerlinPark app, like
the Settings window. It is not a separate `.app` and not a panel embedded in the
main Stage. It connects to the read-only Custos admin socket, shows the sealed
status instrument, and has the secondary diagnostics sheet. Closing it does not
stop Custos. The window has no repair/restart/restore/approve/kill controls.

### 16.6 Residual risk remains explicit

The ring and self-heal make Custos kill-resistant and tamper-evident, but they
are not a consensus system against root or an attacker who can tamper every
baseline copy. A same-user shell can still signal processes or `launchctl
bootout` user agents; launchd/ring restores them, and the GUI shows `SIGNAL
LOST` during the gap. The owner has declined an optional privileged
`custos-guard` for V1, so this residual is accepted rather than hidden.
Absolute protection would require a separate OS user or privileged LaunchDaemon
and remains Advanced-only.

## 17. Round 5 direction: sentinel, no privileged guard, adaptive capacity

### 17.1 Fourth process: sentinel

Added `com.verlintas.custos-sentinel` as a **liveness-only** fourth role. It has
no probes, no decisions, no product actions, and no L4 vote. It only checks the
heartbeats of core/warden/anchor and uses launchd kickstart/bootstrap to
restore a dead role. This raises the cost of killing two roles and gives the
ring a dedicated last-survivor restart path. If the sentinel dies, the other
three restart it.

### 17.2 No privileged guard

The owner declined the optional root `custos-guard`. V1 remains a no-sudo,
user-level ring. Same-user `kill`/`launchctl bootout` is a documented residual:
launchd/ring restores Custos, the GUI shows `SIGNAL LOST`, and no authority is
transferred to the attacker. Absolute protection would require a separate OS
user or privileged LaunchDaemon and is Advanced-only.

### 17.3 Adaptive capacity instead of universal constants

Fixed thresholds were not intelligent enough across machines. The charter now
defines a deterministic two-layer model:

1. **Capacity normalization** — derive caps from physical memory, logical/P/E
   cores, disk free, swap/memory pressure, thermal state, low-power mode, and
   daemon latency. For example, `rss_hard = clamp(0.50×mem, 3, 24 GiB)` and
   `children_hard = clamp(16×cores, 64, 256)`; CPU is normalized to
   `cpu_share = cpu_time/wall/cores`.
2. **Workload calibration** — for each project/tool/command fingerprint, keep
   the last 20 completed runs and use `p95 × 2` with median/MAD outlier
   rejection after 5 samples. Thermal/low-power states apply a bounded
   multiplier. The first 5 runs are learning mode (notices only, except
   effective hard caps or 15-minute no-progress + ignored TERM). Agent-declared
   time is only a sparse-history fallback and cannot override observed history.

The result is still deterministic and local-only: no LLM, no cloud, no agent
editing. It adapts to an 8 GiB laptop, a 64 GiB workstation, a cold cache, and
a thermally throttled run without letting a bad baseline hide a real stall.
Caps use hysteresis and tighten under current memory/disk pressure.

### 17.4 Diagnostics stay basename/metadata-only

The forensic bundle no longer includes stack samples, thread stacks, memory
dumps, full absolute paths, or process argv values. It adds basename-level
process lineage, bounded raw logs, SQLite/WAL/schema stats, queue/lease ids,
launchd/watchdog history, and aggregate calibration p50/p95. The decision path
also does not use `sample`; it relies on `proc_pid_rusage`,
`proc_pidinfo`, output/heartbeat progress, and capacity calibration.

### 17.5 Decisions after this round

Accepted: capacity formulas and 5-run learning mode; 0500/0400 vault +
app-bundle baseline; idle notices to project inbox; quarantine UX
(project/task label, category, consequence, diagnostics entry); menu-bar
diamond status item; no stack samples/full paths.

Remaining:

- Wordmark animation: idle is glow-only; see
  `docs/CUSTOS_WORDMARK_ASCII.md`.
- Audit-chain tamper handling: gap marker vs restored head.
- Calibration drift: distinguish a permanently slow machine from a real stall
  without letting a poisoned baseline hide regressions.
- Whether calibration p50/p95 appears in the forensic bundle as aggregates.
- Whether the menu-bar diamond shows a `CONTAINED` count.

## 18. Round 6 direction: accepted calibration and wordmark art

The owner accepted the capacity model, the 5-run learning window, the
vault/app-bundle self-heal baseline, and inbox delivery for idle-agent notices.
The quarantine display and menu-bar diamond status item are also accepted.

**Custos is a normal GUI window.** Only the `Custos` wordmark is ASCII/character
art; everything else is ordinary SwiftUI:

- the **large diamond** is a drawn SwiftUI shape at the center of the window,
  not an ASCII diagram;
- the **`Custos` wordmark** is the only monospaced character-art element, with
  no small diamond before it;
- integrity dimensions, counts, ring state, and the bounded log use normal
  labels/indicators, not box drawing or ASCII bars;
- the window has no operational controls.

`docs/CUSTOS_GUI_ASCII.md` is **discarded exploration** and must not be
implemented. `docs/CUSTOS_WORDMARK_ASCII.md` holds the accepted four-row
wordmark and its animation:

- boot: an 8-frame print-head reveal, not a single flash;
- idle: completely static letters with a ±8% glow breath; no line, no cursor,
  no character movement;
- `CRITICAL`: a 7-frame multi-row jitter (~580 ms at 12 fps), at most two
  cycle, with no scanline;
- `QUARANTINED` / `SIGNAL LOST`: static, no glow.

The wordmark is implemented as a monospaced text layer with a static fallback,
not as an image asset. The rest of the window is normal SwiftUI.

## 19. Implementation status: observe-only slice

ADR-0235 adds `crates/vp-custos` with `custosd` and `custos`:

- pinned draft charter/hash and a closed v0 action catalog;
- capacity normalization and a pure process-pressure score with learning mode;
- read-only `sysinfo` sampling of `supervised_processes` with shadow
  pressure/stop evidence; no signal or notice;
- four-role heartbeat files, quorum helper, support loops, and a peer watchdog;
- explicit user LaunchAgent install/status/uninstall without sudo;
- read-only SQLite probes (no `Store::open`, no schema migration);
- 0600 Unix socket: `status`, `health`, `charter`, `ping`,
  `diagnostics.generate`;
- content-addressed vault: `--vault-init`, `--vault-verify`,
  `--vault-repair --yes`, optional local Ed25519 (`--vault-keygen`,
  `--vault-public-key`, `--vault-init --signing-key`), pinned
  `--vault-pinned-key` verification, opt-in `--vault-auto-repair` for plists and
  `--vault-auto-repair-binary` for binary+launchd restart, with quarantine
  before restore;
- hash-chained JSONL audit (`--audit-verify`) for start/stop and automatic 30s
  vault-state changes; metadata only;
- sealed one-way notice outbox and turn-boundary agent delivery, with
  `reply: forbidden`; `supervised_process_owners` targets the owning agent/task
  and falls back to the session Leader for legacy/unowned processes;
- local standard + forensic ZIP bundles with a SHA-256 manifest, no upload, and
  no caller-supplied path;
- separate `custos/custos.discovery.json` (0600), not the daemon token;
- no mutation method, token, TCP listener, agent tool, or provider key.

Tests: 66 unit + 4 integration + 1 module-governance; full Rust workspace
4,781 passed / 0 failed / 6 ignored; full Swift suite 673 passed / 0 failed.
The SwiftUI window is a normal GUI with the `Custos` wordmark as the only ASCII
art: graphical diamond, nine dimension rows (including vault/process shadows), counts, ring line, local
diagnostics sheet, and a read-only menu-bar item that only opens/closes the
window.

Still open before mutation authority:

- install/verify the four-role launchd ring on a real machine;
- add peer-uid/token checks and the signed/automatic self-heal path;
- add exact task-scoped notice wording for Labour/Worker and verify on a live run;
- ship a real release key and signing workflow (the vault supports pinned
  verification and opt-in plist/binary repair);
- validate thresholds against shadow-mode evidence;
- implement L2/L3/L4 action execution and audit chain.

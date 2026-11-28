# 0247 — CUSTOS v2: local deterministic guardian, Observer and Advanced

- Status: Accepted (owner direction)
- Date: 2026-11-29
- Scope: CUSTOS product split, local trust model, diagnosis/action model,
  distribution constraints
- Supersedes: the same-user four-role-only direction in ADR-0235 for the
  Advanced tier. The observe-only foundation remains valid and is retained as
  CUSTOS Observer.
- Depends on: ADR-0235 (CUSTOS observe-only foundation), ADR-0161/0187
  (runtime leases), ADR-0232 (run journal), v3 backup/anchor

## Context

The observe-only foundation is deliberately local and read-only. It is useful
as an observer but is not a guardian: all roles are user LaunchAgents, the
heartbeat JSON is unauthenticated, the local vault and Ed25519 seed are
user-readable, and the 2-of-3 quorum is only over fresh files in the same data
directory. A same-user process can kill or replace it. L2–L4 were design-only.

The owner direction is now:

- No Apple Developer Program, no notarization, no public distribution.
- CUSTOS must be strong enough for large and unattended VerlinPark projects.
- The configuration must be humane: first-run users get an Observer, and
  Advanced must install all capabilities at once instead of asking users to
  gradually enable protection after the risky project has already started.
- The core must run fully on-device, with no generative AI/local LLM and no
  network dependency. Network may only be an optional security anchor later;
  it is not part of the default authority path.
- “Intelligent” means deterministic multi-signal diagnosis, causal/
  counterfactual reasoning, uncertainty, expected-loss decisions, and incident
  learning — not a model deciding actions.

## Decision

### 1. Product split

- **CUSTOS Observer** — default, no admin. Same deterministic diagnosis engine,
  but observe/notify/shadow only. It explains itself on first run and detects
  when a project becomes large, high-value, or unattended.
- **CUSTOS Advanced** — one explicit admin installation, all capabilities on by
  default: local Tier C guardian with L2–L4, root-owned audit/vault, safe mode,
  and verified-backup recovery. The main flow has no per-feature L2/L3/L4
  switches; expert custom policy is a separate disclosure.
- **Hard gate.** Before the first unattended run, scheduler auto-dispatch,
  large plan, or high-value project, Observer shows the Advanced check. The
  operator either installs Advanced or explicitly acknowledges Observer's
  limits; high-value projects require Advanced. This prevents “it is too late
  after the project started”.

### 2. Threat model and trust

CUSTOS v2 protects against **AI loss of control, prompt injection, agent
mistakes, daemon bugs, resource exhaustion, and state/worktree corruption**. It
does not protect against root/admin, kernel compromise, or Apple supply-chain
trust. Those are explicit non-goals.

Advanced uses a root-owned local trust domain, not Apple:

- root monitor and a minimal root executor LaunchDaemon;
- `_custos_core`, `_custos_warden`, `_custos_anchor`, `_custos_sentinel`
  independent users with independent Ed25519 keys;
- 2-of-3 signed intents from core/warden/anchor; sentinel is liveness-only;
- root executor validates signatures, allowlist, target identity
  (`pid+start time+code signature`, commit, lease generation), idempotency,
  expiry, and fencing;
- root-owned audit, vault, public keys, and local monotonic generation anchor;
- no shell, no arbitrary kill, no TCP, no inbound control plane;
- explicit admin installation; ad-hoc/source distribution; Gatekeeper warning;
  no notarization, no SMJobBless, no EndpointSecurity entitlement.

The child roles run as separate users so a normal agent/daemon process cannot
read their private keys or signal them. Root remains outside the guarantee.

### 3. Intelligence without generative AI

The authority plane is pure deterministic code:

- multi-source evidence with quality tiers (OS, daemon, DB, agent-reported,
  heuristic); agent-only evidence never triggers an action;
- project/session/agent/task/run/process/worktree/merge/effect/lease graph;
- statistical baselines (EWMA, z-score, CUSUM/change-point, Poisson, survival)
  and bounded graph analysis;
- differential diagnosis with supporting/contradicting evidence, confidence,
  alternatives, uncertainty (`insufficient_evidence`, `conflicting_evidence`),
  and next probes;
- counterfactual dependency closure before an action;
- expected-loss decision (`P(hypothesis) * false-positive loss + (1-P) *
  inaction loss + action cost`), graduated response, cooldown, and quorum;
- local incident memory, shadow comparison, and calibration; policy updates
  only by signed release or explicit admin apply, never online self-modification.

Explanations are short fixed templates with evidence references, not generated
prose. No provider key, no model client, no local LLM, no dynamic code.

### 4. Action model

Every action in the catalog declares preconditions, evidence requirements,
expected benefit, false-positive/inaction cost, blast radius, reversibility,
rollback, cooldown, quorum, visibility, and postcondition probes.

- L2: stale lease, interrupted run, covered projection replay, transient queue
  retry, daemon-owned scratch cleanup, safe bounded restart.
- L3: pause/cancel a stuck session, stop a known process group, quarantine a
  worktree/merge entry, release a stale GUI lease, freeze dispatch, read-only
  project/session.
- L4: daemon restart, verified backup restore, safe mode. Requires 2-of-3,
  fresh local anchor, verified backup, and post-restore integrity checks.

No source/policy/approval/Verifier/merge/budget/model-routing edits. No
operator per-action approval; the operator configures/installs and can enter
safe mode/uninstall with explicit admin consent.

### 5. Compute

No hard CPU/RSS gate. The only hard constraint is no LLM-class compute. The
implementation uses incremental `libproc`/launchd/FSEvents sampling, bounded
event windows, and incremental DB cursors; `custos bench` measures idle,
10-session, 100-task, event-storm, and 500-process scenarios so CUSTOS does not
starve VerlinPark. Short incident-time CPU bursts are acceptable.

### 6. Network

Default: **fully local, no network**. The core authority, audit, vault,
quorum, and actions must work offline. An optional outbound-only security anchor
may be added later for audit/anti-rollback evidence, but it is not part of
Advanced defaults, cannot send actions, cannot widen policy, and has no inbound
control plane. No prompts, source, command text, or file content may be sent.

### 7. Distribution

No Apple Developer Program and no notarization. Local source/ad-hoc DMG,
explicit admin install (`sudo`/Authorization), checksum verification, manual
update, and a documented Gatekeeper bypass. This is a local trust model, not an
Apple trust claim.

## Consequences

- CUSTOS Advanced is significantly stronger than the current observe-only
  foundation while remaining local-only.
- Root-owned state and independent role users remove the same-user tamper path
  for normal agents; root/admin can still win.
- No Apple fee, but Gatekeeper warnings and no Apple supply-chain trust.
- No EndpointSecurity: monitoring is root polling/`libproc`/FSEvents and can
  miss short-lived events; this is documented rather than hidden.
- No network anti-rollback; local generation/audit anchors protect against
  non-root rollback, not admin/root rollback.
- Observer is still only an observer; its limits are visible and gated before
  risky unattended work.
- L2–L4 remain unimplemented until their action-specific fault injection,
  shadow evidence, and rollback tests pass.

## Implementation phases

1. Documentation/threat model (this ADR; CUSTOS plan/charter updates).
2. Phase 1 (no root): Observer/gating model, agent `kill`/`launchctl` capability
   lockdown, CUSTOS path sandboxing, `custos bench`.
3. Phase 2: root monitor/executor + three role users/keys + 2-of-3 +
   root-owned audit/vault; read-only self-test.
4. Phase 3: diagnosis engine + action catalog + shadow; first L2 action.
5. Phase 4: L3/L4 + local anti-rollback + fault injection (kill roles, forged
   intent, audit/vault tamper, restart/unload, rollback, disk pressure).
6. Phase 5: Advanced all-on installer, safe mode/uninstall, local
   distribution/update, real-machine drill.

## Tests

Phase 1: permission tests for blocked process signaling/`launchctl`, shell
secret-root coverage for `custos/`, and an integration denial test. Later
phases add action-specific deterministic tests and fault injection; the current
observe-only tests remain.

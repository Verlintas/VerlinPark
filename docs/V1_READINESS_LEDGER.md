# VerlinPark V1 readiness ledger

- Status: Active
- Date: 2026-11-23
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
| V1-D03 | S1 | A raw-shell git commit produced an `EFFECT_UNKNOWN` receipt that blocked integration. The agent has no reconciliation tool, so the Leader retried with a new manual run and burned the remaining budget; the operator had to reconcile through the CLI. This is fail-closed C2 behavior, but the recovery path is expensive and not guided. | **OPEN** | Run C: receipt `shell:cmd-a2047bef607b`, `EFFECT_UNKNOWN`, task `impl-calc-pow-mod`; reconciled with commit `2d47d49` but the session failed at 1,456,885 tokens |
| V1-D04 | S2 | A plan Worker row can remain `running` after its provider run completes while the task is committed, verified, and merged. Messaging is now safe, but status surfaces still show the Worker as running. | **OPEN** | Run A: Worker run completed at 04:34:45, row still `running` at 04:40; ADR-0232 hardening |
| V1-D05 | S2 | Session budget exhaustion during post-failure recovery has no automatic pause/escalation for `EFFECT_UNKNOWN` or repeated integration blocks. Run C failed at the hard budget with no merged result. | **OPEN** | Run C totals above; the task error names the reconciliation requirement, but the Leader's recovery still consumed the allowance |
| V1-D06 | S2 | The GUI direct-message path to a **running Worker** and the idle **identity** inbox path were not both captured live in the same round. | **OPEN (verification)** | CLI route and Swift/engine tests pass; acceptance scenario 8 still needs the manual GUI pass |
| V1-D07 | S2 | A plan Worker's first-class run can be committing/verifying while the agent row is shown as `running`; the UI has no distinct "verifying/merging" state. | **OPEN** | Overlaps V1-D04; the team graph showed a Worker "RUNNING" with no provider activity |

## Carried findings from the V1 plan

The following groups are carried from [V1_FINAL_PLAN.md](V1_FINAL_PLAN.md) §6.
They stay `OPEN (carried)` until a dedicated audit records evidence. This
ledger does not claim they are fixed.

- **Data safety/security**: tool-secret redaction; safe shell/fetch defaults;
  backup-verify cap/anchor; backup scope warning; remote ACP cleartext/headers;
  invalid approval mode fail-open; native computer/Code Mode experimental
  gating; schema downgrade guard.
- **Core runtime**: Auto Budget across TeamPlan Workers/Labour; in-run plan
  revision; db-rebuild silent skips; imported archive repair semantics;
  backup-verify/source rebuild regression tests.
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

Rust targeted final (after the last steering guard):

```bash
cargo test -p vp-core --lib steering
# 8 passed, 0 failed (includes the 7 operator-route tests and the inspector test)
cargo test -p vp-daemon --bin verlinpadd tests::agent_operator_rpc_tests
# 1 passed, 0 failed
cargo test -p vp-core --lib engine::operator::tests
# 7 passed, 0 failed (run before the final shared-guard edit; same assertions)
```

An earlier full `cargo test --workspace` run passed every other crate and
failed only the daemon operator RPC test, which asserted the old steering
behavior. That test was updated and passed in the final targeted run. The final
full-workspace rerun was blocked by a host problem, not a code failure:
`syspolicyd` entered a sustained ~530–546% CPU spin and newly built binaries
stalled in `_dyld_start` (including a minimal `/tmp` Rust binary). The host
needs its policy service restarted/rebooted before the final full-suite number
can be recorded. `cargo clippy --workspace --all-targets -- -D warnings`
passed after the `has_live_agent_run` refactor and before the last shared
steering guard; that guard compiled and passed in the targeted tests above.

Swift:

```bash
cd apps/macos && xcodegen generate
xcodebuild -project VerlinPark.xcodeproj -scheme VerlinPark \
  -destination 'platform=macOS,arch=arm64' \
  -only-testing:VerlinParkTests/RuntimeReliabilityTests test
# 31 passed, 0 failed
```

Live provider smoke (key from the daemon environment/Keychain only):

```bash
vp --data-dir "$TEMP_DATA" provider-test
```

The live runs are recorded above. They are not a substitute for the full
Rust/Swift suites or for the remaining acceptance scenarios.

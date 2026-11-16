# 0223 — Core module split (`agent` and `engine`)

- Status: Accepted
- Date: 2026-11-16
- Scope: M3.2 structural governance

## Context

`crates/vp-core/src/agent.rs` (29,496 lines) and `crates/vp-core/src/lib.rs`
(24,074 lines) had become the two largest source files in the repository.
They mixed the run loop, tool dispatch, provider plumbing, plan execution,
leases, GUI policy, projects, sources, sessions, replay, merge, usage, and
memory. Navigation and review cost were high, and unrelated changes collided
in the same file.

This is a **mechanical structure change**, not a behavior change: SQL, public
APIs, event names, prompts, bounds, approval ordering, provider policy, and
error text remain the same.

## Decision

### `agent.rs` → `agent/`

`agent.rs` remains the parent module for shared runtime/control state:
`AgentConfig`, `CapacityController`, `ScopeLocks`, `AgentService`, `RunControl`,
`AdmissionGate`, roles/profiles/specs, `SessionBudget`, reservations,
`LoopOutcome`, and shared helpers.

Domain modules:

| Module | Lines | Responsibility |
|---|---:|---|
| `context.rs` | 2,765 | compaction, prompt sections, role prompts, skills, tool bundles, conversation loading, todos |
| `delegation.rs` | 1,456 | ACP delegation, delegate parsing/write scopes, parallel worker delegation |
| `dispatch.rs` | 1,164 | tool authorization/effect gates, shared events, MCP dynamic tools, parallel tool execution, approval/question waits |
| `merge.rs` | 1,437 | merge queue, remote bundle import/integration, verifier checkout/verification |
| `plan.rs` | 2,495 | plan validation, roundtable, worker/labour finish, task memory/provenance, batch selection, TeamPlan execution |
| `provider.rs` | 360 | provider stream retry/consume/usage plumbing |
| `run_loop.rs` | 4,822 | top-level run loop, turn handling, resume, preemption/identity helpers |
| `schemas.rs` | 960 | built-in tool schemas and role/tool schema assembly |
| `scout.rs` | 607 | Scout parsing, briefs, usage, execution |
| `script.rs` | 483 | Code Mode script execution and inner-call routing |
| `service.rs` | 3,951 | `AgentService`: constructors/config, merge queue, programs, intake, continuations, scaling, scheduler, auto-resolve |
| `service_run.rs` | 3,298 | `AgentService`: run start/resume/cancel, checkpoints/forks, plan ops, cache, compaction/context, preemption, pause/resume |
| `support.rs` | 161 | token estimation/calibration, list/truncation helpers |
| `tools.rs` | 2,911 | coordination/team/budget/task tools, tool search/help, artifacts/archive/media, MCP, skill/source/memory/mailbox tools |
| `tests.rs` | 25,821 | unchanged test module |

`agent.rs` is **2,738 lines**. Moved items are `pub(super)` only where sibling
access is required; no external `pub` API was widened. The parent keeps private
glob re-exports so existing sibling `use super::*;` call sites remain valid.

### `lib.rs` → `engine/`

`lib.rs` keeps crate docs/module declarations, imports, the `Engine` definition,
`WorkspaceRunLocks`, `RunLeaseHeartbeat`, constructors/common accessors,
essential re-exports, and `#[path = "tests.rs"] mod tests;`. It is **479
lines**. Private `Engine` fields remain accessible because the modules are
descendants of the crate root. `crates/vp-core/src/tests.rs` is unchanged.

Domain modules:

| Module | Lines | Responsibility |
|---|---:|---|
| `mod.rs` | 30 | submodule declarations + shared parent prelude |
| `leases.rs` | 683 | session/runtime/computer lease guards and methods |
| `gui.rs` | 367 | GUI policy, actions, timeline, retention, helper/status |
| `projects.rs` | 748 | projects, cache/shell/process/preemption/tool-output/integration/scheduler policies, LSP, model presets, project budgets/usage |
| `intake.rs` | 414 | task inbox CRUD/claim/settle/retry/cancel and intake plans |
| `programs.rs` | 356 | programs, milestones, replay, tick |
| `sources.rs` | 2,618 | sources, imports, retrieval/embeddings/ANN, collections, skills, citations |
| `sessions.rs` | 818 | session creation/lifecycle, worktree, diff, transcript, search, context, messages |
| `checkpoints.rs` | 1,558 | session/context checkpoints, retention, fork/lineage/compare |
| `plans.rs` | 2,738 | team plans, revisions, tasks, steering, continuations, stalls, orchestration, scaling/governor |
| `runs.rs` | 3,245 | run journal/replay/repair/import/export/rehydrate/diagnostics and replay projections |
| `merge.rs` | 1,151 | merge queue, verifier/integration, commit/discard/land, PR/forge |
| `usage.rs` | 1,319 | usage/cost/cache telemetry, instructions, prompt layout |
| `memory.rs` | 1,955 | project and identity memory, conflicts, embeddings |
| `artifacts.rs` | 623 | artifact publish/version/diff/materialize/policy/GC |
| `agents.rs` | 1,249 | agents, identities, worktree bindings, assignments, inbox, messages |
| `approvals.rs` | 799 | approvals, grants, templates, attention policy, audit |
| `collaboration.rs` | 337 | A2A, progress ledger, questions, mailbox |
| `integrations.rs` | 551 | MCP/ACP servers and tools, MCP pin trust |
| `surfaces.rs` | 506 | browser/computer/console/process/workspace/git/terminal |
| `effects.rs` | 639 | contracts, effects, intent decisions, MCP pin status |
| `worktrees.rs` | 261 | worktree policy, retention sweep, cleanup |
| `support.rs` | 743 | shared helpers: verification, replay, CSV, git/forge, tool activity |

The module tree totals 24,187 lines (original 24,074; +113 from module headers
and `impl` wrappers). All 597 original `impl Engine` methods and 51 tail free
functions appear exactly once; only visibility qualifiers and module wrappers
differ.

## Invariants

- No SQL/event/prompt/approval/provider policy change.
- No public or `pub(crate)` signature change; moved private items become
  `pub(super)` only as required by sibling access.
- No new `unwrap()`/`expect()` in library code.
- Tests are not rewritten; visibility-only changes are allowed.
- No hidden behavior change may be masked by a passing test.

## Verification

- `cargo fmt --all -- --check` — passed
- `cargo check -p vp-core --all-targets` — passed
- `cargo clippy --workspace --all-targets -- -D warnings` — passed
- `cargo test -p vp-core --lib` — 2,442 passed / 0 failed / 2 ignored
- `cargo test --workspace` — 4,231 passed / 0 failed / 6 ignored
- `crates/vp-core/tests/module_governance.rs` — 3 passed
- Swift unchanged at 535 passed (403 app + 132 helper); no Swift files changed.

## Limits

- This is relocation, not a domain redesign. Some modules remain large.
- Parent glob re-exports and `pub(super)` visibility are more permissive inside
  the tree than an ideal design; they preserve call sites without behavior
  changes.
- `engine/mod.rs` and `agent.rs` act as shared preludes; future name collisions
  could surface as ambiguity and must be resolved explicitly.
- `checkpoint_from_event` remains `pub(crate)` inside `engine::checkpoints`
  without a root re-export; it has no callers outside that module today.
- Module boundaries are mechanical clusters, not a claim of perfect cohesion.
- No runtime/performance change is claimed.

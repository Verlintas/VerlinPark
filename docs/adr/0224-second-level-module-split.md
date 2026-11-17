# 0224 — Second-level module split

- Status: Accepted
- Date: 2026-11-17
- Scope: M3.3 structural governance

## Context

M3.2 split `vp-core/src/agent.rs` and `vp-core/src/lib.rs` into domain
modules. Several domain modules are still large enough to make review and
navigation expensive:

- `agent/run_loop.rs` — 4,822 lines
- `agent/service.rs` — 3,951 lines
- `agent/service_run.rs` — 3,298 lines
- `engine/runs.rs` — 3,245 lines
- `engine/plans.rs` — 2,738 lines
- `engine/sources.rs` — 2,618 lines

This is still a **mechanical structure change**, not a behavior change.

## Decision

Split each of those six files into a smaller parent plus child modules under
a same-named directory (for example `agent/run_loop/turn.rs`). The parent keeps
imports, shared types/helpers, module declarations, and a small amount of glue.
Child modules own cohesive method clusters.

### Agent modules

`agent/run_loop.rs` is **18 lines**:

| Module | Lines | Responsibility |
|---|---:|---|
| `run_loop/agent.rs` | 376 | `run_agent` |
| `run_loop/body.rs` | 1,954 | body setup, provider-turn loop, parallel-tool path, turn finalization |
| `run_loop/entry.rs` | 118 | journal wrapper/finish |
| `run_loop/governor.rs` | 103 | budget-stop/deferral events |
| `run_loop/identity.rs` | 25 | run identity resolution |
| `run_loop/plan_resume.rs` | 255 | plan resume |
| `run_loop/preemption.rs` | 140 | queued/terminal preemption |
| `run_loop/tool_calls.rs` | 1,923 | serial tool execution |

`agent/service.rs` is **10 lines**:

| Module | Lines | Responsibility |
|---|---:|---|
| `service/auto_resolve.rs` | 170 | auto-resolve watcher/scan |
| `service/constructors.rs` | 421 | constructors/config/info/bundle catalog/probe |
| `service/continuations.rs` | 250 | continuation reconciliation |
| `service/intake.rs` | 730 | intake dispatch/reconcile/cancel/retry/dependencies, scheduler status |
| `service/merge.rs` | 1,894 | remote integration, merge retry/resolve |
| `service/programs.rs` | 248 | program reconcile/cancel/milestone start |
| `service/scaling.rs` | 104 | scaling caps/status/tick/controller |
| `service/scheduler.rs` | 158 | task scheduler loop |

`agent/service_run.rs` is **13 lines**:

| Module | Lines | Responsibility |
|---|---:|---|
| `service_run/start.rs` | 707 | start/resume/cancel/spawn/queued runs |
| `service_run/checkpoints.rs` | 343 | checkpoint/fork/merge-fork |
| `service_run/operations.rs` | 88 | manual operation guards |
| `service_run/plan.rs` | 86 | plan revision/restore/edit/resume |
| `service_run/stall.rs` | 275 | stall recovery |
| `service_run/cache.rs` | 212 | cache plan/warm |
| `service_run/tool_bundle.rs` | 413 | bundle inspect/history/diff |
| `service_run/compaction.rs` | 219 | manual compaction |
| `service_run/context.rs` | 576 | configured agent, preamble, context checkpoints/restore |
| `service_run/plan_run.rs` | 240 | dispatch next task/start plan run |
| `service_run/preemption.rs` | 172 | cancel/preempt/pause/resume |

#### Additional tail modules

`agent/tools.rs` is **14 lines**:

| Module | Lines | Responsibility |
|---|---:|---|
| `tools/integrations.rs` | 1,273 | MCP/source/skill/mailbox/memory tools |
| `tools/coordination.rs` | 688 | coordination/team/status/budget/task tools |
| `tools/discovery.rs` | 560 | tool search/help/tool-output paging |
| `tools/artifacts.rs` | 399 | artifact/archive/media/document tools |

`agent/context.rs` is **36 lines**:

| Module | Lines | Responsibility |
|---|---:|---|
| `context/compaction.rs` | 863 | compaction/trim/summaries |
| `context/prompts.rs` | 706 | prompt sections/role prompts/charters |
| `context/bundles.rs` | 554 | tool bundle/prompt/cache metadata |
| `context/state.rs` | 520 | structured context state/todos/conversation loading |
| `context/support.rs` | 120 | bounded shared helpers |

After this tail split, the largest `vp-core` domain module is `agent/plan.rs`
at 2,495 lines; the recursive governance budget remains 3,000 lines.

### Engine modules

`engine/runs.rs` is **138 lines**:

| Module | Lines | Responsibility |
|---|---:|---|
| `runs/support.rs` | 747 | `load_replay_data`, expected/same helpers, replay expectations |
| `runs/replay.rs` | 1,030 | replay report/session, rebuild |
| `runs/repair.rs` | 512 | session repair, policy snapshot restore |
| `runs/export.rs` | 288 | session export |
| `runs/import.rs` | 277 | import/rehydrate |
| `runs/diagnostics.rs` | 286 | recovery, doctor, log status/restore |

`engine/plans.rs` is **49 lines**:

| Module | Lines | Responsibility |
|---|---:|---|
| `plans/governor.rs` | 420 | critical-path policy, governor status/preview/run-context/decisions/admission |
| `plans/revisions.rs` | 245 | plan revisions, revise/restore/edit, set/get plan |
| `plans/orchestration.rs` | 723 | coordination notes/decisions, snapshots, overview, preview/advance/replay, write leases |
| `plans/steering.rs` | 214 | steering create/list/cancel/deliver |
| `plans/continuations.rs` | 653 | defer/continuation lifecycle, claims, ack/retry/fail/reclaim |
| `plans/stalls.rs` | 298 | task progress, stall sweep/evaluation/recovery/escalation |
| `plans/tasks.rs` | 165 | task queue, retry/skip/cancel/pause/resume/interrupt, todos |

`engine/sources.rs` is **9 lines**:

| Module | Lines | Responsibility |
|---|---:|---|
| `sources/citations.rs` | 191 | session citations, citation audit, parsing helpers |
| `sources/collections.rs` | 167 | collections, metadata/labels, source brief, disk restore |
| `sources/embeddings.rs` | 719 | embedding/retrieval policies, ANN status/rebuild, reindex/reconcile |
| `sources/import.rs` | 666 | note/file/URL, import scan, refresh/reindex/delete, ingest |
| `sources/retrieval.rs` | 403 | list/get/search lexical/semantic/hybrid, stats |
| `sources/skills.rs` | 414 | skill CRUD/catalog/activation, build helpers |
| `sources/support.rs` | 84 | shared title/label/collection validation, chunking, project lookup |

## Invariants

- No SQL/event/prompt/approval/provider policy change.
- No public or `pub(crate)` signature change; moved private items become
  `pub(super)`/`pub(crate)` only where sibling or test access requires it.
- `agent.rs`, `engine/mod.rs`, `src/tests.rs`, and `agent/tests.rs` stay
  unchanged.
- No new `unwrap()`/`expect()` in library code.
- Tests are not rewritten.
- Parent files target < 2,000 lines; child modules target < 2,500 lines.
- The module-governance test becomes recursive and lowers the per-module budget
  from 6,000 to 3,000 lines after the split.

## Verification

- `cargo fmt --all -- --check` — passed
- `cargo check -p vp-core --all-targets` — passed
- `cargo clippy --workspace --all-targets -- -D warnings` — passed
- `cargo test -p vp-core --lib` — 2,442 passed / 0 failed / 2 ignored
- `cargo test --workspace` — 4,231 passed / 0 failed / 6 ignored
- module-governance tests — 3 passed, recursive 3k module budget

## Limits

- Relocation only; no performance or behavior claim.
- Mechanical clusters may still mix adjacent responsibilities.
- Parent preludes/glob re-exports may remain wider than an ideal design.

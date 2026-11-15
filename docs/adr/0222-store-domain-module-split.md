# 0222 — `vp-store` domain-module split

- Status: Accepted
- Date: 2026-11-16
- Scope: M3.1 structural governance

## Context

`crates/vp-store/src/store.rs` had grown to 34,939 lines. A single file mixed
schema creation, every domain projection, replay repair, bundle import, and
shared SQL helpers. Review was difficult, merge conflicts were frequent, and
new features paid an increasing navigation cost.

This is a structural change, not a behavior change: SQL, event names, public
methods, transaction boundaries, validation, and bounds remain identical.

## Decision

- `store.rs` remains the parent module: imports, top-level types/constants,
  shared helpers, `Store`, `data_dir`/`lock`, module declarations, and re-exports.
  It is now 6,611 lines.
- Domain modules under `store/` own cohesive `impl Store` blocks:
  schema, projects, contracts, effects, mcp, processes, remote, cache,
  scheduler, runtime, intake, programs, sources, orchestration, approvals,
  sessions, artifacts, tool outputs, test cache, context/progress, usage,
  memory, identity, agents, runs, plans, merge, events, backup, worktrees,
  mailbox, acp.
- Schema creation stays one unit (`store/schema.rs`) so table/migration order
  is unchanged.
- Shared free helpers (`map_*`, `insert_*_tx`, validators) stay in the parent.
- Internal methods used across sibling modules became `pub(super)`; no public
  API was widened.
- `store/tests.rs` remains the single test module and was not rewritten.

## Verification

- `cargo fmt --all --check`
- `cargo check -p vp-store --all-targets`
- `cargo test -p vp-store --lib` — 390 passed
- `cargo test -p vp-core --lib` and full workspace after integration: 4,227 passed / 0 failed / 6 ignored.

## Limits

- This is relocation, not a domain redesign. Some modules remain large
  (`sources`, `memory`, `plans`, `backup`, `schema`).
- `lib.rs` and `agent.rs` are still large and are the next structural targets.
- No new projection/event/query behavior is introduced by this ADR.

# 0228 — CLI and daemon dispatch module splits

- Status: Accepted
- Date: 2026-11-18
- Scope: M3.6 dispatch structural governance

## Context

After M3.4/M3.5 the remaining large application files are the command/route
matches:
- `crates/vp-cli/src/render.rs` ~7,999 lines
- `crates/vp-cli/src/dispatch.rs` ~6,918 lines
- `crates/vp-daemon/src/dispatch.rs` ~5,442 lines

These are cohesive but hard to review and conflict-prone.

This is a **mechanical structure change**, not a behavior change.

## Decision

- Split `vp-cli/render.rs` into output-domain modules behind the existing
  `render::*` surface.
- Split `vp-cli/dispatch.rs` into command-family modules behind
  `run_command(command, discovery, json)`; preserve every match arm and early
  output behavior.
- Split `vp-daemon/dispatch.rs` into route-family modules behind
  `dispatch(state, request)`; preserve every RPC method string, parse/error
  mapping, and unknown-method behavior.
- Parent dispatch/render files keep only module declarations, shared imports,
  family routing, and small glue.

### CLI render

`render.rs` is **33 lines** with 18 child files. Output groups: repository,
support, activity, runtime, forges, instructions, effects, identity, and the
`human/` first-match renderer split into briefs, programs, sessions,
orchestration, sources, projects, mcp, memory, maintenance, and status chunks.
`print_human` preserves the original first-match order and JSON fallback.

### CLI dispatch

`dispatch.rs` is **482 lines** with 23 child files. The parent matches all 425
`Command` variants and routes by command family; each family re-matches its
arms verbatim. Largest families: agent 984, memory 700, task 556, runtime 447,
project 442, remote 422, source 407, team 336, gate 301.

### Daemon dispatch

`vp-daemon/src/dispatch.rs` is **339 lines** with 11 route-family files.
Largest: session 1,122, project 750, team 747, agent 584, policy 545,
memory_source 535. The parent routes by method prefix with explicit
`task.stalls → session` handling and the original unknown-method fallback.

## Invariants

- No CLI argument/output/exit behavior change.
- No RPC method/params/response/error code change.
- No auth/metrics/discovery/route change.
- No public API widening; only crate-local `pub(crate)`.
- No new `unwrap()`/`expect()` in non-test code.
- Tests are not rewritten.

## Verification

- `cargo fmt --all -- --check` — passed
- `cargo check -p vp-cli --all-targets` — passed
- `cargo test -p vp-cli` — 156 passed (145 unit + 10 backup integration + 1 governance)
- `cargo check -p vp-daemon --all-targets` — passed
- `cargo test -p vp-daemon` — 23 unit + 1 governance passed
- `cargo clippy --workspace --all-targets -- -D warnings` — passed
- full workspace — 4,234 passed / 0 failed / 6 ignored

## Limits

- Relocation/routing only; no performance claim.
- If a family split cannot be made mechanically safe, the file stays as-is and
  that is reported rather than hidden.

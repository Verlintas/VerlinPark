# 0225 — `vp-cli` entrypoint split

- Status: Accepted
- Date: 2026-11-17
- Scope: M3.4 application-layer structural governance

## Context

`crates/vp-cli/src/main.rs` is 27,823 lines. It mixes the clap command model,
pre-dispatch special cases, a giant command match, RPC/output helpers, and all
CLI tests. The `main` file is the single largest source file in the repo and
regularly creates merge conflicts.

This is a **mechanical structure change**, not a behavior change.

## Decision

- Move the clap model (`Cli`, `Command`, and argument/action enums) to
  `crates/vp-cli/src/cli.rs`.
- Move the command match into `crates/vp-cli/src/dispatch.rs` as one async
  function that takes the command, discovery handle, and JSON flag.
- Move CLI tests to `crates/vp-cli/src/tests.rs`; tests are not rewritten.
- Move cohesive RPC/discovery helpers and output/render helpers into
  `rpc.rs` / `render.rs` only when the move is mechanical.
- `main.rs` keeps process setup, stack sizing, pre-dispatch special cases, and
  shared glue.

Resulting module map:

| Module | Lines | Responsibility |
|---|---:|---|
| `main.rs` | 1,157 | process/runtime setup, discovery, pre-dispatch cases, shared parsing glue |
| `cli.rs` | 4,047 | clap `Cli`, `Command`, provider/transport/action enums |
| `dispatch.rs` | 6,911 | the command match as `run_command(command, discovery, json)` |
| `render.rs` | 8,017 | JSON/human/CSV renderers and print helpers |
| `rpc.rs` | 183 | RPC/discovery helpers |
| `tests.rs` | 7,593 | unchanged test modules |

`main.rs` drops from 27,823 to 1,157 lines. `dispatch.rs` and `render.rs` are
still large; splitting them by command family or output domain is a later
mechanical step, not part of this decision.

## Invariants

- CLI argument names/defaults, output text/order, exit codes, RPC method names
  and params, discovery/config behavior, and test assertions are unchanged.
- No new `unwrap()`/`expect()` in non-test code.
- Tests remain in the crate test profile and are not made public.
- No changes outside `crates/vp-cli`.

## Verification

- `cargo fmt --all -- --check` — passed
- `cargo check -p vp-cli --all-targets` — passed
- `cargo test -p vp-cli` — 145 unit + 10 backup CLI integration tests passed
- `cargo clippy -p vp-cli --all-targets -- -D warnings` — passed
- Full workspace: 4,231 passed / 0 failed / 6 ignored; Swift unchanged at 535.

## Limits

- This task does not split the dispatch match by command family; that can be a
  later mechanical step if the file remains too large.
- No performance or behavior claim.

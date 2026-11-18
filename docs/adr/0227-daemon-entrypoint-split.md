# 0227 — `vp-daemon` entrypoint split

- Status: Accepted
- Date: 2026-11-18
- Scope: M3.5 daemon structural governance

## Context

`crates/vp-daemon/src/main.rs` is about 8,216 lines. It mixes process setup,
HTTP/WebSocket handlers, the RPC dispatch match, error/param helpers, many
background sweepers, discovery/auth code, and tests.

This is a **mechanical structure change**, not a behavior change.

## Decision

- Keep process setup, `AppState`, router construction, and entrypoint in
  `main.rs`.
- Move the RPC match to `dispatch.rs`.
- Move HTTP/WebSocket handlers to `http.rs`.
- Move background sweeper/reconciler spawners to `sweepers.rs`.
- Move error mappers and param helpers to `errors.rs` / `params.rs`.
- Move backup-status helpers to `backup_status.rs`.
- Move server info/auth/discovery/instance-lock helpers to `server.rs`.
- Move tests to `tests.rs` without rewriting assertions.
- `metrics.rs` stays unchanged.

## Invariants

- RPC method strings, params, response/error codes, auth, metrics, discovery,
  router paths, exit behavior, and tests are unchanged.
- Only crate-local visibility (`pub(crate)`) is added as needed.
- No new `unwrap()`/`expect()` in non-test code.
- No behavior or performance claim.

## Verification

- `cargo fmt --all -- --check`
- `cargo check -p vp-daemon --all-targets`
- `cargo test -p vp-daemon`
- `cargo clippy -p vp-daemon --all-targets -- -D warnings`
- Full workspace: 4,234 passed / 0 failed / 6 ignored; daemon 23 passed; Swift unchanged at 535.

## Limits

- `dispatch.rs` remains a large command match; splitting it by route family is a
  later mechanical step.
- Relocation only.

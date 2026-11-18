# 0226 — `vp-store` second-level module split

- Status: Accepted
- Date: 2026-11-18
- Scope: M3.5 store structural governance

## Context

M3.1 split the 34,939-line `store.rs` into 33 domain modules. Eight modules
remained above 2,000 lines: `employment` 4,798, `backup` 2,592, `workspaces`
2,402, `memory` 2,367, `sources` 2,339, `plans` 2,092, `identity_memory` 2,083,
and `coordination` 2,041.

This is a **mechanical structure change**, not a behavior change.

## Decision

Split each of those eight modules into a smaller parent plus child modules under
a same-named directory. Parents keep module declarations, shared
imports/types/helpers, and genuinely shared glue. Child modules own cohesive
`impl Store` method clusters.

| Parent | New parent lines | Child modules |
|---|---:|---|
| `employment.rs` | 549 | candidates 210, hiring 260, performance 1,042, promotion 373, replay 295, retirement 675, snapshots 83, tests 1,334 |
| `backup.rs` | 8 | import 1,417, queue 63, recover 524, repair 595 |
| `workspaces.rs` | 1,174 | bindings 387, landing 230, leases 126, replay 189, workspace 315 |
| `memory.rs` | 9 | conflicts 304, embedding_index 783, embeddings 421, policy 161, records 708 |
| `sources.rs` | 11 | ann 613, embedding 554, ingest 341, search 206, skills 120, snapshots 103, status 417 |
| `plans.rs` | 10 | control 326, interrupt 96, recoveries 441, snapshots 299, tasks 501, transitions 442 |
| `identity_memory.rs` | 979 | lifecycle 582, query 297, replay 235 |
| `coordination.rs` | 919 | assignments 424, inbox 452, replay 257 |

Schema creation stays untouched in `store/schema.rs`; `store/tests.rs` is not
rewritten. Private items become `pub(super)`/`pub(crate)` only where sibling or
test access requires it. All eight split parents and children are under 1,500 lines;
`employment/tests.rs` is the dedicated test module. `store/schema.rs` remains
2,931 lines as one ordered unit and is covered by the 3,000-line module budget.

## Invariants

- No SQL, event name, ordering, bounds, or error text changes.
- No public or `pub(crate)` signature changes.
- No new `unwrap()`/`expect()` in library code.
- `store/tests.rs` remains the test source of truth.

## Verification

- `cargo fmt --all -- --check`
- `cargo check -p vp-store --all-targets`
- `cargo test -p vp-store --lib`
- `cargo clippy -p vp-store --all-targets -- -D warnings`
- Full workspace: 4,234 passed / 0 failed / 6 ignored; store lib 390 passed; Swift unchanged at 535.

## Limits

- Relocation only; no performance claim.
- Mechanical clusters may mix adjacent store responsibilities.
- Schema creation remains a single ordered unit.

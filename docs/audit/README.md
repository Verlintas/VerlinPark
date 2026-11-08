# Audit ledger

Full-repository audit of 2026-11-09.

- [2026-11-09-findings.md](2026-11-09-findings.md) — complete human-readable table.
- [2026-11-09-findings.csv](2026-11-09-findings.csv) — same rows for sorting/filtering.

The ledger consolidates 116 findings from five read-only reviews (Rust runtime/security, store/protocol/daemon/CLI/providers, SwiftUI client, invariants/tests/docs, and targeted runtime checks) into 65 root-cause entries. Every original finding ID is preserved in the `Source IDs` column.

## Status definitions

- **Fixed** — code applied in this round; regression coverage where the behavior is directly testable.
- **Partial** — the main defect is fixed, but a named residual remains.
- **Deferred** — recorded but not changed in this round.

Current totals: 50 Fixed, 10 Partial, 5 Deferred.

## Source ID prefixes

- `R-*` — Rust runtime/security review.
- `T-*` — runtime test-coverage findings.
- `S-*` — store/protocol/daemon/CLI/providers review.
- `U-*` — SwiftUI app review.
- `X-*` — cross-cutting invariants/docs/tests review.

## Verification at close

- `cargo test --workspace`: 316 active Rust tests, 3 ignored live provider tests.
- `xcodebuild test`: 89 Swift tests.
- `just lint`: `cargo fmt`, `clippy -D warnings`, `swiftformat --lint`.
- Live daemon checks used during the audit: JSON-RPC auth/health, oversized request body, imported path traversal, and missing-approval resolution. Each defect found this way has a regression test.

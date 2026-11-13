# 0214 — Hardening round two: bounded replay, OTel paging, and template snapshot validation

- Status: Accepted
- Date: 2026-11-13
- Scope: remaining C4/C5/S7/S8 hardening from ADR-0213

## Context

ADR-0213 fixed the P0/P1 implementation defects from four production reviews
but left four concrete gaps: controller replay still read the full event log and
was superlinear, diagnosis decoded payloads under the global SQLite lock, OTel
export had no cursor or retryability signal, and bundle/rebuild accepted a team
template without recomputing its content hash. Imported template links could
also appear as locally applied. The project org chart could truncate away the
Leader, and the `vp-remote` cancellation test raced its default timeout.

## Changes

### Bounded replay and diagnosis decode

- `orchestration.replay` reads the newest 20,000 events, reports
  `truncated=true` when the session has more, and adds a warning that older
  plan/task state is outside the window. It no longer scans the entire event
  log for every decision.
- `Store::list_recent_events_window` reads payload strings under the SQLite
  lock and parses JSON after releasing it. Diagnosis and replay no longer stall
  every other session while decoding a 20k/50k event window.
- Replay remains `controller_equivalent_only`: no provider/tool call, no event
  append, no mutation.

### OTel export paging and retryability

- `OtelExportParams` gains `offset`; `OtelExportResult` gains `offset`,
  `next_offset`, and `retryable`.
- Export selects `[offset, offset + limit)` over the projected spans. An
  omitted offset starts at 0; `next_offset` is present only when more spans
  remain. Repeated calls can walk the whole session without duplicating the
  head.
- `retryable` is true for transport errors, HTTP 5xx, and HTTP 429; it is false
  for 2xx, other 4xx, and local serialization/client errors. A successful
  export reports `retryable=false`.
- CLI adds `vp otel-export --offset`; Swift models decode the new fields and
  `CoreClient.exportOtel` accepts an optional offset.

### Team template snapshot validation

- Canonical template hash computation moved to `vp-protocol`, so `vp-store`
  can recompute it without depending on `vp-core`.
- Bundle validation and `insert_team_template_snapshot` recompute the content
  hash and reject a body/metadata/hash mismatch with `TEMPLATE_REPLAY_INVALID`.
  A tampered bundle can no longer persist a template that only carries
  self-consistent metadata.
- `project_team_templates` gains `origin` (`local|imported`). Bundle import
  stores restored links as `imported`; imported links are evidence, do not set
  `entry.applied`, and are marked read-only. A later explicit local apply
  upgrades the link to `local` in a transaction and remains idempotent.
- `TeamTemplateEntry` exposes `origin` on RPC and Swift models; the existing
  `read_only` flag is preserved.

### Org chart and test reliability

- `agent.org.snapshot` sorts eligible identities Leader → Manager → Worker
  before applying the node cap, so truncation cannot hide the chart root or
  department managers behind older Workers. `truncated` counts only eligible
  rows (hidden retired rows do not claim truncation).
- The `vp-remote` cancellation test uses a 30s command timeout and a 15s
  cancellation bound, removing the race between the default 5s timeout and the
  old 5s assertion.

## Remaining explicit gaps

- S5 identity-memory prune is still projection-only; older events retain
  content. No crypto-erasure or event retention/compaction policy exists.
- S8 bundle import recomputes the hash but does not run the full shape
  validator in `vp-store`; malformed-but-self-hashed templates fail later when
  preview/diff validates them, rather than at import.
- S6 performance attribution is conservative without an explicit run binding;
  successor recording does not transfer memory/component-branch ownership.
- S7 still recomputes the performance ledger on org reads and has no automated
  UI suite.
- No coverage tooling, seeded old-database migration fixture, or committed
  adversarial path/URL/shell corpus yet.

## Verification

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`: 3,890 passed, 0 failed
- `swiftformat --lint`: clean
- `xcodegen generate` + `xcodebuild test`: app and helper suites passed
- New tests: OTel paging/retryable decode, OTel offset page selection,
  tampered template snapshot rejection, imported-link provenance, and the
  existing replay/org/remote tests.

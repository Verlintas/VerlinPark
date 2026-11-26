# 0235 — CUSTOS observe-only foundation

- 状态：Accepted
- 日期：2026-11-26

## 背景

CUSTOS is the sealed, deterministic integrity guardian described in
`docs/CUSTOS_CHARTER_V0.md`. The design calls for a four-role process ring,
read-only SwiftUI window, one-way notices, capacity-aware process scoring, and
strict non-interference with approvals, budgets, model routing, identities, and
final merge gates.

The full system cannot land safely in one change. The first slice must prove:

- a separate process can read VerlinPark state without depending on the daemon
  being alive or writing through `Store::open()`;
- the charter/capacity/process-score policy is deterministic and testable in
  isolation;
- the GUI/CLI surface can be read-only and cannot become a second operator
  console;
- later mutation classes have a stable protocol and audit shape.

## 决策

Add a new workspace crate `vp-custos` with two binaries:

- `custosd` — observe-only core process;
- `custos` — read-only CLI client.

The first slice implements:

- a pinned draft charter hash and closed action catalog;
- capacity normalization from physical memory, cores, and disk space;
- a pure process-pressure score with learning-mode and hard-cap guards;
- a four-role heartbeat file format, quorum helper, user LaunchAgent
  installer/status/uninstall (`--install/--uninstall/--agent-status`), and a
  peer watchdog that only kickstarts an already-installed plist;
- read-only SQLite probes for sessions, events, leases, runs, tasks, merge
  queue, effects, approvals, and supervised processes;
- read-only process sampling with `sysinfo` and shadow pressure/stop
  recommendations, without any signal or notice;
- a content-addressed hash vault for Custos-owned binary/plist baselines, with
  optional local Ed25519 signing, explicit init/verify/repair, and quarantine
  before restore;
- a 0600 Unix-domain socket with `status`, `health`, `charter`, `ping`, and the
  explicitly consented `diagnostics.generate`;
- local standard + forensic ZIP bundles (0700 directory, 0600 files, SHA-256
  manifest, 8/32 MiB caps) with no upload and no caller-supplied path;
- a separate `<data-dir>/custos/discovery.json` (0600), not the daemon token;
- no mutation method, no `act`, no `policy.set`, no pause/resume, no TCP
  listener, no provider key, and no agent-facing tool.

The SQLite probe uses `SQLITE_OPEN_READ_ONLY` and `PRAGMA query_only`; it does
not call `vp_store::Store::open()` because that creates/migrates schema and a
daemon identity. A missing/locked database is reported, not repaired.

The SwiftUI surface is a normal GUI window (Settings-style independent window),
with the `Custos` wordmark as the only ASCII/character art. The central diamond,
dimensions, counts, and log are ordinary SwiftUI graphics/text. The window and
menu-bar item are read-only; there are no repair/restart/restore/approve/kill
controls.

## 理由

- Read-only-first prevents the guardian from becoming a second source of truth
  before its evidence and thresholds are validated.
- A separate crate keeps CUSTOS independent of `vp-core` and avoids a dependency
  cycle; it can read state while the daemon is down.
- Pure capacity/score modules can be tested without a live daemon or provider.
- A 0600 socket is enough for observe-only metadata; mutation classes will need
  a sealed token and peer-credential checks before they land.
- The single core role is a deliberate slice: the full four-role ring is needed
  before any mutation authority is enabled.

## 验证

- `cargo test -p vp-custos`: 26 unit tests + 2 integration tests + 1 module
  governance test.
- Integration tests spawn the real `custosd`, seed a minimal SQLite file, and
  verify `status`, `health`, `charter`, `ping`, `METHOD_NOT_FOUND` for `act`,
  and that a missing database is not created.
- `cargo clippy --workspace --all-targets -- -D warnings` passes.
- `cargo test -p vp-custos`: 59 unit + 4 integration + 1 module-governance.
- Full Rust workspace: 4,766 passed / 0 failed / 6 ignored.
- Swift `xcodebuild test`: 673 passed / 0 failed, including Custos wordmark,
  protocol decoding, diagnostics-result decoding, and unavailable-socket model
  tests.

## 限制

- Observe-only, single core role. No L1 notice, L2 repair, L3 containment, or
  L4 recovery action is enabled.
- The four-role launchd installer and heartbeat/watchdog are implemented, but
  the ring has not been installed/verified on a real machine. The hash vault
  supports optional local Ed25519 but is not release-signed and is
  explicit-repair only; no automatic self-heal or automatic boot installation
  yet.
- The socket is 0600 but peer-uid/token checks are not implemented; only
  read-only status metadata is exposed.
- Probe coverage is bounded to the named tables and does not run a full
  `PRAGMA integrity_check`.
- Process sampling is read-only shadow evidence; notices and any action remain
  later slices.
- The GUI does not yet generate diagnostic bundles.
- No live daemon or real project data is modified by this slice.

## 备选

- In-daemon module: rejected for the guardian because a hung daemon cannot heal
  itself and the agent/tool surface would be harder to keep separate.
- Direct `vp-store` reads: rejected for the first slice because `Store::open()`
  is read-write and performs migrations.
- Full four-role ring immediately: rejected until the observe-only protocol and
  evidence are validated.

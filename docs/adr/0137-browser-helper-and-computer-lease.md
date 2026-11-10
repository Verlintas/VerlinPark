# ADR-0137: Browser helper and daemon-wide ComputerLease

- **Status:** Accepted
- **Date:** 2026-11-10
- **Owners:** Runtime, Agent Tools, macOS
- **Related:** ADR-0004, ADR-0008, ADR-0080, ADR-0087, ADR-0123, ADR-0134, ADR-0135

## Context

ADR-0134/0135 deferred browser and computer control. The earlier headless
Chrome/CDP spike could start Chrome only after disabling Chrome's own sandbox
and widening temp/crashpad writes; even then local HTTP pages stalled because
Chrome's network service was not usable inside the daemon's nested Seatbelt
profile. Running Chrome unsandboxed would have created a host write path outside
the worktree and contradicted the ADE security model.

This ADR records a second attempt with two separate deliverables:

1. a real daemon-wide `ComputerLease` for serialized interactive surfaces;
2. a bounded local browser helper that is not an agent shell command.

The lease is useful even without a browser. The browser is only shipped because
the helper architecture is now demonstrably reliable on macOS and its limits are
documented rather than hidden.

## Decision

### 1. ComputerLease is separate from write-scope leases

`computer_leases` is a SQLite table in the daemon database:

| Column | Meaning |
|---|---|
| `lease_id` | unique UUID for one acquisition; used by renewal/release |
| `surface` | one serialized interactive surface, currently `browser` (unique) |
| `owner_id` | daemon runtime id; a second runtime cannot take an unexpired lease |
| `session_id`, `agent_id`, `agent_name` | audited owner of the surface |
| `acquired_at`, `heartbeat_at`, `expires_at` | TTL and expiry recovery |

The lease is acquired by the browser manager for the helper's lifetime, with a
120-second TTL and a heartbeat every TTL/3. Acquisition deletes expired rows and
their audit events in the same transaction. An unexpired lease on the same
surface fails fast with the holder; there is no hidden queue. `Drop` stops the
heartbeat and leaves the row to expire after a crash. Normal paths release
explicitly, append `computer.lease.released`, and stop the helper.

The write-scope `write_leases` table stays unchanged. It serializes worktree
write scopes across sessions. `computer_leases` serializes visual/interactive
state, not file writes. A browser never writes the worktree; the daemon writes a
screenshot through the normal path jail after the helper returns a PNG in its
own scratch directory.

Audit events are appended atomically with the lease row:

- `computer.lease.acquired`
- `computer.lease.denied`
- `computer.lease.released`
- `computer.lease.expired`

These are operational audit events, not session projections. `session.replay`
and `session.repair` ignore them deliberately: the live lease table is
daemon-wide state and cannot be rebuilt by replaying one session's event stream.
Daemon recovery clears all computer leases and emits `computer.lease.expired`
for each row before the next run. `computer.leases` /
`computer.leases.sweep` / `computer.leases.release` expose read-only status,
expiry sweeping, and an operator escape hatch; `vp computer` wraps them. Manual
acquisition is intentionally not exposed because an unowned lease would block the
surface without a heartbeat.

### 2. Browser is a separate WKWebView helper, not agent shell

`VerlinParkBrowserHelper` is a small Swift executable built by the macOS Xcode
project. The daemon launches it with:

```text
/usr/bin/sandbox-exec \
  -D SCRATCH=<session helper scratch> \
  -D WORKTREE=<agent worktree> \
  -p <browser helper profile> \
  VerlinParkBrowserHelper --scratch <scratch> --worktree <worktree>
```

The helper is a trusted daemon-owned binary, not an agent-chosen command. It is
launched **outside the agent shell's Seatbelt profile** but under its own
profile:

- normal reads and localhost network access are allowed;
- writes are confined to the helper scratch directory, `/dev/null`, and
  `/dev/zero`; the agent worktree is explicitly write-denied;
- Apple event delivery is denied;
- the helper refuses to start unless the daemon sets
  `VERLINPARK_BROWSER_HELPER_SANDBOX=seatbelt`, so an accidental unsandboxed
  launch fails closed;
- if `sandbox-exec` is unavailable the tool returns an explicit error and never
  runs the helper unsandboxed.

The helper uses `WKWebsiteDataStore.nonPersistent()` and a scratch `HOME` /
`TMPDIR` / cache directory. Cookies, local storage, and WebKit website data do
not become a shared browser profile. The helper process and its scratch
directory are per session and per worktree.

### 3. Navigation and output policy

The daemon validates every navigation before sending it to the helper, and the
helper repeats the check:

- `http(s)` only for `localhost`, `127.0.0.1`, and `[::1]` (any port);
- `file:` only for a regular file under the caller's canonical worktree and not
  under `.git`;
- `about:blank` only;
- URL credentials, external hosts, data URLs, and other schemes are rejected;
- downloads, non-displayable responses, attachment responses, new windows, file
  pickers, and JavaScript dialogs are refused or recorded; confirm/prompt
  dialogs are denied/cancelled.

`snapshot` is a DOM-derived accessibility-shaped tree with stable `eN`
references, role/name/text/value/checked/disabled state, plus bounded visible
text, URL, title, and viewport. It is not the native macOS Accessibility API and
does not claim frame-level AX fidelity. Output is capped (nodes, text, 32 KiB
structured event cap) and full page HTML is never returned by default.

`click`, `type`, `press`, and `select` target an `eN` reference or a CSS
selector and use synthetic DOM events. This works for ordinary web apps and
frameworks that listen to DOM events. Synthetic events do not reproduce every
native browser default action; `Enter` on a form control calls
`form.requestSubmit()` as a deterministic fallback.

`screenshot` asks WKWebView for the viewport, writes a PNG into helper scratch,
and the daemon validates the source path, caps it at 16 MiB, and writes it into
the worktree with the canonical path jail, symlink-parent check, atomic write,
and SHA-256. The default path is
`.verlinpark/browser/screenshot-<id>.png`; v1 captures the viewport, not a
stitched full page. OCR remains `read_image`.

`console` returns a bounded in-page console/error ring. `evaluate` is the only
JavaScript execution action and is classified `HostState`: `smart` asks the
user, `strict` asks, and `unattended` denies. Browser open/input/screenshot are
`WorkspaceWrite`; snapshot/console/close are read-only.

Roles: Leader and coding Worker/Labour may use the browser. Manager, Verifier,
research, verification, and generic profiles may not.

### 4. Lifecycle

- One helper per `(session, worktree)`.
- The helper owns the `browser` ComputerLease until close, idle sweep, session
  cleanup, or daemon exit.
- `VERLINPARK_BROWSER_IDLE_SECS` (default 600) closes idle helpers and releases
  the lease. The existing terminal sweeper also sweeps browsers.
- Worktree cleanup stops session browsers before removing scratch/worktree.
- A helper that crashes or times out is removed, killed, and the next action
  starts a fresh helper and lease.
- The helper binary is discovered from `VERLINPARK_BROWSER_HELPER`, next to
  `verlinpadd`, the development Xcode products directory, or `PATH`. Missing
  helper is an explicit `BROWSER_HELPER_UNAVAILABLE`-style error, not a stub.

## Evidence from the feasibility spike

The earlier Chrome/CDP failure was reproduced and contrasted with WKWebView:

- WKWebView works offscreen from a command-line process: local HTTP navigation,
  `evaluateJavaScript`, `callAsyncJavaScript`, DOM snapshots, clicks, and
  `takeSnapshot` all succeeded.
- Under `sandbox-exec` with only `(allow default)`, a data URL loaded but local
  HTTP stalled. Adding explicit `(allow network*)`, `(allow network-outbound)`,
  `(allow network-inbound)`, `(allow mach*)`, and `(allow ipc*)` made local HTTP
  work while writes stayed confined to scratch. This is the concrete fix for the
  network-service failure that blocked Chrome.
- WKWebView works with a write-denial profile that allows only the scratch
  subtree. Screenshot encoding and PNG writes succeed when the screenshot is
  written non-atomically inside scratch. `Data.write(options: .atomic)` failed
  because Foundation places its temp file in the system temporary directory,
  which is outside the scratch allowance; the helper writes a temp file in the
  same scratch directory and renames it instead.
- `WKContentRuleList` compilation fails under this write-confined profile (even
  with an explicit rule-list store URL), so the helper does **not** use content
  rule lists and does **not** claim to block page-script subresource requests.
  Top-level navigation, new windows, downloads, and file access are enforced;
  page JavaScript can still make network requests to external hosts. The helper
  is a write/profile boundary, not a network egress boundary.
- WebKit's XPC services (Networking, GPU, WebContent) are Apple-signed system
  services with their own sandbox profiles. The helper's Seatbelt profile
  confines the helper's own writes; it does not add a second network sandbox
  around those services. The helper's non-persistent store and blocked
  downloads are the profile/data boundary.

The spike was run on macOS 27.0.1 with WebKit 625.2.8 and Swift 6.4.2. The
shipped helper is built by the `VerlinParkBrowserHelper` Xcode target; the Rust
daemon discovers it next to `verlinpadd` or through
`VERLINPARK_BROWSER_HELPER`.

## Consequences

Positive:

- A real daemon-wide ComputerLease now exists with ownership, TTL, heartbeat,
  expiry recovery, audit events, RPC/CLI status, and tests.
- A local browser is available for frontend verification without giving the
  agent a shell path to Chrome.
- Browser scratch/profile/screenshot writes stay inside runtime scratch/worktree;
  the worktree write boundary is not weakened.
- The browser is explicit about being unavailable when the helper is missing,
  when Seatbelt is unavailable, or when the machine has no GUI session for
  WebKit.

Negative / limitations:

- The browser is macOS-only and requires a logged-in GUI session where WebKit
  can attach to WindowServer. It is not a headless CI browser.
- Page-script network egress is not filtered; do not use the browser as a
  network isolation boundary.
- Snapshot is DOM-derived, not native AX; synthetic input does not cover every
  browser default action.
- Screenshots are viewport-only and default under `.verlinpark/browser` in the
  worktree; callers should publish or ignore them deliberately.
- ComputerLease contention fails fast; there is no queue or fairness policy.
- This remains a single-daemon lease backed by SQLite, like `runtime_leases`. It
  is not a distributed fencing token and does not replace the data-dir instance
  lock.
- ComputerLease events are audit-only and not replay projections. Recovery
  clears stale rows and emits expiry events rather than reconstructing them.
- The helper binary must be built and packaged with the app/daemon; a source-only
  checkout that has not built the Xcode target gets an explicit unavailable
  error.

## Alternatives

- **Headless Chrome/CDP under the agent shell sandbox:** rejected after the
  earlier spike; Chrome's network service cannot load local HTTP in the nested
  profile, and disabling its sandbox/widening writes did not fix navigation.
- **Run Chrome unsandboxed:** rejected; it creates host writes outside the
  worktree and weakens the ADE boundary.
- **Host the browser in the SwiftUI client:** rejected; tools must work from the
  CLI/daemon without the GUI app, and the app is a client, not a daemon service.
- **Use `WKContentRuleList` as a network allowlist:** rejected after repeated
  compilation failures under the write-confined helper profile. Do not claim a
  network boundary the helper does not have.
- **Use the native macOS Accessibility API:** deferred with the computer tool;
  this ADR ships a DOM accessibility-shaped browser snapshot, not AX.
- **Expose manual `computer.leases.acquire`:** rejected for now; an unowned lease
  would block the surface without a heartbeat. Operators can inspect, sweep, and
  release a stuck lease.

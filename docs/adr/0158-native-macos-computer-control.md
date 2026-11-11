# ADR-0158: Native macOS computer control

- **Status:** Accepted
- **Date:** 2026-11-11
- **Owners:** Runtime, macOS, Protocol
- **Related:** ADR-0026, ADR-0066, ADR-0092, ADR-0134, ADR-0135, ADR-0137

## Context

VerlinPark already ships a WKWebView helper behind the `browser` agent tool. That
helper is deliberately narrow: non-persistent data store, localhost/worktree
navigation only, no downloads, and a Seatbelt profile that permits reads and
localhost network but only scratch writes. It solves web UI automation, not
native application automation.

Agents increasingly need to operate the applications around the repository:
launch or focus a build under test, inspect a native dialog, verify that a menu
item is enabled, type into a scratch document, or capture a window. The browser
helper cannot do this. Shell commands cannot do this safely either: `osascript`
and synthetic event tools are host-level effects hidden inside an opaque shell
command, and they bypass the typed approval surface.

At the same time, macOS does not offer a sandbox that permits Accessibility
(`AXUIElement`), global `CGEvent` posting, and ScreenCaptureKit. Those are
host-level TCC privileges. Pretending they are sandboxed would be dishonest and
would weaken the security story. The design therefore has to make the trust
boundary explicit, keep the daemon in charge, and make every agent-visible
operation small, typed, bounded, auditable, and approval-gated by the existing
policy.

This ADR records the R4 workstream from `docs/LONG_TASK_RUNTIME_PLAN.md`.

## Decision

### 1. A separate, daemon-owned helper

Add a new packaged command-line tool, `VerlinParkComputerHelper`, built from:

- `apps/macos/VerlinParkComputerHelperCore` — pure validation, bounding,
  redaction, coordinate, key, snapshot, and encoding helpers shared with tests;
- `apps/macos/VerlinParkComputerHelper` — AppKit/ApplicationServices/
  ScreenCaptureKit implementation and JSON-lines dispatch.

The daemon, not the agent, launches the helper. The agent never receives a
helper path, process handle, or stdin. The helper:

- clears the inherited environment, keeps a minimal `PATH`, and points `HOME`,
  `TMPDIR`, and caches at a per-session daemon scratch directory;
- is placed in its own process group and killed as a group on timeout, close,
  idle sweep, session cleanup, or daemon exit;
- receives `--scratch` and `--parent-pid`; it exits on stdin EOF, after `close`,
  or when the parent process disappears;
- refuses to run without the daemon's `VERLINPARK_COMPUTER_HELPER=1`
  environment marker. This is a guardrail against accidental standalone use,
  not a security boundary: a user can run the binary directly.

The helper has no shell execution, no AppleScript bridge, no URL opener, no
process launcher, no network client, and no arbitrary file writes other than one
PNG below its scratch `screenshots/` directory.

### 2. JSON-lines protocol

The transport is one JSON object per line:

- request: `{"id":"…","action":"…","params":{…}}`
- success: `{"id":"…","ok":true,"result":{…}}`
- error: `{"id":"…","ok":false,"error":{"code":"…","message":"…"}}`
- startup: `{"event":"ready","version":"1",…}`

Requests are capped at 1 MiB, response frames at 2 MiB. The Rust manager reads
frames with a pre-allocation bound, skips unsolicited events, matches ids, and
applies an action-specific deadline. A malformed or oversized frame, timeout,
EOF, or process crash invalidates and kills the helper rather than leaving a
half-read pipe. Typed helper errors such as `STALE_REF`, `ELEMENT_NOT_FOUND`, or
`WINDOW_NOT_FOUND` keep the helper alive for a retry.

The protocol is intentionally not part of the daemon's JSON-RPC surface. It is
an internal child-process contract. The additive daemon RPC is
`computer.status`, which reports helper availability and TCC permissions without
posting input.

### 3. Actions and bounds

The helper exposes exactly the R4 actions:

| Action | Result | Bounds |
| --- | --- | --- |
| `permissions` | Accessibility + Screen Recording preflight | no prompt |
| `apps` | bundle id, name, pid, frontmost, hidden, activation policy | 200 apps |
| `windows` | CGWindow id, title, bounds, layer, focused, on-screen | 100 windows |
| `focus` | activate app and raise a matching window | app + optional title/id |
| `snapshot` | bounded AX tree, stable refs, roles, labels, values, states, frames | depth 1–20, 1–1200 elements, 1 KiB–256 KiB |
| `screenshot` | PNG metadata under scratch | 320–2560 px, 5 MiB |
| `click` | element ref or normalized point inside target window | 0–1 coordinates, 1–2 clicks |
| `type` | Unicode text into focused element | 10 KiB, no NUL |
| `key` | whitelisted key + up to four modifiers | key table, destructive combos |
| `scroll` | line/pixel wheel event | ±1000 |
| `drag` | normalized from/to within target window | 0–2000 ms, 1–60 steps |
| `wait` | ref/element/window/app-frontmost condition | 50–30000 ms |
| `close` | stop helper | — |

Coordinates are normalized to a target window selected by `bundle_id`, `pid`,
or `window_id`. The helper resolves the window frame, rejects NaN/infinite values,
and re-checks that the resolved point is inside the window. It never accepts
absolute screen coordinates from the model.

Element refs are `ax-` plus a 64-bit FNV-1a hash of process id, window index, and
the AX path components (role, subrole, identifier, child index). They are not
raw pointers, they are stable while the path is unchanged, and they are replaced
on every snapshot. A click by ref resolves the current AX element, focuses it,
and posts a bounded CGEvent at the element's frame center.

### 4. Snapshot redaction and bounding

The snapshot walker requests at most 200 children per node
(`AXUIElementCopyAttributeValues`), tracks visited AX elements to stop cycles,
and stops at the requested depth. Each node carries role, subrole, identifier,
title, description/label, value, enabled/focused/selected, and frame.

- Secure fields (`AXSecureTextField` / `AXSecureTextArea`, by role or subrole)
  have their value replaced with `***` before any truncation or serialization.
- Values are only read for text/value-bearing roles.
- Titles, labels, and values are capped per field; the whole tree is counted
  against a byte budget and re-checked after JSON serialization.
- The daemon renders a readable tree for the transcript and caps the structured
  tree that is persisted in the event stream.
- Snapshot is a read action, but it reads host application content. The daemon
  classifies it as `External`, not workspace-local.

### 5. Screenshot artifacts, not inline data

The daemon gives the helper only a generated relative path below
`<scratch>/screenshots/`. The helper re-validates that the path has no absolute
prefix, `..`, or symlink escape, captures a window or display through
ScreenCaptureKit, scales to the requested bounds, and writes PNG bytes through a
temporary file and rename.

The Rust manager then:

1. canonicalizes the source and requires it to remain below the helper scratch;
2. rejects empty files and files over 5 MiB;
3. verifies dimensions and the reported SHA-256;
4. publishes the bytes through `Engine::publish_artifact_binary` into the normal
   session artifact store;
5. deletes the scratch source.

The tool result contains an artifact id, blob path, dimensions, byte count, and
digest. It never contains base64 image data. This preserves the existing artifact
retention, preview, and GC behavior.

### 6. One GUI surface, one lease

The helper holds the daemon-wide `ComputerLease` for the `computer` surface for
its lifetime, exactly like the browser helper holds `browser`. Acquiring a second
surface fails fast with the holder's session/agent/expiry instead of queueing.
The lease is SQLite state with TTL and heartbeat; an operator can inspect,
release, or sweep it through the existing `computer.leases*` RPCs and
`vp computer` CLI.

The lease is released explicitly on `close`, idle sweep, session cleanup, spawn
failure, and helper error paths. A process crash cannot run async cleanup, so the
row remains until its TTL expires; the heartbeat stops with the guard. This is a
single-daemon lease, not a distributed fence.

The `computer.status` path does not take the lease because it only preflights
TCC permissions and never posts input or captures content.

### 7. Approval and risk classification

The agent loop already calls `assess_tool_call` before dispatch. `computer` is
classified by effect:

- `permissions`, `apps`, `windows`, `snapshot`, `wait`, `screenshot`:
  `External` — reads host application/screen state. `smart` allows without a
  prompt but records `permission.allowed`; `strict` asks.
- `close`: `ReadOnly`.
- `focus`, `click`, `type`, `key`, `scroll`, `drag`: `HostState` — `smart` asks
  before the first call, `unattended` denies, and `strict` asks.
- destructive `key` combinations (`cmd+q`, `cmd+shift+delete`,
  `cmd+option+escape`, `ctrl+cmd+q`, command-shift logout variants):
  `Destructive` — `smart` asks, `unattended` denies.

The helper repeats the destructive-key check. The daemon marks
`allow_destructive` only after the permission decision has passed; the helper
refuses the shortcut when that marker is absent. This is defense in depth, not a
new approval mechanism.

The tool is advertised only to the Leader and to coding Worker/Labour profiles.
Managers, Verifiers, Scouts, research Workers, and generic Workers do not receive
it. The execution-time allowlist repeats the check, so a forged tool call cannot
bypass the schema gate. Existing project approval grants may remember a
`computer` call, but `HostState` and `Destructive` calls still follow the same
`ApprovalMode` semantics; there is no auto-approval switch for this tool.

### 8. Permissions fail closed

The helper checks `AXIsProcessTrusted()` and `CGPreflightScreenCaptureAccess()`.
It does not call the prompting variants. Accessibility is required for apps,
windows, focus, snapshots, element waits, and all input. Screen Recording is
required for screenshots. Missing permission returns a structured
`PERMISSION_DENIED` error naming the exact System Settings pane.

The helper's TCC identity is its own executable path. Users may need to grant
permission again after moving or rebuilding the helper. The daemon status RPC
makes the current state visible without silently falling back to AppleScript,
`cliclick`, or shell.

### 9. Packaging and discovery

`apps/macos/project.yml` adds the helper target, a pure-core test target, and
scheme entries. `scripts/bundle-daemon.sh` copies
`VerlinParkComputerHelper` into `VerlinPark.app/Contents/Helpers/` next to
`verlinpadd`. The Rust resolver checks:

1. `VERLINPARK_COMPUTER_HELPER` (must be executable or fails closed);
2. the daemon's own directory and the app `Contents/Helpers` directory;
3. development `apps/macos/build/Build/Products/{Debug,Release}/`;
4. `PATH`.

A missing helper is a structured `COMPUTER_HELPER_UNAVAILABLE` error, not a
silent fallback.

### 10. Trust model and explicit non-goals

**Native macOS computer control is not sandboxed.** The helper runs with the
user's authority. Accessibility can read other applications' UI; CGEvent can
type into them; ScreenCaptureKit can capture them. A malicious or compromised
helper is equivalent to a local GUI automation process. The mitigation is
process ownership, approval, bounded actions, audit events, and a narrow
protocol — not a kernel sandbox.

This ADR does not claim:

- confinement of arbitrary reads, network access, or daemon-mediated writes by
  the helper;
- isolation from the clipboard, credentials visible in UI, or other apps;
- that the helper survives a locked screen or runs without a logged-in GUI
  session;
- that `ComputerLease` is a distributed lock or a queue;
- native Windows/Linux GUI control;
- AppleScript, shell, menu-bar walking, or arbitrary URL opening;
- automatic permission grants or silent screen capture.

The helper adds no network code and no shell path. It does not touch durable
runs, remote workspaces, or programs from the other R4 workstreams.

## Consequences

### Positive

- Agents can operate native apps through the same typed, audited tool loop as
  file and shell actions.
- Every host-state action is visible in the event stream and subject to the
  existing approval mode.
- Snapshot redaction and screenshot artifact publication avoid leaking raw
  password values or unbounded images into model context.
- The browser and computer helpers share the same lease, lifecycle, and bounded
  protocol patterns, so operators have one mental model.
- Pure Swift core tests cover ref generation, snapshot bounding, redaction,
  coordinate validation, key policy, and path jails without requiring GUI
  permissions.

### Negative / risks

- The helper is a high-authority local process. A bug in AX traversal, CGEvent
  coordinates, or screenshot path validation could affect another app.
- TCC permissions are manual and may need to be re-granted after moving the
  helper.
- AX trees are app-specific; some apps expose sparse or unstable trees.
- Screenshots may include sensitive content; they are stored as normal session
  artifacts and inherit artifact retention/GC policy.
- A crashed helper leaves a lease row until TTL. The status/lease RPCs make this
  observable.

### Mitigations

- Bounded frames, action validation in both Rust and Swift, and a short per-action
  deadline.
- Redaction before bounding; refs are hashes, not pointers; coordinates are
  normalized and re-checked.
- Screenshot path canonicalization plus digest and size checks in the daemon.
- Process-group cleanup, parent watchdog, idle sweeper, and session cleanup.
- Manual smoke instructions; CI explicitly does not claim GUI/TCC coverage.

## Testing

Rust coverage includes:

- action normalization and unknown-action rejection;
- click ref-vs-point validation, NaN/out-of-range rejection, modifier/normalize
  handling;
- text size/NUL bounds, key whitelist, destructive-combo mapping;
- scroll/drag/snapshot/wait caps;
- helper response parsing, bounded frames, helper-missing resolution;
- screenshot path/size validation;
- `computer` lease fail-fast contention;
- a fake JSON-lines helper that exercises ready, permissions, and close;
- permission risk mapping for every action class.

Swift coverage includes:

- request parsing, oversized request rejection, response envelopes;
- stable ref hashing and validity rules;
- secure-field redaction and field bounding;
- snapshot depth/element/byte budgets;
- normalized point validation and window resolution;
- click/type/key/scroll/drag/wait/focus/screenshot spec validation;
- key whitelist and destructive combinations;
- screenshot output path jail and aspect-fit bounds;
- Codable snake_case application/window models.

Manual smoke (documented in
`apps/macos/VerlinParkComputerHelper/README.md`): build and bundle, grant
Accessibility/Screen Recording, run `vp computer --status`, exercise
permissions/apps/windows/snapshot against TextEdit, click a safe ref, type into a
scratch document, verify the `cmd+q` approval classification, and capture a
window screenshot artifact. CI does not grant TCC permissions or drive a GUI.

## Alternatives considered

1. **AppleScript through the shell tool.** Rejected: opaque host effects, hard to
   bound, easy to smuggle arbitrary app scripting, no typed snapshot/ref model.
2. **A separately installed GUI automation binary.** Rejected: path guessing,
   version skew, no daemon-owned lifecycle, and a second permission identity.
3. **Accessibility-only, no CGEvent.** Rejected: many apps expose incomplete AX
   actions; normalized CGEvent clicks are needed for canvas/web/native custom
   views. AX remains the snapshot/read layer.
4. **Sandbox the helper.** Rejected for v1: macOS TCC/AX/CGEvent/ScreenCaptureKit
   are not available inside the browser Seatbelt profile. The trust model is
   documented instead of misrepresented.
5. **Inline base64 screenshots.** Rejected: unbounded context cost, no artifact
   retention, and a direct path to leaking large screen captures into the model.

## Architecture at a glance

```text
┌──────────────────────────────────────────────────────────────────────┐
│ SwiftUI client / VP CLI                                             │
│   computer.status → helper availability + TCC preflight              │
└──────────────────────────────┬───────────────────────────────────────┘
                               │ local JSON-RPC
┌──────────────────────────────▼───────────────────────────────────────┐
│ verlinpadd                                                           │
│  agent loop → assess_tool_call("computer") → ApprovalMode             │
│  Engine::computer_action                                             │
│    ├─ ComputerManager: validation, deadlines, idle sweep              │
│    ├─ computer ComputerLease (SQLite, TTL, heartbeat, fail fast)      │
│    ├─ screenshot → artifact store (5 MiB, zstd, SHA-256)             │
│    └─ event stream (tool.started, permission.*, computer.lease.*)    │
└──────────────────────────────┬───────────────────────────────────────┘
                               │ stdio JSON lines (1 MiB in / 2 MiB out)
┌──────────────────────────────▼───────────────────────────────────────┐
│ Contents/Helpers/VerlinParkComputerHelper                            │
│  computer-core: action/ref/point/key/snapshot/path validation        │
│  Accessibility: AX tree, refs, windows, focus, waits                 │
│  CGEvent: click/type/key/scroll/drag on normalized targets           │
│  ScreenCaptureKit: bounded window/display PNG in scratch             │
│  parent watchdog + stdin EOF + close                                │
└──────────────────────────────┬───────────────────────────────────────┘
                               │ TCC (user authority, NOT sandboxed)
                    macOS Accessibility / Screen Recording
```

The daemon is the only writer of session state. The helper is an effect
executor with no database, event, artifact, or session access. It returns
bounded data to the manager, which re-validates app/window/snapshot/screenshot
shapes before rendering or publishing.

The browser helper and the computer helper share the `ComputerLease` pattern but
not a process: a browser lease does not serialize a computer action, because
they are different surfaces. The daemon-wide capacity, budget, approval,
Verifier, merge, and worktree gates are unchanged.

## Error and failure model

The manager distinguishes a *protocol* error from a *transport* failure:

| Code | Source | Helper kept? | Meaning |
| --- | --- | --- | --- |
| `INVALID_ARGUMENT` | daemon before spawn / helper | yes | malformed or unsupported action, point, ref, key, or cap |
| `PERMISSION_DENIED` | helper | yes | Accessibility or Screen Recording is not granted |
| `APP_NOT_FOUND` | helper | yes | bundle id or pid is not running |
| `WINDOW_NOT_FOUND` | helper | yes | window id/title is absent or not shareable |
| `ELEMENT_NOT_FOUND` | helper | yes | element condition did not resolve |
| `ELEMENT_NOT_ACTIONABLE` | helper | yes | ref exists but has no usable frame |
| `STALE_REF` | helper | yes | snapshot changed since the ref was issued |
| `SNAPSHOT_TOO_LARGE` | helper / daemon | yes | snapshot budget cannot fit even the root/window |
| `SNAPSHOT_REJECTED` | daemon | yes | daemon-side tree validation failed |
| `APPS_REJECTED` / `WINDOWS_REJECTED` | daemon | yes | malformed helper result |
| `DESTRUCTIVE_KEY_BLOCKED` | helper | yes | daemon did not mark the approved destructive key |
| `SCREENSHOT_FAILED` | helper | yes | ScreenCaptureKit or PNG encoding failed |
| `SCREENSHOT_TOO_LARGE` | helper | yes | PNG cannot be bounded under 5 MiB |
| `TIMEOUT` | helper / daemon | no | action or frame deadline elapsed |
| `PROTOCOL_ERROR` | helper / daemon | no | malformed frame, wrong id, oversized frame |
| `COMPUTER_BUSY` | daemon lease | no | another session holds the `computer` surface |
| `COMPUTER_HELPER_UNAVAILABLE` | daemon resolver | no | helper binary is missing or not executable |
| `COMPUTER_HELPER_FAILED` | daemon spawn | no | helper failed its ready handshake |

Transport failures kill the helper and release the lease so the next call starts
from a clean process. Typed errors keep the helper so an agent can take a fresh
snapshot or pick another window. The daemon never silently retries a mutating
action; a retry is a new tool call and a new approval decision.

## Observability and audit

Every agent call emits the existing `tool.started` and `permission.allowed` (or
`permission.denied`) events. The computer manager adds only bounded structured
payloads: app/window counts, snapshot node/byte counts, screenshot artifact
metadata, and error codes. It does not put the full AX tree, screen pixels, or
typed text into the event payload beyond what the normal tool result already
contains.

The `computer` lease emits `computer.lease.acquired`, `computer.lease.denied`,
`computer.lease.released`, and `computer.lease.expired` through the existing
store path. `computer.leases`, `computer.leases.sweep`, and
`computer.leases.release` are the operator controls. `computer.status` is
side-effect free except for launching a short-lived permission probe helper.

The status result deliberately reports `permissions_checked: false` when the
helper cannot run. A caller must not treat missing permissions as granted.

## Threat model

| Threat | Mitigation |
| --- | --- |
| Agent clicks a destructive control | `HostState` approval; destructive key combos are `Destructive`; no auto-approve for those risks |
| Agent reads a password field | secure role/subrole redaction in helper and again in daemon |
| Agent captures unrelated sensitive windows | explicit target, Screen Recording permission, bounded screenshot artifact, normal artifact retention |
| Agent escapes the worktree with screenshot output | daemon-generated relative path below helper scratch; helper and daemon canonicalize; symlink escapes rejected |
| Agent types unbounded text | 10 KiB cap, NUL rejection, helper chunking |
| Agent hangs the surface | per-action deadline plus idle sweep; transport failure kills the process group |
| Two sessions drive the GUI at once | single `computer` lease, fail-fast contention, TTL/heartbeat |
| Helper survives daemon exit | parent watchdog, stdin EOF, process-group kill |
| Helper becomes a shell bridge | no shell/AppleScript/URL/process APIs in the helper or protocol |
| Helper reads provider secrets | environment is cleared; only minimal PATH/HOME/TMP/LANG is passed |
| Stale AX element is used for a click | refs are snapshot-scoped; stale refs fail instead of falling back to a pointer |
| Malformed helper result overflows the event/context | frame caps, typed app/window/snapshot validation, per-field caps, byte budget |
| Coordinates land on another display/Space | normalized to a target window and checked against the current displays |
| Permission is silently absent | explicit preflight, structured denial, no AppleScript fallback |

Residual risk: the helper is a local process with the user's GUI authority. A
local attacker who can replace the helper binary, inject into the daemon, or
drive the user's session can already do equivalent things without this feature.
The design does not claim to defend against a compromised local account.

## Rollout and verification

1. Land the helper target, core tests, Rust manager, protocol status, and ADR.
2. Build the app and bundle the helper; verify `vp computer --status` reports
   `helper_available: true`.
3. Grant Accessibility and Screen Recording on a test machine. Run the manual
   smoke sequence in the helper README against TextEdit and a scratch document.
4. Confirm `smart` prompts for `focus`/`click`/`type`/`key` and allows
   `snapshot`/`windows` with an audit event.
5. Confirm `unattended` denies host-state and destructive calls and that
   `strict` prompts for external reads.
6. Confirm a screenshot becomes a session artifact and can be previewed/GC'd.
7. Confirm `close`, idle sweep, session worktree cleanup, and daemon shutdown
   release the lease and kill the process group.
8. Run the fake-helper Rust tests in CI; run GUI/TCC tests manually.

Rollback is additive: remove the `computer` tool schema/allowlist and the
helper target. Stored `computer.status`/lease events remain readable as ordinary
events and do not change existing session semantics.

## Compatibility

- The daemon JSON-RPC protocol remains version 1. `computer.status` is a new
  method with an optional params object; older clients ignore it.
- `ComputerStatusResult` fields are additive and optional where possible; the
  Swift client decodes missing permissions/path/error as nil.
- The helper JSON-lines protocol is versioned by its ready event. A future
  incompatible helper requires both sides to change; the manager rejects a
  missing `ready` event rather than guessing.
- No existing browser, shell, approval, merge, capacity, or budget behavior is
  changed. The `computer` surface is a new lease surface string only.
- The helper is copied into the existing app `Contents/Helpers` directory; no
  new launch agent, privileged helper, or network listener is installed.

## Manual smoke matrix

Run each row on a logged-in macOS machine after granting the permissions. CI
does not run this matrix.

| # | Action | Expected |
| --- | --- | --- |
| 1 | `permissions` | Accessibility/screen flags match System Settings; no prompt |
| 2 | `apps` | frontmost app is marked; pids are positive; no background apps unless requested |
| 3 | `windows` on TextEdit | a CGWindow id, title, finite bounds, layer, focus flag |
| 4 | `snapshot` on TextEdit | bounded `ax-…` refs; no raw pointers; secure fields are `***` |
| 5 | `focus` on TextEdit | app comes forward; response names the raised window |
| 6 | `click` a safe button ref | exactly one click; approval prompt in smart mode |
| 7 | `type` into a scratch document | text appears; 10 KiB cap is enforced |
| 8 | `key` Tab / Escape | key works; no destructive classification |
| 9 | `key` cmd+q | approval is `destructive`; unattended denies; helper receives the approved marker only after approval |
| 10 | `scroll` in a long document | bounded line/pixel scroll |
| 11 | `drag` within a window | from/to normalized points; off-screen endpoints are rejected |
| 12 | `wait` for an element | matches or returns `TIMEOUT` within the deadline |
| 13 | `screenshot` window | artifact id/path, finite dimensions, ≤5 MiB; no inline base64 |
| 14 | permission removed | next action returns `PERMISSION_DENIED`, no fallback |
| 15 | `close` | lease row disappears; helper scratch is removed |
| 16 | idle sweep / daemon exit | helper process group exits; lease expires or is released |

Record the macOS version, app, and helper path in bug reports. A screen capture
of a sensitive window is a data-handling action even though the tool itself is
classified as an external read.

## Performance and footprint

The helper is a single ~0.9 MB debug binary and has no persistent background
work: it sleeps on stdin between requests. A snapshot stops at the requested
budget, and the daemon renders at most 60 KB into the transcript while retaining
a bounded tree for the event stream. Screenshots are downscaled before PNG
encoding, so a 5K display does not produce a multi-megabyte model payload.

The manager holds at most one helper per session/worktree key and only while the
lease is alive. The idle sweep (default 600 seconds) bounds memory and process
count for abandoned sessions. App/window/snapshot responses are capped at 2 MiB
per frame; normal app lists are a few KB.

No provider/model request is made by this module. It adds no hidden routing and
does not consume the token budget by itself; only the normal tool result enters
the model context.

## Files

- `apps/macos/VerlinParkComputerHelper/` — packaged helper, protocol dispatch,
  Accessibility, CGEvent, ScreenCaptureKit, manual README.
- `apps/macos/VerlinParkComputerHelperCore/` — pure validators shared with tests.
- `apps/macos/VerlinParkComputerHelperTests/` — Swift unit tests; no GUI/TCC.
- `crates/vp-core/src/computer.rs` — manager, validation, lease, artifacts, tests.
- `crates/vp-protocol/src/types.rs` — `computer.status` result types.
- `crates/vp-core/src/agent.rs` — schema, dispatch, role allowlist.
- `crates/vp-core/src/permissions.rs` — risk map.
- `crates/vp-daemon/src/main.rs` — `computer.status` and idle sweeper.
- `crates/vp-cli/src/main.rs` — `vp computer --status`.
- `apps/macos/project.yml`, `scripts/bundle-daemon.sh`, `justfile` — packaging.

## Operator runbook

**Inspect the surface.** `vp computer` lists the current `computer` lease with
owner, session, agent, heartbeat, and expiry. `vp computer --status` reports
whether the helper is installed and whether TCC permissions are present. Neither
command posts input.

**Stuck lease.** First confirm the holder session is idle, failed, or
cancelled. `vp computer --release <lease-id>` is an explicit operator override;
the normal path is `close`, the idle sweep, or session cleanup. Do not release a
lease for a session that is actively running — the helper process may still be
alive and would become unowned.

**Helper crash loop.** Check stderr logs from the daemon (the helper's stderr is
forwarded at debug level). Common causes are a stale TCC entry after rebuilding,
a missing `Contents/Helpers` copy, or a ScreenCaptureKit framework mismatch.
`vp computer --status` distinguishes missing binary from failed handshake.

**Permission prompt does not appear.** The helper uses preflight checks and does
not call the prompting APIs. Add the helper manually in System Settings →
Privacy & Security → Accessibility and Screen Recording. Restart the daemon after
changing permissions so a fresh helper process gets the new TCC state.

**Screenshot artifact growth.** Screenshots use the normal artifact store and
retention/GC policy. A project with a very small artifact cap may reject the
publish; the helper file is deleted and the error is returned. Increase the
artifact policy or use a smaller `max_width`/`max_height`.

**Unexpected screen state.** If an agent focuses the wrong window, take a fresh
snapshot and prefer element refs over normalized points. Points are normalized to
the selected window and checked against displays, but another app can still
raise a window between the check and the click.

**Daemon shutdown.** In-flight computer calls are cancelled with the daemon
runtime. The helper's parent watchdog exits it within two seconds; the lease row
also stops heartbeating and expires after its TTL. Worktree cleanup additionally
calls `stop_session_computers` before removing session scratch.

**Rollback.** Remove the `computer` tool schema/allowlist and stop advertising
`computer.status`; the helper binary may remain packaged harmlessly. Existing
events and leases are ordinary rows and do not need migration.

## Terminology

- **Helper** — the packaged `VerlinParkComputerHelper` command-line process.
- **Surface** — an interactive resource serialized by `ComputerLease`; this ADR
  adds the `computer` surface alongside `browser`.
- **Ref** — a snapshot-scoped `ax-…` hash that resolves to an AX element in the
  helper's current table.
- **Normalized point** — an `(x, y)` pair in `0…1` resolved against a target
  window's frame, never an absolute screen coordinate.
- **Host state** — a risk class for actions that can change another application
  or the user's desktop; `smart` asks before these calls.
- **Destructive** — a risk class for shortcuts that can quit or log out
  applications; `smart` asks and `unattended` denies.
- **Transport failure** — EOF, timeout, malformed frame, or crash; the helper is
  killed and the lease is released.

## Acceptance checklist

- [x] Helper target builds and is copied into `Contents/Helpers`.
- [x] `permissions`, `apps`, `windows`, `focus`, `snapshot`, `screenshot`,
      `click`, `type`, `key`, `scroll`, `drag`, `wait`, and `close` exist.
- [x] Accessibility and Screen Recording fail closed with explicit errors.
- [x] Snapshot depth/element/byte caps and secure-field redaction exist in both
      helper and daemon.
- [x] Ref and normalized-point validation reject stale/invalid/off-screen input.
- [x] Text/key/scroll/drag/wait/window/app bounds are enforced.
- [x] Screenshot output is confined to scratch and published as an artifact.
- [x] `ComputerLease` fail-fast contention, explicit release, TTL crash path,
      idle sweep, and session cleanup are implemented.
- [x] `computer` is limited to Leader and coding Worker/Labour.
- [x] Risk classification covers read, host-state, and destructive actions.
- [x] Daemon protocol status method and CLI status flag are additive.
- [x] Rust tests, pure Swift tests, and manual smoke instructions exist.
- [x] ADR records the non-sandboxed trust model and its limits.

## Open follow-ups

These are intentionally out of scope for R4 and are listed so the trust boundary
is not overstated:

- Native GUI control on Windows/Linux.
- A menu-bar walker or AppleScript/Shortcuts bridge.
- Clipboard read/write, drag-and-drop file promises, and rich text selection.
- AX tree diffing between snapshots; current refs are snapshot-scoped.
- A user-facing permission onboarding flow; v1 uses System Settings plus
  `vp computer --status`.
- Distributed computer leases or remote GUI workers.
- Encrypted screenshot retention beyond the existing artifact policy.
- Automatic re-grant of TCC after helper replacement; macOS owns that state.
- Precise preemption of a long `wait`; v1 checks at action boundaries and the
  manager deadline, while the helper's sleep is not interrupted mid-call.

Reviewers should treat the helper binary and the daemon as the same trust
boundary for local GUI effects. A future change that adds network, shell, URL,
or file-system access to the helper would need a new ADR and a new sandbox
story; it must not be slipped into this protocol.

## Verification evidence

The implementation is considered complete for R4 when the following evidence
exists in the branch:

- `cargo test --workspace` passes, including the computer manager unit tests and
  the fake-helper lifecycle/action tests.
- `cargo clippy --workspace --all-targets -- -D warnings` passes.
- `cargo fmt --all -- --check` passes.
- `swiftformat --lint` passes for the app, browser, computer helper, core, and
  helper tests.
- `xcodegen generate` plus `xcodebuild ... test` passes both the existing app
  tests and the pure computer-helper tests.
- `just bundle` places `verlinpadd`, `VerlinParkBrowserHelper`, and
  `VerlinParkComputerHelper` in `Contents/Helpers`.
- The manual smoke matrix has been run on at least one logged-in macOS machine;
  CI does not claim TCC or GUI coverage.
- A screenshot smoke produces an artifact with a digest and dimensions, and a
  secure text field appears as `***` in a snapshot.

The branch intentionally reports the manual rows separately from CI so reviewers
do not mistake pure validation tests for proof that ScreenCaptureKit, AX, or TCC
work in every environment.

## Why not reuse the browser helper?

The browser helper uses WKWebView and a non-persistent web data store. It can
only see content that WKWebView loaded, and its Seatbelt profile deliberately
denies Apple events and worktree writes. Native apps are not web views. AX and
CGEvent require a process with Accessibility trust, and ScreenCaptureKit needs
Screen Recording trust; neither is available inside the browser profile.

Combining them would either (a) weaken the browser profile to host authority or
(b) force native actions through a web view, which cannot operate menus, native
dialogs, or other applications. Separate helpers keep the browser's narrow trust
story intact and make the native trust boundary explicit and reviewable.

The two helpers still share the `ComputerLease`, process-group cleanup, bounded
JSON-lines transport, and daemon-owned scratch conventions. That reuse is at the
manager and lifecycle level, not at the capability level.

## Why not expose absolute coordinates?

An absolute screen coordinate is ambiguous across displays, Spaces, scaling, and
window movement. A model can also copy a coordinate from one window and click a
different app. Normalized points are resolved against a freshly selected target
window, checked against the current display list, and rejected when off-screen.
Element refs are preferred because they resolve to an AX element rather than a
guess. Absolute coordinates would be a larger, less reviewable capability and
are intentionally absent.

## Testing the manager without a GUI

The Rust tests use fake executable shell helpers that speak the same JSON-lines
protocol. This exercises the real manager code — ready handshake, request ids,
frame parsing, typed errors, close, process-group cleanup, lease release, status
preflight, and screenshot publication — without granting TCC or driving a GUI.
The fake helper is intentionally not a mock of AX or ScreenCaptureKit; those
remain manual smoke rows.

The pure Swift core is compiled into a separate test bundle. It exercises request
limits, ref generation, snapshot budgets, secure-field redaction, path jails,
coordinate math, key policy, and action capabilities. That separation is why CI
can be meaningful without claiming GUI coverage.

## Summary for reviewers

If you remember only five things:

1. The helper is daemon-owned but **not sandboxed**; it has the user's GUI
   authority and every host-state action passes the existing approval policy.
2. `ComputerLease` serializes the `computer` surface and fails fast; it is not a
   queue or a distributed lock.
3. Snapshot refs are hashes, values are bounded and secure fields are `***`;
   screenshots are scratch-confined artifacts, never inline base64.
4. Accessibility and Screen Recording are explicit preflights with no fallback
   to AppleScript, shell, or an external automation binary.
5. CI proves the protocol/manager/pure policy; GUI, TCC, AX, CGEvent, and
   ScreenCaptureKit behavior is a documented manual smoke matrix.

## Compatibility matrix

| Component | Requirement | Failure mode |
| --- | --- | --- |
| macOS | 15.0+ (deployment target) | helper build fails or ScreenCaptureKit API unavailable |
| CPU | arm64 (current app build) | helper target follows the app architecture |
| Daemon protocol | v1 | `computer.status` is additive |
| Swift | 6 language mode, strict concurrency minimal | callbacks use bounded semaphores and serial action queue |
| TCC | Accessibility + Screen Recording | explicit structured denial |
| GUI session | logged-in, unlocked | AX/ScreenCaptureKit fail or return no content |
| ComputerLease | SQLite row, TTL/heartbeat | crash path expires; no distributed fencing |
| App bundle | helper under `Contents/Helpers` | resolver fails closed with unavailable error |

## References

- `docs/LONG_TASK_RUNTIME_PLAN.md` R4.
- `crates/vp-core/src/browser.rs` — helper lifecycle and `ComputerLease` pattern.
- `crates/vp-core/src/console.rs`, `terminal.rs` — managed child cleanup.
- `crates/vp-core/src/permissions.rs` — risk and approval policy.
- `apps/macos/VerlinParkComputerHelper/README.md` — protocol and manual smoke.

# VerlinParkComputerHelper

`VerlinParkComputerHelper` is the daemon-owned macOS helper behind the
`computer` agent tool. It is a command-line tool, not a GUI app bundle. It uses
Accessibility (`AXUIElement`), `CGEvent`, and ScreenCaptureKit to inspect and
operate native applications.

The helper is **not sandboxed**. It runs as the logged-in user because macOS
does not expose Accessibility, global input posting, or ScreenCaptureKit through
a Seatbelt profile. The Rust daemon launches it, applies the existing approval
policy before every host-state action, and serializes the surface with the
`computer` `ComputerLease`. See `docs/adr/0158-native-macos-computer-control.md`.

## Version

The helper protocol version is `1`. The daemon rejects a helper that does not
emit a `ready` event; it does not attempt to speak an unversioned protocol. A
future incompatible change must bump the version and update both sides together.

The version is carried in the startup event, not in every request, so the
daemon can fail before dispatching an action to an incompatible helper.

## Status

This helper is part of the M2 four-tier delegation runtime and implements the R4
native computer-control workstream. It is additive to the existing browser
helper. The daemon exposes it through the `computer` agent tool to the Leader and
to coding Worker/Labour profiles only.

The feature is safety-first, not sandbox-first: it does not claim that native
GUI control is confined. It does claim that the daemon owns the process, that
every host-state action is approval-classified, that bounds and redaction are
enforced twice, and that the surface is serialized by a single lease.

## Build

```bash
cd apps/macos
xcodegen generate
xcodebuild -project VerlinPark.xcodeproj -scheme VerlinPark \
  -configuration Debug -destination 'platform=macOS' \
  -derivedDataPath build CODE_SIGNING_ALLOWED=NO build
```

`just bundle` copies the helper to
`VerlinPark.app/Contents/Helpers/VerlinParkComputerHelper` next to
`verlinpadd`. The daemon also discovers a development build under
`apps/macos/build/Build/Products/{Debug,Release}/`, or an explicit
`VERLINPARK_COMPUTER_HELPER` path.

## Required permissions

Grant these to the **helper binary** (not the SwiftUI app) in
System Settings → Privacy & Security:

- **Accessibility** — apps, windows, snapshots, focus, click, type, key, scroll,
  drag, and element waits.
- **Screen Recording** — screenshots only.

The `permissions` action never prompts. It reports the current TCC state and the
exact settings paths. Every other action fails closed with
`ACCESSIBILITY_DENIED` or `SCREEN_RECORDING_DENIED` when its permission is
missing.

The daemon-side status path is:

```bash
vp computer --status
```

It launches the helper only to preflight permissions. It does not take the GUI
lease and does not post input.

## JSON-lines protocol

The helper writes one JSON object per line to stdout. Diagnostics go to stderr.
The daemon writes one request per line to stdin.

Ready event:

```json
{"event":"ready","version":"1","pid":321,"scratch":"/…/scratch","accessibility_trusted":true,"screen_recording":true}
```

Request:

```json
{"id":"1","action":"snapshot","params":{"bundle_id":"com.apple.TextEdit","max_depth":8,"max_elements":300}}
```

Success:

```json
{"id":"1","ok":true,"result":{…}}
```

Error:

```json
{"id":"1","ok":false,"error":{"code":"PERMISSION_DENIED","message":"Accessibility permission is required…"}}
```

Requests are capped at 1 MiB. Responses are capped at 2 MiB. The helper exits if
stdin closes, after a `close` action, or when the daemon parent disappears.

## Lifecycle

The daemon manager owns one helper per session/worktree key while the
`computer` lease is valid:

```text
agent tool call
   │
   ├─ validate action/params (Rust)
   ├─ approval policy (RiskLevel)
   ├─ acquire `computer` ComputerLease (fail fast)
   ├─ spawn helper with scratch + parent pid + cleared environment
   ├─ read bounded ready event
   ├─ write bounded request, read bounded response by id
   ├─ render bounded tool result / publish screenshot artifact
   └─ release on close, idle sweep, session cleanup, error, or daemon exit
```

If the agent asks for `close`, the manager removes the helper from its map,
sends `close`, waits briefly, kills the process group, releases the lease, and
removes scratch. The idle sweeper (`VERLINPARK_COMPUTER_IDLE_SECS`, default 600)
does the same after inactivity. Session worktree cleanup calls
`stop_session_computers` before removing the session scratch.

A timeout, EOF, oversized frame, or invalid JSON is a transport failure: the
manager kills the process group and releases the lease. Typed helper errors
(`STALE_REF`, `WINDOW_NOT_FOUND`, `PERMISSION_DENIED`, …) keep the helper alive
so the agent can take a fresh snapshot or choose another target.

## Actions

| Action | Purpose | Daemon risk |
| --- | --- | --- |
| `permissions` | Accessibility + Screen Recording status | external |
| `apps` | Running applications, bundle id, pid, frontmost | external |
| `windows` | Windows, CGWindow id, title, bounds, layer, focused | external |
| `focus` | Activate an app and raise a window | host state |
| `snapshot` | Bounded AX tree with stable `ax-…` refs | external |
| `screenshot` | Bounded PNG in daemon scratch | external |
| `click` | Element ref or normalized window point | host state |
| `type` | Bounded Unicode text into the focused element | host state |
| `key` | Whitelisted key + modifiers | host state |
| destructive `key` | cmd+q, cmd+shift+delete, cmd+option+escape, ctrl+cmd+q | destructive |
| `scroll` | Bounded line/pixel wheel event | host state |
| `drag` | Bounded normalized drag | host state |
| `wait` | Element/window/frontmost condition with timeout | external |
| `close` | Stop the helper | read only |

Element references are a hash of the process, window index, role, subrole,
Accessibility identifier, and index path. They are stable across snapshots while
the path is unchanged, are not raw pointers, and become `STALE_REF` after the
tree changes. A new snapshot replaces the reference table.

Passwords and secure text fields are rendered as `***` before any bounding.
Snapshot depth, element count, per-field characters, and total bytes are capped
in the helper and again in the daemon.

## Screenshots

The daemon supplies a relative output name below `<scratch>/screenshots/`. The
helper re-validates that path, captures a window (default) or the main display,
scales to the requested bounds, and writes PNG bytes atomically. The daemon
re-checks the path, 5 MiB cap, dimensions, and SHA-256, then publishes the bytes
through the session artifact store. Screenshots are never returned as unbounded
inline model data.

## Request examples

The daemon builds these requests; they are shown here for debugging a direct
helper session. All actions are bounded by `ComputerCore.swift`.

```json
{"id":"1","action":"permissions"}
{"id":"2","action":"apps","params":{"include_background":false}}
{"id":"3","action":"windows","params":{"bundle_id":"com.apple.TextEdit","include_offscreen":false}}
{"id":"4","action":"focus","params":{"bundle_id":"com.apple.TextEdit","window_title":"Untitled"}}
{"id":"5","action":"snapshot","params":{"bundle_id":"com.apple.TextEdit","max_depth":8,"max_elements":300,"max_bytes":131072}}
{"id":"6","action":"click","params":{"ref":"ax-0123456789abcdef"}}
{"id":"7","action":"click","params":{"x":0.5,"y":0.5,"bundle_id":"com.apple.TextEdit","window_id":42}}
{"id":"8","action":"type","params":{"text":"hello"}}
{"id":"9","action":"key","params":{"key":"Tab"}}
{"id":"10","action":"key","params":{"key":"q","modifiers":["command"],"allow_destructive":true}}
{"id":"11","action":"scroll","params":{"dy":-5,"unit":"line"}}
{"id":"12","action":"drag","params":{"from_x":0.2,"from_y":0.2,"to_x":0.8,"to_y":0.8,"duration_ms":300,"steps":12}}
{"id":"13","action":"wait","params":{"condition":"element","role":"AXButton","title":"Save","timeout_ms":5000}}
{"id":"14","action":"screenshot","params":{"target":"window","max_width":1600,"max_height":1200,"output":"screenshots/debug.png"}}
{"id":"15","action":"close"}
```

A `key` request without `allow_destructive` is refused for `cmd+q`,
`cmd+shift+delete`, `cmd+option+escape`, and `ctrl+cmd+q`. The daemon sets that
marker only after its approval decision; direct clients should leave it out.

Screenshot output must be exactly `screenshots/<name>.png` with no `.`/`..`
components or absolute prefix. The manager additionally requires the returned
path to canonicalize below its own helper scratch directory.

## Manual smoke test (not CI)

CI covers the pure Swift validation/bounding helpers and the Rust manager
protocol. It does **not** grant TCC permissions or drive a GUI. Run this manually
on a logged-in macOS session:

1. Build the helper and bundle it:

   ```bash
   just build
   just bundle
   ```

2. Start the daemon and check installation:

   ```bash
   vp daemon
   vp computer --status
   ```

3. Grant Accessibility (and Screen Recording for screenshots) to
   `VerlinPark.app/Contents/Helpers/VerlinParkComputerHelper`, then run
   `vp computer --status` again. Both values should be `true`.

4. Exercise the protocol directly. Use a throwaway scratch directory and a
   harmless target such as TextEdit:

   ```bash
   SCRATCH="$(mktemp -d)"
   HELPER="apps/macos/build/Build/Products/Debug/VerlinParkComputerHelper"
   printf '%s\n' \
     '{"id":"1","action":"permissions"}' \
     '{"id":"2","action":"apps"}' \
     '{"id":"3","action":"windows","params":{"bundle_id":"com.apple.TextEdit"}}' \
     '{"id":"4","action":"snapshot","params":{"bundle_id":"com.apple.TextEdit","max_depth":6,"max_elements":100}}' \
     '{"id":"5","action":"close"}' \
   | VERLINPARK_COMPUTER_HELPER=1 "$HELPER" --scratch "$SCRATCH" --parent-pid $$
   ```

5. In a VerlinPark session, ask the Leader to run `computer` with
   `{"action":"permissions"}`, then `{"action":"windows"}`, then a `snapshot` of
   a safe app. Confirm the transcript shows bounded AX nodes and `ax-…` refs.

6. Click a non-destructive element by ref, then type into a scratch TextEdit
   document. Confirm the approval prompt appears in `smart` mode for the input
   action, and that `cmd+q` is classified as destructive.

7. Capture `{"action":"screenshot","target":"window"}` and confirm the result is
   an artifact id/path, not inline base64.

Clean up the helper after the test:

```bash
rm -rf "$SCRATCH"
```

## Troubleshooting

**`COMPUTER_HELPER_UNAVAILABLE`** — the daemon cannot find an executable helper.
Run `just bundle`, or set `VERLINPARK_COMPUTER_HELPER` to the built binary. The
daemon fails closed rather than falling back to AppleScript or `cliclick`.

**Accessibility says `not granted` after rebuilding** — TCC identifies the
binary. Remove the old helper entry in System Settings, re-add the current
`Contents/Helpers/VerlinParkComputerHelper`, and restart the daemon.

**Screen Recording is granted but screenshots fail** — ScreenCaptureKit can
refuse a window that is minimized, protected, or owned by a different Space.
Try `target: "screen"` to confirm the permission is working, then focus the
target window before capturing it.

**A ref becomes `STALE_REF`** — take a new snapshot. The helper intentionally
does not keep raw AX pointers across snapshots; refs are hashes of the AX path.

**`COMPUTER_BUSY`** — another session holds the `computer` lease. Wait for the
idle sweep, close that surface, or use `vp computer --release <lease-id>` after
confirming the holder is gone.

**`WINDOW_NOT_FOUND` with a valid `window_id`** — the window may be off-screen,
minimized, or exposed by a different app process. `windows` returns only
Accessibility-visible windows.

**Click lands on the wrong window** — pass `bundle_id` and `window_id` with a
normalized point, or click an element ref instead. Points are normalized to the
target window and checked against the current displays; they are never absolute
screen coordinates.

**The helper exits immediately** — it requires `VERLINPARK_COMPUTER_HELPER=1`
and `--scratch`; direct invocation without those is intentionally refused. The
daemon always supplies both.

## Change checklist

Any change to this helper should answer:

- Does a new action have a daemon risk classification and a role allowlist?
- Does it validate refs/points/text/keys/windows before posting an event?
- Does it enforce Accessibility or Screen Recording explicitly, with no
  AppleScript or shell fallback?
- Does it stay inside the JSON-lines frame, snapshot byte, text, and screenshot
  caps?
- Does it write only below `<scratch>/screenshots/` and return a path the daemon
  re-validates?
- Does it redact secure fields before any string cap or serialization?
- Does it avoid new network access, process launching, URL opening, or Apple
  event delivery?
- Does it release the lease and kill the process group on every failure path?
- Is there a pure Swift test, a Rust validation test, or a manual smoke step?
- Is the trust model still "daemon-owned process with user authority", not a
  claim of sandboxing?

If an action cannot be bounded, it should not be added to this helper.

## Protocol safety details

- The helper does not parse or execute any request-supplied script, path,
  command, URL, AppleScript source, or environment variable.
- `screenshot.output` is the only request-supplied path. It must be a relative
  `screenshots/*.png`; the helper canonicalizes the parent and refuses symlink
  escapes. The daemon rejects arbitrary paths before sending them.
- `key.allow_destructive` is a boolean marker set by the daemon only after its
  approval decision. The helper refuses destructive combinations when it is
  absent. A direct client cannot use it to bypass the daemon's approval because
  the daemon is the only agent-facing path.
- Element refs are validated for shape and resolved only against the latest
  snapshot table. They cannot be used to address an AX element by pointer.
- `wait` is a polling action with a fixed timeout; it cannot run indefinitely or
  execute a callback.
- Any response larger than 2 MiB is replaced by a `RESPONSE_TOO_LARGE` error.
  Snapshot and screenshot caps are much smaller in normal operation.

## Limits

- A logged-in GUI session is required. The helper cannot drive the login window
  or a locked screen.
- Native GUI control is not a sandbox and is not isolated from the user's
  credentials, clipboard, or other applications.
- `ComputerLease` is a single-daemon SQLite lease with TTL/heartbeat. It is not
  a distributed fence.
- ScreenCaptureKit may omit some protected or minimized windows.
- Accessibility trees vary by app; some apps expose only a shallow tree or no
  stable identifiers.
- There is no menu-bar walker, AppleScript bridge, shell execution, URL opener,
  or process launcher in this helper.

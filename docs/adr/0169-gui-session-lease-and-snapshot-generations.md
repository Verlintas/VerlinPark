# ADR-0169: GUI session lease and snapshot generations

- **Status:** Accepted
- **Date:** 2026-11-11
- **Owners:** Runtime, macOS, Protocol
- **Related:** ADR-0026, ADR-0066, ADR-0092, ADR-0134, ADR-0135, ADR-0137, ADR-0158

## Context

The browser helper (`browser`) and the native computer helper (`computer`) both
drive the same physical GUI: display focus, keyboard focus, the Accessibility
tree, ScreenCaptureKit, and CGEvents. Each helper acquired its own
`ComputerLease` surface. Two sessions could therefore hold `browser` and
`computer` rows at the same time and race for the user's screen, keyboard, and
security dialogs.

The original lease was also tied to helper lifetime. A warm helper retained the
global lease for up to 600 idle seconds. A second session received an immediate
`COMPUTER_BUSY` string with no holder identity, no retry hint, and no bounded
wait. The only remedy was a manual operator release or waiting for the 120s
lease TTL.

Finally, computer snapshots had a millisecond `generation` field, but refs were
deterministic hashes with no generation binding. A model could reuse a ref from
an older snapshot; the helper resolved it against its latest ref map and either
acted on an unintended element or returned an ambiguous error. There was no
typed stale-ref contract, no sensitive-target policy, no bounded action
timeline, and `computer.status` spawned a temporary helper on every dashboard
poll.

## Decision

### 1. One logical `gui` surface

`computer_leases.surface` is now written as `gui` for both browser and computer
actions. The SQLite row remains the authority for ownership. Acquisition checks
`gui`, `browser`, and `computer` rows together, so a legacy row from an older
daemon blocks a new acquisition until it expires or an operator releases it.

At daemon startup, legacy `browser`/`computer` rows are retired with a
`computer.lease.expired` event. Helper processes are daemon children, so no
legacy helper survives a restart. A legacy row that appears later (for example
from an older process sharing the data directory) still blocks new work and is
visible through `computer.leases` and `gui.status`.

The lease remains a single-daemon SQLite lease with a heartbeat. It is not a
distributed lock, not a cross-host queue claim, and not an exactly-once fencing
token for external effects. Two daemon processes do not share an in-memory FIFO;
the second receives a typed `GUI_BUSY` with the SQLite holder and a retry hint.

### 2. Grace release, warm helpers, bounded wait

The lease and the helper have separate lifetimes:

- an action acquires the lease (or reenters the same session's lease);
- after the action, the lease is retained for `GuiPolicy.grace_secs` (default
  45s, range 0–600);
- a short daemon sweeper releases the lease when grace expires, or `close`,
  session cleanup, cancellation, or the daemon's normal cleanup releases it
  immediately;
- the helper process stays warm for its own idle window (default 600s) and is
  reused on the next action, which reacquires the lease.

`GuiPolicy.wait_ms` (default 15s, range 0–60000) bounds the wait. The wait queue
is a deterministic FIFO inside one daemon. A waiter that times out is removed and
receives a structured `GUI_BUSY` with:

- `holder.session_id`, `holder.agent_id`, `holder.agent_name`,
  `holder.surface`, and `holder.expires_at`;
- `retry_after_ms` derived from the holder's remaining grace or lease expiry;
- `waited_ms`, `wait_ms`, and `queue_position`;
- `lease_surface: "gui"`.

There is no hidden infinite wait and no automatic retry. Same-session reentry is
immediate while the SQLite row is alive and may switch between browser and
computer; that is how a multi-step session avoids self-contention. An operator
release invalidates the row, and the next action reacquires it.

### 3. Snapshot generations and stale refs

Every successful computer snapshot increments a helper-local generation and
returns `snapshot_generation`. Refs are formatted as:

```
ax-<8 hex generation>-<16 hex hash>
```

The helper records the refs emitted by the latest snapshot. A ref from an older
generation, or a ref that is not in the latest snapshot, is rejected before any
input is posted. The helper returns typed `STALE_REF` with:

- the current `snapshot_generation`;
- the `ref_generation` parsed from the supplied ref;
- a `hint` to take a fresh snapshot;
- `auto_retry: false`.

The Rust manager passes the helper's details through as structured tool output:
`code`, `snapshot_generation`, `hint`, `retryable`, `auto_retry: false`, and
`fresh_snapshot_required: true`. It never retries click/type/key/scroll/drag on a
stale ref. Snapshots are the only refresh path; `wait` with a ref is also
rejected when the ref is stale.

Legacy deterministic refs without a generation remain parseable for warm
helpers built before this ADR, but they are still checked against the helper's
latest ref set and rejected with the same typed error when absent.

### 4. Sensitive targets

`GuiPolicy.sensitive_mode` is `deny` by default and may be
`require_approval`. Built-in sensitive bundle ids include System Settings,
SecurityAgent, Keychain Access, Passwords, loginwindow, 1Password, Bitwarden,
LastPass, Dashlane, NordPass, and Keeper. Built-in title patterns include the
corresponding app/window names and security panes. A project may add bundles and
titles; it cannot remove the built-ins through this policy.

Enforcement has two layers:

1. The Rust permission assessor classifies an explicitly named sensitive target
   as `Destructive` (approval in `smart`/`strict`, denial in `unattended`) in
   `require_approval` mode, and as a deterministic `blocked` assessment in
   `deny` mode. A full-screen screenshot is always `HostState` because it can
   expose unrelated windows.
2. The helper parses the same policy from the request and resolves the actual
   target (ref owner, window owner, or frontmost app) before snapshot, screenshot,
   focus, click, type, key, scroll, or drag. It refuses with
   `SENSITIVE_TARGET` before touching the target. `require_approval` permits an
   action only when the daemon marked an explicitly named target; a ref-only
   action must be reissued with `bundle_id`/`window_id` so the approval layer can
   see it.

Target resolution uses Accessibility and CGWindow metadata. It is a
best-effort preflight, not a security boundary: a coordinate click can hit a
different window if the screen changes between preflight and event posting, and
the helper is not sandboxed. Sensitive capture of a target that is not
recognized by bundle/title remains the operator's responsibility; the built-in
list is intentionally conservative and project-extensible.

### 5. Bounded action timeline

Every GUI action emits one `gui.action` event with:

- action, surface, session/agent;
- target app, bundle id, and (for non-sensitive targets) window title;
- `snapshot_generation`, `lease_generation`, queued time, request latency;
- result and typed error code.

Typed text, element values, page text, and screenshot bytes are never included.
Sensitive target window titles are omitted. `gui.timeline` reads a bounded,
newest-first query (default 100, max 500), optionally scoped to one session.
Manage Dash and the Stage pill render the same metadata. Events are retained with
the session event stream; the read path is bounded but the timeline is not a
separate retention domain.

### 6. Status cache

`computer.status` caches helper/TCC state for `GuiPolicy.status_ttl_secs`
(default 30s, range 5–600). The cache is process-local; `refresh: true` bypasses
it explicitly. A dashboard refresh returns `cached: true` and `cache_age_ms`
without spawning a helper. `gui.status` combines the cache with the lease
holder, FIFO, helper warmth, browser availability, and policy. Browser status is
inspection-only and never spawns a helper.

### 7. Protocol and RPC

New fields are additive and `serde(default)`:

- `GuiPolicy`, `GuiSensitiveMode`, `GuiPolicyParams`, `SetGuiPolicyParams`,
  `GuiPolicyResult`;
- `GuiLeaseInfo`, `GuiQueueInfo`, `GuiStatusParams`, `GuiStatusResult`;
- `GuiActionInfo`, `GuiTimelineParams`, `GuiTimelineResult`;
- `ComputerStatusResult.cached`, `cache_age_ms`, `grace_secs`, `wait_ms`,
  `snapshot_generations`;
- `ComputerStatusParams.refresh`.

RPCs:

- `gui.status`, `gui.timeline`, `gui.policy.get`, `gui.policy.set`;
- existing `computer.status`, `computer.leases`, `computer.leases.sweep`,
  `computer.leases.release` remain.

CLI: `vp gui`, `vp gui-policy`, `vp gui-policy-set`, plus
`vp computer --status --refresh`. UI: Manage Dash → Team (live GUI surface) and
Policies (GUI policy), and a Stage header pill.

### 8. Trust model

The helper remains unsandboxed and TCC-bound. The browser helper remains under
its Seatbelt write profile. The GUI lease serializes ownership; it does not grant
permissions, sandbox a helper, prevent a malicious local process from using the
same Accessibility permissions, or make a screenshot safe. Approval, Seatbelt,
path-jail, write-scope, Verifier, merge, capacity, and budget gates are
unchanged. The sensitive list reduces accidental automation of security UI; it
is not a complete deny-by-default macOS automation sandbox.

## Operational sequences

### Normal multi-step session

1. The agent calls `computer snapshot` or `browser snapshot`.
2. The manager loads the project `GuiPolicy` and calls `gui_begin_action`.
3. The coordinator finds no holder and creates a `gui` SQLite row with the
   next persisted `generation`.
4. The helper request runs and the manager records a `gui.action` event.
5. The coordinator refreshes the holder's last-action time. The lease remains
   held for `grace_secs`.
6. If another action arrives from the same session, reentry is immediate; it
   may switch browser/computer and keeps the lease generation.
7. If grace expires, the 1s sweeper releases the lease and wakes the first
   waiter. The helper remains warm.

### Contention

1. Session B calls an action while session A holds the lease.
2. The coordinator enqueues B with a FIFO position and waits up to
   `wait_ms`.
3. If A releases, the coordinator wakes B; B tries the SQLite row and either
   acquires it or re-checks once more if another waiter won.
4. If the timeout expires, B is removed from the queue and receives
   `GUI_BUSY` with A's identity and a retry hint.
5. A different daemon process does not join the FIFO. It receives
   `GUI_BUSY` immediately from the SQLite contention path.

### Operator release

`computer.leases.release` deletes the row. The next acquisition from any
session checks the row for a live in-memory holder and clears it instead of
waiting out grace. The same check runs on every action, so an operator release
or a sweep does not leave the coordinator believing it still owns the surface.

### Close, cancel, session cleanup

- `computer close` / `browser close` shut down the helper and release the lease
  immediately.
- `session.run.cancel_requested` releases the lease immediately (grace is for
  normal interaction, not for a cancelled run).
- `stop_session_browsers` / `stop_session_computers` release immediately before
  scratch/worktree cleanup.
- A daemon crash leaves the SQLite row to expire after the 120s lease TTL.
  Helpers are daemon children and exit with the daemon.

## Failure modes

| Symptom | Response | Operator action |
| --- | --- | --- |
| `GUI_BUSY` with holder and retry | Waiter timed out; no automatic retry | Wait, or release the holder from Manage Dash → Team if it is stuck |
| `GUI_QUEUE_FULL` | 64 local waiters, including stale waiters | Inspect the holder; queue bounds prevent memory growth |
| `GUI_LEASE_FAILED` | SQLite acquisition failed (not ordinary contention) | Check the daemon log/data directory |
| `STALE_REF` | Ref generation or membership mismatch; action not posted | Take a fresh snapshot and use the new ref |
| `SENSITIVE_TARGET` | Helper refused before acting | Use an explicit target and approval, or change the project policy |
| `COMPUTER_HELPER_START_FAILED` | Helper did not emit `ready` | Check the packaged helper and TCC permissions |
| `COMPUTER_HELPER_UNAVAILABLE` | Binary missing | Install the helper or set `VERLINPARK_COMPUTER_HELPER` |
| `DISPLAY_NOT_FOUND` | `display_id`/`display_index` is stale | Run `computer displays`, then retry the screenshot |
| `PERMISSION_DENIED` | Accessibility/Screen Recording not granted | Grant TCC permission to `VerlinParkComputerHelper` |
| Status `cached: true` | Dashboard used the TTL cache | Pass `refresh: true` for an explicit probe |

## Operator runbook

```bash
# Show the unified holder, FIFO, helper warmth, TCC state, and policy.
vp gui --project <project-id>

# Show the bounded timeline for one session.
vp gui --timeline --session <session-id> --limit 100

# Show or change the project policy.
vp gui-policy <project-id>
vp gui-policy-set <project-id> --grace-secs 30 --wait-ms 5000   --sensitive-mode require_approval   --sensitive-bundle com.example.Vault

# Inspect/release/sweep raw lease rows when the UI is unavailable.
vp computer
vp computer --release <lease-id>
vp computer --sweep
vp computer --status --refresh

# Deep status with the raw cached computer payload.
vp gui --project <project-id>
```

The timeline never contains typed text, AX values, page text, or screenshot
bytes. It may contain app names, bundle ids, and non-sensitive window titles.
Sensitive-target titles are omitted; the bundle id and `sensitive_target` flag
remain for audit.

## Persistence and replay

- `gui.action` is a normal session event; it is mirrored to the per-session
  JSONL log and included in replay/repair event scans. It is not a projection,
  so a repair does not rewrite it.
- The lease row is operational state, not a replay projection. It is cleared on
  startup recovery for this process owner and expired by the sweeper.
- The lease `generation` is persisted through `store_meta` so it remains
  monotonic across deletes and reopen; the row value is the generation of the
  current acquisition.
- `GuiPolicy` is project policy, stored in SQLite and `projects.jsonl`, and
  restored by `vp db-rebuild`. It is not a session event.
- The status cache is process-local and disappears on restart; it never
  changes ownership.

## Test matrix

| Layer | Coverage |
| --- | --- |
| `vp-store` | policy round-trip/defaults, manifest rebuild, legacy contention/retirement, generation monotonicity, timeline limit/session filter, expiry |
| `vp-protocol` | additive defaults for policy/status/timeline/lease generation, round-trip, unknown-field tolerance |
| `vp-core` coordinator | one lease across browser/computer, FIFO, reentry, grace wake, zero wait, queue bound, operator release, legacy contention, generations, timeline/redaction, policy validation |
| `vp-core` managers | warm helper across lease release, crash respawn, stale-ref structured/no-retry, sensitive params/ack, status cache/refresh/concurrency, browser/computer contention, displays target |
| `vp-core` integration | cross-engine holder, five-waiter FIFO, grace across engines, manifest rebuild, durable timeline, status cache, cleanup release, persisted generation |
| Swift app | policy/status/timeline decode, legacy defaults, unknown mode fail-closed, snake-case encode, release model |
| Swift helper core | generation refs, stale-ref details, sensitive matching/parse bounds, generation counter, error envelope details, display selectors, wait deadline |
| Manual smoke | real AX/TCC helper, window/display screenshot, sensitive dialog refusal, AppKit notification observation |

## Known limitations

- **Single daemon only.** The FIFO is in process memory. The SQLite row gives
  cross-process exclusion and a typed busy result, but no cross-process wait
  queue, fair ordering, or distributed fencing. A second daemon on the same
  data directory cannot join the FIFO.
- **No cross-host GUI.** Remote workspaces (`ExecutionHost::ssh`) do not expose
  the local GUI. A remote helper would be a different trust problem and is not
  implemented.
- **Best-effort target resolution.** The helper resolves the target from a ref,
  window id, bundle/pid, or the frontmost app. A coordinate action can land on
  a different window if the screen changes between preflight and event posting.
  The helper is not sandboxed; Accessibility/CGEvent are host-level authority.
  A malicious local process can bypass this policy entirely.
- **Observer limits.** `computer wait` uses an AX observer when it can create
  one, but AppKit does not guarantee a notification for every visual change.
  Polling remains the fallback and the timeout is authoritative. Wait is
  bounded by the helper request deadline and does not survive helper restart.
- **Sensitive list is not exhaustive.** Built-ins cover common security
  settings and password managers, and projects can extend the list. A custom
  or renamed security UI that is not in the list is not detected. Read capture
  of a sensitive target requires an explicit, approvable target; a full-screen
  screenshot is always a host-state approval in the risk policy.
- **Legacy rows are not migrated.** They remain readable, block new `gui`
  acquisitions, and expire or are retired at startup. There is no conversion of
  an old holder into the new queue.
- **Status cache is process-local.** It reduces dashboard helper spawns but does
  not guarantee a fresh TCC answer; `refresh: true` is the explicit probe.
- **Timeline retention.** The read API is bounded (default 100, max 500), but
  `gui.action` events share the session event log and are not a separate
  retention domain. Operators should not treat the timeline as a complete
  forensic recording of all GUI activity.
- **No screenshot redaction.** The daemon stores the captured PNG as an
  artifact. The timeline omits content, but the artifact itself is the screen
  content and follows the normal artifact retention policy.
- **Generation scope.** The generation is monotonic across the SQLite database,
  not a fencing token for external effects. A helper restart resets its own
  snapshot generation and rejects old refs by membership as well as generation.
- **Unattended mode.** Sensitive targets and full-screen screenshots are denied
  because there is no human to approve them. Ordinary window capture and input
  still require the session's normal host-state policy.

## Migration notes

- The `computer_leases` table gains a `generation INTEGER NOT NULL DEFAULT 0`
  column through the existing `ensure_column` migration. Older rows decode
  with generation 0; the next acquisition starts from the persisted
  `store_meta.computer_lease_generation` counter and the current maximum.
- A `gui` acquisition checks the `gui`, `browser`, and `computer` surfaces
  together. A database with only a legacy row therefore blocks new GUI work
  until the row expires or is retired. On daemon startup, legacy rows are
  removed with `computer.lease.expired` and `legacy_surface: true`.
- `GuiStatusResult.holder.lease_generation` is the persisted SQLite generation,
  not a per-coordinator counter. A daemon restart therefore does not reset the
  value an operator sees.
- `project_gui_policies` is a JSON policy table like
  `project_orchestration_policies`; missing rows mean the default policy. The
  project manifest carries `gui_policy`, and `vp db-rebuild` restores it.
- Existing clients that decode `ComputerStatusResult` without the new fields
  keep working because every added field is `serde(default)` and the legacy
  `lease_surface: "computer"` string is still readable. New daemons report
  `"gui"`.
- Existing browser/computer tool schemas gain `displays` and screenshot display
  selectors only; there is no new agent tool name and no hidden model routing.
- Approval, Seatbelt write confinement, path jail, write scopes, Verifier,
  merge, capacity, and budget gates are unchanged. A legacy helper process that
  survives inside a running daemon is still blocked by its legacy row.

## Consequences

- Browser and computer can no longer drive the GUI concurrently.
- Multi-step GUI sessions stay fast (warm helper, grace lease) without starving
  other sessions for the full helper idle window.
- A second session gets a deterministic holder/retry result instead of an
  opaque busy string, but only within one daemon process.
- Ref reuse across snapshots is a typed error, not an accidental click.
- Sensitive security/password targets are denied by default and require an
  explicit, approvable target in `require_approval` mode.
- Operators can inspect a bounded, screen-content-free GUI timeline.
- Status polling no longer spawns a helper per dashboard refresh.
- Legacy rows remain readable and safe but are not migrated into the new queue;
  they expire or are retired at startup.
- The queue does not coordinate across hosts or daemon processes, and helper
  target resolution is best-effort rather than a sandbox.

## Alternatives considered

- **Keep separate `browser`/`computer` leases and add a second mutex.** Rejected:
  the physical surface is one; two authority rows recreate the race.
- **Hold the lease for the full warm-helper lifetime.** Rejected: a single
  session starves every other session for up to 600s.
- **Block indefinitely in the acquisition path.** Rejected: hidden waits make
  cancellation and operator diagnosis impossible.
- **Auto-retry stale refs by taking a new snapshot.** Rejected: an implicit new
  snapshot can change which element receives the action, and the agent cannot
  reason about the retry.
- **Trust the model to avoid security dialogs.** Rejected: the target is
  resolved and refused by the helper, not by prompt text.
- **Log the AX tree or screenshots for debugging.** Rejected: that is screen
  content and may contain credentials or private data.

## Test plan

- Rust coordinator: one lease across browser/computer, bounded FIFO, same-session
  reentry, grace release/reacquire, zero wait, legacy contention, generations,
  timeline bounds/redaction, policy validation.
- Rust helper manager: warm helper survives lease release, crash respawn, typed
  stale ref with no input retry, sensitive params/ack reach the helper, status
  cache avoids spawns and `refresh` bypasses it.
- Store: policy defaults/round-trip, manifest persistence, legacy lease
  contention/retirement, bounded/session-scoped timeline.
- Protocol: additive defaults and round-trip for policy/status/timeline.
- Swift app: policy/status/timeline decoding, legacy payload defaults.
- Computer helper core: generation-bearing refs, stale-ref details, sensitive
  target matching/refusal/parse bounds, generation counter, error envelope
  details.

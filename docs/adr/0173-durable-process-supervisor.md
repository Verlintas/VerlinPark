# ADR-0173: Durable process supervisor

- **Status:** Accepted
- **Date:** 2026-11-11
- **Owners:** Runtime, daemon, agent tooling, CLI, SwiftUI
- **Related:** ADR-0134/0135 (tool output and shell sandboxing), ADR-0161
  (runtime lease fencing), ADR-0169 (GUI sessions), `docs/SCALING_PROCESS_SOURCE_REMOTE_BUDGET_PLAN.md` F2

## Context

The existing `process` tool kept children in a process-global map owned by one
provider run. That made three common workflows unreliable:

1. **Daemon restart lost the handle.** A long build or dev server kept running
   as an orphan, but the agent could not inspect it, and the daemon had no safe
   way to reattach. The only safe rule was "never survive a restart".
2. **Output was memory-only.** A chatty process was capped by an in-memory ring
   buffer. Output disappeared with the daemon, and a second client could not
   read the same stream.
3. **Signals were unsafe by construction.** Without PID identity evidence,
   reattaching after a restart could signal a different process that happened
   to reuse the pid.

F2 requires a production supervisor that survives a daemon restart, never
adopts a PID on partial evidence, bounds retained output, stays event-backed,
and exposes the same semantics to agents, RPC, CLI, and SwiftUI. It is not a
host-reboot supervisor and does not restart commands.

## Decision

### 1. Durable row model

`supervised_processes` is a per-session operational table with one row per
process. Durable fields:

| Field | Meaning |
| --- | --- |
| `id` | `psup_<uuid-prefix>` durable handle |
| `session_id` | owning session |
| `name`, `command`, `argv` | display label and immutable spawn description |
| `cwd`, `worktree` | absolute working directory and worktree root |
| `env_hash` | SHA-256 over the effective allowlisted environment |
| `command_hash` | SHA-256 over command, cwd, env hash, and sandbox policy |
| `state` | `starting`, `running`, `exited`, `stopped`, `lost`, `unknown` |
| `pid`, `pgid` | process and process-group ids |
| `nonce` | random per-spawn identity value |
| `started_at`, `process_started_at` | daemon clock and `ps` start time |
| `heartbeat_at`, `finished_at` | observation timestamps |
| `exit_code`, `signal` | observed terminal values when available |
| `output_path`, `output_bytes`, `output_lines` | daemon-owned retained log |
| `total_output_bytes`, `output_rotations` | cumulative/rotation telemetry |
| `max_output_bytes`, `max_output_lines` | per-row retention caps |
| `owner_instance` | `instance_id:pid` of the supervising daemon incarnation |
| `retention_deadline` | terminal-row prune deadline |
| `idle_ttl_secs` | per-process idle TTL |
| `last_access_at`, `detail` | operator-facing observation |

Lifecycle transitions are event-backed with full-row payloads:
`process.starting`, `process.started`, `process.adopted`, `process.exited`,
`process.lost`, `process.unknown`, `process.stopped`, and `process.pruned`.
The `process.pruned` payload carries only the id, reason, and terminal state.

Heartbeat, last-access, and output byte/line counters are **operational
observation state**, like `plan_task_progress`. They are updated without an
event so a chatty build cannot flood the event stream. Replay and repair
reconstruct lifecycle rows and compare durable fields; they do not claim
byte-exact telemetry or restore output bodies. Output bodies never enter events,
bundles, or the tool result's machine-readable event payload.

The intent row and its `process.starting` event are written before spawn, so a
crash between spawn and the running event leaves a discoverable row that
reconciliation marks `lost`; it cannot leave an untracked orphan with no
handle.

### 2. Spawn and sandbox

Spawn reuses `sandboxed_command`, so the existing macOS Seatbelt write sandbox,
Git metadata exceptions, scratch directory, HOME/read jails, network policy,
secret read-denials, and `SHELL_ENV_PASSTHROUGH` allowlist are unchanged. The
supervisor adds:

- its own process group (`pgid = pid`), so stop signals the whole tree;
- detached stdio: stdin is `/dev/null`; stdout and stderr append to one
  daemon-owned `output.log` with mode `0600` under
  `<data-dir>/processes/<session>/<id>/`;
- `kill_on_drop(false)`, so a daemon shutdown detaches rather than kills the
  child;
- a reserved environment (`VERLINPARK_SUPERVISED_PROCESS`,
  `VERLINPARK_PROCESS_ID`, `VERLINPARK_PROCESS_NONCE`,
  `VERLINPARK_PROCESS_COMMAND_HASH`) and a trailing shell prologue.

The shell runs `sh -c "<command>\nexit $?"` with reserved positional
parameters carrying the nonce and command hash. The trailing `exit $?` prevents
the shell from exec-optimizing the final command, which keeps the supervised
pid and its identity parameters alive for the process lifetime. This matters on
macOS: the kernel hides the environment of platform binaries (including
`/bin/sh`) from `KERN_PROCARGS2`, so the environment copy alone is not reliably
observable. The same values are written to the environment for non-platform
children, and adoption uses the environment value when present and the argv
value otherwise.

Explicit `env` entries are capped at 32 keys and 4 KiB per value, must be valid
POSIX names, and may not use the `VERLINPARK_` prefix. `cwd` is resolved with
the existing worktree path jail. The active cap is the project
`ProcessPolicy.max_processes` (default 8). A malformed spawn is rejected before a
row or file is created; a spawn error transitions the intent row to `lost` and
removes its empty directory.

### 3. Reattachment evidence

After a daemon restart, each unfinished row is reconciled before the daemon
serves RPCs. Adoption requires **all** of:

1. the recorded pid is alive (`kill(pid, 0)` succeeds, or `EPERM` proves a
   same-host process exists);
2. `ps -p pid -o lstart=` exactly equals the stored normalized
   `process_started_at`;
3. `KERN_PROCARGS2` (or the argv fallback) still reports the exact per-spawn
   `VERLINPARK_PROCESS_NONCE`;
4. the same observation reports the exact `VERLINPARK_COMMAND_HASH`;
5. the process still has a non-empty argv.

Outcomes are fail-closed:

- all evidence matches: `running`, `owner_instance` updated, a
  `process.adopted` event is written, and a reattach monitor polls identity and
  heartbeat;
- pid absent: `lost` with a reason; the exit status is not observable and is
  not invented;
- pid alive but start time differs: `lost` with `pid was reused`; the new
  process is never signaled;
- evidence partially available (hidden environment, missing start time, wrong
  nonce/hash, unreadable argv): `unknown`; a later tick can adopt on full
  evidence, or mark `lost` when the pid disappears.

A row with no pid is `lost`. Imported archive sessions are never reconciled;
their process rows remain historical metadata and their log paths are ignored
because `output_path` must canonicalize under the current daemon's
`processes/` root.

For processes spawned by the current daemon, the monitor owns a live child
handle and can collect the real exit status and signal. For adopted processes
the daemon is not the parent, so a later disappearance becomes `lost` rather
than a fabricated `exited`; an explicit stop still records `stopped` plus the
signal that was sent. This distinction is deliberate and visible in the row.

### 4. Output retention

One append-only file per process receives interleaved stdout/stderr. The monitor
increments observed bytes/lines, and when either cap is exceeded it rewrites the
file with the newest half-cap, preferring a line boundary and keeping the last
`max_output_lines / 2` lines. Rewriting is safe with the child's `O_APPEND`
descriptor: subsequent writes continue at the new end. The implementation is
best-effort and may lose a few bytes that race the rewrite; it never blocks the
child and never lets the file grow without bound.

Defaults are 1 MiB / 20,000 lines; accepted ranges are 4 KiB–4 MiB and
100–50,000 lines. `session.process.logs` and the `logs` tool action page by byte
offset or from the end, with a hard 1 MiB byte and 20,000 line page cap. A
missing or unreadable file returns empty content rather than fabricating output.
Log bodies are daemon-owned operational state, are never part of a session
event or bundle, and are removed by retention pruning.

### 5. Stop, policy, and sweep

`stop` sends the requested signal (default SIGTERM) to the process group, polls
the durable row for a bounded grace period (default 5 s, max 30 s), then sends
SIGKILL and polls again. The monitor records `stopped` with the last signal;
`forced` is true when SIGKILL was needed. No command is restarted. Explicit
signals are allowed only for a child this daemon owns or a row whose full
identity still matches; an unknown/partial row is never signaled.

Idle sweep is policy-driven at two levels. Each row carries the effective
`idle_ttl_secs`, and each project has a persisted `ProcessPolicy`:

| Field | Default | Meaning |
| --- | --- | --- |
| `idle_sweep_enabled` | true | allow automatic idle stops |
| `max_processes` | 8 | active rows per session |
| `idle_ttl_secs` | 3600 | upper bound on a row's requested TTL |
| `max_output_bytes` | 1 MiB | upper bound on retained bytes |
| `max_output_lines` | 20,000 | upper bound on retained lines |

A start request may ask for a shorter TTL or smaller log; `ProcessPolicy::clamp`
never widens the project limits. The daemon sweeper can additionally be disabled
with `VERLINPARK_PROCESS_IDLE_SWEEP_SECS=0`. The sweep re-verifies identity
before signaling. `running` rows with full evidence are stopped; `starting`/
`unknown` rows are marked `lost` without sending a signal when their evidence is
incomplete. A project that disables idle sweep still gets unknown-row
resolution, but no automatic signal. A terminal row is pruned after its
`retention_deadline` (default 24 h, configurable via
`VERLINPARK_PROCESS_RETENTION_SECS`) with a `process.pruned` event and directory
removal. Session discard and worktree cleanup call `stop_session` before
removing the worktree.

`ProcessPolicy` is persisted in SQLite, mirrored in `projects.jsonl`, restored by
`db-rebuild`, and exposed through `process.policy.get/set` and
`vp process-policy[-set]`. It does not alter approval, sandbox, or Verifier
gates.

### 6. Event, replay, and rebuild model

`processes_from_events` folds lifecycle events into rows; `process.pruned`
removes one. A malformed lifecycle payload returns `None`, so repair fails
closed. `processes_match` compares identity, lifecycle, ownership, retention,
caps, and terminal fields but deliberately excludes heartbeat, last-access, and
output counters. `session.replay` reports `processes_match`;
`session.replay {repair:true}` and `session.rebuild` replace the projection;
`session.export` carries rows without output bodies; bundle import restores
rows and events; `vp db-rebuild` reconstructs them through
`restore_session_log` → `rebuild_session`. Imported rows cannot be adopted or
used to read a local file.

### 7. Protocol, CLI, and UI

New RPCs are additive and every new field is `serde(default)`:

- `session.processes` → `ProcessListResult`;
- `session.process.logs` → `ProcessLogsResult`;
- `session.process.stop` → `ProcessStopResult`;
- `process.policy.get/set` → `ProcessPolicyResult`.

`daemon.metrics` / `/metrics` gain `supervised_processes` counts by bounded
state and `supervised_process_output_bytes`; no session/process ids, command
text, or paths are exported. CLI adds `vp processes`, `vp process-logs` (with
bounded `--follow`), `vp process-stop`, and `vp process-policy[-set]`. Swift adds
a Preview "Processes" card with state, pid, elapsed time, exit code, bounded log
tail, and an explicit Stop button, plus a Stage process pill/popover and
`SupervisedProcess`/`ProcessListResult`/`ProcessLogsResult`/`ProcessStopResult`
decoders.

The agent `process` tool keeps `start`, `status`, `read`, `write`, `signal`,
`wait`, and `stop`, adds `logs` and `list`, makes `start` return immediately,
bounds `wait`, and pages logs. `read` remains a compatibility alias for the
newest retained page. Durable processes have detached stdin, so `write` returns
a typed `PROCESS_IO_FAILED` instead of pretending to write. `process` start
keeps the shell-command risk classification; stop/signal/write are classified as
workspace writes, which is stricter than the old read-only label. Sandbox,
path-jail, approval, write-scope, Verifier, merge, capacity, and budget gates are
otherwise unchanged.

The new tool error codes are `PROCESS_LOST`, `PROCESS_UNKNOWN`, and
`PROCESS_STOP_TIMEOUT`; existing process codes and `WAIT_TIMEOUT` remain.

## Consequences

- Long builds and dev servers can be inspected after a provider turn or daemon
  restart without re-running them.
- PID reuse is rejected by construction, and an unverifiable pid is never
  signaled.
- Output cannot grow without bound or pollute the event stream, but it is
  operational state: deleting the data directory or pruning the row loses it.
- The daemon does not become a host-reboot supervisor. A reboot kills children;
  the next startup reconciliation marks those rows `lost`.
- Exit codes are only observable for processes this daemon spawned. Adopted
  processes that exit later are `lost`, not `exited`.
- Rotation is best-effort under concurrent writes; a few bytes can be lost
  during a rewrite.
- Reattachment is same-host and single-daemon. It does not coordinate two live
  daemons, a remote host, or a distributed lease. The data-dir instance lock
  still enforces one daemon per data directory.
- The supervisor deliberately does not auto-restart, retry, or resurrect a
  command. Restart is an explicit new `process start` from an agent or operator.

## Non-goals

- Host-reboot persistence, launchd integration, or automatic restart.
- Sandboxing or network-isolating the command beyond the existing Seatbelt
  write/env/HOME/network policy.
- Exactly-once exit observation, provider-call resume, or distributed process
  ownership.
- Streaming output into the event log, terminal emulation, or interactive stdin.
- Reattaching a process whose command line intentionally `exec`s a different
  program after spawn (its argv no longer carries the identity marker; the row
  becomes `unknown` and is never adopted).
- Cross-host/remote process supervision; the existing `remote_*` tools remain
  explicit SSH operations.

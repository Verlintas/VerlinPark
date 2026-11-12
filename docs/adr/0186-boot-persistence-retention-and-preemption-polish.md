# ADR-0186: Boot persistence, retention, and preemption polish

- **Status:** Accepted
- **Date:** 2026-11-12
- **Deciders:** VerlinPark runtime maintainers
- **Related:** ADR-0139 (session cancel/pause), ADR-0158 (browser/computer GUI
  lease), ADR-0170 (tool error contract), ADR-0173 (durable process supervisor),
  ADR-0178 (test-result cache and impact map), ADR-0179 (precise preemption and
  deadlines), `docs/GAP_CLOSURE_PLAN.md` I5

## Context

Four lifecycle gaps remained after the long-run operations round:

1. The daemon had no supported login-item story. Operators either started
   `verlinpadd` by hand or wrote their own launchd job. That is error-prone and
   easy to confuse with the daemon's own instance lock and discovery file.
2. Durable supervised processes were adopted after a daemon restart only when
   their full identity still matched. A process that died while the daemon was
   down became `lost`, even when the operator wanted the dev server or watcher
   back. Restarting automatically would be a hidden behavior change.
3. `gui.action` events accumulated for the lifetime of a session. They are
   useful audit telemetry, but unlike messages and plan state they are not a
   projection input. Keeping them forever makes the event table grow with GUI
   polling rather than with user work.
4. Per-tool deadlines were enforced for sequential tools, but a parallel
   read-only batch shared one token with no per-tool deadline. One slow
   `read_file`/`grep` could therefore run for the whole batch window even when
   the project policy declared a per-tool bound.
5. The test-cache key covered the worktree, dependency closure, and a fixed
   build-environment subset. Operators could not declare a Git-ignored local
   config file or an environment variable that changes test behavior.

This ADR records the explicit, bounded fixes. None of them weaken approval,
Seatbelt, path-jail, write-scope, Verifier, merge, capacity, or budget gates.

## Decision

### 1. User LaunchAgent: explicit, user-level, never automatic

`vp daemon-agent install|uninstall|status` owns the login item.

- **Location:** `~/Library/LaunchAgents/com.verlinpark.daemon.plist`. The
  installer creates `~/Library/LaunchAgents` and the data-dir `logs/` directory
  with private modes where appropriate, writes the plist atomically with mode
  `0644`, and never writes outside the user's home or data directory.
- **Plist content:** deterministic XML with `Label`, `ProgramArguments`
  (`verlinpadd --data-dir <absolute data dir>`), `RunAtLoad`, `KeepAlive`,
  `ProcessType Interactive`, `ThrottleInterval 10`, `Umask 077`, and
  `StandardOutPath`/`StandardErrorPath` under `<data dir>/logs/`. The binary is
  resolved explicitly (`--binary`, `VERLINPARK_DAEMON_BIN`, a `verlinpadd`
  sibling of `vp`, or `PATH`); the installer never points at `vp` and never
  guesses across unrelated directories.
- **Confirmation:** `install` and `uninstall` require explicit confirmation.
  The CLI prompts on a terminal; `--yes` is the non-interactive form. EOF or a
  blank answer is a refusal.
- **Refusals:** `install` refuses when the label is already loaded, or when a
  live discovery pid exists outside the label. `--force` permits replacing an
  existing not-loaded plist only; it never displaces a running job.
- **No root:** every launchctl call is `/bin/launchctl` with argv, through
  `launchctl bootstrap gui/<uid> <plist>`, `kickstart`, or `bootout`. There is
  no `sudo`, no shell string, and no privileged helper.
- **Status:** read-only. It queries `launchctl print` and
  `launchctl print-disabled`, parses loaded/running/pid/disabled, and reports
  `not_installed`, `installed`, `loaded`, `disabled`, `running`, or `unknown`.
  A failed launchctl query is `unknown`, never a false negative. It never
  spawns the daemon and never writes the plist. The status is also exposed as
  the read-only `daemon.agent.status` RPC for Manage Dash.
- **Uninstall:** bootout first; a bootout failure other than "not found"
  aborts before the plist is removed. A dry run prints the exact audited argv
  and writes nothing.
- **Testability:** all launchctl calls go through the `LaunchctlRunner` trait.
  Tests use a fake runner and temporary home/data roots; no test requires a
  real launchd session.

Limits: launchd availability and TCC/login-session behavior are host
properties. The installer validates paths and the executable bit, not code
signing. It is not a cross-user or system-wide service, and it does not install
a helper for root.

### 2. Supervised process boot restart: opt-in, bounded, identity-first

`ProcessPolicy` gains three additive fields:

| Field | Default | Meaning |
| --- | --- | --- |
| `restart_on_boot` | `false` | opt in to respawning a `running` row whose original child is gone |
| `restart_max_attempts` | `3` | consecutive boot-restart attempts (0–10) |
| `restart_backoff_secs` | `5` | base exponential backoff (0–300); attempt N waits `base * 2^(N-1)`, capped at 300s |

Startup reconciliation keeps the existing identity rules:

- Full identity match → adopt, never duplicate. Restart policy does not apply
  while the original child is still alive.
- Absent pid or a reused pid → eligible for an opted-in restart.
- Partial evidence (matching pid/start time but missing/mismatched nonce or
  command hash) → `blocked` with `identity_unverified`; ambiguous evidence is
  never a reason to spawn another copy.
- `starting` rows and `unknown` rows are not restarted automatically.
- Imported archive sessions are never restarted.

When restart is allowed, the row enters `restarting`, increments
`restart_attempts`, and persists a `process.restarting` event before the
backoff. The respawn loop is asynchronous and outside daemon startup's critical
path. Each attempt:

1. Re-reads the row and aborts if an operator stopped/pruned it or another
   reconciler changed the attempt.
2. Reconstructs the original explicit environment from a **0600 sidecar**
   (`processes/<session>/<id>/env.json`). Values never enter SQLite, events,
   CLI output, status, or logs. A missing, non-private, oversized, or invalid
   sidecar fails closed with `restart_env_unavailable`.
3. Revalidates the canonical worktree and cwd under the current sandbox,
   network, and read-jail policy. Missing paths block with
   `restart_worktree_unavailable`/`restart_cwd_unavailable`.
4. Spawns under the same Seatbelt/env contract with a **new nonce, command
   hash, pid, and process start time**, then updates the same row and emits
   `process.restarted`.
5. On failure, retries until `restart_max_attempts`, then becomes `blocked`
   with a stable reason (`restart_attempts_exhausted`, `restart_spawn_failed`,
   `restart_policy_disabled`, etc.). `blocked` is terminal for automatic
   restart and can be pruned by normal process retention.

`restart_attempts` persists across daemon restarts so a crash loop cannot
reset itself by restarting the daemon. An explicit new `process.start` creates
a fresh row with attempts at zero. A policy changed to `restart_on_boot=false`
before the next boot blocks any pending `restarting` row instead of silently
respawning it.

Events: `process.restarting`, `process.restarted`, `process.restart_blocked`.
Replay reconstructs all three. No event contains command output or environment
values.

An operator can explicitly retry a non-live row with
`session.process.restart` / `vp process-restart`. It requires `confirm: true`
(CLI `--yes` or a prompt), refuses `running`/`starting`/`restarting` rows and
ambiguous `unknown` identity, resets the attempt counter, and then uses the
same sidecar, sandbox, cwd, and bounded retry loop. It is allowed even when
`restart_on_boot=false` because this is the operator's explicit request, not a
silent policy restart. A project with `restart_max_attempts=0` refuses manual
retries too.

### 3. GUI timeline retention: only `gui.action`, bounded and audited

`GuiPolicy.timeline_retention_days` defaults to 30; `0` keeps forever and a
non-zero value is capped at 3,650 days. The RPC/CLI may override the value for
one operation, but an explicit `0` is rejected: "prune everything" is not a
retention operation.

The daemon sweep (`VERLINPARK_GUI_RETENTION_SWEEP_SECS`, default 6 hours,
minimum 60s) and the explicit `gui.timeline.prune` RPC call the same bounded
store method. Per call, at most `limit` (default 1,000, hard cap 10,000)
`gui.action` rows are selected strictly older than the cutoff. A truncated
batch reports `truncated: true`; the next sweep continues.

Guarantees:

- The SQL predicate is `type = 'gui.action'`. No other event kind is selected
  or deleted; `other_kinds_touched` remains 0.
- Events at exactly the cutoff boundary are retained (`<`, not `<=`).
- A session whose JSONL mirror cannot be read or atomically rewritten is
  skipped **before deletion**. If a mirror exists it is rewritten in the same
  mirror critical section, so `session.log_status` still matches; a missing
  mirror (imported/legacy) is left missing rather than fabricated.
- One `gui.timeline_pruned` event per affected session records the project,
  retention days, cutoff, and count. It never includes payloads or screen
  content.
- `restore-log` still requires a contiguous mirror. Pruning creates
  intentional sequence gaps, so restoring a pruned session's log fails closed
  with the existing diagnostic; restoration is not a retention bypass.
- A manual non-dry-run prune requires `confirm: true` (CLI `--yes` or a
  prompt); the daemon rejects a request without it. The automatic sweep does
  not go through the RPC.
- A dry run returns the exact eligible count and cutoff without touching the
  database or mirror; `gui.timeline.retention` / `vp gui-timeline-status`
  reports total, eligible, oldest, newest, and the effective cutoff without
  reading payloads.

### 4. Per-tool deadlines in parallel read-only batches

A parallel read-only batch keeps one child of the run token for
operator/wall-clock preemption. Each tool additionally receives
`PreemptToken::tool_child(&batch_token, tool_deadline)`. The tool future is
raced against its own token; when the deadline fires:

- that future is dropped (including any helper work it owns),
- the tool receives a typed `DEADLINE_EXCEEDED` result with
  `reason = tool_deadline`, `auto_retry = false`, and honest elapsed evidence,
- one `tool.preempted` event is emitted for that call,
- sibling tools and the run continue.

The batch still aborts when the run token or cooperative cancel fires. The
deadline is measured from each tool's own start, not batch start. No new
scheduler preempts tools on a timer; the deadline comes from the existing
project `PreemptionPolicy.tool_deadline_secs` (and `enabled` switch).

### 5. Test-cache external scopes: explicit, hashed, fail closed

`TestCachePolicy` gains:

- `extra_env: [String]` — explicit environment variable names (not values),
  max 32, max 128 chars, syntactically valid, `VERLINPARK_*` reserved names
  rejected.
- `extra_paths: [String]` — worktree-relative regular-file paths, max 64, max
  512 chars, no absolute paths, `..`, NUL, or `.git`; max 8 MiB per file and
  64 MiB total.

The key material gains `external_scopes_hash`. It covers the configured lists
themselves plus observed state: a missing environment variable is hashed as
`unset` (setting it later invalidates), path content is hashed by canonical
relative path and SHA-256, and the configured environment names/values are
hashed only. Values are never persisted in SQLite, events, CLI output, or UI.

Failure behavior: an unreadable or missing path, symlink escape, non-file,
non-Unicode env, or any size bound sets `complete=false`; `run_project_task`
then disables reuse for that run and records an `uncacheable` provenance note
instead of returning a possible false pass. Policy normalization trims/sorts/
dedupes names and paths; the project manifest and `db-rebuild` carry the fields
through normal serde defaults, so an old manifest decodes with empty scopes.
`test_cache.scope.preview` / `vp test-cache-scopes` reports each configured
name/path as present/unset, bytes, complete/failed, and a bounded reason
without reading values into the response. The status is read-only and never
executes a test command.

## Consequences

Positive:

- Login persistence is an explicit, auditable, user-level action with no root.
- Durable processes can survive a daemon restart without hidden behavior:
  policy is off by default, identity is still checked, attempts/backoff are
  bounded, and exhausted rows are visibly blocked.
- GUI telemetry grows with retention policy rather than forever, and auditing
  remains possible through count-only events.
- A slow read-only tool no longer consumes the whole parallel batch budget.
- Operators can declare ignored local inputs without weakening the
  content-addressed key or risking stale passes.

Costs and limits:

- Restarting requires a private env sidecar; rows created before this ADR have
  none and block instead of restarting. This is intentional and typed.
- The sidecar is operational state protected by file mode, not Keychain. It is
  never emitted, but a same-user process that can read the daemon data dir can
  read it, exactly as it can read the process output log.
- Restart attempts are consecutive across daemon restarts; an operator must
  start a new row or raise/disable the policy to retry after exhaustion.
- GUI retention physically deletes `gui.action` rows and rewrites mirrors.
  Pruned sessions cannot use `restore-log` as if the log were complete.
- External cache scopes are explicit only. There is no directory glob, no
  automatic discovery of dotfiles, and no cross-host cache sharing.
- LaunchAgent installation does not verify code signing, install a privileged
  helper, or manage a system-level daemon.

## Protocol, CLI, and UI surface

All additions are backward-compatible JSON-RPC fields; every new request field
is optional and every new response field has a serde default.

| Surface | RPC | CLI | UI |
| --- | --- | --- | --- |
| LaunchAgent status | `daemon.agent.status` | `vp daemon-agent status` | Manage Dash → Diagnostics card |
| LaunchAgent install/uninstall | — (local user action only) | `vp daemon-agent install\|uninstall [--yes] [--dry-run]` | Read-only card; install is deliberately CLI-only |
| Process boot restart | `process.policy.get/set` | `vp process-policy[-set] --restart-*` | CLI/policy JSON |
| Explicit process retry | `session.process.restart` | `vp process-restart <session> <process> --yes` | CLI |
| GUI retention status | `gui.timeline.retention` | `vp gui-timeline-status` | GUI policy card |
| GUI prune | `gui.timeline.prune` | `vp gui-timeline-prune ... [--yes]` | GUI policy card confirmation alert |
| GUI audit | `gui.timeline.audit` | `vp gui-timeline-audit` | GUI policy card count |
| External cache scopes | `test_cache.scope.preview` | `vp test-cache-scopes` | Test-cache panel entries |
| External cache policy | `test_cache.policy.set` | `vp test-cache-policy-set --extra-env/--extra-path` | Test-cache panel editors |

`daemon.agent.status` is read-only. There is intentionally no RPC that installs
a LaunchAgent: a remote or automated caller must not create user login items,
and the CLI keeps the confirmation and path-discovery logic in one auditable
place. `session.process.restart` and GUI prune are local operator RPCs; they
still require `confirm: true`.

## Observability and failure behavior

- LaunchAgent: no secret is ever read. Status reports `launchctl` failure
  honestly as `unknown`; install/uninstall failures include bounded launchctl
  stderr and remove a half-written plist only when bootstrap failed.
- Restart: lifecycle events carry a bounded `detail` and the stable
  `restart_blocked_reason`. Environment values, command output, and process
  argv beyond the already-published command are excluded. A sidecar that is
  not 0600 fails closed.
- GUI retention: the audit event carries `project_id`, `retention_days`,
  `cutoff`, `pruned`, and `kind=gui.action` only. `gui.timeline.audit` returns
  the count/cutoff history, not the removed payloads.
- Cache scopes: preview entries expose `kind`, `label`, `present`, `bytes`,
  `complete`, and a bounded note. They never expose an environment value or
  file content. Key computation returns `complete=false` rather than a key that
  might miss a changed input.
- Per-tool deadline: the provider sees one `DEADLINE_EXCEEDED` tool result and
  `tool.preempted` records the reason/phase/elapsed; the batch/session remain
  running. `tool.finished.error_code` and `retryable=false` are already part of
  the tool observability contract.

## Test matrix

- `daemon_agent` unit tests: deterministic plist, XML escaping, `plutil -lint`,
  running/loaded/disabled/unknown parsing, confirmation refusal, dry-run no
  writes, loaded/other-supervisor refusal, 0644 install, bootstrap failure
  cleanup, bootout-before-remove, missing-service uninstall, binary resolution,
  and no-sudo/no-shell argv assertions. No test needs real launchd.
- `process_supervisor` tests: opted-in respawn with a fresh nonce, off-by-default
  lost behavior, attempt exhaustion/blocked reason, ambiguous identity refusal,
  reused-pid restart, env-sidecar failure, retry-then-block, explicit
  confirmation/live-row refusal/attempt reset, status counts, event replay, and
  secret-free payload assertions.
- GUI tests: strict cutoff boundary, only `gui.action`, audit event, mirror
  rewrite/log status, dry run, bounded/truncated batch, keep-forever skip,
  multi-project sweep, unreadable-mirror skip, status counts, and audit limits.
- Parallel deadline tests: one slow + one fast fake tool, typed
  `DEADLINE_EXCEEDED` for the slow call only, fast call success, dropped work,
  run token untriggered, and run-preemption aborting both.
- Test-cache tests: path content change invalidates, env change invalidates,
  missing/unreadable/escape/directory/oversized/total-bound fail closed,
  order/duplicate normalization preserves the hash, values never rendered,
  preview metadata, manifest/DB policy round-trip, and uncacheable execution.

## Rollback

- Removing the LaunchAgent is always `vp daemon-agent uninstall`; it boots the
  job out before deleting the plist.
- Setting `restart_on_boot=false` stops future boots from respawning and blocks
  a pending `restarting` row on the next reconcile. Rows already running are
  unaffected; an operator can stop them normally.
- Setting `timeline_retention_days=0` keeps all future and existing
  `gui.action` events; it does not restore already pruned rows.
- Removing `extra_env`/`extra_paths` invalidates cached keys naturally because
  the external-scope hash changes.
- Disabling the project preemption `enabled` switch or clearing
  `tool_deadline_secs` restores the previous sequential/batch deadline
  behavior; no run or batch state is rewritten.

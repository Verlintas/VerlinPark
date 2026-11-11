# ADR-0156: Remote workspaces over SSH and stdio

- **Status:** Accepted
- **Date:** 2026-11-11
- **Scope:** `vp-remote`, `vp-remote-agent`, project `ExecutionHost`, the six
  explicit `remote_*` agent tools, host/remote RPCs, and remote worktrees
- **Related:** ADR-0003 (deterministic daemon ownership), ADR-0011 (tool
  approval broker), ADR-0022 (worktrees and integration), ADR-0134 (bounded
  tool output), R2 in `docs/LONG_TASK_RUNTIME_PLAN.md`

## Context

VerlinPark's daemon, tools, and worktrees are local. Projects whose source or
build host lives elsewhere had only two poor choices: install a second daemon
on the remote machine, or let the model compose ad-hoc SSH commands with no
structured path policy, no atomic writes, no bounded output, and no audit. Both
weaken the existing invariants:

1. deterministic Rust owns path, permission, and merge decisions;
2. every session-scoped state change is event-backed;
3. no provider credentials are placed on a host that does not need them;
4. no tool gets a write path outside its sandbox;
5. approvals are the only route to host-state changes.

A full remote daemon would duplicate the scheduler, merge queue, Verifier, and
event log, and would require remote credentials and a listening port. The
smaller, auditable design is a dependency-light helper launched over a child
process's stdio, either locally or through `ssh`.

## Decision

### 1. Transport is a child process stdio, not a network service

`crates/vp-remote` defines a newline-delimited JSON-RPC 2.0 protocol. Each line
is one complete frame, bounded at 16 MiB by default. The reader rejects an
oversized frame before allocating it. Requests carry ids; responses are
multiplexed by id; notifications are used for cancellation and shutdown.

Two transports implement the same trait:

- `StdioTransport` spawns a configured helper binary directly and passes only
  an allowlisted environment. Provider keys, daemon discovery tokens, and
  session state are not inherited.
- `SshTransport` spawns the system `ssh` client with an explicit argument
  vector. It never invokes a local shell. OpenSSH necessarily interprets the
  remote command with the remote user's shell, so every remote-side argument is
  single-quoted for POSIX `sh` before the command string is passed to `ssh`.
  Hosts and users are validated to reject whitespace, control characters,
  leading `-`, and shell metacharacters. Extra SSH arguments are bounded and
  control-character checked. Identity files, ports, and `BatchMode=yes` and
  `ConnectTimeout` are explicit.

The helper opens no port, requires no remote daemon, and uses only POSIX and
Git. There is no fallback to local execution when a remote connection fails.
An error is returned to the caller and the tool reports it.

### 2. Handshake is mandatory and refuses before any operation

The first request must be `hello` with:

- the remote protocol version (currently 1),
- the expected canonical root,
- an auth token,
- a client version string.

The helper validates all four. Auth comparison is constant-time. A version,
root, or token mismatch returns a structured error and the helper exits; no
file, shell, or Git operation is accepted before a successful handshake. A
second `hello` on the same connection is refused. The response advertises the
helper version, canonical root, frame limit, exec environment allowlist, and a
capability set. The client requires every version-1 capability before it marks
the workspace healthy, so a partial helper fails at connect rather than at the
first tool call.

Auth tokens never enter SQLite or the project manifest. `ExecutionHost`
stores `token_env`, the name of a daemon environment variable, and optionally
`token_file`, the remote file the helper reads. The daemon must have the token
in its environment to authenticate; the remote helper may read the same token
from a `0600` file. Because OpenSSH does not forward arbitrary environment
variables, a helper launched with only `--token-env` falls back to
`~/.verlinpark/remote-token` (mode `0600`) when the named variable is absent;
an explicit `--token-file` is authoritative. Token values are never logged.

### 3. Every path is resolved through a canonical root jail

`PathJail` canonicalizes the root at helper startup. Every caller path must be
relative; absolute paths, `..`, NUL/newline characters, and control characters
are rejected. Existing paths are canonicalized and must remain under the root.
For writes, every existing component is inspected: a symlink is rejected even
if it resolves inside the root, because an atomic rename over a link is
surprising and can be used to pin a future path. Missing descendants are
created only under an already-canonicalized parent. `.git` and `.gitmodules`
are never exposed through file operations; structured Git operations are the
only route to repository metadata.

A final symlink is preserved for remove/rename so deleting a link deletes the
link, not its target, while the target must still resolve inside the root.
Worktree paths use the same jail. The root itself can never be removed,
renamed, or used as a worktree.

This is path containment, not an OS sandbox. It is TOCTOU-resistant only to
the extent the remote host is trusted; a malicious process running as the same
remote user can race the jail. The trust model is explicit in section 9.

### 4. Operations are bounded, typed, and idempotent

The helper implements:

- `hello`, `probe`, `health`, `stat`, `list_dir`;
- `read_file` with byte offset/limit and UTF-8 or base64 selection;
- `write_file` with expected SHA-256, atomic temp-file + fsync + rename, parent
  creation, mode, and an explicit overwrite escape hatch;
- `mkdir`, `remove`, `rename`;
- `exec` with cwd jail, an environment allowlist, timeout, per-stream output
  caps, process-group kill, and cancellation;
- `git_status`, `git_diff`, `git_log`, `git_rev_parse`,
  `git_worktree_add/list/remove`;
- `cancel` and `shutdown`.

Mutating operations require an `operation_id`. The helper hashes the method and
exact request params, records the result in an append-only JSONL ledger, and
returns the recorded result for an exact retry. The same id with a different
payload is `OPERATION_ID_CONFLICT`; a concurrent duplicate is
`OPERATION_IN_PROGRESS`. Failed operations are recorded so a retry with the
same id does not repeat a possibly-partial side effect. The ledger retains a
bounded number of entries (default 512, hard maximum 8192) and compacts the
file to the same window. A stdio helper can use `--no-ledger`; the daemon
normally passes a per-project ledger path under its data directory, and an SSH
helper uses its configured remote path or `~/.verlinpark`.

`remote.ledger` exposes a bounded metadata tail (operation id, method, success,
completion time) without file content or environment values, so operators can
audit retries and side effects without reading the helper's JSONL file.

`exec` clears the environment, inherits only safe build/runtime names, applies
request overrides from the helper's allowlist, and rejects loader-injection
variables (`LD_PRELOAD`, `DYLD_*`, `BASH_ENV`, and similar) even if a project
tries to allow them. The command runs in its own process group; timeout,
disconnect, and explicit cancellation kill the group. Output is drained but
only the capped prefix is retained. `git` runs with external diff drivers and
hooks disabled, fixed argv, validated refs/path filters, and a timeout.

### 5. Helper errors are actionable and stable

Errors use a stable `RemoteErrorCode` vocabulary (`PATH_ESCAPE`, `SHA_MISMATCH`,
`PRECONDITION_REQUIRED`, `ENV_NOT_ALLOWED`, `TIMEOUT`, `OPERATION_ID_CONFLICT`,
and so on) in `error.data`, while the JSON-RPC numeric code remains compatible
with generic clients. Errors name the rejected path, limit, missing capability,
or conflicting operation id. The client never converts a transport failure into
a successful empty result.

### 6. Project configuration is explicit, local remains the default

`ExecutionHost` is project-scoped, persisted in SQLite
(`project_execution_hosts`) and the project manifest, and exposed through the
protocol with `#[serde(default)]`. Its default mode is `local`; new fields are
backward compatible. `ssh` requires a host, absolute root, helper path, and
`token_env`, with validated bounds for port, timeouts, concurrency, SSH
arguments, identity file, env names, and token/ledger paths. It cannot set a
remote root in local mode.

`RemoteManager` keys one connection per project/config fingerprint. It
reconnects after the configured idle interval or a closed transport, caches the
hello capabilities and helper version, exposes health/status, bounds concurrent
calls with a per-workspace semaphore (1–32), and evicts idle workspaces when the
registry reaches its cap. Idle reconnect never closes a connection with an
in-flight call. A configuration update invalidates the affected workspace.

### 7. Remote tools are explicit, not global routing

Six tools are added to the registry:

| Tool | Effect | Risk |
|---|---|---|
| `remote_read` | read a remote file | read-only |
| `remote_list` | list a remote directory | read-only |
| `remote_write` | atomic remote file write | external |
| `remote_exec` | remote shell command | host-state |
| `remote_git` | remote status/diff/log/rev-parse | read-only |
| `remote_worktree` | remote list/add/remove worktree | list read-only, mutation host-state |

Existing local tools are never rerouted. A local project gets no
`RemoteToolAccess`, so every remote tool fails explicitly and tells the model
to use local tools. A remote project still has local tools for its session
worktree; the remote host is a separate, explicit target. Each tool accepts an
optional `project_id` that must equal the session project; cross-project access
is refused. Paths are always relative to the configured root; there is no
`root` input.

The tools are role-scoped. Leader gets all six. Manager gets read/list/remote_git
(which is read-only). Coding Worker/Labour get all six; research gets
read/write/list/git; verification gets read/list/git/exec; generic gets
read/list/git. Scout gets only read/list/git. The Verifier never gets
`remote_write`.

### 8. Approvals and policy remain the gate

`assess_tool_call` classifies `remote_write` as external, `remote_exec` as
host-state, read/list/git-read as read-only, and remote worktree/Git mutations
as host-state. The existing approval broker therefore asks in `smart` mode for
exec and worktree mutations, asks for remote writes in `strict`, and denies
host-state operations in `unattended`. Project approval grants are path
prefixes without a host identity, so they never auto-approve `remote_*` tools;
the risk broker is always used.

`remote_exec` additionally fails closed when the project `ShellPolicy` denies
network or enables home/volume/read-deny restrictions that the helper cannot
enforce. Remote file operations are still constrained by the helper jail and
the remote user's authority. A remote shell bypass is not available: exec is
host-state, approval-gated, bounded, and uses the same project policy gate.

### 9. Trust model and limitations

- The remote host and the remote user's account are trusted infrastructure.
  The helper is not a security boundary against that user or against a local
  root attacker. The path jail protects the agent from accidental traversal and
  from symlinks that leave the configured root; it is not an OS sandbox.
- The helper binary must already be installed on the remote host (copy
  `vp-remote-agent`, ensure it is executable). VerlinPark does not upload or
  auto-install binaries.
- SSH authentication, host-key verification, agent forwarding, and network
  policy are the user's SSH configuration. VerlinPark adds `BatchMode=yes` and a
  connect timeout but does not replace `known_hosts` policy.
- The helper needs POSIX `sh`, Git, and the remote user's normal toolchain for
  `exec`. It does not need provider credentials; the daemon never sends them.
- There is no remote daemon, no persistent remote scheduler, no cross-host
  distributed scheduler, no remote merge queue, and no automatic reconnection
  of an in-flight exec. A timeout/cancel returns to the caller; retrying the
  same `operation_id` replays the ledger result instead of repeating a
  completed side effect.
- Remote write/exec idempotency depends on the helper ledger surviving a
  restart. With `--no-ledger`, it is in-memory only. A crash in the narrow
  window after a side effect but before its ledger append can lose the replay
  record; the operator must inspect the remote state before retrying that
  operation id.
- Network access inside a remote `exec` is governed by the remote host and SSH
  policy, not by Seatbelt. `ShellPolicy network=deny` therefore refuses remote
  exec rather than pretending to sandbox it.
- Remote Git worktrees are rooted under the configured remote root. They are
  not automatically integrated into the local session merge queue in this ADR;
  integration remains a separate explicit operation.
- `remote.status` does not connect. `host.probe` does, so operators can test a
  host without changing workspace state.

## Consequences

- Remote projects become first-class structured targets without moving the
  daemon, provider credentials, or event log.
- The daemon gains a new crates.io-independent helper protocol; both sides must
  be upgraded together for new operations, but the handshake fails closed on a
  version mismatch.
- The helper ledger is operational audit state, not a session event projection.
  Session events still record tool calls, approvals, and results; the ledger
  exists to prevent duplicate remote side effects across retries and helper
  restarts.
- New remote tables are project-scoped and included in the manifest, so
  `db-rebuild` restores host configuration without restoring secrets.
- The approval surface grows by six explicit tools, each classified by effect.
  No existing local tool changes behavior.

## Alternatives considered

1. **Full remote daemon.** Rejected: duplicate scheduler/merge/event logic,
   remote credentials, a listening port, and a much larger trust surface.
2. **Local shell SSH strings.** Rejected: no path jail, no atomic write, no
   idempotency, output unbounded, and the model composes injection-prone
   commands.
3. **Global routing of `read_file`/`shell` to a remote host.** Rejected: it
   hides which machine is being changed, breaks local session worktrees, and
   weakens the sandbox/approval invariants. Explicit tools keep the target
   visible in the transcript and audit stream.
4. **A remote HTTP service with bearer auth.** Rejected for this milestone:
   opens a port and adds TLS/proxy/deployment surface; stdio over SSH inherits
   the user's existing SSH trust and needs no listening service.
5. **Storing the remote token in SQLite/manifest.** Rejected: secrets belong in
   the environment or a private file, not the database, backups, or project
   manifest.

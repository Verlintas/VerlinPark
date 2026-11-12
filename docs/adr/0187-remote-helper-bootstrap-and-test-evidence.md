# ADR-0187: Remote helper bootstrap and remote test evidence

- **Status:** Accepted
- **Date:** 2026-11-12
- **Depends on:** ADR-0156 (remote workspace trust boundary), ADR-0175
  (remote worktree integration), ADR-0170 (tool error contract)
- **Supersedes:** the manual-only helper installation note in ADR-0156

## Context

A project can configure an SSH execution host, but the `vp-remote-agent`
helper had to be copied to that host by hand. That made remote workspaces
operationally fragile: the daemon could not tell whether the installed helper
matched its protocol version, an operator could accidentally overwrite a
different binary, and there was no bounded, auditable install path. Separately,
remote project checks could be run through `remote_exec` but were not captured
as evidence, so a reviewer could not tell which host, commit, command, or
output produced a "remote test passed" claim.

The local Verifier and deterministic Integrator are the merge authority. A
remote test can never become a second authority: the remote host is trusted
infrastructure but is not reproduced locally, and the local sandbox, Verifier,
and merge gates must remain intact.

## Decision

### 1. Bootstrap is an explicit host-state action

`remote.bootstrap` / `vp remote-bootstrap` uploads the local helper to the
configured SSH host. It is:

- **explicit**: it never runs from the scheduler, a task, a retry loop, or a
  model tool call without operator action;
- **approval-classified**: the result carries `risk = host_state` and
  `confirmation_required = true`, and the RPC/CLI refuses unless `confirm` is
  true;
- **project-scoped**: it uses the stored `ExecutionHost` and fails when the
  project is local;
- **policy-gated**: a project `ShellPolicy` with `network = deny` refuses
  bootstrap before any SSH process is spawned; read restrictions that cannot
  be enforced over SSH also fail closed;
- **non-configuring**: it records the install but never rewrites
  `ExecutionHost.helper_path`. A mismatch is reported in the result and UI so
  the operator can update host configuration explicitly.

This is not an agent tool. The operator-facing surface is the daemon RPC, CLI,
and read-only Manage Dash status. If a future agent tool is added, it must use
the same `RiskLevel::HostState` classification and confirmation gate.

### 2. Upload protocol and checks

Bootstrap cannot use the JSON-RPC helper protocol because the helper may not
exist yet. It uses a tiny line-oriented shell protocol over the existing SSH
connection:

1. **Local inspection**: verify the helper is a regular file, bound its size,
   compute SHA-256, and parse the executable header. Supported targets are
   Mach-O (`macos` arm64/x86_64) and ELF (`linux` arm64/x86_64). Unknown
   formats, universal/fat binaries, and unsupported architectures fail with
   `REMOTE_BOOTSTRAP_UNSUPPORTED` or `REMOTE_BOOTSTRAP_ARCH_MISMATCH` before
   upload.
2. **Preflight**: run a POSIX shell script that expands the install path,
   requires it to be under the remote user's `$HOME`, creates the parent with
   a private umask, checks writability, verifies a SHA-256 tool exists, reports
   OS/architecture, existing file state, and free bytes. No sudo, `su`, or
   privilege escalation is ever invoked. The local side rejects a path outside
   HOME, a non-writable directory, an unsupported target, a missing SHA-256
   tool, a target that already exists unless `force` is set, and insufficient
   disk space (`REMOTE_BOOTSTRAP_DISK`).
3. **Chunked upload**: stream the binary in bounded chunks to
   `<install>.part-<token>` in the same directory. Writes use `umask 077`; the
   file is never installed in place. The daemon enforces a 128 MiB hard cap,
   bounded chunk sizes, and a finite SSH wait. A failed upload triggers
   best-effort cleanup of the temporary file.
4. **Finalize**: set mode 700, recompute SHA-256, execute the **temporary**
   binary as `remote-agent --describe`, and verify the reported
   `protocol_version`, `helper_version`, `target_os`, and `target_arch` before
   replacing anything. Only then is an atomic `mv -f` performed into the final
   path, followed by `chmod 700`. If the file exists and `force` is false, the
   temporary file is removed and `REMOTE_BOOTSTRAP_EXISTS` is returned. If the
   temporary binary cannot execute, the operation fails with
   `REMOTE_BOOTSTRAP_ARCH_MISMATCH` and the existing helper is untouched.
5. **Record**: persist host, install path, SHA-256, byte count, helper version,
   protocol version, target OS/architecture, and timestamps in
   `remote_bootstrap_records` and the project manifest. No token, key, or
   credential is stored. If a session was supplied, append a
   `remote.bootstrap.completed` event; otherwise the record is project
   configuration evidence.

The `--describe` mode is a helper-only metadata command. It prints JSON
containing the helper version, protocol version, compiled target OS/arch, and
capabilities. It never reads tokens and does not start the server.

### 3. Typed errors

Bootstrap maps remote failures to stable Q3 codes:

`REMOTE_BOOTSTRAP_APPROVAL_REQUIRED`, `REMOTE_BOOTSTRAP_PATH_REJECTED`,
`REMOTE_BOOTSTRAP_EXISTS`, `REMOTE_BOOTSTRAP_ARCH_MISMATCH`,
`REMOTE_BOOTSTRAP_CHECKSUM_MISMATCH`, `REMOTE_BOOTSTRAP_VERSION_MISMATCH`,
`REMOTE_BOOTSTRAP_PROTOCOL_MISMATCH`, `REMOTE_BOOTSTRAP_DISK`,
`REMOTE_BOOTSTRAP_UNSUPPORTED`, `REMOTE_BOOTSTRAP_UPLOAD_FAILED`,
`REMOTE_BOOTSTRAP_INSTALL_FAILED`, `REMOTE_BOOTSTRAP_NOT_INSTALLED`,
`REMOTE_BOOTSTRAP_FAILED`, plus `REMOTE_NETWORK_DENIED` and
`REMOTE_NOT_CONFIGURED` for policy/configuration refusals. The RPC carries the
same bounded `{code,message,retryable,hint,evidence,tool}` object as the tool
error contract.

### 4. Remote test evidence

`remote.test` / `vp remote-test` runs one explicit project command:

- The project must opt in with `RemoteTestPolicy.enabled`. Profiles are named,
  reviewable `check`/`lint`/`test`/`other` commands with bounded names,
  commands, relative cwd, and optional timeouts. A one-off `--command` is
  allowed only while the policy is enabled and is recorded as `ad-hoc`.
- Missing policy, profile, command, session, or a non-remote queue entry
  returns a typed actionable error. No test claim is made.
- The command runs through the existing helper `exec` operation: a finite
  `timeout_ms`, per-stream output cap (1 KiB..4 MiB), process-group kill on
  timeout/cancel, and the helper's environment allowlist. The daemon relies on
  the helper's `cwd` path jail and never constructs an absolute cwd.
- Before running, the daemon resolves the remote `HEAD^{commit}` and refuses if
  an expected commit or a remote queue-entry commit does not match. Evidence is
  therefore tied to the exact commit a reviewer sees.
- Output is stored as a daemon-owned content-addressed file under
  `remote-test-outputs/<sha256>.txt` (0600, atomic write), capped again at
  the evidence body bound. The row stores only the reference, SHA-256, byte
  count, and truncation flag. `remote.test.output` pages the body by bounded
  byte ranges and rejects references outside that directory.
- `remote_test.completed` is appended in the same transaction as the evidence
  row. The payload and result explicitly state `supplemental = true`,
  `local_verifier_required = true`, and `merge_gate_unchanged = true`.
- Evidence is event-replayable: `session.replay` rebuilds the
  `remote_test_evidence` projection from `remote_test.completed` events and
  reports drift. Output bodies are operational state and are not reconstructed.

### 5. Merge queue attachment does not change the gate

When an entry is supplied, the daemon requires a remote-sourced queue row in
the same session and the exact entry commit. After evidence is persisted, it
updates `remote_test_evidence_id`, `remote_test_digest`, and
`remote_test_status` on the row and emits `merge_queue.updated` with
`reason = remote_test_evidence_attached` and `supplemental = true`.

Those fields are additive. The operation never changes `status`, `commit`,
`verifier_approved`, `verifier_summary`, or `environment_caveat`. The local
Verifier still must approve, and the deterministic Integrator still runs
`--no-ff`, `git diff --check`, and any configured integration command. A
passing remote test cannot enqueue, merge, or skip verification. It is
evidence for a human or Verifier to consider, not a merge bypass.

### 6. Caveats and limits

- The SSH host is trusted infrastructure. Bootstrap does not sandbox the remote
  user, and `remote_exec` still runs under that user's authority.
- Remote test evidence does not reproduce the local Verifier environment, the
  remote OS/tool versions, environment variables, network, or uncommitted
  files. The fixed caveat says so and is stored in every row and result.
- Network isolation is not claimed. A `network = deny` project refuses remote
  exec/bootstrap because the remote host cannot be confined by the local
  Seatbelt policy; when network is allowed, the remote command can use it.
- The local helper binary must be built for the remote target. The daemon
  detects header/`uname`/`--describe` mismatches and refuses to install, but it
  does not cross-compile, package, or download a helper.
- There is no remote package manager integration, sudo install, system path
  install, service installation, or automatic helper upgrade. The default
  install is under the SSH user's home.
- Bootstrap is not transactional across hosts: each host/path record is
  independent, and a successful rename on one host does not imply anything
  about another.
- Remote test evidence does not create remote test-cache sharing, distributed
  verification, or cross-host merge fencing. Those remain explicit non-goals.
- Remote test output bodies are daemon-owned operational state. There is no
  automatic age-based prune yet; operators can remove the evidence directory
  after the metadata rows are no longer needed.

## Consequences

- Operators can install a matching helper through an auditable, checksum- and
  version-verified path without sudo or manual copying.
- Bootstrap failures leave the previous helper untouched and return stable
  codes that CLI/Swift can render with actionable hints.
- Reviewers get commit-bound, time-bounded remote test evidence with output
  references and an explicit environment caveat.
- The merge queue gains three additive columns and two additive UI fields; old
  rows and old clients decode them as `None`.
- The Verifier and Integrator remain the only merge authority. The module adds
  evidence, not authority.

# ADR-0035: Confine shell writes with a macOS Seatbelt sandbox

- **Status:** Accepted
- **Date:** 2026-10-30

## Context

The shell tool was approval-gated but could still write anywhere the daemon user
could write. The runtime's path checks only covered `read_file`, `write_file`,
`edit_file`, and related tools; arbitrary shell commands could `cd` out of the
worktree and modify the project root, another checkout, or user files. This
contradicted the invariant that agents must not write outside their worktree.

A full App Sandbox or VM/container runtime is not available for this direct
distribution build yet. macOS still ships `sandbox-exec` (Seatbelt), which can
apply an inherited write policy to a child process tree without Developer ID or
entitlements.

## Decision

1. **Seatbelt write confinement.** The shell tool invokes
   `/usr/bin/sandbox-exec` with a profile that allows normal reads (except
   explicit credential/state denials), process execution, and network access,
   but denies file writes outside an allowlist.
2. **Allowlist.** Writes are allowed to:
   - the agent worktree;
   - the worktree's Git directory and the project's common Git directory, so
     normal `git add` / `commit` / `status` / `fetch` work;
   - a runtime-owned scratch directory under the VerlinPark data directory,
     unique to the session and agent;
   - `/dev/null`.
3. **Git metadata exception.** Git needs to write its index, refs, objects, and
   logs outside the checkout. The common Git directory is writable, but the
   common `hooks/` directory and `config` file, plus the worktree
   `config.worktree`, are explicitly denied so the shell cannot plant hooks or
   config that execute later outside the sandbox.
4. **Git paths are validated.** The worktree's `.git` file is not trusted on its
   own. The project root may point at the project Git directory; any other
   checkout must point at a worktree under the project's `worktrees/` directory.
   Otherwise the project Git directory is used. This stops a tampered `.git` file
   from widening the write allowlist.
5. **Scratch and caches.** `TMPDIR`, `XDG_CACHE_HOME`, `CARGO_HOME`, `GOCACHE`,
   `GOMODCACHE`, npm/yarn/pip cache variables, and the Clang module cache point
   into the per-agent scratch directory. The real home directory remains
   readable but is not writable.
6. **Environment allowlist.** The shell starts with a cleared environment and
   only passes standard build/tool variables (PATH, HOME, locale, toolchain
   roots, SSH agent, etc.). Provider keys, cloud credentials, tokens, and
   daemon configuration are not inherited; a command must set extra project env
   explicitly.
7. **Fail closed.** If `sandbox-exec` is unavailable, the shell tool refuses to
   run the command instead of silently falling back to an unsandboxed shell.
8. **Credential/state read denials.** Common credential and local-state paths
   are denied for reads: `~/.ssh`, `~/.gnupg`, cloud/gh/kube/docker credential
   directories, Keychains, shell/package credential files, and the VerlinPark
   database/run/session/log directories. This is a denylist, not a complete read
   jail; custom secret locations may still be readable.
9. **Process group cleanup.** Each shell runs in its own process group. Timeout,
   cancellation, or dropping the tool future sends `SIGKILL` to the whole group,
   so background processes do not outlive the call.
10. **Approval remains required.** The sandbox is a write boundary, not an
    approval bypass. The UI states that reads, network, and daemon-mediated
    writes are not confined.

## Consequences

- Agents can still write build artifacts, source files, and Git commits/refs for
  their own worktree, but direct filesystem writes to the project root and user
  files are denied by the kernel sandbox.
- Build caches are isolated per agent, which increases disk use and can make the
  first build slower. Commands that require global installs or writes to
  `~/Library`, `~/.cargo`, SSH config, or another daemon's state may fail; agents
  should use project-local tooling or an explicitly approved workflow.
- Project-specific environment variables are not inherited. A command that needs
  one must set it inline or load it from a project file; this is intentional so
  provider/cloud secrets do not appear in shell output or traces.
- Common credential/state paths are read-denied, but arbitrary reads and network
  access are not confined; custom secret locations may still be readable. Writes
  mediated by another daemon (for example Docker's socket) are not confined.
  Apple Events are denied, but this is not a complete defense against
  GUI/launch-service mediated writes.
- Shell commands that expect to leave a background daemon running will have that
  process group killed when the tool call ends.
- The shared Git directory is writable so commits and fetches work. An approved
  shell can therefore modify Git refs or metadata for the project (not project
  source files); hooks/config are denied, and destructive Git commands still
  require approval.
- The Verifier can still modify files inside its own worktree through shell.
  Verifier "read-only" remains a prompt/policy constraint, not a sandbox.
- `sandbox-exec` is deprecated. A future runtime should move to an App Sandbox
  helper, container, or VM without weakening the write allowlist.
- Shell scratch directories currently persist under the data directory and need
  a cleanup policy.

## Alternatives considered

- **Post-hoc path auditing:** cannot prevent damage and misses background writes.
- **Static command parsing:** trivially bypassed by shell scripting.
- **Allow only a fixed command list:** breaks real builds and tests.
- **Full App Sandbox:** requires a different distribution/signing model and
  sandbox entitlements; not compatible with the current direct build.
- **Read-only bind mount plus copy-on-write:** stronger but needs a root helper or
  filesystem support; not available in the current app.

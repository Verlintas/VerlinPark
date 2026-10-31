# ADR-0076: Opt-in home read jail

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

Shell writes were confined, but reads were unrestricted. Credentials, SSH keys,
browser state, and other home-directory data were still readable by any shell
tool call. A complete filesystem jail needs a large, toolchain-specific
allowlist; shipping it by default would break common builds.

## Decision

1. **Per-project opt-in.** `ShellPolicy.restrict_home` defaults to false.
   Existing projects keep their behavior until a user enables it.
2. **Deny HOME, allow the workspace.** The Seatbelt profile denies `file-read*`
   under `HOME`, then allows:
   - the worktree, scratch, Git metadata, and common Git dirs;
   - `~/.gitconfig`, `~/.ssh/config`, and `~/.ssh/known_hosts` when present;
   - known toolchain/cache directories (Cargo, Rustup, NVM, Volta, asdf, pyenv,
     Bun, local installs, Xcode Developer, and paths from common env vars).
3. **Metadata only for the rest of HOME.** `file-read-metadata` is allowed under
   HOME so directory traversal and `cd` work; file contents remain denied.
4. **Explicit read-deny wins.** Existing project `read_deny` rules are appended
   after the jail allows, so they still deny specific paths.
5. **No HOME rewriting beyond the jail.** `HOME` remains the user’s home (the
   process uses the same value), so commands attempting `~/.zshrc` or `~/.ssh`
   fail rather than silently reading a scratch copy.
6. **Private keys stay denied.** SSH via an agent can work through
   `SSH_AUTH_SOCK`; `gh` auth/config under HOME is not allowed unless the user
   explicitly adds a read-deny exception or disables the jail.
7. **Scope.** This is a HOME read jail, not a complete filesystem jail: other
   users’ homes, `/Volumes`, arbitrary paths outside HOME, metadata, network
   under `allow`, and daemon-mediated writes remain outside the boundary.

## Consequences

- Users gain a meaningful protection against common credential theft by shell
  commands without breaking Cargo/Rust/Xcode-style toolchains by default.
- The allowlist is heuristic; some tools may need extra paths or the jail must be
  disabled.
- `sandbox-exec` remains deprecated.
- Reads outside HOME are still unconfined; use `read_deny` for targeted paths.

## Alternatives considered

- **Default-on:** too likely to break existing projects.
- **Deny all reads except the worktree:** breaks compilers, runtimes, and Xcode.
- **Read-deny only:** already available, but users must enumerate every secret.

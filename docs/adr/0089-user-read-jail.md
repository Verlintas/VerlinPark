# ADR-0089: Opt-in user/volume read jail

- **Status:** Accepted
- **Date:** 2026-11-06

## Context

The shell sandbox confines writes to the agent worktree, its Git metadata, and
runtime scratch. `ShellPolicy.restrict_home` can additionally deny reads under
`$HOME`, but it leaves other users' homes, `/Users/Shared`, and `/Volumes`
readable. That is too broad for users who run agents on a personal Mac with
sensitive data outside the project.

A fully global `(deny file-read*)` profile aborts the macOS sandbox helper in
practice because process startup and dynamic loading need system reads. The
feasible boundary is a policy deny over user-data roots plus an explicit
allowlist, not a deny of every filesystem read.

## Decision

1. **New `ShellPolicy.read_jail` flag**, default `false`, stored in
   `project_shell_policies.read_jail` and included in project manifests.
2. **Seatbelt deny roots.** When enabled, the profile denies `file-read*` under
   the canonical parent of `$HOME` (normally `/Users`) and under `/Volumes`.
   This covers sibling user homes, `/Users/Shared`, and external volumes.
3. **Explicit allows.** The worktree, runtime scratch, Git metadata, known
   toolchain/cache roots, `.gitconfig`, and SSH config/known_hosts are allowed
   again after the deny. Existing credential read-denies run last.
4. **System reads stay available.** `/System`, `/usr`, `/bin`, `/sbin`,
   `/Library`, `/Applications`, `/private/etc`, `/dev`, temporary directories,
   and other system paths remain readable. This preserves compilers, Git, and
   normal shell startup. It is intentionally not a claim of full read
   confinement.
5. **Surface.** Manage Dash exposes a “Read jail (all users + volumes)” switch;
   CLI `vp shell-policy-set --read-jail`; RPC uses the existing shell-policy
   shape with `read_jail`.
6. **Wiring.** Agent shell, Console, and PTY Terminal all pass the policy into
   `ToolContext`, so the same jail applies to every daemon-owned shell path.

## Consequences

- A malicious or confused agent cannot read another user's home, `/Users/Shared`,
  or mounted volumes through the shell by default when the policy is enabled.
- Toolchain execution continues to work because system and toolchain roots are
  allowlisted.
- This is not full filesystem read confinement: `/private/tmp`, `/private/var`,
  system configuration, and other non-denied paths remain readable, network
  access under `allow` can still exfiltrate data, and process execution is not
  restricted.
- Some projects or tools that read non-standard user caches may need additional
  allowlist entries in a future policy extension.

## Alternatives considered

- **Global file-read deny:** aborts `sandbox-exec`/process startup in testing;
  not viable without a much larger system allowlist.
- **HOME-only deny:** existing `restrict_home`; leaves sibling users and volumes
  readable.
- **Full filesystem jail:** requires a carefully generated system dependency
  graph or a different sandbox mechanism; larger than this slice.

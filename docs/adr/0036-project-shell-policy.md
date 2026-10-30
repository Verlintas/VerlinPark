# ADR-0036: Per-project shell permission policy

- **Status:** Accepted
- **Date:** 2026-10-30

## Context

The shell sandbox started with a fixed write allowlist, a fixed credential
read-denylist, and network access always enabled. Different projects need
different risk profiles: an offline migration task may want no network, while a
project with a private secrets directory may want an extra read denial. A global
hard-coded list cannot express that, and allowing projects to widen permissions
would defeat the sandbox.

The runtime also needs a UI and CLI path that does not require editing database
files or restarting the daemon.

## Decision

1. **Policy shape.** A project has a `ShellPolicy`:
   - `network`: `allow` or `deny` (default `allow`);
   - `read_deny`: additional paths to deny for reads (default empty).
2. **Policy only narrows.** It cannot disable the write sandbox, environment
   allowlist, built-in credential read-denials, or process-group cleanup. There
   is no project-level "unsafe shell" switch.
3. **Persistence.** The policy is stored in the `project_shell_policies` table,
   keyed by project ID. Projects without a row use the default policy. It is
   project metadata, not session state, and is not included in session bundles.
4. **Validation.** `network` must be `allow` or `deny`. Read-deny entries must be
   absolute or start with `~/`, are limited to 64 entries and 4096 bytes each,
   are canonicalized when they exist, and are deduplicated. Relative paths are
   rejected instead of being interpreted ambiguously.
5. **Runtime snapshot.** The agent reads the policy when a run starts and passes
   it into its tool context. Changes apply to subsequent runs; an in-flight run
   keeps its snapshot.
6. **Enforcement.** Extra read-deny paths are appended to the shell's Seatbelt
   read-denial rules. `network: deny` adds a Seatbelt `(deny network*)` rule,
   which also blocks Unix-domain sockets such as Docker's. This can break package
   installs, `git fetch`, and network tests; that is the intended trade-off when
   a project chooses `deny`.
7. **API and UI.** RPC `project.shell_policy.get` / `project.shell_policy.set`;
   CLI `vp shell-policy` / `vp shell-policy-set`; Manage Dash exposes a project
   card with network toggle, one-path-per-line read-deny editor, save, and reset.
   Approval cards show the current network mode and extra read-deny count.

## Consequences

- Teams can make a project offline or hide project-specific secret paths without
  changing global configuration.
- The policy cannot be used to widen access; a compromised project file cannot
  disable the sandbox.
- Read-deny remains a denylist. Unlisted secret paths are still readable, and
  network allow still permits exfiltration.
- Network deny is blunt: it blocks all sockets, including local service sockets,
  not just the public internet.
- Policy changes are not events and are not exported with session bundles. A
  future project settings export can add them.
- The UI and CLI validate the same rules; the daemon is authoritative.

## Alternatives considered

- **Global policy only:** cannot handle per-project risk and offline tasks.
- **Per-session policy:** harder to audit and easy to forget; project-level is a
  stable default.
- **Allow projects to add writable roots:** useful for monorepos/caches but
  widens the sandbox; deferred until there is a separate approval design.
- **Full network allowlist/proxy:** stronger but larger; `allow`/`deny` is the
  first useful step.

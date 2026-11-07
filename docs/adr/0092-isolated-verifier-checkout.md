# ADR-0092: Isolated Verifier checkout

- **Status:** Accepted
- **Date:** 2026-11-07

## Context

The Verifier has no write/edit tools, and the runtime rejects tracked-file
changes after its run. Its shell could still create untracked/ignored files or
mutate Git state in the Worker worktree. Those changes were not part of the
integrated commit, but they made "Verifier is read-only" less true than the UI
implied, and they could interfere with a later retry.

## Decision

1. **Verification runs in a detached clone.** After `commit_worker` returns a
   commit, `verify_worker` creates a fresh `git clone --shared --no-checkout` of
   the project under `<data>/verify/<session>/<uuid>`, checks out the exact
   worker commit detached, and runs the Verifier there.
2. **Worker branch is untouched.** The clone has its own refs and Git metadata
   (objects are borrowed read-only via alternates). Shell/Verifier writes,
   commits, resets, or test artifacts only affect the clone.
3. **Cleanup is guaranteed.** A `VerificationCheckout` drop guard removes the
   clone after the Verifier run, including error paths. Failed worktree creation
   returns a Verifier error rather than falling back to the Worker worktree.
4. **Post-run guard remains.** Tracked-file checks/reset still run on the clone
   as a defense-in-depth check; an approved verdict still integrates the original
   worker commit, not the clone.
5. **Event/UI path.** The Verifier agent and events report the isolated worktree
   and a `verify/<commit-prefix>` branch label. The clone is ephemeral and is not
   exported or rehydrated.

## Consequences

- A Verifier cannot modify the Worker worktree, branch, index, or later retry
  state through ordinary shell/Git commands.
- Tests must run against the committed tree; untracked files that the Worker
  forgot to commit are intentionally unavailable.
- `git clone --shared` is fast but the clone's object store references the
  project; sandbox write confinement still prevents writes to the source object
  directory.
- This is not a network or authentication sandbox: an explicitly allowed
  Verifier shell could still use network credentials passed in the environment.
  The existing shell policy and approval model remain the outer boundary.

## Alternatives considered

- **Clean untracked files in place:** hard to distinguish pre-existing build
  caches and cannot undo Git ref/index changes.
- **A second linked worktree:** shares refs with the project and is not isolated
  enough.
- **Deny all Verifier shell writes:** breaks test/build commands that need temp
  outputs; a clone provides isolation without that restriction.

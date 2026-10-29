# ADR-0029: Explicit local landing; PR flow deferred

- **Status:** Accepted
- **Date:** 2026-10-30

## Context

The session branch was reviewable but had no final landing action. The UI literally
said “Landing is not implemented yet.” Users needed a safe, explicit way to move
reviewed session work into the project’s default branch without turning a queue
merge into an implicit push.

The project is distributed directly (GPL-3.0-only); there is no requirement to
assume a hosted Git provider.

## Decision

1. **Landing is an explicit user action.** `session.land` is never called by an
   agent as part of a task.
2. **Local-only for this slice.** The runtime merges the session branch into the
   project’s stored default branch with `--no-ff`.
3. **Preconditions are checked.** The project checkout must be clean and checked
   out on the default branch. If not, landing fails with a clear error.
4. **No silent remote side effects.** This slice does not push and does not create
   a PR. It emits `session.landed`, records the target branch and commit, and sets
   the session status to `landed`.
5. **UI/CLI.** Preview → Changes exposes a Land button when the session has
   committed work and no uncommitted changes; `vp land <session>` is the
   headless equivalent.
6. **PR/push remains a separate decision.** A future `session.land` mode or
   `session.push`/`session.pr` API may add remote operations, but it must be
   explicit, provider-aware, and tested against a real remote/credential failure.

## Consequences

- Users can complete the local review → land loop without leaving VerlinPark.
- Landing remains auditable and conservative; no accidental push.
- PR creation is not yet available, so a user who wants a PR must push the session
  branch and open it manually.
- Landing into a local default branch is only safe for a clean checkout; the
  runtime refuses to guess.

## Alternatives considered

- **Auto-push + `gh pr create`:** useful, but requires remote/auth assumptions
  and network side effects; should be opt-in and tested separately.
- **Land into a new local branch:** safer but does not satisfy “land to target
  branch”; can be added as a mode later.
- **Auto-land after merge queue:** rejected; a human must approve the final
  delivery.

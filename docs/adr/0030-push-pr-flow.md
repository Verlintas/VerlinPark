# ADR-0030: Explicit push + GitHub PR flow

- **Status:** Accepted
- **Date:** 2026-10-30

## Context

ADR-0029 added explicit local landing but deliberately deferred remote side
effects. A local-only land is not enough for teams whose delivery path is a pull
request. The next step is an explicit “push branch and open PR” action.

This repository uses GitHub and the GitHub CLI (`gh`) is a reasonable direct
dependency for the first PR flow. It must remain opt-in: no agent, queue entry,
or landing action should push or open a PR without the user clicking it.

## Decision

1. **`session.pr` is user-triggered.** The session must not be running; the
   session worktree must be clean; the branch must be ahead of the default
   branch.
2. **Preflight before side effects.** Require a git `origin` remote and require
   `gh auth status` to succeed before pushing.
3. **Push, then create the PR.** Push the session branch with
   `git push -u origin <branch>`; then run
   `gh pr create --base <default> --head <branch> --title <goal> --body ...`.
4. **No fake success.** If the push fails, no PR is attempted. If the PR command
   fails after the push, the error explicitly says the branch was pushed and
   reports the `gh` error. `session.pushed` is recorded before the PR attempt.
5. **Audit events.** Emit `session.pushed` and `session.pr_opened` with branch,
   base, and URL. `PullRequestResult` is returned through RPC/CLI/UI.
6. **Provider boundary.** This first flow assumes GitHub CLI semantics. Other
   remotes/providers can be added behind an explicit adapter later; no hidden
   provider detection.
7. **Environment override.** `VERLINPARK_GH_BIN` can point at a `gh` binary
   (used by the fake-gh integration test and useful for controlled installs).

## Consequences

- Users can complete review → push → PR from the app/CLI without a terminal.
- A failed `gh` step can leave a pushed branch; this is surfaced, not hidden.
- `gh` must be installed and authenticated for PR creation. Local landing remains
  available without it.
- PR creation has network side effects and is not covered by the deterministic
  merge gate; it is a delivery action, not an agent permission.

## Alternatives considered

- **Push + print a compare URL without `gh`:** works everywhere but does not
  create the PR; can be added as a fallback.
- **Provider APIs directly:** more control, but credential/provider scope and
  error handling are much larger than this slice.
- **Auto-open PR after queue merge:** rejected; human review/landing stays
  explicit.

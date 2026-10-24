# ADR-0017: Explicit worktree commit from review

- **Status:** Accepted
- **Date:** 2026-10-24

## Context

The Leader edits files in a session worktree (ADR-0016). Reviewers can see the
diff, but the changes remain uncommitted, so there is no stable point to return
to, no commit hash to reference, and no way to land work without leaving the app.
Auto-committing after every tool call would create noisy history and hide the
review step.

## Decision

1. Add an explicit `session.commit` RPC and a **Commit** action in Preview →
   Changes. The user supplies the message; the action is never automatic.
2. The commit runs in the session worktree only:
   `git add -A` followed by `git commit -m <message>`. Staged and untracked files
   are included. The project root is never touched.
3. Commit refuses when the worktree is clean and returns the short hash plus a
   one-commit summary.
4. Emit `session.committed` with the hash and message so the trace is complete.
5. No push, merge, rebase, or branch deletion in this milestone. Landing remains
   a manual Git operation until the merge queue exists.
6. The UI reloads the diff after commit and shows the committed hash.
7. A separate **Discard** action runs `git reset --hard HEAD` + `git clean -fd` in
   the worktree after confirmation. It removes uncommitted changes only; commits
   are kept.

## Consequences

- The user gets a stable commit per reviewed unit of work without leaving the app.
- Because `git add -A` includes untracked files, the status list is the safety
  surface; a future review UI should show untracked file contents before commit.
- There is no partial staging or hunk selection yet.
- Branches accumulate per session until cleanup/merge lands.

## Alternatives considered

- **Auto-commit each turn:** hides review, produces noisy history, and makes
  failed experiments harder to discard.
- **Only expose git through the shell tool:** works, but the review surface would
  not have a first-class action and the event trail would be less clear.
- **Commit and merge in one action:** too risky before the merge queue and
  verifier gates exist.

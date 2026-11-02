# ADR-0083: Read-only Git history viewer

- **Status:** Accepted
- **Date:** 2026-11-02

## Context

Preview showed the working-tree diff and merge queue, but not the commit history
of the session branch. Users had to use the Console or an external Git client to
inspect what had been committed.

## Decision

1. **Daemon-owned Git reads.** `git.log` and `git.show` run `git -C <session
   worktree>` directly, without a shell. The worktree is resolved through the
   same session/imported checks as the file browser.
2. **Bounded log.** At most 200 commits, parsed from a unit-separator
   `--pretty=format` with hash, short hash, author, email, ISO date, subject, and
   refs.
3. **Validated commit hashes.** `git.show` accepts only 4–40 hexadecimal
   characters, so revision options or paths cannot be injected.
4. **Bounded patch.** `git show --stat --patch` is capped at 200 KB and marks
   truncation.
5. **Surfaces.** Preview has a History tab with branch/base metadata, a commit
   list, and a read-only diff viewer. CLI exposes `vp history` and `vp show`.
6. **Read-only.** There is no checkout, reset, rebase, commit, amend, or history
   rewrite from this feature. Those remain explicit Git/Console actions.

## Consequences

- Users can inspect session commits without leaving the app.
- Log parsing intentionally omits full commit bodies; `git.show` contains the
  message and patch.
- Large patches are previews, not full exports.
- History is best-effort for unusual commit encodings because parsing uses unit
  separators and line splitting.

## Alternatives considered

- **Use `git2`:** typed and avoids subprocess parsing, but adds a heavy native
  dependency and duplicates Git behavior already present in the app.
- **Run `git log` through the shell sandbox:** unnecessary; read-only Git
  commands do not need a shell and the direct argv path avoids quoting bugs.
- **Show full history of the project branch:** would mix unrelated commits into
  the session view; the worktree branch is the useful scope.

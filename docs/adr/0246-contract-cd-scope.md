# 0246 — Contract shell scope follows `cd`

- Status: Accepted
- Date: 2026-11-28
- Scope: task-contract shell path analysis
- Depends on: ADR-0193 (task contracts), ADR-0238 (scratch aliases), ADR-0241
  (find roots)

## Context

A Verifier ran a mutation check:

```sh
cd "$VERLINPARK_SCRATCH" 2>/dev/null || cd "$TMPDIR"
rm -rf t07check
mkdir t07check
cd t07check
cp -R "$OLDPWD"/app "$OLDPWD"/tests .
python3 -m unittest discover -s tests -p 'test_m07.py' -v
```

Contract analysis resolved relative paths against the worktree regardless of
the preceding `cd`, so `rm -rf t07check` looked like an out-of-scope worktree
deletion and requested a destructive host-state approval.

## Decision

`analyze_shell_inner` tracks `cd`/`pushd` for the rest of the command:

- `$VERLINPARK_SCRATCH`/`${VERLINPARK_SCRATCH}` and `$TMPDIR` map to internal
  sandbox sentinels; `$VERLINPARK_WORKTREE` maps to the worktree.
- A relative path under a sandbox sentinel (including a nested `cd`) is a
  sandbox-local write and is in scope when it has no `..`.
- A relative path under a worktree subdirectory is resolved back to a
  worktree-relative path before applying the task scope.
- Absolute paths still use the normal scope check; `cd /tmp` therefore makes a
  relative write resolve under `/tmp` and stay out of scope.
- `..`, `$`-containing targets, `cd -`, and unresolvable targets keep the old
  conservative behavior. The shell sandbox remains authoritative.

## Consequences

- Scratch mutation tests no longer look like worktree deletions.
- A `cd` into a worktree subdirectory no longer incorrectly makes a sibling
  file look in-scope; it is resolved to its real worktree path.
- `pushd` is treated as `cd`; the directory stack is not modeled, and `cd -`
  is not followed.
- This is syntactic analysis, not a shell interpreter.

## Follow-up: assignment-only segments

A bare variable assignment (`S=...`) is a valid shell segment with no command.
The contract analyzer previously marked it `ambiguous`, which turned every
multi-line `S=...` verification script into an approval. Assignment-only
segments are now skipped; an assignment containing `$()`/backticks remains
ambiguous because the analyzer does not execute substitutions.

## Tests

`contract::tests::contract_scope_tracks_cd_into_scratch_and_worktree_subdirs`
covers scratch/TMPDIR cds, nested scratch cwd, absolute `/tmp`, and a worktree
subdirectory write outside the declared scope.

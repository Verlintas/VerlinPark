# 0241 — Find roots are search scopes, not write targets

- Status: Accepted
- Date: 2026-11-28
- Scope: shell permission analysis of `find -exec` and `find -delete`
- Depends on: ADR-0168/0170 (shell sandbox), ADR-0193 (task contracts), ADR-0240
  (copy-source semantics)

## Context

A live Worker cleaned generated Python caches with:

```sh
find . -name __pycache__ -type d -not -path './.git/*' -exec rm -rf {} +
```

`assess_find` treated the search root as the command's write target. For root
`.`, that resolved to the worktree root and produced a host-state
`the worktree root itself is the target` approval, even though the nested
`rm -rf` only receives matched cache directories.

## Decision

1. The `find` root is a traversal scope, not a write target. `.` and the
   configured worktree root are allowed search roots.
2. `-exec` still assesses the nested command with the normal shell analyzer, so
   `find . -exec rm -rf / +` remains destructive/blocked and relative nested
   writes are evaluated against the worktree.
3. `-delete` is a workspace write when the search root is the worktree, scratch,
   or another sandbox-confined path. It never becomes an auto-approved host
   write.
4. A root outside the worktree/sandbox (`/`, `/Users/...`) or an unresolvable
   root still escalates before any `-exec`/`-delete` runs.
5. The Seatbelt profile remains authoritative; this only changes the risk
   classification and approval prompt.

## Consequences

- Routine cache cleanup with `find . -exec` no longer requests host-state
  approval.
- `find / ... -exec rm` and `find /Users/... -delete` remain blocked/host-state.
- A nested destructive command is still classified by its own arguments.
- The analyzer does not evaluate `-name`/`-path` patterns semantically; an
  in-worktree `find . -delete` may remove more than intended but stays inside
  the sandbox and the session worktree.

## Tests

`permissions::tests::find_exec_and_delete_use_the_nested_command_for_writes`
covers worktree/scratch roots, `-exec`, `-delete`, outside roots, and a nested
destructive command.

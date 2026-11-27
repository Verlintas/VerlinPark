# 0242 — Redirection tokens are not path arguments

- Status: Accepted
- Date: 2026-11-28
- Scope: task-contract shell argument analysis
- Depends on: ADR-0193 (task contracts), ADR-0240 (copy sources), ADR-0241
  (find roots)

## Context

A live Worker ran:

```sh
rm -rf app/__pycache__ tests/__pycache__ 2>&1; echo "EXIT=$?"; git status
```

The contract's shell-word pass kept `2>&1` as an argument. `shell_path_in_scope`
could not normalize it, so it looked like an out-of-scope write target and the
`rm` arm escalated to a destructive `CONTRACT_SHELL_AMBIGUOUS` approval.

## Decision

Contract analysis removes shell redirection operators and their targets from
the argument list before command-specific checks:

- bare `>`, `>>`, `<`, `<<`, `2>`, `2>>`, `&>`, `&>>` consume the following
  target word;
- inline forms (`2>&1`, `2>file`, `>/dev/null`, `<<EOF`, `&>file`) are removed
  as one word;
- the existing separate `redirection_target` audit still checks `>` targets
  against the task scope, so redirect writes are not hidden.

This is a bounded tokenizer fix, not a shell parser. Quoted strings that merely
contain `>` are not treated as redirections once shell words are built, and the
existing scope and sandbox checks remain authoritative.

## Consequences

- `rm -rf app/__pycache__ tests/__pycache__ 2>&1` is a normal cache cleanup,
  not an out-of-scope write.
- Redirects to out-of-scope paths are still flagged by `redirection_target`.
- `cp a b 2>&1` selects `b` as the destination rather than `2>&1`.
- Unusual filenames containing bare `>`/`<` remain conservative.

## Tests

`contract::tests::rm_python_caches_outside_the_declared_scope_is_not_a_scope_violation`
covers the live command; `shell_analysis_sees_push_remote_and_redirects` keeps
the out-of-scope redirect assertion.

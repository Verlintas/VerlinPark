# 0243 — Function-call parentheses are not shell segments

- Status: Accepted
- Date: 2026-11-28
- Scope: shell permission segment splitting for interpreter `-c` code
- Depends on: ADR-0168/0170 (shell sandbox), ADR-0239 (substitutions)

## Context

A live Worker inspected its files with:

```sh
python3 -c "print(repr(open('app/greet.py','rb').read()))"
```

The permission splitter treated every `(`/`)` as a shell segment boundary.
`open(` therefore became a segment whose first word was `open`, and the host
application launcher rule classified the whole command as
`opens host applications or URLs` (host-state approval), even though `open` was
a Python function call inside a quoted argument.

## Decision

Parentheses start a shell segment only at a real shell boundary: input start,
whitespace, or an operator (`;`, `|`, `&`, newline). A parenthesis immediately
following an identifier is part of that word, so function-call syntax is not
misread as a command.

- `( cd "$VERLINPARK_SCRATCH" && rm -rf app/__pycache__ )` still splits at the
  boundary parentheses and tracks the subshell `cd`.
- `python3 -c "print(repr(open(...)))"` stays one command; interpreter markers
  are still checked by `assess_interpreter_code`.
- `$(...)`/backtick substitutions remain skipped as a unit and assessed
  recursively (ADR-0239).

## Consequences

- Routine Python one-liners no longer request host application-launch approval.
- `python3 -c "import os; os.system('id')"` remains host-state through the
  interpreter-code markers, not through `system` being a command name.
- A closing `)` after a word remains part of that word; path targets ending in
  `)` are still resolved inside the worktree and the sandbox remains
  authoritative.
- This is a bounded syntactic heuristic, not a language parser.

## Tests

`permissions::tests::interpreter_function_calls_are_not_shell_commands` covers
the live one-liner, a second statement, `os.system`, and a subshell `cd`.

# 0239 — Heredoc-aware shell analysis

- Status: Accepted
- Date: 2026-11-28
- Scope: shell permission/contract analysis of heredocs and `$()`/backtick
  substitutions
- Depends on: ADR-0168/0170 (shell sandbox and policy), ADR-0194 (effect
  receipts), ADR-0238 (scratch aliases)

## Context

A live plan Worker wrote a test file with:

```sh
mkdir -p "$VERLINPARK_SCRATCH/t2check"
cat > "$VERLINPARK_SCRATCH/t2check/test_greet.py" <<'EOF'
...
EOF
PYTHONPATH="$(pwd)" python3 -m unittest discover -s "$VERLINPARK_SCRATCH/t2check" -v
```

The permission classifier split `"$(pwd)"` into fragments and left the quote
state unbalanced. The remainder of the line became one "command word" containing
`$VERLINPARK_SCRATCH`, which was classified as
`shell expansion hides the command name` and requested host-state approval. The
same splitter also treated heredoc bodies as shell commands, so fixture text
such as `rm -rf /` inside a quoted heredoc could be classified as destructive.

## Decision

1. `shell_text::strip_heredoc_bodies` removes heredoc bodies before segment
   splitting and shell-word parsing in both permission and contract analysis.
   Quoted delimiters (`<<'EOF'`, `<<"EOF"`) never expand, so their bodies are
   removed from every scan. Unquoted delimiters still expand; their bodies are
   kept only for the command-substitution scan.
2. `$()` and backtick substitutions are skipped as a unit by the segment
   splitter, respecting nested parens and quotes. Inner commands are assessed
   recursively by `extract_substitutions`, so `PYTHONPATH="$(pwd)" python3 ...`
   is a normal workspace-write command while `$(rm -rf /)` remains destructive.
3. Unterminated substitutions/heredocs keep the previous conservative behavior:
   an unterminated substitution stays in the outer segment, and an unterminated
   heredoc does not execute in a real shell, so its body is dropped.
4. The splitter changes are syntactic. Quote handling remains bounded; this is
   not a POSIX shell parser.

## Consequences

- Routine heredoc fixture writes and `PYTHONPATH="$(pwd)"` test commands no
  longer request host-state approval or create unknown effect receipts.
- Quoted heredoc content cannot smuggle a command into the classifier as if it
  were executable.
- Unquoted heredoc substitutions are still assessed, so
  `cat <<EOF\n$(rm -rf /)\nEOF` remains destructive.
- A heredoc body used as data for a program that later executes it is still the
  program's responsibility; the classifier only models shell expansion.

## Tests

- `shell_text` unit tests for quoted/unquoted bodies, `<<-`, here-strings, and
  missing delimiters.
- `permissions::tests::quoted_substitutions_and_heredoc_data_do_not_hide_the_command`
- `permissions::tests::unquoted_heredoc_substitutions_are_still_assessed`
- `permissions::tests::quoted_heredoc_data_is_not_executed_as_commands`

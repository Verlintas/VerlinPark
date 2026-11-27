# 0240 — Copy-source shell scope semantics

- Status: Accepted
- Date: 2026-11-28
- Scope: task-contract shell analysis of `cp`, `install`, and `ln`
- Depends on: ADR-0193 (task contracts), ADR-0238 (scratch aliases), ADR-0239
  (heredoc-aware shell analysis)

## Context

A live plan Worker verified its task by copying the worktree sources into the
exported scratch directory:

```sh
cp -R app tests "$VERLINPARK_SCRATCH/t1check"/
( cd "$VERLINPARK_SCRATCH/t1check" && python3 -m unittest discover -s tests -v )
```

The contract analyzer treated every non-flag argument of `cp` as a write
target. `app` and `tests` were therefore reported as writes outside the task's
`app/calc.py`/`tests/test_calc.py` scope, producing a host-state
`CONTRACT_SHELL_AMBIGUOUS` approval for a read-only copy source.

## Decision

`cp`, `install`, and `ln` write only their destination:

- `--target-directory=DIR` and `-t DIR` select the destination explicitly.
- `install -d`/`--directory` creates every non-flag argument, so all of them are
  write targets.
- Otherwise the last non-flag argument is the destination; earlier arguments
  are read-only sources.
- `mv` keeps the conservative all-arguments rule because it removes every
  source path, not just the destination.
- Sources are still read under the normal sandbox/read policy. Reading a file
  outside a task's write scope was already allowed; this change does not widen
  reads or writes.

The permission classifier already used destination-only semantics for
`cp`/`mv`/`install`/`ln`; contract analysis now matches it.

## Consequences

- Routine verification copies into scratch no longer request host-state
  approval or create unknown effect receipts.
- Copying a system file into an in-scope path is not a scope violation because
  the source is a read.
- Writing outside the scope through the destination is still rejected.
- `mv` remains conservative: moving an out-of-scope source is a real
  out-of-scope modification.
- Shell analysis remains syntactic; `--target-directory` parsing and
  option-value edge cases are bounded.

## Tests

`contract::tests::copy_sources_are_reads_and_only_the_destination_is_a_write`
covers scratch copies, out-of-scope destinations, read-only system sources,
`-t`, `mv`, and `ln`.

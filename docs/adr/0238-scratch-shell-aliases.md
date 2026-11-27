# 0238 — Bounded scratch shell-alias resolution

- Status: Accepted
- Date: 2026-11-27
- Scope: shell permission and contract analysis for local scratch aliases
- Depends on: ADR-0004 (worktree isolation), ADR-0168/0170 (shell sandbox and
  policy), ADR-0194 (effect receipts)

## Context

Workers and Verifiers commonly wrote:

```sh
SCRATCH="$VERLINPARK_SCRATCH/t1-check"
rm -rf "$SCRATCH"
mkdir -p "$SCRATCH/app"
```

The daemon exports `$VERLINPARK_SCRATCH`, but the permission classifier saw
`$SCRATCH` as an unanalyzable expansion and the contract analyzer saw its
target as outside the task scope. The result was repeated host-state approvals
and `EFFECT_UNKNOWN` receipts for throwaway scratch work. A live plan spent
many turns on this loop instead of verification.

## Decision

Before permission and contract shell analysis, resolve **simple local
assignments** whose value is rooted in a daemon-provided scratch/worktree path:

- `$VERLINPARK_SCRATCH`, `${VERLINPARK_SCRATCH}`
- `$VERLINPARK_WORKTREE`, `${VERLINPARK_WORKTREE}`
- `$TMPDIR`, `${TMPDIR}`

Only direct `$NAME`/`${NAME}` references are expanded, with a bounded number of
assignment rounds. Chained aliases such as `D="$SCRATCH/app"` resolve. Unknown
variables, `$HOME`, absolute host paths, command substitution, backticks, and
paths containing `..` are not made safe by this rule. The expanded text is
still evaluated by the normal permission/contract/sandbox/effect pipeline.

The helper lives in `vp-core/src/shell_alias.rs`; permission and contract
analysis both use it, so their decisions cannot drift.

## Consequences

- Routine scratch cleanup no longer creates host-state approvals or unknown
  effect receipts.
- The shell sandbox remains authoritative; this does not widen a filesystem
  path or permit an outside-worktree write.
- An alias is still just a string expansion. A command that escapes the scratch
  prefix through `..` is rejected by the permission/contract checks.
- Arbitrary variable tricks remain unanalyzable and keep the conservative
  host-state behavior.
- Direct `$HOME`/network/credential paths remain outside the safe set.

## Tests

- `shell_alias::tests::resolves_chained_scratch_aliases_only`
- `permissions::tests::chained_scratch_aliases_are_workspace_writes`
- contract `shell_cache_cleanup_is_not_destructive_or_out_of_scope` includes
  the alias form.

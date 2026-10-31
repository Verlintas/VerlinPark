# ADR-0049: Verifier tracked-file guard

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

The Verifier has no `write_file`/`edit_file` tools, but it can call `shell`, which
can write inside its worktree. Its read-only behavior was therefore prompt-level
only: a model could approve a change after editing source files. The worker
commit is already created before verification, so any tracked modification made
during verification is a policy violation, not required work.

## Decision

1. **No write tools.** The Verifier tool policy continues to exclude
   `write_file` and `edit_file`; only `read_file`, `list_dir`, `grep`, `shell`,
   and (for research profiles) `fetch_url` are exposed.
2. **Post-run tracked-file check.** After the Verifier submits a verdict, the
   runtime runs `git status --porcelain --untracked-files=no` in the verifier
   worktree.
3. **Reset and reject.** If any tracked file changed, the runtime resets tracked
   changes (`git reset --hard HEAD`), marks the Verifier `failed`, emits
   `agent.finished` with `approved: false` and `modified_paths`, and returns a
   synthetic rejection. The Worker is marked `changes_requested`; nothing is
   merged.
4. **Untracked files allowed.** Build outputs and test artifacts are not
   tracked, so they do not trigger the guard. This keeps shell-based tests
   usable without permitting source edits.
5. **Auto-resolution uses the same path.** Resolver verification runs through
   `verify_worker`, so a resolver Verifier gets the same guard.

## Consequences

- A Verifier cannot silently edit tracked source and approve it.
- A Verifier can still create untracked/ignored files and can read arbitrary
  paths; this is not a full filesystem sandbox.
- Reset is destructive for verifier edits. That is intentional: verification is
  supposed to be read-only, and the Worker's committed state is authoritative.
- The guard adds one `git status` call per verification.

## Alternatives considered

- **Remove `shell` from the Verifier:** breaks test execution, the main value of
  verification.
- **Read-only mount/worktree:** robust but requires a second checkout and a
  larger runtime change.
- **Trust the prompt:** the original status quo and the reason for this ADR.

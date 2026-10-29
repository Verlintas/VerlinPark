# ADR-0028: Deterministic merge queue and Integrator gate

- **Status:** Accepted
- **Date:** 2026-10-30

## Context

Verifier-approved Worker branches were merged directly with `git merge --no-ff`.
That was serial, but it had no persisted queue, no integration check, and no
rollback story. A merge could be recorded as failed without a durable entry, and
conflicts left only a Worker status. The next milestone needs a real
Verifier → queue → Integrator → session-branch path.

The product invariant says scheduling, permissions, and merge decisions should
prefer deterministic Rust; an LLM should be used inside a task, not for the
merge mechanics.

## Decision

1. **Persist a merge queue.** Every Verifier-approved Worker commit gets a
   `merge_queue` row with agent, optional task, branch, commit, base commit,
   status, attempt, and detail. Statuses: `queued`, `merging`, `checking`,
   `merged`, `conflict`, `failed`, `cancelled`.
2. **Enqueue before merging.** `finish_worker` enqueues the approved branch,
   marks it `merging`, and only then touches the session worktree.
3. **The Integrator is deterministic.** It merges with `--no-ff`, runs a
   language-agnostic `git diff --check` against the pre-merge base, and
   optionally runs a user-provided command from
   `VERLINPARK_INTEGRATION_COMMAND` (timeout 300s). No hidden LLM call and no
   hidden command.
4. **Rollback on failed integration.** If the diff check or command fails, the
   session worktree is reset to the pre-merge commit. The Worker branch and
   commit are retained for review.
5. **Conflicts abort cleanly.** A conflicting merge is aborted; the entry is
   marked `conflict`, the session branch is unchanged, and the Worker is marked
   failed. Automatic rebase/retry is not claimed yet.
6. **Observable.** `merge_queue.enqueued`, `merge_queue.updated`,
   `integrator.started`, and `integrator.finished` events carry the entry and
   result. `merge_queue.list`, `vp queue`, and Preview → Queue expose the same
   state.
7. **Queue is session-local.** The global cross-session queue, write-scope locks,
   dependency blocking across entries, and automatic conflict resolution remain
   future work.

## Consequences

- Every approved merge now has a durable, inspectable queue record.
- A failed integration check cannot silently leave a bad merge on the session
  branch; it is rolled back.
- The integration command is explicit and opt-in; projects without one still get
  the diff check, not a language-specific test assumption.
- The queue currently drains one entry at a time as each Worker completes. It is
  persistent and serial; retry/rebase is now available for conflict/failed entries,
  but it is not yet a batch scheduler with automatic conflict resolution.
- An LLM Integrator that reviews semantics/conflicts can be added later as an
  optional reviewer; it must not replace deterministic merge mechanics.

## Alternatives considered

- **Keep direct merge:** less state, but no audit/rollback/retry surface.
- **LLM Integrator as the default:** expensive, non-deterministic, and violates
  the “deterministic Rust for merge” invariant.
- **Auto-resolve conflicts:** dangerous and hard to audit; rejected.
- **Assume `cargo test`:** convenient for this repo, wrong for a general platform.

## Follow-up

- Move integration checks to a temporary integration branch before touching the
  session branch.
- Add an optional LLM Integrator reviewer with a strict read-only scope.
- Consider batch queue draining and cross-entry backpressure.

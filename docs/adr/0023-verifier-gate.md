# ADR-0023: Verifier gate before Worker integration

- **Status:** Accepted
- **Date:** 2026-10-29

## Context

Workers commit their task branch and merge it into the session branch as soon as
the Worker returns. Managers are read-only coordinators, not independent
verifiers. A Worker can claim success without evidence, and a bad branch would
still enter the session branch.

## Decision

1. **Every Worker change is verified before merge.** After scope validation and
   the internal task commit, the runtime spawns a Verifier agent under the same
   Manager. The Verifier works in the Worker task worktree and sees the task
   objective, Worker summary, branch, and base commit.
2. **Verifier is a read-only role with shell.** It may read/search and run tests,
   but not edit or write files. It cannot delegate.
3. **Structured verdict.** Verifier must call `submit_verdict` with `approved` and
   a concise `summary`. A run that ends without a verdict is a verification
   failure.
4. **Gate the merge.** `approved: true` merges the task branch with `--no-ff`.
   `approved: false` marks the Worker `changes_requested`, keeps the task branch,
   and does not merge. Verification errors and budget/cancel outcomes also leave
   the branch unmerged.
5. **Evidence is first-class.** The Worker `agent.finished` event carries
   `verified`, `approved`, `verdict`, `commit`, and `merged`. Verifier events carry
   the verdict summary and the verified Worker name.
6. **Parallel Workers are verified sequentially.** After the batch settles, each
   Worker is committed, verified, and merged in task order. A later cancellation
   can leave earlier Workers already merged; the session branch is still
   reviewable from the base commit.
7. **No Integrator yet.** Merging into the session branch is still an integration
   checkpoint, not a landing to `main`. A merge queue and Integrator gate remain
   future work.

## Consequences

- A Worker can no longer merge a branch without an independent verdict.
- Verification costs tokens and can require additional shell approvals.
- The Verifier uses the same configured model as the rest of the org until
  per-role model configuration exists.
- Shell is not truly read-only; the Verifier is instructed not to edit, and its
  writes are not merged unless it approves. A future sandbox should harden this.
- Rejected work remains available on its task branch for a Manager to reassign or
  a user to inspect.
- Verification is sequential, so large parallel batches pay a verification tail.

## Alternatives considered

- **Deterministic verify command only:** cheaper, but cannot inspect intent,
  acceptance criteria, or explain failures. Could be added as an additional gate.
- **Manager approves its own Worker:** not independent; rejected.
- **Verify after merge:** the bad branch would already be in the session; rejected.
- **Verifier in a separate copy of the worktree:** unnecessary for a read-only
  review; using the task worktree keeps the diff exact.

# ADR-0070: Resumable TeamPlan execution

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

A plan run could stop after a crash, cancellation, or budget/verifier failure.
The Plan board still showed which tasks finished, but there was no way to
continue only the unfinished work without asking the Leader to produce a new
plan. Re-running everything would duplicate worktrees and merges.

## Decision

1. **Deterministic resume.** `session.plan_resume` calls
   `execute_team_plan` directly, without another Leader model call. The stored
   `TeamPlan` is the source of truth.
2. **Skip completed tasks.** Completed task IDs are derived from
   `plan_task.finished` events after the latest `team_plan.submitted` event.
   They are reported as skipped and do not emit another finished event.
3. **Retry everything else.** Pending, failed, blocked, and cancelled tasks are
   eligible to run again in dependency batches.
4. **Reattach worktrees.** Task worktrees are reused; a missing worktree is
   reattached to its existing branch. The scope base is the merge-base of the
   task branch and session branch, so checks do not mistake session advances for
   task changes.
5. **Reuse existing commits.** If a resumed task worktree is clean but its
   branch already has a commit beyond the base, that commit re-enters the
   Verifier/Integrator path instead of being treated as “no changes”.
6. **Safe states only.** Resume is allowed for `idle`, `failed`, `cancelled`,
   and `draft`; not for imported archives, `queued`, `running`, `paused`, or
   `landed`.
7. **Admission and lifecycle.** Resume uses the same run control, global session
   slot, Worker caps, budgets, and terminal finalization as a normal run. It
   emits `agent.plan_resumed` and a summary `agent.message`.
8. **No automatic restart.** Resume is explicit (`vp resume-plan`, Preview Plan
   button, RPC); daemon startup does not silently restart plans.

## Consequences

- Users can continue a partially completed plan without regenerating it.
- Completed tasks keep their original events and are not re-verified.
- Plan versions are distinguished only by the latest `team_plan.submitted`
  sequence; task IDs reused in a new plan are not assumed to be complete from
  the old one.
- Resume does not solve partial merge recovery; the Integrator’s existing
  conflict/retry path still applies.
- A completed plan cannot be resumed; the caller gets an error.

## Alternatives considered

- **Ask the Leader to resume:** LLM-dependent and could change scope.
- **Persist a separate plan-task table:** useful, but events already carry the
  needed status and avoid a new projection/replay path.
- **Rerun all tasks:** duplicates work and can merge the same branch twice.

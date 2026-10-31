# ADR-0069: Opt-in worktree retention policy

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

Terminal session worktrees accumulated until a user ran
`project.cleanup_worktrees` manually. Long-running projects could consume
significant disk. The existing cleanup was safe but easy to forget.

## Decision

1. **Per-project policy.** `WorktreeRetentionPolicy` stores `enabled`,
   `older_than_secs`, `delete_branches`, and `keep_failed`. Defaults are
   disabled, 7 days, keep branches, keep failed/cancelled worktrees.
2. **Opt-in.** The daemon never removes worktrees unless the project enables it.
   The policy is exposed through `project.worktree_policy.get/set`,
   `vp worktree-policy*`, and Manage Dash.
3. **Bounded age.** Policy age is capped at 365 days. Manual cleanup can still
   use any age.
4. **Background sweep.** The daemon runs an opt-in sweep every
   `VERLINPARK_WORKTREE_SWEEP_SECS` (default 3600; 0 disables). It calls the
   same cleanup implementation used by the manual command.
5. **Safety unchanged.** Only terminal sessions, dirty worktrees are skipped
   without `force`, branches are kept unless `delete_branches`, and
   failed/cancelled worktrees are kept unless `keep_failed` is false.
6. **Manual override.** `project.cleanup_worktrees` gained `keep_failed`; the
   CLI has `--keep-failed`.
7. **No global quota.** The sweep does not cap total bytes or count; it only
   removes worktrees older than the per-project threshold.

## Consequences

- Projects can bound worktree growth without a cron job.
- The default remains safe: disabled, keeps failed evidence and branches.
- The sweep interval is coarse and runs at daemon level, not exactly on session
  completion.
- Shell scratch directories are not covered; only session worktrees.
- A user who disables the policy keeps existing worktrees; nothing is deleted
  proactively.

## Alternatives considered

- **Always-on cleanup:** risks deleting review/failure evidence.
- **Global retention setting:** projects have different review lifetimes.
- **Delete branches by default:** loses the task commit history.
- **Per-session timers:** more state and races for little benefit.

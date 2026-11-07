# ADR-0103: Automatic checkpoint retention policy

- **Status:** Accepted
- **Date:** 2026-11-08

## Context

ADR-0102 added manual checkpoint deletion and prune. Manual cleanup still
requires the user to remember to run it, and dirty snapshot refs accumulate
across projects. Existing retention patterns for worktrees and memory are
project-scoped, opt-in, and swept by the daemon; checkpoints should follow the
same model.

## Decision

1. **Project policy.** `CheckpointRetentionPolicy` contains:
   - `enabled` (default false);
   - `older_than_secs` (default 30 days, maximum 365 days);
   - `keep_latest` (default 5, maximum 100).
   It is stored in `project_checkpoint_policies`, included in the project
   manifest snapshot with serde defaults, and restored by `vp db-rebuild`.
2. **Daemon sweep.** `VERLINPARK_CHECKPOINT_SWEEP_SECS` (default 3600; 0
   disables) controls a background sweep. For every enabled project, the
   engine lists sessions and calls the existing checkpoint prune path with
   `older_than_days = ceil(older_than_secs / 86400)` and the policy's
   `keep_latest`. Manual prune semantics are unchanged.
3. **Tombstones and refs.** The sweep uses `session.checkpoint.deleted` and
   releases snapshot refs through the existing deletion path. Existing fork
   branches are independent and remain usable.
4. **Interfaces.** RPC `project.checkpoint_policy.get/set`, CLI
   `vp checkpoint-policy` / `vp checkpoint-policy-set`, and Manage Dash
   "Session checkpoints" policy controls (enabled, older days, keep latest).
5. **Safety.** The latest N checkpoints are always protected; age and keep
   conditions combine as "beyond keep latest AND older than threshold". The
   sweep is opt-in per project and off by default.

## Consequences

- Checkpoint storage can be bounded without manual intervention.
- The event log retains checkpoint and deletion tombstones for audit; only the
  active list and refs are pruned.
- A deleted dirty snapshot commit can be garbage-collected if no fork branch
  still points at it.
- The sweep runs independently per project and does not touch fork branches,
  plans, or worktrees.
- Policy changes are manifest-backed and survive daemon restart/rebuild.

## Alternatives considered

- **Global env-only retention:** simpler, but no per-project control and no UI
  state; rejected in favor of the established project-policy pattern.
- **Delete event rows:** breaks append-only replay and mirror reconstruction.
- **Delete fork branches with checkpoints:** unsafe; forks own their refs.

# 0216 — Bounded dynamic fan-out

- Status: Accepted
- Date: 2026-11-13
- Scope: C7 explicit plan expansion

## Context

C1 gives every plan task a trusted write scope and a contract derived from the
stored plan revision. Plans are already mutable through explicit
`team_plan.revise`, `team_plan.task_edit`, and `team_plan.restore`, but a Leader
had no bounded way to turn one planned template into N independent work items
after seeing a tool or artifact result.

Unbounded model-authored plan rewriting would be unsafe: it could create
overlapping write scopes, recursively expand itself, remove completed work,
or bypass budgets. C7 therefore implements one explicit, deterministic
map/reduce operation rather than a general plan-rewrite tool.

## Decision

### Surface

- Agent tool `plan_fanout` for the Leader and RPC `plan.fanout`.
- CLI `vp plan-fanout SESSION --template T1 ... --yes`.
- The tool is classified `HostState`, so the normal approval broker asks before
  it executes; `Unattended` denies it. The RPC/CLI path requires
  `approved: true` and the CLI requires `--yes`.
- There is no scheduler, orchestration controller, retry, or LLM auto-expansion
  path. Nothing calls this method implicitly.

### Expansion semantics

- Items are a bounded JSON array (`1..=20`) taken inline or from one text
  artifact owned by the same session. Unknown item fields are ignored; known
  fields are `key`, `title`, `kind`, `profile`, `priority`, `write_scope`,
  `acceptance`, `token_budget`, and `cost_budget_usd`.
- The template task must exist and be pending, failed, blocked, cancelled, or
  interrupted. Running/claimed/queued/paused/pausing/cancelling/waiting_input,
  completed, and skipped templates are rejected. No other plan task may be
  active when the expansion is written.
- The template is removed from the new plan and replaced, in place, by the
  sorted children and one shared join task. Existing dependents are retargeted
  from the template to the join; a protected dependent is rejected.
- Child ids are deterministic (`template--key-hash`) and fit the 40-character
  task-id limit. Each child carries `parent_task_id` and `fanout_key`.
- Every child must declare a non-empty `write_scope`. Scopes are normalized with
  the runtime's existing rules, must remain inside the template scope when the
  template declared one, and are proven pairwise disjoint with the runtime's
  `scopes_overlap` check.
- Children inherit the template's department, dependencies, per-task token/USD
  budgets, and acceptance unless the item overrides a supported field. The join
  carries `fanout_join_for = template`, no write scope, and depends on exactly
  the children. It remains a normal plan task, so the existing budget,
  capacity, Verifier, and merge gates apply.
- Keys are deduplicated. Identical duplicate items collapse and are reported as
  `deduped_keys`; conflicting duplicates fail. A repeated request after a
  successful expansion is idempotent only when the key set and supplied fields
  match and writes no revision. A different key set fails closed.
- `FANOUT_*` typed errors distinguish approval, missing plan/task, invalid
  items, cap, scope overlap, recursion, protected work, and conflicting replay.

### Persistence and replay

- The result is one `team_plan.revised` event with the new plan, revision,
  reason, author, diff, and a bounded `fanout` object containing the parent,
  join, child ids/keys, deduplicated keys, and item source. The runtime snapshot
  is pinned exactly like other plan revisions.
- Children and join live in the stored plan, so replay/rebuild and bundles
  preserve them without a new projection table. `validate_team_plan` checks
  metadata shape, unique parent/key pairs, join/child dependency equality, and
  no write scope on a join.
- An optimistic `expected_revision` mismatch fails closed. There is no new
  distributed plan lock: active plan tasks are rejected and the revision guard
  catches stale writers, but operators must not race a running plan batch.
- No recursive fan-out: a child or join cannot be used as a template. No
  incremental second expansion after the first; only an idempotent replay is
  accepted.

## Security and gate preservation

This operation only constructs a plan revision. It does not approve work,
widen a write scope, allocate budget, create a worktree, call a provider, or
enter the merge queue. Every child still receives its own C1 contract derived
from its declared scope, every Worker still goes through approvals/Seatbelt/
Verifier/Integrator, and the join is a normal task under the same capacity and
budget gates. The template's scope is an upper bound, never widened.

## Limits

- The join is a normal real task, not a synthetic zero-cost aggregation node.
  It consumes a Worker slot and its inherited per-task budget.
- There is no automatic detection that a task should fan out, no LLM planner,
  no UI, and no cross-session plan sharing.
- Fan-out is not an incremental map primitive: after the first expansion, only
  an idempotent replay is accepted. A second expansion requires an explicit
  new plan revision by the operator.
- Same-host SQLite/runtime only. The active-task check and expected revision are
  optimistic, not a distributed plan lock or cross-host fencing.
- Child ids use a short deterministic hash; collisions are detected and fail
  closed rather than silently changing an existing task.

## Verification

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`: 3,912 passed, 0 failed
- New tests cover disjoint scopes, cap/dedupe/conflict, scope containment,
  recursive/protected rejection, join/child dependency retargeting, per-task
  budget inheritance, explicit approval, artifact item source, idempotent
  replay, revision conflict, and the real agent tool path through the HostState
  approval broker.

## Related

- ADR-0193 (task contracts and intent authorization)
- ADR-0196 (runtime pinning and plan revisions)
- ADR-0213/0214/0215 (production hardening rounds)
- `docs/CONTRACTED_EXECUTION_PLAN.md` C7

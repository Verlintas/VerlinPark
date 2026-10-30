# ADR-0043: Daemon-wide Worker/Labour capacity

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

ADR-0039 added a daemon-wide session semaphore and cross-session write-scope
locks. Those bound how many sessions run at once, but each session can still
start its own full Worker/Labour batch. Four admitted sessions could therefore
run four times the configured per-session Worker cap, exhausting provider rate
limits and local CPU even though the session cap was respected.

The scheduler also needs an inspectable account of how much Worker/Labour
capacity exists. A hidden global limit that only shows up as stalled agents is
not acceptable.

## Decision

1. **One controller per daemon.** `AgentService` owns an `Arc<CapacityController>`
   with one semaphore for Workers and one for Labour. Reconfiguring the provider
   reuses the same controller, so changing the model cannot reset a live limit.
2. **Limits are user-configured.** `VERLINPARK_GLOBAL_WORKERS` defaults to 12
   (clamped 1–100) and `VERLINPARK_GLOBAL_LABOUR` defaults to 24 (clamped
   1–100). Per-session caps and plan batch selection are unchanged; the global
   semaphore is an additional ceiling.
3. **Worker and Labour only.** Leader, Manager, Verifier, conflict resolver, and
   other internal runs are not capacity-controlled here. They are bounded by the
   session semaphore and their own execution paths.
4. **Acquire before running.** `run_loop` acquires the role permit before the
   cross-session write-scope lock and holds it for the entire run, including
   tool execution and nested delegation. A cancelled wait returns
   `LoopOutcome::Cancelled` without creating a fake completion.
5. **Inspectable.** `agent.info` reports `CapacityInfo` with the configured limit
   and currently available permits for both roles. Settings shows the snapshot
   with a refresh button.
6. **No persistence or fairness policy.** Capacity is process-local, first-come
   first-served through Tokio's semaphore. It is released on daemon restart and
   is not a global Task-DAG scheduler, persistent lease, priority queue, or
   per-project quota.

## Consequences

- Multiple sessions can no longer multiply Worker/Labour concurrency without
  bound.
- A Worker can hold a capacity permit while waiting on a cross-session scope
  lock; the scope owner is itself capacity-admitted, so this does not create a
  cycle.
- Reconfiguration does not reset capacity accounting.
- Waiting agents remain `running` rather than `queued`; only session-level
  admission is surfaced as `queued` today. This is a known observability gap.
- The defaults are higher than the per-session defaults (12/24 vs 6/12) so two
  normal sessions can still run at full width.

## Alternatives considered

- **Global limit inside `AgentConfig`:** reset on every provider reconfiguration
  and unavailable while the service is unconfigured.
- **Count only sessions:** does not bound the 4 × 10 Workers case.
- **Persistent scheduler with per-project quotas:** more capable, but needs a
  durable queue and fairness policy that this slice does not implement.
- **Block the session until all Workers fit:** reduces utilization and makes the
  queue harder to reason about; per-run admission is enough.

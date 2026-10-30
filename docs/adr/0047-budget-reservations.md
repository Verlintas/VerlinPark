# ADR-0047: In-flight task and department budget reservations

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

ADR-0045 enforced task and department limits from the persisted usage ledger
before and after each provider call. That allowed a correctness gap: two
Workers in the same department could both read the same remaining budget and
each start a maximal call. The department could then overshoot by one call per
concurrent task, even though every individual check passed.

The session budget already solves this with pre-flight reservations. The plan
budget tree needs the same property.

## Decision

1. **Daemon-wide registry.** `Engine` owns an `Arc<BudgetRegistry>`. Keys are
   `(session_id, scope, id)` where scope is `task` or `department`.
2. **Ledger plus reservations.** The registry combines the persisted ledger
   total with short-lived in-flight reservations. Observed spend is monotonic
   (`max` of the values seen), so a stale caller cannot lower it.
3. **One atomic grant for both scopes.** A Worker requests its task and
   department limits together. The registry computes the tightest output-token
   budget that fits all requested scopes, or rejects the call before mutating
   reservations.
4. **Token and cost.** Reservations cover estimated input + granted output
   tokens and the USD cost at the configured per-role prices. If a cost limit
   is tighter than the token limit, the output budget is reduced to the
   affordable amount; if input alone cannot fit, the call is rejected.
5. **RAII release.** `BudgetLease` releases reservations on drop, including
   provider errors, cancellation, and early returns. The lease is held through
   the provider call and usage recording, then released at the end of the turn.
6. **Ordering.** The runtime computes the session token allowance first, grants
   the plan reservation, reserves session tokens for the granted amount, and
   only then calls the provider. A session reservation failure drops the plan
   lease automatically.
7. **Session cleanup.** A finalized session clears its registry entries. The
   registry is process-local; a daemon restart drops in-flight leases along
   with the dead provider calls. The ledger remains the durable record.
8. **Post-call check stays.** The ledger is still checked after usage is
   committed. Reservations reduce overshoot; they do not replace the post-call
   limit check.

## Consequences

- Concurrent tasks in one department can no longer each pass the same remaining
  budget check.
- The granted output budget may be lower than the configured maximum when other
  calls are in flight. This is intentional backpressure.
- Reservations are not persisted; a crash can lose them, but the provider call
  is also gone.
- Input-token estimates are approximate. A provider that reports more input
  tokens than estimated can still consume slightly more than reserved; the
  post-call ledger check catches it.
- Session budget reservations and plan reservations are separate locks. They
  are acquired in a single order and do not form a cycle.

## Alternatives considered

- **Ledger-only checks:** the original one-call-per-task overshoot.
- **A single global budget semaphore:** cannot express independent task and
  department limits.
- **Persisted reservations:** unnecessary for calls that die with the process;
  the ledger is the durable authority.
- **Reject on any contention:** wastes capacity; reducing the output budget
  keeps a call running at a safe size.

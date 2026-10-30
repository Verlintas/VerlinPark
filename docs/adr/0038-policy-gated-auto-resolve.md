# ADR-0038: Policy-gated automatic conflict resolution

- **Status:** Accepted
- **Date:** 2026-10-30

## Context

ADR-0037 added an explicit resolver action for conflict entries. It works, but a
conflict still waits for a user click. Teams that accept the token cost want the
queue to heal itself after a Worker run. Running the resolver by default would
spend tokens and auto-approve tools without consent, so automation must be
opt-in, capped, and auditable.

## Decision

1. **Per-project integration policy.** Add `IntegrationPolicy`:
   - `auto_resolve`: default `false`;
   - `max_auto_attempts`: default `1`, allowed `0..=5`;
   - `max_resolve_tokens`: optional per-resolution cap for resolver + Verifier,
     `1000..=10_000_000`.
2. **Opt-in only.** Automatic resolution runs only when the project enables it.
   Explicit `merge_queue.resolve` remains available regardless of the policy.
3. **Attempt accounting.** A persisted `merge_auto_attempts` table records
   attempts per queue entry. The watcher bumps the counter before each automatic
   resolution; once it reaches `max_auto_attempts`, no further automatic attempt
   is made. Manual retries do not reset the counter silently.
4. **Token cap.** The automatic resolver builds a session budget whose limit is
   the smaller of the session limit and `current_usage + max_resolve_tokens`.
   Resolver and Verifier share that budget. A capped run marks the entry
   `conflict` instead of merging.
5. **Trigger.** The daemon starts one watcher per process. It scans existing
   sessions at startup (and after provider configuration) and reacts to
   `session.updated` with status `idle`. Conflicts are resolved serially per
   session; a session that is already running is skipped. Setting a policy with
   `auto_resolve=true` immediately scans that project.
6. **Reuse the ADR-0037 path.** Automatic resolution uses the same rebase,
   marker check, fresh Verifier, queue commit/base update, and deterministic
   Integrator. It emits `merge_queue.auto_resolve_started` /
   `merge_queue.auto_resolve_finished` in addition to the resolver events.
7. **Failures are bounded.** Any failure increments the attempt count. A failed
   attempt leaves the entry `conflict`, so the user can still inspect it or use
   explicit Auto-resolve after fixing the cause.
8. **Control surface.** RPC `project.integration_policy.get/set`; CLI
   `vp integration-policy` / `vp integration-policy-set`; Manage Dash exposes a
   card with the toggle, attempt cap, token cap, save, and reset. Defaults are
   safe and visible.

## Consequences

- A project can opt into queue self-healing without changing global behavior.
- Token spend is bounded by configurable attempts and an optional per-resolution
  cap, in addition to the session budget.
- Auto-resolve still bypasses per-tool approvals for the resolver/verifier; the
  shell sandbox remains, but the user must explicitly enable the policy.
- The watcher scans on idle events, so a conflict created while the session is
  still running waits for the run to finish.
- Attempt counters and policies are project/queue metadata, not events; policy
  changes and auto attempts are visible in logs/events but are not exported in
  session bundles.
- Auto-resolution can still fail on binary/semantic conflicts; it is not a
  guarantee that every conflict merges.

## Alternatives considered

- **Default-on auto-resolve:** rejected; spends tokens and bypasses approvals
  without consent.
- **No caps, rely on session budget:** one conflict could consume the whole
  session budget; rejected.
- **Background daemon without a policy:** rejected; implicit automation is hard
  to audit.
- **Only explicit Auto-resolve:** safe but leaves queue self-healing on the
  table; kept as a manual fallback.

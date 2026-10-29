# ADR-0027: Manager roundtable deliberation

- **Status:** Accepted
- **Date:** 2026-10-29

## Context

Managers could split work and delegate, but they could not exchange positions with
each other. In auto-dispatch they were deterministic department nodes with no
voice at all. The user asked for at least the Manager layer to discuss and
negotiate before work starts.

A free-form group chat would be noisy, costly, and hard to audit. The runtime
already has a validated TeamPlan and a shared event log.

## Decision

1. **Leader convenes a bounded roundtable.** `deliberate_team_plan` is a
   Leader-only tool. It accepts 1–3 rounds (default 2) and runs after
   `submit_team_plan`, before `run_team_plan`.
2. **All planned Managers participate.** The runtime creates/reuses one Manager
   per department (max 5), with its title/profile. Managers run concurrently in
   each round.
3. **Peer positions are shared between rounds.** Round 1 gives each Manager the
   plan; every later round includes all positions from the previous round. Each
   Manager returns an updated position/decision.
4. **Deliberation is read-only.** Managers cannot delegate, edit, or run shell in
   this mode. They can read/search and, for research profiles, fetch public URLs.
   This is enforced by the tool policy and `allow_delegation = false`.
5. **Positions are persisted.** Every position emits a `manager.roundtable` event
   with round, manager id/title/profile, and summary. The Leader receives a
   combined report as the tool result.
6. **No free chat.** There is no mailbox, peer-to-peer wakeup, or hidden
   conversation. The roundtable is a structured, bounded exchange; tasks are not
   executed by this tool.
7. **Then execute.** `run_team_plan` reuses the same Managers and executes the
   DAG with Workers, Verifier gates, and integration.

## Consequences

- Managers can surface dependencies, risks, and handoff agreements before
  Workers start, which improves plan quality and reduces isolated assumptions.
- Deliberation costs `managers × rounds` LLM calls. Defaults (max 5 managers,
  2 rounds) bound the cost, and the session token budget is the hard stop.
- The discussion is a round-based board, not real-time chat. A Manager cannot ask
  a spontaneous follow-up outside the next round.
- Managers remain deterministic department owners during auto-dispatch; the
  roundtable is their LLM voice, not a persistent coordinator loop.
- Worker/Manager and Worker/Worker peer messaging is still not implemented.

## Alternatives considered

- **Free-form group chat:** unbounded turns, hard to terminate, poor auditability.
- **No deliberation:** fastest, but loses cross-department risk/handoff review.
- **One LLM simulating all Managers:** cheaper but not genuinely peer-informed.
- **Persistent Manager LLM loops:** more autonomous but much more expensive; can
  be an option later.

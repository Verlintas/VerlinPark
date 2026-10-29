# ADR-0025: Automatic TeamPlan dispatch with fewer hard caps

- **Status:** Accepted
- **Date:** 2026-10-29

## Context

The Leader could submit a validated TeamPlan, but the plan was only informational:
the Leader still had to delegate every Manager manually. That is unnecessarily
slow for a self-hosted platform where the user controls budgets and wants high
throughput.

Hard-coded small caps (5 Managers, 10 Workers, 50 tasks) were also limiting
legitimate research or large software tasks. The session token budget is a better
primary stop than a tiny fixed org size.

## Decision

1. **`run_team_plan` executes the stored plan.** The Leader calls it after
   `submit_team_plan`. Tasks are scheduled in dependency batches: all ready tasks
   in a batch run concurrently, up to the configured Worker concurrency, then
   each result is verified and integrated before the next batch.
2. **Managers are deterministic department owners for plan execution.** The
   runtime creates one Manager per department with the planned title/profile and
   attributes Workers to it. Managers do not run a separate LLM loop in this
   auto-dispatch path; the Leader can still delegate manually when it wants an
   LLM coordinator.
3. **Task workers are real agents.** Each task gets a Worker in its own task
   worktree, profile-derived tools, write scope, acceptance text, verifier gate,
   and event trail. Research tasks may produce no file changes and complete with a
   summary; coding tasks commit and merge only after Verifier approval.
4. **Failures block dependents.** A failed or rejected task is marked failed and
   its dependent tasks are marked blocked; independent tasks continue. Cancel and
   budget exhaustion stop the plan and mark the remaining tasks.
5. **Caps are configurable and much higher.** Defaults are 8 Managers,
   32 Workers, 128 Labour. The app/API can request up to 32 / 128 / 512; a plan
   may contain up to 500 tasks. The per-session token budget remains the primary
   hard stop. Verifier cap is raised to 1000 so multi-task plans do not fail on a
   small verifier counter.
6. **No hidden scheduling.** Auto-dispatch is an explicit Leader tool call; the
   plan and task events are persisted and visible. The Leader can still override
   by delegating manually.

## Consequences

- A single Leader can execute a multi-department plan without manually calling
  `delegate` for every Manager.
- Throughput and cost are now primarily governed by the user's Worker concurrency
  and token budget, not tiny fixed org caps.
- Managers in auto-dispatch are organizational/attribution nodes, not autonomous
  coordinators. This is a deliberate efficiency tradeoff and must be represented
  accurately in the UI.
- Verification is sequential per completed Worker, so a wide batch has a
  verification tail.
- There is still no global scheduler across sessions, merge queue, or Integrator
  gate. Cancellation after earlier merges does not roll them back.
- Resetting the primary caps upward can produce very large token spend if the
  session budget is set high; budgets must be configured deliberately.

## Alternatives considered

- **Keep manual delegation only:** too slow and defeats the plan feature.
- **Make every Manager run an LLM coordination loop:** more autonomous but much
  more expensive and slower; can be added as an option later.
- **Keep the 5/10/50 caps:** rejected as unnecessary for a self-hosted platform;
  the user explicitly prioritizes efficiency over vendor-style guardrails.
- **Unlimited concurrency:** rejected; Worker concurrency is still a configured
  knob, and the token budget is the hard stop.

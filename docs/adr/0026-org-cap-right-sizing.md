# ADR-0026: Right-size default org caps and spawn Managers lazily

- **Status:** Accepted
- **Date:** 2026-10-29

## Context

ADR-0025 removed small limits by raising defaults to 8 Managers / 32 Workers /
128 Labour, with ceilings of 32 / 128 / 512 and plans of 500 tasks. That was
overcorrection: it looked like the runtime would summon a corporation onto a
laptop. The goal was fewer arbitrary restrictions, not a giant standing org.

The real cost/size controls should be:

- a small default team,
- lazy creation of Managers as work arrives,
- a bounded Worker concurrency batch,
- the user's session token budget as the primary hard stop.

## Decision

1. **Small defaults.** The app preset defaults to **4 Managers / 6 Workers /
   12 Labour**. Runtime caps are **5 / 10 / 40** by default and are enforced by
   `agent.configure` unless the user overrides them through the headless env
   configuration.
2. **Lazy Managers.** Auto-dispatch no longer creates every department Manager
   up front. A Manager is created (or reused) only when a task in that department
   becomes ready. Departments with no ready work remain plan entries, not agents.
3. **Bounded batches.** Auto-dispatch runs ready tasks in batches limited by the
   configured Worker concurrency (default 6, max 10). A 100-task plan does not
   create 100 live Workers; it drains the DAG in batches.
4. **Plan and delegation ceilings are modest.** TeamPlan supports at most 100
   tasks; `delegate_many` accepts at most 10 tasks; Verifier cap is 100.
5. **Budget remains the hard stop.** The session token budget still stops the
   whole run. Org caps are a guard against accidental explosions, not the primary
   cost policy.

## Consequences

- The default visual is a small team, not an agent army.
- Large plans are still possible, but they drain slowly and lazily; only active
  departments and the current Worker batch materialize as agents.
- Users who really want a larger org can raise the headless env caps or change
  the preset; the app's visible ranges remain conservative.
- Auto-dispatch Managers are still deterministic department owners in this path.
- The next capacity feature should be lazy auto-expansion driven by ready-task
  pressure and remaining budget, not pre-created tiers.

## Alternatives considered

- **Keep 32/128/512 defaults:** rejected as operationally absurd and confusing.
- **Make everything unlimited:** rejected; a runaway plan would create thousands
  of agents before the budget check catches it.
- **Create all Managers up front:** rejected; it produces idle agents and the
  "corporation on my laptop" effect.
- **Remove the presets entirely:** loses a useful guardrail for new users.

# ADR-0024: Leader-defined TeamPlan and agent profiles

- **Status:** Accepted
- **Date:** 2026-10-29

## Context

The org was fixed to four tiers and a small set of hard-coded roles. Managers
required a title, but the Leader could not express a complete department/task
plan before delegating. The runtime also assumed coding tools, which does not fit
research, operations, or other knowledge work.

VerlinPark needs a domain-neutral organization model without letting the model
invent unlimited agents, permissions, or budgets.

## Decision

1. **Leader submits a TeamPlan.** `submit_team_plan` is a Leader-only tool. The
   plan contains a goal, 1–5 departments, and up to 50 tasks. Each department has
   a name, a unique Manager title, a profile, and an objective. Tasks have an id,
   department, title, kind, dependencies, write scope, and acceptance criteria.
2. **Runtime validates the plan.** It checks caps, unique names/titles, known
   profiles, department references, unique task ids, acceptance items, and DAG
   acyclicity. Invalid plans are rejected as tool errors; they are not partially
   stored.
3. **Plan is persisted and visible.** The plan is stored on the session, emitted
   as `team_plan.submitted`, and shown in Collab as the TEAM PLAN card. The CLI
   exposes `vp team-plan <session>`.
4. **Plan titles bind Manager delegation.** Once a plan exists, delegating a
   Manager requires a title matching a planned department. The department's
   profile becomes the Manager profile; the Manager cannot silently run a
   different domain.
5. **Structural role and domain profile are separate.** Structural roles remain
   Leader / Manager / Worker / Verifier / Labour. Profiles are coding, research,
   verification, and generic. Profiles choose the tool subset and prompt context.
   Coding gets edit/write/shell; research gets read/search/fetch plus report
   writes; verification gets read/shell; generic is read-only.
6. **Research fetch is a real tool.** `fetch_url` fetches public HTTP(S) pages,
   blocks localhost/private addresses, does not follow redirects, enforces a
   timeout, and truncates output. Profiles gate access.
7. **Tasks are not auto-scheduled yet.** The plan defines the organization and
   task DAG, but the Leader still delegates Managers explicitly. Automatic
   dispatch of plan tasks is the next scheduler slice.

## Consequences

- The Leader can create coding, research, mixed, or custom departments within
  deterministic caps.
- Research Managers can fetch sources and read/search but cannot write code;
  research Workers can read/fetch and write report artifacts within scope.
- Plan tasks are currently informational; a plan can be submitted without being
  fully executed. The UI/CLI make that visible, and delegation is title-checked.
- `fetch_url` does not implement web search, pagination, or redirect handling.
  Search/MCP/browser research tools remain future work.
- The same model is still used for every profile; per-role model policy is not
  implemented.
- The plan profile does not yet drive Verifier strategy automatically; the
  Verifier remains code/test-oriented and needs research-specific verdict
  prompts next.

## Alternatives considered

- **Let the Leader spawn arbitrary agents directly:** violates caps and makes
  org state unverifiable.
- **Keep only fixed department titles:** blocks research/ops teams.
- **Build a full task scheduler in the same slice:** too risky; plan validation +
  profile-aware delegation gives a reviewable foundation first.
- **Use an external search API immediately:** requires credentials and vendor
  decisions; direct URL fetch is a smaller, deterministic first step.

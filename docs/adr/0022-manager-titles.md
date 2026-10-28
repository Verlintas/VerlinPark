# ADR-0022: Managers carry an explicit department role

- **Status:** Accepted
- **Date:** 2026-10-28

## Context

Managers were just named `Manager 1`, `Manager 2`, and so on. The Leader could
delegate a department objective, but the runtime and UI had no explicit record of
what department that Manager owned. That makes the org chart decorative and makes
it hard to reason about which Manager should receive a task.

## Decision

1. `delegate` gains an optional `title` field. When the target role is `manager`,
   `title` is required; otherwise it is ignored.
2. The title is a short department role such as `Discovery`, `Implementation`, or
   `Quality`. The Leader prompt explicitly asks for distinct titles for each
   Manager.
3. The title is stored on the agent record (`agents.title`), included in agent
   events (`agent_title`), and displayed in Collab and approval cards.
4. If no explicit name is provided, the Manager's display name defaults to its
   title, so the roster reads `Discovery` rather than `Manager 1`.
5. The Manager system prompt identifies the agent as the `<Title> Manager` and
   repeats the delegated objective.

## Consequences

- The Leader now has to make its org structure explicit instead of relying on
  ordinal names.
- Existing sessions and old agent rows have no title and continue to display
  their names.
- Duplicate titles are not deterministically rejected yet; the prompt asks for
  distinct roles. A future task/plan validator should enforce one Manager per
  department and link titles to plan departments.
- Titles are free-form within a length limit, so users can use custom department
  names, not only the default three.
- The title is metadata, not a permission boundary; Manager write restrictions
  still come from the role.

## Alternatives considered

- **Keep ordinal names only:** unreadable org chart and ambiguous delegation.
- **Hard-code the three default departments:** too restrictive for custom teams.
- **Use a separate manager profile object now:** premature; a single validated
  title covers the current delegation slice.

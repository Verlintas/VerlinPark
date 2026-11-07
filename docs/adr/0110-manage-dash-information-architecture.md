# ADR-0110: Manage Dash information architecture

- **Status:** Accepted
- **Date:** 2026-11-08

## Context

Manage Dash had grown into a single 760×600 scroll containing roughly twenty
cards across unrelated concerns: approvals, questions, shell policy,
integration, budgets, usage, memory, scheduler, doctor, worktree cleanup,
checkpoints, forks, artifacts, mailbox, and project forest. Finding a card
required scrolling past unrelated policy forms, and project/session context was
only available by closing the dashboard and changing selection in the Stage.

## Decision

1. **Sidebar navigation.** Manage Dash uses a persistent section sidebar:
   overview, inbox, team/tasks, usage/budget, git/forks, checkpoints,
   storage/memory, policies, and diagnostics. Sections carry live badges for
   inbox, running tasks, fork roots, and checkpoints.
2. **Larger workspace.** The sheet grows to 1080×740 and uses a content pane
   rather than one long card column.
3. **Self-contained context.** Project and session pickers live in the sidebar,
   so users can change scope without closing the dashboard.
4. **Overview first.** Overview shows workspace/inbox/scheduler/checkpoint/fork
   metrics, pending actions with jump buttons, runtime/capacity state, and the
   most recent session events. The long policy forms are not on the overview.
5. **Section filtering and persistence.** A sidebar filter finds sections by
   title/detail; the last section is persisted in AppStorage.
6. **Card grouping and file split.** Cards are grouped by concern:
   - `DashboardOverview.swift`: overview metrics, pending actions, runtime,
     recent activity, project-required empty state.
   - `DashboardTeamCards.swift`: agents, scheduler, usage, approvals,
     questions, memory.
   - `DashboardGitCards.swift`: forks, project forest, compare, tree,
     checkpoint cards.
   - `DashboardPolicyCards.swift`: shell/integration/budget/storage policies,
     doctor, worktree cleanup.
   `DashboardView.swift` owns navigation and refresh orchestration only.
7. **Agent roster card.** Team/tasks now includes a session agent roster with
   role, profile, model, and status, instead of only an aggregate count.

## Consequences

- Related controls are adjacent; users no longer scroll past policy forms to
  reach approvals or fork state.
- Manage Dash can change project/session without closing.
- Overview remains useful with no project selected; project-scoped sections
  show an explicit project-required state.
- Section refreshes are scoped to the visible section, reducing unnecessary
  RPC traffic.
- The view code is split into focused files; `DashboardView.swift` is now small
  enough to review as navigation logic.
- The sheet is wider, which is acceptable because Manage Dash is a dedicated
  management surface, not the primary chat stage.

## Alternatives considered

- **Expand the existing single scroll:** more width/height does not solve
  discoverability.
- **Move everything to Settings:** settings should hold configuration, not
  live approvals, tasks, health, and fork state.
- **Use a separate window per section:** unnecessary window management for a
  small local app.
- **Tab bar instead of sidebar:** tabs do not scale to nine sections with
  badges and context pickers.

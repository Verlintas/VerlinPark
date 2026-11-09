# ADR 0120: Dynamic team graph for Collab

- Status: Accepted
- Date: 2026-11-09
- Area: UI / orchestration

## Context

Collab previously rendered the team as a flat, indented list. It showed who
existed and whether they were running, but not the shape of the team, the task
handoffs, or where work was blocked. The list also hid the relationship between
a Manager, its Workers/Verifiers/Labour, and the plan tasks that did not have an
agent yet.

The runtime already has the facts needed for a graph: `AgentRecord.parent_id`,
plan departments/tasks, the persistent task queue, the selected session's event
window, and the merge queue. No second scheduler or model-driven layout is
needed.

## Decision

Collab is a deterministic, live tree graph.

1. **Tree projection in Swift.** `CollabGraphBuilder` derives a pure projection
   from agents, `TeamPlan`, scheduled tasks, events, and the merge queue. It does
   not write state or call a model.
2. **Left-to-right org chart.** Depth is horizontal (Leader → Manager → Worker →
   Verifier/Labour); siblings stack vertically. This fits the narrow Studio
   panel better than a wide top-down tree.
3. **Planned work is explicit.** Departments without a live Manager and plan
   tasks without a live Worker render as dashed `PLANNED` nodes. Completed or
   skipped ghost tasks are hidden. The `Plan` toggle can remove all planned
   nodes.
4. **Live activity.** `tool.started`, task lifecycle, capacity, permission, and
   `coordination.note` events provide the node activity line. Node borders,
   status dots, and edges use the projected status; running edges are accented.
5. **Pan/zoom/fit.** The panel auto-fits to a readable minimum (55%) and uses
   top-leading anchoring so the Leader is always visible; users can pan, zoom,
   or open a wide `Team graph` sheet. The sheet fits the whole tree and keeps
   the same node/inspector components.
6. **Task ownership in the projection.** `AgentRecord` gains optional
   `task_id` and `department` fields, persisted as additive SQLite columns and
   emitted in `agent.updated`. Plan Workers, Verifiers, Labour, Managers, and
   conflict resolvers set these fields. Older agents continue to decode.
7. **Coordination is visible.** `coordinate` records typed `coordination.note`
   events (note/blocker/unblock/handoff/escalate/decision); `team_status`
   returns the live board. Coordination notes appear as node activity and in the
   team-status tool. This does not add a hidden model router: agents choose when
   to call the tools.
8. **Read-only surface.** Collab remains an orchestration monitor, not a second
   plan editor. The inspector exposes task/activity/scope information; task
   mutation stays in Manage Dash and the existing task APIs.
9. **Stale leader safety.** If multiple Leader rows exist after a crash, the
   healthiest Leader becomes the root and stale duplicate Leaders are omitted
   from the graph. The runtime invariant remains one Leader per session.

## Layout and interaction constraints

- Node sizes are role-specific; subtree width/height is measured recursively and
  parents are centered over their children.
- Edge paths are cubic Bezier curves with the control point at the horizontal
  midpoint.
- The panel's default zoom floor is 0.55 so text is not reduced below legibility;
  `Fit tree` deliberately allows a smaller overview zoom.
- Reduce Motion disables graph/sheet transitions and pulses.
- The graph is capped by the number of agents/tasks the runtime already caps
  (Workers 10, Labour 40, plan tasks 100); no unbounded canvas state is stored.

## Consequences

- Collab now communicates hierarchy, ownership, readiness, blockers, and live
  activity at a glance.
- The narrow Studio panel still requires panning for large teams; the expanded
  sheet is the overview surface.
- `AgentRecord` gains two optional fields, an additive schema migration, and a
  new `set_agent_task` event path. Replay/bundle decoding remains backward
  compatible.
- The graph is only as fresh as the event stream and the per-session task-queue
  snapshot. Both are refreshed on plan/task events; stale loaders are guarded by
  selection generation.
- General Manager/Labour auto-expansion, durable cross-process leases, and a
  computer lease remain separate future work.

## Verification

- Swift tests build a multi-department tree, verify task ownership/activity,
  dependency readiness, planned nodes, stale-leader selection, and render the
  graph to a PNG.
- Rust tests cover agent task assignment persistence and the `coordinate` /
  `team_status` tool path.

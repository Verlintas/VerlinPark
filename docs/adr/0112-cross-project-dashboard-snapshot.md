# ADR-0112: Cross-project Manage Dash snapshot

- **Status:** Accepted
- **Date:** 2026-11-08

## Context

Manage Dash assembled its overview from many independent reads: session lists,
global approvals, questions, task queues, checkpoint cards, fork forests, and
merge queues. Some values were scoped to the selected session, so the overview
could claim there were no pending approvals while another project was waiting.
There was no single read-only view of cross-project health or a consistent
source for sidebar badges and "needs attention" alerts.

## Decision

1. **One read-only RPC.** Add `dashboard.snapshot` with an optional
   `project_id`, exposed as `vp dashboard`. It returns a summary plus a bounded
   alert list. It never mutates state and never starts agents, Doctor, fork
   audit/repair, provider probes, or integration.
2. **Summary fields.** The summary aggregates project/session counts, session
   status counts, pending approvals/questions, running/queued sessions, task
   and lease counts, checkpoint/dirty counts, and fork root/fork counts. It
   also includes a per-project breakdown (sessions, running/queued/failed,
   pending approvals/questions, tasks/running tasks, checkpoints/dirty, fork
   roots/forks), sorted by activity then name. The daemon attaches the current
   `agent.info` capacity snapshot.
3. **Alerts.** Alerts cover pending approvals/questions, failed and queued
   sessions, failed/blocked/interrupted plan tasks, and conflict/failed/
   interrupted merge queue entries. Each alert carries kind, severity, title,
   detail, optional project/session IDs, and a UI section hint. Critical alerts
   sort before warnings and informational alerts; the response is capped at 100
   with an `alerts_truncated` flag.
4. **Project filter.** A project-scoped snapshot includes only sessions,
   approvals, questions, tasks, checkpoints, and merge entries belonging to
   that project. A missing project is an error rather than an empty snapshot.
5. **UI consumption.** Manage Dash loads the snapshot when it opens, after a
   project change, and on overview/team refresh. Overview metrics, pending
   actions, badges, and the "Needs attention" card read from it, with local
   card state as a fallback. When no project is selected, a compact Projects
   card shows the most active projects with a one-click Open action. Alerts can
   jump to the relevant section and select the alert's project/session.
6. **Explicit refresh.** The snapshot is not recomputed on every event; the
   daemon remains the source of truth and the dashboard offers a scoped Refresh
   action. Fork audit issues still require an explicit audit because they walk
   Git refs/commits.

## Consequences

- Manage Dash has one cross-project source for counts and alerts.
- Opening the overview performs bounded local SQLite reads and checkpoint event
  reads; it does not call the network or run Git ref audits.
- The snapshot is a point-in-time projection, not a subscription. A running
  session can change immediately after it is fetched.
- Alerts are derived and not persisted; acknowledgements and notification
  history remain owned by the existing notification center.
- The snapshot is intentionally conservative: no orphan-task dispatcher, no
  cost estimation without the usage ledger, and no implicit audit.

## Alternatives considered

- **Keep composing separate RPCs in Swift:** duplicates filtering rules and
  makes badges inconsistent with the overview.
- **Persist a dashboard projection:** adds invalidation complexity for a
  read-only management view.
- **Run Doctor/audit inside the snapshot:** turns a fast overview refresh into
  a potentially expensive Git/filesystem scan.

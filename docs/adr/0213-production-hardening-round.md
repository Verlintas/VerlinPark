# 0213 — Production hardening round for contracted execution and project identities

- Status: Accepted
- Date: 2026-11-13
- Scope: C1–C6, S1–S8 production-review fixes

## Context

Four independent, read-only production reviews covered C1–C3, C4–C6, S1–S4,
and S5–S8. The reviews found real implementation defects that green tests did
not expose: a cascading projection repair, lease expiry during long runs,
workspace bases unrelated to the session base, a permanently blocked land, a
stale-effect sweeper that killed live tools, an effect identity shared across
agents, MCP pin bypasses outside contracts, sibling tool calls lost on a gate
wait, a migration snapshot reverted by repair, and S8 approval/isolation
defects. This ADR records the hardening changes and the gaps that remain.

## Fixes

### Identity and repair (S1–S2)

- `agent.identities.replay {repair:true}` no longer deletes the whole identity
  projection. Expected rows are upserted, only local rows absent from the
  event stream are deleted, imported rows are preserved as evidence, and any
  touched identity with an active assignment or workspace fails the transaction
  with `IDENTITY_REPAIR_LIVE_WORK`.
- `INSERT OR REPLACE` on `agent_identities` was replaced with an `ON CONFLICT`
  upsert so repair/import cannot cascade-delete assignments, inbox rows,
  workspaces, bindings, or identity memory.
- S1 retirement refuses live assignments/workspaces; pause remains cooperative
  and is reflected by prompt-identity status filtering.
- Assignment expiry skips sessions that are `running`, `queued`, `paused`, or
  `waiting_input`. A long tool call cannot lose its identity through the 30s
  sweep. Prompt selection additionally requires a live (`lease_active`) lease.
- Idempotent re-assign extends the live lease instead of returning an unchanged
  row, and emits `agent.assignment.renewed`. It still reports `created=false`.

### Run identity and leases (S3–S4)

- `AgentSpec` carries a resolved project `identity_id`. The run loop resolves
  Leader/Manager/Worker identity once, persists it on the `agents` row with an
  `agent.identity_binding` event, and Labour inherits the Worker identity that
  owns its workspace. S6 performance attribution can now use the explicit
  binding rather than a role/department heuristic.
- The run loop heartbeats both the assignment and the session Worker workspace
  lease at a bounded cadence. Lease loss is an event-backed warning
  (`agent.lease_heartbeat` / `agent.lease_heartbeat_failed`), not a silent mode
  switch.
- Workspace lease renewal rotates the binding and workspace token together and
  refuses to revive an expired lease or steal another session's lease. Release
  is owner-fenced: a stale session cannot clear a workspace lease that has been
  reassigned.
- Workspace time-expiry skips bindings attached to a live session.
- A Worker run that cannot acquire its component workspace emits
  `agent.workspace.fallback` before using the legacy task worktree.

### Workspace integration (S4)

- Component branches are based on `session.base_commit`, not the project
  checkout's current `HEAD`. Existing component/session branches must share
  history with the session base or the attach fails with
  `WORKSPACE_BASE_MISMATCH`.
- `workspace.land` applies only the commits the session branch accepted.
  Rejected commits remain on the workspace branch as evidence and are reported
  as `skipped_commits`; they no longer block landing accepted work forever.
- Every rollback reads HEAD back and reports
  `WORKSPACE_LAND_ROLLBACK_FAILED` if the exact pre-land commit cannot be
  proven.

### Effects and contracts (C1–C2)

- The stale-intent sweep only considers sessions that are not live. The grace
  period is a post-restart recovery bound, not a wall-clock timeout on an
  in-flight tool. A live completion may replace a sweep-marked `unknown` with
  the real probe result; genuine unknown receipts remain reconcile-only.
- Effect identity is scoped by agent id and the current worktree head, so a
  Verifier cannot replay a Worker's `run_project_task` output and a write after
  a reset or new commit executes again. The run-journal fingerprint guard still
  handles duplicate writes within one unchanged context.
- HTTP effect identity includes a query hash (the persisted target remains
  query-free), and a 3xx mutation without following the redirect settles
  `unknown`.
- `session.land` now checks task-scoped unresolved effects as well as
  session-scoped receipts. Reconciliation refuses imported/landed archives.
- Out-of-scope shell writes escalate even when `enforce_shell=false`; other
  shell hints remain advisory in that mode.
- An exact egress allowlist entry matches exactly that host. Subdomains require
  `*.suffix`.
- MCP generic `call` enforces `server.enabled` and pin callability independent
  of contract mode. A pin-sync error falls back to stored pins and never
  reclassifies every discovered tool as TOFU-trusted.

### Gates (C3)

- A gate wait synthesizes a bounded tool result for every skipped sibling call,
  so the resumed conversation has one result per assistant tool call.
- Gate labels and notes are single-line `<external>…</external>` data in the
  continuation prompt, not raw instructions.
- Gate list truncation compares the filtered state count, not the all-state
  total. Landed sessions can cancel an open gate; imported archives remain
  read-only.

### Runtime pinning (C4)

- `plan.migrated` is a first-class snapshot source for repair, bundle import,
  and rebuild; a routine repair no longer reverts an approved migration.
- `plan.migrate` repins interrupted/failed runs of that session in the same
  transaction, so an approved revision change can be resumed.
- `RuntimeSnapshot.schema_version` participates in compatibility comparison.

### Reliability and diagnosis (C5–C6)

- Diagnosis treats `run_project_task` as test evidence only when its structured
  kind/command is test-like; build/lint runs cannot create false consensus.
- A task started without a terminal event is `interrupted` when the session is
  not live.
- False consensus uses the latest failed merge and disappears when a later
  merge succeeded.
- Verbose structured tool results preserve `command`, `kind`, `ok`, and
  `exit_code` in the capped summary so test evidence survives.
- Saved reliability reports validate unique, complete trial repetitions.
- OTel export selects the newest spans rather than replaying the same head.

### Identity memory, employment, org, templates (S5–S8)

- Memory brief and list queries apply viewer identity/department in SQL, so a
  busy project cannot starve an identity's own memory out of the result.
- Imported memory rows are never deleted by a local repair.
- Repair refuses a truncated memory/employment event stream instead of deleting
  rows that still have events.
- Retired identities cannot verify or extend agent memory TTL.
- Employment replay validates event/project identity before rebuilding
  projections and refuses truncated streams.
- The C6 `unsubstantiated_handoff` reason code now increments the S6 retirement
  counter (both singular and plural forms are accepted).
- Promoting a Manager to Leader is refused while it has live direct reports.
- Imported identities are excluded from retirement candidates.
- Identity detail queries parent/successor by project and loads hire/employment
  history by identity rather than a project-wide truncated window.
- Team template apply always requires `approved:true`, even with `dry_run`;
  `dry_run` always returns a preview.
- Template list/get is project-scoped to templates exported, imported, applied,
  or event-registered by that project; another project's source id is never
  exposed. Import-only registration records a per-project event even when the
  global template row already exists.

## Remaining gaps

These are known and deliberately not claimed as solved:

- C4 controller replay still reads a large event window and rebuilds state
  superlinearly; diagnosis decodes payloads while holding the global SQLite
  lock. Both need paging/incremental state.
- S5 prune is projection-only. Older memory events retain content for replay and
  audit; there is no crypto-erasure or event retention policy. ADR-0209 must be
  read with that boundary.
- OTel export has no cursor/offset and cannot distinguish a transient transport
  failure from a permanent endpoint error; `retryable` remains false.
- S8 bundle/rebuild does not recompute the template hash/shape, and an imported
  template link can still look locally applied.
- S6 attribution remains conservative when no explicit run binding exists;
  successor recording does not transfer memory or component-branch ownership.
- S7 org truncation ordering and full-ledger recomputation are not optimized;
  GUI has model/state tests plus manual smoke, not automated UI tests.
- `vp-remote::exec::tests::cancellation_kills_a_running_command` can flake under
  full-workspace load.
- No coverage tooling, seeded old-database migration fixture, or systematic
  adversarial path/URL/shell corpus is committed yet.

## Verification

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`: 3,887 passed, 0 failed
- `swiftformat --lint`: 0/133 files
- `xcodegen generate` + `xcodebuild test`: app and helper suites passed
- New regression tests: non-destructive identity repair with live work,
  assignment sweep skipping live sessions, accepted-subset land, template
  `apply && dry_run` approval, and project-scoped template reads.

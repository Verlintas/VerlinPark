# 0208 — Worker workspaces and two-level integration (S4)

- **Status:** Accepted
- **Date:** 2026-11-13
- **Base:** `dbae8f5` (after ADR-0207)
- **Scope:** S4 of `docs/PROJECT_SCOPED_AGENT_IDENTITIES_PLAN.md`
- **Related:** ADR-0205 (project identities), ADR-0206 (assignments and inbox),
  ADR-0207 (identity prompts and Worker/Labour contracts), ADR-0188
  (instruction store/cache layout), ADR-0193 (task contracts), ADR-0194
  (effect receipts), ADR-0196 (runtime identity pinning)

## Context

S1–S3 made Leader, Manager, and Worker durable project records. A Worker now
has a stable identity, a deterministic role contract, and a lease that prevents
two sessions from driving the same identity concurrently. Execution was still
run-scoped: every Worker and Labour run received a throwaway task worktree
branched from the Leader session branch, and Labour shared its parent Worker's
worktree. There was no place for a Worker to accumulate reviewed component work
across sessions, and no explicit boundary between "a Labour change is ready for
Worker review" and "a Worker commit is ready for the independent Verifier and
the deterministic Integrator".

S4 adds that missing boundary without changing the gates:

```text
project repository
  └── component branch (per Worker identity, long-lived)
        └── session Worker worktree (one branch per session attachment)
              └── Labour task worktree (one branch per Labour run)
```

The important properties are:

- a Worker identity owns a component branch, not a process;
- one session attaches a component workspace at a time;
- Labour worktrees branch from the session Worker HEAD, never directly from the
  session branch;
- Labour→Worker is a deterministic internal merge with a path check, a diff
  check, and an exact rollback;
- Worker→session still goes through the existing Verifier, merge queue, and
  deterministic Integrator;
- the component branch only advances through an explicit `workspace.land` after
  the session branch has accepted the commits;
- legacy sessions with no Worker assignment keep the old task-worktree flow
  byte-for-byte.

## Decision

### 1. Workspace records

Two additive project-scoped tables hold durable workspace state.

```sql
agent_workspaces(
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  identity_id TEXT NOT NULL REFERENCES agent_identities(id) ON DELETE CASCADE,
  component TEXT NOT NULL,
  branch TEXT NOT NULL,
  base_commit TEXT NOT NULL,
  worktree_path TEXT NOT NULL,
  status TEXT NOT NULL,                 -- active | archived | retired
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  last_session_id TEXT,
  lease_token TEXT,
  lease_expires_at TEXT,
  origin TEXT NOT NULL DEFAULT 'local'
);

session_worker_workspaces(
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL REFERENCES agent_workspaces(id) ON DELETE CASCADE,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  identity_id TEXT NOT NULL,
  worktree_path TEXT NOT NULL,
  branch TEXT NOT NULL,
  base_commit TEXT NOT NULL,
  status TEXT NOT NULL,                 -- active | released | archived
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  lease_token TEXT,
  lease_expires_at TEXT,
  UNIQUE (session_id, identity_id)
);
```

Hard invariants are enforced in SQLite and rechecked transactionally so callers
receive typed errors rather than raw constraint failures:

- one active workspace per `(project_id, identity_id, component)`;
- one live component lease per `(project_id, component)`;
- one row per `(session_id, identity_id)`, reactivated on later attach;
- one active session binding per `(session_id, identity_id)`.

The lease fields on `session_worker_workspaces` mirror the workspace lease so a
stale session binding is visible without joining the workspace row. They are
operational coordination state; the lease token never enters a wire response,
event, bundle, CLI line, or UI model.

### 2. Deterministic branch and path names

A workspace may not choose an arbitrary branch or directory. The protocol
exposes two deterministic constructors:

```text
component branch: verlinpark/components/<component-slug>--<identity-slug>
session branch:   verlinpark/sessions/<session-slug>/worker-<identity-slug>
```

`workspace_ref_slug` lowercases ASCII, collapses other characters to `-`, caps
the readable part at 40 characters, and appends a stable FNV-1a suffix. The
store validates that a row's branch equals the constructor output for its
identity/component and that its worktree path is absolute. A branch outside the
`verlinpark/components/` or `verlinpark/sessions/` namespace is rejected.

Worktree paths live outside the existing session worktree tree:

```text
<data>/workspaces/<project>/components/<identity-slug>   // lazy land checkout
<data>/workspaces/<project>/sessions/<session>/<identity-slug>
```

The component checkout is created lazily by `workspace.land`; a normal session
pays for one checkout, not two. The Worktree Retention Policy remains scoped to
the existing `worktrees/` tree; workspace checkouts are evidence and are not
silently removed by that sweeper. This is a known retention gap.

### 3. Assignment and attach flow

`Engine::ensure_agent_workspace` is the single creation/attach path. It:

1. loads the session (when supplied), project, and Worker identity;
2. rejects cross-project identities, non-Worker identities, paused/retired
   identities, imported identities, and a disabled `AgentIdentityPolicy`;
3. resolves the component (explicit argument, identity component, or `worker`);
4. resolves the project HEAD and ensures the component branch exists at that
   commit without resetting an existing branch;
5. prepares the active workspace row and acquires the component lease;
6. creates or reattaches the session Worker worktree at the component HEAD;
7. creates or reactivates the session binding and records
   `agent.workspace.session_attached`.

A Worker run resolves its identity through the S3 deterministic resolver
(`prompt_identity_for_run` with role `worker`, then title/department/age
tie-breaks). If an active assignment exists and the workspace/component is
available, the run uses the session Worker worktree. If no assignment exists,
the run keeps the legacy task-worktree flow. If the component is leased by
another identity/session, `ensure` returns typed `WORKSPACE_BUSY`; the agent
flow falls back to a legacy task worktree and logs the typed reason instead of
failing an otherwise runnable plan.

Within one session, multiple parallel Worker runs for the same workspace are
serialized by an in-process `tokio::sync::Mutex` keyed by workspace id. Different
components use different mutexes and remain parallel. Cross-session exclusion is
the SQLite component lease.

### 4. Labour child worktrees

A Labour delegation under a workspace-bound Worker gets its own task worktree:

- branch: the existing `verlinpark/<session>--<agent-id>` task branch;
- base: the current session Worker worktree HEAD, not the session branch;
- path: the existing session task-worktree path;
- run metadata: `workspace_id = None`, `parent_workspace_id = <session binding
  id>` (plus the normal task/session fields).

Legacy Labour (parent has no session workspace) still shares the parent Worker
worktree exactly as before. This is deliberate: a project that never opts into
identities sees no new branch, worktree, or merge behavior.

### 5. Labour→Worker internal integration

`Engine::merge_labour_commit` is the only Labour→Worker path. It holds the
project Git mutex and:

1. verifies the workspace is local/active and the session binding is active;
2. requires the Worker worktree to be clean (`WORKSPACE_NOT_QUIESCENT`
   otherwise);
3. validates the exact Labour commit id and existence;
4. computes every path changed between the Worker HEAD and the Labour commit;
5. rejects an out-of-scope path with a typed failure and leaves HEAD untouched;
6. runs `git merge --no-ff <commit> -m ...`;
7. on conflict or Git failure, aborts, resets hard to the exact pre-merge
   commit, and reports the conflict paths;
8. after a successful merge, runs `git diff --check` over the merge range and
   resets to the pre-merge commit if the check fails;
9. records `agent.workspace.labour_merged` with the pre/post commits, merged
   paths, task/agent ids, and bounded detail. File content, prompts, and tool
   arguments are not stored.

A failed internal merge is returned to the Worker as typed feedback in the
`delegate` tool result, and the Labour run is marked `failed`. The Worker
workspace HEAD is verified to equal the pre-merge commit before the event is
written. This is review/integration evidence, not a Verifier verdict.

### 6. Worker→session integration is unchanged

The Worker runs in the session workspace worktree and its commit is handled by
the existing `finish_worker` path:

1. commit all changes and enforce the Worker write scope;
2. create an isolated Verifier checkout of the exact commit;
3. run the Verifier with its read-only tool surface and post-run tracked-file
   guard;
4. enqueue exactly one merge-queue entry with the Worker branch and exact
   commit;
5. let the deterministic Integrator merge with `--no-ff`, run
   `git diff --check`, optionally run the project integration command under the
   sandbox, and roll back on failure.

S4 changes only the source worktree/branch. It does not add a second queue
entry, a second Verifier, a new merge path, or an automatic component-branch
update. The Labour→Worker merge commit is the commit the Verifier sees when the
Worker had no additional changes; the Labour commit is reachable through it.

### 7. Explicit component land

`agent.workspace.land` is operator-only and requires `confirm = true`. It:

1. resolves the workspace and its last/selected session;
2. requires the session to be quiescent (`idle`, `failed`, `cancelled`,
   `interrupted`, or `landed`) and not imported/landed;
3. rejects a live lease owned by another session;
4. requires the component and session Worker branches to exist;
5. requires every commit in `component..session_worker` to be an ancestor of
   the session branch (`WORKSPACE_NOT_ACCEPTED` otherwise) — this proves the
   session branch accepted the exact commits through the Verifier/Integrator;
6. creates/reuses the lazy component checkout, requires it clean, and merges
   the session Worker branch with `--no-ff`;
7. runs `git diff --check`; a conflict or failed check resets the component
   branch to its exact pre-land commit and returns a typed failure;
8. records `agent.workspace.landed`, updates the workspace base commit, and
   releases the session binding/lease.

There is no automatic land on session end, plan completion, daemon idle, or
merge-queue success. A failed land never leaves a partial component merge.

### 8. Archive and retire

`agent.workspace.archive` and `agent.workspace.retire` require a released
workspace: a live lease or an active session binding fails with
`WORKSPACE_BUSY`. They never delete the component branch, commits, worktree
path, session bindings, or events. Archive marks future attach/land unavailable;
retire is terminal evidence. A worktree may be removed later only by an explicit
operator action outside S4; no silent cleanup deletes unmerged work.

### 9. Lease lifecycle

The workspace lease defaults to ten minutes (`60..=86_400` seconds); the engine
uses one hour for a Worker run to reduce expiry during a long provider turn.
Lease transitions are operational, while durable transitions are event-backed:

- `agent.workspace.created` — new active workspace row;
- `agent.workspace.session_attached` — session binding active;
- `agent.workspace.labour_merged` — internal merge success or rejection;
- `agent.workspace.landed` — component branch advanced;
- `agent.workspace.archived` / `.retired` — evidence-preserving close;
- `agent.workspace.released` — explicit/session-end/startup release;
- `agent.workspace.lease_expired` — bounded expiry sweep.

Expiry and recovery paths:

- `expire_agent_workspace_leases` clears due leases and releases their active
  bindings, emitting one `.lease_expired` row each;
- `reconcile_agent_workspace_leases` releases leases whose session is no longer
  `running`, `queued`, `paused`, or `waiting_input`, emitting `.released`;
- `Engine::update_session_status` releases workspace leases when a session
  becomes `idle`, `failed`, `cancelled`, `landed`, or `interrupted`;
- daemon startup calls the sweep after interrupted sessions are recovered, and
  the 30-second coordination sweeper repeats it.

A live lease is never silently reassigned. A stale one is reclaimed only after
its recorded expiry or after the owning session is confirmed inactive.

### 10. Events, replay, manifests, and bundles

`agent_workspace_events` is a project-scoped append-only stream with a portable
event id (UUID) and per-project `seq`, mirroring S2 coordination events. Each
event carries bounded snapshots:

```json
{
  "workspace": { "id": "…", "project_id": "…", "identity_id": "…",
                 "component": "auth", "branch": "verlinpark/components/…",
                 "base_commit": "…", "worktree_path": "…", "status": "active",
                 "last_session_id": "…", "origin": "local" },
  "session": { "id": "…", "workspace_id": "…", "session_id": "…",
               "identity_id": "…", "branch": "verlinpark/sessions/…",
               "status": "active" }
}
```

No prompt text, file content, tool argument, provider usage, credential, or
lease token is present. Worktree paths and commits are operational evidence and
already appear in existing agent records.

`agent.workspace.replay` folds the stream into workspace/binding projections and
compares event-derived rows with stored rows. `repair` replaces the projection
from events only; it is read-only with respect to the stream and clears
operational leases. The project manifest (`projects.jsonl`) carries the same
projection plus the stream so `vp db-rebuild` can rebuild both. Session bundles
carry the same three fields; an imported workspace is marked `origin =
imported`, its lease is cleared, an active binding is imported as `released`,
and local attach/land/archive is rejected for imported evidence. Bundle import
inserts the session row before bindings so foreign keys stay valid.

### 11. Protocol, RPC, CLI, and UI

Protocol additions are additive and serde-defaulted:

- `AgentWorkspace`, `SessionWorkerWorkspace`, `AgentWorkspaceEvent`;
- list/get/ensure/land/archive/retire/replay params and results;
- `session.workspace` params/result;
- `AgentRecord.workspace_id` and `.parent_workspace_id`;
- `SessionBundle` workspace fields;
- `ProjectManifestSnapshot` workspace fields.

RPC methods are `agent.workspace.list/get/ensure/land/archive/retire/replay` and
`session.workspace`. `workspace.get/ensure/land/archive/retire/replay` are also
accepted as aliases. The historical `workspace.list` method is the file-browser
listing; it dispatches to the S4 list only when the params carry an agent-shape
selector (`project_id`, `identity_id`, or `status`), so the existing v1
file-browser contract is unchanged. CLI commands keep the product-facing names:

```bash
vp workspaces --project <id> [--session <id>] [--identity <id>] [--status]
vp workspace <workspace-id> | --session <id>
vp workspace-land <workspace-id> --yes [--session <id>]
vp workspace-archive <workspace-id> [--retire] [--reason <text>]
```

`workspace-land` fails without `--yes`; there is no implicit confirmation and no
view-level mutation. Swift decodes workspace/binding metadata and shows a
read-only Manage Dash card and a Stage inspector section. The app never attaches,
merges, lands, archives, or retires; those remain explicit operator/CLI actions.

All new fields are `serde(default)` / `decodeIfPresent` with legacy defaults.
The daemon maps typed workspace failures to bounded RPC errors with the stable
code in `data.code`.

## Tests

Unit tests cover:

- deterministic component/session branch names, slug safety, component/lease
  validation, and invalid identity/session path components;
- Labour merge success, out-of-scope rejection, dirty-Worker rejection, conflict
  abort with exact rollback, diff-check rollback, and no partial content;
- component land success, idempotent no-op, conflict rollback, and unaccepted
  commit detection;
- event replay ordering/duplicate rejection, two-lease projection rejection,
  and projection comparison drift;
- store prepare/attach/busy, lease expiry, explicit release, archive evidence,
  and replay primitives.

Integration tests (`crates/vp-core/tests/worker_workspaces.rs`) cover:

- an assigned Worker gets the component branch + session worktree, and a Labour
  child worktree base equals the Worker workspace HEAD;
- Labour commit merges into the Worker workspace, its event carries no file
  content, and a conflicting second Labour merge leaves Worker HEAD unchanged;
- two sessions/identities cannot lease the same component; the agent flow falls
  back without failing the session;
- expired lease recovery and explicit release are idempotent;
- land fails before the session branch accepts the commit, succeeds afterward,
  updates the component branch, and records `.landed`;
- archive requires release and preserves branch/event evidence;
- a legacy session with no assignment has no workspace and keeps the task
  worktree flow;
- replay/repair rebuilds the projection; bundle export/import preserves
  read-only evidence;
- the full Leader → Manager → Worker → Labour path still produces exactly one
  Verifier/Integrator merge-queue entry, merges the Worker workspace commit to
  the session branch, and materializes the Labour file.

Protocol, CLI, daemon error, and Swift decode tests cover the additive surfaces.
The full Rust workspace and Swift test suites stay green.

## Limits and non-goals

- A workspace lease is same-host SQLite coordination. It is not a distributed
  lock, cross-host consensus, or exactly-once external effect.
- The in-process workspace mutex does not coordinate separate daemon processes;
  the data-dir instance lock and the SQLite lease are the cross-process boundary.
- A workspace worktree is not removed by the existing worktree retention
  sweeper. Archived/retired rows retain their paths and branches; storage
  reclamation is an explicit operator action outside S4.
- `land` verifies accepted commits and a clean diff, but it does not rerun the
  project test suite. The Verifier and Integrator already did; an integration
  command is not repeated on land.
- An imported workspace is evidence. It cannot be attached, re-leased, merged
  into, landed, archived, or retired locally; rehoming is not part of S4.
- The component branch is one per Worker identity/component. There is no
  cross-identity shared component branch, no cross-project workspace, and no
  automatic workspace migration/successor transfer (S6).
- S4 does not add identity memory (S5), hiring/retirement (S6), an org chart
  (S7), or team templates (S8).
- No automatic component-branch land, no hidden merge, no force-push, no
  replacement of the Verifier/Integrator, and no new model routing.
- A worker run that cannot acquire its workspace falls back to a legacy task
  worktree; that run's commit still reaches the session branch but is not part
  of the component workspace until an explicit future mechanism integrates it.
  This is visible in the absence of a workspace binding on that run.
- The `agent.workspace.*` RPC prefix is the canonical S4 name because
  `workspace.list` is also the existing file-browser method. The guarded
  `workspace.list` compatibility path is selector-based; a session-only agent
  list should use `agent.workspace.list`.

## Acceptance matrix

| Requirement | Where it is enforced | Test evidence |
| --- | --- | --- |
| Assigned Worker gets component branch + session worktree | `Engine::ensure_agent_workspace`, `agent_workspace::ensure_worktree` | assigned-worker integration test |
| Labour child base equals Worker workspace HEAD | agent delegation Labour branch from `spec.branch` | assigned-worker and full-flow tests |
| Labour merge has scope/diff/rollback | `agent_workspace::merge_labour_commit` | unit + integration conflict tests |
| Worker commit still enters Verifier/Integrator | unchanged `finish_worker`/`integrate_worker` | full-flow one-queue/merged test |
| No double enqueue/double merge | single `finish_worker` call site; queue count assertion | full-flow test |
| Two sessions cannot lease one component | partial unique index + typed `WORKSPACE_BUSY` | store + integration conflict tests |
| Expired lease recovery and explicit release | `expire_*`, `reconcile_*`, `release_session_workspaces` | expiry/release tests |
| Land requires quiescence and accepted commits | `land_agent_workspace` ancestor checks | land accept/reject test |
| Failed land rolls back component branch | `land_component_branch` reset hard | core rollback test |
| Archive preserves evidence | `archive_agent_workspace` never deletes | archive test |
| Legacy flow unchanged | no assignment → legacy task worktree | legacy test |
| Replay/repair/bundle/db-rebuild | workspace stream + manifest + bundle fields | replay + bundle tests |
| Additive protocol/Swift decode | serde defaults + `decodeIfPresent` | protocol + Swift tests |

## Operational notes

- `vp workspaces --project <id>` is the main read surface. A live lease is shown
  as `leased`; it is not a claim that the process is healthy.
- Before landing, confirm the session is quiescent and the queue has no pending
  Worker entry for the same workspace. `land` refuses unaccepted commits, but
  it cannot know whether an operator intended to include a later commit.
- A failed land leaves the component checkout at its pre-land commit. The
  session workspace and session branch are untouched.
- Archive after release is the normal close-out. Retire is terminal and should
  be used only when the identity will not work this component again.
- The workspace event stream is project-scoped and grows with session count;
  current reads are bounded to 100,000 events, matching the coordination stream.
- If a daemon crashes mid-land, the component checkout is left at the
  pre-merge or merge commit; the absence of a `.landed` event means the land is
  not published. The operator should inspect the component checkout before
  retrying.
- Imported archives never reattach a worktree. `session.rehydrate` remains the
  explicit local-worktree path for an imported session and does not grant a
  project workspace.

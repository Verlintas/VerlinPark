# 0232 — Collab Agent Inspector and direct operator channel

- Status: Accepted
- Date: 2026-11-22
- Scope: Collab inspection, agent-scoped event index, bounded/redacted read model, operator message routing, delivery states, audit, SwiftUI
- Depends on: ADR-0119 (risk approvals and Collab), ADR-0120 (dynamic team graph), ADR-0139 (operator steering), ADR-0159 (tool-output paging), ADR-0164 (context checkpoints), ADR-0188 (instruction store and prompt layout), ADR-0189 (append-only conversation), ADR-0191 (cache epochs), ADR-0192 (cache telemetry), ADR-0205 (project identities), ADR-0206 (assignments and project inbox), ADR-0207 (identity prompts and role contracts), ADR-0209 (identity memory), ADR-0231 (auto budget and adaptive tooling)
- Plan: [V1_FINAL_PLAN.md §3.1](../V1_FINAL_PLAN.md#31-collab-agent-inspector-and-direct-channel)
- Does not implement: a general chat room, agent-to-agent chat, a new chat runtime, spawning an idle Agent just to chat, cross-session or cross-project targeting, a full hidden-prompt dump, new approval/gate/Verifier/merge authority, or a distributed event index.

## Context

Collab already selects Agent nodes and shows bounded task/activity metadata.
Stage already supports `session.steer` for a running or waiting Agent, and the
project inbox already supports durable operator messages for idle identities.
Those pieces are insufficient for V1:

- the graph cannot show an Agent's own context, visible conversation, tool
  evidence, or delivery history without leaving Collab;
- `session.steer` is reachable only through a separate Stage sheet, so the
  operator can effectively talk only to the Leader from the graph;
- the event table is ordered by `(session_id, seq)` and `agent_id` lives inside
  JSON payloads, so "this Agent's events" is not an indexed read;
- a naive inspector would scan the whole session, then risk returning
  `agent.started.prompt`, raw tool input, raw tool output, or an unbounded
  transcript. Those bytes are not an operator UI contract.

V1 therefore needs an Agent-level inspection surface and a truthful direct
message route, not another orchestration console and not a chat product.

## Decision

### 1. The Collab Agent node is the Agent Inspector

Selecting a real Agent row opens a bounded inspector with five tabs:

| Tab | Content |
|---|---|
| **Overview** | role, title, department, task, status, model, profile, parent, worktree/branch label, budget/cost, identity/workspace identifiers |
| **Context** | current objective, task acceptance/write scope, context usage, bounded last-compaction label, memory-brief size/hash, tool bundle/count, prompt-prefix hash, agent-scoped usage/cache telemetry |
| **Conversation** | the Agent's recent visible turns, tool calls and result status/error summaries, test evidence, citations, and failures |
| **Messages** | steering and inbox history for this Agent, delivery state, unread counts, and the operator composer |
| **Activity** | tools, approvals, commits, merge queue, coordination, failures, and recovery evidence carried by agent-scoped events, grouped by the server-provided timeline category |

The node uses the existing graph's derived status/activity state (including
running and tool-active), plus pending-steering, blocked/failed, and completion
signals. Unread inbox state is shown in the selected Agent's Messages tab; a
per-node unread badge is not claimed in this round because inbox counts are
loaded only for the selected Agent. Planned ghost nodes, department group nodes,
and task-only nodes keep the existing task inspector; they are not inspectable
Agent rows and have no message channel.

The client is a renderer. It does not invent delivery, mark a message delivered,
or treat a local optimistic state as daemon truth. The existing task actions
(retry/skip/pause/resume/cancel) remain available where they already exist; the
inspector does not become a second plan editor.

### 2. `agent.inspect` is one bounded, read-only snapshot

`agent.inspect { session_id, agent_id, timeline_limit? }` returns:

- `agent`: the stored `AgentRecord`;
- `task?`: plan-task title, department, kind, status, acceptance, write scope,
  dependencies, attempts, verifier status;
- `context`: bounded context/cache/usage metadata;
- `timeline`: the newest bounded Agent-scoped events, oldest-first in the wire
  snapshot (the UI may display newest-first), each with a stable category
  (`message`, `tool`, `approval`, `result`, `context`, `coordination`, `merge`,
  `question`, or `other`), event kind, bounded/redacted text, tool name,
  tool-call id, status, and error flag;
- `steering`: this run's bounded steering history;
- `inbox` and `inbox_counts`: this session's inbox rows for the Agent's
  persisted identity, plus unread/read/done/expired counts;
- `truncated`: whether more indexed Agent events exist outside the window.

`timeline_limit` defaults to **80** server-side (the macOS client requests
**80**) and is clamped to **1..=200**. The newest window is returned;
`truncated` is proven by reading one extra row below the hard ceiling and by an
indexed count at the ceiling. The engine caps timeline
text at **1,200 characters per row**, steering at **50 rows**, and inbox at
**50 rows**. These are wire bounds, not UI suggestions.

Target resolution is strict:

- `session_id` and `agent_id` are required;
- the Agent must be a member of that session; there is no lookup by name,
  title, role, or task;
- a foreign/unknown target fails with `AGENT_NOT_FOUND`; a structurally invalid
  request fails with `AGENT_TARGET_INVALID`;
- an imported archive is inspectable as preserved evidence but remains
  read-only for messaging;
- inspection never appends an event, changes Agent status, marks inbox rows
  read, starts a run, wakes a session, or resolves an approval.

The read model is one snapshot, not one RPC per tab. The plan's proposed
`agent.context` is the `context` portion of this snapshot; V1 does not add a
second independent context RPC. The UI maps `timeline.category` into
Conversation and Activity. This avoids N round trips and inconsistent snapshots
while the Agent is moving.

### 3. Agent-scoped reads use a derived event index

`agent_event_index` is derived state:

```text
agent_event_index(
  session_id TEXT NOT NULL,
  agent_id   TEXT NOT NULL,
  seq        INTEGER NOT NULL,
  kind       TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY (session_id, agent_id, seq)
)
```

A SQLite trigger inserts a row after every `events` insert when
`json_valid(payload)` and `json_extract(payload, '$.agent_id')` is text. A
delete trigger removes the corresponding derived row. Existing databases are
backfilled once from `events` under the `agent_event_index_v1` marker; if the
marker exists but the table is empty, the backfill runs again rather than
returning a silently empty inspector.

Reads join `agent_event_index` back to the authoritative `events` row, so a
stale or orphaned index row cannot fabricate an event. Indexing is explicit:
only a payload `agent_id` is indexed. Role, title, name, task, and parent
matching are never used to guess ownership.

The index is rebuildable, not evidence:

- it is not a bundle authority and is not exported as a source of truth;
- event reinsertion (replay/repair/`db-rebuild`) re-derives it through the
  triggers;
- the store reopen path repairs a missing/empty index;
- the authoritative event rows remain the only source.

Task, merge queue, commit, and recovery projections still supply task-level
evidence that has no Agent payload. The inspector may merge that evidence into
Activity by the selected Agent's `task_id`, but it must label it as task-scoped
and must not attribute a shared task attempt exclusively to one run. There is
no cross-session or cross-project index; this is same-host SQLite state.

### 4. Bounds and redaction are part of the contract

The inspector is an operator view, not a hidden-prompt dump. The read model
must not return:

- `agent.started.prompt`, composed system/core/project/identity prompt text,
  skill bodies, or memory-brief bodies;
- raw `tool.started.input`, raw/unbounded `tool.finished.output`, or file
  content;
- raw approval input, raw inbox JSON payloads, provider keys, credentials,
  lease tokens, or environment values;
- an unbounded transcript, compaction summary, or tool-output body.

The server may read `agent.started` for goal/model metadata, but it returns only
metadata. Conversation rows carry a bounded, deterministically redacted display
string; tool rows carry name/status/error metadata and a bounded summary, not
the raw tool arguments or output. `AgentContextView` carries counts, hashes, and
tool-bundle identity rather than prompt text: `estimated_tokens`,
window/threshold/hard limit, input/output/cache tokens, cost, `tool_count`,
`tool_bundle`, `prefix_hash`, `last_compaction`,
`memory_brief_chars`, and `memory_brief_hash`. Inbox rows carry a bounded
display body drawn from known fields; an unknown-shape payload is reduced to a
bounded, redacted summary, never the full raw object. Operator-authored
steering/inbox text is displayed as the operator wrote it, bounded by the
message cap; it is not hidden prompt material.

Redaction is deterministic and pattern/field-based. It is not a general secret
classifier, and Agent-authored text remains untrusted. Truncation is always
reported (`truncated` and bounded row length); silence is not used to imply
completeness.

### 5. `agent.message.send` routes by target state

`agent.message.send { session_id, agent_id, body, task_id?, operation_id?,
wake_session? }` is the only Collab-local operator send path. `body` is
1–4,000 characters. It is operator-only: it is not an Agent tool, not exposed
through MCP or Code Mode, and not an Agent-to-Agent chat bus.

Routing is deterministic and truthful:

| Target state | Route | Delivery state |
|---|---|---|
| `running` or `waiting_input` | existing `session.steer` | `pending` → `delivered` at the next provider boundary, or `cancelled` when the run ends first |
| not active, with a persisted project `identity_id` | existing `agent.inbox.send` (`kind=message`) | `queued`/`unread`, later `read`, `done`, or `expired`; optional wake |
| not active, without an identity (Labour/Verifier/Scout and other ephemeral runs) | reject | typed `AGENT_NOT_ACTIVE`; no inbox row, no pending note, no new run |
| foreign session/project, unknown Agent, retired/invalid identity, imported/landed target | reject | typed `AGENT_NOT_FOUND`, `AGENT_TARGET_INVALID`, or the existing `INBOX_*`/steering error; no write |

Additional rules:

- The target's identity is taken only from the run's persisted `identity_id`.
  Role/title/name matching is never used to find an inbox recipient.
- A supplied `task_id` must match the target run's current task; the route does
  not silently retarget a message.
- A live run uses steering even when it also has an identity. Idle identity
  delivery is the fallback, not a parallel broadcast.
- For an idle identity, `wake_session=true` may create the existing deferred
  continuation only when the session is `idle`, `failed`, or `cancelled`. It
  returns a bounded `AgentCoordinationWake` (`requested`, `created`,
  `continuation_id`, `reason`). A wake does not call a provider, create a run
  or worktree, reserve a Worker/Labour slot, or claim instant delivery.
- Without a wake, an inbox row is durably stored but remains pull-based. V1
  does not auto-inject inbox rows into a run prompt, so the row can remain
  unread; the operator should use an explicit wake for timely delivery.
- `operation_id` follows the existing `session.steer` operation-receipt
  semantics for steering. An inbox row is identified by its own id; a blind
  retry after a transport error is not a distributed transaction.
- The route never spawns an Agent, starts a session, creates a worktree, or
  changes capacity/budget/model routing.

`AgentMessageDelivery` reports `kind` (`steering`, `inbox`, or defensively
`rejected`), `status`, a bounded truthful `message`, the Agent/identity, the
steering or inbox row, and the wake result. `AGENT_NOT_ACTIVE` is surfaced as a
typed JSON-RPC error with `data.code`, never as a fake successful delivery.

### 6. Messages are audited; authority is unchanged

Successful delivery is audited through the existing rows and events:

- `steering.created`, `steering.delivered`, `steering.cancelled`;
- `agent.inbox.message`, `agent.inbox.read`, `agent.inbox.done`,
  `agent.inbox.expired`.

The router must not introduce an unaudited delivery path. It may add a
content-free routing observation, but it must not duplicate the message body
into a second source of truth. A rejected send creates no deliverable message
row.

A direct message is text, not a command channel. It cannot:

- resolve an approval or approval template;
- resolve or cancel an external gate;
- submit a Verifier verdict;
- enqueue, retry, resolve, or merge a commit/branch;
- change budgets, unlimited mode, capacity, plan tasks, dependencies, model
  routing, sandbox/write scope, or the project contract policy.

If the target Agent later chooses an action, that action still passes through
the normal contract, approval, sandbox, Verifier, Integrator, merge, budget,
and capacity gates. The message itself grants no authority.

Cross-session and cross-project targets fail closed before any row is written.
The daemon remains loopback-only with its existing discovery-token
authentication; the new RPCs add no public listener.

### 7. Surfaces

RPC:

- `agent.inspect`
- `agent.message.send`
- existing `session.steering`, `session.steering.cancel`, `agent.inbox.list`,
  `agent.inbox.read`, and `agent.inbox.ack` remain the read/ack surfaces.

Swift:

- `AgentInspectModels.swift` decodes the bounded snapshot and preserves typed
  `AGENT_NOT_ACTIVE` error text;
- `AppModel.loadCollabAgentInspect` is an explicit, non-polling refresh;
- `AppModel.sendCollabAgentMessage` is explicit, non-auto-retried, and never
  synthesizes a delivery notice from a failed RPC;
- the Collab inspector renders the five tabs and badges from daemon data only.

CLI parity is implemented through the same RPCs, not a second delivery path:

- `vp agent-inspect <session> <agent> [--timeline-limit N]`
- `vp agent-message <session> <agent> <text> [--task T] [--wake] [--operation-id ID]`

Both preserve typed daemon errors and print the delivery kind/status; JSON mode
passes the daemon payload through unchanged.

## Consequences

- The operator can inspect any live or historical Agent from Collab without
  loading the whole session transcript.
- Running Agent messages use the established cooperative steering path; idle
  project identities use the durable inbox; ephemeral idle runs fail honestly.
- Agent-scoped reads are indexed and bounded. The event index is derived and
  can be rebuilt; no new source of truth was added.
- Context/Conversation remain operator views. They are not the hidden prompt and
  not a complete conversation archive; Stage/search remain the full audit path.
- The route is a router, not a chat engine. There is no new Agent, run, mailbox,
  model, or approval authority.

Limits:

- Delivery is cooperative. A steering message cannot interrupt an in-flight
  provider stream or tool call; it waits for the next provider boundary and is
  cancelled if the run ends first.
- An idle ephemeral Agent cannot receive a message. This is intentional; no
  undeliverable pending note is stored and no run is spawned to simulate chat.
- The project inbox is pull-based. Without a wake, V1 does not inject the row
  into a run, so it can remain unread until an explicit read or wake.
- ACP-backed runs do not have a native steering drain in V1. A message routed to
  a running ACP Worker can remain `pending` and end as `cancelled`; there is no
  ACP-specific direct-channel bridge.
- The inspector timeline, steering list, and inbox list are bounded recent
  windows, not complete archives. Older history is available through Stage
  transcript/search, not by unbounded inspector pagination.
- Redaction is best-effort and deterministic; model-authored text is untrusted.
  The inspector is not a semantic secret classifier.
- Task-level evidence merged into Activity is labeled and may be shared by
  multiple attempts of the same task; it is not exclusive Agent attribution.
- The event index is same-host SQLite coordination. There is no cross-host or
  distributed inspector index.
- Node-level unread inbox badges are not part of this round. Unread counts are
  available for the selected Agent in Messages; a session-wide unread summary
  (and therefore a badge on every node) is a follow-up.
- The Context tab does not expose a checkpoint summary in this round. It shows
  the bounded last-compaction label plus context/usage/cache metadata;
  checkpoint detail remains in the existing Stage/checkpoint surfaces.
- The UI path is covered by the app test suite plus a manual smoke requirement;
  automated XCUITest coverage is still absent.

## Verification

Status: **implementation verified on 2026-11-22**. Final full-suite evidence:
`cargo test --workspace` **4,534 passed / 0 failed / 6 ignored**;
`cargo test -p vp-store` 433; `cargo test -p vp-protocol` 294;
`cargo test -p vp-core --lib` 2,571 / 2 ignored; `cargo test -p vp-daemon`
49 unit + 1 governance; `cargo test -p vp-cli` 186 unit + 10 backup integration
+ 1 governance; `xcodebuild test` **474 app + 132 helper, 0 failures**;
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
and `swiftformat --lint` passed.

Commands:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p vp-protocol
cargo test -p vp-store
cargo test -p vp-core
cargo test -p vp-daemon
cargo test -p vp-cli
cargo test --workspace
swiftformat --lint apps/macos/VerlinPark apps/macos/VerlinParkTests \
  apps/macos/VerlinParkBrowserHelper apps/macos/VerlinParkComputerHelper \
  apps/macos/VerlinParkComputerHelperCore apps/macos/VerlinParkComputerHelperTests
cd apps/macos && xcodegen generate
cd apps/macos && xcodebuild -project VerlinPark.xcodeproj -scheme VerlinPark \
  -configuration Debug -destination 'platform=macOS' \
  -derivedDataPath build CODE_SIGNING_ALLOWED=NO test
```

Required scenarios:

1. **Index**: trigger insert/delete, one-time backfill, empty-index repair,
   authoritative-event join, newest bounded window, count-based truncation,
   and no cross-Agent rows.
2. **Inspect**: same-session target only; unknown/foreign target fails closed;
   imported archive is read-only; `truncated` is honest; timeline text is
   bounded/redacted; `agent.started.prompt`, raw tool input/output, raw approval
   input, and raw/unbounded inbox payloads do not appear.
3. **Live send**: running Worker returns `steering`/`pending`; delivery at the
   next boundary becomes `delivered`; a run that ends first becomes `cancelled`;
   task mismatch fails closed.
4. **Idle identity send**: `agent.inbox.send` creates one `queued`/`unread` row;
   read/ack transitions are idempotent; `wake_session` returns an explicit wake
   result and only creates a continuation for idle/failed/cancelled sessions.
5. **Ephemeral send**: Labour/Verifier/Scout with no identity returns typed
   `AGENT_NOT_ACTIVE`, writes no inbox row, and starts no run.
6. **Authority**: a message cannot resolve an approval, resolve a gate, submit a
   verdict, enqueue/merge, change budget/capacity/plan/model routing, or bypass
   the Verifier/Integrator.
7. **Swift**: inspector load, tab/category rendering, badge derivation, typed
   error preservation, delivery notice truthfulness, no optimistic delivery, and
   selection-generation cancellation. `AGENT_NOT_ACTIVE` is shown as a typed
   failure, not a fake success.
8. **Persistence**: bundle import/replay/`db-rebuild` re-derive the index and do
   not treat it as evidence; imported sessions remain inspectable but cannot
   send.

The final counts are recorded above. The index, inspect, live-send, idle-identity,
ephemeral `AGENT_NOT_ACTIVE`, authority, Swift, and persistence scenarios are
covered by the store/core/daemon/CLI and Swift tests; the manual macOS GUI smoke
remains a release-gate item rather than an automated XCUITest.

## Alternatives considered

- **Filter the whole session transcript in the client.** Rejected: unbounded,
  no Agent index, and it would send hidden prompt and raw tool bytes to the UI.
- **Return the full composed prompt in Context.** Rejected: it is a hidden
  provider contract, may contain project/identity instruction bodies, and is
  not an operator read model.
- **Store a pending note for an idle ephemeral Agent.** Rejected: the note can
  never be delivered, and it would imply a channel that does not exist.
- **Spawn a new idle Agent run to host the conversation.** Rejected: it creates
  work, cost, capacity, and a fake conversation rather than a real channel.
- **Add an Agent-to-Agent chat bus.** Rejected: V1's direct channel is
  operator-only; existing coordination notes, steering, and the project inbox
  remain the Agent collaboration surfaces.
- **Use the existing session mailbox `send_message` tool for the operator.**
  Rejected: it is session-scoped, Agent-facing, and not addressed to one
  running Agent or durable identity with truthful delivery states.
- **One RPC per tab.** Rejected: multiple snapshots can disagree while an Agent
  is moving, and the index already supports one bounded read.
- **Interpret message text as an approval or gate command.** Rejected: text
  must never carry runtime authority; all existing gates remain mandatory.

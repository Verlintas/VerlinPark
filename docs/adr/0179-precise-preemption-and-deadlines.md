# ADR-0179: Precise preemption and deadlines

- **Status:** Accepted
- **Date:** 2026-11-12
- **Deciders:** VerlinPark runtime maintainers
- **Related:** ADR-0139 (session cancel/pause), ADR-0141 (session time budget),
  ADR-0156 (shell write sandbox), ADR-0170 (tool error contract),
  ADR-0172 (deterministic scaling), ADR-0173 (durable process supervisor),
  ADR-0176 (critical-path governor), `docs/LONG_RUN_OPERATIONS_HARDENING_PLAN.md` H3

## Context

A long VerlinPark run already supports cooperative `session.cancel`,
`session.pause`/`session.resume`, a session wall-clock budget, provider idle
timeouts, tool timeouts, and a durable run journal. Those controls share one
weakness: they are checked at turn/tool boundaries. A provider that is emitting
a long SSE response, a shell tree that is sleeping, a hanging local HTTP
request, a browser helper RPC, an LSP request, or an MCP tool call can run far
past the wall-clock deadline without observing the cooperative flag. The run
then looks "cancelled" while the underlying work is still alive.

H3 adds a narrow, typed hard-stop path. It is deliberately **not** a scheduler,
not an automatic run killer, and not an exactly-once side-effect system.

## Decision

### 1. Two distinct controls

| Control | Trigger | Granularity | Effect |
| --- | --- | --- | --- |
| Cooperative cancel/pause | operator | turn/tool boundary | asks the run loop to stop/pause; in-flight work may finish |
| Hard preemption | wall-clock, explicit `session.preempt`, per-tool deadline | in-flight request/tool | aborts provider/tool work with bounded cleanup and a typed result |

Cooperative pause remains the default operator action. Hard preemption is used
only when the run is already out of wall-clock budget, an operator explicitly
asks for it, or a configured per-tool deadline fires. No background scheduler
scans for "slow" runs and preempts them.

### 2. `PreemptToken` and `RunDeadline`

`PreemptToken` lives in `vp-providers/src/preempt.rs` because the `Provider`
trait accepts it directly; `vp-core/src/preemption.rs` re-exports it and owns
the policy, attribution, tool-result mapping, and event records.

A token is cloneable, process-local, and idempotent:

- **Reason:** `wall_clock`, `explicit`, or `tool_deadline`.
- **Phase:** `provider`, `tool`, or `queue` (admission/write-lease/approval
  wait). The runtime records the phase that was active when the trigger fired.
- **Elapsed:** milliseconds from token creation, recorded on the stable
  snapshot.
- **Deadline:** an optional `Instant`; there is no infinite grace. A token with
  no deadline still accepts an explicit operator preemption.
- **Side effects:** `side_effects_unknown` defaults to `true` for a tool phase
  and is never downgraded after it is set. Read-only tools are the only
  exception, and only when the specific tool result is known to be safe.
- **Partial usage:** input/output/cache tokens, output bytes, and an
  `usage_estimated` flag are attached after the cut, never invented.
- **Parent/child:** a per-tool child token observes its parent run token. A
  child's own deadline produces `tool_deadline`; a parent trigger propagates
  the parent's reason and phase instead of being misattributed.

`RunDeadline` is the deterministic wall-clock view: session start plus the
configured time budget. It uses the same clock as `SessionBudget`, so the
journal, budget check, and preemption event agree on when the run expired.

### 3. Provider semantics

Provider adapters accept the token through
`Provider::stream_chat_preemptible`. The default trait implementation ignores
the token for compatibility, but the runtime consumer also watches the token
between chunks, so third-party providers still stop at the next chunk.

First-party OpenAI-compatible and Anthropic adapters:

- abort the HTTP/SSE read between chunks and drop the response body;
- do not retry a preempted attempt, including retry backoff sleeps;
- yield a typed `ProviderError::Preempted` with reason, phase, elapsed, and
  side-effect certainty;
- never resume a stream or replay a partial turn.

After the cut, the runtime gives the provider a bounded `provider_grace_ms`
window to deliver a final usage event. It accepts only `Usage`/`Done`; the
first text or tool chunk ends the drain. Text received in this window is
discarded rather than appended to the partial message.

Partial output is persisted as an `agent.message` event with `partial: true`
and a `provider.preempted` event. The run journal marks the model step failed
and the run `interrupted` with failure kind `preempted`. Provider-reported
usage is persisted exactly; absent usage is estimated from the request/partial
output and clearly marked `estimated`. The partial turn is not replayed into a
resumed provider request.

### 4. Tool semantics

The run loop scopes each tool call inside its child token. Tools fall into two
groups:

1. **Protocol-aware:** shell, process, browser, computer, MCP, `fetch_url`,
   `local_http`, and the LSP-backed code tools. They observe the token and
   return a typed preemption result themselves.
2. **Outer-bounded:** every other tool runs under a `select!` that drops the
   future when the token fires. Dropping the future aborts the in-flight
   request; the loop synthesizes the same typed result.

A typed tool result is `PREEMPTED` for wall-clock/explicit preemption and
`DEADLINE_EXCEEDED` for a per-tool deadline. It contains `reason`, `phase`,
`tool`, `elapsed_ms`, `side_effects_unknown`, partial token/byte counts, and
`auto_retry: false`.

Specific cleanup:

- **Shell:** SIGTERM to the process group, wait `kill_escalation_ms`, SIGKILL,
  then wait `tool_grace_ms` for reaping. Bounded stdout/stderr are captured
  from shared buffers before the reader tasks are aborted. The existing
  `ProcessGroupGuard` remains as the final drop-time SIGKILL backstop. No orphan
  group is expected; a failure to reap is reported as unknown.
- **Durable process:** `process wait` observes the token and returns without
  mutating the process; `start`/`stop` are bounded supervisor operations.
- **HTTP/fetch/local_http:** the request future is aborted. `local_http` is
  conservatively side-effect-unknown because it can issue non-GET methods;
  `fetch_url` is GET-only and reports no workspace side effect.
- **Browser/computer:** the helper RPC future is dropped, the helper is shut
  down when its protocol state is unknown, the GUI action timeline records
  `preempted`, and the daemon-wide `gui` lease is released. Snapshot refs are
  not reused after preemption.
- **LSP:** best-effort `$/cancelRequest` with the request id, pending entry
  removed, typed `LspError::Preempted`. LSP-backed code tools do not silently
  fall back to lexical results after a preemption.
- **MCP:** protocol `notifications/cancelled` with `requestId` where the
  transport exposes the id. Transport cancellation is best-effort; a server
  that ignores the notification is abandoned within the existing bounded MCP
  timeout. Tool calls are never replayed.

A tool deadline terminates only the tool. The typed failure is fed back to the
agent, and the run continues. Wall-clock and explicit preemption terminate the
run.

### 5. Policy

A project-scoped `PreemptionPolicy` is persisted in SQLite
(`project_preemption_policies`), copied into `projects.jsonl`, and restored by
`db-rebuild`:

```json
{
  "enabled": true,
  "provider_grace_ms": 2000,
  "tool_grace_ms": 5000,
  "kill_escalation_ms": 2000,
  "tool_deadline_secs": null,
  "checkpoint_on_preempt": true
}
```

- `enabled` defaults to true for wall-clock preemption. It does not disable an
  explicit `session.preempt`.
- Graces are clamped to `provider_grace_ms <= 30000`,
  `tool_grace_ms <= 60000`, and `kill_escalation_ms <= 30000`. There is no
  "infinite" value.
- `tool_deadline_secs` is optional and clamped to `1..=3600`. `null` disables
  per-tool deadlines. A per-tool deadline also requires `enabled`.
- `checkpoint_on_preempt` defaults to true. A checkpoint failure is logged and
  never changes the cancelled status or merges work.

Older manifests decode the bounded default. Invalid RPC values are rejected;
accepted values are normalized before storage.

### 6. Surfaces and events

- **RPC:** `session.preempt` (operator signal with optional bounded reason),
  `session.preemption` (live token state plus event-derived history, policy, and
  phase/reason counts), `preemption.overview` (project-wide terminal counts and
  per-session summaries), `project.preemption_policy.get/set`.
- **CLI:** `vp preempt <session> [--reason]`, `vp preemption <session>`,
  `vp preemptions <project>`, `vp preemption-policy <project>`,
  `vp preemption-policy-set ...`.
- **Swift:** Stage preemption banner and a "Preempt Now" menu action; Manage
  Dash preemption card and overview metric.
- **Events:** `session.preempt_requested`, `provider.preempted`,
  `tool.preempted`, and the terminal `session.preempted`. Payloads are bounded
  `PreemptionRecord` objects with no prompt text, tool input, output body,
  credentials, or screen content.
- **Terminal record:** `session.preempted` is the authoritative record used by
  metrics. `provider.preempted`/`tool.preempted` have `terminal: false`; a tool
  deadline record never claims the session was preempted.
- **Replay:** history is derived from the event stream rather than a mutable
  projection. Replay, repair, bundle import, and `db-rebuild` preserve it.
- **Metrics:** daemon metrics expose `preemptions_by_phase` and
  `preemptions_by_reason` from terminal records, plus Prometheus
  `verlinpark_preemptions{phase=...}` and
  `verlinpark_preemptions_by_reason{reason=...}`. No session, agent, or task id
  is a label.

### 7. Checkpoint and merge safety

A terminal preemption checkpoints the session worktree at the safe boundary,
then appends `session.preempted`, then finalizes the session as `cancelled`.
When the Leader preempts while a Worker/Labour task is the active agent, the
runtime also snapshots that task worktree under a daemon-owned
`preempt-<uuid>` ref and records the commit as `task_checkpoint_commit`. That
snapshot may include tracked and untracked non-ignored work, never moves the
task branch, and never enters the merge queue automatically.
The checkpoint uses a preemption-specific internal path that bypasses the
running/paused quiescence gate because the run loop is the only writer and all
provider/tool work has stopped; it is not exposed over RPC and is not a
general-purpose bypass. If the checkpoint fails, the record has
`checkpoint_id: null` and the run still stops.

Incomplete Worker/Labour work is never committed or merged by preemption. A
cancelled worker branch simply does not enter the merge queue; the existing
Verifier and Integrator gates are unchanged. A preempted durable run is marked
`interrupted` with failure kind `preempted` and is excluded from automatic
resume; an operator can still resume it explicitly.

### 8. Limits and non-goals

- Preemption is a **same-daemon** signal. It is not a distributed cancellation
  protocol, cross-host lease, or remote attestation.
- Provider calls are **not exactly-once**. A preempted HTTP request may have
  been processed by the provider even when no usage was returned. Partial
  usage is honest, not complete billing truth.
- Tool side effects are not rolled back. `side_effects_unknown: true` means an
  operator or resumed agent must inspect the worktree/process/remote state.
- There is no mid-stream resume, no replay of a partial provider turn, and no
  automatic retry after a preemption.
- A tool deadline is not a session deadline; it does not abort sibling tools in
  a parallel batch (parallel batches observe the run token).
- A process group that escapes its group, a daemon-mediated side effect
  (Docker, launch services), or a remote host is outside the local kill/flush
  guarantee.
- Grace windows are bounded, but they are not a latency SLA: a provider or
  kernel may take time to close a socket or reap a child after the bound.

## Implementation map

| Concern | Location |
| --- | --- |
| Token, deadline, parent/child propagation | `crates/vp-providers/src/preempt.rs` |
| Provider trait hook and retry abort | `crates/vp-providers/src/lib.rs`, `openai.rs`, `anthropic.rs` |
| Policy, records, tool result mapping | `crates/vp-core/src/preemption.rs` |
| Run loop, phases, finalization, checkpoint | `crates/vp-core/src/agent.rs` |
| Shell process group and bounded output | `crates/vp-core/src/tools.rs` |
| Browser/computer RPC and GUI lease release | `crates/vp-core/src/browser.rs`, `computer.rs` |
| LSP `$/cancelRequest` | `crates/vp-core/src/tooling/lsp.rs` |
| MCP `notifications/cancelled` / bounded abandon | `crates/vp-core/src/mcp.rs`, `mcp/stdio.rs`, `mcp/http.rs` |
| HTTP and fetch abort | `crates/vp-core/src/tooling/http.rs`, `tools.rs` |
| Policy persistence and manifest | `crates/vp-store/src/store.rs`, `vp-core/src/rebuild.rs` |
| RPC, metrics, CLI, UI | `vp-daemon/src/main.rs`, `metrics.rs`, `vp-cli/src/main.rs`, Swift Stage/Manage Dash |

## Operational examples

An operator preempts a stalled run:

```bash
vp preempt 01J... --reason "provider stream stalled after a deploy"
```

The RPC result acknowledges the signal; it does not claim the run already
finished:

```json
{
  "session_id": "01J...",
  "preempted": true,
  "reason": "explicit",
  "detail": "preemption signalled during provider; the run will stop after bounded cleanup"
}
```

The terminal event is the audit record:

```json
{
  "session_id": "01J...",
  "project_id": "01H...",
  "agent_id": "01G...",
  "agent_name": "Leader",
  "agent_role": "leader",
  "reason": "explicit",
  "phase": "provider",
  "tool": null,
  "elapsed_ms": 1005,
  "side_effects_unknown": false,
  "partial_input_tokens": 26486,
  "partial_output_tokens": 41,
  "partial_output_bytes": 0,
  "usage_estimated": true,
  "checkpoint_id": "01J...",
  "task_checkpoint_commit": null,
  "terminal": true,
  "created_at": "2026-11-12T00:00:00Z"
}
```

Inspect history and project totals:

```bash
vp preemption 01J...
vp preemptions 01H... --limit 50
```

Change the policy without changing cooperative pause/cancel:

```bash
vp preemption-policy-set 01H... \
  --tool-grace-ms 1500 \
  --kill-escalation-ms 500 \
  --tool-deadline-secs 30
```

Disable automatic wall-clock preemption (explicit preemption still works):

```bash
vp preemption-policy-set 01H... --disabled
```

## Failure modes

| Situation | Behavior |
| --- | --- |
| Provider ignores stream cancellation | the runtime drops the response and records the partial turn; no retry |
| Provider closes without usage | runtime estimates partial usage and marks `usage_estimated` |
| Shell child escapes its process group | local group kill cannot reach it; output/status may be incomplete |
| Tool has unknown side effects | `side_effects_unknown: true`; no automatic retry |
| Browser/computer helper is mid-command | helper is shut down; lease is released; snapshot refs are not reused |
| LSP/MCP ignores cancellation | bounded abandon; the request is not replayed |
| Checkpoint fails | event still records preemption with `checkpoint_id: null` |
| Worker task worktree snapshot fails | event still records preemption with `task_checkpoint_commit: null` |
| Daemon restarts mid-preemption | in-flight run is interrupted; no exactly-once external effect is claimed |
| Policy is disabled | wall-clock and tool deadlines are off; explicit operator preemption still applies |

## Test matrix

The H3 tests cover:

- token idempotence, concurrent waiters, parent/child propagation, deadline
  update/clear, spurious notification handling, grace copying, side-effect
  stickiness, bounded detail, and serde round-trip;
- OpenAI and Anthropic slow SSE streams, retry-backoff abort, partial reported
  usage, estimated partial usage, and no retry/replay;
- explicit provider preemption, wall-clock deadline, a healthy run finishing
  before its deadline, disabled policy, paused-run preemption, queued/admission
  preemption, and approval-queue preemption;
- session checkpoint creation, Worker task-worktree snapshot without moving the
  branch, and no merge-queue entry for preempted work;
- shell process-tree termination with no surviving pids, `local_http` abort,
  browser helper RPC cancel and lease release, LSP `$/cancelRequest`, MCP
  bounded abandon, process-wait abort, and per-tool deadline continuation;
- policy round-trip/clamping, manifest compatibility, database rebuild,
  terminal/phase/reason metrics, project overview grouping, protocol decode,
  CLI parsing, and Swift model decode;
- existing cancel/pause/timeout/retry/stream-partial tests remain green.

## Consequences

- Operators can stop a runaway stream/tool without waiting for a boundary, and
  the audit trail says exactly why and where.
- The runtime now has two stop paths; docs and UI must keep the distinction
  explicit. Pause/cancel remains the default.
- Partial provider output is visible but never replayed; resumed runs start a
  new provider call and see the interruption through the journal note.
- Tool failures gain stable `PREEMPTED` and `DEADLINE_EXCEEDED` codes. Unknown
  side effects fail closed and are never auto-retried.

## Alternatives considered

- **Poll the cooperative flag more often:** does not interrupt an in-flight
  HTTP/SSE read, shell tree, LSP request, or MCP call.
- **Wrap every tool in a hard timeout:** loses typed reasons/partial usage and
  cannot distinguish operator preemption from a deadline.
- **A scheduler that preempts "slow" runs:** rejected; it would be hidden
  policy and could kill healthy long tasks.
- **Exactly-once tool effects:** rejected as infeasible without transactional
  external services; the contract reports uncertainty instead.
- **Put the token only in `vp-core`:** would create a dependency cycle because
  `vp-providers` must accept it in the provider trait. The shared primitive is
  in `vp-providers` and re-exported by `vp-core`.

## Tests

- Provider: slow SSE preemption between chunks, retry-backoff abort, partial
  reported usage, no retry/replay.
- Runtime: explicit stream preemption, wall-clock deadline, disabled policy,
  checkpoint creation, terminal/tool event records, replay/rebuild history.
- Tools: shell process-tree SIGTERM/SIGKILL cleanup with no surviving pids,
  hanging `local_http` abort, browser helper RPC cancel and lease release, LSP
  `$/cancelRequest`, MCP bounded abandon, tool deadline non-terminal behavior.
- Protocol: policy defaults/clamps, record/result backward-compatible decode,
  metrics decode without new groups.
- CLI: `preempt`, `preemption`, and policy command parsing.
- Swift: model decode/defaults and snake_case round-trip.
- Existing cancel/pause/timeout/retry tests remain green.

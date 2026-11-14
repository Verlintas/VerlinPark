# 0218 — MCP asynchronous tasks and elicitation

- Status: Accepted
- Date: 2026-11-14
- Scope: C9 MCP task handles, explicit task inspection, and form/URL elicitation

## Context

MCP servers can return a task handle from `tools/call` instead of a completed
result, and can issue `elicitation/create` requests while a call or task poll is
in flight. The existing client returned raw tool results, rejected every
server-initiated request with `-32601`, and had no durable local projection of
remote task handles. C9 makes async work explicit and inspectable without
adding a hidden polling loop or weakening MCP pin/approval gates.

## Decision

### Task handles and durable projection

- `tools/call` responses are inspected for a task handle: nested
  `{"task": {...}}` or a top-level `taskId`/`status` object. Supported ids are
  bounded, statuses normalize to `working`, `input_required`, `completed`,
  `failed`, `cancelled`, or `expired`, and `ttl`/`pollInterval` are bounded.
- A task is persisted only when the call has a real session context. The
  operator RPC without a session still returns the raw server response but does
  not create a local row. `mcp_tasks` stores owner/session/plan-task/server/
  tool/remote id, status, TTL, poll interval, timestamps, expiry, result hash/
  bytes, optional effect receipt id, and a bounded last error. Results are
  capped at 256 KiB. The unique key is `(session_id, server_id, remote_task_id)`.
- The daemon never polls automatically. `mcp.task.get`, `mcp.task.result`,
  `mcp.task.cancel`, and `mcp.task.list` are explicit. `get` performs one
  `tasks/get`; `result` performs one `tasks/result` unless the caller asks for
  cached/no-refresh; `cancel` performs one `tasks/cancel`; `list` is a local
  durable projection filtered by project/session/server/status. `input_required`
  is surfaced as `pending` with the server's poll interval and never spins.
- Task lifecycle is event-backed (`mcp.task.created`, `updated`, `completed`,
  `failed`, `cancelled`, `expired`, `result`) and mirrored through the normal
  session event stream. The result body is not duplicated into events; the
  event carries result hash/bytes and the optional effect receipt id.
- A mutating MCP call still creates the normal effect receipt. The task row
  references that receipt when available. Task completion is evidence only and
  **does not auto-settle** an `unknown` effect receipt; an operator must still
  reconcile it. No gate is fabricated for a remote task; waiting for user input
  is represented by the question inbox/elicitation path.

### Elicitation

- Transports accept one server-initiated method, `elicitation/create`, when a
  session-scoped handler is installed. All other server-initiated methods keep
  the conservative `-32601` response.
- Form elicitation is converted to a normal session question (question inbox)
  with a bounded message and a compact field summary. The answer is parsed as a
  JSON object when possible; a single-field schema gets typed coercion
  (`boolean`, `integer`, `number`, string). The returned MCP result is
  `{"action":"accept","content":...}`, or `decline`/`cancel` on failure/timeout.
  The answer is capped at 16 KiB.
- URL elicitation (`mode=url` or a URL without a form schema) is denied without
  opening anything. The handler never launches a browser or fetches the URL.
- `mcp.elicitation` audit events record server, tool, mode, action, bounded
  reason, and question id. They never record message, schema, or answer content.
  A form timeout defaults to 300s (`VERLINPARK_MCP_ELICITATION_TIMEOUT_SECS`,
  30–3600s) and closes the question as `timed_out`.
- Dynamic MCP server add/update/remove remains operator-driven and approval/
  policy-gated as before; this ADR adds no automatic server lifecycle.

## Security and gate preservation

- The generic `mcp` tool and dynamic MCP tools keep their existing approval,
  contract, pin-trust, capacity, budget, and preemption gates. Task polling is
  not a bypass: `tasks/*` requests use the same server connection and handler,
  and tool execution still requires a trusted pin.
- Task results are local operational state, not authorization. An unknown
  effect receipt still blocks merge; task result evidence cannot silently
  resolve it.
- URL elicitation is denied by default and never auto-opened. Elicitation text
  is treated as untrusted server input; no prompt injection is executed.

## Limits

- Task state is same-host SQLite plus session events, not a distributed task
  service and not rebuilt from bundles/`db-rebuild`; the result body is stored
  in the local task row and is excluded from event payloads.
- No hidden or scheduled polling, no push task notifications, no cross-session
  task sharing/claiming, and no automatic effect reconciliation.
- Task handle detection and status normalization are heuristic across MCP
  server variants; a server that does not return a recognizable handle is just
  a synchronous call.
- Form elicitation is a single question; complex nested schemas may require the
  server to accept a JSON answer. URL mode is deliberately unavailable.
- MCP tasks and elicitation are not sandbox boundaries. HTTP/SSE servers remain
  trusted external endpoints; stdio binaries remain trusted user software.

## Verification

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`: 3,932 passed, 0 failed
- New tests cover task handle parsing/normalization, durable task CRUD/result
  bounds, explicit get/result/list/cancel, TTL expiry, event kinds, form
  question routing, URL denial, and CLI parsing.

## Related

- ADR-0193 (MCP tool pins/contracts)
- ADR-0194 (effect receipts)
- ADR-0195 (external gates)
- ADR-0217 (tool search v2)
- `docs/CONTRACTED_EXECUTION_PLAN.md` C9

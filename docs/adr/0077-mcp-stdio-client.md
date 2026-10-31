# ADR-0077: MCP stdio client

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

The agent toolset was fixed in the daemon. MCP is the common way to expose
external tools, but there was no configuration or client path.

## Decision

1. **Global server configs.** MCP servers are stored with name, command, args,
   env, enabled, and timestamps. CRUD is exposed through `mcp.server.*`, CLI, and
   Settings → MCP.
2. **Persistent stdio transport.** The daemon keeps one process per configured
   server. On first use it performs the MCP `initialize` handshake plus
   `notifications/initialized`; later `tools/list` and `tools/call` requests reuse
   the same process. Calls to one server are serialized because stdio carries a
   single JSON-RPC stream.
3. **Lifecycle.** Connections are torn down after `VERLINPARK_MCP_IDLE_SECS`
   (default 300) without use, on config update/delete, or when the process exits.
   `tools/list` may reconnect once because it is idempotent; `tools/call` is never
   retried because it may have side effects. Process groups are killed on drop so
   child processes do not survive the daemon.
4. **Bounded I/O.** 60-second operation timeout, 2 MiB response-line cap, JSON-RPC
   error reporting, and process-group cleanup.
5. **Environment allowlist.** Servers inherit only PATH/HOME/USER/LANG/TMPDIR/TZ
   plus their configured env; provider/daemon secrets are not inherited.
6. **Agent access.** A generic `mcp` tool lists servers/tools or calls a tool.
   `call` requires approval unless the agent is auto-approved; `list` does not.
7. **Pagination.** `tools/list`, `resources/list`, `resources/templates/list`, and
   `prompts/list` follow `nextCursor` up to 50 pages / 2,000 items and reject
   repeated cursors.
8. **Dynamic tool exposure.** Leader, Worker, and Labour runs discover enabled
   servers before the first turn and expose up to 32 MCP tools as
   `mcp_<server>_<tool>` schemas (sanitized names, 400-character descriptions,
   original `inputSchema`). Managers and Verifiers do not receive dynamic tools
   to preserve their read-only policy; they can still use the generic `mcp` tool.
   Dynamic calls require approval unless the run is explicitly auto-approved.
   Collisions and oversized names are skipped/truncated.
9. **Trust boundary.** Configured MCP servers run as local processes outside the
   shell sandbox. Only trusted commands should be added; env values are stored in
   the local database, not Keychain.
10. **Resources and prompts.** `resources/list`, `resources/templates/list`,
    `resources/read`, `prompts/list`, and `prompts/get` are exposed through the
    `mcp` agent tool, CLI, RPC, and Settings inspect menu. Resource text is
    returned to agents; binary resources report only URI, MIME type, and base64
    length. Prompt messages are returned as JSON content blocks. Reads never
    require approval; only `tools/call` does.

## Consequences

- MCP tools can be added without rebuilding or restarting the daemon.
- The first call to a server pays startup cost; later calls avoid `npx`/`uvx`
  launch latency, at the cost of idle child processes.
- A crashed server is replaced on the next call; an in-flight tool call that lost
  its response is reported as failed rather than retried.
- Environment secrets for a server are stored in SQLite; there is no Keychain
  reference path yet.
- Servers can read/do whatever their process permissions allow; MCP is not
  sandboxed.
- Sampling and server notifications are not handled. Binary resources are sized
  but not decoded or materialized.
- Dynamic schemas consume context and are discovered on every agent run (the MCP
  process is reused). There is no per-tool enable/disable or schema cache yet.

## Alternatives considered

- **Spawn per call:** simplest, but unacceptable startup latency for real MCP
  servers and loses server-side session state between calls.
- **Dynamic per-tool schemas:** better model ergonomics but requires schema
  refresh and conflict handling in every run.
- **Tools only:** simpler, but ignores resources/prompts that servers already
  expose for context and reusable instructions.
- **Only CLI access:** misses the point of agent tool use.

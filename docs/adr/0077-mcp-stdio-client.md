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
7. **Pagination.** `tools/list` follows `nextCursor` up to 50 pages / 2,000 tools
   and rejects repeated cursors.
8. **No dynamic schemas yet.** MCP tools are discovered at runtime instead of
   being injected as individual tool schemas. The agent must call `mcp` with
   `action=list` first.
9. **Trust boundary.** Configured MCP servers run as local processes outside the
   shell sandbox. Only trusted commands should be added; env values are stored in
   the local database, not Keychain.

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
- Resources, prompts, sampling, and server notifications are not handled.

## Alternatives considered

- **Spawn per call:** simplest, but unacceptable startup latency for real MCP
  servers and loses server-side session state between calls.
- **Dynamic per-tool schemas:** better model ergonomics but requires schema
  refresh and conflict handling in every run.
- **Only CLI access:** misses the point of agent tool use.

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
2. **Stdio transport.** Each operation spawns the configured command, performs
   the MCP `initialize` handshake plus `notifications/initialized`, then calls
   `tools/list` or `tools/call` and exits.
3. **Stateless MVP.** There is no persistent MCP process, reconnect, resource,
   prompt, sampling, or notification support.
4. **Bounded I/O.** 60-second operation timeout, 2 MiB response-line cap, JSON-RPC
   error reporting, and `kill_on_drop` cleanup.
5. **Environment allowlist.** Servers inherit only PATH/HOME/USER/LANG/TMPDIR/TZ
   plus their configured env; provider/daemon secrets are not inherited.
6. **Agent access.** A generic `mcp` tool lists servers/tools or calls a tool.
   `call` requires approval unless the agent is auto-approved; `list` does not.
7. **No dynamic schemas yet.** MCP tools are discovered at runtime instead of
   being injected as individual tool schemas. The agent must call `mcp` with
   `action=list` first.
8. **Trust boundary.** Configured MCP servers run as local processes outside the
   shell sandbox. Only trusted commands should be added; env values are stored in
   the local database, not Keychain.

## Consequences

- MCP tools can be added without rebuilding or restarting the daemon.
- Spawning per call adds startup latency (especially for `npx`/`uvx`).
- Environment secrets for a server are stored in SQLite; there is no Keychain
  reference path yet.
- Servers can read/do whatever their process permissions allow; MCP is not
  sandboxed.
- Pagination, resources, prompts, and server notifications are not handled.

## Alternatives considered

- **Persistent process manager:** lower latency but needs lifecycle, restart,
  stderr, and crash handling.
- **Dynamic per-tool schemas:** better model ergonomics but requires schema
  refresh and conflict handling in every run.
- **Only CLI access:** misses the point of agent tool use.

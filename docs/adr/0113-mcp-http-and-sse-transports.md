# ADR-0113: MCP HTTP and SSE transports

- **Status:** Accepted
- **Date:** 2026-11-08

## Context

The MCP client only supported stdio servers. That covered local commands but
not remote or containerized MCP servers, and the audit called out the missing
HTTP/SSE transport. The MCP specification now defines **Streamable HTTP**:
JSON-RPC is POSTed to one endpoint, responses may be JSON or SSE, and a server
may return an `Mcp-Session-Id` that must be sent on later requests. Older
servers still use the **legacy HTTP+SSE** shape: a long-lived SSE stream
announces a POST endpoint, and responses arrive on that stream.

Adding transports also exposed two reliability problems in the existing
manager: stdio ignored server-initiated requests instead of rejecting them, and
concurrent first connections could create duplicate transports. Both are fixed
as part of this work.

## Decision

1. **Explicit transport model.** `McpServer` gains `transport` (`stdio`,
   `http`, `sse`), `url`, and `headers`. `McpServerConfig` is the validated
   write shape used by RPC/CLI/store, and `McpTransport` defaults to `stdio` for
   backward compatibility. Transport-specific fields are exclusive: stdio uses
   `command`/`args`/`env`; HTTP/SSE use `url`/`headers`.
2. **Streamable HTTP.** Each request is a POST with JSON-RPC, `Accept:
   application/json, text/event-stream`, and `Content-Type: application/json`.
   The client accepts either a JSON body or an SSE response, captures
   `Mcp-Session-Id`, sends `MCP-Protocol-Version` after initialization, and
   closes with DELETE. Notifications accept 202/2xx without a response body.
3. **Legacy HTTP+SSE.** The client opens a GET SSE stream, requires an
   `endpoint` event, restricts that endpoint to the configured origin, POSTs
   JSON-RPC to it, and reads matching responses from the stream. A relative
   endpoint is resolved against the configured URL.
4. **One session per server.** `McpManager` keeps either a stdio process, a
   Streamable HTTP session, or a legacy SSE session. Calls to one server are
   serialized. Idle sweep and invalidation close the transport (process group
   kill, HTTP DELETE, or SSE stream drop). If two callers race to create the
   first session, the loser is closed rather than leaked.
5. **Server-initiated methods fail closed.** Stdio, HTTP, and SSE all answer
   unsupported server requests (sampling, roots, elicitation) with JSON-RPC
   `-32601` instead of silently ignoring them. Notifications are ignored with
   the same policy on every transport. This keeps servers from hanging while
   avoiding an unreviewed callback surface.
6. **Security boundaries.** Only `http`/`https` URLs are accepted. URLs with
   credentials or fragments are rejected. Redirects and the legacy SSE
   `endpoint` must stay on the configured origin. Reserved transport headers
   (`Accept`, `Content-Type`, `Mcp-Session-Id`, `MCP-Protocol-Version`, etc.)
   cannot be overridden. Sensitive header names (`Authorization`, `Cookie`,
   `X-API-Key`, etc.) must use `${ENV_VAR}` indirection; values are expanded
   from the daemon environment at connection time and are never logged or
   stored expanded. Request bodies and SSE events are capped at 8 MiB, with
   60-second operation timeouts and a 15-second connect timeout.
7. **Retry semantics.** Read-only list methods may reconnect and retry once on
   a transport failure, matching stdio. Tool calls are never retried because
   they may have side effects. Authentication/validation 4xx errors are not
   retried; 404/410 marks the session dead and reconnects on the next call.
8. **ACP propagation.** When an external ACP agent is started, stdio MCP
   servers are always passed; HTTP/SSE servers are passed only if the agent's
   `mcpCapabilities` advertise that transport. Headers are expanded before
   `session/new`, and unsupported transports are skipped rather than failing
   the whole delegation.
9. **Probe.** `mcp.server.probe` initializes a server, reports server name,
   version, negotiated protocol, tool count, and whether resources/prompts are
   supported. It is exposed in CLI and Settings, and uses the same normalized
   transport path as real calls.
10. **UI/CLI.** Settings can add, edit, probe, enable, and remove stdio, HTTP,
    and SSE servers. The CLI adds `--transport`, `--url`, `--header`,
    `vp mcp-update`, and `vp mcp-probe`.

## Consequences

- Remote MCP servers are usable without a local process; legacy SSE servers
  remain supported.
- Secrets for HTTP headers stay in the daemon environment instead of SQLite.
  Stdio `env` values are still stored in the local database; that pre-existing
  boundary is documented, not expanded.
- The client does not open a standalone server-notification stream, does not
  implement sampling/roots/elicitation, and does not materialize binary
  resources. These are explicit future surfaces.
- MCP HTTP/SSE servers are trusted endpoints with tool-call authority. The
  origin checks reduce redirect/SSRF pivots but do not sandbox a remote server.
- Stdio MCP processes remain outside the shell Seatbelt sandbox; users should
  only add trusted commands.

## Alternatives considered

- **Only Streamable HTTP:** simpler, but leaves older SSE servers unusable.
- **Store expanded header values:** required for some static-token servers, but
  violates the project's no-secrets-in-SQLite invariant.
- **Ignore server-initiated requests:** matches old behavior but can hang a
  server that waits for a reply.
- **Retry tool calls on transport failure:** risks duplicate side effects.
- **Implement sampling/roots now:** expands the untrusted callback surface
  before the local filesystem/terminal jail work is complete.

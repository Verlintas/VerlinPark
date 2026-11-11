# ADR-0165: ACP Streamable HTTP and WebSocket transport

- **Status:** Accepted
- **Date:** 2026-11-11
- **Owners:** Runtime, Protocol, Store, CLI, macOS
- **Related:** ADR-0137 (external ACP agents), ADR-0156 (remote workspaces),
  `docs/CONTEXT_CHECKPOINT_ACP_ANN_BACKUP_PLAN.md` D2,
  ACP RFD "Streamable HTTP & WebSocket Transport"

## Context

VerlinPark is an ACP **client**. External agents were reachable only as local
stdio child processes. That model is simple and gets strong local isolation:
the executable is spawned through the same Seatbelt write sandbox as shell
work, HOME/XDG paths are redirected to per-agent scratch, the worktree is
path-jailed, and the ACP permission requests run through the risk-based
approval center. It cannot reach an agent running in a container, on another
machine, or behind an HTTP load balancer.

The ACP RFD defines two remote profiles on one endpoint:

- **Streamable HTTP**: `POST` for client-to-server JSON-RPC; `initialize`
  returns `200` with a JSON body and `Acp-Connection-Id`; other posts return
  `202` immediately; one connection-scoped `GET` SSE stream and one
  session-scoped `GET` SSE stream per session carry server-to-client messages;
  `DELETE` terminates the connection.
- **WebSocket**: a `GET` upgrade on the same endpoint carrying one JSON-RPC
  message per text frame; `initialize` is the first message.

Two things make this different from "just add an HTTP client":

1. **Trust changes.** A remote agent does not run under the local Seatbelt
   sandbox, does not have a worktree jail, and may execute tools on a remote
   host. Treating it like a stdio agent would be a false safety claim.
2. **Message routing is transport-level.** Responses arrive on long-lived
   streams, not on the POST that sent the request. Server-initiated permission
   requests must be routed back with the right connection and session headers.
   A buffering or retry layer that guesses would either drop a permission
   prompt or duplicate a prompt.

The ACP v1 RFD deliberately does not provide in-flight replay or resumability.
Sessions can survive a disconnect, but server messages emitted while the client
was away are not re-delivered. This ADR accepts that limitation rather than
inventing a private resumption protocol.

## Decision

### 1. Additive configuration

`AcpAgent`, `AddAcpAgentParams`, `UpdateAcpAgentParams`, and the new
`AcpAgentConfig` add:

| Field | Default | Meaning |
|---|---|---|
| `transport` | `stdio` | `stdio`, `streamable_http`, or `websocket` |
| `endpoint` | `null` | `http(s)` for Streamable HTTP, `ws(s)` for WebSocket |
| `auth_header_env` | `null` | Environment variable **name** whose value is the `Authorization` header |
| `headers` | `{}` | Bounded extra header templates; values may use `${ENV_VAR}` |
| `connect_timeout_secs` | `30` | Connect/handshake/stream-open timeout, bounded 1–300 |
| `allow_remote` | `false` | Explicit opt-in required before any remote contact |

`command`, `args`, and `env` remain the stdio configuration. A remote agent
must have an empty command, no args, no process environment, and
`allow_remote=true`; a stdio agent may not set `endpoint`, `auth_header_env`,
`headers`, or `allow_remote`. Existing rows decode unchanged: SQLite migration
adds the columns with their defaults, and the protocol types use
`#[serde(default)]`.

`AcpAgentConfig` is the internal validated shape used by `vp-daemon`,
`vp-store`, the CLI, and tests. The old stdio-only engine/store helpers remain
as wrappers so existing callers and tests keep their behavior.

### 2. Validation and secret indirection

Validation is in `vp-core/src/acp/config.rs` and is repeated at connect time,
not only at add/update time. A hand-edited database or a direct library caller
must not be able to bypass the remote opt-in.

- Endpoint must parse as a URL, have a host, and use a scheme matching the
  profile (`http`/`https` or `ws`/`wss`).
- No userinfo, no fragment, no whitespace. Query strings are allowed, but
  credential-like keys (`token`, `api_key`, `password`, `secret`, `sig`, …)
  are rejected because endpoints are returned by list/probe RPCs and copied
  into the project manifest.
- `auth_header_env` is a valid environment-variable **name**, never a value.
  The value is read from the daemon environment when a connection opens and is
  used verbatim as `Authorization` (for example `Bearer …`). VerlinPark does
  not invent an auth scheme.
- Extra headers are limited to 32 entries, 128 bytes per name, 4096 bytes per
  value, and 32 KiB total. CR/LF/NUL are rejected.
- Transport-managed headers (`authorization`, `cookie`, `host`,
  `content-length`, `content-type`, `accept`, `upgrade`, `connection`,
  `acp-connection-id`, `acp-session-id`, …) cannot be overridden.
- `${ENV_VAR}` references are validated literally at rest and expanded only in
  memory. Missing variables fail the connection with the variable name; values
  are never persisted, returned in RPCs, or logged.

### 3. Transport adapter

`RemoteTransport` implements `ConnectTo<Client>` (the client-side component
type for `Client.builder().connect_with`) and bridges an ACP
`Channel`/`jsonrpcmsg::Message` to the RFD wire format. The existing ACP
client setup — initialize, `session/new`, MCP propagation, prompt, streaming
updates, permission request/response, cancellation — stays in `acp.rs`; only
the transport swapped. The driver owns:

- routing of outgoing messages to connection-scoped or session-scoped POSTs;
- a connection-scoped SSE pump and a session-scoped SSE pump per `sessionId`;
- correlation of JSON-RPC responses to pending request methods (used to learn
  when a `session/new` or `session/load` response creates a session);
- correlation of server-initiated request ids back to the session for the
  response header;
- cookie jar, explicit headers, bounded frame/event sizes, and cleanup.

#### Streamable HTTP

1. `initialize` is the first outgoing message. It is POSTed without
   `Acp-Connection-Id`, with `Accept: application/json` and
   `Content-Type: application/json`.
2. The server returns `200` with a JSON-RPC response. The connection id is read
   from the `Acp-Connection-Id` header, falling back to `result.connectionId` in
   the body. Missing id is a hard failure.
3. The client opens the connection-scoped `GET` with
   `Accept: text/event-stream` and `Acp-Connection-Id`, and waits for the HTTP
   response headers before forwarding the initialize response. This prevents a
   `session/new` response from racing the stream.
4. The ACP client sends `session/new` (connection-scoped). The POST returns
   `202`; the response arrives on the connection stream. Before forwarding it,
   the transport opens the session-scoped `GET` with both `Acp-Connection-Id`
   and `Acp-Session-Id`.
5. `session/prompt` and `session/cancel` POST with both headers and return
   `202`. Updates, server request_permission messages, and the prompt response
   arrive on the session stream.
6. A client response to a server request is POSTed as a JSON-RPC response with
   the originating session header. A cancel is a `session/cancel`
   notification.
7. On shutdown the client sends `DELETE` with `Acp-Connection-Id`. A
   best-effort guard also performs the DELETE from a detached task because the
   ACP SDK owns the transport future and may drop it as soon as the client
   callback returns.

The transport also accepts a `200` POST with a JSON-RPC body as a direct
response, because some early servers do not implement the `202` split. That is
a compatibility path, not a separate mode: it is routed through the same
response/session handling and is covered by tests.

#### WebSocket

A `GET` with the standard WebSocket upgrade headers is used, with configured
headers and cookies applied to the handshake. `initialize` must be the first
text frame. Every non-initialize message is one JSON-RPC object in one text
frame. Binary frames are ignored per the RFD. Client pings receive pongs. A
server `Close` is clean; read errors and malformed text fail the connection.

`wss` uses rustls with webpki roots. `ws` is accepted for localhost/development
endpoints; operators explicitly opt in with `allow_remote` either way.

### 4. Identity, cookies, and header forwarding

- `Acp-Connection-Id` binds a connection to its negotiated capabilities. It is
  required on every HTTP POST after initialize, on both GET streams, and on
  DELETE.
- `Acp-Session-Id` is required on session-scoped POSTs and the session GET.
  The transport derives it from `params.sessionId` for client requests, and
  from a recorded server request id for client responses. A method that the
  RFD requires to be session-scoped fails if no `sessionId` is present; the
  transport does not guess.
- A cookie jar stores `Set-Cookie` values from initialize and later responses
  and returns them on all subsequent HTTP requests, including SSE GETs. This
  supports sticky load balancers. WebSocket handshakes forward configured
  headers; v1 does not reconnect a WebSocket, so there is no later handshake on
  which a cookie could be replayed.
- `initialize` sends `Acp-Protocol-Version: 1`. Later POSTs and GETs also send
  it. The RFD marks this header as future work, so it is additive.

### 5. Method and stream routing matrix

The adapter derives routing from the JSON-RPC method and `params.sessionId`; it
does not inspect arbitrary payload fields.

| Message | Transport destination | Required headers |
|---|---|---|
| `initialize` | POST, direct 200 response | none before initialize |
| `session/new` | POST, 202; response on connection SSE | `Acp-Connection-Id` |
| `session/load` | POST, 202; response on connection SSE | connection + session |
| `session/prompt` | POST, 202; response on session SSE | connection + session |
| `session/cancel` | POST, 202 | connection + session |
| `session/set_mode` / `set_config_option` | POST, 202 | connection + session |
| client response to `session/request_permission` | POST, 202 | connection + session recorded from the request id |
| client response to a connection-level request | POST, 202 | connection only |
| server notification with `sessionId` | session SSE (or connection SSE if the server sends it there) | n/a |
| server notification without `sessionId` | connection SSE | n/a |
| `DELETE` | connection cleanup | connection |

Every inbound message is forwarded into the same ACP channel. The GET streams
are separate network resources, not separate protocol namespaces; this is why a
notification that a server routes on the connection stream is still delivered
correctly even if it belongs to a session. The adapter never silently creates a
session stream based on payload text: it opens one when a session-scoped
outgoing message names a new session id, or when a `session/new` response is
observed.

### 6. Bounds, timeout, and failure behavior

The transport is deliberately boring when something is wrong:

- One JSON-RPC message is capped at 16 MiB in both directions; one SSE event at
  8 MiB; one WebSocket message/frame at 16 MiB. Oversized data fails the
  connection instead of buffering without limit.
- SSE decoding is incremental and bounded by line and event size. Invalid
  UTF-8, malformed JSON, a JSON array, an empty message, an ambiguous
  JSON-RPC response (neither `result` nor `error`), or a missing event
  terminator beyond the bound fails with a bounded error.
- `connect_timeout_secs` bounds TCP/TLS setup, the initialize response, and the
  SSE GET response headers. A live stream has no idle timeout by default
  because a reasoning agent can legitimately be quiet; tests and embedders can
  set one internally. The existing ACP run timeout remains the outer bound.
- HTTP errors never include the server response body. A misconfigured proxy
  or malicious origin could reflect the submitted `Authorization` value; the
  status and request context are enough to diagnose the failure.
- Malformed frames, unknown connections/sessions, a connection stream that
  closes unexpectedly, and a failed DELETE are reported honestly. A failed
  DELETE is logged but does not retroactively fail a completed prompt.

### 7. Reconnect and session resumption (v1)

There is no background reconnect and no in-flight replay. A dropped transport
ends the run with an error; the runtime does not send the prompt again, because
that could duplicate side effects. `delegate_acp` accepts an optional
`session_id`; when the agent advertises `loadSession`, the run sends
`session/load` on a fresh connection instead of `session/new`. If the agent
does not advertise the capability, the run fails before sending the prompt.
VerlinPark does not silently create a new session in that case.

Sessions themselves are server-owned and survive disconnects per the RFD; the
client only promises to reuse one when `session/load` is available. Server
messages emitted while disconnected are not re-delivered; this is documented
rather than hidden by a cache.

### 8. Trust model

**Remote ACP agents are trusted external services and are not sandboxed.**

- The transport never spawns a process for a remote agent. The
  `command`/`args`/`env` fields are rejected for remote configuration.
- The local Seatbelt write sandbox, worktree write scope, HOME jail, and
  scratch HOME do not apply to remote execution. The agent may run tools on its
  own host with its own credentials.
- `delegate_acp` creates an isolated local task worktree for bookkeeping, but a
  remote prompt does not claim that the worktree was edited. Remote runs get a
  different prompt that says the agent has no access to the local worktree.
  If the remote agent produces no local commit, the Worker finishes with “no
  changes to merge”; it is not treated as a local code change.
- The first remote delegation in a run is classified as `HostState` and asks
  for operator approval. `unattended` mode denies remote delegation. A stdio
  delegation keeps its existing classification and sandbox.
- `allow_remote=true` is required both at configuration validation and again in
  `delegate_acp`. The transport re-checks it at connect time.
- Auth values and expanded headers stay in daemon memory. RPC list/detail,
  probe output, CLI, and Swift show the endpoint, transport, header **names**,
  and the `auth_header_env` variable name only.

## Consequences

- Remote agents now use the standard ACP transport instead of a private bridge.
  A server that supports only WebSocket can be selected with
  `transport=websocket`; a server that supports only HTTP SSE with
  `transport=streamable_http`. A probe that succeeds with one profile does not
  imply the other works.
- HTTP/2 is negotiated over TLS through rustls/ALPN when the server supports
  it. The in-process conformance server uses HTTP/1.1 on localhost because
  axum's plain listener does not speak h2 prior-knowledge; this is explicitly a
  test-environment limitation, not a protocol mode. The client still sends the
  same requests and uses one POST per message.
- The transport adds `reqwest` cookies/http2, `tokio-tungstenite` with rustls,
  and `futures-channel` to vp-core. No new daemon-wide state is introduced.
- Remote delegation is less automatic than stdio: it needs explicit
  configuration, explicit host-state approval, and honest failure when
  `session/load` is unavailable.
- `agent.spawned` records `acp_transport`, `acp_endpoint`, and
  `acp_sandboxed=false` for remote runs; `agent.message` records the transport,
  remote session id, and whether the run was resumed.
- Probe output adds transport, endpoint, `remote`, `load_session`, and a
  capability object (`load_session`, MCP HTTP/SSE, prompt image/audio/embedded
  context).

## Failure modes and operator guidance

| Symptom | Behavior |
|---|---|
| Wrong auth | `initialize` fails with HTTP 401/403; no body or secret echo |
| Endpoint is a WebSocket URL for HTTP profile | Validation rejects at add/update and connect |
| HTTP profile against an SSE-only server | POST/GET status or content-type validation fails explicitly |
| Server returns no `Acp-Connection-Id` | Initialize fails; no connection is guessed |
| `session/new` response races the GET | Transport opens the connection stream before forwarding initialize |
| Session stream 404 | Run fails; no prompt is sent |
| Malformed/oversized SSE | Connection fails with a bounded error |
| Connection drops mid-prompt | Run reports failure; no replay and no duplicate prompt |
| Agent lacks `session/load` | Resume fails before prompting; caller may start a new session explicitly |
| `allow_remote` false | Add/update validation and delegation refuse the agent |

## Configuration examples

### Stdio (unchanged)

```json
{
  "name": "gemini",
  "command": "npx",
  "args": ["-y", "@google/gemini-cli"],
  "env": {"GEMINI_API_KEY": "..."},
  "transport": "stdio",
  "timeout_secs": 1800
}
```

The transport fields may be omitted; the defaults are `stdio`, no endpoint, no
headers, 30-second connect timeout, and `allow_remote=false`.

### Streamable HTTP with an environment-backed Bearer token

```json
{
  "name": "remote-http",
  "command": "",
  "transport": "streamable_http",
  "endpoint": "https://agent.example.test/acp",
  "auth_header_env": "ACP_AGENT_TOKEN",
  "headers": {"x-project": "verlinpark"},
  "connect_timeout_secs": 20,
  "allow_remote": true,
  "timeout_secs": 1800
}
```

`ACP_AGENT_TOKEN` must contain the complete header value, for example
`Bearer eyJ…`. The expanded value lives only in the daemon process. The stored
row contains the variable name, not the token.

### WebSocket with a referenced custom header

```json
{
  "name": "remote-ws",
  "command": "",
  "transport": "websocket",
  "endpoint": "wss://agent.example.test/acp",
  "headers": {"x-tenant": "${ACP_TENANT}", "x-project": "demo"},
  "allow_remote": true
}
```

A missing `ACP_TENANT` fails the connection before any prompt is sent and names
the variable in the error; the value is never echoed.

## CLI examples

```bash
vp acp-add remote-http --transport streamable-http \
  --endpoint https://agent.example.test/acp \
  --auth-header-env ACP_AGENT_TOKEN \
  --allow-remote

vp acp-add remote-ws --transport websocket \
  --endpoint wss://agent.example.test/acp \
  --header 'x-tenant=${ACP_TENANT}' \
  --allow-remote --connect-timeout 15

vp acp-update remote-http --auto-approve --timeout 900
vp acp-probe remote-http --no-session
```

`acp-update` preserves unspecified fields. `--clear-endpoint`,
`--clear-auth-header-env`, and `--clear-headers` exist so an operator can switch
back to stdio without leaving remote-only configuration behind.

## Conformance test matrix

| RFD expectation | Test |
|---|---|
| initialize returns 200 + connection id | `streamable_http_runs_a_prompt_with_sse_routing_and_permission`, `streamable_http_accepts_a_body_only_connection_id` |
| missing connection id fails | `streamable_http_requires_a_connection_id_somewhere`, fault tests |
| non-initialize POST returns 202 | `streamable_http_runs_a_prompt_with_sse_routing_and_permission` |
| connection-scoped vs session-scoped SSE | `streamable_http_runs_a_prompt_with_sse_routing_and_permission`, `one_http_connection_hosts_multiple_sessions` |
| session/new response on connection stream | same, plus `streamable_http_accepts_a_direct_200_session_response` |
| permission request/response with session header | `streamable_http_runs_a_prompt_with_sse_routing_and_permission` |
| cancel | `streamable_http_cancels_a_running_prompt`, WS cancel |
| DELETE on close | `streamable_http_runs_a_prompt_with_sse_routing_and_permission`, delete-failure test |
| cookies on HTTP transports | `streamable_http_forwards_cookies_and_explicit_headers` |
| explicit auth/header forwarding | HTTP cookie/auth test, `websocket_forwards_authentication_and_explicit_headers` |
| wrong auth fails without echo | `streamable_http_rejects_wrong_authentication_without_echoing_it` |
| malformed SSE fails bounded | `streamable_http_rejects_a_malformed_sse_event`, fault tests |
| oversized SSE fails bounded | `oversized_sse_event_fails_with_a_bounded_error` |
| stream idle timeout | `idle_connection_stream_fails_with_a_bounded_error` |
| unknown session/stream | `streamable_http_reports_an_unknown_session_stream`, HTTP error tests |
| WebSocket handshake/initialize/prompt | `websocket_runs_a_prompt_with_permission_and_clean_close` |
| WebSocket text-only / binary ignored | WS prompt test sends a binary frame |
| WebSocket malformed/batch frames fail | `websocket_rejects_malformed_and_batch_server_frames` |
| WebSocket close | `websocket_reports_a_server_close_before_prompt` |
| no child process for remote | `remote_transport_does_not_spawn_the_stdio_command` |
| session/load resume | HTTP and WebSocket resume tests |
| probe reports transport/capabilities | probe tests |
| no session probe | `probe_can_skip_session_creation` |
| remote allow_remote refusal | engine validation test and `delegates_to_a_remote_acp_agent_with_host_state_approval` |
| remote host-state approval | `delegates_to_a_remote_acp_agent_with_host_state_approval` |
| stdio regression | existing `acp::tests::runs_a_prompt_and_streams_updates` and friends |

## Migration and rollout

1. Store migration adds columns with defaults. A row inserted by an older build
   is read as a stdio agent with a 30-second connect timeout and no headers.
2. A daemon restart is not required for existing agents; the new columns are
   additive and the old `add_acp_agent`/`update_acp_agent` library helpers still
   exist for older internal callers.
3. Operators should probe before delegating. A probe creates a session by
   default; `--no-session` is available for a transport-only check.
4. Remote agents are not enabled by merely adding a URL. Both the stored
   `allow_remote` flag and the per-delegation host-state approval are required.
5. Rollback is safe: an older daemon ignores unknown columns, and an older
   client sends stdio defaults. A remote row updated by an older client fails
   validation instead of silently becoming a local process.

## Alternatives considered

1. **Reuse MCP Streamable HTTP per-request SSE.** Rejected: the ACP RFD
   explicitly chooses long-lived connection/session streams and 202 POSTs.
   Per-request streams would multiply connections and make session routing
   ambiguous.
2. **Treat remote agents as sandboxed by wrapping them in a local proxy.**
   Rejected: a local proxy cannot constrain a remote host. It would only
   misrepresent the trust boundary.
3. **Automatic retry of a prompt after a dropped connection.** Rejected:
   prompts can have side effects; v1 has no message ids or resumption, so a
   retry is a duplicate, not a resume.
4. **Persist the auth header value for convenience.** Rejected: secrets stay in
   the environment or a Keychain-backed reference. Only the variable name is
   stored.
5. **Invent a custom resumption protocol with `Last-Event-ID`.** Rejected: it
   would not interoperate with the RFD's v1 semantics and would hide lost
   messages.
6. **Implement only WebSocket.** Rejected: the RFD requires both client
   profiles. Probe and selection distinguish them explicitly.

## Tests

- `vp-core/src/acp/config.rs`: endpoint schemes, userinfo/secret query
  rejection, env-name validation, header bounds/reserved names, `${ENV_VAR}`
  expansion, missing variables, redaction, and stdio passthrough.
- `vp-core/src/acp/transport/tests.rs`: SSE line/event bounds, CRLF/multiline
  parsing, chunk splits, UTF-8 rejection, JSON-RPC batch/malformed parsing,
  message-size bounds, session-header rules, and transport config opt-in.
- `vp-core/src/acp/transport_integration.rs`: in-process axum server
  conformance for initialize 200 + connection id, connection/session SSE
  routing, session/new and prompt 202s, permission request/response, cancel,
  DELETE, cookie and explicit-header/auth forwarding, wrong auth, malformed
  SSE, unknown session stream, probe, `session/load`, no child process spawn,
  and WebSocket handshake/initialize/prompt/permission/binary-ignore/close.
- `vp-protocol`: legacy stdio JSON decodes with defaults; remote JSON and
  probe capabilities decode additively.
- `vp-store`: remote transport fields round-trip through add/get/list/update.
- `vp-core` engine tests: remote config validation and stdio regression.
- Swift `ProtocolCompatibilityTests`: legacy ACP agent/probe decode, remote
  agent/probe decode, no secret fields, transport summary.
- Existing stdio ACP tests remain unchanged and green.

# `vp-a2a`

Loopback-only Agent2Agent (A2A) facade library for VerlinPark.

This crate deliberately contains **no local authority**. It parses and bounds
A2A messages, stores ephemeral tasks, serves a loopback HTTP/SSE surface, and
delegates all execution to a host callback. It never runs a shell, reads a
file, fetches a URL, dispatches a VerlinPark tool, approves an approval, or
merges a branch.

## Security posture

- Disabled by default; enabling is a host decision.
- Binds only `127.0.0.1` or `::1`; public hosts, schemes, URLs and redirects are
  rejected.
- Exactly one bearer token. Comparisons use constant-time SHA-256 digests; the
  plaintext is redacted from `Debug`, never serialized, and available exactly
  once through `A2aStartHandle::take_token`.
- Bounded body (1 MiB), history (128 messages), parts (32), text/data (64 KiB),
  artifacts (256 KiB), skills (32), tasks (1,024), TTL (24 h), and SSE frames.
- No automatic POST retries after a request may have been accepted. A client
  can resubscribe explicitly; it never reconnects automatically.
- No push notifications, public listener, cross-host transport, or task
  durability.

## Server

```rust,no_run
use vp_a2a::{A2aPolicy, A2aRequestContext, A2aResult, A2aServer, A2aTaskUpdate, TaskState};

# async fn run() -> A2aResult<()> {
let server = A2aServer::start(
    A2aPolicy::new("verlinpark", "loopback facade", "0.1.0")
        .with_bearer_token("a-long-random-token")
        .unwrap()
        .with_listen("127.0.0.1:0")
        .unwrap()
        .enable(),
    |request: A2aRequestContext| async move {
        // The host callback is the only execution path. This example rejects.
        Ok::<_, vp_a2a::A2aError>(A2aTaskUpdate::new(TaskState::Rejected))
    },
)
.await?;
let token = server.take_token().expect("one-time token");
println!("{}", server.url());
server.shutdown().await?;
# Ok(())
# }
```

## Client

- `A2aClient::new(loopback_url, token)` rejects non-loopback URLs.
- `card`, `send`, `stream`, `get_task`, `cancel`, `resubscribe`.
- Client SSE streams are bounded and never auto-reconnect.

## Tests

```bash
cargo test -p vp-a2a
```

Covers card/policy validation, task state transitions, idempotency, bounds,
unauthorized clients, loopback-only binding, token redaction, SSE ordering,
terminal cancellation, and client/server round trips.

## Non-goals

- Inbound tool execution or implicit VerlinPark authority.
- Public A2A, push notifications, registry/federation, cross-host tasks.
- Durable task replay or exactly-once remote delivery.
- An OS sandbox for the host callback.

//! Loopback-only Agent2Agent (A2A) facade.
//!
//! This crate is deliberately transport- and policy-focused. It owns the
//! bounded wire model, an in-memory task projection, a loopback HTTP/SSE
//! server, and a matching client. It never grants local tool authority: an
//! inbound task must be authorized and executed by a host-supplied
//! [`A2aHandler`] callback.
//!
//! # Security posture
//!
//! * The facade is disabled by default and binds only `127.0.0.1` or `::1`.
//! * Every route requires one bearer token. Comparisons are constant-time over
//!   SHA-256 digests; the plaintext token is redacted from `Debug`, never
//!   serialized, and available only once from [`A2aStartHandle::take_token`].
//! * The served [`AgentCard::url`] is always the actual bound loopback URL.
//!   Decoding a card with a public, hostname-based, or otherwise non-loopback
//!   URL fails.
//! * Request bodies, messages, parts, artifacts, skills, task history, task
//!   count and lifetime are bounded.
//! * `message/send`, `message/stream`, `tasks/get`, `tasks/cancel`, and
//!   `tasks/resubscribe` are the supported JSON-RPC methods. `agent/card` is
//!   the HTTP GET `/.well-known/agent-card.json` resource, not JSON-RPC.
//!
//! # Non-goals
//!
//! This crate does not execute shell commands, access files, fetch URLs,
//! dispatch tools, proxy network traffic, or sandbox the host callback. Push
//! notifications are not implemented. Task state and events live in memory
//! and are lazily TTL-swept; there is no durable replay or cross-host
//! transport here. SSE frames are bounded and never automatically reconnected.
//!
//! The task-state vocabulary serializes the canonical underscore spellings
//! (`input_required`, `auth_required`) requested by this project and accepts
//! A2A's hyphenated spellings on input. Streaming responses carry raw
//! [`A2aStreamEvent`] frames; the client parser also accepts JSON-RPC
//! `result` wrappers from other A2A peers. `message/send` supports blocking
//! (default) and non-blocking (background host callback) modes.
//!
//! Shutdown closes the HTTP listener and aborts a stuck HTTP server after a
//! short grace period, but host callbacks are cooperative: they observe
//! [`A2aCancellation`] and are not force-killed by the facade.
//!
//! # Example
//!
//! ```no_run
//! use vp_a2a::{A2aClient, A2aHandler, A2aPolicy, A2aRequestContext, A2aServer, A2aTaskUpdate, TaskState};
//!
//! # async fn example() -> vp_a2a::A2aResult<()> {
//! let handler = |_request: A2aRequestContext| async {
//!     Ok::<_, vp_a2a::A2aError>(A2aTaskUpdate::new(TaskState::Completed))
//! };
//! let policy = A2aPolicy::new("example", "loopback example", "1.0.0")
//!     .with_bearer_token("example-token-0123456789")?
//!     .enable();
//! let server = A2aServer::start(policy, handler).await?;
//! let token = server.take_token().expect("one-time token");
//! let client = A2aClient::new(server.url(), token)?;
//! let card = client.card().await?;
//! assert!(card.url.starts_with("http://127.0.0.1:"));
//! server.shutdown().await?;
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

mod client;
mod error;
mod jsonrpc;
mod limits;
mod policy;
mod server;
mod sse;
mod store;
mod url;
mod wire;

pub use client::{A2aClient, A2aEventStream};
pub use error::{A2aError, A2aErrorCode, A2aResult};
pub use jsonrpc::{
    AGENT_CARD_PATH, JSONRPC_VERSION, JsonRpcError, JsonRpcId, JsonRpcRequest, JsonRpcResponse,
    METHOD_MESSAGE_SEND, METHOD_MESSAGE_STREAM, METHOD_TASKS_CANCEL, METHOD_TASKS_GET,
    METHOD_TASKS_RESUBSCRIBE, MessageSendConfiguration, RPC_PATH, SendMessageParams,
    TASK_EVENTS_PATH_PREFIX, TaskEventsQuery, TaskIdParams, TaskQueryParams, TaskResubscribeParams,
};
pub use limits::{
    A2aLimits, MAX_ARTIFACT_BYTES, MAX_ARTIFACTS_PER_TASK, MAX_BODY_BYTES, MAX_DATA_BYTES,
    MAX_FILE_PART_BYTES, MAX_ID_BYTES, MAX_MESSAGE_PARTS, MAX_METADATA_BYTES, MAX_PART_TEXT_BYTES,
    MAX_RETAINED_EVENTS, MAX_SKILLS, MAX_SSE_EVENT_BYTES, MAX_STORED_TASKS, MAX_STREAM_QUEUE,
    MAX_TASK_ARTIFACT_BYTES, MAX_TASK_HISTORY, MAX_TASK_HISTORY_BYTES, MAX_TASK_TTL, MAX_URL_BYTES,
};
pub use policy::{A2aPolicy, A2aSecretToken, AgentCardSpec};
pub use server::{A2aHandler, A2aRequestContext, A2aServer, A2aStartHandle};
pub use store::A2aCancellation;
pub use url::{
    A2aListenAddr, A2aScheme, A2aUrl, is_exact_loopback, is_loopback_url,
    validate_loopback_http_url,
};
pub use wire::{
    A2aEventFrame, A2aStreamEvent, A2aTask, A2aTaskUpdate, AGENT_PROTOCOL_VERSION,
    AgentCapabilities, AgentCard, AgentExtension, AgentProvider, AgentSkill, Artifact, FileContent,
    Message, MessageRole, Part, TaskArtifactUpdateEvent, TaskState, TaskStatus,
    TaskStatusUpdateEvent,
};

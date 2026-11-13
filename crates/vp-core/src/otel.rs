//! OpenTelemetry GenAI projection of the session event log (ADR-0197).
//!
//! This module is a **read-only projection**. It maps existing events to
//! bounded OTLP/HTTP JSON spans and sends them only when an operator explicitly
//! calls `telemetry.otel.export` (or `vp otel-export`). It never appends an
//! event, changes usage, touches a budget, or participates in approval,
//! Verifier, merge, capacity, or scheduling decisions.
//!
//! Security invariants:
//!
//! - only whitelisted metadata is exported: ids, roles, provider/model names,
//!   tool names, durations, status enums, counts, and token usage;
//! - prompts, goals, summaries, tool arguments, tool results, file content,
//!   raw errors, and secrets are never copied into a span attribute;
//! - attribute count, key length, value length, span count, body size, and
//!   request timeout are all bounded;
//! - the collector endpoint is loopback-only (`127.0.0.0/8`, `localhost`,
//!   `::1`) and credentials in the URL are rejected;
//! - export is disabled by default and has no background producer.
//!
//! Projection limits are explicit: the event window is capped at
//! [`MAX_EVENTS`], spans at the policy batch limit, and attributes at
//! [`MAX_ATTRIBUTES`] per span. A truncated window is reported, not hidden.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::time::Instant;

use anyhow::{Result, anyhow};
use reqwest::Url;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use vp_protocol::otel::{
    MAX_OTEL_BATCH_LIMIT, OtelExportResult, OtelPolicy, OtelPolicyResult, OtelProjectionCounts,
    OtelStatusResult, OtelTestResult, otel_codes,
};
use vp_protocol::types::Event;
use vp_store::Store;

use crate::Engine;

/// Maximum events examined by one status/export projection.
pub const MAX_EVENTS: usize = 5_000;
/// Maximum attributes attached to one span.
pub const MAX_ATTRIBUTES: usize = 24;
/// Maximum attribute-key characters.
pub const MAX_ATTRIBUTE_KEY_CHARS: usize = 96;
/// Maximum attribute-value characters.
pub const MAX_ATTRIBUTE_VALUE_CHARS: usize = 256;
/// Maximum span-name characters.
pub const MAX_SPAN_NAME_CHARS: usize = 160;
/// Maximum serialized OTLP request body accepted for one export.
pub const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
/// Stable scope name.
pub const SCOPE_NAME: &str = "verlinpark.genai";
/// Traces path appended to a base endpoint.
pub const TRACES_PATH: &str = "/v1/traces";

/// Typed OTel failure. RPC callers can downcast and surface the stable code in
/// `RpcError.data`; the message never contains a collector credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtelError {
    pub code: &'static str,
    pub message: String,
}

impl std::fmt::Display for OtelError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for OtelError {}

impl OtelError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn data(&self) -> Value {
        json!({
            "code": self.code,
            "message": self.message,
            "retryable": false,
        })
    }
}

pub fn otel_error(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    anyhow!(OtelError::new(code, message))
}

pub fn otel_error_code(error: &anyhow::Error) -> Option<&'static str> {
    error.downcast_ref::<OtelError>().map(|error| error.code)
}

/// A validated loopback collector endpoint. The stored value is a base URL;
/// [`ValidatedEndpoint::traces_url`] is the exact POST target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedEndpoint {
    pub base_url: String,
    pub traces_url: String,
    pub host: String,
}

/// Validate the configured endpoint. Only loopback hosts, HTTP(S), no
/// credentials, no query, no fragment, and a base path of `/` or
/// `/v1/traces` are accepted. This is an export destination, not a public
/// listener.
pub fn validate_endpoint(raw: &str) -> Result<ValidatedEndpoint> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(otel_error(
            otel_codes::OTEL_ENDPOINT_INVALID,
            "the OTel endpoint is empty",
        ));
    }
    let mut url = Url::parse(trimmed).map_err(|_| {
        otel_error(
            otel_codes::OTEL_ENDPOINT_INVALID,
            "the OTel endpoint is not a valid absolute URL",
        )
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(otel_error(
            otel_codes::OTEL_ENDPOINT_INVALID,
            "the OTel endpoint must use http or https",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(otel_error(
            otel_codes::OTEL_ENDPOINT_INVALID,
            "credentials are not allowed in the OTel endpoint",
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(otel_error(
            otel_codes::OTEL_ENDPOINT_INVALID,
            "query strings and fragments are not allowed in the OTel endpoint",
        ));
    }
    let host = url.host_str().unwrap_or_default().to_string();
    let bare_host = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(&host);
    let loopback = bare_host.eq_ignore_ascii_case("localhost")
        || bare_host
            .parse::<IpAddr>()
            .map(|address| address.is_loopback())
            .unwrap_or(false);
    if !loopback {
        return Err(otel_error(
            otel_codes::OTEL_ENDPOINT_NOT_LOOPBACK,
            "the OTel endpoint must be loopback (127.0.0.1, localhost, or ::1)",
        ));
    }
    let path = url.path().trim_end_matches('/');
    if !path.is_empty() && path != TRACES_PATH {
        return Err(otel_error(
            otel_codes::OTEL_ENDPOINT_INVALID,
            format!("the OTel endpoint path must be empty or {TRACES_PATH}"),
        ));
    }
    url.set_path(TRACES_PATH);
    url.set_query(None);
    url.set_fragment(None);
    let traces_url = url.to_string();
    Ok(ValidatedEndpoint {
        base_url: trimmed.to_string(),
        traces_url,
        host: bare_host.to_string(),
    })
}

/// Bounded attribute builder. Duplicate keys are ignored; values are stripped
/// of control characters and truncated. No caller can attach an unbounded
/// prompt or tool result through this type.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Attributes {
    entries: Vec<(String, Value)>,
    keys: HashSet<String>,
}

impl Attributes {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert_string(&mut self, key: &str, value: impl AsRef<str>) {
        self.insert(key, json!({"stringValue": sanitize(value.as_ref())}));
    }

    pub fn insert_bool(&mut self, key: &str, value: bool) {
        self.insert(key, json!({"boolValue": value}));
    }

    pub fn insert_int(&mut self, key: &str, value: i64) {
        self.insert(key, json!({"intValue": value.to_string()}));
    }

    /// Replace an existing value while preserving insertion order. Used only
    /// for a span that was first opened by `agent.spawned` and later refined by
    /// `agent.started` / retry or finish events.
    pub fn set_string(&mut self, key: &str, value: impl AsRef<str>) {
        self.set(key, json!({"stringValue": sanitize(value.as_ref())}));
    }

    pub fn set_int(&mut self, key: &str, value: i64) {
        self.set(key, json!({"intValue": value.to_string()}));
    }

    fn set(&mut self, key: &str, value: Value) {
        let key = sanitize(key);
        if let Some((_, existing)) = self
            .entries
            .iter_mut()
            .find(|(candidate, _)| candidate == &key)
        {
            *existing = value;
            return;
        }
        self.insert(&key, value);
    }

    fn insert(&mut self, key: &str, value: Value) {
        if self.entries.len() >= MAX_ATTRIBUTES {
            return;
        }
        let key = sanitize(key);
        if key.is_empty() || key.chars().count() > MAX_ATTRIBUTE_KEY_CHARS {
            return;
        }
        if !self.keys.insert(key.clone()) {
            return;
        }
        self.entries.push((key, value));
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn to_otlp(&self) -> Vec<Value> {
        self.entries
            .iter()
            .map(|(key, value)| {
                let mut object = serde_json::Map::new();
                object.insert("key".to_string(), Value::String(key.clone()));
                object.insert("value".to_string(), value.clone());
                Value::Object(object)
            })
            .collect()
    }

    pub fn value(&self, key: &str) -> Option<&Value> {
        self.entries
            .iter()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, value)| value)
    }
}

/// One projected span. Fields are intentionally metadata-only.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectedSpan {
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    /// 1 = internal, 3 = client.
    pub kind: u8,
    pub start_unix_nano: u64,
    pub end_unix_nano: u64,
    pub attributes: Attributes,
    /// 0 = unset, 1 = ok, 2 = error.
    pub status: u8,
}

impl ProjectedSpan {
    pub fn to_otlp(&self) -> Value {
        let mut span = serde_json::Map::new();
        span.insert("traceId".into(), json!(self.trace_id));
        span.insert("spanId".into(), json!(self.span_id));
        if let Some(parent) = &self.parent_span_id {
            span.insert("parentSpanId".into(), json!(parent));
        }
        span.insert("name".into(), json!(self.name));
        span.insert("kind".into(), json!(self.kind));
        span.insert(
            "startTimeUnixNano".into(),
            json!(self.start_unix_nano.to_string()),
        );
        span.insert(
            "endTimeUnixNano".into(),
            json!(self.end_unix_nano.to_string()),
        );
        span.insert("attributes".into(), Value::Array(self.attributes.to_otlp()));
        span.insert("status".into(), json!({"code": self.status}));
        Value::Object(span)
    }
}

/// Result of projecting one session event window.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectedSession {
    pub session_id: String,
    pub spans: Vec<ProjectedSpan>,
    pub counts: OtelProjectionCounts,
    pub last_event_seq: u64,
}

/// Project events into a bounded span set. Events must be ordered by `seq`;
/// the caller loads them from the store. Unknown event kinds are skipped and
/// counted. Nothing from a payload is exported unless a field is explicitly
/// read below.
pub fn project_events(session_id: &str, events: &[Event], truncated: bool) -> ProjectedSession {
    let trace_id = trace_id_for(session_id);
    let mut spans = Vec::new();
    let mut counts = OtelProjectionCounts {
        events_examined: events.len() as u64,
        truncated,
        ..OtelProjectionCounts::default()
    };
    let mut mapped = 0u64;

    let session_start = events
        .iter()
        .find(|event| event.kind == "session.created")
        .map(|event| nanos(&event.created_at))
        .or_else(|| events.first().map(|event| nanos(&event.created_at)))
        .unwrap_or(0);
    let session_end = events
        .iter()
        .map(|event| nanos(&event.created_at))
        .max()
        .unwrap_or(session_start)
        .max(session_start);

    let mut session_attributes = Attributes::new();
    session_attributes.insert_string("gen_ai.operation.name", "session");
    session_attributes.insert_string("gen_ai.conversation.id", session_id);
    session_attributes.insert_string("verlinpark.session.id", session_id);
    session_attributes.insert_int("verlinpark.session.event_count", events.len() as i64);
    session_attributes.insert_bool("verlinpark.session.truncated", truncated);
    if let Some(project_id) = events
        .iter()
        .find(|event| event.kind == "session.created")
        .and_then(|event| event.payload.get("project_id"))
        .and_then(Value::as_str)
    {
        session_attributes.insert_string("verlinpark.project.id", project_id);
    }
    let session_span = ProjectedSpan {
        trace_id: trace_id.clone(),
        span_id: span_id_for(session_id, 0, "session"),
        parent_span_id: None,
        name: "session".to_string(),
        kind: 1,
        start_unix_nano: session_start,
        end_unix_nano: session_end,
        attributes: session_attributes,
        status: 0,
    };
    let root_span_id = session_span.span_id.clone();
    spans.push(session_span);
    counts.session_spans = 1;
    mapped += 1;

    #[derive(Debug, Clone)]
    struct AgentTrack {
        span_index: usize,
        span_id: String,
        attempts: u32,
        retries: u32,
        started: bool,
        finished: bool,
        model: String,
        provider: String,
    }

    let mut agents: HashMap<String, AgentTrack> = HashMap::new();
    let mut tool_indexes: HashMap<String, usize> = HashMap::new();

    for event in events {
        match event.kind.as_str() {
            "session.created" => {}
            "agent.spawned" => {
                let Some(agent_id) = event.payload.get("agent_id").and_then(Value::as_str) else {
                    counts.events_skipped += 1;
                    continue;
                };
                if let Some(track) = agents.get(agent_id)
                    && !track.finished
                {
                    counts.events_skipped += 1;
                    continue;
                }
                let span = agent_span(&trace_id, &root_span_id, session_id, event, agent_id, 1, 0);
                let index = spans.len();
                let span_id = span.span_id.clone();
                spans.push(span);
                agents.insert(
                    agent_id.to_string(),
                    AgentTrack {
                        span_index: index,
                        span_id,
                        attempts: 1,
                        retries: 0,
                        started: false,
                        finished: false,
                        model: string_field(event, "model"),
                        provider: string_field(event, "provider"),
                    },
                );
                counts.agent_spans += 1;
                mapped += 1;
            }
            "agent.started" => {
                let Some(agent_id) = event.payload.get("agent_id").and_then(Value::as_str) else {
                    counts.events_skipped += 1;
                    continue;
                };
                if let Some(track) = agents.get_mut(agent_id)
                    && !track.finished
                {
                    if track.started {
                        track.attempts = track.attempts.saturating_add(1);
                    }
                    track.started = true;
                    track.model = string_field(event, "model");
                    track.provider = string_field(event, "provider");
                    spans[track.span_index]
                        .attributes
                        .set_int("verlinpark.agent.attempt", i64::from(track.attempts));
                    if !track.model.is_empty() {
                        spans[track.span_index]
                            .attributes
                            .set_string("gen_ai.request.model", &track.model);
                    }
                    if !track.provider.is_empty() {
                        spans[track.span_index]
                            .attributes
                            .set_string("gen_ai.provider.name", &track.provider);
                    }
                    mapped += 1;
                    continue;
                }
                let span = agent_span(&trace_id, &root_span_id, session_id, event, agent_id, 1, 0);
                let index = spans.len();
                let span_id = span.span_id.clone();
                spans.push(span);
                agents.insert(
                    agent_id.to_string(),
                    AgentTrack {
                        span_index: index,
                        span_id,
                        attempts: 1,
                        retries: 0,
                        started: true,
                        finished: false,
                        model: string_field(event, "model"),
                        provider: string_field(event, "provider"),
                    },
                );
                counts.agent_spans += 1;
                mapped += 1;
            }
            "agent.stream_retry" => {
                let Some(agent_id) = event.payload.get("agent_id").and_then(Value::as_str) else {
                    counts.events_skipped += 1;
                    continue;
                };
                if let Some(track) = agents.get_mut(agent_id) {
                    track.retries = track.retries.saturating_add(1);
                    spans[track.span_index]
                        .attributes
                        .set_int("verlinpark.agent.stream_retries", i64::from(track.retries));
                    mapped += 1;
                } else {
                    counts.events_skipped += 1;
                }
            }
            "agent.finished" => {
                let Some(agent_id) = event.payload.get("agent_id").and_then(Value::as_str) else {
                    counts.events_skipped += 1;
                    continue;
                };
                let Some(track) = agents.get_mut(agent_id) else {
                    counts.events_skipped += 1;
                    continue;
                };
                let status = event
                    .payload
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let span = &mut spans[track.span_index];
                span.end_unix_nano = nanos(&event.created_at).max(span.start_unix_nano);
                span.status = if status == "failed" { 2 } else { 1 };
                span.attributes
                    .insert_string("verlinpark.agent.status", status);
                if let Some(approved) = event.payload.get("approved").and_then(Value::as_bool) {
                    span.attributes
                        .insert_bool("verlinpark.agent.approved", approved);
                }
                track.finished = true;
                mapped += 1;
            }
            "tool.started" => {
                let Some(call_id) = event.payload.get("tool_call_id").and_then(Value::as_str)
                else {
                    counts.events_skipped += 1;
                    continue;
                };
                let name = string_field(event, "name");
                let agent_id = string_field(event, "agent_id");
                let parent = agents
                    .get(&agent_id)
                    .map(|track| track.span_id.clone())
                    .unwrap_or_else(|| root_span_id.clone());
                let mut attributes = Attributes::new();
                attributes.insert_string("gen_ai.operation.name", "execute_tool");
                attributes.insert_string("gen_ai.conversation.id", session_id);
                attributes.insert_string("gen_ai.tool.name", &name);
                attributes.insert_string("gen_ai.tool.call.id", call_id);
                if !agent_id.is_empty() {
                    attributes.insert_string("gen_ai.agent.id", &agent_id);
                }
                attributes
                    .insert_string("verlinpark.agent.role", string_field(event, "agent_role"));
                insert_identity_details(&mut attributes, event);
                let span = ProjectedSpan {
                    trace_id: trace_id.clone(),
                    span_id: span_id_for(session_id, event.seq, "tool"),
                    parent_span_id: Some(parent),
                    name: span_name("execute_tool", &name),
                    kind: 1,
                    start_unix_nano: nanos(&event.created_at),
                    end_unix_nano: nanos(&event.created_at),
                    attributes,
                    status: 0,
                };
                let index = spans.len();
                spans.push(span);
                tool_indexes.insert(call_id.to_string(), index);
                counts.tool_spans += 1;
                mapped += 1;
            }
            "tool.finished" => {
                let Some(call_id) = event.payload.get("tool_call_id").and_then(Value::as_str)
                else {
                    counts.events_skipped += 1;
                    continue;
                };
                let name = string_field(event, "name");
                let duration_ms = event
                    .payload
                    .get("duration_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    .min(86_400_000);
                let end = nanos(&event.created_at);
                let start = end.saturating_sub(duration_ms.saturating_mul(1_000_000));
                let is_error = event
                    .payload
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if let Some(index) = tool_indexes.get(call_id).copied() {
                    let span = &mut spans[index];
                    span.end_unix_nano = end.max(span.start_unix_nano);
                    if duration_ms > 0 {
                        span.start_unix_nano = span
                            .end_unix_nano
                            .saturating_sub(duration_ms.saturating_mul(1_000_000));
                    }
                    span.status = if is_error { 2 } else { 1 };
                    span.attributes
                        .insert_bool("verlinpark.tool.is_error", is_error);
                    insert_tool_details(&mut span.attributes, event);
                } else {
                    let agent_id = string_field(event, "agent_id");
                    let parent = agents
                        .get(&agent_id)
                        .map(|track| track.span_id.clone())
                        .unwrap_or_else(|| root_span_id.clone());
                    let mut attributes = Attributes::new();
                    attributes.insert_string("gen_ai.operation.name", "execute_tool");
                    attributes.insert_string("gen_ai.conversation.id", session_id);
                    attributes.insert_string("gen_ai.tool.name", &name);
                    attributes.insert_string("gen_ai.tool.call.id", call_id);
                    if !agent_id.is_empty() {
                        attributes.insert_string("gen_ai.agent.id", &agent_id);
                    }
                    attributes.insert_bool("verlinpark.tool.is_error", is_error);
                    insert_tool_details(&mut attributes, event);
                    insert_identity_details(&mut attributes, event);
                    let span = ProjectedSpan {
                        trace_id: trace_id.clone(),
                        span_id: span_id_for(session_id, event.seq, "tool"),
                        parent_span_id: Some(parent),
                        name: span_name("execute_tool", &name),
                        kind: 1,
                        start_unix_nano: start,
                        end_unix_nano: end.max(start),
                        attributes,
                        status: if is_error { 2 } else { 1 },
                    };
                    spans.push(span);
                    counts.tool_spans += 1;
                }
                mapped += 1;
            }
            "agent.usage" => {
                let agent_id = string_field(event, "agent_id");
                let role = string_field(event, "agent_role");
                let model = string_field(event, "model");
                let provider = agents
                    .get(&agent_id)
                    .map(|track| track.provider.clone())
                    .unwrap_or_default();
                let parent = agents
                    .get(&agent_id)
                    .map(|track| track.span_id.clone())
                    .unwrap_or_else(|| root_span_id.clone());
                let mut attributes = Attributes::new();
                attributes.insert_string("gen_ai.operation.name", "chat");
                attributes.insert_string("gen_ai.provider.name", provider);
                attributes.insert_string("gen_ai.request.model", &model);
                attributes.insert_string("gen_ai.response.model", &model);
                attributes.insert_string("gen_ai.conversation.id", session_id);
                if !agent_id.is_empty() {
                    attributes.insert_string("gen_ai.agent.id", &agent_id);
                    attributes
                        .insert_string("gen_ai.agent.name", string_field(event, "agent_name"));
                }
                attributes.insert_string("verlinpark.agent.role", role);
                insert_identity_details(&mut attributes, event);
                attributes.insert_int(
                    "gen_ai.usage.input_tokens",
                    int_field(event, "input_tokens"),
                );
                attributes.insert_int(
                    "gen_ai.usage.output_tokens",
                    int_field(event, "output_tokens"),
                );
                attributes.insert_int(
                    "gen_ai.usage.cache_read.input_tokens",
                    int_field(event, "cache_read_tokens"),
                );
                attributes.insert_int(
                    "gen_ai.usage.cache_write.input_tokens",
                    int_field(event, "cache_write_tokens"),
                );
                attributes.insert_bool(
                    "verlinpark.usage.estimated",
                    event
                        .payload
                        .get("estimated")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                );
                let at = nanos(&event.created_at);
                spans.push(ProjectedSpan {
                    trace_id: trace_id.clone(),
                    span_id: span_id_for(session_id, event.seq, "usage"),
                    parent_span_id: Some(parent),
                    name: span_name("chat", &model),
                    kind: 3,
                    start_unix_nano: at,
                    end_unix_nano: at,
                    attributes,
                    status: 1,
                });
                counts.usage_spans += 1;
                mapped += 1;
            }
            _ => {
                counts.events_skipped += 1;
            }
        }
    }

    counts.events_mapped = mapped;
    counts.total_spans = spans.len() as u64;
    let last_event_seq = events.iter().map(|event| event.seq).max().unwrap_or(0);
    ProjectedSession {
        session_id: session_id.to_string(),
        spans,
        counts,
        last_event_seq,
    }
}

/// Bounded span name. This is metadata, not user content, but it still must
/// not become an unbounded attribute carrier.
fn span_name(prefix: &str, value: &str) -> String {
    let value = sanitize(value);
    let mut name = format!("{prefix} {value}");
    if name.chars().count() > MAX_SPAN_NAME_CHARS {
        name = name.chars().take(MAX_SPAN_NAME_CHARS).collect();
    }
    name
}

fn insert_identity_details(attributes: &mut Attributes, event: &Event) {
    if let Some(identity_id) = event.payload.get("identity_id").and_then(Value::as_str)
        && !identity_id.is_empty()
    {
        attributes.insert_string("verlinpark.agent.identity_id", identity_id);
    }
    if let Some(generation) = event
        .payload
        .get("identity_generation")
        .and_then(Value::as_u64)
    {
        attributes.insert_int(
            "verlinpark.agent.identity_generation",
            i64::try_from(generation.min(i64::MAX as u64)).unwrap_or(i64::MAX),
        );
    }
}

fn insert_tool_details(attributes: &mut Attributes, event: &Event) {
    if let Some(duration) = event.payload.get("duration_ms").and_then(Value::as_u64) {
        attributes.insert_int(
            "verlinpark.tool.duration_ms",
            i64::try_from(duration.min(i64::MAX as u64)).unwrap_or(i64::MAX),
        );
    }
    if let Some(code) = event.payload.get("error_code").and_then(Value::as_str) {
        attributes.insert_string("verlinpark.tool.error_code", code);
    }
    if let Some(retryable) = event.payload.get("retryable").and_then(Value::as_bool) {
        attributes.insert_bool("verlinpark.tool.retryable", retryable);
    }
    if let Some(bytes) = event.payload.get("output_bytes").and_then(Value::as_u64) {
        attributes.insert_int(
            "verlinpark.tool.output_bytes",
            i64::try_from(bytes.min(i64::MAX as u64)).unwrap_or(i64::MAX),
        );
    }
    if let Some(truncated) = event.payload.get("truncated").and_then(Value::as_bool) {
        attributes.insert_bool("verlinpark.tool.truncated", truncated);
    }
    if let Some(risk) = event.payload.get("approval_risk").and_then(Value::as_str) {
        attributes.insert_string("verlinpark.tool.approval_risk", risk);
    }
}

fn agent_span(
    trace_id: &str,
    root_span_id: &str,
    session_id: &str,
    event: &Event,
    agent_id: &str,
    attempt: u32,
    retries: u32,
) -> ProjectedSpan {
    let role = string_field(event, "agent_role");
    let name = string_field(event, "agent_name");
    let display_name = if name.is_empty() {
        role.clone()
    } else {
        name.clone()
    };
    let mut attributes = Attributes::new();
    attributes.insert_string("gen_ai.operation.name", "invoke_agent");
    attributes.insert_string("gen_ai.conversation.id", session_id);
    attributes.insert_string("gen_ai.agent.id", agent_id);
    attributes.insert_string("gen_ai.agent.name", &display_name);
    attributes.insert_string("verlinpark.agent.role", &role);
    attributes.insert_string("gen_ai.provider.name", string_field(event, "provider"));
    attributes.insert_string("gen_ai.request.model", string_field(event, "model"));
    attributes.insert_int("verlinpark.agent.attempt", i64::from(attempt));
    attributes.insert_int("verlinpark.agent.stream_retries", i64::from(retries));
    attributes.insert_int("verlinpark.agent.depth", int_field(event, "agent_depth"));
    let task_id = string_field(event, "task_id");
    if !task_id.is_empty() {
        attributes.insert_string("verlinpark.agent.task_id", task_id);
    }
    let department = string_field(event, "department");
    if !department.is_empty() {
        attributes.insert_string("verlinpark.agent.department", department);
    }
    insert_identity_details(&mut attributes, event);
    ProjectedSpan {
        trace_id: trace_id.to_string(),
        span_id: span_id_for(session_id, event.seq, "agent"),
        parent_span_id: Some(root_span_id.to_string()),
        name: span_name("invoke_agent", &display_name),
        kind: 1,
        start_unix_nano: nanos(&event.created_at),
        end_unix_nano: nanos(&event.created_at),
        attributes,
        status: 0,
    }
}

fn string_field(event: &Event, key: &str) -> String {
    event
        .payload
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn int_field(event: &Event, key: &str) -> i64 {
    event
        .payload
        .get(key)
        .and_then(Value::as_u64)
        .map(|value| i64::try_from(value.min(i64::MAX as u64)).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn sanitize(value: &str) -> String {
    let mut sanitized = String::with_capacity(value.len().min(MAX_ATTRIBUTE_VALUE_CHARS));
    for (count, character) in value.chars().enumerate() {
        if count >= MAX_ATTRIBUTE_VALUE_CHARS {
            break;
        }
        if character.is_control() {
            sanitized.push(' ');
        } else {
            sanitized.push(character);
        }
    }
    sanitized.trim().to_string()
}

fn nanos(value: &str) -> u64 {
    OffsetDateTime::parse(value, &Rfc3339)
        .ok()
        .and_then(|time| u64::try_from(time.unix_timestamp_nanos()).ok())
        .unwrap_or(0)
}

fn trace_id_for(session_id: &str) -> String {
    let digest = Sha256::digest(format!("verlinpark-otel-trace:{session_id}").as_bytes());
    hex_prefix(&digest, 16)
}

fn span_id_for(session_id: &str, seq: u64, kind: &str) -> String {
    let digest =
        Sha256::digest(format!("verlinpark-otel-span:{session_id}:{seq}:{kind}").as_bytes());
    hex_prefix(&digest, 8)
}

fn hex_prefix(bytes: &[u8], count: usize) -> String {
    let mut out = String::with_capacity(count * 2);
    for byte in bytes.iter().take(count) {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

/// Build the OTLP/HTTP JSON request body for a projected session. The resource
/// carries only fixed build identity and the configured service name.
pub fn otlp_request(
    projected: &ProjectedSession,
    policy: &OtelPolicy,
    spans: &[ProjectedSpan],
) -> Value {
    let mut resource = Attributes::new();
    resource.insert_string("service.name", &policy.service_name);
    resource.insert_string("service.version", vp_protocol::APP_VERSION);
    resource.insert_string("telemetry.sdk.name", "verlinpark");
    resource.insert_string("telemetry.sdk.language", "rust");
    resource.insert_string("telemetry.sdk.version", vp_protocol::APP_VERSION);
    resource.insert_string("verlinpark.projection", "event_log");
    resource.insert_int(
        "verlinpark.projection.event_count",
        i64::try_from(projected.counts.events_examined.min(i64::MAX as u64)).unwrap_or(i64::MAX),
    );
    resource.insert_bool(
        "verlinpark.projection.truncated",
        projected.counts.truncated,
    );
    json!({
        "resourceSpans": [{
            "resource": {"attributes": resource.to_otlp()},
            "scopeSpans": [{
                "scope": {
                    "name": SCOPE_NAME,
                    "version": vp_protocol::APP_VERSION
                },
                "spans": spans.iter().map(ProjectedSpan::to_otlp).collect::<Vec<_>>()
            }]
        }]
    })
}

/// Load a bounded event window. `list_events` pages at 1,000 rows; the loop
/// stops at [`MAX_EVENTS`] and reports whether more rows likely exist.
pub async fn load_events(store: &Store, session_id: &str) -> Result<(Vec<Event>, bool)> {
    let mut events: Vec<Event> = Vec::new();
    let mut since_seq = 0u64;
    let mut truncated = false;
    loop {
        let store = store.clone();
        let session_id = session_id.to_string();
        let page =
            tokio::task::spawn_blocking(move || store.list_events(&session_id, since_seq, 1_000))
                .await??;
        if page.is_empty() {
            break;
        }
        let last_seq = page.last().map(|event| event.seq).unwrap_or(since_seq);
        events.extend(page);
        if events.len() >= MAX_EVENTS {
            truncated = true;
            break;
        }
        if last_seq < since_seq {
            break;
        }
        since_seq = last_seq.saturating_add(1);
    }
    events.sort_by_key(|event| event.seq);
    Ok((events, truncated))
}

impl Engine {
    pub async fn get_otel_policy(&self, project_id: &str) -> Result<OtelPolicy> {
        let store = self.store().clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking(move || store.get_otel_policy(&project_id)).await?
    }

    pub async fn set_otel_policy(
        &self,
        project_id: &str,
        policy: OtelPolicy,
    ) -> Result<OtelPolicyResult> {
        let policy = policy
            .normalized()
            .map_err(|error| otel_error(otel_codes::OTEL_POLICY_INVALID, error))?;
        validate_endpoint(&policy.endpoint)?;
        let store = self.store().clone();
        let project_id_owned = project_id.to_string();
        let saved = policy.clone();
        tokio::task::spawn_blocking(move || store.set_otel_policy(&project_id_owned, &saved))
            .await??;
        if let Err(error) = self.record_project_manifest().await {
            tracing::warn!(%error, "failed to record the project manifest after an OTel policy change");
        }
        let mut warnings = Vec::new();
        if !policy.enabled {
            warnings.push("export is disabled; no background exporter exists".to_string());
        }
        Ok(OtelPolicyResult {
            project_id: project_id.to_string(),
            policy,
            endpoint_valid: true,
            endpoint_loopback: true,
            warnings,
        })
    }

    pub async fn otel_status(
        &self,
        project_id: &str,
        session_id: Option<&str>,
    ) -> Result<OtelStatusResult> {
        let store = self.store().clone();
        let lookup = project_id.to_string();
        let project = tokio::task::spawn_blocking(move || store.get_project(&lookup)).await??;
        if project.is_none() {
            return Err(otel_error(
                otel_codes::OTEL_PROJECT_NOT_FOUND,
                format!("project {project_id} does not exist"),
            ));
        }
        let policy = self.get_otel_policy(project_id).await?;
        let endpoint = validate_endpoint(&policy.endpoint).ok();
        let mut warnings = Vec::new();
        let mut counts = OtelProjectionCounts::default();
        let mut last_event_seq = 0u64;
        if let Some(session_id) = session_id {
            let store = self.store().clone();
            let lookup = session_id.to_string();
            let session = tokio::task::spawn_blocking(move || store.get_session(&lookup)).await??;
            let session = session.ok_or_else(|| {
                otel_error(
                    otel_codes::OTEL_SESSION_NOT_FOUND,
                    format!("session {session_id} does not exist"),
                )
            })?;
            if session.project_id != project_id {
                return Err(otel_error(
                    otel_codes::OTEL_SESSION_NOT_FOUND,
                    "the session does not belong to the requested project",
                ));
            }
            let (events, truncated) = load_events(self.store(), session_id).await?;
            let projected = project_events(session_id, &events, truncated);
            counts = projected.counts;
            last_event_seq = projected.last_event_seq;
            if truncated {
                warnings.push(format!(
                    "the event window was truncated at {MAX_EVENTS} events; counts are for that window"
                ));
            }
        }
        if !policy.enabled {
            warnings.push("export is disabled (default); status remains read-only".to_string());
        }
        if let Some(endpoint) = &endpoint
            && !endpoint.traces_url.ends_with(TRACES_PATH)
        {
            warnings.push("the endpoint will be normalized to /v1/traces".to_string());
        }
        Ok(OtelStatusResult {
            project_id: project_id.to_string(),
            session_id: session_id.map(str::to_string),
            policy: policy.clone(),
            enabled: policy.enabled,
            endpoint_valid: endpoint.is_some(),
            endpoint_loopback: endpoint.is_some(),
            traces_path: endpoint
                .map(|endpoint| endpoint.traces_url)
                .unwrap_or_default(),
            counts,
            last_event_seq,
            generated_at: now_rfc3339(),
            warnings,
        })
    }

    /// Explicit one-shot export. No background loop calls this function.
    pub async fn otel_export(
        &self,
        session_id: &str,
        limit: Option<u32>,
    ) -> Result<OtelExportResult> {
        let store = self.store().clone();
        let lookup = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&lookup)).await??;
        let session = session.ok_or_else(|| {
            otel_error(
                otel_codes::OTEL_SESSION_NOT_FOUND,
                format!("session {session_id} does not exist"),
            )
        })?;
        let policy = self.get_otel_policy(&session.project_id).await?;
        let endpoint = validate_endpoint(&policy.endpoint)?;
        if !policy.enabled {
            return Err(otel_error(
                otel_codes::OTEL_DISABLED,
                "OTel export is disabled for this project; enable it explicitly before exporting",
            ));
        }
        let (events, events_truncated) = load_events(self.store(), session_id).await?;
        let projected = project_events(session_id, &events, events_truncated);
        let available = projected.spans.len() as u64;
        let cap = limit
            .unwrap_or(policy.batch_limit)
            .clamp(1, MAX_OTEL_BATCH_LIMIT) as usize;
        // Export the newest spans. Selecting the head made repeated limited
        // exports replay the same early spans forever and never reach later
        // agents/tools. A cursor is still absent, so `truncated` stays honest.
        let start = projected.spans.len().saturating_sub(cap);
        let selected = &projected.spans[start..];
        let truncated = events_truncated || start > 0;
        let mut result = OtelExportResult {
            session_id: session_id.to_string(),
            endpoint: endpoint.traces_url.clone(),
            available_spans: available,
            truncated,
            generated_at: now_rfc3339(),
            ..OtelExportResult::default()
        };
        if selected.is_empty() {
            result.status = "empty".to_string();
            result.error_code = Some(otel_codes::OTEL_EMPTY.to_string());
            result.message = Some("the session event window produced no exportable spans".into());
            return Ok(result);
        }
        let body = otlp_request(&projected, &policy, selected);
        let bytes = serde_json::to_vec(&body).map_err(|error| {
            otel_error(
                otel_codes::OTEL_EXPORT_FAILED,
                format!("could not serialize the OTLP request: {error}"),
            )
        })?;
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err(otel_error(
                otel_codes::OTEL_EXPORT_FAILED,
                format!(
                    "the OTLP request is {} bytes, above the {MAX_REQUEST_BYTES} byte limit",
                    bytes.len()
                ),
            ));
        }
        result.span_count = selected.len() as u64;
        result.request_bytes = bytes.len() as u64;
        let started = Instant::now();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(policy.timeout_secs))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .map_err(|error| {
                otel_error(
                    otel_codes::OTEL_EXPORT_FAILED,
                    format!("could not create the OTLP HTTP client: {error}"),
                )
            })?;
        match client
            .post(&endpoint.traces_url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(bytes)
            .send()
            .await
        {
            Ok(response) => {
                result.http_status = Some(response.status().as_u16());
                result.duration_ms = started.elapsed().as_millis() as u64;
                if response.status().is_success() {
                    result.exported = true;
                    result.status = "exported".to_string();
                } else {
                    result.status = "failed".to_string();
                    result.error_code = Some(otel_codes::OTEL_EXPORT_FAILED.to_string());
                    result.message = Some(format!(
                        "the local collector returned HTTP {}",
                        response.status().as_u16()
                    ));
                }
            }
            Err(error) => {
                result.duration_ms = started.elapsed().as_millis() as u64;
                result.status = "failed".to_string();
                result.error_code = Some(otel_codes::OTEL_EXPORT_FAILED.to_string());
                result.message = Some(bounded_error(&error));
            }
        }
        Ok(result)
    }

    /// Explicit connectivity test. It sends one synthetic span and never reads
    /// session content. The policy does not need to be enabled because this is
    /// an operator-initiated diagnostic, not a background export.
    pub async fn otel_test(&self, project_id: &str) -> Result<OtelTestResult> {
        let store = self.store().clone();
        let lookup = project_id.to_string();
        let project = tokio::task::spawn_blocking(move || store.get_project(&lookup)).await??;
        if project.is_none() {
            return Err(otel_error(
                otel_codes::OTEL_PROJECT_NOT_FOUND,
                format!("project {project_id} does not exist"),
            ));
        }
        let policy = self.get_otel_policy(project_id).await?;
        let endpoint = validate_endpoint(&policy.endpoint)?;
        let mut attributes = Attributes::new();
        attributes.insert_string("gen_ai.operation.name", "otel_test");
        attributes.insert_string("verlinpark.project.id", project_id);
        attributes.insert_bool("verlinpark.otel.test", true);
        let span = ProjectedSpan {
            trace_id: trace_id_for(&format!("otel-test:{project_id}")),
            span_id: span_id_for(project_id, 0, "otel-test"),
            parent_span_id: None,
            name: "verlinpark.otel.test".to_string(),
            kind: 1,
            start_unix_nano: nanos(&now_rfc3339()),
            end_unix_nano: nanos(&now_rfc3339()),
            attributes,
            status: 1,
        };
        let mut projected = ProjectedSession {
            session_id: String::new(),
            spans: vec![span.clone()],
            counts: OtelProjectionCounts {
                total_spans: 1,
                ..OtelProjectionCounts::default()
            },
            last_event_seq: 0,
        };
        projected.counts.events_examined = 0;
        let body = otlp_request(&projected, &policy, &[span]);
        let bytes = serde_json::to_vec(&body).map_err(|error| {
            otel_error(
                otel_codes::OTEL_EXPORT_FAILED,
                format!("could not serialize the OTel test request: {error}"),
            )
        })?;
        let mut result = OtelTestResult {
            project_id: project_id.to_string(),
            endpoint: endpoint.traces_url.clone(),
            request_bytes: bytes.len() as u64,
            span_count: 1,
            generated_at: now_rfc3339(),
            ..OtelTestResult::default()
        };
        let started = Instant::now();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(policy.timeout_secs))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .map_err(|error| {
                otel_error(
                    otel_codes::OTEL_EXPORT_FAILED,
                    format!("could not create the OTLP HTTP client: {error}"),
                )
            })?;
        match client
            .post(&endpoint.traces_url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(bytes)
            .send()
            .await
        {
            Ok(response) => {
                result.http_status = Some(response.status().as_u16());
                result.duration_ms = started.elapsed().as_millis() as u64;
                result.ok = response.status().is_success();
                if !result.ok {
                    result.error_code = Some(otel_codes::OTEL_EXPORT_FAILED.to_string());
                    result.message = Some(format!(
                        "the local collector returned HTTP {}",
                        response.status().as_u16()
                    ));
                }
            }
            Err(error) => {
                result.duration_ms = started.elapsed().as_millis() as u64;
                result.ok = false;
                result.error_code = Some(otel_codes::OTEL_EXPORT_FAILED.to_string());
                result.message = Some(bounded_error(&error));
            }
        }
        Ok(result)
    }
}

fn bounded_error(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        return "the local collector request timed out".to_string();
    }
    if error.is_connect() {
        return "the local collector refused the connection".to_string();
    }
    let mut message = error.to_string();
    message = message.chars().take(200).collect();
    message
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;
    use tempfile::TempDir;
    use vp_protocol::otel::{OtelExportParams, OtelStatusParams};
    use vp_protocol::types::Session;

    fn event(seq: u64, kind: &str, payload: Value, at: &str) -> Event {
        Event {
            session_id: "ses-1".to_string(),
            seq,
            kind: kind.to_string(),
            payload,
            created_at: at.to_string(),
        }
    }

    fn mapping_events() -> Vec<Event> {
        vec![
            event(
                1,
                "session.created",
                json!({
                    "goal": "SECRET_GOAL",
                    "project_id": "proj-1",
                    "worktree": "/tmp/SECRET_PATH"
                }),
                "2026-11-12T00:00:00.000Z",
            ),
            event(
                2,
                "agent.started",
                json!({
                    "agent_id": "agent-1",
                    "agent_name": "Leader",
                    "agent_role": "leader",
                    "agent_depth": 0,
                    "model": "model-1",
                    "provider": "provider-1",
                    "goal": "SECRET_GOAL",
                    "prompt": "SECRET_PROMPT"
                }),
                "2026-11-12T00:00:01.000Z",
            ),
            event(
                3,
                "agent.stream_retry",
                json!({
                    "agent_id": "agent-1",
                    "attempt": 1,
                    "phase": "stream",
                    "error": "SECRET_ERROR"
                }),
                "2026-11-12T00:00:02.000Z",
            ),
            event(
                4,
                "tool.started",
                json!({
                    "tool_call_id": "call-1",
                    "name": "write_file",
                    "agent_id": "agent-1",
                    "agent_role": "leader",
                    "input": {"path": "src/lib.rs", "content": "SECRET_TOOL_ARGS"}
                }),
                "2026-11-12T00:00:03.000Z",
            ),
            event(
                5,
                "tool.finished",
                json!({
                    "tool_call_id": "call-1",
                    "name": "write_file",
                    "agent_id": "agent-1",
                    "is_error": false,
                    "duration_ms": 25,
                    "output_bytes": 12,
                    "truncated": false,
                    "error_code": null,
                    "retryable": false,
                    "approval_risk": "workspace_write",
                    "output": "SECRET_TOOL_RESULT"
                }),
                "2026-11-12T00:00:04.000Z",
            ),
            event(
                6,
                "agent.usage",
                json!({
                    "agent_id": "agent-1",
                    "agent_name": "Leader",
                    "agent_role": "leader",
                    "model": "model-1",
                    "input_tokens": 100,
                    "output_tokens": 50,
                    "cache_read_tokens": 40,
                    "cache_write_tokens": 10,
                    "estimated": false
                }),
                "2026-11-12T00:00:05.000Z",
            ),
            event(
                7,
                "agent.finished",
                json!({
                    "agent_id": "agent-1",
                    "agent_role": "leader",
                    "agent_name": "Leader",
                    "status": "idle",
                    "summary": "SECRET_SUMMARY"
                }),
                "2026-11-12T00:00:06.000Z",
            ),
        ]
    }

    #[test]
    fn loopback_endpoint_policy_is_strict() {
        for accepted in [
            "http://127.0.0.1:4318",
            "http://localhost:4318/",
            "http://[::1]:4318/v1/traces",
            "https://127.0.0.1:4318",
        ] {
            let endpoint = validate_endpoint(accepted).unwrap();
            assert!(endpoint.traces_url.ends_with("/v1/traces"), "{accepted}");
        }
        for rejected in [
            "http://collector.example.com:4318",
            "https://8.8.8.8:4318",
            "http://user:secret@127.0.0.1:4318",
            "http://127.0.0.1:4318/?token=secret",
            "http://127.0.0.1:4318/custom/path",
            "ftp://127.0.0.1:4318",
            "not a url",
            "",
        ] {
            let error = validate_endpoint(rejected).unwrap_err();
            assert!(
                matches!(
                    otel_error_code(&error),
                    Some(otel_codes::OTEL_ENDPOINT_INVALID)
                        | Some(otel_codes::OTEL_ENDPOINT_NOT_LOOPBACK)
                ),
                "{rejected}: {error}"
            );
        }
    }

    #[test]
    fn projection_maps_ids_usage_duration_and_status_without_content() {
        let projected = project_events("ses-1", &mapping_events(), false);
        assert_eq!(projected.counts.session_spans, 1);
        assert_eq!(projected.counts.agent_spans, 1);
        assert_eq!(projected.counts.tool_spans, 1);
        assert_eq!(projected.counts.usage_spans, 1);
        assert_eq!(projected.counts.total_spans, 4);
        assert_eq!(projected.counts.events_examined, 7);
        assert_eq!(projected.counts.events_mapped, 7);
        assert_eq!(projected.last_event_seq, 7);

        let tool = projected
            .spans
            .iter()
            .find(|span| span.attributes.value("gen_ai.tool.name").is_some())
            .unwrap();
        assert_eq!(tool.status, 1);
        assert_eq!(
            tool.parent_span_id.as_deref(),
            Some(
                projected
                    .spans
                    .iter()
                    .find(|span| span.attributes.value("gen_ai.agent.id").is_some())
                    .unwrap()
                    .span_id
                    .as_str()
            )
        );
        assert_eq!(
            tool.end_unix_nano.saturating_sub(tool.start_unix_nano),
            25_000_000
        );
        assert_eq!(
            tool.attributes
                .value("verlinpark.tool.output_bytes")
                .and_then(|value| value.get("intValue"))
                .and_then(Value::as_str),
            Some("12")
        );
        assert_eq!(
            tool.attributes
                .value("verlinpark.tool.duration_ms")
                .and_then(|value| value.get("intValue"))
                .and_then(Value::as_str),
            Some("25")
        );
        assert_eq!(
            tool.attributes
                .value("verlinpark.tool.approval_risk")
                .and_then(|value| value.get("stringValue"))
                .and_then(Value::as_str),
            Some("workspace_write")
        );

        let agent = projected
            .spans
            .iter()
            .find(|span| span.name.starts_with("invoke_agent"))
            .unwrap();
        assert_eq!(
            agent
                .attributes
                .value("verlinpark.agent.stream_retries")
                .and_then(|value| value.get("intValue"))
                .and_then(Value::as_str),
            Some("1")
        );
        assert_eq!(
            agent
                .attributes
                .value("gen_ai.provider.name")
                .and_then(|value| value.get("stringValue"))
                .and_then(Value::as_str),
            Some("provider-1")
        );
        assert_eq!(
            agent
                .attributes
                .value("verlinpark.agent.attempt")
                .and_then(|value| value.get("intValue"))
                .and_then(Value::as_str),
            Some("1")
        );

        let usage = projected
            .spans
            .iter()
            .find(|span| span.name.starts_with("chat "))
            .unwrap();
        assert_eq!(usage.kind, 3);
        assert_eq!(usage.status, 1);
        for (key, expected) in [
            ("gen_ai.usage.input_tokens", "100"),
            ("gen_ai.usage.output_tokens", "50"),
            ("gen_ai.usage.cache_read.input_tokens", "40"),
            ("gen_ai.usage.cache_write.input_tokens", "10"),
        ] {
            assert_eq!(
                usage
                    .attributes
                    .value(key)
                    .and_then(|value| value.get("intValue"))
                    .and_then(Value::as_str),
                Some(expected),
                "{key}"
            );
        }
        assert_eq!(
            usage
                .attributes
                .value("gen_ai.provider.name")
                .and_then(|value| value.get("stringValue"))
                .and_then(Value::as_str),
            Some("provider-1")
        );

        let request = otlp_request(&projected, &OtelPolicy::default(), &projected.spans);
        assert!(
            request["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["startTimeUnixNano"]
                .is_string()
        );
        assert!(
            request["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["endTimeUnixNano"].is_string()
        );
        let serialized = serde_json::to_string(&request).unwrap();
        for secret in [
            "SECRET_GOAL",
            "SECRET_PROMPT",
            "SECRET_TOOL_ARGS",
            "SECRET_TOOL_RESULT",
            "SECRET_SUMMARY",
            "SECRET_ERROR",
            "SECRET_PATH",
        ] {
            assert!(!serialized.contains(secret), "leaked {secret}");
        }
        for field in ["\"input\"", "\"output\"", "\"summary\"", "\"prompt\""] {
            assert!(!serialized.contains(field), "leaked field {field}");
        }
    }

    #[test]
    fn attribute_and_span_names_are_bounded() {
        let mut attributes = Attributes::new();
        for index in 0..(MAX_ATTRIBUTES + 5) {
            attributes.insert_string(&format!("key.{index}"), "x".repeat(4_000));
        }
        assert_eq!(attributes.len(), MAX_ATTRIBUTES);
        for value in attributes.to_otlp() {
            let text = value["value"]["stringValue"].as_str().unwrap();
            assert!(text.chars().count() <= MAX_ATTRIBUTE_VALUE_CHARS);
        }
        let mut named = Attributes::new();
        named.insert_string(&"k".repeat(MAX_ATTRIBUTE_KEY_CHARS + 1), "value");
        assert!(named.is_empty());
        named.insert_string("control\nkey", "a\u{0000}b");
        let value = named.to_otlp();
        assert_eq!(value[0]["value"]["stringValue"], "a b");

        let event = event(
            1,
            "agent.started",
            json!({
                "agent_id": "a",
                "agent_name": "n".repeat(2_000),
                "agent_role": "leader",
                "model": "m".repeat(2_000),
                "provider": "p".repeat(2_000)
            }),
            "2026-11-12T00:00:00Z",
        );
        let projected = project_events("ses-1", &[event], false);
        let agent = &projected.spans[1];
        assert!(agent.name.chars().count() <= MAX_SPAN_NAME_CHARS + "invoke_agent ".len());
        assert!(
            agent
                .attributes
                .value("gen_ai.request.model")
                .and_then(|value| value.get("stringValue"))
                .and_then(Value::as_str)
                .unwrap()
                .chars()
                .count()
                <= MAX_ATTRIBUTE_VALUE_CHARS
        );
    }

    #[test]
    fn repeated_starts_use_attempt_and_error_status_is_bounded() {
        let events = vec![
            event(
                1,
                "agent.started",
                json!({"agent_id": "a", "agent_role": "worker", "agent_name": "W"}),
                "2026-11-12T00:00:00Z",
            ),
            event(
                2,
                "agent.started",
                json!({"agent_id": "a", "agent_role": "worker", "agent_name": "W"}),
                "2026-11-12T00:00:01Z",
            ),
            event(
                3,
                "tool.finished",
                json!({
                    "tool_call_id": "c",
                    "name": "shell",
                    "agent_id": "a",
                    "is_error": true,
                    "error_code": "COMMAND_FAILED",
                    "retryable": false,
                    "output_bytes": 2,
                    "output": "SECRET"
                }),
                "2026-11-12T00:00:02Z",
            ),
            event(
                4,
                "agent.finished",
                json!({"agent_id": "a", "agent_role": "worker", "status": "failed"}),
                "2026-11-12T00:00:03Z",
            ),
        ];
        let projected = project_events("ses-1", &events, false);
        let agent = projected
            .spans
            .iter()
            .find(|span| span.name.starts_with("invoke_agent"))
            .unwrap();
        assert_eq!(agent.status, 2);
        assert_eq!(
            agent
                .attributes
                .value("verlinpark.agent.attempt")
                .and_then(|value| value.get("intValue"))
                .and_then(Value::as_str),
            Some("2")
        );
        let tool = projected
            .spans
            .iter()
            .find(|span| span.name.starts_with("execute_tool"))
            .unwrap();
        assert_eq!(tool.status, 2);
        assert_eq!(
            tool.attributes
                .value("verlinpark.tool.error_code")
                .and_then(|value| value.get("stringValue"))
                .and_then(Value::as_str),
            Some("COMMAND_FAILED")
        );
    }

    fn init_repo(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        let run = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(path)
                    .status()
                    .unwrap()
                    .success(),
                "git {args:?}"
            );
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
        std::fs::write(path.join("README.md"), "otel\n").unwrap();
        run(&["add", "README.md"]);
        run(&["commit", "-q", "-m", "init"]);
    }

    struct Fixture {
        _data: TempDir,
        _repo: TempDir,
        engine: Engine,
        project_id: String,
        session: Session,
    }

    async fn fixture() -> Fixture {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let store = Store::open(data.path()).unwrap();
        let engine = Engine::new(store);
        let project = engine
            .store()
            .add_project(repo.path().to_str().unwrap(), Some("otel"))
            .unwrap();
        let session = engine
            .create_session(project.id.clone(), "otel".into(), None)
            .await
            .unwrap();
        for event in mapping_events() {
            engine
                .append_event(&session.id, &event.kind, event.payload)
                .await
                .unwrap();
        }
        Fixture {
            _data: data,
            _repo: repo,
            engine,
            project_id: project.id,
            session,
        }
    }

    fn code(error: &anyhow::Error) -> &'static str {
        otel_error_code(error).expect("typed otel error")
    }

    #[tokio::test]
    async fn export_is_disabled_by_default_and_rejects_public_endpoints() {
        let fixture = fixture().await;
        let policy = fixture
            .engine
            .get_otel_policy(&fixture.project_id)
            .await
            .unwrap();
        assert!(!policy.enabled);
        let error = fixture
            .engine
            .otel_export(&fixture.session.id, None)
            .await
            .unwrap_err();
        assert_eq!(code(&error), otel_codes::OTEL_DISABLED);

        let mut public = policy.clone();
        public.endpoint = "http://collector.example.com:4318".into();
        let error = fixture
            .engine
            .set_otel_policy(&fixture.project_id, public)
            .await
            .unwrap_err();
        assert_eq!(code(&error), otel_codes::OTEL_ENDPOINT_NOT_LOOPBACK);

        let status = fixture
            .engine
            .otel_status(&fixture.project_id, Some(&fixture.session.id))
            .await
            .unwrap();
        assert!(!status.enabled);
        assert!(status.endpoint_loopback);
        assert_eq!(status.counts.total_spans, 4);
        assert!(!status.counts.truncated);
    }

    #[tokio::test]
    async fn status_rejects_a_session_from_another_project() {
        let fixture = fixture().await;
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let other = fixture
            .engine
            .store()
            .add_project(repo.path().to_str().unwrap(), Some("other"))
            .unwrap();
        let error = fixture
            .engine
            .otel_status(&other.id, Some(&fixture.session.id))
            .await
            .unwrap_err();
        assert_eq!(code(&error), otel_codes::OTEL_SESSION_NOT_FOUND);
    }

    #[tokio::test]
    async fn explicit_export_posts_bounded_otlp_to_a_local_collector() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/traces"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(3)
            .mount(&mock)
            .await;
        let fixture = fixture().await;
        let mut policy = fixture
            .engine
            .get_otel_policy(&fixture.project_id)
            .await
            .unwrap();
        policy.enabled = true;
        policy.endpoint = mock.uri();
        policy.batch_limit = 4;
        let saved = fixture
            .engine
            .set_otel_policy(&fixture.project_id, policy)
            .await
            .unwrap();
        assert!(saved.endpoint_loopback);
        assert!(saved.policy.enabled);

        let before_events = fixture
            .engine
            .list_events(fixture.session.id.clone(), 0, 10_000)
            .await
            .unwrap();
        let result = fixture
            .engine
            .otel_export(&fixture.session.id, None)
            .await
            .unwrap();
        assert!(result.exported, "{result:?}");
        assert_eq!(result.status, "exported");
        assert_eq!(result.http_status, Some(200));
        assert_eq!(result.span_count, 4);
        assert_eq!(result.available_spans, 4);
        assert!(!result.truncated);
        assert!(result.request_bytes > 0);
        let bounded = fixture
            .engine
            .otel_export(&fixture.session.id, Some(2))
            .await
            .unwrap();
        assert!(bounded.exported);
        assert_eq!(bounded.span_count, 2);
        assert!(bounded.truncated);

        let requests = mock.received_requests().await.unwrap();
        let body = String::from_utf8(requests[0].body.clone()).unwrap();
        assert!(body.contains("gen_ai.usage.input_tokens"));
        assert!(body.contains("gen_ai.conversation.id"));
        for secret in [
            "SECRET_GOAL",
            "SECRET_PROMPT",
            "SECRET_TOOL_ARGS",
            "SECRET_TOOL_RESULT",
            "SECRET_SUMMARY",
            "SECRET_ERROR",
        ] {
            assert!(!body.contains(secret), "leaked {secret}");
        }
        assert!(!body.contains("collector.example.com"));

        let test = fixture.engine.otel_test(&fixture.project_id).await.unwrap();
        assert!(test.ok, "{test:?}");
        assert_eq!(test.http_status, Some(200));
        let requests = mock.received_requests().await.unwrap();
        assert_eq!(requests.len(), 3);
        let test_body = String::from_utf8(requests[2].body.clone()).unwrap();
        assert!(test_body.contains("verlinpark.otel.test"));
        assert!(!test_body.contains(&fixture.session.id));
        mock.verify().await;
        let after_events = fixture
            .engine
            .list_events(fixture.session.id.clone(), 0, 10_000)
            .await
            .unwrap();
        assert_eq!(before_events.len(), after_events.len());
        for (before, after) in before_events.iter().zip(after_events.iter()) {
            assert_eq!(before.seq, after.seq);
            assert_eq!(before.kind, after.kind);
        }
    }

    #[tokio::test]
    async fn disabled_policy_can_still_run_an_explicit_connectivity_test() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/traces"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&mock)
            .await;
        let fixture = fixture().await;
        let policy = OtelPolicy {
            endpoint: mock.uri(),
            ..OtelPolicy::default()
        };
        fixture
            .engine
            .set_otel_policy(&fixture.project_id, policy)
            .await
            .unwrap();
        let error = fixture
            .engine
            .otel_export(&fixture.session.id, None)
            .await
            .unwrap_err();
        assert_eq!(code(&error), otel_codes::OTEL_DISABLED);
        let result = fixture.engine.otel_test(&fixture.project_id).await.unwrap();
        assert!(!result.ok);
        assert_eq!(result.http_status, Some(503));
        assert_eq!(
            result.error_code.as_deref(),
            Some(otel_codes::OTEL_EXPORT_FAILED)
        );
    }

    #[tokio::test]
    async fn export_does_not_follow_a_collector_redirect() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/traces"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", "http://collector.example.com/v1/traces"),
            )
            .expect(1)
            .mount(&mock)
            .await;
        let fixture = fixture().await;
        let mut policy = fixture
            .engine
            .get_otel_policy(&fixture.project_id)
            .await
            .unwrap();
        policy.enabled = true;
        policy.endpoint = mock.uri();
        fixture
            .engine
            .set_otel_policy(&fixture.project_id, policy)
            .await
            .unwrap();
        let result = fixture
            .engine
            .otel_export(&fixture.session.id, None)
            .await
            .unwrap();
        assert!(!result.exported);
        assert_eq!(result.status, "failed");
        assert_eq!(result.http_status, Some(302));
        assert_eq!(
            result.error_code.as_deref(),
            Some(otel_codes::OTEL_EXPORT_FAILED)
        );
        mock.verify().await;
    }

    #[tokio::test]
    async fn policy_round_trips_and_manifest_records_it() {
        let fixture = fixture().await;
        let policy = OtelPolicy {
            enabled: true,
            service_name: "custom-service".into(),
            batch_limit: 50,
            timeout_secs: 3,
            ..OtelPolicy::default()
        };
        let result = fixture
            .engine
            .set_otel_policy(&fixture.project_id, policy.clone())
            .await
            .unwrap();
        assert_eq!(result.policy, policy);
        let stored = fixture
            .engine
            .get_otel_policy(&fixture.project_id)
            .await
            .unwrap();
        assert_eq!(stored, policy);
        let manifests = vp_store::read_project_manifest(fixture.engine.store().data_dir()).unwrap();
        assert_eq!(manifests[0].otel_policy, policy);
        let status_params = OtelStatusParams {
            project_id: fixture.project_id.clone(),
            session_id: None,
        };
        let status = fixture
            .engine
            .otel_status(&status_params.project_id, None)
            .await
            .unwrap();
        assert!(status.enabled);
        assert_eq!(status.counts.total_spans, 0);
    }

    #[tokio::test]
    async fn db_rebuild_restores_the_otel_policy_from_the_manifest() {
        let fixture = fixture().await;
        let project_id = fixture.project_id.clone();
        let data_dir = fixture.engine.store().data_dir().to_path_buf();
        let policy = OtelPolicy {
            enabled: true,
            service_name: "rebuild-service".into(),
            batch_limit: 25,
            ..OtelPolicy::default()
        };
        fixture
            .engine
            .set_otel_policy(&project_id, policy.clone())
            .await
            .unwrap();
        drop(fixture.engine);
        crate::rebuild::rebuild_database(&data_dir, false)
            .await
            .unwrap();
        let store = Store::open(&data_dir).unwrap();
        let engine = Engine::new(store);
        assert_eq!(engine.get_otel_policy(&project_id).await.unwrap(), policy);
    }

    #[test]
    fn identity_metadata_is_projected_without_prompt_or_tool_content() {
        let events = vec![
            event(
                1,
                "session.created",
                json!({"goal": "SECRET_GOAL", "project_id": "proj-1"}),
                "2026-11-12T00:00:00.000Z",
            ),
            event(
                2,
                "agent.spawned",
                json!({
                    "agent_id": "agent-1",
                    "agent_role": "worker",
                    "identity_id": "identity-1",
                    "identity_generation": 3,
                    "prompt": "SECRET_PROMPT",
                    "objective": "SECRET_OBJECTIVE"
                }),
                "2026-11-12T00:00:01.000Z",
            ),
            event(
                3,
                "tool.started",
                json!({
                    "tool_call_id": "call-1",
                    "name": "write_file",
                    "agent_id": "agent-1",
                    "identity_id": "identity-1",
                    "identity_generation": 3,
                    "input": {"content": "SECRET_TOOL_ARGS"}
                }),
                "2026-11-12T00:00:02.000Z",
            ),
            event(
                4,
                "agent.usage",
                json!({
                    "agent_id": "agent-1",
                    "agent_role": "worker",
                    "identity_id": "identity-1",
                    "identity_generation": 3,
                    "model": "model-1",
                    "input_tokens": 10,
                    "output_tokens": 2,
                    "prompt": "SECRET_PROMPT"
                }),
                "2026-11-12T00:00:03.000Z",
            ),
        ];
        let projected = project_events("ses-1", &events, false);
        let agent = projected
            .spans
            .iter()
            .find(|span| span.name.starts_with("invoke_agent"))
            .expect("agent span");
        assert_eq!(
            agent
                .attributes
                .value("verlinpark.agent.identity_id")
                .and_then(|value| value.get("stringValue"))
                .and_then(Value::as_str),
            Some("identity-1")
        );
        assert_eq!(
            agent
                .attributes
                .value("verlinpark.agent.identity_generation")
                .and_then(|value| value.get("intValue"))
                .and_then(Value::as_str),
            Some("3")
        );
        let tool = projected
            .spans
            .iter()
            .find(|span| span.name.starts_with("execute_tool"))
            .expect("tool span");
        assert_eq!(
            tool.attributes
                .value("verlinpark.agent.identity_id")
                .and_then(|value| value.get("stringValue"))
                .and_then(Value::as_str),
            Some("identity-1")
        );
        let usage = projected
            .spans
            .iter()
            .find(|span| span.name.starts_with("chat"))
            .expect("usage span");
        assert_eq!(
            usage
                .attributes
                .value("verlinpark.agent.identity_generation")
                .and_then(|value| value.get("intValue"))
                .and_then(Value::as_str),
            Some("3")
        );
        let body = serde_json::to_string(&otlp_request(
            &projected,
            &OtelPolicy::default(),
            &projected.spans,
        ))
        .unwrap();
        assert!(!body.contains("SECRET_PROMPT"));
        assert!(!body.contains("SECRET_GOAL"));
        assert!(!body.contains("SECRET_OBJECTIVE"));
        assert!(!body.contains("SECRET_TOOL_ARGS"));
        assert!(body.contains("identity-1"));
    }

    #[test]
    fn export_params_decode_without_limit() {
        let params: OtelExportParams =
            serde_json::from_value(json!({"session_id": "ses-1"})).unwrap();
        assert_eq!(params.session_id, "ses-1");
        assert!(params.limit.is_none());
    }
}

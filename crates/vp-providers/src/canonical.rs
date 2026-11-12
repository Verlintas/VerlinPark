//! Canonical conversation serialization and append-only continuity.
//!
//! Provider adapters must send the *same bytes* for every message that has
//! already appeared in an earlier request. This module owns that contract:
//!
//! - a deterministic canonical JSON writer (sorted object keys at every level,
//!   stable string escaping, no insignificant whitespace),
//! - a provider-aware message projection for OpenAI-compatible endpoints and
//!   Anthropic's Messages API,
//! - a hash chain over the ordered message list so two calls can be compared
//!   without persisting prompt text,
//! - bounded prefix-continuity classification (`append_only`, the first
//!   differing message index, and a typed break reason).
//!
//! The canonical form deliberately excludes transport metadata such as
//! Anthropic `cache_control` markers and provider-only reasoning fields. Those
//! values are not prompt content; moving a breakpoint or dropping
//! `reasoning_content` must not invalidate a cache prefix or make the
//! append-only accounting lie.
//!
//! Security and privacy: the canonical bytes and the per-message hashes are
//! computed in memory. Telemetry stores only hashes, message counts, roles, and
//! tool-call ids. It never stores message text.

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::{ChatMessage, ChatRequest, SystemSection};

/// Version of the canonical JSON and hash-chain format. Bump only for an
/// intentional, documented format change; an old hash never equals a new one.
pub const CANONICAL_VERSION: u32 = 1;

/// Maximum JSON nesting depth accepted by the canonical writer. Provider
/// message payloads are far below this. A pathological value is serialized
/// with a deterministic truncation marker instead of recursing without bound.
pub const MAX_CANONICAL_DEPTH: usize = 64;

/// Maximum number of per-message hash prefixes persisted by telemetry. The
/// full chain hash still covers every message; the stored list exists only to
/// locate the first difference when a break occurs.
pub const MAX_TELEMETRY_MESSAGE_HASHES: usize = 512;

/// Number of hex characters persisted for one per-message hash prefix.
pub const TELEMETRY_MESSAGE_HASH_CHARS: usize = 16;

/// Domain separator for the message hash chain.
const MESSAGE_CHAIN_DOMAIN: &str = "verlinpark.canonical.messages.v1";

/// Domain separator for the full request hash.
const REQUEST_DOMAIN: &str = "verlinpark.canonical.request.v1";

/// Domain separator for head and tail section hashes.
const SECTION_DOMAIN: &str = "verlinpark.canonical.sections.v1";

/// The provider wire format whose message projection is being canonicalized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProviderWire {
    OpenAiCompatible,
    Anthropic,
}

impl ProviderWire {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "openai-compatible",
            Self::Anthropic => "anthropic",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "openai-compatible" | "openai" | "openai_compatible" => Some(Self::OpenAiCompatible),
            "anthropic" | "claude" => Some(Self::Anthropic),
            _ => None,
        }
    }
}

/// Whether a provider accepts a system message after conversation history.
///
/// - `Supported`: the adapter sends a trailing `system` message.
/// - `Unsupported`: the adapter sends a stable user message prefixed
///   `[memory]`. This is the Anthropic path because its `system` field is a
///   top-level request parameter.
/// - `Unknown`: the adapter uses the conservative user form. A server that
///   accepts mid-history system messages still accepts user messages, so this
///   cannot reject the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailSystemCapability {
    Supported,
    Unsupported,
    Unknown,
}

impl TailSystemCapability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Unsupported => "unsupported",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "supported" => Some(Self::Supported),
            "unsupported" => Some(Self::Unsupported),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }

    /// `true` only when the adapter may place a `system` message after history.
    pub fn accepts_system(self) -> bool {
        matches!(self, Self::Supported)
    }

    /// The stable role used for tail memory when a trailing system message is
    /// not available.
    pub fn fallback_role(self) -> &'static str {
        "user"
    }
}

/// Provider capabilities that affect prompt layout, not model routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderCacheCapabilities {
    pub wire: ProviderWire,
    pub tail_system: TailSystemCapability,
}

impl ProviderCacheCapabilities {
    pub fn new(wire: ProviderWire, tail_system: TailSystemCapability) -> Self {
        Self { wire, tail_system }
    }

    pub fn openai_compatible() -> Self {
        Self::new(
            ProviderWire::OpenAiCompatible,
            TailSystemCapability::Supported,
        )
    }

    pub fn anthropic() -> Self {
        Self::new(ProviderWire::Anthropic, TailSystemCapability::Unsupported)
    }

    /// Infer capabilities from the configured provider name. The runtime only
    /// configures `openai-compatible` or `anthropic`; the extra names keep the
    /// helper useful for presets, tests, and future adapters without changing
    /// the default path.
    pub fn infer(provider_name: &str) -> Self {
        let normalized = provider_name.trim().to_ascii_lowercase();
        if normalized == "anthropic"
            || normalized.contains("claude")
            || normalized.contains("anthropic")
        {
            return Self::anthropic();
        }
        if normalized == "openai-compatible"
            || normalized == "openai"
            || normalized == "deepseek"
            || normalized.contains("openai")
            || normalized.contains("deepseek")
            || normalized.contains("openrouter")
            || normalized.contains("azure")
            || normalized.contains("ollama")
            || normalized.contains("lmstudio")
            || normalized.contains("vllm")
        {
            return Self::openai_compatible();
        }
        // An unknown OpenAI-compatible server may or may not accept a
        // mid-history system message. The user fallback is accepted by both
        // shapes, so it is the only safe deterministic choice.
        Self::new(
            ProviderWire::OpenAiCompatible,
            TailSystemCapability::Unknown,
        )
    }
}

/// Where the current tail memory was placed on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailSectionMode {
    /// No tail section was present.
    None,
    /// One or more tail sections were appended after history as system
    /// messages.
    TrailingSystem,
    /// One or more tail sections were appended after history as `[memory]`
    /// user messages.
    TrailingUser,
    /// The tail text was already present as a message in history (the
    /// append-only memory anchor). No duplicate suffix was added.
    Embedded,
}

impl TailSectionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::TrailingSystem => "trailing_system",
            Self::TrailingUser => "trailing_user",
            Self::Embedded => "embedded",
        }
    }
}

/// Typed reason for a non-append-only transition. These strings are part of
/// the telemetry contract and must stay stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheBreakReason {
    FirstCall,
    SystemChanged,
    ToolBundleChanged,
    MessageEdited,
    MessageReordered,
    CompactionReset,
    ProviderCacheMiss,
    Unknown,
}

impl CacheBreakReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FirstCall => "first_call",
            Self::SystemChanged => "system_changed",
            Self::ToolBundleChanged => "tool_bundle_changed",
            Self::MessageEdited => "message_edited",
            Self::MessageReordered => "message_reordered",
            Self::CompactionReset => "compaction_reset",
            Self::ProviderCacheMiss => "provider_cache_miss",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "first_call" => Some(Self::FirstCall),
            "system_changed" => Some(Self::SystemChanged),
            "tool_bundle_changed" => Some(Self::ToolBundleChanged),
            "message_edited" => Some(Self::MessageEdited),
            "message_reordered" => Some(Self::MessageReordered),
            "compaction_reset" => Some(Self::CompactionReset),
            "provider_cache_miss" => Some(Self::ProviderCacheMiss),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }

    /// `true` for reasons that describe a structural (pre-call) discontinuity.
    /// `ProviderCacheMiss` is post-call and does not mean our bytes changed.
    pub fn is_structural(self) -> bool {
        !matches!(self, Self::ProviderCacheMiss | Self::FirstCall)
    }
}

/// One deterministic message-sequence observation. Issues are diagnostic: the
/// runtime never reorders or drops messages to silence them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageSequenceIssue {
    pub index: usize,
    pub code: &'static str,
    pub detail: String,
}

impl MessageSequenceIssue {
    pub fn payload(&self) -> Value {
        serde_json::json!({
            "index": self.index,
            "code": self.code,
            "detail": self.detail,
        })
    }
}

/// Inspect a `ChatMessage` sequence for tool-call/tool-result ordering
/// problems. The check is provider-neutral and never mutates the input.
///
/// Codes:
/// - `orphan_tool_result`: a tool result does not answer a pending call.
/// - `duplicate_tool_call_id`: the same call id appears twice.
/// - `unanswered_tool_call`: a non-tool message (or the end of the list)
///   arrived while results were still pending.
/// - `tool_result_reordered`: results answered pending calls out of order.
/// - `empty_tool_call_id`: a tool call or result has no id.
pub fn audit_message_sequence(messages: &[ChatMessage]) -> Vec<MessageSequenceIssue> {
    const MAX_ISSUES: usize = 32;
    let mut issues = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let push = |issues: &mut Vec<MessageSequenceIssue>, index, code, detail: String| {
        if issues.len() < MAX_ISSUES {
            issues.push(MessageSequenceIssue {
                index,
                code,
                detail,
            });
        }
    };
    for (index, message) in messages.iter().enumerate() {
        if message.role == "tool" {
            let id = message.tool_call_id.clone().unwrap_or_default();
            if id.is_empty() {
                push(
                    &mut issues,
                    index,
                    "empty_tool_call_id",
                    "tool result has no id".into(),
                );
                continue;
            }
            if let Some(position) = pending.iter().position(|pending_id| pending_id == &id) {
                if position > 0 {
                    push(
                        &mut issues,
                        index,
                        "tool_result_reordered",
                        format!("result {id} answered before {} pending call(s)", position),
                    );
                }
                pending.remove(position);
            } else if seen.contains(&id) {
                push(
                    &mut issues,
                    index,
                    "orphan_tool_result",
                    format!("result {id} was already answered"),
                );
            } else {
                push(
                    &mut issues,
                    index,
                    "orphan_tool_result",
                    format!("result {id} does not match a pending call"),
                );
            }
            continue;
        }
        if !pending.is_empty() {
            push(
                &mut issues,
                index,
                "unanswered_tool_call",
                format!(
                    "{} call(s) still pending before role {}",
                    pending.len(),
                    message.role
                ),
            );
            pending.clear();
        }
        if let Some(calls) = &message.tool_calls {
            for call in calls {
                if call.id.is_empty() {
                    push(
                        &mut issues,
                        index,
                        "empty_tool_call_id",
                        "tool call has no id".into(),
                    );
                    continue;
                }
                if !seen.insert(call.id.clone()) {
                    push(
                        &mut issues,
                        index,
                        "duplicate_tool_call_id",
                        format!("call {} appears more than once", call.id),
                    );
                    continue;
                }
                pending.push(call.id.clone());
            }
        }
    }
    if !pending.is_empty() {
        push(
            &mut issues,
            messages.len(),
            "unanswered_tool_call",
            format!("{} call(s) have no result", pending.len()),
        );
    }
    issues
}

/// Reconstruct a canonical observation from a persisted event and compare it
/// with a request. This is the read-only verification path used by tests and
/// diagnostics; it never mutates a tracker and never edits a message.
pub fn verify_append_only(
    previous_payload: Option<&Value>,
    request: &ChatRequest,
    capabilities: ProviderCacheCapabilities,
    tools_hash: &str,
    compaction_pending: bool,
) -> ContinuityReport {
    let canonical = canonical_request(request, capabilities, tools_hash);
    let current = canonical.snapshot();
    let previous = previous_payload.and_then(MessagePrefixSnapshot::from_event_payload);
    let mut report = ContinuityReport::detect(previous.as_ref(), &current, compaction_pending);
    attach_difference_metadata(&mut report, &canonical);
    report
}

/// One provider-projected message list in canonical form.
#[derive(Debug, Clone)]
pub struct CanonicalRequest {
    pub version: u32,
    pub wire: ProviderWire,
    pub capabilities: ProviderCacheCapabilities,
    /// System/head messages as they conceptually precede the conversation.
    /// Anthropic transports these in its top-level `system` field, but the
    /// canonical identity is the same ordered message-shaped view.
    pub head_messages: Vec<Value>,
    /// Conversation messages, provider-projected.
    pub history_messages: Vec<Value>,
    /// Tail messages actually appended after history. Empty when the tail was
    /// already embedded in history or when no tail was present.
    pub tail_messages: Vec<Value>,
    pub tail_mode: TailSectionMode,
    pub head_prefix_hash: String,
    pub message_prefix_hash: String,
    pub tail_prefix_hash: String,
    pub request_prefix_hash: String,
    pub tools_hash: String,
    /// Full SHA-256 hex of each canonical history message, in order.
    pub message_hashes: Vec<String>,
    /// Bounded diagnostic ordering issues. They never change the bytes.
    pub sequence_issues: Vec<MessageSequenceIssue>,
    /// Canonical bytes of the history array (without the enclosing brackets are
    /// included for exact byte assertions).
    pub history_bytes: Vec<u8>,
    /// Canonical bytes of head + history + tail.
    pub request_bytes: Vec<u8>,
}

impl CanonicalRequest {
    pub fn message_count(&self) -> usize {
        self.history_messages.len()
    }

    pub fn canonical_request_bytes(&self) -> usize {
        self.request_bytes.len()
    }

    pub fn snapshot(&self) -> MessagePrefixSnapshot {
        MessagePrefixSnapshot::from_canonical(self)
    }
}

/// The bounded part of a canonical request that telemetry persists and a later
/// call reads back. `message_hashes` may be truncated to
/// [`MAX_TELEMETRY_MESSAGE_HASHES`]; `history_hash` always covers every
/// message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessagePrefixSnapshot {
    pub version: u32,
    pub wire: ProviderWire,
    pub message_count: usize,
    pub history_hash: String,
    pub message_hashes: Vec<String>,
    /// `true` only when `message_hashes` contains a full-length SHA-256 for
    /// every history message. Telemetry shortens and caps the list, so a
    /// decoded snapshot is normally incomplete; the chain hash still proves a
    /// prefix at an arbitrary message count.
    pub message_hashes_complete: bool,
    pub head_hash: String,
    pub tools_hash: String,
}

impl MessagePrefixSnapshot {
    /// Build a snapshot from a canonical request. The in-memory snapshot keeps
    /// every per-message hash so continuity checks are exact; only the
    /// telemetry `payload()` truncates the list.
    pub fn from_canonical(canonical: &CanonicalRequest) -> Self {
        Self {
            version: canonical.version,
            wire: canonical.wire,
            message_count: canonical.message_count(),
            history_hash: canonical.message_prefix_hash.clone(),
            message_hashes_complete: true,
            message_hashes: canonical.message_hashes.clone(),
            head_hash: canonical.head_prefix_hash.clone(),
            tools_hash: canonical.tools_hash.clone(),
        }
    }

    pub fn hashes_complete(&self) -> bool {
        self.message_hashes_complete && self.message_hashes.len() >= self.message_count
    }

    /// Bounded telemetry representation. Hashes are shortened to
    /// [`TELEMETRY_MESSAGE_HASH_CHARS`] characters and the list is capped; the
    /// full chain hash is kept intact.
    pub fn payload(&self) -> Value {
        let hashes = self
            .message_hashes
            .iter()
            .take(MAX_TELEMETRY_MESSAGE_HASHES)
            .map(|hash| {
                hash.chars()
                    .take(TELEMETRY_MESSAGE_HASH_CHARS)
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let complete = self.hashes_complete() && hashes.len() == self.message_count;
        serde_json::json!({
            "canonical_version": self.version,
            "provider_wire": self.wire.as_str(),
            "message_count": self.message_count,
            "message_prefix_hash": self.history_hash,
            "message_hashes": hashes,
            "message_hashes_complete": complete,
            "head_prefix_hash": self.head_hash,
            "tools_hash": self.tools_hash,
        })
    }

    /// Decode a snapshot from a persisted cache event. Returns `None` for
    /// legacy events that predate canonical continuity, so the caller records
    /// `first_call` rather than inventing a previous state.
    pub fn from_event_payload(payload: &Value) -> Option<Self> {
        let history_hash = payload
            .get("message_prefix_hash")
            .and_then(Value::as_str)
            .filter(|hash| !hash.is_empty())?;
        let message_count = payload
            .get("message_count")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let message_hashes = payload
            .get("message_hashes")
            .and_then(Value::as_array)
            .map(|hashes| {
                hashes
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let wire = payload
            .get("provider_wire")
            .and_then(Value::as_str)
            .and_then(ProviderWire::parse)
            .unwrap_or(ProviderWire::OpenAiCompatible);
        let message_hashes_complete = payload
            .get("message_hashes_complete")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            && message_hashes.len() >= message_count
            && message_hashes.iter().all(|hash| hash.len() == 64);
        Some(Self {
            version: payload
                .get("canonical_version")
                .and_then(Value::as_u64)
                .unwrap_or(1) as u32,
            wire,
            message_count,
            history_hash: history_hash.to_string(),
            message_hashes,
            message_hashes_complete,
            head_hash: payload
                .get("head_prefix_hash")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            tools_hash: payload
                .get("tools_hash")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        })
    }
}

/// Result of comparing the current canonical message list with the previous
/// observed list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinuityReport {
    pub append_only: bool,
    /// `None` when the previous list is an exact prefix of the current list.
    pub reason: Option<CacheBreakReason>,
    pub had_previous: bool,
    pub first_differing_index: Option<usize>,
    pub first_differing_role: Option<String>,
    pub first_differing_tool_call_id: Option<String>,
    pub previous_message_count: usize,
    pub message_count: usize,
    pub previous_message_prefix_hash: String,
    pub message_prefix_hash: String,
    pub previous_head_prefix_hash: String,
    pub head_prefix_hash: String,
    pub previous_tools_hash: String,
    pub tools_hash: String,
}

impl ContinuityReport {
    pub fn first_call(current: &MessagePrefixSnapshot) -> Self {
        Self {
            append_only: false,
            reason: Some(CacheBreakReason::FirstCall),
            had_previous: false,
            first_differing_index: None,
            first_differing_role: None,
            first_differing_tool_call_id: None,
            previous_message_count: 0,
            message_count: current.message_count,
            previous_message_prefix_hash: String::new(),
            message_prefix_hash: current.history_hash.clone(),
            previous_head_prefix_hash: String::new(),
            head_prefix_hash: current.head_hash.clone(),
            previous_tools_hash: String::new(),
            tools_hash: current.tools_hash.clone(),
        }
    }

    /// Compare two snapshots. `compaction_pending` is set by the runtime after
    /// a successful compaction and only wins when the list is not append-only;
    /// a compaction that did not change the active list is not a reset.
    pub fn detect(
        previous: Option<&MessagePrefixSnapshot>,
        current: &MessagePrefixSnapshot,
        compaction_pending: bool,
    ) -> Self {
        let Some(previous) = previous else {
            return Self::first_call(current);
        };
        let mut report = Self {
            append_only: false,
            reason: None,
            had_previous: true,
            first_differing_index: None,
            first_differing_role: None,
            first_differing_tool_call_id: None,
            previous_message_count: previous.message_count,
            message_count: current.message_count,
            previous_message_prefix_hash: previous.history_hash.clone(),
            message_prefix_hash: current.history_hash.clone(),
            previous_head_prefix_hash: previous.head_hash.clone(),
            head_prefix_hash: current.head_hash.clone(),
            previous_tools_hash: previous.tools_hash.clone(),
            tools_hash: current.tools_hash.clone(),
        };

        if previous.wire != current.wire {
            report.reason = Some(CacheBreakReason::SystemChanged);
            return report;
        }
        if previous.tools_hash != current.tools_hash {
            report.reason = Some(CacheBreakReason::ToolBundleChanged);
            return report;
        }
        if previous.head_hash != current.head_hash {
            report.reason = Some(CacheBreakReason::SystemChanged);
            return report;
        }
        if is_prefix(previous, current) {
            report.append_only = true;
            return report;
        }

        let (index, role, tool_call_id) = first_difference(previous, current);
        report.first_differing_index = index;
        report.first_differing_role = role;
        report.first_differing_tool_call_id = tool_call_id;
        if compaction_pending {
            report.reason = Some(CacheBreakReason::CompactionReset);
        } else if reordered(previous, current) {
            report.reason = Some(CacheBreakReason::MessageReordered);
        } else if index.is_some() {
            report.reason = Some(CacheBreakReason::MessageEdited);
        } else {
            report.reason = Some(CacheBreakReason::Unknown);
        }
        report
    }
}

fn is_prefix(previous: &MessagePrefixSnapshot, current: &MessagePrefixSnapshot) -> bool {
    if current.message_count < previous.message_count {
        return false;
    }
    if previous.message_count == 0 {
        return true;
    }
    if previous.hashes_complete() {
        return current
            .message_hashes
            .get(..previous.message_count)
            .is_some_and(|prefix| prefix == previous.message_hashes.as_slice());
    }
    // The persisted previous list was truncated. Its chain hash still commits
    // to every message, so a prefix check at the previous count is exact.
    chain_hash_at(&current.message_hashes, previous.message_count)
        .is_some_and(|hash| hash == previous.history_hash)
}

fn hash_matches(stored: &str, full: &str) -> bool {
    !stored.is_empty() && full.starts_with(stored)
}

fn first_difference(
    previous: &MessagePrefixSnapshot,
    current: &MessagePrefixSnapshot,
) -> (Option<usize>, Option<String>, Option<String>) {
    let limit = previous
        .message_hashes
        .len()
        .min(current.message_hashes.len());
    for index in 0..limit {
        if !hash_matches(
            &previous.message_hashes[index],
            &current.message_hashes[index],
        ) {
            return difference_at(previous, current, index);
        }
    }
    if previous.hashes_complete() && current.message_count > previous.message_count {
        // The compared prefix matched but the chain did not, which means the
        // stored prefix list was complete and the difference is beyond it.
        return difference_at(previous, current, previous.message_count);
    }
    if current.message_count != previous.message_count
        && limit == current.message_hashes.len()
        && previous.hashes_complete()
    {
        return difference_at(previous, current, current.message_count);
    }
    (None, None, None)
}

fn difference_at(
    _previous: &MessagePrefixSnapshot,
    _current: &MessagePrefixSnapshot,
    index: usize,
) -> (Option<usize>, Option<String>, Option<String>) {
    // The snapshot stores hashes, not message content. The role and tool id
    // metadata is attached by `describe_difference` when the caller still has
    // the canonical messages. This keeps the snapshot small and content-free.
    (Some(index), None, None)
}

/// Find the first differing index and attach safe metadata (role and tool-call
/// id) from the current canonical messages. No content is returned.
pub fn describe_difference(
    canonical: &CanonicalRequest,
    index: usize,
) -> (Option<String>, Option<String>) {
    let Some(message) = canonical.history_messages.get(index) else {
        return (None, None);
    };
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .map(str::to_string);
    let tool_call_id = message
        .get("tool_call_id")
        .and_then(Value::as_str)
        .or_else(|| {
            message
                .get("tool_calls")
                .and_then(Value::as_array)
                .and_then(|calls| calls.first())
                .and_then(|call| call.get("id"))
                .and_then(Value::as_str)
        })
        .map(str::to_string);
    (role, tool_call_id)
}

/// Refine a report with role/tool metadata from the current canonical list.
pub fn attach_difference_metadata(report: &mut ContinuityReport, canonical: &CanonicalRequest) {
    if let Some(index) = report.first_differing_index {
        let (role, tool_call_id) = describe_difference(canonical, index);
        report.first_differing_role = role;
        report.first_differing_tool_call_id = tool_call_id;
    }
}

fn reordered(previous: &MessagePrefixSnapshot, current: &MessagePrefixSnapshot) -> bool {
    let limit = previous
        .message_hashes
        .len()
        .min(current.message_hashes.len());
    let Some(first) = (0..limit).find(|index| {
        !hash_matches(
            &previous.message_hashes[*index],
            &current.message_hashes[*index],
        )
    }) else {
        return false;
    };
    // A pure reorder keeps every hash but moves at least one of the first
    // differing entries. Appends, edits, and compactions do not.
    let previous_hash = &previous.message_hashes[first];
    let current_hash = &current.message_hashes[first];
    let moved_from_previous = current
        .message_hashes
        .iter()
        .skip(first + 1)
        .any(|hash| hash_matches(previous_hash, hash));
    let moved_from_current = previous
        .message_hashes
        .iter()
        .skip(first + 1)
        .any(|hash| hash_matches(hash, current_hash));
    moved_from_previous || moved_from_current
}

/// Deterministic canonical JSON writer. Object keys are sorted at every depth;
/// strings use JSON escaping; numbers keep their serde_json representation.
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_value(value, &mut out, 0);
    out
}

fn write_value(value: &Value, out: &mut String, depth: usize) {
    if depth > MAX_CANONICAL_DEPTH {
        out.push_str("null");
        return;
    }
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(boolean) => out.push_str(if *boolean { "true" } else { "false" }),
        Value::Number(number) => out.push_str(&number.to_string()),
        Value::String(text) => write_string(text, out),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_value(item, out, depth + 1);
            }
            out.push(']');
        }
        Value::Object(object) => write_object(object, out, depth),
    }
}

fn write_object(object: &Map<String, Value>, out: &mut String, depth: usize) {
    let mut keys = object.keys().collect::<Vec<_>>();
    keys.sort_unstable();
    out.push('{');
    for (index, key) in keys.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        write_string(key, out);
        out.push(':');
        if let Some(value) = object.get(*key) {
            write_value(value, out, depth + 1);
        } else {
            out.push_str("null");
        }
    }
    out.push('}');
}

fn write_string(text: &str, out: &mut String) {
    // `serde_json` implements the JSON string escaping rules (including
    // control characters and surrogate pairs) deterministically.
    match serde_json::to_string(text) {
        Ok(quoted) => out.push_str(&quoted),
        Err(_) => out.push_str("\"\""),
    }
}

/// Remove provider transport metadata and provider-only reasoning fields from a
/// message value. Message text is never altered.
pub fn strip_volatile_fields(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut cleaned = Map::new();
            for (key, item) in object {
                if matches!(
                    key.as_str(),
                    "cache_control" | "reasoning_content" | "reasoning_details"
                ) {
                    continue;
                }
                cleaned.insert(key.clone(), strip_volatile_fields(item));
            }
            Value::Object(cleaned)
        }
        Value::Array(items) => Value::Array(items.iter().map(strip_volatile_fields).collect()),
        other => other.clone(),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn message_hash(message: &Value) -> String {
    sha256_hex(canonical_json(message).as_bytes())
}

/// Hash chain over an ordered list of per-message hashes. This commits to every
/// message and lets a later call prove that an earlier list is a prefix in
/// O(1) space using only the stored chain value and message count.
pub fn chain_hash(message_hashes: &[String]) -> String {
    let mut material =
        String::with_capacity(MESSAGE_CHAIN_DOMAIN.len() + 32 + message_hashes.len() * 66);
    material.push_str(MESSAGE_CHAIN_DOMAIN);
    material.push('\n');
    material.push_str(&message_hashes.len().to_string());
    for hash in message_hashes {
        material.push('\n');
        material.push_str(hash);
    }
    sha256_hex(material.as_bytes())
}

/// Chain hash for the first `count` entries. Returns `None` when the list is
/// shorter than `count`.
pub fn chain_hash_at(message_hashes: &[String], count: usize) -> Option<String> {
    if message_hashes.len() < count {
        return None;
    }
    Some(chain_hash(&message_hashes[..count]))
}

fn section_hash(kind: &str, messages: &[Value]) -> String {
    sha256_hex(
        format!(
            "{SECTION_DOMAIN}\n{kind}\n{}",
            canonical_json(&Value::Array(messages.to_vec()))
        )
        .as_bytes(),
    )
}

fn request_hash(
    wire: ProviderWire,
    head_hash: &str,
    history_hash: &str,
    tail_hash: &str,
    tools_hash: &str,
) -> String {
    sha256_hex(
        format!(
            "{REQUEST_DOMAIN}\n{}\n{}\n{}\n{}\n{}",
            wire.as_str(),
            head_hash,
            history_hash,
            tail_hash,
            tools_hash
        )
        .as_bytes(),
    )
}

/// Canonical JSON for one tool schema.
pub fn canonical_tool_json(tool: &crate::ToolSchema) -> String {
    canonical_tool_json_inner(tool)
}

fn canonical_tool_json_inner(tool: &crate::ToolSchema) -> String {
    // Field order intentionally matches the L3 `tool_bundle` canonical schema:
    // name, description, parameters, with recursively sorted parameter keys.
    // Tool parameters are user-defined schema data: transport-only field names
    // such as `cache_control` are legitimate properties here and must not be
    // stripped.
    let parameters = &tool.parameters;
    let mut out = String::new();
    out.push_str("{\"name\":");
    write_string(&tool.name, &mut out);
    out.push_str(",\"description\":");
    write_string(&tool.description, &mut out);
    out.push_str(",\"parameters\":");
    write_value(parameters, &mut out, 0);
    out.push('}');
    out
}

/// Canonical JSON for a tool list: sorted by name, first occurrence wins,
/// parameter keys recursively sorted.
pub fn canonical_tools_json(tools: &[crate::ToolSchema]) -> String {
    let mut sorted = tools.iter().collect::<Vec<_>>();
    sorted.sort_by(|left, right| left.name.cmp(&right.name));
    let mut seen = std::collections::BTreeSet::new();
    let rendered = sorted
        .into_iter()
        .filter(|tool| seen.insert(tool.name.as_str()))
        .map(canonical_tool_json_inner)
        .collect::<Vec<_>>();
    format!("[{}]", rendered.join(","))
}

/// SHA-256 of [`canonical_tools_json`].
pub fn canonical_tools_hash(tools: &[crate::ToolSchema]) -> String {
    sha256_hex(canonical_tools_json(tools).as_bytes())
}

fn head_messages(request: &ChatRequest) -> Vec<Value> {
    let sections = request
        .system_sections
        .iter()
        .filter(|section| section.has_text())
        .map(|section| {
            Value::Object(Map::from_iter([
                ("role".to_string(), Value::String("system".to_string())),
                ("content".to_string(), Value::String(section.text.clone())),
            ]))
        })
        .collect::<Vec<_>>();
    if !sections.is_empty() {
        return sections;
    }
    request
        .system
        .as_ref()
        .filter(|system| !system.is_empty())
        .map(|system| {
            vec![Value::Object(Map::from_iter([
                ("role".to_string(), Value::String("system".to_string())),
                ("content".to_string(), Value::String(system.clone())),
            ]))]
        })
        .unwrap_or_default()
}

/// A stable `[memory]` user message. The prefix is part of the canonical text
/// so a provider that rejects mid-history system messages receives exactly the
/// same conceptual section.
pub fn memory_user_content(text: &str) -> String {
    format!("[memory]\n\n{text}")
}

fn tail_value(section: &SystemSection, capabilities: ProviderCacheCapabilities) -> Value {
    if capabilities.tail_system.accepts_system() {
        Value::Object(Map::from_iter([
            ("role".to_string(), Value::String("system".to_string())),
            ("content".to_string(), Value::String(section.text.clone())),
        ]))
    } else {
        Value::Object(Map::from_iter([
            (
                "role".to_string(),
                Value::String(capabilities.tail_system.fallback_role().to_string()),
            ),
            (
                "content".to_string(),
                Value::String(memory_user_content(&section.text)),
            ),
        ]))
    }
}

pub(crate) fn openai_history_message(
    message: &ChatMessage,
    capabilities: ProviderCacheCapabilities,
) -> Value {
    if message.role == "system" && !capabilities.tail_system.accepts_system() {
        return Value::Object(Map::from_iter([
            ("role".to_string(), Value::String("user".to_string())),
            (
                "content".to_string(),
                Value::String(memory_user_content(&message.content)),
            ),
        ]));
    }
    let mut value = Map::new();
    value.insert("role".to_string(), Value::String(message.role.clone()));
    value.insert(
        "content".to_string(),
        Value::String(message.content.clone()),
    );
    if let Some(tool_calls) = &message.tool_calls {
        let calls = tool_calls
            .iter()
            .map(|call| {
                Value::Object(Map::from_iter([
                    ("id".to_string(), Value::String(call.id.clone())),
                    ("type".to_string(), Value::String("function".to_string())),
                    (
                        "function".to_string(),
                        Value::Object(Map::from_iter([
                            ("name".to_string(), Value::String(call.name.clone())),
                            (
                                "arguments".to_string(),
                                Value::String(call.arguments.clone()),
                            ),
                        ])),
                    ),
                ]))
            })
            .collect::<Vec<_>>();
        value.insert("tool_calls".to_string(), Value::Array(calls));
    }
    if let Some(tool_call_id) = &message.tool_call_id {
        value.insert(
            "tool_call_id".to_string(),
            Value::String(tool_call_id.clone()),
        );
    }
    Value::Object(value)
}

pub(crate) fn anthropic_history_message(message: &ChatMessage) -> Option<Value> {
    if message.role == "tool" {
        let tool_use_id = message.tool_call_id.clone().unwrap_or_default();
        return Some(Value::Object(Map::from_iter([
            ("role".to_string(), Value::String("user".to_string())),
            (
                "content".to_string(),
                Value::Array(vec![Value::Object(Map::from_iter([
                    ("type".to_string(), Value::String("tool_result".to_string())),
                    ("tool_use_id".to_string(), Value::String(tool_use_id)),
                    (
                        "content".to_string(),
                        Value::String(message.content.clone()),
                    ),
                ]))]),
            ),
        ])));
    }
    if message.role == "system" {
        // Anthropic's `system` field is top-level. A persisted memory anchor in
        // history is represented as a stable user message, never silently
        // dropped and never moved ahead of earlier messages.
        return Some(Value::Object(Map::from_iter([
            ("role".to_string(), Value::String("user".to_string())),
            (
                "content".to_string(),
                Value::String(memory_user_content(&message.content)),
            ),
        ])));
    }
    if let Some(tool_calls) = &message.tool_calls {
        let mut blocks = Vec::with_capacity(tool_calls.len() + 1);
        if !message.content.is_empty() {
            blocks.push(Value::Object(Map::from_iter([
                ("type".to_string(), Value::String("text".to_string())),
                ("text".to_string(), Value::String(message.content.clone())),
            ])));
        }
        for call in tool_calls {
            let input: Value =
                serde_json::from_str(&call.arguments).unwrap_or_else(|_| Value::Object(Map::new()));
            blocks.push(Value::Object(Map::from_iter([
                ("type".to_string(), Value::String("tool_use".to_string())),
                ("id".to_string(), Value::String(call.id.clone())),
                ("name".to_string(), Value::String(call.name.clone())),
                ("input".to_string(), input),
            ])));
        }
        return Some(Value::Object(Map::from_iter([
            ("role".to_string(), Value::String("assistant".to_string())),
            ("content".to_string(), Value::Array(blocks)),
        ])));
    }
    Some(Value::Object(Map::from_iter([
        ("role".to_string(), Value::String(message.role.clone())),
        (
            "content".to_string(),
            Value::String(message.content.clone()),
        ),
    ])))
}

fn project_history(request: &ChatRequest, capabilities: ProviderCacheCapabilities) -> Vec<Value> {
    match capabilities.wire {
        ProviderWire::OpenAiCompatible => request
            .messages
            .iter()
            .map(|message| openai_history_message(message, capabilities))
            .map(|value| strip_volatile_fields(&value))
            .collect(),
        ProviderWire::Anthropic => request
            .messages
            .iter()
            .filter_map(anthropic_history_message)
            .map(|value| strip_volatile_fields(&value))
            .collect(),
    }
}

/// Resolve tail sections against the already-projected history. A tail whose
/// exact message value is present in history is the append-only memory anchor:
/// it is not appended again as a suffix.
pub fn resolved_tail_messages(
    request: &ChatRequest,
    capabilities: ProviderCacheCapabilities,
    history: &[Value],
) -> (Vec<Value>, TailSectionMode) {
    let mut tail_messages = Vec::new();
    let mut embedded = false;
    for section in request
        .tail_sections
        .iter()
        .filter(|section| section.has_text())
    {
        let value = strip_volatile_fields(&tail_value(section, capabilities));
        if history.iter().any(|message| message == &value) {
            embedded = true;
            continue;
        }
        tail_messages.push(value);
    }
    let mode = if tail_messages.is_empty() {
        if embedded {
            TailSectionMode::Embedded
        } else {
            TailSectionMode::None
        }
    } else {
        match capabilities.wire {
            ProviderWire::Anthropic => TailSectionMode::TrailingUser,
            ProviderWire::OpenAiCompatible if capabilities.tail_system.accepts_system() => {
                TailSectionMode::TrailingSystem
            }
            ProviderWire::OpenAiCompatible => TailSectionMode::TrailingUser,
        }
    };
    (tail_messages, mode)
}

/// The exact OpenAI-compatible `messages` array: head system sections, stable
/// history projection, then any non-embedded tail. Adapters and canonical
/// telemetry share this function.
pub fn openai_wire_messages(
    request: &ChatRequest,
    capabilities: ProviderCacheCapabilities,
) -> Vec<Value> {
    let mut messages = head_messages(request);
    let history = project_history(request, capabilities);
    let (tail, _) = resolved_tail_messages(request, capabilities, &history);
    messages.extend(history);
    messages.extend(tail);
    messages
}

/// The Anthropic `messages` array without the top-level `system` field. The
/// adapter renders system sections separately; this function covers
/// conversation history plus a non-embedded tail fallback.
pub fn anthropic_wire_messages(
    request: &ChatRequest,
    capabilities: ProviderCacheCapabilities,
) -> Vec<Value> {
    let history = project_history(request, capabilities);
    let (tail, _) = resolved_tail_messages(request, capabilities, &history);
    let mut messages = history;
    messages.extend(tail);
    messages
}

/// Build the canonical request view. This is the single function used by both
/// adapters and cache telemetry, so the bytes that are hashed are the bytes that
/// are sent (minus provider cache-control metadata, which is transport-only).
pub fn canonical_request(
    request: &ChatRequest,
    capabilities: ProviderCacheCapabilities,
    tools_hash: &str,
) -> CanonicalRequest {
    let head_messages = head_messages(request)
        .into_iter()
        .map(|value| strip_volatile_fields(&value))
        .collect::<Vec<_>>();
    let history_messages = project_history(request, capabilities);
    let (tail_messages, tail_mode) =
        resolved_tail_messages(request, capabilities, &history_messages);

    let message_hashes = history_messages
        .iter()
        .map(message_hash)
        .collect::<Vec<_>>();
    let message_prefix_hash = chain_hash(&message_hashes);
    let head_prefix_hash = section_hash("head", &head_messages);
    let tail_prefix_hash = section_hash("tail", &tail_messages);
    let request_prefix_hash = request_hash(
        capabilities.wire,
        &head_prefix_hash,
        &message_prefix_hash,
        &tail_prefix_hash,
        tools_hash,
    );
    let history_bytes = canonical_json(&Value::Array(history_messages.clone())).into_bytes();
    let mut full = Vec::new();
    full.extend_from_slice(canonical_json(&Value::Array(head_messages.clone())).as_bytes());
    full.push(b'\n');
    full.extend_from_slice(&history_bytes);
    full.push(b'\n');
    full.extend_from_slice(canonical_json(&Value::Array(tail_messages.clone())).as_bytes());

    CanonicalRequest {
        version: CANONICAL_VERSION,
        wire: capabilities.wire,
        capabilities,
        head_messages,
        history_messages,
        tail_messages,
        tail_mode,
        head_prefix_hash,
        message_prefix_hash,
        tail_prefix_hash,
        request_prefix_hash,
        tools_hash: tools_hash.to_string(),
        message_hashes,
        sequence_issues: audit_message_sequence(&request.messages),
        history_bytes,
        request_bytes: full,
    }
}

/// The canonical full outbound message array in wire order: head, history,
/// tail. Adapters use this to assert byte identity in tests.
pub fn canonical_wire_messages(canonical: &CanonicalRequest) -> Vec<Value> {
    let mut messages = Vec::with_capacity(
        canonical.head_messages.len()
            + canonical.history_messages.len()
            + canonical.tail_messages.len(),
    );
    messages.extend(canonical.head_messages.iter().cloned());
    messages.extend(canonical.history_messages.iter().cloned());
    messages.extend(canonical.tail_messages.iter().cloned());
    messages
}

/// `true` when a status/body pair looks like a provider rejecting a
/// mid-history system message. The OpenAI adapter uses this only for a bounded
/// one-shot retry with the stable user fallback.
pub fn looks_like_tail_system_rejection(status: u16, body: &str) -> bool {
    if !matches!(status, 400 | 404 | 422) {
        return false;
    }
    let lower = body.to_ascii_lowercase();
    if !lower.contains("system") {
        return false;
    }
    lower.contains("role")
        || lower.contains("unsupported")
        || lower.contains("not supported")
        || lower.contains("invalid")
        || lower.contains("unexpected")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CacheTtl, PromptCacheConfig, ToolCall, ToolSchema};
    use serde_json::json;

    fn capabilities() -> ProviderCacheCapabilities {
        ProviderCacheCapabilities::openai_compatible()
    }

    fn anthropic_capabilities() -> ProviderCacheCapabilities {
        ProviderCacheCapabilities::anthropic()
    }

    fn request(messages: Vec<ChatMessage>) -> ChatRequest {
        ChatRequest::new("test-model", messages)
    }

    fn tool(name: &str) -> ToolSchema {
        ToolSchema {
            name: name.to_string(),
            description: format!("{name} tool"),
            parameters: json!({"type": "object"}),
        }
    }

    fn canonical(request: &ChatRequest) -> CanonicalRequest {
        canonical_request(request, capabilities(), "tools")
    }

    fn snapshot(request: &ChatRequest) -> MessagePrefixSnapshot {
        canonical(request).snapshot()
    }

    #[test]
    fn canonical_json_sorts_keys_at_every_depth() {
        let value = json!({
            "z": 1,
            "a": {"y": true, "b": [{"d": 2, "c": 3}]},
        });
        assert_eq!(
            canonical_json(&value),
            r#"{"a":{"b":[{"c":3,"d":2}],"y":true},"z":1}"#
        );
    }

    #[test]
    fn canonical_json_is_stable_under_object_insertion_order() {
        let first = json!({"b": 1, "a": {"d": 2, "c": 3}});
        let second = json!({"a": {"c": 3, "d": 2}, "b": 1});
        assert_eq!(canonical_json(&first), canonical_json(&second));
    }

    #[test]
    fn canonical_json_preserves_array_order() {
        assert_eq!(canonical_json(&json!([3, 1, 2])), "[3,1,2]");
        assert_ne!(
            canonical_json(&json!(["a", "b"])),
            canonical_json(&json!(["b", "a"]))
        );
    }

    #[test]
    fn canonical_json_escapes_strings_like_json() {
        let value = json!({"text": "line\nquote\"tab\t\u{1}"});
        let encoded = canonical_json(&value);
        assert!(encoded.contains("line\\nquote\\\"tab\\t"));
        let decoded: Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded["text"], json!("line\nquote\"tab\t\u{1}"));
    }

    #[test]
    fn unicode_content_is_byte_stable() {
        let first = json!({"text": "你好，世界 — cache"});
        let second = json!({"text": "你好，世界 — cache"});
        assert_eq!(canonical_json(&first), canonical_json(&second));
    }

    #[test]
    fn transport_and_reasoning_fields_are_stripped() {
        let value = json!({
            "role": "assistant",
            "content": "answer",
            "reasoning_content": "secret chain",
            "cache_control": {"type": "ephemeral"},
            "nested": {"cache_control": {"type": "ephemeral"}, "keep": 1}
        });
        let cleaned = strip_volatile_fields(&value);
        assert_eq!(cleaned["content"], json!("answer"));
        assert!(cleaned.get("reasoning_content").is_none());
        assert!(cleaned.get("cache_control").is_none());
        assert_eq!(cleaned["nested"]["keep"], json!(1));
        assert!(cleaned["nested"].get("cache_control").is_none());
    }

    #[test]
    fn canonical_groups_identical_messages_after_stripping_transport_fields() {
        let first = json!({"role": "assistant", "content": "x"});
        let second = json!({
            "role": "assistant",
            "content": "x",
            "cache_control": {"type": "ephemeral"},
            "reasoning_content": "not replayed"
        });
        assert_eq!(
            canonical_json(&strip_volatile_fields(&first)),
            canonical_json(&strip_volatile_fields(&second))
        );
    }

    #[test]
    fn canonical_tools_are_sorted_and_deduplicated() {
        let tools = vec![tool("write_file"), tool("read_file"), tool("write_file")];
        let json = canonical_tools_json(&tools);
        let parsed: Value = serde_json::from_str(&json).unwrap();
        let names = parsed
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["read_file", "write_file"]);
    }

    #[test]
    fn tool_order_does_not_change_the_hash() {
        let first = vec![tool("a"), tool("b")];
        let second = vec![tool("b"), tool("a")];
        assert_eq!(canonical_tools_hash(&first), canonical_tools_hash(&second));
    }

    #[test]
    fn tool_parameter_key_order_does_not_change_the_hash() {
        let mut first = tool("a");
        first.parameters = json!({"type": "object", "required": ["b"], "properties": {"b": {"type": "string"}, "a": {"type": "number"}}});
        let mut second = tool("a");
        second.parameters = json!({"properties": {"a": {"type": "number"}, "b": {"type": "string"}}, "required": ["b"], "type": "object"});
        assert_eq!(
            canonical_tools_hash(&[first]),
            canonical_tools_hash(&[second])
        );
    }

    #[test]
    fn history_message_bytes_are_independent_of_surrounding_sections() {
        let messages = vec![
            ChatMessage::user("one"),
            ChatMessage::assistant_tool_calls("", vec![]),
            ChatMessage::user("two"),
        ];
        let mut first = request(messages.clone());
        let mut second = request(messages);
        first.system = Some("SYSTEM A".to_string());
        second.system = Some("SYSTEM B".to_string());
        assert_eq!(
            canonical(&first).history_bytes,
            canonical(&second).history_bytes
        );
    }

    #[test]
    fn append_detects_an_exact_prefix() {
        let previous = snapshot(&request(vec![
            ChatMessage::user("one"),
            ChatMessage::assistant_tool_calls("first", vec![]),
        ]));
        let current = snapshot(&request(vec![
            ChatMessage::user("one"),
            ChatMessage::assistant_tool_calls("first", vec![]),
            ChatMessage::user("two"),
        ]));
        let report = ContinuityReport::detect(Some(&previous), &current, false);
        assert!(report.append_only);
        assert_eq!(report.reason, None);
        assert_eq!(report.previous_message_count, 2);
        assert_eq!(report.message_count, 3);
    }

    #[test]
    fn unchanged_messages_are_append_only() {
        let previous = snapshot(&request(vec![ChatMessage::user("one")]));
        let current = snapshot(&request(vec![ChatMessage::user("one")]));
        let report = ContinuityReport::detect(Some(&previous), &current, false);
        assert!(report.append_only);
        assert_eq!(report.first_differing_index, None);
    }

    #[test]
    fn first_call_has_no_previous_prefix() {
        let current = snapshot(&request(vec![ChatMessage::user("one")]));
        let report = ContinuityReport::detect(None, &current, false);
        assert!(!report.append_only);
        assert_eq!(report.reason, Some(CacheBreakReason::FirstCall));
        assert!(!report.had_previous);
        assert_eq!(report.previous_message_prefix_hash, "");
    }

    #[test]
    fn edited_message_is_detected_with_its_index() {
        let previous = snapshot(&request(vec![
            ChatMessage::user("one"),
            ChatMessage::user("two"),
            ChatMessage::user("three"),
        ]));
        let mut current_request = request(vec![
            ChatMessage::user("one"),
            ChatMessage::user("TWO EDITED"),
            ChatMessage::user("three"),
        ]);
        current_request.tools = vec![];
        let current = snapshot(&current_request);
        let report = ContinuityReport::detect(Some(&previous), &current, false);
        assert!(!report.append_only);
        assert_eq!(report.reason, Some(CacheBreakReason::MessageEdited));
        assert_eq!(report.first_differing_index, Some(1));
    }

    #[test]
    fn reordered_messages_are_detected() {
        let previous = snapshot(&request(vec![
            ChatMessage::user("one"),
            ChatMessage::assistant_tool_calls("a", vec![]),
            ChatMessage::user("two"),
        ]));
        let current = snapshot(&request(vec![
            ChatMessage::user("one"),
            ChatMessage::user("two"),
            ChatMessage::assistant_tool_calls("a", vec![]),
        ]));
        let report = ContinuityReport::detect(Some(&previous), &current, false);
        assert!(!report.append_only);
        assert_eq!(report.reason, Some(CacheBreakReason::MessageReordered));
        assert_eq!(report.first_differing_index, Some(1));
    }

    #[test]
    fn reordered_tool_calls_are_detected_as_reorder() {
        let previous = snapshot(&request(vec![ChatMessage::assistant_tool_calls(
            "working",
            vec![
                ToolCall {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    arguments: "{}".into(),
                },
                ToolCall {
                    id: "call_2".into(),
                    name: "write_file".into(),
                    arguments: "{}".into(),
                },
            ],
        )]));
        let current = snapshot(&request(vec![ChatMessage::assistant_tool_calls(
            "working",
            vec![
                ToolCall {
                    id: "call_2".into(),
                    name: "write_file".into(),
                    arguments: "{}".into(),
                },
                ToolCall {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    arguments: "{}".into(),
                },
            ],
        )]));
        // The two messages are still a single message, so this is an edit of
        // that message, not a message reorder. The canonical bytes differ.
        let report = ContinuityReport::detect(Some(&previous), &current, false);
        assert!(!report.append_only);
        assert_eq!(report.reason, Some(CacheBreakReason::MessageEdited));
        assert_eq!(report.first_differing_index, Some(0));

        // Reordering two whole tool-call messages is a message reorder.
        let previous = snapshot(&request(vec![
            ChatMessage::assistant_tool_calls("a", vec![]),
            ChatMessage::assistant_tool_calls("b", vec![]),
        ]));
        let current = snapshot(&request(vec![
            ChatMessage::assistant_tool_calls("b", vec![]),
            ChatMessage::assistant_tool_calls("a", vec![]),
        ]));
        let report = ContinuityReport::detect(Some(&previous), &current, false);
        assert_eq!(report.reason, Some(CacheBreakReason::MessageReordered));
    }

    #[test]
    fn truncation_is_an_edit() {
        let previous = snapshot(&request(vec![
            ChatMessage::user("one"),
            ChatMessage::user("two"),
            ChatMessage::user("three"),
        ]));
        let current = snapshot(&request(vec![ChatMessage::user("one")]));
        let report = ContinuityReport::detect(Some(&previous), &current, false);
        assert!(!report.append_only);
        assert_eq!(report.reason, Some(CacheBreakReason::MessageEdited));
        assert_eq!(report.first_differing_index, Some(1));
    }

    #[test]
    fn compaction_marker_wins_over_edit_classification() {
        let previous = snapshot(&request(vec![
            ChatMessage::user("one"),
            ChatMessage::user("two"),
            ChatMessage::user("three"),
        ]));
        let current = snapshot(&request(vec![
            ChatMessage::user("summary"),
            ChatMessage::user("three"),
        ]));
        let report = ContinuityReport::detect(Some(&previous), &current, true);
        assert!(!report.append_only);
        assert_eq!(report.reason, Some(CacheBreakReason::CompactionReset));
    }

    #[test]
    fn compaction_marker_without_a_change_stays_append_only() {
        let previous = snapshot(&request(vec![ChatMessage::user("one")]));
        let current = snapshot(&request(vec![ChatMessage::user("one")]));
        let report = ContinuityReport::detect(Some(&previous), &current, true);
        assert!(report.append_only);
        assert_eq!(report.reason, None);
    }

    #[test]
    fn head_change_is_system_changed() {
        let previous_snapshot = snapshot(&request(vec![ChatMessage::user("one")]));
        let mut current_request = request(vec![ChatMessage::user("one")]);
        current_request.system_sections = vec![SystemSection::new("core", "NEW CHARTER")];
        let current = snapshot(&current_request);
        let report = ContinuityReport::detect(Some(&previous_snapshot), &current, false);
        assert_eq!(report.reason, Some(CacheBreakReason::SystemChanged));
        assert!(!report.append_only);
    }

    #[test]
    fn tool_bundle_change_wins_over_head_change() {
        let mut previous_request = request(vec![ChatMessage::user("one")]);
        previous_request.system_sections = vec![SystemSection::new("core", "A")];
        let previous = canonical_request(&previous_request, capabilities(), "tools-a").snapshot();
        let mut current_request = request(vec![ChatMessage::user("one")]);
        current_request.system_sections = vec![SystemSection::new("core", "B")];
        let current = canonical_request(&current_request, capabilities(), "tools-b").snapshot();
        let report = ContinuityReport::detect(Some(&previous), &current, false);
        assert_eq!(report.reason, Some(CacheBreakReason::ToolBundleChanged));
    }

    #[test]
    fn wire_change_is_system_changed() {
        let previous = canonical_request(
            &request(vec![ChatMessage::user("one")]),
            capabilities(),
            "tools",
        )
        .snapshot();
        let current = canonical_request(
            &request(vec![ChatMessage::user("one")]),
            anthropic_capabilities(),
            "tools",
        )
        .snapshot();
        let report = ContinuityReport::detect(Some(&previous), &current, false);
        assert_eq!(report.reason, Some(CacheBreakReason::SystemChanged));
    }

    #[test]
    fn truncated_previous_hashes_still_prove_a_prefix_with_the_chain() {
        let mut previous = snapshot(&request(vec![
            ChatMessage::user("one"),
            ChatMessage::user("two"),
            ChatMessage::user("three"),
        ]));
        // Simulate a telemetry-truncated previous list that keeps the full
        // chain hash. Only one per-message hash survives.
        previous.message_hashes.truncate(1);
        assert!(!previous.hashes_complete());
        let current = snapshot(&request(vec![
            ChatMessage::user("one"),
            ChatMessage::user("two"),
            ChatMessage::user("three"),
            ChatMessage::user("four"),
        ]));
        let report = ContinuityReport::detect(Some(&previous), &current, false);
        assert!(report.append_only);
    }

    #[test]
    fn truncated_previous_hashes_can_still_reject_a_lost_prefix() {
        let mut previous = snapshot(&request(vec![
            ChatMessage::user("one"),
            ChatMessage::user("two"),
            ChatMessage::user("three"),
        ]));
        previous.message_hashes.truncate(1);
        let current = snapshot(&request(vec![
            ChatMessage::user("one"),
            ChatMessage::user("TWO"),
            ChatMessage::user("three"),
        ]));
        let report = ContinuityReport::detect(Some(&previous), &current, false);
        assert!(!report.append_only);
        // The surviving first hash matched and the rest of the previous list
        // was not persisted, so the break is real but its index and kind are
        // not determinable. Reporting `unknown` is honest; inventing an edit
        // would be a fabricated fact.
        assert_eq!(report.reason, Some(CacheBreakReason::Unknown));
        assert_eq!(report.first_differing_index, None);
    }

    #[test]
    fn snapshot_payload_caps_the_per_message_list() {
        let messages = (0..(MAX_TELEMETRY_MESSAGE_HASHES + 32))
            .map(|index| ChatMessage::user(format!("message {index}")))
            .collect::<Vec<_>>();
        let snapshot = snapshot(&request(messages));
        let payload = snapshot.payload();
        assert_eq!(
            payload["message_hashes"].as_array().unwrap().len(),
            MAX_TELEMETRY_MESSAGE_HASHES
        );
        assert_eq!(payload["message_hashes_complete"], json!(false));
        for hash in payload["message_hashes"].as_array().unwrap() {
            assert_eq!(hash.as_str().unwrap().len(), TELEMETRY_MESSAGE_HASH_CHARS);
        }
    }

    #[test]
    fn snapshot_round_trips_through_payload() {
        let snapshot = snapshot(&request(vec![
            ChatMessage::user("one"),
            ChatMessage::assistant_tool_calls("two", vec![]),
        ]));
        let decoded = MessagePrefixSnapshot::from_event_payload(&snapshot.payload()).unwrap();
        assert_eq!(decoded.message_count, snapshot.message_count);
        assert_eq!(decoded.history_hash, snapshot.history_hash);
        assert_eq!(decoded.head_hash, snapshot.head_hash);
        assert_eq!(decoded.tools_hash, snapshot.tools_hash);
        assert_eq!(decoded.wire, ProviderWire::OpenAiCompatible);
        // Shortened hashes cannot prove a long prefix, but the chain can.
        assert!(!decoded.hashes_complete());
    }

    #[test]
    fn legacy_events_without_canonical_fields_decode_as_none() {
        assert!(MessagePrefixSnapshot::from_event_payload(&json!({})).is_none());
        assert!(
            MessagePrefixSnapshot::from_event_payload(&json!({"prefix_hash": "legacy"})).is_none()
        );
    }

    #[test]
    fn tail_system_is_appended_for_supported_providers() {
        let mut request = request(vec![ChatMessage::user("one")]);
        request.tail_sections = vec![SystemSection::new("tail/memory", "MEMORY")];
        let canonical = canonical(&request);
        assert_eq!(canonical.tail_mode, TailSectionMode::TrailingSystem);
        assert_eq!(canonical.tail_messages.len(), 1);
        assert_eq!(canonical.tail_messages[0]["role"], json!("system"));
        assert_eq!(canonical.tail_messages[0]["content"], json!("MEMORY"));
    }

    #[test]
    fn tail_memory_uses_a_user_message_for_anthropic() {
        let mut request = request(vec![ChatMessage::user("one")]);
        request.tail_sections = vec![SystemSection::new("tail/memory", "MEMORY")];
        let canonical = canonical_request(&request, anthropic_capabilities(), "tools");
        assert_eq!(canonical.tail_mode, TailSectionMode::TrailingUser);
        assert_eq!(canonical.tail_messages[0]["role"], json!("user"));
        assert_eq!(
            canonical.tail_messages[0]["content"],
            json!("[memory]\n\nMEMORY")
        );
    }

    #[test]
    fn tail_memory_uses_a_user_message_for_unknown_providers() {
        let capabilities = ProviderCacheCapabilities::new(
            ProviderWire::OpenAiCompatible,
            TailSystemCapability::Unknown,
        );
        let mut request = request(vec![ChatMessage::user("one")]);
        request.tail_sections = vec![SystemSection::new("tail/memory", "MEMORY")];
        let canonical = canonical_request(&request, capabilities, "tools");
        assert_eq!(canonical.tail_mode, TailSectionMode::TrailingUser);
        assert_eq!(canonical.tail_messages[0]["role"], json!("user"));
    }

    #[test]
    fn embedded_tail_is_not_duplicated() {
        let mut request = request(vec![
            ChatMessage::user("one"),
            ChatMessage::system("MEMORY"),
        ]);
        request.tail_sections = vec![SystemSection::new("tail/memory", "MEMORY")];
        let canonical = canonical(&request);
        assert_eq!(canonical.tail_mode, TailSectionMode::Embedded);
        assert!(canonical.tail_messages.is_empty());
        assert_eq!(
            canonical
                .history_messages
                .iter()
                .filter(|message| message["content"] == json!("MEMORY"))
                .count(),
            1
        );
    }

    #[test]
    fn embedded_tail_matches_after_anthropic_projection() {
        let mut request = request(vec![
            ChatMessage::user("one"),
            ChatMessage::system("MEMORY"),
        ]);
        request.tail_sections = vec![SystemSection::new("tail/memory", "MEMORY")];
        let canonical = canonical_request(&request, anthropic_capabilities(), "tools");
        assert_eq!(canonical.tail_mode, TailSectionMode::Embedded);
        assert_eq!(canonical.history_messages[1]["role"], json!("user"));
        assert_eq!(
            canonical.history_messages[1]["content"],
            json!("[memory]\n\nMEMORY")
        );
    }

    #[test]
    fn no_tail_section_reports_none() {
        let canonical = canonical(&request(vec![ChatMessage::user("one")]));
        assert_eq!(canonical.tail_mode, TailSectionMode::None);
        assert!(canonical.tail_messages.is_empty());
    }

    #[test]
    fn head_and_tail_hashes_are_independent_of_history() {
        let mut first = request(vec![ChatMessage::user("one")]);
        first.system_sections = vec![SystemSection::new("core", "CORE")];
        first.tail_sections = vec![SystemSection::new("tail/memory", "MEM")];
        let mut second = request(vec![ChatMessage::user("two")]);
        second.system_sections = vec![SystemSection::new("core", "CORE")];
        second.tail_sections = vec![SystemSection::new("tail/memory", "MEM")];
        let first = canonical(&first);
        let second = canonical(&second);
        assert_eq!(first.head_prefix_hash, second.head_prefix_hash);
        assert_eq!(first.tail_prefix_hash, second.tail_prefix_hash);
        assert_ne!(first.message_prefix_hash, second.message_prefix_hash);
    }

    #[test]
    fn system_section_boundaries_are_part_of_head_identity() {
        let mut first = request(vec![ChatMessage::user("one")]);
        first.system_sections = vec![
            SystemSection::new("core", "A"),
            SystemSection::new("project", "B"),
        ];
        let mut second = request(vec![ChatMessage::user("one")]);
        second.system_sections = vec![SystemSection::new("core", "A\n\nB")];
        let first = canonical(&first);
        let second = canonical(&second);
        assert_ne!(first.head_prefix_hash, second.head_prefix_hash);
    }

    #[test]
    fn openai_history_never_includes_reasoning_content() {
        let request = request(vec![
            ChatMessage::user("read"),
            ChatMessage::assistant_tool_calls(
                "thinking",
                vec![ToolCall {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    arguments: "{\"path\":\"README.md\"}".into(),
                }],
            ),
        ]);
        let canonical = canonical(&request);
        let encoded = canonical_json(&Value::Array(canonical.history_messages));
        assert!(!encoded.contains("reasoning_content"));
        assert!(!encoded.contains("reasoning_details"));
    }

    #[test]
    fn anthropic_history_projects_tool_results_without_changing_content() {
        let request = request(vec![
            ChatMessage::user("read"),
            ChatMessage::tool("toolu_1", "file contents"),
        ]);
        let canonical = canonical_request(&request, anthropic_capabilities(), "tools");
        let last = canonical.history_messages.last().unwrap();
        assert_eq!(last["role"], json!("user"));
        assert_eq!(last["content"][0]["type"], json!("tool_result"));
        assert_eq!(last["content"][0]["tool_use_id"], json!("toolu_1"));
        assert_eq!(last["content"][0]["content"], json!("file contents"));
    }

    #[test]
    fn anthropic_system_history_is_a_stable_memory_user_message() {
        let request = request(vec![
            ChatMessage::user("one"),
            ChatMessage::system("MEMORY"),
            ChatMessage::user("two"),
        ]);
        let canonical = canonical_request(&request, anthropic_capabilities(), "tools");
        assert_eq!(canonical.history_messages[1]["role"], json!("user"));
        assert_eq!(
            canonical.history_messages[1]["content"],
            json!("[memory]\n\nMEMORY")
        );
        assert_eq!(canonical.history_messages[2]["content"], json!("two"));
    }

    #[test]
    fn openai_unknown_capability_converts_system_history_to_user() {
        let capabilities = ProviderCacheCapabilities::new(
            ProviderWire::OpenAiCompatible,
            TailSystemCapability::Unknown,
        );
        let request = request(vec![
            ChatMessage::user("one"),
            ChatMessage::system("MEMORY"),
        ]);
        let canonical = canonical_request(&request, capabilities, "tools");
        assert_eq!(canonical.history_messages[1]["role"], json!("user"));
        assert_eq!(
            canonical.history_messages[1]["content"],
            json!("[memory]\n\nMEMORY")
        );
    }

    #[test]
    fn tool_call_arguments_are_kept_byte_exact() {
        let request = request(vec![ChatMessage::assistant_tool_calls(
            "",
            vec![ToolCall {
                id: "call_1".into(),
                name: "edit_file".into(),
                arguments: "{\"path\":\"a\",\"content\":\"b\"}".into(),
            }],
        )]);
        let canonical = canonical(&request);
        assert_eq!(
            canonical.history_messages[0]["tool_calls"][0]["function"]["arguments"],
            json!("{\"path\":\"a\",\"content\":\"b\"}")
        );
    }

    #[test]
    fn append_only_detection_does_not_reorder_anything() {
        let request = request(vec![ChatMessage::user("one"), ChatMessage::user("two")]);
        let canonical = canonical(&request);
        assert_eq!(canonical.history_messages[0]["content"], json!("one"));
        assert_eq!(canonical.history_messages[1]["content"], json!("two"));
    }

    #[test]
    fn canonical_request_bytes_are_deterministic() {
        let first = canonical(&request(vec![
            ChatMessage::user("one"),
            ChatMessage::assistant_tool_calls("two", vec![]),
        ]));
        let second = canonical(&request(vec![
            ChatMessage::user("one"),
            ChatMessage::assistant_tool_calls("two", vec![]),
        ]));
        assert_eq!(first.request_bytes, second.request_bytes);
        assert_eq!(first.request_prefix_hash, second.request_prefix_hash);
    }

    #[test]
    fn chain_hash_is_order_sensitive() {
        let a = message_hash(&json!({"role": "user", "content": "a"}));
        let b = message_hash(&json!({"role": "user", "content": "b"}));
        assert_ne!(chain_hash(&[a.clone(), b.clone()]), chain_hash(&[b, a]));
    }

    #[test]
    fn chain_hash_at_returns_none_beyond_the_list() {
        let hashes = vec![message_hash(&json!({"role": "user", "content": "a"}))];
        assert!(chain_hash_at(&hashes, 0).is_some());
        assert!(chain_hash_at(&hashes, 1).is_some());
        assert!(chain_hash_at(&hashes, 2).is_none());
    }

    #[test]
    fn empty_history_has_a_stable_hash() {
        let first = canonical(&request(vec![]));
        let second = canonical(&request(vec![]));
        assert_eq!(first.message_prefix_hash, second.message_prefix_hash);
        assert_eq!(first.message_count(), 0);
    }

    #[test]
    fn tail_system_capability_inference_is_deterministic() {
        assert_eq!(
            ProviderCacheCapabilities::infer("anthropic").tail_system,
            TailSystemCapability::Unsupported
        );
        assert_eq!(
            ProviderCacheCapabilities::infer("openai-compatible").tail_system,
            TailSystemCapability::Supported
        );
        assert_eq!(
            ProviderCacheCapabilities::infer("deepseek").tail_system,
            TailSystemCapability::Supported
        );
        assert_eq!(
            ProviderCacheCapabilities::infer("mystery").tail_system,
            TailSystemCapability::Unknown
        );
        assert!(
            !ProviderCacheCapabilities::infer("mystery")
                .tail_system
                .accepts_system()
        );
    }

    #[test]
    fn cache_break_reason_round_trips() {
        for reason in [
            CacheBreakReason::FirstCall,
            CacheBreakReason::SystemChanged,
            CacheBreakReason::ToolBundleChanged,
            CacheBreakReason::MessageEdited,
            CacheBreakReason::MessageReordered,
            CacheBreakReason::CompactionReset,
            CacheBreakReason::ProviderCacheMiss,
            CacheBreakReason::Unknown,
        ] {
            assert_eq!(CacheBreakReason::parse(reason.as_str()), Some(reason));
        }
        assert_eq!(CacheBreakReason::parse("nope"), None);
    }

    #[test]
    fn tail_system_rejection_detection_is_narrow() {
        assert!(looks_like_tail_system_rejection(
            400,
            "invalid role system in messages"
        ));
        assert!(looks_like_tail_system_rejection(
            422,
            "system messages are not supported after history"
        ));
        assert!(!looks_like_tail_system_rejection(401, "invalid system key"));
        assert!(!looks_like_tail_system_rejection(500, "system failure"));
        assert!(!looks_like_tail_system_rejection(
            400,
            "max_tokens required"
        ));
    }

    #[test]
    fn prompt_cache_config_does_not_affect_canonical_bytes() {
        let mut first = request(vec![ChatMessage::user("one")]);
        first.cache = Some(PromptCacheConfig::new(true, CacheTtl::OneHour, true));
        let second = request(vec![ChatMessage::user("one")]);
        assert_eq!(
            canonical(&first).request_prefix_hash,
            canonical(&second).request_prefix_hash
        );
    }

    #[test]
    fn max_tokens_and_temperature_do_not_change_message_identity() {
        let mut first = request(vec![ChatMessage::user("one")]);
        first.max_tokens = Some(1024);
        first.temperature = Some(0.7);
        let second = request(vec![ChatMessage::user("one")]);
        assert_eq!(
            canonical(&first).message_prefix_hash,
            canonical(&second).message_prefix_hash
        );
    }

    #[test]
    fn valid_parallel_tool_calls_have_no_sequence_issues() {
        let messages = vec![
            ChatMessage::user("read both"),
            ChatMessage::assistant_tool_calls(
                "",
                vec![
                    ToolCall {
                        id: "call_1".into(),
                        name: "read_file".into(),
                        arguments: "{}".into(),
                    },
                    ToolCall {
                        id: "call_2".into(),
                        name: "read_file".into(),
                        arguments: "{}".into(),
                    },
                ],
            ),
            ChatMessage::tool("call_1", "one"),
            ChatMessage::tool("call_2", "two"),
            ChatMessage::assistant_tool_calls("both read", vec![]),
        ];
        assert!(audit_message_sequence(&messages).is_empty());
    }

    #[test]
    fn orphan_tool_result_is_reported() {
        let messages = vec![
            ChatMessage::user("go"),
            ChatMessage::tool("call_missing", "result"),
        ];
        let issues = audit_message_sequence(&messages);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].code, "orphan_tool_result");
        assert_eq!(issues[0].index, 1);
    }

    #[test]
    fn out_of_order_tool_results_are_reported_without_reordering() {
        let messages = vec![
            ChatMessage::assistant_tool_calls(
                "",
                vec![
                    ToolCall {
                        id: "call_1".into(),
                        name: "a".into(),
                        arguments: "{}".into(),
                    },
                    ToolCall {
                        id: "call_2".into(),
                        name: "b".into(),
                        arguments: "{}".into(),
                    },
                ],
            ),
            ChatMessage::tool("call_2", "two"),
            ChatMessage::tool("call_1", "one"),
        ];
        let issues = audit_message_sequence(&messages);
        assert_eq!(issues[0].code, "tool_result_reordered");
        assert_eq!(issues[0].index, 1);
        // The canonical list keeps the caller's order; diagnostics never
        // reorder anything.
        let canonical = canonical(&request(messages));
        assert_eq!(
            canonical.history_messages[1]["tool_call_id"],
            json!("call_2")
        );
        assert_eq!(
            canonical.history_messages[2]["tool_call_id"],
            json!("call_1")
        );
    }

    #[test]
    fn unanswered_tool_call_is_reported() {
        let messages = vec![
            ChatMessage::assistant_tool_calls(
                "",
                vec![ToolCall {
                    id: "call_1".into(),
                    name: "a".into(),
                    arguments: "{}".into(),
                }],
            ),
            ChatMessage::user("next"),
        ];
        let issues = audit_message_sequence(&messages);
        assert!(
            issues
                .iter()
                .any(|issue| issue.code == "unanswered_tool_call")
        );
    }

    #[test]
    fn duplicate_and_empty_tool_call_ids_are_reported() {
        let messages = vec![
            ChatMessage::assistant_tool_calls(
                "",
                vec![ToolCall {
                    id: String::new(),
                    name: "a".into(),
                    arguments: "{}".into(),
                }],
            ),
            ChatMessage::tool("", "result"),
            ChatMessage::assistant_tool_calls(
                "",
                vec![ToolCall {
                    id: "call_1".into(),
                    name: "a".into(),
                    arguments: "{}".into(),
                }],
            ),
            ChatMessage::tool("call_1", "one"),
            ChatMessage::assistant_tool_calls(
                "",
                vec![ToolCall {
                    id: "call_1".into(),
                    name: "a".into(),
                    arguments: "{}".into(),
                }],
            ),
        ];
        let issues = audit_message_sequence(&messages);
        assert!(
            issues
                .iter()
                .any(|issue| issue.code == "empty_tool_call_id")
        );
        assert!(
            issues
                .iter()
                .any(|issue| issue.code == "duplicate_tool_call_id")
        );
    }

    #[test]
    fn verify_append_only_reconstructs_from_a_payload() {
        let first = request(vec![ChatMessage::user("one")]);
        let canonical_first = canonical(&first);
        let payload = canonical_first.snapshot().payload();
        let second = request(vec![
            ChatMessage::user("one"),
            ChatMessage::assistant_tool_calls("answer", vec![]),
            ChatMessage::user("two"),
        ]);
        let report = verify_append_only(Some(&payload), &second, capabilities(), "tools", false);
        assert!(report.append_only);
        let edited = request(vec![ChatMessage::user("TWO")]);
        let report = verify_append_only(Some(&payload), &edited, capabilities(), "tools", false);
        assert!(!report.append_only);
        assert_eq!(report.reason, Some(CacheBreakReason::MessageEdited));
    }

    #[test]
    fn verify_append_only_with_no_previous_payload_is_first_call() {
        let report = verify_append_only(
            None,
            &request(vec![ChatMessage::user("one")]),
            capabilities(),
            "tools",
            false,
        );
        assert_eq!(report.reason, Some(CacheBreakReason::FirstCall));
        assert!(!report.had_previous);
    }

    #[test]
    fn canonical_payload_contains_no_generated_timestamps_or_ids() {
        let canonical = canonical(&request(vec![ChatMessage::user("one")]));
        let encoded = canonical_json(&Value::Array(canonical.history_messages));
        assert!(!encoded.contains("created_at"));
        assert!(!encoded.contains("timestamp"));
        assert!(!encoded.contains("uuid"));
        assert!(!encoded.contains("2026-"));
    }
}

#[cfg(test)]
mod additional_tests {
    use super::*;
    use crate::{ChatRequest, ToolCall};
    use serde_json::json;

    fn request(messages: Vec<ChatMessage>) -> ChatRequest {
        ChatRequest::new("test-model", messages)
    }

    fn canonical(request: &ChatRequest) -> CanonicalRequest {
        canonical_request(
            request,
            ProviderCacheCapabilities::openai_compatible(),
            "tools",
        )
    }

    #[test]
    fn empty_content_is_stable_and_distinct_from_a_missing_message() {
        let empty = canonical(&request(vec![ChatMessage::user("")]));
        let content = canonical(&request(vec![ChatMessage::user("x")]));
        assert_ne!(empty.message_prefix_hash, content.message_prefix_hash);
        assert_eq!(empty.message_count(), 1);
        assert_eq!(
            canonical_json(&empty.history_messages[0]),
            r#"{"content":"","role":"user"}"#
        );
    }

    #[test]
    fn one_megabyte_content_has_a_deterministic_hash() {
        let text = "a".repeat(1_048_576);
        let first = canonical(&request(vec![ChatMessage::user(text.clone())]));
        let second = canonical(&request(vec![ChatMessage::user(text)]));
        assert_eq!(first.message_prefix_hash, second.message_prefix_hash);
        assert!(first.history_bytes.len() > 1_000_000);
    }

    #[test]
    fn tail_changes_do_not_change_the_history_chain() {
        let mut first = request(vec![ChatMessage::user("one")]);
        first.tail_sections = vec![SystemSection::new("tail/memory", "A")];
        let mut second = request(vec![ChatMessage::user("one")]);
        second.tail_sections = vec![SystemSection::new("tail/memory", "B")];
        let first = canonical(&first);
        let second = canonical(&second);
        assert_eq!(first.message_prefix_hash, second.message_prefix_hash);
        assert_ne!(first.tail_prefix_hash, second.tail_prefix_hash);
        assert_ne!(first.request_prefix_hash, second.request_prefix_hash);
    }

    #[test]
    fn tools_change_the_request_hash_not_the_message_chain() {
        let request = request(vec![ChatMessage::user("one")]);
        let first = canonical_request(
            &request,
            ProviderCacheCapabilities::openai_compatible(),
            "tools-a",
        );
        let second = canonical_request(
            &request,
            ProviderCacheCapabilities::openai_compatible(),
            "tools-b",
        );
        assert_eq!(first.message_prefix_hash, second.message_prefix_hash);
        assert_ne!(first.request_prefix_hash, second.request_prefix_hash);
    }

    #[test]
    fn canonical_json_depth_cap_is_deterministic() {
        let mut value = json!(0);
        for _ in 0..(MAX_CANONICAL_DEPTH + 10) {
            value = json!({"nested": value});
        }
        let first = canonical_json(&value);
        let second = canonical_json(&value);
        assert_eq!(first, second);
        assert!(first.ends_with("null}") || first.contains("null"));
    }

    #[test]
    fn identical_duplicate_messages_still_have_a_stable_order() {
        let messages = vec![
            ChatMessage::user("same"),
            ChatMessage::user("same"),
            ChatMessage::user("different"),
        ];
        let reordered = vec![
            ChatMessage::user("same"),
            ChatMessage::user("different"),
            ChatMessage::user("same"),
        ];
        let first = canonical(&request(messages));
        let second = canonical(&request(reordered));
        assert_ne!(first.message_prefix_hash, second.message_prefix_hash);
        let report = ContinuityReport::detect(Some(&first.snapshot()), &second.snapshot(), false);
        assert!(!report.append_only);
        assert_eq!(report.reason, Some(CacheBreakReason::MessageReordered));
    }

    #[test]
    fn provider_wire_is_part_of_the_request_identity() {
        let request = request(vec![ChatMessage::user("one")]);
        let openai = canonical_request(
            &request,
            ProviderCacheCapabilities::openai_compatible(),
            "tools",
        );
        let anthropic =
            canonical_request(&request, ProviderCacheCapabilities::anthropic(), "tools");
        // The conceptual head messages are provider-neutral; the wire and
        // message projection are part of the full request identity.
        assert_eq!(openai.head_prefix_hash, anthropic.head_prefix_hash);
        assert_ne!(openai.request_prefix_hash, anthropic.request_prefix_hash);
    }

    #[test]
    fn shortened_telemetry_hashes_still_locate_a_cross_run_edit() {
        let previous = canonical(&request(vec![
            ChatMessage::user("one"),
            ChatMessage::assistant_tool_calls("two", vec![]),
            ChatMessage::user("three"),
        ]));
        let payload = previous.snapshot().payload();
        let decoded = MessagePrefixSnapshot::from_event_payload(&payload).unwrap();
        assert!(!decoded.hashes_complete());
        let current = canonical(&request(vec![
            ChatMessage::user("one"),
            ChatMessage::assistant_tool_calls("two", vec![]),
            ChatMessage::user("THREE"),
        ]));
        let report = ContinuityReport::detect(Some(&decoded), &current.snapshot(), false);
        assert_eq!(report.reason, Some(CacheBreakReason::MessageEdited));
        assert_eq!(report.first_differing_index, Some(2));
    }

    #[test]
    fn snapshot_payload_never_contains_message_text() {
        let canonical = canonical(&request(vec![
            ChatMessage::user("secret prompt text"),
            ChatMessage::assistant_tool_calls(
                "secret answer",
                vec![ToolCall {
                    id: "call_secret".into(),
                    name: "read_file".into(),
                    arguments: "{\"path\":\"secret.txt\"}".into(),
                }],
            ),
        ]));
        let encoded = canonical.snapshot().payload().to_string();
        assert!(!encoded.contains("secret prompt text"));
        assert!(!encoded.contains("secret answer"));
        assert!(!encoded.contains("secret.txt"));
    }

    #[test]
    fn tool_call_ids_are_not_rewritten_by_canonicalization() {
        let request = request(vec![ChatMessage::assistant_tool_calls(
            "",
            vec![ToolCall {
                id: "provider_generated_call".into(),
                name: "read_file".into(),
                arguments: "{}".into(),
            }],
        )]);
        let canonical = canonical(&request);
        assert_eq!(
            canonical.history_messages[0]["tool_calls"][0]["id"],
            json!("provider_generated_call")
        );
        assert_eq!(
            canonical.history_messages[0]["tool_calls"][0]["function"]["name"],
            json!("read_file")
        );
    }

    #[test]
    fn unbounded_message_counts_still_prove_a_prefix_through_the_chain() {
        let mut previous_messages = Vec::new();
        for index in 0..700 {
            previous_messages.push(ChatMessage::user(format!("message {index}")));
        }
        let previous = canonical(&request(previous_messages.clone()));
        let mut current_messages = previous_messages;
        current_messages.push(ChatMessage::user("appended"));
        let current = canonical(&request(current_messages));
        let report =
            ContinuityReport::detect(Some(&previous.snapshot()), &current.snapshot(), false);
        assert!(report.append_only);
        // The telemetry snapshot caps per-message hashes, but the chain is
        // complete and still proves the exact prefix.
        let payload = previous.snapshot().payload();
        assert_eq!(
            payload["message_hashes"].as_array().unwrap().len(),
            MAX_TELEMETRY_MESSAGE_HASHES
        );
        assert_eq!(payload["message_hashes_complete"], json!(false));
    }

    #[test]
    fn tool_schema_property_named_cache_control_is_not_stripped() {
        let mut tool = crate::ToolSchema {
            name: "cache_config".into(),
            description: "Configure a cache".into(),
            parameters: json!({
                "type": "object",
                "properties": {"cache_control": {"type": "string"}}
            }),
        };
        let first = canonical_tools_hash(&[tool.clone()]);
        tool.parameters = json!({
            "type": "object",
            "properties": {"cache_control": {"type": "number"}}
        });
        let second = canonical_tools_hash(&[tool]);
        assert_ne!(first, second);
        assert!(
            canonical_tool_json(&crate::ToolSchema {
                name: "cache_config".into(),
                description: "Configure a cache".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {"cache_control": {"type": "string"}}
                }),
            })
            .contains("cache_control")
        );
    }

    #[test]
    fn message_sequence_diagnostics_are_bounded() {
        let mut messages = Vec::new();
        for index in 0..100 {
            messages.push(ChatMessage::tool(format!("missing_{index}"), "orphan"));
        }
        let issues = audit_message_sequence(&messages);
        assert_eq!(issues.len(), 32);
    }
}

//! Append-only conversation accounting for provider calls.
//!
//! `vp-providers::canonical` owns the wire projection and the byte-level hash
//! chain. This module owns the runtime side:
//!
//! - a per-run tracker seeded from the latest persisted cache observation,
//! - bounded `context.cache_break` payloads with a first-differing index and
//!   safe metadata (role and tool-call id, never content),
//! - the stable memory anchor. A memory brief is appended to the conversation
//!   once and never moved. That is what makes the "tail" memory compatible with
//!   an exact-prefix provider cache: subsequent turns append after it instead
//!   of reinserting it ahead of new messages.
//!
//! No function here edits, reorders, or drops a prior message. Compaction is
//! the only intentional history rewrite and it is recorded separately.

use serde_json::Value;
use vp_providers::canonical::{
    CacheBreakReason, CanonicalRequest, ContinuityReport, MessagePrefixSnapshot,
    ProviderCacheCapabilities, attach_difference_metadata, canonical_request,
};
use vp_providers::{ChatMessage, ChatRequest};

pub use vp_providers::canonical::{
    TailSectionMode, TailSystemCapability, canonical_tools_hash, canonical_tools_json,
};

/// Event kind for a structural or provider-side cache break. Bounded metadata
/// only; message content is never persisted here.
pub const CACHE_BREAK_EVENT: &str = "context.cache_break";

/// Event kind for appending or reusing the stable memory anchor.
pub const MEMORY_ANCHOR_EVENT: &str = "context.memory.anchor";

/// In-run append-only tracker. It is seeded from the latest persisted ordinary
/// agent `context.cache` event so a resumed run still compares against the last
/// real provider call. Warm calls are excluded because they send the stable
/// sections only and are not part of the conversation chain.
#[derive(Debug, Clone, Default)]
pub struct MessagePrefixTracker {
    previous: Option<MessagePrefixSnapshot>,
    compaction_pending: bool,
}

impl MessagePrefixTracker {
    pub fn new(previous: Option<MessagePrefixSnapshot>) -> Self {
        Self {
            previous,
            compaction_pending: false,
        }
    }

    pub fn from_event(payload: Option<&Value>) -> Self {
        Self::new(payload.and_then(MessagePrefixSnapshot::from_event_payload))
    }

    pub fn previous(&self) -> Option<&MessagePrefixSnapshot> {
        self.previous.as_ref()
    }

    pub fn mark_compaction(&mut self) {
        self.compaction_pending = true;
    }

    pub fn compaction_pending(&self) -> bool {
        self.compaction_pending
    }

    /// Observe one provider call. The current snapshot becomes the next
    /// comparison target whether the call succeeds or fails; a retry of the
    /// same messages is then append-only, while a changed list is not.
    pub fn observe(&mut self, canonical: &CanonicalRequest) -> ContinuityReport {
        let current = canonical.snapshot();
        let mut report =
            ContinuityReport::detect(self.previous.as_ref(), &current, self.compaction_pending);
        attach_difference_metadata(&mut report, canonical);
        self.previous = Some(current);
        // The compaction marker describes exactly one observation. A call that
        // was append-only clears it without reporting a reset.
        self.compaction_pending = false;
        report
    }
}

/// One canonical observation prepared before a provider call.
#[derive(Debug, Clone)]
pub struct CanonicalCallObservation {
    pub canonical: CanonicalRequest,
    pub report: ContinuityReport,
    pub capabilities: ProviderCacheCapabilities,
}

impl CanonicalCallObservation {
    pub fn new(
        request: &ChatRequest,
        capabilities: ProviderCacheCapabilities,
        tools_hash: &str,
        tracker: &mut MessagePrefixTracker,
    ) -> Self {
        let canonical = canonical_request(request, capabilities, tools_hash);
        let report = tracker.observe(&canonical);
        Self {
            canonical,
            report,
            capabilities,
        }
    }

    pub fn structural_break(&self) -> Option<CacheBreakReason> {
        self.report.reason.filter(|reason| reason.is_structural())
    }

    pub fn tail_memory_mode(&self) -> TailSectionMode {
        self.canonical.tail_mode
    }
}

/// Cache-event fields shared by `context.cache` and `context.cache_break`.
/// `previous_message_prefix_hash` is empty on the first call.
pub fn canonical_payload(observation: &CanonicalCallObservation) -> Value {
    let snapshot = observation.canonical.snapshot();
    let mut payload = snapshot.payload();
    if let Some(object) = payload.as_object_mut() {
        object.insert(
            "previous_message_prefix_hash".into(),
            Value::String(observation.report.previous_message_prefix_hash.clone()),
        );
        object.insert(
            "previous_message_count".into(),
            serde_json::json!(observation.report.previous_message_count),
        );
        object.insert(
            "append_only".into(),
            serde_json::json!(observation.report.append_only),
        );
        object.insert(
            "tail_prefix_hash".into(),
            Value::String(observation.canonical.tail_prefix_hash.clone()),
        );
        object.insert(
            "request_prefix_hash".into(),
            Value::String(observation.canonical.request_prefix_hash.clone()),
        );
        object.insert(
            "canonical_message_bytes".into(),
            serde_json::json!(observation.canonical.history_bytes.len()),
        );
        object.insert(
            "tail_memory_mode".into(),
            Value::String(observation.canonical.tail_mode.as_str().to_string()),
        );
        object.insert(
            "tail_system_capability".into(),
            Value::String(observation.capabilities.tail_system.as_str().to_string()),
        );
        object.insert(
            "provider_tail_system".into(),
            serde_json::json!(observation.capabilities.tail_system.accepts_system()),
        );
        object.insert(
            "cache_break_reason".into(),
            match observation.report.reason {
                Some(reason) => Value::String(reason.as_str().to_string()),
                None => Value::Null,
            },
        );
        object.insert(
            "message_sequence_issue_count".into(),
            serde_json::json!(observation.canonical.sequence_issues.len()),
        );
        object.insert(
            "message_sequence_issues".into(),
            Value::Array(
                observation
                    .canonical
                    .sequence_issues
                    .iter()
                    .take(8)
                    .map(|issue| Value::String(issue.code.to_string()))
                    .collect(),
            ),
        );
    }
    payload
}

/// Bounded payload for one `context.cache_break` event. This deliberately
/// contains no message text. The first differing message is described only by
/// index, role, and tool-call id.
pub fn cache_break_payload(
    observation: &CanonicalCallObservation,
    reason: CacheBreakReason,
    provider: &str,
    model: &str,
) -> Value {
    let mut payload = canonical_payload(observation);
    if let Some(object) = payload.as_object_mut() {
        object.insert("reason".into(), Value::String(reason.as_str().to_string()));
        object.insert(
            "first_differing_index".into(),
            observation
                .report
                .first_differing_index
                .map_or(Value::Null, |index| serde_json::json!(index)),
        );
        object.insert(
            "first_differing_role".into(),
            observation
                .report
                .first_differing_role
                .clone()
                .map_or(Value::Null, Value::String),
        );
        object.insert(
            "first_differing_tool_call_id".into(),
            observation
                .report
                .first_differing_tool_call_id
                .clone()
                .map_or(Value::Null, Value::String),
        );
        object.insert("provider".into(), Value::String(provider.to_string()));
        object.insert("model".into(), Value::String(model.to_string()));
        object.insert(
            "had_previous".into(),
            serde_json::json!(observation.report.had_previous),
        );
    }
    payload
}

/// Post-call payload for a provider-side miss on an otherwise append-only
/// request. It carries the same continuity fields as `context.cache` so a
/// reader can distinguish "provider evicted" from "we changed a byte".
pub fn provider_miss_payload(
    observation: &CanonicalCallObservation,
    provider: &str,
    model: &str,
    input_tokens: u64,
    cache_read_tokens: u64,
) -> Value {
    let mut payload = cache_break_payload(
        observation,
        CacheBreakReason::ProviderCacheMiss,
        provider,
        model,
    );
    if let Some(object) = payload.as_object_mut() {
        object.insert("input_tokens".into(), serde_json::json!(input_tokens));
        object.insert(
            "cache_read_tokens".into(),
            serde_json::json!(cache_read_tokens),
        );
    }
    payload
}

/// The exact content of a persisted memory anchor. It is the same text as the
/// L1 `tail/memory` section; the provider projection decides whether the wire
/// role is `system` or a `[memory]` user message.
pub fn memory_anchor_content(section: &vp_providers::SystemSection) -> Option<String> {
    let text = section.text.trim();
    if text.is_empty() {
        None
    } else {
        Some(section.text.clone())
    }
}

/// Latest persisted memory anchor in the active conversation. Only a system
/// message is considered; user prompts and assistant output cannot be mistaken
/// for the runtime-owned anchor.
pub fn latest_memory_anchor(messages: &[ChatMessage]) -> Option<&ChatMessage> {
    messages
        .iter()
        .rev()
        .find(|message| message.role == "system")
}

/// `true` when the current memory brief is already the latest anchor. A
/// reverted brief still appends a new anchor so it takes effect after the
/// newer one, while an unchanged brief adds no bytes.
pub fn memory_anchor_is_current(messages: &[ChatMessage], content: &str) -> bool {
    latest_memory_anchor(messages).is_some_and(|message| message.content == content)
}

/// Resolve the provider capability record for a request. Returns `None` when
/// the latest call already used the same wire and tail-memory shape, so an
/// ordinary turn does not emit a duplicate capability event.
pub fn capability_change_payload(
    current: ProviderCacheCapabilities,
    previous: Option<&Value>,
    provider: &str,
) -> Option<Value> {
    let previous = previous?;
    let previous_wire = previous
        .get("provider_wire")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let previous_tail = previous
        .get("tail_system_capability")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if previous_wire == current.wire.as_str() && previous_tail == current.tail_system.as_str() {
        return None;
    }
    Some(serde_json::json!({
        "provider": provider,
        "provider_wire": current.wire.as_str(),
        "tail_system_capability": current.tail_system.as_str(),
        "provider_tail_system": current.tail_system.accepts_system(),
        "previous_provider_wire": previous_wire,
        "previous_tail_system_capability": previous_tail,
    }))
}

/// Bounded `context.memory.anchor` payload. No text is included.
pub fn memory_anchor_payload(
    content: &str,
    appended: bool,
    message_position: usize,
    layout: u32,
) -> Value {
    serde_json::json!({
        "anchor_hash": crate::sha256_hex(content.as_bytes()),
        "anchor_chars": content.chars().count() as u64,
        "appended": appended,
        "message_position": message_position,
        "prompt_layout": layout,
    })
}

/// Aggregate append-only observations for `session.cache`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CacheContinuityAggregate {
    pub append_only_calls: u64,
    pub non_append_only_calls: u64,
    pub last_append_only: bool,
    pub last_message_prefix_hash: Option<String>,
    pub last_previous_message_prefix_hash: Option<String>,
    pub last_message_count: u64,
    pub last_tail_memory_mode: Option<String>,
    pub provider_tail_system: Option<bool>,
    pub last_tail_system_capability: Option<String>,
    pub break_reason_counts: Vec<vp_protocol::types::CacheCountBucket>,
    pub last_break_at: Option<String>,
    pub last_message_sequence_issue_count: u64,
    pub last_message_sequence_issues: Vec<String>,
    pub message_sequence_issue_counts: Vec<vp_protocol::types::CacheCountBucket>,
}

/// Aggregate the ordinary (non-warm) cache calls and every persisted break
/// event. Counts are derived only from persisted telemetry; a legacy event
/// without canonical fields contributes nothing rather than a fabricated fact.
pub fn aggregate_continuity(
    cache_events: &[vp_protocol::types::Event],
    break_events: &[vp_protocol::types::Event],
) -> CacheContinuityAggregate {
    use std::collections::BTreeMap;

    let mut aggregate = CacheContinuityAggregate::default();
    let mut reasons: BTreeMap<String, u64> = BTreeMap::new();
    let mut sequence_issues: BTreeMap<String, u64> = BTreeMap::new();
    for event in cache_events {
        let payload = &event.payload;
        if payload
            .get("warm")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            continue;
        }
        let append_only = payload.get("append_only").and_then(Value::as_bool);
        if let Some(append_only) = append_only {
            if append_only {
                aggregate.append_only_calls = aggregate.append_only_calls.saturating_add(1);
            } else {
                aggregate.non_append_only_calls = aggregate.non_append_only_calls.saturating_add(1);
            }
            aggregate.last_append_only = append_only;
        }
        aggregate.last_message_prefix_hash = payload
            .get("message_prefix_hash")
            .and_then(Value::as_str)
            .map(str::to_string);
        aggregate.last_previous_message_prefix_hash = payload
            .get("previous_message_prefix_hash")
            .and_then(Value::as_str)
            .filter(|hash| !hash.is_empty())
            .map(str::to_string);
        aggregate.last_message_count = payload
            .get("message_count")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        aggregate.last_tail_memory_mode = payload
            .get("tail_memory_mode")
            .and_then(Value::as_str)
            .map(str::to_string);
        aggregate.provider_tail_system =
            payload.get("provider_tail_system").and_then(Value::as_bool);
        aggregate.last_tail_system_capability = payload
            .get("tail_system_capability")
            .and_then(Value::as_str)
            .map(str::to_string);
        let issues = payload
            .get("message_sequence_issues")
            .and_then(Value::as_array)
            .map(|issues| {
                issues
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if payload.get("message_sequence_issues").is_some()
            || payload.get("message_sequence_issue_count").is_some()
        {
            aggregate.last_message_sequence_issue_count = payload
                .get("message_sequence_issue_count")
                .and_then(Value::as_u64)
                .unwrap_or(issues.len() as u64);
            aggregate.last_message_sequence_issues = issues;
            for issue in &aggregate.last_message_sequence_issues {
                *sequence_issues.entry(issue.clone()).or_default() += 1;
            }
        }
        if let Some(reason) = payload
            .get("cache_break_reason")
            .and_then(Value::as_str)
            .filter(|reason| !reason.is_empty())
        {
            *reasons.entry(reason.to_string()).or_default() += 1;
        }
    }
    for event in break_events {
        let Some(reason) = event.payload.get("reason").and_then(Value::as_str) else {
            continue;
        };
        *reasons.entry(reason.to_string()).or_default() += 1;
        aggregate.last_break_at = Some(event.created_at.clone());
    }
    aggregate.break_reason_counts = reasons
        .into_iter()
        .map(|(key, calls)| vp_protocol::types::CacheCountBucket { key, calls })
        .collect();
    aggregate.message_sequence_issue_counts = sequence_issues
        .into_iter()
        .map(|(key, calls)| vp_protocol::types::CacheCountBucket { key, calls })
        .collect();
    aggregate
}

#[cfg(test)]
mod tests {
    use super::*;
    use vp_providers::ChatMessage;
    use vp_providers::canonical::{ProviderCacheCapabilities, ProviderWire, TailSystemCapability};
    use vp_providers::{ChatRequest, SystemSection};

    fn capabilities() -> ProviderCacheCapabilities {
        ProviderCacheCapabilities::openai_compatible()
    }

    fn observe(
        tracker: &mut MessagePrefixTracker,
        messages: Vec<ChatMessage>,
    ) -> CanonicalCallObservation {
        let request = ChatRequest::new("model", messages);
        CanonicalCallObservation::new(&request, capabilities(), "tools", tracker)
    }

    fn event(kind: &str, payload: Value) -> vp_protocol::types::Event {
        vp_protocol::types::Event {
            session_id: "ses-1".into(),
            seq: 1,
            kind: kind.into(),
            payload,
            created_at: "2026-11-12T00:00:00Z".into(),
        }
    }

    #[test]
    fn tracker_marks_an_appended_turn_as_append_only() {
        let mut tracker = MessagePrefixTracker::default();
        let first = observe(&mut tracker, vec![ChatMessage::user("one")]);
        assert_eq!(first.report.reason, Some(CacheBreakReason::FirstCall));
        assert!(!first.report.append_only);
        let second = observe(
            &mut tracker,
            vec![
                ChatMessage::user("one"),
                ChatMessage::assistant_tool_calls("answer", vec![]),
                ChatMessage::user("two"),
            ],
        );
        assert!(second.report.append_only);
        assert_eq!(second.report.reason, None);
        assert!(second.structural_break().is_none());
    }

    #[test]
    fn tracker_reports_edits_with_role_metadata_but_no_content() {
        let mut tracker = MessagePrefixTracker::default();
        observe(
            &mut tracker,
            vec![ChatMessage::user("one"), ChatMessage::user("two")],
        );
        let changed = observe(
            &mut tracker,
            vec![ChatMessage::user("one"), ChatMessage::user("TWO")],
        );
        assert_eq!(changed.report.reason, Some(CacheBreakReason::MessageEdited));
        assert_eq!(changed.report.first_differing_index, Some(1));
        assert_eq!(changed.report.first_differing_role.as_deref(), Some("user"));
        let payload = cache_break_payload(
            &changed,
            CacheBreakReason::MessageEdited,
            "deepseek",
            "model",
        );
        assert!(!payload.to_string().contains("TWO"));
        assert_eq!(payload["first_differing_index"], 1);
        assert_eq!(payload["first_differing_role"], "user");
    }

    #[test]
    fn tracker_marks_compaction_once() {
        let mut tracker = MessagePrefixTracker::default();
        observe(&mut tracker, vec![ChatMessage::user("one")]);
        tracker.mark_compaction();
        let compacted = observe(&mut tracker, vec![ChatMessage::user("summary")]);
        assert_eq!(
            compacted.report.reason,
            Some(CacheBreakReason::CompactionReset)
        );
        assert!(!tracker.compaction_pending());
        let appended = observe(
            &mut tracker,
            vec![
                ChatMessage::user("summary"),
                ChatMessage::assistant_tool_calls("answer", vec![]),
                ChatMessage::user("next"),
            ],
        );
        assert!(appended.report.append_only);
    }

    #[test]
    fn compaction_marker_clears_on_an_append_only_call() {
        let mut tracker = MessagePrefixTracker::default();
        observe(&mut tracker, vec![ChatMessage::user("one")]);
        tracker.mark_compaction();
        let same = observe(&mut tracker, vec![ChatMessage::user("one")]);
        assert!(same.report.append_only);
        assert!(!tracker.compaction_pending());
    }

    #[test]
    fn tracker_seeds_from_a_persisted_event() {
        let mut tracker = MessagePrefixTracker::default();
        let first = observe(&mut tracker, vec![ChatMessage::user("one")]);
        let payload = canonical_payload(&first);
        let mut resumed = MessagePrefixTracker::from_event(Some(&payload));
        let next = observe(
            &mut resumed,
            vec![
                ChatMessage::user("one"),
                ChatMessage::assistant_tool_calls("answer", vec![]),
                ChatMessage::user("two"),
            ],
        );
        // Shortened telemetry hashes are incomplete, but the chain still proves
        // the exact prefix.
        assert!(next.report.append_only);
    }

    #[test]
    fn legacy_seed_means_first_call() {
        let mut tracker = MessagePrefixTracker::from_event(Some(&serde_json::json!({})));
        let observation = observe(&mut tracker, vec![ChatMessage::user("one")]);
        assert_eq!(observation.report.reason, Some(CacheBreakReason::FirstCall));
    }

    #[test]
    fn canonical_payload_has_all_continuity_fields() {
        let mut tracker = MessagePrefixTracker::default();
        observe(&mut tracker, vec![ChatMessage::user("one")]);
        let observation = observe(
            &mut tracker,
            vec![
                ChatMessage::user("one"),
                ChatMessage::assistant_tool_calls("answer", vec![]),
                ChatMessage::user("two"),
            ],
        );
        let payload = canonical_payload(&observation);
        assert_eq!(payload["append_only"], true);
        assert_eq!(payload["message_count"], 3);
        assert_eq!(payload["previous_message_count"], 1);
        assert_eq!(payload["provider_wire"], "openai-compatible");
        assert_eq!(payload["tail_memory_mode"], "none");
        assert_eq!(payload["canonical_version"], 1);
        assert!(payload["message_prefix_hash"].as_str().unwrap().len() == 64);
        assert!(
            payload["previous_message_prefix_hash"]
                .as_str()
                .unwrap()
                .len()
                == 64
        );
        assert!(payload.get("cache_break_reason").is_some());
    }

    #[test]
    fn structural_break_excludes_first_call_and_provider_miss() {
        assert!(!CacheBreakReason::FirstCall.is_structural());
        assert!(!CacheBreakReason::ProviderCacheMiss.is_structural());
        assert!(CacheBreakReason::MessageEdited.is_structural());
        assert!(CacheBreakReason::CompactionReset.is_structural());
    }

    #[test]
    fn aggregate_counts_append_only_and_break_reasons() {
        let cache = vec![
            event(
                "context.cache",
                serde_json::json!({
                    "append_only": false,
                    "warm": false,
                    "cache_break_reason": "first_call",
                    "message_prefix_hash": "a",
                    "message_count": 1,
                    "tail_memory_mode": "none",
                    "provider_tail_system": true,
                    "tail_system_capability": "supported",
                    "message_sequence_issue_count": 1,
                    "message_sequence_issues": ["tool_result_reordered"]
                }),
            ),
            event(
                "context.cache",
                serde_json::json!({
                    "append_only": true,
                    "warm": false,
                    "message_prefix_hash": "b",
                    "previous_message_prefix_hash": "a",
                    "message_count": 2
                }),
            ),
            event(
                "context.cache",
                serde_json::json!({
                    "append_only": true,
                    "warm": true,
                    "message_prefix_hash": "warm"
                }),
            ),
        ];
        let breaks = vec![event(
            "context.cache_break",
            serde_json::json!({"reason": "message_edited"}),
        )];
        let aggregate = aggregate_continuity(&cache, &breaks);
        assert_eq!(aggregate.append_only_calls, 1);
        assert_eq!(aggregate.non_append_only_calls, 1);
        assert!(aggregate.last_append_only);
        assert_eq!(aggregate.last_message_prefix_hash.as_deref(), Some("b"));
        assert_eq!(
            aggregate.last_previous_message_prefix_hash.as_deref(),
            Some("a")
        );
        assert_eq!(
            aggregate.break_reason_counts,
            vec![
                vp_protocol::types::CacheCountBucket {
                    key: "first_call".into(),
                    calls: 1
                },
                vp_protocol::types::CacheCountBucket {
                    key: "message_edited".into(),
                    calls: 1
                }
            ]
        );
        assert_eq!(aggregate.last_message_sequence_issue_count, 1);
        assert_eq!(
            aggregate.last_message_sequence_issues,
            vec!["tool_result_reordered"]
        );
        assert_eq!(
            aggregate.message_sequence_issue_counts,
            vec![vp_protocol::types::CacheCountBucket {
                key: "tool_result_reordered".into(),
                calls: 1
            }]
        );
    }

    #[test]
    fn aggregate_ignores_legacy_events_without_append_only() {
        let cache = vec![event(
            "context.cache",
            serde_json::json!({"input_tokens": 100}),
        )];
        let aggregate = aggregate_continuity(&cache, &[]);
        assert_eq!(aggregate.append_only_calls, 0);
        assert_eq!(aggregate.non_append_only_calls, 0);
        assert!(aggregate.last_message_prefix_hash.is_none());
        assert!(aggregate.break_reason_counts.is_empty());
    }

    #[test]
    fn memory_anchor_is_current_only_for_the_latest_system_message() {
        let messages = vec![
            ChatMessage::system("memory v1"),
            ChatMessage::user("prompt"),
            ChatMessage::assistant_tool_calls("answer", vec![]),
            ChatMessage::system("memory v2"),
            ChatMessage::user("next"),
        ];
        assert!(memory_anchor_is_current(&messages, "memory v2"));
        assert!(!memory_anchor_is_current(&messages, "memory v1"));
        assert!(!memory_anchor_is_current(&[], "memory v1"));
    }

    #[test]
    fn memory_anchor_ignores_user_messages_with_the_same_text() {
        let messages = vec![ChatMessage::user(
            "Run memory (retrieved for this session):\nx",
        )];
        assert!(!memory_anchor_is_current(
            &messages,
            "Run memory (retrieved for this session):\nx"
        ));
    }

    #[test]
    fn memory_anchor_payload_has_no_text() {
        let payload = memory_anchor_payload("secret memory text", true, 4, 3);
        assert!(!payload.to_string().contains("secret memory text"));
        assert_eq!(payload["appended"], true);
        assert_eq!(payload["message_position"], 4);
        assert_eq!(payload["prompt_layout"], 3);
    }

    #[test]
    fn empty_memory_section_is_not_an_anchor() {
        let section = SystemSection::new("tail/memory", "   \n  ");
        assert!(memory_anchor_content(&section).is_none());
        let section = SystemSection::new("tail/memory", "MEMORY");
        assert_eq!(memory_anchor_content(&section).as_deref(), Some("MEMORY"));
    }

    #[test]
    fn capability_change_payload_is_only_emitted_on_a_change() {
        let supported = ProviderCacheCapabilities::openai_compatible();
        assert!(capability_change_payload(supported, None, "openai-compatible").is_none());
        let previous = serde_json::json!({
            "provider_wire": "openai-compatible",
            "tail_system_capability": "supported"
        });
        assert!(
            capability_change_payload(supported, Some(&previous), "openai-compatible").is_none()
        );
        let downgraded = ProviderCacheCapabilities::new(
            ProviderWire::OpenAiCompatible,
            TailSystemCapability::Unsupported,
        );
        let payload =
            capability_change_payload(downgraded, Some(&previous), "openai-compatible").unwrap();
        assert_eq!(payload["tail_system_capability"], "unsupported");
        assert_eq!(payload["provider_tail_system"], false);
        assert_eq!(payload["previous_tail_system_capability"], "supported");
        assert_eq!(payload["provider"], "openai-compatible");
    }

    #[test]
    fn provider_miss_payload_is_labeled_and_has_no_content() {
        let mut tracker = MessagePrefixTracker::default();
        observe(&mut tracker, vec![ChatMessage::user("one")]);
        let observation = observe(
            &mut tracker,
            vec![
                ChatMessage::user("one"),
                ChatMessage::assistant_tool_calls("answer", vec![]),
                ChatMessage::user("two"),
            ],
        );
        let payload = provider_miss_payload(&observation, "deepseek", "model", 100, 0);
        assert_eq!(payload["reason"], "provider_cache_miss");
        assert_eq!(payload["input_tokens"], 100);
        assert_eq!(payload["cache_read_tokens"], 0);
        assert!(!payload.to_string().contains("answer"));
    }
}

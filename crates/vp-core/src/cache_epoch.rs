//! Cache epochs, cross-session prefix reuse, and compaction boundaries.
//!
//! A cache epoch is the identity of the *shareable stable instruction prefix*:
//!
//! ```text
//! cache_epoch = sha256(model, tools_hash, core section hashes,
//!                      project section hashes)
//! ```
//!
//! It deliberately excludes session/run context, the current prompt, the
//! memory tail, and the conversation. Those bytes may be part of the provider
//! request, but they must not turn a stable instruction block into a different
//! epoch. Two sessions with the same model, tool bundle, and core/project
//! sections share an epoch even when their goals, task objectives, prompts, and
//! memory briefs differ.
//!
//! The module owns the deterministic side of the lifecycle:
//!
//! - epoch identity and change attribution;
//! - event-derived per-session observations for the `prompt_prefixes`
//!   projection;
//! - cross-session reuse attribution from provider-reported usage (never from
//!   a fabricated "cache should be warm" assumption);
//! - the bounded epoch timeline used by `session.cache.epoch`.
//!
//! It contains no provider I/O, no scheduling, and no hidden model routing.
//! Warm calls are excluded from observations: an explicit warm is billed as
//! its own call but is not evidence that a session used the prefix.

use std::collections::BTreeMap;

use serde_json::Value;
use vp_protocol::types::{
    CacheEpochTimelineEntry, Event, PromptPrefixAggregate, PromptPrefixObservation,
    PromptSectionRef,
};

use crate::prompt_cache::PromptSectionHashes;
use crate::sha256_hex;

/// Versioned domain separator for the epoch hash. Bump only with a deliberate
/// cache identity migration; existing epochs remain valid historical rows.
pub const CACHE_EPOCH_VERSION: u32 = 1;
pub const CACHE_EPOCH_SCHEME: &str = "verlinpark.cache_epoch/v1";

/// A low provider hit on an unchanged epoch is attributed to the provider, not
/// to our prefix. Partial hits above this ratio are treated as ordinary cache
/// behavior and are not reported as misses.
pub const PROVIDER_MISS_HIT_RATIO: f64 = 0.5;

/// Bounded timeline length accepted from a caller.
pub const MAX_CACHE_EPOCH_TIMELINE: usize = 200;
/// Bounded project aggregate list accepted from a caller.
pub const MAX_CACHE_EPOCH_AGGREGATES: usize = 100;

/// Stable instruction identity for one agent run.
///
/// `core_sections` and `project_sections` carry every head section whose key
/// belongs to the core or project component, in ordinal order. Session/run
/// context and tail memory are intentionally absent.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CacheEpochIdentity {
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub tools_hash: String,
    #[serde(default)]
    pub core_hash: String,
    #[serde(default)]
    pub project_hash: String,
    #[serde(default)]
    pub core_sections: Vec<PromptSectionRef>,
    #[serde(default)]
    pub project_sections: Vec<PromptSectionRef>,
}

impl CacheEpochIdentity {
    pub fn new(
        model: impl Into<String>,
        tools_hash: impl Into<String>,
        core_hash: impl Into<String>,
        project_hash: impl Into<String>,
        core_sections: Vec<PromptSectionRef>,
        project_sections: Vec<PromptSectionRef>,
    ) -> Self {
        let mut identity = Self {
            model: model.into(),
            tools_hash: tools_hash.into(),
            core_hash: core_hash.into(),
            project_hash: project_hash.into(),
            core_sections,
            project_sections,
        };
        identity.sort_sections();
        identity
    }

    /// Build from the L1/L3 prompt hashes. The combined `core_hash` and
    /// `project_hash` already commit to the joined section text; the ordered
    /// refs additionally bind key names and ordinals so a section reorder or
    /// rename cannot accidentally reuse a provider prefix that represents
    /// different bytes.
    pub fn from_hashes(model: &str, hashes: &PromptSectionHashes) -> Self {
        let mut core_sections = Vec::new();
        let mut project_sections = Vec::new();
        for section in &hashes.head_sections {
            if is_core_key(&section.key) {
                core_sections.push(section.clone());
            } else if is_project_key(&section.key) {
                project_sections.push(section.clone());
            }
        }
        Self::new(
            model,
            hashes.tools_hash.clone(),
            hashes.core_hash.clone(),
            hashes.project_hash.clone(),
            core_sections,
            project_sections,
        )
    }

    fn sort_sections(&mut self) {
        self.core_sections.sort_by(|left, right| {
            left.ordinal
                .cmp(&right.ordinal)
                .then_with(|| left.key.cmp(&right.key))
        });
        self.project_sections.sort_by(|left, right| {
            left.ordinal
                .cmp(&right.ordinal)
                .then_with(|| left.key.cmp(&right.key))
        });
    }

    /// Canonical bytes hashed for the epoch. Field order is fixed and every
    /// component is length-delimited with unit separators so concatenation
    /// cannot collide (`a|bc` vs `ab|c`).
    pub fn canonical_material(&self) -> String {
        let mut material = String::with_capacity(
            CACHE_EPOCH_SCHEME.len()
                + self.model.len()
                + self.tools_hash.len()
                + self.core_hash.len()
                + self.project_hash.len()
                + 128,
        );
        material.push_str(CACHE_EPOCH_SCHEME);
        material.push('\n');
        material.push_str(&CACHE_EPOCH_VERSION.to_string());
        material.push('\n');
        push_component(&mut material, "model", &self.model);
        push_component(&mut material, "tools", &self.tools_hash);
        push_component(&mut material, "core", &self.core_hash);
        push_component(&mut material, "project", &self.project_hash);
        for (component, sections) in [
            ("core_sections", &self.core_sections),
            ("project_sections", &self.project_sections),
        ] {
            material.push_str(component);
            material.push('\u{1f}');
            for section in sections {
                material.push_str(&section.key);
                material.push('\u{1f}');
                material.push_str(&section.ordinal.to_string());
                material.push('\u{1f}');
                material.push_str(&section.hash);
                material.push('\u{1e}');
            }
            material.push('\n');
        }
        material
    }

    pub fn hash(&self) -> String {
        sha256_hex(self.canonical_material().as_bytes())
    }

    pub fn payload(&self) -> Value {
        serde_json::json!({
            "cache_epoch_scheme": CACHE_EPOCH_SCHEME,
            "cache_epoch_version": CACHE_EPOCH_VERSION,
            "cache_epoch_model": self.model,
            "cache_epoch_tools_hash": self.tools_hash,
            "cache_epoch_core_hash": self.core_hash,
            "cache_epoch_project_hash": self.project_hash,
            "cache_epoch_core_sections": self.core_sections,
            "cache_epoch_project_sections": self.project_sections,
        })
    }

    /// Parse the identity recorded on a `context.prompt_prefix` or
    /// `context.cache` event. Missing fields produce an invalid identity rather
    /// than a defaulted epoch.
    pub fn from_event_payload(payload: &Value) -> Option<Self> {
        let text = |key: &str| payload.get(key).and_then(Value::as_str).map(str::to_string);
        let sections = |key: &str| -> Option<Vec<PromptSectionRef>> {
            payload.get(key).and_then(|value| {
                serde_json::from_value::<Vec<PromptSectionRef>>(value.clone()).ok()
            })
        };
        Some(Self {
            model: text("cache_epoch_model")?,
            tools_hash: text("cache_epoch_tools_hash")?,
            core_hash: text("cache_epoch_core_hash")?,
            project_hash: text("cache_epoch_project_hash")?,
            core_sections: sections("cache_epoch_core_sections").unwrap_or_default(),
            project_sections: sections("cache_epoch_project_sections").unwrap_or_default(),
        })
    }
}

/// Finalized epoch value plus the identity that produced it.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CacheEpoch {
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub identity: CacheEpochIdentity,
}

impl CacheEpoch {
    pub fn from_hashes(model: &str, hashes: &PromptSectionHashes) -> Self {
        let identity = CacheEpochIdentity::from_hashes(model, hashes);
        let hash = identity.hash();
        Self { hash, identity }
    }

    pub fn from_identity(identity: CacheEpochIdentity) -> Self {
        let hash = identity.hash();
        Self { hash, identity }
    }

    /// Parse an event payload that carries both the final epoch and its
    /// identity fields. Legacy L1 placeholder events have `cache_epoch` equal
    /// to the prefix hash but no `cache_epoch_model`; they return `None` so
    /// they cannot be mistaken for finalized epochs.
    pub fn from_event_payload(payload: &Value) -> Option<Self> {
        let hash = payload
            .get("cache_epoch")
            .and_then(Value::as_str)
            .filter(|value| is_cache_epoch(value))?;
        let identity = CacheEpochIdentity::from_event_payload(payload)?;
        if identity.hash() != hash {
            return None;
        }
        Some(Self {
            hash: hash.to_string(),
            identity,
        })
    }

    /// Record the epoch fields on `context.prompt_prefix`, `context.cache`,
    /// and warm telemetry. The returned map is bounded: hashes, names, and
    /// ordinals only, never text.
    pub fn payload_fields(&self) -> BTreeMap<String, Value> {
        let mut fields = BTreeMap::new();
        fields.insert("cache_epoch".to_string(), Value::String(self.hash.clone()));
        for (key, value) in self
            .identity
            .payload()
            .as_object()
            .cloned()
            .unwrap_or_default()
        {
            fields.insert(key, value);
        }
        fields
    }

    pub fn short_hash(&self) -> &str {
        self.hash.get(..12).unwrap_or(&self.hash)
    }
}

fn push_component(material: &mut String, name: &str, value: &str) {
    material.push_str(name);
    material.push('\u{1f}');
    material.push_str(value);
    material.push('\u{1e}');
}

fn is_core_key(key: &str) -> bool {
    key == "core" || key.starts_with("core/")
}

fn is_project_key(key: &str) -> bool {
    key == "project" || key.starts_with("project/")
}

pub fn is_cache_epoch(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

/// Return a finalized epoch from an event payload. L1 placeholder events used
/// `cache_epoch = prefix_hash` before the epoch identity existed; treating them
/// as finalized would pollute cross-session counts with sessions that never
/// recorded model/tool/core/project identity.
pub fn finalized_epoch_hash(payload: &Value) -> Option<&str> {
    if payload
        .get("cache_epoch_placeholder")
        .and_then(Value::as_bool)
        == Some(true)
    {
        return None;
    }
    if payload.get("cache_epoch_source").and_then(Value::as_str) == Some("l1_placeholder") {
        return None;
    }
    payload
        .get("cache_epoch")
        .and_then(Value::as_str)
        .filter(|value| is_cache_epoch(value))
}

/// Why a finalized epoch differs from the previous one. The reason is derived
/// from the identity fields, not from timing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheEpochChangeReason {
    First,
    Model,
    Tools,
    Project,
    Core,
    Sections,
    Unchanged,
    Unknown,
}

impl CacheEpochChangeReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::First => "first",
            Self::Model => "model",
            Self::Tools => "tools",
            Self::Project => "project",
            Self::Core => "core",
            Self::Sections => "sections",
            Self::Unchanged => "unchanged",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "first" => Self::First,
            "model" => Self::Model,
            "tools" => Self::Tools,
            "project" => Self::Project,
            "core" => Self::Core,
            "sections" => Self::Sections,
            "unchanged" => Self::Unchanged,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }
}

pub fn epoch_change_reason(
    previous: Option<&CacheEpochIdentity>,
    current: &CacheEpochIdentity,
) -> CacheEpochChangeReason {
    let Some(previous) = previous else {
        return CacheEpochChangeReason::First;
    };
    if previous.model != current.model {
        CacheEpochChangeReason::Model
    } else if previous.tools_hash != current.tools_hash {
        CacheEpochChangeReason::Tools
    } else if previous.project_hash != current.project_hash {
        CacheEpochChangeReason::Project
    } else if previous.core_hash != current.core_hash {
        CacheEpochChangeReason::Core
    } else if previous.core_sections != current.core_sections
        || previous.project_sections != current.project_sections
    {
        CacheEpochChangeReason::Sections
    } else if previous == current {
        CacheEpochChangeReason::Unchanged
    } else {
        // Identical visible components that still produce different section
        // refs are not a known generator change.
        CacheEpochChangeReason::Unknown
    }
}

/// Attach provider-reported cross-session reuse to one call.
///
/// Only the first provider call of a session can be attributed wholly to
/// another session: later cache reads include the conversation prefix this
/// session itself created. `prior_sessions` is the number of *other* sessions
/// that had already observed the epoch before this call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CrossSessionAttribution {
    pub prior_sessions: u64,
    pub first_session_call: bool,
    pub cross_session_cache_read_tokens: u64,
}

/// Projection view used by the agent before the first provider call: how many
/// *other* sessions already observed the epoch, and whether this session has
/// observed it before.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheEpochReuse {
    pub prior_sessions: u64,
    pub session_calls: u64,
    pub first_session_call: bool,
}

pub fn reuse_from_projection(
    aggregate: Option<&PromptPrefixAggregate>,
    observation: Option<&PromptPrefixObservation>,
) -> CacheEpochReuse {
    let observed_sessions = aggregate.map(|row| row.session_count).unwrap_or(0);
    let session_calls = observation.map(|row| row.calls).unwrap_or(0);
    CacheEpochReuse {
        prior_sessions: if observation.is_some() {
            observed_sessions.saturating_sub(1)
        } else {
            observed_sessions
        },
        session_calls,
        first_session_call: session_calls == 0,
    }
}

pub fn cross_session_attribution(
    prior_sessions: u64,
    first_session_call: bool,
    cache_read_tokens: u64,
) -> CrossSessionAttribution {
    CrossSessionAttribution {
        prior_sessions,
        first_session_call,
        cross_session_cache_read_tokens: if first_session_call && prior_sessions > 0 {
            cache_read_tokens
        } else {
            0
        },
    }
}

/// True when a provider-reported call is a low cache hit for attribution.
pub fn is_low_cache_hit(input_tokens: u64, cache_read_tokens: u64) -> bool {
    if input_tokens == 0 {
        return false;
    }
    let ratio = cache_read_tokens.min(input_tokens) as f64 / input_tokens as f64;
    ratio < PROVIDER_MISS_HIT_RATIO
}

/// Derive one session's epoch observations from its persisted events.
///
/// Returns `None` when no `context.cache` event carries the L4 epoch field, so
/// replay preserves rows for legacy sessions that predate this projection.
/// Returns `Some(rows)` (possibly empty) as soon as an event claims an epoch:
/// a malformed epoch then surfaces as projection drift instead of being
/// silently ignored.
pub fn observations_from_events(
    project_id: &str,
    session_id: &str,
    events: &[Event],
) -> Option<Vec<PromptPrefixObservation>> {
    let mut saw_epoch_field = false;
    let mut by_epoch: BTreeMap<String, PromptPrefixObservation> = BTreeMap::new();
    for event in events {
        if event.kind != "context.cache" {
            continue;
        }
        if event
            .payload
            .get("warm")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            continue;
        }
        if event.payload.get("cache_epoch").is_none() {
            continue;
        }
        if event
            .payload
            .get("cache_epoch_placeholder")
            .and_then(Value::as_bool)
            == Some(true)
            || event
                .payload
                .get("cache_epoch_source")
                .and_then(Value::as_str)
                == Some("l1_placeholder")
        {
            continue;
        }
        saw_epoch_field = true;
        let Some(epoch) = CacheEpoch::from_event_payload(&event.payload).map(|epoch| epoch.hash)
        else {
            continue;
        };
        let text = |key: &str| {
            event
                .payload
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let sections = |key: &str| {
            event
                .payload
                .get(key)
                .and_then(|value| {
                    serde_json::from_value::<Vec<PromptSectionRef>>(value.clone()).ok()
                })
                .unwrap_or_default()
        };
        let input = event
            .payload
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let read = event
            .payload
            .get("cache_read_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let row = by_epoch
            .entry(epoch.to_string())
            .or_insert_with(|| PromptPrefixObservation {
                project_id: project_id.to_string(),
                cache_epoch: epoch.to_string(),
                session_id: session_id.to_string(),
                first_seen: event.created_at.clone(),
                last_seen: event.created_at.clone(),
                ..PromptPrefixObservation::default()
            });
        row.model = text("cache_epoch_model");
        row.tool_bundle = text("tool_bundle");
        row.tool_bundle_hash = text("tool_bundle_hash");
        row.core_hash = text("cache_epoch_core_hash");
        row.project_hash = text("cache_epoch_project_hash");
        row.core_sections = sections("cache_epoch_core_sections");
        row.project_sections = sections("cache_epoch_project_sections");
        if event.created_at < row.first_seen {
            row.first_seen = event.created_at.clone();
        }
        if event.created_at > row.last_seen {
            row.last_seen = event.created_at.clone();
        }
        row.observed_input_tokens = row.observed_input_tokens.saturating_add(input);
        row.observed_cache_read_tokens = row
            .observed_cache_read_tokens
            .saturating_add(read.min(input));
        row.calls = row.calls.saturating_add(1);
    }
    if !saw_epoch_field {
        return None;
    }
    let mut rows = by_epoch.into_values().collect::<Vec<_>>();
    rows.sort_by(|left, right| left.cache_epoch.cmp(&right.cache_epoch));
    Some(rows)
}

fn sections_equal(left: &[PromptSectionRef], right: &[PromptSectionRef]) -> bool {
    left == right
}

/// Compare projected observations to event-derived observations. Timestamps,
/// token sums, call counts, and identity metadata are all durable fields.
pub fn observations_match(
    projected: &[PromptPrefixObservation],
    expected: &[PromptPrefixObservation],
) -> bool {
    if projected.len() != expected.len() {
        return false;
    }
    let mut projected = projected.to_vec();
    projected.sort_by(|left, right| left.cache_epoch.cmp(&right.cache_epoch));
    projected.iter().zip(expected).all(|(left, right)| {
        left.project_id == right.project_id
            && left.cache_epoch == right.cache_epoch
            && left.session_id == right.session_id
            && left.model == right.model
            && left.tool_bundle == right.tool_bundle
            && left.tool_bundle_hash == right.tool_bundle_hash
            && left.core_hash == right.core_hash
            && left.project_hash == right.project_hash
            && left.first_seen == right.first_seen
            && left.last_seen == right.last_seen
            && left.observed_input_tokens == right.observed_input_tokens
            && left.observed_cache_read_tokens == right.observed_cache_read_tokens
            && left.calls == right.calls
            && sections_equal(&left.core_sections, &right.core_sections)
            && sections_equal(&left.project_sections, &right.project_sections)
    })
}

/// Recompute epoch aggregates from observation rows. Used by replay repair and
/// database rebuild; the deterministic sort keeps SQLite writes stable.
pub fn aggregate_from_observations(
    observations: &[PromptPrefixObservation],
) -> Vec<PromptPrefixAggregate> {
    let mut by_epoch: BTreeMap<String, PromptPrefixAggregate> = BTreeMap::new();
    for observation in observations {
        let row = by_epoch
            .entry(observation.cache_epoch.clone())
            .or_insert_with(|| PromptPrefixAggregate {
                project_id: observation.project_id.clone(),
                cache_epoch: observation.cache_epoch.clone(),
                model: observation.model.clone(),
                tool_bundle: observation.tool_bundle.clone(),
                tool_bundle_hash: observation.tool_bundle_hash.clone(),
                core_hash: observation.core_hash.clone(),
                project_hash: observation.project_hash.clone(),
                core_sections: observation.core_sections.clone(),
                project_sections: observation.project_sections.clone(),
                first_seen: observation.first_seen.clone(),
                last_seen: observation.last_seen.clone(),
                ..PromptPrefixAggregate::default()
            });
        row.model = observation.model.clone();
        row.tool_bundle = observation.tool_bundle.clone();
        row.tool_bundle_hash = observation.tool_bundle_hash.clone();
        row.core_hash = observation.core_hash.clone();
        row.project_hash = observation.project_hash.clone();
        row.core_sections = observation.core_sections.clone();
        row.project_sections = observation.project_sections.clone();
        if observation.first_seen < row.first_seen {
            row.first_seen = observation.first_seen.clone();
        }
        if observation.last_seen > row.last_seen {
            row.last_seen = observation.last_seen.clone();
        }
        row.observed_input_tokens = row
            .observed_input_tokens
            .saturating_add(observation.observed_input_tokens);
        row.observed_cache_read_tokens = row
            .observed_cache_read_tokens
            .saturating_add(observation.observed_cache_read_tokens);
        row.session_count = row.session_count.saturating_add(1);
        row.call_count = row.call_count.saturating_add(observation.calls);
    }
    by_epoch.into_values().collect()
}

pub fn aggregates_match(
    projected: &[PromptPrefixAggregate],
    expected: &[PromptPrefixAggregate],
) -> bool {
    if projected.len() != expected.len() {
        return false;
    }
    let mut projected = projected.to_vec();
    projected.sort_by(|left, right| left.cache_epoch.cmp(&right.cache_epoch));
    projected.iter().zip(expected).all(|(left, right)| {
        left.project_id == right.project_id
            && left.cache_epoch == right.cache_epoch
            && left.model == right.model
            && left.tool_bundle == right.tool_bundle
            && left.tool_bundle_hash == right.tool_bundle_hash
            && left.core_hash == right.core_hash
            && left.project_hash == right.project_hash
            && left.core_sections == right.core_sections
            && left.project_sections == right.project_sections
            && left.first_seen == right.first_seen
            && left.last_seen == right.last_seen
            && left.observed_input_tokens == right.observed_input_tokens
            && left.observed_cache_read_tokens == right.observed_cache_read_tokens
            && left.session_count == right.session_count
            && left.call_count == right.call_count
    })
}

/// Build the bounded, chronological epoch timeline from `context.prompt_prefix`
/// and `context.compaction.cache_reset` events.
///
/// The timeline is descriptive only. A compaction entry says the conversation
/// cache boundary moved; it does not claim the stable epoch changed. Prefix
/// entries carry the deterministic change reason recorded when the run
/// started.
pub fn epoch_timeline(
    prefix_events: &[Event],
    reset_events: &[Event],
    limit: usize,
) -> (Vec<CacheEpochTimelineEntry>, bool) {
    let limit = limit.clamp(1, MAX_CACHE_EPOCH_TIMELINE);
    let mut entries = Vec::new();
    for event in prefix_events {
        let epoch = finalized_epoch_hash(&event.payload).map(str::to_string);
        let change_reason = event
            .payload
            .get("cache_epoch_change_reason")
            .and_then(Value::as_str)
            .map(str::to_string);
        let model = event
            .payload
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string);
        let tool_bundle = event
            .payload
            .get("tool_bundle")
            .and_then(Value::as_str)
            .map(str::to_string);
        let prefix_hash = event
            .payload
            .get("prefix_hash")
            .and_then(Value::as_str)
            .map(str::to_string);
        entries.push(CacheEpochTimelineEntry {
            at: event.created_at.clone(),
            kind: "prefix".to_string(),
            cache_epoch: epoch,
            change_reason,
            model,
            tool_bundle,
            prefix_hash,
            compaction_count: None,
        });
    }
    for event in reset_events {
        let epoch = event
            .payload
            .get("cache_epoch_before")
            .or_else(|| event.payload.get("cache_epoch_after"))
            .and_then(Value::as_str)
            .filter(|value| is_cache_epoch(value))
            .map(str::to_string);
        entries.push(CacheEpochTimelineEntry {
            at: event.created_at.clone(),
            kind: "compaction_reset".to_string(),
            cache_epoch: epoch,
            change_reason: Some("compaction".to_string()),
            model: None,
            tool_bundle: None,
            prefix_hash: None,
            compaction_count: event
                .payload
                .get("compaction_count")
                .and_then(Value::as_u64),
        });
    }
    entries.sort_by(|left, right| left.at.cmp(&right.at));
    let truncated = entries.len() > limit;
    if truncated {
        entries = entries.split_off(entries.len() - limit);
    }
    (entries, truncated)
}

/// Build the bounded epoch summary list used by `session.cache.epoch`.
pub fn summarize_epochs(
    aggregates: &[PromptPrefixAggregate],
    observations: &[PromptPrefixObservation],
    limit: usize,
) -> Vec<vp_protocol::types::CacheEpochSummary> {
    let limit = limit.clamp(1, MAX_CACHE_EPOCH_AGGREGATES);
    let by_epoch = observations
        .iter()
        .map(|row| (row.cache_epoch.as_str(), row))
        .collect::<BTreeMap<_, _>>();
    let mut rows = aggregates
        .iter()
        .map(|aggregate| {
            let this_session = by_epoch.get(aggregate.cache_epoch.as_str()).copied();
            let observed_sessions = aggregate.session_count.max(1);
            vp_protocol::types::CacheEpochSummary {
                cache_epoch: aggregate.cache_epoch.clone(),
                model: aggregate.model.clone(),
                tool_bundle: aggregate.tool_bundle.clone(),
                tool_bundle_hash: aggregate.tool_bundle_hash.clone(),
                core_hash: aggregate.core_hash.clone(),
                project_hash: aggregate.project_hash.clone(),
                core_sections: aggregate.core_sections.clone(),
                project_sections: aggregate.project_sections.clone(),
                first_seen: aggregate.first_seen.clone(),
                last_seen: aggregate.last_seen.clone(),
                session_count: aggregate.session_count,
                call_count: aggregate.call_count,
                observed_input_tokens: aggregate.observed_input_tokens,
                observed_cache_read_tokens: aggregate.observed_cache_read_tokens,
                cache_hit_ratio: if aggregate.observed_input_tokens == 0 {
                    0.0
                } else {
                    (aggregate.observed_cache_read_tokens as f64
                        / aggregate.observed_input_tokens as f64)
                        .clamp(0.0, 1.0)
                },
                this_session_calls: this_session.map(|row| row.calls).unwrap_or(0),
                this_session_input_tokens: this_session
                    .map(|row| row.observed_input_tokens)
                    .unwrap_or(0),
                this_session_cache_read_tokens: this_session
                    .map(|row| row.observed_cache_read_tokens)
                    .unwrap_or(0),
                reused_from_sessions: if this_session.is_some() {
                    observed_sessions.saturating_sub(1)
                } else {
                    observed_sessions
                },
            }
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .last_seen
            .cmp(&left.last_seen)
            .then_with(|| left.cache_epoch.cmp(&right.cache_epoch))
    });
    rows.truncate(limit);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use vp_protocol::types::Event;
    use vp_providers::ToolSchema;

    use crate::prompt_cache::PromptSections;

    fn tool(name: &str) -> ToolSchema {
        ToolSchema {
            name: name.to_string(),
            description: format!("{name} tool"),
            parameters: serde_json::json!({"type": "object"}),
        }
    }

    fn v3_sections(project: &str) -> PromptSections {
        let head = vec![
            crate::prompt_cache::PromptHeading::new("core/charter", "charter bytes", 10, false),
            crate::prompt_cache::PromptHeading::new("core/recipes", "recipe bytes", 20, false),
            crate::prompt_cache::PromptHeading::new("project/sources", project, 30, false),
            crate::prompt_cache::PromptHeading::new("project/skills", "skills", 40, false),
            crate::prompt_cache::PromptHeading::new("session/context", "session goal", 50, false),
            crate::prompt_cache::PromptHeading::new("run/task", "task objective", 60, false),
        ];
        let tail = vec![crate::prompt_cache::PromptHeading::new(
            "tail/memory",
            "memory brief",
            90,
            true,
        )];
        PromptSections {
            core: "charter bytes\n\nrecipe bytes".to_string(),
            project: format!("{project}\n\nskills"),
            volatile: String::new(),
            layout: crate::instruction::PROMPT_LAYOUT_V3,
            head,
            tail,
        }
    }

    fn epoch_tools() -> Vec<ToolSchema> {
        vec![tool("read_file"), tool("write_file")]
    }

    fn event(seq: u64, kind: &str, payload: Value) -> Event {
        Event {
            session_id: "ses-1".to_string(),
            seq,
            kind: kind.to_string(),
            payload,
            created_at: format!("2026-11-12T00:00:{seq:02}Z"),
        }
    }

    fn cache_event(seq: u64, epoch: &CacheEpoch, input: u64, read: u64) -> Event {
        let mut payload = serde_json::json!({
            "input_tokens": input,
            "cache_read_tokens": read,
            "tool_bundle": "leader",
            "tool_bundle_hash": "bundle",
            "warm": false,
        });
        for (key, value) in epoch.payload_fields() {
            payload[key] = value;
        }
        event(seq, "context.cache", payload)
    }

    #[test]
    fn epoch_is_stable_across_prompt_memory_and_tail_changes() {
        let tools = epoch_tools();
        let first = v3_sections("source brief v1").hashes("deepseek-flash", &tools);
        let mut second = v3_sections("source brief v1");
        second.head[4].text = "a completely different session goal".to_string();
        second.head[5].text = "a completely different task".to_string();
        second.tail[0].text = "a new memory brief".to_string();
        second.volatile = "volatile run context".to_string();
        let second = second.hashes("deepseek-flash", &tools);

        assert_ne!(
            first.prefix_hash, second.prefix_hash,
            "the full prefix identity may change with session context"
        );
        let left = CacheEpoch::from_hashes("deepseek-flash", &first);
        let right = CacheEpoch::from_hashes("deepseek-flash", &second);
        assert_eq!(left.hash, right.hash);
        assert_eq!(left.identity.core_sections, right.identity.core_sections);
        assert_eq!(
            left.identity.project_sections,
            right.identity.project_sections
        );
    }

    #[test]
    fn epoch_changes_on_model_tools_project_and_core_changes() {
        let tools = epoch_tools();
        let baseline = CacheEpoch::from_hashes(
            "deepseek-flash",
            &v3_sections("source brief v1").hashes("deepseek-flash", &tools),
        );
        let model = CacheEpoch::from_hashes(
            "deepseek-reasoner",
            &v3_sections("source brief v1").hashes("deepseek-reasoner", &tools),
        );
        assert_ne!(baseline.hash, model.hash);
        assert_eq!(
            epoch_change_reason(Some(&baseline.identity), &model.identity),
            CacheEpochChangeReason::Model
        );

        let more_tools = vec![tool("read_file"), tool("write_file"), tool("apply_patch")];
        let tools_changed = CacheEpoch::from_hashes(
            "deepseek-flash",
            &v3_sections("source brief v1").hashes("deepseek-flash", &more_tools),
        );
        assert_ne!(baseline.hash, tools_changed.hash);
        assert_eq!(
            epoch_change_reason(Some(&baseline.identity), &tools_changed.identity),
            CacheEpochChangeReason::Tools
        );

        let project_changed = CacheEpoch::from_hashes(
            "deepseek-flash",
            &v3_sections("source brief v2").hashes("deepseek-flash", &tools),
        );
        assert_ne!(baseline.hash, project_changed.hash);
        assert_eq!(
            epoch_change_reason(Some(&baseline.identity), &project_changed.identity),
            CacheEpochChangeReason::Project
        );

        let mut core_changed_sections = v3_sections("source brief v1");
        core_changed_sections.head[0].text = "new charter".to_string();
        core_changed_sections.core = "new charter\n\nrecipe bytes".to_string();
        let core_changed = CacheEpoch::from_hashes(
            "deepseek-flash",
            &core_changed_sections.hashes("deepseek-flash", &tools),
        );
        assert_ne!(baseline.hash, core_changed.hash);
        assert_eq!(
            epoch_change_reason(Some(&baseline.identity), &core_changed.identity),
            CacheEpochChangeReason::Core
        );
    }

    #[test]
    fn epoch_hash_is_deterministic_and_rejects_placeholders() {
        let epoch = CacheEpoch::from_hashes(
            "deepseek-flash",
            &v3_sections("sources").hashes("deepseek-flash", &epoch_tools()),
        );
        assert!(is_cache_epoch(&epoch.hash));
        assert_eq!(epoch.hash, epoch.identity.hash());
        assert_eq!(epoch.short_hash().len(), 12);
        assert!(!is_cache_epoch(&"A".repeat(64)));
        assert!(!is_cache_epoch("short"));

        let mut payload = epoch.payload_fields();
        payload.insert("cache_epoch".to_string(), Value::String(epoch.hash.clone()));
        let parsed = CacheEpoch::from_event_payload(&Value::Object(
            payload.into_iter().collect::<serde_json::Map<_, _>>(),
        ))
        .expect("complete epoch payload parses");
        assert_eq!(parsed, epoch);

        let mut placeholder = serde_json::json!({
            "cache_epoch": "a".repeat(64),
            "cache_epoch_source": "l1_placeholder",
            "cache_epoch_placeholder": true,
        });
        placeholder["prefix_hash"] = Value::String(epoch.hash.clone());
        assert!(CacheEpoch::from_event_payload(&placeholder).is_none());
    }

    #[test]
    fn epoch_payload_carries_hashes_not_prompt_text() {
        let hashes = v3_sections("do-not-leak-this-source-brief").hashes("m", &epoch_tools());
        let epoch = CacheEpoch::from_hashes("m", &hashes);
        let payload = Value::Object(epoch.payload_fields().into_iter().collect());
        let encoded = serde_json::to_string(&payload).unwrap();
        assert!(!encoded.contains("charter bytes"));
        assert!(!encoded.contains("recipe bytes"));
        assert!(!encoded.contains("do-not-leak-this-source-brief"));
        assert!(!encoded.contains("memory brief"));
        assert!(!encoded.contains("task objective"));
        assert!(encoded.len() < 4_096, "epoch telemetry must stay bounded");
        assert!(epoch.identity.canonical_material().contains("core/charter"));
        assert!(!epoch.identity.canonical_material().contains("memory brief"));
    }

    #[test]
    fn epoch_change_reason_is_first_for_a_new_session() {
        let epoch = CacheEpoch::from_hashes("m", &v3_sections("s").hashes("m", &epoch_tools()));
        assert_eq!(
            epoch_change_reason(None, &epoch.identity),
            CacheEpochChangeReason::First
        );
        assert_eq!(
            epoch_change_reason(Some(&epoch.identity), &epoch.identity),
            CacheEpochChangeReason::Unchanged
        );
    }

    #[test]
    fn cross_session_attribution_only_uses_the_first_session_call() {
        let first = cross_session_attribution(0, true, 900);
        assert_eq!(first.cross_session_cache_read_tokens, 0);
        let reused = cross_session_attribution(2, true, 900);
        assert_eq!(reused.cross_session_cache_read_tokens, 900);
        assert_eq!(reused.prior_sessions, 2);
        let later = cross_session_attribution(2, false, 900);
        assert_eq!(later.cross_session_cache_read_tokens, 0);
    }

    #[test]
    fn section_reorder_changes_epoch_even_with_the_same_combined_hashes() {
        let tools = epoch_tools();
        let hashes = v3_sections("sources").hashes("m", &tools);
        let baseline = CacheEpoch::from_hashes("m", &hashes);
        let mut reordered = CacheEpochIdentity::from_hashes("m", &hashes);
        reordered.core_sections.reverse();
        assert_ne!(
            baseline.hash,
            reordered.hash(),
            "key/ordinal order is part of the cache identity"
        );

        let mut renamed = CacheEpochIdentity::from_hashes("m", &hashes);
        renamed.core_sections[0].key = "core/renamed".into();
        assert_ne!(baseline.hash, renamed.hash());
    }

    #[test]
    fn v2_combined_sections_still_produce_a_stable_epoch() {
        let tools = epoch_tools();
        let v2 = PromptSections::new("charter", "sources", "volatile run context");
        let first = CacheEpoch::from_hashes("m", &v2.hashes("m", &tools));
        let second = CacheEpoch::from_hashes("m", &v2.hashes("m", &tools));
        assert_eq!(first.hash, second.hash);
        assert_eq!(first.identity.core_sections.len(), 1);
        assert_eq!(first.identity.project_sections.len(), 1);
        assert_eq!(first.identity.core_sections[0].key, "core");
        assert_eq!(first.identity.project_sections[0].key, "project");
    }

    #[test]
    fn finalized_epoch_payload_rejects_hash_identity_mismatch() {
        let epoch = CacheEpoch::from_hashes("m", &v3_sections("s").hashes("m", &epoch_tools()));
        let mut fields: serde_json::Map<String, Value> =
            epoch.payload_fields().into_iter().collect();
        fields.insert("cache_epoch".into(), Value::String("b".repeat(64)));
        assert!(
            CacheEpoch::from_event_payload(&Value::Object(fields)).is_none(),
            "a recorded hash that does not match the identity is corrupt"
        );
    }

    #[test]
    fn reuse_projection_separates_other_sessions_from_this_session() {
        let aggregate = PromptPrefixAggregate {
            cache_epoch: "a".repeat(64),
            session_count: 3,
            call_count: 9,
            ..PromptPrefixAggregate::default()
        };
        let observation = PromptPrefixObservation {
            cache_epoch: "a".repeat(64),
            session_id: "ses".into(),
            calls: 4,
            ..PromptPrefixObservation::default()
        };
        let seen = reuse_from_projection(Some(&aggregate), Some(&observation));
        assert_eq!(seen.prior_sessions, 2);
        assert_eq!(seen.session_calls, 4);
        assert!(!seen.first_session_call);
        let new = reuse_from_projection(Some(&aggregate), None);
        assert_eq!(new.prior_sessions, 3);
        assert_eq!(new.session_calls, 0);
        assert!(new.first_session_call);
        assert_eq!(reuse_from_projection(None, None).prior_sessions, 0);
    }

    #[test]
    fn summarize_epochs_is_bounded_sorted_and_marks_session_contribution() {
        let mut aggregates = Vec::new();
        for (index, byte) in ["a", "b", "c"].iter().enumerate() {
            aggregates.push(PromptPrefixAggregate {
                project_id: "prj".into(),
                cache_epoch: byte.repeat(64),
                model: "m".into(),
                session_count: (index + 1) as u64,
                call_count: 2,
                observed_input_tokens: 1_000,
                observed_cache_read_tokens: 900,
                last_seen: format!("2026-11-12T00:00:0{}Z", index + 1),
                first_seen: "2026-11-12T00:00:00Z".into(),
                ..PromptPrefixAggregate::default()
            });
        }
        let observations = vec![PromptPrefixObservation {
            cache_epoch: "b".repeat(64),
            session_id: "ses".into(),
            calls: 1,
            observed_input_tokens: 500,
            observed_cache_read_tokens: 400,
            ..PromptPrefixObservation::default()
        }];
        let rows = summarize_epochs(&aggregates, &observations, 2);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].cache_epoch, "c".repeat(64));
        assert_eq!(rows[1].cache_epoch, "b".repeat(64));
        assert_eq!(rows[1].this_session_calls, 1);
        assert_eq!(rows[1].this_session_input_tokens, 500);
        assert_eq!(rows[1].reused_from_sessions, 1);
        assert_eq!(rows[0].reused_from_sessions, 3);
    }

    #[test]
    fn timeline_orders_multiple_epochs_and_compaction_boundaries() {
        let first = CacheEpoch::from_hashes("m", &v3_sections("s").hashes("m", &epoch_tools()));
        let mut second_sections = v3_sections("s2");
        second_sections.head[2].text = "s2".into();
        second_sections.project = "s2\n\nskills".into();
        let second = CacheEpoch::from_hashes("m", &second_sections.hashes("m", &epoch_tools()));
        let mut prefix_payload = serde_json::Map::new();
        prefix_payload.insert("cache_epoch".into(), Value::String(first.hash.clone()));
        prefix_payload.insert(
            "cache_epoch_change_reason".into(),
            Value::String("first".into()),
        );
        let mut second_payload = serde_json::Map::new();
        second_payload.insert("cache_epoch".into(), Value::String(second.hash.clone()));
        second_payload.insert(
            "cache_epoch_change_reason".into(),
            Value::String("project".into()),
        );
        let prefixes = vec![
            Event {
                session_id: "ses".into(),
                seq: 1,
                kind: "context.prompt_prefix".into(),
                payload: Value::Object(prefix_payload),
                created_at: "2026-11-12T00:00:01Z".into(),
            },
            Event {
                session_id: "ses".into(),
                seq: 2,
                kind: "context.prompt_prefix".into(),
                payload: Value::Object(second_payload),
                created_at: "2026-11-12T00:00:03Z".into(),
            },
        ];
        let reset = vec![Event {
            session_id: "ses".into(),
            seq: 3,
            kind: "context.compaction.cache_reset".into(),
            payload: serde_json::json!({
                "cache_epoch_before": second.hash,
                "cache_epoch_after": second.hash,
                "compaction_count": 1,
            }),
            created_at: "2026-11-12T00:00:02Z".into(),
        }];
        let (timeline, truncated) = epoch_timeline(&prefixes, &reset, 10);
        assert!(!truncated);
        assert_eq!(timeline.len(), 3);
        assert_eq!(timeline[0].kind, "prefix");
        assert_eq!(timeline[1].kind, "compaction_reset");
        assert_eq!(timeline[2].kind, "prefix");
        assert_eq!(timeline[2].change_reason.as_deref(), Some("project"));
        assert_ne!(first.hash, second.hash);
    }

    #[test]
    fn low_hit_threshold_is_explicit_and_clamped() {
        assert!(!is_low_cache_hit(0, 0));
        assert!(is_low_cache_hit(1_000, 0));
        assert!(is_low_cache_hit(1_000, 499));
        assert!(!is_low_cache_hit(1_000, 500));
        assert!(!is_low_cache_hit(1_000, 2_000));
    }

    #[test]
    fn events_group_into_one_observation_per_epoch() {
        let epoch = CacheEpoch::from_hashes(
            "deepseek-flash",
            &v3_sections("sources").hashes("deepseek-flash", &epoch_tools()),
        );
        let events = vec![
            cache_event(1, &epoch, 1_000, 900),
            cache_event(2, &epoch, 2_000, 1_800),
            event(
                3,
                "context.cache",
                serde_json::json!({"warm": true, "cache_epoch": epoch.hash, "input_tokens": 50}),
            ),
        ];
        let rows = observations_from_events("prj", "ses-1", &events).expect("epoch events");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].calls, 2);
        assert_eq!(rows[0].observed_input_tokens, 3_000);
        assert_eq!(rows[0].observed_cache_read_tokens, 2_700);
        assert_eq!(rows[0].first_seen, "2026-11-12T00:00:01Z");
        assert_eq!(rows[0].last_seen, "2026-11-12T00:00:02Z");
        assert_eq!(rows[0].tool_bundle, "leader");
        assert_eq!(rows[0].model, "deepseek-flash");
        assert!(!rows[0].core_sections.is_empty());
        assert_eq!(
            observations_from_events(
                "prj",
                "ses-1",
                &[event(
                    1,
                    "context.cache",
                    serde_json::json!({"input_tokens": 1})
                )]
            ),
            None,
            "legacy events without an epoch preserve the projection"
        );
    }

    #[test]
    fn l1_placeholder_events_are_not_epoch_authority() {
        let placeholder = event(
            1,
            "context.cache",
            serde_json::json!({
                "input_tokens": 1000,
                "cache_read_tokens": 900,
                "cache_epoch": "a".repeat(64),
                "cache_epoch_source": "l1_placeholder",
                "cache_epoch_placeholder": true,
                "prompt_prefix_hash": "a".repeat(64),
            }),
        );
        assert_eq!(
            observations_from_events("prj", "ses", std::slice::from_ref(&placeholder)),
            None,
            "L1 placeholders preserve legacy projections instead of creating fake epochs"
        );
        assert!(finalized_epoch_hash(&placeholder.payload).is_none());
        let prefix = event(
            2,
            "context.prompt_prefix",
            serde_json::json!({
                "cache_epoch": "a".repeat(64),
                "cache_epoch_source": "l1_placeholder",
                "cache_epoch_placeholder": true,
            }),
        );
        let (timeline, _) = epoch_timeline(&[prefix], &[], 10);
        assert_eq!(timeline.len(), 1);
        assert!(timeline[0].cache_epoch.is_none());
    }

    #[test]
    fn projection_compare_detects_drift() {
        let epoch = CacheEpoch::from_hashes("m", &v3_sections("s").hashes("m", &epoch_tools()));
        let expected = observations_from_events("prj", "ses", &[cache_event(1, &epoch, 100, 80)])
            .expect("rows");
        assert!(observations_match(&expected, &expected));
        let mut drifted = expected.clone();
        drifted[0].observed_cache_read_tokens = 0;
        assert!(!observations_match(&drifted, &expected));
        let aggregates = aggregate_from_observations(&expected);
        assert_eq!(aggregates.len(), 1);
        assert_eq!(aggregates[0].session_count, 1);
        assert_eq!(aggregates[0].call_count, 1);
        assert!(aggregates_match(
            &aggregates,
            &aggregate_from_observations(&expected)
        ));
        let mut aggregate_drift = aggregates.clone();
        aggregate_drift[0].call_count = 9;
        assert!(!aggregates_match(&aggregate_drift, &aggregates));
    }

    #[test]
    fn aggregate_sums_sessions_and_uses_latest_identity() {
        let epoch = CacheEpoch::from_hashes("m", &v3_sections("s").hashes("m", &epoch_tools()));
        let mut first =
            observations_from_events("prj", "a", &[cache_event(1, &epoch, 100, 40)]).unwrap();
        let second =
            observations_from_events("prj", "b", &[cache_event(2, &epoch, 300, 280)]).unwrap();
        let mut all = first.clone();
        all.extend(second);
        let aggregates = aggregate_from_observations(&all);
        assert_eq!(aggregates[0].session_count, 2);
        assert_eq!(aggregates[0].observed_input_tokens, 400);
        assert_eq!(aggregates[0].observed_cache_read_tokens, 320);
        assert_eq!(aggregates[0].call_count, 2);
        first[0].model = "changed".into();
        let summaries = summarize_epochs(&aggregates, &first, 10);
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].this_session_calls, 1);
        assert_eq!(summaries[0].reused_from_sessions, 1);
        assert!((summaries[0].cache_hit_ratio - 0.8).abs() < 1e-9);
    }

    #[test]
    fn timeline_is_chronological_and_bounded() {
        let epoch = CacheEpoch::from_hashes("m", &v3_sections("s").hashes("m", &epoch_tools()));
        let mut fields = epoch.payload_fields();
        fields.insert(
            "cache_epoch_change_reason".to_string(),
            Value::String("first".to_string()),
        );
        fields.insert("model".to_string(), Value::String("m".to_string()));
        fields.insert(
            "tool_bundle".to_string(),
            Value::String("leader".to_string()),
        );
        let prefix = vec![event(
            2,
            "context.prompt_prefix",
            Value::Object(fields.into_iter().collect()),
        )];
        let reset = vec![event(
            1,
            "context.compaction.cache_reset",
            serde_json::json!({
                "cache_epoch_before": epoch.hash,
                "cache_epoch_after": epoch.hash,
                "compaction_count": 1,
            }),
        )];
        let (timeline, truncated) = epoch_timeline(&prefix, &reset, 10);
        assert!(!truncated);
        assert_eq!(timeline.len(), 2);
        assert_eq!(timeline[0].kind, "compaction_reset");
        assert_eq!(timeline[1].kind, "prefix");
        assert_eq!(timeline[1].change_reason.as_deref(), Some("first"));
        let (bounded, truncated) = epoch_timeline(&prefix, &reset, 1);
        assert!(truncated);
        assert_eq!(bounded.len(), 1);
        assert_eq!(bounded[0].kind, "prefix");
    }
}

//! Cache-first prompt assembly and attribution.
//!
//! The runtime splits the system prompt into three deterministic sections:
//!
//! - `core`: role charter, security/output contract, and stable project
//!   conventions. Shared by every agent with the same role/profile/project.
//! - `project`: indexed source brief and skill catalog. Semi-stable; a source
//!   or skill edit invalidates this section without moving `core`.
//! - `volatile`: run context, objective, and the retrieved memory brief. It is
//!   always last and never receives an explicit cache breakpoint.
//!
//! This module contains no provider I/O. It owns hashes, breakpoint plans,
//! miss-reason attribution, savings math, and compaction cache generations so
//! the agent loop and protocol projections can share one deterministic model.

use serde_json::Value;
use vp_protocol::types::{CachePolicy, SessionCacheCall};
use vp_providers::{
    CacheBreakpointPlan, CacheTtl, ChatMessage, PromptCacheConfig, SystemSection,
    plan_cache_breakpoints,
};

use crate::sha256_hex;

pub const PROMPT_SECTION_VERSION: u32 = 2;

pub const CORE_SECTION: &str = "core";
pub const PROJECT_SECTION: &str = "project";
pub const VOLATILE_SECTION: &str = "volatile";

/// Deterministic system-prompt sections. `render()` is the backward-compatible
/// combined `system` string; adapters receive the exact boundaries separately.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptSections {
    pub core: String,
    pub project: String,
    pub volatile: String,
}

impl PromptSections {
    pub fn new(
        core: impl Into<String>,
        project: impl Into<String>,
        volatile: impl Into<String>,
    ) -> Self {
        Self {
            core: core.into(),
            project: project.into(),
            volatile: volatile.into(),
        }
    }

    pub fn has_core(&self) -> bool {
        !self.core.is_empty()
    }

    pub fn has_project(&self) -> bool {
        !self.project.is_empty()
    }

    pub fn has_volatile(&self) -> bool {
        !self.volatile.is_empty()
    }

    /// Combined compatibility string. Stable sections always come first; the
    /// volatile section is never moved ahead of them.
    pub fn render(&self) -> String {
        let mut rendered = String::new();
        if self.has_core() {
            rendered.push_str(&self.core);
        }
        if self.has_project() {
            if !rendered.is_empty() {
                rendered.push_str("\n\n");
            }
            rendered.push_str(&self.project);
        }
        if self.has_volatile() {
            if !rendered.is_empty() {
                rendered.push_str("\n\n");
            }
            rendered.push_str(&self.volatile);
        }
        rendered
    }

    /// The stable prefix marker retained for older adapters and for the
    /// Anthropic legacy fallback. It is `core + project`, never volatile.
    pub fn stable_prefix(&self) -> String {
        let mut stable = String::new();
        if self.has_core() {
            stable.push_str(&self.core);
        }
        if self.has_project() {
            if !stable.is_empty() {
                stable.push_str("\n\n");
            }
            stable.push_str(&self.project);
        }
        stable
    }

    pub fn provider_sections(&self) -> Vec<SystemSection> {
        let mut sections = Vec::with_capacity(3);
        if self.has_core() {
            sections.push(SystemSection::new(CORE_SECTION, self.core.clone()));
        }
        if self.has_project() {
            sections.push(SystemSection::new(PROJECT_SECTION, self.project.clone()));
        }
        if self.has_volatile() {
            sections.push(SystemSection::new(VOLATILE_SECTION, self.volatile.clone()));
        }
        sections
    }

    pub fn hashes(&self, model: &str, tools: &[vp_providers::ToolSchema]) -> PromptSectionHashes {
        PromptSectionHashes::compute(self, model, tools)
    }
}

/// Component hashes recorded on every prompt-prefix and cache event. They are
/// SHA-256 hex strings; no prompt text is persisted in cache telemetry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptSectionHashes {
    pub core_hash: String,
    pub project_hash: String,
    pub tools_hash: String,
    pub volatile_hash: String,
    pub stable_system_hash: String,
    /// Hash of `(version, model, tools, core, project)`. Volatile content is
    /// intentionally excluded so memory changes cannot invalidate the shared
    /// prefix.
    pub prefix_hash: String,
}

impl PromptSectionHashes {
    pub fn compute(
        sections: &PromptSections,
        model: &str,
        tools: &[vp_providers::ToolSchema],
    ) -> Self {
        let mut sorted = tools.to_vec();
        sorted.sort_by(|left, right| left.name.cmp(&right.name));
        let tool_bundle = serde_json::to_string(&sorted).unwrap_or_default();
        let tools_hash = sha256_hex(tool_bundle.as_bytes());
        let core_hash = sha256_hex(sections.core.as_bytes());
        let project_hash = sha256_hex(sections.project.as_bytes());
        let volatile_hash = sha256_hex(sections.volatile.as_bytes());
        let stable_system_hash = sha256_hex(sections.stable_prefix().as_bytes());
        let prefix_hash = prefix_hash_from_parts(model, &tools_hash, &core_hash, &project_hash);
        Self {
            core_hash,
            project_hash,
            tools_hash,
            volatile_hash,
            stable_system_hash,
            prefix_hash,
        }
    }

    pub fn payload(&self) -> Value {
        serde_json::json!({
            "core_hash": self.core_hash,
            "project_hash": self.project_hash,
            "tools_hash": self.tools_hash,
            "volatile_hash": self.volatile_hash,
            "stable_system_hash": self.stable_system_hash,
            "prefix_hash": self.prefix_hash,
        })
    }

    /// Fingerprint used by miss attribution. `stable_system_hash` is omitted:
    /// it is a function of core+project and would be redundant.
    pub fn fingerprint(&self) -> CacheCallFingerprint {
        CacheCallFingerprint {
            core_hash: self.core_hash.clone(),
            project_hash: self.project_hash.clone(),
            tools_hash: self.tools_hash.clone(),
            volatile_hash: self.volatile_hash.clone(),
            prefix_hash: self.prefix_hash.clone(),
        }
    }
}

/// Hash of the reusable stable prefix. Version and model are included so a
/// model switch or an intentional prompt-layout revision creates a new
/// generation instead of silently reusing an incompatible prefix.
pub fn prefix_hash_from_parts(
    model: &str,
    tools_hash: &str,
    core_hash: &str,
    project_hash: &str,
) -> String {
    sha256_hex(
        format!("{PROMPT_SECTION_VERSION}\n{model}\n{tools_hash}\n{core_hash}\n{project_hash}")
            .as_bytes(),
    )
}

/// Map a project policy and role to the provider request policy.
pub fn prompt_cache_config(policy: &CachePolicy, role: &str) -> PromptCacheConfig {
    PromptCacheConfig::new(
        policy.enabled,
        CacheTtl::parse(policy.ttl_for(role)).unwrap_or_default(),
        policy.automatic_caching,
    )
}

/// Compute the actual provider plan for one request. The runtime records this
/// before the provider call; adapter tests assert the same wire shape.
pub fn request_cache_plan(
    provider_name: &str,
    sections: &PromptSections,
    tool_count: usize,
    message_count: usize,
    policy: &CachePolicy,
    role: &str,
) -> CacheBreakpointPlan {
    let config = prompt_cache_config(policy, role);
    plan_cache_breakpoints(
        provider_name,
        &sections.provider_sections(),
        tool_count,
        message_count,
        config,
    )
}

/// Cache-call identity for miss attribution. This is deliberately narrower
/// than the full prompt: only components that can invalidate the reusable
/// prefix take part.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CacheCallFingerprint {
    pub core_hash: String,
    pub project_hash: String,
    pub tools_hash: String,
    pub volatile_hash: String,
    pub prefix_hash: String,
}

impl CacheCallFingerprint {
    pub fn from_event_payload(payload: &Value) -> Option<Self> {
        let get = |key: &str| payload.get(key).and_then(Value::as_str).map(str::to_string);
        let prefix_hash = get("prompt_prefix_hash").or_else(|| get("prefix_hash"))?;
        Some(Self {
            core_hash: get("core_hash").unwrap_or_default(),
            project_hash: get("project_hash").unwrap_or_default(),
            tools_hash: get("tools_hash").unwrap_or_default(),
            volatile_hash: get("volatile_hash").unwrap_or_default(),
            prefix_hash,
        })
    }
}

/// Derived miss attribution. A reason is recorded only when the provider
/// reported zero cache reads for the call; `None` means the call hit the
/// prefix cache or no attribution was needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheMissReason {
    FirstCall,
    PrefixChanged,
    ToolsChanged,
    ProjectChanged,
    TtlOrProviderMiss,
    CompactionReset,
    Unknown,
}

impl CacheMissReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FirstCall => "first_call",
            Self::PrefixChanged => "prefix_changed",
            Self::ToolsChanged => "tools_changed",
            Self::ProjectChanged => "project_changed",
            Self::TtlOrProviderMiss => "ttl_or_provider_miss",
            Self::CompactionReset => "compaction_reset",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "first_call" => Self::FirstCall,
            "prefix_changed" => Self::PrefixChanged,
            "tools_changed" => Self::ToolsChanged,
            "project_changed" => Self::ProjectChanged,
            "ttl_or_provider_miss" => Self::TtlOrProviderMiss,
            "compaction_reset" => Self::CompactionReset,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }
}

/// In-run miss tracker. It is seeded from the latest persisted cache call so a
/// resumed run can still attribute a tool/project change correctly, and it
/// marks the next observation after a successful compaction.
#[derive(Debug, Clone, Default)]
pub struct CacheCallTracker {
    last: Option<CacheCallFingerprint>,
    compaction_pending: bool,
}

impl CacheCallTracker {
    pub fn new(last: Option<CacheCallFingerprint>) -> Self {
        Self {
            last,
            compaction_pending: false,
        }
    }

    pub fn from_event(payload: Option<&Value>) -> Self {
        Self::new(payload.and_then(CacheCallFingerprint::from_event_payload))
    }

    pub fn last(&self) -> Option<&CacheCallFingerprint> {
        self.last.as_ref()
    }

    pub fn mark_compaction(&mut self) {
        self.compaction_pending = true;
    }

    pub fn compaction_pending(&self) -> bool {
        self.compaction_pending
    }

    /// Observe one call and return a miss reason when the provider reported no
    /// cache reads. A hit clears the compaction marker because the following
    /// call's prefix is no longer the pre-compaction generation.
    pub fn observe(
        &mut self,
        current: &CacheCallFingerprint,
        cache_read_tokens: u64,
    ) -> Option<CacheMissReason> {
        let reason = if cache_read_tokens > 0 {
            None
        } else if let Some(last) = self.last.as_ref() {
            if last.tools_hash != current.tools_hash {
                Some(CacheMissReason::ToolsChanged)
            } else if last.project_hash != current.project_hash {
                Some(CacheMissReason::ProjectChanged)
            } else if last.core_hash != current.core_hash || last.prefix_hash != current.prefix_hash
            {
                Some(CacheMissReason::PrefixChanged)
            } else if self.compaction_pending {
                Some(CacheMissReason::CompactionReset)
            } else {
                Some(CacheMissReason::TtlOrProviderMiss)
            }
        } else if self.compaction_pending {
            Some(CacheMissReason::CompactionReset)
        } else {
            Some(CacheMissReason::FirstCall)
        };
        self.last = Some(current.clone());
        // The marker describes the next observation only. Whether that call
        // hit or missed, a later call compares against the new fingerprint and
        // must not keep reporting the same compaction reset.
        self.compaction_pending = false;
        reason
    }
}

/// Estimated no-cache cost minus billed cost for one call. `input` already
/// includes cache reads and writes. Negative values are valid when cache
/// writes cost more than the reads they save; the runtime reports them honestly
/// instead of clamping the per-call number to look good.
#[allow(clippy::too_many_arguments)]
pub fn estimated_cache_savings_usd(
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    input_price: f64,
    output_price: f64,
    cache_read_price: f64,
    cache_write_price: f64,
) -> f64 {
    let cache_read = cache_read.min(input);
    let cache_write = cache_write.min(input.saturating_sub(cache_read));
    let uncached = input.saturating_sub(cache_read).saturating_sub(cache_write);
    let no_cache = per_million(input, input_price) + per_million(output, output_price);
    let billed = per_million(uncached, input_price)
        + per_million(cache_read, cache_read_price)
        + per_million(cache_write, cache_write_price)
        + per_million(output, output_price);
    let savings = no_cache - billed;
    if savings.is_finite() { savings } else { 0.0 }
}

fn per_million(tokens: u64, price: f64) -> f64 {
    tokens as f64 * price / 1_000_000.0
}

/// Hash of the active conversation generation at one instant. Compaction
/// changes this even when the stable system/tool prefix is unchanged.
pub fn conversation_generation(prefix_hash: &str, messages: &[ChatMessage]) -> String {
    let mut material = String::with_capacity(prefix_hash.len() + messages.len() * 32);
    material.push_str(prefix_hash);
    material.push('\u{1f}');
    for message in messages {
        material.push_str(&message.role);
        material.push('\u{1f}');
        material.push_str(&message.content.len().to_string());
        material.push('\u{1f}');
        material.push_str(&message.content);
        material.push('\u{1f}');
        if let Some(calls) = &message.tool_calls {
            material.push_str(&serde_json::to_string(calls).unwrap_or_default());
        }
        material.push('\u{1f}');
        if let Some(call_id) = &message.tool_call_id {
            material.push_str(call_id);
        }
        material.push('\u{1e}');
    }
    sha256_hex(material.as_bytes())
}

/// Round a USD estimate for persisted telemetry without pretending to have
/// invoice precision.
pub fn round_usd(value: f64) -> f64 {
    if !value.is_finite() {
        return 0.0;
    }
    (value * 1_000_000.0).round() / 1_000_000.0
}

/// Build the bounded protocol record for one `context.cache` event payload.
pub fn cache_call_record(payload: &Value, created_at: &str) -> SessionCacheCall {
    let u64 = |key: &str| payload.get(key).and_then(Value::as_u64).unwrap_or(0);
    let text = |key: &str| payload.get(key).and_then(Value::as_str).map(str::to_string);
    SessionCacheCall {
        at: created_at.to_string(),
        agent_name: text("agent_name"),
        role: text("agent_role"),
        model: text("model"),
        input_tokens: u64("input_tokens"),
        output_tokens: u64("output_tokens"),
        cache_read_tokens: u64("cache_read_tokens"),
        cache_write_tokens: u64("cache_write_tokens"),
        cache_hit_ratio: payload
            .get("cache_hit_ratio")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        cache_breakpoints: payload
            .get("cache_breakpoints")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32,
        cache_ttl: text("cache_ttl"),
        first_token_ms: payload.get("first_token_ms").and_then(Value::as_u64),
        core_hash: text("core_hash"),
        project_hash: text("project_hash"),
        tools_hash: text("tools_hash"),
        volatile_hash: text("volatile_hash"),
        prompt_prefix_hash: text("prompt_prefix_hash"),
        estimated_savings_usd: payload
            .get("estimated_savings_usd")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        miss_reason: text("miss_reason"),
        warm: payload
            .get("warm")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

/// Aggregates a bounded window of cache and compaction events for the
/// `session.cache` projection. All values are derived from persisted telemetry;
/// missing or legacy payloads contribute zero rather than a fabricated fact.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CacheTelemetryAggregate {
    pub calls: u64,
    pub warm_calls: u64,
    pub miss_reason_counts: Vec<vp_protocol::types::CacheCountBucket>,
    pub ttl_counts: Vec<vp_protocol::types::CacheCountBucket>,
    pub breakpoint_counts: Vec<vp_protocol::types::CacheCountBucket>,
    pub savings_by_role: Vec<vp_protocol::types::CacheSavingsBucket>,
    pub first_token_p50_ms: Option<u64>,
    pub first_token_p95_ms: Option<u64>,
    pub compaction_resets: u64,
    pub compaction_invalidation_tokens: u64,
    pub compaction_invalidation_estimated_usd: f64,
}

/// Nearest-rank percentile over a small sorted sample. Returns `None` for an
/// empty sample; never invents a latency for missing telemetry.
fn nearest_rank(sorted: &[u64], percentile: u64) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (percentile as f64 / 100.0 * sorted.len() as f64).ceil() as usize;
    let index = rank.saturating_sub(1).min(sorted.len() - 1);
    Some(sorted[index])
}

/// Aggregate cache calls and compaction resets. Buckets are sorted by key so
/// repeated calls produce byte-identical protocol output.
pub fn aggregate_cache_events(
    cache_events: &[vp_protocol::types::Event],
    compaction_events: &[vp_protocol::types::Event],
) -> CacheTelemetryAggregate {
    use std::collections::BTreeMap;

    let mut aggregate = CacheTelemetryAggregate::default();
    let mut misses: BTreeMap<String, u64> = BTreeMap::new();
    let mut ttls: BTreeMap<String, u64> = BTreeMap::new();
    let mut breakpoints: BTreeMap<String, u64> = BTreeMap::new();
    let mut savings: BTreeMap<String, (u64, f64)> = BTreeMap::new();
    let mut first_tokens = Vec::new();
    for event in cache_events {
        let payload = &event.payload;
        let warm = payload
            .get("warm")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if warm {
            aggregate.warm_calls = aggregate.warm_calls.saturating_add(1);
        } else {
            aggregate.calls = aggregate.calls.saturating_add(1);
        }
        if let Some(reason) = payload.get("miss_reason").and_then(Value::as_str) {
            *misses.entry(reason.to_string()).or_default() += 1;
        }
        if let Some(ttl) = payload.get("cache_ttl").and_then(Value::as_str) {
            *ttls.entry(ttl.to_string()).or_default() += 1;
        }
        if let Some(count) = payload.get("cache_breakpoints").and_then(Value::as_u64) {
            *breakpoints.entry(count.to_string()).or_default() += 1;
        }
        let role = payload
            .get("agent_role")
            .and_then(Value::as_str)
            .unwrap_or("(unassigned)")
            .to_string();
        let value = payload
            .get("estimated_savings_usd")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let entry = savings.entry(role).or_insert((0, 0.0));
        entry.0 = entry.0.saturating_add(1);
        entry.1 += value;
        if let Some(latency) = payload.get("first_token_ms").and_then(Value::as_u64)
            && latency > 0
        {
            first_tokens.push(latency);
        }
    }
    first_tokens.sort_unstable();
    aggregate.first_token_p50_ms = nearest_rank(&first_tokens, 50);
    aggregate.first_token_p95_ms = nearest_rank(&first_tokens, 95);
    aggregate.miss_reason_counts = misses
        .into_iter()
        .map(|(key, calls)| vp_protocol::types::CacheCountBucket { key, calls })
        .collect();
    aggregate.ttl_counts = ttls
        .into_iter()
        .map(|(key, calls)| vp_protocol::types::CacheCountBucket { key, calls })
        .collect();
    aggregate.breakpoint_counts = breakpoints
        .into_iter()
        .map(|(key, calls)| vp_protocol::types::CacheCountBucket { key, calls })
        .collect();
    aggregate.savings_by_role = savings
        .into_iter()
        .map(
            |(role, (calls, estimated_savings_usd))| vp_protocol::types::CacheSavingsBucket {
                role,
                calls,
                estimated_savings_usd: round_usd(estimated_savings_usd),
            },
        )
        .collect();
    for event in compaction_events {
        if event
            .payload
            .get("cache_reset_reason")
            .and_then(Value::as_str)
            == Some("compaction")
        {
            aggregate.compaction_resets = aggregate.compaction_resets.saturating_add(1);
        }
        aggregate.compaction_invalidation_tokens =
            aggregate.compaction_invalidation_tokens.saturating_add(
                event
                    .payload
                    .get("cache_invalidation_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            );
        aggregate.compaction_invalidation_estimated_usd += event
            .payload
            .get("cache_invalidation_estimated_usd")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
    }
    aggregate.compaction_invalidation_estimated_usd =
        round_usd(aggregate.compaction_invalidation_estimated_usd);
    aggregate
}

#[cfg(test)]
mod tests {
    use super::*;
    use vp_providers::ToolSchema;

    fn tool(name: &str) -> ToolSchema {
        ToolSchema {
            name: name.to_string(),
            description: format!("{name} tool"),
            parameters: serde_json::json!({"type": "object"}),
        }
    }

    fn sections() -> PromptSections {
        PromptSections::new("role charter", "sources and skills", "run context")
    }

    #[test]
    fn render_is_stable_first_and_skips_empty_sections() {
        assert_eq!(
            PromptSections::new("core", "", "volatile").render(),
            "core\n\nvolatile"
        );
        assert_eq!(
            PromptSections::new("", "project", "volatile").render(),
            "project\n\nvolatile"
        );
        assert_eq!(PromptSections::new("core", "", "").render(), "core");
        assert_eq!(PromptSections::default().render(), "");
    }

    #[test]
    fn provider_sections_preserve_core_project_volatile_order() {
        let provider = sections().provider_sections();
        assert_eq!(provider.len(), 3);
        assert_eq!(provider[0].kind, CORE_SECTION);
        assert_eq!(provider[1].kind, PROJECT_SECTION);
        assert_eq!(provider[2].kind, VOLATILE_SECTION);
        assert_eq!(provider[0].text, "role charter");
        assert_eq!(provider[1].text, "sources and skills");
        assert_eq!(provider[2].text, "run context");
    }

    #[test]
    fn memory_brief_never_changes_the_shared_prefix() {
        let tools = vec![tool("read_file")];
        let without_memory = sections().hashes("model", &tools);
        let mut with_memory = sections();
        with_memory.volatile = format!(
            "{}\nRun memory: use the API key from 1Password.",
            with_memory.volatile
        );
        let with_memory = with_memory.hashes("model", &tools);
        assert_eq!(without_memory.core_hash, with_memory.core_hash);
        assert_eq!(without_memory.project_hash, with_memory.project_hash);
        assert_eq!(without_memory.tools_hash, with_memory.tools_hash);
        assert_eq!(without_memory.prefix_hash, with_memory.prefix_hash);
        assert_ne!(without_memory.volatile_hash, with_memory.volatile_hash);
    }

    #[test]
    fn source_and_skill_changes_invalidate_only_the_project_component() {
        let tools = vec![tool("read_file")];
        let baseline = sections().hashes("model", &tools);
        let mut updated = sections();
        updated.project = "sources: new chunk; skills: rust-review".to_string();
        let updated = updated.hashes("model", &tools);
        assert_eq!(baseline.core_hash, updated.core_hash);
        assert_eq!(baseline.tools_hash, updated.tools_hash);
        assert_eq!(baseline.volatile_hash, updated.volatile_hash);
        assert_ne!(baseline.project_hash, updated.project_hash);
        assert_ne!(baseline.prefix_hash, updated.prefix_hash);
    }

    #[test]
    fn tool_changes_invalidate_tools_and_prefix_but_not_sections() {
        let first = sections().hashes("model", &[tool("read_file")]);
        let second = sections().hashes("model", &[tool("read_file"), tool("write_file")]);
        assert_eq!(first.core_hash, second.core_hash);
        assert_eq!(first.project_hash, second.project_hash);
        assert_ne!(first.tools_hash, second.tools_hash);
        assert_ne!(first.prefix_hash, second.prefix_hash);
    }

    #[test]
    fn tool_order_does_not_change_the_tools_hash() {
        let first = sections().hashes("model", &[tool("write_file"), tool("read_file")]);
        let second = sections().hashes("model", &[tool("read_file"), tool("write_file")]);
        assert_eq!(first.tools_hash, second.tools_hash);
        assert_eq!(first.prefix_hash, second.prefix_hash);
    }

    #[test]
    fn model_change_creates_a_new_prefix_generation() {
        let tools = vec![tool("read_file")];
        let first = sections().hashes("model-a", &tools);
        let second = sections().hashes("model-b", &tools);
        assert_ne!(first.prefix_hash, second.prefix_hash);
    }

    #[test]
    fn cache_policy_role_override_is_applied_without_inference() {
        let mut policy = CachePolicy::default();
        policy
            .role_ttl
            .insert("worker".to_string(), "1h".to_string());
        assert_eq!(
            prompt_cache_config(&policy, "leader").ttl,
            CacheTtl::FiveMinutes
        );
        assert_eq!(
            prompt_cache_config(&policy, "worker").ttl,
            CacheTtl::OneHour
        );
        assert!(prompt_cache_config(&policy, "leader").automatic);
    }

    #[test]
    fn request_plan_matches_adapter_breakpoint_limits() {
        let policy = CachePolicy::default();
        let plan = request_cache_plan("anthropic", &sections(), 4, 2, &policy, "leader");
        assert_eq!(plan.breakpoints, 4);
        let openai = request_cache_plan("openai-compatible", &sections(), 4, 2, &policy, "leader");
        assert_eq!(openai.breakpoints, 0);
        assert!(openai.automatic);
    }

    #[test]
    fn first_call_is_attributed_once() {
        let mut tracker = CacheCallTracker::default();
        let current = sections().hashes("model", &[]).fingerprint();
        assert_eq!(
            tracker.observe(&current, 0),
            Some(CacheMissReason::FirstCall)
        );
        // A second call with the same prefix and no reads is a TTL/provider
        // miss, not another first call.
        assert_eq!(
            tracker.observe(&current, 0),
            Some(CacheMissReason::TtlOrProviderMiss)
        );
    }

    #[test]
    fn hits_do_not_produce_a_miss_reason() {
        let mut tracker = CacheCallTracker::default();
        let current = sections().hashes("model", &[]).fingerprint();
        assert_eq!(tracker.observe(&current, 1_024), None);
    }

    #[test]
    fn tools_changed_takes_priority_over_project_changes() {
        let tools = vec![tool("read_file")];
        let first = sections().hashes("model", &tools);
        let mut tracker = CacheCallTracker::new(Some(first.fingerprint()));
        let mut updated = sections();
        updated.project = "new sources".to_string();
        let second = updated.hashes("model", &[tool("read_file"), tool("write_file")]);
        assert_eq!(
            tracker.observe(&second.fingerprint(), 0),
            Some(CacheMissReason::ToolsChanged)
        );
    }

    #[test]
    fn project_change_is_distinguished_from_prefix_change() {
        let first = sections().hashes("model", &[]);
        let mut tracker = CacheCallTracker::new(Some(first.fingerprint()));
        let mut updated = sections();
        updated.project = "new sources".to_string();
        assert_eq!(
            tracker.observe(&updated.hashes("model", &[]).fingerprint(), 0),
            Some(CacheMissReason::ProjectChanged)
        );
    }

    #[test]
    fn core_change_is_a_prefix_change() {
        let first = sections().hashes("model", &[]);
        let mut tracker = CacheCallTracker::new(Some(first.fingerprint()));
        let mut updated = sections();
        updated.core = "new charter".to_string();
        assert_eq!(
            tracker.observe(&updated.hashes("model", &[]).fingerprint(), 0),
            Some(CacheMissReason::PrefixChanged)
        );
    }

    #[test]
    fn compaction_reset_is_attributed_only_when_the_prefix_is_otherwise_stable() {
        let current = sections().hashes("model", &[]).fingerprint();
        let mut tracker = CacheCallTracker::new(Some(current.clone()));
        tracker.mark_compaction();
        assert_eq!(
            tracker.observe(&current, 0),
            Some(CacheMissReason::CompactionReset)
        );
        assert!(!tracker.compaction_pending());
    }

    #[test]
    fn a_hit_clears_the_compaction_marker() {
        let current = sections().hashes("model", &[]).fingerprint();
        let mut tracker = CacheCallTracker::new(Some(current.clone()));
        tracker.mark_compaction();
        assert_eq!(tracker.observe(&current, 512), None);
        assert!(!tracker.compaction_pending());
        assert_eq!(
            tracker.observe(&current, 0),
            Some(CacheMissReason::TtlOrProviderMiss)
        );
    }

    #[test]
    fn fingerprint_reads_legacy_and_new_event_keys() {
        let payload = serde_json::json!({
            "prefix_hash": "prefix",
            "core_hash": "core",
            "tools_hash": "tools",
        });
        let fingerprint = CacheCallFingerprint::from_event_payload(&payload).unwrap();
        assert_eq!(fingerprint.prefix_hash, "prefix");
        assert_eq!(fingerprint.core_hash, "core");
        assert_eq!(fingerprint.tools_hash, "tools");
        assert_eq!(fingerprint.project_hash, "");
        let new_payload = serde_json::json!({
            "prompt_prefix_hash": "new-prefix",
            "project_hash": "project",
        });
        assert_eq!(
            CacheCallFingerprint::from_event_payload(&new_payload)
                .unwrap()
                .prefix_hash,
            "new-prefix"
        );
        assert!(CacheCallFingerprint::from_event_payload(&serde_json::json!({})).is_none());
    }

    #[test]
    fn savings_math_prices_cache_reads_as_billed_not_free() {
        // 1M input at $3, 60% read at $0.3, 10% write at $3.75.
        let savings =
            estimated_cache_savings_usd(1_000_000, 0, 600_000, 100_000, 3.0, 0.0, 0.3, 3.75);
        // No-cache $3.00; billed 0.3M*$3 + 0.6M*$0.3 + 0.1M*$3.75 = $1.455.
        assert!((savings - 1.545).abs() < 1e-9, "{savings}");
    }

    #[test]
    fn savings_can_be_negative_when_cache_writes_cost_more_than_reads_save() {
        let savings = estimated_cache_savings_usd(100_000, 0, 0, 100_000, 3.0, 0.0, 0.3, 6.0);
        assert!(savings < 0.0, "{savings}");
        assert!((savings + 0.3).abs() < 1e-9, "{savings}");
    }

    #[test]
    fn savings_are_zero_when_the_provider_did_not_use_cache() {
        assert_eq!(
            estimated_cache_savings_usd(1_000, 500, 0, 0, 3.0, 15.0, 0.3, 3.75),
            0.0
        );
    }

    #[test]
    fn conversation_generation_changes_after_a_compaction_boundary() {
        let prefix = "stable-prefix";
        let before = vec![
            ChatMessage::user("one"),
            ChatMessage::assistant_tool_calls("", Vec::new()),
            ChatMessage::user("two"),
        ];
        let after = vec![ChatMessage::user("Earlier conversation summary")];
        assert_ne!(
            conversation_generation(prefix, &before),
            conversation_generation(prefix, &after)
        );
        assert_eq!(
            conversation_generation(prefix, &before),
            conversation_generation(prefix, &before)
        );
    }

    #[test]
    fn cache_call_record_tolerates_legacy_payloads() {
        let record = cache_call_record(&serde_json::json!({"input_tokens": 100}), "now");
        assert_eq!(record.input_tokens, 100);
        assert_eq!(record.cache_breakpoints, 0);
        assert!(!record.warm);
        assert_eq!(record.at, "now");
    }

    #[test]
    fn cache_call_record_reads_new_telemetry() {
        let payload = serde_json::json!({
            "agent_name": "Leader",
            "agent_role": "leader",
            "model": "claude",
            "input_tokens": 1000,
            "output_tokens": 50,
            "cache_read_tokens": 600,
            "cache_write_tokens": 100,
            "cache_hit_ratio": 0.6,
            "cache_breakpoints": 4,
            "cache_ttl": "1h",
            "first_token_ms": 321,
            "core_hash": "core",
            "project_hash": "project",
            "tools_hash": "tools",
            "volatile_hash": "volatile",
            "prompt_prefix_hash": "prefix",
            "estimated_savings_usd": 0.25,
            "miss_reason": "ttl_or_provider_miss",
            "warm": false,
        });
        let record = cache_call_record(&payload, "now");
        assert_eq!(record.cache_breakpoints, 4);
        assert_eq!(record.cache_ttl.as_deref(), Some("1h"));
        assert_eq!(record.miss_reason.as_deref(), Some("ttl_or_provider_miss"));
        assert_eq!(record.estimated_savings_usd, 0.25);
        assert_eq!(record.first_token_ms, Some(321));
    }

    fn event(kind: &str, payload: serde_json::Value) -> vp_protocol::types::Event {
        vp_protocol::types::Event {
            session_id: "ses-1".into(),
            seq: 1,
            kind: kind.into(),
            payload,
            created_at: "2026-11-11T00:00:00Z".into(),
        }
    }

    #[test]
    fn aggregate_orders_buckets_and_separates_warm_calls() {
        let events = vec![
            event(
                "context.cache",
                serde_json::json!({
                    "agent_role": "worker",
                    "cache_ttl": "1h",
                    "cache_breakpoints": 4,
                    "miss_reason": "tools_changed",
                    "estimated_savings_usd": 0.5,
                    "first_token_ms": 100,
                    "warm": false
                }),
            ),
            event(
                "context.cache",
                serde_json::json!({
                    "agent_role": "leader",
                    "cache_ttl": "5m",
                    "cache_breakpoints": 4,
                    "miss_reason": "first_call",
                    "estimated_savings_usd": -0.1,
                    "first_token_ms": 300,
                    "warm": false
                }),
            ),
            event(
                "context.cache",
                serde_json::json!({
                    "agent_role": "leader",
                    "cache_ttl": "5m",
                    "cache_breakpoints": 3,
                    "estimated_savings_usd": 0.0,
                    "first_token_ms": 200,
                    "warm": true
                }),
            ),
        ];
        let aggregate = aggregate_cache_events(&events, &[]);
        assert_eq!(aggregate.calls, 2);
        assert_eq!(aggregate.warm_calls, 1);
        assert_eq!(aggregate.first_token_p50_ms, Some(200));
        assert_eq!(aggregate.first_token_p95_ms, Some(300));
        assert_eq!(
            aggregate.miss_reason_counts,
            vec![
                vp_protocol::types::CacheCountBucket {
                    key: "first_call".into(),
                    calls: 1
                },
                vp_protocol::types::CacheCountBucket {
                    key: "tools_changed".into(),
                    calls: 1
                }
            ]
        );
        assert_eq!(
            aggregate.ttl_counts,
            vec![
                vp_protocol::types::CacheCountBucket {
                    key: "1h".into(),
                    calls: 1
                },
                vp_protocol::types::CacheCountBucket {
                    key: "5m".into(),
                    calls: 2
                }
            ]
        );
        assert_eq!(
            aggregate.breakpoint_counts,
            vec![
                vp_protocol::types::CacheCountBucket {
                    key: "3".into(),
                    calls: 1
                },
                vp_protocol::types::CacheCountBucket {
                    key: "4".into(),
                    calls: 2
                }
            ]
        );
        assert_eq!(aggregate.savings_by_role.len(), 2);
        assert_eq!(aggregate.savings_by_role[0].role, "leader");
        assert!((aggregate.savings_by_role[0].estimated_savings_usd + 0.1).abs() < 1e-9);
        assert_eq!(aggregate.savings_by_role[1].role, "worker");
        assert!((aggregate.savings_by_role[1].estimated_savings_usd - 0.5).abs() < 1e-9);
    }

    #[test]
    fn aggregate_ignores_missing_fields_and_counts_compaction_resets() {
        let cache = vec![event("context.cache", serde_json::json!({}))];
        let compactions = vec![
            event(
                "context.compaction",
                serde_json::json!({
                    "cache_reset_reason": "compaction",
                    "cache_invalidation_tokens": 700,
                    "cache_invalidation_estimated_usd": 0.0063
                }),
            ),
            event("context.compaction", serde_json::json!({})),
        ];
        let aggregate = aggregate_cache_events(&cache, &compactions);
        assert_eq!(aggregate.calls, 1);
        assert_eq!(aggregate.warm_calls, 0);
        assert!(aggregate.miss_reason_counts.is_empty());
        assert!(aggregate.ttl_counts.is_empty());
        assert!(aggregate.breakpoint_counts.is_empty());
        assert_eq!(aggregate.savings_by_role.len(), 1);
        assert_eq!(aggregate.savings_by_role[0].role, "(unassigned)");
        assert_eq!(aggregate.first_token_p50_ms, None);
        assert_eq!(aggregate.compaction_resets, 1);
        assert_eq!(aggregate.compaction_invalidation_tokens, 700);
        assert!((aggregate.compaction_invalidation_estimated_usd - 0.0063).abs() < 1e-9);
    }

    #[test]
    fn nearest_rank_is_empty_safe_and_within_bounds() {
        assert_eq!(nearest_rank(&[], 50), None);
        assert_eq!(nearest_rank(&[10], 95), Some(10));
        assert_eq!(nearest_rank(&[10, 20, 30, 40], 50), Some(20));
        assert_eq!(nearest_rank(&[10, 20, 30, 40], 95), Some(40));
        assert_eq!(nearest_rank(&[10, 20, 30, 40], 100), Some(40));
    }

    #[test]
    fn miss_reason_strings_round_trip() {
        for reason in [
            CacheMissReason::FirstCall,
            CacheMissReason::PrefixChanged,
            CacheMissReason::ToolsChanged,
            CacheMissReason::ProjectChanged,
            CacheMissReason::TtlOrProviderMiss,
            CacheMissReason::CompactionReset,
            CacheMissReason::Unknown,
        ] {
            assert_eq!(CacheMissReason::parse(reason.as_str()), Some(reason));
        }
        assert_eq!(CacheMissReason::parse("provider_said_no"), None);
    }
}

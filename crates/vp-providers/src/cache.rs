//! Provider-neutral prompt-cache request model.
//!
//! The runtime owns prompt construction and cache policy; this module only
//! describes what the adapters should express on the wire. It deliberately
//! contains no provider I/O and no automatic warming.

use serde_json::{Value, json};

/// Anthropic-compatible prompt-cache TTLs. The 5-minute pool is the default;
/// the 1-hour pool is an explicit opt-in because its cache writes cost more.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CacheTtl {
    #[default]
    FiveMinutes,
    OneHour,
}

impl CacheTtl {
    pub const FIVE_MINUTES: &'static str = "5m";
    pub const ONE_HOUR: &'static str = "1h";

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FiveMinutes => Self::FIVE_MINUTES,
            Self::OneHour => Self::ONE_HOUR,
        }
    }

    /// Parse the protocol spelling. Unknown values are rejected rather than
    /// silently mapped to a TTL the operator did not choose.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "5m" | "5min" | "5-minute" | "5 minute" => Some(Self::FiveMinutes),
            "1h" | "1hr" | "1-hour" | "1 hour" => Some(Self::OneHour),
            _ => None,
        }
    }

    /// `cache_control` marker for one explicit breakpoint. The default pool is
    /// expressed by omitting `ttl`; the extended pool is always explicit.
    pub fn cache_control(self) -> Value {
        match self {
            Self::FiveMinutes => json!({"type": "ephemeral"}),
            Self::OneHour => json!({"type": "ephemeral", "ttl": "1h"}),
        }
    }
}

/// One deterministic system-prompt section carried to an adapter.
///
/// `kind` is `core`, `project`, or `volatile`. Adapters use the kind only for
/// cache placement and message ordering; they never rewrite the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemSection {
    pub kind: String,
    pub text: String,
}

impl SystemSection {
    pub fn new(kind: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            text: text.into(),
        }
    }

    pub fn has_text(&self) -> bool {
        !self.text.is_empty()
    }
}

/// Cache behavior for one request. This is copied from the project policy by
/// the runtime; adapters never invent or infer it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptCacheConfig {
    pub enabled: bool,
    pub ttl: CacheTtl,
    /// Request the provider's automatic moving breakpoint where supported.
    /// When false, Anthropic-compatible adapters place the moving breakpoint
    /// on the last cacheable message block instead.
    pub automatic: bool,
}

impl Default for PromptCacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            ttl: CacheTtl::FiveMinutes,
            automatic: true,
        }
    }
}

impl PromptCacheConfig {
    pub fn new(enabled: bool, ttl: CacheTtl, automatic: bool) -> Self {
        Self {
            enabled,
            ttl,
            automatic,
        }
    }

    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }
}

/// Provider-specific breakpoint plan. The runtime records this plan before the
/// request so telemetry describes what was actually asked for, not a guess
/// made after the response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheBreakpointPlan {
    pub provider: String,
    pub enabled: bool,
    /// Explicit block-level breakpoints plus a request-level automatic or
    /// tail breakpoint. Always `<= 4`, matching Anthropic's documented limit.
    pub breakpoints: u32,
    pub ttl: CacheTtl,
    /// True when the TTL is sent on the wire. OpenAI-compatible endpoints use
    /// automatic prefix caching and receive no TTL field.
    pub ttl_explicit: bool,
    /// True when the adapter requests a provider-managed moving breakpoint.
    pub automatic: bool,
    /// True when the adapter puts the moving breakpoint on the last message
    /// block because automatic request-level caching is disabled.
    pub tail_message: bool,
    pub tool_breakpoint: bool,
    pub core_breakpoint: bool,
    pub project_breakpoint: bool,
}

impl CacheBreakpointPlan {
    pub fn disabled(provider: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            enabled: false,
            breakpoints: 0,
            ttl: CacheTtl::FiveMinutes,
            ttl_explicit: false,
            automatic: false,
            tail_message: false,
            tool_breakpoint: false,
            core_breakpoint: false,
            project_breakpoint: false,
        }
    }

    pub fn payload(&self) -> Value {
        json!({
            "provider": self.provider,
            "enabled": self.enabled,
            "breakpoints": self.breakpoints,
            "ttl": self.ttl.as_str(),
            "ttl_explicit": self.ttl_explicit,
            "automatic": self.automatic,
            "tail_message": self.tail_message,
            "tool_breakpoint": self.tool_breakpoint,
            "core_breakpoint": self.core_breakpoint,
            "project_breakpoint": self.project_breakpoint,
        })
    }
}

/// Compute the exact cache plan for one request.
///
/// Anthropic accepts at most four cache breakpoints across tools, system
/// blocks, and message blocks. We reserve:
/// - one for the tool bundle (last tool),
/// - one for `core`,
/// - one for `project`,
/// - and one moving slot, either the request-level automatic breakpoint or a
///   breakpoint on the last cacheable message block.
///
/// The `volatile` section, run memory, and objective are intentionally never
/// given an explicit breakpoint; they must stay after the reusable prefix.
/// OpenAI-compatible endpoints use automatic prefix caching and therefore do
/// not emit explicit breakpoints.
pub fn plan_cache_breakpoints(
    provider: &str,
    sections: &[SystemSection],
    tool_count: usize,
    message_count: usize,
    policy: PromptCacheConfig,
) -> CacheBreakpointPlan {
    if !policy.enabled {
        let mut disabled = CacheBreakpointPlan::disabled(provider);
        disabled.ttl = policy.ttl;
        return disabled;
    }
    let anthropic = provider == "anthropic";
    if !anthropic {
        return CacheBreakpointPlan {
            provider: provider.to_string(),
            enabled: true,
            breakpoints: 0,
            ttl: policy.ttl,
            ttl_explicit: false,
            automatic: true,
            tail_message: false,
            tool_breakpoint: false,
            core_breakpoint: false,
            project_breakpoint: false,
        };
    }
    let has = |kind: &str| {
        sections
            .iter()
            .any(|section| section.kind == kind && section.has_text())
    };
    let tool_breakpoint = tool_count > 0;
    let core_breakpoint = has("core");
    let project_breakpoint = has("project");
    let explicit = u32::from(tool_breakpoint)
        .saturating_add(u32::from(core_breakpoint))
        .saturating_add(u32::from(project_breakpoint));
    // The four-breakpoint limit is a hard provider constraint. Our three
    // explicit sections plus one moving slot can never exceed it, but keep the
    // clamp visible so future section additions cannot silently break it.
    let moving = if policy.automatic || message_count > 0 {
        1
    } else {
        0
    };
    let breakpoints = explicit.saturating_add(moving).min(4);
    CacheBreakpointPlan {
        provider: provider.to_string(),
        enabled: true,
        breakpoints,
        ttl: policy.ttl,
        ttl_explicit: true,
        automatic: policy.automatic,
        // A tail breakpoint is only used when automatic caching is off. When
        // automatic is on, the provider owns the moving slot.
        tail_message: !policy.automatic && message_count > 0,
        tool_breakpoint,
        core_breakpoint,
        project_breakpoint,
    }
}

/// True when a provider rejection is plausibly about prompt-cache TTL or cache
/// control rather than an unrelated request error. Used only to make an
/// explicit 1h opt-in fail with an actionable message; it never triggers a
/// silent downgrade.
pub fn looks_like_cache_ttl_rejection(status: u16, body: &str) -> bool {
    if !matches!(status, 400 | 404 | 422) {
        return false;
    }
    let lower = body.to_ascii_lowercase();
    [
        "ttl",
        "cache_control",
        "cache control",
        "extended cache",
        "prompt caching",
        "cache ttl",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sections() -> Vec<SystemSection> {
        vec![
            SystemSection::new("core", "role charter"),
            SystemSection::new("project", "sources and skills"),
            SystemSection::new("volatile", "run context"),
        ]
    }

    #[test]
    fn ttl_spellings_parse_without_inference() {
        assert_eq!(CacheTtl::parse("5m"), Some(CacheTtl::FiveMinutes));
        assert_eq!(CacheTtl::parse("1h"), Some(CacheTtl::OneHour));
        assert_eq!(CacheTtl::parse(" 1H "), Some(CacheTtl::OneHour));
        assert_eq!(CacheTtl::parse("30m"), None);
        assert_eq!(CacheTtl::parse(""), None);
    }

    #[test]
    fn ttl_markers_omit_the_default_and_name_the_extended_pool() {
        assert_eq!(
            CacheTtl::FiveMinutes.cache_control(),
            json!({"type": "ephemeral"})
        );
        assert_eq!(
            CacheTtl::OneHour.cache_control(),
            json!({"type": "ephemeral", "ttl": "1h"})
        );
    }

    #[test]
    fn anthropic_plan_uses_all_four_slots() {
        let policy = PromptCacheConfig::new(true, CacheTtl::FiveMinutes, true);
        let plan = plan_cache_breakpoints("anthropic", &sections(), 12, 6, policy);
        assert!(plan.enabled);
        assert_eq!(plan.breakpoints, 4);
        assert!(plan.automatic);
        assert!(!plan.tail_message);
        assert!(plan.tool_breakpoint);
        assert!(plan.core_breakpoint);
        assert!(plan.project_breakpoint);
        assert!(plan.ttl_explicit);
    }

    #[test]
    fn anthropic_plan_uses_a_tail_breakpoint_when_automatic_is_off() {
        let policy = PromptCacheConfig::new(true, CacheTtl::OneHour, false);
        let plan = plan_cache_breakpoints("anthropic", &sections(), 3, 9, policy);
        assert_eq!(plan.breakpoints, 4);
        assert!(!plan.automatic);
        assert!(plan.tail_message);
        assert_eq!(plan.ttl.as_str(), "1h");
    }

    #[test]
    fn anthropic_plan_never_exceeds_four_when_optional_sections_are_absent() {
        let core_only = vec![SystemSection::new("core", "charter")];
        let policy = PromptCacheConfig::new(true, CacheTtl::FiveMinutes, true);
        let plan = plan_cache_breakpoints("anthropic", &core_only, 0, 2, policy);
        assert_eq!(plan.breakpoints, 2); // core + automatic
        assert!(!plan.project_breakpoint);
        assert!(!plan.tool_breakpoint);
    }

    #[test]
    fn disabled_policy_emits_no_breakpoints() {
        let plan = plan_cache_breakpoints(
            "anthropic",
            &sections(),
            8,
            4,
            PromptCacheConfig::disabled(),
        );
        assert!(!plan.enabled);
        assert_eq!(plan.breakpoints, 0);
        assert!(!plan.automatic);
        assert!(!plan.core_breakpoint);
    }

    #[test]
    fn openai_plan_reports_automatic_prefix_caching_without_explicit_slots() {
        let plan = plan_cache_breakpoints(
            "openai-compatible",
            &sections(),
            8,
            4,
            PromptCacheConfig::new(true, CacheTtl::FiveMinutes, true),
        );
        assert!(plan.enabled);
        assert_eq!(plan.breakpoints, 0);
        assert!(plan.automatic);
        assert!(!plan.ttl_explicit);
        assert!(!plan.tail_message);
    }

    #[test]
    fn ttl_rejection_detection_is_narrow() {
        assert!(looks_like_cache_ttl_rejection(
            400,
            "cache_control.ttl: unsupported value 1h"
        ));
        assert!(looks_like_cache_ttl_rejection(
            422,
            "extended cache ttl is not enabled for this model"
        ));
        assert!(!looks_like_cache_ttl_rejection(
            400,
            "max_tokens is required"
        ));
        assert!(!looks_like_cache_ttl_rejection(
            500,
            "cache ttl backend down"
        ));
        assert!(!looks_like_cache_ttl_rejection(429, "ttl"));
    }
}

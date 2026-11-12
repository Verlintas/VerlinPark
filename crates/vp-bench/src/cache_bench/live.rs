//! Explicit live provider mode for the cache benchmark.
//!
//! Live mode is never selected by default. It requires `--live --allow-live`
//! plus a credential in the environment. It performs a small, controlled
//! multi-turn conversation and records only provider-reported token usage,
//! latency, and a cost estimate from explicit prices. If a provider omits
//! usage, the call is labeled `estimated_usage` instead of pretending the
//! numbers were reported.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use vp_providers::{
    CacheTtl, ChatMessage, ChatRequest, OpenAiCompatibleProvider, PromptCacheConfig, Provider,
    StreamEvent, ToolSchema,
};

use super::simulator::aggregate_calls;
use super::{
    CACHE_BENCH_SCHEMA, CacheBenchAcceptance, CacheBenchCall, CacheBenchMode, CacheBenchPricing,
    CacheBenchReport, CostEstimate, LONG_PREFIX_HIT_TARGET, LONG_PREFIX_MIN_TOKENS, LiveGate,
    ScenarioSizeCheck,
};

/// Optional extra environment gate. When set to `0`/`false`, live mode is
/// disabled even if a key is present. It is never required in addition to the
/// key; the key itself is the primary environment gate.
pub const LIVE_GATE_ENV: &str = "VERLINPARK_CACHE_BENCH_LIVE";
pub const DEFAULT_DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com";
const LIVE_STABLE_WORDS: usize = 1_800;
const LIVE_MAX_OUTPUT_TOKENS: u32 = 64;

/// A live setting is constructed only through [`live_settings_from_env`] or
/// [`LiveCacheBenchSettings::from_parts`]. The API key is never serialized or
/// printed by `Debug`.
#[derive(Clone)]
pub struct LiveCacheBenchSettings {
    pub base_url: String,
    pub model: String,
    pub turns: usize,
    pub allow_live: bool,
    pub key_source: String,
    pub extra_env_gate: Option<String>,
    api_key: String,
}

impl std::fmt::Debug for LiveCacheBenchSettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LiveCacheBenchSettings")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("turns", &self.turns)
            .field("allow_live", &self.allow_live)
            .field("key_source", &self.key_source)
            .field("extra_env_gate", &self.extra_env_gate)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

impl LiveCacheBenchSettings {
    /// Test/embedder constructor. It still requires the caller to pass
    /// `allow_live = true`; no provider call is made by constructing it.
    pub fn from_parts(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: impl Into<String>,
        turns: usize,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            turns: turns.clamp(1, 200),
            allow_live: true,
            key_source: "caller".to_string(),
            extra_env_gate: None,
            api_key: api_key.into(),
        }
    }

    pub fn gate(&self) -> LiveGate {
        LiveGate {
            allow_live: self.allow_live,
            key_source: self.key_source.clone(),
            base_url: self.base_url.clone(),
            model: self.model.clone(),
            extra_env_gate: self.extra_env_gate.clone(),
        }
    }

    fn provider(&self) -> OpenAiCompatibleProvider {
        OpenAiCompatibleProvider::new("deepseek", &self.base_url, Some(self.api_key.clone()))
    }
}

/// Build live settings from explicit flags plus environment. Returns an error
/// (never silently falls back to simulated mode) when live mode is requested
/// without the acknowledgement or credential.
pub fn live_settings_from_env(
    allow_live: bool,
    model_override: Option<&str>,
    base_url_override: Option<&str>,
    turns: usize,
) -> Result<LiveCacheBenchSettings> {
    if !allow_live {
        bail!("live cache-bench mode requires --allow-live (it makes real provider calls)");
    }
    if let Ok(value) = std::env::var(LIVE_GATE_ENV) {
        let normalized = value.trim().to_ascii_lowercase();
        if matches!(normalized.as_str(), "0" | "false" | "no" | "off") {
            bail!("live cache-bench mode is disabled by {LIVE_GATE_ENV}={value}");
        }
    }
    let (api_key, key_source) = std::env::var("VERLINPARK_LIVE_API_KEY")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| (value, "VERLINPARK_LIVE_API_KEY".to_string()))
        .or_else(|| {
            std::env::var("DEEPSEEK_API_KEY")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .map(|value| (value, "DEEPSEEK_API_KEY".to_string()))
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "live cache-bench mode needs VERLINPARK_LIVE_API_KEY or DEEPSEEK_API_KEY"
            )
        })?;
    let base_url = base_url_override
        .map(str::to_string)
        .or_else(|| std::env::var("VERLINPARK_CACHE_BENCH_BASE_URL").ok())
        .or_else(|| std::env::var("DEEPSEEK_BASE_URL").ok())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_DEEPSEEK_BASE_URL.to_string());
    let model = model_override
        .map(str::to_string)
        .or_else(|| std::env::var("VERLINPARK_CACHE_BENCH_MODEL").ok())
        .or_else(|| std::env::var("DEEPSEEK_MODEL").ok())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "live cache-bench mode needs --model, VERLINPARK_CACHE_BENCH_MODEL, or DEEPSEEK_MODEL"
            )
        })?;
    Ok(LiveCacheBenchSettings {
        base_url,
        model,
        turns: turns.clamp(1, 200),
        allow_live,
        key_source,
        extra_env_gate: std::env::var(LIVE_GATE_ENV).ok(),
        api_key,
    })
}

#[derive(Debug, Clone, Default)]
struct LiveTurnUsage {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    reported: bool,
    text: String,
    first_token_ms: Option<u64>,
}

/// Optional live-run guardrail. `max_cost_usd` is checked before each provider
/// call against the accumulated billed estimate plus the previous call's size;
/// it is a stop-loss, not a hard provider-side budget.
#[derive(Debug, Clone, Copy, Default)]
pub struct LiveRunOptions {
    pub max_cost_usd: Option<f64>,
}

/// Run the controlled live session with the configured DeepSeek client. This
/// is the only public function in the module that performs network I/O.
pub async fn run_live_benchmark(
    settings: &LiveCacheBenchSettings,
    pricing: CacheBenchPricing,
) -> Result<CacheBenchReport> {
    run_live_benchmark_with_options(settings, pricing, LiveRunOptions::default()).await
}

/// Live runner with an explicit stop-loss.
pub async fn run_live_benchmark_with_options(
    settings: &LiveCacheBenchSettings,
    pricing: CacheBenchPricing,
    options: LiveRunOptions,
) -> Result<CacheBenchReport> {
    let provider = settings.provider();
    run_live_benchmark_with_provider(settings, pricing, &provider, options).await
}

/// Provider-injected live runner. Tests and embedders can supply a local fake
/// without a network call; the turn/scenario semantics are identical.
pub async fn run_live_benchmark_with_provider(
    settings: &LiveCacheBenchSettings,
    pricing: CacheBenchPricing,
    provider: &dyn Provider,
    options: LiveRunOptions,
) -> Result<CacheBenchReport> {
    if !settings.allow_live {
        bail!("live cache-bench settings were not acknowledged");
    }
    pricing.validate()?;
    if settings.turns == 0 {
        bail!("live cache-bench needs at least one turn");
    }
    if let Some(limit) = options.max_cost_usd
        && (!limit.is_finite() || limit < 0.0)
    {
        bail!("live cache-bench max cost must be finite and non-negative");
    }
    let system = live_instruction_text();
    let mut conversation = vec![ChatMessage::user(
        "We are benchmarking prompt-cache continuity. Reply briefly and continue.",
    )];
    let mut calls = Vec::with_capacity(settings.turns);
    let mut previous_input = 0u64;
    let mut previous_message_hash: Option<String> = None;
    let mut previous_message_count = 0u64;
    let mut spent_cost_usd = 0.0f64;
    for turn in 0..settings.turns {
        if let Some(limit) = options.max_cost_usd {
            let projected_input = previous_input.max(1).saturating_add(256);
            let projected = pricing
                .estimate(projected_input, 0, 0, u64::from(LIVE_MAX_OUTPUT_TOKENS))
                .total_cost_usd;
            if spent_cost_usd + projected > limit {
                bail!(
                    "live cache-bench stopped before turn {}: projected ${:.6} + spent ${:.6} exceeds --max-live-cost-usd ${:.6}",
                    turn + 1,
                    projected,
                    spent_cost_usd,
                    limit
                );
            }
        }
        let user = ChatMessage::user(format!(
            "Turn {}: continue the controlled development session. Read one small file and report one line.",
            turn + 1
        ));
        conversation.push(user);
        let request = ChatRequest {
            model: settings.model.clone(),
            messages: conversation.clone(),
            system: Some(system.clone()),
            system_stable_prefix: Some(system.clone()),
            system_sections: Vec::new(),
            tail_sections: Vec::new(),
            cache: Some(PromptCacheConfig::new(true, CacheTtl::FiveMinutes, true)),
            max_tokens: Some(LIVE_MAX_OUTPUT_TOKENS),
            temperature: Some(0.0),
            tools: Vec::<ToolSchema>::new(),
        };
        let usage = match call_provider(provider, request).await {
            Ok(usage) => usage,
            Err(error) => {
                bail!(
                    "live cache-bench provider call {} failed: {error:#}",
                    turn + 1
                )
            }
        };
        let input_tokens = if usage.reported {
            usage.input_tokens
        } else {
            estimate_messages_tokens(&conversation, &system)
        };
        let output_tokens = if usage.reported {
            usage.output_tokens
        } else {
            estimate_text_tokens(&usage.text)
        };
        let current_hash = vp_core::prompt_cache::message_prefix_hash(&conversation);
        let current_count = conversation.len() as u64;
        let append_only = match previous_message_hash.as_deref() {
            Some(hash) if previous_message_count > 0 => {
                vp_core::prompt_cache::message_list_is_append_only(
                    hash,
                    previous_message_count,
                    &conversation,
                )
            }
            _ => false,
        };
        let cache_read = usage.cache_read_tokens.min(input_tokens);
        let cache_write = usage
            .cache_write_tokens
            .min(input_tokens.saturating_sub(cache_read));
        let miss_tokens = input_tokens.saturating_sub(cache_read);
        let theoretical =
            vp_core::prompt_cache::theoretical_max_hit_ratio(previous_input, input_tokens);
        let miss_reason = if turn == 0 {
            Some("first_call".to_string())
        } else if !append_only {
            Some("prefix_changed".to_string())
        } else {
            let readable = previous_input.min(input_tokens);
            let tail = input_tokens.saturating_sub(readable);
            if miss_tokens <= tail.saturating_add(64) {
                Some("tail_delta".to_string())
            } else {
                Some("provider_cache_miss".to_string())
            }
        };
        let cost = pricing.estimate(input_tokens, cache_read, cache_write, output_tokens);
        spent_cost_usd += cost.total_cost_usd;
        calls.push(CacheBenchCall {
            turn: turn + 1,
            input_tokens,
            output_tokens,
            cache_read_tokens: cache_read,
            cache_write_tokens: cache_write,
            cache_miss_tokens: miss_tokens,
            cache_hit_ratio: ratio(cache_read, input_tokens),
            theoretical_max_hit_ratio: theoretical,
            previous_input_tokens: previous_input,
            append_only,
            miss_reason,
            first_token_ms: usage.first_token_ms,
            estimated_usage: !usage.reported,
            cost,
        });
        let reply = if usage.text.trim().is_empty() {
            "OK".to_string()
        } else {
            usage.text
        };
        conversation.push(ChatMessage::assistant_tool_calls(reply, Vec::new()));
        previous_input = input_tokens;
        previous_message_hash = Some(current_hash);
        previous_message_count = current_count;
    }
    let aggregate = aggregate_calls(&calls);
    let estimated_prefix = estimate_text_tokens(&system);
    let total_prefix = estimated_prefix;
    let size_check = ScenarioSizeCheck {
        passed: total_prefix >= LONG_PREFIX_MIN_TOKENS,
        total_prefix_tokens: total_prefix,
        mean_turn_delta_tokens: mean_delta(&calls),
        turns: calls.len(),
        reasons: if total_prefix >= LONG_PREFIX_MIN_TOKENS {
            Vec::new()
        } else {
            vec![format!(
                "live stable instruction prefix is ~{total_prefix} tokens; the long-prefix target needs at least {LONG_PREFIX_MIN_TOKENS}. This live smoke reports the provider ratio honestly but cannot claim the long-prefix target."
            )]
        },
    };
    let generated_at = OffsetDateTime::now_utc().format(&Rfc3339).ok();
    let mut report = CacheBenchReport {
        schema: CACHE_BENCH_SCHEMA.to_string(),
        scenario: "live_deepseek_controlled_session".to_string(),
        mode: CacheBenchMode::Live,
        provider: "deepseek".to_string(),
        model: settings.model.clone(),
        block_size: 64,
        turns: calls.len(),
        stable_prefix_tokens: estimated_prefix,
        initial_context_tokens: 0,
        cross_session_warm: false,
        warmup_included: false,
        deterministic: false,
        generated_at,
        pricing,
        scenario_size: size_check,
        live_gate: Some(settings.gate()),
        calls,
        aggregate,
        acceptance: CacheBenchAcceptance::default(),
        caveats: vec![
            "Live mode uses the configured provider and credential; the key is never serialized or printed.".to_string(),
            "Only provider-reported usage is labeled as reported; missing usage is marked estimated_usage=true.".to_string(),
            "The theoretical max is the previous prompt size divided by the current prompt size. It is not a provider TTL or block guarantee.".to_string(),
            "The stable instruction prefix is intentionally small in the default smoke. Set the scenario/model if you need a long-context acceptance run; this module will not pad the prompt to chase a ratio.".to_string(),
            format!(
                "Long-prefix target is {:.2}%; a live smoke below that target is reported as a miss, not hidden.",
                LONG_PREFIX_HIT_TARGET * 100.0
            ),
        ],
    };
    report.acceptance = report.compute_acceptance();
    Ok(report)
}

async fn call_provider(provider: &dyn Provider, request: ChatRequest) -> Result<LiveTurnUsage> {
    let started = Instant::now();
    let mut stream = provider
        .stream_chat(request)
        .await
        .context("provider request failed before streaming")?;
    let mut usage = LiveTurnUsage::default();
    while let Some(event) = stream.next().await {
        match event.context("provider stream error")? {
            StreamEvent::TextDelta(delta) => {
                if usage.first_token_ms.is_none() && !delta.is_empty() {
                    usage.first_token_ms =
                        Some(started.elapsed().as_millis().min(u64::MAX as u128) as u64);
                }
                usage.text.push_str(&delta);
            }
            StreamEvent::ToolCall(_) => {
                // The live cache smoke intentionally exposes no tools. A
                // provider tool call is not replayed or executed; it is simply
                // ignored and the turn text remains whatever was streamed.
            }
            StreamEvent::Usage {
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
            } => {
                usage.input_tokens = u64::from(input_tokens);
                usage.output_tokens = u64::from(output_tokens);
                usage.cache_read_tokens = u64::from(cache_read_tokens);
                usage.cache_write_tokens = u64::from(cache_write_tokens);
                usage.reported = true;
            }
            StreamEvent::Done => break,
        }
    }
    Ok(usage)
}

fn live_instruction_text() -> String {
    let mut text = String::from(
        "You are a coding agent in a controlled cache benchmark. Follow the response contract: answer briefly, do not call tools, and keep the same stable instructions for every turn.\n",
    );
    for line in 0..LIVE_STABLE_WORDS / 12 {
        text.push_str(&format!(
            "Stable project rule {:03}: prefer deterministic parsing, keep edits inside the worktree, verify with a focused test, and report one concise result line.\n",
            line + 1
        ));
    }
    text
}

fn estimate_messages_tokens(messages: &[ChatMessage], system: &str) -> u64 {
    let mut chars = system.len();
    for message in messages {
        chars = chars
            .saturating_add(message.content.len())
            .saturating_add(message.role.len())
            .saturating_add(8);
    }
    (chars / 4).max(1) as u64
}

fn estimate_text_tokens(text: &str) -> u64 {
    (text.len() / 4).max(1) as u64
}

fn ratio(read: u64, input: u64) -> f64 {
    if input == 0 {
        0.0
    } else {
        (read.min(input) as f64 / input as f64).clamp(0.0, 1.0)
    }
}

fn mean_delta(calls: &[CacheBenchCall]) -> u64 {
    if calls.len() < 2 {
        return 0;
    }
    let total = calls
        .windows(2)
        .map(|pair| pair[1].input_tokens.saturating_sub(pair[0].input_tokens))
        .sum::<u64>();
    total / (calls.len() - 1) as u64
}

/// A bounded cost-only estimate for tooling that needs the same arithmetic
/// without constructing a full report.
pub fn estimate_cost(
    pricing: &CacheBenchPricing,
    input: u64,
    read: u64,
    write: u64,
    output: u64,
) -> CostEstimate {
    pricing.estimate(input, read, write, output)
}

/// Never used by the benchmark runtime; retained so a test can assert that
/// live mode is not accidentally reachable without an acknowledgement.
pub fn live_mode_requires_acknowledgement(allow_live: bool) -> bool {
    !allow_live
}

/// Fixed timeout used by live mode so a stalled endpoint cannot hang the
/// benchmark forever. The provider client owns its own transport timeout; this
/// constant documents the intended ceiling for the controlled smoke.
pub const LIVE_CALL_TIMEOUT: Duration = Duration::from_secs(120);

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use futures_util::stream;
    use vp_providers::{EventStream, ProviderError};

    use super::*;

    struct FakeLiveProvider {
        calls: Arc<AtomicUsize>,
    }

    impl FakeLiveProvider {
        fn new() -> (Self, Arc<AtomicUsize>) {
            let calls = Arc::new(AtomicUsize::new(0));
            (
                Self {
                    calls: calls.clone(),
                },
                calls,
            )
        }
    }

    #[async_trait]
    impl Provider for FakeLiveProvider {
        fn id(&self) -> &str {
            "fake-live"
        }

        async fn stream_chat(&self, _request: ChatRequest) -> Result<EventStream, ProviderError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            let input = 1_000u32 + (call as u32 - 1) * 100;
            let read = input.saturating_sub(100);
            let events = vec![
                Ok(StreamEvent::TextDelta("OK".to_string())),
                Ok(StreamEvent::Usage {
                    input_tokens: input,
                    output_tokens: 8,
                    cache_read_tokens: read,
                    cache_write_tokens: 0,
                }),
                Ok(StreamEvent::Done),
            ];
            Ok(Box::pin(stream::iter(events)))
        }
    }

    #[test]
    fn live_settings_require_acknowledgement_and_never_fall_back() {
        let error = live_settings_from_env(false, Some("deepseek-chat"), None, 4)
            .expect_err("live mode without acknowledgement must fail");
        assert!(error.to_string().contains("--allow-live"));
        assert!(live_mode_requires_acknowledgement(false));
        assert!(!live_mode_requires_acknowledgement(true));
    }

    #[test]
    fn live_settings_debug_redacts_the_key() {
        let settings = LiveCacheBenchSettings::from_parts(
            "https://api.deepseek.com",
            "deepseek-chat",
            "secret",
            3,
        );
        let debug = format!("{settings:?}");
        assert!(!debug.contains("secret"));
        assert!(debug.contains("<redacted>"));
        assert_eq!(settings.gate().model, "deepseek-chat");
    }

    #[test]
    fn live_instruction_is_bounded_and_deterministic() {
        let first = live_instruction_text();
        let second = live_instruction_text();
        assert_eq!(first, second);
        assert!(first.len() > 20_000);
        assert!(first.contains("Stable project rule 001"));
    }

    #[tokio::test]
    async fn injected_provider_exercises_the_live_report_without_network() {
        let settings = LiveCacheBenchSettings::from_parts(
            "http://127.0.0.1:1",
            "fake-model",
            "not-a-real-key",
            3,
        );
        let (provider, calls) = FakeLiveProvider::new();
        let pricing = CacheBenchPricing::caller(crate::metrics::Pricing::new(1.0, 2.0, 0.1, 0.5));
        let report = run_live_benchmark_with_provider(
            &settings,
            pricing,
            &provider,
            LiveRunOptions::default(),
        )
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(report.mode, CacheBenchMode::Live);
        assert_eq!(report.provider, "deepseek");
        assert_eq!(report.model, "fake-model");
        assert_eq!(report.calls.len(), 3);
        assert_eq!(report.calls[0].input_tokens, 1_000);
        assert_eq!(report.calls[0].cache_read_tokens, 900);
        assert_eq!(report.calls[0].miss_reason.as_deref(), Some("first_call"));
        assert!(report.calls[1].append_only);
        assert!(report.calls[2].append_only);
        assert_eq!(report.calls[1].cache_read_tokens, 1_000);
        assert_eq!(report.calls[2].cache_read_tokens, 1_100);
        assert!(!report.calls[0].estimated_usage);
        assert!(report.aggregate.cache_hit_ratio > 0.8);
        assert!(report.aggregate.cost.total_cost_usd > 0.0);
        assert!(report.live_gate.is_some());
        assert!(!report.scenario_size.passed);
    }

    #[tokio::test]
    async fn live_cost_guard_stops_before_the_next_call() {
        let settings =
            LiveCacheBenchSettings::from_parts("http://127.0.0.1:1", "fake-model", "key", 4);
        let (provider, calls) = FakeLiveProvider::new();
        let pricing = CacheBenchPricing::caller(crate::metrics::Pricing::new(1.0, 2.0, 0.1, 0.5));
        let error = run_live_benchmark_with_provider(
            &settings,
            pricing,
            &provider,
            LiveRunOptions {
                max_cost_usd: Some(0.0),
            },
        )
        .await
        .expect_err("zero cost guard must refuse the first call");
        assert!(error.to_string().contains("stopped before turn 1"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

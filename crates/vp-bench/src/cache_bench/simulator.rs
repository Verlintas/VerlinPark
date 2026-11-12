//! Deterministic 64-token block prefix-cache simulator.
//!
//! The simulator is intentionally simple and inspectable:
//!
//! * a prompt is a vector of synthetic token ids;
//! * the cache stores a chain hash for every complete 64-token block it has
//!   seen for an epoch;
//! * a call reads the longest block-aligned prefix whose chain hash is present;
//! * every complete block in the request is then cached, matching prefix-cache
//!   semantics (a later different prefix can reuse blocks only while all prior
//!   blocks match).
//!
//! No network, provider, credential, clock, or random source is used. Two runs
//! with the same configuration produce identical metrics.

use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use super::{
    CACHE_BENCH_SCHEMA, CacheBenchAcceptance, CacheBenchAggregate, CacheBenchCall, CacheBenchMode,
    CacheBenchPricing, CacheBenchReport, CostEstimate, LONG_PREFIX_MIN_TOKENS, MAX_MEAN_TURN_DELTA,
    MIN_LONG_PREFIX_TURNS, PricingSource, ScenarioSizeCheck,
};

/// Version of the simulator contract. Bump when the tokenizer or block model
/// changes so comparisons refuse silently different semantics.
pub const SIMULATOR_VERSION: &str = "prefix-cache-sim/v1";
/// DeepSeek-style prefix cache block size used by the simulator.
pub const SIMULATOR_BLOCK_SIZE: u64 = 64;
/// Synthetic output tokens per measured turn. Output does not affect cache
/// reads; it is included so the cost estimate is not input-only.
pub const OUTPUT_TOKENS_PER_TURN: u64 = 48;

/// Positive long-prefix scenario plus two negative controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CacheBenchScenarioKind {
    /// Previous session primed the stable prefix; measured turns are append-only.
    #[default]
    WarmLongPrefix,
    /// No cross-session prime. The first call pays the whole stable prefix.
    ColdLongPrefix,
    /// Warm prefix, then one earlier message token is changed mid-session. The
    /// simulator must report a prefix break and lose the cache tail.
    PrefixBreakControl,
}

impl CacheBenchScenarioKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WarmLongPrefix => "warm_long_prefix",
            Self::ColdLongPrefix => "cold_long_prefix",
            Self::PrefixBreakControl => "prefix_break_control",
        }
    }

    pub fn primes_cross_session(self) -> bool {
        !matches!(self, Self::ColdLongPrefix)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheBenchScenarioConfig {
    pub turns: usize,
    pub stable_prefix_tokens: usize,
    pub initial_context_tokens: usize,
    pub allow_unrealistic: bool,
    pub kind: CacheBenchScenarioKind,
}

impl Default for CacheBenchScenarioConfig {
    fn default() -> Self {
        Self {
            turns: 50,
            stable_prefix_tokens: 180_000,
            initial_context_tokens: 20_000,
            allow_unrealistic: false,
            kind: CacheBenchScenarioKind::WarmLongPrefix,
        }
    }
}

/// Token-id scenario. Only counts and hashes appear in reports; token ids never
/// leave the simulator.
#[derive(Debug, Clone)]
pub struct CacheBenchScenario {
    pub name: &'static str,
    pub stable_tokens: Vec<u64>,
    pub initial_tokens: Vec<u64>,
    pub turn_tokens: Vec<Vec<u64>>,
    pub epoch: String,
    pub size_check: ScenarioSizeCheck,
    pub kind: CacheBenchScenarioKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimulatedUsage {
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_miss_tokens: u64,
    pub hit_blocks: u64,
    pub total_blocks: u64,
}

/// Block-prefix cache simulator keyed by epoch. The epoch is the stable
/// prompt-prefix identity; a new epoch cannot read another epoch's blocks even
/// if the token ids happen to match.
#[derive(Debug, Clone, Default)]
pub struct PrefixCacheSimulator {
    block_size: u64,
    cached: HashMap<String, HashSet<String>>,
}

impl PrefixCacheSimulator {
    pub fn new() -> Self {
        Self {
            block_size: SIMULATOR_BLOCK_SIZE,
            cached: HashMap::new(),
        }
    }

    pub fn block_size(&self) -> u64 {
        self.block_size
    }

    /// Cache every complete block of `tokens` for `epoch` without reporting a
    /// call. Used to model a previous session's warm stable prefix.
    pub fn prime(&mut self, epoch: &str, tokens: &[u64]) {
        let chains = block_chains(tokens, self.block_size);
        let entry = self.cached.entry(epoch.to_string()).or_default();
        for chain in chains {
            entry.insert(chain);
        }
    }

    /// Serve one request and update the cache. `cache_read_tokens` is always a
    /// multiple of the block size and never exceeds the input size.
    pub fn complete(&mut self, epoch: &str, tokens: &[u64]) -> SimulatedUsage {
        let chains = block_chains(tokens, self.block_size);
        let entry = self.cached.entry(epoch.to_string()).or_default();
        let mut hit_blocks = 0u64;
        for chain in &chains {
            if entry.contains(chain) {
                hit_blocks = hit_blocks.saturating_add(1);
            } else {
                break;
            }
        }
        for chain in &chains {
            entry.insert(chain.clone());
        }
        let input_tokens = tokens.len() as u64;
        let cache_read_tokens = hit_blocks.saturating_mul(self.block_size).min(input_tokens);
        SimulatedUsage {
            input_tokens,
            cache_read_tokens,
            cache_miss_tokens: input_tokens.saturating_sub(cache_read_tokens),
            hit_blocks,
            total_blocks: chains.len() as u64,
        }
    }

    pub fn forget_epoch(&mut self, epoch: &str) {
        self.cached.remove(epoch);
    }
}

/// Chain hashes for complete blocks. A partial final block is deliberately not
/// cached: the simulator reports only whole-block reads, matching the
/// conservative interpretation of prefix caching.
fn block_chains(tokens: &[u64], block_size: u64) -> Vec<String> {
    let block_size = block_size.max(1) as usize;
    let mut chains = Vec::with_capacity(tokens.len() / block_size);
    let mut parent = String::from("root");
    for block in tokens.chunks_exact(block_size) {
        let mut hasher = BlockHasher::new();
        hasher.update(parent.as_bytes());
        hasher.update(b"\x1f");
        for token in block {
            hasher.update(&token.to_le_bytes());
        }
        parent = hasher.finish_hex();
        chains.push(parent.clone());
    }
    chains
}

/// Small SHA-256 wrapper kept local so the simulator does not expose a
/// crypto dependency to callers.
struct BlockHasher {
    hasher: sha2::Sha256,
}

impl BlockHasher {
    fn new() -> Self {
        use sha2::Digest;
        Self {
            hasher: sha2::Sha256::new(),
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        use sha2::Digest;
        self.hasher.update(bytes);
    }

    fn finish_hex(self) -> String {
        use sha2::Digest;
        format!("{:x}", self.hasher.finalize())
    }
}

/// Build the canonical long-prefix development scenario.
///
/// The stable prefix represents a role charter, tool schemas, project source
/// brief, skill catalog, and repository map. The initial context represents
/// file reads and command outputs before the measured turns. Each measured turn
/// appends a small user message, a short assistant response, and a tool result.
/// The shape is synthetic but deterministic; it is not a claim about a vendor
/// tokenizer.
pub fn long_prefix_scenario(config: CacheBenchScenarioConfig) -> Result<CacheBenchScenario> {
    let stable_tokens = synthetic_tokens(config.stable_prefix_tokens, 0xA11CE);
    let initial_tokens = synthetic_tokens(config.initial_context_tokens, 0xB0B);
    let mut turn_tokens = Vec::with_capacity(config.turns);
    for turn in 0..config.turns {
        let seed = 0xC0DE_u64.wrapping_add((turn as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        // User question + assistant plan + tool result. The total is bounded by
        // MAX_MEAN_TURN_DELTA; the exact split is deterministic.
        let user = synthetic_tokens(12, seed);
        let assistant = synthetic_tokens(36, seed ^ 0x1111);
        let tool = synthetic_tokens(120, seed ^ 0x2222);
        let mut turn = user;
        turn.extend(assistant);
        turn.extend(tool);
        turn_tokens.push(turn);
    }
    let total_prefix = (stable_tokens.len() + initial_tokens.len()) as u64;
    let delta_total = turn_tokens.iter().map(Vec::len).sum::<usize>() as u64;
    let mean_turn_delta = if config.turns == 0 {
        0
    } else {
        delta_total / config.turns as u64
    };
    let mut reasons = Vec::new();
    if total_prefix < LONG_PREFIX_MIN_TOKENS {
        reasons.push(format!(
            "total prefix is {total_prefix} tokens; the long-prefix scenario needs at least {LONG_PREFIX_MIN_TOKENS}"
        ));
    }
    if config.turns < MIN_LONG_PREFIX_TURNS {
        reasons.push(format!(
            "only {} measured turns; the long-prefix scenario needs at least {MIN_LONG_PREFIX_TURNS}",
            config.turns
        ));
    }
    if mean_turn_delta > MAX_MEAN_TURN_DELTA {
        reasons.push(format!(
            "mean turn delta is {mean_turn_delta} tokens; it must stay at or below {MAX_MEAN_TURN_DELTA} so the ratio is not driven by a huge moving tail"
        ));
    }
    if config.initial_context_tokens as u64 > (config.stable_prefix_tokens as u64) / 2 {
        reasons.push(
            "initial context exceeds half of the stable prefix; the scenario would measure a short session rather than a long stable prefix"
                .to_string(),
        );
    }
    if config.stable_prefix_tokens as u64 > LONG_PREFIX_MIN_TOKENS * 2 {
        reasons.push(format!(
            "stable prefix is {} tokens; above {} a prefix-size-driven ratio is rejected",
            config.stable_prefix_tokens,
            LONG_PREFIX_MIN_TOKENS * 2
        ));
    }
    let size_check = ScenarioSizeCheck {
        passed: reasons.is_empty(),
        total_prefix_tokens: total_prefix,
        mean_turn_delta_tokens: mean_turn_delta,
        turns: config.turns,
        reasons,
    };
    if !size_check.passed && !config.allow_unrealistic {
        bail!(
            "cache-bench scenario rejected: {}",
            size_check.reasons.join("; ")
        );
    }
    Ok(CacheBenchScenario {
        name: match config.kind {
            CacheBenchScenarioKind::WarmLongPrefix => "long_prefix_50_turn_development",
            CacheBenchScenarioKind::ColdLongPrefix => "cold_start_50_turn_development",
            CacheBenchScenarioKind::PrefixBreakControl => "prefix_break_control_50_turn",
        },
        stable_tokens,
        initial_tokens,
        turn_tokens,
        epoch: "sim-epoch-deepseek-prefix-v1".to_string(),
        size_check,
        kind: config.kind,
    })
}

/// Deterministic token ids. The vocabulary is intentionally large enough that
/// adjacent turns are not accidentally identical, while the generator remains
/// reproducible across machines.
fn synthetic_tokens(count: usize, seed: u64) -> Vec<u64> {
    const VOCABULARY: u64 = 4_096;
    let mut state = seed | 1;
    let mut tokens = Vec::with_capacity(count);
    for _ in 0..count {
        state = splitmix64(state);
        tokens.push(state % VOCABULARY);
    }
    tokens
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = value;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn token_prefix_hash(tokens: &[u64]) -> String {
    let mut hasher = BlockHasher::new();
    for token in tokens {
        hasher.update(&token.to_le_bytes());
    }
    hasher.finish_hex()
}

fn classify_miss(
    turn: usize,
    append_only: bool,
    previous_input: u64,
    input: u64,
    miss: u64,
) -> Option<String> {
    if turn == 0 {
        return Some("first_call".to_string());
    }
    if !append_only {
        return Some("prefix_changed".to_string());
    }
    let readable = previous_input.min(input);
    let tail = input.saturating_sub(readable);
    let tolerance = SIMULATOR_BLOCK_SIZE;
    if miss <= tail.saturating_add(tolerance) {
        Some("tail_delta".to_string())
    } else {
        Some("provider_cache_miss".to_string())
    }
}

/// Run the simulated scenario and build a report. The warm stable prefix is
/// cached once before measured turns and is not part of the aggregate.
pub fn run_simulated_benchmark(
    config: CacheBenchScenarioConfig,
    pricing: CacheBenchPricing,
) -> Result<CacheBenchReport> {
    pricing.validate()?;
    let scenario = long_prefix_scenario(config)?;
    let mut simulator = PrefixCacheSimulator::new();
    // A previous session with the same model/tool/project epoch populated the
    // stable prefix. This is the cross-session reuse the plan targets; the
    // warm call itself is excluded and recorded as such. Cold-start and
    // prefix-break controls deliberately skip or invalidate it.
    let cross_session_warm = scenario.kind.primes_cross_session();
    if cross_session_warm {
        simulator.prime(&scenario.epoch, &scenario.stable_tokens);
    }
    let break_turn = if scenario.kind == CacheBenchScenarioKind::PrefixBreakControl {
        (scenario.turn_tokens.len() / 2).max(1)
    } else {
        usize::MAX
    };
    let mut prefix_break_injected = false;

    let mut prompt = Vec::with_capacity(
        scenario.stable_tokens.len()
            + scenario.initial_tokens.len()
            + scenario.turn_tokens.iter().map(Vec::len).sum::<usize>(),
    );
    prompt.extend_from_slice(&scenario.stable_tokens);
    prompt.extend_from_slice(&scenario.initial_tokens);

    let mut calls = Vec::with_capacity(scenario.turn_tokens.len());
    let mut previous_input = 0u64;
    let mut previous_len = 0usize;
    let mut previous_hash: Option<String> = None;
    for (turn, delta) in scenario.turn_tokens.iter().enumerate() {
        prompt.extend_from_slice(delta);
        if turn == break_turn && !scenario.initial_tokens.is_empty() {
            // Mutate one early conversation token. Prior history is no longer
            // an exact prefix, so the provider can only reuse the stable head;
            // the control asserts that the simulator reports the break.
            let index = scenario.stable_tokens.len() + scenario.initial_tokens.len() / 2;
            if let Some(token) = prompt.get_mut(index) {
                *token = token.wrapping_add(1);
                prefix_break_injected = true;
            }
        }
        let append_only = match previous_hash.as_deref() {
            Some(hash) if previous_len > 0 => token_prefix_hash(&prompt[..previous_len]) == hash,
            _ => false,
        };
        let usage = simulator.complete(&scenario.epoch, &prompt);
        let miss_reason = classify_miss(
            turn,
            append_only,
            previous_input,
            usage.input_tokens,
            usage.cache_miss_tokens,
        );
        let cost = pricing.estimate(
            usage.input_tokens,
            usage.cache_read_tokens,
            0,
            OUTPUT_TOKENS_PER_TURN,
        );
        calls.push(CacheBenchCall {
            turn: turn + 1,
            input_tokens: usage.input_tokens,
            output_tokens: OUTPUT_TOKENS_PER_TURN,
            cache_read_tokens: usage.cache_read_tokens,
            cache_write_tokens: 0,
            cache_miss_tokens: usage.cache_miss_tokens,
            cache_hit_ratio: ratio(usage.cache_read_tokens, usage.input_tokens),
            theoretical_max_hit_ratio: vp_core::prompt_cache::theoretical_max_hit_ratio(
                previous_input,
                usage.input_tokens,
            ),
            previous_input_tokens: previous_input,
            append_only,
            miss_reason,
            first_token_ms: None,
            estimated_usage: false,
            cost,
        });
        previous_input = usage.input_tokens;
        previous_len = prompt.len();
        previous_hash = Some(token_prefix_hash(&prompt));
    }
    let aggregate = aggregate_calls(&calls);
    let mut caveats = vec![
        "Simulator tokens are deterministic synthetic word tokens, not a vendor tokenizer; use live mode for provider-reported usage.".to_string(),
        "The stable prefix is primed once as a previous session with the same epoch. The warmup is excluded from measured calls and is reported as warmup_included=false.".to_string(),
        "Cache read tokens are billed. The hit ratio is cache_read/input, not a discount or a provider guarantee.".to_string(),
        "64-token blocks model DeepSeek-style prefix caching. No provider TTL, eviction policy, or future hit is inferred.".to_string(),
    ];
    if pricing.source == PricingSource::IllustrativeDefault {
        caveats.push(
            "Prices are explicit illustrative defaults, not a claim about a vendor's current invoice. Pass all four --*-price flags for a caller-owned estimate."
                .to_string(),
        );
    }
    if !cross_session_warm {
        caveats.push(
            "Cold-start control: no previous-session prime. The first call pays the stable prefix and the long-prefix target is not expected to pass."
                .to_string(),
        );
    }
    if prefix_break_injected {
        caveats.push(
            "Prefix-break control: one early conversation token was changed mid-session. The expected outcome is a prefix_changed call and a lost cache tail, not a provider fault."
                .to_string(),
        );
    }
    if !scenario.size_check.passed {
        caveats.push(
            "The scenario size check was bypassed with --allow-unrealistic-scenario; the hit ratio is not a long-prefix acceptance result."
                .to_string(),
        );
    }
    let total_prefix = scenario.size_check.total_prefix_tokens;
    let mut report = CacheBenchReport {
        schema: CACHE_BENCH_SCHEMA.to_string(),
        scenario: scenario.name.to_string(),
        mode: CacheBenchMode::Simulated,
        provider: "simulator".to_string(),
        model: "deepseek-prefix-sim".to_string(),
        block_size: SIMULATOR_BLOCK_SIZE,
        turns: calls.len(),
        stable_prefix_tokens: scenario.stable_tokens.len() as u64,
        initial_context_tokens: scenario.initial_tokens.len() as u64,
        cross_session_warm,
        warmup_included: false,
        deterministic: true,
        generated_at: None,
        pricing,
        scenario_size: scenario.size_check,
        live_gate: None,
        calls,
        aggregate,
        acceptance: CacheBenchAcceptance::default(),
        caveats: {
            let mut caveats = caveats;
            caveats.push(format!(
                "Total stable+initial prefix is {total_prefix} synthetic tokens; measured calls append {} turns of small deltas.",
                scenario.turn_tokens.len()
            ));
            caveats
        },
    };
    report.acceptance = report.compute_acceptance();
    Ok(report)
}

fn ratio(read: u64, input: u64) -> f64 {
    if input == 0 {
        0.0
    } else {
        (read.min(input) as f64 / input as f64).clamp(0.0, 1.0)
    }
}

fn percentile(sorted: &[f64], percentile: u64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (percentile as f64 / 100.0 * sorted.len() as f64).ceil() as usize;
    sorted[(rank.saturating_sub(1)).min(sorted.len() - 1)]
}

pub fn aggregate_calls(calls: &[CacheBenchCall]) -> CacheBenchAggregate {
    let input_tokens = calls.iter().map(|call| call.input_tokens).sum::<u64>();
    let output_tokens = calls.iter().map(|call| call.output_tokens).sum::<u64>();
    let cache_read_tokens = calls.iter().map(|call| call.cache_read_tokens).sum::<u64>();
    let cache_write_tokens = calls
        .iter()
        .map(|call| call.cache_write_tokens)
        .sum::<u64>();
    let cache_miss_tokens = calls.iter().map(|call| call.cache_miss_tokens).sum::<u64>();
    let mut hit_ratios = calls
        .iter()
        .map(|call| call.cache_hit_ratio)
        .collect::<Vec<_>>();
    let mut theoretical = calls
        .iter()
        .map(|call| call.theoretical_max_hit_ratio)
        .collect::<Vec<_>>();
    hit_ratios.sort_by(|left, right| left.total_cmp(right));
    theoretical.sort_by(|left, right| left.total_cmp(right));
    let input_cost_usd = calls
        .iter()
        .map(|call| call.cost.input_cost_usd)
        .sum::<f64>();
    let cache_read_cost_usd = calls
        .iter()
        .map(|call| call.cost.cache_read_cost_usd)
        .sum::<f64>();
    let cache_write_cost_usd = calls
        .iter()
        .map(|call| call.cost.cache_write_cost_usd)
        .sum::<f64>();
    let output_cost_usd = calls
        .iter()
        .map(|call| call.cost.output_cost_usd)
        .sum::<f64>();
    let total_cost_usd = calls
        .iter()
        .map(|call| call.cost.total_cost_usd)
        .sum::<f64>();
    let no_cache_cost_usd = calls
        .iter()
        .map(|call| call.cost.no_cache_cost_usd)
        .sum::<f64>();
    let cost = CostEstimate {
        input_cost_usd: round_usd(input_cost_usd),
        cache_read_cost_usd: round_usd(cache_read_cost_usd),
        cache_write_cost_usd: round_usd(cache_write_cost_usd),
        output_cost_usd: round_usd(output_cost_usd),
        total_cost_usd: round_usd(total_cost_usd),
        no_cache_cost_usd: round_usd(no_cache_cost_usd),
        estimated_savings_usd: round_usd(no_cache_cost_usd - total_cost_usd),
    };
    CacheBenchAggregate {
        calls: calls.len() as u64,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        cache_miss_tokens,
        cache_hit_ratio: ratio(cache_read_tokens, input_tokens),
        cache_hit_ratio_p50: percentile(&hit_ratios, 50),
        cache_hit_ratio_p95: percentile(&hit_ratios, 95),
        theoretical_max_p50: percentile(&theoretical, 50),
        theoretical_max_p95: percentile(&theoretical, 95),
        prefix_breaks: calls
            .iter()
            .enumerate()
            .filter(|(index, call)| *index > 0 && !call.append_only)
            .count() as u64,
        tail_delta_calls: calls
            .iter()
            .filter(|call| call.miss_reason.as_deref() == Some("tail_delta"))
            .count() as u64,
        provider_cache_miss_calls: calls
            .iter()
            .filter(|call| call.miss_reason.as_deref() == Some("provider_cache_miss"))
            .count() as u64,
        first_call_miss_tokens: calls
            .first()
            .map(|call| call.cache_miss_tokens)
            .unwrap_or(0),
        cost,
    }
}

fn round_usd(value: f64) -> f64 {
    if value.is_finite() {
        (value * 1_000_000.0).round() / 1_000_000.0
    } else {
        0.0
    }
}

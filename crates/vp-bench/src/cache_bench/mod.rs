//! Deterministic and live prompt-cache benchmarks for VerlinPark.
//!
//! The default path is a pure in-process simulator: no network, no provider
//! call, no API key. It models a 64-token block prefix cache over a long stable
//! instruction/repository prefix and a realistic 50-turn append-only
//! development conversation. The live path is explicit opt-in and requires both
//! `--allow-live` and an API key in the environment.
//!
//! The report is deliberately honest:
//!
//! * the simulator token counts are synthetic word tokens, not a vendor
//!   tokenizer;
//! * the cross-session warm is recorded separately and is never counted as a
//!   measured call;
//! * cost is computed from caller-supplied or clearly labeled illustrative
//!   prices, never from a hidden model route;
//! * a simulated run rejects unrealistic scenario shapes (short prefix, huge
//!   per-turn delta, too few turns) instead of inflating the ratio.

pub mod live;
pub mod simulator;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::metrics::Pricing;

pub use live::{
    LIVE_GATE_ENV, LiveCacheBenchSettings, LiveRunOptions, live_settings_from_env,
    run_live_benchmark, run_live_benchmark_with_options, run_live_benchmark_with_provider,
};
pub use simulator::{
    CacheBenchScenario, CacheBenchScenarioConfig, CacheBenchScenarioKind, PrefixCacheSimulator,
    SIMULATOR_BLOCK_SIZE, SIMULATOR_VERSION, long_prefix_scenario, run_simulated_benchmark,
};

/// Stable schema identifier for cache-bench JSON artifacts.
pub const CACHE_BENCH_SCHEMA: &str = "verlinpark.cache-bench/v1";
/// Long-prefix acceptance floor from the lossless cache plan.
pub const LONG_PREFIX_HIT_TARGET: f64 = 0.995;
/// Stretch target reported for stable-prefix, long-context sessions.
pub const LONG_PREFIX_STRETCH_TARGET: f64 = 0.9984;
/// Minimum total prompt tokens for the long-prefix scenario.
pub const LONG_PREFIX_MIN_TOKENS: u64 = 200_000;
/// Maximum mean per-turn delta accepted by the scenario size check.
pub const MAX_MEAN_TURN_DELTA: u64 = 1_024;
/// Minimum measured turns for the long-prefix scenario.
pub const MIN_LONG_PREFIX_TURNS: usize = 20;

/// Simulated or real provider execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheBenchMode {
    Simulated,
    Live,
}

impl CacheBenchMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Simulated => "simulated",
            Self::Live => "live",
        }
    }
}

/// Four explicit per-million prices. Cache reads are billed; a zero
/// cache-read price is accepted only when the caller supplies it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CacheBenchPricing {
    pub input_usd_per_million: f64,
    pub output_usd_per_million: f64,
    pub cache_read_usd_per_million: f64,
    pub cache_write_usd_per_million: f64,
    /// `caller`, `illustrative_default`, or `environment`.
    pub source: PricingSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PricingSource {
    Caller,
    IllustrativeDefault,
    Environment,
}

impl CacheBenchPricing {
    pub fn caller(pricing: Pricing) -> Self {
        Self {
            input_usd_per_million: pricing.input_usd_per_million,
            output_usd_per_million: pricing.output_usd_per_million,
            cache_read_usd_per_million: pricing.cache_read_usd_per_million,
            cache_write_usd_per_million: pricing.cache_write_usd_per_million,
            source: PricingSource::Caller,
        }
    }

    /// Explicit illustrative defaults. These are not a claim about a vendor's
    /// current invoice; reports always carry `source` next to the values.
    pub fn illustrative_default() -> Self {
        Self {
            input_usd_per_million: 0.28,
            output_usd_per_million: 0.42,
            cache_read_usd_per_million: 0.028,
            cache_write_usd_per_million: 0.28,
            source: PricingSource::IllustrativeDefault,
        }
    }

    pub fn validate(&self) -> Result<()> {
        for (name, value) in [
            ("input", self.input_usd_per_million),
            ("output", self.output_usd_per_million),
            ("cache_read", self.cache_read_usd_per_million),
            ("cache_write", self.cache_write_usd_per_million),
        ] {
            if !value.is_finite() || value < 0.0 {
                bail!("cache-bench price '{name}' must be finite and non-negative");
            }
        }
        Ok(())
    }

    pub fn fingerprint(&self) -> String {
        format!(
            "{:.9}|{:.9}|{:.9}|{:.9}",
            self.input_usd_per_million,
            self.output_usd_per_million,
            self.cache_read_usd_per_million,
            self.cache_write_usd_per_million
        )
    }

    pub fn estimate(&self, input: u64, read: u64, write: u64, output: u64) -> CostEstimate {
        let read = read.min(input);
        let write = write.min(input.saturating_sub(read));
        let uncached = input.saturating_sub(read).saturating_sub(write);
        let input_cost = per_million(uncached, self.input_usd_per_million);
        let cache_read_cost = per_million(read, self.cache_read_usd_per_million);
        let cache_write_cost = per_million(write, self.cache_write_usd_per_million);
        let output_cost = per_million(output, self.output_usd_per_million);
        let total = input_cost + cache_read_cost + cache_write_cost + output_cost;
        let no_cache = per_million(input, self.input_usd_per_million)
            + per_million(output, self.output_usd_per_million);
        CostEstimate {
            input_cost_usd: round_usd(input_cost),
            cache_read_cost_usd: round_usd(cache_read_cost),
            cache_write_cost_usd: round_usd(cache_write_cost),
            output_cost_usd: round_usd(output_cost),
            total_cost_usd: round_usd(total),
            no_cache_cost_usd: round_usd(no_cache),
            estimated_savings_usd: round_usd(no_cache - total),
        }
    }
}

fn per_million(tokens: u64, price: f64) -> f64 {
    tokens as f64 * price / 1_000_000.0
}

fn round_usd(value: f64) -> f64 {
    if value.is_finite() {
        (value * 1_000_000.0).round() / 1_000_000.0
    } else {
        0.0
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct CostEstimate {
    pub input_cost_usd: f64,
    pub cache_read_cost_usd: f64,
    pub cache_write_cost_usd: f64,
    pub output_cost_usd: f64,
    pub total_cost_usd: f64,
    pub no_cache_cost_usd: f64,
    pub estimated_savings_usd: f64,
}

/// One measured provider (or simulator) call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheBenchCall {
    pub turn: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub cache_miss_tokens: u64,
    pub cache_hit_ratio: f64,
    pub theoretical_max_hit_ratio: f64,
    pub previous_input_tokens: u64,
    pub append_only: bool,
    pub miss_reason: Option<String>,
    #[serde(default)]
    pub first_token_ms: Option<u64>,
    #[serde(default)]
    pub estimated_usage: bool,
    pub cost: CostEstimate,
}

/// Aggregate over measured calls. `warmup` is intentionally excluded.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CacheBenchAggregate {
    pub calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub cache_miss_tokens: u64,
    pub cache_hit_ratio: f64,
    pub cache_hit_ratio_p50: f64,
    pub cache_hit_ratio_p95: f64,
    pub theoretical_max_p50: f64,
    pub theoretical_max_p95: f64,
    pub prefix_breaks: u64,
    pub tail_delta_calls: u64,
    pub provider_cache_miss_calls: u64,
    pub first_call_miss_tokens: u64,
    pub cost: CostEstimate,
}

/// Scenario shape check. A run fails closed when the shape cannot support a
/// meaningful long-prefix claim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScenarioSizeCheck {
    pub passed: bool,
    pub total_prefix_tokens: u64,
    pub mean_turn_delta_tokens: u64,
    pub turns: usize,
    pub reasons: Vec<String>,
}

/// Guardrail report for the live path. It records what was configured without
/// ever containing the key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveGate {
    pub allow_live: bool,
    pub key_source: String,
    pub base_url: String,
    pub model: String,
    pub extra_env_gate: Option<String>,
}

/// Explicit acceptance verdict for a report. It distinguishes a real
/// long-prefix pass from a cold-start or prefix-break control, and from a
/// synthetic report that cannot claim provider acceptance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheBenchAcceptance {
    pub measurement: String,
    pub provider_reported: bool,
    pub scenario_shape_valid: bool,
    pub cross_session_warm: bool,
    pub hit_ratio: f64,
    pub long_prefix_floor: f64,
    pub stretch_target: f64,
    pub working_day_target: f64,
    pub short_session_target: f64,
    pub floor_met: bool,
    pub stretch_met: bool,
    /// `stretch`, `pass`, `below_floor`, `unrealistic`, `cold_start_control`,
    /// or `prefix_break_control`.
    pub verdict: String,
    pub notes: Vec<String>,
}

impl Default for CacheBenchAcceptance {
    fn default() -> Self {
        Self {
            measurement: "unknown".to_string(),
            provider_reported: false,
            scenario_shape_valid: false,
            cross_session_warm: false,
            hit_ratio: 0.0,
            long_prefix_floor: LONG_PREFIX_HIT_TARGET,
            stretch_target: LONG_PREFIX_STRETCH_TARGET,
            working_day_target: 0.99,
            short_session_target: 0.95,
            floor_met: false,
            stretch_met: false,
            verdict: "unknown".to_string(),
            notes: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheBenchReport {
    pub schema: String,
    pub scenario: String,
    pub mode: CacheBenchMode,
    pub provider: String,
    pub model: String,
    pub block_size: u64,
    pub turns: usize,
    pub stable_prefix_tokens: u64,
    pub initial_context_tokens: u64,
    /// True when the stable prefix was primed by a previous session. That
    /// warmup is never counted as a measured call.
    #[serde(default)]
    pub cross_session_warm: bool,
    pub warmup_included: bool,
    pub deterministic: bool,
    #[serde(default)]
    pub generated_at: Option<String>,
    pub pricing: CacheBenchPricing,
    pub scenario_size: ScenarioSizeCheck,
    #[serde(default)]
    pub live_gate: Option<LiveGate>,
    pub calls: Vec<CacheBenchCall>,
    pub aggregate: CacheBenchAggregate,
    #[serde(default)]
    pub acceptance: CacheBenchAcceptance,
    pub caveats: Vec<String>,
}

impl CacheBenchReport {
    pub fn to_json_pretty(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    /// Per-call CSV for spreadsheet analysis. It uses the same token math as
    /// the JSON report and contains no prompt text.
    pub fn to_csv(&self) -> String {
        let mut out = String::from(
            "turn,input_tokens,output_tokens,cache_read_tokens,cache_write_tokens,cache_miss_tokens,cache_hit_ratio,theoretical_max_hit_ratio,previous_input_tokens,append_only,miss_reason,first_token_ms,estimated_usage,total_cost_usd\n",
        );
        for call in &self.calls {
            out.push_str(&format!(
                "{},{},{},{},{},{},{:.9},{:.9},{},{},{},{},{},{:.9}\n",
                call.turn,
                call.input_tokens,
                call.output_tokens,
                call.cache_read_tokens,
                call.cache_write_tokens,
                call.cache_miss_tokens,
                call.cache_hit_ratio,
                call.theoretical_max_hit_ratio,
                call.previous_input_tokens,
                call.append_only,
                call.miss_reason.as_deref().unwrap_or(""),
                call.first_token_ms
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                call.estimated_usage,
                call.cost.total_cost_usd
            ));
        }
        out
    }

    /// Stable identity for comparisons. Timestamps and per-call rows are
    /// excluded so two deterministic runs compare equal.
    pub fn fingerprint(&self) -> String {
        let canonical = json!({
            "schema": self.schema,
            "scenario": self.scenario,
            "mode": self.mode,
            "provider": self.provider,
            "model": self.model,
            "block_size": self.block_size,
            "turns": self.turns,
            "stable_prefix_tokens": self.stable_prefix_tokens,
            "initial_context_tokens": self.initial_context_tokens,
            "warmup_included": self.warmup_included,
            "pricing": self.pricing.fingerprint(),
        });
        sha256_hex(canonical.to_string().as_bytes())
    }

    pub fn markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# VerlinPark cache benchmark\n\n");
        out.push_str(&format!("- Scenario: `{}`\n", self.scenario));
        out.push_str(&format!(
            "- Mode: `{}` · provider `{}` · model `{}`\n",
            self.mode.as_str(),
            self.provider,
            self.model
        ));
        out.push_str(&format!(
            "- Turns: {} measured · prefix {} tokens · initial context {} tokens\n",
            self.turns, self.stable_prefix_tokens, self.initial_context_tokens
        ));
        out.push_str(&format!(
            "- Simulator: {} · block size {} · deterministic {}\n",
            SIMULATOR_VERSION, self.block_size, self.deterministic
        ));
        out.push_str(&format!(
            "- Prices: input ${:.4}/M · output ${:.4}/M · cache read ${:.4}/M · cache write ${:.4}/M ({:?})\n",
            self.pricing.input_usd_per_million,
            self.pricing.output_usd_per_million,
            self.pricing.cache_read_usd_per_million,
            self.pricing.cache_write_usd_per_million,
            self.pricing.source
        ));
        out.push_str("\n## Aggregate\n\n");
        out.push_str("| metric | value |\n|---|---:|\n");
        out.push_str(&format!("| calls | {} |\n", self.aggregate.calls));
        out.push_str(&format!(
            "| input tokens | {} |\n",
            self.aggregate.input_tokens
        ));
        out.push_str(&format!(
            "| cache read tokens | {} |\n",
            self.aggregate.cache_read_tokens
        ));
        out.push_str(&format!(
            "| cache miss tokens | {} |\n",
            self.aggregate.cache_miss_tokens
        ));
        out.push_str(&format!(
            "| hit ratio | {:.4}% |\n",
            self.aggregate.cache_hit_ratio * 100.0
        ));
        out.push_str(&format!(
            "| p50 / p95 hit ratio | {:.4}% / {:.4}% |\n",
            self.aggregate.cache_hit_ratio_p50 * 100.0,
            self.aggregate.cache_hit_ratio_p95 * 100.0
        ));
        out.push_str(&format!(
            "| theoretical max p50 / p95 | {:.4}% / {:.4}% |\n",
            self.aggregate.theoretical_max_p50 * 100.0,
            self.aggregate.theoretical_max_p95 * 100.0
        ));
        out.push_str(&format!(
            "| prefix breaks | {} |\n",
            self.aggregate.prefix_breaks
        ));
        out.push_str(&format!(
            "| tail delta / provider miss calls | {} / {} |\n",
            self.aggregate.tail_delta_calls, self.aggregate.provider_cache_miss_calls
        ));
        out.push_str(&format!(
            "| total cost | ${:.6} |\n",
            self.aggregate.cost.total_cost_usd
        ));
        out.push_str(&format!(
            "| no-cache cost | ${:.6} |\n",
            self.aggregate.cost.no_cache_cost_usd
        ));
        out.push_str(&format!(
            "| estimated savings | ${:.6} |\n",
            self.aggregate.cost.estimated_savings_usd
        ));
        out.push_str("\n## Acceptance\n\n");
        out.push_str(&format!(
            "- verdict: `{}` · measurement `{}` · provider reported {}\n",
            self.acceptance.verdict, self.acceptance.measurement, self.acceptance.provider_reported
        ));
        out.push_str(&format!(
            "- hit ratio {:.4}% · floor {:.4}% · stretch {:.4}% · floor met {} · stretch met {}\n",
            self.acceptance.hit_ratio * 100.0,
            self.acceptance.long_prefix_floor * 100.0,
            self.acceptance.stretch_target * 100.0,
            self.acceptance.floor_met,
            self.acceptance.stretch_met
        ));
        for note in &self.acceptance.notes {
            out.push_str(&format!("- {note}\n"));
        }
        out.push_str("\n## Scenario size check\n\n");
        out.push_str(&format!("- passed: {}\n", self.scenario_size.passed));
        out.push_str(&format!(
            "- mean turn delta: {} tokens\n",
            self.scenario_size.mean_turn_delta_tokens
        ));
        if !self.scenario_size.reasons.is_empty() {
            out.push_str("- reasons:\n");
            for reason in &self.scenario_size.reasons {
                out.push_str(&format!("  - {reason}\n"));
            }
        }
        if let Some(gate) = &self.live_gate {
            out.push_str("\n## Live gate\n\n");
            out.push_str(&format!(
                "- allow_live: {} · key source: `{}`\n",
                gate.allow_live, gate.key_source
            ));
            out.push_str(&format!("- base URL: `{}`\n", gate.base_url));
            out.push_str(&format!("- model: `{}`\n", gate.model));
        }
        out.push_str("\n## Calls\n\n");
        out.push_str("| turn | input | read | miss | hit | theoretical | append-only | miss reason | cost USD |\n");
        out.push_str("|---:|---:|---:|---:|---:|---:|:---:|---|---:|\n");
        for call in &self.calls {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {:.4}% | {:.4}% | {} | {} | {:.6} |\n",
                call.turn,
                call.input_tokens,
                call.cache_read_tokens,
                call.cache_miss_tokens,
                call.cache_hit_ratio * 100.0,
                call.theoretical_max_hit_ratio * 100.0,
                if call.append_only { "yes" } else { "no" },
                call.miss_reason.as_deref().unwrap_or("—"),
                call.cost.total_cost_usd
            ));
        }
        out.push_str("\n## Caveats\n\n");
        for caveat in &self.caveats {
            out.push_str(&format!("- {caveat}\n"));
        }
        out
    }

    pub fn live_target_met(&self) -> bool {
        self.aggregate.cache_hit_ratio >= LONG_PREFIX_HIT_TARGET
    }

    /// Structural validation for a loaded or embedded report. It rejects
    /// internally inconsistent token math instead of letting a dashboard show
    /// a ratio that the call rows do not support.
    pub fn validate(&self) -> Result<()> {
        if self.schema != CACHE_BENCH_SCHEMA {
            bail!(
                "cache-bench schema is '{}', expected '{CACHE_BENCH_SCHEMA}'",
                self.schema
            );
        }
        if self.block_size == 0 {
            bail!("cache-bench block_size must be positive");
        }
        if self.calls.is_empty() {
            bail!("cache-bench report has no measured calls");
        }
        if self.calls.len() != self.turns {
            bail!(
                "cache-bench report says {} turns but contains {} calls",
                self.turns,
                self.calls.len()
            );
        }
        let mut input = 0u64;
        let mut output = 0u64;
        let mut read = 0u64;
        let mut write = 0u64;
        let mut miss = 0u64;
        let mut total_cost = 0.0f64;
        for (index, call) in self.calls.iter().enumerate() {
            if call.turn != index + 1 {
                bail!(
                    "cache-bench call {} has turn {}, expected {}",
                    index,
                    call.turn,
                    index + 1
                );
            }
            if call.input_tokens == 0 {
                bail!("cache-bench call {} has zero input tokens", call.turn);
            }
            if call.cache_read_tokens > call.input_tokens {
                bail!(
                    "cache-bench call {} reports more cache reads than input",
                    call.turn
                );
            }
            if call.cache_write_tokens > call.input_tokens.saturating_sub(call.cache_read_tokens) {
                bail!(
                    "cache-bench call {} reports cache writes overlapping reads",
                    call.turn
                );
            }
            if call.cache_miss_tokens != call.input_tokens.saturating_sub(call.cache_read_tokens) {
                bail!(
                    "cache-bench call {} miss tokens do not equal input minus reads",
                    call.turn
                );
            }
            for (name, value) in [
                ("hit_ratio", call.cache_hit_ratio),
                ("theoretical_max_hit_ratio", call.theoretical_max_hit_ratio),
            ] {
                if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                    bail!("cache-bench call {} has invalid {name}: {value}", call.turn);
                }
            }
            let expected_ratio = call.cache_read_tokens as f64 / call.input_tokens as f64;
            if (call.cache_hit_ratio - expected_ratio).abs() > 1e-9 {
                bail!(
                    "cache-bench call {} hit ratio does not match token math",
                    call.turn
                );
            }
            for (name, value) in [
                ("input_cost_usd", call.cost.input_cost_usd),
                ("cache_read_cost_usd", call.cost.cache_read_cost_usd),
                ("cache_write_cost_usd", call.cost.cache_write_cost_usd),
                ("output_cost_usd", call.cost.output_cost_usd),
                ("total_cost_usd", call.cost.total_cost_usd),
                ("no_cache_cost_usd", call.cost.no_cache_cost_usd),
            ] {
                if !value.is_finite() || value < 0.0 {
                    bail!("cache-bench call {} has invalid {name}: {value}", call.turn);
                }
            }
            input = input.saturating_add(call.input_tokens);
            output = output.saturating_add(call.output_tokens);
            read = read.saturating_add(call.cache_read_tokens);
            write = write.saturating_add(call.cache_write_tokens);
            miss = miss.saturating_add(call.cache_miss_tokens);
            total_cost += call.cost.total_cost_usd;
        }
        let aggregate = &self.aggregate;
        if aggregate.calls != self.turns as u64
            || aggregate.input_tokens != input
            || aggregate.output_tokens != output
            || aggregate.cache_read_tokens != read
            || aggregate.cache_write_tokens != write
            || aggregate.cache_miss_tokens != miss
        {
            bail!("cache-bench aggregate token totals do not match its call rows");
        }
        let expected = read as f64 / input.max(1) as f64;
        if !aggregate.cache_hit_ratio.is_finite()
            || (aggregate.cache_hit_ratio - expected).abs() > 1e-9
        {
            bail!("cache-bench aggregate hit ratio does not match token math");
        }
        if (aggregate.cost.total_cost_usd - total_cost).abs() > 1e-5 {
            bail!("cache-bench aggregate cost does not match its call rows");
        }
        if self.calls[0].append_only {
            bail!("cache-bench first measured call cannot be append-only");
        }
        if self.acceptance.hit_ratio != aggregate.cache_hit_ratio {
            bail!("cache-bench acceptance ratio disagrees with the aggregate");
        }
        Ok(())
    }

    /// Derive the honest acceptance verdict. The 99.84% stretch and 99.5%
    /// floor apply only to a valid warm long-prefix scenario; cold-start and
    /// prefix-break reports are controls even if their raw ratio happens to be
    /// high.
    pub fn compute_acceptance(&self) -> CacheBenchAcceptance {
        let provider_reported = self.mode == CacheBenchMode::Live
            && !self.calls.is_empty()
            && self.calls.iter().all(|call| !call.estimated_usage);
        let measurement = match self.mode {
            CacheBenchMode::Simulated => "simulator",
            CacheBenchMode::Live if provider_reported => "provider_reported",
            CacheBenchMode::Live => "provider_estimated",
        }
        .to_string();
        let ratio = self.aggregate.cache_hit_ratio;
        let floor_met = ratio >= LONG_PREFIX_HIT_TARGET;
        let stretch_met = ratio >= LONG_PREFIX_STRETCH_TARGET;
        let prefix_break_control = self.aggregate.prefix_breaks > 0;
        let cold_start_control = !self.cross_session_warm
            && self.mode == CacheBenchMode::Simulated
            && self.scenario.starts_with("cold_start");
        let mut notes = Vec::new();
        let verdict = if prefix_break_control {
            notes.push(
                "Prefix-break control: the raw ratio is not an acceptance result. The control exists to prove break detection and lost-cache accounting."
                    .to_string(),
            );
            "prefix_break_control"
        } else if cold_start_control {
            notes.push(
                "Cold-start control: no prior-session prefix. The first call pays the whole stable prefix, so the long-prefix target does not apply."
                    .to_string(),
            );
            "cold_start_control"
        } else if !self.scenario_size.passed {
            notes.push(
                "The scenario shape check failed; this report cannot claim a long-prefix acceptance result."
                    .to_string(),
            );
            "unrealistic"
        } else if self.mode == CacheBenchMode::Simulated {
            notes.push(
                "Simulator result: deterministic and useful for regression, but not provider-reported evidence."
                    .to_string(),
            );
            if stretch_met {
                "stretch"
            } else if floor_met {
                "pass"
            } else {
                "below_floor"
            }
        } else if !provider_reported {
            notes.push(
                "At least one live call omitted provider usage; the ratio includes estimated token counts."
                    .to_string(),
            );
            if floor_met {
                "provider_estimated_floor_met"
            } else {
                "provider_estimated_below_floor"
            }
        } else if stretch_met {
            "stretch"
        } else if floor_met {
            "pass"
        } else {
            "below_floor"
        };
        if self.aggregate.cache_miss_tokens > 0 && prefix_break_control {
            notes.push(format!(
                "Observed {} uncached token(s) across {} calls; the expected tail plus the injected break explain the miss.",
                self.aggregate.cache_miss_tokens, self.aggregate.calls
            ));
        }
        CacheBenchAcceptance {
            measurement,
            provider_reported,
            scenario_shape_valid: self.scenario_size.passed,
            cross_session_warm: self.cross_session_warm,
            hit_ratio: ratio,
            long_prefix_floor: LONG_PREFIX_HIT_TARGET,
            stretch_target: LONG_PREFIX_STRETCH_TARGET,
            working_day_target: 0.99,
            short_session_target: 0.95,
            floor_met,
            stretch_met,
            verdict: verdict.to_string(),
            notes,
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// One refused comparison. The caller receives every mismatch, not just the
/// first, so a report diff is actionable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheBenchRefusal {
    pub field: String,
    pub left: String,
    pub right: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheBenchComparison {
    pub schema: String,
    pub scenario: String,
    pub left_fingerprint: String,
    pub right_fingerprint: String,
    pub hit_ratio_left: f64,
    pub hit_ratio_right: f64,
    pub hit_ratio_delta: f64,
    pub miss_token_delta: i64,
    pub total_cost_delta_usd: f64,
    pub left_target_met: bool,
    pub right_target_met: bool,
    pub compatible: bool,
    #[serde(default)]
    pub refusals: Vec<CacheBenchRefusal>,
}

/// Compare two reports. The default is strict: a different scenario, model,
/// mode, turn count, block size, or price profile is refused because its ratio
/// is not comparable. `allow_mismatch` labels the comparison instead of
/// silently accepting it.
pub fn compare_cache_bench_reports(
    left: &CacheBenchReport,
    right: &CacheBenchReport,
    allow_mismatch: bool,
) -> Result<CacheBenchComparison> {
    let mut refusals = Vec::new();
    let mut check = |field: &str, a: String, b: String| {
        if a != b {
            refusals.push(CacheBenchRefusal {
                field: field.to_string(),
                left: a,
                right: b,
            });
        }
    };
    check("schema", left.schema.clone(), right.schema.clone());
    check("scenario", left.scenario.clone(), right.scenario.clone());
    check(
        "mode",
        left.mode.as_str().into(),
        right.mode.as_str().into(),
    );
    check("provider", left.provider.clone(), right.provider.clone());
    check("model", left.model.clone(), right.model.clone());
    check(
        "block_size",
        left.block_size.to_string(),
        right.block_size.to_string(),
    );
    check("turns", left.turns.to_string(), right.turns.to_string());
    check(
        "stable_prefix_tokens",
        left.stable_prefix_tokens.to_string(),
        right.stable_prefix_tokens.to_string(),
    );
    check(
        "initial_context_tokens",
        left.initial_context_tokens.to_string(),
        right.initial_context_tokens.to_string(),
    );
    check(
        "pricing",
        left.pricing.fingerprint(),
        right.pricing.fingerprint(),
    );
    if !refusals.is_empty() && !allow_mismatch {
        bail!(
            "cache-bench reports are not comparable: {}",
            refusals
                .iter()
                .map(|refusal| format!("{} ({} vs {})", refusal.field, refusal.left, refusal.right))
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    let delta = left.aggregate.cache_hit_ratio - right.aggregate.cache_hit_ratio;
    Ok(CacheBenchComparison {
        schema: CACHE_BENCH_SCHEMA.to_string(),
        scenario: left.scenario.clone(),
        left_fingerprint: left.fingerprint(),
        right_fingerprint: right.fingerprint(),
        hit_ratio_left: left.aggregate.cache_hit_ratio,
        hit_ratio_right: right.aggregate.cache_hit_ratio,
        hit_ratio_delta: delta,
        miss_token_delta: left.aggregate.cache_miss_tokens as i64
            - right.aggregate.cache_miss_tokens as i64,
        total_cost_delta_usd: round_usd(
            left.aggregate.cost.total_cost_usd - right.aggregate.cost.total_cost_usd,
        ),
        left_target_met: left.live_target_met(),
        right_target_met: right.live_target_met(),
        compatible: refusals.is_empty(),
        refusals,
    })
}

/// Read a saved JSON report. Strict schema checking keeps a future report from
/// being interpreted as a current one.
pub fn load_cache_bench_report(path: &Path) -> Result<CacheBenchReport> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("could not read {}", path.display()))?;
    let value: Value = serde_json::from_str(&text)
        .with_context(|| format!("{} is not valid JSON", path.display()))?;
    let schema = value
        .get("schema")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("{} has no schema field", path.display()))?;
    if schema != CACHE_BENCH_SCHEMA {
        bail!(
            "{} uses cache-bench schema '{schema}', expected '{CACHE_BENCH_SCHEMA}'",
            path.display()
        );
    }
    let report: CacheBenchReport = serde_json::from_value(value)
        .with_context(|| format!("{} is not a cache-bench report", path.display()))?;
    report
        .validate()
        .with_context(|| format!("{} failed cache-bench validation", path.display()))?;
    Ok(report)
}

/// Write JSON and Markdown artifacts. The caller owns the directory.
pub fn write_cache_bench_artifacts(
    directory: &Path,
    report: &CacheBenchReport,
) -> Result<(PathBuf, PathBuf)> {
    std::fs::create_dir_all(directory)
        .with_context(|| format!("could not create {}", directory.display()))?;
    let suffix = match report.mode {
        CacheBenchMode::Simulated => "simulated",
        CacheBenchMode::Live => "live",
    };
    let json_path = directory.join(format!("cache-bench-{suffix}.json"));
    let markdown_path = directory.join(format!("cache-bench-{suffix}.md"));
    std::fs::write(&json_path, report.to_json_pretty()?)
        .with_context(|| format!("could not write {}", json_path.display()))?;
    std::fs::write(&markdown_path, report.markdown())
        .with_context(|| format!("could not write {}", markdown_path.display()))?;
    Ok((json_path, markdown_path))
}

// ---------------------------------------------------------------------------
// CLI surface shared by `vp cache-bench` and the library.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Subcommand)]
pub enum CacheBenchCommand {
    /// Run the deterministic simulator (default) or the explicit live mode.
    Run(CacheBenchRunArgs),
    /// Compare two cache-bench JSON reports. Mismatches are refused by default.
    Compare {
        left: PathBuf,
        right: PathBuf,
        /// Label an apples-to-oranges comparison instead of refusing it.
        #[arg(long)]
        allow_mismatch: bool,
    },
    /// Render a saved cache-bench JSON report as Markdown.
    Report { file: PathBuf },
    /// CI gate: validate a saved report and require a hit-ratio floor.
    Verify {
        file: PathBuf,
        /// Minimum aggregate hit ratio (0..=1). Defaults to the 99.5% floor.
        #[arg(long, default_value_t = LONG_PREFIX_HIT_TARGET)]
        min_hit_ratio: f64,
        /// Require every call to be provider-reported (live mode with usage).
        #[arg(long)]
        require_provider_reported: bool,
        /// Accept a cold-start/prefix-break control even below the floor.
        #[arg(long)]
        allow_control: bool,
    },
    /// Run the warm baseline plus both negative controls and compare them.
    Matrix(CacheBenchMatrixArgs),
}

#[derive(Debug, Clone, Args)]
pub struct CacheBenchMatrixArgs {
    #[arg(long, default_value_t = 50)]
    pub turns: usize,
    #[arg(long, default_value_t = 180_000)]
    pub stable_prefix_tokens: usize,
    #[arg(long, default_value_t = 20_000)]
    pub initial_context_tokens: usize,
    /// Directory for the three JSON/Markdown report pairs.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheBenchMatrix {
    pub schema: String,
    pub scenarios: Vec<CacheBenchReport>,
}

impl CacheBenchMatrix {
    pub fn markdown(&self) -> String {
        let mut out = String::from("# VerlinPark cache-bench scenario matrix\n\n");
        out.push_str("| scenario | verdict | hit ratio | miss tokens | breaks | floor met |\n");
        out.push_str("|---|---|---:|---:|---:|:---:|\n");
        for report in &self.scenarios {
            out.push_str(&format!(
                "| {} | `{}` | {:.4}% | {} | {} | {} |\n",
                report.scenario,
                report.acceptance.verdict,
                report.aggregate.cache_hit_ratio * 100.0,
                report.aggregate.cache_miss_tokens,
                report.aggregate.prefix_breaks,
                report.acceptance.floor_met
            ));
        }
        out.push_str("\nThe cold-start and prefix-break rows are controls; they are expected to miss the long-prefix floor. Compare them with the warm baseline, not with each other.\n");
        out
    }
}

#[derive(Debug, Clone, Default, Args)]
pub struct CacheBenchRunArgs {
    /// Number of measured turns (minimum 20 for the long-prefix scenario).
    #[arg(long, default_value_t = 50)]
    pub turns: usize,
    /// Stable instruction/repository prefix tokens (default 160k).
    #[arg(long, default_value_t = 180_000)]
    pub stable_prefix_tokens: usize,
    /// Initial conversation/tool-output tokens before the measured turns.
    #[arg(long, default_value_t = 20_000)]
    pub initial_context_tokens: usize,
    /// Enable the live provider path. Never the default.
    #[arg(long)]
    pub live: bool,
    /// Required acknowledgement for live mode, in addition to an API key.
    #[arg(long)]
    pub allow_live: bool,
    /// Live model override. Also readable from the environment.
    #[arg(long)]
    pub model: Option<String>,
    /// Live base URL override. Also readable from the environment.
    #[arg(long)]
    pub base_url: Option<String>,
    /// Directory for JSON/Markdown artifacts.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// USD per million input tokens.
    #[arg(long)]
    pub input_price: Option<f64>,
    /// USD per million output tokens.
    #[arg(long)]
    pub output_price: Option<f64>,
    /// USD per million cache-read tokens.
    #[arg(long)]
    pub cache_read_price: Option<f64>,
    /// USD per million cache-write tokens.
    #[arg(long)]
    pub cache_write_price: Option<f64>,
    /// Reject a scenario whose shape cannot support a long-prefix claim.
    #[arg(long)]
    pub allow_unrealistic_scenario: bool,
    /// Exit nonzero when the long-prefix target is missed.
    #[arg(long)]
    pub require_target: bool,
    /// Print the per-call table as CSV instead of Markdown.
    #[arg(long)]
    pub csv: bool,
    /// Stop live mode before the projected spend exceeds this USD estimate.
    #[arg(long)]
    pub max_live_cost_usd: Option<f64>,
    /// Negative control: do not prime the stable prefix from a previous
    /// session. The first call pays the whole prefix.
    #[arg(long, conflicts_with = "prefix_break")]
    pub cold_start: bool,
    /// Negative control: change one early conversation token mid-session and
    /// assert that the simulator reports a prefix break.
    #[arg(long, conflicts_with = "cold_start")]
    pub prefix_break: bool,
}

impl CacheBenchRunArgs {
    pub fn pricing(&self) -> Result<CacheBenchPricing> {
        let explicit = self.input_price.is_some()
            || self.output_price.is_some()
            || self.cache_read_price.is_some()
            || self.cache_write_price.is_some();
        let base = if explicit {
            CacheBenchPricing {
                input_usd_per_million: self.input_price.unwrap_or(0.28),
                output_usd_per_million: self.output_price.unwrap_or(0.42),
                cache_read_usd_per_million: self.cache_read_price.unwrap_or(0.028),
                cache_write_usd_per_million: self.cache_write_price.unwrap_or(0.28),
                source: PricingSource::Caller,
            }
        } else {
            CacheBenchPricing::illustrative_default()
        };
        base.validate()?;
        Ok(base)
    }

    pub fn scenario_config(&self) -> CacheBenchScenarioConfig {
        CacheBenchScenarioConfig {
            turns: self.turns,
            stable_prefix_tokens: self.stable_prefix_tokens,
            initial_context_tokens: self.initial_context_tokens,
            allow_unrealistic: self.allow_unrealistic_scenario,
            kind: if self.prefix_break {
                CacheBenchScenarioKind::PrefixBreakControl
            } else if self.cold_start {
                CacheBenchScenarioKind::ColdLongPrefix
            } else {
                CacheBenchScenarioKind::WarmLongPrefix
            },
        }
    }
}

/// Execute one cache-bench command and return the process exit code.
///
/// Exit codes: 0 success/target met, 1 target missed with `--require-target`,
/// 2 configuration/report error.
pub async fn execute(command: CacheBenchCommand, json: bool) -> Result<i32> {
    match command {
        CacheBenchCommand::Run(args) => {
            let pricing = args.pricing()?;
            if args.live && (args.cold_start || args.prefix_break) {
                bail!(
                    "--cold-start/--prefix-break are deterministic simulator controls; they cannot be combined with --live"
                );
            }
            let report = if args.live {
                let settings = live_settings_from_env(
                    args.allow_live,
                    args.model.as_deref(),
                    args.base_url.as_deref(),
                    args.turns,
                )?;
                run_live_benchmark_with_options(
                    &settings,
                    pricing,
                    LiveRunOptions {
                        max_cost_usd: args.max_live_cost_usd,
                    },
                )
                .await?
            } else {
                run_simulated_benchmark(args.scenario_config(), pricing)?
            };
            if let Some(directory) = args.output.as_deref() {
                let (json_path, markdown_path) = write_cache_bench_artifacts(directory, &report)?;
                if !json {
                    eprintln!(
                        "wrote {} and {}",
                        json_path.display(),
                        markdown_path.display()
                    );
                }
            }
            if json {
                println!("{}", report.to_json()?);
            } else if args.csv {
                print!("{}", report.to_csv());
            } else {
                print!("{}", report.markdown());
            }
            if args.require_target && !report.live_target_met() {
                eprintln!(
                    "cache-bench target missed: {:.4}% < {:.4}%",
                    report.aggregate.cache_hit_ratio * 100.0,
                    LONG_PREFIX_HIT_TARGET * 100.0
                );
                return Ok(1);
            }
            Ok(0)
        }
        CacheBenchCommand::Matrix(args) => {
            let pricing = CacheBenchPricing::illustrative_default();
            let base = CacheBenchScenarioConfig {
                turns: args.turns,
                stable_prefix_tokens: args.stable_prefix_tokens,
                initial_context_tokens: args.initial_context_tokens,
                allow_unrealistic: false,
                kind: CacheBenchScenarioKind::WarmLongPrefix,
            };
            let scenarios = [
                CacheBenchScenarioKind::WarmLongPrefix,
                CacheBenchScenarioKind::ColdLongPrefix,
                CacheBenchScenarioKind::PrefixBreakControl,
            ]
            .into_iter()
            .map(|kind| run_simulated_benchmark(CacheBenchScenarioConfig { kind, ..base }, pricing))
            .collect::<Result<Vec<_>>>()?;
            let matrix = CacheBenchMatrix {
                schema: "verlinpark.cache-bench-matrix/v1".to_string(),
                scenarios,
            };
            if let Some(directory) = args.output.as_deref() {
                std::fs::create_dir_all(directory)?;
                for (report, suffix) in
                    matrix
                        .scenarios
                        .iter()
                        .zip(["warm", "cold-start", "prefix-break"])
                {
                    std::fs::write(
                        directory.join(format!("cache-bench-matrix-{suffix}.json")),
                        report.to_json_pretty()?,
                    )?;
                    std::fs::write(
                        directory.join(format!("cache-bench-matrix-{suffix}.md")),
                        report.markdown(),
                    )?;
                }
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&matrix)?);
            } else {
                print!("{}", matrix.markdown());
            }
            Ok(0)
        }
        CacheBenchCommand::Compare {
            left,
            right,
            allow_mismatch,
        } => {
            let left = load_cache_bench_report(&left)?;
            let right = load_cache_bench_report(&right)?;
            let comparison = compare_cache_bench_reports(&left, &right, allow_mismatch)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&comparison)?);
            } else {
                println!("cache-bench compare  {}", comparison.scenario);
                println!(
                    "  hit ratio   {:.4}% -> {:.4}%  ({:+.4} pts)",
                    comparison.hit_ratio_left * 100.0,
                    comparison.hit_ratio_right * 100.0,
                    comparison.hit_ratio_delta * 100.0
                );
                println!(
                    "  miss tokens {:+.0} · cost {:+.6} USD",
                    comparison.miss_token_delta, comparison.total_cost_delta_usd
                );
                println!(
                    "  target      left {} · right {} · compatible {}",
                    if comparison.left_target_met {
                        "met"
                    } else {
                        "missed"
                    },
                    if comparison.right_target_met {
                        "met"
                    } else {
                        "missed"
                    },
                    comparison.compatible
                );
                for refusal in &comparison.refusals {
                    println!(
                        "  mismatch    {}: {} vs {}",
                        refusal.field, refusal.left, refusal.right
                    );
                }
            }
            Ok(0)
        }
        CacheBenchCommand::Report { file } => {
            let report = load_cache_bench_report(&file)?;
            if json {
                println!("{}", report.to_json()?);
            } else {
                print!("{}", report.markdown());
            }
            Ok(0)
        }
        CacheBenchCommand::Verify {
            file,
            min_hit_ratio,
            require_provider_reported,
            allow_control,
        } => {
            if !min_hit_ratio.is_finite() || !(0.0..=1.0).contains(&min_hit_ratio) {
                bail!("--min-hit-ratio must be finite and between 0 and 1");
            }
            let report = load_cache_bench_report(&file)?;
            let control = report.acceptance.verdict.contains("control");
            let floor_met = report.aggregate.cache_hit_ratio >= min_hit_ratio;
            let provider_ok = !require_provider_reported || report.acceptance.provider_reported;
            let control_ok = !control || allow_control;
            let passed = floor_met && provider_ok && control_ok;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "schema": "verlinpark.cache-bench-verify/v1",
                        "file": file,
                        "passed": passed,
                        "hit_ratio": report.aggregate.cache_hit_ratio,
                        "min_hit_ratio": min_hit_ratio,
                        "floor_met": floor_met,
                        "provider_reported": report.acceptance.provider_reported,
                        "provider_required": require_provider_reported,
                        "provider_ok": provider_ok,
                        "verdict": report.acceptance.verdict,
                        "control": control,
                        "allow_control": allow_control,
                        "control_ok": control_ok
                    }))?
                );
            } else {
                println!("cache-bench verify  {}", file.display());
                println!(
                    "  hit ratio        {:.4}% (required {:.4}%) · {}",
                    report.aggregate.cache_hit_ratio * 100.0,
                    min_hit_ratio * 100.0,
                    if floor_met { "pass" } else { "FAIL" }
                );
                println!(
                    "  measurement      {} · provider reported {} · {}",
                    report.acceptance.measurement,
                    report.acceptance.provider_reported,
                    if provider_ok { "pass" } else { "FAIL" }
                );
                println!(
                    "  verdict          {} · control {} · {}",
                    report.acceptance.verdict,
                    control,
                    if control_ok { "pass" } else { "FAIL" }
                );
                println!(
                    "  result           {}",
                    if passed { "PASS" } else { "FAIL" }
                );
            }
            Ok(if passed { 0 } else { 1 })
        }
    }
}

#[cfg(test)]
mod tests;

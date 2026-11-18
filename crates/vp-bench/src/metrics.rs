//! Token, pricing, and derived-metric math for the benchmark harness.
//!
//! The formulas intentionally mirror the daemon's usage accounting:
//!
//! * `input_tokens` is the provider's total input and includes cache reads and
//!   cache writes.
//! * cache reads and cache writes are disjoint subsets of that total.
//! * cost is charged once per disjoint portion at the caller-supplied per
//!   million prices.
//!
//! A benchmark must never invent a price or silently fall back to a free model.
//! The caller supplies all four prices, and the report records them verbatim.

use serde::{Deserialize, Serialize};

/// Per-million-token USD prices for one benchmark run.
///
/// All four fields are explicit even when a provider does not publish a
/// separate cache price. Use [`Pricing::with_cache_fallback`] to make that
/// choice visible instead of hiding it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Pricing {
    pub input_usd_per_million: f64,
    pub output_usd_per_million: f64,
    pub cache_read_usd_per_million: f64,
    pub cache_write_usd_per_million: f64,
}

impl Pricing {
    /// Fully explicit constructor.
    pub fn new(
        input_usd_per_million: f64,
        output_usd_per_million: f64,
        cache_read_usd_per_million: f64,
        cache_write_usd_per_million: f64,
    ) -> Self {
        Self {
            input_usd_per_million,
            output_usd_per_million,
            cache_read_usd_per_million,
            cache_write_usd_per_million,
        }
    }

    /// Conservative fallback: cache reads and writes cost the full input price.
    pub fn with_cache_fallback(input_usd_per_million: f64, output_usd_per_million: f64) -> Self {
        Self::new(
            input_usd_per_million,
            output_usd_per_million,
            input_usd_per_million,
            input_usd_per_million,
        )
    }

    /// The benchmark's illustrative default price profile.
    ///
    /// These numbers are not a claim about any vendor's current bill. Reports
    /// label them as illustrative defaults (see [`PricingProvenance`]) and
    /// include the values.
    pub fn benchmark_default() -> Self {
        Self::new(3.0, 15.0, 0.30, 3.75)
    }

    /// Validate that every price is finite and non-negative.
    pub fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            ("input", self.input_usd_per_million),
            ("output", self.output_usd_per_million),
            ("cache_read", self.cache_read_usd_per_million),
            ("cache_write", self.cache_write_usd_per_million),
        ] {
            if !value.is_finite() || value < 0.0 {
                return Err(format!(
                    "price '{name}' must be a finite, non-negative USD per-million value"
                ));
            }
        }
        Ok(())
    }

    /// Stable fingerprint used to refuse an A/B comparison under different
    /// prices. Prices are formatted at nanodollar precision because that is
    /// already far below any observable provider charge.
    pub fn fingerprint(&self) -> String {
        let canonical = format!(
            "{:.9}|{:.9}|{:.9}|{:.9}",
            self.input_usd_per_million,
            self.output_usd_per_million,
            self.cache_read_usd_per_million,
            self.cache_write_usd_per_million
        );
        sha256_hex(canonical.as_bytes())
    }
}

/// Where one price value came from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceSource {
    /// A report written before provenance was recorded. Never invent a source.
    #[default]
    Unknown,
    /// Supplied explicitly by the operator for this invocation.
    Caller,
    /// The benchmark's illustrative default, not a vendor quote.
    IllustrativeDefault,
    /// Some fields were explicit and others kept the illustrative default.
    PartialCaller,
    /// Read from a named environment integration.
    Environment,
}

impl PriceSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Caller => "caller",
            Self::IllustrativeDefault => "illustrative_default",
            Self::PartialCaller => "partial_caller",
            Self::Environment => "environment",
        }
    }
}

/// Per-field pricing provenance recorded next to the numeric values.
///
/// A report must not call an illustrative default "caller-supplied". Keeping
/// one source per field also makes a partial `--*-price` override visible
/// instead of labelling all four values as caller-owned.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PricingProvenance {
    pub source: PriceSource,
    pub input: PriceSource,
    pub output: PriceSource,
    pub cache_read: PriceSource,
    pub cache_write: PriceSource,
}

impl PricingProvenance {
    pub fn illustrative_default() -> Self {
        Self {
            source: PriceSource::IllustrativeDefault,
            input: PriceSource::IllustrativeDefault,
            output: PriceSource::IllustrativeDefault,
            cache_read: PriceSource::IllustrativeDefault,
            cache_write: PriceSource::IllustrativeDefault,
        }
    }

    pub fn caller() -> Self {
        Self {
            source: PriceSource::Caller,
            input: PriceSource::Caller,
            output: PriceSource::Caller,
            cache_read: PriceSource::Caller,
            cache_write: PriceSource::Caller,
        }
    }

    /// Build per-field provenance from the four explicit-override flags.
    pub fn from_overrides(input: bool, output: bool, cache_read: bool, cache_write: bool) -> Self {
        let field = |explicit: bool| {
            if explicit {
                PriceSource::Caller
            } else {
                PriceSource::IllustrativeDefault
            }
        };
        let count = [input, output, cache_read, cache_write]
            .into_iter()
            .filter(|explicit| *explicit)
            .count();
        let source = match count {
            0 => PriceSource::IllustrativeDefault,
            4 => PriceSource::Caller,
            _ => PriceSource::PartialCaller,
        };
        Self {
            source,
            input: field(input),
            output: field(output),
            cache_read: field(cache_read),
            cache_write: field(cache_write),
        }
    }

    pub fn label(&self) -> &'static str {
        self.source.as_str()
    }

    pub fn all_caller(&self) -> bool {
        self.source == PriceSource::Caller
    }

    /// Reject an internally contradictory provenance record.
    pub fn validate(&self) -> Result<(), String> {
        let fields = [self.input, self.output, self.cache_read, self.cache_write];
        match self.source {
            PriceSource::Caller => {
                if fields.iter().any(|field| *field != PriceSource::Caller) {
                    return Err(
                        "pricing source 'caller' requires every field to be caller-supplied"
                            .to_string(),
                    );
                }
            }
            PriceSource::IllustrativeDefault => {
                if fields
                    .iter()
                    .any(|field| *field != PriceSource::IllustrativeDefault)
                {
                    return Err(
                        "pricing source 'illustrative_default' requires every field to use the illustrative default"
                            .to_string(),
                    );
                }
            }
            PriceSource::PartialCaller => {
                let caller = fields
                    .iter()
                    .filter(|field| **field == PriceSource::Caller)
                    .count();
                if caller == 0 || caller == fields.len() {
                    return Err(
                        "pricing source 'partial_caller' requires a mix of caller and default fields"
                            .to_string(),
                    );
                }
            }
            PriceSource::Environment | PriceSource::Unknown => {}
        }
        Ok(())
    }

    /// Stable fingerprint included in the comparison key.
    pub fn fingerprint(&self) -> String {
        sha256_hex(
            format!(
                "{}|{}|{}|{}|{}",
                self.source.as_str(),
                self.input.as_str(),
                self.output.as_str(),
                self.cache_read.as_str(),
                self.cache_write.as_str()
            )
            .as_bytes(),
        )
    }
}

/// Remove URL userinfo, query, and fragment before an endpoint is persisted in
/// a report. The path is kept because gateways are commonly distinguished by
/// path, but credentials and signed query strings never belong in a benchmark
/// artifact.
pub fn redact_endpoint(value: &str) -> String {
    let trimmed = value.trim();
    let (prefix, rest) = match trimmed.split_once("://") {
        Some((scheme, rest)) => (format!("{scheme}://"), rest),
        None => (String::new(), trimmed),
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let host = authority.rsplit('@').next().unwrap_or(authority);
    let tail = &rest[authority_end..];
    let path_end = tail.find(['?', '#']).unwrap_or(tail.len());
    format!("{prefix}{host}{}", &tail[..path_end])
}

/// Token counts accumulated for one run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Provider total input, including cache reads and cache writes.
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Subset of `input_tokens`.
    pub cache_read_tokens: u64,
    /// Subset of `input_tokens`.
    pub cache_write_tokens: u64,
    pub calls: u64,
}

impl TokenUsage {
    pub fn total_input_and_output(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }

    /// Normalize inconsistent provider metadata so cache buckets stay inside
    /// the total input and never overlap. This mirrors the daemon ledger.
    pub fn normalized(&self) -> Self {
        let cache_read = self.cache_read_tokens.min(self.input_tokens);
        let cache_write = self
            .cache_write_tokens
            .min(self.input_tokens.saturating_sub(cache_read));
        Self {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_read_tokens: cache_read,
            cache_write_tokens: cache_write,
            calls: self.calls,
        }
    }

    /// Cache reads divided by the disjoint input buckets. `None` when there is
    /// no input at all; an absent denominator is not a 0% hit rate.
    pub fn cache_hit_ratio(&self) -> Option<f64> {
        let usage = self.normalized();
        let uncached = usage
            .input_tokens
            .saturating_sub(usage.cache_read_tokens)
            .saturating_sub(usage.cache_write_tokens);
        let denominator = uncached
            .saturating_add(usage.cache_read_tokens)
            .saturating_add(usage.cache_write_tokens);
        if denominator == 0 {
            None
        } else {
            Some((usage.cache_read_tokens as f64 / denominator as f64).clamp(0.0, 1.0))
        }
    }
}

/// Price one usage record with explicit per-million prices.
pub fn priced_cost(usage: &TokenUsage, pricing: &Pricing) -> f64 {
    let usage = usage.normalized();
    let uncached = usage
        .input_tokens
        .saturating_sub(usage.cache_read_tokens)
        .saturating_sub(usage.cache_write_tokens);
    let per_million = |tokens: u64, price: f64| (tokens as f64 / 1_000_000.0) * price;
    per_million(uncached, pricing.input_usd_per_million)
        + per_million(usage.cache_read_tokens, pricing.cache_read_usd_per_million)
        + per_million(
            usage.cache_write_tokens,
            pricing.cache_write_usd_per_million,
        )
        + per_million(usage.output_tokens, pricing.output_usd_per_million)
}

/// A ratio with an explicit `None` for a zero denominator.
pub fn ratio_or_none(numerator: u64, denominator: u64) -> Option<f64> {
    if denominator == 0 {
        None
    } else {
        Some(numerator as f64 / denominator as f64)
    }
}

/// `numerator / solved` where a zero solved count yields `None`, never zero or
/// infinity. The benchmark CLI renders `None` as `n/a`.
pub fn cost_per_solved(total_cost_usd: f64, solved_tasks: u64) -> Option<f64> {
    if solved_tasks == 0 {
        None
    } else {
        Some(total_cost_usd / solved_tasks as f64)
    }
}

/// `tokens / solved` with the same zero-solved rule as [`cost_per_solved`].
pub fn tokens_per_solved(total_tokens: u64, solved_tasks: u64) -> Option<f64> {
    if solved_tasks == 0 {
        None
    } else {
        Some(total_tokens as f64 / solved_tasks as f64)
    }
}

/// Percent change from `a` to `b`. Returns `None` when `a` is absent or zero,
/// because a relative change from zero has no finite interpretation.
pub fn percent_change(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(a), Some(b)) if a.abs() > f64::EPSILON => Some(((b - a) / a) * 100.0),
        _ => None,
    }
}

/// Absolute percentage-point change between two ratios.
pub fn point_change(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(a), Some(b)) => Some((b - a) * 100.0),
        _ => None,
    }
}

/// Format a USD amount with stable precision for Markdown, CSV, and CLI text.
pub fn format_usd(value: f64) -> String {
    format!("{value:.6}")
}

/// Format an optional ratio as a percentage with one decimal.
pub fn format_percent(value: Option<f64>) -> String {
    match value {
        Some(value) => format!("{:.1}%", value * 100.0),
        None => "n/a".to_string(),
    }
}

/// Format an optional number with a caller-selected precision.
pub fn format_optional(value: Option<f64>, precision: usize) -> String {
    match value {
        Some(value) => format!("{value:.precision$}"),
        None => "n/a".to_string(),
    }
}

/// Round USD values before they enter a JSON report. Provider costs are
/// fractions of a cent; nanodollar precision is far below anything a provider
/// bills, and stable rounding makes JSON round trips reproducible.
pub fn round_usd(value: f64) -> f64 {
    (value * 1_000_000_000.0).round() / 1_000_000_000.0
}

/// SHA-256 helper used for fixture, acceptance, and price fingerprints.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: u64, output: u64, read: u64, write: u64) -> TokenUsage {
        TokenUsage {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: read,
            cache_write_tokens: write,
            calls: 1,
        }
    }

    #[test]
    fn prices_disjoint_cache_portions_once() {
        let pricing = Pricing::new(10.0, 20.0, 1.0, 2.0);
        // 1,000 total input = 500 uncached + 400 read + 100 write.
        let usage = usage(1_000, 200, 400, 100);
        let expected = (500.0 / 1_000_000.0) * 10.0
            + (400.0 / 1_000_000.0) * 1.0
            + (100.0 / 1_000_000.0) * 2.0
            + (200.0 / 1_000_000.0) * 20.0;
        let actual = priced_cost(&usage, &pricing);
        assert!((actual - expected).abs() < 1e-15, "{actual} != {expected}");
    }

    #[test]
    fn cache_portions_are_clamped_before_pricing() {
        let pricing = Pricing::new(10.0, 20.0, 1.0, 2.0);
        // Inconsistent metadata: reads and writes exceed input. The ledger
        // gives reads priority and clamps writes to the remainder.
        let usage = usage(100, 0, 90, 90);
        let normalized = usage.normalized();
        assert_eq!(normalized.cache_read_tokens, 90);
        assert_eq!(normalized.cache_write_tokens, 10);
        let expected = (90.0 / 1_000_000.0) * 1.0 + (10.0 / 1_000_000.0) * 2.0;
        assert!((priced_cost(&usage, &pricing) - expected).abs() < 1e-15);
    }

    #[test]
    fn zero_prices_produce_zero_cost_not_none() {
        let pricing = Pricing::new(0.0, 0.0, 0.0, 0.0);
        assert_eq!(priced_cost(&usage(1_000, 500, 0, 0), &pricing), 0.0);
    }

    #[test]
    fn cache_ratio_is_none_for_zero_input() {
        assert_eq!(usage(0, 0, 0, 0).cache_hit_ratio(), None);
        assert_eq!(usage(0, 0, 10, 0).cache_hit_ratio(), None);
    }

    #[test]
    fn cache_ratio_uses_disjoint_buckets_and_clamps() {
        let ratio = usage(1_000, 0, 600, 100).cache_hit_ratio().unwrap();
        assert!((ratio - 0.6).abs() < 1e-12);
        let clamped = usage(100, 0, 200, 200).cache_hit_ratio().unwrap();
        assert_eq!(clamped, 1.0);
    }

    #[test]
    fn zero_solved_is_none_for_costs_and_tokens() {
        assert_eq!(cost_per_solved(1.25, 0), None);
        assert_eq!(tokens_per_solved(10_000, 0), None);
        assert_eq!(cost_per_solved(1.25, 5), Some(0.25));
        assert_eq!(tokens_per_solved(10_000, 5), Some(2_000.0));
    }

    #[test]
    fn zero_denominator_ratios_are_none() {
        assert_eq!(ratio_or_none(0, 0), None);
        assert_eq!(ratio_or_none(1, 0), None);
        assert_eq!(ratio_or_none(3, 4), Some(0.75));
    }

    #[test]
    fn percent_change_handles_zero_and_absent_values() {
        assert_eq!(percent_change(Some(0.0), Some(1.0)), None);
        assert_eq!(percent_change(None, Some(1.0)), None);
        assert_eq!(percent_change(Some(2.0), Some(3.0)), Some(50.0));
        assert_eq!(point_change(Some(0.25), Some(0.5)), Some(25.0));
    }

    #[test]
    fn price_validation_rejects_negative_and_non_finite_values() {
        assert!(Pricing::benchmark_default().validate().is_ok());
        assert!(Pricing::new(-1.0, 1.0, 0.0, 0.0).validate().is_err());
        assert!(Pricing::new(f64::NAN, 1.0, 0.0, 0.0).validate().is_err());
        assert!(
            Pricing::new(1.0, f64::INFINITY, 0.0, 0.0)
                .validate()
                .is_err()
        );
    }

    #[test]
    fn price_fingerprint_is_stable_and_discriminating() {
        let a = Pricing::benchmark_default();
        let b = Pricing::benchmark_default();
        assert_eq!(a.fingerprint(), b.fingerprint());
        assert_ne!(
            a.fingerprint(),
            Pricing::new(3.0, 15.0, 0.31, 3.75).fingerprint()
        );
    }

    #[test]
    fn pricing_provenance_records_partial_overrides_per_field() {
        let defaults = PricingProvenance::from_overrides(false, false, false, false);
        assert_eq!(defaults.source, PriceSource::IllustrativeDefault);
        assert_eq!(defaults.input, PriceSource::IllustrativeDefault);
        assert!(defaults.validate().is_ok());

        let partial = PricingProvenance::from_overrides(true, false, false, false);
        assert_eq!(partial.source, PriceSource::PartialCaller);
        assert_eq!(partial.input, PriceSource::Caller);
        assert_eq!(partial.output, PriceSource::IllustrativeDefault);
        assert!(partial.validate().is_ok());

        let caller = PricingProvenance::from_overrides(true, true, true, true);
        assert_eq!(caller.source, PriceSource::Caller);
        assert!(caller.all_caller());
        assert!(caller.validate().is_ok());

        let contradictory = PricingProvenance {
            source: PriceSource::Caller,
            ..partial
        };
        assert!(contradictory.validate().is_err());
        assert_ne!(defaults.fingerprint(), partial.fingerprint());
    }

    #[test]
    fn endpoint_redaction_keeps_the_host_and_path_but_not_credentials() {
        assert_eq!(
            redact_endpoint("https://user:secret@api.example.com/v1?token=abc#frag"),
            "https://api.example.com/v1"
        );
        assert_eq!(
            redact_endpoint("http://127.0.0.1:8080"),
            "http://127.0.0.1:8080"
        );
        assert_eq!(
            redact_endpoint("api.example.com/v1?q=1"),
            "api.example.com/v1"
        );
    }

    #[test]
    fn formatting_is_stable_for_reports() {
        assert_eq!(format_usd(0.123456789), "0.123457");
        assert_eq!(format_percent(Some(0.1234)), "12.3%");
        assert_eq!(format_percent(None), "n/a");
        assert_eq!(format_optional(Some(12.345), 1), "12.3");
        assert_eq!(format_optional(None, 2), "n/a");
    }

    #[test]
    fn sha256_matches_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}

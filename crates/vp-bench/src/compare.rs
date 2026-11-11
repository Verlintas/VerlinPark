//! A/B report comparison with explicit mismatch refusal.
//!
//! Cost-per-solved-task numbers are only comparable when the fixture, its
//! acceptance bytes, the provider mode, the model, and the pricing profile are
//! the same. The comparator refuses mismatches by default and never silently
//! compares apples to oranges.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::metrics::{format_optional, format_percent, format_usd, percent_change, point_change};
use crate::report::{BenchReport, BenchSummary, ScenarioAggregate};

#[derive(Debug, Clone, Copy, Default)]
pub struct CompareOptions {
    pub allow_mismatch: bool,
}

/// One incompatible field between the two reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComparisonMismatch {
    pub scenario_id: String,
    pub field: String,
    pub a: String,
    pub b: String,
}

/// Refusal returned when incompatible reports are compared without
/// `--allow-mismatch`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompareRefusal {
    pub mismatches: Vec<ComparisonMismatch>,
}

impl std::fmt::Display for CompareRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            formatter,
            "comparison refused: the reports differ in {} compatibility field(s)",
            self.mismatches.len()
        )?;
        for mismatch in &self.mismatches {
            writeln!(
                formatter,
                "  - {} / {}: {:?} != {:?}",
                mismatch.scenario_id, mismatch.field, mismatch.a, mismatch.b
            )?;
        }
        write!(
            formatter,
            "pass --allow-mismatch to compare anyway (the result will be labelled not comparable)"
        )
    }
}

impl std::error::Error for CompareRefusal {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricDelta {
    pub a: Option<f64>,
    pub b: Option<f64>,
    /// Relative percent change from A to B; absent when A is zero/absent.
    pub percent_change: Option<f64>,
    /// Absolute percentage-point change for ratios.
    pub point_change: Option<f64>,
}

impl MetricDelta {
    fn ratio(a: Option<f64>, b: Option<f64>) -> Self {
        Self {
            a,
            b,
            percent_change: percent_change(a, b),
            point_change: point_change(a, b),
        }
    }

    fn value(a: Option<f64>, b: Option<f64>) -> Self {
        Self {
            a,
            b,
            percent_change: percent_change(a, b),
            point_change: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScenarioComparison {
    pub scenario_id: String,
    pub title: String,
    pub comparable: bool,
    pub mismatches: Vec<ComparisonMismatch>,
    pub solved_rate: MetricDelta,
    pub cost_per_solved_task_usd: MetricDelta,
    pub tokens_per_solved_task: MetricDelta,
    pub cache_hit_ratio: MetricDelta,
    pub verifier_pass_rate: MetricDelta,
    pub merge_rate: MetricDelta,
    pub mean_wall_clock_ms: MetricDelta,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComparisonReport {
    pub schema_version: u32,
    pub generated_at: String,
    pub a_path: PathBuf,
    pub b_path: PathBuf,
    pub allow_mismatch: bool,
    pub comparable: bool,
    pub mismatches: Vec<ComparisonMismatch>,
    pub warnings: Vec<String>,
    pub a_summary: BenchSummary,
    pub b_summary: BenchSummary,
    pub scenarios: Vec<ScenarioComparison>,
}

impl ComparisonReport {
    pub fn to_json(&self) -> serde_json::Result<serde_json::Value> {
        serde_json::to_value(self)
    }

    pub fn to_markdown(&self) -> String {
        render_comparison(self)
    }

    pub fn a_all_solved(&self) -> bool {
        self.a_summary.all_solved
    }

    pub fn b_all_solved(&self) -> bool {
        self.b_summary.all_solved
    }
}

/// Compare two reports. Compatibility mismatches are returned as a typed
/// refusal unless `allow_mismatch` is set.
pub fn compare_reports(
    a: &BenchReport,
    b: &BenchReport,
    a_path: PathBuf,
    b_path: PathBuf,
    options: CompareOptions,
) -> Result<ComparisonReport, CompareRefusal> {
    let mut mismatches = Vec::new();
    let mut warnings = Vec::new();
    collect_report_mismatches(a, b, &mut mismatches, &mut warnings);
    if !mismatches.is_empty() && !options.allow_mismatch {
        return Err(CompareRefusal { mismatches });
    }
    let comparable = mismatches.is_empty();
    let mut scenarios = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for scenario in &a.scenarios {
        seen.insert(scenario.scenario_id.clone());
        let other = b.scenario(&scenario.scenario_id);
        let scenario_mismatches = mismatches
            .iter()
            .filter(|mismatch| mismatch.scenario_id == scenario.scenario_id)
            .cloned()
            .collect::<Vec<_>>();
        scenarios.push(compare_scenario(scenario, other, scenario_mismatches));
    }
    for scenario in &b.scenarios {
        if seen.insert(scenario.scenario_id.clone()) {
            let scenario_mismatches = mismatches
                .iter()
                .filter(|mismatch| mismatch.scenario_id == scenario.scenario_id)
                .cloned()
                .collect::<Vec<_>>();
            scenarios.push(compare_scenario(scenario, None, scenario_mismatches));
        }
    }
    if !comparable {
        for scenario in &mut scenarios {
            scenario.comparable = false;
        }
    }
    if a.selection.repetitions != b.selection.repetitions {
        warnings.push(format!(
            "repetition count differs: A={} B={}",
            a.selection.repetitions, b.selection.repetitions
        ));
    }
    if a.commit != b.commit {
        warnings.push(format!(
            "commit differs: A={} B={}",
            a.commit.as_deref().unwrap_or("unknown"),
            b.commit.as_deref().unwrap_or("unknown")
        ));
    }
    if a.host != b.host {
        warnings.push("host metadata differs; wall-clock comparisons are local only".to_string());
    }
    if a.pricing != b.pricing {
        warnings
            .push("pricing profiles differ; cost comparison is not billing-equivalent".to_string());
    }
    let now = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown".to_string());
    Ok(ComparisonReport {
        schema_version: crate::report::SCHEMA_VERSION,
        generated_at: now,
        a_path,
        b_path,
        allow_mismatch: options.allow_mismatch,
        comparable,
        mismatches,
        warnings,
        a_summary: a.summary.clone(),
        b_summary: b.summary.clone(),
        scenarios,
    })
}

fn collect_report_mismatches(
    a: &BenchReport,
    b: &BenchReport,
    mismatches: &mut Vec<ComparisonMismatch>,
    warnings: &mut Vec<String>,
) {
    if a.schema_version != b.schema_version {
        mismatches.push(ComparisonMismatch {
            scenario_id: "(report)".to_string(),
            field: "schema_version".to_string(),
            a: a.schema_version.to_string(),
            b: b.schema_version.to_string(),
        });
    }
    let a_ids = a
        .scenarios
        .iter()
        .map(|scenario| scenario.scenario_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let b_ids = b
        .scenarios
        .iter()
        .map(|scenario| scenario.scenario_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    for id in a_ids.symmetric_difference(&b_ids) {
        mismatches.push(ComparisonMismatch {
            scenario_id: (*id).to_string(),
            field: "scenario_presence".to_string(),
            a: if a_ids.contains(id) {
                "present"
            } else {
                "missing"
            }
            .to_string(),
            b: if b_ids.contains(id) {
                "present"
            } else {
                "missing"
            }
            .to_string(),
        });
    }
    if a.provider.mode != b.provider.mode {
        mismatches.push(ComparisonMismatch {
            scenario_id: "(report)".to_string(),
            field: "provider_mode".to_string(),
            a: a.provider.mode.clone(),
            b: b.provider.mode.clone(),
        });
    }
    if a.provider.model != b.provider.model {
        mismatches.push(ComparisonMismatch {
            scenario_id: "(report)".to_string(),
            field: "model".to_string(),
            a: a.provider.model.clone(),
            b: b.provider.model.clone(),
        });
    }
    if a.pricing != b.pricing {
        mismatches.push(ComparisonMismatch {
            scenario_id: "(report)".to_string(),
            field: "pricing".to_string(),
            a: format!(
                "{:.6}/{:.6}",
                a.pricing.input_usd_per_million, a.pricing.output_usd_per_million
            ),
            b: format!(
                "{:.6}/{:.6}",
                b.pricing.input_usd_per_million, b.pricing.output_usd_per_million
            ),
        });
    }
    for scenario in &a.scenarios {
        let Some(other) = b.scenario(&scenario.scenario_id) else {
            continue;
        };
        compare_keys(scenario, other, mismatches);
    }
    let a_unavailable = a
        .scenarios
        .iter()
        .filter(|scenario| scenario.availability != "available")
        .count();
    if a_unavailable > 0 {
        warnings.push(format!(
            "A contains {a_unavailable} unavailable scenario aggregate(s); check the catalog"
        ));
    }
}

fn compare_keys(
    a: &ScenarioAggregate,
    b: &ScenarioAggregate,
    mismatches: &mut Vec<ComparisonMismatch>,
) {
    for (field, a_value, b_value) in [
        (
            "fixture_kind",
            a.comparison.fixture_kind.clone(),
            b.comparison.fixture_kind.clone(),
        ),
        (
            "fixture_fingerprint",
            a.comparison.fixture_fingerprint.clone(),
            b.comparison.fixture_fingerprint.clone(),
        ),
        (
            "acceptance_fingerprint",
            a.comparison.acceptance_fingerprint.clone(),
            b.comparison.acceptance_fingerprint.clone(),
        ),
        (
            "provider_mode",
            a.comparison.provider_mode.clone(),
            b.comparison.provider_mode.clone(),
        ),
        (
            "model",
            a.comparison.model.clone(),
            b.comparison.model.clone(),
        ),
        (
            "pricing_fingerprint",
            a.comparison.pricing_fingerprint.clone(),
            b.comparison.pricing_fingerprint.clone(),
        ),
    ] {
        if a_value != b_value {
            mismatches.push(ComparisonMismatch {
                scenario_id: a.scenario_id.clone(),
                field: field.to_string(),
                a: a_value,
                b: b_value,
            });
        }
    }
}

fn compare_scenario(
    a: &ScenarioAggregate,
    b: Option<&ScenarioAggregate>,
    mismatches: Vec<ComparisonMismatch>,
) -> ScenarioComparison {
    let (b_solved, b_cost, b_tokens, b_cache, b_verifier, b_merge, b_wall) =
        b.map_or((None, None, None, None, None, None, None), |scenario| {
            (
                scenario.success_rate,
                scenario.cost_per_solved_task_usd,
                scenario.tokens_per_solved_task,
                scenario.cache_hit_ratio,
                scenario.verifier_pass_rate,
                scenario.merge_rate,
                Some(scenario.mean_wall_clock_ms),
            )
        });
    ScenarioComparison {
        scenario_id: a.scenario_id.clone(),
        title: a.title.clone(),
        comparable: mismatches.is_empty() && b.is_some(),
        mismatches,
        solved_rate: MetricDelta::ratio(a.success_rate, b_solved),
        cost_per_solved_task_usd: MetricDelta::value(a.cost_per_solved_task_usd, b_cost),
        tokens_per_solved_task: MetricDelta::value(a.tokens_per_solved_task, b_tokens),
        cache_hit_ratio: MetricDelta::ratio(a.cache_hit_ratio, b_cache),
        verifier_pass_rate: MetricDelta::ratio(a.verifier_pass_rate, b_verifier),
        merge_rate: MetricDelta::ratio(a.merge_rate, b_merge),
        mean_wall_clock_ms: MetricDelta::value(Some(a.mean_wall_clock_ms), b_wall),
    }
}

pub fn render_comparison(comparison: &ComparisonReport) -> String {
    let mut text = String::new();
    text.push_str("# VerlinPark benchmark A/B comparison\n\n");
    text.push_str(&format!("- A: `{}`\n", comparison.a_path.display()));
    text.push_str(&format!("- B: `{}`\n", comparison.b_path.display()));
    text.push_str(&format!(
        "- Comparable: {}{}\n",
        if comparison.comparable { "yes" } else { "no" },
        if comparison.allow_mismatch && !comparison.mismatches.is_empty() {
            " (mismatch explicitly allowed)"
        } else {
            ""
        }
    ));
    text.push_str(&format!(
        "- Solved runs: A {}/{} · B {}/{}\n\n",
        comparison.a_summary.solved_runs,
        comparison.a_summary.run_count,
        comparison.b_summary.solved_runs,
        comparison.b_summary.run_count
    ));
    if !comparison.mismatches.is_empty() {
        text.push_str("## Compatibility mismatches\n\n");
        for mismatch in &comparison.mismatches {
            text.push_str(&format!(
                "- `{}` / `{}`: `{}` vs `{}`\n",
                mismatch.scenario_id, mismatch.field, mismatch.a, mismatch.b
            ));
        }
        text.push('\n');
    }
    if !comparison.warnings.is_empty() {
        text.push_str("## Warnings\n\n");
        for warning in &comparison.warnings {
            text.push_str(&format!("- {warning}\n"));
        }
        text.push('\n');
    }
    text.push_str(
        "| Scenario | Solved A→B | Cost/solved A→B | Δ cost | Tokens/solved A→B | Δ tokens | Cache A→B | Wall A→B |\n",
    );
    text.push_str("|---|---:|---:|---:|---:|---:|---:|---:|\n");
    for scenario in &comparison.scenarios {
        text.push_str(&format!(
            "| {} | {} → {} | {} → {} | {} | {} → {} | {} | {} → {} | {} → {} ms |\n",
            scenario.scenario_id,
            format_percent(scenario.solved_rate.a),
            format_percent(scenario.solved_rate.b),
            format_optional(scenario.cost_per_solved_task_usd.a, 6),
            format_optional(scenario.cost_per_solved_task_usd.b, 6),
            format_delta(scenario.cost_per_solved_task_usd.percent_change),
            format_optional(scenario.tokens_per_solved_task.a, 1),
            format_optional(scenario.tokens_per_solved_task.b, 1),
            format_delta(scenario.tokens_per_solved_task.percent_change),
            format_percent(scenario.cache_hit_ratio.a),
            format_percent(scenario.cache_hit_ratio.b),
            format_optional(scenario.mean_wall_clock_ms.a, 1),
            format_optional(scenario.mean_wall_clock_ms.b, 1)
        ));
    }
    text.push('\n');
    text.push_str(&format!(
        "Totals: A cost {} USD / B cost {} USD. Prices are caller-supplied. This comparison is not a universal ranking.\n",
        format_usd(comparison.a_summary.total_cost_usd),
        format_usd(comparison.b_summary.total_cost_usd)
    ));
    text
}

fn format_delta(value: Option<f64>) -> String {
    match value {
        Some(value) => format!("{value:+.1}%"),
        None => "n/a".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::Pricing;
    use crate::report::{
        BenchSelection, ComparisonKey, HostMetadata, ProviderMetadata, ScenarioAggregate,
    };
    use std::collections::BTreeMap;

    fn key(model: &str, acceptance: &str) -> ComparisonKey {
        ComparisonKey {
            fixture_kind: "single-session".into(),
            fixture_fingerprint: "fixture".into(),
            acceptance_fingerprint: acceptance.into(),
            provider_mode: "fake".into(),
            model: model.into(),
            pricing_fingerprint: Pricing::benchmark_default().fingerprint(),
        }
    }

    fn aggregate(solved: usize, runs: usize, cost: Option<f64>) -> ScenarioAggregate {
        ScenarioAggregate {
            scenario_id: "baseline-single-session".into(),
            title: "Baseline".into(),
            fixture_kind: "single-session".into(),
            availability: "available".into(),
            repetitions: runs,
            solved_runs: solved,
            unsolved_runs: runs - solved,
            success_rate: Some(solved as f64 / runs as f64),
            total_cost_usd: cost.unwrap_or(0.0) * solved as f64,
            cost_per_solved_task_usd: cost,
            total_tokens: 1_000 * solved as u64,
            total_input_tokens: 900 * solved as u64,
            total_output_tokens: 100 * solved as u64,
            total_cache_read_tokens: 100 * solved as u64,
            total_cache_write_tokens: 10 * solved as u64,
            tokens_per_solved_task: if solved > 0 { Some(1_000.0) } else { None },
            cache_hit_ratio: Some(0.10),
            verifier_pass_rate: Some(1.0),
            merge_rate: None,
            mean_wall_clock_ms: 100.0,
            median_wall_clock_ms: 100.0,
            max_peak_workers: 1,
            total_compactions: 0,
            total_scouts: 0,
            session_ids: vec!["s".into()],
            comparison: key("bench", "acceptance"),
            unsolved_reason_counts: BTreeMap::new(),
        }
    }

    fn summary(solved: usize, runs: usize, cost: Option<f64>) -> BenchSummary {
        BenchSummary {
            scenario_count: 1,
            run_count: runs,
            solved_runs: solved,
            unsolved_runs: runs - solved,
            harness_faults: 0,
            all_solved: solved == runs,
            total_cost_usd: cost.unwrap_or(0.0) * solved as f64,
            cost_per_solved_task_usd: cost,
            total_tokens: 1_000 * solved as u64,
            tokens_per_solved_task: if solved > 0 { Some(1_000.0) } else { None },
            cache_hit_ratio: Some(0.1),
            verifier_pass_rate: Some(1.0),
            merge_rate: None,
        }
    }

    fn report(scenario: ScenarioAggregate, summary: BenchSummary, model: &str) -> BenchReport {
        BenchReport {
            schema_version: 1,
            generated_at: "2026-11-11T00:00:00Z".into(),
            commit: Some("abc".into()),
            commit_dirty: Some(false),
            caveat: crate::report::REPORT_CAVEAT.into(),
            host: HostMetadata {
                os: "macos".into(),
                arch: "aarch64".into(),
                hostname: Some("host".into()),
                logical_cpus: 8,
            },
            provider: ProviderMetadata::fake("fake", model, "simulated", 0),
            pricing: Pricing::benchmark_default(),
            selection: BenchSelection {
                scenario_ids: vec![scenario.scenario_id.clone()],
                repetitions: scenario.repetitions,
            },
            summary,
            scenarios: vec![scenario],
            runs: vec![],
        }
    }

    #[test]
    fn matching_reports_compare_and_show_deltas() {
        let a = report(
            aggregate(1, 1, Some(1.0)),
            summary(1, 1, Some(1.0)),
            "bench",
        );
        let b = report(
            aggregate(2, 2, Some(0.5)),
            summary(2, 2, Some(0.5)),
            "bench",
        );
        let comparison = compare_reports(
            &a,
            &b,
            "a.json".into(),
            "b.json".into(),
            CompareOptions::default(),
        )
        .unwrap();
        assert!(comparison.comparable);
        let scenario = &comparison.scenarios[0];
        assert_eq!(
            scenario.cost_per_solved_task_usd.percent_change,
            Some(-50.0)
        );
        assert!(comparison.to_markdown().contains("A/B comparison"));
    }

    #[test]
    fn mismatched_model_is_refused_by_default() {
        let a = report(
            aggregate(1, 1, Some(1.0)),
            summary(1, 1, Some(1.0)),
            "model-a",
        );
        let b = report(
            aggregate(1, 1, Some(1.0)),
            summary(1, 1, Some(1.0)),
            "model-b",
        );
        let refusal = compare_reports(
            &a,
            &b,
            "a.json".into(),
            "b.json".into(),
            CompareOptions::default(),
        )
        .unwrap_err();
        assert!(
            refusal
                .mismatches
                .iter()
                .any(|mismatch| mismatch.field == "model")
        );
        assert!(refusal.to_string().contains("--allow-mismatch"));
    }

    #[test]
    fn mismatched_acceptance_is_refused_but_can_be_allowed() {
        let a = report(
            aggregate(1, 1, Some(1.0)),
            summary(1, 1, Some(1.0)),
            "bench",
        );
        let mut b_scenario = aggregate(1, 1, Some(1.0));
        b_scenario.comparison.acceptance_fingerprint = "other".into();
        let b = report(b_scenario, summary(1, 1, Some(1.0)), "bench");
        assert!(
            compare_reports(&a, &b, "a".into(), "b".into(), CompareOptions::default()).is_err()
        );
        let comparison = compare_reports(
            &a,
            &b,
            "a".into(),
            "b".into(),
            CompareOptions {
                allow_mismatch: true,
            },
        )
        .unwrap();
        assert!(!comparison.comparable);
        assert!(!comparison.scenarios[0].comparable);
        assert!(
            comparison
                .to_markdown()
                .contains("mismatch explicitly allowed")
        );
    }

    #[test]
    fn missing_scenario_is_a_mismatch() {
        let a = report(
            aggregate(1, 1, Some(1.0)),
            summary(1, 1, Some(1.0)),
            "bench",
        );
        let mut b = a.clone();
        b.scenarios[0].scenario_id = "other".into();
        b.selection.scenario_ids = vec!["other".into()];
        let refusal =
            compare_reports(&a, &b, "a".into(), "b".into(), CompareOptions::default()).unwrap_err();
        assert!(
            refusal
                .mismatches
                .iter()
                .any(|mismatch| mismatch.field == "scenario_presence")
        );
    }

    #[test]
    fn zero_solved_keeps_cost_delta_absent() {
        let a = report(aggregate(0, 1, None), summary(0, 1, None), "bench");
        let b = report(
            aggregate(1, 1, Some(1.0)),
            summary(1, 1, Some(1.0)),
            "bench",
        );
        let comparison =
            compare_reports(&a, &b, "a".into(), "b".into(), CompareOptions::default()).unwrap();
        assert_eq!(
            comparison.scenarios[0]
                .cost_per_solved_task_usd
                .percent_change,
            None
        );
        assert_eq!(comparison.scenarios[0].cost_per_solved_task_usd.a, None);
    }

    #[test]
    fn warnings_report_commit_and_host_differences() {
        let a = report(
            aggregate(1, 1, Some(1.0)),
            summary(1, 1, Some(1.0)),
            "bench",
        );
        let mut b = a.clone();
        b.commit = Some("def".into());
        b.host.logical_cpus = 16;
        let comparison =
            compare_reports(&a, &b, "a".into(), "b".into(), CompareOptions::default()).unwrap();
        assert!(
            comparison
                .warnings
                .iter()
                .any(|warning| warning.contains("commit"))
        );
        assert!(
            comparison
                .warnings
                .iter()
                .any(|warning| warning.contains("host"))
        );
    }
}

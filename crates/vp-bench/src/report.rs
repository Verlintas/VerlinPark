//! Machine, Markdown, and CSV reports.
//!
//! A report is the durable output of one `vp bench run`. JSON is the machine
//! source of truth; Markdown is a human summary; CSV is one row per run. All
//! three are written atomically with a bounded size.
//!
//! The report explicitly records the caveat that one host, one model, one
//! pricing profile, and one fixture set are not a universal ranking.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::fixture::AcceptanceExecution;
use crate::metrics::{
    Pricing, TokenUsage, cost_per_solved, format_optional, format_percent, format_usd,
    percent_change, priced_cost, ratio_or_none, round_usd, tokens_per_solved,
};
use crate::scenario::Scenario;

pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_REPORT_BYTES: usize = 8 * 1024 * 1024;
pub const REPORT_CAVEAT: &str = "These results are local measurements for the recorded \
fixture, provider mode, model, pricing profile, host, and commit. Fake-provider runs measure \
harness/runtime mechanics, not model quality. Caller-supplied prices may not match provider \
billing. One host and one model are not a universal ranking, and this report does not claim \
that any configuration is generally faster, cheaper, or better.";

/// Host and build metadata recorded once per report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostMetadata {
    pub os: String,
    pub arch: String,
    pub hostname: Option<String>,
    pub logical_cpus: usize,
}

impl HostMetadata {
    pub fn detect() -> Self {
        Self {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            hostname: Command::new("hostname")
                .output()
                .ok()
                .and_then(|output| {
                    output
                        .status
                        .success()
                        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
                })
                .filter(|hostname| !hostname.is_empty()),
            logical_cpus: std::thread::available_parallelism()
                .map(std::num::NonZeroUsize::get)
                .unwrap_or(0),
        }
    }
}

/// Detect the current commit and worktree dirtiness for report metadata.
pub fn detect_commit() -> (Option<String>, Option<bool>) {
    let commit = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|commit| !commit.is_empty());
    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| !output.stdout.is_empty());
    (commit, dirty)
}

/// Provider identity and accounting source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderMetadata {
    /// `fake` or `real`.
    pub mode: String,
    pub id: String,
    pub model: String,
    /// `fixed`, `simulated`, or `provider`.
    pub usage_source: String,
    pub simulated_latency_ms: u64,
}

impl ProviderMetadata {
    pub fn fake(
        id: impl Into<String>,
        model: impl Into<String>,
        usage_source: impl Into<String>,
        simulated_latency_ms: u64,
    ) -> Self {
        Self {
            mode: "fake".to_string(),
            id: id.into(),
            model: model.into(),
            usage_source: usage_source.into(),
            simulated_latency_ms,
        }
    }

    pub fn real(id: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            mode: "real".to_string(),
            id: id.into(),
            model: model.into(),
            usage_source: "provider".to_string(),
            simulated_latency_ms: 0,
        }
    }
}

/// Run-level identity used to refuse an apples-to-oranges A/B comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComparisonKey {
    pub fixture_kind: String,
    pub fixture_fingerprint: String,
    pub acceptance_fingerprint: String,
    pub provider_mode: String,
    pub model: String,
    pub pricing_fingerprint: String,
}

/// A structured reason a run is unsolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnsolvedReason {
    pub code: String,
    pub detail: String,
}

impl UnsolvedReason {
    pub fn new(code: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            detail: detail.into(),
        }
    }
}

/// Outcome of the harness-owned acceptance script.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceResult {
    pub passed: bool,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub duration_ms: u64,
    pub stdout_preview: String,
    pub stderr_preview: String,
    pub fingerprint: String,
}

impl AcceptanceResult {
    pub fn from_execution(execution: AcceptanceExecution, fingerprint: impl Into<String>) -> Self {
        Self {
            passed: execution.passed,
            exit_code: execution.exit_code,
            timed_out: execution.timed_out,
            duration_ms: execution.duration_ms,
            stdout_preview: execution.stdout,
            stderr_preview: execution.stderr,
            fingerprint: fingerprint.into(),
        }
    }
}

/// One Verifier verdict, retained for audit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerdictRecord {
    pub task_id: Option<String>,
    pub approved: bool,
    pub summary: String,
}

/// Verifier gate telemetry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerifierMetrics {
    pub checked: u64,
    pub approved: u64,
    pub rejected: u64,
    /// `approved`, `rejected`, `incomplete`, or absent when no verdict was
    /// required.
    pub verdict: Option<String>,
    pub pass_rate: Option<f64>,
    pub verdicts: Vec<VerdictRecord>,
}

impl VerifierMetrics {
    pub fn from_verdicts(verdicts: Vec<VerdictRecord>) -> Self {
        let checked = verdicts.len() as u64;
        let approved = verdicts.iter().filter(|verdict| verdict.approved).count() as u64;
        let rejected = checked.saturating_sub(approved);
        let verdict = if checked == 0 {
            None
        } else if rejected == 0 {
            Some("approved".to_string())
        } else if approved == 0 {
            Some("rejected".to_string())
        } else {
            Some("mixed".to_string())
        };
        Self {
            checked,
            approved,
            rejected,
            verdict,
            pass_rate: ratio_or_none(approved, checked),
            verdicts,
        }
    }
}

/// Merge queue telemetry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeMetrics {
    pub entries: u64,
    pub merged: u64,
    pub failed: u64,
    pub conflicts: u64,
    pub expected: Option<usize>,
    pub success: Option<bool>,
    pub rate: Option<f64>,
}

impl MergeMetrics {
    pub fn new(
        entries: u64,
        merged: u64,
        failed: u64,
        conflicts: u64,
        expected: Option<usize>,
    ) -> Self {
        let success = if entries == 0 {
            None
        } else {
            Some(merged == entries && failed == 0 && conflicts == 0)
        };
        Self {
            entries,
            merged,
            failed,
            conflicts,
            expected,
            success,
            rate: ratio_or_none(merged, entries),
        }
    }
}

/// Per-run counter/token metrics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunMetrics {
    pub wall_clock_ms: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub total_tokens: u64,
    pub provider_calls: u64,
    pub usage_calls: u64,
    pub estimated_usage: bool,
    pub cost_usd: f64,
    pub ledger_cost_usd: f64,
    pub cost_reconciled: bool,
    pub cost_per_solved_task_usd: Option<f64>,
    pub tokens_per_solved_task: Option<f64>,
    pub cache_hit_ratio: Option<f64>,
    pub tool_calls: u64,
    pub tool_errors: u64,
    pub tool_output_bytes: u64,
    /// `None` when this build has no honest spill/paging measurement.
    pub tool_output_spills: Option<u64>,
    pub tool_output_paging_supported: bool,
    pub compactions: u64,
    pub compaction_failures: u64,
    pub retries: u64,
    pub human_interventions: u64,
    pub scouts: u64,
    pub peak_workers: u64,
    pub agent_failures: u64,
    pub plan_tasks_total: u64,
    pub plan_tasks_completed: u64,
    pub plan_tasks_failed: u64,
    pub plan_tasks_running: u64,
}

impl RunMetrics {
    pub fn token_usage(&self) -> TokenUsage {
        TokenUsage {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_read_tokens: self.cache_read_tokens,
            cache_write_tokens: self.cache_write_tokens,
            calls: self.usage_calls,
        }
    }

    pub fn recompute_cost(&mut self, pricing: &Pricing, solved: bool) {
        self.cost_usd = round_usd(priced_cost(&self.token_usage(), pricing));
        self.ledger_cost_usd = round_usd(self.ledger_cost_usd);
        self.cost_reconciled =
            (self.cost_usd - self.ledger_cost_usd).abs() <= (self.cost_usd.abs().max(1.0) * 1e-9);
        self.cost_per_solved_task_usd = if solved { Some(self.cost_usd) } else { None };
        self.tokens_per_solved_task =
            tokens_per_solved(self.total_tokens, if solved { 1 } else { 0 });
        self.cache_hit_ratio = self.token_usage().cache_hit_ratio();
    }
}

/// One benchmark run, corresponding to one repetition of one scenario.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: String,
    pub scenario_id: String,
    pub scenario_title: String,
    pub repetition: usize,
    pub started_at: String,
    pub finished_at: String,
    pub session_id: String,
    pub session_status: String,
    pub timed_out: bool,
    pub solved: bool,
    pub acceptance: AcceptanceResult,
    pub verifier: VerifierMetrics,
    pub merge: MergeMetrics,
    pub metrics: RunMetrics,
    pub unsolved_reasons: Vec<UnsolvedReason>,
    /// Set only for harness failures that make the result invalid rather than
    /// merely unsolved (for example an exhausted deterministic script).
    pub harness_fault: Option<String>,
    pub comparison: ComparisonKey,
    pub pricing: Pricing,
    pub provider: ProviderMetadata,
    pub target_paths: Vec<String>,
    /// Populated only when `--keep-worktrees` retained the disposable fixture.
    #[serde(default)]
    pub retained_workspace: Option<String>,
}

impl RunRecord {
    pub fn solved_task(&self) -> u64 {
        u64::from(self.solved)
    }

    pub fn unsolved_codes(&self) -> Vec<&str> {
        self.unsolved_reasons
            .iter()
            .map(|reason| reason.code.as_str())
            .collect()
    }
}

/// Aggregate for one scenario across its repetitions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScenarioAggregate {
    pub scenario_id: String,
    pub title: String,
    pub fixture_kind: String,
    pub availability: String,
    pub repetitions: usize,
    pub solved_runs: usize,
    pub unsolved_runs: usize,
    pub success_rate: Option<f64>,
    pub total_cost_usd: f64,
    pub cost_per_solved_task_usd: Option<f64>,
    pub total_tokens: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cache_read_tokens: u64,
    pub total_cache_write_tokens: u64,
    pub tokens_per_solved_task: Option<f64>,
    pub cache_hit_ratio: Option<f64>,
    pub verifier_pass_rate: Option<f64>,
    pub merge_rate: Option<f64>,
    pub mean_wall_clock_ms: f64,
    pub median_wall_clock_ms: f64,
    pub max_peak_workers: u64,
    pub total_compactions: u64,
    pub total_scouts: u64,
    pub session_ids: Vec<String>,
    pub comparison: ComparisonKey,
    pub unsolved_reason_counts: BTreeMap<String, u64>,
}

/// Cross-scenario summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenchSummary {
    pub scenario_count: usize,
    pub run_count: usize,
    pub solved_runs: usize,
    pub unsolved_runs: usize,
    pub harness_faults: usize,
    pub all_solved: bool,
    pub total_cost_usd: f64,
    pub cost_per_solved_task_usd: Option<f64>,
    pub total_tokens: u64,
    pub tokens_per_solved_task: Option<f64>,
    pub cache_hit_ratio: Option<f64>,
    pub verifier_pass_rate: Option<f64>,
    pub merge_rate: Option<f64>,
}

/// Selection metadata for the run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchSelection {
    pub scenario_ids: Vec<String>,
    pub repetitions: usize,
}

/// The complete durable report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenchReport {
    pub schema_version: u32,
    pub generated_at: String,
    pub commit: Option<String>,
    pub commit_dirty: Option<bool>,
    pub caveat: String,
    pub host: HostMetadata,
    pub provider: ProviderMetadata,
    pub pricing: Pricing,
    pub selection: BenchSelection,
    pub summary: BenchSummary,
    pub scenarios: Vec<ScenarioAggregate>,
    pub runs: Vec<RunRecord>,
}

impl BenchReport {
    /// Build a report from completed runs. Every selected scenario must have at
    /// least one run, and repetitions must be consistent.
    pub fn build(
        runs: Vec<RunRecord>,
        selected: &[Scenario],
        repetitions: usize,
        generated_at: impl Into<String>,
        commit: Option<String>,
        commit_dirty: Option<bool>,
        host: HostMetadata,
    ) -> Result<Self> {
        if runs.is_empty() {
            bail!("cannot build a benchmark report with no runs");
        }
        if repetitions == 0 || repetitions > 100 {
            bail!("report repetitions must be 1-100");
        }
        if selected.is_empty() {
            bail!("report selection cannot be empty");
        }
        let mut provider = runs[0].provider.clone();
        let pricing = runs[0].pricing;
        for run in &runs {
            if run.provider.mode != provider.mode
                || run.provider.id != provider.id
                || run.provider.model != provider.model
                || run.provider.usage_source != provider.usage_source
            {
                bail!("all runs in one report must use the same provider identity");
            }
            if run.pricing != pricing {
                bail!("all runs in one report must use the same pricing profile");
            }
            provider.simulated_latency_ms = provider
                .simulated_latency_ms
                .max(run.provider.simulated_latency_ms);
        }
        let mut by_scenario: BTreeMap<&str, Vec<&RunRecord>> = BTreeMap::new();
        for run in &runs {
            by_scenario
                .entry(run.scenario_id.as_str())
                .or_default()
                .push(run);
        }
        let selected_ids = selected
            .iter()
            .map(|scenario| scenario.id.as_str())
            .collect::<HashSet<_>>();
        if runs
            .iter()
            .any(|run| !selected_ids.contains(run.scenario_id.as_str()))
        {
            bail!("report runs include a scenario that was not selected");
        }
        let mut scenarios = Vec::with_capacity(selected.len());
        for scenario in selected {
            let scenario_runs = by_scenario
                .get(scenario.id.as_str())
                .with_context(|| format!("selected scenario {} has no runs", scenario.id))?;
            if scenario_runs.len() != repetitions {
                bail!(
                    "scenario {} has {} runs, expected {repetitions}",
                    scenario.id,
                    scenario_runs.len()
                );
            }
            scenarios.push(aggregate_scenario(scenario, scenario_runs)?);
        }
        let summary = summarize(&runs);
        let selection = BenchSelection {
            scenario_ids: selected
                .iter()
                .map(|scenario| scenario.id.clone())
                .collect(),
            repetitions,
        };
        let report = Self {
            schema_version: SCHEMA_VERSION,
            generated_at: generated_at.into(),
            commit,
            commit_dirty,
            caveat: REPORT_CAVEAT.to_string(),
            host,
            provider,
            pricing,
            selection,
            summary,
            scenarios,
            runs,
        };
        report.validate()?;
        Ok(report)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != SCHEMA_VERSION {
            bail!(
                "unsupported report schema version {}; expected {SCHEMA_VERSION}",
                self.schema_version
            );
        }
        if self.caveat.trim().is_empty() {
            bail!("report caveat cannot be empty");
        }
        if self.selection.scenario_ids.is_empty() {
            bail!("report selection cannot be empty");
        }
        if self.selection.repetitions == 0 || self.selection.repetitions > 100 {
            bail!("report repetitions must be 1-100");
        }
        if self.runs.len() > 1000 {
            bail!("report contains too many runs");
        }
        if self.summary.run_count != self.runs.len() {
            bail!("report summary run count does not match the runs array");
        }
        if self.scenarios.len() != self.selection.scenario_ids.len() {
            bail!("report scenario aggregates do not match the selection");
        }
        if self.pricing.validate().is_err() {
            bail!("report contains an invalid pricing profile");
        }
        Ok(())
    }

    pub fn all_solved(&self) -> bool {
        self.summary.all_solved
    }

    pub fn unsolved_runs(&self) -> usize {
        self.summary.unsolved_runs
    }

    pub fn harness_faults(&self) -> usize {
        self.summary.harness_faults
    }

    pub fn scenario(&self, id: &str) -> Option<&ScenarioAggregate> {
        self.scenarios
            .iter()
            .find(|scenario| scenario.scenario_id == id)
    }

    pub fn to_json_bytes(&self) -> Result<Vec<u8>> {
        let bytes = serde_json::to_vec_pretty(self).context("serializing benchmark report")?;
        if bytes.len() > MAX_REPORT_BYTES {
            bail!(
                "JSON report is {} bytes, above the {} byte limit",
                bytes.len(),
                MAX_REPORT_BYTES
            );
        }
        Ok(bytes)
    }

    pub fn to_markdown(&self) -> String {
        render_markdown(self)
    }

    pub fn to_csv(&self) -> String {
        render_csv(self)
    }
}

fn aggregate_scenario(scenario: &Scenario, runs: &[&RunRecord]) -> Result<ScenarioAggregate> {
    let comparison = runs[0].comparison.clone();
    if runs.iter().any(|run| run.comparison != comparison) {
        bail!(
            "scenario {} has inconsistent comparison metadata across repetitions",
            scenario.id
        );
    }
    let solved_runs = runs.iter().filter(|run| run.solved).count();
    let total_cost_usd = round_usd(runs.iter().map(|run| run.metrics.cost_usd).sum::<f64>());
    let total_input_tokens = runs.iter().map(|run| run.metrics.input_tokens).sum::<u64>();
    let total_output_tokens = runs
        .iter()
        .map(|run| run.metrics.output_tokens)
        .sum::<u64>();
    let total_cache_read_tokens = runs
        .iter()
        .map(|run| run.metrics.cache_read_tokens)
        .sum::<u64>();
    let total_cache_write_tokens = runs
        .iter()
        .map(|run| run.metrics.cache_write_tokens)
        .sum::<u64>();
    let total_tokens = total_input_tokens.saturating_add(total_output_tokens);
    let usage = TokenUsage {
        input_tokens: total_input_tokens,
        output_tokens: total_output_tokens,
        cache_read_tokens: total_cache_read_tokens,
        cache_write_tokens: total_cache_write_tokens,
        calls: runs.iter().map(|run| run.metrics.usage_calls).sum(),
    };
    let verifier_checked = runs.iter().map(|run| run.verifier.checked).sum::<u64>();
    let verifier_approved = runs.iter().map(|run| run.verifier.approved).sum::<u64>();
    let merge_entries = runs.iter().map(|run| run.merge.entries).sum::<u64>();
    let merge_merged = runs.iter().map(|run| run.merge.merged).sum::<u64>();
    let mut walls = runs
        .iter()
        .map(|run| run.metrics.wall_clock_ms)
        .collect::<Vec<_>>();
    walls.sort_unstable();
    let mean_wall_clock_ms = walls.iter().sum::<u64>() as f64 / walls.len() as f64;
    let median_wall_clock_ms = if walls.is_empty() {
        0.0
    } else {
        let middle = walls.len() / 2;
        if walls.len() % 2 == 0 {
            (walls[middle - 1] as f64 + walls[middle] as f64) / 2.0
        } else {
            walls[middle] as f64
        }
    };
    let mut unsolved_reason_counts = BTreeMap::new();
    for run in runs {
        for reason in &run.unsolved_reasons {
            *unsolved_reason_counts
                .entry(reason.code.clone())
                .or_insert(0) += 1;
        }
    }
    Ok(ScenarioAggregate {
        scenario_id: scenario.id.clone(),
        title: scenario.title.clone(),
        fixture_kind: scenario.fixture.as_str().to_string(),
        availability: match &scenario.availability {
            crate::scenario::Availability::Available => "available".to_string(),
            crate::scenario::Availability::Unavailable { .. } => "unavailable".to_string(),
        },
        repetitions: runs.len(),
        solved_runs,
        unsolved_runs: runs.len().saturating_sub(solved_runs),
        success_rate: ratio_or_none(solved_runs as u64, runs.len() as u64),
        total_cost_usd,
        cost_per_solved_task_usd: cost_per_solved(total_cost_usd, solved_runs as u64)
            .map(round_usd),
        total_tokens,
        total_input_tokens,
        total_output_tokens,
        total_cache_read_tokens,
        total_cache_write_tokens,
        tokens_per_solved_task: tokens_per_solved(total_tokens, solved_runs as u64),
        cache_hit_ratio: usage.cache_hit_ratio(),
        verifier_pass_rate: ratio_or_none(verifier_approved, verifier_checked),
        merge_rate: ratio_or_none(merge_merged, merge_entries),
        mean_wall_clock_ms,
        median_wall_clock_ms,
        max_peak_workers: runs
            .iter()
            .map(|run| run.metrics.peak_workers)
            .max()
            .unwrap_or(0),
        total_compactions: runs.iter().map(|run| run.metrics.compactions).sum(),
        total_scouts: runs.iter().map(|run| run.metrics.scouts).sum(),
        session_ids: runs.iter().map(|run| run.session_id.clone()).collect(),
        comparison,
        unsolved_reason_counts,
    })
}

fn summarize(runs: &[RunRecord]) -> BenchSummary {
    let solved_runs = runs.iter().filter(|run| run.solved).count();
    let total_cost_usd = round_usd(runs.iter().map(|run| run.metrics.cost_usd).sum::<f64>());
    let total_tokens = runs.iter().map(|run| run.metrics.total_tokens).sum::<u64>();
    let usage = TokenUsage {
        input_tokens: runs.iter().map(|run| run.metrics.input_tokens).sum(),
        output_tokens: runs.iter().map(|run| run.metrics.output_tokens).sum(),
        cache_read_tokens: runs.iter().map(|run| run.metrics.cache_read_tokens).sum(),
        cache_write_tokens: runs.iter().map(|run| run.metrics.cache_write_tokens).sum(),
        calls: runs.iter().map(|run| run.metrics.usage_calls).sum(),
    };
    let verifier_checked = runs.iter().map(|run| run.verifier.checked).sum::<u64>();
    let verifier_approved = runs.iter().map(|run| run.verifier.approved).sum::<u64>();
    let merge_entries = runs.iter().map(|run| run.merge.entries).sum::<u64>();
    let merge_merged = runs.iter().map(|run| run.merge.merged).sum::<u64>();
    BenchSummary {
        scenario_count: runs
            .iter()
            .map(|run| run.scenario_id.as_str())
            .collect::<HashSet<_>>()
            .len(),
        run_count: runs.len(),
        solved_runs,
        unsolved_runs: runs.len().saturating_sub(solved_runs),
        harness_faults: runs
            .iter()
            .filter(|run| run.harness_fault.is_some())
            .count(),
        all_solved: solved_runs == runs.len(),
        total_cost_usd,
        cost_per_solved_task_usd: cost_per_solved(total_cost_usd, solved_runs as u64)
            .map(round_usd),
        total_tokens,
        tokens_per_solved_task: tokens_per_solved(total_tokens, solved_runs as u64),
        cache_hit_ratio: usage.cache_hit_ratio(),
        verifier_pass_rate: ratio_or_none(verifier_approved, verifier_checked),
        merge_rate: ratio_or_none(merge_merged, merge_entries),
    }
}

/// Write all three report formats with one stable stem.
pub fn write_artifacts(report: &BenchReport, output_dir: &Path) -> Result<ReportArtifacts> {
    fs::create_dir_all(output_dir).with_context(|| {
        format!(
            "creating benchmark output directory {}",
            output_dir.display()
        )
    })?;
    let now = OffsetDateTime::now_utc();
    let id = Uuid::now_v7().simple().to_string();
    let stem = format!("bench-{}-{}", now.unix_timestamp(), &id[..8]);
    write_artifacts_with_stem(report, output_dir, &stem)
}

/// Testable form of [`write_artifacts`].
pub fn write_artifacts_with_stem(
    report: &BenchReport,
    output_dir: &Path,
    stem: &str,
) -> Result<ReportArtifacts> {
    if stem.is_empty()
        || stem.len() > 120
        || !stem
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        bail!("report stem must be 1-120 safe ASCII characters");
    }
    report.validate()?;
    fs::create_dir_all(output_dir).with_context(|| {
        format!(
            "creating benchmark output directory {}",
            output_dir.display()
        )
    })?;
    let json_path = output_dir.join(format!("{stem}.json"));
    let markdown_path = output_dir.join(format!("{stem}.md"));
    let csv_path = output_dir.join(format!("{stem}.csv"));
    let json = report.to_json_bytes()?;
    let markdown = report.to_markdown();
    let csv = report.to_csv();
    write_atomic_bounded(&json_path, &json)?;
    write_atomic_bounded(&markdown_path, markdown.as_bytes())?;
    write_atomic_bounded(&csv_path, csv.as_bytes())?;
    Ok(ReportArtifacts {
        stem: stem.to_string(),
        json_path,
        markdown_path,
        csv_path,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportArtifacts {
    pub stem: String,
    pub json_path: PathBuf,
    pub markdown_path: PathBuf,
    pub csv_path: PathBuf,
}

impl ReportArtifacts {
    pub fn summary_line(&self) -> String {
        format!("report: {}", self.json_path.display())
    }
}

/// Load a JSON report with a size bound and shape validation.
pub fn load_report(path: &Path) -> Result<BenchReport> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("reading benchmark report metadata {}", path.display()))?;
    if metadata.len() > MAX_REPORT_BYTES as u64 {
        bail!(
            "report {} is {} bytes, above the {} byte limit",
            path.display(),
            metadata.len(),
            MAX_REPORT_BYTES
        );
    }
    let bytes =
        fs::read(path).with_context(|| format!("reading benchmark report {}", path.display()))?;
    parse_report(&bytes)
}

/// Parse a report from bytes. Kept public for tests and CI tools.
pub fn parse_report(bytes: &[u8]) -> Result<BenchReport> {
    if bytes.len() > MAX_REPORT_BYTES {
        bail!(
            "report is {} bytes, above the {} byte limit",
            bytes.len(),
            MAX_REPORT_BYTES
        );
    }
    let report: BenchReport = serde_json::from_slice(bytes).context("parsing benchmark JSON")?;
    report.validate()?;
    Ok(report)
}

fn write_atomic_bounded(path: &Path, bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_REPORT_BYTES {
        bail!(
            "{} is {} bytes, above the {} byte limit",
            path.display(),
            bytes.len(),
            MAX_REPORT_BYTES
        );
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("report path has no file name: {}", path.display()))?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("report path has no parent: {}", path.display()))?;
    let temp_name = format!(
        ".{file_name}.tmp-{}-{}",
        std::process::id(),
        Uuid::now_v7().simple()
    );
    let temp_path = parent.join(temp_name);
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp_path)
            .with_context(|| format!("creating temporary report {}", temp_path.display()))?;
        file.write_all(bytes)
            .with_context(|| format!("writing temporary report {}", temp_path.display()))?;
        file.sync_all()
            .with_context(|| format!("syncing temporary report {}", temp_path.display()))?;
        drop(file);
        fs::rename(&temp_path, path).with_context(|| {
            format!(
                "renaming temporary report {} to {}",
                temp_path.display(),
                path.display()
            )
        })?;
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

pub fn render_markdown(report: &BenchReport) -> String {
    let mut text = String::new();
    text.push_str("# VerlinPark cost-per-solved-task benchmark\n\n");
    text.push_str(&format!("- Generated: `{}`\n", report.generated_at));
    text.push_str(&format!(
        "- Commit: `{}`{}\n",
        report.commit.as_deref().unwrap_or("unknown"),
        match report.commit_dirty {
            Some(true) => " (dirty worktree)",
            Some(false) => "",
            None => " (dirtiness unknown)",
        }
    ));
    text.push_str(&format!(
        "- Host: {} / {} · {} logical CPUs{}\n",
        report.host.os,
        report.host.arch,
        report.host.logical_cpus,
        report
            .host
            .hostname
            .as_deref()
            .map(|hostname| format!(" · {hostname}"))
            .unwrap_or_default()
    ));
    text.push_str(&format!(
        "- Provider: `{}` (`{}`) · model `{}` · usage `{}`\n",
        report.provider.id,
        report.provider.mode,
        report.provider.model,
        report.provider.usage_source
    ));
    text.push_str(&format!(
        "- Prices (USD per million): input {} · output {} · cache read {} · cache write {}\n",
        format_usd(report.pricing.input_usd_per_million),
        format_usd(report.pricing.output_usd_per_million),
        format_usd(report.pricing.cache_read_usd_per_million),
        format_usd(report.pricing.cache_write_usd_per_million)
    ));
    text.push_str(&format!(
        "- Selection: {} scenario(s) × {} repetition(s)\n\n",
        report.selection.scenario_ids.len(),
        report.selection.repetitions
    ));
    text.push_str(&format!("> **Caveat:** {}\n\n", report.caveat));
    text.push_str("## Summary\n\n");
    text.push_str(&format!(
        "Solved {}/{} runs · total cost {} USD · cost/solved {} USD · tokens/solved {} · cache read {} · verifier pass {} · merge rate {}\n\n",
        report.summary.solved_runs,
        report.summary.run_count,
        format_usd(report.summary.total_cost_usd),
        format_optional(report.summary.cost_per_solved_task_usd, 6),
        format_optional(report.summary.tokens_per_solved_task, 1),
        format_percent(report.summary.cache_hit_ratio),
        format_percent(report.summary.verifier_pass_rate),
        format_percent(report.summary.merge_rate)
    ));
    if report.summary.harness_faults > 0 {
        text.push_str(&format!(
            "**Harness faults:** {} run(s) are invalid and force exit code 2.\n\n",
            report.summary.harness_faults
        ));
    }
    text.push_str("## Scenarios\n\n");
    text.push_str(
        "| Scenario | Solved | Cost / solved | Tokens / solved | Cache read | Verifier | Merge | Mean wall | Peak Workers |\n",
    );
    text.push_str("|---|---:|---:|---:|---:|---:|---:|---:|---:|\n");
    for scenario in &report.scenarios {
        text.push_str(&format!(
            "| {} (`{}`) | {}/{} | {} | {} | {} | {} | {} | {} ms | {} |\n",
            markdown_cell(&scenario.title),
            scenario.scenario_id,
            scenario.solved_runs,
            scenario.repetitions,
            format_optional(scenario.cost_per_solved_task_usd, 6),
            format_optional(scenario.tokens_per_solved_task, 1),
            format_percent(scenario.cache_hit_ratio),
            format_percent(scenario.verifier_pass_rate),
            format_percent(scenario.merge_rate),
            format_optional(Some(scenario.mean_wall_clock_ms), 1),
            scenario.max_peak_workers
        ));
    }
    text.push('\n');
    if !report.runs.iter().all(|run| run.solved) {
        text.push_str("## Unsolved runs\n\n");
        text.push_str("| Scenario | Rep | Reasons |\n|---|---:|---|\n");
        for run in report.runs.iter().filter(|run| !run.solved) {
            let reasons = run
                .unsolved_reasons
                .iter()
                .map(|reason| format!("{}: {}", reason.code, reason.detail))
                .collect::<Vec<_>>()
                .join("; ");
            text.push_str(&format!(
                "| {} | {} | {} |\n",
                markdown_cell(&run.scenario_id),
                run.repetition,
                markdown_cell(&reasons)
            ));
        }
        text.push('\n');
    }
    if report.runs.iter().any(|run| run.harness_fault.is_some()) {
        text.push_str("## Harness faults\n\n");
        for run in report.runs.iter().filter(|run| run.harness_fault.is_some()) {
            text.push_str(&format!(
                "- `{}` rep {}: {}\n",
                run.scenario_id,
                run.repetition,
                run.harness_fault.as_deref().unwrap_or_default()
            ));
        }
        text.push('\n');
    }
    text.push_str("## Reproducing\n\n");
    text.push_str(
        "Run `vp bench list` for the scenario catalog and `vp bench run --scenario <id> --repetitions <n> --output <dir>` to reproduce. The deterministic fake provider performs no network calls; real-provider mode requires an explicit flag and environment gate.\n",
    );
    text
}

pub fn render_csv(report: &BenchReport) -> String {
    const HEADER: &[&str] = &[
        "scenario",
        "repetition",
        "run_id",
        "session_id",
        "solved",
        "acceptance_passed",
        "session_status",
        "timed_out",
        "harness_fault",
        "verifier_checked",
        "verifier_approved",
        "verifier_verdict",
        "merge_entries",
        "merge_merged",
        "merge_expected",
        "merge_success",
        "wall_clock_ms",
        "input_tokens",
        "output_tokens",
        "cache_read_tokens",
        "cache_write_tokens",
        "total_tokens",
        "cost_usd",
        "cost_per_solved_task_usd",
        "tokens_per_solved_task",
        "cache_hit_ratio",
        "provider_calls",
        "usage_calls",
        "estimated_usage",
        "tool_calls",
        "tool_errors",
        "tool_output_bytes",
        "tool_output_spills",
        "compactions",
        "compaction_failures",
        "retries",
        "human_interventions",
        "scouts",
        "peak_workers",
        "plan_tasks_total",
        "plan_tasks_completed",
        "plan_tasks_failed",
        "unsolved_reasons",
    ];
    let mut csv = String::new();
    push_csv_row(
        &mut csv,
        &HEADER
            .iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>(),
    );
    for run in &report.runs {
        let reasons = run
            .unsolved_reasons
            .iter()
            .map(|reason| format!("{}: {}", reason.code, reason.detail))
            .collect::<Vec<_>>()
            .join("; ");
        let row = vec![
            run.scenario_id.clone(),
            run.repetition.to_string(),
            run.run_id.clone(),
            run.session_id.clone(),
            run.solved.to_string(),
            run.acceptance.passed.to_string(),
            run.session_status.clone(),
            run.timed_out.to_string(),
            run.harness_fault.clone().unwrap_or_default(),
            run.verifier.checked.to_string(),
            run.verifier.approved.to_string(),
            run.verifier.verdict.clone().unwrap_or_default(),
            run.merge.entries.to_string(),
            run.merge.merged.to_string(),
            run.merge
                .expected
                .map(|value| value.to_string())
                .unwrap_or_default(),
            run.merge
                .success
                .map(|value| value.to_string())
                .unwrap_or_default(),
            run.metrics.wall_clock_ms.to_string(),
            run.metrics.input_tokens.to_string(),
            run.metrics.output_tokens.to_string(),
            run.metrics.cache_read_tokens.to_string(),
            run.metrics.cache_write_tokens.to_string(),
            run.metrics.total_tokens.to_string(),
            format!("{:.6}", run.metrics.cost_usd),
            run.metrics
                .cost_per_solved_task_usd
                .map(|value| format!("{value:.6}"))
                .unwrap_or_default(),
            run.metrics
                .tokens_per_solved_task
                .map(|value| format!("{value:.3}"))
                .unwrap_or_default(),
            run.metrics
                .cache_hit_ratio
                .map(|value| format!("{value:.6}"))
                .unwrap_or_default(),
            run.metrics.provider_calls.to_string(),
            run.metrics.usage_calls.to_string(),
            run.metrics.estimated_usage.to_string(),
            run.metrics.tool_calls.to_string(),
            run.metrics.tool_errors.to_string(),
            run.metrics.tool_output_bytes.to_string(),
            run.metrics
                .tool_output_spills
                .map(|value| value.to_string())
                .unwrap_or_default(),
            run.metrics.compactions.to_string(),
            run.metrics.compaction_failures.to_string(),
            run.metrics.retries.to_string(),
            run.metrics.human_interventions.to_string(),
            run.metrics.scouts.to_string(),
            run.metrics.peak_workers.to_string(),
            run.metrics.plan_tasks_total.to_string(),
            run.metrics.plan_tasks_completed.to_string(),
            run.metrics.plan_tasks_failed.to_string(),
            reasons,
        ];
        push_csv_row(&mut csv, &row);
    }
    csv
}

fn push_csv_row(output: &mut String, fields: &[String]) {
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        output.push_str(&escape_csv(field));
    }
    output.push('\n');
}

fn escape_csv(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\n') || field.contains('\r') {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

fn markdown_cell(value: &str) -> String {
    let single_line = value.replace(['\n', '\r'], " ");
    let escaped = single_line.replace('|', "\\|");
    if escaped.chars().count() <= 160 {
        escaped
    } else {
        let mut truncated = escaped.chars().take(157).collect::<String>();
        truncated.push_str("...");
        truncated
    }
}

/// Convenience used by CLI comparisons when only aggregate fields are needed.
pub fn delta_percent(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    percent_change(a, b)
}

/// Build a comparison key for a scenario and concrete run provider/pricing.
pub fn comparison_key(
    scenario: &Scenario,
    provider: &ProviderMetadata,
    pricing: &Pricing,
) -> ComparisonKey {
    ComparisonKey {
        fixture_kind: scenario.fixture.as_str().to_string(),
        fixture_fingerprint: scenario.fixture_plan().fingerprint(),
        acceptance_fingerprint: scenario.fixture_plan().acceptance_fingerprint(),
        provider_mode: provider.mode.clone(),
        model: provider.model.clone(),
        pricing_fingerprint: pricing.fingerprint(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::AcceptanceExecution;
    use crate::scenario::Scenario;
    use serde_json::json;

    fn pricing() -> Pricing {
        Pricing::new(3.0, 15.0, 0.3, 3.75)
    }

    fn provider() -> ProviderMetadata {
        ProviderMetadata::fake("vp-bench-fake", "bench-fake-1", "simulated", 0)
    }

    fn scenario() -> Scenario {
        Scenario::find("baseline-single-session").unwrap()
    }

    fn run(solved: bool, tokens: u64, wall: u64) -> RunRecord {
        let scenario = scenario();
        let usage = TokenUsage {
            input_tokens: tokens.saturating_sub(100),
            output_tokens: 100,
            cache_read_tokens: 20,
            cache_write_tokens: 5,
            calls: 1,
        };
        let mut metrics = RunMetrics {
            wall_clock_ms: wall,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            cache_write_tokens: usage.cache_write_tokens,
            total_tokens: tokens,
            provider_calls: 1,
            usage_calls: 1,
            estimated_usage: false,
            cost_usd: 0.0,
            ledger_cost_usd: 0.0,
            cost_reconciled: true,
            cost_per_solved_task_usd: None,
            tokens_per_solved_task: None,
            cache_hit_ratio: None,
            tool_calls: 1,
            tool_errors: 0,
            tool_output_bytes: 42,
            tool_output_spills: None,
            tool_output_paging_supported: false,
            compactions: 0,
            compaction_failures: 0,
            retries: 0,
            human_interventions: 0,
            scouts: 0,
            peak_workers: 1,
            agent_failures: 0,
            plan_tasks_total: 0,
            plan_tasks_completed: 0,
            plan_tasks_failed: 0,
            plan_tasks_running: 0,
        };
        metrics.recompute_cost(&pricing(), solved);
        RunRecord {
            run_id: format!("run-{wall}-{solved}"),
            scenario_id: scenario.id.clone(),
            scenario_title: scenario.title.clone(),
            repetition: 1,
            started_at: "2026-11-11T00:00:00Z".to_string(),
            finished_at: "2026-11-11T00:00:01Z".to_string(),
            session_id: format!("session-{wall}"),
            session_status: if solved { "idle" } else { "failed" }.to_string(),
            timed_out: false,
            solved,
            acceptance: AcceptanceResult::from_execution(
                AcceptanceExecution {
                    passed: solved,
                    exit_code: Some(if solved { 0 } else { 1 }),
                    timed_out: false,
                    duration_ms: 4,
                    stdout: "acceptance: ok".to_string(),
                    stderr: String::new(),
                },
                scenario.acceptance.fingerprint(),
            ),
            verifier: VerifierMetrics::from_verdicts(Vec::new()),
            merge: MergeMetrics::new(0, 0, 0, 0, None),
            metrics,
            unsolved_reasons: if solved {
                Vec::new()
            } else {
                vec![UnsolvedReason::new("acceptance_failed", "fixture mismatch")]
            },
            harness_fault: None,
            comparison: comparison_key(&scenario, &provider(), &pricing()),
            pricing: pricing(),
            provider: provider(),
            target_paths: scenario.target_paths(),
            retained_workspace: None,
        }
    }

    fn report() -> BenchReport {
        let scenario = scenario();
        BenchReport::build(
            vec![run(true, 2_000, 100), run(true, 4_000, 300)],
            std::slice::from_ref(&scenario),
            2,
            "2026-11-11T00:00:00Z",
            Some("deadbeef".to_string()),
            Some(false),
            HostMetadata {
                os: "macos".into(),
                arch: "aarch64".into(),
                hostname: Some("bench-host".into()),
                logical_cpus: 10,
            },
        )
        .unwrap()
    }

    #[test]
    fn report_builds_aggregates_and_zero_solved_handling() {
        let report = report();
        assert!(report.all_solved());
        assert_eq!(report.summary.run_count, 2);
        assert_eq!(report.summary.solved_runs, 2);
        assert_eq!(report.scenarios.len(), 1);
        let expected_total = report
            .runs
            .iter()
            .map(|run| run.metrics.cost_usd)
            .sum::<f64>();
        assert!((report.scenarios[0].total_cost_usd - expected_total).abs() < 1e-12);
        assert!(
            (report.scenarios[0].cost_per_solved_task_usd.unwrap() - expected_total / 2.0).abs()
                < 1e-12
        );
        assert_eq!(report.scenarios[0].median_wall_clock_ms, 200.0);

        let unsolved = run(false, 1_000, 100);
        let report = BenchReport::build(
            vec![unsolved.clone()],
            std::slice::from_ref(&scenario()),
            1,
            "2026-11-11T00:00:00Z",
            None,
            None,
            HostMetadata::detect(),
        )
        .unwrap();
        assert!(!report.all_solved());
        assert_eq!(report.summary.cost_per_solved_task_usd, None);
        assert_eq!(report.summary.tokens_per_solved_task, None);
        assert_eq!(
            report.summary.cache_hit_ratio,
            unsolved.metrics.cache_hit_ratio
        );
    }

    #[test]
    fn mixed_simulated_latency_keeps_one_provider_identity() {
        let scenario = scenario();
        let mut first = run(true, 1_000, 10);
        first.provider.simulated_latency_ms = 0;
        let mut second = run(true, 1_000, 20);
        second.provider.simulated_latency_ms = 40;
        let report = BenchReport::build(
            vec![first, second],
            std::slice::from_ref(&scenario),
            2,
            "2026-11-11T00:00:00Z",
            None,
            None,
            HostMetadata::detect(),
        )
        .unwrap();
        assert_eq!(report.provider.simulated_latency_ms, 40);
        assert_eq!(report.runs[0].provider.simulated_latency_ms, 0);
        assert_eq!(report.runs[1].provider.simulated_latency_ms, 40);
    }

    #[test]
    fn report_rejects_inconsistent_run_counts() {
        let scenario = scenario();
        let error = BenchReport::build(
            vec![run(true, 1_000, 10)],
            std::slice::from_ref(&scenario),
            2,
            "now",
            None,
            None,
            HostMetadata::detect(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("expected 2"));
    }

    #[test]
    fn json_round_trip_preserves_schema_and_caveat() {
        let report = report();
        let bytes = report.to_json_bytes().unwrap();
        let loaded = parse_report(&bytes).unwrap();
        assert_eq!(loaded.schema_version, SCHEMA_VERSION);
        assert_eq!(loaded.caveat, REPORT_CAVEAT);
        assert_eq!(loaded.summary, report.summary);
    }

    #[test]
    fn parser_rejects_oversized_and_bad_schema_reports() {
        let oversized = vec![0u8; MAX_REPORT_BYTES + 1];
        assert!(parse_report(&oversized).is_err());
        let mut value = serde_json::to_value(report()).unwrap();
        value["schema_version"] = json!(999);
        assert!(parse_report(serde_json::to_string(&value).unwrap().as_bytes()).is_err());
    }

    #[test]
    fn markdown_contains_metadata_caveat_and_unsolved_reasons() {
        let report = report();
        let markdown = report.to_markdown();
        assert!(markdown.contains("cost-per-solved-task"));
        assert!(markdown.contains("deadbeef"));
        assert!(markdown.contains("bench-host"));
        assert!(markdown.contains("not a universal ranking"));
        assert!(markdown.contains(&format_usd(
            report.summary.cost_per_solved_task_usd.unwrap()
        )));
        let mut unsolved = report.clone();
        unsolved.runs[0].solved = false;
        unsolved.runs[0].unsolved_reasons =
            vec![UnsolvedReason::new("verifier_rejected", "fixture said no")];
        unsolved.summary = summarize(&unsolved.runs);
        let markdown = unsolved.to_markdown();
        assert!(markdown.contains("Unsolved runs"));
        assert!(markdown.contains("verifier_rejected"));
    }

    #[test]
    fn csv_shape_and_escaping_are_stable() {
        let report = report();
        let csv = report.to_csv();
        let lines = csv.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("scenario,repetition,run_id"));
        assert!(lines[1].contains("baseline-single-session"));
        assert!(!lines[1].contains("acceptance_failed"));
        let escaped = escape_csv("a,b\"c\nd");
        assert_eq!(escaped, "\"a,b\"\"c\nd\"");
        assert!(render_csv(&report).contains("cost_per_solved_task_usd"));
    }

    #[test]
    fn atomic_writes_produce_three_bounded_files_and_no_temp_leftovers() {
        let dir = tempfile::TempDir::new().unwrap();
        let artifacts = write_artifacts_with_stem(&report(), dir.path(), "test-report").unwrap();
        for path in [
            &artifacts.json_path,
            &artifacts.markdown_path,
            &artifacts.csv_path,
        ] {
            assert!(path.exists(), "{} was not written", path.display());
            assert!(fs::metadata(path).unwrap().len() <= MAX_REPORT_BYTES as u64);
        }
        let leftovers = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".test-report")
            })
            .count();
        assert_eq!(leftovers, 0);
        let loaded = load_report(&artifacts.json_path).unwrap();
        assert_eq!(loaded.summary, report().summary);
    }

    #[test]
    fn unsafe_report_stems_are_rejected() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(write_artifacts_with_stem(&report(), dir.path(), "../escape").is_err());
        assert!(write_artifacts_with_stem(&report(), dir.path(), "").is_err());
    }

    #[test]
    fn comparison_key_changes_with_acceptance_and_model() {
        let scenario = scenario();
        let baseline = comparison_key(&scenario, &provider(), &pricing());
        let mut changed = scenario.clone();
        changed.acceptance.expected_files[0]
            .content
            .push_str("changed");
        let changed = comparison_key(&changed, &provider(), &pricing());
        assert_ne!(
            baseline.acceptance_fingerprint,
            changed.acceptance_fingerprint
        );
        let other_model = ProviderMetadata::fake("vp-bench-fake", "other", "simulated", 0);
        let other = comparison_key(&scenario, &other_model, &pricing());
        assert_ne!(baseline.model, other.model);
    }

    #[test]
    fn markdown_cells_are_bounded_and_escape_pipes() {
        let cell = markdown_cell(&format!("a|b\n{}", "x".repeat(300)));
        assert!(cell.starts_with("a\\|b"));
        assert!(cell.ends_with("..."));
        assert!(cell.chars().count() <= 160);
    }

    #[test]
    fn delta_percent_uses_none_for_zero_baseline() {
        assert_eq!(delta_percent(Some(0.0), Some(1.0)), None);
        assert_eq!(delta_percent(Some(1.0), Some(2.0)), Some(100.0));
    }
}

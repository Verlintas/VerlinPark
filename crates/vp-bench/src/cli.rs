//! `vp-bench` / `vp bench` command surface.
//!
//! Exit codes are part of the contract:
//!
//! * `0` — every selected scenario solved;
//! * `1` — at least one selected scenario is unsolved;
//! * `2` — harness, configuration, report, or compatibility error.
//!
//! `vp bench report` exits 0 after successfully loading and rendering a report.
//! `vp bench compare` exits 0 only when both reports are fully solved and
//! comparable; an unsolved side returns 1, and a refused comparison returns 2.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use vp_store::paths;

use crate::compare::{CompareOptions, compare_reports};
use crate::metrics::{Pricing, format_optional, format_percent, format_usd};
use crate::provider::RealProviderSettings;
use crate::report::{BenchReport, REPORT_CAVEAT, ReportArtifacts, load_report, write_artifacts};
use crate::runner::{BenchRunOptions, ProviderSelection, report_environment, run_scenarios};
use crate::scenario::{Availability, Scenario};

#[derive(Debug, Parser)]
#[command(
    name = "vp-bench",
    about = "Deterministic cost-per-solved-task benchmark harness",
    long_about = "Run disposable, scripted VerlinPark scenarios and report cost per solved task.\n\nExit codes: 0 all selected scenarios solved; 1 any scenario unsolved; 2 harness/config/report/comparison error."
)]
pub struct BenchCli {
    /// Print raw JSON instead of human-readable output.
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: BenchCommand,
}

#[derive(Debug, Clone, Subcommand)]
pub enum BenchCommand {
    /// List the scenario catalog, including unavailable entries and reasons.
    List,
    /// Run one or more scenarios with the deterministic fake provider by default.
    Run(RunArgs),
    /// Render a saved JSON report as Markdown (or JSON with --json).
    Report {
        /// JSON report emitted by `vp bench run`.
        file: PathBuf,
    },
    /// Compare two saved reports. Mismatched fixtures/acceptance/models are refused.
    Compare {
        a: PathBuf,
        b: PathBuf,
        /// Explicitly allow an apples-to-oranges comparison and label it.
        #[arg(long)]
        allow_mismatch: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ProviderArg {
    Fake,
    Real,
}

impl ProviderArg {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fake => "fake",
            Self::Real => "real",
        }
    }
}

#[derive(Debug, Clone, Args)]
pub struct RunArgs {
    /// Scenario id; repeatable. Defaults to all available scenarios.
    #[arg(long = "scenario")]
    pub scenarios: Vec<String>,
    /// Number of repetitions per scenario (1-100).
    #[arg(long, default_value_t = 1)]
    pub repetitions: usize,
    /// Directory for the JSON/Markdown/CSV report.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Retain disposable fixture workspaces for debugging.
    #[arg(long)]
    pub keep_worktrees: bool,
    /// Provider mode. Fake is deterministic and performs no network I/O.
    #[arg(long, value_enum, default_value_t = ProviderArg::Fake)]
    pub provider: ProviderArg,
    /// Required acknowledgement for real-provider mode.
    #[arg(long)]
    pub allow_real_provider: bool,
    /// Model override for real-provider mode (also needs the environment gate).
    #[arg(long)]
    pub model: Option<String>,
    /// USD per million input tokens. Defaults to the scenario profile.
    #[arg(long)]
    pub input_price: Option<f64>,
    /// USD per million output tokens. Defaults to the scenario profile.
    #[arg(long)]
    pub output_price: Option<f64>,
    /// USD per million cache-read tokens. Defaults to the scenario profile.
    #[arg(long)]
    pub cache_read_price: Option<f64>,
    /// USD per million cache-write tokens. Defaults to the scenario profile.
    #[arg(long)]
    pub cache_write_price: Option<f64>,
    /// Timeout for the harness-owned acceptance script (1-300 seconds).
    #[arg(long, default_value_t = 60)]
    pub acceptance_timeout_secs: u64,
    /// Override every scenario's wall-clock timeout (1-3600 seconds).
    #[arg(long)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct CommandOutcome {
    pub exit_code: i32,
    pub human: String,
    pub json: Option<Value>,
}

pub async fn execute(command: BenchCommand, json_mode: bool) -> Result<CommandOutcome> {
    match command {
        BenchCommand::List => list_command(json_mode),
        BenchCommand::Run(args) => run_command(args, json_mode).await,
        BenchCommand::Report { file } => report_command(&file, json_mode),
        BenchCommand::Compare {
            a,
            b,
            allow_mismatch,
        } => compare_command(&a, &b, allow_mismatch, json_mode),
    }
}

fn list_command(json_mode: bool) -> Result<CommandOutcome> {
    let catalog = Scenario::catalog();
    Scenario::validate_catalog(&catalog).map_err(anyhow::Error::msg)?;
    let mut human = String::from(
        "ID                             STATUS       FIXTURE        MERGES  VERIFIER  DESCRIPTION\n",
    );
    for scenario in &catalog {
        let status = match &scenario.availability {
            Availability::Available => "available".to_string(),
            Availability::Unavailable { .. } => "unavailable".to_string(),
        };
        let merges = scenario
            .requirements
            .expected_merges
            .count()
            .map(|count| count.to_string())
            .unwrap_or_else(|| "none".to_string());
        let verifier = match scenario.requirements.verifier {
            crate::scenario::VerifierRequirement::NotRequired => "no",
            crate::scenario::VerifierRequirement::AllApproved { .. } => "yes",
        };
        human.push_str(&format!(
            "{:<30} {:<12} {:<14} {:<7} {:<9} {}\n",
            scenario.id,
            status,
            scenario.fixture.as_str(),
            merges,
            verifier,
            scenario.description
        ));
        if let Some(reason) = scenario.availability.reason() {
            human.push_str(&format!("  unavailable: {reason}\n"));
        }
    }
    human.push_str(&format!(
        "\n{} scenario(s). Fake provider is the default; real-provider mode is opt-in.\n",
        catalog.len()
    ));
    let json = json_mode.then(|| {
        json!({
            "scenarios": catalog.iter().map(|scenario| json!({
                "id": scenario.id,
                "title": scenario.title,
                "description": scenario.description,
                "fixture": scenario.fixture.as_str(),
                "available": scenario.availability.is_available(),
                "unavailable_reason": scenario.availability.reason(),
                "expected_merges": scenario.requirements.expected_merges.count(),
                "verifier_required": scenario.requirements.verifier.required_tasks().is_some(),
                "repetitions_default": 1,
            })).collect::<Vec<_>>(),
        })
    });
    Ok(CommandOutcome {
        exit_code: 0,
        human,
        json,
    })
}

async fn run_command(args: RunArgs, json_mode: bool) -> Result<CommandOutcome> {
    if args.provider == ProviderArg::Fake && args.allow_real_provider {
        bail!("--allow-real-provider is only valid with --provider real");
    }
    if args.provider == ProviderArg::Fake && args.model.is_some() {
        bail!("--model is only valid with --provider real");
    }
    let catalog = Scenario::catalog();
    Scenario::validate_catalog(&catalog).map_err(anyhow::Error::msg)?;
    let mut selected = if args.scenarios.is_empty() {
        catalog
            .into_iter()
            .filter(|scenario| scenario.availability.is_available())
            .collect::<Vec<_>>()
    } else {
        let available = catalog
            .into_iter()
            .map(|scenario| (scenario.id.clone(), scenario))
            .collect::<std::collections::HashMap<_, _>>();
        let mut selected = Vec::new();
        for id in &args.scenarios {
            let scenario = available
                .get(id)
                .cloned()
                .with_context(|| format!("unknown benchmark scenario '{id}'"))?;
            if let Some(reason) = scenario.availability.reason() {
                bail!("scenario '{id}' is unavailable in this build: {reason}");
            }
            selected.push(scenario);
        }
        selected
    };
    if selected.is_empty() {
        bail!("no runnable benchmark scenarios were selected");
    }
    for scenario in &mut selected {
        scenario.config.pricing = pricing_with_overrides(&args, scenario.config.pricing)?;
    }
    let provider = match args.provider {
        ProviderArg::Fake => ProviderSelection::Fake,
        ProviderArg::Real => {
            if !args.allow_real_provider {
                bail!(
                    "real-provider mode requires --allow-real-provider and {} = 1",
                    RealProviderSettings::GATE_ENV
                );
            }
            ProviderSelection::Real(RealProviderSettings::from_env(args.model.as_deref())?)
        }
    };
    let options = BenchRunOptions {
        repetitions: args.repetitions,
        keep_worktrees: args.keep_worktrees,
        provider,
        acceptance_timeout_secs: args.acceptance_timeout_secs,
        timeout_override_secs: args.timeout_secs,
    };
    let runs = run_scenarios(&selected, &options).await?;
    let (host, commit, dirty) = report_environment();
    let generated_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown".to_string());
    let report = BenchReport::build(
        runs,
        &selected,
        args.repetitions,
        generated_at,
        commit,
        dirty,
        host,
    )?;
    let output_dir = args
        .output
        .clone()
        .unwrap_or_else(|| paths::data_dir().join("bench"));
    let artifacts = write_artifacts(&report, &output_dir)?;
    let exit_code = benchmark_exit_code(report.all_solved(), report.harness_faults());
    let human = render_run_summary(&report, &artifacts, exit_code);
    let json = json_mode.then(|| {
        json!({
            "exit_code": exit_code,
            "report": report,
            "artifacts": artifacts,
            "output_dir": output_dir,
        })
    });
    Ok(CommandOutcome {
        exit_code,
        human,
        json,
    })
}

fn report_command(file: &std::path::Path, json_mode: bool) -> Result<CommandOutcome> {
    let report = load_report(file)?;
    let human = report.to_markdown();
    let json = json_mode.then(|| json!({"report": report}));
    Ok(CommandOutcome {
        exit_code: 0,
        human,
        json,
    })
}

fn compare_command(
    a: &std::path::Path,
    b: &std::path::Path,
    allow_mismatch: bool,
    json_mode: bool,
) -> Result<CommandOutcome> {
    let a_report = load_report(a)?;
    let b_report = load_report(b)?;
    let comparison = compare_reports(
        &a_report,
        &b_report,
        a.to_path_buf(),
        b.to_path_buf(),
        CompareOptions { allow_mismatch },
    )
    .map_err(anyhow::Error::msg)?;
    let exit_code =
        if comparison.comparable && comparison.a_all_solved() && comparison.b_all_solved() {
            0
        } else {
            1
        };
    let human = comparison.to_markdown();
    let json = json_mode.then(|| json!({"comparison": comparison}));
    Ok(CommandOutcome {
        exit_code,
        human,
        json,
    })
}

fn benchmark_exit_code(all_solved: bool, harness_faults: usize) -> i32 {
    if harness_faults > 0 {
        2
    } else if all_solved {
        0
    } else {
        1
    }
}

fn pricing_with_overrides(args: &RunArgs, base: Pricing) -> Result<Pricing> {
    let pricing = Pricing {
        input_usd_per_million: args.input_price.unwrap_or(base.input_usd_per_million),
        output_usd_per_million: args.output_price.unwrap_or(base.output_usd_per_million),
        cache_read_usd_per_million: args
            .cache_read_price
            .unwrap_or(base.cache_read_usd_per_million),
        cache_write_usd_per_million: args
            .cache_write_price
            .unwrap_or(base.cache_write_usd_per_million),
    };
    pricing.validate().map_err(anyhow::Error::msg)?;
    Ok(pricing)
}

fn render_run_summary(report: &BenchReport, artifacts: &ReportArtifacts, exit_code: i32) -> String {
    let mut human = format!(
        "vp bench · {} scenario(s) × {} repetition(s)\n",
        report.selection.scenario_ids.len(),
        report.selection.repetitions
    );
    for scenario in &report.scenarios {
        human.push_str(&format!(
            "{:<30} {}/{} solved · cost/solved {} USD · tokens/solved {} · cache {} · verifier {} · merge {}\n",
            scenario.scenario_id,
            scenario.solved_runs,
            scenario.repetitions,
            format_optional(scenario.cost_per_solved_task_usd, 6),
            format_optional(scenario.tokens_per_solved_task, 1),
            format_percent(scenario.cache_hit_ratio),
            format_percent(scenario.verifier_pass_rate),
            format_percent(scenario.merge_rate),
        ));
    }
    human.push_str(&format!(
        "summary: {}/{} runs solved · total cost {} USD · {} harness fault(s)\n",
        report.summary.solved_runs,
        report.summary.run_count,
        format_usd(report.summary.total_cost_usd),
        report.summary.harness_faults
    ));
    human.push_str(&format!("report: {}\n", artifacts.json_path.display()));
    human.push_str(&format!(
        "markdown: {}\n",
        artifacts.markdown_path.display()
    ));
    human.push_str(&format!("csv: {}\n", artifacts.csv_path.display()));
    human.push_str(&format!(
        "provider: {} · model: {} · prices USD/M: in {} out {} cache-read {} cache-write {}\n",
        report.provider.mode,
        report.provider.model,
        format_usd(report.pricing.input_usd_per_million),
        format_usd(report.pricing.output_usd_per_million),
        format_usd(report.pricing.cache_read_usd_per_million),
        format_usd(report.pricing.cache_write_usd_per_million)
    ));
    human.push_str(&format!("exit: {exit_code}\n"));
    human.push_str(&format!("caveat: {REPORT_CAVEAT}\n"));
    human
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_list_run_report_and_compare() {
        let cli = BenchCli::parse_from(["vp-bench", "list"]);
        assert!(matches!(cli.command, BenchCommand::List));

        let cli = BenchCli::parse_from([
            "vp-bench",
            "run",
            "--scenario",
            "baseline-single-session",
            "--repetitions",
            "3",
            "--output",
            "/tmp/bench",
            "--input-price",
            "1.5",
        ]);
        match cli.command {
            BenchCommand::Run(args) => {
                assert_eq!(args.scenarios, vec!["baseline-single-session"]);
                assert_eq!(args.repetitions, 3);
                assert_eq!(args.output.unwrap().to_string_lossy(), "/tmp/bench");
                assert_eq!(args.input_price, Some(1.5));
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let cli = BenchCli::parse_from(["vp-bench", "report", "report.json"]);
        assert!(matches!(cli.command, BenchCommand::Report { .. }));

        let cli = BenchCli::parse_from(["vp-bench", "compare", "a.json", "b.json"]);
        match cli.command {
            BenchCommand::Compare { allow_mismatch, .. } => assert!(!allow_mismatch),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn list_is_a_successful_compact_summary() {
        let outcome = list_command(false).unwrap();
        assert_eq!(outcome.exit_code, 0);
        assert!(outcome.human.contains("baseline-single-session"));
        assert!(outcome.human.contains("tool-output-paging-on"));
        assert!(outcome.human.contains("available"));
        assert!(!outcome.human.contains("unavailable:"));
        assert!(outcome.json.is_none());
    }

    #[tokio::test]
    async fn fake_provider_flags_are_rejected_without_real_mode() {
        let args = RunArgs {
            scenarios: vec![],
            repetitions: 1,
            output: None,
            keep_worktrees: false,
            provider: ProviderArg::Fake,
            allow_real_provider: true,
            model: None,
            input_price: None,
            output_price: None,
            cache_read_price: None,
            cache_write_price: None,
            acceptance_timeout_secs: 60,
            timeout_secs: None,
        };
        let error = run_command(args, false).await.unwrap_err();
        assert!(error.to_string().contains("only valid"));
    }

    #[test]
    fn exit_code_contract_is_explicit() {
        assert_eq!(benchmark_exit_code(true, 0), 0);
        assert_eq!(benchmark_exit_code(false, 0), 1);
        assert_eq!(benchmark_exit_code(false, 1), 2);
        assert_eq!(benchmark_exit_code(true, 1), 2);
    }

    #[test]
    fn pricing_overrides_are_explicit_and_validated() {
        let mut args = RunArgs {
            scenarios: vec![],
            repetitions: 1,
            output: None,
            keep_worktrees: false,
            provider: ProviderArg::Fake,
            allow_real_provider: false,
            model: None,
            input_price: Some(1.0),
            output_price: Some(2.0),
            cache_read_price: Some(0.1),
            cache_write_price: Some(0.2),
            acceptance_timeout_secs: 60,
            timeout_secs: None,
        };
        let pricing = pricing_with_overrides(&args, Pricing::benchmark_default()).unwrap();
        assert_eq!(pricing.input_usd_per_million, 1.0);
        assert_eq!(pricing.output_usd_per_million, 2.0);
        args.input_price = Some(-1.0);
        assert!(pricing_with_overrides(&args, Pricing::benchmark_default()).is_err());
    }
}

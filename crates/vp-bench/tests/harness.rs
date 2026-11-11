//! End-to-end harness tests.
//!
//! These tests exercise disposable Git repositories, ordinary sessions and
//! TeamPlans, the scripted provider, the Verifier/Integrator path, acceptance,
//! report generation, and comparison refusal. They perform no network I/O.

use std::fs;

use vp_bench::cli::{BenchCommand, ProviderArg, RunArgs, execute};
use vp_bench::compare::{CompareOptions, compare_reports};
use vp_bench::metrics::Pricing;
use vp_bench::report::{
    BenchReport, HostMetadata, REPORT_CAVEAT, load_report, write_artifacts_with_stem,
};
use vp_bench::runner::{BenchRunOptions, ProviderSelection, run_scenarios};
use vp_bench::scenario::Scenario;

fn baseline() -> Scenario {
    Scenario::find("baseline-single-session").unwrap()
}

fn build_report(
    runs: Vec<vp_bench::RunRecord>,
    selected: &[Scenario],
    repetitions: usize,
) -> BenchReport {
    BenchReport::build(
        runs,
        selected,
        repetitions,
        "2026-11-11T00:00:00Z",
        Some("test-commit".to_string()),
        Some(false),
        HostMetadata {
            os: "macos".to_string(),
            arch: "aarch64".to_string(),
            hostname: Some("test-host".to_string()),
            logical_cpus: 8,
        },
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn baseline_plan_and_scout_runs_are_solved_and_reported() {
    let baseline = Scenario::find("baseline-single-session").unwrap();
    let plan = Scenario::find("plan-worker-chain").unwrap();
    let scout = Scenario::find("read-only-scout").unwrap();
    let selected = vec![baseline, plan, scout];
    let runs = run_scenarios(&selected, &BenchRunOptions::fake(1))
        .await
        .unwrap();
    assert_eq!(runs.len(), 3, "one run per scenario");
    for run in &runs {
        assert!(
            run.solved,
            "{} had unsolved reasons: {:?}",
            run.scenario_id, run.unsolved_reasons
        );
        assert_eq!(run.session_status, "idle");
        assert!(run.acceptance.passed);
        assert!(!run.session_id.is_empty());
        assert!(run.metrics.cost_usd > 0.0);
        assert!(run.metrics.input_tokens > 0);
        assert!(!run.metrics.estimated_usage);
        assert!(run.metrics.cost_reconciled);
    }
    let plan_run = runs
        .iter()
        .find(|run| run.scenario_id == "plan-worker-chain")
        .unwrap();
    assert_eq!(plan_run.verifier.checked, 1);
    assert_eq!(plan_run.verifier.approved, 1);
    assert_eq!(plan_run.merge.merged, 1);
    assert_eq!(plan_run.merge.expected, Some(1));
    assert_eq!(plan_run.metrics.plan_tasks_completed, 1);
    let scout_run = runs
        .iter()
        .find(|run| run.scenario_id == "read-only-scout")
        .unwrap();
    assert_eq!(scout_run.metrics.scouts, 1);
    assert_eq!(scout_run.merge.entries, 0);

    let report = build_report(runs, &selected, 1);
    assert!(report.all_solved());
    assert_eq!(report.summary.solved_runs, 3);
    assert!(report.summary.cost_per_solved_task_usd.unwrap() > 0.0);
    assert!(report.summary.tokens_per_solved_task.unwrap() > 0.0);
    assert!(report.summary.cache_hit_ratio.unwrap() > 0.0);
    assert_eq!(report.summary.verifier_pass_rate, Some(1.0));
    assert_eq!(report.scenarios.len(), 3);
    assert_eq!(report.scenarios[0].session_ids.len(), 1);

    let dir = tempfile::TempDir::new().unwrap();
    let artifacts = write_artifacts_with_stem(&report, dir.path(), "e2e-report").unwrap();
    let loaded = load_report(&artifacts.json_path).unwrap();
    assert_eq!(loaded.summary, report.summary);
    assert!(
        fs::read_to_string(&artifacts.markdown_path)
            .unwrap()
            .contains(REPORT_CAVEAT)
    );
    let csv = fs::read_to_string(&artifacts.csv_path).unwrap();
    assert_eq!(csv.lines().count(), 4, "header + one row per run");
    assert!(csv.contains("baseline-single-session"));
    assert!(csv.contains("plan-worker-chain"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeated_runs_are_deterministic_for_tokens_and_cost() {
    let scenario = baseline();
    let runs = run_scenarios(&[scenario], &BenchRunOptions::fake(2))
        .await
        .unwrap();
    assert_eq!(runs.len(), 2);
    assert!(runs.iter().all(|run| run.solved));
    let a = &runs[0];
    let b = &runs[1];
    assert_eq!(a.metrics.input_tokens, b.metrics.input_tokens);
    assert_eq!(a.metrics.output_tokens, b.metrics.output_tokens);
    assert_eq!(a.metrics.cost_usd, b.metrics.cost_usd);
    assert_eq!(a.metrics.tool_calls, b.metrics.tool_calls);
    assert_ne!(a.session_id, b.session_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejected_verifier_and_failed_merge_are_unsolved() {
    let mut scenario = Scenario::find("plan-worker-chain").unwrap();
    scenario.config.fake_verifier_approves = false;
    let runs = run_scenarios(std::slice::from_ref(&scenario), &BenchRunOptions::fake(1))
        .await
        .unwrap();
    let run = &runs[0];
    assert!(
        !run.solved,
        "rejection must not be reported as a solved task"
    );
    assert!(run.metrics.cost_usd > 0.0, "tokens were still spent");
    assert!(
        run.unsolved_reasons
            .iter()
            .any(|reason| reason.code == "verifier_rejected")
    );
    assert!(
        run.unsolved_reasons
            .iter()
            .any(|reason| reason.code == "acceptance_failed")
    );
    assert!(
        run.unsolved_reasons
            .iter()
            .any(|reason| reason.code == "merge_expected_missing")
    );
    assert_eq!(run.merge.merged, 0);
    let report = build_report(runs, std::slice::from_ref(&scenario), 1);
    assert!(!report.all_solved());
    assert_eq!(report.summary.cost_per_solved_task_usd, None);
    assert_eq!(report.summary.tokens_per_solved_task, None);
    assert!(report.summary.total_cost_usd > 0.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn worktree_parallelism_caps_change_measured_peak() {
    let one = Scenario::find("worktree-parallelism-1").unwrap();
    let four = Scenario::find("worktree-parallelism-4").unwrap();
    let runs = run_scenarios(&[one, four], &BenchRunOptions::fake(1))
        .await
        .unwrap();
    assert_eq!(runs.len(), 2);
    assert!(runs.iter().all(|run| run.solved), "{runs:?}");
    let serial = runs
        .iter()
        .find(|run| run.scenario_id == "worktree-parallelism-1")
        .unwrap();
    let parallel = runs
        .iter()
        .find(|run| run.scenario_id == "worktree-parallelism-4")
        .unwrap();
    assert_eq!(serial.metrics.peak_workers, 1);
    assert!(
        parallel.metrics.peak_workers >= 2,
        "parallel peak was {}",
        parallel.metrics.peak_workers
    );
    assert_eq!(serial.merge.merged, 4);
    assert_eq!(parallel.merge.merged, 4);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unavailable_scenario_is_refused_not_silently_skipped() {
    let scenario = Scenario::find("tool-output-paging-on").unwrap();
    let error = run_scenarios(&[scenario], &BenchRunOptions::fake(1))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("unavailable"));
    assert!(error.to_string().contains("C1"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_run_report_and_compare_are_consistent() {
    let dir = tempfile::TempDir::new().unwrap();
    let output = dir.path().join("reports");
    let args = RunArgs {
        scenarios: vec!["baseline-single-session".to_string()],
        repetitions: 1,
        output: Some(output.clone()),
        keep_worktrees: false,
        provider: ProviderArg::Fake,
        allow_real_provider: false,
        model: None,
        input_price: None,
        output_price: None,
        cache_read_price: None,
        cache_write_price: None,
        acceptance_timeout_secs: 60,
        timeout_secs: None,
    };
    let outcome = execute(BenchCommand::Run(args), false).await.unwrap();
    assert_eq!(outcome.exit_code, 0, "{}", outcome.human);
    assert!(outcome.human.contains("1/1 solved"));
    let json_path = fs::read_dir(&output)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension.to_string_lossy() == "json")
        })
        .expect("JSON report");
    let report = load_report(&json_path).unwrap();
    assert!(report.all_solved());

    let report_outcome = execute(
        BenchCommand::Report {
            file: json_path.clone(),
        },
        false,
    )
    .await
    .unwrap();
    assert_eq!(report_outcome.exit_code, 0);
    assert!(report_outcome.human.contains("cost-per-solved-task"));

    let compare_outcome = execute(
        BenchCommand::Compare {
            a: json_path.clone(),
            b: json_path.clone(),
            allow_mismatch: false,
        },
        false,
    )
    .await
    .unwrap();
    assert_eq!(compare_outcome.exit_code, 0);

    let mut changed: serde_json::Value =
        serde_json::from_slice(&fs::read(&json_path).unwrap()).unwrap();
    changed["provider"]["model"] = serde_json::json!("different-model");
    let changed_path = dir.path().join("changed.json");
    fs::write(&changed_path, serde_json::to_vec_pretty(&changed).unwrap()).unwrap();
    let refused = execute(
        BenchCommand::Compare {
            a: json_path,
            b: changed_path,
            allow_mismatch: false,
        },
        false,
    )
    .await
    .unwrap_err();
    assert!(refused.to_string().contains("comparison refused"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn comparison_refuses_fixture_and_pricing_mismatches() {
    let scenario = baseline();
    let runs = run_scenarios(std::slice::from_ref(&scenario), &BenchRunOptions::fake(1))
        .await
        .unwrap();
    let report = build_report(runs, std::slice::from_ref(&scenario), 1);
    let same = compare_reports(
        &report,
        &report,
        "a".into(),
        "b".into(),
        CompareOptions::default(),
    )
    .unwrap();
    assert!(same.comparable);

    let mut different_pricing = report.clone();
    different_pricing.pricing = Pricing::new(1.0, 2.0, 0.1, 0.2);
    let refusal = compare_reports(
        &report,
        &different_pricing,
        "a".into(),
        "b".into(),
        CompareOptions::default(),
    )
    .unwrap_err();
    assert!(
        refusal
            .mismatches
            .iter()
            .any(|mismatch| mismatch.field == "pricing")
    );

    let mut fixture_changed = report.clone();
    fixture_changed.scenarios[0].comparison.fixture_fingerprint = "different".into();
    let refusal = compare_reports(
        &report,
        &fixture_changed,
        "a".into(),
        "b".into(),
        CompareOptions::default(),
    )
    .unwrap_err();
    assert!(
        refusal
            .mismatches
            .iter()
            .any(|mismatch| mismatch.field == "fixture_fingerprint")
    );

    let allowed = compare_reports(
        &report,
        &fixture_changed,
        "a".into(),
        "b".into(),
        CompareOptions {
            allow_mismatch: true,
        },
    )
    .unwrap();
    assert!(!allowed.comparable);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_provider_selection_is_gated_and_ignored_by_default() {
    let scenario = baseline();
    let settings = vp_bench::provider::RealProviderSettings::from_parts(
        "http://127.0.0.1:9/v1",
        "local-test",
        None,
    );
    let options = BenchRunOptions {
        repetitions: 1,
        keep_worktrees: false,
        provider: ProviderSelection::Real(settings),
        acceptance_timeout_secs: 60,
        timeout_override_secs: None,
    };
    // The environment gate is normally absent in CI. The run must fail before
    // performing any network call, rather than silently using fake data.
    if !vp_bench::provider::RealProviderSettings::gate_enabled() {
        let error = run_scenarios(&[scenario], &options).await.unwrap_err();
        assert!(error.to_string().contains("real-provider mode is disabled"));
    }
}

/// Real-provider smoke test. This is intentionally ignored by default: it
/// requires `VERLINPARK_BENCH_REAL_PROVIDER=1` plus a configured
/// `VERLINPARK_BENCH_BASE_URL`/`VERLINPARK_BENCH_MODEL` (or their
/// `VERLINPARK_*` fallbacks) and may incur provider charges.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires explicit real-provider environment and network access"]
async fn real_provider_baseline_smoke() {
    if !vp_bench::provider::RealProviderSettings::gate_enabled() {
        eprintln!("skipping real-provider smoke: gate environment is not set");
        return;
    }
    let settings = vp_bench::provider::RealProviderSettings::from_env(None).unwrap();
    let scenario = baseline();
    let options = BenchRunOptions {
        repetitions: 1,
        keep_worktrees: false,
        provider: ProviderSelection::Real(settings),
        acceptance_timeout_secs: 120,
        timeout_override_secs: Some(300),
    };
    let runs = run_scenarios(&[scenario], &options).await.unwrap();
    assert_eq!(runs.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn report_artifact_paths_are_bounded_and_local() {
    let dir = tempfile::TempDir::new().unwrap();
    let scenario = baseline();
    let runs = run_scenarios(std::slice::from_ref(&scenario), &BenchRunOptions::fake(1))
        .await
        .unwrap();
    let report = build_report(runs, &[scenario], 1);
    let artifacts = write_artifacts_with_stem(&report, dir.path(), "local").unwrap();
    assert!(artifacts.json_path.starts_with(dir.path()));
    assert!(artifacts.markdown_path.starts_with(dir.path()));
    assert!(artifacts.csv_path.starts_with(dir.path()));
    assert_eq!(artifacts.stem, "local");
}

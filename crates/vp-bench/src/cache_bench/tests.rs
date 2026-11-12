use super::*;
use std::sync::OnceLock;
use tempfile::TempDir;

fn report() -> CacheBenchReport {
    static REPORT: OnceLock<CacheBenchReport> = OnceLock::new();
    REPORT
        .get_or_init(|| {
            run_simulated_benchmark(
                CacheBenchScenarioConfig::default(),
                CacheBenchPricing::illustrative_default(),
            )
            .unwrap()
        })
        .clone()
}

fn assert_aggregate_close(left: &CacheBenchAggregate, right: &CacheBenchAggregate) {
    assert_eq!(left.calls, right.calls);
    assert_eq!(left.input_tokens, right.input_tokens);
    assert_eq!(left.cache_read_tokens, right.cache_read_tokens);
    assert_eq!(left.cache_miss_tokens, right.cache_miss_tokens);
    assert_eq!(left.prefix_breaks, right.prefix_breaks);
    assert!((left.cache_hit_ratio - right.cache_hit_ratio).abs() < 1e-12);
    assert!((left.cache_hit_ratio_p50 - right.cache_hit_ratio_p50).abs() < 1e-12);
    assert!((left.cache_hit_ratio_p95 - right.cache_hit_ratio_p95).abs() < 1e-12);
    assert!((left.cost.total_cost_usd - right.cost.total_cost_usd).abs() < 1e-12);
}

#[test]
fn long_prefix_scenario_reaches_the_995_floor_without_prefix_breaks() {
    let report = report();
    assert_eq!(report.mode, CacheBenchMode::Simulated);
    assert!(report.deterministic);
    assert!(!report.warmup_included);
    assert_eq!(report.calls.len(), 50);
    assert!(report.aggregate.input_tokens >= LONG_PREFIX_MIN_TOKENS);
    assert!(
        report.aggregate.cache_hit_ratio >= LONG_PREFIX_HIT_TARGET,
        "hit ratio {:.6} did not reach {:.6}",
        report.aggregate.cache_hit_ratio,
        LONG_PREFIX_HIT_TARGET
    );
    assert_eq!(report.aggregate.prefix_breaks, 0);
    assert!(
        report.aggregate.cache_miss_tokens < 40_000,
        "unexpectedly large miss total: {}",
        report.aggregate.cache_miss_tokens
    );
    assert!(report.live_target_met());
    assert!(report.scenario_size.passed);
    // The first measured call must read the primed stable cross-session
    // prefix, not start from zero.
    assert!(report.calls[0].cache_read_tokens >= 160_000);
    assert_eq!(report.calls[0].miss_reason.as_deref(), Some("first_call"));
    for call in report.calls.iter().skip(1) {
        assert!(call.append_only, "turn {} was not append-only", call.turn);
        assert_ne!(
            call.miss_reason.as_deref(),
            Some("prefix_changed"),
            "turn {} reported a prefix break",
            call.turn
        );
        assert!(
            call.cache_miss_tokens <= 64 + 512,
            "turn {} miss {} exceeded the small-delta bound",
            call.turn,
            call.cache_miss_tokens
        );
    }
    assert!(report.aggregate.tail_delta_calls >= 45);
    assert_eq!(report.aggregate.provider_cache_miss_calls, 0);
}

#[test]
fn simulated_runs_are_byte_deterministic() {
    let first = report();
    let second = report();
    assert_eq!(first.to_json().unwrap(), second.to_json().unwrap());
    assert_eq!(first.markdown(), second.markdown());
    assert_eq!(first.fingerprint(), second.fingerprint());
    assert!(first.generated_at.is_none());
}

#[test]
fn block_cache_is_prefix_only_and_epoch_scoped() {
    let mut simulator = PrefixCacheSimulator::new();
    let stable = vec![1u64; 128];
    simulator.prime("epoch-a", &stable);
    let first = simulator.complete("epoch-a", &vec![1u64; 192]);
    assert_eq!(first.cache_read_tokens, 128);
    assert_eq!(first.cache_miss_tokens, 64);
    assert_eq!(first.hit_blocks, 2);
    // A different epoch cannot read the same bytes.
    let other = simulator.complete("epoch-b", &vec![1u64; 192]);
    assert_eq!(other.cache_read_tokens, 0);
    assert_eq!(other.cache_miss_tokens, 192);
    // A different prefix after the first block must stop at the divergence.
    let mut second = PrefixCacheSimulator::new();
    second.prime("epoch", &vec![7u64; 128]);
    let mut mutated = vec![7u64; 128];
    mutated.extend(vec![9u64; 64]);
    mutated.extend(vec![7u64; 64]);
    let usage = second.complete("epoch", &mutated);
    assert_eq!(usage.cache_read_tokens, 128);
}

#[test]
fn theoretical_max_uses_previous_prompt_not_the_current_tail() {
    let scenario = CacheBenchScenarioConfig {
        turns: 20,
        stable_prefix_tokens: 160_000,
        initial_context_tokens: 40_000,
        allow_unrealistic: true,
        kind: CacheBenchScenarioKind::WarmLongPrefix,
    };
    let report =
        run_simulated_benchmark(scenario, CacheBenchPricing::illustrative_default()).unwrap();
    for call in report.calls.iter().skip(1) {
        assert!(call.theoretical_max_hit_ratio > 0.99);
        assert!(call.cache_hit_ratio <= 1.0);
        assert!(call.theoretical_max_hit_ratio >= call.cache_hit_ratio - 0.02);
    }
}

#[test]
fn scenario_size_check_rejects_short_or_moving_benchmarks() {
    let short = CacheBenchScenarioConfig {
        turns: 50,
        stable_prefix_tokens: 1_000,
        initial_context_tokens: 1_000,
        allow_unrealistic: false,
        kind: CacheBenchScenarioKind::WarmLongPrefix,
    };
    assert!(long_prefix_scenario(short).is_err());

    let few_turns = CacheBenchScenarioConfig {
        turns: 4,
        stable_prefix_tokens: 180_000,
        initial_context_tokens: 20_000,
        allow_unrealistic: false,
        kind: CacheBenchScenarioKind::WarmLongPrefix,
    };
    assert!(long_prefix_scenario(few_turns).is_err());

    let huge_prefix = CacheBenchScenarioConfig {
        turns: 50,
        stable_prefix_tokens: 500_000,
        initial_context_tokens: 20_000,
        allow_unrealistic: false,
        kind: CacheBenchScenarioKind::WarmLongPrefix,
    };
    assert!(long_prefix_scenario(huge_prefix).is_err());

    // Explicit override produces a report but marks the check failed.
    let allowed = run_simulated_benchmark(
        CacheBenchScenarioConfig {
            turns: 50,
            stable_prefix_tokens: 1_000,
            initial_context_tokens: 1_000,
            allow_unrealistic: true,
            kind: CacheBenchScenarioKind::WarmLongPrefix,
        },
        CacheBenchPricing::illustrative_default(),
    )
    .unwrap();
    assert!(!allowed.scenario_size.passed);
    assert!(!allowed.scenario_size.reasons.is_empty());
}

#[test]
fn cost_estimate_charges_cache_reads_and_reports_savings() {
    let pricing = CacheBenchPricing::caller(Pricing::new(1.0, 2.0, 0.1, 0.5));
    let cost = pricing.estimate(1_000_000, 800_000, 0, 100_000);
    assert!((cost.input_cost_usd - 0.2).abs() < 1e-9);
    assert!((cost.cache_read_cost_usd - 0.08).abs() < 1e-9);
    assert!((cost.output_cost_usd - 0.2).abs() < 1e-9);
    assert!((cost.total_cost_usd - 0.48).abs() < 1e-9);
    assert!((cost.no_cache_cost_usd - 1.2).abs() < 1e-9);
    assert!((cost.estimated_savings_usd - 0.72).abs() < 1e-9);
}

#[test]
fn report_markdown_and_json_keep_the_denominator_explicit() {
    let report = report();
    let markdown = report.markdown();
    assert!(markdown.contains("cache read tokens"));
    assert!(markdown.contains("cache miss tokens"));
    assert!(markdown.contains("hit ratio"));
    assert!(markdown.contains("cache_read/input"));
    assert!(markdown.contains("Simulator tokens are deterministic"));
    let json = report.to_json_pretty().unwrap();
    let decoded: CacheBenchReport = serde_json::from_str(&json).unwrap();
    assert_aggregate_close(&decoded.aggregate, &report.aggregate);
    assert_eq!(decoded.fingerprint(), report.fingerprint());
}

#[test]
fn csv_contains_one_row_per_call_with_the_same_math() {
    let report = report();
    let csv = report.to_csv();
    let lines = csv.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), report.calls.len() + 1);
    assert!(lines[0].contains("cache_miss_tokens"));
    for (line, call) in lines.iter().skip(1).zip(&report.calls) {
        let fields = line.split(',').collect::<Vec<_>>();
        assert_eq!(fields[0], call.turn.to_string());
        assert_eq!(fields[1], call.input_tokens.to_string());
        assert_eq!(fields[5], call.cache_miss_tokens.to_string());
        assert_eq!(fields[9], call.append_only.to_string());
        assert_eq!(fields[10], call.miss_reason.as_deref().unwrap_or(""));
    }
    assert!(!csv.contains("function"));
}

#[test]
fn comparison_refuses_mismatched_pricing_and_can_label_override() {
    let left = report();
    let mut right = report();
    right.pricing = CacheBenchPricing::caller(Pricing::new(9.0, 9.0, 9.0, 9.0));
    let error = compare_cache_bench_reports(&left, &right, false).unwrap_err();
    assert!(error.to_string().contains("pricing"));
    let comparison = compare_cache_bench_reports(&left, &right, true).unwrap();
    assert!(!comparison.compatible);
    assert_eq!(comparison.refusals.len(), 1);
    assert_eq!(comparison.hit_ratio_delta, 0.0);

    let identical = compare_cache_bench_reports(&left, &report(), false).unwrap();
    assert!(identical.compatible);
    assert!(identical.refusals.is_empty());
    assert_eq!(identical.miss_token_delta, 0);
}

#[test]
fn artifacts_round_trip_through_the_loader() {
    let dir = TempDir::new().unwrap();
    let report = report();
    let (json, markdown) = write_cache_bench_artifacts(dir.path(), &report).unwrap();
    assert!(json.exists());
    assert!(markdown.exists());
    let loaded = load_cache_bench_report(&json).unwrap();
    assert_eq!(loaded.fingerprint(), report.fingerprint());
    assert_aggregate_close(&loaded.aggregate, &report.aggregate);
}

#[test]
fn report_validation_rejects_inconsistent_token_math() {
    let report = report();
    report.validate().unwrap();

    let mut bad_miss = report.clone();
    bad_miss.calls[3].cache_miss_tokens += 1;
    let error = bad_miss.validate().unwrap_err();
    assert!(error.to_string().contains("miss tokens"));

    let mut bad_aggregate = report.clone();
    bad_aggregate.aggregate.input_tokens += 1;
    let error = bad_aggregate.validate().unwrap_err();
    assert!(error.to_string().contains("aggregate"));

    let mut bad_first = report.clone();
    bad_first.calls[0].append_only = true;
    let error = bad_first.validate().unwrap_err();
    assert!(error.to_string().contains("first measured call"));

    let mut bad_acceptance = report.clone();
    bad_acceptance.acceptance.hit_ratio += 0.01;
    let error = bad_acceptance.validate().unwrap_err();
    assert!(error.to_string().contains("acceptance"));

    let mut bad_ratio = report.clone();
    bad_ratio.calls[2].cache_hit_ratio = 0.999;
    let error = bad_ratio.validate().unwrap_err();
    assert!(error.to_string().contains("hit ratio"));

    let mut bad_cost = report.clone();
    bad_cost.aggregate.cost.total_cost_usd += 1.0;
    let error = bad_cost.validate().unwrap_err();
    assert!(error.to_string().contains("cost"));
}

#[test]
fn loader_rejects_tampered_reports() {
    let dir = TempDir::new().unwrap();
    let report = report();
    let path = dir.path().join("tampered.json");
    let mut value = serde_json::to_value(&report).unwrap();
    value["aggregate"]["cache_miss_tokens"] = serde_json::json!(1);
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let error = load_cache_bench_report(&path).unwrap_err();
    assert!(error.to_string().contains("validation"));
}

#[test]
fn validation_rejects_empty_turn_order_and_bad_ratios() {
    let mut empty = report();
    empty.calls.clear();
    empty.turns = 0;
    empty.aggregate = CacheBenchAggregate::default();
    assert!(empty.validate().is_err());

    let mut wrong_turn = report();
    wrong_turn.calls[4].turn = 99;
    assert!(
        wrong_turn
            .validate()
            .unwrap_err()
            .to_string()
            .contains("turn")
    );

    let mut bad_read = report();
    bad_read.calls[1].cache_read_tokens = bad_read.calls[1].input_tokens + 1;
    assert!(bad_read.validate().is_err());

    let mut bad_output = report();
    bad_output.calls[2].cache_hit_ratio = 2.0;
    assert!(bad_output.validate().is_err());
}

#[test]
fn report_decodes_legacy_payload_without_l5_fields() {
    let report = report();
    let mut value = serde_json::to_value(&report).unwrap();
    value.as_object_mut().unwrap().remove("acceptance");
    value.as_object_mut().unwrap().remove("cross_session_warm");
    let legacy: CacheBenchReport = serde_json::from_value(value).unwrap();
    assert_eq!(legacy.acceptance, CacheBenchAcceptance::default());
    assert!(!legacy.cross_session_warm);
    assert_aggregate_close(&legacy.aggregate, &report.aggregate);
    // A legacy report still validates structurally; acceptance is recomputed
    // explicitly by the caller, never inferred from a missing field.
    legacy.validate().unwrap_err();
}

#[test]
fn loader_rejects_foreign_schema() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("bad.json");
    std::fs::write(&path, r#"{"schema":"other/v1"}"#).unwrap();
    let error = load_cache_bench_report(&path).unwrap_err();
    assert!(error.to_string().contains("schema"));
}

#[test]
fn cold_start_control_pays_the_prefix_and_does_not_claim_the_target() {
    let cold = run_simulated_benchmark(
        CacheBenchScenarioConfig {
            kind: CacheBenchScenarioKind::ColdLongPrefix,
            ..CacheBenchScenarioConfig::default()
        },
        CacheBenchPricing::illustrative_default(),
    )
    .unwrap();
    assert!(!cold.cross_session_warm);
    assert_eq!(cold.calls[0].cache_read_tokens, 0);
    assert!(
        cold.aggregate.cache_hit_ratio < report().aggregate.cache_hit_ratio,
        "cold start should be lower than the warm scenario"
    );
    assert_eq!(cold.acceptance.verdict, "cold_start_control");
    assert!(!cold.acceptance.provider_reported);
    assert_eq!(cold.acceptance.measurement, "simulator");
}

#[test]
fn prefix_break_control_reports_a_break_and_does_not_claim_the_target() {
    let broken = run_simulated_benchmark(
        CacheBenchScenarioConfig {
            kind: CacheBenchScenarioKind::PrefixBreakControl,
            ..CacheBenchScenarioConfig::default()
        },
        CacheBenchPricing::illustrative_default(),
    )
    .unwrap();
    assert!(broken.cross_session_warm);
    assert!(broken.aggregate.prefix_breaks >= 1);
    let break_call = broken
        .calls
        .iter()
        .find(|call| !call.append_only && call.turn > 1)
        .expect("a mid-session break call");
    assert_eq!(break_call.miss_reason.as_deref(), Some("prefix_changed"));
    assert!(broken.aggregate.cache_hit_ratio < report().aggregate.cache_hit_ratio);
    assert_eq!(broken.acceptance.verdict, "prefix_break_control");
}

#[test]
fn acceptance_verdict_distinguishes_stretch_pass_and_unrealistic() {
    let warm = report();
    assert_eq!(warm.acceptance.measurement, "simulator");
    assert!(warm.acceptance.scenario_shape_valid);
    assert!(warm.acceptance.cross_session_warm);
    assert!(warm.acceptance.floor_met);
    assert!(matches!(
        warm.acceptance.verdict.as_str(),
        "pass" | "stretch"
    ));

    let unrealistic = run_simulated_benchmark(
        CacheBenchScenarioConfig {
            stable_prefix_tokens: 1_000,
            initial_context_tokens: 1_000,
            allow_unrealistic: true,
            ..CacheBenchScenarioConfig::default()
        },
        CacheBenchPricing::illustrative_default(),
    )
    .unwrap();
    assert_eq!(unrealistic.acceptance.verdict, "unrealistic");
    assert!(!unrealistic.acceptance.scenario_shape_valid);
}

#[tokio::test]
async fn matrix_writes_warm_cold_and_prefix_break_evidence() {
    let dir = TempDir::new().unwrap();
    let code = execute(
        CacheBenchCommand::Matrix(CacheBenchMatrixArgs {
            turns: 50,
            stable_prefix_tokens: 180_000,
            initial_context_tokens: 20_000,
            output: Some(dir.path().to_path_buf()),
        }),
        true,
    )
    .await
    .unwrap();
    assert_eq!(code, 0);
    for suffix in ["warm", "cold-start", "prefix-break"] {
        assert!(
            dir.path()
                .join(format!("cache-bench-matrix-{suffix}.json"))
                .exists()
        );
        assert!(
            dir.path()
                .join(format!("cache-bench-matrix-{suffix}.md"))
                .exists()
        );
    }
    let warm: CacheBenchReport = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join("cache-bench-matrix-warm.json")).unwrap(),
    )
    .unwrap();
    let cold: CacheBenchReport = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join("cache-bench-matrix-cold-start.json")).unwrap(),
    )
    .unwrap();
    let broken: CacheBenchReport = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join("cache-bench-matrix-prefix-break.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(warm.acceptance.verdict, "pass");
    assert_eq!(cold.acceptance.verdict, "cold_start_control");
    assert_eq!(broken.acceptance.verdict, "prefix_break_control");
    assert!(warm.aggregate.cache_hit_ratio > cold.aggregate.cache_hit_ratio);
    assert!(warm.aggregate.prefix_breaks == 0);
    assert!(broken.aggregate.prefix_breaks > 0);
    let matrix = CacheBenchMatrix {
        schema: "verlinpark.cache-bench-matrix/v1".into(),
        scenarios: vec![warm, cold, broken],
    };
    let markdown = matrix.markdown();
    assert!(markdown.contains("long_prefix_50_turn_development"));
    assert!(markdown.contains("cold_start_control"));
    assert!(markdown.contains("prefix_break_control"));
}

#[tokio::test]
async fn verify_command_gates_floor_provider_and_controls() {
    let dir = TempDir::new().unwrap();
    let report = report();
    let (json, _) = write_cache_bench_artifacts(dir.path(), &report).unwrap();
    let pass = execute(
        CacheBenchCommand::Verify {
            file: json.clone(),
            min_hit_ratio: 0.995,
            require_provider_reported: false,
            allow_control: false,
        },
        true,
    )
    .await
    .unwrap();
    assert_eq!(pass, 0);
    let below = execute(
        CacheBenchCommand::Verify {
            file: json.clone(),
            min_hit_ratio: 0.9999,
            require_provider_reported: false,
            allow_control: false,
        },
        true,
    )
    .await
    .unwrap();
    assert_eq!(below, 1);
    let provider_required = execute(
        CacheBenchCommand::Verify {
            file: json,
            min_hit_ratio: 0.995,
            require_provider_reported: true,
            allow_control: false,
        },
        true,
    )
    .await
    .unwrap();
    assert_eq!(provider_required, 1);
    let invalid = execute(
        CacheBenchCommand::Verify {
            file: dir.path().join("missing.json"),
            min_hit_ratio: 0.995,
            require_provider_reported: false,
            allow_control: false,
        },
        true,
    )
    .await;
    assert!(invalid.is_err());
}

#[test]
fn live_target_constant_matches_the_plan() {
    assert!((LONG_PREFIX_HIT_TARGET - 0.995).abs() < 1e-12);
    assert!((LONG_PREFIX_STRETCH_TARGET - 0.9984).abs() < 1e-12);
}

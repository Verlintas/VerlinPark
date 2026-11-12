//! Deterministic cost-per-solved-task benchmark harness for VerlinPark.
//!
//! The crate is usable in three ways:
//!
//! * as a library (`run_scenarios`, [`report::BenchReport`]);
//! * through the standalone `vp-bench` binary;
//! * through the additive `vp bench` CLI namespace.
//!
//! It runs ordinary sessions and stored TeamPlans through the normal
//! Verifier/Integrator path. It does not add a private merge shortcut, hidden
//! model routing, or an alternate approval gate. The default provider is a
//! deterministic in-process fake; real-provider mode requires an explicit flag
//! and environment gate.

pub mod cache_bench;
pub mod cli;
pub mod compare;
pub mod fixture;
pub mod metrics;
pub mod provider;
pub mod reliability;
pub mod report;
pub mod runner;
pub mod scenario;

pub use cache_bench::{
    CACHE_BENCH_SCHEMA, CacheBenchAcceptance, CacheBenchAggregate, CacheBenchCall,
    CacheBenchCommand, CacheBenchComparison, CacheBenchMatrix, CacheBenchMatrixArgs,
    CacheBenchMode, CacheBenchPricing, CacheBenchRefusal, CacheBenchReport, CacheBenchRunArgs,
    CacheBenchScenario, CacheBenchScenarioConfig, CacheBenchScenarioKind, CostEstimate,
    LONG_PREFIX_HIT_TARGET, LONG_PREFIX_STRETCH_TARGET, LiveCacheBenchSettings, LiveRunOptions,
    PrefixCacheSimulator, SIMULATOR_BLOCK_SIZE, ScenarioSizeCheck, compare_cache_bench_reports,
    execute as execute_cache_bench, load_cache_bench_report, long_prefix_scenario,
    run_live_benchmark, run_live_benchmark_with_options, run_live_benchmark_with_provider,
    run_simulated_benchmark, write_cache_bench_artifacts,
};
pub use compare::{
    CompareOptions, CompareRefusal, ComparisonMismatch, ComparisonReport, compare_reports,
};
pub use fixture::{
    AcceptanceHarness, AcceptanceSpec, BenchWorkspace, FixturePlan, materialize_repo,
    prepare_fixture,
};
pub use metrics::{Pricing, TokenUsage, cost_per_solved, priced_cost, tokens_per_solved};
pub use provider::{BenchBehavior, FakeProvider, FakeUsageModel, RealProviderSettings};
pub use reliability::{
    CoordinationUsage, LuckyPassSummary, LuckySignal, ReliabilityArtifacts, ReliabilityReport,
    ReliabilitySelection, ReliabilitySummary, ScenarioReliability, TrialEvidence, TrialOutcome,
    analyze_events, flake_rate, from_saved_report, pass_at_k, pass_power_k, resolve_k,
    success_variance, write_reliability_artifacts,
};
pub use report::{
    BenchReport, BenchSelection, HostMetadata, ProviderMetadata, RunRecord, ScenarioAggregate,
    load_report, parse_report, write_artifacts,
};
pub use runner::{BenchRunOptions, ProviderSelection, run_scenarios};
pub use scenario::{
    Availability, DirectTask, ExpectedMerges, FixtureKind, PlanSpec, PlanTaskSpec, Requirements,
    Scenario, ScenarioConfig, VerifierRequirement,
};

/// The benchmark report schema version.
pub const SCHEMA_VERSION: u32 = report::SCHEMA_VERSION;

/// The stable report caveat.
pub const REPORT_CAVEAT: &str = report::REPORT_CAVEAT;

/// Convenience helper for `cache_hit_ratio` without importing [`metrics`]
/// directly. Zero denominator returns `None`.
pub fn cache_hit_ratio(
    input_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
) -> Option<f64> {
    TokenUsage {
        input_tokens,
        output_tokens: 0,
        cache_read_tokens,
        cache_write_tokens,
        calls: 0,
    }
    .cache_hit_ratio()
}

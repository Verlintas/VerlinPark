//! Scenario execution through the ordinary VerlinPark runtime.
//!
//! The runner creates a disposable project/session, injects only the configured
//! fake provider, and then uses the same `AgentService`, `TeamPlan`, Verifier,
//! and Integrator paths as a normal session. It does not call internal merge or
//! approval helpers directly.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::time::sleep;
use vp_core::Engine;
use vp_core::agent::{AgentConfig, AgentService, CapacityController, ScopeLocks};
use vp_core::tools::ToolRegistry;
use vp_protocol::types::{
    ApprovalMode, PlanDepartment, PlanTask, RoleModelConfig, RoleModels, Session, TeamPlan,
    ToolOutputPolicy,
};
use vp_providers::Provider;
use vp_store::Store;

use crate::fixture::{BenchWorkspace, prepare_fixture};
use crate::metrics::{Pricing, TokenUsage};
use crate::provider::{
    BenchBehavior, FakeProvider, FakeUsageModel, MeteringProvider, RealProviderSettings,
};
use crate::report::{
    AcceptanceResult, HostMetadata, MergeMetrics, ProviderMetadata, RunMetrics, RunRecord,
    UnsolvedReason, VerdictRecord, VerifierMetrics, comparison_key,
};
use crate::scenario::{ExpectedMerges, FixtureKind, Scenario};

/// The C1 tool-output pager is part of the runtime. Paging-on scenarios force
/// a small spill threshold and must observe `tool.output_spilled`; the runner
/// still promotes the capability from observed events so a mixed/older build
/// cannot claim support without evidence.
pub const TOOL_OUTPUT_PAGING_SUPPORTED: bool = true;

/// Provider selection for a benchmark run. Fake is the default everywhere.
#[derive(Clone)]
pub enum ProviderSelection {
    Fake,
    Real(RealProviderSettings),
}

/// Knobs shared by every scenario in one invocation.
#[derive(Clone)]
pub struct BenchRunOptions {
    pub repetitions: usize,
    pub keep_worktrees: bool,
    pub provider: ProviderSelection,
    pub acceptance_timeout_secs: u64,
    /// Optional override for each scenario's wall-clock timeout.
    pub timeout_override_secs: Option<u64>,
}

impl BenchRunOptions {
    pub fn fake(repetitions: usize) -> Self {
        Self {
            repetitions,
            keep_worktrees: false,
            provider: ProviderSelection::Fake,
            acceptance_timeout_secs: 60,
            timeout_override_secs: None,
        }
    }
}

/// Validate and execute every selected scenario/repetition sequentially.
///
/// Sequential execution is intentional: each repetition owns a SQLite data
/// directory, and the benchmark must not let two scenarios compete for the
/// daemon-wide worker capacity it is trying to measure.
pub async fn run_scenarios(
    scenarios: &[Scenario],
    options: &BenchRunOptions,
) -> Result<Vec<RunRecord>> {
    if scenarios.is_empty() {
        bail!("select at least one benchmark scenario");
    }
    if options.repetitions == 0 || options.repetitions > 100 {
        bail!("repetitions must be 1-100");
    }
    if options.acceptance_timeout_secs == 0 || options.acceptance_timeout_secs > 300 {
        bail!("acceptance timeout must be 1-300 seconds");
    }
    if let Some(timeout) = options.timeout_override_secs
        && (timeout == 0 || timeout > 3600)
    {
        bail!("scenario timeout override must be 1-3600 seconds");
    }
    if matches!(options.provider, ProviderSelection::Real(_))
        && !RealProviderSettings::gate_enabled()
    {
        bail!(
            "real-provider mode is disabled; set {} = 1 and pass --allow-real-provider",
            RealProviderSettings::GATE_ENV
        );
    }
    let mut ids = HashSet::new();
    for scenario in scenarios {
        scenario.validate().map_err(anyhow::Error::msg)?;
        if let Some(reason) = scenario.availability.reason() {
            bail!(
                "scenario '{}' is unavailable in this build: {reason}",
                scenario.id
            );
        }
        if !ids.insert(scenario.id.as_str()) {
            bail!("scenario '{}' was selected more than once", scenario.id);
        }
    }
    let mut runs = Vec::with_capacity(scenarios.len().saturating_mul(options.repetitions));
    for scenario in scenarios {
        for repetition in 1..=options.repetitions {
            runs.push(
                run_one(scenario, repetition, options)
                    .await
                    .with_context(|| {
                        format!("running scenario '{}' repetition {repetition}", scenario.id)
                    })?,
            );
        }
    }
    Ok(runs)
}

struct BuiltProvider {
    provider: Arc<dyn Provider>,
    metadata: ProviderMetadata,
    fake: Option<Arc<FakeProvider>>,
    metering: Arc<MeteringProvider>,
    model: String,
}

/// Run one scenario repetition and collect a report record.
async fn run_one(
    scenario: &Scenario,
    repetition: usize,
    options: &BenchRunOptions,
) -> Result<RunRecord> {
    let workspace = BenchWorkspace::create()?;
    let fixture_plan = scenario.fixture_plan();
    let acceptance = prepare_fixture(&fixture_plan, &workspace)?;
    let store = Store::open(workspace.data_dir())?;
    let engine = Engine::new(store);
    let project = engine
        .add_project(
            workspace.repo().to_string_lossy().to_string(),
            Some("vp-bench fixture".to_string()),
        )
        .await?;
    apply_scenario_tool_output_policy(scenario, &engine, &project.id)?;
    let built = build_provider(scenario, options)?;
    let config = build_agent_config(scenario, &engine, &built)?;
    let pricing = scenario.config.pricing;
    let service = AgentService::configured(engine.clone(), config);
    let started_at = now_rfc3339();
    let started = Instant::now();
    let session = engine
        .create_session(project.id.clone(), scenario.prompt.clone(), None)
        .await?;
    engine
        .create_leader_agent(&session.id, &built.model)
        .await?;
    preload_messages(scenario, &engine, &session.id).await?;

    if let Some(plan) = &scenario.plan {
        let team_plan = build_team_plan(plan)?;
        engine.set_team_plan(&session.id, &team_plan).await?;
        if !service.resume_plan(&session.id).await? {
            bail!("the stored benchmark plan did not start");
        }
    } else {
        service.start(&session.id, &scenario.prompt).await?;
    }

    let timeout_secs = options
        .timeout_override_secs
        .unwrap_or(scenario.config.timeout_secs);
    let (terminal, timed_out) = wait_for_terminal(
        &engine,
        &service,
        &project.id,
        &session.id,
        Duration::from_secs(timeout_secs),
    )
    .await?;
    let merge_entries = wait_for_merges(
        &engine,
        &session.id,
        scenario.requirements.expected_merges.count().unwrap_or(0),
        Duration::from_secs(scenario.config.merge_timeout_secs),
    )
    .await?;

    let worktree = terminal
        .worktree
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("benchmark session has no worktree"))?;
    let acceptance_execution = acceptance
        .run(
            Path::new(worktree),
            Duration::from_secs(options.acceptance_timeout_secs),
        )
        .await?;
    let acceptance_result =
        AcceptanceResult::from_execution(acceptance_execution, acceptance.fingerprint.clone());

    let events = engine.list_events(session.id.clone(), 0, 50_000).await?;
    let event_stats = EventStats::from_events(&events);
    let ledger = engine.usage_ledger(&session.id).await?;
    let queue = engine.task_queue(None, Some(session.id.clone())).await?;
    let plan_tasks = queue.tasks;
    let fake_summary = built.fake.as_ref().map(|provider| provider.summary());
    let provider_peak_workers = built.metering.peak_workers();

    let verifier = VerifierMetrics::from_verdicts(event_stats.verdicts.clone());
    let expected_merges = scenario.requirements.expected_merges.count();
    let merged = merge_entries
        .iter()
        .filter(|entry| entry.status == "merged")
        .count() as u64;
    let failed = merge_entries
        .iter()
        .filter(|entry| matches!(entry.status.as_str(), "failed" | "cancelled"))
        .count() as u64;
    let conflicts = merge_entries
        .iter()
        .filter(|entry| entry.status == "conflict")
        .count() as u64;
    let merge = MergeMetrics::new(
        merge_entries.len() as u64,
        merged,
        failed,
        conflicts,
        expected_merges,
    );

    let provider_errors = fake_summary
        .as_ref()
        .map(|summary| summary.errors)
        .unwrap_or(0);
    let (plan_total, plan_completed, plan_failed, plan_running) = plan_task_counts(&plan_tasks);
    let paging_supported = TOOL_OUTPUT_PAGING_SUPPORTED || event_stats.spills > 0;
    let ledger_usage = TokenUsage {
        input_tokens: ledger.totals.input_tokens,
        output_tokens: ledger.totals.output_tokens,
        cache_read_tokens: ledger.totals.cache_read_tokens,
        cache_write_tokens: ledger.totals.cache_write_tokens,
        calls: ledger.totals.calls,
    };
    let mut metrics = RunMetrics {
        wall_clock_ms: started.elapsed().as_millis() as u64,
        input_tokens: ledger_usage.input_tokens,
        output_tokens: ledger_usage.output_tokens,
        cache_read_tokens: ledger_usage.cache_read_tokens,
        cache_write_tokens: ledger_usage.cache_write_tokens,
        total_tokens: ledger_usage.total_input_and_output(),
        provider_calls: fake_summary
            .as_ref()
            .map(|summary| summary.calls)
            .unwrap_or(ledger_usage.calls),
        usage_calls: ledger_usage.calls,
        estimated_usage: ledger.entries.iter().any(|entry| entry.estimated),
        cost_usd: 0.0,
        ledger_cost_usd: ledger.totals.cost_usd,
        cost_reconciled: false,
        cost_per_solved_task_usd: None,
        tokens_per_solved_task: None,
        cache_hit_ratio: None,
        tool_calls: event_stats.tool_calls,
        tool_errors: event_stats.tool_errors,
        tool_output_bytes: event_stats.tool_output_bytes,
        tool_output_spills: paging_supported.then_some(event_stats.spills),
        tool_output_paging_supported: paging_supported,
        compactions: event_stats.compactions,
        compaction_failures: event_stats.compaction_failures,
        retries: event_stats.retries,
        human_interventions: event_stats.human_interventions,
        scouts: event_stats.scouts,
        peak_workers: event_stats.peak_workers.max(provider_peak_workers),
        agent_failures: event_stats.agent_failures,
        plan_tasks_total: plan_total,
        plan_tasks_completed: plan_completed,
        plan_tasks_failed: plan_failed,
        plan_tasks_running: plan_running,
    };
    let harness_fault = fake_summary
        .as_ref()
        .filter(|summary| summary.exhausted)
        .map(|summary| {
            format!(
                "the deterministic fake provider exhausted its response script after {} call(s); \
                 this is a harness fault, not a benchmark result",
                summary.calls
            )
        });
    let mut reasons = classify(ClassificationInput {
        scenario,
        session: &terminal,
        timed_out,
        acceptance: &acceptance_result,
        verifier: &verifier,
        merge: &merge,
        metrics: &metrics,
        provider_errors,
        plan_tasks: &plan_tasks,
    });
    if let Some(fault) = &harness_fault {
        reasons.push(UnsolvedReason::new("harness_fault", fault.clone()));
    }
    let solved = reasons.is_empty();
    metrics.recompute_cost(&pricing, solved);
    let comparison = comparison_key(scenario, &built.metadata, &pricing);
    let mut record = RunRecord {
        run_id: format!(
            "{}-r{}-{}",
            scenario.id,
            repetition,
            &uuid::Uuid::now_v7().simple().to_string()[..8]
        ),
        scenario_id: scenario.id.clone(),
        scenario_title: scenario.title.clone(),
        repetition,
        started_at,
        finished_at: now_rfc3339(),
        session_id: session.id.clone(),
        session_status: terminal.status.clone(),
        timed_out,
        solved,
        acceptance: acceptance_result,
        verifier,
        merge,
        metrics,
        unsolved_reasons: reasons,
        harness_fault,
        comparison,
        pricing,
        provider: built.metadata.clone(),
        target_paths: scenario.target_paths(),
        retained_workspace: None,
    };
    if options.keep_worktrees {
        let retained = workspace.keep();
        record.retained_workspace = Some(retained.to_string_lossy().to_string());
    }
    drop(service);
    drop(engine);
    Ok(record)
}

fn apply_scenario_tool_output_policy(
    scenario: &Scenario,
    engine: &Engine,
    project_id: &str,
) -> Result<()> {
    let config = &scenario.config;
    if config.tool_output_enabled.is_none()
        && config.tool_output_spill_threshold_bytes.is_none()
        && config.tool_output_preview_bytes.is_none()
    {
        return Ok(());
    }
    let store = engine.store();
    let mut policy: ToolOutputPolicy = store.get_tool_output_policy(project_id)?;
    if let Some(enabled) = config.tool_output_enabled {
        policy.enabled = enabled;
    }
    if let Some(bytes) = config.tool_output_spill_threshold_bytes {
        policy.spill_threshold_bytes = bytes;
    }
    if let Some(bytes) = config.tool_output_preview_bytes {
        policy.preview_bytes = bytes;
    }
    store.set_tool_output_policy(project_id, &policy)?;
    Ok(())
}

fn build_provider(scenario: &Scenario, options: &BenchRunOptions) -> Result<BuiltProvider> {
    match &options.provider {
        ProviderSelection::Fake => {
            let behavior = if let Some(plan) = &scenario.plan {
                let targets = plan
                    .tasks
                    .iter()
                    .map(|task| (task.target_path.clone(), task.target_content.clone()))
                    .collect::<BTreeMap<_, _>>();
                BenchBehavior::plan(targets, scenario.config.fake_verifier_approves)
            } else if scenario.fixture == FixtureKind::Scout {
                BenchBehavior::scout()
            } else {
                let direct = scenario
                    .direct
                    .as_ref()
                    .context("direct scenario has no direct task")?;
                BenchBehavior::direct(
                    &direct.target_path,
                    &direct.target_content,
                    direct.prefetch_path.clone(),
                )
            };
            let usage_model = FakeUsageModel::benchmark_default();
            let usage_source = usage_model.source().to_string();
            let fake = Arc::new(FakeProvider::bench(
                scenario.config.model.clone(),
                behavior,
                usage_model,
                Duration::from_millis(scenario.config.provider_latency_ms),
                scenario.config.max_provider_calls,
            ));
            let inner: Arc<dyn Provider> = fake.clone();
            let metering = Arc::new(MeteringProvider::new(inner));
            let provider: Arc<dyn Provider> = metering.clone();
            Ok(BuiltProvider {
                provider,
                metadata: ProviderMetadata::fake(
                    "vp-bench-fake",
                    scenario.config.model.clone(),
                    usage_source,
                    scenario.config.provider_latency_ms,
                ),
                fake: Some(fake),
                metering,
                model: scenario.config.model.clone(),
            })
        }
        ProviderSelection::Real(settings) => {
            if !RealProviderSettings::gate_enabled() {
                bail!(
                    "real-provider mode is disabled; set {} = 1 and pass --allow-real-provider",
                    RealProviderSettings::GATE_ENV
                );
            }
            let metadata =
                ProviderMetadata::real(settings.provider_id.clone(), settings.model.clone());
            let metering = Arc::new(MeteringProvider::new(settings.clone().into_provider()));
            let provider: Arc<dyn Provider> = metering.clone();
            Ok(BuiltProvider {
                provider,
                metadata,
                fake: None,
                metering,
                model: settings.model.clone(),
            })
        }
    }
}

fn build_agent_config(
    scenario: &Scenario,
    engine: &Engine,
    built: &BuiltProvider,
) -> Result<AgentConfig> {
    let config = &scenario.config;
    let model = &built.model;
    let role = role_model(model, &config.pricing, config.context_window);
    let role_models = RoleModels {
        leader: role.clone(),
        manager: role.clone(),
        worker: role.clone(),
        verifier: role.clone(),
        labour: role.clone(),
        scout: role,
    };
    Ok(AgentConfig {
        provider: built.provider.clone(),
        provider_name: built.metadata.id.clone(),
        model: model.clone(),
        role_models,
        base_url: None,
        max_turns: config.max_turns,
        max_tokens: Some(2_048),
        max_managers: 3,
        max_workers: config.max_workers,
        max_labour: config.max_labour,
        auto_expand_workers: false,
        integration_command: None,
        tools: ToolRegistry::default_tools(),
        max_active_sessions: 2,
        scope_locks: Arc::new(ScopeLocks::with_store(engine.store().clone())),
        capacity: Arc::new(CapacityController::new(
            config.max_workers,
            config.max_labour,
        )),
        provider_idle_timeout_secs: 30,
        question_timeout_secs: 30,
        approval_mode: ApprovalMode::Unattended,
        approval_timeout_secs: 30,
        compact_after_tokens: config.compact_after_tokens,
        compact_keep_messages: config.compact_keep_messages,
        compact_keep_tokens: 15_000,
        compact_prepare_ratio: 0.6,
        context_window: config.context_window,
    })
}

fn role_model(model: &str, pricing: &Pricing, context_window: u64) -> RoleModelConfig {
    RoleModelConfig {
        model: Some(model.to_string()),
        input_usd_per_million: Some(pricing.input_usd_per_million),
        output_usd_per_million: Some(pricing.output_usd_per_million),
        cache_read_usd_per_million: Some(pricing.cache_read_usd_per_million),
        cache_write_usd_per_million: Some(pricing.cache_write_usd_per_million),
        context_window: (context_window > 0).then_some(context_window),
    }
}

fn build_team_plan(plan: &crate::scenario::PlanSpec) -> Result<TeamPlan> {
    Ok(TeamPlan {
        goal: plan.goal.clone(),
        departments: vec![PlanDepartment {
            name: plan.department.clone(),
            manager_title: plan.manager_title.clone(),
            profile: plan.profile.clone(),
            objective: plan.objective.clone(),
            token_budget: None,
            cost_budget_usd: None,
        }],
        tasks: plan
            .tasks
            .iter()
            .map(|task| PlanTask {
                id: task.id.clone(),
                department: plan.department.clone(),
                title: task.title.clone(),
                kind: task.kind.clone(),
                priority: 0,
                profile: Some(plan.profile.clone()),
                deps: Vec::new(),
                write_scope: task.write_scope.clone(),
                acceptance: task.acceptance.clone(),
                token_budget: None,
                cost_budget_usd: None,
            })
            .collect(),
    })
}

async fn preload_messages(scenario: &Scenario, engine: &Engine, session_id: &str) -> Result<()> {
    let count = scenario.config.preload_messages;
    if count == 0 {
        return Ok(());
    }
    let chars = scenario.config.preload_message_chars.max(1);
    for index in 0..count {
        let role = if index % 2 == 0 { "user" } else { "assistant" };
        let content = format!(
            "bench context message {index:03}: {}",
            "deterministic-history ".repeat(chars / 22 + 1)
        );
        engine
            .append_message(session_id, role, &content, serde_json::json!([]), None)
            .await?;
    }
    Ok(())
}

async fn wait_for_terminal(
    engine: &Engine,
    service: &AgentService,
    project_id: &str,
    session_id: &str,
    timeout: Duration,
) -> Result<(Session, bool)> {
    let deadline = Instant::now() + timeout;
    let mut latest = load_session(engine, project_id, session_id).await?;
    loop {
        if matches!(latest.status.as_str(), "idle" | "failed" | "cancelled") {
            sleep(Duration::from_millis(40)).await;
            return Ok((latest, false));
        }
        if Instant::now() >= deadline {
            let _ = service.cancel_session_run(session_id, None).await;
            let _ = service.cancel(session_id);
            let cancel_deadline = Instant::now() + Duration::from_secs(5);
            let mut last = latest;
            while Instant::now() < cancel_deadline {
                sleep(Duration::from_millis(50)).await;
                last = match load_session(engine, project_id, session_id).await {
                    Ok(session) => session,
                    Err(_) => last,
                };
                if matches!(last.status.as_str(), "idle" | "failed" | "cancelled") {
                    break;
                }
            }
            return Ok((last, true));
        }
        sleep(Duration::from_millis(25)).await;
        latest = load_session(engine, project_id, session_id).await?;
    }
}

async fn load_session(engine: &Engine, project_id: &str, session_id: &str) -> Result<Session> {
    let sessions = engine.list_sessions(Some(project_id.to_string())).await?;
    sessions
        .into_iter()
        .find(|session| session.id == session_id)
        .ok_or_else(|| anyhow::anyhow!("benchmark session {session_id} disappeared"))
}

async fn wait_for_merges(
    engine: &Engine,
    session_id: &str,
    expected: usize,
    max_wait: Duration,
) -> Result<Vec<vp_protocol::types::MergeQueueEntry>> {
    if expected == 0 {
        return engine.list_merge_queue(session_id).await;
    }
    let deadline = Instant::now() + max_wait;
    let mut last_len = usize::MAX;
    let mut last_change = Instant::now();
    loop {
        let entries = engine.list_merge_queue(session_id).await?;
        let terminal = entries.iter().all(|entry| {
            matches!(
                entry.status.as_str(),
                "merged" | "failed" | "conflict" | "cancelled"
            )
        });
        if entries.len() >= expected && terminal {
            return Ok(entries);
        }
        if entries.len() != last_len {
            last_len = entries.len();
            last_change = Instant::now();
        }
        if Instant::now() >= deadline
            || (entries.len() < expected && last_change.elapsed() > Duration::from_secs(2))
        {
            return Ok(entries);
        }
        sleep(Duration::from_millis(25)).await;
    }
}

#[derive(Default)]
struct EventStats {
    tool_calls: u64,
    tool_errors: u64,
    tool_output_bytes: u64,
    spills: u64,
    compactions: u64,
    compaction_failures: u64,
    retries: u64,
    human_interventions: u64,
    scouts: u64,
    peak_workers: u64,
    agent_failures: u64,
    verdicts: Vec<VerdictRecord>,
}

impl EventStats {
    fn from_events(events: &[vp_protocol::types::Event]) -> Self {
        let mut stats = Self::default();
        let mut active_workers = BTreeSet::new();
        let mut verifier_events = Vec::new();
        let mut worker_verdict_events = Vec::new();
        for event in events {
            match event.kind.as_str() {
                "tool.finished" => {
                    stats.tool_calls = stats.tool_calls.saturating_add(1);
                    if event.payload["is_error"].as_bool().unwrap_or(false) {
                        stats.tool_errors = stats.tool_errors.saturating_add(1);
                    }
                    if let Some(output) = event.payload["output"].as_str() {
                        stats.tool_output_bytes =
                            stats.tool_output_bytes.saturating_add(output.len() as u64);
                    }
                }
                "tool.output_spilled" => {
                    stats.spills = stats.spills.saturating_add(1);
                }
                "context.compacted" => {
                    stats.compactions = stats.compactions.saturating_add(1);
                }
                "context.compact_failed" => {
                    stats.compaction_failures = stats.compaction_failures.saturating_add(1);
                }
                "agent.stream_retry" => {
                    stats.retries = stats.retries.saturating_add(1);
                }
                "approval.resolved" | "question.answered" => {
                    stats.human_interventions = stats.human_interventions.saturating_add(1);
                }
                "scout.completed" => {
                    stats.scouts = stats.scouts.saturating_add(1);
                }
                "agent.failed" => {
                    stats.agent_failures = stats.agent_failures.saturating_add(1);
                }
                "agent.started" if event.payload["agent_role"] == "worker" => {
                    if let Some(id) = event_agent_id(event) {
                        active_workers.insert(id);
                        stats.peak_workers = stats.peak_workers.max(active_workers.len() as u64);
                    }
                }
                "agent.finished" if event.payload["agent_role"] == "worker" => {
                    if let Some(id) = event_agent_id(event) {
                        active_workers.remove(&id);
                    }
                    if has_bool(&event.payload, "approved") {
                        worker_verdict_events.push(verdict_record(event));
                    }
                }
                "agent.finished"
                    if event.payload["agent_role"] == "verifier"
                        && has_bool(&event.payload, "approved") =>
                {
                    verifier_events.push(verdict_record(event));
                }
                _ => {}
            }
        }
        stats.verdicts = if verifier_events.is_empty() {
            worker_verdict_events
        } else {
            verifier_events
        };
        stats
    }
}

fn has_bool(payload: &serde_json::Value, field: &str) -> bool {
    payload
        .get(field)
        .and_then(|value| value.as_bool())
        .is_some()
}

fn event_agent_id(event: &vp_protocol::types::Event) -> Option<String> {
    event
        .payload
        .get("agent_id")
        .and_then(|value| value.as_str())
        .or_else(|| {
            event
                .payload
                .get("agent_name")
                .and_then(|value| value.as_str())
        })
        .map(str::to_string)
}

fn verdict_record(event: &vp_protocol::types::Event) -> VerdictRecord {
    VerdictRecord {
        task_id: event
            .payload
            .get("task_id")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        approved: event.payload["approved"].as_bool().unwrap_or(false),
        summary: event
            .payload
            .get("summary")
            .and_then(|value| value.as_str())
            .unwrap_or("verdict recorded")
            .to_string(),
    }
}

fn plan_task_counts(tasks: &[vp_protocol::types::ScheduledTask]) -> (u64, u64, u64, u64) {
    let mut total = 0u64;
    let mut completed = 0u64;
    let mut failed = 0u64;
    let mut running = 0u64;
    for task in tasks {
        total = total.saturating_add(1);
        match task.status.as_str() {
            "completed" => completed = completed.saturating_add(1),
            "failed" | "blocked" | "cancelled" => failed = failed.saturating_add(1),
            "running" | "claimed" => running = running.saturating_add(1),
            _ => {}
        }
    }
    (total, completed, failed, running)
}

struct ClassificationInput<'a> {
    scenario: &'a Scenario,
    session: &'a Session,
    timed_out: bool,
    acceptance: &'a AcceptanceResult,
    verifier: &'a VerifierMetrics,
    merge: &'a MergeMetrics,
    metrics: &'a RunMetrics,
    provider_errors: u64,
    plan_tasks: &'a [vp_protocol::types::ScheduledTask],
}

fn classify(input: ClassificationInput<'_>) -> Vec<UnsolvedReason> {
    let ClassificationInput {
        scenario,
        session,
        timed_out,
        acceptance,
        verifier,
        merge,
        metrics,
        provider_errors,
        plan_tasks,
    } = input;
    let mut reasons = Vec::new();
    if timed_out {
        reasons.push(UnsolvedReason::new(
            "timed_out",
            format!(
                "scenario did not settle within {}s",
                scenario.config.timeout_secs
            ),
        ));
    }
    if session.status != "idle" {
        reasons.push(UnsolvedReason::new(
            format!("session_{}", session.status),
            format!("session ended in status '{}'", session.status),
        ));
    }
    if !acceptance.passed {
        let detail = if acceptance.timed_out {
            format!(
                "acceptance timed out after {} ms: {}",
                acceptance.duration_ms, acceptance.stderr_preview
            )
        } else {
            format!(
                "acceptance exited {:?}: {} {}",
                acceptance.exit_code,
                acceptance.stderr_preview.trim(),
                acceptance.stdout_preview.trim()
            )
        };
        reasons.push(UnsolvedReason::new(
            "acceptance_failed",
            truncate_detail(&detail, 500),
        ));
    }
    if provider_errors > 0 {
        reasons.push(UnsolvedReason::new(
            "provider_error",
            format!("provider reported {provider_errors} error(s)"),
        ));
    }
    if scenario.requirements.require_no_agent_failures && metrics.agent_failures > 0 {
        reasons.push(UnsolvedReason::new(
            "agent_failed",
            format!("{} agent failure event(s)", metrics.agent_failures),
        ));
    }
    match scenario.requirements.expected_merges {
        ExpectedMerges::None => {
            if merge.entries > 0 {
                reasons.push(UnsolvedReason::new(
                    "unexpected_merge",
                    format!("{} merge queue entry/entries were created", merge.entries),
                ));
            }
        }
        ExpectedMerges::Exactly(expected) => {
            if merge.entries < expected as u64 {
                reasons.push(UnsolvedReason::new(
                    "merge_expected_missing",
                    format!("expected {expected} merge(s), observed {}", merge.entries),
                ));
            } else if merge.entries > expected as u64 {
                reasons.push(UnsolvedReason::new(
                    "unexpected_merge",
                    format!("expected {expected} merge(s), observed {}", merge.entries),
                ));
            }
            if merge.failed > 0 || merge.conflicts > 0 {
                reasons.push(UnsolvedReason::new(
                    "merge_failed",
                    format!(
                        "{} failed and {} conflicted merge(s)",
                        merge.failed, merge.conflicts
                    ),
                ));
            } else if merge.entries == expected as u64 && merge.merged < expected as u64 {
                reasons.push(UnsolvedReason::new(
                    "merge_failed",
                    format!("{} of {expected} merge entries are merged", merge.merged),
                ));
            }
        }
    }
    if let Some(tasks) = scenario.requirements.verifier.required_tasks() {
        if verifier.checked < tasks as u64 {
            reasons.push(UnsolvedReason::new(
                "verifier_missing",
                format!(
                    "expected {tasks} Verifier verdict(s), observed {}",
                    verifier.checked
                ),
            ));
        }
        if verifier.rejected > 0 || verifier.approved < tasks as u64 {
            reasons.push(UnsolvedReason::new(
                "verifier_rejected",
                format!(
                    "{} approved and {} rejected verdict(s)",
                    verifier.approved, verifier.rejected
                ),
            ));
        }
    }
    if metrics.scouts < scenario.requirements.min_scouts as u64 {
        reasons.push(UnsolvedReason::new(
            "scout_missing",
            format!(
                "expected at least {} Scout completion(s), observed {}",
                scenario.requirements.min_scouts, metrics.scouts
            ),
        ));
    }
    if let Some(max) = scenario.requirements.max_scouts
        && metrics.scouts > max as u64
    {
        reasons.push(UnsolvedReason::new(
            "scout_unexpected",
            format!("observed {} Scouts, maximum {max}", metrics.scouts),
        ));
    }
    if metrics.compactions < scenario.requirements.min_compactions as u64 {
        reasons.push(UnsolvedReason::new(
            "compaction_missing",
            format!(
                "expected at least {} compaction(s), observed {}",
                scenario.requirements.min_compactions, metrics.compactions
            ),
        ));
    }
    if let Some(max) = scenario.requirements.max_compactions
        && metrics.compactions > max as u64
    {
        reasons.push(UnsolvedReason::new(
            "compaction_unexpected",
            format!(
                "observed {} compactions, maximum {max}",
                metrics.compactions
            ),
        ));
    }
    if metrics.peak_workers < scenario.requirements.min_peak_workers as u64 {
        reasons.push(UnsolvedReason::new(
            "parallelism_too_low",
            format!(
                "expected at least {} concurrent Worker(s), observed {}",
                scenario.requirements.min_peak_workers, metrics.peak_workers
            ),
        ));
    }
    if let Some(max) = scenario.requirements.max_peak_workers
        && metrics.peak_workers > max as u64
    {
        reasons.push(UnsolvedReason::new(
            "parallelism_too_high",
            format!(
                "observed {} concurrent Workers, maximum {max}",
                metrics.peak_workers
            ),
        ));
    }
    if scenario.requirements.require_all_plan_tasks_completed {
        if plan_tasks.is_empty() {
            reasons.push(UnsolvedReason::new(
                "plan_tasks_missing",
                "plan run produced no scheduled task rows",
            ));
        } else {
            let completed = plan_tasks
                .iter()
                .filter(|task| task.status == "completed")
                .count();
            if completed < plan_tasks.len() {
                let statuses = plan_tasks
                    .iter()
                    .map(|task| format!("{}={}", task.task_id, task.status))
                    .collect::<Vec<_>>()
                    .join(", ");
                reasons.push(UnsolvedReason::new(
                    "plan_tasks_incomplete",
                    truncate_detail(&format!("task statuses: {statuses}"), 500),
                ));
            }
        }
    }
    reasons
}

fn truncate_detail(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let mut out = value.chars().take(limit).collect::<String>();
    out.push('…');
    out
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown".to_string())
}

/// Construct report metadata for the current host and repository.
pub fn report_environment() -> (HostMetadata, Option<String>, Option<bool>) {
    let (commit, dirty) = crate::report::detect_commit();
    (HostMetadata::detect(), commit, dirty)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::{
        Availability, DirectTask, Requirements, ScenarioConfig, VerifierRequirement,
    };
    use vp_protocol::types::{Event, ScheduledTask};

    fn direct_scenario(id: &str) -> Scenario {
        let target = "bench-target.txt";
        let content = "unit\n";
        Scenario {
            id: id.to_string(),
            title: "unit".to_string(),
            description: "unit scenario".to_string(),
            prompt: "create the target".to_string(),
            fixture: FixtureKind::SingleSession,
            config: ScenarioConfig {
                timeout_secs: 30,
                merge_timeout_secs: 5,
                max_provider_calls: 64,
                ..ScenarioConfig::defaults()
            },
            direct: Some(DirectTask {
                target_path: target.to_string(),
                target_content: content.to_string(),
                prefetch_path: None,
            }),
            plan: None,
            acceptance: crate::fixture::AcceptanceSpec::new().expect_file(target, content),
            requirements: Requirements::default(),
            availability: Availability::available(),
        }
    }

    fn session(status: &str) -> Session {
        Session {
            id: "session".into(),
            project_id: "project".into(),
            goal: "goal".into(),
            status: status.into(),
            priority: 0,
            worktree: None,
            branch: None,
            token_budget: None,
            cost_budget_usd: None,
            time_budget_secs: None,
            unlimited: false,
            cost_spent_usd: 0.0,
            base_commit: None,
            tokens_in: 0,
            tokens_out: 0,
            prompt_layout: 2,
            created_at: "now".into(),
        }
    }

    fn passed_acceptance() -> AcceptanceResult {
        AcceptanceResult {
            passed: true,
            exit_code: Some(0),
            timed_out: false,
            duration_ms: 1,
            stdout_preview: "acceptance: ok".into(),
            stderr_preview: String::new(),
            fingerprint: "f".into(),
        }
    }

    fn empty_metrics() -> RunMetrics {
        RunMetrics {
            wall_clock_ms: 10,
            input_tokens: 10,
            output_tokens: 2,
            cache_read_tokens: 1,
            cache_write_tokens: 0,
            total_tokens: 12,
            provider_calls: 1,
            usage_calls: 1,
            estimated_usage: false,
            cost_usd: 0.0,
            ledger_cost_usd: 0.0,
            cost_reconciled: false,
            cost_per_solved_task_usd: None,
            tokens_per_solved_task: None,
            cache_hit_ratio: None,
            tool_calls: 1,
            tool_errors: 0,
            tool_output_bytes: 10,
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
        }
    }

    fn scheduled(status: &str) -> ScheduledTask {
        ScheduledTask {
            session_id: "s".into(),
            project_id: "p".into(),
            plan_seq: 1,
            task_id: "T1".into(),
            department: "d".into(),
            title: "t".into(),
            priority: 0,
            status: status.into(),
            deps: vec![],
            write_scope: vec![],
            attempts: 1,
            enqueued_at: "now".into(),
            started_at: None,
            finished_at: None,
            last_error: None,
            next_attempt_at: None,
        }
    }

    fn classify_ready(
        scenario: &Scenario,
        metrics: &RunMetrics,
        merge: &MergeMetrics,
        verifier: &VerifierMetrics,
    ) -> Vec<UnsolvedReason> {
        classify(ClassificationInput {
            scenario,
            session: &session("idle"),
            timed_out: false,
            acceptance: &passed_acceptance(),
            verifier,
            merge,
            metrics,
            provider_errors: 0,
            plan_tasks: &[],
        })
    }

    #[test]
    fn a_clean_direct_run_is_solved() {
        let scenario = direct_scenario("unit-clean");
        let metrics = empty_metrics();
        let merge = MergeMetrics::new(0, 0, 0, 0, None);
        let verifier = VerifierMetrics::from_verdicts(vec![]);
        assert!(classify_ready(&scenario, &metrics, &merge, &verifier).is_empty());
    }

    #[test]
    fn acceptance_failure_is_unsolved_even_with_tokens_spent() {
        let scenario = direct_scenario("unit-acceptance");
        let metrics = empty_metrics();
        let merge = MergeMetrics::new(0, 0, 0, 0, None);
        let verifier = VerifierMetrics::from_verdicts(vec![]);
        let reasons = classify(ClassificationInput {
            scenario: &scenario,
            session: &session("idle"),
            timed_out: false,
            acceptance: &AcceptanceResult {
                passed: false,
                exit_code: Some(1),
                timed_out: false,
                duration_ms: 1,
                stdout_preview: String::new(),
                stderr_preview: "content mismatch".into(),
                fingerprint: "f".into(),
            },
            verifier: &verifier,
            merge: &merge,
            metrics: &metrics,
            provider_errors: 0,
            plan_tasks: &[],
        });
        assert!(
            reasons
                .iter()
                .any(|reason| reason.code == "acceptance_failed")
        );
    }

    #[test]
    fn verifier_rejection_and_merge_failure_are_unsolved() {
        let mut scenario = direct_scenario("unit-verifier");
        scenario.requirements.verifier = VerifierRequirement::AllApproved { tasks: 1 };
        let metrics = empty_metrics();
        let merge = MergeMetrics::new(0, 0, 0, 0, None);
        let verifier = VerifierMetrics::from_verdicts(vec![VerdictRecord {
            task_id: Some("T1".into()),
            approved: false,
            summary: "no".into(),
        }]);
        let reasons = classify_ready(&scenario, &metrics, &merge, &verifier);
        assert!(
            reasons
                .iter()
                .any(|reason| reason.code == "verifier_rejected")
        );

        let mut scenario = direct_scenario("unit-merge");
        scenario.requirements.expected_merges = ExpectedMerges::Exactly(1);
        let merge = MergeMetrics::new(1, 0, 1, 0, Some(1));
        let reasons = classify_ready(
            &scenario,
            &metrics,
            &merge,
            &VerifierMetrics::from_verdicts(vec![]),
        );
        assert!(reasons.iter().any(|reason| reason.code == "merge_failed"));
    }

    #[test]
    fn missing_merge_is_unsolved() {
        let mut scenario = direct_scenario("unit-missing-merge");
        scenario.requirements.expected_merges = ExpectedMerges::Exactly(2);
        let reasons = classify_ready(
            &scenario,
            &empty_metrics(),
            &MergeMetrics::new(0, 0, 0, 0, Some(2)),
            &VerifierMetrics::from_verdicts(vec![]),
        );
        assert!(
            reasons
                .iter()
                .any(|reason| reason.code == "merge_expected_missing")
        );
    }

    #[test]
    fn compaction_and_parallelism_requirements_are_checked() {
        let mut scenario = direct_scenario("unit-compaction");
        scenario.requirements.min_compactions = 1;
        scenario.requirements.min_peak_workers = 3;
        let metrics = empty_metrics();
        let reasons = classify_ready(
            &scenario,
            &metrics,
            &MergeMetrics::new(0, 0, 0, 0, None),
            &VerifierMetrics::from_verdicts(vec![]),
        );
        assert!(
            reasons
                .iter()
                .any(|reason| reason.code == "compaction_missing")
        );
        assert!(
            reasons
                .iter()
                .any(|reason| reason.code == "parallelism_too_low")
        );
    }

    #[test]
    fn plan_task_status_is_part_of_solving() {
        let mut scenario = direct_scenario("unit-plan");
        scenario.requirements.require_all_plan_tasks_completed = true;
        let reasons = classify(ClassificationInput {
            scenario: &scenario,
            session: &session("idle"),
            timed_out: false,
            acceptance: &passed_acceptance(),
            verifier: &VerifierMetrics::from_verdicts(vec![]),
            merge: &MergeMetrics::new(0, 0, 0, 0, None),
            metrics: &empty_metrics(),
            provider_errors: 0,
            plan_tasks: &[scheduled("failed")],
        });
        assert!(
            reasons
                .iter()
                .any(|reason| reason.code == "plan_tasks_incomplete")
        );
    }

    #[test]
    fn event_stats_count_tools_verdicts_and_peak_workers() {
        let events = vec![
            Event {
                session_id: "s".into(),
                seq: 1,
                kind: "tool.finished".into(),
                payload: serde_json::json!({"output": "abc", "is_error": true}),
                created_at: "now".into(),
            },
            Event {
                session_id: "s".into(),
                seq: 2,
                kind: "agent.started".into(),
                payload: serde_json::json!({"agent_id": "w1", "agent_role": "worker"}),
                created_at: "now".into(),
            },
            Event {
                session_id: "s".into(),
                seq: 3,
                kind: "agent.started".into(),
                payload: serde_json::json!({"agent_id": "w2", "agent_role": "worker"}),
                created_at: "now".into(),
            },
            Event {
                session_id: "s".into(),
                seq: 4,
                kind: "agent.finished".into(),
                payload: serde_json::json!({
                    "agent_id": "w1", "agent_role": "worker", "approved": true, "summary": "ok"
                }),
                created_at: "now".into(),
            },
            Event {
                session_id: "s".into(),
                seq: 5,
                kind: "scout.completed".into(),
                payload: serde_json::json!({}),
                created_at: "now".into(),
            },
        ];
        let stats = EventStats::from_events(&events);
        assert_eq!(stats.tool_calls, 1);
        assert_eq!(stats.tool_errors, 1);
        assert_eq!(stats.tool_output_bytes, 3);
        assert_eq!(stats.peak_workers, 2);
        assert_eq!(stats.scouts, 1);
        assert_eq!(stats.verdicts.len(), 1);
        assert!(stats.verdicts[0].approved);
    }
}

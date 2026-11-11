//! Benchmark scenario definitions and validation.
//!
//! A scenario is deliberately data, not a bespoke test function. The runner
//! executes every scenario through the same ordinary session/plan path, and the
//! acceptance harness decides whether the fixture's objective was met.
//!
//! Scenarios whose prerequisite feature is not present in this build remain in
//! the catalog with an explicit `Unavailable` reason. They are never silently
//! skipped or reported as solved.

use serde::{Deserialize, Serialize};

use crate::fixture::{AcceptanceSpec, FixtureFile, FixturePlan, validate_relative_path};
use crate::metrics::Pricing;

/// The repository/teardown shape used to exercise a runtime path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureKind {
    SingleSession,
    TeamPlan,
    Scout,
    Compaction,
    Paging,
    Parallel,
}

impl FixtureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SingleSession => "single-session",
            Self::TeamPlan => "team-plan",
            Self::Scout => "scout",
            Self::Compaction => "compaction",
            Self::Paging => "paging",
            Self::Parallel => "parallel",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::SingleSession => "single session",
            Self::TeamPlan => "plan / Worker chain",
            Self::Scout => "read-only Scout",
            Self::Compaction => "context compaction",
            Self::Paging => "tool-output paging",
            Self::Parallel => "worktree parallelism",
        }
    }
}

/// Whether this build can execute the scenario honestly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Available,
    Unavailable { reason: String },
}

impl Availability {
    pub fn available() -> Self {
        Self::Available
    }

    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self::Unavailable {
            reason: reason.into(),
        }
    }

    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Available => None,
            Self::Unavailable { reason } => Some(reason),
        }
    }
}

/// Expected merge queue behavior. `None` asserts that no Worker merge happened;
/// `Exactly(n)` asserts the plan produced exactly `n` merged commits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpectedMerges {
    #[default]
    None,
    Exactly(usize),
}

impl ExpectedMerges {
    pub fn count(self) -> Option<usize> {
        match self {
            Self::None => None,
            Self::Exactly(count) => Some(count),
        }
    }
}

/// Verifier gate expectation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifierRequirement {
    #[default]
    NotRequired,
    AllApproved {
        tasks: usize,
    },
}

impl VerifierRequirement {
    pub fn required_tasks(self) -> Option<usize> {
        match self {
            Self::NotRequired => None,
            Self::AllApproved { tasks } => Some(tasks),
        }
    }
}

/// Classification requirements layered on top of the acceptance script.
///
/// Acceptance is necessary but not sufficient: a plan run whose Worker commit
/// failed to merge, whose Verifier rejected the change, or whose expected Scout
/// never ran is unsolved even if some intermediate file happened to exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requirements {
    pub expected_merges: ExpectedMerges,
    pub verifier: VerifierRequirement,
    pub require_all_plan_tasks_completed: bool,
    pub require_no_agent_failures: bool,
    pub min_scouts: usize,
    pub max_scouts: Option<usize>,
    pub min_compactions: usize,
    pub max_compactions: Option<usize>,
    pub min_peak_workers: usize,
    pub max_peak_workers: Option<usize>,
}

impl Default for Requirements {
    fn default() -> Self {
        Self {
            expected_merges: ExpectedMerges::None,
            verifier: VerifierRequirement::NotRequired,
            require_all_plan_tasks_completed: false,
            require_no_agent_failures: true,
            min_scouts: 0,
            max_scouts: None,
            min_compactions: 0,
            max_compactions: None,
            min_peak_workers: 0,
            max_peak_workers: None,
        }
    }
}

/// A scenario that expects a direct (Leader-only) file edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectTask {
    pub target_path: String,
    pub target_content: String,
    /// Optional large fixture file the fake Leader reads first. This exercises
    /// bounded tool-output telemetry without changing the expected solution.
    pub prefetch_path: Option<String>,
}

/// A plan task definition. The runner converts these into a normal
/// `vp_protocol::types::PlanTask`; the runtime remains the source of truth for
/// dependencies, write scopes, gating, and integration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanTaskSpec {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub write_scope: Vec<String>,
    pub acceptance: Vec<String>,
    pub target_path: String,
    pub target_content: String,
}

/// A normal VerlinPark TeamPlan rendered as benchmark data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanSpec {
    pub goal: String,
    pub department: String,
    pub manager_title: String,
    pub profile: String,
    pub objective: String,
    pub tasks: Vec<PlanTaskSpec>,
}

/// Knobs that change the runtime path under test. They are ordinary
/// `AgentConfig`/session settings, never a private benchmark shortcut.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScenarioConfig {
    pub model: String,
    pub pricing: Pricing,
    pub max_turns: u32,
    pub max_workers: usize,
    pub max_labour: usize,
    /// Zero disables automatic context management.
    pub compact_after_tokens: u64,
    pub compact_keep_messages: usize,
    pub context_window: u64,
    /// Stored before the run; used to force compaction deterministically.
    pub preload_messages: u32,
    pub preload_message_chars: usize,
    /// Deterministic fake-provider latency. It is never applied to the real
    /// provider and is recorded in the report.
    pub provider_latency_ms: u64,
    pub timeout_secs: u64,
    pub merge_timeout_secs: u64,
    pub max_provider_calls: usize,
    /// Whether the deterministic fake Verifier approves. A deliberate negative
    /// fixture can set this false while requirements still expect approval, and
    /// the run must be classified unsolved.
    pub fake_verifier_approves: bool,
}

impl ScenarioConfig {
    pub(crate) fn defaults() -> Self {
        Self {
            model: "bench-fake-1".to_string(),
            pricing: Pricing::benchmark_default(),
            max_turns: 8,
            max_workers: 4,
            max_labour: 8,
            compact_after_tokens: 0,
            compact_keep_messages: 8,
            context_window: 0,
            preload_messages: 0,
            preload_message_chars: 0,
            provider_latency_ms: 0,
            timeout_secs: 120,
            merge_timeout_secs: 10,
            max_provider_calls: 512,
            fake_verifier_approves: true,
        }
    }
}

/// One benchmark scenario.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scenario {
    pub id: String,
    pub title: String,
    pub description: String,
    pub prompt: String,
    pub fixture: FixtureKind,
    pub config: ScenarioConfig,
    pub direct: Option<DirectTask>,
    pub plan: Option<PlanSpec>,
    pub acceptance: AcceptanceSpec,
    pub requirements: Requirements,
    pub availability: Availability,
}

impl Scenario {
    pub fn validate(&self) -> Result<(), String> {
        if self.id.len() < 3 || self.id.len() > 80 {
            return Err("scenario id must be 3-80 characters".to_string());
        }
        if !self.id.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        }) {
            return Err("scenario id must be lowercase ASCII letters, digits, and '-'".to_string());
        }
        if self.title.trim().is_empty() {
            return Err("scenario title cannot be empty".to_string());
        }
        if self.description.trim().is_empty() {
            return Err("scenario description cannot be empty".to_string());
        }
        if self.prompt.trim().is_empty() {
            return Err("scenario prompt cannot be empty".to_string());
        }
        self.config
            .pricing
            .validate()
            .map_err(|error| format!("scenario pricing: {error}"))?;
        if self.config.model.trim().is_empty() {
            return Err("scenario model cannot be empty".to_string());
        }
        if self.config.max_turns == 0 || self.config.max_turns > 64 {
            return Err("max_turns must be 1-64".to_string());
        }
        if self.config.max_workers == 0 || self.config.max_workers > 10 {
            return Err("max_workers must be 1-10".to_string());
        }
        if self.config.max_labour == 0 || self.config.max_labour > 40 {
            return Err("max_labour must be 1-40".to_string());
        }
        if self.config.compact_keep_messages == 0 {
            return Err("compact_keep_messages must be greater than zero".to_string());
        }
        if self.config.compact_after_tokens > 0 {
            if self.config.context_window == 0 {
                return Err(
                    "compaction needs a non-zero context_window to enable context management"
                        .to_string(),
                );
            }
            if self.config.compact_after_tokens > self.config.context_window {
                return Err("compact_after_tokens cannot exceed context_window".to_string());
            }
        }
        if self.config.preload_messages > 200 {
            return Err("preload_messages cannot exceed 200".to_string());
        }
        if self.config.preload_message_chars > 4000 {
            return Err("preload_message_chars cannot exceed 4000".to_string());
        }
        if self.config.provider_latency_ms > 5000 {
            return Err("provider_latency_ms cannot exceed 5000".to_string());
        }
        if self.config.timeout_secs == 0 || self.config.timeout_secs > 3600 {
            return Err("timeout_secs must be 1-3600".to_string());
        }
        if self.config.merge_timeout_secs == 0 || self.config.merge_timeout_secs > 300 {
            return Err("merge_timeout_secs must be 1-300".to_string());
        }
        if self.config.max_provider_calls == 0 || self.config.max_provider_calls > 10_000 {
            return Err("max_provider_calls must be 1-10000".to_string());
        }
        if let Availability::Unavailable { reason } = &self.availability
            && reason.trim().is_empty()
        {
            return Err("unavailable scenarios need a reason".to_string());
        }
        self.acceptance
            .validate()
            .map_err(|error| format!("scenario acceptance: {error}"))?;
        self.fixture_plan()
            .validate()
            .map_err(|error| format!("scenario fixture: {error}"))?;
        self.validate_requirements()?;
        Ok(())
    }

    fn validate_requirements(&self) -> Result<(), String> {
        if let Some(plan) = &self.plan {
            if self.direct.is_some() {
                return Err("a scenario cannot be both direct and plan-based".to_string());
            }
            if plan.tasks.is_empty() || plan.tasks.len() > 20 {
                return Err("plan scenarios need 1-20 tasks".to_string());
            }
            let mut ids = std::collections::HashSet::new();
            let mut targets = std::collections::HashSet::new();
            for task in &plan.tasks {
                if task.id.trim().is_empty()
                    || !task
                        .id
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric() || character == '-')
                {
                    return Err(format!("invalid plan task id: {}", task.id));
                }
                if !ids.insert(task.id.clone()) {
                    return Err(format!("duplicate plan task id: {}", task.id));
                }
                if !targets.insert(task.target_path.clone()) {
                    return Err(format!("duplicate plan target path: {}", task.target_path));
                }
                validate_relative_path(&task.target_path, "plan target")
                    .map_err(|error| error.to_string())?;
                if task.title.trim().is_empty() || task.acceptance.is_empty() {
                    return Err(format!(
                        "plan task '{}' needs a title and acceptance",
                        task.id
                    ));
                }
                if !task
                    .write_scope
                    .iter()
                    .any(|scope| scope == &task.target_path)
                {
                    return Err(format!(
                        "plan task '{}' write_scope must include its target path",
                        task.id
                    ));
                }
            }
            let tasks = plan.tasks.len();
            if self.requirements.expected_merges != ExpectedMerges::Exactly(tasks) {
                return Err(format!("plan scenario must expect exactly {tasks} merges"));
            }
            if self.requirements.verifier != (VerifierRequirement::AllApproved { tasks }) {
                return Err(format!(
                    "plan scenario must require {tasks} approved Verifier verdicts"
                ));
            }
            if !self.requirements.require_all_plan_tasks_completed {
                return Err("plan scenario must require all plan tasks to complete".to_string());
            }
            let expected = self
                .acceptance
                .expected_files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<std::collections::HashSet<_>>();
            if expected.len() != targets.len()
                || !targets
                    .iter()
                    .all(|target| expected.contains(target.as_str()))
            {
                return Err(
                    "plan acceptance expected files must match the plan target paths".to_string(),
                );
            }
        } else {
            if self.direct.is_none() && self.fixture != FixtureKind::Scout {
                return Err("non-plan scenarios need a direct task or Scout fixture".to_string());
            }
            if self.requirements.expected_merges != ExpectedMerges::None {
                return Err("direct scenarios cannot expect merges".to_string());
            }
            if self.requirements.verifier != VerifierRequirement::NotRequired {
                return Err("direct scenarios cannot require a Verifier".to_string());
            }
            if self.requirements.require_all_plan_tasks_completed {
                return Err("direct scenarios cannot require plan completion".to_string());
            }
            if let Some(direct) = &self.direct {
                validate_relative_path(&direct.target_path, "direct target")
                    .map_err(|error| error.to_string())?;
                if let Some(path) = &direct.prefetch_path {
                    validate_relative_path(path, "prefetch path")
                        .map_err(|error| error.to_string())?;
                }
                let matches = self.acceptance.expected_files.iter().any(|file| {
                    file.path == direct.target_path && file.content == direct.target_content
                });
                if !matches {
                    return Err(
                        "direct acceptance must expect the target path and exact content"
                            .to_string(),
                    );
                }
            }
        }
        if self.fixture == FixtureKind::Scout && self.direct.is_some() {
            return Err(
                "Scout scenarios are read-only and cannot declare a direct write".to_string(),
            );
        }
        if self.fixture == FixtureKind::Scout && self.requirements.min_scouts == 0 {
            return Err("Scout scenarios must require at least one Scout".to_string());
        }
        if self.fixture != FixtureKind::Scout && self.requirements.min_scouts > 0 {
            return Err("min_scouts is only valid for Scout scenarios".to_string());
        }
        if self.requirements.min_peak_workers > self.config.max_workers {
            return Err("min_peak_workers cannot exceed the scenario's max_workers".to_string());
        }
        if let Some(max) = self.requirements.max_peak_workers
            && max > self.config.max_workers
        {
            return Err("max_peak_workers cannot exceed the scenario's max_workers".to_string());
        }
        if let Some(max) = self.requirements.max_peak_workers
            && self.requirements.min_peak_workers > max
        {
            return Err("min_peak_workers cannot exceed max_peak_workers".to_string());
        }
        if let Some(max) = self.requirements.max_scouts
            && self.requirements.min_scouts > max
        {
            return Err("min_scouts cannot exceed max_scouts".to_string());
        }
        if let Some(max) = self.requirements.max_compactions
            && self.requirements.min_compactions > max
        {
            return Err("min_compactions cannot exceed max_compactions".to_string());
        }
        if self.fixture == FixtureKind::Compaction
            && self.requirements.min_compactions > 0
            && self.config.compact_after_tokens == 0
        {
            return Err(
                "a compaction-required scenario must enable compact_after_tokens".to_string(),
            );
        }
        Ok(())
    }

    /// Materialize the repository and acceptance plan from the declarative
    /// scenario. This is also the source of comparison fingerprints.
    pub fn fixture_plan(&self) -> FixturePlan {
        let mut plan = FixturePlan::new(self.fixture);
        plan.repo_files.push(FixtureFile::new(
            "README.md",
            format!(
                "# VerlinPark benchmark fixture\n\n\
                 This repository is disposable and exists only for `vp bench`.\n\n\
                 scenario: {}\n\
                 BENCH-MARKER: verlinpark-bench-v1\n",
                self.id
            ),
        ));
        plan.repo_files.push(FixtureFile::new(
            "docs/task.md",
            format!(
                "Benchmark objective\n\n{}\n\n\
                 The acceptance check is owned by the harness and is not part of this repository.\n",
                self.description
            ),
        ));
        if self.fixture == FixtureKind::Paging {
            plan.repo_files
                .push(FixtureFile::new("data/large.log", large_log()));
        }
        if self.fixture == FixtureKind::Compaction {
            plan.repo_files.push(FixtureFile::new(
                "docs/history.md",
                "This fixture includes a preloaded conversation to exercise the normal \
                 compaction path. The fixture itself is intentionally small.\n",
            ));
        }
        plan.acceptance = self.acceptance.clone();
        plan
    }

    /// Target paths that a successful run must produce, for reporting.
    pub fn target_paths(&self) -> Vec<String> {
        if let Some(plan) = &self.plan {
            return plan
                .tasks
                .iter()
                .map(|task| task.target_path.clone())
                .collect();
        }
        self.direct
            .iter()
            .map(|direct| direct.target_path.clone())
            .collect()
    }

    pub fn comparison_fixture_kind(&self) -> &'static str {
        self.fixture.as_str()
    }

    pub fn max_expected_merges(&self) -> usize {
        self.requirements.expected_merges.count().unwrap_or(0)
    }
}

impl Scenario {
    /// Return the complete catalog, including explicitly unavailable future
    /// scenarios. `vp bench list` shows both; `vp bench run` refuses an
    /// unavailable selection with exit code 2.
    pub fn catalog() -> Vec<Self> {
        vec![
            baseline_scenario(),
            plan_worker_scenario(),
            scout_scenario(),
            paging_off_scenario(),
            paging_on_scenario(),
            compaction_off_scenario(),
            compaction_on_scenario(),
            parallel_scenario(1),
            parallel_scenario(4),
        ]
    }

    pub fn find(id: &str) -> Option<Self> {
        Self::catalog()
            .into_iter()
            .find(|scenario| scenario.id == id)
    }

    pub fn catalog_ids() -> Vec<String> {
        Self::catalog()
            .into_iter()
            .map(|scenario| scenario.id)
            .collect()
    }

    pub fn validate_catalog(catalog: &[Scenario]) -> Result<(), String> {
        if catalog.is_empty() {
            return Err("scenario catalog cannot be empty".to_string());
        }
        let mut ids = std::collections::HashSet::new();
        for scenario in catalog {
            scenario.validate()?;
            if !ids.insert(scenario.id.clone()) {
                return Err(format!("duplicate scenario id: {}", scenario.id));
            }
        }
        Ok(())
    }
}

fn baseline_scenario() -> Scenario {
    let target = "bench-target.txt";
    let content = "solved by the baseline single-session path\n";
    Scenario {
        id: "baseline-single-session".to_string(),
        title: "Baseline single session".to_string(),
        description: "A Leader runs directly in the session worktree and writes one exact file."
            .to_string(),
        prompt: format!(
            "Create the file {target} containing exactly: {content:?}. Use the write_file tool and then finish."
        ),
        fixture: FixtureKind::SingleSession,
        config: ScenarioConfig {
            max_workers: 2,
            ..ScenarioConfig::defaults()
        },
        direct: Some(DirectTask {
            target_path: target.to_string(),
            target_content: content.to_string(),
            prefetch_path: None,
        }),
        plan: None,
        acceptance: AcceptanceSpec::new().expect_file(target, content),
        requirements: Requirements::default(),
        availability: Availability::available(),
    }
}

fn plan_worker_scenario() -> Scenario {
    let target = "bench-target-main.txt";
    let content = "solved by the plan worker chain\n";
    let task = PlanTaskSpec {
        id: "T1".to_string(),
        title: format!("Create {target} with the required benchmark content"),
        kind: "coding".to_string(),
        write_scope: vec![target.to_string()],
        acceptance: vec![format!("{target} contains the exact benchmark content")],
        target_path: target.to_string(),
        target_content: content.to_string(),
    };
    Scenario {
        id: "plan-worker-chain".to_string(),
        title: "Plan and Worker chain".to_string(),
        description: "A stored TeamPlan runs one Worker task through the Verifier and Integrator."
            .to_string(),
        prompt: "Execute the stored benchmark TeamPlan.".to_string(),
        fixture: FixtureKind::TeamPlan,
        config: ScenarioConfig {
            max_workers: 2,
            ..ScenarioConfig::defaults()
        },
        direct: None,
        plan: Some(PlanSpec {
            goal: "Run the benchmark plan fixture".to_string(),
            department: "implementation".to_string(),
            manager_title: "Implementation".to_string(),
            profile: "coding".to_string(),
            objective: "Create the required benchmark target.".to_string(),
            tasks: vec![task],
        }),
        acceptance: AcceptanceSpec::new()
            .expect_file(target, content)
            .require_clean(true),
        requirements: Requirements {
            expected_merges: ExpectedMerges::Exactly(1),
            verifier: VerifierRequirement::AllApproved { tasks: 1 },
            require_all_plan_tasks_completed: true,
            ..Requirements::default()
        },
        availability: Availability::available(),
    }
}

fn scout_scenario() -> Scenario {
    Scenario {
        id: "read-only-scout".to_string(),
        title: "Read-only Scout".to_string(),
        description: "A Leader delegates exploration to a fresh-context Scout that cannot write."
            .to_string(),
        prompt:
            "Use the scout tool to read README.md and report the benchmark marker. Do not write."
                .to_string(),
        fixture: FixtureKind::Scout,
        config: ScenarioConfig {
            max_workers: 2,
            ..ScenarioConfig::defaults()
        },
        direct: None,
        plan: None,
        acceptance: AcceptanceSpec::new()
            .expect_absent("bench-target.txt")
            .require_clean(true),
        requirements: Requirements {
            min_scouts: 1,
            max_scouts: Some(1),
            ..Requirements::default()
        },
        availability: Availability::available(),
    }
}

fn paging_off_scenario() -> Scenario {
    let target = "bench-target.txt";
    let content = "solved after reading a large tool output\n";
    Scenario {
        id: "tool-output-paging-off".to_string(),
        title: "Large tool output, paging off".to_string(),
        description: "The Leader reads a ~48 KiB fixture log before solving a small file task."
            .to_string(),
        prompt: "Inspect data/large.log, then create the benchmark target.".to_string(),
        fixture: FixtureKind::Paging,
        config: ScenarioConfig {
            max_workers: 2,
            ..ScenarioConfig::defaults()
        },
        direct: Some(DirectTask {
            target_path: target.to_string(),
            target_content: content.to_string(),
            prefetch_path: Some("data/large.log".to_string()),
        }),
        plan: None,
        acceptance: AcceptanceSpec::new().expect_file(target, content),
        requirements: Requirements::default(),
        availability: Availability::available(),
    }
}

fn paging_on_scenario() -> Scenario {
    let target = "bench-target.txt";
    let content = "solved with paged tool output\n";
    Scenario {
        id: "tool-output-paging-on".to_string(),
        title: "Large tool output, paging on".to_string(),
        description: "The same large-output fixture with the C1 tool-output store enabled."
            .to_string(),
        prompt: "Inspect data/large.log, then create the benchmark target.".to_string(),
        fixture: FixtureKind::Paging,
        config: ScenarioConfig {
            max_workers: 2,
            ..ScenarioConfig::defaults()
        },
        direct: Some(DirectTask {
            target_path: target.to_string(),
            target_content: content.to_string(),
            prefetch_path: Some("data/large.log".to_string()),
        }),
        plan: None,
        acceptance: AcceptanceSpec::new().expect_file(target, content),
        requirements: Requirements::default(),
        availability: Availability::unavailable(
            "the C1 tool-output pager is not present in this build; the harness refuses to label \
             the bounded audit-event payload as paged output",
        ),
    }
}

fn compaction_off_scenario() -> Scenario {
    let target = "bench-target.txt";
    let content = "solved without automatic compaction\n";
    Scenario {
        id: "compaction-off".to_string(),
        title: "Compaction off".to_string(),
        description:
            "A preloaded short message history runs with automatic context management disabled."
                .to_string(),
        prompt: "Continue the benchmark task without automatic compaction.".to_string(),
        fixture: FixtureKind::Compaction,
        config: ScenarioConfig {
            max_workers: 2,
            preload_messages: 40,
            preload_message_chars: 300,
            ..ScenarioConfig::defaults()
        },
        direct: Some(DirectTask {
            target_path: target.to_string(),
            target_content: content.to_string(),
            prefetch_path: None,
        }),
        plan: None,
        acceptance: AcceptanceSpec::new().expect_file(target, content),
        requirements: Requirements {
            max_compactions: Some(0),
            ..Requirements::default()
        },
        availability: Availability::available(),
    }
}

fn compaction_on_scenario() -> Scenario {
    let target = "bench-target.txt";
    let content = "solved after context compaction\n";
    Scenario {
        id: "compaction-on".to_string(),
        title: "Compaction on".to_string(),
        description: "The same preloaded history compacts through the ordinary summary/trim path."
            .to_string(),
        prompt: "Continue the benchmark task after compaction.".to_string(),
        fixture: FixtureKind::Compaction,
        config: ScenarioConfig {
            max_workers: 2,
            compact_after_tokens: 800,
            compact_keep_messages: 4,
            context_window: 32_000,
            preload_messages: 40,
            preload_message_chars: 300,
            ..ScenarioConfig::defaults()
        },
        direct: Some(DirectTask {
            target_path: target.to_string(),
            target_content: content.to_string(),
            prefetch_path: None,
        }),
        plan: None,
        acceptance: AcceptanceSpec::new().expect_file(target, content),
        requirements: Requirements {
            min_compactions: 1,
            ..Requirements::default()
        },
        availability: Availability::available(),
    }
}

fn parallel_scenario(workers: usize) -> Scenario {
    let tasks = (1..=4)
        .map(|index| {
            let target = format!("bench-target-T{index}.txt");
            let content = format!("solved by parallel worker task T{index}\n");
            PlanTaskSpec {
                id: format!("T{index}"),
                title: format!(
                    "Create {target} with the required benchmark content for task T{index}"
                ),
                kind: "coding".to_string(),
                write_scope: vec![target.clone()],
                acceptance: vec![format!("{target} contains the exact benchmark content")],
                target_path: target,
                target_content: content,
            }
        })
        .collect::<Vec<_>>();
    let mut acceptance = AcceptanceSpec::new().require_clean(true);
    for task in &tasks {
        acceptance = acceptance.expect_file(&task.target_path, &task.target_content);
    }
    Scenario {
        id: format!("worktree-parallelism-{workers}"),
        title: format!("Worktree parallelism {workers}"),
        description: format!(
            "Four independent plan tasks run with a Worker cap of {workers}; serial integration still applies."
        ),
        prompt: "Execute the stored parallel benchmark TeamPlan.".to_string(),
        fixture: FixtureKind::Parallel,
        config: ScenarioConfig {
            max_workers: workers,
            provider_latency_ms: 40,
            merge_timeout_secs: 30,
            ..ScenarioConfig::defaults()
        },
        direct: None,
        plan: Some(PlanSpec {
            goal: "Run four independent benchmark tasks".to_string(),
            department: "implementation".to_string(),
            manager_title: "Implementation".to_string(),
            profile: "coding".to_string(),
            objective: "Create four independent benchmark target files.".to_string(),
            tasks,
        }),
        acceptance,
        requirements: Requirements {
            expected_merges: ExpectedMerges::Exactly(4),
            verifier: VerifierRequirement::AllApproved { tasks: 4 },
            require_all_plan_tasks_completed: true,
            min_peak_workers: if workers > 1 { 2 } else { 1 },
            max_peak_workers: Some(workers),
            ..Requirements::default()
        },
        availability: Availability::available(),
    }
}

fn large_log() -> String {
    let mut log = String::with_capacity(64 * 1024);
    log.push_str("BENCH-LARGE-MARKER: start of deterministic fixture output\n");
    for line in 0..480 {
        log.push_str(&format!(
            "line {line:04}: deterministic fixture output for tool-bound telemetry; \
             no secrets, no network, no provider data\n"
        ));
    }
    log.push_str("BENCH-LARGE-MARKER: end of deterministic fixture output\n");
    log
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_unique_and_valid() {
        let catalog = Scenario::catalog();
        assert_eq!(catalog.len(), 9);
        Scenario::validate_catalog(&catalog).unwrap();
        let ids = Scenario::catalog_ids();
        assert!(ids.contains(&"baseline-single-session".to_string()));
        assert!(ids.contains(&"worktree-parallelism-4".to_string()));
    }

    #[test]
    fn catalog_has_one_explicitly_unavailable_scenario() {
        let unavailable = Scenario::catalog()
            .into_iter()
            .filter(|scenario| !scenario.availability.is_available())
            .collect::<Vec<_>>();
        assert_eq!(unavailable.len(), 1);
        assert_eq!(unavailable[0].id, "tool-output-paging-on");
        assert!(unavailable[0].availability.reason().unwrap().contains("C1"));
    }

    #[test]
    fn scenario_lookup_and_validation_are_stable() {
        let scenario = Scenario::find("baseline-single-session").unwrap();
        scenario.validate().unwrap();
        assert_eq!(scenario.fixture_plan().kind, FixtureKind::SingleSession);
        assert!(Scenario::find("missing").is_none());
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let mut catalog = Scenario::catalog();
        catalog.push(catalog[0].clone());
        let error = Scenario::validate_catalog(&catalog).unwrap_err();
        assert!(error.contains("duplicate"));
    }

    #[test]
    fn direct_acceptance_must_match_target_bytes() {
        let mut scenario = Scenario::find("baseline-single-session").unwrap();
        scenario.acceptance.expected_files[0].content = "different\n".to_string();
        let error = scenario.validate().unwrap_err();
        assert!(error.contains("exact content"));
    }

    #[test]
    fn plan_acceptance_must_match_all_targets() {
        let mut scenario = Scenario::find("plan-worker-chain").unwrap();
        scenario.acceptance.expected_files.clear();
        let error = scenario.validate().unwrap_err();
        assert!(error.contains("match the plan target"));
    }

    #[test]
    fn plan_requirements_must_match_task_count() {
        let mut scenario = Scenario::find("plan-worker-chain").unwrap();
        scenario.requirements.expected_merges = ExpectedMerges::Exactly(2);
        let error = scenario.validate().unwrap_err();
        assert!(error.contains("exactly 1 merges"));
    }

    #[test]
    fn parallel_scenarios_differ_in_worker_cap_and_peak_requirement() {
        let one = Scenario::find("worktree-parallelism-1").unwrap();
        let four = Scenario::find("worktree-parallelism-4").unwrap();
        assert_eq!(one.config.max_workers, 1);
        assert_eq!(four.config.max_workers, 4);
        assert_eq!(four.requirements.min_peak_workers, 2);
        assert_eq!(four.plan.unwrap().tasks.len(), 4);
    }

    #[test]
    fn paging_fixture_contains_a_bounded_large_log() {
        let scenario = Scenario::find("tool-output-paging-off").unwrap();
        let fixture = scenario.fixture_plan();
        let large = fixture
            .repo_files
            .iter()
            .find(|file| file.path == "data/large.log")
            .unwrap();
        assert!(large.content.len() > 32 * 1024);
        assert!(large.content.len() < 64 * 1024);
        assert!(large.content.contains("BENCH-LARGE-MARKER"));
    }

    #[test]
    fn compaction_scenarios_validate_their_context_shape() {
        let off = Scenario::find("compaction-off").unwrap();
        let on = Scenario::find("compaction-on").unwrap();
        assert_eq!(off.config.compact_after_tokens, 0);
        assert_eq!(off.requirements.max_compactions, Some(0));
        assert!(on.config.compact_after_tokens > 0);
        assert_eq!(on.requirements.min_compactions, 1);
        assert_eq!(on.config.preload_messages, off.config.preload_messages);
    }

    #[test]
    fn invalid_worker_caps_and_peak_requirements_are_rejected() {
        let mut scenario = Scenario::find("worktree-parallelism-4").unwrap();
        scenario.config.max_workers = 0;
        assert!(scenario.validate().is_err());
        let mut scenario = Scenario::find("worktree-parallelism-4").unwrap();
        scenario.requirements.min_peak_workers = 5;
        assert!(scenario.validate().is_err());
    }

    #[test]
    fn scout_scenario_is_read_only_and_requires_a_scout() {
        let scenario = Scenario::find("read-only-scout").unwrap();
        assert!(scenario.direct.is_none());
        assert!(scenario.requirement_has_no_merges());
        assert!(scenario.acceptance.require_clean_git);
    }

    impl Scenario {
        fn requirement_has_no_merges(&self) -> bool {
            self.requirements.expected_merges == ExpectedMerges::None
        }
    }
}

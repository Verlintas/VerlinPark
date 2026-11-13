//! Reliability measurement for benchmark trials (ADR-0197).
//!
//! This module is **observation-only**. It consumes the ordinary benchmark run
//! records and the session event log, then computes:
//!
//! * `pass@k` and `pass^k` over repeated deterministic trials;
//! * success variance and a flake rate from outcome transitions;
//! * Lucky-Pass signals derived from event/tool ordering, never model prose;
//! * coordination overhead from the usage ledger's role/task attribution;
//! * cost per solved task and per solved scenario.
//!
//! Nothing here is a merge gate, approval, Verifier verdict, or scheduler
//! input. The fake provider remains the default, and analyzing a saved report
//! performs no provider call. Every report is per scenario: one host, one
//! fixture, one model, and one pricing profile are not a universal leaderboard.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;
use vp_protocol::types::{Event, UsageEntry};

use crate::metrics::{format_optional, format_percent, format_usd, round_usd, sha256_hex};
use crate::report::{ProviderMetadata, RunRecord, SCHEMA_VERSION as BENCH_SCHEMA_VERSION};
use crate::scenario::Scenario;

pub const RELIABILITY_SCHEMA_VERSION: u32 = 1;
pub const MAX_RELIABILITY_TRIALS: usize = 100;
pub const MAX_RELIABILITY_SCENARIOS: usize = 20;
pub const MAX_RELIABILITY_K: u32 = 20;
pub const MAX_RELIABILITY_BYTES: usize = 8 * 1024 * 1024;
pub const RELIABILITY_CAVEAT: &str = "Reliability metrics are local, \
observation-only measurements for the recorded fixture, provider mode, model, pricing profile, \
host, and commit. Fake-provider trials measure harness/runtime mechanics, not model quality. \
pass@k and pass^k are descriptive estimators over the recorded deterministic trials, not a \
provider guarantee, significance test, merge gate, or universal ranking. Lucky-Pass signals are \
deterministic ordering heuristics, not a semantic proof that a run was correct.";

/// Stable Lucky-Pass signal codes. They are persisted in reports and are part
/// of the additive wire shape.
pub mod lucky_codes {
    pub const COMMIT_WITHOUT_PASSING_TEST: &str = "commit_without_passing_test";
    pub const RETRY_WITHOUT_NEW_EVIDENCE: &str = "retry_without_new_evidence";
    pub const EDIT_REVERT_CYCLE: &str = "edit_revert_cycle";
    pub const TEST_BEFORE_IMPLEMENT: &str = "test_before_implement";
    pub const VERIFY_BEFORE_IMPLEMENT: &str = "verify_before_implement";

    pub const ALL: [&str; 5] = [
        COMMIT_WITHOUT_PASSING_TEST,
        RETRY_WITHOUT_NEW_EVIDENCE,
        EDIT_REVERT_CYCLE,
        TEST_BEFORE_IMPLEMENT,
        VERIFY_BEFORE_IMPLEMENT,
    ];
}

/// One deterministic signal with bounded evidence. `detail` never contains
/// prompts, file content, tool arguments, tool results, or raw errors.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LuckySignal {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub count: u64,
    #[serde(default)]
    pub first_seq: Option<u64>,
    #[serde(default)]
    pub detail: String,
}

/// Deterministic trace evidence collected from one benchmark repetition.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrialEvidence {
    #[serde(default)]
    pub implementations: u64,
    #[serde(default)]
    pub tests_run: u64,
    #[serde(default)]
    pub tests_passed: u64,
    #[serde(default)]
    pub tests_failed: u64,
    #[serde(default)]
    pub verifications: u64,
    #[serde(default)]
    pub commits: u64,
    #[serde(default)]
    pub retries: u64,
    #[serde(default)]
    pub first_implementation_seq: Option<u64>,
    #[serde(default)]
    pub first_test_seq: Option<u64>,
    #[serde(default)]
    pub first_verification_seq: Option<u64>,
    #[serde(default)]
    pub first_commit_seq: Option<u64>,
    #[serde(default)]
    pub commit_without_passing_test: u64,
    #[serde(default)]
    pub retry_without_new_evidence: u64,
    #[serde(default)]
    pub edit_revert_cycles: u64,
    #[serde(default)]
    pub test_before_implement: bool,
    #[serde(default)]
    pub verify_before_implement: bool,
    #[serde(default)]
    pub signals: Vec<LuckySignal>,
}

impl TrialEvidence {
    pub fn flagged(&self) -> bool {
        !self.signals.is_empty()
    }

    pub fn signal_codes(&self) -> Vec<&str> {
        self.signals
            .iter()
            .map(|signal| signal.code.as_str())
            .collect()
    }

    pub fn has_code(&self, code: &str) -> bool {
        self.signals.iter().any(|signal| signal.code == code)
    }

    pub fn signal_count(&self, code: &str) -> u64 {
        self.signals
            .iter()
            .filter(|signal| signal.code == code)
            .map(|signal| signal.count)
            .sum()
    }
}

/// Token attribution used for coordination overhead. `orchestration_tokens`
/// covers Leader + Manager + Verifier. `delegation_tokens` covers Scout and any
/// Worker/Labour usage row without a plan task id (delegation-scoped or
/// resolver-style runs). `productive_tokens` covers Worker/Labour rows with a
/// plan task id. The ratio is `(orchestration + delegation) / productive`;
/// absent productivity yields `None`, never zero or infinity.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CoordinationUsage {
    #[serde(default)]
    pub orchestration_tokens: u64,
    #[serde(default)]
    pub delegation_tokens: u64,
    #[serde(default)]
    pub productive_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    /// `orchestration_tokens + delegation_tokens`.
    #[serde(default)]
    pub numerator_tokens: u64,
    /// Alias for `productive_tokens`, kept explicit for the ratio contract.
    #[serde(default)]
    pub denominator_tokens: u64,
    #[serde(default)]
    pub untasked_worker_tokens: u64,
    #[serde(default)]
    pub scout_tokens: u64,
    #[serde(default)]
    pub ratio: Option<f64>,
    /// `numerator / total`. All coordination share is `None` for an empty
    /// ledger.
    #[serde(default)]
    pub coordination_share: Option<f64>,
}

impl CoordinationUsage {
    pub fn from_entries(entries: &[UsageEntry]) -> Self {
        let mut usage = CoordinationUsage::default();
        for entry in entries {
            let tokens = entry.input_tokens.saturating_add(entry.output_tokens);
            if tokens == 0 {
                continue;
            }
            usage.total_tokens = usage.total_tokens.saturating_add(tokens);
            let role = entry.role.to_ascii_lowercase();
            match role.as_str() {
                "leader" | "manager" | "verifier" => {
                    usage.orchestration_tokens = usage.orchestration_tokens.saturating_add(tokens);
                }
                "worker" | "labour" => {
                    if entry.task_id.as_deref().is_some_and(|id| !id.is_empty()) {
                        usage.productive_tokens = usage.productive_tokens.saturating_add(tokens);
                    } else {
                        usage.untasked_worker_tokens =
                            usage.untasked_worker_tokens.saturating_add(tokens);
                        usage.delegation_tokens = usage.delegation_tokens.saturating_add(tokens);
                    }
                }
                "scout" => {
                    usage.scout_tokens = usage.scout_tokens.saturating_add(tokens);
                    usage.delegation_tokens = usage.delegation_tokens.saturating_add(tokens);
                }
                _ => {
                    usage.delegation_tokens = usage.delegation_tokens.saturating_add(tokens);
                }
            }
        }
        usage.finish();
        usage
    }

    pub fn combine(values: impl IntoIterator<Item = CoordinationUsage>) -> Self {
        let mut combined = CoordinationUsage::default();
        for value in values {
            combined.orchestration_tokens = combined
                .orchestration_tokens
                .saturating_add(value.orchestration_tokens);
            combined.delegation_tokens = combined
                .delegation_tokens
                .saturating_add(value.delegation_tokens);
            combined.productive_tokens = combined
                .productive_tokens
                .saturating_add(value.productive_tokens);
            combined.total_tokens = combined.total_tokens.saturating_add(value.total_tokens);
            combined.numerator_tokens = combined
                .numerator_tokens
                .saturating_add(value.numerator_tokens);
            combined.denominator_tokens = combined
                .denominator_tokens
                .saturating_add(value.denominator_tokens);
            combined.untasked_worker_tokens = combined
                .untasked_worker_tokens
                .saturating_add(value.untasked_worker_tokens);
            combined.scout_tokens = combined.scout_tokens.saturating_add(value.scout_tokens);
        }
        combined.finish();
        combined
    }

    fn finish(&mut self) {
        let numerator = self
            .orchestration_tokens
            .saturating_add(self.delegation_tokens);
        self.numerator_tokens = numerator;
        self.denominator_tokens = self.productive_tokens;
        self.ratio = if self.productive_tokens == 0 {
            None
        } else {
            Some(numerator as f64 / self.productive_tokens as f64)
        };
        self.coordination_share = if self.total_tokens == 0 {
            None
        } else {
            Some(numerator as f64 / self.total_tokens as f64)
        };
    }
}

/// Analyze one trial's event stream. Events must be ordered by `seq`; all
/// decisions are deterministic and based only on event/tool ordering and
/// bounded metadata.
pub fn analyze_events(events: &[Event]) -> TrialEvidence {
    let mut evidence = TrialEvidence::default();
    let mut last_implementation_seq: Option<u64> = None;
    let mut last_passing_test_seq: Option<u64> = None;
    let mut last_retry_evidence: Option<String> = None;
    let mut evidence_hash = sha256_hex(b"bench-evidence-start");
    let mut path_before: BTreeMap<String, String> = BTreeMap::new();
    let mut first_commit_flag: Option<u64> = None;
    let mut first_retry_flag: Option<u64> = None;
    let mut first_revert_flag: Option<u64> = None;

    for event in events {
        match event.kind.as_str() {
            "tool.finished" => {
                let name = event
                    .payload
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                let is_error = event
                    .payload
                    .get("is_error")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                evidence_hash = extend_evidence_hash(&evidence_hash, event, name, is_error);

                if is_implementation_tool(name)
                    && !is_error
                    && !event
                        .payload
                        .get("structured")
                        .and_then(|structured| structured.get("dry_run"))
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
                {
                    evidence.implementations = evidence.implementations.saturating_add(1);
                    evidence.first_implementation_seq.get_or_insert(event.seq);
                    last_implementation_seq = Some(event.seq);
                    inspect_mutations(
                        event,
                        &mut path_before,
                        &mut evidence,
                        &mut first_revert_flag,
                    );
                }

                if is_test_tool(name) && test_evidence(event, name) {
                    evidence.tests_run = evidence.tests_run.saturating_add(1);
                    evidence.first_test_seq.get_or_insert(event.seq);
                    if is_error || structured_exit_code(event).is_some_and(|code| code != 0) {
                        evidence.tests_failed = evidence.tests_failed.saturating_add(1);
                    } else if structured_exit_code(event).is_some() {
                        evidence.tests_passed = evidence.tests_passed.saturating_add(1);
                        last_passing_test_seq = Some(event.seq);
                    }
                }

                if name == "submit_verdict" {
                    evidence.verifications = evidence.verifications.saturating_add(1);
                    evidence.first_verification_seq.get_or_insert(event.seq);
                }
            }
            "agent.finished" => {
                if event
                    .payload
                    .get("agent_role")
                    .and_then(serde_json::Value::as_str)
                    == Some("verifier")
                    && event.payload.get("approved").is_some()
                {
                    evidence.verifications = evidence.verifications.saturating_add(1);
                    evidence.first_verification_seq.get_or_insert(event.seq);
                }
            }
            "session.committed" | "merge_queue.enqueued" => {
                evidence.commits = evidence.commits.saturating_add(1);
                evidence.first_commit_seq.get_or_insert(event.seq);
                let lacks_test = match (last_implementation_seq, last_passing_test_seq) {
                    (Some(implementation), Some(passed)) => passed < implementation,
                    (Some(_), None) => true,
                    (None, _) => false,
                };
                if lacks_test {
                    evidence.commit_without_passing_test =
                        evidence.commit_without_passing_test.saturating_add(1);
                    first_commit_flag.get_or_insert(event.seq);
                }
            }
            "agent.stream_retry" | "plan_task.retry_scheduled" | "merge_queue.retry" => {
                evidence.retries = evidence.retries.saturating_add(1);
                if last_retry_evidence.as_deref() == Some(evidence_hash.as_str()) {
                    evidence.retry_without_new_evidence =
                        evidence.retry_without_new_evidence.saturating_add(1);
                    first_retry_flag.get_or_insert(event.seq);
                }
                last_retry_evidence = Some(evidence_hash.clone());
            }
            _ => {}
        }
    }

    if let (Some(implementation), Some(test)) =
        (evidence.first_implementation_seq, evidence.first_test_seq)
        && test < implementation
    {
        evidence.test_before_implement = true;
    }
    if let (Some(implementation), Some(verification)) = (
        evidence.first_implementation_seq,
        evidence.first_verification_seq,
    ) && verification < implementation
    {
        evidence.verify_before_implement = true;
    }

    if evidence.commit_without_passing_test > 0 {
        evidence.signals.push(LuckySignal {
            code: lucky_codes::COMMIT_WITHOUT_PASSING_TEST.to_string(),
            count: evidence.commit_without_passing_test,
            first_seq: first_commit_flag.or(evidence.first_commit_seq),
            detail: format!(
                "{} commit(s) had no passing test after the most recent implementation",
                evidence.commit_without_passing_test
            ),
        });
    }
    if evidence.retry_without_new_evidence > 0 {
        evidence.signals.push(LuckySignal {
            code: lucky_codes::RETRY_WITHOUT_NEW_EVIDENCE.to_string(),
            count: evidence.retry_without_new_evidence,
            first_seq: first_retry_flag,
            detail: format!(
                "{} retry event(s) repeated a retry with an unchanged tool-evidence hash",
                evidence.retry_without_new_evidence
            ),
        });
    }
    if evidence.edit_revert_cycles > 0 {
        evidence.signals.push(LuckySignal {
            code: lucky_codes::EDIT_REVERT_CYCLE.to_string(),
            count: evidence.edit_revert_cycles,
            first_seq: first_revert_flag,
            detail: format!(
                "{} same-path mutation(s) returned to the previous content hash without progress",
                evidence.edit_revert_cycles
            ),
        });
    }
    if evidence.test_before_implement {
        evidence.signals.push(LuckySignal {
            code: lucky_codes::TEST_BEFORE_IMPLEMENT.to_string(),
            count: 1,
            first_seq: evidence.first_test_seq,
            detail: "the first test command preceded the first implementation mutation".to_string(),
        });
    }
    if evidence.verify_before_implement {
        evidence.signals.push(LuckySignal {
            code: lucky_codes::VERIFY_BEFORE_IMPLEMENT.to_string(),
            count: 1,
            first_seq: evidence.first_verification_seq,
            detail: "the first verification preceded the first implementation mutation".to_string(),
        });
    }
    evidence
}

fn extend_evidence_hash(previous: &str, event: &Event, name: &str, is_error: bool) -> String {
    let code = event
        .payload
        .get("error_code")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let bytes = event
        .payload
        .get("output_bytes")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let call_id = event
        .payload
        .get("tool_call_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    sha256_hex(format!("{previous}|{call_id}|{name}|{is_error}|{code}|{bytes}").as_bytes())
}

fn is_implementation_tool(name: &str) -> bool {
    matches!(
        name,
        "write_file" | "edit_file" | "apply_patch" | "file_manage"
    )
}

fn is_test_tool(name: &str) -> bool {
    matches!(name, "run_project_task" | "shell")
}

fn structured_exit_code(event: &Event) -> Option<i64> {
    event
        .payload
        .get("structured")
        .and_then(|structured| structured.get("exit_code"))
        .and_then(serde_json::Value::as_i64)
}

fn command_indicates_test(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    lower
        .split(|character: char| !character.is_ascii_alphanumeric())
        .any(|token| {
            matches!(
                token,
                "test"
                    | "tests"
                    | "pytest"
                    | "vitest"
                    | "jest"
                    | "rspec"
                    | "phpunit"
                    | "ctest"
                    | "tox"
                    | "unittest"
            )
        })
}

/// A tool call is test evidence only when its structured command looks like a
/// test runner. `run_project_task` carries the resolved command; `shell` may
/// omit it, in which case no test evidence is inferred.
fn test_evidence(event: &Event, name: &str) -> bool {
    let structured = event.payload.get("structured");
    let command = structured
        .and_then(|structured| structured.get("command"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    match name {
        "run_project_task" => command_indicates_test(command),
        "shell" => {
            let raw_command = structured
                .and_then(|structured| structured.get("command"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            command_indicates_test(raw_command)
        }
        _ => false,
    }
}

fn inspect_mutations(
    event: &Event,
    path_before: &mut BTreeMap<String, String>,
    evidence: &mut TrialEvidence,
    first_revert: &mut Option<u64>,
) {
    let structured = event.payload.get("structured");
    let Some(structured) = structured else {
        return;
    };
    if let Some(files) = structured
        .get("files")
        .and_then(serde_json::Value::as_array)
    {
        for file in files {
            inspect_mutation(
                event.seq,
                file.get("path").and_then(serde_json::Value::as_str),
                file.get("before_sha256")
                    .and_then(serde_json::Value::as_str),
                file.get("after_sha256").and_then(serde_json::Value::as_str),
                path_before,
                evidence,
                first_revert,
            );
        }
        return;
    }
    let path = structured.get("path").and_then(serde_json::Value::as_str);
    let before = structured
        .get("before_sha256")
        .and_then(serde_json::Value::as_str);
    let after = structured
        .get("after_sha256")
        .or_else(|| structured.get("sha256"))
        .and_then(serde_json::Value::as_str);
    inspect_mutation(
        event.seq,
        path,
        before,
        after,
        path_before,
        evidence,
        first_revert,
    );
}

fn inspect_mutation(
    seq: u64,
    path: Option<&str>,
    before: Option<&str>,
    after: Option<&str>,
    path_before: &mut BTreeMap<String, String>,
    evidence: &mut TrialEvidence,
    first_revert: &mut Option<u64>,
) {
    let (Some(path), Some(before), Some(after)) = (path, before, after) else {
        return;
    };
    if path.is_empty() || path.chars().count() > 512 || before == after {
        return;
    }
    if path_before.get(path).map(String::as_str) == Some(after) {
        evidence.edit_revert_cycles = evidence.edit_revert_cycles.saturating_add(1);
        first_revert.get_or_insert(seq);
    }
    path_before.insert(path.to_string(), before.to_string());
}

/// Deterministic `pass@k` estimator:
/// `1 - C(n-c, k) / C(n, k)`. Returns `None` when `k` is zero or greater than
/// the number of trials, because the estimator is undefined there.
pub fn pass_at_k(trials: usize, solved: usize, k: u32) -> Option<f64> {
    if trials == 0 || k == 0 || k as usize > trials || solved > trials {
        return None;
    }
    if solved == 0 {
        return Some(0.0);
    }
    let failures = trials - solved;
    if failures < k as usize {
        return Some(1.0);
    }
    let ratio = combination(failures, k as usize) / combination(trials, k as usize);
    Some((1.0 - ratio).clamp(0.0, 1.0))
}

/// Deterministic `pass^k` estimator: `C(c, k) / C(n, k)`. It is the
/// probability that all `k` sampled trials succeed without replacement.
pub fn pass_power_k(trials: usize, solved: usize, k: u32) -> Option<f64> {
    if trials == 0 || k == 0 || k as usize > trials || solved > trials {
        return None;
    }
    if solved < k as usize {
        return Some(0.0);
    }
    Some((combination(solved, k as usize) / combination(trials, k as usize)).clamp(0.0, 1.0))
}

fn combination(n: usize, k: usize) -> f64 {
    if k > n {
        return 0.0;
    }
    let k = k.min(n.saturating_sub(k));
    let mut result = 1.0f64;
    for index in 0..k {
        result *= (n - index) as f64 / (index + 1) as f64;
    }
    result
}

/// Sample variance of the binary trial outcomes (`n-1` denominator). `None`
/// when fewer than two trials exist.
pub fn success_variance(trials: usize, solved: usize) -> Option<f64> {
    if trials < 2 || solved > trials {
        return None;
    }
    let p = solved as f64 / trials as f64;
    Some(p * (1.0 - p) * trials as f64 / (trials - 1) as f64)
}

/// Fraction of adjacent trial pairs whose outcome changed. `None` when fewer
/// than two trials exist. A stable all-solved or all-unsolved scenario is 0.
pub fn flake_rate(outcomes: &[bool]) -> Option<f64> {
    if outcomes.len() < 2 {
        return None;
    }
    let transitions = outcomes
        .windows(2)
        .filter(|pair| pair[0] != pair[1])
        .count();
    Some(transitions as f64 / (outcomes.len() - 1) as f64)
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LuckyPassSummary {
    #[serde(default)]
    pub flagged_runs: u64,
    #[serde(default)]
    pub solved_flagged_runs: u64,
    /// Flagged solved runs divided by solved runs. `None` when no trial solved.
    #[serde(default)]
    pub lucky_pass_rate: Option<f64>,
    #[serde(default)]
    pub total_flags: u64,
    /// Number of runs carrying each signal.
    #[serde(default)]
    pub signal_runs: BTreeMap<String, u64>,
    /// Total occurrences of each signal across runs.
    #[serde(default)]
    pub signal_counts: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TrialOutcome {
    #[serde(default)]
    pub run_id: String,
    #[serde(default)]
    pub repetition: usize,
    #[serde(default)]
    pub solved: bool,
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default)]
    pub flagged: bool,
    #[serde(default)]
    pub signal_codes: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ScenarioReliability {
    #[serde(default)]
    pub scenario_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub fixture_kind: String,
    #[serde(default)]
    pub trials: usize,
    #[serde(default)]
    pub solved: usize,
    #[serde(default)]
    pub unsolved: usize,
    #[serde(default)]
    pub success_rate: Option<f64>,
    #[serde(default)]
    pub pass_at_k: Option<f64>,
    #[serde(default)]
    pub pass_power_k: Option<f64>,
    #[serde(default)]
    pub success_variance: Option<f64>,
    #[serde(default)]
    pub flake_rate: Option<f64>,
    #[serde(default)]
    pub flaky: bool,
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default)]
    pub cost_per_solved_task_usd: Option<f64>,
    #[serde(default)]
    pub cost_per_solved_scenario_usd: Option<f64>,
    #[serde(default)]
    pub coordination: CoordinationUsage,
    #[serde(default)]
    pub lucky_pass: LuckyPassSummary,
    #[serde(default)]
    pub trials_detail: Vec<TrialOutcome>,
}

impl ScenarioReliability {
    pub fn from_runs(scenario: &Scenario, runs: &[&RunRecord], k: u32) -> Result<Self> {
        if runs.is_empty() {
            bail!("scenario {} has no trials", scenario.id);
        }
        let mut ordered = runs.to_vec();
        ordered.sort_by_key(|run| run.repetition);
        let trials = ordered.len();
        let solved = ordered.iter().filter(|run| run.solved).count();
        let outcomes = ordered.iter().map(|run| run.solved).collect::<Vec<_>>();
        let total_cost = round_usd(ordered.iter().map(|run| run.metrics.cost_usd).sum::<f64>());
        let coordination =
            CoordinationUsage::combine(ordered.iter().map(|run| run.coordination.clone()));
        let mut lucky_pass = LuckyPassSummary::default();
        let mut trials_detail = Vec::with_capacity(trials);
        for run in &ordered {
            let flagged = run.trial_evidence.flagged();
            if flagged {
                lucky_pass.flagged_runs = lucky_pass.flagged_runs.saturating_add(1);
                if run.solved {
                    lucky_pass.solved_flagged_runs =
                        lucky_pass.solved_flagged_runs.saturating_add(1);
                }
                for signal in &run.trial_evidence.signals {
                    *lucky_pass
                        .signal_runs
                        .entry(signal.code.clone())
                        .or_insert(0) += 1;
                    *lucky_pass
                        .signal_counts
                        .entry(signal.code.clone())
                        .or_insert(0) += signal.count;
                    lucky_pass.total_flags = lucky_pass.total_flags.saturating_add(signal.count);
                }
            }
            trials_detail.push(TrialOutcome {
                run_id: run.run_id.clone(),
                repetition: run.repetition,
                solved: run.solved,
                cost_usd: round_usd(run.metrics.cost_usd),
                flagged,
                signal_codes: run
                    .trial_evidence
                    .signal_codes()
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
            });
        }
        lucky_pass.lucky_pass_rate = if solved == 0 {
            None
        } else {
            Some(lucky_pass.solved_flagged_runs as f64 / solved as f64)
        };
        let flake = flake_rate(&outcomes);
        Ok(Self {
            scenario_id: scenario.id.clone(),
            title: scenario.title.clone(),
            fixture_kind: scenario.fixture.as_str().to_string(),
            trials,
            solved,
            unsolved: trials.saturating_sub(solved),
            success_rate: Some(solved as f64 / trials as f64),
            pass_at_k: pass_at_k(trials, solved, k),
            pass_power_k: pass_power_k(trials, solved, k),
            success_variance: success_variance(trials, solved),
            flake_rate: flake,
            flaky: outcomes.iter().any(|value| *value) && outcomes.iter().any(|value| !*value),
            cost_usd: total_cost,
            cost_per_solved_task_usd: if solved == 0 {
                None
            } else {
                Some(round_usd(total_cost / solved as f64))
            },
            cost_per_solved_scenario_usd: if solved == 0 {
                None
            } else {
                Some(round_usd(total_cost))
            },
            coordination,
            lucky_pass,
            trials_detail,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReliabilitySelection {
    #[serde(default)]
    pub scenario_ids: Vec<String>,
    #[serde(default)]
    pub trials: usize,
    #[serde(default)]
    pub k: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReliabilitySummary {
    #[serde(default)]
    pub scenario_count: usize,
    #[serde(default)]
    pub run_count: usize,
    #[serde(default)]
    pub trials_per_scenario: usize,
    #[serde(default)]
    pub k: u32,
    #[serde(default)]
    pub solved_runs: usize,
    #[serde(default)]
    pub unsolved_runs: usize,
    #[serde(default)]
    pub harness_faults: usize,
    #[serde(default)]
    pub all_solved: bool,
    /// Descriptive mean of the per-scenario `pass@k` values. It is not a
    /// cross-model ranking and is absent when no scenario has a defined value.
    #[serde(default)]
    pub mean_pass_at_k: Option<f64>,
    #[serde(default)]
    pub mean_pass_power_k: Option<f64>,
    #[serde(default)]
    pub mean_success_variance: Option<f64>,
    #[serde(default)]
    pub flaky_scenarios: usize,
    #[serde(default)]
    pub flake_rate: Option<f64>,
    #[serde(default)]
    pub total_cost_usd: f64,
    #[serde(default)]
    pub cost_per_solved_task_usd: Option<f64>,
    #[serde(default)]
    pub cost_per_solved_scenario_usd: Option<f64>,
    #[serde(default)]
    pub coordination: CoordinationUsage,
    #[serde(default)]
    pub lucky_pass: LuckyPassSummary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReliabilityReport {
    pub schema_version: u32,
    pub generated_at: String,
    pub source: String,
    pub caveat: String,
    pub provider: ProviderMetadata,
    pub pricing: crate::metrics::Pricing,
    pub selection: ReliabilitySelection,
    pub summary: ReliabilitySummary,
    pub scenarios: Vec<ScenarioReliability>,
}

impl ReliabilityReport {
    pub fn build(
        runs: &[RunRecord],
        selected: &[Scenario],
        k: u32,
        source: impl Into<String>,
        generated_at: impl Into<String>,
    ) -> Result<Self> {
        if runs.is_empty() {
            bail!("cannot build a reliability report with no trials");
        }
        if selected.is_empty() {
            bail!("cannot build a reliability report with no scenarios");
        }
        if selected.len() > MAX_RELIABILITY_SCENARIOS {
            bail!("reliability selection is limited to {MAX_RELIABILITY_SCENARIOS} scenarios");
        }
        if k == 0 || k > MAX_RELIABILITY_K {
            bail!("reliability k must be 1-{MAX_RELIABILITY_K}");
        }
        let mut by_scenario: BTreeMap<&str, Vec<RunRecord>> = BTreeMap::new();
        for run in runs {
            by_scenario
                .entry(run.scenario_id.as_str())
                .or_default()
                .push(run.clone());
        }
        let selected_ids = selected
            .iter()
            .map(|scenario| scenario.id.as_str())
            .collect::<HashSet<_>>();
        if let Some(run) = runs
            .iter()
            .find(|run| !selected_ids.contains(run.scenario_id.as_str()))
        {
            bail!(
                "reliability runs include scenario {} which was not selected",
                run.scenario_id
            );
        }
        let mut scenarios = Vec::with_capacity(selected.len());
        let mut trials_per_scenario = None;
        let mut provider: Option<ProviderMetadata> = None;
        let mut pricing = None;
        for scenario in selected {
            let scenario_runs = by_scenario
                .get(scenario.id.as_str())
                .with_context(|| format!("scenario {} has no trials", scenario.id))?;
            if scenario_runs.len() > MAX_RELIABILITY_TRIALS {
                bail!(
                    "scenario {} has {} trials, above the {MAX_RELIABILITY_TRIALS} trial cap",
                    scenario.id,
                    scenario_runs.len()
                );
            }
            if let Some(expected) = trials_per_scenario {
                if scenario_runs.len() != expected {
                    bail!(
                        "scenario {} has {} trials, expected {expected}",
                        scenario.id,
                        scenario_runs.len()
                    );
                }
            } else {
                trials_per_scenario = Some(scenario_runs.len());
            }
            for run in scenario_runs {
                if let Some(provider) = &provider {
                    if run.provider.mode != provider.mode
                        || run.provider.id != provider.id
                        || run.provider.model != provider.model
                    {
                        bail!("all reliability trials must use the same provider identity");
                    }
                } else {
                    provider = Some(run.provider.clone());
                }
                if let Some(pricing) = pricing {
                    if run.pricing != pricing {
                        bail!("all reliability trials must use the same pricing profile");
                    }
                } else {
                    pricing = Some(run.pricing);
                }
            }
            let refs = scenario_runs.iter().collect::<Vec<_>>();
            scenarios.push(ScenarioReliability::from_runs(scenario, &refs, k)?);
        }
        let provider = provider.ok_or_else(|| anyhow::anyhow!("no provider metadata"))?;
        let pricing = pricing.ok_or_else(|| anyhow::anyhow!("no pricing profile"))?;
        let trials = trials_per_scenario.unwrap_or(0);
        let harness_faults = runs
            .iter()
            .filter(|run| run.harness_fault.is_some())
            .count();
        let summary = summarize(&scenarios, trials, k, harness_faults);
        let report = Self {
            schema_version: RELIABILITY_SCHEMA_VERSION,
            generated_at: generated_at.into(),
            source: source.into(),
            caveat: RELIABILITY_CAVEAT.to_string(),
            provider,
            pricing,
            selection: ReliabilitySelection {
                scenario_ids: selected
                    .iter()
                    .map(|scenario| scenario.id.clone())
                    .collect(),
                trials,
                k,
            },
            summary,
            scenarios,
        };
        report.validate()?;
        Ok(report)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != RELIABILITY_SCHEMA_VERSION {
            bail!(
                "unsupported reliability schema version {}; expected {RELIABILITY_SCHEMA_VERSION}",
                self.schema_version
            );
        }
        if self.caveat.trim().is_empty() {
            bail!("reliability caveat cannot be empty");
        }
        if self.selection.k == 0 || self.selection.k > MAX_RELIABILITY_K {
            bail!("reliability k must be 1-{MAX_RELIABILITY_K}");
        }
        if self.selection.trials == 0 || self.selection.trials > MAX_RELIABILITY_TRIALS {
            bail!("reliability trials must be 1-{MAX_RELIABILITY_TRIALS}");
        }
        if self.scenarios.len() != self.selection.scenario_ids.len() {
            bail!("reliability scenario rows do not match the selection");
        }
        for (id, row) in self.selection.scenario_ids.iter().zip(&self.scenarios) {
            if id != &row.scenario_id {
                bail!("reliability scenario row {id} does not match its aggregate");
            }
        }
        if self.scenarios.len() > MAX_RELIABILITY_SCENARIOS {
            bail!("reliability report has too many scenarios");
        }
        if self.summary.run_count != self.scenarios.iter().map(|row| row.trials).sum::<usize>() {
            bail!("reliability summary run count does not match the scenario rows");
        }
        if self.summary.k != self.selection.k {
            bail!("reliability summary k does not match the selection");
        }
        Ok(())
    }

    pub fn all_solved(&self) -> bool {
        self.summary.all_solved
    }

    pub fn to_json_bytes(&self) -> Result<Vec<u8>> {
        let bytes = serde_json::to_vec_pretty(self).context("serializing reliability report")?;
        if bytes.len() > MAX_RELIABILITY_BYTES {
            bail!(
                "reliability JSON report is {} bytes, above the {MAX_RELIABILITY_BYTES} byte limit",
                bytes.len()
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

fn summarize(
    scenarios: &[ScenarioReliability],
    trials_per_scenario: usize,
    k: u32,
    harness_faults: usize,
) -> ReliabilitySummary {
    let run_count = scenarios.iter().map(|row| row.trials).sum::<usize>();
    let solved_runs = scenarios.iter().map(|row| row.solved).sum::<usize>();
    let mean = |values: Vec<f64>| -> Option<f64> {
        if values.is_empty() {
            None
        } else {
            Some(values.iter().sum::<f64>() / values.len() as f64)
        }
    };
    let flaky_scenarios = scenarios.iter().filter(|row| row.flaky).count();
    let flake_values = scenarios
        .iter()
        .filter_map(|row| row.flake_rate)
        .collect::<Vec<_>>();
    let total_cost = round_usd(scenarios.iter().map(|row| row.cost_usd).sum::<f64>());
    let coordination =
        CoordinationUsage::combine(scenarios.iter().map(|row| row.coordination.clone()));
    let lucky_pass = summarize_lucky(scenarios);
    ReliabilitySummary {
        scenario_count: scenarios.len(),
        run_count,
        trials_per_scenario,
        k,
        solved_runs,
        unsolved_runs: run_count.saturating_sub(solved_runs),
        harness_faults,
        all_solved: solved_runs == run_count && run_count > 0,
        mean_pass_at_k: mean(
            scenarios
                .iter()
                .filter_map(|row| row.pass_at_k)
                .collect::<Vec<_>>(),
        ),
        mean_pass_power_k: mean(
            scenarios
                .iter()
                .filter_map(|row| row.pass_power_k)
                .collect::<Vec<_>>(),
        ),
        mean_success_variance: mean(
            scenarios
                .iter()
                .filter_map(|row| row.success_variance)
                .collect::<Vec<_>>(),
        ),
        flaky_scenarios,
        flake_rate: mean(flake_values),
        total_cost_usd: total_cost,
        cost_per_solved_task_usd: if solved_runs == 0 {
            None
        } else {
            Some(round_usd(total_cost / solved_runs as f64))
        },
        cost_per_solved_scenario_usd: {
            let solved_scenarios = scenarios.iter().filter(|row| row.solved > 0).count();
            if solved_scenarios == 0 {
                None
            } else {
                Some(round_usd(total_cost / solved_scenarios as f64))
            }
        },
        coordination,
        lucky_pass,
    }
}

fn summarize_lucky(scenarios: &[ScenarioReliability]) -> LuckyPassSummary {
    let mut summary = LuckyPassSummary::default();
    for scenario in scenarios {
        summary.flagged_runs = summary
            .flagged_runs
            .saturating_add(scenario.lucky_pass.flagged_runs);
        summary.solved_flagged_runs = summary
            .solved_flagged_runs
            .saturating_add(scenario.lucky_pass.solved_flagged_runs);
        summary.total_flags = summary
            .total_flags
            .saturating_add(scenario.lucky_pass.total_flags);
        for (code, count) in &scenario.lucky_pass.signal_runs {
            *summary.signal_runs.entry(code.clone()).or_insert(0) += count;
        }
        for (code, count) in &scenario.lucky_pass.signal_counts {
            *summary.signal_counts.entry(code.clone()).or_insert(0) += count;
        }
    }
    let solved = scenarios.iter().map(|row| row.solved).sum::<usize>();
    summary.lucky_pass_rate = if solved == 0 {
        None
    } else {
        Some(summary.solved_flagged_runs as f64 / solved as f64)
    };
    summary
}

fn render_markdown(report: &ReliabilityReport) -> String {
    let mut markdown = String::new();
    markdown.push_str("# VerlinPark reliability report\n\n");
    markdown.push_str(&format!(
        "- Generated: {}\n- Source: {}\n- Provider: {} / {}\n- Model: {}\n- Trials per scenario: {}\n- k: {}\n\n",
        report.generated_at,
        report.source,
        report.provider.mode,
        report.provider.id,
        report.provider.model,
        report.selection.trials,
        report.selection.k
    ));
    markdown.push_str(&format!("> {}\n\n", report.caveat));
    markdown.push_str("| Scenario | Trials | Solved | pass@k | pass^k | Variance | Flake | Coordination | Cost/solved | Lucky runs |\n");
    markdown.push_str("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n");
    for row in &report.scenarios {
        markdown.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            markdown_cell(&row.scenario_id),
            row.trials,
            row.solved,
            format_optional(row.pass_at_k, 4),
            format_optional(row.pass_power_k, 4),
            format_optional(row.success_variance, 4),
            format_optional(row.flake_rate, 4),
            format_optional(row.coordination.ratio, 3),
            format_optional(row.cost_per_solved_task_usd, 6),
            row.lucky_pass.flagged_runs
        ));
    }
    markdown.push_str("\n## Summary\n\n");
    markdown.push_str(&format!(
        "- Runs: {} ({} solved, {} unsolved)\n",
        report.summary.run_count, report.summary.solved_runs, report.summary.unsolved_runs
    ));
    markdown.push_str(&format!(
        "- Mean pass@k: {}\n- Mean pass^k: {}\n- Mean success variance: {}\n",
        format_optional(report.summary.mean_pass_at_k, 4),
        format_optional(report.summary.mean_pass_power_k, 4),
        format_optional(report.summary.mean_success_variance, 4)
    ));
    markdown.push_str(&format!(
        "- Flaky scenarios: {} (mean flake rate {})\n",
        report.summary.flaky_scenarios,
        format_percent(report.summary.flake_rate)
    ));
    markdown.push_str(&format!(
        "- Coordination ratio: {} (orchestration {} + delegation {} / productive {})\n",
        format_optional(report.summary.coordination.ratio, 3),
        report.summary.coordination.orchestration_tokens,
        report.summary.coordination.delegation_tokens,
        report.summary.coordination.productive_tokens
    ));
    markdown.push_str(&format!(
        "- Cost: {} USD total; {} USD per solved task; {} USD per solved scenario\n",
        format_usd(report.summary.total_cost_usd),
        format_optional(report.summary.cost_per_solved_task_usd, 6),
        format_optional(report.summary.cost_per_solved_scenario_usd, 6)
    ));
    markdown.push_str(&format!(
        "- Lucky-Pass: {} flagged run(s), {} flagged solved run(s), rate {}\n",
        report.summary.lucky_pass.flagged_runs,
        report.summary.lucky_pass.solved_flagged_runs,
        format_percent(report.summary.lucky_pass.lucky_pass_rate)
    ));
    if !report.summary.lucky_pass.signal_counts.is_empty() {
        markdown.push_str("\n### Lucky-Pass signals\n\n");
        for (code, count) in &report.summary.lucky_pass.signal_counts {
            markdown.push_str(&format!("- `{code}`: {count}\n"));
        }
    }
    markdown.push_str("\n## Trial outcomes\n\n");
    for row in &report.scenarios {
        markdown.push_str(&format!("### {}\n\n", row.scenario_id));
        for trial in &row.trials_detail {
            markdown.push_str(&format!(
                "- {}: {} · {} USD{} \n",
                trial.run_id,
                if trial.solved { "solved" } else { "unsolved" },
                format_usd(trial.cost_usd),
                if trial.flagged {
                    format!(" · signals: {}", trial.signal_codes.join(", "))
                } else {
                    String::new()
                }
            ));
        }
        markdown.push('\n');
    }
    markdown
}

fn render_csv(report: &ReliabilityReport) -> String {
    let mut csv = String::from(
        "scenario,trials,solved,unsolved,pass_at_k,pass_power_k,success_variance,flake_rate,flaky,coordination_ratio,cost_usd,cost_per_solved_task_usd,cost_per_solved_scenario_usd,lucky_pass_runs,lucky_pass_rate\n",
    );
    for row in &report.scenarios {
        let fields = [
            escape_csv(&row.scenario_id),
            row.trials.to_string(),
            row.solved.to_string(),
            row.unsolved.to_string(),
            format_optional(row.pass_at_k, 6),
            format_optional(row.pass_power_k, 6),
            format_optional(row.success_variance, 6),
            format_optional(row.flake_rate, 6),
            row.flaky.to_string(),
            format_optional(row.coordination.ratio, 6),
            format_usd(row.cost_usd),
            format_optional(row.cost_per_solved_task_usd, 6),
            format_optional(row.cost_per_solved_scenario_usd, 6),
            row.lucky_pass.flagged_runs.to_string(),
            format_optional(row.lucky_pass.lucky_pass_rate, 6),
        ];
        csv.push_str(&fields.join(","));
        csv.push('\n');
    }
    csv
}

fn markdown_cell(value: &str) -> String {
    let mut cell = value.replace('|', "\\|").replace('\n', " ");
    if cell.chars().count() > 160 {
        cell = cell.chars().take(157).collect::<String>() + "...";
    }
    cell
}

fn escape_csv(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReliabilityArtifacts {
    pub json_path: PathBuf,
    pub markdown_path: PathBuf,
    pub csv_path: PathBuf,
    pub stem: String,
}

impl ReliabilityArtifacts {
    pub fn summary_line(&self) -> String {
        format!(
            "reliability report: {} ({} / {} / {})",
            self.stem,
            self.json_path.display(),
            self.markdown_path.display(),
            self.csv_path.display()
        )
    }
}

/// Write JSON, Markdown, and CSV atomically with one shared stem.
pub fn write_reliability_artifacts(
    report: &ReliabilityReport,
    output_dir: &Path,
) -> Result<ReliabilityArtifacts> {
    fs::create_dir_all(output_dir).with_context(|| {
        format!(
            "creating reliability output directory {}",
            output_dir.display()
        )
    })?;
    let now = OffsetDateTime::now_utc();
    let id = Uuid::now_v7().simple().to_string();
    let stem = format!("reliability-{}-{}", now.unix_timestamp(), &id[..8]);
    write_reliability_artifacts_with_stem(report, output_dir, &stem)
}

pub fn write_reliability_artifacts_with_stem(
    report: &ReliabilityReport,
    output_dir: &Path,
    stem: &str,
) -> Result<ReliabilityArtifacts> {
    if stem.is_empty()
        || stem.contains('/')
        || stem.contains('\\')
        || stem.contains("..")
        || stem.starts_with('.')
    {
        bail!("unsafe reliability report stem");
    }
    let json = report.to_json_bytes()?;
    let markdown = report.to_markdown();
    let csv = report.to_csv();
    let json_path = output_dir.join(format!("{stem}.json"));
    let markdown_path = output_dir.join(format!("{stem}.md"));
    let csv_path = output_dir.join(format!("{stem}.csv"));
    write_atomic(&json_path, &json)?;
    write_atomic(&markdown_path, markdown.as_bytes())?;
    write_atomic(&csv_path, csv.as_bytes())?;
    Ok(ReliabilityArtifacts {
        json_path,
        markdown_path,
        csv_path,
        stem: stem.to_string(),
    })
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_RELIABILITY_BYTES {
        bail!(
            "artifact {} is {} bytes, above the {} byte limit",
            path.display(),
            bytes.len(),
            MAX_RELIABILITY_BYTES
        );
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("artifact path has no parent"))?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow::anyhow!("artifact path has no file name"))?;
    let temporary = parent.join(format!(".{name}.{}.tmp", Uuid::now_v7().simple()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .with_context(|| format!("creating temporary artifact {}", temporary.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("writing temporary artifact {}", temporary.display()))?;
    file.sync_all()
        .with_context(|| format!("syncing temporary artifact {}", temporary.display()))?;
    drop(file);
    fs::rename(&temporary, path)
        .with_context(|| format!("renaming {} to {}", temporary.display(), path.display()))?;
    Ok(())
}

/// Load a saved benchmark report and reinterpret its runs as reliability
/// trials. This performs no provider call and no session mutation.
pub fn from_saved_report(path: &Path, selected: &[String], k: u32) -> Result<ReliabilityReport> {
    let bytes =
        fs::read(path).with_context(|| format!("reading benchmark report {}", path.display()))?;
    let report = crate::report::parse_report(&bytes)
        .with_context(|| format!("parsing benchmark report {}", path.display()))?;
    if report.schema_version != BENCH_SCHEMA_VERSION {
        bail!(
            "benchmark report schema {} is not supported; expected {BENCH_SCHEMA_VERSION}",
            report.schema_version
        );
    }
    let scenarios = crate::scenario::Scenario::catalog();
    let mut selected_scenarios = Vec::new();
    let ids = if selected.is_empty() {
        report.selection.scenario_ids.clone()
    } else {
        selected.to_vec()
    };
    for id in &ids {
        let scenario = scenarios
            .iter()
            .find(|scenario| scenario.id == *id)
            .with_context(|| format!("unknown benchmark scenario '{id}'"))?;
        selected_scenarios.push(scenario.clone());
    }
    let k = resolve_k(k, report.selection.repetitions)?;
    let selected_ids = selected_scenarios
        .iter()
        .map(|scenario| scenario.id.as_str())
        .collect::<HashSet<_>>();
    let runs = report
        .runs
        .into_iter()
        .filter(|run| selected_ids.contains(run.scenario_id.as_str()))
        .collect::<Vec<_>>();
    // A saved report must contain the same unique repetitions it declares.
    // Duplicate or missing trials silently change k/flake denominators.
    for scenario_id in &selected_ids {
        let repetitions = runs
            .iter()
            .filter(|run| run.scenario_id == *scenario_id)
            .map(|run| run.repetition)
            .collect::<Vec<_>>();
        if repetitions.len() != report.selection.repetitions as usize {
            bail!(
                "scenario {scenario_id} has {} saved trials, expected {}",
                repetitions.len(),
                report.selection.repetitions
            );
        }
        let unique = repetitions.iter().copied().collect::<HashSet<_>>();
        if unique.len() != repetitions.len() {
            bail!("scenario {scenario_id} has duplicate trial repetitions");
        }
    }
    ReliabilityReport::build(
        &runs,
        &selected_scenarios,
        k,
        format!("report:{}", path.display()),
        now_rfc3339(),
    )
}

/// Resolve `k=0` as `min(trials, 5)`, then enforce the bounded range.
pub fn resolve_k(requested: u32, trials: usize) -> Result<u32> {
    let k = if requested == 0 {
        u32::try_from(trials.min(5)).unwrap_or(0).max(1)
    } else {
        requested
    };
    if trials == 0 {
        bail!("reliability trials must be at least 1");
    }
    if k == 0 || k > MAX_RELIABILITY_K {
        bail!("reliability k must be 1-{MAX_RELIABILITY_K}");
    }
    Ok(k)
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

/// Compute the stable evidence-hash for a sequence of tool outcomes. Kept
/// public for deterministic fixtures and tests.
pub fn evidence_fingerprint(events: &[Event]) -> String {
    let mut hash = sha256_hex(b"bench-evidence-start");
    for event in events {
        if event.kind != "tool.finished" {
            continue;
        }
        let name = event
            .payload
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let is_error = event
            .payload
            .get("is_error")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        hash = extend_evidence_hash(&hash, event, name, is_error);
    }
    hash
}

/// Stable SHA-256 helper for report fingerprints and tests.
pub fn hash_text(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
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
    use serde_json::json;
    use vp_protocol::types::UsageEntry;

    fn event(seq: u64, kind: &str, payload: serde_json::Value) -> Event {
        Event {
            session_id: "ses-1".to_string(),
            seq,
            kind: kind.to_string(),
            payload,
            created_at: format!("2026-11-12T00:00:{seq:02}Z"),
        }
    }

    fn verdict(run_id: &str, repetition: usize, solved: bool) -> RunRecord {
        crate::report::run_record_fixture(run_id, repetition, solved)
    }

    fn usage(role: &str, task: Option<&str>, input: u64, output: u64) -> UsageEntry {
        UsageEntry {
            id: format!("usage-{role}-{input}"),
            session_id: "ses-1".into(),
            agent_id: "a".into(),
            agent_name: role.into(),
            role: role.into(),
            task_id: task.map(str::to_string),
            department: None,
            model: "model".into(),
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            estimated: false,
            cost_usd: 0.0,
            created_at: "2026-11-12T00:00:00Z".into(),
        }
    }

    #[test]
    fn pass_at_k_matches_deterministic_combination_math() {
        assert_eq!(pass_at_k(5, 0, 1), Some(0.0));
        assert_eq!(pass_at_k(5, 5, 1), Some(1.0));
        assert!((pass_at_k(5, 1, 1).unwrap() - 0.2).abs() < 1e-12);
        assert!((pass_at_k(5, 1, 2).unwrap() - 0.4).abs() < 1e-12);
        assert_eq!(pass_at_k(5, 0, 2), Some(0.0));
        assert_eq!(pass_at_k(5, 1, 5), Some(1.0));
        assert_eq!(pass_at_k(5, 1, 6), None);
        assert_eq!(pass_at_k(5, 1, 0), None);
        assert_eq!(pass_at_k(0, 0, 1), None);
        assert!((pass_at_k(10, 3, 3).unwrap() - (1.0 - 35.0 / 120.0)).abs() < 1e-12);
    }

    #[test]
    fn pass_power_k_matches_all_success_math() {
        assert_eq!(pass_power_k(5, 0, 1), Some(0.0));
        assert_eq!(pass_power_k(5, 5, 1), Some(1.0));
        assert_eq!(pass_power_k(5, 1, 2), Some(0.0));
        assert!((pass_power_k(5, 2, 2).unwrap() - 0.1).abs() < 1e-12);
        assert_eq!(pass_power_k(5, 5, 5), Some(1.0));
        assert_eq!(pass_power_k(5, 2, 6), None);
    }

    #[test]
    fn variance_and_flake_rate_are_stable_and_bounded() {
        assert_eq!(success_variance(1, 1), None);
        assert_eq!(success_variance(2, 1), Some(0.5));
        assert!((success_variance(3, 1).unwrap() - 1.0 / 3.0).abs() < 1e-12);
        assert_eq!(flake_rate(&[true]), None);
        assert_eq!(flake_rate(&[true, true, true]), Some(0.0));
        assert_eq!(flake_rate(&[true, false, true]), Some(1.0));
        assert_eq!(flake_rate(&[true, true, false]), Some(0.5));
    }

    #[test]
    fn commit_without_passing_test_is_deterministic() {
        let events = vec![
            event(
                1,
                "tool.finished",
                json!({
                    "name": "write_file",
                    "structured": {
                        "path": "src/lib.rs",
                        "before_sha256": "aaaa",
                        "sha256": "bbbb",
                        "dry_run": false
                    }
                }),
            ),
            event(
                2,
                "session.committed",
                json!({"commit": "abc", "message": "SECRET"}),
            ),
        ];
        let evidence = analyze_events(&events);
        assert_eq!(evidence.commit_without_passing_test, 1);
        assert!(evidence.has_code(lucky_codes::COMMIT_WITHOUT_PASSING_TEST));
    }

    #[test]
    fn a_passing_test_after_the_last_implementation_clears_the_commit_signal() {
        let events = vec![
            event(
                1,
                "tool.finished",
                json!({
                    "name": "write_file",
                    "structured": {
                        "path": "src/lib.rs",
                        "before_sha256": "aaaa",
                        "sha256": "bbbb"
                    }
                }),
            ),
            event(
                2,
                "tool.finished",
                json!({
                    "name": "run_project_task",
                    "is_error": false,
                    "structured": {
                        "command": "cargo test -p vp-core",
                        "exit_code": 0
                    }
                }),
            ),
            event(3, "session.committed", json!({"commit": "abc"})),
        ];
        let evidence = analyze_events(&events);
        assert_eq!(evidence.tests_passed, 1);
        assert_eq!(evidence.commit_without_passing_test, 0);
        assert!(!evidence.has_code(lucky_codes::COMMIT_WITHOUT_PASSING_TEST));
    }

    #[test]
    fn a_failed_test_does_not_clear_the_commit_signal() {
        let events = vec![
            event(
                1,
                "tool.finished",
                json!({
                    "name": "edit_file",
                    "structured": {
                        "path": "src/lib.rs",
                        "before_sha256": "aaaa",
                        "after_sha256": "bbbb"
                    }
                }),
            ),
            event(
                2,
                "tool.finished",
                json!({
                    "name": "run_project_task",
                    "is_error": true,
                    "structured": {"command": "cargo test", "exit_code": 1}
                }),
            ),
            event(3, "merge_queue.enqueued", json!({"entry_id": "m1"})),
        ];
        let evidence = analyze_events(&events);
        assert_eq!(evidence.tests_failed, 1);
        assert_eq!(evidence.commit_without_passing_test, 1);
    }

    #[test]
    fn retry_without_new_evidence_counts_only_repeated_retries() {
        let base = vec![
            event(1, "agent.stream_retry", json!({"attempt": 1})),
            event(2, "agent.stream_retry", json!({"attempt": 2})),
        ];
        let evidence = analyze_events(&base);
        assert_eq!(evidence.retries, 2);
        assert_eq!(evidence.retry_without_new_evidence, 1);
        assert!(evidence.has_code(lucky_codes::RETRY_WITHOUT_NEW_EVIDENCE));

        let with_evidence = vec![
            event(1, "agent.stream_retry", json!({"attempt": 1})),
            event(
                2,
                "tool.finished",
                json!({
                    "name": "read_file",
                    "tool_call_id": "c1",
                    "is_error": false,
                    "structured": {"path": "README.md"}
                }),
            ),
            event(3, "agent.stream_retry", json!({"attempt": 2})),
        ];
        let evidence = analyze_events(&with_evidence);
        assert_eq!(evidence.retry_without_new_evidence, 0);
    }

    #[test]
    fn edit_revert_cycle_requires_same_path_and_prior_hash() {
        let events = vec![
            event(
                1,
                "tool.finished",
                json!({
                    "name": "edit_file",
                    "structured": {
                        "path": "src/lib.rs",
                        "before_sha256": "aaaa",
                        "after_sha256": "bbbb"
                    }
                }),
            ),
            event(
                2,
                "tool.finished",
                json!({
                    "name": "edit_file",
                    "structured": {
                        "path": "src/lib.rs",
                        "before_sha256": "bbbb",
                        "after_sha256": "aaaa"
                    }
                }),
            ),
        ];
        let evidence = analyze_events(&events);
        assert_eq!(evidence.edit_revert_cycles, 1);
        assert!(evidence.has_code(lucky_codes::EDIT_REVERT_CYCLE));

        let other_path = vec![
            event(
                1,
                "tool.finished",
                json!({
                    "name": "edit_file",
                    "structured": {
                        "path": "src/a.rs",
                        "before_sha256": "aaaa",
                        "after_sha256": "bbbb"
                    }
                }),
            ),
            event(
                2,
                "tool.finished",
                json!({
                    "name": "edit_file",
                    "structured": {
                        "path": "src/b.rs",
                        "before_sha256": "bbbb",
                        "after_sha256": "aaaa"
                    }
                }),
            ),
        ];
        assert_eq!(analyze_events(&other_path).edit_revert_cycles, 0);
    }

    #[test]
    fn verify_before_implement_and_test_before_implement_are_ordering_only() {
        let events = vec![
            event(
                1,
                "tool.finished",
                json!({
                    "name": "run_project_task",
                    "is_error": false,
                    "structured": {"command": "cargo test", "exit_code": 0}
                }),
            ),
            event(
                2,
                "tool.finished",
                json!({
                    "name": "submit_verdict",
                    "is_error": false,
                    "structured": {"approved": true}
                }),
            ),
            event(
                3,
                "tool.finished",
                json!({
                    "name": "write_file",
                    "structured": {"path": "src/lib.rs", "sha256": "bbbb"}
                }),
            ),
        ];
        let evidence = analyze_events(&events);
        assert!(evidence.test_before_implement);
        assert!(evidence.verify_before_implement);
        assert!(evidence.has_code(lucky_codes::TEST_BEFORE_IMPLEMENT));
        assert!(evidence.has_code(lucky_codes::VERIFY_BEFORE_IMPLEMENT));
    }

    #[test]
    fn apply_patch_files_are_inspected_individually() {
        let events = vec![event(
            1,
            "tool.finished",
            json!({
                "name": "apply_patch",
                "structured": {
                    "files": [
                        {"path": "a.rs", "before_sha256": "1111", "after_sha256": "2222"},
                        {"path": "b.rs", "before_sha256": "3333", "after_sha256": "4444"}
                    ]
                }
            }),
        )];
        let evidence = analyze_events(&events);
        assert_eq!(evidence.implementations, 1);
        assert_eq!(evidence.first_implementation_seq, Some(1));
    }

    #[test]
    fn coordination_overhead_uses_role_and_task_attribution() {
        let entries = vec![
            usage("leader", None, 100, 20),
            usage("manager", Some("T1"), 50, 10),
            usage("verifier", Some("T1"), 30, 5),
            usage("worker", Some("T1"), 400, 100),
            usage("labour", Some("T1"), 200, 50),
            usage("worker", None, 40, 10),
            usage("scout", None, 25, 5),
            usage("future-role", None, 10, 0),
        ];
        let usage = CoordinationUsage::from_entries(&entries);
        assert_eq!(usage.orchestration_tokens, 215);
        assert_eq!(usage.productive_tokens, 750);
        assert_eq!(usage.untasked_worker_tokens, 50);
        assert_eq!(usage.scout_tokens, 30);
        assert_eq!(usage.delegation_tokens, 90);
        assert_eq!(usage.total_tokens, 1055);
        assert_eq!(usage.numerator_tokens, 305);
        assert_eq!(usage.denominator_tokens, 750);
        assert!((usage.ratio.unwrap() - (305.0 / 750.0)).abs() < 1e-12);
        assert!((usage.coordination_share.unwrap() - 305.0 / 1055.0).abs() < 1e-12);
    }

    #[test]
    fn coordination_overhead_is_none_without_productive_tokens() {
        let usage = CoordinationUsage::from_entries(&[usage("leader", None, 100, 0)]);
        assert_eq!(usage.productive_tokens, 0);
        assert_eq!(usage.ratio, None);
        assert_eq!(usage.coordination_share, Some(1.0));
    }

    #[test]
    fn scenario_reliability_combines_trials_and_flags() {
        let scenario = crate::scenario::Scenario::find("baseline-single-session").unwrap();
        let mut first = verdict("run-1", 1, true);
        first.metrics.cost_usd = 0.5;
        first.trial_evidence = analyze_events(&[
            event(
                1,
                "tool.finished",
                json!({
                    "name": "write_file",
                    "structured": {"path": "a", "before_sha256": "aaaa", "sha256": "bbbb"}
                }),
            ),
            event(2, "session.committed", json!({"commit": "abc"})),
        ]);
        let mut second = verdict("run-2", 2, false);
        second.metrics.cost_usd = 0.5;
        let row = ScenarioReliability::from_runs(&scenario, &[&first, &second], 2).unwrap();
        assert_eq!(row.trials, 2);
        assert_eq!(row.solved, 1);
        assert_eq!(row.flake_rate, Some(1.0));
        assert!(row.flaky);
        assert_eq!(row.pass_at_k, Some(1.0));
        assert_eq!(row.pass_power_k, Some(0.0));
        assert_eq!(row.lucky_pass.flagged_runs, 1);
        assert_eq!(row.lucky_pass.solved_flagged_runs, 1);
        assert_eq!(row.lucky_pass.lucky_pass_rate, Some(1.0));
        assert_eq!(row.cost_per_solved_task_usd, Some(1.0));
        assert_eq!(row.cost_per_solved_scenario_usd, Some(1.0));
    }

    #[test]
    fn report_builds_per_scenario_only_and_round_trips() {
        let scenario = crate::scenario::Scenario::find("baseline-single-session").unwrap();
        let runs = vec![
            verdict("run-1", 1, true),
            verdict("run-2", 2, true),
            verdict("run-3", 3, false),
        ];
        let report = ReliabilityReport::build(
            &runs,
            std::slice::from_ref(&scenario),
            2,
            "test",
            "2026-11-12T00:00:00Z",
        )
        .unwrap();
        assert_eq!(report.selection.trials, 3);
        assert_eq!(report.summary.run_count, 3);
        assert_eq!(report.summary.solved_runs, 2);
        assert_eq!(report.summary.flaky_scenarios, 1);
        assert_eq!(report.scenarios[0].pass_at_k, Some(1.0));
        assert_eq!(report.scenarios[0].pass_power_k, Some(1.0 / 3.0));
        let json = report.to_json_bytes().unwrap();
        let decoded: ReliabilityReport = serde_json::from_slice(&json).unwrap();
        assert_eq!(decoded.selection, report.selection);
        assert!(report.to_markdown().contains("universal ranking"));
        assert!(report.to_csv().contains("pass_at_k"));
        assert!(report.validate().is_ok());
    }

    #[test]
    fn report_rejects_inconsistent_trial_counts_and_unknown_scenarios() {
        let scenario = crate::scenario::Scenario::find("baseline-single-session").unwrap();
        let runs = vec![verdict("run-1", 1, true), verdict("run-2", 2, true)];
        let mut selected = vec![scenario.clone()];
        selected.push(crate::scenario::Scenario::find("plan-worker-chain").unwrap());
        assert!(
            ReliabilityReport::build(&runs, &selected, 2, "test", "now")
                .unwrap_err()
                .to_string()
                .contains("has no trials")
        );
    }

    #[test]
    fn saved_report_analysis_performs_no_provider_call() {
        let scenario = crate::scenario::Scenario::find("baseline-single-session").unwrap();
        let runs = vec![verdict("run-1", 1, true), verdict("run-2", 2, false)];
        let bench_report = crate::report::BenchReport::build(
            runs,
            std::slice::from_ref(&scenario),
            2,
            "2026-11-12T00:00:00Z",
            None,
            None,
            crate::report::HostMetadata::detect(),
        )
        .unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let artifacts =
            crate::report::write_artifacts_with_stem(&bench_report, dir.path(), "saved").unwrap();
        let reliability = from_saved_report(&artifacts.json_path, &[], 2).unwrap();
        assert_eq!(
            reliability.source,
            format!("report:{}", artifacts.json_path.display())
        );
        assert_eq!(reliability.selection.trials, 2);
        assert_eq!(reliability.scenarios[0].solved, 1);
    }

    #[test]
    fn artifacts_are_atomic_and_bounded() {
        let scenario = crate::scenario::Scenario::find("baseline-single-session").unwrap();
        let runs = vec![verdict("run-1", 1, true), verdict("run-2", 2, true)];
        let report = ReliabilityReport::build(
            &runs,
            std::slice::from_ref(&scenario),
            2,
            "test",
            "2026-11-12T00:00:00Z",
        )
        .unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let artifacts =
            write_reliability_artifacts_with_stem(&report, dir.path(), "reliability-test").unwrap();
        for path in [
            &artifacts.json_path,
            &artifacts.markdown_path,
            &artifacts.csv_path,
        ] {
            assert!(path.exists());
            assert!(fs::metadata(path).unwrap().len() <= MAX_RELIABILITY_BYTES as u64);
        }
        let leftovers = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".reliability-test")
            })
            .count();
        assert_eq!(leftovers, 0);
        assert!(write_reliability_artifacts_with_stem(&report, dir.path(), "../escape").is_err());
    }

    #[test]
    fn evidence_fingerprint_changes_only_with_tool_evidence() {
        let empty = evidence_fingerprint(&[]);
        let retry_only =
            evidence_fingerprint(&[event(1, "agent.stream_retry", json!({"attempt": 1}))]);
        assert_eq!(empty, retry_only);
        let tool = evidence_fingerprint(&[event(
            1,
            "tool.finished",
            json!({"name": "read_file", "tool_call_id": "c", "is_error": false}),
        )]);
        assert_ne!(empty, tool);
        assert_eq!(hash_text("abc").len(), 64);
    }
}

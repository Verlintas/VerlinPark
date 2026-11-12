//! Controller replay wire types (ADR-0196).
//!
//! Controller replay re-runs the **deterministic** orchestration/scaling
//! controllers against recorded inputs and diffs the recorded decision. It
//! never replays model output, never calls a provider, never executes a tool,
//! and never mutates a plan, budget, capacity, approval, Verifier, or merge
//! state. Results are labelled "controller-equivalent only" and are not
//! verification or merge evidence.

use serde::{Deserialize, Serialize};

/// Result for one recorded decision.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControllerReplayStatus {
    #[default]
    Matched,
    Mismatched,
    Skipped,
}

impl ControllerReplayStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Matched => "matched",
            Self::Mismatched => "mismatched",
            Self::Skipped => "skipped",
        }
    }
}

/// One bounded field difference. Strings are capped by the daemon.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerReplayDiff {
    #[serde(default)]
    pub field: String,
    #[serde(default)]
    pub recorded: String,
    #[serde(default)]
    pub recomputed: String,
}

/// One recorded deterministic decision and its recomputed outcome.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerReplayEntry {
    /// Event sequence of the recorded decision.
    #[serde(default)]
    pub seq: u64,
    /// `orchestration.decision`, `scaling.decision`, `plan_task.started`, or
    /// `session.resume`.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub recorded_at: Option<String>,
    #[serde(default)]
    pub status: ControllerReplayStatus,
    #[serde(default)]
    pub recorded_action: Option<String>,
    #[serde(default)]
    pub recomputed_action: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub changes: Vec<ControllerReplayDiff>,
    /// Human-readable bounded explanation for `skipped`/`mismatched`.
    #[serde(default)]
    pub reason: Option<String>,
}

/// Bounded replay report. `limit` is the requested decision window; `truncated`
/// is true when older decisions exist outside it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerReplayReport {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub limit: u32,
    #[serde(default)]
    pub window_start_seq: u64,
    #[serde(default)]
    pub window_end_seq: u64,
    /// Number of decision-bearing events in the full session log.
    #[serde(default)]
    pub total_recorded: u64,
    #[serde(default)]
    pub examined: u64,
    #[serde(default)]
    pub matched: u64,
    #[serde(default)]
    pub mismatched: u64,
    #[serde(default)]
    pub skipped: u64,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub entries: Vec<ControllerReplayEntry>,
    /// Current policy hash used for the recomputation, when available.
    #[serde(default)]
    pub policy_hash: String,
    /// Always true. Kept explicit so no surface can mistake the report for
    /// model replay or verification evidence.
    #[serde(default)]
    pub controller_equivalent_only: bool,
    #[serde(default)]
    pub generated_at: String,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ControllerReplayParams {
    pub session_id: String,
    /// Decision window, 1..=2000. Default 200.
    #[serde(default)]
    pub limit: Option<u32>,
}

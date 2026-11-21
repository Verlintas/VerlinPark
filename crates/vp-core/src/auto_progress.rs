//! Deterministic no-progress evidence for Auto Budget.
//!
//! This module consumes the persisted session event stream and classifies
//! **real progress evidence**. It never reads model prose, never calls a
//! provider or a tool, and never mutates a plan, budget, approval, worktree, or
//! session status. It is a pure, bounded state machine that the runtime can
//! consult at a turn boundary:
//!
//! 1. feed each new [`vp_protocol::types::Event`] to
//!    [`ProgressTracker::observe`];
//! 2. call [`ProgressTracker::finish_turn`] once after every completed turn,
//!    including turns that produced no event;
//! 3. call [`ProgressTracker::decide`] with a [`NoProgressPolicy`]; and
//! 4. when the decision is [`NoProgressDecision::Pause`] or
//!    [`NoProgressDecision::Escalate`], emit
//!    [`ProgressTracker::event_payload`] as bounded evidence and let the
//!    existing session controller perform the action.
//!
//! The tracker is advisory input to that deterministic controller. Pausing is
//! still an ordinary cooperative session pause; this module cannot pause or
//! cancel anything by itself.
//!
//! # Progress predicates
//!
//! The following events are progress evidence:
//!
//! | Event kind | Evidence | Success |
//! | --- | --- | --- |
//! | `session.committed` | `commit` | yes |
//! | `session.landed` | `merge` | yes |
//! | `merge_queue.merged`, or `merge_queue.updated` with `entry.status=merged` | `merge` | yes |
//! | `agent.finished` with `merged=true` | `merge` | yes |
//! | `agent.finished` with a non-empty `commit` | `commit` | `status=idle` and not `approved=false` |
//! | `agent.finished` with `integrator=true, merged=false` | `integration_failed` | no |
//! | `agent.finished` with `agent_role=verifier` | `verifier_approved` / `verifier_rejected` | verdict |
//! | `plan_task.finished` | `plan_task_finished` | `task_status=completed` |
//! | `test_cache.stored` | `test_passed` / `test_failed` | cached status |
//! | `tool.finished` for `run_project_task` | `test_passed` / `test_failed` | structured `ok` |
//! | `artifact.published`, `artifact.version_published` | `artifact_published` | yes |
//! | `session.checkpoint` | `session_checkpoint` | yes |
//! | `agent.workspace.landed` / `workspace.land` / `workspace.landed` | `workspace_landed` | yes |
//! | `tool.finished` for `write_file`, `edit_file`, `apply_patch`, `file_manage` | `file_changed` | yes |
//! | `effect.settled` with a `done` workspace-write/patch/manage, commit, or merge receipt | matching kind | yes |
//!
//! Everything else is non-progress. In particular model text
//! (`agent.message`, `agent.message_delta`, `agent.thought_delta`), read-only
//! tools, retries (`plan_task.started`, `plan_task.retried`,
//! `plan_task.retry_scheduled`), queue bookkeeping, approvals, context/cache/
//! usage telemetry, and budget telemetry never reset the counter. Failed tests
//! and failed Verifier/integration outcomes are still *new evidence* and do
//! reset the counter, but they are marked `success=false`; they never count as
//! task success.
//!
//! # Deduplication and bounds
//!
//! Evidence identity is `sha256(kind, success, target)` where `target` contains
//! only identifiers, paths, short hashes, and test digests. No file content,
//! command text, model text, tool output, URL, or credential is copied into the
//! state. A repeated `test_cache.stored`, the same commit observed twice, or
//! the same file path with the same content hash therefore does not reset
//! `turns_without_progress`.
//!
//! The tracker retains a bounded recent-evidence list plus a bounded
//! recent-hash window (128 by default, 512 maximum). Deduplication is exact
//! within that window; the event stream remains the authority if a caller wants
//! to replay a wider history. All strings are single-line, control characters
//! are removed, and every list/string is truncated deterministically.
//!
//! # Operator resume
//!
//! [`ProgressTracker::reset_on_operator_resume`] is the only API that clears
//! the counter without progress evidence. It records an operator reset, keeps
//! the actual last-progress evidence, and excuses the current turn so a
//! read-only turn immediately after resume cannot re-trigger the guard. It
//! does not clear evidence history, budgets, or session status.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use vp_protocol::types::Event;

/// Default no-progress threshold in completed turns.
pub const DEFAULT_NO_PROGRESS_TURNS: u32 = 6;
/// Hard ceiling for a no-progress threshold (matches the run turn ceiling).
pub const MAX_NO_PROGRESS_TURNS: u32 = 200;
/// Default number of recent evidence rows retained for display.
pub const DEFAULT_MAX_RECENT_EVIDENCE: usize = 12;
/// Hard ceiling for retained evidence rows.
pub const MAX_RECENT_EVIDENCE_CEILING: usize = 64;
/// Default number of recent evidence hashes retained for deduplication.
pub const DEFAULT_MAX_SEEN_HASHES: usize = 128;
/// Hard ceiling for the recent-hash window.
pub const MAX_SEEN_HASHES_CEILING: usize = 512;
/// Maximum characters in one bounded target label.
pub const MAX_TARGET_CHARS: usize = 160;
/// Maximum characters in a session or task identifier retained in state.
pub const MAX_ID_CHARS: usize = 200;
/// Maximum characters in an event-kind label retained in evidence.
pub const MAX_EVENT_KIND_CHARS: usize = 80;
/// Maximum evidence rows included in one event payload.
pub const PAYLOAD_EVIDENCE_LIMIT: usize = 8;
/// Schema label for the no-progress event payload.
pub const NO_PROGRESS_PAYLOAD_SCHEMA: &str = "verlinpark.no_progress/v1";

/// Deterministic action selected when the threshold is reached.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoProgressAction {
    /// Cooperatively pause the session at the next safe boundary.
    #[default]
    Pause,
    /// Create an attention/coordination record instead of pausing.
    Escalate,
}

impl NoProgressAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pause => "pause",
            Self::Escalate => "escalate",
        }
    }

    pub fn decision(self) -> NoProgressDecision {
        match self {
            Self::Pause => NoProgressDecision::Pause,
            Self::Escalate => NoProgressDecision::Escalate,
        }
    }
}

/// Pure decision for the runtime controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoProgressDecision {
    Continue,
    Pause,
    Escalate,
}

impl NoProgressDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Continue => "continue",
            Self::Pause => "pause",
            Self::Escalate => "escalate",
        }
    }

    /// True when the controller should act at the next safe boundary.
    pub fn is_stop(self) -> bool {
        !matches!(self, Self::Continue)
    }

    pub fn reason_code(self) -> &'static str {
        match self {
            Self::Continue => "below_threshold",
            Self::Pause | Self::Escalate => "no_progress",
        }
    }
}

/// Bounded threshold/action pair. `no_progress_turns=0` is normalized to `1`
/// rather than disabling the guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NoProgressPolicy {
    pub no_progress_turns: u32,
    pub action: NoProgressAction,
}

impl Default for NoProgressPolicy {
    fn default() -> Self {
        Self {
            no_progress_turns: DEFAULT_NO_PROGRESS_TURNS,
            action: NoProgressAction::Pause,
        }
    }
}

impl NoProgressPolicy {
    pub fn new(no_progress_turns: u32, action: NoProgressAction) -> Self {
        Self {
            no_progress_turns,
            action,
        }
        .effective()
    }

    /// Clamp invalid values into the documented range. The threshold is never
    /// treated as "disabled"; a zero value becomes one completed turn.
    pub fn effective(&self) -> Self {
        Self {
            no_progress_turns: self.no_progress_turns.clamp(1, MAX_NO_PROGRESS_TURNS),
            action: self.action,
        }
    }
}

/// Bounded retention for tracker state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProgressTrackerLimits {
    pub max_recent_evidence: usize,
    pub max_seen_hashes: usize,
}

impl Default for ProgressTrackerLimits {
    fn default() -> Self {
        Self {
            max_recent_evidence: DEFAULT_MAX_RECENT_EVIDENCE,
            max_seen_hashes: DEFAULT_MAX_SEEN_HASHES,
        }
    }
}

impl ProgressTrackerLimits {
    pub fn effective(&self) -> Self {
        Self {
            max_recent_evidence: self
                .max_recent_evidence
                .clamp(1, MAX_RECENT_EVIDENCE_CEILING),
            max_seen_hashes: self.max_seen_hashes.clamp(1, MAX_SEEN_HASHES_CEILING),
        }
    }
}

/// One classified progress kind. Failed tests, rejected verdicts, and failed
/// integration are evidence with `success=false`, not task success.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressEvidenceKind {
    Commit,
    Merge,
    TestPassed,
    TestFailed,
    PlanTaskFinished,
    ArtifactPublished,
    WorkspaceLanded,
    SessionCheckpoint,
    FileChanged,
    VerifierApproved,
    VerifierRejected,
    IntegrationFailed,
}

impl ProgressEvidenceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Merge => "merge",
            Self::TestPassed => "test_passed",
            Self::TestFailed => "test_failed",
            Self::PlanTaskFinished => "plan_task_finished",
            Self::ArtifactPublished => "artifact_published",
            Self::WorkspaceLanded => "workspace_landed",
            Self::SessionCheckpoint => "session_checkpoint",
            Self::FileChanged => "file_changed",
            Self::VerifierApproved => "verifier_approved",
            Self::VerifierRejected => "verifier_rejected",
            Self::IntegrationFailed => "integration_failed",
        }
    }
}

/// A bounded, secret-free progress evidence row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProgressEvidence {
    pub kind: ProgressEvidenceKind,
    pub success: bool,
    /// Session sequence number of the event that produced this evidence.
    pub seq: u64,
    /// Turn in which the evidence was observed.
    pub turn: u64,
    /// Bounded label built only from identifiers, paths, and short digests.
    pub target: String,
    /// Source event kind.
    pub event_kind: String,
    /// Stable identity hash. Never contains file content or model text.
    pub hash: String,
}

impl Default for ProgressEvidence {
    fn default() -> Self {
        Self {
            kind: ProgressEvidenceKind::Commit,
            success: false,
            seq: 0,
            turn: 0,
            target: String::new(),
            event_kind: String::new(),
            hash: String::new(),
        }
    }
}

impl ProgressEvidence {
    /// Return a copy with every persisted string re-bounded. Used by both the
    /// trust-boundary normalizer and the payload builder so a manually loaded
    /// state cannot emit an oversized event.
    fn bounded(&self) -> Self {
        Self {
            kind: self.kind,
            success: self.success,
            seq: self.seq,
            turn: self.turn,
            target: safe_token(&self.target, MAX_TARGET_CHARS),
            event_kind: safe_token(&self.event_kind, MAX_EVENT_KIND_CHARS),
            hash: normalize_hash(&self.hash),
        }
    }

    fn from_classified(
        classified: &ClassifiedProgress,
        seq: u64,
        turn: u64,
        event_kind: &str,
    ) -> Self {
        let target = bounded_single_line(&classified.target, MAX_TARGET_CHARS);
        let hash = evidence_hash(classified.kind, classified.success, &target);
        Self {
            kind: classified.kind,
            success: classified.success,
            seq,
            turn,
            target,
            event_kind: safe_token(event_kind, MAX_EVENT_KIND_CHARS),
            hash,
        }
    }
}

/// One progress candidate produced by [`classify_event`]. `target` is already
/// bounded and contains no file content or model prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedProgress {
    pub kind: ProgressEvidenceKind,
    pub success: bool,
    pub target: String,
}

impl ClassifiedProgress {
    fn new(kind: ProgressEvidenceKind, success: bool, target: impl Into<String>) -> Self {
        Self {
            kind,
            success,
            target: bounded_single_line(&target.into(), MAX_TARGET_CHARS),
        }
    }
}

/// Why an event is not progress. This is diagnostic only; all variants leave
/// the tracker unchanged apart from turn accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NonProgressReason {
    /// Assistant text, deltas, thoughts, or other model prose.
    ModelText,
    /// A known read-only tool result.
    ReadOnlyTool,
    /// A tool result with no recognized durable effect.
    ToolWithoutEffect,
    /// A dry-run file operation.
    DryRun,
    /// A failed or incomplete file operation.
    FailedTool,
    /// A write that reported an unchanged file.
    NoOp,
    /// Context, cache, usage, budget, or cost telemetry.
    Telemetry,
    /// Plan task start/retry/pause bookkeeping.
    PlanBookkeeping,
    /// Merge queue enqueue/retry/integration bookkeeping.
    QueueBookkeeping,
    /// Session run journal, continuation, lease, or lifecycle bookkeeping.
    RunBookkeeping,
    /// Approval lifecycle bookkeeping.
    ApprovalBookkeeping,
    /// Agent lifecycle bookkeeping.
    AgentBookkeeping,
    /// Session lifecycle or presentation bookkeeping.
    SessionBookkeeping,
    /// An event outside the explicit progress predicate table.
    Unclassified,
}

/// Result of the pure event classifier before deduplication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventClassification {
    Progress(Vec<ClassifiedProgress>),
    NonProgress(NonProgressReason),
}

/// Result of [`ProgressTracker::observe`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObserveOutcome {
    /// New evidence accepted into state, in event order.
    pub accepted: Vec<ProgressEvidence>,
    /// Evidence whose stable hash was already in the retained window.
    pub duplicates: Vec<ProgressEvidence>,
    /// Non-progress reason, when the event was classified as non-progress.
    pub non_progress: Option<NonProgressReason>,
    /// The event sequence was already observed or predates an operator reset.
    pub stale: bool,
    /// The event belongs to a different session and was ignored.
    pub session_mismatch: bool,
}

impl ObserveOutcome {
    pub fn is_progress(&self) -> bool {
        !self.accepted.is_empty()
    }

    pub fn is_duplicate(&self) -> bool {
        !self.duplicates.is_empty()
    }
}

/// Bounded summary for `session.auto_paused` / escalation payloads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NoProgressSummary {
    pub session_id: String,
    pub task_id: Option<String>,
    pub decision: NoProgressDecision,
    pub action: NoProgressAction,
    pub threshold: u32,
    pub reason: String,
    pub turns_without_progress: u32,
    pub evidence_count: u64,
    pub last_progress_seq: u64,
    pub last_progress_kind: Option<ProgressEvidenceKind>,
    pub last_evidence_hash: Option<String>,
    pub last_observed_seq: u64,
    pub operator_resets: u32,
    /// Newest evidence first, bounded to [`PAYLOAD_EVIDENCE_LIMIT`].
    pub recent_evidence: Vec<ProgressEvidence>,
    pub evidence_truncated: bool,
}

impl Default for NoProgressSummary {
    fn default() -> Self {
        Self {
            session_id: String::new(),
            task_id: None,
            decision: NoProgressDecision::Continue,
            action: NoProgressAction::Pause,
            threshold: DEFAULT_NO_PROGRESS_TURNS,
            reason: NoProgressDecision::Continue.reason_code().to_string(),
            turns_without_progress: 0,
            evidence_count: 0,
            last_progress_seq: 0,
            last_progress_kind: None,
            last_evidence_hash: None,
            last_observed_seq: 0,
            operator_resets: 0,
            recent_evidence: Vec::new(),
            evidence_truncated: false,
        }
    }
}

impl NoProgressSummary {
    /// Bounded JSON payload suitable for an event or attention record.
    pub fn event_payload(&self) -> Value {
        let evidence = self
            .recent_evidence
            .iter()
            .take(PAYLOAD_EVIDENCE_LIMIT)
            .map(ProgressEvidence::bounded)
            .collect::<Vec<_>>();
        json!({
            "schema": NO_PROGRESS_PAYLOAD_SCHEMA,
            "session_id": bounded_single_line(&self.session_id, MAX_ID_CHARS),
            "task_id": self.task_id.as_deref().map(|value| bounded_single_line(value, MAX_ID_CHARS)),
            "decision": self.decision,
            "action": self.action,
            "threshold": self.threshold,
            "reason": self.decision.reason_code(),
            "turns_without_progress": self.turns_without_progress,
            "evidence_count": self.evidence_count,
            "last_progress_seq": self.last_progress_seq,
            "last_progress_kind": self.last_progress_kind,
            "last_evidence_hash": self.last_evidence_hash.as_deref().map(normalize_hash).filter(|hash| !hash.is_empty()),
            "last_observed_seq": self.last_observed_seq,
            "operator_resets": self.operator_resets,
            "recent_evidence": evidence,
            "evidence_truncated": self.evidence_truncated || self.recent_evidence.len() > PAYLOAD_EVIDENCE_LIMIT,
        })
    }
}

/// Deterministic per-session (or per-task) progress tracker.
///
/// The struct is serde-friendly so the daemon can persist it in
/// `session_auto_budgets.state_json`. Deserialize, then call
/// [`ProgressTracker::normalized`] before use at a trust boundary.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProgressTracker {
    session_id: String,
    task_id: Option<String>,
    turn: u64,
    turn_open: bool,
    turn_had_progress: bool,
    turns_without_progress: u32,
    evidence_count: u64,
    last_progress_seq: u64,
    last_observed_seq: u64,
    last_progress_kind: Option<ProgressEvidenceKind>,
    last_evidence_hash: Option<String>,
    operator_resets: u32,
    last_operator_reset_seq: u64,
    /// Oldest to newest.
    recent_evidence: Vec<ProgressEvidence>,
    /// Oldest to newest stable evidence hashes.
    recent_hashes: VecDeque<String>,
    limits: ProgressTrackerLimits,
}

impl ProgressTracker {
    pub fn new(session_id: impl Into<String>, task_id: Option<String>) -> Self {
        Self {
            session_id: bounded_single_line(&session_id.into(), MAX_ID_CHARS),
            task_id: task_id.map(|value| bounded_single_line(&value, MAX_ID_CHARS)),
            ..Self::default()
        }
    }

    /// Replace the retention bounds. Values are clamped by [`Self::normalized`].
    pub fn with_limits(mut self, limits: ProgressTrackerLimits) -> Self {
        self.limits = limits.effective();
        self.enforce_bounds();
        self
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn task_id(&self) -> Option<&str> {
        self.task_id.as_deref()
    }

    pub fn current_turn(&self) -> u64 {
        self.turn
    }

    pub fn turns_without_progress(&self) -> u32 {
        self.turns_without_progress
    }

    pub fn evidence_count(&self) -> u64 {
        self.evidence_count
    }

    pub fn last_progress_seq(&self) -> u64 {
        self.last_progress_seq
    }

    pub fn last_observed_seq(&self) -> u64 {
        self.last_observed_seq
    }

    pub fn last_progress_kind(&self) -> Option<ProgressEvidenceKind> {
        self.last_progress_kind
    }

    pub fn last_evidence_hash(&self) -> Option<&str> {
        self.last_evidence_hash.as_deref()
    }

    pub fn operator_resets(&self) -> u32 {
        self.operator_resets
    }

    /// Oldest-to-newest retained evidence.
    pub fn recent_evidence(&self) -> &[ProgressEvidence] {
        &self.recent_evidence
    }

    pub fn limits(&self) -> ProgressTrackerLimits {
        self.limits.clone()
    }

    /// Lowest sequence the runtime still needs to read. An operator resume
    /// moves this floor forward without claiming that progress happened.
    pub fn scan_floor_seq(&self) -> u64 {
        self.last_progress_seq.max(self.last_operator_reset_seq)
    }

    /// Classify one event without touching state.
    pub fn classify(event: &Event) -> EventClassification {
        classify_event(event)
    }

    /// Observe one event for `turn`.
    ///
    /// Turn ids are expected to be monotonic and cumulative across resumes
    /// (the daemon's persisted `total_turns` is suitable). A new turn id closes
    /// the previous open turn; a turn gap is counted as no-progress turns.
    /// Events with an already-observed sequence are ignored as stale.
    pub fn observe(&mut self, event: &Event, turn: u64) -> ObserveOutcome {
        let mut outcome = ObserveOutcome::default();
        if self.session_id.is_empty() {
            self.session_id = bounded_single_line(&event.session_id, MAX_ID_CHARS);
        }
        if event.session_id != self.session_id {
            outcome.session_mismatch = true;
            return outcome;
        }
        if event.seq > 0 && event.seq <= self.last_observed_seq {
            outcome.stale = true;
            return outcome;
        }
        let turn = turn.max(1);
        self.ensure_turn(turn);
        if event.seq > self.last_observed_seq {
            self.last_observed_seq = event.seq;
        }
        match classify_event(event) {
            EventClassification::NonProgress(reason) => {
                outcome.non_progress = Some(reason);
            }
            EventClassification::Progress(items) => {
                for item in items {
                    let evidence = ProgressEvidence::from_classified(
                        &item,
                        event.seq,
                        self.turn.max(turn),
                        &event.kind,
                    );
                    if self.seen_hash(&evidence.hash) {
                        outcome.duplicates.push(evidence);
                        continue;
                    }
                    self.remember_hash(&evidence.hash);
                    self.evidence_count = self.evidence_count.saturating_add(1);
                    self.turns_without_progress = 0;
                    self.last_progress_seq = event.seq;
                    self.last_progress_kind = Some(evidence.kind);
                    self.last_evidence_hash = Some(evidence.hash.clone());
                    self.turn_had_progress = true;
                    self.recent_evidence.push(evidence.clone());
                    self.enforce_bounds();
                    outcome.accepted.push(evidence);
                }
            }
        }
        outcome
    }

    /// Finalize one turn and return the current no-progress count.
    ///
    /// Call this exactly once per completed turn, including turns with no
    /// events. Repeated calls for the same turn are idempotent.
    pub fn finish_turn(&mut self, turn: u64) -> u32 {
        let turn = turn.max(1);
        if turn > self.turn {
            if self.turn == 0 {
                // The tracker started mid-stream (for example after a process
                // restart): adopt the first observed turn without inventing
                // history for turns it never saw.
                self.turn = turn;
                self.turn_open = false;
                self.turn_had_progress = false;
                self.turns_without_progress = self.turns_without_progress.saturating_add(1);
                return self.turns_without_progress;
            }
            let skipped = turn.saturating_sub(self.turn).saturating_sub(1);
            self.close_open_turn();
            self.turns_without_progress = self
                .turns_without_progress
                .saturating_add(u32::try_from(skipped).unwrap_or(u32::MAX));
            self.turn = turn;
            // No event was observed for this turn, so it is a no-progress turn.
            self.turns_without_progress = self.turns_without_progress.saturating_add(1);
            self.turn_open = false;
            self.turn_had_progress = false;
        } else if turn == self.turn {
            self.close_open_turn();
        }
        self.turns_without_progress
    }

    /// Pure threshold decision. The current (open) turn is not counted until
    /// [`Self::finish_turn`] closes it.
    pub fn decide(&self, policy: &NoProgressPolicy) -> NoProgressDecision {
        let policy = policy.effective();
        if self.turns_without_progress >= policy.no_progress_turns {
            policy.action.decision()
        } else {
            NoProgressDecision::Continue
        }
    }

    /// Explicit operator-only reset. Returns the previous counter value.
    ///
    /// This is not progress: it does not touch `last_progress_seq`,
    /// `last_progress_kind`, `last_evidence_hash`, or the evidence list. It
    /// excuses the currently open turn so the first post-resume read-only turn
    /// cannot immediately re-trigger the guard.
    pub fn reset_on_operator_resume(&mut self) -> u32 {
        let previous = self.turns_without_progress;
        self.turns_without_progress = 0;
        self.operator_resets = self.operator_resets.saturating_add(1);
        self.last_operator_reset_seq = self.last_observed_seq;
        if self.turn_open {
            self.turn_had_progress = true;
        }
        previous
    }

    /// Bounded summary with the newest evidence first.
    pub fn summary(&self, policy: &NoProgressPolicy) -> NoProgressSummary {
        let policy = policy.effective();
        let decision = self.decide(&policy);
        let limit = PAYLOAD_EVIDENCE_LIMIT;
        let recent_evidence = self
            .recent_evidence
            .iter()
            .rev()
            .take(limit)
            .map(ProgressEvidence::bounded)
            .collect::<Vec<_>>();
        let evidence_truncated = self.recent_evidence.len() > limit
            || self.evidence_count > self.recent_evidence.len() as u64;
        NoProgressSummary {
            session_id: bounded_single_line(&self.session_id, MAX_ID_CHARS),
            task_id: self
                .task_id
                .as_deref()
                .map(|value| bounded_single_line(value, MAX_ID_CHARS)),
            decision,
            action: policy.action,
            threshold: policy.no_progress_turns,
            reason: decision.reason_code().to_string(),
            turns_without_progress: self.turns_without_progress,
            evidence_count: self.evidence_count,
            last_progress_seq: self.last_progress_seq,
            last_progress_kind: self.last_progress_kind,
            last_evidence_hash: self
                .last_evidence_hash
                .as_deref()
                .map(|hash| bounded_single_line(hash, 64))
                .filter(|hash| !hash.is_empty()),
            last_observed_seq: self.last_observed_seq,
            operator_resets: self.operator_resets,
            recent_evidence,
            evidence_truncated,
        }
    }

    /// Convenience wrapper around [`NoProgressSummary::event_payload`].
    pub fn event_payload(&self, policy: &NoProgressPolicy) -> Value {
        self.summary(policy).event_payload()
    }

    /// Clamp every bound and re-check persisted state at a trust boundary.
    pub fn normalized(mut self) -> Self {
        self.session_id = bounded_single_line(&self.session_id, MAX_ID_CHARS);
        self.task_id = self
            .task_id
            .as_deref()
            .map(|value| bounded_single_line(value, MAX_ID_CHARS))
            .filter(|value| !value.is_empty());
        self.limits = self.limits.effective();
        for evidence in &mut self.recent_evidence {
            *evidence = evidence.bounded();
        }
        if self.turn == 0 {
            self.turn_open = false;
            self.turn_had_progress = false;
        }
        if self.recent_hashes.is_empty() {
            self.recent_hashes = self
                .recent_evidence
                .iter()
                .map(|evidence| normalize_hash(&evidence.hash))
                .filter(|hash| !hash.is_empty())
                .collect();
        } else {
            for hash in &mut self.recent_hashes {
                *hash = normalize_hash(hash);
            }
            self.recent_hashes.retain(|hash| !hash.is_empty());
        }
        if self.last_evidence_hash.is_none() {
            self.last_evidence_hash = self
                .recent_evidence
                .last()
                .map(|evidence| evidence.hash.clone());
        }
        self.last_evidence_hash = self
            .last_evidence_hash
            .as_deref()
            .map(normalize_hash)
            .filter(|hash| !hash.is_empty());
        self.enforce_bounds();
        self
    }

    fn ensure_turn(&mut self, turn: u64) {
        if self.turn == 0 {
            self.turn = turn;
            self.turn_open = true;
            self.turn_had_progress = false;
            return;
        }
        if turn > self.turn {
            let skipped = turn.saturating_sub(self.turn).saturating_sub(1);
            self.close_open_turn();
            self.turns_without_progress = self
                .turns_without_progress
                .saturating_add(u32::try_from(skipped).unwrap_or(u32::MAX));
            self.turn = turn;
            self.turn_open = true;
            self.turn_had_progress = false;
        } else if turn == self.turn && !self.turn_open {
            self.turn_open = true;
            self.turn_had_progress = false;
        }
    }

    fn close_open_turn(&mut self) {
        if self.turn_open {
            if !self.turn_had_progress {
                self.turns_without_progress = self.turns_without_progress.saturating_add(1);
            }
            self.turn_open = false;
            self.turn_had_progress = false;
        }
    }

    fn seen_hash(&self, hash: &str) -> bool {
        self.recent_hashes.iter().any(|seen| seen == hash)
    }

    fn remember_hash(&mut self, hash: &str) {
        let max = self.limits.effective().max_seen_hashes;
        while self.recent_hashes.len() >= max {
            self.recent_hashes.pop_front();
        }
        self.recent_hashes.push_back(hash.to_string());
    }

    fn enforce_bounds(&mut self) {
        self.limits = self.limits.effective();
        let max_evidence = self.limits.max_recent_evidence;
        if self.recent_evidence.len() > max_evidence {
            let excess = self.recent_evidence.len() - max_evidence;
            self.recent_evidence.drain(0..excess);
        }
        let max_hashes = self.limits.max_seen_hashes;
        while self.recent_hashes.len() > max_hashes {
            self.recent_hashes.pop_front();
        }
    }
}

fn evidence_hash(kind: ProgressEvidenceKind, success: bool, target: &str) -> String {
    sha256_hex(
        format!(
            "{}\u{0}{}\u{0}{}",
            kind.as_str(),
            if success { "1" } else { "0" },
            target
        )
        .as_bytes(),
    )
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Keep only the hex characters of a persisted hash. A tampered or imported
/// state cannot smuggle prose or a credential-like value through this field.
fn normalize_hash(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_hexdigit())
        .take(64)
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

/// Short, display-safe digest. Invalid or missing input becomes `unknown`, so
/// an imported payload cannot leak prose through a hash-labelled field.
fn short_hash(value: &str) -> String {
    let normalized = normalize_hash(value);
    if normalized.is_empty() {
        "unknown".to_string()
    } else {
        normalized.chars().take(12).collect()
    }
}

fn short_digest(value: &str, chars: usize) -> String {
    let digest = sha256_hex(value.as_bytes());
    digest.chars().take(chars).collect()
}

fn bounded_single_line(value: &str, max_chars: usize) -> String {
    let mut output = String::new();
    for (count, ch) in value.chars().enumerate() {
        if count >= max_chars {
            break;
        }
        if ch == '\n' || ch == '\r' || ch == '\t' {
            output.push(' ');
        } else if !ch.is_control() {
            output.push(ch);
        }
    }
    output.trim().to_string()
}

/// Token-safe text for identifiers, paths, commits, and short hashes. It
/// removes separators that make credential-like assignments easy to smuggle
/// into a target label.
fn safe_token(value: &str, max_chars: usize) -> String {
    let mut output = String::new();
    for (count, ch) in value.chars().enumerate() {
        if count >= max_chars {
            break;
        }
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':' | '@' | '+' | '>')
        {
            output.push(ch);
        } else if ch.is_whitespace() {
            output.push('_');
        }
    }
    let trimmed = output.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed
    }
}

fn path_label(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let parts = normalized
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect::<Vec<_>>();
    let start = parts.len().saturating_sub(4);
    if parts.is_empty() {
        return "path".to_string();
    }
    safe_token(&parts[start..].join("/"), MAX_TARGET_CHARS)
}

fn normalize_test_kind(kind: &str) -> &'static str {
    match kind {
        "test" => "test",
        "lint" => "lint",
        _ => "check",
    }
}

fn test_target(
    kind: &str,
    status: &str,
    command: Option<&str>,
    inputs_hash: Option<&str>,
    cache_key: Option<&str>,
    changed_paths: &[String],
) -> String {
    let kind = normalize_test_kind(kind);
    let command_digest = command
        .filter(|value| !value.is_empty())
        .map(|value| sha256_hex(value.as_bytes()))
        .unwrap_or_default();
    let basis = inputs_hash
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            cache_key
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| changed_paths.join(","));
    let short = short_digest(
        &format!("{kind}\u{0}{command_digest}\u{0}{basis}\u{0}{status}"),
        12,
    );
    format!("{kind}:{status}:{short}")
}

fn string_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn nested_string_field<'a>(value: &'a Value, parent: &str, key: &str) -> Option<&'a str> {
    value.get(parent).and_then(|inner| string_field(inner, key))
}

fn bool_field(value: &Value, key: &str) -> Option<bool> {
    value.get(key).and_then(Value::as_bool)
}

fn status_is_failed(status: &str) -> bool {
    matches!(
        status.to_ascii_lowercase().as_str(),
        "failed" | "error" | "conflict" | "rejected"
    )
}

/// Classify one event. The table is documented at the module level.
pub fn classify_event(event: &Event) -> EventClassification {
    let kind = event.kind.as_str();
    let payload = &event.payload;
    match kind {
        "session.committed" => classify_commit(payload, NonProgressReason::Unclassified),
        "session.landed" => classify_merge(payload),
        "merge_queue.merged" => classify_merge(payload),
        "merge_queue.updated" => {
            let Some(entry) = payload.get("entry") else {
                return EventClassification::NonProgress(NonProgressReason::QueueBookkeeping);
            };
            match string_field(entry, "status") {
                Some("merged") => {
                    let target = string_field(entry, "commit")
                        .or_else(|| string_field(entry, "id"))
                        .unwrap_or("merge");
                    EventClassification::Progress(vec![ClassifiedProgress::new(
                        ProgressEvidenceKind::Merge,
                        true,
                        safe_token(target, 80),
                    )])
                }
                Some(status) if status_is_failed(status) => {
                    let target = string_field(entry, "commit")
                        .or_else(|| string_field(entry, "id"))
                        .unwrap_or("integration");
                    EventClassification::Progress(vec![ClassifiedProgress::new(
                        ProgressEvidenceKind::IntegrationFailed,
                        false,
                        safe_token(target, 80),
                    )])
                }
                _ => EventClassification::NonProgress(NonProgressReason::QueueBookkeeping),
            }
        }
        "agent.finished" => classify_agent_finished(payload),
        "plan_task.finished" => classify_plan_task_finished(payload),
        "test_cache.stored" => classify_test_cache(payload),
        "artifact.published" | "artifact.version_published" => classify_artifact(payload),
        "session.checkpoint" => classify_checkpoint(payload),
        "agent.workspace.landed" | "workspace.land" | "workspace.landed" => {
            classify_workspace_landed(payload)
        }
        "effect.settled" => classify_effect_settled(payload),
        "tool.finished" => classify_tool_finished(payload),
        "tool.started" => EventClassification::NonProgress(NonProgressReason::ToolWithoutEffect),
        "agent.message" | "agent.message_delta" | "agent.thought_delta" => {
            EventClassification::NonProgress(NonProgressReason::ModelText)
        }
        "plan_task.started"
        | "plan_task.retried"
        | "plan_task.retry_scheduled"
        | "plan_task.paused"
        | "plan_task.pausing"
        | "plan_task.resumed"
        | "plan_task.cancelling"
        | "plan_task.skipped"
        | "plan_task.blocked"
        | "plan_task.unblocked" => {
            EventClassification::NonProgress(NonProgressReason::PlanBookkeeping)
        }
        "merge_queue.enqueued"
        | "merge_queue.auto_resolve_started"
        | "merge_queue.auto_resolve_finished"
        | "merge_queue.recovered"
        | "merge_queue.recovery_skipped" => {
            EventClassification::NonProgress(NonProgressReason::QueueBookkeeping)
        }
        "approval.created"
        | "approval.requested"
        | "approval.resolved"
        | "approval.expired"
        | "approval.coalesced"
        | "approval.template.created"
        | "approval.template.updated"
        | "approval.template.deleted"
        | "approval.template.matched"
        | "approval.policy.updated" => {
            EventClassification::NonProgress(NonProgressReason::ApprovalBookkeeping)
        }
        "agent.started"
        | "agent.queued"
        | "agent.spawned"
        | "agent.updated"
        | "agent.snapshot"
        | "agent.turn_started"
        | "agent.turn_finished"
        | "agent.deferred"
        | "agent.waiting_input"
        | "agent.paused"
        | "agent.resumed"
        | "agent.cancelled"
        | "agent.failed"
        | "agent.budget_warning"
        | "agent.budget_exceeded"
        | "agent.capacity_wait"
        | "agent.capacity_ready"
        | "agent.plan_resume_queued"
        | "agent.plan_resumed"
        | "agent.task_dispatch_queued"
        | "agent.task_dispatched"
        | "agent.stream_retry" => {
            EventClassification::NonProgress(NonProgressReason::AgentBookkeeping)
        }
        "agent.usage" | "agent.cost" | "session.usage" => {
            EventClassification::NonProgress(NonProgressReason::Telemetry)
        }
        _ if kind.starts_with("context.") => {
            EventClassification::NonProgress(NonProgressReason::Telemetry)
        }
        _ if kind.starts_with("budget.") || kind.starts_with("cache.") => {
            EventClassification::NonProgress(NonProgressReason::Telemetry)
        }
        _ if kind.starts_with("session.run.")
            || kind.starts_with("session.lease.")
            || kind.starts_with("session.continuation.") =>
        {
            EventClassification::NonProgress(NonProgressReason::RunBookkeeping)
        }
        _ if kind.starts_with("session.")
            || kind.starts_with("plan.")
            || kind.starts_with("team_plan.")
            || kind.starts_with("todo.")
            || kind.starts_with("question.") =>
        {
            EventClassification::NonProgress(NonProgressReason::SessionBookkeeping)
        }
        _ => EventClassification::NonProgress(NonProgressReason::Unclassified),
    }
}

fn classify_commit(payload: &Value, fallback: NonProgressReason) -> EventClassification {
    let Some(commit) = string_field(payload, "commit") else {
        return EventClassification::NonProgress(fallback);
    };
    EventClassification::Progress(vec![ClassifiedProgress::new(
        ProgressEvidenceKind::Commit,
        true,
        safe_token(commit, 80),
    )])
}

fn classify_merge(payload: &Value) -> EventClassification {
    let target = string_field(payload, "commit")
        .or_else(|| string_field(payload, "entry_id"))
        .or_else(|| string_field(payload, "branch"))
        .unwrap_or("merge");
    EventClassification::Progress(vec![ClassifiedProgress::new(
        ProgressEvidenceKind::Merge,
        true,
        safe_token(target, 80),
    )])
}

fn classify_agent_finished(payload: &Value) -> EventClassification {
    let role = string_field(payload, "agent_role").unwrap_or_default();
    let status = string_field(payload, "status").unwrap_or_default();
    let approved = bool_field(payload, "approved");
    if role == "verifier" {
        let target = string_field(payload, "task_id")
            .or_else(|| string_field(payload, "verified_worker"))
            .unwrap_or("task");
        return match approved {
            Some(true) => EventClassification::Progress(vec![ClassifiedProgress::new(
                ProgressEvidenceKind::VerifierApproved,
                true,
                safe_token(target, 80),
            )]),
            Some(false) => EventClassification::Progress(vec![ClassifiedProgress::new(
                ProgressEvidenceKind::VerifierRejected,
                false,
                safe_token(target, 80),
            )]),
            None => EventClassification::NonProgress(NonProgressReason::AgentBookkeeping),
        };
    }
    let commit = string_field(payload, "commit");
    let merged = bool_field(payload, "merged").unwrap_or(false);
    let integrator = bool_field(payload, "integrator").unwrap_or(false);
    if let Some(commit) = commit {
        let target = safe_token(commit, 80);
        if merged {
            return EventClassification::Progress(vec![ClassifiedProgress::new(
                ProgressEvidenceKind::Merge,
                true,
                target,
            )]);
        }
        if integrator {
            return EventClassification::Progress(vec![ClassifiedProgress::new(
                ProgressEvidenceKind::IntegrationFailed,
                false,
                target,
            )]);
        }
        let success = status == "idle" && approved != Some(false);
        return EventClassification::Progress(vec![ClassifiedProgress::new(
            ProgressEvidenceKind::Commit,
            success,
            target,
        )]);
    }
    if merged {
        let target = string_field(payload, "task_id").unwrap_or("merge");
        return EventClassification::Progress(vec![ClassifiedProgress::new(
            ProgressEvidenceKind::Merge,
            true,
            safe_token(target, 80),
        )]);
    }
    EventClassification::NonProgress(NonProgressReason::AgentBookkeeping)
}

fn classify_plan_task_finished(payload: &Value) -> EventClassification {
    let status = string_field(payload, "task_status").unwrap_or("unknown");
    let Some(task_id) = string_field(payload, "task_id") else {
        return EventClassification::NonProgress(NonProgressReason::PlanBookkeeping);
    };
    EventClassification::Progress(vec![ClassifiedProgress::new(
        ProgressEvidenceKind::PlanTaskFinished,
        status == "completed",
        format!("{}:{}", safe_token(task_id, 80), safe_token(status, 40)),
    )])
}

fn classify_test_cache(payload: &Value) -> EventClassification {
    let Some(entry) = payload.get("entry") else {
        return EventClassification::NonProgress(NonProgressReason::Unclassified);
    };
    let status = string_field(entry, "status").unwrap_or("unknown");
    let kind = string_field(entry, "kind").unwrap_or("check");
    let changed_paths = entry
        .get("covered_paths")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let target = test_target(
        kind,
        status,
        string_field(entry, "command"),
        string_field(entry, "inputs_hash"),
        string_field(entry, "cache_key"),
        &changed_paths,
    );
    let evidence_kind = if status == "passed" {
        ProgressEvidenceKind::TestPassed
    } else {
        ProgressEvidenceKind::TestFailed
    };
    EventClassification::Progress(vec![ClassifiedProgress::new(
        evidence_kind,
        status == "passed",
        target,
    )])
}

fn classify_artifact(payload: &Value) -> EventClassification {
    let Some(artifact_id) = string_field(payload, "artifact_id") else {
        return EventClassification::NonProgress(NonProgressReason::Unclassified);
    };
    let version = payload.get("version").and_then(Value::as_u64).unwrap_or(1);
    let sha = string_field(payload, "sha256")
        .map(short_hash)
        .unwrap_or_else(|| "unknown".to_string());
    EventClassification::Progress(vec![ClassifiedProgress::new(
        ProgressEvidenceKind::ArtifactPublished,
        true,
        format!(
            "{}:v{}:{}",
            safe_token(artifact_id, 50),
            version,
            safe_token(&sha, 12)
        ),
    )])
}

fn classify_checkpoint(payload: &Value) -> EventClassification {
    let target = string_field(payload, "checkpoint_id")
        .or_else(|| string_field(payload, "commit"))
        .unwrap_or("checkpoint");
    EventClassification::Progress(vec![ClassifiedProgress::new(
        ProgressEvidenceKind::SessionCheckpoint,
        true,
        safe_token(target, 80),
    )])
}

fn classify_workspace_landed(payload: &Value) -> EventClassification {
    let landed_commit = string_field(payload, "landed_commit")
        .or_else(|| nested_string_field(payload, "land", "landed_commit"))
        .or_else(|| string_field(payload, "commit"));
    let workspace_id = payload
        .get("workspace")
        .and_then(|workspace| string_field(workspace, "id"))
        .or_else(|| string_field(payload, "workspace_id"));
    let target = match (workspace_id, landed_commit) {
        (Some(workspace), Some(commit)) => {
            format!("{}:{}", safe_token(workspace, 50), safe_token(commit, 50))
        }
        (None, Some(commit)) => safe_token(commit, 80),
        (Some(workspace), None) => safe_token(workspace, 80),
        (None, None) => "workspace".to_string(),
    };
    EventClassification::Progress(vec![ClassifiedProgress::new(
        ProgressEvidenceKind::WorkspaceLanded,
        true,
        target,
    )])
}

fn classify_effect_settled(payload: &Value) -> EventClassification {
    let Some(receipt) = payload.get("receipt") else {
        return EventClassification::NonProgress(NonProgressReason::Unclassified);
    };
    if string_field(receipt, "state") != Some("done") {
        return EventClassification::NonProgress(NonProgressReason::Unclassified);
    }
    let target_kind = receipt
        .get("target")
        .and_then(|target| string_field(target, "kind"))
        .unwrap_or_default();
    let target_key = receipt
        .get("target")
        .and_then(|target| string_field(target, "key"))
        .unwrap_or_default();
    match target_kind {
        "git_commit" => {
            let commit = string_field(receipt, "external_id")
                .or_else(|| {
                    receipt
                        .get("evidence")
                        .and_then(|e| string_field(e, "commit"))
                })
                .or_else(|| (!target_key.is_empty()).then_some(target_key));
            match commit {
                Some(commit) => EventClassification::Progress(vec![ClassifiedProgress::new(
                    ProgressEvidenceKind::Commit,
                    true,
                    safe_token(commit, 80),
                )]),
                None => EventClassification::NonProgress(NonProgressReason::Unclassified),
            }
        }
        "git_merge" => {
            let commit = string_field(receipt, "external_id")
                .or_else(|| {
                    receipt
                        .get("evidence")
                        .and_then(|e| string_field(e, "commit"))
                })
                .or_else(|| (!target_key.is_empty()).then_some(target_key));
            match commit {
                Some(commit) => EventClassification::Progress(vec![ClassifiedProgress::new(
                    ProgressEvidenceKind::Merge,
                    true,
                    safe_token(commit, 80),
                )]),
                None => EventClassification::NonProgress(NonProgressReason::Unclassified),
            }
        }
        "workspace_write" | "workspace_patch" => {
            let mut items = Vec::new();
            let mut had_path_evidence = false;
            if let Some(paths) = receipt
                .get("evidence")
                .and_then(|e| e.get("paths"))
                .and_then(Value::as_array)
            {
                had_path_evidence = !paths.is_empty();
                for path in paths.iter().take(8) {
                    let status = string_field(path, "status").unwrap_or("unknown");
                    if status == "unchanged" || status == "missing" {
                        continue;
                    }
                    let Some(path_name) = string_field(path, "path") else {
                        continue;
                    };
                    let sha = string_field(path, "sha256")
                        .map(short_hash)
                        .unwrap_or_else(|| safe_token(status, 20));
                    items.push(ClassifiedProgress::new(
                        ProgressEvidenceKind::FileChanged,
                        true,
                        format!("{}@{}", path_label(path_name), sha),
                    ));
                }
            }
            if items.is_empty()
                && !had_path_evidence
                && target_kind == "workspace_write"
                && !target_key.is_empty()
            {
                let sha = string_field(receipt, "response_hash")
                    .map(short_hash)
                    .unwrap_or_else(|| "done".to_string());
                items.push(ClassifiedProgress::new(
                    ProgressEvidenceKind::FileChanged,
                    true,
                    format!("{}@{}", path_label(target_key), sha),
                ));
            }
            if items.is_empty() {
                let reason = if had_path_evidence {
                    NonProgressReason::NoOp
                } else {
                    NonProgressReason::Unclassified
                };
                EventClassification::NonProgress(reason)
            } else {
                EventClassification::Progress(items)
            }
        }
        "workspace_manage" => {
            if target_key.is_empty() {
                return EventClassification::NonProgress(NonProgressReason::Unclassified);
            }
            EventClassification::Progress(vec![ClassifiedProgress::new(
                ProgressEvidenceKind::FileChanged,
                true,
                path_label(target_key),
            )])
        }
        _ => EventClassification::NonProgress(NonProgressReason::Unclassified),
    }
}

fn classify_tool_finished(payload: &Value) -> EventClassification {
    let name = string_field(payload, "name").unwrap_or_default();
    let is_error = bool_field(payload, "is_error").unwrap_or(false);
    let structured = payload.get("structured");
    match name {
        "write_file" => classify_write_tool(structured, is_error),
        "edit_file" => classify_edit_tool(structured, is_error),
        "apply_patch" => classify_patch_tool(structured, is_error),
        "file_manage" => classify_manage_tool(structured, is_error),
        "run_project_task" => classify_project_task(structured),
        _ if is_read_only_tool(name) => {
            EventClassification::NonProgress(NonProgressReason::ReadOnlyTool)
        }
        _ => EventClassification::NonProgress(NonProgressReason::ToolWithoutEffect),
    }
}

fn is_read_only_tool(name: &str) -> bool {
    matches!(
        name,
        "read_file"
            | "read_files"
            | "list_files"
            | "find_files"
            | "grep"
            | "search"
            | "file_info"
            | "repo_map"
            | "find_symbol"
            | "find_references"
            | "dependency_graph"
            | "affected_tests"
            | "code_diagnostics"
            | "build_context"
            | "git"
            | "fetch_url"
            | "local_http"
            | "read_tool_output"
            | "tool_help"
            | "tool_search"
            | "recall"
            | "list_artifacts"
            | "read_artifact"
            | "read_mailbox"
            | "team_status"
            | "progress"
    )
}

fn classify_write_tool(structured: Option<&Value>, is_error: bool) -> EventClassification {
    if is_error {
        return EventClassification::NonProgress(NonProgressReason::FailedTool);
    }
    let Some(structured) = structured else {
        return EventClassification::NonProgress(NonProgressReason::FailedTool);
    };
    if bool_field(structured, "dry_run").unwrap_or(false) {
        return EventClassification::NonProgress(NonProgressReason::DryRun);
    }
    let Some(path) = string_field(structured, "path") else {
        return EventClassification::NonProgress(NonProgressReason::FailedTool);
    };
    let after = string_field(structured, "sha256");
    let before = string_field(structured, "before_sha256");
    let created = bool_field(structured, "created").unwrap_or(false);
    if let (Some(after), Some(before)) = (after, before)
        && after == before
        && !created
    {
        return EventClassification::NonProgress(NonProgressReason::NoOp);
    }
    file_changed_classification(path, after, "written")
}

fn classify_edit_tool(structured: Option<&Value>, is_error: bool) -> EventClassification {
    if is_error {
        return EventClassification::NonProgress(NonProgressReason::FailedTool);
    }
    let Some(structured) = structured else {
        return EventClassification::NonProgress(NonProgressReason::FailedTool);
    };
    if bool_field(structured, "dry_run").unwrap_or(false) {
        return EventClassification::NonProgress(NonProgressReason::DryRun);
    }
    let Some(path) = string_field(structured, "path") else {
        return EventClassification::NonProgress(NonProgressReason::FailedTool);
    };
    let before = string_field(structured, "before_sha256");
    let after = string_field(structured, "after_sha256");
    if let (Some(before), Some(after)) = (before, after)
        && before == after
    {
        return EventClassification::NonProgress(NonProgressReason::NoOp);
    }
    file_changed_classification(path, after, "edited")
}

fn classify_patch_tool(structured: Option<&Value>, is_error: bool) -> EventClassification {
    if is_error {
        return EventClassification::NonProgress(NonProgressReason::FailedTool);
    }
    let Some(structured) = structured else {
        return EventClassification::NonProgress(NonProgressReason::FailedTool);
    };
    if bool_field(structured, "dry_run").unwrap_or(false) {
        return EventClassification::NonProgress(NonProgressReason::DryRun);
    }
    let Some(files) = structured.get("files").and_then(Value::as_array) else {
        return EventClassification::NonProgress(NonProgressReason::FailedTool);
    };
    let mut items = Vec::new();
    for file in files.iter().take(8) {
        let status = string_field(file, "status").unwrap_or("unknown");
        if status == "unchanged" {
            continue;
        }
        let Some(path) = string_field(file, "path") else {
            continue;
        };
        let sha = string_field(file, "after_sha256")
            .map(short_hash)
            .or_else(|| (status == "deleted").then(|| "deleted".to_string()));
        items.push(ClassifiedProgress::new(
            ProgressEvidenceKind::FileChanged,
            true,
            format!(
                "{}@{}",
                path_label(path),
                sha.unwrap_or_else(|| status.to_string())
            ),
        ));
    }
    if items.is_empty() {
        EventClassification::NonProgress(NonProgressReason::NoOp)
    } else {
        EventClassification::Progress(items)
    }
}

fn classify_manage_tool(structured: Option<&Value>, is_error: bool) -> EventClassification {
    if is_error {
        return EventClassification::NonProgress(NonProgressReason::FailedTool);
    }
    let Some(structured) = structured else {
        return EventClassification::NonProgress(NonProgressReason::FailedTool);
    };
    if bool_field(structured, "dry_run").unwrap_or(false) {
        return EventClassification::NonProgress(NonProgressReason::DryRun);
    }
    let action = string_field(structured, "action").unwrap_or("manage");
    if action == "mkdir" && !bool_field(structured, "created").unwrap_or(false) {
        return EventClassification::NonProgress(NonProgressReason::NoOp);
    }
    let source = string_field(structured, "source").or_else(|| string_field(structured, "path"));
    let destination = string_field(structured, "destination");
    let target = match (source, destination) {
        (Some(source), Some(destination)) => format!(
            "{}:{}->{}",
            safe_token(action, 20),
            path_label(source),
            path_label(destination)
        ),
        (Some(source), None) => {
            format!("{}:{}", safe_token(action, 20), path_label(source))
        }
        (None, Some(destination)) => {
            format!("{}:{}", safe_token(action, 20), path_label(destination))
        }
        (None, None) => return EventClassification::NonProgress(NonProgressReason::FailedTool),
    };
    EventClassification::Progress(vec![ClassifiedProgress::new(
        ProgressEvidenceKind::FileChanged,
        true,
        target,
    )])
}

fn classify_project_task(structured: Option<&Value>) -> EventClassification {
    let Some(structured) = structured else {
        return EventClassification::NonProgress(NonProgressReason::Unclassified);
    };
    let Some(ok) = bool_field(structured, "ok") else {
        return EventClassification::NonProgress(NonProgressReason::Unclassified);
    };
    let Some(kind) = string_field(structured, "kind") else {
        return EventClassification::NonProgress(NonProgressReason::Unclassified);
    };
    let status =
        string_field(structured, "cache_status").unwrap_or(if ok { "passed" } else { "failed" });
    let changed_paths = structured
        .get("changed_paths")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let cache_key = structured
        .get("cache")
        .and_then(|cache| string_field(cache, "cache_key"));
    let target = test_target(
        kind,
        status,
        string_field(structured, "command"),
        string_field(structured, "inputs_hash"),
        cache_key,
        &changed_paths,
    );
    let evidence_kind = if ok {
        ProgressEvidenceKind::TestPassed
    } else {
        ProgressEvidenceKind::TestFailed
    };
    EventClassification::Progress(vec![ClassifiedProgress::new(evidence_kind, ok, target)])
}

fn file_changed_classification(
    path: &str,
    sha: Option<&str>,
    fallback: &str,
) -> EventClassification {
    let sha = sha
        .map(short_hash)
        .unwrap_or_else(|| safe_token(fallback, 20));
    EventClassification::Progress(vec![ClassifiedProgress::new(
        ProgressEvidenceKind::FileChanged,
        true,
        format!("{}@{}", path_label(path), sha),
    )])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(seq: u64, kind: &str, payload: Value) -> Event {
        Event {
            session_id: "s1".to_string(),
            seq,
            kind: kind.to_string(),
            payload,
            created_at: "2026-11-21T00:00:00Z".to_string(),
        }
    }

    fn tracker() -> ProgressTracker {
        ProgressTracker::new("s1", None)
    }

    fn commit_event(seq: u64, commit: &str) -> Event {
        event(
            seq,
            "session.committed",
            json!({"commit": commit, "message": "progress"}),
        )
    }

    fn read_event(seq: u64) -> Event {
        event(
            seq,
            "tool.finished",
            json!({
                "name": "read_file",
                "is_error": false,
                "structured": {"path": "src/lib.rs"},
            }),
        )
    }

    fn write_event(seq: u64, path: &str, sha: &str, before: Option<&str>) -> Event {
        event(
            seq,
            "tool.finished",
            json!({
                "name": "write_file",
                "is_error": false,
                "structured": {
                    "path": path,
                    "sha256": sha,
                    "before_sha256": before,
                    "created": before.is_none(),
                    "dry_run": false,
                },
            }),
        )
    }

    fn test_event(seq: u64, status: &str) -> Event {
        event(
            seq,
            "test_cache.stored",
            json!({
                "entry": {
                    "kind": "test",
                    "command": "cargo test",
                    "cache_key": "cache-key",
                    "inputs_hash": "inputs-hash",
                    "status": status,
                },
            }),
        )
    }

    fn drain_turn(tracker: &mut ProgressTracker, turn: u64) -> u32 {
        tracker.finish_turn(turn)
    }

    #[test]
    fn read_only_loop_increments_until_pause() {
        let mut tracker = tracker();
        let policy = NoProgressPolicy::new(3, NoProgressAction::Pause);
        for turn in 1..=3 {
            let outcome = tracker.observe(&read_event(turn), turn);
            assert!(outcome.accepted.is_empty());
            assert_eq!(outcome.non_progress, Some(NonProgressReason::ReadOnlyTool));
            drain_turn(&mut tracker, turn);
        }
        assert_eq!(tracker.turns_without_progress(), 3);
        assert_eq!(tracker.evidence_count(), 0);
        assert_eq!(tracker.decide(&policy), NoProgressDecision::Pause);
    }

    #[test]
    fn commit_resets_turns_without_progress() {
        let mut tracker = tracker();
        let policy = NoProgressPolicy::new(3, NoProgressAction::Pause);
        for turn in 1..=2 {
            tracker.observe(&read_event(turn), turn);
            drain_turn(&mut tracker, turn);
        }
        assert_eq!(tracker.turns_without_progress(), 2);
        let outcome = tracker.observe(&commit_event(10, "abcdef1234567890"), 3);
        assert!(outcome.is_progress());
        assert_eq!(tracker.turns_without_progress(), 0);
        assert_eq!(
            tracker.last_progress_kind(),
            Some(ProgressEvidenceKind::Commit)
        );
        drain_turn(&mut tracker, 3);
        assert_eq!(tracker.turns_without_progress(), 0);
        assert_eq!(tracker.decide(&policy), NoProgressDecision::Continue);
        assert_eq!(tracker.evidence_count(), 1);
    }

    #[test]
    fn plan_completion_resets_turns_without_progress() {
        let mut tracker = tracker();
        for turn in 1..=2 {
            tracker.observe(&read_event(turn), turn);
            drain_turn(&mut tracker, turn);
        }
        let outcome = tracker.observe(
            &event(
                20,
                "plan_task.finished",
                json!({"task_id": "T1", "task_status": "completed"}),
            ),
            3,
        );
        assert!(outcome.is_progress());
        assert_eq!(tracker.turns_without_progress(), 0);
        assert_eq!(
            tracker.last_progress_kind(),
            Some(ProgressEvidenceKind::PlanTaskFinished)
        );
        drain_turn(&mut tracker, 3);
        assert_eq!(tracker.turns_without_progress(), 0);
    }

    #[test]
    fn duplicate_commit_does_not_reset_the_counter() {
        let mut tracker = tracker();
        tracker.observe(&commit_event(1, "deadbeefcafebabe"), 1);
        drain_turn(&mut tracker, 1);
        for turn in 2..=3 {
            tracker.observe(&read_event(turn), turn);
            drain_turn(&mut tracker, turn);
        }
        assert_eq!(tracker.turns_without_progress(), 2);
        let outcome = tracker.observe(&commit_event(4, "deadbeefcafebabe"), 4);
        assert!(outcome.accepted.is_empty());
        assert_eq!(outcome.duplicates.len(), 1);
        assert_eq!(tracker.turns_without_progress(), 2);
        assert_eq!(tracker.evidence_count(), 1);
        drain_turn(&mut tracker, 4);
        assert_eq!(tracker.turns_without_progress(), 3);
    }

    #[test]
    fn duplicate_test_and_file_evidence_do_not_reset() {
        let mut tracker = tracker();
        tracker.observe(&test_event(1, "passed"), 1);
        drain_turn(&mut tracker, 1);
        let duplicate = tracker.observe(&test_event(2, "passed"), 2);
        assert_eq!(duplicate.duplicates.len(), 1);
        assert_eq!(tracker.turns_without_progress(), 0);
        drain_turn(&mut tracker, 2);
        assert_eq!(tracker.turns_without_progress(), 1);

        tracker.observe(
            &write_event(3, "src/lib.rs", "aaaa1111", Some("bbbb2222")),
            3,
        );
        drain_turn(&mut tracker, 3);
        let repeated = tracker.observe(
            &write_event(4, "src/lib.rs", "aaaa1111", Some("bbbb2222")),
            4,
        );
        assert_eq!(repeated.duplicates.len(), 1);
        assert_eq!(tracker.turns_without_progress(), 0);
        drain_turn(&mut tracker, 4);
        assert_eq!(tracker.turns_without_progress(), 1);
        assert_eq!(tracker.evidence_count(), 2);
    }

    #[test]
    fn test_cache_event_and_project_task_result_dedupe() {
        let mut tracker = tracker();
        let stored = tracker.observe(&test_event(1, "passed"), 1);
        assert_eq!(stored.accepted.len(), 1);
        let tool = tracker.observe(
            &event(
                2,
                "tool.finished",
                json!({
                    "name": "run_project_task",
                    "is_error": false,
                    "structured": {
                        "ok": true,
                        "kind": "test",
                        "command": "cargo test",
                        "inputs_hash": "inputs-hash",
                        "changed_paths": [],
                    },
                }),
            ),
            1,
        );
        assert!(tool.accepted.is_empty());
        assert_eq!(tool.duplicates.len(), 1);
        assert_eq!(tracker.evidence_count(), 1);
    }

    #[test]
    fn failed_test_is_new_evidence_but_not_success() {
        let mut tracker = tracker();
        for turn in 1..=2 {
            tracker.observe(&read_event(turn), turn);
            drain_turn(&mut tracker, turn);
        }
        assert_eq!(tracker.turns_without_progress(), 2);
        let failed = tracker.observe(&test_event(3, "failed"), 3);
        assert_eq!(failed.accepted.len(), 1);
        assert_eq!(failed.accepted[0].kind, ProgressEvidenceKind::TestFailed);
        assert!(!failed.accepted[0].success);
        assert_eq!(tracker.turns_without_progress(), 0);
        drain_turn(&mut tracker, 3);
        assert_eq!(tracker.turns_without_progress(), 0);

        let passed = tracker.observe(&test_event(4, "passed"), 4);
        assert_eq!(passed.accepted.len(), 1);
        assert_eq!(passed.accepted[0].kind, ProgressEvidenceKind::TestPassed);
        assert!(passed.accepted[0].success);
        assert_eq!(tracker.evidence_count(), 2);
    }

    #[test]
    fn failed_verifier_and_integration_are_failed_evidence() {
        let mut tracker = tracker();
        let verifier = tracker.observe(
            &event(
                1,
                "agent.finished",
                json!({
                    "agent_role": "verifier",
                    "status": "idle",
                    "approved": false,
                    "task_id": "T1",
                }),
            ),
            1,
        );
        assert_eq!(
            verifier.accepted[0].kind,
            ProgressEvidenceKind::VerifierRejected
        );
        assert!(!verifier.accepted[0].success);

        let integration = tracker.observe(
            &event(
                2,
                "agent.finished",
                json!({
                    "agent_role": "worker",
                    "status": "failed",
                    "commit": "abc123",
                    "integrator": true,
                    "merged": false,
                }),
            ),
            2,
        );
        assert_eq!(
            integration.accepted[0].kind,
            ProgressEvidenceKind::IntegrationFailed
        );
        assert!(!integration.accepted[0].success);
        assert_eq!(tracker.evidence_count(), 2);
    }

    #[test]
    fn threshold_decision_honors_action() {
        let mut tracker = tracker();
        let pause = NoProgressPolicy::new(2, NoProgressAction::Pause);
        let escalate = NoProgressPolicy::new(2, NoProgressAction::Escalate);
        drain_turn(&mut tracker, 1);
        assert_eq!(tracker.decide(&pause), NoProgressDecision::Continue);
        drain_turn(&mut tracker, 2);
        assert_eq!(tracker.decide(&pause), NoProgressDecision::Pause);
        assert_eq!(tracker.decide(&escalate), NoProgressDecision::Escalate);
        // Zero is normalized to one rather than disabling the guard.
        assert_eq!(
            tracker.decide(&NoProgressPolicy::new(0, NoProgressAction::Pause)),
            NoProgressDecision::Pause
        );
    }

    #[test]
    fn operator_resume_is_the_only_explicit_reset() {
        let mut tracker = tracker();
        let policy = NoProgressPolicy::new(2, NoProgressAction::Pause);
        for turn in 1..=3 {
            tracker.observe(&read_event(turn), turn);
            drain_turn(&mut tracker, turn);
        }
        assert_eq!(tracker.decide(&policy), NoProgressDecision::Pause);
        let previous = tracker.reset_on_operator_resume();
        assert_eq!(previous, 3);
        assert_eq!(tracker.turns_without_progress(), 0);
        assert_eq!(tracker.operator_resets(), 1);
        assert_eq!(tracker.decide(&policy), NoProgressDecision::Continue);
        // The current turn is excused; the next completed turn counts again.
        drain_turn(&mut tracker, 3);
        assert_eq!(tracker.turns_without_progress(), 0);
        tracker.observe(&read_event(4), 4);
        drain_turn(&mut tracker, 4);
        assert_eq!(tracker.turns_without_progress(), 1);
        assert_eq!(tracker.decide(&policy), NoProgressDecision::Continue);
    }

    #[test]
    fn operator_resume_ignores_replayed_old_events() {
        let mut tracker = tracker();
        tracker.observe(&read_event(3), 1);
        drain_turn(&mut tracker, 1);
        tracker.reset_on_operator_resume();
        let replay = tracker.observe(&read_event(3), 2);
        assert!(replay.stale);
        assert!(replay.accepted.is_empty());
        assert_eq!(tracker.turns_without_progress(), 0);
    }

    #[test]
    fn bounded_state_truncates_evidence_and_hashes() {
        let limits = ProgressTrackerLimits {
            max_recent_evidence: 2,
            max_seen_hashes: 2,
        };
        let mut tracker = tracker().with_limits(limits);
        for index in 1..=5u64 {
            let commit = format!("commit{index:04}");
            let outcome = tracker.observe(&commit_event(index, &commit), index);
            assert_eq!(outcome.accepted.len(), 1);
        }
        assert_eq!(tracker.evidence_count(), 5);
        assert_eq!(tracker.recent_evidence().len(), 2);
        assert_eq!(tracker.limits().max_recent_evidence, 2);
        assert_eq!(tracker.limits().max_seen_hashes, 2);
        let summary = tracker.summary(&NoProgressPolicy::default());
        assert_eq!(summary.recent_evidence.len(), 2);
        assert!(summary.evidence_truncated);
        // The retained evidence is newest-first.
        assert_eq!(summary.recent_evidence[0].target, "commit0005");
    }

    #[test]
    fn normalized_clamps_persisted_state() {
        let mut tracker = tracker();
        tracker.session_id = "x".repeat(MAX_ID_CHARS + 50);
        tracker.recent_evidence = (0..40)
            .map(|index| ProgressEvidence {
                kind: ProgressEvidenceKind::Commit,
                success: true,
                seq: index,
                turn: 1,
                target: format!("{}secret-ish", "y".repeat(MAX_TARGET_CHARS + 50)),
                event_kind: "session.committed\nnested".to_string(),
                hash: "z".repeat(100),
            })
            .collect();
        tracker.recent_hashes = (0..900).map(|index| format!("{index}")).collect();
        tracker.last_evidence_hash = Some("a".repeat(200));
        let normalized = tracker.normalized();
        assert!(normalized.session_id().chars().count() <= MAX_ID_CHARS);
        assert!(normalized.recent_evidence().len() <= MAX_RECENT_EVIDENCE_CEILING);
        assert!(
            normalized
                .recent_evidence()
                .iter()
                .all(|e| e.target.chars().count() <= MAX_TARGET_CHARS)
        );
        assert!(
            normalized
                .recent_evidence()
                .iter()
                .all(|e| !e.event_kind.contains('\n'))
        );
        assert!(normalized.last_evidence_hash().unwrap().chars().count() <= 64);
        assert!(normalized.limits().max_seen_hashes <= MAX_SEEN_HASHES_CEILING);
    }

    #[test]
    fn serde_round_trip_preserves_state_and_dedupe() {
        let mut tracker = tracker().with_limits(ProgressTrackerLimits {
            max_recent_evidence: 4,
            max_seen_hashes: 8,
        });
        tracker.observe(&commit_event(1, "abcdef"), 1);
        tracker.observe(&test_event(2, "failed"), 2);
        tracker.finish_turn(1);
        tracker.finish_turn(2);
        let encoded = serde_json::to_string(&tracker).expect("serialize");
        let mut decoded: ProgressTracker = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, tracker);
        let duplicate = decoded.observe(&commit_event(3, "abcdef"), 3);
        assert_eq!(duplicate.duplicates.len(), 1);
        assert_eq!(decoded.evidence_count(), 2);
    }

    #[test]
    fn classifies_progress_event_predicates() {
        let cases = [
            (
                event(1, "session.committed", json!({"commit": "abc123"})),
                ProgressEvidenceKind::Commit,
                true,
            ),
            (
                event(2, "session.landed", json!({"commit": "abc123"})),
                ProgressEvidenceKind::Merge,
                true,
            ),
            (
                event(
                    3,
                    "merge_queue.updated",
                    json!({"entry": {"status": "merged", "commit": "abc123"}}),
                ),
                ProgressEvidenceKind::Merge,
                true,
            ),
            (
                event(
                    4,
                    "agent.finished",
                    json!({"agent_role": "worker", "status": "idle", "commit": "abc123", "merged": true}),
                ),
                ProgressEvidenceKind::Merge,
                true,
            ),
            (
                event(
                    5,
                    "agent.finished",
                    json!({"agent_role": "worker", "status": "changes_requested", "commit": "abc123", "approved": false}),
                ),
                ProgressEvidenceKind::Commit,
                false,
            ),
            (
                event(
                    6,
                    "agent.finished",
                    json!({"agent_role": "verifier", "status": "idle", "approved": true, "task_id": "T1"}),
                ),
                ProgressEvidenceKind::VerifierApproved,
                true,
            ),
            (
                event(
                    7,
                    "test_cache.stored",
                    json!({"entry": {"kind": "test", "command": "cargo test", "inputs_hash": "i", "status": "passed"}}),
                ),
                ProgressEvidenceKind::TestPassed,
                true,
            ),
            (
                event(
                    8,
                    "test_cache.stored",
                    json!({"entry": {"kind": "test", "command": "cargo test", "inputs_hash": "i", "status": "failed"}}),
                ),
                ProgressEvidenceKind::TestFailed,
                false,
            ),
            (
                event(
                    9,
                    "plan_task.finished",
                    json!({"task_id": "T1", "task_status": "completed"}),
                ),
                ProgressEvidenceKind::PlanTaskFinished,
                true,
            ),
            (
                event(
                    10,
                    "artifact.published",
                    json!({"artifact_id": "a1", "version": 2, "sha256": "deadbeef"}),
                ),
                ProgressEvidenceKind::ArtifactPublished,
                true,
            ),
            (
                event(
                    11,
                    "session.checkpoint",
                    json!({"checkpoint_id": "c1", "commit": "abc123"}),
                ),
                ProgressEvidenceKind::SessionCheckpoint,
                true,
            ),
            (
                event(
                    12,
                    "agent.workspace.landed",
                    json!({"workspace": {"id": "w1"}, "land": {"landed_commit": "abc123"}}),
                ),
                ProgressEvidenceKind::WorkspaceLanded,
                true,
            ),
            (
                event(
                    13,
                    "effect.settled",
                    json!({"receipt": {"state": "done", "target": {"kind": "git_commit", "key": "main"}, "external_id": "abc123"}}),
                ),
                ProgressEvidenceKind::Commit,
                true,
            ),
            (
                event(
                    14,
                    "effect.settled",
                    json!({"receipt": {"state": "done", "target": {"kind": "workspace_write", "key": "src/lib.rs"}, "evidence": {"paths": [{"path": "src/lib.rs", "status": "modified", "sha256": "deadbeef"}]}}}),
                ),
                ProgressEvidenceKind::FileChanged,
                true,
            ),
            (
                write_event(15, "src/lib.rs", "aaaa", Some("bbbb")),
                ProgressEvidenceKind::FileChanged,
                true,
            ),
            (
                event(
                    16,
                    "tool.finished",
                    json!({"name": "run_project_task", "is_error": true, "structured": {"ok": false, "kind": "test", "command": "cargo test", "inputs_hash": "i", "changed_paths": []}}),
                ),
                ProgressEvidenceKind::TestFailed,
                false,
            ),
        ];
        for (event, kind, success) in cases {
            match classify_event(&event) {
                EventClassification::Progress(items) => {
                    assert_eq!(items.len(), 1, "event {}", event.kind);
                    assert_eq!(items[0].kind, kind, "event {}", event.kind);
                    assert_eq!(items[0].success, success, "event {}", event.kind);
                }
                other => panic!("expected progress for {}: {other:?}", event.kind),
            }
        }
    }

    #[test]
    fn classifies_non_progress_event_predicates() {
        let cases = [
            (
                event(1, "agent.message", json!({"text": "I made progress"})),
                NonProgressReason::ModelText,
            ),
            (
                event(2, "agent.message_delta", json!({"delta": "still working"})),
                NonProgressReason::ModelText,
            ),
            (read_event(3), NonProgressReason::ReadOnlyTool),
            (
                event(4, "plan_task.started", json!({"task_id": "T1"})),
                NonProgressReason::PlanBookkeeping,
            ),
            (
                event(5, "borderline", json!({"text": "unknown"})),
                NonProgressReason::Unclassified,
            ),
            (
                event(6, "context.usage", json!({"tokens": 100})),
                NonProgressReason::Telemetry,
            ),
            (
                event(7, "session.run.step_intent", json!({"step": 1})),
                NonProgressReason::RunBookkeeping,
            ),
            (
                event(
                    8,
                    "merge_queue.updated",
                    json!({"entry": {"status": "queued"}}),
                ),
                NonProgressReason::QueueBookkeeping,
            ),
        ];
        for (event, reason) in cases {
            assert_eq!(
                classify_event(&event),
                EventClassification::NonProgress(reason),
                "event {}",
                event.kind
            );
        }
    }

    #[test]
    fn dry_run_and_unchanged_writes_are_not_progress() {
        let dry = event(
            1,
            "tool.finished",
            json!({
                "name": "write_file",
                "is_error": false,
                "structured": {"path": "src/lib.rs", "sha256": "aaaa", "before_sha256": "bbbb", "dry_run": true},
            }),
        );
        assert_eq!(
            classify_event(&dry),
            EventClassification::NonProgress(NonProgressReason::DryRun)
        );
        let unchanged = event(
            2,
            "tool.finished",
            json!({
                "name": "write_file",
                "is_error": false,
                "structured": {"path": "src/lib.rs", "sha256": "aaaa", "before_sha256": "aaaa", "dry_run": false, "created": false},
            }),
        );
        assert_eq!(
            classify_event(&unchanged),
            EventClassification::NonProgress(NonProgressReason::NoOp)
        );
        let failed = event(
            3,
            "tool.finished",
            json!({
                "name": "edit_file",
                "is_error": true,
                "structured": {"path": "src/lib.rs"},
            }),
        );
        assert_eq!(
            classify_event(&failed),
            EventClassification::NonProgress(NonProgressReason::FailedTool)
        );
        let unchanged_effect = event(
            4,
            "effect.settled",
            json!({
                "receipt": {
                    "state": "done",
                    "target": {"kind": "workspace_write", "key": "src/lib.rs"},
                    "response_hash": "aaaa",
                    "evidence": {
                        "paths": [{
                            "path": "src/lib.rs",
                            "status": "unchanged",
                            "sha256": "aaaa",
                        }],
                    },
                },
            }),
        );
        assert_eq!(
            classify_event(&unchanged_effect),
            EventClassification::NonProgress(NonProgressReason::NoOp)
        );
    }

    #[test]
    fn apply_patch_can_produce_multiple_bounded_file_evidence() {
        let patch = event(
            1,
            "tool.finished",
            json!({
                "name": "apply_patch",
                "is_error": false,
                "structured": {
                    "dry_run": false,
                    "files": [
                        {"path": "src/a.rs", "status": "modified", "after_sha256": "aaaa"},
                        {"path": "src/b.rs", "status": "unchanged", "after_sha256": "bbbb"},
                        {"path": "src/c.rs", "status": "deleted", "after_sha256": null},
                    ],
                },
            }),
        );
        match classify_event(&patch) {
            EventClassification::Progress(items) => {
                assert_eq!(items.len(), 2);
                assert!(
                    items
                        .iter()
                        .all(|item| item.kind == ProgressEvidenceKind::FileChanged)
                );
            }
            other => panic!("expected file evidence: {other:?}"),
        }
    }

    #[test]
    fn turn_gaps_are_counted_as_no_progress() {
        let mut tracker = tracker();
        tracker.observe(&commit_event(1, "abc123"), 1);
        tracker.finish_turn(1);
        // Turns 2 and 3 produced no events at all.
        assert_eq!(tracker.finish_turn(3), 2);
        assert_eq!(tracker.turns_without_progress(), 2);
        // Progress in turn 4 clears all consecutive no-progress turns.
        tracker.observe(&commit_event(2, "def456"), 4);
        assert_eq!(tracker.turns_without_progress(), 0);
    }

    #[test]
    fn stale_and_wrong_session_events_are_ignored() {
        let mut tracker = tracker();
        tracker.observe(&commit_event(5, "abc123"), 1);
        let stale = tracker.observe(&commit_event(5, "def456"), 2);
        assert!(stale.stale);
        assert!(stale.accepted.is_empty());
        let mut other = commit_event(6, "def456");
        other.session_id = "s2".to_string();
        let mismatch = tracker.observe(&other, 2);
        assert!(mismatch.session_mismatch);
        assert!(mismatch.accepted.is_empty());
        assert_eq!(tracker.evidence_count(), 1);
    }

    #[test]
    fn payload_is_bounded_and_contains_no_model_text_or_file_content() {
        let mut tracker = tracker();
        tracker.observe(
            &event(
                1,
                "test_cache.stored",
                json!({"entry": {"kind": "test", "command": "OPENAI_API_KEY=secret cargo test", "inputs_hash": "i", "status": "passed", "content": "secret file body"}}),
            ),
            1,
        );
        tracker.observe(
            &event(2, "agent.message", json!({"text": "secret prose"})),
            1,
        );
        let payload = tracker.event_payload(&NoProgressPolicy::default());
        let encoded = serde_json::to_string(&payload).expect("payload");
        assert!(!encoded.contains("secret"));
        assert!(!encoded.contains("OPENAI_API_KEY"));
        assert!(!encoded.contains("secret file body"));
        assert_eq!(payload["schema"], NO_PROGRESS_PAYLOAD_SCHEMA);
        assert!(payload["recent_evidence"].as_array().unwrap().len() <= PAYLOAD_EVIDENCE_LIMIT);
    }
}

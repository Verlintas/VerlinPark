//! Bounded, advisory progress ledger for long-running Manager rounds (C12).
//!
//! A Manager may report a small progress round after each planning turn:
//! what it considers satisfied, what looks like an in-loop repeat, what is
//! still progressing, explicit blockers, a suggested next task, an instruction,
//! and a few evidence pointers. This module validates those rounds, appends
//! them to a bounded ledger, and derives a deterministic [`ProgressAssessment`].
//!
//! The ledger is **evidence only**:
//!
//! - it never selects, claims, skips, cancels, retries, or rewrites a plan task;
//! - it never calls a model, a tool, or a provider;
//! - it never merges, pushes, approves, or changes budgets/capacity;
//! - `next_task` and `instruction` are copied suggestions, never applied.
//!
//! The deterministic controller in the daemon may use the assessment as one
//! input to an operator-visible decision, but the assessment is not an
//! authorization token and [`ProgressAssessment::advisory_only`] is always set
//! to `true` by this module.
//!
//! # Wire types
//!
//! The serde types (`ProgressRound`, `ProgressEvidence`, `ProgressEvidenceKind`,
//! `ProgressStatus`, `ProgressAssessment`, `ProgressLedgerResult`) live in
//! `vp-protocol` so the daemon, the CLI, and the store share one wire shape.
//! They are re-exported here for convenience. This module owns the pure policy,
//! validation, append, assessment, and brief-rendering logic.
//!
//! # Round sequence rule
//!
//! `round_id` must end in a positive decimal sequence number (for example `1`,
//! `r1`, or `manager-round-2`). Sequence numbers must start at 1 and increase by
//! exactly one per appended round. Empty ids may be assigned by
//! [`ProgressLedger::push_round`]. The rule is intentionally strict: it makes
//! duplicate, out-of-order, and replayed rounds fail closed without depending on
//! wall-clock time.
//!
//! # Bounds
//!
//! Every list, evidence vector, text field, and rendered brief is bounded.
//! Validation truncates overlong text and overlong lists deterministically and
//! rejects empty rounds, control characters, absolute paths in identifiers,
//! URLs in evidence references, and obvious credential-key assignments. That
//! screening is key-shape based, not a secret classifier.
//!
//! # Status rules
//!
//! Assessment is deterministic and uses only the newest
//! [`ProgressPolicy::max_rounds`] rounds:
//!
//! 1. `insufficient_evidence` — no rounds, or no evidence pointers.
//! 2. `blocked` — the latest round lists an explicit blocker.
//! 3. `completed` — every retained round has evidence, no round lists an
//!    `in_loop` item or blocker, and the latest round introduces new satisfied
//!    work backed by non-note evidence while listing nothing as progressing.
//!    This is still an advisory claim, never a plan/task completion gate.
//! 4. `in_loop` — one `in_loop` item persists for
//!    [`ProgressPolicy::max_in_loop_streak`] consecutive rounds, or the latest
//!    round lists only `in_loop` items without satisfied/progressing items.
//! 5. `stalled` — no new satisfied/progressing item appeared for
//!    [`ProgressPolicy::stall_after_rounds`] trailing rounds.
//! 6. `progressing` — a new satisfied/progressing item appeared within
//!    [`ProgressPolicy::progress_window`] rounds, or the ledger is quiet but
//!    below the stall threshold (`reason = not_stalled`).
//!
//! A `reason_code`, `reason`, `next_task`, and `instruction` accompany the
//! status. The suggestions are echoed for an operator or a later planning turn;
//! nothing in this module executes them.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub use vp_protocol::progress::{
    ProgressAssessment, ProgressEvidence, ProgressEvidenceKind, ProgressLedgerResult,
    ProgressRound, ProgressStatus,
};

/// Default maximum number of retained rounds.
pub const DEFAULT_MAX_ROUNDS: usize = 32;
/// Default maximum number of items in each progress list.
pub const DEFAULT_MAX_ITEMS_PER_LIST: usize = 8;
/// Default maximum number of evidence pointers per round.
pub const DEFAULT_MAX_EVIDENCE: usize = 8;
/// Default maximum characters for `instruction`/`next_task`.
pub const DEFAULT_MAX_TEXT_CHARS: usize = 1_000;
/// Default maximum characters for one evidence reference/identifier.
pub const DEFAULT_MAX_REFERENCE_CHARS: usize = 200;
/// Default repeated `in_loop` streak that triggers an in-loop assessment.
pub const DEFAULT_MAX_IN_LOOP_STREAK: usize = 3;
/// Default number of quiet rounds before an assessment is stalled.
pub const DEFAULT_STALL_AFTER_ROUNDS: usize = 4;
/// Default number of recent rounds inspected for new progress.
pub const DEFAULT_PROGRESS_WINDOW: usize = 3;

/// Hard ceiling for `ProgressPolicy::max_rounds` (matches the wire cap).
pub const MAX_ROUNDS_CEILING: usize = 256;
/// Hard ceiling for `ProgressPolicy::max_items_per_list`.
pub const MAX_ITEMS_CEILING: usize = 64;
/// Hard ceiling for `ProgressPolicy::max_evidence`.
pub const MAX_EVIDENCE_CEILING: usize = 64;
/// Hard ceiling for `ProgressPolicy::max_text_chars`.
pub const MAX_TEXT_CEILING: usize = 8_000;
/// Hard ceiling for `ProgressPolicy::max_reference_chars`.
pub const MAX_REFERENCE_CEILING: usize = 1_000;
/// Hard ceiling for `ProgressPolicy::max_in_loop_streak`.
pub const MAX_IN_LOOP_STREAK_CEILING: usize = 64;
/// Hard ceiling for `ProgressPolicy::stall_after_rounds`.
pub const STALL_AFTER_CEILING: usize = 256;
/// Hard ceiling for `ProgressPolicy::progress_window`.
pub const PROGRESS_WINDOW_CEILING: usize = 64;

/// Maximum characters in a round id (matches `vp-protocol`).
pub const MAX_ROUND_ID_CHARS: usize = 80;
/// Maximum characters in one list item (matches `vp-protocol`).
pub const MAX_ITEM_CHARS: usize = 200;
/// Maximum characters retained in a session id.
pub const MAX_SESSION_ID_CHARS: usize = 200;
/// Maximum characters in an optional `created_at` string.
pub const MAX_CREATED_AT_CHARS: usize = 64;
/// Hard cap for a rendered brief, including the truncation marker.
pub const MAX_BRIEF_CHARS: usize = 4_096;
/// Schema label included in [`ProgressLedger::assessment_payload`].
pub const ASSESSMENT_SCHEMA: &str = "verlinpark.progress_assessment/v1";

/// Stable typed error codes. The daemon maps these to RPC/tool errors.
pub mod codes {
    /// The policy contains a zero or out-of-range bound.
    pub const PROGRESS_POLICY_INVALID: &str = "PROGRESS_POLICY_INVALID";
    /// The session id is empty, malformed, or credential-like.
    pub const PROGRESS_SESSION_INVALID: &str = "PROGRESS_SESSION_INVALID";
    /// A round field is invalid after normalization.
    pub const PROGRESS_ROUND_INVALID: &str = "PROGRESS_ROUND_INVALID";
    /// Reserved for callers that enforce an external hard cap.
    /// [`ProgressLedger::push_round`] instead retains the newest
    /// [`ProgressPolicy::max_rounds`] window.
    pub const PROGRESS_ROUND_CAP: &str = "PROGRESS_ROUND_CAP";
    /// A round id does not follow the monotonic sequence rule.
    pub const PROGRESS_ROUND_SEQUENCE: &str = "PROGRESS_ROUND_SEQUENCE";
}

/// Stable lowercase reason codes carried by [`ProgressAssessment`].
pub mod reasons {
    pub const NO_ROUNDS: &str = "no_rounds";
    pub const NO_EVIDENCE: &str = "no_evidence";
    pub const BLOCKED: &str = "blocked";
    pub const COMPLETED: &str = "completed";
    pub const IN_LOOP_STREAK: &str = "in_loop_streak";
    pub const IN_LOOP_NO_ADVANCE: &str = "in_loop_no_advance";
    pub const STALLED: &str = "stalled";
    pub const PROGRESSING: &str = "progressing";
    pub const NOT_STALLED: &str = "not_stalled";
}

/// Typed progress-ledger failure with a stable code and operator hint.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {message}")]
pub struct ProgressLedgerError {
    pub code: &'static str,
    pub message: String,
    pub hint: &'static str,
}

impl ProgressLedgerError {
    /// Construct a typed error. Kept public so integration code can preserve
    /// the same shape for adjacent validation.
    pub fn new(code: &'static str, message: impl Into<String>, hint: &'static str) -> Self {
        Self {
            code,
            message: message.into(),
            hint,
        }
    }
}

/// Result alias for the pure ledger API.
pub type ProgressResult<T> = std::result::Result<T, ProgressLedgerError>;

fn invalid_round(message: impl Into<String>) -> ProgressLedgerError {
    ProgressLedgerError::new(
        codes::PROGRESS_ROUND_INVALID,
        message,
        "Report a bounded round with a monotonic round id and at least one of satisfied/in_loop/progressing/blockers.",
    )
}

fn invalid_policy(message: impl Into<String>) -> ProgressLedgerError {
    ProgressLedgerError::new(
        codes::PROGRESS_POLICY_INVALID,
        message,
        "Use non-zero policy bounds within the documented hard ceilings.",
    )
}

fn invalid_session(message: impl Into<String>) -> ProgressLedgerError {
    ProgressLedgerError::new(
        codes::PROGRESS_SESSION_INVALID,
        message,
        "Pass the real session id; the ledger is session-scoped evidence.",
    )
}

/// Bounded policy for validation, assessment, and rendering.
///
/// `serde` uses [`ProgressPolicy::default`] for missing fields, so older
/// records gain new bounds without a migration. [`ProgressPolicy::normalized`]
/// is strict: zero or over-ceiling values fail instead of silently widening.
/// Read-only helpers fall back to the default policy when the caller
/// supplies an invalid one, because they cannot return an error without
/// changing their pure `-> ProgressAssessment` / `-> String` contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProgressPolicy {
    pub max_rounds: usize,
    pub max_items_per_list: usize,
    pub max_evidence: usize,
    pub max_text_chars: usize,
    pub max_reference_chars: usize,
    pub max_in_loop_streak: usize,
    pub stall_after_rounds: usize,
    pub progress_window: usize,
}

impl Default for ProgressPolicy {
    fn default() -> Self {
        Self {
            max_rounds: DEFAULT_MAX_ROUNDS,
            max_items_per_list: DEFAULT_MAX_ITEMS_PER_LIST,
            max_evidence: DEFAULT_MAX_EVIDENCE,
            max_text_chars: DEFAULT_MAX_TEXT_CHARS,
            max_reference_chars: DEFAULT_MAX_REFERENCE_CHARS,
            max_in_loop_streak: DEFAULT_MAX_IN_LOOP_STREAK,
            stall_after_rounds: DEFAULT_STALL_AFTER_ROUNDS,
            progress_window: DEFAULT_PROGRESS_WINDOW,
        }
    }
}

impl ProgressPolicy {
    /// Validate every bound and return the policy unchanged.
    ///
    /// The validation is deliberately strict: out-of-range values are rejected
    /// rather than clamped so a typo cannot silently disable stall detection.
    pub fn normalized(&self) -> ProgressResult<Self> {
        check_bound("max_rounds", self.max_rounds, MAX_ROUNDS_CEILING)?;
        check_bound(
            "max_items_per_list",
            self.max_items_per_list,
            MAX_ITEMS_CEILING,
        )?;
        check_bound("max_evidence", self.max_evidence, MAX_EVIDENCE_CEILING)?;
        check_bound("max_text_chars", self.max_text_chars, MAX_TEXT_CEILING)?;
        check_bound(
            "max_reference_chars",
            self.max_reference_chars,
            MAX_REFERENCE_CEILING,
        )?;
        check_bound(
            "max_in_loop_streak",
            self.max_in_loop_streak,
            MAX_IN_LOOP_STREAK_CEILING,
        )?;
        check_bound(
            "stall_after_rounds",
            self.stall_after_rounds,
            STALL_AFTER_CEILING,
        )?;
        check_bound(
            "progress_window",
            self.progress_window,
            PROGRESS_WINDOW_CEILING,
        )?;
        Ok(self.clone())
    }

    /// Normalize for read-only helpers. An invalid policy falls back to the
    /// bounded default instead of panicking.
    pub fn effective(&self) -> Self {
        self.normalized().unwrap_or_default()
    }
}

fn check_bound(name: &str, value: usize, maximum: usize) -> ProgressResult<()> {
    if value == 0 || value > maximum {
        return Err(invalid_policy(format!(
            "{name} must be between 1 and {maximum}, got {value}"
        )));
    }
    Ok(())
}

/// One normalized round used by the deterministic assessment. It is private so
/// the public wire type stays the single source of truth.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RoundView {
    round_id: String,
    satisfied: Vec<String>,
    in_loop: Vec<String>,
    progressing: Vec<String>,
    blockers: Vec<String>,
    evidence: Vec<(ProgressEvidenceKind, String)>,
    next_task: Option<String>,
    instruction: Option<String>,
}

/// One session-scoped ledger. The assessment field is derived state kept in
/// sync by [`ProgressLedger::push_round`] and
/// [`ProgressLedger::refresh_assessment`]; callers may always recompute it with
/// [`ProgressLedger::assess`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProgressLedger {
    pub session_id: String,
    pub plan_revision: u64,
    pub rounds: Vec<ProgressRound>,
    pub assessment: ProgressAssessment,
}

impl ProgressLedger {
    /// Create an empty ledger with a bounded session id.
    ///
    /// `plan_revision` is metadata for correlation; it is never changed by this
    /// module. Use `0` when no plan exists yet. This is a strict constructor:
    /// empty, control-containing, URL-shaped, and credential-like session ids
    /// fail. [`ProgressLedger::try_new`] is an explicit alias for callers that
    /// prefer a `try_` name at trust boundaries.
    pub fn new(session_id: impl Into<String>, plan_revision: u64) -> ProgressResult<Self> {
        Self::try_new(session_id, plan_revision)
    }

    /// Strict constructor used at trust boundaries. Rejects an empty,
    /// control-containing, URL-shaped, or credential-like session id.
    pub fn try_new(session_id: impl Into<String>, plan_revision: u64) -> ProgressResult<Self> {
        let session_id = normalize_single_line_checked(
            &session_id.into(),
            MAX_SESSION_ID_CHARS,
            "session_id",
            false,
        )
        .map_err(|error| invalid_session(error.message))?;
        if session_id.is_empty() {
            return Err(invalid_session("session_id must not be empty".to_string()));
        }
        if looks_like_url(&session_id) {
            return Err(invalid_session(
                "session_id must be an identifier, not a URL".to_string(),
            ));
        }
        let mut ledger = Self {
            session_id,
            plan_revision,
            rounds: Vec::new(),
            assessment: ProgressAssessment::default(),
        };
        ledger.assessment = ledger.assess(&ProgressPolicy::default());
        Ok(ledger)
    }

    /// Adopt a stored wire result without revalidating it. Persisted ledgers
    /// may predate a policy change; [`ProgressLedger::assess`] bounds untrusted
    /// input defensively.
    pub fn from_result(result: ProgressLedgerResult) -> Self {
        let mut assessment = result.assessment;
        assessment.advisory_only = true;
        Self {
            session_id: result.session_id,
            plan_revision: result.plan_revision,
            rounds: result.rounds,
            assessment,
        }
    }

    /// Project the ledger into the shared wire result, retaining at most the
    /// policy's newest rounds and recomputing the assessment.
    pub fn to_result(&self, policy: &ProgressPolicy, truncated: bool) -> ProgressLedgerResult {
        let policy = policy.effective();
        let start = self.rounds.len().saturating_sub(policy.max_rounds);
        let rounds = self.rounds[start..]
            .iter()
            .map(|round| lossy_round(round, &policy))
            .collect::<Vec<_>>();
        let mut assessment = assess_rounds(&rounds, &policy);
        assessment.advisory_only = true;
        ProgressLedgerResult {
            session_id: lossy_single_line(&self.session_id, MAX_SESSION_ID_CHARS),
            plan_revision: self.plan_revision,
            rounds,
            assessment,
            truncated,
            advisory_only: true,
        }
    }

    /// Latest appended round, if any.
    pub fn latest_round(&self) -> Option<&ProgressRound> {
        self.rounds.last()
    }

    /// Deterministically computed next round id. Returns an error when the
    /// latest round id does not follow the sequence rule.
    pub fn next_round_id(&self) -> ProgressResult<String> {
        let expected = match self.rounds.last() {
            None => 1,
            Some(round) => {
                let previous = round_sequence(&round.round_id)
                    .ok_or_else(|| sequence_error(&round.round_id, 0))?;
                previous
                    .checked_add(1)
                    .ok_or_else(|| sequence_error(&round.round_id, previous))?
            }
        };
        Ok(format!("r{expected}"))
    }

    /// Append one round after validation.
    ///
    /// Rules:
    /// - the ledger retains at most `policy.max_rounds` rounds; appending past
    ///   the bound drops the oldest rounds deterministically;
    /// - a sequence number starts at 1 and increases by exactly one;
    /// - an empty `round_id` is assigned the next `rN` value;
    /// - a failed append leaves the ledger untouched;
    /// - the derived assessment is refreshed after a successful append.
    pub fn push_round(
        &mut self,
        policy: &ProgressPolicy,
        mut round: ProgressRound,
    ) -> ProgressResult<()> {
        let policy = policy.normalized()?;
        let expected = match self.rounds.last() {
            None => 1,
            Some(previous) => {
                let value = round_sequence(&previous.round_id)
                    .ok_or_else(|| sequence_error(&previous.round_id, 0))?;
                value
                    .checked_add(1)
                    .ok_or_else(|| sequence_error(&previous.round_id, value))?
            }
        };
        if round.round_id.trim().is_empty() {
            round.round_id = format!("r{expected}");
        }
        let normalized = validate_round(&round, &policy)?;
        let actual = round_sequence(&normalized.round_id)
            .ok_or_else(|| sequence_error(&normalized.round_id, expected))?;
        if actual != expected {
            return Err(sequence_error(&normalized.round_id, expected));
        }
        let excess = self
            .rounds
            .len()
            .saturating_add(1)
            .saturating_sub(policy.max_rounds);
        if excess > 0 {
            self.rounds.drain(0..excess);
        }
        self.rounds.push(normalized);
        self.assessment = self.assess(&policy);
        Ok(())
    }

    /// Recompute the assessment with `policy` and store it.
    pub fn refresh_assessment(&mut self, policy: &ProgressPolicy) -> ProgressAssessment {
        let policy = policy.effective();
        let assessment = self.assess(&policy);
        self.assessment = assessment.clone();
        assessment
    }

    /// Deterministic assessment over the newest `policy.max_rounds` rounds.
    /// This is read-only and never mutates rounds, plans, or tasks.
    pub fn assess(&self, policy: &ProgressPolicy) -> ProgressAssessment {
        assess_rounds(&self.rounds, policy)
    }

    /// Bounded, deterministic Markdown-free brief for prompts and UI surfaces.
    /// It contains status, reason, counts, at most `max_items_per_list` items
    /// per list, at most `max_evidence` evidence pointers, and no timestamps.
    pub fn render_brief(&self, policy: &ProgressPolicy) -> String {
        let policy = policy.effective();
        let views = build_round_views(&self.rounds, &policy);
        let assessment = assess_views(&views, &policy);
        let latest = views.last();
        let mut brief = String::new();
        brief.push_str(&format!(
            "[progress] status={} reason={} advisory_only=true\n",
            assessment.status.as_str(),
            assessment.reason_code
        ));
        brief.push_str(&format!(
            "session={} plan_revision={} rounds={}\n",
            lossy_single_line(&self.session_id, MAX_SESSION_ID_CHARS),
            self.plan_revision,
            views.len()
        ));
        brief.push_str(&format!(
            "satisfied_count={} evidence_count={} in_loop_streak={} stalled_rounds={}\n",
            assessment.satisfied_count,
            assessment.evidence_count,
            assessment.in_loop_streak,
            assessment.stalled_rounds
        ));
        brief.push_str(&format!(
            "reason: {}\n",
            lossy_text(&assessment.reason, policy.max_text_chars)
        ));
        if let Some(latest) = latest {
            if !latest.round_id.is_empty() {
                brief.push_str(&format!("round={}\n", latest.round_id));
            }
            append_list(&mut brief, "satisfied", &latest.satisfied);
            append_list(&mut brief, "in_loop", &latest.in_loop);
            append_list(&mut brief, "progressing", &latest.progressing);
            append_list(&mut brief, "blockers", &latest.blockers);
        }
        if let Some(next_task) = assessment.next_task.as_deref() {
            brief.push_str(&format!(
                "next_task: {}\n",
                lossy_single_line(next_task, policy.max_text_chars)
            ));
        }
        if let Some(instruction) = assessment.instruction.as_deref() {
            brief.push_str(&format!(
                "instruction: {}\n",
                lossy_text(instruction, policy.max_text_chars)
            ));
        }
        if let Some(latest) = latest
            && !latest.evidence.is_empty()
        {
            let rendered = latest
                .evidence
                .iter()
                .take(policy.max_evidence)
                .map(|(kind, reference)| format!("{}:{}", kind.as_str(), reference))
                .collect::<Vec<_>>()
                .join("; ");
            brief.push_str(&format!(
                "evidence: {}\n",
                lossy_single_line(&rendered, MAX_BRIEF_CHARS)
            ));
        }
        truncate_chars(&brief, MAX_BRIEF_CHARS)
    }

    /// JSON value suitable for a `progress.assessed` event or an RPC response.
    /// The stored assessment is embedded as-is; callers that need a fresh one
    /// should call [`ProgressLedger::assess`] or
    /// [`ProgressLedger::refresh_assessment`] first.
    pub fn assessment_payload(&self) -> Value {
        let mut payload = assessment_value(&self.assessment);
        if let Value::Object(ref mut object) = payload {
            object.insert(
                "session_id".to_string(),
                json!(lossy_single_line(&self.session_id, MAX_SESSION_ID_CHARS)),
            );
            object.insert("plan_revision".to_string(), json!(self.plan_revision));
            object.insert("round_count".to_string(), json!(self.rounds.len()));
            object.insert(
                "latest_round_id".to_string(),
                match self.rounds.last() {
                    Some(round) => json!(lossy_single_line(&round.round_id, MAX_ROUND_ID_CHARS)),
                    None => Value::Null,
                },
            );
        }
        payload
    }
}

/// Validate and normalize one wire round. Returns a fresh value; the input is
/// never mutated.
pub fn validate_round(
    round: &ProgressRound,
    policy: &ProgressPolicy,
) -> ProgressResult<ProgressRound> {
    let policy = policy.normalized()?;
    let item_chars = item_char_limit(&policy);
    let round_id =
        normalize_single_line_checked(&round.round_id, MAX_ROUND_ID_CHARS, "round_id", false)?;
    if round_sequence(&round_id).is_none() {
        return Err(sequence_error(&round_id, 0));
    }
    let agent_id = normalize_optional_reference(&round.agent_id, &policy, "agent_id")?;
    let agent_name = normalize_optional_reference(&round.agent_name, &policy, "agent_name")?;
    let task_id = normalize_optional_reference(&round.task_id, &policy, "task_id")?;
    let satisfied = normalize_items_checked(
        &round.satisfied,
        policy.max_items_per_list,
        item_chars,
        "satisfied",
    )?;
    let in_loop = normalize_items_checked(
        &round.in_loop,
        policy.max_items_per_list,
        item_chars,
        "in_loop",
    )?;
    let progressing = normalize_items_checked(
        &round.progressing,
        policy.max_items_per_list,
        item_chars,
        "progressing",
    )?;
    let blockers = normalize_items_checked(
        &round.blockers,
        policy.max_items_per_list,
        item_chars,
        "blockers",
    )?;
    if satisfied.is_empty() && in_loop.is_empty() && progressing.is_empty() && blockers.is_empty() {
        return Err(invalid_round(
            "round has no satisfied, in_loop, progressing, or blocker items after normalization",
        ));
    }
    let next_task = normalize_optional_inline(&round.next_task, &policy, "next_task")?;
    let instruction = normalize_optional_text(&round.instruction, &policy, "instruction")?;
    let evidence = normalize_evidence_checked(&round.evidence, &policy)?;
    let created_at = match round.created_at.as_deref() {
        Some(value) => {
            let value =
                normalize_single_line_checked(value, MAX_CREATED_AT_CHARS, "created_at", false)?;
            if value.is_empty() { None } else { Some(value) }
        }
        None => None,
    };
    Ok(ProgressRound {
        round_id,
        agent_id,
        agent_name,
        task_id,
        satisfied,
        in_loop,
        progressing,
        blockers,
        next_task,
        instruction,
        evidence,
        created_at,
    })
}

/// Extension trait so callers can write `round.validate(&policy)` while the
/// shared wire type remains defined in `vp-protocol`.
pub trait ProgressRoundExt {
    /// Validate and normalize this round. See [`validate_round`].
    fn validate(&self, policy: &ProgressPolicy) -> ProgressResult<ProgressRound>;
}

impl ProgressRoundExt for ProgressRound {
    fn validate(&self, policy: &ProgressPolicy) -> ProgressResult<ProgressRound> {
        validate_round(self, policy)
    }
}

/// Assess an arbitrary bounded round slice without constructing a ledger.
/// Only the newest `policy.max_rounds` rounds participate.
pub fn assess_rounds(rounds: &[ProgressRound], policy: &ProgressPolicy) -> ProgressAssessment {
    let policy = policy.effective();
    let views = build_round_views(rounds, &policy);
    assess_views(&views, &policy)
}

/// Serialize one assessment to the bounded event shape. Always reports
/// `advisory_only: true`, even if the input value was tampered with after
/// deserialization.
pub fn assessment_value(assessment: &ProgressAssessment) -> Value {
    let policy = ProgressPolicy::default();
    json!({
        "schema": ASSESSMENT_SCHEMA,
        "advisory_only": true,
        "status": assessment.status.as_str(),
        "reason_code": lossy_single_line(&assessment.reason_code, MAX_ROUND_ID_CHARS),
        "reason": lossy_text(&assessment.reason, policy.max_text_chars),
        "in_loop_streak": assessment.in_loop_streak,
        "stalled_rounds": assessment.stalled_rounds,
        "satisfied_count": assessment.satisfied_count,
        "evidence_count": assessment.evidence_count,
        "next_task": assessment
            .next_task
            .as_deref()
            .map(|value| lossy_single_line(value, policy.max_text_chars)),
        "instruction": assessment
            .instruction
            .as_deref()
            .map(|value| lossy_text(value, policy.max_text_chars)),
    })
}

/// Aliased free function for callers that name the event body a payload.
/// [`ProgressLedger::assessment_payload`] adds session/revision metadata.
pub fn assessment_payload(assessment: &ProgressAssessment) -> Value {
    assessment_value(assessment)
}

/// Parse the trailing positive decimal sequence of a round id.
///
/// Accepted examples: `1`, `r2`, `round-3`, `manager.round-4`. Rejected: empty
/// ids, ids without trailing digits, zero, leading zeros, non-ASCII digits, and
/// ids longer than [`MAX_ROUND_ID_CHARS`].
pub fn round_sequence(round_id: &str) -> Option<u64> {
    let value = round_id.trim();
    if value.is_empty() || value.starts_with('-') || value.chars().count() > MAX_ROUND_ID_CHARS {
        return None;
    }
    if !value.chars().all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | ':')
    }) {
        return None;
    }
    let bytes = value.as_bytes();
    let mut split = bytes.len();
    while split > 0 && bytes[split - 1].is_ascii_digit() {
        split -= 1;
    }
    if split == bytes.len() {
        return None;
    }
    let suffix = &value[split..];
    if suffix.len() > 1 && suffix.starts_with('0') {
        return None;
    }
    let sequence = suffix.parse::<u64>().ok()?;
    if sequence == 0 {
        return None;
    }
    Some(sequence)
}

fn sequence_error(round_id: &str, expected: u64) -> ProgressLedgerError {
    let message = if expected == 0 {
        format!(
            "round_id '{round_id}' must end in a positive decimal sequence (for example r1); leading zeros and missing digits are rejected"
        )
    } else {
        format!("round_id '{round_id}' does not match the expected next sequence {expected}")
    };
    ProgressLedgerError::new(
        codes::PROGRESS_ROUND_SEQUENCE,
        message,
        "Append rounds in order starting at 1; use `next_round_id()` or leave round_id empty.",
    )
}

fn build_round_views(rounds: &[ProgressRound], policy: &ProgressPolicy) -> Vec<RoundView> {
    let start = rounds.len().saturating_sub(policy.max_rounds);
    rounds[start..]
        .iter()
        .map(|round| round_view(round, policy))
        .collect()
}

fn round_view(round: &ProgressRound, policy: &ProgressPolicy) -> RoundView {
    let item_chars = item_char_limit(policy);
    RoundView {
        round_id: lossy_single_line(&round.round_id, MAX_ROUND_ID_CHARS),
        satisfied: lossy_items(&round.satisfied, policy.max_items_per_list, item_chars),
        in_loop: lossy_items(&round.in_loop, policy.max_items_per_list, item_chars),
        progressing: lossy_items(&round.progressing, policy.max_items_per_list, item_chars),
        blockers: lossy_items(&round.blockers, policy.max_items_per_list, item_chars),
        evidence: lossy_evidence(round, policy)
            .into_iter()
            .map(|item| (item.kind, item.reference))
            .collect(),
        next_task: lossy_optional_single_line(round.next_task.as_deref(), policy.max_text_chars),
        instruction: lossy_optional_text(round.instruction.as_deref(), policy.max_text_chars),
    }
}

fn assess_views(views: &[RoundView], policy: &ProgressPolicy) -> ProgressAssessment {
    if views.is_empty() {
        return ProgressAssessment {
            status: ProgressStatus::InsufficientEvidence,
            reason_code: reasons::NO_ROUNDS.to_string(),
            reason: "no progress rounds recorded".to_string(),
            ..ProgressAssessment::default()
        };
    }

    let latest = match views.last() {
        Some(latest) => latest,
        None => {
            return ProgressAssessment {
                status: ProgressStatus::InsufficientEvidence,
                reason_code: reasons::NO_ROUNDS.to_string(),
                reason: "no progress rounds recorded".to_string(),
                ..ProgressAssessment::default()
            };
        }
    };

    let mut satisfied_keys = BTreeSet::new();
    let mut evidence_count = 0usize;
    for view in views {
        for item in &view.satisfied {
            satisfied_keys.insert(item.to_lowercase());
        }
        evidence_count = evidence_count.saturating_add(view.evidence.len());
    }

    let current_streaks = in_loop_streaks(views);
    let in_loop_streak = current_streaks.values().copied().max().unwrap_or(0);
    let stalled_rounds = quiet_rounds(views);
    let latest_in_loop_no_advance =
        !latest.in_loop.is_empty() && latest.satisfied.is_empty() && latest.progressing.is_empty();
    let streak_triggered = in_loop_streak >= policy.max_in_loop_streak;
    let all_rounds_have_evidence = views.iter().all(|view| !view.evidence.is_empty());
    let no_loops_or_blockers = views
        .iter()
        .all(|view| view.in_loop.is_empty() && view.blockers.is_empty());
    let latest_has_strong_evidence = latest
        .evidence
        .iter()
        .any(|(kind, reference)| *kind != ProgressEvidenceKind::Note && !reference.is_empty());
    let latest_introduced_satisfied = introduced_satisfied_rounds(views)
        .last()
        .copied()
        .unwrap_or(false);
    let completed = all_rounds_have_evidence
        && no_loops_or_blockers
        && latest_has_strong_evidence
        && latest_introduced_satisfied
        && !latest.satisfied.is_empty()
        && latest.progressing.is_empty();
    let recent_progress = recent_new_progress(views, policy.progress_window);
    let stalled = stalled_rounds as usize >= policy.stall_after_rounds;

    let (status, reason_code, reason) = if evidence_count == 0 {
        (
            ProgressStatus::InsufficientEvidence,
            reasons::NO_EVIDENCE,
            format!(
                "{} round(s) recorded but no evidence pointers; assessment cannot claim progress",
                views.len()
            ),
        )
    } else if !latest.blockers.is_empty() {
        (
            ProgressStatus::Blocked,
            reasons::BLOCKED,
            format!(
                "the latest round reports {} blocker(s); this is advisory and does not change task state",
                latest.blockers.len()
            ),
        )
    } else if completed {
        (
            ProgressStatus::Completed,
            reasons::COMPLETED,
            "the latest round reports newly satisfied work with strong evidence and no in_loop/blockers; advisory completion only, not a task or merge gate"
                .to_string(),
        )
    } else if streak_triggered {
        (
            ProgressStatus::InLoop,
            reasons::IN_LOOP_STREAK,
            format!(
                "an in_loop item repeated for {in_loop_streak} consecutive rounds (threshold {})",
                policy.max_in_loop_streak
            ),
        )
    } else if latest_in_loop_no_advance {
        (
            ProgressStatus::InLoop,
            reasons::IN_LOOP_NO_ADVANCE,
            "the latest round lists in_loop items without satisfied or progressing items"
                .to_string(),
        )
    } else if stalled {
        (
            ProgressStatus::Stalled,
            reasons::STALLED,
            format!(
                "no new satisfied/progressing item for {stalled_rounds} rounds (threshold {})",
                policy.stall_after_rounds
            ),
        )
    } else if recent_progress {
        (
            ProgressStatus::Progressing,
            reasons::PROGRESSING,
            format!(
                "new satisfied/progressing items appeared within the last {} round(s)",
                policy.progress_window.min(views.len())
            ),
        )
    } else {
        (
            ProgressStatus::Progressing,
            reasons::NOT_STALLED,
            format!(
                "no new items in the last {} round(s), but the stall threshold {} is not reached",
                policy.progress_window.min(views.len()),
                policy.stall_after_rounds
            ),
        )
    };

    ProgressAssessment {
        status,
        reason_code: reason_code.to_string(),
        reason,
        in_loop_streak: count32(in_loop_streak),
        stalled_rounds,
        satisfied_count: count32(satisfied_keys.len()),
        evidence_count: count32(evidence_count),
        next_task: latest.next_task.clone(),
        instruction: latest.instruction.clone(),
        advisory_only: true,
    }
}

fn in_loop_streaks(views: &[RoundView]) -> BTreeMap<String, usize> {
    let mut current: BTreeMap<String, usize> = BTreeMap::new();
    for view in views {
        let present: BTreeSet<String> = view
            .in_loop
            .iter()
            .map(|item| item.to_lowercase())
            .collect();
        current.retain(|key, _| present.contains(key));
        for key in present {
            let streak = current.entry(key).or_insert(0);
            *streak = streak.saturating_add(1);
        }
    }
    current
}

fn introduced_satisfied_rounds(views: &[RoundView]) -> Vec<bool> {
    let mut seen = BTreeSet::new();
    let mut introduced = Vec::with_capacity(views.len());
    for view in views {
        let mut had_new = false;
        for item in &view.satisfied {
            if seen.insert(item.to_lowercase()) {
                had_new = true;
            }
        }
        introduced.push(had_new);
    }
    introduced
}

fn introduced_progress_rounds(views: &[RoundView]) -> Vec<bool> {
    let mut seen = BTreeSet::new();
    let mut introduced = Vec::with_capacity(views.len());
    for view in views {
        let mut had_new = false;
        for item in view.satisfied.iter().chain(view.progressing.iter()) {
            if seen.insert(item.to_lowercase()) {
                had_new = true;
            }
        }
        introduced.push(had_new);
    }
    introduced
}

fn quiet_rounds(views: &[RoundView]) -> u32 {
    let introduced = introduced_progress_rounds(views);
    let last_new = introduced
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, had_new)| had_new.then_some(index));
    let quiet = match last_new {
        Some(index) => views.len().saturating_sub(index + 1),
        None => views.len(),
    };
    count32(quiet)
}

fn recent_new_progress(views: &[RoundView], window: usize) -> bool {
    let introduced = introduced_progress_rounds(views);
    let start = introduced.len().saturating_sub(window.max(1));
    introduced[start..].iter().any(|had_new| *had_new)
}

fn item_char_limit(policy: &ProgressPolicy) -> usize {
    policy.max_text_chars.clamp(1, MAX_ITEM_CHARS)
}

fn append_list(brief: &mut String, label: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    let rendered = items
        .iter()
        .map(|item| lossy_single_line(item, MAX_ITEM_CHARS))
        .collect::<Vec<_>>()
        .join("; ");
    brief.push_str(&format!("{label}: {rendered}\n"));
}

fn count32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn normalize_optional_reference(
    value: &Option<String>,
    policy: &ProgressPolicy,
    label: &str,
) -> ProgressResult<Option<String>> {
    match value.as_deref() {
        Some(raw) => {
            let value =
                normalize_single_line_checked(raw, policy.max_reference_chars, label, false)?;
            if value.is_empty() {
                return Ok(None);
            }
            Ok(Some(value))
        }
        None => Ok(None),
    }
}

fn normalize_optional_inline(
    value: &Option<String>,
    policy: &ProgressPolicy,
    label: &str,
) -> ProgressResult<Option<String>> {
    match value.as_deref() {
        Some(raw) => {
            let value = normalize_single_line_checked(raw, policy.max_text_chars, label, true)?;
            if value.is_empty() {
                return Ok(None);
            }
            Ok(Some(value))
        }
        None => Ok(None),
    }
}

fn normalize_optional_text(
    value: &Option<String>,
    policy: &ProgressPolicy,
    label: &str,
) -> ProgressResult<Option<String>> {
    match value.as_deref() {
        Some(raw) => {
            let value = normalize_text_checked(raw, policy.max_text_chars, label)?;
            if value.is_empty() {
                return Ok(None);
            }
            Ok(Some(value))
        }
        None => Ok(None),
    }
}

fn normalize_items_checked(
    values: &[String],
    limit: usize,
    char_limit: usize,
    label: &str,
) -> ProgressResult<Vec<String>> {
    // Inspect a bounded prefix of an untrusted list; validated rounds are
    // already within the limit. This keeps malformed deserialized input from
    // turning validation into unbounded work.
    let scan_limit = limit.saturating_mul(8).saturating_add(8);
    let mut output = Vec::new();
    let mut seen = BTreeSet::new();
    for raw in values.iter().take(scan_limit) {
        let item = normalize_single_line_checked(raw, char_limit, label, false)?;
        if item.is_empty() {
            continue;
        }
        let key = item.to_lowercase();
        if !seen.insert(key) {
            continue;
        }
        output.push(item);
        if output.len() >= limit {
            break;
        }
    }
    Ok(output)
}

fn normalize_evidence_checked(
    evidence: &[ProgressEvidence],
    policy: &ProgressPolicy,
) -> ProgressResult<Vec<ProgressEvidence>> {
    let scan_limit = policy.max_evidence.saturating_mul(8).saturating_add(8);
    let mut output = Vec::new();
    let mut seen = BTreeSet::new();
    for item in evidence.iter().take(scan_limit) {
        let reference = normalize_single_line_checked(
            &item.reference,
            policy.max_reference_chars,
            "evidence.reference",
            false,
        )?;
        if reference.is_empty() {
            return Err(invalid_round(
                "evidence.reference must not be empty".to_string(),
            ));
        }
        let note = match item.note.as_deref() {
            Some(note) => {
                let note = normalize_text_checked(note, item_char_limit(policy), "evidence.note")?;
                if note.is_empty() { None } else { Some(note) }
            }
            None => None,
        };
        let key = format!("{}\u{0}{}", item.kind.as_str(), reference.to_lowercase());
        if !seen.insert(key) {
            continue;
        }
        output.push(ProgressEvidence {
            kind: item.kind,
            reference,
            note,
        });
        if output.len() >= policy.max_evidence {
            break;
        }
    }
    Ok(output)
}

fn normalize_single_line_checked(
    value: &str,
    limit: usize,
    label: &str,
    allow_url: bool,
) -> ProgressResult<String> {
    let limit = limit.max(1);
    let prefix: String = value
        .chars()
        .take(limit.saturating_mul(2).saturating_add(2))
        .collect();
    if let Some(character) = prefix.chars().find(|character| character.is_control()) {
        return Err(invalid_round(format!(
            "{label} contains control character {}",
            character.escape_debug()
        )));
    }
    let collapsed = collapse_whitespace(&prefix);
    let bounded = truncate_chars(&collapsed, limit);
    if bounded.is_empty() {
        return Ok(bounded);
    }
    if looks_like_absolute_path(&bounded) {
        return Err(invalid_round(format!(
            "{label} must not contain an absolute host path"
        )));
    }
    if !allow_url && looks_like_url(&bounded) {
        return Err(invalid_round(format!("{label} must not contain a URL")));
    }
    if has_credential_like_content(&bounded) {
        return Err(invalid_round(format!(
            "{label} contains a credential-like key assignment; store a label or reference, not a secret"
        )));
    }
    Ok(bounded)
}

fn normalize_text_checked(value: &str, limit: usize, label: &str) -> ProgressResult<String> {
    let limit = limit.max(1);
    let prefix: String = value
        .chars()
        .take(limit.saturating_mul(2).saturating_add(2))
        .collect();
    let normalized = normalize_newlines(&prefix);
    if let Some(character) = normalized
        .chars()
        .find(|character| character.is_control() && !matches!(character, '\n' | '\t'))
    {
        return Err(invalid_round(format!(
            "{label} contains control character {}",
            character.escape_debug()
        )));
    }
    let bounded = truncate_chars(normalized.trim(), limit);
    if bounded.is_empty() {
        return Ok(bounded);
    }
    if has_credential_like_content(&bounded) {
        return Err(invalid_round(format!(
            "{label} contains a credential-like key assignment; store a label or reference, not a secret"
        )));
    }
    Ok(bounded)
}

fn lossy_items(values: &[String], limit: usize, char_limit: usize) -> Vec<String> {
    let scan_limit = limit.saturating_mul(8).saturating_add(8);
    let mut output = Vec::new();
    let mut seen = BTreeSet::new();
    for raw in values.iter().take(scan_limit) {
        let item = lossy_single_line(raw, char_limit);
        if item.is_empty() || looks_like_absolute_path(&item) || has_credential_like_content(&item)
        {
            continue;
        }
        let key = item.to_lowercase();
        if !seen.insert(key) {
            continue;
        }
        output.push(item);
        if output.len() >= limit {
            break;
        }
    }
    output
}

fn lossy_evidence(round: &ProgressRound, policy: &ProgressPolicy) -> Vec<ProgressEvidence> {
    let scan_limit = policy.max_evidence.saturating_mul(8).saturating_add(8);
    let mut output = Vec::new();
    let mut seen = BTreeSet::new();
    for item in round.evidence.iter().take(scan_limit) {
        let reference = lossy_single_line(&item.reference, policy.max_reference_chars);
        if reference.is_empty()
            || looks_like_absolute_path(&reference)
            || looks_like_url(&reference)
            || has_credential_like_content(&reference)
        {
            continue;
        }
        let key = format!("{}\u{0}{}", item.kind.as_str(), reference.to_lowercase());
        if !seen.insert(key) {
            continue;
        }
        let note = item
            .note
            .as_deref()
            .map(|note| lossy_text(note, item_char_limit(policy)))
            .filter(|note| !note.is_empty() && !has_credential_like_content(note));
        output.push(ProgressEvidence {
            kind: item.kind,
            reference,
            note,
        });
        if output.len() >= policy.max_evidence {
            break;
        }
    }
    output
}

fn lossy_round(round: &ProgressRound, policy: &ProgressPolicy) -> ProgressRound {
    ProgressRound {
        round_id: lossy_single_line(&round.round_id, MAX_ROUND_ID_CHARS),
        agent_id: lossy_optional_single_line(round.agent_id.as_deref(), policy.max_reference_chars),
        agent_name: lossy_optional_single_line(
            round.agent_name.as_deref(),
            policy.max_reference_chars,
        ),
        task_id: lossy_optional_single_line(round.task_id.as_deref(), policy.max_reference_chars),
        satisfied: lossy_items(
            &round.satisfied,
            policy.max_items_per_list,
            item_char_limit(policy),
        ),
        in_loop: lossy_items(
            &round.in_loop,
            policy.max_items_per_list,
            item_char_limit(policy),
        ),
        progressing: lossy_items(
            &round.progressing,
            policy.max_items_per_list,
            item_char_limit(policy),
        ),
        blockers: lossy_items(
            &round.blockers,
            policy.max_items_per_list,
            item_char_limit(policy),
        ),
        next_task: lossy_optional_single_line(round.next_task.as_deref(), policy.max_text_chars),
        instruction: lossy_optional_text(round.instruction.as_deref(), policy.max_text_chars),
        evidence: lossy_evidence(round, policy),
        created_at: lossy_optional_single_line(round.created_at.as_deref(), MAX_CREATED_AT_CHARS),
    }
}

fn lossy_optional_text(value: Option<&str>, limit: usize) -> Option<String> {
    let value = value?;
    let value = lossy_text(value, limit);
    if value.is_empty() || has_credential_like_content(&value) {
        None
    } else {
        Some(value)
    }
}

fn lossy_optional_single_line(value: Option<&str>, limit: usize) -> Option<String> {
    let value = value?;
    let value = lossy_single_line(value, limit);
    if value.is_empty() || has_credential_like_content(&value) {
        None
    } else {
        Some(value)
    }
}

fn lossy_single_line(value: &str, limit: usize) -> String {
    let bounded: String = value
        .chars()
        .take(limit.saturating_mul(2).saturating_add(2))
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    truncate_chars(&collapse_whitespace(&bounded), limit.max(1))
}

fn lossy_text(value: &str, limit: usize) -> String {
    let bounded: String = value
        .chars()
        .take(limit.saturating_mul(2).saturating_add(2))
        .map(|character| {
            if character.is_control() && !matches!(character, '\n' | '\t') {
                ' '
            } else {
                character
            }
        })
        .collect();
    truncate_chars(normalize_newlines(&bounded).trim(), limit.max(1))
}

fn normalize_newlines(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\r' {
            if characters.peek() == Some(&'\n') {
                characters.next();
            }
            output.push('\n');
        } else {
            output.push(character);
        }
    }
    output
}

fn collapse_whitespace(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut pending_space = false;
    for character in value.chars() {
        if character.is_whitespace() {
            pending_space = true;
        } else {
            if pending_space && !output.is_empty() {
                output.push(' ');
            }
            pending_space = false;
            output.push(character);
        }
    }
    output
}

fn truncate_chars(value: &str, limit: usize) -> String {
    let limit = limit.max(1);
    if value.chars().count() <= limit {
        return value.to_string();
    }
    if limit == 1 {
        return "…".to_string();
    }
    let mut output: String = value.chars().take(limit - 1).collect();
    output.push('…');
    output
}

fn looks_like_absolute_path(value: &str) -> bool {
    let value = value.trim();
    if value.starts_with('/') || value.starts_with("~/") || value.starts_with("~\\") {
        return true;
    }
    if value.starts_with("\\\\") {
        return true;
    }
    let bytes = value.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
}

fn looks_like_url(value: &str) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    [
        "http://", "https://", "ws://", "wss://", "file://", "ssh://", "git://", "s3://",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
        || lower.starts_with("git@")
}

const CREDENTIAL_KEYS: &[&str] = &[
    "api_key",
    "apikey",
    "access_token",
    "accesstoken",
    "refresh_token",
    "refreshtoken",
    "token",
    "password",
    "passphrase",
    "secret",
    "client_secret",
    "clientsecret",
    "private_key",
    "privatekey",
    "authorization",
    "credential",
    "credentials",
    "bearer",
    "cookie",
    "session_token",
    "sessiontoken",
];

/// Key-shape credential screening shared by validation and lossy rendering.
/// This is not a secret classifier: it rejects obvious assignments such as
/// `password=...`, `"api_key": ...`, and PEM private-key markers.
pub fn has_credential_like_content(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    if lower.contains("-----begin") && lower.contains("private key-----") {
        return true;
    }
    for key in CREDENTIAL_KEYS {
        let mut search_start = 0;
        while let Some(offset) = lower[search_start..].find(key) {
            let start = search_start.saturating_add(offset);
            let end = start.saturating_add(key.len());
            let before_ok = start == 0
                || !is_identifier_byte(lower.as_bytes().get(start.saturating_sub(1)).copied());
            let after = lower[end..].trim_start();
            let after = after.strip_prefix('"').unwrap_or(after).trim_start();
            let assignment = after.starts_with('=') || after.starts_with(':');
            if before_ok && assignment {
                return true;
            }
            search_start = end;
            if search_start >= lower.len() {
                break;
            }
        }
    }
    false
}

fn is_identifier_byte(byte: Option<u8>) -> bool {
    matches!(byte, Some(value) if value.is_ascii_alphanumeric() || matches!(value, b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    //! Small unit checks for private helpers. The behavioral suite lives in
    //! `crates/vp-core/tests/progress_ledger.rs`.

    use super::*;

    #[test]
    fn sequence_parser_is_strict_and_deterministic() {
        assert_eq!(round_sequence("1"), Some(1));
        assert_eq!(round_sequence("r12"), Some(12));
        assert_eq!(round_sequence("manager-round-7"), Some(7));
        assert_eq!(round_sequence("r0"), None);
        assert_eq!(round_sequence("r01"), None);
        assert_eq!(round_sequence("round"), None);
        assert_eq!(round_sequence("-1"), None);
    }

    #[test]
    fn credential_screening_is_key_shape_only() {
        assert!(has_credential_like_content("password=hunter2"));
        assert!(has_credential_like_content("\"api_key\": \"value\""));
        assert!(!has_credential_like_content("token budget increased"));
        assert!(!has_credential_like_content("refresh the token cache"));
    }
}

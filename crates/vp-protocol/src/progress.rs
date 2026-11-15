//! Advisory progress-ledger wire types (C12 / ADR-0220).
//!
//! The ledger is **evidence only**. It never selects a task, rewrites a plan,
//! retries, cancels, merges, or calls a model. The daemon stores bounded Manager
//! rounds so operators and deterministic controllers can see repeated loops,
//! stalls, and suggested next steps without turning model prose into authority.

use serde::{Deserialize, Serialize};

/// Maximum rounds retained in one request/result.
pub const MAX_PROGRESS_ROUNDS: u32 = 256;
/// Maximum items in each of satisfied/in_loop/progressing/blockers.
pub const MAX_PROGRESS_ITEMS: usize = 8;
/// Maximum evidence pointers per round.
pub const MAX_PROGRESS_EVIDENCE: usize = 8;
/// Maximum characters for one list item.
pub const MAX_PROGRESS_ITEM_CHARS: usize = 200;
/// Maximum characters for next_task/instruction.
pub const MAX_PROGRESS_TEXT_CHARS: usize = 1_000;
/// Maximum characters for an evidence reference.
pub const MAX_PROGRESS_REFERENCE_CHARS: usize = 200;
/// Maximum characters for a round id.
pub const MAX_PROGRESS_ROUND_ID_CHARS: usize = 80;
/// Maximum rounds accepted in one `session.progress.report` call.
pub const MAX_REPORT_ROUNDS: usize = 1;
/// Default number of rounds returned by `session.progress`.
pub const DEFAULT_PROGRESS_LIMIT: u32 = 64;

/// Stable error codes for pre-validation failures.
pub mod progress_codes {
    pub const PROGRESS_INVALID: &str = "PROGRESS_INVALID";
    pub const PROGRESS_ROUND_DUPLICATE: &str = "PROGRESS_ROUND_DUPLICATE";
    pub const PROGRESS_LIMIT: &str = "PROGRESS_LIMIT";
    pub const PROGRESS_SESSION_NOT_FOUND: &str = "PROGRESS_SESSION_NOT_FOUND";
    pub const PROGRESS_IMPORTED_READ_ONLY: &str = "PROGRESS_IMPORTED_READ_ONLY";
}

/// Evidence kinds accepted in a progress round.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressEvidenceKind {
    Task,
    Test,
    Commit,
    Artifact,
    Event,
    Note,
}

impl ProgressEvidenceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Test => "test",
            Self::Commit => "commit",
            Self::Artifact => "artifact",
            Self::Event => "event",
            Self::Note => "note",
        }
    }
}

/// One bounded evidence pointer. Evidence is a reference, never file content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressEvidence {
    pub kind: ProgressEvidenceKind,
    #[serde(default)]
    pub reference: String,
    #[serde(default)]
    pub note: Option<String>,
}

impl ProgressEvidence {
    pub fn normalized(self) -> Self {
        Self {
            kind: self.kind,
            reference: self
                .reference
                .trim()
                .chars()
                .take(MAX_PROGRESS_REFERENCE_CHARS)
                .collect(),
            note: self.note.map(|note| {
                note.trim()
                    .chars()
                    .take(MAX_PROGRESS_ITEM_CHARS)
                    .collect::<String>()
            }),
        }
    }
}

/// One bounded Manager progress round.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressRound {
    #[serde(default)]
    pub round_id: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_name: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub satisfied: Vec<String>,
    #[serde(default)]
    pub in_loop: Vec<String>,
    #[serde(default)]
    pub progressing: Vec<String>,
    #[serde(default)]
    pub blockers: Vec<String>,
    #[serde(default)]
    pub next_task: Option<String>,
    #[serde(default)]
    pub instruction: Option<String>,
    #[serde(default)]
    pub evidence: Vec<ProgressEvidence>,
    #[serde(default)]
    pub created_at: Option<String>,
}

/// Deterministic advisory status. Never an instruction to the scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressStatus {
    Progressing,
    InLoop,
    Stalled,
    Blocked,
    Completed,
    InsufficientEvidence,
}

impl ProgressStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Progressing => "progressing",
            Self::InLoop => "in_loop",
            Self::Stalled => "stalled",
            Self::Blocked => "blocked",
            Self::Completed => "completed",
            Self::InsufficientEvidence => "insufficient_evidence",
        }
    }
}

/// Deterministic assessment over the retained rounds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressAssessment {
    pub status: ProgressStatus,
    #[serde(default)]
    pub reason_code: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub in_loop_streak: u32,
    #[serde(default)]
    pub stalled_rounds: u32,
    #[serde(default)]
    pub satisfied_count: u32,
    #[serde(default)]
    pub evidence_count: u32,
    #[serde(default)]
    pub next_task: Option<String>,
    #[serde(default)]
    pub instruction: Option<String>,
    /// Always true: this object is evidence, not an authorization token.
    #[serde(default = "advisory_true")]
    pub advisory_only: bool,
}

fn advisory_true() -> bool {
    true
}

impl Default for ProgressAssessment {
    fn default() -> Self {
        Self {
            status: ProgressStatus::InsufficientEvidence,
            reason_code: "no_rounds".to_string(),
            reason: "no progress rounds recorded".to_string(),
            in_loop_streak: 0,
            stalled_rounds: 0,
            satisfied_count: 0,
            evidence_count: 0,
            next_task: None,
            instruction: None,
            advisory_only: true,
        }
    }
}

/// Result of recording or reading a session progress ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressLedgerResult {
    pub session_id: String,
    #[serde(default)]
    pub plan_revision: u64,
    #[serde(default)]
    pub rounds: Vec<ProgressRound>,
    #[serde(default)]
    pub assessment: ProgressAssessment,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default = "advisory_true")]
    pub advisory_only: bool,
}

/// RPC parameters for recording one Manager round.
#[derive(Debug, Clone, Deserialize)]
pub struct ReportProgressParams {
    pub session_id: String,
    pub round: ProgressRound,
}

/// RPC parameters for reading the ledger.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SessionProgressParams {
    pub session_id: String,
    #[serde(default)]
    pub plan_revision: Option<u64>,
    #[serde(default)]
    pub limit: Option<u32>,
}

/// RPC parameters for an assessment-only read.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SessionProgressAssessmentParams {
    pub session_id: String,
    #[serde(default)]
    pub plan_revision: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_and_evidence_labels_are_stable() {
        assert_eq!(ProgressStatus::InLoop.as_str(), "in_loop");
        assert_eq!(
            ProgressStatus::InsufficientEvidence.as_str(),
            "insufficient_evidence"
        );
        assert_eq!(ProgressEvidenceKind::Artifact.as_str(), "artifact");
        for kind in [
            ProgressEvidenceKind::Task,
            ProgressEvidenceKind::Test,
            ProgressEvidenceKind::Commit,
            ProgressEvidenceKind::Artifact,
            ProgressEvidenceKind::Event,
            ProgressEvidenceKind::Note,
        ] {
            let json = serde_json::to_string(&kind).unwrap();
            let decoded: ProgressEvidenceKind = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded, kind);
        }
    }

    #[test]
    fn default_assessment_is_advisory_and_conservative() {
        let assessment = ProgressAssessment::default();
        assert!(assessment.advisory_only);
        assert_eq!(assessment.status, ProgressStatus::InsufficientEvidence);
        let ledger = ProgressLedgerResult {
            session_id: "ses-1".into(),
            plan_revision: 0,
            rounds: Vec::new(),
            assessment,
            truncated: false,
            advisory_only: true,
        };
        assert!(ledger.advisory_only);
    }

    #[test]
    fn evidence_normalization_is_bounded() {
        let evidence = ProgressEvidence {
            kind: ProgressEvidenceKind::Note,
            reference: "x".repeat(MAX_PROGRESS_REFERENCE_CHARS + 10),
            note: Some("n".repeat(MAX_PROGRESS_ITEM_CHARS + 10)),
        }
        .normalized();
        assert_eq!(
            evidence.reference.chars().count(),
            MAX_PROGRESS_REFERENCE_CHARS
        );
        assert_eq!(
            evidence.note.unwrap().chars().count(),
            MAX_PROGRESS_ITEM_CHARS
        );
    }
}

#[cfg(test)]
mod wire_tests {
    use super::*;

    #[test]
    fn round_defaults_are_empty_and_decodable() {
        let round: ProgressRound = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(round, ProgressRound::default());
        assert!(round.satisfied.is_empty());
        assert!(round.evidence.is_empty());
    }

    #[test]
    fn evidence_kind_rejects_unknown_values() {
        let error = serde_json::from_value::<ProgressEvidenceKind>(serde_json::json!("network"));
        assert!(error.is_err());
        for kind in ["task", "test", "commit", "artifact", "event", "note"] {
            assert!(
                serde_json::from_value::<ProgressEvidenceKind>(serde_json::json!(kind)).is_ok()
            );
        }
    }

    #[test]
    fn assessment_serde_is_additive_and_advisory() {
        let assessment = ProgressAssessment::default();
        let json = serde_json::to_value(&assessment).unwrap();
        assert_eq!(json["advisory_only"], true);
        let decoded: ProgressAssessment = serde_json::from_value(serde_json::json!({
            "status": "stalled",
            "reason_code": "stalled",
            "reason": "no advance"
        }))
        .unwrap();
        assert!(decoded.advisory_only);
        assert_eq!(decoded.status, ProgressStatus::Stalled);
        assert_eq!(decoded.in_loop_streak, 0);
    }

    #[test]
    fn bounds_are_stable_and_positive() {
        const { assert!(MAX_PROGRESS_ITEMS >= 1) };
        const { assert!(MAX_PROGRESS_EVIDENCE >= 1) };
        const { assert!(MAX_PROGRESS_ROUNDS >= 1) };
        const { assert!(DEFAULT_PROGRESS_LIMIT <= MAX_PROGRESS_ROUNDS) };
        const { assert!(MAX_REPORT_ROUNDS == 1) };
        const { assert!(MAX_PROGRESS_TEXT_CHARS >= MAX_PROGRESS_ITEM_CHARS) };
    }
}

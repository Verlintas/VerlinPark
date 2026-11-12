//! Failure attribution and provenance wire types (ADR-0198).
//!
//! The daemon derives every value here from existing session events. The
//! report intentionally excludes prompts, tool arguments, tool output, file
//! contents, command text, and secrets: nodes and evidence carry only stable
//! identifiers, hashes, paths, event sequence numbers, and fixed reason text.
//!
//! Diagnosis is advisory. No type in this module authorizes a retry, cancel,
//! merge, reassignment, approval, budget change, or capacity change.

use serde::{Deserialize, Serialize};

/// Schema version for the advisory diagnosis wire shape. A change is an
/// intentional wire/identity change and must be recorded in the ADR.
pub const DIAGNOSIS_SCHEMA_VERSION: u32 = 1;

/// Stable event kind for an explicitly recorded diagnosis snapshot.
pub const FAILURE_ATTRIBUTION_EVENT: &str = "failure.attribution";

/// Stable reason codes. They are persisted and are part of the wire contract.
pub mod reason_codes {
    pub const UNSUBSTANTIATED_HANDOFF: &str = "unsubstantiated_handoff";
    pub const STALE_EVIDENCE: &str = "stale_evidence";
    pub const FALSE_CONSENSUS: &str = "false_consensus";
    pub const CASCADE_UPSTREAM: &str = "cascade_upstream";
    pub const MISSING_EVIDENCE: &str = "missing_evidence";
    pub const VERIFIER_REJECTION: &str = "verifier_rejection";
    pub const MERGE_CONFLICT: &str = "merge_conflict";
    pub const INTEGRATION_FAILURE: &str = "integration_failure";
    pub const TASK_FAILURE: &str = "task_failure";
    pub const UNATTRIBUTED_FAILURE: &str = "unattributed_failure";
    pub const NO_FAILURE: &str = "no_failure";
}

/// Stable node kinds. Unknown future kinds decode as strings in clients.
pub mod node_kinds {
    pub const TASK: &str = "task";
    pub const AGENT: &str = "agent";
    pub const IDENTITY: &str = "identity";
    pub const ARTIFACT: &str = "artifact";
    pub const COMMIT: &str = "commit";
    pub const TOOL_CALL: &str = "tool_call";
    pub const TEST_EVIDENCE: &str = "test_evidence";
    pub const VERIFICATION: &str = "verification";
    pub const MERGE: &str = "merge";
    pub const WORKSPACE: &str = "workspace";
    pub const EFFECT: &str = "effect";
    pub const CONTRACT: &str = "contract";
    pub const FAILURE: &str = "failure";
}

/// Stable edge kinds.
pub mod edge_kinds {
    pub const DEPENDS_ON: &str = "depends_on";
    pub const PRODUCED: &str = "produced";
    pub const USED: &str = "used";
    pub const VERIFIED: &str = "verified";
    pub const MERGED: &str = "merged";
    pub const FAILED: &str = "failed";
}

/// One bounded evidence pointer. `reference` is never file content, prompt
/// text, command text, or tool output.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosisEvidence {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub reference: String,
    #[serde(default)]
    pub hash: Option<String>,
    #[serde(default)]
    pub event_seq: Option<u64>,
    #[serde(default)]
    pub event_kind: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

/// One node in the bounded provenance DAG. Node ids are deterministic for the
/// same event window and never encode free text.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceNode {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_name: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub identity_id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub at_seq: u64,
    #[serde(default)]
    pub at: String,
    #[serde(default)]
    pub evidence: Vec<DiagnosisEvidence>,
}

/// One directed provenance edge. `depends_on` points from a downstream task to
/// an upstream task; `produced`, `used`, `verified`, and `merged` point from
/// the actor/object to the evidence it produced or consumed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceEdge {
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub to: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub at_seq: u64,
}

/// Bounded graph container. `truncated` is true when any node, edge, depth,
/// or event-window cap was reached.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceGraph {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub roots: Vec<String>,
    #[serde(default)]
    pub nodes: Vec<ProvenanceNode>,
    #[serde(default)]
    pub edges: Vec<ProvenanceEdge>,
    #[serde(default)]
    pub truncated: bool,
}

/// Counts and bounds for a graph without requiring the caller to fetch every
/// node. This is safe to display in a dashboard or CLI header.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceSummary {
    #[serde(default)]
    pub nodes: u32,
    #[serde(default)]
    pub edges: u32,
    #[serde(default)]
    pub tasks: u32,
    #[serde(default)]
    pub agents: u32,
    #[serde(default)]
    pub artifacts: u32,
    #[serde(default)]
    pub commits: u32,
    #[serde(default)]
    pub tool_calls: u32,
    #[serde(default)]
    pub test_evidence: u32,
    #[serde(default)]
    pub verifications: u32,
    #[serde(default)]
    pub merges: u32,
    #[serde(default)]
    pub max_depth: u32,
    #[serde(default)]
    pub depth_limited: bool,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub scanned_events: u32,
    #[serde(default)]
    pub total_events: u64,
    #[serde(default)]
    pub window_start_seq: u64,
    #[serde(default)]
    pub window_end_seq: u64,
    /// SHA-256 of the canonical graph nodes/edges. Stable for the same bounded
    /// window and useful for comparing two reports without copying content.
    #[serde(default)]
    pub digest: String,
}

/// The agent/identity/responsibility attached to a finding. `identity_id` is
/// present only when the event stream itself carried it; the daemon never
/// infers a historical identity from the current assignment table.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttributionAgent {
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_name: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub identity_id: Option<String>,
    /// `root`, `contributing`, `flag`, or `symptom`.
    #[serde(default)]
    pub responsibility: String,
}

/// One deterministic attribution finding. `summary` is generated from the
/// fixed reason template, never from model prose or tool output.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttributionFinding {
    #[serde(default)]
    pub finding_id: String,
    #[serde(default)]
    pub reason_code: String,
    /// `root`, `contributing`, `flag`, `symptom`, or `missing_evidence`.
    #[serde(default)]
    pub severity: String,
    #[serde(default)]
    pub node_id: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub at_seq: u64,
    #[serde(default)]
    pub at: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub agents: Vec<AttributionAgent>,
    #[serde(default)]
    pub evidence: Vec<DiagnosisEvidence>,
    #[serde(default)]
    pub flags: Vec<String>,
}

/// One step on the upstream-to-target cascade path.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CascadeStep {
    #[serde(default)]
    pub node_id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub at_seq: u64,
    /// `root`, `task_dependency`, or `failure`.
    #[serde(default)]
    pub relation: String,
}

/// The failure the report is attributed to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosisTarget {
    /// `task`, `merge_queue`, `verification`, or `integration`.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub at_seq: u64,
}

/// Current or historical session assignment context. This is evidence for the
/// operator, not proof that a specific run used that identity.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosisAssignment {
    #[serde(default)]
    pub identity_id: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub assigned_at: String,
    #[serde(default)]
    pub lease_expires_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionDiagnosisParams {
    pub session_id: String,
    /// Attribute one task. When omitted, the latest observed failure is used.
    #[serde(default)]
    pub target_task_id: Option<String>,
    /// Bounded event window. Defaults to 20,000 and clamps to 500..=50,000.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Include the bounded graph. The summary is always returned.
    #[serde(default)]
    pub include_graph: bool,
}

/// Explicit ledger-recording params. The plain `session.diagnosis` RPC is
/// always read-only; this separate call is the only path that appends a
/// bounded `failure.attribution` event.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionDiagnosisRecordParams {
    pub session_id: String,
    #[serde(default)]
    pub target_task_id: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub include_graph: bool,
}

/// Read-only diagnosis report. The `advisory` flag is always true and is
/// intentionally not configurable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionDiagnosisResult {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub generated_at: String,
    /// `no_failure`, `attributed`, `missing_evidence`, or `bounded`.
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub target: Option<DiagnosisTarget>,
    /// The earliest unsupported claim on the causal path.
    #[serde(default)]
    pub earliest: Option<AttributionFinding>,
    #[serde(default)]
    pub findings: Vec<AttributionFinding>,
    #[serde(default)]
    pub responsible_agents: Vec<AttributionAgent>,
    #[serde(default)]
    pub cascade: Vec<CascadeStep>,
    #[serde(default)]
    pub graph: Option<ProvenanceGraph>,
    #[serde(default)]
    pub graph_summary: ProvenanceSummary,
    /// Bounded count of other failure symptoms in the same event window.
    #[serde(default)]
    pub other_failures: u32,
    #[serde(default)]
    pub assignments: Vec<DiagnosisAssignment>,
    #[serde(default)]
    pub advisory: bool,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
    /// Set only when `session.diagnosis.record` succeeded.
    #[serde(default)]
    pub recorded_event_seq: Option<u64>,
}

/// Bounded, persisted snapshot of one attribution. This is what a
/// `failure.attribution` event carries; it is deliberately smaller than the
/// full report and contains no graph unless the caller recorded a report with
/// `include_graph`, in which case the graph summary is still all that is
/// persisted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureAttributionRecord {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub attribution_id: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub target: Option<DiagnosisTarget>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub reason_code: String,
    #[serde(default)]
    pub earliest_node_id: String,
    #[serde(default)]
    pub earliest_seq: u64,
    #[serde(default)]
    pub responsible_agents: Vec<AttributionAgent>,
    #[serde(default)]
    pub findings: Vec<AttributionFinding>,
    #[serde(default)]
    pub cascade: Vec<CascadeStep>,
    #[serde(default)]
    pub graph_digest: String,
    #[serde(default)]
    pub graph_nodes: u32,
    #[serde(default)]
    pub graph_edges: u32,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub advisory: bool,
    #[serde(default)]
    pub created_at: String,
}

/// One event-backed ledger row for `session.attributions`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedAttribution {
    #[serde(default)]
    pub event_seq: u64,
    #[serde(default)]
    pub recorded_at: String,
    #[serde(default)]
    pub attribution: FailureAttributionRecord,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionAttributionsParams {
    pub session_id: String,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionAttributionsResult {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub attributions: Vec<RecordedAttribution>,
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn diagnosis_params_default_to_read_only_and_bounded() {
        let params: SessionDiagnosisParams =
            serde_json::from_value(json!({"session_id": "ses-1"})).unwrap();
        assert_eq!(params.session_id, "ses-1");
        assert!(params.target_task_id.is_none());
        assert!(params.limit.is_none());
        assert!(!params.include_graph);
        let record: SessionDiagnosisRecordParams =
            serde_json::from_value(json!({"session_id": "ses-1"})).unwrap();
        assert!(record.target_task_id.is_none());
        assert!(!record.include_graph);
    }

    #[test]
    fn diagnosis_report_round_trips_and_defaults_legacy_fields() {
        let report = SessionDiagnosisResult {
            schema_version: DIAGNOSIS_SCHEMA_VERSION,
            session_id: "ses-1".into(),
            generated_at: "2026-11-13T00:00:00Z".into(),
            status: "attributed".into(),
            target: Some(DiagnosisTarget {
                kind: "task".into(),
                id: "t2".into(),
                task_id: Some("t2".into()),
                status: "failed".into(),
                at_seq: 9,
            }),
            earliest: Some(AttributionFinding {
                finding_id: "finding-1".into(),
                reason_code: reason_codes::UNSUBSTANTIATED_HANDOFF.into(),
                severity: "root".into(),
                node_id: "task:t2".into(),
                task_id: Some("t2".into()),
                at_seq: 9,
                at: "2026-11-13T00:00:09Z".into(),
                summary: "the task result cites no upstream commit or artifact hash".into(),
                agents: vec![AttributionAgent {
                    agent_id: Some("agt-1".into()),
                    agent_name: Some("Worker 1".into()),
                    role: Some("worker".into()),
                    task_id: Some("t2".into()),
                    identity_id: None,
                    responsibility: "root".into(),
                }],
                evidence: vec![DiagnosisEvidence {
                    kind: "event".into(),
                    reference: "plan_task.finished".into(),
                    hash: None,
                    event_seq: Some(9),
                    event_kind: Some("plan_task.finished".into()),
                    note: Some("downstream result".into()),
                }],
                flags: vec!["worker".into()],
            }),
            responsible_agents: vec![],
            findings: vec![],
            cascade: vec![CascadeStep {
                node_id: "task:t1".into(),
                kind: "task".into(),
                task_id: Some("t1".into()),
                status: Some("failed".into()),
                at_seq: 5,
                relation: "root".into(),
            }],
            graph: Some(ProvenanceGraph {
                session_id: "ses-1".into(),
                roots: vec!["task:t1".into()],
                nodes: vec![],
                edges: vec![],
                truncated: false,
            }),
            graph_summary: ProvenanceSummary {
                nodes: 2,
                edges: 1,
                tasks: 2,
                ..ProvenanceSummary::default()
            },
            other_failures: 0,
            assignments: vec![],
            advisory: true,
            truncated: false,
            warnings: vec![],
            recorded_event_seq: None,
        };
        let encoded = serde_json::to_value(&report).unwrap();
        let decoded: SessionDiagnosisResult = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, report);
        assert!(decoded.advisory);
    }

    #[test]
    fn legacy_report_decodes_with_empty_defaults() {
        let decoded: SessionDiagnosisResult =
            serde_json::from_value(json!({"session_id": "ses-1", "status": "no_failure"})).unwrap();
        assert_eq!(decoded.schema_version, 0);
        assert!(decoded.findings.is_empty());
        assert!(decoded.earliest.is_none());
        assert!(!decoded.advisory);
        assert!(!decoded.truncated);
        assert_eq!(decoded.graph_summary.nodes, 0);
    }

    #[test]
    fn attribution_record_has_no_content_fields() {
        let record = FailureAttributionRecord {
            schema_version: DIAGNOSIS_SCHEMA_VERSION,
            attribution_id: "fattr-1".into(),
            session_id: "ses-1".into(),
            target: None,
            status: "attributed".into(),
            reason_code: reason_codes::STALE_EVIDENCE.into(),
            earliest_node_id: "commit:abc".into(),
            earliest_seq: 4,
            responsible_agents: vec![],
            findings: vec![],
            cascade: vec![],
            graph_digest: "d".repeat(64),
            graph_nodes: 3,
            graph_edges: 2,
            truncated: false,
            advisory: true,
            created_at: "2026-11-13T00:00:00Z".into(),
        };
        let encoded = serde_json::to_string(&record).unwrap();
        for forbidden in ["prompt", "output", "content", "command", "args"] {
            assert!(
                !encoded.contains(forbidden),
                "{forbidden} leaked: {encoded}"
            );
        }
    }

    #[test]
    fn recorded_attribution_decodes_event_seq() {
        let recorded: RecordedAttribution = serde_json::from_value(json!({
            "event_seq": 17,
            "recorded_at": "2026-11-13T00:00:00Z",
            "attribution": {"schema_version": 1, "session_id": "ses-1"}
        }))
        .unwrap();
        assert_eq!(recorded.event_seq, 17);
        assert_eq!(recorded.attribution.session_id, "ses-1");
    }
}

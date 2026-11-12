//! Failure attribution and the bounded error-cascade ledger (ADR-0198).
//!
//! Everything in this module is derived from existing session events. The
//! builder reads an allowlist of structural fields (ids, statuses, commits,
//! artifact hashes, changed paths, contract/effect identifiers, event
//! sequence numbers) and deliberately ignores prompts, message bodies, tool
//! arguments, tool output, file contents, command text, and provider keys.
//!
//! The report is advisory. It never retries, cancels, merges, reassigns,
//! approves, blocks, or changes a budget or capacity. A diagnosis may record
//! one bounded `failure.attribution` audit event when the caller explicitly
//! asks for it; the default read path only reads events.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use anyhow::Result;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use vp_protocol::diagnosis::{
    AttributionAgent, AttributionFinding, CascadeStep, DIAGNOSIS_SCHEMA_VERSION,
    DiagnosisAssignment, DiagnosisEvidence, DiagnosisTarget, FAILURE_ATTRIBUTION_EVENT,
    FailureAttributionRecord, ProvenanceEdge, ProvenanceGraph, ProvenanceNode, ProvenanceSummary,
    RecordedAttribution, SessionAttributionsParams, SessionAttributionsResult,
    SessionDiagnosisParams, SessionDiagnosisRecordParams, SessionDiagnosisResult, edge_kinds,
    node_kinds, reason_codes,
};
use vp_protocol::types::Event;

/// Default bounded event window for one diagnosis.
pub const DEFAULT_EVENT_WINDOW: u32 = 20_000;
/// Minimum accepted event window.
pub const MIN_EVENT_WINDOW: u32 = 500;
/// Maximum accepted event window.
pub const MAX_EVENT_WINDOW: u32 = 50_000;
/// Maximum graph nodes retained after truncation.
pub const MAX_GRAPH_NODES: usize = 2_000;
/// Maximum graph edges retained after truncation.
pub const MAX_GRAPH_EDGES: usize = 4_000;
/// Maximum dependency depth shown from the target backwards.
pub const MAX_GRAPH_DEPTH: u32 = 64;
/// Maximum findings returned in one report.
pub const MAX_FINDINGS: usize = 16;
/// Maximum cascade steps returned in one report.
pub const MAX_CASCADE_STEPS: usize = 24;
/// Maximum evidence pointers on one node or finding.
pub const MAX_EVIDENCE: usize = 12;
/// Maximum responsible agents returned in one report.
pub const MAX_RESPONSIBLE_AGENTS: usize = 16;
/// Maximum ledger rows parsed from a bounded event window.
pub const MAX_LEDGER_ROWS: usize = 200;

/// Stable Q3-style diagnosis error codes.
pub mod codes {
    pub const DIAGNOSIS_SESSION_NOT_FOUND: &str = "DIAGNOSIS_SESSION_NOT_FOUND";
    pub const DIAGNOSIS_INVALID_WINDOW: &str = "DIAGNOSIS_INVALID_WINDOW";
    pub const DIAGNOSIS_RECORD_INVALID: &str = "DIAGNOSIS_RECORD_INVALID";
}

/// A typed, bounded diagnosis error. Message text never contains prompts,
/// tool output, command text, or secrets.
#[derive(Debug, Clone)]
pub struct DiagnosisError {
    pub code: &'static str,
    pub message: String,
}

impl DiagnosisError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn data(&self) -> Value {
        let (hint, retryable) = error_hint(self.code);
        json!({
            "code": self.code,
            "message": self.message,
            "retryable": retryable,
            "hint": hint,
            "advisory": true,
        })
    }
}

impl std::fmt::Display for DiagnosisError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "error[{}]: {}", self.code, self.message)
    }
}

impl std::error::Error for DiagnosisError {}

fn error_hint(code: &str) -> (&'static str, bool) {
    match code {
        codes::DIAGNOSIS_SESSION_NOT_FOUND => (
            "Refresh the session id; the diagnosis reads an existing session event log only.",
            false,
        ),
        codes::DIAGNOSIS_INVALID_WINDOW => (
            "Use an event window between 500 and 50,000 events and retry.",
            true,
        ),
        codes::DIAGNOSIS_RECORD_INVALID => (
            "Use `session.diagnosis` without `record` for a read-only report, then record a bounded snapshot explicitly.",
            false,
        ),
        _ => (
            "Run `vp diagnose <session>` again after the session log is available.",
            true,
        ),
    }
}

/// Input for the pure, deterministic diagnosis function.
#[derive(Debug, Clone)]
pub struct DiagnosisInput {
    pub session_id: String,
    pub events: Vec<Event>,
    pub total_events: u64,
    pub target_task_id: Option<String>,
    pub include_graph: bool,
    pub assignments: Vec<DiagnosisAssignment>,
}

#[derive(Debug, Clone, Default)]
struct AgentRef {
    id: Option<String>,
    name: Option<String>,
    role: Option<String>,
    task_id: Option<String>,
    identity_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct TaskState {
    id: String,
    deps: Vec<String>,
    plan_seq: u64,
    plan_at: String,
    status: String,
    status_seq: u64,
    status_at: String,
    result_seq: Option<u64>,
    result_at: Option<String>,
    result_status: Option<String>,
    result_agent: Option<AgentRef>,
    failure_seq: Option<u64>,
    failure_at: Option<String>,
    failure_status: Option<String>,
    cited_hashes: BTreeSet<String>,
    produced_hashes: BTreeSet<String>,
    artifact_ids: BTreeSet<String>,
    read_artifact_ids: BTreeSet<String>,
    commits: Vec<(u64, String)>,
    evidence: Vec<DiagnosisEvidence>,
}

#[derive(Debug, Clone, Default)]
struct AgentState {
    agent: AgentRef,
    status: String,
    seq: u64,
    at: String,
    commit: Option<String>,
    changed_paths: Vec<String>,
    approved: Option<bool>,
    merged: bool,
    verified: bool,
}

#[derive(Debug, Clone, Default)]
struct ArtifactState {
    id: String,
    sha256: String,
    task_id: Option<String>,
    agent: AgentRef,
    seq: u64,
    at: String,
    version: u32,
}

#[derive(Debug, Clone, Default)]
struct CommitState {
    hash: String,
    task_id: Option<String>,
    agent: AgentRef,
    seq: u64,
    at: String,
    changed_paths: BTreeSet<String>,
    sources: BTreeSet<String>,
}

#[derive(Debug, Clone, Default)]
struct ToolState {
    id: String,
    name: String,
    task_id: Option<String>,
    agent: AgentRef,
    started_seq: u64,
    finished_seq: Option<u64>,
    at: String,
    is_error: Option<bool>,
    error_code: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct TestState {
    id: String,
    task_id: Option<String>,
    agent: AgentRef,
    seq: u64,
    at: String,
    passed: bool,
    source: String,
    inputs_hash: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct VerificationState {
    id: String,
    task_id: Option<String>,
    agent: AgentRef,
    seq: u64,
    at: String,
    approved: bool,
    verified_worker: Option<String>,
    verified_commit: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct MergeState {
    id: String,
    task_id: Option<String>,
    agent_id: Option<String>,
    agent_name: Option<String>,
    commit: String,
    base_commit: String,
    status: String,
    attempt: u64,
    source: String,
    seq: u64,
    at: String,
    verifier_approved: Option<bool>,
    remote_host: Option<String>,
    bundle_sha256: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct ContractState {
    hash: String,
    task_id: Option<String>,
    role: Option<String>,
    seq: u64,
    at: String,
}

#[derive(Debug, Clone, Default)]
struct EffectState {
    task_id: Option<String>,
    tool: String,
    state: String,
    evidence_hash: Option<String>,
    seq: u64,
    at: String,
}

#[derive(Debug, Clone, Default)]
struct WorkspaceState {
    status: String,
    commit: Option<String>,
    seq: u64,
    at: String,
}

#[derive(Debug, Clone, Default)]
struct IntegrationState {
    task_id: Option<String>,
    status: String,
    seq: u64,
    at: String,
    host: Option<String>,
    branch: Option<String>,
    bundle_sha256: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct Symptom {
    id: String,
    kind: String,
    task_id: Option<String>,
    status: String,
    seq: u64,
    at: String,
    agent: Option<AgentRef>,
    reason_code: String,
    evidence: Vec<DiagnosisEvidence>,
}

#[derive(Debug, Clone, Default)]
struct Scan {
    tasks: BTreeMap<String, TaskState>,
    agents: BTreeMap<String, AgentState>,
    artifacts: BTreeMap<String, ArtifactState>,
    commits: BTreeMap<String, CommitState>,
    tools: BTreeMap<String, ToolState>,
    tests: Vec<TestState>,
    verifications: Vec<VerificationState>,
    merges: BTreeMap<String, MergeState>,
    contracts: BTreeMap<String, ContractState>,
    effects: BTreeMap<String, EffectState>,
    workspaces: BTreeMap<String, WorkspaceState>,
    integrations: BTreeMap<String, IntegrationState>,
    symptoms: Vec<Symptom>,
    intent_evidence: BTreeMap<String, Vec<DiagnosisEvidence>>,
}

/// Build a deterministic, bounded provenance graph from the allowlisted event
/// fields. No event body is copied into the graph.
pub fn build_provenance_graph(session_id: &str, events: &[Event]) -> ProvenanceGraph {
    let scan = scan_events(events);
    build_graph(session_id, &scan)
}

/// Read-only deterministic diagnosis. The caller supplies the bounded event
/// window; this function does not touch SQLite, Git, providers, approvals, or
/// the merge queue.
pub fn diagnose(input: &DiagnosisInput) -> SessionDiagnosisResult {
    let scan = scan_events(&input.events);
    let mut graph = build_graph(&input.session_id, &scan);
    let mut warnings = Vec::new();
    let mut truncated = false;
    if input.total_events > input.events.len() as u64 {
        truncated = true;
        warnings.push(format!(
            "event window contains {} of {} events; older evidence is not attributed",
            input.events.len(),
            input.total_events
        ));
    }

    let target_symptom = select_target(&scan, input.target_task_id.as_deref());
    let target = target_symptom
        .as_ref()
        .map(|symptom| DiagnosisTarget {
            kind: symptom.kind.clone(),
            id: symptom.id.clone(),
            task_id: symptom.task_id.clone(),
            status: symptom.status.clone(),
            at_seq: symptom.seq,
        })
        .or_else(|| {
            input.target_task_id.as_deref().and_then(|task_id| {
                scan.tasks.get(task_id).map(|task| DiagnosisTarget {
                    kind: "task".to_string(),
                    id: task.id.clone(),
                    task_id: Some(task.id.clone()),
                    status: task.status.clone(),
                    at_seq: task.status_seq,
                })
            })
        });

    if input.target_task_id.is_some() && target.is_none() {
        warnings.push("target task was not found in the bounded event window".to_string());
    }

    let (mut findings, earliest, mut cascade, responsible_agents) = match (&target, target_symptom)
    {
        (Some(target), Some(symptom)) => attribute(&scan, target, &symptom),
        _ => (Vec::new(), None, Vec::new(), Vec::new()),
    };

    if findings.len() > MAX_FINDINGS {
        findings.truncate(MAX_FINDINGS);
        truncated = true;
        warnings.push("finding list was truncated at the report cap".to_string());
    }
    if cascade.len() > MAX_CASCADE_STEPS {
        cascade.truncate(MAX_CASCADE_STEPS);
        truncated = true;
        warnings.push("cascade path was truncated at the report cap".to_string());
    }

    let mut depth_limited = false;
    if let Some(target) = &target
        && let Some(task_id) = target.task_id.as_deref()
    {
        depth_limited = prune_graph_depth(&mut graph, task_id, MAX_GRAPH_DEPTH);
        if depth_limited {
            truncated = true;
            warnings.push(format!(
                "provenance graph was limited to {MAX_GRAPH_DEPTH} dependency levels from the target task"
            ));
        }
    }
    if graph.truncated {
        truncated = true;
        warnings.push("provenance graph hit a node or edge cap".to_string());
    }

    let graph_summary = summarize_graph(
        &input.session_id,
        &mut graph,
        input,
        truncated,
        depth_limited,
    );

    let status = match (&target, &earliest) {
        (None, _) => "no_failure",
        (Some(_), Some(finding))
            if finding.severity == "missing_evidence"
                || finding.reason_code == reason_codes::MISSING_EVIDENCE =>
        {
            "missing_evidence"
        }
        (Some(_), Some(_)) => "attributed",
        (Some(_), None) => "missing_evidence",
    }
    .to_string();

    let other_failures = other_failure_count(&scan, target.as_ref());
    SessionDiagnosisResult {
        schema_version: DIAGNOSIS_SCHEMA_VERSION,
        session_id: input.session_id.clone(),
        generated_at: vp_protocol::now_rfc3339(),
        status,
        target,
        earliest,
        findings,
        responsible_agents,
        cascade,
        graph: input.include_graph.then_some(graph),
        graph_summary,
        other_failures,
        assignments: input.assignments.clone(),
        advisory: true,
        truncated,
        warnings,
        recorded_event_seq: None,
    }
}

/// Parse the bounded ledger rows carried by `failure.attribution` events.
/// Malformed rows are skipped rather than turned into synthetic evidence.
pub fn attributions_from_events(events: &[Event]) -> Vec<RecordedAttribution> {
    let mut rows = Vec::new();
    for event in events {
        if event.kind != FAILURE_ATTRIBUTION_EVENT {
            continue;
        }
        let Some(value) = event.payload.get("attribution") else {
            continue;
        };
        let Ok(attribution) = serde_json::from_value::<FailureAttributionRecord>(value.clone())
        else {
            continue;
        };
        if attribution.session_id != event.session_id {
            continue;
        }
        rows.push(RecordedAttribution {
            event_seq: event.seq,
            recorded_at: event.created_at.clone(),
            attribution,
        });
    }
    rows.sort_by_key(|row| std::cmp::Reverse(row.event_seq));
    rows
}

/// Convert a report into the bounded record persisted by `failure.attribution`.
/// The record never carries graph node/edge bodies, prompt text, tool output,
/// or file content.
pub fn attribution_record(
    report: &SessionDiagnosisResult,
    created_at: &str,
) -> FailureAttributionRecord {
    let earliest = report.earliest.as_ref();
    let mut findings = report.findings.iter().take(8).cloned().collect::<Vec<_>>();
    for finding in &mut findings {
        finding.evidence.truncate(6);
        finding.agents.truncate(4);
        finding.flags.truncate(8);
    }
    let mut responsible_agents = report
        .responsible_agents
        .iter()
        .take(8)
        .cloned()
        .collect::<Vec<_>>();
    responsible_agents.truncate(8);
    let mut cascade = report.cascade.iter().take(16).cloned().collect::<Vec<_>>();
    cascade.truncate(16);
    let reason_code = earliest
        .map(|finding| finding.reason_code.clone())
        .unwrap_or_else(|| {
            report
                .target
                .as_ref()
                .map(|target| target.status.clone())
                .unwrap_or_else(|| reason_codes::NO_FAILURE.to_string())
        });
    let attribution_id = {
        let mut hasher = Sha256::new();
        for part in [
            report.session_id.as_str(),
            report
                .target
                .as_ref()
                .map(|target| target.kind.as_str())
                .unwrap_or(""),
            report
                .target
                .as_ref()
                .map(|target| target.id.as_str())
                .unwrap_or(""),
            reason_code.as_str(),
            earliest
                .map(|finding| finding.node_id.as_str())
                .unwrap_or(""),
            report.graph_summary.digest.as_str(),
        ] {
            hasher.update(part.as_bytes());
            hasher.update([0]);
        }
        format!("fattr_{}", hex(&hasher.finalize()))
    };
    FailureAttributionRecord {
        schema_version: DIAGNOSIS_SCHEMA_VERSION,
        attribution_id,
        session_id: report.session_id.clone(),
        target: report.target.clone(),
        status: report.status.clone(),
        reason_code,
        earliest_node_id: earliest
            .map(|finding| finding.node_id.clone())
            .unwrap_or_default(),
        earliest_seq: earliest.map(|finding| finding.at_seq).unwrap_or(0),
        responsible_agents,
        findings,
        cascade,
        graph_digest: report.graph_summary.digest.clone(),
        graph_nodes: report.graph_summary.nodes,
        graph_edges: report.graph_summary.edges,
        truncated: report.truncated,
        advisory: true,
        created_at: created_at.to_string(),
    }
}

impl crate::Engine {
    /// Read-only session diagnosis. This call never appends an event or
    /// mutates a task, session, approval, effect, budget, or merge row.
    pub async fn session_diagnosis(
        &self,
        params: SessionDiagnosisParams,
    ) -> Result<SessionDiagnosisResult> {
        self.diagnose_session(
            &params.session_id,
            params.target_task_id,
            params.limit,
            params.include_graph,
        )
        .await
    }

    /// Explicitly record one bounded attribution snapshot. This is the only
    /// diagnosis path that appends an event; it never performs remediation.
    pub async fn record_session_diagnosis(
        &self,
        params: SessionDiagnosisRecordParams,
    ) -> Result<SessionDiagnosisResult> {
        let session_id = params.session_id.clone();
        let store = self.store.clone();
        let imported = tokio::task::spawn_blocking({
            let session_id = session_id.clone();
            move || -> Result<bool> {
                Ok(store
                    .get_session(&session_id)?
                    .is_some_and(|session| session.status == "imported"))
            }
        })
        .await??;
        if imported {
            return Err(DiagnosisError::new(
                codes::DIAGNOSIS_RECORD_INVALID,
                "imported sessions are read-only archives; diagnosis remains read-only",
            )
            .into());
        }
        let mut report = self
            .diagnose_session(
                &session_id,
                params.target_task_id,
                params.limit,
                params.include_graph,
            )
            .await?;
        let record = attribution_record(&report, &vp_protocol::now_rfc3339());
        let event = self
            .append_event(
                &session_id,
                FAILURE_ATTRIBUTION_EVENT,
                json!({"attribution": record, "advisory": true}),
            )
            .await?;
        report.recorded_event_seq = Some(event.seq);
        Ok(report)
    }

    async fn diagnose_session(
        &self,
        session_id: &str,
        target_task_id: Option<String>,
        requested_limit: Option<u32>,
        include_graph: bool,
    ) -> Result<SessionDiagnosisResult> {
        let limit = requested_limit.unwrap_or(DEFAULT_EVENT_WINDOW);
        if !(MIN_EVENT_WINDOW..=MAX_EVENT_WINDOW).contains(&limit) {
            return Err(DiagnosisError::new(
                codes::DIAGNOSIS_INVALID_WINDOW,
                format!(
                    "event window must be {MIN_EVENT_WINDOW}..={MAX_EVENT_WINDOW}, got {limit}"
                ),
            )
            .into());
        }
        let session_id = session_id.to_string();
        let store = self.store.clone();
        let (events, total_events, assignments) = tokio::task::spawn_blocking({
            let session_id = session_id.clone();
            move || -> Result<(Vec<Event>, u64, Vec<DiagnosisAssignment>)> {
                if store.get_session(&session_id)?.is_none() {
                    return Err(DiagnosisError::new(
                        codes::DIAGNOSIS_SESSION_NOT_FOUND,
                        format!("session not found: {session_id}"),
                    )
                    .into());
                }
                let events = store.list_recent_events_window(&session_id, limit)?;
                let total_events = store.event_count(&session_id)?;
                let assignments = store
                    .list_session_agent_assignments(Some(&session_id), None, None, None, Some(50))?
                    .into_iter()
                    .map(|assignment| DiagnosisAssignment {
                        identity_id: assignment.identity_id,
                        role: Some(assignment.role),
                        status: assignment.status,
                        assigned_at: assignment.assigned_at,
                        lease_expires_at: assignment.lease_expires_at,
                    })
                    .collect();
                Ok((events, total_events, assignments))
            }
        })
        .await??;
        let input = DiagnosisInput {
            session_id,
            events,
            total_events,
            target_task_id,
            include_graph,
            assignments,
        };
        Ok(diagnose(&input))
    }

    /// Bounded read-only ledger of explicitly recorded attributions.
    pub async fn session_attributions(
        &self,
        params: SessionAttributionsParams,
    ) -> Result<SessionAttributionsResult> {
        let limit = params.limit.unwrap_or(50).clamp(1, MAX_LEDGER_ROWS as u32);
        let session_id = params.session_id.clone();
        let store = self.store.clone();
        let (events, total) = tokio::task::spawn_blocking({
            let session_id = session_id.clone();
            move || -> Result<(Vec<Event>, u64)> {
                if store.get_session(&session_id)?.is_none() {
                    return Err(DiagnosisError::new(
                        codes::DIAGNOSIS_SESSION_NOT_FOUND,
                        format!("session not found: {session_id}"),
                    )
                    .into());
                }
                let events = store.list_recent_events_by_kind(
                    &session_id,
                    FAILURE_ATTRIBUTION_EVENT,
                    limit + 1,
                )?;
                let total = store.event_count_by_kind(&session_id, FAILURE_ATTRIBUTION_EVENT)?;
                Ok((events, total))
            }
        })
        .await??;
        let truncated = events.len() > limit as usize;
        let mut attributions = attributions_from_events(&events);
        attributions.truncate(limit as usize);
        Ok(SessionAttributionsResult {
            session_id,
            attributions,
            total,
            truncated,
        })
    }
}

fn scan_events(events: &[Event]) -> Scan {
    let mut scan = Scan::default();
    for event in events {
        match event.kind.as_str() {
            "team_plan.submitted" | "team_plan.revised" => {
                scan_plan_event(&mut scan, event);
            }
            "plan_task.started" => {
                if let Some(task_id) = text(&event.payload, "task_id") {
                    let agent = agent_ref(&event.payload);
                    set_task_status(&mut scan, task_id, "running", event, Some(agent), false);
                }
            }
            "plan_task.finished" => scan_plan_task_finished(&mut scan, event),
            "plan_task.blocked" | "plan_task.cancelled" | "plan_task.interrupted" => {
                let Some(task_id) = text(&event.payload, "task_id") else {
                    continue;
                };
                let status = event.kind.strip_prefix("plan_task.").unwrap_or("failed");
                set_task_status(
                    &mut scan,
                    task_id,
                    status,
                    event,
                    Some(agent_ref(&event.payload)),
                    true,
                );
            }
            "plan_task.retry_scheduled" => {
                if let Some(task_id) = text(&event.payload, "task_id") {
                    set_task_status(
                        &mut scan,
                        task_id,
                        "pending",
                        event,
                        Some(agent_ref(&event.payload)),
                        false,
                    );
                }
            }
            "agent.started" | "agent.spawned" => scan_agent_started(&mut scan, event),
            "agent.finished" => scan_agent_finished(&mut scan, event),
            "tool.started" => scan_tool_started(&mut scan, event),
            "tool.finished" => scan_tool_finished(&mut scan, event),
            "artifact.published" => scan_artifact(&mut scan, event),
            "test_cache.stored" => scan_test_cache(&mut scan, event),
            "merge_queue.enqueued" | "merge_queue.updated" => scan_merge_queue(&mut scan, event),
            "remote.integration.failed"
            | "remote.integration.rejected"
            | "remote.integration.enqueued"
            | "remote.integration.finished" => scan_integration(&mut scan, event),
            "contract.derived" => scan_contract(&mut scan, event),
            "intent.decision" => scan_intent(&mut scan, event),
            "effect.intent" | "effect.settled" | "effect.failed" | "effect.unknown"
            | "effect.reconciled" => scan_effect(&mut scan, event),
            "agent.workspace.created"
            | "agent.workspace.updated"
            | "agent.workspace.landed"
            | "agent.workspace.archived"
            | "agent.workspace.retired"
            | "agent.workspace.released" => scan_workspace(&mut scan, event),
            _ => {}
        }
    }
    pair_verifications(&mut scan);
    scan
}

fn pair_verifications(scan: &mut Scan) {
    let mut pairs = Vec::new();
    for (index, verification) in scan.verifications.iter().enumerate() {
        let Some(task_id) = verification.task_id.as_deref() else {
            continue;
        };
        let mut commits = scan
            .tasks
            .get(task_id)
            .map(|task| task.commits.clone())
            .unwrap_or_default();
        commits.sort_by_key(|(seq, _)| *seq);
        let candidate = commits
            .iter()
            .find(|(seq, _)| *seq > verification.seq)
            .or_else(|| commits.last());
        if let Some((_, hash)) = candidate {
            pairs.push((index, hash.clone()));
        }
    }
    for (index, hash) in pairs {
        scan.verifications[index].verified_commit = Some(hash);
    }
}

fn scan_plan_event(scan: &mut Scan, event: &Event) {
    let plan = event.payload.get("plan").unwrap_or(&event.payload);
    let Some(tasks) = plan.get("tasks").and_then(Value::as_array) else {
        return;
    };
    for task in tasks.iter().take(2_000) {
        let Some(id) = text(task, "id") else {
            continue;
        };
        let deps = string_array(task, "deps", 64);
        let entry = scan.tasks.entry(id.to_string()).or_default();
        entry.id = id.to_string();
        entry.deps = deps;
        entry.plan_seq = event.seq;
        entry.plan_at = event.created_at.clone();
        if entry.status.is_empty() {
            entry.status = "pending".to_string();
            entry.status_seq = event.seq;
            entry.status_at = event.created_at.clone();
        }
        push_evidence(
            &mut entry.evidence,
            evidence_for_event(event, "plan task definition"),
        );
    }
}

fn scan_plan_task_finished(scan: &mut Scan, event: &Event) {
    let Some(task_id) = text(&event.payload, "task_id") else {
        return;
    };
    let status = text(&event.payload, "task_status").unwrap_or("failed");
    let agent = agent_ref(&event.payload);
    set_task_status(
        scan,
        task_id,
        status,
        event,
        Some(agent.clone()),
        matches!(status, "failed" | "blocked" | "interrupted"),
    );
    let Some(task) = scan.tasks.get_mut(task_id) else {
        return;
    };
    if status == "completed" {
        task.result_seq = Some(event.seq);
        task.result_at = Some(event.created_at.clone());
        task.result_status = Some(status.to_string());
        task.result_agent = Some(agent.clone());
    }
    for hash in hash_tokens(text(&event.payload, "summary").unwrap_or_default()) {
        task.cited_hashes.insert(hash);
    }
    if matches!(status, "failed" | "blocked" | "interrupted") {
        let mut evidence = vec![evidence_for_event(event, "task failure")];
        if text(&event.payload, "reason").is_some() {
            evidence.push(DiagnosisEvidence {
                kind: "reason".to_string(),
                reference: "bounded_reason_present".to_string(),
                ..DiagnosisEvidence::default()
            });
        }
        add_symptom(
            scan,
            Symptom {
                id: format!("task:{task_id}:{}", event.seq),
                kind: "task".to_string(),
                task_id: Some(task_id.to_string()),
                status: status.to_string(),
                seq: event.seq,
                at: event.created_at.clone(),
                agent: Some(agent),
                reason_code: reason_codes::TASK_FAILURE.to_string(),
                evidence,
            },
        );
    }
}

fn set_task_status(
    scan: &mut Scan,
    task_id: &str,
    status: &str,
    event: &Event,
    agent: Option<AgentRef>,
    failed: bool,
) {
    let entry = scan.tasks.entry(task_id.to_string()).or_default();
    entry.id = task_id.to_string();
    if entry.plan_seq == 0 {
        entry.plan_seq = event.seq;
        entry.plan_at = event.created_at.clone();
    }
    entry.status = status.to_string();
    entry.status_seq = event.seq;
    entry.status_at = event.created_at.clone();
    push_evidence(
        &mut entry.evidence,
        evidence_for_event(event, "task status"),
    );
    if failed {
        entry.failure_seq = Some(event.seq);
        entry.failure_at = Some(event.created_at.clone());
        entry.failure_status = Some(status.to_string());
        if let Some(agent) = &agent {
            entry.result_agent = Some(agent.clone());
        }
    }
}

fn scan_agent_started(scan: &mut Scan, event: &Event) {
    let agent = agent_ref(&event.payload);
    let Some(key) = agent_key(&agent) else {
        return;
    };
    let entry = scan.agents.entry(key).or_default();
    entry.agent = agent;
    if entry.status.is_empty() || entry.status == "pending" {
        entry.status = "running".to_string();
    }
    if entry.seq == 0 {
        entry.seq = event.seq;
    }
    if entry.at.is_empty() {
        entry.at = event.created_at.clone();
    }
}

fn scan_agent_finished(scan: &mut Scan, event: &Event) {
    let agent = agent_ref(&event.payload);
    let Some(key) = agent_key(&agent) else {
        return;
    };
    let status = text(&event.payload, "status")
        .unwrap_or("unknown")
        .to_string();
    let commit = text(&event.payload, "commit")
        .filter(|value| is_hex_hash(value))
        .map(str::to_string);
    let changed_paths = string_array(&event.payload, "changed_paths", 64);
    let approved = event.payload.get("approved").and_then(Value::as_bool);
    let merged = event
        .payload
        .get("merged")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let verified = event
        .payload
        .get("verified")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if let Some(commit) = &commit {
        record_commit(scan, commit, &agent, event, "agent");
    }
    let entry = scan.agents.entry(key.clone()).or_default();
    entry.agent = agent.clone();
    entry.status = status.clone();
    entry.seq = if entry.seq == 0 { event.seq } else { entry.seq };
    if entry.at.is_empty() {
        entry.at = event.created_at.clone();
    }
    if let Some(commit) = &commit {
        entry.commit = Some(commit.clone());
    }
    entry.changed_paths = changed_paths;
    entry.approved = approved;
    entry.merged = merged;
    entry.verified = verified;

    if agent.role.as_deref() == Some("worker")
        && let Some(task_id) = agent.task_id.as_deref()
        && let Some(task) = scan.tasks.get_mut(task_id)
        && let Some(summary) = text(&event.payload, "summary")
    {
        for hash in hash_tokens(summary) {
            task.cited_hashes.insert(hash);
        }
    }

    if agent.role.as_deref() == Some("verifier")
        && let Some(approved) = approved
    {
        let verification = VerificationState {
            id: format!("{}:{}", key, event.seq),
            task_id: agent.task_id.clone(),
            agent: agent.clone(),
            seq: event.seq,
            at: event.created_at.clone(),
            approved,
            verified_worker: text(&event.payload, "verified_worker").map(str::to_string),
            verified_commit: None,
        };
        if !approved {
            add_symptom(
                scan,
                Symptom {
                    id: format!("verification:{}:{}", key, event.seq),
                    kind: "verification".to_string(),
                    task_id: agent.task_id.clone(),
                    status: "rejected".to_string(),
                    seq: event.seq,
                    at: event.created_at.clone(),
                    agent: Some(agent.clone()),
                    reason_code: reason_codes::VERIFIER_REJECTION.to_string(),
                    evidence: vec![evidence_for_event(event, "Verifier rejection")],
                },
            );
        }
        scan.verifications.push(verification);
    }
    if agent.role.as_deref() == Some("worker")
        && matches!(status.as_str(), "failed" | "changes_requested")
        && agent.task_id.is_some()
    {
        add_symptom(
            scan,
            Symptom {
                id: format!("agent:{}:{}", key, event.seq),
                kind: "agent".to_string(),
                task_id: agent.task_id.clone(),
                status: status.clone(),
                seq: event.seq,
                at: event.created_at.clone(),
                agent: Some(agent.clone()),
                reason_code: reason_codes::TASK_FAILURE.to_string(),
                evidence: vec![evidence_for_event(event, "Worker result failed")],
            },
        );
    }
}

fn scan_tool_started(scan: &mut Scan, event: &Event) {
    let tool_call_id = text(&event.payload, "tool_call_id").unwrap_or("");
    let name = text(&event.payload, "name").unwrap_or("unknown");
    if name.is_empty() {
        return;
    }
    let key = if tool_call_id.is_empty() {
        format!("{name}:{}", event.seq)
    } else {
        tool_call_id.to_string()
    };
    let agent = agent_ref(&event.payload);
    let task_id = agent
        .task_id
        .clone()
        .or_else(|| text(&event.payload, "task_id").map(str::to_string));
    let entry = scan.tools.entry(key.clone()).or_default();
    entry.id = key;
    entry.name = name.to_string();
    entry.task_id = task_id.clone();
    entry.agent = agent;
    entry.started_seq = event.seq;
    entry.at = event.created_at.clone();
    if matches!(name, "read_artifact" | "list_artifacts")
        && let Some(task_id) = task_id
        && let Some(task) = scan.tasks.get_mut(&task_id)
    {
        for artifact_id in artifact_ids_from_input(&event.payload) {
            task.read_artifact_ids.insert(artifact_id);
        }
    }
}

fn scan_tool_finished(scan: &mut Scan, event: &Event) {
    let tool_call_id = text(&event.payload, "tool_call_id").unwrap_or("");
    let name = text(&event.payload, "name").unwrap_or("unknown");
    if name.is_empty() {
        return;
    }
    let key = if tool_call_id.is_empty() {
        format!("{name}:{}", event.seq)
    } else {
        tool_call_id.to_string()
    };
    let agent = agent_ref(&event.payload);
    let task_id = agent
        .task_id
        .clone()
        .or_else(|| text(&event.payload, "task_id").map(str::to_string));
    let is_error = event.payload.get("is_error").and_then(Value::as_bool);
    let entry = scan.tools.entry(key.clone()).or_default();
    if entry.id.is_empty() {
        entry.id = key.clone();
    }
    entry.name = name.to_string();
    entry.task_id = task_id.clone();
    entry.agent = agent.clone();
    entry.started_seq = if entry.started_seq == 0 {
        event.seq
    } else {
        entry.started_seq
    };
    entry.finished_seq = Some(event.seq);
    if entry.at.is_empty() {
        entry.at = event.created_at.clone();
    }
    entry.is_error = is_error;
    entry.error_code = text(&event.payload, "error_code").map(str::to_string);
    if name == "run_project_task"
        && let Some(task_id) = task_id
    {
        scan.tests.push(TestState {
            id: format!("check:{key}"),
            task_id: Some(task_id),
            agent,
            seq: event.seq,
            at: event.created_at.clone(),
            passed: !is_error.unwrap_or(false),
            source: "run_project_task".to_string(),
            inputs_hash: None,
        });
    }
}

fn scan_artifact(scan: &mut Scan, event: &Event) {
    let Some(artifact_id) = text(&event.payload, "artifact_id") else {
        return;
    };
    let sha256 = text(&event.payload, "sha256")
        .filter(|value| is_hex_hash(value))
        .unwrap_or("")
        .to_string();
    let task_id = text(&event.payload, "task_id").map(str::to_string);
    let agent = agent_ref(&event.payload);
    let entry = scan.artifacts.entry(artifact_id.to_string()).or_default();
    entry.id = artifact_id.to_string();
    entry.sha256 = sha256.clone();
    entry.task_id = task_id.clone();
    entry.agent = agent;
    entry.seq = if entry.seq == 0 { event.seq } else { entry.seq };
    entry.at = if entry.at.is_empty() {
        event.created_at.clone()
    } else {
        entry.at.clone()
    };
    entry.version = event
        .payload
        .get("version")
        .and_then(Value::as_u64)
        .unwrap_or(1) as u32;
    if let Some(task_id) = task_id
        && let Some(task) = scan.tasks.get_mut(&task_id)
    {
        task.artifact_ids.insert(artifact_id.to_string());
        if !sha256.is_empty() {
            task.produced_hashes.insert(sha256);
        }
    }
}

fn scan_test_cache(scan: &mut Scan, event: &Event) {
    let Some(entry) = event.payload.get("entry") else {
        return;
    };
    let Some(id) = text(entry, "id") else {
        return;
    };
    let status = text(entry, "status").unwrap_or("unknown");
    scan.tests.push(TestState {
        id: format!("testcache:{id}"),
        task_id: text(entry, "task_id").map(str::to_string),
        agent: AgentRef::default(),
        seq: event.seq,
        at: event.created_at.clone(),
        passed: status == "passed",
        source: "test_cache".to_string(),
        inputs_hash: text(entry, "inputs_hash").map(str::to_string),
    });
}

fn scan_merge_queue(scan: &mut Scan, event: &Event) {
    let Some(entry) = event.payload.get("entry") else {
        return;
    };
    let Some(id) = text(entry, "id") else {
        return;
    };
    let status = text(entry, "status").unwrap_or("unknown").to_string();
    let commit = text(entry, "commit")
        .or_else(|| text(entry, "commit_hash"))
        .filter(|value| is_hex_hash(value))
        .unwrap_or("")
        .to_string();
    let task_id = text(entry, "task_id").map(str::to_string);
    let agent_id = text(entry, "agent_id").map(str::to_string);
    let agent_name = text(entry, "agent_name").map(str::to_string);
    let attempt = entry.get("attempt").and_then(Value::as_u64).unwrap_or(0);
    let base_commit = text(entry, "base_commit")
        .filter(|value| is_hex_hash(value))
        .unwrap_or("")
        .to_string();
    let source = text(entry, "source").unwrap_or("local").to_string();
    let verifier_approved = entry.get("verifier_approved").and_then(Value::as_bool);
    let remote_host = text(entry, "remote_host").map(str::to_string);
    let bundle_sha256 = text(entry, "bundle_sha256")
        .filter(|value| is_hex_hash(value))
        .map(str::to_string);
    let state = scan.merges.entry(id.to_string()).or_default();
    state.id = id.to_string();
    state.task_id = task_id.clone();
    state.agent_id = agent_id.clone();
    state.agent_name = agent_name.clone();
    if !commit.is_empty() {
        state.commit = commit.clone();
    }
    state.base_commit = base_commit;
    state.status = status.clone();
    state.attempt = attempt;
    state.source = source;
    state.seq = event.seq;
    state.at = event.created_at.clone();
    state.verifier_approved = verifier_approved;
    state.remote_host = remote_host;
    state.bundle_sha256 = bundle_sha256;

    if !commit.is_empty() {
        let agent = AgentRef {
            id: agent_id.clone(),
            name: agent_name.clone(),
            role: Some("worker".to_string()),
            task_id: task_id.clone(),
            identity_id: None,
        };
        record_commit(scan, &commit, &agent, event, "merge_queue");
    }
    if matches!(status.as_str(), "conflict" | "failed" | "interrupted") {
        add_symptom(
            scan,
            Symptom {
                id: format!("merge:{id}:{}", event.seq),
                kind: "merge_queue".to_string(),
                task_id: task_id.clone(),
                status: status.clone(),
                seq: event.seq,
                at: event.created_at.clone(),
                agent: agent_name.clone().map(|name| AgentRef {
                    id: agent_id.clone(),
                    name: Some(name),
                    role: Some("worker".to_string()),
                    task_id: task_id.clone(),
                    identity_id: None,
                }),
                reason_code: if status == "conflict" {
                    reason_codes::MERGE_CONFLICT.to_string()
                } else {
                    reason_codes::INTEGRATION_FAILURE.to_string()
                },
                evidence: vec![evidence_for_event(event, "merge queue outcome")],
            },
        );
    }
}

fn scan_integration(scan: &mut Scan, event: &Event) {
    let status = event
        .kind
        .strip_prefix("remote.integration.")
        .unwrap_or("failed")
        .to_string();
    let id = format!("integration:{}", event.seq);
    let task_id = text(&event.payload, "task_id").map(str::to_string);
    let host = text(&event.payload, "host").map(str::to_string);
    let branch = text(&event.payload, "remote_branch")
        .or_else(|| text(&event.payload, "branch"))
        .map(str::to_string);
    let bundle_sha256 = text(&event.payload, "bundle_sha256")
        .filter(|value| is_hex_hash(value))
        .map(str::to_string);
    scan.integrations.insert(
        id.clone(),
        IntegrationState {
            task_id: task_id.clone(),
            status: status.clone(),
            seq: event.seq,
            at: event.created_at.clone(),
            host,
            branch,
            bundle_sha256,
        },
    );
    if matches!(status.as_str(), "failed" | "rejected") {
        add_symptom(
            scan,
            Symptom {
                id: format!("integration:{}", event.seq),
                kind: "integration".to_string(),
                task_id,
                status,
                seq: event.seq,
                at: event.created_at.clone(),
                agent: Some(agent_ref(&event.payload)),
                reason_code: reason_codes::INTEGRATION_FAILURE.to_string(),
                evidence: vec![evidence_for_event(event, "remote integration failure")],
            },
        );
    }
}

fn scan_contract(scan: &mut Scan, event: &Event) {
    let Some(contract) = event.payload.get("contract") else {
        return;
    };
    let Some(id) = text(contract, "contract_id") else {
        return;
    };
    let hash = text(contract, "hash")
        .filter(|value| is_hex_hash(value))
        .unwrap_or("")
        .to_string();
    let task_id = text(contract, "task_id").map(str::to_string);
    scan.contracts.insert(
        id.to_string(),
        ContractState {
            hash,
            task_id: task_id.clone(),
            role: text(contract, "role").map(str::to_string),
            seq: event.seq,
            at: event.created_at.clone(),
        },
    );
    if let Some(task_id) = task_id
        && let Some(task) = scan.tasks.get_mut(&task_id)
    {
        push_evidence(
            &mut task.evidence,
            DiagnosisEvidence {
                kind: "contract".to_string(),
                reference: id.to_string(),
                hash: (!text(contract, "hash").unwrap_or("").is_empty())
                    .then(|| text(contract, "hash").unwrap_or("").to_string()),
                event_seq: Some(event.seq),
                event_kind: Some(event.kind.clone()),
                note: Some("task contract".to_string()),
            },
        );
    }
}

fn scan_intent(scan: &mut Scan, event: &Event) {
    let Some(task_id) = text(&event.payload, "task_id") else {
        return;
    };
    let decision = text(&event.payload, "decision").unwrap_or("unknown");
    let code = text(&event.payload, "code").unwrap_or("");
    let tool = text(&event.payload, "tool").unwrap_or("");
    if decision == "allow" {
        return;
    }
    let evidence = DiagnosisEvidence {
        kind: "intent_decision".to_string(),
        reference: format!("{decision}:{code}"),
        hash: text(&event.payload, "contract_hash")
            .filter(|value| is_hex_hash(value))
            .map(str::to_string),
        event_seq: Some(event.seq),
        event_kind: Some(event.kind.clone()),
        note: (!tool.is_empty()).then(|| format!("tool {}", truncate_ref(tool, 80))),
    };
    scan.intent_evidence
        .entry(task_id.to_string())
        .or_default()
        .push(evidence);
}

fn scan_effect(scan: &mut Scan, event: &Event) {
    let Some(receipt) = event.payload.get("receipt") else {
        return;
    };
    let Some(id) = text(receipt, "id") else {
        return;
    };
    let state = text(receipt, "state").unwrap_or("unknown").to_string();
    let tool = text(receipt, "tool").unwrap_or("unknown").to_string();
    let task_id = text(receipt, "task_id").map(str::to_string);
    let evidence_hash = text(receipt, "response_hash")
        .filter(|value| is_hex_hash(value))
        .or_else(|| {
            receipt
                .get("evidence")
                .and_then(|evidence| {
                    evidence
                        .get("sha256")
                        .or_else(|| evidence.get("commit"))
                        .and_then(Value::as_str)
                })
                .filter(|value| is_hex_hash(value))
        })
        .map(str::to_string);
    scan.effects.insert(
        id.to_string(),
        EffectState {
            task_id: task_id.clone(),
            tool,
            state,
            evidence_hash,
            seq: event.seq,
            at: event.created_at.clone(),
        },
    );
    if let Some(task_id) = task_id
        && let Some(task) = scan.tasks.get_mut(&task_id)
    {
        push_evidence(
            &mut task.evidence,
            DiagnosisEvidence {
                kind: "effect".to_string(),
                reference: id.to_string(),
                hash: None,
                event_seq: Some(event.seq),
                event_kind: Some(event.kind.clone()),
                note: Some(format!(
                    "effect {}",
                    text(receipt, "state").unwrap_or("unknown")
                )),
            },
        );
    }
}

fn scan_workspace(scan: &mut Scan, event: &Event) {
    let Some(workspace) = event.payload.get("workspace") else {
        return;
    };
    let Some(id) = text(workspace, "id") else {
        return;
    };
    let commit = text(workspace, "commit")
        .or_else(|| text(workspace, "landed_commit"))
        .filter(|value| is_hex_hash(value))
        .map(str::to_string);
    scan.workspaces.insert(
        id.to_string(),
        WorkspaceState {
            status: text(workspace, "status").unwrap_or("unknown").to_string(),
            commit,
            seq: event.seq,
            at: event.created_at.clone(),
        },
    );
}

fn add_symptom(scan: &mut Scan, symptom: Symptom) {
    if scan.symptoms.iter().any(|current| current.id == symptom.id) {
        return;
    }
    scan.symptoms.push(symptom);
}

fn record_commit(scan: &mut Scan, hash: &str, agent: &AgentRef, event: &Event, source: &str) {
    if !is_hex_hash(hash) {
        return;
    }
    let entry = scan.commits.entry(hash.to_string()).or_default();
    entry.hash = hash.to_string();
    entry.task_id = agent.task_id.clone();
    entry.agent = agent.clone();
    entry.seq = if entry.seq == 0 { event.seq } else { entry.seq };
    entry.at = if entry.at.is_empty() {
        event.created_at.clone()
    } else {
        entry.at.clone()
    };
    entry.sources.insert(source.to_string());
    if let Some(task_id) = &agent.task_id
        && let Some(task) = scan.tasks.get_mut(task_id)
    {
        task.produced_hashes.insert(hash.to_string());
        if !task.commits.iter().any(|(_, existing)| existing == hash) {
            task.commits.push((event.seq, hash.to_string()));
        }
    }
}

fn build_graph(session_id: &str, scan: &Scan) -> ProvenanceGraph {
    let mut builder = GraphBuilder::default();
    for (task_id, task) in &scan.tasks {
        let mut node = ProvenanceNode {
            id: node_id(node_kinds::TASK, task_id),
            kind: node_kinds::TASK.to_string(),
            label: format!("task {task_id}"),
            task_id: Some(task.id.clone()),
            status: Some(task.status.clone()),
            at_seq: task.plan_seq.max(1),
            at: task.plan_at.clone(),
            evidence: task.evidence.clone(),
            ..ProvenanceNode::default()
        };
        if node.evidence.is_empty() {
            node.evidence.push(DiagnosisEvidence {
                kind: "task".to_string(),
                reference: task_id.clone(),
                ..DiagnosisEvidence::default()
            });
        }
        builder.add_node(node);
    }
    for task in scan.tasks.values() {
        let from = node_id(node_kinds::TASK, &task.id);
        for dep in &task.deps {
            let to = node_id(node_kinds::TASK, dep);
            builder.add_edge(from.clone(), to, edge_kinds::DEPENDS_ON, task.plan_seq);
        }
    }
    for (key, agent) in &scan.agents {
        let id = node_id(node_kinds::AGENT, key);
        builder.add_node(ProvenanceNode {
            id: id.clone(),
            kind: node_kinds::AGENT.to_string(),
            label: format!(
                "agent {}",
                agent.agent.name.as_deref().unwrap_or(key.as_str())
            ),
            task_id: agent.agent.task_id.clone(),
            agent_id: agent.agent.id.clone(),
            agent_name: agent.agent.name.clone(),
            role: agent.agent.role.clone(),
            identity_id: agent.agent.identity_id.clone(),
            status: Some(agent.status.clone()),
            at_seq: agent.seq.max(1),
            at: agent.at.clone(),
            evidence: vec![DiagnosisEvidence {
                kind: "agent".to_string(),
                reference: key.clone(),
                hash: None,
                event_seq: Some(agent.seq),
                event_kind: Some("agent.finished".to_string()),
                note: Some("agent result".to_string()),
            }],
        });
        if let Some(identity_id) = &agent.agent.identity_id {
            let identity_node = node_id(node_kinds::IDENTITY, identity_id);
            builder.add_node(ProvenanceNode {
                id: identity_node.clone(),
                kind: node_kinds::IDENTITY.to_string(),
                label: format!("identity {identity_id}"),
                identity_id: Some(identity_id.clone()),
                at_seq: agent.seq.max(1),
                at: agent.at.clone(),
                ..ProvenanceNode::default()
            });
            builder.add_edge(identity_node, id, edge_kinds::PRODUCED, agent.seq);
        }
    }
    for (id, artifact) in &scan.artifacts {
        let node = node_id(node_kinds::ARTIFACT, id);
        builder.add_node(ProvenanceNode {
            id: node.clone(),
            kind: node_kinds::ARTIFACT.to_string(),
            label: format!("artifact {}", truncate_ref(id, 32)),
            task_id: artifact.task_id.clone(),
            agent_id: artifact.agent.id.clone(),
            agent_name: artifact.agent.name.clone(),
            role: artifact.agent.role.clone(),
            identity_id: artifact.agent.identity_id.clone(),
            status: Some(format!("version {}", artifact.version.max(1))),
            at_seq: artifact.seq.max(1),
            at: artifact.at.clone(),
            evidence: vec![DiagnosisEvidence {
                kind: "artifact_sha256".to_string(),
                reference: id.clone(),
                hash: (!artifact.sha256.is_empty()).then(|| artifact.sha256.clone()),
                event_seq: Some(artifact.seq),
                event_kind: Some("artifact.published".to_string()),
                note: None,
            }],
        });
        if let Some(task_id) = &artifact.task_id {
            builder.add_edge(
                node_id(node_kinds::TASK, task_id),
                node.clone(),
                edge_kinds::PRODUCED,
                artifact.seq,
            );
        }
        if let Some(key) = agent_key(&artifact.agent) {
            builder.add_edge(
                node_id(node_kinds::AGENT, &key),
                node,
                edge_kinds::PRODUCED,
                artifact.seq,
            );
        }
    }
    for (hash, commit) in &scan.commits {
        let node = node_id(node_kinds::COMMIT, hash);
        let mut evidence = vec![DiagnosisEvidence {
            kind: "commit".to_string(),
            reference: truncate_ref(hash, 64),
            hash: Some(hash.clone()),
            event_seq: Some(commit.seq),
            event_kind: Some("commit".to_string()),
            note: None,
        }];
        for (index, path) in commit.changed_paths.iter().take(MAX_EVIDENCE).enumerate() {
            evidence.push(DiagnosisEvidence {
                kind: "changed_path".to_string(),
                reference: truncate_ref(path, 200),
                event_seq: Some(commit.seq),
                note: Some(format!("path {}", index + 1)),
                ..DiagnosisEvidence::default()
            });
        }
        builder.add_node(ProvenanceNode {
            id: node.clone(),
            kind: node_kinds::COMMIT.to_string(),
            label: format!("commit {}", truncate_ref(hash, 12)),
            task_id: commit.task_id.clone(),
            agent_id: commit.agent.id.clone(),
            agent_name: commit.agent.name.clone(),
            role: commit.agent.role.clone(),
            identity_id: commit.agent.identity_id.clone(),
            status: None,
            at_seq: commit.seq.max(1),
            at: commit.at.clone(),
            evidence,
        });
        if let Some(task_id) = &commit.task_id {
            builder.add_edge(
                node_id(node_kinds::TASK, task_id),
                node.clone(),
                edge_kinds::PRODUCED,
                commit.seq,
            );
        }
        if let Some(key) = agent_key(&commit.agent) {
            builder.add_edge(
                node_id(node_kinds::AGENT, &key),
                node,
                edge_kinds::PRODUCED,
                commit.seq,
            );
        }
    }
    for (key, tool) in &scan.tools {
        let (kind, node) = if tool.name == "run_project_task" {
            (
                node_kinds::TEST_EVIDENCE,
                node_id(node_kinds::TEST_EVIDENCE, key),
            )
        } else {
            (node_kinds::TOOL_CALL, node_id(node_kinds::TOOL_CALL, key))
        };
        let status = match tool.is_error {
            Some(true) => "failed",
            Some(false) => "ok",
            None => "started",
        };
        builder.add_node(ProvenanceNode {
            id: node.clone(),
            kind: kind.to_string(),
            label: format!("tool {}", truncate_ref(&tool.name, 48)),
            task_id: tool.task_id.clone(),
            agent_id: tool.agent.id.clone(),
            agent_name: tool.agent.name.clone(),
            role: tool.agent.role.clone(),
            identity_id: tool.agent.identity_id.clone(),
            status: Some(status.to_string()),
            at_seq: tool.started_seq.max(1),
            at: tool.at.clone(),
            evidence: vec![DiagnosisEvidence {
                kind: "tool".to_string(),
                reference: truncate_ref(&tool.name, 80),
                hash: None,
                event_seq: tool.finished_seq.or(Some(tool.started_seq)),
                event_kind: Some(if tool.finished_seq.is_some() {
                    "tool.finished".to_string()
                } else {
                    "tool.started".to_string()
                }),
                note: tool
                    .error_code
                    .clone()
                    .map(|code| format!("error code {}", truncate_ref(&code, 64))),
            }],
        });
        if let Some(task_id) = &tool.task_id {
            builder.add_edge(
                node_id(node_kinds::TASK, task_id),
                node.clone(),
                edge_kinds::PRODUCED,
                tool.started_seq,
            );
        }
        if let Some(key) = agent_key(&tool.agent) {
            builder.add_edge(
                node_id(node_kinds::AGENT, &key),
                node,
                edge_kinds::PRODUCED,
                tool.started_seq,
            );
        }
    }
    for test in &scan.tests {
        let node = node_id(node_kinds::TEST_EVIDENCE, &test.id);
        let status = if test.passed { "passed" } else { "failed" };
        builder.add_node(ProvenanceNode {
            id: node.clone(),
            kind: node_kinds::TEST_EVIDENCE.to_string(),
            label: format!("check {}", truncate_ref(&test.source, 32)),
            task_id: test.task_id.clone(),
            agent_id: test.agent.id.clone(),
            agent_name: test.agent.name.clone(),
            role: test.agent.role.clone(),
            identity_id: test.agent.identity_id.clone(),
            status: Some(status.to_string()),
            at_seq: test.seq.max(1),
            at: test.at.clone(),
            evidence: vec![DiagnosisEvidence {
                kind: "test".to_string(),
                reference: truncate_ref(&test.id, 120),
                hash: test.inputs_hash.clone(),
                event_seq: Some(test.seq),
                event_kind: Some("test_result".to_string()),
                note: Some(test.source.clone()),
            }],
        });
        if let Some(task_id) = &test.task_id {
            builder.add_edge(
                node_id(node_kinds::TASK, task_id),
                node.clone(),
                edge_kinds::PRODUCED,
                test.seq,
            );
        }
        if let Some(key) = agent_key(&test.agent) {
            builder.add_edge(
                node_id(node_kinds::AGENT, &key),
                node,
                edge_kinds::PRODUCED,
                test.seq,
            );
        }
    }
    for verification in &scan.verifications {
        let node = node_id(node_kinds::VERIFICATION, &verification.id);
        builder.add_node(ProvenanceNode {
            id: node.clone(),
            kind: node_kinds::VERIFICATION.to_string(),
            label: format!(
                "verification {}",
                if verification.approved {
                    "approved"
                } else {
                    "rejected"
                }
            ),
            task_id: verification.task_id.clone(),
            agent_id: verification.agent.id.clone(),
            agent_name: verification.agent.name.clone(),
            role: verification.agent.role.clone(),
            identity_id: verification.agent.identity_id.clone(),
            status: Some(if verification.approved {
                "approved".to_string()
            } else {
                "rejected".to_string()
            }),
            at_seq: verification.seq.max(1),
            at: verification.at.clone(),
            evidence: vec![DiagnosisEvidence {
                kind: "verification".to_string(),
                reference: truncate_ref(&verification.id, 120),
                hash: None,
                event_seq: Some(verification.seq),
                event_kind: Some("agent.finished".to_string()),
                note: verification.verified_worker.clone(),
            }],
        });
        if let Some(key) = agent_key(&verification.agent) {
            builder.add_edge(
                node_id(node_kinds::AGENT, &key),
                node.clone(),
                edge_kinds::PRODUCED,
                verification.seq,
            );
        }
        if let Some(task_id) = &verification.task_id {
            builder.add_edge(
                node.clone(),
                node_id(node_kinds::TASK, task_id),
                edge_kinds::VERIFIED,
                verification.seq,
            );
        }
        if let Some(commit) = &verification.verified_commit {
            builder.add_edge(
                node,
                node_id(node_kinds::COMMIT, commit),
                edge_kinds::VERIFIED,
                verification.seq,
            );
        }
    }
    for (id, merge) in &scan.merges {
        let node = node_id(node_kinds::MERGE, id);
        builder.add_node(ProvenanceNode {
            id: node.clone(),
            kind: node_kinds::MERGE.to_string(),
            label: format!("merge {}", truncate_ref(id, 32)),
            task_id: merge.task_id.clone(),
            agent_id: merge.agent_id.clone(),
            agent_name: merge.agent_name.clone(),
            role: Some("worker".to_string()),
            status: Some(merge.status.clone()),
            at_seq: merge.seq.max(1),
            at: merge.at.clone(),
            evidence: vec![DiagnosisEvidence {
                kind: "merge".to_string(),
                reference: truncate_ref(id, 120),
                hash: (!merge.commit.is_empty()).then(|| merge.commit.clone()),
                event_seq: Some(merge.seq),
                event_kind: Some("merge_queue.updated".to_string()),
                note: Some(format!("attempt {}", merge.attempt)),
            }],
            ..ProvenanceNode::default()
        });
        if !merge.commit.is_empty() {
            builder.add_edge(
                node.clone(),
                node_id(node_kinds::COMMIT, &merge.commit),
                edge_kinds::MERGED,
                merge.seq,
            );
        }
        if let Some(task_id) = &merge.task_id {
            builder.add_edge(
                node.clone(),
                node_id(node_kinds::TASK, task_id),
                edge_kinds::MERGED,
                merge.seq,
            );
        }
    }
    for (id, contract) in &scan.contracts {
        let node = node_id(node_kinds::CONTRACT, id);
        builder.add_node(ProvenanceNode {
            id: node.clone(),
            kind: node_kinds::CONTRACT.to_string(),
            label: format!("contract {}", truncate_ref(id, 32)),
            task_id: contract.task_id.clone(),
            role: contract.role.clone(),
            status: None,
            at_seq: contract.seq.max(1),
            at: contract.at.clone(),
            evidence: vec![DiagnosisEvidence {
                kind: "contract_hash".to_string(),
                reference: truncate_ref(id, 120),
                hash: (!contract.hash.is_empty()).then(|| contract.hash.clone()),
                event_seq: Some(contract.seq),
                event_kind: Some("contract.derived".to_string()),
                note: None,
            }],
            ..ProvenanceNode::default()
        });
        if let Some(task_id) = &contract.task_id {
            builder.add_edge(
                node_id(node_kinds::TASK, task_id),
                node,
                edge_kinds::USED,
                contract.seq,
            );
        }
    }
    for (id, effect) in &scan.effects {
        let node = node_id(node_kinds::EFFECT, id);
        builder.add_node(ProvenanceNode {
            id: node.clone(),
            kind: node_kinds::EFFECT.to_string(),
            label: format!("effect {}", truncate_ref(&effect.tool, 32)),
            task_id: effect.task_id.clone(),
            status: Some(effect.state.clone()),
            at_seq: effect.seq.max(1),
            at: effect.at.clone(),
            evidence: vec![DiagnosisEvidence {
                kind: "effect".to_string(),
                reference: truncate_ref(id, 120),
                hash: effect.evidence_hash.clone(),
                event_seq: Some(effect.seq),
                event_kind: Some("effect.receipt".to_string()),
                note: Some(format!("state {}", effect.state)),
            }],
            ..ProvenanceNode::default()
        });
        if let Some(task_id) = &effect.task_id {
            builder.add_edge(
                node_id(node_kinds::TASK, task_id),
                node,
                edge_kinds::PRODUCED,
                effect.seq,
            );
        }
    }
    for (id, workspace) in &scan.workspaces {
        let node = node_id(node_kinds::WORKSPACE, id);
        builder.add_node(ProvenanceNode {
            id: node.clone(),
            kind: node_kinds::WORKSPACE.to_string(),
            label: format!("workspace {}", truncate_ref(id, 32)),
            status: Some(workspace.status.clone()),
            at_seq: workspace.seq.max(1),
            at: workspace.at.clone(),
            evidence: vec![DiagnosisEvidence {
                kind: "workspace".to_string(),
                reference: truncate_ref(id, 120),
                hash: workspace.commit.clone(),
                event_seq: Some(workspace.seq),
                event_kind: Some("agent.workspace".to_string()),
                note: None,
            }],
            ..ProvenanceNode::default()
        });
        if let Some(commit) = &workspace.commit {
            builder.add_edge(
                node,
                node_id(node_kinds::COMMIT, commit),
                edge_kinds::PRODUCED,
                workspace.seq,
            );
        }
    }
    for (id, integration) in &scan.integrations {
        let node = node_id(node_kinds::WORKSPACE, id);
        builder.add_node(ProvenanceNode {
            id: node.clone(),
            kind: node_kinds::WORKSPACE.to_string(),
            label: "remote integration".to_string(),
            task_id: integration.task_id.clone(),
            status: Some(integration.status.clone()),
            at_seq: integration.seq.max(1),
            at: integration.at.clone(),
            evidence: vec![DiagnosisEvidence {
                kind: "remote_integration".to_string(),
                reference: truncate_ref(id, 120),
                hash: integration.bundle_sha256.clone(),
                event_seq: Some(integration.seq),
                event_kind: Some("remote.integration".to_string()),
                note: integration
                    .host
                    .as_deref()
                    .map(|host| format!("host {}", truncate_ref(host, 80)))
                    .or_else(|| integration.branch.clone()),
            }],
            ..ProvenanceNode::default()
        });
        if let Some(task_id) = &integration.task_id {
            builder.add_edge(
                node_id(node_kinds::TASK, task_id),
                node,
                edge_kinds::USED,
                integration.seq,
            );
        }
    }
    for (task_id, task) in &scan.tasks {
        for dep in &task.deps {
            let Some(_upstream) = scan.tasks.get(dep) else {
                continue;
            };
            for cited in &task.cited_hashes {
                for commit in scan
                    .commits
                    .values()
                    .filter(|commit| commit.task_id.as_deref() == Some(dep.as_str()))
                {
                    if hash_matches(cited, &commit.hash) {
                        builder.add_edge(
                            node_id(node_kinds::TASK, task_id),
                            node_id(node_kinds::COMMIT, &commit.hash),
                            edge_kinds::USED,
                            task.result_seq.unwrap_or(task.status_seq),
                        );
                    }
                }
                for artifact in scan
                    .artifacts
                    .values()
                    .filter(|artifact| artifact.task_id.as_deref() == Some(dep.as_str()))
                {
                    if !artifact.sha256.is_empty() && hash_matches(cited, &artifact.sha256) {
                        builder.add_edge(
                            node_id(node_kinds::TASK, task_id),
                            node_id(node_kinds::ARTIFACT, &artifact.id),
                            edge_kinds::USED,
                            task.result_seq.unwrap_or(task.status_seq),
                        );
                    }
                }
            }
        }
        for artifact_id in &task.read_artifact_ids {
            if scan.artifacts.contains_key(artifact_id) {
                builder.add_edge(
                    node_id(node_kinds::TASK, task_id),
                    node_id(node_kinds::ARTIFACT, artifact_id),
                    edge_kinds::USED,
                    task.status_seq,
                );
            }
        }
    }
    for symptom in &scan.symptoms {
        let node = node_id(node_kinds::FAILURE, &symptom.id);
        builder.add_node(ProvenanceNode {
            id: node.clone(),
            kind: node_kinds::FAILURE.to_string(),
            label: format!("failure {}", truncate_ref(&symptom.reason_code, 48)),
            task_id: symptom.task_id.clone(),
            agent_id: symptom.agent.as_ref().and_then(|agent| agent.id.clone()),
            agent_name: symptom.agent.as_ref().and_then(|agent| agent.name.clone()),
            role: symptom.agent.as_ref().and_then(|agent| agent.role.clone()),
            identity_id: symptom
                .agent
                .as_ref()
                .and_then(|agent| agent.identity_id.clone()),
            status: Some(symptom.status.clone()),
            at_seq: symptom.seq.max(1),
            at: symptom.at.clone(),
            evidence: symptom.evidence.clone(),
        });
        if let Some(task_id) = &symptom.task_id {
            builder.add_edge(
                node,
                node_id(node_kinds::TASK, task_id),
                edge_kinds::FAILED,
                symptom.seq,
            );
        }
    }
    let mut graph = builder.finish(session_id);
    graph.roots = scan
        .tasks
        .values()
        .filter(|task| task.deps.is_empty())
        .map(|task| node_id(node_kinds::TASK, &task.id))
        .collect();
    graph.roots.sort();
    graph
}

#[derive(Default)]
struct GraphBuilder {
    nodes: BTreeMap<String, ProvenanceNode>,
    edges: BTreeMap<(String, String, String), ProvenanceEdge>,
    truncated: bool,
}

impl GraphBuilder {
    fn add_node(&mut self, mut node: ProvenanceNode) {
        if node.id.is_empty() {
            return;
        }
        if let Some(existing) = self.nodes.get_mut(&node.id) {
            if node.at_seq > 0 && (existing.at_seq == 0 || node.at_seq < existing.at_seq) {
                existing.at_seq = node.at_seq;
                existing.at = node.at.clone();
            }
            if node.status.is_some() {
                existing.status = node.status;
            }
            for evidence in node.evidence {
                push_evidence(&mut existing.evidence, evidence);
            }
            return;
        }
        if self.nodes.len() >= MAX_GRAPH_NODES {
            self.truncated = true;
            return;
        }
        node.evidence.truncate(MAX_EVIDENCE);
        self.nodes.insert(node.id.clone(), node);
    }

    fn add_edge(&mut self, from: String, to: String, kind: &str, at_seq: u64) {
        if from.is_empty() || to.is_empty() || from == to {
            return;
        }
        if !self.nodes.contains_key(&from) || !self.nodes.contains_key(&to) {
            return;
        }
        let key = (from.clone(), to.clone(), kind.to_string());
        if self.edges.contains_key(&key) {
            return;
        }
        if self.edges.len() >= MAX_GRAPH_EDGES {
            self.truncated = true;
            return;
        }
        self.edges.insert(
            key,
            ProvenanceEdge {
                from,
                to,
                kind: kind.to_string(),
                at_seq,
            },
        );
    }

    fn finish(self, session_id: &str) -> ProvenanceGraph {
        let mut nodes = self.nodes.into_values().collect::<Vec<_>>();
        for node in &mut nodes {
            node.evidence.sort_by(|left, right| {
                left.event_seq
                    .cmp(&right.event_seq)
                    .then_with(|| left.kind.cmp(&right.kind))
                    .then_with(|| left.reference.cmp(&right.reference))
                    .then_with(|| left.hash.cmp(&right.hash))
            });
            node.evidence.dedup();
            node.evidence.truncate(MAX_EVIDENCE);
        }
        nodes.sort_by(|left, right| {
            left.at_seq
                .cmp(&right.at_seq)
                .then_with(|| left.kind.cmp(&right.kind))
                .then_with(|| left.id.cmp(&right.id))
        });
        let mut edges = self.edges.into_values().collect::<Vec<_>>();
        edges.sort_by(|left, right| {
            left.at_seq
                .cmp(&right.at_seq)
                .then_with(|| left.from.cmp(&right.from))
                .then_with(|| left.to.cmp(&right.to))
                .then_with(|| left.kind.cmp(&right.kind))
        });
        ProvenanceGraph {
            session_id: session_id.to_string(),
            roots: Vec::new(),
            nodes,
            edges,
            truncated: self.truncated,
        }
    }
}

fn summarize_graph(
    session_id: &str,
    graph: &mut ProvenanceGraph,
    input: &DiagnosisInput,
    report_truncated: bool,
    depth_limited_before_prune: bool,
) -> ProvenanceSummary {
    let mut summary = ProvenanceSummary {
        nodes: graph.nodes.len() as u32,
        edges: graph.edges.len() as u32,
        scanned_events: input.events.len() as u32,
        total_events: input.total_events,
        window_start_seq: input.events.first().map(|event| event.seq).unwrap_or(0),
        window_end_seq: input.events.last().map(|event| event.seq).unwrap_or(0),
        truncated: graph.truncated || report_truncated,
        ..ProvenanceSummary::default()
    };
    for node in &graph.nodes {
        match node.kind.as_str() {
            node_kinds::TASK => summary.tasks += 1,
            node_kinds::AGENT => summary.agents += 1,
            node_kinds::ARTIFACT => summary.artifacts += 1,
            node_kinds::COMMIT => summary.commits += 1,
            node_kinds::TOOL_CALL => summary.tool_calls += 1,
            node_kinds::TEST_EVIDENCE => summary.test_evidence += 1,
            node_kinds::VERIFICATION => summary.verifications += 1,
            node_kinds::MERGE => summary.merges += 1,
            _ => {}
        }
    }
    let (depth, limited) = dependency_depth(graph);
    summary.max_depth = depth;
    summary.depth_limited = limited || depth_limited_before_prune;
    summary.digest = graph_digest(session_id, graph);
    summary
}

fn dependency_depth(graph: &ProvenanceGraph) -> (u32, bool) {
    let deps = graph
        .edges
        .iter()
        .filter(|edge| edge.kind == edge_kinds::DEPENDS_ON)
        .fold(HashMap::<String, Vec<String>>::new(), |mut map, edge| {
            map.entry(edge.from.clone())
                .or_default()
                .push(edge.to.clone());
            map
        });
    let tasks = graph
        .nodes
        .iter()
        .filter(|node| node.kind == node_kinds::TASK)
        .map(|node| node.id.clone())
        .collect::<Vec<_>>();
    let mut depths = tasks
        .iter()
        .map(|task| (task.clone(), 0u32))
        .collect::<HashMap<_, _>>();
    for _ in 0..(MAX_GRAPH_DEPTH.saturating_add(2)) {
        let mut changed = false;
        for task in &tasks {
            let mut depth = 0u32;
            if let Some(upstream) = deps.get(task) {
                for dep in upstream {
                    let candidate = depths.get(dep).copied().unwrap_or(0).saturating_add(1);
                    depth = depth.max(candidate);
                }
            }
            if depth > MAX_GRAPH_DEPTH {
                return (MAX_GRAPH_DEPTH, true);
            }
            if depths.get(task).copied().unwrap_or(0) != depth {
                depths.insert(task.clone(), depth);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let max = depths
        .values()
        .copied()
        .max()
        .unwrap_or(0)
        .min(MAX_GRAPH_DEPTH);
    (max, depths.values().any(|depth| *depth > MAX_GRAPH_DEPTH))
}

fn prune_graph_depth(graph: &mut ProvenanceGraph, target_task: &str, max_depth: u32) -> bool {
    let target = node_id(node_kinds::TASK, target_task);
    if !graph.nodes.iter().any(|node| node.id == target) {
        return false;
    }
    let deps = graph
        .edges
        .iter()
        .filter(|edge| edge.kind == edge_kinds::DEPENDS_ON)
        .fold(HashMap::<String, Vec<String>>::new(), |mut map, edge| {
            map.entry(edge.from.clone())
                .or_default()
                .push(edge.to.clone());
            map
        });
    let mut allowed = HashSet::new();
    let mut exceeded = false;
    let mut queue = VecDeque::from([(target.clone(), 0u32)]);
    while let Some((task, depth)) = queue.pop_front() {
        if depth > max_depth {
            exceeded = true;
            continue;
        }
        if !allowed.insert(task.clone()) {
            continue;
        }
        if let Some(upstream) = deps.get(&task) {
            for dep in upstream {
                queue.push_back((dep.clone(), depth.saturating_add(1)));
            }
        }
    }
    if !exceeded {
        return false;
    }
    let before_nodes = graph.nodes.len();
    let before_edges = graph.edges.len();
    let allowed_tasks = allowed.clone();
    graph.nodes.retain(|node| {
        if node.kind == node_kinds::TASK {
            allowed_tasks.contains(&node.id)
                || node
                    .task_id
                    .as_deref()
                    .is_some_and(|task| allowed_tasks.contains(&node_id(node_kinds::TASK, task)))
        } else if let Some(task_id) = node.task_id.as_deref() {
            allowed_tasks.contains(&node_id(node_kinds::TASK, task_id))
        } else {
            true
        }
    });
    let remaining = graph
        .nodes
        .iter()
        .map(|node| node.id.clone())
        .collect::<HashSet<_>>();
    graph
        .edges
        .retain(|edge| remaining.contains(&edge.from) && remaining.contains(&edge.to));
    graph.roots.retain(|root| remaining.contains(root));
    let pruned = graph.nodes.len() != before_nodes || graph.edges.len() != before_edges;
    if pruned {
        graph.truncated = true;
    }
    pruned
}

fn graph_digest(session_id: &str, graph: &ProvenanceGraph) -> String {
    let mut nodes = graph.nodes.clone();
    nodes.sort_by(|left, right| left.id.cmp(&right.id));
    let edges = graph.edges.clone();
    let value = json!({
        "session_id": session_id,
        "nodes": nodes,
        "edges": edges,
    });
    let bytes = serde_json::to_vec(&value).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex(&hasher.finalize())
}

fn select_target(scan: &Scan, task_id: Option<&str>) -> Option<Symptom> {
    let mut candidates = scan
        .symptoms
        .iter()
        .filter(|symptom| task_id.is_none_or(|task| symptom.task_id.as_deref() == Some(task)))
        .cloned()
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        left.seq
            .cmp(&right.seq)
            .then_with(|| symptom_priority(&left.kind).cmp(&symptom_priority(&right.kind)))
            .then_with(|| left.id.cmp(&right.id))
    });
    candidates.pop()
}

fn symptom_priority(kind: &str) -> u8 {
    match kind {
        "merge_queue" => 5,
        "integration" => 4,
        "verification" => 3,
        "task" => 2,
        "agent" => 1,
        _ => 0,
    }
}

fn attribute(
    scan: &Scan,
    target: &DiagnosisTarget,
    symptom: &Symptom,
) -> (
    Vec<AttributionFinding>,
    Option<AttributionFinding>,
    Vec<CascadeStep>,
    Vec<AttributionAgent>,
) {
    let mut findings = Vec::new();
    let mut candidates = Vec::new();
    let target_task = target.task_id.clone();

    if let Some(task_id) = &target_task {
        let ancestors = ancestor_tasks(scan, task_id);
        for (ancestor_id, _depth) in &ancestors {
            let Some(ancestor) = scan.tasks.get(ancestor_id) else {
                continue;
            };
            if matches!(
                ancestor.status.as_str(),
                "failed" | "blocked" | "interrupted"
            ) && let Some(seq) = ancestor.failure_seq
            {
                let node = scan
                    .symptoms
                    .iter()
                    .find(|candidate| {
                        candidate.task_id.as_deref() == Some(ancestor_id)
                            && candidate.seq == seq
                            && candidate.kind == "task"
                    })
                    .map(|candidate| node_id(node_kinds::FAILURE, &candidate.id))
                    .unwrap_or_else(|| node_id(node_kinds::TASK, ancestor_id));
                let mut finding = finding(
                    &format!("cascade:{}:{}", ancestor_id, seq),
                    reason_codes::CASCADE_UPSTREAM,
                    "root",
                    &node,
                    Some(ancestor_id),
                    seq,
                    ancestor.failure_at.clone().unwrap_or_default(),
                    "an upstream task failed before the downstream result",
                    agent_list(ancestor.result_agent.as_ref(), "root"),
                    ancestor_evidence(scan, ancestor, "upstream failure"),
                    vec!["cascade".to_string()],
                );
                push_evidence(
                    &mut finding.evidence,
                    evidence_for_target(target, "downstream failure"),
                );
                push_unique_finding(&mut findings, &mut candidates, finding, 0);
            } else if ancestor.status == "completed"
                && ancestor.produced_hashes.is_empty()
                && ancestor.artifact_ids.is_empty()
            {
                let seq = ancestor.result_seq.unwrap_or(ancestor.status_seq);
                let mut finding = finding(
                    &format!("missing:upstream:{}:{}", ancestor_id, seq),
                    reason_codes::MISSING_EVIDENCE,
                    "root",
                    &node_id(node_kinds::TASK, ancestor_id),
                    Some(ancestor_id),
                    seq,
                    ancestor.result_at.clone().unwrap_or_default(),
                    "an upstream task result has no commit or artifact hash",
                    agent_list(ancestor.result_agent.as_ref(), "root"),
                    ancestor_evidence(scan, ancestor, "upstream result"),
                    vec!["missing_evidence".to_string(), "upstream".to_string()],
                );
                push_evidence(
                    &mut finding.evidence,
                    evidence_for_target(target, "downstream failure"),
                );
                push_unique_finding(&mut findings, &mut candidates, finding, 1);
            }
        }
    }

    if let Some(task_id) = &target_task
        && let Some(task) = scan.tasks.get(task_id)
    {
        if let Some(handoff) = handoff_finding(scan, task) {
            push_unique_finding(&mut findings, &mut candidates, handoff, 2);
        }
        if let Some(stale) = stale_finding(scan, task, target) {
            push_unique_finding(&mut findings, &mut candidates, stale, 3);
        }
        if let Some(false_consensus) = false_consensus_findings(scan, task, target) {
            for (index, finding) in false_consensus.into_iter().enumerate() {
                push_unique_finding(&mut findings, &mut candidates, finding, 4 + index as u8);
            }
        }
        if task.status == "completed" && task.produced_hashes.is_empty() {
            let seq = task.result_seq.unwrap_or(task.status_seq);
            let finding = finding(
                &format!("missing:result:{task_id}:{seq}"),
                reason_codes::MISSING_EVIDENCE,
                "contributing",
                &node_id(node_kinds::TASK, task_id),
                Some(task_id),
                seq,
                task.result_at
                    .clone()
                    .unwrap_or_else(|| task.status_at.clone()),
                "the task reports completion without a commit or artifact hash",
                agent_list(task.result_agent.as_ref(), "contributing"),
                ancestor_evidence(scan, task, "task result"),
                vec!["missing_evidence".to_string()],
            );
            push_unique_finding(&mut findings, &mut candidates, finding, 5);
        }
    }

    if candidates.is_empty() {
        let mut evidence = symptom.evidence.clone();
        if let Some(task_id) = symptom.task_id.as_deref()
            && let Some(task) = scan.tasks.get(task_id)
        {
            for item in task.evidence.iter().take(6) {
                if matches!(
                    item.kind.as_str(),
                    "contract" | "effect" | "intent_decision"
                ) {
                    push_evidence(&mut evidence, item.clone());
                }
            }
        }
        let agents = symptom
            .agent
            .as_ref()
            .map(|agent| vec![responsible(agent, "symptom")])
            .unwrap_or_default();
        let symptom_finding = finding(
            &format!("symptom:{}:{}", symptom.id, symptom.seq),
            &symptom.reason_code,
            "root",
            &node_id(node_kinds::FAILURE, &symptom.id),
            symptom.task_id.as_deref(),
            symptom.seq,
            symptom.at.clone(),
            "the recorded failure is the earliest supported failure in this window",
            agents,
            evidence,
            vec!["symptom".to_string()],
        );
        push_unique_finding(&mut findings, &mut candidates, symptom_finding, 9);
        let missing = finding(
            &format!("missing:window:{}:{}", symptom.id, symptom.seq),
            reason_codes::MISSING_EVIDENCE,
            "missing_evidence",
            &node_id(node_kinds::FAILURE, &symptom.id),
            symptom.task_id.as_deref(),
            symptom.seq,
            symptom.at.clone(),
            "no earlier unsupported claim is evidenced in the bounded window",
            Vec::new(),
            vec![evidence_for_target(target, "failure symptom")],
            vec!["missing_evidence".to_string()],
        );
        push_unique_finding(&mut findings, &mut candidates, missing, 10);
    }

    candidates.sort_by(|left, right| {
        left.1
            .at_seq
            .cmp(&right.1.at_seq)
            .then_with(|| left.0.cmp(&right.0))
            .then_with(|| left.1.finding_id.cmp(&right.1.finding_id))
    });
    let earliest = candidates
        .iter()
        .find(|(_, finding)| finding.severity == "root")
        .map(|(_, finding)| finding.clone())
        .or_else(|| candidates.first().map(|(_, finding)| finding.clone()));

    findings.sort_by(|left, right| {
        severity_rank(&left.severity)
            .cmp(&severity_rank(&right.severity))
            .then_with(|| left.at_seq.cmp(&right.at_seq))
            .then_with(|| left.finding_id.cmp(&right.finding_id))
    });
    findings.dedup_by(|left, right| left.finding_id == right.finding_id);

    let cascade = earliest
        .as_ref()
        .and_then(|finding| cascade_path(scan, finding, target, symptom))
        .unwrap_or_default();
    let responsible_agents = collect_responsible(&findings);
    (findings, earliest, cascade, responsible_agents)
}

fn push_unique_finding(
    findings: &mut Vec<AttributionFinding>,
    candidates: &mut Vec<(u8, AttributionFinding)>,
    finding: AttributionFinding,
    rank: u8,
) {
    if findings
        .iter()
        .any(|current| current.finding_id == finding.finding_id)
    {
        return;
    }
    candidates.push((rank, finding.clone()));
    findings.push(finding);
}

fn handoff_finding(scan: &Scan, task: &TaskState) -> Option<AttributionFinding> {
    if task.deps.is_empty() {
        return None;
    }
    let result_seq = task
        .result_seq
        .or(task.failure_seq)
        .unwrap_or(task.status_seq);
    let result_at = task
        .result_at
        .clone()
        .or(task.failure_at.clone())
        .unwrap_or_else(|| task.status_at.clone());
    let mut upstream_hashes = BTreeSet::new();
    let mut upstream_artifacts = BTreeSet::new();
    let mut has_upstream_evidence = false;
    let mut evidence = Vec::new();
    for dep in &task.deps {
        let Some(upstream) = scan.tasks.get(dep) else {
            evidence.push(DiagnosisEvidence {
                kind: "missing_task".to_string(),
                reference: dep.clone(),
                event_seq: None,
                event_kind: Some("plan_task".to_string()),
                note: Some("declared dependency has no task event".to_string()),
                ..DiagnosisEvidence::default()
            });
            continue;
        };
        if !upstream.produced_hashes.is_empty() || !upstream.artifact_ids.is_empty() {
            has_upstream_evidence = true;
        }
        upstream_hashes.extend(upstream.produced_hashes.clone());
        upstream_artifacts.extend(upstream.artifact_ids.clone());
        evidence.push(DiagnosisEvidence {
            kind: "dependency".to_string(),
            reference: dep.clone(),
            hash: upstream.produced_hashes.iter().next().cloned(),
            event_seq: upstream.result_seq.or(upstream.failure_seq),
            event_kind: Some("plan_task.finished".to_string()),
            note: Some(format!("upstream status {}", upstream.status)),
        });
    }
    if !has_upstream_evidence {
        return Some(finding(
            &format!("missing:handoff:{}:{}", task.id, result_seq),
            reason_codes::MISSING_EVIDENCE,
            "root",
            &node_id(node_kinds::TASK, &task.id),
            Some(&task.id),
            result_seq,
            result_at,
            "the task depends on upstream work that has no commit or artifact hash",
            agent_list(task.result_agent.as_ref(), "contributing"),
            evidence,
            vec!["missing_evidence".to_string(), "handoff".to_string()],
        ));
    }
    let cited_hashes = task
        .cited_hashes
        .iter()
        .filter(|hash| is_hex_hash(hash))
        .cloned()
        .collect::<Vec<_>>();
    let artifact_read = task
        .read_artifact_ids
        .iter()
        .any(|artifact| upstream_artifacts.contains(artifact));
    let hash_match = cited_hashes
        .iter()
        .any(|cited| upstream_hashes.iter().any(|hash| hash_matches(cited, hash)));
    if artifact_read || hash_match {
        return None;
    }
    let mut evidence_out = evidence;
    for item in task.evidence.iter().take(4) {
        if matches!(
            item.kind.as_str(),
            "contract" | "effect" | "intent_decision"
        ) {
            push_evidence(&mut evidence_out, item.clone());
        }
    }
    for hash in cited_hashes.iter().take(MAX_EVIDENCE) {
        evidence_out.push(DiagnosisEvidence {
            kind: "cited_hash".to_string(),
            reference: "worker_result".to_string(),
            hash: Some(truncate_ref(hash, 64)),
            event_seq: task.result_seq.or(task.failure_seq),
            event_kind: Some("plan_task.finished".to_string()),
            note: Some("hash did not match an upstream result".to_string()),
        });
    }
    if cited_hashes.is_empty() {
        evidence_out.push(DiagnosisEvidence {
            kind: "cited_hash".to_string(),
            reference: "worker_result".to_string(),
            hash: None,
            event_seq: task.result_seq.or(task.failure_seq),
            event_kind: Some("plan_task.finished".to_string()),
            note: Some("no hash citation present".to_string()),
        });
    }
    Some(finding(
        &format!("handoff:{}:{}", task.id, result_seq),
        reason_codes::UNSUBSTANTIATED_HANDOFF,
        "root",
        &node_id(node_kinds::TASK, &task.id),
        Some(&task.id),
        result_seq,
        result_at,
        "the Worker result does not cite an upstream commit or artifact hash it used",
        agent_list(task.result_agent.as_ref(), "root"),
        evidence_out,
        vec!["handoff".to_string(), "worker".to_string()],
    ))
}

fn stale_finding(
    scan: &Scan,
    task: &TaskState,
    target: &DiagnosisTarget,
) -> Option<AttributionFinding> {
    let mut commits = task.commits.clone();
    for merge in scan.merges.values().filter(|merge| {
        merge.task_id.as_deref() == Some(task.id.as_str()) && !merge.commit.is_empty()
    }) {
        if !commits.iter().any(|(_, hash)| hash == &merge.commit) {
            commits.push((merge.seq, merge.commit.clone()));
        }
    }
    commits.sort_by_key(|(seq, _)| *seq);
    commits.dedup_by(|left, right| left.1 == right.1);
    let verifier = scan
        .verifications
        .iter()
        .filter(|verification| verification.task_id.as_deref() == Some(task.id.as_str()))
        .max_by_key(|verification| verification.seq);
    let tests = scan
        .tests
        .iter()
        .filter(|test| test.task_id.as_deref() == Some(task.id.as_str()) && test.passed)
        .collect::<Vec<_>>();

    // A verdict or check that belongs to a previous commit is stale when a
    // newer commit for the same task exists and no later evidence covers it.
    if commits.len() >= 2 {
        let (previous_seq, previous_hash) = &commits[commits.len() - 2];
        let (latest_seq, latest_hash) = &commits[commits.len() - 1];
        let test_for_previous = tests
            .iter()
            .filter(|test| test.seq >= *previous_seq && test.seq < *latest_seq)
            .max_by_key(|test| test.seq);
        let test_after_latest = tests.iter().any(|test| test.seq > *latest_seq);
        let verifier_for_previous = verifier.as_ref().filter(|verification| {
            verification.seq >= *previous_seq && verification.seq < *latest_seq
        });
        let verifier_after_latest = verifier
            .as_ref()
            .is_some_and(|verification| verification.seq > *latest_seq);
        if (test_for_previous.is_some() && !test_after_latest)
            || (verifier_for_previous.is_some() && !verifier_after_latest)
        {
            let mut evidence = vec![
                DiagnosisEvidence {
                    kind: "commit".to_string(),
                    reference: truncate_ref(previous_hash, 12),
                    hash: Some(previous_hash.clone()),
                    event_seq: Some(*previous_seq),
                    event_kind: Some("commit".to_string()),
                    note: Some("commit covered by the earlier check".to_string()),
                },
                DiagnosisEvidence {
                    kind: "commit".to_string(),
                    reference: truncate_ref(latest_hash, 12),
                    hash: Some(latest_hash.clone()),
                    event_seq: Some(*latest_seq),
                    event_kind: Some("commit".to_string()),
                    note: Some("later commit without a covering check".to_string()),
                },
            ];
            if let Some(test) = test_for_previous {
                evidence.push(DiagnosisEvidence {
                    kind: "test".to_string(),
                    reference: "run_project_task".to_string(),
                    event_seq: Some(test.seq),
                    event_kind: Some("tool.finished".to_string()),
                    note: Some("check predates the latest commit".to_string()),
                    ..DiagnosisEvidence::default()
                });
            }
            if let Some(verification) = verifier_for_previous {
                evidence.push(DiagnosisEvidence {
                    kind: "verification".to_string(),
                    reference: truncate_ref(&verification.id, 120),
                    event_seq: Some(verification.seq),
                    event_kind: Some("agent.finished".to_string()),
                    note: Some("verdict predates the latest commit".to_string()),
                    ..DiagnosisEvidence::default()
                });
            }
            let agent = verifier_for_previous
                .map(|verification| verification.agent.clone())
                .or_else(|| test_for_previous.map(|test| test.agent.clone()))
                .or_else(|| task.result_agent.clone());
            return Some(finding(
                &format!("stale:{}:{}", task.id, latest_seq),
                reason_codes::STALE_EVIDENCE,
                "root",
                &node_id(node_kinds::COMMIT, latest_hash),
                Some(&task.id),
                *latest_seq,
                commit_at(scan, latest_hash).unwrap_or_else(|| target_at(scan, target)),
                "the latest commit has no test or Verifier evidence that covers it",
                agent_list(agent.as_ref(), "root"),
                evidence,
                vec!["stale".to_string()],
            ));
        }
    }
    if let Some(verification) = &verifier
        && let Some(latest) = commits.last()
        && let Some(verified_commit) = &verification.verified_commit
        && verified_commit != &latest.1
        && verification.seq < latest.0
    {
        return Some(finding(
            &format!("stale:verdict:{}:{}", task.id, latest.0),
            reason_codes::STALE_EVIDENCE,
            "root",
            &node_id(node_kinds::COMMIT, &latest.1),
            Some(&task.id),
            latest.0,
            commit_at(scan, &latest.1).unwrap_or_else(|| target_at(scan, target)),
            "the Verifier verdict refers to a different commit than the latest task commit",
            agent_list(Some(&verification.agent), "root"),
            vec![
                DiagnosisEvidence {
                    kind: "commit".to_string(),
                    reference: truncate_ref(&latest.1, 12),
                    hash: Some(latest.1.clone()),
                    event_seq: Some(latest.0),
                    event_kind: Some("commit".to_string()),
                    note: Some("latest task commit".to_string()),
                },
                DiagnosisEvidence {
                    kind: "verification".to_string(),
                    reference: truncate_ref(&verification.id, 120),
                    hash: Some(verified_commit.clone()),
                    event_seq: Some(verification.seq),
                    event_kind: Some("agent.finished".to_string()),
                    note: Some("verdict covered another commit".to_string()),
                },
            ],
            vec!["stale".to_string()],
        ));
    }
    None
}

fn false_consensus_findings(
    scan: &Scan,
    task: &TaskState,
    target: &DiagnosisTarget,
) -> Option<Vec<AttributionFinding>> {
    let merge_failure = target.kind == "merge_queue" || target.kind == "integration";
    let merge_for_task = scan.merges.values().find(|merge| {
        merge.task_id.as_deref() == Some(task.id.as_str())
            && matches!(merge.status.as_str(), "conflict" | "failed" | "interrupted")
    });
    if !merge_failure && merge_for_task.is_none() {
        return None;
    }
    let failure_seq = merge_for_task
        .map(|merge| merge.seq)
        .unwrap_or(target.at_seq);
    let verifier = scan
        .verifications
        .iter()
        .filter(|verification| {
            verification.task_id.as_deref() == Some(task.id.as_str())
                && verification.approved
                && verification.seq < failure_seq
        })
        .max_by_key(|verification| verification.seq)?;
    let test = scan
        .tests
        .iter()
        .filter(|test| {
            test.task_id.as_deref() == Some(task.id.as_str())
                && test.passed
                && test.seq < failure_seq
        })
        .max_by_key(|test| test.seq)?;
    let failure_evidence = merge_for_task
        .map(|merge| DiagnosisEvidence {
            kind: "merge".to_string(),
            reference: truncate_ref(&merge.id, 120),
            hash: (!merge.commit.is_empty()).then(|| merge.commit.clone()),
            event_seq: Some(merge.seq),
            event_kind: Some("merge_queue.updated".to_string()),
            note: Some(format!("status {}", merge.status)),
        })
        .unwrap_or_else(|| evidence_for_target(target, "integration failure"));
    let worker_finding = finding(
        &format!("false:worker:{}:{}", task.id, test.seq),
        reason_codes::FALSE_CONSENSUS,
        "root",
        &node_id(node_kinds::TEST_EVIDENCE, &test.id),
        Some(&task.id),
        test.seq,
        test.at.clone(),
        "the Worker project-task check and the Verifier verdict accepted work that integration later failed",
        agent_list(Some(&test.agent), "flag"),
        vec![
            DiagnosisEvidence {
                kind: "test".to_string(),
                reference: truncate_ref(&test.id, 120),
                event_seq: Some(test.seq),
                event_kind: Some("tool.finished".to_string()),
                note: Some("Worker check passed".to_string()),
                ..DiagnosisEvidence::default()
            },
            DiagnosisEvidence {
                kind: "verification".to_string(),
                reference: truncate_ref(&verifier.id, 120),
                event_seq: Some(verifier.seq),
                event_kind: Some("agent.finished".to_string()),
                note: Some("Verifier approved".to_string()),
                ..DiagnosisEvidence::default()
            },
            failure_evidence.clone(),
        ],
        vec!["worker".to_string(), "false_consensus".to_string()],
    );
    let verifier_finding = finding(
        &format!("false:verifier:{}:{}", task.id, verifier.seq),
        reason_codes::FALSE_CONSENSUS,
        "flag",
        &node_id(node_kinds::VERIFICATION, &verifier.id),
        Some(&task.id),
        verifier.seq,
        verifier.at.clone(),
        "the Worker project-task check and the Verifier verdict accepted work that integration later failed",
        agent_list(Some(&verifier.agent), "flag"),
        vec![
            DiagnosisEvidence {
                kind: "verification".to_string(),
                reference: truncate_ref(&verifier.id, 120),
                event_seq: Some(verifier.seq),
                event_kind: Some("agent.finished".to_string()),
                note: Some("Verifier approved".to_string()),
                ..DiagnosisEvidence::default()
            },
            DiagnosisEvidence {
                kind: "test".to_string(),
                reference: truncate_ref(&test.id, 120),
                event_seq: Some(test.seq),
                event_kind: Some("tool.finished".to_string()),
                note: Some("Worker check passed".to_string()),
                ..DiagnosisEvidence::default()
            },
            failure_evidence,
        ],
        vec!["verifier".to_string(), "false_consensus".to_string()],
    );
    Some(vec![worker_finding, verifier_finding])
}

fn cascade_path(
    scan: &Scan,
    earliest: &AttributionFinding,
    target: &DiagnosisTarget,
    symptom: &Symptom,
) -> Option<Vec<CascadeStep>> {
    let target_task = target.task_id.as_ref()?;
    let root_task = earliest
        .task_id
        .clone()
        .or_else(|| symptom.task_id.clone())?;
    if root_task == *target_task {
        return Some(vec![CascadeStep {
            node_id: earliest.node_id.clone(),
            kind: "task".to_string(),
            task_id: Some(root_task.clone()),
            status: scan
                .tasks
                .get(&root_task)
                .map(|task| task.status.clone())
                .or_else(|| Some(symptom.status.clone())),
            at_seq: earliest.at_seq,
            relation: "root".to_string(),
        }]);
    }
    let mut children = HashMap::<String, Vec<String>>::new();
    for task in scan.tasks.values() {
        for dep in &task.deps {
            children
                .entry(dep.clone())
                .or_default()
                .push(task.id.clone());
        }
    }
    for values in children.values_mut() {
        values.sort();
        values.dedup();
    }
    let mut queue = VecDeque::from([root_task.clone()]);
    let mut previous = HashMap::new();
    let mut found = false;
    while let Some(task) = queue.pop_front() {
        if task == *target_task {
            found = true;
            break;
        }
        if previous.len() > 512 {
            break;
        }
        if let Some(next) = children.get(&task) {
            for child in next {
                if previous.contains_key(child) || *child == root_task {
                    continue;
                }
                previous.insert(child.clone(), task.clone());
                queue.push_back(child.clone());
            }
        }
    }
    if !found {
        return None;
    }
    let mut path = vec![target_task.clone()];
    let mut cursor = target_task.clone();
    while let Some(parent) = previous.get(&cursor) {
        path.push(parent.clone());
        cursor = parent.clone();
    }
    path.reverse();
    let mut steps = Vec::new();
    for (index, task_id) in path.iter().enumerate() {
        let task = scan.tasks.get(task_id);
        let (seq, relation) = if index == 0 {
            (earliest.at_seq, "root")
        } else if index + 1 == path.len() {
            (symptom.seq, "failure")
        } else {
            (
                task.and_then(|task| task.result_seq.or(task.failure_seq))
                    .unwrap_or_else(|| task.map(|task| task.status_seq).unwrap_or(0)),
                "task_dependency",
            )
        };
        steps.push(CascadeStep {
            node_id: node_id(node_kinds::TASK, task_id),
            kind: "task".to_string(),
            task_id: Some(task_id.clone()),
            status: task.map(|task| task.status.clone()),
            at_seq: seq,
            relation: relation.to_string(),
        });
    }
    Some(steps)
}

fn collect_responsible(findings: &[AttributionFinding]) -> Vec<AttributionAgent> {
    let mut seen = HashSet::new();
    let mut agents = Vec::new();
    for finding in findings {
        for agent in &finding.agents {
            let key = format!(
                "{}:{}:{}",
                agent.agent_id.as_deref().unwrap_or(""),
                agent.agent_name.as_deref().unwrap_or(""),
                agent.role.as_deref().unwrap_or("")
            );
            if seen.insert(key) {
                agents.push(agent.clone());
            }
            if agents.len() >= MAX_RESPONSIBLE_AGENTS {
                return agents;
            }
        }
    }
    agents
}

fn ancestor_tasks(scan: &Scan, target: &str) -> Vec<(String, u32)> {
    let mut seen = HashSet::new();
    let mut queue = VecDeque::from([(target.to_string(), 0u32)]);
    let mut out = Vec::new();
    while let Some((task_id, depth)) = queue.pop_front() {
        if depth > MAX_GRAPH_DEPTH || !seen.insert(task_id.clone()) {
            continue;
        }
        let Some(task) = scan.tasks.get(&task_id) else {
            continue;
        };
        if task_id != target {
            out.push((task_id.clone(), depth));
        }
        for dep in &task.deps {
            queue.push_back((dep.clone(), depth.saturating_add(1)));
        }
    }
    out
}

fn ancestor_evidence(scan: &Scan, task: &TaskState, note: &str) -> Vec<DiagnosisEvidence> {
    let mut evidence = vec![DiagnosisEvidence {
        kind: "task".to_string(),
        reference: task.id.clone(),
        event_seq: task
            .result_seq
            .or(task.failure_seq)
            .or(Some(task.status_seq)),
        event_kind: Some("plan_task.finished".to_string()),
        note: Some(note.to_string()),
        ..DiagnosisEvidence::default()
    }];
    for hash in task.produced_hashes.iter().take(4) {
        evidence.push(DiagnosisEvidence {
            kind: "hash".to_string(),
            reference: truncate_ref(hash, 12),
            hash: Some(hash.clone()),
            event_seq: task.result_seq.or(task.failure_seq),
            event_kind: Some("plan_task.finished".to_string()),
            note: Some("task result evidence".to_string()),
        });
    }
    for artifact in task.artifact_ids.iter().take(4) {
        evidence.push(DiagnosisEvidence {
            kind: "artifact".to_string(),
            reference: truncate_ref(artifact, 120),
            hash: scan
                .artifacts
                .get(artifact)
                .map(|artifact| artifact.sha256.clone())
                .filter(|value| !value.is_empty()),
            event_seq: task.result_seq.or(task.failure_seq),
            event_kind: Some("artifact.published".to_string()),
            note: Some("task artifact".to_string()),
        });
    }
    if let Some(intent) = scan.intent_evidence.get(&task.id) {
        for item in intent.iter().take(2) {
            push_evidence(&mut evidence, item.clone());
        }
    }
    for item in task.evidence.iter().take(4) {
        if matches!(
            item.kind.as_str(),
            "contract" | "effect" | "intent_decision"
        ) {
            push_evidence(&mut evidence, item.clone());
        }
    }
    evidence
}

#[allow(clippy::too_many_arguments)]
fn finding(
    finding_id: &str,
    reason_code: &str,
    severity: &str,
    node_id: &str,
    task_id: Option<&str>,
    at_seq: u64,
    at: String,
    summary: &str,
    agents: Vec<AttributionAgent>,
    evidence: Vec<DiagnosisEvidence>,
    flags: Vec<String>,
) -> AttributionFinding {
    let mut bounded = evidence;
    bounded.truncate(MAX_EVIDENCE);
    AttributionFinding {
        finding_id: finding_id.to_string(),
        reason_code: reason_code.to_string(),
        severity: severity.to_string(),
        node_id: node_id.to_string(),
        task_id: task_id.map(str::to_string),
        at_seq,
        at,
        summary: summary.to_string(),
        agents,
        evidence: bounded,
        flags,
    }
}

fn agent_list(agent: Option<&AgentRef>, responsibility: &str) -> Vec<AttributionAgent> {
    agent
        .map(|agent| vec![responsible(agent, responsibility)])
        .unwrap_or_default()
}

fn responsible(agent: &AgentRef, responsibility: &str) -> AttributionAgent {
    AttributionAgent {
        agent_id: agent.id.clone(),
        agent_name: agent.name.clone(),
        role: agent.role.clone(),
        task_id: agent.task_id.clone(),
        identity_id: agent.identity_id.clone(),
        responsibility: responsibility.to_string(),
    }
}

fn evidence_for_event(event: &Event, note: &str) -> DiagnosisEvidence {
    DiagnosisEvidence {
        kind: "event".to_string(),
        reference: truncate_ref(&event.kind, 120),
        hash: None,
        event_seq: Some(event.seq),
        event_kind: Some(event.kind.clone()),
        note: Some(note.to_string()),
    }
}

fn evidence_for_target(target: &DiagnosisTarget, note: &str) -> DiagnosisEvidence {
    DiagnosisEvidence {
        kind: "target".to_string(),
        reference: truncate_ref(&target.id, 120),
        hash: None,
        event_seq: Some(target.at_seq),
        event_kind: Some(target.kind.clone()),
        note: Some(note.to_string()),
    }
}

fn commit_at(scan: &Scan, hash: &str) -> Option<String> {
    scan.commits.get(hash).map(|commit| commit.at.clone())
}

fn target_at(scan: &Scan, target: &DiagnosisTarget) -> String {
    target
        .task_id
        .as_deref()
        .and_then(|task| scan.tasks.get(task))
        .map(|task| task.status_at.clone())
        .unwrap_or_default()
}

fn other_failure_count(scan: &Scan, target: Option<&DiagnosisTarget>) -> u32 {
    let mut seen = HashSet::new();
    for symptom in &scan.symptoms {
        if target.is_some_and(|target| {
            target.id == symptom.id
                || (target.task_id.is_some() && target.task_id == symptom.task_id)
        }) {
            continue;
        }
        seen.insert(symptom.id.clone());
    }
    seen.len().min(999) as u32
}

fn severity_rank(severity: &str) -> u8 {
    match severity {
        "root" => 0,
        "flag" => 1,
        "contributing" => 2,
        "missing_evidence" => 3,
        _ => 4,
    }
}

fn push_evidence(list: &mut Vec<DiagnosisEvidence>, evidence: DiagnosisEvidence) {
    if list.len() >= MAX_EVIDENCE {
        return;
    }
    if !list.iter().any(|current| {
        current.kind == evidence.kind
            && current.reference == evidence.reference
            && current.hash == evidence.hash
            && current.event_seq == evidence.event_seq
    }) {
        list.push(evidence);
    }
}

fn agent_ref(payload: &Value) -> AgentRef {
    AgentRef {
        id: text(payload, "agent_id").map(str::to_string),
        name: text(payload, "agent_name").map(str::to_string),
        role: text(payload, "agent_role")
            .or_else(|| text(payload, "role"))
            .map(str::to_string),
        task_id: text(payload, "task_id").map(str::to_string),
        identity_id: text(payload, "identity_id").map(str::to_string),
    }
}

fn agent_key(agent: &AgentRef) -> Option<String> {
    agent
        .id
        .clone()
        .or_else(|| agent.name.clone())
        .filter(|value| !value.is_empty())
}

fn artifact_ids_from_input(payload: &Value) -> Vec<String> {
    let Some(input) = payload.get("input") else {
        return Vec::new();
    };
    let mut ids = Vec::new();
    if let Some(id) = input.get("artifact_id").and_then(Value::as_str) {
        ids.push(id.to_string());
    }
    if let Some(values) = input.get("artifact_ids").and_then(Value::as_array) {
        for value in values.iter().filter_map(Value::as_str).take(16) {
            ids.push(value.to_string());
        }
    }
    ids
}

fn string_array(payload: &Value, key: &str, limit: usize) -> Vec<String> {
    payload
        .get(key)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .take(limit)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn text<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn node_id(kind: &str, value: &str) -> String {
    let value = truncate_ref(value.trim(), 160);
    format!("{kind}:{value}")
}

fn truncate_ref(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.to_string()
    } else {
        value.chars().take(max).collect()
    }
}

fn is_hex_hash(value: &str) -> bool {
    let value = value.trim();
    (7..=64).contains(&value.len()) && value.chars().all(|ch| ch.is_ascii_hexdigit())
}

/// Extract bounded hexadecimal hash tokens without retaining surrounding
/// prose. This is the only content-derived operation in the module: it copies
/// hash-shaped tokens, never summaries, commands, or output.
fn hash_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in text.chars().take(16_000) {
        if ch.is_ascii_hexdigit() {
            current.push(ch.to_ascii_lowercase());
        } else {
            if (12..=64).contains(&current.len()) && !tokens.contains(&current) {
                tokens.push(current.clone());
            }
            current.clear();
            if tokens.len() >= 32 {
                return tokens;
            }
        }
    }
    if (12..=64).contains(&current.len()) && !tokens.contains(&current) {
        tokens.push(current);
    }
    tokens
}

fn hash_matches(cited: &str, evidence: &str) -> bool {
    let cited = cited.trim().to_ascii_lowercase();
    let evidence = evidence.trim().to_ascii_lowercase();
    cited.len() >= 12 && (evidence.starts_with(&cited) || cited.starts_with(&evidence))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use vp_protocol::diagnosis::{SessionDiagnosisResult, reason_codes};

    const C1: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const C2: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const C3: &str = "cccccccccccccccccccccccccccccccccccccccc";
    const ART1: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

    fn at(seq: u64) -> String {
        format!("2026-11-13T00:{:02}:{:02}Z", (seq / 60) % 60, seq % 60)
    }

    fn event(seq: u64, kind: &str, payload: Value) -> Event {
        Event {
            session_id: "ses-1".into(),
            seq,
            kind: kind.into(),
            payload,
            created_at: at(seq),
        }
    }

    fn plan(seq: u64, tasks: &[(&str, &[&str])]) -> Event {
        let tasks = tasks
            .iter()
            .map(|(id, deps)| {
                json!({
                    "id": id,
                    "department": "engineering",
                    "title": format!("task {id}"),
                    "kind": "coding",
                    "deps": deps,
                    "write_scope": [],
                    "acceptance": [],
                })
            })
            .collect::<Vec<_>>();
        event(
            seq,
            "team_plan.submitted",
            json!({"goal": "fixture", "departments": [], "tasks": tasks}),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn agent_finish(
        seq: u64,
        id: &str,
        name: &str,
        role: &str,
        task: Option<&str>,
        status: &str,
        commit: Option<&str>,
        approved: Option<bool>,
        merged: bool,
        verified: bool,
        summary: &str,
        changed_paths: &[&str],
    ) -> Event {
        let mut payload = json!({
            "status": status,
            "summary": summary,
            "merged": merged,
            "verified": verified,
            "agent_id": id,
            "agent_name": name,
            "agent_role": role,
            "changed_paths": changed_paths,
        });
        if let Some(commit) = commit {
            payload["commit"] = json!(commit);
        }
        if let Some(approved) = approved {
            payload["approved"] = json!(approved);
        }
        if role == "verifier" {
            payload["verified_worker"] = json!("Worker 1");
        }
        if let Some(task) = task {
            payload["task_id"] = json!(task);
        }
        event(seq, "agent.finished", payload)
    }

    fn task_finish(seq: u64, task: &str, status: &str, summary: &str) -> Event {
        event(
            seq,
            "plan_task.finished",
            json!({
                "task_id": task,
                "task_status": status,
                "summary": summary,
                "agent_id": format!("worker-{task}"),
                "agent_name": format!("Worker {task}"),
                "agent_role": "worker",
            }),
        )
    }

    fn task_start(seq: u64, task: &str) -> Event {
        event(
            seq,
            "plan_task.started",
            json!({
                "task_id": task,
                "agent_id": format!("worker-{task}"),
                "agent_name": format!("Worker {task}"),
                "agent_role": "worker",
            }),
        )
    }

    fn check(seq: u64, task: &str, passed: bool) -> Event {
        event(
            seq,
            "tool.finished",
            json!({
                "tool_call_id": format!("tool-{task}-{seq}"),
                "name": "run_project_task",
                "task_id": task,
                "agent_id": format!("worker-{task}"),
                "agent_name": format!("Worker {task}"),
                "agent_role": "worker",
                "is_error": !passed,
            }),
        )
    }

    fn merge(seq: u64, id: &str, task: &str, commit: &str, status: &str, agent: &str) -> Event {
        event(
            seq,
            "merge_queue.updated",
            json!({
                "entry": {
                    "id": id,
                    "session_id": "ses-1",
                    "agent_id": format!("worker-{task}"),
                    "agent_name": agent,
                    "task_id": task,
                    "branch": format!("task/{task}"),
                    "commit": commit,
                    "base_commit": C1,
                    "status": status,
                    "attempt": 1,
                    "source": "local",
                }
            }),
        )
    }

    fn artifact(seq: u64, id: &str, task: &str, sha256: &str) -> Event {
        event(
            seq,
            "artifact.published",
            json!({
                "artifact_id": id,
                "task_id": task,
                "agent_id": format!("worker-{task}"),
                "agent_name": format!("Worker {task}"),
                "agent_role": "worker",
                "sha256": sha256,
                "mime_type": "text/plain",
                "version": 1,
            }),
        )
    }

    fn input(events: Vec<Event>, target: Option<&str>, include_graph: bool) -> DiagnosisInput {
        let total = events.len() as u64;
        let mut input = DiagnosisInput {
            session_id: "ses-1".into(),
            events,
            total_events: total,
            target_task_id: target.map(str::to_string),
            include_graph,
            assignments: Vec::new(),
        };
        input.total_events = input.events.len() as u64;
        input
    }

    fn report_for(events: Vec<Event>, target: Option<&str>) -> SessionDiagnosisResult {
        diagnose(&input(events, target, true))
    }

    #[test]
    fn upstream_failure_is_blamed_before_the_downstream_failure() {
        let events = vec![
            plan(1, &[("t1", &[]), ("t2", &["t1"]), ("t3", &[])]),
            task_start(2, "t1"),
            task_finish(3, "t1", "failed", "upstream failed"),
            task_start(4, "t2"),
            task_finish(5, "t2", "failed", "downstream failed"),
            task_start(6, "t3"),
            task_finish(7, "t3", "failed", "sibling failed"),
        ];
        let report = report_for(events, Some("t2"));
        let earliest = report.earliest.expect("root finding");
        assert_eq!(earliest.reason_code, reason_codes::CASCADE_UPSTREAM);
        assert_eq!(earliest.task_id.as_deref(), Some("t1"));
        assert_eq!(report.cascade.len(), 2);
        assert_eq!(report.cascade[0].task_id.as_deref(), Some("t1"));
        assert_eq!(report.cascade[1].task_id.as_deref(), Some("t2"));
        assert!(
            !report
                .cascade
                .iter()
                .any(|step| step.task_id.as_deref() == Some("t3")),
            "a sibling failure must not be copied into the cascade path"
        );
        assert_eq!(report.other_failures, 2);
    }

    #[test]
    fn missing_citation_is_an_unsubstantiated_handoff() {
        let events = vec![
            plan(1, &[("t1", &[]), ("t2", &["t1"])]),
            task_start(2, "t1"),
            agent_finish(
                3,
                "worker-t1",
                "Worker t1",
                "worker",
                Some("t1"),
                "idle",
                Some(C1),
                None,
                true,
                true,
                "produced work",
                &["src/a.rs"],
            ),
            task_finish(4, "t1", "completed", "t1 result"),
            task_start(5, "t2"),
            agent_finish(
                6,
                "worker-t2",
                "Worker t2",
                "worker",
                Some("t2"),
                "idle",
                Some(C2),
                None,
                false,
                false,
                "used deadbeefdeadbeef but not the upstream hash",
                &["src/b.rs"],
            ),
            task_finish(7, "t2", "completed", "t2 used deadbeefdeadbeef"),
            merge(8, "mq-1", "t2", C2, "failed", "Worker t2"),
            task_finish(9, "t2", "failed", "integration failed"),
        ];
        let report = report_for(events, Some("t2"));
        let earliest = report.earliest.expect("root finding");
        assert_eq!(earliest.reason_code, reason_codes::UNSUBSTANTIATED_HANDOFF);
        assert_eq!(earliest.task_id.as_deref(), Some("t2"));
        assert!(earliest.flags.contains(&"worker".to_string()));
        assert!(report.findings.iter().any(|finding| {
            finding
                .evidence
                .iter()
                .any(|evidence| evidence.hash.as_deref() == Some(C1))
        }));
    }

    #[test]
    fn matching_upstream_hash_substantiates_the_handoff() {
        let cited = format!("used {C1}");
        let events = vec![
            plan(1, &[("t1", &[]), ("t2", &["t1"])]),
            task_start(2, "t1"),
            agent_finish(
                3,
                "worker-t1",
                "Worker t1",
                "worker",
                Some("t1"),
                "idle",
                Some(C1),
                None,
                true,
                true,
                "produced work",
                &[],
            ),
            task_finish(4, "t1", "completed", "t1 result"),
            task_start(5, "t2"),
            agent_finish(
                6,
                "worker-t2",
                "Worker t2",
                "worker",
                Some("t2"),
                "idle",
                Some(C2),
                None,
                false,
                false,
                &cited,
                &[],
            ),
            task_finish(7, "t2", "completed", &cited),
            merge(8, "mq-1", "t2", C2, "failed", "Worker t2"),
            task_finish(9, "t2", "failed", "integration failed"),
        ];
        let report = report_for(events, Some("t2"));
        assert!(
            !report
                .findings
                .iter()
                .any(|finding| finding.reason_code == reason_codes::UNSUBSTANTIATED_HANDOFF),
            "a matching cited hash is evidence, not a finding"
        );
        let graph = report.graph.expect("graph");
        assert!(graph.edges.iter().any(|edge| {
            edge.kind == edge_kinds::USED
                && edge.from == "task:t2"
                && edge.to.starts_with("commit:")
        }));
    }

    #[test]
    fn reading_an_upstream_artifact_substantiates_the_handoff() {
        let events = vec![
            plan(1, &[("t1", &[]), ("t2", &["t1"])]),
            task_start(2, "t1"),
            artifact(3, "art-1", "t1", ART1),
            task_finish(4, "t1", "completed", "artifact result"),
            task_start(5, "t2"),
            event(
                6,
                "tool.started",
                json!({
                    "tool_call_id": "read-1",
                    "name": "read_artifact",
                    "task_id": "t2",
                    "agent_id": "worker-t2",
                    "agent_name": "Worker t2",
                    "agent_role": "worker",
                    "input": {"artifact_id": "art-1"},
                }),
            ),
            agent_finish(
                7,
                "worker-t2",
                "Worker t2",
                "worker",
                Some("t2"),
                "idle",
                Some(C3),
                None,
                false,
                false,
                "used the upstream artifact",
                &[],
            ),
            task_finish(8, "t2", "failed", "downstream failed"),
        ];
        let report = report_for(events, Some("t2"));
        assert!(
            !report
                .findings
                .iter()
                .any(|finding| finding.reason_code == reason_codes::UNSUBSTANTIATED_HANDOFF)
        );
        let graph = report.graph.expect("graph");
        assert!(graph.edges.iter().any(|edge| {
            edge.kind == edge_kinds::USED && edge.from == "task:t2" && edge.to == "artifact:art-1"
        }));
        assert!(graph.nodes.iter().any(|node| {
            node.kind == node_kinds::ARTIFACT
                && node
                    .evidence
                    .iter()
                    .any(|evidence| evidence.hash.as_deref() == Some(ART1))
        }));
    }

    #[test]
    fn stale_test_evidence_is_reported_before_the_latest_commit() {
        let events = vec![
            plan(1, &[("t1", &[])]),
            task_start(2, "t1"),
            agent_finish(
                3,
                "worker-t1",
                "Worker t1",
                "worker",
                Some("t1"),
                "idle",
                Some(C1),
                None,
                false,
                false,
                "first attempt",
                &[],
            ),
            check(4, "t1", true),
            agent_finish(
                5,
                "worker-t1",
                "Worker t1",
                "worker",
                Some("t1"),
                "idle",
                Some(C2),
                None,
                false,
                false,
                "second attempt",
                &[],
            ),
            merge(6, "mq-1", "t1", C2, "failed", "Worker t1"),
            task_finish(7, "t1", "failed", "integration failed"),
        ];
        let report = report_for(events, Some("t1"));
        let earliest = report.earliest.expect("root finding");
        assert_eq!(earliest.reason_code, reason_codes::STALE_EVIDENCE);
        assert_eq!(earliest.node_id, format!("commit:{C2}"));
        assert!(earliest.evidence.iter().any(|evidence| {
            evidence.hash.as_deref() == Some(C1) || evidence.hash.as_deref() == Some(C2)
        }));
    }

    #[test]
    fn worker_and_verifier_false_consensus_flags_both_agents() {
        let events = vec![
            plan(1, &[("t1", &[])]),
            task_start(2, "t1"),
            agent_finish(
                3,
                "worker-t1",
                "Worker t1",
                "worker",
                Some("t1"),
                "idle",
                Some(C1),
                None,
                true,
                true,
                "worker done",
                &["src/lib.rs"],
            ),
            check(4, "t1", true),
            agent_finish(
                5,
                "verifier-1",
                "Verifier 1",
                "verifier",
                Some("t1"),
                "idle",
                None,
                Some(true),
                false,
                false,
                "approved",
                &[],
            ),
            merge(6, "mq-1", "t1", C1, "failed", "Worker t1"),
            task_finish(7, "t1", "failed", "integration failed"),
        ];
        let report = report_for(events, Some("t1"));
        let earliest = report.earliest.expect("root finding");
        assert_eq!(earliest.reason_code, reason_codes::FALSE_CONSENSUS);
        let consensus = report
            .findings
            .iter()
            .filter(|finding| finding.reason_code == reason_codes::FALSE_CONSENSUS)
            .collect::<Vec<_>>();
        assert_eq!(consensus.len(), 2);
        assert!(
            consensus
                .iter()
                .any(|finding| finding.flags.contains(&"worker".to_string()))
        );
        assert!(
            consensus
                .iter()
                .any(|finding| finding.flags.contains(&"verifier".to_string()))
        );
        assert!(
            report
                .responsible_agents
                .iter()
                .any(|agent| agent.role.as_deref() == Some("verifier"))
        );
        assert!(
            report
                .responsible_agents
                .iter()
                .any(|agent| agent.role.as_deref() == Some("worker"))
        );
    }

    #[test]
    fn completed_upstream_without_evidence_is_a_missing_evidence_finding() {
        let events = vec![
            plan(1, &[("t1", &[]), ("t2", &["t1"])]),
            task_start(2, "t1"),
            task_finish(3, "t1", "completed", "nothing recorded"),
            task_start(4, "t2"),
            task_finish(5, "t2", "failed", "downstream failed"),
        ];
        let report = report_for(events, Some("t2"));
        let earliest = report.earliest.expect("root finding");
        assert_eq!(earliest.reason_code, reason_codes::MISSING_EVIDENCE);
        assert_eq!(earliest.task_id.as_deref(), Some("t1"));
        assert_eq!(report.status, "missing_evidence");
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.summary.contains("no commit or artifact hash"))
        );
    }

    #[test]
    fn graph_is_bounded_by_nodes_edges_and_depth() {
        let tasks = (0..2_300)
            .map(|index| {
                let id = format!("t{index:04}");
                let deps = if index == 0 {
                    Vec::new()
                } else {
                    vec![format!("t{:04}", index - 1)]
                };
                (id, deps)
            })
            .collect::<Vec<_>>();
        let tasks_json = tasks
            .iter()
            .map(|(id, deps)| {
                json!({"id": id, "deps": deps, "department": "engineering", "kind": "coding"})
            })
            .collect::<Vec<_>>();
        let mut events = vec![event(
            1,
            "team_plan.submitted",
            json!({"goal": "long", "departments": [], "tasks": tasks_json}),
        )];
        events.push(task_finish(2, "t2299", "failed", "end failed"));
        let graph = build_provenance_graph("ses-1", &events);
        assert!(graph.nodes.len() <= MAX_GRAPH_NODES);
        assert!(graph.edges.len() <= MAX_GRAPH_EDGES);
        assert!(graph.truncated);
        let report = report_for(events, Some("t2299"));
        assert!(report.truncated);
        assert!(report.graph_summary.depth_limited);
        assert!(report.graph_summary.max_depth <= MAX_GRAPH_DEPTH);
        if let Some(graph) = report.graph {
            assert!(graph.nodes.len() < 2_300);
        }
    }

    #[test]
    fn report_never_contains_prompt_tool_or_file_content() {
        let events = vec![
            plan(1, &[("t1", &[])]),
            event(
                2,
                "agent.started",
                json!({
                    "agent_id": "worker-t1",
                    "agent_name": "Worker t1",
                    "agent_role": "worker",
                    "task_id": "t1",
                    "prompt": "SECRET_PROMPT_TEXT",
                    "goal": "SECRET_GOAL_TEXT",
                }),
            ),
            event(
                3,
                "tool.started",
                json!({
                    "tool_call_id": "tool-secret",
                    "name": "write_file",
                    "task_id": "t1",
                    "agent_id": "worker-t1",
                    "agent_name": "Worker t1",
                    "agent_role": "worker",
                    "input": {"path": "src/secret.rs", "content": "SECRET_FILE_CONTENT"},
                }),
            ),
            event(
                4,
                "tool.finished",
                json!({
                    "tool_call_id": "tool-secret",
                    "name": "write_file",
                    "task_id": "t1",
                    "agent_id": "worker-t1",
                    "agent_name": "Worker t1",
                    "agent_role": "worker",
                    "is_error": true,
                    "output": "SECRET_TOOL_OUTPUT",
                    "error_code": "TOOL_FAILED",
                }),
            ),
            agent_finish(
                5,
                "worker-t1",
                "Worker t1",
                "worker",
                Some("t1"),
                "failed",
                None,
                None,
                false,
                false,
                "SECRET_SUMMARY_TEXT",
                &[],
            ),
            task_finish(6, "t1", "failed", "SECRET_REASON_TEXT"),
        ];
        let report = report_for(events, Some("t1"));
        let encoded = serde_json::to_string(&report).unwrap();
        for secret in [
            "SECRET_PROMPT_TEXT",
            "SECRET_GOAL_TEXT",
            "SECRET_FILE_CONTENT",
            "SECRET_TOOL_OUTPUT",
            "SECRET_SUMMARY_TEXT",
            "SECRET_REASON_TEXT",
        ] {
            assert!(!encoded.contains(secret), "{secret} leaked into diagnosis");
        }
        let record = attribution_record(&report, "2026-11-13T00:00:00Z");
        let record_json = serde_json::to_string(&record).unwrap();
        for secret in [
            "SECRET_PROMPT_TEXT",
            "SECRET_FILE_CONTENT",
            "SECRET_TOOL_OUTPUT",
        ] {
            assert!(!record_json.contains(secret));
        }
        assert!(record.advisory);
    }

    #[test]
    fn attribution_is_deterministic_and_evidence_only() {
        let events = vec![
            plan(1, &[("t1", &[]), ("t2", &["t1"])]),
            task_start(2, "t1"),
            agent_finish(
                3,
                "worker-t1",
                "Worker t1",
                "worker",
                Some("t1"),
                "idle",
                Some(C1),
                None,
                true,
                true,
                "produced",
                &[],
            ),
            task_finish(4, "t1", "completed", "result"),
            task_start(5, "t2"),
            task_finish(6, "t2", "failed", "failed"),
        ];
        let first = report_for(events.clone(), Some("t2"));
        let second = report_for(events, Some("t2"));
        assert_eq!(first.graph_summary.digest, second.graph_summary.digest);
        assert_eq!(first.earliest, second.earliest);
        assert_eq!(first.findings, second.findings);
        assert_eq!(first.cascade, second.cascade);
        assert!(first.advisory);
        assert!(
            !first
                .findings
                .iter()
                .any(|finding| finding.summary == "result")
        );
    }

    #[test]
    fn ledger_events_parse_without_synthetic_rows() {
        let record = FailureAttributionRecord {
            schema_version: DIAGNOSIS_SCHEMA_VERSION,
            attribution_id: "fattr-1".into(),
            session_id: "ses-1".into(),
            status: "attributed".into(),
            advisory: true,
            created_at: "2026-11-13T00:00:00Z".into(),
            ..FailureAttributionRecord::default()
        };
        let events = vec![
            event(
                1,
                FAILURE_ATTRIBUTION_EVENT,
                json!({"attribution": record.clone()}),
            ),
            event(
                2,
                FAILURE_ATTRIBUTION_EVENT,
                json!({"attribution": "not a record"}),
            ),
        ];
        let rows = attributions_from_events(&events);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].event_seq, 1);
        assert_eq!(rows[0].attribution.attribution_id, "fattr-1");
    }

    #[test]
    fn contract_and_effect_pointers_are_evidence_not_verdicts() {
        let contract_hash = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
        let events = vec![
            plan(1, &[("t1", &[])]),
            event(
                2,
                "contract.derived",
                json!({"contract": {
                    "contract_id": "ctr-1",
                    "hash": contract_hash,
                    "task_id": "t1",
                    "role": "worker"
                }}),
            ),
            event(
                3,
                "effect.unknown",
                json!({"receipt": {
                    "id": "eff-1",
                    "task_id": "t1",
                    "tool": "remote_write",
                    "state": "unknown",
                    "response_hash": "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
                }}),
            ),
            task_finish(4, "t1", "failed", "remote write failed"),
        ];
        let report = report_for(events, Some("t1"));
        let earliest = report.earliest.expect("root finding");
        assert!(earliest.evidence.iter().any(|evidence| {
            evidence.kind == "contract" && evidence.hash.as_deref() == Some(contract_hash)
        }));
        assert!(
            earliest
                .evidence
                .iter()
                .any(|evidence| evidence.kind == "effect" && evidence.reference == "eff-1")
        );
        let graph = report.graph.expect("graph");
        let effect = graph
            .nodes
            .iter()
            .find(|node| node.id == "effect:eff-1")
            .expect("effect node");
        assert!(effect.evidence.iter().any(|evidence| {
            evidence.hash.as_deref()
                == Some("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff")
        }));
        let contract = graph
            .nodes
            .iter()
            .find(|node| node.id == "contract:ctr-1")
            .expect("contract node");
        assert!(
            contract
                .evidence
                .iter()
                .any(|evidence| { evidence.hash.as_deref() == Some(contract_hash) })
        );
    }

    #[test]
    fn unrelated_events_do_not_create_failure_attribution() {
        let events = vec![plan(1, &[("t1", &[])]), task_start(2, "t1")];
        let report = report_for(events, None);
        assert_eq!(report.status, "no_failure");
        assert!(report.target.is_none());
        assert!(report.earliest.is_none());
        assert!(report.findings.is_empty());
        assert!(report.advisory);
    }
}

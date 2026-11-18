//! Project-scoped hiring, promotion, transfer, retirement, and the derived
//! performance ledger (S6).
//!
//! Employment decisions are explicit project operations. A hire request never
//! creates an identity by itself, a retirement proposal never fires an
//! identity, and no function in this module is called by a scheduler or by a
//! model. Each decision commits its projection row, its normal S1/S2/S4 side
//! effects, and one append-only `agent_employment_events` row in a single
//! transaction.
//!
//! The performance ledger is a deterministic read model over existing task,
//! usage, Verifier, assignment, and failure-attribution evidence. It carries no
//! prompt text, tool output, file content, or model score. It can be rebuilt at
//! any time and never authorizes a mutation.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;
use vp_protocol::types::agent_performance_evidence_reasons as perf_reasons;
use vp_protocol::types::agent_retirement_reasons;
use vp_protocol::types::{
    AGENT_EMPLOYMENT_EVENT_KINDS, AGENT_EMPLOYMENT_MAX_EVIDENCE, AGENT_EMPLOYMENT_MAX_LIST,
    AGENT_EMPLOYMENT_MAX_OPERATOR_CHARS, AGENT_EMPLOYMENT_MAX_REASON_CHARS,
    AGENT_HIRE_APPROVED_EVENT, AGENT_HIRE_REJECTED_EVENT, AGENT_HIRE_REQUEST_STATUSES,
    AGENT_HIRE_REQUESTED_EVENT, AGENT_HIRE_ROLES, AGENT_IDENTITY_CREATED_EVENT,
    AGENT_IDENTITY_MAX_ID_CHARS, AGENT_IDENTITY_PROFILE_VALUES, AGENT_PERFORMANCE_MAX_EVIDENCE,
    AGENT_PERFORMANCE_MAX_REASON_COUNTS, AGENT_PROMOTE_APPROVED_EVENT, AGENT_PROMOTION_TARGETS,
    AGENT_RETIREMENT_APPROVED_EVENT, AGENT_RETIREMENT_EXECUTED_EVENT,
    AGENT_RETIREMENT_MAX_CANDIDATES, AGENT_RETIREMENT_MAX_LOOKBACK_DAYS,
    AGENT_RETIREMENT_MIN_LOOKBACK_DAYS, AGENT_RETIREMENT_PROPOSAL_STATUSES,
    AGENT_RETIREMENT_PROPOSED_EVENT, AGENT_RETIREMENT_REJECTED_EVENT,
    AGENT_TRANSFER_APPROVED_EVENT, AgentEmploymentEvent, AgentHireListParams, AgentHireListResult,
    AgentHireRequest, AgentHireResult, AgentIdentity, AgentIdentityError, AgentPerformance,
    AgentPerformanceEvidence, AgentPerformanceReasonCount, AgentPerformanceReplayReport,
    AgentPerformanceSummaryParams, AgentPerformanceSummaryResult, AgentPerformanceTotals,
    AgentPromoteParams, AgentPromoteResult, AgentRetireParams, AgentRetireResult,
    AgentRetirementCandidate, AgentRetirementDecisionResult, AgentRetirementPolicy,
    AgentRetirementPolicyResult, AgentRetirementProposal, AgentRetirementProposalListParams,
    AgentRetirementProposalListResult, AgentTransferParams, AgentTransferResult,
    CreateAgentIdentityParams, agent_identity_codes,
};

use super::coordination::{ASSIGNMENT_SELECT, map_assignment};
use super::workspaces::{archive_identity_workspaces_tx, identity_workspace_activity_tx};
use super::{
    Store, agent_identity_policy_tx, create_agent_identity_with_id_tx,
    ensure_identity_role_capacity_tx, get_agent_identity_tx, insert_agent_identity_event_tx,
    normalize_identity_policy, normalize_identity_text, update_agent_identity_row_tx,
    validate_identity_parent_tx, validate_identity_successor_tx,
};

const MAX_EMPLOYMENT_EVENT_PAYLOAD_BYTES: usize = 64 * 1024;
const MAX_PERFORMANCE_EVIDENCE_ROWS: usize = 50_000;
const MAX_PERFORMANCE_TASK_ROWS: usize = 50_000;
const MAX_PERFORMANCE_USAGE_ROWS: usize = 50_000;
const MAX_PERFORMANCE_MERGE_ROWS: usize = 20_000;
const MAX_PERFORMANCE_AGENT_ROWS: usize = 50_000;

fn employment_error(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    AgentIdentityError::new(code, message).into()
}

fn normalize_operator(value: Option<&str>) -> Result<String> {
    let value = value.unwrap_or("operator").trim();
    if value.is_empty() {
        return Ok("operator".to_string());
    }
    if value.chars().count() > AGENT_EMPLOYMENT_MAX_OPERATOR_CHARS {
        return Err(employment_error(
            agent_identity_codes::IDENTITY_INVALID,
            format!(
                "operator label must be at most {AGENT_EMPLOYMENT_MAX_OPERATOR_CHARS} characters"
            ),
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(employment_error(
            agent_identity_codes::IDENTITY_INVALID,
            "operator label must not contain control characters",
        ));
    }
    Ok(value.to_string())
}

fn normalize_reason(value: Option<&str>, field: &str) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.chars().count() > AGENT_EMPLOYMENT_MAX_REASON_CHARS {
        return Err(employment_error(
            agent_identity_codes::IDENTITY_INVALID,
            format!("{field} must be at most {AGENT_EMPLOYMENT_MAX_REASON_CHARS} characters"),
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(employment_error(
            agent_identity_codes::IDENTITY_INVALID,
            format!("{field} must not contain control characters"),
        ));
    }
    Ok(Some(value.to_string()))
}

fn normalize_approval_risk(value: Option<&str>) -> Result<String> {
    match value.unwrap_or("host_state").trim() {
        "host_state" => Ok("host_state".to_string()),
        other => Err(employment_error(
            agent_identity_codes::IDENTITY_INVALID,
            format!("approval_risk must be host_state: {other}"),
        )),
    }
}

fn role_rank(role: &str) -> Option<u8> {
    match role {
        "worker" => Some(1),
        "manager" => Some(2),
        "leader" => Some(3),
        _ => None,
    }
}

fn format_time(value: OffsetDateTime) -> Result<String> {
    value
        .format(&Rfc3339)
        .context("formatting employment timestamp")
}

fn parse_time(value: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(value, &Rfc3339).ok()
}

fn retirement_policy_tx(tx: &Transaction<'_>, project_id: &str) -> Result<AgentRetirementPolicy> {
    let stored: Option<String> = tx
        .query_row(
            "SELECT policy FROM project_agent_retirement_policies WHERE project_id = ?1",
            params![project_id],
            |row| row.get(0),
        )
        .optional()?;
    let policy = match stored {
        Some(raw) => serde_json::from_str::<AgentRetirementPolicy>(&raw)
            .context("parsing stored agent retirement policy")?,
        None => AgentRetirementPolicy::default(),
    };
    policy
        .normalized()
        .map_err(|error| employment_error(agent_identity_codes::RETIREMENT_POLICY_INVALID, error))
}

fn project_exists_tx(tx: &Transaction<'_>, project_id: &str) -> Result<()> {
    let exists: bool = tx
        .query_row(
            "SELECT 1 FROM projects WHERE id = ?1",
            params![project_id],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    if !exists {
        return Err(employment_error(
            agent_identity_codes::IDENTITY_NOT_FOUND,
            format!("project not found: {project_id}"),
        ));
    }
    Ok(())
}

fn next_employment_seq_tx(tx: &Transaction<'_>, project_id: &str) -> Result<u64> {
    let seq: i64 = tx.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM agent_employment_events WHERE project_id = ?1",
        params![project_id],
        |row| row.get(0),
    )?;
    Ok(seq.max(1) as u64)
}

pub(crate) fn insert_employment_event_tx(
    tx: &Transaction<'_>,
    project_id: &str,
    identity_id: &str,
    kind: &str,
    payload: Value,
) -> Result<AgentEmploymentEvent> {
    if !AGENT_EMPLOYMENT_EVENT_KINDS.contains(&kind) {
        return Err(employment_error(
            agent_identity_codes::IDENTITY_INVALID,
            format!("unknown employment event kind: {kind}"),
        ));
    }
    if identity_id.trim().is_empty() {
        return Err(employment_error(
            agent_identity_codes::IDENTITY_INVALID,
            "employment event is missing its subject identity",
        ));
    }
    let encoded = serde_json::to_string(&payload)?;
    if encoded.len() > MAX_EMPLOYMENT_EVENT_PAYLOAD_BYTES {
        return Err(employment_error(
            agent_identity_codes::IDENTITY_INVALID,
            format!("employment event payload exceeds {MAX_EMPLOYMENT_EVENT_PAYLOAD_BYTES} bytes"),
        ));
    }
    let event = AgentEmploymentEvent {
        id: Uuid::now_v7().to_string(),
        project_id: project_id.to_string(),
        seq: next_employment_seq_tx(tx, project_id)?,
        identity_id: identity_id.to_string(),
        kind: kind.to_string(),
        payload,
        created_at: super::now_rfc3339(),
    };
    tx.execute(
        "INSERT INTO agent_employment_events
         (id, project_id, seq, identity_id, kind, payload, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            event.id,
            event.project_id,
            event.seq as i64,
            event.identity_id,
            event.kind,
            encoded,
            event.created_at
        ],
    )?;
    Ok(event)
}

fn map_hire_request(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentHireRequest> {
    let policy_json: String = row.get(10)?;
    let policy = serde_json::from_str(&policy_json).unwrap_or_else(|_| serde_json::json!({}));
    Ok(AgentHireRequest {
        id: row.get(0)?,
        project_id: row.get(1)?,
        identity_id: row.get(2)?,
        role: row.get(3)?,
        department: row.get(4)?,
        component: row.get(5)?,
        title: row.get(6)?,
        profile: row.get(7)?,
        model: row.get(8)?,
        parent_identity_id: row.get(9)?,
        policy,
        status: row.get(11)?,
        requested_at: row.get(12)?,
        requested_by: row.get(13)?,
        approval_risk: row.get(14)?,
        decided_at: row.get(15)?,
        decided_by: row.get(16)?,
        reason: row.get(17)?,
        origin: row.get(18)?,
    })
}

const HIRE_REQUEST_SELECT: &str = "SELECT id, project_id, identity_id, role, department,
    component, title, profile, model, parent_identity_id, policy_json, status,
    requested_at, requested_by, approval_risk, decided_at, decided_by, reason, origin
    FROM agent_hire_requests";

fn get_hire_request_tx(tx: &Transaction<'_>, id: &str) -> Result<Option<AgentHireRequest>> {
    tx.query_row(
        &format!("{HIRE_REQUEST_SELECT} WHERE id = ?1"),
        params![id],
        map_hire_request,
    )
    .optional()
    .map_err(Into::into)
}

pub(crate) fn insert_hire_request_tx(
    tx: &Transaction<'_>,
    request: &AgentHireRequest,
) -> Result<()> {
    let policy = serde_json::to_string(&request.policy)?;
    tx.execute(
        "INSERT INTO agent_hire_requests
         (id, project_id, identity_id, role, department, component, title, profile, model,
          parent_identity_id, policy_json, status, requested_at, requested_by, approval_risk,
          decided_at, decided_by, reason, origin)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
        params![
            request.id,
            request.project_id,
            request.identity_id,
            request.role,
            request.department,
            request.component,
            request.title,
            request.profile,
            request.model,
            request.parent_identity_id,
            policy,
            request.status,
            request.requested_at,
            request.requested_by,
            request.approval_risk,
            request.decided_at,
            request.decided_by,
            request.reason,
            request.origin
        ],
    )?;
    Ok(())
}

fn update_hire_request_tx(tx: &Transaction<'_>, request: &AgentHireRequest) -> Result<()> {
    let policy = serde_json::to_string(&request.policy)?;
    tx.execute(
        "UPDATE agent_hire_requests
         SET status = ?1, decided_at = ?2, decided_by = ?3, reason = ?4, policy_json = ?5
         WHERE id = ?6",
        params![
            request.status,
            request.decided_at,
            request.decided_by,
            request.reason,
            policy,
            request.id
        ],
    )?;
    Ok(())
}

fn create_params_from_hire(request: &AgentHireRequest) -> CreateAgentIdentityParams {
    CreateAgentIdentityParams {
        project_id: request.project_id.clone(),
        role: request.role.clone(),
        department: request.department.clone(),
        component: request.component.clone(),
        title: request.title.clone(),
        profile: request.profile.clone(),
        model: request.model.clone(),
        parent_identity_id: request.parent_identity_id.clone(),
        policy: Some(request.policy.clone()),
    }
}

pub(crate) fn validate_hire_preflight_tx(
    tx: &Transaction<'_>,
    request: &AgentHireRequest,
) -> Result<()> {
    if !AGENT_HIRE_ROLES.contains(&request.role.as_str()) {
        return Err(employment_error(
            agent_identity_codes::HIRE_INVALID,
            "role must be manager or worker; the Leader is never hired through this RPC",
        ));
    }
    let department = normalize_identity_text(
        request.department.as_deref(),
        "department",
        vp_protocol::types::AGENT_IDENTITY_MAX_DEPARTMENT_CHARS,
    )?;
    let component = normalize_identity_text(
        request.component.as_deref(),
        "component",
        vp_protocol::types::AGENT_IDENTITY_MAX_COMPONENT_CHARS,
    )?;
    let title = normalize_identity_text(
        request.title.as_deref(),
        "title",
        vp_protocol::types::AGENT_IDENTITY_MAX_TITLE_CHARS,
    )?;
    let profile = normalize_identity_text(
        request.profile.as_deref(),
        "profile",
        vp_protocol::types::AGENT_IDENTITY_MAX_DEPARTMENT_CHARS,
    )?;
    if let Some(profile) = &profile
        && !AGENT_IDENTITY_PROFILE_VALUES.contains(&profile.as_str())
    {
        return Err(employment_error(
            agent_identity_codes::IDENTITY_INVALID,
            format!("profile must be one of coding, research, verification, generic: {profile}"),
        ));
    }
    let _ = (department, component, title);
    normalize_identity_text(
        request.model.as_deref(),
        "model",
        vp_protocol::types::AGENT_IDENTITY_MAX_MODEL_CHARS,
    )?;
    let parent = normalize_identity_text(
        request.parent_identity_id.as_deref(),
        "parent_identity_id",
        AGENT_IDENTITY_MAX_ID_CHARS,
    )?;
    validate_identity_parent_tx(tx, &request.project_id, &request.role, parent.as_deref())?;
    ensure_identity_role_capacity_tx(tx, &request.project_id, &request.role, None)?;
    normalize_identity_policy(&request.policy)?;
    Ok(())
}

pub(crate) fn build_hire_request(
    params: &vp_protocol::types::AgentHireParams,
    identity_id: String,
    operator: &str,
    now: &str,
) -> Result<AgentHireRequest> {
    let role = params.role.trim().to_ascii_lowercase();
    if !AGENT_HIRE_ROLES.contains(&role.as_str()) {
        return Err(employment_error(
            agent_identity_codes::HIRE_INVALID,
            "role must be manager or worker; the Leader is never hired through this RPC",
        ));
    }
    let policy = match &params.policy {
        Some(policy) => normalize_identity_policy(policy)?,
        None => serde_json::json!({}),
    };
    Ok(AgentHireRequest {
        id: Uuid::now_v7().to_string(),
        project_id: params.project_id.trim().to_string(),
        identity_id,
        role,
        department: normalize_identity_text(
            params.department.as_deref(),
            "department",
            vp_protocol::types::AGENT_IDENTITY_MAX_DEPARTMENT_CHARS,
        )?,
        component: normalize_identity_text(
            params.component.as_deref(),
            "component",
            vp_protocol::types::AGENT_IDENTITY_MAX_COMPONENT_CHARS,
        )?,
        title: normalize_identity_text(
            params.title.as_deref(),
            "title",
            vp_protocol::types::AGENT_IDENTITY_MAX_TITLE_CHARS,
        )?,
        profile: normalize_identity_text(
            params.profile.as_deref(),
            "profile",
            vp_protocol::types::AGENT_IDENTITY_MAX_DEPARTMENT_CHARS,
        )?,
        model: normalize_identity_text(
            params.model.as_deref(),
            "model",
            vp_protocol::types::AGENT_IDENTITY_MAX_MODEL_CHARS,
        )?,
        parent_identity_id: normalize_identity_text(
            params.parent_identity_id.as_deref(),
            "parent_identity_id",
            AGENT_IDENTITY_MAX_ID_CHARS,
        )?,
        policy,
        status: "pending".to_string(),
        requested_at: now.to_string(),
        requested_by: operator.to_string(),
        approval_risk: "host_state".to_string(),
        decided_at: None,
        decided_by: None,
        reason: None,
        origin: "local".to_string(),
    })
}

fn update_identity_and_event_tx(
    tx: &Transaction<'_>,
    existing: &AgentIdentity,
    next: &AgentIdentity,
    changed_fields: &[&'static str],
    event_kind: &str,
    extra: Value,
) -> Result<AgentIdentity> {
    update_agent_identity_row_tx(tx, next)?;
    let mut payload = serde_json::json!({
        "identity": next,
        "changed_fields": changed_fields,
        "previous_generation": existing.generation,
        "generation": next.generation,
    });
    if let (Some(target), Some(extra)) = (payload.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            target.insert(key.clone(), value.clone());
        }
    }
    insert_agent_identity_event_tx(tx, next, event_kind, payload)?;
    Ok(next.clone())
}

fn identity_conflict_counts_tx(tx: &Transaction<'_>, identity_id: &str) -> Result<(u64, u64, u64)> {
    let assignments: i64 = tx.query_row(
        "SELECT COUNT(*) FROM session_agent_assignments
         WHERE identity_id = ?1 AND status = 'active'",
        params![identity_id],
        |row| row.get(0),
    )?;
    let (workspaces, bindings) = identity_workspace_activity_tx(tx, identity_id)?;
    Ok((assignments.max(0) as u64, workspaces, bindings))
}

pub(crate) fn get_retirement_proposal_for_import(
    tx: &Transaction<'_>,
    id: &str,
) -> Result<Option<AgentRetirementProposal>> {
    get_retirement_proposal_tx(tx, id)
}

pub(crate) fn insert_retirement_proposal_for_import(
    tx: &Transaction<'_>,
    proposal: &AgentRetirementProposal,
) -> Result<()> {
    insert_retirement_proposal_tx(tx, proposal)
}

pub(crate) fn get_hire_request_for_import(
    tx: &Transaction<'_>,
    id: &str,
) -> Result<Option<AgentHireRequest>> {
    get_hire_request_tx(tx, id)
}

pub(crate) fn insert_hire_request_for_import(
    tx: &Transaction<'_>,
    request: &AgentHireRequest,
) -> Result<()> {
    insert_hire_request_tx(tx, request)
}

mod candidates;
mod hiring;
mod performance;
mod promotion;
mod replay;
mod retirement;
mod snapshots;

pub use candidates::*;
pub use hiring::*;
pub use performance::*;
pub use promotion::*;
pub use replay::*;
pub use retirement::*;
pub use snapshots::*;

#[cfg(test)]
mod tests;
